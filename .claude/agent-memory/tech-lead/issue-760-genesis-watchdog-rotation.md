---
name: issue-760-genesis-watchdog-rotation
description: #760-A genesis cold-restart wedge root cause — #742 unproductive-dynamo watchdog (connection_lifecycle.rs:2464) rotates a legitimately-parked genesis dynamo every 30s. FIX (6212c2895b): rotate only genuinely-silent dynamos (csj fragment NOT ahead of chain), keep parked ones. The blunt !is_genesis_bulk_sync flip is WRONG (re-opens #742).
metadata:
  type: project
---

## Root Cause

`connection_lifecycle.rs:2464` — the "unproductive-claim" watchdog for CSJ dynamos fires after `3 × grace_period = 30s` when `fetch_runs.is_empty()` AND `is_genesis_bulk_sync`. On cold genesis restart from a far-below snapshot, the dynamo intersects at the snapshot tip (correct) and begins streaming in-horizon headers, but:

1. `pending_headers` drains faster than the ledger advances (BlockFetch is faster than the forecast window can expand)
2. The dynamo parks in `forecast_park_or_disconnect` (sync.rs:5247) waiting for ledger to advance
3. While parked, BlockFetch polls every 10ms, sees `fetch_runs.is_empty()`, increments `unproductive_since_ms`
4. After 30s the watchdog fires: `rotate_dynamo` → new dynamo re-intersects at same frozen tip → re-parks → repeats
5. Net result: ~1 blk/min (30s/rotation × negligible blocks before park)

**Why:** `block_fetch_grace_period = 10.0s` (config.rs:118), so `3 × 10s = 30s` threshold.

## The Circular Dependency

- Ledger advance requires LoE to advance (trimToLoE cap)
- LoE requires dynamo fragment to fill (shared_candidate_prefix in gsm.rs:923)
- Dynamo fragment fills only when in-horizon headers arrive (forecast_park_or_disconnect passes)
- Forecast horizon only advances when ledger advances

On cold restart the cycle never self-primes before the watchdog rotates.

## Secondary Contributing Factor

Jumpers never receive jump info (csj.rs:194-197 — newly elected dynamo has `jump_info = None`). `on_roll_forward` only broadcasts when `slot > last_jump_slot + jump_size = snapshot_tip + 4320`. With 30s rotation window, jump never fires. Jumper fragments stay empty → LoE pinned at snapshot_tip → chain selection cap frozen. This is secondary — fixing the watchdog (primary) is sufficient.

## Fix (IMPLEMENTED — commit 6212c2895b)

The blunt `!is_genesis_bulk_sync` flip (originally proposed) is WRONG: CSJ is
disabled in praos, so flipping makes the 2464 watchdog a no-op everywhere,
RE-OPENING #742 (a genuinely-silent dynamo in genesis would never be rotated —
the 2661 ChainSel-starvation path can't catch it because it only fires once a
range has been DISPATCHED). The watchdog must DISTINGUISH a silent dynamo from
a parked-with-headers one.

Implemented (`connection_lifecycle.rs`): gate the 2464 rotation on
`should_rotate_unproductive_dynamo(csj.fragment_head_slot(&addr), chain_tip)`.
A dynamo whose CSJ fragment leads the selected chain by more than
`GENESIS_PARKED_DYNAMO_MARGIN_SLOTS = 2000` (well below the smallest network
forecast window, preview `3k/f = 25 920`) has fed headers and is legitimately
PARKED → KEEP it. One at/near our tip, or with no fragment, is genuinely silent
(the #742 target) → rotate. New `CsjRegistry::fragment_head_slot()` accessor.
Mirrors Haskell, where a peer blocked at the forecast horizon is not starving us.

**Praos safety:** the new gate sits inside the existing `is_genesis_bulk_sync`
branch, so praos is byte-identical. 1102/1102 dugite-node tests pass; 3 new
unit tests (csj accessor + watchdog discriminator).

**NOTE on reproduction:** the wedge is TIMING-dependent — a fresh epoch-500
genesis cold-restart on the unfixed binary did NOT reproduce it (synced
100k+ slot/min, LoE advancing). It only manifests when BlockFetch falls behind
enough that the dynamo parks AND ChainSel starves 30s. The fix removes the
false-positive rotation regardless.

## Disambiguation diagnostics

Watch for `"BlockFetch: dynamo unproductive past watchdog"` or `"CSJ: promoted peer re-intersected at frontier"` at ~30s intervals. If present → primary root cause confirmed.

**Why:** **#** The condition `is_genesis_bulk_sync` was INTENDED to scope the watchdog to genesis only; it inadvertently fires on legitimate forecast parks.
