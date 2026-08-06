#!/usr/bin/env bash
# Shared helpers for 18-plutus-edges (#1033): collateral / reference-input /
# datum edge cases from upstream cardano-node-tests tests_plutus*/ suites.
#
# Every script here locks/funds its OWN UTxO (the 17-context-inspecting
# isolation argument, see _ctx-helper.sh) so parity batches are independent.
#
# expect_utxo_rejection mirrors 16-cert-negatives/_cert-neg-helper.sh's
# expect_cert_rejection: "both nodes rejected it" is a weaker statement than
# it looks, because a client cannot act on the WRONG reject reason. Every
# negative test here therefore names the exact Conway UTXO/UTXOW
# predicate-failure constructor it expects, with three distinct outcomes
# rather than a binary pass/fail:
#
#   accepted             -> FAIL (dugite too lax)
#   rejected, named       -> PASS
#   rejected, different   -> FAIL (reject-reason divergence, #979 shape)
set -euo pipefail

EDGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$EDGE_DIR/../03-plutus/_lock-helper.sh"

# expect_utxo_rejection <name> <signed-tx> <expected-constructor> [<note>] [<sock>]
expect_utxo_rejection() {
    local name="$1" signed="$2" want="$3" note="${4:-}" sock="${5:-$ZOO_SOCKET}"
    local out rc
    out=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
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
    # A generic mempool failure is the specific regression this category
    # guards against (see 16-cert-negatives, #979) — call it out by name
    # rather than lumping it in with "wrong reason".
    if echo "$out" | grep -q 'ConwayMempoolFailure'; then
        zoo_fail "$name: degraded to ConwayMempoolFailure instead of $want (#979 shape)"
        zoo_record "$name" FAIL "" "degraded-to-mempool-failure-want-$want"
        return 1
    fi
    # A raw wire-decode failure (cardano-cli's own CDDL decoder choking on a
    # malformed rejection payload) must be labelled as such, not run through
    # the generic constructor-name regex below — that regex's `Babbage[A-Za-z]+`
    # alternative spuriously matches inert era-name tokens inside the
    # DeserialiseFailure's HardFork-combinator boilerplate (e.g. "BabbageEra"
    # from "ShelleyBlock (Praos StandardCrypto) BabbageEra"), which reads as a
    # real-but-wrong predicate name and hides a wire-corruption bug behind a
    # misleading "wrong reason" report.
    if echo "$out" | grep -q 'DeserialiseFailure'; then
        local decfail
        decfail=$(echo "$out" | grep -oE 'DeserialiseFailure [0-9]+ "[^"]*"' | head -1)
        zoo_fail "$name: node's rejection reply is malformed CBOR ($decfail) instead of $want — wire-encoder bug, not a wrong reason"
        zoo_record "$name" FAIL "" "malformed-cbor-reply-want-$want"
        return 1
    fi
    local got
    got=$(echo "$out" | grep -oE '\(?Conway[A-Za-z]*Failure[^)]*|Babbage[A-Za-z]+|[A-Za-z]+UTxO|[A-Za-z]+UTXOW?|[A-Za-z]+DELEG' \
          | head -1 | tr -d ',' | cut -c1-110)
    [ -z "$got" ] && got=$(echo "$out" | grep -m1 Error | tr -d ',' | cut -c1-110)
    zoo_fail "$name: rejected, but as '${got}' not $want"
    zoo_record "$name" FAIL "" "rejected-wrong-reason-want-$want"
    return 1
}

# Build (`transaction build`, auto fee/exunits) + sign, generic tx-in args
# passed straight through. Mirrors _cert-neg-helper.sh's cert_build_signed,
# but without its auto-UTxO-selection — every script here needs specific,
# already-known inputs (script UTxOs, collateral UTxOs, reference inputs),
# not "pick the biggest one at addr". Prints the signed file path on
# success; on failure the caller inspects $ZOO_LOGS/$name.err itself, same
# convention as 16a/16e's local-build-refusal handling.
edge_build_signed() {   # edge_build_signed <name> -- <build args...>
    local name="$1"; shift
    [ "${1:-}" = "--" ] && shift
    local raw="$ZOO_BUILT/$name.raw" signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        "$@" --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$name.err" || return 1
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" --tx-body-file "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" --out-file "$signed" >/dev/null || return 1
    printf '%s' "$signed"
}

# ---- 18l vendored artifact ----
#
# 18l needs ONE extra upstream compiled envelope beyond
# lib/plutus/*.plutus (which all come from
# tests/conformance/upstream/plutus-examples.json via lib/build-plutus.sh):
# a real PlutusV2 minting policy exercising byteStringToInteger, the V3-era
# builtin retroactively added to the PlutusV2 cost model. Vendored at
# tests/conformance/upstream/plutus-v2-v3-builtins.json — see that file's
# `description`/`source_*` fields for full provenance. No compiler runs
# here, same #969/#970 discipline as lib/build-plutus.sh.
EDGE_BUILTINS_FIXTURE="$EDGE_DIR/../../../../tests/conformance/upstream/plutus-v2-v3-builtins.json"
EDGE_BUILTINS_SCRIPT="$ZOO_LIB/plutus/upstream-byte-string-roundtrip-v2.plutus"

# Materialise + hash-verify the vendored envelope. Mirrors
# lib/build-plutus.sh's verify_upstream_hashes discipline: an envelope that
# loads under the WRONG hash silently moves the script's address, and every
# subsequent failure would look like a dugite bug instead of a vendoring bug.
edge_materialize_builtins_script() {
    [ -s "$EDGE_BUILTINS_FIXTURE" ] || {
        zoo_fail "missing $EDGE_BUILTINS_FIXTURE"
        return 1
    }
    command -v jq >/dev/null || { zoo_fail "jq required"; return 1; }

    local cbor want
    cbor="$(jq -r '.cborHex' "$EDGE_BUILTINS_FIXTURE")"
    want="$(jq -r '.scriptHash' "$EDGE_BUILTINS_FIXTURE")"
    if [ -z "$cbor" ] || [ "$cbor" = null ] || [ -z "$want" ] || [ "$want" = null ]; then
        zoo_fail "fixture $EDGE_BUILTINS_FIXTURE missing cborHex/scriptHash"
        return 1
    fi

    mkdir -p "$(dirname "$EDGE_BUILTINS_SCRIPT")"
    # See the fixture's cborHex_wrapping_note: this hex is ALREADY the
    # cardano-cli-ready double-wrapped form (it came from cardano-node-tests'
    # own text envelope), unlike lib/build-plutus.sh's _wrap_cbor_bytes path
    # which wraps a BARE flat-script hex once. Do not re-wrap it here.
    jq -n --arg t "PlutusScriptV2" \
          --arg d "byteStringToIntegerRoundtripPolicyV2 (cardano-node-tests, tx-zoo #1033)" \
          --arg c "$cbor" \
          '{type:$t, description:$d, cborHex:$c}' > "$EDGE_BUILTINS_SCRIPT"

    command -v cardano-cli >/dev/null || {
        zoo_info "cardano-cli absent — skipping hash verification of $EDGE_BUILTINS_SCRIPT"
        return 0
    }
    local got
    got="$(cardano-cli hash script --script-file "$EDGE_BUILTINS_SCRIPT" 2>/dev/null || true)"
    if [ -z "$got" ]; then
        # cardano-cli builds without a dedicated `hash script` subcommand
        # (or one that rejects V2 for some reason) still expose the same
        # hash via `transaction policyid` — script hash == policy id.
        got="$(cardano-cli conway transaction policyid --script-file "$EDGE_BUILTINS_SCRIPT" 2>/dev/null || true)"
    fi
    if [ -z "$got" ]; then
        zoo_fail "could not compute a hash for $EDGE_BUILTINS_SCRIPT with this cardano-cli"
        return 1
    fi
    if [ "$got" != "$want" ]; then
        zoo_fail "byteStringToIntegerRoundtripPolicyV2 hash mismatch: envelope=$got upstream=$want"
        return 1
    fi
    zoo_ok "verified byteStringToIntegerRoundtripPolicyV2 hash against cardano-node-tests ($want)"
    return 0
}
