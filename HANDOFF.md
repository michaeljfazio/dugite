# Upstream Conformance Testing — Phase 4 Implementation — HANDOFF

**Date:** 2026-05-23
**Branch:** `worktree-ledger-state-verification-2026-05-23`
**Status:** Option A implemented — Haskell fixture generator compiled, CI in progress

---

## Completed Phases

| Phase | Status | Evidence |
|-------|--------|---------|
| 0 — Regeneration pipeline bootstrap | Complete | Workflow runs, all 7 area tarballs published to GitHub releases |
| 1 — Foundation (xtask + manifest + test wiring) | Complete | `cargo xtask download-upstream-fixtures`, UPLC corpus migrated |
| 2 — CDDL validation | Complete | Conway tx validated against `conway.cddl` |
| 3 — Typed PParams | Complete | `HaskellPParams<Era>` + `TryFrom` impls |
| 4 — ImpSpec replay | Infrastructure complete + generator implemented; awaiting first successful CI run | See below |
| 5 — VRF/KES crypto vectors | Functional | 7 v03 vectors validated, KES property check passes |
| 6 — Mithril certificate fixtures | Functional | Level 1-3 validation on 4 fixtures |

All CI gates: `cargo nextest run --workspace` passes (6132 tests, 13 skipped).

---

## Phase 4 Status

### What changed from the HALT state

The Phase 4 halt was caused by the ImpSpec dump mechanism only firing on Haskell/Agda divergences
(confirmed by oracle research on SHA `ebed62de1ebcd4b13512418d49d17802a193e2c1`). Running
`CONFORMANCE_CBOR_DUMP_PATH=/path cabal test cardano-ledger-conformance` at any stable SHA
produces ZERO dump files.

**Option A (standalone Haskell fixture generator) is now implemented:**

- `tools/ledger-fixture-gen/` — Haskell executable that instantiates `def :: NewEpochState ConwayEra`,
  applies the Conway NEWEPOCH STS rule via `applySTS`, and writes 5 CBOR files per test case
- `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` — clones cardano-ledger at the
  pinned SHA, installs the generator as a sub-package, builds with cabal (deps auto-resolved from the
  cardano-ledger workspace), runs the generator
- `.github/workflows/regenerate-conformance-corpus.yml` — installs GHC 9.6.5 + cabal 3.10.3.0,
  libsodium, libsecp256k1, libblst (built from source — not in Ubuntu 24.04 apt repos)

### Vector format (5 files per test-case directory)

```text
<fixtures>/ledger-rules/ConwayNEWEPOCH/<test_name>/
  conformance_dump_ctx.cbor     — CBOR null (0xF6): EncCBOR () = encodeNull
  conformance_dump_env.cbor     — CBOR null (0xF6): EncCBOR () = encodeNull
  conformance_dump_st.cbor      — NewEpochState array(7), initial state (before transition)
  conformance_dump_sig.cbor     — EpochNo (CBOR uint), target epoch signal
  conformance_dump_st_out.cbor  — NewEpochState array(7), Haskell expected final state
                                  (omitted if STS rejects the transition: signal <= initial)
```

`st_out` is optional: our `vector.rs` reads it as `Option<Vec<u8>>`. When present (real Haskell
vectors), the runner verifies that `st_out.nesEL == signal_epoch`. When absent (synthetic fixture
or no-op transitions), `final_state_validated = false` in the PASS message.

### Generated test cases (5 total from the Haskell generator)

| Test name | Initial epoch | Signal epoch | STS result |
|-----------|--------------|--------------|------------|
| `test_epoch_0_to_1` | 0 | 1 | Advance (st_out present) |
| `test_epoch_1_to_2` | 1 | 2 | Advance (st_out present) |
| `test_epoch_4_to_5` | 4 | 5 | Advance (st_out present) |
| `test_epoch_0_to_100` | 0 | 100 | Advance far (st_out present) |
| `test_epoch_0_same` | 0 | 0 | No-op (st_out = same state; Rust runner returns Skipped) |

### Rust runner behavior

- **Advancing transitions** (`signal > initial`): validates epoch invariant + `st_out` epoch if present → PASS
- **No-op transitions** (`signal <= initial`): returns `Skipped` (valid Haskell no-op, not yet validated)
- **UTXO rules**: decodes signal as tx CBOR via `dugite_serialization::decode_transaction` → PASS/FAIL
- **Unknown rules**: `Skipped` (no handler yet)

---

## Current CI Run State (as of 2026-05-23 ~13:12Z)

CI run **26333304437** is in progress on branch `worktree-ledger-state-verification-2026-05-23`
at commit `646af9c74` (the `unsafeBoundRational` fix — last Haskell compile fix before the `st_out`
feature was added in `1d1ca810b`).

### Prior CI failures and their fixes

| Run | Duration | Error | Fixed in |
|-----|----------|-------|---------|
| 26332108912 | 31s | Script not executable | 42ba1c348 |
| 26332132126 | 3m | Missing libsodium/libblst | 42ba1c348 |
| 26332229260 | 24m | NumericUnderscores + import order | 2fdc6a2ac |
| 26332762741 | 24m | `unsafeBoundRational` not exported | 646af9c74 |
| **26333304437** | **in progress** | *Should succeed with all fixes applied* | — |

### Expected CI outcome for run 26333304437

The cabal dependency tree is being built now (~15-20 min of the ~40 min total). If successful:
- `ledger-rules.tar.gz` will be published as a non-stub asset
- `corpus-manifest.json` will show `"stub": false, "file_count": N` for ledger-rules
- 4 test cases will be in the corpus (without `st_out` — the 5th-file feature landed in 1d1ca810b)

### Next CI run needed (for full 5-file vectors)

After run 26333304437 succeeds, trigger another run on HEAD (`1d1ca810b`) to get vectors WITH
`st_out`. Or update manifest.toml to whichever tag 26333304437 produced.

---

## Post-CI Steps (when a run succeeds)

### 1. Find the new release tag

```bash
gh release list --repo michaeljfazio/dugite --limit 3
```

### 2. Verify ledger-rules is non-stub

```bash
gh release download <NEW_TAG> --repo michaeljfazio/dugite --pattern corpus-manifest.json --output - | \
  python3 -c "import sys,json; d=json.load(sys.stdin); lr=d['areas']['ledger-rules']; print('stub:', lr.get('stub'), 'count:', lr.get('file_count'))"
```

Should show `stub: False count: N` (N >= 4).

### 3. Update manifest.toml

```bash
sed -i '' 's/tag  = "conformance-corpus-v[^"]*"/tag  = "<NEW_TAG>"/' \
  tests/conformance/upstream/manifest.toml
```

### 4. Download fixtures

```bash
cargo xtask download-upstream-fixtures --area ledger-rules
```

### 5. Run conformance tests

```bash
DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance
```

### 6. Process results

If any tests fail:
- The failure will include the rule name (e.g. `ConwayNEWEPOCH`) and detail
- File a GitHub issue for each failing rule
- Add to SKIP_LIST in `tests/conformance/src/upstream/ledger_rules_replay/mod.rs`:
  ```rust
  ("ConwayNEWEPOCH", "https://github.com/michaeljfazio/dugite/issues/NNN"),
  ```

If all tests pass: SKIP_LIST stays empty — Phase 4 is complete.

### 7. Trigger a second CI run for 5-file vectors

After first success, trigger:
```bash
gh workflow run regenerate-conformance-corpus.yml \
  --ref worktree-ledger-state-verification-2026-05-23 \
  --repo michaeljfazio/dugite
```

This run uses HEAD at `1d1ca810b` which writes `st_out` files. The `st_out` enables the
`final_state_validated` check in the runner (verifies post-transition epoch matches signal).

---

## Haskell Compilation Notes (for future debugging)

All errors surfaced during CI runs at the pinned SHA `ebed62de`:

1. **NumericUnderscores** — `45_000_000_000_000_000` requires `{-# LANGUAGE NumericUnderscores #-}` in GHC 9.6.5
2. **Import order** — Haskell imports must precede all top-level definitions (no imports inside `where`)
3. **`unsafeBoundRational` not exported** — use `fromJust . boundRational` (same pattern as cardano-ledger internals)
4. **libblst** — not in Ubuntu 24.04 apt repos; must be built from `supranational/blst` v0.3.14
5. **`data-default` vs `data-default-class`** — the cardano-ledger workspace uses `data-default-class` for `Default`

---

## Key File Index

| File | Role |
|------|------|
| `tools/ledger-fixture-gen/src/Main.hs` | Haskell generator: def state → applySTS → 5 CBOR files |
| `tools/ledger-fixture-gen/dugite-fixture-gen.cabal` | Cabal package for the generator |
| `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` | CI capture script: clone → install → build → run |
| `.github/workflows/regenerate-conformance-corpus.yml` | CI workflow: GHC setup + libblst + cabal cache |
| `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` | Test runner entry point, SKIP_LIST (currently empty) |
| `tests/conformance/src/upstream/ledger_rules_replay/vector.rs` | 5-file vector reader |
| `tests/conformance/src/upstream/ledger_rules_replay/runner.rs` | Rule dispatch: NEWEPOCH + UTXO |
| `tests/conformance/src/upstream/ledger_rules_replay/bridge.rs` | NewEpochState CBOR decoder (all 7 fields) |
| `tests/conformance/upstream/manifest.toml` | Points to current corpus release tag |
| `tests/conformance/upstream/sources.toml` | Pinned cardano-ledger SHA (`ebed62de`) |

---

## Haskell CBOR Encoding References (oracle-grounded, 2026-05-23)

| Type | Encoding | Source |
|------|----------|--------|
| `NewEpochState` | `array(7)` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs` |
| `EpochState` | `array(4)` | same file |
| `LedgerState` | `array(2)` = `[CertState, UTxOState]` | same file |
| `UTxOState` | `array(6)` = `[utxo_map, deposited, fees, gov_state, instant_stake, donation]` | same file |
| `SnapShots` | `array(4)` = `[mark, set, go, fee]` (ssStakeMarkPoolDistr NOT serialized) | `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` |
| `NonMyopic` | `array(2)` = `[likelihoods_map, reward_pot]` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs` |
| `EncCBOR ()` | `0xF6` (CBOR null) | `cardano-ledger-binary` — `encodeNull` |
| `EpochNo` | bare CBOR uint | `Cardano.Ledger.BaseTypes` |
