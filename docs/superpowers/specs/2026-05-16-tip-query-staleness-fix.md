# Tip-query staleness: forge path skips snapshot + metric updates — Fix Design

**Date:** 2026-05-16
**Status:** Design / Pre-implementation
**Priority:** P1
**Related:** unblocks `verify.sh` predicate 4 ("tip parity over time"). Currently
the predicate excludes dugite-bp as a workaround.
**File:** `crates/dugite-node/src/node/mod.rs`

---

## Problem

`cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-501/dbp.sock`
against dugite-bp returns a stale tip — typically the last *peer-adopted*
block, even though the actual chain has advanced by many own-forged blocks.

Prometheus reports the same staleness: `dugite_block_number` and `dugite_slot`
gauges do not advance when dugite-bp forges its own block. They only advance
on peer-block adoption.

**Concrete evidence from `evidence/20260516T084623Z/`:**

| source | value |
|---|---|
| `dugite_blocks_forged_total` (Prometheus) | 26 |
| `dugite_block_number` (Prometheus) | 1 (last peer-adopted) |
| `cardano-cli query tip` block_no | 1 |
| `cardano-cli query tip` slot | 22 |
| actual VolatileDB tip (from log) | block 40, slot 313 |

UTxO queries against the same socket are NOT affected — they read from
`ledger_state` directly. Only the tip-style queries
(`GetChainBlockNo`, `GetChainPoint`, `GetCurrentEra`) and the Prometheus
gauges that share the same snapshot/metric writes are stale.

---

## Root cause

The N2C `QueryHandler` (`crates/dugite-node/src/node/n2c_query/mod.rs`) reads
the chain tip from `state: Arc<NodeStateSnapshot>`:

```rust
2 => QueryResult::ChainBlockNo(self.state.block_number.0),
3 => match &self.state.tip.point { ... }   // GetChainPoint
```

`NodeStateSnapshot` is refreshed by `Node::update_query_state()`
(`query.rs:169`). That function is currently called from exactly two places:

1. Once at boot, after ledger init (`mod.rs:1817`).
2. After every successful `apply_fetched_block`, rate-limited to once per
   second (`mod.rs:3855`).

**The forge apply path
(`try_forge_block_at`, `mod.rs:5358-5475`) calls neither
`update_query_state()` nor any of the metric setters
(`metrics.set_block_number`, `metrics.set_slot`, `metrics.set_tip_slot_time_ms`,
`metrics.set_epoch`).**

When dugite-bp forges its own block, the ledger, ChainDB, chain fragment, and
consensus tip are all updated correctly. The N2C snapshot and the Prometheus
gauges are left frozen.

This is a silent omission, not a deliberate decision: the same set of updates
is present in `apply_fetched_block` (`mod.rs:3747-3855`) but was never
duplicated into the forge path when forging was added.

---

## Fix

Extract a single helper that runs the **post-apply housekeeping** — metrics +
snapshot refresh + mempool sweep — and call it from both code paths.

### Step 1: Extract `post_block_apply_updates`

Add a private async method to `Node`:

```rust
/// Refresh post-apply state shared by every code path that adopts a block.
///
/// Responsible for:
/// - Updating live metrics (block_number, slot, tip_slot_time_ms, epoch,
///   sync_progress).
/// - Refreshing the N2C `NodeStateSnapshot` (rate-limited via
///   `last_query_state_update`).
/// - Removing confirmed transactions from the mempool and re-running
///   mempool input-conflict / TTL sweeps.
///
/// Lock order: ledger_state read lock acquired briefly (metrics + mempool
/// sweep), then released before `update_query_state()` acquires it.
async fn post_block_apply_updates(
    &mut self,
    block: &Block,
    block_slot: SlotNo,
    block_number: BlockNo,
) {
    // 1. Metrics — set_block_number, set_slot, set_tip_slot_time_ms, set_epoch.
    self.metrics.set_block_number(block_number.0);
    self.metrics.set_slot(block_slot.0);
    {
        let ls = self.ledger_state.read().await;
        let sc = &ls.slot_config;
        let slot_time_ms = sc.zero_time
            + block_slot.0.saturating_sub(sc.zero_slot) * sc.slot_length as u64;
        self.metrics.set_tip_slot_time_ms(slot_time_ms);
        self.metrics.set_epoch(ls.epoch.0);
    }
    self.metrics.refresh_sync_progress(block_slot.0);

    // 2. Mempool: remove confirmed + sweep invalid (existing logic from
    //    apply_fetched_block:3795-3845).
    let confirmed: Vec<_> = block.transactions.iter().map(|tx| tx.hash).collect();
    if !confirmed.is_empty() {
        self.mempool.remove_txs(&confirmed);
    }
    if !self.mempool.is_empty() {
        // ... same input-conflict + TTL sweep as in apply_fetched_block.
    }
    self.metrics.set_mempool_count(self.mempool.len() as u64);
    self.metrics.mempool_bytes.store(
        self.mempool.total_bytes() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );

    // 3. Rate-limited N2C snapshot refresh.  Same predicate as the existing
    //    call site at mod.rs:3854 — at most once per second to avoid the
    //    O(n²) DRep delegator scan stalling the apply loop.
    if self.last_query_state_update.elapsed() >= Duration::from_secs(1) {
        self.update_query_state().await;
        self.last_query_state_update = Instant::now();
    }
}
```

### Step 2: Call the helper from BOTH paths

`apply_fetched_block` (`mod.rs:3747-3855`): replace the duplicated block of
metric + snapshot updates with a single call:

```rust
// (after the block is applied, fragment updated, consensus tip set,
//  Chain extended logged, blocks_received/applied incremented:)
self.post_block_apply_updates(&block, block_slot, block_number).await;

// Announce to downstream peers (UNCHANGED — stays inline because the
// announcement channel is specific to fetched blocks).
if let Some(ref tx) = self.block_announcement_tx { ... }
```

The fork-replay loop inside `apply_fetched_block` (the `TriggeredFork` arm,
mod.rs:3469-3596) already updates per-block metrics inline (lines 3556-3567).
It does NOT call `update_query_state`. After this fix, the *last* iteration of
the fork-replay loop will still run only its inline metric updates; the outer
`apply_fetched_block` doesn't fall through to the post-apply path because
`fork_replayed=true` causes an early `return`. We move the
`update_query_state()` call into the fork-replay loop too (rate-limited so the
multi-block replay only refreshes once at the end).

`try_forge_block_at` (`mod.rs:5358-5475`): add the same call after
`blocks_forged` increment + `TraceAdoptedBlock` log:

```rust
self.metrics.blocks_forged.fetch_add(1, Ordering::Relaxed);
info!(target: "forge", ..., "TraceAdoptedBlock");

// NEW: refresh post-apply state so N2C tip queries and Prometheus gauges
// reflect the own-forged block immediately.
self.post_block_apply_updates(&block, next_slot, block_number).await;

// Announce the new block (UNCHANGED — forge has its own announcement
// semantics with subscriber-count checking).
if let Some(ref tx) = self.block_announcement_tx { ... }
```

### Step 3: Unit test the helper is called from both paths

Add a test that asserts the Prometheus `block_number` gauge advances after a
successful forge and after a successful peer-block adopt:

```rust
#[tokio::test]
async fn forge_advances_metrics_and_snapshot() {
    let node = Node::new_for_test(...).await;
    let initial_block_number = node.metrics.block_number.load(Ordering::Relaxed);

    // Synthesize a forge result that wins the chain race.
    node.try_forge_block_at_for_test(slot_with_leader, ...).await;

    // After post_block_apply_updates fires, block_number must reflect the forged block.
    let new_block_number = node.metrics.block_number.load(Ordering::Relaxed);
    assert!(new_block_number > initial_block_number);

    // The N2C snapshot must also reflect the new tip.
    let handler = node.n2c_query_handler();
    let snapshot_tip = handler.state().tip.point.slot().unwrap().0;
    assert_eq!(snapshot_tip, slot_with_leader.0);
}
```

A similar test asserts `apply_fetched_block` still calls the helper (we did not
regress the existing path).

---

## Why minimal and correct

**Minimal:**
- 1 new private async method (~80 LoC).
- 2 call sites inserted (1 line each).
- Existing metric/snapshot update blocks in `apply_fetched_block` are
  REMOVED in favor of the helper call — net LoC change is negative.

**Correct:**
- Identical logic moved into one place. No semantic change for the existing
  `apply_fetched_block` path.
- Forge path gains the same updates that have been working correctly for
  peer-adopted blocks since N2C was added.
- The rate limiter (`last_query_state_update`) is preserved, so forge bursts
  do not cause snapshot rebuild storms (the O(n²) DRep scan).

**Lock order preserved:** the helper acquires `ledger_state` for the metric
read, drops it, then `update_query_state` acquires `ledger_state` again. Same
as the pre-extraction inline code. No new deadlock risk.

---

## Test strategy

1. **Unit test (new):** `forge_advances_metrics_and_snapshot` (see Step 3).
2. **Unit test (existing):** `apply_fetched_block_advances_metrics_and_snapshot`
   already passes; assert it still passes after the refactor (no behavior
   change for the peer-adopted path).
3. **Integration (local-devnet):**
   - `./testnet/local-devnet/soak.sh 180` — 3-minute smoke.
   - `cardano-cli query tip --socket-path /tmp/ld-501/dbp.sock` after the soak
     must report a tip within 2 blocks of the relay's tip.
   - Re-run `verify.sh` predicate 4 (tip parity) WITHOUT the
     "excluding dugite-bp" workaround — must report ≥95% in-parity windows.

---

## Risks

1. **Forge burst causing snapshot rebuild storm.** Mitigated by the existing
   rate limiter (1 refresh / second). Even if dugite-bp forges every 2 slots
   on a hot devnet, at most one snapshot rebuild fires per second.

2. **Mempool sweep behavior change for forged blocks.** Forged blocks already
   trigger a mempool `remove_txs` call (mod.rs:5399). Adding the full sweep
   (TTL + input conflict) duplicates that work but is idempotent. We can
   short-circuit the sweep when called from the forge path if profiling shows
   it matters; for now keep it identical.

3. **Lock ordering invariant.** The helper acquires `ledger_state` for read,
   releases it, then `update_query_state()` re-acquires for read. This is
   the same pattern as the pre-extraction inline code; no new contention risk.

4. **Test scaffolding.** We need a `Node` constructor for tests that supports
   the forge path. The existing test harness at `mod.rs:5829+` already
   exercises gate predicates; we extend it with a minimal forge-success path
   that doesn't require a real VRF/KES setup (use the existing test fixtures
   for forged blocks).

---

## Estimated effort

| Component | Change | LoC |
|---|---|---|
| `mod.rs`: extract `post_block_apply_updates` | new method | ~80 |
| `mod.rs`: call site in `apply_fetched_block` | replace inline | -40 net |
| `mod.rs`: call site in `try_forge_block_at` | insert call | +5 |
| `mod.rs`: call site in `apply_fetched_block` TriggeredFork loop | insert call | +5 |
| Unit test: forge_advances_metrics_and_snapshot | new | ~80 |
| **Total** | 1 file | **~130 net** |

Estimated wall-clock: 2 hours including verification.

---

## Acceptance criteria

1. `cargo nextest run -p dugite-node --release` — all tests pass, including
   the new forge-snapshot test.
2. `cargo clippy --workspace --all-targets -- -D warnings` — clean.
3. `cargo fmt --check` — clean.
4. After Bug D fix is also in, `./testnet/local-devnet/verify.sh testnet/local-devnet/evidence/<latest>/`
   prints `PASSED all predicates: p1 p2 p3 p4` with NO "excluding dugite-bp"
   caveat for predicate 4.
5. `cardano-cli query tip --socket-path /tmp/ld-501/dbp.sock` returns a tip
   within 2 blocks of the cardano-bp tip throughout the soak.
