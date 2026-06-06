#!/usr/bin/env bash
# Unit checks for disk GC: keep-last-N retention + disk-fit predicate.
set -euo pipefail
cd "$(dirname "$0")/../../.."
G=scripts/prod-readiness/lib/gc-disk.sh

TMP=$(mktemp -d)
export REPO_ROOT="$TMP" CLONES_DIR="$TMP/db-clones" DUMPS_DIR="$TMP/dumps" KEEP_CLONES=2 MIN_FREE_GB=40
mkdir -p "$CLONES_DIR"
trap 'rm -rf "$TMP"' EXIT

# four preprod clones, ascending mtime ep1<ep2<ep3<ep4
for i in 1 2 3 4; do
  mkdir -p "$CLONES_DIR/preprod-ep$i"
  touch -t "0101000${i}00" "$CLONES_DIR/preprod-ep$i"
done

"$G" gc-clones preprod
left=$(find "$CLONES_DIR" -maxdepth 1 -name 'preprod-*' | wc -l | tr -d ' ')
[ "$left" -eq 2 ] || { echo "FAIL: expected 2 kept, got $left"; exit 1; }
[ -d "$CLONES_DIR/preprod-ep4" ] && [ -d "$CLONES_DIR/preprod-ep3" ] \
  || { echo "FAIL: newest two not kept"; exit 1; }
[ ! -d "$CLONES_DIR/preprod-ep1" ] || { echo "FAIL: oldest not GC'd"; exit 1; }

# disk-fit: an impossibly large clone never fits
if "$G" fits 999999; then echo "FAIL: impossible size reported as fitting"; exit 1; fi
# a 0 GB clone always fits (free disk on tmp volume >> MIN_FREE_GB)
"$G" fits 0 || { echo "FAIL: zero-size should fit"; exit 1; }

echo "PASS"
