# Queries

Dugite CLI provides a comprehensive set of queries against a running node via the N2C (Node-to-Client) protocol over a Unix domain socket.

## Chain Tip

Query the current chain tip:

```bash
dugite-cli query tip --socket-path ./node.sock
```

For testnets:

```bash
dugite-cli query tip --socket-path ./node.sock --testnet-magic 2
```

Output:

```json
{
    "slot": 73429851,
    "hash": "a1b2c3d4e5f6...",
    "block": 2847392,
    "epoch": 170,
    "era": "Conway",
    "syncProgress": "99.87"
}
```

## UTxO Query

Query UTxOs at a specific address:

```bash
dugite-cli query utxo \
  --address addr_test1qz... \
  --socket-path ./node.sock \
  --testnet-magic 2
```

Alternatively, query one or more specific UTxOs by transaction input
reference (`--tx-in` may be repeated), or dump the entire UTxO set with
`--whole` (warning: very large on mainnet):

```bash
dugite-cli query utxo \
  --tx-in "abc123...#0" \
  --tx-in "def456...#1" \
  --socket-path ./node.sock \
  --testnet-magic 2

dugite-cli query utxo --whole --socket-path ./node.sock
```

Exactly one of `--address`, `--tx-in`, or `--whole` must be provided.

Output:

```
TxHash#Ix                                                            Datum           Lovelace
------------------------------------------------------------------------------------------------
a1b2c3d4...#0                                                           no            5000000
e5f6a7b8...#1                                                          yes           10000000

Total UTxOs: 2
```

## Protocol Parameters

Query current protocol parameters:

```bash
# Print to stdout
dugite-cli query protocol-parameters \
  --socket-path ./node.sock

# Save to file
dugite-cli query protocol-parameters \
  --socket-path ./node.sock \
  --out-file protocol-params.json
```

The output is a JSON object containing all active protocol parameters, including fee settings, execution unit limits, and governance thresholds.

## Stake Distribution

Query the stake distribution across all registered pools:

```bash
dugite-cli query stake-distribution \
  --socket-path ./node.sock
```

Output is JSON by default (matching cardano-cli), keyed by pool ID hex with
each pool's stake expressed as an exact `num/den` fraction of total active
stake; pass `--output-text` for the human-readable table below, or
`--out-file` to write to a file instead of stdout.

JSON output (default):

```json
{
  "<pool_id_hex>": {
    "poolId": "<pool_id_hex>",
    "stakeFraction": "1/8523"
  }
}
```

`--output-text` output:

```
PoolId                                                       Stake Fraction
--------------------------------------------------------------------------------
<pool_id_hex>                                                    0.0001173413

Total pools: 3200
```

## Stake Address Info

Query delegation and rewards for a stake address:

```bash
dugite-cli query stake-address-info \
  --address stake_test1uz... \
  --socket-path ./node.sock \
  --testnet-magic 2
```

Output:

```json
[
  {
    "address": "stake_test1uz...",
    "delegation": "pool1abc...",
    "rewardAccountBalance": 5234000
  }
]
```

## Stake Pools

List the IDs of all registered stake pools (use [Pool Parameters](#pool-parameters)
below for the pledge/cost/margin of a specific pool):

```bash
dugite-cli query stake-pools \
  --socket-path ./node.sock
```

Output is a JSON array of bech32 pool IDs by default (matching cardano-cli),
sorted by raw hash bytes; `--output-text` prints the same IDs
newline-separated instead. `--out-file` writes to a file instead of stdout.
`--output-yaml` is accepted by the flag parser but not yet implemented — it
currently errors at runtime.

JSON output (default):

```json
[
    "pool1abc...",
    "pool1def..."
]
```

## Pool Parameters

Query detailed parameters for a specific pool:

```bash
dugite-cli query pool-params \
  --socket-path ./node.sock \
  --stake-pool-id pool1abc...
```

## Non-Myopic Member Rewards

Query expected (non-myopic) member rewards for hypothetical delegator
stakes, matching `cardano-cli query non-myopic-member-rewards`. Returns the
expected lovelace reward for each requested stake amount against every
registered pool, assuming ideal performance — used as input to pool-ranking
tools:

```bash
dugite-cli query non-myopic-member-rewards \
  --socket-path ./node.sock \
  --stake 1000000000
```

`--stake` may be repeated for multiple hypothetical stake values (in
lovelace); it defaults to 1 ADA when omitted.

## Stake Snapshots

Query the mark/set/go stake snapshots:

```bash
dugite-cli query stake-snapshot \
  --socket-path ./node.sock

# Filter by pool
dugite-cli query stake-snapshot \
  --socket-path ./node.sock \
  --stake-pool-id pool1abc...
```

## Governance State (Conway)

Query the overall governance state:

```bash
dugite-cli query gov-state --socket-path ./node.sock
```

Output is JSON by default (matching cardano-cli); pass `--output-text` for
the human-readable summary shown below, or `--out-file` to write to a file.

Output:

```
Governance State (Conway)
========================
Treasury:         1234567890 ADA
Registered DReps: 456
Committee Members: 7
Active Proposals: 12

Proposals:
Type                 TxId     Yes     No  Abstain
----------------------------------------------------
InfoAction           a1b2c3#0    42     3        5
TreasuryWithdrawals  d4e5f6#1    28    12        8
```

## DRep State (Conway)

Query registered DReps. Exactly one DRep selector is required — there is no
bare "all DReps" default:

```bash
# All DReps
dugite-cli query drep-state --all-dreps --socket-path ./node.sock

# Specific DRep by key hash
dugite-cli query drep-state \
  --socket-path ./node.sock \
  --drep-key-hash a1b2c3d4...
```

| Flag | Description |
|------|-------------|
| `--all-dreps` | Query for all DReps |
| `--drep-key-hash` | Filter by DRep key hash (28-byte blake2b-224 hex) |
| `--drep-script-hash` | Filter by DRep script hash (28-byte hex) |
| `--drep-verification-key` | Derive the DRep key hash from a verification key hex string |
| `--drep-verification-key-file` | Derive the DRep key hash from a verification key text-envelope file |
| `--include-stake` | Include each DRep's delegated stake in the response |
| `--output-json` | Format output as JSON (the cardano-cli default) |
| `--output-yaml` | Format output as YAML (mutually exclusive with `--output-json`) |
| `--out-file` | Optional output file. Default is stdout |

These selector flags are mutually exclusive — pass exactly one.

Output:

```
DRep State (Conway)
===================
Total DReps: 456

Credential Hash                                                    Deposit (ADA)    Epoch
--------------------------------------------------------------------------------------------
a1b2c3d4...                                                                500      412
  Anchor: https://example.com/drep-metadata.json
```

## Committee State (Conway)

Query the constitutional committee:

```bash
dugite-cli query committee-state --socket-path ./node.sock
```

Output:

```
Constitutional Committee State (Conway)
=======================================
Active Members: 7
Resigned Members: 1

Cold Credential                                                    Hot Credential
--------------------------------------------------------------------------------------------------------------------------------------
a1b2c3d4...                                                        e5f6a7b8...

Resigned:
  d4e5f6a7...
```

## Transaction Mempool

Query the node's transaction mempool:

```bash
# Mempool info (size, capacity, tx count)
dugite-cli query tx-mempool info --socket-path ./node.sock

# Check if a specific transaction is in the mempool
dugite-cli query tx-mempool has-tx \
  --socket-path ./node.sock \
  --tx-id a1b2c3d4...
```

Info output:

```
Mempool snapshot at slot 73429851:
  Capacity:     2000000 bytes
  Size:         45320 bytes
  Transactions: 12
```

## Treasury

Query the treasury balance (matches `cardano-cli query treasury`, which
reports treasury only — not reserves):

```bash
dugite-cli query treasury --socket-path ./node.sock
```

The default output is the bare lovelace integer (cardano-cli-compatible):

```
1234567890000000
```

`--output-text` prints a human-readable summary that includes both treasury
and reserves:

```
Account State
=============
Treasury: 1234567890000000 lovelace (1234567 ADA)
Reserves: 9876543210000000 lovelace (9876543 ADA)
```

`--out-file` writes the selected format to a file instead of stdout.

## Constitution (Conway)

Query the current constitution:

```bash
dugite-cli query constitution --socket-path ./node.sock
```

Output:

```
Constitution
============
URL:         https://constitution.gov/hash.json
Data Hash:   a1b2c3d4e5f6...
Script Hash: none
```

## Ratification State (Conway)

Query the ratification state (enacted/expired proposals from the most recent epoch transition):

```bash
dugite-cli query ratify-state --socket-path ./node.sock
```

Output:

```
Ratification State
==================
Enacted proposals: 1
  a1b2c3d4e5f6...#0
Expired proposals: 2
  d4e5f6a7b8c9...#1
  e5f6a7b8c9d0...#0
Delayed:           false
```

## Governance Proposals (Conway)

Query active governance action proposals, matching `cardano-cli query
proposals`:

```bash
# All live proposals
dugite-cli query proposals --socket-path ./node.sock

# Filter to a specific proposal
dugite-cli query proposals \
  --socket-path ./node.sock \
  --governance-action-tx-id a1b2c3d4... \
  --governance-action-index 0
```

| Flag | Description |
|------|-------------|
| `--all-proposals` | Return all proposals (default) |
| `--governance-action-tx-id` | Filter by governance action tx ID |
| `--governance-action-index` | Filter by governance action index (requires `--governance-action-tx-id`) |
| `--out-file` | Optional output file. Default is stdout |

## Slot Number

Convert a wall-clock time to a Cardano slot number:

```bash
dugite-cli query slot-number \
  --socket-path ./node.sock \
  --testnet-magic 2 \
  --utc-time "2026-03-20T12:00:00Z"
```

Output:

```
Slot: 73851200
```

This is useful for computing TTL values or verifying that a specific point in time falls within a given epoch.

## KES Period Info

Query KES period information for an operational certificate:

```bash
dugite-cli query kes-period-info \
  --socket-path ./node.sock \
  --op-cert-file opcert.cert
```

Unlike the other queries above, the default output here is the
human-readable text summary shown below; pass `--output-json` for JSON, or
`--out-file` to write to a file.

Output:

```
KES Period Info
===============
On-chain: yes
Operational certificate counter on-chain: 3
Certificate issue counter: 3

Current KES period: 418
Operational certificate start KES period: 418
KES max evolutions: 62
KES periods remaining: 62

Node start time: 2026-03-19T08:00:00Z
KES key expiry: 2026-09-14T08:00:00Z
```

Use this command to verify that a KES key is current and to determine when rotation is needed.

## Leadership Schedule

Compute the slots a stake pool is expected to mint a block in, matching
`cardano-cli query leadership-schedule`. This queries the running node for
live stake and epoch state (via `--socket-path`) rather than taking manual
stake/coefficient inputs:

```bash
dugite-cli query leadership-schedule \
  --socket-path ./node.sock \
  --testnet-magic 2 \
  --genesis config/preview/shelley-genesis.json \
  --stake-pool-id pool1abc... \
  --vrf-signing-key-file vrf.skey \
  --current
```

| Flag | Required | Description |
|------|----------|-------------|
| `--socket-path` | No (default `node.sock`) | Path to the node socket. Overrides `CARDANO_NODE_SOCKET_PATH` |
| `--mainnet` | No | Use the mainnet magic ID (mutually exclusive with `--testnet-magic`) |
| `--testnet-magic` | No | Testnet magic ID |
| `--genesis` | Yes | Shelley genesis file path |
| `--stake-pool-id` | No | Stake pool ID (hex-encoded hash) |
| `--cold-verification-key-file` | No | Path to the cold verification key file |
| `--vrf-signing-key-file` | Yes | Path to the VRF signing key |
| `--current` | No | Leadership schedule for the current epoch (mutually exclusive with `--next`) |
| `--next` | No | Leadership schedule for the following epoch |
| `--output-json` | No | Format output as JSON (default) |
| `--output-text` | No | Format output as text |
| `--out-file` | No | Optional output file. Default is stdout |

## Ledger State (Debug)

Dump the raw ledger state (debug endpoint):

```bash
dugite-cli query ledger-state \
  --socket-path ./node.sock \
  --out-file ledger-state.cbor
```

| Flag | Description |
|------|-------------|
| `--out-file` | Optional output file. Default is stdout |

## Protocol State (Debug)

Dump the raw protocol (consensus) state — includes the KES evolving nonce,
candidate nonce, and epoch nonce (debug endpoint):

```bash
# Raw CBOR hex (default)
dugite-cli query protocol-state --socket-path ./node.sock

# JSON, matching cardano-cli's --output-json
dugite-cli query protocol-state \
  --socket-path ./node.sock \
  --output-json
```

| Flag | Description |
|------|-------------|
| `--output-json` | Render the response as JSON (matches cardano-cli's `query protocol-state --output-json`). Without this flag, dugite emits the raw CBOR as hex |
| `--out-file` | Optional output file. Default is stdout |
