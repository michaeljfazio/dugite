# Quick Start

This guide walks you through getting Dugite running on the Cardano preview testnet.

> **Dugite is in early development and is not recommended for production use.** Run it on testnets only — see [Project Status](./introduction.md#project-status).

## 1. Install

**Option A: Pre-built binary** (fastest)

The release tarball contains `dugite-node`, `dugite-cli`, and the `config/` tree.

```bash
curl -LO https://github.com/michaeljfazio/dugite/releases/latest/download/dugite-x86_64-linux.tar.gz
tar xzf dugite-x86_64-linux.tar.gz
sudo mv dugite-node dugite-cli /usr/local/bin/
```

**Option B: Container image**

```bash
docker pull ghcr.io/michaeljfazio/dugite:latest
```

Multi-arch (`linux/amd64`, `linux/arm64`), ships all four binaries, and bundles `config/` at `/opt/dugite/config/`. See [Installation](./installation.md#container-image).

**Option C: Build from source**

Requires a stable Rust toolchain and `protoc` — see [Installation](./installation.md#system-dependencies).

```bash
git clone https://github.com/michaeljfazio/dugite.git
cd dugite
cargo build --release
```

## 2. Fast Sync with Mithril (Recommended)

Import a Mithril-certified snapshot to skip syncing the chain from genesis:

```bash
dugite-node mithril-import \
  --network-magic 2 \
  --database-path ./db-preview
```

This downloads the latest snapshot from the Mithril aggregator, verifies its certificate chain, extracts it, and bulk-imports the blocks into the ImmutableDB. The ancillary archive (the Haskell ledger state at the immutable tip) is downloaded by default, which cuts bootstrap from multi-hour to roughly 15 minutes; pass `--no-include-ancillary` to replay from blocks instead. See [Mithril Snapshot Import](./running/mithril.md) for snapshot sizes, disk requirements, and the [trust model](./running/mithril-ancillary.md).

Or via the justfile:

```bash
just mithril-import preview
```

## 3. Run the Node

Dugite ships with configuration files for `mainnet`, `preview`, and `preprod`, under `config/<network>/` — `config.json`, `topology.json`, and four genesis files (`byron-genesis.json`, `shelley-genesis.json`, `alonzo-genesis.json`, `conway-genesis.json`). The release tarball and container image both bundle this tree. Network magic is `764824073` for mainnet, `2` for preview, and `1` for preprod.

```bash
dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

Or via the top-level [justfile](./development.md):

```bash
just run-relay preview
```

The node will:
1. Load the configuration and genesis files
2. Replay imported blocks through the ledger (builds UTxO set, protocol params, delegations)
3. Connect to preview testnet peers
4. Sync remaining blocks to chain tip

Progress is logged every 5 seconds, showing sync percentage, blocks-per-second throughput, UTxO count, and epoch number. Logs go to stdout by default; add `--log-output file --log-dir /var/log/dugite` for file logging. See [Logging](./running/logging.md) for all options.

## 4. Query the Node

Once the node is running, query it using the CLI via the Unix domain socket:

```bash
# Query the current tip
dugite-cli query tip \
  --socket-path ./node.sock \
  --testnet-magic 2
```

Example output (field order matches cardano-cli 10.x — alphabetical, no `network` field):

```json
{
    "block": 4094745,
    "epoch": 1232,
    "era": "Conway",
    "hash": "8498ccda...",
    "slot": 106453897,
    "slotInEpoch": 9097,
    "slotsToEpochEnd": 77303,
    "syncProgress": "100.00"
}
```

```bash
# Query protocol parameters
dugite-cli query protocol-parameters \
  --socket-path ./node.sock \
  --testnet-magic 2

# Query mempool
dugite-cli query tx-mempool info \
  --socket-path ./node.sock \
  --testnet-magic 2
```

## 5. Check Metrics

Prometheus metrics are served on port **12796** by default — deliberately offset from cardano-node's 12798 so both can run on the same host. Override with `--metrics-port`, or the `MetricsPort` field in `config.json` (the shipped configs set 12796 explicitly).

```bash
curl -s http://localhost:12796/metrics | grep dugite_sync_progress
# dugite_sync_progress_percent 10000
```

The value is a percentage scaled by 100 — divide by 100 for percent.

## Next Steps

- [Configuration](./running/configuration.md) — Detailed configuration options
- [Networks](./running/networks.md) — Connecting to mainnet, preview, or preprod
- [Mithril Import](./running/mithril.md) — Fast initial sync details
- [Monitoring](./running/monitoring.md) — Prometheus metrics endpoint
- [Kubernetes Deployment](./running/kubernetes.md) — Helm chart for production deployments
- [Relay Node](./running/relay.md) — Running relay nodes for a stake pool
- [Block Producer](./running/block-producer.md) — Running a stake pool
- [CLI Reference](./cli/overview.md) — Full CLI command reference
