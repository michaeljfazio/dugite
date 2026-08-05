#!/usr/bin/env bash
# 11e — 100-tx dependent chain (mempool dependency tracking at depth,
# extending 01h's 3-tx chain and 11a-c's mempool coverage). Upstream
# precedent: cardano-node-tests chained-tx coverage (#1032, cardano-node-tests
# adoption P0.1).
#
# Each tx spends output #0 of the previous one. Every tx is built, signed,
# and its txid computed BEFORE any submission happens (a signed tx's id is
# deterministic — `transaction txid` needs no network).
#
# NOT fired at the mempool 100-deep with zero waiting, though — dugite-mempool
# hard-caps how many UNCONFIRMED dependent txs it will chain:
# `VIRTUAL_CHAIN_MAX_DEPTH = 5` in crates/dugite-mempool/src/lib.rs, a
# deliberate, documented bound on worst-case cascade-eviction cost (its own
# comment: "Haskell cardano-node does not admit chained (dependent) txs at
# all; we allow a small fixed depth to support wallet workflows"). Confirmed
# live: the first 5 back-to-back submits land in the mempool, the 6th is
# rejected at admission (dugite-relay.log:
# "Mempool: rejecting tx — virtual chain depth limit exceeded ... depth=5
# max=5"), matching `test_virtual_chain_depth_cap` in that same file exactly
# (a root tx + 4 virtual children = depth 5 succeeds, the 5th child fails).
# This is not a bug to route around by construction — no tx shape changes
# what admission counts. Once a chain's root is confirmed on-chain the
# virtual-chain counter for its descendants resets, so this script submits in
# BATCH-sized (5) back-to-back bursts — still zero-wait AT the real boundary —
# and only waits for on-chain inclusion BETWEEN bursts, reaching a genuine
# depth-100 dependent chain across 20 confirmed bursts.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

# Earlier scripts may still have transactions in flight; building on a UTxO
# the ledger view reports but that a pending tx has already claimed is an
# unavoidable input-conflict at submit time (the 11c lesson, #918).
zoo_wait_mempool_quiet 90 || true

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# zoo_largest_utxo ranks by lovelace only. The shared genesis funder address
# accumulates leftover native-asset UTxOs from earlier tx-zoo runs (15-series
# asset-lattice scripts mint/burn against this same key), and one of those can
# out-rank every ada-only UTxO by lovelace alone. `transaction build-raw`
# does NOT auto-balance value the way `transaction build` does — a single
# ada-only --tx-out against a multi-asset input silently drops the asset and
# the node correctly rejects with ValueNotConservedUTxO (confirmed against
# cardano-node directly: supplied MultiAsset non-empty, expected empty). Every
# hop in this chain is ada-only build-raw, so the STARTING utxo must be
# ada-only too, or the very first hop fails this way.
UTXO=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r '[to_entries[] | select((.value.value | keys) == ["lovelace"])]
             | sort_by(-.value.value.lovelace) | .[0]
             | if . == null then empty else "\(.key) \(.value.value.lovelace)" end')
[ -n "$UTXO" ] || { zoo_record "$NAME" FAIL "" "no-ada-only-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
FEE=200000
DEPTH=100
# 100 hops x 200000 fee = 20,000,000 lovelace total burned to fees. The
# genesis funder's ada-only UTxOs are minted at 50,000,000 lovelace each (see
# setup.sh), so the chain's final output lands at 30,000,000 — comfortably
# above minUTxO (~1,000,000-2,000,000 depending on era) at every one of the
# 100 intermediate hops, not just the last. If AMT is ever smaller than
# DEPTH*FEE + 2000000 here, that is a setup-side change, not this script.
MIN_REQUIRED=$((DEPTH * FEE + 2000000))
if [ "$AMT" -lt "$MIN_REQUIRED" ]; then
    zoo_record "$NAME" FAIL "" "funding-utxo-too-small amt=$AMT need=$MIN_REQUIRED"
    exit 1
fi

cur_in="$TXIN"
cur_amt="$AMT"
TXIDS=()
FILES=()
for n in $(seq 1 "$DEPTH"); do
    out_amt=$((cur_amt - FEE))
    if [ "$out_amt" -lt 2000000 ]; then
        zoo_info "insufficient funds after $((n - 1)) chain steps, stopping there"
        break
    fi
    RAW="$ZOO_BUILT/$NAME-$n.raw"
    SIGNED="$ZOO_BUILT/$NAME-$n.signed"
    # Fresh (truncated, not appended) per-attempt error file, and print the
    # first non-blank line rather than the last: cardano-cli's stderr is
    # "Command failed: ..." then the actual "Error: ..." line, then a
    # multi-line Haskell CallStack whose last line is a blank — `tail -1`
    # against that always printed an empty reason.
    cardano-cli conway transaction build-raw \
        --tx-in     "$cur_in" \
        --tx-out    "${ADDR}+${out_amt}" \
        --fee       "$FEE" \
        --out-file  "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.build.err" || {
        zoo_info "build failed at step $n: $(grep -m2 -v '^[[:space:]]*$' "$ZOO_LOGS/$NAME.build.err" | tr '\n' ' ')"
        break
    }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$RAW" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$SIGNED" >/dev/null
    txid=$(cardano-cli conway transaction txid --tx-file "$SIGNED" --output-text 2>/dev/null)
    TXIDS+=("$txid")
    FILES+=("$SIGNED")
    cur_in="${txid}#0"
    cur_amt="$out_amt"
done

TOTAL=${#FILES[@]}
if [ "$TOTAL" -eq 0 ]; then
    zoo_record_env_skip "$NAME" "no-txs-built"
    exit 0
fi

# ── Fire the chain at the mempool in BATCH-sized (5) back-to-back bursts ───
# See header: VIRTUAL_CHAIN_MAX_DEPTH=5 caps unconfirmed dependent depth.
BATCH=5
SUBMITTED=0
i=0
while [ "$i" -lt "$TOTAL" ]; do
    batch_end=$((i + BATCH < TOTAL ? i + BATCH : TOTAL))
    batch_ok=1
    for ((j = i; j < batch_end; j++)); do
        f="${FILES[$j]}"
        # Fresh (truncated, not appended) per-attempt error file, and print
        # the first non-blank line rather than the last: cardano-cli's
        # stderr is "Command failed: ..." then the actual "Error: ..."
        # line, then a multi-line Haskell CallStack whose last line is
        # blank — `tail -1` against that always printed an empty reason.
        if cardano-cli conway transaction submit \
                --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
                --tx-file "$f" >/dev/null 2> "$ZOO_LOGS/$NAME.submit.err"; then
            SUBMITTED=$((SUBMITTED + 1))
        else
            zoo_info "submit failed at chain position $SUBMITTED: $(grep -m2 -v '^[[:space:]]*$' "$ZOO_LOGS/$NAME.submit.err" | tr '\n' ' ')"
            batch_ok=0
            break
        fi
    done
    [ "$batch_ok" -eq 1 ] || break

    # Wait for the LAST tx of this burst to land on-chain before the next
    # burst — that confirms the whole linear chain up to here (a single
    # block can and does include all 5 at once) and resets the virtual-chain
    # depth counter for the next burst's root.
    batch_last_txid="${TXIDS[$((batch_end - 1))]}"
    if ! zoo_wait_inclusion "$batch_last_txid" 60; then
        zoo_info "burst ending at chain position $batch_end not included within 60s"
        break
    fi
    i=$batch_end
done

# RED-PROOF: change `-ne "$TOTAL"` to a looser threshold to hide the mempool
# dropping a middle transaction while still accepting a later dependent one
# (which would itself indicate a correctness bug, not just a coverage gap).
if [ "$SUBMITTED" -ne "$TOTAL" ]; then
    zoo_record "$NAME" FAIL "" "only-$SUBMITTED-of-$TOTAL-submitted"
    exit 1
fi

LAST_TXID="${TXIDS[-1]}"
if ! zoo_wait_inclusion "$LAST_TXID" 90; then
    zoo_record "$NAME" FAIL "$LAST_TXID" "chain-not-included depth=$TOTAL"
    exit 1
fi

# ── Exact final value: initial amount minus (depth x fee) ──────────────────
ACTUAL=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r --arg t "$LAST_TXID" '[to_entries[] | select(.key | startswith($t))][0].value.value.lovelace // empty')

# RED-PROOF: drop this equality (accepting any positive balance) to hide a
# fee miscounted somewhere along a 100-tx chain.
if [ "${ACTUAL:-}" = "$cur_amt" ]; then
    zoo_record "$NAME" PASS "$LAST_TXID" "chain=$TOTAL value=$ACTUAL"
else
    zoo_record "$NAME" FAIL "$LAST_TXID" "value-mismatch expected=$cur_amt actual=${ACTUAL:-none}"
    exit 1
fi
