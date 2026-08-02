#!/usr/bin/env bash
# Shared helpers for 14-gov-negatives.
#
# WHY THIS CATEGORY EXISTS (#956)
# -------------------------------
# The zoo had ZERO governance negatives. Given the project's history in this
# exact area — #914 (InvalidPrevGovActionId silently dropped where canonical
# Gov.hs does `failBecause`), #922/#949/#950 (frozen-pulser and DRep-distr
# divergences), #951 (drep_voting_thresholds encoded in the wrong order, i.e.
# the values that decide whether actions pass) — ratification is a proven bug
# habitat that nothing was probing from the reject side.
#
# EVERY expectation here is oracle-verified against IntersectMBO/cardano-ledger
# `Conway.Rules.Gov` / `Governance.Internal`, because three plausible-looking
# negatives turn out NOT to be rejections at all:
#
#   * Two competing proposals in the same lane are ACCEPTED. Each governance
#     purpose is a tree (PRoot/PGraph); sibling insertion under one parent is
#     deliberate CIP-1694 design, and losing siblings are pruned at enactment.
#   * A DRep voting on NoConfidence is LEGAL. DReps are never disallowed by
#     action type — only SPOs and CC members are.
#   * A TreasuryWithdrawal exceeding the treasury is NOT rejected at
#     submission. It is a silent per-pass soft-fail at RATIFY
#     (`withdrawalCanWithdraw`), retried each epoch until it expires.
#
# Writing any of those as a negative would have produced a test that fails
# against a correct node. They are recorded here so nobody adds them later.

# expect_gov_rejection <name> <signed-tx> <expected-constructor> [<human note>]
#
# Submits and requires rejection naming the given Conway predicate-failure
# constructor. Three outcomes, all distinct:
#   accepted            -> FAIL (dugite too lax)
#   rejected, named     -> PASS
#   rejected, different -> FAIL (reject-reason divergence, P2)
#
# The third case matters: "both reject" is a weaker predicate than it looks,
# and a client cannot act on the wrong reason.
expect_gov_rejection() {
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
    local got
    got=$(echo "$out" | grep -oE '\(Conway[A-Za-z]*Failure[^)]*' | head -1 | tr -d ',' | cut -c1-110)
    [ -z "$got" ] && got=$(echo "$out" | grep -m1 Error | tr -d ',' | cut -c1-110)
    zoo_fail "$name: rejected, but as '${got}' not $want"
    zoo_record "$name" FAIL "" "rejected-wrong-reason-want-$want"
    return 1
}

# Build a proposal tx WITHOUT submitting. Some negatives are refused by
# `transaction build` client-side; callers decide whether that counts.
# Prints the signed file path on success.
gov_build_signed() {   # gov_build_signed <name> <addr> <skey> <build args...>
    local name="$1" addr="$2" skey="$3"; shift 3
    local utxo raw signed
    utxo=$(zoo_largest_utxo "$addr") || return 1
    raw="$ZOO_BUILT/$name.raw"; signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${utxo%% *}" --change-address "$addr" \
        "$@" --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$name.err" || return 1
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$skey" \
        --out-file "$signed" >/dev/null || return 1
    echo "$signed"
}
