---
name: issue-1003-peermanager-dead-code-audit
description: Oracle-verified wire-or-delete verdicts for 4 orphaned NodePeerManager methods (#1003) + full "used by networking rewrite" annotation audit in dugite-node
type: project
---

## The four methods (all oracle-verified against ouroboros-network, pinned SHAs in
[[nodepeermanager-orphaned-methods-upstream-audit]] under the cardano-haskell-oracle
memory namespace)

- `record_block_fetch` (networking.rs) — **DELETED**. Its only effect was calling
  `PeerInfo::record_success()`, which is now ALSO orphaned in `dugite-network` (out of
  scope, left alone — has a same-crate unit test so it doesn't warn). Oracle: upstream
  `PeerMetric.hs` has exactly 4 metrics (`joinedPeerMetricAt`/`upstreamyness`/
  `fetchynessBytes`/`fetchynessBlocks`), all consumed ONLY by `hotDemotionPolicy`
  (which peer to demote first) — there is no positive/reward signal anywhere upstream.
  `jobPromoteColdPeer`'s success branch only sets two flags
  (`setSuccessfulConnectionFlag`, `clearTepidFlag`); `resetFailCount` has zero callers
  from any promotion-success path. Failure decay is a FIXED 120s timer
  (`policyClearFailCountDelay`), never triggered by a subsequent success. dugite's
  `reputation: f64` incrementing-on-success field has no upstream analog at all.
- `gc_fresh_inbound` (networking.rs) — **WIRED**. Called once per governor tick
  (`node/mod.rs` ~line 5460, same `pm.write()` block as `gc_divergence_witnesses`).
  Oracle: upstream is the OPPOSITE of dugite's read-time-filter-only prior state —
  `InboundGovernor/State.hs` eagerly pops matured entries out of a time-ordered
  `OrdPSQ` (`freshDuplexPeers`) into `matureDuplexPeers` via `OrdPSQ.atMostView` on
  its own wake arm inside the SAME governor loop, exactly the pattern now wired.
  `inboundMaturePeerDelay = 15*60` matches dugite's constant exactly.
- `peer_category` (networking.rs) + `PeerCategory` enum — **DELETED**. Oracle: no
  unified peer-classification sum type exists upstream. `PublicRootPeers.hs` holds
  `getLedgerPeers`/`getBigLedgerPeers` as separate `Set`s; `peerSelectionStateToView`
  (`Governor/Types.hs`) computes every category via independent `Set.intersection`/
  `Set.member` at each call site — exactly what dugite's real production code already
  does (`local_root_groups.iter().any(...)` + `big_ledger_peers.contains(addr)`,
  checked separately wherever needed). `peer_category()` was a redundant convenience
  wrapper nothing ever adopted.
- `find_inbound_duplex_by_ip` (networking.rs) — **DELETED**. Oracle: `ConnMap.hs`
  always keys by the FULL `ConnectionId` (local+remote `SockAddr`, i.e. IP+port); the
  one relaxed-match fn (`lookupByRemoteAddr`) still requires an exact remote address
  and only relaxes on which local-side entry answers (handles the transient
  `ReservedOutboundState` race during outbound connect) — a different axis entirely
  from "same IP, different port". No legitimate upstream precedent for IP-only
  lookup — same #996-shaped mistake (see
  [[project_996_mempool_revalidation_checklist]]) recurring one layer down at the
  connection-manager level. Real prod duplex detection (`register_inbound_connection`
  in connection_lifecycle.rs) already does exact `SocketAddr` matching via
  `has_any_to`/`ConnectionId` — the upstream-aligned shape was already there.

## Broader "used by networking rewrite" audit (all 18 literal hits, not the 25 the
issue claimed — line-count claims drift, always re-verify with `rg`)

All 18 resolved: 8 were STALE on code with real production callers (annotation just
removed — `add_shared_peer`, `mark_peer_duplex`, `record_handshake_rtt`,
`effective_diffusion_mode`, `is_advertisable`, `DiffusionMode::InitiatorOnly`,
`should_snapshot_normal`, `find_best_snapshot_for_rollback`); 4 were on genuinely
zero-reference legacy N2N client types never adopted (`HeaderBatchResult`,
`HeaderInfo`, `ClientError`, `DuplexError` — deleted, superseded by
`dugite_network::PipelinedChainSyncClient`/`BlockFetchClient`); the 4 target methods
above; and 2 needed a REWORDED (not removed) annotation because the underlying claim
was legitimately partial-true — `PeerManagerConfig` (5 of 6 fields are write-only,
governor targets flow through a separate `PeerTargets` path instead) and `EbbInfo`
(its only consumer, `sync::process_forward_blocks`, is ITSELF dead — see below).

## Adjacent findings, found but explicitly OUT OF SCOPE for #1003 (flagged in report,
not fixed)

- **`sync::Node::process_forward_blocks`** (sync.rs, ~1400 lines, `too_many_arguments`)
  is fully dead — its OWN neighboring comment at sync.rs:1039 already says
  "`process_forward_blocks` has no callers", and mod.rs has a dozen comments
  describing its logic as "ported" to `apply_fetched_block`. A stale sync.rs:2381
  comment wrongly claims it's "retained as the block application entry point". Prime
  candidate for a dedicated #1003 follow-up — deleting it would also let `EbbInfo`
  and the free-standing `validate_genesis_blocks` wrapper method go fully clean.
  Too large/risky to fold into this issue's diff.
- **`ConnectionMetrics` trait** (`dugite_network::ConnectionMetrics`, used by
  `N2NConnectionMetrics`/`N2CConnectionMetrics` in serve.rs) is architecturally
  orphaned end-to-end: the real N2N/N2C accept loops never construct an implementor,
  so `NodeMetrics::record_protocol_error` — despite being fully working, tested code
  — never fires in a real running node. Protocol-level connection errors are
  currently NOT recorded to Prometheus at all. Genuine wiring gap, not "networking
  rewrite" debt.
- **`node/mod.rs:11,17`** module-level `#[allow(dead_code)]` on
  `connection_lifecycle`/`peer_connection` mods are ALSO stale for those modules'
  own top-level items (both heavily used) — but a module-level `#[allow]` suppresses
  the lint recursively, and lifting it surfaced a separate set of PRE-EXISTING dead
  code inside those files (`PeerConnection::has_warm_protocols`/`has_hot_protocols`,
  `FetchedBlock::tip_slot`/`tip_hash`/`tip_block_number`, some `fake_with_*` test
  helpers). Deliberately left in place with an honest comment rather than cascading
  into an unrelated audit of connection_lifecycle.rs/peer_connection.rs.

## Methodology note: LIB vs BIN dead-code ground truth

`dugite-node` has BOTH a `[lib]` and a `[[bin]]` target that re-declares the same
`mod node; mod topology;` tree (`main.rs` has its own `mod` block duplicating
`lib.rs`'s). `cargo build -p dugite-node --lib` alone NEVER warns about unused pub
items (rustc treats pub-in-lib as external API, exempt from dead_code — real, since
`dugite-config` depends on the lib target). Ground truth for "does the actual running
node use this" is `cargo build -p dugite-node --all-targets` (the BIN targets), which
has no such exemption since a bin has no external consumers. Confirmed empirically:
this matched `rg` reference-counting exactly for all 4 target methods. Also confirmed:
`#[allow(dead_code)]` on a method transitively suppresses "field never read" warnings
for fields ONLY reachable through that method's body (e.g. `should_snapshot_bulk`'s
allow already covered `bulk_min_blocks`/`bulk_min_interval` — no separate field-level
annotation needed). And: `#[allow(dead_code)]` on a `mod` item is recursive over the
whole subtree, which is how 2 stale module-level annotations were hiding unrelated
dead code inside connection_lifecycle.rs/peer_connection.rs.
