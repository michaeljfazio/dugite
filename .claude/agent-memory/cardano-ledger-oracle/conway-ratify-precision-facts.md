---
name: conway-ratify-precision-facts
description: Live-GitHub-verified precision facts for Conway RATIFY/GOV internals — pvCanFollow exact modulus, HardFork proposal chaining, reorderActions stability, ensTreasury freeze timing, RatifyEnv frozen fields, committee minSize gate ordering, returnProposalDeposits unclaimed routing, rsDelayed mid-pass semantics
metadata:
  type: reference
---

Verified against IntersectMBO/cardano-ledger master as of 2026-07-04 (via cardano-haskell-oracle live fetch, not training-data recall). Complements [[oracle_ledger_governance]] and [[feedback_proposal_deposit_epoch_boundary]] with source-quoted precision on points those files only summarized.

## 1. `pvCanFollow` — exact modulus, not "any greater minor"

`eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs:299-307` (NOT in BaseTypes.hs):
```haskell
pvCanFollow (ProtVer curMajor curMinor) (ProtVer newMajor newMinor) =
  (succVersion curMajor, 0) == (Just newMajor, newMinor)
    || (curMajor, curMinor + 1) == (newMajor, newMinor)
```
Same-major bump requires new minor == curMinor + 1 EXACTLY. `newMinor > curMinor` (any gap) is illegal and must raise `ProposalCantFollow`.

## 2. HardFork proposal chaining — `preceedingHardFork`

**SUPERSEDED — see [[hardfork-pvcanfollow-exact-mechanics]] for the byte-exact, raw-source-verified (2026-07-06) version.** The summary below was imprecise: it omitted a second short-circuit branch. Correct 3-way resolution: (1) `mPrev == root` → base = live current PParams version; (2) `mPrev /= root` AND proposed major already exceeds `succVersion(current major)` (implausible jump) → base is ALSO forced to live current PParams version (chain lookup bypassed entirely — this prevents compounding two major bumps via chaining within one epoch); (3) `mPrev /= root` AND major-jump plausible → base = the in-flight parent's OWN target ProtVer via `proposalsLookupId`. Only case (3) uses the chain; case (2) looks superficially like chaining but isn't.

## 3. `reorderActions` — stable sort, ties keep insertion order

`Governance/Internal.hs:534-544`: `reorderActions = SS.fromList . sortOn (actionPriority . gasAction) . toList`. `Data.List.sortOn` is documented-stable. Priority: NoConfidence=0, UpdateCommittee=1, NewConstitution=2, HardForkInitiation=3, ParameterChange=4, TreasuryWithdrawals=5, InfoAction=6. Ties preserve on-chain submission/OMap-insertion order, never GovActionId order.

## 4. `ensTreasury` at pulser creation — NOT stale N-1 (corrects a plausible-but-wrong assumption)

`NewEpoch.hs:167-175`: `applyRUpd`/`updateRewards` (this boundary's RUPD `deltaT`) runs and mutates `casTreasuryL` BEFORE the EPOCH rule even starts. Inside `Epoch.hs` (POOLREAP refunds → enacted-withdrawal payouts subtracted → donations/unclaimed-deposit-refunds added), THEN at the end `setFreshDRepPulsingState eNo stakePoolDistr epochState2` builds the new pulser, and `Governance.hs:507-509` sets `dpEnactState = mkEnactState govState & ensTreasuryL .~ epochState ^. treasuryL`. So the pulser created at the E→E+1 boundary snapshots treasury AFTER this same boundary's RUPD + POOLREAP + withdrawal-payout + donation/unclaimed effects are all applied — it is current as of that boundary, not one epoch behind. If comparing to a Rust port, check whether the port's RUPD-equivalent and treasury-snapshot-for-pulser ordering match this sequence (RUPD/POOLREAP effects must land in the treasury value BEFORE the new pulser snapshot is taken).

## 5/6. `RatifyEnv` — every field frozen at pulser creation, never live

`Governance/Internal.hs:554-563`:
```haskell
data RatifyEnv era = RatifyEnv
  { reInstantStake, reStakePoolDistr, reDRepDistr, reDRepState
  , reCurrentEpoch, reCommitteeState, reAccounts, reStakePools }
```
Built once in `finishDRepPulser` (`DRepPulser.hs:398-408`) from `dp*` fields set at `setFreshDRepPulsingState`. `computeDRepDistr` (`DRepPulser.hs:200-241`) attributes proposal deposits (`dpProposalDeposits = proposalsDeposits props`, frozen at creation) against the frozen `dpAccounts`/instant-stake credential map — a mid-epoch DRep re-delegation on the return-address credential does NOT affect that epoch's already-running RATIFY pass.

## 7. Committee minSize gate STRICTLY PRECEDES zero-threshold auto-pass

`Governance/Internal.hs:444-480`:
```haskell
threshold = case committeeThreshold <$> committee of
  SJust t | hardforkConwayBootstrapPhase pv || activeCommitteeSize >= minSize -> VotingThreshold t
  _ -> NoVotingThreshold
```
`NoVotingThreshold -> SNothing -> committeeAccepted = False` unconditionally. The `r == minBound` (zero-threshold auto-pass) branch in `committeeAccepted` is only reachable AFTER a `VotingThreshold t` value has already survived the minSize-or-bootstrap gate. So: real committee + 0% threshold + activeSize < minSize (non-bootstrap) => REJECTED, never auto-passed.
- (b) minSize gate is SKIPPED during bootstrap (PV9): `hardforkConwayBootstrapPhase pv || activeCommitteeSize >= minSize` — bootstrap alone satisfies the OR.
- (c) active member predicate: cold key present in `hotKeys` map AND not `CommitteeMemberResigned` AND `currentEpoch <= validUntil`. A cold key with NO entry at all in hotKeys (never authorized, never resigned) counts as NOT active.

## 8. `returnProposalDeposits` — unregistered return account -> treasury

`Epoch.hs:179-193`: iterates only `allRemovedGovActions` (expired ∪ enacted ∪ enactment-removed siblings). Per action, `updateLookupAccountState` on the return credential; if the account lookup fails (Nothing), the deposit goes into an `unclaimed :: Map GovActionId Coin` instead of being paid out, and `unclaimed` is folded straight into `casTreasuryL` alongside `utxosDonationL`. Confirms/extends [[feedback_proposal_deposit_epoch_boundary]] fact 1 with the exact unregistered-account branch.

## 9. Expiry — strict `<`, `reCurrentEpoch` = new/incoming epoch number at creation

`Ratify.hs:357`: `if gasExpiresAfter < reCurrentEpoch then ... rsExpiredL %~ Set.insert gasId`. Strict less-than; expiring exactly AT reCurrentEpoch is not yet expired (matches [[feedback_proposal_deposit_epoch_boundary]] fact 4). `reCurrentEpoch` traces to `dpCurrentEpoch = eNo`, the SAME `eNo` NewEpoch.hs uses to set `nesEL`, i.e. the epoch number the boundary transitions INTO — fixed for that pulser's entire lifetime regardless of when `finishDRepPulser` actually completes.

## 10. Mid-pass RATIFY fold semantics

`Ratify.hs:318-360` (`ratifyTransition`, recursive over `RatifySignal`):
- (a) `rsEnactStateL` (ensCommittee/ensCurPParams/ensTreasury) IS updated via ENACT and threaded into the recursive call for the rest of the pass — later actions in the same pass see already-enacted committee/PParams changes.
- (b) `env :: RatifyEnv` (including `reCommitteeState`) is passed UNCHANGED through every recursive call — never rebuilt mid-pass. Hot-key authorization/resignation bookkeeping stays exactly as frozen at pulser creation even while `ensCommittee` membership changes mid-pass.
- (c) `delayingAction`: NoConfidence, HardForkInitiation, UpdateCommittee, NewConstitution = True; ParameterChange, TreasuryWithdrawals, InfoAction = False. Once an enacted delaying action sets `rsDelayedL .~ True`, `not rsDelayed` fails for every subsequent action in the SAME pass regardless of their own threshold outcome — they fall to the else-branch (expiry-check only, deferred to next epoch unless expired).

## Related
[[project_dugite_ratify_audit_divergences_2026_07_04]] — 3 concrete divergences found in dugite-ledger's governance.rs while cross-checking these facts against the Rust port on 2026-07-04.
