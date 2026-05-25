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

## Status

| Area | Source | Coverage | Status |
|---|---|---|---|
| UPLC (Plutus) | IntersectMBO/plutus | 999 evaluation cases | 100% — skip list empty |
| ouroboros-consensus | IntersectMBO/ouroboros-consensus | Block / header golden files per era | passing |
| cardano-ledger | IntersectMBO/cardano-ledger | Genesis JSON, CDDL schema, golden transactions | passing |
| cardano-node | IntersectMBO/cardano-node | Genesis spec files | passing |
| ledger-rules (ImpSpec) | IntersectMBO/cardano-ledger | CBOR NEWEPOCH + LEDGER vectors from ImpSpec | passing — `SKIP_LIST` empty |
| cardano-base | IntersectMBO/cardano-base | VRF v03 crypto test vectors | passing |
| mithril | input-output-hk/mithril | Certificate fixture JSON | passing |

## Per-area detail

### UPLC (Plutus)

**Source:** [IntersectMBO/plutus](https://github.com/IntersectMBO/plutus), pinned to tag `1.65.0.0` in `sources.toml`.

**What's validated:** 999 evaluation test cases from `plutus-conformance/test-cases/uplc/evaluation/`. Each test case provides a UPLC program and the expected result (a term, a budget exhaustion, or a specific runtime error). The dugite-uplc CEK machine evaluates each program and the harness compares term-for-term, budget-for-budget against the expected output.

**Status:** 100% passing. The skip list is empty as of v1.7.0. The harness covers normalisation by evaluation (NbE) readback, per-builtin cost model wiring, CIP-122 bit ordering, BLS LE scalar handling with null augmentation, and BIP-340 `verify_raw` semantics (not the SHA-256-wrapped `verify`).

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

**What's validated:** Two STS-rule families — NEWEPOCH (epoch-boundary transitions) and LEDGER (transaction application). The harness replays each captured CBOR vector through the corresponding dugite ledger code path and compares the resulting state byte-for-byte.

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
