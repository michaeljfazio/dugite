# Hand-written Ledger Conformance Vectors

This directory holds a small set of hand-written JSON test vectors and the
Rust harness that runs them against Dugite's ledger code.

## What the vectors are

**The 58 vectors in `vectors/` are hand-written.** They are NOT generated from
the Agda formal ledger specification, and there is no Agda -> Haskell
generator: the `generator/` directory that earlier versions of this file
described does not exist, and nothing in this repository compiles the spec.

Their expected outputs are not an independent oracle. They were written by
hand and edited to follow code changes (for example commit `bc6f2bf5e7`, which
changed DRep expiry handling and the vectors that depend on it). A vector that
passes shows that Dugite still behaves the way the vector's author expected
when it was last edited, not that it agrees with the Haskell node or the
formal spec.

For comparisons against upstream sources see the `upstream/` fixtures and
`docs/src/reference/conformance.md`. In the `ledger-rules` area, LEDGER and
NEWEPOCH vectors are applied through dugite's ledger and compared with Haskell's
`st_out` (`just test-conformance-ledger-rules-apply-compare`; known divergences
are pinned in `tests/conformance/expected/ledger_rules_replay.tsv`); the other
rules (POOL, CERT, ...) only decode their fixtures
(`just test-conformance-ledger-rules-decode`) and apply no ledger rule.

**Validation**: The `dugite-conformance` crate loads the JSON vectors,
converts them to Dugite types via adapter functions, runs the corresponding
ledger function, and compares the result against the expected output in the
vector.

## Directory Structure

```
tests/conformance/
├── Cargo.toml              # Rust crate definition
├── README.md               # This file
├── src/
│   ├── lib.rs              # Crate root
│   ├── schema.rs           # Test vector JSON schema types
│   ├── adapters.rs         # Agda type ↔ Dugite type converters
│   └── runner.rs           # Test execution and comparison engine
├── tests/
│   └── conformance_tests.rs  # Integration test entry point
├── vectors/                # JSON test vectors
│   ├── utxo/               # UTXO rule vectors
│   ├── cert/               # CERT rule vectors
│   ├── gov/                # GOV rule vectors
│   └── epoch/              # EPOCH rule vectors
└── upstream/               # Pinned upstream fixture manifests (separate harness)
```

## Running Tests

```bash
# Run all conformance tests
cargo test -p dugite-conformance

# Run with output
cargo test -p dugite-conformance -- --nocapture

# Run only UTXO tests
cargo test -p dugite-conformance conformance_utxo

# Run only CERT tests
cargo test -p dugite-conformance conformance_cert

# Run only GOV tests
cargo test -p dugite-conformance conformance_gov

# Run only EPOCH tests
cargo test -p dugite-conformance conformance_epoch
```

## Supported Rules

| Rule   | Status        | Description |
|--------|--------------|-------------|
| UTXO   | 30 vectors   | Transaction validation against UTxO state (success + failure) |
| CERT   | 14 vectors   | Certificate processing (delegation, registration, DRep, failure) |
| GOV    | 8 vectors    | Governance proposal submission and voting (Conway era) |
| EPOCH  | 6 vectors    | Epoch boundary transitions (proposal expiry, pool retirement, DRep inactivity) |

## Test Vector Schema

Each test vector is a JSON file with this structure:

```json
{
  "rule": "UTXO",
  "description": "Human-readable description",
  "environment": { ... },
  "input_state": { ... },
  "signal": { ... },
  "expected_output": {
    "type": "success",
    "state": { ... }
  }
}
```

For failure cases:

```json
{
  "expected_output": {
    "type": "failure",
    "errors": ["ValueNotConserved"]
  }
}
```

### Type Mapping

The vectors use simplified representations of the spec's abstract types that
map to Dugite's concrete types:

| Agda Type     | JSON Representation          | Dugite Type              |
|---------------|------------------------------|---------------------------|
| TxId          | 64-char hex string           | `Hash32` (TransactionHash)|
| Addr          | Tagged enum (base/enterprise/reward/byron) | `Address` enum |
| Credential    | `{"type": "vkey", "hash": "..."}` | `Credential` enum   |
| Coin          | Integer (lovelace)           | `Lovelace(u64)`           |
| UTxO          | Array of `{tx_hash, index, output}` | `UtxoSet`          |
| PParams       | Subset of protocol params    | `ProtocolParameters`      |

## Adding New Rules

1. **Schema**: Add new types in `src/schema.rs` for the rule's environment,
   state, and signal types.

2. **Adapters**: Add conversion functions in `src/adapters.rs` to map between
   the test vector types and Dugite's types.

3. **Runner**: Add a `run_<rule>_test()` function in `src/runner.rs` and
   register it in the `run_test()` dispatch.

4. **Vectors**: Create JSON test vectors in `vectors/<rule>/`.

5. **Integration test**: Add a `conformance_<rule>_vectors()` test in
   `tests/conformance_tests.rs`.

## Limitations

- The vectors are hand-written and cover basic scenarios only. Their expected
  outputs are not derived from the Agda spec or the Haskell node.
- UTXO validation skips witness verification (signatures/scripts) since test
  vectors use simplified types without cryptographic material.
- The CERT runner simulates certificate application directly rather than going
  through `LedgerState::process_certificate` (which is not public API at the
  UTxO level).
- Error matching uses category names rather than exact error types, since the
  formal spec's error types are more abstract than Dugite's.
