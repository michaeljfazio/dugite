# Cardano Ledger Oracle Memory

## Meta
- [KB table files in system prompt don't exist](kb-table-files-missing-use-live-github.md) — use live GitHub via `gh api`; confirmed missing 2026-07-07, `gh` authenticated for search/code/contents

## Dijkstra era (nested transactions) — UNRELEASED, master-only
- [Dijkstra sub-tx wire format + SUB-rule chain (live-verified 2026-08-05 @ 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 = master HEAD)](dijkstra-subtx-wire-and-sub-rule-chain.md) — ONE GADT `DijkstraTxBodyRaw l era`, full SubTx CBOR key table, DijkstraSubTx has own TxWits, SUBUTXOW/SUBCERTS-family/SUBGOV wiring + full pred-failure lists. cabal=0.4.0.0 unreleased, latest tag 0.3.0.0

## STS Framework / Trusted Replay Mechanics
- [ValidateNone/reapplySTS predicate-skip mechanics](reapply-validatenone-predicate-skip-mechanics.md) — trusted block reapply skips every `?!`/`runTest` check; state transitions still run in full (live-verified 2026-07-07)

## Reward Calculation (Byte-Exact)
- [Reward formula 3-stage floor chain + sigma vs sigmaA](reward-calc-floor-chain-and-sigma-vs-sigmaA.md) — 3 independent `rationalToCoinViaFloor` stages; sigma vs sigmaA distinct; PParams from prior epoch; pledge-gate `<=` on go-snapshot (live-verified 2026-07-07)

## CBOR Structure Reference
- [NewEpochState/EpochState/LedgerState/UTxOState encoding](newepochstate-complete-encoding.md) — field order, array sizes
- [Conway PParams array(31) field order](conway-pparams-field-order.md) — all 31 fields indexed 0-30
- [Conway CertState/DState/PState/VState encoding](conway-certstate-encoding.md) — array sizes, StakePoolState vs StakePoolParams
- [SnapShots new vs old format](snapshots-encoding.md) — array(2) new, array(3) old, StakePoolSnapShot array(10)
- [ConwayGovState encoding](conway-gov-state-encoding-detailed.md) — array(7), nested types
- [Conway Accounts/ConwayAccountState encoding](conway-accounts-encoding.md) — per-account array(4), nullable delegations
- [OutputTooBigUTxO/maxValSize mechanics](outputtoobigutxo-maxvalsize-exact-mechanics.md) — fresh re-encode not wire bytes; encodeMap definite/indefinite threshold at 23; strict `>` (live-verified 2026-07-31)

## JSON Debug-Dump (aeson ToJSON) — separate from CBOR wire format
- [PPUPState/ProposedPPUpdates/PParamsUpdate JSON keys + Conway "ppups"](ppup-json-field-names-debug-dump.md) — data-driven ppName keys; ProposedPPUpdates is array-of-pairs not object (live-verified 2026-07-06)

## Script Hashing (native/Timelock, Plutus)
- [hashScript/SafeToHash/MemoBytes mechanics](native-script-hash-memobytes-safetohash.md) — hash = prefixTag<>originalBytes, never a re-encode; Timelock decoder tolerates both array forms; prefix native=0x00/V1=0x01/V2=0x02/V3=0x03 (live-verified 2026-07-06)
- [Dugite native-script hash audit: 2 confirmed bugs](project_dugite_native_script_hash_audit_2026_07_06.md) — NOT FIXED: no raw-byte capture (re-encodes instead of hashing wire bytes); shelley/alonzo hard-reject indefinite arrays

## V3 ScriptContext / Plutus Data Encoding
- [ChangedParameters (PParamsUpdate) Plutus Data encoding](changed-parameters-plutus-data-encoding.md) — Data::Map keyed by ppuTag ints; Rational=List not Constr 0

## V1/V2 TxInfo Translation Gates (Conway + Babbage) — escalate to cardano-haskell-oracle for new ground
- [ConwayContextError/BabbageContextError exact gates](v1v2-txinfo-conway-babbage-gates.md) — guardConwayFeaturesForPlutusV1V2 fires on both V1+V2; whole-tx CollectErrors, never per-script skip (live-verified 2026-07-06)
- [PlutusV3 unit-return + Byron-addr drop/error + StakingPtr passthrough](plutus-txinfo-translation-v3unit-byron-pointer.md) — unit-check language-gated not PV-gated; Alonzo drops Byron TxOuts, Babbage+ hard-errors (live-verified 2026-07-06)
- [Dugite Plutus-context audit: 2 divergences + 2 confirmed-correct](project_dugite_plutus_context_audit_2026_07_06.md) — NOT FIXED: rationals not gcd-reduced; Byron-addr error applied to Alonzo too (should drop-only)
- [Certifying/Rewarding/Voting/Proposing witness matrix + scriptsNeeded indexing](conway-plutus-purpose-witness-and-indexing.md) — per-TxCert witness table; guardrails hash is STRICT EQUALITY; hidden CertificateNotSupported restricts V1/V2 to plain certs only (live-verified 2026-08-02)

## Governance / Epoch Boundary Corrections
- [Conway proposal deposit epoch boundary](feedback_proposal_deposit_epoch_boundary.md) — returnProposalDeposits scope, expiry off-by-one, no silent drops
- [DRep expiry and vsNumDormantEpochs mechanics](drep-expiry-numDormantEpochs.md) — computeDRepExpiry formula, cumulative counter, delta-only correction
- [Conway RATIFY/GOV precision facts](conway-ratify-precision-facts.md) — pvCanFollow modulus (point 2 SUPERSEDED below), reorderActions stability, committee zero-threshold gate (live-verified 2026-07-04)
- [computeDRepDistr composition + deposit attribution](drep-distr-deposit-attribution.md) — stake = InstantStake+ProposalDeposits+AccountBalance per credential; deposit keys, ppKeyDeposit excluded (live-verified 2026-07-26)
- [pvCanFollow/preceedingHardFork/ProposalCantFollow exact mechanics](hardfork-pvcanfollow-exact-mechanics.md) — corrects conway-ratify-precision-facts.md #2; 3-way base resolution; GOV=block-apply time (raw-source-verified 2026-07-06)
- [Dugite ratify audit: 3 open divergences](project_dugite_ratify_audit_divergences_2026_07_04.md) — NOT FIXED: pv_can_follow too permissive; committee zero-threshold ordering
- [BoundedRatio decode bounds + Conway ENACT totality](bounded-ratio-decode-and-enact-totality.md) — UnitInterval rejects num>den at decode; a0 unbounded; ENACT PredicateFailure=Void (live-verified 2026-07-06)
- [MIR pot-transfer exact semantics](mir-pot-transfer-semantics.md) — NEWEPOCH-boundary, own-pot-only delta; PredicateFailure=Void; Conway deletes MIR entirely (live-verified 2026-07-06)
- [Shelley PPUP/NEWPP votedFuturePParams](shelley-ppup-votedfutureparams-verified.md) — tally keyed by whole-value identity, no merge on 0-or-2+ proposers; quorum=static sgUpdateQuorum (live-verified 2026-07-06)
- [Dugite #784 PPUP quorum bug: 6 call sites](project_dugite_issue_784_ppup_quorum_fix_2026_07_06.md) — counted distinct proposers instead of byte-identical value at quorum; fix adds `fold_pp_proposals`
- [Conway FuturePParams lifecycle + JSON shape](conway-futurepparams-lifecycle-and-json.md) — 3 ctors, EPOCH always resets to Nothing; solidifies at point-of-no-return = firstSlotNextEpoch-2*stabilityWindow (live-verified 2026-08-02)

## Shelley/Byron/Alonzo Deep-Dive Facts
- **Some 2026-07-04 entries had missing files** — re-derive live before citing. Restored: byron-duplicate-inputs, shelley-ppup (Governance section), MIR-witness half of BBODY entry. Still missing: POOLREAP future-pool-params half, BBODY ExUnits-scope half.
- [BBODY total-ExUnits scope + MIR/GenesisDeleg witness rules](alonzo-bbody-exunits-and-cert-witnesses.md) — MISSING FILE: maxBlockExUnits sums ALL txs incl IsValid=False
- [GenesisDelegCert maturation + MIR quorum-witness](shelley-genesisdeleg-and-mir-witness-quorum.md) — maturation delay = stabilityWindow ONCE (not 2x); MIR witness intersects genDelegs VALUES; absent in Conway (live-verified 2026-07-06). See [[project_dugite_genesisdeleg_mir_gaps_2026_07_06]]
- [POOLREAP future-pool-params adoption](shelley-ppup-votedvalue-and-poolparams-adoption.md) — MISSING FILE: psFutureStakePoolParams Map.merge drops future-only keys
- [Byron duplicate-TxIn + Byron→Shelley translation pot values](byron-duplicate-inputs-and-shelley-translation.md) — translateToShelley zeroes fee/deposit/treasury pots; reserves absorb burned Byron fees (re-verified 2026-07-06)

## CBOR Set-Duplicate Decoding
- [Duplicate TxIn — decode-level PV9 gate, not Phase-1](set-txin-duplicate-decode-pv9-gate.md) — PV>=9 `decodeSetEnforceNoDuplicates` fails decode; PV2-8 silently dedups, no Phase-1 check ever existed (live-verified 2026-07-29)

## Phase-2 Script Collection Errors
- [CollectError(NoCostModel) exact mechanics](nocostmodel-collecterror-exact-mechanics.md) — hard-rejects regardless of isValid; per-script language keying; native scripts excluded before lookup (live-verified 2026-07-06)

## Phase-1/UTXOW Witness + Script Exactness
- [evalTimelock semantics + cert script-witness rules](timelock-and-cert-script-witnessing.md) — MISSING FILE: SNothing bound = automatic FAIL not vacuous pass
- [ppViewHashesMatch/mkScriptIntegrity carve-out + babbageMissingScripts](utxow-script-integrity-and-witness-checks.md) — MISSING FILE: PV11 renames PPViewHashesDontMatch→ScriptIntegrityHashMismatch
- [Plutus script CBOR+Flat deserialise, V1/V2 vs V3 RemainderError](plutus-script-cbor-flat-deserialise.md) — MISSING FILE: trailing-garbage check only enforced V3+; dugite-uplc gap confirmed
- [Babbage/Conway UTXO exact computations](babbage-conway-utxo-exact-computations.md) — MISSING FILE: supplemental datum hashes include ref-input UTxOs; maxValSize is exact PV-aware CBOR length
