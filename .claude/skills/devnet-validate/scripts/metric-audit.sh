#!/usr/bin/env bash
# metric-audit.sh — assert every dugite-node metric matches expectations.
#
# Where `health-probe.sh` answers "is the node healthy right now?" with the
# 14-step procedure, this script answers a different question: "is EVERY
# metric dugite exposes consistent with itself and with its peers?". It is
# the programmatic equivalent of a human operator walking through every
# field in dugite-monitor and checking that the number makes sense.
#
# Usage:
#   metric-audit.sh [--bp-port 12798] [--relay-port 12799]
#                   [--haskell-socket /tmp/ld-501/cbp.sock]
#                   [--magic 42] [--public] [--verbose]
#
# Scope:
#   - Reads every `dugite_*` metric from both BP and relay endpoints.
#   - Validates per-node arithmetic invariants (e.g. connected == hot + warm).
#   - Validates per-counter monotonicity (counters never decrease across
#     two scrapes — non-counter metrics are exempt).
#   - Cross-validates BP↔relay agreement (e.g. relay's blocks_received_total
#     ≈ BP's blocks_forged_total in single-forger topology).
#   - Cross-validates against Haskell `cardano-cli query tip` when available.
#   - Flags missing or zero-valued metrics that should be populated by now.
#
# Exit codes:
#   0 — all assertions pass
#   1 — one or more assertions failed
#   2 — usage / unreachable endpoint
set -euo pipefail

BP_PORT=12798
RELAY_PORT=12799
HASKELL_SOCKET=""
MAGIC=42
PUBLIC=0
VERBOSE=0
MONOTONIC_WINDOW=3

while [ $# -gt 0 ]; do
    case "$1" in
        --bp-port)         BP_PORT="$2"; shift 2 ;;
        --relay-port)      RELAY_PORT="$2"; shift 2 ;;
        --haskell-socket)  HASKELL_SOCKET="$2"; shift 2 ;;
        --magic)           MAGIC="$2"; shift 2 ;;
        --public)          PUBLIC=1; shift ;;
        --verbose)         VERBOSE=1; shift ;;
        -h|--help)         sed -n '2,/^set -e/p' "$0" | sed -e 's/^# \{0,1\}//' -e '$d'; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

# Auto-detect Haskell socket if not given.
if [ -z "$HASKELL_SOCKET" ]; then
    for cand in /tmp/ld-501/cbp.sock /tmp/ld-*/cbp.sock; do
        [ -S "$cand" ] && { HASKELL_SOCKET="$cand"; break; }
    done
fi

PASS=()
FAIL=()

ok()   { PASS+=("$1"); [ "$VERBOSE" -eq 1 ] && printf '  ✓ %s\n' "$1" || true; }
fail() { FAIL+=("$1"); printf '  ✗ %s\n' "$1"; }
info() { [ "$VERBOSE" -eq 1 ] && printf '  · %s\n' "$1" || true; }

toi() {
    awk -v v="${1:-}" 'BEGIN{ if (v ~ /^-?[0-9]+(\.[0-9]+)?$/) printf "%.0f", v; else print 0 }'
}

scrape() {
    local port="$1" out
    out=$(curl -fs --max-time 3 "localhost:${port}/metrics" 2>/dev/null) || return 1
    printf '%s' "$out"
}

metric() {
    awk -v k="$1" '$1==k {print $2; found=1; exit} END{ if(!found) print ""}'
}

# ---------------------------------------------------------------------------
# Read scrapes A (BP) and B (relay).
# ---------------------------------------------------------------------------
SCRAPE_BP="$(mktemp)"; SCRAPE_BP2="$(mktemp)"
SCRAPE_R="$(mktemp)";  SCRAPE_R2="$(mktemp)"
trap 'rm -f "$SCRAPE_BP" "$SCRAPE_BP2" "$SCRAPE_R" "$SCRAPE_R2"' EXIT

if ! scrape "$BP_PORT" > "$SCRAPE_BP"; then
    echo "FATAL: BP prometheus :${BP_PORT} unreachable" >&2
    exit 2
fi
if ! scrape "$RELAY_PORT" > "$SCRAPE_R"; then
    echo "FATAL: relay prometheus :${RELAY_PORT} unreachable" >&2
    exit 2
fi

# Per-node read helper (uses the scraped file).
m_bp()    { metric "$1" < "$SCRAPE_BP"; }
m_r()     { metric "$1" < "$SCRAPE_R"; }

# ---------------------------------------------------------------------------
# Phase 1: completeness — every metric dugite-monitor reads should be present
# and numeric on the BP scrape. Missing values = a regression in the
# metrics registry.
# ---------------------------------------------------------------------------
# Inventory the metrics dugite-monitor consumes (extracted from
# crates/dugite-monitor/src/*). When dugite-monitor grows, add the new
# names here.
DUGITE_MONITOR_METRICS=(
    dugite_block_number
    dugite_blocks_applied_total
    dugite_blocks_forged_total
    dugite_blocks_received_total
    dugite_blocks_announced_total
    dugite_block_apply_failures_total
    dugite_chainsync_idle_seconds
    dugite_committee_hot_count
    dugite_committee_no_confidence
    dugite_committee_total_count
    dugite_conn_duplex
    dugite_conn_full_duplex
    dugite_conn_inbound
    dugite_conn_outbound
    dugite_conn_terminating
    dugite_conn_unidirectional
    dugite_constitution_present
    dugite_delegation_count
    dugite_diffusion_mode
    dugite_disk_available_bytes
    dugite_disk_total_bytes
    dugite_disk_used_bytes
    dugite_drep_active
    dugite_drep_count
    dugite_epoch_length
    dugite_epoch_number
    dugite_forge_failures_total
    dugite_forge_race_lost_total
    dugite_forge_slot_battles_total
    dugite_gov_dormant_epochs
    dugite_is_block_producer
    dugite_leader_checks_total
    dugite_leader_checks_not_elected_total
    dugite_max_peer_tip_slot
    dugite_mempool_tx_count
    dugite_mempool_tx_max
    dugite_mempool_bytes
    dugite_n2c_connections_active
    dugite_n2n_connections_active
    dugite_n2n_connections_total
    dugite_network_magic
    dugite_peers_cold
    dugite_peers_connected
    dugite_peers_duplex
    dugite_peers_hot
    dugite_peers_inbound
    dugite_peers_outbound
    dugite_peers_warm
    dugite_pool_count
    dugite_proposal_count
    dugite_reserves_lovelace
    dugite_rollback_count_total
    dugite_slot_number
    dugite_snapshot_failed_total
    dugite_snapshot_worker_alive
    dugite_sync_progress_percent
    dugite_tip_age_seconds
    dugite_transactions_received_total
    dugite_transactions_rejected_total
    dugite_transactions_validated_total
    dugite_treasury_lovelace
    dugite_utxo_count
    dugite_utxo_flush_failed_total
    dugite_vote_delegation_count
    dugite_cpu_percent
    dugite_mem_resident_bytes
    dugite_mem_peak_bytes
    dugite_peer_rtt_avg_ms
    dugite_uptime_seconds
)
echo "── Phase 1: completeness (BP) ──"
n_missing=0
for m in "${DUGITE_MONITOR_METRICS[@]}"; do
    v=$(m_bp "$m")
    if [ -z "$v" ]; then
        fail "metric not exposed: $m (dugite-monitor would render N/A)"
        n_missing=$((n_missing + 1))
    fi
done
[ "$n_missing" -eq 0 ] && ok "all ${#DUGITE_MONITOR_METRICS[@]} dugite-monitor metrics present on BP"

# Known-gap: dugite-monitor renders these if present but the node does not
# yet emit them. Surface as info, not failure.
KNOWN_GAP_METRICS=(
    dugite_chain_density
    dugite_peer_handshake_rtt_ms
)
for m in "${KNOWN_GAP_METRICS[@]}"; do
    v=$(m_bp "$m")
    if [ -z "$v" ]; then
        info "known-gap: $m not exposed (dugite-monitor renders N/A)"
    else
        info "previously-gap: $m now exposed = $v — update KNOWN_GAP_METRICS"
    fi
done

# ---------------------------------------------------------------------------
# Phase 2: per-node arithmetic invariants
# ---------------------------------------------------------------------------
echo "── Phase 2: arithmetic invariants ──"

for node in bp relay; do
    if [ "$node" = "bp" ]; then mf=m_bp; else mf=m_r; fi
    h=$(toi "$($mf dugite_peers_hot)")
    w=$(toi "$($mf dugite_peers_warm)")
    c=$(toi "$($mf dugite_peers_connected)")
    cold=$(toi "$($mf dugite_peers_cold)")
    inb=$(toi "$($mf dugite_peers_inbound)")
    out=$(toi "$($mf dugite_peers_outbound)")
    dup=$(toi "$($mf dugite_peers_duplex)")

    if [ "$((h + w))" -eq "$c" ]; then
        ok "$node: peers_connected ($c) == peers_hot ($h) + peers_warm ($w)"
    else
        fail "$node: peers_connected ($c) != peers_hot ($h) + peers_warm ($w)"
    fi

    # Static-topology devnets should have no cold peers in steady state.
    if [ "$PUBLIC" -eq 0 ] && [ "$cold" -ne 0 ]; then
        fail "$node: peers_cold = $cold (expected 0 on static devnet)"
    else
        ok "$node: peers_cold = $cold"
    fi

    # Peer-count invariant — current dugite semantics (see audit-findings A8).
    # Haskell-canonical: connection counts ONCE in inbound XOR outbound by
    # Provenance. Dugite double-counts DuplexConn in both. So today's
    # arithmetic is `inbound + outbound - full_duplex == connected`; under
    # Haskell-correct semantics it would be `inbound + outbound == connected`.
    # Once dugite aligns (P1 — see audit findings), flip this check.
    cfd=$(toi "$($mf dugite_conn_full_duplex)")
    if [ "$((inb + out - cfd))" -eq "$c" ]; then
        ok "$node: peers_inbound ($inb) + peers_outbound ($out) - conn_full_duplex ($cfd) == peers_connected ($c) [current dugite semantics]"
        # Warn whenever the Haskell-canonical arithmetic doesn't hold so we
        # don't lose visibility of the underlying divergence.
        if [ "$cfd" -gt 0 ] && [ "$((inb + out))" -ne "$c" ]; then
            info "$node: peers_inbound + peers_outbound ($((inb+out))) != peers_connected ($c) — dugite DuplexConn double-counts by direction; Haskell counts by Provenance only (audit-findings A8)"
        fi
    else
        fail "$node: peers_inbound ($inb) + peers_outbound ($out) - conn_full_duplex ($cfd) != peers_connected ($c) — peer-count semantics broken"
    fi
    if [ "$dup" -ge "$cfd" ]; then
        ok "$node: peers_duplex ($dup) ≥ conn_full_duplex ($cfd)"
    else
        fail "$node: peers_duplex ($dup) < conn_full_duplex ($cfd) — should never happen"
    fi

    # Hot peer requirement once past boot.
    if [ "$h" -lt 1 ] && [ "$c" -ge 1 ]; then
        fail "$node: $c connected peers but zero hot — peer stuck in warm state"
    fi

    # Conn-mgr counters should be self-consistent.
    cfd=$(toi "$($mf dugite_conn_full_duplex)")
    cdup=$(toi "$($mf dugite_conn_duplex)")
    cuni=$(toi "$($mf dugite_conn_unidirectional)")
    cterm=$(toi "$($mf dugite_conn_terminating)")
    n2n_active=$(toi "$($mf dugite_n2n_connections_active)")
    if [ "$cterm" -ne 0 ]; then
        fail "$node: dugite_conn_terminating = $cterm (expected 0 in steady state)"
    else
        ok "$node: conn_terminating = 0"
    fi
done

# ---------------------------------------------------------------------------
# Phase 3: counter monotonicity over a short window
# ---------------------------------------------------------------------------
echo "── Phase 3: counter monotonicity (${MONOTONIC_WINDOW}s window) ──"
sleep "$MONOTONIC_WINDOW"
scrape "$BP_PORT"    > "$SCRAPE_BP2" || { fail "BP scrape #2 failed"; }
scrape "$RELAY_PORT" > "$SCRAPE_R2"  || { fail "relay scrape #2 failed"; }

m_bp2() { metric "$1" < "$SCRAPE_BP2"; }
m_r2()  { metric "$1" < "$SCRAPE_R2"; }

COUNTERS=(
    dugite_blocks_applied_total
    dugite_blocks_forged_total
    dugite_blocks_received_total
    dugite_blocks_announced_total
    dugite_block_apply_failures_total
    dugite_forge_failures_total
    dugite_forge_race_lost_total
    dugite_forge_slot_battles_total
    dugite_leader_checks_total
    dugite_leader_checks_not_elected_total
    dugite_n2n_connections_total
    dugite_rollback_count_total
    dugite_snapshot_failed_total
    dugite_transactions_received_total
    dugite_transactions_rejected_total
    dugite_transactions_validated_total
    dugite_utxo_flush_failed_total
)
n_mono_violations=0
for c in "${COUNTERS[@]}"; do
    for node in bp relay; do
        if [ "$node" = "bp" ]; then v1=$(toi "$(m_bp "$c")"); v2=$(toi "$(m_bp2 "$c")"); else v1=$(toi "$(m_r "$c")"); v2=$(toi "$(m_r2 "$c")"); fi
        if [ "$v2" -lt "$v1" ]; then
            fail "$node: counter $c decreased ($v1 → $v2)"
            n_mono_violations=$((n_mono_violations + 1))
        fi
    done
done
[ "$n_mono_violations" -eq 0 ] && ok "all ${#COUNTERS[@]} counters monotonic across ${MONOTONIC_WINDOW}s window on both nodes"

# ---------------------------------------------------------------------------
# Phase 4: cross-node consistency
# ---------------------------------------------------------------------------
echo "── Phase 4: cross-node consistency ──"

# Tip parity: BP and relay should have block_number within ±1.
bp_block=$(toi "$(m_bp2 dugite_block_number)")
r_block=$(toi "$(m_r2 dugite_block_number)")
gap=$((bp_block > r_block ? bp_block - r_block : r_block - bp_block))
if [ "$gap" -le 1 ]; then
    ok "block_number parity: BP=$bp_block relay=$r_block (gap=$gap ≤ 1)"
else
    fail "block_number parity: BP=$bp_block relay=$r_block (gap=$gap > 1)"
fi

# Epoch must match exactly.
bp_epoch=$(toi "$(m_bp2 dugite_epoch_number)")
r_epoch=$(toi "$(m_r2 dugite_epoch_number)")
if [ "$bp_epoch" -eq "$r_epoch" ]; then
    ok "epoch parity: $bp_epoch on both"
else
    fail "epoch parity: BP=$bp_epoch relay=$r_epoch"
fi

# Network magic must match between nodes.
bp_mag=$(toi "$(m_bp2 dugite_network_magic)")
r_mag=$(toi "$(m_r2 dugite_network_magic)")
if [ "$bp_mag" -eq "$r_mag" ]; then
    ok "network_magic parity: $bp_mag on both"
else
    fail "network_magic mismatch: BP=$bp_mag relay=$r_mag"
fi

# Forge↔receive balance: in single-forger topology, the relay's
# `blocks_received_total` should track BP's `blocks_forged_total` minus
# a small in-flight gap.
bp_forged=$(toi "$(m_bp2 dugite_blocks_forged_total)")
r_received=$(toi "$(m_r2 dugite_blocks_received_total)")
diff_fr=$((bp_forged - r_received))
if [ "$diff_fr" -ge -1 ] && [ "$diff_fr" -le 2 ]; then
    ok "forge↔receive balance: BP forged=$bp_forged, relay received=$r_received (gap=$diff_fr)"
else
    fail "forge↔receive imbalance: BP forged=$bp_forged, relay received=$r_received (gap=$diff_fr — expected ≤2)"
fi

# Topology sanity: in single-forger devnet, BP should NOT have received
# blocks (cardano-bp is non-forging). Flag clearly so operator isn't
# confused.
bp_received=$(toi "$(m_bp2 dugite_blocks_received_total)")
if [ "$bp_received" -eq 0 ]; then
    info "BP blocks_received_total=0 — expected on single-forger devnet (cardano-bp is non-forging). To exercise BP receive-path, use a bp-pair topology where cardano-bp also forges."
else
    info "BP blocks_received_total=$bp_received — multi-forger topology exercised."
fi

# Topology-specific hot-count expectation (Haskell semantics).
# 3-node devnet: BP↔relay↔cardano-bp. Each pair is a local-root link with
# HotValency=1, so under Haskell semantics every node should report
# `peers_hot == #_of_neighbors`:
#   BP    → 1 hot (relay)
#   relay → 2 hot (BP + cardano-bp)
# Today dugite reports inflated counts (audit-findings A8.divergence-3 —
# each DuplexConn peer is counted twice). Emit an INFO until the dugite
# fix lands; flip to a hard assertion afterwards.
bp_hot=$(toi "$(m_bp2 dugite_peers_hot)")
r_hot=$(toi "$(m_r2 dugite_peers_hot)")
expect_bp=1; expect_relay=2
if [ "$bp_hot" -eq "$expect_bp" ] && [ "$r_hot" -eq "$expect_relay" ]; then
    ok "Haskell-canonical hot count: BP=$bp_hot (≥$expect_bp), relay=$r_hot (≥$expect_relay) — devnet topology matches"
else
    info "hot-count divergence vs Haskell-canonical: BP=$bp_hot (expected $expect_bp), relay=$r_hot (expected $expect_relay) — see audit-findings A8.divergence-3"
fi

# is_block_producer should be 1 on BP, 0 on relay.
bp_isbp=$(toi "$(m_bp2 dugite_is_block_producer)")
r_isbp=$(toi "$(m_r2 dugite_is_block_producer)")
if [ "$bp_isbp" -eq 1 ] && [ "$r_isbp" -eq 0 ]; then
    ok "role assignment: BP=is_block_producer=1, relay=0"
else
    fail "role assignment broken: BP=$bp_isbp (expected 1), relay=$r_isbp (expected 0)"
fi

# ---------------------------------------------------------------------------
# Phase 5: Haskell cross-validation (via cardano-cli on the socket).
# ---------------------------------------------------------------------------
echo "── Phase 5: Haskell cross-validation ──"
if [ -n "$HASKELL_SOCKET" ] && [ -S "$HASKELL_SOCKET" ]; then
    if h_tip=$(cardano-cli query tip --testnet-magic "$MAGIC" --socket-path "$HASKELL_SOCKET" 2>/dev/null); then
        h_block=$(printf '%s' "$h_tip" | jq -r .block)
        h_slot=$(printf '%s' "$h_tip" | jq -r .slot)
        h_epoch=$(printf '%s' "$h_tip" | jq -r .epoch)
        h_era=$(printf '%s' "$h_tip" | jq -r .era)

        bp_slot=$(toi "$(m_bp2 dugite_slot_number)")
        slot_gap=$((bp_slot > h_slot ? bp_slot - h_slot : h_slot - bp_slot))
        block_gap=$((bp_block > h_block ? bp_block - h_block : h_block - bp_block))

        # Devnet steady-state tolerance.
        slot_tol=5; block_tol=1
        [ "$PUBLIC" -eq 1 ] && { slot_tol=20; block_tol=2; }

        if [ "$slot_gap" -le "$slot_tol" ] && [ "$block_gap" -le "$block_tol" ] && [ "$h_epoch" = "$bp_epoch" ]; then
            ok "Haskell parity: cardano-bp(slot=$h_slot block=$h_block epoch=$h_epoch era=$h_era) ≈ dugite-bp(slot=$bp_slot block=$bp_block epoch=$bp_epoch)"
        else
            fail "Haskell parity drift: cardano-bp(slot=$h_slot block=$h_block epoch=$h_epoch) vs dugite-bp(slot=$bp_slot block=$bp_block epoch=$bp_epoch) — slot_gap=$slot_gap (≤$slot_tol) block_gap=$block_gap (≤$block_tol)"
        fi
    else
        info "Haskell socket present but query tip failed — skipping parity"
    fi
else
    info "Haskell socket not provided/found — skipping Phase 5"
fi

# ---------------------------------------------------------------------------
# Phase 6: range / sanity checks on selected gauges
# ---------------------------------------------------------------------------
echo "── Phase 6: range checks ──"

# tip_age, chainsync_idle, sync_progress
for node in bp relay; do
    if [ "$node" = "bp" ]; then mf=m_bp2; else mf=m_r2; fi
    age=$(toi "$($mf dugite_tip_age_seconds)")
    idle=$(toi "$($mf dugite_chainsync_idle_seconds)")
    sync=$(toi "$($mf dugite_sync_progress_percent)")
    tol_age=5; tol_idle=10
    [ "$PUBLIC" -eq 1 ] && { tol_age=60; tol_idle=60; }
    if [ "$age" -gt "$tol_age" ]; then fail "$node: tip_age_seconds=$age > $tol_age"; else ok "$node: tip_age_seconds=$age"; fi
    if [ "$idle" -gt "$tol_idle" ]; then fail "$node: chainsync_idle_seconds=$idle > $tol_idle"; else ok "$node: chainsync_idle_seconds=$idle"; fi
    if [ "$sync" -lt 10000 ]; then
        fail "$node: sync_progress_percent=$sync (expected 10000 = 100% at-tip)"
    else
        ok "$node: sync_progress_percent=10000 (100%)"
    fi
done

# mempool bytes / tx_count vs max
mt=$(toi "$(m_bp2 dugite_mempool_tx_count)")
mm=$(toi "$(m_bp2 dugite_mempool_tx_max)")
if [ "$mt" -gt "$mm" ]; then
    fail "BP: mempool_tx_count=$mt > mempool_tx_max=$mm — capacity violated"
else
    ok "BP: mempool_tx_count=$mt ≤ max=$mm"
fi

# disk available > 1 GB.
disk_avail=$(toi "$(m_bp2 dugite_disk_available_bytes)")
if [ "$disk_avail" -lt $((1024 * 1024 * 1024)) ]; then
    fail "BP: disk_available_bytes=$disk_avail < 1 GiB"
else
    ok "BP: disk_available_bytes=$(awk -v v="$disk_avail" 'BEGIN{printf "%.1f", v/(1024*1024*1024)}') GiB"
fi

# Snapshot worker alive.
sa=$(toi "$(m_bp2 dugite_snapshot_worker_alive)")
if [ "$sa" -eq 1 ]; then ok "BP: snapshot_worker_alive=1"; else fail "BP: snapshot_worker_alive=$sa (worker died)"; fi

# Treasury / reserves: must be non-negative; once past epoch 0 boundary,
# treasury should be > 0 unless explicit zero-bound passed.
res=$(toi "$(m_bp2 dugite_reserves_lovelace)")
trs=$(toi "$(m_bp2 dugite_treasury_lovelace)")
if [ "$res" -lt 0 ]; then fail "BP: reserves_lovelace=$res < 0 — accounting bug"; else ok "BP: reserves_lovelace=$res"; fi
if [ "$trs" -lt 0 ]; then fail "BP: treasury_lovelace=$trs < 0 — accounting bug"; else ok "BP: treasury_lovelace=$trs"; fi

# UTxO count must increase or stay (never < 1 on a started devnet with funded wallets).
utx=$(toi "$(m_bp2 dugite_utxo_count)")
if [ "$utx" -lt 1 ]; then fail "BP: utxo_count=$utx (devnet should have funded UTxOs)"; else ok "BP: utxo_count=$utx"; fi

# ---------------------------------------------------------------------------
# Verdict
# ---------------------------------------------------------------------------
echo
echo "──────────────────────────────────────────────────────────────────"
echo "metric-audit — summary"
echo "  passed: ${#PASS[@]}"
echo "  failed: ${#FAIL[@]}"
if [ ${#FAIL[@]} -eq 0 ]; then
    echo "verdict: ALL METRICS CONSISTENT"
    exit 0
else
    echo "verdict: FAILED"
    for f in "${FAIL[@]}"; do echo "  - $f"; done
    exit 1
fi
