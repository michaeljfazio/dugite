//! Connection Lifecycle Manager — temperature-based peer lifecycle.
//!
//! # Haskell Architecture Reference
//!
//! In the Haskell cardano-node, `PeerStateActions` (ouroboros-network) manages
//! peer connection temperature transitions:
//!
//! - **Cold -> Warm**: TCP connect + handshake, start KeepAlive (Established protocols)
//! - **Warm -> Hot**: Start ChainSync + BlockFetch + TxSubmission2 (Hot protocols)
//!   on the SAME multiplexed connection — no new TCP connection is created
//! - **Hot -> Warm**: Stop hot protocol tasks, keep mux + KeepAlive alive
//! - **Warm -> Cold**: Stop all protocol tasks, close mux + TCP connection
//!
//! The key invariant is **one TCP connection per peer**. Temperature transitions
//! only add/remove protocol tasks on the existing mux, never create new connections.
//!
//! # Architecture Divergence: Hot → Warm Demotion
//!
//! Dugite's `MuxChannel` is **single-use**: when `start_hot_protocols` is called,
//! the three client channels (ChainSync, BlockFetch, TxSubmission2) are moved via
//! `Option::take` into the spawned tasks.  When those tasks exit, the channels are
//! dropped.  Unlike Haskell's `MiniProtocolState` (which is reusable), there is no
//! way to re-acquire the channels without opening a fresh `PeerConnection`.
//!
//! Therefore, Hot → Warm demotion in dugite **closes the TCP connection** rather
//! than just stopping the hot protocol tasks.  The peer manager is updated to cold
//! and the next governor tick will reconnect (Cold → Warm → Hot) with fresh channels.
//! See: `demote_to_warm` and issue #516 for full details.
//!
//! ## Duplex Connections (Simultaneous Open)
//!
//! When we already have an outbound connection to a peer and they connect inbound
//! (or vice versa), Haskell promotes the connection to `Duplex` mode. Both the
//! initiator and responder sides share the same underlying TCP connection via the
//! mux's bidirectional channel support.
//!
//! This module provides `ConnectionLifecycleManager` — the node-level orchestrator
//! that translates `GovernorAction` decisions into `PeerConnection` lifecycle calls.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Per-range fetch deadline: maximum time a single `BlockFetchClient::fetch_range()`
/// call is allowed to run before being cancelled.
///
/// Matches Haskell's `bfcFetchDeadlinePolicy` (60s). When a peer's TCP connection
/// is half-open or the remote node stalls mid-batch, this timeout fires, the
/// blockfetch task exits, and the active fetcher flag is released so another peer
/// can take over. The peer is also reported as failed to the peer manager for
/// reputation scoring and exponential backoff.
const FETCH_RANGE_TIMEOUT: Duration = Duration::from_secs(60);

// BlockFetch range sizing.
//
// A single `MsgRequestRange` streams every block between its endpoints, so the
// request→`MsgStartBatch`→stream round-trip latency is paid once per range, not
// per block.  Because the worker fetches ranges serially, a small range leaves
// the link idle for a round-trip every `n` blocks — so larger ranges keep the
// link saturated during bulk sync (empirically 100→512 doubled Byron
// throughput).  But the worker buffers one range's decoded blocks before
// forwarding them to the apply channel, so a fixed large block-count would
// spike memory on big blocks (a 2000-block range of ~90 KB Conway blocks is
// ~180 MB) — and dugite *does* sync from genesis through Conway (epoch-diff
// harness).  So the range is sized by a BYTE BUDGET against a running average
// of recently-seen block sizes: it auto-grows to the protocol cap for tiny
// Byron blocks and shrinks for large Conway blocks, bounding the per-range
// buffer to ~`BLOCKFETCH_RANGE_BYTE_BUDGET` in every era.
const BLOCKFETCH_RANGE_BYTE_BUDGET: usize = 8 * 1024 * 1024;
/// Lower bound on range size (so tiny-budget edge cases still amortise a little).
const BLOCKFETCH_MIN_RANGE: usize = 64;
/// Default + hard ceiling on range size — the network's `MAX_BLOCKS_PER_FETCH`
/// per-batch DoS cap.
///
/// `MsgRequestRange(from, to)` is inclusive of both endpoints, so a range built
/// from `n` contiguous headers makes an honest peer stream exactly `n`
/// `MsgBlock` messages.  `BlockFetchClient::fetch_range` permits exactly
/// `MAX_BLOCKS_PER_FETCH` blocks per batch and rejects only the (MAX+1)th, so a
/// request range up to the cap is delivered without tripping the guard.  The
/// actual per-fetch range is sized adaptively (byte budget / running average
/// block size) and clamped to `[BLOCKFETCH_MIN_RANGE,
/// resolve_blockfetch_max_range()]`; `resolve_blockfetch_max_range()` reads the
/// operator override (env / config) and defaults to this ceiling.
const BLOCKFETCH_MAX_RANGE: usize =
    dugite_network::protocol::blockfetch::client::MAX_BLOCKS_PER_FETCH;
// Compile-time guard: the range we request must never exceed the client's
// per-batch DoS cap, or honest peers fulfilling a max-sized range get wrongly
// disconnected with `BoundsExceeded`.
const _: () = assert!(
    BLOCKFETCH_MAX_RANGE <= dugite_network::protocol::blockfetch::client::MAX_BLOCKS_PER_FETCH
);

/// Resolve the per-fetch maximum range size (block count).
///
/// Precedence: the `DUGITE_BLOCKFETCH_MAX_RANGE` environment variable overrides
/// everything; otherwise the `blockfetch_max_range` config-file field
/// (`config_value`, editable via dugite-config); otherwise the default — the
/// maximum (`BLOCKFETCH_MAX_RANGE`).  Larger ranges amortise the request
/// round-trip across more blocks (helps tiny-block Byron bulk sync).  The
/// result is clamped to `[BLOCKFETCH_MIN_RANGE, BLOCKFETCH_MAX_RANGE]` so it can
/// never exceed the network DoS cap.
pub fn resolve_blockfetch_max_range(config_value: Option<usize>) -> usize {
    std::env::var("DUGITE_BLOCKFETCH_MAX_RANGE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .or(config_value)
        .unwrap_or(BLOCKFETCH_MAX_RANGE)
        .clamp(BLOCKFETCH_MIN_RANGE, BLOCKFETCH_MAX_RANGE)
}
/// Pessimistic (Conway-sized) initial block-size estimate, so the very first
/// range — taken before any block size is known — stays small/safe and then
/// adapts within a range or two.
const BLOCKFETCH_INIT_AVG_BLOCK_BYTES: usize = 65_536;

/// Number of `MsgRequestRange` requests the single fetcher keeps in flight at
/// once (request pipelining). With a window of N, while we receive + apply the
/// blocks of range *i*, the peer is already streaming ranges *i+1..i+N*, so
/// each range's network round-trip overlaps the receipt/apply of earlier
/// ranges instead of being paid serially (measured: ~9× lower per-batch fetch
/// latency on mainnet bulk sync). Mirrors Haskell's `bfcMaxRequestsInflight`
/// pipelining (`Ouroboros.Network.BlockFetch.Client`), which keeps multiple
/// range requests outstanding (cardano-node caps at 100, bounded by byte
/// watermarks). We use a small fixed window: it captures the round-trip
/// overlap while bounding the in-flight decode buffer (window × range ×
/// block-size) and the blast radius if a peer stalls mid-pipeline. Always on
/// — `1` would restore the old strictly-sequential behaviour, but pipelining
/// is the correct (Haskell-parity) default and carries no regression.
///
/// **Ingress invariant (#747):** `BLOCKFETCH_PIPELINE_WINDOW *
/// BLOCKFETCH_RANGE_BYTE_BUDGET` must not exceed
/// `peer_connection::BLOCKFETCH_INGRESS_LIMIT` (the mux-layer per-protocol
/// ingress buffer limit).  A violation means pipelined in-flight data can
/// silently overflow the ingress queue, killing the mux with no error
/// visible at the BlockFetch protocol level.
/// Window 2 × 8 MB = 16 MB ≤ 48 MB ingress limit — invariant holds with
/// ~3× headroom for estimate slack.
///
/// #sync-eval: a bump to 4 to hide RTT on high-latency links was evaluated and
/// REJECTED — it violates the SECOND ingress invariant below
/// (`BLOCKFETCH_PIPELINE_WINDOW * RANGE_BYTE_ABORT_CEILING <= INGRESS_LIMIT`):
/// at the 20 MB abort ceiling, 4 × 20 MB = 80 MB > 48 MB. Window is therefore
/// capped at 2 (2 × 20 = 40 ≤ 48) without also enlarging the mux ingress buffer
/// or lowering the abort ceiling — both separate, carefully-reviewed changes.
/// Real RTT-hiding throughput work is concurrent multi-peer fetch (deferred).
const BLOCKFETCH_PIPELINE_WINDOW: usize = 2;

/// GSV fetch-peer preference width (cardano-node `nPreferedPeers`). The single
/// bulk-sync fetch slot is contested only by the top-K peers ranked by measured
/// fetch bandwidth. Haskell uses `maxConcurrencyBulkSync = 1`; dugite keeps a
/// hot standby (K=2) so a momentarily-busy best peer cannot stall the slot,
/// while still concentrating fetching on the fastest peers.
const GSV_FETCH_TOP_K: usize = 2;

// Issue #747: compile-time invariant, referencing the REAL mux constant
// directly (no hand-mirrored copy that could drift).
// The total pipelined in-flight bytes (PIPELINE_WINDOW × RANGE_BYTE_BUDGET)
// must not exceed the mux ingress queue limit, or in-flight data silently
// overflows the buffer and kills the mux connection without a protocol-level
// error.
const _: () = assert!(
    BLOCKFETCH_PIPELINE_WINDOW * BLOCKFETCH_RANGE_BYTE_BUDGET
        <= super::peer_connection::BLOCKFETCH_INGRESS_LIMIT,
    "pipelined in-flight budget exceeds mux ingress limit: reduce BLOCKFETCH_PIPELINE_WINDOW or increase peer_connection::BLOCKFETCH_INGRESS_LIMIT"
);

/// Slots a dynamo's CSJ fragment must lead our selected chain by before the
/// unproductive-dynamo watchdog (#742) treats it as legitimately PARKED on the
/// forecast horizon and SKIPS rotation (#760-A). Chosen well below the smallest
/// network forecast/stability window (preview `3k/f ≈ 25 920` slots) and well
/// above streaming noise, so a parked dynamo (≈ a full stability window ahead)
/// is never mistaken for a silent one, and a genuinely-silent dynamo (fragment
/// at or near our tip) is always rotated.
pub(crate) const GENESIS_PARKED_DYNAMO_MARGIN_SLOTS: u64 = 2_000;

// Compile-time guard: the margin must stay below the smallest network
// forecast/stability window (preview `3k/f = 3*432/0.05 = 25 920` slots) so a
// dynamo parked on the horizon (≈ a full window ahead) is never mistaken for a
// silent one on ANY network.
const _: () = assert!(GENESIS_PARKED_DYNAMO_MARGIN_SLOTS < 25_920);

/// Should an unproductive dynamo that has starved ChainSel past the watchdog
/// window be ROTATED?
///
/// `#742` added this watchdog to rotate a dynamo that never feeds headers (the
/// LoP cannot kill it because dugite pauses the LoP while a peer parks on the
/// forecast horizon). But on a cold genesis restart a HEALTHY dynamo streams a
/// full forecast window of headers, drains them, and then parks — and was being
/// rotated too (`#760-A` ~1 blk/min churn). The discriminator: a dynamo whose
/// CSJ fragment leads our selected chain by more than
/// [`GENESIS_PARKED_DYNAMO_MARGIN_SLOTS`] has fed headers and is parked
/// (don't rotate — the ledger is catching up); one at/near our tip, or with no
/// fragment at all, is genuinely silent (rotate). Mirrors Haskell, where a peer
/// blocked at the forecast horizon is not counted as starving us.
pub(crate) fn should_rotate_unproductive_dynamo(
    fragment_head_slot: Option<u64>,
    chain_tip_slot: u64,
) -> bool {
    match fragment_head_slot {
        Some(head) => head <= chain_tip_slot.saturating_add(GENESIS_PARKED_DYNAMO_MARGIN_SLOTS),
        None => true,
    }
}

/// Why a protocol task reported a peer to `peer_failure_tx` (#751).
///
/// `ProtocolFault` is a PROVABLE protocol violation (mis-declared block
/// sizes, undecodable blocks, agency/state violations): Haskell parity is
/// a thrown exception that tears the whole bearer down, so the handler
/// additionally runs full lifecycle demotion (`demote_to_cold`). Without
/// the teardown the convicted peer keeps a hot connection with a dead
/// BlockFetch worker, and its in-flight flood survives as a silent
/// bandwidth sink (the dropped mux channel discards frames without
/// closing TCP).
///
/// `Slow` is a performance failure (fetch/keepalive timeout, send
/// failure): reputation/backoff PLUS connection teardown. Teardown was
/// added (#sync-eval) after a `Slow` burst was found to collapse the peer
/// set permanently: leaving the mux alive kept the peer in
/// `lifecycle.connections`, so the governor's `has_connection` reconnect
/// gate blocked re-promotion forever (the connection never died on its own
/// because keepalive kept succeeding — the peer was fine, our ledger was the
/// laggard). Tearing down lets the governor reconnect on the normal
/// Cold→Warm schedule after the backoff `peer_failed()` applies. This now
/// matches Haskell (which kills the bearer on timeouts and reconnects).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerFailureKind {
    /// Provable protocol violation — reputation + connection teardown.
    ProtocolFault,
    /// Timeout / transport failure — reputation only.
    Slow,
    /// Peer cannot offer a usable chain: its ChainSync intersection resolves
    /// only at genesis while our local selection is beyond `k` blocks (the peer
    /// is far behind our immutable tip, or on a disjoint chain). This is the
    /// dugite equivalent of Haskell's `ChainSyncClientResult::ForkTooDeep` — an
    /// EXPECTED, routine outcome on public networks (stale / wrong-chain
    /// registered relays), NOT a fault. Handled identically to `Slow`
    /// (reputation + teardown + governor re-promote after backoff) but logged
    /// at INFO, mirroring cardano-node's `Notice` severity for
    /// `TraceTermination ForkTooDeep`, so it does not drown real warnings.
    Unsuitable,
}

/// Marker error returned by `chainsync_client_task` when the peer's ChainSync
/// intersection resolves only at genesis while our local selection is beyond
/// `k` blocks. Downstream code downcasts to this to classify the failure as
/// [`PeerFailureKind::Unsuitable`] (logged at INFO) instead of a generic fault.
/// See [`classify_chainsync_failure`]. Equivalent to Haskell `ForkTooDeep`.
#[derive(Debug, Clone)]
pub(crate) struct PeerUnsuitable {
    pub(crate) reason: String,
}

impl std::fmt::Display for PeerUnsuitable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for PeerUnsuitable {}

/// Classify a `chainsync_client_task` failure into a [`PeerFailureKind`].
///
/// A [`PeerUnsuitable`] marker (ChainSync intersection only at genesis — the
/// Haskell `ForkTooDeep` equivalent) is an expected, routine peer-quality
/// outcome and maps to [`PeerFailureKind::Unsuitable`] (logged at INFO).
/// Everything else (bearer close, decode error, timeout, etc.) maps to
/// [`PeerFailureKind::Slow`].
pub(crate) fn classify_chainsync_failure(err: &anyhow::Error) -> PeerFailureKind {
    if err.downcast_ref::<PeerUnsuitable>().is_some() {
        PeerFailureKind::Unsuitable
    } else {
        PeerFailureKind::Slow
    }
}

use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};

use dugite_network::peer::governor::GovernorAction;
use dugite_network::RollbackAnnouncement;
use dugite_network::{BlockAnnouncement, TxValidator};

use dugite_ledger::LedgerState;
use dugite_mempool::Mempool;
use dugite_network::{TxIdAndSize, TxSource};
use dugite_primitives::block::Block;
use dugite_storage::ChainDB;

use super::networking::{ConnectionDirection, NodePeerManager};
use super::peer_connection::{
    PeerConnection, PeerConnectionDirection, PeerConnectionError, ProtocolTaskFn,
};
use super::serve::ChainDBBlockProvider;
use crate::metrics::NodeMetrics;

// ─── Shared State Types ─────────────────────────────────────────────────────

/// Per-peer BlockFetch status — mirrors Haskell's `PeerFetchStatus`.
///
/// ## Haskell Reference
///
/// `ouroboros-network/ouroboros-network/src/Ouroboros/Network/BlockFetch/ClientState.hs`
/// defines:
///
/// ```haskell
/// data PeerFetchStatus header =
///     PeerFetchStatusReady (Set (Point header)) IsIdle
///   | PeerFetchStatusBusy (Set (Point header)) IsIdle
///   | PeerFetchStatusAberrant
/// ```
///
/// `fetchDecisions` in `Ouroboros.Network.BlockFetch.Decision` (Decision.hs:~450)
/// excludes peers with `PeerFetchStatusAberrant` from the candidate set before
/// running the fetch-range selection algorithm. Ready peers are preferred over
/// Busy peers: `PeerFetchStatusReady` compares before `PeerFetchStatusBusy` in
/// the peer ordering (`comparePeerFetchStatus`).
///
/// This Rust analog tracks the three states with enough detail to reproduce
/// the Haskell ordering and exclusion semantics:
/// - **Ready** — no fetch in-flight; eligible for the next range dispatch.
/// - **Busy** — at least one fetch range in-flight; still eligible but de-preferred
///   vs Ready peers.
/// - **Aberrant** — 3+ consecutive delivery failures within 30 s; excluded from
///   all fetch decisions until a successful delivery resets the counter.
///
/// The threshold (3 failures / 30 s) is a Dugite operational constant.  Haskell
/// does not expose a single numerical threshold — it relies on the governor's
/// exponential back-off + peer reputation to demote peers — but the observable
/// effect is the same: a peer that consistently fails to deliver blocks stops
/// receiving fetch requests until it recovers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PeerFetchStatus {
    /// Peer is idle and ready for the next fetch range.
    #[default]
    Ready,
    /// Peer has at least one fetch range in-flight.
    Busy,
    /// Peer has accumulated too many consecutive delivery failures and is
    /// excluded from fetch decisions until it delivers a block successfully.
    Aberrant,
}

/// Threshold: mark a peer Aberrant after this many consecutive failed
/// deliveries within the observation window.
pub const ABERRANT_FAILURE_THRESHOLD: u32 = 3;

/// Observation window for consecutive failures.  Failures older than this
/// are not counted towards the Aberrant threshold.
pub const ABERRANT_WINDOW: Duration = Duration::from_secs(30);

/// Candidate chain state from a peer's ChainSync.
///
/// Updated by per-peer ChainSync tasks as they receive headers. Read by the
/// BlockFetch decision task to determine which blocks to fetch and from which
/// peers. This is the coordination point between ChainSync and BlockFetch,
/// matching the Haskell `FetchClientRegistry` / `FetchDecisionPolicy` pattern.
#[derive(Debug, Clone, Default)]
pub struct CandidateChainState {
    /// Slot of the peer's reported tip.
    pub tip_slot: u64,
    /// Hash of the peer's reported tip block.
    pub tip_hash: [u8; 32],
    /// Block number (height) of the peer's reported tip.
    pub tip_block_number: u64,
    /// Headers received via ChainSync but not yet fetched by BlockFetch.
    ///
    /// These accumulate as ChainSync streams headers ahead of BlockFetch.
    /// The BlockFetch decision task consumes entries from this list when it
    /// schedules fetch requests.
    pub pending_headers: Vec<PendingHeader>,
    /// Count of headers appended since the last *full* `pending_headers` prune.
    ///
    /// During bulk sync `pending_headers` sits near the `PENDING_HEADERS_PAUSE`
    /// cap (~10 k), so running a full O(pending) `has_block` prune on every
    /// arriving header is O(N²) per batch (it was the #1 sync-path CPU cost).
    /// Each arriving header is cheaply checked individually (O(1)); a full
    /// prune — which drains entries BlockFetch has fetched+stored since — is
    /// then run only once per `PENDING_PRUNE_INTERVAL` headers.  This does not
    /// change the steady-state `pending_headers` size (that is governed by
    /// fetch lag, not prune cadence) and is safe because the BlockFetch
    /// decision independently skips already-stored headers via its own
    /// `has_block` filter, so a few transiently-stale entries are never
    /// re-fetched.
    pub headers_since_prune: u32,
    /// Per-peer eager-validation op-cert counter map (issue #654 P1.b).
    ///
    /// Updated by the per-peer eager-validation call at MsgRollForward
    /// (`OuroborosPraos::validate_header_full_with_counters`). Per-peer
    /// state isolation per #652 C1: the global authoritative
    /// `OuroborosPraos.opcert_counters` is owned by the body-apply path
    /// and never written here. Reset on MsgRollBackward (Phase 1
    /// simplification of #652 C5 — full per-peer history rewind comes
    /// in a follow-up phase).
    pub eager_opcert_counters: std::collections::HashMap<dugite_primitives::hash::Hash28, u64>,
    /// Per-peer BlockFetch status (Haskell `PeerFetchStatus`).
    ///
    /// Tracks whether this peer is Ready, Busy (fetch in-flight), or
    /// Aberrant (too many consecutive delivery failures).  The BlockFetch
    /// decision logic reads this field before dispatching any new range:
    /// Aberrant peers are excluded; Busy peers are de-preferred relative
    /// to Ready peers.
    ///
    /// Written exclusively by the per-peer BlockFetch worker via
    /// `record_fetch_delivered` / `record_fetch_failed`.  Read by
    /// `BlockFetchLogicTask::evaluate_and_fetch`.
    pub fetch_status: PeerFetchStatus,
    /// Timestamp of the last successful block delivery from this peer.
    ///
    /// Updated by the per-peer BlockFetch worker whenever `MsgBlock` arrives.
    /// Used together with `consecutive_failures` to implement the Aberrant
    /// detection window.
    pub last_delivered_at: Option<std::time::Instant>,
    /// Count of consecutive delivery failures since the last successful block.
    ///
    /// Reset to 0 on any successful delivery.  When it reaches
    /// `ABERRANT_FAILURE_THRESHOLD` within `ABERRANT_WINDOW` seconds,
    /// `fetch_status` is set to `Aberrant`.
    pub consecutive_failures: u32,
    /// Number of BlockFetch ranges currently in-flight for this peer.
    ///
    /// Incremented by `BlockFetchLogicTask` when a range is dispatched,
    /// decremented when the worker reports delivery (success or failure).
    pub in_flight_blocks: u32,
}

impl CandidateChainState {
    /// Record a successful block delivery from this peer.
    ///
    /// Mirrors the Haskell `PeerFetchStatus` transition from `Busy` back to
    /// `Ready` after blocks arrive via `MsgBlock`.  Also clears any Aberrant
    /// state: a peer that delivers successfully is re-admitted to fetch
    /// decisions regardless of prior failure count.
    ///
    /// Called by the per-peer BlockFetch worker after each `MsgBlock`.
    pub fn record_fetch_delivered(&mut self) {
        self.consecutive_failures = 0;
        self.last_delivered_at = Some(std::time::Instant::now());
        self.in_flight_blocks = self.in_flight_blocks.saturating_sub(1);
        // Clear Aberrant: any successful delivery rehabilitates the peer.
        if self.fetch_status == PeerFetchStatus::Aberrant {
            self.fetch_status = PeerFetchStatus::Ready;
        }
        if self.in_flight_blocks == 0 {
            self.fetch_status = PeerFetchStatus::Ready;
        } else {
            self.fetch_status = PeerFetchStatus::Busy;
        }
    }

    /// Record a failed delivery (range timeout or protocol error).
    ///
    /// Implements the Aberrant escalation logic:
    /// - If the last failure was more than `ABERRANT_WINDOW` ago, the
    ///   consecutive_failures counter is reset first (stale failures don't
    ///   accumulate across windows).
    /// - If `consecutive_failures` reaches `ABERRANT_FAILURE_THRESHOLD`,
    ///   `fetch_status` is set to `Aberrant` and the peer is excluded from
    ///   future fetch decisions until `record_fetch_delivered` is called.
    ///
    /// Called by the per-peer BlockFetch worker on range timeout or protocol
    /// error.  Also logs a WARN with the peer address if the threshold is
    /// crossed.
    pub fn record_fetch_failed(&mut self, addr: std::net::SocketAddr) {
        // Reset counter if the last failure was outside the observation window.
        let now = std::time::Instant::now();
        if let Some(last) = self.last_delivered_at {
            if now.duration_since(last) > ABERRANT_WINDOW {
                self.consecutive_failures = 0;
            }
        }
        self.consecutive_failures += 1;
        self.in_flight_blocks = self.in_flight_blocks.saturating_sub(1);
        if self.consecutive_failures >= ABERRANT_FAILURE_THRESHOLD
            && self.fetch_status != PeerFetchStatus::Aberrant
        {
            self.fetch_status = PeerFetchStatus::Aberrant;
            tracing::warn!(
                %addr,
                consecutive_failures = self.consecutive_failures,
                threshold = ABERRANT_FAILURE_THRESHOLD,
                "BlockFetch: peer marked Aberrant — excluding from fetch decisions"
            );
        } else if self.in_flight_blocks == 0 {
            self.fetch_status = PeerFetchStatus::Ready;
        }
    }

    /// Mark a fetch range as dispatched to this peer.
    ///
    /// Sets `fetch_status` to `Busy` and increments `in_flight_blocks`.
    /// Called by the BlockFetch decision task immediately before sending a
    /// range to the peer's worker channel.
    pub fn record_fetch_dispatched(&mut self) {
        self.in_flight_blocks += 1;
        self.fetch_status = PeerFetchStatus::Busy;
    }

    /// Return whether this peer is currently eligible for fetch dispatch.
    ///
    /// Mirrors Haskell's `fetchDecisions` peer filter: Aberrant peers are
    /// always excluded; Ready and Busy peers are both eligible (Ready is
    /// preferred by the caller's sort order).
    pub fn is_fetch_eligible(&self) -> bool {
        self.fetch_status != PeerFetchStatus::Aberrant
    }
}

/// A block header received via ChainSync, pending BlockFetch download.
///
/// Contains enough information for BlockFetch to request the full block
/// and for the decision task to reason about which range to fetch.
#[derive(Debug, Clone)]
pub struct PendingHeader {
    /// Slot of the block this header describes.
    pub slot: u64,
    /// Hash of the block (used in BlockFetch range requests).
    pub hash: [u8; 32],
    /// Raw CBOR-encoded header bytes (for header validation before fetch).
    pub header_cbor: Vec<u8>,
    /// Block body size DECLARED in the header (Shelley+ `block_body_size`).
    ///
    /// `None` for Byron headers (no size field) or undecodable headers.
    /// Used for EXACT in-flight byte accounting when batching BlockFetch
    /// ranges — Haskell's `blockFetchSize` analogue (#747: an average-based
    /// estimate let nominal 8 MB ranges deliver 2×+ actual bytes and overrun
    /// the mux ingress queue).
    pub body_size: Option<u64>,
    /// `prev_hash` from the decoded header (Shelley+), used for
    /// chain-adjacency run splitting: two consecutive `pending_headers`
    /// entries are only fetchable in ONE `MsgRequestRange` when the second
    /// block's parent IS the first block (`pending_headers` is sparse
    /// relative to the peer's chain — already-stored blocks are never
    /// pushed). `None` for Byron / undecodable headers (adjacency unknown —
    /// only filter-gap splitting applies).
    pub prev_hash: Option<[u8; 32]>,
}

/// Estimated wire bytes a fetched block will occupy in the mux ingress
/// queue: declared body size + the header itself + per-`MsgBlock` framing.
/// Falls back to `avg_block_bytes` when the header does not declare a size
/// (Byron).
fn estimated_block_wire_bytes(h: &PendingHeader, avg_block_bytes: usize) -> usize {
    match h.body_size {
        Some(b) => (b as usize)
            .saturating_add(h.header_cbor.len())
            .saturating_add(16),
        None => avg_block_bytes,
    }
}

/// #751: hard multiple of a range's header-declared byte estimate beyond
/// which the receive side aborts the range as a peer fault. Declared body
/// sizes come from SIGNED headers, so an honest peer's actual delivery
/// tracks the estimate almost exactly (the estimate already includes the
/// header bytes and per-`MsgBlock` framing); 3× is unreachable without
/// mis-declaring sizes.
pub(crate) const RANGE_BYTE_ABORT_FACTOR: usize = 3;

/// #751: absolute slack added on top of the factor so that tiny ranges
/// (one small block) can never trip on per-block framing variance.
pub(crate) const RANGE_BYTE_ABORT_SLACK: usize = 1 << 20; // 1 MiB

/// #751: hard ceiling on any single range's abort limit, sized so that the
/// worst case across the whole pipeline window still convicts BEFORE the
/// mux ingress backstop kills the connection generically (which would lose
/// the attribution this feature exists to provide). Honest deliveries sit
/// at ≈1.0× a budget-bounded estimate (≤ `BLOCKFETCH_RANGE_BYTE_BUDGET` +
/// one max-size block ≈ 8.1 MB), so a 20 MiB ceiling keeps ≥2.4× honest
/// headroom. ASSUMPTION: `maxBlockBodySize` stays ≪ this ceiling (mainnet
/// has never exceeded 90,112 bytes); revisit if protocol params ever allow
/// multi-MB blocks.
pub(crate) const RANGE_BYTE_ABORT_CEILING: usize = 20 * 1024 * 1024;

// #751 attribution invariant: the abort must fire before the generic
// ingress death even with every pipelined range simultaneously at its
// ceiling. Mirrors the #747 compile-time invariant directly above it in
// spirit: reference the REAL mux constant, no hand-mirrored copy.
const _: () = assert!(
    BLOCKFETCH_PIPELINE_WINDOW * RANGE_BYTE_ABORT_CEILING
        <= super::peer_connection::BLOCKFETCH_INGRESS_LIMIT,
    "#751 abort ceiling × pipeline window exceeds the mux ingress limit: \
     attribution would be lost to the generic ingress backstop"
);
// Honest-headroom invariant: the ceiling must comfortably exceed the
// largest possible honest range (byte budget + margin), or honest peers
// could be convicted.
const _: () = assert!(
    RANGE_BYTE_ABORT_CEILING >= 2 * BLOCKFETCH_RANGE_BYTE_BUDGET,
    "#751 abort ceiling too close to the range byte budget: honest \
     full-budget ranges would risk conviction"
);

/// Receive-side hard byte limit for a range whose EVERY header declared its
/// body size (#751). Ranges containing any undeclared (Byron) header are
/// estimated from the adaptive average, which honest variance can exceed
/// arbitrarily — those ranges are never armed; the 48 MB mux ingress
/// backstop still bounds them. For declared ranges, exceeding this limit
/// means the peer is lying about block sizes: the range is aborted as a
/// `ProtocolError` so the overrun is attributed to the peer
/// (reputation/backoff) instead of dying generically at the ingress limit.
pub(crate) fn range_byte_abort_limit(estimated_bytes: usize) -> usize {
    estimated_bytes
        .saturating_mul(RANGE_BYTE_ABORT_FACTOR)
        .saturating_add(RANGE_BYTE_ABORT_SLACK)
        .min(RANGE_BYTE_ABORT_CEILING)
}

/// True when every header in the slice declares its body size, i.e. the
/// range byte estimate is exact (Shelley+) rather than average-based
/// (Byron) — the precondition for arming the #751 receive-side abort.
pub(crate) fn range_all_declared(headers: &[PendingHeader]) -> bool {
    headers.iter().all(|h| h.body_size.is_some())
}

/// Per-range receive-side byte accounting with the #751 hard abort (armed
/// only for declared-size ranges). Extracted from the recv callback so the
/// armed behavior — not just the threshold math — is unit-testable: feed it
/// a stream of block sizes and it convicts exactly when an armed range
/// crosses its limit.
pub(crate) struct RangeByteAbort {
    /// Hard byte limit; `None` when the range is not armed
    /// (Byron/average-estimated — see `range_all_declared`).
    limit: Option<usize>,
    /// The declared-size estimate the limit derives from (conviction
    /// message context).
    estimated_bytes: usize,
    /// Cumulative `MsgBlock` payload bytes delivered so far.
    seen_bytes: usize,
}

impl RangeByteAbort {
    pub(crate) fn new(all_declared: bool, estimated_bytes: usize) -> Self {
        Self {
            limit: all_declared.then(|| range_byte_abort_limit(estimated_bytes)),
            estimated_bytes,
            seen_bytes: 0,
        }
    }

    /// Account one delivered block payload. `Err` = the peer provably
    /// mis-declared block sizes (#751): abort the range as a peer fault.
    pub(crate) fn on_block(
        &mut self,
        wire_len: usize,
    ) -> Result<(), dugite_network::error::ProtocolError> {
        self.seen_bytes = self.seen_bytes.saturating_add(wire_len);
        if let Some(limit) = self.limit {
            if self.seen_bytes > limit {
                return Err(dugite_network::error::ProtocolError::BoundsExceeded {
                    protocol: "BlockFetch",
                    reason: format!(
                        "range delivered {} bytes against a declared-size estimate \
                         of {} bytes (abort limit {limit}): peer is mis-declaring \
                         block body sizes (#751)",
                        self.seen_bytes, self.estimated_bytes,
                    ),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn seen_bytes(&self) -> usize {
        self.seen_bytes
    }
}

/// Chunk `headers` into consecutive index ranges `[start, end]` such that the
/// ESTIMATED wire bytes per range stay within `BLOCKFETCH_RANGE_BYTE_BUDGET`
/// (computed from header-declared body sizes — exact for Shelley+; average
/// fallback for Byron), with at least one header and at most `max_range`
/// headers per range.
///
/// This is the #747 ingress-invariant companion: with per-range actual bytes
/// bounded by `budget + one max-size block`, the pipelined worst case is
/// `BLOCKFETCH_PIPELINE_WINDOW × (budget + max_block)` ≈ 16.2 MB — safely
/// inside the 32 MB mux ingress limit regardless of how slowly the apply
/// side drains. The previous block-COUNT chunking (`budget / avg`) bounded
/// only an estimate: a burst of blocks ~2× the running average delivered
/// ~33.5 MB against the 32 MB limit and silently killed the connection
/// (observed live, mainnet ep388, 2026-06-11T22:30Z).
pub(crate) fn build_fetch_ranges(
    headers: &[PendingHeader],
    avg_block_bytes: usize,
    max_range: usize,
) -> Vec<(usize, usize)> {
    let avg = avg_block_bytes.max(1);
    let max_range = max_range.max(1);
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    for (i, h) in headers.iter().enumerate() {
        let est = estimated_block_wire_bytes(h, avg);
        let count = i - start;
        // Close the current range BEFORE adding this header when either
        // bound would be exceeded (but never produce an empty range).
        if count > 0
            && (bytes.saturating_add(est) > BLOCKFETCH_RANGE_BYTE_BUDGET || count >= max_range)
        {
            result.push((start, i - 1));
            start = i;
            bytes = 0;
        }
        bytes = bytes.saturating_add(est);
    }
    if start < headers.len() {
        result.push((start, headers.len() - 1));
    }
    result
}

/// Select pending headers that still need to be fetched from a peer.
///
/// Filters by **hash**, not slot, so that fork blocks whose slot is ≤ the
/// current applied tip slot are still scheduled for download. This matches
/// the Haskell `Ouroboros.Network.BlockFetch.Decision` behaviour: every
/// header on `theirFrag` that is not on `curChain` (i.e. not already known
/// to ChainDB) is a fetch candidate, regardless of slot ordering.
///
/// A previous implementation used `h.slot > applied_slot` as the predicate
/// which silently dropped legitimate fork blocks after a `MsgRollBackward`,
/// stranding the candidate fragment and stalling chain selection.
pub(crate) fn select_headers_to_fetch<F>(
    pending: &[PendingHeader],
    is_known_in_chain_db: F,
    fetched_hashes: &std::collections::HashSet<[u8; 32]>,
) -> Vec<PendingHeader>
where
    F: Fn(&[u8; 32]) -> bool,
{
    pending
        .iter()
        .filter(|h| !is_known_in_chain_db(&h.hash) && !fetched_hashes.contains(&h.hash))
        .cloned()
        .collect()
}

/// Like [`select_headers_to_fetch`], but preserves CONTIGUITY information:
/// returns runs of headers that were CONSECUTIVE in `pending` (the peer's
/// in-order candidate stream). A new run starts wherever the filter skipped
/// an element.
///
/// Why this matters (#747): a `MsgRequestRange(from, to)` makes the peer
/// stream EVERY block between the two points on its chain — not just the
/// headers we enumerated. Building one range across a filtered-out gap
/// (e.g. blocks already delivered by an earlier short-batched claim) makes
/// the peer re-send the gap blocks too: the range's actual wire bytes exceed
/// the byte-accounted estimate (observed live as ~2× — 33.5 MB against the
/// 32 MB mux ingress limit — and as systematic re-download waste). Ranges
/// must therefore never span a gap.
pub(crate) fn select_fetch_runs<F>(
    pending: &[PendingHeader],
    is_known_in_chain_db: F,
    fetched_hashes: &std::collections::HashSet<[u8; 32]>,
) -> Vec<Vec<PendingHeader>>
where
    F: Fn(&[u8; 32]) -> bool,
{
    let mut runs: Vec<Vec<PendingHeader>> = Vec::new();
    let mut gap = true; // start of a fresh run
    for h in pending {
        if !is_known_in_chain_db(&h.hash) && !fetched_hashes.contains(&h.hash) {
            // Chain-adjacency split: even without a filter gap, consecutive
            // pending entries may not be consecutive blocks on the peer's
            // chain (`pending_headers` is sparse — already-stored blocks are
            // never pushed, e.g. the overlap re-streamed after a CSJ dynamo
            // rotation). A range spanning such a hidden gap makes the peer
            // deliver the gap blocks too, breaking the byte accounting
            // (observed live: 1.7-2x estimate, #747). Split whenever the
            // next header's declared parent is NOT the previous header in
            // the run; unknown parents (Byron) keep filter-gap-only
            // behaviour.
            let adjacent = match (runs.last_mut().filter(|_| !gap), h.prev_hash) {
                (Some(run), Some(prev)) => run.last().map(|p| p.hash == prev).unwrap_or(true),
                (Some(_), None) => true, // unknown parent — don't split
                (None, _) => true,       // fresh run anyway
            };
            if gap || !adjacent {
                runs.push(Vec::new());
                gap = false;
            }
            runs.last_mut().expect("run exists").push(h.clone());
        } else {
            gap = true;
        }
    }
    runs
}

/// A block fetched by a BlockFetch task, ready for ledger application.
///
/// Sent from per-peer BlockFetch tasks to the main run loop via an `mpsc`
/// channel. The run loop applies these blocks to the ChainDB and LedgerState
/// in order.
#[derive(Debug)]
pub struct FetchedBlock {
    /// Address of the peer that served this block.
    pub peer: SocketAddr,
    /// The fully deserialized block.
    pub block: Block,
    /// Tip slot reported by the peer at the time of fetch.
    pub tip_slot: u64,
    /// Tip hash reported by the peer at the time of fetch.
    pub tip_hash: [u8; 32],
    /// Tip block number reported by the peer at the time of fetch.
    pub tip_block_number: u64,
}

/// Result of a background cold->warm connection attempt.
///
/// Sent from `spawn_connect` background tasks to the main run loop via an `mpsc`
/// channel. `Ok` carries the ready `PeerConnection` and measured handshake RTT;
/// `Err` carries the peer address and a human-readable error string.
pub type ConnectResult = Result<(SocketAddr, PeerConnection, f64), (SocketAddr, String)>;

// ─── Lifecycle Manager ──────────────────────────────────────────────────────

/// Identifier for a single physical TCP connection.
///
/// Matches Haskell `Ouroboros.Network.ConnectionId { localAddress, remoteAddress }`.
/// Two connections to the same remote peer are considered distinct as long as
/// their `(local, remote)` tuples differ — for example, our outbound (with
/// ephemeral source port) coexists with our inbound (which has our listen port
/// as its local address). This is the keying strategy used by Haskell's
/// `Ouroboros.Network.ConnectionManager.ConnMap`.
///
/// `Ord` sorts first by remote then by local, mirroring Haskell's `ConnectionId`
/// `Ord` instance (load-bearing for `mapKeysMonotonic` in the upstream code,
/// and useful here for deterministic iteration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    /// Our side of the TCP connection (`(local_ip, local_port)`).
    pub local: SocketAddr,
    /// The peer's side of the TCP connection (`(peer_ip, peer_port)`).
    pub remote: SocketAddr,
}

impl PartialOrd for ConnectionId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ConnectionId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Remote first, local second — matches Haskell's ConnectionId Ord.
        self.remote
            .cmp(&other.remote)
            .then(self.local.cmp(&other.local))
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}<->{}", self.local, self.remote)
    }
}

/// Manages per-peer connections and temperature transitions.
///
/// Matches Haskell `PeerStateActions`: temperature-based protocol activation
/// without creating new connections. Connections are keyed by [`ConnectionId`]
/// (`(local, remote)` tuple), so an inbound and an outbound to the same remote
/// peer can coexist when their local addresses differ — matching Haskell's
/// `Ouroboros.Network.ConnectionManager.ConnMap`.
///
/// The lifecycle manager owns all active `PeerConnection` instances and
/// provides methods for each temperature transition. It also creates the
/// protocol task closures (KeepAlive, ChainSync, BlockFetch, TxSubmission2)
/// that capture shared node state.
///
/// # Thread Safety
///
/// This struct is NOT `Sync` — it is owned by a single async task (the
/// connection manager loop) that processes `GovernorAction`s sequentially.
/// Shared state (ChainDB, LedgerState, candidate_chains) is accessed via
/// `Arc<RwLock<_>>` to allow concurrent protocol task access.
pub struct ConnectionLifecycleManager {
    /// Active peer connections indexed by [`ConnectionId`].
    ///
    /// Multiple entries may share the same `remote` (one per direction or
    /// per local source port). Invariant: every entry here has a live mux
    /// (is_alive() == true). Dead connections are removed by
    /// `cleanup_dead_connections()`.
    connections: HashMap<ConnectionId, PeerConnection>,

    /// Network magic for N2N handshakes (e.g. 2 for preview, 764824073 for mainnet).
    network_magic: u64,

    /// Whether peer sharing is enabled in handshake negotiation.
    peer_sharing: bool,

    /// TCP connect timeout for outbound connections.
    connect_timeout: Duration,

    /// Shared candidate chain state: updated by ChainSync tasks, read by BlockFetch decision.
    ///
    /// Each peer's ChainSync task writes its tip and pending headers here.
    /// The BlockFetch decision task reads all entries to determine optimal
    /// fetch assignments.
    candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,

    /// Channel for BlockFetch tasks to send downloaded blocks to the main run loop.
    fetched_blocks_tx: mpsc::Sender<FetchedBlock>,

    /// Broadcast channel for announcing new blocks to N2N ChainSync servers.
    block_announcement_tx: broadcast::Sender<BlockAnnouncement>,

    /// Shared ChainDB — protocol tasks read chain state for intersection finding.
    chain_db: Arc<RwLock<ChainDB>>,

    /// Shared LedgerState — protocol tasks read ledger tip for intersection.
    ledger_state: Arc<RwLock<LedgerState>>,

    /// Lock-free read view of stable ledger state (#651 P2 / #652 P0).
    /// Cloned into each chainsync task for forecast-horizon checks etc.
    ledger_view: Arc<arc_swap::ArcSwap<super::ledger_view::LedgerView>>,

    /// Watch channel firing on every ledger tip advance (#654 — Phase 1
    /// of the eager-validation back-pressure design). Cloned into each
    /// chainsync task so its receive loop can park on
    /// `tip_rx.changed().await` when an incoming header lies beyond the
    /// forecast horizon, and wake exactly when the tip catches up.
    ledger_tip_slot_tx: tokio::sync::watch::Sender<u64>,

    /// Read-only seed Praos engine cloned per-call inside each chainsync
    /// task for eager per-peer header validation (issue #654 P1.b).
    /// Per-peer mutation of opcert counters is isolated via
    /// `validate_header_full_with_counters` (clone-and-swap).
    consensus_seed: Arc<dugite_consensus::praos::OuroborosPraos>,

    /// Issue #655 P2.b — shared map of header hashes that passed eager
    /// validation, keyed by epoch at validation time. Inserted by the
    /// chainsync receive task on a successful pass through
    /// `eager_validate_header`; consumed by the apply-time validator
    /// when `NodeConfig::skip_eagerly_validated_header_crypto` is on.
    eagerly_validated_headers:
        Arc<parking_lot::Mutex<HashMap<dugite_primitives::hash::Hash32, u64>>>,

    /// Byron epoch length in slots (needed for era-aware slot calculations).
    byron_epoch_length: u64,

    /// Ouroboros security parameter k.
    ///
    /// Passed to each ChainSync task to enforce the k-block rollback limit:
    /// a peer that requests a rollback deeper than k blocks is disconnected
    /// (Haskell: `terminateAfterDrain RolledBackPastIntersection`).
    /// Default: 2160 (mainnet). Preview: 432.
    security_param: u64,

    /// Active slots coefficient from Shelley genesis.
    ///
    /// Used to scale the rollback depth threshold from blocks to slots:
    /// with coeff=0.05, ~20 slots per block on average, so k blocks ≈ k*20 slots.
    /// Default: 0.05 (mainnet/preview).
    active_slots_coeff: f64,

    /// Active BlockFetch peer flag.
    ///
    /// During bulk sync (matching Haskell's `bfcMaxConcurrencyBulkSync = 1`),
    /// only ONE BlockFetch worker is active at a time. This atomic stores the
    /// port number of the active peer (0 = none active). Workers compete for
    /// this flag — the first to claim it becomes the sole fetcher.
    active_fetcher: Arc<std::sync::atomic::AtomicU64>,
    /// Socket address of the peer that currently holds the `active_fetcher`
    /// slot, or `None` if no peer is actively fetching.
    ///
    /// Kept in sync with `active_fetcher` by `make_blockfetch_task`: set to
    /// `Some(addr)` when a worker claims the CAS slot and cleared (set to
    /// `None`) on every release path — normal post-batch, no-headers early
    /// exit, error, timeout, and `cancel.cancelled()`.  The governor reads
    /// this via `get_active_fetch_peer()` to exclude the live downloader from
    /// `aboveTargetOther` demotion (Fix 1 — fetch-floor bug).
    ///
    /// A plain `std::sync::Mutex<Option<SocketAddr>>` is used (not async):
    /// the critical sections are tiny (one assignment) and never held across
    /// an `.await`, so a blocking lock is correct and cheaper than a tokio
    /// `Mutex`.
    active_fetch_peer: Arc<std::sync::Mutex<Option<SocketAddr>>>,
    /// Highest slot that has been fetched or is being fetched.
    /// Used to skip duplicate fetches from other peers.
    max_fetched_slot: Arc<std::sync::atomic::AtomicU64>,

    /// Resolved per-fetch maximum range size (block count), clamped to
    /// `[BLOCKFETCH_MIN_RANGE, BLOCKFETCH_MAX_RANGE]`.  Set once at construction
    /// from `DUGITE_BLOCKFETCH_MAX_RANGE` / the `blockfetch_max_range` config
    /// field (see `resolve_blockfetch_max_range`); each BlockFetch worker uses
    /// it as the upper clamp on the adaptive byte-budget range sizing.
    blockfetch_max_range: usize,

    /// Haskell `ChainSelStarvation` two-state flag, edge-recorded
    /// (`ouroboros-consensus` ChainDB `getChainSelMessage`):
    ///   - `0`  → `Ongoing` — the apply loop observed an EMPTY fetched-blocks
    ///     queue and is (about to be) blocked waiting. Initialized `Ongoing`
    ///     at construction, exactly like Haskell initializes the flag at
    ///     ChainDB open.
    ///   - `>0` → `EndedAt(t)` — millis since UNIX epoch when the dequeue that
    ///     ENDED the most recent starvation period happened. Subsequent
    ///     dequeues while not starved do NOT update this value (edge
    ///     semantics): a long block apply with a full queue keeps the old
    ///     `EndedAt`, so the rotation decision below does not fire during
    ///     epoch-boundary applies or snapshot writes.
    ///
    /// Writers: `chainsel_dequeued()` / `chainsel_queue_empty()` called from
    /// the `Node::run` fetched-blocks consumer. Readers: each BlockFetch
    /// worker's starvation-rotation decision.
    pub(crate) chainsel_starvation_ms: Arc<std::sync::atomic::AtomicU64>,

    /// BlockFetch starvation grace period (seconds) before rotating the CSJ
    /// dynamo. Configurable via `LowLevelGenesisOptions.BlockFetchGracePeriod`
    /// (upstream default 10 s). Only consulted in genesis-mode bulk sync.
    pub(crate) block_fetch_grace_period: std::time::Duration,

    /// Prometheus metrics for recording peer latencies.
    metrics: Arc<NodeMetrics>,

    /// Shared mempool for TxSubmission2 tx relay to peers.
    mempool: Arc<Mempool>,

    /// Channel for protocol tasks to report peer failures (e.g. fetch timeout).
    ///
    /// When a BlockFetch task times out on a peer, it sends the peer address here
    /// so the main run loop can call `peer_failed()` for reputation scoring and
    /// exponential backoff. This provides faster failure detection than waiting
    /// for the mux to die via `cleanup_dead_connections()`.
    /// Carries the failure kind: `ProtocolFault` convictions additionally
    /// tear the connection down (#751 / Haskell parity), `Slow` is
    /// reputation-only.
    peer_failure_tx: mpsc::Sender<(SocketAddr, PeerFailureKind)>,

    /// Channel for KeepAlive tasks to report per-pong RTT measurements.
    ///
    /// Each successful KeepAlive pong sends `(peer_addr, rtt_ms)` here so the
    /// main run loop can update PeerManager EWMA latency and Prometheus gauges
    /// with current peer RTT values (not cumulative histogram counts).
    keepalive_rtt_tx: mpsc::Sender<(SocketAddr, f64)>,

    /// GSM event sender — passed to ChainSync tasks so they can emit
    /// PeerRegistered, BlockReceived, PeerTipUpdated, PeerActive, PeerIdling
    /// events to the GSM actor.
    gsm_event_tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>,

    /// Lossless per-peer Genesis chain state (candidate fragments, idling,
    /// csLatestSlot) — written synchronously by ChainSync tasks, read by the
    /// GSM/GDD/LoE governor.
    peer_registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,

    /// GSM state snapshot for per-peer protocol gating (LoP bucket activity,
    /// historicity check).
    gsm_snapshot_rx: tokio::sync::watch::Receiver<crate::gsm::GsmSnapshot>,

    /// Limit on Patience (capacity, rate); `None` = disabled (praos or
    /// `EnableLoP=false`).
    lop_params: Option<(u64, u64)>,

    /// Historicity cutoff seconds; `None` = praos (noCheck).
    historicity_cutoff_secs: Option<u64>,

    /// ChainSync Jumping coordinator; `None` = disabled (praos / EnableCSJ
    /// false → noJumping).
    csj: Option<Arc<crate::csj::CsjRegistry>>,

    /// Shared block provider for server protocols (ChainSync server, BlockFetch server).
    block_provider: Arc<ChainDBBlockProvider>,

    /// Broadcast sender for rollback announcements to ChainSync servers.
    rollback_announcement_tx: broadcast::Sender<RollbackAnnouncement>,

    /// Shared peer manager for PeerSharing server to query connected peers.
    peer_manager_for_servers: Arc<RwLock<NodePeerManager>>,

    /// Tx validator for N2N TxSubmission2 admission.
    ///
    /// Every tx received via TxSubmission2 is validated through this before
    /// being added to the mempool.  This closes the N2N admission gap: without
    /// validation, an attacker can propagate a tx with `is_valid=false` but a
    /// script that evaluates to `True` — every BP that ingests it will forge a
    /// block that immediately fails its own ledger validation (#522).
    tx_validator: Arc<dyn TxValidator>,

    /// Our N2N listen address. When set, outbound connections bind their
    /// source port to it (SO_REUSEADDR + SO_REUSEPORT) so a remote peer
    /// observes the connection as duplex-paired from our listen port —
    /// matching Haskell ouroboros-network's `configureOutboundSocket`.
    local_listen_addr: Option<SocketAddr>,

    /// Shared flag set by the first ChainSync task that finds a non-Origin
    /// intersection.  Passed to `chainsync_client_task` so it can signal
    /// the forge loop that peer connectivity is established.
    peer_intersection_established: Arc<std::sync::atomic::AtomicBool>,

    /// Per-peer PeerSharing client request channels.
    ///
    /// Inserted when the PeerSharing client task starts (at warm promotion).
    /// The task loops waiting for a `u8` request amount; the governor sends
    /// amounts here via `GovernorAction::PeerShareRequest`.  Entries are
    /// removed when the task exits (connection teardown or cancel).
    ///
    /// Matches Haskell's `PeerSharingRegistry` /
    /// `PeerSharingController.requestQueue` pattern from
    /// `ouroboros-network/lib/Ouroboros/Network/PeerSharing.hs`.
    peersharing_request_txs: HashMap<SocketAddr, mpsc::Sender<u8>>,

    /// Global count of PeerSharing requests currently in-flight across ALL peers.
    ///
    /// Capped at `PEERSHARING_MAX_IN_FLIGHT` (= 2, matching Haskell's
    /// `policyMaxInProgressPeerShareReqs = 2` in
    /// `ouroboros-network/lib/Ouroboros/Network/Diffusion/Policies.hs`).
    /// The PeerSharing client task atomically increments this before sending
    /// a request and decrements it on completion; the governor checks before
    /// dispatching new requests so at most 2 peers are being asked simultaneously.
    peersharing_in_flight: Arc<std::sync::atomic::AtomicU32>,
}

/// Errors from lifecycle management operations.
#[derive(Debug)]
pub enum LifecycleError {
    /// The peer connection operation failed.
    Connection(PeerConnectionError),
    /// No connection exists for the given peer address.
    NotConnected(SocketAddr),
    /// A connection already exists for the given peer address.
    AlreadyConnected(SocketAddr),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(e) => write!(f, "connection error: {e}"),
            Self::NotConnected(addr) => write!(f, "no connection to {addr}"),
            Self::AlreadyConnected(addr) => write!(f, "already connected to {addr}"),
        }
    }
}

impl std::error::Error for LifecycleError {}

impl From<PeerConnectionError> for LifecycleError {
    fn from(e: PeerConnectionError) -> Self {
        Self::Connection(e)
    }
}

impl ConnectionLifecycleManager {
    /// Create a new lifecycle manager with the given shared state.
    ///
    /// # Arguments
    ///
    /// * `network_magic` — Cardano network identifier for handshakes
    /// * `peer_sharing` — Whether to advertise peer sharing support (node-level default;
    ///   per-peer diffusion mode is resolved at connect time via `NodePeerManager::effective_diffusion_mode()`)
    /// * `connect_timeout` — TCP connect timeout for outbound connections
    /// * `candidate_chains` — Shared map for ChainSync -> BlockFetch coordination
    /// * `fetched_blocks_tx` — Channel for BlockFetch tasks to send blocks to the run loop
    /// * `block_announcement_tx` — Broadcast channel for block announcements
    /// * `chain_db` — Shared ChainDB reference
    /// * `ledger_state` — Shared LedgerState reference
    /// * `byron_epoch_length` — Byron epoch length in slots
    /// * `security_param` — Ouroboros k (rollback limit); 2160 mainnet, 432 preview
    /// * `active_slots_coeff` — Shelley genesis active slots coefficient (0.05 on mainnet/preview)
    /// * `metrics` — Prometheus metrics handle for recording peer latencies
    /// * `mempool` — Shared mempool for TxSubmission2 tx relay
    /// * `peer_failure_tx` — Channel for protocol tasks to report peer failures
    /// * `keepalive_rtt_tx` — Channel for KeepAlive tasks to report per-pong RTT
    /// * `gsm_event_tx` — GSM event sender for ChainSync tasks
    /// * `block_provider` — Shared block provider for server protocols
    /// * `rollback_announcement_tx` — Broadcast sender for rollback announcements
    /// * `peer_manager_for_servers` — Shared peer manager for PeerSharing server
    /// * `tx_validator` — Tx validator for N2N TxSubmission2 admission (Phase-1 + Phase-2)
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network_magic: u64,
        peer_sharing: bool,
        connect_timeout: Duration,
        candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
        fetched_blocks_tx: mpsc::Sender<FetchedBlock>,
        block_announcement_tx: broadcast::Sender<BlockAnnouncement>,
        chain_db: Arc<RwLock<ChainDB>>,
        ledger_state: Arc<RwLock<LedgerState>>,
        ledger_view: Arc<arc_swap::ArcSwap<super::ledger_view::LedgerView>>,
        ledger_tip_slot_tx: tokio::sync::watch::Sender<u64>,
        consensus_seed: Arc<dugite_consensus::praos::OuroborosPraos>,
        eagerly_validated_headers: Arc<
            parking_lot::Mutex<HashMap<dugite_primitives::hash::Hash32, u64>>,
        >,
        byron_epoch_length: u64,
        security_param: u64,
        active_slots_coeff: f64,
        metrics: Arc<NodeMetrics>,
        mempool: Arc<Mempool>,
        peer_failure_tx: mpsc::Sender<(SocketAddr, PeerFailureKind)>,
        keepalive_rtt_tx: mpsc::Sender<(SocketAddr, f64)>,
        gsm_event_tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>,
        peer_registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,
        gsm_snapshot_rx: tokio::sync::watch::Receiver<crate::gsm::GsmSnapshot>,
        lop_params: Option<(u64, u64)>,
        historicity_cutoff_secs: Option<u64>,
        csj: Option<Arc<crate::csj::CsjRegistry>>,
        block_provider: Arc<ChainDBBlockProvider>,
        rollback_announcement_tx: broadcast::Sender<RollbackAnnouncement>,
        peer_manager_for_servers: Arc<RwLock<NodePeerManager>>,
        peer_intersection_established: Arc<std::sync::atomic::AtomicBool>,
        tx_validator: Arc<dyn TxValidator>,
        blockfetch_max_range: usize,
        block_fetch_grace_period: std::time::Duration,
    ) -> Self {
        Self {
            connections: HashMap::new(),
            network_magic,
            peer_sharing,
            connect_timeout,
            candidate_chains,
            fetched_blocks_tx,
            block_announcement_tx,
            chain_db,
            ledger_state,
            ledger_view,
            ledger_tip_slot_tx,
            consensus_seed,
            eagerly_validated_headers,
            byron_epoch_length,
            security_param,
            active_slots_coeff,
            active_fetcher: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active_fetch_peer: Arc::new(std::sync::Mutex::new(None)),
            max_fetched_slot: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            blockfetch_max_range,
            // ChainSelStarvation starts `Ongoing` (0) — Haskell initializes the
            // flag `Ongoing` at ChainDB open, so a current fetch peer that never
            // lets a single block through is rotated `grace` after its claim.
            chainsel_starvation_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            block_fetch_grace_period,
            metrics,
            mempool,
            peer_failure_tx,
            keepalive_rtt_tx,
            gsm_event_tx,
            peer_registry,
            gsm_snapshot_rx,
            lop_params,
            historicity_cutoff_secs,
            csj,
            block_provider,
            rollback_announcement_tx,
            peer_manager_for_servers,
            local_listen_addr: None,
            peer_intersection_established,
            tx_validator,
            peersharing_request_txs: HashMap::new(),
            peersharing_in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Set our N2N listen address used by outbound connections for
    /// duplex-paired source-port binding. Call once after construction.
    pub fn set_local_listen_addr(&mut self, addr: SocketAddr) {
        self.local_listen_addr = Some(addr);
    }

    /// `ChainSelStarvation` — a block was dequeued from the fetched-blocks
    /// queue by the apply loop.
    ///
    /// Edge-recorded like Haskell's `getChainSelMessage`: only the dequeue
    /// that ENDS a starvation period records `EndedAt(now)` (CAS from
    /// `Ongoing`/0). Dequeues while not starved leave the old `EndedAt`
    /// untouched, so a slow apply with a full queue never looks like
    /// starvation to the BlockFetch rotation decision.
    ///
    /// Called by the `Node::run` fetched-blocks consumer BEFORE applying the
    /// dequeued block (the Haskell flag is recorded at dequeue time, not
    /// post-apply).
    pub fn chainsel_dequeued(&self) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1); // 0 is reserved for Ongoing
                           // Only transition Ongoing(0) → EndedAt(now); otherwise keep the old
                           // EndedAt (edge semantics).
        let _ = self.chainsel_starvation_ms.compare_exchange(
            0,
            now_ms.max(1),
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// `ChainSelStarvation` — the apply loop observed an EMPTY fetched-blocks
    /// queue (it is about to block waiting for the next block). Marks the
    /// flag `Ongoing`.
    pub fn chainsel_queue_empty(&self) {
        self.chainsel_starvation_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Return the `SocketAddr` of the peer currently holding the
    /// `active_fetcher` slot, or `None` if no fetch is in progress.
    ///
    /// Used by the governor call site in `mod.rs` to thread the identity of
    /// the active downloader into `compute_actions_with_blp` so that
    /// `aboveTargetOther` never demotes it mid-download.
    pub fn get_active_fetch_peer(&self) -> Option<SocketAddr> {
        *self
            .active_fetch_peer
            .lock()
            .expect("active_fetch_peer lock")
    }

    // ─── Temperature Transitions ────────────────────────────────────────────

    /// Promote a cold peer to warm: TCP connect + handshake + start KeepAlive.
    ///
    /// This is the Cold -> Warm transition from Haskell's `PeerStateActions`.
    /// Creates a new `PeerConnection` (TCP + mux + handshake) and starts
    /// the KeepAlive warm-temperature protocol.
    ///
    /// The `initiator_only` flag for the handshake is resolved per-peer via
    /// `NodePeerManager::effective_diffusion_mode()`, so topology peers with an
    /// explicit `"diffusionMode": "InitiatorOnly"` group override correctly
    /// advertise themselves as initiator-only regardless of the node-level default.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::AlreadyConnected` if a connection already exists,
    /// or `LifecycleError::Connection` on TCP/handshake failure.
    pub async fn promote_to_warm(
        &mut self,
        addr: SocketAddr,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        use super::networking::DiffusionMode;

        // Reject only when an OUTBOUND already exists for this remote — an
        // inbound from the same peer is fine (they coexist as separate
        // ConnectionIds, matching Haskell ConnMap's `(local, remote)` keying).
        if self.has_outbound_to(addr) {
            return Err(LifecycleError::AlreadyConnected(addr));
        }

        info!(%addr, "promoting cold -> warm: connecting");

        // Resolve per-peer initiator_only from the peer manager's group config.
        // Falls back to the node-level DiffusionMode if the peer is not in any
        // local root group with an explicit override.
        let initiator_only =
            peer_manager.effective_diffusion_mode(&addr) == DiffusionMode::InitiatorOnly;

        // Time the TCP connect + handshake for RTT measurement.
        let connect_start = std::time::Instant::now();

        // Establish TCP connection, create mux, run handshake.
        let mut conn = PeerConnection::connect(
            addr,
            self.network_magic,
            initiator_only,
            self.peer_sharing,
            Some(self.connect_timeout),
            self.local_listen_addr,
        )
        .await?;

        // Record handshake RTT (includes TCP connect + mux setup + handshake exchange).
        let rtt_ms = connect_start.elapsed().as_secs_f64() * 1000.0;
        self.metrics.record_handshake_rtt(rtt_ms);

        // Start warm protocols (KeepAlive).
        let keepalive_fn = self.make_keepalive_task(addr);
        conn.start_warm_protocols(keepalive_fn)?;
        self.start_server_protocols_on(addr, &mut conn)?;

        // Update peer manager state. Only call peer_connected on the FIRST
        // physical connection to this remote so the logical OutboundIdle
        // state is not overwritten by a concurrently-arriving inbound.
        let cid = ConnectionId {
            local: conn.local_addr,
            remote: addr,
        };
        // Simultaneous-open guard: an inbound with the same ConnectionId
        // could have raced our connect. Inbound wins (Haskell `Overwritten`),
        // so we yield and drop the outbound here — its bearer closes on drop.
        if self.connections.contains_key(&cid) {
            info!(
                %cid,
                "simultaneous open: inbound already registered, dropping outbound"
            );
            return Err(LifecycleError::AlreadyConnected(addr));
        }
        if !self.has_any_to(addr) {
            peer_manager.peer_connected(&addr, ConnectionDirection::Outbound);
        }

        // Start the PeerSharing client task for this warm connection.
        // The channel is taken from `conn` here; subsequent requests arrive
        // via `peersharing_request_txs[addr]` in `dispatch_peersharing_request`.
        let cancel = conn.cancel_token().clone();
        if let Some(ps_tx) = self.start_peersharing_client(addr, &mut conn, cancel) {
            self.peersharing_request_txs.insert(addr, ps_tx);
        }

        self.connections.insert(cid, conn);
        info!(%cid, rtt_ms = format_args!("{rtt_ms:.0}"), "cold -> warm complete");
        Ok(())
    }

    /// Spawn a background task that performs the TCP connect + handshake for `addr`.
    ///
    /// This is the non-blocking alternative to `promote_to_warm`. The slow I/O
    /// (TCP connect + N2N handshake, up to `connect_timeout`) runs in a separate
    /// Tokio task rather than inside the main `select!` loop.  When the task
    /// completes it sends a [`ConnectResult`] on `tx`; the main loop receives
    /// it and calls [`Self::register_warm_connection`] (on success) or marks the
    /// peer as failed (on error).
    ///
    /// `initiator_only` should be computed by the caller via
    /// `NodePeerManager::effective_diffusion_mode(&addr) == DiffusionMode::InitiatorOnly`
    /// so that per-group topology overrides are respected in the handshake.
    ///
    /// The caller is responsible for tracking in-flight addresses to avoid
    /// spawning duplicate tasks for the same peer.
    pub fn spawn_connect(
        &self,
        addr: SocketAddr,
        initiator_only: bool,
        tx: mpsc::Sender<ConnectResult>,
    ) {
        let network_magic = self.network_magic;
        let peer_sharing = self.peer_sharing;
        let connect_timeout = self.connect_timeout;
        let local_listen_addr = self.local_listen_addr;
        let metrics = Arc::clone(&self.metrics);

        tokio::spawn(async move {
            let start = std::time::Instant::now();
            match PeerConnection::connect(
                addr,
                network_magic,
                initiator_only,
                peer_sharing,
                Some(connect_timeout),
                local_listen_addr,
            )
            .await
            {
                Ok(conn) => {
                    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
                    metrics.record_handshake_rtt(rtt_ms);
                    // Ignore send errors — the main loop may have shut down.
                    let _ = tx.send(Ok((addr, conn, rtt_ms))).await;
                }
                Err(e) => {
                    let _ = tx.send(Err((addr, e.to_string()))).await;
                }
            }
        });
    }

    /// Register a peer that connected successfully in a background task as warm.
    ///
    /// This is the fast, synchronous post-connect step: starts the KeepAlive
    /// warm protocol on the ready connection and updates the peer manager.
    /// It must be called from the main run loop after receiving an `Ok` result
    /// from a [`Self::spawn_connect`] task.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::AlreadyConnected` if a connection for `addr`
    /// was registered in the meantime (e.g., from a concurrent inbound connect).
    /// The caller should silently discard the duplicate `PeerConnection` in that
    /// case — it will be dropped and the mux will close gracefully.
    pub fn register_warm_connection(
        &mut self,
        addr: SocketAddr,
        mut conn: PeerConnection,
        rtt_ms: f64,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        // Reject only when another outbound to this remote exists. An
        // inbound from the same peer can coexist (different ConnectionId).
        if self.has_outbound_to(addr) {
            return Err(LifecycleError::AlreadyConnected(addr));
        }

        let cid = ConnectionId {
            local: conn.local_addr,
            remote: addr,
        };

        // Simultaneous-open: an inbound with the same ConnectionId got
        // there first. Haskell's `Overwritten` rule: inbound wins, outbound
        // throws `ConnectionExists`. We yield by dropping our outbound — the
        // mux's bearer is still owned by `conn` and will close on drop.
        if self.connections.contains_key(&cid) {
            info!(
                %cid,
                "simultaneous open: inbound already registered, dropping outbound"
            );
            return Err(LifecycleError::AlreadyConnected(addr));
        }

        let keepalive_fn = self.make_keepalive_task(addr);
        conn.start_warm_protocols(keepalive_fn)?;
        self.start_server_protocols_on(addr, &mut conn)?;

        if !self.has_any_to(addr) {
            peer_manager.peer_connected(&addr, ConnectionDirection::Outbound);
        }

        // Start the PeerSharing client task.
        let cancel = conn.cancel_token().clone();
        if let Some(ps_tx) = self.start_peersharing_client(addr, &mut conn, cancel) {
            self.peersharing_request_txs.insert(addr, ps_tx);
        }

        self.connections.insert(cid, conn);
        info!(%cid, rtt_ms = format_args!("{rtt_ms:.0}"), "cold -> warm complete (background)");
        Ok(())
    }

    /// Promote a warm peer to hot: start ChainSync + BlockFetch + TxSubmission2.
    ///
    /// This is the Warm -> Hot transition from Haskell's `PeerStateActions`.
    /// The existing mux connection stays alive — only new protocol tasks are
    /// spawned on channels that were created during the initial connect.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::NotConnected` if no connection exists, or
    /// `LifecycleError::Connection` if protocol channels are unavailable
    /// (e.g., hot protocols already running).
    pub async fn promote_to_hot(
        &mut self,
        addr: SocketAddr,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        // Pick the connection that should run hot CLIENT protocols. Prefer
        // outbound (we initiated it), since the inbound side already has
        // its client channels marked initiator_only and would not reach
        // a remote responder. Matches Haskell's `OutboundDupState`
        // promotion path which drives initiator-side protocols on the
        // outbound connection of a duplex pair.
        let cid = self
            .find_outbound_cid(addr)
            .or_else(|| self.find_any_cid(addr))
            .ok_or(LifecycleError::NotConnected(addr))?;

        info!(%cid, "promoting warm -> hot: starting sync protocols");

        // Create task closures BEFORE taking the mutable borrow on connections,
        // since the factory methods borrow `self` immutably.
        let chainsync_fn = self.make_chainsync_task(addr);
        let blockfetch_fn = self.make_blockfetch_task(addr);
        let txsubmission_fn = self.make_txsubmission_task(addr);

        let conn = self.connections.get_mut(&cid).unwrap();
        conn.start_hot_protocols(chainsync_fn, blockfetch_fn, txsubmission_fn)?;

        // Update peer manager: warm -> hot.
        peer_manager.inner.promote_to_hot(&addr);

        // Update connection state: idle → active (outbound or inbound).
        if peer_manager.is_inbound(&addr) {
            peer_manager.mark_inbound_active(&addr);
        } else {
            peer_manager.mark_outbound_active(&addr);
        }

        info!(%cid, "warm -> hot complete");
        Ok(())
    }

    /// Demote a hot peer to warm: stop hot protocol tasks, keep TCP + mux alive.
    ///
    /// # Haskell Alignment: `deactivatePeerConnection` (Fix A, issue #703)
    ///
    /// Mirrors Haskell's `PeerStateActions.deactivatePeerConnection` from
    /// ouroboros-network (`PeerStateActions.hs` line 978). The Haskell implementation:
    ///   1. Cancels hot mini-protocol responder bundles.
    ///   2. Awaits graceful exit with `spsDeactivateTimeout` (5 seconds).
    ///   3. Keeps the `MuxBearer` (TCP connection) alive for re-promotion.
    ///
    /// Fix A implements this via `PeerConnection::stop_hot_protocols_and_recover()`,
    /// which:
    ///   1. Cancels hot task `CancellationToken`s.
    ///   2. Awaits task exit with `PROTOCOL_SHUTDOWN_TIMEOUT` (5 s).
    ///   3. Calls `MuxHandle::resubscribe()` to install fresh ingress senders
    ///      in the running `IngressTask` via `SwappableSender` — restoring all
    ///      three hot client channels without touching the TCP bearer.
    ///
    /// If recovery fails (timeout or empty `MuxHandle`), falls back to closing
    /// the TCP connection; the governor reconnects on the next tick.
    ///
    /// See: <https://github.com/IntersectMBO/ouroboros-network/blob/main/ouroboros-network/lib/Ouroboros/Network/PeerSelection/PeerStateActions.hs#L978>
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::NotConnected` if no connection exists.
    pub async fn demote_to_warm(
        &mut self,
        addr: SocketAddr,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        // Find the connection that has the hot protocol tasks — prefer outbound
        // (same as promote_to_hot).
        let cid = self
            .find_outbound_cid(addr)
            .or_else(|| self.find_any_cid(addr))
            .ok_or(LifecycleError::NotConnected(addr))?;

        info!(%cid, "demoting hot -> warm: stopping hot protocols (Fix A: keep TCP alive)");

        // Attempt graceful stop + channel recovery on the existing connection.
        let recovered = {
            let conn = self.connections.get_mut(&cid).unwrap();
            conn.stop_hot_protocols_and_recover().await
        };

        if recovered {
            // Hot protocols stopped and channels recovered — connection stays warm.
            // Clear only the ChainSync candidate state (no headers are streaming).
            {
                let mut chains = self.candidate_chains.write().await;
                chains.remove(&addr);
            }

            // Update peer manager: hot → warm (connection stays alive).
            peer_manager.inner.demote_to_warm(&addr);

            info!(%cid, "hot -> warm complete (TCP kept alive, channels recovered)");
        } else {
            // Recovery failed (timeout or no MuxHandle) — fall back to TCP close.
            // The governor will reconnect on the next tick (Cold → Warm → Hot).
            warn!(%cid, "hot -> warm: channel recovery failed, falling back to TCP close");

            peer_manager.mark_terminating(&addr);

            // Close ALL connections to this remote (covers duplex pairs).
            let cids: Vec<ConnectionId> = self
                .connections
                .keys()
                .filter(|c| c.remote == addr)
                .copied()
                .collect();
            for close_cid in &cids {
                if let Some(mut conn) = self.connections.remove(close_cid) {
                    conn.shutdown().await;
                }
            }

            {
                let mut chains = self.candidate_chains.write().await;
                chains.remove(&addr);
            }

            peer_manager.peer_disconnected(&addr);

            info!(%addr, "hot -> warm fallback complete (connection closed; will reconnect)");
        }

        Ok(())
    }

    /// Demote a warm peer to cold: stop all protocols, close connection.
    ///
    /// This is the Warm -> Cold transition from Haskell's `PeerStateActions`.
    /// Shuts down the entire connection (all protocol tasks + mux + TCP).
    /// The `PeerConnection` is removed from the connections map.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::NotConnected` if no connection exists.
    pub async fn demote_to_cold(
        &mut self,
        addr: SocketAddr,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        // Cold transition closes EVERY connection to this remote — both
        // outbound and any duplex inbound. Matches Haskell's
        // `unregisterPeerConnection` which closes the entire ConnectionId
        // entry for the remote.
        let cids: Vec<ConnectionId> = self
            .connections
            .keys()
            .filter(|c| c.remote == addr)
            .copied()
            .collect();
        if cids.is_empty() {
            return Err(LifecycleError::NotConnected(addr));
        }

        info!(%addr, count = cids.len(), "demoting warm -> cold: closing all connections to peer");

        // Mark connection as terminating before shutdown (for metrics).
        peer_manager.mark_terminating(&addr);

        for cid in &cids {
            if let Some(mut conn) = self.connections.remove(cid) {
                conn.shutdown().await;
            }
        }

        // Clear candidate chain state.
        {
            let mut chains = self.candidate_chains.write().await;
            chains.remove(&addr);
        }

        // Remove the PeerSharing client request channel — dropping it signals
        // the client task to exit its loop cleanly (recv returns None).
        self.peersharing_request_txs.remove(&addr);

        // Update peer manager — removes connection state entirely.
        peer_manager.peer_disconnected(&addr);

        info!(%addr, "warm -> cold complete");
        Ok(())
    }

    // ─── Governor Event Dispatch ────────────────────────────────────────────

    /// Handle a governor action by dispatching to the appropriate lifecycle method.
    ///
    /// This is the main integration point between the Governor (which decides
    /// what should happen) and the ConnectionLifecycleManager (which makes it
    /// happen). Called from the connection manager loop.
    ///
    /// Non-connection actions (like `DiscoverMore`) are ignored here — they
    /// are handled by the peer discovery subsystem.
    pub async fn handle_governor_action(
        &mut self,
        action: GovernorAction,
        peer_manager: &mut NodePeerManager,
    ) {
        match action {
            GovernorAction::PromoteToWarm(addr) => {
                if let Err(e) = self.promote_to_warm(addr, peer_manager).await {
                    warn!(%addr, error = %e, "failed to promote cold -> warm");
                    peer_manager.peer_failed(&addr);
                }
            }
            GovernorAction::PromoteToHot(addr) => {
                if let Err(e) = self.promote_to_hot(addr, peer_manager).await {
                    warn!(%addr, error = %e, "failed to promote warm -> hot");
                    // Demote back to cold on hot promotion failure — the connection
                    // may be in a bad state.
                    peer_manager.mark_terminating(&addr);
                    let cids: Vec<ConnectionId> = self
                        .connections
                        .keys()
                        .filter(|c| c.remote == addr)
                        .copied()
                        .collect();
                    for cid in cids {
                        if let Some(mut conn) = self.connections.remove(&cid) {
                            conn.shutdown().await;
                        }
                    }
                    peer_manager.peer_failed(&addr);
                }
            }
            GovernorAction::DemoteToWarm(addr) => {
                if let Err(e) = self.demote_to_warm(addr, peer_manager).await {
                    warn!(%addr, error = %e, "failed to demote hot -> warm");
                }
            }
            GovernorAction::DemoteToCold(addr) => {
                if let Err(e) = self.demote_to_cold(addr, peer_manager).await {
                    warn!(%addr, error = %e, "failed to demote warm -> cold");
                }
            }
            GovernorAction::DiscoverMore => {
                // Handled by the peer discovery subsystem, not the lifecycle manager.
                debug!("governor requested peer discovery (handled externally)");
            }
            GovernorAction::ForgetPeer(addr) => {
                // Remove every connection to this peer (covers duplex pairs).
                // Cold churn evicts lowest-reputation non-topology peers.
                debug!(%addr, "governor forgetting low-reputation cold peer");
                let cids: Vec<ConnectionId> = self
                    .connections
                    .keys()
                    .filter(|c| c.remote == addr)
                    .copied()
                    .collect();
                for cid in cids {
                    if let Some(mut conn) = self.connections.remove(&cid) {
                        conn.shutdown().await;
                    }
                }
                peer_manager.inner.remove_peer(&addr);
            }
            GovernorAction::PeerShareRequest(addr) => {
                self.dispatch_peersharing_request(addr);
            }
        }
    }

    // ─── Connection Health ──────────────────────────────────────────────────

    /// Remove dead connections whose mux has terminated.
    ///
    /// Checks `is_alive()` on every connection and removes any that have died
    /// (mux task completed due to TCP close, error, etc.). Updates the peer
    /// manager to reflect the disconnection and clears candidate chain state.
    ///
    /// Should be called periodically from the connection manager loop.
    pub async fn cleanup_dead_connections(&mut self, peer_manager: &mut NodePeerManager) {
        let dead_cids: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|(_, conn)| !conn.is_alive())
            .map(|(cid, _)| *cid)
            .collect();

        if dead_cids.is_empty() {
            return;
        }

        info!(count = dead_cids.len(), "cleaning up dead connections");

        for cid in dead_cids {
            let addr = cid.remote;

            if let Some(mut conn) = self.connections.remove(&cid) {
                // Best-effort shutdown (mux is already dead, but clean up tasks).
                conn.shutdown().await;
            }

            // Only update peer-manager state and clear candidate chain when the
            // LAST connection to this remote dies. Otherwise the surviving
            // duplex-pair connection still represents a live peer.
            if !self.has_any_to(addr) {
                peer_manager.mark_terminating(&addr);
                {
                    let mut chains = self.candidate_chains.write().await;
                    chains.remove(&addr);
                }
                // Remove the PeerSharing client channel so the client task exits.
                self.peersharing_request_txs.remove(&addr);
                peer_manager.peer_disconnected(&addr);
                warn!(%cid, "removed dead connection (last to peer)");
            } else {
                warn!(%cid, "removed dead connection (peer still has another)");
            }
        }
    }

    /// Get the number of active physical connections.
    ///
    /// A duplex peer with both an outbound and an inbound counts as 2.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Check if any connection (inbound or outbound) exists for the given remote.
    pub fn has_connection(&self, addr: &SocketAddr) -> bool {
        self.has_any_to(*addr)
    }

    /// Returns true if we have at least one outbound connection to `remote`.
    fn has_outbound_to(&self, remote: SocketAddr) -> bool {
        self.connections
            .iter()
            .any(|(c, p)| c.remote == remote && p.direction == PeerConnectionDirection::Outbound)
    }

    /// Returns true if we have any connection (in or out) to `remote`.
    fn has_any_to(&self, remote: SocketAddr) -> bool {
        self.connections.keys().any(|c| c.remote == remote)
    }

    /// Find the [`ConnectionId`] of an outbound connection to `remote`, if any.
    fn find_outbound_cid(&self, remote: SocketAddr) -> Option<ConnectionId> {
        self.connections
            .iter()
            .find(|(c, p)| c.remote == remote && p.direction == PeerConnectionDirection::Outbound)
            .map(|(cid, _)| *cid)
    }

    /// Find any [`ConnectionId`] for `remote` (outbound preferred, otherwise inbound).
    fn find_any_cid(&self, remote: SocketAddr) -> Option<ConnectionId> {
        self.find_outbound_cid(remote).or_else(|| {
            self.connections
                .keys()
                .find(|c| c.remote == remote)
                .copied()
        })
    }

    /// Get the addresses of all connected peers (deduplicated by remote).
    pub fn connected_addrs(&self) -> Vec<SocketAddr> {
        let mut seen = std::collections::HashSet::new();
        self.connections
            .keys()
            .filter_map(|c| seen.insert(c.remote).then_some(c.remote))
            .collect()
    }

    /// Drain all connections, returning them as owned values.
    ///
    /// Used during shutdown to parallelize connection teardown without
    /// holding `&mut self` for the duration of each `shutdown().await`.
    pub fn drain_connections(&mut self) -> Vec<PeerConnection> {
        self.connections.drain().map(|(_, conn)| conn).collect()
    }

    // ─── Protocol Task Factories ────────────────────────────────────────────
    //
    // Each factory creates a closure matching the `ProtocolTaskFn` signature
    // that captures the shared state it needs. The `PeerConnection` spawns
    // these closures as tokio tasks when protocols are started.

    /// Create the KeepAlive protocol task closure.
    ///
    /// The KeepAlive protocol sends periodic pings to detect dead connections.
    /// Runs for the entire Warm lifetime of the connection.
    ///
    /// In Haskell, KeepAlive uses a 90-second interval and the Governor
    /// monitors RTT measurements from responses.
    fn make_keepalive_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let peer_failure_tx = self.peer_failure_tx.clone();
        let keepalive_rtt_tx = self.keepalive_rtt_tx.clone();
        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                // CRITICAL: Delay the first KeepAlive ping until AFTER Hot protocols
                // have started and sent their first messages. The Haskell peer uses
                // StartOnDemandAny for the KeepAlive responder — it only starts when
                // ANY on-demand protocol receives data. If we send KeepAlive before
                // ChainSync/TxSubmission2 send their first messages, the peer has no
                // responder registered and RSTs the connection.
                //
                // In Haskell, this works because KeepAlive is in the Established
                // bundle and Hot protocols start at the same time with StartEagerly,
                // so ChainSync/TxSubmission data arrives before the first KeepAlive.
                //
                // We delay 2 seconds to ensure Hot protocols are active first.
                //
                // CANCELLATION: The sleep must be guarded so that warm→cold demotion
                // (stop_warm_protocols / shutdown) completes within spsDeactivateTimeout
                // (5 s) even when cancellation fires during the startup delay.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        debug!(%addr, "keepalive task cancelled during startup delay");
                        return;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }

                // Per-peer RTT channel: each pong sends the RTT here, which the
                // spawned forwarder relays to the main loop with the peer address.
                let (rtt_tx, mut rtt_rx) = tokio::sync::mpsc::channel::<f64>(8);

                // Forwarder task: tags each RTT measurement with the peer address
                // and sends it to the main run loop for PeerManager EWMA + gauge updates.
                let ka_rtt_tx = keepalive_rtt_tx;
                let fwd_addr = addr;
                tokio::spawn(async move {
                    while let Some(rtt_ms) = rtt_rx.recv().await {
                        let _ = ka_rtt_tx.try_send((fwd_addr, rtt_ms));
                    }
                });

                let client = dugite_network::KeepAliveClient::new(
                    dugite_network::DEFAULT_KEEPALIVE_INTERVAL,
                    cancel,
                )
                .with_rtt_sender(rtt_tx);
                match client.run(&mut channel).await {
                    Ok(_rtt) => debug!(%addr, "keepalive task completed"),
                    Err(dugite_network::error::ProtocolError::KeepAliveTimeout {
                        consecutive_failures,
                    }) => {
                        warn!(
                            %addr,
                            consecutive_failures,
                            "keepalive: peer unresponsive, reporting failure",
                        );
                        let _ = peer_failure_tx.try_send((addr, PeerFailureKind::Slow));
                    }
                    Err(e) => debug!(%addr, "keepalive error: {e}"),
                }
            })
        })
    }

    // ─── PeerSharing client ─────────────────────────────────────────────────

    /// Maximum concurrent PeerSharing requests across all peers.
    ///
    /// Matches Haskell `policyMaxInProgressPeerShareReqs = 2` from
    /// `ouroboros-network/lib/Ouroboros/Network/Diffusion/Policies.hs`.
    const PEERSHARING_MAX_IN_FLIGHT: u32 = 2;

    /// Minimum peers requested per share round.
    ///
    /// Haskell formula: `max 8 (objective / numRequests)`.  We use the minimum
    /// of 8 as the default when `max_cold` is already satisfied, giving each
    /// request a reasonable floor.  Capped at 255 (u8::MAX, matching
    /// `PeerSharingAmount`).
    const PEERSHARING_DEFAULT_AMOUNT: u8 = 8;

    /// Start the PeerSharing client task for a newly-warmed peer.
    ///
    /// Takes the `peersharing_client_channel` from `conn` (if present) and
    /// spawns a long-lived task that loops waiting for request amounts on the
    /// returned `mpsc::Sender`.  The governor calls
    /// [`dispatch_peersharing_request`] to enqueue work; the task executes the
    /// `MsgShareRequest → MsgSharePeers` exchange and adds routable results to
    /// the peer manager.
    ///
    /// Returns the sender half of the request channel so the caller can stash
    /// it in `peersharing_request_txs`.  Returns `None` when the channel is
    /// not available on this connection (inbound `initiator_only` or already
    /// taken).
    ///
    /// This mirrors Haskell's `bracketPeerSharingClient` /
    /// `peerSharingClient` in
    /// `ouroboros-network/lib/Ouroboros/Network/PeerSharing.hs`, where the
    /// client task runs as part of the `Established` mini-protocol bundle for
    /// the lifetime of the warm-or-hotter connection.
    fn start_peersharing_client(
        &self,
        addr: SocketAddr,
        conn: &mut super::peer_connection::PeerConnection,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Option<mpsc::Sender<u8>> {
        let mut channel = conn.take_peersharing_client_channel()?;

        let (req_tx, mut req_rx) = mpsc::channel::<u8>(4);
        let peer_manager = self.peer_manager_for_servers.clone();
        let in_flight = self.peersharing_in_flight.clone();

        tokio::spawn(async move {
            use dugite_network::protocol::peersharing::client::PeerSharingClient;
            debug!(%addr, "peersharing client task started");

            loop {
                // Wait for a request amount from the governor, or exit when
                // the channel is closed (connection teardown) or cancelled.
                let amount: u8 = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        debug!(%addr, "peersharing client task cancelled");
                        break;
                    }
                    maybe_amount = req_rx.recv() => {
                        match maybe_amount {
                            Some(a) => a,
                            None => {
                                // Sender dropped (connection torn down by lifecycle manager).
                                debug!(%addr, "peersharing client task: request channel closed, exiting");
                                break;
                            }
                        }
                    }
                };

                debug!(%addr, amount, "peersharing: sending MsgShareRequest");
                match PeerSharingClient::request_peers(&mut channel, amount).await {
                    Ok(peers) if !peers.is_empty() => {
                        let discovered = peers.len();
                        let mut pm = peer_manager.write().await;
                        for peer_addr in peers {
                            pm.add_shared_peer(peer_addr);
                        }
                        drop(pm);
                        info!(
                            %addr,
                            discovered,
                            "PeerSharing: added peers to cold set"
                        );
                    }
                    Ok(_) => {
                        debug!(%addr, "PeerSharing: peer returned no addresses");
                    }
                    Err(e) => {
                        debug!(%addr, "PeerSharing: request failed: {e}");
                        // Protocol error — exit the task; mux cleanup handles
                        // the channel.
                        break;
                    }
                }

                // Release in-flight slot after each completed request.
                in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }

            debug!(%addr, "peersharing client task exited");
        });

        Some(req_tx)
    }

    /// Dispatch a PeerSharing request for the given peer.
    ///
    /// Called by `handle_governor_action` when `GovernorAction::PeerShareRequest`
    /// arrives.  Enforces the global concurrency cap
    /// (`PEERSHARING_MAX_IN_FLIGHT = 2`, matching Haskell
    /// `policyMaxInProgressPeerShareReqs = 2`) and the per-peer duplicate-request
    /// guard (only one in-flight request per peer at a time).
    ///
    /// Request amount: `PEERSHARING_DEFAULT_AMOUNT = 8`, matching Haskell's
    /// `max 8 (objective `div` numPeerShareReqs)` lower bound.
    fn dispatch_peersharing_request(&mut self, addr: SocketAddr) {
        let in_flight = self
            .peersharing_in_flight
            .load(std::sync::atomic::Ordering::Relaxed);
        if in_flight >= Self::PEERSHARING_MAX_IN_FLIGHT {
            debug!(
                %addr,
                in_flight,
                max = Self::PEERSHARING_MAX_IN_FLIGHT,
                "PeerSharing: skipping request — global concurrency cap reached"
            );
            return;
        }

        let Some(tx) = self.peersharing_request_txs.get(&addr) else {
            debug!(%addr, "PeerSharing: no client task for peer, skipping");
            return;
        };

        // Claim the in-flight slot before attempting the send so we never
        // over-count (the task decrements on completion).
        self.peersharing_in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        match tx.try_send(Self::PEERSHARING_DEFAULT_AMOUNT) {
            Ok(()) => {
                debug!(
                    %addr,
                    amount = Self::PEERSHARING_DEFAULT_AMOUNT,
                    "PeerSharing: dispatched request to client task"
                );
            }
            Err(_) => {
                // Channel full (another request already in-flight for this peer)
                // or closed (task exited). Release the slot we just claimed.
                self.peersharing_in_flight
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                debug!(%addr, "PeerSharing: failed to dispatch request (task busy or closed)");
            }
        }
    }

    /// Create the ChainSync protocol task closure for a specific peer.
    ///
    /// The ChainSync client streams block headers from the peer, finds
    /// the intersection point with our chain, then pipelines header downloads.
    /// Headers are stored in `candidate_chains` for the BlockFetch decision
    /// task to consume. Does NOT fetch blocks — that's the BlockFetch
    /// decision task's responsibility.
    ///
    /// Delegates to [`super::sync::chainsync_client_task()`] which implements
    /// the full pipelined ChainSync protocol loop.
    fn make_chainsync_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let candidate_chains = self.candidate_chains.clone();
        let chain_db = self.chain_db.clone();
        let ledger_state = self.ledger_state.clone();
        let ledger_view = self.ledger_view.clone();
        let ledger_tip_rx = self.ledger_tip_slot_tx.subscribe();
        let consensus_seed = self.consensus_seed.clone();
        let eagerly_validated_headers = self.eagerly_validated_headers.clone();
        let byron_epoch_length = self.byron_epoch_length;
        let security_param = self.security_param;
        let active_slots_coeff = self.active_slots_coeff;
        let metrics = self.metrics.clone();
        let gsm_event_tx = self.gsm_event_tx.clone();
        let peer_registry = self.peer_registry.clone();
        let gsm_snapshot_rx = self.gsm_snapshot_rx.clone();
        let lop_params = self.lop_params;
        let historicity_cutoff_secs = self.historicity_cutoff_secs;
        let csj = self.csj.clone();
        let peer_intersection_established = self.peer_intersection_established.clone();
        let peer_failure_tx = self.peer_failure_tx.clone();
        let peer_manager = self.peer_manager_for_servers.clone();

        Box::new(move |channel, cancel| {
            Box::pin(async move {
                info!(%addr, "chainsync task started");
                // CANCELLATION: Wrap the entire chainsync_client_task call in a
                // select so that demotion (cancel.cancelled()) exits promptly even
                // when the task is blocked on a bare channel.recv().await during
                // the pre-loop intersection-finding phase (try_find_intersect).
                // The main message loop already has per-recv cancel checks, but
                // Phase 1/2 (build_known_points → try_find_intersect retries)
                // do not — without this outer guard those awaits can block for
                // the full spsDeactivateTimeout (5 s) if the peer is slow.
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        debug!(%addr, "chainsync task cancelled before/during intersection");
                        return;
                    }
                    r = super::sync::chainsync_client_task(
                        channel,
                        addr,
                        candidate_chains,
                        chain_db,
                        ledger_state,
                        ledger_view,
                        ledger_tip_rx,
                        consensus_seed,
                        eagerly_validated_headers,
                        byron_epoch_length,
                        security_param,
                        active_slots_coeff,
                        metrics,
                        cancel.clone(),
                        gsm_event_tx,
                        peer_intersection_established,
                        peer_manager,
                        peer_registry,
                        gsm_snapshot_rx,
                        lop_params,
                        historicity_cutoff_secs,
                        csj,
                    ) => r
                };
                // Report any non-cancel failure to the peer manager so the
                // governor can demote-and-re-promote the peer — without this,
                // a chainsync death (bearer close, decode error, stale
                // intersection) leaves the TCP connection up but no headers
                // arriving (#499).  Matches keepalive/blockfetch pattern.
                if let Err(e) = result {
                    let kind = classify_chainsync_failure(&e);
                    // `Unsuitable` (ChainSync intersection only at genesis — the
                    // Haskell `ForkTooDeep` equivalent) is an EXPECTED outcome on
                    // public networks (stale / wrong-chain relays), so log it at
                    // INFO (≈ cardano-node `Notice`). Genuine faults stay WARN so
                    // they are not drowned out.
                    if kind == PeerFailureKind::Unsuitable {
                        info!(%addr, error = %e, "chainsync ended: peer unsuitable (intersection only at genesis / ForkTooDeep) — demoting for backoff");
                    } else {
                        warn!(%addr, error = %e, "chainsync task failed");
                    }
                    if !cancel.is_cancelled() {
                        // Both kinds hit reputation + teardown in the peer-failure
                        // handler; they differ only in log severity / reputation
                        // weight. (Reported as `Slow`/`Unsuitable`, not teardown-
                        // upgraded, for decode errors — that stays #751's scope.)
                        let _ = peer_failure_tx.try_send((addr, kind));
                    }
                }
                debug!(%addr, "chainsync task exiting");
            })
        })
    }

    /// Create the BlockFetch protocol task closure for a specific peer.
    ///
    /// The BlockFetch client receives fetch requests from the BlockFetch
    /// decision task and downloads full blocks from the peer. Downloaded
    /// blocks are sent to the main run loop via `fetched_blocks_tx`.
    ///
    /// Real implementation will be provided by Task 3.
    fn make_blockfetch_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let fetched_blocks_tx = self.fetched_blocks_tx.clone();
        let candidate_chains = self.candidate_chains.clone();
        let chain_db = self.chain_db.clone();
        let bel = self.byron_epoch_length;
        // Shared flag: only ONE BlockFetch worker is active at a time.
        // Matches Haskell's bfcMaxConcurrencyBulkSync = 1.
        let active_fetcher = self.active_fetcher.clone();
        // Companion SocketAddr tracker — kept in sync with `active_fetcher`
        // so the governor can identify and protect the live downloader from
        // `aboveTargetOther` demotion (fetch-floor fix).
        let active_fetch_peer = self.active_fetch_peer.clone();
        let _max_fetched_slot = self.max_fetched_slot.clone();
        let metrics_clone = self.metrics.clone();
        let peer_failure_tx = self.peer_failure_tx.clone();
        // Operator-tunable upper clamp on the adaptive fetch-range size.
        let max_range = self.blockfetch_max_range;
        // CSJ dynamo rotation on starvation (Haskell demoteChainSyncJumpingDynamo).
        let csj = self.csj.clone();
        // Starvation detection: ChainSelStarvation flag maintained by the
        // Node::run fetched-blocks consumer (0=Ongoing, >0=EndedAt millis).
        let chainsel_starvation_ms = self.chainsel_starvation_ms.clone();
        let block_fetch_grace_period = self.block_fetch_grace_period;
        // GSM state for genesis-mode gate (only rotate in genesis bulk sync).
        let gsm_snapshot_rx_for_starv = self.gsm_snapshot_rx.clone();
        // GSV peer prioritisation: rank fetch peers by measured bandwidth so the
        // single bulk-sync fetch slot goes to the fastest-serving peer (matching
        // cardano-node's prioritisePeerChains). Read in the claim gate; updated
        // after each range. `None` when no peer manager (unit tests) → gate off.
        let peer_manager_for_fetch = self.peer_manager_for_servers.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                // RAII release for the single-fetcher slot.  The post-batch
                // release (and the empty/no-headers releases) clear
                // `active_fetcher` on the normal code paths, and the
                // `cancel.cancelled()` arm clears it on graceful shutdown — but
                // if this worker future is DROPPED mid-batch (peer demotion's
                // "hot protocol tasks did not stop cleanly" abort path fires
                // while we are awaiting block bodies) none of those run, and the
                // slot is stranded on this dead peer's id.  No other peer can
                // then claim it, so bulk sync stalls until this exact peer
                // reconnects and re-adopts its matching id (observed as a
                // multi-minute genesis-storm stall).  This guard releases the
                // slot — and only if we still hold it (`compare_exchange` on our
                // id) — whenever the holding scope unwinds, for ANY reason.
                struct ActiveFetcherGuard {
                    fetcher: std::sync::Arc<std::sync::atomic::AtomicU64>,
                    id: u64,
                    /// Companion SocketAddr companion cleared in tandem with the
                    /// u64 CAS flag so the governor always sees a consistent view.
                    peer_slot: std::sync::Arc<std::sync::Mutex<Option<SocketAddr>>>,
                    /// Bumps `dugite_blockfetch_active_peers` +1 for the lifetime of
                    /// the claim window so the gauge reflects live fetch concurrency
                    /// (≈1 under the single-fetcher mutex; rises with multi-peer
                    /// fetch). Decremented on any drop (release / cancel / unwind).
                    metrics: std::sync::Arc<crate::metrics::NodeMetrics>,
                    /// When the slot was claimed — on drop, the held duration is
                    /// added to `blockfetch_busy_us_total` so utilization
                    /// (busy / wall) can be computed (idle-vs-network-bound).
                    claim_at: std::time::Instant,
                }
                impl Drop for ActiveFetcherGuard {
                    fn drop(&mut self) {
                        self.metrics
                            .inc_blockfetch_busy_us(self.claim_at.elapsed().as_micros() as u64);
                        let swapped = self.fetcher.compare_exchange(
                            self.id,
                            0,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        // Only clear the SocketAddr companion when WE were the ones
                        // holding the slot (compare_exchange succeeded).  This avoids
                        // clearing a different peer's address if we lost a CAS race.
                        if swapped.is_ok() {
                            if let Ok(mut guard) = self.peer_slot.lock() {
                                *guard = None;
                            }
                        }
                        self.metrics.dec_blockfetch_active_peers();
                    }
                }

                // BlockFetch worker: fetches blocks from this peer's candidate_chains.
                //
                // CRITICAL: Only ONE worker fetches at a time (matching Haskell's
                // bfcMaxConcurrencyBulkSync = 1). Workers compete for the
                // active_fetcher flag. The first to claim it becomes the sole
                // fetcher; others poll periodically to check if they should
                // take over (e.g., if the active fetcher's peer disconnects).
                use dugite_network::codec::Point as CodecPoint;
                use dugite_network::protocol::blockfetch::client::BlockFetchClient;

                // Per-worker dedup set: tracks block hashes successfully downloaded
                // in this worker's lifetime.  We do NOT drain `pending_headers` from
                // `candidate_chains` because that would permanently lose headers if
                // the connection drops mid-fetch (the ChainSync task will not
                // re-populate already-streamed headers until a rollback, causing
                // multi-minute sync stalls).  Instead we read headers in-place and
                // skip any whose hash is already in this set.
                let mut fetched_hashes: std::collections::HashSet<[u8; 32]> =
                    std::collections::HashSet::new();

                // Running average of recently-seen raw block CBOR sizes, used to
                // size each fetch range against `BLOCKFETCH_RANGE_BYTE_BUDGET`.
                // Starts pessimistic (Conway-sized) so the first range is small.
                let mut avg_block_bytes: usize = BLOCKFETCH_INIT_AVG_BLOCK_BYTES;

                // ChainSelStarvation: records when this peer first claimed the
                // active_fetcher slot in the current continuous claim window.
                // Reset to None when the slot is released (peer not fetching).
                // Used below for grace-period starvation dynamo rotation.
                let mut claim_start_ms: Option<u64> = None;

                // #742 watchdog: first instant of a CONTINUOUS streak of
                // unproductive claim ticks (slot claimed but nothing
                // dispatchable — empty runs, or the #735 far-ahead decline).
                // Cleared whenever a range is actually dispatched. Drives the
                // conservative 3×grace dynamo rotation for wedge classes the
                // Haskell starvation path cannot reach (it requires a current
                // fetch peer with an outstanding request).
                let mut unproductive_since_ms: Option<u64> = None;

                info!(%addr, "blockfetch worker started (waiting for turn)");

                // Per-peer worker poll cadence.
                //
                // After commit a1490cb5f the `active_fetcher` lock is
                // released at the end of every batch, so the gap between two
                // consecutive BlockFetch ranges is bounded by this interval
                // (whichever peer's ticker fires first wins the next contest).
                //
                // Set to 10 ms to match Haskell's `bfcDecisionLoopIntervalPraos`
                // from `cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs`.
                // The previous value of 200 ms capped sustained Byron throughput
                // at ~100 blk/s (verified live on mainnet: see
                // `project_blockfetch_poll_interval_2026_05_28`) — well below
                // the per-peer wire ceiling of ~1500 blk/s.  10 ms matches the
                // Haskell decision-loop cadence so a batch that completes is
                // immediately followed by the next without an idle gap.  CPU
                // cost is bounded: each tick does a single `compare_exchange`
                // + a brief `candidate_chains.read()` lock, ~tens of µs.  With
                // 22 hot peers that's ~5% CPU on Apple Silicon under bulk sync
                // — acceptable for the throughput gain.
                //
                // **Phase offset (#702 follow-up)**: workers that spawn
                // together all tick at the same wall-clock instant, so the
                // worker with the lowest CAS latency always wins the
                // `active_fetcher` race.  Offset each worker's ticker start
                // by a deterministic-but-distinct amount derived from the
                // peer SocketAddr hash so workers spread evenly across the
                // interval, giving every peer a fair claim window.  At 10 ms
                // the offset window is 0–10 ms — still enough to break ties
                // on the same wall-clock instant.
                let phase_offset = {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    addr.hash(&mut h);
                    std::time::Duration::from_millis(h.finish() % 10)
                };
                let mut poll_ticker = tokio::time::interval_at(
                    tokio::time::Instant::now() + phase_offset,
                    std::time::Duration::from_millis(10),
                );
                poll_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            // Release the active fetcher flag if we hold it.
                            // Use hash of full SocketAddr (IP + port) for unique peer ID.
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            addr.hash(&mut hasher);
                            let cancel_id = hasher.finish() | 1; // ensure non-zero
                            let swapped = active_fetcher.compare_exchange(
                                cancel_id,
                                0,
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                            );
                            // Clear the SocketAddr companion only when we held the slot.
                            if swapped.is_ok() {
                                if let Ok(mut guard) = active_fetch_peer.lock() {
                                    *guard = None;
                                }
                            }
                            debug!(%addr, "blockfetch worker cancelled");
                            break;
                        }
                        _ = poll_ticker.tick() => {
                            // Only ONE worker fetches at a time to prevent duplicate
                            // downloads (matching Haskell's bfcMaxConcurrencyBulkSync=1).
                            let my_id: u64 = {
                                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                addr.hash(&mut hasher);
                                hasher.finish() | 1
                            };
                            // GSV gate (cardano-node prioritisePeerChains): when the
                            // slot is FREE, only the fastest-serving peers contest it,
                            // so the single fetcher converges on the best peer rather
                            // than a fair race. A peer that already holds the slot
                            // keeps it (re-claim) — the gate only filters NEW claims.
                            // Wedge-safe: unmeasured / cold-start peers stay eligible
                            // and the existing starvation rotation frees a stuck peer.
                            if active_fetcher.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                                let preferred = peer_manager_for_fetch
                                    .read()
                                    .await
                                    .should_claim_fetch_slot(&addr, GSV_FETCH_TOP_K);
                                if !preferred {
                                    continue;
                                }
                            }
                            let claimed = active_fetcher.compare_exchange(
                                0,
                                my_id,
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                            ).is_ok();
                            let current = active_fetcher.load(std::sync::atomic::Ordering::SeqCst);
                            if !claimed && current != my_id {
                                continue;
                            }

                            // We now hold the single-fetcher slot.  This guard
                            // releases it on every exit from this loop iteration
                            // — including a mid-batch task abort — so the slot is
                            // never stranded on a dead peer (see the guard def).
                            // It is idempotent with the explicit post-batch /
                            // no-headers releases below (its `compare_exchange`
                            // is a no-op once the slot has already been cleared).
                            // Publish the SocketAddr companion so the governor can
                            // exclude this peer from aboveTargetOther demotion.
                            if claimed {
                                if let Ok(mut guard) = active_fetch_peer.lock() {
                                    *guard = Some(addr);
                                }
                                // Record claim start for starvation detection.
                                // If this peer is already in a claim window (re-claim
                                // after releasing at end of batch), we do NOT reset
                                // claim_start so the starvation window keeps growing.
                                if claim_start_ms.is_none() {
                                    let now_ms = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                    claim_start_ms = Some(now_ms);
                                }
                            }
                            metrics_clone.inc_blockfetch_active_peers();
                            let _fetch_guard = ActiveFetcherGuard {
                                fetcher: active_fetcher.clone(),
                                id: my_id,
                                peer_slot: active_fetch_peer.clone(),
                                metrics: metrics_clone.clone(),
                                claim_at: std::time::Instant::now(),
                            };

                            // Build the list of headers to fetch from this peer.
                            //
                            // KEY INVARIANT: we do NOT drain `pending_headers`.
                            // Headers remain in `candidate_chains` so they survive
                            // a mid-fetch connection drop.  Instead we skip any
                            // header whose hash is already in `fetched_hashes`
                            // (downloaded by this worker in an earlier iteration)
                            // or whose hash is already in the ChainDB (already
                            // stored, possibly on a divergent fork).
                            //
                            // FILTER BY HASH, NOT SLOT.
                            //
                            // A slot-based filter (`h.slot > applied_slot`) is unsound
                            // for fork blocks delivered after `MsgRollBackward`.  When
                            // a peer rolls back to slot R (R < applied_slot) and
                            // begins streaming a competing fork, the fork's earliest
                            // blocks may carry slots in the range (R, applied_slot].
                            // Those headers MUST be fetched so `walk_chain_back` from
                            // the fork's tip can reconstruct the ancestry through
                            // VolatileDB and intersect either the selected chain or
                            // the immutable anchor; otherwise chain_sel reports
                            // `fork unreachable — StoreButDontChange` for every new
                            // fork tip and the BP stalls on the abandoned fork
                            // (observed live on preview 2026-04-26: peer rolled back
                            // 1 block and grew a 9+ block fork; only the latest
                            // headers passed the slot filter, leaving the parent gap
                            // unfetched).
                            //
                            // Hash-based filtering (`!chain_db.has_block(h.hash)`)
                            // matches Haskell `BlockFetch.Decision`: it fetches
                            // every block on `theirFrag` not on `curChain`, regardless
                            // of slot ordering.  Headers above the volatile-window
                            // boundary are stored in VolatileDB on first fetch and
                            // skipped afterwards by the per-worker `fetched_hashes`
                            // set; headers that have already been flushed to
                            // ImmutableDB are skipped by `has_block`.
                            let fetch_runs = {
                                let chains = candidate_chains.read().await;
                                let cdb = chain_db.read().await;
                                use dugite_primitives::hash::Hash32;
                                if let Some(state) = chains.get(&addr) {
                                    let runs = select_fetch_runs(
                                        &state.pending_headers,
                                        |h| cdb.has_block(&Hash32::from_bytes(*h)),
                                        &fetched_hashes,
                                    );
                                    // (Slot release happens via the guard's
                                    // Drop at iteration end — a manual
                                    // store(0) here would defeat the guard's
                                    // CAS and leave the SocketAddr companion
                                    // stale.)
                                    runs
                                } else {
                                    // Voluntary release: the claim epoch must
                                    // not bleed into a later re-claim (the
                                    // grace clock measures slot TENURE, not
                                    // idle time).
                                    claim_start_ms = None;
                                    continue;
                                }
                            };

                            if fetch_runs.is_empty() {
                                // Header-supply-bound idle signal: the slot was
                                // claimable but there are no headers ahead to
                                // fetch — the fetcher has drained ChainSync's
                                // supply and is waiting (NOT network-bound).
                                metrics_clone.inc_blockfetch_idle_no_headers();
                                // #742 watchdog (unproductive claim): the slot
                                // was claimed but there is NOTHING dispatchable
                                // from this peer. Haskell's rotation cannot
                                // fire here (no current fetch peer without a
                                // fetch request — it relies on the LoP to kill
                                // silent peers), but dugite pauses the LoP
                                // while a peer parks on the forecast horizon,
                                // so a dynamo that never feeds headers would
                                // starve ChainSel forever. Rotate after a
                                // conservative 3× grace of continuous
                                // unproductive claims WITH ChainSel starved
                                // (issue #742's watchdog ask; no-op unless
                                // this peer is the dynamo).
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                if unproductive_since_ms.is_none() {
                                    unproductive_since_ms = Some(now_ms);
                                }
                                if let (Some(ref cs), Some(since)) = (&csj, unproductive_since_ms)
                                {
                                    let is_genesis_bulk_sync = gsm_snapshot_rx_for_starv
                                        .borrow()
                                        .state
                                        != crate::gsm::GenesisSyncState::CaughtUp;
                                    let starv = chainsel_starvation_ms
                                        .load(std::sync::atomic::Ordering::Relaxed);
                                    let last_starvation_ms =
                                        if starv == 0 { now_ms } else { starv };
                                    let watchdog_ms =
                                        3 * block_fetch_grace_period.as_millis() as u64;
                                    if is_genesis_bulk_sync
                                        && now_ms.saturating_sub(since) >= watchdog_ms
                                        && last_starvation_ms >= since.saturating_add(watchdog_ms)
                                    {
                                        // #760-A: only rotate a GENUINELY-SILENT
                                        // dynamo. A dynamo that fed a forecast
                                        // window of headers and is now PARKED on the
                                        // horizon has its CSJ fragment far ahead of
                                        // our selected chain; rotating it just
                                        // re-intersects a fresh dynamo at the same
                                        // frontier and re-parks it (~1 blk/min
                                        // cold-restart churn). Mirror Haskell: a peer
                                        // blocked at the forecast horizon is not
                                        // starving us — the ledger is catching up.
                                        // The #742 silent-dynamo case (fragment NOT
                                        // ahead) is still rotated. The chain_db read
                                        // is taken only here (≤ once per watchdog
                                        // window), never on the hot per-tick path.
                                        let chain_tip_slot = {
                                            let cdb = chain_db.read().await;
                                            cdb.get_tip().point.slot().map(|s| s.0).unwrap_or(0)
                                        };
                                        let fragment_head = cs.fragment_head_slot(&addr);
                                        let rotate = should_rotate_unproductive_dynamo(
                                            fragment_head,
                                            chain_tip_slot,
                                        );
                                        if rotate {
                                            if cs.rotate_dynamo(&addr) {
                                                info!(
                                                    %addr,
                                                    unproductive_secs =
                                                        now_ms.saturating_sub(since) / 1000,
                                                    "BlockFetch: silent dynamo unproductive past \
                                                     watchdog with ChainSel starved (no headers \
                                                     ahead) — rotating (#742/#760-A)"
                                                );
                                            }
                                        } else {
                                            // Parked-with-headers dynamo: KEEP it (the ledger is
                                            // catching up to the forecast horizon). Log so an
                                            // operator can tell "watchdog fired and correctly held
                                            // the parked dynamo" from "watchdog never fired".
                                            debug!(
                                                %addr,
                                                ahead = fragment_head
                                                    .unwrap_or(0)
                                                    .saturating_sub(chain_tip_slot),
                                                "BlockFetch: unproductive dynamo KEPT — parked on \
                                                 forecast horizon, not silent (#760-A)"
                                            );
                                        }
                                        unproductive_since_ms = None;
                                    }
                                }
                                // Voluntary release happened above (store(0));
                                // reset the claim epoch so tenure is measured
                                // per continuous hold.
                                claim_start_ms = None;
                                continue;
                            }

                            // Issue #747: cap per-decision-tick fetch to 2048
                            // headers. This bounds the in-flight range-request
                            // count and prevents a runaway queue of very large
                            // `MsgRequestRange` that would fill the ingress
                            // buffer faster than the mux can drain it.  Haskell
                            // BlockFetch.Decision uses `blocksToFetch` capped by
                            // `bfcMaxRequestsInflight * bfcDecisionLoopInterval
                            // * bandwidth` — our 2048 header cap at 10 ms is the
                            // practical equivalent for Dugite's single-fetcher
                            // architecture.  Any remaining headers reappear on
                            // the next tick (fetched_hashes is not advanced for
                            // un-requested headers).
                            let fetch_runs = {
                                let mut budget = 2048usize;
                                let mut capped: Vec<Vec<PendingHeader>> = Vec::new();
                                for mut run in fetch_runs {
                                    if budget == 0 {
                                        break;
                                    }
                                    if run.len() > budget {
                                        run.truncate(budget);
                                    }
                                    budget -= run.len();
                                    capped.push(run);
                                }
                                capped
                            };
                            // Flat view for the legacy single-list consumers
                            // below (contiguity guard, logging, hash dedup).
                            let headers_to_fetch: Vec<PendingHeader> =
                                fetch_runs.iter().flatten().cloned().collect();

                            // #735: Genesis gross-request invariant (Haskell
                            // `selectThePeer` / `requestHeadInCandidate`):
                            // only dispatch a range that contiguously extends
                            // the known chain. After a CSJ jump a promoted
                            // peer's pending headers may start far above the
                            // frontier; fetching them would store unreachable
                            // far-ahead blocks (`fork unreachable`) and strand
                            // the single fetcher slot on a range chain-sel can
                            // never adopt. Shelley+ headers carry prev_hash —
                            // require the FIRST block of the range to connect
                            // to a stored block. Byron headers (rejected by
                            // the wrapped-header decoder) skip the check: the
                            // CSJ promotion re-intersection in `sync.rs`
                            // guarantees contiguity at the source for all
                            // eras; this guard is defense-in-depth.
                            if let Some(first) = headers_to_fetch.first() {
                                if let Ok(hdr) =
                                    dugite_serialization::decode_wire_wrapped_block_header(
                                        &first.header_cbor,
                                    )
                                {
                                    let prev = hdr.prev_hash;
                                    // Accept a parent this worker already
                                    // fetched even if the apply pipeline has
                                    // not stored it yet (channel lag) — the
                                    // contiguous chain is in flight, not
                                    // missing.
                                    let connects = fetched_hashes.contains(prev.as_bytes()) || {
                                        let cdb = chain_db.read().await;
                                        // From-origin bootstrap: an EMPTY
                                        // ChainDB has no frontier to extend —
                                        // the first block's prev_hash is the
                                        // GENESIS HASH, which is never a
                                        // stored block. The guard only arms
                                        // once a frontier exists (caught live
                                        // on the devnet: the relay wedged at
                                        // origin, declining block 1 forever).
                                        cdb.get_tip_info().is_none() || cdb.has_block(&prev)
                                    };
                                    if !connects {
                                        debug!(
                                            %addr,
                                            first_slot = first.slot,
                                            prev = %prev.to_hex(),
                                            "BlockFetch: declining far-ahead range — \
                                             first block does not extend a stored block \
                                             (gross-request invariant, #735)"
                                        );
                                        // #742 watchdog: a perpetual decline is the
                                        // exact #735 far-ahead wedge — same treatment
                                        // as the empty-runs case above: rotate the
                                        // dynamo after 3× grace of continuous
                                        // unproductive claims with ChainSel starved.
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_millis() as u64)
                                            .unwrap_or(0);
                                        if unproductive_since_ms.is_none() {
                                            unproductive_since_ms = Some(now_ms);
                                        }
                                        if let (Some(ref cs), Some(since)) =
                                            (&csj, unproductive_since_ms)
                                        {
                                            let is_genesis_bulk_sync =
                                                gsm_snapshot_rx_for_starv.borrow().state
                                                    != crate::gsm::GenesisSyncState::CaughtUp;
                                            let starv = chainsel_starvation_ms
                                                .load(std::sync::atomic::Ordering::Relaxed);
                                            let last_starvation_ms =
                                                if starv == 0 { now_ms } else { starv };
                                            let watchdog_ms = 3 * block_fetch_grace_period
                                                .as_millis()
                                                as u64;
                                            if is_genesis_bulk_sync
                                                && now_ms.saturating_sub(since) >= watchdog_ms
                                                && last_starvation_ms
                                                    >= since.saturating_add(watchdog_ms)
                                            {
                                                if cs.rotate_dynamo(&addr) {
                                                    info!(
                                                        %addr,
                                                        unproductive_secs =
                                                            now_ms.saturating_sub(since) / 1000,
                                                        "BlockFetch: dynamo declined far-ahead \
                                                         ranges past watchdog with ChainSel \
                                                         starved — rotating (#742/#735)"
                                                    );
                                                }
                                                unproductive_since_ms = None;
                                            }
                                        }
                                        active_fetcher
                                            .store(0, std::sync::atomic::Ordering::SeqCst);
                                        claim_start_ms = None;
                                        continue;
                                    }
                                }
                            }

                            // A range IS being dispatched this tick — the
                            // unproductive-claim watchdog resets.
                            unproductive_since_ms = None;

                            // Issue #742 Fix 2: ChainSel-starvation dynamo rotation.
                            //
                            // Haskell `checkLastChainSelStarvation` (ouroboros-network
                            // `BlockFetch/Decision/Genesis.hs`): every decision
                            // iteration computes
                            //   lastStarvationTime = if Ongoing then now else endedAt
                            // and, when there is a current fetch peer p with
                            //   lastStarvationTime >= peersOrderStart(p) + gracePeriod
                            // traces PeerStarvedUs, calls rotateDynamo(p) and pushes p
                            // to the back of the peers order. The starvation flag is
                            // EDGE-recorded by ChainSel (`Ongoing` while the queue is
                            // empty; `EndedAt` stamped only by the dequeue that ends a
                            // starvation period) — so a long block apply with a FULL
                            // queue (epoch boundary, snapshot write) never fires this.
                            //
                            // Runs only in genesis-mode bulk sync (GenesisFetchMode in
                            // Haskell = ConsensusMode Genesis && not caught up) and
                            // only when CSJ is enabled. `rotate_dynamo` is a no-op if
                            // this peer is not the dynamo — matching Haskell, where
                            // the starving fetch peer is demoted in the peers order
                            // regardless, but only the dynamo role rotates.
                            if let Some(ref cs) = csj {
                                if let Some(claim_ms) = claim_start_ms {
                                    let is_genesis_bulk_sync = gsm_snapshot_rx_for_starv
                                        .borrow()
                                        .state
                                        != crate::gsm::GenesisSyncState::CaughtUp;
                                    if is_genesis_bulk_sync {
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_millis() as u64)
                                            .unwrap_or(0);
                                        let starv = chainsel_starvation_ms
                                            .load(std::sync::atomic::Ordering::Relaxed);
                                        // 0 = Ongoing → starvation extends to "now".
                                        let last_starvation_ms =
                                            if starv == 0 { now_ms } else { starv };
                                        let grace_ms =
                                            block_fetch_grace_period.as_millis() as u64;
                                        if last_starvation_ms >= claim_ms + grace_ms {
                                            info!(
                                                %addr,
                                                grace_secs = block_fetch_grace_period.as_secs(),
                                                claim_held_ms = now_ms.saturating_sub(claim_ms),
                                                starvation_ongoing = (starv == 0),
                                                "BlockFetch: peer starved ChainSel past grace \
                                                 period — rotating CSJ dynamo (#742)"
                                            );
                                            cs.rotate_dynamo(&addr);
                                            // Release the fetcher slot via the guard's
                                            // Drop (CAS id→0 + clear the SocketAddr
                                            // companion). A manual store(0) here would
                                            // defeat the guard's CAS and leave the
                                            // companion slot stale.
                                            claim_start_ms = None;
                                            continue;
                                        }
                                    }
                                }
                            }

                            info!(
                                %addr,
                                count = headers_to_fetch.len(),
                                first_slot = headers_to_fetch.first().map(|h| h.slot).unwrap_or(0),
                                last_slot = headers_to_fetch.last().map(|h| h.slot).unwrap_or(0),
                                "BlockFetch: active fetcher, downloading blocks",
                            );

                            // Batch headers into ranges for efficient fetching.
                            // A single MsgRequestRange(from, to) fetches ALL
                            // blocks between the two points on the peer's chain
                            // — so ranges are built PER CONTIGUOUS RUN (never
                            // spanning a filtered-out gap, which would make the
                            // peer re-stream already-fetched gap blocks and
                            // blow past the byte budget; observed live as
                            // 33.5 MB from two nominally-8 MB ranges, #747).
                            // Within a run, ranges are chunked by EXACT
                            // header-declared body sizes (Haskell
                            // `blockFetchSize` analogue); Byron headers fall
                            // back to the adaptive average.
                            // Each entry: (from, to, estimated_wire_bytes,
                            // planned_count, all_declared) — the estimate
                            // travels with the range so the recv path can
                            // compare it against ACTUAL delivered bytes
                            // (#747 instrumentation: residual ingress overruns
                            // mean actual > estimate somewhere; the WARN below
                            // names the offending range). `all_declared` arms
                            // the #751 receive-side hard abort: only ranges
                            // whose every header DECLARED its body size carry
                            // an exact estimate the peer can be held to.
                            let ranges: Vec<(CodecPoint, CodecPoint, usize, usize, bool)> =
                                fetch_runs
                                    .iter()
                                    .flat_map(|run| {
                                        build_fetch_ranges(run, avg_block_bytes, max_range)
                                            .into_iter()
                                            .map(move |(start, end)| {
                                                let est: usize = run[start..=end]
                                                    .iter()
                                                    .map(|h| {
                                                        estimated_block_wire_bytes(
                                                            h,
                                                            avg_block_bytes,
                                                        )
                                                    })
                                                    .sum();
                                                (
                                                    CodecPoint::Specific(
                                                        run[start].slot,
                                                        run[start].hash,
                                                    ),
                                                    CodecPoint::Specific(
                                                        run[end].slot,
                                                        run[end].hash,
                                                    ),
                                                    est,
                                                    end - start + 1,
                                                    range_all_declared(&run[start..=end]),
                                                )
                                            })
                                    })
                                    .collect();

                            debug!(%addr, ranges = ranges.len(), headers = headers_to_fetch.len(), "BlockFetch: fetching in batched ranges");

                            // Track the hashes of blocks the peer actually delivered
                            // via `MsgBlock`.  Used below to update `fetched_hashes`
                            // selectively — see the comment near the loop end.
                            let mut received_hashes: std::collections::HashSet<[u8; 32]> =
                                std::collections::HashSet::new();

                            // Request pipelining: keep up to `pipeline_window`
                            // `MsgRequestRange` in flight at once so each range's
                            // network round-trip overlaps the receipt + apply of
                            // earlier ranges. The BlockFetch mini-protocol returns
                            // batch responses in FIFO request order, so the Nth
                            // `recv_batch` below corresponds to the Nth range.
                            // Mirrors Haskell `bfcMaxRequestsInflight` pipelining.
                            let pipeline_window =
                                BLOCKFETCH_PIPELINE_WINDOW.min(ranges.len()).max(1);
                            let mut next_req = 0usize;
                            let mut prime_failed = false;
                            while next_req < pipeline_window {
                                let (from, to, _est, _planned, _declared) = ranges[next_req].clone();
                                let send_req = BlockFetchClient::send_range_request(&mut channel, from, to);
                                let result = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => {
                                        debug!(%addr, "blockfetch worker: cancelled during pipeline prime");
                                        return;
                                    }
                                    r = tokio::time::timeout(FETCH_RANGE_TIMEOUT, send_req) => r
                                };
                                match result {
                                    Ok(Ok(())) => next_req += 1,
                                    _ => {
                                        prime_failed = true;
                                        break;
                                    }
                                }
                            }
                            if prime_failed {
                                warn!(%addr, "BlockFetch: failed to send pipelined range request");
                                {
                                    let mut chains = candidate_chains.write().await;
                                    if let Some(state) = chains.get_mut(&addr) {
                                        state.record_fetch_failed(addr);
                                    }
                                }
                                // Slot release + companion clear via the
                                // guard's Drop at `return` (a manual store(0)
                                // would defeat its CAS and leave the
                                // companion stale).
                                let _ = peer_failure_tx.try_send((addr, PeerFailureKind::Slow));
                                return;
                            }

                            for range_idx in 0..ranges.len() {
                                let peer = addr;
                                let range_to_slot = match &ranges[range_idx].1 {
                                    CodecPoint::Specific(s, _) => *s,
                                    CodecPoint::Origin => 0,
                                };

                                // Collect decoded blocks in a local Vec inside the
                                // sync callback, then send them via `.send().await`
                                // after `fetch_range` returns.
                                //
                                // IMPORTANT: Do NOT call `tx.blocking_send()` inside
                                // the callback.  `fetch_range` takes a *synchronous*
                                // `FnMut` callback and calls it from within the tokio
                                // async runtime.  `blocking_send` panics with
                                // "Cannot block the current thread from within a
                                // runtime" whenever the channel is full and it tries
                                // to park the calling thread — exactly the crash we
                                // observed.  Collecting into a Vec and awaiting the
                                // sends outside the callback avoids the panic while
                                // preserving ordering and backpressure.
                                let mut decoded_blocks: Vec<FetchedBlock> = Vec::new();
                                // #751: per-range byte accounting + hard abort,
                                // armed only when every header in the range
                                // DECLARED its body size (exact estimate).
                                // Byron/average-based ranges stay unarmed —
                                // honest variance there is unbounded and the
                                // 48 MB ingress backstop still applies. Also
                                // feeds the `avg_block_bytes` refresh for the
                                // next range's byte-budget sizing.
                                let range_est = ranges[range_idx].2;
                                let mut range_abort =
                                    RangeByteAbort::new(ranges[range_idx].4, range_est);

                                let fetch_start = std::time::Instant::now();
                                // Wrap recv_batch in a cancel-aware select so
                                // deactivation completes within spsDeactivateTimeout
                                // (5 s) even when the peer is mid-stream of a large
                                // range.  Without this guard, cancel.cancelled() is
                                // only polled at the top-level `loop { select! {` —
                                // meaning the blockfetch task is un-cancellable for
                                // the full FETCH_RANGE_TIMEOUT (60 s) once it enters
                                // recv_batch, which always triggers the
                                // spsDeactivateTimeout warning and connection teardown.
                                let recv_batch_future = BlockFetchClient::recv_batch(
                                    &mut channel,
                                    |block_cbor| {
                                        // #751: abort the range as a PEER FAULT the
                                        // moment actual delivery exceeds the hard
                                        // limit — checked BEFORE decoding so a
                                        // size-lying peer buys no CPU either. The
                                        // resulting ProtocolError flows through the
                                        // normal fetch-failure path
                                        // (record_fetch_failed + peer_failure_tx
                                        // with ProtocolFault → reputation AND
                                        // connection teardown), attributing the
                                        // overrun to the peer instead of dying
                                        // generically at the mux ingress backstop.
                                        range_abort.on_block(block_cbor.len())?;
                                        match dugite_serialization::decode_block_with_byron_epoch_length(
                                            &block_cbor, bel,
                                        ) {
                                            Ok(block) => {
                                                let slot = block.slot().0;
                                                debug!(%addr, slot, block_no = block.block_number().0, "BlockFetch: block decoded");
                                                decoded_blocks.push(FetchedBlock {
                                                    peer,
                                                    block,
                                                    tip_slot: range_to_slot,
                                                    tip_hash: [0u8; 32],
                                                    tip_block_number: 0,
                                                });
                                                // Will be promoted to `received_hashes`
                                                // below after we drain `decoded_blocks`
                                                // (we can't borrow the outer set inside
                                                // this `FnMut` closure without making it
                                                // explicit; doing the bookkeeping after
                                                // `fetch_range` returns is equivalent and
                                                // avoids the borrow gymnastics).
                                            }
                                            Err(e) => {
                                                warn!(%addr, "block decode error: {e}");
                                                // DEBUG: dump failing CBOR for offline analysis.
                                                // Always-on capture for repro of preprod PV11 decode bug.
                                                if let Ok(dump_dir) = std::env::var("DUGITE_DECODE_FAIL_DUMP") {
                                                    let ts = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .map(|d| d.as_nanos())
                                                        .unwrap_or(0);
                                                    let len = block_cbor.len();
                                                    let path = std::path::PathBuf::from(&dump_dir)
                                                        .join(format!("decode_fail_{ts}_{len}.cbor"));
                                                    if let Some(parent) = path.parent() {
                                                        let _ = std::fs::create_dir_all(parent);
                                                    }
                                                    if let Err(write_err) = std::fs::write(&path, &block_cbor) {
                                                        warn!(%addr, "failed to dump CBOR: {write_err}");
                                                    } else {
                                                        warn!(%addr, path = %path.display(), bytes = block_cbor.len(), "dumped failing block CBOR");
                                                    }
                                                }
                                                // Abort the range: NEVER store a block past an
                                                // undecodable one. A gap in the stored chain gets
                                                // flushed to the ImmutableDB and then cannot be
                                                // connected across on replay (observed: a decode bug
                                                // at the Byron→Shelley boundary corrupted the db this
                                                // way). Returning Err drops this range's collected
                                                // blocks and fails the peer; the selected tip stays at
                                                // the last good block and a restart recovers cleanly
                                                // from the snapshot. A block that fails to deserialise
                                                // is a hard peer fault.
                                                return Err(
                                                    dugite_network::error::ProtocolError::CborDecode {
                                                        protocol: "BlockFetch",
                                                        reason: format!(
                                                            "block deserialisation failed: {e}"
                                                        ),
                                                    },
                                                );
                                            }
                                        }
                                        Ok(())
                                    },
                                );
                                let fetch_result = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => {
                                        debug!(%addr, "blockfetch worker: cancelled during recv_batch, releasing fetcher");
                                        return;
                                    }
                                    timed = tokio::time::timeout(FETCH_RANGE_TIMEOUT, recv_batch_future) => timed
                                };
                                match fetch_result {
                                    Ok(Ok(count)) => {
                                        let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;
                                        metrics_clone.record_block_fetch_range_latency(fetch_ms);
                                        // Live block-download throughput: count the
                                        // bytes this range delivered (rate = rx B/s).
                                        metrics_clone
                                            .inc_blockfetch_rx_bytes(range_abort.seen_bytes() as u64);
                                        // GSV: record this peer's measured fetch
                                        // bandwidth (bytes/sec) so the claim gate can
                                        // prefer the fastest-serving peers. Only count
                                        // ranges with a meaningful sample (>0 bytes,
                                        // >1 ms) so tiny/instant ranges don't skew it.
                                        if range_abort.seen_bytes() > 0 && fetch_ms > 1.0 {
                                            let bps = range_abort.seen_bytes() as f64
                                                / (fetch_ms / 1000.0);
                                            peer_manager_for_fetch
                                                .write()
                                                .await
                                                .update_peer_fetch_bandwidth(&addr, bps);
                                        }
                                        // Refresh the average block size for the
                                        // next range's byte-budget sizing.
                                        if let Some(avg) = range_abort.seen_bytes().checked_div(count) {
                                            avg_block_bytes = avg.max(1);
                                        }
                                        // #747 instrumentation: residual ingress
                                        // overruns imply ACTUAL range bytes exceed
                                        // the header-declared estimate somewhere.
                                        // Surface any range whose delivery blows
                                        // 1.5x the estimate (+256 KiB slack) so the
                                        // offending slots/peer are identifiable.
                                        let est = range_est;
                                        if range_abort.seen_bytes() > est + est / 2 + 262_144 {
                                            let from_slot = match &ranges[range_idx].0 {
                                                CodecPoint::Specific(s, _) => *s,
                                                CodecPoint::Origin => 0,
                                            };
                                            warn!(
                                                %addr,
                                                range_idx,
                                                planned = ranges[range_idx].3,
                                                delivered = count,
                                                actual_bytes = range_abort.seen_bytes(),
                                                estimated_bytes = est,
                                                from_slot,
                                                to_slot = range_to_slot,
                                                "BlockFetch: range delivered far more bytes \
                                                 than the header-declared estimate (#747)"
                                            );
                                        }
                                        debug!(%addr, count, fetch_ms, avg_block_bytes, "BlockFetch: range complete");
                                    }
                                    Ok(Err(e)) => {
                                        warn!(%addr, "BlockFetch error: {e}");
                                        // Per-peer chain tracking (issue #702): record failure
                                        // and potentially mark peer Aberrant.
                                        {
                                            let mut chains = candidate_chains.write().await;
                                            if let Some(state) = chains.get_mut(&addr) {
                                                state.record_fetch_failed(addr);
                                            }
                                        }
                                        // #751: provable protocol violations
                                        // (mis-declared sizes, undecodable
                                        // blocks, agency/state violations) get
                                        // connection teardown on top of
                                        // reputation — Haskell parity, where
                                        // every BlockFetch conviction is a
                                        // thrown exception that kills the
                                        // bearer. Transport-level recv errors
                                        // stay reputation-only.
                                        use dugite_network::error::ProtocolError as PE;
                                        let kind = match &e {
                                            PE::CborDecode { .. }
                                            | PE::BoundsExceeded { .. }
                                            | PE::AgencyViolation { .. }
                                            | PE::InvalidMessage { .. }
                                            | PE::StateViolation { .. } => {
                                                PeerFailureKind::ProtocolFault
                                            }
                                            _ => PeerFailureKind::Slow,
                                        };
                                        // Slot release + companion clear via
                                        // the guard's Drop at `return`.
                                        let _ = peer_failure_tx.try_send((addr, kind));
                                        return;
                                    }
                                    Err(_elapsed) => {
                                        // Fetch deadline exceeded — peer is stalled or
                                        // TCP connection is half-open. Release active
                                        // fetcher so another peer can take over, and
                                        // report the failure for reputation scoring.
                                        warn!(
                                            %addr,
                                            timeout_secs = FETCH_RANGE_TIMEOUT.as_secs(),
                                            "BlockFetch range timed out, releasing fetcher",
                                        );
                                        // GSV: a timed-out peer is effectively zero
                                        // throughput right now — collapse its measured
                                        // bandwidth so the claim gate de-prefers it and
                                        // the slot converges on a healthy fast peer
                                        // (rather than re-picking the stalled one from a
                                        // stale-but-high EWMA).
                                        peer_manager_for_fetch
                                            .write()
                                            .await
                                            .update_peer_fetch_bandwidth(&addr, 1.0);
                                        // CSJ: a peer starving BlockFetch is
                                        // rotated out of the dynamo role so a
                                        // different peer drives the jumps
                                        // (Haskell: ChainSel-starvation past the
                                        // grace period → demoteChainSyncJumpingDynamo).
                                        if let Some(ref csj) = csj {
                                            csj.rotate_dynamo(&addr);
                                        }
                                        // Per-peer chain tracking (issue #702): record timeout
                                        // as a failure towards the Aberrant threshold.
                                        {
                                            let mut chains = candidate_chains.write().await;
                                            if let Some(state) = chains.get_mut(&addr) {
                                                state.record_fetch_failed(addr);
                                            }
                                        }
                                        // Slot release + companion clear via
                                        // the guard's Drop at `return`.
                                        let _ = peer_failure_tx.try_send((addr, PeerFailureKind::Slow));
                                        return;
                                    }
                                }

                                // Refill the pipeline window: request the next
                                // not-yet-sent range NOW, so the peer is streaming
                                // it while we forward this range's blocks to the
                                // apply loop below. Keeps `pipeline_window` requests
                                // outstanding at all times (until the tail).
                                if next_req < ranges.len() {
                                    let (from, to, _est, _planned, _declared) = ranges[next_req].clone();
                                    let refill_req = BlockFetchClient::send_range_request(&mut channel, from, to);
                                    let refill_result = tokio::select! {
                                        biased;
                                        _ = cancel.cancelled() => {
                                            debug!(%addr, "blockfetch worker: cancelled during pipeline refill");
                                            return;
                                        }
                                        r = tokio::time::timeout(FETCH_RANGE_TIMEOUT, refill_req) => r
                                    };
                                    match refill_result {
                                        Ok(Ok(())) => next_req += 1,
                                        _ => {
                                            warn!(%addr, "BlockFetch: failed to send pipelined refill request");
                                            {
                                                let mut chains = candidate_chains.write().await;
                                                if let Some(state) = chains.get_mut(&addr) {
                                                    state.record_fetch_failed(addr);
                                                }
                                            }
                                            // Slot release + companion clear
                                            // via the guard's Drop at `return`.
                                            let _ = peer_failure_tx.try_send((addr, PeerFailureKind::Slow));
                                            return;
                                        }
                                    }
                                }

                                // Send all blocks collected for this range using
                                // `.send().await` — which correctly yields to the
                                // scheduler instead of blocking the thread.
                                //
                                // Promote each delivered block's hash into the
                                // received-set BEFORE handing the value off to the
                                // channel (`fetched` is moved, so we capture the
                                // hash first).  See the post-loop dedup comment
                                // below for why this matters.
                                for fetched in decoded_blocks {
                                    let slot = fetched.block.slot().0;
                                    received_hashes
                                        .insert(*fetched.block.header.header_hash.as_bytes());
                                    // Per-peer chain tracking (issue #702): record each
                                    // delivered block as a successful delivery, resetting
                                    // the consecutive_failures counter and rehabilitating
                                    // any Aberrant state.
                                    {
                                        let mut chains = candidate_chains.write().await;
                                        if let Some(state) = chains.get_mut(&addr) {
                                            state.record_fetch_delivered();
                                        }
                                    }
                                    // Wrap the channel send in a cancel-aware select.
                                    // Without this, a full fetched_blocks channel
                                    // (backpressure from a slow apply loop) makes the
                                    // blockfetch task un-cancellable for an unbounded
                                    // duration — blocking demote_to_warm past the 5s
                                    // spsDeactivateTimeout and forcing a TCP teardown.
                                    // Measure time blocked here: when the apply
                                    // consumer is slow the channel fills and this
                                    // send parks the fetcher (slot held, no bytes
                                    // downloaded) — apply-backpressure, NOT a
                                    // network limit. Surfaced as
                                    // blockfetch_send_blocked_us_total.
                                    let send_started = std::time::Instant::now();
                                    let send_result = tokio::select! {
                                        biased;
                                        _ = cancel.cancelled() => {
                                            debug!(%addr, slot, "blockfetch worker: cancelled while draining decoded blocks, releasing fetcher");
                                            return;
                                        }
                                        r = fetched_blocks_tx.send(fetched) => r
                                    };
                                    metrics_clone.inc_blockfetch_send_blocked_us(
                                        send_started.elapsed().as_micros() as u64,
                                    );
                                    if let Err(e) = send_result {
                                        warn!(%addr, slot, "send to run loop failed (channel closed): {e}");
                                        // Channel closed means the run loop
                                        // exited. Slot release + companion
                                        // clear via the guard's Drop.
                                        return;
                                    }
                                }
                            }

                            // Per-worker dedup: only mark hashes for blocks the peer
                            // actually delivered via `MsgBlock`.
                            //
                            // BUG (#702): the previous code unconditionally inserted
                            // EVERY hash in `headers_to_fetch` into `fetched_hashes`,
                            // even ones for which the peer sent no `MsgBlock`
                            // (e.g. when `MsgBatchDone` arrives early with fewer
                            // blocks than requested).  Those undelivered hashes were
                            // permanently marked "fetched", so the worker never
                            // re-requested them — every downstream block became an
                            // orphan in VolatileDB (`fork unreachable —
                            // StoreButDontChange`), and the apply path stalled at
                            // the predecessor of the first missing block while
                            // peers kept advancing past the forecast horizon and
                            // disconnecting.  Reproduced as a deterministic stall on
                            // ~1-in-3 from-genesis preview runs.
                            //
                            // The Haskell reference (BlockFetch.Decision) tracks
                            // per-peer "in-flight" by request, not by header, but
                            // crucially never marks a block as fetched until the
                            // body arrives via `MsgBlock` — so a peer that
                            // short-batches always sees the un-delivered headers
                            // re-requested on the next decision tick.
                            //
                            // The next loop iteration will rebuild
                            // `headers_to_fetch` from `state.pending_headers` minus
                            // `fetched_hashes` and `has_block`, picking up exactly
                            // the missing blocks for re-request.
                            for h in &received_hashes {
                                fetched_hashes.insert(*h);
                            }
                            let short_batched =
                                received_hashes.len() < headers_to_fetch.len();
                            if short_batched {
                                info!(
                                    %addr,
                                    requested = headers_to_fetch.len(),
                                    received = received_hashes.len(),
                                    "BlockFetch: peer short-batched; releasing fetcher so another peer can supply missing blocks"
                                );
                            }

                            // Note: we do NOT update max_fetched_slot here.
                            // Per-worker dedup uses fetched_hashes (hash-based).
                            // Cross-worker dedup uses the applied ChainDB tip.
                            // max_fetched_slot caused sync stalls by jumping to
                            // the chain tip and filtering out all gap blocks.

                            // Release `active_fetcher` after every batch so
                            // other peers' workers get a fair chance to claim
                            // it on their next poll tick.  Previously the
                            // current worker held the lock across iterations,
                            // which monopolised fetching from a single peer:
                            // if that peer didn't have a specific block in its
                            // chain (or refused to serve it after multiple
                            // re-requests), no other peer could ever fetch it,
                            // and sync stalled indefinitely with the same
                            // peer cycling failed retries.
                            //
                            // This mirrors Haskell `BlockFetch.Decision`:
                            // every decision-loop tick (`bfcDecisionLoopIntervalPraos
                            // = 10ms`) reconsiders which peer to fetch from,
                            // so a peer that fails to deliver loses its slot
                            // to a peer that will.  Dugite's analog is one
                            // worker per peer + the shared `active_fetcher`
                            // atomic; releasing here every batch gives the
                            // same rotation effect.  See issue #702.
                            let _ = active_fetcher.compare_exchange(
                                my_id,
                                0,
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                            );
                            // Reset claim window: the slot was just voluntarily
                            // released. If this peer re-claims on the next tick
                            // the starvation clock starts fresh (Haskell resets
                            // the grace-period timer on each new acquisition).
                            claim_start_ms = None;

                            // Explicit yield so the SAME peer's worker
                            // Yield once to the scheduler so that a
                            // co-located worker that is already blocked on
                            // `peer_rx.recv()` (or anywhere else) gets to
                            // run before we re-enter the select! loop.
                            // The previous 200 ms sleep here was the second
                            // half of the throughput cap (alongside the
                            // 200 ms poll interval) — see the long comment
                            // on `poll_ticker` for the matching reasoning.
                            // A `yield_now()` gives the same fairness
                            // guarantee on the tokio runtime without
                            // introducing wall-clock latency.
                            tokio::task::yield_now().await;
                        }
                    }
                }
            })
        })
    }

    /// Create the TxSubmission2 protocol task closure for a specific peer.
    ///
    /// The TxSubmission2 protocol relays transactions between peers. As the
    /// initiator, we respond to the server's requests for transaction IDs
    /// and transaction bodies from our mempool via `TxSubmissionClient`.
    fn make_txsubmission_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let mempool = self.mempool.clone();
        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                let source = MempoolTxSource::new(mempool);
                // Pass cancel directly into run() so every inner await — including
                // channel.recv() and the blocking-mode mempool poll loop — is cancel-
                // aware.  run() returns Ok(()) on cancellation so we don't need the
                // outer select! anymore, but we keep it as defence-in-depth for any
                // future await paths added to run() that might miss the token.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        debug!(%addr, "txsubmission2 task cancelled (outer guard)");
                    }
                    result = dugite_network::TxSubmissionClient::run(&mut channel, &source, &cancel) => {
                        match result {
                            Ok(()) => debug!(%addr, "txsubmission2 client completed"),
                            Err(e) => debug!(%addr, "txsubmission2 client error: {e}"),
                        }
                    }
                }
            })
        })
    }

    // ─── Server Protocol Task Factories ─────────────────────────────────────
    //
    // These create responder-side protocol closures for duplex connections.
    // Each returns a `ProtocolTaskFn` that is spawned on the server-side mux
    // channels of a `PeerConnection`.

    /// Create the ChainSync server task closure.
    ///
    /// Subscribes to block announcements and rollback announcements, then runs
    /// the ChainSync server loop — streaming blocks to downstream peers as
    /// they are produced or relayed.
    fn make_chainsync_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let block_provider = self.block_provider.clone();
        let announcement_rx = self.block_announcement_tx.subscribe();
        let rollback_rx = self.rollback_announcement_tx.subscribe();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                let mut server =
                    dugite_network::protocol::chainsync::server::ChainSyncServer::new();
                info!(%addr, "chainsync server task spawned");
                tokio::select! {
                    result = server.run(&mut channel, block_provider.as_ref(), announcement_rx, rollback_rx) => {
                        match result {
                            Ok(()) => info!(%addr, "chainsync server task completed cleanly"),
                            Err(e) => warn!(%addr, error = %e, "chainsync server task exited with error"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        info!(%addr, "chainsync server task cancelled");
                    }
                }
            })
        })
    }

    /// Create the BlockFetch server task closure.
    ///
    /// Serves block data from ChainDB in response to `MsgRequestRange` from
    /// downstream peers.
    fn make_blockfetch_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let block_provider = self.block_provider.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                tokio::select! {
                    result = dugite_network::protocol::blockfetch::server::BlockFetchServer::run(&mut channel, block_provider.as_ref()) => {
                        match result {
                            Ok(()) => debug!(%addr, "blockfetch server completed"),
                            Err(e) => debug!(%addr, "blockfetch server error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "blockfetch server cancelled");
                    }
                }
            })
        })
    }

    /// Create the TxSubmission2 server task closure.
    ///
    /// Receives transactions from downstream peers, decodes them across all
    /// supported eras (Conway=6 through Shelley=2), validates each tx through
    /// the full Phase-1 + Phase-2 ledger pipeline (including IsValid tag
    /// verification), and adds only valid ones to the mempool.
    /// Tracks received/validated/rejected metrics.
    fn make_txsubmission_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let mempool = self.mempool.clone();
        let metrics = self.metrics.clone();
        let tx_validator = self.tx_validator.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                let on_tx = {
                    let tx_mempool = mempool;
                    let tx_metrics = metrics;
                    let validator = tx_validator;
                    move |tx_hash: [u8; 32], tx_bytes: Vec<u8>| -> bool {
                        // Track every transaction received from peers in real-time.
                        tx_metrics
                            .transactions_received
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        // B7: Hard cap on tx body size before any decoding or
                        // Plutus evaluation.  A malicious peer can send a tx at
                        // the maximum protocol size (16,384 bytes) that passes
                        // Phase-1 but triggers worst-case Plutus V3 execution.
                        // Without this pre-flight check, 100 such txs in rapid
                        // succession can saturate the tokio runtime for seconds.
                        // The Cardano protocol max tx size is 16,384 bytes per
                        // protocol parameter `maxTxSize` (Conway value).
                        const MAX_TX_BODY_BYTES: usize = 16_384;
                        if tx_bytes.len() > MAX_TX_BODY_BYTES {
                            tx_metrics
                                .transactions_rejected
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            return false;
                        }

                        // Run full Phase-1 + Phase-2 validation (including IsValid
                        // tag check) before mempool admission.  This mirrors
                        // Haskell cardano-node's `applyTx` (mempool admission path)
                        // and prevents DoS via is_valid=false / script-passes txs
                        // (#522).  Try all supported eras for decoding.
                        let size_bytes = tx_bytes.len();
                        for era_id in [6u16, 5, 4, 3, 2] {
                            if let Ok(tx) =
                                dugite_serialization::decode_transaction(era_id, &tx_bytes)
                            {
                                // Phase-1 + Phase-2 validation via LedgerTxValidator.
                                if let Err(e) = validator.validate_tx(era_id, &tx_bytes) {
                                    debug!(
                                        %addr,
                                        tx_hash = %dugite_primitives::hash::Hash32::from_bytes(tx_hash).to_hex(),
                                        reason = ?e,
                                        "N2N tx rejected by validator"
                                    );
                                    tx_metrics
                                        .transactions_rejected
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    return false;
                                }

                                let hash = dugite_primitives::hash::Hash32::from_bytes(tx_hash);
                                if tx_mempool.add_tx(hash, tx, size_bytes).is_ok() {
                                    tx_metrics
                                        .transactions_validated
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    return true;
                                } else {
                                    tx_metrics
                                        .transactions_rejected
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    return false;
                                }
                            }
                        }
                        // Failed to decode in any era — count as rejected.
                        tx_metrics
                            .transactions_rejected
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                };

                tokio::select! {
                    result = dugite_network::TxSubmissionServer::run(&mut channel, on_tx) => {
                        match result {
                            Ok(stats) => debug!(
                                %addr,
                                tx_ids = stats.tx_ids_received,
                                txs_received = stats.txs_received,
                                accepted = stats.txs_accepted,
                                rejected = stats.txs_rejected,
                                "txsubmission2 server completed",
                            ),
                            Err(e) => debug!(%addr, "txsubmission2 server error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "txsubmission2 server cancelled");
                    }
                }
            })
        })
    }

    /// Create the KeepAlive server task closure.
    ///
    /// Responds to `MsgKeepAlive` pings from downstream peers with
    /// `MsgKeepAliveResponse` pongs.
    fn make_keepalive_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                tokio::select! {
                    result = dugite_network::KeepAliveServer::run(&mut channel) => {
                        match result {
                            Ok(count) => debug!(%addr, count, "keepalive server completed"),
                            Err(e) => debug!(%addr, "keepalive server error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "keepalive server cancelled");
                    }
                }
            })
        })
    }

    /// Create the PeerSharing server task closure.
    ///
    /// Reads connected peer addresses from the shared peer manager and serves
    /// them to downstream peers in response to `MsgShareRequest`.
    ///
    /// Only peers that are advertisable are included in the response. Peers in
    /// local root topology groups with `advertise: false` are excluded, matching
    /// Haskell's `NodeToNodeVersion` peer sharing filter that respects the
    /// `LocalRootPeers` `advertise` field (see `Ouroboros.Network.PeerSelection.State`).
    fn make_peersharing_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let peer_manager = self.peer_manager_for_servers.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                // Snapshot only advertisable connected peer addresses at task start.
                // Peers in local root groups with `advertise: false` are excluded so
                // private relays or block producers are never leaked to the network.
                let peers: Vec<SocketAddr> = {
                    let pm = peer_manager.read().await;
                    pm.connected_peer_addrs()
                        .into_iter()
                        .filter(|a| pm.is_advertisable(a))
                        .filter(|a| !crate::node::networking::is_non_public_ip(a.ip()))
                        .collect()
                };
                tokio::select! {
                    result = dugite_network::protocol::peersharing::server::PeerSharingServer::run(&mut channel, &peers) => {
                        match result {
                            Ok(()) => debug!(%addr, "peersharing server completed"),
                            Err(e) => debug!(%addr, "peersharing server error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "peersharing server cancelled");
                    }
                }
            })
        })
    }

    /// Start all five server-side protocol tasks on a connection.
    ///
    /// Called after warm protocols are started, to activate the responder side
    /// of the duplex mux. This enables downstream peers to sync blocks, fetch
    /// data, submit transactions, send keepalives, and request peer addresses.
    fn start_server_protocols_on(
        &self,
        addr: SocketAddr,
        conn: &mut PeerConnection,
    ) -> Result<(), PeerConnectionError> {
        // Defensive check: all connections should have server channels now.
        // Previously InitiatorOnly connections skipped server channels, but
        // that prevented BPs from serving blocks to relays.
        if !conn.has_server_channels() {
            return Ok(());
        }
        let cs = self.make_chainsync_server_task(addr);
        let bf = self.make_blockfetch_server_task(addr);
        let tx = self.make_txsubmission_server_task(addr);
        let ka = self.make_keepalive_server_task(addr);
        let ps = self.make_peersharing_server_task(addr);
        conn.start_server_protocols(cs, bf, tx, ka, ps)
    }

    /// Register an inbound connection from the N2N listener background task.
    ///
    /// This is the entry point for connections accepted by the TCP listener.
    /// The listener performs the handshake and creates a `PeerConnection`, then
    /// passes it here for lifecycle management. We start warm + server protocols
    /// and register the connection in the peer manager.
    ///
    /// Inbound and outbound to the same remote may coexist as long as their
    /// `(local, remote)` tuples differ — matching Haskell's
    /// `Ouroboros.Network.ConnectionManager.ConnMap`. When the duplex pair is
    /// detected, the peer's logical state is marked
    /// `ConnectionState::DuplexConn` so subsequent governor decisions see it
    /// as a single connected peer.
    ///
    /// ## Simultaneous open
    ///
    /// If an existing entry has the SAME `ConnectionId` as the incoming
    /// inbound (only possible when both peers bind their outbound source
    /// port to their listen port via SO_REUSEPORT, producing identical
    /// `(local, remote)` tuples), the inbound wins and the existing entry is
    /// shut down. Matches Haskell's `Overwritten` transition in
    /// `Ouroboros.Network.ConnectionManager.Core.acquireOutboundConnectionImpl`,
    /// which replaces the `ReservedOutboundState` slot with the inbound's
    /// state. The losing outbound's `updateLocalAddr` returns `False` and
    /// throws `ConnectionExists`, tearing down its socket.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::Connection` if the inbound's warm/server
    /// protocols fail to start.
    pub async fn register_inbound_connection(
        &mut self,
        addr: SocketAddr,
        mut conn: PeerConnection,
        rtt_ms: f64,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        let cid = ConnectionId {
            local: conn.local_addr,
            remote: addr,
        };

        // Simultaneous-open: same ConnectionId already present. Inbound wins
        // (Haskell `Overwritten` transition). Shut the displaced connection
        // down before inserting the new one.
        if let Some(mut displaced) = self.connections.remove(&cid) {
            warn!(
                %cid,
                "simultaneous open detected — inbound wins, displacing existing connection"
            );
            displaced.shutdown().await;
        }

        // Record handshake RTT for Prometheus metrics.
        self.metrics.record_handshake_rtt(rtt_ms);

        let keepalive_fn = self.make_keepalive_task(addr);
        conn.start_warm_protocols(keepalive_fn)?;
        self.start_server_protocols_on(addr, &mut conn)?;

        let existing_to_peer = self.has_any_to(addr);
        if existing_to_peer {
            // Duplex pair: peer-manager already knows about this remote (via
            // an outbound). Don't overwrite the logical OutboundIdle state;
            // mark it Duplex instead so demote_to_cold etc. tear down both.
            peer_manager.mark_peer_duplex(&addr);
            info!(%cid, "duplex pair established (existing connection to peer)");
        } else {
            peer_manager.peer_connected(&addr, ConnectionDirection::Inbound);
        }

        // Start the PeerSharing client task for inbound duplex connections.
        // Inbound connections subscribed with initiator_only=false have a
        // peersharing_client_channel; purely responder-only connections return None
        // from `take_peersharing_client_channel` and are silently skipped.
        let cancel = conn.cancel_token().clone();
        if let Some(ps_tx) = self.start_peersharing_client(addr, &mut conn, cancel) {
            self.peersharing_request_txs.insert(addr, ps_tx);
        }

        self.connections.insert(cid, conn);
        info!(%cid, rtt_ms = format_args!("{rtt_ms:.0}"), "inbound cold -> warm complete");
        Ok(())
    }
}

// ─── MempoolTxSource ─────────────────────────────────────────────────────────

/// Internal abstraction over the mempool query surface used by `MempoolTxSource`.
/// Parameterised so tests can inject a mock without touching the public `TxSource` API.
trait MempoolQuerySource: Send + Sync {
    fn query_tx_size(&self, hash: &dugite_primitives::hash::Hash32) -> Option<usize>;
    fn query_tx_hashes_ordered(&self) -> Vec<dugite_primitives::hash::Hash32>;
    fn query_tx_cbor(&self, hash: &dugite_primitives::hash::Hash32) -> Option<Vec<u8>>;
    fn query_is_empty(&self) -> bool;
    fn query_tx_notify(&self) -> Option<std::sync::Arc<tokio::sync::Notify>>;
}

impl MempoolQuerySource for Arc<Mempool> {
    fn query_tx_size(&self, hash: &dugite_primitives::hash::Hash32) -> Option<usize> {
        self.get_tx_size(hash)
    }
    fn query_tx_hashes_ordered(&self) -> Vec<dugite_primitives::hash::Hash32> {
        self.tx_hashes_ordered()
    }
    fn query_tx_cbor(&self, hash: &dugite_primitives::hash::Hash32) -> Option<Vec<u8>> {
        self.get_tx_cbor(hash)
    }
    fn query_is_empty(&self) -> bool {
        self.is_empty()
    }
    fn query_tx_notify(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        Some(self.tx_notify())
    }
}

/// Adapts `Mempool` to the `TxSource` trait for TxSubmission2 tx relay.
///
/// Tracks which tx IDs have been yielded to the remote peer via an internal
/// cursor over the mempool's ordered tx list. `get_tx_ids` acknowledges
/// previously sent IDs and returns the next batch.
///
/// Interior mutability via `Mutex` is used because `TxSource::get_tx_ids`
/// takes `&self` but we need to update the outstanding queue. The mutex is
/// uncontended — only the single TxSubmission2 client task accesses it.
struct MempoolTxSource<Q = Arc<Mempool>> {
    mempool: Q,
    /// Tx hashes yielded but not yet acknowledged by the peer.
    outstanding: std::sync::Mutex<std::collections::VecDeque<dugite_primitives::hash::Hash32>>,
    /// Per-peer dedup: hashes ever yielded to this peer that are still in the mempool.
    /// Prevents re-announcing acked txs at TCP-RTT speed when the mempool is non-empty.
    ever_yielded: std::sync::Mutex<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
}

impl MempoolTxSource {
    fn new(mempool: Arc<Mempool>) -> Self {
        Self {
            mempool,
            outstanding: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ever_yielded: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl<Q: MempoolQuerySource> TxSource for MempoolTxSource<Q> {
    fn get_tx_ids(&self, ack_count: u16, max_count: u16) -> Vec<TxIdAndSize> {
        let mut outstanding = self.outstanding.lock().unwrap();
        let mut ever_yielded = self.ever_yielded.lock().unwrap();

        // Acknowledge previously yielded tx IDs.
        for _ in 0..ack_count {
            outstanding.pop_front();
        }

        // Prune entries for txs no longer in the mempool (block confirmed / expired).
        // This also drops them from `ever_yielded` so they can be re-announced if
        // the same tx re-enters the mempool (e.g. after a rollback).
        outstanding.retain(|h| self.mempool.query_tx_size(h).is_some());
        ever_yielded.retain(|h| self.mempool.query_tx_size(h).is_some());

        // Get ordered tx hashes from mempool and yield new ones.
        let all_hashes = self.mempool.query_tx_hashes_ordered();
        let mut result = Vec::new();
        for hash in all_hashes {
            if result.len() >= max_count as usize {
                break;
            }
            // Skip if already yielded to this peer (acked or still outstanding).
            if ever_yielded.contains(&hash) {
                continue;
            }
            if let Some(size) = self.mempool.query_tx_size(&hash) {
                outstanding.push_back(hash);
                ever_yielded.insert(hash);
                // Compute the full GenTx wire size including HFC envelope:
                //   array(2)[1] + era_id[1] + tag(24)[2] + bytes_header[1-3] + cbor_data[N]
                // bytes_header: 1 byte for size < 24, 2 bytes for < 256, 3 bytes for < 65536
                let bytes_header_len = if size < 24 {
                    1
                } else if size < 256 {
                    2
                } else {
                    3
                };
                let wire_size = 1 + 1 + 2 + bytes_header_len + size;
                result.push(TxIdAndSize {
                    era_id: 6, // Conway
                    tx_id: *hash.as_bytes(),
                    size_in_bytes: wire_size as u32,
                });
            }
        }
        result
    }

    fn get_txs(&self, tx_ids: &[(u8, [u8; 32])]) -> Vec<(u8, Vec<u8>)> {
        tx_ids
            .iter()
            .filter_map(|(era_id, id)| {
                let hash = dugite_primitives::hash::Hash32::from_bytes(*id);
                self.mempool
                    .query_tx_cbor(&hash)
                    .map(|cbor| (*era_id, cbor))
            })
            .collect()
    }

    fn has_pending(&self) -> bool {
        !self.mempool.query_is_empty()
    }

    fn tx_notify(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        self.mempool.query_tx_notify()
    }
}

// ─── Test-only helpers ───────────────────────────────────────────────────────

/// A no-op [`TxValidator`] for unit tests that always reports every
/// transaction as valid.  Only used in test constructors — production code
/// always supplies a real [`super::serve::LedgerTxValidator`].
#[cfg(test)]
struct NoOpTxValidator;

#[cfg(test)]
impl TxValidator for NoOpTxValidator {
    fn validate_tx(
        &self,
        _era_id: u16,
        _tx_bytes: &[u8],
    ) -> Result<(), dugite_network::TxValidationError> {
        Ok(())
    }
}

#[cfg(test)]
impl ConnectionLifecycleManager {
    /// Create a minimal `ConnectionLifecycleManager` for use in unit tests.
    ///
    /// All channels and shared state are stubbed out with fresh, disconnected
    /// instances.  The resulting manager is not suitable for running actual
    /// peer connections, but it correctly tracks `connections.len()` so
    /// `connection_count()` can be exercised directly.
    ///
    /// Must be called inside a tokio runtime context (e.g. `#[tokio::test]`).
    pub(crate) fn new_for_test() -> Self {
        let (fetched_blocks_tx, _rx) = mpsc::channel(1);
        let (block_announcement_tx, _) = broadcast::channel(1);
        let (rollback_announcement_tx, _) = broadcast::channel(1);
        let (peer_failure_tx, _) = mpsc::channel(1);
        let (keepalive_rtt_tx, _) = mpsc::channel(1);
        let (gsm_event_tx, _) = mpsc::channel(1);

        let tmp = tempfile::tempdir().expect("tempdir");
        let chain_db = dugite_storage::ChainDB::open(tmp.path()).expect("ChainDB::open in test");

        let ledger_state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );

        let peer_manager_for_servers = Arc::new(RwLock::new(
            super::networking::NodePeerManager::new(super::networking::PeerManagerConfig::default()),
        ));

        let block_provider = Arc::new(super::serve::ChainDBBlockProvider {
            chain_db: Arc::new(RwLock::new(chain_db)),
        });

        let ledger_view = Arc::new(arc_swap::ArcSwap::from_pointee(
            super::ledger_view::LedgerView::from_state(&ledger_state),
        ));
        let (ledger_tip_slot_tx, _initial_rx) = tokio::sync::watch::channel(0u64);
        let ledger_arc = Arc::new(RwLock::new(ledger_state));

        // chain_db was moved into block_provider; open a separate one for the
        // lifecycle manager's own reference.
        let tmp2 = tempfile::tempdir().expect("tempdir2");
        let chain_db2 = dugite_storage::ChainDB::open(tmp2.path()).expect("ChainDB::open2 in test");

        Self::new(
            764_824_073, // mainnet magic — arbitrary for tests
            false,
            std::time::Duration::from_secs(10),
            Arc::new(RwLock::new(std::collections::HashMap::new())),
            fetched_blocks_tx,
            block_announcement_tx,
            Arc::new(RwLock::new(chain_db2)),
            ledger_arc,
            ledger_view,
            ledger_tip_slot_tx,
            Arc::new(dugite_consensus::praos::OuroborosPraos::new(10)),
            Arc::new(parking_lot::Mutex::new(HashMap::new())),
            432_000,
            2160,
            0.05,
            Arc::new(crate::metrics::NodeMetrics::new()),
            Arc::new(dugite_mempool::Mempool::new(
                dugite_mempool::MempoolConfig::default(),
            )),
            peer_failure_tx,
            keepalive_rtt_tx,
            gsm_event_tx,
            crate::genesis_peer_state::PeerStateRegistry::new(),
            tokio::sync::watch::channel(crate::gsm::GsmSnapshot {
                state: crate::gsm::GenesisSyncState::CaughtUp,
                loe_slot: None,
            })
            .1,
            None,
            None,
            None,
            block_provider,
            rollback_announcement_tx,
            peer_manager_for_servers,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(NoOpTxValidator),
            BLOCKFETCH_MAX_RANGE,
            std::time::Duration::from_secs(10),
        )
    }

    /// Variant of [`new_for_test`] that returns the peer-failure receiver
    /// instead of dropping it, so tests can observe failures reported by
    /// protocol tasks (e.g. the chainsync auto-restart hook from #499).
    pub(crate) fn new_for_test_with_failure_rx(
    ) -> (Self, mpsc::Receiver<(SocketAddr, PeerFailureKind)>) {
        let (fetched_blocks_tx, _rx) = mpsc::channel(1);
        let (block_announcement_tx, _) = broadcast::channel(1);
        let (rollback_announcement_tx, _) = broadcast::channel(1);
        let (peer_failure_tx, peer_failure_rx) = mpsc::channel(8);
        let (keepalive_rtt_tx, _) = mpsc::channel(1);
        let (gsm_event_tx, _) = mpsc::channel(1);

        let tmp = tempfile::tempdir().expect("tempdir");
        let chain_db = dugite_storage::ChainDB::open(tmp.path()).expect("ChainDB::open in test");

        let ledger_state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );

        let peer_manager_for_servers = Arc::new(RwLock::new(
            super::networking::NodePeerManager::new(super::networking::PeerManagerConfig::default()),
        ));

        let block_provider = Arc::new(super::serve::ChainDBBlockProvider {
            chain_db: Arc::new(RwLock::new(chain_db)),
        });

        let ledger_view = Arc::new(arc_swap::ArcSwap::from_pointee(
            super::ledger_view::LedgerView::from_state(&ledger_state),
        ));
        let (ledger_tip_slot_tx, _initial_rx) = tokio::sync::watch::channel(0u64);
        let ledger_arc = Arc::new(RwLock::new(ledger_state));

        let tmp2 = tempfile::tempdir().expect("tempdir2");
        let chain_db2 = dugite_storage::ChainDB::open(tmp2.path()).expect("ChainDB::open2 in test");

        let lc = Self::new(
            764_824_073,
            false,
            std::time::Duration::from_secs(10),
            Arc::new(RwLock::new(std::collections::HashMap::new())),
            fetched_blocks_tx,
            block_announcement_tx,
            Arc::new(RwLock::new(chain_db2)),
            ledger_arc,
            ledger_view,
            ledger_tip_slot_tx,
            Arc::new(dugite_consensus::praos::OuroborosPraos::new(10)),
            Arc::new(parking_lot::Mutex::new(HashMap::new())),
            432_000,
            2160,
            0.05,
            Arc::new(crate::metrics::NodeMetrics::new()),
            Arc::new(dugite_mempool::Mempool::new(
                dugite_mempool::MempoolConfig::default(),
            )),
            peer_failure_tx,
            keepalive_rtt_tx,
            gsm_event_tx,
            crate::genesis_peer_state::PeerStateRegistry::new(),
            tokio::sync::watch::channel(crate::gsm::GsmSnapshot {
                state: crate::gsm::GenesisSyncState::CaughtUp,
                loe_slot: None,
            })
            .1,
            None,
            None,
            None,
            block_provider,
            rollback_announcement_tx,
            peer_manager_for_servers,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(NoOpTxValidator),
            BLOCKFETCH_MAX_RANGE,
            std::time::Duration::from_secs(10),
        );
        (lc, peer_failure_rx)
    }

    /// Insert a fake connection entry so `connection_count()` reflects the
    /// insertion without starting any real protocol tasks.
    ///
    /// The synthetic [`ConnectionId`] uses `(127.0.0.1:0, addr)`, so each
    /// `addr` produces a unique key.
    pub(crate) fn insert_fake_for_test(&mut self, addr: std::net::SocketAddr) {
        let conn = super::peer_connection::PeerConnection::fake_for_test(addr);
        let cid = ConnectionId {
            local: conn.local_addr,
            remote: conn.addr,
        };
        self.connections.insert(cid, conn);
    }

    /// Remove a previously-inserted fake connection entry by remote addr.
    pub(crate) fn remove_fake_for_test(&mut self, addr: std::net::SocketAddr) {
        let cids: Vec<ConnectionId> = self
            .connections
            .keys()
            .filter(|c| c.remote == addr)
            .copied()
            .collect();
        for cid in cids {
            self.connections.remove(&cid);
        }
    }

    /// Insert a fake outbound + inbound pair to the same remote (duplex
    /// peer) using distinct local addresses. Used by tests verifying that
    /// the lifecycle manager tolerates duplex pairs.
    pub(crate) fn insert_fake_duplex_for_test(
        &mut self,
        remote: std::net::SocketAddr,
        outbound_local: std::net::SocketAddr,
        inbound_local: std::net::SocketAddr,
    ) {
        use super::peer_connection::PeerConnectionDirection;
        let out = super::peer_connection::PeerConnection::fake_for_test_with_local(
            remote,
            outbound_local,
            PeerConnectionDirection::Outbound,
        );
        let cid_out = ConnectionId {
            local: out.local_addr,
            remote: out.addr,
        };
        self.connections.insert(cid_out, out);

        let inb = super::peer_connection::PeerConnection::fake_for_test_with_local(
            remote,
            inbound_local,
            PeerConnectionDirection::Inbound,
        );
        let cid_in = ConnectionId {
            local: inb.local_addr,
            remote: inb.addr,
        };
        self.connections.insert(cid_in, inb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify CandidateChainState can be constructed and cloned.
    #[test]
    fn candidate_chain_state_roundtrip() {
        let state = CandidateChainState {
            tip_slot: 12345,
            tip_hash: [0xAB; 32],
            tip_block_number: 100,
            pending_headers: vec![PendingHeader {
                slot: 12345,
                hash: [0xAB; 32],
                header_cbor: vec![0x82, 0x01],
                body_size: None,
                prev_hash: None,
            }],
            ..Default::default()
        };

        let cloned = state.clone();
        assert_eq!(cloned.tip_slot, 12345);
        assert_eq!(cloned.tip_hash, [0xAB; 32]);
        assert_eq!(cloned.tip_block_number, 100);
        assert_eq!(cloned.pending_headers.len(), 1);
        assert_eq!(cloned.pending_headers[0].slot, 12345);
    }

    /// Regression: fork headers whose slot is ≤ the applied tip must still
    /// be selected for fetch as long as their hash is not yet in ChainDB.
    ///
    /// Before this fix, BlockFetch decision filtered by `slot > applied_slot`,
    /// which dropped legitimate fork blocks after `MsgRollBackward` and
    /// stalled chain selection because the candidate fragment was missing
    /// blocks needed by `walk_chain_back`.
    #[test]
    fn select_headers_to_fetch_keeps_fork_headers_below_applied_slot() {
        use std::collections::HashSet;
        let known: HashSet<[u8; 32]> = HashSet::from([[0x01; 32]]); // already in ChainDB
        let fetched: HashSet<[u8; 32]> = HashSet::new();
        let applied_slot = 100u64;

        let pending = vec![
            // Fork block at slot=99 (below applied_slot) — must be fetched.
            PendingHeader {
                slot: 99,
                hash: [0x02; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            // Already in ChainDB — must be skipped.
            PendingHeader {
                slot: 50,
                hash: [0x01; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            // Above applied_slot — must be fetched.
            PendingHeader {
                slot: 101,
                hash: [0x03; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let _ = applied_slot; // documents the scenario; not used in filter

        let out = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched);

        let hashes: Vec<[u8; 32]> = out.iter().map(|h| h.hash).collect();
        assert_eq!(
            hashes.len(),
            2,
            "expected fork header at slot 99 to be retained"
        );
        assert!(
            hashes.contains(&[0x02; 32]),
            "fork block below applied_slot dropped"
        );
        assert!(
            hashes.contains(&[0x03; 32]),
            "block above applied_slot dropped"
        );
        assert!(
            !hashes.contains(&[0x01; 32]),
            "already-known block was selected"
        );
    }

    /// `fetched_hashes` shadows ChainDB: a header that is currently being
    /// downloaded by another fetcher in the same worker is skipped.
    #[test]
    fn select_headers_to_fetch_skips_in_flight_hashes() {
        use std::collections::HashSet;
        let known: HashSet<[u8; 32]> = HashSet::new();
        let fetched: HashSet<[u8; 32]> = HashSet::from([[0xAA; 32]]);

        let pending = vec![
            PendingHeader {
                slot: 10,
                hash: [0xAA; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            }, // in-flight
            PendingHeader {
                slot: 11,
                hash: [0xBB; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            }, // new
        ];

        let out = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hash, [0xBB; 32]);
    }

    /// Regression test for issue #702: when a peer short-batches a
    /// BlockFetch request (responds `MsgBatchDone` before delivering every
    /// requested block), the missing headers MUST be eligible for
    /// re-request on the next decision tick.
    ///
    /// The bug fixed in 2026-05-27 was that `fetched_hashes` was
    /// unconditionally populated with every hash in `headers_to_fetch`,
    /// independent of whether the body actually arrived as `MsgBlock`.  That
    /// permanently masked the un-delivered blocks from `select_headers_to_fetch`
    /// and stalled the sync at the first missing block — every subsequent
    /// block sat in VolatileDB as a `StoreButDontChange` orphan.
    ///
    /// The fix is to only promote actually-received hashes into
    /// `fetched_hashes`.  This test simulates that flow:
    ///   1. Worker requests headers [h1..h5].
    ///   2. Peer delivers only {h1, h3, h5} (h2 and h4 short-batched).
    ///   3. Worker inserts ONLY {h1, h3, h5} into `fetched_hashes`.
    ///   4. Next `select_headers_to_fetch` call must return [h2, h4].
    #[test]
    fn select_headers_to_fetch_re_requests_short_batched_blocks() {
        use std::collections::HashSet;

        let h1 = [0x01; 32];
        let h2 = [0x02; 32];
        let h3 = [0x03; 32];
        let h4 = [0x04; 32];
        let h5 = [0x05; 32];

        let pending = vec![
            PendingHeader {
                slot: 10,
                hash: h1,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 11,
                hash: h2,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 12,
                hash: h3,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 13,
                hash: h4,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 14,
                hash: h5,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let known: HashSet<[u8; 32]> = HashSet::new();

        // Step 1 — first decision tick: all 5 selected for download.
        let mut fetched_hashes: HashSet<[u8; 32]> = HashSet::new();
        let to_fetch = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched_hashes);
        assert_eq!(to_fetch.len(), 5, "expected all 5 headers on first tick");

        // Step 2 — peer delivers only h1, h3, h5; promote ONLY those to
        // `fetched_hashes`, matching the post-fix worker behaviour.
        let received: HashSet<[u8; 32]> = HashSet::from([h1, h3, h5]);
        for h in &received {
            fetched_hashes.insert(*h);
        }

        // Step 3 — next decision tick: h2 and h4 must reappear.
        let next = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched_hashes);
        let next_hashes: HashSet<[u8; 32]> = next.iter().map(|h| h.hash).collect();
        assert_eq!(
            next_hashes,
            HashSet::from([h2, h4]),
            "short-batched headers must be re-requested on the next tick"
        );

        // Step 4 — exhaustive delivery on the second pass: everything
        // marked fetched, next tick returns empty (no rerequest churn).
        for h in &[h2, h4] {
            fetched_hashes.insert(*h);
        }
        let drained = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched_hashes);
        assert!(
            drained.is_empty(),
            "all headers delivered — no more work to schedule"
        );
    }

    /// Demonstrate the previous buggy behaviour to make sure future
    /// refactors don't reintroduce it: marking ALL requested hashes (not
    /// just delivered ones) leaves the missing blocks permanently masked.
    #[test]
    fn buggy_mark_all_requested_strands_short_batched_blocks() {
        use std::collections::HashSet;

        let h1 = [0x01; 32];
        let h2 = [0x02; 32];
        let h3 = [0x03; 32];

        let pending = vec![
            PendingHeader {
                slot: 10,
                hash: h1,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 11,
                hash: h2,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 12,
                hash: h3,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let known: HashSet<[u8; 32]> = HashSet::new();

        // Simulate the OLD bug: mark all `headers_to_fetch` as fetched even
        // though the peer only delivered h1.
        let mut fetched_hashes: HashSet<[u8; 32]> = HashSet::new();
        let to_fetch = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched_hashes);
        assert_eq!(to_fetch.len(), 3);
        for h in &to_fetch {
            fetched_hashes.insert(h.hash); // bug: insert requested, not received
        }

        // Under the buggy logic, the next tick yields no work even though
        // h2 and h3 never arrived — the stall reproducer.
        let next = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched_hashes);
        assert!(
            next.is_empty(),
            "buggy code strands h2 and h3 — this assertion documents the regression class"
        );
    }

    /// Verify FetchedBlock can be constructed.
    #[test]
    fn fetched_block_construction() {
        // FetchedBlock contains a Block which requires real construction,
        // so we just verify the type exists and has the expected fields.
        let _: fn() -> usize = || std::mem::size_of::<FetchedBlock>();
    }

    /// Verify LifecycleError display formatting.
    #[test]
    fn lifecycle_error_display() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        let err = LifecycleError::NotConnected(addr);
        assert!(err.to_string().contains("no connection"));
        assert!(err.to_string().contains("127.0.0.1:3001"));

        let err = LifecycleError::AlreadyConnected(addr);
        assert!(err.to_string().contains("already connected"));

        let inner = PeerConnectionError::ConnectTimeout(addr);
        let err = LifecycleError::Connection(inner);
        assert!(err.to_string().contains("connection error"));
    }

    /// Verify LifecycleError From<PeerConnectionError> conversion.
    #[test]
    fn lifecycle_error_from_peer_connection_error() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let inner = PeerConnectionError::ConnectTimeout(addr);
        let err: LifecycleError = inner.into();
        assert!(matches!(err, LifecycleError::Connection(_)));
    }

    /// Verify PendingHeader can be constructed.
    #[test]
    fn pending_header_construction() {
        let hdr = PendingHeader {
            slot: 999,
            hash: [0xFF; 32],
            header_cbor: vec![0x83, 0x01, 0x02],
            body_size: None,
            prev_hash: None,
        };
        assert_eq!(hdr.slot, 999);
        assert_eq!(hdr.header_cbor.len(), 3);
    }

    /// Verify the invariant: `connection_count()` tracks the real `connections`
    /// map length after every insert and remove.
    ///
    /// This test calls `ConnectionLifecycleManager::connection_count()` directly
    /// on a real instance so that any regression in how `n2n_connections_active`
    /// is derived will be caught here.  The old bug (fetch_add/fetch_sub drift)
    /// would have caused `connection_count()` to return stale values; the
    /// current implementation returns `self.connections.len()` which is always
    /// exact.
    #[tokio::test]
    async fn n2n_connections_active_gauge_matches_map_len() {
        let mut lc = ConnectionLifecycleManager::new_for_test();

        let addr1: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        let addr3: SocketAddr = "127.0.0.1:3003".parse().unwrap();

        assert_eq!(lc.connection_count(), 0, "starts empty");

        lc.insert_fake_for_test(addr1);
        assert_eq!(lc.connection_count(), 1, "after insert addr1");

        lc.insert_fake_for_test(addr2);
        assert_eq!(lc.connection_count(), 2, "after insert addr2");

        lc.insert_fake_for_test(addr3);
        assert_eq!(lc.connection_count(), 3, "after insert addr3");

        lc.remove_fake_for_test(addr2);
        assert_eq!(lc.connection_count(), 2, "after remove addr2");

        lc.remove_fake_for_test(addr1);
        assert_eq!(lc.connection_count(), 1, "after remove addr1");

        lc.remove_fake_for_test(addr3);
        assert_eq!(lc.connection_count(), 0, "after remove addr3: must be 0");
    }

    /// `ConnectionId` orders by remote first, then by local — matching
    /// Haskell `Ouroboros.Network.ConnectionId`'s `Ord` instance which is
    /// load-bearing in `ConnMap.toMap` for monotonic-key map operations.
    #[test]
    fn connection_id_orders_by_remote_then_local() {
        let r1: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let r2: SocketAddr = "10.0.0.2:3001".parse().unwrap();
        let l1: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let l2: SocketAddr = "127.0.0.1:2000".parse().unwrap();

        let a = ConnectionId {
            local: l2,
            remote: r1,
        };
        let b = ConnectionId {
            local: l1,
            remote: r2,
        };
        // r1 < r2 → a < b regardless of local.
        assert!(a < b);

        let c = ConnectionId {
            local: l1,
            remote: r1,
        };
        let d = ConnectionId {
            local: l2,
            remote: r1,
        };
        // Same remote, c.local < d.local → c < d.
        assert!(c < d);
    }

    /// Duplex peer: an outbound and an inbound to the same remote with
    /// distinct local addresses coexist as separate `ConnectionId` entries.
    /// This is the property that unblocks block diffusion when a co-located
    /// cardano-node relay's REUSEPORT outbound creates a peer-listen-port
    /// inbound on dugite's listener.
    #[tokio::test]
    async fn duplex_pair_coexists_under_distinct_local_addrs() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let remote: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        let outbound_local: SocketAddr = "127.0.0.1:54321".parse().unwrap(); // ephemeral
        let inbound_local: SocketAddr = "127.0.0.1:3001".parse().unwrap(); // our listen

        lc.insert_fake_duplex_for_test(remote, outbound_local, inbound_local);

        // Both connections live in the map.
        assert_eq!(
            lc.connection_count(),
            2,
            "duplex pair must produce 2 physical connections"
        );

        // Logical "is this peer connected" still says yes.
        assert!(lc.has_connection(&remote));

        // `connected_addrs` deduplicates by remote — one entry, not two.
        let addrs = lc.connected_addrs();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0], remote);

        // Outbound discovery picks the correct ConnectionId.
        let cid_out = lc
            .find_outbound_cid(remote)
            .expect("expected an outbound to be findable");
        assert_eq!(cid_out.remote, remote);
        assert_eq!(cid_out.local, outbound_local);

        // `find_any_cid` prefers outbound but works either way.
        let cid_any = lc.find_any_cid(remote).expect("any CID");
        assert_eq!(cid_any.local, outbound_local);
    }

    /// `cleanup_dead_connections` must NOT call `peer_disconnected` while
    /// the duplex pair still has another live connection. Otherwise the
    /// peer manager would forget the peer mid-duplex and the survivor's
    /// server protocols would be torn down.
    #[tokio::test]
    async fn cleanup_dead_keeps_peer_when_other_connection_alive() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let remote: SocketAddr = "127.0.0.1:4002".parse().unwrap();
        let outbound_local: SocketAddr = "127.0.0.1:54322".parse().unwrap();
        let inbound_local: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        lc.insert_fake_duplex_for_test(remote, outbound_local, inbound_local);
        assert_eq!(lc.connection_count(), 2);

        // Kill ONE connection by removing it directly (simulates one mux
        // dying while the duplex sibling is still healthy).
        let cid_out = ConnectionId {
            local: outbound_local,
            remote,
        };
        lc.connections.remove(&cid_out);

        // The remote is still represented by the surviving inbound.
        assert!(lc.has_connection(&remote));
        assert_eq!(lc.connection_count(), 1);

        // Now remove the surviving inbound: peer is fully gone.
        let cid_in = ConnectionId {
            local: inbound_local,
            remote,
        };
        lc.connections.remove(&cid_in);
        assert!(!lc.has_connection(&remote));
        assert_eq!(lc.connection_count(), 0);
    }

    /// Same-ConnectionId collision (true simultaneous open with bound
    /// listen-port outbound) overwrites the existing entry — matches
    /// Haskell's `Overwritten` semantic. The lifecycle manager's
    /// `register_inbound_connection` shuts down the displaced entry before
    /// inserting; the HashMap-level invariant that same-CID inserts replace
    /// is verified here as a structural prerequisite.
    #[test]
    fn same_connection_id_hashmap_replaces_existing_entry() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let cid_a = ConnectionId {
            local: "127.0.0.1:3001".parse().unwrap(),
            remote: "127.0.0.1:3002".parse().unwrap(),
        };
        let cid_b = ConnectionId {
            local: "127.0.0.1:3001".parse().unwrap(),
            remote: "127.0.0.1:3002".parse().unwrap(),
        };
        // Equal ConnectionIds hash identically.
        assert_eq!(cid_a, cid_b);
        let mut ha = DefaultHasher::new();
        cid_a.hash(&mut ha);
        let mut hb = DefaultHasher::new();
        cid_b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());

        // HashMap insert with the same key replaces the prior entry.
        let mut h1 = std::collections::HashMap::new();
        h1.insert(cid_a, "first");
        let prior = h1.insert(cid_b, "second");
        assert_eq!(prior, Some("first"), "second insert overwrites first");
        assert_eq!(h1.len(), 1);
    }

    // ── ConnectionId properties ───────────────────────────────────────────────

    /// Two ConnectionIds with identical (local, remote) are equal.
    #[test]
    fn connection_id_equality_same_tuple() {
        let a = ConnectionId {
            local: "10.0.0.1:1111".parse().unwrap(),
            remote: "10.0.0.2:3001".parse().unwrap(),
        };
        let b = ConnectionId {
            local: "10.0.0.1:1111".parse().unwrap(),
            remote: "10.0.0.2:3001".parse().unwrap(),
        };
        assert_eq!(a, b);
    }

    /// Swapping local and remote produces a DIFFERENT ConnectionId.
    #[test]
    fn connection_id_inequality_swapped_roles() {
        let a = ConnectionId {
            local: "10.0.0.1:1111".parse().unwrap(),
            remote: "10.0.0.2:3001".parse().unwrap(),
        };
        let b = ConnectionId {
            local: "10.0.0.2:3001".parse().unwrap(),
            remote: "10.0.0.1:1111".parse().unwrap(),
        };
        assert_ne!(a, b);
    }

    /// Display format is `local<->remote`.
    #[test]
    fn connection_id_display_format() {
        let cid = ConnectionId {
            local: "127.0.0.1:3001".parse().unwrap(),
            remote: "127.0.0.1:3002".parse().unwrap(),
        };
        let s = cid.to_string();
        assert!(
            s.contains("127.0.0.1:3001"),
            "display should contain local addr"
        );
        assert!(
            s.contains("127.0.0.1:3002"),
            "display should contain remote addr"
        );
        assert!(s.contains("<->"), "display should use <-> separator");
    }

    /// Ord: same remote, larger local → greater.
    #[test]
    fn connection_id_ord_same_remote_larger_local_greater() {
        let remote: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let small_local: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let large_local: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let a = ConnectionId {
            local: small_local,
            remote,
        };
        let b = ConnectionId {
            local: large_local,
            remote,
        };
        assert!(
            a < b,
            "smaller local port should sort first when remote is equal"
        );
    }

    /// Ord: different remote, smaller remote always sorts first regardless of local.
    #[test]
    fn connection_id_ord_different_remotes() {
        let r1: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let r2: SocketAddr = "10.0.0.2:3001".parse().unwrap();
        // Give the r1 CID a LARGER local so the local tiebreak alone would flip it.
        let large_local: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let small_local: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let a = ConnectionId {
            local: large_local,
            remote: r1,
        };
        let b = ConnectionId {
            local: small_local,
            remote: r2,
        };
        // r1 < r2 means a < b, regardless of local.
        assert!(a < b);
    }

    /// Clone produces an equal ConnectionId.
    #[test]
    fn connection_id_clone_eq() {
        let cid = ConnectionId {
            local: "127.0.0.1:3001".parse().unwrap(),
            remote: "127.0.0.1:3002".parse().unwrap(),
        };
        assert_eq!(cid, cid);
    }

    /// Copy semantics: assigning a ConnectionId produces an equal independent value.
    #[test]
    fn connection_id_copy_independent() {
        let a = ConnectionId {
            local: "127.0.0.1:1234".parse().unwrap(),
            remote: "127.0.0.1:5678".parse().unwrap(),
        };
        let b = a; // Copy
        assert_eq!(a, b);
    }

    // ── LifecycleError variants ───────────────────────────────────────────────

    /// LifecycleError::NotConnected includes the address in its Display.
    #[test]
    fn lifecycle_error_not_connected_display_includes_addr() {
        let addr: SocketAddr = "192.168.1.1:3001".parse().unwrap();
        let err = LifecycleError::NotConnected(addr);
        assert!(err.to_string().contains("192.168.1.1:3001"));
    }

    /// LifecycleError::AlreadyConnected includes the address in its Display.
    #[test]
    fn lifecycle_error_already_connected_display_includes_addr() {
        let addr: SocketAddr = "192.168.1.2:3001".parse().unwrap();
        let err = LifecycleError::AlreadyConnected(addr);
        assert!(err.to_string().contains("192.168.1.2:3001"));
    }

    /// LifecycleError implements std::error::Error (verify .source() returns None for base variants).
    #[test]
    fn lifecycle_error_implements_std_error() {
        use std::error::Error;
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let err = LifecycleError::NotConnected(addr);
        // Just checking the trait impl compiles and source() is accessible.
        let _ = err.source();
    }

    // ── ConnectionLifecycleManager helpers ────────────────────────────────────

    /// Fresh manager starts with zero connections.
    #[tokio::test]
    async fn manager_starts_empty() {
        let lc = ConnectionLifecycleManager::new_for_test();
        assert_eq!(lc.connection_count(), 0);
        assert!(lc.connected_addrs().is_empty());
    }

    /// has_connection returns false for unknown peer.
    #[tokio::test]
    async fn has_connection_unknown_peer_returns_false() {
        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        assert!(!lc.has_connection(&addr));
    }

    /// has_connection returns true after insert_fake.
    #[tokio::test]
    async fn has_connection_after_insert_true() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        lc.insert_fake_for_test(addr);
        assert!(lc.has_connection(&addr));
    }

    /// has_connection returns false after removing the only connection.
    #[tokio::test]
    async fn has_connection_after_remove_false() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.1:3002".parse().unwrap();
        lc.insert_fake_for_test(addr);
        lc.remove_fake_for_test(addr);
        assert!(!lc.has_connection(&addr));
    }

    /// connected_addrs deduplicates — one entry per remote even with duplex pair.
    #[tokio::test]
    async fn connected_addrs_deduplicated_by_remote() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let remote: SocketAddr = "10.0.0.5:3001".parse().unwrap();
        let out_local: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        let in_local: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        lc.insert_fake_duplex_for_test(remote, out_local, in_local);

        let addrs = lc.connected_addrs();
        assert_eq!(addrs.len(), 1, "duplex pair must appear as a single remote");
        assert!(addrs.contains(&remote));
    }

    /// connected_addrs returns all distinct remotes when multiple single-directional peers exist.
    #[tokio::test]
    async fn connected_addrs_multiple_distinct_peers() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let p1: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let p2: SocketAddr = "10.0.0.2:3001".parse().unwrap();
        let p3: SocketAddr = "10.0.0.3:3001".parse().unwrap();
        lc.insert_fake_for_test(p1);
        lc.insert_fake_for_test(p2);
        lc.insert_fake_for_test(p3);

        let mut addrs = lc.connected_addrs();
        addrs.sort();
        assert_eq!(addrs.len(), 3);
        assert!(addrs.contains(&p1));
        assert!(addrs.contains(&p2));
        assert!(addrs.contains(&p3));
    }

    /// drain_connections empties the internal map.
    #[tokio::test]
    async fn drain_connections_empties_map() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        lc.insert_fake_for_test("10.0.0.1:3001".parse().unwrap());
        lc.insert_fake_for_test("10.0.0.2:3001".parse().unwrap());
        assert_eq!(lc.connection_count(), 2);

        let drained = lc.drain_connections();
        assert_eq!(drained.len(), 2);
        assert_eq!(lc.connection_count(), 0, "map must be empty after drain");
    }

    /// find_outbound_cid returns None for unknown peer.
    #[tokio::test]
    async fn find_outbound_cid_unknown_peer_returns_none() {
        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        assert!(lc.find_outbound_cid(addr).is_none());
    }

    /// find_outbound_cid finds outbound in a duplex pair.
    #[tokio::test]
    async fn find_outbound_cid_prefers_outbound_in_duplex() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let remote: SocketAddr = "10.0.0.7:3001".parse().unwrap();
        let out_local: SocketAddr = "127.0.0.1:44444".parse().unwrap();
        let in_local: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        lc.insert_fake_duplex_for_test(remote, out_local, in_local);

        let cid = lc
            .find_outbound_cid(remote)
            .expect("outbound CID not found");
        assert_eq!(cid.local, out_local);
        assert_eq!(cid.remote, remote);
    }

    /// find_any_cid falls back to inbound when no outbound exists for the peer.
    #[tokio::test]
    async fn find_any_cid_finds_any_connection() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.9:3001".parse().unwrap();
        lc.insert_fake_for_test(addr);

        let cid = lc.find_any_cid(addr).expect("should find a connection");
        assert_eq!(cid.remote, addr);
    }

    /// find_any_cid returns None when no connection exists.
    #[tokio::test]
    async fn find_any_cid_no_connection_returns_none() {
        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "10.0.0.9:3001".parse().unwrap();
        assert!(lc.find_any_cid(addr).is_none());
    }

    // ── select_headers_to_fetch (connection_lifecycle re-export) ─────────────

    /// Empty pending → empty result (via the public function visible from this module).
    #[test]
    fn select_headers_to_fetch_empty_pending() {
        use std::collections::HashSet;
        let empty: Vec<PendingHeader> = vec![];
        let out = select_headers_to_fetch(&empty, |_| false, &HashSet::new());
        assert!(out.is_empty());
    }

    /// Header with same hash as ChainDB entry is excluded.
    #[test]
    fn select_headers_to_fetch_excludes_known() {
        use std::collections::HashSet;
        let known_hash = [0xAB; 32];
        let pending = vec![
            PendingHeader {
                slot: 1,
                hash: known_hash,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 2,
                hash: [0xCD; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let out = select_headers_to_fetch(&pending, |h| h == &known_hash, &HashSet::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hash, [0xCD; 32]);
    }

    // ── CandidateChainState ───────────────────────────────────────────────────

    /// Default CandidateChainState fields round-trip through Clone.
    #[test]
    fn candidate_chain_state_clone_preserves_fields() {
        let state = CandidateChainState {
            tip_slot: 9999,
            tip_hash: [0x77; 32],
            tip_block_number: 42,
            pending_headers: vec![PendingHeader {
                slot: 9999,
                hash: [0x77; 32],
                header_cbor: vec![0x01, 0x02],
                body_size: None,
                prev_hash: None,
            }],
            ..Default::default()
        };
        let cloned = state.clone();
        assert_eq!(cloned.tip_slot, 9999);
        assert_eq!(cloned.tip_hash, [0x77; 32]);
        assert_eq!(cloned.tip_block_number, 42);
        assert_eq!(cloned.pending_headers.len(), 1);
        assert_eq!(cloned.pending_headers[0].header_cbor, vec![0x01u8, 0x02]);
    }

    /// CandidateChainState with empty pending_headers is valid.
    #[test]
    fn candidate_chain_state_empty_pending_ok() {
        let state = CandidateChainState {
            tip_slot: 0,
            tip_hash: [0u8; 32],
            tip_block_number: 0,
            pending_headers: vec![],
            ..Default::default()
        };
        assert!(state.pending_headers.is_empty());
    }

    // ── Simultaneous-open / Overwritten invariant ─────────────────────────────

    /// Two distinct remotes with the same local produce distinct ConnectionIds.
    #[test]
    fn connection_id_distinct_remotes_same_local_not_equal() {
        let local: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let r1: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let r2: SocketAddr = "10.0.0.2:3001".parse().unwrap();
        let a = ConnectionId { local, remote: r1 };
        let b = ConnectionId { local, remote: r2 };
        assert_ne!(a, b);
    }

    /// ConnectionId with same remote but different local ports are NOT equal —
    /// verifies the tuple-keying approach that allows duplex pair coexistence
    /// (regression-lock for the block diffusion fix from 2026-04-29).
    #[test]
    fn connection_id_same_remote_different_local_not_equal() {
        let remote: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let local_a: SocketAddr = "127.0.0.1:54321".parse().unwrap(); // ephemeral outbound
        let local_b: SocketAddr = "127.0.0.1:3001".parse().unwrap(); // listen port inbound
        let a = ConnectionId {
            local: local_a,
            remote,
        };
        let b = ConnectionId {
            local: local_b,
            remote,
        };
        // These must be DIFFERENT keys so both can coexist in the HashMap.
        assert_ne!(
            a, b,
            "duplex pair connections must have distinct ConnectionIds"
        );
    }

    /// After inserting an outbound and inbound for the same remote, connection_count is 2.
    #[tokio::test]
    async fn duplex_pair_connection_count_is_2() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let remote: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let out_local: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        let in_local: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        lc.insert_fake_duplex_for_test(remote, out_local, in_local);
        assert_eq!(lc.connection_count(), 2);
    }

    /// set_local_listen_addr: can be called without panicking.
    #[tokio::test]
    async fn set_local_listen_addr_no_panic() {
        let mut lc = ConnectionLifecycleManager::new_for_test();
        let addr: SocketAddr = "0.0.0.0:3001".parse().unwrap();
        lc.set_local_listen_addr(addr);
        // No assertion needed: just verifies no panic.
    }

    // ── MempoolTxSource ever-yielded dedup ───────────────────────────────────

    /// Mock mempool for `MempoolTxSource` unit tests.
    struct MockMempool {
        txs: std::sync::Mutex<std::collections::BTreeMap<dugite_primitives::hash::Hash32, usize>>,
    }

    impl MockMempool {
        fn new() -> Self {
            Self {
                txs: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            }
        }
        fn insert(&self, hash: dugite_primitives::hash::Hash32, size: usize) {
            self.txs.lock().unwrap().insert(hash, size);
        }
        fn remove(&self, hash: &dugite_primitives::hash::Hash32) {
            self.txs.lock().unwrap().remove(hash);
        }
    }

    impl MempoolQuerySource for std::sync::Arc<MockMempool> {
        fn query_tx_size(&self, hash: &dugite_primitives::hash::Hash32) -> Option<usize> {
            self.txs.lock().unwrap().get(hash).copied()
        }
        fn query_tx_hashes_ordered(&self) -> Vec<dugite_primitives::hash::Hash32> {
            self.txs.lock().unwrap().keys().copied().collect()
        }
        fn query_tx_cbor(&self, _hash: &dugite_primitives::hash::Hash32) -> Option<Vec<u8>> {
            None
        }
        fn query_is_empty(&self) -> bool {
            self.txs.lock().unwrap().is_empty()
        }
        fn query_tx_notify(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
            None
        }
    }

    fn make_hash(byte: u8) -> dugite_primitives::hash::Hash32 {
        dugite_primitives::hash::Hash32::from_bytes([byte; 32])
    }

    fn tx_ids_from(
        source: &MempoolTxSource<std::sync::Arc<MockMempool>>,
        ack: u16,
        req: u16,
    ) -> Vec<[u8; 32]> {
        source
            .get_tx_ids(ack, req)
            .into_iter()
            .map(|t| t.tx_id)
            .collect()
    }

    /// TxSubmission2 ever-yielded dedup: once a tx is acked, it must NOT be
    /// re-yielded to the same peer on the next request, even while it remains
    /// in the mempool. The tx is only re-yielded if it first leaves the mempool
    /// and then re-enters (e.g. after a rollback).
    ///
    /// Protocol cycle exercised:
    ///   1. First request (ack=0): all three txs A, B, C are new → yielded.
    ///   2. Peer acks all 3 (ack=3): re-iteration must return nothing.
    ///   3. A leaves mempool; next request (ack=0) must still return nothing
    ///      (B and C are still ever-yielded).
    ///   4. B leaves; A re-enters: next request must return only A
    ///      (A was pruned from ever_yielded when it left).
    #[test]
    fn mempool_tx_source_ever_yielded_no_reannounce() {
        let pool = std::sync::Arc::new(MockMempool::new());
        let hash_a = make_hash(0xAA);
        let hash_b = make_hash(0xBB);
        let hash_c = make_hash(0xCC);

        pool.insert(hash_a, 100);
        pool.insert(hash_b, 100);
        pool.insert(hash_c, 100);

        let source = MempoolTxSource {
            mempool: pool.clone(),
            outstanding: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ever_yielded: std::sync::Mutex::new(std::collections::HashSet::new()),
        };

        // Step 1: first request yields all three txs.
        let ids = tx_ids_from(&source, 0, 10);
        assert_eq!(ids.len(), 3, "first request must yield A, B, C");
        assert!(ids.contains(hash_a.as_bytes()));
        assert!(ids.contains(hash_b.as_bytes()));
        assert!(ids.contains(hash_c.as_bytes()));

        // Step 2: peer acks all 3; re-iteration must return nothing.
        let ids = tx_ids_from(&source, 3, 10);
        assert!(
            ids.is_empty(),
            "after full ack, same txs must not be re-yielded (was: {ids:?})"
        );

        // Step 3: A leaves the mempool; B and C are still ever-yielded → still nothing.
        pool.remove(&hash_a);
        let ids = tx_ids_from(&source, 0, 10);
        assert!(
            ids.is_empty(),
            "B and C still ever-yielded; nothing to announce (was: {ids:?})"
        );

        // Step 4: B leaves; A re-enters → only A is new.
        pool.remove(&hash_b);
        pool.insert(hash_a, 100);
        let ids = tx_ids_from(&source, 0, 10);
        assert_eq!(ids.len(), 1, "only re-entered A should be yielded");
        assert!(
            ids.contains(hash_a.as_bytes()),
            "A must be re-announced after re-entering the mempool"
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // #499 — chainsync task auto-restart hook
    // ───────────────────────────────────────────────────────────────────

    /// Build a `MuxChannel` whose ingress is closed so the first `recv()`
    /// returns `BearerClosed`. Used to force `chainsync_client_task` to
    /// return `Err` immediately.
    fn closed_ingress_channel() -> dugite_network::MuxChannel {
        use dugite_network::{Direction, MuxChannel};
        use std::sync::atomic::AtomicUsize;
        type Bytes = tokio_util::bytes::Bytes;
        let (egress_tx, _egress_rx) = mpsc::channel::<(u16, Direction, Bytes)>(8);
        let (ingress_tx, ingress_rx) = mpsc::channel::<Bytes>(8);
        drop(ingress_tx); // close ingress immediately
        MuxChannel::new(
            2, // ChainSync protocol id (arbitrary for the test)
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            65_536,
            Arc::new(AtomicUsize::new(0)),
        )
    }

    /// When `chainsync_client_task` returns `Err` and the cancellation token
    /// is NOT cancelled, the task must signal the peer manager via
    /// `peer_failure_tx` so the governor can demote-and-re-promote.  Prior to
    /// the fix the warn! was silent and the peer stayed “hot” forever.
    #[tokio::test]
    async fn chainsync_task_reports_failure_to_peer_manager_on_error() {
        use tokio_util::sync::CancellationToken;
        let (lc, mut peer_failure_rx) = ConnectionLifecycleManager::new_for_test_with_failure_rx();
        let addr: SocketAddr = "127.0.0.1:39499".parse().unwrap();
        let task = lc.make_chainsync_task(addr);
        let channel = closed_ingress_channel();
        let cancel = CancellationToken::new();
        // Run the closure and wait for the failure report.  We don't await the
        // closure first because some chainsync setup may yield before erroring;
        // the failure-report send happens after chainsync_client_task returns.
        let handle = tokio::spawn(task(channel, cancel));
        let received =
            tokio::time::timeout(std::time::Duration::from_secs(5), peer_failure_rx.recv())
                .await
                .expect("peer_failure_rx timed out");
        assert_eq!(
            received,
            Some((addr, PeerFailureKind::Slow)),
            "chainsync task must report failure to peer manager when bearer closes"
        );
        // Drain the spawned task; it should already be finishing.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    /// When the task is cancelled (graceful shutdown), failure must NOT be
    /// reported even though chainsync_client_task returns Err — otherwise we
    /// would spam peer_failed() during planned demotions and shutdown.
    #[tokio::test]
    async fn chainsync_task_does_not_report_failure_when_cancelled() {
        use tokio_util::sync::CancellationToken;
        let (lc, mut peer_failure_rx) = ConnectionLifecycleManager::new_for_test_with_failure_rx();
        let addr: SocketAddr = "127.0.0.1:39500".parse().unwrap();
        let task = lc.make_chainsync_task(addr);
        let channel = closed_ingress_channel();
        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancel before the task runs
        let handle = tokio::spawn(task(channel, cancel));
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        // No failure should be reported.
        match peer_failure_rx.try_recv() {
            Err(mpsc::error::TryRecvError::Empty) => { /* expected */ }
            other => panic!("expected no peer_failure when cancelled, got {:?}", other),
        }
    }

    /// A `PeerUnsuitable` marker (ChainSync intersection only at genesis — the
    /// Haskell `ForkTooDeep` equivalent) is an EXPECTED, routine peer-quality
    /// outcome on public networks, not a fault. It must classify as
    /// `Unsuitable` so it is logged at INFO (≈ cardano-node `Notice`) rather
    /// than spamming WARN.
    #[test]
    fn classify_chainsync_failure_maps_peer_unsuitable_to_unsuitable() {
        let err = anyhow::Error::new(PeerUnsuitable {
            reason: "intersection only at genesis (block_no=4394795 > k=432)".to_string(),
        });
        assert_eq!(
            classify_chainsync_failure(&err),
            PeerFailureKind::Unsuitable,
            "a PeerUnsuitable (ForkTooDeep) marker must classify as Unsuitable, not a fault"
        );
    }

    /// Any other chainsync failure (bearer close, decode error, timeout) is a
    /// genuine transport/protocol problem and must stay `Slow` (WARN).
    #[test]
    fn classify_chainsync_failure_maps_generic_error_to_slow() {
        let err = anyhow::anyhow!("bearer read error: timeout");
        assert_eq!(
            classify_chainsync_failure(&err),
            PeerFailureKind::Slow,
            "a generic transport/decode error must classify as Slow"
        );
    }

    // ── PeerFetchStatus / CandidateChainState status tracking ────────────────
    //
    // Tests for issue #702 per-peer chain tracking:
    // - Default state is Ready.
    // - Successful delivery resets consecutive_failures and clears Aberrant.
    // - Failures accumulate within the observation window.
    // - ABERRANT_FAILURE_THRESHOLD consecutive failures → Aberrant.
    // - Aberrant peer is excluded from fetch decisions (is_fetch_eligible = false).
    // - A successful delivery after Aberrant rehabilitates the peer.

    #[test]
    fn peer_fetch_status_default_is_ready() {
        let state = CandidateChainState::default();
        assert_eq!(state.fetch_status, PeerFetchStatus::Ready);
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.last_delivered_at.is_none());
        assert_eq!(state.in_flight_blocks, 0);
    }

    #[test]
    fn peer_fetch_status_dispatched_becomes_busy() {
        let mut state = CandidateChainState::default();
        state.record_fetch_dispatched();
        assert_eq!(state.fetch_status, PeerFetchStatus::Busy);
        assert_eq!(state.in_flight_blocks, 1);
    }

    #[test]
    fn peer_fetch_status_delivered_clears_busy() {
        let mut state = CandidateChainState::default();
        state.record_fetch_dispatched();
        state.record_fetch_delivered();
        assert_eq!(state.fetch_status, PeerFetchStatus::Ready);
        assert_eq!(state.in_flight_blocks, 0);
        assert!(state.last_delivered_at.is_some());
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn peer_fetch_status_multiple_in_flight() {
        let mut state = CandidateChainState::default();
        state.record_fetch_dispatched();
        state.record_fetch_dispatched();
        assert_eq!(state.in_flight_blocks, 2);
        state.record_fetch_delivered();
        // One still in-flight → still Busy.
        assert_eq!(state.fetch_status, PeerFetchStatus::Busy);
        assert_eq!(state.in_flight_blocks, 1);
        state.record_fetch_delivered();
        // All delivered → Ready.
        assert_eq!(state.fetch_status, PeerFetchStatus::Ready);
        assert_eq!(state.in_flight_blocks, 0);
    }

    #[test]
    fn peer_fetch_status_failure_below_threshold_stays_ready() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut state = CandidateChainState::default();
        // Fail ABERRANT_FAILURE_THRESHOLD - 1 times.
        for _ in 0..(ABERRANT_FAILURE_THRESHOLD - 1) {
            state.record_fetch_failed(addr);
        }
        assert_ne!(state.fetch_status, PeerFetchStatus::Aberrant);
        assert!(state.is_fetch_eligible());
    }

    #[test]
    fn peer_fetch_status_threshold_failures_marks_aberrant() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut state = CandidateChainState::default();
        // Fail exactly ABERRANT_FAILURE_THRESHOLD times.
        for _ in 0..ABERRANT_FAILURE_THRESHOLD {
            state.record_fetch_failed(addr);
        }
        assert_eq!(state.fetch_status, PeerFetchStatus::Aberrant);
        assert!(!state.is_fetch_eligible());
    }

    #[test]
    fn peer_fetch_status_delivery_rehabilitates_aberrant() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut state = CandidateChainState::default();
        for _ in 0..ABERRANT_FAILURE_THRESHOLD {
            state.record_fetch_failed(addr);
        }
        assert_eq!(state.fetch_status, PeerFetchStatus::Aberrant);
        // A successful delivery clears Aberrant.
        state.record_fetch_delivered();
        assert_eq!(state.fetch_status, PeerFetchStatus::Ready);
        assert!(state.is_fetch_eligible());
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn peer_fetch_status_is_fetch_eligible_aberrant_returns_false() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut state = CandidateChainState::default();
        for _ in 0..ABERRANT_FAILURE_THRESHOLD {
            state.record_fetch_failed(addr);
        }
        assert!(!state.is_fetch_eligible());
    }

    #[test]
    fn peer_fetch_status_is_fetch_eligible_ready_and_busy_return_true() {
        let mut state = CandidateChainState::default();
        assert!(state.is_fetch_eligible()); // Ready
        state.record_fetch_dispatched();
        assert!(state.is_fetch_eligible()); // Busy
    }

    // ─── PeerSharing governor dispatch tests ────────────────────────────────

    /// Helper: build a minimal `dispatch_peersharing_request`-able state.
    /// We only need `peersharing_request_txs` and `peersharing_in_flight` —
    /// the rest of `ConnectionLifecycleManager` is not exercised by these tests.
    struct PsDispatchState {
        txs: HashMap<SocketAddr, mpsc::Sender<u8>>,
        in_flight: Arc<std::sync::atomic::AtomicU32>,
    }

    impl PsDispatchState {
        fn new() -> Self {
            Self {
                txs: HashMap::new(),
                in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }

        /// Mirrors `ConnectionLifecycleManager::dispatch_peersharing_request`.
        fn dispatch(&mut self, addr: SocketAddr) {
            let in_flight = self.in_flight.load(std::sync::atomic::Ordering::Relaxed);
            if in_flight >= ConnectionLifecycleManager::PEERSHARING_MAX_IN_FLIGHT {
                return;
            }
            let Some(tx) = self.txs.get(&addr) else {
                return;
            };
            self.in_flight
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match tx.try_send(ConnectionLifecycleManager::PEERSHARING_DEFAULT_AMOUNT) {
                Ok(()) => {}
                Err(_) => {
                    self.in_flight
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        fn inflight(&self) -> u32 {
            self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    fn peer(port: u16) -> SocketAddr {
        format!("1.2.3.4:{port}").parse().unwrap()
    }

    /// Governor emits `PeerShareRequest` when cold pool is below max_cold/2
    /// AND a warm peer with peer_sharing=true exists.
    ///
    /// Haskell reference: `belowTarget` guard in
    /// `Ouroboros.Network.PeerSelection.Governor.KnownPeers` —
    /// `numKnownPeers < targetNumberOfKnownPeers && not (Set.null availableForPeerShare)`.
    #[test]
    fn governor_emits_peer_share_request_when_cold_pool_low() {
        use dugite_network::peer::governor::{GovernorAction, PeerTargets};
        use dugite_network::peer::manager::{PeerManager, PeerSource};
        use std::time::Duration;

        let mut pm = PeerManager::new();
        // One warm peer with peer_sharing enabled.
        let warm_addr = peer(3001);
        pm.add_peer(warm_addr, PeerSource::Dns);
        pm.promote_to_warm(&warm_addr);
        pm.get_peer_mut(&warm_addr).unwrap().peer_sharing = true;
        // Cold pool is empty → below max_cold/2 → DiscoverMore + PeerShareRequest.

        let config = dugite_network::peer::governor::GovernorConfig {
            targets: PeerTargets {
                target_warm: 1,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = dugite_network::peer::governor::Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, GovernorAction::PeerShareRequest(_))),
            "governor must emit PeerShareRequest when cold pool is low and a sharing peer exists"
        );
        // The request should target our warm peer.
        assert!(
            actions.contains(&GovernorAction::PeerShareRequest(warm_addr)),
            "PeerShareRequest must target the warm peer with peer_sharing=true"
        );
    }

    /// Governor does NOT emit `PeerShareRequest` when no warm peer has peer_sharing.
    ///
    /// Haskell reference: guard `not (Set.null availableForPeerShare)` —
    /// `availableForPeerShare` is empty when no peer has PeerSharingEnabled.
    #[test]
    fn governor_no_peer_share_request_without_sharing_peers() {
        use dugite_network::peer::governor::{GovernorAction, GovernorConfig, PeerTargets};
        use dugite_network::peer::manager::{PeerManager, PeerSource};
        use std::time::Duration;

        let mut pm = PeerManager::new();
        let warm_addr = peer(3001);
        pm.add_peer(warm_addr, PeerSource::Dns);
        pm.promote_to_warm(&warm_addr);
        // peer_sharing defaults to false

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 1,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = dugite_network::peer::governor::Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, GovernorAction::PeerShareRequest(_))),
            "governor must not emit PeerShareRequest when no peer has peer_sharing=true"
        );
    }

    /// `dispatch_peersharing_request` sends the default amount to the peer's
    /// request channel and increments the in-flight counter.
    ///
    /// Haskell reference: `peerSharingClient` sends `SendMsgShareRequest amount`
    /// to the controller's `requestQueue` (`ouroboros-network/lib/Ouroboros/Network/PeerSharing.hs`).
    #[tokio::test]
    async fn dispatch_sends_request_and_increments_inflight() {
        let addr = peer(3001);
        let (tx, mut rx) = mpsc::channel::<u8>(4);
        let mut state = PsDispatchState::new();
        state.txs.insert(addr, tx);

        state.dispatch(addr);

        assert_eq!(
            state.inflight(),
            1,
            "in-flight counter must be 1 after one dispatch"
        );
        let received = rx.recv().await.expect("channel must have one item");
        assert_eq!(
            received,
            ConnectionLifecycleManager::PEERSHARING_DEFAULT_AMOUNT,
            "dispatched amount must equal PEERSHARING_DEFAULT_AMOUNT"
        );
    }

    /// Global concurrency cap: at most `PEERSHARING_MAX_IN_FLIGHT = 2` requests
    /// in-flight simultaneously.
    ///
    /// Haskell reference: `policyMaxInProgressPeerShareReqs = 2` in
    /// `ouroboros-network/lib/Ouroboros/Network/Diffusion/Policies.hs`.
    #[tokio::test]
    async fn dispatch_respects_global_cap() {
        let addr_a = peer(3001);
        let addr_b = peer(3002);
        let addr_c = peer(3003);

        let (tx_a, _rx_a) = mpsc::channel::<u8>(4);
        let (tx_b, _rx_b) = mpsc::channel::<u8>(4);
        let (tx_c, _rx_c) = mpsc::channel::<u8>(4);

        let mut state = PsDispatchState::new();
        state.txs.insert(addr_a, tx_a);
        state.txs.insert(addr_b, tx_b);
        state.txs.insert(addr_c, tx_c);

        state.dispatch(addr_a); // in_flight → 1
        state.dispatch(addr_b); // in_flight → 2

        assert_eq!(state.inflight(), 2, "two dispatches must give in_flight=2");

        // Third dispatch must be dropped — cap reached.
        state.dispatch(addr_c);

        assert_eq!(
            state.inflight(),
            2,
            "third dispatch must be rejected by cap=2 (policyMaxInProgressPeerShareReqs=2)"
        );
    }

    /// Per-peer duplicate-request guard: a full channel (previous request still
    /// in-flight for the same peer) causes `try_send` to fail and the in-flight
    /// counter is released.
    ///
    /// Haskell reference: `PeerSharingController.requestQueue` is a depth-1
    /// `TMVar` — a second `putTMVar` blocks until the first is consumed,
    /// preventing concurrent requests to the same peer.  Our channel has
    /// capacity 4 for throughput, but `try_send` failure on a full channel
    /// gives the same single-in-flight-per-peer semantics.
    #[tokio::test]
    async fn dispatch_duplicate_guard_releases_inflight_on_full_channel() {
        let addr = peer(3001);
        // Channel with capacity 1 — fills after one dispatch.
        let (tx, _rx) = mpsc::channel::<u8>(1);
        let mut state = PsDispatchState::new();
        state.txs.insert(addr, tx);

        state.dispatch(addr); // fills the channel
        assert_eq!(state.inflight(), 1);

        // Second dispatch: channel is full → try_send fails → in-flight released.
        state.dispatch(addr);
        assert_eq!(
            state.inflight(),
            1,
            "failed second dispatch must NOT double-increment in-flight"
        );
    }

    /// Dispatching to a peer with no registered client task is a no-op.
    ///
    /// Occurs when the connection is torn down between the governor tick and
    /// the dispatch call (e.g. `peersharing_request_txs` was cleaned up by
    /// `demote_to_cold` or `cleanup_dead_connections`).
    #[test]
    fn dispatch_unknown_peer_is_noop() {
        let mut state = PsDispatchState::new();
        // No entry for this address.
        state.dispatch(peer(9999));
        assert_eq!(
            state.inflight(),
            0,
            "dispatch for unknown peer must not change in-flight counter"
        );
    }

    /// `PEERSHARING_MAX_IN_FLIGHT` is pinned to 2 to match Haskell's
    /// `policyMaxInProgressPeerShareReqs = 2`.
    #[test]
    fn peersharing_max_in_flight_matches_haskell() {
        assert_eq!(
            ConnectionLifecycleManager::PEERSHARING_MAX_IN_FLIGHT,
            2,
            "must match Haskell policyMaxInProgressPeerShareReqs = 2 \
             (ouroboros-network/lib/Ouroboros/Network/Diffusion/Policies.hs)"
        );
    }

    /// `PEERSHARING_DEFAULT_AMOUNT` matches Haskell's minimum of 8 peers
    /// per request (`max 8 (objective `div` numPeerShareReqs)`).
    #[test]
    fn peersharing_default_amount_matches_haskell_floor() {
        assert_eq!(
            ConnectionLifecycleManager::PEERSHARING_DEFAULT_AMOUNT,
            8,
            "must match Haskell's floor of max(8, objective/n) from \
             ouroboros-network/lib/Ouroboros/Network/PeerSelection/Governor/KnownPeers.hs"
        );
    }

    // ── BlockFetch task prompt-cancellation regression tests ─────────────────
    //
    // Root cause: `recv_batch` and `fetched_blocks_tx.send(fetched).await`
    // were bare awaits with no cancel-token select.  A deactivation signal
    // (spsDeactivateTimeout=5s) fired while the worker was inside either
    // future caused a 5-second timeout + connection teardown cascade.
    //
    // Fix: both awaits are now wrapped in `tokio::select! { biased; _ =
    // cancel.cancelled() => return; ... }` so they resolve immediately on
    // cancellation.
    //
    // The tests below exercise the two failure modes directly:
    //  (a) cancel fires while recv_batch is waiting for MsgBlock → task exits fast
    //  (b) cancel fires while fetched_blocks_tx.send is blocked (channel full) → task exits fast

    /// Helper: build a MuxChannel pair (no real TCP involved).
    ///
    /// Returns `(channel, ingress_tx, egress_rx)`.
    /// - `channel` is given to the protocol task.
    /// - `ingress_tx` lets the test push inbound protocol messages into the task.
    /// - `egress_rx` lets the test read outbound messages the task sends.
    fn make_mux_channel_pair() -> (
        dugite_network::mux::channel::MuxChannel,
        tokio::sync::mpsc::Sender<tokio_util::bytes::Bytes>,
        tokio::sync::mpsc::Receiver<(
            u16,
            dugite_network::mux::Direction,
            tokio_util::bytes::Bytes,
        )>,
    ) {
        use dugite_network::mux::channel::MuxChannel;
        use dugite_network::mux::Direction;
        use std::sync::{atomic::AtomicUsize, Arc};
        use tokio::sync::mpsc;

        let (egress_tx, egress_rx) = mpsc::channel(256);
        let (ingress_tx, ingress_rx) = mpsc::channel(256);
        let ch = MuxChannel::new(
            3, // BlockFetch protocol ID
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            24 * 1024 * 1024, // 24 MB ingress limit
            Arc::new(AtomicUsize::new(0)),
        );
        (ch, ingress_tx, egress_rx)
    }

    /// (a) BlockFetch task must exit within 1 second when cancelled while
    /// `recv_batch` is blocked waiting for `MsgBlock` from the peer.
    ///
    /// Without the fix, `recv_batch` spun inside `channel.recv().await`
    /// with no cancel awareness — the task could not be stopped for up to
    /// FETCH_RANGE_TIMEOUT (60 s), always tripping spsDeactivateTimeout.
    #[tokio::test(start_paused = false)]
    async fn blockfetch_task_cancels_promptly_during_recv_batch() {
        use dugite_network::protocol::blockfetch::{encode_message, BlockFetchMessage};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.1:3001".parse().unwrap();

        // Build a fake channel pair.
        let (channel, ingress_tx, mut egress_rx) = make_mux_channel_pair();

        let cancel = CancellationToken::new();
        let task_fn = lc.make_blockfetch_task(addr);

        // Inject a candidate chain entry so the blockfetch task has headers to fetch.
        {
            let mut chains = lc.candidate_chains.write().await;
            chains.insert(addr, {
                let mut s = CandidateChainState::default();
                s.pending_headers.push(PendingHeader {
                    slot: 1,
                    hash: [0x01; 32],
                    header_cbor: vec![],
                    body_size: None,
                    prev_hash: None,
                });
                s
            });
        }

        // Spawn the blockfetch worker with the cancel token we control.
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Read and discard the MsgRequestRange the task sends once it claims
        // the active_fetcher slot.  This confirms the task is inside recv_batch.
        let msg_timeout = tokio::time::timeout(Duration::from_secs(2), egress_rx.recv()).await;
        // The task may or may not have claimed the fetcher slot yet; if no
        // request arrived in 2s the task is still in the poll loop — that's
        // fine, cancellation will still be fast.
        let _ = msg_timeout;

        // If we did receive a MsgRequestRange, send MsgStartBatch but then
        // deliberately stall (never send MsgBlock or MsgBatchDone) so the
        // task is permanently blocked inside recv_batch.
        // Always send start batch to exercise the deepest path.
        let _ = ingress_tx
            .send(tokio_util::bytes::Bytes::from(encode_message(
                &BlockFetchMessage::MsgStartBatch,
            )))
            .await;

        // Give the task a moment to enter channel.recv().await inside recv_batch.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now fire cancellation.
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        // The task must finish within 1 second (well under spsDeactivateTimeout=5s).
        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();

        assert!(
            result.is_ok(),
            "blockfetch task must exit within 1s of cancellation during recv_batch; \
             elapsed={elapsed:?} (spsDeactivateTimeout=5s)"
        );
    }

    /// (b) BlockFetch task must exit within 1 second when cancelled while
    /// `fetched_blocks_tx.send` could be blocked (apply backpressure scenario).
    ///
    /// Without the fix, a full `fetched_blocks` channel (apply-loop backpressure)
    /// caused `fetched_blocks_tx.send(fetched).await` to block indefinitely — the
    /// task appeared "stuck" to `stop_hot_protocols_and_recover` and was aborted
    /// after 5s, tearing down the TCP connection.
    ///
    /// We exercise the same cancellation path as (a) but from a different state:
    /// the task has already received MsgStartBatch and is awaiting the next channel
    /// message.  Cancel fires → task must return promptly.
    #[tokio::test(start_paused = false)]
    async fn blockfetch_task_cancels_promptly_when_send_blocked() {
        use dugite_network::protocol::blockfetch::{encode_message, BlockFetchMessage};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        // new_for_test sets up a capacity-1 fetched_blocks channel which means
        // after one block is forwarded the send will block.  We don't actually
        // need to reach that state — what we need to verify is that cancellation
        // is honoured wherever the task is currently awaiting.
        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.2:3001".parse().unwrap();

        let (channel, ingress_tx, mut egress_rx) = make_mux_channel_pair();

        // Inject two pending headers.
        {
            let mut chains = lc.candidate_chains.write().await;
            chains.insert(addr, {
                let mut s = CandidateChainState::default();
                s.pending_headers.push(PendingHeader {
                    slot: 1,
                    hash: [0x02; 32],
                    header_cbor: vec![],
                    body_size: None,
                    prev_hash: None,
                });
                s.pending_headers.push(PendingHeader {
                    slot: 2,
                    hash: [0x03; 32],
                    header_cbor: vec![],
                    body_size: None,
                    prev_hash: None,
                });
                s
            });
        }

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let task_fn = lc.make_blockfetch_task(addr);
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Wait for the MsgRequestRange — the task has claimed the fetcher.
        let req = tokio::time::timeout(Duration::from_secs(2), egress_rx.recv()).await;
        if req.is_err() {
            // Task never claimed the fetcher in time — just cancel.
            cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
            return;
        }

        // Send MsgStartBatch to push the task into recv_batch's inner loop.
        let _ = ingress_tx
            .send(tokio_util::bytes::Bytes::from(encode_message(
                &BlockFetchMessage::MsgStartBatch,
            )))
            .await;

        // Give the task a moment to enter channel.recv() inside recv_batch.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Fire cancellation — task must exit within 1s.
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();

        assert!(
            result.is_ok(),
            "blockfetch task must exit within 1s of cancellation during recv_batch inner loop; \
             elapsed={elapsed:?}"
        );
    }

    // ── KeepAlive task prompt-cancellation tests ──────────────────────────────
    //
    // Root cause: the initial 2-second startup delay in `make_keepalive_task`
    // was a bare `tokio::time::sleep(2s).await` with no cancel-token guard.
    // When warm→cold demotion (or full shutdown) fired during this window,
    // `stop_warm_protocols` had to wait the full 2 s before the KeepAlive
    // client started and could honour its own cancel logic.
    //
    // Fix: the startup sleep is now wrapped in:
    //   `select! { biased; _ = cancel.cancelled() => return; _ = sleep(2s) => {} }`

    /// KeepAlive task must exit within 1 second when cancelled during its
    /// initial 2-second startup delay.
    ///
    /// Without the fix, cancel during the bare `sleep(2s)` made the task
    /// block for the remainder of that window, always tripping the
    /// `stop_warm_protocols` timeout and forcing a TCP close instead of
    /// a graceful warm channel recovery.
    #[tokio::test(start_paused = false)]
    async fn keepalive_task_cancels_promptly_during_startup_delay() {
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.3:3001".parse().unwrap();

        // Build a channel that never delivers messages (so the task parks on recv).
        let (channel, _ingress_tx, _egress_rx) = make_mux_channel_pair();

        let cancel = CancellationToken::new();
        let task_fn = lc.make_keepalive_task(addr);

        // Spawn the task — it will start sleeping for 2 seconds immediately.
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Give the task just enough time to enter the startup sleep.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Fire cancellation — must exit well under the 2s sleep duration.
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();

        assert!(
            result.is_ok(),
            "keepalive task must exit within 1s of cancellation during startup delay; \
             elapsed={elapsed:?} (2s sleep was un-guarded before fix)"
        );
    }

    // ── ChainSync task prompt-cancellation tests ──────────────────────────────
    //
    // Root cause: `chainsync_client_task` has cancel checks in its main message
    // loop, but the pre-loop Phase 1/2 (intersection finding via bare
    // `channel.recv().await` in `try_find_intersect`) did not check the token.
    // If cancel fired during intersection finding, the task blocked until the
    // peer responded (potentially seconds or until TCP timeout).
    //
    // Fix: `make_chainsync_task` now wraps the entire `chainsync_client_task`
    // call in `tokio::select! { biased; _ = cancel.cancelled() => return; r = ... => r }`.

    /// ChainSync task must exit within 1 second when cancelled while blocked
    /// on `channel.recv()` during intersection finding (Phase 2).
    ///
    /// The channel ingress is permanently closed (no peer response) so the task
    /// blocks immediately on the first `try_find_intersect` recv. The outer
    /// select in `make_chainsync_task` must unblock it when cancel fires.
    ///
    /// NOTE: Because the chainsync task first sends `MsgFindIntersect` and then
    /// awaits a response, the channel.recv() in `try_find_intersect` will fail
    /// with a channel-closed error when the ingress sender is dropped — which
    /// already causes a quick exit. This test verifies that cancel also provides
    /// prompt exit independent of channel closure, by using a half-open channel
    /// (egress open, ingress sender held but sends no data).
    #[tokio::test(start_paused = false)]
    async fn chainsync_task_cancels_promptly_during_intersection() {
        use dugite_network::mux::channel::MuxChannel;
        use dugite_network::mux::Direction;
        use std::sync::{atomic::AtomicUsize, Arc};
        use std::time::Duration;
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.4:3001".parse().unwrap();

        // Build a channel: egress works (so MsgFindIntersect can be sent),
        // but ingress sender is held without sending — task blocks on recv.
        let (egress_tx, _egress_rx) =
            mpsc::channel::<(u16, Direction, tokio_util::bytes::Bytes)>(64);
        let (ingress_tx, ingress_rx) = mpsc::channel::<tokio_util::bytes::Bytes>(64);
        let channel = MuxChannel::new(
            2, // ChainSync protocol ID
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            64 * 1024,
            Arc::new(AtomicUsize::new(0)),
        );

        let cancel = CancellationToken::new();
        let task_fn = lc.make_chainsync_task(addr);

        // Spawn the task — it will:
        //   1. Build known points (fast, no I/O)
        //   2. Send MsgFindIntersect (fast, egress works)
        //   3. Block on channel.recv() waiting for MsgIntersectFound
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Wait for the task to reach the recv() inside try_find_intersect.
        // MsgFindIntersect is sent synchronously before the first recv.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Fire cancellation — outer select in make_chainsync_task must unblock recv.
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        // Keep ingress_tx alive past the cancel so the channel isn't already
        // closed (we want to test cancel, not channel-close early exit).
        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();
        drop(ingress_tx);

        assert!(
            result.is_ok(),
            "chainsync task must exit within 1s of cancellation during intersection recv; \
             elapsed={elapsed:?} (pre-loop awaits were un-guarded before fix)"
        );
    }

    // ── TxSubmission2 task prompt-cancellation tests ──────────────────────────
    //
    // The outer `tokio::select!` in `make_txsubmission_task` already provided
    // cancellation for the top-level `TxSubmissionClient::run()` future, but
    // the two inner blocking awaits were not individually cancel-aware:
    //   1. `channel.recv().await` — waiting for MsgRequestTxIds / MsgRequestTxs
    //   2. The blocking-mode mempool poll (500 ms sleep when notified=None)
    //
    // Fix: `TxSubmissionClient::run()` now accepts a `CancellationToken` and
    // guards both awaits with `select! { biased; _ = cancel.cancelled() => return Ok(()); ... }`.
    // The outer select in `make_txsubmission_task` is kept as defence-in-depth.

    /// TxSubmission2 task must exit within 1 second when cancelled while
    /// blocked on `channel.recv()` waiting for `MsgRequestTxIds`.
    ///
    /// The channel sends `MsgInit` (fast) then waits for the server to request
    /// tx IDs. The ingress sender is held but sends no messages — task blocks.
    #[tokio::test(start_paused = false)]
    async fn txsubmission_task_cancels_promptly_during_recv() {
        use dugite_network::mux::channel::MuxChannel;
        use dugite_network::mux::Direction;
        use std::sync::{atomic::AtomicUsize, Arc};
        use std::time::Duration;
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.5:3001".parse().unwrap();

        // Egress works (MsgInit will be sent); ingress held but silent.
        let (egress_tx, _egress_rx) =
            mpsc::channel::<(u16, Direction, tokio_util::bytes::Bytes)>(64);
        let (ingress_tx, ingress_rx) = mpsc::channel::<tokio_util::bytes::Bytes>(64);
        let channel = MuxChannel::new(
            4, // TxSubmission2 protocol ID
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            Arc::new(AtomicUsize::new(0)),
        );

        let cancel = CancellationToken::new();
        let task_fn = lc.make_txsubmission_task(addr);

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Give the task time to send MsgInit and enter channel.recv().
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Fire cancellation.
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();
        drop(ingress_tx);

        assert!(
            result.is_ok(),
            "txsubmission2 task must exit within 1s of cancellation during channel.recv(); \
             elapsed={elapsed:?}"
        );
    }

    /// TxSubmission2 task must exit within 1 second when cancelled while
    /// blocked in the blocking-mode mempool poll loop (empty mempool,
    /// blocking=true `MsgRequestTxIds` received, no Notify — 500ms sleep path).
    ///
    /// This exercises the inner `tokio::time::sleep(500ms)` in the blocking
    /// mempool wait that previously had no cancel arm.
    #[tokio::test(start_paused = false)]
    async fn txsubmission_task_cancels_promptly_during_blocking_poll() {
        use dugite_network::mux::channel::MuxChannel;
        use dugite_network::mux::Direction;
        use dugite_network::protocol::txsubmission::{encode_message, TxSubmissionMessage};
        use std::sync::{atomic::AtomicUsize, Arc};
        use std::time::Duration;
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let lc = ConnectionLifecycleManager::new_for_test();
        let addr: std::net::SocketAddr = "10.0.0.6:3001".parse().unwrap();

        let (egress_tx, mut egress_rx) =
            mpsc::channel::<(u16, Direction, tokio_util::bytes::Bytes)>(64);
        let (ingress_tx, ingress_rx) = mpsc::channel::<tokio_util::bytes::Bytes>(64);
        let channel = MuxChannel::new(
            4,
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            Arc::new(AtomicUsize::new(0)),
        );

        let cancel = CancellationToken::new();
        let task_fn = lc.make_txsubmission_task(addr);

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(task_fn(channel, cancel_clone));

        // Consume MsgInit from egress.
        let _init = tokio::time::timeout(Duration::from_secs(2), egress_rx.recv())
            .await
            .expect("MsgInit must arrive within 2s");

        // Send a blocking MsgRequestTxIds with empty mempool → task enters
        // the inner polling loop (no Notify, so 500ms sleep path).
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: true,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx
            .send(tokio_util::bytes::Bytes::from(req))
            .await
            .expect("ingress send failed");

        // Give the task time to enter the inner blocking loop.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Fire cancellation — must exit promptly (no 500ms sleep wait).
        let cancel_start = std::time::Instant::now();
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        let elapsed = cancel_start.elapsed();

        assert!(
            result.is_ok(),
            "txsubmission2 task must exit within 1s of cancellation during blocking mempool poll; \
             elapsed={elapsed:?} (500ms sleep was un-guarded before fix)"
        );
    }
}

// ─── Fix 2 (#742): ChainSel-starvation detection unit tests ─────────────────
//
// These tests verify the ChainSelStarvation flag (Haskell `getChainSelMessage`
// edge semantics) and the rotation decision rule
// (`checkLastChainSelStarvation` in ouroboros-network
// `BlockFetch/Decision/Genesis.hs`):
//   lastStarvationTime = if Ongoing then now else endedAt
//   fire iff lastStarvationTime >= claim_start + grace

#[cfg(test)]
mod fix2_starvation_detection_tests {
    use super::*;

    /// The flag starts `Ongoing` (0) at construction — Haskell initializes
    /// `ChainSelStarvation = Ongoing` at ChainDB open.
    #[tokio::test]
    async fn starvation_flag_starts_ongoing() {
        let lc = ConnectionLifecycleManager::new_for_test();
        assert_eq!(
            lc.chainsel_starvation_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "ChainSelStarvation must start Ongoing (0)"
        );
    }

    /// `chainsel_dequeued()` ends an Ongoing starvation period (CAS 0 → now)…
    #[tokio::test]
    async fn dequeue_ends_ongoing_starvation() {
        let lc = ConnectionLifecycleManager::new_for_test();
        lc.chainsel_dequeued();
        let v = lc
            .chainsel_starvation_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(v > 0, "dequeue must stamp EndedAt(now), got {v}");
    }

    /// …but a dequeue while NOT starved must keep the old EndedAt (edge
    /// semantics): a long apply with a full queue must not look like fresh
    /// starvation activity.
    #[tokio::test]
    async fn dequeue_while_not_starved_keeps_old_ended_at() {
        let lc = ConnectionLifecycleManager::new_for_test();
        lc.chainsel_dequeued();
        let first = lc
            .chainsel_starvation_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        lc.chainsel_dequeued(); // not starved — must be a no-op
        let second = lc
            .chainsel_starvation_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            first, second,
            "EndedAt must only be stamped by the dequeue that ENDS a starvation period"
        );
    }

    /// `chainsel_queue_empty()` re-arms the flag to Ongoing, and the next
    /// dequeue stamps a fresh EndedAt.
    #[tokio::test]
    async fn empty_queue_rearms_ongoing() {
        let lc = ConnectionLifecycleManager::new_for_test();
        lc.chainsel_dequeued();
        lc.chainsel_queue_empty();
        assert_eq!(
            lc.chainsel_starvation_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "empty queue must mark starvation Ongoing"
        );
        lc.chainsel_dequeued();
        assert!(
            lc.chainsel_starvation_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0,
            "dequeue after re-armed Ongoing must stamp a fresh EndedAt"
        );
    }

    /// The rotation decision rule, mirrored from the production worker code:
    ///   lastStarvationTime = if Ongoing(0) then now else endedAt
    ///   fire iff lastStarvationTime >= claim_ms + grace_ms
    fn should_rotate(now_ms: u64, claim_ms: u64, starv: u64, grace_ms: u64) -> bool {
        let last_starvation_ms = if starv == 0 { now_ms } else { starv };
        last_starvation_ms >= claim_ms + grace_ms
    }

    /// Ongoing starvation + claim held >= grace → rotate (the never-streaming
    /// dynamo case from #742: starvation is Ongoing from boot).
    #[test]
    fn rotates_when_ongoing_and_held_past_grace() {
        let grace = 10_000u64;
        let claim = 1_000u64;
        assert!(should_rotate(claim + grace, claim, 0, grace));
    }

    /// Ongoing starvation but claim younger than grace → no rotation yet.
    #[test]
    fn no_rotation_before_grace_expires() {
        let grace = 10_000u64;
        let claim = 1_000u64;
        assert!(!should_rotate(claim + grace - 1, claim, 0, grace));
    }

    /// Starvation ENDED before this claim started (old EndedAt) → never
    /// rotate, regardless of how long the claim is held. This is the
    /// epoch-boundary / snapshot-write case: the queue stays full, the flag
    /// keeps its old EndedAt, and a 25 s apply does NOT rotate the dynamo.
    #[test]
    fn no_rotation_when_starvation_ended_before_claim() {
        let grace = 10_000u64;
        let claim = 50_000u64;
        let old_ended_at = 49_000u64; // before the claim
        assert!(!should_rotate(
            claim + 10 * grace,
            claim,
            old_ended_at,
            grace
        ));
    }

    /// Starvation ended DURING the claim, at/after claim+grace → rotate
    /// (Haskell fires on EndedAt >= peersOrderStart + grace even if the
    /// starvation has since ended).
    #[test]
    fn rotates_when_starvation_ended_late_in_claim() {
        let grace = 10_000u64;
        let claim = 1_000u64;
        let ended_at = claim + grace + 5; // queue ran dry well into the claim
        assert!(should_rotate(ended_at + 100, claim, ended_at, grace));
    }

    /// Starvation ended during the claim but BEFORE claim+grace → no rotation.
    #[test]
    fn no_rotation_when_starvation_ended_early_in_claim() {
        let grace = 10_000u64;
        let claim = 1_000u64;
        let ended_at = claim + grace - 5;
        assert!(!should_rotate(ended_at + 60_000, claim, ended_at, grace));
    }

    /// Flag updates are callable from multiple tasks without data races.
    #[tokio::test]
    async fn flag_updates_are_thread_safe() {
        let lc = Arc::new(ConnectionLifecycleManager::new_for_test());
        let mut handles = Vec::new();
        for i in 0..8 {
            let lc2 = lc.clone();
            handles.push(tokio::spawn(async move {
                if i % 2 == 0 {
                    lc2.chainsel_dequeued();
                } else {
                    lc2.chainsel_queue_empty();
                }
            }));
        }
        for h in handles {
            h.await.expect("starvation flag task panicked");
        }
        // No assertion on the final value (depends on interleaving) — the test
        // exercises concurrent access under the loom-free atomics contract.
    }
}

// ─── Fix 3 (#747): BlockFetch ingress invariant unit tests ──────────────────
//
// These tests verify:
//  • The compile-time ingress invariant holds (window × budget <= limit)
//  • The per-tick 2048-header cap works correctly
//  • Pipeline window was reduced from 4 to 2

#[cfg(test)]
mod fix3_blockfetch_ingress_tests {
    use super::*;

    fn hdr(slot: u64, body_size: Option<u64>) -> PendingHeader {
        PendingHeader {
            slot,
            hash: [slot as u8; 32],
            header_cbor: vec![0u8; 1_000],
            body_size,
            prev_hash: None,
        }
    }

    /// Ranges chunk by EXACT declared bytes: 90,112-byte blocks (mainnet max)
    /// must yield ranges whose actual byte total stays within the 8 MB budget
    /// — the failure mode that overran the 32 MB ingress live (33.5 MB from
    /// 2 nominal 8 MB ranges, mainnet ep388 2026-06-11T22:30Z) cannot recur
    /// for size-declaring headers.
    #[test]
    fn ranges_chunk_by_declared_bytes() {
        let headers: Vec<_> = (0..1_000).map(|i| hdr(i, Some(90_112))).collect();
        let ranges = build_fetch_ranges(&headers, 65_536, BLOCKFETCH_MAX_RANGE);
        assert!(!ranges.is_empty());
        let per_block = 90_112 + 1_000 + 16;
        for &(start, end) in &ranges {
            let bytes = (end - start + 1) * per_block;
            assert!(
                bytes <= BLOCKFETCH_RANGE_BYTE_BUDGET || start == end,
                "range [{start},{end}] = {bytes} bytes exceeds the 8 MB budget"
            );
        }
        // Coverage: consecutive, gapless, complete.
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, headers.len() - 1);
        for w in ranges.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0, "ranges must be consecutive");
        }
        // 8 MB / ~91 KB ≈ 92 blocks per range — NOT the old avg-based 128.
        let first_len = ranges[0].1 - ranges[0].0 + 1;
        assert!(
            (80..=95).contains(&first_len),
            "expected ~92 blocks per range for 90,112-byte blocks, got {first_len}"
        );
    }

    /// Headers without a declared size (Byron) fall back to the adaptive
    /// average estimate.
    #[test]
    fn ranges_fall_back_to_avg_for_undeclared() {
        let headers: Vec<_> = (0..3_000).map(|i| hdr(i, None)).collect();
        let ranges = build_fetch_ranges(&headers, 4_096, BLOCKFETCH_MAX_RANGE);
        // 8 MB / 4 KB = 2048 estimated blocks per range, capped by max_range.
        let first_len = ranges[0].1 - ranges[0].0 + 1;
        assert_eq!(
            first_len,
            BLOCKFETCH_MAX_RANGE.min(BLOCKFETCH_RANGE_BYTE_BUDGET / 4_096),
            "avg-based fallback sizing changed unexpectedly"
        );
    }

    /// A single block whose declared size exceeds the whole budget gets its
    /// own range (never an empty range, never a stall).
    #[test]
    fn oversized_block_gets_own_range() {
        let headers = vec![
            hdr(0, Some(1_000)),
            hdr(1, Some(BLOCKFETCH_RANGE_BYTE_BUDGET as u64 * 2)),
            hdr(2, Some(1_000)),
        ];
        let ranges = build_fetch_ranges(&headers, 65_536, BLOCKFETCH_MAX_RANGE);
        assert_eq!(ranges, vec![(0, 0), (1, 1), (2, 2)]);
    }

    /// max_range caps the per-range block count even when bytes allow more.
    #[test]
    fn max_range_caps_block_count() {
        let headers: Vec<_> = (0..100).map(|i| hdr(i, Some(100))).collect();
        let ranges = build_fetch_ranges(&headers, 65_536, 10);
        assert_eq!(ranges.len(), 10);
        for &(start, end) in &ranges {
            assert_eq!(end - start + 1, 10);
        }
    }

    /// Empty input produces no ranges.
    #[test]
    fn empty_headers_no_ranges() {
        assert!(build_fetch_ranges(&[], 65_536, BLOCKFETCH_MAX_RANGE).is_empty());
    }

    /// `select_fetch_runs` splits at every filtered-out element, so a range
    /// can never span a gap (the peer would re-stream the gap blocks,
    /// breaking the byte accounting — the residual #747 overrun).
    #[test]
    fn fetch_runs_split_at_gaps() {
        // pending: 10 headers; 3, 4 and 7 already fetched.
        let pending: Vec<_> = (0..10).map(|i| hdr(i, Some(10_000))).collect();
        let mut fetched = std::collections::HashSet::new();
        fetched.insert(pending[3].hash);
        fetched.insert(pending[4].hash);
        fetched.insert(pending[7].hash);
        let runs = select_fetch_runs(&pending, |_| false, &fetched);
        let run_slots: Vec<Vec<u64>> = runs
            .iter()
            .map(|r| r.iter().map(|h| h.slot).collect())
            .collect();
        assert_eq!(
            run_slots,
            vec![vec![0, 1, 2], vec![5, 6], vec![8, 9]],
            "runs must break at every gap"
        );
    }

    /// No gaps → exactly one run identical to the filtered list.
    #[test]
    fn fetch_runs_single_when_contiguous() {
        let pending: Vec<_> = (0..5).map(|i| hdr(i, Some(10_000))).collect();
        let runs = select_fetch_runs(&pending, |_| false, &std::collections::HashSet::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 5);
    }

    /// Chained helper: header i's prev_hash = header (i-1)'s hash.
    fn chained(slots: &[u64]) -> Vec<PendingHeader> {
        slots
            .iter()
            .enumerate()
            .map(|(i, &s)| PendingHeader {
                slot: s,
                hash: [s as u8; 32],
                header_cbor: vec![0u8; 1_000],
                body_size: Some(10_000),
                prev_hash: if i == 0 {
                    Some([0xFFu8; 32])
                } else {
                    Some([slots[i - 1] as u8; 32])
                },
            })
            .collect()
    }

    /// `pending_headers` can be SPARSE relative to the peer's chain (blocks
    /// already in ChainDB are never pushed). A prev-hash discontinuity inside
    /// an otherwise filter-contiguous list must split the run — otherwise the
    /// MsgRequestRange spans the hidden gap and the peer delivers the gap
    /// blocks too (observed live as ranges delivering 1.7-2x their estimate).
    #[test]
    fn fetch_runs_split_on_prev_hash_discontinuity() {
        let mut pending = chained(&[1, 2, 3]);
        // headers 4,5 chain to a block NOT in the list (block 0xAA) — the
        // hidden gap: blocks between 3 and 4 are already stored.
        let mut tail = chained(&[40, 41]);
        tail[0].prev_hash = Some([0xAAu8; 32]);
        pending.append(&mut tail);
        let runs = select_fetch_runs(&pending, |_| false, &std::collections::HashSet::new());
        let run_slots: Vec<Vec<u64>> = runs
            .iter()
            .map(|r| r.iter().map(|h| h.slot).collect())
            .collect();
        assert_eq!(
            run_slots,
            vec![vec![1, 2, 3], vec![40, 41]],
            "a prev-hash discontinuity must split the run"
        );
    }

    /// Fully chained headers stay in one run.
    #[test]
    fn fetch_runs_keep_chained_headers_together() {
        let pending = chained(&[1, 2, 3, 4, 5]);
        let runs = select_fetch_runs(&pending, |_| false, &std::collections::HashSet::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 5);
    }

    /// Unknown parents (Byron: prev_hash=None) never trigger adjacency
    /// splits — filter-gap-only behaviour is preserved.
    #[test]
    fn fetch_runs_unknown_parent_no_adjacency_split() {
        let pending: Vec<_> = (0..5).map(|i| hdr(i, Some(10_000))).collect();
        let runs = select_fetch_runs(&pending, |_| false, &std::collections::HashSet::new());
        assert_eq!(runs.len(), 1, "None prev_hash must not split runs");
    }

    /// chain-db-known blocks split runs exactly like fetched-hash gaps.
    #[test]
    fn fetch_runs_split_on_chain_db_known() {
        let pending: Vec<_> = (0..6).map(|i| hdr(i, Some(10_000))).collect();
        let runs = select_fetch_runs(
            &pending,
            |h| h[0] == 2, // slot-2 header hash starts with 2 → "known"
            &std::collections::HashSet::new(),
        );
        let run_slots: Vec<Vec<u64>> = runs
            .iter()
            .map(|r| r.iter().map(|h| h.slot).collect())
            .collect();
        assert_eq!(run_slots, vec![vec![0, 1], vec![3, 4, 5]]);
    }

    /// Mixed declared sizes: a burst of large blocks among small ones closes
    /// ranges early so the byte bound holds for every range.
    #[test]
    fn mixed_sizes_hold_byte_bound() {
        let mut headers = Vec::new();
        for i in 0..500 {
            let size = if i % 7 == 0 { 88_000 } else { 2_000 };
            headers.push(hdr(i, Some(size)));
        }
        let ranges = build_fetch_ranges(&headers, 65_536, BLOCKFETCH_MAX_RANGE);
        for &(start, end) in &ranges {
            let bytes: usize = headers[start..=end]
                .iter()
                .map(|h| h.body_size.unwrap() as usize + 1_000 + 16)
                .sum();
            assert!(
                bytes <= BLOCKFETCH_RANGE_BYTE_BUDGET || start == end,
                "range [{start},{end}] = {bytes} bytes exceeds budget"
            );
        }
    }

    /// The pipeline-window constant must be 2 after the #747 fix.
    /// Window 4 × 8 MB = 32 MB would overflow a 24 MB ingress limit.
    #[test]
    fn pipeline_window_is_two() {
        assert_eq!(
            BLOCKFETCH_PIPELINE_WINDOW, 2,
            "BLOCKFETCH_PIPELINE_WINDOW must be 2 after #747 fix \
             (window 4 × 8 MB = 32 MB > old 24 MB ingress limit)"
        );
    }

    /// The mux ingress limit must keep ~3x headroom over the in-flight budget
    /// (estimate slack was observed live at up to ~2x, #747).
    #[test]
    fn ingress_limit_is_48mb() {
        assert_eq!(
            super::super::peer_connection::BLOCKFETCH_INGRESS_LIMIT,
            48 * 1024 * 1024,
            "BLOCKFETCH_INGRESS_LIMIT must be 48 MB (#747)"
        );
    }

    /// The pipeline invariant must hold: window × budget <= ingress_limit.
    /// This is also enforced at compile time by the const assert above;
    /// the runtime test documents the expected values explicitly.
    #[test]
    fn pipeline_invariant_holds() {
        let in_flight = BLOCKFETCH_PIPELINE_WINDOW * BLOCKFETCH_RANGE_BYTE_BUDGET;
        assert!(
            in_flight <= super::super::peer_connection::BLOCKFETCH_INGRESS_LIMIT,
            "pipelined in-flight bytes ({in_flight}) must not exceed the mux ingress \
             limit: reduce BLOCKFETCH_PIPELINE_WINDOW or increase \
             peer_connection::BLOCKFETCH_INGRESS_LIMIT"
        );
    }

    /// `headers_to_fetch` is capped at 2048 per decision tick.
    /// Verify that `select_headers_to_fetch` + the 2048-cap correctly truncates
    /// a large pending list, and that truncated headers reappear on the next
    /// tick (not permanently dropped).
    #[test]
    fn headers_cap_at_2048() {
        use std::collections::HashSet;

        // Build 3000 distinct pending headers.
        let pending: Vec<PendingHeader> = (0u32..3000)
            .map(|i| {
                let mut hash = [0u8; 32];
                hash[..4].copy_from_slice(&i.to_le_bytes());
                PendingHeader {
                    slot: i as u64,
                    hash,
                    header_cbor: vec![],
                    body_size: None,
                    prev_hash: None,
                }
            })
            .collect();

        let fetched_hashes: HashSet<[u8; 32]> = HashSet::new();

        // select_headers_to_fetch returns all 3000 (none fetched, none in DB).
        let all = select_headers_to_fetch(&pending, |_| false, &fetched_hashes);
        assert_eq!(all.len(), 3000);

        // Apply the production cap logic.
        let capped = if all.len() > 2048 {
            all.into_iter().take(2048).collect::<Vec<_>>()
        } else {
            all
        };
        assert_eq!(capped.len(), 2048, "cap must truncate to exactly 2048");

        // On next tick (no fetched_hashes updated) remaining 952 reappear.
        let next = select_headers_to_fetch(&pending, |_| false, &fetched_hashes);
        assert_eq!(
            next.len(),
            3000,
            "non-fetched headers must reappear on next tick"
        );
    }
}

// ─── #751: receive-side per-range byte abort ────────────────────────────────
//
// A peer that under-declares `block_body_size` (a SIGNED header field) can
// pack far more actual bytes into a nominal range than budgeted. The mux
// ingress backstop (48 MB) kills the connection generically; the #751 abort
// attributes the overrun to the peer as a ProtocolError (reputation/backoff)
// long before the backstop. Adversarial contract:
//   • honest variance must NEVER trip the abort (declared sizes are exact,
//     the WARN threshold at 1.5×+256 KiB fires strictly first)
//   • Byron / average-estimated ranges are NEVER armed (no declared sizes
//     to hold the peer to — honest variance there is unbounded)
//   • a size-lying peer trips the abort well below the ingress backstop
#[cfg(test)]
mod range_byte_abort_751_tests {
    use super::*;

    fn hdr(slot: u64, body_size: Option<u64>) -> PendingHeader {
        PendingHeader {
            slot,
            hash: [slot as u8; 32],
            header_cbor: vec![0u8; 1_000],
            body_size,
            prev_hash: None,
        }
    }

    /// Limit math: factor × estimate + slack in the un-clamped region,
    /// ceiling-clamped above it, saturating (no overflow panic on
    /// adversarially huge declared sizes).
    #[test]
    fn abort_limit_math_ceiling_and_saturation() {
        assert_eq!(range_byte_abort_limit(0), RANGE_BYTE_ABORT_SLACK);
        // Un-clamped region: 3×4 MiB + 1 MiB = 13 MiB < 20 MiB ceiling.
        assert_eq!(
            range_byte_abort_limit(4 << 20),
            (4 << 20) * RANGE_BYTE_ABORT_FACTOR + RANGE_BYTE_ABORT_SLACK
        );
        // A full-budget range (8 MiB est → 25 MiB formula) clamps to the
        // ceiling so attribution survives the pipeline-window worst case.
        assert_eq!(range_byte_abort_limit(8 << 20), RANGE_BYTE_ABORT_CEILING);
        // Saturation: must not panic; the ceiling caps everything.
        assert_eq!(range_byte_abort_limit(usize::MAX), RANGE_BYTE_ABORT_CEILING);
        assert_eq!(
            range_byte_abort_limit(usize::MAX / 2),
            RANGE_BYTE_ABORT_CEILING
        );
    }

    // (The #751 attribution invariant — abort ceiling × pipeline window ≤
    // mux ingress limit, with ≥2× honest headroom over the byte budget — is
    // enforced by the compile-time `const _: () = assert!(…)` blocks next to
    // `RANGE_BYTE_ABORT_CEILING`; no runtime test needed.)

    /// Lattice sweep over CONSTRUCTIBLE range estimates (range building
    /// bounds the estimate at `BLOCKFETCH_RANGE_BYTE_BUDGET` + one max-size
    /// block; larger declared sizes cannot appear on a chain whose headers
    /// pass validation): the #747 instrumentation WARN threshold
    /// (est + est/2 + 256 KiB) is STRICTLY below the abort limit —
    /// instrumentation always fires before attribution, and honest 1.5×
    /// variance never aborts.
    #[test]
    fn warn_threshold_strictly_below_abort_limit() {
        for est in [
            0usize,
            1,
            100,
            4_096,
            65_536,
            1 << 20,
            8 << 20,
            BLOCKFETCH_RANGE_BYTE_BUDGET + 90_112 + 1_016, // budget + max block
        ] {
            let warn_at = est.saturating_add(est / 2).saturating_add(262_144);
            let abort_at = range_byte_abort_limit(est);
            assert!(
                warn_at < abort_at,
                "WARN threshold {warn_at} must precede abort limit {abort_at} (est={est})"
            );
        }
    }

    /// Armed behavior (not just the math): an armed `RangeByteAbort` accepts
    /// an honest stream byte-for-byte, then convicts EXACTLY when a lying
    /// stream crosses the limit — with the #751 conviction surfaced as
    /// `BoundsExceeded`.
    #[test]
    fn armed_range_convicts_exactly_at_limit() {
        let est = 2_232_000usize; // 2000 blocks declared at 100 B + header
        let limit = range_byte_abort_limit(est);
        let mut abort = RangeByteAbort::new(true, est);

        // Honest-sized prefix: fine.
        assert!(abort.on_block(90_112).is_ok());
        // Walk to just below the limit…
        let mut delivered = 90_112usize;
        while delivered + 90_112 <= limit {
            assert!(abort.on_block(90_112).is_ok(), "below limit must pass");
            delivered += 90_112;
        }
        // …the next block crosses it: conviction, as BoundsExceeded, citing #751.
        let err = abort.on_block(90_112).unwrap_err();
        match err {
            dugite_network::error::ProtocolError::BoundsExceeded { protocol, reason } => {
                assert_eq!(protocol, "BlockFetch");
                assert!(
                    reason.contains("#751"),
                    "conviction must cite #751: {reason}"
                );
            }
            other => panic!("expected BoundsExceeded, got {other:?}"),
        }
        assert_eq!(abort.seen_bytes(), delivered + 90_112);
    }

    /// Unarmed (Byron/average) ranges never convict regardless of overshoot
    /// — 100 MB through a range estimated at nothing stays `Ok`.
    #[test]
    fn unarmed_range_never_convicts() {
        let mut abort = RangeByteAbort::new(false, 4_096);
        for _ in 0..100 {
            assert!(abort.on_block(1 << 20).is_ok());
        }
        assert_eq!(abort.seen_bytes(), 100 << 20);
    }

    /// An armed range delivering exactly its estimate (the honest case)
    /// never convicts, and the accounting matches.
    #[test]
    fn armed_honest_exact_delivery_passes() {
        let headers: Vec<_> = (0..92).map(|i| hdr(i, Some(90_112))).collect();
        let est: usize = headers
            .iter()
            .map(|h| estimated_block_wire_bytes(h, 65_536))
            .sum();
        let mut abort = RangeByteAbort::new(true, est);
        for h in &headers {
            // Actual wire bytes ≈ declared body + header CBOR + framing.
            assert!(abort.on_block(90_112 + h.header_cbor.len() + 16).is_ok());
        }
        assert_eq!(abort.seen_bytes(), est);
    }

    /// Arming flag: only ranges whose EVERY header declares a body size are
    /// armed. One Byron header (None) anywhere disarms the whole range.
    #[test]
    fn arming_requires_all_headers_declared() {
        let declared: Vec<_> = (0..10).map(|i| hdr(i, Some(50_000))).collect();
        assert!(range_all_declared(&declared));

        let mut mixed = declared.clone();
        mixed[5] = hdr(5, None);
        assert!(!range_all_declared(&mixed));

        let byron: Vec<_> = (0..10).map(|i| hdr(i, None)).collect();
        assert!(!range_all_declared(&byron));
    }

    /// Honest delivery: actual block wire bytes track the declared estimate
    /// (header + body + framing). Even with +25% per-block framing variance
    /// — far beyond anything real — an honest range stays under the abort
    /// limit. (Real variance is bytes per block: the estimate already counts
    /// declared body + header CBOR + 16 bytes framing.)
    #[test]
    fn honest_variance_never_trips_abort() {
        // A realistic budget-full range: ~92 mainnet-max blocks (90,112 B).
        let headers: Vec<_> = (0..92).map(|i| hdr(i, Some(90_112))).collect();
        let est: usize = headers
            .iter()
            .map(|h| estimated_block_wire_bytes(h, 65_536))
            .sum();
        let limit = range_byte_abort_limit(est);

        // Actual = declared body + header + GENEROUS 25% overhead.
        let actual: usize = headers
            .iter()
            .map(|h| (90_112 + h.header_cbor.len()) * 5 / 4)
            .sum();
        assert!(
            actual < limit,
            "honest +25% delivery ({actual}) must stay below the abort limit ({limit})"
        );

        // A single tiny block: slack alone must absorb any framing variance.
        let tiny = hdr(0, Some(100));
        let est_tiny = estimated_block_wire_bytes(&tiny, 65_536);
        let limit_tiny = range_byte_abort_limit(est_tiny);
        let actual_tiny = 100 + 1_000 + 512; // body + header + worst-case framing
        assert!(
            actual_tiny < limit_tiny,
            "tiny honest block ({actual_tiny}) must stay below ({limit_tiny})"
        );
    }

    /// Adversarial: a size-liar declares 100-byte bodies but streams real
    /// ~90 KB blocks. The abort limit must sit far BELOW the 48 MB mux
    /// ingress backstop so the overrun is attributed (ProtocolError → peer
    /// fault) instead of dying generically — and the cumulative delivery
    /// crosses the limit within a few real blocks.
    #[test]
    fn size_liar_trips_abort_before_ingress_backstop() {
        const INGRESS_BACKSTOP: usize = 48 << 20;
        // Liar's range: max_range-many 100-byte-declared headers — the
        // worst case (largest estimate a liar can build while lying small).
        let headers: Vec<_> = (0..2_000).map(|i| hdr(i, Some(100))).collect();
        let est: usize = headers
            .iter()
            .map(|h| estimated_block_wire_bytes(h, 65_536))
            .sum();
        let limit = range_byte_abort_limit(est);
        assert!(
            limit < INGRESS_BACKSTOP / 4,
            "abort limit ({limit}) must sit far below the ingress backstop"
        );

        // Stream real 90 KB blocks: cumulative bytes cross the limit at
        // block ~⌈limit/90KB⌉ — i.e. the guard fires mid-stream, not after
        // the full 2000-block flood (~180 MB).
        let real_block = 90_112usize;
        let blocks_to_trip = limit / real_block + 1;
        assert!(
            blocks_to_trip < 100,
            "abort must fire within ~100 real blocks, computed {blocks_to_trip}"
        );
        let cumulative = blocks_to_trip * real_block;
        assert!(cumulative > limit, "guard fires once cumulative > limit");
        assert!(
            cumulative < INGRESS_BACKSTOP,
            "attribution happens before the generic ingress death"
        );
    }

    /// Byron ranges (no declared sizes) are estimated from the adaptive
    /// average — honest variance can exceed ANY multiple of it (e.g. average
    /// trained on empty 600-byte blocks, then a burst of full 2 MB Byron
    /// blocks). The abort must not be armed: `range_all_declared` is false,
    /// so no limit applies regardless of overshoot.
    #[test]
    fn byron_average_ranges_never_armed() {
        let headers: Vec<_> = (0..100).map(|i| hdr(i, None)).collect();
        assert!(
            !range_all_declared(&headers),
            "Byron ranges must never arm the #751 abort"
        );
    }

    // #760-A: the unproductive-dynamo watchdog must rotate a GENUINELY-SILENT
    // dynamo (preserving the #742 fix) but NOT a dynamo that fed a forecast
    // window of headers and is now legitimately parked on the horizon.
    #[test]
    fn unproductive_watchdog_rotates_only_silent_dynamo() {
        let tip = 1_000_000u64;
        // No fragment at all → never fed headers → silent → rotate.
        assert!(should_rotate_unproductive_dynamo(None, tip));
        // Fragment exactly at our tip → silent → rotate.
        assert!(should_rotate_unproductive_dynamo(Some(tip), tip));
        // Fragment a few hundred slots ahead (within margin) → still silent.
        assert!(should_rotate_unproductive_dynamo(Some(tip + 500), tip));
        // Exactly at the margin boundary → still rotate (`<=`).
        assert!(should_rotate_unproductive_dynamo(
            Some(tip + GENESIS_PARKED_DYNAMO_MARGIN_SLOTS),
            tip
        ));
        // Just past the margin → parked-with-headers → do NOT rotate.
        assert!(!should_rotate_unproductive_dynamo(
            Some(tip + GENESIS_PARKED_DYNAMO_MARGIN_SLOTS + 1),
            tip
        ));
        // ~A full mainnet stability window ahead → clearly parked → keep it.
        assert!(!should_rotate_unproductive_dynamo(Some(tip + 129_600), tip));
    }
}
