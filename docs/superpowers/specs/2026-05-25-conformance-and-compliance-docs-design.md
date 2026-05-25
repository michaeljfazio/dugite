# Conformance & Protocol-Compliance Documentation Rework

**Date:** 2026-05-25
**Status:** Draft — awaiting user review

## Context

Dugite's correctness story is told in two places, and both are currently
under-served:

1. The mdBook (published as GitHub Pages at
   `https://michaeljfazio.github.io/dugite/`) has a `Benchmarks` page in
   the Reference section but no equivalent page for **conformance testing**.
   The `dugite-conformance` crate, the seven-area upstream corpus pinned via
   `tests/conformance/upstream/manifest.toml`, and the 100% UPLC conformance
   shipped in v1.7.0 are not documented anywhere a reader of the docs site
   can find them.

2. The GitHub Wiki (separate `dugite.wiki.git` repo) has a
   `Protocol-Compliance.md` page that is **stale and incomplete**. It carries
   point-in-time validation numbers ("4,111,000+ blocks synced",
   "v0.3.0-alpha mainnet sync"), date-stamped row annotations ("fixed
   2026-04-21", "verified on-chain 2026-05-04"), and predates a number of
   capabilities the node now ships (CSJ Phase A, DNS SRV, defensive parse
   bounds from the May security audit, pre-Conway PPUP decoder, byte-exact
   reward calculation, UPLC 100% conformance).

This change produces a single, coherent answer to the question "how is
dugite's compatibility with cardano-node documented?" by:

- adding a dedicated mdBook page describing the **conformance test suite**
  (the *how do we verify it* story);
- reworking the wiki page to describe **protocol feature compatibility** at
  the current state of `main` (the *what does it support* story);
- cross-linking the two so each side points at the other.

The intended outcome is that a reader landing on either surface can get an
accurate, current picture of dugite's protocol coverage and the evidence
backing it, without having to read commit history or open issues.

## Artefact 1 — New mdBook page: `docs/src/reference/conformance.md`

### Placement

Inserted into `docs/src/SUMMARY.md` immediately above the existing
`Benchmarks` entry, so the sidebar shows the correctness/performance
evidence pair adjacent:

```
- [Protocol Parameters](./reference/protocol-parameters.md)
- [Mini-Protocol Reference](./reference/mini-protocols.md)
- [Upgrading](./reference/upgrading.md)
- [Conformance Test Suite](./reference/conformance.md)   ← NEW
- [Benchmarks](./reference/benchmarks.md)
- [Third-Party Licenses](./reference/third-party-licenses.md)
- [Troubleshooting](./reference/troubleshooting.md)
```

### Page structure

1. **Overview** (~150 words). Define "conformance" as byte-exact alignment
   vs upstream Cardano artefacts and why that's the cornerstone of the
   project's correctness claim. State that conformance is one of three
   layers (the others being feature-compatibility, documented in the wiki
   `Protocol-Compliance` page, and operational soak testing).

2. **The corpus model** (~200 words + diagram). Describe the pipeline:

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

   Explain why pinning happens at the release-tag level (reproducibility,
   single fetch artefact, cacheable by content hash).

3. **Headline scoreboard** — one-line-per-area table:

   | Area | Source | Coverage | Status |
   |---|---|---|---|
   | UPLC (Plutus) | IntersectMBO/plutus | 999 evaluation cases | 100% — skip list empty |
   | ouroboros-consensus | IntersectMBO/ouroboros-consensus | block/header goldens per era | passing |
   | cardano-ledger | IntersectMBO/cardano-ledger | genesis JSON + CDDL + golden txs | passing |
   | cardano-node | IntersectMBO/cardano-node | genesis specs | passing |
   | ledger-rules (ImpSpec) | IntersectMBO/cardano-ledger | CBOR NEWEPOCH + LEDGER vectors | passing — `SKIP_LIST` empty |
   | cardano-base | IntersectMBO/cardano-base | VRF v03 vectors | passing |
   | mithril | input-output-hk/mithril | certificate fixtures | passing |

4. **Per-area sections** (one `##` per area, ~150-250 words each, in the
   order they appear in the scoreboard). Each section contains:

   - **Source** — upstream repo + the SHA pin from `sources.toml`
   - **What's validated** — the fixtures and the predicate exercised
   - **Current status** — pass / skip count, link to skip-list if non-empty
   - **How to replay locally** — the exact `just conformance-<area>` recipe

   Sources for the per-area content: `tests/conformance/upstream/{cardano_base.rs, cardano_ledger.rs, cardano_node.rs, mithril.rs, ouroboros_consensus.rs, ledger_rules_replay/mod.rs}`
   for harness behaviour, and the `justfile` lines 287-308 for recipe names.

5. **CI integration** (~100 words). Describe the `upstream-conformance` job
   in `.github/workflows/ci.yml`, the `DUGITE_REQUIRE_UPSTREAM=1` gate, and
   the fixture cache keyed on the `manifest.toml` content hash.

6. **Updating the corpus** (~80 words). Procedure: edit `sources.toml` →
   trigger the regenerate workflow → bump `[release].tag` in `manifest.toml`
   → `just download-upstream-fixtures && just test-upstream` → commit
   `sources.toml` + `manifest.toml`.

7. **See also** — link to the wiki `Protocol-Compliance` page (feature-level
   compatibility) and the mdBook `Benchmarks` page (performance evidence).

### Maintenance posture

Static prose. Numbers and links update only when the corpus tag bumps or a
skip-list grows. No CI auto-generation is in scope.

### Out of scope

- Epoch-diff Haskell cross-validation (different workflow, different
  artefacts; belongs in `architecture/ledger.md` or its own future page).
- The `dugite-conformance` Agda-spec test vectors under `tests/conformance/vectors/`
  (the original spec-conformance harness predating the upstream corpus —
  may be folded in later, but is not what this page is about).

## Artefact 2 — Reworked GitHub Wiki page: `Protocol-Compliance.md`

The page lives in the separate `dugite.wiki.git` repository (clone:
`https://github.com/michaeljfazio/dugite.wiki.git`). All edits happen in a
local clone of that repo; the implementation step will commit + push there
rather than into this tree.

### What to remove

- "Last updated 2026-05-23" header line (date stamps rot)
- Entire `## Validation Status` section (preview block count, mainnet
  v0.3.0-alpha sync numbers, SAND pool ID — point-in-time data the user has
  explicitly asked to drop)
- Per-row date-stamped status annotations:
  - "verified loopback BP+relay 2026-04-29"
  - "fixed 2026-04-21"
  - "fixed 2026-04-22"
  - "Yes — verified on-chain 2026-05-04 (accepted by cardano-node 10.6.2 relay, orphaned by slot-battle)"
  - "Soak testing — Sandstone σ ≈ 0.0000247, ~0.1 blocks/epoch"
- `## Tooling` row "Soak test script (scripts/soak-test.sh)" (not a
  protocol-compliance fact)
- The inline note about `Hash28::to_hash32_padded()` in the page header
  (internal coding gotcha, not a compatibility statement)

### What to keep, cleaned up

All seven feature tables (`Consensus`, `Ledger`, `Governance`, `Networking`,
`CLI`, `Block Production`, `Tooling`) survive, but every row is rephrased as
a capability statement (no dates, no "fixed", no PR/issue numbers inline).

The "Status" column reading `Yes` for every row is replaced with a
`Details` column containing a one-line technical anchor (e.g. for
"Pipelined ChainSync" → "default depth 300, configurable via
`DUGITE_PIPELINE_DEPTH`"). Rows where there is genuinely nothing to add
become bullet lists instead.

### What to add

#### `## Overview`

Short prose: what "protocol compliance" means in this project (feature-level
parity with cardano-node 11.0.x at the current network protocol version,
backed by byte-exact wire-format compatibility); how it's demonstrated
(conformance suite, Haskell ledger-state cross-validation, soak testing);
"See also" pointer to the mdBook conformance page.

#### `## Supported Protocol Versions`

Small table listing PV 1-11 by era, each marked with dugite's level of
support. Call out that preview is at PV11 today and requires cardano-node
11.0.1+ peers.

| PV | Era | Status |
|---|---|---|
| 1 | Byron | full |
| 2 | Shelley | full |
| ... | ... | ... |
| 9-10 | Conway | full |
| 11 | Conway (preview) | full |

#### `## Eras`

7-row table (Byron → Conway) confirming end-to-end support, with one-line
notes on era-specific gotchas where they exist (e.g. Byron EBB chain
continuity).

#### `## Cryptography`

- Ed25519 / Ed25519-Extended (key derivation)
- VRF (Praos v03 batch-compatible — `cardano-base` conformance vectors pass)
- KES (Sum6Kes)
- Blake2b-224 / Blake2b-256
- BLS12-381 (CIP-381 builtins)
- Schnorr Secp256k1 / BIP-340 (`verify_raw`, not the SHA-256-wrapped form)
- Plutus builtin crypto coverage (verify\_ed25519\_signature,
  verify\_ecdsa\_secp256k1\_signature, verify\_schnorr\_secp256k1\_signature,
  bls12\_381\_\*, keccak\_256, ripemd\_160)

#### `## Plutus Script Evaluation`

In-house UPLC CEK machine. V1/V2/V3 supported. 100% conformance on the
official 999-case suite (skip list empty as of v1.7.0). Cost model wired
per language version and machine. CIP-122 bit ordering, BLS LE scalar +
null aug, case-on-Const. Link to the mdBook conformance page for the
suite-by-suite breakdown.

#### `## Serialization`

In-house multi-era CBOR decoder (`dugite-serialization`). Strict by
default: every wire-level decode runs a defensive size-bounds check before
crypto verification (the May 2026 audit class — `check_header_field_sizes`
+ inline defence-in-depth at five header sites). Byron-era block-header
decode hardening. Pre-Conway PPUP decoder applies update proposals for
Shelley/Allegra/Mary/Alonzo/Babbage tx body key 6.

#### Expansions to existing tables

- `## Networking` — add: DNS SRV peer discovery (v1.7.0), CSJ Phase A
  genesis sync mode (v1.7.0; runtime gate via `--consensus-mode genesis`),
  inbound per-IP rate limiting, peer-sharing outbound-only.
- `## Ledger` — add: pre-Conway PPUP application (#624), canonical
  `isOverlaySlot` check (replaces the old `d_value < 0.8` shortcut), RUPD
  consumes `prevPParams` (byte-exact reward calculation, #438), HFC PV bump
  on era transition (#481).
- `## Consensus` — add: HFC era transitions with explicit PV bumps
  (Byron→Shelley through Babbage→Conway), Conway RestrictedVRFTiebreaker.

#### `## Defensive Controls`

Single short paragraph summarising the security-audit class of fixes:
malformed-frame rejection at parse boundary, defensive bounds checks at
header-decode sites, body-hash algorithm parity, mempool/apply parity. No
per-issue dump.

#### `## Verification`

Closing section pointing out:

- mdBook conformance page → the upstream test suite (proves wire-format /
  evaluation / decoder fidelity)
- mdBook benchmarks page → performance evidence
- Wiki `Known-Issues` page → open gaps and follow-ups

### Style rules

- Capability statements, never status updates.
- No dates, no PR/issue numbers inline (they rot; links live only in the
  `Verification → Known Issues` pointer).
- Single source of truth for any time-stamped or "fixed-on" claim is the
  git log and the Known-Issues page, not this one.

## Maintenance and ownership

- mdBook conformance page: lives in this repo; bumps when corpus tag
  bumps or skip-list changes.
- Wiki page: lives in `dugite.wiki.git`; bumps when a new protocol-level
  capability lands (HFC bump, new era, new builtin, new mini-protocol
  version, new defensive control class). Per-bugfix churn stays out of
  this page by design.

## Verification

This is documentation work, so verification is by reading rather than
running:

1. The new mdBook page builds in the local `mdbook serve` and links to
   `benchmarks.md` and to the wiki are resolvable.
2. The reworked wiki page renders cleanly on the GitHub wiki preview, all
   internal `[[…]]` and external links resolve, no stale dates / issue
   numbers remain (`grep -nE '202[0-9]-[0-9]{2}-[0-9]{2}|#[0-9]+|fixed|verified on-chain'`
   over the file returns no hits in body text).
3. Every entry in the new "Supported Protocol Versions", "Eras",
   "Cryptography", and "Plutus Script Evaluation" sections has a
   corresponding code path in the workspace (spot-check by `grep`).
4. The Wiki `Home.md` "Wiki Pages" table entry for `Protocol Compliance`
   still resolves; no other wiki page is touched.

## Out of scope

- Wiki pages other than `Protocol-Compliance.md` (Home, Known-Issues,
  Performance-Baselines, ADRs, Getting-Started-for-Developers).
- Any change to the `dugite-conformance` crate itself.
- CI auto-generation of the conformance scoreboard.
- The existing `docs/src/reference/benchmarks.md` content (this design
  only places a new page adjacent to it).
