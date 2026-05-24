# Upstream Conformance Testing — Phase 4 Implementation — HANDOFF

**Date:** 2026-05-23
**Branch:** `worktree-ledger-state-verification-2026-05-23`
**Status:** Patched ImpSpec approach implemented — CI triggered

---

## Completed Phases

| Phase | Status | Evidence |
|-------|--------|---------|
| 0 — Regeneration pipeline bootstrap | Complete | Workflow runs, all 7 area tarballs published to GitHub releases |
| 1 — Foundation (xtask + manifest + test wiring) | Complete | `cargo xtask download-upstream-fixtures`, UPLC corpus migrated |
| 2 — CDDL validation | Complete | Conway tx validated against `conway.cddl` |
| 3 — Typed PParams | Complete | `HaskellPParams<Era>` + `TryFrom` impls |
| 4 — ImpSpec replay | Patched ImpSpec approach ready; awaiting first CI run | See below |
| 5 — VRF/KES crypto vectors | Functional | 7 v03 vectors validated, KES property check passes |
| 6 — Mithril certificate fixtures | Functional | Level 1-3 validation on 4 fixtures |

All CI gates: `cargo nextest run --workspace` passes (6132 tests, 13 skipped).

---

## Phase 4 Status

### Design: Patched ImpSpec (official Haskell test vectors)

The Phase 4 design originally assumed ImpSpec would dump CBOR files for all
test cases. Oracle research confirmed ImpSpec only dumps on Haskell/Agda
**divergences** — which never happen at a stable pinned SHA.

A custom standalone Haskell generator was considered but rejected: generating
our own expected outputs doesn't prove conformance if the test scenarios
aren't from the official suite.

**Current approach: patch the official ImpSpec conformance hooks to dump ALL
test cases when `CONFORMANCE_CBOR_DUMP_PATH` is set.**

Two ImpSpec files are patched by `scripts/regenerate-conformance-corpus/patch-impspec-core.py`:

1. **`ExecSpecRule/Core.hs` — `testConformance`** (QuickCheck property tests)
   - Rules captured: ENACT, DELEG, GOVCERT, POOL, CERT, CERTS, GOV
   - Uses official constrained-generator framework for inputs
   - Haskell STS result is the authoritative expected output

2. **`Imp/Core.hs` — `conformanceHook`** (hook-based ImpSpec tests)
   - Rules captured: NEWEPOCH (every epoch boundary), LEDGER (every tx submission)
   - Inputs are real ImpSpec-generated Cardano states
   - `st_out` is the Haskell STS post-transition state

### Vector format (5 files per test-case directory)

```text
<fixtures>/ledger-rules/<Rule>/<test_name>/
  conformance_dump_ctx.cbor     — ExecContext (CBOR null 0xF6 for NEWEPOCH)
  conformance_dump_env.cbor     — Environment (CBOR null 0xF6 for NEWEPOCH)
  conformance_dump_st.cbor      — initial state (NewEpochState array(7))
  conformance_dump_sig.cbor     — signal (EpochNo for NEWEPOCH; tx CBOR for LEDGER)
  conformance_dump_st_out.cbor  — Haskell expected final state (absent when STS rejects)
```

### Rust runner behavior

- **NEWEPOCH advancing** (`signal_epoch > initial_epoch`): validates epoch invariant
  + verifies `st_out` final epoch equals signal → PASS
- **NEWEPOCH no-op** (`signal <= initial_epoch`): → Skipped
- **LEDGER/UTXO rules**: decodes signal as tx CBOR → PASS/FAIL
- **Unknown rules** (ENACT, DELEG, etc.): → Skipped (no handler yet)

---

## Key File Index

| File | Role |
|------|------|
| `scripts/regenerate-conformance-corpus/patch-impspec-core.py` | Python patch: modifies ExecSpecRule/Core.hs + Imp/Core.hs |
| `scripts/regenerate-conformance-corpus/capture-ledger-rules.sh` | CI capture: clone → patch → build → run → collect |
| `.github/workflows/regenerate-conformance-corpus.yml` | CI workflow: GHC 9.6.5 + libblst + cabal cache |
| `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` | Test runner entry point, SKIP_LIST (currently empty) |
| `tests/conformance/src/upstream/ledger_rules_replay/vector.rs` | 5-file vector reader (st_out is optional) |
| `tests/conformance/src/upstream/ledger_rules_replay/runner.rs` | Rule dispatch: NEWEPOCH + UTXO |
| `tests/conformance/src/upstream/ledger_rules_replay/bridge.rs` | NewEpochState CBOR decoder (all 7 fields) |
| `tests/conformance/upstream/manifest.toml` | Points to current corpus release tag |
| `tests/conformance/upstream/sources.toml` | Pinned cardano-ledger SHA (`ebed62de`) |

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

Should show `stub: False count: N` (N >= 10 expected from NEWEPOCH hooks alone).

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

If any NEWEPOCH or LEDGER tests fail:
- The failure will include the rule name and detail
- File a GitHub issue for each failing rule
- Add to SKIP_LIST in `tests/conformance/src/upstream/ledger_rules_replay/mod.rs`:
  ```rust
  ("NEWEPOCH", "https://github.com/michaeljfazio/dugite/issues/NNN"),
  ```

If all tests pass (or only non-handled rules are skipped): SKIP_LIST stays empty.

---

## ImpSpec Patch Details

### `patch-impspec-core.py` changes to `ExecSpecRule/Core.hs`

- New imports: `Data.IORef`, `System.IO.Unsafe`, `Control.Monad.IO.Class`, `symbolVal`
- Global counter: `conformanceDumpCounter :: IORef Int`
- `testConformance` patched: before calling `checkConformance`, dumps 5 files to
  `$CONFORMANCE_CBOR_DUMP_PATH/<ruleName>/test_<N>/` for every QuickCheck iteration

### `patch-impspec-core.py` changes to `Imp/Core.hs`

- New imports: `IORef`, `writeCBOR`, `createDirectoryIfMissing`, `lookupEnv`, `System.FilePath`
- Global counter: `hookDumpCounter :: IORef Int`
- `conformanceHook` patched: prepends dump block before the existing `impAnn` call.
  Dumps 5 files for every NEWEPOCH epoch boundary and LEDGER tx submission.

### Why the patches work

- `ForAllExecTypes EncCBOR rule era` is part of `ExecSpecRule rule era` — all env/state/signal types have CBOR instances
- `EncCBOR (ExecContext rule era)` is also required by `ExecSpecRule` — ctx is encodable
- `eraProtVerLow @era` is available via `Cardano.Ledger.Core`
- `ImpM t` (the Imp monad) has `MonadIO` — all UnliftIO functions work directly

---

## Haskell CBOR Encoding References (oracle-grounded, 2026-05-23)

| Type | Encoding | Source |
|------|----------|--------|
| `NewEpochState` | `array(7)` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs` |
| `EpochState` | `array(4)` | same file |
| `LedgerState` | `array(2)` = `[CertState, UTxOState]` | same file |
| `UTxOState` | `array(6)` = `[utxo_map, deposited, fees, gov_state, instant_stake, donation]` | same file |
| `SnapShots` | `array(4)` = `[mark, set, go, fee]` | `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` |
| `NonMyopic` | `array(2)` = `[likelihoods_map, reward_pot]` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs` |
| `EncCBOR ()` | `0xF6` (CBOR null) | `cardano-ledger-binary` — `encodeNull` |
| `EpochNo` | bare CBOR uint | `Cardano.Ledger.BaseTypes` |
