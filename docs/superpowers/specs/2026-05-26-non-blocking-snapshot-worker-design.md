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

## Goal & honest acceptance criteria

Match cardano-node's snapshot-trigger model: apply fires a single
non-blocking signal to a dedicated background task. Apply itself
never waits for serialization or disk I/O.

### Achievable in this PR

- No multi-second / multi-minute apply gap during save. Expected
  at-tip apply pause drops from **5–15 s today** to **0.5–2 s**
  (the residual is the LSM memtable flush + deep HashMap clones
  needed to capture a consistent point-in-time view; see "Honest
  Performance Floor" below). Bulk replay no longer pauses at all.
- No snapshots written during ImmutableDB chunk replay (Haskell
  does not start background tasks until replay completes).
- Snapshot bytes byte-identical to today (`verify-ledger-snapshot`
  gate from #670 untouched).
- Crash-recovery posture unchanged at-tip: latest on-disk snapshot
  still within `bg_snapshot_scheduler` interval (2000 blocks
  default) of tip.

### Not achieved by this PR — filed as follow-up

The original issue acceptance sketch targets **P99 < 10 ms apply
latency during snapshot**. Hitting that requires reducing Phase-A
to a couple of Arc ref-bumps, which means converting the deep-cloned
HashMap fields (`stake_distribution`, `EpochSnapshots`, `pointer_map`,
`script_stake_credentials`, `genesis_delegates`, `pending_*`,
`ProtocolParameters`) to `Arc<…>` with `Arc::make_mut` semantics, or
introducing an immutable anchor snapshot in the LedgerSeq itself
(closer to Haskell `LedgerDB.V2.LedgerSeq`). That is a broader
refactor across `dugite-ledger` substates and is out of scope here.

### Out of scope

- Snapshot format changes (no WAL, no incremental, no mmap — keeps
  byte-equivalence with the #670 verification gate).
- Changes to `SnapshotScheduler` (`crates/dugite-storage/src/background.rs`)
  cadence/thresholds.
- LSM `save_snapshot` parallelisation. The LSM flush stays under
  the ledger write lock — `LsmTree::save_snapshot` takes `&mut self`
  and is short (~100 ms preview) compared to the bincode walk.
- Snapshot file naming or on-disk layout.

## Haskell reference (ouroboros-consensus)

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
  Background tasks are only launched after `replayStartingWith`
  returns, so no snapshots are written during ImmutableDB chunk
  replay.

## Honest Performance Floor — Phase-A cost analysis

Phase A holds the ledger write lock while it:

1. Copies opcert counters from consensus into ledger state
   (sub-ms).
2. Clears `diff_seq` (a `Vec`, sub-ms).
3. Calls `ls.save_utxo_snapshot()` — LSM memtable flush. Sub-second
   on preview; ~1–3 s on mainnet under churn. **Required** under
   the lock because `LsmTree::save_snapshot` takes `&mut self`.
4. Builds `LedgerStateSnapshot::from(&*ls)`. Per
   `crates/dugite-ledger/src/state/snapshot_format.rs:183`, this is:
   - **Cheap (Arc::clone, O(1)):** `delegations`, `pool_params`,
     `reward_accounts`, `governance`, `epoch_blocks_by_pool`.
   - **Expensive (deep `.clone()`):** `stake_distribution`,
     `snapshots: EpochSnapshots` (mark/set/go: 3× ~1M entries),
     `pointer_map`, `script_stake_credentials`, `genesis_delegates`,
     `pending_pp_updates`, `future_pp_updates`, `pending_retirements`,
     `future_pool_params`, `protocol_params`, `prev_protocol_params`,
     and the various `pending_mir_*` maps. Most are small; the
     biggest is `EpochSnapshots` (hundreds of MB on mainnet).
   - **In-memory-backend only foot-gun:** `utxo_set: s.utxo.utxo_set.clone()`
     is a multi-GB deep clone if the LSM store is not attached.
     Production always attaches LSM, so this is empty in practice.
     A `debug_assert!(s.utxo.utxo_set.has_store(), …)` guards this
     in the view builder.

The deep-cloned HashMaps are also the **memory peak** during the
write window: the worker holds the cloned maps until the bincode
walk completes (5–15 s on mainnet at the disk-bound write rate).
This is **identical to today's #649 `spawn_blocking` path** — not
a regression — but worth quantifying. Expected peak: 200–500 MB
extra residency for the worker's view on mainnet.

`Arc::clone` view IS a point-in-time snapshot because
`crates/dugite-ledger/src/ledger_seq.rs` uses `Arc::make_mut` for
every Arc'd-field mutation. The first post-snapshot mutation on
each Arc'd map pays a `.clone()` cost (refcount > 1 → COW). That
cost falls on the apply thread *after* the lock release — also
unchanged from today.

## Design

### Components

#### 1. `SnapshotWorker`

New module `crates/dugite-node/src/node/snapshot_worker.rs`. A
long-lived task spawned at node startup. Owns:

- `database_path: PathBuf`
- `max_snapshots: usize`
- `mpsc::Receiver<SnapshotRequest>`
- `metrics: Arc<NodeMetrics>` (success / skip / fail / alive)

`SnapshotRequest`:

```rust
pub struct SnapshotRequest {
    pub view: LedgerStateSnapshot,  // Send + 'static
    pub epoch: u64,
    pub slot: u64,
    pub utxo_count: usize,
}
```

Worker loop (one save at a time, panic-isolated):

```rust
pub async fn run_snapshot_worker(
    database_path: PathBuf,
    max_snapshots: usize,
    metrics: Arc<NodeMetrics>,
    mut rx: mpsc::Receiver<SnapshotRequest>,
) {
    metrics.set_snapshot_worker_alive(true);
    while let Some(req) = rx.recv().await {
        let db = database_path.clone();
        let m = metrics.clone();
        // spawn_blocking owns the bincode walk + atomic rename + prune
        let join = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle_snapshot_request(&req, &db, max_snapshots, &m)
            }))
        }).await;
        match join {
            Ok(Ok(Ok(()))) => {}                          // success
            Ok(Ok(Err(e))) => { metrics.inc_snapshot_failed(); error!(...); }
            Ok(Err(panic)) => { metrics.inc_snapshot_failed(); error!(...); }
            Err(join)      => { metrics.inc_snapshot_failed(); error!(...); }
        }
    }
    metrics.set_snapshot_worker_alive(false);
    info!("snapshot worker exiting (sender dropped)");
}
```

`handle_snapshot_request` runs the existing Phase-B body:

1. `LedgerState::write_snapshot_view_to_path(&req.view, &epoch_path)`
2. `link_latest_snapshot(&epoch_path, &latest_path)` (non-fatal on
   failure — `latest.bin` is regenerable from the enumerator).
3. `prune_old_snapshots_in_dir(&database_path, max_snapshots + 1)`.
4. `info!("Snapshot saved (epoch=…, …)`.

`catch_unwind` keeps the worker alive across a malformed-request
panic (a future SerDe bug or OOM in `bincode` should not kill
snapshots permanently).

#### 2. `Node::try_snapshot_async`

```rust
pub enum SnapshotEnqueue { Enqueued, Skipped, Closed }

impl Node {
    pub async fn try_snapshot_async(&self) -> SnapshotEnqueue {
        // Pre-flight: if the worker is busy, do not even take the
        // ledger lock. View-build is expensive (~100s of ms),
        // discarding it on `Full` would burn CPU + hold the lock
        // for nothing.
        let Some(tx) = self.snapshot_tx.as_ref() else {
            return SnapshotEnqueue::Closed;
        };
        if tx.capacity() == 0 {
            self.metrics.inc_snapshot_skipped_busy();
            debug!("snapshot worker busy — skipping (pre-lock)");
            return SnapshotEnqueue::Skipped;
        }

        // Phase A — under ledger write lock. Duration: LSM flush
        // + Arc::clone + HashMap deep-clones. ~0.5–2 s on mainnet.
        let req = {
            let mut ls = self.ledger_state.write().await;
            ls.consensus.opcert_counters = self.consensus.opcert_counters().clone();
            ls.utxo.diff_seq.clear();
            if let Err(e) = ls.save_utxo_snapshot() {
                error!("LSM flush failed during snapshot: {e}");
                self.metrics.inc_utxo_flush_failed();
                // Proceed: the in-memory `utxo_set` may be empty
                // (LSM backend) so the snapshot view is still
                // structurally valid; LSM SST is the UTxO ground
                // truth and on disk may be stale by one save cycle.
            }
            SnapshotRequest {
                view: LedgerStateSnapshot::from(&*ls),
                epoch: ls.epoch.0,
                slot: ls.tip.point.slot().map(|s| s.0).unwrap_or(0),
                utxo_count: ls.utxo.utxo_set.len(),
            }
            // ── LOCK RELEASED HERE ──
        };

        match tx.try_send(req) {
            Ok(()) => {
                self.metrics.inc_snapshot_enqueued();
                SnapshotEnqueue::Enqueued
            }
            Err(TrySendError::Full(_)) => {
                // TOCTOU race vs the pre-flight check; benign.
                self.metrics.inc_snapshot_skipped_busy();
                debug!("snapshot worker busy — skipping (post-lock)");
                SnapshotEnqueue::Skipped
            }
            Err(TrySendError::Closed(_)) => {
                error!("snapshot worker channel closed");
                SnapshotEnqueue::Closed
            }
        }
    }
}
```

`try_snapshot_async` is `async fn` only because it awaits the
ledger `RwLock`. It does **no I/O** of its own — once `try_send`
returns the function completes immediately and apply continues.

#### 3. Caller-side scheduler integration (correctness-critical)

Each at-tip call site replaces:

```rust
if scheduler.maybe_snapshot_check(...) {
    self.save_ledger_snapshot().await;
    scheduler.record_snapshot_taken(epoch);
}
```

with:

```rust
if scheduler.maybe_snapshot_check(...) {
    match self.try_snapshot_async().await {
        SnapshotEnqueue::Enqueued => {
            scheduler.record_snapshot_taken(epoch);
        }
        SnapshotEnqueue::Skipped | SnapshotEnqueue::Closed => {
            // Don't record — let the scheduler retry on the
            // next block. Otherwise the 2000-block interval
            // (`DEFAULT_SNAPSHOT_INTERVAL`) would delay the next
            // attempt by ~11 h on mainnet.
        }
    }
}
```

This is the key correctness fix: skipping a save **must not** reset
the scheduler's `blocks_since_snapshot` counter, or the next attempt
would be delayed by the full interval.

#### 4. Bulk replay (strict Haskell match)

Two replay paths exist and they have different async/sync flavors:

- **`crates/dugite-node/src/node/sync.rs:2150`** — chunk-file replay.
  Inside `try_for_each_chunk_block` which is a synchronous closure
  running on the current OS thread. Uses `ledger_state.blocking_write()`.
  - Drop the `should_snapshot_bulk` save inside the loop.
  - After the loop exits but **still inside the sync closure's
    enclosing function**, call `ls.save_snapshot(&snapshot_path)`
    synchronously using a single `blocking_write()` acquisition.
    This produces one snapshot artefact for the entire replay.

- **`crates/dugite-node/src/node/sync.rs:2611`** — LSM replay from
  ChainDB. Inside an `async fn`. Uses `.write().await`.
  - Drop the `should_snapshot_bulk` save inside the loop.
  - After the loop, `self.save_ledger_snapshot().await` once.

Why blocking save at end of replay is acceptable: replay is finished,
no apply is pending, no other lock contender; we want the snapshot
durable before forward sync starts.

#### 5. Shutdown sequence (race-free)

`Node::shutdown` (`mod.rs:4015–4033`) replaces the lone
`save_ledger_snapshot().await` with:

```rust
// 1. Drop the sender so the worker's recv() returns None.
let worker_handle = self.snapshot_worker_handle.take();
self.snapshot_tx = None;

// 2. Await the worker to drain any in-flight save (with timeout).
if let Some(h) = worker_handle {
    let _ = tokio::time::timeout(Duration::from_secs(20), h).await;
}

// 3. NOW do the synchronous final save. Worker is quiescent;
//    no race on epoch-N.bin or latest.bin.
self.save_ledger_snapshot().await;
```

The 30-s overall shutdown timeout (`mod.rs:4017`) still applies,
so the worker join is bounded by it.

### Files touched

- `crates/dugite-node/src/node/snapshot_worker.rs` *(new)* —
  worker task + `SnapshotRequest` + `run_snapshot_worker`.
- `crates/dugite-node/src/node/mod.rs` —
  - `mod snapshot_worker;`
  - `Node` fields: `snapshot_tx: Option<mpsc::Sender<SnapshotRequest>>`,
    `snapshot_worker_handle: Option<JoinHandle<()>>`.
  - Spawn worker in `Node::run` (or wherever background tasks
    start) and store the handle.
  - Shutdown sequence above.
  - `:4814` (post-block scheduler) → use `try_snapshot_async` +
    conditional `record_snapshot_taken`.
- `crates/dugite-node/src/node/epoch.rs` — add `SnapshotEnqueue` and
  `Node::try_snapshot_async`. Keep `Node::save_ledger_snapshot` for
  shutdown + end-of-replay. Keep `prune_old_snapshots_in_dir` and
  `link_latest_snapshot` (worker calls them).
- `crates/dugite-node/src/node/sync.rs` —
  - `:1499` (`bg_snapshot_scheduler` per-batch) — use
    `try_snapshot_async` + conditional `record_snapshot_taken`.
  - `:1746` (epoch-boundary `should_snapshot_normal`) — ditto, but
    also gate `snapshot_taken()` (the `SnapshotPolicy` field, not
    the scheduler) on `Enqueued`.
  - `:2150` chunk-file replay — drop inline save; sync end-of-loop
    save via `ls.save_snapshot(&snapshot_path)`.
  - `:2611` LSM replay — drop inline save; async end-of-loop
    `save_ledger_snapshot().await`.
- `crates/dugite-node/src/metrics.rs` — add:
  - `snapshot_enqueued_total` (counter)
  - `snapshot_skipped_busy_total` (counter)
  - `snapshot_failed_total` (counter)
  - `snapshot_worker_alive` (gauge, 0/1)
  - `utxo_flush_failed_total` (counter)
- `crates/dugite-ledger/src/state/snapshot_format.rs` — add
  `debug_assert!(s.utxo.utxo_set.has_store(), …)` to the
  `From<&LedgerState>` impl so the in-memory-backend foot-gun is
  surfaced in tests.

### Send-bound check

`LedgerStateSnapshot` is `Send + 'static` (all fields are `Send`
HashMaps / Arc'd HashMaps / owned types). The existing #649
`spawn_blocking` already crosses threads with the same view, so
this is established.

### Backpressure (Haskell-aligned)

Channel capacity = 1. Pre-flight check + post-lock `try_send`:

- Pre-flight skip is the common path when the worker is busy.
- Post-lock `Full` is a benign TOCTOU race — bounded to one
  view-build worth of work.
- Skip never records the snapshot as taken, so the scheduler
  retries on the next block.

This mirrors cardano-node's `SnapshotDelayRange` random delay gate
("skip if too recent").

## Testing

### Unit tests — `snapshot_worker.rs`

- `worker_writes_request_to_disk` — push one `SnapshotRequest`,
  assert `ledger-snapshot-epochN-slotS.bin` exists with correct
  magic + version + checksum.
- `worker_skips_on_full_via_try_send` — fill the channel, assert
  next `try_send` returns `Full` and the worker is still alive.
- `worker_drains_after_sender_dropped` — drop sender mid-write,
  assert the worker exits cleanly within timeout.
- `worker_survives_payload_panic` — inject a panicking serializer
  via a test seam, assert the worker continues to process the
  next request.
- `prune_runs_after_each_write` — write three snapshots with
  `max_snapshots=2`, assert only the two newest remain.

### Unit tests — `try_snapshot_async`

- `enqueued_returns_enqueued_and_record_taken_fires` — happy path.
- `pre_flight_skip_does_not_acquire_lock` — fill channel, assert
  `ledger_state` write count unchanged after `try_snapshot_async`.
- `skipped_does_not_record_snapshot_taken` — assert scheduler's
  `blocks_since_snapshot` keeps growing across skip events.
- `closed_returns_closed_and_logs_error` — after shutdown sequence.

### Replay regression — `sync.rs`

- `chunk_replay_emits_zero_intermediate_snapshots` — drive replay
  with a fake `ChainDB`, assert `ls.save_snapshot` is NOT called
  inside the loop, and that one save fires after the loop exits.
- `lsm_replay_emits_zero_intermediate_snapshots` — same for the
  async LSM-replay path.

### Shutdown ordering

- `shutdown_drops_sender_before_sync_save` — assert no
  concurrent `.tmp` writes by instrumenting `write_snapshot_view_to_path`.

### Existing gates (must remain green)

- `verify-ledger-snapshot` CI step — no `LedgerStateSnapshot`
  field change, so byte-equivalence holds.
- `cargo test --doc`, `cargo nextest run --workspace`,
  `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

### Manual / soak

- Run a from-genesis preview replay with this branch; confirm:
  - No `Snapshot saved` line during replay.
  - Exactly one save fires after `Replay complete`.
  - Sustained ≥ 900 blk/s throughout (no gaps > 5 s).
- Run at-tip on preview for >2 epochs; confirm:
  - Apply latency P99 < 2 s during snapshot window (vs > 5 s
    today).
  - `snapshot_skipped_busy_total` ≈ 0 in steady state.

## Risks

### Worker death silently disables snapshots
Mitigation: `snapshot_worker_alive` gauge metric; `catch_unwind`
in the worker body. TUI can surface the gauge dropping to 0.

### Apply outruns the worker
Channel cap=1 with pre-flight skip prevents queue growth. Skip
counter visible in metrics. In steady state at-tip the worker has
hundreds of blocks of headroom between cadence-driven saves.

### LSM flush failure under the lock
Existing behavior preserved: log + continue. On reload the LSM SST
may lag the ledger snapshot by one save cycle (typically the
delta between two epoch boundaries). Surfaced via
`utxo_flush_failed_total` metric.

### In-memory UTxO backend foot-gun
A `debug_assert` in `LedgerStateSnapshot::from` surfaces the
multi-GB deep clone in tests/benchmarks. Production paths attach
LSM at startup.

### Non-atomic `link_latest_snapshot`
`remove_file` + `hard_link` has a sub-microsecond window where
`latest.bin` is absent. A concurrent `verify-ledger-snapshot`
reader hitting that window sees `NotFound` and would have to
retry. Existing behavior; not changed here. Document in the
worker's success log line.

### P99 < 10 ms not achieved
Documented in "Honest acceptance criteria" above. File a follow-up
issue: *"Reduce Phase-A snapshot view-build to O(Arc-clones) via
ledger-state Arc'ification or LedgerSeq immutable anchor"*.
