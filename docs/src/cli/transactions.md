# Transactions

Dugite CLI supports the full transaction lifecycle: building, signing, submitting, and inspecting transactions.

## Building a Transaction

```bash
dugite-cli transaction build \
  --tx-in <tx_hash>#<index> \
  --tx-out <address>+<lovelace> \
  --change-address <address> \
  --fee <lovelace> \
  --out-file tx.body
```

`transaction build-raw` accepts the identical flag set and produces
identical output — it exists only so scripts that call `cardano-cli
transaction build-raw` work unchanged.

### Arguments

| Argument | Description |
|----------|-------------|
| `--tx-in` | Transaction input in `tx_hash#index` format. Can be specified multiple times |
| `--tx-out` | Transaction output in `address+lovelace` format. Can be specified multiple times |
| `--change-address` | Address to receive change (required for auto-balance mode) |
| `--fee` | Fee in lovelace. If omitted and `--socket-path` is set, the fee is computed automatically (auto-balance mode, see below); if omitted without `--socket-path`, defaults to 200000 |
| `--ttl` | Time-to-live slot number (optional) |
| `--certificate-file` | Path to a certificate file to include (can be repeated) |
| `--withdrawal` | Withdrawal in `stake_address+lovelace` format (can be repeated) |
| `--metadata-json-file` | Path to a JSON metadata file (optional) |
| `--out-file` | Output file for the transaction body |

#### Plutus and Conway flags

| Argument | Description |
|----------|-------------|
| `--tx-in-script-file` | Plutus script (text envelope, `PlutusScriptV1/V2/V3`) to attach to the most recently specified `--tx-in`. The Nth occurrence pairs with the Nth `--tx-in` by declaration order |
| `--tx-in-datum-file` | Datum JSON file (cardano-cli PlutusData schema) for the script-bearing input at the same position |
| `--tx-in-redeemer-file` | Redeemer JSON file (same schema) for the script-bearing input at the same position |
| `--tx-in-execution-units` | Execution units budget for the script-bearing input, format `mem,steps` |
| `--tx-in-collateral` | Collateral input for Plutus scripts, format `tx_hash#index` (can be repeated) |
| `--required-signer-hash` | Required signer key hash, hex (can be repeated) |
| `--mint` | Mint/burn tokens, format `policy_id.asset_name+quantity` or `...-quantity` to burn (can be repeated) |
| `--read-only-tx-in-reference` | Reference input visible to Plutus scripts but not consumed (CIP-31), format `tx_hash#index` (can be repeated) |
| `--tx-out-inline-datum-value` | Inline datum for a transaction output (CIP-32), format `INDEX:JSON` or bare `JSON` (defaults to output 0) |
| `--tx-out-inline-datum-file` | Inline datum file for a transaction output (CIP-32), format `INDEX:FILE` or bare `FILE` |
| `--tx-out-reference-script-file` | Reference script for a transaction output (CIP-33), format `INDEX:FILE` or bare `FILE` |
| `--vote-file` | Vote file to include (Conway governance, can be repeated) |
| `--proposal-file` | Governance proposal file to include (Conway governance, can be repeated) |
| `--calculate-plutus-script-cost` | Evaluate Plutus script execution costs and write the result to a JSON file. Requires `--socket-path` to evaluate against live ledger state |

#### Auto-balance mode

When `--socket-path` is provided and `--fee` is **not** explicitly set,
`transaction build` connects to the node, queries UTxO values for the given
inputs and the current protocol parameters, computes the fee automatically,
derives a change output at `--change-address`, and writes a balanced
transaction — matching `cardano-cli transaction build`'s behavior (as
opposed to `build-raw`'s fully manual fee/change accounting):

```bash
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out "addr_test1qz...+5000000" \
  --change-address "addr_test1qp..." \
  --socket-path ./node.sock \
  --testnet-magic 2 \
  --out-file tx.body
```

| Argument | Description |
|----------|-------------|
| `--socket-path` | Path to the node's Unix domain socket. Enables auto-balance mode when `--fee` is omitted |
| `--mainnet` | Use mainnet (network magic 764824073) |
| `--testnet-magic` | Testnet network magic (e.g. 2 for preview, 1 for preprod) |

### Example: Simple ADA Transfer

```bash
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out "addr_test1qz...+5000000" \
  --change-address "addr_test1qp..." \
  --fee 200000 \
  --ttl 50000000 \
  --out-file tx.body
```

### Multi-Asset Outputs

To include native tokens in an output, use the extended format:

```
address+lovelace+"policy_id.asset_name quantity"
```

Example:

```bash
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out 'addr_test1qz...+2000000+"a1b2c3...d4e5f6.4d79546f6b656e 100"' \
  --change-address "addr_test1qp..." \
  --fee 200000 \
  --out-file tx.body
```

Multiple tokens can be separated with `+` inside the quoted string:

```
"policy1.asset1 100+policy2.asset2 50"
```

### Including Certificates

```bash
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out "addr_test1qz...+5000000" \
  --change-address "addr_test1qp..." \
  --fee 200000 \
  --certificate-file stake-reg.cert \
  --certificate-file stake-deleg.cert \
  --out-file tx.body
```

### Including Metadata

Create a metadata JSON file with integer keys:

```json
{
  "674": {
    "msg": ["Hello, Cardano!"]
  }
}
```

```bash
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out "addr_test1qz...+5000000" \
  --change-address "addr_test1qp..." \
  --fee 200000 \
  --metadata-json-file metadata.json \
  --out-file tx.body
```

## Signing a Transaction

```bash
dugite-cli transaction sign \
  --tx-body-file tx.body \
  --signing-key-file payment.skey \
  --out-file tx.signed
```

Multiple signing keys can be provided:

```bash
dugite-cli transaction sign \
  --tx-body-file tx.body \
  --signing-key-file payment.skey \
  --signing-key-file stake.skey \
  --out-file tx.signed
```

## Submitting a Transaction

```bash
dugite-cli transaction submit \
  --tx-file tx.signed \
  --socket-path ./node.sock
```

The node validates the transaction (Phase-1 and Phase-2 for Plutus transactions) and, if valid, adds it to the mempool for propagation.

## Viewing a Transaction

```bash
dugite-cli transaction view --tx-file tx.signed
```

Output includes:
- Transaction type
- CBOR size
- Transaction hash
- Number of inputs and outputs
- Fee
- TTL (if set)

## Transaction ID

Compute the transaction hash:

```bash
dugite-cli transaction txid --tx-file tx.body
```

Works with both transaction body files and signed transaction files.

## Calculate Minimum Fee

```bash
dugite-cli transaction calculate-min-fee \
  --tx-body-file tx.body \
  --witness-count 2 \
  --protocol-params-file protocol-params.json
```

The fee calculation accounts for:

- Base fee: `txFeeFixed + txFeePerByte * tx_size`
- Script execution: `executionUnitPrices * total_ExUnits` for any Plutus witnesses
- Reference script surcharge: CIP-0112 tiered fee for reference scripts (25KiB tiers, 1.2x multiplier per tier)

To get the current protocol parameters:

```bash
dugite-cli query protocol-parameters \
  --socket-path ./node.sock \
  --out-file protocol-params.json
```

## Calculate Minimum Required UTxO

Compute the minimum lovelace required for a transaction output to satisfy the `minUTxOValue` protocol parameter:

```bash
dugite-cli transaction calculate-min-required-utxo \
  --protocol-params-file protocol-params.json \
  --tx-out "addr_test1qz...+0+\"policy1.asset1 100\""
```

Output:

```
Minimum required lovelace: 1724100
```

This is particularly useful when constructing outputs that carry native tokens, since the minimum lovelace depends on the byte-size of the value bundle (number of policy IDs, asset names, and quantities).

## Creating Witnesses

For multi-signature workflows, you can create witnesses separately and assemble them:

### Create a Witness

```bash
dugite-cli transaction witness \
  --tx-body-file tx.body \
  --signing-key-file payment.skey \
  --out-file payment.witness
```

### Assemble a Transaction

```bash
dugite-cli transaction assemble \
  --tx-body-file tx.body \
  --witness-file payment.witness \
  --witness-file stake.witness \
  --out-file tx.signed
```

## Policy ID

Compute the policy ID (Blake2b-224 hash) of a native script:

```bash
dugite-cli transaction policyid --script-file policy.script
```

## Hash Script Data

Compute the script-data hash (datum + redeemers + language views) used in a
transaction's `scriptDataHash` field:

```bash
dugite-cli transaction hash-script-data \
  --datum-file datum.json \
  --redeemer-file redeemer.json
```

| Argument | Description |
|----------|-------------|
| `--datum-file` | Datum JSON file (optional) |
| `--redeemer-file` | Redeemer JSON file (optional) |
| `--script-data-file` | Script data JSON file (optional) |

## Complete Workflow

```bash
# 1. Query UTxOs to find inputs
dugite-cli query utxo \
  --address addr_test1qz... \
  --socket-path ./node.sock \
  --testnet-magic 2

# 2. Get protocol parameters for fee calculation
dugite-cli query protocol-parameters \
  --socket-path ./node.sock \
  --testnet-magic 2 \
  --out-file pp.json

# 3. Build the transaction
dugite-cli transaction build \
  --tx-in "abc123...#0" \
  --tx-out "addr_test1qr...+5000000" \
  --change-address "addr_test1qz..." \
  --fee 200000 \
  --out-file tx.body

# 4. Calculate the exact fee
dugite-cli transaction calculate-min-fee \
  --tx-body-file tx.body \
  --witness-count 1 \
  --protocol-params-file pp.json

# 5. Rebuild with the correct fee (repeat step 3 with updated --fee)

# 6. Sign
dugite-cli transaction sign \
  --tx-body-file tx.body \
  --signing-key-file payment.skey \
  --out-file tx.signed

# 7. Submit
dugite-cli transaction submit \
  --tx-file tx.signed \
  --socket-path ./node.sock
```
