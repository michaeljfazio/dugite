#!/usr/bin/env bash
# 19d — Legacy Shelley protocol-parameters update proposal in tx-body key 6
# (#1023-adjacent, #1034).
#
# Same wire mechanism as 19a-19c (see their headers and the category
# README.md), but exercising a tx-body FIELD rather than a certificate.
# Shelley-era `update = [{genesis_key_hash => protocol_param_update},
# epoch]` lived at tx-body key 6. Conway's `ConwayTxBodyRaw` repurposes key 6
# entirely for `proposal_procedures` (an `OSet ProposalProcedure`,
# CIP-1694) — the two are structurally incompatible, and there is no
# `update` field left anywhere in ConwayTxBodyRaw. Unlike 19a-19c this is
# not a per-certificate rejection at all: it is a body-shape divergence at
# the tx-body level, so the CDDL question is "does a decoder built around
# ConwayTxBodyRaw's key set know what to do with an `update` value sitting
# at key 6" rather than "does it recognise a specific certificate tag".
#
# `cardano-cli compatible shelley governance action
# create-protocol-parameters-update` requires a genesis verification key
# (the proposer) but the proposal itself does not need to be a real,
# authorisable one for this test — see the category README.md's mechanism
# analysis for why rejection here (if it occurs) is very unlikely to name
# "update" or "key 6" specifically.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/19-era-negatives/_era-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
era_neg_require_compatible_shelley "$NAME" || exit 0

GENESIS_VKEY="$LD_KEYS/genesis-keys/genesis1/key.vkey"
if [ ! -s "$GENESIS_VKEY" ]; then
    zoo_record_env_skip "$NAME" "genesis1-keys-missing-under-$LD_KEYS/genesis-keys"
    exit 0
fi

TIP_EPOCH=$(zoo_tip_epoch)
PROPOSAL="$ZOO_BUILT/$NAME.update-proposal"
cardano-cli compatible shelley governance action create-protocol-parameters-update \
    --epoch "$((TIP_EPOCH + 1))" \
    --genesis-verification-key-file "$GENESIS_VKEY" \
    --min-fee-linear 45 \
    --out-file "$PROPOSAL" \
    || { zoo_record "$NAME" FAIL "" "update-proposal-create"; exit 1; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO_LINE=$(era_neg_pick_utxo "$ADDR" 300000) \
    || { zoo_record "$NAME" SKIP "" "no-precondition:funding-utxo-too-small"; exit 0; }
read -r TXIN AMT FEE CHANGE <<< "$UTXO_LINE"

SIGNED="$ZOO_BUILT/$NAME.signed"
# RED-PROOF: swap `compatible shelley transaction signed-transaction` below
# for a Conway build-raw + sign with the update-proposal-file dropped once —
# that produces an ordinary accepted current-era tx, and
# era_neg_assert_rejected_both must then FAIL with an "accepted where
# rejection was expected" detail line.
cardano-cli compatible shelley transaction signed-transaction \
    --tx-in "$TXIN" \
    --tx-out "${ADDR}+${CHANGE}" \
    --update-proposal-file "$PROPOSAL" \
    --testnet-magic "$LD_MAGIC" \
    --fee "$FEE" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$SIGNED" \
    || { zoo_record "$NAME" FAIL "" "signed-transaction-build-failed"; exit 1; }

TXID=$(cardano-cli conway transaction txid --tx-file "$SIGNED" --output-text 2>/dev/null || echo "")

# #1047: dugite now answers HardForkApplyTxErrWrongEra BEFORE decoding, so
# the reject REASON is assertable. The strict form additionally FAILS if
# dugite rejects via a CBOR decode error — that was the pre-#1047 accident,
# and relying on it would have hidden an accept-where-Haskell-rejects the
# moment any legacy standalone decoder was corrected.
era_neg_assert_wrong_era_both "$NAME" "$SIGNED" "$TXID" era_neg_submit_cli
