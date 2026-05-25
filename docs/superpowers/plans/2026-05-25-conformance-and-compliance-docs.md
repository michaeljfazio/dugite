# Conformance & Protocol-Compliance Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new mdBook page documenting the dugite-conformance test suite (placed adjacent to Benchmarks) and rework the GitHub Wiki `Protocol-Compliance.md` page so it reflects the current state of `main` without point-in-time validation numbers.

**Architecture:** Two artefacts in two different git repos. The mdBook page (`docs/src/reference/conformance.md`) lives in this repo and ships with the docs build. The Wiki page (`Protocol-Compliance.md`) lives in the separate `dugite.wiki.git` repo and is edited via a local clone, then committed + pushed there. The two pages cross-link.

**Tech Stack:** mdBook (the project's existing docs toolchain — `mdbook build` / `mdbook serve`); plain Markdown for the wiki.

**Reference spec:** `docs/superpowers/specs/2026-05-25-conformance-and-compliance-docs-design.md`

---

### Task 1: Scaffold `docs/src/reference/conformance.md` with Overview + Corpus Model + Scoreboard

**Files:**
- Create: `docs/src/reference/conformance.md`

- [ ] **Step 1: Create the file with the Overview, Corpus Model, and Scoreboard sections**

Create `docs/src/reference/conformance.md` with this exact content:

````markdown
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

````

- [ ] **Step 2: Commit the scaffold**

```bash
git add docs/src/reference/conformance.md
git commit -m "docs(conformance): scaffold conformance test suite page"
```

---

### Task 2: Add the seven per-area sections to `conformance.md`

**Files:**
- Modify: `docs/src/reference/conformance.md` (append)

- [ ] **Step 1: Append the per-area sections**

Append the following content to `docs/src/reference/conformance.md` (after the Status table from Task 1):

````markdown

## Per-area detail

### UPLC (Plutus)

**Source:** [IntersectMBO/plutus](https://github.com/IntersectMBO/plutus), pinned to tag `1.65.0.0` in `sources.toml`.

**What's validated:** 999 evaluation test cases from `plutus-conformance/test-cases/uplc/evaluation/`. Each test case provides a UPLC program and the expected result (a term, a budget exhaustion, or a specific runtime error). The dugite-uplc CEK machine evaluates each program and the harness compares term-for-term, budget-for-budget against the expected output.

**Status:** 100% passing. The skip list is empty as of v1.7.0. The harness covers normalisation by evaluation (NbE) readback, per-builtin cost model wiring, CIP-122 bit ordering, BLS LE scalar handling with null augmentation, and BIP-340 `verify_raw` semantics (not the SHA-256-wrapped `verify`).

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-uplc
```

### ouroboros-consensus

**Source:** [IntersectMBO/ouroboros-consensus](https://github.com/IntersectMBO/ouroboros-consensus), SHA-pinned in `sources.toml`.

**What's validated:** Era-tagged golden files for Cardano blocks and headers. The harness exercises the in-house multi-era CBOR decoder against fixtures captured directly from the upstream Haskell encoders, asserting round-trip and structural equivalence per era.

**Status:** passing across all eras (Byron, Shelley, Allegra, Mary, Alonzo, Babbage, Conway).

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-ouroboros-consensus
```

### cardano-ledger

**Source:** [IntersectMBO/cardano-ledger](https://github.com/IntersectMBO/cardano-ledger), SHA-pinned in `sources.toml`.

**What's validated:** Three classes of fixture. Genesis JSON for each era is parsed and structurally compared. The CDDL schema is loaded and exercised against representative documents. Golden transaction CBOR is decoded and asserted for byte-equality on re-encode.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-cardano-ledger
```

### cardano-node

**Source:** [IntersectMBO/cardano-node](https://github.com/IntersectMBO/cardano-node), SHA-pinned in `sources.toml`.

**What's validated:** Genesis spec files (`shelley-genesis.json`, `alonzo-genesis.json`, `conway-genesis.json` and their Byron counterpart). The harness asserts that dugite parses each spec into its internal genesis types and that the resulting types preserve every documented field.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-cardano-node
```

### ledger-rules (ImpSpec)

**Source:** [IntersectMBO/cardano-ledger](https://github.com/IntersectMBO/cardano-ledger) ImpSpec, SHA-pinned in `sources.toml`. The corpus regeneration pipeline builds cardano-ledger from source (GHC 9.6.5 + cabal 3.10.x, ≈35 min cold / 5 min cached) and runs the upstream ImpSpec conformance suite with `CONFORMANCE_CBOR_DUMP_PATH` set to capture every test vector as CBOR.

**What's validated:** Two STS-rule families — NEWEPOCH (epoch-boundary transitions) and LEDGER (transaction application). The harness replays each captured CBOR vector through the corresponding dugite ledger code path and compares the resulting state byte-for-byte.

**Status:** passing. `SKIP_LIST` in `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` is empty.

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-ledger-rules
```

### cardano-base

**Source:** [IntersectMBO/cardano-base](https://github.com/IntersectMBO/cardano-base), SHA-pinned in `sources.toml`.

**What's validated:** VRF v03 test vectors. Each vector ships an input message, a signing key, an expected proof, and an expected output hash. The harness exercises the dugite VRF implementation against every vector and asserts byte-equality on both the proof and the output, which is what guarantees Praos-compatible leader election.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-cardano-base
```

### mithril

**Source:** [input-output-hk/mithril](https://github.com/input-output-hk/mithril), SHA-pinned in `sources.toml`.

**What's validated:** Mithril certificate fixture JSON. The harness loads each certificate, verifies the aggregate signature, and asserts structural equivalence against the upstream-captured form.

**Status:** passing.

**Replay locally:**

```bash
just download-upstream-fixtures
just conformance-mithril
```
````

- [ ] **Step 2: Commit**

```bash
git add docs/src/reference/conformance.md
git commit -m "docs(conformance): add per-area detail sections"
```

---

### Task 3: Add CI integration, updating procedure, and See-also tail to `conformance.md`

**Files:**
- Modify: `docs/src/reference/conformance.md` (append)

- [ ] **Step 1: Append the trailing sections**

Append the following content to `docs/src/reference/conformance.md`:

````markdown

## CI integration

The `upstream-conformance` job in `.github/workflows/ci.yml` runs both the UPLC suite and the upstream tests with the `DUGITE_REQUIRE_UPSTREAM=1` environment variable set. This variable makes a missing fixture a hard failure rather than a silent skip — the gate exists specifically to stop the suite from quietly degrading to a no-op when something is wrong with the fixture cache or download.

Fixture tarballs are cached on the CI runner, keyed by the SHA-256 content hash of `tests/conformance/upstream/manifest.toml`. Bumping `[release].tag` in that file invalidates the cache automatically; no separate cache-bust step is needed.

## Updating the corpus

To adopt a new upstream version:

1. Edit `tests/conformance/upstream/sources.toml`, bumping the SHA (or tag for the `plutus` area) of the area you want to refresh.
2. Trigger the `regenerate-conformance-corpus` workflow on GitHub (manual dispatch, or wait for the weekly automatic run). It produces a new dugite release tagged `conformance-corpus-v<timestamp>` with the seven tarballs attached.
3. Update `[release].tag` in `tests/conformance/upstream/manifest.toml` to point at the new release tag.
4. Run `just download-upstream-fixtures && just test-upstream` locally.
5. Fix any test fallout, then commit the `sources.toml` + `manifest.toml` updates together with the code changes.

## See also

- [Benchmarks](./benchmarks.md) — performance evidence.
- Wiki [Protocol Compliance](https://github.com/michaeljfazio/dugite/wiki/Protocol-Compliance) — feature-by-feature compatibility catalogue.
- Wiki [Known Issues](https://github.com/michaeljfazio/dugite/wiki/Known-Issues) — open gaps and follow-ups.
````

- [ ] **Step 2: Commit**

```bash
git add docs/src/reference/conformance.md
git commit -m "docs(conformance): add CI, updating, and see-also sections"
```

---

### Task 4: Wire the new page into `SUMMARY.md`

**Files:**
- Modify: `docs/src/SUMMARY.md`

- [ ] **Step 1: Insert the new entry above Benchmarks**

Edit `docs/src/SUMMARY.md`, replacing this line:

```markdown
- [Upgrading](./reference/upgrading.md)
- [Benchmarks](./reference/benchmarks.md)
```

with:

```markdown
- [Upgrading](./reference/upgrading.md)
- [Conformance Test Suite](./reference/conformance.md)
- [Benchmarks](./reference/benchmarks.md)
```

- [ ] **Step 2: Commit**

```bash
git add docs/src/SUMMARY.md
git commit -m "docs(summary): add Conformance Test Suite entry"
```

---

### Task 5: Verify the mdBook build

**Files:** (read-only verification)

- [ ] **Step 1: Build the book**

```bash
cd /Users/michaelfazio/Source/dugite/docs && mdbook build
```

Expected: completes with no errors. The output ends with a line like
`Book built successfully`. (If `mdbook` is not on `PATH`, install via
`cargo install mdbook` or `brew install mdbook`.)

- [ ] **Step 2: Verify the conformance page rendered into the book**

```bash
ls /Users/michaelfazio/Source/dugite/docs/book/reference/conformance.html
```

Expected: the file exists.

- [ ] **Step 3: Check the sidebar order**

```bash
grep -A1 "reference/upgrading" /Users/michaelfazio/Source/dugite/docs/book/toc.html | head -20
```

Expected: the entry for `reference/conformance.html` appears between `reference/upgrading.html` and `reference/benchmarks.html`.

- [ ] **Step 4: Check internal links resolve**

```bash
grep -o 'href="[^"]*"' /Users/michaelfazio/Source/dugite/docs/book/reference/conformance.html | sort -u
```

Expected: links to `./benchmarks.html` (relative, internal) resolve; external `github.com/michaeljfazio/dugite/wiki/...` links are present as-is.

---

### Task 6: Clone the wiki repo into a working location

**Files:**
- Create (outside this repo): `/tmp/dugite-wiki/` working clone

- [ ] **Step 1: Fresh clone the wiki repo**

```bash
rm -rf /tmp/dugite-wiki
git clone https://github.com/michaeljfazio/dugite.wiki.git /tmp/dugite-wiki
ls /tmp/dugite-wiki/Protocol-Compliance.md
```

Expected: clone succeeds; the file exists.

- [ ] **Step 2: Confirm git status is clean**

```bash
cd /tmp/dugite-wiki && git status
```

Expected: `nothing to commit, working tree clean`.

---

### Task 7: Rewrite `Protocol-Compliance.md`

**Files:**
- Overwrite: `/tmp/dugite-wiki/Protocol-Compliance.md`

- [ ] **Step 1: Overwrite the file with the new content**

Overwrite `/tmp/dugite-wiki/Protocol-Compliance.md` with this exact content:

````markdown
# Protocol Compliance

This page catalogues dugite's compatibility with the Cardano protocol at the level of features and capabilities. It complements two other surfaces:

- The mdBook [Conformance Test Suite](https://michaeljfazio.github.io/dugite/reference/conformance.html) page describes the upstream test corpus that verifies wire-format and evaluation fidelity.
- The wiki [Known Issues](Known-Issues) page tracks open gaps and follow-ups.

Everything documented here is verified against the cardano-node 11.0.x reference at the current network protocol version.

## Overview

Dugite targets feature-level parity with `cardano-node` at the current network protocol version (PV11 on preview as of this writing), backed by byte-exact wire-format compatibility via an in-house multi-era CBOR decoder. Parity is demonstrated by:

- the upstream conformance test corpus (see the mdBook conformance page);
- per-epoch ledger-state cross-validation against `cardano-cli debug log-epoch-state` from a synced cardano-node;
- continuous soak testing on the local devnet and on the public preview / preprod testnets, including block production as Sandstone Pool [SAND].

## Supported protocol versions

| PV | Era | Status |
|---|---|---|
| 1 | Byron | full |
| 2 | Shelley | full |
| 3 | Allegra | full |
| 4 | Mary | full |
| 5 | Alonzo | full |
| 6 | Alonzo (intra-era) | full |
| 7 | Babbage | full |
| 8 | Babbage (intra-era) | full |
| 9 | Conway | full |
| 10 | Conway (intra-era) | full |
| 11 | Conway (preview) | full |

Preview is at PV11 today and requires cardano-node 11.0.1+ peers.

## Eras

| Era | Status | Notes |
|---|---|---|
| Byron | full | EBB chain continuity preserved across the Byron → Shelley boundary |
| Shelley | full | — |
| Allegra | full | — |
| Mary | full | Multi-asset support |
| Alonzo | full | Plutus V1, collateral, ex-units |
| Babbage | full | Plutus V2, inline datums, reference scripts, reference inputs |
| Conway | full | Plutus V3, CIP-1694 governance, DRep delegation, CIP-381 builtins |

## Cryptography

- Ed25519 and Ed25519-Extended (key derivation, transaction witnesses)
- VRF — Praos v03 batch-compatible. The `cardano-base` upstream VRF test vectors pass byte-for-byte.
- KES — Sum6Kes (operational-key signatures over block headers)
- Blake2b-224 and Blake2b-256 (hashes)
- BLS12-381 — pairing, G1/G2 add/scal-mul/compress, used by Plutus V3 CIP-381 builtins
- Schnorr Secp256k1 — BIP-340 `verify_raw` semantics, used by the Plutus `verifySchnorrSecp256k1Signature` builtin
- Plutus builtin crypto coverage: `verifyEd25519Signature`, `verifyEcdsaSecp256k1Signature`, `verifySchnorrSecp256k1Signature`, all `bls12_381_*` builtins, `keccak_256`, `ripemd_160`

## Plutus script evaluation

In-house UPLC CEK machine (`dugite-uplc`). Supports script languages V1, V2, and V3. The 999-case upstream evaluation suite passes 100% with an empty skip list (see the mdBook [Conformance Test Suite](https://michaeljfazio.github.io/dugite/reference/conformance.html) page for the suite-by-suite breakdown).

The CEK machine wires per-language and per-machine cost models, applies CIP-122 bit ordering, handles BLS little-endian scalar encoding with null augmentation, and implements `case`-on-`Const`. The PlutusV2 cost-model fallback uses the pre-Babbage default to remain consistent with cardano-node on pre-ParameterChange chains.

## Serialization

In-house multi-era CBOR decoder (`dugite-serialization`). Strict by default:

- Defensive size-bound checks on every wire-level decode field, run before any crypto verification. Header decode invokes `check_header_field_sizes` at five sites where a malformed frame would otherwise bypass VRF leader, VRF binding, or opcert verification.
- Byron-era block-header decode hardening (issue #613-class).
- Pre-Conway PPUP decoder applies update proposals carried in tx body key 6 for Shelley, Allegra, Mary, Alonzo, and Babbage.
- All hash-typed fields are length-checked at decode; 28-byte hashes are not silently widened to 32 bytes.

## Consensus

- Ouroboros Praos leader election (VRF + active-slot coefficient)
- KES signature validation (Sum6Kes)
- Epoch nonce computation (per-block, cross-epoch batches, candidate windowing)
- Chain selection — longest chain with Conway `RestrictedVRFTiebreaker`
- Epoch transitions — mark / set / go snapshot model, reward distribution from the go snapshot
- Hard-fork combinator (HFC) era transitions with explicit protocol-version bumps: Byron→Shelley, Shelley→Allegra, Allegra→Mary, Mary→Alonzo, Alonzo→Babbage, Babbage→Conway
- RUPD (deferred reward) timing — consumes `prevPParams`, byte-exact reward calculation
- `isOverlaySlot` canonical check (replaces the older `d_value` shortcut)
- Byron EBB chain continuity
- Limit of Eagerness (LoE) enforcement
- GSM (Genesis State Machine) tip-age tracking
- CSJ (Chain-Sync Jumping) Phase A genesis sync mode (gated via `--consensus-mode genesis`)
- Fork recovery — 3-peer threshold before Origin reset

## Ledger

- Phase-1 transaction validation across all eras
- Phase-2 Plutus V1 / V2 / V3 evaluation with per-redeemer Unit check
- Block-level ExUnits budget enforcement
- Reference script size limit enforcement
- CIP-0112 tiered reference-script fees
- Vote and Propose redeemer validation
- Datum witness validation; inline datums; reference inputs
- Treasury withdrawal Phase-1 validation
- Script-data-hash enforcement (reference scripts included)
- Native script evaluation (timelock, all/any/m-of-n)
- Fee calculation with the `is_valid` byte excluded from the hash
- Byron UTxO validation
- Certificate processing — all Conway certificate types
- Credential type discrimination (key vs script)
- Mempool — input-conflict checking, TTL sweep, epoch-boundary revalidation
- Invalid transactions (`is_valid: false`) consume collateral, add `collateral_return`, skip regular inputs and outputs

## Governance (CIP-1694)

- DRep registration, voting, and delegation
- Constitutional committee membership and voting
- All seven governance action types
- Ratification thresholds — DRep / SPO / CC, varying by action type
- Proposal lifecycle with deposit refunds on enactment, expiry, or removal
- Constitution tracking

## Networking

- Ouroboros mini-protocols — ChainSync, BlockFetch, TxSubmission2, KeepAlive
- N2N protocol versions V14 / V15
- N2C protocol versions V16 through V22
- All 39 LocalStateQuery query tags
- Full-duplex N2N connections (`DuplexPeerConnection`)
- TxSubmission2 responder on every N2N connection
- Pipelined ChainSync — default depth 300, configurable via `DUGITE_PIPELINE_DEPTH`
- Multi-peer BlockFetch — four concurrent fetchers
- Peer sharing — outbound-only by default
- Ledger-based peer discovery — SPO relay addresses extracted from `pool_params` past `useLedgerAfterSlot`
- DNS SRV peer discovery
- Inbound per-IP rate limiting on the listener accept loop
- Block relay to downstream peers — ConnectionId tuple keying, Overwritten simultaneous-open semantics, SO_REUSEPORT listener
- Peer rollback isolation — `MsgRollBackward` does not regress the global ledger
- Fork switch to immutable-tip anchor
- Governor — concurrency cap, dedup, circuit breaker

## CLI

- 38+ cardano-cli compatible subcommands
- Transaction build with auto-balance and Plutus script-witness support
- Leadership-schedule computation
- `stake-pool metadata-hash`
- `transaction calculate-min-fee` — reference scripts and ExUnits supported
- `transaction calculate-min-required-utxo`
- `query slot-number`
- `query kes-period-info`
- cncli compatibility — all 8 commands

## Block production

- VRF proof generation
- KES signing with operational-certificate counter tracking
- Block forging pipeline (`pipeline_depth` guard removed)
- Block announcement on every connected peer
- Live block production verified on preview as Sandstone Pool [SAND]

## Tooling

- `dugite-monitor` — ratatui terminal dashboard, real-time metrics polled from Prometheus
- `dugite-config` — interactive TUI configuration editor with tree navigation, inline editing, type validation, and diff view
- Prometheus metrics — 28+ metrics, default port 12798
- Grafana dashboard (`config/grafana-dashboard.json`)
- Koios cross-validation

## Defensive controls

Every external input — N2N frames, N2C frames, CBOR payloads, blocks, and transactions — is treated as adversarial. Parse-boundary rejection over silent skip. Defensive bounds checks run before any cryptographic verification. Body-hash algorithm parity and mempool/apply parity are enforced. Defence-in-depth applies at every layer; a single malformed message cannot bypass downstream validation.

## Verification

- [Conformance Test Suite](https://michaeljfazio.github.io/dugite/reference/conformance.html) — the upstream replay corpus.
- [Benchmarks](https://michaeljfazio.github.io/dugite/reference/benchmarks.html) — performance evidence.
- [Known Issues](Known-Issues) — open gaps and follow-ups.
````

- [ ] **Step 2: Verify no stale dates or PR/issue numbers remain in body text**

```bash
grep -nE '202[0-9]-[0-9]{2}-[0-9]{2}|\bfixed\b|verified on-chain|#[0-9]{3,}' /tmp/dugite-wiki/Protocol-Compliance.md
```

Expected: hits ONLY on the references inside parenthetical text such as `(issue #613-class)` — these are intentional category labels, not status anchors. No date strings of the form `YYYY-MM-DD`. No `fixed YYYY-…` annotations. If any other matches appear, rephrase them out.

- [ ] **Step 3: Verify all external mdBook links resolve**

```bash
grep -oE 'https://michaeljfazio\.github\.io/dugite/[^)"]+' /tmp/dugite-wiki/Protocol-Compliance.md | sort -u
```

Expected: only `reference/conformance.html` and `reference/benchmarks.html`. (The first will resolve once the docs site rebuilds; the second already exists.)

---

### Task 8: Commit and push the wiki page

**Files:**
- `/tmp/dugite-wiki/Protocol-Compliance.md`

- [ ] **Step 1: Stage and review the diff**

```bash
cd /tmp/dugite-wiki && git diff --stat Protocol-Compliance.md
cd /tmp/dugite-wiki && git diff Protocol-Compliance.md | head -100
```

Expected: only `Protocol-Compliance.md` modified; the diff shows the old `Validation Status` section gone and the new `Overview` / `Supported protocol versions` / `Eras` / `Cryptography` / `Plutus script evaluation` / `Serialization` / `Defensive controls` / `Verification` sections added.

- [ ] **Step 2: Commit**

```bash
cd /tmp/dugite-wiki && git add Protocol-Compliance.md
cd /tmp/dugite-wiki && git commit -m "Rework Protocol Compliance page: drop stale validation numbers, add comprehensive feature catalogue

- Remove point-in-time Validation Status section (preview block count, mainnet v0.3.0-alpha sync, SAND pool ID)
- Drop date-stamped row annotations (fixed YYYY-MM-DD, verified on-chain YYYY-MM-DD)
- Add Overview, Supported Protocol Versions, Eras, Cryptography, Plutus Script Evaluation, Serialization, Defensive Controls, and Verification sections
- Cross-link the mdBook Conformance Test Suite and Benchmarks pages
- Rephrase all rows as capability statements (no dates, no PR/issue numbers inline)"
```

- [ ] **Step 3: Push to the wiki remote**

```bash
cd /tmp/dugite-wiki && git push origin master
```

Expected: push succeeds. (This is the only push step in this plan; check with the user first if the working environment normally requires manual approval for pushes to external remotes.)

---

### Task 9: Final cross-check

**Files:** (read-only)

- [ ] **Step 1: Confirm mdBook page references are reciprocal**

```bash
grep -l "Protocol Compliance" /Users/michaelfazio/Source/dugite/docs/src/reference/conformance.md
grep -l "Conformance Test Suite\|conformance.html" /tmp/dugite-wiki/Protocol-Compliance.md
```

Expected: both `grep` commands print the file path (i.e. each page mentions the other).

- [ ] **Step 2: Confirm SUMMARY.md ordering**

```bash
grep -nE "(upgrading|conformance|benchmarks)\.md" /Users/michaelfazio/Source/dugite/docs/src/SUMMARY.md
```

Expected output (line numbers will vary):

```
NN:- [Upgrading](./reference/upgrading.md)
NN:- [Conformance Test Suite](./reference/conformance.md)
NN:- [Benchmarks](./reference/benchmarks.md)
```

- [ ] **Step 3: Push mdBook commits to remote (only after user confirms)**

```bash
cd /Users/michaelfazio/Source/dugite && git log --oneline origin/main..HEAD
```

Expected: lists exactly the four docs commits added by Tasks 1, 2, 3, and 4. Ask the user before running `git push`.

---

## Self-review notes

- **Spec coverage:** Tasks 1-5 cover artefact 1 (mdBook page); Tasks 6-8 cover artefact 2 (wiki page); Task 9 verifies the cross-linking the spec requires. Every spec section maps to at least one task.
- **No placeholders:** every step contains exact file paths, exact commands, and the full text to write. No `TBD` / `TODO` / `similar to Task N`.
- **Push posture:** the only remote push step (Task 8.3 for the wiki, and the deferred mdBook push in Task 9.3) explicitly asks for user confirmation, since the wiki repo is a separate external remote.
- **Out-of-scope guard:** no edits to `benchmarks.md`, no changes to the `dugite-conformance` crate, no edits to other wiki pages.
