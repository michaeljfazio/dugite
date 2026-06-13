---
name: issue-758-native-script-spend-redeemer
description: Native-script-locked spending input must NOT require a Spend redeemer — Plutus filter in hasExactSetOfRedeemers
type: reference
---

## Issue #758 — Phase-1 False Positive on Native-Script-Locked Input

**Haskell Citation:** `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs`,
`hasExactSetOfRedeemers`, `neededPlutusSet` filter:
```haskell
neededPlutusSet = [(purpose, sh) | (purpose, sh) <- scriptsNeeded
                                  , Map.lookup sh provided == Just (PlutusScript _)]
```
Native-script-locked inputs are in `scriptsNeeded` but drop out of `neededPlutusSet`.

**Fix:** `check_script_redeemers` in `crates/dugite-ledger/src/validation/collateral.rs`
now takes a `script_versions: &HashMap<Hash28, u8>` parameter. The Spend loop gates
`MissingSpendRedeemer` on `script_versions.get(sh).copied().unwrap_or(0) > 0`.

**Pattern:** This is the SAME family as the datum-witness fix (commit eadec38afe).
Any "missing witness" check that applies to script-locked inputs must consult
`plutus_script_version_map` to distinguish Plutus (version ≥ 1) from native (version 0).

**Also fixed:** `test_script_locked_input_missing_redeemer` had a bug where the UTxO
payment credential did not match the Plutus script's actual hash — it was passing only
because the old code was unconditional. Fixed to derive `script_hash` from the script bytes.

**Call site (mod.rs ~3714):**
```rust
let script_versions_for_redeemers = collateral::plutus_script_version_map(tx, utxo_set);
collateral::check_script_redeemers(tx, utxo_set, &script_versions_for_redeemers, &mut errors);
```

**check_extra_redeemers Spend section is CORRECT as-is:** It marks ALL script-locked
inputs (native or Plutus) as valid Spend redeemer targets — a Spend redeemer for a
native-script input is not "extra".
