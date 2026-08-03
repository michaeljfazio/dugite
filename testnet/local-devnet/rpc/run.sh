#!/usr/bin/env bash
# Drive the UTxO RPC (gRPC) suite and write evidence/<ts>/rpc.csv.      (#960)
#
# Usage:
#   ./run.sh                  — auto-detect the newest evidence dir
#   ./run.sh <evidence_dir>   — write rpc.csv there
#
# Exit status: 0 all good, 1 any FAIL/ERROR row.
#
# A SKIP is NOT success. env-skips (grpcurl missing, port closed) are reported
# distinctly from state-skips so the release generator can tell "we could not
# run this" from "we ran it and it passed" — the distinction #953 exists to
# enforce, and the one whose absence let the adversarial N2N suite report
# 26/26 while sending zero bytes.

set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/rpc-common.sh"
set +e

OUT_DIR="${1:-}"
if [ -z "$OUT_DIR" ]; then
    LATEST=$(ls -t "$LD_EVIDENCE" 2>/dev/null | head -1)
    if [ -n "$LATEST" ]; then
        OUT_DIR="$LD_EVIDENCE/$LATEST"
    else
        OUT_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
    fi
fi
mkdir -p "$OUT_DIR"

export RPC_CSV="$OUT_DIR/rpc.csv"
echo "ts,check,api_version,endpoint,status,detail" > "$RPC_CSV"

log_info "=== UTxO RPC suite (#960) ==="
log_info "bp=$RPC_BP_ADDR relay=$RPC_RELAY_ADDR"
log_info "Output: $RPC_CSV"

if ! command -v grpcurl >/dev/null 2>&1; then
    log_warn "grpcurl not installed — every check will record an env-skip."
    log_warn "  brew install grpcurl"
fi

# Record what the server actually advertises. If reflection is off or the
# service set changed, that shows up here rather than as N mystery failures.
if rpc_available "$RPC_BP_ADDR"; then
    SERVICES=$(grpcurl -plaintext -max-time 5 "$RPC_BP_ADDR" list 2>/dev/null | tr '\n' ' ')
    rpc_row "service-discovery" both "$RPC_BP_ADDR" PASS "advertised: ${SERVICES:-<none>}"
    # Both API versions must be present — a build that drops one silently is
    # the regression this row exists to catch.
    for want in utxorpc.v1alpha.query.QueryService utxorpc.v1beta.query.QueryService \
                utxorpc.v1alpha.submit.SubmitService utxorpc.v1beta.submit.SubmitService \
                utxorpc.v1alpha.sync.SyncService utxorpc.v1beta.sync.SyncService \
                utxorpc.v1alpha.watch.WatchService utxorpc.v1beta.watch.WatchService; do
        if printf '%s' "$SERVICES" | grep -q "$want"; then
            rpc_row "service-present" "${want%%.*}" "$want" PASS "advertised"
        else
            rpc_row "service-present" "-" "$want" FAIL "service NOT advertised by the running node"
        fi
    done
else
    rpc_row "service-discovery" both "$RPC_BP_ADDR" SKIP "env-skip: no gRPC reachable"
fi

# The relay must also serve RPC — it is a separate process with its own config
# path, and "works on the BP" has never implied "works on the relay".
if rpc_available "$RPC_RELAY_ADDR"; then
    rpc_row "relay-reachable" both "$RPC_RELAY_ADDR" PASS "relay gRPC answering"
else
    rpc_row "relay-reachable" both "$RPC_RELAY_ADDR" SKIP "env-skip: relay gRPC not reachable"
fi

RC=0
for s in "$SCRIPT_DIR"/0*.sh; do
    [ -f "$s" ] || continue
    log_info "--- $(basename "$s") ---"
    bash "$s" || RC=1
done

TOTAL=$(awk 'NR>1 && NF' "$RPC_CSV" | wc -l | tr -d ' ')
PASS=$(awk -F, 'NR>1 && $5=="PASS"  {c++} END{print c+0}' "$RPC_CSV")
FAIL=$(awk -F, 'NR>1 && $5=="FAIL"  {c++} END{print c+0}' "$RPC_CSV")
ERR=$( awk -F, 'NR>1 && $5=="ERROR" {c++} END{print c+0}' "$RPC_CSV")
ENVS=$(awk -F, 'NR>1 && $5=="SKIP" && $6~/env-skip/   {c++} END{print c+0}' "$RPC_CSV")
STS=$( awk -F, 'NR>1 && $5=="SKIP" && $6!~/env-skip/  {c++} END{print c+0}' "$RPC_CSV")

log_info "=== RPC suite summary ==="
log_info "  rows:       $TOTAL"
log_info "  PASS:       $PASS"
log_info "  FAIL:       $FAIL"
log_info "  ERROR:      $ERR"
log_info "  env-skip:   $ENVS"
log_info "  state-skip: $STS"
log_info "  CSV:        $RPC_CSV"

if [ "$FAIL" -gt 0 ] || [ "$ERR" -gt 0 ]; then
    log_error "RPC suite FAILED ($FAIL fail, $ERR error)"
    exit 1
fi
if [ "$PASS" -eq 0 ]; then
    log_error "RPC suite produced ZERO passing checks — treating as failure, not success."
    log_error "  (A suite that measures nothing must not report success: #923.)"
    exit 1
fi
exit $RC
