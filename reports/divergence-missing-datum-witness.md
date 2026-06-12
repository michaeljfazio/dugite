# Phase-1 False Positive: MissingDatumWitness on Native-Script-Locked Inputs

**Date:** 2026-06-12  
**Era:** Babbage PV8 (epoch ~434–435, mainnet)  
**Instances in log:** 8 (slots 102975745–103133005)  
**Fix:** `crates/dugite-ledger/src/validation/datum.rs`  

---

## Root Cause

Dugite's `check_datum_witnesses` (in `datum.rs`) incorrectly required datum preimage
witnesses for native-script-locked UTxO inputs that carry a `DatumHash` field.

The Haskell reference (`getInputDataHashesTxBody`, file
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs`) only adds a datum hash to the
required set when `isSpendingPlutusScript addr` returns `true`:

```haskell
DatumHash dataHash
  | isSpendingPlutusScript addr -> (Set.insert dataHash hashSet, inputSet)
-- "Though it is somewhat odd to allow native scripts to include a datum,
--  the Alonzo era already set the precedent with datum hashes, and several
--  dapp developers see this as a helpful feature."
_ -> ans
```

`isSpendingPlutusScript` chains `getScriptHash addr >> lookupPlutusScript`, where
`lookupPlutusScript` calls `toPlutusScript` which returns `Nothing` for native scripts
(`NativeScript`/`Timelock`).  A native-script-locked input therefore falls to `_ -> ans`
(no change to the required set) regardless of whether its UTxO carries a `DatumHash`.

Dugite's pre-fix code at line 178 (`datum.rs`) was:

```rust
if let OutputDatum::DatumHash(hash) = &utxo.datum {
    required_datum_hashes.insert(*hash);   // wrong: no Plutus guard
}
```

The `script_versions` map (built by `plutus_script_version_map`) correctly omits native
script hashes (they map to version 0 or are absent).  The `OutputDatum::None` path
already had the correct guard (`if version > 0 && version < 3 { ... }`), but the
`DatumHash` path lacked it.

---

## Where the Datum Actually Lived in the Exemplar Txs

For tx `af4a50e599f6...` (slot 102975745):
- Spent input `62cfc1b2...#1` is at address `279b2518...` — a native-script address
  (`blake2b_224(0x00 || all_of([sig(5c27...)]))` = `279b2518...` confirmed).
- The UTxO carried `datum_hash = 6cdd5320...`.
- The witness set contains ONE datum: a 226-byte `Constr(0, [map(5), 1])` with
  `blake2b_256 = 48955f72...` — this is the **output** datum (not the spent input
  datum).  The spent input datum preimage was deliberately NOT provided.
- Haskell accepted this transaction because the spending script is native, so no
  datum witness was required.

The pattern is identical across all 8 instances: native-script-locked inputs with
`DatumHash` in their UTxO, datum preimage absent from witness set.

---

## Bug Location

**File:** `crates/dugite-ledger/src/validation/datum.rs`  
**Pre-fix lines 175–180:**

```rust
// Only DatumHash outputs require a witness datum.
// InlineDatum outputs embed the datum in the UTxO itself — no witness needed.
if let OutputDatum::DatumHash(hash) = &utxo.datum {
    required_datum_hashes.insert(*hash);
}
```

---

## Fix Summary

Added the same Plutus guard that already existed for the `OutputDatum::None` path
(UnspendableUTxONoDatumHash check).  A datum hash is now added to `required_datum_hashes`
only when `script_versions.get(script_hash).copied().unwrap_or(0) > 0`, i.e., the
locking script is a Plutus script (V1/V2/V3/V4).  When the hash is absent from the map
(version 0 = native script), the input is silently skipped — exactly matching Haskell.

---

## Tests Added

| Test name | Location | Purpose |
|---|---|---|
| `test_native_script_datum_hash_no_witness_required` | `datum.rs::tests` | Regression: native-script-locked input with `DatumHash` and no witness must NOT produce `MissingDatumWitness` |
| `test_plutus_v2_datum_hash_witness_required` | `datum.rs::tests` | Positive control: Plutus V2 input with `DatumHash` and no witness MUST produce `MissingDatumWitness` |

Existing tests updated to pass explicit Plutus version entries in `script_versions`
(tests 1, 2, 7, 8 in `datum.rs::tests`; and 3 tests in `validation::tests::tests`).

---

## Test/Clippy Results

```
cargo nextest run -p dugite-ledger: 1555 passed, 0 failed, 6 skipped
cargo clippy -p dugite-ledger --all-targets -- -D warnings: clean
cargo fmt -p dugite-ledger -- --check: clean
```

---

## Affected Log Lines (will no longer fire after fix)

```
WARN Phase-1 validation divergence ... tx=af4a50e599f6... errors=Missing datum witness ... 6cdd5320...
WARN Phase-1 validation divergence ... tx=ea9501d307547... errors=Missing datum witness ... 48955f72...
WARN Phase-1 validation divergence ... tx=19c0ad35d39d... errors=Missing datum witness ... 0539cb97...
WARN Phase-1 validation divergence ... tx=a2b41133aff8... errors=Missing datum witness ... 81335ccb...
WARN Phase-1 validation divergence ... tx=5f9d48794597... errors=Missing datum witness ... 6e1d810f...
WARN Phase-1 validation divergence ... tx=6dd9582c93c2... errors=Missing datum witness ... ca6fcee9...
WARN Phase-1 validation divergence ... tx=3fb55f94b9c0... errors=Missing datum witness ... 628de0b6...
WARN Phase-1 validation divergence ... tx=d3ad7348a679... errors=Missing datum witness ... 7a0d9cb4...
```
