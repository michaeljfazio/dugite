# Networks

Dugite can connect to any Cardano network. Each network is identified by a unique magic number used during the N2N handshake.

## Network Magic Values

| Network | Magic | Description |
|---------|-------|-------------|
| **Mainnet** | `764824073` | The production Cardano network |
| **Preview** | `2` | Fast-moving testnet for early feature testing |
| **Preprod** | `1` | Stable testnet that mirrors mainnet behavior |

## Ready-Made Configs

The repository ships complete, self-contained config and topology files for all
three networks under `config/<network>/`, alongside the genesis files they
reference:

```
config/mainnet/{config,topology,byron-genesis,shelley-genesis,alonzo-genesis,conway-genesis}.json
config/preview/{...}
config/preprod/{...}
```

Paths inside them are relative, so they work in place:

```bash
just run-relay preview        # or: just run-bp preview

# equivalently
dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 --port 3001
```

The sections below show minimal hand-written equivalents if you would rather
build your own.

## Connecting to Mainnet

Create a `config-mainnet.json`:

```json
{
  "Network": "Mainnet",
  "NetworkMagic": 764824073
}
```

Create a `topology-mainnet.json`:

```json
{
  "bootstrapPeers": [
    { "address": "backbone.cardano.iog.io", "port": 3001 },
    { "address": "backbone.mainnet.cardanofoundation.org", "port": 3001 },
    { "address": "backbone.mainnet.emurgornd.com", "port": 3001 }
  ],
  "localRoots": [],
  "publicRoots": [
    {
      "accessPoints": [
        { "address": "backbone.cardano.iog.io", "port": 3001 },
        { "address": "backbone.mainnet.cardanofoundation.org", "port": 3001 }
      ],
      "advertise": false
    }
  ],
  "useLedgerAfterSlot": 0
}
```

Run the node:

```bash
dugite-node run \
  --config config-mainnet.json \
  --topology topology-mainnet.json \
  --database-path ./db-mainnet \
  --socket-path ./node-mainnet.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

> **Tip:** For a faster initial mainnet sync, consider using [Mithril snapshot import](./mithril.md) first.

> **Note:** A config with no genesis files, like the minimal one above, starts on
> built-in default protocol parameters rather than mainnet's. Point at the real
> genesis files — `config/mainnet/` has them — before syncing for real.

## Connecting to Preview Testnet

> **Note:** Preview testnet is at Protocol Version 11 (PV11). Peers running
> cardano-node 10.x will reject the connection with a version mismatch. Use
> cardano-node 11.0.1+ for any preview peer or soak rig.

Create a `config-preview.json`. The shipped `config/preview/config.json` sets
`ExperimentalHardForksEnabled: true`, which makes the node signal `ProtVer 12 0`
and accept on-chain protocol versions up to 12, rather than the default
`ProtVer 11 0` / max 11:

```json
{
  "Network": "Testnet",
  "NetworkMagic": 2,
  "ExperimentalHardForksEnabled": true
}
```

Create a `topology-preview.json`:

```json
{
  "bootstrapPeers": [
    { "address": "preview-node.play.dev.cardano.org", "port": 3001 }
  ],
  "localRoots": [
    { "accessPoints": [], "advertise": false, "trustable": false, "valency": 1 }
  ],
  "publicRoots": [
    {
      "accessPoints": [
        { "address": "preview-node.play.dev.cardano.org", "port": 3001 }
      ],
      "advertise": false
    }
  ],
  "useLedgerAfterSlot": 102729600
}
```

Run the node:

```bash
dugite-node run \
  --config config-preview.json \
  --topology topology-preview.json \
  --database-path ./db-preview \
  --socket-path ./node-preview.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

## Connecting to Preprod Testnet

Create a `config-preprod.json`:

```json
{
  "Network": "Testnet",
  "NetworkMagic": 1
}
```

Create a `topology-preprod.json`:

```json
{
  "bootstrapPeers": [
    { "address": "preprod-node.play.dev.cardano.org", "port": 3001 }
  ],
  "localRoots": [
    { "accessPoints": [], "advertise": false, "trustable": false, "valency": 1 }
  ],
  "publicRoots": [
    {
      "accessPoints": [
        { "address": "preprod-node.play.dev.cardano.org", "port": 3001 }
      ],
      "advertise": false
    }
  ],
  "useLedgerAfterSlot": 76723200
}
```

Run the node:

```bash
dugite-node run \
  --config config-preprod.json \
  --topology topology-preprod.json \
  --database-path ./db-preprod \
  --socket-path ./node-preprod.sock \
  --host-addr 0.0.0.0 \
  --port 3001
```

## Official Configuration Files

Official configuration and topology files for each network are maintained in the Cardano Operations Book:

- **Preview:** [book.world.dev.cardano.org/environments/preview/](https://book.world.dev.cardano.org/environments/preview/)
- **Preprod:** [book.world.dev.cardano.org/environments/preprod/](https://book.world.dev.cardano.org/environments/preprod/)
- **Mainnet:** [book.world.dev.cardano.org/environments/mainnet/](https://book.world.dev.cardano.org/environments/mainnet/)

These include the full genesis files (Byron, Shelley, Alonzo, Conway) required for complete protocol parameter initialization.

## Using the CLI with Different Networks

When querying a node connected to a testnet, pass the `--testnet-magic` flag to the CLI:

```bash
# Preview
dugite-cli query tip --socket-path ./node-preview.sock --testnet-magic 2

# Preprod
dugite-cli query tip --socket-path ./node-preprod.sock --testnet-magic 1

# Mainnet (default, --testnet-magic not needed)
dugite-cli query tip --socket-path ./node-mainnet.sock
```

## Multiple Nodes

You can run multiple Dugite instances on the same machine, but every one of the
four per-node resources has to be distinct: the N2N port, the N2C socket path,
the database directory, and the Prometheus metrics port.

```bash
# Preview on port 3001
dugite-node run --port 3001 --database-path ./db-preview \
  --socket-path ./preview.sock --metrics-port 12796 ...

# Preprod on port 3002
dugite-node run --port 3002 --database-path ./db-preprod \
  --socket-path ./preprod.sock --metrics-port 12799 ...
```

The metrics port is the one that is easy to miss, because it does not appear on
the command line unless you put it there. Dugite's default is **12796**
(deliberately offset from cardano-node's 12798 so the two can coexist), so two
Dugite nodes started without `--metrics-port` or a `MetricsPort` config field
will collide on it. The shipped configs pre-assign distinct ports: mainnet 12800,
preview 12796, preprod 12799.

By default a metrics bind failure is logged and the node keeps running. Pass
`--require-metrics` to make it a fatal startup error instead, which is what you
want under a supervisor.

Each database directory is protected by an exclusive `flock` on `<db>/lock`, so
pointing two nodes at the same `--database-path` fails fast and names the pid
already holding it rather than corrupting the database.
