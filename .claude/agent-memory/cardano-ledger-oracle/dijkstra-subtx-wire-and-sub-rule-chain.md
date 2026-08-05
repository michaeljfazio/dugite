---
name: dijkstra-subtx-wire-and-sub-rule-chain
description: Dijkstra era (nested transactions) sub-tx body CBOR key numbers, DijkstraSubTx witness-set independence, and the full SUBUTXOW/SUBCERTS-family/SUBGOV rule chain wiring — live-verified 2026-08-05 at commit 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 (= master HEAD at check time, UNRELEASED, cardano-ledger-dijkstra.cabal version 0.4.0.0, latest tag only 0.3.0.0)
metadata:
  type: reference
---

## Pin and stability warning

Verified live at `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` — this SHA **is** master
HEAD (`gh api repos/IntersectMBO/cardano-ledger/compare/<sha>...master` →
`ahead_by:0, behind_by:0, status:identical`, checked 2026-08-05). The Dijkstra
package's own cabal file at this commit reads `version: 0.4.0.0`, and the
`CHANGELOG.md` `## 0.4.0.0` (unreleased) header literally lists "Change the STS
Signal of SUBENTITIES to Tx SubTx era" / "Add SubEntitiesEnv" as pending
changes — i.e. the exact SUBENTITIES shape captured below was added in the
commits that produced this exact snapshot. Latest **tagged** release is
`cardano-ledger-dijkstra-0.3.0.0` (tag SHA `fd86ed0c…`). **Nothing in this
memory has ever shipped in a release.** Re-verify line numbers/shapes before
relying on them again; this is one of the most actively-churning corners of
the repo. See [[kb-table-files-missing-use-live-github]] for the general
policy on this.

## Question A — DijkstraSubTxBodyRaw CBOR keys

File: `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxBody.hs` (1438 lines
at this commit). **ONE GADT**, not two separate types:

```haskell
data DijkstraTxBodyRaw l era where
  DijkstraTxBodyRaw :: { dtbrSpendInputs :: !(Set TxIn), ... 24 fields ... }
    -> DijkstraTxBodyRaw TopTx era
  DijkstraSubTxBodyRaw :: { dstbrSpendInputs :: !(Set TxIn), ... 18 fields ... }
    -> DijkstraTxBodyRaw SubTx era
```
(lines 166-214). **ONE shared `encodeTxBodyRaw` function** (line 477-539) with
two pattern-match clauses — `DijkstraTxBodyRaw {..}` at line 480, exactly
`DijkstraSubTxBodyRaw {..}` at **line 513** (matches a prior caller's citation
"TxBody.hs:513" exactly). **ONE shared sparse decoder**
(`DecCBOR (Annotator (DijkstraTxBodyRaw l era))`, lines 331-475) gated by an
`STxBothLevels l era` GADT witness obtained via `withSTxBothLevels @l`; keys
valid only at `TopTx` use a pattern guard `| STopTx <- sTxLevel`.

Complete `DijkstraSubTxBodyRaw` key table (verbatim from the SubTx encoder
clause, lines 513-539) — **identical key numbers to the corresponding TopTx
fields** where a field is shared (verified by direct comparison of both
encoder clauses):

| Key | Field | Omit condition |
|---|---|---|
| 0 | `dstbrSpendInputs` (inputs) | none (required) |
| 1 | `dstbrOutputs` | none (required) |
| 3 | `dstbrVldt` invalidHereAfter (ttl-equivalent) | `encodeKeyedStrictMaybe` |
| 4 | `dstbrCerts` | `OSet.null` |
| 5 | `dstbrWithdrawals` | `null . unWithdrawals` |
| 7 | `dstbrAuxDataHash` | `encodeKeyedStrictMaybe` |
| 8 | `dstbrVldt` invalidBefore | `encodeKeyedStrictMaybe` |
| 9 | `dstbrMint` | `== mempty` |
| 11 | `dstbrScriptIntegrityHash` | `encodeKeyedStrictMaybe` |
| 14 | `dstbrGuards` — **NOT required_signers**, see below | `null` |
| 15 | `dstbrNetworkId` | `encodeKeyedStrictMaybe` |
| 18 | `dstbrReferenceInputs` | `null` |
| 19 | `dstbrVotingProcedures` | `null . unVotingProcedures` |
| 20 | `dstbrProposalProcedures` | `OSet.null` |
| 21 | `dstbrCurrentTreasuryValue` | `encodeKeyedStrictMaybe` |
| 22 | `dstbrTreasuryDonation` | `== mempty` |
| 24 | `dstbrRequiredTopLevelGuards` — confirmed real field, present at both levels | `== mempty`, custom `E (encodeMap encCBOR (encodeNullStrictMaybe encCBOR))` |
| 25 | `dstbrDirectDeposits` | `null . unDirectDeposits` |
| 26 | `dstbrAccountBalanceIntervals` | `null . unAccountBalanceIntervals` |

**Excluded from SubTx, structurally (separate GADT constructor simply lacks
the field — NOT an `Impossible`/error branch)**: key 2 fee (`dtbrFee` — a
sub-tx has **no fee field at all**, the parent pays), key 13
`dtbrCollateralInputs`, key 16 `dtbrCollateralReturn`, key 17
`dtbrTotalCollateral`, key 23 `dtbrSubTransactions` (no recursive nesting —
one level of sub-tx only), key 27 `dtbrStartingAccountBalanceIntervals`
(TopTx-only). Field count check: TopTx ctor has 24 fields (24 underscores in
the `NFData`/`rnf` pattern at line 234), SubTx ctor has 18 (line 260) — 24−18=6
matches the excluded-key count exactly.

**Trap for anyone assuming Conway numbering**: key 14 in Dijkstra is
`dtbrGuards :: OSet (Credential Guard)` (a brand-new Dijkstra concept), **not**
Conway's `required_signers`/`reqSignerHashes`. Confirmed at TxBody.hs:1197 —
`reqSignerHashesTxBodyL = notSupportedInThisEraL` — Dijkstra has structurally
**removed** the classic required-signers field and reused key 14 for the new
Guards mechanism instead. Everything else that carries over from Conway's
numbering (4=certs, 5=withdrawals, 9=mint, 11=script_integrity_hash,
15=network_id, 18=reference_inputs, 19=voting_procedures,
20=proposal_procedures, 21=treasury_value, 22=donation,
25=direct_deposits, 26=account_balance_intervals) checks out unchanged in
semantics, not just number. Keys 24 (`required_top_level_guards`) and 27
(`starting_account_balance_intervals`) are new, no Conway analog.

Decoder requires: `STopTx -> [(0,"inputs"),(1,"outputs"),(2,"fee")]`,
`SSubTx -> [(0,"inputs"),(1,"outputs")]` (line 469-472) — confirms fee really
is unconditionally absent/unrequired for SubTx at the type level, not just
omitted-when-zero.

## Question B — DijkstraSubTx has its OWN independent witness set

File: `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Tx.hs`, lines 88-101:

```haskell
data DijkstraTx l era where
  DijkstraTx ::
    { dtBody :: !(TxBody TopTx era)
    , dtWits :: !(TxWits era)
    , dtIsPhase2Valid :: !IsPhase2Valid
    , dtAuxData :: !(StrictMaybe (TxAuxData era))
    } -> DijkstraTx TopTx era
  DijkstraSubTx ::
    { dstBody :: !(TxBody SubTx era)
    , dstWits :: !(TxWits era)
    , dstAuxData :: !(StrictMaybe (TxAuxData era))
    } -> DijkstraTx SubTx era
```

`dstWits` is a genuinely separate value of the same TYPE as the parent's
`dtWits` (`TxWits DijkstraEra = AlonzoTxWits DijkstraEra`, unparameterized by
level — see `Dijkstra/TxWits.hs`, 37 lines, a pure `EraTxWits`/
`AlonzoEraTxWits` instance with no Sub-specific witness type at all). **A
sub-tx's spend authorization is settled by ITS OWN witness set — never the
parent's.** Confirmed by `SUBUTXOW`'s transition (`dijkstraSubUtxowTransition`,
`Rules/SubUtxow.hs`) reading `tx ^. witsTxL` off the `stAnnTx` that IS the
sub-tx, running `Shelley.validateVerifiedWits`/`Shelley.validateNeededWitnesses`
against it directly, with no reference anywhere to the parent's witness set.

**Wire placement — sub-txs are NOT block-segwit siblings of the top-level
tx.** They live **embedded inside the parent's own body**, at TxBody key 23
(`dtbrSubTransactions :: !(OMap TxId (Tx SubTx era))`), decoded via
`decodeNonEmptySetLikeEnforceNoDuplicatesAnn` over the general
`DecCBOR (Annotator (Tx l DijkstraEra))` instance, which for `SSubTx` runs
`decodeRecordNamed "DijkstraSubTx" (const 3) $ do body <- decCBOR; wits <-
decCBOR; aux <- ...` — i.e. each embedded sub-tx is a plain 3-element record
`[body, wits, auxData]`, keyed in the `OMap` by its own `TxId` (derived the
normal way, from its own body hash). This is explicitly documented as
DIFFERENT from the top-level Tx's segwit-style block layout — see the doc
comment on `toCBORForMempoolSubmission`/`toCBORForSizeComputation` in Tx.hs:
"this serialisation is neither the serialisation used on-chain (where Txs are
deconstructed using segwit)…". `DijkstraSubTx`'s `[body,wits,auxData]` shape is
literally how it always appears (both on-chain-embedded and for
mempool/size purposes) — it never gets segwit-deconstructed the way a
`TopTx` does at the block level.

## Question C — SUB-rule chain wiring (exact, per rule)

All files under `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Rules/`.
Era-level rule-tag wiring in `Dijkstra/Era.hs` (299 lines) is worth noting
first because it explains which "Sub" rules are bespoke vs pass-through:
top-level `EraRule "DELEG" DijkstraEra = Conway.DELEG DijkstraEra`,
`"CERTS" = Conway.CERTS DijkstraEra`, `"POOL" = Shelley.POOL DijkstraEra` are
**literal reuses of the Conway/Shelley rule tag itself** (no Dijkstra
newtype at all), whereas `GOV`, `GOVCERT`, `SUBCERTS`, `SUBCERT`,
`SUBENTITIES`, `SUBUTXOW`, `SUBUTXO` etc. all get their **own** `data X era`
tag even when (as with GOV/GOVCERT) the transition function plugged in is
still literally Conway's.

### SUBUTXOW (`Rules/SubUtxow.hs`, 353 lines)
**Bespoke transition** `dijkstraSubUtxowTransition` (line 204) — not a single
reused Conway/Babbage monolith, but hand-assembled from individually-reused
helper functions: `Shelley.validateVerifiedWits`,
`Babbage.validateFailedBabbageScripts`, `Shelley.validateNeededWitnesses`,
`Alonzo.missingRequiredDatums`, `Shelley.validateMetadata`,
`Alonzo.checkScriptIntegrityHash`, `Alonzo.hasExactSetOfRedeemers`,
`Babbage.validateScriptsWellFormedTxOuts`, plus Dijkstra's own
`validateGuardDatums`, then `trans @(EraRule "SUBUTXO" era)`.

`DijkstraSubUtxowPredFailure` — 18 constructors (tags 0-17, `Rules/SubUtxow.hs`
lines 68-112, CBOR tags at lines 278-297):
```
0 SubUtxoFailure (PredicateFailure (EraRule "SUBUTXO" era))
1 SubInvalidWitnessesUTXOW (NonEmpty (VKey Witness))
2 SubMissingVKeyWitnessesUTXOW (NonEmptySet (KeyHash Witness))
3 SubScriptWitnessNotValidatingUTXOW (NonEmptySet ScriptHash)
4 SubMissingTxBodyMetadataHash TxAuxDataHash
5 SubMissingTxMetadata TxAuxDataHash
6 SubConflictingMetadataHash (Mismatch RelEQ TxAuxDataHash)
7 SubInvalidMetadata
8 SubMissingRedeemers (NonEmpty (PlutusPurpose AsItem era, ScriptHash))
9 SubMissingRequiredDatums (NonEmptySet DataHash) (Set DataHash)
10 SubNotAllowedSupplementalDatums (NonEmptySet DataHash) (Set DataHash)
11 SubPPViewHashesDontMatch (Mismatch RelEQ (StrictMaybe ScriptIntegrityHash))
12 SubUnspendableUTxONoDatumHash (NonEmptySet TxIn)
13 SubExtraRedeemers (NonEmpty (PlutusPurpose AsIx era))
14 SubMalformedScriptWitnesses (NonEmptySet ScriptHash)
15 SubMalformedReferenceScripts (NonEmptySet ScriptHash)
16 SubScriptIntegrityHashMismatch (Mismatch RelEQ (StrictMaybe ScriptIntegrityHash)) (StrictMaybe ByteString)
17 SubMalformedGuardDatums (NonEmptySet (Credential Guard))
```

### SUBENTITIES (`Rules/SubEntities.hs`, 314 lines)
**Fully bespoke** `dijkstraSubEntitiesTransition` — orchestrates
withdrawal/direct-deposit account validation, `Conway.updateDormantDRepExpiries`
+ `Conway.updateVotingDRepExpiries`, then descends into SUBCERTS. No Conway
analog (ENTITIES/accounts/direct-deposits are Dijkstra-only concepts layered
on Conway's CertState). `SubEntitiesPredFailure` — 6 constructors (tags 0-5):
```
0 SubCertsFailure (PredicateFailure (EraRule "SUBCERTS" era))
1 SubMissingAccountsInWithdrawals Withdrawals
2 SubMissingOriginalAccountsInWithdrawals Withdrawals
3 SubMissingAccountsInDirectDeposits DirectDeposits
4 SubWrongNetworkInWithdrawals Network (NonEmptySet AccountAddress)
5 SubWrongNetworkInDirectDeposits Network (NonEmptySet AccountAddress)
```
(`Signal (SUBENTITIES era) = Tx SubTx era`, `Environment = SubEntitiesEnv era`
— both were changed by the still-unreleased 0.4.0.0 commits per CHANGELOG.)

### SUBCERTS (`Rules/SubCerts.hs`, 184 lines)
**Bespoke** `dijkstraSubCertsTransition` — structurally a recursive fold
(`gamma :|> txCert`) analogous in shape to Conway's CERTS but with its own
`SubCertsEnv { certsTx :: Tx SubTx era, certsPParams, certsCurrentEpoch,
certsCurrentCommittee, certsCommitteeProposals }`. `DijkstraSubCertsPredFailure`
is a **single-constructor newtype**: `SubCertFailure (PredicateFailure
(EraRule "SUBCERT" era))`.

### SUBCERT (`Rules/SubCert.hs`, 233 lines) — the per-certificate dispatcher
**Bespoke** `dijkstraSubCertTransition`, pattern-matches on
`DijkstraTxCert` (Dijkstra's own cert sum type) and dispatches:
`DijkstraTxCertDeleg` → SUBDELEG (via `dijkstraToConwayDelegCert` adapter),
`DijkstraTxCertPool` → SUBPOOL, `DijkstraTxCertGov` → SUBGOVCERT.
`DijkstraSubCertPredFailure` — 3 constructors (tags 1,2,3 — **note: no tag 0**):
```
1 SubDelegFailure (PredicateFailure (EraRule "SUBDELEG" era))
2 SubPoolFailure (PredicateFailure (EraRule "SUBPOOL" era))
3 SubGovCertFailure (PredicateFailure (EraRule "SUBGOVCERT" era))
```

### SUBDELEG (`Rules/SubDeleg.hs`, 76 lines)
`transitionRules = [Conway.conwayDelegTransition]` — **literal, direct reuse**
of Conway's own transition function, byte-for-byte, no wrapping/adaptation of
the logic itself. `DijkstraSubDelegPredFailure` is a bare newtype around
`Conway.ConwayDelegPredFailure era` (derives `EncCBOR`/`DecCBOR` — same wire
encoding as Conway's DELEG failure). Full constructor list (from
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs` lines 105-113, 8
constructors):
```
IncorrectDepositDELEG Coin
StakeKeyRegisteredDELEG (Credential Staking)
StakeKeyNotRegisteredDELEG (Credential Staking)
StakeKeyHasNonZeroAccountBalanceDELEG Coin
DelegateeDRepNotRegisteredDELEG (Credential DRepRole)
DelegateeStakePoolNotRegisteredDELEG (KeyHash StakePool)
DepositIncorrectDELEG (Mismatch RelEQ Coin)
RefundIncorrectDELEG (Mismatch RelEQ Coin)
```

### SUBPOOL (`Rules/SubPool.hs`, 87 lines)
`transitionRules = [Shelley.poolTransition]` — **literal, direct reuse** of
Shelley's POOL transition. `DijkstraSubPoolPredFailure` newtype-wraps
`Shelley.ShelleyPoolPredFailure era`. Full constructor list (from
`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs` lines 91-114, 6
constructors):
```
StakePoolNotRegisteredOnKeyPOOL (KeyHash StakePool)
StakePoolRetirementWrongEpochPOOL (Mismatch RelGT EpochNo) (Mismatch RelLTEQ EpochNo)
StakePoolCostTooLowPOOL (Mismatch RelGTEQ Coin)
WrongNetworkPOOL (Mismatch RelEQ Network) (KeyHash StakePool)
PoolMedataHashTooBig (KeyHash StakePool) Int    -- [sic] "Medata" typo is upstream, verbatim
VRFKeyHashAlreadyRegistered (KeyHash StakePool) (VRFVerKeyHash StakePoolVRF)
```

### SUBGOVCERT (`Rules/SubGovCert.hs`, 86 lines)
`transitionRules = [Conway.conwayGovCertTransition]` — **literal, direct
reuse**. `DijkstraSubGovCertPredFailure` newtype-wraps **Dijkstra's own
top-level `DijkstraGovCertPredFailure`** (not Conway's directly — one more
indirection). `DijkstraGovCertPredFailure` (`Rules/GovCert.hs`, 6
constructors, tags 0-5) is itself a straight 1:1 relabel of Conway's
`ConwayGovCertPredFailure` (see `conwayToDijkstraGovCertPredFailure`):
```
0 DijkstraDRepAlreadyRegistered (Credential DRepRole)
1 DijkstraDRepNotRegistered (Credential DRepRole)
2 DijkstraDRepIncorrectDeposit (Mismatch RelEQ Coin)
3 DijkstraCommitteeHasPreviouslyResigned (Credential ColdCommitteeRole)
4 DijkstraDRepIncorrectRefund (Mismatch RelEQ Coin)
5 DijkstraCommitteeIsUnknown (Credential ColdCommitteeRole)
```
Top-level `GOVCERT` rule (not "Sub") uses the exact same
`transitionRules = [Conway.conwayGovCertTransition]` and the exact same
`DijkstraGovCertPredFailure` type — SUBGOVCERT and GOVCERT are twins that
differ only in which STS tag wraps them.

### SUBGOV (`Rules/SubGov.hs`, 86 lines)
`transitionRules = [Conway.conwayGovTransition]` — **literal, direct reuse of
the SAME function the top-level Dijkstra `GOV` rule also uses**
(`Rules/Gov.hs` line 220 is `transitionRules = [Conway.conwayGovTransition]`
too). `State (SUBGOV era) = Proposals era`, `Environment = Conway.GovEnv era`,
`Signal = Conway.GovSignal era` — identical STS shape to top-level GOV.
`DijkstraSubGovPredFailure` newtype-wraps **Dijkstra's own top-level
`DijkstraGovPredFailure`**. `DijkstraGovPredFailure` (`Rules/Gov.hs`, 19
constructors, tags 0-18) is a straight relabel of Conway's
`ConwayGovPredFailure` (see `conwayToDijkstraGovPredFailure`):
```
0  GovActionsDoNotExist (NonEmpty GovActionId)
1  MalformedProposal (GovAction era)
2  ProposalProcedureNetworkIdMismatch AccountAddress Network
3  TreasuryWithdrawalsNetworkIdMismatch (NonEmptySet AccountAddress) Network
4  ProposalDepositIncorrect (Mismatch RelEQ Coin)
5  DisallowedVoters (NonEmpty (Voter, GovActionId))
6  ConflictingCommitteeUpdate (NonEmptySet (Credential ColdCommitteeRole))
7  ExpirationEpochTooSmall (NonEmptyMap (Credential ColdCommitteeRole) EpochNo)
8  InvalidPrevGovActionId (ProposalProcedure era)
9  VotingOnExpiredGovAction (NonEmpty (Voter, GovActionId))
10 ProposalCantFollow (StrictMaybe (GovPurposeId 'HardForkPurpose)) (Mismatch RelGT ProtVer)
11 InvalidGuardrailsScriptHash (StrictMaybe ScriptHash) (StrictMaybe ScriptHash)
12 DisallowedProposalDuringBootstrap (ProposalProcedure era)
13 DisallowedVotesDuringBootstrap (NonEmpty (Voter, GovActionId))
14 VotersDoNotExist (NonEmpty Voter)
15 ZeroTreasuryWithdrawals (GovAction era)
16 ProposalReturnAccountDoesNotExist AccountAddress
17 TreasuryWithdrawalReturnAccountsDoNotExist (NonEmpty AccountAddress)
18 UnelectedCommitteeVoters (NonEmpty (Credential HotCommitteeRole))
```
(`InvalidPolicyHash` is a `{-# DEPRECATED #-}` pattern synonym alias for
`InvalidGuardrailsScriptHash`, not a distinct constructor/tag.)

## Question D — release status

**Master-only, unreleased.** Latest tag is `cardano-ledger-dijkstra-0.3.0.0`
(tag object SHA `fd86ed0ce78e22157a93f3189317e4d93f225672`); the pinned commit
carries `version: 0.4.0.0` in the cabal file with an "Unreleased" CHANGELOG
section actively describing the SUBENTITIES shape captured above. Treat every
line number and every field/constructor list here as liable to move again —
re-fetch at whatever commit is current before depending on it for a Dugite
implementation decision.
