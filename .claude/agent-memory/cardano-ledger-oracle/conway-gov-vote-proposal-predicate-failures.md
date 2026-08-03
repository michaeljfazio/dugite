---
name: conway-gov-vote-proposal-predicate-failures
description: Exact ConwayGovPredFailure constructors (GOV rule) for every vote/proposal submission-time rejection — expired/nonexistent GovActionId, unregistered/deregistered voter, voter-role x action-type mismatch matrix, deposit exact-equality, prev_gov_action_id lineage/forest semantics, treasury-withdrawal affordability timing. Live-verified 2026-08-02 against IntersectMBO/cardano-ledger HEAD, confirmed byte-identical at dugite's pinned conformance sha a88b60bdcf3248dfe5a2f9372c188c399233f479.
metadata:
  type: project
---

Built for writing negative devnet-validate governance test cases (dugite
tx-zoo style: submit an invalid gov tx, assert dugite and cardano-node
11.0.1 reject with the SAME failure class). Source: live-fetched
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
`.../Governance/Internal.hs`, `.../Governance/Proposals.hs`,
`.../Rules/Enact.hs`, `.../Rules/Ratify.hs`, `.../Rules/Ledger.hs` via
`gh api repos/IntersectMBO/cardano-ledger/contents/...`. See
[[kb-table-files-missing-use-live-github]] for the fetch method and the
version-pin caveat.

## The full `ConwayGovPredFailure` ADT (19 constructors, tags 0-18)

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`. All fire from the
`GOV` STS rule, invoked from `LEDGER` (Conway) as
`trans @(EraRule "GOV" era)`, wrapped as `ConwayGovFailure
(PredicateFailure (EraRule "GOV" era))` inside `ConwayLedgerPredFailure`
(`data ConwayLedgerPredFailure era = ConwayUtxowFailure ... |
ConwayCertsFailure ... | ConwayGovFailure ...`, `Rules/Ledger.hs:117-120`).
GOV is ALWAYS a whole-tx, Phase-1-class, submission-time rejection — it runs
in the LEDGER rule's CERTS -> GOV -> UTXOW pipeline (see below), entirely
before any Phase-2/Plutus concern. A GOV predicate failure means the tx never
lands on chain at all (not even as `is_valid=false` with collateral
forfeit) — the client's `cardano-cli transaction submit` gets a structured,
decodable `SubmitFail`/`ApplyTxError [ConwayGovFailure (...)]`, not a
codec-layer connection drop (that class is reserved for CBOR that fails to
*decode*, e.g. dugite #925's duplicate-Set case — irrelevant here since all
of these are well-formed CBOR that decodes fine and fails semantic
validation instead).

```haskell
data ConwayGovPredFailure era
  = GovActionsDoNotExist (NonEmpty GovActionId)                                    -- 0
  | MalformedProposal (GovAction era)                                             -- 1
  | ProposalProcedureNetworkIdMismatch AccountAddress Network                     -- 2
  | TreasuryWithdrawalsNetworkIdMismatch (NonEmptySet AccountAddress) Network      -- 3
  | ProposalDepositIncorrect (Mismatch RelEQ Coin)                                -- 4
  | DisallowedVoters (NonEmpty (Voter, GovActionId))                              -- 5
  | ConflictingCommitteeUpdate (NonEmptySet (Credential ColdCommitteeRole))        -- 6
  | ExpirationEpochTooSmall (NonEmptyMap (Credential ColdCommitteeRole) EpochNo)   -- 7
  | InvalidPrevGovActionId (ProposalProcedure era)                                -- 8
  | VotingOnExpiredGovAction (NonEmpty (Voter, GovActionId))                      -- 9
  | ProposalCantFollow (StrictMaybe (GovPurposeId 'HardForkPurpose)) (Mismatch RelGT ProtVer) -- 10
  | InvalidGuardrailsScriptHash (StrictMaybe ScriptHash) (StrictMaybe ScriptHash)  -- 11
  | DisallowedProposalDuringBootstrap (ProposalProcedure era)                     -- 12
  | DisallowedVotesDuringBootstrap (NonEmpty (Voter, GovActionId))                -- 13
  | VotersDoNotExist (NonEmpty Voter)                                             -- 14
  | ZeroTreasuryWithdrawals (GovAction era)                                       -- 15
  | ProposalReturnAccountDoesNotExist AccountAddress                              -- 16
  | TreasuryWithdrawalReturnAccountsDoNotExist (NonEmpty AccountAddress)          -- 17
  | UnelectedCommitteeVoters (NonEmpty (Credential HotCommitteeRole))             -- 18
```

`AccountAddress` here = what dugite/older-ledger code calls `RewardAccount`
(network tag + stake credential) — same wire concept, renamed upstream.
`InvalidPolicyHash` is a deprecated pattern synonym for
`InvalidGuardrailsScriptHash`, keep using the latter.

## LEDGER sub-rule order (Conway): CERTS -> GOV -> UTXOW

`Rules/Ledger.hs:394-439`. `certStateAfterCERTS` (this tx's OWN certificates
already applied) is what's threaded into `GovEnv`'s `geCertState` for GOV.
UTXOW instead gets the PRE-CERTS certState (deliberately, so it can compute
deposit refunds against what was registered before this tx). This ordering
is why a DRep that registers-then-unregisters-then-votes ALL IN THE SAME TX
already fails `VotersDoNotExist` — GOV never sees a same-tx window where an
unregistered-this-tx credential still counts as a valid voter.

## Scenario -> failure mapping

1. **Vote on an expired GovActionId.** Two distinct sub-cases, same
   underlying epoch-boundary mechanic:
   - Action still present in the `Proposals` map but
     `currentEpoch > gasExpiresAfter` (expiry epoch elapsed, next epoch
     boundary hasn't swept it yet) -> `VotingOnExpiredGovAction
     (NonEmpty (Voter, GovActionId))`, from
     `checkVotesAreNotForExpiredActions` (`curEpoch <= gasExpiresAfter`
     required).
   - Action already removed from the map (either it expired AND an epoch
     boundary already ran `proposalsApplyEnactment`, or — see scenario 11 —
     a competing sibling for the same governance-purpose lane got enacted
     and pruned it) -> `GovActionsDoNotExist (NonEmpty GovActionId)`, same
     as scenario 2. Not distinguishable from "never proposed" once pruned.
   Both are Phase-1 GOV rejections at submission time. Test-design note:
   pick which sub-case you want by controlling whether an epoch boundary
   has elapsed since expiry.

2. **Vote on a non-existent GovActionId.** `GovActionsDoNotExist (NonEmpty
   GovActionId)`. `Gov.hs`: any voted-on gaId not found in
   `curGovActionIds = proposalsActionsMap proposals` is collected into
   `unknownGovActionIds` -> `failOnNonEmpty ... GovActionsDoNotExist`.
   Payload is just the GovActionIds, not voter pairs (multiple voters
   naming the same missing id get deduped). Phase-1, submission time.

3. **Vote from an unregistered DRep.** `VotersDoNotExist (NonEmpty
   Voter)`. `internVoter` looks the credential up in `knownDReps = vsDReps
   certVState` (from `certStateAfterCERTS`); `Map.lookup` miss -> voter goes
   to the `unknownVoters` set -> `failOnNonEmpty unknownVoters
   VotersDoNotExist`. Same constructor/mechanism covers a bogus
   `StakePoolVoter` (pool never registered) or `CommitteeVoter` (hot cred
   never authorized) — `internVoter` does the same `Map`/`Set` lookup for
   all three roles. Phase-1, submission time.

4. **Vote from a DEREGISTERED DRep** (registered, unregistered in an
   EARLIER tx, then votes). Same as scenario 3: by the time this later tx's
   GOV rule runs, the earlier tx's CERTS/GOVCERT already removed the
   credential from `vsDReps` -> `VotersDoNotExist`. Also true if the
   unregister cert is in the SAME tx as the vote (CERTS-before-GOV ordering
   above). **Adjacent gotcha, NOT what you asked but a trap for a similarly-
   shaped test**: unregistering a DRep does NOT hard-fail votes that DRep
   already cast in EARLIER transactions/blocks on STILL-PENDING proposals —
   those recorded votes get silently stripped from `gasDRepVotesL` for every
   pending proposal via `cleanupProposalVotes`/`unregisteredDReps`
   (`Gov.hs:614-625`), reported only as a `GovRemovedVotes` event, never a
   predicate failure. If you write a test expecting a hard reject for "DRep
   unregisters after having already voted", that expectation is WRONG — it
   succeeds and the vote is quietly zeroed out instead.

5. **Wrong voter role / action-type mismatch.** Confirmed a REAL Phase-1
   hard reject, not "uncounted at ratification": `DisallowedVoters
   (NonEmpty (Voter, GovActionId))`, from `checkVotersAreValid` calling
   `isCommitteeVotingAllowed` / `isDRepVotingAllowed` /
   `isStakePoolVotingAllowed` (`Governance/Internal.hs`). Exact matrix
   (`NoVotingAllowed` = hard-rejected via `DisallowedVoters`;
   `VotingThreshold`/`NoVotingThreshold` = both count as "allowed", handled
   later at ratification):

   | GovAction              | StakePoolVoter                                  | CommitteeVoter    | DRepVoter |
   |-------------------------|-------------------------------------------------|--------------------|-----------|
   | NoConfidence            | allowed                                          | **DISALLOWED**     | allowed |
   | UpdateCommittee         | allowed                                          | **DISALLOWED**     | allowed |
   | NewConstitution         | **DISALLOWED**                                   | allowed            | allowed |
   | HardForkInitiation      | allowed                                          | allowed            | allowed |
   | ParameterChange         | allowed only if the PPU touches a `SecurityGroup` param, else **DISALLOWED** | allowed | allowed |
   | TreasuryWithdrawals     | **DISALLOWED**                                   | allowed            | allowed |
   | InfoAction              | allowed (no threshold either way)                | allowed            | allowed |

   DReps are NEVER `NoVotingAllowed` for any action type (only gated
   separately by the bootstrap-phase check, #13 below). So "a DRep voting on
   NoConfidence" is a BAD test case — it's legal and would NOT reject. Good
   test cases: an `SPO` voting on `TreasuryWithdrawals` or `NewConstitution`
   or a non-security `ParameterChange`; a `CommitteeVoter` voting on
   `NoConfidence` or `UpdateCommittee`.

6. **Proposal deposit below `govActionDeposit`.** `ProposalDepositIncorrect
   (Mismatch RelEQ Coin)` — `mismatchSupplied = pProcDeposit,
   mismatchExpected = pp^.ppGovActionDepositL`. Check is `pProcDeposit ==
   expectedDeposit`, not `>=`. Phase-1, submission time.

7. **Proposal deposit ABOVE the parameter.** Also rejected, same
   constructor — the check is exact equality, not a minimum. An
   over-deposit is just as invalid as an under-deposit. If your test suite
   assumed only under-deposits fail, that assumption is wrong.

8. **TreasuryWithdrawals exceeding treasury balance.** NOT checked at
   proposal-submission time — `Gov.hs`'s `processProposal` has no treasury-
   balance check at all for `TreasuryWithdrawals` (only: registered-return-
   account, network-id match, guardrails-script-hash, sum-nonzero). The
   proposal is ACCEPTED regardless of amount. The actual gate is at
   RATIFICATION, every epoch: `withdrawalCanWithdraw govAction ensTreasury`
   in `Rules/Ratify.hs:292-295` (`sum wdrls <= ensTreasury`, simple `<=`,
   no partial payout). If unaffordable this epoch, the action just isn't
   ratified this pass (stays pending, retried next epoch); if it later
   crosses its own `gasExpiresAfter` still unaffordable, it moves to
   `rsExpiredL` (expired, never enacted, deposit returned) via the
   `ratifyTransition` else-branch (`Ratify.hs:353-359`). `ensTreasury` is
   threaded/decremented across actions processed in the SAME ratify pass
   (priority order: `NoConfidence(0) < UpdateCommittee(1) <
   NewConstitution(2) < HardForkInitiation(3) < ParameterChange(4) <
   TreasuryWithdrawals(5) < InfoAction(6)`, `Internal.hs:534-544`), so with
   TWO competing treasury-withdrawal proposals in the same epoch whose sum
   exceeds the treasury, the one processed first (by that ordering) can
   drain funds out from under the second, which then fails
   `withdrawalCanWithdraw` and doesn't enact THAT epoch. No single action is
   ever partially paid — it's all-or-nothing per action, per epoch.

9. **TreasuryWithdrawals to an unregistered reward account.** Required to
   be registered AT PROPOSAL SUBMISSION time, checked per-address in
   `Gov.hs:509-520`: any withdrawal target credential not in
   `certDState^.accountsL` -> `TreasuryWithdrawalReturnAccountsDoNotExist
   (NonEmpty AccountAddress)`. Phase-1, submission time. NOT verified: what
   happens if the account gets unregistered AFTER proposal submission but
   BEFORE ratification/enactment — `ENACT`'s `PredicateFailure = Void`
   (total, can't fail, `Rules/Enact.hs:75`) so it can't re-reject; it just
   folds the withdrawal into `ensWithdrawals` (`Enact.hs:97-103`) for a
   later sweep into account balances. Whether that sweep silently drops
   funds for a since-deregistered account or something else happens is
   UNVERIFIED — didn't trace far enough (would need the EPOCH-boundary
   withdrawal-sweep code, likely near `drainAccounts`/`applyRUpd`
   territory). Flag as open if a test wants to hit this exact edge.

10. **Wrong/stale `prev_gov_action_id`.** Confirmed: canonical
    `Conway.Rules.Gov` DOES `failBecause` — matches what dugite issue #914
    already fixed to, contradicting the old comment that claimed Haskell
    silently drops these. `runProposalsAddAction` (`Governance/Proposals.hs:
    305-333`) returns `Nothing` when the proposal's stated parent is
    neither the current lane root NOR any node already present in that
    lane's pending graph; `Gov.hs:564-566` turns that into
    `proposals <$ failBecause (injectFailure $ InvalidPrevGovActionId
    proposal)`. Payload is the WHOLE `ProposalProcedure`, not just the
    stale id. Phase-1, submission time.

11. **Two competing proposals, same lane, same tx or same epoch.**
    ACCEPTED, not rejected — this is the CIP-1694 governance-forest design,
    not a bug surface. Each governance-purpose lane (`HardFork`, `Committee`,
    `Constitution`, `PParamUpdate`) is a TREE (`Governance/Proposals.hs`
    `PRoot`/`PGraph`/`PEdges`), and `runProposalsAddAction`'s `update`
    function explicitly allows MULTIPLE children to share one parent
    (`prChildrenL %~ Set.insert newId`, a `Set` not a single slot) —
    siblings referencing the SAME valid parent (the current root, or an
    already-pending non-root node) are both accepted and coexist. Only a
    parent that matches NEITHER the root NOR any live node in that lane's
    graph triggers `InvalidPrevGovActionId` (scenario 10). At the epoch
    boundary, `proposalsApplyEnactment` (`Proposals.hs:485-507`, doc
    comment verbatim: "the sequence of enacted action-ids is promoted to
    the root, removing competing/sibling action-ids and their descendants
    at each step") prunes ALL losing siblings (and their descendants) the
    moment one of them enacts — win-or-vanish, all in the same boundary.
    After that pruning, voting on a pruned loser hits `GovActionsDoNotExist`
    (scenario 1's second sub-case), not a fresh rejection at proposal time.
    **If you were planning to encode "competing proposals get rejected" as
    a test assertion, drop it — it's the wrong expectation.**

12. **Proposal return-address / treasury-withdrawal-target on the wrong
    network.** Two distinct constructors depending on which field:
    - Proposal's own return address (`pProcReturnAddr`) network mismatch ->
      `ProposalProcedureNetworkIdMismatch AccountAddress Network`
      (`Gov.hs:532-535`, compares `aaNetworkId pProcReturnAddr` against
      `networkId` from `ShelleyBase`).
    - `TreasuryWithdrawals` target account(s) network mismatch ->
      `TreasuryWithdrawalsNetworkIdMismatch (NonEmptySet AccountAddress)
      Network` (`Gov.hs:538-544`, filters `Map.keysSet wdrls` for
      `aaNetworkId /= expectedNetworkId`, can list MULTIPLE bad accounts at
      once). Both Phase-1, submission time.

## Open / unverified (flagged, not asserted)

- Scenario 9's post-proposal-deregistration-before-enactment sweep
  behavior — traced only as far as `ENACT` folding into `ensWithdrawals`;
  didn't confirm what the later balance-crediting sweep does for an
  account no longer in `certDState.accounts`.
- Whether GOV's several `runTest`/`failOnNonEmpty` checks within one
  transition can report MULTIPLE simultaneous `ConwayGovPredFailure`s in a
  single `Left` (STS's `Validation`-style accumulation) or short-circuit on
  the first — didn't need this for the scenarios above (each test should
  isolate ONE failure condition anyway) but matters if a test accidentally
  trips two conditions at once and asserts on exact-match-one-failure.
