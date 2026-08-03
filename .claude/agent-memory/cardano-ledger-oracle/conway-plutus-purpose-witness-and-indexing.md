---
name: conway-plutus-purpose-witness-and-indexing
description: Live-verified (2026-08-02) exact witness requirements per Conway TxCert constructor, getConwayScriptsNeeded construction, ConwayPlutusPurpose wire tags 0-5, and the ordering key (submission-order OSet vs sorted Map.keys) for each of the 6 redeemer purposes
metadata:
  type: reference
---

Verified live against IntersectMBO/cardano-ledger @ master (fetched 2026-08-02, files:
`eras/conway/impl/src/Cardano/Ledger/Conway/{UTxO,TxCert,Scripts,TxBody,TxInfo}.hs`,
`eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Procedures.hs`,
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/{UTxO,Plutus/TxInfo}.hs`,
`eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxos.hs`,
`libs/cardano-ledger-core/src/Cardano/Ledger/{Address,Credential,BaseTypes,Core/TxCert}.hs`).
Answers a dugite devnet-test-design audit for Conway script purposes beyond spend/mint.

## 1. Per-certificate script/vkey witness requirement (`getScriptWitnessConwayTxCert` / `getVKeyWitnessConwayTxCert`, Conway/TxCert.hs:763-804)

```haskell
getScriptWitnessConwayTxCert = \case
  ConwayTxCertDeleg delegCert -> case delegCert of
    ConwayRegCert _ SNothing -> Nothing              -- reg_cert (idx 0): NO witness, permissionless
    ConwayRegCert cred (SJust _) -> credScriptHash cred  -- reg_deposit_cert (idx 7): WITNESS REQUIRED
    ConwayUnRegCert cred _ -> credScriptHash cred     -- unreg_cert/unreg_deposit_cert (idx 1/8): ALWAYS witnessed
    ConwayDelegCert cred _ -> credScriptHash cred     -- stake/vote/stake_vote_delegation (idx 2/9/10): ALWAYS witnessed
    ConwayRegDelegCert cred _ _ -> credScriptHash cred -- stake/vote/stake_vote_reg_deleg (idx 11/12/13): ALWAYS witnessed
  ConwayTxCertPool {} -> Nothing                      -- "PoolIds can't be Scripts" (comment verbatim)
  ConwayTxCertGov govCert -> case govCert of
    ConwayAuthCommitteeHotKey coldCred _ -> credScriptHash coldCred  -- idx 14, cold cred only
    ConwayResignCommitteeColdKey coldCred _ -> credScriptHash coldCred -- idx 15
    ConwayRegDRep cred _ _ -> credScriptHash cred     -- reg_drep (idx 16): ALWAYS witnessed, no free-reg carve-out
    ConwayUnRegDRep cred _ -> credScriptHash cred     -- idx 17
    ConwayUpdateDRep cred _ -> credScriptHash cred    -- idx 18
```

`getVKeyWitnessConwayTxCert` mirrors this exactly (`credKeyHashWitness` in place of `credScriptHash`), with pool certs the one exception: `ConwayTxCertPool poolCert -> Just $ poolCertKeyHashWitness poolCert` (always key-witnessed, confirming pool reg/retire is key-only — never scriptable).

**The ONLY permissionless case is `ConwayRegCert cred SNothing`** (CDDL cert-index 0, the deposit-less Shelley-compatible `stake_registration`/`reg_cert`). Source comment verbatim: *"we preserve the old behavior of not requiring a witness for staking credential registration, but only during the transitional period of Conway era and only for staking credential registration certificates without a deposit. Future eras will require a witness for registration certificates, because the one without a deposit will be removed."* Note this carve-out does NOT extend to `ConwayRegDelegCert` (the combined reg+delegate cert, idx 11/12/13) even though it also performs a registration — that combined form is always witnessed.

CDDL cert-index confirmed from `EncCBOR ConwayDelegCert`/`ConwayGovCert` (Conway/TxCert.hs:488-641): 0/1/2 = legacy Shelley forms (`encodeShelleyDelegCert`), 3/4 = pool reg/retire, 7=reg_deposit_cert, 8=unreg_deposit_cert, 9=vote_deleg_cert, 10=stake_vote_deleg_cert, 11=stake_reg_deleg_cert, 12=vote_reg_deleg_cert, 13=stake_vote_reg_deleg_cert, 14=auth_committee_hot_cert, 15=resign_committee_cold_cert, 16=reg_drep_cert, 17=unreg_drep_cert, 18=update_drep_cert. 5 (GenesisDeleg) and 6 (MIR) hard-fail decode: `"...certificates are no longer supported"`.

## 2. `getConwayScriptsNeeded` (Conway/UTxO.hs:62-105) — the canonical scriptsNeeded builder

```haskell
getConwayScriptsNeeded utxo txBody =
  getSpendingScriptsNeeded utxo txBody
    <> getWithdrawingScriptsNeeded txBody
    <> certifyingScriptsNeeded
    <> getMintingScriptsNeeded txBody
    <> votingScriptsNeeded
    <> proposingScriptsNeeded
```

Each sub-builder uses `zipAsIxItem :: Foldable f => f it -> (AsIxItem Word32 it -> c) -> [c]` (Alonzo/UTxO.hs:364-368), which does `zipWith (\it ix -> f (AsIxItem ix it)) (toList xs) [0..]` — **the index is always the position in `toList` of whatever Foldable container is passed in.** The container's own type determines whether that's submission order or sorted order:

| Purpose | Container passed to `zipAsIxItem` | Container type | Resulting index order |
|---|---|---|---|
| Spending (0) | `txBody ^. inputsTxBodyL` | `Set TxIn` | ascending `Ord TxIn` (TxId, then TxIx) |
| Minting (1) | `txBody ^. mintedTxBodyF` | `Set PolicyID` (`SimpleGetter` = `mintTxBodyL . to policies`, Mary/TxBody.hs:74) | ascending `Ord PolicyID` (= ascending ScriptHash bytes) |
| Certifying (2) | `txBody ^. certsTxBodyL` | `OSet.OSet (TxCert era)` (`ctbrCerts`, Conway/TxBody.hs:130) | **submission/insertion order, NOT sorted** |
| Withdrawing/Rewarding (3) | `Map.keys (unWithdrawals $ txBody ^. withdrawalsTxBodyL)` | `Map AccountAddress Coin` | ascending `Ord AccountAddress` (see §4) |
| Voting (4) | `Map.keys (unVotingProcedures (txBody ^. votingProceduresTxBodyL))` | `Map Voter (Map GovActionId VotingProcedure)` | ascending `Ord Voter` (see §5) |
| Proposing (5) | `txBody ^. proposalProceduresTxBodyL` | `OSet.OSet (ProposalProcedure era)` (`ctbrProposalProcedures`, Conway/TxBody.hs:140) | **submission/insertion order, NOT sorted** |

Certifying and Proposing being OSet-backed (order-preserving, not `Ord`-sorted) matches the already-recorded #940 finding (`reference_cbor_set_tag_framing_audit_complete_2026_08_01.md`) that cert order is semantically load-bearing on the wire (registration must precede the delegation that uses it) — the redeemer pointer index tracks that SAME submission order, not a re-sort.

`AlonzoScriptsNeeded era = AlonzoScriptsNeeded { unAlonzoScriptsNeeded :: [(PlutusPurpose AsIxItem era, ScriptHash)] }`, `Monoid`/`Semigroup` = plain list concatenation (Alonzo/UTxO.hs:102-104) — so the six sub-lists above are simply concatenated in the fixed order shown (spend, withdraw, cert, mint, vote, propose); this ordering only matters for internal list traversal, not for the wire (each entry still carries its own `PlutusPurpose AsIx` tag+index).

Per-purpose script-hash extraction:
- Certifying: `getScriptWitnessTxCert txCert` (§1 table above).
- Voting: `CommitteeVoter cred -> credScriptHash cred`, `DRepVoter cred -> credScriptHash cred`, `StakePoolVoter _ -> Nothing` (SPO voters are ALWAYS key-only, never scriptable).
- Proposing: `ParameterChange _ _ (SJust guardrailsScriptHash) -> Just guardrailsScriptHash`, `TreasuryWithdrawals _ (SJust guardrailsScriptHash) -> Just guardrailsScriptHash`, every other `GovAction` constructor -> `Nothing`. These are the ONLY two `GovAction`s that carry a guardrails-script field at all.

`getConwayWitsVKeyNeeded` (vkey side) = `getShelleyWitsVKeyNeededNoGov utxo txBody` ∪ `Set.map asWitness reqSignerHashes` ∪ `voterWitnesses` (which pulls a key-hash witness for `CommitteeVoter`/`DRepVoter` when their credential is `KeyHashObj`, and unconditionally for `StakePoolVoter`).

## 3. `ConwayPlutusPurpose` wire tags — CONFIRMED exact (Conway/Scripts.hs:264-296, `EncCBORGroup`/`DecCBORGroup`)

```haskell
encCBORGroup = \case
  ConwaySpending p    -> encodeWord8 0 <> encCBOR p
  ConwayMinting p     -> encodeWord8 1 <> encCBOR p
  ConwayCertifying p  -> encodeWord8 2 <> encCBOR p
  ConwayWithdrawing p -> encodeWord8 3 <> encCBOR p   -- aka ConwayRewarding, deprecated pattern alias
  ConwayVoting p      -> encodeWord8 4 <> encCBOR p
  ConwayProposing p   -> encodeWord8 5 <> encCBOR p
```
So Spending=0, Minting=1, Certifying=2, Rewarding/Withdrawing=3, Voting=4, Proposing=5 — matches the user's assumption exactly, and matches Alonzo's legacy `RdmrPtr` Tag numbering (Spend=0/Mint=1/Cert=2/Rewrd=3) extended with two new tags.

`ConwayPlutusPurpose f era` data shape: `ConwaySpending !(f Word32 TxIn) | ConwayMinting !(f Word32 PolicyID) | ConwayCertifying !(f Word32 (TxCert era)) | ConwayWithdrawing !(f Word32 AccountAddress) | ConwayVoting !(f Word32 Voter) | ConwayProposing !(f Word32 (ProposalProcedure era))`.

Wire encoding of the redeemer map key at PV>=9 (Conway is always PV>=9): `Redeemers` is `Map (PlutusPurpose AsIx era) (Data era, ExUnits)` (Alonzo/TxWits.hs:144-169); at `ifEncodingVersionAtLeast (natVersion @9)` it's the plain map-form `EncCBOR (Map k v)` (canonical ascending-by-key, i.e. ascending by (tag, index)); pre-Conway (PV<9) it's the legacy definite array-of-`[tag,index,data,ex_units]` form via `encodeFoldableEncoder ... (Map.toAscList rs)`. `AsIx` carries only the `Word32` index; the tag+index pair is what `encCBORGroup`/`decCBORGroup` (above) produces as a 2-element CBOR group, consistent with CDDL `redeemer_tag = 0..5`.

## 4. Rewarding/Withdrawing index — `AccountAddress` Ord (answers "sorted by what")

`data AccountAddress = AccountAddress { aaNetworkId :: !Network, aaId :: !AccountId } deriving (Ord)` (Address.hs:183-190). GHC's derived `Ord` for a record compares fields in **declaration order**: `aaNetworkId` FIRST, then `aaId` (which wraps `Credential Staking`). So the withdrawals map — and hence the Rewarding purpose index — sorts **primarily by `Network` (`Testnet` < `Mainnet`, since `data Network = Testnet | Mainnet` declares Testnet first), secondarily by the staking credential's own `Ord`** (see §Credential below). In practice every withdrawal in one tx shares the same network, so this reduces to "sorted by credential" for realistic test transactions, but it is NOT literally "raw serialised address bytes" — it's the derived `Ord` on the structured `(Network, AccountId)` pair. `AccountAddress` is the modern name for what used to be called `RewardAccount` (`RewardAccount` is now a deprecated pattern synonym over it).

`Credential kr = ScriptHashObj !ScriptHash | KeyHashObj !(KeyHash kr) deriving (Ord)` (Credential.hs:98-101) — **`ScriptHashObj` is declared first, so script credentials sort BEFORE key credentials of the same role**, regardless of hash value; within the same constructor, ordering falls through to the hash bytes' own `Ord`.

## 5. Voting index — `Voter` Ord

`data Voter = CommitteeVoter !(Credential HotCommitteeRole) | DRepVoter !(Credential DRepRole) | StakePoolVoter !(KeyHash StakePool) deriving (Ord)` (Governance/Procedures.hs:338-342). Derived `Ord` across different constructors follows **declaration order**: `CommitteeVoter < DRepVoter < StakePoolVoter`, regardless of the wrapped credential/keyhash value. Within `CommitteeVoter`/`DRepVoter`, ties break on the `Credential`'s own `Ord` (script-before-key, per §4). So the Voting purpose index = position in `Map.keys` of `VotingProcedures` (`Map Voter (Map GovActionId VotingProcedure)`), grouped CC-votes-first, then DRep-votes, then SPO-votes, sorted by credential within each group.

## 6. Proposing — guardrails script is a HARD EQUALITY check against the CURRENT constitution, not "any script the proposer names"

`ParameterChange`/`TreasuryWithdrawals` each carry a `StrictMaybe ScriptHash` field, doc-commented "Guardrails script hash protection" (Governance/Procedures.hs:815-834) — this is the ONLY field read by `getConwayScriptsNeeded`'s `getProposalScriptHash` (§2), so Phase-1 witnessing accepts whatever hash is here.

**But the Conway GOV rule (`conwayGovTransition`, Conway/Rules/Gov.hs:445-559) separately hard-checks it against the on-chain constitution, for BOTH action types:**
```haskell
checkGuardrailsScriptHash expectedHash actualHash =
  failureUnless (actualHash == expectedHash) $ InvalidGuardrailsScriptHash actualHash expectedHash
...
TreasuryWithdrawals wdrls proposalPolicy -> do
  ...
  runTest $ checkGuardrailsScriptHash @era constitutionPolicy proposalPolicy
ParameterChange _ _ proposalPolicy ->
  runTest $ checkGuardrailsScriptHash @era constitutionPolicy proposalPolicy
```
`constitutionPolicy` comes from `GovEnv`, i.e. the CURRENT `Constitution`'s own `constitutionGuardrailsScriptHashL` field. The comparison is **strict `StrictMaybe ScriptHash` equality** — `SNothing == SNothing` also passes (meaning: if the constitution currently has no guardrails script, a `SJust` proposal hash is REJECTED as `InvalidGuardrailsScriptHash`, and a `SNothing` proposal needs no witness at all since `getProposalScriptHash` then returns `Nothing`).

**Consequence for test design: an "arbitrary always-true script" CANNOT be used for a Proposing-purpose test.** The proposal's guardrails hash must be byte-identical to whatever script hash is currently registered as the constitution's guardrails script (`ConwayGovPredFailure` tag 11, `InvalidGuardrailsScriptHash`, deprecated alias `InvalidPolicyHash`). To exercise a real Proposing-purpose Plutus witness: either seed the devnet genesis/bootstrap constitution with the test script's hash as its guardrails script, or submit-and-enact a `NewConstitution` action first that installs the test script, THEN submit the `ParameterChange`/`TreasuryWithdrawals` naming that same hash.

## 7. V1/V2 vs V3 purpose support — NOT just "Voting/Proposing are V3-only"; Certifying has a hidden sub-restriction for V1/V2

Purpose-level gate confirmed exact (`transPlutusPurposeV1V2`, Conway/TxInfo.hs:724-739):
```haskell
transPlutusPurposeV1V2 proxy pv = \case
  SpendingPurpose asIxItem   -> Alonzo.transPlutusPurpose proxy pv $ AlonzoSpending asIxItem
  MintingPurpose asIxItem    -> Alonzo.transPlutusPurpose proxy pv $ AlonzoMinting asIxItem
  CertifyingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoCertifying asIxItem
  WithdrawingPurpose asIxItem -> Alonzo.transPlutusPurpose proxy pv $ AlonzoWithdrawing asIxItem
  purpose -> Left $ inject $ PlutusPurposeNotSupported @era $ hoistPlutusPurpose toAsItem purpose
```
So at the purpose-dispatch level: Spending/Minting/Certifying/Withdrawing(Rewarding) ARE supported for V1 and V2; Voting/Proposing fall to the catch-all `PlutusPurposeNotSupported` — confirming Voting/Proposing are PlutusV3-only (see [[v1v2-txinfo-conway-babbage-gates]] for the full `ConwayContextError`/`guardConwayFeaturesForPlutusV1V2` gate mechanics — those tx-body-level field gates (non-empty votes/proposals, non-zero treasury donation, `SJust` currentTreasuryValue) are STRUCTURAL and fire on V1/V2 regardless of which purpose that script itself serves).

**Hidden second layer — `CertificateNotSupported` restricts WHICH cert types a V1/V2 Certifying script can actually witness**, independent of the purpose-level gate above. `toPlutusTxCert` for V1/V2 is `transTxCertV1V2` (Conway/TxInfo.hs:383-397):
```haskell
transTxCertV1V2 = \case
  RegDepositTxCert stakeCred _deposit -> Right $ PV1.DCertDelegRegKey (...)
  UnRegDepositTxCert stakeCred _refund -> Right $ PV1.DCertDelegDeRegKey (...)
  txCert
    | Just dCert <- Alonzo.transTxCertCommon txCert -> Right dCert
    | otherwise -> Left $ inject $ CertificateNotSupported txCert
```
`Alonzo.transTxCertCommon` (Alonzo/Plutus/TxInfo.hs:364-379) only matches: `RegTxCert`/`UnRegTxCert` (deposit-less forms, via the `getRegTxCert`/`getUnRegTxCert` pattern synonyms that ONLY fire on `ConwayRegCert cred SNothing`/`ConwayUnRegCert cred SNothing`), `DelegStakeTxCert` (which ONLY matches `ConwayDelegCert cred (DelegStake poolId)` — the plain pool-delegation case, via `getDelegStakeTxCert`), and pool reg/retire.

**This means V1/V2 Certifying scripts translate successfully ONLY for**: `reg_cert`/`unreg_cert` (idx 0/1, though idx 0 needs no witness per §1), `reg_deposit_cert`/`unreg_deposit_cert` (idx 7/8, explicitly handled), and plain pool-only `stake_delegation` (idx 2, `DelegStake` case only). **They hard-fail with `CertificateNotSupported` (-> `ConwayContextError` -> `CollectErrors`, whole-tx rejection, see §8) for**: `vote_delegation`/`stake_vote_delegation` (`DelegVote`/`DelegStakeVote`), ALL of `stake_reg_deleg`/`vote_reg_deleg`/`stake_vote_reg_deleg` (`ConwayRegDelegCert`, no case at all in either function), and ALL DRep/CC cert types (`ConwayRegDRep`/`ConwayUnRegDRep`/`ConwayUpdateDRep`/`ConwayAuthCommitteeHotKey`/`ConwayResignCommitteeColdKey` — none are `ConwayTxCertDeleg`, so none can match any Shelley-shaped pattern synonym).

By contrast, PlutusV3's OWN `transTxCert` (Conway/TxInfo.hs:559-597, used via `EraPlutusTxInfo 'PlutusV3 ConwayEra`'s `toPlutusTxCert _ pv = pure . transTxCert pv`) is a TOTAL match over every Conway cert constructor including `RegDepositDelegTxCert`, `AuthCommitteeHotKeyTxCert`, `RegDRepTxCert`, etc. — **no `CertificateNotSupported` restriction exists for V3.**

**Practical test-design consequence**: a devnet test wiring a V1 or V2 script to a DRep credential's `reg_drep`/`update_drep`/`unreg_drep`, a CC cold credential's `auth_committee_hot`/`resign_committee_cold`, a `vote_delegation`-family cert, or a `ConwayRegDelegCert` combo will pass Phase-1 witnessing (scriptsNeeded is language-agnostic) but then HARD-FAIL the entire transaction at script-collection time with `CollectErrors [BadTranslation (CertificateNotSupported ...)]` — this is NOT a phase-2 script-evaluation failure (no `is_valid=false` path available for it; see §8). A legitimate "V1/V2 Certifying purpose" test must use a plain `stake_delegation` (DelegStake) or a `reg_deposit_cert`/`unreg_deposit_cert`/`unreg_cert`.

Rewarding(Withdrawing) has no such sub-restriction — the withdrawing item is a bare `AccountAddress`, translated uniformly, no cert-shaped ambiguity.

## 8. Phase-2 failure path — uniform across all 6 purposes EXCEPT the translation/collection-time failures above

`Babbage.expectScriptsToPass` / `Babbage.babbageEvalScriptsTxInvalid` (Babbage/Rules/Utxos.hs:130-221, reused verbatim by Conway's UTXOS rule at Conway/Rules/Utxos.hs:224,241 — only the failure-constructor wrapping differs) operate on the SINGLE aggregated `scriptsWithContextEither :: Either (NonEmpty CollectError) [PlutusWithContext]` list covering every purpose together:
```haskell
(() <$ scriptsWithContextEither) ?!: (injectFailure . Alonzo.CollectErrors)   -- step 1, BEFORE IsValid is consulted
Alonzo.when2Phase $ whenFailureFree $
  forM_ scriptsWithContextEither $ \scriptsWithContext ->
    case evalPlutusScripts scriptsWithContext of                              -- step 2, IsValid-driven
      Fails/Passes -> ValidationTagMismatch (if actual result disagrees with declared is_valid)
```
No purpose-specific branch exists in either function. So:
- **Category A (translation/collection failures — `CollectErrors`)**: `NoCostModel`, `PlutusPurposeNotSupported` (V1/V2 on Voting/Proposing), `CertificateNotSupported` (V1/V2 on unsupported cert shapes, §7), `InvalidGuardrailsScriptHash` is actually a separate GOV-rule failure not a CollectError but similarly Phase-1/pre-execution — ALL of these are hard whole-transaction rejections evaluated BEFORE the `IsValid` flag is even consulted. There is no `is_valid=false` construction possible for these; the tx cannot be included in a block at all, valid or invalid.
- **Category B (actual CEK evaluation of a well-formed, correctly-collected script that runs and returns `False`/errors)**: uniform `ValidationTagMismatch`/collateral-consuming behavior for ALL SIX purposes identically — Certifying, Rewarding, Voting, and Proposing scripts that fail Phase-2 evaluation behave exactly like a failing Spending or Minting script: `is_valid=false` + matching declared flag -> collateral consumed, tx stays on-chain; declared-vs-actual mismatch -> `ValidationTagMismatch`, whole-tx rejection regardless of purpose.

For devnet always-FALSE-script tests of Certifying/Rewarding/Voting/Proposing: use a PlutusV3 script (V1/V2 restricted per §7) that correctly translates and is legitimately collected, and have it evaluate to `False` — this reaches Category B and behaves identically to the familiar spend/mint always-false-script test shape (`is_valid=false`, collateral consumed).

## Related
[[v1v2-txinfo-conway-babbage-gates]] — the tx-body-level structural gates (`guardConwayFeaturesForPlutusV1V2`) that fire on V1/V2 regardless of purpose; §7 above is the purpose-dispatch AND cert-shape layer on top of those.
[[nocostmodel-collecterror-exact-mechanics]] — same `CollectErrors`/`whenFailureFree`/pre-IsValid hard-reject mechanism, verified independently for the missing-cost-model case.
[[reference_cbor_set_tag_framing_audit_complete_2026_08_01]] — the OSet-vs-Set wire-tag distinction that underlies §2's submission-order-vs-sorted table.
