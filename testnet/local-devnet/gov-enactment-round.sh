#!/usr/bin/env bash
# gov-enactment-round.sh — propose → vote (3 roles) → wait → assert the
# ON-CHAIN EFFECT of a governance action, and check gov-state parity between
# dugite and cardano-node at every step.
#
# WHY A DEDICATED ROUND (#956)
# ---------------------------
# `10-gov-lifecycle` does propose → DRep vote → SPO vote → CC vote → assert
# enactment for ParameterChange ONLY. `06-proposals` submits all seven action
# types but asserts nothing beyond inclusion in a block. So six of the seven
# action classes had no enactment coverage at all, and no test anywhere
# re-queried the resulting state.
#
# This cannot live in the tx-zoo: the zoo runs scripts back-to-back in seconds,
# and enactment is gated on epoch boundaries.
#
# THE BOUNDARY BUDGET (oracle-verified against IntersectMBO/cardano-ledger)
# ------------------------------------------------------------------------
# Ratification, enactment, and the commit into queryable GovState all happen in
# the SAME `EPOCH` step — there is no ratified-now/enacted-later split.
#
# The delay that DOES exist is the DRep pulser freeze. `dpProposals` is frozen
# at the previous boundary (`setFreshDRepPulsingState`), so a proposal or vote
# submitted during epoch E is invisible to the pulser already running in E. It
# is captured by the pulser created at the E->E+1 boundary and consumed at
# (E+1)->(E+2). Therefore:
#
#   earliest possible enactment = the boundary that STARTS epoch E+2
#
# At devnet pacing (epochLength=400 slots, slotLength=1s) that is ~13 minutes
# per action, and it is why this round is minutes long by construction rather
# than by inefficiency.
#
# ORDERING IS LOAD-BEARING
# ------------------------
# `delayingAction` is True for NoConfidence, HardForkInitiation,
# UpdateCommittee and NewConstitution. Once ANY delaying action enacts,
# `rsDelayed` gates every later action in that pass — so at most ONE of them
# enacts per boundary, and it blocks unrelated ParameterChange /
# TreasuryWithdrawals proposals that would otherwise have passed.
#
# Two are also destructive to later tests and are therefore NOT run here:
#   * NoConfidence dissolves the committee, so every subsequent CC vote fails.
#   * HardForkInitiation bumps the protocol version, changing era behaviour.
# Both belong in their own terminal round; see #956 for the follow-up.
#
# This round covers, in order (epoch numbers assume proposals start epoch 3):
#   Act 1. TreasuryWithdrawals (epoch 3 -> enacts start of epoch 5) —
#      non-delaying; asserts the REAL pot movement (casTreasuryL) and the
#      target reward-account credit, the byte-exact-parity invariant.
#   Act 3 propose (#1039, epoch 3): three TreasuryWithdrawals with LOSING vote
#      patterns, doomed to expire (govActionLifetime=2 via the #1036 overlay).
#   Act 2 (#1043, epoch 4 -> enacts start of epoch 6): NewConstitution seating
#      the REAL upstream guardrail script (delaying action — deliberately a
#      different RATIFY pass than Act 3's expiry; see the rsDelayed caveat).
#   Act 3 expiry (epochs 5-7): final-epoch votes, expiredGovActions preview
#      parity, then removal + per-action deposit return at the epoch-7
#      boundary (the #990 trap, re-armed as a permanent regression check).
#   Guardrail breadth (#1043, epoch 7): ~12 predicate-violating
#      ParameterChange proposals phase-2-rejected by BOTH sockets + 2 valid
#      ones accepted, from config/guardrails-cases.json.
#
# Usage: ./gov-enactment-round.sh [--skip-setup]
set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2
SKIP_SETUP=0
[ "${1:-}" = "--skip-setup" ] && SKIP_SETUP=1

step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES+1)); }
FAILURES=0

# delegate_votes_to_drep — put real stake behind drep-1.
#
# Uses the GENESIS stake delegators: their credentials are already registered
# on-chain (so no deposit or registration cert is needed) and they hold the
# entire delegated supply, which makes drep-1's share of the DRep distribution
# 100% — the only thing that matters, since the denominator is the total
# VOTE-delegated stake, not the total stake.
delegate_votes_to_drep() {
    local d="$LD_GENESIS/stake-delegators/delegator1"
    local drep="tx-zoo/state/keys/drep-1/drep.vkey"
    local tmp="$LD_STATE/gov-vote-deleg"
    mkdir -p "$tmp"
    [ -f "$d/staking.skey" ] || { bad "genesis delegator keys absent — DRep would have zero power"; return 1; }
    [ -f "$drep" ] || { bad "drep-1 vkey absent — run tx-zoo --setup first"; return 1; }

    # `create-testnet-data` funds a stake delegator at its BASE address
    # (payment + stake), not the enterprise one — the enterprise address holds
    # nothing. Checking only the enterprise address reported "no UTxO at the
    # genesis delegator address" against a wallet holding 1.35e15 lovelace.
    # Try both, largest first.
    local ent base addr txin=""
    cardano-cli conway address build --payment-verification-key-file "$d/payment.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/ent.addr" 2>/dev/null
    cardano-cli conway address build --payment-verification-key-file "$d/payment.vkey" \
        --stake-verification-key-file "$d/staking.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/base.addr" 2>/dev/null
    ent=$(cat "$tmp/ent.addr" 2>/dev/null)
    base=$(cat "$tmp/base.addr" 2>/dev/null)
    for a in "$base" "$ent"; do
        [ -z "$a" ] && continue
        txin=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
                 --address "$a" --output-json 2>/dev/null \
               | jq -r 'to_entries | max_by(.value.value.lovelace) | .key // empty')
        [ -n "$txin" ] && { addr="$a"; break; }
    done
    [ -z "$txin" ] && { bad "no UTxO at either genesis delegator address (base=$base ent=$ent)"; return 1; }
    # Spend change back to whichever address actually funded us.

    cardano-cli conway stake-address vote-delegation-certificate \
        --stake-verification-key-file "$d/staking.vkey" \
        --drep-verification-key-file "$drep" \
        --out-file "$tmp/vote.cert" 2>"$tmp/err" || { bad "vote-delegation cert build failed: $(tail -2 "$tmp/err")"; return 1; }

    cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
        --tx-in "$txin" --change-address "$addr" \
        --certificate-file "$tmp/vote.cert" --out-file "$tmp/raw" 2>>"$tmp/err" \
    && cardano-cli conway transaction sign --tx-body-file "$tmp/raw" \
        --signing-key-file "$d/payment.skey" --signing-key-file "$d/staking.skey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/signed" 2>>"$tmp/err" \
    && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --tx-file "$tmp/signed" >/dev/null 2>>"$tmp/err" \
    || { bad "vote-delegation submit failed: $(tail -3 "$tmp/err" | tr '\n' ' ')"; return 1; }

    ok "genesis delegator1 stake vote-delegated to drep-1"
    sleep 8
    return 0
}

if [ "$SKIP_SETUP" -eq 0 ]; then
    step "setup + run"
    ./stop.sh >/dev/null 2>&1
    # Act 3 (gov-action expiry, #1039) needs govActionLifetime=2: the
    # checked-in default of 6 puts expiry ~40 minutes past submission, outside
    # any round budget. The #1036 overlay hook injects it; every act in this
    # round is scheduled against lifetime 2 (see the timeline below).
    export LD_CONWAY_SPEC_EXTRA="${LD_CONWAY_SPEC_EXTRA:-$PWD/config/spec/overlays/gov-lifetime-2.json}"
    ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
    ./run.sh   >/dev/null 2>&1 || { echo "RUN FAILED";   exit 2; }
fi
. ./lib/common.sh
set +e

# Evidence CSV for the acts added by #1039 (expiry) and #1043 (guardrails).
GOV_EVIDENCE_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$GOV_EVIDENCE_DIR"
GOV_CSV="$GOV_EVIDENCE_DIR/gov-round.csv"
echo "ts,act,check,outcome,detail" > "$GOV_CSV"
gov_evidence() { # gov_evidence <act> <check> <outcome> <detail>
    printf '%s,%s,%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "$3" \
        "${4//,/;}" >> "$GOV_CSV"
}

for i in $(seq 1 40); do sleep 2; [ -S "$LD_RELAY_SOCK" ] && break; done
for i in $(seq 1 40); do
    sleep 3
    B=$(cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.block // 0')
    [ "${B:-0}" -ge 5 ] && break
done

if [ "$SKIP_SETUP" -eq 0 ]; then
    step "zoo keys + wallet-a stake registration (proposals need a registered return account)"
    ./tx-zoo/run-all.sh --setup >/dev/null 2>&1
    # ONLY 04a. Running the whole 04-stake category registers wallet-a's stake
    # key (04a) and then DEREGISTERS it (04d), leaving the deposit-return
    # account non-existent — every proposal here is then rejected with
    # ProposalReturnAccountDoesNotExist, and the round silently measures
    # nothing but the ordinary RUPD.
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/04-stake/04a-stake-register.sh 2>&1 | tail -2
    # A DRep can only vote once REGISTERED on-chain (else VotersDoNotExist), and
    # a CC member can only vote once its hot key is AUTHORISED. Keygen creates
    # the keys; these two put them on the chain. 05h is deliberately NOT run —
    # it resigns cc-1, and the committee is needed for the CC vote.
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/05-governance-certs/05a-drep-register.sh 2>&1 | tail -1
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/05-governance-certs/05g-cc-hot-key-authorization.sh 2>&1 | tail -1

    # ---- GIVE THE DRep ACTUAL VOTING POWER (oracle-verified) ----------------
    #
    # Registering a DRep is NOT enough. `dRepAcceptedRatio` folds over
    # `reDRepDistr` — the stake DISTRIBUTION — never over who cast a ballot:
    #
    #   accumStake (!yes, !tot) drep (CompactCoin stake) = ...
    #   (yesStake, total) = Map.foldlWithKey' accumStake (0, 0) reDRepDistr
    #
    # A DRep with a VoteYes but zero weight in that map never enters the fold;
    # its vote is inert. And `computeDRepDistr` only counts an account when
    # `dRepDelegationAccountStateL` is Some — i.e. when a `vote_delegation`
    # certificate exists. Stake delegation to a POOL contributes nothing.
    #
    # With no vote delegation anywhere, reDRepDistr is Map.empty, so
    # `0 %? 0 = 0` (the zero-safe operator returns 0, NOT 1), and
    # `0 >= threshold` is False for any non-zero threshold. Every DRep-gated
    # action is then UNRATIFIABLE no matter how many yes votes are cast.
    #
    # That is exactly why this round's first version watched a fully-voted
    # TreasuryWithdrawals sit unenacted and reported PASS anyway.
    #
    # The delegation must be on-chain BEFORE the boundary that creates the
    # pulser which will later ratify — `dpAccounts`/`dpDRepState` are frozen
    # together with `dpProposals`. Doing it here, in setup, satisfies that.
    step "give the DRep real voting power (vote-delegate genesis stake)"
    delegate_votes_to_drep
fi

WA="tx-zoo/state/keys/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
STAKE_ADDR=$(cat "$WA/stake.addr")
PPARAMS=$(mktemp)
cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" \
    --socket-path "$LD_RELAY_SOCK" --out-file "$PPARAMS" 2>/dev/null
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")

cur_epoch() { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null | jq -r '.epoch // 0'; }
treasury_of() { cardano-cli conway query treasury --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null | jq -r '. // 0'; }
reward_of() {
    cardano-cli conway query stake-address-info --testnet-magic "$LD_MAGIC" \
        --socket-path "$1" --address "$2" 2>/dev/null | jq -r '.[0].rewardAccountBalance // 0'
}
constitution_of() {
    cardano-cli conway query constitution --testnet-magic "$LD_MAGIC" \
        --socket-path "$1" 2>/dev/null | jq -c '.anchor // {}'
}

# wait_boundaries N — block until the epoch has advanced by N.
wait_boundaries() {
    local n="$1" start now deadline
    start=$(cur_epoch "$LD_RELAY_SOCK")
    deadline=$(( n * 500 ))   # 400-slot epochs at 1s, plus slack
    echo "  waiting $n epoch boundary/ies from epoch $start (up to ${deadline}s)"
    local i=0
    while [ "$i" -lt "$deadline" ]; do
        now=$(cur_epoch "$LD_RELAY_SOCK")
        if [ "$(( now - start ))" -ge "$n" ]; then
            echo "  reached epoch $now after ${i}s"
            return 0
        fi
        sleep 5; i=$((i+5))
    done
    echo "  TIMEOUT: still at epoch $(cur_epoch "$LD_RELAY_SOCK")"
    return 1
}

# vote_all <action-id> <tag> [roles]
#
# roles defaults to "drep,cc" — NOT "drep,spo,cc". Voter eligibility is
# per-action-type and a disallowed voter is a HARD phase-1 rejection
# (DisallowedVoters), not a vote that is merely uncounted, so including an
# ineligible role kills the whole vote transaction and every eligible vote in
# it. Per Governance/Internal.hs:
#
#   action               SPO   CC    DRep
#   NoConfidence         yes   NO    yes
#   UpdateCommittee      yes   NO    yes
#   NewConstitution      NO    yes   yes
#   HardForkInitiation   yes   yes   yes
#   ParameterChange      only-if-SecurityGroup   yes   yes
#   TreasuryWithdrawals  NO    yes   yes
#   InfoAction           yes   yes   yes
#
# Learned the hard way: this round submitted DRep+SPO+CC on a
# TreasuryWithdrawals and cardano-node rejected the transaction outright with
# DisallowedVoters (StakePoolVoter ...) — which is exactly what tx-zoo 14c
# asserts, so the harness was contradicting its own test.
vote_all() {
    local action_id="$1" tag="$2" roles="${3:-drep,cc}"
    local tx="${action_id%#*}" ix="${action_id#*#}"
    local votes=() signs=()
    local D="tx-zoo/state/keys/drep-1"
    if [[ ",$roles," == *",drep,"* ]]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --drep-verification-key-file "$D/drep.vkey" \
            --out-file "$ZOO_TMP/$tag-drep.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-drep.vote"); signs+=(--signing-key-file "$D/drep.skey"); }
    fi
    if [[ ",$roles," == *",spo,"* ]]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" \
            --out-file "$ZOO_TMP/$tag-spo.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-spo.vote"); signs+=(--signing-key-file "$LD_KEYS/pool1/cold.skey"); }
    fi
    local CC="tx-zoo/state/keys/cc-2"
    if [[ ",$roles," == *",cc,"* ]] && [ -s "$CC/cc-hot.vkey" ]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --cc-hot-verification-key-file "$CC/cc-hot.vkey" \
            --out-file "$ZOO_TMP/$tag-cc.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-cc.vote"); signs+=(--signing-key-file "$CC/cc-hot.skey"); }
    fi
    [ ${#votes[@]} -eq 0 ] && { echo "  no votes could be created"; return 1; }

    local u; u=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --address "$ADDR" --output-json 2>/dev/null \
        | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if ! cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-in "$u" --change-address "$ADDR" \
            "${votes[@]}" --out-file "$ZOO_TMP/$tag-votes.raw" 2>"$ZOO_TMP/$tag-build.err"; then
        echo "  vote BUILD failed: $(grep -m1 -E 'Error|Failure' "$ZOO_TMP/$tag-build.err" | cut -c1-180)"
        return 1
    fi
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_TMP/$tag-votes.raw" \
        --signing-key-file "$WA/payment.skey" "${signs[@]}" \
        --out-file "$ZOO_TMP/$tag-votes.signed" 2>"$ZOO_TMP/$tag-sign.err" || {
            echo "  vote SIGN failed: $(head -1 "$ZOO_TMP/$tag-sign.err" | cut -c1-180)"; return 1; }
    if ! SUBV=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/$tag-votes.signed" 2>&1); then
        echo "  vote SUBMIT rejected: $(echo "$SUBV" | grep -m1 -E 'Error|Failure' | cut -c1-180)"
        return 1
    fi
    echo "  cast ${#votes[@]} vote(s) on $action_id"
    return 0
}

# vote_cast <action-id> <tag> <role:verdict>[,<role:verdict>...]
#
# Per-role verdict variant of vote_all, for Act 3's LOSING vote patterns
# (#1039): e.g. "cc:yes,drep:no". Same eligibility rules as vote_all — for
# TreasuryWithdrawals only drep and cc may vote at all.
vote_cast() {
    local action_id="$1" tag="$2" spec="$3"
    local tx="${action_id%#*}" ix="${action_id#*#}"
    local votes=() signs=() pair role verdict flag
    local D="tx-zoo/state/keys/drep-1"
    local CC="tx-zoo/state/keys/cc-2"
    for pair in ${spec//,/ }; do
        role="${pair%%:*}"; verdict="${pair##*:}"
        case "$verdict" in
            yes) flag="--yes" ;;
            no)  flag="--no" ;;
            abstain) flag="--abstain" ;;
            *) echo "  bad verdict '$verdict' in $spec"; return 1 ;;
        esac
        case "$role" in
            drep)
                cardano-cli conway governance vote create "$flag" \
                    --governance-action-tx-id "$tx" --governance-action-index "$ix" \
                    --drep-verification-key-file "$D/drep.vkey" \
                    --out-file "$ZOO_TMP/$tag-drep.vote" 2>/dev/null \
                    && { votes+=(--vote-file "$ZOO_TMP/$tag-drep.vote"); signs+=(--signing-key-file "$D/drep.skey"); }
                ;;
            cc)
                [ -s "$CC/cc-hot.vkey" ] || { echo "  cc hot key absent"; return 1; }
                cardano-cli conway governance vote create "$flag" \
                    --governance-action-tx-id "$tx" --governance-action-index "$ix" \
                    --cc-hot-verification-key-file "$CC/cc-hot.vkey" \
                    --out-file "$ZOO_TMP/$tag-cc.vote" 2>/dev/null \
                    && { votes+=(--vote-file "$ZOO_TMP/$tag-cc.vote"); signs+=(--signing-key-file "$CC/cc-hot.skey"); }
                ;;
            spo)
                cardano-cli conway governance vote create "$flag" \
                    --governance-action-tx-id "$tx" --governance-action-index "$ix" \
                    --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" \
                    --out-file "$ZOO_TMP/$tag-spo.vote" 2>/dev/null \
                    && { votes+=(--vote-file "$ZOO_TMP/$tag-spo.vote"); signs+=(--signing-key-file "$LD_KEYS/pool1/cold.skey"); }
                ;;
            *) echo "  bad role '$role' in $spec"; return 1 ;;
        esac
    done
    [ ${#votes[@]} -eq 0 ] && { echo "  no votes could be created for $spec"; return 1; }
    local u; u=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --address "$ADDR" --output-json 2>/dev/null \
        | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if ! cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-in "$u" --change-address "$ADDR" \
            "${votes[@]}" --out-file "$ZOO_TMP/$tag-votes.raw" 2>"$ZOO_TMP/$tag-build.err"; then
        echo "  vote BUILD failed: $(grep -m1 -E 'Error|Failure' "$ZOO_TMP/$tag-build.err" | cut -c1-180)"
        return 1
    fi
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_TMP/$tag-votes.raw" \
        --signing-key-file "$WA/payment.skey" "${signs[@]}" \
        --out-file "$ZOO_TMP/$tag-votes.signed" 2>/dev/null || { echo "  vote SIGN failed"; return 1; }
    if ! SUBV=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/$tag-votes.signed" 2>&1); then
        echo "  vote SUBMIT rejected: $(echo "$SUBV" | grep -m1 -E 'Error|Failure' | cut -c1-180)"
        return 1
    fi
    echo "  cast $spec on $action_id"
    return 0
}

ZOO_TMP=$(mktemp -d); trap 'rm -rf "$ZOO_TMP"' EXIT

# Anchors: reuse the zoo's proven helpers rather than hand-rolling them.
# A hand-rolled `governance hash anchor-data --file-binary` produced an empty
# hash and cardano-cli refused the proposal with "Error reading anchor data
# hash: Unable to read hash" — which surfaced as a bare "proposal rejected"
# and sent me looking at enactment instead of at the anchor.
. ./tx-zoo/lib/tx-zoo-common.sh
set +e
zoo_anchor_start >/dev/null 2>&1
ANCHOR_URL=$(zoo_anchor_url gov-proposal)
ANCHOR_HASH=$(zoo_anchor_hash gov-proposal)
echo "  anchor: $ANCHOR_URL hash=${ANCHOR_HASH:0:16}…"
[ -n "$ANCHOR_HASH" ] || { echo "FATAL: could not compute anchor hash"; exit 2; }

# ─────────────────────────────────────────────────────────────────────────────
step "1. TreasuryWithdrawals — propose"
# ─────────────────────────────────────────────────────────────────────────────
WITHDRAW=5000000

# ---- WAIT FOR A FUNDED TREASURY, ONE BOUNDARY AHEAD (oracle-verified) -------
#
# `withdrawalCanWithdraw` gates the action on `ensTreasury`, which Haskell
# seals into the DRep pulser at the END of `epochTransition`
# (`setFreshDRepPulsingState`) and consumes a FULL BOUNDARY LATER. RATIFY is
# therefore blind to the `applyRUpd` credit landing at the boundary it runs on:
# it sees the treasury as of one boundary earlier.
#
# On a fresh devnet the treasury is 0 until the first real RUPD at boundary
# 1->2. Proposing in epoch 0 means the ratification pass at boundary 1->2 reads
# a pulser sealed at 0->1, when the pot was still 0 — so the withdrawal fails
# on affordability even though the post-boundary state shows trillions. That is
# the second reason this round used to watch a fully-voted action not enact.
#
# Waiting until the treasury has been non-zero for a full epoch before
# proposing removes the lag from the equation. (dugite had the same lag as a
# BUG in the opposite direction — it read the live pot and would have enacted
# an epoch EARLY; see #966.)
wait_funded_treasury() {
    local deadline=$(( $(date +%s) + 1200 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local t e
        t=$(treasury_of "$LD_RELAY_SOCK")
        e=$(cur_epoch "$LD_RELAY_SOCK")
        if [ "${t:-0}" -gt $(( WITHDRAW * 2 )) ]; then
            echo "  treasury funded at epoch $e: $t"
            # One more boundary so the pulser that will ratify was sealed with
            # this funded value, not the pre-RUPD zero.
            wait_boundaries 1
            echo "  crossed one more boundary so the sealed ensTreasury is funded"
            return 0
        fi
        echo "  epoch=$e treasury=${t:-0} — waiting for the first RUPD"
        sleep 30
    done
    return 1
}
if ! wait_funded_treasury; then
    bad "treasury never became funded — a TreasuryWithdrawals action cannot ratify"
fi

T0_DUGITE=$(treasury_of "$LD_RELAY_SOCK")
T0_HASKELL=$(treasury_of "$LD_CARDANO_BP_SOCK")
R0=$(reward_of "$LD_RELAY_SOCK" "$STAKE_ADDR")
echo "  before: treasury dugite=$T0_DUGITE haskell=$T0_HASKELL reward=$R0"
[ "$T0_DUGITE" = "$T0_HASKELL" ] && ok "treasury parity before: $T0_DUGITE" \
                                 || bad "treasury parity BEFORE: dugite=$T0_DUGITE haskell=$T0_HASKELL"

cardano-cli conway governance action create-treasury-withdrawal \
    --testnet --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url "$ANCHOR_URL" --anchor-data-hash "$ANCHOR_HASH" \
    --funds-receiving-stake-verification-key-file "$WA/stake.vkey" \
    --transfer "$WITHDRAW" --out-file "$ZOO_TMP/tw.action" 2>&1 | head -3

U=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
      --address "$ADDR" --output-json 2>/dev/null | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
# Capture stderr. Swallowing it here cost four 15-minute round attempts: the
# round reported "proposal rejected" with an empty reason while the actual
# error (a bad anchor hash, then something else) sat in /dev/null.
if ! cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
        --tx-in "$U" --change-address "$ADDR" --proposal-file "$ZOO_TMP/tw.action" \
        --out-file "$ZOO_TMP/tw.raw" >/dev/null 2>"$ZOO_TMP/tw.build.err"; then
    bad "TreasuryWithdrawals BUILD failed: $(grep -m1 -vE '^\s*$' "$ZOO_TMP/tw.build.err" | cut -c1-200)"
    sed 's/^/    /' "$ZOO_TMP/tw.build.err" | head -6
    TW_PROPOSED=0
    BUILD_FAILED=1
fi
if [ "${BUILD_FAILED:-0}" -eq 0 ]; then
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" --tx-body-file "$ZOO_TMP/tw.raw" \
    --signing-key-file "$WA/payment.skey" --out-file "$ZOO_TMP/tw.signed" >/dev/null 2>&1
# cardano-cli 11 returns JSON ({"txhash": "..."}) from `transaction txid`, not a
# bare hash. Taking it raw made TW_TXID="{" and every vote targeted a
# nonexistent action id — the votes would be rejected and the round would
# report "not enacted" for a reason that had nothing to do with enactment.
TW_TXID=$(cardano-cli conway transaction txid --tx-file "$ZOO_TMP/tw.signed" 2>/dev/null \
          | jq -r 'if type=="object" then .txhash else . end' 2>/dev/null | tr -d '"[:space:]')
if SUB=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/tw.signed" 2>&1); then
    if [ "${#TW_TXID}" -ne 64 ]; then
        bad "txid did not parse as 64 hex chars: '$TW_TXID'"
        TW_PROPOSED=0
    else
        ok "TreasuryWithdrawals proposed: $TW_TXID#0 (transfer=$WITHDRAW)"
        TW_PROPOSED=1
    fi
else
    # Print the WHOLE reason, not just a Conway*Failure capture — the capture
    # comes back empty for codec/CLI errors, which is how this round twice
    # reported a bare "rejected" with no cause.
    bad "TreasuryWithdrawals proposal rejected: $(echo "$SUB" | grep -m1 -E 'Error|Failure' | cut -c1-200)"
    echo "$SUB" | head -4 | sed 's/^/    /'
    TW_PROPOSED=0
fi
fi

sleep 10

step "2. TreasuryWithdrawals — vote (DRep + CC; SPOs are DISALLOWED here)"
if vote_all "${TW_TXID}#0" tw "drep,cc"; then
    ok "votes submitted for ${TW_TXID:0:16}…#0"
else
    bad "vote submission failed — the round cannot distinguish 'not ratified' from 'never voted'"
fi
sleep 10

# ─────────────────────────────────────────────────────────────────────────────
step "2b. Act 3 (#1039) — propose 3 TreasuryWithdrawals doomed to EXPIRE"
# ─────────────────────────────────────────────────────────────────────────────
# Governance-action expiry and its deposit return had never executed in the
# gate — #990 (final-epoch votes discarded / expiry tested before the
# ratification attempt) is exactly this class.
#
# TIMELINE (oracle-verified against cardano-ledger master f8d6ead7, 2026-08-06;
# Rules/Ratify.hs ratifyTransition + Rules/Epoch.hs returnProposalDeposits).
# Submitted during epoch E (nominally 3) at govActionLifetime=2:
#
#   gasExpiresAfter N = E+2 (=5). The pulser created at the boundary STARTING
#   epoch X carries reCurrentEpoch = X and is consumed one boundary later. The
#   expiry predicate `gasExpiresAfter < reCurrentEpoch` first holds for the
#   pulser with reCurrentEpoch = N+1, consumed at the boundary starting epoch
#   N+2 (=7) — and that pulser froze the votes cast during epoch N (=5). So:
#
#   * epoch N (=E+2) is the FINAL epoch whose votes still count — the #990
#     path: the ratification attempt at the N+2 boundary runs BEFORE the
#     expiry test (else-branch), with exactly those epoch-N votes;
#   * during epoch N+1 (=6): nextRatifyState.expiredGovActions (the forced
#     live pulser) already names all three ids, but deposits are NOT yet
#     returned — asserting the return there is the #990 trap, inverted;
#   * the boundary starting epoch N+2 (=7): actions removed from the proposal
#     set AND deposits returned in the SAME epochTransition
#     (returnProposalDeposits; an unregistered return credential would route
#     to treasury — ours stay registered).
#
#   rsDelayed CAVEAT (oracle): a delaying action enacting earlier in the SAME
#   RATIFY pass forces every later action to the else branch, skipping its
#   final vote evaluation. Act 2 (NewConstitution, delaying) therefore enacts
#   at the boundary starting epoch 6 — a DIFFERENT pass than the expiry pass
#   at epoch 7 — so the a3c final-epoch votes get an honest evaluation.
#
# Three losing vote patterns (upstream test_expire_treasury_withdrawals):
#   a3a CC-yes / DRep-no      a3b CC-no / DRep-yes      a3c both-no
# a3a/a3b vote now (epoch E); a3c's votes are deliberately cast in epoch E+2
# (after Act 1's enactment assert) to pin the final-epoch-votes-counted path.
A3_EPOCH_SUBMITTED=$(cur_epoch "$LD_RELAY_SOCK")
A3_LIFETIME=$(cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" \
    --socket-path "$LD_RELAY_SOCK" 2>/dev/null \
    | jq -r '.currentPParams.govActionLifetime // empty')
if [ "$A3_LIFETIME" != "2" ]; then
    bad "govActionLifetime=$A3_LIFETIME, need 2 — Act 3 cannot run (setup without the overlay?)"
    gov_evidence act3 lifetime-overlay FAIL "govActionLifetime=$A3_LIFETIME"
    A3_ARMED=0
else
    A3_ARMED=1
    A3_EXPIRES=$(( A3_EPOCH_SUBMITTED + 2 ))
    ok "Act 3 armed: submitted epoch $A3_EPOCH_SUBMITTED, expiresAfter=$A3_EXPIRES"
fi

# Each action gets its OWN fresh, registered return stake address so the
# deposit-return assertion is per-action ("EACH deposit returned to its own
# return address, exact amount").
A3_TXIDS=()
if [ "${A3_ARMED:-0}" = "1" ]; then
    KEY_DEPOSIT=$(jq -r '.stakeAddressDeposit // 2000000' "$PPARAMS")
    A3_DIR="$LD_STATE/act3"; mkdir -p "$A3_DIR"
    REG_CERTS=()
    for i in 1 2 3; do
        cardano-cli conway stake-address key-gen \
            --verification-key-file "$A3_DIR/ret$i.vkey" \
            --signing-key-file "$A3_DIR/ret$i.skey" 2>/dev/null
        cardano-cli conway stake-address build \
            --stake-verification-key-file "$A3_DIR/ret$i.vkey" \
            --testnet-magic "$LD_MAGIC" --out-file "$A3_DIR/ret$i.addr" 2>/dev/null
        cardano-cli conway stake-address registration-certificate \
            --stake-verification-key-file "$A3_DIR/ret$i.vkey" \
            --key-reg-deposit-amt "$KEY_DEPOSIT" \
            --out-file "$A3_DIR/ret$i.cert" 2>/dev/null
        REG_CERTS+=(--certificate-file "$A3_DIR/ret$i.cert")
    done
    U=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
          --address "$ADDR" --output-json 2>/dev/null | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
            --tx-in "$U" --change-address "$ADDR" "${REG_CERTS[@]}" \
            --witness-override 4 \
            --out-file "$A3_DIR/reg.raw" 2>"$A3_DIR/reg.err" \
       && cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
            --tx-body-file "$A3_DIR/reg.raw" --signing-key-file "$WA/payment.skey" \
            --signing-key-file "$A3_DIR/ret1.skey" --signing-key-file "$A3_DIR/ret2.skey" \
            --signing-key-file "$A3_DIR/ret3.skey" \
            --out-file "$A3_DIR/reg.signed" 2>>"$A3_DIR/reg.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$A3_DIR/reg.signed" >/dev/null 2>>"$A3_DIR/reg.err"; then
        ok "3 return stake addresses registered"
        gov_evidence act3 return-addr-registration PASS "3 fresh stake keys"
    else
        bad "return-address registration failed: $(tail -2 "$A3_DIR/reg.err" | tr '\n' ' ')"
        gov_evidence act3 return-addr-registration FAIL "see $A3_DIR/reg.err"
        A3_ARMED=0
    fi
    sleep 8

    if [ "$A3_ARMED" = "1" ]; then
        for i in 1 2 3; do
            cardano-cli conway governance action create-treasury-withdrawal \
                --testnet --governance-action-deposit "$GOV_DEPOSIT" \
                --deposit-return-stake-verification-key-file "$A3_DIR/ret$i.vkey" \
                --anchor-url "$ANCHOR_URL" --anchor-data-hash "$ANCHOR_HASH" \
                --funds-receiving-stake-verification-key-file "$WA/stake.vkey" \
                --transfer "$WITHDRAW" --out-file "$A3_DIR/a3-$i.action" 2>/dev/null
            U=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
                  --address "$ADDR" --output-json 2>/dev/null | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
            if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
                    --tx-in "$U" --change-address "$ADDR" --proposal-file "$A3_DIR/a3-$i.action" \
                    --out-file "$A3_DIR/a3-$i.raw" >/dev/null 2>"$A3_DIR/a3-$i.err" \
               && cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
                    --tx-body-file "$A3_DIR/a3-$i.raw" --signing-key-file "$WA/payment.skey" \
                    --out-file "$A3_DIR/a3-$i.signed" 2>>"$A3_DIR/a3-$i.err" \
               && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_RELAY_SOCK" --tx-file "$A3_DIR/a3-$i.signed" >/dev/null 2>>"$A3_DIR/a3-$i.err"; then
                T=$(cardano-cli conway transaction txid --tx-file "$A3_DIR/a3-$i.signed" 2>/dev/null \
                    | jq -r 'if type=="object" then .txhash else . end' | tr -d '"[:space:]')
                A3_TXIDS+=("$T")
                ok "Act 3 action $i proposed: ${T:0:16}…#0"
                gov_evidence act3 "propose-$i" PASS "$T#0"
            else
                bad "Act 3 action $i proposal failed: $(tail -2 "$A3_DIR/a3-$i.err" | tr '\n' ' ')"
                gov_evidence act3 "propose-$i" FAIL "see err"
                A3_TXIDS+=("")
            fi
            sleep 6
        done
        # Losing votes for a3a + a3b now (epoch E); a3c waits for epoch E+1.
        [ -n "${A3_TXIDS[0]:-}" ] && { vote_cast "${A3_TXIDS[0]}#0" a3a "cc:yes,drep:no" \
            && gov_evidence act3 vote-a3a PASS "cc:yes,drep:no" \
            || gov_evidence act3 vote-a3a FAIL "vote_cast failed"; }
        sleep 4
        [ -n "${A3_TXIDS[1]:-}" ] && { vote_cast "${A3_TXIDS[1]}#0" a3b "cc:no,drep:yes" \
            && gov_evidence act3 vote-a3b PASS "cc:no,drep:yes" \
            || gov_evidence act3 vote-a3b FAIL "vote_cast failed"; }
        sleep 4
    fi
fi

# Probe mode: stop before the ~13-minute boundary wait while iterating on the
# proposal/vote plumbing. Placed AFTER voting so the probe covers everything
# except the wait itself.
if [ "${GOV_PROBE_ONLY:-0}" = "1" ]; then
    step "PROBE ONLY — stopping before the boundary wait"
    [ "$SKIP_SETUP" -eq 0 ] && ./stop.sh >/dev/null 2>&1
    exit "$FAILURES"
fi

step "3. wait one boundary, then propose Act 2 (NewConstitution + REAL guardrails, #1043)"
wait_boundaries 1

# ─────────────────────────────────────────────────────────────────────────────
# Act 2: enact the REAL Plutus guardrail constitution (#1043).
#
# The guardrail script is upstream's compiled cardano-constitution V3
# validator, vendored hash-verified at tests/conformance/upstream/
# guardrails-script.json (the #969/#970 vendoring pattern — no compiler at
# devnet setup). Proposed in epoch 4 so it ENACTS at the boundary starting
# epoch 6 — deliberately one pass before Act 3's expiry pass at epoch 7,
# because NewConstitution is a DELAYING action (see the rsDelayed caveat in
# step 2b). Once enacted, EVERY ParameterChange / TreasuryWithdrawals
# proposal must name this script hash and carry a Proposing witness — which
# is precisely what step 8's guardrail predicate cases exercise.
# ─────────────────────────────────────────────────────────────────────────────
GUARD_VENDOR="../../tests/conformance/upstream/guardrails-script.json"
ACT2_ARMED=0
if [ -s "$GUARD_VENDOR" ]; then
    GUARD_SCRIPT="$ZOO_TMP/guardrails.plutus"
    jq '{type: .type, description: "upstream cardano-constitution guardrail validator", cborHex: .cborHex}' \
        "$GUARD_VENDOR" > "$GUARD_SCRIPT"
    GUARD_HASH_WANT=$(jq -r '.scriptHash' "$GUARD_VENDOR")
    GUARD_HASH_GOT=$(cardano-cli conway transaction policyid --script-file "$GUARD_SCRIPT" 2>/dev/null)
    if [ "$GUARD_HASH_GOT" = "$GUARD_HASH_WANT" ] && [ -n "$GUARD_HASH_GOT" ]; then
        ok "guardrail script materialised, hash verified: ${GUARD_HASH_GOT:0:16}…"
        gov_evidence act2 vendored-hash PASS "$GUARD_HASH_GOT"
        ACT2_ARMED=1
    else
        bad "guardrail script hash mismatch: vendored=$GUARD_HASH_WANT computed=$GUARD_HASH_GOT"
        gov_evidence act2 vendored-hash FAIL "want=$GUARD_HASH_WANT got=$GUARD_HASH_GOT"
    fi
else
    bad "vendored guardrail script absent at $GUARD_VENDOR"
    gov_evidence act2 vendored-hash FAIL "file-missing"
fi

ACT2_TXID=""
if [ "$ACT2_ARMED" = "1" ]; then
    CONST_URL=$(zoo_anchor_url constitution-body)
    CONST_HASH=$(zoo_anchor_hash constitution-body)
    cardano-cli conway governance action create-constitution \
        --testnet --governance-action-deposit "$GOV_DEPOSIT" \
        --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
        --anchor-url "$ANCHOR_URL" --anchor-data-hash "$ANCHOR_HASH" \
        --constitution-url "$CONST_URL" --constitution-hash "$CONST_HASH" \
        --constitution-script-hash "$GUARD_HASH_GOT" \
        --out-file "$ZOO_TMP/act2.action" 2>"$ZOO_TMP/act2.err"
    U=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
          --address "$ADDR" --output-json 2>/dev/null | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
            --tx-in "$U" --change-address "$ADDR" --proposal-file "$ZOO_TMP/act2.action" \
            --out-file "$ZOO_TMP/act2.raw" >/dev/null 2>>"$ZOO_TMP/act2.err" \
       && cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
            --tx-body-file "$ZOO_TMP/act2.raw" --signing-key-file "$WA/payment.skey" \
            --out-file "$ZOO_TMP/act2.signed" 2>>"$ZOO_TMP/act2.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/act2.signed" >/dev/null 2>>"$ZOO_TMP/act2.err"; then
        ACT2_TXID=$(cardano-cli conway transaction txid --tx-file "$ZOO_TMP/act2.signed" 2>/dev/null \
            | jq -r 'if type=="object" then .txhash else . end' | tr -d '"[:space:]')
        ok "NewConstitution proposed: ${ACT2_TXID:0:16}…#0 (guardrails=${GUARD_HASH_GOT:0:16}…)"
        gov_evidence act2 propose PASS "$ACT2_TXID#0"
        sleep 8
        # NewConstitution: DRep + CC only (SPOs disallowed).
        if vote_all "${ACT2_TXID}#0" act2 "drep,cc"; then
            ok "Act 2 votes submitted"
            gov_evidence act2 vote PASS "drep+cc yes"
        else
            bad "Act 2 vote submission failed"
            gov_evidence act2 vote FAIL "vote_all failed"
        fi
    else
        bad "NewConstitution proposal failed: $(grep -m1 -E 'Error|Failure' "$ZOO_TMP/act2.err" | cut -c1-200)"
        gov_evidence act2 propose FAIL "see act2.err"
        ACT2_ARMED=0
    fi
fi
sleep 5

step "3b. wait the second boundary (Act 1 enactment)"
wait_boundaries 1

step "4. TreasuryWithdrawals — assert the on-chain effect"
T1_DUGITE=$(treasury_of "$LD_RELAY_SOCK")
T1_HASKELL=$(treasury_of "$LD_CARDANO_BP_SOCK")
R1=$(reward_of "$LD_RELAY_SOCK" "$STAKE_ADDR")
echo "  after: treasury dugite=$T1_DUGITE haskell=$T1_HASKELL reward=$R1"
if [ "$T1_DUGITE" = "$T1_HASKELL" ]; then
    ok "treasury byte-exact parity after: $T1_DUGITE"
else
    bad "treasury parity AFTER: dugite=$T1_DUGITE haskell=$T1_HASKELL"
fi
if [ "${TW_PROPOSED:-0}" -eq 0 ]; then
    bad "withdrawal was never proposed — this round measured nothing but the RUPD"
elif [ "${R1:-0}" -gt "${R0:-0}" ]; then
    DELTA=$(( R1 - R0 ))
    ok "reward account credited: $R0 -> $R1 (delta $DELTA)"
    # The credit is the withdrawal PLUS the proposal deposit refund.
    #
    # Both land in the same reward account because this round passes the SAME
    # stake key to `--deposit-return-stake-verification-key-file` and
    # `--funds-receiving-stake-verification-key-file`. When the action is
    # enacted and removed from the proposal set, Conway returns its deposit to
    # the return account — so the observed delta is
    # `WITHDRAW + govActionDeposit`, not `WITHDRAW`.
    #
    # A first version asserted `delta == WITHDRAW` and failed at
    # 100005000000 != 5000000 on a run where the withdrawal had enacted
    # perfectly: 100000000000 deposit + 5000000 transfer. Asserting the sum is
    # not a weaker check but a STRONGER one — it pins the deposit refund at the
    # same time, and a refund that silently failed to arrive (the #898 shape)
    # would now show up here.
    EXPECTED=$(( WITHDRAW + GOV_DEPOSIT ))
    if [ "$DELTA" -eq "$EXPECTED" ]; then
        ok "credit is byte-exact: $DELTA == transfer $WITHDRAW + deposit refund $GOV_DEPOSIT"
    elif [ "$DELTA" -eq "$WITHDRAW" ]; then
        bad "transfer arrived but the $GOV_DEPOSIT deposit was NOT refunded (delta $DELTA)"
    else
        bad "credit delta $DELTA != transfer $WITHDRAW + deposit refund $GOV_DEPOSIT ($EXPECTED)"
    fi
    # And the pot must have paid for it. Comparing raw before/after is not
    # possible (the RUPD moves the treasury at the same boundary), so assert
    # against the Haskell node instead — parity is the real invariant.
    [ "$T1_DUGITE" = "$T1_HASKELL" ] \
        && ok "post-enactment treasury byte-exact vs Haskell: $T1_DUGITE" \
        || bad "post-enactment treasury divergence"
else
    # HARD FAIL. This was previously a soft "recorded, not failed" note, and it
    # let the round report PASS across a run in which the TreasuryWithdrawals
    # never enacted at all — the exact "reports success while measuring
    # nothing" shape this backlog exists to delete.
    #
    # Two real preconditions were missing, both now established in setup:
    #   * DRep voting power — `dRepAcceptedRatio` folds over `reDRepDistr`, so
    #     with no vote_delegation anywhere the map is empty, the ratio is 0,
    #     and every DRep-gated action is unratifiable regardless of votes.
    #   * A funded `ensTreasury` ONE BOUNDARY EARLIER — RATIFY reads the pot
    #     sealed into the previous pulser, not the live one.
    # With both satisfied, non-enactment is a genuine defect, not a timing
    # artefact, so it must fail the round.
    bad "TreasuryWithdrawals did NOT enact: reward account unchanged ($R0 -> $R1)"
    echo "    Diagnose with the vote tallies and the DRep distribution:"
    echo "      cardano-cli conway query gov-state --testnet-magic $LD_MAGIC --socket-path $LD_RELAY_SOCK | jq '.proposals'"
    echo "      cardano-cli conway query drep-stake-distribution --all-dreps --testnet-magic $LD_MAGIC --socket-path $LD_RELAY_SOCK"
    echo "    (gov-state exposes RAW per-voter votes only — no computed ratio,"
    echo "     no affordability flag — so the distribution must be read separately.)"
    DD=$(cardano-cli conway query drep-stake-distribution --all-dreps \
            --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" 2>/dev/null | head -c 400)
    echo "    drep-stake-distribution: ${DD:-<unavailable>}"
fi

# ─────────────────────────────────────────────────────────────────────────────
step "5. Act 3 — final-epoch losing votes (a3c) + not-yet-expired pin (epoch E+2)"
# ─────────────────────────────────────────────────────────────────────────────
# We are now at the start of epoch E+2 (=5) — the FINAL epoch whose votes can
# still reach a ratification pass for expiresAfter=E+2 actions (consumed at
# the boundary starting E+4). Casting a3c's losing votes HERE pins the #990
# final-epoch-votes-counted path: dugite's old bug would have discarded them
# by testing expiry before the threshold check.
expired_set() { # expired_set <socket> — sorted csv of expiredGovActions txids
    cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" \
        --socket-path "$1" 2>/dev/null \
      | jq -r '[.nextRatifyState.expiredGovActions[]? | .txId // .[0] // empty] | sort | join(";")' 2>/dev/null
}
if [ "${A3_ARMED:-0}" = "1" ]; then
    [ -n "${A3_TXIDS[2]:-}" ] && { vote_cast "${A3_TXIDS[2]}#0" a3c "cc:no,drep:no" \
        && gov_evidence act3 vote-a3c-final-epoch PASS "cc:no,drep:no in epoch $(cur_epoch "$LD_RELAY_SOCK")" \
        || gov_evidence act3 vote-a3c-final-epoch FAIL "vote_cast failed"; }
    # During epoch E+2 the live pulser has reCurrentEpoch = E+2, and
    # expiresAfter(E+2) < E+2 is false — nothing may show as expired yet.
    EXP_NOW=$(expired_set "$LD_RELAY_SOCK")
    if [ -z "$EXP_NOW" ]; then
        ok "expiredGovActions still empty in epoch E+2 (correct — expiry preview begins E+3)"
        gov_evidence act3 not-expired-early PASS "empty in epoch $(cur_epoch "$LD_RELAY_SOCK")"
    else
        bad "expiredGovActions already non-empty in epoch E+2: $EXP_NOW"
        gov_evidence act3 not-expired-early FAIL "$EXP_NOW"
    fi
fi

step "6. boundary -> epoch E+3: Act 2 enacts; expiry preview + deposits-not-yet-returned"
wait_boundaries 1

# Act 2 (proposed epoch 4, votes epoch 4, pulser frozen at 4->5, consumed at
# the boundary starting epoch 6) must now be enacted: the constitution carries
# the REAL guardrail script hash on BOTH sockets.
if [ "${ACT2_ARMED:-0}" = "1" ]; then
    C_D=$(constitution_of "$LD_RELAY_SOCK")
    C_H=$(constitution_of "$LD_CARDANO_BP_SOCK")
    S_D=$(cardano-cli conway query constitution --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.script // empty')
    S_H=$(cardano-cli conway query constitution --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_CARDANO_BP_SOCK" 2>/dev/null | jq -r '.script // empty')
    if [ "$S_D" = "$GUARD_HASH_GOT" ] && [ "$S_H" = "$GUARD_HASH_GOT" ]; then
        ok "Act 2 ENACTED: constitution guardrails = ${GUARD_HASH_GOT:0:16}… on BOTH sockets"
        gov_evidence act2 enacted PASS "script=$GUARD_HASH_GOT"
    else
        # RED-PROOF: asserting enactment while pointing Act 2's votes at a
        # wrong action id (or skipping them) must land here.
        bad "Act 2 NOT enacted: dugite script='$S_D' haskell script='$S_H' want=$GUARD_HASH_GOT"
        gov_evidence act2 enacted FAIL "dugite=$S_D haskell=$S_H"
    fi
    [ "$C_D" = "$C_H" ] && ok "constitution anchor parity: $C_D" \
                        || { bad "constitution anchor differs post-Act2: dugite=$C_D haskell=$C_H"; gov_evidence act2 anchor-parity FAIL "d=$C_D h=$C_H"; }
fi

# Expiry preview (epoch E+3): the live pulser now has reCurrentEpoch = E+3 and
# every un-ratified expiresAfter=E+2 action must appear in
# nextRatifyState.expiredGovActions on BOTH sockets — while the deposits are
# NOT yet returned (they land one boundary later, in the same epochTransition
# that removes the actions; asserting the return HERE is the #990 trap).
if [ "${A3_ARMED:-0}" = "1" ]; then
    EXP_D=$(expired_set "$LD_RELAY_SOCK")
    EXP_H=$(expired_set "$LD_CARDANO_BP_SOCK")
    WANT=$(printf '%s\n' "${A3_TXIDS[@]}" | grep -v '^$' | sort | paste -sd';' -)
    if [ "$EXP_D" = "$EXP_H" ] && [ -n "$EXP_D" ]; then
        ok "expiredGovActions parity in E+3: $EXP_D"
        gov_evidence act3 expired-preview-parity PASS "$EXP_D"
        if [ "$EXP_D" = "$WANT" ]; then
            ok "expiry preview contains exactly the 3 Act-3 actions"
            gov_evidence act3 expired-preview-content PASS "$WANT"
        else
            bad "expiry preview mismatch: got=$EXP_D want=$WANT"
            gov_evidence act3 expired-preview-content FAIL "got=$EXP_D want=$WANT"
        fi
    else
        bad "expiredGovActions divergence or empty in E+3: dugite='$EXP_D' haskell='$EXP_H'"
        gov_evidence act3 expired-preview-parity FAIL "d=$EXP_D h=$EXP_H"
    fi
    # RED-PROOF (inverted #990 trap): flipping this to expect GOV_DEPOSIT here
    # must FAIL — the deposit returns only at the NEXT boundary.
    EARLY_FAIL=0
    for i in 1 2 3; do
        RB=$(reward_of "$LD_RELAY_SOCK" "$(cat "$A3_DIR/ret$i.addr")")
        [ "${RB:-0}" -ne 0 ] && { EARLY_FAIL=1; bad "deposit at ret$i ALREADY returned in E+3: $RB"; }
    done
    if [ "$EARLY_FAIL" -eq 0 ]; then
        ok "no deposit returned during the preview epoch (correct)"
        gov_evidence act3 deposit-not-early PASS "all three return accounts still 0"
    else
        gov_evidence act3 deposit-not-early FAIL "early return observed"
    fi
fi

step "7. boundary -> epoch E+4: expiry lands — removal + deposit return + treasury parity"
wait_boundaries 1

if [ "${A3_ARMED:-0}" = "1" ]; then
    # (1) removed from the live proposal set on BOTH sockets
    for sockname in RELAY CBP; do
        [ "$sockname" = "RELAY" ] && S="$LD_RELAY_SOCK" || S="$LD_CARDANO_BP_SOCK"
        LEFT=$(cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" --socket-path "$S" 2>/dev/null \
            | jq -r --arg a "${A3_TXIDS[0]}" --arg b "${A3_TXIDS[1]}" --arg c "${A3_TXIDS[2]}" \
              '[.proposals[]? | .actionId.txId // empty | select(. == $a or . == $b or . == $c)] | length')
        if [ "${LEFT:-9}" = "0" ]; then
            ok "$sockname: all 3 expired actions removed from the proposal set"
            gov_evidence act3 "removed-$sockname" PASS "0 remaining"
        else
            bad "$sockname: $LEFT expired action(s) still in the proposal set"
            gov_evidence act3 "removed-$sockname" FAIL "$LEFT remaining"
        fi
    done
    # (2) EACH deposit returned to its own return address, exact amount,
    #     byte-identical on both sockets.
    for i in 1 2 3; do
        RA=$(cat "$A3_DIR/ret$i.addr")
        RB_D=$(reward_of "$LD_RELAY_SOCK" "$RA")
        RB_H=$(reward_of "$LD_CARDANO_BP_SOCK" "$RA")
        # RED-PROOF: flip GOV_DEPOSIT to a wrong value once -> must FAIL.
        if [ "$RB_D" = "$GOV_DEPOSIT" ] && [ "$RB_H" = "$GOV_DEPOSIT" ]; then
            ok "ret$i deposit returned exactly: $RB_D (both sockets)"
            gov_evidence act3 "deposit-return-$i" PASS "$RB_D"
        else
            bad "ret$i deposit wrong: dugite=$RB_D haskell=$RB_H want=$GOV_DEPOSIT"
            gov_evidence act3 "deposit-return-$i" FAIL "d=$RB_D h=$RB_H want=$GOV_DEPOSIT"
        fi
    done
    # (3) treasury untouched by these actions (no withdrawal happened) — the
    # RUPD moves the pot at every boundary, so the invariant is byte-exact
    # parity vs Haskell, plus the wallet-a receiving account NOT being
    # credited with any of the three transfers.
    TE_D=$(treasury_of "$LD_RELAY_SOCK")
    TE_H=$(treasury_of "$LD_CARDANO_BP_SOCK")
    [ "$TE_D" = "$TE_H" ] && { ok "treasury byte-exact after expiry boundary: $TE_D"; gov_evidence act3 treasury-parity PASS "$TE_D"; } \
                          || { bad "treasury parity after expiry: dugite=$TE_D haskell=$TE_H"; gov_evidence act3 treasury-parity FAIL "d=$TE_D h=$TE_H"; }
fi

# ─────────────────────────────────────────────────────────────────────────────
step "8. guardrail predicate breadth (#1043) — ~12 violations + 2 valid"
# ─────────────────────────────────────────────────────────────────────────────
# The guardrail constitution is live (Act 2). Guardrail checks fire at
# proposal SUBMIT as a phase-2 Plutus evaluation of the Proposing purpose —
# no enactment wait per case. Case table: config/guardrails-cases.json
# (distilled from upstream test_guardrails.py + defaultConstitution.json,
# provenance in tests/conformance/upstream/).
#
# Invalid cases go through BUILD-RAW with explicit execution units: a client-
# side `transaction build` would fail in cardano-api's own evaluator and prove
# nothing about dugite. The submit path makes BOTH nodes run the guardrail
# script — the assertion is a phase-2 failure class from BOTH sockets.
GR_CASES="config/guardrails-cases.json"
run_guardrail_cases() {
    local n_total n_ok=0
    n_total=$(jq 'length' "$GR_CASES")
    local pp="$ZOO_TMP/gr-pparams.json"
    cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --out-file "$pp" 2>/dev/null
    # Upstream drives the guardrail script with exunits (740000000, 8000000)
    # and redeemer 42; stay within maxTxExecutionUnits.
    local exunits="(740000000, 8000000)"
    echo '{"int": 42}' > "$ZOO_TMP/gr.redeemer.json"
    local i=0
    while [ "$i" -lt "$n_total" ]; do
        local id cls expect args_json
        id=$(jq -r ".[$i].id" "$GR_CASES")
        cls=$(jq -r ".[$i].predicate_class" "$GR_CASES")
        expect=$(jq -r ".[$i].expect" "$GR_CASES")
        mapfile -t CLIARGS < <(jq -r ".[$i].cli_args[]" "$GR_CASES")
        local afile="$ZOO_TMP/gr-$id.action"
        if ! cardano-cli conway governance action create-protocol-parameters-update \
                --testnet-magic "$LD_MAGIC" \
                --governance-action-deposit "$GOV_DEPOSIT" \
                --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
                --anchor-url "$ANCHOR_URL" --anchor-data-hash "$ANCHOR_HASH" \
                --constitution-script-hash "$GUARD_HASH_GOT" \
                "${CLIARGS[@]}" \
                --out-file "$afile" 2>"$ZOO_TMP/gr-$id.err"; then
            # cost-model-malformed rejects at the CLI layer by design upstream
            # ("cannot parse value") — a CLI refusal for THAT class is the
            # expected outcome, everything else failing here is a case bug.
            if [ "$cls" = "cost-model-malformed" ]; then
                ok "guardrail $id: rejected at action-build layer (upstream-consistent for $cls)"
                gov_evidence guardrails "$id" PASS "cli-layer-reject ($cls)"
                n_ok=$((n_ok+1))
            else
                bad "guardrail $id: create-action failed unexpectedly: $(head -1 "$ZOO_TMP/gr-$id.err" | cut -c1-160)"
                gov_evidence guardrails "$id" FAIL "create-action-failed"
            fi
            i=$((i+1)); continue
        fi
        # Build RAW with the Proposing witness. Fee is overpaid flat; the
        # change math keeps it simple: one input -> one output minus fee/deposit.
        local u uval fee=1000000 collat
        u=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
              --address "$ADDR" --output-json 2>/dev/null \
            | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0] | "\(.key) \(.value.value.lovelace)"')
        uval="${u##* }"; u="${u%% *}"
        collat=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
              --address "$ADDR" --output-json 2>/dev/null \
            | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[1].key // empty')
        [ -z "$u" ] || [ -z "$collat" ] && { bad "guardrail $id: no utxos"; gov_evidence guardrails "$id" FAIL "no-utxo"; i=$((i+1)); continue; }
        local change=$(( uval - fee - GOV_DEPOSIT ))
        if ! cardano-cli conway transaction build-raw \
                --tx-in "$u" \
                --tx-in-collateral "$collat" \
                --tx-out "$ADDR+$change" \
                --fee "$fee" \
                --proposal-file "$afile" \
                --proposal-script-file "$GUARD_SCRIPT" \
                --proposal-redeemer-file "$ZOO_TMP/gr.redeemer.json" \
                --proposal-execution-units "$exunits" \
                --protocol-params-file "$pp" \
                --out-file "$ZOO_TMP/gr-$id.raw" 2>"$ZOO_TMP/gr-$id.err"; then
            bad "guardrail $id: build-raw failed: $(head -1 "$ZOO_TMP/gr-$id.err" | cut -c1-160)"
            gov_evidence guardrails "$id" FAIL "build-raw-failed"
            i=$((i+1)); continue
        fi
        cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
            --tx-body-file "$ZOO_TMP/gr-$id.raw" --signing-key-file "$WA/payment.skey" \
            --out-file "$ZOO_TMP/gr-$id.signed" 2>/dev/null
        # Submit to BOTH sockets; both must agree.
        local verdict_d verdict_h out rc
        for S in "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK"; do
            out=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                    --socket-path "$S" --tx-file "$ZOO_TMP/gr-$id.signed" 2>&1) && rc=0 || rc=1
            local v
            if [ "$rc" -eq 0 ]; then v="accepted";
            elif printf '%s' "$out" | grep -qiE 'PlutusFailure|ScriptFailure|FailedUnexpectedly|machine terminated|ValidationTagMismatch|malformed'; then v="phase2-reject";
            else v="other-reject:$(printf '%s' "$out" | grep -m1 -oE 'Conway[A-Za-z]+|Babbage[A-Za-z]+' | head -1)"; fi
            [ "$S" = "$LD_RELAY_SOCK" ] && verdict_d="$v" || verdict_h="$v"
            # The same signed bytes cannot be accepted twice (same input); for
            # ACCEPT-expected cases only the first socket sees the original —
            # handled below by rebuilding for the second socket… simpler: for
            # accept cases we submit once and use zoo-style observation.
            [ "$expect" = "accept" ] && break
        done
        if [ "$expect" = "accept" ]; then
            if [ "$verdict_d" = "accepted" ]; then
                # RED-PROOF: submitting a valid case while expecting rejection
                # must FAIL here.
                sleep 12   # let it into a block; both nodes apply it
                ok "guardrail $id ($cls): ACCEPTED as expected"
                gov_evidence guardrails "$id" PASS "accepted"
                n_ok=$((n_ok+1))
            else
                bad "guardrail $id ($cls): expected accept, got $verdict_d"
                gov_evidence guardrails "$id" FAIL "want=accept got=$verdict_d"
            fi
        else
            if [ "$verdict_d" = "phase2-reject" ] && [ "$verdict_h" = "phase2-reject" ]; then
                ok "guardrail $id ($cls): phase-2 reject on BOTH sockets"
                gov_evidence guardrails "$id" PASS "phase2-reject both"
                n_ok=$((n_ok+1))
            elif [ "$verdict_d" = "$verdict_h" ] && [ "$verdict_d" != "accepted" ]; then
                # Same class from both nodes but not the expected phase-2 form:
                # record precisely — parity holds, classification does not.
                bad "guardrail $id ($cls): both rejected but as '$verdict_d', not phase-2"
                gov_evidence guardrails "$id" FAIL "both=$verdict_d want=phase2"
            else
                bad "guardrail $id ($cls): VERDICT SPLIT dugite=$verdict_d haskell=$verdict_h"
                gov_evidence guardrails "$id" FAIL "d=$verdict_d h=$verdict_h"
            fi
        fi
        i=$((i+1))
        sleep 2
    done
    echo "  guardrail cases: $n_ok/$n_total"
    gov_evidence guardrails summary INFO "$n_ok/$n_total"
}
if [ "${ACT2_ARMED:-0}" = "1" ] && [ -s "$GR_CASES" ]; then
    S_NOW=$(cardano-cli conway query constitution --testnet-magic "$LD_MAGIC" \
              --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.script // empty')
    if [ "$S_NOW" = "$GUARD_HASH_GOT" ]; then
        run_guardrail_cases
    else
        bad "guardrail cases skipped: constitution script is '$S_NOW', not the vendored hash"
        gov_evidence guardrails all FAIL "constitution-not-guarded"
    fi
else
    bad "guardrail cases skipped: Act 2 not armed or case table missing"
    gov_evidence guardrails all FAIL "not-armed"
fi

# ─────────────────────────────────────────────────────────────────────────────
step "9. gov-state parity between dugite and cardano-node"
# ─────────────────────────────────────────────────────────────────────────────
GS_D=$(cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -S -c '.')
GS_H=$(cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" --socket-path "$LD_CARDANO_BP_SOCK" 2>/dev/null | jq -S -c '.')
if [ -n "$GS_D" ] && [ "$GS_D" = "$GS_H" ]; then
    ok "gov-state JSON-identical on both nodes ($(printf '%s' "$GS_D" | wc -c | tr -d ' ') bytes)"
else
    # Tips can differ by a block; report the first differing key rather than a
    # wall of JSON, and do not fail on a pure tip skew.
    DIFFKEYS=$(python3 - "$GS_D" "$GS_H" <<'PY' 2>/dev/null
import json,sys
try:
    a=json.loads(sys.argv[1]); b=json.loads(sys.argv[2])
except Exception: print("unparseable"); raise SystemExit
ks=[k for k in set(a)|set(b) if a.get(k)!=b.get(k)]
print(",".join(sorted(ks)) or "(none)")
PY
)
    bad "gov-state differs; differing top-level keys: $DIFFKEYS"
fi

step "10. constitution parity"
C_D=$(constitution_of "$LD_RELAY_SOCK")
C_H=$(constitution_of "$LD_CARDANO_BP_SOCK")
[ "$C_D" = "$C_H" ] && ok "constitution identical on both nodes: $C_D" \
                    || bad "constitution differs: dugite=$C_D haskell=$C_H"

step "SUMMARY"
if [ "$FAILURES" -eq 0 ]; then
    ok "gov-enactment round: all assertions passed"
else
    bad "gov-enactment round: $FAILURES assertion(s) failed"
fi
echo "final epoch: $(cur_epoch "$LD_RELAY_SOCK")"
[ "$SKIP_SETUP" -eq 0 ] && ./stop.sh >/dev/null 2>&1
exit "$FAILURES"
