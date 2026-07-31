# Key Generation

Dugite CLI supports generating all key types needed for Cardano operations.

> **The `key` group is a dugite extension.** `key generate-payment-key`,
> `key generate-stake-key`, and `key verification-key-hash` have no
> cardano-cli counterpart — they are additive convenience commands. The
> cardano-cli equivalents are also implemented and produce compatible
> output:
>
> | dugite extension | cardano-cli equivalent |
> |---|---|
> | `key generate-payment-key` | `address key-gen` |
> | `key generate-stake-key` | `stake-address key-gen` |
> | `key verification-key-hash` | `address key-hash` / `stake-address key-hash` |
>
> Scripts written against cardano-cli never need the `key` group; it exists
> because it is convenient and already in use.

## Payment Keys

Generate an Ed25519 key pair for payments:

```bash
dugite-cli key generate-payment-key \
  --signing-key-file payment.skey \
  --verification-key-file payment.vkey
```

Output files:
- `payment.skey` — Payment signing key (keep secret)
- `payment.vkey` — Payment verification key (safe to share)

The cardano-cli-compatible equivalent is `dugite-cli address key-gen
--verification-key-file payment.vkey --signing-key-file payment.skey`.

## Stake Keys

Generate an Ed25519 key pair for staking:

```bash
dugite-cli key generate-stake-key \
  --signing-key-file stake.skey \
  --verification-key-file stake.vkey
```

Output files:
- `stake.skey` — Stake signing key
- `stake.vkey` — Stake verification key

The cardano-cli-compatible equivalent is `dugite-cli stake-address key-gen
--verification-key-file stake.vkey --signing-key-file stake.skey` (see
[Stake Address Commands](stake-address.md)).

## Verification Key Hash

Compute the Blake2b-224 hash of any verification key:

```bash
dugite-cli key verification-key-hash \
  --verification-key-file payment.vkey
```

This outputs the 28-byte key hash in hexadecimal, used in addresses and certificates.

Only Ed25519 verification-key envelope types are accepted (payment, stake,
stake pool, genesis, genesis-delegate, genesis-UTxO, DRep, and CC cold/hot
verification keys). Signing keys and KES/VRF verification keys are rejected
with an error naming the offending envelope `type` — VRF key hashes use a
different convention and are computed with `node key-hash-VRF` instead (see
[Node Commands](node-commands.md)).

## DRep Keys

Generate keys for a Delegated Representative (Conway governance):

```bash
dugite-cli governance drep key-gen \
  --signing-key-file drep.skey \
  --verification-key-file drep.vkey
```

Get the DRep ID:

```bash
# Bech32 format (default)
dugite-cli governance drep id \
  --drep-verification-key-file drep.vkey

# Hex format
dugite-cli governance drep id \
  --drep-verification-key-file drep.vkey \
  --output-format hex
```

## Node Keys

See [Node Commands](node-commands.md) for the full flag reference, including
`--key-output-bech32` / `--key-output-text-envelope` and the canonical
`--operational-certificate-issue-counter-file` spelling. The short version:

### Cold Keys

Generate cold keys and an operational certificate issue counter:

```bash
dugite-cli node key-gen \
  --cold-verification-key-file cold.vkey \
  --cold-signing-key-file cold.skey \
  --operational-certificate-issue-counter-file opcert.counter
```

`--operational-certificate-issue-counter-file` is the cardano-cli-canonical
spelling; `--operational-certificate-issue-counter` and
`--operational-certificate-counter-file` are accepted as aliases.

### KES Keys

Generate Key Evolving Signature keys (rotated periodically). The canonical
cardano-cli subcommand casing is `key-gen-KES` (cardano-cli rejects
lowercase); dugite additionally accepts `key-gen-kes` as a backward-compatible
alias:

```bash
dugite-cli node key-gen-KES \
  --verification-key-file kes.vkey \
  --signing-key-file kes.skey
```

### VRF Keys

Generate Verifiable Random Function keys (for slot leader election). Canonical
casing is `key-gen-VRF`, with `key-gen-vrf` accepted as an alias:

```bash
dugite-cli node key-gen-VRF \
  --verification-key-file vrf.vkey \
  --signing-key-file vrf.skey
```

### Operational Certificate

Issue an operational certificate binding the cold key to the current KES key:

```bash
dugite-cli node issue-op-cert \
  --kes-verification-key-file kes.vkey \
  --cold-signing-key-file cold.skey \
  --operational-certificate-issue-counter-file opcert.counter \
  --kes-period 400 \
  --out-file opcert.cert
```

## Address Generation

### Payment Address

Build a payment address from keys:

```bash
# Enterprise address (no staking)
dugite-cli address build \
  --payment-verification-key-file payment.vkey \
  --testnet-magic 2

# Base address (with staking)
dugite-cli address build \
  --payment-verification-key-file payment.vkey \
  --stake-verification-key-file stake.vkey \
  --testnet-magic 2

# Mainnet address
dugite-cli address build \
  --payment-verification-key-file payment.vkey \
  --stake-verification-key-file stake.vkey \
  --mainnet
```

`--mainnet` and `--testnet-magic <NATURAL>` are mutually exclusive
(cardano-cli compatible). Instead of a key file, the payment and stake keys
can be passed inline as a bech32 or hex string:

```bash
dugite-cli address build \
  --payment-verification-key "addr_vk1..." \
  --stake-verification-key "stake_vk1..." \
  --mainnet
```

dugite additionally accepts a `--network mainnet|testnet` flag (its own
extension, predating `--mainnet`/`--testnet-magic`) and, when none of
`--mainnet`, `--testnet-magic`, or `--network` is given, falls back to the
`CARDANO_NODE_NETWORK_ID` environment variable (`mainnet` or a magic number),
matching cardano-cli. Resolution order: explicit flags, then `--network`,
then `CARDANO_NODE_NETWORK_ID`, then mainnet. An unrecognized value from any
of these sources is a hard error naming the accepted forms — it never falls
back to testnet silently.

## Key File Format

All keys are stored in the cardano-node text envelope format:

```json
{
  "type": "PaymentSigningKeyShelley_ed25519",
  "description": "Payment Signing Key",
  "cborHex": "5820a1b2c3d4..."
}
```

The `cborHex` field contains the CBOR-encoded key bytes. The type field identifies the key type and is used for validation when loading keys.

Key files generated by Dugite are compatible with cardano-cli and vice versa.

## Complete Workflow Example

Generate all keys needed for a basic wallet:

```bash
# 1. Generate payment keys
dugite-cli key generate-payment-key \
  --signing-key-file payment.skey \
  --verification-key-file payment.vkey

# 2. Generate stake keys
dugite-cli key generate-stake-key \
  --signing-key-file stake.skey \
  --verification-key-file stake.vkey

# 3. Build a testnet address
dugite-cli address build \
  --payment-verification-key-file payment.vkey \
  --stake-verification-key-file stake.vkey \
  --testnet-magic 2

# 4. Get the payment key hash
dugite-cli key verification-key-hash \
  --verification-key-file payment.vkey
```
