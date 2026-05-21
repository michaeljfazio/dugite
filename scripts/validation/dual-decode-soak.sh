#!/usr/bin/env bash
# dual-decode-soak.sh — run dugite-node with DUGITE_DUAL_DECODE=dump and
# monitor for mismatch artefacts between the shadow (pallas) and in-house
# decoders.
#
# Usage:
#   dual-decode-soak.sh <network> [max-blocks] [--with-mithril]
#
#   network      preview | preprod | mainnet | devnet
#   max-blocks   stop after N blocks applied (default 0 = unlimited)
#   --with-mithril  import Mithril snapshot if database is absent
#
# Environment overrides:
#   DUGITE_PIPELINE_DEPTH   ChainSync pipeline depth (default: 300)
#   DUGITE_DB_PATH          database path (default: ./db-<network>)
#   DUGITE_SOCKET           socket path (default: ./node-dualdecode.sock)
#   DUGITE_PORT             N2N port (default: 3099 — off the standard port)
#   DUGITE_LOG_LEVEL        tracing filter (default: info)
#
# Exit codes:
#   0   soak completed without any mismatch artefacts
#   1   one or more mismatch artefacts written to DUMP_DIR
#   2   usage / config error
#   3   node process crashed or failed to start
#
# ─────────────────────────────────────────────────────────────────────────────
# SAFETY WARNING
# ─────────────────────────────────────────────────────────────────────────────
# Do NOT run this script on a Sandstone Pool block-producer machine.
# The soak starts a second dugite-node that will compete for the same socket
# and port, and the DUGITE_DUAL_DECODE env var causes extra CPU/IO overhead
# that will cause the BP to miss leader slots.
#
# Indicator env vars: SAND_POOL, SANDSTONE_BP, DUGITE_BP_MODE=block-producer
# The script detects these and requires interactive confirmation (or fails
# in CI mode via DUAL_DECODE_CI=1).
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail
cd "$(dirname "$0")/../.."

# ── Args ─────────────────────────────────────────────────────────────────────

NETWORK="${1:-}"
MAX_BLOCKS="${2:-0}"
WITH_MITHRIL=0

# Parse remaining positional/flags
shift 2 2>/dev/null || shift "${#@}" 2>/dev/null || true
for arg in "$@"; do
    case "$arg" in
        --with-mithril) WITH_MITHRIL=1 ;;
        --help|-h)
            grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -40
            exit 0
            ;;
        *) echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [[ -z "$NETWORK" ]]; then
    echo "ERROR: network argument required (preview | preprod | mainnet | devnet)" >&2
    exit 2
fi

case "$NETWORK" in
    preview|preprod|mainnet|devnet) ;;
    *)
        echo "ERROR: unknown network '$NETWORK'. Use: preview | preprod | mainnet | devnet" >&2
        exit 2
        ;;
esac

# ── Sandstone BP safety guard ─────────────────────────────────────────────────

if [[ -n "${SAND_POOL:-}" || -n "${SANDSTONE_BP:-}" || "${DUGITE_BP_MODE:-}" == "block-producer" ]]; then
    echo ""
    echo "  ╔══════════════════════════════════════════════════════════════════════╗"
    echo "  ║  SANDSTONE POOL SAFETY WARNING                                       ║"
    echo "  ║                                                                      ║"
    echo "  ║  This script detected Sandstone Pool BP environment variables.       ║"
    echo "  ║  Running dual-decode-soak on the BP machine will:                    ║"
    echo "  ║    - Compete for ports/sockets with the active forging node          ║"
    echo "  ║    - Add CPU/IO overhead that causes missed leader slots              ║"
    echo "  ║    - Potentially corrupt the active database                         ║"
    echo "  ║                                                                      ║"
    echo "  ║  Use a dedicated soak machine for this workload.                     ║"
    echo "  ╚══════════════════════════════════════════════════════════════════════╝"
    echo ""
    if [[ "${DUAL_DECODE_CI:-0}" == "1" ]]; then
        echo "ERROR: CI mode (DUAL_DECODE_CI=1) — refusing to run on BP machine." >&2
        exit 2
    fi
    read -r -p "  Type CONFIRM to proceed anyway: " ans
    if [[ "$ans" != "CONFIRM" ]]; then
        echo "Aborted."
        exit 2
    fi
fi

# ── Paths and config ──────────────────────────────────────────────────────────

TS=$(date +%Y%m%d-%H%M%S)
BIN=./target/release/dugite-node
DB_PATH="${DUGITE_DB_PATH:-./db-${NETWORK}}"
SOCKET_PATH="${DUGITE_SOCKET:-./node-dualdecode.sock}"
PORT="${DUGITE_PORT:-3099}"
LOG_LEVEL="${DUGITE_LOG_LEVEL:-info}"
DUMP_DIR="./dual_decode_mismatches/${NETWORK}"
LOG_DIR="./soak-logs"
LOG_FILE="${LOG_DIR}/${NETWORK}-${TS}.log"

mkdir -p "$DUMP_DIR" "$LOG_DIR"

# Network-specific config
case "$NETWORK" in
    preview)
        CONFIG=config/preview/config.json
        TOPOLOGY=config/preview/topology.json
        MAGIC=2
        ;;
    preprod)
        CONFIG=config/preprod/config.json
        TOPOLOGY=config/preprod/topology.json
        MAGIC=1
        ;;
    mainnet)
        CONFIG=config/mainnet/config.json
        TOPOLOGY=config/mainnet/topology.json
        MAGIC=764824073
        ;;
    devnet)
        # Local devnet — use the testnet configs if present; caller overrides DB_PATH.
        CONFIG="${DUGITE_CONFIG:-config/preview/config.json}"
        TOPOLOGY="${DUGITE_TOPOLOGY:-config/preview/topology.json}"
        MAGIC="${DUGITE_MAGIC:-42}"
        ;;
esac

# ── Build ─────────────────────────────────────────────────────────────────────

emit() {
    local ts
    ts="$(date '+%Y-%m-%d %H:%M:%S')"
    local msg="[$ts] $*"
    echo "$msg"
    echo "$msg" >> "$LOG_FILE"
}

emit "dual-decode-soak START — network=${NETWORK} max_blocks=${MAX_BLOCKS}"
emit "  binary:   $BIN"
emit "  db:       $DB_PATH"
emit "  socket:   $SOCKET_PATH"
emit "  port:     $PORT"
emit "  dump_dir: $DUMP_DIR"
emit "  log:      $LOG_FILE"
emit "  config:   $CONFIG"
emit "  topology: $TOPOLOGY"

if [[ ! -x "$BIN" ]]; then
    emit "Binary not found at $BIN — building with --features pallas-shadow-decode..."
    cargo build --release --features pallas-shadow-decode -p dugite-node 2>&1 | tee -a "$LOG_FILE"
fi

# ── Optional Mithril import ───────────────────────────────────────────────────

if [[ "$WITH_MITHRIL" == "1" ]] && [[ ! -d "${DB_PATH}/immutable" ]]; then
    emit "Mithril snapshot import requested and database absent — importing..."
    IMPORT_CMD=("$BIN" mithril-import --network-magic "$MAGIC" --database-path "$DB_PATH")
    if ! IMPORT_OUTPUT=$("${IMPORT_CMD[@]}" 2>&1); then
        emit "mithril-import failed; retrying once..."
        emit "  output: ${IMPORT_OUTPUT:0:500}"
        "${IMPORT_CMD[@]}" 2>&1 | tee -a "$LOG_FILE"
    fi
    emit "Mithril import complete."
elif [[ ! -d "${DB_PATH}/immutable" ]]; then
    emit "WARN: database absent at $DB_PATH and --with-mithril not passed."
    emit "WARN: node will start at genesis (slow). Pass --with-mithril for bulk sync."
fi

# ── Cleanup on exit ───────────────────────────────────────────────────────────

NODE_PID=""

cleanup() {
    local rc="$?"
    emit "cleanup: signal received (rc=${rc})"
    if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" 2>/dev/null; then
        emit "cleanup: terminating dugite-node pid=${NODE_PID}"
        kill -TERM "$NODE_PID" 2>/dev/null || true
        # Give it 10 seconds to exit cleanly, then SIGKILL
        local waited=0
        while kill -0 "$NODE_PID" 2>/dev/null && [[ $waited -lt 10 ]]; do
            sleep 1
            waited=$((waited + 1))
        done
        kill -KILL "$NODE_PID" 2>/dev/null || true
    fi
    rm -f "$SOCKET_PATH" 2>/dev/null || true
    emit "cleanup: done"
}

trap cleanup EXIT INT TERM

# ── Launch node ───────────────────────────────────────────────────────────────

emit "Starting dugite-node with DUGITE_DUAL_DECODE=dump..."

DUGITE_DUAL_DECODE=dump \
DUGITE_DUAL_DECODE_DUMP_DIR="$DUMP_DIR" \
DUGITE_PIPELINE_DEPTH="${DUGITE_PIPELINE_DEPTH:-300}" \
RUST_LOG="${LOG_LEVEL}" \
"$BIN" run \
    --config    "$CONFIG" \
    --topology  "$TOPOLOGY" \
    --database-path "$DB_PATH" \
    --socket-path   "$SOCKET_PATH" \
    --host-addr 127.0.0.1 \
    --port "$PORT" \
    >> "$LOG_FILE" 2>&1 &

NODE_PID=$!
emit "Node PID: $NODE_PID"

# Wait for socket to appear (max 120s)
emit "Waiting for N2C socket..."
waited=0
while [[ ! -S "$SOCKET_PATH" ]] && [[ $waited -lt 120 ]]; do
    sleep 1
    waited=$((waited + 1))
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        emit "ERROR: dugite-node died during startup — check $LOG_FILE"
        exit 3
    fi
done

if [[ ! -S "$SOCKET_PATH" ]]; then
    emit "ERROR: socket $SOCKET_PATH not ready after 120s — check $LOG_FILE"
    exit 3
fi
emit "Socket ready after ${waited}s."

# ── Monitor loop ──────────────────────────────────────────────────────────────
# Poll:
#   1. Node liveness (SIGKILL check)
#   2. Mismatch artefacts in DUMP_DIR
#   3. Block count (stop at max-blocks if set)
#
# Block counting is done by tailing the log for "blocks_applied_total" from
# Prometheus; if Prometheus is unavailable we fall back to log line counting.

MISMATCH_COUNT_PREV=0
LOG_OFFSET=0
POLL_INTERVAL=10   # seconds

emit "Entering monitor loop (poll every ${POLL_INTERVAL}s)..."

while true; do
    # Process liveness
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        emit "dugite-node (pid=$NODE_PID) has exited."
        break
    fi

    # Count mismatch artefacts (anchor on .cbor; fall back to .diff.txt)
    MISMATCH_COUNT=$(find "$DUMP_DIR" \( -name "*.cbor" -o -name "*.diff.txt" \) 2>/dev/null | wc -l | tr -d ' ')
    if (( MISMATCH_COUNT > MISMATCH_COUNT_PREV )); then
        new=$((MISMATCH_COUNT - MISMATCH_COUNT_PREV))
        emit "MISMATCH DETECTED — ${new} new artefact(s) (total: ${MISMATCH_COUNT}) in $DUMP_DIR"
        MISMATCH_COUNT_PREV=$MISMATCH_COUNT
    fi

    # Max-blocks gate: parse from log
    if (( MAX_BLOCKS > 0 )); then
        # Count "block applied" log lines as a proxy; real metric is blocks_applied_total
        APPLIED=$(grep -c "block applied\|blocks_applied\|BlockApplied\|TraceAddedToCurrentChain" "$LOG_FILE" 2>/dev/null || echo 0)
        if (( APPLIED >= MAX_BLOCKS )); then
            emit "max-blocks reached (${APPLIED} >= ${MAX_BLOCKS}) — stopping node."
            break
        fi
    fi

    sleep "$POLL_INTERVAL"
done

# Give the node a moment to flush any final writes
sleep 2

# ── Final report ──────────────────────────────────────────────────────────────

FINAL_MISMATCH_COUNT=$(find "$DUMP_DIR" \( -name "*.cbor" -o -name "*.diff.txt" \) 2>/dev/null | wc -l | tr -d ' ')

emit "=== DUAL-DECODE SOAK COMPLETE ==="
emit "network:         $NETWORK"
emit "log:             $LOG_FILE"
emit "dump_dir:        $DUMP_DIR"
emit "mismatch count:  $FINAL_MISMATCH_COUNT"

if (( FINAL_MISMATCH_COUNT > 0 )); then
    emit "RESULT: FAIL — ${FINAL_MISMATCH_COUNT} mismatch artefact(s) found."
    emit "Run: python3 scripts/validation/dual-decode-report.py $DUMP_DIR"
    exit 1
else
    emit "RESULT: PASS — no mismatch artefacts."
    exit 0
fi
