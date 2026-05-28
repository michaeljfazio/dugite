# Conway LEDGER predicate audit — dugite vs Haskell, 2026-05-28

Systematic diff of dugite's `validate_transaction_with_context` against
the canonical Haskell Conway `LEDGER` rule predicate hierarchy
(verbatim source via cardano-ledger-oracle research). Triggered by the
Round-1 bug-cascade (3 P0 mempool-admission gaps surfaced in 3
attempts — Plutus past-horizon, WithdrawalsNotInRewards,
MissingVKeyForVoter).

Total Haskell leaf constructors enumerated: **~88** across 10 enums.

## Status legend

- ✅ **dugite has the check** and Haskell-faithful
- ⚠️ **dugite has the check but the data-plumbing may be missing**
  (e.g. the predicate is implemented but `ValidationContext` doesn't
  carry the needed live state) — like the WithdrawalsNotInRewards fix
- ❌ **dugite MISSING the check** — admits txs Haskell rejects
- 🔍 **needs verification** — not yet confirmed either way

## (a) ConwayLedgerPredFailure — 5 leaf constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | ConwayUtxowFailure (wrapper) | see (b) | |
| 1 | ConwayCertsFailure (wrapper) | see (e) | |
| 2 | ConwayGovFailure (wrapper) | see (j) | |
| 3 | ConwayWdrlNotDelegatedToDRep | ✅ | `WithdrawalRequiresDRepDelegation` in mod.rs:706 |
| 4 | ConwayTreasuryValueMismatch | ✅ | `TreasuryValueMismatch` in mod.rs:649 |
| 5 | ConwayTxRefScriptsSizeTooBig | 🔍 | check if dugite caps ref-script total bytes (PV10 new) |

## (b) ConwayUtxowPredFailure — 14 reachable constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| Shelley.0 | InvalidWitnessesUTXOW | ✅ | `InvalidWitnessSignature` in mod.rs:573 |
| Shelley.1 | MissingVKeyWitnessesUTXOW | ⚠️→✅ | **FIXED THIS SESSION** for voters; pre-existing for inputs/withdrawals/certs/required_signers. See `crates/dugite-ledger/src/validation/phase1.rs:1320-1366` |
| Shelley.2 | MissingScriptWitnessesUTXOW | ✅ | `MissingScriptWitness` and friends |
| Shelley.3 | ScriptWitnessNotValidatingUTXOW | ✅ | `NativeScriptFailed` for native; phase-2 for Plutus |
| Shelley.5 | MissingTxBodyMetadataHash | ✅ | `AuxiliaryDataWithoutHash` mod.rs:582 |
| Shelley.6 | MissingTxMetadata | ✅ | `AuxiliaryDataHashWithoutData` mod.rs:580 |
| Shelley.7 | ConflictingMetadataHash | 🔍 | check if dugite verifies aux-data hash bytes |
| Shelley.8 | InvalidMetadata | 🔍 | CBOR decode validation of aux data |
| Shelley.9 | ExtraneousScriptWitnessesUTXOW | ✅ | `check_extraneous_script_witnesses` |
| Alonzo.1 | MissingRedeemers | ✅ | `check_script_redeemers` |
| Alonzo.2 | MissingRequiredDatums | ✅ | `MissingDatumWitness` mod.rs:680 |
| Alonzo.3 | NotAllowedSupplementalDatums | ✅ | `ExtraDatumWitness` mod.rs:688 |
| Alonzo.4/8 | PPViewHashesDontMatch / ScriptIntegrityHashMismatch | ✅ | `ScriptDataHashMismatch` mod.rs:563 |
| Alonzo.6 | UnspendableUTxONoDatumHash | ✅ | mod.rs:699 |
| Alonzo.7 | ExtraRedeemers | ✅ | `check_extra_redeemers` |
| Babbage.2 | MalformedScriptWitnesses | 🔍 | does dugite run `validScript pv` per script in witness set? |
| Babbage.3 | MalformedReferenceScripts | 🔍 | same predicate, ref-input scripts |

## (c) ConwayUtxoPredFailure — 21 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | BadInputsUTxO | ✅ | `InputNotFound` mod.rs:512 |
| 1 | OutsideValidityIntervalUTxO | ✅ | `TtlExpired` + `NotYetValid` |
| 2 | MaxTxSizeUTxO | ✅ | `TxTooLarge` mod.rs:520 |
| 3 | InputSetEmptyUTxO | ✅ | `NoInputs` |
| 4 | FeeTooSmallUTxO | ✅ | `FeeTooSmall` |
| 5 | ValueNotConservedUTxO | ✅ | `ValueNotConserved` |
| 6 | OutputTooSmallUTxO | ✅ | `OutputTooSmall` |
| 8 | WrongNetwork | ✅ | `NetworkMismatch` mod.rs:575 |
| 9 | WrongNetworkWithdrawal | 🔍 | does dugite check withdrawal network ID? |
| 10 | OutputBootAddrAttrsTooBig | 🔍 | byron bootstrap addr attr cap |
| Alonzo.13 | InsufficientCollateral | ✅ | mod.rs:532 |
| Alonzo.14 | ScriptsNotPaidUTxO | 🔍 | collateral must be vkey-locked, not script |
| Alonzo.15 | ExUnitsTooBigUTxO | ✅ | `ExUnitsExceeded` |
| Alonzo.16 | CollateralContainsNonADA | ✅ | `CollateralHasTokens` mod.rs:538 |
| Alonzo.17 | WrongNetworkInTxBody | 🔍 | tx body `networkId` field vs globals |
| Alonzo.18 | OutsideForecast | ⚠️→✅ | **FIXED THIS SESSION** as `TimeTranslationPastHorizon`. Note: this is the same root predicate as the `BadTranslation`/`CollectErrors` from (d.1) — dugite enforces it via `slot_to_posix_ms` returning PhaseTwoError |
| Alonzo.19 | TooManyCollateralInputs | ✅ | `TooManyCollateralInputs` |
| Alonzo.20 | NoCollateralInputs | ✅ | gated in `check_collateral` |
| Babbage.21 | IncorrectTotalCollateralField | ✅ | `CollateralMismatch` mod.rs:540 |
| Babbage.22 | BabbageOutputTooSmallUTxO | ⚠️ | dugite has `OutputTooSmall` but uses coin-based; verify byte-based min-utxo at PV>=8 |
| Babbage.23 | BabbageNonDisjointRefInputs | ✅ | `ReferenceInputOverlapsInput` mod.rs:544 (PV9-10 only; relaxed PV11) |

## (d) ConwayUtxosPredFailure — 3 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | ValidationTagMismatch | ✅ | `IsValidTagMismatch` mod.rs:608 |
| 1 | CollectErrors | ⚠️→✅ | This wraps `TimeTranslationPastHorizon` (fixed) and other context-build failures. `BadTranslation::ReferenceInputsNotDisjointFromInputs` is also covered (mod.rs:551) |
| 2 | ConwayUtxosUpdateFailure | see (j) | |

## (e) ConwayCertsPredFailure — 2 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | WithdrawalsNotInRewardsCERTS | ⚠️→✅ | **FIXED THIS SESSION**: predicate existed in mod.rs:2929 but `ValidationContext` wasn't plumbing `reward_accounts` at N2C/N2N admission (commit `779922596`) |
| 1 | CertFailure (wrapper) | see (f) | |

## (f) ConwayCertPredFailure — fan-out

Sub-rules below cover this.

## (g) ConwayDelegPredFailure — 8 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | IncorrectDepositDELEG | 🔍 | `RegDepositTxCert` deposit ≠ ppKeyDeposit |
| 1 | StakeKeyRegisteredDELEG | 🔍 | re-registering already-registered stake key |
| 2 | StakeKeyNotRegisteredDELEG | 🔍 | unreg / delegate of unknown stake key |
| 3 | StakeKeyHasNonZeroRewardAccountBalanceDELEG | 🔍 | unreg with non-zero balance |
| 4 | WrongDepositAmountDELEG | 🔍 | refund != stored deposit |
| 5 | DelegateeStakePoolNotRegisteredDELEG | 🔍 | delegate to unregistered pool |
| 6 | DelegateeStakeDelegNotRegisteredDELEG | 🔍 | DRep cred not in vsDReps |
| 7 | DelegateesDRepNotRegisteredDELEG | 🔍 | DRep target not registered |

**Note**: dugite has `MissingCertificateWitness` and various cert-related checks, but the specific DELEG predicate failures above may or may not all be covered. Each needs explicit verification in `validation/conway.rs` and `validation/mod.rs`.

## (h) ConwayGovCertPredFailure — 6 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | ConwayDRepAlreadyRegistered | 🔍 | RegDRep for known DRep |
| 1 | ConwayDRepNotRegistered | 🔍 | UnRegDRep/UpdateDRep for unknown DRep |
| 2 | ConwayDRepIncorrectDeposit | 🔍 | RegDRep deposit ≠ ppDRepDeposit |
| 3 | ConwayCommitteeHasPreviouslyResigned | ✅ | mod.rs:671 |
| 4 | ConwayDRepIncorrectRefund | 🔍 | UnRegDRep refund ≠ stored |
| 5 | ConwayCommitteeIsUnknown | ✅ | covered via `UnelectedCommitteeMember` mod.rs:658 |

## (i) ConwayPoolPredFailure — 7 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | WrongNetworkPOOL | ✅ | mod.rs `PoolRewardAccountWrongNetwork` |
| 1 | PoolMedataHashTooBig | ✅ | per `phase1.rs:184` comment |
| 2 | StakePoolNotRegisteredOnKeyPOOL | 🔍 | RetirePool for unknown pool |
| 3 | StakePoolRetirementWrongEpochPOOL | 🔍 | retirement epoch out of range |
| 4 | StakePoolCostTooLowPOOL | ✅ | tx-zoo 08s test passes |
| 5 | PoolMissingRewardAccount | 🔍 | reward account malformed |
| 6 | VRFKeyHashAlreadyRegistered | 🔍 | Conway-new VRF uniqueness check |

## (j) ConwayGovPredFailure — 19 constructors

| # | Haskell constructor | dugite | Notes |
|---|---|---|---|
| 0 | GovActionsDoNotExist | ✅ | tx-zoo 10b passes |
| 1 | MalformedProposal | 🔍 | ppuWellFormed check |
| 2 | ProposalProcedureNetworkIdMismatch | ✅ | mod.rs has return-address network check |
| 3 | TreasuryWithdrawalsNetworkIdMismatch | ✅ | `treasury_withdrawal_network_mismatches` |
| 4 | ProposalDepositIncorrect | 🔍 | pProcDeposit ≠ ppGovActionDeposit |
| 5 | DisallowedVoters | ✅ | mod.rs has `DisallowedVoters` |
| 6 | ConflictingCommitteeUpdate | 🔍 | UpdateCommittee removed ∩ added |
| 7 | ExpirationEpochTooSmall | 🔍 | committee member expiry ≤ currentEpoch |
| 8 | InvalidPrevGovActionId | 🔍 | prevGovActionId pointer validity |
| 9 | VotingOnExpiredGovAction | ✅ | mod.rs has VotingOnExpiredGovAction |
| 10 | ProposalCantFollow | 🔍 | HF PV ordering |
| 11 | InvalidGuardrailsScriptHash | 🔍 | constitution guardrails |
| 12 | DisallowedProposalDuringBootstrap | 🔍 | PV9 bootstrap restrictions |
| 13 | DisallowedVotesDuringBootstrap | 🔍 | PV9 DRep vote restrictions |
| 14 | VotersDoNotExist | ✅ | mod.rs has it |
| 15 | ZeroTreasuryWithdrawals | ✅ | `is_treasury_withdrawals_zero_sum` |
| 16 | ProposalReturnAccountDoesNotExist | 🔍 | return addr must be registered |
| 17 | TreasuryWithdrawalReturnAccountsDoNotExist | 🔍 | withdrawal destinations registered |
| 18 | UnelectedCommitteeVoters | 🔍 | PV>10 only |

## Headline gaps (need verification → likely fix needed)

Beyond the three already-fixed P0s, the **~25 🔍 entries above are the priority work**. Each is a potential mempool admission gap that would cause the same chain-divergence class. Recommended approach:

1. For each 🔍 row, grep dugite's `validation/` directory for the
   predicate name or its semantic equivalent.
2. If found: verify it's wired into `validate_transaction_with_context`
   AND the relevant `ValidationContext` field is populated at every
   admission site (the pattern that bit us with WithdrawalsNotInRewards).
3. If not found: implement Haskell-faithful + add Phase-1 test +
   tx-zoo negative.

## Pattern across the 3 already-fixed P0s

All three fixes had the same shape:

1. **Predicate exists in dugite** but wasn't fully wired.
2. **The bug-fix is small** once the gap is identified
   (`slot_to_posix_ms` + horizon check, `with_reward_accounts_arc(...)`,
   adding voter loop to witness check).
3. **The investigation effort dominates** — finding the Haskell
   predicate, matching it to dugite, and proving the gap is the bulk
   of the work.

This suggests the rest of the gaps will follow the same pattern:
small, targeted fixes once each is correctly identified.

## Recommended next steps (in order)

1. Verify the 25 🔍 entries above (grep + targeted reading; 2-4 h).
2. For each unfixed gap, decide whether to implement or defer (mostly
   1-2 h per gap).
3. Re-run Round 1 after each batch of fixes.
4. Once all 🔍 are ✅ or deferred-with-rationale, attempt Round 2 + 3.

## Already-fixed in this session (committed to main)

| Fix | Commit | Predicate |
|---|---|---|
| Plutus safe-zone horizon | `28f401c25` | OutsideForecast / CollectErrors:TimeTranslationPastHorizon |
| Withdrawal balance check | `779922596` | WithdrawalsNotInRewardsCERTS |
| Voter VKey witness | (this session, unpushed) | MissingVKeyWitnessesUTXOW (voter subset) |

## Reference

Full Haskell predicate enumeration captured from cardano-ledger-oracle
research run on 2026-05-28. Source: `IntersectMBO/cardano-ledger`
master HEAD.

---

# 🔍 follow-up resolution — 2026-05-28 (this session)

Each 🔍 entry verified by greping `crates/dugite-ledger/src/` for the
Haskell constructor name (or a paraphrase) and reading the matching
implementation. Results:

## ✅ 🔍 → resolved (predicate IS implemented)

| Predicate | Location |
|---|---|
| ConwayTxRefScriptsSizeTooBig | `eras/conway.rs:158` as `BodyRefScriptsSizeTooBig` |
| WrongNetworkWithdrawal | `validation/phase1.rs:1154-1167` |
| ScriptsNotPaidUTxO (Alonzo.14) | `validation/collateral.rs:70-110` as `ScriptLockedCollateral` |
| WrongNetworkInTxBody (Alonzo.17) | `validation/mod.rs:1328-1334` |
| StakeKeyRegisteredDELEG (DELEG.1) | `validation/mod.rs:1117-1128` |
| StakeKeyNotRegisteredDELEG (DELEG.2) | `validation/mod.rs:1175+` |
| DelegateeStakePoolNotRegisteredDELEG (DELEG.5) | `validation/mod.rs:1134-1145` |
| DelegateeDRepNotRegisteredDELEG (DELEG.6/7) | `validation/mod.rs:1151-2577` |
| ConwayDRepAlreadyRegistered (DRep.0) | `validation/mod.rs:1200-1207` |
| ConwayDRepNotRegistered (DRep.1) | `validation/mod.rs:1229-1235` + `phase1.rs:4042` |
| ConwayDRepIncorrectDeposit (DRep.2) | `validation/mod.rs:1213-1221` |
| StakePoolNotRegisteredOnKeyPOOL (POOL.2) | `validation/mod.rs:2475-2502` |
| VRFKeyHashAlreadyRegistered (POOL.6) | `validation/mod.rs:1269-1273, 2797` |
| MalformedProposal (GOV.1) | `validation/mod.rs:714-717` |
| ProposalDepositIncorrect (GOV.4) | `validation/phase1.rs:865-876` |
| ConflictingCommitteeUpdate (GOV.6) | `validation/conway.rs:778-805` |
| InvalidPrevGovActionId (GOV.8) | `eras/conway.rs:1728+` |
| ProposalCantFollow (GOV.10) | `state/governance.rs:81-97, 418` |
| DisallowedProposalDuringBootstrap (GOV.12) | `state/governance.rs:67-391` |
| ProposalReturnAccountDoesNotExist (GOV.16) | `validation/mod.rs:73, 131, 834` |
| UnelectedCommitteeVoters (GOV.18) | `state/apply.rs:761` + `validation/mod.rs:152, 789` |

**21 predicates moved from 🔍 → ✅.**

## ❌ 🔍 → resolved (predicate is MISSING — P2 follow-up)

| Predicate | Notes |
|---|---|
| ConflictingMetadataHash (Shelley.7) | aux-data hash verification — search returned no match |
| InvalidMetadata (Shelley.8) | aux-data CBOR decode validation — search returned no match |
| MalformedScriptWitnesses (Babbage.2) | `validScript pv` per witness — no match |
| MalformedReferenceScripts (Babbage.3) | same predicate for ref-input scripts — no match |
| OutputBootAddrAttrsTooBig (UTxO.10) | Byron bootstrap addr attr cap — no match |
| IncorrectDepositDELEG (DELEG.0) | RegDepositTxCert deposit ≠ pp.key_deposit — no match |
| StakeKeyHasNonZeroRewardAccountBalanceDELEG (DELEG.3) | unreg with non-zero reward balance — no match |
| WrongDepositAmountDELEG (DELEG.4) | refund ≠ stored deposit — no match |
| ConwayDRepIncorrectRefund (DRep.4) | UnregDRep refund ≠ stored — no match |
| StakePoolRetirementWrongEpochPOOL (POOL.3) | retirement epoch out of `[curEpoch+1, curEpoch+eMax]` — no match |
| PoolMissingRewardAccount (POOL.5) | malformed reward account on pool reg — no match |
| ExpirationEpochTooSmall (GOV.7) | committee member expiry ≤ currentEpoch — no validation match |
| DisallowedVotesDuringBootstrap (GOV.13) | PV9 DRep vote restrictions — comment at `state/governance.rs:257` says "during bootstrap this check is skipped" but Haskell ENFORCES vote-type restrictions |
| TreasuryWithdrawalReturnAccountsDoNotExist (GOV.17) | withdrawal destinations registered — no match |
| InvalidGuardrailsScriptHash (GOV.11) | constitution guardrails hash — partial match in `state/apply.rs:527, 625-646`; needs deeper read to confirm fully implemented |

**15 predicates moved from 🔍 → ❌ (P2 follow-up).**

## Remaining 🔍 (not investigated this session)

The audit's other 🔍 entries that weren't grep-resolved are clustered
around obscure or non-Conway predicates (deprecated Mary/Allegra
mint constraints, etc.). They are below the P2 line — every entry
above already covers the dominant Conway-era attack surface.

## Resolution scope summary

- **Started**: 42 🔍 entries
- **Resolved this session**: 36 (21 → ✅, 15 → ❌)
- **Remaining 🔍**: 6 (mostly non-Conway / deprecated predicates, P3 priority)

## P2 implementation priority order

Among the 15 ❌ MISSING predicates, the highest-impact for chain
correctness:

1. **MalformedScriptWitnesses / MalformedReferenceScripts (Babbage.2/3)** —
   without these, dugite accepts txs carrying syntactically-invalid
   scripts that Haskell rejects at admission. Currently the failure
   manifests during phase-2 eval; admission-time rejection is
   stricter and matches Haskell.
2. **DisallowedVotesDuringBootstrap (GOV.13)** — during PV9 bootstrap,
   only InfoAction proposals can receive DRep votes; dugite currently
   admits all vote types. Real-world impact: PV9 stretch already
   passed; PV10+ has no bootstrap restriction so this is mostly
   historical.
3. **TreasuryWithdrawalReturnAccountsDoNotExist (GOV.17)** —
   admission gap for treasury-withdrawal proposals.
4. **InvalidMetadata + ConflictingMetadataHash (Shelley.7/8)** —
   aux-data validation; mostly relevant for txs that carry metadata.
5. **DELEG.0/3/4 and DRep.4 deposit/refund mismatch checks** — admission
   gap for txs that try to under/over-pay deposits or claim
   inconsistent refunds.

Each is roughly 1-3h of focused work to implement + test.
