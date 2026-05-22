#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# csj-phase-f-haskell.sh — CSJ Phase F cross-validation harness (Haskell side)
# ─────────────────────────────────────────────────────────────────────────────
#
# Runs cardano-node in Genesis mode, captures its JSON trace event stream,
# extracts TraceChainSyncClientEvent.TraceJumpResult events, and on termination
# writes a structured summary parallel to csj-phase-f-validate.sh.
#
# The two summary files and event streams can then be compared with the
# diff step documented in CSJ_PHASE_F.md.
#
# PREREQUISITES
#   - cardano-node (>= 10.6.2) on PATH or at CARDANO_NODE_BIN
#   - Database at DATABASE_PATH (use mithril-bootstrap or node sync first)
#   - Config with TraceChainSyncClient: true and ExperimentalHardForkVersion
#     set appropriately (the config must enable JSON trace output on stderr or
#     a trace socket — see TRACE_MODE below)
#   - jq on PATH
#
# USAGE
#   ./scripts/validation/csj-phase-f-haskell.sh [OPTIONS]
#
#   Options:
#     --network-magic N    Network magic (default: 764824073 = mainnet)
#     --config FILE        cardano-node config JSON
#                          (default: config/mainnet/config.json)
#     --topology FILE      Topology JSON (default: config/mainnet/topology.json)
#     --database-path DIR  ChainDB directory (default: ./db-mainnet-haskell)
#     --socket-path PATH   N2C socket (default: ./cn-csj-validate.sock)
#     --port N             P2P listen port   (default: 3098)
#     --duration N         Run duration in seconds (default: 86400 = 24h)
#     --out-dir DIR        Output directory  (default: validation/<timestamp>-haskell)
#     --cardano-node-bin   Path to cardano-node binary (default: cardano-node)
#     --help
#
# TRACE EVENTS CAPTURED
#   cardano-node emits structured JSON traces on stderr when configured with:
#     "TraceChainSyncClient": true,
#     "TraceChainSyncServerHeader": true
#
#   The events extracted from those traces include:
#     TraceJumpResult         → maps to JumpIssued/IntersectFound/ObjectionRaised
#     TraceDynamoChanged      → maps to DynamoElected
#     TraceObjectionRaised    → maps to ObjectionRaised
#     TraceObjectionResolved  → maps to ObjectionResolved
#
#   Raw JSON lines are written to haskell_events.jsonl alongside the normalised
#   JSONL that the diff step consumes.
#
# CARDANO-NODE GENESIS MODE CONFIGURATION
#   cardano-node enables CSJ automatically when the config contains:
#     "ConsensusMode": "GenesisMode"
#     or when invoked with --consensus-mode GenesisMode (cardano-node >=10.6)
#
#   The node must also have "EnableP2P": true and a populated topology.
#
# LOE-WINDOW VIOLATION DEFINITION (Haskell side)
#   The Haskell node emits a trace event when it detects a density violation;
#   this script greps for "densityViolation" or "LoEViolation" in the JSON
#   trace stream. See CSJ_PHASE_F.md for the full diff methodology.
#
# OUTPUT LAYOUT
#   <OUT_DIR>/
#     cardano-node.log         Raw node log (stderr)
#     haskell_events.jsonl     Extracted CSJ events (normalised JSONL)
#     haskell_raw_events.jsonl Raw JSON trace lines (for archival)
#     summary.txt              Human-readable summary matching dugite format
#     violations.txt           LoE violations (empty = pass)
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
NETWORK_MAGIC=764824073
CONFIG="config/mainnet/config.json"
TOPOLOGY="config/mainnet/topology.json"
DATABASE_PATH="./db-mainnet-haskell"
SOCKET_PATH="./cn-csj-validate.sock"
PORT=3098
DURATION=86400
OUT_DIR=""
CARDANO_NODE_BIN="cardano-node"

# ── Argument parsing ──────────────────────────────────────────────────────────
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --network-magic)   NETWORK_MAGIC="$2";    shift 2 ;;
        --config)          CONFIG="$2";           shift 2 ;;
        --topology)        TOPOLOGY="$2";         shift 2 ;;
        --database-path)   DATABASE_PATH="$2";    shift 2 ;;
        --socket-path)     SOCKET_PATH="$2";      shift 2 ;;
        --port)            PORT="$2";             shift 2 ;;
        --duration)        DURATION="$2";         shift 2 ;;
        --out-dir)         OUT_DIR="$2";          shift 2 ;;
        --cardano-node-bin) CARDANO_NODE_BIN="$2"; shift 2 ;;
        --help|-h)         usage ;;
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
    OUT_DIR="validation/${TIMESTAMP}-haskell"
fi
mkdir -p "$OUT_DIR"

LOG_FILE="$OUT_DIR/cardano-node.log"
CN_EVENTS="$OUT_DIR/haskell_events.jsonl"
CN_RAW_EVENTS="$OUT_DIR/haskell_raw_events.jsonl"
SUMMARY="$OUT_DIR/summary.txt"
VIOLATIONS="$OUT_DIR/violations.txt"

echo "" > "$CN_EVENTS"
echo "" > "$CN_RAW_EVENTS"
echo "" > "$VIOLATIONS"

# ── Prerequisite checks ───────────────────────────────────────────────────────
check_prereq() {
    if ! command -v "$1" &>/dev/null; then
        echo "ERROR: '$1' not found on PATH" >&2
        exit 2
    fi
}
check_prereq jq

if ! command -v "$CARDANO_NODE_BIN" &>/dev/null; then
    echo "ERROR: cardano-node not found at '$CARDANO_NODE_BIN'" >&2
    echo "       Install from: https://github.com/IntersectMBO/cardano-node/releases" >&2
    echo "       Required version: >= 10.6.2" >&2
    exit 2
fi

CN_VERSION="$("$CARDANO_NODE_BIN" --version 2>&1 | head -1 || echo "unknown")"
echo "cardano-node version: $CN_VERSION"

if [[ ! -f "$CONFIG" ]]; then
    echo "ERROR: config file not found: $CONFIG" >&2
    exit 2
fi

# ── Cleanup handler ───────────────────────────────────────────────────────────
CN_PID=""

cleanup() {
    if [[ -n "$CN_PID" ]] && kill -0 "$CN_PID" 2>/dev/null; then
        echo "Stopping cardano-node (PID $CN_PID)..."
        kill -TERM "$CN_PID" 2>/dev/null || true
        local i
        for i in $(seq 1 15); do
            sleep 1
            kill -0 "$CN_PID" 2>/dev/null || break
        done
        kill -KILL "$CN_PID" 2>/dev/null || true
    fi
    write_summary
}
trap cleanup EXIT INT TERM

# ── Event extraction from JSON trace stream ───────────────────────────────────
#
# cardano-node emits one JSON object per log line when configured with:
#   "defaultBackends": ["KatipBK"],
#   "setupBackends": ["KatipBK"],
#   "defaultScribes": [["StdoutSK", "stdout"]]
# and trace options like "TraceChainSyncClient: true".
#
# The JSON structure is typically:
#   {"at":"...","ns":"...","data":{"kind":"TraceJumpResult",...},"sev":"..."}
#
# We normalise these into the same JSONL schema as the dugite side:
#   {"ts":"...","event":"...","extra":{...},"raw":"..."}
extract_haskell_events() {
    local line ts kind event extra

    while IFS= read -r line; do
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        # Skip non-JSON lines (plain text startup messages).
        if ! echo "$line" | jq -e . &>/dev/null 2>&1; then
            continue
        fi

        # Archive raw JSON.
        echo "$line" >> "$CN_RAW_EVENTS"

        # Extract the "kind" field from the data object.
        kind="$(echo "$line" | jq -r '.data.kind // .data.val.kind // .data.msg // empty' 2>/dev/null || true)"

        case "$kind" in
            TraceDynamoChanged|TraceNewDynamo)
                event="DynamoElected"
                local peer
                peer="$(echo "$line" | jq -r '.data.peer // empty' 2>/dev/null || echo "unknown")"
                extra="{\"peer\":\"${peer}\"}"
                ;;
            TraceJumpResult)
                # Cardano-node TraceJumpResult has a "result" sub-field:
                # "result": "IntersectionFound" | "IntersectionNotFound"
                local result
                result="$(echo "$line" | jq -r '.data.result // empty' 2>/dev/null || true)"
                case "$result" in
                    IntersectionFound)
                        event="IntersectFound"
                        extra="{}"
                        ;;
                    IntersectionNotFound)
                        event="ObjectionRaised"
                        extra="{}"
                        ;;
                    *)
                        event="JumpIssued"
                        local slot
                        slot="$(echo "$line" | jq -r '.data.jumpSlot // 0' 2>/dev/null || echo 0)"
                        extra="{\"jump_slot\":${slot}}"
                        ;;
                esac
                ;;
            TraceObjectionRaised|TraceChainSyncJumpObjection)
                event="ObjectionRaised"
                extra="{}"
                ;;
            TraceObjectionResolved|TraceChainSyncJumpResolved)
                event="ObjectionResolved"
                local outcome
                outcome="$(echo "$line" | jq -r '.data.outcome // "unknown"' 2>/dev/null || echo "unknown")"
                extra="{\"outcome\":\"${outcome}\"}"
                ;;
            TraceDynamoStall|TraceDynamoTimedOut)
                event="DynamoStallDemotion"
                extra="{}"
                ;;
            *)
                # Also scan plain-text log lines for known patterns.
                if echo "$line" | grep -qiE "densityViolation|LoEViolation|loe.*violation"; then
                    event="LoEViolation"
                    extra="{\"message\":$(echo "$line" | jq -R .)}"
                    echo "LOE VIOLATION (Haskell): $line" >> "$VIOLATIONS"
                else
                    continue
                fi
                ;;
        esac

        printf '{"ts":"%s","event":"%s","extra":%s,"raw":%s}\n' \
            "$ts" "$event" "$extra" "$(echo "$line" | jq -R .)" \
            >> "$CN_EVENTS"
    done
}

# ── Start cardano-node in Genesis mode ────────────────────────────────────────
echo "=== CSJ Phase F Validation (Haskell) ===" | tee -a "$LOG_FILE"
echo "Started:   $TIMESTAMP" | tee -a "$LOG_FILE"
echo "Duration:  ${DURATION}s" | tee -a "$LOG_FILE"
echo "Config:    $CONFIG" | tee -a "$LOG_FILE"
echo "Database:  $DATABASE_PATH" | tee -a "$LOG_FILE"
echo "Version:   $CN_VERSION" | tee -a "$LOG_FILE"
echo "Output:    $OUT_DIR" | tee -a "$LOG_FILE"
echo "" | tee -a "$LOG_FILE"

START_EPOCH="$(date +%s)"

# cardano-node does not have a --consensus-mode CLI flag in all versions;
# the config file's "ConsensusMode" field controls it. We pass it via config.
# For versions that support the flag, add: --consensus-mode GenesisMode
"$CARDANO_NODE_BIN" run \
    --config "$CONFIG" \
    --topology "$TOPOLOGY" \
    --database-path "$DATABASE_PATH" \
    --socket-path "$SOCKET_PATH" \
    --host-addr "0.0.0.0" \
    --port "$PORT" \
    2>&1 \
    | tee -a "$LOG_FILE" \
    | extract_haskell_events &

CN_PID=$!
echo "cardano-node started (PID: ${CN_PID})"

# Wait for duration or exit.
local_deadline=$(( START_EPOCH + DURATION ))
while true; do
    now="$(date +%s)"
    if (( now >= local_deadline )); then
        echo "Duration of ${DURATION}s elapsed — stopping."
        break
    fi
    if ! kill -0 "$CN_PID" 2>/dev/null; then
        echo "cardano-node exited before duration elapsed."
        break
    fi
    sleep 10
done

# ── Summary ───────────────────────────────────────────────────────────────────
write_summary() {
    local end_epoch
    end_epoch="$(date +%s)"
    local elapsed=$(( end_epoch - START_EPOCH ))
    local elapsed_human
    elapsed_human="$(printf '%dh %02dm %02ds' $((elapsed/3600)) $(( (elapsed%3600)/60 )) $((elapsed%60)))"

    local dynamo_count objection_count objection_resolved_count
    local loe_violation_count jump_count

    dynamo_count="$(grep -c '"event":"DynamoElected"' "$CN_EVENTS" 2>/dev/null || echo 0)"
    objection_count="$(grep -c '"event":"ObjectionRaised"' "$CN_EVENTS" 2>/dev/null || echo 0)"
    objection_resolved_count="$(grep -c '"event":"ObjectionResolved"' "$CN_EVENTS" 2>/dev/null || echo 0)"
    loe_violation_count="$(grep -c '"event":"LoEViolation"' "$CN_EVENTS" 2>/dev/null || echo 0)"
    jump_count="$(grep -c '"event":"JumpIssued"' "$CN_EVENTS" 2>/dev/null || echo 0)"

    local pass_fail="PASS"
    local fail_reasons=()
    if (( dynamo_count == 0 )); then
        pass_fail="FAIL"
        fail_reasons+=("No DynamoElected event observed (CSJ may not be active in config)")
    fi
    if (( loe_violation_count > 0 )); then
        pass_fail="FAIL"
        fail_reasons+=("${loe_violation_count} LoE-window violation(s) detected")
    fi

    cat > "$SUMMARY" <<EOF
=== CSJ Phase F Validation Summary (Haskell cardano-node) ===

Run timestamp:         $TIMESTAMP
Run duration:          ${elapsed_human} (${elapsed}s)
Node binary:           $CARDANO_NODE_BIN
Node version:          $CN_VERSION
Network magic:         $NETWORK_MAGIC
Config:                $CONFIG
Database:              $DATABASE_PATH
Output directory:      $OUT_DIR

--- Event Counts ---
DynamoElected:         $dynamo_count
JumpIssued:            $jump_count
ObjectionRaised:       $objection_count
ObjectionResolved:     $objection_resolved_count
LoEViolation:          $loe_violation_count

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
    echo "  CSJ events:     $CN_EVENTS" >> "$SUMMARY"
    echo "  Raw JSON events: $CN_RAW_EVENTS" >> "$SUMMARY"
    echo "  Node log:       $LOG_FILE" >> "$SUMMARY"
    echo "  Violations:     $VIOLATIONS" >> "$SUMMARY"

    cat "$SUMMARY"

    if [[ "$pass_fail" == "FAIL" ]]; then
        exit 1
    fi
}
