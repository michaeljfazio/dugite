#!/usr/bin/env bash
# Regression test for #916: generate-release-report.sh and analyze-evidence.sh
# must agree on log error/warn counts for the same evidence directory, and the
# shared counter must match by log-LEVEL token, not by the substring "error"
# (dugite INFO lines legitimately carry `error=` fields).
#
# Usage: test-log-level-counts.sh    (no args; uses a temp fixture)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/log-level-counts.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

FIXTURE="$TMP/evidence/20260101T000000Z"
mkdir -p "$FIXTURE/logs"

# Fixture log: 2 real errors (ERROR level + panic), 2 real warns,
# and 3 decoys that must NOT count (error= field on INFO, lowercase error
# in message text, cardano-node JSON with "error" inside a field value).
cat > "$FIXTURE/logs/dugite-bp.log" <<'EOF'
2026-01-01T00:00:00Z INFO dugite_network::protocol::chainsync::serve_core: channel.recv() failed — task exiting protocol="LocalChainSync" error=bearer closed cursor_slot=0
2026-01-01T00:00:01Z INFO dugite_node: retrying connect error_count=3
2026-01-01T00:00:02Z ERROR dugite_ledger::state::apply: block apply failed slot=42
2026-01-01T00:00:03Z WARN dugite_ledger::validation: Validation: transaction rejected
2026-01-01T00:00:04Z INFO dugite_node: peer said something about an error today
thread 'main' panicked at 'boom', src/main.rs:1:1
2026-01-01T00:00:06Z WARN dugite_network: stale intersection retry
EOF
EXPECT_ERR=2
EXPECT_WARN=2

cat > "$FIXTURE/logs/cardano-bp.log" <<'EOF'
{"at":"2026-01-01T00:00:00Z","ns":"ChainDB.AddBlockEvent","data":{"kind":"AddedToCurrentChain","errormsg":"none"},"sev":"Info"}
EOF

FAIL=0
check() { # check <label> <got> <want>
    if [ "$2" != "$3" ]; then echo "FAIL: $1 — got $2, want $3"; FAIL=1; else echo "ok:   $1 = $2"; fi
}

# 1. Shared counters give level-token semantics on the tricky fixture.
check "count_log_errors dugite-bp" "$(count_log_errors "$FIXTURE/logs/dugite-bp.log")" "$EXPECT_ERR"
check "count_log_warns  dugite-bp" "$(count_log_warns  "$FIXTURE/logs/dugite-bp.log")" "$EXPECT_WARN"
check "count_log_errors cardano-bp (substring decoy)" "$(count_log_errors "$FIXTURE/logs/cardano-bp.log")" "0"

# 2. Both consumers on the SAME evidence dir report the SAME counts.
ANALYZE_OUT=$("$SCRIPT_DIR/analyze-evidence.sh" "$FIXTURE" 2>&1 || true)
A_ERR=$(printf '%s\n' "$ANALYZE_OUT" | sed -n 's/^ *dugite-bp *: *\([0-9]*\) ERROR.*/\1/p')
A_WARN=$(printf '%s\n' "$ANALYZE_OUT" | sed -n 's/^ *dugite-bp *: *[0-9]* ERROR \/ \([0-9]*\) WARN.*/\1/p')
check "analyze-evidence dugite-bp errors" "${A_ERR:-missing}" "$EXPECT_ERR"
check "analyze-evidence dugite-bp warns"  "${A_WARN:-missing}" "$EXPECT_WARN"

REPORT_DIR="$TMP/out"
"$SCRIPT_DIR/generate-release-report.sh" --output-dir "$REPORT_DIR" "$FIXTURE" >/dev/null 2>&1 || true
if [ -f "$REPORT_DIR/report.json" ]; then
    G_ERR=$(jq -r '.rounds[0].log_errors."dugite-bp".errors' "$REPORT_DIR/report.json")
    G_WARN=$(jq -r '.rounds[0].log_errors."dugite-bp".warns' "$REPORT_DIR/report.json")
    check "generate-release-report dugite-bp errors" "$G_ERR" "$EXPECT_ERR"
    check "generate-release-report dugite-bp warns"  "$G_WARN" "$EXPECT_WARN"
    check "generator == analyzer (errors)" "$G_ERR" "${A_ERR:-missing}"
    check "generator == analyzer (warns)"  "$G_WARN" "${A_WARN:-missing}"
else
    echo "FAIL: generate-release-report.sh produced no report.json"; FAIL=1
fi

[ "$FAIL" -eq 0 ] && echo "PASS: log-level counting agreement holds" || echo "FAILURES present"
exit "$FAIL"
