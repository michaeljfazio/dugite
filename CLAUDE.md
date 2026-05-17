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

```bash
# Build everything
cargo build --all-targets

# Run all tests (nextest — parallel, matches CI)
cargo nextest run --workspace

# Run tests for a single crate
cargo nextest run -p dugite-ledger

# Run a single test by name
cargo nextest run -p dugite-ledger -E 'test(test_name)'

# Run doc tests (nextest doesn't support these yet)
cargo test --doc

# Lint
cargo clippy --all-targets -- -D warnings

# Format check (fix with: cargo fmt --all)
cargo fmt --all -- --check

# Build release binary
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

dugite-serialization (CBOR encode/decode via pallas)
dugite-crypto (Ed25519, VRF, KES, text envelope)
dugite-primitives (core types: hashes, blocks, txs, addresses, values, protocol params, all eras)
```

### Key Traits & Abstractions
- **`BlockProvider`** (storage) — trait used by N2N server for block serving
- **`TxValidator`** (ledger) — trait used by N2C server for Phase-1/Phase-2 tx validation before mempool admission
- **`ChainDB`** — wraps ImmutableDB (append-only chunk files) + VolatileDB (HashMap), handles rollback and volatile→immutable flush

### Wire Format
- All Cardano wire-format compatibility via pallas crates (v1.0.0-alpha.5)
- `Transaction.hash` field is set during deserialization from `pallas tx.hash()`
- CBOR encoding for N2C protocol params uses integer keys 0-33 (not JSON strings)

## Key Patterns
- `ChainSyncEvent::RollForward` uses `Box<Block>` to avoid large enum variant size
- Invalid transactions (`is_valid: false`): collateral consumed, collateral_return added, regular inputs/outputs skipped
- Batch block storage: `add_blocks_batch()` for efficient batch writes to ImmutableDB
- ChainDB write happens BEFORE ledger apply to prevent divergence on failure
- Epoch transitions use mark/set/go snapshot model with reward distribution from "go" snapshot
- Governance ratification: DRep/SPO/CC voting thresholds vary by action type (CIP-1694)
- Pipelined ChainSync bypasses pallas serial state machine; default pipeline depth 300 (configurable via `DUGITE_PIPELINE_DEPTH`)
- Ledger-based peer discovery: extracts SPO relay addresses from `pool_params` when past `useLedgerAfterSlot`
- Pallas 1.0: `DatumOption` (was `PseudoDatumOption`), `Option<T>` (was `Nullable<T>`)
- Pallas 28-byte hash types (DRep keys, pool voter keys, required signers) must be padded to 32 bytes — do not use `Hash<32>::from()` directly on 28-byte hashes

## Current Focus
Soak testing on preview testnet (Sandstone Pool [SAND], pool ID 6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856). Automated restart cycles, transaction submission via scripts/soak-test.sh, Koios cross-validation. Stability and block production verification.

## Running the Node

```bash
# Fast sync with Mithril snapshot (preview testnet, magic=2)
./target/release/dugite-node mithril-import \
  --network-magic 2 --database-path ./db-preview

# Run the node
./target/release/dugite-node run \
  --config config/preview-config.json \
  --topology config/preview-topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 --port 3001
```

Network magic: Mainnet=764824073, Preview=2, Preprod=1
