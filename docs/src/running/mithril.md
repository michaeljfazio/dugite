# Mithril Snapshot Import

Syncing a Cardano node from genesis can take a very long time. Dugite supports importing [Mithril](https://mithril.network/)-certified snapshots of the immutable database to drastically reduce initial sync time.

## How It Works

Mithril is a stake-based threshold multi-signature scheme that produces certified snapshots of the Cardano immutable database. These snapshots are verified by Mithril signers (stake pool operators) and made available through Mithril aggregator endpoints.

The import process:

1. Queries the Mithril aggregator for the latest available Cardano Database (V2) snapshot
2. Verifies the STM certificate chain back to the network's pinned genesis verification key
3. Downloads the snapshot archive (compressed with zstandard)
4. Extracts the cardano-node chunk files
5. Parses each block using Dugite's in-house multi-era CBOR decoder
6. Bulk-imports blocks into Dugite's ImmutableDB (append-only chunk files)
7. Imports the ancillary archive (the Haskell ledger state at the immutable tip) unless disabled

## Usage

```bash
dugite-node mithril-import \
  --network-magic <magic> \
  --database-path <path>
```

Or via the justfile wrapper, which builds the release binary if needed and picks
the magic and database path for you (`./db-<network>`):

```bash
just mithril-import preview     # preview | preprod | mainnet
```

### Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--network-magic` | `764824073` | Network magic (764824073=mainnet, 2=preview, 1=preprod) |
| `--database-path` | `db` | Path to the database directory |
| `--temp-dir` | system temp | Temporary directory for download and extraction |
| `--include-ancillary` | `true` | Download and import the Mithril ancillary archive (Haskell ledger state at the immutable tip). When enabled, bootstrap drops from multi-hour to ~15 minutes. See [Mithril Ancillary — Trust Model](./mithril-ancillary.md) |
| `--no-include-ancillary` | — | Skip the ancillary download. Equivalent to `--include-ancillary=false`, provided because the negated form reads better in scripts. Conflicts with `--include-ancillary` |
| `--allow-stale-pparams` | `false` | Continue even if the ancillary download fails — falls back to genesis-default protocol parameters at the imported tip (issue #335). NOT recommended for production |
| `--mithril-genesis-vkey` | pinned | Override the Mithril genesis verification key, for private networks. Must be a JSON hex-encoded Ed25519 verification key string |
| `--skip-certificate-verification` | `false` | **UNSAFE, testing only.** Trust the snapshot digest from the aggregator without verifying the certificate chain |

All the standard logging flags (`--log-output`, `--log-format`, `--log-level`, …)
also apply to `mithril-import`. See [Logging](./logging.md).

### Examples

**Mainnet:**

```bash
dugite-node mithril-import \
  --network-magic 764824073 \
  --database-path ./db-mainnet
```

**Preview testnet:**

```bash
dugite-node mithril-import \
  --network-magic 2 \
  --database-path ./db-preview
```

**Preprod testnet:**

```bash
dugite-node mithril-import \
  --network-magic 1 \
  --database-path ./db-preprod
```

## Mithril Aggregator Endpoints

Dugite automatically selects the correct aggregator for each network:

| Network | Aggregator URL |
|---------|---------------|
| Mainnet | `https://aggregator.release-mainnet.api.mithril.network/aggregator` |
| Preview | `https://aggregator.pre-release-preview.api.mithril.network/aggregator` |
| Preprod | `https://aggregator.release-preprod.api.mithril.network/aggregator` |

## Interruption and Re-Runs

An interrupted import **restarts from scratch**. The temporary download
directory is cleared at the start of every run so chunk files from a different
snapshot cannot leak into the new one, and the Mithril client re-fetches
everything. There is no partial-download resume.

Re-running the import is also **destructive to the target database**: before
moving the new chunk files into place, `mithril-import` deletes any existing
`<database-path>/immutable/` directory, plus the Haskell `ledger/` directory and
stale ledger snapshots. It does not take the database lock while doing so.

> **Stop the node before importing.** `mithril-import` writes to the database
> directory with plain filesystem operations and does not acquire the
> `<db>/lock` flock that `dugite-node run` holds, so it will not detect a
> running node and will delete the immutable directory out from under it. Stop
> the node with SIGTERM first, and point `--database-path` at a fresh directory
> if you want to keep the old one.

Download concurrency is tunable via `DUGITE_MITHRIL_DOWNLOAD_PARALLELISM`
(per-immutable-file, default 20, clamped to 1–32).

## After Import

Once the import completes, start the node normally. It will detect the imported blocks and resume syncing from where the snapshot left off:

```bash
dugite-node run \
  --config config/mainnet/config.json \
  --topology config/mainnet/topology.json \
  --database-path ./db-mainnet \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

Stop the node with **SIGTERM**, never `kill -9` — a hard kill risks the active
ImmutableDB chunk's index. See
[Stopping the Node](./configuration.md#stopping-the-node).

## When a Re-Import Is Required

The on-disk ledger snapshot carries a `SNAPSHOT_VERSION`. When a release bumps
it, the old snapshot is rejected on load and the node replays chunks from the
last compatible point; when the *import* format itself changed, a full
`mithril-import` is required instead.

| Upgrading from | Action needed |
|----------------|---------------|
| Before v2.1.0 | **Full `mithril-import` required.** Pre-v2.1.0 imports discarded governance roots (`Proposals.pRoots`), which silently corrupts reward calculation — see issue #898. Re-import; a replay will not repair it |
| v2.1.0 – v2.2.x | Chunk replay on first restart (SNAPSHOT_VERSION reached 31 in v2.3.0) |
| v2.3.0 or later | **Drop-in.** SNAPSHOT_VERSION has been unchanged at 31 since v2.3.0, including the current v2.4.3 release. No re-import and no re-sync |

v2.4.0 additionally introduced two new files inside the database directory —
`lock` and `immutable/clean`. Both are created automatically; no operator action
is required.

## Disk Space Requirements

Mithril snapshots are large. Approximate sizes (which grow over time):

| Network | Compressed Archive | Extracted | Final DB |
|---------|-------------------|-----------|----------|
| Mainnet | ~60-90 GB | ~120-180 GB | ~90-140 GB |
| Preview | ~5-10 GB | ~10-20 GB | ~8-15 GB |
| Preprod | ~15-25 GB | ~30-50 GB | ~20-35 GB |

The temporary directory needs enough space for both the compressed archive and the extracted files. After import, temporary files are automatically cleaned up.

> **Note:** Ensure you have sufficient disk space before starting the import. The `--temp-dir` flag can be used to direct temporary files to a different volume if needed.
