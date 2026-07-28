# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Goal
Implement a 100% compatible Cardano node in Rust. Target full compatibility with cardano-node (Haskell).

## Development Methodology: Ralph Loop
Follow the Ralph autonomous development loop:
1. **Assess** — Evaluate current state, identify highest-impact gaps
2. **Implement** — Build the next feature/fix
3. **Test** — Run `cargo test --all`, ensure zero failures
4. **Verify** — Run `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`
5. **Commit** — Commit and push to remote with descriptive message
6. **Repeat** — Continue to the next iteration

## Build & Test Commands

The top-level `justfile` wraps the common dev commands. Pick whichever feels more natural — both shapes are equivalent.

```bash
# Just recipes (preferred when in a fresh shell)
just check          # full CI gate: fmt-check + clippy + build + test + test-doc
just build
just test           # cargo nextest run --workspace
just test-doc
just clippy
just fmt-check      # cargo fmt --all -- --check  (fix with: just fmt)

# Direct cargo (still works for narrow invocations)
cargo build --all-targets
cargo nextest run --workspace
cargo nextest run -p dugite-ledger                    # single crate
cargo nextest run -p dugite-ledger -E 'test(name)'    # single test
cargo test --doc
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --release
```

The storage layer is pure Rust with no system dependencies. cardano-lsm (used for the on-disk UTxO set) supports `--features io-uring` for async I/O on Linux.

## Hard Requirements
- **Zero warnings** — All code must compile with `RUSTFLAGS="-D warnings"`
- **Clippy clean** — `cargo clippy --all-targets -- -D warnings` must pass
- **Formatted** — `cargo fmt --all -- --check` must pass
- **Tests pass** — All tests must pass before committing
- **CI green** — GitHub Actions pipeline must be passing
- **Commit regularly** — Push changes to remote after each successful iteration
- **Focused commits** — Stage explicit filenames (no `git add -A` / `git commit -a`). The pre-commit hook warns when staged paths span more than two crates; set `DUGITE_PRECOMMIT_STRICT=1` to make that fatal (recommended for autonomous agent runs).

## Architecture

15-crate Cargo workspace under `crates/`. Dependency flow:

```
dugite-node (binary: main node, config, pipelined sync, Mithril import, block forging)
├── dugite-network (Ouroboros mini-protocols, N2N/N2C multiplexer, pipelined client)
├── dugite-consensus (Ouroboros Praos, chain selection, epoch transitions, VRF leader check)
├── dugite-ledger (UTxO set via UTxO-HD, tx validation, ledger state, certificates, rewards, governance)
├── dugite-storage (ChainDB = ImmutableDB append-only chunk files + VolatileDB in-memory)
└── dugite-mempool (thread-safe tx mempool with input-conflict checking and TTL sweep)

dugite-cli (binary: cardano-cli compatible, 38+ subcommands)
dugite-monitor (binary: terminal monitoring dashboard, ratatui-based, real-time metrics)
dugite-config (binary: interactive TUI configuration editor with tree navigation, inline editing, diff view)

dugite-serialization (CBOR encode/decode — in-house multi-era decoder + minicbor)
dugite-crypto (Ed25519, VRF, KES, text envelope)
dugite-primitives (core types: hashes, blocks, txs, addresses, values, protocol params, all eras)
dugite-uplc (in-house UPLC CEK machine; 100% conformant as of v1.7.0)
dugite-lsm (LSM-tree on-disk storage for UTxO-HD)
```

### Key Traits & Abstractions
- **`BlockProvider`** (storage) — trait used by N2N server for block serving
- **`TxValidator`** (ledger) — trait used by N2C server for Phase-1/Phase-2 tx validation before mempool admission
- **`ChainDB`** — wraps ImmutableDB (append-only chunk files) + VolatileDB (HashMap), handles rollback and volatile→immutable flush

### Wire Format
- All Cardano wire-format compatibility via the in-house multi-era CBOR decoder under `crates/dugite-serialization/src/decode/`
- `Transaction.hash` is `blake2b_256(raw_body_cbor)` over the bytes captured by `KeepRaw::parse_with` during decode
- CBOR encoding for N2C protocol params uses integer keys 0-33 (not JSON strings)

## Key Patterns
- `ChainSyncEvent::RollForward` uses `Box<Block>` to avoid large enum variant size
- Invalid transactions (`is_valid: false`): collateral consumed, collateral_return added, regular inputs/outputs skipped
- Batch block storage: `add_blocks_batch()` for efficient batch writes to ImmutableDB
- ChainDB write happens BEFORE ledger apply to prevent divergence on failure
- Epoch transitions use mark/set/go snapshot model with reward distribution from "go" snapshot
- Governance ratification: DRep/SPO/CC voting thresholds vary by action type (CIP-1694)
- Pipelined ChainSync runs an in-house state machine for maximum throughput; default pipeline depth 300 (configurable via `DUGITE_PIPELINE_DEPTH`)
- Ledger-based peer discovery: extracts SPO relay addresses from `pool_params` when past `useLedgerAfterSlot`
- DatumOption / Nullable wrappers: see `crates/dugite-serialization/src/decode/primitives.rs` for the in-house equivalents (`Nullable`, `MaybeIndef`, `KeyValuePairs`)
- 28-byte hash types (DRep keys, pool voter keys, required signers) must be padded to 32 bytes via `Hash28::to_hash32_padded()` — do not use `Hash<32>::from()` directly on 28-byte hashes

## Current Focus
Backlog sweep after v2.2.4 (2026-07-29). Nine open issues triaged and fixed;
two are byte-exact ledger/LSQ divergences, the rest are correctness hygiene
and harness defects that made suites report success while testing nothing.

- **#919 (ledger, SNAPSHOT 30 -> 31)** — dugite had exactly ONE min-UTxO
  helper, the Babbage `(160+size) x coinsPerUTxOByte` formula, applied in
  every era, because `ada_per_utxo_byte` is seeded from the Alonzo genesis at
  startup regardless of the chain's era. Mainnet Shelley txs with 1 ADA
  outputs were rejected at `minimum=1051640` (= 4310 x 244). Haskell defines
  `getMinCoinTxOut` per era and can never apply a Babbage calc to a Shelley
  TxOut. Now PV-dispatched: PV<=3 flat `minUTxOValue`; PV4 Mary
  `scaledMinDeposit` (ada-only short-circuits BEFORE `size`); PV5-6 Alonzo
  `(27 + size + dataHashSize) x coinsPerUTxOWord`; PV>=7 unchanged. The
  shared `Value::mary_value_size()` returns **2** for ada-only — deliberately
  "wrong for Mary, right for Alonzo" upstream, since Mary never reaches it.
  Also fixed: PPU key 15 was decoded then dropped, and key 17 is
  coinsPerUTxOWord pre-Babbage but coinsPerUTxOByte after (disambiguated by
  the PV in force before that update's own PV bump).
- **#922 (LSQ)** — `GetProposals` served the LIVE proposal set. Haskell's
  `queryProposals` never reads `cgsProposals`; it reads the DRep pulser's
  frozen `dpProposals`/`psProposals`, refreshed once per epoch boundary by
  `setFreshDRepPulsingState`, so mid-epoch submissions are invisible until
  the next boundary. Now answers from dugite's #903 ratification snapshot
  (the same `dpProposals` equivalent) — one mechanism, two bugs.
  `GetGovState`'s embedded `cgsProposals` correctly stays live.
- **#920 (network)** — the v2.2.4 trusted-only clamp gated PROMOTION only, so
  peers established during a CaughtUp period that later regressed stayed
  established and the HAA closure could still fail. Now self-healing: the
  governor demotes untrusted established outbound peers straight to Cold
  every tick the clamp holds (no cooldown, no fetch-slot exclusion — a
  planned policy teardown, not a failure), plus a one-shot sweep on the
  regression edge and a register-time gate closing the mid-handshake race.
- **#914 (ledger)** — the GOV apply path silently dropped proposals with an
  invalid `prev_action_id` under a comment claiming Haskell does the same.
  Canonical `Conway.Rules.Gov` does the opposite (`failBecause`). Now hard
  errors: reaching it on ApplyOnly means governance state already diverged
  (the #898 shape), so crash rather than corrupt pots silently.
- **#915 (network)** — `InvalidPrevGovActionId` rejections now encode as
  canonical `ConwayGovFailure` (Ledger tag 3) / GOV tag 8 carrying the full
  `ProposalProcedure`, instead of a generic reason.
- **Harness defects (#916/#917/#918/#921/#923)** — the recurring shape is a
  check that reports success while measuring nothing. The release report
  counted the substring "error" (so `error=` fields on INFO lines showed
  thousands of errors on a clean run); the forge-stall predicate was a ~3%
  per-sample coin flip on a single-forger devnet; three tx-zoo scripts
  skipped structurally on every run; `adv_send_expect_close` returned PASS
  when socat was missing, so every adversarial N2N case in protocols/01-07
  "passed" without sending a byte. Level counting is now shared by generator
  and analyzer with an agreement test; forge-stall accumulates a
  Praos-derived p99.9 gap budget; tx-zoo vendors a stdlib raw-socket writer
  and a CBOR splicer and classifies env-vs-state skips (`--strict-skips`);
  nextest has a `slow-timeout` terminate-after backstop.

**#919 bumps SNAPSHOT_VERSION 30 -> 31** — existing DBs replay chunks on
first restart. Pre-v2.1.0 Mithril DBs still need a full `mithril-import`.

Open query-surface work (does not affect consensus): **#905**
(`query stake-distribution` rebuilds pool stake from live delegations and
misses genesis-seeded ones). #906 (GetProposals payload/order) was already
fixed in `fc892e1759` before v2.2.1 — verified during the #922 work.

Soak testing via Sandstone Pool [SAND] on preview and preprod (pool IDs:
preview `6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856`, preprod
`pool1uju7fuqzv...nh0ch`). Preview is at PV11 — requires peers running
cardano-node 11.0.1+.

### Reading the cli-parity suite

`tx-zoo/09-cli-parity` runs `cardano-cli` against **both** sockets and diffs the
answers — it never invokes `dugite-cli`. What it measures is dugite-**node**'s
LSQ responses. A failure on both sides is a harness bug, never a dugite gap
(this misreading produced four phantom "dugite-cli gaps" in #900). ERROR rows
fail the round, every divergence writes `evidence/<ts>/cli-parity-diffs/`, and
the tip is pinned across both sockets so a block applied mid-comparison cannot
manufacture a false divergence.

## Running the Node

Config files live under per-network subdirectories (`config/{mainnet,preview,preprod}/{config,topology,*-genesis}.json`). The justfile wraps the common launchers; underlying scripts live in `scripts/run/`.

```bash
# Justfile (preferred)
just mithril-import preview
just run-relay preview          # or: just run-bp preview

# Equivalent direct invocation
./target/release/dugite-node mithril-import --network-magic 2 --database-path ./db-preview
./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 --port 3001
```

Network magic: Mainnet=764824073, Preview=2, Preprod=1

## Scripts & configs at a glance

- `config/{mainnet,preview,preprod}/` — per-network configs and genesis files (self-contained, relative paths).
- `config/bp-pair/` — Sandstone preview BP-pair soak rig (dugite-bp + dugite-relay + haskell-relay).
- `config/monitoring/` — Grafana dashboard, Prometheus scrape + alert rules.
- `scripts/run/`, `scripts/soak/`, `scripts/monitoring/`, `scripts/validation/`, `scripts/mithril/`, `scripts/dev/` — see `just --list` for the entry points.

## Upstream Conformance Testing

Dugite maintains byte-exact alignment with upstream Cardano implementations
via a republished corpus. Every upstream artefact flows through a single
pipeline (`scripts/regenerate-conformance-corpus/`) and is published as a
dugite GitHub release pinned in `tests/conformance/upstream/manifest.toml`.

### Daily workflow

```bash
# Download all upstream fixture areas (reads manifest.toml for the release tag)
just download-upstream-fixtures

# Run the full UPLC + upstream golden test suite
just test-conformance

# Run a single area
cargo xtask download-upstream-fixtures --area ledger-rules
DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance \
  --features upstream-conformance --test upstream_tests
```

### Seven fixture areas

| Area | Source | Content |
|------|--------|---------|
| `ouroboros-consensus` | IntersectMBO/ouroboros-consensus | Block/header golden files per era |
| `cardano-ledger` | IntersectMBO/cardano-ledger | Genesis JSON, CDDL schema, golden txs |
| `cardano-node` | IntersectMBO/cardano-node | Genesis spec files |
| `plutus` | IntersectMBO/plutus | 999 UPLC evaluation test cases |
| `ledger-rules` | ImpSpec dump of cardano-ledger | CBOR ImpSpec vectors (NEWEPOCH + LEDGER) |
| `cardano-base` | IntersectMBO/cardano-base | VRF v03 crypto test vectors |
| `mithril` | input-output-hk/mithril | Certificate fixture JSON |

### Refreshing the corpus

1. Edit `tests/conformance/upstream/sources.toml` to bump a pin.
2. Trigger `.github/workflows/regenerate-conformance-corpus.yml` (manual dispatch or weekly auto).
3. Update `[release].tag` in `tests/conformance/upstream/manifest.toml`.
4. Run `just download-upstream-fixtures && just test-conformance`.
5. Commit `sources.toml` + `manifest.toml` + any code changes.

The `ledger-rules` area builds cardano-ledger from source (GHC 9.6.5 +
cabal 3.10.x, ~35 min cold, ~5 min cached) and runs the official ImpSpec
conformance suite with `CONFORMANCE_CBOR_DUMP_PATH` set to capture every
test vector. Phase 4 acceptance: `SKIP_LIST` in
`tests/conformance/src/upstream/ledger_rules_replay/mod.rs` is empty or
every entry has a tracking issue.

### CI

The `upstream-conformance` job in `.github/workflows/ci.yml` runs both the
UPLC and upstream golden suites with `DUGITE_REQUIRE_UPSTREAM=1`. Fixture
cache is keyed on `manifest.toml` content hash; bumping the tag invalidates
the cache automatically.
