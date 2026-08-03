# Cardano Haskell Oracle - Agent Memory

## Plutus V1/V2 ScriptContext / Conway Gating
- [v1v2-scriptcontext-conway-gates.md](v1v2-scriptcontext-conway-gates.md) — guardConwayFeaturesForPlutusV1V2+transPlutusPurposeV1V2 -> BadTranslation hard-rejects tx; Conway's transTxOutV1/transTxInInfoV1 drop 2/3 Babbage V1 restrictions, keep InlineDatumsNotSupported
- [nocostmodel-collecterror-native-script-exclusion.md](nocostmodel-collecterror-native-script-exclusion.md) — NoCostModel via `Map.lookup lang costModelsValid` per-script; native scripts never touch CostModels; merge order can hide NoCostModel behind NoRedeemer

## Plutus Data CBOR Wire Format (Datums/Redeemers, script_data_hash)
- [plutus-data-integer-cbor-bignum-threshold.md](plutus-data-integer-cbor-bignum-threshold.md) — Data::I plain-int vs tag2/3 threshold is EXACTLY `[-(2^64)..2^64-1]`; decode allows non-canonical bignum framing; dugite #952 truncated via `as u64` above 2^64, corrupting script_data_hash

## Plutus Builtins / Ledger-API
- [v2v1-paramname-vanrossem-extension-live.md](v2v1-paramname-vanrossem-extension-live.md) — V1/V2 ParamName not frozen at Babbage size; batch6 hits V1/V2 at PV11 (V1=332,V2=332,V3=350), live on preview
- [v3-paramname-vanrossem-tail-297-349.md](v3-paramname-vanrossem-tail-297-349.md) — exact V3 ParamName idx297-349 table (53 fields/14 builtins), ExpModInteger cost coefficients
- [plutus-builtin-availability-gate.md](plutus-builtin-availability-gate.md) — builtinsIntroducedIn batch1-6 table, PV consts (valentinePV=8 etc.), deserialiseScript rejection path
- [plutus-builtins-adversarial-audit.md](plutus-builtins-adversarial-audit.md) — BuiltinSemanticsVariant<->PV table, Trace deferred-unlifting, SliceByteString Int64 bound, secp256k1 zero-r/s & high-S, ExpMod 2^8191, DropList negative=no-op
- [constrdata-word64-gate-version-boundary.md](constrdata-word64-gate-version-boundary.md) — ConstrData Word64 gate landed in plutus 1.63.0.0, ONE DAY AFTER cn 11.0.1 shipped -> UNGATED in dugite's actual target; unConstrData never gated
- [ed25519-verify-strict-vs-libsodium-ref10.md](ed25519-verify-strict-vs-libsodium-ref10.md) — donna removed Feb-2025, unconditional libsodium all PV; dalek verify_strict matches except missing pk-canonical-encoding gate
- [bls12-381-multiscalarmul-semantics.md](bls12-381-multiscalarmul-semantics.md) — multiScalarMul=`zip` truncation, no length-mismatch error; dugite bls.rs skips elem_type check for empty lists
- [builtin-semantics-variant-costing.md](builtin-semantics-variant-costing.md) — Text char-count->byte/4 at PV11, divide/mod diagonal-shape flips at PV11 (quotient/remainder never flip), SatInt=Int64 saturating

## Consensus / Sync / Genesis
- [lop-historicity-chainsync.md](lop-historicity-chainsync.md) — LoP bucket (cap=100K,500tok/s,Syncing-only), HistoricityCheck cutoff=37h, csIdling GSM gate
- [praos-chain-order-v3-verified.md](praos-chain-order-v3-verified.md) — comparePraos/ChainOrder/VRFTiebreakerFlavor at the tag matching cn 11.0.1 (3.0.1.0); RestrictedVRFTiebreaker(5) Conway-only hardcoded. Supersedes loe-chain-selection.md (wrong tag)
- [chaindb-addblock-tracer-namespaces.md](chaindb-addblock-tracer-namespaces.md) — exact ChainDB.AddBlockEvent.*/Forge.Loop.* namespaces for cn 11.0.1; SwitchedToAFork=chain-switch marker; NO orphaned-block trace anywhere
- [gdd-governor-deep-dive.md](gdd-governor-deep-dive.md) — gddWatcher internals, all 4 densityDisconnect guards verbatim, genesis window=3k/f, CSJ jumper visibility (see also gdd-genesis-density-disconnector.md)
- [genesis-bootstrap-protocol.md](genesis-bootstrap-protocol.md) / [ouroboros-genesis-checkpoints.md](ouroboros-genesis-checkpoints.md) — bootstrap + checkpoint mechanisms
- [chainsync-at-tip.md](chainsync-at-tip.md) — connection stays open, MsgAwaitReply, pipeline lowMark=200/highMark=300
- [chainsync-intersection-vs-rollback-distinction.md](chainsync-intersection-vs-rollback-distinction.md) — MsgIntersectFound is NEVER a wire MsgRollBackward; RolledBackPastIntersection=graceful, InvalidIntersection=hard ban; mkOffsets fibonacci
- [fork-resolution-chainsel.md](fork-resolution-chainsel.md) / [fork-switching-mechanism.md](fork-switching-mechanism.md) — addBlock flow, Praos tiebreaker, rollback, tentative follower (naming fixed in praos-chain-order-v3-verified)
- [block-validation-modes.md](block-validation-modes.md) — tickThenReapply (replay, no crypto) vs tickThenApply (full validation)
- [pool-distr-leader-check.md](pool-distr-leader-check.md) — nesPd=ssStakeMarkPoolDistr(es0), memoized not recomputed mid-epoch; dugite bug used on-the-fly
- [genesis-initialization-ledger-state.md](genesis-initialization-ledger-state.md) — Byron->Shelley translation, genDelegs vs pool regs, preview init
- [blockfetch-concurrency-architecture.md](blockfetch-concurrency-architecture.md) — BulkSync=1 peer/Deadline=2 peers, 100 max in-flight/peer
- [chaindb-architecture.md](chaindb-architecture.md) — ChainDB internals
- [inbound-connection-rate-limiting.md](inbound-connection-rate-limiting.md) — AcceptedConnectionsLimit hard=512/soft=384/delay=5s, no per-IP
- [mithril-snapshot-ledger-init.md](mithril-snapshot-ledger-init.md) — two-archive system, ledger snapshot layout, tickThenReapply/Apply split
- [gsm-haa-syncing-presyncing-regression.md](gsm-haa-syncing-presyncing-regression.md) — Syncing->PreSyncing is HAA-only, no tip-age term; OutboundConnectionsState 4-way split; root-caused dugite genesis-sync freeze
- [haa-outbound-connections-state-verified.md](haa-outbound-connections-state-verified.md) — exact CHaP commits for cn 11.0.1 (cardano-diffusion 17525c3, ouroboros-network a98c885); Praos-vs-Genesis TooOld differs; PreSyncing doesn't pause ChainSync/BlockFetch

## Variable-Length CBOR Framing (encodeList/Set/Seq/Foldable)
- [variable-length-cbor-framing-and-blockbody-hash-over-original-bytes.md](variable-length-cbor-framing-and-blockbody-hash-over-original-bytes.md) — lengthThreshold=23 routing; OSet always-tag vs Set PV-gated; AlonzoBlockBody DecCBOR hashes ORIGINAL bytes, never a re-encode
- [witness-set-ord-instances-and-order-observability.md](witness-set-ord-instances-and-order-observability.md) — Ord WitVKey=blake2b224(vkey), BootstrapWitness=Byron addrRoot (different algo); decode checks no-duplicates only; fresh construction canonicalizes ascending, replay preserves wire order

## Ledger State Snapshots & CBOR Format
- [ext-ledger-state-snapshot-format.md](ext-ledger-state-snapshot-format.md) — state file CBOR: outer array(2)[1,ExtLedgerState], HeaderState telescope, PraosState 8-field array
- [ledger-state-11-0-1-format-changes.md](ledger-state-11-0-1-format-changes.md) — 10.6.2->11.0.1: spsAccountId array(2), Peras field
- [ledger-snapshot-cbor-format.md](ledger-snapshot-cbor-format.md) — HFC telescope, NewEpochState array(7), TxIn MemPack, V1 vs V2 backends
- [utxo-hd-snapshot-format.md](utxo-hd-snapshot-format.md) — UTxO-HD in-memory snapshot: EMPTY UTxO in state file, tables written separately
- [new-epoch-state-cbor.md](new-epoch-state-cbor.md) — NewEpochState CBOR field-by-field
- [cardano-ledger-types-wire-format.md](cardano-ledger-types-wire-format.md) — hash/key types, address header-byte table, MaryValue, script tags, PParams array(31), Rational tag(30)

## Governance (Conway / CIP-1694)
- [drep-dormant-epoch-expiry-exact-mechanism.md](drep-dormant-epoch-expiry-exact-mechanism.md) — numDormantEpochs is bump-at-submission (Certs.hs, NOT add-at-ratify-check-time); dRepAcceptedRatio is bare `reCurrentEpoch > drepExpiry`
- [drep-pulser-ratification.md](drep-pulser-ratification.md) — pulser snapshot at END of EPOCH rule, pulse spreads 4k blocks, RATIFY runs once at finishDRepPulser
- [conway-ratification-details.md](conway-ratification-details.md) — threshold functions, enactment priority, committee expiry, DRep activity, treasury cap; pvCanFollow(minor==cur+1), HFC prevGovActionId chains, ensTreasury includes this-boundary's RUPD
- [conway-gov-rule-deep-dive.md](conway-gov-rule-deep-dive.md) — all 19 ConwayGovPredFailure variants w/ CBOR tags, voter matrix
- [gov-bootstrap-restriction.md](gov-bootstrap-restriction.md) — isBootstrapAction allows ParameterChange/HardForkInitiation/InfoAction only
- [conway-gov-state-encoding-detailed.md](conway-gov-state-encoding-detailed.md) — GetGovState (tag 24): ConwayGovState array(7) field encoding
- [pparams-group-classification.md](pparams-group-classification.md) — PP group classification, threshold combination logic
- [conway-instant-stake-ptr-exclusion.md](conway-instant-stake-ptr-exclusion.md) — ConwayInstantStake has NO sisPtrStake, dropped via HFC `_other`
- [bounded-ratio-decode-bounds-and-enact-totality.md](bounded-ratio-decode-bounds-and-enact-totality.md) — UnitInterval decode rejects n>d; NonNegativeInterval no upper bound; ENACT is total field-merge, no re-validation

## Rewards / Epoch Transition
- [epoch-nonce-tickn-deep-dive.md](epoch-nonce-tickn-deep-dive.md) / [epoch-nonce-calculation.md](epoch-nonce-calculation.md) — eta0 formula, freeze window per era (3k/f thru Babbage, 4k/f Conway+), PraosState fields, per-block update
- [nonintegral-ln-algorithm.md](nonintegral-ln-algorithm.md) — ln' uses continued fraction NOT Taylor series; exact Rational vs f64 in dugite
- [reward-iteration-deep-dive.md](reward-iteration-deep-dive.md) — startStep iterates GO snapshot, genesis pool 2-epoch warm-up, 6 zero-reward cases
- [monetary-expansion-rupd.md](monetary-expansion-rupd.md) — deltaR1/eta/expectedBlocks, block counting, Conway d=0 simplify
- [rupd-timing-data-flow.md](rupd-timing-data-flow.md) — timing windows (sr=4k/f), NEWEPOCH ordering (applyRUpd BEFORE SNAP)
- [newepoch-ordering-details.md](newepoch-ordering-details.md) — exact NEWEPOCH order (applyRUpd->MIR->SNAP->POOLREAP->UPEC->record)
- [reward-update-accounting.md](reward-update-accounting.md) — deltaR/deltaT/deltaF formulas, conservation invariant, undistributed->reserve
- [epoch0-rupd-ssfee-semantics.md](epoch0-rupd-ssfee-semantics.md) — ssFee comes from SnapShots.ssFee (=0 at genesis), not utxosFees
- [babbage-conway-hf-ppup-order.md](babbage-conway-hf-ppup-order.md) — PPUP applied BEFORE translateEra; updateCostModels Map.union left-biased
- [mir-pot-transfer-semantics.md](mir-pot-transfer-semantics.md) — MIR two-phase accumulate/apply; combined-AND solvency; Conway deletes MIRCert entirely
- [conway-validation-rules.md](conway-validation-rules.md) — validation rules, predicate failures, reward formula, epoch transition order
- [apply-chain-tick-forge-mutations.md](apply-chain-tick-forge-mutations.md) — TICK/NEWEPOCH fields visible to forge: intra-epoch vs epoch-boundary

## TPraos Overlay Schedule vs Praos (no overlay check post-Babbage)
- [tpraos-overlay-vs-praos-no-overlay.md](tpraos-overlay-vs-praos-no-overlay.md) — OVERLAY/PRTCL STS is TPraos-only (Overlay.hs/Prtcl.hs); Praos.hs updateChainDepState = KES+VRF only, PraosValidationErr has 11 ctors none overlay-shaped; Praos LedgerView has no lvD/GenDelegs (TPraos's does, w/ verbatim "not present in Babbage" comment); HFEras.hs era->protocol table

## Forging / VRF / KES
- [vrf-input-construction.md](vrf-input-construction.md) / [vrf-leader-check.md](vrf-leader-check.md) — VRF seed (TPraos vs Praos, mkInputVRF); checkLeaderValue, taylorExpCmp, FixedPoint E34, certNat/certNatMax
- [block-forging-deep-dive.md](block-forging-deep-dive.md) — slot tick->leader check->tx selection->body hash->header->KES sign (see also block-forging-flow.md)
- [forge-pipeline-complete.md](forge-pipeline-complete.md) — forkBlockForging, HFC dispatch, cardanoProtocolVersion=11.0, VRF tiebreaker=5
- [forge-chaindb-interaction.md](forge-chaindb-interaction.md) / [kes-signing-verification.md](kes-signing-verification.md) — forge/ChainDB interaction; KES signing/verification
- [block-header-protocol-version.md](block-header-protocol-version.md) — cardanoProtocolVersion hardcoded per-release, ExperimentalHardForksEnabled->11.0

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
- [query-version2-wire-format.md](query-version2-wire-format.md) — 3-level nesting Query->HFC->NS, EitherMismatch wrapping
- [era-history-wire-format.md](era-history-wire-format.md) — GetInterpreter/EraHistory query/response, Bound/EraParams/SafeZone
- [shelley-genesis-cbor.md](shelley-genesis-cbor.md) — GetGenesisConfig (tag 11): CompactGenesis array(15), no tag(30) on activeSlotsCoeff
- [pparams-cbor-encoding.md](pparams-cbor-encoding.md) — PParams array(31) / PParamsUpdate map encoding, field ordering
- [localtxmonitor-localtxsubmission-gentx-n2c-wire-format.md](localtxmonitor-localtxsubmission-gentx-n2c-wire-format.md) — MsgReplyNextTx=`82 06 82 06 D8 18 <tx>` (era-idx array + tag24 at PER-ERA layer); CardanoEras idx incl Dijkstra=7
- [localtxmonitor-mshastx-gentxid-wire-format.md](localtxmonitor-mshastx-gentxid-wire-format.md) — MsgHasTx=`82 07 82 06 5820 <32B>` (NOT bare bstr; OneEraGenTxId=[era_idx,inner], NO tag24 — GenTxId is already a hash); MsgReplyHasTx=`[8,bool]`; DecoderFailure=real Exception->mux teardown, no error-reply message exists

## TxSubmission / Mempool
- [txsubmission2-architecture.md](txsubmission2-architecture.md) — V1/V2 server, outbound client, governor lifecycle, mempool sync
- [txsubmission2-protocol.md](txsubmission2-protocol.md) / [txsubmission2-wire-format.md](txsubmission2-wire-format.md) — state machine + wire format
- [mempool-tx-ordering.md](mempool-tx-ordering.md) — FIFO via TicketNo, virtual ledger state for chained txs
- [mempool-revalidation-after-block.md](mempool-revalidation-after-block.md) / [mempool-implementation.md](mempool-implementation.md) — full revalidation via revalidateTxsFor/reapplyTxs, skips crypto

## Storage
- [lsm-tree-architecture.md](lsm-tree-architecture.md) — lazy levelling merge, 4-file run format, bloom filters, NO WAL
- [ledgerdb-v2-diff-retention-and-snapshot-decoupling.md](ledgerdb-v2-diff-retention-and-snapshot-decoupling.md) — fetched from unpinned `main`; V2 LedgerSeq holds FULL materialized table per volatile-window block; rollback=pure AnchoredSeq trim; disk-snapshot decoupled from garbageCollect. V1-deleted claim CORRECTED by next entry.
- [ledgerdb-init-replay-rollback-anchor-mechanism-pinned.md](ledgerdb-init-replay-rollback-anchor-mechanism-pinned.md) — PINNED at oc 3.0.1.0 (matches cn 11.0.1): V1 still exists, V2 is default. Anchor is reassigned on EVERY replayed block (extend+pruneToImmTipOnly collapse), never "set once at startup". AnchoredSeq has NO hash-chain invariant (that's AnchoredFragment.valid); LedgerSeq coherence is construction-discipline not a checked invariant. Rollback/fork-switch = pure AnchoredSeq index, never disk re-derive.
- [immutabledb-validation-reconstruction.md](immutabledb-validation-reconstruction.md) — ValidateMostRecentChunk vs ValidateAllChunks; chunk files=sole truth, indices ALWAYS reconstructed on mismatch; tip=last validated block, no tip file
- [dblock-directory-locking.md](dblock-directory-locking.md) — DbLock.hs: `<db-path>/lock`, OS flock via ioFileLock, 2s timeout, runs AFTER checkDbMarker BEFORE ChainDB open

## Misc / Ledger Semantics
- [metadatum-codec-definite-indefinite-gates.md](metadatum-codec-definite-indefinite-gates.md) — Metadata.hs decode: TypeTag rejected, checkSizes>PV2, byte-chunk PV12 indef-leniency; encoder always raw encodeListLen/encodeMapLen
- [v1-txinfo-wdrl-encoding.md](v1-txinfo-wdrl-encoding.md) — V1 txInfoWdrl=List[Constr0[cred,amt]] (NOT Map, that's V2)
- [native-script-hash-original-bytes-not-reencode.md](native-script-hash-original-bytes-not-reencode.md) — hashScript=prefix<>originalBytes, never re-encode; prefix table native=0x00/V1-V3=0x01-03

## Plutus / UPLC Flat Wire Format
- [plutus-flat-wire-format-defaultfun.md](plutus-flat-wire-format-defaultfun.md) — DefaultFun wire-ID table (0-100, NOT decl order), constr/case v1.1.0 decode-gate
- [flat-word64-natural-overflow-semantics.md](flat-word64-natural-overflow-semantics.md) — dWord64/lastStep REJECTS iff final chunk>1; Index+Constr-tag Word64 but Program Version is unbounded Natural
- [uplc-cek-case-scope-semantics.md](uplc-cek-case-scope-semantics.md) — checkScope skips Constr/Case (byte-exact risk); Case-on-VCon gated at vanRossemPV=PV11

## Plutus V3 Authoring Toolchain (Plinth) / ScriptContext Fixtures
- [plutus-v3-authoring-toolchain-and-scriptcontext-fixtures.md](plutus-v3-authoring-toolchain-and-scriptcontext-fixtures.md) — plinth-template AuctionValidator.hs real example; V3 single-arg proof (V3.hs `[arg]` vs V1/V2 `[Common.Data]`); exact ScriptContext/TxInfo/ScriptInfo/ScriptPurpose Data field order+tags; serialiseCompiledCode->.plutus envelope (no cardano-api needed); cardano-node-tests real compiled v3 field-inspecting fixtures (witnessRedeemer/timeRange/constitution); plutus-ledger-api-testlib ScriptContextBuilder for byte-diff w/o a validator; cardano-ledger has NO plutus-tx-plugin dep (only plutus-ledger-api)

## Test Vectors
- [test-vectors-reference.md](test-vectors-reference.md) — catalog across all Haskell repos: consensus golden (1620 CBOR files), plutus 999 UPLC tests
