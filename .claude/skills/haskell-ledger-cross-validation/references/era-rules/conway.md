# Conway Era Ledger Rules — Delta over Babbage

Source: `IntersectMBO/cardano-ledger` master, `eras/conway/`.

Primary files: `Rules/Gov.hs`, `Rules/GovCert.hs`, `Rules/Ratify.hs`, `Rules/Enact.hs`, `Rules/Epoch.hs`, `Governance/Procedures.hs`, `Governance/Internal.hs`, `PParams.hs`, `Tx.hs`, `TxBody.hs`, `UTxO.hs`, `Translation.hs`.

## 1. GOVCERT — DRep + Committee certs

Replaces Babbage's DELEG/POOL/GOVCERT split. `conwayGovCertTransition` in `GovCert.hs`.

### DRep registration (`ConwayRegDRep cred deposit mAnchor`)
- Preconditions: `cred ∉ vsDReps` (else `ConwayDRepAlreadyRegistered`); `deposit == ppDRepDeposit` (else `ConwayDRepIncorrectDeposit`)
- Inserts `DRepState`:
  ```
  drepExpiry  = computeDRepExpiryVersioned pp currentEpoch numDormantEpochs
  drepAnchor  = mAnchor
  drepDeposit = ppDRepDepositCompact
  drepDelegs  = mempty
  ```
- **Bootstrap expiry**: `currentEpoch + ppDRepActivity` (no dormant-correction)
- **Post-bootstrap**: `(currentEpoch + ppDRepActivity) - numDormantEpochs`

### DRep unregistration (`ConwayUnRegDRep cred refund`)
- Preconditions: `cred ∈ vsDReps`, `refund == drepState.drepDeposit`
- Removes from `vsDReps`; clears `dRepDelegationAccountStateL .~ Nothing` for every account in `drepDelegs`

### DRep update (`ConwayUpdateDRep cred mAnchor`)
- Refreshes expiry: `currentEpoch + ppDRepActivity - numDormantEpochs`
- Optionally updates anchor

### Committee hot key auth (`ConwayAuthCommitteeHotKey coldCred hotCred`)
- Preconditions: cold key not previously resigned (else `ConwayCommitteeHasPreviouslyResigned`); cold key in current committee OR in live UpdateCommittee proposal (else `ConwayCommitteeIsUnknown`)
- Inserts `CommitteeHotCredential hotCred` at `coldCred`

### Committee resignation (`ConwayResignCommitteeColdKey coldCred anchor`)
- Inserts `CommitteeMemberResigned anchor` at `coldCred`. **PERMANENT**.

## 2. Governance actions (7 types) — CBOR

Defined in `Governance/Procedures.hs`. CBOR: `array(2)[tag, ...fields]` (except `InfoAction` = `array(1)[6]`).

### GovActionId wire
```
GovActionId = [TxId, GovActionIx]    -- [bstr(32), uint]
```
Decoder ≥ N2C V16 uses `decodeRecordNamed "GovActionId" (const 2)`.

`GovPurposeId` = newtype around `GovActionId` with phantom tag (`PParamUpdatePurpose`/`HardForkPurpose`/`CommitteePurpose`/`ConstitutionPurpose`). Same wire format.

### Tags
```
tag 0 = ParameterChange
  [0, prev_gov_action_id_or_null, PParamsUpdate_map, script_hash_or_null]

tag 1 = HardForkInitiation
  [1, prev_gov_action_id_or_null, ProtVer]

tag 2 = TreasuryWithdrawals
  [2, {AccountAddress => Coin, ...}, script_hash_or_null]

tag 3 = NoConfidence
  [3, prev_gov_action_id_or_null]

tag 4 = UpdateCommittee
  [4, prev_gov_action_id_or_null, [ColdCredential, ...], {ColdCredential => EpochNo, ...}, UnitInterval]

tag 5 = NewConstitution
  [5, prev_gov_action_id_or_null, Constitution]

tag 6 = InfoAction
  [6]
```

`prev_gov_action_id_or_null` decoded by `decodeNullStrictMaybe`. `ProtVer` (PV ≥ 9) = `array(2)[major, minor]`.

**Constraints**:
- TreasuryWithdrawals: sum must be positive (`ZeroTreasuryWithdrawals`)
- UpdateCommittee: removed ∩ added = ∅; all new expiries > currentEpoch
- InfoAction: no fields; `NoVotingThreshold` for all voter types (passes ratification mechanically, enactment is no-op)

## 3. Voting procedures

```
VotingProcedures = {Voter => {GovActionId => VotingProcedure, ...}, ...}
```
Inner map must be non-empty (voter with 0 votes rejected).

### Voter CBOR
```
[0, key_hash_28]   -- CommitteeVoter, KeyHashObj hot key
[1, script_hash]   -- CommitteeVoter, ScriptHashObj hot key
[2, key_hash_28]   -- DRepVoter, KeyHashObj
[3, script_hash]   -- DRepVoter, ScriptHashObj
[4, key_hash_28]   -- StakePoolVoter (KEY ONLY; tag 5 script fails)
```

### Vote CBOR (single uint)
- 0 = `VoteNo`
- 1 = `VoteYes`
- 2 = `Abstain`

### VotingProcedure CBOR
```
array(2)[vote_uint, anchor_or_null]
```

### Voter eligibility (`Governance/Internal.hs`)
Returns one of:
- `VotingThreshold t` — allowed, threshold applies
- `NoVotingThreshold` — allowed but no threshold (always passes)
- `NoVotingAllowed` — votes from this body invalid → `DisallowedVoters`

**SPO**: NoConfidence, UpdateCommittee, HardForkInit, ParameterChange (if SecurityGroup). NOT NewConstitution/TreasuryWithdrawals/InfoAction.
**DRep**: ALL. InfoAction = `NoVotingThreshold`.
**CC**: NewConstitution, HardForkInit, ParameterChange, TreasuryWithdrawals. NOT NoConfidence/UpdateCommittee. InfoAction = `NoVotingThreshold`.

### Vote counting semantics

**Committee**: iterates current members. Expired/not-authorized/resigned → abstain. Non-voted authorized → No. `accepted = yes / (yes + no)`.

**DRep**: iterates `reDRepDistr`. Expired/unregistered excluded entirely. Non-voted registered → No (in denominator). `DRepAlwaysAbstain` always abstains. `DRepAlwaysNoConfidence` votes Yes for NoConfidence, No otherwise. `accepted = yes / (yes + no)` excluding abstain.

**SPO (post-bootstrap)**: default depends on operator's DRep delegation. Delegated to `AlwaysNoConfidence` → Yes for NoConfidence default. Delegated to `AlwaysAbstain` → Abstain default. Otherwise → No default. Exception: HardForkInit non-voters always No (regardless of DRep delegation). `accepted = yes / (totalActiveStake - abstainStake)`.

**SPO (bootstrap)**: non-voters → Abstain (all except HardForkInit which is No).

## 4. Proposal procedures

```haskell
data ProposalProcedure era = ProposalProcedure
  { pProcDeposit    :: !Coin          -- must equal ppGovActionDeposit
  , pProcReturnAddr :: !AccountAddress -- must be registered (post-bootstrap)
  , pProcGovAction  :: !(GovAction era)
  , pProcAnchor     :: !Anchor
  }
```

Wire: `array(4)[Coin, AccountAddress, GovAction, Anchor]`.

### Validation in `processProposal`
1. **Bootstrap restriction**: `hardforkConwayBootstrapPhase` → action ∈ {ParameterChange, HardForkInit, InfoAction} (`DisallowedProposalDuringBootstrap`)
2. **HardFork continuity**: PV must immediately follow prev HF / cur PParams PV (`pvCanFollow`)
3. **PParamsUpdate well-formedness**: `ppuWellFormed`
4. **Return addr registered** (post-bootstrap): `ProposalReturnAccountDoesNotExist`
5. **Treasury withdrawal accounts registered** (post-bootstrap): `TreasuryWithdrawalReturnAccountsDoNotExist`
6. **Deposit**: `pProcDeposit == ppGovActionDeposit` (`ProposalDepositIncorrect`)
7. **Guardrails script** (ParameterChange + TreasuryWithdrawals): must match constitution's current script hash
8. **UpdateCommittee**: no removed/added overlap; all new expiries > currentEpoch
9. **Ancestry** (`proposalsAddAction`): prev action ID must point to correct slot in tree (`InvalidPrevGovActionId`)

Deposit tracked in `utxosDeposited`. Returned to `pProcReturnAddr` on enactment/expiration/sibling-removal.

## 5. RATIFY / ENACT

### Ratification ordering (`actionPriority`)
```
0 = NoConfidence
1 = UpdateCommittee
2 = NewConstitution
3 = HardForkInitiation
4 = ParameterChange
5 = TreasuryWithdrawals
6 = InfoAction
```

`reorderActions` sorts pulsed proposals by priority. Processed left-to-right.

### Ratification conditions (ALL must hold)
1. `prevActionAsExpected gas ensPrevGovActionIds` — parent pointer matches last enacted same-purpose action
2. `validCommitteeTerm` — UpdateCommittee: new expiries ≤ `currentEpoch + committeeMaxTermLength`
3. `not rsDelayed` — no delaying action ratified earlier in same RATIFY run
4. `withdrawalCanWithdraw` — TreasuryWithdrawals: total ≤ `ensTreasury`
5. `acceptedByEveryone` — all 3 voting bodies agree

### Voting thresholds

**PoolVotingThresholds** = `array(5)[...]`:
1. pvtMotionNoConfidence
2. pvtCommitteeNormal (when CC exists)
3. pvtCommitteeNoConfidence (when no CC)
4. pvtHardForkInitiation
5. pvtPPSecurityGroup

**DRepVotingThresholds** = `array(10)[...]`:
1. dvtMotionNoConfidence
2. dvtCommitteeNormal
3. dvtCommitteeNoConfidence
4. dvtUpdateToConstitution
5. dvtHardForkInitiation
6. dvtPPNetworkGroup
7. dvtPPEconomicGroup
8. dvtPPTechnicalGroup
9. dvtPPGovGroup
10. dvtTreasuryWithdrawal

ParameterChange effective DRep threshold = max over all touched groups (`pparamsUpdateThreshold`).

**Bootstrap**: all DRep thresholds = `def` (zero/minBound) for everything except InfoAction. CC = deciding body.

**CommitteeVotingThreshold** = committee's own `committeeThreshold`. CC vote = `NoVotingAllowed` for NoConfidence + UpdateCommittee. Committee fails check if active-non-resigned-non-expired count < `ppCommitteeMinSize` (post-bootstrap; skipped during bootstrap).

**Short-circuit**: threshold = `minBound` (0) → ratio comparison skipped → body unconditionally accepts.

### Delaying semantics
`rsDelayed = True` after any of: NoConfidence, HardForkInit, UpdateCommittee, NewConstitution. Subsequent priority-ordered proposals NOT ratified same epoch. NOT expired — carry over.

### ENACT (`enactmentTransition`)
- **ParameterChange**: `applyPPUpdates` to `ensCurPParams`; update `ensPrevPParamUpdateL`
- **HardForkInitiation**: set `ensProtVer`; update `ensPrevHardForkL`
- **TreasuryWithdrawals**: accumulate in `ensWithdrawals`; subtract from `ensTreasury`. ADA move happens in `epochTransition.applyEnactedWithdrawals`
- **NoConfidence**: `ensCommittee = SNothing`; update `ensPrevCommitteeL`
- **UpdateCommittee**: `Map.union added (currentMembers \\ removed)`; set new threshold; update `ensPrevCommitteeL`
- **NewConstitution**: set `ensConstitution`; update `ensPrevConstitutionL`
- **InfoAction**: NO-OP

After enactment in `epochTransition`, `ensWithdrawals` applied via `applyEnactedWithdrawals`. Unregistered targets silently skipped (ADA stays in treasury).

End of `ratifyTransition`:
```haskell
SSeq.Empty -> pure $ st & rsEnactStateL . ensTreasuryL .~ Coin 0
```
`ensTreasury` zeroed out — it's only transient for `withdrawalCanWithdraw`. Real treasury lives in `ChainAccountState.casTreasury`.

## 6. EPOCH rule additions

`epochTransition` Conway-additional steps:
```
SNAP → POOLREAP → (extract DRepPulser result) 
     → applyEnactedWithdrawals → proposalsApplyEnactment
     → update govState → deposit refunds → HARDFORK check → setFreshDRepPulsingState
```

### DRep pulser
Started at previous epoch boundary, pulsed throughout. At boundary: `extractDRepPulsingState` finalises (calling `ratifyTransition` to completion if not done). Produces `RatifyState`:
- `rsEnacted`: proposals that ratified (priority order)
- `rsExpired`: `gasExpiresAfter < reCurrentEpoch`
- `rsDelayed`: structural delay flag

`proposalsApplyEnactment` produces new proposal set with enacted/expired removed + their entire subtrees (orphaned children).

### Deposit refunds
`returnProposalDeposits allRemovedGovActions`: credit `pProcDeposit` back to `gasReturnAddr` account. Unregistered targets → treasury via `casTreasuryL <>~ fold unclaimed`.

### GovState update
```
cgsCommitteeL    .~ ensCommittee
cgsConstitutionL .~ ensConstitution
cgsCurPParamsL   .~ nextEpochPParams govState0    -- resolves FuturePParams
cgsPrevPParamsL  .~ curPParams
cgsFuturePParamsL .~ PotentialPParamsUpdate Nothing
```

### Dormant epoch counter
`updateNumDormantEpochs eNo newProposals vState`: increment `vsNumDormantEpochs` if no live proposals.

### Committee state pruning
`updateCommitteeState committee committeeState`: remove cold credentials not in newly enacted committee.

### HARDFORK sub-rule
If enacted `curPParams.protocolVersion` differs from `prevPParams.protocolVersion` → HARDFORK fires → HFC-level era transition.

### Fresh pulser
`setFreshDRepPulsingState eNo stakePoolDistr epochState2`:
- `dpPulseSize = max 1 (numAccounts / (4 * k))`
- Snapshots: DRep state, distributions, committee, proposals, `ssStakeMarkPoolDistr`
- `dpEnactState` from current GovState with actual treasury

## 7. PPUP rule REPLACED

In Conway:
```haskell
EraRuleFailure "PPUP" ConwayEra = VoidEraRule "PPUP"
```

PPUP can never fire. `cgsFuturePParamsL` holds `FuturePParams` tracking whether ParameterChange has been enacted (`DefinitePParamsUpdate pp`) or anticipated (`PotentialPParamsUpdate (Just pp)`).

Param changes apply at epoch boundaries following ratification of ParameterChange gov action. Minimum voting period = 1 full epoch after submission.

Babbage `TxBody` key 6 (`update`) absent/ignored in Conway decoder.

## 8. Pointer addresses dropped

Byron pointer addresses (`StakeRefPtr`) routing dropped entirely in Conway.

`Conway/State/Stake.hs`: `ConwayInstantStake` has NO `sisPtrStake` field. `addConwayInstantStake` handles `StakeRefPtr` via `_other` catch-all — pointer-referenced stake contributes nothing.

Babbage→Conway TranslateEra: DState translation does NOT carry pointer-keyed entries. Pointer-stake component of snapshots discarded at HFC boundary.

UTxOs at pointer addresses remain spendable (payment cred works), but ADA doesn't count for stake/voting power. Pointer-address holders effectively undelegated.

## 9. New PParams (keys 25-33)

| Key | Field | Type | CBOR |
|---|---|---|---|
| 25 | `poolVotingThresholds` | `PoolVotingThresholds` | `array(5)[rat, rat, rat, rat, rat]` |
| 26 | `dRepVotingThresholds` | `DRepVotingThresholds` | `array(10)[10 rats]` |
| 27 | `committeeMinSize` | `Word16` | uint |
| 28 | `committeeMaxTermLength` | `EpochInterval` | uint |
| 29 | `govActionLifetime` | `EpochInterval` | uint |
| 30 | `govActionDeposit` | `Coin` | uint |
| 31 | `dRepDeposit` | `Coin` | uint |
| 32 | `dRepActivity` | `EpochInterval` | uint |
| 33 | `minFeeRefScriptCostPerByte` | `NonNegativeInterval` | `tag(30)[array(2)[num, den]]` |

Rational thresholds use CBOR tag 30 `array(2)[num, den]`.

### PP group classification
Each PParam field has phantom `PPGroups` (DRepGroup × StakePoolGroup):
- `SecurityGroup` → SPO approval required (`pvtPPSecurityGroup`)
- `NoStakePoolGroup` → SPOs cannot vote

E.g. `minFeeRefScriptCostPerByte` = `EconomicGroup SecurityGroup` → SPOs can vote via `pvtPPSecurityGroup`.

## 10. TxBody new fields (19-22)

All optional, omitted from sparse map when empty/zero:

| Key | Field | Type | Omit condition |
|---|---|---|---|
| 19 | `votingProcedures` | `VotingProcedures` | empty map |
| 20 | `proposalProcedures` | `OSet(ProposalProcedure)` | empty |
| 21 | `currentTreasuryValue` | `StrictMaybe Coin` | `SNothing` |
| 22 | `treasuryDonation` | `Coin` | zero |

**Key 19**: `{Voter => {GovActionId => VotingProcedure}}`. Empty outer → omitted.

**Key 20**: ordered set. Position = `GovActionIx` (zero-indexed). Empty → omitted.

**Key 21**: tx body may assert expected current treasury value (for off-chain verification). Does NOT block on-chain validation. Stored in ledger state.

**Key 22**: ADA from inputs → treasury directly. Must be positive (decode fails "Treasury Donation must be non-zero"). Accumulated in `utxosDonation`, swept to `casTreasury` at boundary. Conway `produced` includes `treasuryDonationTxBodyL` for balance.

Script integrity hash (key 11) covers new Plutus purposes: `VotingPurpose`, `ProposingPurpose`.

## 11. DRep expiry + activity

DReps prove activity by: (a) `ConwayUpdateDRep` cert, OR (b) vote in any epoch with at least one live proposal.

Inactive (`currentEpoch > drepExpiry`) → excluded from `dRepAcceptedRatio` numerator AND denominator.

**Post-bootstrap formula** (`computeDRepExpiry`):
```
drepExpiry = (currentEpoch + ppDRepActivity) - numDormantEpochs
```

`numDormantEpochs` (in `VState.vsNumDormantEpochs`) increments at boundaries with no live proposals. Dormant periods don't count against activity.

**Bootstrap registration expiry** (no dormant correction):
```
drepExpiry = currentEpoch + ppDRepActivity
```
Known breaking change between bootstrap + post-bootstrap.

**Update at boundary**:
```haskell
updateNumDormantEpochs currentEpoch ps vState =
  if null $ OMap.filter ((currentEpoch <=) . gasExpiresAfter) $ ps ^. pPropsL
    then vState & vsNumDormantEpochsL %~ succ
    else vState
```

**AlwaysAbstain / AlwaysNoConfidence**: not `Credential DRepRole` entries; they are `DRep` constructors handled directly in `dRepAcceptedRatio`. Never expire, cannot register.

## 12. Tiered reference script fee

```haskell
getConwayMinFeeTx pp tx refScriptsSize =
  alonzoMinFeeTx pp tx <+> refScriptsFee
  where
    refScriptsFee = tierRefScriptFee
      (unboundRational $ pp ^. ppRefScriptCostMultiplierG)   -- 1.2 hardcoded
      (fromIntegral $ pp ^. ppRefScriptCostStrideG)           -- 25600 bytes hardcoded
      (unboundRational $ pp ^. ppMinFeeRefScriptCostPerByteL) -- PParams key 33
      refScriptsSize
```

**Hardcoded** (NOT in on-chain PParams):
- `ppRefScriptCostMultiplierG = 1.2` (growth factor per tier)
- `ppRefScriptCostStrideG = 25600` bytes (tier width)
- Max per TX: 200 KB
- Max per block: 1 MB

```haskell
tierRefScriptFee multiplier sizeIncrement baseFee totalSize =
  go 0 baseFee totalSize
  where
    go !acc !curTierPrice !n
      | n < sizeIncrement =
          Coin $ floor (acc + toRational n * curTierPrice)
      | otherwise =
          go (acc + toRational sizeIncrement * curTierPrice)
             (multiplier * curTierPrice)
             (n - sizeIncrement)
```

Geometric tier pricing: first 25,600 B at baseFee/byte, next 25,600 B at 1.2×base, then 1.44×base, etc. Floor applied only at final partial tier.

**Ref script size counting** (`txNonDistinctRefScriptsSize`): UNION of ref inputs + regular inputs. Duplicates counted twice. Script size = `originalBytesSize` (raw CBOR length).

`alonzoMinFeeTx` unchanged. Ref script fee purely additive.

## 13. Bootstrap phase (`hardforkConwayBootstrapPhase :: ProtVer -> Bool`)

Active when `pvMajor <= 9` (PV 9.0 = initial Conway; 10.0 = post-bootstrap).

### Restricted proposals
Only `isBootstrapAction` allowed: ParameterChange, HardForkInit, InfoAction. Others fail `DisallowedProposalDuringBootstrap`.

### Restricted DRep votes
DReps cannot vote except on InfoAction → `DisallowedVotesDuringBootstrap`.

### DRep thresholds during bootstrap
```haskell
thresholds = def     -- all zero/minBound
```
DReps effectively zero-threshold; CC = binding constraint.

### SPO default vote during bootstrap
Non-voting SPOs → Abstain (all actions except HardForkInit which is No).

### Committee minimum size check
Bootstrap: skipped. Post-bootstrap: enforced.

### Unelected committee vote check
Post-bootstrap: CC hot keys not associated with elected committee member → `UnelectedCommitteeVoters`. NOT enforced during bootstrap.

## Rust translation notes for dugite

1. **GOV/GOVCERT ordering**: GOVCERT during CERTS rule, BEFORE GOV. DRep unregistrations within same tx visible to gov processing.

2. **Proposal ancestry tree**: `Proposals` is multi-rooted tree (`PGraph`). Each purpose has own root chain. `proposalsAddAction` requires prev-action-id to point to existing node in correct purpose tree (or `SNothing` for root). Flat map loses tree structure — cannot correctly remove subtrees when sibling enacts.

3. **Ratification delay**: `rsDelayed` thread-state through iteration, NOT simple boolean. Process by priority; once delaying type ratifies, skip rest. Don't skip expiration checks.

4. **Treasury in EnactState**: `ensTreasury` is TRANSIENT copy from `setFreshDRepPulsingState`. Don't confuse with `ChainAccountState.casTreasury`. `withdrawalCanWithdraw` uses transient; actual move at boundary.

5. **Tiered ref script fee**: multiplier=1.2, stride=25600 are HARDCODED in Haskell, not on-chain PParams. Only `minFeeRefScriptCostPerByte` (key 33) is configurable.

6. **DRep expiry bootstrap vs post-bootstrap**: formula differs at `pvMajor <= 9`. Preview transitioned to PV10 at epoch 736. Switch formula correctly mid-sync.

7. **Pointer address stake**: `ConwayInstantStake` must NOT aggregate from `StakeRefPtr` addresses. Stake map only contains `StakeRefBase` entries. Silent discard, not "no DRep" bucket.
