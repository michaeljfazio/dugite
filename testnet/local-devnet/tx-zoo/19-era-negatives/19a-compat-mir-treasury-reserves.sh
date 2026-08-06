#!/usr/bin/env bash
# 19a — MIR pot-to-pot transfer certs (#1023, #1034).
#
# Builds a GENUINE Shelley-era transaction (top-level array(3), envelope
# "TxSignedShelley") carrying TWO MoveInstantaneousRewards certificates — a
# reserves<->treasury pot transfer in each direction
# (`create-mir-certificate transfer-to-treasury` / `transfer-to-rewards`,
# CDDL `move_instantaneous_reward = [6, [mir_pot, coin]]`) — and submits it
# against the Conway devnet.
#
# cardano-ledger removed MIRCert entirely at the Conway boundary: dugite's
# Conway decoder hard-rejects certificate tag 6 at CBOR-decode time (#1023,
# era_conway.rs). That fix is scoped to the CONWAY decoder path only — it
# fires when the wire `era_id` is 6/7. This script instead tags the wire
# submission as Shelley (`era_id=1`), which routes dugite through
# `era_shelley::decode_shelley_tx_standalone`
# (crates/dugite-serialization/src/decode/era_shelley.rs) — a genuinely
# DIFFERENT code path that still understands MIR certs (they were valid in
# Shelley). See the category README.md's "IMPORTANT SUBTLETY" section for the
# full mechanism this actually exercises (a real Shelley-shaped tx is
# array(3); dugite's standalone-Shelley decoder currently expects array(4),
# so rejection — if it occurs — is unlikely to *say* "MIR" or "era" at all).
# The assertion below is therefore intentionally reason-agnostic: ANY
# rejection from both observers is a PASS, and the actual wording is recorded
# in the detail field for future triage.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/19-era-negatives/_era-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
era_neg_require_compatible_shelley "$NAME" || exit 0

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO_LINE=$(era_neg_pick_utxo "$ADDR" 300000) \
    || { zoo_record "$NAME" SKIP "" "no-precondition:funding-utxo-too-small"; exit 0; }
read -r TXIN AMT FEE CHANGE <<< "$UTXO_LINE"

CERT_TR="$ZOO_BUILT/$NAME-to-treasury.cert"
CERT_RS="$ZOO_BUILT/$NAME-to-reserves.cert"
cardano-cli compatible shelley governance create-mir-certificate \
    transfer-to-treasury --transfer 1000000 --out-file "$CERT_TR" \
    || { zoo_record "$NAME" FAIL "" "mir-cert-create-to-treasury"; exit 1; }
cardano-cli compatible shelley governance create-mir-certificate \
    transfer-to-rewards --transfer 500000 --out-file "$CERT_RS" \
    || { zoo_record "$NAME" FAIL "" "mir-cert-create-to-reserves"; exit 1; }

SIGNED="$ZOO_BUILT/$NAME.signed"
# RED-PROOF: swap `compatible shelley transaction signed-transaction` below
# for `cardano-cli conway transaction build-raw` + `sign` once, dropping the
# (unsupported-in-Conway-CLI) MIR certs entirely — that produces an ordinary
# accepted current-era tx, and era_neg_assert_rejected_both must then FAIL
# with an "accepted where rejection was expected" detail line.
cardano-cli compatible shelley transaction signed-transaction \
    --tx-in "$TXIN" \
    --tx-out "${ADDR}+${CHANGE}" \
    --certificate-file "$CERT_TR" \
    --certificate-file "$CERT_RS" \
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
