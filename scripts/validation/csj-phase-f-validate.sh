#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# csj-phase-f-validate.sh — CSJ Phase F cross-validation harness (dugite side)
# ─────────────────────────────────────────────────────────────────────────────
#
# Brings up dugite-node in Genesis-mode relay configuration, tails its log
# for CSJ trace events, captures them with timestamps, detects LoE-window
# violations, and on termination writes a structured summary.
#
# Companion script for the Haskell side: csj-phase-f-haskell.sh
#
# PREREQUISITES
#   - dugite-node binary at DUGITE_BIN (default: ./target/release/dugite-node)
#   - Mithril-imported database at DATABASE_PATH (default: ./db-mainnet)
#   - jq on PATH (for Prometheus metrics queries)
#   - A reachable mainnet peer (topology file must list at least one relay)
#
# USAGE
#   ./scripts/validation/csj-phase-f-validate.sh [OPTIONS]
#
#   Options:
#     --network-magic N    Network magic (default: 764824073 = mainnet)
#     --config FILE        Node config JSON (default: config/mainnet/config.json)
#     --topology FILE      Topology JSON   (default: config/mainnet/topology.json)
#     --database-path DIR  ChainDB directory (default: ./db-mainnet)
#     --socket-path PATH   N2C socket path   (default: ./node-csj-validate.sock)
#     --port N             P2P listen port   (default: 3099)
#     --metrics-port N     Prometheus port   (default: 12799)
#     --duration N         Run duration in seconds (default: 86400 = 24h)
#     --out-dir DIR        Output directory  (default: validation/<timestamp>)
#     --dugite-bin PATH    Path to dugite-node binary
#     --log-level LEVEL    RUST_LOG level string (default: info)
#     --help
#
# OUTPUT LAYOUT
#   <OUT_DIR>/
#     dugite.log           Raw node log (stderr of dugite-node)
#     csj_events.jsonl     CSJ trace events (one JSON object per line)
#     loe_samples.jsonl    LoE slot sampled from Prometheus every 30s
#     summary.txt          Human-readable summary (sync time, event counts, etc.)
#     violations.txt       LoE-window violations (empty = pass)
#
# EXIT CODES
#   0 — no LoE violations detected, at least 1 Dynamo election observed
#   1 — LoE violation detected or no Dynamo election (see summary.txt)
#   2 — configuration/prerequisite error
#
# TRACE EVENTS CAPTURED
#   All lines matching any of these patterns are extracted and tagged:
#     "CSJ: elected new dynamo"         → DynamoElected
#     "CSJ: dynamo stalled; demoting"   → DynamoStallDemotion
#     "CSJ: jump issued"                → JumpIssued
#     "CSJ: intersect found"            → IntersectFound
#     "CSJ: intersect not found"        → ObjectionRaised
#     "CSJ/GDD: dynamo wins"            → ObjectionResolved(DynamoWins)
#     "CSJ/GDD: objector wins"          → ObjectionResolved(ObjectorWins)
#     "CSJ invariant violation"         → InvariantViolation
#     "LoE.*violation" (any case)       → LoEViolation
#
# LOE-WINDOW VIOLATION DEFINITION
#   A violation is logged when the Prometheus metric `slot_number` exceeds
#   the `loe_slot` value returned by the GSM snapshot endpoint, as reported
#   by periodic Prometheus scrapes. In practice this requires the node to
#   emit an explicit LoE-violation log line (pattern: "LoE.*violation") OR
#   for the sampled slot to exceed the sampled LoE slot.
#
# PASS CRITERIA (automated)
#   1. At least 1 DynamoElected event fires within the run duration.
#   2. No InvariantViolation events appear.
#   3. No LoEViolation events appear.
#   4. violations.txt is empty.
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
NETWORK_MAGIC=764824073
CONFIG="config/mainnet/config.json"
TOPOLOGY="config/mainnet/topology.json"
DATABASE_PATH="./db-mainnet"
SOCKET_PATH="./node-csj-validate.sock"
PORT=3099
METRICS_PORT=12799
DURATION=86400          # 24 hours
OUT_DIR=""
DUGITE_BIN="./target/release/dugite-node"
LOG_LEVEL="info"

# ── Argument parsing ──────────────────────────────────────────────────────────
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --network-magic)  NETWORK_MAGIC="$2";  shift 2 ;;
        --config)         CONFIG="$2";         shift 2 ;;
        --topology)       TOPOLOGY="$2";       shift 2 ;;
        --database-path)  DATABASE_PATH="$2";  shift 2 ;;
        --socket-path)    SOCKET_PATH="$2";    shift 2 ;;
        --port)           PORT="$2";           shift 2 ;;
        --metrics-port)   METRICS_PORT="$2";   shift 2 ;;
        --duration)       DURATION="$2";       shift 2 ;;
        --out-dir)        OUT_DIR="$2";        shift 2 ;;
        --dugite-bin)     DUGITE_BIN="$2";     shift 2 ;;
        --log-level)      LOG_LEVEL="$2";      shift 2 ;;
        --help|-h)        usage ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

# ── Resolve repo root ─────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# ── Set output directory ──────────────────────────────────────────────────────
TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR="validation/${TIMESTAMP}"
fi
mkdir -p "$OUT_DIR"

LOG_FILE="$OUT_DIR/dugite.log"
CSJ_EVENTS="$OUT_DIR/csj_events.jsonl"
LOE_SAMPLES="$OUT_DIR/loe_samples.jsonl"
SUMMARY="$OUT_DIR/summary.txt"
VIOLATIONS="$OUT_DIR/violations.txt"

echo "" > "$CSJ_EVENTS"
echo "" > "$LOE_SAMPLES"
echo "" > "$VIOLATIONS"

# ── Prerequisite checks ───────────────────────────────────────────────────────
check_prereq() {
    local cmd="$1"
    if ! command -v "$cmd" &>/dev/null; then
        echo "ERROR: '$cmd' not found on PATH" >&2
        exit 2
    fi
}
check_prereq jq

if [[ ! -x "$DUGITE_BIN" ]]; then
    echo "ERROR: dugite-node binary not found at '$DUGITE_BIN'" >&2
    echo "       Build with: cargo build --release" >&2
    exit 2
fi

if [[ ! -f "$CONFIG" ]]; then
    echo "ERROR: config file not found: $CONFIG" >&2
    exit 2
fi

if [[ ! -f "$TOPOLOGY" ]]; then
    echo "ERROR: topology file not found: $TOPOLOGY" >&2
    exit 2
fi

if [[ ! -d "$DATABASE_PATH/immutable" ]]; then
    echo "WARNING: database not found at $DATABASE_PATH" >&2
    echo "         Run: $DUGITE_BIN mithril-import --network-magic $NETWORK_MAGIC --database-path $DATABASE_PATH" >&2
    echo "         Proceeding anyway (node will sync from genesis — very slow)." >&2
fi

# ── Cleanup handler ───────────────────────────────────────────────────────────
DUGITE_PID=""
MONITOR_PID=""

cleanup() {
    if [[ -n "$MONITOR_PID" ]] && kill -0 "$MONITOR_PID" 2>/dev/null; then
        kill "$MONITOR_PID" 2>/dev/null || true
    fi
    if [[ -n "$DUGITE_PID" ]] && kill -0 "$DUGITE_PID" 2>/dev/null; then
        echo "Stopping dugite-node (PID $DUGITE_PID)..."
        kill -TERM "$DUGITE_PID" 2>/dev/null || true
        local i
        for i in $(seq 1 15); do
            sleep 1
            kill -0 "$DUGITE_PID" 2>/dev/null || break
        done
        kill -KILL "$DUGITE_PID" 2>/dev/null || true
    fi
    write_summary
}
trap cleanup EXIT INT TERM

# ── Event extraction from log lines ──────────────────────────────────────────
#
# Called in a background pipeline; receives log lines on stdin and writes
# JSONL event records to CSJ_EVENTS.
#
# Patterns match the tracing::info!/warn! calls in csj_orchestrator.rs and gsm.rs.
extract_csj_events() {
    local line ts event extra peer slot

    while IFS= read -r line; do
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        # Classify the line.
        if   echo "$line" | grep -qF "CSJ: elected new dynamo"; then
            event="DynamoElected"
            peer="$(echo "$line" | grep -oE 'new_dynamo=[^ ]+' | head -1 | cut -d= -f2)"
            extra="{\"peer\":\"${peer:-unknown}\"}"

        elif echo "$line" | grep -qF "CSJ: dynamo stalled; demoting"; then
            event="DynamoStallDemotion"
            peer="$(echo "$line" | grep -oE 'da=[^ ]+' | head -1 | cut -d= -f2)"
            extra="{\"peer\":\"${peer:-unknown}\"}"

        elif echo "$line" | grep -qF "CSJ: jump issued"; then
            event="JumpIssued"
            slot="$(echo "$line" | grep -oE 'jump_slot=[0-9]+' | head -1 | cut -d= -f2)"
            extra="{\"jump_slot\":${slot:-0}}"

        elif echo "$line" | grep -qF "CSJ: intersect found"; then
            event="IntersectFound"
            extra="{}"

        elif echo "$line" | grep -qF "CSJ: intersect not found"; then
            event="ObjectionRaised"
            extra="{}"

        elif echo "$line" | grep -qF "CSJ/GDD: dynamo wins"; then
            event="ObjectionResolved"
            extra="{\"outcome\":\"DynamoWins\"}"

        elif echo "$line" | grep -qF "CSJ/GDD: objector wins"; then
            event="ObjectionResolved"
            extra="{\"outcome\":\"ObjectorWins\"}"

        elif echo "$line" | grep -qF "CSJ invariant violation"; then
            event="InvariantViolation"
            extra="{\"message\":$(echo "$line" | jq -R .)}"
            echo "INVARIANT VIOLATION: $line" >> "$VIOLATIONS"

        elif echo "$line" | grep -qiE "LoE.*violation|loe_violation"; then
            event="LoEViolation"
            extra="{\"message\":$(echo "$line" | jq -R .)}"
            echo "LOE VIOLATION: $line" >> "$VIOLATIONS"

        else
            continue
        fi

        # Write JSONL record.
        printf '{"ts":"%s","event":"%s","extra":%s,"raw":%s}\n' \
            "$ts" "$event" "$extra" "$(echo "$line" | jq -R .)" \
            >> "$CSJ_EVENTS"
    done
}

# ── Prometheus LoE sampler ────────────────────────────────────────────────────
#
# Polls the Prometheus metrics endpoint every 30s and records the LoE slot
# alongside the node's current slot number. Logs a violation if slot > loe_slot.
sample_loe() {
    local metrics_url="http://localhost:${METRICS_PORT}/metrics"
    local interval=30
    local slot loe_slot ts

    while true; do
        sleep "$interval" || return 0

        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        # Fetch metrics; continue on curl failure (node may not be up yet).
        local raw
        raw="$(curl -sf --max-time 5 "$metrics_url" 2>/dev/null)" || continue

        slot="$(echo "$raw" | grep '^dugite_slot_number ' | awk '{print $2}' | head -1)"
        loe_slot="$(echo "$raw" | grep '^dugite_loe_slot ' | awk '{print $2}' | head -1)"
        local peers
        peers="$(echo "$raw" | grep '^dugite_peers_connected ' | awk '{print $2}' | head -1)"

        # Only record when we have values.
        [[ -z "$slot" ]] && continue

        printf '{"ts":"%s","slot":%s,"loe_slot":%s,"peers":%s}\n' \
            "$ts" \
            "${slot:-0}" \
            "${loe_slot:-null}" \
            "${peers:-0}" \
            >> "$LOE_SAMPLES"

        # Violation check: if loe_slot is set (non-null, non-empty, non-zero)
        # and the current slot exceeds it, record a violation.
        if [[ -n "$loe_slot" && "$loe_slot" != "null" && "$loe_slot" != "0" ]]; then
            if [[ -n "$slot" ]] && (( ${slot%%.*} > ${loe_slot%%.*} )); then
                local msg="LoE window violation at ${ts}: slot=${slot} > loe_slot=${loe_slot}"
                echo "LOE VIOLATION: $msg" >> "$VIOLATIONS"
                printf '{"ts":"%s","event":"LoEViolation","extra":{"slot":%s,"loe_slot":%s},"raw":"%s"}\n' \
                    "$ts" "${slot:-0}" "${loe_slot:-0}" "$msg" \
                    >> "$CSJ_EVENTS"
            fi
        fi
    done
}

# ── Start dugite-node in Genesis mode ─────────────────────────────────────────
echo "=== CSJ Phase F Validation (dugite) ===" | tee -a "$LOG_FILE"
echo "Started:   $TIMESTAMP" | tee -a "$LOG_FILE"
echo "Duration:  ${DURATION}s" | tee -a "$LOG_FILE"
echo "Config:    $CONFIG" | tee -a "$LOG_FILE"
echo "Database:  $DATABASE_PATH" | tee -a "$LOG_FILE"
echo "Output:    $OUT_DIR" | tee -a "$LOG_FILE"
echo "" | tee -a "$LOG_FILE"

START_EPOCH="$(date +%s)"

RUST_LOG="${LOG_LEVEL}" \
    "$DUGITE_BIN" run \
        --config "$CONFIG" \
        --topology "$TOPOLOGY" \
        --database-path "$DATABASE_PATH" \
        --socket-path "$SOCKET_PATH" \
        --host-addr 0.0.0.0 \
        --port "$PORT" \
        --consensus-mode genesis \
        --prometheus-port "$METRICS_PORT" \
    2>&1 \
    | tee -a "$LOG_FILE" \
    | extract_csj_events &

DUGITE_PID=$!
echo "dugite-node started (PID: ${DUGITE_PID})"

# Start LoE sampler in background.
sample_loe &
MONITOR_PID=$!

# Run until duration expires or dugite exits.
local_deadline=$(( START_EPOCH + DURATION ))
while true; do
    now="$(date +%s)"
    if (( now >= local_deadline )); then
        echo "Duration of ${DURATION}s elapsed — stopping."
        break
    fi
    # Check if dugite is still running.
    if ! kill -0 "$DUGITE_PID" 2>/dev/null; then
        echo "dugite-node exited before duration elapsed."
        break
    fi
    sleep 10
done

# ── write_summary (called from cleanup via EXIT trap) ─────────────────────────
write_summary() {
    local end_epoch
    end_epoch="$(date +%s)"
    local elapsed=$(( end_epoch - START_EPOCH ))
    local elapsed_human
    elapsed_human="$(printf '%dh %02dm %02ds' $((elapsed/3600)) $(( (elapsed%3600)/60 )) $((elapsed%60)))"

    # Count events from JSONL.
    local dynamo_count objection_count objection_resolved_count
    local invariant_count loe_violation_count jump_count

    dynamo_count="$(grep -c '"event":"DynamoElected"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
    objection_count="$(grep -c '"event":"ObjectionRaised"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
    objection_resolved_count="$(grep -c '"event":"ObjectionResolved"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
    invariant_count="$(grep -c '"event":"InvariantViolation"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
    loe_violation_count="$(grep -c '"event":"LoEViolation"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
    jump_count="$(grep -c '"event":"JumpIssued"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"

    # Last LoE sample for peer count.
    local final_peers="unknown"
    if [[ -s "$LOE_SAMPLES" ]]; then
        final_peers="$(tail -1 "$LOE_SAMPLES" | jq -r '.peers // "unknown"')"
    fi

    # Determine pass/fail.
    local pass_fail="PASS"
    local fail_reasons=()
    if (( dynamo_count == 0 )); then
        pass_fail="FAIL"
        fail_reasons+=("No DynamoElected event observed (CSJ may not be active)")
    fi
    if (( invariant_count > 0 )); then
        pass_fail="FAIL"
        fail_reasons+=("${invariant_count} CSJ invariant violation(s) detected")
    fi
    if (( loe_violation_count > 0 )); then
        pass_fail="FAIL"
        fail_reasons+=("${loe_violation_count} LoE-window violation(s) detected")
    fi

    cat > "$SUMMARY" <<EOF
=== CSJ Phase F Validation Summary (dugite) ===

Run timestamp:         $TIMESTAMP
Run duration:          ${elapsed_human} (${elapsed}s)
Node binary:           $DUGITE_BIN
Network magic:         $NETWORK_MAGIC
Config:                $CONFIG
Database:              $DATABASE_PATH
Output directory:      $OUT_DIR

--- Event Counts ---
DynamoElected:         $dynamo_count
JumpIssued:            $jump_count
ObjectionRaised:       $objection_count
ObjectionResolved:     $objection_resolved_count
InvariantViolation:    $invariant_count
LoEViolation:          $loe_violation_count

--- Final State ---
Connected peers:       $final_peers

--- Pass/Fail ---
Result: $pass_fail
EOF

    if [[ ${#fail_reasons[@]} -gt 0 ]]; then
        echo "" >> "$SUMMARY"
        echo "Failure reasons:" >> "$SUMMARY"
        for r in "${fail_reasons[@]}"; do
            echo "  - $r" >> "$SUMMARY"
        done
    fi

    echo "" >> "$SUMMARY"
    echo "Output files:" >> "$SUMMARY"
    echo "  CSJ events:  $CSJ_EVENTS" >> "$SUMMARY"
    echo "  LoE samples: $LOE_SAMPLES" >> "$SUMMARY"
    echo "  Node log:    $LOG_FILE" >> "$SUMMARY"
    echo "  Violations:  $VIOLATIONS" >> "$SUMMARY"

    cat "$SUMMARY"

    # Exit code mirrors pass/fail.
    if [[ "$pass_fail" == "FAIL" ]]; then
        exit 1
    fi
}
