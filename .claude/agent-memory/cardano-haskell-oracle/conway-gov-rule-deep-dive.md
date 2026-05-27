---
name: conway-gov-rule-deep-dive
description: Complete Conway GOV rule: all 19 predicate failures, CBOR tags, exact ordering, same-tx votes, voter matrix, VotersDoNotExist semantics, ppuWellFormed, epoch-boundary removal, reapplyTx behavior
metadata:
  type: reference
---

# Conway GOV Rule — Complete Reference

Source commit: `ebed62de1ebcd4b13512418d49d17802a193e2c1` (IntersectMBO/cardano-ledger master, 2026-05-26)

## Key files
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` — main rule
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Internal.hs` — VotingThreshold, voting-allowed predicates
- `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` lines ~934-956 — ppuWellFormed
- `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` — hardfork flags (bootstrapPhase=PV9, disallowUnelected=PV≥11)
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs` — authorizedHotCommitteeCredentials (skips resigned)
- `eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs` lines ~581-591 — authorizedElectedHotCommitteeCredentials
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/API/Mempool.hs` lines ~256-274 — reapplyTx skips lblStatic only
- `libs/cardano-ledger-core/src/Cardano/Ledger/Rules/ValidationMode.hs` — lblStatic = "static"

## All 19 predicate failures with CBOR tags

| Tag | Constructor | Payload |
|-----|-------------|---------|
| 0 | GovActionsDoNotExist | NonEmpty GovActionId |
| 1 | MalformedProposal | GovAction era |
| 2 | ProposalProcedureNetworkIdMismatch | AccountAddress, Network |
| 3 | TreasuryWithdrawalsNetworkIdMismatch | NonEmptySet AccountAddress, Network |
| 4 | ProposalDepositIncorrect | Mismatch RelEQ Coin (group-encoded) |
| 5 | DisallowedVoters | NonEmpty (Voter, GovActionId) |
| 6 | ConflictingCommitteeUpdate | NonEmptySet (Credential ColdCommitteeRole) |
| 7 | ExpirationEpochTooSmall | NonEmptyMap (Credential ColdCommitteeRole) EpochNo |
| 8 | InvalidPrevGovActionId | ProposalProcedure era |
| 9 | VotingOnExpiredGovAction | NonEmpty (Voter, GovActionId) |
| 10 | ProposalCantFollow | StrictMaybe GovPurposeId, Mismatch RelGT ProtVer |
| 11 | InvalidGuardrailsScriptHash | StrictMaybe ScriptHash (got), StrictMaybe ScriptHash (expected) |
| 12 | DisallowedProposalDuringBootstrap | ProposalProcedure era |
| 13 | DisallowedVotesDuringBootstrap | NonEmpty (Voter, GovActionId) |
| 14 | VotersDoNotExist | NonEmpty Voter |
| 15 | ZeroTreasuryWithdrawals | GovAction era |
| 16 | ProposalReturnAccountDoesNotExist | AccountAddress |
| 17 | TreasuryWithdrawalReturnAccountsDoNotExist | NonEmpty AccountAddress |
| 18 | UnelectedCommitteeVoters | NonEmpty (Credential HotCommitteeRole) |

## Exact predicate ordering in conwayGovTransition

**Before proposal loop:**
1. UnelectedCommitteeVoters (PV≥11 only) — fires on ALL votes before any proposals processed

**Per-proposal loop (foldlM'):**
2. DisallowedProposalDuringBootstrap
3. ProposalCantFollow (HardFork only)
4. MalformedProposal (ParameterChange only)
5. ProposalReturnAccountDoesNotExist (post-bootstrap only)
6. TreasuryWithdrawalReturnAccountsDoNotExist (post-bootstrap, TreasuryWithdrawals only)
7. ProposalDepositIncorrect
8. ProposalProcedureNetworkIdMismatch
9. TreasuryWithdrawalsNetworkIdMismatch / InvalidGuardrailsScriptHash / ZeroTreasuryWithdrawals / ConflictingCommitteeUpdate / ExpirationEpochTooSmall (per action type)
10. InvalidPrevGovActionId (ancestry/tree insertion)

**After proposal loop (vote processing):**
11. VotersDoNotExist (tag 14)
12. GovActionsDoNotExist (tag 0)
13. DisallowedVotesDuringBootstrap (tag 13) — bootstrap only
14. VotingOnExpiredGovAction (tag 9) — curEpoch > gasExpiresAfter (STRICT GT)
15. DisallowedVoters (tag 5)

**CRITICAL for dugite**: VotingOnExpiredGovAction fires BEFORE DisallowedVoters. Dugite previously had these swapped.

## Same-tx votes

YES — votes in the same tx as proposals CAN vote on those proposals.
Reason: foldlM' processProposal runs first, inserting proposals into proposals OMap.
Then curGovActionIds = proposalsActionsMap proposals (includes just-added proposals).
GovActionId for proposal idx n = GovActionId geTxId (GovActionIx n) where geTxId is the enclosing tx's id.
No explicit exemption needed — same-tx proposals just exist naturally in curGovActionIds.

## Voter authorization matrix

| Action | CC | DRep | SPO |
|--------|----|------|-----|
| NoConfidence | NoVotingAllowed | Yes | Yes (pvtMotionNoConfidence) |
| UpdateCommittee | NoVotingAllowed | Yes | Yes (pvtCommitteeNormal/NoConfidence) |
| NewConstitution | Yes | Yes (dvtUpdateToConstitution) | NoVotingAllowed |
| HardForkInitiation | Yes | Yes (dvtHardForkInitiation) | Yes (pvtHardForkInitiation) |
| ParameterChange | Yes | Yes | Only if any param in SecurityGroup |
| TreasuryWithdrawals | Yes | Yes (dvtTreasuryWithdrawal) | NoVotingAllowed |
| InfoAction | NoVotingThreshold (allowed) | NoVotingThreshold | NoVotingThreshold |

ParameterChange + SPO: paramChangeThreshold in Internal.hs:380-393. Checks modifiedPPGroups ppu for any SecurityGroup entry. If none → NoVotingAllowed → DisallowedVoters.

## VotersDoNotExist semantics

CC: authorizedHotCommitteeCredentials — EXCLUDES resigned members (CommitteeMemberResigned is skipped).
DRep: vsDReps map — EXCLUDES AlwaysAbstain/AlwaysNoConfidence (those are DRep values, not Credential DRepRole; can't be DRepVoter).
SPO: psStakePools — standard map lookup.

UnelectedCommitteeVoters (PV≥11): uses authorizedElectedHotCommitteeCredentials = intersection of csCommitteeCreds with elected committeeMembers. CC voter can pass VotersDoNotExist but fail UnelectedCommitteeVoters.

## VotingOnExpiredGovAction

curEpoch <= gasExpiresAfter → allowed
curEpoch > gasExpiresAfter → VotingOnExpiredGovAction

gasExpiresAfter = addEpochInterval proposedInEpoch ppGovActionLifetime.
Voting is allowed IN the expiry epoch, forbidden the epoch AFTER.

## ppuWellFormed (MalformedProposal)

Only applies to ParameterChange. Fails if any SJust field has:
- MaxBBSize, MaxTxSize, MaxBHSize, MaxValSize, CollateralPercentage == 0
- CommitteeMaxTermLength, GovActionLifetime == EpochInterval 0
- PoolDeposit, GovActionDeposit, DRepDeposit == CompactCoin 0
- CoinsPerUTxOByte == 0 (only post-bootstrap)
- PParamsUpdate == emptyPParamsUpdate (no-op)
- At PV≥11: NOpt == 0

## Epoch boundary: when proposals are removed

In EPOCH rule (Epoch.hs:314-315):
  proposalsApplyEnactment rsEnacted rsExpired (govState0 ^. proposalsGovStateL)
removes enacted + expired proposals BEFORE any new-epoch blocks are applied.
Order: RUPD → SNAP → POOLREAP → proposalsApplyEnactment → govState1 assembly → HARDFORK

## reapplyTx and GOV checks

reapplyTx calls applyTxValidation (ValidateSuchThat (notElem lblStatic)).
lblStatic = "static" — marks crypto-only checks (signatures, VRF, KES).
GOV rule checks (VotersDoNotExist, GovActionsDoNotExist, DisallowedVoters, VotingOnExpiredGovAction) use runTest/failOnNonEmpty (NOT runTestOnSignal/(?!#)).
Therefore ALL GOV checks run during reapplyTx. Votes on removed proposals → GovActionsDoNotExist → tx expelled from mempool.

## Bootstrap (pvMajor == 9) differences

Proposals: only ParameterChange/HardForkInitiation/InfoAction allowed.
ProposalReturnAccountDoesNotExist: NOT checked.
TreasuryWithdrawalReturnAccountsDoNotExist: NOT checked.
Votes: DRep only on InfoAction; CC/SPO follow isBootstrapAction.
UnelectedCommitteeVoters: NOT checked (PV<11).
ParameterChange of any param group is admitted during bootstrap (group check is ratification-time only).

## Dugite divergences found

A. GovActionsDoNotExist payload must be NonEmpty — aggregate all missing IDs, emit once.
B. VotingOnExpiredGovAction BEFORE DisallowedVoters (Haskell order). Dugite had reversed.
C. UnelectedCommitteeVoters check missing entirely (needed at PV≥11).
D. VotersDoNotExist for resigned CC members — must skip resigned entries.
E. DisallowedVoters for ParameterChange+SPO must check SecurityGroup of modified params.
F. ZeroTreasuryWithdrawals: empty map also fails (sum=0).
G. ProposalReturnAccountDoesNotExist must be skipped at PV9.
H. Mempool revalidation GovActionsDoNotExist: should flow through full LEDGER/GOV rule, not special-cased.

## Test file
eras/conway/impl/testlib/Test/Cardano/Ledger/Conway/Imp/GovSpec.hs
- votingSpec (line ~741): VotersDoNotExist, GovActionsDoNotExist, DisallowedVoters, expired (disabled), UnelectedCommitteeVoters
- predicateFailuresSpec (line ~85): MalformedProposal, ProposalDepositIncorrect, ConflictingCommitteeUpdate, ExpirationEpochTooSmall, ProposalReturnAccountDoesNotExist, ZeroTreasuryWithdrawals
Note: "expired gov-actions" test is disableInConformanceIt citing formal-ledger-spec issue #923.
Haskell behavior (curEpoch > gasExpiresAfter = error) is what the node enforces.
