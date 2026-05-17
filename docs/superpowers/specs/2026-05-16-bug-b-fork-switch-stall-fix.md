# Bug B: Fork-switch stall — live apply path skips LedgerSeq delta push

**Status:** Design (ready to implement)
**Date:** 2026-05-16
**Branch:** feature/local-testnet-docs
**Author:** Tech Lead

---

## Problem

In the local-devnet 3-node scenario (dugite-bp + dugite-relay + cardano-bp), after dugite-bp
performs its first successful fork switch it permanently stalls: no further `Chain extended`
events appear, even though the relay's chain grows normally to block 33+.

**Concrete log evidence** (`testnet/local-devnet/logs/dugite-bp.log`):

```
# 5 successful "Chain extended" events for relay blocks at slots 17-46 (blocks 0-4)
07:25:37  INFO  chain_sel: switching to longer fork  fork_block_no=11 fork_slot=79 current_tip_block_no=10
07:25:37  INFO  VolatileDB: chain switch  rollback_count=6 apply_count=7
07:25:37  INFO  Chain selection: fork switch at live tip — rolling back ledger ... intersection_slot=46
07:25:37  ERROR Rollback target outside LedgerSeq volatile window AND no canonical snapshot available.
             Aborting rollback; ledger state preserved. rollback_slot=46 ledger_slot=78
07:25:37  WARN  Fork replay: ledger apply failed: Block does not connect to tip:
             expected e21942... (dugite slot-78 self-forged), got 1599a4... (relay slot-46)
             — clearing volatile and resyncing slot=51 block=5

# Then, every ~2s for 3+ minutes, for every new relay block:
07:25:39 chain_sel: switching to longer fork  fork_block_no=12 fork_slot=80 current_tip_block_no=11
07:25:39 chain_sel: fork unreachable — StoreButDontChange  fork_block_no=12
...  (repeated for blocks 12..33)
```

The ChainSync pipeline is alive and BlockFetch fires for every relay header. The stall is
entirely in chain selection: `StoreButDontChange` is returned for every relay block.

---

## Root Cause

A 3-step cascade triggered when a `TriggeredFork` fires before the LedgerSeq has any deltas.

### Step 1 — LedgerSeq is never populated by the live apply path

All 5 initial relay blocks (slots 17-46) arrive at live tip via BlockFetch and are applied through
`apply_fetched_block` (`crates/dugite-node/src/node/mod.rs:3621`):

```rust
let mut ls = self.ledger_state.write().await;
if let Err(e) = ls.apply_block(&block, validation_mode) { ... }
```

`apply_block` advances the ledger state but does **not** call `apply_block_with_delta` and does
**not** push a `LedgerDelta` to `self.ledger_seq`. The 6 subsequent self-forged blocks (slots
50-79) also go through the same path. Result: after 11 applied blocks, `LedgerSeq` has
**0 deltas** — the volatile window is empty.

Compare with `process_blocks_bulk` (`crates/dugite-node/src/node/sync.rs:1146-1182`), which
correctly calls `apply_block_with_delta` and then pushes each delta to the seq.

### Step 2 — Fork rollback fails (empty seq, no snapshot)

When the relay's block at slot=79 (block_no=11) makes the relay chain strictly longer than
dugite's self-forged chain (block_no=10), `TriggeredFork` fires.
`apply_fetched_block` calls `handle_ledger_rollback(slot=46)`:

- **Fast path** (`sync.rs:311`): `rollback_via_seq` calls `LedgerSeq::find_rollback_n`. The
  seq has 0 deltas → returns `None`. Fast path falls through.
- **Slow path** (`sync.rs:353`): `find_best_snapshot_for_rollback` finds no snapshot (first run,
  no snapshot written yet). Returns `None`.
- **Error path** (`sync.rs:554`): logs ERROR "Rollback target outside LedgerSeq volatile window
  AND no canonical snapshot available" and **returns without modifying the ledger**.

The ledger stays at its pre-fork tip (slot=78, hash=`e21942...`).

### Step 3 — Fork replay fails, volatile cleared, StoreButDontChange cascade

Back in `apply_fetched_block` (TriggeredFork arm, `mod.rs:3448`), the code attempts to replay
the fork's `apply` list. The first block has `prev_hash = 1599a4...` (relay's slot-46 hash),
but the ledger tip is `e21942...` (dugite's slot-78 self-forged block). The apply fails:
"Block does not connect to tip: expected e21942... got 1599a4...".

On that failure (`mod.rs:3458-3460`):
```rust
let mut db = self.chain_db.write().await;
db.clear_volatile();
return;
```

VolatileDB is now empty. `selected_chain = []`.

Subsequently, dugite-bp continues forging its own blocks on top of the (still valid) ledger
state. Each self-forged block extends `selected_chain` from empty → [block_no=11, 12, ...].
Relay blocks arrive via BlockFetch and land in VolatileDB, but their ancestry traces back
through the relay chain's slot-79 block which was cleared. `switch_chain` walks the fork chain
backward via `prev_hash` and finds no shared block with `selected_chain`. Returns `None` →
`StoreButDontChange` — forever.

The ImmutableDB is empty (no blocks finalized yet), so the immutable-anchor fallback in
`switch_chain` also cannot match. Every relay block accumulates as a disconnected entry;
chain selection is permanently stuck.

---

## Fix

### Primary fix (Fix A): Push LedgerDelta in `apply_fetched_block`

**File:** `crates/dugite-node/src/node/mod.rs`
**Location:** The normal live single-block apply section (~line 3619-3642)

Change from `apply_block` to `apply_block_with_delta`, then push the delta to `self.ledger_seq`:

```rust
// Before (lines ~3619-3642):
{
    let mut ls = self.ledger_state.write().await;
    if let Err(e) = ls.apply_block(&block, validation_mode) {
        warn!(...);
        return;
    }
    // ... era transition handling ...
}

// After:
let delta = {
    let mut ls = self.ledger_state.write().await;
    match ls.apply_block_with_delta(&block, validation_mode) {
        Ok(delta) => {
            // era transition handling (same as before) ...
            delta
        }
        Err(e) => {
            warn!(...);
            return;
        }
    }
};
{
    let mut seq = self.ledger_seq.write().await;
    seq.push(delta);
}
```

**Lock order:** `ledger_state` write lock is acquired and RELEASED before `ledger_seq` write
lock is acquired. This is the same pattern already used in `process_blocks_bulk` (line 1177-1183)
and is consistent with the documented lock ordering invariant at the top of `sync.rs`.

This ensures every applied block — regardless of whether it came from bulk sync or live
BlockFetch — contributes a delta to LedgerSeq. When the next fork fires, `rollback_via_seq`
finds the intersection in the delta window and completes the rollback without needing a snapshot.

### Secondary fix (Fix B): Also push deltas in the TriggeredFork replay loop

**File:** `crates/dugite-node/src/node/mod.rs`
**Location:** Fork replay loop at ~line 3448 (inside `TriggeredFork` arm of `apply_fetched_block`)

The same `ls.apply_block()` pattern is used there. Each replayed fork block should also push a
delta; otherwise the LedgerSeq tip diverges from the ledger tip after every fork switch.

Same fix pattern: swap `apply_block` for `apply_block_with_delta` + push to seq.

### Defense-in-depth (Fix C): `handle_ledger_rollback` returns `bool`

**File:** `crates/dugite-node/src/node/sync.rs`

Change `handle_ledger_rollback` return type from `()` to `bool` (true = succeeded,
false = aborted). In `apply_fetched_block`, check the return value:

```rust
if !self.handle_ledger_rollback(&rollback_point).await {
    // Rollback failed — do NOT attempt fork replay (it will fail and corrupt state).
    // The node will resync on the next connection attempt.
    warn!("Fork rollback failed; skipping replay");
    // Optionally: trigger reconnect instead of clear_volatile
    return;
}
// Only reach here if rollback succeeded
for fork_hash in &apply { ... }
```

This prevents the "Block does not connect to tip" WARN and the `clear_volatile()` that destroys
all fork tracking state. Fix A makes Fix C rarely needed, but it's the correct safety net.

---

## Why minimal & correct

**Fix A** is the direct fix for the root cause. The live apply path is the only code path that
doesn't already push deltas. `process_blocks_bulk`, the fork replay path in the bulk loop, and
the forge `apply_fetched_block` variant all have bugs of the same class. Fix A aligns the live
path with the existing bulk-path pattern.

There are no new correctness concerns:
- `apply_block_with_delta` returns a `LedgerDelta` that already reflects the applied block's
  UTxO changes. Pushing it to LedgerSeq is idempotent with respect to the ledger state.
- LedgerSeq::push drops entries beyond k=2160 via `advance_anchor()`. This is the
  intentional design — same behavior as the bulk path.
- Lock ordering is preserved (ledger_state released before ledger_seq acquired).

**Fix C** is valuable independently of Fix A because it prevents a partially-executed fork replay
from clearing VolatileDB and entering the permanent StoreButDontChange cascade. Even if Fix A
is applied, Fix C adds resilience against any future rollback failure (e.g., deep historical
rollbacks, snapshot corruption).

Neither fix requires changes outside `mod.rs`/`sync.rs`, no new inter-crate interfaces.

---

## Haskell cross-reference

In Haskell's `cardano-node`, `LedgerDB.applyBlock` is called for every block regardless of sync
phase. The LedgerDB (equivalent to dugite's LedgerSeq) always has a delta for every applied
block. Haskell makes no distinction between "bulk sync" and "live tip" at the LedgerDB
accounting level — both paths ultimately call `applyBlock` through the same `LedgerDB.V2` API.

This is the invariant that dugite violates: the live single-block path uses `apply_block` which
does not produce a delta, creating a "shadow gap" in LedgerSeq that prevents the fast-path
rollback from working.

Haskell's `forkerRollback` always succeeds within k blocks because the LedgerDB is always
populated. Dugite's equivalent fails precisely because the live path never populated the seq.

---

## Test strategy

### Unit test reproducing the bug (fails before fix, passes after)

```
1. Build a Node with in-memory ChainDB, empty LedgerSeq (k=100)
2. Apply 5 blocks (relay chain, slots 17-46) via apply_fetched_block
3. Apply 6 blocks (self-forged chain, slots 50-79) via apply_fetched_block
4. Synthesize a TriggeredFork: fork tips at relay slot=79 (block_no=11)
   with intersection at relay slot=46, apply_list = [slots 51..79 on fork]
5. Assert: handle_ledger_rollback(slot=46) returns true (succeeds)
6. Assert: ledger.tip.point.slot() == 46
7. Assert: fork replay loop applies fork blocks, logs "Chain extended"
8. Assert: ledger.tip.point.slot() == 79 (fork chain tip)
9. Assert: LedgerSeq.tip_point().slot() == 79
```

### Integration test (devnet smoke run)

After applying Fix A + B + C:
```
./testnet/local-devnet/run.sh
# wait 3 minutes
grep "Chain extended" testnet/local-devnet/logs/dugite-bp.log | wc -l  # should be >> 5
grep "StoreButDontChange" testnet/local-devnet/logs/dugite-bp.log | wc -l  # should be 0
./testnet/local-devnet/verify.sh  # all 4 predicates pass
```

---

## Risks

1. **LedgerSeq capacity saturation**: At 1 block/2s and k=2160, saturation takes ~72 min.
   `advance_anchor()` trims gracefully. No new risk (same as bulk path already handles).

2. **Memory**: Each `LedgerDelta` carries UTxO diffs. For an empty devnet this is trivial;
   for mainnet at ~3 tx/block, each delta is O(tens of KB). Bounded by k entries.

3. **Lock contention**: Two write-lock acquisitions per block (ledger_state then ledger_seq).
   The bulk path already does this. Live tip processes ~1 block/2s, not a tight loop.

4. **Double-push invariant**: `apply_fetched_block` and `process_blocks_bulk` are structurally
   mutually exclusive (bulk sync runs to completion, then live sync starts). No risk of
   duplicate delta push. A comment in the code documenting this invariant is warranted.

5. **Fork replay deltas (Fix B)**: Blocks replayed in the TriggeredFork arm also need deltas.
   Without Fix B, after a successful fork switch the LedgerSeq tip is at the intersection
   point, not the fork tip. This creates a new "shadow gap" for the NEXT fork. Fix B is
   needed alongside Fix A for complete correctness.

---

## Estimated effort

| Component | Change | LoC estimate |
|-----------|--------|-------------|
| Fix A: live apply delta push | `mod.rs`, `apply_fetched_block` | ~15 |
| Fix B: fork replay delta push | `mod.rs`, TriggeredFork arm | ~10 |
| Fix C: rollback returns bool | `sync.rs` + call sites | ~15 |
| Unit test | New test or extend existing | ~80 |
| **Total** | 3 files | **~120** |

Estimated wall-clock: 2-3 hours including test + devnet verification.
