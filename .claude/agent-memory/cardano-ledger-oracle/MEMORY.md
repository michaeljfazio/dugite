# Cardano Ledger Oracle Memory

## Meta
- [KB table files in system prompt don't exist](kb-table-files-missing-use-live-github.md) — use live GitHub via `gh api` (authenticated, works for search/code+contents)

## STS Framework / Trusted Replay Mechanics
- [ValidateNone/reapplySTS skip mechanics](reapply-validatenone-predicate-skip-mechanics.md) — `ValidateNone` skips every `?!`/`runTest` check on trusted reapply; state transitions still run in full
- [applySTS all-failures-accumulate, all-or-nothing state](dijkstra-sub-transaction-pipeline.md) — nested `trans` failures never halt the do-block early, but caller only ever sees `Left`; computed State discarded on any failure anywhere in the tree
- [Dijkstra SUBLEDGERS/SUBLEDGER/SUB* rule pipeline](dijkstra-sub-transaction-pipeline.md) — verbatim source SHA 4849c13d: fold structure, full witness/cert/gov surface per sub-tx, all predicate-failure ADTs, no TxIx on failures

## Reward Calculation (Byte-Exact)
- [Reward formula floor chain + sigma vs sigmaA](reward-calc-floor-chain-and-sigma-vs-sigmaA.md) — 3 independent `rationalToCoinViaFloor` stages; sigma(reward-share) vs sigmaA(apparent-performance) distinct; PParams from PREVIOUS epoch

## CBOR Structure Reference
- [NewEpochState/EpochState/LedgerState/UTxOState encoding](newepochstate-complete-encoding.md) — verified field order, array sizes
- [Conway PParams array(31) field order](conway-pparams-field-order.md) — all 31 fields indexed 0-30
- [Conway CertState/DState/PState/VState encoding](conway-certstate-encoding.md) — array sizes, StakePoolState vs StakePoolParams
- [SnapShots new vs old format](snapshots-encoding.md) — array(2) new, array(3) old, StakePoolSnapShot array(10)
- [ConwayGovState encoding](conway-gov-state-encoding-detailed.md) — array(7), nested types
- [Conway Accounts/ConwayAccountState encoding](conway-accounts-encoding.md) — per-account array(4), nullable delegations
- [OutputTooBigUTxO/maxValSize mechanics](outputtoobigutxo-maxvalsize-exact-mechanics.md) — measures fresh re-encode not wire bytes; encodeMap definite/indefinite threshold=23; strict `>`

## JSON Debug-Dump (aeson ToJSON) — separate from CBOR wire format
- [PPUPState/ProposedPPUpdates/PParamsUpdate JSON keys](ppup-json-field-names-debug-dump.md) — ProposedPPUpdates is array-of-pairs not object; PParamsUpdate keys are data-driven ppName; Conway "ppups" renders ConwayGovState

## Script Hashing (native/Timelock, Plutus)
- [hashScript/SafeToHash/MemoBytes mechanics](native-script-hash-memobytes-safetohash.md) — hashScript=prefixTag<>originalBytes NEVER a re-encode; Timelock decoder tolerates both definite/indefinite array; prefix native=0x00/V1=0x01/V2=0x02/V3=0x03
- [Dugite native-script hash audit: 2 confirmed bugs](project_dugite_native_script_hash_audit_2026_07_06.md) — NOT FIXED: no raw-byte capture (re-encodes canonically); shelley/alonzo hard-reject indefinite outer array

## V3 ScriptContext / Plutus Data Encoding
- [ChangedParameters (PParamsUpdate) Plutus Data encoding](changed-parameters-plutus-data-encoding.md) — Data::Map keyed by ppuTag ints; Rational=List not Constr 0

## V1/V2 TxInfo Translation Gates (Conway + Babbage) — escalate to cardano-haskell-oracle if deeper
- [ConwayContextError/BabbageContextError exact gates](v1v2-txinfo-conway-babbage-gates.md) — guardConwayFeaturesForPlutusV1V2 fires on both V1+V2; all failures whole-tx CollectErrors, never per-script skip
- [PlutusV3 unit-return + Byron-addr + StakingPtr passthrough](plutus-txinfo-translation-v3unit-byron-pointer.md) — unit-check is language-gated not PV-gated; Alonzo drops Byron TxOuts, Babbage+ hard-errors
- [Dugite Plutus-context audit: 2 divergences](project_dugite_plutus_context_audit_2026_07_06.md) — NOT FIXED: rationals not gcd-reduced; Byron-addr error applied to all eras incl. Alonzo
- [Certifying/Rewarding/Voting/Proposing witness matrix + scriptsNeeded indexing](conway-plutus-purpose-witness-and-indexing.md) — only bare ConwayRegCert permissionless; guardrails hash is strict equality; hidden CertificateNotSupported sub-restriction

## Governance / Epoch Boundary Corrections
- [Conway proposal deposit epoch boundary](feedback_proposal_deposit_epoch_boundary.md) — returnProposalDeposits scope, expiry off-by-one, no silent drops
- [DRep expiry and vsNumDormantEpochs](drep-expiry-numDormantEpochs.md) — computeDRepExpiry formula, cumulative counter, expiry check `>`
- [Conway RATIFY/GOV precision facts](conway-ratify-precision-facts.md) — pvCanFollow modulus (pt.2 SUPERSEDED, see hardfork-pvcanfollow), committee minSize-before-zero gate
- [computeDRepDistr composition + deposit attribution](drep-distr-deposit-attribution.md) — stake=InstantStake+ProposalDeposits+AccountBalance; deposits keyed by return-address credential
- [pvCanFollow/preceedingHardFork/ProposalCantFollow mechanics](hardfork-pvcanfollow-exact-mechanics.md) — full verbatim Gov.hs; corrects a wrong worked example in conway-ratify-precision-facts
- [Dugite ratify audit: 3 open divergences](project_dugite_ratify_audit_divergences_2026_07_04.md) — pv_can_follow too permissive; HF chaining ignores in-flight parent target (corrected elsewhere); zero-threshold order bug
- [BoundedRatio decode bounds + Conway ENACT totality](bounded-ratio-decode-and-enact-totality.md) — UnitInterval rejects num>den at decode; ENACT PredicateFailure=Void, total
- [MIR pot-transfer semantics](mir-pot-transfer-semantics.md) — standalone NEWEPOCH transition, own-pot-only delta, PredicateFailure=Void; Conway deletes MIR entirely
- [Shelley PPUP/NEWPP votedFuturePParams](shelley-ppup-votedfutureparams-verified.md) — tally keyed by whole PParamsUpdate value; quorum=fixed sgUpdateQuorum genesis constant
- [Dugite issue #784 PPUP quorum bug](project_dugite_issue_784_ppup_quorum_fix_2026_07_06.md) — counted distinct proposers instead of byte-identical value at quorum; fix routes through `voted_future_pparams`
- [Conway FuturePParams lifecycle + JSON shape](conway-futurepparams-lifecycle-and-json.md) — 3 ctors, EPOCH always resets to PotentialPParamsUpdate; solidifies at firstSlotNextEpoch-2*stabilityWindow

## Shelley/Byron/Alonzo Deep-Dive Facts
- Note: several entries below were indexed 2026-07-04; some files went missing and are marked "MISSING FILE" — re-derive live before relying on those
- [GenesisDelegCert maturation + MIR quorum-witness](shelley-genesisdeleg-and-mir-witness-quorum.md) — maturation=stabilityWindow once (not 2x); MIR witness intersects genDelegs VALUES; absent in Conway
- [POOLREAP future-pool-params adoption](shelley-ppup-votedvalue-and-poolparams-adoption.md) — MISSING FILE: psFutureStakePoolParams Map.merge drops future-only keys
- [Byron duplicate-TxIn + Byron→Shelley translation pots](byron-duplicate-inputs-and-shelley-translation.md) — translateToShelleyLedgerStateFromUtxo zeroes fee/deposit/treasury pots
- [BBODY total-ExUnits scope + cert witnesses](alonzo-bbody-exunits-and-cert-witnesses.md) — MISSING FILE: maxBlockExUnits sums ALL txs incl. IsValid=False

## CBOR Set-Duplicate Decoding
- [Duplicate TxIn decode-level PV9 gate](set-txin-duplicate-decode-pv9-gate.md) — PV>=9 `decodeSetEnforceNoDuplicates` fails decode (not a predicate failure); PV2-8 silently dedups

## Phase-2 Script Collection Errors
- [CollectError(NoCostModel) exact mechanics](nocostmodel-collecterror-exact-mechanics.md) — hard-rejects regardless of isValid; per-script language keying; native scripts excluded from lookup

## Phase-1/UTXOW Witness + Script Exactness
- [evalTimelock + cert script-witness rules](timelock-and-cert-script-witnessing.md) — MISSING FILE: SNothing bound = automatic FAIL; per-constructor scriptsNeeded
- [ppViewHashesMatch/mkScriptIntegrity + babbageMissingScripts](utxow-script-integrity-and-witness-checks.md) — MISSING FILE: PV11 renames PPViewHashesDontMatch
- [Plutus script CBOR+Flat deserialise, V1/V2 vs V3](plutus-script-cbor-flat-deserialise.md) — MISSING FILE: V3+ enforces trailing-garbage check, V1/V2 don't; **dugite-uplc gap confirmed**
- [Babbage/Conway UTXO exact computations](babbage-conway-utxo-exact-computations.md) — MISSING FILE: supplemental datum hashes include ref-input UTxOs
