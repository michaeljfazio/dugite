#!/usr/bin/env bash
# 10e — Gov lifecycle: wait for ratification + enactment, then verify the
# parameter change took effect via query protocol-parameters.
#
# Ratification in Conway happens at the epoch boundary after all required
# votes are cast.  A proposal submitted mid-epoch E cannot enact before the
# E+2 boundary (ratify+enact+commit are one step), so the wait has to span up
# to THREE epochs, and an epoch is `epochLength * slotLength` seconds.
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

# Wait up to MAX_WAIT_SEC for the parameter change to appear.
#
# DERIVE THIS FROM THE EPOCH LENGTH — a bare 900 silently assumed the default
# epochLength=400 (3 epochs = 1200s, and the proposal usually lands early enough that
# 900 was sufficient). Round 2 of the release gate runs an epochLength=600 overlay so
# the futurePParams sampler can reach its non-vacuous window, and at 600 the E+2
# boundary falls at ~1200s while this script gave up at 900 — reporting
# "NOT enacted after 900s: current_minFeeA=44 expected=45" roughly three minutes
# before the enactment it was waiting for. A timeout that encodes one genesis
# parameter as a constant fails as soon as another round legitimately changes it.
#
# The timeout only bounds FAILURE: a successful enactment exits the loop early, so
# scaling it up costs nothing on a passing run.
# Read the values from the GENESIS THE NODES ACTUALLY LOADED, not from an env var a
# caller has to remember to set in step with the overlay. `LD_EPOCH_LENGTH` is a
# convention of the gate driver; `setup.sh` never reads it, so the two can disagree
# and the disagreement is invisible. The generated genesis cannot drift from the
# running chain.
GENESIS="${LD_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}/genesis/shelley-genesis.json"
EPOCH_LEN=$(jq -r '.epochLength // 400' "$GENESIS" 2>/dev/null || echo 400)
SLOT_LEN=$(jq -r '(.slotLength // 1) | floor | if . < 1 then 1 else . end' "$GENESIS" 2>/dev/null || echo 1)
case "$EPOCH_LEN" in ''|*[!0-9]*) EPOCH_LEN=400 ;; esac
case "$SLOT_LEN"  in ''|*[!0-9]*) SLOT_LEN=1   ;; esac
MAX_WAIT_SEC="${GOV_ENACT_TIMEOUT:-$(( (EPOCH_LEN * SLOT_LEN * 32) / 10 ))}"
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
