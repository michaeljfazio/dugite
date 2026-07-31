# Conformance Test Suite

Dugite's correctness story rests on three layers:

1. **Conformance** — byte-exact alignment vs upstream Cardano artefacts, verified by replay. This page.
2. **Feature compatibility** — what protocol features the node implements. See the wiki [Protocol Compliance](https://github.com/michaeljfazio/dugite/wiki/Protocol-Compliance) page.
3. **Operational soak testing** — sustained behaviour on live testnets (preview, preprod) and the local devnet.

This page documents the conformance suite: where the upstream fixtures come from, what each area validates, and how to replay any of them locally.

## The corpus model

```
upstream repos (SHA-pinned in sources.toml)
       │
       ▼
regenerate-conformance-corpus workflow
       │  produces 7 tarballs
       ▼
dugite GitHub release (tag pinned in manifest.toml)
       │
       ▼
just download-upstream-fixtures
       │
       ▼
dugite-conformance test harness (DUGITE_REQUIRE_UPSTREAM=1)
```

Upstream sources are pinned by commit SHA (or tag) in `tests/conformance/upstream/sources.toml`. The `regenerate-conformance-corpus` workflow consumes those pins, materialises the seven fixture areas into tarballs, and publishes them as assets of a single dugite GitHub release. Consumers (CI and local developers alike) then pin to that *release tag* via `tests/conformance/upstream/manifest.toml`, so a single fetch lands every fixture area at a known good combination. Tarballs are cached by content hash of `manifest.toml`, so bumping the tag invalidates the cache automatically.

This two-level pinning separates "what upstream version we test against" (`sources.toml`, only changes when we want to bump) from "what corpus the test run consumed" (`manifest.toml`, deterministic and cacheable).

Fixtures land in `tests/conformance/upstream/fixtures/`, which is gitignored — nothing in the corpus is committed. To fetch a single area rather than all seven, use `just download-upstream-fixtures-area <AREA>` (equivalently `cargo xtask download-upstream-fixtures --area <AREA>`), where `<AREA>` is one of the seven area names listed below.

## Status

| Area | Source | Coverage | Status |
|---|---|---|---|
| UPLC (Plutus) | IntersectMBO/plutus | 1003 evaluation cases | 100% — skip list empty |
| ouroboros-consensus | IntersectMBO/ouroboros-consensus | Block / header golden files per era | passing |
| cardano-ledger | IntersectMBO/cardano-ledger | Genesis JSON, CDDL schema, golden transactions | passing |
| cardano-node | IntersectMBO/cardano-node | Genesis spec files | passing |
| ledger-rules (ImpSpec) | IntersectMBO/cardano-ledger | ~8100 CBOR STS-rule vectors from ImpSpec, across 11 rule families | passing — `SKIP_LIST` empty |
| cardano-base | IntersectMBO/cardano-base | VRF v03 crypto test vectors | passing |
| mithril | input-output-hk/mithril | Certificate fixture JSON | passing |

## Per-area detail

`just test-conformance` is the whole suite: `test-conformance-uplc` (crate
`dugite-uplc`, `--test conformance`) plus `test-conformance-upstream` (crate
`dugite-conformance`, `--test upstream_tests`). The six single-area recipes
below are nextest filters over the latter. Because they are filters, nextest
reports the other areas as **skipped** — that is the filter working, not a
coverage gap.

### UPLC (Plutus)

**Source:** [IntersectMBO/plutus](https://github.com/IntersectMBO/plutus), pinned to tag `1.66.0.0` in `sources.toml`.

**What's validated:** 1003 evaluation test cases from `plutus-conformance/test-cases/uplc/evaluation/`. Each test case provides a UPLC program and the expected result (a term, a budget exhaustion, or a specific runtime error). The dugite-uplc CEK machine evaluates each program and the harness compares term-for-term, budget-for-budget against the expected output.

**Status:** 100% passing. The skip list (`crates/dugite-uplc/tests/conformance_skip.txt`) has been empty since v1.7.0 — it currently contains only comments. The build script fails loudly if a skip entry names a directory that is not in the downloaded corpus, so a stale entry cannot silently hide a fix. The harness covers normalisation by evaluation (NbE) readback, per-builtin cost model wiring, CIP-122 bit ordering, BLS LE scalar handling with null augmentation, and BIP-340 `verify_raw` semantics (not the SHA-256-wrapped `verify`).

> **This pin is version-coupled, not just a version bump.** The UPLC parser
> (`syn::parser::parse_value_literal`) tracks the corpus's own semantics —
> 1.66.0.0 reworked `builtin/constant/value` (`key-*` → `currencyID-*` /
> `tokenID-*`) and changed non-canonical `value` literals from *normalised* to
> *rejected*. `sources.toml` and `manifest.toml` must therefore advance
> together; bumping one alone breaks the suite.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-uplc
```

### ouroboros-consensus

**Source:** [IntersectMBO/ouroboros-consensus](https://github.com/IntersectMBO/ouroboros-consensus), SHA-pinned in `sources.toml`.

**What's validated:** Era-tagged golden files for Cardano blocks and headers. The harness exercises the in-house multi-era CBOR decoder against fixtures captured directly from the upstream Haskell encoders, asserting round-trip and structural equivalence per era.

**Status:** passing across all eras (Byron, Shelley, Allegra, Mary, Alonzo, Babbage, Conway).

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-ouroboros-consensus
```

### cardano-ledger

**Source:** [IntersectMBO/cardano-ledger](https://github.com/IntersectMBO/cardano-ledger), SHA-pinned in `sources.toml`.

**What's validated:** Three classes of fixture. Genesis JSON for each era is parsed and structurally compared. The CDDL schema is loaded and exercised against representative documents. Golden transaction CBOR is decoded and asserted for byte-equality on re-encode.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-cardano-ledger
```

### cardano-node

**Source:** [IntersectMBO/cardano-node](https://github.com/IntersectMBO/cardano-node), SHA-pinned in `sources.toml`.

**What's validated:** Genesis spec files (`shelley-genesis.json`, `alonzo-genesis.json`, `conway-genesis.json` and their Byron counterpart). The harness asserts that dugite parses each spec into its internal genesis types and that the resulting types preserve every documented field.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-cardano-node
```

### ledger-rules (ImpSpec)

**Source:** [IntersectMBO/cardano-ledger](https://github.com/IntersectMBO/cardano-ledger) ImpSpec, SHA-pinned in `sources.toml`. The corpus regeneration pipeline builds cardano-ledger from source (GHC 9.6.5 + cabal 3.10.x, ≈35 min cold / 5 min cached) and runs the upstream ImpSpec conformance suite with `CONFORMANCE_CBOR_DUMP_PATH` set to capture every test vector as CBOR.

**What's validated:** Eleven STS-rule families, not just the two headline ones — `NEWEPOCH` and `ConwayNEWEPOCH` (epoch-boundary transitions), `LEDGER` (transaction application), `POOL`, `CERT`, `CERTS`, `DELEG`, `GOVCERT`, `GOV`, `ENACT`, and `RATIFY`. The current corpus holds roughly 8,100 captured test-case directories. The harness replays each CBOR vector through the corresponding dugite ledger code path and compares the resulting state byte-for-byte.

Note that an empty `SKIP_LIST` means no vector is skipped *by policy*. A vector can still report `Skipped` at runtime if the runner cannot construct its precondition; that is visible in the run output, not hidden by the list.

**Status:** passing. `SKIP_LIST` in `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` is empty.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-ledger-rules
```

### cardano-base

**Source:** [IntersectMBO/cardano-base](https://github.com/IntersectMBO/cardano-base), SHA-pinned in `sources.toml`.

**What's validated:** VRF v03 test vectors. Each vector ships an input message, a signing key, an expected proof, and an expected output hash. The harness exercises the dugite VRF implementation against every vector and asserts byte-equality on both the proof and the output, which is what guarantees Praos-compatible leader election.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-cardano-base
```

### mithril

**Source:** [input-output-hk/mithril](https://github.com/input-output-hk/mithril), SHA-pinned in `sources.toml`.

**What's validated:** Mithril certificate fixture JSON. The harness loads each certificate, verifies the aggregate signature, and asserts structural equivalence against the upstream-captured form.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just test-conformance-mithril
```

## CI integration

The `upstream-conformance` job in `.github/workflows/ci.yml` runs both the UPLC suite and the upstream tests with the `DUGITE_REQUIRE_UPSTREAM=1` environment variable set. This variable makes a missing fixture a hard failure rather than a silent skip — the gate exists specifically to stop the suite from quietly degrading to a no-op when something is wrong with the fixture cache or download.

Fixture tarballs are cached on the CI runner, keyed by the SHA-256 content hash of `tests/conformance/upstream/manifest.toml`. Bumping `[release].tag` in that file invalidates the cache automatically; no separate cache-bust step is needed.

## Updating the corpus

To adopt a new upstream version:

1. Edit `tests/conformance/upstream/sources.toml`, bumping the SHA (or tag for the `plutus` area) of the area you want to refresh.
2. Run the `regenerate-conformance-corpus` workflow. It fires on **three** triggers: weekly cron (Sundays 02:00 UTC), manual dispatch (with optional per-area SHA/tag overrides patched into `sources.toml` for that run), and any push to `main` touching `sources.toml` or `scripts/regenerate-conformance-corpus/**`. It produces a new dugite release tagged `conformance-corpus-v<YYYYmmdd-HHMMSS>` with the seven tarballs plus a `corpus-manifest.json`.
3. The workflow then **opens an adoption PR itself** (`chore/adopt-<NEW_TAG>`, titled `chore(conformance): adopt corpus <NEW_TAG>`) rewriting `[release].tag` in `manifest.toml`. This exists because the workflow used to publish releases nothing pointed at, and the pinned corpus silently drifted about two months stale. If you are adopting by hand, edit `[release].tag` yourself.
4. Run `just download-upstream-fixtures && just test-conformance` locally.
5. Fix any test fallout, then commit the `sources.toml` + `manifest.toml` updates together with the code changes.

To iterate on a capture script without publishing a release, `just regenerate-corpus-local` runs the same pipeline into `target/conformance-corpus/<tag>/`.

### Currently pinned

`[release].tag = "conformance-corpus-v20260725-154355"`, built from:

| Area | Pin |
|---|---|
| ouroboros-consensus | `f205a7103deb732cc07cabd51fa76ce22f84f0d0` |
| cardano-ledger | `a88b60bdcf3248dfe5a2f9372c188c399233f479` |
| cardano-node | `0a21a7437fb9d38060b297c5997275d316e60d5c` |
| plutus | tag `1.66.0.0` |
| ledger-rules | `a88b60bdcf3248dfe5a2f9372c188c399233f479` (same tree as cardano-ledger) |
| cardano-base | `12168e4b32b44d30dd401010ccd969accaf2add7` |
| mithril | `2eedbd254e6bb656f6c10ec83930327dd0768a4a` |

## See also

- [Benchmarks](./benchmarks.md) — performance evidence.
- Wiki [Protocol Compliance](https://github.com/michaeljfazio/dugite/wiki/Protocol-Compliance) — feature-by-feature compatibility catalogue.
- Wiki [Known Issues](https://github.com/michaeljfazio/dugite/wiki/Known-Issues) — open gaps and follow-ups.
