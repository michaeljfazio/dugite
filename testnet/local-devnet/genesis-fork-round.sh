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
# THE HEAD START IS LOAD-BEARING, and this is the second thing the round got wrong.
#
# With both islands running up symmetrically, they reach roughly equal depth and
# DUGITE's fork tends to win chain selection — cardano-bp adopts dugite's chain and
# the adoption path under test is never entered. Measured, on main with the #1057
# fix reverted:
#
#   GF_MIN_FORK_BLOCKS=4  dugite=4 cardano=5  converged at block 11, 0 switches
#   GF_MIN_FORK_BLOCKS=8  dugite=9 cardano=9  converged at block 13, 0 switches
#
# Both INCONCLUSIVE, and both would have read as PASS on tip-hash equality alone.
#
# So the dugite island is FROZEN at a shallow tip while the cardano island runs
# far ahead (step 2b): stop dugite-relay and dugite-bp loses its only peer, which
# makes the peer-connectivity forge gate stop it extending its own chain — the
# gate working FOR the construction. dugite-bp itself is never restarted.
#
# The gap is measured in SLOTS, not blocks, because the target is the forecast
# horizon: devnet is k=40, f=0.5, slotLength=1s, so 3k/f = 240 slots. A default
# GF_HEAD_START_SLOTS=300 puts the peer's headers definitively beyond it. That
# mirrors the original occurrence — dugite frozen at slot 21 while the network ran
# to slot 1004 — and the horizon park is what turns a declined BlockFetch range
# into the endless peer-churn loop.
#
# MEASURED — #1057 REPRODUCED (2026-08-06, main with the reverted fix):
#
#   fork depths           dugite-bp=5 (6 forged), cardano-bp=7, chains differ
#   after the freeze      dugite-bp slot=18 block=5 | cardano-bp slot=326 block=93
#                         slot gap=308 (> the 240-slot horizon)
#   bridge restored       dugite-bp STAYED at block 5 for the full 300s while
#                         cardano-bp reached 167. 0 chain switches. FAIL.
#
# dugite-bp's own log shows it frozen and unable even to try:
#   ERROR forge: TraceNoLedgerView: chain tip too far behind for ledger view
#     forecast — skipping forge current_slot=627 tip_slot=18 lag_slots=609
#     stability_window=240
#
# and dugite-relay's log carries the loop itself, repeating verbatim on every
# reconnect with nothing in between:
#   ChainSync intersection found peer_addr=127.0.0.1:3003 point=origin
#     tip_slot=332 tip_block_number=95
#   ChainSync intersection at Origin with non-Origin local chain — accepting
#     because local chain is within k blocks of genesis …
#   ChainSync rollback rollback_point=origin tip_slot=332 tip_block_number=95
#   WARN chainsync task failed addr=127.0.0.1:3003 error=… header slot 340
#     beyond forecast horizon — no ledger progress for 60s
#
# That refines #1057's diagnosis in one useful way: ChainSync ALREADY accepts the
# Origin intersection, and nothing at all is logged between the rollback and the
# 60s timeout. The blocker is downstream of ChainSync — BlockFetch's #735
# gross-request invariant declines the peer's block 0 (at `debug!`, hence the
# silence), the ledger never advances, and the horizon park kills the peer.

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
# > 3k/f = 240 slots on this devnet (k=40, f=0.5), so the peer's headers land
# beyond dugite-bp's forecast horizon — the condition the original #1057
# occurrence had and the reason its park loop never self-healed.
HEAD_START_SLOTS="${GF_HEAD_START_SLOTS:-300}"
# slotLength is 1s and only pool2 (40% stake) forges on the cardano island, so
# 300 slots of lead is ~300s of wall clock. 900 leaves generous headroom.
HEAD_START_TIMEOUT="${GF_HEAD_START_TIMEOUT:-900}"

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

# ── step 2b: freeze the dugite island, let cardano run far ahead ───────────
# Equal-depth forks resolve in the WRONG direction (dugite's fork wins and cardano
# adopts it), so the peer's chain must be strictly longer AND its headers must sit
# beyond dugite-bp's forecast horizon.
#
# dugite-relay is dugite-bp's only peer, so stopping it leaves dugite-bp peerless
# and the peer-connectivity forge gate stops it extending its own chain. dugite-bp
# is NOT restarted — the adoption still has to happen on the live path.
step "2b. freeze the dugite island; cardano runs >= ${HEAD_START_SLOTS} slots ahead"

stop_pid_file "$LD_STATE/dugite-relay.pid" || note "dugite-relay did not exit within 60s of SIGTERM"
sleep 5
DBP_FROZEN_SLOT=$(tip_field "$LD_DUGITE_BP_SOCK" .slot); DBP_FROZEN_SLOT=${DBP_FROZEN_SLOT:-0}
DBP_FROZEN_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block); DBP_FROZEN_BLK=${DBP_FROZEN_BLK:-0}
note "dugite-bp frozen at slot=$DBP_FROZEN_SLOT block=$DBP_FROZEN_BLK (peerless)"

deadline=$(( $(date +%s) + HEAD_START_TIMEOUT ))
CBP_SLOT=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    CBP_SLOT=$(tip_field "$LD_CARDANO_BP_SOCK" .slot); CBP_SLOT=${CBP_SLOT:-0}
    [ "$CBP_SLOT" -ge $(( DBP_FROZEN_SLOT + HEAD_START_SLOTS )) ] && break
    sleep 10
done

# Re-read dugite-bp: if it kept forging while peerless, the gap must be recomputed
# from where it ACTUALLY is, not from where it was when the relay died.
DBP_NOW_SLOT=$(tip_field "$LD_DUGITE_BP_SOCK" .slot); DBP_NOW_SLOT=${DBP_NOW_SLOT:-0}
DBP_NOW_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block);  DBP_NOW_BLK=${DBP_NOW_BLK:-0}
CBP_BLK2=$(tip_field "$LD_CARDANO_BP_SOCK" .block);    CBP_BLK2=${CBP_BLK2:-0}
SLOT_GAP=$(( CBP_SLOT - DBP_NOW_SLOT ))
note "dugite-bp slot=$DBP_NOW_SLOT block=$DBP_NOW_BLK | cardano-bp slot=$CBP_SLOT block=$CBP_BLK2 | slot gap=$SLOT_GAP"

if [ "$DBP_NOW_BLK" -gt "$DBP_FROZEN_BLK" ]; then
    note "dugite-bp advanced $DBP_FROZEN_BLK -> $DBP_NOW_BLK while peerless (the forge gate did not hold it); the gap below is what matters"
fi

if [ "$CBP_BLK2" -le "$DBP_NOW_BLK" ]; then
    inconc "cardano-bp's chain is NOT strictly longer (cardano=$CBP_BLK2 dugite=$DBP_NOW_BLK) — dugite's fork would win chain selection again. Raise GF_HEAD_START_SLOTS or GF_HEAD_START_TIMEOUT."
fi
if [ "$SLOT_GAP" -lt 240 ]; then
    inconc "slot gap $SLOT_GAP is inside the 240-slot forecast horizon — the horizon park that made #1057 unrecoverable would not be reached. Raise GF_HEAD_START_SLOTS or GF_HEAD_START_TIMEOUT."
fi

if [ "$INCONCLUSIVE" -gt 0 ]; then
    echo; echo "GENESIS-FORK ROUND: INCONCLUSIVE ($INCONCLUSIVE precondition(s) unmet)"
    ./stop.sh >/dev/null 2>&1
    exit 3
fi
ok "preconditions met: dugite-bp holds its own $DBP_NOW_BLK-block genesis-rooted chain, cardano-bp is $((CBP_BLK2 - DBP_NOW_BLK)) blocks / $SLOT_GAP slots ahead on an incompatible one"

# ── step 3: restore the bridge; dugite-bp must adopt ───────────────────────
step "3. restore the bridge — dugite-bp must adopt a chain diverging at GENESIS"
MARK=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null)
# Both node logs are APPENDED across the restart, so step 4 must count only what
# happens after the bridge is restored. The pre-restore relay never saw cardano-bp
# at all, but relying on that is fragile — mark it explicitly.
RELAY_MARK=$(wc -l < "$LD_LOGS/dugite-relay.log" 2>/dev/null)

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
    inconc "tips agree at block ${DBP_AFTER:-?} but dugite-bp logged NO chain switch — its own fork WON chain selection and cardano-bp adopted it, so the genesis-rooted adoption under test never happened. Not a pass: the scenario resolved in the wrong direction. Raise GF_HEAD_START_SLOTS so the cardano island's chain is longer by a wider margin."
else
    ok "dugite-bp ADOPTED the genesis-divergent chain: crossed a fork it did not build (${DBP_SWITCHED} switch/rollback line(s)), tip hash matches cardano-bp at block ${DBP_AFTER:-?}"
fi

# ── step 4: name the wedge signatures ──────────────────────────────────────
#
# THE WEDGED NODE IS THE RELAY, and the first version of this step looked in the
# wrong place. dugite-relay is the node that peers directly with cardano-bp, so it
# is the one that must adopt the genesis-divergent chain; dugite-bp is one hop
# downstream and cannot adopt anything the relay has not adopted first. Both hold
# their own copy of dugite's short chain, so both need the fix — but only the
# relay's log carries the loop.
#
# Measured signature counts on dugite-bp.log alone were 0/0/0/0 while the wedge
# was plainly happening on the relay. Two independent reasons, both worth knowing:
#
#   1. wrong node's log (above), and
#   2. "BlockFetch: declining far-ahead range" is a `debug!`, so it never appears
#      at the devnet's default log level at all. A signature that cannot be
#      observed is not a signature.
#
# So the loop itself is the signature, and every line of it IS emitted at INFO:
#
#   ChainSync intersection found  peer_addr=<cardano-bp> point=origin tip_slot=332
#   ChainSync intersection at Origin with non-Origin local chain — accepting …
#   ChainSync rollback            rollback_point=origin
#   WARN chainsync task failed    error=… header slot 340 beyond forecast horizon
#                                 — no ledger progress for 60s
#   … then reconnect and repeat, forever.
#
# Note what that sequence proves: ChainSync ALREADY accepts the Origin
# intersection. The blocker is downstream — BlockFetch's #735 gross-request
# invariant declines the peer's block 0, the ledger never advances, and the
# horizon park then kills the peer. #1057's ChainSync layer needs no change.
step "4. wedge signatures — these distinguish unfixed from a BAD fix"
count_sig() { # <phrase> [logfile] [mark]
    awk -v p="$1" -v m="${3:-${RELAY_MARK:-0}}" 'NR > m && index($0, p) {c++} END{print c+0}' \
        "${2:-$LD_LOGS/dugite-relay.log}" 2>/dev/null
}
ORIGIN_ACCEPT=$(count_sig "intersection at Origin with non-Origin local chain")
ORIGIN_RB=$(count_sig "rollback_point=origin")
HORIZON=$(count_sig "beyond forecast horizon")
ROLLBACK_FAIL=$(count_sig "Fork rollback failed")
ROLLBACK_FAIL_BP=$(count_sig "Fork rollback failed" "$LD_LOGS/dugite-bp.log" "${MARK:-0}")
ROLLBACK_FAIL=$(( ROLLBACK_FAIL + ROLLBACK_FAIL_BP ))
note "relay accepted an Origin intersection : ${ORIGIN_ACCEPT:-0}"
note "relay rolled ChainSync back to Origin : ${ORIGIN_RB:-0}"
note "peer dropped at forecast horizon      : ${HORIZON:-0}"
note "Fork rollback FAILED (either node)    : ${ROLLBACK_FAIL:-0}"
RELAY_TIP=$(tip_field "$LD_RELAY_SOCK" .block)
note "dugite-relay block=${RELAY_TIP:-?} vs cardano-bp block=${CBP_AFTER:-?}"

# unfixed  -> origin intersection accepted, horizon drops > 0, rollback_fail == 0
#             (ChainSync tried, BlockFetch declined block 0, nothing advanced)
# bad fix  -> rollback_fail > 0  (storage emitted a genesis-rooted SwitchPlan and
#                                 the ledger refused it — the reverted attempt)
# fixed    -> converged, horizon drops 0
if [ "${ROLLBACK_FAIL:-0}" -gt 0 ]; then
    bad "BAD-FIX SHAPE: the chain switch was ATTEMPTED but the ledger could not roll back to Origin. A genesis-rooted SwitchPlan from storage is necessary but NOT sufficient — the ledger must re-initialise from genesis (#1057)"
elif [ "$CONVERGED" -eq 1 ] && [ "${DBP_SWITCHED:-0}" -gt 0 ]; then
    ok "no wedge signatures after a successful genesis-rooted adoption"
elif [ "$CONVERGED" -eq 1 ]; then
    note "no wedge signatures — but see the INCONCLUSIVE above: dugite-bp never crossed the fork, so their absence is not evidence about #1057"
elif [ "${HORIZON:-0}" -gt 0 ] && [ "${ORIGIN_RB:-0}" -gt 0 ]; then
    note "UNFIXED SHAPE CONFIRMED on dugite-relay: it accepted the Origin intersection ${ORIGIN_ACCEPT} time(s), rolled ChainSync back to Origin ${ORIGIN_RB} time(s), made no ledger progress, and was dropped at the forecast horizon ${HORIZON} time(s) — the endless reconnect loop of #1057, with dugite-bp stranded behind it"
else
    note "the round FAILED but neither the unfixed nor the bad-fix signature is present — read $LD_LOGS/dugite-relay.log directly before drawing any conclusion"
fi

# ── step 5: does a genesis-ledger RESTART clear the wedge? ─────────────────
#
# DIAGNOSTIC, not an assertion. It runs only when step 3 already established the
# wedge, and it never changes the verdict — it answers the one question the fix
# plan turns on.
#
# The candidate fix is to MARK-AND-RESTART rather than re-initialise the ledger in
# place: a rollback-to-Origin persists a marker, the node exits cleanly, and
# `Node::new`'s already-validated from-genesis path rebuilds. That path matters
# because it WIPES the on-disk LSM UTxO store, and that wipe is load-bearing — the
# code comment on it records that a stale store makes `sumCoinUTxO` roughly double
# at the Byron→Shelley boundary, drives the reserves recompute to 0, and underflows
# the first MIR debit into a panic. A live in-place re-init would have to invent
# that teardown against open handles; the startup path already has it.
#
# For that plan to be sufficient, a dugite node whose LEDGER is at Origin but whose
# ChainDB still holds its own dead-end fork must be able to adopt the peer's chain.
# Deleting `ledger-snapshot.bin` and restarting reproduces exactly that state.
#
#   adopts -> the restart path alone is sufficient; the storage/BlockFetch genesis
#             clauses are NOT needed, and the reverted two-layer attempt was
#             solving the wrong problem entirely.
#   wedged -> the ChainDB fork blocks adoption regardless of ledger state, so
#             BlockFetch's #735 gross-request invariant and `switch_chain`'s
#             ImmutableDB-anchor test each need their genesis clause as well.
if [ "$CONVERGED" -ne 1 ] && [ "${GF_SKIP_RESTART_PROBE:-0}" -ne 1 ]; then
    step "5. DIAGNOSTIC — does a genesis-ledger restart clear the wedge?"
    stop_pid_file "$LD_STATE/dugite-bp.pid" || note "dugite-bp did not exit within 60s of SIGTERM"
    SNAP="$LD_STATE/dugite-bp.db/ledger-snapshot.bin"
    if [ -f "$SNAP" ]; then
        rm -f "$SNAP"
        note "removed $SNAP — dugite-bp will come up with a GENESIS ledger over a ChainDB that still holds its own ${DBP_NOW_BLK}-block fork"
    else
        note "no ledger-snapshot.bin present; dugite-bp already comes up from genesis"
    fi
    RESTART_MARK=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null)
    start_dugite dugite-bp "$LD_DUGITE_BP_PORT" "$LD_DUGITE_BP_METRICS_PORT" "$LD_DUGITE_BP_SOCK" pool1
    wait_for_socket "$LD_DUGITE_BP_SOCK" 180 >/dev/null 2>&1

    R_CONVERGED=0
    deadline=$(( $(date +%s) + CONVERGE_TIMEOUT ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        sleep 5
        DBP_H=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
        CBP_H=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
        if [ -n "$DBP_H" ] && [ -n "$CBP_H" ] && [ "$DBP_H" = "$CBP_H" ]; then
            R_CONVERGED=1
            break
        fi
    done
    R_DBP=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
    R_CBP=$(tip_field "$LD_CARDANO_BP_SOCK" .block)
    R_HORIZON=$(count_sig "beyond forecast horizon" "$LD_LOGS/dugite-bp.log" "${RESTART_MARK:-0}")
    if [ "$R_CONVERGED" -eq 1 ]; then
        note "RESTART PATH IS SUFFICIENT: with a genesis ledger, dugite-bp adopted the peer's chain (block ${R_DBP:-?} == cardano-bp). The fix is marker + clean exit + the existing from-genesis startup path; the storage/BlockFetch genesis clauses are NOT required."
    else
        # TWO DIFFERENT CAUSES look identical here, and conflating them wasted a
        # cycle: the genesis clause may be ABSENT, or it may be PRESENT but gated
        # off. #1057 half A gates both layers on `ledger_at_origin`, and that turns
        # out to be a transient boot state — `run()` replays the ChainDB and
        # re-applies the node's own fork within seconds, so the ledger has left
        # Origin long before a peer offers a genesis-rooted range. Measured:
        # `tip_slot=36` twelve seconds after boot, `lag_slots=766`.
        #
        # The discriminator is dugite-bp's OWN ledger tip after the restart. If it
        # is at its own fork's tip rather than Origin, the clause was gated off, not
        # missing.
        R_DBP_SLOT=$(tip_field "$LD_DUGITE_BP_SOCK" .slot)
        note "RESTART PATH IS NOT SUFFICIENT: dugite-bp came up at genesis and STILL did not adopt (block ${R_DBP:-?} vs cardano-bp ${R_CBP:-?}, slot ${R_DBP_SLOT:-?}, horizon drops ${R_HORIZON:-0})."
        if [ "${R_DBP_SLOT:-0}" -gt 0 ]; then
            note "  ... and its ledger is at slot ${R_DBP_SLOT} — NOT Origin. The startup ChainDB replay re-applied its own fork, so any fix gated on \"the ledger is at Origin\" is DORMANT: the precondition is false by the time it would matter. The gate needs to be \"the ledger CAN be taken to Origin\" (local chain within k of genesis, matching ChainSync's own clause) plus a real genesis re-init — #1057 half B."
        else
            note "  ... and its ledger IS still at Origin, so the genesis clause in BlockFetch's #735 invariant / switch_chain's anchor test is genuinely missing or not reached."
        fi
    fi
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
