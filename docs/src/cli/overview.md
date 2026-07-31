# CLI Overview

Dugite provides `dugite-cli`, a cardano-cli compatible command-line interface for interacting with a running Dugite node and managing keys, transactions, and governance.

## Binary

```bash
dugite-cli [COMMAND] [OPTIONS]
```

## Command Groups

| Command | Description |
|---------|-------------|
| `address` | Address generation and manipulation |
| `key` | Payment and stake key generation (dugite extension — see below) |
| `transaction` | Transaction building, signing, and submission |
| `query` | Node queries (tip, UTxO, protocol parameters, etc.) |
| `stake-address` | Stake address registration, delegation, and vote delegation |
| `stake-pool` | Stake pool operations (key generation, registration, retirement certificates) |
| `governance` | Conway governance (DRep, voting, proposals) |
| `node` | Node key operations (cold keys, KES, VRF, operational certificates) |
| `byron` | Byron-era key conversion commands (`byron key ...`) |
| `genesis` | Genesis block/bundle commands (keys, delegation certs, `genesis create`) |
| `text-view` | Decode a text-envelope file's CBOR representation |

The `key` command group (`generate-payment-key`, `generate-stake-key`,
`verification-key-hash`) is a dugite-only convenience extension with no
cardano-cli counterpart. The cardano-cli equivalents — `address key-gen`,
`stake-address key-gen`, and `address key-hash` — are also implemented, so
scripts written against cardano-cli work unchanged. See
[Key Generation](key-generation.md) for the mapping.

### Era Prefixes

cardano-cli 11 accepts commands only in their era-prefixed form, e.g.
`cardano-cli conway stake-pool registration-certificate ...`. dugite accepts
**both** the era-prefixed form (`conway`, `babbage`, `alonzo`, `mary`,
`allegra`, `shelley`, `latest`) and the flat form (`dugite-cli stake-pool
registration-certificate ...`) — every era prefix routes to the same
handler, since dugite is era-agnostic at the CLI surface. This makes dugite a
strict superset: any cardano-cli-compatible script works unchanged, and
existing dugite scripts using the flat form keep working too.

## Common Patterns

### Socket Path

Most commands that interact with a running node require `--socket-path` to specify the Unix domain socket:

```bash
dugite-cli query tip --socket-path ./node.sock
```

The default socket path is `node.sock` in the current directory.

### Testnet Magic

When querying a node on a testnet, pass the `--testnet-magic` flag:

```bash
dugite-cli query tip --socket-path ./node.sock --testnet-magic 2
```

For mainnet, `--testnet-magic` is not needed (defaults to mainnet magic 764824073).

### Text Envelope Format

Keys, certificates, and transactions are stored in the cardano-node "text envelope" JSON format:

```json
{
  "type": "PaymentSigningKeyShelley_ed25519",
  "description": "Payment Signing Key",
  "cborHex": "5820..."
}
```

This format is interchangeable with files produced by `cardano-cli`.

### Output Files

Commands that produce artifacts use `--out-file`:

```bash
dugite-cli transaction build ... --out-file tx.body
dugite-cli transaction sign ... --out-file tx.signed
```

## Help

Every command supports `--help`:

```bash
dugite-cli --help
dugite-cli transaction --help
dugite-cli transaction build --help
```
