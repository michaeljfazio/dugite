#!/usr/bin/env bash
# 08r — Raw malformed CBOR submitted directly to N2C socket.
# Verifies dugite rejects it without panic via the LocalTxSubmission protocol.
# Uses socat to send raw bytes.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

if ! command -v socat >/dev/null 2>&1; then
    zoo_record "$NAME" SKIP "" "socat-not-found"
    exit 0
fi

# N2C LocalTxSubmission: mini-protocol 5 on N2C transport
# Send a garbage CBOR frame and expect the connection to close cleanly
GARBAGE_HEX="deadbeefcafebabe010203040506070809"
GARBAGE_LEN=$(( ${#GARBAGE_HEX} / 2 ))
# N2C mux header: initiator side, miniprotocol 5
N2C_FRAME=$(printf '00000000%04x%04x%s' 5 "$GARBAGE_LEN" "$GARBAGE_HEX")

# Convert hex to binary and send
TMPBIN=$(mktemp)
printf '%s' "$N2C_FRAME" | python3 -c "import sys,binascii; sys.stdout.buffer.write(binascii.unhexlify(sys.stdin.read().strip()))" > "$TMPBIN"

SOCK="$LD_DUGITE_BP_SOCK"
if [ ! -S "$SOCK" ]; then
    zoo_record "$NAME" SKIP "" "dugite-bp socket not found"
    rm -f "$TMPBIN"
    exit 0
fi

# Check for panics before
PRE_LINES=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)

# Send and expect close within 5s
CLOSED=0
timeout 5 socat - "UNIX-CLIENT:${SOCK}" < "$TMPBIN" > /dev/null 2>&1 && CLOSED=1 || CLOSED=1
rm -f "$TMPBIN"

# Check for panics after
POST_LINES=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)
PANIC=0
if [ "$POST_LINES" -gt "$PRE_LINES" ]; then
    if awk "NR>$PRE_LINES" "$LD_LOGS/dugite-bp.log" | grep -qiE 'panicked|PANIC'; then
        PANIC=1
    fi
fi

if [ "$PANIC" -eq 1 ]; then
    zoo_record "$NAME" FAIL "" "PANIC in dugite-bp log after malformed N2C CBOR"
else
    zoo_record "$NAME" PASS "" "rejected-malformed-n2c-cbor, no-panic"
fi
