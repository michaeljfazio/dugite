#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# test_csj_validation_smoke.sh — 60-second CSJ Phase F smoke test
# ─────────────────────────────────────────────────────────────────────────────
#
# Brings up dugite-node in Genesis mode against a LOCAL cardano-node instance
# for 60 seconds and asserts:
#   (a) At least one DynamoElected event fires.
#   (b) No LoEViolation event fires.
#   (c) No InvariantViolation event fires.
#
# This is the automated gate — no 24h live sync required.
#
# PREREQUISITES
#   - dugite-node binary (default: ./target/release/dugite-node)
#   - cardano-node binary (for the local reference peer) — OR a pre-existing
#     peer reachable at PEER_HOST:PEER_PORT (pass --peer-addr host:port)
#   - A small local testnet or a reachable preview/preprod peer
#   - jq on PATH
#
# USAGE
#   # Using a local cardano-node as the peer (auto-started):
#   ./scripts/validation/test_csj_validation_smoke.sh \
#       --network-magic 2 \
#       --config config/preview/config.json \
#       --topology config/preview/topology.json \
#       --database-path ./db-preview
#
#   # Using a remote peer (cardano-node not managed by this script):
#   ./scripts/validation/test_csj_validation_smoke.sh \
#       --network-magic 2 \
#       --config config/preview/config.json \
#       --topology config/preview/topology.json \
#       --database-path ./db-preview \
#       --peer-addr 127.0.0.1:3001
#
#   # Custom dugite binary path:
#   ./scripts/validation/test_csj_validation_smoke.sh --dugite-bin ./target/debug/dugite-node
#
# OPTIONS
#   --network-magic N   Network magic (default: 2 = preview)
#   --config FILE       Node config JSON  (default: config/preview/config.json)
#   --topology FILE     Topology JSON     (default: config/preview/topology.json)
#   --database-path DIR ChainDB directory (default: ./db-preview)
#   --dugite-bin PATH   Path to dugite-node binary
#   --peer-addr H:P     Pre-existing peer; skip starting local cardano-node
#   --duration N        Run seconds (default: 60)
#   --out-dir DIR       Output directory (default: /tmp/csj-smoke-<timestamp>)
#   --help
#
# EXIT CODES
#   0 — both assertions pass
#   1 — one or more assertions fail (see RESULT section at the end)
#   2 — configuration/prerequisite error
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
NETWORK_MAGIC=2
CONFIG="config/preview/config.json"
TOPOLOGY="config/preview/topology.json"
DATABASE_PATH="./db-preview"
DUGITE_BIN="./target/release/dugite-node"
PEER_ADDR=""
DURATION=60
OUT_DIR=""

# ── Argument parsing ──────────────────────────────────────────────────────────
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --network-magic)  NETWORK_MAGIC="$2"; shift 2 ;;
        --config)         CONFIG="$2";        shift 2 ;;
        --topology)       TOPOLOGY="$2";      shift 2 ;;
        --database-path)  DATABASE_PATH="$2"; shift 2 ;;
        --dugite-bin)     DUGITE_BIN="$2";    shift 2 ;;
        --peer-addr)      PEER_ADDR="$2";     shift 2 ;;
        --duration)       DURATION="$2";      shift 2 ;;
        --out-dir)        OUT_DIR="$2";       shift 2 ;;
        --help|-h)        usage ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

# ── Resolve repo root ─────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR="/tmp/csj-smoke-${TIMESTAMP}"
fi
mkdir -p "$OUT_DIR"

DUGITE_LOG="$OUT_DIR/dugite.log"
CSJ_EVENTS="$OUT_DIR/csj_events.jsonl"
SMOKE_RESULT="$OUT_DIR/smoke_result.txt"

echo "" > "$CSJ_EVENTS"

# ── Prerequisite checks ───────────────────────────────────────────────────────
if [[ ! -x "$DUGITE_BIN" ]]; then
    echo "ERROR: dugite-node not found at '$DUGITE_BIN'" >&2
    echo "       Build with: cargo build --release" >&2
    exit 2
fi

if [[ ! -f "$CONFIG" ]]; then
    echo "ERROR: config file not found: $CONFIG" >&2
    exit 2
fi

# ── Cleanup handler ───────────────────────────────────────────────────────────
DUGITE_PID=""

cleanup() {
    if [[ -n "$DUGITE_PID" ]] && kill -0 "$DUGITE_PID" 2>/dev/null; then
        kill -TERM "$DUGITE_PID" 2>/dev/null || true
        sleep 3
        kill -KILL "$DUGITE_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

# ── Generate single-peer topology if --peer-addr is given ────────────────────
EFFECTIVE_TOPOLOGY="$TOPOLOGY"

if [[ -n "$PEER_ADDR" ]]; then
    PEER_HOST="${PEER_ADDR%%:*}"
    PEER_PORT="${PEER_ADDR##*:}"
    SINGLE_PEER_TOPO="$OUT_DIR/single-peer-topology.json"
    cat > "$SINGLE_PEER_TOPO" <<EOF
{
  "bootstrapPeers": [],
  "localRoots": [
    {
      "accessPoints": [
        {"address": "${PEER_HOST}", "port": ${PEER_PORT}}
      ],
      "advertise": false,
      "valency": 1,
      "warmValency": 1,
      "hotValency": 1,
      "trustable": true
    }
  ],
  "publicRoots": [],
  "useLedgerAfterSlot": -1
}
EOF
    EFFECTIVE_TOPOLOGY="$SINGLE_PEER_TOPO"
fi

# ── Event extraction ──────────────────────────────────────────────────────────
extract_csj_events() {
    local line ts event extra

    while IFS= read -r line; do
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

        if   echo "$line" | grep -qF "CSJ: elected new dynamo"; then
            event="DynamoElected"; extra="{}"
        elif echo "$line" | grep -qF "CSJ: dynamo stalled; demoting"; then
            event="DynamoStallDemotion"; extra="{}"
        elif echo "$line" | grep -qF "CSJ: jump issued"; then
            local slot
            slot="$(echo "$line" | grep -oE 'jump_slot=[0-9]+' | head -1 | cut -d= -f2 || echo 0)"
            event="JumpIssued"; extra="{\"jump_slot\":${slot:-0}}"
        elif echo "$line" | grep -qF "CSJ: intersect found"; then
            event="IntersectFound"; extra="{}"
        elif echo "$line" | grep -qF "CSJ: intersect not found"; then
            event="ObjectionRaised"; extra="{}"
        elif echo "$line" | grep -qF "CSJ/GDD: dynamo wins"; then
            event="ObjectionResolved"; extra="{\"outcome\":\"DynamoWins\"}"
        elif echo "$line" | grep -qF "CSJ/GDD: objector wins"; then
            event="ObjectionResolved"; extra="{\"outcome\":\"ObjectorWins\"}"
        elif echo "$line" | grep -qF "CSJ invariant violation"; then
            event="InvariantViolation"; extra="{}"
        elif echo "$line" | grep -qiE "LoE.*violation|loe_violation"; then
            event="LoEViolation"; extra="{}"
        else
            continue
        fi

        printf '{"ts":"%s","event":"%s","extra":%s}\n' \
            "$ts" "$event" "$extra" >> "$CSJ_EVENTS"
    done
}

# ── Start dugite-node ─────────────────────────────────────────────────────────
echo "=== CSJ Phase F Smoke Test ===" | tee "$DUGITE_LOG"
echo "Timestamp:   $TIMESTAMP"
echo "Duration:    ${DURATION}s"
echo "Network:     $NETWORK_MAGIC"
echo "Config:      $CONFIG"
echo "Topology:    $EFFECTIVE_TOPOLOGY"
echo "Database:    $DATABASE_PATH"
echo "Output:      $OUT_DIR"
if [[ -n "$PEER_ADDR" ]]; then
    echo "Peer:        $PEER_ADDR (single-peer override)"
fi
echo ""

RUST_LOG="info" \
    "$DUGITE_BIN" run \
        --config "$CONFIG" \
        --topology "$EFFECTIVE_TOPOLOGY" \
        --database-path "$DATABASE_PATH" \
        --socket-path "$OUT_DIR/node.sock" \
        --host-addr 127.0.0.1 \
        --port 13099 \
        --prometheus-port 12900 \
        --consensus-mode genesis \
    2>&1 \
    | tee -a "$DUGITE_LOG" \
    | extract_csj_events &

DUGITE_PID=$!
echo "dugite-node started (PID: $DUGITE_PID)"
echo "Waiting ${DURATION}s..."
sleep "$DURATION"

# Terminate after duration.
kill -TERM "$DUGITE_PID" 2>/dev/null || true
sleep 2
kill -KILL "$DUGITE_PID" 2>/dev/null || true
DUGITE_PID=""

# ── Evaluate assertions ───────────────────────────────────────────────────────
echo ""
echo "=== Evaluating assertions ==="

# Allow a brief moment for the pipeline to drain.
sleep 1

DYNAMO_COUNT="$(grep -c '"event":"DynamoElected"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
LOE_COUNT="$(grep -c '"event":"LoEViolation"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
INVARIANT_COUNT="$(grep -c '"event":"InvariantViolation"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
OBJECTION_COUNT="$(grep -c '"event":"ObjectionRaised"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"
JUMP_COUNT="$(grep -c '"event":"JumpIssued"' "$CSJ_EVENTS" 2>/dev/null || echo 0)"

PASS=true
FAILURES=()

# Assertion (a): at least one DynamoElected event.
if (( DYNAMO_COUNT == 0 )); then
    PASS=false
    FAILURES+=("FAIL: assertion (a) — no DynamoElected event observed in ${DURATION}s")
    FAILURES+=("      This means CSJ did not activate. Check:")
    FAILURES+=("      1. --consensus-mode genesis is accepted (node logs 'CSJ orchestrator started')")
    FAILURES+=("      2. At least 1 hot peer connected (check peer count in Prometheus or log)")
    FAILURES+=("      3. Network magic matches the topology's peers")
else
    echo "PASS: assertion (a) — DynamoElected fired ${DYNAMO_COUNT} time(s)"
fi

# Assertion (b): no LoEViolation.
if (( LOE_COUNT > 0 )); then
    PASS=false
    FAILURES+=("FAIL: assertion (b) — ${LOE_COUNT} LoEViolation event(s) detected")
else
    echo "PASS: assertion (b) — no LoEViolation events"
fi

# Assertion (c): no InvariantViolation.
if (( INVARIANT_COUNT > 0 )); then
    PASS=false
    FAILURES+=("FAIL: assertion (c) — ${INVARIANT_COUNT} CSJ InvariantViolation event(s) detected")
else
    echo "PASS: assertion (c) — no InvariantViolation events"
fi

# ── Write result file ─────────────────────────────────────────────────────────
{
    echo "=== CSJ Phase F Smoke Test Result ==="
    echo "Timestamp:  $TIMESTAMP"
    echo "Duration:   ${DURATION}s"
    echo ""
    echo "Event counts:"
    echo "  DynamoElected:      $DYNAMO_COUNT"
    echo "  JumpIssued:         $JUMP_COUNT"
    echo "  ObjectionRaised:    $OBJECTION_COUNT"
    echo "  LoEViolation:       $LOE_COUNT"
    echo "  InvariantViolation: $INVARIANT_COUNT"
    echo ""
    if $PASS; then
        echo "Result: PASS"
    else
        echo "Result: FAIL"
        echo ""
        for f in "${FAILURES[@]}"; do
            echo "  $f"
        done
    fi
    echo ""
    echo "Artefacts:"
    echo "  CSJ events: $CSJ_EVENTS"
    echo "  Node log:   $DUGITE_LOG"
} | tee "$SMOKE_RESULT"

if $PASS; then
    exit 0
else
    exit 1
fi
