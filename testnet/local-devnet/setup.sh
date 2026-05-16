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

# ---- Key reorganization ----
log_info "Reorganizing keys into testnet/local-devnet/keys/"

mkdir -p "$LD_KEYS/pool1" "$LD_KEYS/pool2" "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys"

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
chmod 0700 "$LD_KEYS" "$LD_KEYS"/pool* "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys"
find "$LD_KEYS" -name '*.skey' -exec chmod 0600 {} \;

log_info "Keys reorganized; payment address: $(cat "$LD_KEYS/utxo/payment.addr")"

# Task 10 adds: config rendering
