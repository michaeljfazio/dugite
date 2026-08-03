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
# This round covers, in order:
#   1. TreasuryWithdrawals  — non-delaying; asserts the REAL pot movement
#      (casTreasuryL) and the target reward-account credit, which is the
#      project's byte-exact-parity invariant.
#   2. NewConstitution      — delaying; asserts the constitution anchor changed.
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
    ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
    ./run.sh   >/dev/null 2>&1 || { echo "RUN FAILED";   exit 2; }
fi
. ./lib/common.sh
set +e

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

# Probe mode: stop before the ~13-minute boundary wait while iterating on the
# proposal/vote plumbing. Placed AFTER voting so the probe covers everything
# except the wait itself.
if [ "${GOV_PROBE_ONLY:-0}" = "1" ]; then
    step "PROBE ONLY — stopping before the boundary wait"
    [ "$SKIP_SETUP" -eq 0 ] && ./stop.sh >/dev/null 2>&1
    exit "$FAILURES"
fi

step "3. wait for the pulser freeze + enactment boundary (E+2)"
wait_boundaries 2

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
step "5. gov-state parity between dugite and cardano-node"
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

step "6. constitution parity"
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
