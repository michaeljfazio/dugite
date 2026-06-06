#!/usr/bin/env bash
# Unit checks for the single heavy-op lock: acquire / bind / reclaim / release.
set -euo pipefail
cd "$(dirname "$0")/../../.."
L=scripts/prod-readiness/lib/heavyop-lock.sh

TMP=$(mktemp -d)
export ENGINE_DIR="$TMP" REPO_ROOT="$TMP" LOCK_FILE="$TMP/.engine-heavyop.lock" HEAVYOP_TTL_SECS=2
trap 'rm -rf "$TMP"' EXIT

# 1. fresh acquire on an empty slot succeeds
"$L" acquire "replay-ep57" || { echo "FAIL: fresh acquire"; exit 1; }

# 2. a lock bound to a LIVE pid with a recent start blocks a new acquire
printf 'pid=%s\nstart=%s\nlabel=live\n' "$$" "$(date +%s)" > "$LOCK_FILE"
if "$L" acquire "replay-other" 2>/dev/null; then echo "FAIL: acquired over a live holder"; exit 1; fi

# 3. dead-pid holder is reclaimable
printf 'pid=999999\nstart=%s\nlabel=zombie\n' "$(date +%s)" > "$LOCK_FILE"
"$L" acquire "after-zombie" || { echo "FAIL: dead-pid not reclaimed"; exit 1; }

# 4. TTL-expired holder is reclaimable even if pid is live
printf 'pid=%s\nstart=1\nlabel=ancient\n' "$$" > "$LOCK_FILE"
"$L" acquire "after-ttl" || { echo "FAIL: TTL not reclaimed"; exit 1; }

# 5. bind updates the recorded pid (so liveness tracks the real bg job)
"$L" bind 4242
grep -q '^pid=4242$' "$LOCK_FILE" || { echo "FAIL: bind did not update pid"; exit 1; }

# 6. release clears the lock
"$L" release && [ ! -f "$LOCK_FILE" ] || { echo "FAIL: release"; exit 1; }

echo "PASS"
