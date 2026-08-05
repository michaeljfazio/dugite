---
name: conway-gov-rule-verbatim-checks
description: Full conwayGovTransition body walked top-to-bottom from live source — exact order of every check, the guardrails-script-hash equality semantics (constitution SNothing requires proposal SNothing), the two DIFFERENT bootstrap-phase vote gates, UnelectedCommitteeVoters vs VotersDoNotExist using DIFFERENT committee-membership sets, and direct runClause proof that GOV accumulates all failures without short-circuiting
metadata:
  type: reference
---

## Pin
Live-verified 2026-08-05 at commit `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`
(resolves via `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`, dated
2026-08-04T21:48:51Z). Full text of
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` (696 lines) read
top to bottom, plus `Cardano/Ledger/Conway/PParams.hs` (`ppuWellFormed`),
`libs/cardano-ledger-core/.../State/Account.hs` (`isAccountRegistered`),
`.../Conway/Governance.hs` + `.../State/CertState.hs`
(`authorizedElectedHotCommitteeCredentials` / `authorizedHotCommitteeCredentials`),
and `libs/small-steps/.../Control/State/Transition/Extended.hs` (`runClause`,
`?!`, `failOnNonEmpty` etc — same file/mechanism already verified for
DELEG/POOL/GOVCERT in [[deleg-pool-govcert-verbatim-transitions]], re-checked
here specifically against GOV's own import list). Supersedes/refines the
2026-08-02 partial note `conway-gov-vote-proposal-predicate-failures.md` in
`.claude/agent-memory/cardano-ledger-oracle/` — the 19-constructor ADT there
was already byte-exact; this note adds the parts that file flagged
"unverified" (the accumulation question) and several details it didn't cover
at all (guardrails-hash SNothing semantics, bootstrap vote matrix,
UnelectedCommitteeVoters mechanics, exact top-to-bottom ordering).

## The ADT — 19 constructors, CBOR tags 0-18 (confirmed byte-exact against source)

```haskell
data ConwayGovPredFailure era
  = GovActionsDoNotExist (NonEmpty GovActionId)                                       -- 0
  | MalformedProposal (GovAction era)                                                 -- 1
  | ProposalProcedureNetworkIdMismatch AccountAddress Network                         -- 2
  | TreasuryWithdrawalsNetworkIdMismatch (NonEmptySet AccountAddress) Network         -- 3
  | ProposalDepositIncorrect (Mismatch RelEQ Coin)                                    -- 4
  | DisallowedVoters (NonEmpty (Voter, GovActionId))                                  -- 5
  | ConflictingCommitteeUpdate (NonEmptySet (Credential ColdCommitteeRole))            -- 6
  | ExpirationEpochTooSmall (NonEmptyMap (Credential ColdCommitteeRole) EpochNo)       -- 7
  | InvalidPrevGovActionId (ProposalProcedure era)                                     -- 8
  | VotingOnExpiredGovAction (NonEmpty (Voter, GovActionId))                           -- 9
  | ProposalCantFollow (StrictMaybe (GovPurposeId 'HardForkPurpose)) (Mismatch RelGT ProtVer) -- 10
  | InvalidGuardrailsScriptHash (StrictMaybe ScriptHash) (StrictMaybe ScriptHash)       -- 11
  | DisallowedProposalDuringBootstrap (ProposalProcedure era)                          -- 12
  | DisallowedVotesDuringBootstrap (NonEmpty (Voter, GovActionId))                     -- 13
  | VotersDoNotExist (NonEmpty Voter)                                                  -- 14
  | ZeroTreasuryWithdrawals (GovAction era)                                            -- 15
  | ProposalReturnAccountDoesNotExist AccountAddress                                   -- 16
  | TreasuryWithdrawalReturnAccountsDoNotExist (NonEmpty AccountAddress)               -- 17
  | UnelectedCommitteeVoters (NonEmpty (Credential HotCommitteeRole))                  -- 18
```

`InvalidPolicyHash`/`checkPolicy` are `{-# DEPRECATED #-}` pattern synonyms
for `InvalidGuardrailsScriptHash`/`checkGuardrailsScriptHash` — same
constructor, same tag 11, just an old name kept for source compat. GOV is
invoked from Conway LEDGER as `trans @(EraRule "GOV" era)`, wrapped
`ConwayGovFailure` at `ConwayLedgerPredFailure` tag 3.

## `conwayGovTransition` — exact top-to-bottom order (`Gov.hs:460-631`)

Env fields destructured up front: `GovEnv txid currentEpoch pp
constitutionPolicy certState committee`. `certState` here is
**`certStateAfterCERTS`** — this tx's OWN certs already applied (confirmed
in `Rules/Ledger.hs`, and matches the ordering fact already recorded in
[[conway-gov-vote-proposal-predicate-failures]]).

### Step 0 — unelected committee voters (PV11+ only), before anything else

```haskell
when (hardforkConwayDisallowUnelectedCommitteeFromVoting $ pp ^. ppProtocolVersionL) $
  failOnNonEmpty
    (unelectedCommitteeVoters committee committeeState gsVotingProcedures)
    (injectFailure . UnelectedCommitteeVoters)
```

Gate: `hardforkConwayDisallowUnelectedCommitteeFromVoting pv = pvMajor pv >
natVersion @10` (`Era.hs:262-263`, **PV11+**, same "> 10" idiom as the CERTS
withdrawal-split gate but a DIFFERENT named constant — don't conflate them).

**`UnelectedCommitteeVoters` vs `VotersDoNotExist` use DIFFERENT membership
sets — this is the subtle part.** `knownCommitteeMembers` (used later by
`internVoter` to decide `VotersDoNotExist`) is:

```haskell
knownCommitteeMembers = authorizedHotCommitteeCredentials committeeState
```

— i.e. `authorizedHotCommitteeCredentials` (`CertState.hs:338-343`): ALL hot
credentials in CertState's `vsCommitteeState` that are currently
`CommitteeHotCredential` (not resigned), **regardless of whether their cold
credential is part of the currently-ENACTED committee**. But the PV11+
`UnelectedCommitteeVoters` check uses:

```haskell
unelectedCommitteeVoters committee committeeState votingProcedures =
  let authorizedElectedCommittee = authorizedElectedHotCommitteeCredentials committee committeeState
   in ... Set.notMember hotCred authorizedElectedCommittee ...
```

`authorizedElectedHotCommitteeCredentials` (`Governance.hs:581-591`):

```haskell
authorizedElectedHotCommitteeCredentials committee committeeState =
  case committee of
    SNothing -> Set.empty
    SJust electedCommittee ->
      authorizedHotCommitteeCredentials $ CommitteeState $
        csCommitteeCreds committeeState `Map.intersection` committeeMembers electedCommittee
```

— INTERSECTS with `committeeMembers` of the currently ENACTED committee
(`geCommittee`, threaded from `govState ^. committeeGovStateL`). **If there
is no enacted committee at all (`SNothing`), this set is EMPTY — every
single `CommitteeVoter` vote is `UnelectedCommitteeVoters` at PV11+ when no
committee exists**, even from a hot key that's perfectly validly authorized
in CertState.

Net effect: a hot-authorized committee credential whose COLD credential was
later removed from the enacted committee (via `NoConfidence` or
`UpdateCommittee`'s remove-list) but never explicitly resigned/deregistered
in CertState:
- **PV<=10**: still counts as a KNOWN voter (`knownCommitteeMembers`
  doesn't check enactment) — passes `VotersDoNotExist`, and (per the
  existing voter-role/action-type matrix, unconditional at every PV) is NOT
  independently rejected by GOV at all; whatever downstream ratification
  does with a stale committee vote is outside GOV's concern.
- **PV>=11**: STILL passes `VotersDoNotExist` (same set, unchanged) but is
  now ADDITIONALLY hard-rejected by `UnelectedCommitteeVoters` — the tx is
  rejected regardless of `VotersDoNotExist` not firing. Two independently
  computed sets, only one PV-gated.

### Step 1 — proposal fold (`foldlM' processProposal st (indexedGovProps ...)`)

Sequential LEFT fold over every `(idx, ProposalProcedure)` in
`gsProposalProcedures` (index = position within THIS tx body's proposal
list, used to build `GovActionId txid idx`). The fold does **not**
short-circuit — every proposal in the tx is processed regardless of an
earlier one's failure (see accumulation section below); on a lineage
failure the fold's accumulator (`proposals`) is left UNCHANGED for that one
proposal (`proposals <$ failBecause (...)`), so a LATER proposal in the same
tx cannot chain off a proposal that itself failed to be added.

Per-proposal checks, in this exact order:

1. **`checkBootstrapProposal pp proposal`** -> `DisallowedProposalDuringBootstrap
   (ProposalProcedure era)` (tag 12). Only active `hardforkConwayBootstrapPhase
   pv` (PV9 exactly): `failureUnless (isBootstrapAction pProcGovAction)`.
   `isBootstrapAction` (`Gov.hs:633-639`) = `True` only for `ParameterChange
   {}`, `HardForkInitiation {}`, `InfoAction` — everything else
   (`NoConfidence`, `UpdateCommittee`, `NewConstitution`,
   `TreasuryWithdrawals`) cannot even be PROPOSED during PV9.
2. **HardFork protocol-version-can-follow** -> `ProposalCantFollow (StrictMaybe
   (GovPurposeId 'HardForkPurpose)) (Mismatch RelGT ProtVer)` (tag 10). Only
   applies when the proposal IS a `HardForkInitiation` whose `mPrev` matches
   the current HardFork-purpose root (`pgaids ^. grHardForkL`) — see
   `preceedingHardFork` (`Gov.hs:673-695`) for the exact "is this really the
   next hardfork in the chain" resolution logic (handles both "prev matches
   current root" and "major version is more than one above current" cases
   specially). Failure fires when `not (pvCanFollow prevProtVer newProtVer)`.
3. **`actionWellFormed (pp ^. ppProtocolVersionL) pProcGovAction`** ->
   `MalformedProposal (GovAction era)` (tag 1). ONLY applies to
   `ParameterChange _ ppd _` — `ppuWellFormed pv ppd`
   (`Conway/PParams.hs:935-962`), ALL other `GovAction` constructors are
   vacuously well-formed. `ppuWellFormed` checks (each field only if
   `SJust`, `SNothing` is always fine — "well-formed" means "if you touched
   it, you touched it validly", not "you must touch it"):
   - `ppuMaxBBSizeL`, `ppuMaxTxSizeL`, `ppuMaxBHSizeL`, `ppuMaxValSizeL`,
     `ppuCollateralPercentageL` each `/= 0`
   - `ppuCommitteeMaxTermLengthL`, `ppuGovActionLifetimeL` each `/=
     EpochInterval 0`
   - `ppuPoolDepositCompactL`, `ppuGovActionDepositCompactL`,
     `ppuDRepDepositCompactL` each `/= CompactCoin 0`
   - `ppuCoinsPerUTxOByteL /= CompactCoin 0` — SKIPPED during PV9 bootstrap
     (`hardforkConwayBootstrapPhase pv || isValid (...) ppuCoinsPerUTxOByteL`)
   - `ppu /= emptyPParamsUpdate` — an EMPTY `ParameterChange` (touches
     nothing) is itself malformed
   - `ppuNOptL /= 0` — ONLY checked at **PV>=11** (`pvMajor pv < natVersion
     @11 || isValid (/= 0) ppuNOptL`); at PV10 and below, `nOpt = 0` in a
     `ParameterChange` is NOT rejected by this check (a PV-gated addition to
     `ppuWellFormed` itself, separate from the GOV-level PV gates above).
4. **`unless (hardforkConwayBootstrapPhase pv)`** (i.e. active whenever PV
   is NOT exactly 9 — so this pair of checks is even SKIPPED during PV9
   bootstrap, a detail the earlier partial note did not capture):
   - `isAccountRegistered (refundAddress credential) accounts ?!
     ProposalReturnAccountDoesNotExist refundAddress` (tag 16) — the
     proposal's OWN deposit-return account (`pProcReturnAddrL`) must already
     be a registered reward account. `isAccountRegistered = Map.member cred
     (accounts ^. accountsMapL)` — plain membership, no balance/network
     check here (network is checked separately, step 6 below).
   - if `govAction` is `TreasuryWithdrawals withdrawals _`: every withdrawal
     TARGET address must be registered too -> collects ALL unregistered
     target addresses into one `NonEmpty AccountAddress` ->
     `TreasuryWithdrawalReturnAccountsDoNotExist` (tag 17) — one combined
     failure listing every bad address, not one failure per address.
5. **Deposit exact-equality** (unconditional, not bootstrap-gated):
   `pProcDeposit == pp^.ppGovActionDepositL ?! ProposalDepositIncorrect
   (Mismatch {supplied=pProcDeposit, expected=...})` (tag 4). Both
   under-deposit and over-deposit fail identically — exact equality, not a
   floor.
6. **Return-address network id** (unconditional): `aaNetworkId
   pProcReturnAddr == expectedNetworkId ?! ProposalProcedureNetworkIdMismatch
   pProcReturnAddr expectedNetworkId` (tag 2).
7. **Action-specific checks** (`case pProcGovAction of`):
   - `TreasuryWithdrawals wdrls proposalPolicy`:
     a. Network-id check on EVERY withdrawal target, collected as a
        `NonEmptySet` -> `TreasuryWithdrawalsNetworkIdMismatch mismatched
        expectedNetworkId` (tag 3) — can list multiple bad accounts at once.
     b. **Guardrails script hash** -> `checkGuardrailsScriptHash
        constitutionPolicy proposalPolicy` (see dedicated section below).
     c. Zero-sum check: `F.fold wdrls /= mempty ?! ZeroTreasuryWithdrawals
        pProcGovAction` (tag 15) — sums ALL withdrawal amounts via `Coin`'s
        `Monoid` (addition); an empty withdrawals map or a map whose entries
        sum to `Coin 0` both trigger this (individual entries can't be
        negative, `Coin` is Natural-backed, so this is really just "map
        non-empty with positive total" collapsed into one check).
   - `UpdateCommittee _mPrevGovActionId membersToRemove membersToAdd _qrm`:
     a. `Set.intersection (keysSet membersToAdd) membersToRemove` ->
        `failOnNonEmptySet ConflictingCommitteeUpdate` (tag 6) — a cold
        credential named in BOTH the add and remove sets of the SAME
        proposal.
     b. `Map.filter (<= currentEpoch) membersToAdd` -> `failOnNonEmptyMap
        ExpirationEpochTooSmall` (tag 7) — new member's expiry must be
        STRICTLY greater than `currentEpoch` (`<=` triggers the failure).
   - `ParameterChange _ _ proposalPolicy`: **same** guardrails-hash check as
     TreasuryWithdrawals (`checkGuardrailsScriptHash constitutionPolicy
     proposalPolicy`) — `ParameterChange` also carries an optional guardrail
     policy hash field.
   - `NoConfidence`, `NewConstitution`, `HardForkInitiation`, `InfoAction`:
     `_ -> pure ()`, no additional checks beyond steps 1-6.
8. **Ancestry / lineage** -> `InvalidPrevGovActionId (ProposalProcedure era)`
   (tag 8). `proposalsAddAction actionState proposals`: `Just
   updatedProposals` on success (parent is the current lane root OR an
   already-pending node in that lane's graph — MULTIPLE siblings sharing one
   valid parent are explicitly allowed, see
   [[conway-gov-vote-proposal-predicate-failures]] scenario 11 for the
   forest-pruning mechanics at enactment); `Nothing` -> `proposals <$
   failBecause (injectFailure $ InvalidPrevGovActionId proposal)` — proposal
   rejected, accumulator unchanged, fold continues to the next proposal.

### `checkGuardrailsScriptHash` — the exact constitution-policy-mismatch semantics

```haskell
checkGuardrailsScriptHash expectedHash actualHash =
  failureUnless (actualHash == expectedHash) $
    InvalidGuardrailsScriptHash actualHash expectedHash
```

Call site: `checkGuardrailsScriptHash @era constitutionPolicy proposalPolicy`
— so `expectedHash = constitutionPolicy` (`geGuardrailsScriptHash` in
`GovEnv`, threaded from `govState ^. constitutionGovStateL .
constitutionGuardrailsScriptHashL`, i.e. the CURRENTLY ENACTED constitution's
own guardrail script hash, live at proposal-submission time — NOT frozen
into any pulser, unlike the RATIFY-side treasury figure documented in
[[reference_ratify_frozen_enstreasury]]), `actualHash = proposalPolicy` (the
`StrictMaybe ScriptHash` field carried on THIS `TreasuryWithdrawals` or
`ParameterChange` action itself). Constructor payload order is `(got,
expected)` = `(proposalPolicy, constitutionPolicy)` — matches the doc
comments on the ADT ("The guardrails script hash in the proposal" first,
"...of the current constitution" second).

**Equality is on the whole `StrictMaybe ScriptHash`.** This directly answers
whether `None` is allowed when the constitution has no guardrail script:
**yes, and it's not merely allowed — it's REQUIRED.** `SNothing == SNothing`
is `True`, so if the current constitution's `constitutionGuardrailsScriptHashL`
is `SNothing` (no guardrail script at all), only a proposal that ALSO
supplies `SNothing` for its own policy hash passes; a proposal attaching
`SJust someHash` while the constitution has no guardrail script fails
`InvalidGuardrailsScriptHash` (`SJust someHash /= SNothing`). Symmetrically,
if the constitution DOES have `SJust guardrailHash`, a proposal must supply
`SJust` the SAME hash — neither `SNothing` nor a different `SJust` hash
passes. There is no "guardrail script absent means anything goes" escape
hatch; it's exact equality in both directions.

### Step 2 — vote processing (after the ENTIRE proposal fold completes)

`curGovActionIds = proposalsActionsMap proposals` uses the POST-fold
`proposals` — **a vote in this tx CAN legally target a gov action THIS SAME
TX just proposed** (propose-then-vote-in-one-tx is valid). Vote-interning
against `knownDReps`/`knownStakePools`/`knownCommitteeMembers` uses
`certStateAfterCERTS` (this tx's own cert registrations already visible).

1. `failOnNonEmpty unknownVoters (injectFailure . VotersDoNotExist)` (tag
   14) — `internVoter` (`DRepVoter`/`StakePoolVoter`/`CommitteeVoter`) fails
   to find the credential in the respective known-set.
2. `failOnNonEmpty unknownGovActionIds (injectFailure . GovActionsDoNotExist)`
   (tag 0) — voted-on `GovActionId` not in `curGovActionIds` (never
   proposed, OR expired-and-swept, OR pruned as a losing sibling — all three
   collapse to the same failure once the id is gone from the map, see
   [[conway-gov-vote-proposal-predicate-failures]] scenario 1).
3. `runTest $ checkBootstrapVotes pp knownVotes` -> `DisallowedVotesDuringBootstrap
   (NonEmpty (Voter, GovActionId))` (tag 13). Only active at PV9
   (`hardforkConwayBootstrapPhase`), and **the matrix is DIFFERENT per voter
   type** — this is a separate, distinct bootstrap gate from
   `checkBootstrapProposal`'s (step 1's action-type restriction applies to
   what can be PROPOSED; this one restricts who can VOTE and on what):
   ```haskell
   DRepVoter {} | gasAction gas == InfoAction -> True   -- DReps: ONLY InfoAction
   DRepVoter {} -> False                                -- any other action: DISALLOWED for DReps
   _ -> isBootstrapAction $ gasAction gas                -- Committee/StakePool: same 3-action allowlist as proposals
   ```
   So during PV9, a DRep can vote ONLY on `InfoAction` proposals — even
   `ParameterChange`/`HardForkInitiation` (which CAN be proposed during
   bootstrap) cannot receive a DRep vote; CommitteeVoter/StakePoolVoter are
   restricted to the same 3-action allowlist as `isBootstrapAction`.
4. `runTest $ checkVotesAreNotForExpiredActions currentEpoch knownVotes` ->
   `VotingOnExpiredGovAction` (tag 9) — `curEpoch <= gasExpiresAfter`
   required (using post-fold state, so a proposal added by THIS tx can never
   be "expired" in the same tx that created it).
5. `runTest $ checkVotersAreValid currentEpoch committeeState knownVotes` ->
   `DisallowedVoters` (tag 5) — the voter-role x action-type matrix,
   UNCONDITIONAL at every PV (not bootstrap-gated), documented in full in
   [[conway-gov-vote-proposal-predicate-failures]] scenario 5.

### Step 3 — state update, non-failing

`updatedProposalStates` folds every `(voter, vote, gas)` in
`knownVotesWithCast` into the proposals (even ones flagged by step 2's
`runTest` checks — since `runTest` only records a failure, it does not
remove the entry from the fold; irrelevant to final wire state since ANY
recorded failure rejects the whole tx, no partial application). Then
`cleanupProposalVotes` strips DRep votes on STILL-PENDING proposals cast in
EARLIER transactions, for any DRep this SAME tx unregisters
(`UnRegDRepTxCert` in `gsCertificates`) — reported only as the
`GovRemovedVotes` event, never a predicate failure (matches
[[conway-gov-vote-proposal-predicate-failures]] scenario 4's "adjacent
gotcha": a DRep unregistering AFTER already voting in an earlier tx is not a
hard reject, the vote is just silently zeroed).

## THE accumulation fact, confirmed directly against GOV's own source

`Gov.hs` imports exactly `failBecause, failOnJust, failOnNonEmpty,
failOnNonEmptyMap, failOnNonEmptySet, judgmentContext, liftSTS, tellEvent,
(?!)` from `Control.State.Transition.Extended` — notably **NOT**
`whenFailureFree`/`ifFailureFree` (the "skip if state already failing"
combinator exists in the framework but GOV never uses it). Every one of
those imported combinators compiles to the same primitive
(`Extended.hs:394-447`):

```haskell
(?!) cond onFail = liftF $ Predicate (if cond then Success () else Failure (onFail :| [])) id ()
failBecause = (False ?!)
failOnJust cond onJust = liftF $ Predicate (failureOnJust cond onJust) id ()
failOnNonEmpty cond onNonEmpty = liftF $ Predicate (failureOnNonEmpty cond onNonEmpty) id ()
-- failOnNonEmptySet / failOnNonEmptyMap: identical shape
```

...which `runClause` (`Extended.hs:703-708`) interprets as:

```haskell
runClause (Predicate cond orElse val) =
  case vp of
    ValidateNone -> pure val
    _ -> case cond of
      Success x -> pure x
      Failure errs -> modify (first (map orElse (reverse (NE.toList errs)) <>)) >> pure val
```

On failure this **appends** to the accumulated failure list (`modify
(first (... <>) ...)`) and returns `pure val` — the enclosing do-block
(`processProposal`, and `conwayGovTransition` itself) is NOT aborted; every
subsequent `?!`/`failOnNonEmpty`/etc call in the SAME proposal and every
SUBSEQUENT proposal in the fold still runs. Confirmed for GOV specifically
(same file/mechanism already established for DELEG/POOL/GOVCERT in
[[deleg-pool-govcert-verbatim-transitions]], now independently re-checked
against GOV's own import list and call sites):

- **Within one proposal**: all applicable checks among steps 1-8 above fire
  and accumulate — e.g. a proposal that is BOTH malformed (bad
  `ParameterChange`) AND has an incorrect deposit AND targets the wrong
  network will produce `MalformedProposal` + `ProposalDepositIncorrect` +
  `ProposalProcedureNetworkIdMismatch` all in the same `Left` list, not just
  the first one hit.
- **Across proposals**: the `foldlM'` processes every proposal in
  `gsProposalProcedures` regardless of earlier failures — N bad proposals in
  one tx body produce N (or more, if any bad proposal itself has multiple
  independent failures) accumulated `ConwayGovPredFailure`s.
- **Across votes**: the five vote-related top-level checks
  (`VotersDoNotExist`, `GovActionsDoNotExist`, `DisallowedVotesDuringBootstrap`,
  `VotingOnExpiredGovAction`, `DisallowedVoters`) each independently scan
  ALL votes in the tx and can each produce their own NonEmpty list; a tx
  with one vote from an unknown voter and a different vote on an expired
  action gets BOTH `VotersDoNotExist` and `VotingOnExpiredGovAction`
  together.
- Final accept/reject verdict is still all-or-nothing (any non-empty
  accumulated list rejects the whole tx) — accumulation changes what's
  IN the rejection's failure list (wire-visible via `MsgRejectTx`), not
  whether the tx is rejected. A Rust port that returns only the first
  triggered `ConwayGovPredFailure` instead of the full accumulated list
  will diverge from cardano-node's wire response on any multi-failure tx,
  even though both sides agree the tx is invalid.

## Open / not independently re-verified in this pass
- Exact `Withdrawals`/`ProposalProcedure`/`GovAction` CBOR field encodings —
  not re-derived here, assumed from type names and prior CBOR-framing notes.
- `preceedingHardFork`'s full interaction with multiple pending
  HardForkInitiation proposals across epochs — read the function body but
  didn't construct a full scenario matrix beyond what's summarized above.
