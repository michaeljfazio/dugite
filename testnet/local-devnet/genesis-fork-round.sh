#!/usr/bin/env bash
# genesis-fork-round.sh — #1057 reproduction and regression gate.
#
# Deterministically constructs the ONE scenario #1057 lives in: a node that
# already holds blocks of its own must adopt a canonical chain that diverges at
# GENESIS. On an unfixed node the adoption never happens — BlockFetch declines
# the peer's block 0 forever (genesis is never a "stored block"), the ledger never
# advances, ChainSync's forecast-horizon park times out, and every peer is
# dropped and re-dialled in a loop with no self-healing. It strands a block
# producer.
#
# WHY THIS ROUND HAD TO EXIST
#
# The first attempt at #1057 shipped with FOUR RED-proven unit tests and was
# still wrong: teaching `VolatileDB::switch_chain` to emit a genesis-rooted
# `SwitchPlan` moved the wedge to the ledger, which cannot roll back to Origin
# ("Rollback target outside LedgerSeq volatile window AND no canonical snapshot
# available"). The node ended up at block 4 while the network was at 84 — WORSE
# than unfixed. Nothing in the suite drove the storage plan through the ledger.
#
# The two-forger round does not cover it either: that run converged before any
# node needed a genesis-rooted ledger rollback, so it passed while the fix was
# broken. A RED-proven unit test bounds the FUNCTION, not the SYSTEM.
#
# CONSTRUCTION (deterministic, not incidental)
#
# The devnet's normal topology makes dugite-relay the BRIDGE:
#   dugite-bp(3001) -> relay(3002);  relay -> {3001, cardano-bp(3003)};
#   cardano-bp -> relay
# so cutting the relay's 3003 edge and cardano-bp's 3002 edge splits the network
# into two islands that share only genesis.
#
#   1. setup.sh (two-forger genesis, so both islands have a real forging pool).
#   2. Cut the bridge. Start the DUGITE island (bp + relay, peered with each
#      other) and the CARDANO island (cardano-bp + arbiter) separately. Each
#      builds its own chain from the same genesis.
#
#      dugite-bp is peered with the relay ON PURPOSE — it cannot forge without a
#      hot peer plus a ChainSync intersection.
#
#   3. Wait until both islands hold >= MIN_FORK_BLOCKS blocks.
#   4. Restore the bridge and restart the two nodes whose topology changed.
#   5. ASSERT dugite-bp converges on the Haskell chain's tip HASH.
#
# Step 5 asserts the tip HASH, not merely "advanced": a node that rebuilt its own
# fork also advances. Hash equality against cardano-bp is the only thing that
# distinguishes "rejoined the network" from "kept extending itself" — the same
# reasoning as two-forger-round step 8.
#
# EXPECTED RESULT ON AN UNFIXED NODE: FAIL. That is the point — this script is
# the reproduction. It should only pass once #1057 is genuinely fixed, which
# requires the ledger to re-initialise from genesis on a rollback-to-Origin (a
# full `init_fresh_ledger`; #989 deleted `reset_to_origin` because a partial
# in-place reset cannot be correct), plus a LedgerSeq re-anchor (#985).
#
# Usage:
#   ./genesis-fork-round.sh
#   GF_MIN_FORK_BLOCKS=3 ./genesis-fork-round.sh
#
# Terminal: tears the devnet down at the end.
#
# EARLIER FALSE NEGATIVE, recorded so it is not repeated: the first version of
# this round reported dugite-bp forging 0 blocks with 0 leadership checks — both
# fully isolated and paired with the relay — and concluded that a dugite BP cannot
# mint the first block of a chain. That conclusion was WRONG. `start_dugite` was
# omitting the --shelley-{kes,vrf,operational-certificate} triple, so dugite-bp ran
# as a plain relay with no forging keys. A node with no forging keys is
# indistinguishable, from the outside, from a node whose forge gates are blocking
# it: no forges, no leadership checks, no "Deferring forge" line either.
#
# The lesson generalises past this script: when a negative result says "the node
# refused to do X", first prove the node was CONFIGURED to do X.
#
# Usage:
#   ./genesis-fork-round.sh
#   GF_MIN_FORK_BLOCKS=3 ./genesis-fork-round.sh
#
# Terminal: tears the devnet down at the end.
#
# STATUS: THIS CONSTRUCTION DOES NOT YET WORK. Two attempts, both measured:
#
#   (a) dugite-bp fully isolated (empty topology): forged 0 blocks while
#       cardano-bp reached 43.
#   (b) dugite-bp peered with dugite-relay, the pair isolated from the cardano
#       island: forged 0 blocks, ZERO `TraceStartLeadershipCheck`, while
#       cardano-bp reached 60.
#
# In (b) the forge loop never even reached its leadership check, so the silent
# catch-up gate at the top of `try_forge_block_at` short-circuited every slot.
# Both dugite nodes start at Origin, so no peer can supply the non-Origin
# intersection the peer-connectivity gate wants, and `tip_slot` stays 0 — the
# dugite island cannot bootstrap its own chain at all. That is the deliberate
# Bug-A/Bug-G protection working: a dugite BP does not mint the first block of a
# chain.
#
# WHICH LEAVES AN OPEN QUESTION ON #1057: in the original occurrence dugite-bp
# DID forge `block_no=0` (slot 4) and its 9-block chain shared NO block with the
# peer's. Something let it past both gates. Until that mechanism is understood
# this round will keep reporting INCONCLUSIVE, and #1057's fix cannot be
# verified — which is exactly why the first fix attempt shipped broken.
#
# The round is checked in anyway because it (1) records these two measured
# negatives so they are not re-derived, and (2) refuses to emit a verdict when
# the scenario is unbuilt, instead of passing vacuously.

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2
. ./lib/common.sh

# ORDER IS LOAD-BEARING: lib/common.sh line 4 is `set -euo pipefail`, so relaxing
# errexit before sourcing it does nothing. A round that ABORTS instead of FAILING
# skips its own assertions and reports less than nothing — see #1044.
set +e
set +u
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

MIN_FORK_BLOCKS="${GF_MIN_FORK_BLOCKS:-5}"
CONVERGE_TIMEOUT="${GF_CONVERGE_TIMEOUT:-300}"
FORK_BUILD_TIMEOUT="${GF_FORK_BUILD_TIMEOUT:-300}"

FAILURES=0
INCONCLUSIVE=0
step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }
inconc() {
    printf '\033[0;33m[INCONCLUSIVE]\033[0m %s\n' "$*"
    INCONCLUSIVE=$((INCONCLUSIVE + 1))
}

DUGITE_BIN="${DUGITE_BIN:-$(cd ../.. && pwd)/target/release/dugite-node}"
[ -x "$DUGITE_BIN" ] || { echo "REFUSING TO RUN: dugite-node not found at $DUGITE_BIN"; exit 2; }
command -v cardano-node >/dev/null 2>&1 || { echo "REFUSING TO RUN: cardano-node not on PATH"; exit 2; }

tip_field() {
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null \
        | jq -r "$2 // empty" 2>/dev/null
}
forge_count() { awk '/TraceForgedBlock/ {c++} END{print c+0}' "$1" 2>/dev/null; }

# start_dugite <name> <port> <metrics> <sock> [forge-pool]
#
# `forge-pool` is REQUIRED for dugite-bp. Omitting the
# --shelley-{kes-key,vrf-key,operational-certificate} triple starts a plain RELAY
# with no forging keys: it forges nothing and runs no leadership checks, which
# looks identical to a node blocked by the forge gates.
#
# The first version of this round made exactly that mistake and concluded — wrongly
# — that "a dugite BP cannot mint the first block of a chain". It had never started
# a BP. The keys are validated for existence here so a missing keypair fails loudly
# instead of silently degrading the node to a relay and producing another false
# negative.
start_dugite() {
    local name="$1" port="$2" metrics="$3" sock="$4" pool="${5:-}"
    local forge_args=() f
    if [ -n "$pool" ]; then
        for f in kes.skey vrf.skey opcert.cert; do
            [ -s "$LD_KEYS/$pool/$f" ] || {
                echo "REFUSING TO RUN: forging key missing: $LD_KEYS/$pool/$f"
                exit 2
            }
        done
        forge_args=(
            --shelley-kes-key                 "$LD_KEYS/$pool/kes.skey"
            --shelley-vrf-key                 "$LD_KEYS/$pool/vrf.skey"
            --shelley-operational-certificate "$LD_KEYS/$pool/opcert.cert"
        )
    fi
    caffeinate_if_macos "$DUGITE_BIN" run \
        --config        "$LD_CONFIG/$name.config.json" \
        --topology      "$LD_CONFIG/$name.topology.json" \
        --database-path "$LD_STATE/$name.db" \
        --socket-path   "$sock" \
        --host-addr     127.0.0.1 \
        --port          "$port" \
        --metrics-port  "$metrics" \
        "${forge_args[@]+"${forge_args[@]}"}" \
        >> "$LD_LOGS/$name.log" 2>&1 &
    write_node_pidfile "$LD_STATE/$name.db" "$LD_STATE/$name.pid" \
        || echo $! > "$LD_STATE/$name.pid"
}

stop_pid_file() { # <pidfile>
    local pid
    pid=$(cat "$1" 2>/dev/null)
    [ -n "$pid" ] || return 0
    kill -TERM "$pid" 2>/dev/null
    # `kill` returning 0 does not prove death — poll.
    for _ in $(seq 1 60); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 1
    done
    return 1
}

# ── setup ──────────────────────────────────────────────────────────────────
step "setup (two-forger genesis)"
./stop.sh >/dev/null 2>&1
LD_TWO_FORGERS=1 ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
ok "genesis regenerated (two-forger)"

[ -f "$LD_GENESIS/.two-forgers" ] || {
    echo "REFUSING TO RUN: $LD_GENESIS/.two-forgers absent — cardano-bp would not be"
    echo "forging, so there would be no competing chain to diverge from."
    exit 2
}

RELAY_TOPO="$LD_CONFIG/dugite-relay.topology.json"
CBP_TOPO="$LD_CONFIG/cardano-bp.topology.json"
cp "$RELAY_TOPO" "$LD_STATE/relay.topology.real.json" || exit 2
cp "$CBP_TOPO"   "$LD_STATE/cbp.topology.real.json"   || exit 2

cleanup() {
    [ -f "$LD_STATE/relay.topology.real.json" ] && cp "$LD_STATE/relay.topology.real.json" "$RELAY_TOPO" 2>/dev/null
    [ -f "$LD_STATE/cbp.topology.real.json" ]   && cp "$LD_STATE/cbp.topology.real.json"   "$CBP_TOPO"   2>/dev/null
    return 0
}
trap cleanup EXIT

# ── step 1: cut the bridge, start both islands ─────────────────────────────
step "1. cut the bridge — two islands sharing only genesis"

# relay keeps ONLY its dugite-bp edge (drops cardano-bp:3003).
jq '.localRoots = [{"accessPoints":[{"address":"127.0.0.1","port":'"$LD_DUGITE_BP_PORT"'}],"advertise":false,"trustable":true,"valency":1}]' \
    "$LD_STATE/relay.topology.real.json" > "$RELAY_TOPO" || exit 2
# cardano-bp keeps NO dugite edge (drops relay:3002); the arbiter still dials it.
jq '.localRoots = []' "$LD_STATE/cbp.topology.real.json" > "$CBP_TOPO" 2>/dev/null \
    || echo '{"localRoots":[],"publicRoots":[],"useLedgerAfterSlot":-1}' > "$CBP_TOPO"
note "relay -> dugite-bp only; cardano-bp -> (none). Bridge cut."

start_dugite dugite-bp    "$LD_DUGITE_BP_PORT" "$LD_DUGITE_BP_METRICS_PORT"    "$LD_DUGITE_BP_SOCK" pool1
start_dugite dugite-relay "$LD_RELAY_PORT"     "$LD_DUGITE_RELAY_METRICS_PORT" "$LD_RELAY_SOCK"

cardano-node run \
    --config        "$LD_CONFIG/cardano-bp.config.json" \
    --topology      "$CBP_TOPO" \
    --database-path "$LD_STATE/cardano-bp.db" \
    --socket-path   "$LD_CARDANO_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_CARDANO_BP_PORT" \
    --shelley-kes-key                 "$LD_KEYS/pool2/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool2/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool2/opcert.cert" \
    > "$LD_LOGS/cardano-bp.log" 2>&1 &
echo $! > "$LD_STATE/cardano-bp.pid"

cardano-node run \
    --config        "$LD_CONFIG/cardano-arbiter.config.json" \
    --topology      "$LD_CONFIG/cardano-arbiter.topology.json" \
    --database-path "$LD_STATE/cardano-arbiter.db" \
    --socket-path   "$LD_CARDANO_ARBITER_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_CARDANO_ARBITER_PORT" \
    > "$LD_LOGS/cardano-arbiter.log" 2>&1 &
echo $! > "$LD_STATE/cardano-arbiter.pid"

wait_for_socket "$LD_DUGITE_BP_SOCK" 180 >/dev/null 2>&1
wait_for_socket "$LD_CARDANO_BP_SOCK" 240 >/dev/null 2>&1

# ── step 2: let both islands build independent chains ──────────────────────
step "2. both islands build independent chains from the same genesis"
deadline=$(( $(date +%s) + FORK_BUILD_TIMEOUT ))
DBP_BLK=0; CBP_BLK=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    DBP_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block);  DBP_BLK=${DBP_BLK:-0}
    CBP_BLK=$(tip_field "$LD_CARDANO_BP_SOCK" .block); CBP_BLK=${CBP_BLK:-0}
    [ "$DBP_BLK" -ge "$MIN_FORK_BLOCKS" ] && [ "$CBP_BLK" -ge "$MIN_FORK_BLOCKS" ] && break
    sleep 5
done
note "fork depths: dugite-bp=$DBP_BLK cardano-bp=$CBP_BLK (wanted >= $MIN_FORK_BLOCKS each)"

DBP_FORGED=$(forge_count "$LD_LOGS/dugite-bp.log")
DBP_HASH_BEFORE=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
CBP_HASH_BEFORE=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)

if [ "$DBP_BLK" -lt "$MIN_FORK_BLOCKS" ] || [ "$CBP_BLK" -lt "$MIN_FORK_BLOCKS" ]; then
    inconc "an island did not reach $MIN_FORK_BLOCKS blocks (dugite-bp=$DBP_BLK forged=$DBP_FORGED, cardano-bp=$CBP_BLK) — the scenario is NOT constructed and every assertion below would be vacuous"
    echo; echo "GENESIS-FORK ROUND: INCONCLUSIVE (scenario not constructed)"
    ./stop.sh >/dev/null 2>&1
    exit 3
fi

if [ "${DBP_FORGED:-0}" -ge 1 ]; then
    ok "dugite-bp forged $DBP_FORGED blocks on its island (blocks are its own, not adopted)"
else
    inconc "dugite-bp forged 0 blocks — its chain came from the relay, so 'its own blocks' is unproven"
fi

if [ -n "$DBP_HASH_BEFORE" ] && [ "$DBP_HASH_BEFORE" != "$CBP_HASH_BEFORE" ]; then
    ok "the two chains are genuinely different (dugite ${DBP_HASH_BEFORE:0:16}… vs cardano ${CBP_HASH_BEFORE:0:16}…)"
else
    inconc "the two tips are identical — the islands did not diverge, so there is nothing to adopt"
fi

if [ "$INCONCLUSIVE" -gt 0 ]; then
    echo; echo "GENESIS-FORK ROUND: INCONCLUSIVE ($INCONCLUSIVE precondition(s) unmet)"
    ./stop.sh >/dev/null 2>&1
    exit 3
fi

# ── step 3: restore the bridge; dugite-bp must adopt ───────────────────────
step "3. restore the bridge — dugite-bp must adopt a chain diverging at GENESIS"
MARK=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null)

stop_pid_file "$LD_STATE/dugite-relay.pid" || note "dugite-relay did not exit within 60s of SIGTERM"
stop_pid_file "$LD_STATE/cardano-bp.pid"   || note "cardano-bp did not exit within 60s of SIGTERM"

cp "$LD_STATE/relay.topology.real.json" "$RELAY_TOPO"
cp "$LD_STATE/cbp.topology.real.json"   "$CBP_TOPO"
note "bridge restored (relay -> dugite-bp + cardano-bp; cardano-bp -> relay)"

start_dugite dugite-relay "$LD_RELAY_PORT" "$LD_DUGITE_RELAY_METRICS_PORT" "$LD_RELAY_SOCK"
cardano-node run \
    --config        "$LD_CONFIG/cardano-bp.config.json" \
    --topology      "$CBP_TOPO" \
    --database-path "$LD_STATE/cardano-bp.db" \
    --socket-path   "$LD_CARDANO_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_CARDANO_BP_PORT" \
    --shelley-kes-key                 "$LD_KEYS/pool2/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool2/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool2/opcert.cert" \
    >> "$LD_LOGS/cardano-bp.log" 2>&1 &
echo $! > "$LD_STATE/cardano-bp.pid"

# dugite-bp is deliberately NOT restarted: the adoption must happen on the LIVE
# path, which is where #1057 bites. A restart would exercise startup replay
# instead and could mask it.
CONVERGED=0
deadline=$(( $(date +%s) + CONVERGE_TIMEOUT ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    sleep 5
    DBP_H=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
    CBP_H=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
    if [ -n "$DBP_H" ] && [ -n "$CBP_H" ] && [ "$DBP_H" = "$CBP_H" ]; then
        CONVERGED=1
        break
    fi
done

DBP_AFTER=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
CBP_AFTER=$(tip_field "$LD_CARDANO_BP_SOCK" .block)
note "after reconnect: dugite-bp block=${DBP_AFTER:-?} cardano-bp block=${CBP_AFTER:-?}"

# DIRECTION MATTERS, and tip-hash equality alone does NOT establish it.
#
# Two chains converging tells us nothing about WHICH side moved. On the first
# working run of this round the hashes matched at block 11 with dugite-bp having
# logged ZERO chain switches — cardano-bp had adopted DUGITE's chain, because
# dugite's fork won chain selection. Reporting that as "dugite-bp adopted the
# genesis-divergent chain" was a false positive, and the same mistake shape as
# #1057's original bad fix: asserting a symptom instead of the mechanism.
#
# So require BOTH: the tips agree, AND dugite-bp actually moved off its own fork.
# A chain switch / ledger rollback in its log after the bridge was restored is the
# only direct evidence it crossed the fork rather than winning it.
DBP_SWITCHED=$(awk -v m="${MARK:-0}" '
    NR > m && (/switching to longer fork/ || /chain switch/ || /rolling back ledger to intersection/) {c++}
    END{print c+0}' "$LD_LOGS/dugite-bp.log" 2>/dev/null)
note "dugite-bp chain-switch / ledger-rollback lines after reconnect: ${DBP_SWITCHED:-0}"

if [ "$CONVERGED" -ne 1 ]; then
    bad "dugite-bp did NOT converge with cardano-bp within ${CONVERGE_TIMEOUT}s (stuck at block ${DBP_AFTER:-?}, cardano-bp at ${CBP_AFTER:-?}) — this is #1057"
elif [ "${DBP_SWITCHED:-0}" -eq 0 ]; then
    inconc "tips agree at block ${DBP_AFTER:-?} but dugite-bp logged NO chain switch — its own fork WON chain selection and cardano-bp adopted it, so the genesis-rooted adoption under test never happened. Not a pass: the scenario resolved in the wrong direction. Give the cardano island a head start (raise GF_MIN_FORK_BLOCKS or start it first) so its chain is strictly longer."
else
    ok "dugite-bp ADOPTED the genesis-divergent chain: crossed a fork it did not build (${DBP_SWITCHED} switch/rollback line(s)), tip hash matches cardano-bp at block ${DBP_AFTER:-?}"
fi

# ── step 4: name the wedge signatures ──────────────────────────────────────
step "4. wedge signatures — these distinguish unfixed from a BAD fix"
count_sig() {
    awk -v m="${MARK:-0}" -v p="$1" 'NR > m && index($0, p) {c++} END{print c+0}' \
        "$LD_LOGS/dugite-bp.log" 2>/dev/null
}
DECLINED=$(count_sig "declining far-ahead range")
HORIZON=$(count_sig "beyond forecast horizon")
ROLLBACK_FAIL=$(count_sig "Fork rollback failed")
ORIGIN_RB=$(count_sig "intersection=00000000")
note "BlockFetch declined a far-ahead range : ${DECLINED:-0}"
note "peer dropped at forecast horizon      : ${HORIZON:-0}"
note "ledger asked to roll back to Origin   : ${ORIGIN_RB:-0}"
note "Fork rollback FAILED                  : ${ROLLBACK_FAIL:-0}"

# unfixed  -> declined/horizon > 0, rollback_fail == 0  (switch never attempted)
# bad fix  -> rollback_fail > 0                         (attempted, ledger refused)
# fixed    -> converged, both 0
if [ "${ROLLBACK_FAIL:-0}" -gt 0 ]; then
    bad "BAD-FIX SHAPE: the chain switch was ATTEMPTED but the ledger could not roll back to Origin. A genesis-rooted SwitchPlan from storage is necessary but NOT sufficient — the ledger must re-initialise from genesis (#1057)"
elif [ "$CONVERGED" -eq 1 ] && [ "${DBP_SWITCHED:-0}" -gt 0 ]; then
    ok "no wedge signatures after a successful genesis-rooted adoption"
elif [ "$CONVERGED" -eq 1 ]; then
    note "no wedge signatures — but see the INCONCLUSIVE above: dugite-bp never crossed the fork, so their absence is not evidence about #1057"
else
    note "UNFIXED SHAPE: the switch was never attempted (BlockFetch declined the range / the horizon park dropped peers)"
fi

# ── summary ────────────────────────────────────────────────────────────────
step "SUMMARY"
if [ "$FAILURES" -gt 0 ]; then
    echo "GENESIS-FORK ROUND: FAIL ($FAILURES assertion(s)) — #1057 reproduced"
    ./stop.sh >/dev/null 2>&1
    exit "$FAILURES"
elif [ "$INCONCLUSIVE" -gt 0 ]; then
    echo "GENESIS-FORK ROUND: INCONCLUSIVE ($INCONCLUSIVE) — the scenario did not resolve in the direction under test; NOT a pass"
    ./stop.sh >/dev/null 2>&1
    exit 3
else
    echo "GENESIS-FORK ROUND: PASS — dugite-bp crossed a genesis-rooted fork"
    ./stop.sh >/dev/null 2>&1
    exit 0
fi
