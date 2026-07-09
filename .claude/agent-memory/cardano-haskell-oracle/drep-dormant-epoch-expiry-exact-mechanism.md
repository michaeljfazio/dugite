---
name: drep-dormant-epoch-expiry-exact-mechanism
description: Exact Conway DRep numDormantEpochs mechanism (bump-at-submission, not add-at-check-time) with verbatim source + permalinks, resolves prior vague memory
type: reference
---

## Resolved contradiction

Mechanism is **bump-at-submission**, NOT add-at-check-time. `dRepAcceptedRatio` in
Ratify.hs does a bare `reCurrentEpoch > drepExpiry drepState` comparison with
**zero** dormant-epoch arithmetic. All dormant-epoch compensation is baked into
`drepExpiry` itself, at three mutation sites plus one reset-and-bump site.

Pinned commit used for permalinks: `8595dbef040a9cc7dcd0da0ce4bf5274086ab3bf`
(cardano-ledger master, fetched 2026-07-09). Re-verify SHA if citing far in the future.

## The four operations on drepExpiry / vsNumDormantEpochs

1. **Registration** (`ConwayRegDRep`, GOVCERT rule) — `GovCert.hs:210-233`, uses
   `computeDRepExpiryVersioned` (bootstrap-phase gate on `pvMajor==9`, else same as raw).

2. **Anchor/metadata update** (`ConwayUpdateDRep`, GOVCERT rule) — `GovCert.hs:255-272`,
   uses raw `computeDRepExpiry` (no bootstrap gate).

3. **Voting** (`updateVotingDRepExpiries`, CERTS rule, not GOVCERT) — `Certs.hs:272-292`,
   uses raw `computeDRepExpiry`, keyed on any `DRepVoter` cred found in the tx's
   `VotingProcedures`.

   All three of the above call:
   ```haskell
   computeDRepExpiry :: EpochInterval -> EpochNo -> EpochNo -> EpochNo
   computeDRepExpiry ppDRepActivity currentEpoch =
     binOpEpochNo (-) (addEpochInterval currentEpoch ppDRepActivity)
   -- i.e. (currentEpoch + drepActivity) - numDormantEpochs
   ```
   `GovCert.hs:278-306`. `currentEpoch` = live epoch of the tx (via `epochFromSlot`,
   `Conway/Rules/Ledger.hs:358`), `numDormantEpochs` = `vsNumDormantEpochsL` value
   **at that moment** (not yet reset). This pre-subtracts dormant epochs accrued
   BEFORE this DRep's registration/vote/update event — so a DRep only ever gets
   credit for dormant epochs occurring AFTER its most recent expiry-touching event.

4. **Reset + bump-all** (`updateDormantDRepExpiry`, CERTS rule, `Empty`-cert branch,
   fires once per tx that carries `proposalProceduresTxBodyL`) — `Certs.hs:306-328`:
   ```haskell
   updateDormantDRepExpiry currentEpoch vState =
     if numDormantEpochs == EpochNo 0
       then vState
       else vState
              & vsNumDormantEpochsL .~ EpochNo 0
              & vsDRepsL %~ Map.map updateExpiry
     where
       numDormantEpochs = vState ^. vsNumDormantEpochsL
       updateExpiry = drepExpiryL %~ \currentExpiry ->
         let actualExpiry = binOpEpochNo (+) numDormantEpochs currentExpiry
          in if actualExpiry < currentEpoch then currentExpiry else actualExpiry
   ```
   Unconditionally maps **every** entry in `vsDReps` (+numDormantEpochs), guarded so
   already-far-expired DReps aren't "revived" (only applies the bump if the bumped
   value would still be < currentEpoch... actually guard is inverted: only skips
   bump if bumping would NOT clear currentEpoch, i.e. leaves clearly-dead DReps
   alone). Gated by `updateDormantDRepExpiries` (`Certs.hs:257-267`) checking
   `hasProposals = not . OSet.null $ tx ^. bodyTxL . proposalProceduresTxBodyL`.
   Doc comment explicitly says it's safe to run before GOV validates the proposal
   because the whole tx fails atomically if GOV later rejects it.

## Counter increment: `updateNumDormantEpochs`

`Epoch.hs:195-201`, called from `epochTransition` (`Epoch.hs:337-344`) with `eNo` =
the NEW epoch being entered and `newProposals` = the post-enactment/expiry proposal
set for that boundary:
```haskell
updateNumDormantEpochs currentEpoch ps vState =
  if null $ OMap.filter ((currentEpoch <=) . gasExpiresAfter) $ ps ^. pPropsL
    then vState & vsNumDormantEpochsL %~ succ
    else vState
```
Increments iff NO proposal survives un-expired (`gasExpiresAfter >= currentEpoch`)
into the new epoch — not "no proposal existed at any point last epoch."

## RATIFY-time check (the actual gate used for ratification)

`Ratify.hs:252-279`, inside `dRepAcceptedRatio`:
```haskell
| reCurrentEpoch > drepExpiry drepState -> (yes, tot) -- drep is expired, excluded
```
No `+numDormantEpochs` term. `reCurrentEpoch` = `dpCurrentEpoch` set in
`setFreshDRepPulsingState epochNo ...` called from the TAIL of `epochTransition(eNo)`
(`Epoch.hs:372`, `liftSTS $ setFreshDRepPulsingState eNo stakePoolDistr epochState2`).
Net effect: a pulser created at boundary (eNo-1 -> eNo) has `dpCurrentEpoch = eNo`,
runs/pulses through epoch `eNo`, and is only *finished* (forcing `finishDRepPulser`
-> `RatifyEnv`) at the START of the NEXT `epochTransition(eNo+1)` call. So
**`reCurrentEpoch` in a given ratification == the epoch number that has just
concluded**, i.e. one less than the `eNo` of the `epochTransition` call in which
extraction textually happens.

## Bonus: read-only display helper confirms the model

```haskell
vsActualDRepExpiry cred vs =
  binOpEpochNo (+) (vsNumDormantEpochs vs) . drepExpiry <$> Map.lookup cred (vsDReps vs)
```
`State/VState.hs:154-156` — for external queries, ADDS the (usually-zero, since it's
normally reset promptly) live counter back on top, since `drepExpiry` itself may be
temporarily "stale" relative to true wall-clock dormancy between resets.

## Dugite implication

If Dugite adds `numDormantEpochs` inside the ratification-time expiry check
("hypothesis B" — add-at-check-time), that is the bug. Correct fix: move ALL
dormant-epoch arithmetic to the four mutation sites above (register/update/vote
subtract at write-time; CERTS bump-all-and-reset on proposal submission), and make
the RATIFY-time comparison a pure `reCurrentEpoch > drepExpiry` with no dormant term.

See also [[drep-pulser-ratification]] for the broader pulser lifecycle this plugs
into, and [[conway-ratification-details]] (had the vague/correct-in-gist-only
version of this before this file was written).
