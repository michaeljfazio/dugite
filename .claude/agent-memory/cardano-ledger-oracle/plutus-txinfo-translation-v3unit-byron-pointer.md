---
name: plutus-txinfo-translation-v3unit-byron-pointer
description: Live-verified (2026-07-06) — PlutusV3 unit-return check (language-gated not PV-gated), Alonzo-drops/Babbage-errors on Byron addresses in TxInfo, StakingPtr translation unchanged across all eras including Conway
metadata:
  type: reference
---

Verified live against IntersectMBO/plutus @ master and IntersectMBO/cardano-ledger @ master (2026-07-06), answering a 4-question dugite audit (issue-audit, not yet filed as GH issues at time of writing).

## 1. PlutusV3 "must return Unit" IS an enforced ledger/evaluator rule — gated by ledger LANGUAGE, not by protocol version

Source: `plutus-ledger-api/src/PlutusLedgerApi/Common/Eval.hs`, `processLogsAndErrors` (shared helper inside `evaluateScriptCounting`/`evaluateScriptRestricting`):

```haskell
processLogsAndErrors ll logs res = do
  tell logs
  case res of
    UPLC.CekFailure err -> throwError $ CekError err
    -- If evaluation result is '()', then that's correct for all Plutus versions.
    UPLC.CekSuccessConstant (Some (ValueOf DefaultUniUnit ())) -> pure ()
    -- If evaluation result is any other constant or term, then it's only correct for V1 and V2.
    UPLC.CekSuccessConstant {} -> handleOldVersions
    UPLC.CekSuccessNonConstant {} -> handleOldVersions
  where
    handleOldVersions = unless (ll == PlutusV1 || ll == PlutusV2) $ throwError InvalidReturnValue
```

`EvaluationError` gained a constructor for this: `InvalidReturnValue` — `"The evaluation finished but the result value is not valid. Plutus V3 scripts must return BuiltinUnit. Returning any other value is considered a failure."`

Wiring proof that `ll` is a compile-time-fixed constant, not derived from the wire language byte at runtime: `PlutusLedgerApi.V3.evaluateScriptRestricting`/`evaluateScriptCounting` (`plutus-ledger-api/src/PlutusLedgerApi/V3.hs`) fix `thisLedgerLanguage = Common.PlutusV3` and pass it straight through to `Common.evaluateScriptRestricting thisLedgerLanguage ...`. cardano-ledger's `PlutusLanguage 'PlutusV3` instance (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs:509-528`) calls `PV3.evaluateScriptRestricting`/`evaluateScriptCounting` for every PV. **PlutusV4** (currently a placeholder, same file lines 530-548) reuses the exact same `PV3.evaluateScriptRestricting`/`evaluateScriptCounting` calls (so V4 will inherit the identical unit-check once it ships).

**There is no `MajorProtocolVersion`/`ifDecoderVersionAtLeast`-style branch anywhere in this check.** The single discriminator is `ll == PlutusV1 || ll == PlutusV2` — true for V1/V2 (any return value accepted, matching "success = CEK didn't throw"), false for V3 (and future V4) unconditionally, at every PV where that language is runnable (PV9+ for V3). So framing this as "PV-gated" is imprecise: it's **ledger-language-gated**, and since V3 doesn't exist before PV9, the *effective* behavior is "always enforced wherever V3 can run" — no historical PV had V3-without-the-check on any live network. Confirmed via `git log` on `plutus-ledger-api/src/PlutusLedgerApi/Common/Eval.hs`: the check was added in commit `d34e4d3f` ("Require PlutusV3 scripts to evaluate to BuiltinUnit (#6159)"), merged 2024-06-03 — **before** Chang#1 mainnet activation (epoch 507, ~2024-07-31), i.e. present from V3's very first day on mainnet. Changelog entry (`plutus-ledger-api/changelog.d/20240530_113312_unsafeFixIO_unit.md`): "`evaluateScriptRestricting` and `evaluateScriptCounting` now require Plutus V3 scripts to return `BuiltinUnit`, otherwise the evaluation is considered to have failed, and a `InvalidReturnValue` error is thrown. There is no such requirement on Plutus V1 and V2 scripts."

`InvalidReturnValue` is treated as an ordinary `EvaluationError` alongside `CekError` — from the ledger's perspective (`Cardano.Ledger.Plutus.Evaluate.evaluatePlutusWithContext`/`runPlutusScriptWithLogs`) it's indistinguishable from any other script failure: it flows into `Left evalError -> explainPlutusEvaluationError` → `ScriptFailure`, same downstream handling (`is_valid=false` territory) as a CEK error.

**Verdict: dugite's unconditional-per-PV V3 unit-check is CORRECT — matches Haskell exactly.** No divergence; do not change. (dugite site: `crates/dugite-uplc/src/eval_redeemer.rs:300-310`.)

## 2. Byron address in TxInfo: Alonzo silently drops, Babbage+ hard-errors — this is ERA-gated (dispatch on the era type param of `EraPlutusTxInfo l era`), not language-gated

**Alonzo** (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/TxInfo.hs`, `instance EraPlutusTxInfo 'PlutusV1 AlonzoEra`):
```haskell
-- | Translate a TxOut. Returns `Nothing` if a Byron address is present in the TxOut.
transTxOut :: (Value era ~ MaryValue, AlonzoEraTxOut era) => TxOut era -> Maybe PV1.TxOut
transTxOut txOut = do
  address <- transAddr (txOut ^. addrTxOutL)
  ...
```
```haskell
  toPlutusTxInfo proxy LedgerTxInfo {..} = ... do
    txInsMaybes <- forM (Set.toList (txBody ^. inputsTxBodyL)) $ toPlutusTxInInfo proxy ltiUTxO
    ...
    PV1.TxInfo
      { -- A mistake was made in Alonzo of filtering out Byron addresses, so we need to
        -- preserve this behavior by only retaining the Just case:
        PV1.txInfoInputs = catMaybes txInsMaybes
      , PV1.txInfoOutputs = mapMaybe transTxOut $ F.toList (txBody ^. outputsTxBodyL)
      , ...
```
A Byron-addressed input or output is silently **dropped** from `txInfoInputs`/`txInfoOutputs` — explicitly documented in-source as "a mistake" that must nonetheless be preserved for consensus compatibility.

**Babbage** (`eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs`, separate `instance EraPlutusTxInfo 'PlutusV1 BabbageEra` at line 314 AND `instance EraPlutusTxInfo 'PlutusV2 BabbageEra` at line 356 — both reuse the same strict path):
```haskell
-- | Given a TxOut, translate it for V2 and return (Right transalation).
-- If the transaction contains any Byron addresses or Babbage features, return Left.
transTxOutV1 :: ... -> Either (ContextError era) PV1.TxOut
transTxOutV1 txOutSource txOut = do
  ...
  case Alonzo.transTxOut txOut of
    Nothing -> Left $ inject $ ByronTxOutInContext @era txOutSource
    Just plutusTxOut -> Right plutusTxOut
```
Same shape for `transTxOutV2` (line ~125-151, also `Nothing -> Left $ inject $ ByronTxOutInContext @era txOutSource`). `ByronTxOutInContext` is a constructor of `BabbageContextError`, tagged with a `TxOutSource` (`TxOutFromInput txIn` / `TxOutFromOutput txIx`) recording where the offending Byron UTxO was found. This is a hard translation failure (`Left`), not a silent drop — it propagates through `ContextError era` and fails constructing the whole `TxInfo`/`ScriptContext`, which fails collecting that script's execution (feeds into `CollectError`/phase-2-adjacent tx failure), not merely a per-item omission.

**Conway reuses Babbage's functions unchanged**: `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs` imports `Cardano.Ledger.Babbage.TxInfo (transTxOutV2, transTxOutV1)` and calls `Babbage.transTxOutV2`/`Babbage.transTxInInfoV2` directly for V2 and V3 TxInfo building — Conway does not redefine or relax this. So V1/V2/V3 in Babbage-or-later all hard-error on Byron; only Alonzo (which only ever had V1) is lenient.

**Key clarification for the era-vs-language framing**: the discriminator is the era's `EraPlutusTxInfo l era` instance (dispatched on the `era` type, i.e. which HFC era the transaction executes under), NOT the Plutus language tag. Since V1 remains runnable in Babbage/Conway transactions, a V1 script witnessing a Babbage-era tx with a Byron TxOut gets the STRICT Babbage behavior (`ByronTxOutInContext`), even though the identical language tag (`PlutusV1`) got the LENIENT behavior under Alonzo. The "V1 vs V2/V3" framing is a red herring — Alonzo only ever had V1 available anyway, so era and "first language available" happen to coincide historically, but the actual Haskell dispatch key is the era instance.

**dugite finding (CONFIRMED DIVERGENCE)**: `crates/dugite-uplc/src/tx_info_populate.rs::address_to_plutus` (lines ~81-106) hard-errors (`Err(PhaseTwoError::Internal(...))`) on `PrimAddress::Byron` **unconditionally**, and this single function is shared by `populate_tx_info_v1`/`populate_tx_info_v2` (`crates/dugite-uplc/src/populate_v1_v2.rs`) with no era parameter threaded in at all — dugite cannot currently distinguish "V1 script in an Alonzo-era tx" (should drop-and-continue, matching the documented Haskell "mistake") from "V1 script in a Babbage/Conway-era tx" (should hard-fail, matching `ByronTxOutInContext`). Babbage+/Conway behavior is already correct; only the Alonzo-era case is wrong. Fix requires threading era (or an "is-legacy-Alonzo" flag) into the V1 TxInfo builder and doing list-level filtering (`catMaybes`-equivalent) instead of a single hard error, for that era only.

## 3. StakingPtr (pointer-credential) translation is IDENTICAL across every era including Conway — one shared function, never touched by the Conway pointer-deprecation changes

Source: `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs` (shared by ALL eras):
```haskell
transStakeReference :: StakeReference -> Maybe PV1.StakingCredential
transStakeReference (StakeRefBase cred) = Just (PV1.StakingHash (transCred cred))
transStakeReference (StakeRefPtr (Ptr (SlotNo32 slot) (TxIx txIx) (CertIx certIx))) =
  Just (PV1.StakingPtr (toInteger slot) (toInteger txIx) (toInteger certIx))
transStakeReference StakeRefNull = Nothing

-- | Translate an address. `Cardano.Ledger.BaseTypes.NetworkId` is discarded and Byron
-- Addresses will result in Nothing.
transAddr :: Addr -> Maybe PV1.Address
transAddr = \case
  AddrBootstrap {} -> Nothing
  Addr _networkId paymentCred stakeReference ->
    Just (PV1.Address (transCred paymentCred) (transStakeReference stakeReference))
```
This is the ONLY definition of `transAddr`/`transStakeReference` in the entire codebase. `Cardano.Ledger.Alonzo.Plutus.TxInfo` imports and calls it as-is (`transAddr (txOut ^. addrTxOutL)` inside `transTxOut`). `Cardano.Ledger.Babbage.TxInfo` imports it too (`transTxOutV2` calls `transAddr` directly). `Cardano.Ledger.Conway.TxInfo` does not even mention `transAddr`/`StakingPtr` in its own source — it calls `Babbage.transTxOutV2`/`Babbage.transTxInInfoV2` for both its V2 and V3 TxInfo builders, which internally reuse the exact same core `transAddr`. There is no PV/era conditional inside `transStakeReference` — `StakeRefPtr` always emits `PV1.StakingPtr slot txIx certIx` verbatim, unconditionally.

**Important scoping note**: Conway's removal of pointer-address *creation* (no new UTxOs may be assigned a `StakeRefPtr` going forward) and simplification of pointer *resolution against DState* for ledger-internal reward/deposit accounting are DELEG-rule/state-layer changes — completely separate from this TxInfo/ScriptContext translation function. Pre-Conway UTxOs that already carry a `StakeRefPtr` address remain spendable/referenceable in Conway transactions, and when a Plutus script observes such a UTxO, it still sees `StakingPtr{slot,txIx,certIx}` exactly as before.

**dugite finding (CONFIRMED CORRECT, no divergence)**: `crates/dugite-uplc/src/tx_info_populate.rs::pointer_to_plutus` does the same unconditional `PrimPointer{slot,tx_index,cert_index} -> StakingCredential::Pointer{slot,tx,cert}` passthrough with no era gate, for every address kind and every era. This already matches Haskell exactly. Do not add era-conditional handling here.

## Related
[[bounded-ratio-decode-and-enact-totality]] — sibling live-verified pass; that file now also carries the BoundedRatio/Rational REDUCTION fact (GHC `%` always reduces to lowest terms), which is the Q2 half of this same audit.
[[project_dugite_plutus_context_audit_2026_07_06]] — the audit ticket tracking the 2 confirmed dugite divergences (Byron-Alonzo drop semantics; Rational not reduced before ToPlutusData) found alongside these verified facts.
