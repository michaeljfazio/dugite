#!/usr/bin/env bash
# wait-catchup.sh — wait for cardano-bp to come within N blocks of dugite-bp.
#
# Replaces the two hand-copied "catch-up gates" that used to live inline in the
# justfile and that, after 60s of no progress, RESTARTED cardano-bp.
#
# Those gates existed because of #980: dugite's N2N ChainSync server stopped
# feeding its downstream peer and never recovered, so cardano-bp parked at a
# stale block forever. Restarting it opened a new connection, which got a fresh
# responder task, which worked. The restart was a band-aid over a dugite bug —
# and worse than inert, because it converted a hard failure into a green run:
# every tip-sensitive suite downstream measured a devnet that had been silently
# repaired mid-round.
#
# #980 is fixed (responder re-arm + error-is-fatal, matching ouroboros-network's
# InboundGovernor and mux policy), so the restart is gone. What remains is an
# honest bounded wait: cardano-bp legitimately needs time to apply a burst of
# blocks. If it has NOT caught up by the deadline, that is a regression and this
# script exits non-zero rather than fixing the symptom.
#
# Usage: wait-catchup.sh [--max-gap N] [--timeout-seconds N] [--label TEXT]
set +e
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

MAX_GAP=5
TIMEOUT=180
LABEL="catch-up gate"
while [ $# -gt 0 ]; do
    case "$1" in
        --max-gap) MAX_GAP="$2"; shift 2 ;;
        --timeout-seconds) TIMEOUT="$2"; shift 2 ;;
        --label) LABEL="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

UID_="$(id -u)"
DBP="/tmp/ld-$UID_/dbp.sock"
CBP="/tmp/ld-$UID_/cbp.sock"

_block() {
    cardano-cli query tip --testnet-magic 42 --socket-path "$1" 2>/dev/null \
        | jq -r '.block // empty'
}

echo "=== $LABEL: waiting for cardano-bp within $MAX_GAP blocks of dugite-bp (${TIMEOUT}s) ==="
deadline=$(( $(date +%s) + TIMEOUT ))
last_c=""
stuck_since=$(date +%s)

while [ "$(date +%s)" -lt "$deadline" ]; do
    d=$(_block "$DBP"); c=$(_block "$CBP")
    if [ -z "$d" ] || [ -z "$c" ]; then
        echo "  (a socket is not answering yet: dugite-bp='${d:-?}' cardano-bp='${c:-?}')"
        sleep 2
        continue
    fi
    gap=$(( d - c )); [ "$gap" -lt 0 ] && gap=$(( -gap ))
    echo "  dugite-bp=$d cardano-bp=$c gap=$gap"
    if [ "$gap" -le "$MAX_GAP" ]; then
        echo "  caught up (gap=$gap)"
        exit 0
    fi
    # Distinguish "applying, just behind" from "parked" — the #980 fingerprint
    # is a tip that does not move at all while the producer keeps forging.
    if [ "$c" != "$last_c" ]; then
        last_c="$c"
        stuck_since=$(date +%s)
    fi
    sleep 2
done

d=$(_block "$DBP"); c=$(_block "$CBP")
gap=$(( ${d:-0} - ${c:-0} )); [ "$gap" -lt 0 ] && gap=$(( -gap ))
stuck_for=$(( $(date +%s) - stuck_since ))
echo "FAIL: cardano-bp did not catch up within ${TIMEOUT}s (dugite-bp=${d:-?} cardano-bp=${c:-?} gap=$gap)" >&2
if [ "$stuck_for" -ge 30 ]; then
    echo "  cardano-bp's tip has not moved for ${stuck_for}s while dugite-bp kept \
forging — that is the #980 stall fingerprint, which is supposed to be fixed. \
Do NOT restore the restart workaround; investigate the ChainSync responder." >&2
fi
exit 1
