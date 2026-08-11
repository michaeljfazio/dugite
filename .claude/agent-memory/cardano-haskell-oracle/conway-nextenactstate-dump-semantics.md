---
name: conway-nextenactstate-dump-semantics
description: rsEnactState returned by finishDRepPulser INCLUDES this pulser's own to-be-enacted effects — a boundary dump shows an enacting action's id/PV one epoch BEFORE it lands in the gov state; EnactState ToJSON prints only 5 of 7 fields
type: reference
---

All verified at cardano-ledger `faa7a9dc347697b11d4da5b7818b1731e11aeeef` =
**cardano-ledger-conway 1.20.0.0** (CHaP `_sources/cardano-ledger-conway/1.20.0.0/meta.toml`,
published 2025-09-10), the version cardano-streamer's 10.6.2 branch pins.

## EnactState ToJSON prints 5 of 7 fields

`eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Internal.hs:190-198`
(`ToKeyValuePairs`, `deriving ToJSON via KeyValuePairs` — older releases called this
`toEnactStatePairs`): keys `committee`, `constitution`, `curPParams`, `prevPParams`,
`prevGovActionIds`. **`ensTreasury` and `ensWithdrawals` are NOT serialized.**
CBOR (`EncCBOR`, same file :231-241) has all 7 in declaration order.

## rsEnactState is SELF-INCLUSIVE (the central fact)

`Rules/Ratify.hs:342-351`: on acceptance, `newEnactState <- trans @ENACT (rsEnactState, …)`
then `st & rsEnactStateL .~ newEnactState & rsEnactedL %~ (:|> gas)` — the accumulated
EnactState IS threaded into the returned RatifyState. `Rules/Enact.hs:83-116`
(`enactmentTransition`): ParameterChange → `ensCurPParamsL %~ applyPPUpdates` +
`ensPrevPParamUpdateL`; HardForkInitiation → `ensProtVerL .~ pv` + `ensPrevHardForkL`;
NoConfidence/UpdateCommittee → `ensCommitteeL` + `ensPrevCommitteeL`; NewConstitution →
`ensConstitutionL` + `ensPrevConstitutionL`; TreasuryWithdrawals → withdrawals/treasury only;
InfoAction → id. Empty-signal case (`Ratify.hs:359`) zeroes `ensTreasury` (unprinted).
**`ensPrevPParams` is written by NO ENACT case and NO RATIFY step** — it changes only at
the EPOCH boundary (`Rules/Epoch.hs:331` `cgsPrevPParamsL .~ curPParams`).

## EPOCH ordering (Rules/Epoch.hs epochTransition, :276-379)

SNAP → POOLREAP → `extractDRepPulsingState` (= `snd . finishDRepPulser`, DRepPulser.hs:509)
→ `applyEnactedWithdrawals` → `proposalsApplyEnactment rsEnacted rsExpired` (promotes
enacted ids to `pRoots`, Proposals.hs:547) → govState1 (`cgsCommitteeL .~ ensCommittee`,
`cgsConstitutionL .~ ensConstitution`, `cgsCurPParamsL .~ nextEpochPParams govState0`
[FuturePParams mechanism, NOT ensCurPParams directly], `cgsPrevPParamsL .~ curPParams`)
→ HARDFORK rule iff PV changed → **`setFreshDRepPulsingState` LAST** (:379), whose
`dpEnactState = mkEnactState govState & ensTreasuryL .~ epochState^.treasuryL`
(Governance.hs:509-511). `mkEnactState` (Governance.hs:327-337): committee/constitution/
cur/prev PParams from cgs* lenses; prevGovActionIds from `pRootsL . to toPrevGovActionIds`;
treasury `zero`; withdrawals `mempty`.

## Dump timing consequence

A first-block-of-epoch-E dump that forces the fresh pulser (dpCurrentEpoch=E, enacts at
E→E+1) shows a ratifying action's GovActionId and its PParams/PV effect **at E — one epoch
BEFORE the gov state itself changes**. Forced result is invariant to pulse progress
(finishDRepPulser processes `Map.drop dpIndex` leftover; snapshot frozen at the boundary).
Verified against preprod dumps: PParamUpdate b52f first at 179 (enacts into 180, costModels),
HardFork ccb2 first at 180 (enacts into 181 ⇒ preprod PV10 live in epoch 181); epoch-180
nextEnactState diff {costModels, protocolVersion} = enacted-at-180 costModels + forced-in PV10.

## JSON shapes for the edge cases

- `StrictMaybe`: `toJSON = toJSON . strictMaybeToMaybe` (cardano-base
  `cardano-strict-containers/src/Data/Maybe/Strict.hs:107-109`) ⇒ SNothing = `null`,
  and aeson `.=` still emits the key ⇒ `"committee": null`, `"Committee": null`.
- `Constitution` (Procedures.hs:915-919): `"anchor"` always; **`"script"` key ABSENT**
  (list-comprehension guard) when constitutionScript = SNothing — not null.
- `CommitteeState` (core `State/CertState.hs:296-301`): GENERIC aeson instance ⇒
  `{"csCommitteeCreds": {<credToText cold cred>: <auth>}}`; keys `keyHash-<hex>`/
  `scriptHash-<hex>` (Credential.hs:161-163).
- `CommitteeAuthorization` (CertState.hs:270-281): GENERIC ⇒ TaggedObject:
  `{"tag":"CommitteeHotCredential","contents":{"keyHash":"…"}}` /
  `{"tag":"CommitteeMemberResigned","contents":null | {"url":…,"dataHash":…}}`.
  NB `Credential` ToJSON (non-key position) is an OBJECT `{"keyHash": …}` (Credential.hs:121-126).
- `GovRelation` (Procedures.hs:738-744): keys `PParamUpdate`,`HardFork`,`Committee`,
  `Constitution`; `GovActionId` (:197-202) `{"txId":…,"govActionIx":<int>}`;
  `GovPurposeId` is `deriving newtype ToJSON` (:649) ⇒ transparent.
- `Committee` (:590-595): `{"members": {<cold cred text key>: <epochNo>}, "threshold": …}`.

Version note: JSON pair functions renamed from `to*Pairs` to `ToKeyValuePairs` instances in
2025; key sets unchanged. Related: [[conway-ratification-details]], [[drep-pulser-ratification]],
[[conway-gov-state-encoding-detailed]].
