---
name: deleg-pool-govcert-verbatim-transitions
description: Verbatim conwayDelegTransition (Conway Deleg.hs), poolTransition (Shelley Pool.hs), conwayGovCertTransition (Conway GovCert.hs) with exact predicate-failure conditions, PV hardfork gates, and the general STS ?! non-short-circuit accumulation fact — live-verified 2026-08-05 at commit 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 (master HEAD at check time)
metadata:
  type: reference
---

## Pin
Verified live: `gh api repos/IntersectMBO/cardano-ledger/commits/4849c13d6f70e5ab46add9af6e0ec5c537b61f69`
resolves, dated 2026-08-04T21:48:51Z ("Merge pull request #5950 …"). Note:
`contents` API 404'd for these paths at this SHA for me — had to resolve via
`git/trees?recursive=1` to get blob SHAs, then `git/blobs/<sha>`. Blob SHAs
pinned below in case `contents` works next time and you want to skip the tree
walk.

These three functions are Conway/Shelley mainline code (not Dijkstra-specific)
— Dijkstra's SUBDELEG/SUBPOOL/SUBGOVCERT just do
`transitionRules = [Conway.conwayDelegTransition]` /
`[Shelley.poolTransition]` / `[Conway.conwayGovCertTransition]` literally,
confirmed in [[dijkstra-subtx-wire-and-sub-rule-chain]]. So this note applies
to **top-level Conway DELEG/GOVCERT and Shelley-through-Conway POOL** just as
much as to Dijkstra's Sub* wrappers.

## THE central mechanic: `?!` never short-circuits within a rule body

`libs/small-steps/src/Control/State/Transition/Extended.hs` (blob
`16fdf110794dcdce7b9f16b9945f55caf893ebd1`), `runClause`'s `Predicate` case:

```haskell
runClause (Predicate cond orElse val) =
  case vp of
    ValidateNone -> pure val
    _ -> case cond of
      Success x -> pure x
      Failure errs -> modify (first (map orElse (reverse (NE.toList errs)) <>)) >> pure val
```

On failure it accumulates into the state's failure list and still `pure val`s
— the do-block **continues**. So EVERY `?!`/`failOnJust`/`failBecause` call in
one certificate's handling always runs; all applicable failures for that one
cert are collected together, not just the first. Rejection is still
all-or-nothing at the top (`applySTSOptsEither`: `(_, pf:pfs) -> Left $ pf :|
pfs`), so accept/reject verdict is unaffected by fail-fast vs accumulate — but
the **returned failure LIST** (wire-visible in `MsgRejectTx`) differs if a
Rust port short-circuits on the first failing check instead of evaluating
every check and collecting all that apply. This is a GENERAL STS fact, not
specific to DELEG/POOL/GOVCERT — applies to every STS rule in the codebase.

## conwayDelegTransition (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs`, blob `19ab37b320f5ca20539ac91e9f996011415e701b`, 377 lines, fn at 177-301)

PV gate: `hardforkConwayDELEGIncorrectDepositsAndRefunds pv = pvMajor pv > natVersion @10` (Era.hs:266-267, PV11+).
`hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9` (Era.hs:257-258, exactly PV9).

- `ConwayRegCert stakeCred sMayDeposit`: `forM_ sMayDeposit checkDepositAgainstPParams` (only if SJust) then `checkStakeKeyNotRegistered`. Deposit mismatch: PV<=10 -> `IncorrectDepositDELEG Coin` (bad supplied value only); PV>=11 -> `DepositIncorrectDELEG (Mismatch RelEQ Coin)`. Both reachable, mutually exclusive by PV, not "one dead".
- `ConwayUnRegCert stakeCred sMayRefund`: `checkInvalidRefund` (Maybe-monad, short-circuits to Nothing/no-failure if account unregistered OR no refund supplied OR refund matches) fires refund mismatch: PV<=10 -> **reuses `IncorrectDepositDELEG suppliedRefund`** (same ctor as deposit case, ambiguous payload); PV>=11 -> `RefundIncorrectDELEG (Mismatch RelEQ Coin)` against the account's OWN recorded `depositAccountStateL`, not the live pparam. Then `checkStakeKeyHasZeroRewardBalance` (also Maybe-gated on account existing) -> `StakeKeyHasNonZeroAccountBalanceDELEG`. Then `case mAccountState of Nothing -> failBecause StakeKeyNotRegisteredDELEG`. When unregistered, the first two checks are structurally None (guarded by `accountState <- mAccountState` inside their Maybe-do), so ONLY `StakeKeyNotRegisteredDELEG` fires.
- `ConwayDelegCert stakeCred delegatee` (plain, no registration): `checkStakeDelegateeRegistered delegatee` THEN `lookupAccountStateIntern stakeCred accounts` — `Nothing -> failBecause StakeKeyNotRegisteredDELEG`. So plain delegation DOES require prior registration, enforced via account lookup, not the `checkStakeKeyNotRegistered` helper (which enforces the opposite — used only by Reg*).
- `ConwayRegDelegCert stakeCred delegatee deposit`: `checkDepositAgainstPParams deposit` (always, deposit is `Coin` not `StrictMaybe`) -> `checkStakeKeyNotRegistered` -> `checkStakeDelegateeRegistered`. All three accumulate.
- `checkStakeDelegateeRegistered`: `DelegStake pool` -> `checkPoolRegistered` only (`Map.member pools`, unconditional, -> `DelegateeStakePoolNotRegisteredDELEG`). `DelegVote drep` -> `checkDRepRegistered` only. `DelegStakeVote pool drep` -> BOTH (`checkPoolRegistered >> checkDRepRegistered`), can both fire together. `checkDRepRegistered` is `unless (hardforkConwayBootstrapPhase pv) $ ...` — SKIPPED entirely at PV9, active PV>=10 (moot pre-Conway).
- `pvMajor pv < natVersion @10` threaded into `processDelegationInternal` as `preserveIncorrectDelegation` (ConwayDelegCert/ConwayRegDelegCert only) — deliberately preserved historical reverse-DRep-delegation bug (#4772) for PV9. Not a predicate failure but a state-mutation divergence.

CBOR tags for `ConwayDelegPredFailure`: **1-8** (no tag 0).

## poolTransition (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`, blob `320047a02231f1cf0716cca65fe45c1a9c1c7f5b`, 324 lines, fn at 209-324)

PV gates: `hardforkAlonzoValidatePoolAccountAddressNetID pv = pvMajor pv > natVersion @4` (PV5+); `SoftForks.restrictPoolMetadataHash pv = pv > ProtVer (natVersion @4) 0` (PV5+); `hardforkConwayDisallowDuplicatedVRFKeys pv = pvMajor pv > natVersion @10` (PV11+).

- `StakePoolCostTooLowPOOL`: unconditional, `sppCost >= minPoolCost` (RelGTEQ).
- `WrongNetworkPOOL`: PV5+ only, `actualNetID (Globals.networkId) == suppliedNetID (aaNetworkId sppAccountAddress)`.
- `PoolMedataHashTooBig` (typo verbatim, upstream): PV5+ only, only runs when `sppMetadata` is SJust (`forM_`, skipped not vacuous-pass if Nothing); `sizeofByteArray (pmHash pmd) <= hashSize @HASH` (32 for Blake2b-256).
- `VRFKeyHashAlreadyRegistered`: registry = `psVRFKeyHashes`, GLOBAL map across ALL currently-registered pools' VRF keys (PState-level). New pool: `Map.notMember sppVrf psVRFKeyHashes`. Re-registration of EXISTING pool: `sppVrf == stakePoolState^.spsVrfL || Map.notMember sppVrf psVRFKeyHashes` — self-reuse of the pool's OWN current key is always fine (first disjunct), else falls through to the SAME global dedup check (not narrower). `psFutureStakePoolParams` consulted to retract a same-epoch previously-queued VRF key before inserting new one (state hygiene, not itself a predicate).
- `StakePoolNotRegisteredOnKeyPOOL`: `Map.member sppId psStakePools`.
- `StakePoolRetirementWrongEpochPOOL`: exact range `cEpoch < e && e <= limitEpoch` where `limitEpoch = addEpochInterval cEpoch (pp^.ppEMaxL)` — i.e. `currentEpoch < retirementEpoch <= currentEpoch + eMax`, pparam `ppEMaxL`/wire name `eMax`. Single combined `?!` (not two independent checks) constructing both `Mismatch RelGT EpochNo {supplied=e,expected=cEpoch}` and `Mismatch RelLTEQ EpochNo {supplied=e,expected=limitEpoch}` together.

CBOR: constructor is reused by Conway POOL too ("ShelleyPoolPredFailure is used in Conway POOL rule" — comment above the DecCBOR instance), so wire encoding must stay stable across eras.

## conwayGovCertTransition (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs`, blob `99ac21b7b0fd8c065e2e612dbbf59f6f14e0c44b`, 306 lines, fn at 170-276)

- `RegDRep`: `ConwayDRepAlreadyRegistered` on `Map.member cred vsDReps`. `ConwayDRepIncorrectDeposit` against CURRENT `cgcePParams^.ppDRepDepositCompactL` (nothing stored yet). Both accumulate.
- `UnRegDRep`: `ConwayDRepNotRegistered` on `mDRepState == Nothing`. `ConwayDRepIncorrectRefund` against the DRep's OWN stored `drepDepositL` (NOT the live pparam) — Maybe-gated so no failure computed at all when unregistered (same pattern as DELEG's checkInvalidRefund).
- `UpdateDRep`: requires prior registration (`ConwayDRepNotRegistered` if not). No deposit. Recomputes `drepExpiry` via UNVERSIONED `computeDRepExpiry` (always applies dormant correction) vs registration's `computeDRepExpiryVersioned` (skips correction at PV9). See [[drep-expiry-numDormantEpochs]] for full formula.
- `AuthCommitteeHotCert` / `ResignCommitteeColdCert`: IDENTICAL shared code path `checkAndOverwriteCommitteeMemberState coldCred newMemberState`, differing only in `newMemberState` value (`CommitteeHotCredential hotCred` vs `CommitteeMemberResigned anchor`).
  - `ConwayCommitteeHasPreviouslyResigned` checked FIRST via `Map.lookup coldCred csCommitteeCreds` (CertState's `vsCommitteeState`) — `CommitteeMemberResigned{} -> Just coldCred` (fires), `CommitteeHotCredential{} -> Nothing`, absent key -> Nothing (not treated as resigned). Applies to BOTH Auth and Resign — a resigned cold cred can never re-authorize a hot key, permanently (nothing clears it back).
  - `ConwayCommitteeIsUnknown` checked against `isCurrentMember (Map.member coldCred . committeeMembers $ cgceCurrentCommittee) || isPotentialFutureMember (any UpdateCommittee proposal in cgceCommitteeProposals naming coldCred in its newMembers)` — the LIVE enacted Committee + pending proposals, NOT the CertState's own `vsCommitteeState` map (which can hold stale/resigned/expired entries). Applies to BOTH Auth and Resign identically.
  - Re-authorizing an already-`CommitteeHotCredential` member with a fresh hot key: unconditionally allowed (no resigned trip, isCurrentMember true).

CBOR tags for `ConwayGovCertPredFailure`: **0-5** (starts at 0, unlike Deleg's 1-8).

## Q4 — completeness

All three functions are LEAF rules — zero `trans @(EraRule "X" era)` calls, zero cross-rule `wrapFailed`. Every failure injects that same rule's own PredicateFailure ADT via `injectFailure`. Constructor lists in [[dijkstra-subtx-wire-and-sub-rule-chain]] (8/6/6 ctors respectively) are confirmed EXHAUSTIVE at this SHA. Only non-failure aside: `poolTransition` emits `PoolEvent` (`RegisterPool`/`ReregisterPool`) via `tellEvent` on success — events, not predicate failures, no equivalent in Deleg/GovCert (`type Event (DELEG era) = Void`, same for GOVCERT).

## Wire tags cross-check (Conway/TxCert.hs, blob `5b6e5f55b08fe8291e8207420f81d7fdcaaeec00`)
`ConwayRegCert cred SNothing` / `ConwayUnRegCert cred SNothing` / `ConwayDelegCert cred (DelegStake _)` reuse LEGACY Shelley cert encoding (not tags 7/8/9). New-in-Conway tags only apply for the SJust/DelegVote/DelegStakeVote/RegDelegCert forms: 7=`ConwayRegCert(SJust deposit)`, 8=`ConwayUnRegCert(SJust deposit)`, 9=`ConwayDelegCert(DelegVote)`, 10=`ConwayDelegCert(DelegStakeVote)`, 11/12/13=`ConwayRegDelegCert` (DelegStake/DelegVote/DelegStakeVote variants).
