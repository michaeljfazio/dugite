# Architecture Overview

Dugite is organized as a 14-crate Cargo workspace. Each crate has a focused responsibility and well-defined dependencies.

## Crate Workspace

| Crate | Description |
|-------|-------------|
| `dugite-primitives` | Core types: hashes, blocks, transactions, addresses, values, protocol parameters (Byron through Conway) |
| `dugite-crypto` | Ed25519 keys, VRF, KES, text envelope format |
| `dugite-serialization` | In-house multi-era CBOR encoding/decoding for Cardano wire format |
| `dugite-lsm` | Pure Rust LSM-tree engine with WAL, compaction, bloom filters, and snapshots |
| `dugite-network` | Ouroboros mini-protocols (ChainSync, BlockFetch, TxSubmission, KeepAlive), N2N client/server, N2C server, multi-peer block fetch pool |
| `dugite-consensus` | Ouroboros Praos, chain selection, epoch transitions, slot leader checks |
| `dugite-ledger` | UTxO set (LSM-backed via UTxO-HD), transaction validation, ledger state, certificate processing, native script evaluation, reward calculation |
| `dugite-mempool` | Thread-safe transaction mempool with input-conflict checking and TTL sweep |
| `dugite-storage` | ChainDB (ImmutableDB append-only chunk files + VolatileDB in-memory) |
| `dugite-node` | Main binary, config, topology, pipelined chain sync loop, Mithril import, block forging |
| `dugite-cli` | cardano-cli compatible CLI (38+ subcommands) |
| `dugite-monitor` | Terminal monitoring dashboard (ratatui-based, real-time metrics via Prometheus polling) |
| `dugite-config` | Interactive TUI configuration editor with tree navigation, inline editing, type validation, and diff view |
| `dugite-uplc` | In-house UPLC CEK machine for Plutus V1/V2/V3 script evaluation |
| `dugite-integration-tests` | End-to-end integration tests across the workspace |

## Crate Dependency Graph

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
    LEDGER --> UPLC[dugite-uplc]
    STORE --> PRIM
    STORE --> SER
    POOL --> PRIM
    SER --> PRIM
    CRYPTO --> PRIM
```

## Key Dependencies

- **tokio** — Async runtime
- **dugite-lsm** — Pure Rust LSM tree for the on-disk UTxO set (UTxO-HD)
- **minicbor** — CBOR encoding for custom types
- **ed25519-dalek** — Ed25519 signatures
- **blake2b_simd** — SIMD-accelerated Blake2b hashing
- **clap** — CLI argument parsing
- **tracing** — Structured logging

## Design Principles

### Zero-Warning Policy

All code must compile with `RUSTFLAGS="-D warnings"` and pass `cargo clippy --all-targets -- -D warnings`. This is enforced by CI.

### Wire-Format Compatibility

Dugite uses an in-house multi-era CBOR decoder (`dugite-serialization`) for all block and transaction deserialization, ensuring exact wire-format compatibility with cardano-node. Internal types (`dugite-primitives`) are populated directly from the decoded CBOR.

Key patterns:
- `Transaction.hash` is `blake2b_256(raw_body_cbor)` over bytes captured by `KeepRaw::parse_with` during decode
- `ChainSyncEvent::RollForward` uses `Box<Block>` to avoid large enum variant size
- Invalid transactions (`is_valid: false`) are skipped during `apply_block`
- Pool IDs are `Hash28` (Blake2b-224), not `Hash32`

### Multi-Era Support

Dugite handles all Cardano eras from Byron through Conway. The serialization layer handles era-specific block formats transparently, while the ledger layer applies era-appropriate validation rules.
