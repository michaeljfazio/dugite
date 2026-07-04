---
name: issue-767-residual-stall-proposed-fix-review
description: Adversarial review of proposed 4-fix plan for #767 residual peer-Slow-cascade stall (2026-06-16)
metadata:
  type: project
---

# #767 Residual Stall: Proposed Fix Review (2026-06-16)

## Key findings from reading real code

### Fix 1 — Hoist chain_db.read() outside candidate_chains.write() (sync.rs:5371)

**The lock nesting described is REAL and CONFIRMED.**
- sync.rs:5332: `let mut chains = candidate_chains.write().await;`
- sync.rs:5371: `{ let cdb = chain_db.read().await; ... }` — nested inside the write lock block
- This means 48 ChainSync tasks can simultaneously hold `candidate_chains.write()` and be suspended awaiting `chain_db.read()`
- When `apply_fetched_block` (on the main task, NOT in the ChainSync tasks) holds `chain_db.write()` for the periodic 1s WAL fsync (mod.rs:6099-6104), all 48 `chain_db.read()` waiters block, holding `candidate_chains.write()`
- BlockFetch decision task at connection_lifecycle.rs:2444 takes `candidate_chains.read()` then `chain_db.read()` — blocked by the write lock convoy on `candidate_chains`
- **The fix is CORRECT and LOW RISK.** The TOCTOU window (block arrives between chain_db.read() and candidate_chains.write()) is harmless: the header gets pushed to pending_headers and pruned on next cycle. One redundant BlockFetch request at worst.
- **WARNING**: WAL fsync holds chain_db.write() for ~1ms, not long enough to explain a 5-7 min stall. Fix 1 eliminates the lock convoy but is unlikely to fully cure the stall alone.

### Fix 2 — Call governor.record_demote() for PeerFailureKind::Slow (mod.rs:5167)

**WILL NOT COMPILE AS DESCRIBED.**
- `record_demote` at governor.rs:213 is a PRIVATE method (`fn`, not `pub fn`).
- The fix requires first making `record_demote` public (or adding a new public wrapper like `pub fn record_slow_demote(addr, now)` in Governor).
- Additionally: `pm.peer_failed()` calls `inner.demote_to_cold()` (networking.rs:682) — the peer goes COLD, not WARM. The `governor.record_demote()` cooldown only blocks Cold→Warm and Warm→Hot promotions — so calling it IS the right behavior (prevent immediate Cold→Warm reconnect), but the description says "demote_to_warm" path, which is misleading.
- The 300s cooldown matches Haskell's `policyPeerShareActivationDelay` — semantically correct.
- **Fix is CONCEPTUALLY CORRECT but needs a public API change to Governor first.**

### Fix 2 — Cascade count correction

**The proposal claims "48 simultaneous Slow failures via FETCH_RANGE_TIMEOUT" — this is PARTIALLY WRONG.**
- Only ONE peer holds the active_fetcher CAS slot at a time (connection_lifecycle.rs:2365). Only that one peer can hit `recv_batch` and fire FETCH_RANGE_TIMEOUT.
- The 47 other peers loop on the 10ms poll_ticker CAS and never call recv_batch.
- HOWEVER: ChainSync tasks ALSO fire Slow (connection_lifecycle.rs:2169) on any bearer error (decode, stale intersection). If the stall causes 48 ChainSync tasks to fail (bearer close, reconnect storm), 48 Slow events ARE possible — just not via FETCH_RANGE_TIMEOUT.
- Step 2 of the proposed mechanism (timeout fires on concurrent peers) is wrong for blockfetch but the cascade is still real via chainsync errors.
- Fix 2 is still the right response regardless — the issue is that Slow failures don't enter cooldown.

### Fix 3 — Backpressure-aware timeout (channel-full check at line 3080)

**The channel-send path is already cancel-aware (connection_lifecycle.rs:3176-3183).**
- `fetched_blocks_tx.send(fetched)` is already inside `tokio::select! { biased; _ = cancel.cancelled() => return; r = ... send => r }`.
- When the channel is full, the worker parks here but can be cancelled — the `spsDeactivateTimeout` issue is already fixed.
- Fix 3 would prevent the SINGLE active blockfetch peer from being marked Slow when it fills the channel. This is CORRECT in principle (it IS backpressure not peer-slowness) but medium risk because: (1) genuine slow peer + full channel → no Slow penalty ever; (2) requires exposing channel capacity to the blockfetch worker. `fetched_blocks_tx.capacity()` is available on a Tokio mpsc Sender — feasible.
- **However**: the current code path shows that FETCH_RANGE_TIMEOUT fires on `recv_batch` (line 3007), NOT on the `send()`. A full channel doesn't cause the timeout — it just parks the worker. So Fix 3 solves a HYPOTHETICAL path, not the observed one unless the observed Slow fires come from ChainSync errors not blockfetch timeouts.

### Fix 4 — Channel cap 1024 → 4096

**Safe and straightforward.**
- Memory at Alonzo avg ~5KB/block: 4096 × 5KB = ~20MB. Fine.
- The doc comment at mod.rs:120-129 explains the existing reasoning (90MB at Conway = acceptable).
- This is defense-in-depth, not a root-cause fix.

## Summary of issues with the proposed fixes

1. **Fix 1**: Correct, low risk, should be applied. Will NOT alone fix the stall (WAL fsync window is ~1ms).
2. **Fix 2**: Correct intent, WILL NOT COMPILE — `record_demote` is private. Requires adding `pub fn record_slow_demote(&mut self, addr: SocketAddr, now: Instant)` to Governor (or making `record_demote` pub). Also the description says the peer goes Warm — it goes Cold via `peer_failed()`.
3. **Fix 3**: Addresses a hypothetical path (send-park → Slow). The observed Slow events are more likely from ChainSync bearer errors (48 simultaneous), not blockfetch send-park. Still useful but medium risk and medium priority.
4. **Fix 4**: Safe, should apply as defense-in-depth.

## Missing from the proposed analysis

The proposal doesn't address whether the `candidate_chains.write()` at connection_lifecycle.rs:3165 (inside the block-drain loop, called once per block while the channel might be full) is also nested inside `chain_db` in a problematic way. That loop acquires `candidate_chains.write()` for `record_fetch_delivered()` per block — but does NOT acquire chain_db, so no cross-lock issue there.

The proposal also doesn't address `update_query_state()` at mod.rs:6957-6959 — fires every 30s in catch-up mode, ~1.4s synchronous on the apply task. This is APPLY LAG but not a cascade trigger.

## Correct order of fixes

1. Apply Fix 1 (hoist chain_db.read in sync.rs:5372) — no API changes needed
2. Apply Fix 2 with public API: add `pub fn record_slow_demote` to Governor, call from mod.rs:5179 — fixes the instant re-promotion cycle
3. Apply Fix 4 (cap 1024→4096) alongside
4. Fix 3 is a follow-up — medium risk, address separately

**Why:** lock-convoy fix (Fix 1) + cooldown fix (Fix 2) together break both the secondary sustain mechanism and the root re-trigger. Fix 3 is only needed if Slow events fire from blockfetch send-park (not currently the primary path).
