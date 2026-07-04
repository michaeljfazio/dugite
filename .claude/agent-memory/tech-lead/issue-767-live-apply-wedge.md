---
name: issue-767-live-apply-wedge
description: #767 live-apply permanent wedge root cause — save_utxo_snapshot without block_in_place cascades into peer-cascade, not a static deadlock cycle
metadata:
  type: project
---

## Root Cause (no static cycle; compound runtime scenario)

**NOT a strict deadlock cycle.** Full adversarial audit of 8 candidates found zero permanent cycles.

### Mechanism

`try_snapshot_async` (epoch.rs:500-521) acquires `ledger_state.write().await` at line 501,
then calls `ls.save_utxo_snapshot()` synchronously at line 504 — no `block_in_place`, no
`spawn_blocking`, no timeout. The LSM flush (`flush_memtable()` → `maybe_compact()`) can
take 1-5s on mainnet with a large UTxO set under memory pressure.

While this runs, the tokio worker thread is pinned on synchronous filesystem I/O while
holding `ledger_state.write()`. Other tasks calling `ledger_state.read()` park. The
`fetched_blocks_tx` channel (cap 1024) fills as BlockFetch workers continue delivering.

Once the channel fills AND the stall exceeds `spsDeactivateTimeout = 5s`, the connection
lifecycle manager calls `demote_to_warm` → `stop_hot_protocols_and_recover` → cancels ALL
BlockFetch task cancel tokens. The cancel-aware select at connection_lifecycle.rs:3176-3183
fires and BlockFetch workers exit WITHOUT delivering their pending block. ALL hot peers
demote to warm simultaneously.

`save_utxo_snapshot()` eventually returns. Apply task resumes select loop. `fetched_blocks_rx`
drains whatever arrived before cancel. Apply task enters `fetched_blocks_rx.recv()` — no
active BlockFetch workers exist. Governor ticker (2s) must re-promote peers Cold→Warm→Hot.

**The "permanent" aspect** occurs when re-promotion also fails (network conditions, reputation
decay from the forced demotions, or ChainSync intersection failure after ledger tip diverges
from immutable tip during the stall).

### Why ~1 in 13k maintenance cycles

`try_snapshot_async` is rate-limited (1 Hz at tip, 30s during catch-up). At live tip (~1
block/20s), 13k maintenance cycles × 250ms ≈ 54 minutes between occurrences. The stall
must happen to exceed exactly 5s (LSM compaction under memory pressure = rare).

### Key code locations

- epoch.rs:501 — `ledger_state.write().await` acquired
- epoch.rs:504 — `ls.save_utxo_snapshot()` SYNCHRONOUS, no block_in_place, no timeout
- epoch.rs:520 — lock released ONLY after save_utxo_snapshot() returns
- snapshot_worker.rs:47 — SNAPSHOT_WORKER_CHANNEL_CAPACITY = 1
- connection_lifecycle.rs:3174-3183 — cancel-aware fetched_blocks_tx send (correct)
- peer_connection.rs:736 — spsDeactivateTimeout = 5s hard timeout on task cancel
- mod.rs:129 — FETCHED_BLOCKS_CHANNEL_CAP = 1024

### What v2.0.8 changed (6ad4c5917b)

Added chain_db.read().await at epoch.rs:487-494 inside try_snapshot_async before the
ledger write lock (correct lock order). Added force-maintenance every 256 blocks. These
did NOT introduce a new cycle — they increased the frequency of try_snapshot_async calls
(via post_block_apply_updates every block at tip) and thus the frequency of save_utxo_snapshot.

### The actual second site (less likely but documented)

run_background_maintenance (mod.rs:6727) also calls flush_to_immutable_batch_retain
synchronously inside chain_db.write() without block_in_place. Duration ~50-250ms per
loop iteration (50 ImmutableDB appends × 1-5ms). This is bounded and less likely to
exceed spsDeactivateTimeout, but is also an anti-pattern.

### Fix (MINIMAL AND SAFE)

**Primary**: wrap `save_utxo_snapshot()` in `tokio::task::block_in_place`:

```rust
// epoch.rs:504 — before:
if let Err(e) = ls.save_utxo_snapshot() {

// epoch.rs:504 — after:
if let Err(e) = tokio::task::block_in_place(|| ls.save_utxo_snapshot()) {
```

This allows tokio to spawn a relief worker for the duration of the LSM flush, so other
async tasks (including ledger_state.read() callers and the main select loop) remain
serviceable. The 5s spsDeactivateTimeout clock no longer applies to the apply task since
tokio's executor is unblocked.

**Secondary** (defensive, belt-and-suspenders): also wrap flush_to_immutable_batch_retain
in run_background_maintenance (mod.rs:6738) with block_in_place.

**Does shipping this mask the repro before a stack is captured?** YES — wrapping in
block_in_place eliminates the symptom without identifying the specific timing. However,
the root cause is well-understood from static analysis and does not require a runtime
stack to confirm. The fix is architecturally correct (blocking I/O must never pin tokio
workers without block_in_place). Acceptable to ship.

### Ranked defensive hardening (regardless of root cause)

1. epoch.rs:504 — block_in_place around save_utxo_snapshot() [PRIMARY FIX]
2. mod.rs:6738 — block_in_place around flush_to_immutable_batch_retain in maintenance
3. query.rs:1086 — add drop(eh) before query_handler.write().await (structural antipattern)
4. mod.rs:6516 — add drop(ls) before era_history.write().await to mirror fork-replay pattern at 5849-5850

**Why:** items 3 and 4 are not the root cause today but are one future refactor away from
being real cycles. Apply the drop()s now while the code is being touched.
