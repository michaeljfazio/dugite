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
#   1.  setup.sh with a two-forger genesis, stake split 15/85 AGAINST dugite (see
#       LD_POOL2_STAKE_PCT below) so cardano's chain outgrows dugite's by construction.
#   2.  Cut the bridge. Both islands build their own chain from the same genesis.
#       dugite-bp is peered with the relay ON PURPOSE — it cannot forge without a hot
#       peer plus a ChainSync intersection.
#   2b. SIGSTOP dugite-bp. The relay is now stranded holding a genesis-rooted chain it
#       cannot extend, while cardano runs >= 300 slots ahead.
#   2c. RESTART the relay. It must come back still able to roll back to Origin.
#   3.  Restore the bridge by SIGHUP (no restarts) and SIGCONT dugite-bp. The relay
#       must adopt cardano's chain IN PLACE.
#   4.  Assert no wedge signatures and no undeclared error-class lines.
#   5.  Restart the relay again; the adopted chain must survive it.
#
# THE SUBJECT IS THE RELAY, not dugite-bp. The relay is the node that peers with
# cardano-bp and therefore the one that must replace its own chain from Origin;
# dugite-bp is one hop downstream and can adopt nothing the relay has not adopted
# first. Step 3 asserts the relay's tip HASH matches cardano-bp's AND that the relay
# logged a chain switch — hash equality alone does not establish DIRECTION, and on the
# first working run of this round the hashes matched with ZERO switches because
# cardano-bp had adopted DUGITE's chain instead.
#
# WHAT A PASS MEANS NOW. Until 2026-08-07 this script was a reproduction and a PASS was
# impossible: the round's own summary said so. It is now a REGRESSION GATE — it requires
# live, in-place adoption with no marker file, no operator action and no discarded
# chain, which is what cardano-node does via `Paths.hs::isReachable`'s `anchorIsGenesis`
# case. A run that "recovers" by any other route must FAIL.
#
# The prediction this header used to carry was also wrong and is worth recording: it
# said the fix would require the ledger to re-initialise from genesis on a
# rollback-to-Origin. It did not. The live half needed only the gate to key off the
# LedgerSeq ANCHOR (`ledger_can_reach_origin`) rather than the ledger tip, so
# `find_rollback_n`'s `target == anchor` case makes the rollback executable with no
# re-initialisation at all. The genuinely missing piece was elsewhere entirely — a
# SNAPSHOT-POLICY bug: dugite snapshotted the live tip where cardano-node snapshots the
# LedgerDB anchor, so a restart came up unable to roll back anywhere.
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
# So the RELAY is frozen at a shallow tip while the cardano island runs far ahead
# (step 2b), by SIGSTOPping the PRODUCER. Topologies are left alone: see step 2b for
# the three freeze shapes that failed before this one, each silently.
#
# TWO THINGS THIS GATE GOT WRONG, both worth keeping written down.
#
# 1. IT MEASURED THE WRONG NODE. The wedged node is dugite-relay — it is the one
#    that peers with cardano-bp and must replace its chain from Origin. dugite-bp
#    is one hop downstream. An earlier gate compared cardano-bp's slot against
#    dugite-bp's and reported INCONCLUSIVE "slot gap 17" on a run whose relay was
#    frozen 302 slots back: the precondition held and the round refused to test it.
#
# 2. A SLOT GAP BETWEEN TWO LIVE ISLANDS CANNOT GROW. A slot number is wall-clock,
#    so every live tip tracks the current slot and their difference stays near zero
#    no matter how long you wait. Four runs chased that impossible target. The gap
#    accrues only against a node that is genuinely stopped — hence the freeze, and
#    hence measuring it at the frozen relay.
#
# The gap is measured in SLOTS because the target is the forecast horizon: devnet
# is k=40, f=0.5, slotLength=1s, so 3k/f = 240 slots, and GF_HEAD_START_SLOTS=300
# clears it. But note the horizon is the SECONDARY condition, downstream of the
# defect: BlockFetch declines the genesis range, the ledger stops, and only then —
# 240 slots later — does the park fire and strand the node. The PRIMARY condition
# is just that chain selection prefers a chain sharing only genesis, which is what
# GF_MIN_BLOCK_LEAD enforces. A round that demanded the horizon breach up front
# would skip the very step it exists to exercise.
#
# MEASURED — #1057 AS IT WAS, on main with the fix reverted (2026-08-06). Kept as the
# reference fingerprint of the unfixed node, NOT as the current expected result:
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
# The shared expected-error/allowlist oracle (#1041), same as kes-round and
# rollback-round. Sourced BEFORE the errexit relaxation below for the same
# order-is-load-bearing reason: anything it pulls in may `set -e`.
. ./lib/expect-log-errors.sh

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
# beyond THE RELAY's forecast horizon — the condition the original #1057
# occurrence had and the reason its park loop never self-healed.
#
# MEASURED AGAINST THE RELAY, NOT dugite-bp. The wedged node is dugite-relay: it
# is the one that must replace its chain from Origin. Comparing cardano-bp's slot
# against dugite-bp's reported INCONCLUSIVE "slot gap 17" on a run whose relay was
# frozen 302 slots back — the gate measured a node that is not under test. Same
# class as step 4 once reading the wrong node's log.
HEAD_START_SLOTS="${GF_HEAD_START_SLOTS:-300}"
# slotLength is 1s and the relay is frozen outright once its edges are dropped, so
# the lead accrues at ~1 slot/s of wall clock. 900 leaves generous headroom.
HEAD_START_TIMEOUT="${GF_HEAD_START_TIMEOUT:-900}"
# cardano's chain must out-length EVERY chain the relay can reach cheaply. Its own
# 5 blocks are a PREFIX of dugite-bp's, so adopting dugite-bp is a plain forward
# roll-forward and does not exercise the genesis-rooted path at all. Only a chain
# that beats dugite-bp's too forces the switch under test.
MIN_BLOCK_LEAD="${GF_MIN_BLOCK_LEAD:-20}"

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

# BOUNDED, because this round deliberately creates nodes that cannot answer.
#
# A SIGSTOPped node still has a LISTENING socket — the kernel completes the connect
# from its backlog — so `cardano-cli query tip` neither fails nor returns: it blocks
# forever. Unbounded, that turned a 15-minute wait loop into a 29-minute silent hang
# with no output at all, and the round looked merely slow rather than stuck.
#
# macOS ships no `timeout(1)`, so the bound is hand-rolled. TWO REJECTED SHAPES,
# recorded because both look correct:
#
#   out=$( cmd & ( sleep N; kill $! ) & wait ... )
#     The watchdog subshell INHERITS the command-substitution pipe, and killing the
#     subshell does not kill its `sleep` child. The orphan holds fd 1 open, so `$( )`
#     cannot see EOF and EVERY call takes the full timeout — including successful ones.
#     Its own test could not catch that, because it only exercised sockets that fail.
#
#   cmd | head -1
#     `head` cannot bound a process that never writes.
#
# So: run the query to a temp file, poll for its exit with a bounded loop, and read the
# file only if it finished. Nothing shares a pipe with the watchdog, and the happy path
# costs one poll interval. An unreadable tip returns EMPTY, which the callers already
# treat as "unmeasured" (see below) rather than as zero.
TIP_QUERY_TIMEOUT="${GF_TIP_QUERY_TIMEOUT:-20}"
tip_field() {
    local sock="$1" filter="$2" tmp q i limit
    tmp="$(mktemp "${TMPDIR:-/tmp}/gf-tipq.XXXXXX")" || return 0
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        >"$tmp" 2>/dev/null &
    q=$!
    i=0
    limit=$(( TIP_QUERY_TIMEOUT * 5 ))          # 200ms granularity
    while [ "$i" -lt "$limit" ]; do
        kill -0 "$q" 2>/dev/null || break
        sleep 0.2
        i=$(( i + 1 ))
    done
    if kill -0 "$q" 2>/dev/null; then
        kill -9 "$q" 2>/dev/null
        wait "$q" 2>/dev/null
        rm -f "$tmp"
        return 0                                # timed out -> unmeasured, not zero
    fi
    wait "$q" 2>/dev/null
    jq -r "$filter // empty" <"$tmp" 2>/dev/null
    rm -f "$tmp"
}

# Why `tip_field` returned nothing.
#
# Three runs in a row reported `relay=?` from an EMPTY `tip_field`, and I read that
# as "the relay did not converge" when it actually meant "the measurement failed" —
# the node was alive with peers and its N2C socket was listening. An assertion built
# on an unreadable value is the reports-nothing-while-looking-definite class
# (#916/#945): it cannot distinguish a wedged node from an unqueryable one.
#
# So when a tip read comes back empty, say WHY: whether the socket file exists, and
# what cardano-cli actually wrote to stderr.
explain_tip_failure() { # <label> <sock>
    local label="$1" sock="$2" err
    [ -S "$sock" ] || { note "  $label: no socket at $sock (node not listening)"; return; }
    err=$(cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" 2>&1 >/dev/null \
          | tr '\n' ' ' | cut -c1-220)
    if [ -n "$err" ]; then
        note "  $label: socket present but 'query tip' failed: $err"
    else
        note "  $label: socket present and 'query tip' succeeded — the jq field selector found nothing (unexpected shape)"
    fi
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
# INVERT THE STAKE SPLIT — this is what finally makes the precondition hold.
#
# `LD_TWO_FORGERS=1` defaults to pool1/pool2 = 60/40, i.e. DUGITE holds the majority.
# Four separate attempts at freezing the dugite island failed on the same wall: with
# 60% of the stake dugite-bp simply out-produces cardano-bp, so the cardano chain is
# never strictly longer and the adoption under test is never entered. Measured across
# those attempts:
#
#   dugite 4  vs cardano 5    converged, 0 switches   (equal depth, dugite won)
#   dugite 9  vs cardano 9    converged, 0 switches
#   dugite 111 vs cardano 86  slot gap 1              (freeze leaked: bidirectional link)
#   dugite 129 vs cardano 84  slot gap -2             (both edges dropped, still ahead)
#
# The knob already exists — `LD_POOL2_STAKE_PCT` — and pointing it the other way is
# the fix. At 85/15 the leader probabilities are P=1-(1-f)^sigma with f=0.5, so
# cardano-bp ~0.45/slot against dugite-bp ~0.10/slot: roughly 4.5x, which makes
# "cardano's chain is strictly longer" a property of the genesis rather than of the
# timing. Deterministic beats fighting the race.
LD_TWO_FORGERS=1 LD_POOL2_STAKE_PCT="${GF_POOL2_STAKE_PCT:-85}" \
    ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
ok "genesis regenerated (two-forger)"

[ -f "$LD_GENESIS/.two-forgers" ] || {
    echo "REFUSING TO RUN: $LD_GENESIS/.two-forgers absent — cardano-bp would not be"
    echo "forging, so there would be no competing chain to diverge from."
    exit 2
}

RELAY_TOPO="$LD_CONFIG/dugite-relay.topology.json"
CBP_TOPO="$LD_CONFIG/cardano-bp.topology.json"
BP_TOPO="$LD_CONFIG/dugite-bp.topology.json"
cp "$RELAY_TOPO" "$LD_STATE/relay.topology.real.json" || exit 2
cp "$CBP_TOPO"   "$LD_STATE/cbp.topology.real.json"   || exit 2

cleanup() {
    [ -f "$LD_STATE/relay.topology.real.json" ] && cp "$LD_STATE/relay.topology.real.json" "$RELAY_TOPO" 2>/dev/null
    [ -f "$LD_STATE/cbp.topology.real.json" ]   && cp "$LD_STATE/cbp.topology.real.json"   "$CBP_TOPO"   2>/dev/null
    [ -f "$LD_STATE/bp.topology.real.json" ]    && cp "$LD_STATE/bp.topology.real.json"    "$BP_TOPO"    2>/dev/null
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
# beyond the RELAY's forecast horizon.
step "2b. freeze the dugite island; cardano runs >= ${HEAD_START_SLOTS} slots ahead"

# FREEZE THE PRODUCER, AND LEAVE THE TOPOLOGIES ALONE. Three earlier shapes failed,
# each silently:
#
#   1. Stop dugite-relay.  Restarting it in step 3 re-anchored its LedgerSeq onto the
#      replayed tip, destroying the `ledger_can_reach_origin` precondition before the
#      peer's chain ever arrived. (The node no longer does that — but see 3.)
#   2. SIGHUP both nodes' `localRoots` to [].  A topology reload does NOT close an
#      ESTABLISHED connection, so the hot dugite-bp <-> relay session survived and
#      dugite-bp fed the relay from block 6 to 32 while the round believed it frozen.
#   3. Empty the relay's `localRoots` AND restart it.  dugite treats a peerless
#      topology as nothing-to-do and EXITS:
#          warn!("No peers configured in topology"); return Ok(());
#      so the relay replayed, logged its genesis anchor, and terminated — after which
#      `wait_for_socket` polled a STALE SOCKET FILE for 180s and the round looked
#      merely slow.
#
# SIGSTOP on dugite-bp is the whole freeze: forging stops instantly, nothing restarts,
# and the relay keeps a peer CONFIGURED (its step-1 cut topology, pointing at
# dugite-bp) so it can still boot in step 2c. A configured-but-frozen peer is also what
# a stranded node looks like in the field — it has peers, they just do not help.
BP_PID=$(cat "$LD_STATE/dugite-bp.pid" 2>/dev/null)

# dugite-bp's height is recorded BEFORE the stop: a stopped node does not answer its
# N2C socket, and an unreadable tip must not be scored as 0.
DBP_HELD_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block); DBP_HELD_BLK=${DBP_HELD_BLK:-0}
if [ -n "$BP_PID" ] && kill -STOP "$BP_PID" 2>/dev/null; then
    note "SIGSTOP -> dugite-bp (pid $BP_PID): frozen holding a ${DBP_HELD_BLK}-block chain, so the relay has nothing new to adopt"
else
    bad "could not SIGSTOP dugite-bp (pid ${BP_PID:-none}) — it would keep feeding the relay and the relay would never be stranded"
fi
note "topologies untouched: the relay still has dugite-bp configured (frozen), so it stays stranded AND can boot when step 2c restarts it"
sleep 30
# THE SUBJECT IS THE RELAY. It is the node that has to replace its chain from
# Origin, so every precondition below is measured at its socket. dugite-bp is
# recorded only to prove its chain does not beat cardano's (see MIN_BLOCK_LEAD).
REL_FROZEN_SLOT=$(tip_field "$LD_RELAY_SOCK" .slot);  REL_FROZEN_SLOT=${REL_FROZEN_SLOT:-0}
REL_FROZEN_BLK=$(tip_field "$LD_RELAY_SOCK" .block);  REL_FROZEN_BLK=${REL_FROZEN_BLK:-0}
# DO NOT QUERY dugite-bp FROM HERE UNTIL THE SIGCONT IN STEP 3. It is stopped, and a
# stopped node's socket is still LISTENING, so the query blocks instead of failing —
# it hung this round for 29 minutes with no output before `tip_field` was bounded.
# Its height is `DBP_HELD_BLK`, captured before the stop and unable to move while it
# is stopped.
note "dugite-relay frozen at slot=$REL_FROZEN_SLOT block=$REL_FROZEN_BLK (no peers — THIS is the node under test)"
note "dugite-bp holds $DBP_HELD_BLK block(s) and is stopped; its chain EXTENDS the relay's, so cardano's must out-length it"

# Wait for cardano's headers to clear the RELAY's forecast horizon. The relay is
# genuinely frozen, so this gap does accrue — unlike a gap measured between two
# LIVE islands, which cannot grow at all: a slot number is wall-clock, so each
# live tip tracks the current slot and their difference stays near zero. Four
# earlier runs chased that impossible target.
deadline=$(( $(date +%s) + HEAD_START_TIMEOUT ))
CBP_SLOT=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    CBP_SLOT=$(tip_field "$LD_CARDANO_BP_SOCK" .slot); CBP_SLOT=${CBP_SLOT:-0}
    [ "$CBP_SLOT" -ge $(( REL_FROZEN_SLOT + HEAD_START_SLOTS )) ] && break
    sleep 10
done

REL_NOW_SLOT=$(tip_field "$LD_RELAY_SOCK" .slot);      REL_NOW_SLOT=${REL_NOW_SLOT:-0}
REL_NOW_BLK=$(tip_field "$LD_RELAY_SOCK" .block);      REL_NOW_BLK=${REL_NOW_BLK:-0}
# dugite-bp is SIGSTOPped and cannot answer, so its chain is the length recorded
# before the stop — it is frozen, so that value cannot have moved.
DBP_NOW_BLK="$DBP_HELD_BLK"
CBP_BLK2=$(tip_field "$LD_CARDANO_BP_SOCK" .block);    CBP_BLK2=${CBP_BLK2:-0}
SLOT_GAP=$(( CBP_SLOT - REL_NOW_SLOT ))
note "dugite-relay slot=$REL_NOW_SLOT block=$REL_NOW_BLK | dugite-bp block=$DBP_NOW_BLK | cardano-bp slot=$CBP_SLOT block=$CBP_BLK2 | slot gap=$SLOT_GAP"

if [ "$REL_NOW_BLK" -gt "$REL_FROZEN_BLK" ]; then
    inconc "dugite-relay advanced $REL_FROZEN_BLK -> $REL_NOW_BLK while it was supposed to be frozen — an edge survived the SIGHUP, so it is not the stranded node this round needs."
fi

# The relay must be unable to reach cardano's chain by any cheap route: not by
# extending its own, and not by adopting dugite-bp's (of which its chain is a
# prefix). Only then does chain selection have to root a switch at genesis.
if [ "$CBP_BLK2" -lt $(( DBP_NOW_BLK + MIN_BLOCK_LEAD )) ]; then
    inconc "cardano-bp's chain ($CBP_BLK2) does not lead dugite-bp's ($DBP_NOW_BLK) by $MIN_BLOCK_LEAD — the relay could satisfy chain selection by rolling FORWARD onto dugite-bp's chain, which shares its whole history, so the genesis-rooted switch would never be attempted."
fi
if [ "$SLOT_GAP" -lt 240 ]; then
    inconc "slot gap $SLOT_GAP is inside the 240-slot forecast horizon — the horizon park that made #1057 unrecoverable would not be reached. Raise GF_HEAD_START_TIMEOUT."
fi

if [ "$INCONCLUSIVE" -gt 0 ]; then
    echo; echo "GENESIS-FORK ROUND: INCONCLUSIVE ($INCONCLUSIVE precondition(s) unmet)"
    ./stop.sh >/dev/null 2>&1
    exit 3
fi
ok "preconditions met: dugite-relay holds a $REL_NOW_BLK-block genesis-rooted chain and is frozen; cardano-bp is $((CBP_BLK2 - REL_NOW_BLK)) blocks / $SLOT_GAP slots ahead on an incompatible one, and leads dugite-bp by $((CBP_BLK2 - DBP_NOW_BLK))"

# ── step 2c: restart the stranded relay — the anchor must SURVIVE it ───────
#
# This used to be impossible to test, and the reason is the whole restart half of
# the defect. dugite snapshotted the LIVE ledger tip and then `reset_anchor`ed onto
# the replayed tip, so a restart came up with an anchor it could not roll back below
# and the wedge outlived the process. That is why earlier versions of this round used
# SIGHUP everywhere and treated a restart as something to avoid.
#
# cardano-node has no such problem: it snapshots the LedgerDB ANCHOR
# (`LedgerDB/V2.hs` takes `anchorHandle` of the pruned sequence) and re-pushes the
# volatile chain on init, so with nothing flushed its anchor is genesis and
# `Paths.hs::isReachable`'s `anchorIsGenesis` case still applies after a restart.
#
# So the restart now goes FIRST, deliberately, and step 3 must still adopt. If the
# startup evidence below is present but step 3 fails, the live half is broken; if the
# evidence is absent, the startup half is.
step "2c. restart the stranded dugite-relay; its genesis anchor must survive"

# Mark BEFORE the restart, so the assertions below can only be satisfied by lines the
# NEW process wrote. Scanning from 0 would also accept a line from the first boot.
RELAY_BOOT_MARK=$(wc -l < "$LD_LOGS/dugite-relay.log" 2>/dev/null)
RELAY_BOOT_MARK=${RELAY_BOOT_MARK:-0}
stop_pid_file "$LD_STATE/dugite-relay.pid" || note "dugite-relay did not exit within 60s of SIGTERM"
start_dugite dugite-relay "$LD_RELAY_PORT" "$LD_DUGITE_RELAY_METRICS_PORT" "$LD_RELAY_SOCK"
wait_for_socket "$LD_RELAY_SOCK" 180 >/dev/null 2>&1

# Its topology is still step 1's cut version — dugite-bp only, and dugite-bp is
# SIGSTOPped. So it boots (a peerless topology would make it exit outright) and comes
# up stranded on its own chain, now re-derived from disk rather than held in memory.
sleep 5
REL_BOOT_BLK=$(tip_field "$LD_RELAY_SOCK" .block); REL_BOOT_BLK=${REL_BOOT_BLK:-0}
note "dugite-relay restarted; it re-derived a ${REL_BOOT_BLK}-block chain from its ChainDB"

if awk -v m="$RELAY_BOOT_MARK" 'NR > m && /replaying it through the LedgerSeq delta path/ {f=1}
        END{exit !f}' "$LD_LOGS/dugite-relay.log" 2>/dev/null; then
    ok "dugite-relay replayed its volatile chain through the LedgerSeq delta path (blocks stay rollback-able)"
else
    bad "dugite-relay did NOT delta-replay its volatile chain at startup — its ImmutableDB is empty, so the whole chain is within k of genesis and every block should have been pushed as a delta. Without that the anchor lands on the replayed tip and the node cannot roll back at all."
fi

if awk -v m="$RELAY_BOOT_MARK" 'NR > m && /keeping its anchor rather than re-anchoring on the replayed tip/ {f=1}
        END{exit !f}' "$LD_LOGS/dugite-relay.log" 2>/dev/null; then
    ok "dugite-relay kept the replay-built anchor instead of re-anchoring on its tip"
else
    bad "dugite-relay re-anchored on the replayed tip after replay — reset_anchor clears the volatile window, so the rollback capability the delta replay just built was discarded"
fi

if awk -v m="$RELAY_BOOT_MARK" 'NR > m && /LedgerSeq was incoherent/ {c++} END{exit (c+0)>0}' \
        "$LD_LOGS/dugite-relay.log" 2>/dev/null; then
    ok "no LedgerSeq incoherence at startup (positive evidence the window chains onto its anchor)"
else
    bad "dugite-relay logged 'LedgerSeq was incoherent' after the restart — the replay-built window does not chain onto its anchor"
fi

# ── step 3: restore the bridge; dugite-bp must adopt ───────────────────────
step "3. restore the bridge — dugite-bp must adopt a chain diverging at GENESIS"
MARK=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null)
# Step 4 must count only what happens after the bridge is restored. Nothing is
# restarted here any more (the bridge comes back via SIGHUP), but the relay's log
# still spans the whole run, and the pre-restore relay never saw cardano-bp at all —
# relying on that is fragile, so mark it explicitly.
RELAY_MARK=$(wc -l < "$LD_LOGS/dugite-relay.log" 2>/dev/null)

# RESTORED BY SIGHUP ON BOTH NODES — NO RESTARTS.
#
# Same reason as step 2b: restarting dugite-relay would move its LedgerSeq anchor off
# Origin and close the gate before cardano-bp's chain ever arrives. Both dugite and
# cardano-node reload `localRoots` from the topology file on SIGHUP, so the bridge can
# be reconnected without touching either process.
cp "$LD_STATE/relay.topology.real.json" "$RELAY_TOPO"
cp "$LD_STATE/cbp.topology.real.json"   "$CBP_TOPO"
[ -f "$LD_STATE/bp.topology.real.json" ] && cp "$LD_STATE/bp.topology.real.json" "$BP_TOPO"

BP_PID=$(cat "$LD_STATE/dugite-bp.pid" 2>/dev/null)
# SIGCONT first: a SIGSTOPped process does not act on a queued SIGHUP, so reloading
# its topology before resuming it would silently do nothing.
if [ -n "$BP_PID" ] && kill -CONT "$BP_PID" 2>/dev/null; then
    note "SIGCONT -> dugite-bp (pid $BP_PID): resumed after the freeze"
else
    note "could not SIGCONT dugite-bp (pid ${BP_PID:-none}) — the relay verdict below is unaffected, only the downstream hop is"
fi
if [ -n "$BP_PID" ] && kill -HUP "$BP_PID" 2>/dev/null; then
    note "SIGHUP -> dugite-bp (pid $BP_PID): re-added its relay edge"
else
    bad "could not SIGHUP dugite-bp (pid ${BP_PID:-none}) — it would stay peerless and could not adopt anything"
fi

RELAY_PID=$(cat "$LD_STATE/dugite-relay.pid" 2>/dev/null)
CBP_PID=$(cat "$LD_STATE/cardano-bp.pid" 2>/dev/null)
if [ -n "$RELAY_PID" ] && kill -HUP "$RELAY_PID" 2>/dev/null; then
    note "SIGHUP -> dugite-relay (pid $RELAY_PID): re-added dugite-bp + cardano-bp"
else
    bad "could not SIGHUP dugite-relay (pid ${RELAY_PID:-none}) — bridge not restored on the dugite side"
fi
if [ -n "$CBP_PID" ] && kill -HUP "$CBP_PID" 2>/dev/null; then
    note "SIGHUP -> cardano-bp (pid $CBP_PID): re-added the relay"
else
    bad "could not SIGHUP cardano-bp (pid ${CBP_PID:-none}) — bridge not restored on the cardano side"
fi
note "bridge restored WITHOUT restarts, so dugite-relay's LedgerSeq anchor is still at Origin"

# dugite-bp is deliberately NOT restarted: the adoption must happen on the LIVE
# path, which is where #1057 bites. A restart would exercise startup replay
# instead and could mask it.
# CONVERGENCE IS MEASURED AT THE RELAY. It peers directly with cardano-bp and is
# the node that must replace its chain from Origin. dugite-bp is one hop downstream
# and can adopt nothing the relay has not adopted first, so it is reported as a
# secondary (its own fork diverges from cardano's at genesis too, which makes its
# adoption a second instance of the same path — useful, but not the subject).
CONVERGED=0
deadline=$(( $(date +%s) + CONVERGE_TIMEOUT ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    sleep 5
    REL_H=$(tip_field "$LD_RELAY_SOCK" .hash)
    CBP_H=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
    if [ -n "$REL_H" ] && [ -n "$CBP_H" ] && [ "$REL_H" = "$CBP_H" ]; then
        CONVERGED=1
        break
    fi
done

REL_AFTER=$(tip_field "$LD_RELAY_SOCK" .block)
DBP_AFTER=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
CBP_AFTER=$(tip_field "$LD_CARDANO_BP_SOCK" .block)
note "after reconnect: dugite-relay block=${REL_AFTER:-?} dugite-bp block=${DBP_AFTER:-?} cardano-bp block=${CBP_AFTER:-?}"

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
REL_SWITCHED=$(awk -v m="${RELAY_MARK:-0}" '
    NR > m && (/switching to longer fork/ || /chain switch/ || /rolling back ledger to intersection/) {c++}
    END{print c+0}' "$LD_LOGS/dugite-relay.log" 2>/dev/null)
DBP_SWITCHED=$(awk -v m="${MARK:-0}" '
    NR > m && (/switching to longer fork/ || /chain switch/ || /rolling back ledger to intersection/) {c++}
    END{print c+0}' "$LD_LOGS/dugite-bp.log" 2>/dev/null)
note "chain-switch / ledger-rollback lines after reconnect: dugite-relay=${REL_SWITCHED:-0} dugite-bp=${DBP_SWITCHED:-0}"

if [ "$CONVERGED" -ne 1 ]; then
    # NOT a failure by itself, and this is a deliberate change of semantics.
    #
    # #1057 half B does not make the LIVE path self-heal — it makes the wedge
    # RECOVERABLE BY RESTART, which is a different and weaker claim, honestly stated.
    # The node still cannot adopt a genesis-divergent chain in place: doing that
    # automatically off a peer's claim would be a remotely-triggerable forced resync.
    #
    # So the verdict moved to step 5. Keeping a hard FAIL here would mean the round
    # could never pass however well recovery worked, and a check that can only ever
    # fail stops being read.
    bad "dugite-relay did NOT adopt the genesis-divergent chain on the LIVE path (stuck at block ${REL_AFTER:-?}, cardano-bp at ${CBP_AFTER:-?}) — this is #1057 unfixed. cardano-node adopts here: Paths.hs::isReachable returns a genesis-rooted ChainDiff whenever the current chain's anchor is genesis, with rollback' = rollback + length chain, and it does so in place with no restart and no operator action."
elif [ "${REL_SWITCHED:-0}" -eq 0 ]; then
    inconc "tips agree at block ${REL_AFTER:-?} but dugite-relay logged NO chain switch — dugite's fork WON chain selection and cardano-bp adopted it, so the genesis-rooted adoption under test never happened. Not a pass: the scenario resolved in the wrong direction. Raise GF_MIN_BLOCK_LEAD so the cardano island's chain leads by a wider margin."
else
    ok "dugite-relay ADOPTED the genesis-divergent chain IN PLACE: crossed a fork it did not build (${REL_SWITCHED} switch/rollback line(s)), tip hash matches cardano-bp at block ${REL_AFTER:-?}"
    if [ "${DBP_AFTER:-0}" -ge "${REL_AFTER:-0}" ] 2>/dev/null && [ "${DBP_SWITCHED:-0}" -gt 0 ]; then
        ok "dugite-bp followed one hop downstream (${DBP_SWITCHED} switch/rollback line(s), block ${DBP_AFTER:-?}) — the same path a second time, through a node that had also forged its own fork"
    else
        note "dugite-bp at block ${DBP_AFTER:-?} with ${DBP_SWITCHED:-0} switch line(s) — it lags the relay; not fatal, the subject is the relay"
    fi
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
# The decline itself, now observable at the DEFAULT log level. It used to be a
# `debug!` indistinguishable from the ordinary far-ahead decline, which is half of
# why this step once reported 0/0/0/0 through a live wedge.
GENESIS_DECLINE=$(count_sig "declining a range rooted at GENESIS")
CANNOT_REJOIN=$(count_sig "1057: this node cannot rejoin the network")
ROLLBACK_FAIL=$(count_sig "Fork rollback failed")
ROLLBACK_FAIL_BP=$(count_sig "Fork rollback failed" "$LD_LOGS/dugite-bp.log" "${MARK:-0}")
ROLLBACK_FAIL=$(( ROLLBACK_FAIL + ROLLBACK_FAIL_BP ))
note "relay accepted an Origin intersection : ${ORIGIN_ACCEPT:-0}"
note "relay rolled ChainSync back to Origin : ${ORIGIN_RB:-0}"
note "BlockFetch declined a GENESIS range   : ${GENESIS_DECLINE:-0}"
note "escalated 'cannot rejoin' ERROR       : ${CANNOT_REJOIN:-0}"
note "peer dropped at forecast horizon      : ${HORIZON:-0}"
note "Fork rollback FAILED (either node)    : ${ROLLBACK_FAIL:-0}"

# The diagnostics are an ASSERTION, not decoration — via the SHARED oracle
# (lib/expect-log-errors.sh, #1041) rather than a hand-rolled count, so this round
# classifies error-class lines the same way analyze-evidence.sh and
# generate-release-report.sh do. A round with its own private notion of "an error"
# is how three components come to disagree (#916).
#
# Two directions, both required:
#   expect_log_errors      — the fault MUST have produced its diagnostic. A wedge
#                            that logs nothing at the default level is the state an
#                            operator cannot act on, and the state this round itself
#                            was fooled by.
#   assert_no_other_errors — no UNDECLARED error-class line. This is what would
#                            catch the BAD-FIX shape ("Fork rollback failed") or
#                            #985's "LedgerSeq was incoherent", neither of which is
#                            allowlisted on purpose.
if [ "$CONVERGED" -ne 1 ]; then
    # REQUIRE A DIAGNOSTIC FOR THE WEDGE THAT ACTUALLY HAPPENED — not for one shape
    # of it. There are two, at different layers, and demanding the wrong one produces
    # a FALSE failure:
    #
    #   BlockFetch layer  "declining a range rooted at GENESIS"
    #                     The peer's blocks never arrive at all.
    #   Ledger layer      "Rollback target outside LedgerSeq volatile window"
    #                     The blocks DID arrive, storage switched the chain, and the
    #                     ledger then refused to roll back to Origin. This is the
    #                     BAD-FIX shape, and it logs an ERROR of its own.
    #
    # An earlier version required the BlockFetch line unconditionally and reported
    # "the node wedged SILENTLY" on a run where the ledger-layer ERROR was present and
    # correct — the node was perfectly diagnosable, in the other layer's words.
    #
    # "beyond forecast horizon" is deliberately in NEITHER required set: it is a
    # downstream CONSEQUENCE whose appearance depends on the 60s park timer landing
    # inside the observation window, and step 4's own counter once saw it while
    # `expect_log_errors` did not, in the same run.
    if [ "${GENESIS_DECLINE:-0}" -gt 0 ]; then
        if expect_log_errors "$LD_LOGS/dugite-relay.log" "${RELAY_MARK:-0}" \
                "declining a range rooted at GENESIS"; then
            ok "the BlockFetch wedge is self-announcing at the DEFAULT log level"
        else
            bad "dugite-relay declined genesis ranges but the diagnostic is not at the default log level — an unrecoverable state that logs nothing cannot be diagnosed in the field"
        fi
    elif [ "${ROLLBACK_FAIL:-0}" -gt 0 ]; then
        ok "the LEDGER-layer wedge is self-announcing: the rollback refusal is logged as an ERROR (step 4's classification below names it)"
    else
        bad "dugite-relay failed to adopt and logged NEITHER wedge diagnostic — neither a declined genesis range nor a rollback refusal. Read $LD_LOGS/dugite-relay.log directly; a wedge with no diagnostic at all is the worst outcome of the three."
    fi

    # The WARN is rate-limited to one per 30s per worker. A count in the hundreds
    # would mean the throttle regressed — the unthrottled first version emitted
    # 29,435 lines / 17 MB in ten minutes, which buries every other line and can
    # fill a disk.
    if [ "${GENESIS_DECLINE:-0}" -gt 200 ]; then
        bad "genesis-decline WARN appeared ${GENESIS_DECLINE} times — the 30s throttle has regressed (#1057). A diagnostic that floods the log is not an improvement on silence."
    else
        ok "genesis-decline WARN is throttled (${GENESIS_DECLINE} line(s) for the whole wedge window)"
    fi

fi

# OUTSIDE the not-converged branch, deliberately. A successful adoption is exactly
# where a bad fix leaves its trace — storage emitting a genesis-rooted plan the
# ledger could not execute logs "Fork rollback failed" and then converges anyway
# by some other route. Running this only on failure would skip the case worth
# checking most.
if assert_no_other_errors "$LD_LOGS/dugite-relay.log" "${RELAY_MARK:-0}" \
        ./genesis-fork-round.allowed-errors; then
    ok "no UNDECLARED error-class line on dugite-relay after the reconnect"
else
    bad "undeclared error-class line(s) on dugite-relay — listed above. If one of them is 'Fork rollback failed' or 'LedgerSeq was incoherent', that is the BAD-FIX / #985 shape, not something to allowlist."
fi
RELAY_TIP=$(tip_field "$LD_RELAY_SOCK" .block)
note "dugite-relay block=${RELAY_TIP:-?} vs cardano-bp block=${CBP_AFTER:-?}"

# unfixed  -> origin intersection accepted, horizon drops > 0, rollback_fail == 0
#             (ChainSync tried, BlockFetch declined block 0, nothing advanced)
# bad fix  -> rollback_fail > 0  (storage emitted a genesis-rooted SwitchPlan and
#                                 the ledger refused it — the reverted attempt)
# fixed    -> converged, horizon drops 0
if [ "${ROLLBACK_FAIL:-0}" -gt 0 ]; then
    bad "BAD-FIX SHAPE: the chain switch was ATTEMPTED but the ledger could not roll back to Origin. A genesis-rooted SwitchPlan from storage is necessary but NOT sufficient — the ledger must re-initialise from genesis (#1057)"
elif [ "$CONVERGED" -eq 1 ] && [ "${REL_SWITCHED:-0}" -gt 0 ]; then
    ok "no wedge signatures after a successful genesis-rooted adoption"
elif [ "$CONVERGED" -eq 1 ]; then
    note "no wedge signatures — but see the INCONCLUSIVE above: dugite-relay never crossed the fork, so their absence is not evidence either way"
elif [ "${HORIZON:-0}" -gt 0 ] && [ "${ORIGIN_RB:-0}" -gt 0 ]; then
    note "UNFIXED SHAPE CONFIRMED on dugite-relay: it accepted the Origin intersection ${ORIGIN_ACCEPT} time(s), rolled ChainSync back to Origin ${ORIGIN_RB} time(s), made no ledger progress, and was dropped at the forecast horizon ${HORIZON} time(s) — the endless reconnect loop of #1057, with dugite-bp stranded behind it"
else
    note "the round FAILED but neither the unfixed nor the bad-fix signature is present — read $LD_LOGS/dugite-relay.log directly before drawing any conclusion"
fi

# ── step 5: the adoption must STICK across one more restart ────────────────
#
# What used to be here was a marker-driven recovery probe: the node wrote a
# "genesis-divergence-detected" file, the next start discarded its local chain, and
# the round called that a PASS while recording that the wedge still happened.
#
# That mechanism is gone, and its absence is the point. cardano-node never discards a
# chain because a peer offered a longer one it cannot root — `isReachable` returns
# `Nothing`, chain selection keeps what it has, and no marker, no operator restart and
# no re-sync are involved. A dugite-only "restart to recover" path was a divergence
# from the reference implementation dressed up as a fix, and step 3 now asserts the
# behaviour upstream actually has: adoption, in place, live.
#
# So the only restart worth probing here is whether the ADOPTED chain survives one.
# The relay has just replaced its entire history; if that left its ledger, ChainDB or
# LedgerSeq inconsistent, a restart is where it shows.
if [ "$CONVERGED" -eq 1 ] && [ "${GF_SKIP_RESTART_PROBE:-0}" -ne 1 ]; then
    step "5. the adopted chain must survive a restart"

    PRE_RESTART_BLK=$(tip_field "$LD_RELAY_SOCK" .block); PRE_RESTART_BLK=${PRE_RESTART_BLK:-0}
    stop_pid_file "$LD_STATE/dugite-relay.pid" || note "dugite-relay did not exit within 60s of SIGTERM"
    RELAY_RESTART_MARK=$(wc -l < "$LD_LOGS/dugite-relay.log" 2>/dev/null)
    start_dugite dugite-relay "$LD_RELAY_PORT" "$LD_DUGITE_RELAY_METRICS_PORT" "$LD_RELAY_SOCK"
    wait_for_socket "$LD_RELAY_SOCK" 180 >/dev/null 2>&1

    R_RELAY_CONVERGED=0
    deadline=$(( $(date +%s) + CONVERGE_TIMEOUT ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        sleep 5
        RH=$(tip_field "$LD_RELAY_SOCK" .hash)
        CH=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
        if [ -n "$RH" ] && [ -n "$CH" ] && [ "$RH" = "$CH" ]; then
            R_RELAY_CONVERGED=1
            break
        fi
    done
    R_RELAY_BLK=$(tip_field "$LD_RELAY_SOCK" .block)
    R_CBP=$(tip_field "$LD_CARDANO_BP_SOCK" .block)

    if [ "$R_RELAY_CONVERGED" -eq 1 ]; then
        ok "the adopted chain survived a restart: dugite-relay came back on it and matches cardano-bp at block ${R_RELAY_BLK:-?} (was ${PRE_RESTART_BLK} before the stop)"
    elif [ -z "$R_RELAY_BLK" ]; then
        # UNMEASURED is not the same as FAILED, and conflating them cost three runs.
        inconc "dugite-relay's tip is UNREADABLE after the restart, so whether it held the adopted chain is unknown — not a failure, an unmeasured result"
        explain_tip_failure "dugite-relay" "$LD_RELAY_SOCK"
    else
        bad "dugite-relay did NOT re-converge with cardano-bp within ${CONVERGE_TIMEOUT}s of restarting on the adopted chain (relay=${R_RELAY_BLK} cardano-bp=${R_CBP:-?}) — the adoption did not persist"
    fi

    if awk -v m="${RELAY_RESTART_MARK:-0}" 'NR > m && /LedgerSeq was incoherent/ {c++} END{exit (c+0)>0}' \
            "$LD_LOGS/dugite-relay.log" 2>/dev/null; then
        ok "no LedgerSeq incoherence after restarting on the adopted chain"
    else
        bad "dugite-relay logged 'LedgerSeq was incoherent' after restarting on the adopted chain"
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
    echo "GENESIS-FORK ROUND: PASS — a node holding its own genesis-rooted chain, restarted while stranded, ADOPTED the peer's chain IN PLACE on the live path and held it across a further restart. No marker, no operator action, no discarded chain: the behaviour cardano-node has via Paths.hs::isReachable's anchorIsGenesis case."
    ./stop.sh >/dev/null 2>&1
    exit 0
fi
