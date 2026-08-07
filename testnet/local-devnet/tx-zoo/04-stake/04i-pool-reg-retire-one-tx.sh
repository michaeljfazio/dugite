#!/usr/bin/env bash
# 04i — pool registration + retirement certs in the SAME tx. The pool does
# not exist until the registration cert applies earlier in the same
# certificate sequence, so this exercises intra-tx cert ordering for POOL
# the way 04h exercises it for DELEG.
#
# Upstream precedent: cardano-node-tests pool lifecycle coverage (#1032,
# cardano-node-tests adoption P0.1).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
[ -s "$WA/stake.vkey" ] || die "wallet-a stake key missing — run setup"

POOL="$ZOO_KEYS/$NAME"
mkdir -p "$POOL"
if [ ! -s "$POOL/cold.skey" ]; then
    cardano-cli conway node key-gen \
        --cold-verification-key-file "$POOL/cold.vkey" \
        --cold-signing-key-file      "$POOL/cold.skey" \
        --operational-certificate-issue-counter-file "$POOL/counter" >/dev/null
fi
if [ ! -s "$POOL/vrf.skey" ]; then
    cardano-cli conway node key-gen-VRF \
        --verification-key-file "$POOL/vrf.vkey" \
        --signing-key-file      "$POOL/vrf.skey" >/dev/null
fi

ADDR=$(cat "$WA/payment-stake.addr")
PPARAMS=$(zoo_pparams_file)
POOL_DEPOSIT=$(jq -r '.stakePoolDeposit' "$PPARAMS")
MIN_POOL_COST=$(jq -r '.minPoolCost' "$PPARAMS")
CURRENT_EPOCH=$(zoo_tip_epoch)
RETIRE_EPOCH=$((CURRENT_EPOCH + 2))   # earliest valid retire epoch, as in 04f

# run-all.sh normally starts the shared anchor HTTP server once, before any
# script runs. When this script runs standalone (as it did on first attempt)
# nothing has seeded $ZOO_ANCHOR_DIR yet, so `zoo_anchor_hash pool3` dies with
# "anchor file missing". zoo_anchor_start is idempotent (checks its pid file
# first) so it is always safe to call here too.
zoo_anchor_start >/dev/null 2>&1

REG_CERT="$ZOO_BUILT/$NAME.reg.cert"
DEREG_CERT="$ZOO_BUILT/$NAME.dereg.cert"
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file "$POOL/cold.vkey" \
    --vrf-verification-key-file  "$POOL/vrf.vkey" \
    --pool-pledge   1000000 \
    --pool-cost     "$MIN_POOL_COST" \
    --pool-margin   0.05 \
    --pool-reward-account-verification-key-file "$WA/stake.vkey" \
    --pool-owner-stake-verification-key-file    "$WA/stake.vkey" \
    --single-host-pool-relay 127.0.0.1 --pool-relay-port 3098 \
    --metadata-url   "$(zoo_anchor_url pool3)" \
    --metadata-hash  "$(zoo_anchor_hash pool3)" \
    --testnet-magic  "$LD_MAGIC" \
    --out-file       "$REG_CERT"
cardano-cli conway stake-pool deregistration-certificate \
    --cold-verification-key-file "$POOL/cold.vkey" \
    --epoch "$RETIRE_EPOCH" \
    --out-file "$DEREG_CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --certificate-file "$REG_CERT" \
    --certificate-file "$DEREG_CERT" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$WA/stake.skey" \
    --signing-key-file "$POOL/cold.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }

if ! zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi

POOL_ID_HEX=$(cardano-cli conway stake-pool id \
    --cold-verification-key-file "$POOL/cold.vkey" --output-hex)

# ── Retirement scheduled on BOTH sockets ────────────────────────────────────
# `query pool-state` is the current name (`pool-params` is the deprecated
# alias). Confirmed live shape (both dugite and cardano-node):
#   { "<pool-id-hex>": { "poolParams": {...}, "futurePoolParams": null,
#                          "retiring": <epoch> } }
# i.e. the retirement epoch is a plain integer keyed by pool id at the TOP
# level of that pool's object — not nested under a `poolRetiring`/`retiring`
# map as originally guessed.
# RED-PROOF: relax the `has($id)` test (or drop the epoch equality below) to hide
# a retirement that never got scheduled, or got scheduled for the wrong epoch.
# THREE outcomes, not two. The previous version ran the query with `2>/dev/null`
# and tested the result with `grep -q "$POOL_ID_HEX"`, so a query that never
# produced an answer was reported as `pool-not-found` — identical to the verdict
# for a pool the ledger genuinely never registered. That is the wrong shape for a
# diagnostic: an UNREADABLE answer is not a failed assertion, and reporting it as
# one sends the reader looking for a ledger bug that is not there.
#
# Measured 2026-08-07: 04i reported `pool-not-found` on the dugite relay while
# cardano-node reported the pool with the correct retirement epoch one second
# later. Everything needed to attribute that had been discarded — the exit code
# and stderr both went to /dev/null. The node was subsequently shown correct
# (12/12 stable queries, byte-identical JSON to cardano-node), so the run had
# thrown away the only evidence that mattered.
#
# Note what is deliberately NOT retried: ABSENCE. A new pool registration lands in
# `psStakePoolParams` immediately (cardano-ledger `Shelley.Rules.Pool`, the
# `hk ∉ dom poolParams` branch), so once the tx is in an applied block the pool
# MUST be visible on the next successful query. Retrying until it appears would
# convert a real staleness bug into a pass — fitting the gate instead of the node.
# Only an unanswered query is retried.
FAIL_SOCKS=""
for sock in "$ZOO_SOCKET" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || continue
    OUT=""; QRC=1; QERR=""
    for attempt in 1 2 3; do
        QERR_FILE="$ZOO_LOGS/$NAME.pool-state.$(basename "$sock").$attempt.err"
        if OUT=$(cardano-cli conway query pool-state \
                    --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
                    --stake-pool-id "$POOL_ID_HEX" --output-json 2>"$QERR_FILE"); then
            QRC=0
            break
        fi
        # `|| true`: this file is created by the 2> redirection above so it should
        # always exist, but the whole point of this block is to survive the case
        # where something unexpected happened — `set -euo pipefail` would turn a
        # failed read of the diagnostic into an exit that discards the diagnostic.
        QERR=$( { tail -2 "$QERR_FILE" 2>/dev/null || true; } | tr -d '\r' | tr '\n' ' ')
        sleep 2
    done
    if [ "$QRC" -ne 0 ]; then
        FAIL_SOCKS="$FAIL_SOCKS $sock(query-failed-3x:${QERR:-no-stderr})"
        continue
    fi
    # cardano-cli exits 0 on a reply it cannot decode, printing a raw CBOR dump
    # instead of JSON — so a zero exit is not proof of a usable answer. Require
    # the output to parse as a JSON object before drawing any conclusion from it.
    if ! printf '%s' "$OUT" | jq -e 'type == "object"' >/dev/null 2>&1; then
        FAIL_SOCKS="$FAIL_SOCKS $sock(undecodable-reply)"
        continue
    fi
    if ! printf '%s' "$OUT" | jq -e --arg id "$POOL_ID_HEX" 'has($id)' >/dev/null 2>&1; then
        FAIL_SOCKS="$FAIL_SOCKS $sock(pool-absent-from-decoded-reply)"
        continue
    fi
    RETIRING_EPOCH=$(printf '%s' "$OUT" | jq -r --arg id "$POOL_ID_HEX" \
        '(.[$id].retiring) // empty')
    if [ "$RETIRING_EPOCH" != "$RETIRE_EPOCH" ]; then
        FAIL_SOCKS="$FAIL_SOCKS $sock(retiring=${RETIRING_EPOCH:-none})"
    fi
done

if [ -n "$FAIL_SOCKS" ]; then
    zoo_fail "pool $POOL_ID_HEX retirement not scheduled at epoch $RETIRE_EPOCH on:$FAIL_SOCKS"
    zoo_record "$NAME" FAIL "$TXID" "retirement-not-scheduled$FAIL_SOCKS"
    exit 1
fi
zoo_record "$NAME" PASS "$TXID" "pool=${POOL_ID_HEX:0:16} retire_at=$RETIRE_EPOCH deposit=$POOL_DEPOSIT"
