# Upstream Conformance Testing — Design

**Date:** 2026-05-23
**Status:** Draft, awaiting approval
**Worktree:** `worktree-ledger-state-verification-2026-05-23`

## Motivation

Dugite currently exercises Cardano conformance through three independent mechanisms:

1. **43 hand-crafted JSON vectors** under `tests/conformance/vectors/` covering UTXO / CERT / GOV / EPOCH at a basic-scenario level.
2. **3,002 UPLC test cases** in `crates/dugite-uplc/tests/conformance/`, downloaded from the IntersectMBO/plutus release artefact by `scripts/dev/download-plutus-conformance.sh` and pinned at `PLUTUS_VERSION` (currently `1.65.0.0`).
3. **Various per-area golden files** — 32 N2C CBOR messages, 5 era-specific transaction hex blobs, leadership-schedule JSON, VRF golden tests, Mithril fixtures.

The hand-crafted ledger vectors are explicitly acknowledged as insufficient (`tests/conformance/README.md`: *"the hand-crafted vectors test basic scenarios. Full coverage requires the Haskell generator with property-based testing"*). The UPLC pipeline, by contrast, gives us continuous byte-exact alignment with the official upstream because we pull the same corpus the Haskell project ships.

We want to extend the UPLC pattern across **every functional area of the node** where official upstream conformance artefacts exist or can be generated from official sources. The blinklabs-io/ouroboros-mock project (used by Dingo) was reviewed for inspiration — it pulls from four IntersectMBO repos at pinned SHAs and vendors ledger-rule replay vectors from pragma-org/amaru. We adopt the same source set plus CDDL validation, strict typed PParams, crypto vectors, and Mithril fixtures, but with **one critical architectural difference**: every upstream artefact flows through a single dugite-owned republication pipeline. We don't pull anything directly from external repos at test time. We don't depend on any sibling project's vendoring choices. We don't have different fetch strategies for different areas.

**Why republish:** uniform trust chain, uniform fetch path, uniform refresh workflow, uniform CI cache key, full provenance traceability via release notes, and immunity to upstream availability blips on PR-CI runs. The pipeline cost is paid once at refresh time; consumers pay only a download.

## Goals

- **One regeneration pipeline** captures or builds every upstream artefact and republishes them as assets of a single dugite GitHub release.
- **One source-pins file** (`sources.toml`) declares which upstream SHAs/tags the pipeline uses to produce each area's content.
- **One manifest file** (`manifest.toml`) declares which dugite release the consumers (xtask + tests) pull from.
- **One xtask binary** with one fetch mode: download the named asset from the pinned dugite release.
- **One fixture root** holds every downloaded area, gitignored, with a sentinel proving freshness.
- **One gating model** — hard fail in CI, silently skip in dev.
- **UPLC** is folded in (its content is republished by the pipeline like every other area).
- **CDDL schema validation** verifies our generated CBOR against `conway.cddl`.
- **Strict typed PParams** deserialization confirms our type matches Haskell's JSON shape exactly.
- **Self-generated ledger-rule replay** drives ImpSpec event sequences through our ledger and compares against expected state.
- **VRF/KES crypto vectors** from `cardano-base` validate our crypto primitives.
- **Mithril certificate fixtures** validate certificate verification.
- **Adding a new upstream area** in future = adding a per-area capture step to the pipeline + a `sources.toml` entry + a `manifest.toml` area entry + a Rust test module.

## Non-Goals (deferred)

- **Mini-protocol conversation tests** (handshake / ChainSync / BlockFetch / TxSubmission). Different architecture (mock-connection harness). Warrants its own design doc.

## Architecture

### Components

```
tests/conformance/upstream/
  sources.toml                            ← committed; upstream SHA/tag pins (input to the workflow)
  manifest.toml                           ← committed; which dugite release to consume
  fixtures/                               ← gitignored; downloaded on demand
    MANIFEST_SHA256                       ← sentinel: sha256(manifest.toml) at download time
    ouroboros-consensus/
    cardano-ledger/
    cardano-node/
    plutus/                               ← moved from crates/dugite-uplc/tests/conformance/
    ledger-rules/                         ← Phase 4 — self-generated ImpSpec dumps
    cardano-base/                         ← Phase 5 — VRF/KES vectors
    mithril/                              ← Phase 6 — certificate vectors

xtask/                                    ← new workspace member
  Cargo.toml
  src/bin/download-upstream-fixtures.rs
  tests/extract.rs

tests/conformance/src/upstream/           ← new test module tree
  mod.rs
  fixtures.rs                             ← shared loader + sentinel check
  status.rs                               ← always-runs banner / hard-fail test
  ouroboros_consensus.rs
  cardano_ledger.rs
  cardano_ledger_cddl.rs                  ← Phase 2
  cardano_ledger_pparams_typed.rs         ← Phase 3
  cardano_node.rs
  ledger_rules_replay.rs                  ← Phase 4
  cardano_base.rs                         ← Phase 5
  mithril.rs                              ← Phase 6

scripts/regenerate-conformance-corpus/
  regenerate.sh                           ← orchestrator: reads sources.toml, runs per-area capture, bundles, publishes
  capture-ouroboros-consensus.sh
  capture-cardano-ledger.sh
  capture-cardano-node.sh
  capture-plutus.sh
  capture-ledger-rules.sh                 ← builds cardano-ledger + runs ImpSpec dump (the expensive one)
  capture-cardano-base.sh
  capture-mithril.sh

.github/workflows/regenerate-conformance-corpus.yml
                                          ← scheduled / on-demand workflow that runs the regenerate script and creates a release

.cargo/config.toml                        ← `xtask` alias
.gitignore                                ← entry for tests/conformance/upstream/fixtures/
justfile                                  ← `download-upstream-fixtures` + `test-upstream` + `regenerate-corpus-local` recipes
```

### Two-file pin model

The pinning is split into two files with different audiences. The flow is unidirectional:

```
  sources.toml          (upstream pins — input to the pipeline)
       │
       ▼  regeneration workflow (or `just regenerate-corpus-local`)
       │
   <area>.tar.gz × N    (per-area asset tarballs)
   corpus-manifest.json (machine-readable provenance)
       │
       ▼  attached to a dugite GitHub release
       │
   conformance-corpus-v<TS> tag
       │
       ▼  manifest.toml points here
       │
       ▼  xtask + tests consume
```

Each file carries a header comment pointing at the other to aid maintainers.

**`tests/conformance/upstream/sources.toml`** — *input to the republication pipeline.* Records which upstream SHAs/tags each per-area capture script uses. **Not consumed by the xtask.** Edited only when bumping an upstream pin and re-triggering the workflow.

```toml
# Upstream source pins for the conformance corpus regeneration pipeline.
# Bumping a pin here and triggering the regenerate-conformance-corpus
# workflow produces a new dugite release that manifest.toml then points at.

[ouroboros-consensus]
repo = "IntersectMBO/ouroboros-consensus"
sha  = "0000000000000000000000000000000000000000"  # placeholder

[cardano-ledger]
repo = "IntersectMBO/cardano-ledger"
sha  = "0000000000000000000000000000000000000000"

[cardano-node]
repo = "IntersectMBO/cardano-node"
sha  = "0000000000000000000000000000000000000000"

[plutus]
repo          = "IntersectMBO/plutus"
tag           = "v1.65.0.0"
release_asset = "plutus-conformance.tar.gz"

[ledger-rules]
# This area builds cardano-ledger + runs ImpSpec with CONFORMANCE_CBOR_DUMP_PATH.
# The SHA below is the cardano-ledger version we build/run against.
repo = "IntersectMBO/cardano-ledger"
sha  = "0000000000000000000000000000000000000000"

[cardano-base]
repo = "IntersectMBO/cardano-base"
sha  = "0000000000000000000000000000000000000000"

[mithril]
repo = "input-output-hk/mithril"
sha  = "0000000000000000000000000000000000000000"
```

**`tests/conformance/upstream/manifest.toml`** — *input to the xtask + tests.* Declares which dugite release tag to pull from and lists the per-area assets in that release.

```toml
# The dugite release this corpus pins to. Every upstream area
# is pulled as an asset of this single release tag.
[release]
repo = "michaeljfazio/dugite"
tag  = "conformance-corpus-v20260601-000000"  # placeholder; bump when adopting a new release

[area.ouroboros-consensus]
asset  = "ouroboros-consensus.tar.gz"
target = "ouroboros-consensus"

[area.cardano-ledger]
asset  = "cardano-ledger.tar.gz"
target = "cardano-ledger"

[area.cardano-node]
asset  = "cardano-node.tar.gz"
target = "cardano-node"

[area.plutus]
asset  = "plutus.tar.gz"
target = "plutus"

[area.ledger-rules]
asset  = "ledger-rules.tar.gz"
target = "ledger-rules"

[area.cardano-base]
asset  = "cardano-base.tar.gz"
target = "cardano-base"

[area.mithril]
asset  = "mithril.tar.gz"
target = "mithril"
```

**Provenance** is preserved in two places:
- The dugite release tag itself (e.g. `conformance-corpus-v20260601-000000`) is a stable reference.
- The release notes (generated by the workflow) record every upstream SHA/tag used and the timestamp of the run.

### Regeneration pipeline

**`.github/workflows/regenerate-conformance-corpus.yml`** triggers:

- `workflow_dispatch` — manual, with optional per-area pin overrides as inputs
- `schedule: '0 6 * * 0'` — weekly Sunday 06:00 UTC
- `push` on `paths: [scripts/regenerate-conformance-corpus/**, tests/conformance/upstream/sources.toml]` — sanity rerun on pipeline changes

The job:

1. Sets up the Haskell toolchain — **prefer the Nix flake** shipped by cardano-ledger upstream (deterministic GHC + cabal versions) for the `ledger-rules` area; fall back to `haskell-actions/setup` only if Nix is unavailable. Mixing toolchains across runs risks non-deterministic dumps; Nix eliminates this.
2. Runs `scripts/regenerate-conformance-corpus/regenerate.sh`, which orchestrates per-area capture scripts in a fail-fast manner. **The script aborts immediately on any per-area failure — no partial release is ever published.**
3. Each per-area capture script (`capture-<area>.sh`):
   - Clones the upstream repo at the SHA from `sources.toml` (or, for Plutus, downloads the upstream release asset).
   - For most areas: copies the curated file subset into a working dir.
   - For `ledger-rules`: builds `cardano-ledger-conformance` via the cardano-ledger Nix flake, runs `cabal test cardano-ledger-conformance` with `CONFORMANCE_CBOR_DUMP_PATH=./dumps`, walks the dump tree.
   - For areas not yet wired into tests (Phase 4-6 before their respective phases land): produces a placeholder tarball containing only a `README.txt` that names the area and notes "stub — populated in Phase N". Consumers tolerate empty area subdirectories.
   - Packages the working dir as `<area>.tar.gz`. **Asset naming convention:** all republished assets are named `<area>.tar.gz` regardless of upstream naming (e.g., upstream's `plutus-conformance.tar.gz` is republished as `plutus.tar.gz`).
   - Emits SHA-256 hashes for every output file into a per-area `hashes.json` (consumed in step 4 below).
4. Generates `corpus-manifest.json` — a machine-readable provenance document attached as one of the release assets. Schema:

   ```json
   {
     "release_tag": "conformance-corpus-v20260601-000000",
     "generated_at": "2026-06-01T00:00:00Z",
     "areas": {
       "ouroboros-consensus": {
         "upstream_repo": "IntersectMBO/ouroboros-consensus",
         "upstream_sha":  "abcdef...",
         "asset":         "ouroboros-consensus.tar.gz",
         "file_count":    34,
         "file_hashes":   { "Block_Byron_EBB": "sha256:...", ... }
       },
       "ledger-rules": {
         "upstream_repo":     "IntersectMBO/cardano-ledger",
         "upstream_sha":      "abcdef...",
         "ghc_version":       "9.6.7",
         "cabal_version":     "3.10.2.0",
         "asset":             "ledger-rules.tar.gz",
         "imp_test_categories": ["ShelleyImpSpec", "MaryImpSpec", ...],
         "file_count":        324,
         "stub":              false
       },
       ...
     }
   }
   ```

   Areas in stub mode set `"stub": true` and omit `file_hashes`.
5. Computes the release tag: `conformance-corpus-v$(date -u +%Y%m%d-%H%M%S)`.
6. Creates a GitHub release on the dugite repo with all `<area>.tar.gz` + `corpus-manifest.json` attached. Release notes are auto-rendered from `corpus-manifest.json` (so notes and JSON never disagree).
7. Workflow permissions: `contents: write`.

**Idempotency:** re-running with the same `sources.toml` produces semantically equivalent content (modulo timestamp). The release tag includes the date so re-runs don't collide.

**Immutability:** once a release is published, its assets must never be overwritten. Maintainers who need to fix a regenerated corpus cut a new tag rather than re-upload. This ensures CI caches keyed by manifest hash are never poisoned.

**Local equivalent:** `just regenerate-corpus-local` runs the same scripts on a developer's machine, producing tarballs in `target/conformance-corpus/` without creating a release. Useful for testing the pipeline. Also serves as the manual fallback if the automated workflow is down: a maintainer can run locally and upload via `gh release create`.

### Adopting a regenerated corpus

1. Edit `sources.toml` to bump the desired area's pin (or accept the current pins for a full refresh).
2. Trigger the workflow manually (or wait for the next scheduled run).
3. Workflow produces a new release `conformance-corpus-v<timestamp>` on the dugite repo.
4. Edit `manifest.toml`: update `[release].tag` to the new release tag.
5. Run `just download-upstream-fixtures` → `just test-upstream` locally.
6. Fix any test fallout (ledger code adaptations, etc.).
7. Commit `sources.toml` + `manifest.toml` + any code changes. PR diff is small and reviewable.

### xtask binary

Workspace layout:

```
xtask/
  Cargo.toml          publish = false; binary `download-upstream-fixtures`
  src/bin/download-upstream-fixtures.rs
  tests/extract.rs    unit tests against a synthetic tarball
```

`.cargo/config.toml`:

```toml
[alias]
xtask = "run --release --package xtask --"
```

Invocation:
- `cargo xtask download-upstream-fixtures` — all areas
- `cargo xtask download-upstream-fixtures --area <name>` — single area

Behaviour:

1. Read and parse `tests/conformance/upstream/manifest.toml` via the `toml` crate.
2. For each area (or just the requested one):
   a. Compute the asset URL: `https://github.com/<release.repo>/releases/download/<release.tag>/<area.asset>`.
   b. Wipe the target subdirectory.
   c. Download with `reqwest::blocking`. Send `Authorization: Bearer $GITHUB_TOKEN` if set.
   d. Retry transient failures 3× with backoff 1s, 2s, 4s.
   e. Extract via `flate2::read::GzDecoder` + `tar::Archive` into the target. Each per-area tarball is self-contained with no allowlist filtering — the regeneration pipeline already curated the contents.
   f. Report `<area>: N files extracted`.
3. After all areas succeed, write `fixtures/MANIFEST_SHA256` = `sha256(manifest.toml contents)`.

Dependencies (minimal):

```toml
[dependencies]
toml    = "0.8"
serde   = { version = "1", features = ["derive"] }
reqwest = { version = "0.12", features = ["blocking", "json"] }
flate2  = "1"
tar     = "0.4"
sha2    = "0.10"
anyhow  = "1"
clap    = { version = "4", features = ["derive"] }
```

Unit tests in `xtask/tests/extract.rs` exercise the extractor against a synthetic tarball checked into the xtask crate. No live network in unit tests.

### Gating model

Two env vars:

- **`DUGITE_UPSTREAM_FIXTURES_DIR`** — fixture root path. Default: walk up from `CARGO_MANIFEST_DIR` looking for a `Cargo.toml` with `[workspace]`, then append `tests/conformance/upstream/fixtures`.
- **`DUGITE_REQUIRE_UPSTREAM=1`** — fixtures missing or sentinel mismatch causes hard test failure. CI always sets it.

`upstream::status::upstream_fixtures_status` always runs and:
- Prints a banner ("present, sentinel matches" / "missing — run `just download-upstream-fixtures`").
- In `REQUIRE` mode, fails when fixtures missing or sentinel hash differs.
- In dev mode, never fails.

Other tests call `require_upstream!()` at top → return-early in dev, panic in REQUIRE mode.

### Feature flags

Single feature `upstream-conformance` gates new modules and the (renamed) UPLC corpus. Backwards-compat alias keeps existing invocations working.

`crates/dugite-uplc/Cargo.toml`:

```toml
[features]
upstream-conformance = []
conformance = ["upstream-conformance"]   # alias — existing `--features conformance` keeps working
```

`tests/conformance/Cargo.toml`:

```toml
[features]
upstream-conformance = []
```

### UPLC unification details

1. Plutus content is now produced by `capture-plutus.sh` (which downloads the IntersectMBO/plutus release asset at the SHA in sources.toml, repackages it as `plutus.tar.gz`) and published as part of the dugite corpus release.
2. UPLC test runner resolves the fixture path via shared `workspace_root()` + `DUGITE_UPSTREAM_FIXTURES_DIR`.
3. Delete `scripts/dev/download-plutus-conformance.sh`.
4. Add `conformance = ["upstream-conformance"]` alias in dugite-uplc/Cargo.toml.
5. CI step rename + switch from the bash script to xtask.

`workspace_root()` helper (~10 lines, mirrored in both crates):

```rust
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists()
            && std::fs::read_to_string(&cargo)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        if !dir.pop() {
            panic!("workspace root not found");
        }
    }
}
```

## Test modules

### Phase 1 — `upstream::ouroboros_consensus`

Strip envelopes explicitly:

```rust
/// Strip CBOR tag 24 wrapping from a Block_<Era> golden → raw block CBOR.
fn unwrap_consensus_block(bytes: &[u8]) -> Vec<u8>;

/// Decode Header_<Era> = `chainsync.WrappedHeader` 3-tuple → (era_tag, header_body).
fn unwrap_wrapped_header(bytes: &[u8]) -> (u32, Vec<u8>);
```

Per-era tests (Byron_EBB through Conway; Dijkstra excluded — upstream golden truncated):

- `block_<era>_decodes` × 8
- `header_cross_checks_<era>` × 8 (slot / hash / blockNumber match paired Block)
- `gentx_id_<era>_matches` × 8 (`GenTx.hash() == GenTxId` 32-byte payload)

**24 hand-written tests, no macros.**

### Phase 1 — `upstream::cardano_ledger`

Pparams use `serde_json::Value` probing (typed conversion in Phase 3):

- `pparams_<era>_has_required_fields` × 4 (shelley/alonzo/babbage/conway)
- `pparams_update_<era>_decodes` × 3 (alonzo/babbage/conway)
- `alonzo_golden_block_decodes`, `alonzo_golden_tx_decodes`, `conway_golden_tx_decodes`
- `alonzo_mainnet_genesis_decodes`

**11 tests.**

### Phase 1 — `upstream::cardano_node`

- `alonzo_genesis_spec_decodes`
- `conway_genesis_spec_decodes`

### Phase 2 — `upstream::cardano_ledger_cddl`

Adds `cddl = "0.9"` (or current) as dev-dep. Validates our generated CBOR against the canonical schema:

```rust
fn validate_cbor_against_cddl(cbor: &[u8], cddl_rule: &str) -> Result<(), String>;
```

Tests:
- `cddl_conway_tx_roundtrip_validates`
- `cddl_conway_block_validates`
- `cddl_conway_header_validates`
- `cddl_conway_govaction_validates`
- (further rules added as gaps surface)

### Phase 3 — `upstream::cardano_ledger_pparams_typed`

Introduces `HaskellPParams<Era>` (matching the Haskell JSON field names exactly) + `impl TryFrom<HaskellPParams<Era>> for PParams`.

Tests:
- `haskell_pparams_<era>_decodes_strict` × 4
- `haskell_pparams_conway_roundtrip` (decode → convert → re-encode → matches input)

Phase 1 `Value` probes stay (they catch unknown-field additions).

### Phase 4 — `upstream::ledger_rules_replay` (self-generated ImpSpec replay)

The highest-value addition. Our regeneration pipeline runs cardano-ledger's `ImpSpec` suite at the pinned SHA with `CONFORMANCE_CBOR_DUMP_PATH` set, captures the dumps, and publishes `ledger-rules.tar.gz` as part of the dugite release.

Each vector file is the same shape every ImpSpec dump produces:

```
CBOR [config(arr[13]), initial_state(arr[7]), final_state(arr[7]), events(arr[N]), title(str)]
events = [Transaction[0, tx_cbor, success_bool, slot]
        | PassTick[1, slot]
        | PassEpoch[2, epoch_delta]]
protocol params referenced by hash → loaded from pparams-by-hash/<hash>
```

Components in `crates/dugite-conformance/src/upstream/ledger_rules_replay/`:

- **`vector.rs`** — CBOR-decode the 5-tuple.
- **`bridge.rs`** — translate dumped `initial_state` into Dugite's `LedgerState` + `EpochState` + `GovState`; translate dumped `final_state` into a Dugite-comparable shape.
- **`runner.rs`** — apply each event. `Transaction` → `Ledger::apply_tx`; `PassTick` → advance slot; `PassEpoch` → epoch boundary.
- **`compare.rs`** — field-by-field equivalence with human-readable diff on mismatch.

Tests are file-driven — one `#[test]` per era category:

```rust
#[test] fn ledger_rules_shelley_imp_spec()    { run_all("ShelleyImpSpec/"); }
#[test] fn ledger_rules_mary_imp_spec()       { run_all("MaryImpSpec/"); }
#[test] fn ledger_rules_allegra_imp_spec()    { run_all("AllegraImpSpec/"); }
#[test] fn ledger_rules_alonzo_imp_spec()     { run_all("AlonzoImpSpec/"); }
#[test] fn ledger_rules_babbage_imp_spec()    { run_all("BabbageImpSpec/"); }
#[test] fn ledger_rules_conway_imp_spec_v10() { run_all("ConwayImpSpec_-_Version_10/"); }
```

**Risk:** Translating the dumped state model to Dugite's internal structures will surface model mismatches — exactly the bugs this conformance pass exists to catch. Phase 4 work includes ledger bugfixes, not just test code.

**Skip list:** On first introduction, expect a non-empty `SKIP_FILES` constant for vectors depending on features Dugite hasn't fully implemented. Each skip is filed as a specific issue; the list decays to zero.

**Conway-ratification fixture supersession:** Conditional on a hard gate — the audit runs **only when the Conway ImpSpec skip-list is empty** (i.e., every Conway vector either passes or has been explicitly excluded with a tracking issue showing the gap is in dugite, not the fixture). Auditing while skips still hide scenarios overlapping with `fixtures/conway-ratification/` could remove real coverage. Once the gate is met:

1. Cross-reference every scenario covered by the three conway-ratification fixtures (bootstrap PV9, post-bootstrap PV10/11, governance action expiry) against the Conway ImpSpec vector titles.
2. If every scenario has equivalent coverage in ImpSpec, **delete**: the fixtures directory, `capture-ratification-fixture` CLI, the README, the inline ratification tests, and the CLAUDE.md memory entry about Koios DRep rate limits. Cite Phase 4 supersession in the commit.
3. If a scenario is uniquely covered by the fixture, keep that single fixture + its test and file an upstream-cardano-ledger issue requesting an ImpSpec scenario for it (so future regeneration absorbs the gap).

### Phase 5 — `upstream::cardano_base` (VRF/KES vectors)

During Phase 5 scoping, identify the exact `cardano-base` test-vector paths. Likely:

- `cardano-crypto-tests/test_vectors/vrf_ver03_*`
- `cardano-crypto-tests/test_vectors/kes_*`
- `cardano-crypto-class/test/Test/Crypto/...`

Tests cross-validate dugite-crypto against these vectors. Existing local `tests/golden/vrf/golden_tests.txt` is checked for redundancy and either retained, deduplicated, or marked supplementary.

Preliminary tests (finalised during Phase 5):
- `vrf_ver03_keypair_derivation_matches_vectors`
- `vrf_ver03_prove_matches_vectors`
- `vrf_ver03_verify_matches_vectors`
- `kes_keygen_matches_vectors`
- `kes_sign_verify_matches_vectors`
- `kes_evolution_matches_vectors`

### Phase 6 — `upstream::mithril` (certificate fixtures)

Pull from `input-output-hk/mithril` via the regeneration pipeline. Replace the current ad-hoc Mithril fixtures (`crates/dugite-node/tests/fixtures/mithril-*.json`).

Preliminary tests (finalised during Phase 6):
- `mithril_certificate_chain_verifies`
- `mithril_aggregator_response_decodes`
- `mithril_signature_aggregation_matches`

### CI integration

`.github/workflows/ci.yml`:

```yaml
- name: Cache upstream conformance fixtures
  uses: actions/cache@v4
  with:
    path: tests/conformance/upstream/fixtures
    key: upstream-fixtures-${{ hashFiles('tests/conformance/upstream/manifest.toml') }}

- name: Download upstream conformance fixtures
  run: cargo xtask download-upstream-fixtures
  env:
    GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}

- name: Run UPLC conformance
  env: { DUGITE_REQUIRE_UPSTREAM: "1" }
  run: cargo nextest run -p dugite-uplc --features upstream-conformance --test conformance --profile ci

- name: Run upstream conformance (all areas)
  env: { DUGITE_REQUIRE_UPSTREAM: "1" }
  run: cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests --profile ci
```

Cache key = manifest content hash. Bumping `[release].tag` invalidates the cache automatically.

For Phase 4 (Amaru-replay-equivalent), if wall-clock cost forces it, promote to nightly + per-PR sample. Measured during Phase 4 implementation.

### `justfile` recipes

```just
download-upstream-fixtures:
    cargo xtask download-upstream-fixtures

download-upstream-fixtures-area AREA:
    cargo xtask download-upstream-fixtures --area {{AREA}}

test-upstream:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-uplc --features upstream-conformance --test conformance
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests

regenerate-corpus-local:
    bash scripts/regenerate-conformance-corpus/regenerate.sh --local
```

Removes the existing `download-plutus-conformance` recipe.

## Implementation phases

Each phase is a separate PR series with its own CI gate.

### Phase 0 — Regeneration pipeline bootstrap

Phase 0 produces the **infrastructure-only** PR: pipeline, sources, scripts, workflow. No `manifest.toml`, no `xtask`, no test code, no test changes. CI is not touched. This isolation eliminates the chicken-and-egg risk where Phase 0 CI would try to consume a non-existent release.

1. Create `scripts/regenerate-conformance-corpus/` with the orchestrator + per-area capture scripts. Areas slated for later phases (ledger-rules, cardano-base, mithril) ship as **stub scripts** that produce placeholder tarballs (a `README.txt` declaring "stub — populated in Phase N"); active areas (ouroboros-consensus, cardano-ledger, cardano-node, plutus) produce real content.
2. Create `.github/workflows/regenerate-conformance-corpus.yml` with `workflow_dispatch` + scheduled + path-trigger modes.
3. Create initial `tests/conformance/upstream/sources.toml` with current upstream pins.
4. Trigger the workflow manually. Verify the release is created with all expected assets (real + stub) plus `corpus-manifest.json`.
5. **Phase 0 acceptance criteria:**
   - One regeneration workflow run completes end-to-end on `main`
   - The resulting GitHub release has all 7 area tarballs + `corpus-manifest.json`
   - Stub areas have `stub: true` in `corpus-manifest.json`
   - Real areas have non-empty `file_hashes`

Phase 1 (the next PR) introduces `manifest.toml` + xtask + test code + consumes the bootstrap release.

Estimated: 3-5 commits + one manual workflow trigger.

### Phase 1 — Foundation (wire format + pparams + genesis + UPLC unification)

xtask + manifest consumer + Phase-1 test areas. UPLC migrates to the unified fixture root.

Estimated: 4-6 commits.

### Phase 2 — CDDL validation

Adds `conway.cddl` to the cardano-ledger capture script + `cddl` crate dev-dep + CDDL test module.

Estimated: 2-3 commits.

### Phase 3 — Strict typed PParams

Code-only — no pipeline changes.

Estimated: 2 commits.

### Phase 4 — Self-generated ledger-rule replay (largest phase)

Wires up `capture-ledger-rules.sh` to build cardano-ledger + run ImpSpec with dump enabled. Triggers a regeneration. Implements the Rust replay harness against the new `ledger-rules.tar.gz` asset. Will surface real ledger bugs — fixes are in scope.

Includes the `fixtures/conway-ratification/` audit + removal once Conway replay is fully green.

Estimated: 8-15 commits across multiple PRs. Each ImpSpec era category (Shelley / Mary / Allegra / Alonzo / Babbage / Conway) can be a separate PR.

### Phase 5 — VRF/KES crypto vectors

Wires up `capture-cardano-base.sh`. Cross-validates dugite-crypto.

Estimated: 2-3 commits.

### Phase 6 — Mithril certificate fixtures

Wires up `capture-mithril.sh`. Replaces ad-hoc local Mithril fixtures.

Estimated: 2-3 commits.

## Risks and Caveats

- **GitHub rate limits.** xtask reads `GITHUB_TOKEN` to bump from 60/hr to 5000/hr.
- **Pipeline as single point of failure.** If the regeneration workflow breaks, no new releases are cut. Mitigation: the script is also runnable locally (`just regenerate-corpus-local`), so any maintainer can produce a release artefact manually and upload it via `gh release create` if the automated path is down.
- **Bootstrap chicken-and-egg.** First-time setup requires running the workflow once before `manifest.toml` can point at anything real. Documented in Phase 0.
- **CBOR envelope wrapping.** ouroboros-consensus goldens are wrapped. Tests use explicit unwrap helpers.
- **Dijkstra era.** Upstream `Block_Dijkstra` truncated; excluded.
- **Pparams JSON schema drift.** Phase 1 uses `Value` probing. Phase 3 introduces typed conversion. Drift between phases caught by Phase 1 tests.
- **`conformance` feature rename.** Mitigated by the alias.
- **xtask compile cost.** Mitigated by excluding xtask from workspace `default-members`.
- **Phase 4 surfaces real bugs.** By design. Budget includes ledger bugfix time.
- **Phase 4 corpus size + CI wall clock.** 300+ vectors per run may be slow. Promote to nightly + per-PR sample if needed.
- **Skip-list discipline.** Each skip = tracked issue with reproduction. Goal: zero.
- **CDDL crate maturity.** Evaluated during Phase 2; switch validator or implement minimal subset if blocking.
- **cardano-ledger Haskell build cost.** First build of the ledger-rules capture is ~30 min. Cached subsequent runs ~5 min. Acceptable for a scheduled / on-demand workflow; would be unacceptable for per-PR CI (which is why the artefact model exists).

## Open questions for follow-up review

- **Phase 4 per-PR vs nightly placement.** Decided during Phase 4 once wall-clock cost is measured.
- **Exact `cardano-base` and `mithril` paths.** Confirmed during Phase 5 / 6 scoping.
- **`tests/golden/vrf/` deduplication.** Decided once upstream vectors land.
- **Conway-ratification fixture audit outcome.** Default removal; reverse only on specific evidence.

## Acceptance criteria

Phase 0 is complete when:
- The regeneration workflow runs end-to-end and produces a dugite release with all area assets (some may be placeholder/empty until later phases wire them up).
- `sources.toml` and initial `manifest.toml` are committed.

Phase 1 is complete when:
- `cargo xtask download-upstream-fixtures` populates all Phase-1 fixture subdirs and writes the sentinel.
- `DUGITE_REQUIRE_UPSTREAM=1 just test-upstream` passes Phase-1 tests on CI and locally.
- `scripts/dev/download-plutus-conformance.sh` is deleted.
- `crates/dugite-uplc/tests/conformance/` is removed; UPLC tests read from the unified fixture root.
- `cargo build --workspace` passes (xtask compiles cleanly).
- CLAUDE.md is updated to document the new conformance flow.

Each subsequent phase has its own acceptance criteria defined at the start of its implementation plan, and ends with all gates (build / nextest / clippy / fmt) green.
