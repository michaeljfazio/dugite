---
name: mempool-mined-cascade-fix
description: Mined parent tx must NOT cascade-evict children — they resolve against on-chain UTxO after apply
metadata:
  type: project
---

## Fix: skip cascade eviction when parent tx is MempoolRemoveReason::Mined

**Commit**: `bbdcb67a1` — fix(mempool): skip cascade eviction when parent tx is Mined

### Root Cause

`remove_tx_inner` ran an unconditional BFS cascade for ALL removal reasons
(Mined, Evicted, Manual). When tx_2 was mined in a block and removed with
`Mined`, the cascade evicted tx_3 (which spent tx_2's virtual output). But
tx_2's real output was ALREADY in the ledger `utxo_set` (apply_block_with_delta
ran before post_block_apply_updates). The cascade made tx_3 permanently lost.

### Why the race window was small but real

The ordering in `try_forge_block_at` / `apply_fetched_block` is:
1. `apply_block_with_delta` → tx_2 outputs enter `ledger.utxo.utxo_set`
2. `post_block_apply_updates` → `remove_txs_with_reason([tx_2], Mined)` → cascade ran → tx_3 evicted
3. `revalidate_all` → tx_3 already gone, no-op

The TxSubmission2 server delivers tx_3 AFTER the forge+apply cycle completes
(separate blocking round-trip). At step 2, tx_3 is evicted from the mempool
BEFORE the N2N path has a chance to re-admit it via the virtual UTxO.

### Fix (crates/dugite-mempool/src/lib.rs)

In `remove_tx_inner`: when `reason == MempoolRemoveReason::Mined`, clean up
only the `dependents` map edge and return early — do NOT cascade children.
Children remain valid because their inputs resolve against `utxo_set`.

For Evicted / Manual: cascade still fires (outputs are permanently gone).

### Key invariant

`apply_block_with_delta` MUST run BEFORE `post_block_apply_updates` and
therefore before `remove_txs_with_reason(Mined)`. If this order is ever
reversed, the on-chain UTxO guarantee breaks and children would be stranded.
The existing code already enforces this (forge path line 6522 → 6613;
live-tip path line 4588 → 4684).

### Regression tests added
- `test_mined_parent_child_survives`
- `test_mined_chain_three_txs_middle_and_tail_survive`  
- `test_evicted_parent_still_cascades` (confirms Evicted still cascades)
- `tx_events_mined_parent_does_not_cascade_child` (updated from old broken test)

**Why:** `feedback_haskell_byte_exact_only` — dugite extends Haskell with
chained-tx support; this fix makes that extension work correctly end-to-end.
