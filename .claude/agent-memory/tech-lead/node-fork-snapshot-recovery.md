---
name: Fork snapshot recovery at startup
description: BP forges un-adopted block → snapshot on fork → stuck ledger on restart; two-layer fix
type: project
---

## The bug (detected 2026-04-22, preview BP at block 4211059)

When the BP forges a block that the network does NOT adopt:
1. The forged block enters VolatileDB and the ledger advances to it.
2. A snapshot fires at the fork tip (e.g. `ledger-snapshot-epoch1275-slot110176139.bin`).
3. On restart, VolatileDB WAL is empty (forged block was never finalized).
4. The snapshot slot is in the "volatile region" (above ImmutableDB tip) so the startup
   canonicality check provisionally accepts it — correct behavior, can't verify volatile range.
5. `replay_from_lsm` calls `get_next_block_after_slot(fork_slot)` → gets first canonical block
   from VolatileDB WAL. Its `prev_hash` != fork block hash → `apply_block` fails with
   "Block does not connect to tip: expected <fork_hash>, got <canonical_hash>". Repeats forever.

**Why:** The startup snapshot check only handles ImmutableDB-range forks. For volatile-range
forks (forged block above immutable tip), the check can't detect the fork at load time.

## Root cause of earlier bug in mod.rs (commit 059c13137)

The snapshot canonicality check in `mod.rs` had a separate bug for ImmutableDB-range forks:
- `get_block_at_or_after_slot` returns block at a DIFFERENT slot than snapshot_slot
- (canonical chain has empty slot where snapshot has a block)
- Old code: `_ => false` (accept) — WRONG
- Fixed: delegated to `epoch::is_snapshot_canonical` which correctly returns `false` for this case

This fix (commit 059c13137) is also needed, but doesn't fix the volatile-range case.

## The actual fix (commit ff8f43e44)

In `replay_from_lsm` (crates/dugite-node/src/node/sync.rs), BEFORE the replay loop:

1. Fetch next canonical block after ledger tip slot from ChainDB.
2. Decode its prev_hash.
3. If prev_hash != ledger tip hash → ledger is on a dead fork.
4. Call `find_best_snapshot_for_rollback(ledger_tip_slot - 1, Some(&db))`.
   - Subtracting 1 excludes the fork snapshot (its slot == ledger_tip_slot).
   - Finds earlier canonical snapshot in ImmutableDB range (definitely canonical).
5. Restore UTxO store in-place via `restore_from_snapshot("ledger")`.
6. Replace ledger state with recovered snapshot.
7. Continue into replay loop — now correctly replays from the earlier snapshot forward.

**Haskell reference:** `LedgerDB.Init.initLedgerDB` rolls back to the youngest snapshot
on the current chain fragment. Same pattern.

## Ancillary fix (commit 059c13137)

Made `epoch::is_snapshot_canonical` `pub(crate)` and `startup::enumerate_snapshots`/
`SnapshotCandidate` `pub(crate)` for reuse. The mod.rs snapshot validation now uses
`is_snapshot_canonical` directly instead of a buggy custom re-implementation.

## Third layer fix — volatile-range blind spot (commit abb370fe3, 2026-04-22)

The startup `is_snapshot_canonical` in epoch.rs had its OWN volatile-range blind spot:

```rust
// OLD (bug):
if snap_slot > imm_tip_slot { return true; }   // accepts ALL volatile-range slots

// NEW (fix):
let db_tip_slot = db.get_tip().point.slot()...;
if snap_slot > db_tip_slot { return true; }    // only accept when genuinely ahead of ChainDB
```

The forged block at slot 110176139 was:
- above ImmutableDB tip (110164590) → old code returned true immediately
- below VolatileDB canonical chain tip (110182746) → new code proceeds to check VolatileDB

`get_block_at_or_after_slot` already checks VolatileDB selected chain only (not orphaned forks).
So the existing hash-comparison logic correctly detects the mismatch.

Added regression test: `test_is_snapshot_canonical_volatile_fork_detected` (VolatileDB has
canonical chain at slots 100/200, snapshot claims different hash at slot 100 with empty ImmutableDB).

## Recovery observed after all three layers

On restart after abb370fe3:
1. Startup detects fork snapshot `a5ac5afc` at slot 110176139 (via VolatileDB selected chain check)
2. Rolls back to epoch 1274 snapshot at slot 110157845 (canonical)
3. Chunk replay to ImmutableDB tip 110164590 (`ee749a9c`)
4. LSM replay detects secondary fork (missing canonical block `8ba27efa` between ImmutableDB and VolatileDB)
5. Rolls back to slot 110094773 (oldest epoch snapshot), replays 2623 blocks
6. Node at live tip block 4211513 within 5 minutes — no operator intervention

## Follow-up needed

The ROOT CAUSE is that the BP saves a snapshot when its forged block is in VolatileDB but
not yet confirmed by the network. Fix: only snapshot at immutable-confirmed anchor points
(matching Haskell's behavior). Track as separate issue.

**Why:** Haskell only snapshots the ImmutableDB-anchored ledger state, so fork snapshots
can never occur there. Dugite snapshots the volatile ledger tip, which can be on a fork.
