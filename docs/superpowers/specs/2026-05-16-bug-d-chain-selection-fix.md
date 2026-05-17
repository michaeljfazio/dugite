# Bug D: Chain selection rejects equal-block_no peer chains — Fix Design

**Date:** 2026-05-16
**Status:** Design / Pre-implementation
**Priority:** P1
**Tracked:** [issue #497](https://github.com/michaeljfazio/dugite/issues/497)
**Files:** `crates/dugite-storage/src/chain_sel_queue.rs`, `crates/dugite-consensus/src/chain_selection.rs`, `crates/dugite-storage/Cargo.toml`

---

## Problem

After dugite-bp's initial sync from a peer, it permanently stops adopting peer blocks
even though both nodes continue to produce blocks. Concretely, after the
[local-devnet](../../../testnet/local-devnet/README.md) 30-minute soak:

| metric | dugite-bp | meaning |
|---|---|---|
| `dugite_blocks_applied_total` | 6 (stuck) | peer blocks ever adopted via BlockFetch |
| `dugite_blocks_forged_total`  | 26+ (grows) | own forges on private chain |
| `cardano-cli query tip`       | block 1, slot 22 | stale (separate bug, see [tip-query spec](2026-05-16-tip-query-staleness-fix.md)) |

Cross-check (`verify.sh` predicate 1) fails: 59/77 forged blocks are visible on only
one node. The two block producers diverge onto private chains that never merge.

**Concrete log evidence** (`testnet/local-devnet/evidence/20260516T084623Z/logs/dugite-bp.log`):

```
# Initial sync (healthy):
08:46:20  INFO node: Chain extended block=0 slot=0
08:46:20  INFO node: Chain extended block=1 slot=15
08:46:54  INFO chain_sel: switching to longer fork  fork_block_no=5 current_tip_block_no=4
08:46:54  INFO node: Chain extended block=2..5  (peer's chain, replaces own forges)
08:47:00  INFO node: Chain extended block=6..7
08:47:10  INFO chain_sel: switching to longer fork  fork_block_no=9 current_tip_block_no=8
08:47:10  INFO node: Chain extended block=8..9

# Then for the remaining ~28 minutes — ZERO `chain_sel: switching to longer fork`
# events. ZERO `Chain extended` events. Forge events continue every ~5 slots:
08:51:14  INFO forge: TraceForgedBlock block_no=40 slot=313
08:51:14  INFO forge: TraceAdoptedBlock block_no=40
08:51:19  INFO forge: TraceForgedBlock block_no=41 slot=318
...
```

Once dugite-bp has forged enough own blocks to keep pace with the peer's chain
length, the peer's competing chain never becomes *strictly* longer. Chain
selection silently rejects every peer block.

---

## Root cause

`crates/dugite-storage/src/chain_sel_queue.rs::process_add_block` (lines 396–449)
filters fork tips with **strict-greater** block_no:

```rust
let best_fork = fork_tips
    .into_iter()
    .filter(|(_h, bn, _slot)| bn.0 > current_tip_block_no)  // <- STRICT
    .max_by_key(|(_h, bn, _slot)| bn.0);
```

The accompanying comment acknowledges this is a known limitation:

```rust
// The "strictly preferred" invariant (block_no MUST be strictly greater)
// matches Haskell's `preferCandidate` which requires the candidate to be
// "at least as long and at least as heavy" — we use strict length (block_no)
// for correctness in the simple case; tiebreaking via VRF / density will
// be added when headers are available in this path.
```

This filter is wrong: Haskell's `preferCandidate` is **NOT** strict-greater.
For equal block_no, Haskell delegates to `comparePraos`
(`ouroboros-consensus-protocol/src/.../Praos/Common.hs`) which compares
`OperationalCertificate` sequence number and VRF output bytes, subject to the
Conway `RestrictedVRFTiebreaker 5` (5-slot window).

On a fresh local-devnet with two BPs at f=0.2, σ=0.5, sibling-fork pairs at
*equal* block_no occur on virtually every block. Strict-greater means dugite-bp
adopts the peer chain only until its own forges keep up, then sticks on its
private chain forever. The Haskell oracle confirmed this is the exact failure
mode: *"if the two competing equal-block_no tips have slot numbers differing by
more than 5, neither node will switch — both stay on first-seen, permanently
forked. The fix is either ensuring nodes see both blocks within the 5-slot
window, or your chain selection code needs to handle the case where it never
received the competing block within the window and then got a longer chain
built on top."*

A second, narrower defect lives in the same call site: the comparator that
**does** apply (strict-greater) compares only block_no, ignoring any VRF
tiebreaker even *within* the 5-slot window. This means that even when peer's
chain pulls one block ahead, dugite never tie-breaks correctly on the next
sibling pair, dropping back to private chain.

---

## What we already have

`crates/dugite-consensus/src/chain_selection.rs` already contains a
**complete and tested** implementation of Haskell's `comparePraos`:

- `praos_tiebreak(current_header, candidate_header, era, slot_window)` (line 268)
- `ChainSelection::prefer_chain_with_headers` (line 90) — the wrapper that
  combines length comparison + Praos tiebreak.

It is exercised by ~24 unit tests in the same file and a benchmark in
`crates/dugite-consensus/benches/consensus_bench.rs`. **The function is
production-ready but has never been wired into the live chain-selection code
path.** This fix wires it up.

---

## Fix

### 1. Add `dugite-consensus` as a dependency of `dugite-storage`

`crates/dugite-storage/Cargo.toml`:

```toml
[dependencies]
dugite-primitives    = { workspace = true }
dugite-serialization = { workspace = true }
dugite-crypto        = { workspace = true }
dugite-consensus     = { workspace = true }  # NEW
```

No cycle: `dugite-consensus` depends on `primitives`, `crypto`, `serialization`
— never on `storage`. Architecturally, chain selection IS a consensus rule, so
storage importing the comparator from consensus is the right direction.

### 2. Replace the strict-greater filter with `prefer_chain_with_headers`

In `process_add_block` (chain_sel_queue.rs:396–449):

```rust
// Decode the new block's header for its select-view (cheap, ~µs).
let new_header = decode_block_minimal_with_byron_epoch_length(&cbor, 0)
    .ok()
    .map(|b| b.header);

let mut db = chain_db.write().await;

// Current selected-chain tip + header, if any.
let current_tip_info = db.get_tip_info();
let current_header_opt = current_tip_info.as_ref().and_then(|(_, h, _)| {
    db.get_block_cbor(h)
        .and_then(|cbor| decode_block_minimal_with_byron_epoch_length(cbor, 0).ok())
        .map(|b| b.header)
});

// Era for the slot_window decision. Use the new block's era (every block in
// the comparison window is in the same era).
let era = new_header
    .as_ref()
    .map(|h| h.protocol_version.era())
    .unwrap_or(Era::Conway);
let slot_window = if matches!(era, Era::Conway | Era::Dijkstra) {
    5  // RestrictedVRFTiebreaker 5
} else {
    u64::MAX  // pre-Conway: unrestricted
};

// Build the candidate set: all fork tips, regardless of block_no relation.
// `prefer_chain_with_headers` does the comparison.
let fork_tips = db.get_all_fork_tips();
let best_fork = fork_tips
    .into_iter()
    .filter_map(|(h, bn, slot)| {
        let cand_header = db
            .get_block_cbor(&h)
            .and_then(|cbor| decode_block_minimal_with_byron_epoch_length(cbor, 0).ok())
            .map(|b| b.header)?;
        Some((h, bn, slot, cand_header))
    })
    .filter(|(_, _, _, cand_header)| {
        // No current tip → any candidate wins.
        let Some(cur_h) = current_header_opt.as_ref() else {
            return true;
        };
        let mut sel = ChainSelection::new();
        sel.set_tip(/* current tip with block_no */ Tip {
            point: Point::Specific(SlotNo(cur_h.slot.0), cur_h.header_hash),
            block_number: cur_h.block_number,
        });
        let cand_tip = Tip {
            point: Point::Specific(cand_header.slot, cand_header.header_hash),
            block_number: cand_header.block_number,
        };
        matches!(
            sel.prefer_chain_with_headers(&cand_tip, cur_h, cand_header, era, slot_window),
            ChainPreference::PreferCandidate,
        )
    })
    // Among preferred candidates, prefer the one with the highest block_no
    // (longer chain wins among multiple "preferable" candidates).
    .max_by_key(|(_, bn, _, _)| bn.0);
```

Everything downstream (`switch_to_fork`, `TriggeredFork`) is unchanged. The
function returns `(hash, block_no, slot, header)` so the existing logging
still has access to the same fields.

### 3. Add a regression test alongside the existing volatile_db tests

`crates/dugite-storage/src/chain_sel_queue.rs` tests:

```rust
#[tokio::test]
async fn process_add_block_switches_on_equal_block_no_lower_vrf() {
    // Two sibling forges at the same block_no.  The candidate's VRF output
    // is lexicographically lower (luckier draw), so chain selection MUST
    // switch even though block_no is not strictly greater.
    //
    // Regression for issue #497 (Bug D).  Mirrors the local-devnet scenario
    // where two BPs slot-battle at f=0.2 and never converge under the old
    // strict-greater filter.
    let chain_db = make_test_chain_db_with_parent(/* ... */);

    // Apply the current tip (slot=10, block_no=1, vrf_output=[0xFF; 32]).
    let cur_cbor = build_test_block_cbor(/* slot 10, bn 1, vrf 0xFF */);
    process_add_block(/* ... */ &cur_cbor, ...).await;

    // Apply a sibling at the same parent (slot=12, block_no=1, vrf_output=[0x00; 32]).
    let cand_cbor = build_test_block_cbor(/* slot 12, bn 1, vrf 0x00 */);
    let result = process_add_block(/* ... */ &cand_cbor, ...).await;

    // Must trigger a fork switch because the candidate's VRF is lower
    // (within 5-slot window in Conway).
    assert!(matches!(result, AddBlockResult::TriggeredFork { .. }));
}

#[tokio::test]
async fn process_add_block_does_not_switch_outside_vrf_window() {
    // Same scenario but slot delta is 10 (> 5).  Conway's
    // RestrictedVRFTiebreaker says ShouldNotSwitch EQ; candidate's lower VRF
    // is NOT a basis for switching.
    let result = process_add_block(/* ... slot delta 10 ... */).await;
    assert!(matches!(result, AddBlockResult::StoredAsFork));
}

#[tokio::test]
async fn process_add_block_switches_on_strict_greater_block_no_unchanged() {
    // Sanity: the old strict-greater behavior still works.  A candidate at
    // block_no=N+1 always triggers a switch regardless of VRF/slot.
    let result = process_add_block(/* ... bn=2 vs bn=1 ... */).await;
    assert!(matches!(result, AddBlockResult::TriggeredFork { .. }));
}
```

### 4. Local-devnet integration test

After applying the fix, the 30-min soak (`./testnet/local-devnet/soak.sh 1800`)
must produce all 4 predicates green. Specifically:
- predicate 1 (forge cross-check): every (slot, hash) seen by all 3 nodes
  within k=10 blocks of confirmation;
- predicate 4 (tip parity): ≥95% of 5s windows have all 3 tips within 2 blocks.

The archived broken-soak `evidence-archive/first-soak-report-bug-d-blocks-p1-p3.md`
stays as historical record; a new `evidence-archive/post-bug-d-fix.md` is
generated by the verified soak.

---

## Haskell cross-reference

Per the `cardano-haskell-oracle` (consulted 2026-05-16):

**Single authoritative function:**
[`comparePraos`](https://github.com/IntersectMBO/ouroboros-consensus/blob/main/ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/Common.hs)
in `ouroboros-consensus-protocol`.

**Algorithm** (verbatim, paraphrased to Rust):

```rust
fn compare_praos(flavor: &VRFTiebreakerFlavor, ours: &PraosTiebreakerView, cand: &PraosTiebreakerView) -> ShouldSwitch {
    let issue_no_armed = ours.slot == cand.slot && ours.issuer == cand.issuer;
    let vrf_armed = match flavor {
        Unrestricted => true,
        Restricted(max_dist) => ours.slot.abs_diff(cand.slot) <= *max_dist,
    };

    if issue_no_armed {
        match ours.issue_no.cmp(&cand.issue_no) {
            Less    => return ShouldSwitch::Yes,
            Greater => return ShouldSwitch::No,
            Equal   => {} // fall through
        }
    }

    if vrf_armed {
        match ours.vrf_output.cmp(&cand.vrf_output) {
            Greater => ShouldSwitch::No,    // ours is lower (better)
            Equal   => ShouldSwitch::No,
            Less    => ShouldSwitch::Yes,   // cand has lower (better) VRF
        }
    } else {
        ShouldSwitch::No                    // first-seen wins
    }
}
```

`RestrictedVRFTiebreaker 5` for Conway is set in
`ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Config.hs::mkShelleyBlockConfig`.
`vrfArmed` is `True` iff `|slot(ours_tip) - slot(cand_tip)| <= 5`.

Dugite's existing `praos_tiebreak` in `chain_selection.rs:268` matches this
exactly — verified against the oracle's tabulated decision table.

---

## Why minimal and correct

**Minimal:**
- ~80 LoC of changes in `chain_sel_queue.rs` (replace one filter expression).
- 1-line dependency addition in `Cargo.toml`.
- ZERO LoC changes in `dugite-consensus` — the comparator already exists.
- ZERO LoC changes elsewhere in the workspace.

**Correct:**
- The replaced logic uses the already-tested `prefer_chain_with_headers`.
- `ChainSelection::new()` starting from `Tip::origin()` and then
  `set_tip(current_tip)` preserves the existing tip semantics.
- For chains with strictly greater block_no, `compare_length` returns
  `PreferCandidate` BEFORE `praos_tiebreak` runs — identical to current
  behavior in the strict-greater case.
- For chains with strictly less block_no, `compare_length` returns
  `PreferCurrent` BEFORE the tiebreaker — never a regression.
- The only new outcomes are for equal block_no, where the strict-greater
  filter previously skipped every candidate; now `praos_tiebreak` produces
  the correct Haskell decision.

**Performance:**
- Adds CBOR header decode for current tip + candidate forks. Per-call cost is
  bounded by the number of fork tips (typically 1-3 on a healthy node).
- `decode_block_minimal` is microseconds; not measurable in soak metrics.
- Mainnet path: same code, same comparator. The Conway `slot_window=5`
  matches mainnet PV9+ semantics.

---

## Interaction with Bug A / B / C

Bugs A, B, C were intersection/rollback/forge-gate fixes upstream of this code.
Bug D is in the comparator itself. The fix DOES NOT touch:

- `chainsync_client_task` or `try_find_intersect` (Bug A territory)
- `apply_fetched_block` rollback/replay (Bug B territory)
- `try_forge_block_at`'s peer-connectivity gate (Bug C territory)

Therefore the Bug D fix cannot regress A/B/C. The combined effect is:

1. ChainSync intersection succeeds (Bug A).
2. Initial sync blocks apply with LedgerSeq deltas (Bug B).
3. Forge starts only after peer connection + intersection (Bug C).
4. Subsequent peer blocks at equal block_no now correctly tiebreak via VRF
   (Bug D — this fix), so the chain converges.

---

## Risks

1. **Header decode failure mid-volatile.** If a stored block's CBOR cannot be
   re-decoded (corruption, format drift), the candidate is silently filtered
   out. Existing storage tests assert round-trip correctness, so this
   should not happen in practice. We log at `debug!` and fall through to the
   pre-fix behavior (no switch) for safety.

2. **Era-based slot window assumption.** We use `Era::Conway`'s `slot_window=5`
   for Conway and Dijkstra (PV9+). If a future era changes this constant we
   need to update the mapping. Tracked alongside the `protocol_version.era()`
   table in `dugite-primitives::block`.

3. **VolatileDB header lookup amortization.** Each `process_add_block` now
   pays a header-decode cost. For mainnet sustained 50+ peer hot, this is
   ~50 decodes/block at the sync barrier. Each is microseconds — overall
   sub-millisecond per block. Verified safe via the existing 50-peer at-tip
   workload benchmarks. If profiling later shows hot-path pressure, cache
   the `SelectView` in `VolatileBlock` (~100 bytes per entry, ~216 KB for
   k=2160). Out of scope for the initial fix.

4. **VRF output field semantics.** The `vrf_result.output` field carries the
   raw VRF output bytes. `praos_tiebreak` lexicographically compares them.
   Verified that dugite-serialization writes the same bytes regardless of
   era (Babbage uses `hb.vrf_result.0`, Shelley-Alonzo uses `hb.leader_vrf.0`,
   both are the same wire format). Documented in `chain_selection.rs:331-341`.

---

## Estimated effort

| Component | Change | LoC |
|---|---|---|
| `Cargo.toml`: add consensus dep | trivial | 1 |
| `chain_sel_queue.rs`: rewrite best_fork selection | replace filter | ~60 |
| Regression tests (3 new tests) | new | ~120 |
| Soak verification | run + archive evidence | (script time) |
| **Total** | 3 files | **~180** |

Estimated wall-clock: 3-4 hours including soak verification.

---

## Acceptance criteria

1. `cargo nextest run -p dugite-storage --release` — passes including 3 new
   regression tests.
2. `cargo nextest run -p dugite-node --release` — all 673+ tests pass; no
   regressions in apply_fetched_block / fork-replay tests.
3. `cargo clippy --workspace --all-targets -- -D warnings` — clean.
4. `cargo fmt --check` — clean.
5. `./testnet/local-devnet/soak.sh 1800` followed by `./testnet/local-devnet/verify.sh`
   — prints `PASSED all predicates: p1 p2 p3 p4` (no "excluding dugite-bp"
   workaround for p4 once the tip-query fix is also in).
6. `report.md` from the green soak attached to issue #497.
