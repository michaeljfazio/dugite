#!/usr/bin/env bash
# 19b — MIR stake-address payout certs (#1023, #1034).
#
# Same mechanism as 19a (see its header and the category README.md), but
# using the OTHER MIRTarget shape: `StakeAddressesMIR`
# (`create-mir-certificate --treasury/--reserves --stake-address ADDR
# --reward N`, CDDL `[6, [mir_pot, {credential => delta_coin}]]`) instead of
# `SendToOppositePotMIR`. Two certs, one per source pot, in one tx — the
# credential does not need to be registered: dugite's MIR predicate suite is
# a documented no-op at PV>=9 regardless
# (crates/dugite-ledger/src/validation/mir.rs), so registration state is not
# what this script is testing.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/19-era-negatives/_era-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
era_neg_require_compatible_shelley "$NAME" || exit 0

# A throwaway stake credential — self-contained, does not depend on any
# other category's wallet state having run first.
STAKE_VKEY="$ZOO_BUILT/$NAME-stake.vkey"
STAKE_SKEY="$ZOO_BUILT/$NAME-stake.skey"
STAKE_ADDR_FILE="$ZOO_BUILT/$NAME-stake.addr"
cardano-cli conway stake-address key-gen \
    --verification-key-file "$STAKE_VKEY" --signing-key-file "$STAKE_SKEY" \
    || { zoo_record "$NAME" FAIL "" "stake-keygen"; exit 1; }
cardano-cli conway stake-address build \
    --stake-verification-key-file "$STAKE_VKEY" \
    --testnet-magic "$LD_MAGIC" \
    --out-file "$STAKE_ADDR_FILE" \
    || { zoo_record "$NAME" FAIL "" "stake-addr-build"; exit 1; }
STAKE_ADDR=$(cat "$STAKE_ADDR_FILE")

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO_LINE=$(era_neg_pick_utxo "$ADDR" 300000) \
    || { zoo_record "$NAME" SKIP "" "no-precondition:funding-utxo-too-small"; exit 0; }
read -r TXIN AMT FEE CHANGE <<< "$UTXO_LINE"

CERT_TR="$ZOO_BUILT/$NAME-pay-treasury.cert"
CERT_RS="$ZOO_BUILT/$NAME-pay-reserves.cert"
cardano-cli compatible shelley governance create-mir-certificate \
    --treasury --stake-address "$STAKE_ADDR" --reward 100000 \
    --out-file "$CERT_TR" \
    || { zoo_record "$NAME" FAIL "" "mir-cert-create-pay-treasury"; exit 1; }
cardano-cli compatible shelley governance create-mir-certificate \
    --reserves --stake-address "$STAKE_ADDR" --reward 50000 \
    --out-file "$CERT_RS" \
    || { zoo_record "$NAME" FAIL "" "mir-cert-create-pay-reserves"; exit 1; }

SIGNED="$ZOO_BUILT/$NAME.signed"
# RED-PROOF: swap `compatible shelley transaction signed-transaction` below
# for a Conway build-raw + sign with the MIR certs dropped once — that
# produces an ordinary accepted current-era tx, and
# era_neg_assert_rejected_both must then FAIL with an "accepted where
# rejection was expected" detail line.
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

era_neg_assert_rejected_both "$NAME" "$SIGNED" "$TXID" era_neg_submit_cli
