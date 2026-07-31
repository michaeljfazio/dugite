# Topology

The topology file defines the peers that the node connects to. Dugite supports the full cardano-node 10.x+ P2P topology format.

## Topology File Format

```json
{
  "bootstrapPeers": [
    { "address": "backbone.cardano.iog.io", "port": 3001 },
    { "address": "backbone.mainnet.cardanofoundation.org", "port": 3001 },
    { "address": "backbone.mainnet.emurgornd.com", "port": 3001 }
  ],
  "localRoots": [
    {
      "accessPoints": [
        { "address": "192.168.1.100", "port": 3001 }
      ],
      "advertise": false,
      "hotValency": 1,
      "warmValency": 2,
      "trustable": true
    }
  ],
  "publicRoots": [
    {
      "accessPoints": [
        { "address": "relays-new.cardano-mainnet.iohk.io", "port": 3001 }
      ],
      "advertise": false
    }
  ],
  "useLedgerAfterSlot": 0,
  "peerSnapshotFile": "peer-snapshot.json"
}
```

## Peer Categories

### Bootstrap Peers

Trusted peers from founding organizations, used during initial sync. These are the first peers the node contacts when starting.

```json
"bootstrapPeers": [
  { "address": "backbone.cardano.iog.io", "port": 3001 }
]
```

Bootstrap peers are unconditionally `trustable` — there is no per-entry flag.
They are what satisfies the Honest-Availability-Assumption closure while the
node is syncing, so a topology with none configured has to fall back on
`trustable` local roots instead.

Set to `null` or an empty array to disable bootstrap peers:

```json
"bootstrapPeers": null
```

### Local Roots

Peers the node should always maintain connections with. Typically used for:
- Your block producer (if running a relay)
- Peer arrangements with other stake pool operators
- Trusted relay nodes you operate

```json
"localRoots": [
  {
    "accessPoints": [
      { "address": "192.168.1.100", "port": 3001 }
    ],
    "advertise": true,
    "hotValency": 2,
    "warmValency": 3,
    "trustable": true,
    "behindFirewall": false,
    "diffusionMode": "InitiatorAndResponder"
  }
]
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `accessPoints` | array | required | List of `{address, port}` entries |
| `advertise` | boolean | `false` | Whether to share these peers via peer sharing protocol |
| `valency` | integer | 1 | *Deprecated.* Target number of active connections. Use `hotValency` instead |
| `hotValency` | integer | `valency` | Target number of hot (actively syncing) peers. Takes precedence over `valency` when both are set |
| `warmValency` | integer | `hotValency + 1` | Target number of warm (connected, not syncing) peers |
| `trustable` | boolean | `false` | Whether these peers are trusted for sync. Trusted peers are preferred during initial sync, and the node disconnects from non-trusted peers when syncing from outdated state. Also accepted as `trust_able` |
| `behindFirewall` | boolean | `false` | If `true`, the node waits for inbound connections from these peers instead of connecting outbound |
| `diffusionMode` | string | `"InitiatorAndResponder"` | Per-group diffusion mode. `"InitiatorOnly"` for unidirectional connections |

Dugite's peer governor drives root-peer connectivity from these per-group
valencies, not from the aggregate `TargetNumberOfRootPeers` config field — so
`hotValency` / `warmValency` are the levers that actually change behaviour here.

### Public Roots

Publicly known nodes (e.g., IOG relays) serving as fallback peers before the node has synced to the `useLedgerAfterSlot` threshold.

```json
"publicRoots": [
  {
    "accessPoints": [
      { "address": "relays-new.cardano-mainnet.iohk.io", "port": 3001 }
    ],
    "advertise": false
  }
]
```

### Ledger-Based Peer Discovery

After the node syncs past the `useLedgerAfterSlot` slot, it discovers peers from stake pool registrations in the ledger state. This provides decentralized peer discovery without relying on centralized relay lists.

```json
"useLedgerAfterSlot": 177724800
```

Set to a negative value or omit to disable ledger peer discovery. `0` enables it
immediately — which is what the shipped `config/mainnet/topology.json` uses.

### Peer Snapshot File

Optional path to a big ledger peer snapshot, used to seed the big-ledger-peer
candidate pool at startup before the live ledger has caught up far enough for
`useLedgerAfterSlot` discovery to populate it:

```json
"peerSnapshotFile": "peer-snapshot.json"
```

The path is resolved relative to the **topology file's** directory (matching
cardano-node), not the config file's. Two shapes are accepted: the IOG
cardano-node 10.x format with a `bigLedgerPools` array of `{relays: [{address,
port}]}`, and a legacy flat array of `{addr, port}` objects. Entries from either
shape are treated as big ledger peers. Hostnames are resolved once, at startup.

### Legacy Producers Format

The pre-P2P `producers` list is still parsed, for older topology files:

```json
"producers": [
  { "addr": "relay.example.com", "port": 3001, "valency": 1 }
]
```

Legacy producers are registered as untrusted, non-advertised peers. Prefer
`localRoots` / `publicRoots` for anything new.

## Example Topologies

These are the topology files shipped in the repository under
`config/<network>/topology.json`.

### Preview Testnet Relay

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

### Preprod Testnet Relay

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

### Mainnet Relay

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
  "useLedgerAfterSlot": 0,
  "peerSnapshotFile": "peer-snapshot.json"
}
```

### Relay with Block Producer

A relay node that maintains a connection to your block producer:

```json
{
  "bootstrapPeers": [
    { "address": "backbone.cardano.iog.io", "port": 3001 },
    { "address": "backbone.mainnet.cardanofoundation.org", "port": 3001 }
  ],
  "localRoots": [
    {
      "accessPoints": [
        { "address": "10.0.0.10", "port": 3001 }
      ],
      "advertise": false,
      "hotValency": 1,
      "warmValency": 2,
      "trustable": true,
      "behindFirewall": true
    }
  ],
  "publicRoots": [
    { "accessPoints": [], "advertise": false }
  ],
  "useLedgerAfterSlot": 177724800
}
```

## DNS SRV Resolution

When a hostname is specified in any `accessPoints` entry, Dugite queries DNS for SRV records at `_cardano._tcp.<host>` before falling back to A/AAAA lookup — matching the behaviour of the Haskell cardano-node. SRV records carry port, priority, and weight fields (RFC 2782); Dugite honours priority ordering and performs a weighted shuffle within equal-priority groups.

If no SRV records exist (NXDOMAIN or empty answer), Dugite falls back to a direct A/AAAA lookup using the port specified in the topology entry.

IPv4 and IPv6 addresses are both accepted; Dugite resolves A and AAAA records concurrently.

## SIGHUP Topology Reload

Dugite supports live topology reloading. Send a `SIGHUP` signal to the running node process, and it will re-read the topology file and update the peer manager with the new configuration:

```bash
kill -HUP $(pgrep -x dugite-node)
```

This allows you to add or remove peers without restarting the node.

The same signal also re-reads the node **config** file. Peer targets, churn
intervals, and log verbosity are applied live; everything else is logged as
needing a restart. See [Live Reload](./configuration.md#live-reload-sighup).

If you use `dugite-config edit`, `Ctrl+R` saves and sends this signal for you.
