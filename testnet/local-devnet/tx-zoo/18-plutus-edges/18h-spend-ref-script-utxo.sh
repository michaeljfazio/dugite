#!/usr/bin/env bash
# 18h — spend a UTxO that ITSELF carries a reference script (attached to the
# very output being spent). REJECTED, two different ways depending on HOW
# the spend witnesses the script — both confirmed live against dugite-relay
# on 2026-08-06, and both are genuine Conway/Babbage predicate failures, not
# dugite over-strictness.
#
# Upstream: test_spend_reference_script.
#
# HISTORY OF THIS SCRIPT (both premises below were tried and both are
# rejections, not accepts — this test asserts the FIRST, cleanly-encoded
# one):
#
# 1. ORIGINAL premise: spend via an ORDINARY --tx-in-script-file witness
#    while the output ALSO carries a matching referenceScript, expecting
#    dugite to ignore the incidental reference field and ACCEPT.
#    WRONG — oracle-verified (IntersectMBO/cardano-ledger,
#    eras/babbage/impl/src/Cardano/Ledger/Babbage/UTxO.hs
#    `getBabbageScriptsProvided`/`getReferenceScripts`):
#      ins = (txBody ^. referenceInputsTxBodyL) `Set.union` (txBody ^. inputsTxBodyL)
#    — the reference-script pool spans BOTH declared reference inputs AND
#    ordinary SPENT inputs, so a script attached as the very output being
#    spent is ALREADY "provided by reference" with no extra declaration.
#    Babbage's `babbageMissingScripts` (Rules/Utxow.hs) then computes:
#      sRefs         = keysSet (getReferenceScripts utxo inputs)  -- includes SCRIPT_TXIN
#      neededNonRefs = sNeeded `Set.difference` sRefs             -- SCRIPT_TXIN's hash excluded
#      extra         = sReceived `Set.difference` neededNonRefs   -- the ordinary witness IS extra
#    so supplying an ORDINARY witness for a hash already resolvable via that
#    same input's own reference script is itself
#    `ExtraneousScriptWitnessesUTXOW` — confirmed live: dugite correctly
#    REJECTS with a clean, well-formed `ExtraneousScriptWitnessesUTXOW`
#    reply (UTXOW tag 9). This is what the script below asserts.
#
# 2. "CORRECTED" premise tried next: spend via `--spending-tx-in-reference`
#    self-pointing at the very UTxO being spent (the CLI-idiomatic way to
#    witness via a reference script), instead of an ordinary witness. This
#    is ALSO a rejection, but for an entirely different, PV-scoped reason:
#    cardano-cli's `--spending-tx-in-reference X` ALWAYS adds X to the tx's
#    `reference_inputs` list even when X is already `--tx-in` (confirmed via
#    `cardano-cli debug transaction view` on the built body: "reference
#    inputs" duplicates "inputs" verbatim) — so a self-referencing spend is
#    structurally indistinguishable from 18f's plain non-disjoint-inputs
#    case. At this devnet's PV10 (inside dugite's documented 8<PV<11 window,
#    phase1.rs Rule 9 / Haskell's `disjointRefInputs`), that is
#    `BabbageNonDisjointRefInputs` — which hits the SAME dugite wire-encoder
#    bug 18f found (the `ReferenceInputOverlapsInput`/tag-22 payload is
#    wrongly wrapped in a CBOR tag-258 Set marker; Haskell's field type is
#    plain `NonEmpty TxIn`, no tag 258 — oracle-confirmed against
#    cardano-ledger `EncCBOR (NonEmpty a)` — so cardano-cli's decoder dies
#    with `DeserialiseFailure … "expected list len or indef"` instead of
#    seeing a clean rejection). That failure mode is real but belongs to
#    18f's report, not this script; asserting on it here would make 18h's
#    pass/fail depend on 18f's already-filed bug AND would need re-scoping
#    once PV11 relaxes the rule. Reverted to variant 1, which is clean at
#    every PV this devnet exercises and does not depend on any other open
#    issue.
#
# Different from 03h (which uses a SEPARATE output purely to carry the
# reference script for a DIFFERENT spend via --spending-tx-in-reference).
#
# Does not go through _lock-helper.sh's plutus_lock: that helper has no way
# to attach --tx-out-reference-script-file, so the lock step is written out
# here directly (same shape as 03h's step 1).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
SCRIPT_ADDR_FILE="$ZOO_BUILT/$(basename "$SCRIPT" .plutus)-18h.addr"
cardano-cli conway address build \
    --payment-script-file "$SCRIPT" \
    --testnet-magic "$LD_MAGIC" \
    --out-file "$SCRIPT_ADDR_FILE"
SCRIPT_ADDR=$(cat "$SCRIPT_ADDR_FILE")

DATUM_FILE="$ZOO_BUILT/$NAME.datum.json"
echo '{"int": 42}' > "$DATUM_FILE"

zoo_wait_mempool_quiet 90 || true
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }

# ---- Lock: SAME output carries BOTH the inline datum AND the reference
# script it will (incidentally) later be spent with a direct witness for. ----
LOCK_RAW="$ZOO_BUILT/$NAME-lock.raw"
LOCK_SIGNED="$ZOO_BUILT/$NAME-lock.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${SCRIPT_ADDR}+5000000" \
    --tx-out-inline-datum-file "$DATUM_FILE" \
    --tx-out-reference-script-file "$SCRIPT" \
    --change-address "$ADDR" \
    --out-file "$LOCK_RAW" >/dev/null 2> "$ZOO_LOGS/$NAME-lock.err" \
    || { zoo_fail "lock build: $(tail -2 "$ZOO_LOGS/$NAME-lock.err")"; zoo_record "$NAME" FAIL "" "lock-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$LOCK_RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$LOCK_SIGNED" >/dev/null
LOCK_TXID=$(zoo_submit "$LOCK_SIGNED") || { zoo_record "$NAME" FAIL "" "lock-submit"; exit 1; }
zoo_wait_inclusion "$LOCK_TXID" 90 || { zoo_record "$NAME" FAIL "$LOCK_TXID" "lock-not-included"; exit 1; }

TMP=$(mktemp)
cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$SCRIPT_ADDR" --out-file "$TMP"
SCRIPT_TXIN=$(jq -r --arg t "$LOCK_TXID" '
    to_entries
    | map(select(.key | startswith($t)))
    | map(select(.value.referenceScript != null and .value.inlineDatum != null))
    | .[0].key // empty' "$TMP")
rm -f "$TMP"
[ -z "$SCRIPT_TXIN" ] && {
    zoo_fail "could not locate the locked output carrying BOTH inlineDatum and referenceScript"
    zoo_record "$NAME" FAIL "$LOCK_TXID" "no-lock-output"; exit 1
}

# ---- Spend it directly via an ORDINARY --tx-in-script-file witness, while
# the very output being spent ALSO carries a matching referenceScript. Per
# the header, that reference-script presence makes the ordinary witness
# copy extraneous. ----
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
RAW="$ZOO_BUILT/$NAME.raw"

# RED-PROOF: flip WANT below to any other constructor and this must FAIL.
WANT="ExtraneousScriptWitnessesUTXOW"

cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral "$COLLAT" \
    --tx-out "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "spend build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "spend-build"; exit 1; }
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

expect_utxo_rejection "$NAME" "$SIGNED" "$WANT"
exit $?
