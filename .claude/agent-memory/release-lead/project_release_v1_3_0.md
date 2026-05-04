---
name: v1.3.0 release
description: Release details for v1.3.0 (2026-05-02) — ChainSel correctness, ledger parity, network stability
type: project
---

Released v1.3.0 from commit `ec7d9c333` (version bump commit) on 2026-05-02.

**Why:** Minor version bump covering the large body of work since v1.1.0-alpha.1: full ChainSel fork-switch alignment with Haskell, 100% predicate parity with cardano-ledger, LedgerSeq Subsystem 4 rollback path, block diffusion correctness (duplex N2N), OCert signature fix, Conway governance ratification pipeline, and numerous node stability fixes validated on preview testnet (Sandstone Pool SAND).

**How to apply:** When generating the next changelog, note v1.2.0 was a tag that pointed at the same tree as v1.3.0's predecessor. The v1.3.0 version bump commit `ec7d9c333` carries no feature content — the features are all in the commits between v1.1.0-alpha.1 and v1.2.0/v1.3.0.

## Key items in this release

### Features
- `feat(ledger,consensus)`: 100% predicate parity with Haskell cardano-ledger (#449)
- `feat(node)`: wire LedgerSeq::rollback as primary rollback path (Subsystem 4)
- `feat(ledger)`: wire LedgerSeq into sync pipeline with lightweight checkpoints (#430, #432)
- `feat(ledger)`: wire governance pipeline into ConwayRules::process_epoch_transition
- `feat(ledger)`: wire RUPD computation into ShelleyRules::process_epoch_transition
- `feat(ledger)`: switch epoch transition dispatch to EraRulesImpl
- `feat(serialization)`: implement BBODY block body size check from raw CBOR (#377)
- `feat(capture)`: aggregate-mode DRep snapshot — 1 Koios call instead of 8800
- `feat(scripts)`: add N2C compat regression suite harness (#409)

### Bug Fixes (node / ChainSel)
- `fix(node)`: key N2N connections by ConnectionId tuple to enable duplex diffusion
- `fix(node)`: align fork-switch semantics with Haskell ChainSel (#439)
- `fix(node)`: auto-recover from stale-peer rollbacks without operator restart
- `fix(node)`: use hash-based BlockFetch and pending-header filters for fork handling
- `fix(node)`: propagate forged blocks correctly and stop ledger fork divergence
- `fix(node)`: handle TriggeredFork verdict on forge so own blocks propagate
- `fix(node)`: skip ChainDB rollback in TriggeredFork (O(N) cascade stall)
- `fix(node)`: don't regress ledger on peer MsgRollBackward
- `fix(forge)`: align forge-loop with Haskell (drop MAX_FORGE_LAG_SLOTS, NoLedgerView gate)
- `fix(forge)`: prevent doomed-fork forging and TTL-expired tx inclusion

### Bug Fixes (ledger / crypto)
- `fix(crypto,consensus,cli)`: share OCertSignable layout to fix InvalidSignatureOCERT
- `fix(ledger)`: script-DRep keying + committee threading invariant
- `fix(ledger)`: cap DiffSeq at k entries to bound memory during replay (#410)
- `fix(ledger)`: stream UTxO scans and snapshot writes (#403)
- `fix(ledger)`: recompute_snapshot_pool_stakes reads snap.stake_distribution (#423)
- `fix(n2c)`: dispatch tip-result parser by query opcode (#407)
- `fix(mithril)`: fix stale chunk contamination and fresh-import ENOENT

### Bug Fixes (network)
- `fix(network)`: allow co-located peers + filter peer-sharing by IP class
- `fix(network)`: classify UnexpectedEof as clean peer disconnect
- `fix(network)`: prevent self-connection when binding on 0.0.0.0

## Known CI issue at release time
Same pre-existing macOS x86_64 cross-compile issue as v1.1.0-alpha:
`release-binaries (macos-latest, x86_64-apple-darwin)` fails with cross-compilation not supported.
This does NOT block the release — build-and-test, coverage, integration-offline all pass.

## Release process notes
- Used `cargo update --workspace` (NOT `cargo generate-lockfile`) to update lockfile — `generate-lockfile` pulled in a newer `mithril-aggregator-discovery` version incompatible with `mithril-client 0.13.2`, causing build failures. `--workspace` updates only workspace package versions.
- All pre-release checks passed locally: fmt, clippy, nextest (4181 tests), doc tests, release build, binary smoke test.
