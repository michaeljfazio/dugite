#!/usr/bin/env bash
# 08r — Raw malformed CBOR submitted directly to the N2C socket.
# Verifies dugite rejects it without panicking.
#
# Transport: the vendored raw-socket writer (lib/raw-socket-send.py), NOT socat.
# socat is not installed by default on macOS or on minimal CI images, and the
# old `command -v socat || SKIP` guard meant this adversarial case had never
# actually run on this host — a permanent SKIP reads as a PASS in the summary
# line (#918). python3 is already a hard tx-zoo dependency.
#
# What is sent, and what is asserted, is unchanged: one mux frame addressed to
# mini-protocol 5 (LocalTxSubmission) carrying garbage bytes, with no preceding
# handshake. The node must not panic. The connection outcome (closed-by-peer /
# reset / open) is recorded in the detail field for triage but does not by
# itself fail the case; a dead N2C socket afterwards does.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

SOCK="$LD_DUGITE_BP_SOCK"
if [ ! -S "$SOCK" ]; then
    zoo_record_env_skip "$NAME" "dugite-bp-socket-not-found"
    exit 0
fi
if [ ! -s "$ZOO_PY_RAW_SEND" ]; then
    zoo_record_env_skip "$NAME" "raw-socket-send.py-missing"
    exit 0
fi

# N2C LocalTxSubmission is mini-protocol 5. The mux header is 8 bytes:
#   [0..3] timestamp u32be, [4..5] (isResponse<<15)|protocol u16be,
#   [6..7] payload length u16be.  Initiator ⇒ isResponse=0 ⇒ 0x0005.
GARBAGE_HEX="deadbeefcafebabe010203040506070809"
GARBAGE_LEN=$(( ${#GARBAGE_HEX} / 2 ))
N2C_FRAME=$(printf '00000000%04x%04x%s' 5 "$GARBAGE_LEN" "$GARBAGE_HEX")

# Panic watermark before.
PRE_LINES=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)

SEND_JSON=$(python3 "$ZOO_PY_RAW_SEND" \
    --unix "$SOCK" \
    --hex  "$N2C_FRAME" \
    --connect-timeout 5 \
    --read-timeout 5 2>&1) && SEND_RC=0 || SEND_RC=$?

OUTCOME=$(printf '%s' "$SEND_JSON" | jq -r '.outcome // "unknown"' 2>/dev/null || echo unknown)
SENT=$(printf '%s' "$SEND_JSON" | jq -r '.sent // 0' 2>/dev/null || echo 0)
log_info "08r raw send: $SEND_JSON"

if [ "$SEND_RC" -eq 2 ]; then
    # Could not even connect — nothing was exercised, so do not claim a pass.
    zoo_record_env_skip "$NAME" "n2c-connect-failed outcome=${OUTCOME}"
    exit 0
fi

# Panic watermark after.
POST_LINES=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)
PANIC=0
if [ "$POST_LINES" -gt "$PRE_LINES" ]; then
    if awk "NR>$PRE_LINES" "$LD_LOGS/dugite-bp.log" | grep -qiE 'panicked|PANIC'; then
        PANIC=1
    fi
fi

# The node must still answer N2C queries: a malformed frame on one connection
# must not take down the socket handler for every other client.
ALIVE=1
cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" >/dev/null 2>&1 || ALIVE=0

if [ "$PANIC" -eq 1 ]; then
    zoo_record "$NAME" FAIL "" "PANIC in dugite-bp log after malformed N2C CBOR (conn=${OUTCOME})"
elif [ "$ALIVE" -eq 0 ]; then
    zoo_record "$NAME" FAIL "" "dugite-bp N2C socket unusable after malformed CBOR (conn=${OUTCOME})"
else
    zoo_record "$NAME" PASS "" "rejected-malformed-n2c-cbor no-panic sent=${SENT}B conn=${OUTCOME}"
fi
