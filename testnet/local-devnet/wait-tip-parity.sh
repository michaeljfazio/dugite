#!/usr/bin/env bash
# wait-tip-parity.sh — wait until ALL THREE observers report the SAME tip block.
#
# WHY THIS EXISTS, AND WHY wait-catchup.sh IS NOT ENOUGH
#
# `soak.sh`'s p4 predicate requires EXACT tip parity across relay, dugite-bp and
# cardano-bp on >=95% of its ticks. Starting a soak straight after a deliberate
# disruption — the chaos suite's SIGKILL, or Round 3's 90s outage — samples
# RECONVERGENCE instead of steady state, and the early ticks fail the predicate on
# noise rather than on any node defect.
#
# `wait-catchup.sh` looks like the remedy and is not, for two reasons measured on
# consecutive gate runs:
#
#   * its default `--max-gap 5` returns while the nodes are still up to five blocks
#     apart, and p4 wants them EQUAL. Measured: it returned with dugite-bp at 666 and
#     cardano-bp at 662, and the next four soak ticks were scored out-of-parity;
#   * it compares cardano-bp against dugite-bp ONLY. The relay is a third observer that
#     p4 also scores, and it was the lagging one in the run before that (relay pinned at
#     636 while both producers ran ahead).
#
# So this waits on exactly the condition p4 measures, over exactly the nodes p4 scores.
#
# This is an ADDED ASSERTION, NOT A RELAXED PREDICATE. The 95% floor is untouched. A
# devnet that cannot reach parity fails HERE, loudly, with the values it last saw —
# rather than yielding a soak whose result says nothing about steady state. An
# unreadable tip is reported as unreadable, never scored as agreement: three nodes that
# all answer "" are not in parity, they are unmeasured.
#
# Usage: wait-tip-parity.sh [--timeout-seconds N] [--stable-samples N] [--label TEXT]
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2
. ./lib/common.sh
set +e
set +u
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

TIMEOUT=180
STABLE=2          # consecutive agreeing samples, so a momentary crossing does not count
LABEL="tip-parity"
while [ $# -gt 0 ]; do
    case "$1" in
        --timeout-seconds) TIMEOUT="$2"; shift 2 ;;
        --stable-samples)  STABLE="$2";  shift 2 ;;
        --label)           LABEL="$2";   shift 2 ;;
        *) echo "unknown arg: $1"; exit 2 ;;
    esac
done

# BOUNDED, and this is the second time the same trap has been paid for.
#
# A SIGSTOPped node keeps a LISTENING socket — the kernel completes the connect from its
# backlog — so `cardano-cli query tip` against it neither fails nor returns. Unbounded,
# this gate HANGS on exactly the condition it exists to detect, which is strictly worse
# than the failure it was meant to report: the caller cannot tell a stalled devnet from a
# slow one. (genesis-fork-round's `tip_field` learned this by silently eating 29 minutes.)
#
# macOS ships no `timeout(1)`, and the obvious `cmd & (sleep N; kill $!) &` shape is wrong
# here too: the watchdog subshell inherits the command-substitution pipe and its orphaned
# `sleep` holds fd 1 open, so EVERY call costs the full timeout, successes included. Run
# the query to a temp file and poll for its exit instead.
QUERY_TIMEOUT="${QUERY_TIMEOUT:-15}"
block_of() {
    local sock="$1" tmp q i limit
    tmp="$(mktemp "${TMPDIR:-/tmp}/wtp.XXXXXX")" || return 0
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        >"$tmp" 2>/dev/null &
    q=$!
    i=0; limit=$(( QUERY_TIMEOUT * 5 ))
    while [ "$i" -lt "$limit" ]; do
        kill -0 "$q" 2>/dev/null || break
        sleep 0.2
        i=$(( i + 1 ))
    done
    if kill -0 "$q" 2>/dev/null; then
        kill -9 "$q" 2>/dev/null
        wait "$q" 2>/dev/null
        rm -f "$tmp"
        return 0                      # unreadable -> empty -> NOT counted as parity
    fi
    wait "$q" 2>/dev/null
    jq -r '.block // empty' <"$tmp" 2>/dev/null
    rm -f "$tmp"
}

echo "=== $LABEL: waiting for relay/dugite-bp/cardano-bp to agree on a tip block (${TIMEOUT}s, ${STABLE} stable samples) ==="

deadline=$(( $(date +%s) + TIMEOUT ))
agree=0
last=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    r=$(block_of "$LD_RELAY_SOCK")
    d=$(block_of "$LD_DUGITE_BP_SOCK")
    c=$(block_of "$LD_CARDANO_BP_SOCK")
    last="relay=${r:-unreadable} dugite-bp=${d:-unreadable} cardano-bp=${c:-unreadable}"
    if [ -n "$r" ] && [ -n "$d" ] && [ -n "$c" ] && [ "$r" = "$d" ] && [ "$d" = "$c" ]; then
        agree=$(( agree + 1 ))
        if [ "$agree" -ge "$STABLE" ]; then
            echo "$LABEL: in parity at block $r ($agree consecutive samples)"
            exit 0
        fi
    else
        agree=0
    fi
    sleep 3
done

echo "$LABEL: NOT in parity within ${TIMEOUT}s — last reading: $last"
echo "  The soak that follows would score reconvergence rather than steady state, so"
echo "  this fails here instead. Investigate why the observers disagree before"
echo "  interpreting any soak result."
exit 1
