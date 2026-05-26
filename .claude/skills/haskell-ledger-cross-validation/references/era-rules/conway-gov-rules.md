# Conway GOV Rule — Complete Cross-Validation Reference

**Source commit**: `ebed62de1ebcd4b13512418d49d17802a193e2c1` (IntersectMBO/cardano-ledger master, 2026-05)  
**Primary file**: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`  
**Supporting files**:
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Internal.hs` — voting-threshold logic
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Procedures.hs` — `VotingProcedures`, `foldrVotingProcedures`, `GovAction` constructors
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs` — `authorizedElectedHotCommitteeCredentials`
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs` — `authorizedHotCommitteeCredentials`
- `libs/cardano-ledger-core/src/Cardano/Ledger/Rules/ValidationMode.hs` — `lblStatic`, `reapplyTx` semantics
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/API/Mempool.hs` — `applyTx`/`reapplyTx`
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Mempool.hs` — `mempoolTransition`
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs` — epoch-boundary proposal removal
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Proposals.hs` — `proposalsApplyEnactment`
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ratify.hs` — expiry check
- `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` — `ppuWellFormed`
- `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` — hardfork feature flags
- `eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/GovSpec.hs` — canonical behaviour tests

---

## 1. Rule Identity

The `ConwayGOV` STS rule processes one transaction's governance payload in a single invocation:

```haskell
-- Gov.hs lines 345-360
instance STS (ConwayGOV era) where
  type State (ConwayGOV era)       = Proposals era
  type Signal (ConwayGOV era)      = GovSignal era
  type Environment (ConwayGOV era) = GovEnv era
  type BaseM (ConwayGOV era)       = ShelleyBase
  type PredicateFailure (ConwayGOV era) = ConwayGovPredFailure era
  transitionRules = [conwayGovTransition]
```

The signal `GovSignal` carries three fields:

```haskell
data GovSignal era = GovSignal
  { gsVotingProcedures   :: !(VotingProcedures era)
  , gsProposalProcedures :: !(OSet.OSet (ProposalProcedure era))
  , gsCertificates       :: !(SSeq.StrictSeq (TxCert era))
  }
```

`gsCertificates` is read only to identify `UnRegDRepTxCert` entries; the GOV rule does not re-validate certs.

The environment `GovEnv` carries:

```haskell
data GovEnv era = GovEnv
  { geTxId                 :: TxId
  , geEpoch                :: EpochNo
  , gePParams              :: PParams era
  , geGuardrailsScriptHash :: StrictMaybe ScriptHash
  , geCertState            :: CertState era
  , geCommittee            :: StrictMaybe (Committee era)
  }
```

`geCommittee` is the **enacted** committee from `govState.committee` — this is the elected membership, not the raw hot-key registry.

---

## 2. ConwayGovPredFailure — Complete Tag Table

CBOR tag assignments are normative (they appear on the wire in `MsgRejectTx`):

| Tag | Constructor | Since |
|-----|-------------|-------|
| 0 | `GovActionsDoNotExist (NonEmpty GovActionId)` | Conway (PV 9) |
| 1 | `MalformedProposal (GovAction era)` | Conway |
| 2 | `ProposalProcedureNetworkIdMismatch AccountAddress Network` | Conway |
| 3 | `TreasuryWithdrawalsNetworkIdMismatch (NonEmptySet AccountAddress) Network` | Conway |
| 4 | `ProposalDepositIncorrect (Mismatch RelEQ Coin)` | Conway |
| 5 | `DisallowedVoters (NonEmpty (Voter, GovActionId))` | Conway |
| 6 | `ConflictingCommitteeUpdate (NonEmptySet (Credential ColdCommitteeRole))` | Conway |
| 7 | `ExpirationEpochTooSmall (NonEmptyMap (Credential ColdCommitteeRole) EpochNo)` | Conway |
| 8 | `InvalidPrevGovActionId (ProposalProcedure era)` | Conway |
| 9 | `VotingOnExpiredGovAction (NonEmpty (Voter, GovActionId))` | Conway |
| 10 | `ProposalCantFollow (StrictMaybe GovPurposeId) (Mismatch RelGT ProtVer)` | Conway |
| 11 | `InvalidGuardrailsScriptHash (StrictMaybe ScriptHash) (StrictMaybe ScriptHash)` | Conway |
| 12 | `DisallowedProposalDuringBootstrap (ProposalProcedure era)` | Conway |
| 13 | `DisallowedVotesDuringBootstrap (NonEmpty (Voter, GovActionId))` | Conway |
| 14 | `VotersDoNotExist (NonEmpty Voter)` | Conway |
| 15 | `ZeroTreasuryWithdrawals (GovAction era)` | Conway |
| 16 | `ProposalReturnAccountDoesNotExist AccountAddress` | Conway |
| 17 | `TreasuryWithdrawalReturnAccountsDoNotExist (NonEmpty AccountAddress)` | Conway |
| 18 | `UnelectedCommitteeVoters (NonEmpty (Credential HotCommitteeRole))` | PV > 10 |

`InvalidPolicyHash` is a deprecated pattern synonym for tag 11 (`InvalidGuardrailsScriptHash`).

---

## 3. Bootstrap vs Post-Bootstrap Feature Flags

These three predicates in `Era.hs` control mode gating:

```haskell
-- Era.hs lines 170-176
hardforkConwayBootstrapPhase :: ProtVer -> Bool
hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9

hardforkConwayDisallowUnelectedCommitteeFromVoting :: ProtVer -> Bool
hardforkConwayDisallowUnelectedCommitteeFromVoting pv = pvMajor pv > natVersion @10
```

Bootstrap phase is **exactly PV 9** (Conway launch). PV 10+ is post-bootstrap. PV 11+ activates `UnelectedCommitteeVoters`.

### What bootstrap permits:

- **Proposals**: only `ParameterChange`, `HardForkInitiation`, `InfoAction` — any other action type fires `DisallowedProposalDuringBootstrap` (tag 12).
- **Votes by DReps**: only allowed on `InfoAction` (and only if `isBootstrapAction` returns true, which DRep votes require). For anything else, fires `DisallowedVotesDuringBootstrap` (tag 13).
- **Votes by SPOs and CC**: allowed on any bootstrap action.
- **`ProposalReturnAccountDoesNotExist`** (tag 16): SKIPPED during bootstrap.
- **`TreasuryWithdrawalReturnAccountsDoNotExist`** (tag 17): SKIPPED during bootstrap.
- **DRep voting thresholds** at ratification time: reset to `def` (zero) during bootstrap — every DRep vote is effectively a `NoVotingThreshold`.

```haskell
-- Internal.hs lines 510-532
votingDRepThresholdInternal pp isElectedCommittee action =
  let thresholds@DRepVotingThresholds {..}
        | hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL) = def
        | otherwise = pp ^. ppDRepVotingThresholdsL
  ...
```

### What changes at PV 10 (post-bootstrap):

- All proposal types allowed.
- DRep votes allowed on all action types (subject to `NoVotingAllowed` check at ratification).
- `ProposalReturnAccountDoesNotExist` and `TreasuryWithdrawalReturnAccountsDoNotExist` activated.
- `DisallowedProposalDuringBootstrap` and `DisallowedVotesDuringBootstrap` no longer fire.

### What changes at PV 11:

- `UnelectedCommitteeVoters` (tag 18) activated in the GOV rule.
- `hardforkConwayDisallowUnelectedCommitteeFromVoting` becomes `True`.
- The `MEMPOOL` rule's inline `unelectedCommitteeVoters` check is bypassed (deferred entirely to GOV).

---

## 4. Predicate Execution Order — conwayGovTransition

This is the **exact** order, line by line, from `Gov.hs`:

### Step 1 — UnelectedCommitteeVoters (PV > 10 only)

```haskell
-- Gov.hs lines 479-482
when (hardforkConwayDisallowUnelectedCommitteeFromVoting $ pp ^. ppProtocolVersionL) $
  failOnNonEmpty
    (unelectedCommitteeVoters committee committeeState gsVotingProcedures)
    (injectFailure . UnelectedCommitteeVoters)
```

This runs **before** proposal processing and before the voter-partitioning loop. It checks voting procedures against the ELECTED committee (intersection of `csCommitteeCreds` with `committeeMembers electedCommittee`), excluding resigned hot keys. A hot credential that is not in `authorizedElectedHotCommitteeCredentials` fires this failure.

### Step 2 — Proposal processing loop (foldlM')

Each proposal in `gsProposalProcedures` is processed left-to-right via `processProposal`. Per-proposal checks fire `failBecause` immediately on the first failure for that proposal. The loop is monadic so a per-proposal failure halts that proposal but the state returned is the proposals-before-that-proposal.

Per-proposal ordering within `processProposal`:

1. `checkBootstrapProposal` → `DisallowedProposalDuringBootstrap` (tag 12)
2. `preceedingHardFork` → `ProposalCantFollow` (tag 10)
3. `actionWellFormed` → `MalformedProposal` (tag 1)
4. Post-bootstrap only: `ProposalReturnAccountDoesNotExist` (tag 16) and `TreasuryWithdrawalReturnAccountsDoNotExist` (tag 17)
5. Deposit amount check → `ProposalDepositIncorrect` (tag 4)
6. Return-address network ID check → `ProposalProcedureNetworkIdMismatch` (tag 2)
7. Per-action-type checks:
   - `TreasuryWithdrawals`: network ID of withdrawal accounts (tag 3), `checkGuardrailsScriptHash` (tag 11), zero-sum check (tag 15)
   - `UpdateCommittee`: `ConflictingCommitteeUpdate` (tag 6), `ExpirationEpochTooSmall` (tag 7)
   - `ParameterChange`: `checkGuardrailsScriptHash` (tag 11)
8. Ancestry/lineal-chain check → `InvalidPrevGovActionId` (tag 8) via `proposalsAddAction`

### Step 3 — Voter partitioning (internVoter)

After all proposals are processed, the voting procedures are partitioned into `(unknownVoters, knownVoters)` using `internVoter`. This is a **pure, lazy partition** — no failure fires here, it just categorises.

```haskell
-- Gov.hs lines 592-601
internVoter = \case
  CommitteeVoter hotCred -> CommitteeVoter <$> internSet hotCred knownCommitteeMembers
  DRepVoter cred         -> DRepVoter      <$> internMap cred knownDReps
  StakePoolVoter poolId  -> StakePoolVoter <$> internMap poolId knownStakePools
```

Where `knownCommitteeMembers = authorizedHotCommitteeCredentials committeeState`, which **excludes resigned members**:

```haskell
-- CertState.hs lines 338-344
authorizedHotCommitteeCredentials CommitteeState {csCommitteeCreds} =
  let toHotCredSet acc = \case
        CommitteeHotCredential hotCred -> Set.insert hotCred acc
        CommitteeMemberResigned {}     -> acc
   in F.foldl' toHotCredSet Set.empty csCommitteeCreds
```

Simultaneously, `foldrVotingProcedures` over `knownVoters` builds:
- `unknownGovActionIds :: [GovActionId]` — one entry per `(voter, gaId)` pair where the gaId is not in `proposalsActionsMap` (may contain **duplicates** if multiple voters vote on the same unknown action)
- `knownVotesWithCast :: [(Voter, Vote, GovActionState)]` — known voter + known action pairs
- `replacedVotes :: Set (Voter, GovActionId)` — votes replacing existing votes in the same tx

### Step 4 — Voter existence check

```haskell
-- Gov.hs line 604
failOnNonEmpty unknownVoters (injectFailure . VotersDoNotExist)
```

Fires `VotersDoNotExist` (tag 14) if any voter in the voting procedures is not in the corresponding ledger registry. The `NonEmpty` payload lists all unknown voters.

### Step 5 — Action existence check

```haskell
-- Gov.hs line 605
failOnNonEmpty unknownGovActionIds (injectFailure . GovActionsDoNotExist)
```

Fires `GovActionsDoNotExist` (tag 0) if any vote references a `GovActionId` not in the current `Proposals`. The `NonEmpty` payload can have **duplicate GovActionIds** if multiple voters voted on the same missing action.

### Step 6 — Bootstrap vote restriction

```haskell
-- Gov.hs line 606
runTest $ checkBootstrapVotes pp knownVotes
```

Fires `DisallowedVotesDuringBootstrap` (tag 13) for any vote in `knownVotes` that is not allowed during bootstrap. During bootstrap, DReps may only vote on `InfoAction`; SPOs and CC may vote on any `isBootstrapAction`.

### Step 7 — Expiry check

```haskell
-- Gov.hs line 607
runTest $ checkVotesAreNotForExpiredActions currentEpoch knownVotes
```

Fires `VotingOnExpiredGovAction` (tag 9). The check is:

```haskell
checkVotesAreNotForExpiredActions curEpoch votes =
  checkDisallowedVotes votes VotingOnExpiredGovAction $ \GovActionState {gasExpiresAfter} _ ->
    curEpoch <= gasExpiresAfter
```

A vote is **allowed** when `currentEpoch <= gasExpiresAfter`. It is **rejected** when `currentEpoch > gasExpiresAfter`. The boundary epoch (curEpoch == gasExpiresAfter) is allowed.

### Step 8 — Voter authority check

```haskell
-- Gov.hs line 608
runTest $ checkVotersAreValid currentEpoch committeeState knownVotes
```

Fires `DisallowedVoters` (tag 5). Delegates to the three `is{Committee,DRep,StakePool}VotingAllowed` predicates in `Internal.hs`.

### Step 9 — Vote state update

Applies all `knownVotesWithCast` to the proposals map via `proposalsAddVote`. Cleans up DRep votes from proposals for any `UnRegDRepTxCert` certificates in the same transaction (the `unregisteredDReps` set).

---

## 5. Voter Authority Matrix (isVotingAllowed)

The three functions in `Internal.hs` determine which voter types may vote on which action types. The return type is `VotingThreshold`:

```haskell
data VotingThreshold
  = VotingThreshold UnitInterval  -- voting is allowed; this is the ratification threshold
  | NoVotingThreshold             -- ratification impossible even with 100% votes
  | NoVotingAllowed               -- votes are not accepted at all → DisallowedVoters
```

`isVotingAllowed` maps these to `Bool`:

```haskell
isVotingAllowed = \case
  VotingThreshold {}  -> True
  NoVotingThreshold   -> True   -- voting is still accepted; action just never ratifies
  NoVotingAllowed     -> False  -- fires DisallowedVoters
```

### SPO (StakePool) voting — `votingStakePoolThresholdInternal`

| Action | Result | Notes |
|--------|--------|-------|
| `NoConfidence` | `VotingThreshold pvtMotionNoConfidence` | allowed |
| `UpdateCommittee` | `VotingThreshold pvtCommitteeNormal/NoConfidence` | allowed |
| `NewConstitution` | `NoVotingAllowed` | fires DisallowedVoters |
| `HardForkInitiation` | `VotingThreshold pvtHardForkInitiation` | allowed |
| `ParameterChange ppu` | `VotingThreshold pvtPPSecurityGroup` if SecurityGroup relevant, else `NoVotingAllowed` | SPO may only vote on security-group PParam changes |
| `TreasuryWithdrawals` | `NoVotingAllowed` | fires DisallowedVoters |
| `InfoAction` | `NoVotingThreshold` | allowed (vote accepted, never ratifies) |

**Key**: for `ParameterChange`, SPO voting is `NoVotingAllowed` unless the update touches a SecurityGroup parameter (`pvtPPSecurityGroup`). This means most `ParameterChange` proposals reject SPO votes with `DisallowedVoters`. The single test `can submit SPO votes` (GovSpec.hs line 885) uses a `ppuTxFeePerByteL` update which IS in SecurityGroup, so SPO votes are allowed there.

### CC (Committee) voting — `votingCommitteeThresholdInternal`

| Action | Result |
|--------|--------|
| `NoConfidence` | `NoVotingAllowed` — fires DisallowedVoters |
| `UpdateCommittee` | `NoVotingAllowed` — fires DisallowedVoters |
| `NewConstitution` | `VotingThreshold` (if committee active and min-size met) |
| `HardForkInitiation` | `VotingThreshold` |
| `ParameterChange` | `VotingThreshold` |
| `TreasuryWithdrawals` | `VotingThreshold` |
| `InfoAction` | `NoVotingThreshold` |

If committee size < `ppCommitteeMinSizeL` (and NOT in bootstrap), threshold degrades to `NoVotingThreshold` — votes are accepted but cannot ratify.

### DRep voting — `votingDRepThresholdInternal`

All action types accept DRep votes (`VotingThreshold` or `NoVotingThreshold` for InfoAction). During bootstrap, all thresholds reset to `def` (zero) — DRep votes on non-`InfoAction` fire `DisallowedVotesDuringBootstrap`, not `DisallowedVoters`.

---

## 6. The UnelectedCommitteeVoters Check (PV > 10)

### What it checks

`unelectedCommitteeVoters` (Gov.hs lines 649-661) collects every `CommitteeVoter hotCred` in `gsVotingProcedures` where `hotCred` is NOT in `authorizedElectedHotCommitteeCredentials committee committeeState`:

```haskell
unelectedCommitteeVoters committee committeeState =
  let authorizedElectedCommittee = authorizedElectedHotCommitteeCredentials committee committeeState
      collectUnelectedCommitteeVotes !unelectedHotCreds voter _ =
        case voter of
          CommitteeVoter hotCred
            | hotCred `Set.notMember` authorizedElectedCommittee ->
                Set.insert hotCred unelectedHotCreds
          _ -> unelectedHotCreds
   in Map.foldlWithKey' collectUnelectedCommitteeVotes Set.empty . unVotingProcedures
```

### How `authorizedElectedHotCommitteeCredentials` is built

```haskell
-- Governance.hs lines 581-591
authorizedElectedHotCommitteeCredentials committee committeeState =
  case committee of
    SNothing -> Set.empty
    SJust electedCommittee ->
      authorizedHotCommitteeCredentials $
        CommitteeState $
          csCommitteeCreds committeeState `Map.intersection` committeeMembers electedCommittee
```

It takes the hot-key registry, intersects with the `electedCommittee`'s member map, then filters out resigned members via `authorizedHotCommitteeCredentials`. This means only a hot credential that:
1. Maps to a cold credential that is in `geCommittee` (the enacted committee), AND
2. Is NOT resigned (`CommitteeMemberResigned`)

...passes this check.

### Distinction from VotersDoNotExist for CC hot credentials

When `UnelectedCommitteeVoters` fires (PV > 10), the same hot credential also fires `VotersDoNotExist` in step 4 (since resigned credentials are excluded from `knownCommitteeMembers`). The test (GovSpec.hs lines 752-764) confirms both failures are expected together at PV 11:

```haskell
if hardforkConwayDisallowUnelectedCommitteeFromVoting pv
  then submitFailingVote (CommitteeVoter hotCred) gaId
    [ injectFailure $ UnelectedCommitteeVoters [hotCred]
    , injectFailure $ VotersDoNotExist [CommitteeVoter hotCred]
    ]
  else submitFailingVote (CommitteeVoter hotCred) gaId
    [injectFailure $ VotersDoNotExist [CommitteeVoter hotCred]]
```

At PV ≤ 10, only `VotersDoNotExist`. At PV 11+, BOTH `UnelectedCommitteeVoters` AND `VotersDoNotExist`.

---

## 7. VotersDoNotExist and Resigned CC Members

### The resigned-member semantics

`knownCommitteeMembers` in the GOV rule is built from `authorizedHotCommitteeCredentials committeeState`, which **excludes** `CommitteeMemberResigned` entries:

```haskell
-- CertState.hs lines 339-343
toHotCredSet acc = \case
  CommitteeHotCredential hotCred -> Set.insert hotCred acc
  CommitteeMemberResigned {}     -> acc
```

A resigned CC member's hot credential is NOT in `knownCommitteeMembers`. Therefore:
- `internVoter (CommitteeVoter hotCred)` returns `Nothing` for a resigned member's hot cred.
- The voter lands in `unknownVoters`.
- `VotersDoNotExist` fires.

A resigned CC member voting on a governance action gets `VotersDoNotExist`, **not** `DisallowedVoters`. This is the correct Haskell behaviour.

---

## 8. ppuWellFormed — MalformedProposal Check

Only applies to `ParameterChange` proposals. All other action types skip the `actionWellFormed` check.

```haskell
-- Gov.hs lines 395-402
actionWellFormed pv ga = failureUnless isWellFormed $ MalformedProposal ga
  where
    isWellFormed = case ga of
      ParameterChange _ ppd _ -> ppuWellFormed pv ppd
      _ -> True
```

`ppuWellFormed` (PParams.hs lines 934-957) rejects proposals where any of the following are true:

- `MaxBBSize`, `MaxTxSize`, `MaxBHSize`, `MaxValSize`, `CollateralPercentage` set to zero
- `CommitteeMaxTermLength` or `GovActionLifetime` set to `EpochInterval 0`
- `PoolDeposit`, `GovActionDeposit`, or `DRepDeposit` set to zero (compact coin)
- During PV ≥ 10: `CoinsPerUTxOByte` set to zero
- The update is `emptyPParamsUpdate` (no fields set)
- During PV ≥ 11: `nOpt` (`desiredNumberOfPools`) set to zero

Any parameter that is `SNothing` (not being updated) passes automatically — `isValid` returns `True` for `SNothing`.

---

## 9. Epoch-Boundary Semantics — Proposal Expiry and Removal

### Expiry criterion (Ratify.hs line 357)

```haskell
if gasExpiresAfter < reCurrentEpoch
  then pure $ st' & rsExpiredL %~ Set.insert gasId
```

A proposal is marked expired when `gasExpiresAfter < reCurrentEpoch` (strict less-than). A proposal with `gasExpiresAfter == reCurrentEpoch` is NOT yet expired — ratification still runs on it.

### Expiry epoch calculation (Gov.hs lines 417-425)

```haskell
mkGovActionState actionId proposal expiryInterval curEpoch =
  GovActionState
    { gasExpiresAfter = addEpochInterval curEpoch expiryInterval
    , ...
    }
```

`addEpochInterval (EpochNo n) (EpochInterval k) = EpochNo (n + k)`.

A proposal submitted in epoch E with `ppGovActionLifetime = EpochInterval L` expires after epoch `E + L`. It is available for voting through the end of epoch `E + L` and is removed at the epoch boundary when `currentEpoch = E + L + 1`.

### Removal mechanism (Epoch.hs lines 314-326)

```haskell
(newProposals, enactedActions, removedDueToEnactment, expiredActions) =
  proposalsApplyEnactment rsEnacted rsExpired (govState0 ^. proposalsGovStateL)
```

`proposalsApplyEnactment` first removes expired proposals via `proposalsRemoveWithDescendants`, then processes enacted proposals. The entire subtree (ancestors and descendants in the priority forest) of an expired proposal is removed together.

Deposit refunds: all removed proposals (expired + enacted + sibling-removal) have their deposits returned to `certDState ^. accountsL` via `returnProposalDeposits`. Unclaimed deposits (for unregistered return addresses) go to treasury at the same boundary.

### `VotingOnExpiredGovAction` vs epoch-boundary removal

The GOV rule fires `VotingOnExpiredGovAction` when `currentEpoch > gasExpiresAfter`. This can only happen mid-epoch if the proposal's expiry was in a prior epoch but the proposal was not yet removed (which cannot happen — removal is atomic at the boundary). In practice this fires when a tx submits a vote in the same epoch as the expiry but the slot falls after the boundary, or more commonly as a guard during the same epoch when the epoch counter has advanced.

The key invariant: within a single epoch, proposals expired in prior epochs have been removed. A proposal visible in `proposalsActionsMap` has `gasExpiresAfter >= currentEpoch`. The expiry check is therefore primarily a defence for edge cases like cross-epoch mempool revalidation.

---

## 10. reapplyTx / Mempool Semantics for GOV

### lblStatic — what gets skipped on reapply

`reapplyTx` runs `ValidateSuchThat (notElem lblStatic)` — it skips only checks marked with `(?!#)` or `runTestOnSignal`. These are pure cryptographic / structural checks that cannot fail on a previously-validated transaction.

In `Gov.hs`, **no check uses `(?!#)` or `runTestOnSignal`**. Every predicate uses `runTest`, `failOnNonEmpty`, `failBecause`, or `?!`. Therefore:

**ALL GOV predicates run on `reapplyTx`.**

This is correct: the GOV predicates depend on the current `Proposals` state, which changes as blocks are applied. A vote valid at block N may become invalid at block N+1 if the action was enacted or expired at the intervening epoch boundary.

### MEMPOOL rule and UnelectedCommitteeVoters pre-PV11

The `mempoolTransition` (Mempool.hs) contains an inline `UnelectedCommitteeVoters` check:

```haskell
unless (hardforkConwayDisallowUnelectedCommitteeFromVoting protVer) $
  failOnNonEmpty
    (unelectedCommitteeVoters ... gsVotingProcedures)
    (ConwayMempoolFailure . addPrefix . T.pack . show . NE.toList)
```

This runs the same `unelectedCommitteeVoters` logic **before** LEDGER is invoked, but only when `pvMajor <= 10`. At PV > 10, it is bypassed because the GOV rule handles it directly. The MEMPOOL failure is a `ConwayMempoolFailure String` (a free-text error), not the typed `UnelectedCommitteeVoters (NonEmpty Credential)` failure — so the wire encoding differs.

---

## 11. Divergences in Dugite

The following are confirmed gaps or semantic differences between the Haskell GOV rule and the current Dugite implementation as of the `ebed62de` reference commit. File references are to `crates/dugite-ledger/src/validation/` and `crates/dugite-node/src/node/serve.rs`.

### D-1 — UnelectedCommitteeVoters missing from validation pipeline

**Status**: Not implemented.

Haskell fires `UnelectedCommitteeVoters` (tag 18) at PV > 10 when a `CommitteeVoter` hot credential is not in `authorizedElectedHotCommitteeCredentials`. This is a separate GOV-rule check that runs **before** the voter-partitioning loop.

Dugite has `ValidationError::UnelectedCommitteeMember` (mod.rs line 543) which fires for `CommitteeHotAuth` **certificates** — this is the CERT rule check for key authorisation, not the GOV rule vote check.

There is no `ValidationError::UnelectedCommitteeVoters` variant. The state-apply path in `governance.rs` has a warning-and-skip at line 279 ("UnelectedCommitteeVoter: CC vote from unelected hot credential — ignoring") but this only affects state mutation after block acceptance; it does not reject the transaction.

**Fix needed**: Add `UnelectedCommitteeVoters` variant to `ValidationError`. In `validate_transaction_with_pools`, before the voter-partitioning loop (around mod.rs line 1608), add a check: when `protocol_version_major > 10`, collect every `CommitteeVoter` hot credential that is not in `committee_authorized_hot_keys_elected` (a new context field that mirrors `authorizedElectedHotCommitteeCredentials`). This requires a new `ValidationContext` field that holds only the elected (non-resigned, actually-in-committee) hot credentials — distinct from `committee_authorized_hot_keys` which currently includes resigned members (see D-4).

### D-2 — Predicate ordering: Expired before DisallowedVoters

**Status**: Present bug.

Haskell ordering (steps 7 and 8):
```
checkVotesAreNotForExpiredActions  →  VotingOnExpiredGovAction   (step 7)
checkVotersAreValid                →  DisallowedVoters           (step 8)
```

Dugite ordering (mod.rs lines 1686, 1747):
```
DisallowedVoters        (around line 1686)
VotingOnExpiredGovAction (around line 1747)
```

Dugite checks `DisallowedVoters` before `VotingOnExpiredGovAction`. Haskell checks `VotingOnExpiredGovAction` first. For a single `(voter, govActionId)` pair where the voter is disallowed AND the action is expired, Haskell fires `VotingOnExpiredGovAction` while Dugite fires `DisallowedVoters`.

The `extra_errors` Vec in Dugite accumulates all errors and they are returned together, but the **first** error in the list is what consumers (like conformance tests) key on. More importantly, if `VotingOnExpiredGovAction` fired early, the `DisallowedVoters` check might skip the pair (since the action lookup would fail). The actual impact depends on whether the action exists in `active_proposals` when expired — it should, since expiry is detected against active proposals. If the action is already absent from `active_proposals` (removed at boundary), neither check fires anyway.

**Fix needed**: Move the `VotingOnExpiredGovAction` block to run before the `DisallowedVoters` block.

### D-3 — GovActionsDoNotExist: Haskell payload may contain duplicates, Dugite deduplicates

**Status**: Semantic mismatch (minor).

Haskell builds `unknownGovActionIds :: [GovActionId]` by prepending `gaId` for each `(voter, gaId)` pair where the action is unknown. If voters A and B both vote on the same missing action X, the list is `[X, X]`. The `NonEmpty` payload passed to `GovActionsDoNotExist` contains duplicates.

Dugite uses a `seen: HashSet<GovActionId>` to deduplicate, so the payload is `[X]` — one entry per unique missing action ID regardless of how many voters referenced it.

This is a cosmetic difference for single-action cases but will cause wire-format divergence in conformance tests that compare the full `GovActionsDoNotExist` payload against a Haskell golden.

**Note**: This is the CBOR encoding of the predicate failure, not the acceptance/rejection decision. Both sides reject the transaction; only the payload differs.

### D-4 — VotersDoNotExist: Resigned CC members not excluded from committee_authorized_hot_keys

**Status**: Semantic bug.

Haskell's `knownCommitteeMembers` is built from `authorizedHotCommitteeCredentials committeeState`, which **excludes** `CommitteeMemberResigned` entries from `csCommitteeCreds`. A resigned CC member's hot credential is absent from `knownCommitteeMembers`, so voting with it fires `VotersDoNotExist`.

Dugite's `build_governance_validation_state` (serve.rs lines 307-313) builds `committee_hot_keys` as:

```rust
let committee_hot_keys = ledger
    .gov.governance.committee_hot_keys
    .values()
    .copied()
    .collect();
```

This is the full hot-key registry, without filtering out resigned members. A resigned CC member's hot credential IS in `committee_authorized_hot_keys`, so `is_voter_unknown` returns `false` for that voter. The voter does not end up in `VotersDoNotExist`.

Depending on what `committee_authorized_hot_keys` contains (all or just active), the resigned member may instead hit `DisallowedVoters` (if the action type is one CC cannot vote on) or pass all checks entirely (if the action is one CC can vote on).

**Fix needed**: When building `committee_hot_keys` for the ValidationContext, filter out hot credentials whose corresponding cold credential is in `committee_resigned`:

```rust
let committee_hot_keys = ledger.gov.governance.committee_hot_keys
    .iter()
    .filter_map(|(cold_hash, hot_hash)| {
        if ledger.gov.governance.committee_resigned.contains_key(cold_hash) {
            None  // resigned → exclude from authorized set
        } else {
            Some(*hot_hash)
        }
    })
    .collect();
```

For D-1's fix, a separate `committee_authorized_hot_keys_elected` field would additionally need to filter against the enacted committee membership (not just resignation status).

### D-5 — DisallowedVoters: ParameterChange SPO uses NoVotingAllowed, not NoVotingThreshold

**Status**: Correct implementation but needs documentation guard.

In `votingStakePoolThresholdInternal` (Internal.hs lines 391-407), `ParameterChange` with a non-SecurityGroup update returns `NoVotingAllowed` → `isVotingAllowed` returns `false` → `DisallowedVoters` fires.

Dugite's `is_voter_disallowed` (conway.rs lines 371-383) does NOT check for `ParameterChange` on SPOs:

```rust
pub(super) fn is_voter_disallowed(voter: &Voter, action: &GovAction) -> bool {
    match (voter, action) {
        (_, GovAction::InfoAction) => false,
        (Voter::StakePool(_), GovAction::NewConstitution { .. }) => true,
        (Voter::StakePool(_), GovAction::TreasuryWithdrawals { .. }) => true,
        (Voter::ConstitutionalCommittee(_), GovAction::NoConfidence { .. }) => true,
        (Voter::ConstitutionalCommittee(_), GovAction::UpdateCommittee { .. }) => true,
        _ => false,
    }
}
```

An SPO voting on a non-SecurityGroup `ParameterChange` returns `false` (not disallowed), silently admitting the vote. Haskell would fire `DisallowedVoters`.

The one exception: if the PPU does touch a SecurityGroup parameter, SPO voting IS allowed. This means the check cannot be a static `(Voter::StakePool, GovAction::ParameterChange)` → disallow; it must inspect the PPU field set.

**Fix needed**: Extend `is_voter_disallowed` to handle `(Voter::StakePool, GovAction::ParameterChange { ppu, .. })`. If the PPU contains no SecurityGroup fields, return `true` (disallowed). Requires implementing a `ppu_is_security_group_relevant(ppu: &PParamsUpdate)` helper that mirrors Haskell's `isSecurityRelevant (PPGroups _ s)` / `modifiedPPGroups` logic.

### D-6 — VotingOnExpiredGovAction strict-greater semantics (confirmed correct)

Dugite's `is_vote_on_expired_action` (conway.rs line 489):

```rust
current_epoch > proposal.expires_after_epoch.0
```

This is `current_epoch > expires_after_epoch`, matching Haskell's `curEpoch <= gasExpiresAfter` (allowed when equal) exactly. The boundary semantics are correct.

### D-7 — reapplyTx: all GOV predicates run (confirmed correct)

As established in section 10, Haskell's GOV rule has no `(?!#)` static-labelled checks. Dugite's `validate_transaction_with_context` runs all voting predicates unconditionally for PV ≥ 9. This is correct.

The `committee_authorized_hot_keys` context field is populated in the mempool path (serve.rs lines 391-400) and is `None` during block-apply revalidation. When `None`, `is_voter_unknown` returns `false` (lenient default). This means reapplyTx in Dugite skips `VotersDoNotExist` for CC votes. This is a latent bug: if a resigned CC member's vote was admitted to the mempool, block revalidation would not catch it. However, block revalidation uses `tickThenReapply` which skips ALL validation (not just GOV), so this path is currently harmless.

### D-8 — Same-tx vote + proposal interaction (confirmed correct)

Dugite correctly treats proposals submitted in the same transaction as having their `GovActionId` resolvable for voting (via `local_proposals` in mod.rs line 1705). This mirrors Haskell where `proposals` at the point of the vote-processing fold already includes proposals added by `foldlM' processProposal` earlier in the same transaction.

---

## 12. Test Files That Pin Behaviour

Primary test file: `eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/GovSpec.hs`

Key test cases:

| Test (approx line) | What it pins |
|--------------------|--------------|
| "VotersDoNotExist" (L 747) | Unknown DRep/SPO/CC credential fires VotersDoNotExist; at PV11 also fires UnelectedCommitteeVoters |
| "expired gov-actions" (L 786, disabledInConformance) | VotingOnExpiredGovAction strict-`>` boundary |
| "non-existent gov-actions" (L 803) | GovActionsDoNotExist for a non-existent GovActionIx |
| "committee member can not vote on UpdateCommittee" (L 817) | DisallowedVoters for CC on UpdateCommittee |
| "committee member can not vote on NoConfidence" (L 825) | DisallowedVoters for CC on NoConfidence |
| "can submit SPO votes" (L 885) | SPO votes on a SecurityGroup ParameterChange succeed |
| Bootstrap "Parameter change" (L 1285+) | Bootstrap: CC cannot vote on UpdateCommittee even during bootstrap |

The "expired gov-actions" test is marked `disableInConformanceIt` (linked to formal-ledger-specifications issue #923) — the ImpSpec conformance suite does not currently test the expiry boundary. This means the conformance runner will not flag the ordering bug in D-2.

---

## 13. Quick Reference: Key Haskell Invariants for Dugite

1. **UnelectedCommitteeVoters fires first** (before VotersDoNotExist, before DisallowedVoters), but only at PV > 10.

2. **VotersDoNotExist fires before GovActionsDoNotExist** (the partition of unknown voters happens before the vote-to-action fold).

3. **VotingOnExpiredGovAction fires before DisallowedVoters** — a vote on an expired action is rejected by expiry, not by voter type.

4. **Resigned CC hot credentials are excluded from `knownCommitteeMembers`** → resigned voters always fire `VotersDoNotExist`, never `DisallowedVoters`.

5. **ParameterChange with non-SecurityGroup PPU**: SPO votes fire `DisallowedVoters`. The threshold is `NoVotingAllowed`, not `NoVotingThreshold`.

6. **All GOV checks run on reapplyTx** — no `lblStatic` labels in Gov.hs.

7. **Expiry boundary**: vote allowed when `currentEpoch == gasExpiresAfter` (strict `>` fires the failure).

8. **Proposals removed at epoch boundary** are no longer in `proposalsActionsMap` at the start of the next epoch. A vote on a removed proposal fires `GovActionsDoNotExist`.

9. **Same-tx vote on same-tx proposal**: legal — `processProposal` runs before the voting partition, so the new proposal is in `proposals` when the vote loop runs.
