#!/usr/bin/env bash
# 11f — two independent txs concurrently reference the SAME read-only input
# (CIP-31), extending 03i's single-tx reference-input wire-shape exercise
# into a mempool-concurrency check. A reference input is never consumed, so
# two txs racing to read it (never to spend it) must both be accepted and
# both included — proving dugite does not mistakenly treat a shared
# reference input as a spend conflict. Upstream precedent: cardano-node-tests
# CIP-31 concurrent-reference coverage (#1032, cardano-node-tests adoption
# P0.1).
#
# First live run recorded FAIL at the "ref-setup-submit" step (i.e. before
# the thing under test — txA/txB — even ran) with the CLI's generic
# `ConwayMempoolFailure "transaction validation failed"` wrapper. Root-caused
# via dugite-relay.log at the matching timestamp:
#   "N2C tx rejected: mempool add failed after validator Ok (duplicate or
#   full) ... reason=Input conflict: input already claimed by mempool tx
#   a608ee71..."
# — an ordinary shared-funder input conflict (the "11c lesson, #918": other
# tx-zoo scripts had pending txs against the same $ZOO_PAY_ADDR_FILE UTxO at
# that moment), unrelated to reference inputs. 11f was the only 11-mempool
# script missing the `zoo_wait_mempool_quiet` guard 11e/11a-c already carry
# for exactly this race.
#
# VERDICT: script bug, not a dugite bug. Reproduced 5/5 clean runs after
# adding the guard below — both txA and txB accepted and included every
# time, reference input still present and unspent afterward. This confirms
# dugite does NOT treat a shared read-only reference input as a spend
# conflict; the generic wire error above was the mempool's real (and
# correct) input-conflict rejection of the unrelated setup tx.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

# Earlier scripts may still have transactions in flight against the shared
# genesis funder; building on a UTxO the ledger view reports but that a
# pending tx has already claimed is an unavoidable input-conflict at submit
# time (the 11c lesson, #918 — see 11e for the same guard).
zoo_wait_mempool_quiet 90 || true

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# ── Setup: one tx creates the ref-input UTxO, a second creates the two
#    disjoint spending inputs txA/txB need. Both wait for inclusion — this
#    is setup, not the thing under test. ────────────────────────────────────
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

REF_RAW="$ZOO_BUILT/$NAME-ref.raw"
REF_SIGNED="$ZOO_BUILT/$NAME-ref.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$TXIN" --tx-out "${ADDR}+3000000" \
    --change-address "$ADDR" --out-file "$REF_RAW" >/dev/null \
    || { zoo_record "$NAME" FAIL "" "ref-setup-build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" --tx-body-file "$REF_RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" --out-file "$REF_SIGNED" >/dev/null
REF_TXID=$(zoo_submit "$REF_SIGNED") || { zoo_record "$NAME" FAIL "" "ref-setup-submit"; exit 1; }
zoo_wait_inclusion "$REF_TXID" 60 || { zoo_record "$NAME" FAIL "$REF_TXID" "ref-setup-not-included"; exit 1; }
REF_IN="${REF_TXID}#0"

UTXO2=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo-2"; exit 1; }
TXIN2=${UTXO2%% *}
SPEND_RAW="$ZOO_BUILT/$NAME-spend.raw"
SPEND_SIGNED="$ZOO_BUILT/$NAME-spend.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$TXIN2" \
    --tx-out "${ADDR}+5000000" --tx-out "${ADDR}+5000000" \
    --change-address "$ADDR" --out-file "$SPEND_RAW" >/dev/null \
    || { zoo_record "$NAME" FAIL "" "spend-setup-build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" --tx-body-file "$SPEND_RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SPEND_SIGNED" >/dev/null
SPEND_TXID=$(zoo_submit "$SPEND_SIGNED") || { zoo_record "$NAME" FAIL "" "spend-setup-submit"; exit 1; }
zoo_wait_inclusion "$SPEND_TXID" 60 || { zoo_record "$NAME" FAIL "$SPEND_TXID" "spend-setup-not-included"; exit 1; }
SPEND_A="${SPEND_TXID}#0"
SPEND_B="${SPEND_TXID}#1"

# ── The thing under test: two txs, both referencing REF_IN, submitted
#    back-to-back without waiting between them. ─────────────────────────────
build_ref_tx() {
    local sfx="$1" spend_in="$2"
    local raw="$ZOO_BUILT/$NAME-$sfx.raw" signed="$ZOO_BUILT/$NAME-$sfx.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$spend_in" \
        --read-only-tx-in-reference "$REF_IN" \
        --tx-out "${ADDR}+2000000" --change-address "$ADDR" \
        --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$NAME.$sfx.err" || return 1
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" --tx-body-file "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" --out-file "$signed" >/dev/null || return 1
    printf '%s' "$signed"
}

SIGNED_A=$(build_ref_tx txA "$SPEND_A") || { zoo_record "$NAME" FAIL "" "build-txA"; exit 1; }
SIGNED_B=$(build_ref_tx txB "$SPEND_B") || { zoo_record "$NAME" FAIL "" "build-txB"; exit 1; }

TXID_A=$(zoo_submit "$SIGNED_A") || { zoo_record "$NAME" FAIL "" "submit-txA"; exit 1; }
TXID_B=$(zoo_submit "$SIGNED_B") || { zoo_record "$NAME" FAIL "$TXID_A" "submit-txB"; exit 1; }

INC_A=0; INC_B=0
zoo_wait_inclusion "$TXID_A" 90 && INC_A=1
zoo_wait_inclusion "$TXID_B" 30 && INC_B=1

# RED-PROOF: relax this pair of checks (e.g. accept 1-of-2) to hide a node
# that treats a shared reference input as a spend conflict and rejects the
# second tx to touch it.
if [ "$INC_A" -ne 1 ] || [ "$INC_B" -ne 1 ]; then
    zoo_record "$NAME" FAIL "$TXID_A;$TXID_B" "included A=$INC_A B=$INC_B"
    exit 1
fi

# The reference input must still exist — it was never consumed.
STILL_THERE=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r --arg t "$REF_TXID" '[keys[] | select(. == ($t + "#0"))] | length')

if [ "${STILL_THERE:-0}" -ge 1 ]; then
    zoo_record "$NAME" PASS "$TXID_A;$TXID_B" "ref=$REF_IN both-included ref-still-unspent"
else
    zoo_record "$NAME" FAIL "$TXID_A;$TXID_B" "ref-input-consumed(should-be-read-only)"
    exit 1
fi
