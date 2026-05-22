<p align="center">
  <img src="assets/logo.png" alt="Dugite logo" width="220" />
</p>

<h1 align="center">Dugite</h1>

Named after the [dugite](https://en.wikipedia.org/wiki/Dugite) (*Pseudonaja affinis*), a highly venomous brown snake indigenous to Perth, Western Australia — where the project's main author is from. Dugites are fast, resilient, and quietly formidable; traits shared by this node. They're also responsible for the loss of more than a few beloved family pets over the years.

A Cardano node implementation written in Rust, aiming for 100% compatibility with [cardano-node](https://github.com/IntersectMBO/cardano-node).

Built by [Sandstone Pool](https://www.sandstone.io/)

[Documentation](https://michaeljfazio.github.io/dugite/) | [Benchmarks](https://michaeljfazio.github.io/dugite/reference/benchmarks.html) | [Developer Wiki](https://github.com/michaeljfazio/dugite/wiki) | [Discussions](https://github.com/michaeljfazio/dugite/discussions)

[![CI](https://github.com/michaeljfazio/dugite/actions/workflows/ci.yml/badge.svg)](https://github.com/michaeljfazio/dugite/actions/workflows/ci.yml)
[![Code Scanning](https://github.com/michaeljfazio/dugite/actions/workflows/code-scanning.yml/badge.svg)](https://github.com/michaeljfazio/dugite/actions/workflows/code-scanning.yml)
[![codecov](https://codecov.io/gh/michaeljfazio/dugite/graph/badge.svg)](https://codecov.io/gh/michaeljfazio/dugite)
[![Nightly Benchmarks](https://github.com/michaeljfazio/dugite/actions/workflows/benchmarks.yml/badge.svg)](https://github.com/michaeljfazio/dugite/actions/workflows/benchmarks.yml)
[![Docs](https://github.com/michaeljfazio/dugite/actions/workflows/docs.yml/badge.svg)](https://michaeljfazio.github.io/dugite/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Cardano](https://img.shields.io/badge/cardano-conway-blueviolet.svg)](https://cardano.org/)
[![GitHub Discussions](https://img.shields.io/github/discussions/michaeljfazio/dugite)](https://github.com/michaeljfazio/dugite/discussions)
[![GitHub Stars](https://img.shields.io/github/stars/michaeljfazio/dugite)](https://github.com/michaeljfazio/dugite/stargazers)

> [!CAUTION]
> **Dugite is in early development and is NOT recommended for production use.**
> APIs, storage formats, and on-chain behavior may change without notice. Ledger validation is incomplete and may accept invalid transactions or reject valid ones. **Do not use this software to operate a stake pool, manage real funds, or participate in mainnet governance.** Use at your own risk on testnets only.

For project status, capability matrix, and known issues see the [Developer Wiki](https://github.com/michaeljfazio/dugite/wiki).

## Quick Start

Dugite ships a top-level [`justfile`](./justfile) — install [just](https://github.com/casey/just) and the most common workflows become one-liners. `just --list` shows everything.

```bash
# Build, lint, test (full CI gate)
just check

# Fast sync with a Mithril snapshot (recommended), then run as a relay on preview.
just mithril-import preview
just run-relay preview
```

Without `just`, the same steps map directly to the underlying scripts and `cargo` commands:

```bash
cargo build --release
./scripts/mithril/import.sh preview
./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

For installation, configuration, networks, monitoring, block-producer setup, and the full CLI reference, see the [documentation](https://michaeljfazio.github.io/dugite/).

| Network | Magic |
|---------|-------|
| Mainnet | `764824073` |
| Preview | `2` |
| Preprod | `1` |

## Architecture

Dugite is a 14-crate Cargo workspace built on [pallas](https://github.com/txpipe/pallas) 1.0.0-alpha.5 for Cardano wire-format compatibility. Three binaries: `dugite-node` (the node), `dugite-cli` (cardano-cli compatible), `dugite-monitor` and `dugite-config` (TUIs).

```mermaid
graph TD
    NODE[dugite-node] --> NET[dugite-network]
    NODE --> CONS[dugite-consensus]
    NODE --> LEDGER[dugite-ledger]
    NODE --> STORE[dugite-storage]
    NODE --> POOL[dugite-mempool]
    CLI[dugite-cli] --> NET
    CLI --> PRIM[dugite-primitives]
    CLI --> CRYPTO[dugite-crypto]
    CLI --> SER[dugite-serialization]
    MON[dugite-monitor] --> PRIM
    CFG[dugite-config] --> PRIM
    NET --> PRIM
    NET --> CRYPTO
    NET --> SER
    NET --> POOL
    CONS --> PRIM
    CONS --> CRYPTO
    LEDGER --> PRIM
    LEDGER --> CRYPTO
    LEDGER --> SER
    LEDGER --> LSM[dugite-lsm]
    STORE --> PRIM
    STORE --> SER
    POOL --> PRIM
    SER --> PRIM
    CRYPTO --> PRIM
```

See [Architecture Overview](https://michaeljfazio.github.io/dugite/architecture/overview.html) for the per-crate breakdown, and [Architecture Decision Records](https://github.com/michaeljfazio/dugite/wiki/Architecture-Decision-Records) for design rationale.

## Development

```bash
# Run all tests (parallel via nextest, matches CI)
cargo nextest run --workspace

# Doc tests
cargo test --doc

# Lint
cargo clippy --all-targets -- -D warnings

# Format check
cargo fmt --all -- --check
```

Zero-warning policy is enforced: all code must compile cleanly with clippy and pass formatting checks. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow, and [Getting Started for Developers](https://github.com/michaeljfazio/dugite/wiki/Getting-Started-for-Developers) on the wiki.

Benchmark instructions and tracked baselines: [Benchmarks](https://michaeljfazio.github.io/dugite/reference/benchmarks.html).

## Acknowledgments

Special thanks to the following individuals for their contributions and support:

- **Andrew Westberg** (BCSH)
- **Samuel Leathers**
- **Homer J** (AAA)

## License

Apache-2.0
