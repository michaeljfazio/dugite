#!/usr/bin/env bash
# rollback-round.sh — the rollback-vs-securityParam contract.               (#1040)
#
# Adopted from upstream cardano-node-tests test_rollback.py
# (test_consensus_reached / test_permanent_fork): partition the network into
# two forging groups, let them build competing chains, heal, and assert the
# ONE invariant Ouroboros makes about the outcome —
#
#   rollback depth <  k  =>  the network MUST reconverge on one chain
#   rollback depth >  k  =>  the two chains MUST NOT reconverge, ever
#
# — a contract this repo had NEVER exercised end-to-end. #957 (two-forger-
# round.sh) proved forks HAPPEN and resolve during routine slot battles, but
# every observed rollback there was shallow (well under k) by construction:
# nothing in that round ever tries to make one side's fork exceed the volatile
# window. This round's whole job is the two cases that round cannot reach.
#
# TOPOLOGY (reuses #957's two-forger devnet, unmodified)
#
#     dugite-bp(pool1) ──┐                   ┌── cardano-bp(pool2)
#          :3001         ├── dugite-relay ───┤        :3003
#                        │      :3002        │
#                        └── cardano-arbiter ┘
#                                :3004
#
# The actual TCP edges (config/templates/*.topology.tmpl.json) are a 4-node
# graph, NOT a star through the relay:
#   dugite-bp    <-> dugite-relay      (3001 <-> 3002)   same-group
#   cardano-bp   <-> dugite-relay      (3003 <-> 3002)   CROSS-group
#   cardano-arbiter <-> dugite-bp      (3004 <-> 3001)   CROSS-group
#   cardano-arbiter <-> cardano-bp     (3004 <-> 3003)   same-group
# (dugite-relay never peers with cardano-arbiter; dugite-bp never peers with
# cardano-bp directly — see the four topology templates.)
#
# PARTITION MECHANISM — group-pair pfctl/iptables rules, not
# chaos/network-partition.sh as-is
# ----------------------------------------------------------------------------
# chaos/network-partition.sh blocks ONE port ("from any to any port
# $LD_RELAY_PORT") to isolate a single node from the whole network. That rule
# shape cannot express a two-GROUP partition here: blocking all traffic to
# port 3002 would also sever dugite-bp<->dugite-relay (same-group), and
# blocking port 3003 would also sever cardano-arbiter<->cardano-bp
# (same-group) — both nodes on every side of this topology are on 127.0.0.1,
# so a destination-port-only rule cannot distinguish "which neighbour sent
# this" the way distinct source IPs would on a real network.
#
# What DOES distinguish them: dugite and cardano-node both bind OUTBOUND
# dial sockets to their own configured N2N port (SO_REUSEPORT — see
# ConnectionId's (local, remote)-keyed connections, needed so a peer's
# inbound-accepted and outbound-dialed sessions to the SAME remote correlate
# to one logical duplex connection). So every packet on a CROSS-group edge
# carries one of {3002,3003} or one of {3001,3004} on BOTH sides, and every
# packet on a SAME-group edge never does. Blocking by exact (src port, dst
# port) PAIR — not by destination port alone — isolates precisely the two
# cross edges and leaves both same-group edges untouched:
#
#   block tcp  3002 <-> 3003   (dugite-relay <-> cardano-bp)
#   block tcp  3001 <-> 3004   (dugite-bp    <-> cardano-arbiter)
#
# This round therefore SOURCES chaos/lib.sh (OS/tool detection —
# $CHAOS_NET_TOOL, $CHAOS_OS — plus chaos_require_net_tool, count_matching,
# chaos_record, log_info/log_warn/die) and reuses network-partition.sh's exact
# pfctl-anchor / iptables idiom (anchor name, `-f -` piped ruleset, `-F rules`
# teardown), but under its OWN anchor ("dugite-chaos-rollback", distinct from
# network-partition.sh's "dugite-chaos" so a concurrently running chaos
# scenario can never collide with or clobber this round's rules) and with the
# port-PAIR ruleset above instead of network-partition.sh's single-port one.
# lib.sh does not (and, given the above, cannot) expose a generic "partition
# these two node-port groups" helper — this round's `partition_groups_start` /
# `partition_groups_end` are the missing piece, written to the SAME idiom.
#
# PRIVILEGE HANDLING
# ----------------------------------------------------------------------------
# network-partition.sh calls `sudo pfctl` / `sudo iptables` directly with no
# preflight — in an environment without passwordless sudo that either hangs
# on a password prompt or fails deep inside the scenario. This round checks
# `sudo -n true` BEFORE touching the devnet at all (privilege is a pure
# environment predicate; there is no reason to pay a two-forger boot to learn
# we cannot partition anything) and ENV_SKIPs loudly — a distinct exit code
# (3), a csv row, and a printed reason — rather than exiting 0 (which would
# read as "0 failures" i.e. PASS) or silently doing nothing (#959's rule).
#
# BLOCK-BUDGET MATH (k=40, activeSlotsCoeff f=0.5, slotLength=1s — see
# genesis/shelley-genesis.json; pool split 60/40 dugite/cardano, per #957's
# LD_POOL2_STAKE_PCT default)
# ----------------------------------------------------------------------------
# This round uses the LINEAR approximation the issue itself sizes against —
# blocks-per-side over T seconds ~= T * f * share — NOT the exact Praos
# per-slot leader probability P(sigma) = 1 - (1-f)^sigma. At f=0.5 the exact
# formula gives the 40% side a slightly HIGHER rate (1-0.5^0.4 = 0.242 vs the
# linear 0.5*0.4 = 0.200), so the linear estimate below is a conservative
# UNDERESTIMATE for the minority pool — real forging should clear the k=40
# bar with a bit more margin than the numbers here claim, never less.
#
#   dugite side (pool1, 60%): rate ~= 0.5 * 0.6 = 0.30 blocks/s
#   cardano side (pool2, 40%): rate ~= 0.5 * 0.4 = 0.20 blocks/s
#
#   Case 1 (reconverge, < k):  T=30s  => ~9 blocks dugite-side, ~6 cardano-side
#     both well under k=40 — a shallow fork, expected to heal.
#
#   Case 2 (permanent fork, > k): the issue's original 150s figure assumes
#     ~1 block/2s (0.5/s) on BOTH sides, which only holds for the 60% side;
#     at 0.20/s the 40% side would still be at ~30 blocks (under k) after
#     150s. Sized instead so BOTH sides clear k=40 with margin:
#       T=260s => ~78 blocks dugite-side (95% margin over k=40)
#                 ~52 blocks cardano-side (30% margin over k=40)
#     The 30% cardano-side margin is the tighter one and is a real open risk
#     under forging variance (see "Open risks" in the review) — RB_CASE2_SEC
#     is an env override if a live run needs more headroom.
#
# RED-PROOFS (marked `# RED-PROOF:` at point of use — this is a static
# delivery; live execution happens centrally, see the round's header commit)
#   - Case 1's mid-partition divergence probe fires at T+RB_CASE1_SEC/2 by
#     design, giving both sides forging time first. Setting
#     RB_DIVERGE_PROBE_DELAY=0 forces the probe to run at the INSTANT the
#     partition is installed, before either side can have built a single
#     divergent block — the assertion must then legitimately FAIL.
#   - Case 2's "no convergence over 120s" assertion samples the same
#     same-group/cross-group hash comparison Case 1 uses to prove
#     divergence. Setting RB_CASE2_SEC below the cardano-side k crossover
#     (e.g. RB_CASE2_SEC=60, squarely inside Case 1's own regime) must flip
#     this same assertion to FAIL, because the network genuinely reconverges
#     within the 120s window — proving the check is sensitive to real state,
#     not hardcoded to pass.
#
# Usage:
#   ./rollback-round.sh                     # full round (~10-12 min)
#   RB_CASE1_SEC=30 RB_CASE2_SEC=260 ./rollback-round.sh
#   RB_SKIP_SETUP=1 ./rollback-round.sh      # reuse a running two-forger devnet
#
# This round is TERMINAL: Case 2 deliberately leaves the devnet's four nodes
# on two chains that can never reconcile, so it always ends with a full
# `./stop.sh`, regardless of RB_SKIP_SETUP.

set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2

RB_SKIP_SETUP="${RB_SKIP_SETUP:-0}"
RB_CASE1_SEC="${RB_CASE1_SEC:-30}"
RB_CASE2_SEC="${RB_CASE2_SEC:-260}"
RB_DIVERGE_PROBE_DELAY="${RB_DIVERGE_PROBE_DELAY:-$((RB_CASE1_SEC / 2))}"
RB_RECONVERGE_TIMEOUT="${RB_RECONVERGE_TIMEOUT:-60}"
RB_CASE2_NO_CONVERGE_WINDOW="${RB_CASE2_NO_CONVERGE_WINDOW:-120}"
RB_CASE2_SAMPLE_INTERVAL="${RB_CASE2_SAMPLE_INTERVAL:-15}"
RB_MIN_PREFIX_HEIGHT="${RB_MIN_PREFIX_HEIGHT:-45}"
RB_PREFIX_TIMEOUT="${RB_PREFIX_TIMEOUT:-420}"
EX_ENV_SKIP=3

FAILURES=0
step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }

. ./lib/common.sh
set +e

# Predefine EVIDENCE_DIR before sourcing chaos/lib.sh so its own
# chaos-events.csv co-locates with this round's rollback-round.csv in the
# same timestamped directory instead of minting a second one.
EVIDENCE_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
. ./chaos/lib.sh
set +e
. ./lib/expect-log-errors.sh

RB_CSV="$EVIDENCE_DIR/rollback-round.csv"
echo "ts,case,check,outcome,detail" > "$RB_CSV"
rb_evidence() { # rb_evidence <case> <check> <outcome> <detail>
    printf '%s,%s,%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "$3" \
        "${4//,/;}" >> "$RB_CSV"
}
note "evidence: $RB_CSV"

# ---------------------------------------------------------------- helpers ----
tip_json() { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null; }
tip_field() { tip_json "$1" | jq -r "$2 // empty" 2>/dev/null; }

# Mirrors chaos/lib.sh's count_matching, but windowed by a log_mark line
# number rather than a `tail -n` suffix — the same awk-based idiom (never
# `grep -c ... || echo 0`, which prints two lines and breaks arithmetic tests;
# see chaos/lib.sh's own comment on this exact footgun).
since_mark_count() { # since_mark_count <log> <mark> <ere>
    local log="$1" mark="$2" re="$3"
    [ -f "$log" ] || { echo 0; return 0; }
    tail -n +"$((mark + 1))" "$log" 2>/dev/null \
        | awk -v re="$re" 'BEGIN{c=0} $0 ~ re {c++} END{print c+0}'
}

rollback_metric() { # rollback_metric <port> -> dugite_rollback_count_total, or 0
    local v
    v=$(curl -s "localhost:$1/metrics" 2>/dev/null | awk '/^dugite_rollback_count_total /{print $2}')
    echo "${v:-0}"
}

# ---------------------------------------------------------- 0. privilege gate --
step "0. privilege + tool check (must pass BEFORE paying a two-forger boot)"
if ! chaos_require_net_tool; then
    rb_evidence "setup" "net-tool" "ENV_SKIP" "no-net-tool-on-$CHAOS_OS"
    chaos_record "rollback-round" "skip" "0" "ENV_SKIP" "no-net-tool-$CHAOS_NET_TOOL"
    echo "ENV_SKIP: no $CHAOS_NET_TOOL available on $CHAOS_OS — cannot partition."
    exit "$EX_ENV_SKIP"
fi
if ! sudo -n true 2>/dev/null; then
    rb_evidence "setup" "sudo-privilege" "ENV_SKIP" "no-passwordless-sudo-for-$CHAOS_NET_TOOL"
    chaos_record "rollback-round" "skip" "0" "ENV_SKIP" "no-passwordless-sudo"
    echo "ENV_SKIP: 'sudo -n true' failed — sudo needs an interactive password in this"
    echo "  environment, so $CHAOS_NET_TOOL rules cannot be installed non-interactively."
    echo "  Grant passwordless sudo for $CHAOS_NET_TOOL (a scoped NOPASSWD sudoers entry)"
    echo "  to run this round for real. Exiting with a distinct code ($EX_ENV_SKIP), not 0,"
    echo "  so this is never read as '0 failures'."
    exit "$EX_ENV_SKIP"
fi
ok "privilege check: $CHAOS_NET_TOOL present on $CHAOS_OS, sudo usable non-interactively"
rb_evidence "setup" "privilege" "PASS" "$CHAOS_NET_TOOL,sudo-n-true"

# --------------------------------------------------- partition group helpers --
# Cross-group edges only (see header): dugite-relay<->cardano-bp (3002/3003)
# and dugite-bp<->cardano-arbiter (3001/3004). Same-group edges
# (dugite-bp<->dugite-relay: 3001/3002; cardano-arbiter<->cardano-bp:
# 3004/3003) never appear in this port-pair list and stay open throughout.
PF_ANCHOR_RB="dugite-chaos-rollback"

partition_groups_start() {
    case "$CHAOS_NET_TOOL" in
        pfctl)
            {
                printf 'block quick proto tcp from any port %d to any port %d\n' "$LD_RELAY_PORT" "$LD_CARDANO_BP_PORT"
                printf 'block quick proto tcp from any port %d to any port %d\n' "$LD_CARDANO_BP_PORT" "$LD_RELAY_PORT"
                printf 'block quick proto tcp from any port %d to any port %d\n' "$LD_DUGITE_BP_PORT" "$LD_CARDANO_ARBITER_PORT"
                printf 'block quick proto tcp from any port %d to any port %d\n' "$LD_CARDANO_ARBITER_PORT" "$LD_DUGITE_BP_PORT"
            } | sudo pfctl -a "$PF_ANCHOR_RB" -f - 2>/dev/null
            sudo pfctl -e 2>/dev/null || true
            ;;
        iptables)
            sudo iptables -A OUTPUT -p tcp --sport "$LD_RELAY_PORT" --dport "$LD_CARDANO_BP_PORT" -j DROP
            sudo iptables -A OUTPUT -p tcp --sport "$LD_CARDANO_BP_PORT" --dport "$LD_RELAY_PORT" -j DROP
            sudo iptables -A OUTPUT -p tcp --sport "$LD_DUGITE_BP_PORT" --dport "$LD_CARDANO_ARBITER_PORT" -j DROP
            sudo iptables -A OUTPUT -p tcp --sport "$LD_CARDANO_ARBITER_PORT" --dport "$LD_DUGITE_BP_PORT" -j DROP
            sudo iptables -A INPUT  -p tcp --sport "$LD_RELAY_PORT" --dport "$LD_CARDANO_BP_PORT" -j DROP
            sudo iptables -A INPUT  -p tcp --sport "$LD_CARDANO_BP_PORT" --dport "$LD_RELAY_PORT" -j DROP
            sudo iptables -A INPUT  -p tcp --sport "$LD_DUGITE_BP_PORT" --dport "$LD_CARDANO_ARBITER_PORT" -j DROP
            sudo iptables -A INPUT  -p tcp --sport "$LD_CARDANO_ARBITER_PORT" --dport "$LD_DUGITE_BP_PORT" -j DROP
            ;;
    esac
}

partition_groups_end() {
    case "$CHAOS_NET_TOOL" in
        pfctl)
            sudo pfctl -a "$PF_ANCHOR_RB" -F rules 2>/dev/null || true
            ;;
        iptables)
            sudo iptables -D OUTPUT -p tcp --sport "$LD_RELAY_PORT" --dport "$LD_CARDANO_BP_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D OUTPUT -p tcp --sport "$LD_CARDANO_BP_PORT" --dport "$LD_RELAY_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D OUTPUT -p tcp --sport "$LD_DUGITE_BP_PORT" --dport "$LD_CARDANO_ARBITER_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D OUTPUT -p tcp --sport "$LD_CARDANO_ARBITER_PORT" --dport "$LD_DUGITE_BP_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D INPUT  -p tcp --sport "$LD_RELAY_PORT" --dport "$LD_CARDANO_BP_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D INPUT  -p tcp --sport "$LD_CARDANO_BP_PORT" --dport "$LD_RELAY_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D INPUT  -p tcp --sport "$LD_DUGITE_BP_PORT" --dport "$LD_CARDANO_ARBITER_PORT" -j DROP 2>/dev/null || true
            sudo iptables -D INPUT  -p tcp --sport "$LD_CARDANO_ARBITER_PORT" --dport "$LD_DUGITE_BP_PORT" -j DROP 2>/dev/null || true
            ;;
    esac
}

partition_rules_present() {
    case "$CHAOS_NET_TOOL" in
        pfctl)
            [ -n "$(sudo pfctl -a "$PF_ANCHOR_RB" -s rules 2>/dev/null)" ]
            ;;
        iptables)
            sudo iptables -S OUTPUT 2>/dev/null | grep -q -- "--dport $LD_CARDANO_BP_PORT"
            ;;
    esac
}

# Registered NOW (before any partition exists) so every exit path — including
# a hard `exit` mid-Case-2 — removes the anchor/rules. `-F rules` on an empty
# or not-yet-loaded anchor is a documented no-op, so calling this before the
# first partition_groups_start is harmless.
trap 'partition_groups_end >/dev/null 2>&1 || true' EXIT

# --------------------------------------------------------------- setup + run --
if [ "$RB_SKIP_SETUP" -eq 0 ]; then
    step "setup + run (two-forger mode)"
    ./stop.sh >/dev/null 2>&1
    LD_TWO_FORGERS=1 LD_POOL2_STAKE_PCT="${RB_POOL2_PCT:-40}" ./setup.sh >/dev/null 2>&1 \
        || { echo "SETUP FAILED"; exit 2; }
    # NOT `./run.sh | tail || ...` — a pipeline's exit status is the LAST
    # command's, so `tail` (always 0) would mask a failed run.sh entirely.
    if ! ./run.sh > "state/rollback-run.log" 2>&1; then
        echo "RUN FAILED — last 20 lines:"
        tail -20 "state/rollback-run.log"
        exit 2
    fi
    tail -12 "state/rollback-run.log"
fi

[ -f "$LD_GENESIS/.two-forgers" ] || {
    echo "REFUSING TO RUN: $LD_GENESIS/.two-forgers is absent, so this is a"
    echo "single-forger devnet. Every assertion below would be vacuous."
    exit 2
}

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

step "2. common prefix established WELL past genesis (block >= $RB_MIN_PREFIX_HEIGHT, unanimous hash)"
# The two-forger genesis trap (SKILL.md): partitioning before all four nodes
# share a real, deep-enough prefix risks a genesis-depth fork that instantly
# exceeds k=40 and can NEVER reconverge — which would make Case 1 fail for a
# reason unrelated to the rollback contract this round exists to test.
PREFIX_OK=0
PREFIX_DEADLINE=$(( $(date +%s) + RB_PREFIX_TIMEOUT ))
PREFIX_HASH=""
PREFIX_HEIGHT=0
while [ "$(date +%s)" -lt "$PREFIX_DEADLINE" ]; do
    declare -a PH=() PB=()
    for s in "${SOCKS[@]}"; do
        PH+=("$(tip_field "$s" .hash)")
        PB+=("$(tip_field "$s" .block)")
    done
    same=1
    for h in "${PH[@]}"; do
        [ -z "$h" ] && { same=0; break; }
        [ "$h" != "${PH[0]}" ] && same=0
    done
    if [ "$same" -eq 1 ] && [ "${PB[0]:-0}" -ge "$RB_MIN_PREFIX_HEIGHT" ]; then
        PREFIX_OK=1
        PREFIX_HASH="${PH[0]}"
        PREFIX_HEIGHT="${PB[0]}"
        break
    fi
    sleep 5
done
if [ "$PREFIX_OK" -eq 1 ]; then
    ok "common prefix established: block=$PREFIX_HEIGHT hash=${PREFIX_HASH:0:16}… (>= $RB_MIN_PREFIX_HEIGHT)"
    rb_evidence "setup" "common-prefix" "PASS" "block=$PREFIX_HEIGHT hash=${PREFIX_HASH:0:16}"
else
    bad "no common prefix reached within ${RB_PREFIX_TIMEOUT}s"
    echo "REFUSING TO PARTITION: risk of a genesis-depth fork that can never reconverge."
    echo "  Do not shorten LD_GENESIS_DELAY (forced to >=150s in two-forger mode)."
    rb_evidence "setup" "common-prefix" "FAIL" "no-unanimous-tip-within-${RB_PREFIX_TIMEOUT}s"
    exit 1
fi

step "3. baseline metrics"
DBP_RB_BASE=$(rollback_metric "$LD_DUGITE_BP_METRICS_PORT")
DRELAY_RB_BASE=$(rollback_metric "$LD_DUGITE_RELAY_METRICS_PORT")
note "baseline dugite_rollback_count_total: dugite-bp=$DBP_RB_BASE dugite-relay=$DRELAY_RB_BASE"

# ═══════════════════════════════════════════════════════════════════════════
# CASE 1 — reconverge (rollback depth < k)                     (test_consensus_reached)
# ═══════════════════════════════════════════════════════════════════════════

step "4. CASE 1 — partition ~${RB_CASE1_SEC}s (expect ~9 dugite-side / ~6 cardano-side blocks, both < k=40)"
C1_DBP_MARK=$(log_mark "$LD_LOGS/dugite-bp.log")
C1_DRELAY_MARK=$(log_mark "$LD_LOGS/dugite-relay.log")
C1_CBP_MARK=$(log_mark "$LD_LOGS/cardano-bp.log")
C1_CARB_MARK=$(log_mark "$LD_LOGS/cardano-arbiter.log")

partition_groups_start
chaos_record "rollback-round" "case1-partition-start" "0" "IN_PROGRESS" "port-pairs=3002/3003+3001/3004 duration=${RB_CASE1_SEC}s"
rb_evidence "case1" "partition-start" "IN_PROGRESS" "duration=${RB_CASE1_SEC}s"

sleep "$RB_DIVERGE_PROBE_DELAY"

step "5. CASE 1 — verify the sides ACTUALLY diverged during the partition"
# RED-PROOF: this probe fires at T+RB_DIVERGE_PROBE_DELAY specifically so
# both sides have had forging time. Re-run with RB_DIVERGE_PROBE_DELAY=0 to
# see it correctly FAIL — at T+0 neither side has built a block yet, so the
# hashes are still equal and "diverged" is false, as it must be.
C1_D_HASH=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
C1_D_BLK=$(tip_field "$LD_DUGITE_BP_SOCK" .block)
C1_C_HASH=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
C1_C_BLK=$(tip_field "$LD_CARDANO_BP_SOCK" .block)
if [ -n "$C1_D_HASH" ] && [ -n "$C1_C_HASH" ] && [ "$C1_D_HASH" != "$C1_C_HASH" ]; then
    ok "sides diverged mid-partition: dugite=$C1_D_BLK/${C1_D_HASH:0:12} cardano=$C1_C_BLK/${C1_C_HASH:0:12}"
    rb_evidence "case1" "divergence-during-partition" "PASS" "dugite=$C1_D_BLK/${C1_D_HASH:0:12},cardano=$C1_C_BLK/${C1_C_HASH:0:12}"
else
    bad "sides did NOT diverge during the partition — the case measures nothing (dugite=${C1_D_BLK:-?}/${C1_D_HASH:-none} cardano=${C1_C_BLK:-?}/${C1_C_HASH:-none}); either forging stalled or the firewall rules are not actually blocking traffic"
    rb_evidence "case1" "divergence-during-partition" "FAIL" "dugite=${C1_D_BLK:-?}/${C1_D_HASH:-none},cardano=${C1_C_BLK:-?}/${C1_C_HASH:-none}"
fi

REMAINING=$(( RB_CASE1_SEC - RB_DIVERGE_PROBE_DELAY ))
[ "$REMAINING" -gt 0 ] && sleep "$REMAINING"

# Snapshot each node's tip right before healing — used below to attribute
# which side actually rolled back once everyone reconverges.
declare -a C1_PRE_HEAL=()
for s in "${SOCKS[@]}"; do C1_PRE_HEAL+=("$(tip_field "$s" .hash)"); done

partition_groups_end
chaos_record "rollback-round" "case1-partition-end" "$RB_CASE1_SEC" "IN_PROGRESS" "healed"
rb_evidence "case1" "partition-end" "IN_PROGRESS" "elapsed=${RB_CASE1_SEC}s"

step "6. CASE 1 — reconverge on ONE identical tip within ${RB_RECONVERGE_TIMEOUT}s"
C1_CONVERGED=0
C1_FINAL_HASH=""
C1_FINAL_BLK=""
DEADLINE=$(( $(date +%s) + RB_RECONVERGE_TIMEOUT ))
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    declare -a FT=()
    for s in "${SOCKS[@]}"; do FT+=("$(tip_field "$s" .hash)"); done
    same=1
    for t in "${FT[@]}"; do
        [ -z "$t" ] && { same=0; break; }
        [ "$t" != "${FT[0]}" ] && same=0
    done
    if [ "$same" -eq 1 ]; then
        C1_CONVERGED=1
        C1_FINAL_HASH="${FT[0]}"
        C1_FINAL_BLK=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .block)
        break
    fi
    sleep 5
done
if [ "$C1_CONVERGED" -eq 1 ]; then
    ok "all four nodes converged: block=$C1_FINAL_BLK hash=${C1_FINAL_HASH:0:16}…"
    rb_evidence "case1" "reconverge" "PASS" "block=$C1_FINAL_BLK,hash=${C1_FINAL_HASH:0:16}"
else
    DET=$(for i in "${!SOCKS[@]}"; do printf '%s=%s ' "${NAMES[$i]}" "$(tip_field "${SOCKS[$i]}" .hash | cut -c1-12)"; done)
    bad "did NOT reconverge within ${RB_RECONVERGE_TIMEOUT}s: $DET"
    rb_evidence "case1" "reconverge" "FAIL" "$DET"
fi

step "7. CASE 1 — the losing side actually rolled back"
# Signal pinned for dugite: Prometheus counter dugite_rollback_count_total
# (crates/dugite-node/src/metrics.rs:449,1669-1671), incremented ONLY inside
# the fork-switch code paths (crates/dugite-node/src/node/sync.rs:1316-1327,
# crates/dugite-node/src/node/mod.rs:6540-6553 and :10159-10170) alongside the
# log line "Chain selection: fork switch — rolling back ledger to
# intersection" (or "... fork switch at live tip ..."). It is NOT incremented
# by ordinary forward extension, so — unlike comparing pre/post tip hashes,
# which a winning side's own forward growth would also change — a metric
# delta unambiguously means a real switch_chain rollback happened.
# Signal pinned for cardano-node: the 'SwitchedToAFork' trace (same one
# two-forger-round.sh step 4 already established as meaningful).
DBP_RB_AFTER=$(rollback_metric "$LD_DUGITE_BP_METRICS_PORT")
DRELAY_RB_AFTER=$(rollback_metric "$LD_DUGITE_RELAY_METRICS_PORT")
DUGITE_ROLLED_BACK=0
[ "${DBP_RB_AFTER:-0}" -gt "${DBP_RB_BASE:-0}" ] && DUGITE_ROLLED_BACK=1
[ "${DRELAY_RB_AFTER:-0}" -gt "${DRELAY_RB_BASE:-0}" ] && DUGITE_ROLLED_BACK=1
DBP_SWITCH_LOG=$(since_mark_count "$LD_LOGS/dugite-bp.log" "$C1_DBP_MARK" 'Chain selection: fork switch')
DRELAY_SWITCH_LOG=$(since_mark_count "$LD_LOGS/dugite-relay.log" "$C1_DRELAY_MARK" 'Chain selection: fork switch')
CBP_SWITCHED=$(since_mark_count "$LD_LOGS/cardano-bp.log" "$C1_CBP_MARK" 'SwitchedToAFork')
CARB_SWITCHED=$(since_mark_count "$LD_LOGS/cardano-arbiter.log" "$C1_CARB_MARK" 'SwitchedToAFork')
note "dugite_rollback_count_total delta: dugite-bp=$((DBP_RB_AFTER - DBP_RB_BASE)) dugite-relay=$((DRELAY_RB_AFTER - DRELAY_RB_BASE))"
note "dugite 'Chain selection: fork switch' log lines: dugite-bp=$DBP_SWITCH_LOG dugite-relay=$DRELAY_SWITCH_LOG"
note "cardano 'SwitchedToAFork' log lines: cardano-bp=$CBP_SWITCHED cardano-arbiter=$CARB_SWITCHED"
if [ "$DUGITE_ROLLED_BACK" -eq 1 ] || [ "${CBP_SWITCHED:-0}" -gt 0 ] || [ "${CARB_SWITCHED:-0}" -gt 0 ]; then
    ok "a rollback/fork-switch signal fired on the losing side (dugite metric and/or cardano SwitchedToAFork)"
    rb_evidence "case1" "rollback-signal" "PASS" "dugite_delta=$((DBP_RB_AFTER - DBP_RB_BASE))+$((DRELAY_RB_AFTER - DRELAY_RB_BASE)),cardano_switched=$((CBP_SWITCHED + CARB_SWITCHED))"
else
    bad "neither side shows a rollback/fork-switch signal despite observed divergence+reconvergence — chain selection may not have actually run the switch"
    rb_evidence "case1" "rollback-signal" "FAIL" "no-signal-on-either-side"
fi
note "(pre-heal per-node tips for reference: $(for i in "${!NAMES[@]}"; do printf '%s=%.10s ' "${NAMES[$i]}" "${C1_PRE_HEAL[$i]}"; done))"

step "8. CASE 1 — zero invalid-block events (a fork must never mean a bad block)"
for log in cardano-bp cardano-arbiter; do
    case "$log" in
        cardano-bp) mark="$C1_CBP_MARK" ;;
        cardano-arbiter) mark="$C1_CARB_MARK" ;;
    esac
    INV=$(since_mark_count "$LD_LOGS/$log.log" "$mark" 'AddBlockValidation\.InvalidBlock|ForgedInvalidBlock')
    if [ "${INV:-0}" -eq 0 ]; then
        ok "$log: 0 invalid-block events since the case-1 partition"
        rb_evidence "case1" "invalid-blocks-$log" "PASS" "0-events"
    else
        bad "$log: $INV invalid-block event(s) — a forged block failed Haskell validation"
        rb_evidence "case1" "invalid-blocks-$log" "FAIL" "$INV-events"
    fi
done

step "9. CASE 1 — no unexpected error-class log lines"
for log in dugite-bp dugite-relay cardano-bp cardano-arbiter; do
    case "$log" in
        dugite-bp) mark="$C1_DBP_MARK" ;;
        dugite-relay) mark="$C1_DRELAY_MARK" ;;
        cardano-bp) mark="$C1_CBP_MARK" ;;
        cardano-arbiter) mark="$C1_CARB_MARK" ;;
    esac
    if assert_no_other_errors "$LD_LOGS/$log.log" "$mark" "./rollback-round.allowed-errors"; then
        ok "$log: no un-allowlisted error-class lines since the case-1 partition"
        rb_evidence "case1" "log-allowlist-$log" "PASS" "clean"
    else
        bad "$log: un-allowlisted error-class line(s) since the case-1 partition (see stderr above)"
        rb_evidence "case1" "log-allowlist-$log" "FAIL" "see-stderr"
    fi
done

# ═══════════════════════════════════════════════════════════════════════════
# CASE 2 — permanent fork (rollback depth > k), TERMINAL          (test_permanent_fork)
# ═══════════════════════════════════════════════════════════════════════════

step "10. CASE 2 — pre-check: still on one chain after case 1 (note-only)"
declare -a PRE2=()
for s in "${SOCKS[@]}"; do PRE2+=("$(tip_field "$s" .hash)"); done
same=1
for t in "${PRE2[@]}"; do [ -z "$t" ] && same=0; [ "$t" != "${PRE2[0]}" ] && same=0; done
if [ "$same" -eq 1 ]; then
    note "unanimous going into case 2"
else
    note "NOT unanimous going into case 2 — case 2's own divergence assertions are still meaningful (it partitions and asserts sustained divergence regardless of the exact starting split), but interpret with that in mind"
fi

step "11. CASE 2 — partition ~${RB_CASE2_SEC}s (expect ~78 dugite-side / ~52 cardano-side blocks, BOTH > k=40 with margin — see header math)"
C2_DBP_MARK=$(log_mark "$LD_LOGS/dugite-bp.log")
C2_DRELAY_MARK=$(log_mark "$LD_LOGS/dugite-relay.log")
C2_CBP_MARK=$(log_mark "$LD_LOGS/cardano-bp.log")
C2_CARB_MARK=$(log_mark "$LD_LOGS/cardano-arbiter.log")

partition_groups_start
chaos_record "rollback-round" "case2-partition-start" "0" "IN_PROGRESS" "port-pairs=3002/3003+3001/3004 duration=${RB_CASE2_SEC}s"
rb_evidence "case2" "partition-start" "IN_PROGRESS" "duration=${RB_CASE2_SEC}s"
sleep "$RB_CASE2_SEC"
partition_groups_end
chaos_record "rollback-round" "case2-partition-end" "$RB_CASE2_SEC" "IN_PROGRESS" "healed"
rb_evidence "case2" "partition-end" "IN_PROGRESS" "elapsed=${RB_CASE2_SEC}s"

step "12. CASE 2 — no leftover firewall rules right after healing (self-check)"
if partition_rules_present; then
    bad "$CHAOS_NET_TOOL still shows the rollback-round block rules after partition_groups_end"
    rb_evidence "case2" "firewall-cleared" "FAIL" "rules-still-present-post-heal"
else
    ok "$CHAOS_NET_TOOL shows no rollback-round block rules after healing"
    rb_evidence "case2" "firewall-cleared" "PASS" "clean"
fi

step "13. CASE 2 — sustained NON-convergence over ${RB_CASE2_NO_CONVERGE_WINDOW}s"
# RED-PROOF: this is the SAME same-group/cross-group hash comparison Case 1's
# step 5 uses to prove divergence. Re-run with RB_CASE2_SEC=60 (inside
# Case 1's own <k regime) and this assertion must FAIL — the network
# reconverges well within the window, exactly like Case 1's step 6, proving
# the check reacts to real state rather than a hardcoded verdict.
SAMPLES=0
BAD_SAMPLES=0
LAST_DETAIL=""
DEADLINE=$(( $(date +%s) + RB_CASE2_NO_CONVERGE_WINDOW ))
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    D_H=$(tip_field "$LD_DUGITE_BP_SOCK" .hash)
    R_H=$(tip_field "$LD_RELAY_SOCK" .hash)
    C_H=$(tip_field "$LD_CARDANO_BP_SOCK" .hash)
    A_H=$(tip_field "$LD_CARDANO_ARBITER_SOCK" .hash)
    SAMPLES=$((SAMPLES + 1))
    LAST_DETAIL="dbp=${D_H:0:10} drelay=${R_H:0:10} cbp=${C_H:0:10} carb=${A_H:0:10}"
    # A sample only counts as "stayed split" if BOTH same-group pairs still
    # agree internally (their own edge is never blocked) AND the two groups
    # disagree with each other.
    if [ -n "$D_H" ] && [ -n "$R_H" ] && [ -n "$C_H" ] && [ -n "$A_H" ] \
       && [ "$D_H" = "$R_H" ] && [ "$C_H" = "$A_H" ] && [ "$D_H" != "$C_H" ]; then
        :
    else
        BAD_SAMPLES=$((BAD_SAMPLES + 1))
        note "sample $SAMPLES did not show a clean two-way split: $LAST_DETAIL"
    fi
    sleep "$RB_CASE2_SAMPLE_INTERVAL"
done
if [ "$SAMPLES" -gt 0 ] && [ "$BAD_SAMPLES" -eq 0 ]; then
    ok "network stayed permanently split across $SAMPLES samples over ${RB_CASE2_NO_CONVERGE_WINDOW}s: $LAST_DETAIL"
    rb_evidence "case2" "no-convergence" "PASS" "samples=$SAMPLES,last=$LAST_DETAIL"
else
    bad "network did NOT stay split ($BAD_SAMPLES/$SAMPLES samples showed convergence or missing data) — either the sizing in the header math is wrong for this run, or reconciliation happened when it must not have: $LAST_DETAIL"
    rb_evidence "case2" "no-convergence" "FAIL" "bad_samples=$BAD_SAMPLES/$SAMPLES,last=$LAST_DETAIL"
fi

step "14. CASE 2 — dugite logs its fork-too-deep / max-rollback class"
# Two candidate dugite lines were found by grepping for
# 'ForkTooDeep|fork too deep|max rollback|rollback.*exceed' across crates/ —
# both are legitimate evidence of the same contract and either may fire
# first depending on whether ChainSync's FindIntersect still resolves inside
# the (10 000-block) volatile window or not:
#   (a) crates/dugite-node/src/node/sync.rs ~5377-5397 — FindIntersect only
#       matches at Origin and our chain is > k blocks along: INFO
#       "ChainSync intersection only at genesis (peer far behind our
#       immutable tip / disjoint chain) — ending ChainSync, demoting for
#       backoff (Haskell ForkTooDeep equivalent)"
#   (b) crates/dugite-storage/src/volatile_db.rs:1882-1891 — intersection IS
#       found (VolatileDB retains far more than k), but the resulting
#       rollback.len() exceeds max_rollback: WARN "VolatileDB: fork requires
#       rollback beyond k — refusing switch (Ouroboros k-finality; Haskell
#       treats deeper rollback as impossible)"
# Neither line is at ERROR level (INFO / WARN respectively), so neither is
# classified as an "error-class" line by LOG_ERROR_PATTERN — they do not
# need to be in the allowlist for assert_no_other_errors to pass, and are
# listed there only as defense-in-depth against a future severity bump.
FORKDEEP_PAT='ChainSync intersection only at genesis|fork requires rollback beyond k'
DBP_FORKDEEP=0
DRELAY_FORKDEEP=0
expect_log_errors "$LD_LOGS/dugite-bp.log" "$C2_DBP_MARK" "$FORKDEEP_PAT" 2>/dev/null && DBP_FORKDEEP=1
expect_log_errors "$LD_LOGS/dugite-relay.log" "$C2_DRELAY_MARK" "$FORKDEEP_PAT" 2>/dev/null && DRELAY_FORKDEEP=1
if [ "$DBP_FORKDEEP" -eq 1 ] || [ "$DRELAY_FORKDEEP" -eq 1 ]; then
    ok "dugite logged the fork-too-deep/max-rollback class (dugite-bp=$DBP_FORKDEEP dugite-relay=$DRELAY_FORKDEEP)"
    rb_evidence "case2" "fork-too-deep-signal" "PASS" "dugite-bp=$DBP_FORKDEEP,dugite-relay=$DRELAY_FORKDEEP"
else
    bad "neither dugite-bp nor dugite-relay logged the fork-too-deep/max-rollback class after the case-2 partition"
    rb_evidence "case2" "fork-too-deep-signal" "FAIL" "pattern-not-found"
fi
# Corroborating (not asserted): the equivalent Haskell trace names on the
# cardano side of the same two edges, for the same fault.
CBP_FORKDEEP_LOG=$(since_mark_count "$LD_LOGS/cardano-bp.log" "$C2_CBP_MARK" 'ForkTooDeep|IntersectNotFound')
CARB_FORKDEEP_LOG=$(since_mark_count "$LD_LOGS/cardano-arbiter.log" "$C2_CARB_MARK" 'ForkTooDeep|IntersectNotFound')
note "cardano-side corroboration: cardano-bp ForkTooDeep/IntersectNotFound=$CBP_FORKDEEP_LOG cardano-arbiter=$CARB_FORKDEEP_LOG"

step "15. CASE 2 — dugite stays UP and answers N2C queries (the permanent fork must not take the node down)"
for pair in "dugite-bp:$LD_DUGITE_BP_SOCK" "dugite-relay:$LD_RELAY_SOCK"; do
    nm="${pair%%:*}"; sock="${pair#*:}"
    TIP_OK=0
    PP_OK=0
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" >/dev/null 2>&1 && TIP_OK=1
    cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" --socket-path "$sock" >/dev/null 2>&1 && PP_OK=1
    if [ "$TIP_OK" -eq 1 ] && [ "$PP_OK" -eq 1 ]; then
        ok "$nm: N2C query tip + protocol-parameters both succeeded post-partition"
        rb_evidence "case2" "n2c-alive-$nm" "PASS" "tip+pparams-ok"
    else
        bad "$nm: N2C query failed post-partition (tip_ok=$TIP_OK pparams_ok=$PP_OK)"
        rb_evidence "case2" "n2c-alive-$nm" "FAIL" "tip_ok=$TIP_OK,pparams_ok=$PP_OK"
    fi
done

step "16. CASE 2 — final teardown (TERMINAL round: the devnet is unusable by design)"
# Unconditional regardless of RB_SKIP_SETUP — Case 2 always leaves the four
# nodes on two chains that can never reconcile (k-finality is unconditional),
# so there is no "keep it running for the next round" outcome here.
FINAL_STATE=$(for i in "${!SOCKS[@]}"; do printf '%s=%s/%.10s ' "${NAMES[$i]}" \
    "$(tip_field "${SOCKS[$i]}" .block)" "$(tip_field "${SOCKS[$i]}" .hash)"; done)
note "final (permanently split) state: $FINAL_STATE"
./stop.sh >/dev/null 2>&1
if partition_rules_present; then
    bad "$CHAOS_NET_TOOL still shows rollback-round rules after ./stop.sh — cleaning up now"
    partition_groups_end
    rb_evidence "case2" "teardown-firewall-clean" "FAIL" "had-to-force-clean"
else
    ok "no leftover $CHAOS_NET_TOOL rules after teardown"
    rb_evidence "case2" "teardown-firewall-clean" "PASS" "clean"
fi

step "SUMMARY"
if [ "$FAILURES" -eq 0 ]; then
    ok "rollback round: all assertions passed"
else
    bad "rollback round: $FAILURES assertion(s) failed"
fi
note "case 1 (<k): partition=${RB_CASE1_SEC}s, expect reconverge"
note "case 2 (>k): partition=${RB_CASE2_SEC}s, expect PERMANENT split"
note "final state: $FINAL_STATE"
exit "$FAILURES"
