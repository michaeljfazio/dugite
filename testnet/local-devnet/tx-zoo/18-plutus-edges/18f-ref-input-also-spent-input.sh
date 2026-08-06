#!/usr/bin/env bash
# 18f — the same UTxO declared as BOTH a spent input (--tx-in) and a
# reference input (--read-only-tx-in-reference).
#
# Upstream: tests_plutus_v2/test_spend_ref_inputs_raw.py (~line 239).
#
# No Plutus script needed — disjointness between `inputs` and
# `reference_inputs` is a plain UTXO-level structural rule, independent of
# whether any script is present.
#
# PV-WINDOW CAVEAT (this test is only valid while the devnet stays at PV10):
# dugite phase-1 (crates/dugite-ledger/src/validation/phase1.rs Rule 9)
# enforces disjointness ONLY in the window 8 < PV < 11, mirroring Haskell's
# `disjointRefInputs` exactly (Babbage Rules/Utxo.hs, post cardano-ledger
# PR #5011). At PV >= 11 the rule relaxes for V1/V2/native scripts and the
# equivalent check moves into PlutusV3 TxInfo translation as
# `ConwayContextError::ReferenceInputsNotDisjointFromInputs` (a phase-2
# BadTranslation, not this phase-1 rejection) — see dugite #470. This
# script's plain-payment shape (no Plutus involved at all) would then simply
# be ACCEPTED and this test would need to flip to a positive assertion.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WANT="BabbageNonDisjointRefInputs"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
IN=${UTXO%% *}

RAW="$ZOO_BUILT/$NAME.raw"
if cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$IN" \
        --read-only-tx-in-reference "$IN" \
        --tx-out "${ADDR}+2000000" \
        --change-address "$ADDR" \
        --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    SIGNED="$ZOO_BUILT/$NAME.signed"
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
    # RED-PROOF: flip WANT above to any other constructor and this must FAIL.
    expect_utxo_rejection "$NAME" "$SIGNED" "$WANT"
    exit $?
fi

if grep -qi "$WANT\|disjoint\|overlap" "$ZOO_LOGS/$NAME.err"; then
    zoo_ok "$NAME: refused at build ($WANT)"
    zoo_record "$NAME" PASS "" "rejected-$WANT-at-build"
    exit 0
fi
zoo_fail "$NAME: build failed for a reason unrelated to $WANT: $(tail -2 "$ZOO_LOGS/$NAME.err")"
zoo_record "$NAME" FAIL "" "build-failed-not-$WANT"
exit 1
