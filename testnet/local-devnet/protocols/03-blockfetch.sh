#!/usr/bin/env bash
# Adversarial BlockFetch (mini-protocol 3) tests.
#
# BlockFetch messages:
#   MsgRequestRange  = [0, from_point, to_point]
#   MsgClientDone    = [1]
#   MsgStartBatch    = [2]
#   MsgNoBlocks      = [3]
#   MsgBlock         = [4, block_bytes]
#   MsgBatchDone     = [5]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

run_blockfetch_case() {
    local name="$1" bf_hex="$2"
    local since; since=$(adv_log_line)

    # Valid handshake header
    local HS_CBOR="8200a10d8402182af4f4"
    local HS_LEN=$(( ${#HS_CBOR} / 2 ))
    local HS_FRAME; HS_FRAME=$(printf '00000000%04x%04x%s' 0 "$HS_LEN" "$HS_CBOR")

    # BlockFetch mini-protocol = 3, initiator side
    local BF_LEN=$(( ${#bf_hex} / 2 ))
    local BF_FRAME; BF_FRAME=$(printf '00000000%04x%04x%s' 3 "$BF_LEN" "$bf_hex")

    local combined="${HS_FRAME}${BF_FRAME}"
    local closed=0
    adv_send_expect_close "$combined" && closed=1 || closed=0

    local no_panic=1
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0

    if [ "$no_panic" -eq 0 ]; then
        adv_record "blockfetch" "$name" "PANIC" "panic in relay log"
        FAIL=$(( FAIL + 1 ))
    elif [ "$closed" -eq 0 ]; then
        adv_record "blockfetch" "$name" "SILENT_SKIP" "connection not closed"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "blockfetch" "$name" "REJECTED" "closed cleanly"
        PASS=$(( PASS + 1 ))
    fi
}

# ---- Case 1: MsgRequestRange with inverted range (to < from) ----------------
# MsgRequestRange = [0, [1000, hash1], [1, hash2]]
# to_point.slot (1) < from_point.slot (1000) → must be rejected
REQ_RANGE_CBOR="8300820319e8204420000000820101442000000000"
run_blockfetch_case "inverted-range" "$REQ_RANGE_CBOR"

# ---- Case 2: MsgRequestRange with oversized range (slots 0 to 2^32-1) ------
HUGE_RANGE_CBOR="83008200442000000082001a${ffffffff}4420000000"
# Use a well-formed version
HUGE_RANGE_CBOR="83008201441a0000000082001affffffff44ffffffff"
run_blockfetch_case "oversized-range" "$HUGE_RANGE_CBOR"

# ---- Case 3: MsgBlock sent by the client (wrong direction — server sends) ----
# The client is not supposed to send MsgBlock; server should demote
MSGBLOCK_CBOR="82044500000000ff"
run_blockfetch_case "client-sends-block" "$MSGBLOCK_CBOR"

# ---- Case 4: Garbage CBOR in BlockFetch position ----------------------------
run_blockfetch_case "blockfetch-garbage" "cafebabe0102030405"

log_info "=== blockfetch adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
