---
name: v1v2-scriptcontext-conway-gates
description: Exact mechanism + source quotes for V1/V2 Plutus ScriptContext/TxInfo translation gating in Conway (guardConwayFeaturesForPlutusV1V2, transPlutusPurposeV1V2, CollectErrors propagation); resolves the "silently skip vs whole-tx reject" question
type: reference
---

Verified live 2026-07-06 against IntersectMBO/cardano-ledger `master` @ `3448adc634eac8f97ec6616dc86a6c96dedab504`.

## Two independent V1/V2 gates in Conway (both funnel to the SAME hard failure)

1. **Field-presence gate** — `guardConwayFeaturesForPlutusV1V2` in
   `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`. Called unconditionally at
   the top of BOTH the `EraPlutusTxInfo 'PlutusV1 ConwayEra` and `'PlutusV2 ConwayEra`
   `toPlutusTxInfo` instances — i.e. it fires whenever ANY V1/V2 script needs to run in
   the tx, regardless of that script's own purpose (even a plain spending script fails
   if the tx body ALSO happens to carry a non-empty votingProcedures/proposalProcedures/
   treasuryDonation or an SJust currentTreasuryValue).
   ```haskell
   guardConwayFeaturesForPlutusV1V2 tx = do
     let txBody = tx ^. bodyTxL
         currentTreasuryValue = txBody ^. currentTreasuryValueTxBodyL
         votingProcedures = txBody ^. votingProceduresTxBodyL
         proposalProcedures = txBody ^. proposalProceduresTxBodyL
         treasuryDonation = txBody ^. treasuryDonationTxBodyL
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
   Guard semantics: votingProcedures/proposalProcedures use `null` (empty-collection
   check); treasuryDonation uses `== Coin 0` (value check, not `isJust`); currentTreasuryValue
   uses `SNothing`/`SJust` pattern match — ANY `SJust` fails regardless of the wrapped
   amount (even `SJust (Coin 0)` would fail — it's structural presence, not `> 0`).

2. **Purpose-kind gate** — `transPlutusPurposeV1V2`, same file. Only Spending/Minting/
   Certifying/Withdrawing purposes are handled (delegated to Alonzo's translator); every
   other `PlutusPurpose` constructor — i.e. `ConwayVoting` and `ConwayProposing` (the real
   GADT constructors backing pattern synonyms `VotingPurpose`/`ProposingPurpose`, see
   `eras/conway/impl/src/Cardano/Ledger/Conway/Scripts.hs`) — hits the catch-all:
   ```haskell
   transPlutusPurposeV1V2 proxy pv = \case
     SpendingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoSpending asIxItem
     MintingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoMinting asIxItem
     CertifyingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoCertifying asIxItem
     WithdrawingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoWithdrawing asIxItem
     purpose -> Left $ inject $ PlutusPurposeNotSupported @era $ hoistPlutusPurpose toAsItem purpose
   ```
   This fires ONLY when the V1/V2 script itself is assigned VotingPurpose/ProposingPurpose;
   it does NOT depend on any of guard (1)'s field checks.

## ConwayContextError — full constructor set + CBOR tags (encCBOR in same file)
```
data ConwayContextError era
  = BabbageContextError (BabbageContextError era)              -- tag 8
  | CertificateNotSupported (TxCert era)                        -- tag 9
  | PlutusPurposeNotSupported (PlutusPurpose AsItem era)         -- tag 10  (fires for ConwayVoting/ConwayProposing/any non-{Spend,Mint,Cert,Withdraw})
  | CurrentTreasuryFieldNotSupported Coin                        -- tag 11
  | VotingProceduresFieldNotSupported (VotingProcedures era)     -- tag 12
  | ProposalProceduresFieldNotSupported (OSet.OSet (ProposalProcedure era)) -- tag 13
  | TreasuryDonationFieldNotSupported Coin                       -- tag 14
  | ReferenceInputsNotDisjointFromInputs (NonEmpty TxIn)         -- tag 15 (V3-only, PV>=11, see below)
```
`BabbageContextError` (`eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs`), tags 0/1/2/4/5/6/7 (3 is a gap — retired constructor):
```
data BabbageContextError era
  = AlonzoContextError (AlonzoContextError era)     -- tag 1 (TranslationLogicMissingInput) / tag 7 (TimeTranslationPastHorizon)
  | ByronTxOutInContext TxOutSource                 -- tag 0
  | RedeemerPointerPointsToNothing (PlutusPurpose AsIx era) -- tag 2
  | InlineDatumsNotSupported TxOutSource            -- tag 4
  | ReferenceScriptsNotSupported TxOutSource        -- tag 5
  | ReferenceInputsNotSupported (Set.Set TxIn)      -- tag 6 (Babbage V1 ONLY — see relaxation note below)
```

## Consumption path: translation failure = WHOLE-TX hard reject, not "skip this script"

`collectPlutusScriptsWithContext` (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs`)
→ `scriptsWithContextFromLedgerTxInfoWithResult`'s `merge` combinator: the very first `Left`
(from any script's `mkPlutusWithContext`, wrapped as `BadTranslation`) collapses the ENTIRE
`Either (NonEmpty CollectError) [PlutusWithContext]` result to `Left`. It is NOT a per-script
filter — one bad script poisons the whole collection result:
```haskell
gg (Right t) (Right cs) = case f t of Right c -> Right (c:cs); Left e -> Left [e]
gg (Left a) (Right _) = Left [a]
gg (Right _) (Left cs) = Left cs
gg (Left a) (Left cs) = Left (NonEmpty.cons a cs)
```
UTXOS (`eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxos.hs`, reused verbatim by Conway
via `Babbage.expectScriptsToPass` / `Babbage.babbageEvalScriptsTxInvalid` in
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxos.hs`) checks this BEFORE branching on
the tx's declared `IsValid` flag, in BOTH branches:
```haskell
let scriptsWithContextEither = plutusScriptsWithContextStAnnTx stAnnTx
(() <$ scriptsWithContextEither) ?!: (injectFailure . Alonzo.CollectErrors)
```
This is identical code in `expectScriptsToPass` (IsValid=True path) and
`babbageEvalScriptsTxInvalid` (IsValid=False path) — a translation failure hard-fails UTXOS
with `CollectErrors` REGARDLESS of the tx's declared IsValid flag. This is categorically
different from a normal Phase-2 script-evaluation failure (`ValidationTagMismatch`, which DOES
depend on IsValid and is the collateral-consuming "phase-2 invalid" path) — CollectErrors means
the transaction cannot be validly included in a block AT ALL, full stop.

Confirmed empirically by the ledger team's own ImpSpec tests in
`eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/UtxosSpec.hs`
(`conwayFeaturesPlutusV1V2FailureSpec`, helper `testPlutusV1V2Failure`): every case uses
`submitFailingTx` (asserts the whole tx is REJECTED by the ledger STS) with
`injectFailure $ Alonzo.CollectErrors [BadTranslation errorField]` — never a "tx still valid,
script excluded" outcome. No test in the repo exercises `PlutusPurposeNotSupported` for
Voting/Proposing directly (only the field-presence gate is covered by name), but the source
path is unambiguous and structurally identical.

## Reachability (Q3): V1/V2 CAN be assigned Voting/Proposing purposes on-chain

`getConwayScriptsNeeded` (`eras/conway/impl/src/Cardano/Ledger/Conway/UTxO.hs`) builds the
needed-scripts list with NO language restriction on either DRep/CC voter credential scripts
or governance-action guardrail/policy scripts:
```haskell
votingScriptsNeeded = ... getVoterScriptHash voter
  where getVoterScriptHash = \case
          CommitteeVoter cred -> credScriptHash cred
          DRepVoter cred -> credScriptHash cred
          StakePoolVoter _ -> Nothing
proposingScriptsNeeded = ... getProposalScriptHash proposal
  where getProposalScriptHash ProposalProcedure {pProcGovAction} = case pProcGovAction of
          ParameterChange _ _ (SJust guardrailsScriptHash) -> Just guardrailsScriptHash
          TreasuryWithdrawals _ (SJust guardrailsScriptHash) -> Just guardrailsScriptHash
          _ -> Nothing
```
`credScriptHash`/`ScriptHash` are language-agnostic — nothing in Phase-1 (scriptsNeeded,
witnessing) restricts these to PlutusV3. A V1 or V2 script CAN be registered as a DRep/CC hot
credential or set as a ParameterChange/TreasuryWithdrawals guardrail script; it will pass
Phase-1 witnessing, then hard-fail at UTXOS via `CollectErrors [BadTranslation
(PlutusPurposeNotSupported ...)]` the moment it's actually invoked for a vote/proposal. The
ledger's own testlib (`govPolicySpec`) only ever exercises `SPlutusV3` for govPolicy scripts —
V1/V2 govPolicy is real-world dead-on-arrival by construction, not by an explicit type/era gate.

## Surprising Q4(c)-adjacent finding: Conway RELAXED TWO of Babbage's three V1 restrictions (unconditionally, all Conway PVs, NOT PV11-gated) — kept the third

Conway defines its OWN module-local `transTxOutV1`/`transTxInInfoV1` in
`eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs` (both exported from that module, lines
~301-329) — DIFFERENT functions from Babbage's same-named `transTxOutV1`/`transTxInInfoV1` in
`eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs` (used only by Babbage's own instance).
Re-verified exact source 2026-07-06 (same commit):
```haskell
-- Conway's transTxOutV1 (module-local override, NOT imported from Babbage)
transTxOutV1 txOutSource txOut = do
  when (isSJust (txOut ^. dataTxOutL)) $ do
    Left $ inject $ InlineDatumsNotSupported @era txOutSource
  case Alonzo.transTxOut txOut of
    Nothing -> Left $ inject $ ByronTxOutInContext @era txOutSource
    Just plutusTxOut -> Right plutusTxOut

transTxInInfoV1 utxo txIn = do
  txOut <- left (inject . AlonzoContextError @era) $ Alonzo.transLookupTxOut utxo txIn
  plutusTxOut <- transTxOutV1 (TxOutFromInput txIn) txOut
  Right (PV1.TxInInfo (TxInfo.transTxIn txIn) plutusTxOut)
```
vs Babbage's `transTxOutV1` (kept unchanged there):
```haskell
transTxOutV1 txOutSource txOut = do
  when (isSJust (txOut ^. referenceScriptTxOutL)) $ do
    Left $ inject $ ReferenceScriptsNotSupported @era txOutSource
  when (isSJust (txOut ^. dataTxOutL)) $ do
    Left $ inject $ InlineDatumsNotSupported @era txOutSource
  case Alonzo.transTxOut txOut of ...
```
**Conway drops the `referenceScriptTxOutL` check entirely** — a V1 script in Conway can freely
touch (as regular input, reference input, or produced output) a UTxO entry carrying a reference
script; `ReferenceScriptsNotSupported` (tag 5) is Babbage-only dead code from Conway's
perspective, same fate as `ReferenceInputsNotSupported` (tag 6, below). **Conway KEEPS the
`dataTxOutL`/`InlineDatumsNotSupported` check** (tag 4) — a V1 script in Conway still hard-fails
if it touches any UTxO entry carrying an inline datum. This applies uniformly since regular
inputs, reference inputs (discarded), and outputs ALL funnel through this one Conway-local
`transTxOutV1`:
```haskell
inputs <- mapM (transTxInInfoV1 ltiUTxO) (Set.toList (txBody ^. inputsTxBodyL))
mapM_ (transTxInInfoV1 ltiUTxO) (Set.toList (txBody ^. referenceInputsTxBodyL))
outputs <- zipWithM (transTxOutV1 . TxOutFromOutput) [minBound ..] (F.toList (txBody ^. outputsTxBodyL))
```
Babbage V1 additionally had a blanket check that Conway also drops — any non-empty
`referenceInputsTxBodyL` at all:
```haskell
-- Babbage EraPlutusTxInfo 'PlutusV1 BabbageEra
let refInputs = txBody ^. referenceInputsTxBodyL
unless (Set.null refInputs) $ Left (ReferenceInputsNotSupported refInputs)
```
Conway has no equivalent blanket check; it just translates-and-discards each ref input via
`mapM_` above (fails only if that specific referenced output has an inline datum or Byron
address — NOT for having a reference script, per the relaxed `transTxOutV1`, and NOT for the
mere fact of being a reference input). The translated `PV1.TxInInfo` for a reference input
never appears in `PV1.TxInfo` either way (`mapM_`, and `PV1.TxInfo` has no
`txInfoReferenceInputs` field at all — that's V2/V3-only).

Net effect, none of this gated by `pvMajor` (true for Conway PV9/10/11+ alike, unlike
`ReferenceInputsNotDisjointFromInputs` below): **2 of Babbage's 3 V1 restrictions
(`ReferenceInputsNotSupported` tag 6, `ReferenceScriptsNotSupported` tag 5) are dead in Conway;
only `InlineDatumsNotSupported` (tag 4) survives.** This overturns the plausible-sounding
assumption that Babbage's V1 restrictions carry over unchanged into Conway.

Separately, `checkReferenceInputsNotDisjointFromInputs` (ConwayContextError tag 15,
`ReferenceInputsNotDisjointFromInputs`) is a DIFFERENT, PlutusV3-only, PV>=11-gated Phase-2
check (added per CHANGELOG "Add `checkReferenceInputsNotDisjointFromInputs`", cardano-ledger-conway
1.23.0.0) — confirmed genuinely separate from the Phase-1 UTXO-level disjointness relaxation
(`Babbage.BabbageNonDisjointRefInputs`, checked `whenMajorVersionAtMost @10`) per
`eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/UtxosSpec.hs`:
```haskell
whenMajorVersionAtMost @10 $
  submitFailingTx consumingTx (pure . injectFailure $ Babbage.BabbageNonDisjointRefInputs badTxIns)
whenMajorVersionAtLeast @11 $
  when (lang > eraMaxLanguage @BabbageEra) $  -- i.e. only for PlutusV3
    submitFailingTx @era consumingTx
      [injectFailure $ Alonzo.CollectErrors [BadTranslation . inject $ ReferenceInputsNotDisjointFromInputs @era badTxIns]]
```
So at PV>=11: the Phase-1 UTXO disjointness rule is gone for ALL languages (V1/V2/V3/native);
V3 scripts get a NEW Phase-2 replacement check (fails if inputs/refInputs overlap); V1/V2 get
NO disjointness check of any kind post-PV11 (consistent with V1 already ignoring refInputs
content per above, and V2 never having restricted them).

## Dijkstra (future/unreleased) purpose kinds — not relevant to current Conway fix

`eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Scripts.hs` adds a 7th `DijkstraPlutusPurpose`
constructor, `DijkstraGuarding` (pattern `GuardingPurpose`, `f Word32 ScriptHash`) — NOT a
"HardForkInitiation" purpose kind (no such constructor exists anywhere). Dijkstra is an
unreleased future era; irrelevant to Conway/PV11 byte-exact work.

## Reconciliation of the two prior investigations
Investigation A was right about the MECHANISM (`transPlutusPurposeV1V2` really does return
`Left (inject (PlutusPurposeNotSupported ...))` for ConwayVoting/ConwayProposing against a
V1/V2 script) but WRONG about the CONSEQUENCE — it is not "excluded from collection, tx stays
valid"; it is a `BadTranslation` that poisons `collectPlutusScriptsWithContext`'s result,
which UTXOS turns into a `CollectErrors` predicate failure that hard-rejects the WHOLE
transaction regardless of its declared IsValid flag. Investigation B was right on the outcome
(reject-the-whole-tx via `CollectErrors`/`BadTranslation`/`ConwayContextError`) — this repo's
own `Utxos.hs` and `Imp/UtxosSpec.hs` prove it directly, independent of whatever the specific
real on-chain tx under investigation actually used (V2 vs V3).
