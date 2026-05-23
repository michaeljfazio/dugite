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

14-crate Cargo workspace under `crates/`. Dependency flow:

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
v1.7.0 released. Current work: Byron-era block header decode hardening (issue #613) and epoch-diff cross-validation against cardano-node 11.0.1 on preview/preprod. UPLC CEK machine conformance is complete (all tests passing). Soak testing via Sandstone Pool [SAND] on both preview and preprod (pool IDs: preview `6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856`, preprod `pool1uju7fuqzv...nh0ch`). Preview testnet is at PV11 — requires connecting to peers running cardano-node 11.0.1+.

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
