---
name: treasury-withdrawal-enactment-mechanics
description: Verbatim Conway TreasuryWithdrawals enactment path @ faa7a9dc — ENACT accumulates ensWithdrawals, EPOCH applies AFTER SNAP; ensTreasury frozen at setFreshDRepPulsingState; SPO threshold = NoVotingAllowed -> auto-yes; rsDelayed latch; final RatifyState ensTreasury zeroed
metadata:
  type: project
---

Live-verified 2026-08-15 at pin `faa7a9dc347697b11d4da5b7818b1731e11aeeef` (cardano-node 11.0.1).

**ENACT does NOT touch the real pots.** `Enact.hs:97-103`: `TreasuryWithdrawals wdrls _` accumulates
`ensWithdrawals = Map.unionWith (<>) (Map.mapKeys raCredential wdrls) ensWithdrawals` and decrements the
FROZEN `ensTreasury` (this is also the cumulative sufficiency budget for later withdrawals in the same run).
Real state mutates only in `Epoch.hs`:

Order inside `epochTransition` (`Conway/Rules/Epoch.hs:276-379`):
1. SNAP over `ledgerState0` (line 292-293) — so the withdrawal credit and deposit refund of THIS boundary are
   NOT in this boundary's mark snapshot; they reach ssStake only at the NEXT boundary. (`ssStake` DOES include
   account balances: `resolveInstantStake` merges accounts, `core State/Stake.hs:124-150`.)
2. POOLREAP (295-297).
3. `extractDRepPulsingState` (301-304) — RATIFY already ran inside `finishDRepPulser`; EPOCH only APPLIES.
4. `applyEnactedWithdrawals` (306-307, def 216-249): `casTreasuryL %~ (<-> fold successfulWithdrawls)`;
   credit ONLY registered accounts (`guard (isAccountRegistered ...)`); unregistered withdrawals silently
   remain in treasury; resets `ensWithdrawals=empty`, `ensTreasury=mempty`.
5. `proposalsApplyEnactment` removes enacted/expired proposals (321-322).
6. `returnProposalDeposits` (335-336, def 186-200): refund `balanceAccountStateL <>~ gasDeposit` to
   `raCredential (gasReturnAddr gas)`; unregistered return account -> `unclaimed` -> treasury (354-357).
7. `utxosDepositedL .~ totalObligation certState2 govState1` (358-360) — oblProposal shrinks.
8. HARDFORK if PV changed (374-378), then `setFreshDRepPulsingState eNo stakePoolDistr epochState2` (379)
   — the NEW pulser snapshots accounts AFTER the credit, so the same-boundary psDRepDistr includes it.

**ensTreasury source** (`Governance.hs:509-511`): `mkEnactState govState & ensTreasuryL .~ epochState ^. treasuryL`
at pulser creation = live treasury at the PREVIOUS boundary (post-RUPD, post-that-boundary's-enactment).
`mkEnactState` itself seeds `ensTreasury = zero` (Governance.hs:334). RATIFY's Empty base case ZEROES
`ensTreasury` (`Ratify.hs:359`) so the stored/`nextRatifyState` EnactState always shows treasury 0.

**RATIFY gate for TreasuryWithdrawals** (`Ratify.hs:336-340`), all conjuncts:
`prevActionAsExpected` (trivially True — `withGovActionParent ... TreasuryWithdrawals _ _ -> noParent`,
Procedures.hs:789) && `validCommitteeTerm` (True for TW) && `not rsDelayed` && `withdrawalCanWithdraw`
(sum <= frozen-and-decremented ensTreasury, Ratify.hs:291-294) && `acceptedByEveryone`:
- CC: committee's own threshold; requires `activeCommitteeSize >= ppCommitteeMinSize` (bootstrap bypasses
  size gate) else SNothing -> False (Internal.hs:460-487).
- SPO: `NoVotingAllowed` -> `SJust minBound` -> auto-accept, SPO votes irrelevant (Internal.hs:413, 346-350).
- DRep: `dvtTreasuryWithdrawal` (Internal.hs:539); bootstrap (pvMajor 9) zeroes DRep thresholds but TW can't
  be SUBMITTED at PV9 anyway.
`rsDelayed` latch: `rsDelayedL .~ delayingAction govAction` on each enactment; delaying actions are
priorities 0-3 in `reorderActions` (TW = 5), so a same-run delaying enactment blocks TW that boundary.
Expiry (`gasExpiresAfter < reCurrentEpoch`) is tested only on the NOT-ratified branch (#990 final-chance).

**Timing**: pulser created at boundary E-1->E freezes proposals+votes+accounts+drepState+committeeState+
treasury as of that instant (`dpCurrentEpoch = E`); pulsed on every non-boundary TICK
(`NewEpoch.hs:163-168`); consumed at boundary E->E+1. Votes cast during epoch E only count at E+1->E+2.
NEWEPOCH applies RUPD (es1) BEFORE EPOCH (NewEpoch.hs:170-177), so mark includes reward payouts but not
same-boundary withdrawal credits/deposit refunds.

**Dump-diff signature of one-boundary-early enactment** (first-block-of-epoch dumps): treasury -X,
deposits.proposal/-total -deposit, drepDistr sum +X exactly (deposit nets out: counted via
dpProposalDeposits on the not-yet-enacted side vs refunded balance on the enacted side, same DRep either
way), snapshots.{mark,set,go} UNCHANGED at that epoch (SNAP-before-credit) — first stake divergence would be
the NEXT epoch's mark. Used for the epoch-577 mainnet divergence analysis.
