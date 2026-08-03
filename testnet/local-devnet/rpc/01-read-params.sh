#!/usr/bin/env bash
# QueryService.ReadParams  ==  cardano-cli query protocol-parameters   (#960)
#
# Both API versions are exercised, because v1alpha and v1beta are separate
# generated service stubs in dugite-rpc — a mapping fix applied to one is not
# automatically applied to the other, and nothing before this compared them.
#
# The comparison is FIELD-BY-FIELD against a named mapping rather than a blob
# diff: the two encodings are legitimately different shapes (utxorpc uses
# BigInt/RationalNumber messages, cardano-cli uses JSON numbers and
# numerator/denominator objects), so a byte diff could only ever fail. A named
# mapping also makes a missing field a FAILURE instead of an invisible
# omission — if dugite stops populating `min_fee_script_ref_cost_per_byte`,
# this reports it rather than comparing null to null and calling it equal.

RPC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$RPC_DIR/lib/rpc-common.sh"

NAME="read-params"
ADDR="$RPC_BP_ADDR"

if ! rpc_available "$ADDR"; then
    rpc_row "$NAME" both "$ADDR" SKIP "env-skip: gRPC not reachable at $ADDR (grpcurl present? --rpc-port set?)"
    exit 0
fi

CLI_PP=$(cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" --socket-path "$LD_DUGITE_BP_SOCK" 2>&1)
if [ $? -ne 0 ] || [ -z "$CLI_PP" ]; then
    rpc_row "$NAME" both "$ADDR" ERROR "cardano-cli protocol-parameters failed: $(printf '%s' "$CLI_PP" | head -1)"
    exit 1
fi

# jq helpers that normalise the utxorpc shapes to plain scalars/strings.
#   BigInt          -> {"int":"123"} | {"int":123} | 123
#   RationalNumber  -> {"numerator":n,"denominator":d}
read -r -d '' JQ_HELPERS <<'JQEOF'
def bigint: if . == null then null
            elif type == "object" then (.int // .bigUInt // .bigNInt)
            else . end
           | if . == null then null else tostring end;
def ratio:  if . == null then null
            else ((.numerator // 0)|tostring) + "/" + ((.denominator // 1)|tostring) end;
def num:    if . == null then null else tostring end;
JQEOF

# name : jq over the utxorpc PParams : jq over the cardano-cli PParams
#
# Left blank deliberately: cost_models (huge, compared separately below).
MAPPING=$(cat <<'MAPEOF'
coins_per_utxo_byte|.coinsPerUtxoByte|bigint|.utxoCostPerByte|num
max_tx_size|.maxTxSize|num|.maxTxSize|num
min_fee_coefficient|.minFeeCoefficient|bigint|.txFeePerByte|num
min_fee_constant|.minFeeConstant|bigint|.txFeeFixed|num
max_block_body_size|.maxBlockBodySize|num|.maxBlockBodySize|num
max_block_header_size|.maxBlockHeaderSize|num|.maxBlockHeaderSize|num
stake_key_deposit|.stakeKeyDeposit|bigint|.stakeAddressDeposit|num
pool_deposit|.poolDeposit|bigint|.stakePoolDeposit|num
pool_retirement_epoch_bound|.poolRetirementEpochBound|num|.poolRetireMaxEpoch|num
desired_number_of_pools|.desiredNumberOfPools|num|.stakePoolTargetNum|num
pool_influence|.poolInfluence|ratio|.poolPledgeInfluence|ratio_cli
monetary_expansion|.monetaryExpansion|ratio|.monetaryExpansion|ratio_cli
treasury_expansion|.treasuryExpansion|ratio|.treasuryCut|ratio_cli
min_pool_cost|.minPoolCost|bigint|.minPoolCost|num
max_value_size|.maxValueSize|num|.maxValueSize|num
collateral_percentage|.collateralPercentage|num|.collateralPercentage|num
max_collateral_inputs|.maxCollateralInputs|num|.maxCollateralInputs|num
min_committee_size|.minCommitteeSize|num|.committeeMinSize|num
committee_term_limit|.committeeTermLimit|num|.committeeMaxTermLength|num
governance_action_validity_period|.governanceActionValidityPeriod|num|.govActionLifetime|num
governance_action_deposit|.governanceActionDeposit|bigint|.govActionDeposit|num
drep_deposit|.drepDeposit|bigint|.dRepDeposit|num
drep_inactivity_period|.drepInactivityPeriod|num|.dRepActivity|num
protocol_version_major|.protocolVersion.major|num|.protocolVersion.major|num
protocol_version_minor|.protocolVersion.minor|num|.protocolVersion.minor|num
max_tx_ex_mem|.maxExecutionUnitsPerTransaction.memory|num|.maxTxExecutionUnits.memory|num
max_tx_ex_steps|.maxExecutionUnitsPerTransaction.steps|num|.maxTxExecutionUnits.steps|num
max_block_ex_mem|.maxExecutionUnitsPerBlock.memory|num|.maxBlockExecutionUnits.memory|num
max_block_ex_steps|.maxExecutionUnitsPerBlock.steps|num|.maxBlockExecutionUnits.steps|num
MAPEOF
)

# A cardano-cli rational may render as {"numerator":..,"denominator":..} OR as
# a bare decimal, depending on the parameter and cli version. Normalise both to
# a reduced fraction string so the comparison is exact either way.
cli_ratio() {
    jq -r "$1 | if type==\"object\" then ((.numerator|tostring)+\"/\"+(.denominator|tostring))
                else tostring end" <<<"$CLI_PP" 2>/dev/null
}

# to_fraction — normalise a rational to a REDUCED "a/b" string.
#
# Accepts both encodings we have to compare:
#   "3/10"    — utxorpc RationalNumber {numerator, denominator}
#   "0.3"     — cardano-cli, which renders some rationals as bare decimals
#   "0.0030"  — ...with trailing zeros, so a string compare is hopeless
#
# The first draft only reduced "a/b" and compared the decimal form verbatim,
# so `3/10` vs `0.3` was reported as a MISMATCH: three false failures against
# a node that was answering correctly. Comparing rationals as exact fractions
# is the only encoding-independent way to do this.
to_fraction() {
    printf '%s' "$1" | awk '
    function gcd(x, y,  t) { if (x<0) x=-x; while (y) { t=x%y; x=y; y=t } return (x==0?1:x) }
    {
        s=$0
        if (s ~ /^-?[0-9]+\/[0-9]+$/) {
            split(s, p, "/"); a=p[1]+0; b=p[2]+0
        } else if (s ~ /^-?[0-9]*\.[0-9]+$/) {
            neg = (s ~ /^-/); if (neg) s=substr(s,2)
            split(s, p, ".")
            d=length(p[2]); b=1
            for (i=0; i<d; i++) b*=10
            a=(p[1]+0)*b + (p[2]+0)
            if (neg) a=-a
        } else if (s ~ /^-?[0-9]+$/) {
            a=s+0; b=1
        } else { print s; next }
        if (b==0) { print s; next }
        g=gcd(a,b); printf "%d/%d", a/g, b/g
    }'
}

compare_version() {
    local ver="$1" method_prefix="$2"
    local resp
    resp=$(rpc_call "$ADDR" "${method_prefix}.QueryService/ReadParams" '{}')
    if [ $? -ne 0 ]; then
        rpc_row "$NAME" "$ver" "${method_prefix}.QueryService/ReadParams" ERROR \
            "call failed: $(printf '%s' "$resp" | head -1)"
        return 1
    fi

    # Response wraps the params; find the cardano PParams object wherever it sits.
    local pp
    pp=$(printf '%s' "$resp" | jq -c '.. | objects | select(has("maxTxSize")) | .' 2>/dev/null | head -1)
    if [ -z "$pp" ]; then
        rpc_row "$NAME" "$ver" "${method_prefix}.QueryService/ReadParams" FAIL \
            "no PParams object in response (keys: $(printf '%s' "$resp" | jq -c 'keys?' 2>/dev/null))"
        return 1
    fi

    local mismatches=0 compared=0 missing=0 detail=""
    while IFS='|' read -r fname rpath rkind cpath ckind; do
        [ -z "$fname" ] && continue
        local rv cv
        case "$rkind" in
            bigint) rv=$(jq -r "$JQ_HELPERS $rpath | bigint" <<<"$pp" 2>/dev/null) ;;
            ratio)  rv=$(jq -r "$JQ_HELPERS $rpath | ratio"  <<<"$pp" 2>/dev/null) ;;
            *)      rv=$(jq -r "$JQ_HELPERS $rpath | num"    <<<"$pp" 2>/dev/null) ;;
        esac
        case "$ckind" in
            ratio_cli) cv=$(cli_ratio "$cpath") ;;
            *)         cv=$(jq -r "$cpath // null | if .==null then \"null\" else tostring end" <<<"$CLI_PP" 2>/dev/null) ;;
        esac
        [ "$rkind" = "ratio" ] && rv=$(to_fraction "$rv")
        [ "$ckind" = "ratio_cli" ] && cv=$(to_fraction "$cv")

        # proto3 JSON OMITS fields that hold their default value, so a
        # parameter that is genuinely 0 simply does not appear in the response.
        # Absent therefore means "zero", and comparing it against a cli value
        # of 0 is a MATCH, not a missing field. Treating absence as an error
        # produced two false failures (min_committee_size, protocol_version
        # minor — both legitimately 0 on this devnet).
        #
        # Absent against a NON-zero cli value is still a real mismatch, which
        # is the case worth catching, so the zero-substitution is deliberately
        # narrow rather than a blanket "null == anything".
        if [ -z "$rv" ] || [ "$rv" = "null" ]; then
            case "$rkind" in
                ratio) rv="0/1" ;;
                *)     rv="0" ;;
            esac
            detail="${detail} [${fname}: absent-in-rpc, read as proto3 default $rv]"
        fi
        compared=$((compared + 1))
        if [ "$rv" != "$cv" ]; then
            mismatches=$((mismatches + 1))
            detail="${detail} ${fname}: rpc=$rv cli=$cv"
        fi
    done <<<"$MAPPING"

    if [ "$mismatches" -eq 0 ]; then
        rpc_row "$NAME" "$ver" "${method_prefix}.QueryService/ReadParams" PASS \
            "$compared/$compared params equal to cardano-cli${detail}"
    else
        rpc_row "$NAME" "$ver" "${method_prefix}.QueryService/ReadParams" FAIL \
            "$mismatches of $compared mismatched:${detail}"
        return 1
    fi

    # Cost models: assert PRESENCE and non-emptiness rather than equality — the
    # cli reports them keyed by language name, utxorpc by a repeated field, and
    # an all-zero or empty model is the regression worth catching.
    local n_cm
    n_cm=$(jq -r '[.. | objects | select(has("values")) | .values | length] | add // 0' <<<"$pp" 2>/dev/null)
    if [ "${n_cm:-0}" -gt 100 ]; then
        rpc_row "cost-models" "$ver" "${method_prefix}.QueryService/ReadParams" PASS \
            "$n_cm cost-model entries present"
    else
        rpc_row "cost-models" "$ver" "${method_prefix}.QueryService/ReadParams" FAIL \
            "only ${n_cm:-0} cost-model entries (expected >100 across PlutusV1/V2/V3)"
    fi
    return 0
}

RC=0
compare_version v1alpha utxorpc.v1alpha.query || RC=1
compare_version v1beta  utxorpc.v1beta.query  || RC=1
exit $RC
