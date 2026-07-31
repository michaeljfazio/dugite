# Node Commands

The `dugite-cli node` subcommands manage cold keys, KES keys, VRF keys, and operational certificates for block producer setup.

## key-gen

Generate a cold key pair and an operational certificate issue counter:

```bash
dugite-cli node key-gen \
  --cold-verification-key-file cold.vkey \
  --cold-signing-key-file cold.skey \
  --operational-certificate-issue-counter-file opcert.counter
```

| Flag | Required | Description |
|------|----------|-------------|
| `--cold-verification-key-file` | Yes | Output path for the cold verification key |
| `--cold-signing-key-file` | Yes | Output path for the cold signing key |
| `--operational-certificate-issue-counter-file` | Yes | Output path for the opcert issue counter. Canonical cardano-cli spelling; `--operational-certificate-issue-counter` and the legacy `--operational-certificate-counter-file` are accepted as aliases |
| `--key-output-bech32` | No | Write keys as bech32 instead of a text envelope (cardano-cli), instead of `--key-output-text-envelope` |
| `--key-output-text-envelope` | No | Write keys as a text envelope (default; cardano-cli) |
| `--key-output-format` | No | Deprecated cardano-cli spelling: `text-envelope` or `bech32`. Superseded by the two flags above but still accepted |

The cold key identifies your stake pool. Keep the signing key offline (air-gapped) after initial setup.

## key-gen-KES

Generate a KES (Key Evolving Signature) key pair. The canonical cardano-cli
subcommand casing is `key-gen-KES` (cardano-cli itself rejects the lowercase
form); dugite also accepts `key-gen-kes` as a backward-compatible alias.

```bash
dugite-cli node key-gen-KES \
  --verification-key-file kes.vkey \
  --signing-key-file kes.skey
```

| Flag | Required | Description |
|------|----------|-------------|
| `--verification-key-file` | Yes | Output path for the KES verification key |
| `--signing-key-file` | Yes | Output path for the KES signing key |
| `--key-output-bech32` | No | Write keys as bech32 instead of a text envelope |
| `--key-output-text-envelope` | No | Write keys as a text envelope (default) |
| `--key-output-format` | No | Deprecated: `text-envelope` or `bech32` |

KES keys are rotated periodically. Each key is valid for a limited number of KES periods (62 periods on mainnet, approximately 90 days total).

## key-gen-VRF

Generate a VRF (Verifiable Random Function) key pair. Canonical casing is
`key-gen-VRF`; `key-gen-vrf` is accepted as an alias.

```bash
dugite-cli node key-gen-VRF \
  --verification-key-file vrf.vkey \
  --signing-key-file vrf.skey
```

| Flag | Required | Description |
|------|----------|-------------|
| `--verification-key-file` | Yes | Output path for the VRF verification key |
| `--signing-key-file` | Yes | Output path for the VRF signing key |
| `--key-output-bech32` | No | Write keys as bech32 instead of a text envelope |
| `--key-output-text-envelope` | No | Write keys as a text envelope (default) |
| `--key-output-format` | No | Deprecated: `text-envelope` or `bech32` |

VRF keys are used for slot leader election and do not need rotation.

## key-hash-VRF

Get the Blake2b-256 hash of a VRF verification key. Canonical casing is
`key-hash-VRF`; `key-hash-vrf` is accepted as an alias.

```bash
dugite-cli node key-hash-VRF \
  --verification-key-file vrf.vkey
```

| Flag | Required | Description |
|------|----------|-------------|
| `--verification-key-file` | One of these two | Path to the VRF verification key file |
| `--verification-key` | One of these two | VRF verification key as an inline bech32 or hex STRING (cardano-cli alternative to the `-file` form) |
| `--out-file` | No | Write the hash to a file instead of stdout |

Note this hash uses Blake2b-256 (32 bytes) — a different convention from
`key verification-key-hash`, which is Blake2b-224 (28 bytes) and rejects
VRF/KES key types outright.

## issue-op-cert

Issue an operational certificate binding the cold key to the current KES key:

```bash
dugite-cli node issue-op-cert \
  --kes-verification-key-file kes.vkey \
  --cold-signing-key-file cold.skey \
  --operational-certificate-issue-counter-file opcert.counter \
  --kes-period 400 \
  --out-file opcert.cert
```

| Flag | Required | Description |
|------|----------|-------------|
| `--kes-verification-key-file` | Yes | Path to the KES verification key |
| `--cold-signing-key-file` | Yes | Path to the cold signing key |
| `--operational-certificate-issue-counter-file` | Yes | Path to the opcert issue counter (incremented automatically). `--operational-certificate-issue-counter` and `--operational-certificate-counter-file` are accepted as aliases |
| `--kes-period` | Yes | Current KES period (`current_slot / slots_per_kes_period`) |
| `--out-file` | Yes | Output path for the operational certificate |

The opcert must be regenerated each time you rotate KES keys. The counter file is incremented each time to prevent replay attacks.

## new-counter

Create a new operational certificate issue counter (useful if the original counter is lost):

```bash
dugite-cli node new-counter \
  --cold-verification-key-file cold.vkey \
  --counter-value 5 \
  --operational-certificate-issue-counter-file opcert.counter
```

| Flag | Required | Description |
|------|----------|-------------|
| `--cold-verification-key-file` | Yes | Path to the cold verification key |
| `--counter-value` | Yes | Counter value to set |
| `--operational-certificate-issue-counter-file` | Yes | Output path for the counter file. `--operational-certificate-issue-counter` and `--operational-certificate-counter-file` are accepted as aliases |
