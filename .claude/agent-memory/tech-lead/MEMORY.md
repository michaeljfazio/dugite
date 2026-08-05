# Tech Lead Agent Memory

## Conway Phase-1 Transaction Validation Audit (2026-08-05)
- [Full Phase-1 audit](issue-1021-1022-1024-1026-1028-conway-phase1-audit.md) — #1021 ProposalCantFollow (GOV tag 10, zero Phase-1 impl) + #1022 TooManyCollateralInputs wrongly gated behind has_plutus_scripts + #1024 ConwayTxRefScriptsSizeTooBig (per-tx 200KiB cap) missing entirely (all 3 FIXED, RED-GREEN verified, 26 new tests); #1026 DisallowedProposalDuringBootstrap (PV9-only, unreachable today) + #1028 InvalidGuardrailsScriptHash None-vs-SNothing ambiguity (unreachable while all 3 genesis configs seed a guardrail) both FILED not fixed. CERTS/DELEG/POOL/GOVCERT and ~17/19 GOV predicates confirmed CORRECT (clean negatives, do not re-audit). Trap: adding ValidationError variants broke an exhaustive match invisible to `cargo build --workspace --lib` — only `--all-targets`/clippy catches it.

## Conway N2C LSQ + Mempool-Reject Audit (2026-08-05)
- [Full LSQ/mempool audit](issue-1018-1027-lsq-mempool-audit-2026-08-05.md) — #1018 tag-33 GetFuturePParams hardcoded + #1019 ensWithdrawals hardcoded empty (both FIXED); #1020 NextEpochChange/ensCommittee live-not-frozen, #1023 MIR/GenesisDeleg accept-where-Haskell-rejects (P1), #1025 residual generic ScriptFailed arms, #1027 `query ledger-state` LIVE-VERIFIED undecodable, zero test coverage (all FILED). Methodology: cli-parity.csv hash-history as a free vacuous-vs-real oracle; throwaway single-node devnet for cheap live round-trips.

## Conway Epoch/NEWEPOCH Pipeline Audit (2026-08-05)
- [Full NEWEPOCH audit](audit-conway-epoch-newepoch-pipeline-2026-08-05.md) — #1017 test-path committee-prune gap (fixed) + #1016 misattributed comment (fixed) + #1015 Babbage nonce formula wrongly folds extraEntropy (filed, dormant). Extensive clean negatives: reward floor chain, zero-block-pool gate, RATIFY ordering, POOLREAP, deposits/obligation. Version-pin discipline: master HEAD != cardano-node 11.0.1's actual pin — check this repo's own conformance-corpus SHA first.
## #1014 aux-data key set (2026-08-05)
- [AlonzoTxAuxData shared decoder + guardPlutus PV gate](issue-1014-auxdata-key5-shared-decoder-pv-gate.md) — ONE decoder across all eras, keys 2-5 individually PV-gated (5/7/9/12), not per-era key sets; ceiling model works today but is fragile to a future non-Plutus-gated key. Dijkstra key 5 deliberately capped at 4 (no `plutus_v4_scripts` field yet).

## Dijkstra Sub-Transaction Rules (2026-08-05)
- [#1011 SUBCERTS/SUBDELEG/SUBPOOL/SUBGOVCERT/SUBENTITIES landed](issue-1011-dijkstra-subcerts-subpool-subgovcert.md) — clone-then-mutate-or-discard pattern (imbl+Arc make CertSubState/GovSubState clones cheap) beats extracting the top-level validator; SUBGOV+mint stay guarded (too large for one issue)

## Ledger Review Batches (2026-07-06)
- [#804 GenesisKeyDelegation + MIR quorum](issue-804-genesisdeleg-mir-quorum.md) — SNAPSHOT v27->28; new `future_gen_delegs` field; MIR quorum broke exhaustive ValidationError match
- [#784 PPUP votedValue quorum](issue-784-ppup-voted-value-quorum.md) — 3 buggy distinct-proposer copies routed through unused `voted_future_pparams`; LATENT (no live-chain diff)
- [#796/#803 batch](issues-796-803-batch-fix.md) — signed delta_reserves i128 (SNAPSHOT v27); MIR apply panic->NoMirTransfer
- [#794/795/797/808/809/789/801 batch](issues-794-795-797-808-809-789-801-batch-fix.md) — block ExUnits IsValid filter, dup-tx-hash fatal, Byron fee burn at fork, collateral sign bugs
- [#799/800/802/812 Conway gov batch](issues-799-800-802-812-batch-fix.md) — ratify tie-break (SNAPSHOT v26), CC zero-threshold ordering, atomic PParams enactment
- [#805/806/807/813 robustness batch](issues-805-806-807-813-batch-fix.md) — UtxoStore crash-not-diverge on LSM errors, LedgerSeq/DiffSeq desync guard, pp_future PPUP diagnostic

## Live-Apply Rollback Investigations
- [LedgerSeq genesis-anchor overlay wedge (2026-08-03)](ledgerseq-genesis-anchor-overlay-wedge.md) — v2.5.0 quarantine boot leaves seq anchored at GENESIS (PV6/d=1); first fork switch installs chimera pparams via rollback_via_seq → TPraos overlay falsely rejects canonical Conway block → invalid_cache wedge. Overlay is TPraos-ONLY in Haskell (oracle-pinned).
- [DiffSeq clear vs hardened fallback (2026-07-08)](rollback-diffseq-clear-vs-caller-fallback-hardened.md) — real root cause = vestigial `diff_seq.clear()` in epoch.rs defeats already-k-bounded `push_bounded` window; node-level snapshot-reload fallback is ALREADY hardened (refutes naive "reloads latest snapshot" theory) — fix is to stop clearing, not to further harden the fallback

## UPLC CEK Machine
- [Flat wire ID vs cost table (#761)](uplc-builtin-flat-id-mismatch.md) — BLS G1/G2 + UPLC 1.1.0 builtin IDs mis-ordered; conformance (text-format) doesn't catch flat-ID bugs
- [PV-gates #819/#820/#824/#828 (2026-07-06)](uplc-root-cause-a-pv-gates-819-820-824-828.md) — SemanticsVariant threaded into cost layer; 999-corpus is E-only/no-PV, blind to PV<11; #828.5 ConstrData tag-overflow is a scoped known limitation
- [Flat-decode strictness #821/#822 (2026-07-06)](uplc-flat-decode-strictness-821-822.md) — builtin-availability + Constr/Case v1.1.0 gate as separate post-decode pass; unmasked latent #835 double-filler bug
- [BLS unlifting/hardening #816/#827/#839/#843 (2026-07-06)](uplc-bls-unlifting-and-hardening-816-827-839-843.md) — ByteString-as-element laxity, MSM empty-list elem_type, subgroup-recheck, final_verify UB fix
- [Kont-depth/scope-check/decode #817/#823/#836/#842 (2026-07-06)](uplc-kont-depth-scope-check-decode-source-842-836-817-823.md) — depth cap removed, eager checkScope; #836 proved ref scripts ARE CBOR-double-wrapped (flags latent dugite-ledger bug)
- [Perf/hygiene/testing #838/#840/#841/#845 (2026-07-06)](uplc-perf-hygiene-testing-838-840-841-845.md) — TxInfoCache (Rc-shared, kills per-redeemer O(n²)); UplcError::MachineError split from Internal (6 sites); #845 audit found fuzz targets already exist (verdict stale)

## Era Rules
- [Dijkstra era rules dispatch (#462)](issue-462-dijkstra-era-rules.md) — Conway alias removed; DijkstraRules delegates to Conway + identity translateEraDijkstra

## Validation Rules
- [#810 raw_cbor=None pre-Conway](issue-810-raw-cbor-none-pre-conway-reachability.md) — confirmed universal: pre-Conway output decode never KeepRaw-wraps; #810 re-encode fix neutralizes it
- DO NOT skip V1/V2 Propose/Vote/Guarding redeemers (REFUTED 2026-06-13) — Haskell REJECTS via `guardConwayFeaturesForPlutusV1V2`; real divergence was V3 ScriptContext bug #761
- [Conway DRep bootstrap delegatee check](conway-drep-bootstrap-phase-delegatee-check.md) — `DelegateeDRepNotRegisteredDELEG` skipped at PV9, fires only PV>=10
- [Datum witness native-script exemption](datum-native-script-false-positive.md) — `MissingDatumWitness` false positive on native-script inputs; guard on `version > 0`
- [Redeemer native-script exemption (#758)](issue-758-native-script-spend-redeemer.md) — Spend/Reward/Cert/Vote all gate on `script_versions.get(sh) > 0`
- [DuplicateInput false positive Babbage PV<9 (#759)](issue-759-babbage-duplicate-input.md) — gate on `pv >= 9`; Haskell silently dedups below that
- [VRF key uniqueness PV11 gate](vrf-key-uniqueness-pv11-gate.md) — must gate PV>=11 not PV>=9; epoch 523 mainnet divergence
- [Conway cert script-witness reqs](conway-cert-redeemer-witnessing.md) — 3 DRep gov-certs + ConwayStakeRegistration need a Cert redeemer
- [Conway PlutusV3 cost-model seeding](conway-plutus-v3-cost-model-seeding.md) — seed at Babbage→Conway HF AND post-snapshot guard; else budget-exhausted every V3 tx from ep507

## Genesis Mode
- [GSM PreSyncing Mithril stall (#757)](gsm-presyncing-mithril-stall.md) — `syncing_startup_threshold_secs` gate on Mithril tip age
- [#760-A cold-restart watchdog wedge](issue-760-genesis-watchdog-rotation.md) — unproductive-claim watchdog fires on legit parked dynamo; `!is_genesis_bulk_sync` guard fixes

## Live-Apply Wedge (#767)
- [Lens A static-cycle analysis](issue-767-live-apply-deadlock.md) — no true AB-BA cycle; best candidate = synchronous LSM stall holding ledger_state.write()
- [Slow-demotion cascade](issue-767-slow-demotion-cascade.md) — peer_failed(Slow) demotes without tearing down TCP mux → reconnect storm
- [Live-apply permanent wedge](issue-767-live-apply-wedge.md) — save_utxo_snapshot() w/o block_in_place pins tokio worker → cascade; fix at epoch.rs:504
- [Residual stall Lens C](issue-767-residual-stall-lens-c.md) — apply-lag-triggered cascade via fetched_blocks_rx backpressure; self-recovers
- [Residual fix adversarial review](issue-767-residual-stall-proposed-fix-review.md) — Fix1 confirmed correct; Fix2 won't compile (private fn); Fix3 targets wrong path

## Critical Invariants & Bug Patterns
- [#782 LedgerSeq delta allowlist audit](issue-782-ledgerseq-delta-allowlist-audit.md) — delta model missed 11 LedgerState fields; guard test forces future-field audit
- [Mempool Mined-cascade fix](mempool-mined-cascade-fix.md) — Mined parent must NOT cascade children
- [GOV apply-path prev_action_id bypass](gov-apply-path-prev-action-id-bypass.md) — both process_proposal AND process_governance_votes_and_proposals need validation updates
- [#609 snapshot version quarantine](issue-609-snapshot-version-quarantine.md) — fail-fast version guard + rename to quarantine file, don't retry
- [Forge connectivity gate (Bug C)](forge-connectivity-gate-bug-c.md) — forge-before-peers-connect → self-fork; AtomicBool + hot_peer_count gate
- [Live apply skips LedgerSeq delta (Bug B)](node-live-apply-no-ledgerseq-delta.md) — use apply_block_with_delta in apply_fetched_block + fork replay
- [ChainSync at_tip rollback stall](chainsync-at-tip-rollback-stall.md) — at_tip not reset on MsgRollBackward → pipeline freeze
- [ChainSync Origin intersection stall](chainsync-origin-intersection-fix.md) — Origin intersection blocks switch_chain; fix disconnect+reconnect
- [Fork snapshot stall cascade](fork-snapshot-stall-fix.md) — 6-bug cascade fixed (1ff9cbce)
- [Live-tip fork stall fix](node-fork-stall-fix.md) — TriggeredFork + MsgRollBackward + LSM lock; 3 commits
- [Cascade failure invariant](ledger-cascade-failure-invariant.md) — never hard-return on confirmed blocks; log+self-correct
- [Forge body size bug](forge-body-size-bug.md) — body_size miscalc + epoch nonce + KES expiry off-by-one
- [RUPD snapshot position fix](ledger-rupd-snapshot-fix.md) — use `set` snapshot not `go` in calculate_rewards()
- [Rollback UTxO store](ledger-rollback-utxo-store.md) — slow-path rollback must open fresh store from LSM snapshot
- [Output CBOR re-encode](crypto-output-cbor-reencode.md) — indefinite-length inline datum + legacy vs post-Alonzo detection
- [Deferred pointer stake](ledger-ptr-stake-deferred.md) — ptr_stake resolves at SNAP time not insertion; 603 epoch mismatches

## N2C Protocol Compliance
- [Conway PParams protocolVersion position](n2c-pparams-protover-position.md) — index 12, NOT 30 (prior note was the bug #434 fixed)
- [#434 gov-state decode failure](issue-434-gov-state-decode.md) — 3 stacked bugs: PParams order, vote-map dedup, OMap insertion order
- [Hash32 padding convention](n2c-hash32-padding.md) — 28→32 byte padding/truncation for N2C wire output
- [Credential type discrimination](n2c-credential-type-discrimination.md) — track KeyHash vs Script via HashSets
- [Committee state encoding bugs](n2c-committee-state-bugs.md) — open: wrong source map, hardcoded hot credential type

## N2N Protocol
- [#1003 NodePeerManager dead-code audit](issue-1003-peermanager-dead-code-audit.md) — oracle-verified: no BlockFetch-success reward upstream (deleted), inbound-maturity GC IS upstream pattern (wired), no unified PeerCategory (deleted), no IP-only conn lookup (deleted). LIB vs BIN dead-code ground-truth methodology.
- [ChainSync server direction bug](network-chainsync-direction-bug.md) — InitiatorAndResponder confusion; TxSubmission2 deadlock
- [Duplex connection architecture](network-duplex-connection.md) — Phase 1+2 done; pallas plexer semantics
- [Duplex Phase 3 integration](node-duplex-phase3.md) — into_pipelined() conversion; TxSubmission2 responder JoinHandle
- [ConnectionId tuple keying](connection-id-tuple-keying.md) — keyed by `(local, remote)`; SO_REUSEPORT unblocks co-located BP+relay

## Consensus
- [LoE enforcement](consensus-loe-enforcement.md) — flush_to_immutable_loe() gating + GSM integration
- [Forge pipeline depth](consensus-forge-pipeline-depth.md) — forge disabled during sync (pipeline_depth > 1)
- [Preview pool expected rates](consensus-preview-pool-rates.md) — SAND ~0.155 blocks/hour
- [Forge loop Haskell alignment](forge-loop-haskell-alignment.md) — MAX_FORGE_LAG_SLOTS removed; TraceNoLedgerView gate added

## Ledger
- [Reward formula validation](ledger-reward-formula-validation.md) — Koios cross-validation; 1-epoch RUPD timing diff vs Haskell
- [#438 Koios oracle decomposition](issue-438-koios-oracle-decomposition.md) — decompose pool_fees/deleg_rewards/account_reward_history
- [#438 formula cleared](issue-438-formula-cleared.md) — Haskell leaderRew byte-exact; bug is snapshot inflation
- [#438 Koios vs ssStake semantics](issue-438-koios-stake-vs-ssstake.md) — Koios active_stake is UTxO-only, Haskell adds reward balance
- [#438 static-audit complete](issue-438-static-audit-complete.md) — rollback/dual-RUPD theories eliminated; path forward is live-replay
- [#438 RESOLVED](issue-438-live-capture-findings.md) — undistributed=reward_pot−Σrewards was dropped; add to delta_treasury+reserves
- [#438 formula confirmed correct](issue-438-formula-confirmed-correct.md) — overshoot is 100% from excess reserves vs Haskell, not formula
- [Blueprint divergences](ledger-blueprint-divergences.md) — ref script fee ceiling/floor, totalRefScriptSize, chain-sel tiebreaker
- [DRep count fix](ledger-drep-count-fix.md) — use active_drep_count() not dreps.len()
- [Plutus test coverage](ledger-plutus-test-coverage.md) — is_valid=false UTxO, treasury Phase-1, per-redeemer V3 Unit tests
- [Mempool epoch revalidation](node-mempool-epoch-revalidation.md) — revalidate with new pparams after epoch transition

## CLI
- [Build-raw alias](cli-build-raw-alias.md) — transaction build-raw as alias for transaction build
- [UTxO --tx-in query](cli-utxo-txin-query.md) — GetUTxOByTxIn (tag 15) wire format
- [Stake address info](cli-stake-address-info.md) — server-side filtering via tag 10
- [P1 commands](cli-p1-commands.md) — calculate-min-fee, calculate-min-required-utxo, policyid, pool-params, slot-number, kes-period-info
- [#998 CIP-0094 poll commands NOT implemented](issue-998-cip94-poll-commands-removed.md) — cardano-cli deleted them May 2025 (PR #1178); closed not-planned. Follow-up #1006: CLI surface-enumeration gate.
- [#1006 CLI surface-parity gate built](issue-1006-cli-surface-parity-gate.md) — recursive `--help` walker; 2 parser bugs found by running it (empty-key bash assoc-array, ANSI/blank-line block terminator); 82 real gaps filed as #1008.
- [#1008 first implementation pass](issue-1008-cli-surface-parity-implementation.md) — 69/151→77/149; `hash` cmd group + version + drep metadata-hash (Plutus hash needs the CBOR bstr wrapper RETAINED, not stripped); alias-only renames invisible to walker (must make cardano-cli's name primary); walker verified NOT misclassifying positional alternatives (386/386 leaves checked).

## TUI
- [Layout polish](tui-layout-polish.md) — wide mode, kv_aligned patterns, Monokai theme, RTT bar

## Storage
- [LSM perf baselines](storage-lsm-perf-baselines.md) — mainnet-scale runtimes on M-series (1M insert ~25s)
- [Large tests feature](storage-large-tests-feature.md) — feature flag design, key/value sizing, deterministic PRNG
- [ImmutableDB stale fork repair](storage-immutabledb-fork-repair.md) — delete stale chunks + rewrite tip.meta to fix gap-bridge loop
- [Fork snapshot recovery](node-fork-snapshot-recovery.md) — volatile-range blind spot in is_snapshot_canonical; 3-layer fix

## Serialization
- [Serialization test coverage](crypto-serialization-tests.md) — 133 tests, public API patterns, PPU extraction

## Soak Test Findings (2026-03-27)
- [ChainSync log flood stall](soak-test-chainsync-log-flood.md) — inbound syncer floods 1.2M INFO logs/10min; downgrade to DEBUG
- [CLI tx build change output](cli-tx-build-change-output.md) — --change-address computes but doesn't append change output
