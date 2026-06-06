---
name: opcert-counter-stale-after-replay
description: Stale OuroborosPraos.opcert_counters after replay causes false CounterOverIncrementedOCERT → InvalidBlockCache chain-poison → permanent wedge
metadata:
  type: project
---

## Bug: Stale opcert counters after replay wedge live sync

**Symptom:** Node wedges at first live Praos block from a pool whose opcert counter was incremented during the replay window. `CounterOverIncrementedOCERT` fires (e.g., `got=31, last_seen=29`), the block enters `InvalidBlockCache`, and every downstream block in the chain is refused as `StoreButDontChange`.

**Observed:** preprod block 718744 (slot 22975227, epoch 56 Babbage), commit `3c3b82cc0`.

**Root cause:** During both chunk-file and LSM replays, blocks are applied with `BlockValidationMode::ApplyOnly`. This skips `validate_header_full` and therefore never calls `check_opcert_counter` on `self.consensus` (OuroborosPraos validator). `ls.consensus.opcert_counters` (LedgerState) IS updated via `compute_shelley_nonce` per block. But `self.consensus.opcert_counters` stays frozen at the snapshot value until `set_strict_verification(true)` fires.

**Fix location:** `crates/dugite-node/src/node/mod.rs`, immediately before `set_strict_verification(true)`:
```rust
// After replay_ledger_from_storage() returns, merge ls.consensus.opcert_counters
// (updated per-block via compute_shelley_nonce) into self.consensus.opcert_counters.
// Per-pool max semantics, same as merge_opcert_counters_from_praos.
let ls = self.ledger_state.read().await;
let mut merged = self.consensus.opcert_counters().clone();
for (pool_id, &ledger_seq) in &ls.consensus.opcert_counters {
    merged.entry(*pool_id).and_modify(|cur| { if ledger_seq > *cur { *cur = ledger_seq; } }).or_insert(ledger_seq);
}
self.consensus.set_opcert_counters(merged);
```

**Why:** The `merge_opcert_counters_from_praos` call in `replay_from_lsm` goes the WRONG direction for this purpose (it merges PraosValidator → LedgerState). We need the reverse: LedgerState → PraosValidator.

**How to apply:** Any time replay is extended or a new replay path is added, ensure opcert counters are reseeded from the post-replay ledger before strict verification is enabled. The 2-source design (ledger tracks per-apply, validator tracks per-strict-validation) requires this synchronization step at the replay→live transition.
