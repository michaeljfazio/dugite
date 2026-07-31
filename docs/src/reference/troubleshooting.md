# Troubleshooting

Common issues and their solutions when running Dugite.

## Build Issues

### Compilation is slow

The initial build compiles all dependencies from source, which takes several minutes. Subsequent builds are much faster due to cargo caching.

For faster development iteration, use debug builds:

```bash
cargo build  # debug mode, faster compilation
```

Only use `--release` when running against a live network.

## Connection Issues

### Cannot connect to peers

**Symptoms:** Node starts but never receives blocks. Logs show connection failures.

**Possible causes:**

1. **Firewall blocking outbound connections on port 3001.** Ensure outbound TCP connections to port 3001 are allowed.

2. **Incorrect network magic.** Verify the `NetworkMagic` in your config matches the target network:
   - Mainnet: `764824073`
   - Preview: `2`
   - Preprod: `1`

3. **DNS resolution failure.** If topology uses hostnames, ensure DNS is working:
   ```bash
   nslookup preview-node.play.dev.cardano.org
   ```

4. **Stale topology.** Peer addresses may change. Download the latest topology from the [Cardano Operations Book](https://book.world.dev.cardano.org/).

### Handshake failures

**Error:** `Handshake failed: version mismatch`

Dugite proposes N2N versions **15 and 14** (preferring 15) and N2C versions
**16 through 23**. This error usually means the peer supports neither N2N 14 nor
15. Note that network-magic mismatch produces a *refusal*, not a version
mismatch — check both. Ensure you are connecting to an up-to-date cardano-node:
- **Mainnet / Preprod:** cardano-node 10.x+ required
- **Preview:** cardano-node 11.0.1+ required (preview is at Protocol Version 11; peers running 10.x will reject the handshake)

## Socket Issues

### Cannot connect to node socket

**Error:** `Cannot connect to node socket './node.sock': No such file or directory`

**Solutions:**

1. **Node is not running.** Start the node first.

2. **Wrong socket path.** Verify the socket path matches what the node was started with:
   ```bash
   dugite-cli query tip --socket-path /path/to/actual/node.sock
   ```

3. **Permission denied.** Ensure the user running the CLI has read/write access to the socket file.

4. **Stale socket file.** If the node crashed, the socket file may remain. Delete it and restart:
   ```bash
   rm ./node.sock
   dugite-node run ...
   ```

### Socket permission denied

**Error:** `Permission denied (os error 13)`

The Unix socket file inherits the permissions of the process that created it. Ensure both the node and CLI processes run as the same user, or adjust the socket file permissions.

## Stopping the Node

**Always stop the node with SIGTERM (or SIGINT / `Ctrl-C`). Never `kill -9`.**

On SIGTERM the node demotes its peers, flushes volatile blocks to the
ImmutableDB, fsyncs the chunk and secondary index files, writes the primary
index and `tip.meta`, persists the mmap block index, stamps the
`immutable/clean` marker, and finally saves a ledger snapshot. The flush and
persist phase has a 30 s budget; the final snapshot has its own 120 s budget on
large databases. Wait for the process to actually exit.

`kill -9` skips every one of those steps. The recorded consequences are a
rebuilt block index at minimum, and — historically — a lost active-chunk index
that cost roughly ten hours of blocks.

A **second** SIGINT/SIGTERM during shutdown forces an immediate exit
(`exit 130` / `exit 143`), so do not repeat the signal while waiting.

```bash
kill $(pidof dugite-node)      # correct
systemctl stop dugite-node     # correct
kill -9 $(pidof dugite-node)   # do not do this
```

## Storage Issues

### Database directory is locked

**Error:**

```
database directory is locked by another dugite process (pid 12345) — refusing
concurrent open of /path/to/db/lock (issue #929: a second writer would corrupt
the ImmutableDB)
```

Since v2.4.0 `dugite-node run` takes an **exclusive advisory flock** on
`<database-path>/lock` before touching any other file, and holds it for the
process lifetime. Two writers on one directory corrupt the ImmutableDB, so the
second one fails fast and names the PID holding the lock.

**What to do:**

1. Check whether that PID is still alive (`ps -p 12345`). If your previous node
   is still shutting down, wait — the lock is released when its file descriptor
   closes.
2. If two nodes are genuinely configured against the same `--database-path`,
   give each its own directory. Ports are not the only thing that must be
   distinct.
3. A crashed process leaves **no** stale lock — the kernel releases the flock on
   process death. The `lock` file itself is never deleted; its presence alone
   means nothing.

Two caveats:

- `dugite-node db info` also opens the ChainDB read-write, so it takes the same
  lock and will fail against a live node. This is by design.
- **`mithril-import` does not take the lock.** It will happily delete the
  immutable directory out from under a running node. Stop the node first.

### Unclean shutdown detected

**Log:**

```
ImmutableDB: unclean shutdown detected (no clean marker) — rebuilding mmap block
index from secondary entries (#928)
```

The `<db>/immutable/clean` marker is written by the graceful shutdown flush and
removed the moment the node opens the database for writing. Its absence at
startup means the previous stop was not graceful, so the persistent mmap hash
index cannot be trusted and is rebuilt from the secondary index entries.

This is recovery working, not a fault. It costs startup time proportional to
database size, and nothing else. If you see it after every restart, your stop
procedure is sending SIGKILL somewhere — check your service manager's
`KillSignal` and `TimeoutStopSec`.

### Chunk reconciliation and quarantine on open

Every open reconciles the on-disk chunks before any index is trusted. The
messages you may see, and what each means:

| Log line | Meaning |
|---|---|
| `truncating torn trailing bytes from tail chunk's secondary index` | Partial index write from a hard stop; trimmed |
| `reconciling tail chunk — truncating to the verified prefix; dropped blocks will be re-fetched from peers` | The tail chunk was CRC-verified block by block and cut back to the last good one. The dropped blocks come back from peers |
| `quarantining unservable tail chunk — data preserved, blocks will be re-fetched from peers` | The tail chunk could not be verified at all. Renamed to `<NNNNN>.chunk.orphaned`; its `.secondary` and `.primary` are deleted |
| `tail chunk does not chain onto the previous chunk — quarantining the orphan island above the hole` | The chain has a break at the tail boundary; everything above it is quarantined |

Quarantined data is **preserved**, never deleted. Once the node is healthy you
can remove the `.chunk.orphaned` files.

### Inconsistent chunk — node refuses to start

**Error:**

```
inconsistent chunk 00123 in ImmutableDB: <reason>. Refusing to open with a hole
below the tip (issues #926/#928) — restore the damaged chunk (e.g.
`dugite-node mithril-import`) or remove the damaged chunk files manually
```

This is deliberate. Damage at the *tail* is recoverable by truncation or
quarantine; damage **below** the tail would leave a hole in the middle of the
chain, and serving from a holed chain is worse than refusing to start.

**Recovery, in order of preference:**

```bash
# 1. Re-import from Mithril into a fresh directory (fastest)
dugite-node mithril-import --network-magic <magic> --database-path ./db-new

# 2. Full resync from genesis (slowest, always works)
rm -rf ./db-path
dugite-node run ...
```

### Ledger tip is below the ImmutableDB tip

**Log (WARN, at startup after replay):**

```
Ledger tip is BELOW the ImmutableDB tip after replay — the immutable chain
contains blocks the ledger could not apply ... Sync will advance the ledger from
ChainDB via the gap-bridge where possible; if this gap persists, inspect the
seam and consider re-import via `dugite-node mithril-import` (#927).
```

The immutable chain holds blocks the ledger could not apply — typically after
crash damage or a replay apply failure. The node handles this rather than
wedging: it offers its known points **newest-first by slot** (immutable tip
ahead of the stale ledger tip), and it exempts the peer's initial protocol-
mandated rollback to the exactly-agreed intersection from the divergent-peer
guard. Before this behaviour existed, the guard disconnected every peer for
rolling back to a point the node had itself offered, and sync stopped forever.

**What to do:** watch the gap. If `ledger_slot` climbs toward
`immutable_tip_slot` over the next few minutes, it is self-healing — leave it.
If it stays flat, re-import.

### Ledger snapshot rejected on startup

**Log:**

```
Quarantined unreadable ledger snapshot — chain will be replayed from ImmutableDB
on next start. Inspect or delete the .v{NN}-unreadable file once recovery
completes.
```

The snapshot's `SNAPSHOT_VERSION` does not match this build's. Expected after an
upgrade that bumps it — see [Upgrading](./upgrading.md). The file is renamed to
`<name>.bin.v<NN>-unreadable`, never deleted, and the ledger is rebuilt by
replaying ImmutableDB chunks. Blocks are not lost.

Related messages that do **not** quarantine (delete the snapshot by hand if they
recur): `Snapshot is missing the DUGT framing header`, `Snapshot checksum
mismatch — file may be corrupted`.

### Database corruption (last resort)

If none of the targeted recoveries above apply, delete the database and resync:

```bash
rm -rf ./db-path
dugite-node run ...
```

For faster recovery, use [Mithril snapshot import](../running/mithril.md):

```bash
rm -rf ./db-path
dugite-node mithril-import --network-magic 2 --database-path ./db-path
dugite-node run ...
```

### Disk space

Cardano databases grow continuously. Approximate sizes:

| Network | Database Size |
|---------|--------------|
| Mainnet | 90-140+ GB |
| Preview | 8-15+ GB |
| Preprod | 20-35+ GB |

Monitor disk usage and ensure adequate free space.

## Sync Issues

### Sync is slow

**Possible causes:**

1. **Single peer.** Dugite benefits from multiple peers for block fetching. Ensure your topology includes multiple bootstrap peers or enable ledger-based peer discovery.

2. **Network latency.** The ChainSync protocol has an inherent per-header RTT (~300ms). High-latency connections will reduce throughput.

3. **Slow disk.** Storage performance depends on disk I/O speed. SSDs are strongly recommended. On Linux, enable `io_uring` for improved UTxO storage performance: `cargo build --release --features io-uring`.

4. **CPU-bound during ledger validation.** Block processing includes UTxO validation and Plutus script execution. This is CPU-intensive during sync.

**Recommendation:** Use [Mithril snapshot import](../running/mithril.md) to bypass the initial sync bottleneck entirely.

### Sync stalls

**Symptoms:** Progress percentage stops increasing, no new blocks logged.

**Possible causes:**

1. **Peer disconnected.** The node will reconnect automatically with exponential backoff. Wait a few minutes.

2. **All peers at same height.** If all configured peers are also syncing, they may not have new blocks to serve. Add more peers to the topology.

3. **Resource exhaustion.** Check for out-of-memory or file descriptor limits.

## Memory Issues

### Out of memory

Dugite's memory usage depends on:
- UTxO set size (the largest memory consumer)
- Number of connected peers
- VolatileDB (last k=2160 blocks in memory)

For mainnet, expect memory usage of 8-16 GB depending on sync progress.

If running on a memory-constrained system, ensure adequate swap space is configured.

## Logging

### Increase log verbosity

Use the `RUST_LOG` environment variable:

```bash
# Debug all crates
RUST_LOG=debug dugite-node run ...

# Debug specific crate
RUST_LOG=dugite_network=debug dugite-node run ...

# Trace level (very verbose)
RUST_LOG=trace dugite-node run ...
```

### Log to file

Use the built-in file logging:

```bash
dugite-node run --log-output file --log-dir /var/log/dugite ...
```

Log files are rotated daily by default. See [Logging](../running/logging.md) for rotation options and multi-target output.

## SIGHUP: Topology Reload and Log Verbosity

Sending `SIGHUP` to the node reloads the topology file and the hot-reloadable
parts of the node config, without a restart. Fields that require a restart are
named in the log and ignored (`config_reload: restart-required fields changed —
ignored`), so a SIGHUP never half-applies a change.

1. **Topology reload** — The node re-reads the topology file and updates the peer manager:

   ```bash
   # Edit topology.json, then:
   kill -HUP $(pidof dugite-node)
   ```

2. **Log verbosity reload** — If `LogDirective` is set in the config file, the per-subsystem log filter is reloaded:

   ```bash
   # Add/update in your config JSON:
   # "LogDirective": "info,dugite_network=trace"
   #
   # Then send SIGHUP:
   kill -HUP $(pidof dugite-node)
   ```

   This is useful for enabling trace logging for a specific subsystem while the node is running, without disrupting sync or block production.

See [Logging](../running/logging.md#runtime-log-verbosity-reload-sighup) for full details.

## Block Producer Issues

### Block producer shows ZERO stake

**Cause:** Snapshot loaded before UTxO store was attached, corrupting `pool_stake` values.

**Fix:** Automatic on restart — `rebuild_stake_distribution` runs after UTxO store attachment.

**Verify:** Check the log for `"Block producer: pool stake in 'set' snapshot"` with a non-zero `pool_stake_lovelace` value.

### Node enters reconnection loop after forging

**Cause:** Forged block lost a slot battle and was persisted to ImmutableDB.

**Symptoms:** Log shows `"intersection fell to Origin"` or the node repeatedly reconnects to upstream peers.

**Fix:** The fork recovery mechanism now handles this automatically. If the issue persists, re-import from Mithril:

```bash
dugite-node mithril-import --network-magic <magic> --database-path <path>
```

See [Fork Recovery & ImmutableDB Contamination](../architecture/storage.md#fork-recovery--immutabledb-contamination) for details on how the recovery mechanism works.

## Epoch & State Issues

### Epoch number appears wrong (e.g., epoch 445 instead of 1239)

**Cause:** Snapshot saved with incorrect `epoch_length` defaults (mainnet 432000 instead of preview 86400).

**Fix:** Automatic correction on load — the epoch is recalculated from the tip slot using genesis parameters.

**Log message:** `"Snapshot epoch differs from computed epoch — correcting"`

### VRF verification failures after restart

**Cause:** Epoch nonce in snapshot may be stale if saved with wrong epoch boundaries, or the node is replaying blocks in non-strict mode.

**Fix:** VRF verification is non-fatal during non-strict (initial sync / replay) mode. Once the node reaches the chain tip it enables strict verification and the serialized `epoch_nonce` from the snapshot is used directly — matching Haskell's behavior.

## Getting Help

If you encounter an issue not covered here:

1. Check the [GitHub issues](https://github.com/michaeljfazio/dugite/issues)
2. Open a new issue with:
   - Dugite version (`dugite-node --version`)
   - Operating system
   - Configuration files (redact any sensitive info)
   - Relevant log output
   - Steps to reproduce
