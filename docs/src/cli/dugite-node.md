# dugite-node Reference

`dugite-node` is the main Dugite node binary. The two subcommands used in
day-to-day operation are `run` (start the node) and `mithril-import` (import
a Mithril snapshot for fast initial sync), documented below.

The binary also ships several operator/debug subcommands not covered in
detail here: `db info` (database size and block count), `dump-snapshot`
(replay the chain and dump ledger state at epoch boundaries, for
cross-validation), `verify-ledger-snapshot` (byte-exact comparison of two
ledger snapshots), and `snapshot-convert` (convert a ledger snapshot between
the in-memory and LSM UTxO backends without a chain replay). Run
`dugite-node <subcommand> --help` for their flags.

## run

Start the Dugite node:

```bash
dugite-node run [OPTIONS]
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--config` | `config/mainnet/config.json` | Path to the node configuration file |
| `--topology` | `config/mainnet/topology.json` | Path to the topology file |
| `--database-path` | `db` | Path to the database directory |
| `--socket-path` | `node.sock` | Unix domain socket path for N2C (local client) connections |
| `--port` | `3001` | TCP port for N2N (node-to-node) connections |
| `--host-addr` | `0.0.0.0` | Host address to bind to |
| `--metrics-port` | | Prometheus metrics port. If omitted, the config file's `MetricsPort` is used; if neither is set, defaults to `12798` |
| `--no-metrics` | `false` | Disable the Prometheus metrics server entirely. Equivalent to `--metrics-port 0` |
| `--require-metrics` | `false` | Make a metrics bind failure a fatal startup error (default: node continues if the port can't be bound) |
| `--rpc-host` | | UTxO RPC (gRPC) server bind address. Overrides `Rpc.ListenAddr` from the config file. Defaults to `127.0.0.1` when the server is enabled |
| `--rpc-port` | | UTxO RPC (gRPC) server port. Overrides `Rpc.Port`; setting this implies enabling the RPC server. Defaults to `50051` when set via config |
| `--no-rpc` | `false` | Disable the UTxO RPC (gRPC) server entirely, overriding `--rpc-host`/`--rpc-port`/`Rpc.Enabled` |
| `--compat-metrics` | `false` | Also emit `cardano_node_metrics_*` compatibility aliases alongside the native `dugite_*` metrics, for reuse of existing cardano-node Grafana dashboards |
| `--liveness-threshold-secs` | `600` | Liveness threshold (seconds) for the `/live` HTTP endpoint; `0` disables it (always 200) |
| `--consensus-mode` | | Consensus mode override: `praos` or `genesis` (Ouroboros Genesis with GSM). When omitted, read from the config file's `ConsensusMode` field (default `PraosMode`) |
| `--validate-all-blocks` | `false` | Force full Phase-2 Plutus validation on all blocks, even during initial sync (normally only blocks at tip are fully validated) |
| `--skip-eagerly-validated-header-crypto` | `false` | Skip apply-time header re-validation for headers that already passed eager per-peer validation. Off by default; see the flag's doc comment before enabling in production |
| `--dijkstra-genesis` | | Path to the Dijkstra-era genesis JSON file, overriding the config file's `DijkstraGenesisFile` (parsed but not yet applied to runtime protocol parameters) |
| `--shelley-kes-key` | | Path to the KES signing key (enables block production) |
| `--shelley-vrf-key` | | Path to the VRF signing key (enables block production) |
| `--shelley-operational-certificate` | | Path to the operational certificate (enables block production) |
| `--shelley-cold-key` | | Path to the cold signing key file, used for pool ID derivation |
| `--log-output` | `stdout` | Log output target: `stdout`, `file`, or `journald`. Can be specified multiple times. |
| `--log-format` | `text` | Log format: `text` (human-readable) or `json` (structured). |
| `--log-level` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`). Overridden by `RUST_LOG`. |
| `--log-dir` | `logs` | Directory for log files (used with `--log-output file`) |
| `--log-file-rotation` | `daily` | Log file rotation strategy: `daily`, `hourly`, or `never` |
| `--log-no-color` | `false` | Disable ANSI colors in stdout output |
| `--log-retention-days` | `7` | Number of days to retain log files |
| `--stdout-overflow` | `drop` | Channel-full policy for the non-blocking stdout writer: `drop` (keep going, count dropped lines) or `block` (lossless, but re-introduces blocking on the hot path) |
| `--mempool-max-tx` | `16384` | Maximum number of transactions in the mempool |
| `--mempool-max-bytes` | `536870912` | Maximum mempool size in bytes (default 512 MB) |
| `--snapshot-max-retained` | `2` | Maximum number of ledger snapshots to retain on disk |
| `--snapshot-bulk-min-blocks` | `50000` | Minimum blocks between bulk-sync snapshots |
| `--snapshot-bulk-min-secs` | `360` | Minimum seconds between bulk-sync snapshots |
| `--storage-profile` | `high-memory` | Storage profile: `ultra-memory` (32GB), `high-memory` (16GB), `low-memory` (8GB), or `minimal` (4GB) |
| `--immutable-index-type` | | Override block index type: `in-memory` or `mmap` |
| `--utxo-backend` | | Override UTxO backend: `in-memory` or `lsm` |
| `--utxo-memtable-size-mb` | | Override LSM memtable size in MB |
| `--utxo-block-cache-size-mb` | | Override LSM block cache size in MB |
| `--utxo-bloom-filter-bits` | | Override LSM bloom filter bits per key |

### Relay Node (default)

Run as a relay node with no block production keys:

```bash
dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

### Block Producer

Run as a block producer by providing all three key/certificate paths:

```bash
dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001 \
  --shelley-kes-key ./keys/kes.skey \
  --shelley-vrf-key ./keys/vrf.skey \
  --shelley-operational-certificate ./keys/opcert.cert
```

When all three block producer flags are provided, the node enters block production mode. The cold signing key is not needed at runtime — the cold verification key is extracted from the operational certificate, matching cardano-node behavior.

If any of the three flags is missing, the node runs in relay-only mode.

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DUGITE_PIPELINE_DEPTH` | `300` | ChainSync pipeline depth (number of blocks requested ahead) |
| `RUST_LOG` | `info` | Log level filter (e.g., `debug`, `info`, `warn`, `dugite_node=debug`). Overrides `--log-level`. |

See [Logging](../running/logging.md) for details on output targets, file rotation, and per-crate filtering.

### Configuration File

The `--config` file follows the same JSON format as cardano-node. Key fields:

```json
{
  "Protocol": "Cardano",
  "RequiresNetworkMagic": "RequiresMagic",
  "ByronGenesisFile": "byron-genesis.json",
  "ShelleyGenesisFile": "shelley-genesis.json",
  "AlonzoGenesisFile": "alonzo-genesis.json",
  "ConwayGenesisFile": "conway-genesis.json"
}
```

Genesis file paths are resolved relative to the directory containing the config file.

### Metrics

When `--metrics-port` is non-zero, Prometheus metrics are served at `http://localhost:<port>/metrics`. See [Monitoring](../running/monitoring.md) for the full list of available metrics.

## mithril-import

Import a Mithril snapshot for fast initial sync. This downloads and verifies a certified snapshot from a Mithril aggregator, then imports all blocks into the local database.

```bash
dugite-node mithril-import [OPTIONS]
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--network-magic` | `764824073` | Network magic value |
| `--database-path` | `db` | Path to the database directory |
| `--temp-dir` | | Temporary directory for download and extraction (uses system temp if omitted) |
| `--mithril-genesis-vkey` | | Override the Mithril genesis verification key (JSON hex-encoded Ed25519 verification key string), for private networks |
| `--skip-certificate-verification` | `false` | Skip Mithril STM certificate chain verification (UNSAFE — testing only) |
| `--allow-stale-pparams` | `false` | Continue the import even if the ancillary archive can't be downloaded, falling back to genesis-default protocol parameters at the imported tip. Not recommended for production |
| `--include-ancillary` / `--no-include-ancillary` | `true` | Download and import the Mithril ancillary archive (Haskell ledger state at the immutable tip), dropping bootstrap time from multi-hour to ~15 minutes. `--no-include-ancillary` restores the pre-ancillary behavior of deriving ledger state entirely from chunk-by-chunk block replay — see [Mithril Ancillary](../running/mithril-ancillary.md) |
| `--log-output` | `stdout` | Log output target: `stdout`, `file`, or `journald`. Can be specified multiple times. |
| `--log-format` | `text` | Log format: `text` (human-readable) or `json` (structured). |
| `--log-level` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`). Overridden by `RUST_LOG`. |
| `--log-dir` | `logs` | Directory for log files (used with `--log-output file`) |
| `--log-file-rotation` | `daily` | Log file rotation strategy: `daily`, `hourly`, or `never` |
| `--log-no-color` | `false` | Disable ANSI colors in stdout output |
| `--log-retention-days` | `7` | Number of days to retain log files |
| `--stdout-overflow` | `drop` | Channel-full policy for the non-blocking stdout writer: `drop` or `block` |

### Network Magic Values

| Network | Magic |
|---------|-------|
| Mainnet | `764824073` |
| Preview | `2` |
| Preprod | `1` |

### Example: Preview Testnet

```bash
dugite-node mithril-import \
  --network-magic 2 \
  --database-path ./db-preview

# Then start the node to sync from the snapshot to tip
dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock
```

The import process:

1. Downloads the latest snapshot from the Mithril aggregator
2. Verifies the snapshot digest (SHA256)
3. Extracts and parses immutable chunk files
4. Imports blocks into ChainDB with CRC32 verification
5. Supports resume — skips blocks already in the database

On preview testnet, importing ~4M blocks takes approximately 2 minutes.
