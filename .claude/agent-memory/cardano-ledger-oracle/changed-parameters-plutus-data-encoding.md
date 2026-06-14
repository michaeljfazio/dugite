---
name: changed-parameters-plutus-data-encoding
description: Exact Plutus Data encoding for ChangedParameters (PParamsUpdate) inside ParameterChange governance action for V3 ScriptContext — ppuTag keys, all value type shapes, CostModels, voting thresholds
metadata:
  type: reference
---

# ChangedParameters — Exact Plutus Data Encoding (Conway V3)

Verified from IntersectMBO/cardano-ledger master (2026-06-14).

## Key Sources

- `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs` — `transGovAction`, `toPlutusChangedParameters`
- `libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs` — `ToPlutusData (PParamsUpdate era)` instance
- `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/ToPlutusData.hs` — all value type instances
- `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` — `eraPParams` ppuTag table, `PoolVotingThresholds`/`DRepVotingThresholds` instances
- `plutus-ledger-api/src/PlutusLedgerApi/V3/Contexts.hs` — `ChangedParameters` newtype, `GovernanceAction` schema

## 1. ChangedParameters Shape

```haskell
newtype ChangedParameters = ChangedParameters { getChangedParameters :: PlutusTx.BuiltinData }
  deriving newtype (PlutusTx.ToData, PlutusTx.FromData, ...)
```

`ChangedParameters` is an OPAQUE `BuiltinData` wrapper. Its content is populated by:

```haskell
-- In Conway/TxInfo.hs:
instance ConwayEraPlutusTxInfo 'PlutusV3 ConwayEra where
  toPlutusChangedParameters _ x =
    PV3.ChangedParameters (PV3.dataToBuiltinData (toPlutusData x))
```

Where `toPlutusData x` calls the `ToPlutusData (PParamsUpdate era)` instance.

## 2. PParamsUpdate → Data::Map

```haskell
-- In libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs:
instance ConwayEraScript era => ToPlutusData (PParamsUpdate era) where
  toPlutusData ppu = P.Map $ mapMaybe ppToData (eraPParams @era)
    where
      ppToData PParam {ppUpdate} = do
        PParamUpdate {ppuTag, ppuLens} <- ppUpdate
        t <- strictMaybeToMaybe $ ppu ^. ppuLens
        pure (P.I (toInteger @Word ppuTag), toPlutusData t)
```

**Top-level shape: `Data::Map [(I ppuTag, value)]`**

- ONLY fields that are `SJust` (set in the update) appear in the map
- `SNothing` fields are OMITTED entirely
- Map is built from `eraPParams` in field order (ascending ppuTag), filtered by presence
- Keys are `Data::I` integers (the ppuTag word)

## 3. ppuTag → Integer Key Table

Keys 12, 13, 14, 15 are ABSENT in Conway (removed in Alonzo/Babbage).
protocolVersion (array pos 30 in PParams) has NO ppuTag — NOT updatable.

| ppuTag | Field name | Haskell type |
|--------|-----------|--------------|
| 0 | txFeePerByte (minFeeA) | CoinPerByte |
| 1 | txFeeFixed (minFeeB) | CompactForm Coin |
| 2 | maxBBSize | Word32 |
| 3 | maxTxSize | Word32 |
| 4 | maxBHSize | Word16 |
| 5 | keyDeposit | CompactForm Coin |
| 6 | poolDeposit | CompactForm Coin |
| 7 | eMax (poolRetireMaxEpoch) | EpochInterval |
| 8 | nOpt (stakePoolTargetNum) | Word16 |
| 9 | a0 (poolPledgeInfluence) | NonNegativeInterval |
| 10 | rho (monetaryExpansion) | UnitInterval |
| 11 | tau (treasuryCut) | UnitInterval |
| (12,13,14,15 absent in Conway) | | |
| 16 | minPoolCost | CompactForm Coin |
| 17 | coinsPerUTxOByte | CoinPerByte |
| 18 | costModels | CostModels |
| 19 | prices (executionUnitPrices) | Prices |
| 20 | maxTxExUnits | ExUnits |
| 21 | maxBlockExUnits | ExUnits |
| 22 | maxValSize | Word32 |
| 23 | collateralPercentage | Word16 |
| 24 | maxCollateralInputs | Word16 |
| 25 | poolVotingThresholds | PoolVotingThresholds |
| 26 | dRepVotingThresholds | DRepVotingThresholds |
| 27 | committeeMinSize | Word16 |
| 28 | committeeMaxTermLength | EpochInterval |
| 29 | govActionLifetime | EpochInterval |
| 30 | govActionDeposit | CompactForm Coin |
| 31 | dRepDeposit | CompactForm Coin |
| 32 | dRepActivity | EpochInterval |
| 33 | minFeeRefScriptCostPerByte | NonNegativeInterval |

**CRITICAL NOTE**: The ppuTag integers DO match the CBOR PParamsUpdate map keys.
The array-position index in PParams is DIFFERENT (e.g., minPoolCost is at array pos 12 but ppuTag 16).

## 4. Value Type Encodings

### Coin / CompactForm Coin / CoinPerByte
All → `Data::I n` (the lovelace integer)

```haskell
-- CompactForm Coin:
instance ToPlutusData (CompactForm Coin) where
  toPlutusData = toPlutusData . fromCompact  -- → Coin → I n

-- CoinPerByte:
instance ToPlutusData CoinPerByte where
  toPlutusData (CoinPerByte c) = toPlutusData @(CompactForm Coin) c  -- → I n
```

### Word16 / Word32 / Word (ppuTag type)
All → `Data::I n`

```haskell
instance ToPlutusData Word16 where
  toPlutusData w16 = I (toInteger @Word16 w16)
instance ToPlutusData Word32 where
  toPlutusData w32 = I (toInteger @Word32 w32)
instance ToPlutusData Word where
  toPlutusData w = I (toInteger @Word w)
```

### EpochInterval (eMax, committeeMaxTermLength, govActionLifetime, dRepActivity)
→ `Data::I n` (underlying Word32)

```haskell
deriving instance ToPlutusData EpochInterval
-- EpochInterval is newtype Word32 → I (toInteger word32)
```

### UnitInterval / NonNegativeInterval (a0, rho, tau, minFeeRefScriptCostPerByte, threshold fields)
→ `Data::List [I numerator, I denominator]`  (NOT Constr 0!)

```haskell
instance ToPlutusData UnitInterval where
  toPlutusData = toPlutusData . unboundRational  -- → Rational

instance ToPlutusData Rational where
  toPlutusData (num :% denom) = List [I num, I denom]
```

**CRITICAL**: Rational in Plutus Data = `Data::List [I num, I den]`
**NOT** `Constr 0 [I num, I den]` (the Constr 0 form is used for `Rational` in PlutusLedgerApi's `UpdateCommittee` threshold, which is a DIFFERENT type using `makeIsDataSchemaIndexed`)

### ExUnits (maxTxExUnits, maxBlockExUnits)
→ `Data::List [I mem, I steps]` (mem FIRST, steps SECOND)

```haskell
instance ToPlutusData ExUnits where
  toPlutusData (ExUnits a b) = List [toPlutusData a, toPlutusData b]
-- ExUnits { exUnitsMem = a, exUnitsSteps = b }
```

### Prices (executionUnitPrices / prices)
→ `Data::List [List[I mem_num, I mem_den], List[I steps_num, I steps_den]]`
(prMem FIRST as a Rational, prSteps SECOND as a Rational)

```haskell
instance ToPlutusData Prices where
  toPlutusData p = List [toPlutusData (prMem p), toPlutusData (prSteps p)]
-- each rational → List [I num, I den]
```

### CostModels
→ `Data::Map [(I lang_id, List [I cost0, I cost1, ...])]`

```haskell
instance ToPlutusData CostModels where
  toPlutusData costModels = toPlutusData $ fmap toInteger <$> flattenCostModels costModels
  -- flattenCostModels :: CostModels -> Map Word8 [Int64]
  -- languageToWord8 lang = fromEnum lang
  -- PlutusV1=0, PlutusV2=1, PlutusV3=2, PlutusV4=3

-- Map (Word8→[Integer]) encodes as:
instance (Ord a, ToPlutusData a, ToPlutusData b) => ToPlutusData (Map a b) where
  toPlutusData m = Map $ map (\(a,b) -> (toPlutusData a, toPlutusData b)) (Map.toAscList m)

-- Word8 → I (toInteger w8)
-- [Integer] → List [I i0, I i1, ...]
```

Final shape: `Data::Map [(I 0, List[I c0, I c1, ...]), (I 1, List[...]), (I 2, List[...])]`
Keys: 0=PlutusV1, 1=PlutusV2, 2=PlutusV3
Each value: `Data::List` of integers (the cost model parameter values in order)
Unknown languages also included with their raw Word8 key.

### PoolVotingThresholds (ppuTag 25)
→ `Data::List` of 5 UnitInterval rationals

```haskell
instance ToPlutusData PoolVotingThresholds where
  toPlutusData x = P.List
    [ toPlutusData (pvtMotionNoConfidence x)      -- List [I n, I d]
    , toPlutusData (pvtCommitteeNormal x)          -- List [I n, I d]
    , toPlutusData (pvtCommitteeNoConfidence x)    -- List [I n, I d]
    , toPlutusData (pvtHardForkInitiation x)       -- List [I n, I d]
    , toPlutusData (pvtPPSecurityGroup x)          -- List [I n, I d]
    ]
```

### DRepVotingThresholds (ppuTag 26)
→ `Data::List` of 10 UnitInterval rationals

```haskell
instance ToPlutusData DRepVotingThresholds where
  toPlutusData x = P.List
    [ toPlutusData (dvtMotionNoConfidence x)       -- [0]
    , toPlutusData (dvtCommitteeNormal x)           -- [1]
    , toPlutusData (dvtCommitteeNoConfidence x)     -- [2]
    , toPlutusData (dvtUpdateToConstitution x)      -- [3]
    , toPlutusData (dvtHardForkInitiation x)        -- [4]
    , toPlutusData (dvtPPNetworkGroup x)            -- [5]
    , toPlutusData (dvtPPEconomicGroup x)           -- [6]
    , toPlutusData (dvtPPTechnicalGroup x)          -- [7]
    , toPlutusData (dvtPPGovGroup x)                -- [8]
    , toPlutusData (dvtTreasuryWithdrawal x)        -- [9]
    ]
```

Each element: `Data::List [I numerator, I denominator]` (via UnitInterval → Rational)

## 5. SNothing Fields Are Omitted

From the `mapMaybe ppToData` logic:
```haskell
t <- strictMaybeToMaybe $ ppu ^. ppuLens
```
If `ppuLens` returns `SNothing`, the `do`-block returns `Nothing` and `mapMaybe` drops it.
The resulting Plutus Data map contains ONLY the fields that are `SJust` (actually changed).

## 6. protocolVersion

NOT present in PParamsUpdate at all. Conway removed it from the updatable set (governed by HFC).
It has no ppuTag and will never appear in the `ChangedParameters` Data map.

## 7. GovernanceAction::ParameterChange Wrapping

```
ParameterChange = Constr 0 [Maybe GovActionId, ChangedParameters, Maybe ScriptHash]
```

`ChangedParameters` is field index 1 (0-based). Its Plutus Data value is the `Data::Map` described above — passed through `dataToBuiltinData` in the ledger, then unwrapped back to `Data` for script evaluation.

## 8. Rust Implementation Notes (dugite-uplc)

In `crates/dugite-uplc/src/populate_gov.rs`, replace the placeholder:
```rust
// WRONG (current placeholder):
Data::Constr(0, vec![])

// CORRECT:
fn changed_parameters_to_data(ppu: &ProtocolParamUpdate) -> Data {
    let mut entries: Vec<(Data, Data)> = Vec::new();
    // For each field in ppuTag order (0,1,2,...,33 skipping 12-15):
    //   if the field is Some(v): entries.push((Data::I(ppuTag.into()), encode_value(v)))
    // Sort by ppuTag (ascending) to match Map.toAscList on eraPParams order
    Data::Map(entries)
}
```

Value encoders:
- Coin/CompactForm Coin/CoinPerByte → `Data::I(lovelace)`
- Word16/Word32/EpochInterval → `Data::I(n)`
- UnitInterval/NonNegativeInterval (a0, rho, tau, minFeeRefScriptCostPerByte, threshold rationals)
  → `Data::List(vec![Data::I(num), Data::I(den)])`  ← NOT Constr 0!
- ExUnits → `Data::List(vec![Data::I(mem), Data::I(steps)])` (mem first)
- Prices → `Data::List(vec![rational_to_data(mem_price), rational_to_data(steps_price)])`
- CostModels → `Data::Map` keyed by language integer (0=V1, 1=V2, 2=V3) → `Data::List` of `Data::I` cost values
- PoolVotingThresholds → `Data::List` of 5 rationals
- DRepVotingThresholds → `Data::List` of 10 rationals

## 9. Key Pitfalls

1. **Rational = List NOT Constr 0**: The `Rational` in `UpdateCommittee` (the quorum threshold field) uses `Constr 0 [I num, I den]` because it's from `PlutusLedgerApi.V3` `Rational` type with `makeIsDataSchemaIndexed`. But the `Rational` in `ToPlutusData.hs` (used for all PParam rationals) is `List [I num, I den]`. These are DIFFERENT types with DIFFERENT encodings.

2. **ppuTag ≠ PParams array index**: e.g., minPoolCost is at array index 12 in PParams but ppuTag 16 in PParamsUpdate.

3. **Map ordering**: The map entries appear in `eraPParams` field order (ascending ppuTag), which equals ascending integer key order. This matches `Map.toAscList`.

4. **CostModels includes unknowns**: `flattenCostModels` merges both `validCostModels` (keyed by Language) and `unknownCostModels` (keyed by Word8). If unknown language cost models are present on-chain, they appear in the Plutus Data map with their raw integer keys.
