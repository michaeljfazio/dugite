---
name: Forge loop Haskell alignment
description: MAX_FORGE_LAG_SLOTS=60 removed; TraceNoLedgerView gate and full Haskell check sequence added
type: project
---

MAX_FORGE_LAG_SLOTS=60 had no Haskell analog and was a dugite invention. It fired a
WARN every second whenever the inter-block gap exceeded 60 slots on Conway preview —
which is completely normal with f=0.05 (empty 60-slot windows are common). It also
suppressed legitimate forge attempts.

**What replaced it:** `dugite_consensus::stability_window_slots(k, f)` = `ceil(3*k/f)`.
For preview/mainnet (k=2160, f=0.05) this is **129 600 slots = 36 hours**. Gate now
fires `TraceNoLedgerView` (ERROR) only when `current_slot - tip_slot > 129 600`.

**Why:** Matches Haskell's `forecastFor` failure condition in
`Ouroboros.Consensus.NodeKernel.forkBlockForging`. The only stale-tip gate Haskell has.

**How to apply:** Do NOT re-add any sub-stability-window forge lag checks. The 36h
window is intentionally lenient; the Haskell design relies on the relay always being
near tip, not on the BP refusing to forge when slightly behind.

## Full Haskell-aligned check sequence (try_forge_block_at)

1. TraceStartLeadershipCheck (INFO, every slot)
2. TraceBlockFromFuture (ERROR) — tip_slot >= current_slot
3. TraceSlotIsImmutable (ERROR) — immutable_tip_slot == current_slot
4. TraceBlockContext (DEBUG) — prev_point logged
5. TraceNoLedgerState (ERROR) / TraceLedgerState (DEBUG) — ledger state check
6. TraceNoLedgerView (ERROR) / TraceLedgerView (DEBUG) — stability window gate
7. TraceNodeNotLeader (INFO) / TraceNodeIsLeader (INFO) — VRF leader election
8. TraceForgeTickedLedgerState (DEBUG) / TraceForgingMempoolSnapshot (DEBUG)
9. TraceForgedBlock (INFO) — block constructed
10. TraceAdoptedBlock (INFO) / TraceDidntAdoptBlock (ERROR) / TraceForgedInvalidBlock (ERROR)

## stability_window_slots location
- `dugite_consensus::stability_window_slots(k: u64, f: f64) -> u64` in
  `crates/dugite-consensus/src/lib.rs`
- Unit tests in the same file: `stability_window_slots_preview`, `_mainnet`,
  `_rounds_up`, `_zero_f_returns_max`
- Node-level gate unit tests in `crates/dugite-node/src/node/mod.rs`:
  `stability_window_slots_preview`, `forge_gate_block_from_future`,
  `forge_gate_slot_is_immutable`, `forge_gate_no_ledger_view_fires_*`,
  `forge_gate_no_ledger_view_does_not_fire_for_small_lag`,
  `forge_gate_no_ledger_view_boundary`
