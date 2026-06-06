#!/usr/bin/env bash
# test-smoke-wake.sh — dry ASSESS+SCHEDULE spine: helpers run, state parses, no heavy op.
# Proves the engine can start a wake with zero human intervention and zero side effects.
set -euo pipefail
cd "$(dirname "$0")/../../.."
D=scripts/prod-readiness

# 1. health sample is valid JSON
bash "$D/lib/health-sample.sh" | python3 -c 'import sys,json; json.load(sys.stdin)' \
  || { echo "FAIL: health-sample not valid JSON"; exit 1; }

# 2. STATE has all 9 sections
n=$(grep -cE '^## (Control|Frontiers|Backlog|In-progress|Running jobs|DB clones|Gauntlet ledger|Token spend|Last node state)' "$D/engine-state.md")
[ "$n" -eq 9 ] || { echo "FAIL: state sections = $n (want 9)"; exit 1; }

# 3. HALT defaults false
grep -q '^- HALT: false' "$D/engine-state.md" || { echo "FAIL: HALT not false"; exit 1; }

# 4. heavy-op lock starts free
[ "$(bash "$D/lib/heavyop-lock.sh" status)" = "free" ] || { echo "FAIL: lock not free at rest"; exit 1; }

# 5. runbook present, has the 5 phases, no placeholders
for ph in ASSESS SCHEDULE DRIVE RECORD RESCHEDULE; do
  grep -q "$ph" "$D/engine-runbook.md" || { echo "FAIL: runbook missing phase $ph"; exit 1; }
done
! grep -qE 'TODO|TBD|FIXME' "$D/engine-runbook.md" || { echo "FAIL: runbook has placeholders"; exit 1; }

# 6. muscle present with a meta literal
grep -q 'export const meta = {' "$D/muscle.workflow.js" || { echo "FAIL: muscle missing meta"; exit 1; }

# 7. no heavy op was started, no clone left behind by this test
[ "$(bash "$D/lib/heavyop-lock.sh" status)" = "free" ] || { echo "FAIL: smoke left a lock"; exit 1; }

echo "PASS smoke wake"
