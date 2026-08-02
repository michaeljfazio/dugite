#!/usr/bin/env bash
# Bootstrap the local-devnet: generate genesis, keys, configs.
# Run once before run.sh. Idempotent — re-running wipes prior state.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

log_info "=== Local devnet setup ==="

check_prereqs
assert_ports_free

# Preserve evidence across rounds. The standard workflow is
# `setup -> round -> stop -> setup -> round -> …` and only generates the
# release report at the very end, from `ls -t evidence | sed -n '1p;2p;3p'`.
# Deleting evidence/ here made every round but the last unreportable, so a
# multi-round run could not produce the report that gates a release.
#
# Move (never delete) any existing evidence into evidence-archive/auto/ so the
# report generator can still be pointed at earlier rounds.
if [ -d "$LD_EVIDENCE" ] && [ -n "$(ls -A "$LD_EVIDENCE" 2>/dev/null)" ]; then
  # Auto-archived runs go in their own subdirectory: evidence-archive/ itself
  # holds hand-curated, version-controlled archives (bug-j-fix-1800s-soak-pass
  # and friends), and dropping timestamped run debris beside them would leave
  # untracked cruft in the repo after every devnet run.
  archive="$LD_ROOT/evidence-archive/auto"
  mkdir -p "$archive"
  for d in "$LD_EVIDENCE"/*; do
    [ -e "$d" ] || continue
    base=$(basename "$d")
    if [ -e "$archive/$base" ]; then
      base="${base}-$(date -u +%s)"
    fi
    mv "$d" "$archive/$base"
    log_info "Archived prior evidence: evidence-archive/auto/$base"
  done
fi

# Reset the tx-zoo results ledger so each round's results.csv covers only that
# round. tx-zoo/state/ deliberately lives outside $LD_STATE (keys and funded
# UTxOs are reused via `run-all.sh --setup`), so results.csv would otherwise
# accumulate across every round of a multi-round run and make the per-round
# tx counts in the release report meaningless.
_zoo_results="$LD_ROOT/tx-zoo/state/results.csv"
if [ -f "$_zoo_results" ]; then
  mkdir -p "$LD_ROOT/evidence-archive/auto"
  mv "$_zoo_results" \
     "$LD_ROOT/evidence-archive/auto/results-$(date -u +%Y%m%dT%H%M%SZ).csv"
  log_info "Archived prior tx-zoo results.csv"
fi

log_info "Wiping prior state (genesis, keys, state, logs)"
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

# Step B.5: pre-generate TWO Constitutional Committee key pairs so we can patch
# the conway-genesis.json after Step D. cardano-cli 11.0.0's
# create-testnet-data IGNORES the `committee` field in its --spec-conway
# input — it always emits members={} and threshold=0 regardless of what the
# spec says. To bootstrap real seated CC members we have to post-process the
# generated conway-genesis.json *before* we hash it (Step D.5 below).
#
# Two members are seated so the tx-zoo can exercise both wire paths in the
# same run: cc-1 is authorised by 05g and resigned by 05h (testing the
# authorize+resign cert paths); cc-2 stays authorised through the run so
# 07f/07g can vote with a hot key that remains attached to a live seated
# member after 05h retires cc-1.
log_info "Pre-generating CC member keys (cc-1, cc-2)"
for cc in cc-1 cc-2; do
    mkdir -p "$LD_KEYS/$cc"
    cardano-cli conway governance committee key-gen-cold \
        --verification-key-file "$LD_KEYS/$cc/cc-cold.vkey" \
        --cold-signing-key-file "$LD_KEYS/$cc/cc-cold.skey"
    cardano-cli conway governance committee key-gen-hot \
        --verification-key-file "$LD_KEYS/$cc/cc-hot.vkey" \
        --signing-key-file      "$LD_KEYS/$cc/cc-hot.skey"
done
CC1_COLD_HASH="$(cardano-cli conway governance committee key-hash \
                --verification-key-file "$LD_KEYS/cc-1/cc-cold.vkey")"
CC2_COLD_HASH="$(cardano-cli conway governance committee key-hash \
                --verification-key-file "$LD_KEYS/cc-2/cc-cold.vkey")"
# Pick an expiry epoch well past anything a soak/zoo run will reach
# (govActionLifetime + committeeMaxTermLength = 6 + 73 = 79 epochs upper bound).
CC_EXPIRY_EPOCH=1000
log_info "CC cold-key hashes: cc-1=$CC1_COLD_HASH cc-2=$CC2_COLD_HASH (expiry epoch $CC_EXPIRY_EPOCH)"

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
#
# Two pools are generated (so the keys exist for any cross-validation
# tooling that wants to address them), but ALL 20 stake-delegators are
# redirected to pool1 in Step D.4 below. pool2 has zero active stake,
# never wins a leader lottery, and cardano-bp runs as a non-forging
# validator (relay role) — see run.sh, which omits the
# --shelley-{kes,vrf,operational-certificate} flags for cardano-bp.
#
# Rationale: equal-stake (10:10) between the two pools produces
# constant chain divergence — both forge at the same rate and the
# relay's chain selection flips between forks ~every block. Even with
# a heavy 95/5 skew, the 5% pool occasionally beats the propagation
# window and produces a competing block at the same height
# (cardano-bp forges at its own slot before dugite-bp's block reaches
# it), creating an asymmetric fork that is slow to resolve.
#
# CORRECTION (2026-08-02, oracle-verified against ouroboros-consensus
# release-ouroboros-consensus-3.0.1.0, the version cardano-node 11.0.1
# resolves via CHaP): an earlier revision of this comment attributed
# the stuck fork to a "first-seen tiebreaker". There is no such
# primitive. `comparePraos` compares, in order: (1) blockNo — strictly
# longer ALWAYS wins, independent of everything below; (2) same issuer
# AND same slot -> higher opcert issue number; (3) otherwise the VRF
# value, LOWER wins, but only when `vrfArmed`. Conway hardcodes
# `RestrictedVRFTiebreaker 5`, so the VRF tiebreak is DISARMED when the
# competing blocks' slots are more than 5 apart, and `comparePraos`
# then returns `ShouldNotSwitch EQ`. ChainSel drops every
# non-`ShouldSwitch` verdict, so the incumbent chain is kept — which
# LOOKS like first-seen-wins and is the documented "Frankfurt problem"
# the VRF tiebreak was introduced to fix. It is not permanent: rule (1)
# forces a switch the moment either chain extends by one block. So
# "never resolves" was an overstatement; "resolves only once someone
# extends" is accurate. Making cardano-bp non-forging by default
# remains the right call, because it removes the race from the
# cross-validation path entirely rather than depending on tiebreak
# semantics — and because with 3+ forgers the restricted tiebreaker is
# provably non-transitive. The
# clean fix is to make cardano-bp non-forging: dugite-bp is the sole
# producer, cardano-bp chainsync+blockfetches and applies every block
# through the Haskell ledger — exact cross-validation, zero
# divergence possible.
cardano-cli conway genesis create-testnet-data \
    --spec-shelley "$TMP_SPEC/shelley-spec.json" \
    --spec-conway  "$TMP_SPEC/conway-spec.json" \
    --testnet-magic "$LD_MAGIC" \
    --genesis-keys 3 \
    --pools 2 \
    --stake-delegators 20 \
    --utxo-keys 1 \
    --total-supply     60000000000000000 \
    --delegated-supply 30000000000000000 \
    --start-time       "$START_TIME" \
    --relays           "$LD_CONFIG/relays.json" \
    --out-dir          "$LD_GENESIS"

log_info "Genesis generated at $LD_GENESIS"
ls -1 "$LD_GENESIS"

# Step D.4: redirect ALL 20 stake-delegators to pool1. cardano-cli splits
# the delegators evenly across the two generated pools by default
# (10:10), but pool2's stake would only be useful if cardano-bp were
# forging — and we run cardano-bp as a non-forger to eliminate the
# asymmetric-fork class entirely (see Step D rationale). Giving all
# stake to pool1 maximises dugite-bp's leader rate.
POOL1_HEX="$(cardano-cli conway stake-pool id \
    --cold-verification-key-file "$LD_GENESIS/pools-keys/pool1/cold.vkey" \
    --output-hex)"
POOL2_HEX="$(cardano-cli conway stake-pool id \
    --cold-verification-key-file "$LD_GENESIS/pools-keys/pool2/cold.vkey" \
    --output-hex)"

# ---- Two-forger mode (#957) ----
#
# The single-forger topology makes chain selection under contention, slot
# battles, competing-chain rollback and orphan handling STRUCTURALLY
# impossible to exercise: with one producer there is never a second candidate
# for the same height, so no round of any duration can reach those paths. That
# was the largest operating-condition blind spot in the release gate, and
# #763 established that offline replay cannot cover the rollback path either —
# so it was validated nowhere.
#
#   LD_TWO_FORGERS=1 ./setup.sh && ./run.sh
#
# The mode is recorded in a marker file rather than relying on the environment
# variable being re-exported for run.sh: setting it for setup and forgetting it
# for run would silently produce a single-forger devnet that the round script
# then asserts two-forger properties about.
#
# Stake split: the default is deliberately UNEVEN (see LD_POOL2_STAKE_PCT).
# At 50/50 with f=0.5 both pools win ~50% of slots and fork constantly, which
# is a fine stress test but a poor convergence test — you cannot tell a
# resolved fork from a chain that never settled. An uneven split makes the
# majority chain the expected winner while still producing regular battles.
if [ "${LD_TWO_FORGERS:-0}" = "1" ]; then
    POOL2_PCT="${LD_POOL2_STAKE_PCT:-40}"
    log_info "TWO-FORGER MODE: splitting genesis delegation pool1/pool2 = $((100 - POOL2_PCT))/$POOL2_PCT"
    jq --arg p1 "$POOL1_HEX" --arg p2 "$POOL2_HEX" --argjson pct "$POOL2_PCT" '
        .staking.stake as $s
        | ($s | keys | length) as $n
        | (($n * $pct / 100) | floor) as $k
        | .staking.stake = (
            $s | to_entries
               | to_entries
               | map(.value.value = (if .key < $k then $p2 else $p1 end) | .value)
               | from_entries
          )' "$LD_GENESIS/shelley-genesis.json" \
       > "$LD_GENESIS/shelley-genesis.patched.json"
    mv "$LD_GENESIS/shelley-genesis.patched.json" "$LD_GENESIS/shelley-genesis.json"
    echo "$POOL2_PCT" > "$LD_GENESIS/.two-forgers"
else
    log_info "Redirecting all stake delegations to pool1=$POOL1_HEX"
    rm -f "$LD_GENESIS/.two-forgers"
    jq --arg p1 "$POOL1_HEX" '
        .staking.stake = (
            .staking.stake | with_entries(.value = $p1)
        )' "$LD_GENESIS/shelley-genesis.json" \
       > "$LD_GENESIS/shelley-genesis.patched.json"
    mv "$LD_GENESIS/shelley-genesis.patched.json" "$LD_GENESIS/shelley-genesis.json"
fi
log_info "stake-pool delegation counts: $(jq -c '
    .staking.stake | to_entries | group_by(.value) | map({pool: .[0].value, n: length})
' "$LD_GENESIS/shelley-genesis.json")"

# Step D.5: patch the conway-genesis.json to seat both CC members (and set a
# matching threshold). cardano-cli omits this field on output, so we inject
# it post-hoc before any hash is taken. The hash recorded into
# genesis-hashes.env will reflect this patched content, so nodes will boot
# with a properly populated committee. cardano-spec threshold is a Rational;
# we pick 1/2 so a single CC vote is enough for ratification (the tx-zoo only
# casts one vote per action) and so the committee remains viable after 05h
# retires cc-1 (one of two ⇒ one of one ⇒ 100% ≥ 50%).
log_info "Patching conway-genesis.json to seat CC members cc-1 + cc-2"
jq --arg cred1 "keyHash-${CC1_COLD_HASH}" \
   --arg cred2 "keyHash-${CC2_COLD_HASH}" \
   --argjson exp "$CC_EXPIRY_EPOCH" \
   '.committee.members = {($cred1): $exp, ($cred2): $exp}
    | .committee.threshold = {"numerator": 1, "denominator": 2}' \
   "$LD_GENESIS/conway-genesis.json" > "$LD_GENESIS/conway-genesis.patched.json"
mv "$LD_GENESIS/conway-genesis.patched.json" "$LD_GENESIS/conway-genesis.json"
log_info "conway-genesis.committee now: $(jq -c .committee "$LD_GENESIS/conway-genesis.json")"

# Step D.6 (OPT-IN): seat a guardrails script on the constitution.
#
# The Conway `Proposing` ScriptPurpose (redeemer tag 5) is only reachable when
# a proposal names a guardrails policy hash, and Conway's GOV rule requires
# that hash to equal the CURRENT constitution's guardrails script EXACTLY
# (`checkGuardrailsScriptHash`, strict equality including SNothing == SNothing).
#
# That cuts both ways, which is why this is off by default: with a guardrails
# script seated, EVERY ParameterChange and TreasuryWithdrawals proposal must
# name it — including 06b, 06d and 10a, which name none and would start failing
# with `InvalidGuardrailsScriptHash`. So seeding it is a whole-devnet
# configuration change, not a local one.
#
#   LD_SEED_GUARDRAILS=1 ./setup.sh   # enables 13h (Proposing purpose)
#
# The hash used is the always-true V3 script the tx-zoo already vendors, so
# 13h can satisfy it. Any proposal that must pass under this genesis needs
# `--constitution-script-hash <that hash>`.
if [ "${LD_SEED_GUARDRAILS:-0}" = "1" ]; then
    _guard_script="$LD_ROOT/tx-zoo/lib/plutus/always-true-v3.plutus"
    if [ -s "$_guard_script" ]; then
        _guard_hash=$(cardano-cli conway transaction policyid --script-file "$_guard_script")
        jq --arg h "$_guard_hash" '.constitution.script = $h' \
            "$LD_GENESIS/conway-genesis.json" > "$LD_GENESIS/conway-genesis.guard.json"
        mv "$LD_GENESIS/conway-genesis.guard.json" "$LD_GENESIS/conway-genesis.json"
        log_info "LD_SEED_GUARDRAILS=1 — constitution guardrails script = $_guard_hash"
        log_info "  NOTE: every ParameterChange / TreasuryWithdrawals proposal must now"
        log_info "        pass --constitution-script-hash $_guard_hash"
    else
        log_info "LD_SEED_GUARDRAILS=1 but $_guard_script is absent — run tx-zoo/lib/build-plutus.sh first; leaving constitution unguarded"
    fi
fi

# ---- Key reorganization ----
log_info "Reorganizing keys into testnet/local-devnet/keys/"

mkdir -p "$LD_KEYS/pool1" "$LD_KEYS/pool2" "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys"
# $LD_KEYS/cc-1 and $LD_KEYS/cc-2 were already created earlier when we
# bootstrapped the CC members; keep their tightened perms in the chmod
# sweep below.

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
    # pool.id — the bech32 pool id, consumed by 09-cli-parity's pool-scoped
    # queries (pool-state, stake-snapshot, stake-pool-default-vote,
    # leadership-schedule).
    #
    # This file was never written. All four of those parity checks therefore
    # short-circuited to SKIP "pool1 id not found (run setup.sh first)" on
    # EVERY run ever recorded — 4 of the 22-query compared surface silently
    # uncompared, while the release notes read "18 EQUAL" (#953 finding 5).
    # The id was already being computed at Step D.4 for the stake redirect;
    # it just was not persisted.
    cardano-cli conway stake-pool id \
        --cold-verification-key-file "$dst/cold.vkey" \
        --output-bech32 > "$dst/pool.id"
    cardano-cli conway stake-pool id \
        --cold-verification-key-file "$dst/cold.vkey" \
        --output-hex > "$dst/pool.id.hex"
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
chmod 0700 "$LD_KEYS" "$LD_KEYS"/pool* "$LD_KEYS/utxo" "$LD_KEYS/genesis-keys" \
           "$LD_KEYS/cc-1" "$LD_KEYS/cc-2"
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
# The two-forger arbiter (#957) — an independent cardano-node validator peered
# DIRECTLY with both producers. Rendered unconditionally (cheap, and keeps the
# JSON sanity check honest); only started by run.sh in two-forger mode.
render_template "$LD_CONFIG/templates/cardano-arbiter.config.tmpl.json"   "$LD_CONFIG/cardano-arbiter.config.json"
render_template "$LD_CONFIG/templates/cardano-arbiter.topology.tmpl.json" "$LD_CONFIG/cardano-arbiter.topology.json"

# Sanity check — every rendered file must parse as JSON
for f in "$LD_CONFIG"/dugite-*.json "$LD_CONFIG"/cardano-*.json; do
    jq empty "$f" || die "Rendered config $f is not valid JSON"
done

log_info "All configs + topologies rendered to $LD_CONFIG/"
log_info "Setup complete. Next: ./run.sh"
