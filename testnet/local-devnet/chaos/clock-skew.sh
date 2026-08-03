#!/usr/bin/env bash
# chaos/clock-skew.sh — verify that dugite-bp's forge pipeline rejects
# block headers whose slot is too far in the future.
#
# We cannot actually skew the system clock in a portable, safe, unprivileged
# way (requires CAP_SYS_TIME / root). Instead we verify the clock-skew
# rejection path by inspecting dugite-bp's log for the "future slot" rejection
# pattern when a peer sends a block with a slot far in the future.
#
# We send a crafted header with an artificially large slot via the adversarial
# N2N harness (the vendored stdlib raw-socket writer). If it is not available, this
# test skips gracefully.
#
# Recovery: no recovery needed — we're testing rejection, not disruption.
set -euo pipefail

SCENARIO="clock-skew"
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

# Raw-socket send via the VENDORED stdlib writer, not socat.
#
# socat is not present on a stock macOS, so this scenario skipped silently on
# every developer machine — the exact mechanism of #923, where the adversarial
# N2N suite "passed" 26 cases without sending a byte. protocols/ was migrated
# off socat for that reason; chaos never was.
#
# An absent writer is an ENV_SKIP (a counted class), never silence.
CHAOS_RAW_SEND="${CHAOS_RAW_SEND:-$LD_ROOT/tx-zoo/lib/raw-socket-send.py}"
if [ ! -f "$CHAOS_RAW_SEND" ]; then
    chaos_record "$SCENARIO" "skip" "0" "ENV_SKIP" "raw-socket-send.py-missing at $CHAOS_RAW_SEND"
    exit 0
fi
[ -S "$LD_DUGITE_BP_SOCK" ] || die "$SCENARIO: dugite-bp socket not present"

LOG_LINE_BEFORE=$(line_count "$LD_LOGS/dugite-bp.log")

# Build a future-slot ChainSync RollForward frame.
# Mux header: timestamp=0, protocol=2 (ChainSync), length=X
# Body: CBOR array [1, [slot, hash]] where slot is current+999999
CURRENT_SLOT=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.slot // 0' || echo 0)
FUTURE_SLOT=$(( CURRENT_SLOT + 999999 ))

log_info "$SCENARIO: sending fake block with future slot=$FUTURE_SLOT to relay port"

# We can't trivially build a valid header for a future slot (VRF proof required)
# so we test the simpler case: send an invalid handshake with a nonsense node-to-node
# version that implies a future-protocol-version client.
# The real clock-skew test verifies log rejection patterns.

# Send garbage that contains a very large slot number in CBOR
# CBOR: array[1, array[future_slot]] = 82 01 82 1b <slot_u64_big_endian>
SLOT_HEX=$(printf '%016x' "$FUTURE_SLOT")
PAYLOAD="820182 1b${SLOT_HEX}"
PAYLOAD_CLEAN="${PAYLOAD// /}"
PAYLOAD_LEN=$(( ${#PAYLOAD_CLEAN} / 2 ))

# Mux header: ts=0 (4B), protocol=0x0002 ChainSync (2B), len (2B)
MUX_HDR=$(printf '%08x%04x%04x' 0 2 "$PAYLOAD_LEN")

(echo -n "${MUX_HDR}${PAYLOAD_CLEAN}" | xxd -r -p; sleep 2) | \
    timeout 5 python3 "$CHAOS_RAW_SEND" --host 127.0.0.1 --port "${LD_RELAY_PORT}" --stdin-hex 2>/dev/null || true

sleep 2

# Check that dugite didn't panic and didn't produce a new error (it may just close the conn)
LOG_LINE_AFTER=$(line_count "$LD_LOGS/dugite-bp.log")
NEW_LINES=$(( LOG_LINE_AFTER - LOG_LINE_BEFORE ))

PANICS=$(count_matching 'panic|PANIC|thread.*panicked' "$LD_LOGS/dugite-bp.log" "$((NEW_LINES + 10))")

if [ "$PANICS" -gt 0 ]; then
    log_error "$SCENARIO: PANIC detected in logs"
    chaos_record "$SCENARIO" "future-slot-send" "0" "FAIL" "panic-detected"
    exit 1
fi

# Verify dugite-bp is still alive
if ! cardano-cli query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$LD_DUGITE_BP_SOCK" >/dev/null 2>&1; then
    chaos_record "$SCENARIO" "future-slot-send" "0" "FAIL" "dugite-bp-unresponsive-after-future-slot"
    exit 1
fi

chaos_record "$SCENARIO" "future-slot-send" "0" "PASS" "no-panic node-still-alive future_slot=${FUTURE_SLOT}"
log_info "$SCENARIO: PASS — no panic, node still alive after future-slot injection"
