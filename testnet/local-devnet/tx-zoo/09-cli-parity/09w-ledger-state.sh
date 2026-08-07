#!/usr/bin/env bash
# 09w — query ledger-state (Ledger.NewEpochState)
#
# WHY THIS EXISTS (#1027)
# -----------------------
# `cardano-cli conway query ledger-state` was completely undecodable against
# dugite and NOTHING in this harness noticed, because nothing ran it. The only
# reference to `ledger-state` under testnet/local-devnet/ was two-forger-round.sh,
# which queries it against the HASKELL arbiter only — never against dugite's own
# socket. The query had therefore never been exercised against dugite at all.
#
# The failure mode is why this needs its own script rather than a plain
# `parity_query_json` row: when cardano-cli cannot decode the reply into
# `NewEpochState` it does NOT fail. It **exits 0** and prints a raw-CBOR
# diagnostic dump to stdout instead of JSON. A naive rc check passes; a naive
# sha compare says DIVERGENT without saying why. So the first assertion here is
# the literal property #1027 is about: *dugite's answer parses as JSON*.
#
# The root cause was one field — `PoolDistr.pdTotalActiveStake` is a
# `NonZero Coin` upstream and dugite encoded a literal 0 whenever the chain had
# no active stake, which made the whole reply undecodable.
#
# Values are NOT compared. Two independently-forging nodes legitimately differ
# in snapshot/pot detail at any given tip, and a value compare would be flaky.
# What is compared is the SHAPE: the set of key paths each side emits. That is
# what catches a missing sub-record (`dsGenDelegs` was hardcoded empty) or a
# spurious one, which is the failure class this query has actually exhibited.
set -euo pipefail

. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

QUERY_NAME="ledger-state"

# Depth-limited key-path set. Full-depth paths include per-pool / per-credential
# hashes, which differ between nodes by design; depth 4 keeps the record
# structure (esLState/utxoState/ppups/currentPParams …) and drops the data.
#
# #1067 LANDED, so the `likelihoodsNM` exclusion that used to sit here is GONE.
# dugite now tracks per-pool `Likelihood` and the frozen `rewardPotNM`, so
# `esNonMyopic` is compared on the same strict footing as every other subtree —
# which is the whole point of having removed the scaffolding rather than
# widening it.
PATHS_JQ='[paths | select(length <= 4) | select(all(.[]; type == "string")) | join(".")] | unique'

if [ "${PARITY_MODE:-exact}" = "skip" ]; then
    parity_record "$QUERY_NAME" "SKIP" "skip" "skip" "skip-mode"
    exit 0
fi

# Pin the tip across both sockets, same rationale as parity_query_json.
attempt=0
while :; do
    t0_d=$(_parity_tip "$LD_DUGITE_BP_SOCK")
    t0_c=$(_parity_tip "$LD_CARDANO_BP_SOCK")

    dugite_out=$(cardano-cli conway query ledger-state \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_DUGITE_BP_SOCK" \
                    --output-json 2>&1) && dugite_rc=0 || dugite_rc=$?
    cardano_out=$(cardano-cli conway query ledger-state \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_CARDANO_BP_SOCK" \
                    --output-json 2>&1) && cardano_rc=0 || cardano_rc=$?

    t1_d=$(_parity_tip "$LD_DUGITE_BP_SOCK")
    t1_c=$(_parity_tip "$LD_CARDANO_BP_SOCK")
    if [ "$t0_d" = "$t1_d" ] && [ "$t0_c" = "$t1_c" ] && [ "$t0_d" = "$t0_c" ]; then
        break
    fi
    attempt=$(( attempt + 1 ))
    if [ "$attempt" -ge "${PARITY_TIP_RETRIES:-20}" ]; then
        parity_record "$QUERY_NAME" "SKIP" "unstable" "unstable" \
            "TIP_UNSTABLE after $attempt attempts"
        exit 0
    fi
    sleep 2
done

if [ "$dugite_rc" -ne 0 ] || [ "$cardano_rc" -ne 0 ]; then
    if [ "$dugite_rc" -ne 0 ] && [ "$cardano_rc" -ne 0 ]; then
        note="HARNESS both-sides-failed rc=$dugite_rc/$cardano_rc: $(echo "$cardano_out" | head -1)"
    elif [ "$dugite_rc" -ne 0 ]; then
        note="dugite ERROR rc=$dugite_rc: $(echo "$dugite_out" | head -1)"
    else
        note="cardano ERROR rc=$cardano_rc: $(echo "$cardano_out" | head -1)"
    fi
    parity_record "$QUERY_NAME" "ERROR" "error" "error" "$note"
    exit 2
fi

# ---- Assertion 1: the reply DECODES (this is #1027) ------------------------
# cardano-cli exits 0 and prints a CBOR diagnostic dump when it cannot decode
# NewEpochState, so rc is not the signal — parseability is.
diff_dir="$(dirname "$PARITY_CSV")/cli-parity-diffs"
for side in dugite cardano; do
    eval "out=\$${side}_out"
    if ! printf '%s' "$out" | jq -e 'type == "object"' >/dev/null 2>&1; then
        mkdir -p "$diff_dir"
        printf '%s\n' "$out" > "$diff_dir/ledger-state.${side}.raw"
        parity_record "$QUERY_NAME" "DIVERGENT" "unparsed" "unparsed" \
            "$side answer is NOT JSON — cardano-cli fell back to its raw-CBOR diagnostic printer (the #1027 signature); raw output in cli-parity-diffs/ledger-state.${side}.raw"
        exit 1
    fi
done

# ---- Assertion 2: same record SHAPE ----------------------------------------
dugite_paths=$(printf '%s' "$dugite_out" | jq -c "$PATHS_JQ")
cardano_paths=$(printf '%s' "$cardano_out" | jq -c "$PATHS_JQ")
n_paths=$(printf '%s' "$cardano_paths" | jq -r 'length')

# Vacuity guard: a shape comparison over a near-empty path set proves nothing.
# The reference document has well over a hundred paths at depth <= 4; if it
# somehow has almost none, the comparison is not measuring the structure and
# must say so rather than pass.
if [ "${n_paths:-0}" -lt 20 ]; then
    parity_record "$QUERY_NAME" "SKIP" "vacuous" "vacuous" \
        "INCONCLUSIVE: reference ledger-state exposed only ${n_paths:-0} key paths at depth<=4; too few to compare a structure against"
    exit 0
fi

if [ "$dugite_paths" != "$cardano_paths" ]; then
    mkdir -p "$diff_dir"
    printf '%s' "$dugite_paths"  | jq -r '.[]' > "$diff_dir/ledger-state.dugite.paths"
    printf '%s' "$cardano_paths" | jq -r '.[]' > "$diff_dir/ledger-state.cardano.paths"
    diff -u "$diff_dir/ledger-state.cardano.paths" "$diff_dir/ledger-state.dugite.paths" \
        > "$diff_dir/ledger-state.paths.diff" 2>/dev/null || true
    difflines=$(grep -cE '^[+-][^+-]' "$diff_dir/ledger-state.paths.diff" 2>/dev/null || echo 0)
    parity_record "$QUERY_NAME" "DIVERGENT" "shape" "shape" \
        "NewEpochState record shape differs: ${difflines} path(s); see cli-parity-diffs/ledger-state.paths.diff"
    exit 1
fi

parity_record "$QUERY_NAME" "COMPARED" "decoded" "decoded" \
    "decodes as NewEpochState JSON on both sockets; ${n_paths} key paths identical (values not compared)"
exit 0
