# Upstream Conformance Testing — Phase 4 Halt — HANDOFF

**Date:** 2026-05-23
**Branch:** `worktree-ledger-state-verification-2026-05-23`
**Last commit:** (current HEAD after this commit)
**Status:** Condition (b) halt — Phase 4 design requires product-owner redesign decision

---

## Completed Phases

| Phase | Status | Evidence |
|-------|--------|---------|
| 0 — Regeneration pipeline bootstrap | Complete | Workflow runs, all 7 area tarballs published to GitHub releases |
| 1 — Foundation (xtask + manifest + test wiring) | Complete | `cargo xtask download-upstream-fixtures`, UPLC corpus migrated |
| 2 — CDDL validation | Complete | Conway tx validated against `conway.cddl` |
| 3 — Typed PParams | Complete | `HaskellPParams<Era>` + `TryFrom` impls |
| 4 — ImpSpec replay | Infrastructure complete, corpus blocked by design | See below |
| 5 — VRF/KES crypto vectors | Functional | 7 v03 vectors validated, KES property check passes |
| 6 — Mithril certificate fixtures | Functional | Level 1-3 validation on 4 fixtures |

All CI gates: `cargo nextest run --workspace` passes (6132+ tests).

---

## Phase 4 Status

### What is implemented (ready for real vectors)

- **4-file vector format**: `vector.rs` reads `conformance_dump_{ctx,env,st,sig}.cbor` from per-test directories
- **Full NewEpochState structural bridge**: `bridge.rs` decodes all 7 fields including LedgerState (UTxO count, deposits, fees), SnapShots (mark/set/go pool counts), NonMyopic (likelihood count, reward pot)
- **Runner**: NEWEPOCH epoch-invariant check + UTXO tx decode; logs treasury/reserves/utxos/pools from initial state
- **GitHub Actions CI workflow**: GHC 9.6.5 + cabal 3.10.3.0 set up in `regenerate-conformance-corpus.yml`
- **Synthetic fixture**: `ConwayNEWEPOCH/test_minimal_epoch_advance` exercises full decode path
- **SKIP_LIST**: empty (no pending entries — no corpus vectors exist)
- **GitHub Issue #627**: tracks the corpus generation requirement

### The blocking design error (confirmed by cardano-haskell-oracle)

The Phase 4 spec (`docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md`) assumes that running `CONFORMANCE_CBOR_DUMP_PATH=/path cabal test cardano-ledger-conformance` produces fixture files. This assumption is incorrect.

Oracle research on SHA `ebed62de1ebcd4b13512418d49d17802a193e2c1`,
function `checkConformance` in
`libs/cardano-ledger-conformance/src/Test/Cardano/Ledger/Conformance/ExecSpecRule/Core.hs`:

```haskell
case (implResNorm, agdaResNorm) of
    (Right agda, Right impl)
      | agda == impl -> pure ()   -- MATCH: no dump
    (Left _, Left _) -> pure ()   -- BOTH FAIL: no dump
    (agda, impl) -> do            -- DIVERGENCE ONLY: dump fires
      ...
      CONFORMANCE_CBOR_DUMP_PATH -> dumpCbor ...
```

The dump fires ONLY when the Haskell result diverges from the Agda spec result.
The reference implementation at any stable pinned SHA passes all its own ImpSpec tests — that is
what makes it the reference. Therefore:

- Running ImpSpec on the pinned SHA produces ZERO dump files
- Phase 4's capture approach as designed literally cannot produce fixture vectors

### Additional confirmed finding: capture script used wrong invocation

The previous `capture-ledger-rules.sh` used:
```bash
cabal test cardano-ledger-conformance \
    --test-options "--dump-path ${DUMP_DIR}"
```

This was doubly wrong:
1. `--dump-path` is not a valid test option; only the `CONFORMANCE_CBOR_DUMP_PATH` env var works
2. Even with the correct env var, no dumps are produced from a correct implementation

The script has been corrected (env var, streaming output) but the fundamental issue
remains: the stable SHA produces no divergences and therefore no vectors.

### Additional confirmed finding: dump output is flat

Oracle research also confirmed:
- Dumps are written flat (all 4 files in one directory, overwriting on multiple failures)
- Only the LAST diverging test case per run survives — not suitable as a multi-vector corpus even if divergences existed

### Why this is condition (b)

The spec's Phase 4 design is based on an incorrect architectural assumption about ImpSpec.
Fixing this requires redesigning the capture approach — a product-owner-level decision that
determines the shape of all downstream implementation work.

---

## Alternative Approaches (for product-owner consideration)

### Option A: Standalone Haskell fixture generator (Recommended)

Write a small Haskell executable (or cabal executable in the regeneration pipeline) that:
1. Instantiates specific Conway STS rules with known inputs
2. Runs the Haskell STS transition
3. Serializes: initial state (NewEpochState or UTxOState etc.) + signal + result
4. Writes 4 CBOR files per test case

This does not depend on ImpSpec divergences at all. The generator defines its own test scenarios
with full control over inputs. The output is fully compatible with the existing `vector.rs` reader.

**Estimated:** 3-5 days (Haskell expertise required); produces controlled, deterministic fixture files.

**Why recommended:** Low complexity, deterministic, no dependency on ImpSpec internals,
direct compatibility with the existing 4-file vector format and `bridge.rs` decoder.

### Option B: QuickCheck-based fixture generator

Write a QuickCheck generator that:
1. Generates random valid (initial_state, signal) pairs using cardano-ledger's constrained generators
2. Runs the Haskell STS transition on each
3. Dumps the (initial_state, signal, result) triple as CBOR

This produces large corpora of random-input fixtures. The SKIP_LIST handles any Dugite divergences.

**Estimated:** 5-7 days; produces high-coverage fixtures but non-deterministic (seed-pinned for reproducibility).

### Option C: Formal spec dump (Agda/MAlonzo)

Invoke the Agda-compiled Haskell interface (`cardano-ledger-executable-spec`) directly to generate
transitions from the formal spec side. More complex but tests Dugite against the Agda spec,
not just the Haskell implementation.

**Estimated:** 7-10 days; highest conformance value but most complex.

### Option D: Abandon ImpSpec approach; hand-craft Conway vectors

Write Conway ledger state transitions as hand-crafted CBOR blobs using oracle knowledge of the
encoding. Slow but requires no Haskell toolchain in the CI pipeline.

**Estimated:** 2-3 weeks for meaningful coverage; brittle to format changes.

---

## Recommended Next Steps

1. **Product owner decision**: Choose an alternative approach from the list above.
   Option A is recommended (standalone Haskell fixture generator).

2. **Redesign capture script**: Implement the chosen approach in
   `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh`
   (or a new Haskell executable under `tools/ledger-fixture-gen/`).

3. **Update spec**: Correct `docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md`
   Phase 4 section to reflect the ImpSpec limitation and the chosen alternative approach.

4. **When real fixtures exist**:
   - Add per-rule entries to SKIP_LIST in `tests/conformance/src/upstream/ledger_rules_replay/mod.rs`
     for any Dugite divergences, each with a tracking issue URL
   - The wildcard has already been removed; SKIP_LIST is now correctly empty

---

## Current Phase 4 Code State

| File | Status | Notes |
|------|--------|-------|
| `tests/conformance/src/upstream/ledger_rules_replay/vector.rs` | Complete | 4-file reader, rule from parent dir name |
| `tests/conformance/src/upstream/ledger_rules_replay/bridge.rs` | Structurally complete | All 7 NewEpochState fields; LedgerState sub-tree extracts utxo_count/deposited/fees/donation |
| `tests/conformance/src/upstream/ledger_rules_replay/runner.rs` | Functional | NEWEPOCH + UTXO dispatch; utxo_count + pool_count in PASS message |
| `tests/conformance/src/upstream/ledger_rules_replay/compare.rs` | Stub | Byte-level comparison ready; not exercised until apply path is wired |
| `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` | Updated | SKIP_LIST empty + correct comment; wildcard and `"*"` special-case removed |
| `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` | Updated | Correct env var; documents ImpSpec dump semantics; stub-fallback preserved |
| `.github/workflows/regenerate-conformance-corpus.yml` | Complete | GHC 9.6.5 + cabal 3.10.3.0 + cabal-store cache |

---

## Files That Need Changes for the Chosen Approach

| File | Action |
|------|--------|
| `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` | Rewrite capture mechanism per chosen option |
| `docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md` | Correct Phase 4 section |
| `tests/conformance/upstream/sources.toml` | May need new SHA/config for the generator |
| (new) `tools/ledger-fixture-gen/` | Haskell fixture generator (Options A/B/C) |

---

## Haskell CBOR Source References (oracle-grounded, 2026-05-23)

All sub-tree shapes confirmed via `gh api` against `IntersectMBO/cardano-ledger`:

| Type | Encoding | Source file |
|------|----------|-------------|
| `NewEpochState` | `array(7)` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs` |
| `EpochState` | `array(4)` | same file |
| `LedgerState` | `array(2)` = `[CertState, UTxOState]` | same file |
| `UTxOState` | `array(6)` = `[utxo_map, deposited, fees, gov_state, instant_stake, donation]` | same file |
| `SnapShots` | `array(4)` = `[mark, set, go, fee]` (ssStakeMarkPoolDistr NOT serialized) | `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` |
| `SnapShot` | `array(2)` (new) or `array(3)` (old) | same file |
| `NonMyopic` | `array(2)` = `[likelihoods_map, reward_pot]` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs` |
| `checkConformance` dump gate | divergence-only (Haskell != Agda result) | `libs/cardano-ledger-conformance/src/Test/Cardano/Ledger/Conformance/ExecSpecRule/Core.hs` |
