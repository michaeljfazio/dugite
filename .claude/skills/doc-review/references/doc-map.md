# Doc Map — Cross-Check Targets by Page

For each doc page, the source files most likely to reveal stale or wrong content.

---

## Getting Started

### `docs/src/introduction.md`
Cross-check: `README.md`, `Cargo.toml` (workspace members for crate list), `crates/dugite-node/src/` (feature list accuracy)
Watch for: feature list completeness, port numbers, crate counts, removed deps (pallas/aiken)

### `docs/src/installation.md`
Cross-check: `Cargo.toml` (rust edition/toolchain), `flake.nix` (nix install), `justfile` (just commands), `README.md`
Watch for: Rust version requirements, install command accuracy, `just` recipe names

### `docs/src/quickstart.md`
Cross-check: `justfile` (recipe names), `scripts/run/`, `scripts/mithril/`
Watch for: `just run-relay preview` vs actual recipe name, mithril import command

### `docs/src/development.md`
Cross-check: `CONTRIBUTING.md`, `justfile`, `CLAUDE.md`
Watch for: test commands (`cargo nextest` vs `cargo test`), clippy flags, fmt commands

---

## Running a Node

### `docs/src/running/configuration.md`
Cross-check: `crates/dugite-node/src/config.rs` (or similar config struct), `config/preview/config.json`, `config/preprod/config.json`
Watch for: field names, defaults, valid values, new fields not documented, removed fields still listed

### `docs/src/running/config-editor.md`
Cross-check: `crates/dugite-config/src/` (subcommands, flags)
Watch for: subcommand names (`init`, `edit`, `validate`, `get`, `set`), key bindings accuracy

### `docs/src/running/topology.md`
Cross-check: `config/preview/topology.json`, `config/preprod/topology.json`, `crates/dugite-node/src/`
Watch for: topology format fields, P2P vs non-P2P topology differences

### `docs/src/running/networks.md`
Cross-check: `config/` (mainnet/preview/preprod dirs), `CLAUDE.md` (network magic numbers)
Watch for: network magic numbers (Mainnet=764824073, Preview=2, Preprod=1), PV11 note for preview

### `docs/src/running/mithril.md`
Cross-check: `scripts/mithril/`, `justfile` (mithril-import recipe), `crates/dugite-node/src/`
Watch for: Mithril snapshot command syntax, download performance notes

### `docs/src/running/logging.md`
Cross-check: `crates/dugite-node/src/` (logging setup), `CLAUDE.md` (log level flags)
Watch for: `--log-level` flag name, `RUST_LOG` env var, log output targets (stdout/file/journald)

### `docs/src/running/monitoring.md`
Cross-check: `crates/dugite-monitor/src/`, `config/monitoring/`, metrics port
Watch for: **metrics port must be 12798** (restored in 9921bc577), Prometheus endpoint path, Grafana dashboard notes

### `docs/src/running/relay.md`
Cross-check: `justfile` (`run-relay` recipe), `config/preview/`, `scripts/run/`
Watch for: relay startup command accuracy, DiffusionMode setting

### `docs/src/running/block-producer.md`
Cross-check: `justfile` (`run-bp` recipe), `config/bp-pair/`, `scripts/run/`, `CLAUDE.md` (KES notes)
Watch for: BP startup flags (`--shelley-kes-key`, `--shelley-vrf-key`, `--shelley-operational-certificate`), DiffusionMode=InitiatorOnly

### `docs/src/running/local-testnet.md`
Cross-check: `scripts/dev/`, `deploy/`, `justfile` (devnet recipes)
Watch for: local devnet setup commands, config file locations

### `docs/src/running/kubernetes.md`
Cross-check: `charts/` (Helm chart), `deploy/`
Watch for: Helm chart values, image names, resource limits

---

## CLI Reference

### `docs/src/cli/overview.md`
Cross-check: `crates/dugite-cli/src/main.rs` or `crates/dugite-cli/src/cli.rs` (subcommand list)
Watch for: subcommand count (docs say "38+"), missing subcommands

### `docs/src/cli/dugite-node.md`
Cross-check: `crates/dugite-node/src/main.rs` or CLI parsing (clap definitions)
Watch for: flag names, defaults, `--consensus-mode genesis` flag (noted as needed for local testnet)

### `docs/src/cli/key-generation.md`
Cross-check: `crates/dugite-cli/src/` (key-gen subcommands)
Watch for: subcommand names, output file names

### `docs/src/cli/transactions.md`
Cross-check: `crates/dugite-cli/src/` (tx subcommands)
Watch for: flag names, transaction building commands

### `docs/src/cli/queries.md`
Cross-check: `crates/dugite-cli/src/` (query subcommands), N2C protocol implementation
Watch for: query subcommand names, socket path flag

### `docs/src/cli/stake-address.md`
Cross-check: `crates/dugite-cli/src/`
Watch for: subcommand names, derivation path flags

### `docs/src/cli/stake-pool.md`
Cross-check: `crates/dugite-cli/src/`
Watch for: pool registration subcommand names, flags

### `docs/src/cli/node-commands.md`
Cross-check: `crates/dugite-cli/src/`
Watch for: `node key-gen-KES`, `node key-gen-VRF`, `node issue-op-cert` accuracy

### `docs/src/cli/governance.md`
Cross-check: `crates/dugite-cli/src/` (governance subcommands), CLAUDE.md (Conway governance notes)
Watch for: DRep commands, governance action commands

---

## Architecture

### `docs/src/architecture/overview.md`
Cross-check: `Cargo.toml` (actual workspace member list), `CLAUDE.md` (architecture section)
Watch for: **crate count** (14 vs 15), crate names, dependency graph accuracy, `dugite-uplc` presence

### `docs/src/architecture/sync-pipeline.md`
Cross-check: `crates/dugite-node/src/sync.rs` (or similar), `CLAUDE.md` (pipeline depth = 300)
Watch for: pipeline depth (default 300, env var `DUGITE_PIPELINE_DEPTH`), sync stages

### `docs/src/architecture/storage.md`
Cross-check: `crates/dugite-storage/src/`, CLAUDE.md (ChainDB, ImmutableDB, VolatileDB)
Watch for: chunk file format, flush-to-immutable logic description, volatile→immutable threshold

### `docs/src/architecture/ledger.md`
Cross-check: `crates/dugite-ledger/src/`, CLAUDE.md (ledger notes)
Watch for: UTxO-HD description, invalid tx handling (`is_valid: false`), reward model, governance

### `docs/src/architecture/consensus.md`
Cross-check: `crates/dugite-consensus/src/`, CLAUDE.md (VRF, KES, epoch transitions)
Watch for: VRF leader check description, epoch nonce, KES period tracking

### `docs/src/architecture/networking.md`
Cross-check: `crates/dugite-network/src/`, CLAUDE.md (mini-protocols list)
Watch for: mini-protocol list (ChainSync, BlockFetch, TxSubmission2, KeepAlive, PeerSharing), N2C protocols

### `docs/src/architecture/p2p-governor.md`
Cross-check: `crates/dugite-network/src/` (peer manager), CLAUDE.md (cold/warm/hot lifecycle)
Watch for: peer lifecycle states, ledger-based discovery description, inbound rate limiting

### `docs/src/architecture/genesis-support.md`
Cross-check: `crates/dugite-network/src/` (CSJ implementation), CLAUDE.md (CSJ Phase A)
Watch for: CSJ description, Ouroboros Genesis Phase A accuracy

---

## Reference

### `docs/src/reference/protocol-parameters.md`
Cross-check: `crates/dugite-primitives/src/` (PParams types), `config/preview/shelley-genesis.json`
Watch for: parameter names, Conway PParams fields

### `docs/src/reference/mini-protocols.md`
Cross-check: `crates/dugite-network/src/` (protocol version numbers, message types)
Watch for: protocol version numbers, message type names

### `docs/src/reference/upgrading.md`
Cross-check: `CHANGELOG.md` or git tags, current version in `Cargo.toml`
Watch for: version numbers, breaking changes, migration steps

### `docs/src/reference/benchmarks.md`
Cross-check: `benches/`, `reports/`
Watch for: benchmark figures accuracy, environment descriptions

### `docs/src/reference/third-party-licenses.md`
Cross-check: `Cargo.lock` (actual deps), `Cargo.toml`
Watch for: **must NOT list pallas or aiken**, must include `dugite-uplc` deps (blst, k256, etc.)

### `docs/src/reference/troubleshooting.md`
Cross-check: known issues from CLAUDE.md memory context, common error messages in code
Watch for: solutions that reference removed features, port numbers (12798)
