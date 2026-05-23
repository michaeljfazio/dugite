# Alonzo Era Ledger Rules — Delta over Mary

Source: `IntersectMBO/cardano-ledger` master, `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/`.

## 1. Block body — 4-segment SegWit

`BlockBody/Internal.hs`:
```haskell
data AlonzoBlockBody era = AlonzoBlockBodyInternal
  { abbTxs             :: !(StrictSeq (Tx TopTx era))
  , abbHash            :: Hash.Hash HASH EraIndependentBlockBody
  , abbTxsBodyBytes    :: BSL.ByteString   -- segment 0
  , abbTxsWitsBytes    :: BSL.ByteString   -- segment 1
  , abbTxsAuxDataBytes :: BSL.ByteString   -- segment 2
  , abbTxsIsValidBytes :: BSL.ByteString   -- segment 3
  }
```

`EncCBORGroup` encodes 4 segments concatenated, `listLen _ = 4`. Block CBOR group: `[bodies_array, wits_array, auxdata_map, isvalid_array]`.

**Non-validating index encoding**: stores only indices of `IsValid = False` txs:
```haskell
nonValidatingIndices xs =
  Seq.foldrWithIndex (\idx tx acc ->
    if tx ^. isValidTxL == IsValid False then idx : acc else acc) [] xs
```

For block with no failing txs, segment 3 is empty CBOR array. Hash computed over 4 independent sub-hashes (not the raw 4-array).

**Rust note**: switch from 3-element to 4-element block body decoder at Alonzo boundary.

## 2. TransactionBody — new sparse-map keys

From `TxBody.hs` `EncCBOR (AlonzoTxBodyRaw l AlonzoEra)`:

| Key | Field | Type | Notes |
|---|---|---|---|
| 0 | `inputs` | `Set TxIn` | required |
| 1 | `outputs` | `StrictSeq TxOut` | required |
| 2 | `fee` | `Coin` | required |
| 3 | `ttl` | `SlotNo` | optional (= `validity_interval_end`) |
| 4 | `certs` | `StrictSeq TxCert` | omitted when empty |
| 5 | `withdrawals` | `Withdrawals` | omitted when empty |
| 6 | `update` | `Update` | optional (PPUP) |
| 7 | `auxiliary_data_hash` | `TxAuxDataHash` | optional |
| 8 | `validity_interval_start` | `SlotNo` | optional |
| 9 | `mint` | `MultiAsset` | omitted when zero |
| **11** | **`script_data_hash`** | `ScriptIntegrityHash` | **Alonzo new** |
| **13** | **`collateral`** | `Set TxIn` | **Alonzo new; omitted when empty** |
| **14** | **`required_signers`** | `Set (KeyHash Guard)` | **Alonzo new; omitted when empty** |
| **15** | **`network_id`** | `Network` | **Alonzo new; optional** |

**Note**: keys 10 and 12 are unused in Alonzo (10 = Babbage `collateral_return`, 12 = Babbage `total_collateral`).

### Key 11 — `script_data_hash`
`Blake2b-256` over `ScriptIntegrity`:
```
ScriptIntegrity = (txRedeemers_bytes, txDats_bytes (omit when empty), Set canonical LangDepView)
hash = Blake2b-256(redeemers_bytes ++ dats_bytes ++ canonical_lang_view_map)
```
PlutusV1 language view has a deliberate double-serialization bug:
```haskell
PlutusV1 -> LangDepView (serialize' v (serialize' v lang)) ...
```
**Must reproduce exactly** for cross-validation.

Absent when no redeemers/datums/languages. If absent and script inputs exist, UTXOW fails.

### Key 13 — `collateral`
Set of `TxIn` to VKey-locked UTxOs. Consumed entirely on Phase-2 failure. Disjoint from `inputs` (key 0). `allInputsTxBodyF = inputs ∪ collateral` ⊆ `dom(utxo)`.

### Key 14 — `required_signers`
Set of `KeyHash Guard`. Added to `witsVKeyNeeded` in UTXOW. Corresponding VKey sigs required.

### Key 15 — `network_id`
Optional `Network`. Must match `Globals.networkId`. Validated by `validateWrongNetworkInTxBody`.

## 3. TransactionWitnessSet — new keys

From `TxWits.hs`:

| Key | Field |
|---|---|
| 0 | `vkeyWitnesses` |
| 1 | `nativeScripts` |
| 2 | `bootstrapWitnesses` |
| **3** | **`plutusV1Scripts`** |
| **4** | **`plutusData`** — set of raw `Data`, tag 24 wrapped |
| **5** | **`redeemers`** |
| 6 | `plutusV2Scripts` (forward, activated in Babbage) |
| 7 | `plutusV3Scripts` (forward) |

**Redeemers encoding split** at protocol version 9:
- **Pre-PV9**: indefinite-length list of `[tag, index, data, ex_units]` 4-tuples (tag: 0=Spend, 1=Mint, 2=Cert, 3=Withdraw)
- **PV9+**: definite CBOR map from `PlutusPurpose` (2-element group) to `(Data, ExUnits)`

**Replay code must handle both** for historical block replay across the Alonzo era.

## 4. IsValid flag

```haskell
newtype IsValid = IsValid Bool
```

Block body's 4th segment (`abbTxsIsValidBytes`) lists only indices where IsValid = False. `IsValid True` is implicit. `alignedValidFlags` reconstructs full sequence.

### UTXOS dispatch
```haskell
case tx ^. isValidTxL of
  IsValid True  -> alonzoEvalScriptsTxValid
  IsValid False -> alonzoEvalScriptsTxInvalid
```

- **`IsValid True`**: scripts evaluated. Any Plutus failure → `ValidationTagMismatch (IsValid True) (FailedUnexpectedly fs)`. PPUP sub-rule applied.
- **`IsValid False`**: scripts evaluated. All-pass → `ValidationTagMismatch (IsValid False) PassedUnexpectedly`. **PPUP NOT applied** for invalid txs.

Block producer sets the flag; ledger trusts the tag for code path but verifies consistency.

## 5. Collateral consumption (Alonzo-specific)

**Entire collateral input ADA forfeited** on Phase-2 failure. No `collateral_return`, no `total_collateral` (those are Babbage).

```haskell
IsValid False ->
  let !(utxoKeep, utxoDel) = extractKeys (unUTxO utxo) (txBody ^. collateralInputsTxBodyL)
  in pure $! utxos
    { utxosUtxo    = UTxO utxoKeep
    , utxosFees    = utxosFees utxos <> sumAllCoin utxoDel
    , utxosInstantStake = deleteInstantStake (UTxO utxoDel) (utxos ^. instantStakeL)
    }
```

Regular `inputs`/`outputs`/`certs`/`withdrawals`/scripts/datum **completely skipped** when IsValid=False.

### Collateral validation (`feesOK`)
1. `minFee ≤ txfee` — validated even for invalid txs
2. If `txrdmrs ≠ ∅` → validate collateral:
   - Part 3: All collateral inputs must be VKey-locked (`validateScriptsNotPaidUTxO`)
   - Part 4: `balance * 100 ≥ txfee * collateralPercentage` (`validateInsufficientCollateral`)
   - Part 5: Collateral inputs ADA-only (`validateCollateralContainsNonADA`)
   - Part 6: At least one collateral input (`NoCollateralInputs`)
   - `‖collateral‖ ≤ maxCollateralInputs` (`validateTooManyCollateralInputs`)

## 6. Phase-1 vs Phase-2 validation

### Phase-1 — deterministic, always runs

UTXO predicates (order from `utxoTransition`):
1. `validateOutsideValidityIntervalUTxO`
2. `validateOutsideForecast`
3. `validateInputSetEmptyUTxO`
4. `feesOK`
5. `validateBadInputsUTxO`
6. `validateValueNotConservedUTxO`
7. `validateOutputTooSmallUTxO` — utxoEntrySize-based
8. `validateOutputTooBigUTxO` — `serSize(value) ≤ maxValSize pp`
9. `validateOutputBootAddrAttrsTooBig`
10. `validateWrongNetwork`
11. `validateWrongNetworkWithdrawal`
12. `validateWrongNetworkInTxBody`
13. `validateMaxTxSizeUTxO`
14. `validateExUnitsTooBigUTxO`

UTXOW predicates (order from `alonzoStyleWitness`):
1. `validateFailedNativeScripts`
2. `validateMissingScripts`
3. `missingRequiredDatums` (3-part: UnspendableUTxONoDatumHash / MissingRequiredDatums / NotAllowedSupplementalDatums)
4. `hasExactSetOfRedeemers` (ExtraRedeemers / MissingRedeemers)
5. `validateVerifiedWits`
6. `validateNeededWitnesses` (includes `required_signers`)
7. `validateMIRInsufficientGenesisSigs`
8. `validateMetadata`
9. `checkScriptIntegrityHash` (must match exactly; absent if both)

### Phase-2 — script evaluation (UTXOS)

1. `collectPlutusScriptsWithContext` — build `PlutusWithContext` for each Plutus script
2. `CollectErrors` if any required script/datum/redeemer missing
3. `evalPlutusScripts` — CEK machine within ExUnits budget
4. **Tag cross-check**: IsValid=True path → Fails = error. IsValid=False path → Passes = error.

`totExUnits tx = foldMap snd redeemers`. Each script gets its declared budget.

**Cost model**: `ppCostModels` (PPUP key 18). Alonzo: only PlutusV1.

## 7. PPUP additions (Alonzo)

| Update Key | Field | Type |
|---|---|---|
| 16 | `minPoolCost` | `Coin` |
| **17** | `coinsPerUTxOWord` | `CoinPerWord` |
| **18** | `costModels` | `CostModels` |
| **19** | `executionUnitPrices` | `Prices` |
| **20** | `maxTxExecutionUnits` | `ExUnits` |
| **21** | `maxBlockExecutionUnits` | `ExUnits` |
| **22** | `maxValueSize` | `Word32` |
| **23** | `collateralPercentage` | `Word16` |
| **24** | `maxCollateralInputs` | `Word16` |

Defaults: `appPrices = Prices minBound minBound`; `appCollateralPercentage = 150`; `appMaxCollateralInputs = 5`.

**`Prices`**: 2 `NonNegativeInterval`s (mem + step prices).

**`ExUnits`**: 2 `Word64`s (mem + steps), 2-element CBOR array.

**CostModels**: PlutusV1 uses **indefinite-length** CBOR list at Alonzo (script integrity hash compat). PlutusV2+ definite. Critical: an implementation using definite-length for V1 will produce different `script_data_hash`.

## 8. Era transition Mary → Alonzo

Context: `AlonzoGenesis` provides `UpgradeAlonzoPParams`:
```haskell
data UpgradeAlonzoPParams f = UpgradeAlonzoPParams
  { uappCoinsPerUTxOWord, uappPlutusV1CostModel, uappPrices,
    uappMaxTxExUnits, uappMaxBlockExUnits, uappMaxValSize,
    uappCollateralPercentage, uappMaxCollateralInputs }
```

**`upgradeAlonzoPParams`**: copies all Shelley fields + appends 8 new from genesis. Notably:
- `appCostModels = singleton PlutusV1 ...`
- `appMinUTxOValue` (Mary) DROPPED; replaced by `appCoinsPerUTxOWord`
- `appProtocolVersion` unchanged; HFC bumps to `5.0` at transition epoch

**State translation**:
- UTxO: each `TxOut` via `upgradeTxOut` (Alonzo adds optional datum hash, `SNothing` for all carried-over)
- `LedgerState`, `CertState`, `DState`, `PState`: identity translation
- `ShelleyGovState`: PPUP proposals upgraded via `fmap upgradePParamsUpdate`
- Translated Mary txs: `isValidTxL = IsValid True` (Mary has no Phase-2)
- `stashedAVVMAddresses = ()`

## 9. Auxiliary data — CBOR tag 259

`TxAuxData.hs`:
```haskell
encCBOR AlonzoTxAuxDataRaw{...} =
  encode $ Tag 259 $ Keyed (...)
    !> Omit null (Key 0 $ To atadrMetadata)         -- Map Word64 Metadatum
    !> Omit null (Key 1 $ To atadrNativeScripts)
    !> Omit isNothing (Key 2 $ ...)  -- PlutusV1
    !> Omit isNothing (Key 3 $ ...)  -- PlutusV2 (Babbage)
    !> Omit isNothing (Key 4 $ ...)  -- PlutusV3 (Conway)
    !> Omit isNothing (Key 5 $ ...)  -- PlutusV4 (future)
```

Alonzo: keys 0, 1, 2 meaningful. Keys 3-5 forward-allocated only.

**Backward compat dispatch**: decoder peeks at leading token:
- CBOR map (type 5) → Shelley metadata
- Tag 259 → Alonzo-extended aux data

## 10. Predicate failure tags (wire)

UTXO (Alonzo-new):
| Tag | Failure |
|---|---|
| 13 | `InsufficientCollateral` |
| 14 | `ScriptsNotPaidUTxO` |
| 15 | `ExUnitsTooBigUTxO` |
| 16 | `CollateralContainsNonADA` |
| 17 | `WrongNetworkInTxBody` |
| 18 | `OutsideForecast` |
| 19 | `TooManyCollateralInputs` |
| 20 | `NoCollateralInputs` |

UTXOW (Alonzo-new):
| Tag | Failure |
|---|---|
| 0 | `ShelleyInAlonzoUtxowPredFailure` (wraps Shelley) |
| 1 | `MissingRedeemers` |
| 2 | `MissingRequiredDatums` |
| 3 | `NotAllowedSupplementalDatums` |
| 4 | `PPViewHashesDontMatch` (pre-PV11) |
| 6 | `UnspendableUTxONoDatumHash` |
| 7 | `ExtraRedeemers` |
| 8 | `ScriptIntegrityHashMismatch` (PV11+; same semantic as 4) |

Tag 5 absent (removed constructor, reserved for stability).

UTXOS:
| Tag | Failure |
|---|---|
| 0 | `ValidationTagMismatch` |
| 1 | `CollectErrors` |
| 2 | `UpdateFailure` (PPUP) |

## Rust notes for dugite

1. **Block body decoder**: 4-element group at Alonzo. 4th element is array of `u64` indices. Use `alignedValidFlags` inversion.
2. **TxBody keys 11/13/14/15**: sparse map. Key 13 + 14 use `Omit null` (absent when empty).
3. **Collateral on IsValid=false**: delete collateral from UTxO, add summed ADA to fees, remove stake. Nothing else.
4. **Script integrity hash**: `Blake2b-256(redeemers_bytes ++ dats_bytes ++ canonical_lang_view_map)`. PlutusV1 lang view double-wrap bug must be reproduced.
5. **Redeemer encoding version split**: pre-PV9 indef list 4-tuples; PV9+ definite map.
6. **Aux data tag 259**: peek at leading token to dispatch Shelley vs Alonzo.
7. **Phase-2 skip on replay**: `tickThenReapply` / `reapplyTx` skip script eval (STS.ValidateNone).
8. **`required_signers` (14)**: add to `witsVKeyNeeded` in UTXOW check.
