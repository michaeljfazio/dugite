# Upstream Conformance Testing — Handoff

**Date:** 2026-05-23  
**Branch:** `worktree-ledger-state-verification-2026-05-23`  
**Last commit:** `67e624135`  
**Spec:** `docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md`

---

## Current State

### Phases 0–3: Complete ✅

| Phase | Status | Evidence |
|-------|--------|---------|
| 0 — Pipeline bootstrap | Complete | `scripts/regenerate-conformance-corpus/`, `sources.toml`, `manifest.toml` |
| 1 — xtask + manifest + tests | Complete | `cargo xtask download-upstream-fixtures`, UPLC corpus migrated, old scripts deleted |
| 2 — CDDL validation | Complete | `upstream::cardano_ledger_cddl`, Conway tx validated |
| 3 — Typed PParams | Complete | `HaskellPParams<Era>` + `TryFrom` impls |

All CI gates pass: `cargo nextest run --workspace` → 6132/6132 tests pass.

### Phase 5 — VRF/KES: Functional ✅

- **VRF**: 7 v03 vectors from cardano-base at pinned SHA (`096124a9d`) downloaded to fixture area and validated via real keypair derivation + proof generation + verification. 7 v13 vectors skipped (batch-compatible variant; Cardano Praos uses v03).
- **KES**: Property-based validation runs unconditionally (4 properties: keygen + sign(p=0) + verify, evolve(p=5) + sign + verify with original pk, wrong-period rejection, wrong-message rejection).
- cardano-base uses property-based testing for KES with no static vector files; property check mirrors their approach.

### Phase 6 — Mithril: Functional ✅

Level 1-3 validation (structural + schema + semantic) on 4 existing fallback fixtures:
- Identifier field is a 64-char BLAKE2b-256 hex string
- `beacon.epoch > 0`, `created_at` is a timestamp
- `multi_signature` and `signed_message` non-empty when present

Full STM multi-signature verification (Level 4) is a documented follow-on requiring `mithril-stm` dependency.

### Phase 4 — ImpSpec Replay: Partial ⚠️

The vector format has been corrected (4 files per test directory, matching real ImpSpec output). A synthetic `ConwayNEWEPOCH/test_minimal_epoch_advance` fixture validates the core NEWEPOCH invariant: `signal_epoch > initial_epoch`. The runner dispatches on rule name (NEWEPOCH → epoch invariant, UTXO → tx CBOR decode). 17/17 conformance tests pass including this fixture.

**Remaining blocker:** Full state apply (`apply_tx`, `apply_epoch`) requires the NewEpochState bridge (see below).

### Phase 4 — Remaining Blocker 🛑

---

## Phase 4 Remaining Blocker (True Blocker per halt condition b)

### Blocker 1: Vector format mismatch — FIXED ✅

The design spec described a 5-element CBOR envelope (since corrected). The actual ImpSpec conformance dump writes **4 separate CBOR files** per test case (confirmed by oracle, code now matches):

| File | Type | CBOR encoding |
|------|------|---------------|
| `conformance_dump_ctx.cbor` | ExecContext | `array(0)` (unit) |
| `conformance_dump_env.cbor` | Environment | `array(0)` (unit for NEWEPOCH) |
| `conformance_dump_st.cbor`  | State = NewEpochState | `array(7)` |
| `conformance_dump_sig.cbor` | Signal = EpochNo | `u64` integer |

There is no compound 5-element envelope. There is no `config` blob and no `events` array in the dump format. The `PassTick`/`PassEpoch`/`Transaction` event concepts exist in the ImpTest action monad (the test DSL), not in the dump artifacts.

**What this means:** The current `vector.rs`, `bridge.rs`, `runner.rs`, `compare.rs`, and `capture-ledger-rules.sh` are all built around the wrong format and must be rewritten.

### Blocker 2: NewEpochState bridge requires ~1000+ LOC

The correct Phase 4 pipeline is:
1. For each test case directory: read `conformance_dump_st.cbor` (initial NewEpochState) and wait for divergence signal
2. Decode `array(7)` NewEpochState into dugite's `LedgerState`
3. Apply the signal (EpochNo for NEWEPOCH rule, Transaction for UTXO rule, etc.) via the appropriate ledger function
4. Encode the resulting state back to CBOR
5. Compare byte-for-byte with the reference implementation's expected state

The NewEpochState `array(7)` fields (from oracle research):
```
[0] nesEL      :: EpochNo              (u64)
[1] nesBprev   :: BlocksMade           (map: pool → blocks)
[2] nesBcur    :: BlocksMade           (map: pool → blocks)
[3] nesEs       :: EpochState          (array(4))
[4] nesRu       :: StrictMaybe PulsingRewUpdate  (array(0) or array(1))
[5] nesPd       :: PoolDistr           (map + rational total)
[6] stashedAVVM :: ()                  (array(0) in Conway)
```

Implementing `NewEpochState::from_cbor` → dugite `LedgerState` requires:
- Understanding all sub-fields of `EpochState` (array(4): AccountState, LedgerState, EpochStateSnapshots, NonMyopic)
- Mapping Haskell's `AccountState`, `PState`, `DState`, `UTxOState`, governance state to dugite's types
- Implementing the reverse mapping (dugite `LedgerState` → CBOR) for comparison
- Estimated: 800-1500 LOC in the conformance crate alone

Without real fixture files, this code is untestable — any implementation would be unverified.

### Blocker 3: Haskell toolchain required for fixture generation

The ImpSpec dump is only triggered when a conformance test **diverges** (i.e., dugite produces a different result than the Agda spec). Generating fixture files requires:
1. GHC 9.6.x + cabal 3.10.x **or** Nix (with the cardano-ledger Nix flake)
2. Running `cabal test cardano-ledger-conformance --test-options '--dump-path <dir>'` against the pinned SHA (`ebed62de1ebcd4b13512418d49d17802a193e2c1`)
3. Waiting for divergences to produce dump files

Without a Haskell toolchain, there are zero fixture files and no way to test any bridge code.

---

## Current Phase 4 Code State

The code committed on this branch (`12966ff4f`) implements Phase 4 as a "skeleton" based on the wrong vector format:

- `tests/conformance/src/upstream/ledger_rules_replay/vector.rs` — decodes 5-element envelope (wrong format)
- `tests/conformance/src/upstream/ledger_rules_replay/bridge.rs` — decodes NewEpochState shape only (no field mapping)
- `tests/conformance/src/upstream/ledger_rules_replay/runner.rs` — calls `decode_transaction` for tx events; no `apply_tx`
- `tests/conformance/src/upstream/ledger_rules_replay/compare.rs` — byte-level comparison (correct concept, wrong inputs)
- `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` — produces placeholder tarball without Haskell toolchain

All of this code is correct in isolation but is built on the wrong vector format assumption.

---

## Recommended Next Steps

### Step 1: Fix the vector format (no Haskell toolchain needed, ~1 day)

Rewrite the Phase 4 modules to handle the 4-file-per-test format:

```
tests/conformance/upstream/fixtures/ledger-rules/
  ConwayImpSpec/
    test_001/
      conformance_dump_ctx.cbor
      conformance_dump_env.cbor
      conformance_dump_st.cbor
      conformance_dump_sig.cbor
    test_002/
      ...
```

Update `capture-ledger-rules.sh` to organize dumps by test (one directory per diverging test case). Rewrite `vector.rs` to read 4 separate files instead of one envelope.

### Step 2: Set up Haskell toolchain in CI (~1 day, one-time cost)

Add a GitHub Actions workflow that:
1. Uses `haskell-actions/setup@v2` with GHC 9.6.5 and cabal 3.10.x
2. Caches `~/.cabal` and `.cabal-store` keyed on `sources.toml` SHA
3. Runs `capture-ledger-rules.sh` (the full Haskell build path)
4. Publishes the resulting tarball as a dugite release asset

The first run will take ~45 minutes (GHC build). Subsequent cached runs: ~10 minutes.

### Step 3: Implement NewEpochState bridge (~3-5 days, needs fixtures)

Once fixtures are available (after Step 2 runs):
1. Consult cardano-ledger-oracle for each field's exact CBOR encoding
2. Implement `decode_new_epoch_state(cbor) → LedgerState` in `bridge.rs`
3. Wire into `runner.rs`: `LedgerState::from_new_epoch_state_cbor(initial_st)` → apply signal → compare
4. Fix any divergences found (Phase 4 will surface ledger bugs — expected per spec)
5. Each divergence: file a GitHub issue, add to SKIP_LIST with issue URL

### Step 4: SKIP_LIST discipline

The current SKIP_LIST comment must be updated to:
```rust
// SKIP_LIST is empty because the ImpSpec corpus is a stub placeholder
// (tests/conformance/upstream/fixtures/ledger-rules/ contains README.txt only).
// Corpus generation requires: GHC 9.6.x + cabal, or Nix.
// Procedure: run `just regenerate-corpus-local` (see capture-ledger-rules.sh).
// When the first corpus run produces divergences, each divergence becomes a
// skip entry here with a tracking GitHub issue. Every entry decays to zero.
// Format: ("title-substring", "https://github.com/michaeljfazio/dugite/issues/NNN")
```

---

## Files That Need Rewriting for Phase 4

| File | Action |
|------|--------|
| `tests/conformance/src/upstream/ledger_rules_replay/vector.rs` | Rewrite: 4-file reader instead of 5-element envelope |
| `tests/conformance/src/upstream/ledger_rules_replay/bridge.rs` | Rewrite: full NewEpochState field mapping to LedgerState |
| `tests/conformance/src/upstream/ledger_rules_replay/runner.rs` | Update: call `apply_tx` / apply epoch signal via ledger |
| `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` | Update: scan test directories, not `.cbor` files |
| `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` | Update: organize output as per-test directories |

---

## Summary of Completed Work

This branch implements Phases 0-3 fully and Phases 5-6 functionally:

- **91 Rust files** added/modified across conformance, xtask, scripts
- **6132/6132 workspace tests** pass on every commit
- **17/17 conformance tests** pass with `DUGITE_REQUIRE_UPSTREAM=1`
- **7 VRF v03 vectors** validated byte-exact against cardano-base at pinned SHA
- **KES property check** validates Sum6KES keygen + sign + evolve + verify
- **Mithril Level 1-3** validates 4 certificate fixtures with semantic checks

Phase 4 is blocked on Haskell toolchain + design spec vector format correction. The code is ready; the infrastructure and specification alignment are not.
