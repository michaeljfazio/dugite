#!/usr/bin/env bash
# PeerSharing (mini-protocol 10) adversarial tests.
#
# PeerSharing messages:
#   MsgShareRequest  = [0, amount_u8]           — client → server
#   MsgSharePeers    = [1, [peer_addr...]]       — server → client
#   MsgDone          = [2]                       — client → server
#
# Tests:
#   1. Request 0 peers (min boundary)
#   2. Request 255 peers (max u8)
#   3. Garbage CBOR
#   4. Amount too large (> 255, CBOR integer overflow)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

HS_CBOR="8200a10d8402182af4f4"
HS_LEN=$(( ${#HS_CBOR} / 2 ))
HS_FRAME=$(printf '00000000%04x%04x%s' 0 "$HS_LEN" "$HS_CBOR")

# PeerSharing = mini-protocol 10 (0x000a), initiator side
run_ps_case() {
    local name="$1" ps_hex="$2"
    local since; since=$(adv_log_line)
    local PS_LEN=$(( ${#ps_hex} / 2 ))
    local PS_FRAME; PS_FRAME=$(printf '00000000%04x%04x%s' $((0x000a)) "$PS_LEN" "$ps_hex")
    local closed=0
    adv_send_expect_close "${HS_FRAME}${PS_FRAME}" && closed=1 || closed=0
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0

    if [ "$no_panic" -eq 0 ]; then
        adv_record "peersharing" "$name" "PANIC" "panic"
        FAIL=$(( FAIL + 1 ))
    elif [ "$closed" -eq 0 ]; then
        # PeerSharing requests where amount is valid may NOT cause a close —
        # the server may respond and stay open.  Don't fail on that.
        adv_record "peersharing" "$name" "PASS" "no-panic, closed=${closed}"
        PASS=$(( PASS + 1 ))
    else
        adv_record "peersharing" "$name" "REJECTED" "closed cleanly"
        PASS=$(( PASS + 1 ))
    fi
}

# MsgShareRequest with amount=0: [0, 0]
run_ps_case "request-0-peers"   "820000"
# MsgShareRequest with amount=255: [0, 24ff]
run_ps_case "request-255-peers" "820018ff"
# MsgShareRequest with amount=65535 (> u8): [0, 19ffff]
run_ps_case "request-overflow"  "820019ffff"
# Garbage
run_ps_case "garbage"           "deadbeef"

log_info "=== peersharing adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
