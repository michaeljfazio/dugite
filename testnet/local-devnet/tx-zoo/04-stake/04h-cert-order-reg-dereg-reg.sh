#!/usr/bin/env bash
# 04h — submit reg/dereg/reg/dereg/reg (5 certificate-file args, odd count)
# for a FRESH stake key in one tx, mirroring cardano-node-tests'
# test_addr_registration_certificate_order EXACTLY (same file passed 5x).
#
# What actually reaches the wire is NOT 5 certs. `cardano-cli conway
# transaction build` collapses certificate-file arguments through a
# client-side Set/OSet before it ever serialises CBOR: this is confirmed by
# `tx-cbor-tool.py show-certs` on the built raw body, which reports
# cert_count=2, tags=[7,8] (registration, deregistration) — the 3 duplicate
# entries never leave the CLI. Both dugite and cardano-node therefore receive
# and apply the IDENTICAL 2-cert sequence [reg, dereg], never the 5 upstream
# intended to prove an ordering effect with.
#
# This is not a dugite gap or a script bug to route around — it is the exact
# known, still-open cardano-ledger defect upstream's own test documents and
# XFAILs on for every Conway protocol version (9, 10, 11):
# cardano-ledger#4566 "Repeated certificates stripped from Conway
# transaction" (see cardano-node-tests/cardano_node_tests/tests/issues.py
# `ledger_4566`, referenced from
# test_addr_registration.py::test_addr_registration_certificate_order, which
# calls `issues.ledger_4566.finish_test(force_blocked=True)` and never
# reaches its own "must be REGISTERED" assertion at PV9-11).
#
# So this script asserts what ACTUALLY happens end-to-end, which is a parity
# claim rather than the originally-intended ordering claim: after the
# stripped [reg, dereg] pair applies, the stake address ends up
# DEREGISTERED (not registered) on BOTH sockets, and because the deposit is
# charged by the registration then immediately refunded by the
# deregistration IN THE SAME TX, the net deposit effect telescopes to ZERO
# (only the fee is charged) — not "charged exactly once" as originally
# claimed. What this script actually verifies: dugite and cardano-node agree
# byte-for-byte on the (upstream-limited) outcome of this construction.
#
# Upstream precedent: cardano-node-tests stake-registration/deregistration
# ordering coverage (#1032, cardano-node-tests adoption P0.1) — the test this
# script mirrors is itself blocked by cardano-ledger#4566 on every Conway PV.
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
# Pass the same reg/dereg cert files 5x, exactly like upstream's
# certificate_files=[reg, dereg, reg, dereg, reg] — this is EXPECTED to
# collapse to 2 distinct certs at build time (cardano-ledger#4566).
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

# ── Pin the wire-level dedup: exactly 2 certs made it into the body ────────
# This is the load-bearing evidence for the header's claim. If this ever
# reports 5, cardano-ledger#4566 has been fixed upstream and this whole
# script (and its expected final state) needs revisiting.
CERT_COUNT=$(python3 "$ZOO_PY_TX_CBOR" show-certs --in "$RAW" | jq -r '.cert_count')
if [ "$CERT_COUNT" != "2" ]; then
    zoo_fail "expected cardano-ledger#4566 to strip 5 certs down to 2, got $CERT_COUNT — upstream may have fixed the dedup; script needs revisiting"
    zoo_record "$NAME" FAIL "$TXID" "cert-count-changed=$CERT_COUNT"
    exit 1
fi

# ── Final state must be DEREGISTERED on BOTH sockets ────────────────────────
# Not REGISTERED: the effective sequence is [reg, dereg] (see header), so the
# address ends up deregistered again. What matters here is that dugite and
# cardano-node reach the IDENTICAL outcome — a parity claim, not an ordering
# claim.
# RED-PROOF: flip this check to accept "yes" (or skip it) to hide a node that
# diverges from cardano-node on the stripped 2-cert sequence's outcome.
FAIL_SOCKS=""
for sock in "$ZOO_SOCKET" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || continue
    REG=$(cardano-cli conway query stake-address-info \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r 'if length>0 then "yes" else "no" end')
    [ "$REG" = "no" ] || FAIL_SOCKS="$FAIL_SOCKS $sock"
done
if [ -n "$FAIL_SOCKS" ]; then
    zoo_fail "$STAKE_ADDR not DEREGISTERED after the stripped [reg,dereg] sequence on:$FAIL_SOCKS"
    zoo_record "$NAME" FAIL "$TXID" "final-state-not-deregistered$FAIL_SOCKS"
    exit 1
fi

# ── Net deposit telescopes to ZERO (charged then refunded in the same tx) ──
FEE_TEXT=$(cardano-cli debug transaction view --tx-body-file "$RAW" 2>/dev/null | jq -r '.fee')
FEE=${FEE_TEXT%% *}
EXPECTED_CHANGE=$((TXIN_AMT - FEE))
ACTUAL_CHANGE=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r --arg t "$TXID" '[to_entries[] | select(.key | startswith($t))][0].value.value.lovelace // empty')

# RED-PROOF: relax this equality (e.g. to a range check) to hide a deposit
# net-charged or net-refunded when it should telescope to zero.
if [ "${ACTUAL_CHANGE:-}" = "$EXPECTED_CHANGE" ]; then
    zoo_record "$NAME" PASS "$TXID" "deregistered net-deposit=0 (charged+refunded $DEPOSIT) balance=$ACTUAL_CHANGE"
else
    zoo_record "$NAME" FAIL "$TXID" "deposit-mismatch expected=$EXPECTED_CHANGE actual=${ACTUAL_CHANGE:-none}"
    exit 1
fi
