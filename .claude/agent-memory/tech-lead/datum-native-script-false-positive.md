---
name: datum-native-script-false-positive
description: MissingDatumWitness false positives on native-script-locked inputs with DatumHash (epoch 434 Babbage)
metadata:
  type: reference
---

# Datum Witness Native-Script Exemption

**Bug:** dugite emitted `MissingDatumWitness` for native-script-locked inputs carrying a `DatumHash` — Haskell never requires datum witnesses for these.

**Haskell source:** `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs`, `getInputDataHashesTxBody`:
```haskell
DatumHash dataHash
  | isSpendingPlutusScript addr -> (Set.insert dataHash hashSet, inputSet)
-- native scripts fall through to:
_ -> ans
```
`isSpendingPlutusScript` = `getScriptHash addr >> lookupPlutusScript`, where `lookupPlutusScript` uses `toPlutusScript` returning `Nothing` for native scripts.

**Fix location:** `crates/dugite-ledger/src/validation/datum.rs`, the `OutputDatum::DatumHash` branch in `check_datum_witnesses`. Now guarded with `if version > 0` (same guard as the `OutputDatum::None` path).

**Key invariant:** `script_versions` map (built by `plutus_script_version_map`) omits native script hashes → version 0 → not required. Tests must explicitly insert the Plutus version into the map when testing true-positive behavior.

**Comment from Haskell source:** "Though it is somewhat odd to allow native scripts to include a datum, the Alonzo era already set the precedent with datum hashes, and several dapp developers see this as a helpful feature."
