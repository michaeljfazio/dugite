---
name: v1v2-txinfo-conway-babbage-gates
description: Exact ConwayContextError/BabbageContextError constructors and guard functions that reject V1/V2 Plutus TxInfo translation when Conway-only tx-body fields or Babbage-only output features are present; whole-tx CollectErrors semantics, not per-script skip
metadata:
  type: reference
---

# V1/V2 TxInfo Translation Gates (Conway + Babbage) — live-verified 2026-07-06

Source: IntersectMBO/cardano-ledger master commit `3448adc634eac8f97ec6616dc86a6c96dedab504`, verified live via cardano-haskell-oracle (not from this agent's static KB — escalate there for anything Plutus-TxInfo-translation-shaped, this topic is NOT covered by the pre-built oracle_ledger_*.md files).

## Conway: `ConwayContextError` + `guardConwayFeaturesForPlutusV1V2`

Location: `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`.

```haskell
data ConwayContextError era
  = BabbageContextError (BabbageContextError era)
  | CertificateNotSupported (TxCert era)
  | PlutusPurposeNotSupported (PlutusPurpose AsItem era)
  | CurrentTreasuryFieldNotSupported Coin
  | VotingProceduresFieldNotSupported (VotingProcedures era)
  | ProposalProceduresFieldNotSupported (OSet.OSet (ProposalProcedure era))
  | TreasuryDonationFieldNotSupported Coin
  | ReferenceInputsNotDisjointFromInputs (NonEmpty TxIn)
```

Guard (real name `guardConwayFeaturesForPlutusV1V2`), runs unconditionally at the top of BOTH `EraPlutusTxInfo 'PlutusV1 ConwayEra` and `'PlutusV2 ConwayEra` `toPlutusTxInfo` — fires for every V1/V2 script regardless of that script's own purpose:

```haskell
guardConwayFeaturesForPlutusV1V2 tx = do
  unless (null $ unVotingProcedures votingProcedures) $
    Left $ inject $ VotingProceduresFieldNotSupported @era votingProcedures
  unless (null proposalProcedures) $
    Left $ inject $ ProposalProceduresFieldNotSupported @era proposalProcedures
  unless (treasuryDonation == Coin 0) $
    Left $ inject $ TreasuryDonationFieldNotSupported @era treasuryDonation
  case currentTreasuryValue of
    SNothing -> Right ()
    SJust treasury -> Left $ inject $ CurrentTreasuryFieldNotSupported @era treasury
```

Field-gate semantics — NOT uniform:
- `votingProcedures` non-empty -> fail, both V1 and V2. Structural `null` check on the map (not purpose-scoped: a plain spending script still fails if the tx merely carries votes anywhere).
- `proposalProcedures` non-empty -> fail, both V1 and V2. Same `null` check.
- `currentTreasuryValue` -> fails on `SJust` of ANY value (structural presence, not value comparison).
- `treasuryDonation` -> the ONE field that's VALUE-gated not presence-gated: check is `treasuryDonation == Coin 0` (plain `Coin`, no `StrictMaybe` wrapper). `Coin 0` passes; any non-zero value fails. Do not implement this as "field present" — it must be "value != 0".

## `transPlutusPurposeV1V2` catch-all

Same file. Real purpose constructors: `ConwaySpending/ConwayMinting/ConwayCertifying/ConwayWithdrawing/ConwayVoting/ConwayProposing` (`VotingPurpose`/`ProposingPurpose` are pattern synonyms over the latter two). `ConwayVoting`/`ConwayProposing` fall through a catch-all `purpose -> Left $ inject $ PlutusPurposeNotSupported @era ...`. No "HardForkInitiation" purpose kind exists anywhere in the repo (Dijkstra's future 7th purpose is `DijkstraGuarding`/`GuardingPurpose`, unrelated, unreleased).

## Downstream consumption — WHOLE-TX rejection, not per-script skip

Chain: `getConwayScriptsNeeded` (Conway/UTxO.hs) -> `resolveNeededPlutusScriptsWithPurpose` -> `collectPlutusScriptsWithContext` (Alonzo/Plutus/Evaluate.hs) -> `scriptsWithContextFromLedgerTxInfoWithResult`. The `merge` combinator collapses ANY single script's `Left` (wrapped `first BadTranslation`) into an overall `Left (NonEmpty CollectError)` for the whole tx — it does NOT filter the offending script out of the list. Conway's UTXOS reuses Babbage's rule verbatim; both the `IsValid=True` (`expectScriptsToPass`) and `IsValid=False` (`babbageEvalScriptsTxInvalid`) branches check the scripts-with-context `Either` FIRST:

```haskell
(() <$ scriptsWithContextEither) ?!: (injectFailure . Alonzo.CollectErrors)
```

**CollectErrors is a hard whole-transaction predicate failure evaluated BEFORE the `IsValid` flag is consulted** — categorically different from `ValidationTagMismatch` (which IS `IsValid`-dependent and drives the collateral-consuming path). A V1/V2 translation failure (field-gate or purpose-gate) always rejects the entire tx from the block, never "silently skips the script and stays valid."

Confirmed by ledger's own ImpSpec: `eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/UtxosSpec.hs` (`conwayFeaturesPlutusV1V2FailureSpec`) uses `submitFailingTx` (whole-tx rejection assertion) with `injectFailure $ Alonzo.CollectErrors [BadTranslation errorField]`.

## Reachability: V1/V2 CAN be assigned governance-purpose credentials on-chain

`getConwayScriptsNeeded` (Conway/UTxO.hs) has no language restriction on DRep/CC voter credential scripts or `ParameterChange`/`TreasuryWithdrawals` guardrail script hashes — `ScriptHash`/`Credential` are language-agnostic in Phase-1. So a V1/V2 script CAN legally be registered as a DRep/CC hot credential or set as a governance-action guardrail script; Phase-1 witnessing does not block it. Consequence is exactly the Q2 mechanism: passes witnessing, then hard-fails the whole tx via `CollectErrors [BadTranslation (PlutusPurposeNotSupported ...)]` the instant that script is actually exercised for a vote/proposal purpose. (No ImpSpec test found exercising this exact non-V3 guardrail scenario by name, but code path is unambiguous.)

## Babbage: `BabbageContextError`, V1-only restrictions

Location: `eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs`.

```haskell
data BabbageContextError era
  = AlonzoContextError (AlonzoContextError era)
  | ByronTxOutInContext TxOutSource
  | RedeemerPointerPointsToNothing (PlutusPurpose AsIx era)
  | InlineDatumsNotSupported TxOutSource
  | ReferenceScriptsNotSupported TxOutSource
  | ReferenceInputsNotSupported (Set.Set TxIn)
```

`transTxOutV1` checks reference-script-present and inline-datum-present per TxOut (`TxOutSource`-scoped, i.e. names which specific output failed); a separate check in the V1 `EraPlutusTxInfo` instance does `unless (Set.null refInputs) $ Left (ReferenceInputsNotSupported refInputs)` for non-empty `referenceInputs` on the tx body. Constructor names confirmed exact.

V2 (`transTxOutV2`) has NO such guards at all — builds `PV2.TxOut` including `referenceScript`/`OutputDatum` directly, and unconditionally maps `referenceInputs` into `PV2.TxInfo`. Full exemption confirmed.

## CRITICAL CORRECTION vs a plausible-but-wrong assumption: Conway RELAXES the V1 `ReferenceInputsNotSupported` check itself (not just the Phase-1 disjointness rule), unconditionally, NOT PV-gated

Two SEPARATE mechanisms, do not conflate:

1. **Phase-1 UTXO disjointness rule** (`BabbageNonDisjointRefInputs`): inputs vs referenceInputs must be disjoint. Relaxed at PV>=11 for non-V3 scripts; V3 gets a new Phase-2 disjointness check instead (`checkReferenceInputsNotDisjointFromInputs`, gated `pvMajor >= 11`, surfaces as `ReferenceInputsNotDisjointFromInputs` inside `ConwayContextError` via `CollectErrors`).
   ```haskell
   whenMajorVersionAtMost @10 $ submitFailingTx consumingTx (pure . injectFailure $ Babbage.BabbageNonDisjointRefInputs badTxIns)
   whenMajorVersionAtLeast @11 $ when (lang > eraMaxLanguage @BabbageEra) $
     submitFailingTx @era consumingTx [injectFailure $ Alonzo.CollectErrors [BadTranslation . inject $ ReferenceInputsNotDisjointFromInputs @era badTxIns]]
   ```

2. **Babbage TxInfo-translation `ReferenceInputsNotSupported` V1 restriction**: Conway defines its OWN `EraPlutusTxInfo 'PlutusV1 ConwayEra` instance (distinct from Babbage's) in Conway/TxInfo.hs that DROPS the `unless (Set.null refInputs)` check entirely:
   ```haskell
   inputs <- mapM (transTxInInfoV1 ltiUTxO) (Set.toList (txBody ^. inputsTxBodyL))
   mapM_ (transTxInInfoV1 ltiUTxO) (Set.toList (txBody ^. referenceInputsTxBodyL))
   ```
   Each ref input is still translated by V1 output rules (still fails if THAT output has inline datum/ref script/Byron addr) then DISCARDED (`mapM_` not `mapM` — `PV1.TxInfo` has no `txInfoReferenceInputs` field). Net: in Conway, a V1 script CAN run in a tx with non-empty referenceInputs as long as none of those referenced outputs carry Babbage-only features; it just never sees them in its TxInfo. `ReferenceInputsNotSupported` (the Babbage constructor) is dead code from Conway's perspective — it is NOT PV-gated, true unconditionally from Conway genesis (PV9) onward, NOT specifically a PV11 thing.

Do not assume "V1 refInputs restriction persists forever, only the disjointness rule relaxed at PV11" — that assumption is WRONG. The TxInfo-level restriction was already gone in Conway (PV9/10), independent of the PV11 disjointness change.

## Dugite implication

dugite-ledger's V1/V2 ScriptContext/TxInfo builder currently has none of these gates (silently ignores Conway-only fields) — per cardano-haskell-oracle's fix guidance: implement as whole-tx hard rejection (equivalent to a `CollectErrors`-class failure), independent of `is_valid`, not a per-script skip. Needs: (1) Conway field gates (voting/proposal non-empty, currentTreasuryValue SJust, treasuryDonation != 0) on V1 AND V2 paths; (2) purpose-kind gate rejecting ConwayVoting/ConwayProposing redeemers against V1/V2 scripts; (3) Babbage-era V1 gates (inline datum / ref script / non-empty refInputs) scoped ONLY to pre-Conway eras — must NOT apply the refInputs-empty check to Conway's V1 path, since Conway's own V1 TxInfo instance has no such restriction.

Full agent research thread (in case of follow-up): cardano-haskell-oracle agent id `a65057d25fb606e68`.
