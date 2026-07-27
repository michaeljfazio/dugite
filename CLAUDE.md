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
v2.2.2 released (2026-07-28). Five networking/consensus fixes ending the
preprod sync churn — the "keep connecting and disconnecting" behaviour visible
during catch-up. All five compound; none is a ledger divergence.

- **#908 (client)** — the flap loop had **no backoff**. `cleanup_dead_connections`
  called only `peer_disconnected()`, which drops a peer to Cold with
  `next_connect_after` unset, so `eligible_cold` re-offered it on the next
  governor tick (10 full cycles with one peer in 90 s; 45 in one 19 h log).
  Everything the GC reaps died *unexpectedly* — every planned teardown removes
  its connections from the map synchronously first — so the reap now applies
  `peer_failed()`. Repeat reports for one teardown (protocol task **and** GC)
  collapse inside a 2 s window: Haskell backs off per connection *attempt*.
  Also, `MsgIntersectNotFound` for every offered point now classifies as
  `PeerUnsuitable`/ForkTooDeep directly instead of "syncing from Origin".
- **#908 (server)** — **dugite's ChainSync server answered
  `MsgIntersectNotFound` to every point deeper than its own immutable tip.**
  `handle_find_intersect` validated points with `find_chain_ancestor`, which is
  a *rewind* helper: volatile selected chain + the immutable **tip** only. That
  is exactly the anchor set dugite's own client offers
  (`get_immutable_historical_points`), so a peer could only intersect on our
  live tip block. New `BlockProvider::canonical_point_slot` resolves the whole
  canonical chain. See [[reference_find_chain_ancestor_is_not_an_intersection_lookup]].
- **#909** — bulk-sync hot demotion evicted the **active BlockFetch downloader**.
  `peer_score` weights keepalive RTT 40%, and the downloader's pings queue
  behind its own 2048-block payload stream, so the busiest peer ranked worst.
  Haskell has no identity exclusion — `simpleChurnModePeerSelectionPolicy`
  sorts by `fetchynessBytes` ascending and the metric *is* the protection.
  Added a rolling fetched-bytes window; `hot_demotion_rank` uses it in bulk
  sync, `peer_score` at tip. The churn-rotation path also gained the
  fetch-slot exclusion it never had.
- **#910** — the pipeline **could not drain**, so `MsgDone` was never sent and
  a reused mux carried prior-session residue (`317 stale next-phase responses
  … (bound 316)`, 45x). Root cause: dugite blasted 300 `MsgRequestNext`
  unconditionally and refilled to 300 whenever `!at_tip`, parking 200-300
  requests answerable only as blocks are minted. `pipeline_target_depth` now
  bounds depth by the known block gap (Haskell `pipelineDecisionLowHighMark`):
  1 at tip, unchanged at 300 during bulk sync. Cancel drains to zero then
  sends `MsgDone`; the mux is reused only if that succeeded, else TCP close.
  Residue tolerance 316 → 8.
- **#911** — eager OCERT upper bound is now **advisory**. In the per-peer eager
  view `m` is a single peer's reconstruction on a startup-frozen baseline that
  is reset on `MsgRollBackward`, so a canonical header reads as an
  over-increment (`got=474 last_seen=472` on a Koios-verified preprod block).
  It already deferred to body apply, but WARNed like a rejection and never
  advanced the counter, so every following header from that pool re-tripped.
  Now: `debug!`, advance the high-water mark, still skip. Lower bound
  (`CounterTooSmallOCERT`) stays fatal.

No SNAPSHOT_VERSION change — v2.2.2 is a drop-in upgrade from v2.2.1.
Pre-v2.1.0 Mithril DBs still need a full `mithril-import`.

Open query-surface work (neither affects consensus): **#905**
(`query stake-distribution` rebuilds pool stake from live delegations and
misses genesis-seeded ones) and **#906** (`GetProposals` drops govAction
payloads and misorders results).

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
