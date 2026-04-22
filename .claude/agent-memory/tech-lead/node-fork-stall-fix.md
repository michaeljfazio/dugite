---
name: Fork stall fix (TriggeredFork + MsgRollBackward + Mithril)
description: Three-part fix for live-tip sync stall after a fork; commits 85f1d53a8, 040cb132c, c364c59c2
type: project
---

## Bug: Node stalls permanently after any live-tip fork

### Symptom
After a 1-block fork, BlockFetch cycles through all peers every 0.5s for the same slot but "Chain extended" never fires. `tip_age_seconds` climbs unbounded.

### Root Cause 1: TriggeredFork arm doesn't apply fork blocks (85f1d53a8)

`apply_fetched_block` in `node/mod.rs` handles `AddBlockResult::TriggeredFork { rollback, apply }` by calling `handle_rollback` then returning `storage_succeeded = true`. The comment claimed `process_forward_blocks` would replay the `apply` list, but that function is dead_code and never called from the live run loop. Fix: replay the `apply` list inline in the TriggeredFork arm, set `fork_replayed = true`, and guard against re-applying the incoming block.

### Root Cause 2: MsgRollBackward not propagated to run loop (040cb132c)

`chainsync_client_task` in `sync.rs` handles `MsgRollBackward` by only trimming `candidate_chains`. The run loop never calls `handle_rollback`. After ledger replay completes at slot X, peers send rollback to Y < X. The ledger stays at X. All new blocks have block_no ≤ tip_block → silently dropped by the `block_no <= tip_block` guard at line ~2575.

Fix: add `rollback_event_tx: mpsc::Sender<Point>` channel. `chainsync_client_task` sends rollback points; the run loop's `select!` processes them via `handle_rollback`.

### Root Cause 3: LSM lock conflict in handle_rollback (040cb132c)

`handle_rollback` tries `open_from_snapshot` on the live LSM store path. The live node already holds the lock → `EAGAIN (os error 35)`. The old fallback did a full genesis reset. Fix: on `open_from_snapshot` failure, fall back to in-memory detach + bincode snapshot load (not genesis reset).

### Mithril Re-import Issues (c364c59c2)

During debugging, two additional bugs were found in the Mithril import path:

**Bug 4: Stale chunk contamination** — `import_snapshot` in `mithril.rs` does `fs::rename(imm, dest_dir)`. On macOS (cross-filesystem), rename fails → `copy_dir_recursive` copies new files but leaves old chunks (e.g., 25446, 25447 from prior node runs) in `dest_dir`. Replay then fails with "Block does not connect to tip" because stale chunks don't chain to the new snapshot tip. Fix: `remove_dir_all(dest_dir)` before rename.

**Bug 5: Snapshot-ahead-of-ChainDB discarded** — `node/mod.rs` line 615 discards the ledger snapshot when `snapshot_slot > db_tip_slot`, logging "crash before ChainDB persist". But the Mithril ancillary Haskell snapshot is legitimately a few hundred blocks ahead of the last complete immutable chunk (the partial tip chunk may not be included). The ChainDB-before-ledger invariant means crashes cannot produce snapshot > ChainDB. Fix: accept the snapshot and let peers fill the gap.

**Bug 6: UTxO store reset cascade** — After fixing Bug 5, a stale `ledger-snapshot.bin` from a prior genesis-replay run was loaded instead of the Mithril snapshot (overwritten at same path). With an empty UTxO store, the `utxo_count < min_expected (100K)` check at node/mod.rs line 895 resets the ledger to genesis and triggers a full replay. Not a code bug — operational issue requiring `rm -rf db-preview` before re-import.

### Key Invariants Confirmed

- `ChainDB write → ledger apply → snapshot save` order means `snapshot_slot > ChainDB_tip` can ONLY happen after Mithril import (never after crash)
- Mithril epoch 1272 snapshot: UTxOs stored inline in 634.5MB bincode; migrated to LSM on first node startup. Named snapshot saved AFTER migration is only 35MB (no inline UTxOs) — requires the LSM store to be intact
- `hash_index.dat` is rebuilt automatically; safe to delete
- `ledger-snapshot.bin` is overwritten on every startup if ledger progresses → always do `rm -rf db-preview` before fresh import

**Why:** Discovered during soak testing on preview testnet after a natural 1-block fork at slot 109938519.
**How to apply:** If node stalls at live tip after fork (BlockFetch cycling same slot, tip_age climbing), check for "TriggeredFork" in logs. If MsgRollBackward log appears without subsequent "Rollback complete", the run loop isn't processing it.
