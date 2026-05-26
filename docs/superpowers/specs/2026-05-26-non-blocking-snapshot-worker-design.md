# Non-Blocking Ledger Snapshot Worker (issue #695)

## Problem

`save_ledger_snapshot` blocks the apply loop for multi-second
(preview at-tip) to ~2-minute (bulk replay) windows. Symptoms:

- **Bulk chunk replay** stalls every 50K blocks for ~2 min while
  the bincode write runs. Apply progress is invisible during the
  stall (the issue ships with a log timeline showing the silence).
- **At-tip** apply latency P99 spikes whenever the `~72 min`
  normal-cadence snapshot fires.
- Forging can be delayed mid-save — exactly what we want to avoid.

Root cause: the apply paths call into a save that ultimately drives
the CBOR/bincode walk synchronously from the apply task. Even the
existing `Node::save_ledger_snapshot` (issue #649) — which already
factors a Phase-A view-build + Phase-B `spawn_blocking` write — is
awaited at every call site, so the apply task still pauses for the
duration of the disk write.

## Goal

Match cardano-node's snapshot timing/blocking characteristics:

1. Apply never waits for snapshot serialization or disk I/O.
2. No snapshots are written during ImmutableDB chunk replay (Haskell
   does not start background tasks until replay completes).
3. Snapshot bytes remain semantically identical (the `verify-ledger-snapshot`
   gate from #670 continues to pass byte-for-byte).
4. Crash-recovery posture unchanged at-tip: latest on-disk snapshot
   is still within `k*2` slots of tip.

## Haskell reference (ouroboros-consensus)

From the oracle research summarised in the conversation:

- `Ouroboros.Consensus.Storage.LedgerDB.Snapshots` — `SnapshotPolicy`
  (cadence: `k*2` slots, 5–10 min random delay gate, 2 retained).
- `Ouroboros.Consensus.Storage.ChainDB.Impl.Background` —
  `ledgerDbTaskWatcher` is a dedicated background thread spawned by
  `launchBgTasks`. The apply thread (`addBlockRunner`) fires a single
  `writeTVar` (O(1) STM) to trigger; the watcher picks it up and
  serializes lock-free.
- `Ouroboros.Consensus.Storage.LedgerDB.V2.implTryTakeSnapshot`
  holds the RAW read-lock only long enough to `duplicateStateRef`
  (O(1) — Haskell persistent-map reference copy). The lock is
  released before CBOR serialization runs.
- `Ouroboros.Consensus.Storage.LedgerDB.API` — `replayStartingWith`
  uses `reapplyThenPush`, which does **not** call `tryTakeSnapshot`.
  Background tasks are only launched after `replayStartingWith` returns,
  so no snapshots are written during ImmutableDB chunk replay.

## Design

### Components

1. **`SnapshotWorker`** (new module
   `crates/dugite-node/src/node/snapshot_worker.rs`).
   A long-lived task spawned at node startup. Owns:
   - `database_path: PathBuf`
   - `max_snapshots: usize`
   - `mpsc::Receiver<SnapshotRequest>`
   - `metrics: Arc<NodeMetrics>` (for skip/success counters)

   `SnapshotRequest` holds:
   ```
   struct SnapshotRequest {
       view: LedgerStateSnapshot,  // Send + 'static
       epoch: u64,
       slot: u64,
       utxo_count: usize,
   }
   ```

   The worker loops on `recv().await`, and for each request runs the
   existing `tokio::task::spawn_blocking` body from
   `Node::save_ledger_snapshot` (Phase B):

   - `LedgerState::write_snapshot_view_to_path(view, &epoch_path)`
   - `link_latest_snapshot(&epoch_path, &latest_path)` (non-fatal on
     failure — `latest.bin` is regenerable from the enumerator)
   - `prune_old_snapshots_in_dir(&database_path, max_snapshots+1)`
   - log `Snapshot saved ...` and bump metrics

2. **`SnapshotTrigger`** (Sender handle on `Node`).
   Holds `mpsc::Sender<SnapshotRequest>` (bounded `channel(1)`).
   Replaces every at-tip `self.save_ledger_snapshot().await` call.

   Flow inside `Node::try_snapshot_async`:
   ```
   // Phase A — under ledger write lock (already short)
   let req = {
       let mut ls = self.ledger_state.write().await;
       // existing prep: opcert copy, diff_seq clear,
       // LSM flush (still under lock — `&mut LsmTree`)
       ls.consensus.opcert_counters = self.consensus.opcert_counters().clone();
       ls.utxo.diff_seq.clear();
       if let Err(e) = ls.save_utxo_snapshot() {
           error!(... );
       }
       SnapshotRequest {
           view: LedgerStateSnapshot::from(&*ls),  // Arc::clone for big maps
           epoch: ls.epoch.0,
           slot: ls.tip.point.slot().map(|s| s.0).unwrap_or(0),
           utxo_count: ls.utxo.utxo_set.len(),
       }
       // ── LOCK RELEASED HERE ──
   };
   // Phase B — fire-and-forget. Apply continues.
   match self.snapshot_tx.try_send(req) {
       Ok(()) => debug!("snapshot enqueued (epoch={epoch})"),
       Err(TrySendError::Full(_)) => {
           debug!(epoch, "snapshot worker busy — skipping (Haskell-aligned delay gate)");
           metrics.inc_snapshot_skipped();
       }
       Err(TrySendError::Closed(_)) => {
           error!("snapshot worker channel closed — node shutting down");
       }
   }
   ```

   `try_snapshot_async` is `async fn` because it awaits the ledger
   `RwLock`, but it does **no I/O** of its own — once the view is
   built and `try_send` returns, the function completes immediately.
   The only pause point is the existing Phase-A lock acquisition,
   whose duration is unchanged (LSM flush + Arc::clone view build,
   already short).

3. **Bulk replay** (strict Haskell match).
   - Delete the `should_snapshot_bulk` triggers in `sync.rs:2185` and
     `sync.rs:2611`. No snapshot is taken during chunk-file replay.
   - At the *end* of replay (after the `Replay complete` log line in
     both code paths), perform a single blocking save by calling the
     existing `Node::save_ledger_snapshot` (the awaited Phase-A/B
     path). Blocking is fine here — replay is finished, no apply is
     pending, and we want the snapshot durable before forward sync
     begins.

4. **Shutdown.**
   - `Node::shutdown` drops the `snapshot_tx` Sender. The worker's
     `recv()` returns `None`, it exits cleanly.
   - The existing `self.save_ledger_snapshot().await` in `mod.rs:4031`
     stays — synchronous final save during shutdown, which is the
     desired durability boundary.

### What changes / what does not

| Concern | Before | After |
|---|---|---|
| At-tip apply pause during save | seconds | none (lock held only for Phase A) |
| Bulk replay 2-min stall | every 50K blocks | none |
| Snapshot bytes on disk | bincode + DUGT header | unchanged |
| `verify-ledger-snapshot` (#670) | passing | passing (no semantic change) |
| Snapshot policy / cadence (`SnapshotPolicy`) | unchanged | unchanged |
| `Node::save_ledger_snapshot` (issue #649 path) | called via `.await` | replaced by `try_snapshot_async` at apply sites; retained for shutdown + end-of-replay |
| Concurrent snapshots possible | no (single await) | no (channel cap=1, worker is single-task) |
| Memory peak during save | view + serialized bytes | unchanged (view held by worker, serialization streams) |
| Crash recovery at-tip | re-apply ≤ `k*2` slots | identical |
| Crash recovery during replay | re-apply ≤ `bulk_min_blocks` | re-apply entire current replay (Haskell-equivalent) |

### Backpressure (resolved)

Channel capacity = 1. If the worker is still writing the previous
snapshot when a new request arrives, `try_send` returns `Full` and the
trigger logs at debug + bumps a skip counter. This mirrors
cardano-node's `SnapshotDelayRange` gate, which skips taking a snapshot
when the previous one is too recent.

This is safe because `SnapshotPolicy` already throttles requests:

- at-tip: minimum `k*2` seconds (~72 min mainnet) between requests, so
  a `Full` channel implies the disk is two orders of magnitude
  slower than expected — operator-visible via metric, not a routine
  occurrence.
- end-of-replay: a single request at shutdown of the replay loop, no
  contention possible.

### Send-bound check

`LedgerStateSnapshot` is `Send + 'static`:

- All fields are `HashMap`/`BTreeMap`/`Arc<HashMap>` of owned types.
- `Arc<HashMap<...>>` is `Send` if the inner is `Send`. All
  contained types (`Hash28`, `Hash32`, `Lovelace`, `PoolRegistration`,
  `GovernanceState`, ...) are `Send`.
- No raw pointers, no `Rc`, no thread-local refs.

The existing #649 `spawn_blocking` already moves the same view across
threads, so this is established.

### Files touched

- `crates/dugite-node/src/node/snapshot_worker.rs` *(new)*
- `crates/dugite-node/src/node/mod.rs` — module decl, Node holds
  `Option<mpsc::Sender<SnapshotRequest>>`, worker spawn in
  `Node::start_runtime` (or wherever background tasks are launched),
  shutdown drops the sender.
- `crates/dugite-node/src/node/epoch.rs` — add
  `Node::try_snapshot_async` (non-async, non-blocking — Phase A +
  try_send). Keep `Node::save_ledger_snapshot` for shutdown +
  end-of-replay (now unused by hot apply paths).
- `crates/dugite-node/src/node/sync.rs` — three replacements:
  - `:1499` `self.save_ledger_snapshot().await` → `self.try_snapshot_async().await`
    (Phase A still awaits the ledger lock; that's all)
  - `:1746` ditto
  - `:2185` and `:2611` (bulk-replay) drop the
    `should_snapshot_bulk` save; add a single
    `self.save_ledger_snapshot().await` after the replay loop exits.
- `crates/dugite-node/src/node/mod.rs:4814` (post-block scheduler) →
  `self.try_snapshot_async().await`.
- `crates/dugite-node/src/metrics.rs` — add
  `snapshot_enqueued_total`, `snapshot_skipped_busy_total`,
  `snapshot_failed_total` counters.

### Out of scope

- Snapshot format changes (no WAL, no incremental, no mmap — keeping
  semantic byte-equivalence so #670 verification gate is untouched).
- Changes to `SnapshotPolicy` cadence or thresholds.
- LSM `save_snapshot` parallelisation. The LSM flush stays under the
  ledger write lock — it requires `&mut LsmTree` and is short
  (~100ms on preview) compared to the bincode walk (multiple
  seconds).
- Changing the snapshot file naming or on-disk layout.

## Testing

- **`crates/dugite-node/src/node/snapshot_worker.rs` unit tests**
  - Worker writes an enqueued request to disk and emits the success log.
  - `try_send` on a full channel returns `Full` and the worker is still alive.
  - Dropping the sender causes the worker future to resolve to `()`.
  - Pruning runs after each successful write.

- **Replay regression**
  - `chunk_replay_emits_zero_intermediate_snapshots` — drive the
    replay loop with a fake `ChainDB`, assert that
    `ls.save_snapshot` is never called inside the loop, and that
    exactly one `Node::save_ledger_snapshot` fires after the loop.

- **Existing gates**
  - `verify-ledger-snapshot` CI step continues to pass (no
    `LedgerStateSnapshot` field change).
  - `cargo test --doc`, `cargo nextest run --workspace`, clippy.

## Risks

- **Worker death silently disables snapshots.** Mitigation: if the
  worker task panics, the channel is closed; the next `try_send`
  surfaces `Closed` and we error-log on every subsequent attempt.
  Add a `worker_alive` health metric so the TUI/monitor can surface
  this.
- **Apply outruns the worker.** Channel cap=1 with `Full` => skip
  prevents queue growth. Skip counter is metricised so operators see
  if disks can't keep up.
- **Shutdown loses an in-flight snapshot.** Mitigation: the
  shutdown path calls `Node::save_ledger_snapshot().await`
  synchronously *after* dropping the worker sender, so we always
  have a fresh final snapshot independent of the worker state.
