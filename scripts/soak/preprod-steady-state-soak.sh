#!/usr/bin/env bash
# preprod-steady-state-soak.sh — pre-release soak on the REAL preprod network.
#
# Usage: ./scripts/soak/preprod-steady-state-soak.sh [MINUTES] [DB_PATH]
#          MINUTES  default 60
#          DB_PATH  default ./db-preprod, then ../../db-preprod
#
# WHAT THIS MEASURES, and how it differs from goal-soak.sh
#
# `goal-soak.sh` WIPES the database and re-imports from Mithril: it answers "can a
# fresh node reach tip in 30 minutes". This script answers a different and, before a
# release, more important question: **does a node already at tip keep operating
# correctly on a live network** — sync, apply, serve, pots, peers, memory — for an
# hour. It therefore REUSES an existing synced DB and never wipes.
#
# Every predicate is measured against an independent oracle (Koios preprod) or against
# the node's own logs, and every one RECORDS THE COMPARED VALUE rather than just a
# verdict: a gate that prints PASS without the number it compared cannot be audited
# later, and this repo has been bitten by exactly that (#916/#945/#953).
#
# GATED ON CATCH-UP, deliberately. Sampling tip-parity while the node is still
# converging measures reconvergence, not steady state, and fails on noise — the trap
# recorded after the v2.6.0 soak was started immediately behind a deliberate
# disruption. Nothing is sampled until the node is demonstrably at tip.
#
# SIGTERM, never SIGKILL: `kill -9` corrupts the ImmutableDB (its chunk index is
# flushed on clean shutdown).
#
# The DB is opened under dugite's exclusive advisory flock (#929), so if another
# process already holds it this exits immediately and names the holder rather than
# corrupting anything.

set -eu
# Deliberately NOT pipefail: Koios and the metrics port both hiccup transiently, and
# one failed sample must not abort an hour-long run. Individual samples that fail are
# counted and reported instead.
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

cd "$(dirname "$0")/../.."
REPO="$(pwd)"
# The main checkout, for the DB fallback below. In a worktree `--git-common-dir` is
# the main repo's `.git`; its parent is that checkout. In a normal clone this is just
# $REPO, so the branch below is a no-op there.
MAIN_CHECKOUT="$(dirname "$(git rev-parse --git-common-dir 2>/dev/null || echo "$REPO/.git")")"

MINUTES="${1:-60}"
DB_ARG="${2:-}"

MAGIC=1
PORT=3001
METRICS=12799
KOIOS="https://preprod.koios.rest/api/v1"

# ── locate a synced DB ─────────────────────────────────────────────────────
if [ -n "$DB_ARG" ]; then
    DB="$DB_ARG"
elif [ -d "$REPO/db-preprod" ]; then
    DB="$REPO/db-preprod"
elif [ -n "${MAIN_CHECKOUT:-}" ] && [ -d "$MAIN_CHECKOUT/db-preprod" ]; then
    # Worktrees do not carry the 19 GB chain DB, so fall back to the MAIN checkout's.
    #
    # Derived from `git rev-parse --git-common-dir` rather than counting `..`: a
    # worktree under `.claude/worktrees/<name>` is THREE levels down, not two, and
    # the hand-counted version silently found nothing and refused to run.
    #
    # Sharing the directory is safe because dugite holds an exclusive advisory flock
    # on it (#929): a second opener fails fast naming the holder's pid rather than
    # racing. It is not COPIED because the DB is ~19 GB and this machine has ~28 GiB
    # free — and a full disk shows up as bogus linker errors, not as a disk message.
    DB="$MAIN_CHECKOUT/db-preprod"
else
    echo "REFUSING TO RUN: no preprod database found."
    echo "  Looked in: $REPO/db-preprod and ${MAIN_CHECKOUT:-?}/db-preprod"
    echo "  Seed one with: just mithril-import preprod   (then let it reach tip)"
    exit 2
fi

BIN="$REPO/target/release/dugite-node"
[ -x "$BIN" ] || { echo "REFUSING TO RUN: $BIN not built (cargo build --release)"; exit 2; }
[ -d "$DB/immutable" ] || { echo "REFUSING TO RUN: $DB has no immutable/ — not a synced DB"; exit 2; }

OUT="$REPO/reports/soak/preprod-$(date -u +%Y%m%d-%H%M%SZ)"
mkdir -p "$OUT"
LOG="$OUT/node.log"
SAMPLES="$OUT/samples.tsv"
REPORT="$OUT/report.md"
SOCK="/tmp/dugite-preprod-soak.sock"
rm -f "$SOCK"

FAILURES=0
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }
step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }

koios_tip_slot() { curl -s --max-time 20 "$KOIOS/tip" 2>/dev/null | jq -r '.[0].abs_slot // empty' 2>/dev/null; }
koios_epoch()    { curl -s --max-time 20 "$KOIOS/tip" 2>/dev/null | jq -r '.[0].epoch_no // empty' 2>/dev/null; }
metric() { # <metric-name>
    curl -s --max-time 10 "http://127.0.0.1:$METRICS/metrics" 2>/dev/null \
        | awk -v m="$1" '$1 == m { print $2; exit }'
}
node_slot() { metric dugite_slot_number; }
# VERIFIED against crates/dugite-node/src/metrics.rs — `dugite_connected_peers`
# was a guess and does not exist; the real gauge is `dugite_peers_connected`. A
# metric name that does not exist reads as 0 forever, which would have made the
# peer predicate silently vacuous.
node_peers() { metric dugite_peers_connected; }

step "preprod steady-state soak — ${MINUTES} min"
note "db      : $DB"
note "binary  : $BIN ($(git rev-parse --short HEAD 2>/dev/null || echo unknown))"
note "evidence: $OUT"

# ── launch ─────────────────────────────────────────────────────────────────
LAUNCH="$BIN run \
    --config        $REPO/config/preprod/config.json \
    --topology      $REPO/config/preprod/topology.json \
    --database-path $DB \
    --socket-path   $SOCK \
    --host-addr     0.0.0.0 \
    --port          $PORT \
    --metrics-port  $METRICS"

if command -v caffeinate >/dev/null 2>&1; then
    # Without this macOS sleeps mid-soak and the gap reads as a sync stall.
    caffeinate -is $LAUNCH >> "$LOG" 2>&1 &
else
    $LAUNCH >> "$LOG" 2>&1 &
fi
NODE_PID=$!
echo "$NODE_PID" > "$OUT/node.pid"
note "node pid $NODE_PID"

cleanup() {
    if kill -0 "$NODE_PID" 2>/dev/null; then
        kill -TERM "$NODE_PID" 2>/dev/null || true
        for _ in $(seq 1 90); do
            kill -0 "$NODE_PID" 2>/dev/null || break
            sleep 1
        done
        # `kill` returning 0 does not prove death — poll, and say so if it survived.
        kill -0 "$NODE_PID" 2>/dev/null && \
            note "node $NODE_PID still alive 90s after SIGTERM (NOT escalating to -9: that corrupts the ImmutableDB)"
    fi
    rm -f "$SOCK"
}
trap cleanup EXIT

# A DB held by another process must be reported as such, not as a sync failure.
sleep 20
if ! kill -0 "$NODE_PID" 2>/dev/null; then
    if grep -qiE 'already (locked|held)|lock.*pid|DbDirLock' "$LOG" 2>/dev/null; then
        bad "the node exited immediately: $DB is locked by another process (see $LOG)"
    else
        bad "the node exited within 20s of launch — see $LOG"
    fi
    tail -20 "$LOG"
    exit 1
fi

# ── gate: reach tip before measuring anything ──────────────────────────────
step "catch-up gate — nothing is sampled until the node is at tip"
CATCHUP_TIMEOUT="${SOAK_CATCHUP_TIMEOUT:-3600}"
TIP_TOLERANCE_SLOTS="${SOAK_TIP_TOLERANCE:-120}"
deadline=$(( $(date +%s) + CATCHUP_TIMEOUT ))
AT_TIP=0
last_report=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    ns=$(node_slot); ks=$(koios_tip_slot)
    if [ -n "${ns:-}" ] && [ -n "${ks:-}" ]; then
        ns_i=${ns%.*}; delta=$(( ks - ns_i ))
        [ "$delta" -lt 0 ] && delta=$(( -delta ))
        now=$(date +%s)
        if [ $(( now - last_report )) -ge 60 ]; then
            note "catching up: node slot=$ns_i koios=$ks delta=${delta}"
            last_report=$now
        fi
        if [ "$delta" -le "$TIP_TOLERANCE_SLOTS" ]; then
            AT_TIP=1
            ok "at tip: node slot=$ns_i koios=$ks delta=${delta} (tolerance ${TIP_TOLERANCE_SLOTS})"
            break
        fi
    fi
    kill -0 "$NODE_PID" 2>/dev/null || { bad "node died during catch-up — see $LOG"; tail -30 "$LOG"; exit 1; }
    sleep 15
done
if [ "$AT_TIP" -ne 1 ]; then
    bad "node did not reach tip within ${CATCHUP_TIMEOUT}s — soak NOT run (measuring now would sample reconvergence, not steady state)"
    exit 1
fi

# ── soak ───────────────────────────────────────────────────────────────────
step "soak — ${MINUTES} min at tip"
printf 'ts\tnode_slot\tkoios_slot\tdelta\tpeers\trss_mb\terrors\tapply_fail\n' > "$SAMPLES"
END=$(( $(date +%s) + MINUTES * 60 ))
SAMPLE_INTERVAL="${SOAK_SAMPLE_INTERVAL:-300}"
MAX_DELTA=0
MIN_PEERS=999
SAMPLE_COUNT=0
FAILED_SAMPLES=0
RSS_FIRST=""
RSS_LAST=""

while [ "$(date +%s)" -lt "$END" ]; do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        bad "node DIED mid-soak — see $LOG"
        tail -40 "$LOG"
        break
    fi
    ns=$(node_slot); ks=$(koios_tip_slot); pe=$(node_peers)
    rss=$(ps -o rss= -p "$NODE_PID" 2>/dev/null | tr -d ' ')
    rss_mb=$(( ${rss:-0} / 1024 ))
    errs=$(grep -cE ' ERROR |panicked' "$LOG" 2>/dev/null || echo 0)
    afail=$(grep -cE 'Failed to apply block|apply failed|block application failed' "$LOG" 2>/dev/null || echo 0)

    if [ -n "${ns:-}" ] && [ -n "${ks:-}" ]; then
        ns_i=${ns%.*}; d=$(( ks - ns_i )); [ "$d" -lt 0 ] && d=$(( -d ))
        [ "$d" -gt "$MAX_DELTA" ] && MAX_DELTA=$d
        pe_i=${pe%.*}; pe_i=${pe_i:-0}
        [ "$pe_i" -lt "$MIN_PEERS" ] && MIN_PEERS=$pe_i
        [ -z "$RSS_FIRST" ] && RSS_FIRST=$rss_mb
        RSS_LAST=$rss_mb
        SAMPLE_COUNT=$(( SAMPLE_COUNT + 1 ))
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$(date -u +%H:%M:%SZ)" "$ns_i" "$ks" "$d" "$pe_i" "$rss_mb" "$errs" "$afail" >> "$SAMPLES"
        note "sample $SAMPLE_COUNT: slot=$ns_i delta=$d peers=$pe_i rss=${rss_mb}MB errors=$errs"
    else
        FAILED_SAMPLES=$(( FAILED_SAMPLES + 1 ))
        note "sample skipped (node_slot='${ns:-}' koios='${ks:-}') — counted, not silently dropped"
    fi
    sleep "$SAMPLE_INTERVAL"
done

# ── verdict ────────────────────────────────────────────────────────────────
step "verdict"

# A soak that took no samples proves nothing; say so rather than passing vacuously.
if [ "$SAMPLE_COUNT" -lt 3 ]; then
    bad "only $SAMPLE_COUNT usable sample(s) (+$FAILED_SAMPLES failed) — too few to conclude anything"
else
    ok "$SAMPLE_COUNT usable samples (+$FAILED_SAMPLES failed reads)"
fi

if [ "$MAX_DELTA" -le "$TIP_TOLERANCE_SLOTS" ]; then
    ok "tip parity held: worst delta vs Koios = ${MAX_DELTA} slots (tolerance ${TIP_TOLERANCE_SLOTS})"
else
    bad "tip drifted: worst delta vs Koios = ${MAX_DELTA} slots (tolerance ${TIP_TOLERANCE_SLOTS})"
fi

if [ "$MIN_PEERS" -ge 1 ] && [ "$MIN_PEERS" -ne 999 ]; then
    ok "peers held: minimum observed = ${MIN_PEERS}"
else
    bad "peer count fell to ${MIN_PEERS} — a node with no peers is not serving or following anything"
fi

ERRS=$(grep -cE ' ERROR |panicked' "$LOG" 2>/dev/null || echo 0)
if [ "${ERRS:-0}" -eq 0 ]; then
    ok "0 ERROR/panic lines in the node log"
else
    bad "${ERRS} ERROR/panic line(s) in the node log"
    grep -E ' ERROR |panicked' "$LOG" | head -10 | cut -c1-200
fi

# #985: a node applying blocks WITHOUT this line is positive evidence the startup
# LedgerSeq re-anchor fired. Its presence means a chimera was reconstructed.
INCOH=$(grep -c 'LedgerSeq was incoherent' "$LOG" 2>/dev/null || echo 0)
if [ "${INCOH:-0}" -eq 0 ]; then
    ok "0 'LedgerSeq was incoherent' — positive evidence the startup re-anchor fired (#985)"
else
    bad "${INCOH} 'LedgerSeq was incoherent' line(s) — the #985 chimera shape"
fi

# #1057's own signatures must not appear on a healthy synced node.
WEDGE=$(grep -c 'declining a range rooted at GENESIS' "$LOG" 2>/dev/null || echo 0)
if [ "${WEDGE:-0}" -eq 0 ]; then
    ok "0 genesis-range declines (#1057 wedge absent, as expected on a synced node)"
else
    bad "${WEDGE} genesis-range decline(s) — #1057 should be unreachable here"
fi

RSS_NOTE="first=${RSS_FIRST:-?}MB last=${RSS_LAST:-?}MB"
if [ -n "${RSS_FIRST:-}" ] && [ -n "${RSS_LAST:-}" ] && [ "$RSS_FIRST" -gt 0 ]; then
    GROWTH=$(( (RSS_LAST - RSS_FIRST) * 100 / RSS_FIRST ))
    if [ "$GROWTH" -lt 50 ]; then
        ok "RSS stable: $RSS_NOTE (${GROWTH}% change)"
    else
        bad "RSS grew ${GROWTH}%: $RSS_NOTE — possible leak over ${MINUTES} min"
    fi
else
    note "RSS not measured"
fi

KEP=$(koios_epoch)
note "Koios preprod epoch at end: ${KEP:-unknown}"

{
    echo "# preprod steady-state soak"
    echo
    echo "- commit: \`$(git rev-parse --short HEAD 2>/dev/null || echo unknown)\`"
    echo "- duration: ${MINUTES} min at tip (after a gated catch-up)"
    echo "- database: \`$DB\` (reused, never wiped)"
    echo "- oracle: Koios preprod \`$KOIOS\`"
    echo
    echo "| metric | value |"
    echo "|---|---|"
    echo "| usable samples | $SAMPLE_COUNT (+$FAILED_SAMPLES failed reads) |"
    echo "| worst tip delta vs Koios | ${MAX_DELTA} slots |"
    echo "| minimum peers | ${MIN_PEERS} |"
    echo "| ERROR/panic lines | ${ERRS} |"
    echo "| LedgerSeq incoherent | ${INCOH} |"
    echo "| genesis-range declines | ${WEDGE} |"
    echo "| RSS | ${RSS_NOTE} |"
    echo "| Koios epoch at end | ${KEP:-unknown} |"
    echo
    echo "Verdict: $([ "$FAILURES" -eq 0 ] && echo PASS || echo "FAIL ($FAILURES)")"
    echo
    echo "Raw samples: \`samples.tsv\`. Node log: \`node.log\`."
} > "$REPORT"

step "SUMMARY"
cat "$REPORT"
if [ "$FAILURES" -eq 0 ]; then
    echo "PREPROD SOAK: PASS"
    exit 0
fi
echo "PREPROD SOAK: FAIL ($FAILURES predicate(s))"
exit 1
