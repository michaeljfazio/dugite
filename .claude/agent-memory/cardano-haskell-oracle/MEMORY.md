# Cardano Haskell Oracle - Agent Memory

## Plutus V1/V2 ScriptContext / Conway Gating
- [v1v2-scriptcontext-conway-gates.md](v1v2-scriptcontext-conway-gates.md) — guardConwayFeaturesForPlutusV1V2 + transPlutusPurposeV1V2 both -> BadTranslation -> CollectErrors hard-rejects whole tx; Conway's own transTxOutV1/transTxInInfoV1 drop 2 of 3 Babbage V1 restrictions, keep InlineDatumsNotSupported
- [nocostmodel-collecterror-native-script-exclusion.md](nocostmodel-collecterror-native-script-exclusion.md) — NoCostModel built in Evaluate.hs `apply` via `Map.lookup lang costModelsValid`; per-script language key; native scripts filtered out earlier via `lookupPlutusScript`/`toPlutusScript`, never touch CostModels; merge/gg ordering can hide a later NoCostModel behind an earlier NoRedeemer

## Plutus Builtins / Ledger-API
- [v2v1-paramname-vanrossem-extension-live.md](v2v1-paramname-vanrossem-extension-live.md) — V1/V2 ParamName NOT frozen at Babbage size; batch6 hits V1/V2 too at PV11 (V1=332,V2=332,V3=350), live on preview now
- [v3-paramname-vanrossem-tail-297-349.md](v3-paramname-vanrossem-tail-297-349.md) — exact V3 ParamName idx297-349 table (53 fields/14 builtins), ExpModInteger cost formula coefficients
- [plutus-builtin-availability-gate.md](plutus-builtin-availability-gate.md) — builtinsIntroducedIn batch1-6 table (which DefaultFun exists per LL/PV), PV consts (valentinePV=8 etc.), deserialiseScript rejection path, isValidPlutus
- [plutus-builtins-adversarial-audit.md](plutus-builtins-adversarial-audit.md) — BuiltinSemanticsVariant<->PV table, Trace deferred-unlifting, SliceByteString Int64 bound, secp256k1 zero-r/s & high-S, ExpMod 2^8191 bounds, DropList negative=no-op. ConstrData/Ed25519 sections corrected below.
- [constrdata-word64-gate-version-boundary.md](constrdata-word64-gate-version-boundary.md) — Data=Constr Integer, CBOR tags 121-127/1280-1400/102, Flat=FlatViaSerialise(same CBOR bytes). CRITICAL: ConstrData Word64 gate added in plutus 1.63.0.0 (PR #7754), released ONE DAY AFTER cardano-node 11.0.1 itself — 11.0.1 almost certainly bundles pre-gate plutus, so constrData is UNGATED even at PV11 in dugite's actual current target. unConstrData never gated.
- [ed25519-verify-strict-vs-libsodium-ref10.md](ed25519-verify-strict-vs-libsodium-ref10.md) — donna removed from plutus Feb-2025 (#6848), DEAD CODE now, unconditional libsodium for ALL PV; dalek verify_strict matches libsodium except missing pk-canonical-encoding gate
- [bls12-381-multiscalarmul-semantics.md](bls12-381-multiscalarmul-semantics.md) — multiScalarMul=`zip` truncation (no error on length mismatch); dugite bls.rs gap: skips elem_type check for empty lists
- [builtin-semantics-variant-costing.md](builtin-semantics-variant-costing.md) — costing-only audit: Text char-count→byte/4 at PV11, divide/mod diagonal-shape flips at PV11 but quotient/remainder never flip, SatInt=Int64 saturating throughout

## Consensus / Sync / Genesis
- [lop-historicity-chainsync.md](lop-historicity-chainsync.md) — LoP bucket (cap=100K,500tok/s,Syncing-only), HistoricityCheck cutoff=37h, csIdling GSM gate
- [loe-chain-selection.md](loe-chain-selection.md) — LoE type, trimToLoE algorithm, GDD governor, GSM-gating, Praos bypass
- [gdd-genesis-density-disconnector.md](gdd-genesis-density-disconnector.md) — GDD algorithm: genesis window=3k/f slots, density bounds, 4 disconnection guards
- [gdd-governor-deep-dive.md](gdd-governor-deep-dive.md) — exhaustive gddWatcher internals, all 4 densityDisconnect guards verbatim, CSJ jumper visibility
- [genesis-bootstrap-protocol.md](genesis-bootstrap-protocol.md) — genesis mode bootstrap protocol details
- [ouroboros-genesis-checkpoints.md](ouroboros-genesis-checkpoints.md) — genesis checkpoints mechanism
- [chainsync-at-tip.md](chainsync-at-tip.md) — connection stays open, MsgAwaitReply, pipeline lowMark=200/highMark=300
- [fork-resolution-chainsel.md](fork-resolution-chainsel.md) — ChainSel algorithm: addBlock flow, Praos tiebreaker, rollback, tentative follower
- [fork-switching-mechanism.md](fork-switching-mechanism.md) — fork switch mechanics
- [block-validation-modes.md](block-validation-modes.md) — tickThenReapply (replay, no crypto) vs tickThenApply (full validation)
- [pool-distr-leader-check.md](pool-distr-leader-check.md) — nesPd=ssStakeMarkPoolDistr(es0), memoized not recomputed mid-epoch; dugite bug uses on-the-fly snapshot
- [genesis-initialization-ledger-state.md](genesis-initialization-ledger-state.md) — Byron→Shelley translation, genDelegs vs pool regs, preview init sequence
- [blockfetch-concurrency-architecture.md](blockfetch-concurrency-architecture.md) — BulkSync=1 peer/Deadline=2 peers, 100 max in-flight/peer
- [chaindb-architecture.md](chaindb-architecture.md) — ChainDB internals
- [inbound-connection-rate-limiting.md](inbound-connection-rate-limiting.md) — AcceptedConnectionsLimit hard=512/soft=384/delay=5s, no per-IP cap
- [mithril-snapshot-ledger-init.md](mithril-snapshot-ledger-init.md) — two-archive system, ledger snapshot layout, tickThenReapply/Apply split
- [gsm-haa-syncing-presyncing-regression.md](gsm-haa-syncing-presyncing-regression.md) — Syncing→PreSyncing is HAA-only, NO tip-age term (tip-age only gates CaughtUp↔PreSyncing); OutboundConnectionsState 4-way case split (Genesis-mode BLP-count branch has no closure-over-established-peers condition); root-caused dugite from-genesis freeze to gsm.rs/networking.rs

## Ledger State Snapshots & CBOR Format
- [ext-ledger-state-snapshot-format.md](ext-ledger-state-snapshot-format.md) — state file CBOR: outer array(2)[1,ExtLedgerState], HeaderState telescope, PraosState 8-field array
- [ledger-state-11-0-1-format-changes.md](ledger-state-11-0-1-format-changes.md) — 10.6.2→11.0.1 breaking changes: spsAccountId array(2), Peras field added
- [ledger-snapshot-cbor-format.md](ledger-snapshot-cbor-format.md) — full wire format: HFC telescope, NewEpochState array(7), TxIn MemPack, V1 vs V2 backends
- [utxo-hd-snapshot-format.md](utxo-hd-snapshot-format.md) — UTxO-HD in-memory snapshot: EMPTY UTxO in state file, tables written separately
- [new-epoch-state-cbor.md](new-epoch-state-cbor.md) — NewEpochState CBOR field-by-field
- [cardano-ledger-types-wire-format.md](cardano-ledger-types-wire-format.md) — hash/key types, address header-byte table, MaryValue, script tags, PParams array(31), Rational tag(30)

## Governance (Conway / CIP-1694)
- [drep-dormant-epoch-expiry-exact-mechanism.md](drep-dormant-epoch-expiry-exact-mechanism.md) — DEFINITIVE: numDormantEpochs is bump-at-submission (Certs.hs updateDormantDRepExpiry adds to ALL vsDReps + resets counter), NOT add-at-ratify-check-time; dRepAcceptedRatio is bare `reCurrentEpoch > drepExpiry`; exact permalinks pinned to SHA 8595dbef
- [drep-pulser-ratification.md](drep-pulser-ratification.md) — pulser snapshot at END of EPOCH rule, pulse spreading 4k blocks, RATIFY runs once at finishDRepPulser
- [conway-ratification-precision-audit.md](conway-ratification-precision-audit.md) — 10-pt audit: pvCanFollow(minor==cur+1), HFC prevGovActionId chains, ensTreasury includes this-boundary's RUPD — supersedes vague bits below
- [conway-ratification-details.md](conway-ratification-details.md) — threshold functions, enactment priority, committee expiry, DRep activity, treasury cap
- [conway-gov-rule-deep-dive.md](conway-gov-rule-deep-dive.md) — all 19 ConwayGovPredFailure variants w/ CBOR tags, voter matrix (full doc in haskell-ledger-cross-validation skill)
- [gov-bootstrap-restriction.md](gov-bootstrap-restriction.md) — isBootstrapAction allows ParameterChange/HardForkInitiation/InfoAction only
- [gov-state-cbor-encoding.md](gov-state-cbor-encoding.md) — GetGovState (tag 24): ConwayGovState array(7) encoding
- [conway-gov-state-encoding-detailed.md](conway-gov-state-encoding-detailed.md) — detailed GovState field encoding
- [pparams-group-classification.md](pparams-group-classification.md) — PP group classification, threshold combination logic
- [conway-instant-stake-ptr-exclusion.md](conway-instant-stake-ptr-exclusion.md) — ConwayInstantStake has NO sisPtrStake, dropped via HFC `_other`
- [key-reference-files.md](key-reference-files.md) — file-path index into all Conway era rule modules + Shelley reward/pool-rank modules
- [bounded-ratio-decode-bounds-and-enact-totality.md](bounded-ratio-decode-bounds-and-enact-totality.md) — UnitInterval decode rejects n>d; NonNegativeInterval no upper bound; ENACT is total field-merge, no re-validation

## Rewards / Epoch Transition
- [epoch-nonce-tickn-deep-dive.md](epoch-nonce-tickn-deep-dive.md) — eta0 formula, freeze window per era (3k/f thru Babbage, 4k/f Conway+)
- [epoch-nonce-calculation.md](epoch-nonce-calculation.md) — Praos epoch nonce: PraosState fields, per-block update, stability windows
- [nonintegral-ln-algorithm.md](nonintegral-ln-algorithm.md) — ln' uses continued fraction NOT Taylor series; exact Rational in Haskell vs f64 in dugite
- [reward-iteration-deep-dive.md](reward-iteration-deep-dive.md) — startStep iterates GO snapshot, genesis pool 2-epoch warm-up, 6 zero-reward conditions
- [monetary-expansion-rupd.md](monetary-expansion-rupd.md) — deltaR1/eta/expectedBlocks, block counting, Conway d=0 simplification
- [rupd-timing-data-flow.md](rupd-timing-data-flow.md) — timing windows (sr=4k/f), NEWEPOCH ordering (applyRUpd BEFORE SNAP)
- [newepoch-ordering-details.md](newepoch-ordering-details.md) — exact NEWEPOCH order (applyRUpd→MIR→SNAP→POOLREAP→UPEC→record)
- [reward-update-accounting.md](reward-update-accounting.md) — deltaR/deltaT/deltaF formulas, conservation invariant, undistributed→reserves
- [epoch0-rupd-ssfee-semantics.md](epoch0-rupd-ssfee-semantics.md) — ssFee comes from SnapShots.ssFee (=0 at genesis), not utxosFees
- [babbage-conway-hf-ppup-order.md](babbage-conway-hf-ppup-order.md) — PPUP applied BEFORE translateEra; updateCostModels Map.union left-biased
- [mir-pot-transfer-semantics.md](mir-pot-transfer-semantics.md) — MIR two-phase accumulate/apply; combined-AND solvency; Conway deletes MIRCert entirely
- [conway-validation-rules.md](conway-validation-rules.md) — validation rules, predicate failures, reward formula, epoch transition order
- [apply-chain-tick-forge-mutations.md](apply-chain-tick-forge-mutations.md) — TICK/NEWEPOCH fields visible to forge: intra-epoch vs epoch-boundary

## Forging / VRF / KES
- [vrf-input-construction.md](vrf-input-construction.md) — VRF seed: TPraos vs Praos, mkInputVRF, domain separation
- [vrf-leader-check.md](vrf-leader-check.md) — checkLeaderValue, taylorExpCmp, FixedPoint E34, certNat/certNatMax algorithm
- [block-forging-flow.md](block-forging-flow.md) — slot tick→leader check→tx selection→body hash→header→KES sign
- [block-forging-deep-dive.md](block-forging-deep-dive.md) — deep dive on forging internals
- [forge-pipeline-complete.md](forge-pipeline-complete.md) — forkBlockForging, HFC dispatch, cardanoProtocolVersion=11.0, VRF tiebreaker=5
- [forge-chaindb-interaction.md](forge-chaindb-interaction.md) — forge/ChainDB interaction
- [kes-signing-verification.md](kes-signing-verification.md) — KES signing/verification details
- [block-header-protocol-version.md](block-header-protocol-version.md) — cardanoProtocolVersion hardcoded per-release, ExperimentalHardForksEnabled→11.0

## Network Wire Format (N2N / N2C)
- [n2n-protocols.md](n2n-protocols.md) — mini-protocol IDs, CBOR/CDDL, version negotiation (V14 Plomin mandatory, V15 SRV DNS)
- [n2c-protocol-details.md](n2c-protocol-details.md) — N2C: 4 mini-protocols, 40 query types, QueryVersion2/3, V21 PParams change
- [n2c-version-v17-v22-changes.md](n2c-version-v17-v22-changes.md) — full N2C version change table
- [n2c-query-bugs-tag30-33-35.md](n2c-query-bugs-tag30-33-35.md) — N2C query tag 30/33/35 bugs
- [n2n-chainsync-header-era-tags.md](n2n-chainsync-header-era-tags.md) — ChainSync header era-tag encoding
- [mux-connection-architecture.md](mux-connection-architecture.md) — single TCP/peer, SDU framing, temperature lifecycle, all timeouts
- [n2n-connection-architecture.md](n2n-connection-architecture.md) — MuxMode, DataFlow, bit-15 convention, Hot/Warm/Cold
- [ouroboros-network-architecture.md](ouroboros-network-architecture.md) — repo structure: protocol types, codecs, N2N versions, CDDL paths
- [p2p-governor-architecture.md](p2p-governor-architecture.md) — P2P governor architecture
- [peer-sharing-protocol.md](peer-sharing-protocol.md) — PeerSharing wire format, address encoding, policy constants
- [ledger-peer-snapshot-encoding.md](ledger-peer-snapshot-encoding.md) — GetLedgerPeerSnapshot (tag 34): V1/V2/V3 wire format
- [sdu-segmentation-critical.md](sdu-segmentation-critical.md) — SDUSize=12288 is PAYLOAD split point (NOT 12280), no -8 adjustment
- [blockfetch-hfc-wire-format.md](blockfetch-hfc-wire-format.md) — MsgBlock=tag(24) bstr(stored_cbor), NOT array[hfc_index,tag24(body)]
- [msgrejecttx-wire-format.md](msgrejecttx-wire-format.md) — MsgRejectTx CBOR: mini-protocol envelope, all Conway predicate failure tags
- [lsq-result-encoding.md](lsq-result-encoding.md) — MsgResult wire format, HFC success wrapper, era mismatch encoding
- [query-version2-wire-format.md](query-version2-wire-format.md) — 3-level nesting Query→HFC→NS, EitherMismatch wrapping
- [era-history-wire-format.md](era-history-wire-format.md) — GetInterpreter/EraHistory query/response, Bound/EraParams/SafeZone
- [shelley-genesis-cbor.md](shelley-genesis-cbor.md) — GetGenesisConfig (tag 11): CompactGenesis array(15), no tag(30) on activeSlotsCoeff
- [pparams-cbor-encoding.md](pparams-cbor-encoding.md) — PParams array(31) / PParamsUpdate map encoding, field ordering
- [localtxmonitor-localtxsubmission-gentx-n2c-wire-format.md](localtxmonitor-localtxsubmission-gentx-n2c-wire-format.md) — MsgReplyNextTx=`82 06 82 06 D8 18 <tx>` (era-idx array + tag24 wrap at PER-ERA layer not HFC layer); MsgSubmitTx/MsgReplyNextTx byte-identical GenTx embedding; CardanoEras idx incl Dijkstra=7

## TxSubmission / Mempool
- [txsubmission2-architecture.md](txsubmission2-architecture.md) — V1/V2 server, outbound client, governor lifecycle, mempool sync
- [txsubmission2-protocol.md](txsubmission2-protocol.md) — protocol state machine details
- [txsubmission2-wire-format.md](txsubmission2-wire-format.md) — wire format specifics
- [mempool-tx-ordering.md](mempool-tx-ordering.md) — FIFO via TicketNo, virtual ledger state for chained txs, revalidation logic
- [mempool-revalidation-after-block.md](mempool-revalidation-after-block.md) — full revalidation via revalidateTxsFor/reapplyTxs, skips crypto
- [mempool-implementation.md](mempool-implementation.md) — mempool implementation details

## Storage
- [lsm-tree-architecture.md](lsm-tree-architecture.md) — lazy levelling merge, 4-file run format, bloom filters, NO WAL
- [ledgerdb-v2-diff-retention-and-snapshot-decoupling.md](ledgerdb-v2-diff-retention-and-snapshot-decoupling.md) — V1 DbChangelog DELETED from main; V2 LedgerSeq/StateRef holds FULL materialized table per volatile-window block (not diffs-to-reverse-apply); rollback=pure AnchoredSeq trim; disk-snapshot (implTryTakeSnapshot) never touches live ldbSeq TVar, fully decoupled from garbageCollect (k-driven pruning)

## Misc / Ledger Semantics
- [v1-txinfo-wdrl-encoding.md](v1-txinfo-wdrl-encoding.md) — V1 txInfoWdrl=List[Constr0[cred,amt]] (NOT Map, that's V2)
- [native-script-hash-original-bytes-not-reencode.md](native-script-hash-original-bytes-not-reencode.md) — hashScript=prefix<>originalBytes, never re-encode; Timelock decoder accepts indefinite arrays + non-minimal ints; prefix table native=0x00/V1-V3=0x01-03

## Plutus / UPLC Flat Wire Format
- [plutus-flat-wire-format-defaultfun.md](plutus-flat-wire-format-defaultfun.md) — DefaultFun wire-ID table (0-100, NOT decl order), constr/case v1.1.0 decode-gate, checkScope skips Constr/Case
- [flat-word64-natural-overflow-semantics.md](flat-word64-natural-overflow-semantics.md) — dWord64/lastStep REJECTS iff final chunk>1; Index+Constr-tag Word64 but Program Version is unbounded Natural
- [uplc-cek-case-scope-semantics.md](uplc-cek-case-scope-semantics.md) — checkScope skips Constr/Case (byte-exact risk); Case-on-VCon gated at vanRossemPV=PV11

## Test Vectors
- [test-vectors-reference.md](test-vectors-reference.md) — catalog across all Haskell repos: consensus golden (1620 CBOR files), plutus 999 UPLC tests
