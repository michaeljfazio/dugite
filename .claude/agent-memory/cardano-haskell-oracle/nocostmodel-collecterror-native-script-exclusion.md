---
name: nocostmodel-collecterror-native-script-exclusion
description: Exact source for the NoCostModel CollectError path (missing-language rejection), per-script language keying, and proof native/Timelock scripts never enter the CostModels lookup at all
type: reference
---

Verified live 2026-07-06 against IntersectMBO/cardano-ledger `master` (same session as
[[v1v2-scriptcontext-conway-gates]], which covers the general `CollectErrors`/`merge` hard-reject
mechanism in depth — this memory is the `NoCostModel`-specific companion).

## 1. NoCostModel construction site

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs`, `apply` helper inside
`scriptsWithContextFromLedgerTxInfoWithResult`:
```haskell
apply (plutusScript, plutusPurpose, redeemerData, exUnits, plutusScriptHash) = do
  let lang = plutusScriptLanguage plutusScript
  costModel <- maybe (Left (NoCostModel lang)) Right $ Map.lookup lang $ costModelsValid costModels
  first BadTranslation $
    mkPlutusWithContext
      plutusScript plutusScriptHash plutusPurpose lti txInfoResult
      (redeemerData, exUnits) costModel
```
Not `MaybeT` — plain `maybe (Left ...) Right $ Map.lookup ...` inside an `Either`-monad do-block.

`CollectError` sum type, `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Context.hs:344-349`:
```haskell
data CollectError era
  = NoRedeemer !(PlutusPurpose AsItem era)
  | NoWitness !ScriptHash
  | NoCostModel !Language
  | BadTranslation !(ContextError era)
  deriving (Generic)
```
CBOR tags (Summands, same file, `encCBOR`/`decCBOR`): `NoRedeemer`=0, `NoWitness`=1,
`NoCostModel`=2, `BadTranslation`=3. Names/tags current as of this session — no renames since
Alonzo introduced this type.

## 2. Hard-reject regardless of IsValid — confirmed for NoCostModel specifically

Conway's `ConwayUtxosPredFailure` (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxos.hs:53-63`):
```haskell
data ConwayUtxosPredFailure era
  = ValidationTagMismatch IsValid Alonzo.TagMismatchDescription
  | CollectErrors (NonEmpty (CollectError era))
```
Conway's `utxosTransition` (same file, lines 218-224) branches on `IsValid` FIRST, but BOTH
branches independently re-check `plutusScriptsWithContextStAnnTx` for `CollectErrors` before ever
looking at script results — Conway just delegates verbatim to Babbage's implementation:
```haskell
utxosTransition = judgmentContext >>= \(TRC ((), (), stAnnTx)) -> do
  let tx = stAnnTx ^. txStAnnTxG
  case tx ^. isValidTxL of
    IsValid True -> conwayEvalScriptsTxValid       -- calls Babbage.expectScriptsToPass
    IsValid False -> Babbage.babbageEvalScriptsTxInvalid @era stAnnTx
```
Both `Babbage.expectScriptsToPass` and `Babbage.babbageEvalScriptsTxInvalid`
(`eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxos.hs:139-222`) contain IDENTICAL code:
```haskell
let scriptsWithContextEither = plutusScriptsWithContextStAnnTx stAnnTx
(() <$ scriptsWithContextEither) ?!: (injectFailure . Alonzo.CollectErrors)
Alonzo.when2Phase $ whenFailureFree $ forM_ scriptsWithContextEither $ \scriptsWithContext -> ...
```
`(?!:)` (`libs/small-steps/src/Control/State/Transition/Extended.hs:452-454`) turns a `Left e` into
an accumulated STS `Predicate` failure via `Validation`/`eitherToValidation` — it does NOT abort the
monadic `do` block, it just marks the transition as failed. `whenFailureFree` (same file,
482-486, via `ifFailureFree`) then checks the accumulator: if a failure is ALREADY recorded (e.g.
the `CollectErrors` just added), it skips running `evalPlutusScripts` entirely — script evaluation
never happens when cost model is missing. Net effect: a missing cost model hard-rejects the whole
UTXOS transition (hence whole tx) via `CollectErrors`, identically whether the tx declares
`IsValid True` or `IsValid False` — never demoted to a normal `ValidationTagMismatch`/phase-2
script-failure outcome.

Also note (subtle, `scriptsWithContextFromLedgerTxInfoWithResult`'s `merge`/`gg`, same as
[[v1v2-scriptcontext-conway-gates]]): once ANY error occurs while folding the script list
left-to-right, `apply` (hence the cost-model lookup) is NEVER called again for later scripts in the
list — `gg (Right _) (Left cs) = Left cs` discards the later item's own result without evaluating
it. So if script #1 hits `NoRedeemer` and script #2 (later in the list) would have hit
`NoCostModel`, the reported `NonEmpty CollectError` is `[NoRedeemer #1]` only — `NoCostModel #2` is
never discovered/reported. Doesn't change the reject-or-not outcome, but DOES change the exact
`NonEmpty (CollectError era)` payload inside the `CollectErrors` predicate failure (byte-relevant
for CBOR of rejection reasons / ImpSpec conformance vectors, ties to
[[msgrejecttx-wire-format]]-adjacent concerns). Also note both the success-list and the
accumulated failure list come out in REVERSE of script-processing order (`c : cs` / `NonEmpty.cons
a cs` both prepend), so element order in the encoded `NonEmpty` is the reverse of tx script order
when there are 2+ errors.

## 3. Per-script language keying (NOT a whole-tx/whole-era check)

`plutusScriptLanguage` (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Scripts.hs:269-270`):
```haskell
plutusScriptLanguage :: AlonzoEraScript era => PlutusScript era -> Language
plutusScriptLanguage ps = withPlutusScript ps plutusLanguage
```
Pulls the `Language` singleton directly out of that specific `PlutusScript era` GADT value (each
`PlutusScript` wraps a `Plutus l` tagged by its own `SLanguage l`). Called once per resolved
script inside `apply`, so a tx containing ONLY PlutusV2 scripts never looks up `PlutusV1` in
`costModelsValid` — V1's absence from the map is simply never observed.

## 4. Absence vs wrong-param-count — genuinely different mechanisms

`CostModels` (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/CostModels.hs:376-388`):
```haskell
data CostModels = CostModels
  { _costModelsValid :: !(Map Language CostModel)
  , _costModelsUnknown :: !(Map Word8 [Int64])
  }
costModelsValid :: CostModels -> Map Language CostModel
costModelsValid = _costModelsValid
```
`NoCostModel` fires ONLY when `Map.lookup lang (_costModelsValid costModels) == Nothing` — i.e.
the `Language` key is wholly absent from the map (never populated, e.g. protocol params never set
a cost model for that language, or a lenient decode dropped it into `_costModelsUnknown` instead).
Wrong-param-count is a completely different, EARLIER mechanism: `CostModel`'s own doc comment
(same file, ~line 103-113) says values are retained as-is (`cmValues :: [Int64]`) and "When less
than the expected number is supplied, `maxBound` will be used instead by the Plutus smart
constructor" (`mkEvaluationContext`) at `mkCostModel`/decode time — this produces a `CostModel`
that DOES exist in the map (just built from padded/truncated params), so `Map.lookup` still
succeeds and `NoCostModel` never fires for that case.

## 5. Native scripts never reach this path at all

`AlonzoScriptsNeeded` (built by `getAlonzoScriptsNeeded`,
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs`) is language-agnostic — it's populated purely
from `ScriptHash`es extracted from spending/withdrawing/certifying/minting purposes, regardless of
whether the hash resolves to a Plutus or native script. The Plutus-only filter happens one step
later, in `resolveNeededPlutusScriptsWithPurpose` (same file, ~422-428):
```haskell
resolveNeededPlutusScriptsWithPurpose (ScriptsProvided scriptsProvided) (AlonzoScriptsNeeded scriptsNeeded) =
  [(sh, sp, s) | (sp, sh) <- scriptsNeeded, Just s <- [lookupPlutusScript sh scriptsProvided]]
```
`lookupPlutusScript` (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Scripts.hs:759-764`):
```haskell
-- | ... Returns Nothing when script is missing or it is not a PlutusScript
lookupPlutusScript scriptHash = Map.lookup scriptHash >=> toPlutusScript
```
`toPlutusScript` default method (`AlonzoEraScript` class, same file ~181-184):
```haskell
toPlutusScript :: Script era -> Maybe (PlutusScript era)
toPlutusScript = \case
  PlutusScript ps -> Just ps
  _ -> Nothing        -- NativeScript falls through here
```
So a native (Timelock/MultiSig) script hash makes `lookupPlutusScript` return `Nothing`, and the
list-comprehension's `Just s <- [...]` pattern-match silently drops that entry — it never becomes
part of `plutusScriptsUsed`/`neededPlutusScripts`, never reaches `scriptsWithContextFromLedgerTxInfo`,
never triggers a `costModelsValid` lookup. Native scripts are validated entirely separately via
`validateNativeScript` (class method, e.g. `validateMultiSig` for Shelley/Allegra Timelock,
`eras/shelley/impl/src/Cardano/Ledger/Shelley/Tx.hs:222-223`), called from
`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs:371` — a wholly separate UTXOW
code path that never touches `CostModels`.

## Where collectPlutusScriptsWithContext actually runs (timing detail)

It's called once, up front, when constructing the `AlonzoStAnnTx` "annotated tx" signal — NOT
inside the UTXOS transition itself. See `mkAlonzoStAnnTx`,
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo.hs:112-148`:
```haskell
mkAlonzoStAnnTx ei sysStart pp utxo tx =
  let scriptsNeeded = getScriptsNeeded utxo (tx ^. bodyTxL)
      scriptsProvided = getScriptsProvided utxo tx
      plutusScriptsUsed = resolveNeededPlutusScriptsWithPurpose scriptsProvided scriptsNeeded
      ledgerTxInfo = LedgerTxInfo { ... }
   in AlonzoStAnnTx
        { asatTx = tx
        , asatScriptsNeeded = scriptsNeeded
        , asatScriptsProvided = scriptsProvided
        , asatPlutusLanguagesUsed = Set.fromList [plutusScriptLanguage s | (_, _, s) <- plutusScriptsUsed]
        , asatPlutusScriptsWithContext =
            scriptsWithContextFromLedgerTxInfo ledgerTxInfo (pp ^. ppCostModelsL) plutusScriptsUsed
        }
```
UTXOS (`plutusScriptsWithContextStAnnTx`) just reads this pre-computed field back out
(`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs:195`) — Conway does not override this
construction, only `AlonzoEraUTxO era` field-accessor instance differs per era (Conway's own
`getScriptsNeeded`/`getConwayScriptsNeeded` in `eras/conway/impl/src/Cardano/Ledger/Conway/UTxO.hs`
adds governance-purpose script hashes, but the Plutus-vs-native filter step is identical/reused).
