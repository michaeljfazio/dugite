#!/usr/bin/env bash
# Rewards-maturity round: make 04g execute POSITIVELY.                   (#958)
#
# WHY THIS ROUND EXISTS
# ---------------------
# `04g-reward-withdrawal.sh` records SKIP `no-rewards` on every normal run,
# because rewards need the full mark/set/go pipeline and no standard round
# crosses enough boundaries. Consequence: the `--withdrawal` wire path, the
# reward-account zeroing, and the Phase-1 semantics around withdrawals have
# NEVER executed in a release gate (v2.4.3-v2.4.5 all record the skip). #898
# was a WithdrawalAmountMismatch wedge, so this is not a hypothetical area.
#
# TIMING (oracle-verified against IntersectMBO/cardano-ledger)
#
#   `createInitialState` seeds mark/set/go ALL empty, then
#   `resetStakeDistribution` writes ONLY `mark` from the genesis stake:
#
#       initSnapShot = snapShotFromInstantStake
#                        (addInstantStake (nes ^. utxoL) mempty) dState pState
#
#   So genesis skips the "populate mark" step a mid-run delegation must pay
#   for. mark->set at 0->1, set->go at 1->2, so GO = genesis stake from epoch 2;
#   the RUPD computed during epoch 2 (bprev = epoch 1 blocks) applies at
#   boundary 2->3.
#
#   => a GENESIS-delegated key first has a withdrawable reward at the start of
#      epoch 3 — THREE boundaries. A key delegated mid-run in epoch M waits
#      until M+4. This round therefore uses a genesis delegator, which is both
#      an epoch faster and carries real stake (1/20th of the delegated supply)
#      so the reward is meaningfully non-zero.
#
#   A pool that forged ZERO blocks in the relevant epoch is structurally
#   excluded (`mkPoolRewardInfo` returns Left, `startStep` drops it), so its
#   delegators get nothing. pool1 = dugite-bp is the forger here.
#
# THE GATE THAT WILL BITE YOU (PV10, oracle-verified)
#
#   `conwayLedgerTransitionTRC` runs `validateWithdrawalsDelegated` for every
#   KeyHashObj-credentialed reward account, BEFORE and INDEPENDENT of any
#   balance check, failing with `ConwayWdrlNotDelegatedToDRep` (tag 4 of
#   ConwayLedgerPredFailure). Genesis registration structurally CANNOT set a
#   DRep delegation — `ShelleyGenesisStaking.staking.stake` is a plain
#   (stakeKeyHash, poolKeyHash) map and `registerShelleyAccount` never touches
#   `dRepDelegationAccountStateL`. So a genesis delegator's very first
#   withdrawal fails on the DRep gate, not on any amount check, unless a
#   `vote_delegation` certificate lands first. Step 1 below does exactly that.
#
# Usage:
#   ./rewards-round.sh                  # full round (~25 min)
#   RW_TARGET_EPOCH=4 ./rewards-round.sh
#   RW_SKIP_SETUP=1 ./rewards-round.sh  # reuse a running devnet

set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2

TARGET_EPOCH="${RW_TARGET_EPOCH:-3}"
SKIP_SETUP="${RW_SKIP_SETUP:-0}"
FAILURES=0

step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }

if [ "$SKIP_SETUP" -eq 0 ]; then
    step "setup + run"
    ./stop.sh >/dev/null 2>&1
    ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
    ./run.sh >/dev/null 2>&1 || { echo "RUN FAILED"; exit 2; }
fi

. ./lib/common.sh
set +e

WORK="$LD_STATE/rewards-round"
mkdir -p "$WORK"
SOCK="$LD_RELAY_SOCK"

for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 2; done
[ -S "$SOCK" ] || { echo "relay socket never appeared"; exit 2; }

cur_epoch() { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" 2>/dev/null | jq -r '.epoch // 0'; }
cur_slot()  { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" 2>/dev/null | jq -r '.slot // 0'; }

# reward_of <socket> <stake-addr>
reward_of() {
    cardano-cli conway query stake-address-info --testnet-magic "$LD_MAGIC" \
        --socket-path "$1" --address "$2" 2>/dev/null \
      | jq -r '(.[0].rewardAccountBalance // .[0].rewardBalance // 0)' 2>/dev/null || echo 0
}

DELEG_DIR="$LD_GENESIS/stake-delegators/delegator1"
DELEG2_DIR="$LD_GENESIS/stake-delegators/delegator2"
for d in "$DELEG_DIR" "$DELEG2_DIR"; do
    [ -f "$d/staking.skey" ] || { echo "missing $d/staking.skey — genesis layout changed"; exit 2; }
done

build_addrs() {
    local dir="$1" tag="$2"
    cardano-cli conway stake-address build \
        --stake-verification-key-file "$dir/staking.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$WORK/$tag.stake.addr" 2>/dev/null
    cardano-cli conway address build \
        --payment-verification-key-file "$dir/payment.vkey" \
        --stake-verification-key-file "$dir/staking.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$WORK/$tag.pay.addr" 2>/dev/null
    # The genesis UTxO for a delegator sits at the ENTERPRISE address (no stake
    # part); the base address above is only used for the reward account.
    cardano-cli conway address build \
        --payment-verification-key-file "$dir/payment.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$WORK/$tag.ent.addr" 2>/dev/null
}
build_addrs "$DELEG_DIR"  d1
build_addrs "$DELEG2_DIR" d2

STAKE_ADDR=$(cat "$WORK/d1.stake.addr")
STAKE_ADDR2=$(cat "$WORK/d2.stake.addr")
note "delegator1 stake addr: $STAKE_ADDR"

# funding_utxo <tag> — the largest UTxO across the delegator's addresses.
# `create-testnet-data` funds a stake delegator at its BASE address
# (payment + stake); the enterprise address holds NOTHING. Check base first —
# checking only the enterprise address reports "no UTxO" against a wallet
# holding 1.35e15 lovelace.
funding_utxo() {
    local tag="$1" a
    for a in "$(cat "$WORK/$tag.pay.addr")" "$(cat "$WORK/$tag.ent.addr")"; do
        [ -z "$a" ] && continue
        local j
        j=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
                --socket-path "$SOCK" --address "$a" --output-json 2>/dev/null)
        local k
        k=$(printf '%s' "$j" | jq -r 'to_entries | max_by(.value.value.lovelace) | .key // empty' 2>/dev/null)
        if [ -n "$k" ]; then printf '%s|%s' "$k" "$a"; return 0; fi
    done
    return 1
}

step "1. vote-delegate the genesis stake credentials (PV10 gate)"
# Without this every withdrawal below fails with ConwayWdrlNotDelegatedToDRep
# before any amount is even looked at. `--always-abstain` is sufficient: the
# gate only asks that a DRep delegation EXISTS.
VD_OK=0
FU=$(funding_utxo d1)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    cardano-cli conway stake-address vote-delegation-certificate \
        --stake-verification-key-file "$DELEG_DIR/staking.vkey" \
        --always-abstain --out-file "$WORK/d1.vote.cert" 2>"$WORK/vd.err"
    cardano-cli conway stake-address vote-delegation-certificate \
        --stake-verification-key-file "$DELEG2_DIR/staking.vkey" \
        --always-abstain --out-file "$WORK/d2.vote.cert" 2>>"$WORK/vd.err"
    if cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" \
            --certificate-file "$WORK/d1.vote.cert" \
            --certificate-file "$WORK/d2.vote.cert" \
            --out-file "$WORK/vd.raw" 2>>"$WORK/vd.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/vd.raw" \
            --signing-key-file "$DELEG_DIR/payment.skey" \
            --signing-key-file "$DELEG_DIR/staking.skey" \
            --signing-key-file "$DELEG2_DIR/staking.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/vd.signed" 2>>"$WORK/vd.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$SOCK" --tx-file "$WORK/vd.signed" >/dev/null 2>>"$WORK/vd.err"; then
        VD_OK=1
        ok "vote-delegation submitted for delegator1 + delegator2 (always-abstain)"
    else
        bad "vote-delegation tx failed: $(tail -3 "$WORK/vd.err" | tr '\n' ' ')"
    fi
else
    bad "no funding UTxO for delegator1 — cannot satisfy the DRep gate"
fi
sleep 10

step "2. wait for rewards to mature (target epoch >= $TARGET_EPOCH)"
note "genesis-delegated stake: GO snapshot from epoch 2, first RUPD applies at boundary 2->3"
DEADLINE=$(( $(date +%s) + 2400 ))
REWARD=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    E=$(cur_epoch)
    REWARD=$(reward_of "$SOCK" "$STAKE_ADDR")
    if [ "${E:-0}" -ge "$TARGET_EPOCH" ] && [ "${REWARD:-0}" -gt 0 ]; then break; fi
    note "epoch=${E:-?} slot=$(cur_slot) reward=${REWARD:-0} — waiting"
    sleep 30
done
E=$(cur_epoch)
REWARD=$(reward_of "$SOCK" "$STAKE_ADDR")
if [ "${REWARD:-0}" -gt 0 ]; then
    ok "reward matured at epoch $E: $REWARD lovelace"
else
    bad "no reward by epoch $E after waiting — the withdrawal path cannot be exercised"
    echo "  (pool1 must have forged in the epoch feeding the RUPD; check dugite-bp forge log)"
    exit "$FAILURES"
fi

step "3. reward balance parity across all three nodes"
R_RELAY=$(reward_of "$LD_RELAY_SOCK" "$STAKE_ADDR")
R_DBP=$(reward_of "$LD_DUGITE_BP_SOCK" "$STAKE_ADDR")
R_CBP=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR")
note "reward: relay=$R_RELAY dugite-bp=$R_DBP cardano-bp=$R_CBP"
if [ "$R_RELAY" = "$R_CBP" ] && [ "$R_DBP" = "$R_CBP" ]; then
    ok "reward balance byte-exact on dugite and Haskell: $R_CBP"
else
    bad "REWARD DIVERGENCE dugite=$R_DBP/$R_RELAY vs haskell=$R_CBP"
fi

step "4. NEGATIVE twin — withdrawing the wrong amount must be rejected"
# Haskell uses `amountAcceptable = (==)`; over and under produce the SAME
# failure (WithdrawalsNotInRewardsCERTS, ConwayCertsPredFailure tag 0, wrapped
# as ConwayCertsFailure tag 2 of ConwayLedgerPredFailure at PV<=10).
FU=$(funding_utxo d1)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    WRONG=$(( REWARD - 1 ))
    if cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" \
            --withdrawal "${STAKE_ADDR}+${WRONG}" \
            --out-file "$WORK/neg.raw" 2>"$WORK/neg.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/neg.raw" \
            --signing-key-file "$DELEG_DIR/payment.skey" \
            --signing-key-file "$DELEG_DIR/staking.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/neg.signed" 2>>"$WORK/neg.err"; then
        for s in "$SOCK" "$LD_CARDANO_BP_SOCK"; do
            OUT=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                    --socket-path "$s" --tx-file "$WORK/neg.signed" 2>&1)
            RC=$?
            NM=$(basename "$s" .sock)
            if [ "$RC" -eq 0 ]; then
                bad "$NM ACCEPTED a withdrawal of $WRONG against a balance of $REWARD"
            elif printf '%s' "$OUT" | grep -qiE 'WithdrawalsNotInRewards|IncompleteWithdrawals|withdrawal'; then
                ok "$NM rejected the wrong amount with a withdrawal-class error"
            else
                bad "$NM rejected, but not with a withdrawal error: $(printf '%s' "$OUT" | head -c 200)"
            fi
        done
    else
        # cardano-cli's `build` pre-validates withdrawals, so it may refuse to
        # construct this at all. That is still a correct rejection — record it
        # as such rather than as an untested path.
        if grep -qiE 'withdrawal|reward' "$WORK/neg.err"; then
            ok "cardano-cli refused to BUILD the wrong-amount withdrawal (client-side reject): $(head -c 160 "$WORK/neg.err")"
        else
            bad "negative twin could not be built for an unrelated reason: $(tail -3 "$WORK/neg.err" | tr '\n' ' ')"
        fi
    fi
else
    bad "no funding UTxO for the negative twin"
fi

step "5. POSITIVE — withdraw the exact balance"
BEFORE=$(reward_of "$SOCK" "$STAKE_ADDR")
FU=$(funding_utxo d1)
if [ -n "$FU" ] && [ "${BEFORE:-0}" -gt 0 ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" \
            --withdrawal "${STAKE_ADDR}+${BEFORE}" \
            --out-file "$WORK/pos.raw" 2>"$WORK/pos.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pos.raw" \
            --signing-key-file "$DELEG_DIR/payment.skey" \
            --signing-key-file "$DELEG_DIR/staking.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pos.signed" 2>>"$WORK/pos.err"; then
        TXID=$(cardano-cli conway transaction txid --tx-file "$WORK/pos.signed" 2>/dev/null \
               | jq -r '.txhash // empty' 2>/dev/null)
        [ -z "$TXID" ] && TXID=$(cardano-cli conway transaction txid --tx-file "$WORK/pos.signed" 2>/dev/null | tr -d '[:space:]')
        OUT=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                --socket-path "$SOCK" --tx-file "$WORK/pos.signed" 2>&1)
        if [ $? -ne 0 ]; then
            bad "exact-balance withdrawal REJECTED: $(printf '%s' "$OUT" | head -c 300)"
        else
            ok "withdrawal of $BEFORE submitted (tx $TXID)"
            SEEN=0
            for _ in $(seq 1 40); do
                sleep 3
                A=$(reward_of "$LD_RELAY_SOCK"      "$STAKE_ADDR")
                B=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR")
                if [ "${A:-1}" -eq 0 ] && [ "${B:-1}" -eq 0 ]; then SEEN=1; break; fi
            done
            if [ "$SEEN" -eq 1 ]; then
                ok "reward account ZEROED on BOTH dugite and Haskell (was $BEFORE)"
            else
                bad "reward account did not reach 0 on both nodes (dugite=$(reward_of "$LD_RELAY_SOCK" "$STAKE_ADDR") haskell=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR"))"
            fi
        fi
    else
        bad "could not build the positive withdrawal: $(tail -3 "$WORK/pos.err" | tr '\n' ' ')"
    fi
else
    bad "no funding UTxO or zero balance for the positive withdrawal"
fi

step "6. MULTI-WITHDRAWAL — two reward accounts in one transaction"
R1=$(reward_of "$SOCK" "$STAKE_ADDR")
R2=$(reward_of "$SOCK" "$STAKE_ADDR2")
note "delegator1 reward=$R1 delegator2 reward=$R2"
if [ "${R2:-0}" -gt 0 ]; then
    FU=$(funding_utxo d2)
    if [ -n "$FU" ]; then
        TXIN="${FU%%|*}"; FADDR="${FU##*|}"
        ARGS=(--withdrawal "${STAKE_ADDR2}+${R2}")
        SIGNS=(--signing-key-file "$DELEG2_DIR/payment.skey"
               --signing-key-file "$DELEG2_DIR/staking.skey")
        if [ "${R1:-0}" -gt 0 ]; then
            ARGS+=(--withdrawal "${STAKE_ADDR}+${R1}")
            SIGNS+=(--signing-key-file "$DELEG_DIR/staking.skey")
        fi
        if cardano-cli conway transaction build \
                --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
                --tx-in "$TXIN" --change-address "$FADDR" \
                "${ARGS[@]}" --out-file "$WORK/multi.raw" 2>"$WORK/multi.err" \
           && cardano-cli conway transaction sign --tx-body-file "$WORK/multi.raw" \
                "${SIGNS[@]}" --testnet-magic "$LD_MAGIC" \
                --out-file "$WORK/multi.signed" 2>>"$WORK/multi.err" \
           && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                --socket-path "$SOCK" --tx-file "$WORK/multi.signed" >/dev/null 2>>"$WORK/multi.err"; then
            ok "multi-account withdrawal accepted (${#ARGS[@]} withdrawal args)"
            sleep 20
            M2=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR2")
            if [ "${M2:-1}" -eq 0 ]; then
                ok "delegator2 reward account zeroed on the Haskell node"
            else
                bad "delegator2 reward still $M2 after the multi-withdrawal"
            fi
        else
            bad "multi-withdrawal failed: $(tail -3 "$WORK/multi.err" | tr '\n' ' ')"
        fi
    else
        bad "no funding UTxO for delegator2"
    fi
else
    note "delegator2 has no reward yet — multi-withdrawal SKIPPED (state, not env)"
fi

step "SUMMARY"
if [ "$FAILURES" -eq 0 ]; then
    ok "rewards round: all assertions passed"
else
    bad "rewards round: $FAILURES assertion(s) failed"
fi
note "final epoch: $(cur_epoch)"
exit "$FAILURES"
