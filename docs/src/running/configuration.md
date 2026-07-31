# Configuration

Dugite reads a JSON configuration file that controls network settings, genesis file paths, P2P parameters, and tracing options. The format is compatible with the cardano-node configuration format.

## Configuration File Format

The configuration file uses PascalCase keys (matching the cardano-node convention). This is `config/preview/config.json` as shipped in the repository:

```json
{
  "Network": "Testnet",
  "NetworkMagic": 2,
  "DiffusionMode": "InitiatorAndResponder",
  "ByronGenesisFile": "byron-genesis.json",
  "ByronGenesisHash": "81cf23542e33d64c541699926c2b5e6e9c286583f0c8a3fb5f22ea7b352dd174",
  "ShelleyGenesisFile": "shelley-genesis.json",
  "ShelleyGenesisHash": "363498d1024f84bb39d3fa9593ce391483cb40d479b87233f868d6e57c3a400d",
  "AlonzoGenesisFile": "alonzo-genesis.json",
  "ConwayGenesisFile": "conway-genesis.json",
  "TargetNumberOfRootPeers": 60,
  "TargetNumberOfActivePeers": 15,
  "TargetNumberOfEstablishedPeers": 30,
  "TargetNumberOfKnownPeers": 85,
  "TargetNumberOfActiveBigLedgerPeers": 5,
  "TargetNumberOfEstablishedBigLedgerPeers": 10,
  "TargetNumberOfKnownBigLedgerPeers": 15,
  "MinSeverity": "Info",
  "LogDirective": "info",
  "MetricsPort": 12796,
  "ExperimentalHardForksEnabled": true
}
```

> **Unknown keys are ignored.** The node's deserializer does not reject fields it
> does not recognise, so a typo in a key name silently leaves the default in
> force with no warning. Run `dugite-config validate <file>` to have unknown keys
> reported — see [Configuration Editor](./config-editor.md).

## Fields Reference

### Network Settings

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `Network` | string | `"Mainnet"` | Network identifier: `"Mainnet"` or `"Testnet"` |
| `NetworkMagic` | integer | auto | Network magic number. If omitted, derived from `Network` (764824073 for mainnet) |
| `DiffusionMode` | string | `"InitiatorAndResponder"` | Controls inbound connection acceptance. `"InitiatorAndResponder"` (default): relay mode, accepts inbound N2N connections. `"InitiatorOnly"`: block producer mode, outbound only (no listening port opened) |
| `PeerSharing` | boolean/null | `null` | Enable the peer sharing mini-protocol. When `null` (default), peer sharing is automatically disabled for block producers (when `--shelley-kes-key` is provided) and enabled for relays. Set explicitly to override |
| `ConsensusMode` | string | `"Praos"` | `"Praos"` (default) or `"Genesis"` (trustless bulk sync). `"PraosMode"` / `"GenesisMode"` are accepted legacy aliases. The `--consensus-mode` CLI flag, taking `praos` or `genesis`, overrides this field |
| `ExperimentalHardForksEnabled` | boolean | `false` | Advertise readiness for the next major protocol version. `false` → the node signals `ProtVer 11 0` in forged headers and rejects headers whose on-chain protocol version exceeds 11; `true` → signals `ProtVer 12 0` (Dijkstra) and accepts up to 12. Must stay `false` on mainnet. The shipped `config/preview/config.json` sets it `true` |

> **`EnableP2P` is not a Dugite config key.** Dugite is always P2P; there is no
> non-P2P path to switch off. If your config file carries `EnableP2P` (from a
> cardano-node 8.x config) the node ignores it silently.

### Protocol

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `Protocol` | string/object | absent | Accepts either a bare string (e.g. `"Cardano"`, which is ignored) or an object carrying `RequiresNetworkMagic` |
| `Protocol.RequiresNetworkMagic` | string | `"RequiresMagic"` | Whether network magic is required in the handshake |
| `RequiresNetworkMagic` | string | none | The same setting at the top level, for guild-style and newer cardano-node configs |

> **These three are inert.** They are parsed so a cardano-node config file drops
> in without error, but nothing in the workspace reads them — Dugite always
> sends the network magic in the N2N handshake. `NetworkMagic` is the field that
> actually decides which network you join.

### Genesis Files

Genesis file paths are resolved relative to the directory containing the configuration file. For example, if your config is at `/opt/cardano/config.json` and specifies `"ShelleyGenesisFile": "shelley-genesis.json"`, Dugite will look for `/opt/cardano/shelley-genesis.json`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `ByronGenesisFile` | string | none | Path to Byron genesis JSON |
| `ShelleyGenesisFile` | string | none | Path to Shelley genesis JSON |
| `AlonzoGenesisFile` | string | none | Path to Alonzo genesis JSON |
| `ConwayGenesisFile` | string | none | Path to Conway genesis JSON |
| `DijkstraGenesisFile` | string | none | Path to Dijkstra genesis JSON (post-Conway HFC). Parsed at startup but not yet applied to runtime ledger rules. Overridden by the `--dijkstra-genesis` CLI flag |
| `ByronGenesisHash` | string | none | Expected Blake2b-256 of the Byron genesis file, as 64 hex characters |
| `ShelleyGenesisHash` | string | none | Expected Blake2b-256 of the Shelley genesis file |
| `AlonzoGenesisHash` | string | none | Expected Blake2b-256 of the Alonzo genesis file |
| `ConwayGenesisHash` | string | none | Expected Blake2b-256 of the Conway genesis file |
| `DijkstraGenesisHash` | string | none | Expected Blake2b-256 of the Dijkstra genesis file |

At startup the node checks that every configured genesis file exists (resolved
against the config file's directory) and that every configured hash is exactly
64 hex characters. Either check failing is a fatal startup error naming the era.

> **Tip:** Genesis files for each network can be downloaded from the [Cardano Operations Book](https://book.world.dev.cardano.org/).

### P2P Parameters

These parameters control the P2P peer governor's target counts, matching the cardano-node defaults. The governor continuously works to maintain these targets by promoting/demoting peers and discovering new ones.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `TargetNumberOfRootPeers` | integer | 60 | Target number of root peers (bootstrap + local + public roots) |
| `TargetNumberOfActivePeers` | integer | 20 | Target number of active (hot) peers — fully syncing with ChainSync + BlockFetch |
| `TargetNumberOfEstablishedPeers` | integer | 30 | Target number of established (warm) peers — TCP connected, keepalive running |
| `TargetNumberOfKnownPeers` | integer | 150 | Target number of known (cold) peers in the peer table |
| `TargetNumberOfActiveBigLedgerPeers` | integer | 5 | Target number of active big ledger peers (high-stake SPOs, prioritised during sync) |
| `TargetNumberOfEstablishedBigLedgerPeers` | integer | 10 | Target number of established big ledger peers |
| `TargetNumberOfKnownBigLedgerPeers` | integer | 15 | Target number of known big ledger peers |

Two of these are advisory in Dugite rather than direct policy levers:
`TargetNumberOfRootPeers` is validated and exported as a Prometheus gauge, but
root-peer connectivity is actually driven per-group by the topology's
`hotValency` / `warmValency`; and `TargetNumberOfKnownBigLedgerPeers` is not
enforced as a separate cap, because the known set as a whole is bounded by
`TargetNumberOfKnownPeers` and selectively forgetting scarce big ledger peers
would hurt Genesis sync.

#### Genesis-mode sync targets

A second target set applies while the node is in Genesis-mode bulk sync. It is
parsed and validated unconditionally, but only takes effect when
`ConsensusMode` is `"Genesis"`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `SyncTargetNumberOfActivePeers` | integer | 5 | Active peers during Genesis bulk sync |
| `SyncTargetNumberOfEstablishedPeers` | integer | 10 | Established peers during Genesis bulk sync |
| `SyncTargetNumberOfKnownPeers` | integer | 150 | Known peers during Genesis bulk sync |
| `SyncTargetNumberOfRootPeers` | integer | 0 | Root peers during Genesis bulk sync |
| `SyncTargetNumberOfActiveBigLedgerPeers` | integer | 30 | Active big ledger peers during Genesis bulk sync |
| `SyncTargetNumberOfEstablishedBigLedgerPeers` | integer | 40 | Established big ledger peers during Genesis bulk sync |
| `SyncTargetNumberOfKnownBigLedgerPeers` | integer | 100 | Known big ledger peers during Genesis bulk sync |
| `MinBigLedgerPeersForTrustedState` | integer | 5 | Pause sync if active big ledger peers drop below this |

#### Startup validation

Both target sets are checked at startup against the same predicates as Haskell's
`sanePeerSelectionTargets`, and a violation is a fatal startup error naming the
set (`[deadline]` or `[sync]`):

- `active <= established <= known`, and `root <= known`
- `activeBigLedger <= establishedBigLedger <= knownBigLedger`
- `active <= 100`, `established <= 1000`, `known <= 10000` (and the same three
  ceilings for the big-ledger-peer counts)

#### Churn

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `ChurnIntervalNormalSecs` | integer | 3300 | Governor churn interval while caught up (55 min, matching cardano-node) |
| `ChurnIntervalSyncSecs` | integer | 900 | Governor churn interval while syncing (15 min) |

### Ouroboros Genesis Tuning

`LowLevelGenesisOptions` mirrors cardano-node's object of the same name and is
only consulted when `ConsensusMode` is `"Genesis"`. Omit the whole object to get
the upstream defaults.

```json
"LowLevelGenesisOptions": {
  "EnableCSJ": true,
  "EnableLoEAndGDD": true,
  "EnableLoP": true,
  "BlockFetchGracePeriod": 10,
  "BucketCapacity": 100000,
  "BucketRate": 500,
  "CSJJumpSize": 4320,
  "GDDRateLimit": 1.0
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `EnableCSJ` | boolean | `true` | Enable ChainSync Jumping |
| `EnableLoEAndGDD` | boolean | `true` | Enable Limit on Eagerness + Genesis Density Disconnection |
| `EnableLoP` | boolean | `true` | Enable the Limit on Patience leaky bucket |
| `BlockFetchGracePeriod` | float | 10 | Seconds before rotating a starving bulk-sync peer |
| `BucketCapacity` | integer | 100000 | LoP bucket capacity, in tokens |
| `BucketRate` | integer | 500 | LoP bucket leak rate, in tokens/second |
| `CSJJumpSize` | integer | 4320 | CSJ jump size in slots (2 × 2160, the Byron forecast range) |
| `GDDRateLimit` | float | 1.0 | Minimum seconds between GDD evaluations |
| `SnapshotMinIntervalBulkSync` | float | 1800 | Dugite-specific (not a cardano-node key). Minimum wall-clock seconds between epoch-boundary ledger snapshots during bulk sync. Raising it cuts snapshot I/O; lowering it shrinks the rollback blast radius on an unexpected stop |

### Checkpoints

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `CheckpointsFile` | string | none | Path to a lightweight-checkpoints JSON file (`{"checkpoints":[{"blockNo":N,"hash":"<hex>"},...]}`), resolved relative to the config file's directory. Checkpoints are enforced for every header in **both** consensus modes |
| `CheckpointsFileHash` | string | none | Blake2b-256 hex of the checkpoints file bytes. A mismatch is a fatal startup error |

### Connection Management

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `AcceptedConnectionsLimit.hardLimit` | integer | 512 | Refuse new inbound connections beyond this count |
| `AcceptedConnectionsLimit.softLimit` | integer | 384 | Start delaying new inbound connections at this count |
| `AcceptedConnectionsLimit.delay` | float | 5.0 | Maximum delay in seconds applied above the soft limit |
| `PerIpRateLimitN2n` | integer | 5 | Maximum concurrent N2N inbound connections per source IP. `0` disables per-IP limiting (not recommended) |
| `MaxN2cConnections` | integer | 16 | Maximum concurrent N2C (Unix socket) connections |
| `BlockFetchMaxRange` | integer | max | Maximum blocks pulled by a single BlockFetch `MsgRequestRange`. Clamped to `[64, 2000]` at use; omitted means the 2000-block network cap. The `DUGITE_BLOCKFETCH_MAX_RANGE` env var overrides this field |

`AcceptedConnectionsLimit` also accepts the older long key names
(`acceptedConnectionsHardLimit`, `acceptedConnectionsSoftLimit`,
`acceptedConnectionsDelay`) as aliases.

The following four keys are **parsed for cardano-node config-file compatibility
but not currently enforced**. Setting them changes nothing; they are reserved so
that a cardano-node config drops in without a parse error.

| Field | Type | Default | Why it is inert |
|-------|------|---------|-----------------|
| `ProtocolIdleTimeout` | float | 5.0 | Dugite prunes idle connections via the connection manager's own 300 s `INBOUND_IDLE_TIMEOUT` |
| `TimeWaitTimeout` | float | 60.0 | Dugite relies on the OS TCP `TIME_WAIT` |
| `EgressPollInterval` | float | 0.0 | The governor runs on a fixed, tuned 2 s tick |
| `ChainSyncIdleTimeout` | float | none | Dugite randomises the timeout between Haskell's `minChainSyncTimeout` / `maxChainSyncTimeout` bounds; a fixed override would defeat that |

### Metrics, RPC, and Storage

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `MetricsPort` | integer | 12796 | Prometheus metrics port. `0` disables the server. Dugite's default is deliberately offset from cardano-node's 12798 so both can run on one host |
| `TurnOnLogMetrics` | boolean | `true` | Master switch for the metrics endpoint, matching cardano-node. `false` disables the server regardless of `MetricsPort` |
| `Rpc` | object | none | UTxO RPC (gRPC) server block: `Enabled`, `ListenAddr`, `Port`, `MaxConcurrentStreams`, `StreamBufferSize`, `ReflectionEnabled`, `WebEnabled`, `AlphaEnabled`, `Tls: {CertPath, KeyPath}`. See [UTxO RPC](./utxo-rpc.md) |
| `Storage` | object | none | Storage overrides layered on top of `--storage-profile` (index type, UTxO backend, LSM memtable/cache/bloom sizing) |

Effective metrics port, highest precedence first: `--no-metrics` → `0`;
`--metrics-port <PORT>`; `TurnOnLogMetrics: false` → `0`; `MetricsPort`;
otherwise `12796`. Note that an explicit `--metrics-port` wins even over
`TurnOnLogMetrics: false`.

### Tracing

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `MinSeverity` | string | `"Info"` | Minimum log severity, in cardano-node's syslog vocabulary: `Debug`, `Info`, `Notice`, `Warning`, `Error`, `Critical`, `Alert`, `Emergency`. Mapped onto `tracing` levels — `Notice` → `info`, and `Critical`/`Alert`/`Emergency` → `error`. Anything unrecognised falls back to `info` |
| `LogDirective` | string | none | `RUST_LOG`-style filter directive (e.g. `"info,dugite_network=trace"`). Applied at startup **and** on SIGHUP, and takes precedence over `MinSeverity` |
| `TraceOptions.TraceBlockFetchClient` | boolean | `false` | Trace block fetch client activity |
| `TraceOptions.TraceBlockFetchServer` | boolean | `false` | Trace block fetch server activity |
| `TraceOptions.TraceChainDb` | boolean | `false` | Trace ChainDB operations |
| `TraceOptions.TraceChainSyncClient` | boolean | `false` | Trace chain sync client activity |
| `TraceOptions.TraceChainSyncServer` | boolean | `false` | Trace chain sync server activity |
| `TraceOptions.TraceForge` | boolean | `false` | Trace block forging |
| `TraceOptions.TraceMempool` | boolean | `false` | Trace mempool activity |

## Log Level Control

Verbosity is resolved with this precedence, highest first:

1. `RUST_LOG` environment variable
2. `--log-level` CLI flag
3. `LogDirective` config field
4. `MinSeverity` config field

```bash
# Via CLI flag
dugite-node run --log-level debug ...

# Via environment variable (takes priority over --log-level)
RUST_LOG=info dugite-node run ...

# Debug only for specific crates
RUST_LOG=dugite_network=debug,dugite_consensus=debug dugite-node run ...
```

Because the config file is read after the tracing subscriber is constructed, the
config-file values are applied via a live filter reload immediately after
startup — but only when neither `RUST_LOG` nor `--log-level` is set, so an
explicit operator override is never clobbered by the file.

That guard applies at startup only. A later SIGHUP applies `LogDirective` /
`MinSeverity` unconditionally, so a reload *will* override a level you set on
the command line or in the environment.

Dugite supports multiple log output targets (stdout, file, journald) and file rotation. See [Logging](./logging.md) for full details on output configuration.

## Live Reload (SIGHUP)

Sending `SIGHUP` re-reads both the config file and the topology file. Changed
fields are partitioned into those that can be applied live and those that need a
restart; restart-required changes are logged as warnings and the reloadable ones
are still applied.

**Hot-reloadable:** all seven `TargetNumberOf*` deadline targets,
`ChurnIntervalNormalSecs`, `ChurnIntervalSyncSecs`, `LogDirective`,
`MinSeverity`.

**Restart required:** `Network`, `NetworkMagic`, every genesis file path and
hash, `MetricsPort`, `TurnOnLogMetrics`, `DiffusionMode`,
`ExperimentalHardForksEnabled`, `ConsensusMode` — plus everything set on the
command line (database and socket paths, listen address and port, KES/VRF/OpCert
paths).

```bash
kill -HUP $(pgrep -x dugite-node)
```

## Stopping the Node

Always stop the node with **SIGTERM** (or SIGINT / `Ctrl-C`), never `SIGKILL`.

```bash
kill -TERM $(pgrep -x dugite-node)     # correct
kill -9    $(pgrep -x dugite-node)     # do not do this
```

On SIGTERM the node demotes its peers, flushes storage, and writes a final
ledger snapshot. A hard kill skips all of that and risks damaging the active
ImmutableDB chunk's secondary index. Since v2.4.0 the node reconciles that
damage at open — verifying the tail chunk by CRC, truncating to the verified
prefix, and quarantining an index-less tail chunk as `.chunk.orphaned` — but
recovery still costs the un-flushed blocks, and damage below the tail chunk is a
hard `InconsistentChunk` error rather than something the node repairs.

A second SIGINT/SIGTERM during shutdown forces an immediate exit, matching
cardano-node's behaviour, so you never need `kill -9` to get out of a wedged
shutdown.

`ChainDB::open` takes an exclusive advisory `flock` on `<database-path>/lock`,
so a second process pointed at the same directory fails fast and names the pid
already holding it, instead of two nodes silently interleaving writes.

## Minimal Configuration

The smallest viable configuration file specifies only the network:

```json
{
  "Network": "Testnet",
  "NetworkMagic": 2
}
```

All other fields use the defaults tabulated above. Note that with no genesis
files specified the node falls back to built-in default protocol parameters,
which will not match a real network — for anything beyond a smoke test, point at
the genesis files for the network you are joining.

## Format Support

The parser picks its format from the file extension: `.json` is parsed as
cardano-node-compatible JSON, and **anything else** is parsed as TOML. If the
path does not exist at all, the node starts on built-in defaults rather than
failing — so a typo in `--config` produces a mainnet-default node, not an error.

## Editing Configuration Interactively

`dugite-config` is a standalone TUI for browsing and editing these files with
per-field type validation, tuning hints, a diff view, and a save-and-SIGHUP live
reload. See [Configuration Editor (dugite-config)](./config-editor.md).
