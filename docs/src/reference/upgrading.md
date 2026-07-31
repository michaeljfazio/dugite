# Upgrading Dugite

This page is the operator's upgrade path: what changes between releases, what
each release requires of your database, and the exact procedure to follow.

For what changed *functionally* in a release, read the
[GitHub Releases page](https://github.com/michaeljfazio/dugite/releases). This
page only covers what you have to **do**.

## The one thing that matters: `SNAPSHOT_VERSION`

Dugite's ledger state is persisted as a snapshot under `<database-path>/ledger/`,
stamped with a `SNAPSHOT_VERSION` byte. That number decides how much work an
upgrade costs you:

| Situation | Cost |
|---|---|
| `SNAPSHOT_VERSION` unchanged | **Drop-in.** Swap the binary and restart. |
| `SNAPSHOT_VERSION` bumped | Snapshot rejected on load, ledger rebuilt by **replaying ImmutableDB chunks**. Blocks are kept; expect minutes-to-hours depending on database size. |
| Import format changed | **Full `mithril-import` required.** Replay cannot repair it. |

The current value is **31**, defined at
`crates/dugite-ledger/src/state/snapshot.rs`.

A version mismatch is never destructive. The unreadable snapshot is *renamed*,
not deleted — to `<name>.bin.v<NN>-unreadable` — and the node logs:

```
Quarantined unreadable ledger snapshot — chain will be replayed from ImmutableDB
on next start. Inspect or delete the .v{NN}-unreadable file once recovery completes.
```

Delete the `.v*-unreadable` files once the node is healthy again.

## Upgrade matrix

Find the version you are upgrading **from**:

| Upgrading from | What happens | Action needed |
|---|---|---|
| **Before v2.1.0** | Pre-v2.1.0 Mithril imports discarded governance roots (`Proposals.pRoots`), which silently corrupts reward calculation (issue #898). A chunk replay will not repair it. | **Full `mithril-import` required** — see below |
| **v2.1.0 – v2.2.x** | `SNAPSHOT_VERSION` reached 31 in v2.3.0 (from 29 in v2.1.0, 30 in v2.2.x). Snapshot is quarantined and the ledger replays from chunks. | Stop, upgrade, restart. Allow extra time on first start |
| **v2.3.0 – v2.4.2** | `SNAPSHOT_VERSION` unchanged at 31. | **Drop-in.** Stop, upgrade, restart |

### v2.4.3 (current release)

**Drop-in.** `SNAPSHOT_VERSION` is unchanged at **31**. No re-sync, no
re-import, no snapshot wipe, no config change. Stop the node with SIGTERM,
replace the binaries, restart.

### Release notes that changed operator behaviour

These are the only releases since v2.1.0 that changed anything on disk or in
the shutdown/startup contract. Everything not listed here was a pure drop-in.

- **v2.4.0** — added two files inside the database directory: `<db>/lock` and
  `<db>/immutable/clean`. Both are created automatically and need no operator
  action. Consequence worth knowing: `<db>/lock` is an exclusive advisory flock
  held for the lifetime of `dugite-node run`, so a **second process cannot open
  the same database directory** — it fails fast naming the holder's PID. Tools
  that open the ChainDB (`dugite-node db info`) now fail against a live node by
  design. See [Troubleshooting](./troubleshooting.md#database-directory-is-locked).
- **v2.3.0** — bumped `SNAPSHOT_VERSION` **30 → 31** (issue #919, per-era
  min-UTxO). This is the last release that required a chunk replay. Existing
  databases replay on first restart; blocks are not discarded.
- **v2.1.0** — fixed the Mithril import path that dropped `Proposals.pRoots`
  (#898). Databases imported by any *earlier* version must be re-imported.

## Upgrade procedure

### 1. Stop the node — SIGTERM, never SIGKILL

```bash
# Graceful shutdown
kill $(pidof dugite-node)     # SIGTERM
# or
systemctl stop dugite-node
```

On SIGTERM (or SIGINT / `Ctrl-C`) the node demotes its peers, flushes volatile
blocks to the ImmutableDB, fsyncs the chunk and index files, writes `tip.meta`,
stamps the `immutable/clean` marker, and saves a final ledger snapshot.

`kill -9` skips all of that. The active chunk's index and the clean marker are
left in an unknown state, so the next start pays for an index rebuild and
possibly a chunk reconciliation. Wait for the process to actually exit — the
final snapshot has its own 120 s budget on large databases.

A second SIGTERM during shutdown forces an immediate exit (`exit 143`), so do
not spam the signal.

### 2. Install the new binaries

**From a release tarball:**

```bash
curl -LO https://github.com/michaeljfazio/dugite/releases/latest/download/dugite-x86_64-linux.tar.gz
tar xzf dugite-x86_64-linux.tar.gz
sudo mv dugite-node dugite-cli dugite-monitor dugite-config /usr/local/bin/
```

Published targets: `dugite-x86_64-linux.tar.gz`, `dugite-aarch64-linux.tar.gz`,
`dugite-aarch64-macos.tar.gz`. Checksums are attached to each release as
`SHA256SUMS.txt`.

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
docker pull ghcr.io/michaeljfazio/dugite:2.4.3
```

**Helm:** the chart is published to `oci://ghcr.io/michaeljfazio/charts/dugite-node`
and its `appVersion` tracks the node release. See
[Kubernetes Deployment](../running/kubernetes.md).

### 3. Restart

```bash
dugite-node run \
  --config config.json \
  --topology topology.json \
  --database-path ./db \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

Confirm the node resumed from the right place:

```bash
dugite-node --version
dugite-cli query tip --socket-path ./node.sock
```

### 4. Watch the first minute of logs

An upgrade is the most likely moment for a database problem to surface. These
lines are the ones that matter:

| Log line | Meaning |
|---|---|
| `Quarantined unreadable ledger snapshot` | `SNAPSHOT_VERSION` changed — replay is running, this is expected on a version bump |
| `ImmutableDB: unclean shutdown detected (no clean marker)` | Previous stop was not graceful; the block index is being rebuilt |
| `Ledger tip is BELOW the ImmutableDB tip after replay` | Ledger/immutable seam — see [Troubleshooting](./troubleshooting.md#ledger-tip-is-below-the-immutabledb-tip) |
| `database directory is locked by another dugite process` | The old process has not exited yet, or a second node is pointed at the same directory |
| `inconsistent chunk … Refusing to open with a hole below the tip` | Storage damage below the tip — recovery required, see Troubleshooting |

## Re-import (only when the matrix says so)

```bash
# Stop the node FIRST — mithril-import does not take the DB lock and will
# happily delete the immutable directory out from under a running node.
kill $(pidof dugite-node)

dugite-node mithril-import --network-magic 1 --database-path ./db-preprod
```

Network magic: mainnet `764824073`, preview `2`, preprod `1`. See
[Mithril Snapshot Import](../running/mithril.md).

## Configuration compatibility

New config fields are always optional with defaults, so an existing config file
keeps working across upgrades. Validate before restarting if you edited it:

```bash
dugite-config validate config.json
```

Most config changes can also be applied to a **running** node with `SIGHUP`
(topology and log directives reload live; restart-required fields are listed in
the log and ignored). See [Troubleshooting](./troubleshooting.md#sighup-topology-reload-and-log-verbosity).

## Protocol version compatibility

Dugite tracks the handshake versions supported by the current `cardano-node`
release: **N2N v14–v15** and **N2C v16–v23**. If the network hard-forks to an
era your build predates, peers will refuse the handshake — upgrade Dugite
before the fork. See the [Mini-Protocol Reference](./mini-protocols.md).

## Downgrading

Downgrading across a `SNAPSHOT_VERSION` bump does not work: the older binary
rejects the newer snapshot and there is no forward-compatible reader. It will
quarantine the snapshot and replay from the ImmutableDB, which is safe but slow.
Block storage itself is format-stable and is never the reason to wipe a
database.
