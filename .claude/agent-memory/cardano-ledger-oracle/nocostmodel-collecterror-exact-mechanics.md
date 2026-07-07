---
name: nocostmodel-collecterror-exact-mechanics
description: Exact source for CollectError(NoCostModel)/collectPlutusScriptsWithContext — per-script language keying, hard-reject regardless of isValid, native-script exclusion (live-verified 2026-07-06)
metadata:
  type: reference
---

Live-verified against `IntersectMBO/cardano-ledger` @ master commit `3448adc634eac8f97ec6616dc86a6c96dedab504` (2026-07-03), via cardano-haskell-oracle. Extends the paraphrased summary in `oracle_ledger_validation.md` ("Script Collection Failures (CollectError)" section) with byte-exact quotes. Full write-up saved by the researching agent at `.claude/agent-memory/cardano-haskell-oracle/nocostmodel-collecterror-native-script-exclusion.md` — cross-link, don't duplicate.

## 1. Construction site
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs`, inside `apply` (used by `scriptsWithContextFromLedgerTxInfoWithResult`, called from `collectPlutusScriptsWithContext`):
```haskell
let lang = plutusScriptLanguage plutusScript
costModel <- maybe (Left (NoCostModel lang)) Right $ Map.lookup lang $ costModelsValid costModels
```
Plain `Either`-monad do-block, NOT `MaybeT`. `CollectError` sum type (`.../Alonzo/Plutus/Context.hs:344-349`):
```haskell
data CollectError era
  = NoRedeemer !(PlutusPurpose AsItem era)
  | NoWitness !ScriptHash
  | NoCostModel !Language
  | BadTranslation !(ContextError era)
```
CBOR `Summands` tags: NoRedeemer=0, NoWitness=1, NoCostModel=2, BadTranslation=3.

**Ordering gotcha (matters for wire-exact CollectErrors payload, not for accept/reject)**: errors are folded via `gg`, and `gg (Right _) (Left cs) = Left cs` — once ANY script in the list has already failed, `apply` is never invoked for later scripts. A later script's own `NoCostModel` can be silently absent from the reported `NonEmpty` if an earlier script failed for a different reason (e.g. `NoRedeemer`). Both success list and error list end up built via prepend (`c:cs` / `NonEmpty.cons`), i.e. in reverse of script-processing order.

## 2. Hard-reject regardless of IsValid — confirmed
Conway `ConwayUtxosPredFailure` (`Conway/Rules/Utxos.hs:53-63`) has `CollectErrors (NonEmpty (CollectError era))` as a distinct constructor from `ValidationTagMismatch IsValid TagMismatchDescription`. Conway's `utxosTransition` branches on `IsValid` first but delegates BOTH branches to Babbage's `expectScriptsToPass` / `babbageEvalScriptsTxInvalid` — no Conway override. Both contain identical guard code that runs BEFORE looking at script pass/fail:
```haskell
let scriptsWithContextEither = plutusScriptsWithContextStAnnTx stAnnTx
(() <$ scriptsWithContextEither) ?!: (injectFailure . Alonzo.CollectErrors)
Alonzo.when2Phase $
  whenFailureFree $
    forM_ scriptsWithContextEither $ \scriptsWithContext -> ...
```
`(?!:)` records a `Left` as an accumulated STS failure without aborting; `whenFailureFree` then skips `evalPlutusScripts` entirely if a failure is already recorded. So missing-cost-model hard-fails identically whether `IsValid True` or `IsValid False` — never demoted to a normal phase-2 script-failure/ValidationTagMismatch outcome. **This is the same "CollectErrors is a distinct hard-reject class from evalScripts Fails" fact already summarized in `oracle_ledger_validation.md`, now with the exact call chain.**

## 3. Per-script, not per-tx language check
`plutusScriptLanguage :: PlutusScript era -> Language` (`Alonzo/Scripts.hs:269-270`) extracts the `Language` singleton from that specific `PlutusScript` GADT value (tagged by its own `SLanguage l`), called once per resolved script inside `apply`. A tx with only PlutusV2 scripts never calls `Map.lookup PlutusV1 ...` at all — a missing V1 cost model is simply never observed.

## 4. Absence vs wrong-param-count — disjoint mechanisms
`CostModels` (`libs/cardano-ledger-core/.../Plutus/CostModels.hs:376-389`) = `{ _costModelsValid :: Map Language CostModel, _costModelsUnknown :: Map Word8 [Int64] }`. `NoCostModel` fires ONLY on total key absence from `_costModelsValid`. Wrong-param-count is handled at decode/`mkCostModel` time — short param lists get `maxBound`-padded by the Plutus smart constructor BEFORE the value ever enters the map, so `Map.lookup` still succeeds; `NoCostModel` never fires for that case.

## 5. Native scripts never enter this path
`AlonzoScriptsNeeded` is language-agnostic (includes native-script hashes). The Plutus-only filter is `resolveNeededPlutusScriptsWithPurpose` (`Alonzo/UTxO.hs:~422-428`), which uses `lookupPlutusScript sh scriptsProvided` (`Alonzo/Scripts.hs:759-764`) — `Map.lookup scriptHash >=> toPlutusScript`, and `toPlutusScript` returns `Nothing` for the `NativeScript`/Timelock constructor. A native-script hash is silently dropped by the `Just s <- [...]` list-comprehension filter — never reaches `costModelsValid` lookup at all. Native scripts validate via a wholly separate UTXOW path (`validateNativeScript`/`validateMultiSig`, Shelley `Rules/Utxow.hs:371`).

## Bonus: timing
`collectPlutusScriptsWithContext` doesn't run inside the UTXOS transition itself — it's precomputed once when building the `AlonzoStAnnTx` annotated-tx signal (`Alonzo.hs:112-148`, `mkAlonzoStAnnTx`). UTXOS just reads back the precomputed `asatPlutusScriptsWithContext` field. Conway doesn't override this construction.

## Rust translation notes (Dugite)
- `dugite-ledger` phase-2 collection should key cost-model lookup per-executed-script's own language tag, not on a tx-wide "languages present" set — mirrors point 3.
- The hard-reject-regardless-of-isValid semantics (point 2) means Dugite's `TxValidator` must treat `CollectErrors`-equivalent failures as a distinct pre-isValid-branch gate, not folded into the same code path as `ValidationTagMismatch`.
- The ordering gotcha in point 1 only matters if Dugite ever serializes/reports which CollectError(s) fired for wire-compatible diagnostics — for pure accept/reject it's irrelevant (any non-empty error list rejects).
- Point 5's filter mechanism (`toPlutusScript` returning `Nothing` for native scripts) is a good model for Dugite's own "scripts needed" resolution: filter to Plutus-tagged scripts before ever consulting cost models.
