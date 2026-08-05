#!/usr/bin/env bash
# 04h — one tx carrying reg/dereg/reg/dereg/reg (5 certs, odd count) for a
# FRESH stake key. Certificates apply in sequence WITHIN one tx, so the final
# state must be REGISTERED and the net deposit charged exactly once (the
# alternating +D/-D/+D/-D/+D telescopes to a single +D).
#
# Upstream precedent: cardano-node-tests stake-registration/deregistration
# ordering coverage (#1032, cardano-node-tests adoption P0.1).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

KDIR="$ZOO_KEYS/$NAME"
mkdir -p "$KDIR"
if [ ! -s "$KDIR/payment.skey" ]; then
    cardano-cli conway address key-gen \
        --verification-key-file "$KDIR/payment.vkey" \
        --signing-key-file      "$KDIR/payment.skey" >/dev/null
fi
if [ ! -s "$KDIR/stake.skey" ]; then
    cardano-cli conway stake-address key-gen \
        --verification-key-file "$KDIR/stake.vkey" \
        --signing-key-file      "$KDIR/stake.skey" >/dev/null
fi
if [ ! -s "$KDIR/payment-stake.addr" ]; then
    cardano-cli conway address build \
        --payment-verification-key-file "$KDIR/payment.vkey" \
        --stake-verification-key-file   "$KDIR/stake.vkey" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$KDIR/payment-stake.addr" >/dev/null
fi
if [ ! -s "$KDIR/stake.addr" ]; then
    cardano-cli conway stake-address build \
        --stake-verification-key-file "$KDIR/stake.vkey" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$KDIR/stake.addr" >/dev/null
fi
ADDR=$(cat "$KDIR/payment-stake.addr")
STAKE_ADDR=$(cat "$KDIR/stake.addr")

# Fund the fresh wallet from the genesis funder if it has no UTxO yet — a
# brand-new key pair starts with zero balance every run.
EXISTING=$(zoo_largest_utxo "$ADDR" 2>/dev/null) || EXISTING=""
if [ -z "$EXISTING" ]; then
    FUND_ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
    FUND_UTXO=$(zoo_largest_utxo "$FUND_ADDR") || { zoo_record "$NAME" FAIL "" "no-fund-utxo"; exit 1; }
    FTXIN=${FUND_UTXO%% *}
    FUND_RAW="$ZOO_BUILT/$NAME-fund.raw"
    FUND_SIGNED="$ZOO_BUILT/$NAME-fund.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$FTXIN" --tx-out "${ADDR}+20000000" \
        --change-address "$FUND_ADDR" --out-file "$FUND_RAW" >/dev/null \
        || { zoo_record "$NAME" FAIL "" "fund-build"; exit 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" --tx-body-file "$FUND_RAW" \
        --signing-key-file "$ZOO_PAY_SKEY" --out-file "$FUND_SIGNED" >/dev/null
    FUND_TXID=$(zoo_submit "$FUND_SIGNED") || { zoo_record "$NAME" FAIL "" "fund-submit"; exit 1; }
    zoo_wait_inclusion "$FUND_TXID" 60 || { zoo_record "$NAME" FAIL "$FUND_TXID" "fund-not-included"; exit 1; }
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")

REG_CERT="$ZOO_BUILT/$NAME.reg.cert"
DEREG_CERT="$ZOO_BUILT/$NAME.dereg.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-verification-key-file "$KDIR/stake.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$REG_CERT"
cardano-cli conway stake-address deregistration-certificate \
    --stake-verification-key-file "$KDIR/stake.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$DEREG_CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo-after-fund"; exit 1; }
TXIN=${UTXO%% *}
TXIN_AMT=${UTXO##* }
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
# Order is the point: reg, dereg, reg, dereg, reg — 5 certs, odd count, so
# the final ledger state must be REGISTERED.
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --certificate-file "$REG_CERT" \
    --certificate-file "$DEREG_CERT" \
    --certificate-file "$REG_CERT" \
    --certificate-file "$DEREG_CERT" \
    --certificate-file "$REG_CERT" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$KDIR/payment.skey" \
    --signing-key-file "$KDIR/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }

if ! zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi

# ── Final state must be REGISTERED on BOTH sockets ──────────────────────────
# RED-PROOF: flip this check to accept "no" (or skip it) to hide a node that
# processes the 5 certs out of order and ends up deregistered.
FAIL_SOCKS=""
for sock in "$ZOO_SOCKET" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || continue
    REG=$(cardano-cli conway query stake-address-info \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r 'if length>0 then "yes" else "no" end')
    [ "$REG" = "yes" ] || FAIL_SOCKS="$FAIL_SOCKS $sock"
done
if [ -n "$FAIL_SOCKS" ]; then
    zoo_fail "$STAKE_ADDR not REGISTERED after reg/dereg/reg/dereg/reg on:$FAIL_SOCKS"
    zoo_record "$NAME" FAIL "$TXID" "final-state-not-registered$FAIL_SOCKS"
    exit 1
fi

# ── Net deposit charged exactly once ────────────────────────────────────────
FEE_TEXT=$(cardano-cli debug transaction view --tx-body-file "$RAW" 2>/dev/null | jq -r '.fee')
FEE=${FEE_TEXT%% *}
EXPECTED_CHANGE=$((TXIN_AMT - FEE - DEPOSIT))
ACTUAL_CHANGE=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r --arg t "$TXID" '[to_entries[] | select(.key | startswith($t))][0].value.value.lovelace // empty')

# RED-PROOF: relax this equality (e.g. to a range check) to hide a deposit
# charged twice, zero times, or refunded at the wrong step.
if [ "${ACTUAL_CHANGE:-}" = "$EXPECTED_CHANGE" ]; then
    zoo_record "$NAME" PASS "$TXID" "registered deposit=$DEPOSIT balance=$ACTUAL_CHANGE"
else
    zoo_record "$NAME" FAIL "$TXID" "deposit-mismatch expected=$EXPECTED_CHANGE actual=${ACTUAL_CHANGE:-none}"
    exit 1
fi
