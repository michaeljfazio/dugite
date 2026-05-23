# Babbage Era Ledger Rules — Delta over Alonzo

Source: `IntersectMBO/cardano-ledger` master, `eras/babbage/`.
Protocol version: 7.0–8.x (until Conway PV9).

## 1. Block header — Praos replaces TPraos

**Consensus-layer change** (not ledger). TPraos `BHBody` had 2 separate VRF outputs (`bheaderEta` for nonce, `bheaderL` for leader) and inlined `OCert` + `ProtVer` as flat fields → 15-element flat array.

**Praos `HeaderBody`** (in `ouroboros-consensus-protocol`):
```haskell
data HeaderBody crypto = HeaderBody
  { hbBlockNo, hbSlotNo, hbPrev, hbVk
  , hbVrfVk    :: !(VRF.VerKeyVRF (VRF crypto))
  , hbVrfRes   :: !(VRF.CertifiedVRF (VRF crypto) InputVRF)  -- SINGLE for both leader + nonce
  , hbBodySize :: !Word32
  , hbBodyHash :: !(Hash.Hash HASH EraIndependentBlockBody)
  , hbOCert    :: !(OCert crypto)    -- nested array(4)
  , hbProtVer  :: !ProtVer            -- nested array(2)
  }
```

CBOR uses `Rec HeaderBody` (map-style via `encode`), not flat list. `MemoBytes (HeaderRaw crypto)`. OCert nested array(4), ProtVer nested array(2).

**InputVRF**: `mkInputVRF slot epochNonce`. Single VRF eval covers both:
- Nonce: `vrfNonceValue = hashToNonce (certifiedOutput hbVrfRes)`
- Leader: `vrfLeaderValue = checkLeaderValue (certifiedOutput hbVrfRes) ...`

## 2. TransactionBody — new sparse-map keys

From `TxBody.hs` `EncCBOR` Babbage:

| Key | Field | Type | Notes |
|---|---|---|---|
| 0  | `inputs` | `Set TxIn` | required |
| **13** | `collateralInputs` | `Set TxIn` | Alonzo |
| **18** | `referenceInputs` | `Set TxIn` | **Babbage new** |
| 1  | `outputs` | `StrictSeq TxOut` | required |
| **16** | `collateralReturn` | `StrictMaybe TxOut` | **Babbage new (optional)** |
| **17** | `totalCollateral` | `StrictMaybe Coin` | **Babbage new (optional)** |
| 2  | `fee` | `Coin` | required |
| 3  | `validity_interval_end` (ttl) | `SlotNo` | |
| 4  | `certs` | | omitted when empty |
| 5  | `withdrawals` | | omitted when empty |
| 6  | `update` | `Update` | (PPUP) |
| 8  | `validity_interval_start` | `SlotNo` | |
| 14 | `requiredSigners` | `Set (KeyHash)` | omitted when empty |
| 9  | `mint` | | omitted when 0 |
| 11 | `scriptIntegrityHash` | | |
| 7  | `auxDataHash` | | |
| 15 | `networkId` | | |

### Key 16 — collateral_return
A `TxOut` (Babbage map format, see §3). Omitted when `SNothing`. TxIn: `TxIn (txIdTxBody txBody) (length outputs)` — index immediately after last regular output.

### Key 17 — total_collateral
`Coin` — net ADA consumed if scripts fail. Enforced: `collAdaBalance txBody utxoCollateral == toDeltaCoin total_collateral`. Protects against over-collateralization.

### Key 18 — reference_inputs
`Set TxIn`. Read-only access. **Not consumed**. Included in `allInputsTxBodyF` (subject to `validateBadInputsUTxO`) but not in `spendableInputsTxBodyF`.

```haskell
babbageAllInputsTxBodyF = to $ \txBody ->
  (txBody ^. inputsTxBodyL) `Set.union`
  (txBody ^. collateralInputsTxBodyL) `Set.union`
  (txBody ^. referenceInputsTxBodyL)
```

### Disjointness rule (PV9-PV10 only)
```haskell
disjointRefInputs pp inputs refInputs =
  when (pv > Babbage_high && pv < 11)
    (failureOnNonEmpty common BabbageNonDisjointRefInputs)
  where common = inputs `Set.intersection` refInputs
```

- Babbage itself (PV7, PV8): inputs ∩ ref allowed
- PV9, PV10 (early Conway): disjointness REQUIRED
- PV11+: relaxed again. See dugite issue #470.

## 3. TransactionOutput — map format

`TxOut.hs`. Optional map format **alongside** legacy 2/3-element arrays.

### Internal repr (memory-optimized)
```haskell
data BabbageTxOut era
  = TxOutCompact'         !CompactAddr !(CompactForm (Value era))
  | TxOutCompactDH'       !CompactAddr !(CompactForm (Value era)) !DataHash
  | TxOutCompactDatum     !CompactAddr !(CompactForm (Value era)) !(BinaryData era)
  | TxOutCompactRefScript !CompactAddr !(CompactForm (Value era)) !(Datum era) !(Script era)
  | TxOut_AddrHash28_AdaOnly ...
  | TxOut_AddrHash28_AdaOnly_DataHash32 ...
```

### CBOR encoding — new map format
```haskell
encodeTxOut cAddr cVal datum script =
  encode $ Keyed (,,,,)
    !> Key 0 (To cAddr)                              -- address
    !> Key 1 (To (fromCompact cVal))                 -- value
    !> Omit (== NoDatum) (Key 2 (To datum))          -- datum_option (optional)
    !> encodeKeyedStrictMaybeWith 3 encodeNestedCbor script  -- script_ref (optional, tag-24)
```

**Without datum/script** → legacy:
- 2-element: `[address, value]`
- 3-element: `[address, value, data_hash]` (Alonzo style)

### Decoder dispatch
```haskell
decodeBabbageTxOut decAddr =
  peekTokenType >>= \case
    TypeMapLenIndef -> decodeTxOut decAddr   -- map format
    TypeMapLen      -> decodeTxOut decAddr   -- map format
    _               -> oldTxOut              -- legacy array
```

### Key 2 — DatumOption
```haskell
data Datum era
  = NoDatum
  | DatumHash DataHash
  | Datum (BinaryData era)
```
- `[0, data_hash_bytes]` — hash variant
- `[1, #6.24(bstr)]` — inline variant (tag-24 wrapped CBOR bytes)

### Key 3 — script_ref
`#6.24(bstr)` — CBOR tag 24 wrapping era-versioned script bytes (2-element `[script_type, script_body]`).

### Minimum UTxO — byte-based
```haskell
babbageMinUTxOValue pp sizedTxOut =
  Coin $ fromIntegral (160 + sizedSize sizedTxOut) * fromIntegral cpb
  where CoinPerByte (CompactCoin cpb) = pp ^. ppCoinsPerUTxOByteL
```

160-byte constant overhead (TxIn key + Map entry).

## 4. Inline datums

`UTxO.hs::getBabbageSpendingDatum`:
```haskell
getBabbageSpendingDatum (UTxO utxo) tx sp = do
  AsItem txIn <- toSpendingPurpose sp
  txOut <- Map.lookup txIn utxo
  let txOutDataFromWits = do
        dataHash <- strictMaybeToMaybe (txOut ^. dataHashTxOutL)
        Map.lookup dataHash (tx ^. witsTxL . datsTxWitsL . unTxDatsL)
  strictMaybeToMaybe (txOut ^. dataTxOutL) <|> txOutDataFromWits
```

**Inline datum tried first**, witness-set fallback. UTXOW's `missingRequiredDatums` similarly accounts.

Inline datums in **output** TxOuts AND **referenced** TxOuts contribute to supplemental hashes:
```haskell
getBabbageSupplementalDataHashes (UTxO utxo) txBody =
  Set.fromList [dh | txOut <- outs, SJust dh <- [txOut ^. dataHashTxOutL]]
  where
    newOuts = map sizedValue $ toList $ txBody ^. allSizedOutputsTxBodyF
    referencedOuts = Map.elems $ Map.restrictKeys utxo (txBody ^. referenceInputsTxBodyL)
    outs = newOuts <> referencedOuts
```

## 5. Reference scripts

### Resolution (`getBabbageScriptsProvided`)
```haskell
getBabbageScriptsProvided utxo tx = ScriptsProvided ans
  where
    ins = (txBody ^. referenceInputsTxBodyL) `Set.union` (txBody ^. inputsTxBodyL)
    ans = getReferenceScripts utxo ins `Map.union` (tx ^. witsTxL . scriptTxWitsL)

getReferenceScripts utxo ins = Map.fromList $
  [ (hashScript script, script)
  | txOut <- Map.elems (Map.restrictKeys (unUTxO utxo) ins)
  , SJust script <- [txOut ^. referenceScriptTxOutL]
  ]
```

### Missing script witness check (`babbageMissingScripts`)
```haskell
neededNonRefs = sNeeded `Set.difference` sRefs        -- scripts needed but NOT from refs
missing       = neededNonRefs `Set.difference` sReceived  -- still missing in wits
extra         = sReceived `Set.difference` neededNonRefs   -- extraneous wits
```

Script covered by ref input need NOT appear in witness set.

### Well-formedness
Adds check: all witness scripts AND all reference scripts in tx outputs must pass `validScript pv`. Failures: `MalformedScriptWitnesses`, `MalformedReferenceScripts`.

### Reference script fee (Babbage: NONE)
`min_fee_ref_script_cost_per_byte` does not exist in Babbage. Introduced in Conway. Babbage `getMinFeeTxUtxo` = `getShelleyMinFeeTxUtxo pp tx` (no ref-script-byte charge).

## 6. Reference inputs — read-only

Properties:
1. **Not consumed** — no UTxO deletion
2. **Must exist** — in `allInputsTxBodyF`, subject to `validateBadInputsUTxO`
3. **No witness required** — no spending credential. `scriptsNeeded` ignores ref inputs for credentials.
4. **Inline datum/script access** — Plutus context (`PV2.TxInInfo`) gets full visibility (datum, value, address, ref script hash)

**PlutusV1 forbidden access**: `transTxOutV1` returns `Left (ReferenceScriptsNotSupported)` or `Left (InlineDatumsNotSupported)` for any referenced output with those features.

## 7. Collateral return

### ADA balance computation
```haskell
collAdaBalance txBody utxoCollateral = toDeltaCoin $
  case txBody ^. collateralReturnTxBodyL of
    SNothing -> colbal
    SJust txOut -> colbal <-> (txOut ^. coinTxOutL @era)
  where colbal = sumAllCoin utxoCollateral
```

### UTxO update on script failure
```haskell
let !(utxoKeep, utxoDel) = extractKeys (unUTxO utxo) (txBody ^. collateralInputsTxBodyL)
    UTxO collouts = collOuts txBody
    DeltaCoin collateralFees = collAdaBalance txBody utxoDel
in pure $! utxoState
  { utxosUtxo  = UTxO (Map.union utxoKeep collouts)
  , utxosFees  = utxosFees utxoState <> Coin collateralFees
  , utxosInstantStake = deleteInstantStake (UTxO utxoDel)
                           (addInstantStake (UTxO collouts) (utxoState ^. instantStakeL))
  }
```

Return output goes to TxIn index `length outputs`.

### Non-ADA collateral
Allowed if all non-ADA value goes to `collateralReturnTxBodyL`. `validateCollateralContainsNonADA` checks:
```haskell
totalCollateralBalance = case txBody ^. collateralReturnTxBodyL of
  SNothing     -> collateralBalance
  SJust retOut -> collateralBalance <-> (retOut ^. valueTxOutL @era)
-- Then: failureUnless (Val.isAdaOnly totalCollateralBalance) ...
```

### `total_collateral` enforcement
```haskell
validateCollateralEqBalance bal txcoll = case txcoll of
  SNothing -> pure ()
  SJust tc -> failureUnless (bal == toDeltaCoin tc) (IncorrectTotalCollateralField bal tc)
```

## 8. Era transition Alonzo → Babbage

`Translation.hs`, `PParams.hs`.

### Protocol version
HFC tick writes `ProtVer 7 0` into `curPParams` at boundary.

### Removed fields
```haskell
ppDG = to (const minBound)         -- d always effectively 0
hkdDL = notSupportedInThisEraL     -- lens fails on access
hkdExtraEntropyL = notSupportedInThisEraL
hkdMinUTxOValueCompactL = notSupportedInThisEraL
```

Downgrade carries:
```haskell
data DowngradeBabbagePParams f = DowngradeBabbagePParams
  { dbppD            :: !(HKD f UnitInterval)
  , dbppExtraEntropy :: !(HKD f Nonce)
  }
```
Carries Alonzo `d` but unused (Babbage+ always d=0).

### UTxO translation
```haskell
translateEra _ctxt utxo = pure $ UTxO $ upgradeTxOut `Map.map` unUTxO utxo
```
Each Alonzo `TxOut` → `BabbageTxOut` via `upgradeAlonzoTxOut`. Inline datums + ref scripts = SNothing/NoDatum for carried-over outputs.

### TPraos → Praos
HFC handles at Babbage boundary. Ledger unaware. Same VRF key type, different header body. See §1.

## 9. PParams delta

### Removed from Alonzo
| Alonzo field | Babbage |
|---|---|
| `appD` (decentralization) | removed |
| `appExtraEntropy` | removed |
| `appCoinsPerUTxOWord` | replaced by `bppCoinsPerUTxOByte` |
| `appMinUTxOValue` (Shelley) | removed |

### Key 17 semantics change
- **Alonzo key 17**: `coins_per_utxo_word` (ADA per 8-byte word)
- **Babbage key 17**: `coins_per_utxo_byte` (ADA per byte)

Conversion at era boundary:
```haskell
coinsPerUTxOWordToCoinsPerUTxOByte (CoinPerWord (Coin c)) =
  CoinPerByte . CompactCoin $ fromIntegral (c `div` 8)   -- /8, floored
```
Mainnet: `34482 / 8 = 4310`.

Special PPUP path: when a tx PPUP sets coinsPerUTxOWord, translated NAIVELY (no /8) to preserve intent:
```haskell
coinsPerUTxOWordToCoinsPerUTxOByteInTx (CoinPerWord (Coin c)) =
  CoinPerByte . toCompactPartial $ Coin c
```

### Babbage PParams (22 positional fields, vs Alonzo 25)
```
0=txFeePerByte, 1=txFeeFixed, 2=maxBBSize, 3=maxTxSize, 4=maxBHSize,
5=keyDeposit, 6=poolDeposit, 7=eMax, 8=nOpt, 9=a0, 10=rho, 11=tau,
12=protocolVersion, 13=minPoolCost, 14=coinsPerUTxOByte, 15=costModels,
16=prices, 17=maxTxExUnits, 18=maxBlockExUnits, 19=maxValSize,
20=collateralPercentage, 21=maxCollateralInputs
```

N2C V21+: `protocolVersion` encodes as `array(2)`. Wire array length depends on encoding.

### `min_fee_ref_script_cost_per_byte` — NOT in Babbage
Introduced in Conway (CIP-69 / PlutusV3 era). Babbage PParams decoder should reject/ignore if present.

## 10. PlutusV2 + UTXOS

### Language support
- V2 adds: reference inputs (`txInfoReferenceInputs`), inline datums in `TxOut`, ref script hashes
- V1 forbidden: any V1 invocation in tx with reference inputs OR inline datums fails context translation

```haskell
-- transTxOutV1 rejects:
when (isSJust (txOut ^. referenceScriptTxOutL)) ReferenceScriptsNotSupported
when (isSJust (txOut ^. dataTxOutL)) InlineDatumsNotSupported

-- transTxOutV2 supports all:
let datum = case txOut ^. datumTxOutF of
      NoDatum -> PV2.NoOutputDatum
      DatumHash dh -> PV2.OutputDatumHash $ transDataHash dh
      Datum binaryData -> PV2.OutputDatum . PV2.Datum . ...
    referenceScript = transReferenceScript $ txOut ^. referenceScriptTxOutL
```

### Cost models
`bppCostModels` holds V1 + V2. `ScriptIntegrityHash` covers cost model views for ALL languages used.

### UTXOS IsValid branching
```haskell
case tx ^. isValidTxL of
  IsValid True  -> babbageEvalScriptsTxValid    -- PPUP + scripts + UTxO update
  IsValid False -> do
    babbageEvalScriptsTxInvalid @era stAnnTx     -- verify scripts DO fail
    pure pup
-- Then updateUTxOStateByTxValidity handles UTxO surgery
```

## 11. BBODY rule

```haskell
type instance EraRuleFailure "BBODY" BabbageEra = Alonzo.AlonzoBbodyPredFailure BabbageEra
```

BBODY logic inherited from Alonzo. Babbage adds failure injection instances routing `BabbageUtxoPredFailure` + `BabbageUtxowPredFailure` through Alonzo wrapper. No new block-level checks.

## Rust translation notes for dugite

### Wire format
1. **TxBody**: decode as CBOR map. Keys 16, 17, 18 optional. Key 18 set of TxIn. Key 16 full TxOut in new map format. Key 17 u64 Coin.
2. **TxOut format dispatch**: `TypeMapLen[Indef]` → Babbage map; `TypeListLen[Indef]` → legacy array (2 or 3 elements). 3-element form carries datum hash at index 2.
3. **Inline datum (key 2 of map)**: 2-element `[tag, content]`. Tag 0 = hash (32 bytes), tag 1 = inline (`#6.24(bstr)`-encoded PlutusData).
4. **Reference script (key 3)**: `#6.24(bstr)` wrapping era-versioned `[script_type, script_body]`.
5. **Praos header**: 10-field structure; `hbVrfRes` single CertifiedVRF (not 2 separate). OCert nested array(4). ProtVer nested array(2).

### Validation sequence (IsValid = false in Alonzo/Babbage)
- Consume `collateral_inputs`
- Create `collateral_return` at index `len(outputs)` (if present)
- Fee delta = `sum(collateral_input_ADA) - collateral_return_ADA`
- If `total_collateral` present, fee delta must equal it
- Add net fee to `utxosFees`

Without `collateral_return`: all collateral ADA → fees, no new UTxO entry.

### Reference script lookup
1. Collect ref scripts from UTxOs at `inputs ∪ reference_inputs`
2. Spending credential script satisfied by (a) witness script with matching hash, OR (b) ref script with matching hash
3. Witness scripts neither needed nor covering ref are extraneous → fail

### MinUTxO
`min_utxo = (160 + serialized_txout_byte_size) * coins_per_utxo_byte`. 160 hardcoded. Byte size via `Sized`/`sizedSize` (versioned CBOR for the era).

### PParams
22-element positional array. `coinsPerUTxOByte` at logical index 14 (PPUP key 17). `d` doesn't exist — code accessing should use `0` (or `minBound` UnitInterval).
