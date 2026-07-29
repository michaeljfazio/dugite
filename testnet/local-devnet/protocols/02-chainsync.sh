#!/usr/bin/env bash
# Adversarial ChainSync (mini-protocol 2) tests.
#
# These tests require a fully handshaked connection.  We use a helper
# function that performs a valid N2N handshake first, then injects
# adversarial ChainSync messages.
#
# Cardano N2N ChainSync v7 messages:
#   MsgRequestNext     = [0]
#   MsgAwaitReply      = [1]
#   MsgRollForward     = [2, wrappedHeader, tip]
#   MsgRollBackward    = [3, point, tip]
#   MsgFindIntersect   = [4, [points...]]
#   MsgIntersectFound  = [5, point, tip]
#   MsgIntersectNotFound = [6, tip]
#   MsgDone            = [7]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

# Perform a valid handshake with devnet magic, then send adversarial ChainSync.
# Frames are written in two phases: handshake + ChainSync injection.
# For simplicity, we send the adversarial message immediately after the
# valid handshake (before the server responds with MsgAcceptVersion).
# The server must handle the out-of-order message gracefully.

run_chainsync_case() {
    local name="$1" chainsync_hex="$2"
    local since; since=$(adv_log_line)

    # Valid handshake for devnet (magic=42, version 13)
    # [0, {13: [42, false, 0, false]}]
    local HS_CBOR="8200a10d8402182af4f4"
    local HS_LEN=$(( ${#HS_CBOR} / 2 ))
    local HS_FRAME; HS_FRAME=$(printf '00000000%04x%04x%s' 0 "$HS_LEN" "$HS_CBOR")

    # ChainSync is mini-protocol 2, initiator side
    local CS_LEN=$(( ${#chainsync_hex} / 2 ))
    # miniprotocol 2, isResponse=0 → 0x0002
    local CS_FRAME; CS_FRAME=$(printf '00000000%04x%04x%s' 2 "$CS_LEN" "$chainsync_hex")

    # Combine and send
    local combined="${HS_FRAME}${CS_FRAME}"
    local closed=0
    adv_send_expect_close "$combined" && closed=1 || closed=0

    local no_panic=1
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0

    if [ "$no_panic" -eq 0 ]; then
        adv_record "chainsync" "$name" "PANIC" "panic in relay log"
        FAIL=$(( FAIL + 1 ))
    elif [ "$closed" -eq 0 ]; then
        adv_record "chainsync" "$name" "SILENT_SKIP" "connection survived adversarial message"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "chainsync" "$name" "REJECTED" "closed cleanly"
        PASS=$(( PASS + 1 ))
    fi
}

# ---- Case 1: MsgRollBackward to genesis (safe, large rollback depth) ---------
# point = origin = [0]
# This is a valid message but we send it without completing the handshake
# sequence first — the server should detect the protocol violation.
# MsgRollBackward = [3, [0], [0, "0000"]]
ROLLBACK_CBOR="8303810082004400000000"
run_chainsync_case "rollback-depth-1" "$ROLLBACK_CBOR"

# ---- Case 2: MsgFindIntersect with 1000 points (oversized) ------------------
# Generate [4, [point × 1000]] — expect rejection or truncation
# Each point = [slot, hash32] — we use dummy points
MANY_POINTS_CBOR="84049903e8"
for _ in $(seq 1 1000); do
    MANY_POINTS_CBOR+="820019fffff420"  # [65535, "f4"] (dummy)
done
run_chainsync_case "oversized-find-intersect" "$MANY_POINTS_CBOR"

# ---- Case 3: Completely invalid CBOR in ChainSync position ------------------
run_chainsync_case "chainsync-garbage" "deadbeefcafe"

# ---- Case 4: MsgDone sent before intersect (wrong state) --------------------
# MsgDone = [7] — sent before MsgFindIntersect which is required
MSGDONE_CBOR="8107"
run_chainsync_case "chainsync-premature-done" "$MSGDONE_CBOR"

log_info "=== chainsync adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
