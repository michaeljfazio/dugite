---
name: treasury-withdrawals-ratify-treasury-snapshot-lag
description: Two independent, compounding reasons a TreasuryWithdrawals action can fail to ratify on a fresh devnet even with DRep+CC yes votes -- (1) RATIFY's ensTreasury is a snapshot frozen ONE epoch-boundary before the boundary it's consumed at, so a just-applied RUPD's proceeds are invisible to that same pass's affordability check; (2) dRepAcceptedRatio's yes/(yes+no) uses the safe-ratio operator (%?) which returns exactly 0 (not 1, not an error) when the denominator is 0, and stake never delegated to any DRep contributes literally nothing to reDRepDistr regardless of how that DRep voted. Live-verified 2026-08-02 against IntersectMBO/cardano-ledger at dugite's pinned conformance sha a88b60bdcf3248dfe5a2f9372c188c399233f479.
metadata:
  type: reference
---

Built to answer: why did a TreasuryWithdrawals action proposed in epoch 0,
with DRep+CC yes votes, fail to enact at the 1->2 boundary, when the devnet's
20 genesis delegators only ever delegated STAKE (to pool1) and never cast a
`vote_delegation` cert to any DRep. All source fetched live via `gh api
repos/IntersectMBO/cardano-ledger/contents/...?ref=a88b60bdcf3248dfe5a2f9372c188c399233f479`
(see [[kb-table-files-missing-use-live-github]] for the fetch method).

## Fact 1 — `dRepAcceptedRatio` on zero stake returns exactly 0, via a safe-ratio operator

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ratify.hs:252-281`:

```haskell
dRepAcceptedRatio RatifyEnv {reDRepDistr, reDRepState, reCurrentEpoch} gasDRepVotes govAction =
  toInteger yesStake %? toInteger totalExcludingAbstainStake
  where
    accumStake (!yes, !tot) drep (CompactCoin stake) =
      case drep of
        DRepCredential cred -> case Map.lookup cred reDRepState of
          Nothing -> (yes, tot)  -- not registered, don't consider
          Just drepState
            | reCurrentEpoch > drepExpiry drepState -> (yes, tot)  -- expired
            | otherwise -> case Map.lookup cred gasDRepVotes of
                Nothing -> (yes, tot + stake)      -- no vote -> counts as No
                Just VoteYes -> (yes + stake, tot + stake)
                Just Abstain -> (yes, tot)
                Just VoteNo -> (yes, tot + stake)
        DRepAlwaysNoConfidence -> case govAction of
          NoConfidence _ -> (yes + stake, tot + stake)
          _ -> (yes, tot + stake)
        DRepAlwaysAbstain -> (yes, tot)
    (yesStake, totalExcludingAbstainStake) = Map.foldlWithKey' accumStake (0, 0) reDRepDistr
```

**Critically, the fold is over `reDRepDistr` (the stake distribution), not over
`gasDRepVotes` (who voted).** A DRep credential that received a `VoteYes` but
has ZERO weight in `reDRepDistr` never enters the fold at all — its vote is
completely inert. This is not a special case in the code; it falls out of
folding over the distribution map as the source of truth.

The `%?` operator (`libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes/NonZero.hs:148-153`):

```haskell
infixl 7 %?
(%?) :: Integral a => a -> a -> Ratio a
x %? y
  | y == 0 = 0
  | otherwise = x % y
```

So `0 %? 0 = 0` — not `1`, not a runtime error (plain `Data.Ratio.%` throws
"Ratio has zero denominator" on `y==0`; `%?` exists specifically to avoid
that). Then in `dRepAccepted` (`Ratify.hs:228-238`):

```haskell
dRepAccepted re rs GovActionState {gasDRepVotes, gasProposalProcedure} =
  case votingDRepThreshold rs govAction of
    SJust r -> r == minBound || dRepAcceptedRatio re gasDRepVotes govAction >= unboundRational r
    SNothing -> False
```

Ratio `0 >= threshold` is `False` for any `threshold > 0`. So unless
`dvtTreasuryWithdrawal` is configured to a literal `0` (auto-pass, degenerate),
**a devnet where no stake credential has ever delegated voting power to any
DRep can NEVER satisfy the DRep threshold for ANY action type**, no matter how
many DReps register or how they vote. This is structural, not an epoch-timing
issue — it does not resolve itself by waiting.

## Fact 2 — why `reDRepDistr` is literally empty when no vote_delegation cert was ever submitted

`computeDRepDistr` (`eras/conway/impl/src/Cardano/Ledger/Conway/Governance/DRepPulser.hs:200-241`)
folds over every registered stake **account**, not over DReps:

```haskell
addToDRepDistr accountState stakeAndDeposits distr = fromMaybe distr $ do
  dRep <- accountState ^. dRepDelegationAccountStateL   -- Maybe short-circuit
  let balance = accountState ^. balanceAccountStateL
      updatedDistr = Map.insertWith (<>) dRep (stakeAndDeposits <> balance) distr
  Just $ case dRep of ...
```

If `dRepDelegationAccountStateL` is `Nothing` (no `vote_delegation` cert ever
submitted for that credential — stake delegation to a pool via
`stake_delegation` is a completely separate cert/field), the whole `do` block
short-circuits to `Nothing`, `fromMaybe distr` returns `distr` **unchanged** —
that account contributes ZERO to the DRep distribution, full stop, regardless
of pool delegation, account balance, or anything else. 20 genesis delegators
who only ever delegated to pool1 (never voted their DRep power) contribute
nothing. Confirms the setup's premise is fatal by itself.

## Fact 3 — RATIFY's `ensTreasury` is frozen ONE epoch-boundary before the boundary it's used at (independent, compounding cause)

The DRep pulser snapshots its whole RATIFY environment, including the
treasury, at CREATION time and never refreshes it:

`DRepPulser` record (`DRepPulser.hs:272-273`):
```haskell
, dpEnactState :: !(EnactState era)
-- ^ Snapshot of the EnactState, Used to build the Env of the RATIFY rule
```

`finishDRepPulser`'s `DRPulsing` arm (`DRepPulser.hs:410-417`) builds the
`RatifyState` fed to `runConwayRatify` as `rsEnactState = dpEnactState` —
straight from the frozen field, not re-derived from any live state at
extraction time, even though extraction can happen much later (at the epoch
boundary, forced synchronously if pulsing somehow hasn't finished).

Where `dpEnactState`'s treasury comes from —
`setFreshDRepPulsingState` (`eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs:458-516`):

```haskell
dpEnactState =
  mkEnactState govState
    & ensTreasuryL .~ epochState ^. treasuryL
```

`setFreshDRepPulsingState` is called from exactly one place, the LAST line of
`epochTransition` (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs:372`):

```haskell
liftSTS $ setFreshDRepPulsingState eNo stakePoolDistr epochState2
```

`epochState2` is EPOCH's own final state for THIS boundary — i.e. it already
reflects THIS boundary's `applyEnactedWithdrawals`, `proposalsApplyEnactment`,
committee/pparams writes, and (crucially) THIS boundary's already-applied
RUPD, because EPOCH's *input* (`epochState0`, aliased through to `epochState2`)
is `es1` from the calling NEWEPOCH transition, and `es1` is the state
AFTER RUPD has already been folded in.

Proof from `newEpochTransition`
(`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs:154-188`):

```haskell
es1 <- case ru of  -- Here is where we extract the result of Reward pulsing.
  SNothing -> pure es0
  SJust p@(Pulsing _ _) -> do
    (ans, event) <- liftSTS (completeRupd p)
    ...
    updateRewards es0 eNo ans
  SJust (Complete ru') -> updateRewards es0 eNo ru'
es2 <- trans @(EraRule "EPOCH" era) $ TRC ((), es1, eNo)   -- EPOCH gets es1, RUPD already applied
```

**So the pulser CREATED at boundary N->(N+1) freezes the treasury as it stood
immediately AFTER boundary N->(N+1)'s own RUPD.** That pulser runs during
epoch N+1 and is CONSUMED (its RatifyState extracted and `withdrawalCanWithdraw`
already having been evaluated against that frozen treasury) at the NEXT
boundary, (N+1)->(N+2). **The treasury inflow from the (N+1)->(N+2) boundary's
OWN RUPD — the "first RUPD" in this devnet's case — is NOT visible to the
`withdrawalCanWithdraw` check consumed at that very same boundary.** It is
visible only to the pulser created at THAT boundary, i.e. first usable at the
FOLLOWING boundary, (N+2)->(N+3).

Applied to the reported scenario (proposal submitted epoch 0, treasury 0 at
that point, first real RUPD applied at the 1->2 boundary funding treasury to
3,311,997,088,120):
- Pulser P1 created at 0->1 boundary, `ensTreasury` frozen at ~0 (epoch 0's
  fees hadn't been through a real RUPD yet).
- P1 runs during epoch 1, decides `withdrawalCanWithdraw amount 0 = False`
  for any positive withdrawal — sealed, regardless of votes.
- At the 1->2 boundary, NEWEPOCH applies the real RUPD first (es1 treasury =
  3.3T), THEN EPOCH extracts P1's already-sealed (and already-failing) result.
  **The withdrawal fails this pass even though the boundary's own block state
  shows a fully-funded treasury**, because RATIFY never looked at that number.
- Pulser P2, created at the END of this same 1->2 boundary, freezes
  `ensTreasury ≈ 3.3T` (post this pass's, zero, withdrawals). P2 runs during
  epoch 2; if the DRep-stake problem (Fact 1/2) is ALSO fixed by then, the
  STILL-LIVE proposal (its already-cast DRep+CC yes votes carry over
  unchanged in `cgsProposalsL`, no need to re-vote) would finally see
  `withdrawalCanWithdraw amount 3.3T = True` at the 2->3 boundary.

`ratifyTransition`'s else-branch (`Ratify.hs:353-359`) confirms this is a
silent per-pass retry, not a rejection or drop: the action stays in
`cgsProposalsL` (not added to `rsEnacted` or `rsExpired`) and is picked up
fresh, with a fresh `ensTreasury`, by the next pulser — until it succeeds or
crosses `gasExpiresAfter`.

## Fact 4 — threshold matrix specific to TreasuryWithdrawals (`Governance/Internal.hs`)

- SPO: `votingStakePoolThresholdInternal` (`Internal.hs:395-406`) ->
  `TreasuryWithdrawals {} -> NoVotingAllowed`. SPOs cannot vote on this action
  type AT ALL — a pool operator voting on a `TreasuryWithdrawals` proposal is
  a hard hard Phase-1 `DisallowedVoters` rejection at submission, not merely
  "uncounted." SPO participation is irrelevant to whether it ratifies.
- CC: `votingCommitteeThresholdInternal` (`Internal.hs:444-470`) ->
  `TreasuryWithdrawals {} -> threshold`, where `threshold` resolves from
  `committeeThreshold <$> committee` gated by the minSize-or-bootstrap check.
  `committeeThreshold :: !UnitInterval` is a SINGLE flat field on the
  `Committee` record (`Governance/Procedures.hs:563`) — one ratio for the
  WHOLE committee, not a per-action-type PParams field. This is exactly the
  "threshold 1/2" configured in the scenario's conway-genesis.
- DRep: `votingDRepThresholdInternal` (`Internal.hs:504-532`) ->
  `TreasuryWithdrawals {} -> VotingThreshold dvtTreasuryWithdrawal`, one field
  of the 10-field `DRepVotingThresholds` record, PParams key 26.

`acceptedByEveryone` (`Ratify.hs:297-306`) ANDs all three
(`committeeAccepted && spoAccepted && dRepAccepted`). Verified exactly how
SPO's `NoVotingAllowed` flows through — `toRatifyVotingThreshold`
(`Internal.hs:338-342`):

```haskell
toRatifyVotingThreshold = \case
  VotingThreshold t -> SJust t
  NoVotingThreshold -> SNothing         -- no voting threshold prevents ratification
  NoVotingAllowed -> SJust minBound     -- votes should not count, set threshold to zero
```

So `votingStakePoolThreshold` for `TreasuryWithdrawals` returns `SJust
minBound` (NOT `SNothing`). In `spoAccepted` (`Ratify.hs:165-176`):
`SJust r -> r == minBound || ...` — `r == minBound` is `True`, so `spoAccepted`
short-circuits to unconditional `True` via `||` without ever evaluating
`spoAcceptedRatio`. **SPO participation for TreasuryWithdrawals is not merely
irrelevant — it is forced to auto-pass (vacuous truth), by the exact same
zero-threshold mechanic documented for the committee/DRep short-circuits
elsewhere in this file.** Only CC (`committeeAccepted`) and DRep
(`dRepAccepted`) can actually block a TreasuryWithdrawals action.

## Precondition checklist this reference exists to support

See the answer given in-conversation 2026-08-02 for the full checklist; the
two load-bearing, independently-sufficient preconditions are:
1. Nonzero stake must be `vote_delegation`-delegated to a REGISTERED, not
   expired DRep (or use `always-abstain`/`always-no-confidence` deliberately,
   though those don't help a normal yes-vote), landed on-chain before the
   epoch boundary that creates the pulser which will later be consumed for
   this proposal's ratification pass.
2. The treasury amount being withdrawn must be `<=` the treasury value as it
   stood at the boundary ONE EPOCH BEFORE the boundary where ratification is
   being observed — i.e. budget for an extra epoch of lag beyond the normal
   pulser-freeze delay already documented in
   [[reference_conway_gov_enactment_timing]] project memory (dugite repo
   root `~/.claude/projects/-Users-michaelfazio-Source-dugite/memory/`).

## Related
[[kb-table-files-missing-use-live-github]] — fetch method, sha pin.
Cross-references `~/.claude/projects/-Users-michaelfazio-Source-dugite/memory/reference_conway_gov_enactment_timing.md`
(pulser-freeze E->E+2 timing, `withdrawalCanWithdraw` existence) and
`.../reference_conway_gov_predicate_failures.md` (submission-time gates) —
this file adds the byte-exact zero-stake-ratio and treasury-snapshot-lag
mechanics those two didn't drill into.
