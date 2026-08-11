---
name: pv11-gates-exhaustive-at-faa7a9dc
description: EXHAUSTIVE PV11 behaviour-gate catalog at cardano-ledger faa7a9dc (conway 1.20.0.0, cn 11.0.1 pin) — 8 hardfork predicates, all natVersion sites, 9 PV11 deltas, zero CBOR gates at v10/v11
metadata:
  type: reference
---

Pin `faa7a9dc347697b11d4da5b7818b1731e11aeeef` (2025-09-09, conway 1.20.0.0 = cn 11.0.1
CHaP pin). Cloned + swept 2026-08-12: grep `natVersion` / `pvMajor` / `hardfork` /
`mkVersion` / `eraProtVer{Low,High}` over all `eras/*/impl/src` + `libs/*/src`.
Era bounds (`libs/cardano-ledger-core/internal/.../Definition/Era.hs:123-140`):
Babbage 7-8, **Conway 9-11**, Dijkstra 12-12.

## All 8 hardfork* predicates (+2 SoftForks)
Shelley/Era.hs: AllegraAggregatedRewards >2 (:127), AlonzoAllowMIRTransfer >4 (:139),
AlonzoValidatePoolRewardAccountNetID >4 (:146), BabbageForgoRewardPrefilter >6 (:152),
**ConwayDisallowDuplicatedVRFKeys >10 (:157)**.
Conway/Era.hs: BootstrapPhase ==9 (:172), **DisallowUnelectedCommitteeFromVoting >10
(:177)**, **DELEGIncorrectDepositsAndRefunds >10 (:181)**.
SoftForks.hs: validMetadata >2.0, restrictPoolMetadataHash >4.0.

## The 9 PV11 deltas (ONLY these switch at 11)
1. **POOL: duplicate VRF keys rejected** — Shelley Rules/Pool.hs:253-300, RegPool fresh
   (:255-256) + re-register (:267-271, exempts same-VRF). New failure
   `VRFKeyHashAlreadyRegistered` POOL **tag 6** arity 3. CONSENSUS, new-reject.
2. **New state `psVRFKeyHashes`** — PState field [0] of array(4)
   (core State/CertState.hs:225,246-257; encoder unconditional at ALL PVs, empty pre-11).
   Maintained: POOL insert (gated), POOLREAP remove unconditional (PoolReap.hs:140-168,
   214-216: dangling-on-reregister withoutKeys + retired decrement).
3. **HARDFORK ==11 arm `populateVRFKeyHashes`** — Conway Rules/HardFork.hs:77-78,107-124;
   counts stake+future pools, `insertWith` saturating +1. Trigger Epoch.hs:375-377
   (`curPv /= prevPv`, FULL ProtVer). NOT idempotent (re-run double-counts).
   ==10 arm = updateDRepDelegations.
4. **GOV: `UnelectedCommitteeVoters`** — Gov.hs:466-469, def :637-652 (uses
   authorizedELECTEDHotCommitteeCredentials; contrast ungated VotersDoNotExist tag 14
   :589 which uses ALL authorized). GOV **tag 18**, NonEmpty hot creds. CONSENSUS
   new-reject.
5. **MEMPOOL-only unelected-CC check DISABLED at 11** — Mempool.hs:122-135 (`unless`
   gate; at PV9/10 it was a mempool-only ConwayMempoolFailure text). Node-local only.
6. **DELEG failure constructors** — Deleg.hs:193,241: wrong deposit/refund emit
   `DepositIncorrectDELEG` **7** / `RefundIncorrectDELEG` **8** (Mismatch) instead of
   `IncorrectDepositDELEG` **1** (Coin). Wire-only.
7. **Script-integrity failure constructor** — Alonzo Rules/Utxow.hs:293-312 (`< @11`).
   Conway wire (Rules/Utxow.hs:271,276): PPViewHashesDontMatch **13 ToGroup(flattened)**
   vs ScriptIntegrityHashMismatch **18 nested + StrictMaybe preimage bytes**. Wire-only.
8. **inputs∩refInputs**: `disjointRefInputs` (Babbage Rules/Utxo.hs:225-239) active ONLY
   `>8 && <11` ⇒ `BabbageNonDisjointRefInputs` (BabbageUtxo tag 4) STOPS at 11 — overlap
   newly ACCEPTED unless a PlutusV3 script runs: Conway TxInfo.hs:487-489 raises
   `ReferenceInputsNotDisjointFromInputs` (ConwayContextError **tag 15**) at >=11 during
   V3 TxInfo translation (BadTranslation path). CONSENSUS both directions.
9. **BBODY ref-script size accumulates intra-block UTxO** — Conway Rules/Bbody.hs:325-341
   `totalRefScriptSizeInBlock`: <=10 measures every tx vs block-initial UTxO; >=11 folds
   outputs of earlier txs in (isValid⇒txouts, else collOuts) so same-block-created ref
   scripts now COUNT toward BodyRefScriptsSizeTooBig. CONSENSUS, new-reject, block-level.
Plus conduit: `validScript` (Alonzo Scripts.hs:641-648) passes pvMajor→plutus
`isValidPlutusScript`; drives UTXOW MalformedScriptWitnesses/MalformedReferenceScripts
(Babbage Utxow.hs:269-284) and `pwcProtocolVersion` (Plutus/Context.hs:180). PV11 delta
(batch6 builtins, Case-on-VCon) lives in the plutus repo.

## Zero CBOR change PV10→PV11
No encoder/decoder gate at version 10 or 11 ANYWHERE (binary gates only @2/@7/@9/@12).
serialize(pvMajor) sites (Allegra/Alonzo OutputTooBig, LangDepView Alonzo PParams:567,
Tools:253, TPraos BHeader signable) all byte-identical v10 vs v11. Decoder gates
(@9 set-tags, @12 Mary MultiAsset/PlutusV4 guardPlutus) key off `eraProtVerLow` = ERA,
not live PV.

## NOT at this pin (master differs!)
`hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule` +
ConwayWithdrawalsMissingAccounts/ConwayIncompleteWithdrawals are POST-pin;
here CERTS `WithdrawalsNotInRewardsCERTS` is UNGATED (Certs.hs:258-260). cn 11.0.1
mainnet PV11 keeps the combined CERTS-level check. See
[[conway-certs-rule-dispatch-and-withdrawal-split]] (that memory = NEWER commit).

## PV gates that do NOT move at 11
PV9-only bootstrap (==9, so 10==11): Gov 379/428/494/533, GovCert 283, Ratify 215 (SPO
default abstain), Governance/Internal 475/527 (thresholds), PParams 932
(coinsPerUTxOByte /=0 check waived), Ledger 402 (WdrlNotDelegatedToDRep active 10+),
Deleg 215 (DelegateeDRepNotRegistered active 10+), TxInfo 565/570 (cert deposit
Nothing at 9). PV10: preserveIncorrectDelegation (<10, Deleg 264/270), HARDFORK ==10
arm. Early: @2/@4/@6 hardforks, SoftForks.
