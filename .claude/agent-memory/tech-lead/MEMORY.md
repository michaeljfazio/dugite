# Tech Lead Agent Memory

## #1071 nesRu wire arms — SNothing/Complete real, Pulsing deferred (2026-08-20)
- [Full writeup](issue-1071-nesru-wire-arms.md) — kept rupd_pulser_started/rupd_monetary UNTOUCHED (40+ tests), added parallel rupd_snapshot. RewardUpdate.rs is tag-258 Set at PV>=9, threshold-23 BOTH levels (oracle-verified, not analogy). Pulsing needs StakePoolSnapShot/StakeWithDelegation/Reward types dugite doesn't have anywhere — SNothing fallback, not fabricated.

## #1088 snapshot map-ordering fix (2026-08-20)
- [Full writeup](issue-1088-snapshot-map-ordering-fix.md) — 42 field decls (54 instances) moved HashMap/imbl→BTreeMap or new `*Wire` mirrors. Replaces `snapshot_one_bump_invariant.rs` w/ version-pinned hash guard. Disarming ONE 2-entry field is only ~50% likely to fail — RED-prove by reverting the whole fix.

## #1050/#1051 collateral + refinput wire arms (2026-08-06)
- [Fix](issue-1050-1051-collateral-refinput-wire-fixes.md) · [shakedown](issue-1050-1051-18-19-tx-zoo-shakedown.md) — InsufficientCollateral/CollateralHasTokens gained payloads; BabbageNonDisjointRefInputs de-Set-tagged (cardano-cli decode crash); 4 script-construction bugs; worktree wasn't exclusive.

## PoolRetirement + OutputTooSmall wire gaps (2026-08-06)
- [Fix](issue-pool-retirement-output-too-small-wire-gaps.md) — retirement used DELEG not POOL predicate; OutputTooSmall had zero encoder arm. #1025 pattern, 3rd confirmation.

## tx-zoo 18-plutus-edges, #1033 (2026-08-06)
- [Impl](issue-1033-plutus-edges-tx-zoo-category.md) — 12 scripts + edge-helper; dugite Rule 5 never checks collateral_return against minUTxO (expected live fail); several predicates confirmed already correct.

## Conway cert/decode/gov audits (2026-08-05)
- [Cert tags 5/6 reject](issue-1023-conway-cert-tags-5-6-decode-reject.md) — MIR/GenesisKeyDelegation hard-reject at Conway/Dijkstra decode; #1029 filed for Dijkstra tags 0/1.
- [Phase-1 audit](issue-1021-1022-1024-1026-1028-conway-phase1-audit.md) — #1021/#1022/#1024 fixed (26 tests); #1026/#1028 filed unreachable. Adding ValidationError variants breaks exhaustive match invisibly to `--lib` build.
- [LSQ/mempool audit](issue-1018-1027-lsq-mempool-audit-2026-08-05.md) — #1018/#1019 fixed; #1020/#1023/#1025/#1027 filed. cli-parity.csv hash-history is a free vacuous-vs-real oracle.
- [NEWEPOCH audit](audit-conway-epoch-newepoch-pipeline-2026-08-05.md) — #1017/#1016 fixed, #1015 filed dormant. Check THIS repo's conformance-corpus SHA, not master HEAD.
- [#1014 aux-data key set](issue-1014-auxdata-key5-shared-decoder-pv-gate.md) — one decoder, keys 2-5 individually PV-gated, not per-era sets.
- [#1011 Dijkstra sub-tx rules](issue-1011-dijkstra-subcerts-subpool-subgovcert.md) — clone-then-mutate-or-discard beats extracting the top-level validator.

## Ledger review batches (2026-07-06)
- [#804](issue-804-genesisdeleg-mir-quorum.md) SNAPSHOT v27→28, `future_gen_delegs` · [#784](issue-784-ppup-voted-value-quorum.md) LATENT quorum bug · [#796/#803](issues-796-803-batch-fix.md) signed delta_reserves i128 · [#794 batch](issues-794-795-797-808-809-789-801-batch-fix.md) IsValid filter, collateral signs · [#799 gov batch](issues-799-800-802-812-batch-fix.md) ratify tie-break v26 · [#805 robustness](issues-805-806-807-813-batch-fix.md) crash-not-diverge.

## Live-apply rollback investigations
- [LedgerSeq genesis-anchor overlay wedge](ledgerseq-genesis-anchor-overlay-wedge.md) — v2.5.0 quarantine boot leaves seq at GENESIS; overlay is TPraos-ONLY.
- [DiffSeq clear vs hardened fallback](rollback-diffseq-clear-vs-caller-fallback-hardened.md) — vestigial `diff_seq.clear()` defeats the k-bounded window.

## UPLC CEK machine (2026-07-06 unless noted)
- [Flat wire ID vs cost table, #761](uplc-builtin-flat-id-mismatch.md) — BLS G1/G2 + 1.1.0 IDs mis-ordered; text-format conformance is blind to this.
- [PV-gates #819/#820/#824/#828](uplc-root-cause-a-pv-gates-819-820-824-828.md) — corpus is E-only/no-PV, blind to PV<11.
- [Flat-decode strictness #821/#822](uplc-flat-decode-strictness-821-822.md) — builtin-availability as a post-decode pass; unmasked #835.
- [BLS unlifting/hardening #816/#827/#839/#843](uplc-bls-unlifting-and-hardening-816-827-839-843.md)
- [Kont-depth/scope/decode #817/#823/#836/#842](uplc-kont-depth-scope-check-decode-source-842-836-817-823.md) — #836 proved ref scripts ARE CBOR-double-wrapped.
- [Perf/hygiene/testing #838/#840/#841/#845](uplc-perf-hygiene-testing-838-840-841-845.md) — TxInfoCache kills per-redeemer O(n²).

## Era rules
- [Dijkstra dispatch, #462](issue-462-dijkstra-era-rules.md) — delegates to Conway + identity translateEraDijkstra.

## Validation rules
- [#810 raw_cbor=None pre-Conway](issue-810-raw-cbor-none-pre-conway-reachability.md) · DO NOT skip V1/V2 Propose/Vote redeemers (REFUTED — Haskell rejects via guardConwayFeaturesForPlutusV1V2).
- [DRep bootstrap delegatee](conway-drep-bootstrap-phase-delegatee-check.md) skip@PV9 · [Datum native-script exemption](datum-native-script-false-positive.md) · [Redeemer exemption #758](issue-758-native-script-spend-redeemer.md)
- [DuplicateInput PV<9, #759](issue-759-babbage-duplicate-input.md) · [VRF uniqueness PV11 gate](vrf-key-uniqueness-pv11-gate.md) · [Cert script-witness reqs](conway-cert-redeemer-witnessing.md) · [PlutusV3 cost-model seeding](conway-plutus-v3-cost-model-seeding.md)

## Genesis mode
- [GSM PreSyncing Mithril stall, #757](gsm-presyncing-mithril-stall.md) · [Cold-restart watchdog wedge, #760-A](issue-760-genesis-watchdog-rotation.md)

## Live-apply wedge, #767
- [Lens A](issue-767-live-apply-deadlock.md) no true AB-BA cycle · [Slow-demotion cascade](issue-767-slow-demotion-cascade.md) · [Permanent wedge](issue-767-live-apply-wedge.md) save_utxo_snapshot needs block_in_place · [Lens C](issue-767-residual-stall-lens-c.md) self-recovers · [Fix review](issue-767-residual-stall-proposed-fix-review.md)

## Critical invariants & bug patterns
- [#782 LedgerSeq delta allowlist audit](issue-782-ledgerseq-delta-allowlist-audit.md) — missed 11 fields; guard test forces future audit.
- [Mempool Mined-cascade](mempool-mined-cascade-fix.md) · [GOV prev_action_id bypass](gov-apply-path-prev-action-id-bypass.md) · [#609 snapshot quarantine](issue-609-snapshot-version-quarantine.md)
- [Forge connectivity gate, Bug C](forge-connectivity-gate-bug-c.md) · [Live-apply LedgerSeq delta, Bug B](node-live-apply-no-ledgerseq-delta.md)
- [ChainSync at_tip rollback stall](chainsync-at-tip-rollback-stall.md) · [Origin intersection stall](chainsync-origin-intersection-fix.md) · [Fork snapshot stall cascade](fork-snapshot-stall-fix.md) 6 bugs · [Live-tip fork stall](node-fork-stall-fix.md)
- [Cascade failure invariant](ledger-cascade-failure-invariant.md) never hard-return · [Forge body size bug](forge-body-size-bug.md) · [RUPD snapshot position](ledger-rupd-snapshot-fix.md) use `set` not `go`
- [Rollback UTxO store](ledger-rollback-utxo-store.md) · [Output CBOR re-encode](crypto-output-cbor-reencode.md) · [Deferred pointer stake](ledger-ptr-stake-deferred.md) resolves at SNAP not insertion

## N2C protocol compliance
- [Conway PParams protocolVersion position](n2c-pparams-protover-position.md) index 12 · [#434 gov-state decode, 3 bugs](issue-434-gov-state-decode.md)
- [Hash32 padding](n2c-hash32-padding.md) · [Credential type discrimination](n2c-credential-type-discrimination.md) · [Committee state encoding bugs, OPEN](n2c-committee-state-bugs.md)

## N2N protocol
- [#1003 PeerManager dead-code audit](issue-1003-peermanager-dead-code-audit.md) — LIB vs BIN ground-truth methodology.
- [ChainSync server direction bug](network-chainsync-direction-bug.md) · [Duplex architecture](network-duplex-connection.md) · [Duplex Phase 3](node-duplex-phase3.md) · [ConnectionId tuple keying](connection-id-tuple-keying.md)

## Consensus
- [LoE enforcement](consensus-loe-enforcement.md) · [Forge pipeline depth](consensus-forge-pipeline-depth.md) · [Preview pool rates](consensus-preview-pool-rates.md) · [Forge loop alignment](forge-loop-haskell-alignment.md)

## Ledger
- [Reward formula validation](ledger-reward-formula-validation.md) vs Koios · [#438 series](issue-438-live-capture-findings.md) RESOLVED — undistributed pot term was dropped (see also: oracle-decomposition, formula-cleared, stake-vs-ssstake, static-audit, formula-confirmed-correct topic files)
- [Blueprint divergences](ledger-blueprint-divergences.md) · [DRep count fix](ledger-drep-count-fix.md) use active_drep_count() · [Plutus test coverage](ledger-plutus-test-coverage.md) · [Mempool epoch revalidation](node-mempool-epoch-revalidation.md)

## CLI
- [Build-raw alias](cli-build-raw-alias.md) · [UTxO --tx-in](cli-utxo-txin-query.md) · [Stake address info](cli-stake-address-info.md) · [P1 commands](cli-p1-commands.md)
- [#998 CIP-0094 removed](issue-998-cip94-poll-commands-removed.md) not-planned · [#1006 surface-parity gate](issue-1006-cli-surface-parity-gate.md) 82 gaps→#1008 · [#1008 impl pass](issue-1008-cli-surface-parity-implementation.md) 69/151→77/149

## TUI / Storage / Serialization / Soak
- [TUI layout polish](tui-layout-polish.md) · [LSM perf baselines](storage-lsm-perf-baselines.md) · [Large tests feature](storage-large-tests-feature.md) · [ImmutableDB fork repair](storage-immutabledb-fork-repair.md) · [Fork snapshot recovery](node-fork-snapshot-recovery.md)
- [Serialization test coverage](crypto-serialization-tests.md) · [ChainSync log flood](soak-test-chainsync-log-flood.md) · [CLI tx build change output](cli-tx-build-change-output.md)
