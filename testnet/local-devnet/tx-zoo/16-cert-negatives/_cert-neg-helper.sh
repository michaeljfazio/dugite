#!/usr/bin/env bash
# Shared assertions for the certificate-level negative cases (#979).
#
# These exist because "both nodes rejected it" is a weaker statement than it
# looks. The bidirectional parity oracle scores a rejection whose REASON
# differs as CLASSDIFF, and a client cannot act on the wrong reason: it has no
# way to tell "you already registered that key" from "that pool does not
# exist". Every assertion here therefore names the Conway predicate-failure
# constructor it expects.
#
# Before #979 these all reached cardano-cli as
# `ConwayMempoolFailure "transaction validation failed"`, so a test that only
# checked for rejection passed while the wire form was wrong.

# expect_cert_rejection <name> <signed-tx> <expected-constructor> [<note>]
#
# Three outcomes, all distinct:
#   accepted            -> FAIL (dugite too lax)
#   rejected, named     -> PASS
#   rejected, different -> FAIL (reject-reason divergence)
expect_cert_rejection() {
    local name="$1" signed="$2" want="$3" note="${4:-}"
    local out rc
    out=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-file "$signed" 2>&1) && rc=0 || rc=1

    if [ "$rc" -eq 0 ]; then
        zoo_fail "$name: ACCEPTED — expected rejection with $want"
        zoo_record "$name" FAIL "" "accepted-expected-$want"
        return 1
    fi
    if echo "$out" | grep -q "$want"; then
        zoo_ok "$name: rejected with $want${note:+ ($note)}"
        zoo_record "$name" PASS "" "rejected-$want"
        return 0
    fi
    # A generic mempool failure is the specific regression these guard, so
    # call it out by name rather than lumping it in with "wrong reason".
    if echo "$out" | grep -q 'ConwayMempoolFailure'; then
        zoo_fail "$name: degraded to ConwayMempoolFailure instead of $want (#979)"
        zoo_record "$name" FAIL "" "degraded-to-mempool-failure-want-$want"
        return 1
    fi
    local got
    got=$(echo "$out" | grep -oE '\(Conway[A-Za-z]*Failure[^)]*|[A-Za-z]+DELEG|[A-Za-z]*POOL' \
          | head -1 | tr -d ',' | cut -c1-110)
    [ -z "$got" ] && got=$(echo "$out" | grep -m1 Error | tr -d ',' | cut -c1-110)
    zoo_fail "$name: rejected, but as '${got}' not $want"
    zoo_record "$name" FAIL "" "rejected-wrong-reason-want-$want"
    return 1
}

# Build + sign a certificate-bearing tx without submitting.
# Prints the signed file path.
cert_build_signed() {   # cert_build_signed <name> <addr> <skey...> -- <build args...>
    local name="$1" addr="$2"; shift 2
    local skeys=()
    while [ "$1" != "--" ]; do skeys+=("$1"); shift; done
    shift
    local utxo raw signed
    utxo=$(zoo_largest_utxo "$addr") || return 1
    raw="$ZOO_BUILT/$name.raw"; signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${utxo%% *}" --change-address "$addr" \
        "$@" --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$name.err" || return 1
    local args=(--testnet-magic "$LD_MAGIC" --tx-body-file "$raw")
    local k
    for k in "${skeys[@]}"; do args+=(--signing-key-file "$k"); done
    cardano-cli conway transaction sign "${args[@]}" --out-file "$signed" >/dev/null || return 1
    printf '%s' "$signed"
}
