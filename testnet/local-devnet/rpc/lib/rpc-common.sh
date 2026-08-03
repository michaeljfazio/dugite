#!/usr/bin/env bash
# Shared helpers for the UTxO RPC (gRPC) suite — #960.
#
# WHY THIS SUITE EXISTS
# ---------------------
# dugite-rpc ships four services (Query / Submit / Sync / Watch) at two API
# versions (v1alpha + v1beta). Before this suite, `testnet/local-devnet/`
# contained ZERO references to --rpc-port, grpcurl, or grpc: run.sh launched
# every node without the flag, so nothing here had ever spoken to the gRPC
# surface. Meanwhile SKILL.md listed gRPC `submit_tx` under the submit-path
# axis and "RPC oversized/replay/flood" under the actor axis as covered
# dimensions of the release gate. They were not covered; they were not
# reachable.
#
# That is the same failure shape the whole #962 backlog exists to remove — a
# suite that reports success while measuring nothing — so every check here
# reports SKIP with a reason rather than silently passing when a precondition
# (grpcurl absent, port closed) is missing, and run.sh fails the round on any
# ERROR row.

set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

RPC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$(cd "$RPC_DIR/.." && pwd)/lib/common.sh"
set +e

# lib/common.sh re-enables `set -e` on the way out (documented trap: it has
# killed three round scripts at their first nonzero command). Everything below
# deliberately runs commands that are EXPECTED to fail.

RPC_BP_ADDR="127.0.0.1:${LD_DUGITE_BP_RPC_PORT}"
RPC_RELAY_ADDR="127.0.0.1:${LD_DUGITE_RELAY_RPC_PORT}"

# CSV columns: ts,check,api_version,endpoint,status,detail
: "${RPC_CSV:=}"

rpc_row() {
    local check="$1" ver="$2" endpoint="$3" status="$4" detail="$5"
    detail="${detail//,/;}"
    detail="${detail//$'\n'/ }"
    [ -n "$RPC_CSV" ] && \
        printf '%s,%s,%s,%s,%s,%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$check" "$ver" "$endpoint" "$status" "$detail" \
            >> "$RPC_CSV"
    case "$status" in
        PASS)  printf '\033[0;32m[RPC PASS]\033[0m %-28s %-8s %s\n' "$check" "$ver" "$detail" ;;
        FAIL)  printf '\033[0;31m[RPC FAIL]\033[0m %-28s %-8s %s\n' "$check" "$ver" "$detail" ;;
        ERROR) printf '\033[0;31m[RPC ERR ]\033[0m %-28s %-8s %s\n' "$check" "$ver" "$detail" ;;
        SKIP)  printf '\033[0;33m[RPC SKIP]\033[0m %-28s %-8s %s\n' "$check" "$ver" "$detail" ;;
    esac
}

# rpc_available <addr> -> 0 if we can actually speak gRPC to it
rpc_available() {
    local addr="$1"
    command -v grpcurl >/dev/null 2>&1 || return 1
    grpcurl -plaintext -max-time 5 "$addr" list >/dev/null 2>&1
}

# rpc_call <addr> <method> <json-request> -> response JSON on stdout, rc=grpcurl's
#
# Empty request bodies are passed as `{}` rather than omitted: grpcurl with no
# -d reads stdin, which in a non-interactive round script blocks forever.
rpc_call() {
    local addr="$1" method="$2" body="${3:-{\}}"
    # Large bodies go via stdin: an 8 MiB payload passed as an argv element
    # blows past ARG_MAX ("Argument list too long"). When that happened to the
    # oversized-message case the request was never built, grpcurl failed for
    # the WRONG reason, and rpc_expect_error scored it PASS — a green row for a
    # check that never sent the payload it exists to send.
    if [ "${#body}" -gt 100000 ]; then
        printf '%s' "$body" | grpcurl -plaintext -max-time 30 -d @ "$addr" "$method" 2>&1
    else
        grpcurl -plaintext -max-time 30 -d "$body" "$addr" "$method" 2>&1
    fi
}

# rpc_expect_error <addr> <method> <body> <check> <ver> <what>
#
# Asserts the server answers with a STRUCTURED gRPC error and stays alive.
# A panic, a dropped connection, or a hang are all distinct from a clean
# InvalidArgument, and only the last one is acceptable — dugite-node is
# adversarial-deployment software, so "rejected loudly" is the requirement.
rpc_expect_error() {
    local addr="$1" method="$2" body="$3" check="$4" ver="$5" what="$6"
    local out rc
    out=$(rpc_call "$addr" "$method" "$body")
    rc=$?
    if [ "$rc" -eq 0 ]; then
        rpc_row "$check" "$ver" "$method" FAIL "$what was ACCEPTED (expected a structured error)"
        return 1
    fi
    # Connection-level failures mean the server died or hung up rather than
    # answering — that is the outcome this check exists to catch.
    if printf '%s' "$out" | grep -qiE 'connection refused|connection reset|EOF|transport is closing|Unavailable'; then
        rpc_row "$check" "$ver" "$method" FAIL "$what killed/closed the connection: $(printf '%s' "$out" | head -1)"
        return 1
    fi
    # Server still answering afterwards?
    if ! rpc_available "$addr"; then
        rpc_row "$check" "$ver" "$method" FAIL "$what left the RPC server unreachable"
        return 1
    fi
    rpc_row "$check" "$ver" "$method" PASS "$what rejected cleanly: $(printf '%s' "$out" | grep -oiE 'code = [A-Za-z]+' | head -1)"
    return 0
}

# Canonicalise a JSON document for comparison: sort keys recursively.
rpc_canon() { jq -S 'walk(if type=="object" then to_entries|sort|from_entries else . end)' 2>/dev/null; }
