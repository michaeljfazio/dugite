# Upstream Conformance Testing — Design

**Date:** 2026-05-23
**Status:** Draft, awaiting approval
**Worktree:** `worktree-ledger-state-verification-2026-05-23`

## Motivation

Dugite currently exercises Cardano conformance through three independent mechanisms:

1. **43 hand-crafted JSON vectors** under `tests/conformance/vectors/` covering UTXO / CERT / GOV / EPOCH at a basic-scenario level.
2. **3,002 UPLC test cases** in `crates/dugite-uplc/tests/conformance/`, downloaded from the IntersectMBO/plutus release artefact by `scripts/dev/download-plutus-conformance.sh` and pinned at `PLUTUS_VERSION` (currently `1.65.0.0`).
3. **Various per-area golden files** — 32 N2C CBOR messages, 5 era-specific transaction hex blobs, leadership-schedule JSON, VRF golden tests, Mithril fixtures.

The hand-crafted ledger vectors are explicitly acknowledged as insufficient (see `tests/conformance/README.md`: *"the hand-crafted vectors test basic scenarios. Full coverage requires the Haskell generator with property-based testing"*). The UPLC pipeline, by contrast, gives us continuous byte-exact alignment with the official upstream because we pull the same corpus the Haskell project ships.

We want to extend the UPLC pattern across **every functional area of the node** where official upstream conformance artefacts exist: pull them directly from the canonical IntersectMBO and partner repositories, run our decoders/parsers/validators/replay harnesses against them, and gate CI on the result. The blinklabs-io/ouroboros-mock project (used by Dingo) was reviewed for inspiration — it pulls from four IntersectMBO repos at pinned SHAs plus the pragma-org/amaru ledger-rule replay corpus. We adopt the same source set plus crypto and Mithril, but with our own integration approach (Rust xtask + manifest, fixtures downloaded not committed).

## Goals

- One **manifest file** is the single source of truth for upstream version pinning across all conformance areas.
- One **download tool** fetches and extracts the curated content from each upstream source (supporting both explicit-file and recursive-path selection).
- One **fixture root** holds every downloaded corpus, gitignored, with a sentinel proving freshness.
- One **gating model** decides "hard fail in CI, silently skip in dev" uniformly across crates.
- **UPLC** is folded into the unified scheme without disrupting its existing tests.
- **CDDL schema validation** verifies our generated CBOR against `conway.cddl`.
- **Strict typed PParams** deserialization confirms our type matches Haskell's JSON shape exactly.
- **Amaru ledger-rule replay** drives transaction/epoch event sequences through our ledger and compares against expected state.
- **VRF/KES crypto vectors** from `cardano-base` validate our crypto primitives byte-exact.
- **Mithril certificate fixtures** from the Mithril repo validate certificate verification.
- **Adding a new upstream source** in future requires editing the manifest and writing test code, not changing infrastructure.

## Non-Goals (deferred to future design docs)

- **Mini-protocol conversation tests** (handshake, ChainSync, BlockFetch, TxSubmission flow tests). These require a mock-connection harness — a different architecture from static-fixture conformance. ouroboros-mock-the-system rather than ouroboros-mock's fixtures. Warrants its own design doc.

## Architecture

### Components

```
tests/conformance/upstream/
  manifest.toml                           ← committed; single source of truth
  fixtures/                               ← gitignored; downloaded on demand
    MANIFEST_SHA256                       ← sentinel: sha256(manifest.toml) at download time
    ouroboros-consensus/
    cardano-ledger/
    cardano-node/
    plutus/                               ← moved from crates/dugite-uplc/tests/conformance/
    amaru/                                ← Phase 4 — ledger-rule replay
    cardano-base/                         ← Phase 5 — VRF/KES vectors
    mithril/                              ← Phase 6 — certificate vectors

xtask/                                    ← new workspace member
  Cargo.toml
  src/bin/download-upstream-fixtures.rs

tests/conformance/src/upstream/           ← new test module tree
  mod.rs
  fixtures.rs                             ← shared loader + sentinel check
  status.rs                               ← always-runs banner / hard-fail test
  ouroboros_consensus.rs
  cardano_ledger.rs
  cardano_ledger_cddl.rs                  ← Phase 2 — CDDL validation
  cardano_ledger_pparams_typed.rs         ← Phase 3 — strict typed PParams
  cardano_node.rs
  amaru.rs                                ← Phase 4 — ledger replay
  cardano_base.rs                         ← Phase 5 — crypto vectors
  mithril.rs                              ← Phase 6 — Mithril certs

.cargo/config.toml                        ← `xtask` alias
.gitignore                                ← entry for tests/conformance/upstream/fixtures/
justfile                                  ← `download-upstream-fixtures` + `test-upstream` recipes
```

### Manifest format

Two selection modes per repo:

- **`files`** — explicit allowlist of paths inside the tarball. For curated subsets (the goldens from cardano-ledger / cardano-node / ouroboros-consensus).
- **`paths`** — directory prefixes pulled recursively. For whole-subtree corpora (Amaru's 358 ledger-rule replay vectors, Plutus conformance, possibly Mithril test data).

Either may be combined. Missing files in `files` mode are a **hard error**; empty matches in `paths` mode are a hard error.

```toml
# tests/conformance/upstream/manifest.toml
# (SHAs / tags shown below are illustrative placeholders — pin to current values at implementation time.)

# --- Phase 1: wire format ---
[repo.ouroboros-consensus]
url    = "https://github.com/IntersectMBO/ouroboros-consensus"
sha    = "0000000000000000000000000000000000000000"
target = "ouroboros-consensus"
files = [
  "ouroboros-consensus-cardano/golden/cardano/CardanoNodeToNodeVersion2/Block_Byron_EBB",
  # ... Block_<Era> × 8, Header_<Era> × 8, GenTx_<Era> × 8, GenTxId_<Era> × 8
]

# --- Phase 1: pparams + genesis (and Phase 2: CDDL) ---
[repo.cardano-ledger]
url    = "https://github.com/IntersectMBO/cardano-ledger"
sha    = "0000000000000000000000000000000000000000"
target = "cardano-ledger"
files = [
  "eras/shelley/impl/golden/pparams.json",
  "eras/alonzo/impl/golden/pparams.json",
  "eras/alonzo/impl/golden/pparams-update.json",
  "eras/babbage/impl/golden/pparams.json",
  "eras/babbage/impl/golden/pparams-update.json",
  "eras/conway/impl/golden/pparams.json",
  "eras/conway/impl/golden/pparams-update.json",
  "eras/alonzo/test-suite/golden/block.cbor",
  "eras/alonzo/test-suite/golden/tx.cbor",
  "eras/conway/impl/golden/tx.cbor",
  "eras/alonzo/test-suite/golden/mainnet-alonzo-genesis.json",
  # Phase 2 — CDDL schema:
  "eras/conway/impl/cddl/data/conway.cddl",
]

# --- Phase 1: genesis specs ---
[repo.cardano-node]
url    = "https://github.com/IntersectMBO/cardano-node"
sha    = "0000000000000000000000000000000000000000"
target = "cardano-node"
files = [
  "cardano-testnet/files/data/alonzo/genesis.alonzo.spec.json",
  "cardano-testnet/files/data/conway/genesis.conway.spec.json",
]

# --- Phase 1: existing UPLC, now unified ---
[repo.plutus]
url           = "https://github.com/IntersectMBO/plutus"
tag           = "v1.65.0.0"
release_asset = "plutus-conformance.tar.gz"
target        = "plutus"
# no files / paths — extract the entire release asset

# --- Phase 4: ledger-rule replay vectors ---
[repo.amaru]
url    = "https://github.com/pragma-org/amaru"
sha    = "0000000000000000000000000000000000000000"
target = "amaru"
paths = [
  "crates/amaru-ledger/tests/data/rules-conformance",
]

# --- Phase 5: VRF/KES crypto vectors ---
[repo.cardano-base]
url    = "https://github.com/IntersectMBO/cardano-base"
sha    = "0000000000000000000000000000000000000000"
target = "cardano-base"
# Exact paths to be confirmed during Phase 5 scoping; expected examples:
files = [
  # placeholder — confirm during Phase 5 scoping
  # "cardano-crypto-tests/test_vectors/vrf_ver03_generated_1",
  # "cardano-crypto-tests/test_vectors/kes_test_vectors",
]

# --- Phase 6: Mithril certificate fixtures ---
[repo.mithril]
url    = "https://github.com/input-output-hk/mithril"
sha    = "0000000000000000000000000000000000000000"
target = "mithril"
# Exact paths to be confirmed during Phase 6 scoping; expected examples:
paths = [
  # placeholder — confirm during Phase 6 scoping
  # "mithril-aggregator/tests/golden",
  # "mithril-client/tests/golden",
]
```

Fetch modes:

- **`sha` mode** — fetch `https://codeload.github.com/<org>/<repo>/tar.gz/<sha>`. Top-level dir `<repo>-<sha>/` is stripped during extraction. Apply `files` and/or `paths` selection.
- **`tag` + `release_asset` mode** — query `https://api.github.com/repos/<org>/<repo>/releases/tags/<tag>`, locate the named asset, download it. Either extract entire archive (no selection list) or apply `files`/`paths`.

### xtask binary

Workspace layout:

```
xtask/
  Cargo.toml          publish = false; binary `download-upstream-fixtures`
  src/bin/download-upstream-fixtures.rs
  tests/extract.rs    unit tests against small fixture tarballs
```

`.cargo/config.toml`:

```toml
[alias]
xtask = "run --release --package xtask --"
```

Invocation:
- `cargo xtask download-upstream-fixtures` — all repos
- `cargo xtask download-upstream-fixtures --repo <name>` — single repo
- `cargo xtask download-upstream-fixtures --phase 1` — only repos tagged for phase 1 (optional sugar; phases can be encoded as a manifest metadata field)

Behaviour:

1. Read and parse `tests/conformance/upstream/manifest.toml` via the `toml` crate.
2. Filter to requested repos.
3. For each repo:
   a. Wipe the target subdirectory (per-repo atomicity — if extraction fails halfway, the target is left empty rather than partially populated; the next test run will surface the issue via the sentinel check).
   b. Build the tarball URL based on fetch mode.
   c. Download with `reqwest::blocking`, sending `Authorization: Bearer $GITHUB_TOKEN` if the env var is set (raises rate limit from 60/h to 5000/h).
   d. Retry transient failures (HTTP 5xx, connection errors): 3 attempts, exponential backoff 1s, 2s, 4s.
   e. Extract via `flate2::read::GzDecoder` + `tar::Archive`. Strip top-level dir. Apply `files` allowlist (missing entries = hard error) and/or `paths` recursive copy (empty match = hard error).
   f. Report `<repo>: N files extracted from P paths`.
4. After all selected repos succeed, write `fixtures/MANIFEST_SHA256` = `sha256(manifest.toml)`.

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

Unit tests in `xtask/tests/extract.rs` exercise the extractor against a small synthetic tarball checked into the xtask crate. No live network calls in unit tests.

### Gating model

Two env vars:

- **`DUGITE_UPSTREAM_FIXTURES_DIR`** — path to fixture root. Default: walk up from `CARGO_MANIFEST_DIR` looking for a `Cargo.toml` containing `[workspace]`, then append `tests/conformance/upstream/fixtures`.
- **`DUGITE_REQUIRE_UPSTREAM=1`** — fixtures missing or sentinel mismatch causes hard test failure. When unset, tests return early. CI always sets it.

`upstream::status::upstream_fixtures_status` test always runs and:
- Prints a banner summarising state ("present, sentinel matches" / "missing — run `just download-upstream-fixtures`").
- In REQUIRE mode, fails when fixtures missing or sentinel hash differs from current `manifest.toml` hash.
- In dev mode, never fails.

Other tests call `require_upstream!()` at top → return-early in dev, panic in REQUIRE mode.

### Feature flags

Single feature `upstream-conformance` gates the new modules and (renamed) UPLC corpus. Backwards-compat alias keeps existing invocations working.

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

1. Move fixtures: xtask writes Plutus corpus to `tests/conformance/upstream/fixtures/plutus/` instead of the dugite-uplc-local path.
2. Update UPLC test runner to resolve path via shared `workspace_root()` helper + `DUGITE_UPSTREAM_FIXTURES_DIR`.
3. Delete `scripts/dev/download-plutus-conformance.sh`.
4. Add `conformance = ["upstream-conformance"]` alias in dugite-uplc/Cargo.toml.
5. CI step rename + switch from bash script to xtask invocation.

`workspace_root()` helper (mirrored in both crates, ~10 lines, no cross-crate dep):

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

Hand-written per-era tests (no macros). Strip envelopes explicitly:

```rust
/// Strip CBOR tag 24 wrapping from a Block_<Era> golden → raw block CBOR.
fn unwrap_consensus_block(bytes: &[u8]) -> Vec<u8>;

/// Decode Header_<Era> = `chainsync.WrappedHeader` 3-tuple → (era_tag, header_body).
fn unwrap_wrapped_header(bytes: &[u8]) -> (u32, Vec<u8>);
```

Per-era tests (Byron_EBB through Conway; Dijkstra excluded due to known upstream truncation):

- `block_<era>_decodes` — `MultiEraBlock` decode, era assertion (8 functions)
- `header_cross_checks_<era>` — slot / hash / blockNumber match paired Block (8 functions)
- `gentx_id_<era>_matches` — `GenTx.hash()` == GenTxId 32-byte payload (8 functions)

**24 test functions total.**

### Phase 1 — `upstream::cardano_ledger`

Pparams use `serde_json::Value` probing (typed conversion comes in Phase 3):

- `pparams_shelley_has_required_fields`
- `pparams_alonzo_has_required_fields` (+ executionUnitPrices, costModels)
- `pparams_babbage_has_required_fields`
- `pparams_conway_has_required_fields` (+ drepVotingThresholds, etc.)
- `pparams_update_alonzo_decodes`
- `pparams_update_babbage_decodes`
- `pparams_update_conway_decodes`
- `alonzo_golden_block_decodes` — `MultiEraBlock`, `tx_count > 0`
- `alonzo_golden_tx_decodes` — `MultiEraTx`, hash is 32 bytes
- `conway_golden_tx_decodes`
- `alonzo_mainnet_genesis_decodes`

**11 test functions.**

### Phase 1 — `upstream::cardano_node`

- `alonzo_genesis_spec_decodes` — cost models non-empty, execution costs sane
- `conway_genesis_spec_decodes` — DRep thresholds, pool voting thresholds present

**2 test functions.**

### Phase 2 — `upstream::cardano_ledger_cddl`

Adds the `cddl` crate (`cddl = "0.9"` or current) as a dev-dependency in dugite-conformance. Validates our generated CBOR against the canonical schema:

```rust
fn validate_cbor_against_cddl(cbor: &[u8], cddl_rule: &str) -> Result<(), String>;
```

Tests:

- `cddl_conway_tx_roundtrip_validates` — our re-encoded Alonzo/Conway txs validate against `tx` rule
- `cddl_conway_block_validates` — our re-encoded block validates against `block` rule
- `cddl_conway_header_validates` — header validates against `header` rule
- `cddl_conway_govaction_validates` — GovAction validates against `gov_action` rule
- (additional rules added as we discover gaps — proposal_procedure, voting_procedure, etc.)

Inputs: the same Alonzo/Conway golden tx/block files already pulled in Phase 1, plus synthesized CBOR via our encoders driven by proptest strategies (5-10 examples per rule).

### Phase 3 — `upstream::cardano_ledger_pparams_typed`

Introduces a `HaskellPParams<Era>` struct in `crates/dugite-conformance/src/upstream/haskell_pparams.rs` whose serde field names exactly match the Haskell JSON layout (`minfeeA`, `poolDeposit`, `executionUnitPrices`, etc.), and a conversion `impl TryFrom<HaskellPParams<Era>> for PParams { ... }` per era.

Tests:

- `haskell_pparams_shelley_decodes_strict` — typed deserialize, no `serde_json::Value` fallback
- `haskell_pparams_alonzo_decodes_strict`
- `haskell_pparams_babbage_decodes_strict`
- `haskell_pparams_conway_decodes_strict`
- `haskell_pparams_conway_roundtrip` — decode → convert → re-encode → matches input JSON modulo field order

Replaces the Phase 1 `Value` probes for the relevant fields. Phase 1 tests are kept (they catch unknown-field additions).

### Phase 4 — `upstream::amaru` (ledger-rule replay)

The highest-value addition. Amaru's `rules-conformance/` corpus (~314 vectors + 44 protocol parameter snapshots) encodes:

```
each vector = CBOR [config(arr[13]), initial_state(arr[7]), final_state(arr[7]), events(arr[N]), title(str)]
events = [Transaction[0, tx_cbor, success_bool, slot]
        | PassTick[1, slot]
        | PassEpoch[2, epoch_delta]]
protocol params referenced by hash → loaded from pparams-by-hash/<hash>
```

Components:

- **Vector parser** — `dugite-conformance/src/upstream/amaru/vector.rs` — CBOR-decode the 5-tuple into Rust structs.
- **State bridge** — `dugite-conformance/src/upstream/amaru/bridge.rs` — translate Amaru's `initial_state` into Dugite's `LedgerState` + `EpochState` + `GovState`; translate the `final_state` into a Dugite-comparable structure.
- **Replay runner** — `dugite-conformance/src/upstream/amaru/runner.rs` — apply each event in sequence to the loaded state. `Transaction` events go through `Ledger::apply_tx`; `PassTick` advances slot; `PassEpoch` triggers epoch boundary.
- **Comparator** — `dugite-conformance/src/upstream/amaru/compare.rs` — assert field-by-field equivalence of expected vs actual final state, with human-readable diff on mismatch (UTxO inserts/removes, governance proposal map, reward balances, certificate state, etc.).

Tests are file-driven — one `#[test]` per vector category (Shelley / Mary / Allegra / Alonzo / Babbage / Conway). Within each category, the runner iterates every matching CBOR file and accumulates failures into a single panic with a structured report.

```rust
#[test] fn amaru_shelley_imp_spec()          { run_all("ShelleyImpSpec/"); }
#[test] fn amaru_mary_imp_spec()             { run_all("MaryImpSpec/"); }
#[test] fn amaru_allegra_imp_spec()          { run_all("AllegraImpSpec/"); }
#[test] fn amaru_alonzo_imp_spec()           { run_all("AlonzoImpSpec/"); }
#[test] fn amaru_babbage_imp_spec()          { run_all("BabbageImpSpec/"); }
#[test] fn amaru_conway_imp_spec_v10()       { run_all("ConwayImpSpec_-_Version_10/"); }
```

**Risk:** Amaru's state model is a CBOR replica of cardano-ledger's `NewEpochState`. Translating it to Dugite's internal structures will surface model mismatches — exactly the kind of bug this conformance pass is designed to catch. Some Phase 4 work will likely involve fixing Dugite ledger logic, not just writing tests. This is in-scope and expected.

**Skip list:** We accept that on first introduction, some categories may need an explicit skip list (`SKIP_FILES: &[&str] = &[...]`) for vectors that depend on features Dugite hasn't fully implemented yet (e.g., Plutus V3 edge cases not yet in our UPLC machine). Each skip is filed as a separate issue and tracked toward zero.

### Phase 5 — `upstream::cardano_base` (VRF/KES vectors)

During Phase 5 scoping, identify the exact `cardano-base` test-vector paths. Likely sources:

- `cardano-crypto-tests/test_vectors/vrf_ver03_*` — VRF (draft-03) test vectors
- `cardano-crypto-tests/test_vectors/kes_*` — KES test vectors  
- `cardano-crypto-class/test/Test/Crypto/...` — additional crypto correctness inputs

Tests cross-validate dugite-crypto's VRF and KES implementations against these vectors. Our existing local `tests/golden/vrf/golden_tests.txt` is checked for redundancy and either retained, deduplicated, or marked as supplementary.

**Tests (preliminary; finalised during Phase 5):**
- `vrf_ver03_keypair_derivation_matches_vectors`
- `vrf_ver03_prove_matches_vectors`
- `vrf_ver03_verify_matches_vectors`
- `kes_keygen_matches_vectors`
- `kes_sign_verify_matches_vectors`
- `kes_evolution_matches_vectors`

### Phase 6 — `upstream::mithril` (certificate fixtures)

Pull Mithril test data from `input-output-hk/mithril`. Replace the current ad-hoc Mithril fixtures (`crates/dugite-node/tests/fixtures/mithril-*.json`) with the upstream-pinned set.

**Tests (preliminary; finalised during Phase 6):**
- `mithril_certificate_chain_verifies` — pull a real cert chain fixture; verify with our STM verification logic
- `mithril_aggregator_response_decodes` — replaces our current local Mithril JSON fixtures with upstream-pinned equivalents
- `mithril_signature_aggregation_matches` — STM aggregation produces upstream-byte-exact result on a known input

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

Cache key includes manifest content hash; any pin bump invalidates the cache.

For Amaru replay (large corpus, slower run), consider promoting it to a nightly job rather than per-PR if wall-clock budget becomes a concern. Decision deferred to Phase 4 implementation when actual run time is measurable.

### `justfile` recipes

```just
download-upstream-fixtures:
    cargo xtask download-upstream-fixtures

download-upstream-fixtures-repo REPO:
    cargo xtask download-upstream-fixtures --repo {{REPO}}

test-upstream:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-uplc --features upstream-conformance --test conformance
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests
```

Removes the existing `download-plutus-conformance` recipe.

### Bumping an upstream pin

1. Edit `tests/conformance/upstream/manifest.toml`: change the `sha`/`tag`. Update `files`/`paths` if upstream renamed/moved anything.
2. Run `just download-upstream-fixtures` locally.
3. Run `just test-upstream` — surfaces decoder/replay issues.
4. Fix any test fallout (may involve adapting decoder code in dugite-serialization, ledger code in dugite-ledger, etc.).
5. Commit manifest.toml + adaptation code. **No fixtures committed.** PR diff shows the SHA bump and adaptation work cleanly.

## Implementation phases

Each phase is a separate PR series with its own CI gate. Earlier phases ship infrastructure that later phases reuse.

### Phase 1 — Foundation (wire format + pparams + genesis + UPLC unification)

Smallest unit of value. Establishes the entire infrastructure (xtask, manifest, gating model, CI integration) plus three useful test areas. UPLC unification ships here so the new infrastructure replaces the old script in one go.

Estimated: 4-6 commits.

### Phase 2 — CDDL validation

Pulls `conway.cddl` (already added to cardano-ledger's `files` list in Phase 1's manifest). Adds `cddl` crate as dev-dep. New test module validates our CBOR against the schema.

Estimated: 2-3 commits.

### Phase 3 — Strict typed PParams

Code-only phase (no new fixtures). Adds `HaskellPParams<Era>` struct + conversion. Replaces Phase 1 `Value` probes for relevant fields.

Estimated: 2 commits.

### Phase 4 — Amaru ledger-rule replay (largest phase)

Adds Amaru as a source. Implements vector parser + state bridge + replay runner + comparator. Will surface real ledger bugs — fixes in dugite-ledger are in-scope. Likely involves a skip-list start that decays to zero.

Estimated: 8-15 commits over multiple PRs. Each ImpSpec category (Shelley/Mary/Allegra/Alonzo/Babbage/Conway) can be a separate PR.

### Phase 5 — VRF/KES crypto vectors

Adds `cardano-base` as a source. Cross-validates dugite-crypto.

Estimated: 2-3 commits.

### Phase 6 — Mithril certificate fixtures

Adds Mithril repo as a source. Replaces ad-hoc local Mithril fixtures.

Estimated: 2-3 commits.

## Risks and Caveats

- **GitHub rate limits.** Unauthenticated CI hits 60 req/hr. xtask reads `GITHUB_TOKEN` to bump to 5000/hr. CI provides this via `secrets.GITHUB_TOKEN`.
- **Upstream file renames.** A bump that renames a `files` entry fails with a clear "file not found in tarball" error. Manifest is updated to track.
- **CBOR envelope wrapping.** ouroboros-consensus goldens are wrapped (tag-24 blocks, `chainsync.WrappedHeader` headers). Tests use explicit unwrap helpers, documented in code.
- **Dijkstra era.** Upstream `Block_Dijkstra` is truncated — full decode not possible. Excluded; tracked as follow-up.
- **Pparams JSON schema drift.** Phase 1 uses `Value` probing. Phase 3 introduces typed deserialization. A schema drift between phases is caught by the Phase 1 tests until Phase 3 lands.
- **Renaming `conformance` feature.** Mitigated by `conformance = ["upstream-conformance"]` alias.
- **xtask compile cost.** Adding `xtask/` adds it to `cargo check --workspace`. Mitigated by excluding xtask from workspace `default-members`.
- **Amaru replay surfaces real bugs.** This is by design — the conformance corpus exists precisely to catch model mismatches. Implementation budget for Phase 4 must include time for ledger bugfixes, not just test code.
- **Amaru corpus size + CI wall clock.** 314+ vectors per run may be slow. If wall clock exceeds CI budget, promote Amaru to nightly + per-PR sample. Measured during Phase 4.
- **Skip-list discipline.** Each skip in Phase 4 must be tracked as a specific issue with reproduction info, never an opaque `// FIXME`. Goal: skip list shrinks to zero.
- **CDDL crate maturity.** The Rust `cddl` crate may have gaps versus what Conway exercises. Evaluated during Phase 2; if blocking, switch to a different validator or implement a minimal subset ourselves.

## Open questions for follow-up review

- **Should Amaru replay run per-PR or nightly?** Decided during Phase 4 based on measured wall-clock cost.
- **Exact `cardano-base` and `mithril` paths.** Confirmed during Phase 5 and Phase 6 scoping; manifest entries currently use placeholder comments.
- **Phase 5 deduplication with existing `tests/golden/vrf/`.** Decided once upstream vectors are pulled.

## Acceptance criteria

Phase 1 is considered complete when:

- `cargo xtask download-upstream-fixtures` populates all four Phase-1 fixture subdirs and writes the sentinel.
- `DUGITE_REQUIRE_UPSTREAM=1 just test-upstream` passes all Phase-1 tests on CI and locally.
- The previous `scripts/dev/download-plutus-conformance.sh` is deleted.
- The previous `crates/dugite-uplc/tests/conformance/` directory is empty / removed and UPLC tests run from the unified fixture root.
- `cargo build --workspace` passes (xtask compiles cleanly).
- CLAUDE.md is updated to document the new conformance flow.

Each subsequent phase has its own acceptance criteria defined at the start of its implementation plan.
