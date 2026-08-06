#!/usr/bin/env bash
# 19c — Genesis key delegation cert (#1023, #1034).
#
# Same mechanism as 19a/19b (see their headers and the category README.md),
# for certificate tag 5 (`GenesisKeyDelegation`,
# CDDL `[5, genesis_hash, genesis_delegate_hash, vrf_hash]`) instead of tag 6
# (MIR).
#
# Unlike MIR (a documented Phase-1 no-op at PV>=9,
# crates/dugite-ledger/src/validation/mir.rs), GenesisKeyDelegation DOES
# carry a live Phase-1 witness requirement — the named genesis key must sign
# (crates/dugite-ledger/src/validation/phase1.rs:170,
# `cert_required_witnesses`) — and dugite's ledger-state APPLY path fully
# supports adopting a genesis delegation with no era gate at all
# (crates/dugite-ledger/src/eras/common.rs
# `enqueue_genesis_key_delegations`/`state/certificates.rs`). So this script
# signs with the REAL genesis key from `$LD_KEYS/genesis-keys/genesis1/`
# (provisioned by setup.sh's `cardano-cli conway genesis create-testnet-data
# --genesis-keys 3`) rather than a throwaway one: a properly-witnessed
# genesis-key-delegation cert is the realistic case, and the ONLY thing this
# script wants to test is whether the wire-level Shelley routing rejects it
# the way Conway's decoder does (#1023) — not an incidental missing-witness
# failure that would reject the tx for the wrong reason.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/19-era-negatives/_era-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
era_neg_require_compatible_shelley "$NAME" || exit 0

GENESIS_VKEY="$LD_KEYS/genesis-keys/genesis1/key.vkey"
GENESIS_SKEY="$LD_KEYS/genesis-keys/genesis1/key.skey"
if [ ! -s "$GENESIS_VKEY" ] || [ ! -s "$GENESIS_SKEY" ]; then
    zoo_record_env_skip "$NAME" "genesis1-keys-missing-under-$LD_KEYS/genesis-keys"
    exit 0
fi

# Throwaway delegate + VRF keys — the DELEGATE being appointed, not the
# authorising genesis key. Self-contained: does not depend on setup.sh
# having preserved $LD_GENESIS/delegate-keys (only genesis-keys/ is copied
# into $LD_KEYS).
DELEG_VKEY="$ZOO_BUILT/$NAME-deleg.vkey"
DELEG_SKEY="$ZOO_BUILT/$NAME-deleg.skey"
DELEG_COUNTER="$ZOO_BUILT/$NAME-deleg.counter"
cardano-cli legacy genesis key-gen-delegate \
    --verification-key-file "$DELEG_VKEY" \
    --signing-key-file "$DELEG_SKEY" \
    --operational-certificate-issue-counter-file "$DELEG_COUNTER" \
    || { zoo_record "$NAME" FAIL "" "delegate-keygen"; exit 1; }

VRF_VKEY="$ZOO_BUILT/$NAME-vrf.vkey"
VRF_SKEY="$ZOO_BUILT/$NAME-vrf.skey"
cardano-cli node key-gen-VRF \
    --verification-key-file "$VRF_VKEY" --signing-key-file "$VRF_SKEY" \
    || { zoo_record "$NAME" FAIL "" "vrf-keygen"; exit 1; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO_LINE=$(era_neg_pick_utxo "$ADDR" 300000) \
    || { zoo_record "$NAME" SKIP "" "no-precondition:funding-utxo-too-small"; exit 0; }
read -r TXIN AMT FEE CHANGE <<< "$UTXO_LINE"

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli compatible shelley governance create-genesis-key-delegation-certificate \
    --genesis-verification-key-file "$GENESIS_VKEY" \
    --genesis-delegate-verification-key-file "$DELEG_VKEY" \
    --vrf-verification-key-file "$VRF_VKEY" \
    --out-file "$CERT" \
    || { zoo_record "$NAME" FAIL "" "gkd-cert-create"; exit 1; }

SIGNED="$ZOO_BUILT/$NAME.signed"
# RED-PROOF: swap `compatible shelley transaction signed-transaction` below
# for a Conway build-raw + sign with the genesis-key-delegation cert dropped
# once — that produces an ordinary accepted current-era tx, and
# era_neg_assert_rejected_both must then FAIL with an "accepted where
# rejection was expected" detail line.
cardano-cli compatible shelley transaction signed-transaction \
    --tx-in "$TXIN" \
    --tx-out "${ADDR}+${CHANGE}" \
    --certificate-file "$CERT" \
    --testnet-magic "$LD_MAGIC" \
    --fee "$FEE" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$GENESIS_SKEY" \
    --out-file "$SIGNED" \
    || { zoo_record "$NAME" FAIL "" "signed-transaction-build-failed"; exit 1; }

TXID=$(cardano-cli conway transaction txid --tx-file "$SIGNED" --output-text 2>/dev/null || echo "")

# #1047: dugite now answers HardForkApplyTxErrWrongEra BEFORE decoding, so
# the reject REASON is assertable. The strict form additionally FAILS if
# dugite rejects via a CBOR decode error — that was the pre-#1047 accident,
# and relying on it would have hidden an accept-where-Haskell-rejects the
# moment any legacy standalone decoder was corrected.
era_neg_assert_wrong_era_both "$NAME" "$SIGNED" "$TXID" era_neg_submit_cli
