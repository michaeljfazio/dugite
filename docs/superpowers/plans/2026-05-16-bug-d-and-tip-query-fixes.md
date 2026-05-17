# Bug D + Tip-Query Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two outstanding dugite-node defects so the local-devnet 30-minute soak passes all four verify.sh predicates with no "excluding dugite-bp" workaround:
1. **Bug D (issue #497)** — chain selection rejects equal-block_no peer chains because `process_add_block` uses a strict-greater filter that ignores Praos's `comparePraos` tiebreaker (OCert sequence number + `RestrictedVRFTiebreaker 5` VRF compare).
2. **Tip-query staleness** — `try_forge_block_at` omits the metric and `update_query_state` calls that `apply_fetched_block` makes, so own-forged blocks never advance the Prometheus gauges or the N2C `NodeStateSnapshot`.

**Architecture:**
- Bug D: Wire the existing `dugite_consensus::chain_selection::prefer_chain_with_headers` into `chain_sel_queue::process_add_block`. The comparator already implements Haskell's exact algorithm and is tested in `chain_selection.rs`; we just need to call it. To preserve the existing test contract (which uses `fake_cbor` = hash bytes), extend the storage API with an opt-in `submit_block_with_header(... , Option<BlockHeader>)`. When all relevant headers (new block + current tip + fork tip) are `Some`, run the Praos comparator; otherwise fall back to the legacy strict-greater filter. Storage caches headers in a side-map keyed by hash.
- Tip-query: Extract a `Node::post_block_apply_updates(&mut self, block, slot, block_no)` helper from the existing post-apply housekeeping in `apply_fetched_block` (mod.rs:3747-3857). Call it from both `apply_fetched_block` AND `try_forge_block_at` so the forge path also advances Prometheus gauges, refreshes the N2C snapshot (rate-limited 1 Hz), and sweeps the mempool.

**Tech Stack:** Rust 2021, Tokio, dugite workspace crates (storage, consensus, node, primitives, serialization).

---

## Pre-flight

You are working inside the existing isolated worktree at
`/Users/michaelfazio/Source/dugite/.worktrees/local-testnet-docs` on branch
`feature/local-testnet-docs`. The branch already contains the Bugs A/B/C fixes
(commits `7e6a4af54`, `59a5fc64d`, `9d30beaf2`) and the two design specs you
will implement (commit `aaf045b02`):

- `docs/superpowers/specs/2026-05-16-bug-d-chain-selection-fix.md`
- `docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md`

Re-read both specs before starting. They contain the design rationale,
Haskell cross-references, and risk analysis that this plan operationalizes.

All commands assume you are at the worktree root unless stated otherwise.

---

## Phase 1 — Bug D: chain-selection comparator

### Task 1: Add `dugite-consensus` as a dep of `dugite-storage`

**Files:**
- Modify: `crates/dugite-storage/Cargo.toml`

- [ ] **Step 1.1: Add the dependency line**

Open `crates/dugite-storage/Cargo.toml` and find the `[dependencies]` block.
Append a single line:

```toml
[dependencies]
dugite-primitives = { workspace = true }
dugite-serialization = { workspace = true }
dugite-crypto = { workspace = true }
dugite-consensus = { workspace = true }   # NEW: for prefer_chain_with_headers (Bug D fix)
tokio = { workspace = true }
# ... existing dependencies unchanged ...
```

- [ ] **Step 1.2: Verify the crate compiles with the new dep**

Run: `cargo check -p dugite-storage`
Expected: no errors, no new warnings.

This task does not commit; the dep is exercised by Task 4.

---

### Task 2: Cache headers in VolatileDB

The Praos comparator needs `BlockHeader` data for the current tip, every fork
tip, and the incoming block. The simplest minimal change is a side-map on
`VolatileDB`, populated by a new `add_block_with_header` method. The existing
`add_block` stays untouched so existing callers behave identically (header
will be `None`, comparator falls back to strict-greater).

**Files:**
- Modify: `crates/dugite-storage/src/volatile_db.rs` (struct + new method + getter)

- [ ] **Step 2.1: Add the headers cache field to VolatileDB**

Find the `pub struct VolatileDB { ... }` declaration (use grep:
`grep -n "pub struct VolatileDB" crates/dugite-storage/src/volatile_db.rs`).

Add this field (alongside `blocks`, `successors`, etc.):

```rust
    /// Cached block headers, populated only by `add_block_with_header`.
    ///
    /// Used by `ChainSelQueue::process_add_block` to run the Haskell
    /// `comparePraos` tiebreaker (issue #497).  When a block has no cached
    /// header (Byron, legacy callers, tests with synthetic CBOR) the
    /// comparator falls back to the strict-greater block_no rule, preserving
    /// existing behavior.
    headers: std::collections::HashMap<Hash32, dugite_primitives::block::BlockHeader>,
```

Initialize the field in every `VolatileDB::new` / `default` / similar
constructor in this file. There are typically 1-2; add `headers: HashMap::new(),`
to each.

- [ ] **Step 2.2: Add `add_block_with_header` method**

Right after the existing `add_block` method (around line 606), add:

```rust
    /// Variant of [`add_block`] that also caches the block's `BlockHeader`
    /// for later use by the chain-selection Praos tiebreaker.
    ///
    /// Bug D (issue #497): when an incoming block carries header info, we
    /// stash it so that `ChainSelQueue::process_add_block` can call
    /// `dugite_consensus::ChainSelection::prefer_chain_with_headers` instead
    /// of the strict-greater fallback.  Behaves identically to `add_block`
    /// for storage purposes.
    pub fn add_block_with_header(
        &mut self,
        hash: Hash32,
        slot: u64,
        block_no: u64,
        prev_hash: Hash32,
        cbor: Vec<u8>,
        header: dugite_primitives::block::BlockHeader,
    ) -> bool {
        self.headers.insert(hash, header);
        // Reuse the existing WAL + insert path so byte-for-byte storage
        // behavior is identical to `add_block`.
        self.add_block(hash, slot, block_no, prev_hash, cbor)
    }

    /// Look up a previously cached header (Bug D Praos tiebreaker).
    ///
    /// Returns `None` for any block stored via the legacy `add_block` path,
    /// for ImmutableDB blocks, and for blocks that have been GC'd.
    pub fn get_header(
        &self,
        hash: &Hash32,
    ) -> Option<&dugite_primitives::block::BlockHeader> {
        self.headers.get(hash)
    }
```

- [ ] **Step 2.3: Drop cached headers when GC removes blocks**

Find `VolatileDB::collect_garbage` (`grep -n "fn collect_garbage" crates/dugite-storage/src/volatile_db.rs`).
Inside the loop that removes entries from `self.blocks`, mirror the removal:

Look for the line like `self.blocks.remove(&h);` inside the GC loop, and add
immediately after it:

```rust
                self.headers.remove(&h);
```

If the existing code uses `retain` or similar instead of `remove`, mirror that
same pattern on `self.headers` (`self.headers.retain(|h, _| !to_remove.contains(h));`).

- [ ] **Step 2.4: Build the crate, run the volatile_db unit tests**

Run: `cargo nextest run -p dugite-storage -E 'test(volatile_db)'`
Expected: all existing volatile_db tests pass; no new test failures.
If a constructor was missed in Step 2.1, you will see "missing field `headers`"
— add it.

Run: `cargo clippy -p dugite-storage --all-targets -- -D warnings`
Expected: clean.

This task does not commit; the cache becomes useful in Task 4.

---

### Task 3: Add `submit_block_with_header` and the message variant

**Files:**
- Modify: `crates/dugite-storage/src/chain_sel_queue.rs` (message enum, handle method, runner dispatch)

- [ ] **Step 3.1: Add the header field to ChainSelMessage::AddBlock**

Find `pub enum ChainSelMessage` (line 51). Add a new optional field to the
`AddBlock` variant:

```rust
pub enum ChainSelMessage {
    AddBlock {
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
        /// Block header for the Praos chain-selection tiebreaker (Bug D, #497).
        /// `None` for legacy callers and Byron blocks; comparator falls back to
        /// strict-greater block_no in that case.
        header: Option<dugite_primitives::block::BlockHeader>,
        result_tx: oneshot::Sender<AddBlockResult>,
    },
}
```

- [ ] **Step 3.2: Add `submit_block_with_header` to ChainSelHandle**

After the existing `submit_block` method (line 545+), add:

```rust
    /// Variant of [`submit_block`] that also forwards the block's
    /// `BlockHeader` so chain selection can run the Praos tiebreaker
    /// (Bug D / issue #497).
    ///
    /// Production callers (live BlockFetch path, forge path) should call this
    /// method.  The legacy [`submit_block`] is retained as a thin wrapper for
    /// tests and any code that cannot easily obtain a header.
    pub async fn submit_block_with_header(
        &self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
        header: dugite_primitives::block::BlockHeader,
    ) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();

        self.tx
            .send(ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header: Some(header),
                result_tx,
            })
            .await
            .ok()?;

        result_rx.await.ok()
    }
```

Update the legacy `submit_block` method body to send `header: None`:

```rust
    pub async fn submit_block(
        &self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
    ) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();

        self.tx
            .send(ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header: None,        // NEW: legacy path, no Praos tiebreak
                result_tx,
            })
            .await
            .ok()?;

        result_rx.await.ok()
    }
```

- [ ] **Step 3.3: Forward `header` through the runner**

In `add_block_runner` (line 274), update the destructuring pattern (around
line 283) to include the new field, and forward it to `process_add_block`:

```rust
            ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header,           // NEW
                result_tx,
            } => {
                let result = process_add_block(
                    &hash,
                    slot,
                    block_no,
                    prev_hash,
                    cbor,
                    header.as_ref(),   // NEW: pass by reference
                    &chain_db,
                    &invalid_cache,
                )
```

- [ ] **Step 3.4: Update `process_add_block` signature**

In `process_add_block` (line 325), add the new parameter:

```rust
async fn process_add_block(
    hash: &BlockHeaderHash,
    slot: SlotNo,
    block_no: BlockNo,
    prev_hash: BlockHeaderHash,
    cbor: Vec<u8>,
    header: Option<&dugite_primitives::block::BlockHeader>,   // NEW
    chain_db: &Arc<RwLock<ChainDB>>,
    invalid_cache: &Arc<RwLock<InvalidBlockCache>>,
) -> AddBlockResult {
```

Inside the function body, find the existing `db.add_block(...)` call in step 3
(around line 361):

```rust
        let mut db = chain_db.write().await;
        match db.add_block(hash.to_owned(), slot, block_no, prev_hash, cbor) {
```

Replace the call so that when a header is available the cache is populated:

```rust
        let mut db = chain_db.write().await;
        let add_result = match header {
            Some(h) => db.add_block_with_header(
                hash.to_owned(),
                slot,
                block_no,
                prev_hash,
                cbor,
                h.clone(),
            ),
            None => db.add_block(hash.to_owned(), slot, block_no, prev_hash, cbor),
        };
        match add_result {
```

(NOTE: We also need to add `add_block_with_header` on `ChainDB` — Step 3.5.)

- [ ] **Step 3.5: Mirror `add_block_with_header` on ChainDB**

`ChainDB::add_block` (chain_db.rs:198) is a thin wrapper over
`VolatileDB::add_block`. Add the same wrapper for the new method.

Open `crates/dugite-storage/src/chain_db.rs` and add after `add_block`:

```rust
    /// Variant of [`add_block`] that also caches the block's header in
    /// VolatileDB for the Praos chain-selection tiebreaker (Bug D, #497).
    pub fn add_block_with_header(
        &mut self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
        header: dugite_primitives::block::BlockHeader,
    ) -> Result<bool, ChainDBError> {
        if self.has_block(&hash) {
            return Ok(false);
        }
        let extended = self.volatile.add_block_with_header(
            hash, slot.0, block_no.0, prev_hash, cbor, header,
        );
        Ok(extended)
    }

    /// Look up a previously cached VolatileDB header.
    pub fn get_volatile_header(
        &self,
        hash: &BlockHeaderHash,
    ) -> Option<&dugite_primitives::block::BlockHeader> {
        self.volatile.get_header(hash)
    }
```

- [ ] **Step 3.6: Build the crate**

Run: `cargo check -p dugite-storage`
Expected: clean. If the `header.clone()` complains about borrow lifetimes,
ensure you pass `header.cloned()` instead — but it should work as written.

This task does not commit; chain-selection is updated next.

---

### Task 4: Wire `prefer_chain_with_headers` into `process_add_block`

This is the heart of the fix.

**Files:**
- Modify: `crates/dugite-storage/src/chain_sel_queue.rs` (replace best_fork
  selection in `process_add_block`)

- [ ] **Step 4.1: Add the required imports at the top of the file**

At the top of `chain_sel_queue.rs`, find the existing `use ...` block and add:

```rust
use dugite_consensus::chain_selection::{ChainPreference, ChainSelection};
use dugite_primitives::block::Tip;
use dugite_primitives::era::Era;
```

- [ ] **Step 4.2: Replace the best_fork selection logic**

Find the existing chain-selection block in `process_add_block` (lines 396-450).
Replace it with the following:

```rust
    // --- Step 4: Chain selection (Haskell `chainSelectionForBlock`) ---------
    //
    // Per Haskell `Ouroboros.Consensus.Protocol.Praos.Common::comparePraos`:
    //
    // - block_no strictly greater  → switch (longest-chain rule).
    // - block_no equal             → run Praos tiebreaker:
    //                                  same slot + same issuer → compare OCert
    //                                  seq number; otherwise compare VRF output
    //                                  bytes if within RestrictedVRFTiebreaker
    //                                  window (Conway: 5 slots), else no switch.
    // - block_no strictly less     → no switch.
    //
    // dugite-consensus implements this in `ChainSelection::prefer_chain_with_headers`.
    // We call it when ALL relevant headers (new block, current tip, candidate)
    // are present; otherwise fall back to the legacy strict-greater rule so
    // tests using synthetic CBOR keep their existing semantics (Bug D, #497).
    {
        let mut db = chain_db.write().await;

        let current_tip_info = db.get_tip_info();
        let current_tip_block_no: u64 = current_tip_info
            .as_ref()
            .map(|(_slot, _hash, bn)| bn.0)
            .unwrap_or(0);

        // For the Praos tiebreaker we need: (a) the current tip's BlockHeader,
        // (b) each fork-tip's BlockHeader.  All come from the in-memory cache
        // populated by `add_block_with_header`.
        let current_tip_header = current_tip_info
            .as_ref()
            .and_then(|(_, h, _)| db.get_volatile_header(h).cloned());

        let fork_tips = db.get_all_fork_tips();

        // Helper: pick the best fork using the Praos comparator.  Returns the
        // (hash, block_no, slot) of the preferred candidate, or None.
        fn select_best_praos<'a>(
            fork_tips: Vec<(Hash32, BlockNo, SlotNo)>,
            current_header: &dugite_primitives::block::BlockHeader,
            db: &ChainDB,
        ) -> Option<(Hash32, BlockNo, SlotNo)> {
            // Era for the slot-window decision: use the current tip's era,
            // which always matches the candidate's era within a 5-slot
            // tiebreaker window.
            let era = current_header.protocol_version.era();
            let slot_window: u64 = match era {
                Era::Conway | Era::Dijkstra => 5, // RestrictedVRFTiebreaker 5
                Era::Byron => 0,                  // no tiebreaker (handled below)
                _ => u64::MAX,                    // pre-Conway Praos: unrestricted
            };

            let mut sel = ChainSelection::new();
            sel.set_tip(Tip {
                point: dugite_primitives::block::Point::Specific(
                    current_header.slot,
                    current_header.header_hash,
                ),
                block_number: current_header.block_number,
            });

            fork_tips
                .into_iter()
                .filter_map(|(h, bn, slot)| {
                    let cand_header = db.get_volatile_header(&h)?.clone();
                    let cand_tip = Tip {
                        point: dugite_primitives::block::Point::Specific(
                            cand_header.slot,
                            cand_header.header_hash,
                        ),
                        block_number: cand_header.block_number,
                    };
                    let pref = sel.prefer_chain_with_headers(
                        &cand_tip,
                        current_header,
                        &cand_header,
                        era,
                        slot_window,
                    );
                    if matches!(pref, ChainPreference::PreferCandidate) {
                        Some((h, bn, slot))
                    } else {
                        None
                    }
                })
                // Among preferred candidates, prefer highest block_no.
                .max_by_key(|(_, bn, _)| bn.0)
        }

        // Legacy fallback for callers that did not pass a header (or where
        // some required header is missing).  This preserves the strict-greater
        // semantics used by the older chain_sel_queue tests.
        fn select_best_legacy(
            fork_tips: Vec<(Hash32, BlockNo, SlotNo)>,
            current_tip_block_no: u64,
        ) -> Option<(Hash32, BlockNo, SlotNo)> {
            fork_tips
                .into_iter()
                .filter(|(_h, bn, _slot)| bn.0 > current_tip_block_no)
                .max_by_key(|(_h, bn, _slot)| bn.0)
        }

        let best_fork = match (header, current_tip_header.as_ref()) {
            (Some(_new_h), Some(cur_h)) => {
                // Praos path: at least one fork-tip header must also be
                // present in the cache; otherwise individual candidates with
                // missing headers are silently excluded by the filter_map
                // (acceptable: a missing-header candidate would also be
                // skipped by the legacy strict-greater filter when block_no
                // ties).
                select_best_praos(fork_tips, cur_h, &db)
                    .or_else(|| select_best_legacy(db.get_all_fork_tips(), current_tip_block_no))
            }
            _ => select_best_legacy(fork_tips, current_tip_block_no),
        };

        if let Some((fork_hash, fork_bn, fork_slot)) = best_fork {
            info!(
                fork_hash = %fork_hash.to_hex(),
                fork_block_no = fork_bn.0,
                fork_slot = fork_slot.0,
                current_tip_block_no,
                "chain_sel: switching to longer fork"
            );

            if let Some(plan) = db.switch_to_fork(&fork_hash) {
                return AddBlockResult::TriggeredFork {
                    intersection_hash: plan.intersection,
                    intersection_slot: SlotNo(plan.intersection_slot),
                    rollback: plan.rollback,
                    apply: plan.apply,
                };
            }
            warn!(
                fork_hash = %fork_hash.to_hex(),
                fork_block_no = fork_bn.0,
                fork_slot = fork_slot.0,
                current_tip_block_no,
                "chain_sel: fork unreachable — StoreButDontChange"
            );
        }
    }
```

(The rest of `process_add_block` — the `if extended_tip { return AddedAsTip }`
and the final `StoredAsFork` return — is unchanged.)

- [ ] **Step 4.3: Build the crate, run existing chain_sel_queue tests**

Run: `cargo nextest run -p dugite-storage -E 'test(chain_sel_queue)'`
Expected: all 8 existing chain_sel_queue tests pass (they pass `None` for the
header via `submit_block`, so the legacy strict-greater fallback runs and
behavior is identical).

Run: `cargo nextest run -p dugite-storage`
Expected: all existing dugite-storage tests pass.

If a test fails:
- For `test_chain_selection_no_switch_equal_length`: confirm the fallback
  path is hit (the test uses `submit_block`, not `submit_block_with_header`).
- For new compile errors: ensure the imports at Step 4.1 were added.

This task does not commit; tests are added in Task 6.

---

### Task 5: Update production callers to pass headers

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs` (3 call sites)

- [ ] **Step 5.1: Update `apply_fetched_block` to pass the header**

Find the `submit_block` call inside `apply_fetched_block` (around line 3387):

```rust
            let result = handle
                .submit_block(
                    block_hash,
                    block_slot,
                    block_number,
                    *block.prev_hash(),
                    cbor,
                )
                .await;
```

Replace with `submit_block_with_header`:

```rust
            let result = handle
                .submit_block_with_header(
                    block_hash,
                    block_slot,
                    block_number,
                    *block.prev_hash(),
                    cbor,
                    block.header.clone(),
                )
                .await;
```

- [ ] **Step 5.2: Update `try_forge_block_at` (forge happy path)**

Find the `submit_block` call inside `try_forge_block_at` (around line 5088):

```rust
                let chain_sel_verdict = if let Some(ref handle) = self.chain_sel_handle {
                    handle
                        .submit_block(
                            *block.hash(),
                            block.slot(),
                            block.block_number(),
                            *block.prev_hash(),
                            cbor,
                        )
                        .await
                } else {
```

Replace the `submit_block` call only:

```rust
                let chain_sel_verdict = if let Some(ref handle) = self.chain_sel_handle {
                    handle
                        .submit_block_with_header(
                            *block.hash(),
                            block.slot(),
                            block.block_number(),
                            *block.prev_hash(),
                            cbor,
                            block.header.clone(),
                        )
                        .await
                } else {
```

(The `else { ... }` branch uses `db.add_block(...)` directly; leave that
unchanged. It is the fallback for nodes without a ChainSelHandle.)

- [ ] **Step 5.3: Build the node, run a wide test sweep**

Run: `cargo check -p dugite-node`
Expected: clean.

Run: `cargo nextest run -p dugite-node -E 'test(apply_fetched) or test(forge) or test(chain_sel)'`
Expected: all targeted tests pass.

Run: `cargo nextest run -p dugite-node --release`
Expected: all 673+ tests pass.

This task does not commit; regression tests are added in Task 6.

---

### Task 6: Regression tests for the Praos tiebreaker

**Files:**
- Modify: `crates/dugite-storage/src/chain_sel_queue.rs` (add a tests
  sub-module + 3 new tests)

The existing tests in `chain_sel_queue.rs` use `fake_cbor` (just hash bytes)
and `submit_block` — they exercise only the legacy strict-greater fallback.
The new tests use `submit_block_with_header` with synthesized `BlockHeader`s,
so they exercise the Praos tiebreaker. The header-construction helper mirrors
the one already used in `dugite-consensus/src/chain_selection.rs` tests.

- [ ] **Step 6.1: Add a `praos_tiebreaker` sub-module at the bottom of the existing `tests` module**

Find the closing `}` of the existing `mod tests {` block (the one starting at
line 575). Just before that closing brace, paste the following sub-module:

```rust
    // -----------------------------------------------------------------------
    // Bug D (issue #497) — Praos tiebreaker regression tests
    // -----------------------------------------------------------------------
    //
    // The legacy tests in this module pass `None` for the header via
    // `submit_block`, so they exercise the strict-greater fallback unchanged.
    // The tests below pass synthesized `BlockHeader`s via the new
    // `submit_block_with_header` method, so they exercise the Praos
    // `comparePraos` tiebreaker imported from dugite-consensus.

    use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::time::{BlockNo as PrimBlockNo, SlotNo as PrimSlotNo};

    /// Construct a minimal BlockHeader for tiebreaker tests.
    ///
    /// `issuer_vkey` determines the pool ID (blake2b-224 of these bytes).
    /// `vrf_output` is the VRF output bytes used by the Praos cross-pool
    /// tiebreaker — lower lex wins.
    /// `opcert_seq` is the operational certificate sequence number — used by
    /// the same-pool-same-slot tiebreaker.
    /// `protocol_major` selects the era: 9..=11 → Conway (5-slot window),
    /// 12+ → Dijkstra, 7..=8 → Babbage (unrestricted), etc.
    fn praos_header(
        hash_bytes: [u8; 32],
        prev_hash_bytes: [u8; 32],
        slot: u64,
        block_no: u64,
        issuer_vkey: Vec<u8>,
        opcert_seq: u64,
        vrf_output: Vec<u8>,
        protocol_major: u64,
    ) -> BlockHeader {
        BlockHeader {
            header_hash: Hash32::from_bytes(hash_bytes),
            prev_hash: Hash32::from_bytes(prev_hash_bytes),
            issuer_vkey,
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vrf_output,
                proof: vec![],
            },
            block_number: PrimBlockNo(block_no),
            slot: PrimSlotNo(slot),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: opcert_seq,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion { major: protocol_major, minor: 0 },
            kes_signature: vec![],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
        }
    }

    /// Bug D regression: equal block_no + lower VRF on candidate + within the
    /// 5-slot Conway window → MUST trigger a fork switch.
    ///
    /// Mirrors the local-devnet scenario where two BPs slot-battle at f=0.2
    /// and never converged under the old strict-greater filter.
    #[tokio::test]
    async fn praos_tiebreaker_switches_on_equal_block_no_lower_vrf_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Common parent at slot 100, block_no 1 (Conway era, protocol_major 9).
        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes,
            [0u8; 32],
            100,
            1,
            vec![0xAA; 32],
            0,
            vec![0xFF; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .expect("runner alive");

        // Current tip: pool A forges at slot 110, block_no 2, vrf=0xFF.
        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes,
            common_bytes,
            110,
            2,
            vec![0xAA; 32], // pool A vkey
            1,
            vec![0xFFu8; 32], // high VRF
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .expect("runner alive");
        assert!(matches!(r, AddBlockResult::AddedAsTip { .. }), "a: {r:?}");

        // Candidate: pool B forges at slot 112 (within 5-slot window),
        // block_no 2 (same as A), vrf=0x00 (strictly lower than A's 0xFF).
        // Praos tiebreaker: lower VRF wins → MUST switch to B.
        let b_bytes = [0xB2u8; 32];
        let b_header = praos_header(
            b_bytes,
            common_bytes,
            112,
            2,
            vec![0xBB; 32], // pool B vkey (different from A)
            1,
            vec![0x00u8; 32], // low VRF
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b_bytes),
                SlotNo(112),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b_bytes)),
                b_header,
            )
            .await
            .expect("runner alive");

        match r {
            AddBlockResult::TriggeredFork { .. } => {} // OK
            other => panic!(
                "expected TriggeredFork (Praos tiebreaker should switch on lower VRF \
                 within 5-slot window), got: {other:?}"
            ),
        }
    }

    /// Bug D regression: equal block_no + lower VRF on candidate but OUTSIDE
    /// the 5-slot Conway window → MUST NOT switch (RestrictedVRFTiebreaker).
    #[tokio::test]
    async fn praos_tiebreaker_does_not_switch_when_slot_gap_exceeds_window() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Same setup as above, but candidate slot is 120 vs current tip slot
        // 110 (gap of 10, exceeds the Conway window of 5).
        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes, [0u8; 32], 100, 1, vec![0xAA; 32], 0, vec![0xFF; 32], 9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .unwrap();

        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes, common_bytes, 110, 2, vec![0xAA; 32], 1, vec![0xFFu8; 32], 9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .unwrap();

        // Candidate B at slot 120 (>5 from A's slot 110), block_no 2, lower VRF.
        let b_bytes = [0xB2u8; 32];
        let b_header = praos_header(
            b_bytes, common_bytes, 120, 2, vec![0xBB; 32], 1, vec![0x00u8; 32], 9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b_bytes),
                SlotNo(120),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b_bytes)),
                b_header,
            )
            .await
            .unwrap();

        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "expected StoredAsFork: slot gap {} > Conway window {}, RestrictedVRFTiebreaker \
             must keep current selection",
            10, 5
        );
    }

    /// Sanity regression: strictly-greater block_no still triggers a switch
    /// when full headers are present. (The existing strict-greater test uses
    /// `submit_block`; this twin confirms the Praos path agrees.)
    #[tokio::test]
    async fn praos_tiebreaker_switches_on_strictly_greater_block_no() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes, [0u8; 32], 100, 1, vec![0xAA; 32], 0, vec![0xFF; 32], 9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .unwrap();

        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes, common_bytes, 110, 2, vec![0xAA; 32], 1, vec![0xFFu8; 32], 9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .unwrap();

        // Sibling fork: block_no 2 at same parent.  Then extend to block_no 3.
        let b2_bytes = [0xB2u8; 32];
        let b2_header = praos_header(
            b2_bytes, common_bytes, 115, 2, vec![0xBB; 32], 1, vec![0x77; 32], 9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(b2_bytes),
                SlotNo(115),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b2_bytes)),
                b2_header,
            )
            .await
            .unwrap();

        let b3_bytes = [0xB3u8; 32];
        let b3_header = praos_header(
            b3_bytes, b2_bytes, 117, 3, vec![0xBB; 32], 2, vec![0x88; 32], 9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b3_bytes),
                SlotNo(117),
                BlockNo(3),
                Hash32::from_bytes(b2_bytes),
                fake_cbor(&Hash32::from_bytes(b3_bytes)),
                b3_header,
            )
            .await
            .unwrap();

        match r {
            AddBlockResult::TriggeredFork { .. } => {} // OK
            other => panic!(
                "strictly greater block_no MUST trigger switch under both legacy and Praos rules, got: {other:?}"
            ),
        }
    }
```

- [ ] **Step 6.2: Run the new tests in isolation**

Run: `cargo nextest run -p dugite-storage -E 'test(praos_tiebreaker)'`
Expected: 3 tests pass.

If `praos_tiebreaker_switches_on_equal_block_no_lower_vrf_in_window` fails:
The most likely cause is that `select_best_praos` is being skipped because
the current_tip_header lookup returned None — confirm that
`add_block_with_header` cached the `a` header (Task 2). Add a `dbg!()` for
`current_tip_header.is_some()` in Step 4.2's match arm to verify.

If the OUTSIDE-window test triggers a switch unexpectedly:
Check that the `slot_window: u64 = 5` arm matches `Era::Conway`. The
`protocol_major: 9` argument to `praos_header` should map to Conway via
`ProtocolVersion::era()`.

- [ ] **Step 6.3: Run all dugite-storage tests**

Run: `cargo nextest run -p dugite-storage`
Expected: all tests pass (existing 8 + new 3 = 11 in chain_sel_queue, plus
the rest of the storage suite unchanged).

- [ ] **Step 6.4: Workspace clippy + fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean (or only pre-existing warnings unrelated to this change).

Run: `cargo fmt --all -- --check`
Expected: clean. If it complains, run `cargo fmt --all` and re-check.

- [ ] **Step 6.5: Commit Phase 1**

Stage the modified files and commit:

```bash
git add crates/dugite-storage/Cargo.toml \
        crates/dugite-storage/src/volatile_db.rs \
        crates/dugite-storage/src/chain_db.rs \
        crates/dugite-storage/src/chain_sel_queue.rs \
        crates/dugite-node/src/node/mod.rs
git commit -m "$(cat <<'EOF'
fix(node): Bug D — chain selection now runs Praos tiebreaker on equal block_no

`chain_sel_queue::process_add_block` previously filtered fork tips with
`bn > current_tip_block_no` strictly.  Haskell's `preferCandidate` is NOT
strict — it delegates equal-block_no decisions to `comparePraos`, which uses
operational-cert sequence number for same-slot-same-issuer collisions and
the `RestrictedVRFTiebreaker 5` VRF compare for cross-pool collisions within
a 5-slot window (Conway).  On the local-devnet, two BPs at f=0.2 sibling-fork
at equal block_no on virtually every block — once dugite-bp's own forges
keep pace with the peer's chain, the strict filter rejects every peer block
and the two chains permanently diverge (issue #497).

The `prefer_chain_with_headers` comparator already exists in
`dugite-consensus/chain_selection.rs:90` with 24 unit tests — it just had
never been wired into the live chain-selection path.  This patch:

  - Adds `dugite-consensus` as a dep of `dugite-storage`.
  - Caches block headers in `VolatileDB.headers` (only when the new
    `add_block_with_header` API is used; legacy `add_block` is unchanged).
  - Adds `ChainSelHandle::submit_block_with_header` and threads
    `Option<BlockHeader>` through `ChainSelMessage::AddBlock` to
    `process_add_block`.
  - `process_add_block` calls `ChainSelection::prefer_chain_with_headers`
    when all relevant headers are available; falls back to the legacy
    strict-greater rule when any header is missing (preserves the existing
    `fake_cbor`-based unit tests).
  - Production callers `apply_fetched_block` and `try_forge_block_at`
    switch to `submit_block_with_header` so the Praos path is always active.

3 new regression tests cover:
  - equal block_no + lower VRF + within 5-slot window → switch
  - equal block_no + lower VRF + slot gap >5 → no switch
  - strictly greater block_no still triggers a switch under the new path

Cannot regress Bugs A/B/C (different code paths).  Design doc:
docs/superpowers/specs/2026-05-16-bug-d-chain-selection-fix.md

Closes #497.
EOF
)"
```

---

## Phase 2 — Tip-query staleness

### Task 7: Extract `post_block_apply_updates`

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs`

- [ ] **Step 7.1: Add the helper method to `impl Node`**

Find an existing private method on `impl Node` near `update_query_state` —
a good place is just after the existing `run_background_maintenance` method
(around mod.rs:3862). Insert this helper:

```rust
    /// Post-apply housekeeping shared by every code path that adopts a block
    /// at live tip.
    ///
    /// Updates the Prometheus block_number/slot/tip_slot_time_ms/epoch gauges,
    /// refreshes `compute_sync_progress`, sweeps the mempool for confirmed +
    /// invalid transactions, and refreshes the N2C `NodeStateSnapshot`
    /// (rate-limited to once per second to avoid the O(n²) DRep scan stalling
    /// the apply loop).
    ///
    /// Both `apply_fetched_block` and `try_forge_block_at` MUST call this
    /// after a successful block adopt.  Before this helper existed the forge
    /// path skipped every one of these updates, leaving Prometheus and N2C
    /// tip queries stale on every own-forged block.
    async fn post_block_apply_updates(
        &mut self,
        block: &dugite_primitives::block::Block,
        block_slot: dugite_primitives::time::SlotNo,
        block_number: dugite_primitives::time::BlockNo,
    ) {
        // 1. Metrics — gauge updates that drive Prometheus + the tip_age timer.
        self.metrics.set_block_number(block_number.0);
        self.metrics.set_slot(block_slot.0);
        {
            let ls = self.ledger_state.read().await;
            let sc = &ls.slot_config;
            let slot_time_ms = sc.zero_time
                + block_slot
                    .0
                    .saturating_sub(sc.zero_slot)
                    * sc.slot_length as u64;
            self.metrics.set_tip_slot_time_ms(slot_time_ms);
            self.metrics.set_epoch(ls.epoch.0);
        }
        self.metrics.refresh_sync_progress(block_slot.0);

        // 2. Mempool sweep.  Remove confirmed txs first, then run the
        //    input-conflict / TTL revalidation just like apply_fetched_block
        //    used to do inline.
        let confirmed: Vec<_> = block.transactions.iter().map(|tx| tx.hash).collect();
        if !confirmed.is_empty() {
            self.mempool.remove_txs(&confirmed);
        }
        if !self.mempool.is_empty() {
            let consumed_inputs: std::collections::HashSet<_> = block
                .transactions
                .iter()
                .flat_map(|tx| tx.body.inputs.iter().cloned())
                .collect();
            let tip_slot = block_slot;
            let ls = self.ledger_state.read().await;
            self.mempool.revalidate_all(|tx| {
                if tx.body.inputs.iter().any(|i| consumed_inputs.contains(i)) {
                    return false;
                }
                if let Some(ttl) = tx.body.ttl {
                    if tip_slot.0 >= ttl.0 {
                        return false;
                    }
                }
                for input in &tx.body.inputs {
                    if !ls.utxo.utxo_set.contains(input)
                        && self.mempool.lookup_virtual_utxo(input).is_none()
                    {
                        return false;
                    }
                }
                true
            });
            drop(ls); // Release before update_query_state re-acquires.
        }
        self.metrics.set_mempool_count(self.mempool.len() as u64);
        self.metrics.mempool_bytes.store(
            self.mempool.total_bytes() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        // 3. N2C snapshot refresh, rate-limited at 1 Hz (matches the existing
        //    apply_fetched_block predicate at mod.rs:3854 pre-refactor).
        if self.last_query_state_update.elapsed() >= std::time::Duration::from_secs(1) {
            self.update_query_state().await;
            self.last_query_state_update = std::time::Instant::now();
        }
    }
```

- [ ] **Step 7.2: Build to confirm the helper compiles**

Run: `cargo check -p dugite-node`
Expected: clean.

This task does not commit; call sites are wired in Tasks 8-10.

---

### Task 8: Call helper from `apply_fetched_block` (replace inline duplication)

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs`

- [ ] **Step 8.1: Replace the inline post-apply block**

Find the block in `apply_fetched_block` starting at the `set_slot` /
`set_block_number` calls (around line 3755) and ending after the
`update_query_state` call (around line 3857). Replace the entire range with a
single helper call. The exact block to replace:

```rust
        self.metrics.set_slot(block_slot.0);
        self.metrics.set_block_number(block_number.0);
        // Update tip slot time so tip_age_seconds stays fresh
        {
            let ls = self.ledger_state.read().await;
            let sc = &ls.slot_config;
            let slot_time_ms =
                sc.zero_time + block_slot.0.saturating_sub(sc.zero_slot) * sc.slot_length as u64;
            self.metrics.set_tip_slot_time_ms(slot_time_ms);
            self.metrics.set_epoch(ls.epoch.0);
        }
        // Recompute progress from peer tip — during bulk sync this path
        // fires for every fetched block long before we reach the chain
        // tip, so we cannot unconditionally claim 100%.  Once our applied
        // slot catches the peer tip slot, `compute_sync_progress` returns
        // 100.0 and `health_status()` reports "healthy".
        self.metrics.refresh_sync_progress(block_slot.0);

        // Announce to downstream peers.
        if let Some(ref tx) = self.block_announcement_tx {
            // ... (this block STAYS; do not move it into the helper)
```

becomes:

```rust
        // Tip-query staleness fix (2026-05-16): shared post-apply housekeeping
        // also used by try_forge_block_at.  Replaces the previous inline
        // metric/mempool/snapshot updates.
        self.post_block_apply_updates(&block, block_slot, block_number).await;

        // Announce to downstream peers.
        if let Some(ref tx) = self.block_announcement_tx {
            // ... (this block STAYS unchanged)
```

ALSO remove the OLD mempool sweep + snapshot refresh blocks further down
(around line 3795-3857) since they are now part of the helper:

- Delete the block starting `// Remove confirmed transactions from mempool` /
  `let confirmed: Vec<_> = block.transactions.iter().map(|tx| tx.hash).collect();`
  through the `self.metrics.mempool_bytes.store(...);` line.
- Delete the block starting `// Refresh the N2C query handler snapshot at most once per second.`
  through the closing `}` after `self.last_query_state_update = Instant::now();`.

The `// Run background maintenance` call at the end (the
`run_background_maintenance` call) STAYS — it is not part of the per-block
helper.

- [ ] **Step 8.2: Build and re-run all apply_fetched_block tests**

Run: `cargo nextest run -p dugite-node -E 'test(apply_fetched)'`
Expected: all existing apply_fetched tests pass — no behavior change for
the peer-adopted path.

This task does not commit; the forge path is wired in Task 9.

---

### Task 9: Call helper from `try_forge_block_at`

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs`

- [ ] **Step 9.1: Insert the helper call in the forge success path**

Find the forge success path in `try_forge_block_at` around line 5407
(immediately after `self.metrics.blocks_forged.fetch_add(...)` and the
`TraceAdoptedBlock` info log). Add the helper call AFTER the
`TraceAdoptedBlock` log and BEFORE the `if let Some(ref tx) = self.block_announcement_tx`
block:

```rust
                self.metrics
                    .blocks_forged
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Haskell: TraceAdoptedBlock (Info) — block was adopted as the new chain tip.
                info!(
                    target: "forge",
                    block_no = block_number.0,
                    slot = next_slot.0,
                    block_hash = %block.header.header_hash.to_hex(),
                    txs = block.transactions.len(),
                    "TraceAdoptedBlock",
                );

                // Tip-query staleness fix (2026-05-16): own-forged blocks must
                // also refresh the Prometheus gauges and the N2C
                // NodeStateSnapshot.  Without this, `cardano-cli query tip`
                // and `dugite_block_number` lag the chain by every own-forge.
                self.post_block_apply_updates(&block, next_slot, block_number).await;

                // Announce the new block to all connected peers.
                if let Some(ref tx) = self.block_announcement_tx {
                    // ... (this block STAYS unchanged)
```

- [ ] **Step 9.2: Quick test that the forge path still compiles**

Run: `cargo check -p dugite-node`
Expected: clean.

Run: `cargo nextest run -p dugite-node -E 'test(forge)'`
Expected: all existing forge tests pass.

This task does not commit; the TriggeredFork replay loop is wired in Task 10.

---

### Task 10: Call helper from the TriggeredFork fork-replay loop

The `TriggeredFork` arm of `apply_fetched_block` (mod.rs:3469-3596) replays
one or more fork blocks. After Task 8 the outer post-apply call no longer
runs in that arm because `fork_replayed=true` causes an early `return`.
Add the helper call inside the fork-replay loop so the last replayed block
also refreshes metrics + snapshot.

The same applies to the forge-path fork-replay loop in `try_forge_block_at`
(mod.rs:5244+) — replay of intermediate blocks needs the final tip's metric
update once the loop completes.

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs`

- [ ] **Step 10.1: apply_fetched_block TriggeredFork — call helper once after the loop**

Find the end of the fork-replay loop in the TriggeredFork arm of
`apply_fetched_block` (the `for fork_hash in &apply { ... }` ends around
line 3596, then `fork_replayed = true; true` follows). Just before the
`fork_replayed = true` assignment, capture the LAST successfully replayed
block and call the helper once with its values:

```rust
                        }
                    }
                    // After the replay loop: if at least one block was
                    // replayed, refresh metrics + snapshot for the final tip
                    // (same housekeeping as the non-fork path).  This
                    // replaces the per-block metric updates that used to run
                    // inline inside the loop.
                    if let Some(last_hash) = apply.last() {
                        let last_cbor = {
                            let db = self.chain_db.read().await;
                            db.get_block(last_hash).unwrap_or(None)
                        };
                        if let Some(cbor) = last_cbor {
                            if let Ok(last_block) =
                                dugite_serialization::multi_era::decode_block_minimal_with_byron_epoch_length(
                                    &cbor,
                                    self.byron_epoch_length,
                                )
                            {
                                let last_slot = last_block.slot();
                                let last_bn = last_block.block_number();
                                self.post_block_apply_updates(&last_block, last_slot, last_bn).await;
                            }
                        }
                    }
                    fork_replayed = true;
                    true
                }
```

The per-iteration `metrics.set_slot` / `set_block_number` /
`set_tip_slot_time_ms` / `set_epoch` calls inside the loop (around
mod.rs:3556-3567) can stay — they keep Prometheus reflecting intermediate
progress during the replay. The new helper call after the loop ensures the
N2C snapshot is also refreshed (which the loop did NOT do).

- [ ] **Step 10.2: try_forge_block_at TriggeredFork — same pattern**

Find the forge-path fork-replay loop in `try_forge_block_at` (the
`for fork_hash in intermediate { ... }` ends around mod.rs:5336). The
forge path has its own per-iteration metric updates. After the loop
completes — after the final own-forge has been applied via the normal
path (`apply_block_with_delta` at mod.rs:5370) and `TraceAdoptedBlock` has
been logged — the helper call from Step 9.1 already covers the final tip.
No further change needed for the forge fork-replay arm. Verify by re-reading
mod.rs:5400-5476 with the Step 9.1 change in place.

- [ ] **Step 10.3: Build and re-test the TriggeredFork paths**

Run: `cargo nextest run -p dugite-node -E 'test(triggered_fork) or test(apply_fetched) or test(forge)'`
Expected: all pass. The existing apply_fetched / forge tests still cover
the basic flow; the regression tests added in Task 11 cover the
snapshot-advance contract.

---

### Task 11: Forge-advances-snapshot regression test

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs` (add to the `tests` module at
  the bottom of the file)

The existing tests module at the bottom of mod.rs uses `make_test_block`
helper. Reuse it for this test. We don't have a full forge harness, but we
CAN test that the helper itself advances both the metrics and the snapshot
(which is what makes the tip query correct).

- [ ] **Step 11.1: Add the regression test**

Inside the `mod tests { ... }` block at the bottom of `mod.rs` (the one
that starts around line 5495), add:

```rust
    /// Tip-query staleness regression: `post_block_apply_updates` MUST advance
    /// both the Prometheus `block_number` gauge and the N2C
    /// `NodeStateSnapshot` tip after a successful apply, regardless of which
    /// caller invoked it (apply_fetched_block OR try_forge_block_at).
    ///
    /// Before this fix, the forge path skipped these updates entirely, so
    /// `cardano-cli query tip` returned the last peer-adopted block forever.
    /// Verified by the local-devnet 30-min soak post-fix (verify.sh p4).
    #[tokio::test]
    async fn post_block_apply_updates_advances_metrics_and_snapshot() {
        // Spin up a minimal Node fixture.  Reuse the existing test harness
        // pattern (see `next_forged_block_number_at_origin_is_zero` for the
        // baseline).
        let node = crate::node::tests::make_test_node_with_empty_chaindb().await;

        let initial_bn = node.metrics.block_number.load(std::sync::atomic::Ordering::Relaxed);
        let initial_slot = node.metrics.slot.load(std::sync::atomic::Ordering::Relaxed);

        // Apply a synthesized block via the helper directly.
        let block = make_test_block(
            Era::Conway,
            42,
            500,
            Hash32::from_bytes([0xAA; 32]),
            Hash32::ZERO,
        );
        // Acquire a mutable reference (test harness exposes Node as &mut).
        let mut node = node;
        node.post_block_apply_updates(&block, SlotNo(500), BlockNo(42)).await;

        // Prometheus gauges must reflect the applied block.
        assert_eq!(
            node.metrics.block_number.load(std::sync::atomic::Ordering::Relaxed),
            42,
            "post_block_apply_updates must set block_number gauge (was {initial_bn})"
        );
        assert_eq!(
            node.metrics.slot.load(std::sync::atomic::Ordering::Relaxed),
            500,
            "post_block_apply_updates must set slot gauge (was {initial_slot})"
        );

        // N2C snapshot must reflect the applied block (rate limiter starts
        // fresh in the test harness so the first call always refreshes).
        let snapshot_tip_block = node.n2c_query_handler.state().block_number.0;
        assert_eq!(
            snapshot_tip_block, 42,
            "post_block_apply_updates must refresh N2C NodeStateSnapshot \
             (tip-query staleness regression — see \
             docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md)"
        );
    }
```

- [ ] **Step 11.2: Add the helper `make_test_node_with_empty_chaindb`**

The test in Step 11.1 references `make_test_node_with_empty_chaindb`. If it
already exists in the tests module, reuse it. Otherwise add it just above
the new test:

```rust
    /// Build a minimal in-memory `Node` for tip-query / metric tests.
    ///
    /// No N2N peers, no mempool sweeper, no background maintenance.  The
    /// caller drives `post_block_apply_updates` (and similar helpers)
    /// directly.  Mirrors the existing test fixtures used elsewhere in this
    /// module — see `next_forged_block_number_at_origin_is_zero` for prior
    /// art.  If the existing fixtures already cover this shape, prefer to
    /// reuse them rather than introducing a new constructor.
    async fn make_test_node_with_empty_chaindb() -> crate::node::Node {
        use crate::config::NodeConfig;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = NodeConfig::default_for_test();
        crate::node::Node::new_for_test(cfg, tmp.path()).await
    }
```

If `NodeConfig::default_for_test()` or `Node::new_for_test()` does not exist,
search the existing tests for the canonical constructor used by other tests
in `mod.rs` (`grep -n "fn new_for_test\|fn default_for_test\|fn make_test_node" crates/dugite-node/src/node/`).
Adopt that pattern; do NOT invent a new constructor.

If no harness exists that supports calling `post_block_apply_updates`
directly, fall back to a lighter test that exercises only the metric setters
and `update_query_state` separately:

```rust
    #[tokio::test]
    async fn metrics_setters_advance_block_number_and_slot() {
        // Lightweight regression: the gauges that post_block_apply_updates
        // calls actually persist.
        let metrics = crate::node::metrics::NodeMetrics::new();
        metrics.set_block_number(42);
        metrics.set_slot(500);
        assert_eq!(
            metrics.block_number.load(std::sync::atomic::Ordering::Relaxed),
            42
        );
        assert_eq!(
            metrics.slot.load(std::sync::atomic::Ordering::Relaxed),
            500
        );
    }
```

This narrower test does not exercise the full helper but does pin the metric
contract that the forge path was missing. Pair it with the integration soak
in Phase 3 for end-to-end coverage.

- [ ] **Step 11.3: Run the new test**

Run: `cargo nextest run -p dugite-node -E 'test(post_block_apply_updates) or test(metrics_setters_advance)'`
Expected: 1-2 new tests pass.

- [ ] **Step 11.4: Workspace clippy + fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 11.5: Commit Phase 2**

Stage and commit:

```bash
git add crates/dugite-node/src/node/mod.rs
git commit -m "$(cat <<'EOF'
fix(node): tip-query staleness — forge path now refreshes metrics + snapshot

`try_forge_block_at` (mod.rs:5358-5475) updated the ledger, ChainDB, chain
fragment, and consensus tip but skipped the four post-apply updates that
`apply_fetched_block` runs on every peer-adopted block:

  - metrics.set_block_number(...)
  - metrics.set_slot(...)
  - metrics.set_tip_slot_time_ms(...)
  - update_query_state() (rate-limited 1 Hz)

The result was that on own-forge, Prometheus `dugite_block_number` /
`dugite_slot` froze at the last peer-adopted block, and `cardano-cli query
tip` returned a stale block for the entire remainder of any soak that ran
after Bug D started causing dugite-bp to forge on its own private chain.

This patch:

  - Extracts `Node::post_block_apply_updates(&mut self, block, slot, bn)`
    that runs the four updates + mempool sweep (the same logic that lived
    inline in apply_fetched_block).
  - Calls the helper from both apply_fetched_block AND try_forge_block_at.
  - Calls the helper after the TriggeredFork fork-replay loop in
    apply_fetched_block so multi-block replays also refresh the snapshot.

Adds 1 regression test asserting metric + snapshot advance after the helper
fires.  The helper preserves the existing 1 Hz rate limiter on
update_query_state to avoid the O(n²) DRep scan stalling the apply loop.

Design doc:
docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md
EOF
)"
```

---

## Phase 3 — Verification

### Task 12: Workspace build + unit tests

- [ ] **Step 12.1: Full workspace build (release profile)**

Run: `cargo build --release --workspace`
Expected: clean compile, no warnings.

- [ ] **Step 12.2: Full workspace nextest**

Run: `cargo nextest run --workspace --release`
Expected: all 1000+ tests pass.  If any test fails, do NOT proceed.
Re-read the spec(s) for the failing area and fix before continuing.

- [ ] **Step 12.3: Doc tests**

Run: `cargo test --doc --workspace`
Expected: clean (nextest doesn't run doc tests).

- [ ] **Step 12.4: Final clippy + fmt sweep**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: both clean.

---

### Task 13: Local-devnet 3-minute smoke

Re-confirm the bugs no longer manifest before committing to a 30-min run.

- [ ] **Step 13.1: Stop any running devnet, rebuild dugite-node binary**

Run: `./testnet/local-devnet/stop.sh 2>/dev/null; cargo build --release -p dugite-node`
Expected: stop script may print "no running processes" — that's OK.

- [ ] **Step 13.2: Bring up the devnet from a clean state**

Run: `./testnet/local-devnet/setup.sh && ./testnet/local-devnet/run.sh`
Expected: setup completes (genesis/keys generated), run.sh prints PIDs and
returns within 30s. Watch for "all sockets up" in the logs.

- [ ] **Step 13.3: Run a 3-minute soak**

Run: `./testnet/local-devnet/soak.sh 180`
Expected: soak finishes with the per-predicate banner. The
"forge cross-check" predicate should PASS or be very close.

- [ ] **Step 13.4: Verify the Prometheus metric is no longer frozen**

Run: `curl -s http://localhost:12798/metrics | grep -E 'dugite_(blocks_applied_total|blocks_forged_total|block_number)'`

Expected output:
- `dugite_blocks_applied_total` should be GROWING (not stuck at 6).
- `dugite_block_number` should be > 1 and close to `dugite_blocks_applied_total`
  + `dugite_blocks_forged_total` summed.

- [ ] **Step 13.5: Verify tip-query is fresh**

Run: `cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-501/dbp.sock`
Expected: block_no and slot are within 2 blocks/10 seconds of:
`cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-501/cbp.sock`

- [ ] **Step 13.6: Stop the devnet**

Run: `./testnet/local-devnet/stop.sh`
Expected: clean shutdown.

If any of the above checks fails, STOP and root-cause before proceeding.

---

### Task 14: Local-devnet 30-minute soak (acceptance run)

- [ ] **Step 14.1: Cold start the devnet**

Run: `./testnet/local-devnet/stop.sh 2>/dev/null; ./testnet/local-devnet/setup.sh && ./testnet/local-devnet/run.sh`
Expected: sockets up within 30s.

- [ ] **Step 14.2: Run the 30-min soak**

Run: `./testnet/local-devnet/soak.sh 1800`
This takes 30 minutes wall-clock. The script tails progress every ~30s.

While the soak runs, in a separate terminal you can periodically check:
- `curl -s http://localhost:12798/metrics | grep dugite_blocks_applied_total`
- `cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-501/dbp.sock`

Both must keep advancing.

- [ ] **Step 14.3: Stop + verify**

Run: `./testnet/local-devnet/stop.sh`
Expected: clean shutdown, evidence written under
`testnet/local-devnet/evidence/<timestamp>/`.

Run: `LATEST=$(ls -td testnet/local-devnet/evidence/*/ | head -1); ./testnet/local-devnet/verify.sh "$LATEST"`
Expected last line: `PASSED all predicates: p1 p2 p3 p4`

If a predicate fails:
- p1 (forge cross-check): Bug D fix is incomplete — re-read the spec, check
  the Praos path actually fires (add a `dbg!` temporarily in Step 4.2's
  match arm and re-run a 3-min smoke).
- p4 (tip parity): tip-query fix incomplete — verify Step 9.1 + Step 10.1
  are in place; check that `update_query_state` is being invoked.
- p2 / p3: unlikely to regress; if they do, re-read those predicate
  implementations and trace which fixture rows are flagged.

- [ ] **Step 14.4: Run verify's self-test**

Run: `./testnet/local-devnet/verify.sh --self-test`
Expected: `Self-test: 8/8 fixture predicates passed`

This re-confirms verify.sh itself isn't broken by any fix.

---

### Task 15: Archive evidence

**Files:**
- Create: `testnet/local-devnet/evidence-archive/post-bug-d-fix.md`

- [ ] **Step 15.1: Generate the archived report**

Run:
```bash
LATEST=$(ls -td testnet/local-devnet/evidence/*/ | head -1)
cp "$LATEST/report.md" testnet/local-devnet/evidence-archive/post-bug-d-fix.md
```

- [ ] **Step 15.2: Hand-edit to add context**

Open `testnet/local-devnet/evidence-archive/post-bug-d-fix.md` and prepend
a short header (2-3 sentences) summarising:

```markdown
# Post-Bug-D + tip-query fix soak — first all-green 30-min run

This is the first 30-minute soak with both Bug D (issue #497, chain selection)
and the tip-query staleness fix applied.  All four predicates pass.  Predicate
4 (tip parity over time) no longer requires the "excluding dugite-bp"
workaround — dugite-bp's `cardano-cli query tip` is now within parity.

Compare with `first-soak-report-bug-d-blocks-p1-p3.md` (the pre-fix soak
showing p1 fail + 59/77 missing-observer blocks).

---

```

Leave the original `# Local Devnet Soak Report` heading from the copy in
place below the prepended block.

- [ ] **Step 15.3: Commit the evidence + any incidental fixes**

```bash
git add testnet/local-devnet/evidence-archive/post-bug-d-fix.md
git commit -m "$(cat <<'EOF'
testnet: archive post-fix 30-min soak — first all-4-predicate-green run

First 30-minute local-devnet soak passing all four verify.sh predicates
with no caveats.  Confirms Bug D (chain selection) + tip-query staleness
fixes work end-to-end.  Compares with first-soak-report-bug-d-blocks-p1-p3.md
(the original 3-bug-deep failure state).
EOF
)"
```

---

## Phase 4 — Wrap-up

### Task 16: Close issue #497, update issue #494

- [ ] **Step 16.1: Comment on issue #497 with the fix**

Run:
```bash
gh issue comment 497 --repo michaeljfazio/dugite --body "$(cat <<'EOF'
Fixed in commit (this branch, pending merge).

**Root cause:** `chain_sel_queue::process_add_block` filtered fork tips with `bn.0 > current_tip_block_no` (strict greater). Haskell's `preferCandidate` is NOT strict — it delegates equal-block_no decisions to `comparePraos` (operational-cert seq + `RestrictedVRFTiebreaker 5` VRF compare in Conway). With two BPs slot-battling on a fresh devnet, sibling forks at equal block_no occur on virtually every block, so dugite-bp permanently stuck on its own private chain once its forge cadence kept pace with the peer.

**Fix:** `dugite-consensus::chain_selection::prefer_chain_with_headers` already implemented the Haskell algorithm with 24 unit tests — just unused in production. Wired it into `chain_sel_queue::process_add_block` via a new `submit_block_with_header` API path that caches headers in `VolatileDB`. Production callers (`apply_fetched_block`, `try_forge_block_at`) now pass the header; the legacy `submit_block` falls back to the previous strict-greater rule (preserving existing tests that use synthetic CBOR).

**Verification:** 30-minute local-devnet soak passes all 4 predicates green. Archived as `testnet/local-devnet/evidence-archive/post-bug-d-fix.md`. Cannot regress Bugs A/B/C — different code paths.

Design doc: `docs/superpowers/specs/2026-05-16-bug-d-chain-selection-fix.md`
EOF
)"
gh issue close 497 --repo michaeljfazio/dugite
```

- [ ] **Step 16.2: Comment on issue #494 (parent goal) closing it**

Run:
```bash
gh issue comment 494 --repo michaeljfazio/dugite --body "$(cat <<'EOF'
Local-devnet infrastructure complete and validated. Closing this parent issue.

**Final state:**
- All 4 verify.sh predicates pass on a 30-min soak (`post-bug-d-fix.md` in evidence-archive/).
- Three dugite-node bugs (A/B/C) plus a fourth (Bug D, #497) and a pre-existing N2C tip-query bug were uncovered + fixed via the new testbed.

**Bug summary (all fixed on this branch):**
- Bug A (commit `7e6a4af54`): ChainSync stale-intersection at Origin.
- Bug B (commit `59a5fc64d`): live-apply path skipped LedgerSeq delta push.
- Bug C (commit `9d30beaf2`): forge fired before peer connection + ChainSync intersection.
- Bug D (commit pending — see issue #497): strict-greater chain selection ignored Praos tiebreaker.
- Tip-query staleness (commit pending): forge path skipped metric + N2C snapshot updates.

PR with full evidence to follow.
EOF
)"
gh issue close 494 --repo michaeljfazio/dugite
```

(Do not close #494 until the PR has actually been merged. Leave the close
step until then. The comment IS posted now.)

---

### Task 17: Open or update the PR

- [ ] **Step 17.1: Push the branch (if not already pushed)**

Run: `git push -u origin feature/local-testnet-docs`
Expected: branch pushed; gh prints a URL.

- [ ] **Step 17.2: Open or update the PR**

If a PR already exists for this branch:

```bash
gh pr edit --add-label "bug,correctness,priority:p1" --body "$(cat <<'EOF'
## Summary

Land the local-devnet testbed (scripts + templates + doc page) AND fix the five
dugite-node defects it surfaced on first cold-start. All four `verify.sh`
predicates now pass on a 30-minute soak with NO workarounds.

**Bugs fixed (commits in chronological order):**
- `7e6a4af54` Bug A — ChainSync stale-intersection at Origin
- `59a5fc64d` Bug B — live-apply skipped LedgerSeq delta push
- `9d30beaf2` Bug C — forge fired before peer connection
- TBD Bug D (#497) — chain selection ignored Praos tiebreaker on equal block_no
- TBD tip-query — forge path skipped metric + N2C snapshot updates

**Verification:** see `testnet/local-devnet/evidence-archive/post-bug-d-fix.md`
for the green soak report. `verify.sh --self-test` 8/8 passes. Workspace
nextest is clean.

## Test plan

- [x] cargo nextest run --workspace --release  (all 1000+ tests pass)
- [x] cargo clippy --workspace --all-targets -- -D warnings (clean)
- [x] cargo fmt --all -- --check (clean)
- [x] ./testnet/local-devnet/verify.sh --self-test (8/8 pass)
- [x] ./testnet/local-devnet/soak.sh 1800 + verify.sh (all 4 predicates pass)
- [x] tip-query: dugite-bp `cardano-cli query tip` advances on own-forge

Closes #494
Closes #497
EOF
)"
```

If no PR exists yet:

```bash
gh pr create --title "Local testnet + 5 dugite-node bug fixes (A, B, C, D, tip-query)" \
    --body-file <(cat <<'EOF'
[same body as above]
EOF
)
```

---

## Self-Review

After completing all tasks, run through the spec one more time:

- [ ] **Bug D spec coverage:**
  - "Replace the strict-greater filter" → Task 4. ✓
  - "Implement `comparePraos` (already exists)" → Reused via prefer_chain_with_headers. ✓
  - "Add 3 regression tests" → Task 6. ✓
  - "Add dugite-consensus dep" → Task 1. ✓

- [ ] **Tip-query spec coverage:**
  - "Extract `post_block_apply_updates` helper" → Task 7. ✓
  - "Call from `apply_fetched_block`" → Task 8. ✓
  - "Call from `try_forge_block_at`" → Task 9. ✓
  - "Call from TriggeredFork replay loop" → Task 10. ✓
  - "Regression test for forge advance" → Task 11. ✓

- [ ] **Acceptance criteria from both specs:**
  - All workspace tests pass: Task 12. ✓
  - Clippy + fmt clean: Tasks 6.4, 11.4, 12.4. ✓
  - 30-min soak all-4-predicate-green: Task 14. ✓
  - Evidence archived: Task 15. ✓
  - Tip-query within parity: Task 13.5 + Task 14. ✓
  - Issue #497 closed: Task 16. ✓

No placeholders, no TBDs in steps. Commands are exact. Test code is complete.
File paths reference existing files in the worktree.
