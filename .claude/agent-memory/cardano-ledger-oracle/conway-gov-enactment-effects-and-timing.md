---
name: conway-gov-enactment-effects-and-timing
description: Byte-exact per-constructor ENACT effects for all 7 Conway GovAction types, exact epoch-boundary timing (ratify==enact==commit, all in the same EPOCH-rule step), treasury-withdrawal capacity gate, InfoAction unratifiability, single-delaying-action-per-boundary rule, UpdateCommittee add/remove union semantics, prev-action-id purpose lineage. Live-verified 2026-08-02, HEAD and dugite's pinned conformance SHA (a88b60bdcf3248dfe5a2f9372c188c399233f479) byte-identical for all files cited.
metadata:
  type: reference
---

Verified by fetching raw source (not summarizing) from IntersectMBO/cardano-ledger, both HEAD
(`4f7cb2d6874df70561e32147084ed82cee773e8a`, 2026-08-02) and dugite's own conformance-corpus pin
(`tests/conformance/upstream/sources.toml` `[cardano-ledger] sha`) — `diff` showed byte-identical for
every file below at both refs, so no drift risk between "current Haskell master" and "what dugite's
own corpus tests against". Triggered by a user request for devnet-test-grade enactment semantics for
all 7 `GovAction` constructors.

Files: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/{Enact,Ratify,Epoch}.hs`,
`.../Conway/Governance.hs`, `.../Conway/Governance/{Internal,Procedures}.hs`.

## 1. ENACT is total and per-constructor (`Rules/Enact.hs:83-134`, verbatim already in this dir's
sibling notes — see [[bounded-ratio-decode-and-enact-totality]] for the totality proof). Exact effects:

- `ParameterChange _ ppup _` → `ensCurPParamsL %~ (applyPPUpdates ppup)`, `ensPrevPParamUpdateL .~ SJust (GovPurposeId gaid)`.
- `HardForkInitiation _ pv` → `ensProtVerL .~ pv` (== `ensCurPParamsL . ppProtocolVersionL`), `ensPrevHardForkL .~ SJust (GovPurposeId gaid)`.
- `TreasuryWithdrawals wdrls _` → `ensWithdrawalsL %~ Map.unionWith (<>) (Map.mapKeys (^.accountAddressCredentialL) wdrls)` (accumulates, doesn't replace; strips network-id byte down to the staking `Credential`) AND `ensTreasuryL %~ (<-> fold wdrls)` — this is a **bookkeeping-only** debit against `EnactState`'s own `ensTreasury` copy, not the real pot (see §3).
- `NoConfidence _` → `ensCommitteeL .~ SNothing`, `ensPrevCommitteeL .~ SJust (GovPurposeId gaid)`. Empties the committee to `Nothing`, not a distinguished "no confidence" sentinel value.
- `UpdateCommittee _ removed added newThreshold` → `ensCommitteeL %~ SJust . updatedCommittee removed added newThreshold`, `ensPrevCommitteeL .~ SJust (GovPurposeId gaid)`. `updatedCommittee`: if current is `SNothing`, result = `Committee added newThreshold` (remove-set irrelevant, nothing to remove from). If `SJust (Committee current _)`: `newMembers = Map.union added (current `Map.withoutKeys` removed)`. **`Map.union` is left-biased** — a credential in BOTH `removed` and `added` ends up ADDED (with `added`'s expiry epoch); add wins over remove, unconditionally, no proposal-submission-time rejection for this overlap (`actionWellFormed`, `Rules/Gov.hs:393-399`, only checks `ParameterChange`'s `ppuWellFormed`; every other constructor including `UpdateCommittee` is unconditionally well-formed).
- `NewConstitution _ c` → `ensConstitutionL .~ c` (direct replace, no merge/append), `ensPrevConstitutionL .~ SJust (GovPurposeId gaid)`.
- `InfoAction` → `st` unchanged. Literally a no-op arm.

`EnactState` fields (`Governance/Internal.hs:129-140`, CBOR `Rec` array(7) in this exact order):
`ensCommittee :: StrictMaybe (Committee era)`, `ensConstitution :: Constitution era`,
`ensCurPParams`, `ensPrevPParams :: PParams era`, `ensTreasury :: Coin`,
`ensWithdrawals :: Map (Credential Staking) Coin`, `ensPrevGovActionIds :: GovRelation StrictMaybe`.

## 2. Epoch-boundary sequencing — ratify, enact, and commit-to-live-state are ALL the same EPOCH-rule step

`Rules/Epoch.hs::epochTransition` (not `Rules/Ratify.hs` — RATIFY is never invoked via `trans` inside
`epochTransition`; its result is pre-computed by the DRep **pulser**, see §5) does, in this exact order:

1. SNAP, then POOLREAP.
2. `ratifyState@RatifyState{rsEnactState, rsEnacted, rsExpired} = extractDRepPulsingState pulsingState`
   — extracts the (by now fully computed) result of the PREVIOUS-boundary pulser. If pulsing somehow
   hasn't finished, this call forces the remainder synchronously (referentially transparent, same
   answer either way — see §5).
3. `applyEnactedWithdrawals` — see §3, real treasury debit + reward-account credit happens HERE.
4. `proposalsApplyEnactment rsEnacted rsExpired (govState0 ^. proposalsGovStateL)` — applied to the
   **live** `cgsProposalsL` (a superset of the pulser's frozen `dpProposals`, see §5), not the frozen
   copy, so votes cast during the pulsing epoch on still-pending actions survive.
5. `govState1 = govState0 & cgsProposalsL .~ newProposals & cgsCommitteeL .~ ensCommittee & cgsConstitutionL .~ ensConstitution & cgsCurPParamsL .~ nextEpochPParams govState0 & cgsPrevPParamsL .~ curPParams & cgsFuturePParamsL .~ PotentialPParamsUpdate Nothing`
   — **committee, constitution, and pparams are written into the live, queryable `ConwayGovState` in
   this exact step**, atomically, all at once.
6. `returnProposalDeposits` on `enactedActions ∪ removedDueToEnactment ∪ expiredActions` — see §4.
7. `updateCommitteeState` prunes `VState.vsCommitteeStateL` (hot-key auth/resignation map) to
   `Map.intersection creds (foldMap committeeMembers newCommittee)` — a cold credential that is no
   longer a committee member (removed by `NoConfidence`/`UpdateCommittee`, or never re-added) is
   **deleted from `CommitteeState` outright**, not tombstoned. `query committee-state` for a removed
   member returns absent, not present-with-a-removed-marker.
8. HARDFORK rule fires iff `epochState1^.curPParamsEpochStateL.ppProtocolVersionL /= prevPParamsL`'s
   — i.e. exactly when step 5's `nextEpochPParams` swap actually changed the protocol version.
9. `setFreshDRepPulsingState eNo stakePoolDistr epochState2` — creates the NEW pulser from the
   NOW-current (post-step-4/5) live `cgsProposalsL`, becomes the candidate set for the FOLLOWING
   boundary.

**`nextEpochPParams` (`libs/cardano-ledger-core/.../State/Governance.hs:121-133`) resolves to
`ensCurPParams` of this exact same `rsEnactState`** — it reads `cgsFuturePParamsL`, which was set to
`PotentialPParamsUpdate <thunk over ensCurPParams (rsEnactState of the SAME pulser)>` back when this
pulser was created (`predictFuturePParams`, `Governance.hs:299-321`) and force-evaluated once by
`solidifyFuturePParams` in TICK 2 stability windows before this same epoch's end (purely for HFC
lead-time, memoized/referentially-transparent — not a different or earlier value). **Net: there is no
extra epoch of delay for protocol-version/pparam visibility vs. committee/constitution/treasury — all
land in queryable state at the identical epoch boundary.**

**Answer to "must HardForkInitiation be the only action enacted at that boundary?"**: not
HF-specific — it's a general rule. `delayingAction` (`Rules/Ratify.hs:283-290`) = True for
`NoConfidence, HardForkInitiation, UpdateCommittee, NewConstitution`; False for `ParameterChange,
TreasuryWithdrawals, InfoAction`. In `ratifyTransition` (`Rules/Ratify.hs:334-360`), actions are
processed in `reorderActions`-sorted order (priority `NoConfidence=0, UpdateCommittee=1,
NewConstitution=2, HardForkInitiation=3, ParameterChange=4, TreasuryWithdrawals=5, InfoAction=6`,
`Governance/Internal.hs:534-544`, stable sort, ties keep insertion order). Every action's acceptance
test includes `&& not rsDelayed`, and a successful ENACT sets `rsDelayedL .~ delayingAction govAction`
(plain `.~`, not sticky-OR, but since `not rsDelayed` gates entry to the `then` branch that performs
that set, once True it can never be reset false within the same pass). **Consequence: at most ONE
delaying-category action (any of the 4) can enact per boundary, and because delaying types occupy
priority slots 0-3 (before all non-delaying types 4-6), if one enacts it blocks EVERY subsequent
action in that pass regardless of type — including unrelated ParameterChange/TreasuryWithdrawals
proposals that would otherwise have passed their own vote thresholds.** If no delaying action enacts
that pass, multiple non-delaying actions (one ParameterChange + several TreasuryWithdrawals, etc.) CAN
all enact together.

## 3. TreasuryWithdrawals — two separate treasury bookkeeping mechanisms, capacity gated at RATIFY

`ensTreasury` (EnactState's own copy) is debited by the FULL requested amount at ENACT time — this is
**bookkeeping only**, used purely to gate subsequent withdrawal proposals in the SAME pass (§ below).
The REAL pot (`ChainAccountState.casTreasuryL`) and reward-account credit happen later, in
`applyEnactedWithdrawals` (`Rules/Epoch.hs:209-242`), called from `epochTransition` at the SAME
boundary:
```haskell
successfulWithdrawls = Map.mapMaybeWithKey
  (\cred w -> compactCoinOrError w <$ guard (isAccountRegistered cred accounts))
  enactedWithdrawals
chainAccountState' = chainAccountState & casTreasuryL %~ (<-> fromCompact (fold successfulWithdrawls))
dState' = dState & accountsL %~ addToBalanceAccounts successfulWithdrawls
```
A withdrawal whose target credential is **no longer registered** at this epoch-boundary moment (it may
have been registered when the proposal was submitted/enacted, then deregistered before the boundary
finished applying it) is silently excluded from `successfulWithdrawls` — the real treasury pot is
simply never debited for it (it was never actually removed), and the money is not separately routed to
`unclaimed`/`donations` either; it just stays in the treasury having never left. No event/failure is
raised for this.

**Capacity check — answers "what if requested amount exceeds treasury at enactment":**
`withdrawalCanWithdraw (TreasuryWithdrawals m _) treasury = Map.foldr' (<+>) zero m <= treasury;
withdrawalCanWithdraw _ _ = True` (`Rules/Ratify.hs:292-295`), ANDed into the per-action acceptance
test in `ratifyTransition` alongside `acceptedByEveryone`. **It is neither a proposal-submission-time
rejection nor a ratification hard-failure — it's a silent per-pass soft-fail**: if false, the action
just isn't enacted this pass (falls to the same else-branch as "didn't meet vote threshold"), stays
live in `cgsProposalsL`, and is re-evaluated fresh at every subsequent pulser (fresh `ensTreasury`
seeded from the real pot at that boundary, fresh vote tally from whatever accumulated by then) until
either it succeeds or its `gasExpiresAfter` passes. Because `ensTreasury` is threaded through the fold
and decremented for each earlier-processed accepted `TreasuryWithdrawals` in the SAME pass, **priority
order determines who gets capacity when multiple withdrawal actions compete within one pass** —
earlier-processed ones (by `actionPriority`/insertion-order tiebreak) claim capacity first; a later one
that would have individually fit can still fail this pass if an earlier one already consumed the
headroom. `TreasuryWithdrawals (Map AccountAddress Coin) (StrictMaybe ScriptHash)` — one proposal can
target multiple reward accounts at once; amounts to the same credential from multiple different
enacted-in-the-same-pass `TreasuryWithdrawals` actions are summed (`Map.unionWith (<>)` in ENACT) into
one lump payout per credential.

## 4. Deposit refund timing

`returnProposalDeposits` (`Rules/Epoch.hs:179-193`) iterates ONLY
`allRemovedGovActions = expiredActions ∪ enactedActions ∪ removedDueToEnactment` (siblings pruned by
enactment) — called at the SAME epoch boundary as steps 2-9 above, i.e. the identical boundary the
action is (enacted|expired|orphaned-by-a-sibling's-enactment). Pays into
`AccountState.balanceAccountStateL` via `updateLookupAccountState`; if the return credential is no
longer registered, the deposit goes to an `unclaimed :: Map GovActionId Coin` map instead, which is
folded straight into `casTreasuryL` (alongside `utxosDonationL`) later in the same transition
(`Rules/Epoch.hs:347-350`). See also [[feedback_proposal_deposit_epoch_boundary]] (expiry is strict
`gasExpiresAfter < reCurrentEpoch`) and [[conway-ratify-precision-facts]] fact #8.

## 5. InfoAction can never be ratified — confirmed at the threshold level, not just by convention

`votingCommitteeThresholdInternal`, `votingStakePoolThresholdInternal`, `votingDRepThresholdInternal`
(`Governance/Internal.hs:406,459,532`) all have `InfoAction {} -> NoVotingThreshold`.
`toRatifyVotingThreshold NoVotingThreshold = SNothing` ("no voting threshold **prevents** ratification",
verbatim comment, `Internal.hs:338-341`), and `committeeAccepted`/`spoAccepted`/`dRepAccepted`
(`Rules/Ratify.hs:118-236`) all short-circuit `SNothing -> False`. So `acceptedByEveryone` is `False &&
False && False` for InfoAction unconditionally, regardless of any vote tally — it is structurally
unratifiable, not merely conventionally never enacted. Its only terminal state is expiry
(`gasExpiresAfter < reCurrentEpoch`), at which point it's swept by the same `returnProposalDeposits`
path as any other expired action — **it does still accrue and refund its deposit normally**, it just
never appears in `rsEnacted`.

(Third `VotingThreshold` constructor for completeness: `NoVotingAllowed -> SJust minBound`, a 0%
auto-pass, distinct from `NoVotingThreshold`'s auto-fail — not used for InfoAction but exists for other
bootstrap-phase-gated cases.)

## 6. Proposal-purpose lineage / prev-action-id chaining (`Governance/Procedures.hs:618-869`)

4 purposes: `PParamUpdatePurpose, HardForkPurpose, CommitteePurpose, ConstitutionPurpose`
(`GovActionPurpose`, `GovRelation (f :: Type -> Type)` has exactly these 4 fields,
`grPParamUpdate/grHardFork/grCommittee/grConstitution`). Constructor → purpose:
- `ParameterChange (StrictMaybe (GovPurposeId 'PParamUpdatePurpose)) ...` — own lineage.
- `HardForkInitiation (StrictMaybe (GovPurposeId 'HardForkPurpose)) ...` — own lineage. (See
  [[hardfork-pvcanfollow-exact-mechanics]] for the extra `pvCanFollow`/`preceedingHardFork` check this
  purpose alone gets, on top of the generic lineage check below.)
- `NoConfidence (StrictMaybe (GovPurposeId 'CommitteePurpose))` and
  `UpdateCommittee (StrictMaybe (GovPurposeId 'CommitteePurpose)) ...` — **share ONE lineage**
  (`CommitteePurpose`); either type's prevGovActionId must point to the last-enacted action of EITHER
  type.
- `NewConstitution (StrictMaybe (GovPurposeId 'ConstitutionPurpose)) ...` — own lineage.
- `TreasuryWithdrawals (Map AccountAddress Coin) (StrictMaybe ScriptHash)` and `InfoAction` — **no
  prevGovActionId field at all**, not chainable, `withGovActionParent` returns `noParent` for both
  (`Procedures.hs:807,811`), `isGovActionWithPurpose` is `False` for both regardless of purpose asked.

Two independent checks use this lineage, at two different times:
- **Proposal submission** (GOV rule, block-apply time): `proposalsAddAction`/`InvalidPrevGovActionId`
  — structural check against the LIVE proposal forest (does prevGovActionId resolve to the current root
  or an existing live node of that purpose?). Competing/sibling proposals pointing to the same valid
  parent are BOTH accepted here (not exclusive) — enactment of one later prunes the other via
  `proposalsRemoveWithDescendants`, see [[feedback_proposal_deposit_epoch_boundary]] point 2.
- **Ratification** (`prevActionAsExpected`, `Rules/Ratify.hs:364-367`): checks the proposal's
  prevGovActionId against `ensPrevGovActionIds` — the ACTUALLY-enacted lineage root, which can move
  mid-pass (an earlier action of the same purpose enacted earlier in this same pass updates it, so a
  same-pass child correctly sees its just-enacted parent). An action whose parent was orphaned by
  another action of the same purpose enacting first (this pass or an earlier one) fails this check
  forever — not removed early, just silently never enacts, until it expires.

## Related
[[bounded-ratio-decode-and-enact-totality]] — ENACT totality proof, `applyPPUpdates`.
[[conway-ratify-precision-facts]] — `pvCanFollow`, `reorderActions`, `ensTreasury`-not-stale,
`RatifyEnv` frozen fields, committee minSize gate, `rsDelayed` mid-pass fold (this file supersedes that
one's §10c one-liner on `rsDelayed` with the full priority-ordering consequence).
[[drep-distr-deposit-attribution]] — `computeDRepDistr`, proposal-deposit stake attribution.
[[feedback_proposal_deposit_epoch_boundary]] — deposit refund scope, expiry off-by-one.
[[conway-gov-vote-proposal-predicate-failures]] — sibling file in this exact session, GOV-rule
predicate failures for vote/proposal submission (complements this file's ENACT/RATIFY/EPOCH focus).

## §5 pulser mechanics referenced above, not fully detailed in this file
See `Governance/DRepPulser.hs::finishDRepPulser` (`dpProposals = proposalsActions props` frozen once at
pulser creation; `RatifySignal $ reorderActions ratifySig`; initial `RatifyState` seeded with
`rsDelayed = False, rsEnacted = mempty, rsExpired = mempty`) and
[[ratify-consumes-previous-boundary-pulser]] (dugite-project memory, `~/.claude/projects/...dugite/memory/`)
for the "proposal submitted in epoch E is invisible to the E→E+1 pulser, first visible to the
(E+1)→(E+2) one" cross-epoch mechanic — that file's claim is consistent with and independently
corroborated by the `Rules/Epoch.hs:302-313` comment quoted in this file's §2 step 4.
