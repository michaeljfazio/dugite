---
name: node-live-apply-no-ledgerseq-delta
description: FIXED (commit 59a5fc64d). apply_fetched_block uses apply_block (no delta push), leaving LedgerSeq empty; fork rollback fails → StoreButDontChange cascade
metadata:
  type: project
---

## Status: FIXED in commit 59a5fc64d (2026-05-16)

## Bug B: Live-apply path skips LedgerSeq delta push → fork-switch stall

`apply_fetched_block` in `crates/dugite-node/src/node/mod.rs:3621` calls `ls.apply_block()`,
which does NOT call `apply_block_with_delta` and does NOT push a `LedgerDelta` to
`self.ledger_seq`. Only `process_blocks_bulk` (sync.rs:1146) pushes deltas.

### Cascade when fork fires before any snapshot exists

1. Live relay blocks (and self-forged blocks) → apply_fetched_block → no delta pushed
2. LedgerSeq has 0 entries even after 11+ applied blocks
3. TriggeredFork fires: `handle_ledger_rollback(intersection_slot)` is called
4. Fast path: `rollback_via_seq` returns None (empty seq)
5. Slow path: `find_best_snapshot_for_rollback` returns None (no snapshots written yet)
6. ERROR "Rollback target outside LedgerSeq volatile window AND no canonical snapshot"
7. Ledger stays at pre-fork tip (slot X); fork replay tries first fork block; prev_hash mismatch
8. `clear_volatile()` destroys all VolatileDB state
9. Every subsequent relay block → `StoreButDontChange` (ancestry chain cleared) → permanent stall

### Fix

**Fix A** (primary): In `apply_fetched_block` (~line 3619), replace `ls.apply_block()` with
`ls.apply_block_with_delta()`, then push delta to `self.ledger_seq`. Same lock-order pattern
as `process_blocks_bulk` (release ledger_state before acquiring ledger_seq).

**Fix B**: Same change in the TriggeredFork replay loop inside `apply_fetched_block` (~line 3448).
Without this, LedgerSeq tip diverges from ledger tip after every fork switch.

**Fix C**: Change `handle_ledger_rollback` return type to `bool`. In call sites, skip fork replay
if rollback failed (avoid triggering clear_volatile → StoreButDontChange cascade).

### Design doc
`docs/superpowers/specs/2026-05-16-bug-b-fork-switch-stall-fix.md`

**Why:** Discovered during local-devnet testing (2026-05-16). Bug is deterministic on first fork
after a fresh start with no ledger snapshots. Preview testnet hides it because Mithril-import
snapshot is taken at ImmutableDB tip = LedgerSeq anchor, so deltas accumulate from that anchor.

**How to apply:** Any time node stalls permanently with repeated `StoreButDontChange` after a fork,
check whether the ERROR "Rollback target outside LedgerSeq volatile window" appeared just before.
That ERROR is the definitive indicator of this bug.
