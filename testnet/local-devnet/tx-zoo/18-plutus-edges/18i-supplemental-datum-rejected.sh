#!/usr/bin/env bash
# 18i — a supplemental (unsolicited) datum witness on a V2 inline-datum
# spend.
#
# Upstream: tests_plutus_v2/test_spend_datum_raw.py::test_lock_tx_datum_as_witness
# (verified via `gh api` against the live file, commit ad1430e3d…: locks with
# `use_inline_datum=True`, then builds the redeeming ScriptTxIn with
# `datum_file=plutus_op.datum_file` — i.e. it supplies a datum-HASH-style
# witness for an input whose on-chain datum is INLINE, never declaring
# `--tx-in-inline-datum-present`).
#
# Mechanism (mirrors upstream exactly): lock with an INLINE datum, then
# spend via build-raw supplying ONLY `--tx-in-datum-file` (the datum bytes
# as an explicit witness) and deliberately OMITTING
# `--tx-in-inline-datum-present`. build-raw does no chain cross-check (that
# is the point of "raw"), so it happily emits a tx whose witness set carries
# a datum entry that does not correspond to any by-hash datum reference —
# an unsolicited/supplemental datum from the ledger's point of view.
# Expected upstream assertion (node >= 8.6.0): "NotAllowedSupplementalDatums"
# (the pre-rename text was "NonOutputSupplimentaryDatums").
#
# dugite: crates/dugite-ledger/src/validation/datum.rs's
# `ValidationError::ExtraDatumWitness` is wired to wire tag 12 /
# `NotAllowedSupplementalDatumsUTXOW` in
# crates/dugite-node/src/node/serve.rs's typed N2C mapping (not the
# ScriptFailed-degraded path elsewhere in that file).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WANT="NotAllowedSupplementalDatums"
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}; SCRIPT_AMT=${PAIR##* }
DATUM_FILE="$ZOO_BUILT/$(basename "$SCRIPT" .plutus).datum.json"   # written by plutus_lock

COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
EXUNITS="(1000000,1000000)"
FEE=2000000
REG_OUT=$((SCRIPT_AMT - FEE))
TIP=$(zoo_tip_slot)
TTL=$((TIP + 100))
PPARAMS=$(zoo_pparams_file)

# RED-PROOF: add `--tx-in-inline-datum-present` alongside --tx-in-datum-file
# (declaring the datum correctly, on top of the extra witness) and this
# either fails to build (cardano-cli rejecting the conflicting pair) or, if
# it builds, must ACCEPT — proving the rejection genuinely depends on the
# mismatch between "how the datum is declared" and "how it's actually
# stored on-chain", not on collateral/redeemer/fee plumbing.
RAW="$ZOO_BUILT/$NAME.raw"
cardano-cli conway transaction build-raw \
    --tx-in "$SCRIPT_TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-datum-file "$DATUM_FILE" \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$EXUNITS" \
    --tx-in-collateral "$COLLAT" \
    --tx-out "${ADDR}+${REG_OUT}" \
    --fee "$FEE" \
    --ttl "$TTL" \
    --protocol-params-file "$PPARAMS" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

expect_utxo_rejection "$NAME" "$SIGNED" "$WANT"
