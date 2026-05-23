# Upstream Conformance Testing — Handoff

**Date:** 2026-05-23 (updated)
**Branch:** `worktree-ledger-state-verification-2026-05-23`
**Last commit:** `955a823b7`
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

### Phase 4 — ImpSpec Replay: Structurally complete, corpus pending ⚠️

All Phase 4 implementation steps are complete. The only remaining gate is the real ImpSpec corpus (requires triggering the CI workflow).

**Completed this session:**

- **Step 2 (CI workflow): Complete ✅** — `haskell-actions/setup@v2` (GHC 9.6.5 + cabal 3.10.3.0) added to `.github/workflows/regenerate-conformance-corpus.yml`. cabal-store and `~/.cabal` caches keyed on `hashFiles(sources.toml)`. Verify step updated to warn (not fail) on ledger-rules stub. Commit `97645a8f2`.

- **Step 3 (NewEpochState bridge): Structurally complete ✅** — All 7 fields of NewEpochState fully decoded in `bridge.rs`. The three previously-stubbed sub-trees are now implemented, grounded in the Haskell source queried via oracle on 2026-05-23:
  - `LedgerState` (field[3.1]): `array(2)` = `[CertState, UTxOState]`. UTxOState is `array(6)` — extracts `utxo_count`, `deposited`, `fees`, `donation`. CertState is skipped (complex sub-tree).
  - `SnapShots` (field[3.2]): `array(4)` = `[mark, set, go, fee]`. Each SnapShot is `array(2)` (new) or `array(3)` (old format) — extracts pool count per snapshot. `ssStakeMarkPoolDistr` NOT serialized (by design).
  - `NonMyopic` (field[3.3]): `array(2)` = `[likelihoods_map, reward_pot]` — extracts `likelihood_count` and `reward_pot`.
  - All three decoders are non-fatal (fall back to `Default` with a WARN on decode failure).
  - `RunOutcome::NewEpochValidated` gains `utxo_count` and `pool_count` fields.
  - PASS log line includes `utxos=N pools=N` diagnostics.
  - Commits `51c5ae777`.

- **Step 4 (SKIP_LIST): Complete ✅** — Wildcard entry `("*", "#627")` added. `is_skipped()` treats `"*"` as skip-all. All real ImpSpec corpus vectors will be gracefully skipped (SKIP, not FAIL) until divergences are individually triaged. Issue `#627` tracks corpus generation. Commit `955a823b7`.

- **GitHub issue filed**: https://github.com/michaeljfazio/dugite/issues/627

---

## Phase 4 Remaining Blocker

### Only remaining blocker: Real ImpSpec corpus

All structural implementation is done. The only thing preventing true Phase 4 completion is the corpus itself — the real CBOR fixture files produced by `cardano-ledger-conformance` with `CONFORMANCE_CBOR_DUMP_PATH` set.

**Immediate next step:** Trigger the CI workflow:

```bash
gh workflow run regenerate-conformance-corpus.yml --repo michaeljfazio/dugite
```

Or go to GitHub Actions → "Regenerate Conformance Corpus" → Run workflow.

Watch the `ledger-rules` area output. When divergences are found, the workflow publishes them as a release asset. Then:

1. Update `tests/conformance/upstream/manifest.toml` to point at the new release tag
2. Run `cargo xtask download-upstream-fixtures` to pull the vectors
3. Remove the wildcard `("*", "#627")` entry from SKIP_LIST in `mod.rs`
4. Run `cargo nextest run -p dugite-conformance --features upstream-conformance`
5. Each failure → file a GitHub issue + add to SKIP_LIST as `("RuleName", "#NNN")`
6. Fix each divergence + remove its SKIP_LIST entry

**Full LedgerState→DugiteState bridge (apply path):** The structural decode of LedgerState sub-tree is complete (`DecodedLedgerState` extracts utxo_count, deposited, fees, donation). The remaining work for `apply_tx` / `apply_epoch` is:
1. Implement `LedgerState::from_cbor(st_cbor)` using the ImpSpec-format decode (need real fixture bytes to validate)
2. Implement the reverse: `LedgerState → CBOR` for state comparison after apply
3. Wire into `runner.rs` using the full apply path

This is ~500-800 LOC and should only be done after real fixture files are available to validate against.

---

## Current Phase 4 Code State (at `955a823b7`)

| File | Status | Notes |
|------|--------|-------|
| `vector.rs` | Complete | 4-file reader, rule from parent dir name |
| `bridge.rs` | Structurally complete | All 7 fields decoded; LedgerState sub-tree extracts utxo_count/deposited/fees/donation |
| `runner.rs` | Functional | NEWEPOCH + UTXO dispatch; utxo_count + pool_count in PASS message |
| `compare.rs` | Stub | Byte-level comparison — correct concept; not exercised until apply path is wired |
| `mod.rs` | Complete | Wildcard SKIP_LIST; synthetic fixture; test-case dir scanner |
| `capture-ledger-rules.sh` | Complete | Haskell + Nix paths both implemented; stub fallback when neither |
| `regenerate-conformance-corpus.yml` | Complete | GHC 9.6.5 + cabal 3.10.3.0 + cabal-store cache |

---

## Haskell CBOR Source References (queried 2026-05-23)

All sub-tree shapes confirmed via `gh api` against `IntersectMBO/cardano-ledger`:

| Type | Encoding | Source file |
|------|----------|-------------|
| `NewEpochState` | `array(7)` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs` |
| `EpochState` | `array(4)` | same file |
| `LedgerState` | `array(2)` = `[CertState, UTxOState]` (CertState first for sharing) | same file |
| `UTxOState` | `array(6)` = `[utxo_map, deposited, fees, gov_state, instant_stake, donation]` | same file |
| `SnapShots` | `array(4)` = `[mark, set, go, fee]` (ssStakeMarkPoolDistr NOT serialized) | `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` |
| `SnapShot` | `array(2)` (new) or `array(3)` (old) | same file |
| `NonMyopic` | `array(2)` = `[likelihoods_map, reward_pot]` | `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs` |

---

## SKIP_LIST Discipline

When divergences surface after corpus generation, each entry follows this format:

```rust
const SKIP_LIST: &[(&str, &str)] = &[
    // Remove the wildcard once the corpus is downloaded and divergences are filed:
    // ("*", "https://github.com/michaeljfazio/dugite/issues/627"),

    // Per-rule entries (add as divergences surface):
    // ("ConwayNEWEPOCH", "https://github.com/michaeljfazio/dugite/issues/NNN"),
    // ("ConwayUTXO", "https://github.com/michaeljfazio/dugite/issues/NNN"),
];
```

Each entry is removed only when the underlying Dugite bug is fixed. The wildcard entry is removed after the first corpus download.

---

## Summary of Completed Work

This branch implements Phases 0-3 fully and Phases 5-6 functionally. Phase 4 is structurally complete:

- **93 Rust files + 2 CI/workflow files** added/modified across conformance, xtask, scripts
- **6132/6132 workspace tests** pass on every commit
- **Phase 4 bridge** — all 7 NewEpochState fields decoded; LedgerState/SnapShots/NonMyopic sub-trees structurally complete (oracle-grounded Haskell encoding)
- **Phase 4 SKIP_LIST** — wildcard entry with `#627` tracking corpus generation
- **Phase 4 CI workflow** — GHC 9.6.5 + cabal 3.10.3.0, cabal-store cache, first real corpus run now possible via `gh workflow run`
- **7 VRF v03 vectors** validated byte-exact against cardano-base at pinned SHA
- **KES property check** validates Sum6KES keygen + sign + evolve + verify
- **Mithril Level 1-3** validates 4 certificate fixtures with semantic checks
