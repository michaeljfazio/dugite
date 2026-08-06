#!/usr/bin/env bash
# Two-forger round: forks, slot battles, chain selection, rollback.      (#957)
#
# WHY THIS ROUND EXISTS
# ---------------------
# The default devnet has exactly ONE forger. Chain selection under contention,
# slot battles, competing-chain rollback and orphan handling are therefore
# STRUCTURALLY unreachable — no round of any duration can exercise them. #763
# established that offline replay cannot validate the rollback path either, so
# before this round those paths were validated nowhere prior to release, in a
# node whose stated posture is crash-don't-diverge.
#
# TOPOLOGY
#
#     dugite-bp(pool1) ──┐                   ┌── cardano-bp(pool2)
#          :3001         ├── dugite-relay ───┤        :3003
#                        │      :3002        │
#                        └── cardano-arbiter ┘
#                                :3004
#
# cardano-arbiter is a THIRD cardano-node, non-forging, peered directly with
# both producers. It is the independent Haskell oracle for "which block won".
# Without it the round could only compare dugite-bp against dugite-relay — two
# dugite processes agreeing proves dugite is self-consistent, not that its Praos
# tiebreaker matches cardano-node's. cardano-bp cannot arbitrate either: it is
# one of the two contestants.
#
# WHAT IS SAFE TO ASSERT (oracle-verified against ouroboros-consensus
# release-ouroboros-consensus-3.0.1.0, which cardano-node 11.0.1 resolves to)
#
#   comparePraos compares, in order:
#     1. blockNo            — strictly longer ALWAYS wins, independent of below
#     2. same issuer AND same slot  -> higher opcert issue number
#     3. otherwise VRF value, LOWER wins — but ONLY when `vrfArmed`
#   Conway hardcodes `RestrictedVRFTiebreaker 5` (not configurable), so the VRF
#   tiebreak is DISARMED when the competing blocks' slots are >5 apart, and
#   comparePraos returns ShouldNotSwitch EQ. ChainSel drops every
#   non-ShouldSwitch verdict, so the incumbent is kept — which looks like
#   first-seen-wins (upstream calls this the "Frankfurt problem").
#
#   => SAFE: assert that every node's tip CONVERGES to the same (blockNo, hash).
#      That relies only on rule 1, which is unconditional.
#   => NOT SAFE: assert a same-height standoff resolves quickly, or that
#      battles never happen. At f=0.5 with two forgers, same-blockNo collisions
#      are expected and frequent, and a >5-slot-apart standoff is a real
#      possibility until either chain extends.
#
# Usage:
#   ./two-forger-round.sh                 # full round (~8 min)
#   TF_DURATION=600 ./two-forger-round.sh # longer observation window
#   TF_SKIP_SETUP=1 ./two-forger-round.sh # reuse a running two-forger devnet

set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2

DURATION="${TF_DURATION:-360}"
SKIP_SETUP="${TF_SKIP_SETUP:-0}"
FAILURES=0

step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }

if [ "$SKIP_SETUP" -eq 0 ]; then
    step "setup + run (two-forger mode)"
    ./stop.sh >/dev/null 2>&1
    LD_TWO_FORGERS=1 LD_POOL2_STAKE_PCT="${TF_POOL2_PCT:-40}" ./setup.sh >/dev/null 2>&1 \
        || { echo "SETUP FAILED"; exit 2; }
    # NOT `./run.sh | tail || ...` — a pipeline's exit status is the LAST
    # command's, so `tail` (always 0) masks a failed run.sh entirely. That is
    # the documented `just check | tail` trap, and it bit here: run.sh reported
    # "Socket cbp.sock did not become ready within 120s" and died, the round
    # sailed past it, and every assertion afterwards ran against a devnet that
    # was missing a node.
    if ! ./run.sh > "state/two-forger-run.log" 2>&1; then
        echo "RUN FAILED — last 20 lines:"
        tail -20 "state/two-forger-run.log"
        exit 2
    fi
    tail -12 "state/two-forger-run.log"
fi

. ./lib/common.sh
set +e

[ -f "$LD_GENESIS/.two-forgers" ] || {
    echo "REFUSING TO RUN: $LD_GENESIS/.two-forgers is absent, so this is a"
    echo "single-forger devnet. Every assertion below would be vacuous."
    exit 2
}

# ---------------------------------------------------------------- helpers ----
tip_json() { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null; }
tip_field() { tip_json "$1" | jq -r "$2 // empty" 2>/dev/null; }

# All four observers; the arbiter is only present in this mode.
SOCKS=("$LD_DUGITE_BP_SOCK" "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK" "$LD_CARDANO_ARBITER_SOCK")
NAMES=(dugite-bp dugite-relay cardano-bp cardano-arbiter)

step "1. all four nodes up (two producers + relay + Haskell arbiter)"
for i in "${!SOCKS[@]}"; do
    for _ in $(seq 1 60); do [ -S "${SOCKS[$i]}" ] && break; sleep 2; done
    if [ -S "${SOCKS[$i]}" ]; then
        ok "${NAMES[$i]} socket up"
    else
        bad "${NAMES[$i]} socket never appeared (${SOCKS[$i]})"
    fi
done
[ "$FAILURES" -gt 0 ] && { echo "cannot proceed without all four nodes"; exit 1; }

step "1b. the arbiter must actually see BOTH producers"
# Assert it; do not assume it. The arbiter was first given a copy of
# cardano-bp's config, whose peer targets are all 1 (that node needs exactly one
# upstream, the relay). It therefore established exactly ONE connection —
# only ever to 3001, never even attempting 3003 — so the "independent
# arbiter" saw a single producer and could not arbitrate anything. The round
# still reported PASS.
ARB_PID_F="$LD_STATE/cardano-arbiter.pid"
ARB_PEERS=0
if [ -f "$ARB_PID_F" ]; then
    for _ in $(seq 1 30); do
        ARB_PEERS=$(lsof -p "$(cat "$ARB_PID_F")" -iTCP -a -P -n 2>/dev/null \
                    | grep -c 'ESTABLISHED' || true)
        [ "${ARB_PEERS:-0}" -ge 2 ] && break
        sleep 2
    done
fi
if [ "${ARB_PEERS:-0}" -ge 2 ]; then
    ok "arbiter holds $ARB_PEERS established connections (both producers)"
else
    bad "arbiter has only ${ARB_PEERS:-0} established connection(s) — it cannot arbitrate between two producers, so every convergence verdict below would be one-sided"
fi

step "2. observation window (${DURATION}s) — let both pools forge and contend"
END=$(( $(date +%s) + DURATION ))
SAMPLES=0
CONVERGED_SAMPLES=0
LAST_DIVERGENCE=""
while [ "$(date +%s)" -lt "$END" ]; do
    sleep 15
    SAMPLES=$((SAMPLES + 1))
    declare -a TIPS=()
    for s in "${SOCKS[@]}"; do
        TIPS+=("$(tip_field "$s" .hash)")
    done
    # Convergence is judged on the ARBITER's view as reference.
    same=1
    for t in "${TIPS[@]}"; do
        [ -z "$t" ] && { same=0; break; }
        [ "$t" != "${TIPS[0]}" ] && same=0
    done
    if [ "$same" -eq 1 ]; then
        CONVERGED_SAMPLES=$((CONVERGED_SAMPLES + 1))
    else
        LAST_DIVERGENCE="$(for i in "${!TIPS[@]}"; do printf '%s=%.8s ' "${NAMES[$i]}" "${TIPS[$i]:-none}"; done)"
    fi
done
note "$SAMPLES samples, $CONVERGED_SAMPLES with all four tips identical"
[ -n "$LAST_DIVERGENCE" ] && note "last divergent sample: $LAST_DIVERGENCE"

step "3. both pools actually forged (the round is vacuous otherwise)"
# Count ONLY the real forge events.
#
# The first version matched `TraceForgedBlock|forge:` and reported 955 forges
# from dugite-bp across a 300-second window on a 1-second-slot chain — a
# physically impossible number, because `forge:` is a log PREFIX that appears on
# many unrelated lines. The assertion passed on a meaningless metric, which is
# the failure shape this backlog exists to delete. awk counts exact matches and
# cannot pick up a bare count-plus-fallback the way `grep -c || echo 0` does.
DBP_FORGED=$(awk '/TraceForgedBlock/ {c++} END{print c+0}' "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)
CBP_FORGED=$(awk '/Forge\.Loop\.AdoptedBlock/ {c++} END{print c+0}' "$LD_LOGS/cardano-bp.log" 2>/dev/null || echo 0)
note "forge events: dugite-bp=$DBP_FORGED cardano-bp=$CBP_FORGED"
if [ "${DBP_FORGED:-0}" -ge 3 ] && [ "${CBP_FORGED:-0}" -ge 3 ]; then
    ok "both pools forged (dugite-bp=$DBP_FORGED, cardano-bp=$CBP_FORGED) — contention was real"
else
    bad "two-forger round did not achieve two forgers (dugite-bp=$DBP_FORGED cardano-bp=$CBP_FORGED); every assertion below would be vacuous"
fi

step "3b. dugite-bp is NOT stranded on its own self-forged fork (#1057 regression)"
# #1057: a BP that won a slot before its initial sync completed forged blocks
# 0..8 on Origin and then NEVER adopted a peer block again. BlockFetch declined
# the canonical chain's block 0 (genesis is not a "stored block"), the ledger
# froze at the BP's own last forged block, ChainSync's forecast-horizon park
# dropped every peer on a loop, and the node stayed wedged for the process
# lifetime.
#
# Step 5's convergence check does fail on this, but it reports "tips differ",
# which reads as ordinary propagation lag. This asserts the SIGNATURE directly:
# dugite-bp's tip block number must exceed the number of blocks it forged
# itself, i.e. it has adopted at least one block from somebody else.
DBP_TIP_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
ARB_TIP_BLK=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .block)
note "dugite-bp tip block=$DBP_TIP_BLK, arbiter tip block=$ARB_TIP_BLK, dugite-bp self-forged=$DBP_FORGED"
if [ -z "$DBP_TIP_BLK" ] || [ -z "$ARB_TIP_BLK" ]; then
    bad "could not read dugite-bp / arbiter tip — cannot evaluate the #1057 guard"
elif [ "$DBP_TIP_BLK" -le "${DBP_FORGED:-0}" ] && [ "$ARB_TIP_BLK" -gt "$DBP_TIP_BLK" ]; then
    bad "dugite-bp appears STRANDED ON ITS OWN FORK (#1057): tip block=$DBP_TIP_BLK is no higher than the $DBP_FORGED blocks it forged itself, while the arbiter is at block=$ARB_TIP_BLK. Check dugite-bp.log for repeated 'beyond forecast horizon ... disconnecting' against every peer with a frozen ledger tip"
else
    ok "dugite-bp has adopted blocks from the network (tip block=$DBP_TIP_BLK > self-forged=$DBP_FORGED, arbiter=$ARB_TIP_BLK)"
fi

step "4. slot battles / lost blocks observed"
# Haskell's own signal that it forged a valid block and lost the race.
# ouroboros-consensus documents this as rare-but-expected, at Error severity:
#   "We did not adopt the block we produced, but the block was valid."
DIDNT_ADOPT=$(grep -acE 'DidntAdoptBlock' "$LD_LOGS/cardano-bp.log" 2>/dev/null || true)
SWITCHED=$(grep -acE 'SwitchedToAFork' "$LD_LOGS/cardano-arbiter.log" 2>/dev/null || true)
SWITCHED_CBP=$(grep -acE 'SwitchedToAFork' "$LD_LOGS/cardano-bp.log" 2>/dev/null || true)
DUGITE_SWITCH=$(grep -acE 'chain switch|rollback complete' "$LD_LOGS/dugite-bp.log" 2>/dev/null || true)
note "cardano-bp DidntAdoptBlock=$DIDNT_ADOPT"
note "SwitchedToAFork: arbiter=$SWITCHED cardano-bp=$SWITCHED_CBP"
note "dugite-bp chain-switch/rollback lines=$DUGITE_SWITCH"

BATTLE_EVIDENCE=$(( DIDNT_ADOPT + SWITCHED + SWITCHED_CBP + DUGITE_SWITCH ))
if [ "$BATTLE_EVIDENCE" -gt 0 ]; then
    ok "contention observed ($BATTLE_EVIDENCE fork/battle events across the topology)"
else
    # Deliberately NOT a failure. Battles are probabilistic; a window with none
    # is a valid outcome, and failing on it would make the round flaky. What
    # WOULD be a defect is divergence, which step 5 tests unconditionally.
    note "no fork/battle events in this window — probabilistic, not a defect."
    note "  (increase TF_DURATION or TF_POOL2_PCT to raise the collision rate)"
fi

step "5. CONVERGENCE — every node agrees with the Haskell arbiter"
# Poll rather than sample once: a single read can catch the topology
# mid-propagation and manufacture a false divergence.
CONVERGED=0
for _ in $(seq 1 24); do
    A_HASH=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .hash)
    A_BLK=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .block)
    [ -z "$A_HASH" ] && { sleep 5; continue; }
    all_match=1
    for i in "${!SOCKS[@]}"; do
        h=$(tip_field "${SOCKS[$i]}" .hash)
        [ "$h" != "$A_HASH" ] && all_match=0
    done
    if [ "$all_match" -eq 1 ]; then CONVERGED=1; break; fi
    sleep 5
done
if [ "$CONVERGED" -eq 1 ]; then
    ok "all four nodes converged on the arbiter's tip: block=$A_BLK hash=${A_HASH:0:16}…"
else
    DETAIL=$(for i in "${!SOCKS[@]}"; do
        printf '%s=%s/%.12s ' "${NAMES[$i]}" \
            "$(tip_field "${SOCKS[$i]}" .block)" "$(tip_field "${SOCKS[$i]}" .hash)"
    done)
    bad "nodes did NOT converge within 120s: $DETAIL"
fi

step "6. no INVALID blocks anywhere (a fork must never mean a bad block)"
for log in cardano-bp cardano-arbiter; do
    INV=$(grep -acE 'AddBlockValidation\.InvalidBlock|ForgedInvalidBlock' \
        "$LD_LOGS/$log.log" 2>/dev/null || true)
    if [ "${INV:-0}" -eq 0 ]; then
        ok "$log: 0 invalid-block events"
    else
        bad "$log: $INV invalid-block event(s) — a dugite-forged block failed Haskell validation"
    fi
done

step "7. ledger parity with the arbiter after all the switching"
DBP_T=$(curl -s "localhost:$LD_DUGITE_BP_METRICS_PORT/metrics" 2>/dev/null | awk '/^dugite_treasury_lovelace /{print $2}')
DBP_R=$(curl -s "localhost:$LD_DUGITE_BP_METRICS_PORT/metrics" 2>/dev/null | awk '/^dugite_reserves_lovelace /{print $2}')
ARB=$(cardano-cli query ledger-state --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_CARDANO_ARBITER_SOCK" 2>/dev/null \
      | jq '.stateBefore.esChainAccountState // .esChainAccountState' 2>/dev/null)
ARB_T=$(printf '%s' "$ARB" | jq -r '.treasury // empty' 2>/dev/null)
ARB_R=$(printf '%s' "$ARB" | jq -r '.reserves // empty' 2>/dev/null)
note "dugite treasury=${DBP_T:-?} reserves=${DBP_R:-?}"
note "arbiter treasury=${ARB_T:-?} reserves=${ARB_R:-?}"
if [ -z "$ARB_T" ] || [ -z "$DBP_T" ]; then
    note "pot parity INCONCLUSIVE — could not read both sides (epoch 0 has no RUPD yet)"
elif [ "$DBP_T" = "$ARB_T" ] && [ "$DBP_R" = "$ARB_R" ]; then
    ok "pots byte-exact vs the Haskell arbiter (treasury=$DBP_T reserves=$DBP_R)"
else
    bad "POT DIVERGENCE after fork resolution: dugite T=$DBP_T R=$DBP_R vs arbiter T=$ARB_T R=$ARB_R"
fi

step "8. restart under contention — rejoin must cross a real fork"
# The standard Round 3 restart rejoins a chain only IT was extending, so the
# intersection is trivially its own tip. Here the OTHER forger keeps producing
# while dugite-bp is down, so the rejoin has to intersect a chain that moved on
# without it — the non-trivial case #957 asks for.
TIP_BEFORE=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
if [ -n "$TIP_BEFORE" ] && [ -f "$LD_STATE/dugite-bp.pid" ]; then
    kill "$(cat "$LD_STATE/dugite-bp.pid")" 2>/dev/null
    note "dugite-bp stopped (SIGTERM) at block $TIP_BEFORE; cardano-bp keeps forging"
    sleep 90
    ARB_MID=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .block)
    note "arbiter advanced to block ${ARB_MID:-?} while dugite-bp was down"
    ../../.claude/skills/devnet-validate/scripts/restart-dugite-bp.sh >/dev/null 2>&1
    # "Advanced past its old tip" is too weak a criterion: a node that rebuilt
    # its OWN fork also advances. The rejoin only counts if dugite-bp lands on
    # the SAME chain the rest of the network is on, so require its tip hash to
    # match the arbiter's — which is the whole reason the arbiter exists.
    REJOINED=0
    for _ in $(seq 1 36); do
        sleep 5
        TIP_AFTER=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
        DBP_H=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
        ARB_H=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .hash)
        if [ -n "$TIP_AFTER" ] && [ "${TIP_AFTER:-0}" -gt "${TIP_BEFORE:-0}" ] \
           && [ -n "$DBP_H" ] && [ "$DBP_H" = "$ARB_H" ]; then
            REJOINED=1; break
        fi
    done
    if [ "$REJOINED" -eq 1 ]; then
        ok "dugite-bp rejoined the network's chain across a fork it did not build: $TIP_BEFORE -> $TIP_AFTER (hash matches the arbiter)"
    else
        bad "dugite-bp did not rejoin the arbiter's chain within 180s (tip $TIP_BEFORE -> ${TIP_AFTER:-?}, dbp=${DBP_H:0:12} arbiter=${ARB_H:0:12}) — advancing on its OWN fork is not a rejoin"
    fi
    STALE=$(grep -acE 'stale intersection' "$LD_LOGS/dugite-bp.log" 2>/dev/null || true)
    if [ "${STALE:-0}" -eq 0 ]; then
        ok "0 stale-intersection warnings after rejoin"
    else
        note "$STALE stale-intersection line(s) — inspect if rejoin was slow"
    fi
else
    note "restart step SKIPPED — could not read a pre-restart tip or pidfile"
fi

step "9. FINAL convergence — the network must not be left partitioned"
# The single most important assertion, and the one the first version LACKED.
#
# Step 5 samples convergence BEFORE the restart. That first run passed step 5 at
# block 148 and then finished with dugite-bp=150, relay=176, cardano-bp=176,
# arbiter=149 — four nodes on three different chains — and still reported
# "all assertions passed", because nothing re-checked afterwards.
#
# A partition here is not cosmetic. Two producers that fork at genesis each
# build past `k` and can never adopt each other
# (`ChainSync intersection only at genesis ... ForkTooDeep`), so the devnet
# silently becomes two independent chains and every cross-node comparison in
# the whole gate is meaningless.
FINAL_CONVERGED=0
for _ in $(seq 1 36); do
    sleep 5
    declare -a FT=()
    for s in "${SOCKS[@]}"; do FT+=("$(tip_field "$s" .hash)"); done
    same=1
    for t in "${FT[@]}"; do
        [ -z "$t" ] && { same=0; break; }
        [ "$t" != "${FT[0]}" ] && same=0
    done
    [ "$same" -eq 1 ] && { FINAL_CONVERGED=1; break; }
done
if [ "$FINAL_CONVERGED" -eq 1 ]; then
    ok "final convergence: all four nodes on one chain, block=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .block)"
else
    DET=$(for i in "${!SOCKS[@]}"; do
        printf '%s=%s/%.10s ' "${NAMES[$i]}" \
            "$(tip_field "${SOCKS[$i]}" .block)" "$(tip_field "${SOCKS[$i]}" .hash)"
    done)
    bad "NETWORK LEFT PARTITIONED after 180s: $DET"
    echo "    A persistent split means the producers forked deeper than k=40 and"
    echo "    can no longer adopt each other. Check for"
    echo "    'intersection only at genesis' in logs/dugite-relay.log — if present,"
    echo "    the producers began forging before they were connected, which the"
    echo "    LD_GENESIS_DELAY bump in setup.sh exists to prevent."
fi

step "SUMMARY"
if [ "$FAILURES" -eq 0 ]; then
    ok "two-forger round: all assertions passed"
else
    bad "two-forger round: $FAILURES assertion(s) failed"
fi
note "final tips:"
for i in "${!SOCKS[@]}"; do
    printf '  %-16s block=%s hash=%.16s\n' "${NAMES[$i]}" \
        "$(tip_field "${SOCKS[$i]}" .block)" "$(tip_field "${SOCKS[$i]}" .hash)"
done
exit "$FAILURES"
