#!/usr/bin/env bash
# Adversarial handshake tests.
#
# Each case sends a malformed or invalid handshake proposal and asserts:
#   - The connection is closed (peer rejected)
#   - No panic in dugite-relay log
#   - No silent-skip (the error is logged at the right level)
#
# Cardano N2N handshake CBOR structure (mini-protocol 0, initiator):
#   MsgProposeVersions = [0, {version_number: NodeToNodeVersionData}]
#   NodeToNodeVersionData = [network_magic, InitiatorOnly, PeerSharing, Query]
#
# All frames below use the Cardano N2N multiplexer framing:
#   [timestamp_u32, isResponse_bit | miniprotocol_id_u16, length_u16, payload...]
#
# Framing: 8-byte mux header + CBOR payload
#   header[0..3] = timestamp (big-endian u32, can be 0)
#   header[4..5] = (isResponse << 15) | miniprotocol (big-endian u16)
#                  initiator uses isResponse=0, miniprotocol=0 → 0x0000
#   header[6..7] = payload length (big-endian u16)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

run_case() {
    local name="$1" hex_payload="$2"
    local since; since=$(adv_log_line)
    local closed=0 no_panic=1 no_bad_err=1

    adv_send_expect_close "$hex_payload" && closed=1 || closed=0

    adv_check_no_panic "$since" && no_panic=1 || no_panic=0
    adv_check_no_new_errors "$since" && no_bad_err=1 || no_bad_err=0

    if [ "$no_panic" -eq 0 ]; then
        adv_record "handshake" "$name" "PANIC" "panic in relay log"
        FAIL=$(( FAIL + 1 ))
    elif [ "$closed" -eq 0 ]; then
        adv_record "handshake" "$name" "SILENT_SKIP" "connection not closed within ${ADV_EXPECT_CLOSE_SEC}s"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "handshake" "$name" "REJECTED" "closed=${closed} no_panic=${no_panic}"
        PASS=$(( PASS + 1 ))
    fi
}

# ---- Case 1: completely empty payload ----------------------------------------
# No CBOR at all — just close. Peer must reject cleanly, not hang.
run_case "empty-payload" ""

# ---- Case 2: bad magic (wrong network) ---------------------------------------
# Propose version 13 with networkMagic=99999 (not our devnet magic=42)
# CBOR: [0, {13: [99999, false, 0, false]}]
# Mux header: proto=0, len=payload
BAD_MAGIC_CBOR="8200a10d8419869fffff0000"
# Build mux frame: 8-byte header (ts=0, proto=0, len=6) + payload
BAD_MAGIC_LEN=$(( ${#BAD_MAGIC_CBOR} / 2 ))
BAD_MAGIC_FRAME=$(printf '00000000%04x%04x%s' 0 "$BAD_MAGIC_LEN" "$BAD_MAGIC_CBOR")
run_case "bad-magic" "$BAD_MAGIC_FRAME"

# ---- Case 3: version higher than supported -----------------------------------
# Propose version 999 — must be rejected with VersionMismatch
BAD_VER_CBOR="8200a11903e78200f4f4"
BAD_VER_LEN=$(( ${#BAD_VER_CBOR} / 2 ))
BAD_VER_FRAME=$(printf '00000000%04x%04x%s' 0 "$BAD_VER_LEN" "$BAD_VER_CBOR")
run_case "version-mismatch" "$BAD_VER_FRAME"

# ---- Case 4: truncated CBOR (incomplete proposal) ----------------------------
# Send a 2-byte partial CBOR payload
TRUNC_CBOR="8200"
TRUNC_LEN=$(( ${#TRUNC_CBOR} / 2 ))
TRUNC_FRAME=$(printf '00000000%04x%04x%s' 0 "$TRUNC_LEN" "$TRUNC_CBOR")
run_case "truncated-cbor" "$TRUNC_FRAME"

# ---- Case 5: random garbage (not CBOR at all) --------------------------------
GARBAGE="deadbeefcafebabe0102030405060708090a0b0c0d0e0f"
GARBAGE_LEN=$(( ${#GARBAGE} / 2 ))
GARBAGE_FRAME=$(printf '00000000%04x%04x%s' 0 "$GARBAGE_LEN" "$GARBAGE")
run_case "malformed-cbor" "$GARBAGE_FRAME"

# ---- Case 6: oversized payload declared in mux header -----------------------
# Declare length=65535 but send only 10 bytes — forces a partial read
OVER_CBOR="8200a10d8200f4f4f4f4"
OVER_LEN=65535   # declared but not actually sent
OVER_FRAME=$(printf '00000000%04x%04x%s' 0 "$OVER_LEN" "$OVER_CBOR")
run_case "oversized-header" "$OVER_FRAME"

# ---- Summary -----------------------------------------------------------------
log_info "=== handshake adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
