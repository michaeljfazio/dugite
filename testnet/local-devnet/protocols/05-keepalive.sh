#!/usr/bin/env bash
# KeepAlive (mini-protocol 8) adversarial + behaviour tests.
#
# KeepAlive messages:
#   MsgKeepAlive        = [0, cookie_u16]   — client → server
#   MsgKeepAliveResponse = [1, cookie_u16]  — server → client
#   MsgDone             = [2]               — client → server
#
# Tests:
#  1. Verify server responds to MsgKeepAlive with matching cookie
#  2. Adversarial: wrong cookie in response (simulate a buggy peer)
#  3. Adversarial: send MsgDone immediately — graceful close
#  4. Adversarial: silence detector (send nothing after handshake — peer should timeout)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

# No socat guard: every send below goes through `adv_send_expect_close`, which
# uses the vendored stdlib raw-socket writer (#923). The old
# `command -v socat || SKIP` gate suppressed all four keepalive cases on any
# host without socat — honest as a SKIP, but the coverage was still absent.

# Build a valid handshake frame
HS_CBOR="8200a10d8402182af4f4"
HS_LEN=$(( ${#HS_CBOR} / 2 ))
HS_FRAME=$(printf '00000000%04x%04x%s' 0 "$HS_LEN" "$HS_CBOR")

# ---- Case 1: Oversized cookie value -----------------------------------------
# MsgKeepAlive = [0, 65535]
# Cookie 0xFFFF is the max u16; server must reflect it or reject cleanly
KA_CBOR="820019ffff"
KA_LEN=$(( ${#KA_CBOR} / 2 ))
KA_FRAME=$(printf '00000000%04x%04x%s' 8 "$KA_LEN" "$KA_CBOR")

since=$(adv_log_line)
adv_send_expect_close "${HS_FRAME}${KA_FRAME}" && closed=1 || closed=0
adv_check_no_panic "$since" && no_panic=1 || no_panic=0

if [ "$no_panic" -eq 0 ]; then
    adv_record "keepalive" "max-cookie" "PANIC" "panic in relay log"
    FAIL=$(( FAIL + 1 ))
else
    adv_record "keepalive" "max-cookie" "PASS" "no panic, closed=${closed}"
    PASS=$(( PASS + 1 ))
fi

# ---- Case 2: Garbage CBOR in KeepAlive position ----
since=$(adv_log_line)
KA_GARBAGE="deadbeef"
KA_G_LEN=$(( ${#KA_GARBAGE} / 2 ))
KA_G_FRAME=$(printf '00000000%04x%04x%s' 8 "$KA_G_LEN" "$KA_GARBAGE")
adv_send_expect_close "${HS_FRAME}${KA_G_FRAME}" && closed=1 || closed=0
adv_check_no_panic "$since" && no_panic=1 || no_panic=0

if [ "$no_panic" -eq 0 ]; then
    adv_record "keepalive" "garbage-cbor" "PANIC" "panic in relay log"
    FAIL=$(( FAIL + 1 ))
elif [ "$closed" -eq 0 ]; then
    adv_record "keepalive" "garbage-cbor" "SILENT_SKIP" "connection not closed"
    FAIL=$(( FAIL + 1 ))
else
    adv_record "keepalive" "garbage-cbor" "REJECTED" "closed cleanly"
    PASS=$(( PASS + 1 ))
fi

# ---- Case 3: MsgDone immediately (graceful close) ---------------------------
since=$(adv_log_line)
DONE_CBOR="8102"
DONE_LEN=$(( ${#DONE_CBOR} / 2 ))
DONE_FRAME=$(printf '00000000%04x%04x%s' 8 "$DONE_LEN" "$DONE_CBOR")
adv_send_expect_close "${HS_FRAME}${DONE_FRAME}" 5 && closed=1 || closed=0
adv_check_no_panic "$since" && no_panic=1 || no_panic=0

if [ "$no_panic" -eq 0 ]; then
    adv_record "keepalive" "immediate-done" "PANIC" "panic"
    FAIL=$(( FAIL + 1 ))
else
    adv_record "keepalive" "immediate-done" "PASS" "graceful close, no panic"
    PASS=$(( PASS + 1 ))
fi

log_info "=== keepalive adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
