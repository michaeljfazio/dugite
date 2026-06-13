# Tech Lead Agent Memory

## Era Rules
- [Dijkstra era rules dispatch (#462)](issue-462-dijkstra-era-rules.md) — Conway alias removed; DijkstraRules delegates to Conway plus identity translateEraDijkstra

## Validation Rules
- DO NOT skip V1/V2 Propose/Vote/Guarding redeemers in phase-2 (REFUTED 2026-06-13). A prior note claimed they must be silently skipped; the cardano-ledger-oracle proved Haskell REJECTS a V1/V2 script in a tx with Conway-only fields via `guardConwayFeaturesForPlutusV1V2` → `BadTranslation(ProposalProceduresFieldNotSupported)`. The on-chain is_valid=true txs (51f495aa, b2a591ac) actually use a V3 guardrails script; the divergence is a V3 ScriptContext data-shape bug (#761), not a V1/V2-skip issue. Skipping would FALSE-ACCEPT txs Haskell rejects.
- [Conway DRep bootstrap phase delegatee check](conway-drep-bootstrap-phase-delegatee-check.md) — `DelegateeDRepNotRegisteredDELEG` SKIPPED at PV9 (bootstrap); only fires PV>=10; fix `mod.rs:3184` `>= 9` → `>= 10`; 26 mainnet PV9 divergences
- [Datum witness native-script exemption](datum-native-script-false-positive.md) — `MissingDatumWitness` false positives on native-script-locked inputs with DatumHash; Haskell's `getInputDataHashesTxBody` guards on `isSpendingPlutusScript`; fix: `version > 0` guard in `DatumHash` branch of `check_datum_witnesses`
- [Spend/Reward/Cert/Vote redeemer native-script exemption (#758)](issue-758-native-script-spend-redeemer.md) — All 4 purposes: native-script credentials silently excluded from `neededPlutusSet`; `check_extra_redeemers` also narrowed; `check_script_redeemers` + `check_extra_redeemers` both gate on `script_versions.get(sh) > 0`
- [DuplicateInput false positive Babbage PV<9 (#759)](issue-759-babbage-duplicate-input.md) — Rule 1b must gate on `pv >= 9`; Haskell `Set.fromList` silently dedups at PV<9; no BabbageUtxoPredFailure constructor for duplicates; fixture tx-5ca83e21.hex pins regression
- [VRF key uniqueness PV11 gate](vrf-key-uniqueness-pv11-gate.md) — `VRFKeyHashAlreadyRegistered` must gate at PV>=11 NOT PV>=9; Haskell `hardforkConwayDisallowDuplicatedVRFKeys = pvMajor > 10`; mod.rs:3468 `>= 9` → `>= 11`; epoch 523 mainnet divergence
- [Conway cert script-witness / Cert-redeemer reqs](conway-cert-redeemer-witnessing.md) — getScriptWitnessConwayTxCert: ALL 3 DRep gov-certs (RegDRep/UnRegDRep/UpdateDRep) symmetric + deposit-bearing ConwayStakeRegistration (tag 7) need a Cert redeemer; omitting them → FALSE ExtraRedeemer rejection (a6639ae520, 01718b8b88)
- [Conway PlutusV3 cost-model seeding](conway-plutus-v3-cost-model-seeding.md) — V3 cost model from conway-genesis.json must be seeded at the Babbage→Conway HF (on_era_transition) AND via a post-snapshot guard (pv>=9 && plutus_v3.is_none()); else ScriptDataHashMismatch + budget-exhausted on every V3 tx from ep507. shelley.rs wholesale cost-model replace is CORRECT (pre-Conway path) — do NOT merge it (00a1a3ac8b)

## Genesis Mode
- [GSM PreSyncing Mithril stall (#757)](gsm-presyncing-mithril-stall.md) — genesis PreSyncing LoE caps at k=2160; fix: `syncing_startup_threshold_secs` in GsmConfig (sgen×slot_length ≈ 36h); Mithril tip age < threshold → start Syncing
- [#760-A genesis cold-restart watchdog wedge](issue-760-genesis-watchdog-rotation.md) — unproductive-claim watchdog (connection_lifecycle.rs:2464) fires after 30s on legitimately-parked dynamo in genesis bulk sync; fix: `!is_genesis_bulk_sync` guard flips the condition; ChainSel-starvation rotation at line 2661 is correct and stays

## Critical Invariants & Bug Patterns
- [Mempool Mined-cascade fix (bbdcb67a1)](mempool-mined-cascade-fix.md) — Mined parent must NOT cascade children; outputs move to on-chain UTxO; early-return in remove_tx_inner for Mined reason; fixes 01h-tx-chain test
- [GOV apply-path prev_action_id bypass (1f1367a82)](gov-apply-path-prev-action-id-bypass.md) — process_governance_votes_and_proposals bypassed InvalidPrevGovActionId; stale prev_id admitted silently; ratification fails forever; BOTH process_proposal AND process_governance_votes_and_proposals must be updated for any validation change
- [Issue #609 snapshot version quarantine](issue-609-snapshot-version-quarantine.md) — SNAPSHOT_VERSION bump silently wiped ledger snapshot (cryptic bincode "tag for enum is not valid" → init_fresh_ledger → from-genesis re-sync); fix: fail-fast version guard + rename to `<name>.bin.vNN-unreadable` so quarantined file is preserved AND not retried; ChainDB untouched; bump checklist updated
- [Forge connectivity gate (Bug C)](forge-connectivity-gate-bug-c.md) — forge before peers connect → self-forged fork → Bug-A disconnect loop → permanent stall; fix: AtomicBool + hot_peer_count gate in try_forge_block_at; flag set after Bug-A guard in chainsync_client_task (9d30beaf2)
- [Live apply path skips LedgerSeq delta push (Bug B)](node-live-apply-no-ledgerseq-delta.md) — apply_fetched_block uses apply_block (no delta), LedgerSeq empty, fork rollback fails → clear_volatile → StoreButDontChange forever; fix: use apply_block_with_delta + push in apply_fetched_block and fork replay loop
- [ChainSync at_tip rollback stall](chainsync-at-tip-rollback-stall.md) — at_tip not reset on MsgRollBackward → pipeline freeze → bearer closed; fix: at_tip=false in MsgRollBackward arm (5abaf2687)
- [ChainSync Origin intersection stall](chainsync-origin-intersection-fix.md) — intersection=Origin with non-Origin local chain → VolatileDB switch_chain blocked (isReachable fails) → node stuck on self-forged fork; fix: disconnect+reconnect (5 lines after try_find_intersect call site)
- [Fork snapshot stall cascade](fork-snapshot-stall-fix.md) — 6-bug cascade: fork snapshot → bad intersection → deep rollback → UTxO corruption; all fixed (1ff9cbce)
- [Live-tip fork stall fix](node-fork-stall-fix.md) — TriggeredFork doesn't apply blocks + MsgRollBackward not propagated + LSM lock; 3 commits (85f1d53, 040cb13, c364c59)
- [Cascade failure invariant](ledger-cascade-failure-invariant.md) — Never hard-return on confirmed blocks; log+self-correct for ledger-state-divergence checks
- [Forge body size bug](forge-body-size-bug.md) — body_size miscalculation + epoch nonce not updated + KES expiry off-by-one
- [RUPD snapshot position fix](ledger-rupd-snapshot-fix.md) — Use `set` snapshot (not `go`) in calculate_rewards(); stale treasury diagnostics
- [Rollback UTxO store](ledger-rollback-utxo-store.md) — Slow-path rollback must open fresh store from LSM snapshot
- [Output CBOR re-encode](crypto-output-cbor-reencode.md) — Indefinite-length inline datum CBOR and legacy vs post-Alonzo detection
- [Deferred pointer stake (sisPtrStake)](ledger-ptr-stake-deferred.md) — ptr_stake field + StakeRouting enum; resolves at SNAP time not insertion; 603 epoch mismatches from epoch 647

## N2C Protocol Compliance
- [Conway PParams protocolVersion position](n2c-pparams-protover-position.md) — protocolVersion sits at index 12 (between tau and minPoolCost), NOT 30; the prior "index 30" entry was the inverted bug fixed by issue #434 (2026-05-12)
- [Issue #434 gov-state decode failure](issue-434-gov-state-decode.md) — 3 stacked bugs all surfacing as "0 active proposals": PParams positional order, vote-map dedup, OMap forest insertion order
- [Hash32 padding convention](n2c-hash32-padding.md) — 28→32 byte padding/truncation rules for N2C wire output
- [Credential type discrimination](n2c-credential-type-discrimination.md) — Track KeyHash vs Script via HashSets; DRep stores full Credential
- [Committee state encoding bugs](n2c-committee-state-bugs.md) — Open issues: wrong source map, hardcoded hot credential type

## N2N Protocol
- [ChainSync server direction bug](network-chainsync-direction-bug.md) — InitiatorAndResponder role confusion; TxSubmission2 deadlock (server sends MsgRequestTxIds first)
- [Duplex connection architecture](network-duplex-connection.md) — Phase 1+2 implementation; pallas plexer semantics; Phase 3 pending
- [Duplex Phase 3 integration](node-duplex-phase3.md) — into_pipelined() conversion; TxSubmission2 responder JoinHandle
- [ConnectionId tuple keying 2026-04-29](connection-id-tuple-keying.md) — connections keyed by `(local, remote)`; Overwritten simultaneous-open; `SO_REUSEPORT` listener; unblocks co-located BP+relay diffusion

## Consensus
- [LoE enforcement](consensus-loe-enforcement.md) — flush_to_immutable_loe() gating in block pipeline; GSM integration
- [Forge pipeline depth](consensus-forge-pipeline-depth.md) — Forge disabled during sync (pipeline_depth > 1); metric interpretation
- [Preview pool expected rates](consensus-preview-pool-rates.md) — SAND pool: ~0.155 blocks/hour, 1-block expected after 6.5+ hours at tip
- [Forge loop Haskell alignment](forge-loop-haskell-alignment.md) — MAX_FORGE_LAG_SLOTS=60 removed; TraceNoLedgerView gate (129600 slots) + full Haskell trace sequence added

## Ledger
- [Reward formula validation](ledger-reward-formula-validation.md) — Koios cross-validation methodology; 1-epoch RUPD timing difference vs Haskell
- [Issue 438 Koios oracle decomposition](issue-438-koios-oracle-decomposition.md) — pool_fees vs deleg_rewards vs account_reward_history; decompose properly for single-owner pools
- [Issue 438 formula cleared 2026-05-13](issue-438-formula-cleared.md) — synthetic test pins Haskell `leaderRew` byte-exact at owner_stake=511_912_077; bug is snapshot inflation (~22.98 ADA on owner)
- [Issue 438 Koios vs ssStake semantics 2026-05-13](issue-438-koios-stake-vs-ssstake.md) — Koios active_stake is UTxO-only; Haskell ssStake adds reward balance; 22.98 ADA gap is stale reward_accounts balance, requires per-epoch replay diff
- [Issue 438 static-audit complete 2026-05-13](issue-438-static-audit-complete.md) — Rollback-asymmetry + dual-RUPD theories structurally eliminated; commit 648d72484 pins invariants via 2 source-scan tests; path forward is live-replay (#471)
- [Issue 438 RESOLVED 2026-05-13](issue-438-live-capture-findings.md) — undistributed=reward_pot−Σrewards was dropped; fix: add to delta_treasury+delta_reserves; conservation identity holds; commits 2a14be2fe+30fd58db8; #479 re-enabled via a7591523b
- [Issue 438 formula confirmed correct 2026-05-14](issue-438-formula-confirmed-correct.md) — pool_reward formula is byte-exact given correct inputs; +25,066 overshoot is 100% from +4.887T lovelace excess reserves vs Haskell at epoch 1269; formula/split NOT the bug
- [Blueprint divergences](ledger-blueprint-divergences.md) — Ref script fee ceiling/floor, totalRefScriptSize check, chain selection tiebreaker
- [DRep count fix](ledger-drep-count-fix.md) — Use active_drep_count() not dreps.len()
- [Plutus test coverage](ledger-plutus-test-coverage.md) — is_valid=false UTxO, treasury Phase-1, per-redeemer V3 Unit tests
- [Mempool epoch revalidation](node-mempool-epoch-revalidation.md) — Revalidate mempool with new protocol params after epoch transition

## CLI
- [Build-raw alias](cli-build-raw-alias.md) — transaction build-raw as alias for transaction build
- [UTxO --tx-in query](cli-utxo-txin-query.md) — GetUTxOByTxIn (tag 15) wire format
- [Stake address info](cli-stake-address-info.md) — Server-side filtering via tag 10
- [P1 commands](cli-p1-commands.md) — calculate-min-fee, calculate-min-required-utxo, policyid, pool-params, slot-number, kes-period-info

## TUI
- [Layout polish](tui-layout-polish.md) — Wide mode, kv_aligned patterns, Monokai theme, RTT bar

## Storage
- [LSM perf baselines](storage-lsm-perf-baselines.md) — Mainnet-scale test runtimes on M-series (1M insert ~25s, total ~27.5s)
- [Large tests feature](storage-large-tests-feature.md) — Feature flag design, key/value sizing, deterministic PRNG
- [ImmutableDB stale fork repair](storage-immutabledb-fork-repair.md) — Delete stale chunk files + rewrite tip.meta (48-byte BE: slot/hash/block_no) to fix gap-bridge loop after fork flush
- [Fork snapshot recovery (2026-04-22)](node-fork-snapshot-recovery.md) — BP forged un-adopted block → volatile-range blind spot in is_snapshot_canonical + replay_from_lsm; 3-layer fix: 059c131+ff8f43e+abb370fe

## Serialization
- [Serialization test coverage](crypto-serialization-tests.md) — 133 tests, public API patterns, PPU extraction for integration tests

## Soak Test Findings (2026-03-27)
- [ChainSync log flood stall](soak-test-chainsync-log-flood.md) — Inbound Haskell syncer floods 1.2M INFO logs/10min → I/O stall; fix: downgrade to DEBUG
- [CLI tx build change output](cli-tx-build-change-output.md) — --change-address computes change but doesn't append change output to tx body
