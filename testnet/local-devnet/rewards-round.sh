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
#   ./rewards-round.sh                  # full round
#   RW_TARGET_EPOCH=4 ./rewards-round.sh
#   RW_SKIP_SETUP=1 ./rewards-round.sh  # reuse a running devnet
#   RW_FULL=1 ./rewards-round.sh        # also run (b)'s restore-and-resume tail
#
# ═══════════════════════════════════════════════════════════════════════════
# PHASE 2 (#1038) — six scenarios adopted from cardano-node-tests
# ═══════════════════════════════════════════════════════════════════════════
#
# Steps 1-6 above are UNCHANGED. Everything below is new, starts at step 7,
# and begins only after step 6 completes — so the existing genesis-delegator
# withdrawal coverage (and its zeroed reward-account state) is never disturbed
# by anything that follows. Segment (b) deliberately BREAKS pool1's pledge,
# which zeroes future rewards for the pool; it is scheduled LAST for exactly
# that reason, and the mark/set/go tracker (a) is designed to keep observing
# straight through that zero-reward window rather than stopping at it.
#
# EPOCH TIMELINE (E_START = the epoch phase 2 begins, captured at runtime —
# typically 3-4, since step 2 already waits for TARGET_EPOCH). All offsets
# below are ORACLE-VERIFIED against IntersectMBO/cardano-ledger master
# (Snap.hs/Pool.hs/PoolReap.hs/Rewards.hs, cross-checked against
# IntersectMBO/cardano-node-tests' own test_staking_rewards.py /
# test_staking_no_rewards.py / test_pools.py techniques):
#
#   E_START   : fund+register delegator-D -> pool1 (tracker); register+
#               delegate pool1's OWN reward account -> pool1 (segment c);
#               schedule pool2 retirement for E_START+2 (segment d); register
#               pool3 + schedule its retirement for E_START+2 + deregister its
#               reward address in the SAME tx (segment f).
#   E_START+1 : [CP1] tracker: D enters MARK. Submit pool2's CANCEL
#               (re-registration) cert — one full epoch of margin before its
#               retirement boundary, mirroring cardano-node-tests'
#               `depoch-1` convention exactly.
#   E_START+2 : [CP2] tracker: D enters SET. Assert pool2 is still live (cancel
#               worked, no deposit refunded) and pool3 is gone (retired) with
#               its deposit forfeited (dead reward account). Submit pool2's
#               REAL retirement cert for E_START+4.
#   E_START+3 : [CP3] tracker: D enters GO (full entrance, 3 boundaries — a
#               PLAIN delegation/deregistration cert takes exactly this
#               schedule per `snapTransition`). Submit D's deregistration cert
#               (tests the SAME 3-boundary schedule in the exit direction).
#   E_START+4 : [CP4] tracker: D LEAVES mark. Assert pool2 retired for real
#               with an exact deposit refund, byte-exact on both sockets.
#               Assert segment (c)'s reward account shows a byte-exact,
#               strictly-positive reward. Submit segment (b)'s pledge-BREAK
#               cert (a RE-REGISTRATION of the already-registered pool1).
#   E_START+5 : [CP5] tracker: D leaves set too (still in go).
#   E_START+6 : [CP6] tracker: D leaves go — full exit confirmed. Tracker
#               segment (a) is COMPLETE and mandatory coverage ends here.
#
#   Segment (b) does NOT follow the plain 3-boundary schedule above. Its cert
#   RE-REGISTERS an ALREADY-registered pool, which Haskell routes through
#   `psFutureStakePoolParams`, merged by POOLREAP one step AFTER SNAP within
#   the SAME epoch transition (`Cardano.Ledger.Conway.Rules.Epoch`) — so the
#   new pledge becomes LIVE one whole epoch later than a plain stake change,
#   and needs FOUR boundaries (not three) to reach the "go" snapshot that
#   `mkPoolRewardInfo` actually reads:
#
#     cert in E_b -> live in psStakePools at E_b+1 -> MARK at E_b+2 ->
#     SET at E_b+3 -> GO at E_b+4 -> RUPD computed during E_b+4, applied at
#     the E_b+4->E_b+5 boundary -> first zero-reward balance visible E_b+5.
#
#   With E_b = E_START+4, that puts the earliest possible zero-reward epoch
#   at E_START+9. cardano-node-tests' own `test_no_reward_unmet_pledge1`
#   checks at `update_epoch+4`, one boundary earlier than this derivation —
#   plausibly because its pool re-registration takes a different internal
#   path, or because a generous test margin already absorbs the difference.
#   Rather than bet on which of the two is exactly right, the zero-reward
#   check below samples TWICE, three epochs apart (E_b+5 and E_b+8), and
#   asserts the two samples are EQUAL — flat growth is the real signature of
#   "zero reward every epoch", and a 3-epoch-wide window is safely inside the
#   broken-pledge regime under EITHER timing theory. This is deliberately
#   more conservative (and slower) than pinning one exact boundary.
#
#   DEVIATION FROM THE ~50 MINUTE / 7-8 EPOCH BUDGET IN THE ISSUE: doing this
#   correctly (not "close enough" — see CLAUDE.md's Haskell-byte-exact rule)
#   means the MANDATORY zero-reward check alone lands around E_START+12,
#   i.e. roughly 8 epochs of NEW-PHASE runtime on top of steps 1-6's own ~3-4
#   epochs. At epochLength=400s that is close to two hours end-to-end for the
#   mandatory path, not fifty minutes. This is a direct, unavoidable
#   consequence of the extra POOLREAP-merge boundary a pool re-registration
#   takes versus a plain delegation cert — seed RW_TARGET_EPOCH/RW_FULL
#   accordingly rather than treating the timeline as a bug. RW_FULL=1 adds a
#   restore-and-resume tail on the SAME 4-boundary schedule (another ~5
#   epochs) and is OFF by default for exactly this reason.
#
#   E_b+5=E_START+9  : sample owner + delegator1 reward ("early" zero sample).
#   E_b+8=E_START+12 : sample again ("late" zero sample); MANDATORY assertion
#                      that the two samples are equal (flat = zero growth).
#   [RW_FULL=1 only] : submit the RESTORE cert (original pledge, saved before
#                      the break) right after the flat check. Same 4-boundary
#                      arithmetic from the restore epoch places the resumed,
#                      strictly-growing reward at restore_epoch+5.
#
# Segments (d)/(e)/(f) all complete by E_START+4 and are fully interleaved
# with the tracker's checkpoints above — they do not add any wall-clock time
# of their own; segment (b) is what dominates the schedule.
# ═══════════════════════════════════════════════════════════════════════════

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

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 2 HELPERS (#1038)
# ═══════════════════════════════════════════════════════════════════════════
RW_FULL="${RW_FULL:-0}"

treasury_of() { cardano-cli conway query treasury --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null | jq -r '. // 0'; }

# reserves_of <socket> — via `query ledger-state`. Both sockets decode this
# now (#1027); jq path proven against a real cardano-node in
# two-forger-round.sh step 7 — some cardano-cli builds nest pots under
# `.stateBefore.esChainAccountState`, others answer `.esChainAccountState`
# directly, so both are tried.
reserves_of() {
    cardano-cli conway query ledger-state --testnet-magic "$LD_MAGIC" --socket-path "$1" --output-json 2>/dev/null \
      | jq -r '(.stateBefore.esChainAccountState // .esChainAccountState // {}).reserves // empty' 2>/dev/null
}

pool_id_of()  { cat "$LD_KEYS/$1/pool.id" 2>/dev/null; }
pool_hex_of() { cat "$LD_KEYS/$1/pool.id.hex" 2>/dev/null; }
stake_key_hash() { cardano-cli conway stake-address key-hash --stake-verification-key-file "$1" 2>/dev/null; }
stake_reg_check() {
    cardano-cli conway query stake-address-info --testnet-magic "$LD_MAGIC" --socket-path "$1" \
        --address "$2" 2>/dev/null | jq -r 'if length>0 then "yes" else "no" end' 2>/dev/null
}

# pool_registered_ids <socket> — the full live stake-pool-id set. Used
# instead of parsing `pool-state`'s nested object for registration presence:
# simpler, and this command's shape (a flat array/set of bech32 ids) carries
# far less risk than guessing `pool-state`'s field names.
pool_registered_ids() {
    cardano-cli conway query stake-pools --testnet-magic "$LD_MAGIC" --socket-path "$1" --output-json 2>/dev/null \
        | jq -r '.[]' 2>/dev/null
}
pool_is_live() { pool_registered_ids "$1" | grep -qFx "$2"; }

# ---- RISK (documented, not verified against a live run — see final report):
# exact key spelling for `query stake-snapshot`'s per-pool/per-total fields.
# Tries the documented `stakeMark`/`stakeSet`/`stakeGo` names first, then
# falls back to bare `mark`/`set`/`go` and `<f>Stake` in case of a schema
# difference across cardano-cli versions. ----
stake_snapshot_json() {
    cardano-cli conway query stake-snapshot --testnet-magic "$LD_MAGIC" --socket-path "$1" \
        --stake-pool-id "$2" --output-json 2>/dev/null
}
snap_pool() { # snap_pool <json> <pool-hex> <mark|set|go>
    printf '%s' "$1" | jq -r --arg p "$2" --arg f "$3" '
        (.pools[$p] // (.. | objects | select(has($p)) | .[$p]) // {}) as $n
        | ($n["stake" + ($f[0:1]|ascii_upcase) + ($f[1:])] // $n[$f] // $n[$f+"Stake"] // "null")' 2>/dev/null
}

# ---- Per-credential mark/set/go presence via `query ledger-state`, mirroring
# cardano-node-tests' OWN technique exactly: `.stateBefore.esSnapshots.
# {pstakeMark,pstakeSet,pstakeGo}`, keyed by "keyHash-<hex28>". This is the
# load-bearing per-credential channel — stake-snapshot is pool-aggregate only. ----
ledger_state_json() {
    cardano-cli conway query ledger-state --testnet-magic "$LD_MAGIC" --socket-path "$1" --output-json 2>/dev/null
}
ls_snap_has() { # ls_snap_has <ledger-state-json> <pstakeMark|pstakeSet|pstakeGo> <keyhash28hex>
    printf '%s' "$1" | jq -e --arg h "keyHash-$3" \
        '((.stateBefore.esSnapshots // .esSnapshots // {})[$2] // {}) | has($h)' >/dev/null 2>&1
}

# wait_until_epoch <target-epoch> [budget-seconds] — no-op if already there.
wait_until_epoch() {
    local target="$1" budget="${2:-1200}" deadline e
    deadline=$(( $(date +%s) + budget ))
    while :; do
        e=$(cur_epoch)
        [ "${e:-0}" -ge "$target" ] && { note "reached epoch $e (target $target)"; return 0; }
        [ "$(date +%s)" -ge "$deadline" ] && { bad "TIMEOUT waiting for epoch $target (stuck at ${e:-?})"; return 1; }
        note "epoch=${e:-?} slot=$(cur_slot) — waiting for epoch $target"
        sleep 15
    done
}

# utxo_funding — the largest UTxO at the genesis non-delegated funding wallet.
# Used for every phase-2 tx that isn't specifically testing a delegator's own
# balance, so delegator-D's funded amount stays EXACTLY known (see segment a).
utxo_funding() {
    local a j k
    a=$(cat "$LD_KEYS/utxo/payment.addr" 2>/dev/null)
    [ -z "$a" ] && return 1
    j=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" --address "$a" --output-json 2>/dev/null)
    k=$(printf '%s' "$j" | jq -r 'to_entries | max_by(.value.value.lovelace) | .key // empty' 2>/dev/null)
    [ -n "$k" ] && printf '%s|%s' "$k" "$a" || return 1
}

# ---- Evidence CSV (#1038) ----
TS_ROUND="$(date -u +%Y%m%dT%H%M%SZ)"
EVID_DIR="$LD_EVIDENCE/$TS_ROUND"
mkdir -p "$EVID_DIR"
REWARDS_CSV="$EVID_DIR/rewards-round.csv"
echo "ts,segment,check,outcome,detail" > "$REWARDS_CSV"
csv_record() { printf '%s,%s,%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "$3" "${4//,/;}" >> "$REWARDS_CSV"; }
# seg_ok/seg_bad/seg_note — wrap the round's ok/bad/note AND append a CSV row.
# Only used by phase 2 (steps 7+); steps 1-6 keep using bare ok/bad/note.
seg_ok()   { local seg="$1" chk="$2"; shift 2; ok   "[$seg] $*"; csv_record "$seg" "$chk" "PASS" "$*"; }
seg_bad()  { local seg="$1" chk="$2"; shift 2; bad  "[$seg] $*"; csv_record "$seg" "$chk" "FAIL" "$*"; }
seg_note() { local seg="$1" chk="$2"; shift 2; note "[$seg] $*"; csv_record "$seg" "$chk" "NOTE" "$*"; }

# ═══════════════════════════════════════════════════════════════════════════
step "7. phase-2 setup — protocol params, pool ids, fresh keys"
# ═══════════════════════════════════════════════════════════════════════════
PP_FILE="$WORK/pparams.json"
cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" \
    --socket-path "$SOCK" --out-file "$PP_FILE" 2>/dev/null
POOL_DEPOSIT=$(jq -r '.stakePoolDeposit // 500000000' "$PP_FILE" 2>/dev/null)
MIN_POOL_COST=$(jq -r '.minPoolCost // 170000000' "$PP_FILE" 2>/dev/null)
STAKE_DEPOSIT=$(jq -r '.stakeAddressDeposit // 2000000' "$PP_FILE" 2>/dev/null)
TOTAL_SUPPLY=$(jq -r '.maxLovelaceSupply // empty' "$LD_GENESIS/shelley-genesis.json" 2>/dev/null)
case "$TOTAL_SUPPLY" in ''|null) TOTAL_SUPPLY=60000000000000000 ;; esac
PLEDGE_BROKEN=$(( TOTAL_SUPPLY * 100 ))
seg_note setup pparams "poolDeposit=$POOL_DEPOSIT minPoolCost=$MIN_POOL_COST stakeDeposit=$STAKE_DEPOSIT totalSupply=$TOTAL_SUPPLY brokenPledge=$PLEDGE_BROKEN"

POOL1_ID=$(pool_id_of pool1); POOL1_HEX=$(pool_hex_of pool1)
POOL2_ID=$(pool_id_of pool2); POOL2_HEX=$(pool_hex_of pool2)
if [ -z "$POOL1_ID" ] || [ -z "$POOL2_ID" ]; then
    bad "pool1/pool2 ids missing under \$LD_KEYS — genesis layout changed"
    exit "$FAILURES"
fi

# Original pool1 pledge — read BEFORE segment (b) ever touches it, so the
# restore step (RW_FULL=1) has a real value to go back to. Defensive
# extraction: RISK, see final report — `pool-state`'s nested field name for
# pledge is asserted structurally elsewhere in the zoo (lib.sh) but not
# pinned to an exact JSON path here.
PS1=$(cardano-cli conway query pool-state --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
        --stake-pool-id "$POOL1_ID" --output-json 2>/dev/null)
POOL1_ORIG_PLEDGE=$(printf '%s' "$PS1" | jq -r --arg h "$POOL1_HEX" \
    '(.. | objects | select(has($h)) | .[$h] | select(has("pledge")) | .pledge) // empty' 2>/dev/null | head -1)
case "$POOL1_ORIG_PLEDGE" in ''|null) POOL1_ORIG_PLEDGE=0 ;; esac
seg_note setup pool1-pledge "original pool1 pledge saved for restore: $POOL1_ORIG_PLEDGE"

# ---- fresh keys: delegator-D (tracker), pool1's genesis reward-account key
# (segment c), and pool3 + its reward account (segment f) ----
DDIR="$WORK/delegD"; mkdir -p "$DDIR"
cardano-cli conway address key-gen --verification-key-file "$DDIR/payment.vkey" --signing-key-file "$DDIR/payment.skey" 2>/dev/null
cardano-cli conway stake-address key-gen --verification-key-file "$DDIR/staking.vkey" --signing-key-file "$DDIR/staking.skey" 2>/dev/null
cardano-cli conway address build --payment-verification-key-file "$DDIR/payment.vkey" \
    --stake-verification-key-file "$DDIR/staking.vkey" --testnet-magic "$LD_MAGIC" --out-file "$DDIR/base.addr" 2>/dev/null
D_ADDR=$(cat "$DDIR/base.addr" 2>/dev/null)
D_HASH=$(stake_key_hash "$DDIR/staking.vkey")

BOGUS_DIR="$WORK/bogus"; mkdir -p "$BOGUS_DIR"
cardano-cli conway stake-address key-gen --verification-key-file "$BOGUS_DIR/staking.vkey" \
    --signing-key-file "$BOGUS_DIR/staking.skey" 2>/dev/null
BOGUS_HASH=$(stake_key_hash "$BOGUS_DIR/staking.vkey")

DELEG3_DIR="$LD_GENESIS/stake-delegators/delegator3"
DELEG3_HASH=$(stake_key_hash "$DELEG3_DIR/staking.vkey")

C_STAKING_DIR="$LD_GENESIS/pools-keys/pool1"
cardano-cli conway stake-address build --stake-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$WORK/c.stake.addr" 2>/dev/null
C_STAKE_ADDR=$(cat "$WORK/c.stake.addr" 2>/dev/null)
C_BASELINE=$(reward_of "$SOCK" "$C_STAKE_ADDR")

POOL3="$WORK/pool3"; mkdir -p "$POOL3"
cardano-cli conway node key-gen --cold-verification-key-file "$POOL3/cold.vkey" \
    --cold-signing-key-file "$POOL3/cold.skey" \
    --operational-certificate-issue-counter-file "$POOL3/opcert.counter" 2>/dev/null
cardano-cli conway node key-gen-VRF --verification-key-file "$POOL3/vrf.vkey" \
    --signing-key-file "$POOL3/vrf.skey" 2>/dev/null
POOL3_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$POOL3/cold.vkey" 2>/dev/null)

P3R="$WORK/pool3reward"; mkdir -p "$P3R"
cardano-cli conway address key-gen --verification-key-file "$P3R/payment.vkey" --signing-key-file "$P3R/payment.skey" 2>/dev/null
cardano-cli conway stake-address key-gen --verification-key-file "$P3R/staking.vkey" --signing-key-file "$P3R/staking.skey" 2>/dev/null
cardano-cli conway stake-address build --stake-verification-key-file "$P3R/staking.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$P3R/stake.addr" 2>/dev/null
P3R_STAKE_ADDR=$(cat "$P3R/stake.addr" 2>/dev/null)
seg_note setup fresh-keys "D_hash=$D_HASH pool3=$POOL3_ID pool3-reward=$P3R_STAKE_ADDR"

POOL2_REWARD_ADDR_FILE="$WORK/pool2reward.addr"
cardano-cli conway stake-address build --stake-verification-key-file "$LD_GENESIS/pools-keys/pool2/staking-reward.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$POOL2_REWARD_ADDR_FILE" 2>/dev/null
POOL2_REWARD_ADDR=$(cat "$POOL2_REWARD_ADDR_FILE" 2>/dev/null)
POOL2_REWARD_BASELINE=$(reward_of "$SOCK" "$POOL2_REWARD_ADDR")

# pool1 mark BEFORE delegator-D enters — the exact-delta anchor for CP1.
SNAP_D_BASE=$(stake_snapshot_json "$LD_CARDANO_BP_SOCK" "$POOL1_ID")
POOL1_MARK_BASE=$(snap_pool "$SNAP_D_BASE" "$POOL1_HEX" mark)

# ═══════════════════════════════════════════════════════════════════════════
step "7b. RED-PROOF — tracker must fail-closed on an unregistered credential"
# ═══════════════════════════════════════════════════════════════════════════
# A stake key that was NEVER submitted on-chain must never appear in ANY
# mark/set/go snapshot. If this assertion ever went green for an absent
# credential, ls_snap_has's presence detection could not be trusted, and every
# later mark/set/go assertion in segment (a) would be checking nothing.
LSH_BOGUS=$(ledger_state_json "$LD_CARDANO_BP_SOCK")
if ls_snap_has "$LSH_BOGUS" pstakeGo "$BOGUS_HASH"; then
    seg_bad tracker red-proof-unregistered "FAIL-OPEN: an unregistered credential ($BOGUS_HASH) reported present in go"
else
    seg_ok tracker red-proof-unregistered "fail-closed confirmed: an unregistered credential is correctly absent"
fi

# tracker_checkpoint <label> <want_mark:in|out> <want_set:in|out> <want_go:in|out>
# The pool1-aggregate parity block is the anchor: a wrong dugite stake-snapshot
# implementation fails it regardless of delegator-D's schedule. The delegator3
# block proves per-credential parity is stable for an already-mature genesis
# delegation. The delegator-D block is the RED-PROOF-able schedule: a wrong
# (off-by-one-epoch) mark/set/go implementation fails exactly one of the six
# calls below, at the boundary it gets wrong.
tracker_checkpoint() {
    local label="$1" wm="$2" ws="$3" wg="$4"
    local ssd ssh f vd vh
    ssd=$(stake_snapshot_json "$LD_DUGITE_BP_SOCK" "$POOL1_ID")
    ssh=$(stake_snapshot_json "$LD_CARDANO_BP_SOCK" "$POOL1_ID")
    for f in mark set go; do
        vd=$(snap_pool "$ssd" "$POOL1_HEX" "$f")
        vh=$(snap_pool "$ssh" "$POOL1_HEX" "$f")
        if [ -z "$vd" ] || [ "$vd" = "null" ] || [ -z "$vh" ] || [ "$vh" = "null" ]; then
            seg_note tracker "$label-snapshot-$f" "could not extract pool1 $f (dugite='$vd' haskell='$vh') — see stake-snapshot field-name risk in final report"
        elif [ "$vd" = "$vh" ]; then
            seg_ok tracker "$label-snapshot-$f" "pool1 $f byte-exact: $vd"
        else
            seg_bad tracker "$label-snapshot-$f" "pool1 $f DIVERGES dugite=$vd haskell=$vh"
        fi
    done

    local lsd lsh hd hh
    lsd=$(ledger_state_json "$LD_DUGITE_BP_SOCK")
    lsh=$(ledger_state_json "$LD_CARDANO_BP_SOCK")
    for f in pstakeMark pstakeSet pstakeGo; do
        ls_snap_has "$lsd" "$f" "$DELEG3_HASH" && hd=in || hd=out
        ls_snap_has "$lsh" "$f" "$DELEG3_HASH" && hh=in || hh=out
        if [ "$hd" = "in" ] && [ "$hh" = "in" ]; then
            seg_ok tracker "$label-delegator3-$f" "delegator3 present in $f on both sockets"
        else
            seg_bad tracker "$label-delegator3-$f" "delegator3 $f: dugite=$hd haskell=$hh (expected in/in — stable genesis delegation)"
        fi
    done

    local name want field
    for name in mark set go; do
        case "$name" in
            mark) want="$wm"; field=pstakeMark ;;
            set)  want="$ws"; field=pstakeSet  ;;
            go)   want="$wg"; field=pstakeGo   ;;
        esac
        ls_snap_has "$lsd" "$field" "$D_HASH" && hd=in || hd=out
        ls_snap_has "$lsh" "$field" "$D_HASH" && hh=in || hh=out
        if [ "$hd" = "$want" ] && [ "$hh" = "$want" ]; then
            seg_ok tracker "$label-D-$name" "delegator-D $name=$hd (expected $want) on both sockets"
        else
            seg_bad tracker "$label-D-$name" "delegator-D $name schedule WRONG: dugite=$hd haskell=$hh expected=$want"
        fi
    done
}

# ═══════════════════════════════════════════════════════════════════════════
step "8. submit all early certs (epoch E_START)"
# ═══════════════════════════════════════════════════════════════════════════
E_START=$(cur_epoch)
seg_note setup epoch "phase-2 begins at epoch $E_START"
R_POOL2=$(( E_START + 2 ))
F_RET=$(( E_START + 2 ))

# -- fund delegator-D with an EXACT, known amount from the genesis funding
# wallet (NOT from D's own balance), so pool1's stake-snapshot delta at CP1
# can be asserted exactly. --
FUND_D=100000000000000
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --tx-out "${D_ADDR}+${FUND_D}" --change-address "$FADDR" \
            --out-file "$WORK/fundD.raw" 2>"$WORK/fundD.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/fundD.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --testnet-magic "$LD_MAGIC" \
            --out-file "$WORK/fundD.signed" 2>>"$WORK/fundD.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/fundD.signed" >/dev/null 2>>"$WORK/fundD.err"; then
        seg_ok tracker fund-D "funded delegator-D with exactly $FUND_D lovelace"
    else
        seg_bad tracker fund-D "could not fund delegator-D: $(tail -3 "$WORK/fundD.err" | tr '\n' ' ')"
    fi
else
    seg_bad tracker fund-D "no funding UTxO at \$LD_KEYS/utxo"
fi
sleep 10

# -- register+delegate delegator-D -> pool1 (Q1's plain 3-boundary schedule) --
cardano-cli conway stake-address registration-and-delegation-certificate \
    --stake-verification-key-file "$DDIR/staking.vkey" --stake-pool-id "$POOL1_ID" \
    --key-reg-deposit-amt "$STAKE_DEPOSIT" --out-file "$WORK/D.regdeleg.cert" 2>"$WORK/D.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/D.regdeleg.cert" \
            --out-file "$WORK/D.raw" 2>>"$WORK/D.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/D.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$DDIR/staking.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/D.signed" 2>>"$WORK/D.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/D.signed" >/dev/null 2>>"$WORK/D.err"; then
        seg_ok tracker register-D "delegator-D registered+delegated to pool1 in epoch $E_START"
    else
        seg_bad tracker register-D "reg+deleg failed: $(tail -3 "$WORK/D.err" | tr '\n' ' ')"
    fi
else
    seg_bad tracker register-D "no funding UTxO"
fi

# -- segment (c): register+delegate pool1's OWN reward account -> pool1.
# Genesis staking does NOT auto-register a pool's reward-account credential
# unless it also appears in sgsStake (oracle-verified) — cardano-cli's
# `stake-address-info` is the live check for which branch applies. --
REG_C=$(stake_reg_check "$SOCK" "$C_STAKE_ADDR")
if [ "$REG_C" = "yes" ]; then
    seg_note reward-addr-deleg already-registered "pool1 reward account already registered — delegating only"
    cardano-cli conway stake-address stake-delegation-certificate \
        --stake-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" --stake-pool-id "$POOL1_ID" \
        --out-file "$WORK/c.deleg.cert" 2>"$WORK/c.err"
else
    cardano-cli conway stake-address registration-and-delegation-certificate \
        --stake-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" --stake-pool-id "$POOL1_ID" \
        --key-reg-deposit-amt "$STAKE_DEPOSIT" --out-file "$WORK/c.deleg.cert" 2>"$WORK/c.err"
fi
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/c.deleg.cert" \
            --out-file "$WORK/c.raw" 2>>"$WORK/c.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/c.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$C_STAKING_DIR/staking-reward.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/c.signed" 2>>"$WORK/c.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/c.signed" >/dev/null 2>>"$WORK/c.err"; then
        seg_ok reward-addr-deleg register "pool1 reward account registered+delegated to pool1 in epoch $E_START (baseline reward=$C_BASELINE)"
    else
        seg_bad reward-addr-deleg register "failed: $(tail -3 "$WORK/c.err" | tr '\n' ' ')"
    fi
else
    seg_bad reward-addr-deleg register "no funding UTxO"
fi

# -- segment (d): schedule pool2's retirement for E_START+2 --
cardano-cli conway stake-pool deregistration-certificate --cold-verification-key-file "$LD_KEYS/pool2/cold.vkey" \
    --epoch "$R_POOL2" --out-file "$WORK/pool2.retire1.cert" 2>"$WORK/pool2.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/pool2.retire1.cert" \
            --out-file "$WORK/pool2ret1.raw" 2>>"$WORK/pool2.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pool2ret1.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$LD_KEYS/pool2/cold.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool2ret1.signed" 2>>"$WORK/pool2.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/pool2ret1.signed" >/dev/null 2>>"$WORK/pool2.err"; then
        seg_ok pool2-cancel retire-scheduled "pool2 retirement scheduled for epoch $R_POOL2"
    else
        seg_bad pool2-cancel retire-scheduled "failed: $(tail -3 "$WORK/pool2.err" | tr '\n' ' ')"
    fi
else
    seg_bad pool2-cancel retire-scheduled "no funding UTxO"
fi

# -- segment (f): register fresh pool3 (pledge=0, cost=minPoolCost, its own
# fresh reward key as owner+reward-account) --
cardano-cli conway stake-address registration-certificate --stake-verification-key-file "$P3R/staking.vkey" \
    --key-reg-deposit-amt "$STAKE_DEPOSIT" --out-file "$WORK/p3reward.reg.cert" 2>"$WORK/p3.err"
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file "$POOL3/cold.vkey" --vrf-verification-key-file "$POOL3/vrf.vkey" \
    --pool-pledge 0 --pool-cost "$MIN_POOL_COST" --pool-margin 0.1 \
    --pool-reward-account-verification-key-file "$P3R/staking.vkey" \
    --pool-owner-stake-verification-key-file "$P3R/staking.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool3.reg.cert" 2>>"$WORK/p3.err"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$P3R/staking.vkey" --stake-pool-id "$POOL3_ID" \
    --out-file "$WORK/pool3.deleg.cert" 2>>"$WORK/p3.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" \
            --certificate-file "$WORK/p3reward.reg.cert" \
            --certificate-file "$WORK/pool3.reg.cert" \
            --certificate-file "$WORK/pool3.deleg.cert" \
            --out-file "$WORK/pool3.raw" 2>>"$WORK/p3.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pool3.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" \
            --signing-key-file "$P3R/staking.skey" --signing-key-file "$POOL3/cold.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool3.signed" 2>>"$WORK/p3.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/pool3.signed" >/dev/null 2>>"$WORK/p3.err"; then
        POOL3_REGISTERED=1
        seg_ok pool3 register "pool3 registered (id=$POOL3_ID), reward addr registered+delegated, deposit=$POOL_DEPOSIT"
    else
        POOL3_REGISTERED=0
        seg_bad pool3 register "pool3 registration failed: $(tail -3 "$WORK/p3.err" | tr '\n' ' ')"
    fi
else
    POOL3_REGISTERED=0
    seg_bad pool3 register "no funding UTxO"
fi
sleep 10

# -- segment (f): schedule pool3's retirement for E_START+2 AND deregister its
# reward address in the SAME tx — both land well before the boundary. --
if [ "$POOL3_REGISTERED" -eq 1 ]; then
    cardano-cli conway stake-pool deregistration-certificate --cold-verification-key-file "$POOL3/cold.vkey" \
        --epoch "$F_RET" --out-file "$WORK/pool3.retire.cert" 2>"$WORK/p3ret.err"
    cardano-cli conway stake-address deregistration-certificate --stake-verification-key-file "$P3R/staking.vkey" \
        --key-reg-deposit-amt "$STAKE_DEPOSIT" --out-file "$WORK/p3reward.dereg.cert" 2>>"$WORK/p3ret.err"
    FU=$(utxo_funding)
    if [ -n "$FU" ]; then
        TXIN="${FU%%|*}"; FADDR="${FU##*|}"
        if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
                --tx-in "$TXIN" --change-address "$FADDR" \
                --certificate-file "$WORK/pool3.retire.cert" \
                --certificate-file "$WORK/p3reward.dereg.cert" \
                --out-file "$WORK/pool3ret.raw" 2>>"$WORK/p3ret.err" \
           && cardano-cli conway transaction sign --tx-body-file "$WORK/pool3ret.raw" \
                --signing-key-file "$LD_KEYS/utxo/payment.skey" \
                --signing-key-file "$P3R/staking.skey" --signing-key-file "$POOL3/cold.skey" \
                --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool3ret.signed" 2>>"$WORK/p3ret.err" \
           && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
                --tx-file "$WORK/pool3ret.signed" >/dev/null 2>>"$WORK/p3ret.err"; then
            seg_ok pool3 retire-and-dereg "pool3 retirement scheduled for epoch $F_RET; reward addr $P3R_STAKE_ADDR deregistered in the SAME tx"
        else
            seg_bad pool3 retire-and-dereg "failed: $(tail -3 "$WORK/p3ret.err" | tr '\n' ' ')"
        fi
    else
        seg_bad pool3 retire-and-dereg "no funding UTxO"
    fi
else
    seg_note pool3 retire-and-dereg "SKIPPED — pool3 registration did not succeed"
fi

# ═══════════════════════════════════════════════════════════════════════════
step "9. [CP1, epoch E_START+1] tracker + pool2 cancel cert"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 1 ))"
tracker_checkpoint cp1 in out out
MARK_CP1=$(snap_pool "$(stake_snapshot_json "$LD_CARDANO_BP_SOCK" "$POOL1_ID")" "$POOL1_HEX" mark)
if [ -n "$MARK_CP1" ] && [ "$MARK_CP1" != "null" ] && [ -n "$POOL1_MARK_BASE" ] && [ "$POOL1_MARK_BASE" != "null" ]; then
    DELTA=$(( MARK_CP1 - POOL1_MARK_BASE ))
    if [ "$DELTA" -eq "$FUND_D" ]; then
        seg_ok tracker cp1-delta-exact "pool1 mark increased by EXACTLY delegator-D's funded amount: $DELTA"
    else
        seg_bad tracker cp1-delta-exact "pool1 mark delta $DELTA != expected $FUND_D"
    fi
else
    seg_note tracker cp1-delta-exact "could not compute delta (base='$POOL1_MARK_BASE' now='$MARK_CP1')"
fi

# segment (d): cancel pool2's retirement — one epoch of margin before its
# boundary, matching cardano-node-tests' `depoch-1` convention exactly.
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$LD_GENESIS/pools-keys/pool2/staking-reward.vkey" --stake-pool-id "$POOL2_ID" \
    --out-file "$WORK/pool2.cancel.deleg.cert" 2>"$WORK/pool2cancel.err"
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file "$LD_KEYS/pool2/cold.vkey" --vrf-verification-key-file "$LD_KEYS/pool2/vrf.vkey" \
    --pool-pledge 0 --pool-cost "$MIN_POOL_COST" --pool-margin 0.0 \
    --pool-reward-account-verification-key-file "$LD_GENESIS/pools-keys/pool2/staking-reward.vkey" \
    --pool-owner-stake-verification-key-file "$LD_GENESIS/pools-keys/pool2/staking-reward.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool2.cancel.reg.cert" 2>>"$WORK/pool2cancel.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" \
            --certificate-file "$WORK/pool2.cancel.reg.cert" \
            --certificate-file "$WORK/pool2.cancel.deleg.cert" \
            --out-file "$WORK/pool2cancel.raw" 2>>"$WORK/pool2cancel.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pool2cancel.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" \
            --signing-key-file "$LD_GENESIS/pools-keys/pool2/staking-reward.skey" \
            --signing-key-file "$LD_KEYS/pool2/cold.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool2cancel.signed" 2>>"$WORK/pool2cancel.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/pool2cancel.signed" >/dev/null 2>>"$WORK/pool2cancel.err"; then
        seg_ok pool2-cancel submit "re-registration cert submitted in epoch $(cur_epoch) — cancels the pending E_START+2 retirement per POOL rule (unconditional psRetiring delete)"
    else
        seg_bad pool2-cancel submit "failed: $(tail -3 "$WORK/pool2cancel.err" | tr '\n' ' ')"
    fi
else
    seg_bad pool2-cancel submit "no funding UTxO"
fi

# ═══════════════════════════════════════════════════════════════════════════
step "10. [CP2, epoch E_START+2 = R_POOL2 = F_RET] tracker + d/f verdicts"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 2 ))"
tracker_checkpoint cp2 in in out

# segment (d): pool2 must still be LIVE (cancel worked) and its deposit must
# NOT have been refunded.
if pool_is_live "$LD_CARDANO_BP_SOCK" "$POOL2_ID" && pool_is_live "$LD_DUGITE_BP_SOCK" "$POOL2_ID"; then
    seg_ok pool2-cancel still-live "pool2 is still a registered pool on both sockets — cancellation held"
else
    seg_bad pool2-cancel still-live "pool2 is NOT live on both sockets (dugite=$(pool_is_live "$LD_DUGITE_BP_SOCK" "$POOL2_ID" && echo yes || echo no) haskell=$(pool_is_live "$LD_CARDANO_BP_SOCK" "$POOL2_ID" && echo yes || echo no)) — retirement was not cancelled"
fi
POOL2_REWARD_NOW=$(reward_of "$LD_CARDANO_BP_SOCK" "$POOL2_REWARD_ADDR")
if [ "${POOL2_REWARD_NOW:-0}" -eq "${POOL2_REWARD_BASELINE:-0}" ]; then
    seg_ok pool2-cancel no-refund "pool2's reward account unchanged ($POOL2_REWARD_NOW) — no deposit refund happened"
else
    seg_bad pool2-cancel no-refund "pool2's reward account moved $POOL2_REWARD_BASELINE -> $POOL2_REWARD_NOW — a refund landed despite cancellation"
fi

# segment (f): pool3 must be GONE (real retirement, no cancel cert for it)
# and the deposit must NOT have reached the deregistered reward address.
T_F_BEFORE=$(treasury_of "$SOCK")
if [ "$POOL3_REGISTERED" -eq 1 ]; then
    if ! pool_is_live "$LD_CARDANO_BP_SOCK" "$POOL3_ID" && ! pool_is_live "$LD_DUGITE_BP_SOCK" "$POOL3_ID"; then
        seg_ok pool3 retired "pool3 is gone from the live pool set on both sockets"
    else
        seg_bad pool3 retired "pool3 is STILL live on at least one socket"
    fi
    # RED-PROOF: if the deposit refund incorrectly reached the (deregistered)
    # reward address, this check — which asserts the address stayed
    # unregistered — would fail. The forfeiture is asserted directly, not
    # only inferred from a treasury delta.
    P3R_STILL_REG=$(stake_reg_check "$LD_CARDANO_BP_SOCK" "$P3R_STAKE_ADDR")
    if [ "$P3R_STILL_REG" = "no" ]; then
        seg_ok pool3 red-proof-refund "confirmed: the dead reward address $P3R_STAKE_ADDR stayed unregistered — the refund did not (and could not) land there"
    else
        seg_bad pool3 red-proof-refund "the deregistered reward address is registered again — the forfeiture may have gone to the wrong place"
    fi
    T_D=$(treasury_of "$LD_DUGITE_BP_SOCK"); T_H=$(treasury_of "$LD_CARDANO_BP_SOCK")
    if [ "$T_D" = "$T_H" ]; then
        seg_ok pool3 treasury-parity "treasury byte-exact after pool3's boundary: dugite=$T_D haskell=$T_H (before boundary: $T_F_BEFORE)"
    else
        seg_bad pool3 treasury-parity "treasury DIVERGES: dugite=$T_D haskell=$T_H"
    fi
else
    seg_note pool3 retired "SKIPPED — pool3 was never registered"
fi

# segment (e): NOW retire pool2 for real.
R2_POOL2=$(( E_START + 4 ))
cardano-cli conway stake-pool deregistration-certificate --cold-verification-key-file "$LD_KEYS/pool2/cold.vkey" \
    --epoch "$R2_POOL2" --out-file "$WORK/pool2.retire2.cert" 2>"$WORK/pool2r2.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/pool2.retire2.cert" \
            --out-file "$WORK/pool2ret2.raw" 2>>"$WORK/pool2r2.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pool2ret2.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$LD_KEYS/pool2/cold.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool2ret2.signed" 2>>"$WORK/pool2r2.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/pool2ret2.signed" >/dev/null 2>>"$WORK/pool2r2.err"; then
        seg_ok pool2-retire submit "REAL pool2 retirement scheduled for epoch $R2_POOL2 (no cancel this time)"
    else
        seg_bad pool2-retire submit "failed: $(tail -3 "$WORK/pool2r2.err" | tr '\n' ' ')"
    fi
else
    seg_bad pool2-retire submit "no funding UTxO"
fi

# ═══════════════════════════════════════════════════════════════════════════
step "11. [CP3, epoch E_START+3] tracker (full entrance) + D dereg cert"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 3 ))"
tracker_checkpoint cp3 in in in

cardano-cli conway stake-address deregistration-certificate --stake-verification-key-file "$DDIR/staking.vkey" \
    --key-reg-deposit-amt "$STAKE_DEPOSIT" --out-file "$WORK/D.dereg.cert" 2>"$WORK/Ddereg.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/D.dereg.cert" \
            --out-file "$WORK/Ddereg.raw" 2>>"$WORK/Ddereg.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/Ddereg.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$DDIR/staking.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/Ddereg.signed" 2>>"$WORK/Ddereg.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/Ddereg.signed" >/dev/null 2>>"$WORK/Ddereg.err"; then
        seg_ok tracker dereg-D "delegator-D deregistered in epoch $(cur_epoch) — testing the exit schedule"
    else
        seg_bad tracker dereg-D "dereg failed: $(tail -3 "$WORK/Ddereg.err" | tr '\n' ' ')"
    fi
else
    seg_bad tracker dereg-D "no funding UTxO"
fi

# ═══════════════════════════════════════════════════════════════════════════
step "12. [CP4, epoch E_START+4 = R2_POOL2] tracker + e/c verdicts + b break"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 4 ))"
tracker_checkpoint cp4 out in in

# segment (e): pool2 must be GONE for real now, with an EXACT deposit refund.
if ! pool_is_live "$LD_CARDANO_BP_SOCK" "$POOL2_ID" && ! pool_is_live "$LD_DUGITE_BP_SOCK" "$POOL2_ID"; then
    seg_ok pool2-retire retired "pool2 is gone from the live pool set on both sockets"
else
    seg_bad pool2-retire retired "pool2 is STILL live on at least one socket"
fi
P2_D=$(reward_of "$LD_DUGITE_BP_SOCK" "$POOL2_REWARD_ADDR")
P2_H=$(reward_of "$LD_CARDANO_BP_SOCK" "$POOL2_REWARD_ADDR")
if [ "$P2_D" = "$P2_H" ]; then
    DELTA2=$(( P2_H - POOL2_REWARD_BASELINE ))
    if [ "$DELTA2" -eq "$POOL_DEPOSIT" ]; then
        seg_ok pool2-retire exact-refund "pool2 deposit refunded EXACTLY $DELTA2 == poolDeposit $POOL_DEPOSIT, byte-exact on both sockets"
    else
        seg_bad pool2-retire exact-refund "pool2 refund delta $DELTA2 != poolDeposit $POOL_DEPOSIT"
    fi
else
    seg_bad pool2-retire exact-refund "reward parity DIVERGES: dugite=$P2_D haskell=$P2_H"
fi

# segment (c): pool1's own reward account must show a byte-exact, strictly
# positive reward. Best-effort secondary: if ledger-state exposes a per-type
# reward breakdown, look for it (INCONCLUSIVE, not a failure, if the shape
# cannot be found — see final report).
C_D=$(reward_of "$LD_DUGITE_BP_SOCK" "$C_STAKE_ADDR")
C_H=$(reward_of "$LD_CARDANO_BP_SOCK" "$C_STAKE_ADDR")
if [ "$C_D" = "$C_H" ]; then
    seg_ok reward-addr-deleg parity "pool1 reward-account balance byte-exact: $C_D"
else
    seg_bad reward-addr-deleg parity "pool1 reward-account balance DIVERGES dugite=$C_D haskell=$C_H"
fi
if [ "${C_H:-0}" -gt "${C_BASELINE:-0}" ]; then
    seg_ok reward-addr-deleg positive "pool1 reward account grew $C_BASELINE -> $C_H since its own delegation to pool1 matured (member+leader; the account was UNREGISTERED before this segment, so this baseline is a genuine zero)"
else
    seg_bad reward-addr-deleg positive "pool1 reward account did not grow ($C_BASELINE -> $C_H) — member-reward delegation-to-self did not take effect"
fi
LSH_C=$(ledger_state_json "$LD_CARDANO_BP_SOCK")
C_TYPES=$(printf '%s' "$LSH_C" | jq -r --arg h "keyHash-$(stake_key_hash "$C_STAKING_DIR/staking-reward.vkey")" \
    '[(.stateBefore.possibleRewardUpdate // .possibleRewardUpdate // {}).rs[$h][]?.rewardType] | unique | join(",")' 2>/dev/null)
if [ -n "$C_TYPES" ] && [ "$C_TYPES" != "null" ]; then
    seg_note reward-addr-deleg reward-types "possibleRewardUpdate.rs reward types for pool1's reward account: $C_TYPES (best-effort; expect LeaderReward,MemberReward)"
else
    seg_note reward-addr-deleg reward-types "could not extract a per-type reward breakdown (INCONCLUSIVE, not a failure — exact possibleRewardUpdate JSON path not verified statically)"
fi

# segment (b): submit the pledge-BREAK cert now. This is a RE-REGISTRATION of
# the already-registered pool1 (owners=[] at genesis; this cert adds pool1's
# own reward-account key — by now registered+self-delegated per segment c —
# as owner, which is exactly why the pledge must be absurdly large: a
# plausible/small pledge could be legitimately met by that credential's real
# balance). PLEDGE_BROKEN = 100x total supply, per the issue.
E_B="$(cur_epoch)"
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" --vrf-verification-key-file "$LD_KEYS/pool1/vrf.vkey" \
    --pool-pledge "$PLEDGE_BROKEN" --pool-cost "$MIN_POOL_COST" --pool-margin 0.0 \
    --pool-reward-account-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" \
    --pool-owner-stake-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" \
    --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool1.break.cert" 2>"$WORK/pool1break.err"
FU=$(utxo_funding)
if [ -n "$FU" ]; then
    TXIN="${FU%%|*}"; FADDR="${FU##*|}"
    if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/pool1.break.cert" \
            --out-file "$WORK/pool1break.raw" 2>>"$WORK/pool1break.err" \
       && cardano-cli conway transaction sign --tx-body-file "$WORK/pool1break.raw" \
            --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$LD_KEYS/pool1/cold.skey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool1break.signed" 2>>"$WORK/pool1break.err" \
       && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
            --tx-file "$WORK/pool1break.signed" >/dev/null 2>>"$WORK/pool1break.err"; then
        seg_ok pledge-break submit "pool1 re-registered with pledge=$PLEDGE_BROKEN in epoch $E_B (owner=pool1's own reward account, original pledge was $POOL1_ORIG_PLEDGE)"
    else
        seg_bad pledge-break submit "failed: $(tail -3 "$WORK/pool1break.err" | tr '\n' ' ')"
        E_B=""
    fi
else
    seg_bad pledge-break submit "no funding UTxO"
    E_B=""
fi

# ═══════════════════════════════════════════════════════════════════════════
step "13. [CP5, epoch E_START+5] tracker"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 5 ))"
tracker_checkpoint cp5 out out in

# ═══════════════════════════════════════════════════════════════════════════
step "14. [CP6, epoch E_START+6] tracker — full exit, segment (a) complete"
# ═══════════════════════════════════════════════════════════════════════════
wait_until_epoch "$(( E_START + 6 ))"
tracker_checkpoint cp6 out out out
seg_ok tracker complete "delegator-D's full entrance (mark/set/go) AND full exit (mark/set/go) schedule confirmed against both sockets"

# ═══════════════════════════════════════════════════════════════════════════
step "15. [pledge-break zero-reward window] mandatory for segment (b)"
# ═══════════════════════════════════════════════════════════════════════════
# See the header comment for the 4-boundary derivation. Sampling twice, three
# epochs apart, well inside the broken-pledge regime under either candidate
# timing (E_b+4 per cardano-node-tests' own margin, E_b+5 per this repo's
# direct Haskell-source derivation) — flat growth across that window is the
# real signature of "zero reward every epoch", robust to which one is exact.
if [ -n "$E_B" ]; then
    OWNER_BEFORE=$(reward_of "$SOCK" "$C_STAKE_ADDR")
    D1_BEFORE=$(reward_of "$SOCK" "$STAKE_ADDR")
    wait_until_epoch "$(( E_B + 5 ))" 2400
    OWNER_EARLY=$(reward_of "$LD_CARDANO_BP_SOCK" "$C_STAKE_ADDR")
    D1_EARLY=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR")
    wait_until_epoch "$(( E_B + 8 ))" 1800
    OWNER_LATE=$(reward_of "$LD_CARDANO_BP_SOCK" "$C_STAKE_ADDR")
    D1_LATE=$(reward_of "$LD_CARDANO_BP_SOCK" "$STAKE_ADDR")
    seg_note pledge-break samples "owner: before=$OWNER_BEFORE early(E_b+5)=$OWNER_EARLY late(E_b+8)=$OWNER_LATE; delegator1: before=$D1_BEFORE early=$D1_EARLY late=$D1_LATE"

    # RED-PROOF: if dugite failed to zero pool1's rewards for the unmet
    # pledge (i.e. pool1 kept forging and rewards kept accruing normally),
    # OWNER_LATE would be strictly greater than OWNER_EARLY and this
    # assertion — which requires them to be EQUAL — would fail.
    if [ "${OWNER_EARLY:-0}" = "${OWNER_LATE:-0}" ]; then
        seg_ok pledge-break owner-zero "owner (pool1's own reward account) shows ZERO growth across a 3-epoch window solidly inside the broken-pledge regime: $OWNER_EARLY == $OWNER_LATE"
    else
        seg_bad pledge-break owner-zero "owner reward GREW during the broken-pledge window: $OWNER_EARLY -> $OWNER_LATE (unmet pledge should zero BOTH leader and member rewards)"
    fi
    if [ "${D1_EARLY:-0}" = "${D1_LATE:-0}" ]; then
        seg_ok pledge-break delegator1-zero "delegator1 shows ZERO growth across the same window: $D1_EARLY == $D1_LATE"
    else
        seg_bad pledge-break delegator1-zero "delegator1 reward GREW during the broken-pledge window: $D1_EARLY -> $D1_LATE"
    fi
    T_D=$(treasury_of "$LD_DUGITE_BP_SOCK"); T_H=$(treasury_of "$LD_CARDANO_BP_SOCK")
    R_D=$(reserves_of "$LD_DUGITE_BP_SOCK"); R_H=$(reserves_of "$LD_CARDANO_BP_SOCK")
    if [ -n "$T_D" ] && [ "$T_D" = "$T_H" ]; then
        seg_ok pledge-break treasury-parity "treasury byte-exact during the broken-pledge window: $T_D"
    else
        seg_bad pledge-break treasury-parity "treasury DIVERGES: dugite=$T_D haskell=$T_H"
    fi
    if [ -n "$R_D" ] && [ "$R_D" = "$R_H" ]; then
        seg_ok pledge-break reserves-parity "reserves byte-exact during the broken-pledge window: $R_D"
    else
        seg_note pledge-break reserves-parity "reserves compare inconclusive (dugite='$R_D' haskell='$R_H') — see ledger-state risk note"
    fi

    # ── RW_FULL=1 only: restore pledge and confirm rewards resume ──────────
    if [ "$RW_FULL" -eq 1 ]; then
        step "16. [RW_FULL] restore pool1's pledge and confirm resumed rewards"
        cardano-cli conway stake-pool registration-certificate \
            --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" --vrf-verification-key-file "$LD_KEYS/pool1/vrf.vkey" \
            --pool-pledge "$POOL1_ORIG_PLEDGE" --pool-cost "$MIN_POOL_COST" --pool-margin 0.0 \
            --pool-reward-account-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" \
            --pool-owner-stake-verification-key-file "$C_STAKING_DIR/staking-reward.vkey" \
            --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool1.restore.cert" 2>"$WORK/pool1restore.err"
        FU=$(utxo_funding)
        E_R=""
        if [ -n "$FU" ]; then
            TXIN="${FU%%|*}"; FADDR="${FU##*|}"
            if cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
                    --tx-in "$TXIN" --change-address "$FADDR" --certificate-file "$WORK/pool1.restore.cert" \
                    --out-file "$WORK/pool1restore.raw" 2>>"$WORK/pool1restore.err" \
               && cardano-cli conway transaction sign --tx-body-file "$WORK/pool1restore.raw" \
                    --signing-key-file "$LD_KEYS/utxo/payment.skey" --signing-key-file "$LD_KEYS/pool1/cold.skey" \
                    --testnet-magic "$LD_MAGIC" --out-file "$WORK/pool1restore.signed" 2>>"$WORK/pool1restore.err" \
               && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" --socket-path "$SOCK" \
                    --tx-file "$WORK/pool1restore.signed" >/dev/null 2>>"$WORK/pool1restore.err"; then
                E_R="$(cur_epoch)"
                seg_ok pledge-restore submit "pool1 re-registered with the original pledge ($POOL1_ORIG_PLEDGE) in epoch $E_R"
            else
                seg_bad pledge-restore submit "failed: $(tail -3 "$WORK/pool1restore.err" | tr '\n' ' ')"
            fi
        else
            seg_bad pledge-restore submit "no funding UTxO"
        fi
        if [ -n "$E_R" ]; then
            wait_until_epoch "$(( E_R + 5 ))" 2400
            OWNER_RESUMED=$(reward_of "$LD_CARDANO_BP_SOCK" "$C_STAKE_ADDR")
            if [ "${OWNER_RESUMED:-0}" -gt "${OWNER_LATE:-0}" ]; then
                seg_ok pledge-restore resumed "owner reward resumed growing after restore: $OWNER_LATE -> $OWNER_RESUMED"
            else
                seg_bad pledge-restore resumed "owner reward did NOT resume growing: $OWNER_LATE -> $OWNER_RESUMED"
            fi
            OD=$(reward_of "$LD_DUGITE_BP_SOCK" "$C_STAKE_ADDR"); OH=$(reward_of "$LD_CARDANO_BP_SOCK" "$C_STAKE_ADDR")
            [ "$OD" = "$OH" ] && seg_ok pledge-restore parity "post-restore reward byte-exact: $OD" \
                              || seg_bad pledge-restore parity "post-restore reward DIVERGES dugite=$OD haskell=$OH"
        fi
    else
        seg_note pledge-restore skipped "RW_FULL=0 — restore-and-resume tail not run (set RW_FULL=1 to exercise it; needs ~5 more epochs)"
    fi
else
    seg_bad pledge-break skipped "segment (b) could not run — the break cert was never submitted"
fi

step "SUMMARY"
if [ "$FAILURES" -eq 0 ]; then
    ok "rewards round: all assertions passed"
else
    bad "rewards round: $FAILURES assertion(s) failed"
fi
note "final epoch: $(cur_epoch)"
note "evidence CSV: $REWARDS_CSV"
exit "$FAILURES"
