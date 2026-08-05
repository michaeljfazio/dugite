---
name: dijkstra-sub-transaction-pipeline
description: Dijkstra-era SUBLEDGERS/SUBLEDGER/SUB* STS rule pipeline, verbatim source + all-or-nothing failure semantics (live-verified 2026-08-05, SHA 4849c13d)
metadata:
  type: reference
---

Pinned commit: `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (IntersectMBO/cardano-ledger `master`, 2026-08-04). All paths under `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/`.

## Rule tree (top-level tx)
`LEDGER` (`Rules/Ledger.hs`) runs, in this exact order inside `dijkstraLedgerTransition`:
1. `EraRule "SUBLEDGERS"` FIRST — folds over `subTransactionsStAnnTx stAnnTx :: [StAnnTx SubTx era]` (from `TxBody.dtbrSubTransactions :: OMap TxId (Tx SubTx era)`), producing `LedgerState utxoStateAfterSubLedgers certStateAfterSubLedgers`.
2. Then (only if parent tx `isPhase2ValidTxL == Phase2Valid`): `ENTITIES` (parent's own certs/withdrawals) then `GOV` (parent's own votes/proposals), seeded from the POST-sub-ledger state.
3. Then `UTXOW` (parent's own witnessing + UTXO update) — note its `DijkstraUtxoEnv` is built with `lsCertState ledgerState` = the ORIGINAL pre-everything certState (the `TRC` input param), NOT `certStateFinal`.

`SUBLEDGERS` (`Rules/SubLedgers.hs`): `transitionRules = [dijkstraSubLedgersTransition]`, body is exactly
```haskell
foldM (\ls subTx -> trans @(EraRule "SUBLEDGER" era) $ TRC (env, ls, subTx)) ledgerState subTxs
```
Accumulator type = full `LedgerState era` (UTxOState + CertState), not a narrower sub-ledger type. `Signal (SUBLEDGERS era) = [StAnnTx SubTx era]`.

`SUBLEDGER` (`Rules/SubLedger.hs`, `Signal = StAnnTx SubTx era`) per sub-tx, in order:
- IF top-tx (`sleTopTxIsPhase2Valid`, NOT the sub-tx's own validity — there is no per-sub-tx phase-2 flag) is `Phase2Valid`: `Conway.validateTreasuryValue` (runTest, no sub-rule) -> `SUBENTITIES` (cert/withdrawal processing, incl. `SUBCERTS`) -> `SUBGOV` (reuses `Conway.conwayGovTransition` unchanged). If top-tx is `Phase2Invalid`, BOTH are skipped entirely (`pure (utxoState, certState)`).
- ALWAYS (regardless of phase-2 validity): `SUBUTXOW` -> `SUBUTXO`.

`SUBCERTS`/`SUBCERT` (`Rules/SubCerts.hs`, `Rules/SubCert.hs`): dispatches by `DijkstraTxCert` constructor — `DijkstraTxCertDeleg -> SUBDELEG` (reuses `Conway.conwayDelegTransition` unchanged), `DijkstraTxCertPool -> SUBPOOL` (reuses `Shelley.poolTransition` unchanged), `DijkstraTxCertGov -> SUBGOVCERT` (reuses `Conway.conwayGovCertTransition` unchanged).

## Full witness/cert/gov surface for a sub-tx
This is the FULL set, not a reduced subset — `SUBUTXOW` runs the same checks as top-level `UTXOW` (verified witnesses, needed witnesses, missing datums, metadata hash, script integrity hash, exact redeemer set, well-formed scripts, guard datums), `SUBCERT`/`SUBDELEG`/`SUBPOOL`/`SUBGOVCERT` reuse the literal Conway/Shelley transition functions. The ONE structural difference: **a sub-tx's `TxBody` (`DijkstraSubTxBodyRaw`, `TxBody.hs` line ~194-214) has NO `fee`, NO `collateralReturn`, NO `totalCollateral`, NO `collateralInputs` fields at all** (compare to `DijkstraTxBodyRaw TopTx` which has all four). Correspondingly `SUBUTXO` (`Rules/SubUtxo.hs`) hard-`error`s ("Impossible: ... for SUBUTXO") on `FeeTooSmallUTxO`, `ValueNotConservedUTxO`, `InsufficientCollateral`, `ScriptsNotPaidUTxO`, `ExUnitsTooBigUTxO`, `CollateralContainsNonADA`, `TooManyCollateralInputs`, `NoCollateralInputs`, `IncorrectTotalCollateralField`, `BabbageNonDisjointRefInputs`, `PtrPresentInCollateralReturn`, `WithdrawalsExceedAccountBalance` — these predicates structurally cannot fire for a sub-tx. Fee/collateral/value-conservation is a TopTx-only concept; sub-txs only move value within the batch.
Also: `SUBUTXO`'s actual UTxO-set update is itself gated on the TOP tx's phase-2 validity (`case topTxIsPhase2Valid of Phase2Valid -> updateUTxOAndInstantStake ...; Phase2Invalid -> pure utxoState`) even though the witness/redeemer checks in `SUBUTXOW` run unconditionally before it.

## Failure semantics — CONFIRMED via `libs/small-steps/src/Control/State/Transition/Extended.hs` (same SHA)
Two independent facts combine to give whole-tx-atomic, all-failures-accumulated semantics:
1. **All failures accumulate, nothing short-circuits locally.** `runClause`'s `SubTrans` case (`Extended.hs:713-723`) always calls `pure $ next ss` after a nested `trans` call regardless of failure; `Predicate`'s `Failure errs` branch (`Extended.hs:703-708`) does `modify (...) >> pure val` (continues). `applySTSInternal'` (`Extended.hs:778`) runs `goRule jc \`traverse\` transitionRules` unconditionally — the `isFailing`/`isAlreadyFailing` plumbing only gates pre/post-condition ASSERTIONS, never the rule body. So `foldM` over `subTxs` in `SUBLEDGERS` runs and checks EVERY sub-tx even after an earlier one fails, and the parent tx's own `ENTITIES`/`GOV`/`UTXOW` steps in `dijkstraLedgerTransition` still execute after `SUBLEDGERS` fails — every predicate failure anywhere in the whole nested tree (any sub-tx, any depth, plus the parent's own checks) is collected into one list.
2. **But the caller only ever sees all-or-nothing.** `applySTS = applySTSOptsEither defaultOpts` (`Extended.hs:610-622`), and `applySTSOptsEither` (`Extended.hs:592-608`): `(st, []) -> Right st; (_, pf:pfs) -> Left $ pf :| pfs`. If the accumulated failure list is non-empty ANYWHERE in the tree, the caller gets `Left (NonEmpty failures)` and the internally-computed (possibly garbage/partial) `State` is thrown away — never surfaced. Since Haskell state is immutable, this means: one sub-tx failing (or the parent's own ENTITIES/GOV/UTXOW failing) causes the ENTIRE top-level tx's `LEDGER` application to fail, with ZERO effect from ANY earlier-processed sub-tx and ZERO effect from the parent tx's own certs/gov/utxo — exactly matching ordinary single-tx Shelley-era LEDGER atomicity, just with an extra fold layer inside.

## Predicate failure constructor lists (verbatim, all CBOR tags as coded)
- `DijkstraSubLedgersPredFailure` (SubLedgers.hs): **1 ctor** — `SubLedgerFailure (PredicateFailure (EraRule "SUBLEDGER" era))`. No `TxIx`/index/TxId field anywhere at this level or below — a failure cannot be attributed to "which sub-tx" from the type alone (would need cross-referencing against the OMap position/TxId externally, e.g. in tests/tooling, not in the predicate-failure ADT itself).
- `DijkstraSubLedgerPredFailure` (SubLedger.hs): **4 ctors** — `SubUtxowFailure`, `SubEntitiesFailure`, `SubGovFailure`, `SubTreasuryValueMismatch (Mismatch RelEQ Coin)`. CBOR tags 1/2/3/5 (4 unused).
- `DijkstraSubUtxowPredFailure` (SubUtxow.hs): **18 ctors**, tags 0-17 (`SubUtxoFailure`, `SubInvalidWitnessesUTXOW`, `SubMissingVKeyWitnessesUTXOW`, `SubScriptWitnessNotValidatingUTXOW`, `SubMissingTxBodyMetadataHash`, `SubMissingTxMetadata`, `SubConflictingMetadataHash`, `SubInvalidMetadata`, `SubMissingRedeemers`, `SubMissingRequiredDatums`, `SubNotAllowedSupplementalDatums`, `SubPPViewHashesDontMatch`, `SubUnspendableUTxONoDatumHash`, `SubExtraRedeemers`, `SubMalformedScriptWitnesses`, `SubMalformedReferenceScripts`, `SubScriptIntegrityHashMismatch`, `SubMalformedGuardDatums`).
- `DijkstraSubUtxoPredFailure` (SubUtxo.hs): **10 ctors**, tags 0,1,2,3,4,6,7,8,9,10 (tag 5 skipped/reserved) — `SubBadInputsUTxO`, `SubOutsideValidityIntervalUTxO`, `SubMaxTxSizeUTxO`, `SubInputSetEmptyUTxO`, `SubWrongNetwork`, `SubOutputBootAddrAttrsTooBig`, `SubOutputTooBigUTxO`, `SubWrongNetworkInTxBody`, `SubOutsideForecast`, `SubBabbageOutputTooSmallUTxO`.
- `SubEntitiesPredFailure` (SubEntities.hs): **6 ctors**, tags 0-5 — `SubCertsFailure`, `SubMissingAccountsInWithdrawals`, `SubMissingOriginalAccountsInWithdrawals`, `SubMissingAccountsInDirectDeposits`, `SubWrongNetworkInWithdrawals`, `SubWrongNetworkInDirectDeposits`.
- `DijkstraSubCertsPredFailure` (SubCerts.hs): **1 ctor** (newtype) — `SubCertFailure (PredicateFailure (EraRule "SUBCERT" era))`.
- `DijkstraSubCertPredFailure` (SubCert.hs): **3 ctors**, tags 1/2/3 — `SubDelegFailure`, `SubPoolFailure`, `SubGovCertFailure`.
- `DijkstraSubDelegPredFailure` (SubDeleg.hs): newtype-derives straight from `Conway.ConwayDelegPredFailure era` (identical wire shape).
- `DijkstraSubPoolPredFailure` (SubPool.hs): newtype-derives straight from `Shelley.ShelleyPoolPredFailure era`.
- `DijkstraSubGovCertPredFailure` (SubGovCert.hs): newtype-derives straight from `DijkstraGovCertPredFailure era`.
- `DijkstraSubGovPredFailure` (SubGov.hs): newtype-derives straight from `DijkstraGovPredFailure era`.

`DijkstraLedgerPredFailure` (Ledger.hs) top-level: **6 ctors**, tags 1-6 — `DijkstraUtxowFailure`, `DijkstraEntitiesFailure`, `DijkstraGovFailure`, `DijkstraTreasuryValueMismatch`, `DijkstraTxRefScriptsSizeTooBig`, `DijkstraSubLedgersFailure (PredicateFailure (EraRule "SUBLEDGERS" era))` — tag 6 is the sub-ledger wrapper.

## Notable design facts worth double-checking against any dugite reimplementation
- `Conway.ConwayWdrlNotDelegatedToDRep` is UNREACHABLE in Dijkstra (`error "Impossible"` in both `Ledger.hs` and `SubLedger.hs` conversion functions) — the check does not exist for Dijkstra at all, top or sub level.
- `Conway.ConwayMempoolFailure` moved OUT of LEDGER entirely into a new dedicated `MEMPOOL` rule (`Rules/Mempool.hs`) in Dijkstra — not covered in this note, flag if asked.
- `Shelley.DelegsFailure` is unreachable (`error "Impossible: DELEGS has been removed in Dijkstra"`) — Dijkstra has no `DELEGS` rule, only `ENTITIES`/`SUBENTITIES`.
