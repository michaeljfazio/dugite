#!/usr/bin/env bash
# Shared helper for 17-context-inspecting (#969).
#
# Every other Plutus category in the zoo drives a validator that returns a
# constant. Those prove dugite BUILDS a ScriptContext, resolves the right
# redeemer pointer, charges a budget and reaches cardano-node's verdict — but
# never that the context's CONTENTS are right. A wrong field value, a wrong
# list order or a missing entry passes all of them.
#
# The scripts here read the context and fail if it does not describe the
# transaction that is actually being validated. They are IntersectMBO's own
# plutus-tx output (see lib/build-plutus.sh), so they encode upstream's view of
# the ScriptContext rather than a third-party compiler's — the circularity #772
# and #970 both call out.
#
# What each one actually checks (cardano-ledger
# libs/plutus-preprocessor/.../Source/V3.hs):
#
#   purposeIsWellformedNoDatum    per purpose, the ScriptInfo's subject must
#                                 appear in the matching TxInfo field:
#                                   MintingScript cs    -> cs in mintValueMinted
#                                   RewardingScript c   -> c  in txInfoWdrl
#                                   CertifyingScript.. -> cert in txInfoTxCerts
#                                   VotingScript voter  -> voter in txInfoVotes
#   purposeIsWellformedWithDatum  the spent TxOutRef must appear in
#                                 txInfoInputs
#   datumIsWellformed             the datum must appear in txInfoData
#   inputsOutputsAreNotEmpty*     txInfoInputs and txInfoOutputs non-empty
#   redeemerSameAsDatum           redeemer == datum (drives a DELIBERATE
#                                 phase-2 failure when they differ)
set -euo pipefail

CTX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$CTX_DIR/../03-plutus/_lock-helper.sh"

# Budget for a real plutus-tx program — see 03j/03l, where the trivial
# always-true budget silently changed what the test measured.
CTX_EXUNITS="(2000000000,20000000)"

ctx_script() {
    printf '%s/lib/plutus/upstream-%s-v3.plutus' "$ZOO_DIR" "$1"
}

# Spend a UTxO locked at a context-inspecting script.
#
# ctx_spend <script-basename> <datum-mode> <redeemer-json> <expect: accept|reject>
#
# `cardano-cli transaction build` EVALUATES the script locally before it will
# emit a body, so a validator that rejects the context fails at build time and
# never reaches a node. That is a legitimate outcome for the negative cases —
# it is still the script refusing the context — but it must be distinguished
# from a node-side rejection, so both are reported.
ctx_spend() {
    local script_name="$1" datum_mode="$2" redeemer_json="$3" expect="$4"
    local name="$5"
    local script; script="$(ctx_script "$script_name")"
    [ -s "$script" ] || { zoo_record "$name" FAIL "" "missing-script=$script_name"; return 1; }

    local pair; pair=$(plutus_lock "$script" "$datum_mode" 5000000) || {
        zoo_record "$name" FAIL "" "lock"; return 1; }
    local txin=${pair%% *}

    local collat_pair; collat_pair=$(plutus_collateral_pair) || {
        zoo_record "$name" FAIL "" "collat"; return 1; }
    local collat=${collat_pair%% *}

    local redeemer="$ZOO_BUILT/$name.redeemer.json"
    printf '%s' "$redeemer_json" > "$redeemer"
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local raw="$ZOO_BUILT/$name.raw" signed="$ZOO_BUILT/$name.signed"

    local datum_arg=()
    case "$datum_mode" in
        inline) datum_arg=(--tx-in-inline-datum-present) ;;
        hash)   datum_arg=(--tx-in-datum-file "$ZOO_BUILT/$(basename "$script" .plutus).datum.json") ;;
        none)   datum_arg=() ;;
    esac

    if ! cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-in "$txin" \
            --tx-in-script-file "$script" \
            "${datum_arg[@]}" \
            --tx-in-redeemer-file "$redeemer" \
            --tx-in-collateral "$collat" \
            --tx-out "${addr}+2000000" \
            --change-address "$addr" \
            --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$name.err"; then
        if [ "$expect" = reject ]; then
            # The script refused the context. Confirm it is a script
            # evaluation error and not, say, a missing UTxO — otherwise this
            # "negative test" would pass on any build failure at all.
            if grep -qi 'script evaluation error\|The machine terminated' "$ZOO_LOGS/$name.err"; then
                zoo_record "$name" PASS "" "phase2-rejected-at-build (script refused the context)"
                return 0
            fi
            zoo_fail "build failed for a NON-script reason: $(tail -2 "$ZOO_LOGS/$name.err")"
            zoo_record "$name" FAIL "" "build-failed-not-script"
            return 1
        fi
        zoo_fail "build: $(tail -3 "$ZOO_LOGS/$name.err")"
        zoo_record "$name" FAIL "" "build"
        return 1
    fi

    if [ "$expect" = reject ]; then
        zoo_fail "expected the script to REFUSE this context, but the tx built cleanly"
        zoo_record "$name" FAIL "" "expected-phase2-failure"
        return 1
    fi

    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$signed" >/dev/null
    local txid; txid=$(zoo_submit "$signed") || { zoo_record "$name" FAIL "" "submit"; return 1; }
    if zoo_wait_all_observers "$txid" 120 "$addr"; then
        zoo_record "$name" PASS "$txid" "context accepted by $script_name"
        return 0
    fi
    zoo_record "$name" FAIL "$txid" "not-included"
    return 1
}
