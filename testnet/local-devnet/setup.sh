#!/usr/bin/env bash
# Bootstrap the local-devnet: generate genesis, keys, configs.
# Run once before run.sh. Idempotent — re-running wipes prior state.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

log_info "=== Local devnet setup ==="

check_prereqs
assert_ports_free

log_info "Wiping prior state (genesis, keys, state, logs, evidence)"
rm -rf "$LD_GENESIS" "$LD_KEYS" "$LD_STATE" "$LD_LOGS" "$LD_EVIDENCE"
rm -f "$LD_CONFIG"/dugite-*.json "$LD_CONFIG"/cardano-*.json \
      "$LD_CONFIG"/relays.json "$LD_CONFIG"/genesis-hashes.env

mkdir -p "$LD_GENESIS" "$LD_KEYS" "$LD_STATE" "$LD_LOGS" "$LD_EVIDENCE"

log_info "Dir prep complete"

# ---- Genesis generation ----
log_info "Generating genesis via cardano-cli conway genesis create-testnet-data"

# Compute genesis start time = now + 30s (cardano-cli's own default; spelled out so we can sanity-check later)
START_TIME=$(date -u -v+30S +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+30 seconds' +%Y-%m-%dT%H:%M:%SZ)
log_info "Genesis start time: $START_TIME"

# Step A: generate cardano-cli's default specs to a tmpdir so we can merge our overrides in.
TMP_DEFAULTS="$(mktemp -d)"
trap 'rm -rf "$TMP_DEFAULTS"' EXIT
cardano-cli conway genesis create-testnet-data \
    --pools 0 --testnet-magic 1 --out-dir "$TMP_DEFAULTS/defaults" >/dev/null

# Step B: deep-merge our override fragments onto the defaults.
TMP_SPEC="$(mktemp -d)"
trap 'rm -rf "$TMP_DEFAULTS" "$TMP_SPEC"' EXIT
jq -s '.[0] * .[1]' \
    "$TMP_DEFAULTS/defaults/shelley-genesis.json" \
    "$LD_CONFIG/spec/shelley-spec.json" > "$TMP_SPEC/shelley-spec.json"
jq -s '.[0] * .[1]' \
    "$TMP_DEFAULTS/defaults/conway-genesis.json" \
    "$LD_CONFIG/spec/conway-spec.json" > "$TMP_SPEC/conway-spec.json"

# Step B.5: pre-generate a Constitutional Committee key pair so we can patch
# the conway-genesis.json after Step D. cardano-cli 11.0.0's
# create-testnet-data IGNORES the `committee` field in its --spec-conway
# input — it always emits members={} and threshold=0 regardless of what the
# spec says. To bootstrap a real seated CC member we have to post-process the
# generated conway-genesis.json *before* we hash it (Step D.5 below).
log_info "Pre-generating CC member keys (cc-1)"
mkdir -p "$LD_KEYS/cc-1"
cardano-cli conway governance committee key-gen-cold \
    --verification-key-file "$LD_KEYS/cc-1/cc-cold.vkey" \
    --cold-signing-key-file "$LD_KEYS/cc-1/cc-cold.skey"
cardano-cli conway governance committee key-gen-hot \
    --verification-key-file "$LD_KEYS/cc-1/cc-hot.vkey" \
    --signing-key-file      "$LD_KEYS/cc-1/cc-hot.skey"
CC_COLD_HASH="$(cardano-cli conway governance committee key-hash \
                --verification-key-file "$LD_KEYS/cc-1/cc-cold.vkey")"
# Pick an expiry epoch well past anything a soak/zoo run will reach
# (govActionLifetime + committeeMaxTermLength = 6 + 73 = 79 epochs upper bound).
CC_EXPIRY_EPOCH=1000
log_info "CC cold-key hash: $CC_COLD_HASH (expiry epoch $CC_EXPIRY_EPOCH)"

# Step C: write the on-chain pool relay descriptor.
# cardano-cli 11.0.0.0 expects a Map keyed by pool index (Word) → array of relay entries,
# NOT a top-level array. Numeric-string keys are accepted as Word values.
cat > "$LD_CONFIG/relays.json" <<EOF
{
  "1": [ { "single host address": { "IPv4": "127.0.0.1", "IPv6": null, "port": $LD_RELAY_PORT } } ],
  "2": [ { "single host address": { "IPv4": "127.0.0.1", "IPv6": null, "port": $LD_RELAY_PORT } } ]
}
EOF

# Step D: generate the real testnet data with our merged spec.
cardano-cli conway genesis create-testnet-data \
    --spec-shelley "$TMP_SPEC/shelley-spec.json" \
    --spec-conway  "$TMP_SPEC/conway-spec.json" \
    --testnet-magic "$LD_MAGIC" \
    --genesis-keys 3 \
    --pools 2 \
    --stake-delegators 4 \
    --utxo-keys 1 \
    --total-supply     60000000000000000 \
    --delegated-supply 30000000000000000 \
    --start-time       "$START_TIME" \
    --relays           "$LD_CONFIG/relays.json" \
    --out-dir          "$LD_GENESIS"

log_info "Genesis generated at $LD_GENESIS"
ls -1 "$LD_GENESIS"

# Step D.5: patch the conway-genesis.json to seat the CC member (and set a
# matching threshold). cardano-cli omits this field on output, so we inject
# it post-hoc before any hash is taken. The hash recorded into
# genesis-hashes.env will reflect this patched content, so nodes will boot
# with a properly populated committee. cardano-spec threshold is a Rational;
# we pick 1/1 so a single CC vote is enough for ratification (the tx-zoo only
# seats one member).
log_info "Patching conway-genesis.json to seat CC member cc-1"
jq --arg cred "keyHash-${CC_COLD_HASH}" --argjson exp "$CC_EXPIRY_EPOCH" \
   '.committee.members = {($cred): $exp}
    | .committee.threshold = {"numerator": 1, "denominator": 1}' \
   "$LD_GENESIS/conway-genesis.json" > "$LD_GENESIS/conway-genesis.patched.json"
mv "$LD_GENESIS/conway-genesis.patched.json" "$LD_GENESIS/conway-genesis.json"
log_info "conway-genesis.committee now: $(jq -c .committee "$LD_GENESIS/conway-genesis.json")"

# ---- Key reorganization ----
log_info "Reorganizing keys into testnet/local-devnet/keys/"

mkdir -p "$LD_KEYS/pool1" "$LD_KEYS/pool2" "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys"
# $LD_KEYS/cc-1 was already created earlier when we bootstrapped the CC member;
# keep its tightened perms in the chmod sweep below.

# Pools — cardano-cli writes them as pool1/, pool2/ inside pools-keys/.
# Note: cardano-cli 11.0.0.0 emits the operational counter as opcert.counter
# (NOT cold.counter as the original plan assumed).
for n in 1 2; do
    src="$LD_GENESIS/pools-keys/pool$n"
    dst="$LD_KEYS/pool$n"
    [ -d "$src" ] || die "Expected $src — cardano-cli output schema may have changed"
    cp "$src/cold.skey"     "$dst/cold.skey"
    cp "$src/cold.vkey"     "$dst/cold.vkey"
    cp "$src/opcert.counter" "$dst/opcert.counter"
    cp "$src/vrf.skey"      "$dst/vrf.skey"
    cp "$src/vrf.vkey"      "$dst/vrf.vkey"
    cp "$src/kes.skey"      "$dst/kes.skey"
    cp "$src/kes.vkey"      "$dst/kes.vkey"
    cp "$src/opcert.cert"   "$dst/opcert.cert"
done

# UTxO funds key — for tx submission tests
cp "$LD_GENESIS/utxo-keys/utxo1/utxo.skey"  "$LD_KEYS/utxo/payment.skey"
cp "$LD_GENESIS/utxo-keys/utxo1/utxo.vkey"  "$LD_KEYS/utxo/payment.vkey"
cp "$LD_GENESIS/utxo-keys/utxo1/utxo-stake.skey"  "$LD_KEYS/utxo/stake.skey"  2>/dev/null || true
cp "$LD_GENESIS/utxo-keys/utxo1/utxo-stake.vkey"  "$LD_KEYS/utxo/stake.vkey"  2>/dev/null || true

# Derive payment address — base address if stake key exists, else enterprise
if [ -f "$LD_KEYS/utxo/stake.vkey" ]; then
    cardano-cli conway address build \
        --payment-verification-key-file "$LD_KEYS/utxo/payment.vkey" \
        --stake-verification-key-file   "$LD_KEYS/utxo/stake.vkey" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$LD_KEYS/utxo/payment.addr"
else
    cardano-cli conway address build \
        --payment-verification-key-file "$LD_KEYS/utxo/payment.vkey" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$LD_KEYS/utxo/payment.addr"
fi

# Genesis keys — kept for completeness, not used at runtime
cp -R "$LD_GENESIS"/genesis-keys/* "$LD_KEYS/genesis-keys/" 2>/dev/null || true

# Tighten permissions
chmod 0700 "$LD_KEYS" "$LD_KEYS"/pool* "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys" "$LD_KEYS/cc-1"
find "$LD_KEYS" -name '*.skey' -exec chmod 0600 {} \;

log_info "Keys reorganized; payment address: $(cat "$LD_KEYS/utxo/payment.addr")"

# ---- Config + topology rendering ----
log_info "Computing genesis hashes"

BYRON_HASH="$(cardano-cli byron genesis print-genesis-hash --genesis-json "$LD_GENESIS/byron-genesis.json")"
SHELLEY_HASH="$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/shelley-genesis.json")"
ALONZO_HASH="$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/alonzo-genesis.json")"
CONWAY_HASH="$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/conway-genesis.json")"

cat > "$LD_CONFIG/genesis-hashes.env" <<EOF
BYRON_HASH=$BYRON_HASH
SHELLEY_HASH=$SHELLEY_HASH
ALONZO_HASH=$ALONZO_HASH
CONWAY_HASH=$CONWAY_HASH
EOF

log_info "Genesis hashes: byron=$BYRON_HASH shelley=$SHELLEY_HASH alonzo=$ALONZO_HASH conway=$CONWAY_HASH"

# Render every template — substitute @@TOKEN@@ placeholders
render_template() {
    local src="$1" dst="$2"
    sed \
        -e "s|@@GENESIS_DIR@@|$LD_GENESIS|g" \
        -e "s|@@KEYS_DIR@@|$LD_KEYS|g" \
        -e "s|@@BYRON_HASH@@|$BYRON_HASH|g" \
        -e "s|@@SHELLEY_HASH@@|$SHELLEY_HASH|g" \
        -e "s|@@ALONZO_HASH@@|$ALONZO_HASH|g" \
        -e "s|@@CONWAY_HASH@@|$CONWAY_HASH|g" \
        "$src" > "$dst"
}

render_template "$LD_CONFIG/templates/dugite-bp.config.tmpl.json"      "$LD_CONFIG/dugite-bp.config.json"
render_template "$LD_CONFIG/templates/dugite-relay.config.tmpl.json"   "$LD_CONFIG/dugite-relay.config.json"
render_template "$LD_CONFIG/templates/cardano-bp.config.tmpl.json"     "$LD_CONFIG/cardano-bp.config.json"
render_template "$LD_CONFIG/templates/dugite-bp.topology.tmpl.json"    "$LD_CONFIG/dugite-bp.topology.json"
render_template "$LD_CONFIG/templates/dugite-relay.topology.tmpl.json" "$LD_CONFIG/dugite-relay.topology.json"
render_template "$LD_CONFIG/templates/cardano-bp.topology.tmpl.json"   "$LD_CONFIG/cardano-bp.topology.json"

# Sanity check — every rendered file must parse as JSON
for f in "$LD_CONFIG"/dugite-*.json "$LD_CONFIG"/cardano-*.json; do
    jq empty "$f" || die "Rendered config $f is not valid JSON"
done

log_info "All configs + topologies rendered to $LD_CONFIG/"
log_info "Setup complete. Next: ./run.sh"
