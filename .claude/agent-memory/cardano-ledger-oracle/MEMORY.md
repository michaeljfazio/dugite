# Cardano Ledger Oracle Memory

## Meta
- [KB table files in system prompt don't exist](kb-table-files-missing-use-live-github.md) — use live GitHub via `gh api`; confirmed missing 2026-07-07, `gh` authenticated for search/code/contents

## cardano-cli / cardano-api hashing & sizing (script hash, ref-script-size, anchor/metadata hash)
- [Plutus script hash retains ONE CBOR bstr wrapper](plutus-script-hash-retains-one-cbor-bstr-wrapper.md) — hashes `tag <> CBOR-bstr(flat_bytes)`, NOT bare flat bytes; the "double CBOR encoding" is real. Empirically verified vs a real mainnet PlutusV2 hash via Koios (live-verified 2026-08-05).
- [getReferenceInputsSizeForTxIds / txNonDistinctRefScriptsSize](getreferenceinputssize-and-refscriptsize-nondistinct-sum.md) — cardano-cli `query ref-script-size` uses the SAME primitive as CIP-0112 tiered ref-script fees: sum of `originalBytesSize` per matching TxIn, non-deduplicated (live-verified 2026-08-05).
- [anchor-data / drep metadata-hash / stake-pool metadata-hash](anchor-data-and-metadata-hash-raw-bytes-no-canonicalization.md) — all three = `blake2b_256(raw bytes)`, no JSON-LD canonicalization anywhere; stake-pool alone adds a 512-byte+schema validation GATE that doesn't touch the hash input (live-verified 2026-08-05).

## Dijkstra era (nested transactions) — UNRELEASED, master-only
- [Dijkstra sub-tx wire format + SUB-rule chain (live-verified 2026-08-05 @ 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 = master HEAD)](dijkstra-subtx-wire-and-sub-rule-chain.md) — ONE GADT `DijkstraTxBodyRaw l era`, full SubTx CBOR key table, DijkstraSubTx has own TxWits, SUBUTXOW/SUBCERTS-family/SUBGOV wiring + full pred-failure lists. cabal=0.4.0.0 unreleased, latest tag 0.3.0.0
- [conwayDelegTransition/poolTransition/conwayGovCertTransition verbatim (live-verified 2026-08-05 @ 4849c13d)](deleg-pool-govcert-verbatim-transitions.md) — full fn bodies, exact PV hardfork gates, IncorrectDepositDELEG vs DepositIncorrectDELEG partitioned by PV11, VRF dedup registry scope, committee resigned/unknown share one code path. Plus the GENERAL STS fact: `?!` never short-circuits within a rule body (Extended.hs runClause Predicate case) — all applicable failures per-cert accumulate, wire-visible in MsgRejectTx.

- [Dijkstra SUBLEDGERS/SUBLEDGER/SUB* rule pipeline](dijkstra-sub-transaction-pipeline.md) — verbatim source @ 4849c13d: fold structure, full witness/cert/gov surface per sub-tx, all predicate-failure ADTs, no TxIx on failures

## STS Framework / Trusted Replay Mechanics
- [ValidateNone/reapplySTS predicate-skip mechanics](reapply-validatenone-predicate-skip-mechanics.md) — trusted block reapply skips every `?!`/`runTest` check; state transitions still run in full (live-verified 2026-07-07)
- [applySTS all-failures-accumulate, all-or-nothing state](dijkstra-sub-transaction-pipeline.md) — nested `trans` failures never halt the do-block early, but the caller only ever sees `Left`; computed State discarded on any failure anywhere in the tree

## Reward Calculation (Byte-Exact)
- [Reward formula 3-stage floor chain + sigma vs sigmaA](reward-calc-floor-chain-and-sigma-vs-sigmaA.md) — 3 independent `rationalToCoinViaFloor` stages; sigma vs sigmaA distinct; PParams from prior epoch; pledge-gate `<=` on go-snapshot (live-verified 2026-07-07)
- [pv<=2 filterRewards + applyRUpdFiltered verbatim](shelley-filter-rewards-apply-rupd-verbatim.md) — deleteFindMin keeps Leader-first MIN; unreg min→treasury, extras→deltaR2→reserves; deltaR2 uses FROZEN pv; LAST filtered boundary is one epoch AFTER the HF epoch starts (live-verified 2026-08-10 @ adcb341f)
- [#1074 first-pulse prefilter hole](project_1074_first_pulse_prefilter_hole.md) — mainnet 233→236 treasury-high divergence = pulse runs BEFORE rupd_addrs_rew capture with `is_none_or` permissive default; ONE dereg'd cred at queue index 3, proven to the lovelace (70,698/163,916 exact); pv≥7 networks structurally blind
- [mkPoolRewardInfo zero-block-pool gate](mkpoolrewardinfo-zero-block-pool-gate.md) — gate lives INSIDE mkPoolRewardInfo (`Map.lookup` into BlocksMade), not in startStep's iteration; mkApparentPerformance's `d>=0.8=>1` branch is unreachable for a zero-block pool; zero-block pool gets NO reward at all, leader or member (live-verified 2026-08-05 @ 4849c13d)
- [NonMyopic (likelihoodsNM/rewardPotNM) precision + CHaP pinning method](nonmyopic-leaderprobability-precision-and-float-cbor.md) — leaderProbability's 3 `realToFrac` boundaries; getSigma uses `sigma` (totalStake denom), never sigmaA; allPoolInfo keys = ALL go-snapshot pools; updateNonMyopic fires ONLY in completeRupd (Phase 3), rewardPotNM = `_R` (post-treasury-cut pot); EncCBOR Float = unconditional 0xfa. Includes the per-package CHaP index-state pinning method (different cardano-ledger sub-packages pin DIFFERENT commits). Diffed clean vs master (live-verified 2026-08-08).
- [RewardUpdate.rs exact CBOR: Map (Cred) (Set Reward), tag-258 per-credential](rewardupdate-rs-field-exact-cbor-encoding.md) — RewardUpdate's own EncCBOR is hand-written (not Rec/To DSL), so `rs` inherits plain generic Map/Set instances verbatim; at PV>=9 each credential's Set Reward IS tag-258-wrapped, the outer Map never gets a tag. Full 3-regime PV table. Flags a possible #938-class definite-vs-indefinite gap in dugite's `enc.map()/enc.array()` calls for the outer map at >23 credentials, unconfirmed. Live-verified 2026-08-20.

## Predicate-Failure CBOR (ConwayUtxoPredFailure and friends)
- [Conway UTXO collateral predicate-failure CBOR: tags + payload shapes](conway-utxo-collateral-predfailure-cbor.md) — InsufficientCollateral=12 (DeltaCoin,Coin), CollateralContainsNonADA=15 (full MaryValue; TRIGGER is netted inputs-minus-return via canonical-zero-pruning MultiAsset subtraction, only the error PAYLOAD picks a non-netted value in 2/3 branches — corrected 2026-08-06 after a tech-lead catch), BabbageNonDisjointRefInputs=22 (bare NonEmpty TxIn array, no tag 258). DeltaCoin EncCBOR = derived-newtype plain signed Integer. Live-verified 2026-08-06 @ f8d6ead7c8.

## CBOR Structure Reference
- [NewEpochState/EpochState/LedgerState/UTxOState encoding](newepochstate-complete-encoding.md) — field order, array sizes. Re-verified verbatim 2026-08-05 @ a88b60bd; corrected a wrong `()`=array(0) claim (see below).
- [Conway PParams array(31) field order](conway-pparams-field-order.md) — all 31 fields indexed 0-30
- [Conway CertState/DState/PState/VState encoding](conway-certstate-encoding.md) — array sizes, StakePoolState vs StakePoolParams, DRepState array(4), CommitteeState=bare map, CommitteeAuthorization sum shape (re-verified 2026-08-05 @ a88b60bd)
- [`()`/StrictMaybe/Maybe EncCBOR wire shapes](unit-strictmaybe-maybe-enccbor-wire-shapes.md) — `()`=CBOR null NOT array(0); 3 distinct optional-value encoders (default StrictMaybe=array-wrapped, encodeNullStrictMaybe/encodeNullMaybe=null-or-bare) — easy to conflate, verified 2026-08-05
- [UTxOState.utxosUtxo MemPack asymmetry + DebugNewEpochState/DebugEpochState always-empty](utxostate-utxo-mempack-asymmetry-debugquery-empty.md) — EncCBOR wraps entries as MemPack-bstr, DecCBOR reads plain TxIn/TxOut; reconciled because these two LSQ queries are QFNoTables ⇒ utxosUtxo is ALWAYS mempty in real cardano-node replies, regardless of live UTxO size (verified 2026-08-05, cardano-ledger + ouroboros-consensus Query.hs)
- [SnapShots new vs old format](snapshots-encoding.md) — array(2) new, array(3) old, StakePoolSnapShot array(10)
- [ConwayGovState encoding](conway-gov-state-encoding-detailed.md) — array(7), nested types
- [Conway Accounts/ConwayAccountState encoding](conway-accounts-encoding.md) — per-account array(4), nullable delegations (fields [2]/[3] are `Maybe` not `StrictMaybe` — corrected 2026-08-05)
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
- [TreasuryWithdrawals enactment verbatim](treasury-withdrawal-enactment-mechanics.md) — ENACT only accumulates ensWithdrawals; EPOCH applies AFTER SNAP; ensTreasury frozen at pulser set; SPO threshold auto-yes; rsDelayed latch; dump signature of one-boundary-early enactment (live-verified 2026-08-15 @ faa7a9dc)
- [Obligations type + totalObligation composition](obligations-type-and-totalobligation-composition.md) — verbatim 4-field record (oblStake/oblPool/oblDRep/oblProposal), Shelley-vs-Conway obligationCertState/obligationGovState split, no committee-deposit field exists. Dugite audited clean (live-verified 2026-08-05 @ 4849c13d).
- [Conway proposal deposit epoch boundary](feedback_proposal_deposit_epoch_boundary.md) — returnProposalDeposits scope, expiry off-by-one, no silent drops
- [DRep expiry and vsNumDormantEpochs mechanics](drep-expiry-numDormantEpochs.md) — computeDRepExpiry formula; counter RESETS via updateDormantDRepExpiry (no-resurrection clamp); expiry NEVER affects psDRepDistr membership (@ faa7a9dc)
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
- **Some 2026-07-04 entries had missing files** — re-derive live before citing. Restored: byron-duplicate-inputs, shelley-ppup (Governance section), MIR-witness half of BBODY entry, POOLREAP (see below, superseded 2026-08-05). Still missing: BBODY ExUnits-scope half.
- [BBODY total-ExUnits scope + MIR/GenesisDeleg witness rules](alonzo-bbody-exunits-and-cert-witnesses.md) — MISSING FILE: maxBlockExUnits sums ALL txs incl IsValid=False
- [GenesisDelegCert maturation + MIR quorum-witness](shelley-genesisdeleg-and-mir-witness-quorum.md) — maturation delay = stabilityWindow ONCE (not 2x); MIR witness intersects genDelegs VALUES; absent in Conway (live-verified 2026-07-06). See [[project_dugite_genesisdeleg_mir_gaps_2026_07_06]]
- [Conway POOLREAP full verbatim mechanics](conway-poolreap-verbatim-mechanics.md) — SUPERSEDES the old missing `shelley-ppup-votedvalue-and-poolparams-adoption.md` pointer. SNAP runs BEFORE POOLREAP in EPOCH; Conway reuses Shelley's POOLREAP unmodified; unregistered-account refunds → treasury; delegations to a retiring pool ARE actively purged via `removeStakePoolDelegations`/`spsDelegators` (refutes a prior "dangling delegation" belief — none exists in current source); futurePoolParams merge runs BEFORE same-boundary retirement snapshot (can redirect refund to a re-registered reward account); refund = deposit at ORIGINAL registration, immutable, never the live `poolDeposit` param (live-verified 2026-08-05 @ 4849c13d).
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
