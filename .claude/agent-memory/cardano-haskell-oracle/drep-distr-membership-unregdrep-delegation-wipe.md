---
name: drep-distr-membership-unregdrep-delegation-wipe
description: psDRepDistr membership rules at conway 1.20.0.0 (rev faa7a9dc): UnRegDRep WIPES delegations via drepDelegs; #4772 preserve-bug at PV9; PV10 HARDFORK updateDRepDelegations; no expiry filter in distr
metadata:
  type: reference
---

Pinned rev: `faa7a9dc347697b11d4da5b7818b1731e11aeeef` (cardano-ledger-conway 1.20.0.0,
what cardano-node 11.0.1 / cstreamer 10.6.2-era ships). Verified verbatim 2026-08-11
while analysing the preprod drepDistr divergence (dugite vs cstreamer, epochs 168-184).

## psDRepDistr membership (DRepPulser.hs:206-248)

`computeDRepDistr` folds ACCOUNTS (delegator-driven). An entry exists iff at least one
account has `dRepDelegationAccountStateL = Just dRep`, and for `DRepCredential cred`
additionally `Map.member cred regDReps` (= `dpDRepState` = vsDReps snapshot).
AlwaysAbstain/NoConfidence unconditional. Stake per account =
`instantStake(cred) <> proposalDeposits(cred) <> balance`. **NO expiry filter anywhere
in the pulser.** Expiry is checked ONLY in `dRepAcceptedRatio` (Ratify.hs:251-280):
unregistered -> skipped entirely; `reCurrentEpoch > drepExpiry` -> skipped;
no-vote -> denominator only. `reCurrentEpoch = dpCurrentEpoch` = the eNo the pulser
was seeded with = the epoch that concludes when RATIFY runs.

## ConwayUnRegDRep WIPES delegations (GovCert.hs:229-249)

Deletes cred from vsDReps AND
`clearDRepDelegations (drepDelegs dRepState)` = for every staking cred in the DRep's
`drepDelegs` reverse index, `Map.adjust (dRepDelegationAccountStateL .~ Nothing)` on
the accounts map. Registration (`ConwayRegDRep`, GovCert.hs:205-228) starts
`drepDelegs = mempty` — a dereg/re-reg cycle PERMANENTLY orphans prior delegators;
they must re-delegate. This is the mechanism dugite was missing (preprod divergence:
two DReps did dereg+re-reg WITHIN one epoch — 37s apart and 17min apart — and dugite
kept counting the wiped delegators forever).

## #4772 preserve-bug at PV9 (Deleg.hs:264,270,295-328)

`processDelegationInternal preserveIncorrectDelegation` with the flag =
`pvMajor pv < natVersion @10`. At PV9, re-delegating X->Y (Y REGISTERED) builds the
final vsDReps from the PRE-removal map, so the delegator STAYS in X's drepDelegs
(stale). Quirk: if Y is unregistered/AlwaysAbstain/NoConfidence the removal survives
even at PV9 (`_ -> cState'` branch). Consequence: X's later dereg collaterally wipes
delegations that had MOVED to Y. Consensus-visible; must be replicated bug-for-bug.
Also PV9 (`hardforkConwayBootstrapPhase pv = pvMajor == 9`, Era.hs:171-172) skips the
`DelegateeDRepNotRegisteredDELEG` check (Deleg.hs:210-216) — delegating to an
unregistered DRep is LEGAL at PV9, rejected from PV10.

## PV10/PV11 intra-era HARDFORK rule (HardFork.hs:68-124, Epoch.hs:371-379)

`epochTransition`: SNAP -> POOLREAP -> extract pulser -> enact -> ... ->
`if curPv /= prevPv then trans @HARDFORK` -> `setFreshDRepPulsingState eNo` (so the
new pulser sees POST-hardfork-cleanup state). `hardforkTransition`:
- pvMajor==10: `updateDRepDelegations` — resets every registered DRep's drepDelegs to
  empty, walks ALL accounts, DELETES forward delegations to unregistered DReps,
  re-indexes delegations to registered ones. One-shot reconciliation of the #4772 mess.
- pvMajor==11: `populateVRFKeyHashes` (psVRFKeyHashes from stake+future pools).

## Expiry computation (Q3 sites, all verified at pin)

- Register: `computeDRepExpiryVersioned` (GovCert.hs:272-286) — PV9: `cur + drepActivity`
  (dormant NOT subtracted); PV10+: `cur + drepActivity - numDormantEpochs`.
- Update cert: raw `computeDRepExpiry` (subtracts) at all PVs (GovCert.hs:260-263).
- Vote: CERTS Empty branch (Certs.hs:239-250) adjusts each DRepVoter's expiry.
- Proposal submission: `updateDormantDRepExpiry` (Certs.hs:283-300) bump-all + reset ctr.
- Counter: `updateNumDormantEpochs` (Epoch.hs:204-210) succ iff no un-expired proposal.

## Consensus impact

psDRepDistr IS `reDRepDistr` (finishDRepPulser, DRepPulser.hs:387-424 builds RatifyEnv
from `finalDRepDistr`). An extra entry for an UNREGISTERED cred is inert in the ratio
(reDRepState lookup skips it), but a stale-delegation entry for a REGISTERED unexpired
DRep lands in the DENOMINATOR as implicit-NO -> false-reject direction, can flip
ratification. Preprod's ParameterChange (ratified by epoch-179 pulser) and HFI-10
(epoch-180 pulser) were both decided inside such a window.

## Oracle corroboration method

Koios preprod REST (curl, not the MCP which is preview-only):
`drep_updates` gives cert history, `drep_voting_power_history` gives db-sync's
per-epoch power — agreed with cstreamer at EVERY epoch incl. the weird 7,652,894.
Epoch mapping: preprod epoch N starts at unix `1654041600 + N*432000`.
CIP-129 bech32: header 0x22 keyhash / 0x23 scripthash, hrp `drep`.

See also [[drep-pulser-ratification]] (lifecycle) and
[[drep-dormant-epoch-expiry-exact-mechanism]] (dormant arithmetic).
