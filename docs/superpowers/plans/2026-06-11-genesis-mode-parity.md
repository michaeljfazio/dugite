# Ouroboros Genesis Consensus Mode — Haskell Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every consensus-semantic change MUST be cross-checked against the quoted Haskell before commit (per repo feedback rule: never reason "I think Haskell does X").

**Goal:** Make `--consensus-mode genesis` a real Ouroboros Genesis implementation — LoE-constrained chain selection, fragment-based GDD, GSM with exact Haskell transitions, LoP leaky bucket, historicity check, live CSJ, genesis BlockFetch — with praos mode provably unchanged.

**Architecture:** A lossless per-peer shared-state registry (the Rust analogue of Haskell's per-peer `StrictTVar (ChainSyncState blk)`) feeds a GSM/GDD governor that computes an anchored **LoE fragment** (`sharedCandidatePrefix`) and exact `densityDisconnect` verdicts; the LoE fragment is published to `ChainSelQueue` (dugite-storage) via `arc_swap`, where `trimToLoE` gates adoption; CSJ drives live ChainSync clients via per-peer instruction channels; LoP/historicity attach inside `chainsync_client_task`. Every genesis path is behind `LoeState::Disabled`-style gates so praos compiles to today's behavior.

**Tech stack:** existing workspace (tokio, arc-swap already in tree). No new external deps.

**Authoritative references (quoted throughout):**
- ouroboros-consensus `release-ouroboros-consensus-3.0.1.0` (`c87aa760001e`) — pinned by cardano-node 11.0.1
- Oracle reference docs (full quotes + permalinks) saved at `/tmp/audit-{gsm,csj,bf}-oracle.md` and in the audit workflow journal:
  `~/.claude/projects/-Users-michaelfazio-Source-dugite/eb05b705-8bb8-495c-80e9-3efbb629905b/subagents/workflows/wf_08d43177-1e8/journal.jsonl`
- Finding catalog (78 findings, IDs cited per task): `/tmp/genesis-audit-findings.json`

**Praos-safety invariant (every task):** with `consensus_mode == "praos"`, the node must behave byte-identically to HEAD. Each task adds a praos-polarity test. The full suite + devnet-validate in BOTH modes gate the release.

**Honest scope note:** Step-level Rust code is authored at execution time against the quoted Haskell contracts in each task (the contracts below are the spec; the oracle docs hold the verbatim Haskell). Test cases are specified concretely per task and written FIRST (TDD).

---

## Findings → Task map (dedup)

| Task | Findings resolved |
|---|---|
| T0 config/params | blockfetch-04, blockfetch-05, blockfetch-03, gsm-07, gdd-04, loe-chainsel-08, blockfetch-06 (config half), N2C era-summaries genesis_window |
| T1 peer-state registry | gdd-01 (substrate), gdd-05, gdd-06, gdd-08, gdd-10, gsm-06, gsm-14, lop-03, lop-04, loe-chainsel-03 |
| T2 LoE fragment | loe-chainsel-02, loe-chainsel-04, gdd-01 |
| T3 GDD parity | gdd-02 (calc half), gdd-07, gdd-09, gdd-11, gdd-12, gsm-13 |
| T4 GDD kill | gdd-03, gsm-05, loe-chainsel-07 |
| T5 trimToLoE | gsm-01, loe-chainsel-01, loe-chainsel-05, loe-chainsel-06, gdd-02 |
| T6 GSM transitions | gsm-02, gsm-03, gsm-04, gsm-08, gsm-09, gsm-12, lop-06, blockfetch-07, blockfetch-08, blockfetch-02 (targets), gsm-15 |
| T7 LoP bucket | lop-01, csj-09 (bucket part) |
| T8 historicity | lop-02 |
| T9 CSJ | csj-01..csj-12, gsm-10, gdd-14 |
| T10 genesis BlockFetch | blockfetch-01, gsm-11, csj-10, blockfetch-12 (documented parity at defaults), blockfetch-13 |
| T11 checkpoints + reload + scrub | blockfetch-11, blockfetch-14, lop-05, blockfetch-09/10 (behavioral mapping documented) |
| T12 observability | gdd-15, loe-chainsel-11, blockfetch-15 |
| T13 integration tests | gdd-16, gsm-16, loe-chainsel-10, lop-07, csj-12, blockfetch-16 |

---

## Task 0: Genesis parameters & configuration surface

**Files:**
- Create: `crates/dugite-node/src/genesis_params.rs`
- Modify: `crates/dugite-node/src/config.rs` (ConsensusMode values, LowLevelGenesisOptions, MinBigLedgerPeersForTrustedState)
- Modify: `crates/dugite-node/src/node/mod.rs` (GsmConfig from genesis, EraHistory per-era genesis_window)
- Modify: `crates/dugite-consensus/src/era_history.rs` (per-era genesis_window: Byron `2k`, Shelley+ `ceil(3k/f)`)
- Test: same files' `#[cfg(test)]` modules

**Contract (Haskell):**
- `ConsensusMode` JSON values are `"Genesis"` / `"Praos"` (cardano-node `Cardano.Node.Types`). dugite must accept these; keep `"GenesisMode"`/`"PraosMode"` as documented legacy aliases (warn once).
- `LowLevelGenesisOptions` JSON object → `GenesisConfigFlags`: `gcfEnableCSJ=true`, `gcfEnableLoEAndGDD=true`, `gcfEnableLoP=true`, `gcfBlockFetchGracePeriod=10s`, `gcfBucketCapacity=100_000`, `gcfBucketRate=500/s`, `gcfCSJJumpSize=4320`, `gcfGDDRateLimit=1.0s` (defaults from `mkGenesisConfig`).
- `MinBigLedgerPeersForTrustedState` default `5`.
- sgen = `computeStabilityWindow k f = ceiling (3k / f)` (cardano-ledger `StabilityWindow.hs`) — NOT floor.
- HistoricityCutoff = `3k/f slots × slot_seconds + 3600` (mainnet: 133_200 s).
- Per-era `eraGenesisWin`: Byron `GenesisWindow (2*k)`, Shelley-based `GenesisWindow (ceil 3k/f)` (`shelleyEraParams`).
- Praos mode = `disableGenesisConfig`: every subsystem off.

**Steps:**
- [x] Failing tests: `genesis_params_from_shelley_genesis` (mainnet k=2160 f=0.05 → sgen=129600, cutoff=133200; preview k=432 → sgen=25920), `consensus_mode_json_accepts_genesis_and_praos{,_legacy_aliases}`, `low_level_genesis_options_defaults`, `era_history_genesis_window_per_era` (Byron 4320, Shelley 129600 for mainnet params).
- [x] Implement `GenesisParams { security_param_k, active_slot_coeff, sgen_slots, historicity_cutoff_secs, options: LowLevelGenesisOptions }` + config parsing + EraHistory per-era window.
- [x] `just clippy && cargo nextest run -p dugite-node -p dugite-consensus` green.
- [x] Commit `feat(genesis): network-derived genesis params + cardano-node config surface (T0)`.

## Task 1: Lossless per-peer chain-state registry (Haskell `ChainSyncClientHandle` analogue)

**Files:**
- Create: `crates/dugite-node/src/genesis_peer_state.rs`
- Modify: `crates/dugite-node/src/node/sync.rs` (chainsync_client_task writes), `crates/dugite-node/src/node/connection_lifecycle.rs` (registration plumbing), `crates/dugite-node/src/gsm.rs` (consume registry)

**Contract (Haskell `ChainSyncState`):** per-peer `{ csCandidate :: AnchoredFragment, csIdling :: Bool, csLatestSlot :: StrictMaybe (WithOrigin SlotNo) }` updated ATOMICALLY (TVar semantics — lossless): candidate appended per validated RollForward header; truncated per RollBackward; `csLatestSlot` updated BEFORE fragment extension (may exceed fragment when header beyond forecast horizon); `csIdling := true` on MsgAwaitReply, `:= false` on RollForward AND RollBackward.

**Rust shape:**
```rust
pub struct PeerChainState {
    pub fragment: Mutex<CandidateFragment>, // anchored (anchor_point, VecDeque<FragEntry{slot,hash,block_no}>)
    pub idling: AtomicBool,
    pub latest_slot: Mutex<Option<WithOrigin>>, // None until first header
}
pub struct PeerStateRegistry(RwLock<HashMap<SocketAddr, Arc<PeerChainState>>>);
```
Anchor = registration intersection. Re-anchor as immutable tip advances: if imm tip ∈ fragment → drop older + re-anchor at it; if anchor above imm tip or imm tip not a member → fragment "does not reach the immutable tip" (sharedCandidatePrefix treats as empty-at-imm-tip; Haskell CSJ-jumper case). Writers: chainsync_client_task (synchronous, no channel). GsmEvents stay as wakeup hints only. GSM `peer_info`/DensityWindow path replaced by registry reads.

**Steps:**
- [x] Failing tests: fragment append/truncate/rollback; idling cleared on rollback; latest_slot precedes fragment; re-anchoring on imm-tip advance incl. not-a-member case; registry register/deregister; dedup (same header twice doesn't double-count).
- [x] Implement registry; wire registration in connection_lifecycle → chainsync_client_task; write-points at the existing GsmEvent emission sites (events retained as hints).
- [x] Praos polarity: registry populated but consumed by nothing praos-side; `cargo nextest run -p dugite-node` green.
- [x] Commit `feat(genesis): lossless per-peer candidate fragments + idling registry (T1)`.

## Task 2: LoE fragment — `sharedCandidatePrefix`

**Files:**
- Create: `crates/dugite-node/src/loe.rs`
- Modify: `crates/dugite-node/src/gsm.rs` (actor computes + publishes)

**Contract (Haskell `Genesis.Governor.sharedCandidatePrefix` + `setGetLoEFragment`):**
- Per peer: split candidate at immutable tip; not reaching imm tip → empty fragment anchored at imm tip. LoE fragment = longest common prefix (slot+hash) of all per-peer suffixes, anchored at imm tip.
- Zero candidates → LoE fragment = current selection's volatile suffix (anchored imm tip) — "losing all peers lifts the constraint to k past selection".
- State mapping: PreSyncing → `Enabled(empty @ imm tip)`; Syncing → `Enabled(gdd fragment)`; CaughtUp → `Disabled`. Praos → `Disabled` always.

**Rust shape:**
```rust
pub enum LoeState { Disabled, Enabled { anchor: (u64,[u8;32]), tip: (u64,[u8;32]), members: Vec<(u64,[u8;32])>, k: u64 } }
// published via arc_swap::ArcSwap<LoeState>, shared with ChainSelQueue (T5)
```

**Steps:**
- [x] Failing tests: common-prefix of agreeing peers = min tip; divergent peers → prefix stops at divergence (slot+hash, same-slot different-hash divergence detected); peer not reaching imm tip → empty; zero peers → selection suffix; PreSyncing/CaughtUp/praos mappings.
- [x] Implement `shared_candidate_prefix(imm_tip, selection_suffix, peers) -> LoeFragment` + GSM actor publication (replaces scalar `loe_slot`; keep `loe_slot` field as `tip.0` for metrics compat).
- [x] Commit `feat(genesis): anchored LoE fragment via sharedCandidatePrefix (T2)`.

## Task 3: GDD `densityDisconnect` exact port

**Files:**
- Modify: `crates/dugite-node/src/gsm.rs` (replace `gdd_evaluate`), `crates/dugite-consensus/src/chain_selection.rs` (retire DensityWindow from live path)

**Contract (Haskell `Genesis.Governor.densityDisconnect`, quoted in journal/gdd-oracle):**
- Window: `firstSlotAfterGenesisWindow = succWithOrigin (AF.headSlot loeFrag) + sgen`; sgen per-era via EraHistory at slot `loeHead+1` from the IMMUTABLE ledger; `PastHorizon → skip whole evaluation`.
- Per peer (suffix = candidate after LoE intersection... per oracle: after imm tip, clipped to window): Gate 0: `csLatestSlot == None → excluded`. `clippedFragment = splitAtSlot firstSlotAfter… candidateSuffix`; `hasBlockAfter = max(headSlot candidateSuffix, latestSlot) >= firstSlotAfter`; `potentialSlots = hasBlockAfter ? 0 : firstSlotAfter − succWithOrigin(headSlot clipped)`; `lb = len clipped`; `ub = lb + potentialSlots`; `offersMoreThanK = len candidateSuffix > k` (FULL suffix).
- Guards for disconnecting peer0 given peer1: (1) `idling0 || not(null frag0) || hasBlockAfter0`; (2) `lastPoint frag0 /= lastPoint frag1` (slot+hash); (3) `offersMoreThanK1 || lb0 == ub0`; (4) `lb1 >= (idling0 ? lb0 : ub0)`. Dedup (`nubOrd`).
- Trigger: event-driven on any change to `{peer → (csLatestSlot, csIdling)}` fingerprint; Syncing only; rate limit = 1s sleep AFTER evaluation; also runs once at startup; on CaughtUp → trigger chain-sel reprocess (T5).
- LoE fragment updated atomically with the verdicts; `triggerChainSelectionAsync` when LoE tip hash changed.

**Steps:**
- [x] Failing tests: a port of each upstream guard scenario — no-signal skip; same-lastPoint skip (incl. same-slot different-hash NOT skipped); guard-3 both branches; guard-4 idling vs not; Gate-0 exclusion; offersMoreThanK full-suffix; window from LoE head with per-era sgen; PastHorizon skip; dedup.
- [x] Implement `densityDisconnect` over registry snapshots; fingerprint-dirty wakeup + 1s post-eval sleep in actor.
- [x] Commit `feat(genesis): exact densityDisconnect over candidate fragments (T3)`.

## Task 4: GDD kill = real disconnect

**Files:**
- Modify: `crates/dugite-node/src/node/mod.rs` (GddAction consumer), `crates/dugite-node/src/node/connection_lifecycle.rs`

**Contract:** Haskell `cschGDDKill = throwTo tid DensityTooLow` → ChainSync client dies → handle removed → connection torn down; peer is not instantly re-promotable (normal governor cool-down applies).

**Steps:**
- [x] Failing test (integration-style with test lifecycle manager): GddAction::DisconnectPeer cancels the peer's protocol tasks, closes the connection, removes candidate_chains + registry entries, records a failure (governor cooldown), and the GSM sees PeerDisconnected exactly once.
- [x] Implement: route GddAction through `ConnectionLifecycleManager::demote_to_cold`-equivalent teardown (the same path governor demotion uses), not bare `pm.peer_disconnected`.
- [x] Commit `fix(genesis): GDD disconnect tears down the connection (DensityTooLow parity) (T4)`.

## Task 5: trimToLoE in live chain selection + reprocess

**Files:**
- Modify: `crates/dugite-storage/src/chain_sel_queue.rs`, `crates/dugite-storage/src/chain_db.rs`, `crates/dugite-storage/src/volatile_db.rs` (ancestry walk helper)
- Modify: `crates/dugite-node/src/node/mod.rs` (inject ArcSwap<LoeState>, reprocess kick, startup k-cap)

**Contract (Haskell `ChainSel.trimToLoE` + `sanitizeLoEFrag` + reprocess):**
- Candidate trimmed to: if LoE tip on candidate → `candPrefix ++ takeOldest k candSuffix` (≤ k blocks past LoE tip); else (candidate diverges from LoE before its tip) → `candPrefix` (nothing past divergence).
- Empty LoE fragment anchored at imm tip → ≤ k blocks past imm tip (PreSyncing is NOT a total freeze).
- Stale fragment not containing current imm tip → sanitize to empty@imm tip.
- Flush/GC NEVER gated. Initial chain selection k-capped iff LoE enabled (`maximalCandidates limit = Just k`) — NOTE dugite wrinkle: `retain_blocks=10000 > k` means volatile holds >k; cap candidates, not retention.
- `ChainSelReprocessLoEBlocks`: when GDD advances LoE tip (head hash change) or GSM hits CaughtUp → re-run selection over volatile successors so deferred blocks get adopted.
- `LoEDisabled → identity` (praos fast path).

**dugite integration:** `process_add_block` consults `LoeState`: for `AddedAsTip` candidates, gate extension at `depth_past_loe ≤ k` (depth via ancestry walk vs `members`/tip, cached per selection tip); blocks beyond → `StoredAsFork` (stay volatile, no ledger apply). For `switch_to_fork` candidates, trim the fork point list before preference comparison; a fork trimmed to ≤ intersection is not adopted. New `ChainSelMessage::ReprocessLoE` re-evaluates fork tips without a new block.

**Steps:**
- [x] Failing tests (chain_sel_queue unit + storage integration): praos Disabled = byte-identical decisions (golden test vs HEAD behavior); extension within k of LoE tip adopted; extension k+1 past LoE tip stored-not-adopted then adopted after LoE advance + ReprocessLoE; divergent candidate trimmed to common prefix (no switch); empty-LoE@imm-tip allows exactly k; stale-LoE sanitize; startup k-cap genesis-only.
- [x] Implement LoeState injection + trim + reprocess + startup cap.
- [x] Commit `feat(genesis): trimToLoE enforcement in chain selection + LoE reprocess (T5)`.

## Task 6: GSM transition parity

**Files:**
- Modify: `crates/dugite-node/src/gsm.rs`, `crates/dugite-node/src/node/mod.rs` (SyncStatus emitter replacement), `crates/dugite-node/src/node/networking.rs` (hot-BLP count), config wiring from T0

**Contract (Haskell GSM.hs, quoted in /tmp/audit-gsm-oracle.md):**
- HAA: `isHaaSatisfied ⇔ active (HOT) big-ledger peers ≥ MinBigLedgerPeersForTrustedState(5)` (GenesisMode arm of `outboundConnectionsState`).
- PreSyncing→Syncing: HAA true. Syncing→PreSyncing: HAA false.
- Syncing→CaughtUp (`blockUntilCaughtUp`, atomic): `not (Map.null states) && all csIdling` AND `∀ candidate: preferAnchoredCandidate selection candidate == False` (no candidate better than selection). NO tip-age conjunct, NO within-window conjunct, NO global idle heuristic. On entry: write state, touch marker, sleep `minCaughtUpDuration` (= maxCaughtUpAge = 20 min) unconditionally.
- CaughtUp→PreSyncing: `durationUntilTooOld(selection tip slot)`: slot→wallclock via EraHistory (`PastHorizon → Already`); fires when `now − slot_time > maxCaughtUpAge`, plus jitter `uniform[0,300]s`; selection change (head point) resets the timer. On exit: delete marker.
- Startup: marker present → validate tip age (`Already → delete marker, PreSyncing`); marker absent → PreSyncing. Marker path: `<db>/gsm/CaughtUpMarker` (migrate: also honor+remove legacy `caught_up.marker`).
- `writeGsmState` notifies per-peer machinery (LoP bucket reconfig T7, fetch mode T10) — implement as a `watch::Sender<GsmState>` consumed by those tasks.
- Sync peer targets: while genesis-mode AND state ∈ {PreSyncing,Syncing} the governor uses sync targets (config `SyncTargetNumberOf*`, defaults per `defaultSyncTargets`: known 150, established 10, active 5, knownBLP 100, establishedBLP 40, activeBLP 30, root 0); CaughtUp/praos → deadline targets (today's).
- Candidate-better-than-selection: reuse `dugite_consensus::ChainSelection::prefer_chain_with_headers` against per-peer fragment tip (block_no + tiebreakers) — the same comparator chain selection uses (Haskell uses the same `preferAnchoredCandidate`).

**Steps:**
- [x] Failing tests: CaughtUp entry requires nonempty+all-idle+no-better-candidate (each conjunct's polarity); entry writes marker then dwell; exit on stale tip with jitter + reset-on-selection-change; startup marker staleness table (4 rows from oracle §3); HAA hot-only; sync-target switching on state; praos: evaluate() inert, no marker IO.
- [x] Implement; delete dead conjuncts (`all_chainsync_idle`, `all_peers_within_window`, tip-age entry gate) and the broken `chainsync_idle_secs` metric plumbing (recompute live at read site for display only).
- [x] Commit `fix(genesis): exact GSM transitions (blockUntilCaughtUp / durationUntilTooOld / marker staleness) (T6)`.

## Task 7: LoP leaky bucket

**Files:**
- Create: `crates/dugite-node/src/leaky_bucket.rs`
- Modify: `crates/dugite-node/src/node/sync.rs` (chainsync client integration)

**Contract (Haskell `Util/LeakyBucket.hs` + Client.hs, quoted in journal):**
- Config per GSM state: Syncing+enabled → `{capacity=100_000, rate=500/s, fillOnOverflow=true, onEmpty=disconnect}`; PreSyncing/CaughtUp/praos → dummy (never fires). Refill to capacity on state-driven reconfig.
- Pause on MsgAwaitReply; resume on RollForward AND RollBackward. Token grant (+1, capped) only when `blockNo hdr > kBestBlockNo` (then update kBestBlockNo). Empty → `EmptyBucket` → disconnect peer.
- CSJ: paused while a jumper awaits instructions (T9).

**Steps:**
- [x] Failing tests: drain math (tokio time): capacity/rate → empty at 200s without grants; grant only on strictly-advancing block_no; pause stops drain; resume restarts; overflow caps; reconfig refills; PreSyncing/CaughtUp/praos inert; EmptyBucket disconnects (harness).
- [x] Implement bucket (interval-free: compute level lazily from elapsed time; single deadline task) + wire into chainsync_client_task per-message arms + GSM watch reconfig.
- [x] Commit `feat(genesis): Limit on Patience leaky bucket in ChainSync client (T7)`.

## Task 8: Historicity check

**Files:**
- Create: `crates/dugite-node/src/historicity.rs`
- Modify: `crates/dugite-node/src/node/sync.rs`

**Contract (Haskell `HistoricityCheck.hs`):** judge on (a) MsgRollBackward — `HeaderStateWithTime` of the OLDEST rolled-back header (depth-0 rollbacks never historical), (b) MsgAwaitReply — candidate tip. Reject (disconnect) when `arrival_wallclock − slot_wallclock(point) > cutoff` (133_200s mainnet, derived per network from T0). Applies in PreSyncing+Syncing; CaughtUp/praos → no check.

**Steps:**
- [x] Failing tests: stale rollback rejected; fresh rollback ok; depth-0 ok; stale AwaitReply (candidate tip old) rejected during Syncing; CaughtUp exempt; praos exempt; slot→wallclock via EraHistory incl. PastHorizon behavior (treat as not-historical? — Haskell: query failure cannot occur for already-validated headers; use slot_to_wallclock Ok-path, log+pass on Err).
- [x] Implement + wire.
- [x] Commit `feat(genesis): historicity check on RollBackward/AwaitReply (T8)`.

## Task 9: CSJ — real ChainSync Jumping

**Files:**
- Rewrite: `crates/dugite-node/src/csj_orchestrator.rs` → `crates/dugite-node/src/csj.rs` (registry-integrated, per /tmp/audit-csj-oracle.md state table §20)
- Modify: `crates/dugite-network/src/protocol/chainsync/jumping.rs` (align pure types with Haskell states incl. `FreshJumper/StartedJumper`, `DynamoStarting/Started`, `Disengaging/DisengagedDone`)
- Modify: `crates/dugite-node/src/node/sync.rs` (client hooks: nextInstruction gate, offerJump, onRollForward/Backward/AwaitReply hooks, updateJumpInfo)
- Modify: `crates/dugite-node/src/node/connection_lifecycle.rs` (register/unregister)
- Delete: dead `GenesisFetchCoordinator` (replaced by T10), fabricated-name constants

**Contract highlights (full spec in oracle doc):**
- Roles per Haskell: ordered handle collection (Map + VecDeque; rotate = move-to-back); first non-disengaged = dynamo; registration in CaughtUp → DisengagedDone; jumpSize=4320 default; jump trigger `slot > lastJumpSlot + jumpSize` evaluated on dynamo RollForward BEFORE validation; jump payload = dynamo `JumpInfo` (fragment snapshot); only Happy jumpers receive jumps; accepted `JumpTo` updates jumper csCandidate+csLatestSlot (GDD visibility!); rejected → bisection `dropNewest(len/2)` loop, base case ≤1 → objector election (oldest badPoint slot wins; at most one; rest queue as FoundIntersection); `JumpToGoodPoint` for dynamo/objector promotion (updates client kis); disengage table: AwaitReply (all roles), dynamo rollback < lastJumpSlot, objector rollback < badPoint, objector RollForward at badPoint; `Disengaging → Restart` (client re-runs FindIntersect), `DisengagedDone → RunNormally`; unregister → backfillDynamo (prefer Started objector) / electNewObjector; rotateDynamo (from T10 starvation): old dynamo → Happy FreshJumper, moved to back, others reset Happy FreshJumper; wire jump = `MsgFindIntersect [single point]`, IntersectFound at any other point → InvalidJumpResponse (disconnect); jumpers block awaiting instructions with LoP paused; CSJ never re-engages after CaughtUp.
- Praos / `gcfEnableCSJ=false` → `noJumping` (all hooks no-op, RunNormally) — zero behavior change.

**Steps:**
- [x] Failing tests, three layers: (1) pure state-machine table tests mirroring oracle §20 rows; (2) orchestrator-level: registration order → dynamo election; jump fan-out only to Happy; bisection sequence on scripted Accept/Reject; objector election oldest-wins incl. demote-requeue; rotate/backfill; disengage table; (3) wire-level harness (scripted ChainSync messages through a MuxChannel pair — extend the `make_chainsync_task` test harness): jumper sends exactly one MsgFindIntersect per instruction and no MsgRequestNext while Happy; dynamo serves normally; promotion round-trip; InvalidJumpResponse disconnect.
- [x] Implement (replace placeholder semantics; delete slot-estimate "GDD verdict" — objector-vs-dynamo is resolved by real GDD (T3) over their now-real fragments; the LoP/objection "gate" in gsm.rs is renamed `csj_objections` and kept ONLY as diagnostics).
- [x] Praos polarity test: csj disabled → chainsync byte-stream identical to HEAD harness recording.
- [x] Commit `feat(genesis): live ChainSync Jumping with Haskell-parity roles/bisection/rotation (T9)`.

## Task 10: Genesis BlockFetch (PeersOrder + starvation rotation)

**Files:**
- Modify: `crates/dugite-node/src/node/connection_lifecycle.rs` (active-fetcher selection becomes PeersOrder-aware in genesis), `crates/dugite-node/src/node/mod.rs` (ChainSelStarvation signal from apply loop)
- Delete: `GenesisFetchCoordinator` + `CSJ_REPROCESS_LOE_DELAY_SECS` (decision.rs)

**Contract (Haskell `Decision/Genesis.hs`, /tmp/audit-bf-oracle.md):**
- FetchMode: GenesisMode ∧ GSM ∈ {PreSyncing,Syncing} → GenesisFetchMode; CaughtUp → Deadline (= today's behavior).
- `PeersOrder { current, start, all: VecDeque }`; per decision round: if `last_starvation_time ≥ start + grace(10s)` → demote current (push to back) + `rotateDynamo(current)` (T9); choose the FIRST peer in order whose candidate contains the next needed block; single peer fetches (dugite's global active_fetcher already enforces concurrency 1 — keep); decision cadence 40ms in genesis (vs 10ms praos).
- `ChainSelStarvation`: Ongoing when the apply/selection loop is idle waiting for blocks; EndedAt(t) when a block arrives. Maintained by the fetched-block consumer.
- Praos: byte-identical to HEAD (10ms, current selection logic, no PeersOrder).

**Steps:**
- [x] Failing tests: starvation→rotation after grace (tokio time); rotation calls CSJ demote hook; peer order round-robin on repeated starvation; first-in-order-with-block selection; CaughtUp/praos paths unchanged (polarity).
- [x] Implement: starvation tracker + PeersOrder in lifecycle fetch claim path, gated on genesis mode + GSM watch.
- [x] Commit `feat(genesis): starvation-rotated single-peer bulk fetch (PeersOrder) (T10)`.

## Task 11: Checkpoints, SIGHUP, naming scrub, diffusion mapping notes

**Files:**
- Modify: `crates/dugite-node/src/config.rs` (+`CheckpointsFile`/`CheckpointsFileHash`), `crates/dugite-consensus` header validation (checkpoint map), `crates/dugite-node/src/config_reload.rs`
- Docs: `docs/src/` genesis page update

**Contract:** `validateIfCheckpoint`: if `blockNo ∈ map` and `headerHash ≠ checkpoint` → header invalid (disconnect), after number/slot envelope checks; file = `{"checkpoints":[{"blockNo":N,"hash":hex}]}`, optional Blake2b-256 file-hash pin; applies to ALL headers, both modes (cardano-node semantics), loaded once at startup. SIGHUP: warn on changed-but-static genesis fields. Scrub fabricated identifiers (`csjReprocessLoEDelay` etc.) and the false-LoP comments. Document the deliberate diffusion mapping: dugite governor implements Haskell's sync-vs-deadline target switching (T6) and genesis churn ≈ ChurnDefault; bootstrap-peer `UseBootstrapPeers` trust machinery is a Praos-era mechanism deprecated by Genesis and intentionally not implemented (bootstrapPeers entries remain ordinary roots).

**Steps:**
- [x] Failing tests: checkpoint match passes, mismatch rejects header + disconnects, absent blockNo silent; file-hash pin mismatch → startup error; duplicate blockNo → parse error; SIGHUP warns on sync-target changes.
- [x] Implement + scrub + docs.
- [x] Commit `feat(node): CheckpointsFile enforcement + genesis config reload coverage + naming scrub (T11)`.

## Task 12: Observability

**Files:** `crates/dugite-node/src/metrics.rs`, `crates/dugite-monitor/src/widgets/header_bar.rs`

- [x] Gauges/counters: `dugite_gsm_state` (0/1/2), `dugite_loe_tip_slot`, `dugite_loe_depth_past` , `dugite_gdd_disconnects_total`, `dugite_csj_role{dynamo,objector,jumpers,disengaged}`, `dugite_lop_bucket_level_min`, `dugite_blockfetch_rotations_total`; monitor header shows GSM state in genesis mode. Tests: metrics exported + updated on transitions.
- [x] Commit `feat(genesis): GSM/LoE/GDD/CSJ/LoP observability (T12)`.

## Task 13: Adversarial integration suite + praos regression suite

**Files:** `crates/dugite-node/tests/genesis_adversarial.rs` (new), extend `tests/csj_adversarial.rs` harness

Scenarios (each asserted end-to-end through the wire-level harness):
- [x] Sparse-chain eclipse: 2 peers, one dense one sparse fork → GDD disconnects sparse, selection follows dense, LoE never exceeded by k.
- [x] Stall: peer trickles headers slower than LoP rate in Syncing → disconnected ~capacity/rate; same peer at CaughtUp → kept.
- [x] Historic rollback attack → disconnect.
- [x] LoE freeze: peers disagree below tip → selection holds at divergence+k until GDD resolves; then ReprocessLoE adopts.
- [x] CSJ happy-path sync: 3 peers, jumps at 4320 cadence, single header stream, jumpers' fragments advance on accepted jumps (GDD sees them).
- [x] CSJ adversarial dynamo: serves fork then rolls back past lastJumpSlot → disengaged+rotated.
- [x] Praos regression: full suite run with `--consensus-mode praos` asserting zero genesis side-effects (no LoE trim, no LoP, no historicity, no CSJ messages, fetch cadence 10ms) — plus `just check`.
- [x] Commit `test(genesis): adversarial integration suite + praos polarity guard (T13)`.

## Task 14: Local validation gate

- [x] `just check` (fmt, clippy -D warnings, build, nextest, doc) — zero failures.
- [x] devnet-validate skill: standard run in praos mode AND a genesis-mode round (validates devnet sync + forging unaffected).
- [x] Commit any fixes; push.

## Phase 2 (post-plan): mainnet genesis sync + byte-exact cross-checks + v2.0.2 release
Tracked as session tasks #4/#5 (not part of this code plan): resume db-mainnet under `--consensus-mode genesis` with peer snapshot installed; epoch-boundary `DUGITE_EPOCH_STATE_DUMP` cross-checks vs Koios mainnet at intervals; on full sync + clean soak → release-lead skill → v2.0.2.

---

## Self-review notes
- Spec coverage: all 78 finding IDs mapped (table above); blockfetch-12 resolved as documented-parity-at-defaults (both Praos concurrency limits are 1 upstream; dugite single-fetcher matches); blockfetch-09/10 resolved as documented behavioral mapping in T11 + targets switching in T6 (the Haskell churn-regime machinery is Praos-governor-internal and has no dugite analogue to diverge from).
- Verify-resume verdicts (re-running at plan time) must be reconciled before executing each task: check `/tmp/genesis-audit-findings.json` against the final workflow output corrections.
- Praos-risk concentration: T5 (shared chain-selection code) and T10 (fetch path) — both carry explicit Disabled-identity tests before any genesis logic.
