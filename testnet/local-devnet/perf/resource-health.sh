#!/usr/bin/env bash
# perf/resource-health.sh — D8: sample CPU%, RSS, FD count, thread count every
# 5s over SAMPLE_DURATION_SEC, assert thresholds, and emit resource-samples.csv.
#
# Thresholds (configurable via env):
#   CPU_THRESHOLD_PCT=80      — average CPU must be below this
#   RSS_THRESHOLD_MB=2048     — peak RSS must be below this
#   FD_THRESHOLD=4096         — peak FD count must be below this
#   THREAD_THRESHOLD=512      — peak thread count must be below this
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
RESOURCE_CSV="$EVIDENCE_DIR/resource-samples.csv"
[ -f "$RESOURCE_CSV" ] || echo "ts,pid,node,cpu_pct,rss_kb,fds,threads" > "$RESOURCE_CSV"

SAMPLE_DURATION="${SAMPLE_DURATION_SEC:-30}"
SAMPLE_INTERVAL=5
CPU_THRESHOLD="${CPU_THRESHOLD_PCT:-80}"
RSS_THRESHOLD_KB=$(( ${RSS_THRESHOLD_MB:-2048} * 1024 ))
FD_THRESHOLD="${FD_THRESHOLD:-4096}"
THREAD_THRESHOLD="${THREAD_THRESHOLD:-512}"

[ -S "$LD_DUGITE_BP_SOCK" ] || die "dugite-bp socket not present — run ./run.sh first"

BP_PID=$(cat "$LD_STATE/dugite-bp.pid" 2>/dev/null || echo "")
RELAY_PID=$(cat "$LD_STATE/dugite-relay.pid" 2>/dev/null || echo "")

if [ -z "$BP_PID" ] || ! kill -0 "$BP_PID" 2>/dev/null; then
    log_warn "resource-health: dugite-bp PID not found or not running — skipping"
    exit 0
fi

sample_pid() {
    local pid="$1" node="$2"
    # CPU and RSS via ps
    local cpu rss
    case "$(uname -s)" in
        Darwin)
            read -r cpu rss <<< "$(ps -p "$pid" -o %cpu=,rss= 2>/dev/null | awk '{print $1, $2}' || echo "0 0")"
            ;;
        Linux)
            read -r cpu rss <<< "$(ps -p "$pid" -o %cpu=,rss= 2>/dev/null | awk '{print $1, $2}' || echo "0 0")"
            ;;
        *)
            cpu="0"; rss="0"
            ;;
    esac

    # FD count
    local fds=0
    case "$(uname -s)" in
        Darwin) fds=$(lsof -p "$pid" 2>/dev/null | wc -l | tr -d ' ' || echo 0) ;;
        Linux)  fds=$(ls /proc/"$pid"/fd 2>/dev/null | wc -l | tr -d ' ' || echo 0) ;;
    esac

    # Thread count
    local threads=0
    case "$(uname -s)" in
        Darwin) threads=$(ps -p "$pid" -M 2>/dev/null | tail -n +2 | wc -l | tr -d ' ' || echo 0) ;;
        Linux)  threads=$(cat /proc/"$pid"/status 2>/dev/null | awk '/^Threads:/{print $2}' || echo 0) ;;
    esac

    printf '%s,%s,%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$pid" "$node" "$cpu" "$rss" "$fds" "$threads"
}

log_info "Sampling resource health for ${SAMPLE_DURATION}s (pid=$BP_PID)..."

ELAPSED=0
CPU_SUM="0"; CPU_COUNT=0
PEAK_RSS=0; PEAK_FDS=0; PEAK_THREADS=0

while [ "$ELAPSED" -lt "$SAMPLE_DURATION" ]; do
    # Sample dugite-bp
    ROW=$(sample_pid "$BP_PID" "dugite-bp")
    echo "$ROW" >> "$RESOURCE_CSV"

    CPU=$(echo "$ROW" | cut -d, -f4)
    RSS=$(echo "$ROW" | cut -d, -f5)
    FDS=$(echo "$ROW" | cut -d, -f6)
    THREADS=$(echo "$ROW" | cut -d, -f7)

    CPU_SUM=$(echo "scale=2; $CPU_SUM + $CPU" | bc 2>/dev/null || echo "$CPU_SUM")
    CPU_COUNT=$(( CPU_COUNT + 1 ))
    [ "$RSS" -gt "$PEAK_RSS" ] 2>/dev/null && PEAK_RSS="$RSS"
    [ "$FDS" -gt "$PEAK_FDS" ] 2>/dev/null && PEAK_FDS="$FDS"
    [ "$THREADS" -gt "$PEAK_THREADS" ] 2>/dev/null && PEAK_THREADS="$THREADS"

    # Also sample relay if running
    if [ -n "$RELAY_PID" ] && kill -0 "$RELAY_PID" 2>/dev/null; then
        sample_pid "$RELAY_PID" "dugite-relay" >> "$RESOURCE_CSV"
    fi

    log_info "  cpu=${CPU}% rss=${RSS}KB fds=${FDS} threads=${THREADS}"
    sleep "$SAMPLE_INTERVAL"
    ELAPSED=$(( ELAPSED + SAMPLE_INTERVAL ))
done

# Compute averages and assert thresholds
AVG_CPU=0
if [ "$CPU_COUNT" -gt 0 ]; then
    AVG_CPU=$(echo "scale=1; $CPU_SUM / $CPU_COUNT" | bc 2>/dev/null || echo "0")
fi

FAILURES=()
cpu_int="${AVG_CPU%.*}"
cpu_thresh_int="${CPU_THRESHOLD%.*}"
[ "${cpu_int:-0}" -gt "${cpu_thresh_int:-80}" ] && FAILURES+=("cpu=${AVG_CPU}%>threshold=${CPU_THRESHOLD}%")
[ "$PEAK_RSS" -gt "$RSS_THRESHOLD_KB" ] && FAILURES+=("peak_rss=${PEAK_RSS}KB>threshold=${RSS_THRESHOLD_KB}KB")
[ "$PEAK_FDS" -gt "$FD_THRESHOLD" ] && FAILURES+=("peak_fds=${PEAK_FDS}>threshold=${FD_THRESHOLD}")
[ "$PEAK_THREADS" -gt "$THREAD_THRESHOLD" ] && FAILURES+=("peak_threads=${PEAK_THREADS}>threshold=${THREAD_THRESHOLD}")

if [ "${#FAILURES[@]}" -gt 0 ]; then
    log_error "resource-health FAIL: ${FAILURES[*]}"
    exit 1
fi

log_info "resource-health PASS: avg_cpu=${AVG_CPU}% peak_rss=${PEAK_RSS}KB peak_fds=${PEAK_FDS} peak_threads=${PEAK_THREADS}"
