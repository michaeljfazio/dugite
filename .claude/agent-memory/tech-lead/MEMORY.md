# Tech Lead Agent Memory

## Era Rules
- [Dijkstra era rules dispatch (#462)](issue-462-dijkstra-era-rules.md) — Conway alias removed; DijkstraRules delegates to Conway plus identity translateEraDijkstra

## Critical Invariants & Bug Patterns
- [ChainSync at_tip rollback stall](chainsync-at-tip-rollback-stall.md) — at_tip not reset on MsgRollBackward → pipeline freeze → bearer closed; fix: at_tip=false in MsgRollBackward arm (5abaf2687)
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
