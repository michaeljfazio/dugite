# Upgrading Dugite

## Version Compatibility

Dugite is in early development. Between releases, the following may change without backward compatibility:

- **Ledger snapshot format** — Ledger state snapshots (saved in the `ledger/` subdirectory of the database path) use `bincode` serialization. The field order is fixed, but new fields added in a release will make old snapshot files unreadable.
- **Block storage format** — ImmutableDB chunk files use the same on-disk format as `cardano-node` and are forward-compatible. These do not need to be deleted when upgrading.
- **Configuration file format** — New config fields are always optional with sensible defaults. Existing config files are forward-compatible.
- **Protocol versions** — Dugite tracks the N2N and N2C protocol versions supported by the current `cardano-node` release. If the network upgrades to a new era, update Dugite to avoid handshake failures.

## Upgrade Procedure

### 1. Stop the Running Node

```bash
# Graceful shutdown (SIGTERM)
kill $(pidof dugite-node)
# or
pkill dugite-node
```

Wait for the process to exit. Dugite flushes its write buffer and closes the socket cleanly on SIGTERM.

### 2. Install the New Binary

**From a release tarball:**

```bash
curl -LO https://github.com/michaeljfazio/dugite/releases/latest/download/dugite-x86_64-linux.tar.gz
tar xzf dugite-x86_64-linux.tar.gz
sudo mv dugite-node dugite-cli dugite-monitor dugite-config /usr/local/bin/
```

**From source:**

```bash
git pull
cargo build --release
sudo cp target/release/dugite-node target/release/dugite-cli \
         target/release/dugite-monitor target/release/dugite-config \
         /usr/local/bin/
```

**Container:**

```bash
docker pull ghcr.io/michaeljfazio/dugite:latest
```

### 3. Clear Ledger Snapshots (if the snapshot format changed)

If the release notes mention a ledger snapshot format change, delete the saved snapshots before restarting:

```bash
rm -f /path/to/db/ledger/*.snapshot
```

The node will rebuild the ledger state from the ImmutableDB (block storage) on next startup. For large databases this can take several minutes; consider using [Mithril snapshot import](../running/mithril.md) to speed up recovery.

> **Block storage is safe to keep.** Only `db/ledger/` needs to be cleared when the snapshot format changes. Do not delete `db/immutable/` or `db/volatile/` unless instructed.

### 4. Verify Configuration

Check the release notes for any new required configuration fields. New fields are always optional; your existing config will continue to work. To validate:

```bash
dugite-config validate config.json
```

### 5. Restart

```bash
dugite-node run \
  --config config.json \
  --topology topology.json \
  --database-path ./db \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

Confirm the node resumes from the correct tip:

```bash
dugite-cli query tip --socket-path ./node.sock
```

## Checking the Installed Version

```bash
dugite-node --version
dugite-cli --version
```

## Checking the Release Notes

Release notes are published on the [GitHub Releases page](https://github.com/michaeljfazio/dugite/releases). Each release notes which components are affected and whether a ledger snapshot wipe is required.
