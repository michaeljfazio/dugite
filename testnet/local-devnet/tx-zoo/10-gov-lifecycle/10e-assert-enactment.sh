#!/usr/bin/env bash
# 10e — Gov lifecycle: wait for ratification + enactment, then verify the
# parameter change took effect via query protocol-parameters.
#
# Ratification in Conway happens at the epoch boundary after all required
# votes are cast.  With our devnet (epochLength=400 slots, slotLength=1s),
# we wait up to 2 epoch lengths (~800s) for enactment.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

GOV_STATE="$ZOO_STATE/gov-lifecycle"
EXPECTED_FILE="$GOV_STATE/expected-min-fee-a"
if [ ! -f "$EXPECTED_FILE" ]; then
    zoo_record "$NAME" SKIP "" "no-expected-min-fee-a (run 10a first)"
    exit 0
fi

EXPECTED_MIN_FEE_A=$(cat "$EXPECTED_FILE")

# Wait up to MAX_WAIT_SEC for the parameter change to appear
MAX_WAIT_SEC="${GOV_ENACT_TIMEOUT:-900}"
POLL_INTERVAL=15
ELAPSED=0

log_info "Waiting up to ${MAX_WAIT_SEC}s for govAction enactment (minFeeA=${EXPECTED_MIN_FEE_A})..."

while [ "$ELAPSED" -lt "$MAX_WAIT_SEC" ]; do
    CURRENT_MIN_FEE_A=$(cardano-cli conway query protocol-parameters \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --output-json 2>/dev/null | jq -r '.txFeePerByte // 0')

    if [ "$CURRENT_MIN_FEE_A" = "$EXPECTED_MIN_FEE_A" ]; then
        zoo_record "$NAME" PASS "" \
            "enacted: minFeeA=${CURRENT_MIN_FEE_A} after ${ELAPSED}s"
        # Write marker for downstream assertion
        echo "enacted" > "$GOV_STATE/enacted"
        # Record the enacted action ID so 10a can chain from it on re-run.
        # Per Conway/CIP-1694 the lineal chain invariant requires subsequent
        # ParameterChange proposals to reference the last enacted one.
        if [ -f "$GOV_STATE/proposal.actionid" ]; then
            cp "$GOV_STATE/proposal.actionid" "$GOV_STATE/enacted.actionid"
        fi
        exit 0
    fi

    # Also check via gov-state query to see if proposal was ratified
    PROPOSALS=$(cardano-cli conway query proposals \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --output-json 2>/dev/null | jq -r '. | length' 2>/dev/null || echo "?")
    log_info "  elapsed=${ELAPSED}s current_minFeeA=${CURRENT_MIN_FEE_A} pending_proposals=${PROPOSALS}"

    sleep "$POLL_INTERVAL"
    ELAPSED=$(( ELAPSED + POLL_INTERVAL ))
done

# Check if proposals list is empty (means it was enacted / expired)
PROPOSALS=$(cardano-cli conway query proposals \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '. | length' 2>/dev/null || echo "?")

CURRENT_MIN_FEE_A=$(cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '.txFeePerByte // 0')

if [ "$CURRENT_MIN_FEE_A" = "$EXPECTED_MIN_FEE_A" ]; then
    zoo_record "$NAME" PASS "" "enacted (found at final check)"
    echo "enacted" > "$GOV_STATE/enacted"
    if [ -f "$GOV_STATE/proposal.actionid" ]; then
        cp "$GOV_STATE/proposal.actionid" "$GOV_STATE/enacted.actionid"
    fi
else
    zoo_record "$NAME" FAIL "" \
        "NOT enacted after ${MAX_WAIT_SEC}s: current_minFeeA=${CURRENT_MIN_FEE_A} expected=${EXPECTED_MIN_FEE_A} pending=${PROPOSALS}"
fi
