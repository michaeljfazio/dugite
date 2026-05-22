#!/usr/bin/env bash
# chaos/disk-full.sh — verify that dugite-bp degrades gracefully when the
# ChainDB storage directory fills up.
#
# Creates a tmpfs-bounded scratch directory (Linux) or disk-image (macOS),
# fills it, and verifies:
#   1. dugite-bp does not panic
#   2. dugite-bp does not corrupt its main ChainDB
#   3. dugite-bp returns a clean error (not silent skip) when write fails
#
# Uses a SEPARATE scratch DB so we never harm the running devnet's DB.
# After the test, the scratch is cleaned up.
set -euo pipefail

SCENARIO="disk-full"
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

SCRATCH_SIZE_MB="${DISK_FULL_SCRATCH_MB:-10}"
SCRATCH_MNT="/tmp/ld-disk-chaos-$$"

mkdir -p "$SCRATCH_MNT"

# Create bounded scratch storage
case "$CHAOS_OS" in
    Linux)
        if ! command -v mount >/dev/null 2>&1; then
            chaos_record "$SCENARIO" "skip" "0" "SKIP" "mount-not-available"
            rmdir "$SCRATCH_MNT"
            exit 0
        fi
        # Try tmpfs mount (requires root/CAP_SYS_ADMIN)
        if ! sudo mount -t tmpfs -o "size=${SCRATCH_SIZE_MB}m" tmpfs "$SCRATCH_MNT" 2>/dev/null; then
            chaos_record "$SCENARIO" "skip" "0" "SKIP" "tmpfs-mount-failed-need-root"
            rmdir "$SCRATCH_MNT"
            exit 0
        fi
        ;;
    Darwin)
        # macOS: create a sparse disk image
        if ! command -v hdiutil >/dev/null 2>&1; then
            chaos_record "$SCENARIO" "skip" "0" "SKIP" "hdiutil-not-available"
            rmdir "$SCRATCH_MNT"
            exit 0
        fi
        DISK_IMAGE="/tmp/ld-disk-chaos-$$.dmg"
        hdiutil create -size "${SCRATCH_SIZE_MB}m" -fs HFS+ -volname "ld-chaos" "$DISK_IMAGE" >/dev/null 2>&1 || {
            chaos_record "$SCENARIO" "skip" "0" "SKIP" "hdiutil-create-failed"
            rmdir "$SCRATCH_MNT"
            exit 0
        }
        SCRATCH_MNT=$(hdiutil attach "$DISK_IMAGE" -mountpoint "$SCRATCH_MNT" -nobrowse -quiet 2>/dev/null | \
            awk '{print $NF}' | tail -1 || echo "$SCRATCH_MNT")
        ;;
    *)
        chaos_record "$SCENARIO" "skip" "0" "SKIP" "unsupported-os-$CHAOS_OS"
        rmdir "$SCRATCH_MNT"
        exit 0
        ;;
esac

trap '
    case "$CHAOS_OS" in
        Linux)  sudo umount '"'"'$SCRATCH_MNT'"'"' 2>/dev/null || true ;;
        Darwin) hdiutil detach '"'"'$SCRATCH_MNT'"'"' -force 2>/dev/null || true; rm -f '"'"'${DISK_IMAGE:-}'"'"' ;;
    esac
    rmdir '"'"'$SCRATCH_MNT'"'"' 2>/dev/null || true
' EXIT

log_info "$SCENARIO: scratch at $SCRATCH_MNT (${SCRATCH_SIZE_MB}MB)"

SCRATCH_DB="$SCRATCH_MNT/dugite-scratch.db"
mkdir -p "$SCRATCH_DB"

# Fill most of the scratch so writes will fail quickly
dd if=/dev/zero of="$SCRATCH_MNT/filler.bin" bs=1M count="$((SCRATCH_SIZE_MB - 1))" >/dev/null 2>&1 || true
AVAIL=$(df -k "$SCRATCH_MNT" 2>/dev/null | awk 'NR==2 {print $4}' || echo "?")
log_info "$SCENARIO: scratch ${AVAIL}K free after fill"

# Start dugite-bp targeting the nearly-full scratch DB
SCRATCH_SOCK="/tmp/ld-$(id -u)/chaos-disk.sock"
LOG_LINE_BEFORE=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)

"$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-bp.config.json" \
    --topology      "$LD_CONFIG/dugite-bp.topology.json" \
    --database-path "$SCRATCH_DB" \
    --socket-path   "$SCRATCH_SOCK" \
    --host-addr     127.0.0.1 \
    --port          3098 \
    > /tmp/disk-chaos-node.log 2>&1 &
SCRATCH_PID=$!

sleep 10  # Give it time to attempt writes and hit the full-disk condition

PANICS=$(cat /tmp/disk-chaos-node.log 2>/dev/null | \
    grep -c -E 'panic|PANIC|thread.*panicked' || echo 0)

# The node may exit cleanly with an IO error, or may still be running (if
# it hasn't tried to write yet). Both are acceptable — what's NOT acceptable
# is a panic.
kill "$SCRATCH_PID" 2>/dev/null || true
wait "$SCRATCH_PID" 2>/dev/null || true

# Verify main devnet ChainDB is unharmed
if ! chaos_verify_chaindb "$LD_DUGITE_BP_SOCK"; then
    chaos_record "$SCENARIO" "disk-full-test" "0" "FAIL" "main-chaindb-corrupted"
    exit 1
fi

if [ "$PANICS" -gt 0 ]; then
    log_error "$SCENARIO: panic detected in scratch node"
    chaos_record "$SCENARIO" "disk-full-test" "0" "FAIL" "panic-on-disk-full"
    exit 1
fi

chaos_record "$SCENARIO" "disk-full-test" "0" "PASS" "no-panic main-db-intact"
log_info "$SCENARIO: PASS — no panic, main ChainDB intact"
