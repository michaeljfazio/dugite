---
name: issue-767-live-apply-deadlock
description: Root-cause analysis of #767 hard wedge in live apply path — LENS A static cycle analysis COMPLETED; no true AB-BA mutex or channel cycle found; best candidate is synchronous LSM stall inside try_snapshot_async holding ledger_state.write()
type: project
---

## Symptom
After many hours / millions of blocks at live tip, all ~13 tokio worker threads park permanently. CPU → ~0.7%. SIGTERM + restart cleanly resumes past the wedge point. Appeared after v2.0.8 (#762 maintenance-every-256-blocks patch). Occurs ~1/13k maintenance cycles.

## LENS A Static Analysis (2026-06-16)

### Tasks and their lock ownership (exhaustive enumeration)

| Task | Locks (in acquisition order) |
|------|-------------------------------|
| Apply task (single-threaded `Node::run` select loop) | chain_db.write (temporary, lines 6046/6100/6207) → submit_block_with_header oneshot AWAIT (no lock held) → ledger_state.write (line 6408) → era_history.read (line 6416, nested inside ledger_state.write) → era_history.write (line 6516, still inside ledger_state.write, era transition only) → ledger_seq.write (line 6568, after ledger_state released) → chain_fragment.write (line 6574) → ledger_state.read (line 6823 in post_block_apply_updates) → era_history.read (line 6849) → query_handler.write (query.rs:1087, after all others released) |
| ChainSel runner (independent tokio::spawn) | chain_db.write (run_selection_pass line 398; process_add_block line 663) → chain_db.read (line 594, 701) |
| BlockFetch worker (per-peer tokio::spawn) | candidate_chains.write (line 3165) BEFORE channel send; no chain_db or ledger_state locks held during fetched_blocks_tx.send().await (line 3182) |
| Snapshot worker (run_snapshot_worker) | none — operates on moved LedgerStateSnapshot (Clone-free Arc fields) via spawn_blocking |
| N2C LSQ task (lsq_task) | query_handler.blocking_read() inside block_in_place (lines 7214-7238), per-query, dropped before return |

### Key lock sequences in try_snapshot_async (epoch.rs:466-541)

1. `chain_db.read().await` (line 488-494) — get immutable_tip_slot — RELEASED
2. `ledger_state.write().await` (line 501) — held WHILE `save_utxo_snapshot()` runs synchronously (no await, no block_in_place; LSM flush is synchronous I/O on the worker thread)
3. `try_send(req)` (line 523) — non-blocking, does NOT hold ledger_state.write() (released at line 521)

### AB-BA Analysis

No AB-BA found:
- Apply task never holds chain_db AND ledger_state simultaneously. All chain_db writes in apply_fetched_block (lines 6046, 6100, 6207) are released BEFORE submit_block_with_header is called (line 6115). After the oneshot await completes, no chain_db lock is held when ledger_state.write() is acquired (line 6408).
- ChainSel runner only holds chain_db — never ledger_state.
- BlockFetch workers only hold candidate_chains.write() (line 3165), which is entirely disjoint from all apply-path locks.
- era_history is exclusively written by the apply task (single-threaded); no AB-BA possible.
- try_snapshot_async acquires chain_db.read() BEFORE ledger_state.write() (per the #762 comment at epoch.rs:482-494). Lock order enforced.
- run_background_maintenance: chain_db.write() loop — each iteration is `{ acquire; flush; release }` THEN `yield_now()`. No lock held across yield.

### Channel deadlock analysis

- ChainSel mpsc: capacity 512. Apply task sends ONE message per block and awaits oneshot before sending next. Channel never fills. Not a cycle.
- fetched_blocks_rx: capacity 1024. Apply task drains it one block at a time. BlockFetch workers can fill it during run_background_maintenance (where apply task is not draining). But maintenance is bounded: after it returns, the select! loop drains the channel again. Self-clearing.
- Snapshot worker channel: capacity 1. `try_send` is non-blocking. Never blocks the apply task.

### Prior candidate refuted: block_in_place worker starvation

Tokio's `block_in_place` spawns an ADDITIONAL worker from its thread pool before blocking the current thread. This is a documented mechanism to prevent task starvation. The "all 13 workers in block_in_place" scenario is self-limiting because tokio expands the worker count. This is LATENCY only, not a permanent cycle. Refuted as the root cause.

## Best candidate: synchronous LSM stall inside try_snapshot_async

The strongest remaining candidate for a PERMANENT wedge is:

**Mechanism:** `save_utxo_snapshot()` (epoch.rs:504) runs synchronously inside `ledger_state.write()` (line 501). If the LSM library (cardano-lsm) internally deadlocks or stalls indefinitely (e.g., waiting for memory from the OS mmap allocator under memory pressure, or hitting an internal mutex while the blocking thread pool is also saturated), the apply task's worker thread is permanently blocked inside the lock.

**Effect:** `ledger_state.write()` is held forever. Any task needing `ledger_state` (even read) is permanently blocked. No task that needs `ledger_state.read()` can proceed. The fetched_blocks_tx.send().await in BlockFetch workers fills `fetched_blocks_rx` to 1024, then all BlockFetch workers block on send. Eventually all 13 workers are either in block_in_place or blocked on the channel. CPU → 0.7%.

**Why restart recovers:** SIGTERM triggers the post-loop drain path which skips `try_snapshot_async` (the snapshot_tx is dropped, `try_snapshot_async` returns `Closed`). The volatile blocks are applied and the final synchronous `save_ledger_snapshot` runs in a clean single-threaded context.

**Why ~1/13k:** The LSM stall requires a specific combination: snapshot triggered (scheduler fires every ~2000 blocks OR epoch boundary), concurrent memory pressure from large blocks or ValidateAll Plutus evaluation GC, and the LSM hitting its specific internal stall condition.

**is_permanent_cycle:** TRUE if the LSM library has an internal permanent hang; FALSE (self-clearing latency) if it's just a slow LSM flush (eventually completes). Evidence from the symptom ("permanent until SIGTERM") favors TRUE.

## Practical fix

Wrap `save_utxo_snapshot()` call inside `try_snapshot_async` in a timeout (e.g., 10s). On timeout, log an ERROR, release the lock, and continue. This converts a permanent hang into a recoverable error.

Alternative: move the LSM flush into the snapshot worker's `spawn_blocking` call entirely, making Phase A (under the ledger write lock) only do the Arc clones for the view, not the LSM flush.

The `ls.utxo.diff_seq.clear()` at epoch.rs:503 must still happen under the lock. But `save_utxo_snapshot()` at line 504 does NOT need to be under the ledger write lock — it could be deferred to the spawn_blocking call in the worker. The diff_seq is cleared before the view snapshot is built (line 513), so LSM state is consistent for the view.

Surgical fix (minimal change): 
```rust
// Phase A: under the ledger write lock
let req = {
    let mut ls = self.ledger_state.write().await;
    ls.consensus.opcert_counters = self.consensus.opcert_counters().clone();
    ls.utxo.diff_seq.clear();
    // NOTE: save_utxo_snapshot() moved OUT of the lock into the worker
    let req = SnapshotRequest {
        view: dugite_ledger::LedgerStateSnapshot::from(&*ls),
        ...
    };
    req
    // lock released here — no LSM flush under the lock
};
// Phase B: in the snapshot worker's spawn_blocking, call save_utxo_snapshot()
// on the snapshot data (which carries the utxo_set Arc for the LSM handle)
```
