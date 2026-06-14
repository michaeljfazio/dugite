---
name: babbage-conway-hf-ppup-order
description: Exact Babbage→Conway HFC era transition order of operations: PPUP application, translateEra, FuturePParams handling, updateCostModels semantics
type: reference
---

# Babbage→Conway Hard Fork: PParams Update Order & updateCostModels Semantics

## Key Source Files

- `libs/cardano-ledger-core/src/Cardano/Ledger/State/Governance.hs` — FuturePParams type, nextEpochPParams, solidifyFuturePParams
- `eras/conway/impl/src/Cardano/Ledger/Conway/Translation.hs` — TranslateEra instances for NewEpochState, EpochState, LedgerState, UTxOState, GovState (translateGovState)
- `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` — upgradeConwayPParams, updateCostModels call site
- `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/CostModels.hs` — updateCostModels definition
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Epoch.hs` — EPOCH rule: SNAP→POOLREAP→UPEC sequence
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Upec.hs` — UPEC: calls nextEpochPParams then NEWPP
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs` — votedFuturePParams calls applyPPUpdates

## FuturePParams Type

```haskell
data FuturePParams era
  = NoPParamsUpdate
  | DefinitePParamsUpdate !(PParams era)   -- set 2 stability windows before epoch end
  | PotentialPParamsUpdate (Maybe (PParams era))  -- lazy, may or may not apply
```

- `solidifyFuturePParams` converts `PotentialPParamsUpdate (Just pp) -> DefinitePParamsUpdate pp`
- `nextEpochPParams govState` returns `DefinitePParamsUpdate pp -> Just pp`, else `curPParams`
- `votedFuturePParams` calls `applyPPUpdates pp ppu` to produce the updated PParams

## PPUP Application Sequence (Babbage last epoch, pre-HF)

The EPOCH rule executes: SNAP → POOLREAP → UPEC

UPEC calls `nextEpochPParams ppupState` which extracts the `DefinitePParamsUpdate` PParams
(i.e., the result of `applyPPUpdates curPParams votedUpdate`).

This becomes `pp'` (the new `curPParams` after UPEC runs).

**So: the Babbage PPUP is applied during the EPOCH rule of the LAST BABBAGE epoch boundary,
BEFORE `translateEra` is called.**

## translateEra Order of Operations (Babbage→Conway)

At the hard fork transition, `translateEra @ConwayEra` is called on the state
AFTER the Babbage NewEpochState has already run its final NEWEPOCH rule
(including EPOCH which ran UPEC which applied the PPUP).

The translation chain:
1. `TranslateEra ConwayEra NewEpochState` → translates `nesEs` via `translateEra' ctxt`
2. `TranslateEra ConwayEra EpochState` → translates `esLState` (UTxOState)
3. `TranslateEra ConwayEra UTxOState` → translates `utxosGovState` via `translateGovState`
4. `translateGovState ctxt sgov`:
   - `curPParams = translateEra' ctxt (sgov ^. curPParamsGovStateL)` — the POST-PPUP Babbage PParams upgraded to Conway
   - `prevPParams = translateEra' ctxt (sgov ^. prevPParamsGovStateL)`
   - `futurePParams = translateEra' ctxt (sgov ^. futurePParamsGovStateL)` — translated (not discarded!)
   
5. `TranslateEra ConwayEra PParams` → calls `upgradePParams cgUpgradePParams babbage_pp`
6. `TranslateEra ConwayEra FuturePParams`:
   - `NoPParamsUpdate -> NoPParamsUpdate`
   - `DefinitePParamsUpdate pp -> DefinitePParamsUpdate <$> translateEra ctxt pp`
   - `PotentialPParamsUpdate mpp -> PotentialPParamsUpdate <$> mapM (translateEra ctxt) mpp`

## upgradeConwayPParams — Cost Model Merge

```haskell
cppCostModels =
  THKD $
    hkdLiftA2 @f
      updateCostModels
      bppCostModels                                -- first arg: OLD (Babbage cost models)
      ( hkdMap
          (Proxy @f)
          (mkCostModels . Map.singleton PlutusV3)
          ucppPlutusV3CostModel                    -- second arg: NEW (V3 from ConwayGenesis)
      )
```

## updateCostModels — Exact Semantics

```haskell
updateCostModels ::
  -- | Old CostModels that will be overwritten
  CostModels ->
  -- | New CostModels that will overwrite
  CostModels ->
  CostModels
updateCostModels (CostModels oldValid oldUnk) (CostModels modValid modUnk) =
  CostModels
    newValid
    (Map.union modUnk oldUnk Map.\\ Map.mapKeys languageToWord8 newValid)
  where
    newValid = Map.union modValid oldValid
```

**Argument mapping in upgradeConwayPParams:**
- `oldValid` = `bppCostModels` (Babbage V1+V2 cost models, possibly updated by PPUP)
- `modValid` = `Map.singleton PlutusV3 v3_model` (from ConwayGenesis)

**`Map.union` is LEFT-biased in Haskell**: `Map.union modValid oldValid` means `modValid` wins on key collision.

Therefore:
- For PlutusV3: always comes from ConwayGenesis (modValid), since Babbage cannot have V3
- For PlutusV1, PlutusV2: comes from `oldValid` (bppCostModels = Babbage PPUP-applied models)
- If Babbage PPUP had updated V1 or V2, those UPDATED models survive into Conway

**The claim "V3 from genesis survives even if the PPUP had no V3 entry" is CORRECT.**
- Babbage PPUP CANNOT contain PlutusV3 entries (Babbage era does not support V3)
- The V3 model always comes from ConwayGenesis.cgUpgradePParams.ucppPlutusV3CostModel
- Map.union puts modValid (V3-only map) on the LEFT, so V3 wins; but V3 was never in oldValid anyway

## Can Babbage PPUP Carry PlutusV3?

No. The Babbage PParamsUpdate type's costModels field is `CostModels` but:
- The Babbage era only knows about PlutusV1 and PlutusV2
- A V3 entry in a Babbage PPUP would land in `_costModelsUnknown` (the `Map Word8 [Int64]` field)
- `updateCostModels` specifically clears unknown entries when a valid entry exists for the same language
- The V3 unknown entry would be REMOVED from unknowns once V3 is validated in Conway

## Summary: The Original Claim

"Haskell applies the Babbage PPUP FIRST, THEN translateEra runs upgradeConwayPParams → updateCostModels,
which does Map.union {V3_from_genesis} {V1_new, V2_new_from_PPUP}, so V3 from genesis survives"

**STATUS: CORRECT**, with one clarification:
- The Map.union is `Map.union modValid oldValid` where modValid={V3_genesis}, oldValid={V1_ppup, V2_ppup}
- Since there is no V3 key collision (Babbage can't have valid V3), genesis V3 always wins
- V1/V2 from PPUP (oldValid) survive because they are only keys in oldValid, not in modValid
- The order is: (1) PPUP applied in Babbage EPOCH/UPEC, (2) translateEra called on post-PPUP state, (3) upgradeConwayPParams merges genesis V3 with PPUP-updated V1/V2
