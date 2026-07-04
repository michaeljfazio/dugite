---
name: issue-767-residual-stall-lens-c
description: "#767 residual stall — full 4-lens synthesis PLUS adversarial review of proposed fix (2026-06-16): Fix 1 has deadlock risk, Fix 2 won't compile, cascade mechanism description has three factual errors"
metadata:
  type: project
---

## Full 4-Lens Synthesis: #767 Residual Stall Mechanism

### Reconciled Root Cause (ALL 4 Lenses Agree)

The residual stall is a **self-sustaining apply-lag-cascade**, NOT snapshot-triggered,
NOT epoch-boundary-only. The cycle period is ~60s (FETCH_RANGE_TIMEOUT), matching
the observed "every few minutes" within ep309.

**Trigger type: peer-cascade-self-sustaining**

### The Exact 9-Step Loop

1. **Apply-lag accumulates** (`mod.rs:4827 apply_fetched_block`): ValidateAll Alonzo
   blocks take ~50-200ms each. `update_query_state()` fires every 30s and costs ~1.4s
   (mod.rs:6956-6959). Over ~60s of sustained lag, the 1024-cap `fetched_blocks_rx`
   channel fills (mod.rs:129).

2. **Blockfetch drain parks on send** (`connection_lifecycle.rs:3176-3182`): the
   cancel-aware `select! { cancel || fetched_blocks_tx.send(fetched) }` parks on the
   send because the channel is full. The peer is delivering blocks but they queue up.
   NOTE: `FETCH_RANGE_TIMEOUT=60s` (line 54) guards `recv_batch` (line 3007), NOT the
   post-batch drain send — so the timer fires on the NEXT range request, not immediately.

3. **FETCH_RANGE_TIMEOUT fires on the next range** (connection_lifecycle.rs:3080-3108):
   after 60s the timer fires → `peer_failure_tx.try_send(Slow)` → worker exits →
   `ActiveFetcherGuard` dropped.

4. **peer_failed skips cooldown** (networking.rs:663-683, mod.rs:5167-5178):
   `peer_failure_rx` arm calls only `pm.peer_failed()` for `Slow` — reputation hit,
   sets `next_connect_after` backoff, calls `inner.demote_to_cold()`. Critically: does
   NOT call `lifecycle.demote_to_cold()` (no TCP teardown) and does NOT call
   `governor.record_demote()` → peer is invisible to `in_cooldown()`.

5. **Governor re-promotes in ≤2s** (governor.rs:593-628): `belowTargetOther` warm→hot
   path sees `hot_count < target_hot`, iterates warm peers, calls `in_cooldown()` which
   returns `false` (never written by peer_failed path), emits `PromoteToHot` immediately.

6. **Mass rollback-to-tip storm** (sync.rs:5779-5798): each re-promoting peer's
   `chainsync_client_task` runs `try_find_intersect`, agrees on our current tip, peer
   sends `MsgRollBackward rollback_point=<our_tip>`. With 48 peers, 48 simultaneous
   rollbacks → 48 INFO log lines → 48 × `candidate_chains.write()` acquisitions.

7. **Lock-dependency chain creates secondary stall** (sync.rs:5332-5404):
   MsgRollForward arm holds `candidate_chains.write()` while awaiting `chain_db.read()`
   at line 5372 — nested inside the write lock. `apply_fetched_block` holds
   `chain_db.write()` for periodic WAL fsync every 1s (mod.rs:6099-6104). During that
   ~1ms write window, ALL 48 MsgRollForward handlers waiting for `chain_db.read()` are
   blocked while holding `candidate_chains.write()`. BlockFetch decision task needs
   `candidate_chains.read()` (connection_lifecycle.rs:2444) — blocked. No new fetch
   ranges built → `fetched_blocks_tx` goes idle despite apply task running.

8. **Apply idles** (mod.rs:4818): `fetched_blocks_rx.recv()` blocks because no
   blocks are arriving from any fetcher. The channel drains to empty, then the loop
   waits indefinitely.

9. **Re-trigger**: once peers re-establish and a fetcher claims `active_fetcher`,
   step 1 begins again. The re-intersection churn itself (step 7 lock convoy) adds
   CPU/lock latency that slows the apply task → channel fills faster → shorter time
   to the next FETCH_RANGE_TIMEOUT → cycle frequency increases over time.

### Why it is NOT snapshot-triggered for mid-epoch stalls

- `bg_snapshot_scheduler` in `catchup_mode=true` fires at most once per 30 minutes
  (bulk_sync_rate_limit=1800s, background.rs:633-637), only at epoch boundaries.
- Slot-interval trigger suppressed in catchup_mode (background.rs:656-659).
- Old `SnapshotPolicy` (should_snapshot_bulk/normal) has no live callers on the
  apply path — only used by `process_forward_blocks` (Mithril/chunk-file replay).
- Stalls every few minutes within ep309 are impossible to explain via snapshot cadence.

### Key code locations (verified)

| Location | What |
|----------|------|
| `mod.rs:129` | `FETCHED_BLOCKS_CHANNEL_CAP = 1024` |
| `connection_lifecycle.rs:54` | `FETCH_RANGE_TIMEOUT = 60s` |
| `connection_lifecycle.rs:3007` | `recv_batch` wrapped in timeout (NOT the drain send) |
| `connection_lifecycle.rs:3176-3182` | cancel-aware drain send — parks when channel full |
| `connection_lifecycle.rs:3108` | `peer_failure_tx.try_send(Slow)` on timeout |
| `mod.rs:5167-5178` | `peer_failure_rx` arm — `Slow` → `peer_failed()` only |
| `networking.rs:663-683` | `peer_failed()` — no `record_demote()` call |
| `governor.rs:213-215` | `record_demote()` — NEVER called from `peer_failed` path |
| `governor.rs:616` | `in_cooldown()` — bypass means instant re-promotion |
| `governor.rs:596-628` | `belowTargetOther` warm→hot — no batch cap |
| `mod.rs:6099-6104` | `chain_db.write()` every 1s for WAL fsync (!at_tip) |
| `sync.rs:5333` | `candidate_chains.write()` start of MsgRollForward block |
| `sync.rs:5372` | `chain_db.read()` NESTED inside `candidate_chains.write()` |
| `mod.rs:6956-6959` | `update_query_state()` every 30s catch-up, ~1.4s cost |
| `mod.rs:4827` | `apply_fetched_block` synchronous arm — blocks select! |

### Proposed Fixes (in priority order)

**Fix 1 (highest impact, lowest risk): Hoist chain_db.read() outside candidate_chains.write() lock in MsgRollForward (sync.rs:5372)**

Move the `chain_db.read()` acquisition to BEFORE the `candidate_chains.write()` block.
This breaks the write-lock convoy: even with 48 peers' MsgRollForward handlers all
queuing for `candidate_chains.write()`, none are blocked waiting for `chain_db.read()`
while holding it. No semantic change — the `has_block()` result and prune are still
correct; there is a tiny TOCTOU window (chain_db advances between read and write)
but prune over-deletes at most 1 header (the just-stored block) which is harmless
because it will be re-announced via MsgRollForward.
Byte-exact safe: yes (no ledger/consensus logic, pure lock ordering change).

**Fix 2 (critical correctness): Call record_demote() for Slow failures (mod.rs:5167 / networking.rs:663)**

After `pm.peer_failed(&failed_addr)` for `PeerFailureKind::Slow`, call
`governor.record_demote(failed_addr, Instant::now())`. This inserts the peer into
`recently_demoted` with a 300s cooldown, preventing the governor's `belowTargetOther`
from immediately re-promoting the just-failed peer. This is the primary fix for
the self-sustaining cascade: without instant re-promotion, the FETCH_RANGE_TIMEOUT
cycle period becomes 300s instead of 60s, breaking the loop.
Byte-exact safe: yes (governance of peer connections only).

**Fix 3 (prevents initial trigger): Distinguish channel-full backpressure from peer-slow timeout**

The `FETCH_RANGE_TIMEOUT` guards `recv_batch` but the stall starts when the drain
send parks on a full channel. Adding a check: if `fetched_blocks_tx.is_full()` when
a range timeout fires, log "apply backpressure" instead of `Slow`, and do NOT send
`peer_failure_tx(Slow)`. This prevents the cascade from starting when the apply task
is simply slow (WE are the bottleneck, not the peer).
Byte-exact safe: yes.

**Fix 4 (defense in depth): Increase FETCHED_BLOCKS_CHANNEL_CAP**

Raise from 1024 to 4096. Absorbs longer apply spikes before channel fills. At 90KB/block
the worst-case memory is ~360MB (Conway blocks). During Alonzo (avg ~5KB/block) it's
~20MB. Buys more time before the cascade triggers. Does NOT fix the root cause.
Byte-exact safe: yes.

**How to apply:** Fix 1 + Fix 2 together eliminate both the secondary stall (lock convoy)
and the self-sustaining re-trigger (no cooldown). Fix 3 eliminates the initial trigger.
Fix 4 is a fallback that makes the node more resilient to any apply latency spikes.

---

## ADVERSARIAL REVIEW OF PROPOSED FIX (2026-06-16)

Verdict: needs-revision. Three factual errors in the cascade description, Fix 1 introduces a new deadlock, Fix 2 does not compile as stated.

### Error 1: Step 2 mechanism (FETCH_RANGE_TIMEOUT fires when channel full) — WRONG

`FETCH_RANGE_TIMEOUT=60s` at `connection_lifecycle.rs:3007` wraps `recv_batch_future` — the TCP peer response timer. A full `fetched_blocks_tx` channel parks the blockfetch worker at `line 3182` (`send().await`) inside a cancel-aware `select!`. The send has NO separate timeout — it only unblocks on cancel or channel drain. `FETCH_RANGE_TIMEOUT` and channel fullness are orthogonal events. A full channel does NOT cause `recv_batch` to fire the timeout. The timeout fires only when the PEER fails to deliver blocks in 60s.

The actual trigger sequence: apply-lag → channel fills → blockfetch worker parked at `send().await` → NEW range requests are not issued → peer's TCP connection eventually goes idle or peer stalls → THEN recv_batch times out. This is a second-order effect, not the direct one stated.

### Error 2: Step 4 — `send()` is NOT inside `candidate_chains.write()` — WRONG

The proposal claims the `fetched_blocks_tx.send()` drain (line 3182) is inside `candidate_chains.write()`. It is not. The write lock is acquired at `line 3165` for `record_fetch_delivered()`, and is dropped at `line 3169` (closing brace of the inner block). The `send().await` at `line 3182` runs AFTER the write lock is released. The claim about "48 MsgRollForward handlers block[ing] `candidate_chains.write()` while awaiting `chain_db.read()`" has the correct specific detail (line 5372 IS inside the write lock at 5333), but the send-drain chain through candidate_chains is invented.

### Error 3: Step 3 — peer goes to Cold, NOT Warm, after Slow failure — WRONG

`peer_failed()` at `networking.rs:682` calls `self.inner.demote_to_cold(addr)` — the peer goes to Cold state in PeerManager. The proposal says "no `lifecycle.demote_to_cold()`" (correct) and "no `record_demote()`" (correct), but then says the governor's warm→hot path immediately re-promotes. The warm→hot path only sees Warm peers. Since the peer is now Cold, re-promotion requires Cold→Warm (TCP reconnect, background task) then Warm→Hot — two governor cycles, not one. This makes the self-sustaining cycle period ~4s minimum (two 2s ticks), not instant. The no-cooldown issue is real but the path is slower than stated.

### Fix 1 Introduces a New Deadlock

Proposed reorder: acquire `chain_db.read()` BEFORE `candidate_chains.write()` in `sync.rs:5372`.

The blockfetch decision task at `connection_lifecycle.rs:2444` acquires `candidate_chains.read()` THEN `chain_db.read()`. With the reorder:

- Task A (MsgRollForward): holds `chain_db.read()`, waiting for `candidate_chains.write()`
- Task B (blockfetch decision): holds `candidate_chains.read()`, waiting for `chain_db.read()`
- Task C (apply WAL fsync mod.rs:6100): waiting for `chain_db.write()`

tokio RwLock is writer-preferring: once C's write is pending, new `chain_db.read()` calls block. B already holds `candidate_chains.read()` and now cannot get `chain_db.read()` (C's write pending blocks new reads). A holds `chain_db.read()` waiting for `candidate_chains.write()` (blocked by B's read). C waits for existing chain_db readers (A) to release. Cycle: A→blocked by B, B→blocked by C, C→blocked by A. This is a three-way deadlock.

The current code is safe precisely because `chain_db.read()` is acquired INSIDE `candidate_chains.write()`, not before it — so `chain_db.read()` and `candidate_chains.read()` are never held simultaneously.

**Safer alternative for Fix 1:** Split the MsgRollForward critical section into two: (a) acquire+drop `candidate_chains.write()` for tip/pending-headers mutations, (b) separately call `prune_already_known_pending_headers()` with a fresh write lock acquisition and a separately-acquired `chain_db.read()`. Or hold `candidate_chains.write()` but tighten it to NOT await `chain_db.read()` inside — instead cache a snapshot of hashes from a prior read outside. Or change `prune_already_known_pending_headers` to take `&HashSet<Hash32>` instead of `&ChainDB` to avoid the nested lock acquisition.

### Fix 2 Does Not Compile As Stated

`governor.record_demote()` at `governor.rs:213` is declared `fn record_demote(...)` — private, not `pub fn`. Cannot be called from `mod.rs`. Must add a public wrapper method first:

```rust
// in governor.rs
pub fn record_peer_slow(&mut self, addr: SocketAddr, now: Instant) {
    self.record_demote(addr, now);
}
```

Then call `governor.record_peer_slow(failed_addr, Instant::now())` from the `peer_failure_rx` arm in `mod.rs:5167`. Additionally the `governor` variable is in scope at that point (declared at `mod.rs:4656`) — the access itself is fine once the method is public.

### Fix 3 Is Placed Correctly but Addresses a Narrower Case

The channel-full check in the `Err(_elapsed)` arm at `connection_lifecycle.rs:3080` correctly handles the case where (1) the peer genuinely timed out on `recv_batch` AND (2) the channel is simultaneously full. In that case, the apply task is the bottleneck and reporting the peer as Slow is a false positive. The check is valid and useful. However it does NOT address the primary cascade path (which goes through governor demote → reconnect → rollback storm → chains.write() convoy), only the narrower scenario described above.

### Fix 4 Memory Estimate

The comment at `mod.rs:4292` says the cap was reduced from 1000 to 128 to cap memory; current cap is 1024. FetchedBlock structs contain a decoded `Block` (allocated) plus the raw CBOR bytes clone. At Conway 90KB avg, 4096 slots × 90KB = ~368MB just for this channel. During Alonzo the blocks are smaller but the decoded structs still have heap allocations per transaction. The ~20MB Alonzo estimate may undercount struct overhead.
