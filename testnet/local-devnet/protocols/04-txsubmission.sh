#!/usr/bin/env bash
# Adversarial TxSubmission v2 (mini-protocol 4) tests.
#
# TxSubmission v2 messages (initiator = server, so we send as the responder):
#   MsgInit                = [6] (server sends to initiate)
#   MsgRequestTxIds        = [0, blocking, ackCount, reqCount]  — server → client
#   MsgReplyTxIds          = [1, [txid...]]                     — client → server
#   MsgRequestTxs          = [2, [txid...]]                     — server → client
#   MsgReplyTxs            = [3, [tx...]]                       — client → server
#   MsgDone                = [4]                                 — server → client
#
# NOTE: in TxSubmission v2, the remote node (server) drives the protocol.
# Our test connects as a peer (client), waits for MsgInit/MsgRequestTxIds,
# then injects garbage in response.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

run_txsub_case() {
    local name="$1" txsub_hex="$2"
    local since; since=$(adv_log_line)

    # Valid handshake
    local HS_CBOR="8200a10d8402182af4f4"
    local HS_LEN=$(( ${#HS_CBOR} / 2 ))
    local HS_FRAME; HS_FRAME=$(printf '00000000%04x%04x%s' 0 "$HS_LEN" "$HS_CBOR")

    # TxSubmission v2 = mini-protocol 4, responder side
    # isResponse bit set → 0x8004
    local TX_LEN=$(( ${#txsub_hex} / 2 ))
    local TX_FRAME; TX_FRAME=$(printf '00000000%04x%04x%s' $((0x8004)) "$TX_LEN" "$txsub_hex")

    local combined="${HS_FRAME}${TX_FRAME}"
    local closed=0
    adv_send_expect_close "$combined" && closed=1 || closed=0

    local no_panic=1
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0

    if [ "$no_panic" -eq 0 ]; then
        adv_record "txsubmission" "$name" "PANIC" "panic in relay log"
        FAIL=$(( FAIL + 1 ))
    elif [ "$closed" -eq 0 ]; then
        adv_record "txsubmission" "$name" "SILENT_SKIP" "connection not closed"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "txsubmission" "$name" "REJECTED" "closed cleanly"
        PASS=$(( PASS + 1 ))
    fi
}

# ---- Case 1: Duplicate tx IDs (same txid repeated 100 times) ----------------
# MsgReplyTxIds = [1, [[txid, size] × 100]] — server should detect duplicates
DUPE_TXID="5820$(printf 'aa%.0s' {1..32})"   # 32-byte dummy hash
DUPE_PAYLOAD="820182"
for _ in $(seq 1 100); do
    DUPE_PAYLOAD+="82${DUPE_TXID}190100"  # txid + size=256
done
run_txsub_case "duplicate-txids" "$DUPE_PAYLOAD"

# ---- Case 2: Flood — 1000 tx IDs in a single MsgReplyTxIds -----------------
FLOOD_PAYLOAD="82018219${3e8}"  # [1, [1000 entries]]
FLOOD_PAYLOAD="8201821903e8"
for _ in $(seq 1 1000); do
    hash=$(printf '%064x' $RANDOM)
    FLOOD_PAYLOAD+="8258${40}${hash}190100"
done
run_txsub_case "flood-txids" "$FLOOD_PAYLOAD"

# ---- Case 3: Garbage CBOR as MsgReplyTxIds ----------------------------------
run_txsub_case "txsub-garbage" "deadbeefcafebabe"

log_info "=== txsubmission adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
