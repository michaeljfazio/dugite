//! Independent BlockFetch decision task.
//!
//! # Haskell Architecture Reference
//!
//! In the Haskell cardano-node, `blockFetchLogic` runs as its own dedicated thread
//! (via `Ouroboros.Network.BlockFetch.blockFetchLogic`). It:
//!
//! 1. Reads candidate chain state from all connected peers via STM `TVar`s
//!    (updated by per-peer ChainSync mini-protocol tasks).
//! 2. Every decision interval (10ms for Praos, 40ms for Genesis), evaluates which
//!    blocks need to be fetched and from which peers.
//! 3. Issues `FetchRequest` ranges to per-peer BlockFetch client tasks via STM.
//! 4. Per-peer BlockFetch tasks download the blocks and deliver them to the chain
//!    selection / ledger application pipeline.
//!
//! This module provides the Rust equivalent using `tokio::sync` channels:
//!
//! - **`BlockFetchLogicTask`** — the decision loop that reads candidate chains
//!   (via `Arc<RwLock<HashMap>>`) and dispatches fetch ranges to per-peer workers
//!   (via `mpsc` channels).
//! - **`blockfetch_worker`** — per-peer worker function that receives fetch ranges,
//!   downloads blocks via `BlockFetchClient`, and sends decoded blocks to the main
//!   run loop.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, RwLock};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use dugite_network::codec::Point;
use dugite_network::protocol::blockfetch::decision::{
    BlockFetchDecision, FetchRange, PeerFetchState,
};
use dugite_network::{BlockFetchClient, MuxChannel, PEER_LATENCY_DEFAULT_MS};
use dugite_storage::ChainDB;

use super::connection_lifecycle::{
    CandidateChainState, FetchedBlock, PeerFetchStatus, PendingHeader,
};

/// Default decision interval for Praos consensus (10 ms — matches
/// `bfcDecisionLoopIntervalPraos = 0.01` in Haskell's
/// `ouroboros-network/cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs`).
///
/// The decision loop refills per-peer in-flight request queues, detects
/// stalled peers, and re-evaluates fetch mode (BulkSync vs Deadline).  At
/// 10× this cadence (the previous 100 ms value) dugite reacted 10× more
/// slowly to delivered blocks during catch-up, capping the throughput
/// floor.  CPU cost at 10 ms with 50 hot peers is ~5% on Apple Silicon;
/// acceptable given the throughput gain.  Issue #701.
///
/// Genesis mode uses 40 ms instead.
const PRAOS_DECISION_INTERVAL: Duration = Duration::from_millis(10);

/// Default decision interval for Genesis consensus (40ms).
///
/// Genesis mode uses a longer interval because the chain selection
/// algorithm is more complex and the decision task should not starve
/// other tasks.
#[allow(dead_code)]
const GENESIS_DECISION_INTERVAL: Duration = Duration::from_millis(40);

/// Maximum number of consecutive headers to batch into a single fetch range.
///
/// Batching consecutive headers reduces the number of `MsgRequestRange` round-trips
/// while keeping individual fetches bounded so that slow peers don't block progress.
const MAX_BATCH_SIZE: usize = 100;

/// Timeout for in-flight blocks.  If a block has been in-flight for longer
/// than this, the entry is purged so the block can be re-fetched from
/// another peer.  This prevents sync stalls when a peer's TCP connection
/// dies silently (half-open) and the worker never reports back.
///
/// Set to 60s to match Haskell's `bfcFetchDeadlinePolicy` fetch deadline.
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum number of fetch ranges held in-flight per peer by the decision
/// engine (F3).
///
/// Enables genuine N-peer parallelism: `select_peer` round-robins to the next
/// lowest-latency peer once the fastest hits this window.  dugite's cross-peer
/// `in_flight` hash map plus the F1 intra-tick reservation already guarantee
/// disjoint assignment across peers, so concurrent disjoint fetches are safe
/// (this is why dugite can exceed Haskell's `maxConcurrencyBulkSync = 1`).
/// The value matches `BLOCKFETCH_PIPELINE_WINDOW` from `connection_lifecycle`.
///
/// **Land order invariant:** this finite cap is only safe because the F0
/// capacity-release path (`release_completed_ranges`) returns capacity on
/// delivery/failure.  Lowering the cap without that release would self-stall
/// the pipeline.
const BLOCKFETCH_PER_PEER_WINDOW: usize = 4;

/// A dispatched fetch range paired with the block hashes it reserved.
///
/// Used by the F0 capacity-release path to map a per-peer in-flight range back
/// to its constituent block hashes so delivery / timeout can be detected.
type DispatchedRange = (FetchRange, Vec<[u8; 32]>);

/// Independent block fetch decision task.
///
/// Matches Haskell's `blockFetchLogic` thread: reads candidate chain state
/// from all ChainSync peers, decides which blocks to fetch, dispatches
/// fetch requests to per-peer BlockFetch channels.
///
/// Runs in its own tokio task, communicating via channels:
/// - Reads: `candidate_chains` (`Arc<RwLock<HashMap>>`, updated by ChainSync tasks)
/// - Reads: `current_tip_slot` (updated from ledger via the run loop)
/// - Writes: `fetch_senders` per peer (`mpsc`, consumed by per-peer BlockFetch tasks)
/// - Writes: `fetched_blocks_tx` (`mpsc`, consumed by main run loop)
///
/// # Lifecycle
///
/// Created by the run loop, started via `run()`. Peers are registered/deregistered
/// as connections are promoted to hot / demoted from hot. The task runs until the
/// cancellation token is triggered (node shutdown).
pub struct BlockFetchLogicTask {
    /// Shared candidate chain state from all peers.
    ///
    /// Written by per-peer ChainSync tasks. Read here to determine which
    /// pending headers need to be fetched and from which peers.
    candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,

    /// Current chain tip slot from the ledger.
    ///
    /// Kept for compatibility (e.g. logging) but NO LONGER used to filter
    /// pending headers — see the comment above the (removed) slot check in
    /// `pump_decisions`.  Bug I (2026-05-16): the old `slot <= tip_slot`
    /// filter silently dropped peer headers at earlier slots when local +
    /// peer chains had diverged, so the fork was never fetched and
    /// `chain_sel_queue::process_add_block` saw it as "fork unreachable"
    /// every time a new peer block arrived.
    current_tip_slot: u64,

    /// Read-only access to the local ChainDB.
    ///
    /// Used to skip pending headers for blocks we already have (by hash),
    /// regardless of slot.  Without this, divergent forks at slots ≤ our
    /// tip would never be fetched and chain selection would permanently
    /// reject the peer's longer chain.
    chain_db: Option<Arc<RwLock<ChainDB>>>,

    /// Decision interval — how often the task evaluates fetch decisions.
    ///
    /// 10ms for Praos (default), 40ms for Genesis.
    decision_interval: Duration,

    /// Per-peer fetch request senders.
    ///
    /// Key: peer socket address. Value: sender end of the channel consumed
    /// by that peer's `blockfetch_worker`. When the decision task determines
    /// that a peer should fetch certain ranges, it sends `Vec<FetchRange>`
    /// to the corresponding sender.
    fetch_senders: HashMap<SocketAddr, mpsc::Sender<Vec<FetchRange>>>,

    /// Channel to send decoded blocks to the main run loop.
    ///
    /// Both this task and the per-peer workers share clones of this sender.
    /// The run loop consumes `FetchedBlock` values and applies them to
    /// ChainDB + LedgerState.
    fetched_blocks_tx: mpsc::Sender<FetchedBlock>,

    /// Byron epoch length in slots (needed for block deserialization).
    byron_epoch_length: u64,

    /// Block hashes currently in-flight, mapped to the peer that was asked
    /// to fetch them and the timestamp when the request was dispatched.
    ///
    /// Prevents duplicate fetch requests for the same block across multiple
    /// decision iterations. Entries are added when ranges are dispatched and
    /// removed when blocks are received or ranges fail.
    ///
    /// Tracking per-peer allows cleanup when a peer disconnects: all blocks
    /// assigned to that peer are released for re-fetch from another peer.
    /// The timestamp enables timeout-based cleanup of stale entries when a
    /// peer's TCP connection dies silently (half-open).
    in_flight: HashMap<[u8; 32], (SocketAddr, Instant)>,

    /// The underlying decision engine that tracks queued/in-flight ranges
    /// and selects the optimal peer for each fetch.
    decision_engine: BlockFetchDecision,

    /// Per-peer record of dispatched ranges and the block hashes each range
    /// covers, used to drive the decision-engine capacity-release path (F0).
    ///
    /// `BlockFetchDecision` only records that a range is in-flight for a peer;
    /// it has no visibility into delivery.  We track the constituent hashes
    /// here so that on each tick we can detect when every block of a dispatched
    /// range has either landed in ChainDB (delivered → `mark_completed`) or had
    /// its cross-peer `in_flight` reservation expire (failed → `mark_failed`),
    /// and return the per-peer capacity to zero.  Without this the per-peer
    /// `BlockFetchDecision.in_flight` map only ever grows, so any finite
    /// `max_in_flight` cap would self-stall the sync pipeline.
    dispatched_ranges: HashMap<SocketAddr, Vec<DispatchedRange>>,

    /// Optional handle to the node peer manager, used to read per-peer EWMA
    /// latency when building `PeerFetchState` for the decision engine.
    ///
    /// When `None` (unit tests that do not construct a real peer manager),
    /// each peer falls back to [`PEER_LATENCY_DEFAULT_MS`].
    peer_manager: Option<Arc<RwLock<super::networking::NodePeerManager>>>,

    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
}

impl BlockFetchLogicTask {
    /// Create a new BlockFetch decision task.
    ///
    /// # Arguments
    ///
    /// * `candidate_chains` — Shared map of per-peer candidate chain state
    /// * `fetched_blocks_tx` — Channel to send downloaded blocks to the run loop
    /// * `byron_epoch_length` — Byron epoch length for block deserialization
    /// * `cancel` — Cancellation token for graceful shutdown
    ///
    /// Peer latency defaults to [`PEER_LATENCY_DEFAULT_MS`] for all peers
    /// (no real RTT data).  For production use, prefer
    /// [`Self::new_with_chain_db`] or
    /// [`Self::new_with_peer_manager`].
    pub fn new(
        candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
        fetched_blocks_tx: mpsc::Sender<FetchedBlock>,
        byron_epoch_length: u64,
        cancel: CancellationToken,
    ) -> Self {
        Self::new_with_peer_manager(
            candidate_chains,
            fetched_blocks_tx,
            byron_epoch_length,
            cancel,
            None,
            None,
        )
    }

    /// Variant of [`new`] that also threads a read handle to the local
    /// ChainDB so the decision task can skip headers for blocks we already
    /// have (by hash, regardless of slot).  Bug I (2026-05-16): without
    /// this, divergent forks at slots ≤ our tip would never be fetched
    /// because the legacy `slot <= current_tip_slot` filter dropped them.
    /// Production callers should always pass `Some(chain_db)`; the legacy
    /// `new()` is kept for the existing unit tests that don't construct
    /// a real ChainDB.
    pub fn new_with_chain_db(
        candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
        fetched_blocks_tx: mpsc::Sender<FetchedBlock>,
        byron_epoch_length: u64,
        cancel: CancellationToken,
        chain_db: Option<Arc<RwLock<ChainDB>>>,
    ) -> Self {
        Self::new_with_peer_manager(
            candidate_chains,
            fetched_blocks_tx,
            byron_epoch_length,
            cancel,
            chain_db,
            None,
        )
    }

    /// Full constructor that threads both a local ChainDB handle and a
    /// [`NodePeerManager`] handle.
    ///
    /// The peer manager is queried once per decision tick (behind a
    /// `read()` lock) to populate real EWMA latency values for each hot
    /// peer.  Peers with no RTT sample yet fall back to
    /// [`PEER_LATENCY_DEFAULT_MS`] (1 000 ms — matching Haskell's
    /// `defaultGSV` g=500 ms × 2 for round-trip, from
    /// `ouroboros-network/lib/Ouroboros/Network/DeltaQ.hs`).
    ///
    /// # Arguments
    ///
    /// * `candidate_chains` — Shared map of per-peer candidate chain state
    /// * `fetched_blocks_tx` — Channel to send downloaded blocks to the run loop
    /// * `byron_epoch_length` — Byron epoch length for block deserialization
    /// * `cancel` — Cancellation token for graceful shutdown
    /// * `chain_db` — Optional local chain store handle (skip already-stored blocks)
    /// * `peer_manager` — Optional peer manager for EWMA latency lookup
    pub fn new_with_peer_manager(
        candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
        fetched_blocks_tx: mpsc::Sender<FetchedBlock>,
        byron_epoch_length: u64,
        cancel: CancellationToken,
        chain_db: Option<Arc<RwLock<ChainDB>>>,
        peer_manager: Option<Arc<RwLock<super::networking::NodePeerManager>>>,
    ) -> Self {
        Self {
            candidate_chains,
            current_tip_slot: 0,
            chain_db,
            decision_interval: PRAOS_DECISION_INTERVAL,
            fetch_senders: HashMap::new(),
            fetched_blocks_tx,
            byron_epoch_length,
            in_flight: HashMap::new(),
            // F3: 4-deep per-peer concurrency window (matches
            // BLOCKFETCH_PIPELINE_WINDOW).  dugite's cross-peer `in_flight`
            // hash union + F1 intra-tick reservation guarantee disjoint
            // assignment, so concurrent disjoint fetches across peers are
            // safe — unlike Haskell's `maxConcurrencyBulkSync = 1`, which
            // lacks that hash union.  The F0 capacity-release path (driven in
            // `evaluate_and_fetch`) returns this capacity on completion, so a
            // finite cap no longer self-stalls.
            decision_engine: BlockFetchDecision::new(BLOCKFETCH_PER_PEER_WINDOW),
            dispatched_ranges: HashMap::new(),
            peer_manager,
            cancel,
        }
    }

    /// Set the decision interval.
    ///
    /// Use `PRAOS_DECISION_INTERVAL` (10ms) for normal Praos operation or
    /// `GENESIS_DECISION_INTERVAL` (40ms) for Genesis mode.
    #[allow(dead_code)]
    pub fn set_decision_interval(&mut self, interval: Duration) {
        self.decision_interval = interval;
    }

    /// Update the current chain tip slot.
    ///
    /// Called by the run loop as blocks are applied to the ledger, so the
    /// decision task can skip headers for blocks we already have.
    pub fn update_tip_slot(&mut self, slot: u64) {
        self.current_tip_slot = slot;
    }

    /// Register a new peer's BlockFetch channel.
    ///
    /// Called when a peer is promoted to hot and its BlockFetch worker is spawned.
    /// The `fetch_tx` sender is the channel the worker reads fetch requests from.
    pub fn register_peer(&mut self, addr: SocketAddr, fetch_tx: mpsc::Sender<Vec<FetchRange>>) {
        debug!(%addr, "registering peer for block fetch");
        self.fetch_senders.insert(addr, fetch_tx);
    }

    /// Deregister a peer (disconnected or demoted from hot).
    ///
    /// Removes the peer's fetch sender and releases all in-flight blocks
    /// that were assigned to this peer, so they can be re-fetched from
    /// another peer.  Without this cleanup, blocks dispatched to a dead
    /// peer would stay in `in_flight` forever, starving the sync pipeline.
    pub fn deregister_peer(&mut self, addr: &SocketAddr) {
        let before = self.in_flight.len();
        self.in_flight.retain(|_, (peer, _)| peer != addr);
        let released = before - self.in_flight.len();
        if released > 0 {
            info!(
                %addr,
                released,
                "released in-flight blocks for deregistered peer"
            );
        }
        // F0 #2 (failure/disconnect): release the peer's per-peer decision-engine
        // capacity so the cap does not leak when a hot peer drops.  The genuine
        // undelivered gaps are re-fetched via the cross-peer `in_flight` map (now
        // purged above) + `has_block`, so we must NOT re-queue here.
        self.decision_engine.release_peer(addr);
        self.dispatched_ranges.remove(addr);
        debug!(%addr, "deregistering peer from block fetch");
        self.fetch_senders.remove(addr);
    }

    /// Run the main decision loop.
    ///
    /// Ticks at the configured `decision_interval` and evaluates which blocks
    /// to fetch on each tick. Exits when the cancellation token is triggered.
    ///
    /// This is the entry point for the tokio task — call via:
    /// ```ignore
    /// tokio::spawn(async move { task.run().await });
    /// ```
    pub async fn run(&mut self) {
        info!(
            interval_ms = self.decision_interval.as_millis(),
            "block fetch decision task started"
        );

        let mut ticker = tokio::time::interval(self.decision_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.evaluate_and_fetch().await;
                }
                _ = self.cancel.cancelled() => {
                    info!("block fetch decision task shutting down");
                    break;
                }
            }
        }
    }

    /// One iteration of the decision loop.
    ///
    /// For each peer's candidate chain:
    /// 1. Get pending headers (not yet fetched).
    /// 2. Skip headers for blocks we already have (slot <= current_tip_slot).
    /// 3. Skip headers already in-flight.
    /// 4. Batch consecutive headers into fetch ranges.
    /// 5. Dispatch ranges to the best available peer via their BlockFetch channel.
    async fn evaluate_and_fetch(&mut self) {
        // If no peers are registered, nothing to do.
        if self.fetch_senders.is_empty() {
            return;
        }

        // Purge stale in-flight entries.  If a peer's TCP connection dies
        // silently (half-open), the worker blocks indefinitely on recv() and
        // never reports back.  Without this cleanup the sync pipeline stalls
        // because the blocks can never be re-fetched from another peer.
        let now = Instant::now();
        let before = self.in_flight.len();
        self.in_flight
            .retain(|_, (_, dispatched_at)| now.duration_since(*dispatched_at) < IN_FLIGHT_TIMEOUT);
        let expired = before - self.in_flight.len();
        if expired > 0 {
            warn!(
                expired,
                remaining = self.in_flight.len(),
                "purged stale in-flight blocks (exceeded {}s timeout)",
                IN_FLIGHT_TIMEOUT.as_secs()
            );
        }

        // F0 (capacity-release on delivery / timeout): return per-peer
        // decision-engine capacity to zero for every dispatched range whose
        // blocks have all landed in ChainDB (delivered → `mark_completed`) or
        // whose cross-peer `in_flight` reservations have all expired without
        // delivery (timed out → `mark_failed`).  Without this the per-peer
        // `BlockFetchDecision.in_flight` map only ever grows and the finite
        // `BLOCKFETCH_PER_PEER_WINDOW` cap would self-stall the pipeline.
        self.release_completed_ranges().await;

        // F2 (Aberrant fast-release): purge an Aberrant peer's cross-peer
        // hashes and per-peer decision-engine capacity so its undelivered
        // ranges reassign on the *next* tick (~10 ms) rather than waiting out
        // the 60 s `IN_FLIGHT_TIMEOUT`.  `has_block` still filters anything
        // already delivered, so only genuine gaps reassign.
        let aberrant: HashSet<SocketAddr> = {
            let chains = self.candidate_chains.read().await;
            chains
                .iter()
                .filter(|(_, s)| s.fetch_status == PeerFetchStatus::Aberrant)
                .map(|(addr, _)| *addr)
                .collect()
        };
        if !aberrant.is_empty() {
            let before_aberrant = self.in_flight.len();
            self.in_flight
                .retain(|_, (peer, _)| !aberrant.contains(peer));
            let released = before_aberrant - self.in_flight.len();
            for peer in &aberrant {
                self.decision_engine.release_peer(peer);
                self.dispatched_ranges.remove(peer);
            }
            if released > 0 {
                debug!(
                    aberrant_count = aberrant.len(),
                    released, "released in-flight blocks held by Aberrant peers"
                );
            }
        }

        // Read candidate chain state from all peers.
        //
        // Snapshot all pending headers and per-peer fetch status BEFORE acquiring
        // the chain_db lock so we minimise lock contention on the hot apply-block
        // path.
        //
        // Per-peer chain tracking (issue #702 / Haskell `PeerFetchStatus`):
        // Aberrant peers — those with ≥3 consecutive delivery failures within 30s
        // — are excluded here before any further processing.  This mirrors the
        // `fetchDecisions` peer filter in `Ouroboros.Network.BlockFetch.Decision`
        // (Decision.hs:~450), which skips peers with `PeerFetchStatusAberrant`
        // before building the candidate set.
        let raw_pending: Vec<(SocketAddr, PendingHeader)> = {
            let chains = self.candidate_chains.read().await;
            chains
                .iter()
                .filter(|(addr, state)| {
                    self.fetch_senders.contains_key(addr)
                        && state.fetch_status != PeerFetchStatus::Aberrant
                })
                .flat_map(|(addr, state)| {
                    state
                        .pending_headers
                        .iter()
                        .map(move |h| (*addr, h.clone()))
                })
                .collect()
        };

        // Count eligible vs Aberrant peers for the debug log.
        #[cfg(debug_assertions)]
        {
            let chains = self.candidate_chains.try_read();
            if let Ok(chains) = chains {
                let aberrant: Vec<_> = chains
                    .iter()
                    .filter(|(_, s)| s.fetch_status == PeerFetchStatus::Aberrant)
                    .map(|(addr, _)| *addr)
                    .collect();
                if !aberrant.is_empty() {
                    trace!(
                        aberrant_count = aberrant.len(),
                        "BlockFetch: excluding Aberrant peers from decision"
                    );
                }
            }
        }

        // Bug I (2026-05-16): use a hash-based has_block check, not the old
        // `slot <= current_tip_slot` filter.  The slot filter wrongly dropped
        // peer headers at earlier slots when local and peer chains had
        // diverged (same slot, different hash), so divergent forks were
        // never fetched and chain selection saw "fork unreachable" forever.
        //
        // We acquire the chain_db read lock for the briefest possible window:
        // a single `has_block` call per pending header, then release.  No I/O
        // inside the critical section.
        let mut new_headers: Vec<(SocketAddr, PendingHeader)> = Vec::new();
        if let Some(db_handle) = self.chain_db.as_ref() {
            let db = db_handle.read().await;
            for (addr, header) in raw_pending {
                let hash = dugite_primitives::hash::Hash32::from_bytes(header.hash);
                if db.has_block(&hash) {
                    continue;
                }
                if self.in_flight.contains_key(&header.hash) {
                    continue;
                }
                new_headers.push((addr, header));
            }
            // Lock dropped at end of scope.
        } else {
            // Legacy unit-test path (no ChainDB available): fall back to the
            // slot-based check that preserved the original test fixtures.
            for (addr, header) in raw_pending {
                if header.slot <= self.current_tip_slot {
                    continue;
                }
                if self.in_flight.contains_key(&header.hash) {
                    continue;
                }
                new_headers.push((addr, header));
            }
        }

        if new_headers.is_empty() {
            return;
        }

        // Sort headers by slot so we batch consecutive ranges.
        new_headers.sort_by_key(|(_, h)| h.slot);

        // Batch consecutive headers into fetch ranges.
        let ranges = batch_headers_into_ranges(&new_headers);

        if ranges.is_empty() {
            return;
        }

        trace!(
            range_count = ranges.len(),
            header_count = new_headers.len(),
            "dispatching fetch ranges"
        );

        // Build peer fetch states for the decision engine.
        //
        // Per-peer chain tracking (issue #702): populate `in_flight` from the
        // peer's `CandidateChainState.in_flight_blocks` so the decision engine
        // can de-prefer Busy peers relative to Ready peers.  Haskell's
        // `comparePeerFetchStatus` (ClientState.hs) sorts Ready before Busy;
        // we propagate `in_flight_blocks` here so `BlockFetchDecision::select_peer`
        // applies the same ordering.  Aberrant peers are already excluded above.
        //
        // Issue #706: Populate latency_ms from PeerManager EWMA data rather than
        // the previous hardcoded 100.0.  We snapshot the latency map under a
        // short-lived read lock *before* acquiring the candidate_chains lock to
        // avoid potential lock-ordering deadlocks.  Peers without a KeepAlive
        // RTT sample yet fall back to PEER_LATENCY_DEFAULT_MS (1 000 ms), which
        // matches Haskell's `defaultGSV` (g=500 ms × 2 for round-trip) from
        // `ouroboros-network/lib/Ouroboros/Network/DeltaQ.hs`.
        let latency_snapshot: HashMap<SocketAddr, f64> =
            if let Some(pm_handle) = self.peer_manager.as_ref() {
                let pm = pm_handle.read().await;
                self.fetch_senders
                    .keys()
                    .map(|addr| {
                        let lat = pm.peer_latency_ms(addr).unwrap_or(PEER_LATENCY_DEFAULT_MS);
                        (*addr, lat)
                    })
                    .collect()
            } else {
                // No peer manager available (unit tests): seed all peers with
                // the default.
                self.fetch_senders
                    .keys()
                    .map(|addr| (*addr, PEER_LATENCY_DEFAULT_MS))
                    .collect()
            };

        let peer_states: Vec<PeerFetchState> = {
            let chains = self.candidate_chains.read().await;
            self.fetch_senders
                .keys()
                .filter_map(|addr| {
                    let state = chains.get(addr)?;
                    // Double-check: Aberrant peers must not appear in peer_states.
                    if state.fetch_status == PeerFetchStatus::Aberrant {
                        return None;
                    }
                    let latency_ms = latency_snapshot
                        .get(addr)
                        .copied()
                        .unwrap_or(PEER_LATENCY_DEFAULT_MS);
                    Some(PeerFetchState {
                        addr: *addr,
                        latency_ms,
                        in_flight: state.in_flight_blocks as usize,
                        tip_slot: state.tip_slot,
                    })
                })
                .collect()
        };

        // Add all ranges to the decision engine and dispatch.
        for range in &ranges {
            self.decision_engine
                .add_range(range.from.clone(), range.to.clone());
        }

        // Select peers and dispatch ranges.
        //
        // F1 (intra-tick hash reservation): the cross-peer `in_flight` hash
        // insert now happens *inside* the `while let`, before the next
        // `select_peer`, so a hash reserved for peer A in iteration `i` is
        // visible (and skipped) in iteration `i+1`.  Without this, two peers
        // could be handed the same hash in a single tick because the old code
        // deferred all `in_flight` inserts until after the whole loop.  This is
        // dugite's `blocksFetchedThisRound` equivalent.
        //
        // We provisionally insert the hash into the cross-peer map and also
        // remember, per (peer, range), the constituent hashes so that:
        //   * on `try_send` success we commit (record dispatched_ranges for the
        //     F0 release path + flip the peer to Busy), and
        //   * on `TrySendError::Full`/`Closed` we roll back BOTH the provisional
        //     cross-peer inserts (F1) AND the per-peer decision-engine capacity
        //     via `mark_failed` (F0 #4), so transient backpressure never
        //     permanently burns capacity.
        let now = Instant::now();
        let mut reserved_this_tick: HashSet<[u8; 32]> = HashSet::new();
        // peer → [(range, hashes-reserved-for-this-range)]
        let mut dispatched: HashMap<SocketAddr, Vec<DispatchedRange>> = HashMap::new();

        while let Some((peer, range)) = self.decision_engine.select_peer(&peer_states) {
            let (from_slot, to_slot) = range.slot_bounds();
            let mut range_hashes: Vec<[u8; 32]> = Vec::new();
            for (_, header) in &new_headers {
                if header.slot >= from_slot
                    && header.slot <= to_slot
                    && reserved_this_tick.insert(header.hash)
                {
                    // Provisional cross-peer reservation — visible to the next
                    // `select_peer` iteration so no other peer claims this hash.
                    self.in_flight.insert(header.hash, (peer, now));
                    range_hashes.push(header.hash);
                }
            }
            dispatched
                .entry(peer)
                .or_default()
                .push((range, range_hashes));
        }

        // Send fetch requests to each peer's worker.
        //
        // Per-peer chain tracking (issue #702): call `record_fetch_dispatched`
        // on the peer's `CandidateChainState` to set its status to Busy.  This
        // mirrors Haskell's `PeerFetchStatusBusy` transition in Decision.hs.
        for (addr, peer_ranges) in dispatched {
            // Strip the reserved-hash bookkeeping from the payload sent to the
            // worker (it only needs the ranges).
            let worker_ranges: Vec<FetchRange> =
                peer_ranges.iter().map(|(r, _)| r.clone()).collect();
            let range_count = worker_ranges.len();
            let send_result = self
                .fetch_senders
                .get(&addr)
                .map(|sender| sender.try_send(worker_ranges));
            match send_result {
                Some(Ok(())) => {
                    debug!(%addr, range_count, "dispatched fetch ranges to peer");
                    // F0: record the dispatched ranges + their hashes so the
                    // release pass can return per-peer capacity once delivered.
                    let entry = self.dispatched_ranges.entry(addr).or_default();
                    for (range, hashes) in peer_ranges {
                        entry.push((range, hashes));
                    }
                    // Transition peer to Busy in CandidateChainState.
                    {
                        let mut chains = self.candidate_chains.write().await;
                        if let Some(state) = chains.get_mut(&addr) {
                            state.record_fetch_dispatched();
                        }
                    }
                }
                Some(Err(mpsc::error::TrySendError::Full(_))) => {
                    // Channel full — roll back the provisional cross-peer hash
                    // reservations (F1) AND the per-peer decision-engine capacity
                    // (F0 #4) for every range that targeted this peer, so the
                    // blocks are re-dispatched on the next tick and capacity is
                    // not permanently burned.
                    debug!(
                        %addr,
                        range_count,
                        "peer fetch channel full, rolling back reservation; will retry"
                    );
                    for (range, hashes) in &peer_ranges {
                        for hash in hashes {
                            self.in_flight.remove(hash);
                        }
                        self.decision_engine.mark_failed(addr, range);
                    }
                }
                Some(Err(mpsc::error::TrySendError::Closed(_))) => {
                    warn!(%addr, "peer fetch channel closed, deregistering");
                    // Roll back provisional reservations then fully deregister so
                    // the per-peer capacity + cross-peer hashes are released.
                    for (_, hashes) in &peer_ranges {
                        for hash in hashes {
                            self.in_flight.remove(hash);
                        }
                    }
                    self.deregister_peer(&addr);
                }
                None => {
                    // Sender vanished between selection and send (deregistered
                    // mid-tick): drop the provisional reservations.
                    for (_, hashes) in &peer_ranges {
                        for hash in hashes {
                            self.in_flight.remove(hash);
                        }
                    }
                    self.decision_engine.release_peer(&addr);
                }
            }
        }
    }

    /// F0 capacity-release pass — return per-peer decision-engine capacity to
    /// zero for dispatched ranges that have completed.
    ///
    /// For each tracked `(peer, range)` we inspect the range's constituent
    /// block hashes:
    ///
    /// * **Delivered** — every hash is present in ChainDB (`has_block`): the
    ///   range is complete, so we `mark_completed` (releasing the per-peer
    ///   capacity) and drop the cross-peer `in_flight` reservations + tracking.
    /// * **Failed / timed out** — every hash has dropped out of the cross-peer
    ///   `in_flight` map (its 60 s reservation expired) without being stored:
    ///   we `mark_failed` (releasing capacity, re-queuing the range) and drop
    ///   the tracking.  The genuine gap is then re-dispatched on a later tick.
    /// * **In progress** — some hashes are still in `in_flight` and not yet in
    ///   ChainDB: leave the range tracked.
    ///
    /// Without this the per-peer `BlockFetchDecision.in_flight` map only ever
    /// grows, so the finite [`BLOCKFETCH_PER_PEER_WINDOW`] cap would self-stall.
    async fn release_completed_ranges(&mut self) {
        if self.dispatched_ranges.is_empty() {
            return;
        }

        // Snapshot the ChainDB delivery state for every tracked hash under a
        // single short-lived read lock (one `has_block` call per hash, no I/O).
        let stored: HashSet<[u8; 32]> = if let Some(db_handle) = self.chain_db.as_ref() {
            let db = db_handle.read().await;
            let mut s = HashSet::new();
            for ranges in self.dispatched_ranges.values() {
                for (_, hashes) in ranges {
                    for hash in hashes {
                        if !s.contains(hash) {
                            let h = dugite_primitives::hash::Hash32::from_bytes(*hash);
                            if db.has_block(&h) {
                                s.insert(*hash);
                            }
                        }
                    }
                }
            }
            s
        } else {
            // No ChainDB (legacy unit-test path): delivery is signalled purely
            // by `mark_received` clearing the cross-peer `in_flight` map.  Treat
            // a hash as delivered once it is no longer in `in_flight`.
            HashSet::new()
        };

        let has_db = self.chain_db.is_some();
        let mut completed: Vec<(SocketAddr, FetchRange)> = Vec::new();
        let mut failed: Vec<(SocketAddr, FetchRange)> = Vec::new();

        for (peer, ranges) in self.dispatched_ranges.iter_mut() {
            ranges.retain(|(range, hashes)| {
                if hashes.is_empty() {
                    // A range with no resolvable hashes (e.g. all already in
                    // ChainDB at dispatch time) is immediately complete.
                    completed.push((*peer, range.clone()));
                    return false;
                }
                let all_delivered = if has_db {
                    hashes.iter().all(|h| stored.contains(h))
                } else {
                    // Legacy path: delivered == cleared from cross-peer in_flight.
                    hashes.iter().all(|h| !self.in_flight.contains_key(h))
                };
                if all_delivered {
                    completed.push((*peer, range.clone()));
                    // Clear any lingering cross-peer reservations for this range.
                    for h in hashes {
                        self.in_flight.remove(h);
                    }
                    return false;
                }
                // Failed/timed-out: every reservation has expired from the
                // cross-peer map but the blocks are NOT in ChainDB.
                let all_expired = has_db && hashes.iter().all(|h| !self.in_flight.contains_key(h));
                if all_expired {
                    failed.push((*peer, range.clone()));
                    return false;
                }
                true
            });
        }
        self.dispatched_ranges.retain(|_, v| !v.is_empty());

        for (peer, range) in completed {
            self.decision_engine.mark_completed(peer, &range);
        }
        for (peer, range) in failed {
            // `mark_failed` re-queues the range; the genuine gap is re-fetched.
            self.decision_engine.mark_failed(peer, &range);
        }
    }

    /// Remove a block hash from the in-flight map.
    ///
    /// Called when a block is successfully received or a fetch fails.
    pub fn mark_received(&mut self, hash: &[u8; 32]) {
        self.in_flight.remove(hash);
    }
}

/// Batch sorted pending headers into fetch ranges.
///
/// Groups headers into `FetchRange` entries of up to `MAX_BATCH_SIZE` headers
/// each.  The BlockFetch `MsgRequestRange(from, to)` protocol uses Points
/// (slot + hash) to define the range, and the server walks the chain between
/// those points — slot gaps between blocks are perfectly normal in Cardano
/// (Praos slots are sparse) and do NOT require splitting into separate ranges.
///
/// The input must be sorted by slot (ascending).
fn batch_headers_into_ranges(headers: &[(SocketAddr, PendingHeader)]) -> Vec<FetchRange> {
    if headers.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut batch_start = &headers[0].1;
    let mut batch_end = &headers[0].1;
    let mut batch_count = 1usize;

    for (_, header) in headers.iter().skip(1) {
        if batch_count < MAX_BATCH_SIZE {
            batch_end = header;
            batch_count += 1;
        } else {
            // Flush the current batch — hit size limit.
            ranges.push(FetchRange {
                from: Point::Specific(batch_start.slot, batch_start.hash),
                to: Point::Specific(batch_end.slot, batch_end.hash),
            });
            batch_start = header;
            batch_end = header;
            batch_count = 1;
        }
    }

    // Flush the final batch.
    ranges.push(FetchRange {
        from: Point::Specific(batch_start.slot, batch_start.hash),
        to: Point::Specific(batch_end.slot, batch_end.hash),
    });

    ranges
}

/// Per-peer BlockFetch worker.
///
/// Receives fetch ranges from the decision task via `request_rx`, downloads
/// blocks via `BlockFetchClient`, and sends decoded blocks to the main run loop
/// via `fetched_blocks_tx`.
///
/// Runs as a dedicated tokio task for each hot peer. Exits when:
/// - The request channel is closed (peer deregistered from decision task).
/// - A protocol error occurs (bearer died).
/// - The cancellation token is triggered (node shutdown).
///
/// # Arguments
///
/// * `channel` — Mux channel for the BlockFetch mini-protocol (protocol ID 3).
/// * `request_rx` — Receiver for fetch range requests from the decision task.
/// * `fetched_blocks_tx` — Sender for decoded blocks to the main run loop.
/// * `peer_addr` — Remote peer socket address (for logging and `FetchedBlock.peer`).
/// * `byron_epoch_length` — Byron epoch length for block deserialization.
/// * `cancel` — Cancellation token for graceful shutdown.
pub async fn blockfetch_worker(
    mut channel: MuxChannel,
    mut request_rx: mpsc::Receiver<Vec<FetchRange>>,
    fetched_blocks_tx: mpsc::Sender<FetchedBlock>,
    peer_addr: SocketAddr,
    byron_epoch_length: u64,
    cancel: CancellationToken,
) {
    info!(%peer_addr, "blockfetch worker started");

    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                debug!(%peer_addr, "blockfetch worker shutting down");
                break;
            }

            request = request_rx.recv() => {
                let ranges = match request {
                    Some(r) => r,
                    None => {
                        debug!(%peer_addr, "blockfetch request channel closed");
                        break;
                    }
                };

                for range in ranges {
                    let from = range.from.clone();
                    let to = range.to.clone();

                    // Accumulate decoded blocks from the callback to send after
                    // the fetch completes (callback is FnMut, not async).
                    let mut decoded_blocks: Vec<dugite_primitives::block::Block> = Vec::new();
                    let epoch_len = byron_epoch_length;

                    // Wrap the fetch in a timeout to detect dead connections.
                    // If the peer's TCP connection is half-open, recv() blocks
                    // forever; this timeout ensures the worker exits and the
                    // connection lifecycle manager can clean up.
                    let fetch_future = BlockFetchClient::fetch_range(
                        &mut channel,
                        from,
                        to,
                        |block_cbor| {
                            // Decode the block from raw CBOR.
                            match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                                &block_cbor,
                                epoch_len,
                            ) {
                                Ok(block) => {
                                    decoded_blocks.push(block);
                                    Ok(())
                                }
                                Err(e) => {
                                    Err(dugite_network::error::ProtocolError::CborDecode {
                                        protocol: "BlockFetch",
                                        reason: format!("block decode failed: {e}"),
                                    })
                                }
                            }
                        },
                    );

                    let result = match tokio::time::timeout(IN_FLIGHT_TIMEOUT, fetch_future).await {
                        Ok(inner) => inner,
                        Err(_elapsed) => {
                            error!(
                                %peer_addr,
                                timeout_secs = IN_FLIGHT_TIMEOUT.as_secs(),
                                "blockfetch range timed out, exiting worker"
                            );
                            return;
                        }
                    };

                    match result {
                        Ok(count) => {
                            debug!(
                                %peer_addr,
                                block_count = count,
                                "blockfetch range complete"
                            );

                            // Send each decoded block to the run loop.
                            for block in decoded_blocks {
                                let fetched = FetchedBlock {
                                    peer: peer_addr,
                                    tip_slot: block.slot().0,
                                    tip_hash: block.hash().0,
                                    tip_block_number: block.block_number().0,
                                    block,
                                };

                                if fetched_blocks_tx.send(fetched).await.is_err() {
                                    warn!(
                                        %peer_addr,
                                        "fetched_blocks channel closed, exiting worker"
                                    );
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                %peer_addr,
                                error = %e,
                                "blockfetch protocol error, exiting worker"
                            );
                            // Bearer died or protocol violation — exit the worker.
                            // The connection lifecycle manager will detect the dead
                            // connection and clean up.
                            return;
                        }
                    }
                }
            }
        }
    }

    // Send MsgClientDone to cleanly terminate the BlockFetch protocol.
    if let Err(e) = BlockFetchClient::done(&mut channel).await {
        debug!(
            %peer_addr,
            error = %e,
            "failed to send MsgClientDone (bearer may already be closed)"
        );
    }

    info!(%peer_addr, "blockfetch worker stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(port: u16) -> SocketAddr {
        use std::net::{IpAddr, Ipv4Addr};
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
    }

    fn test_header(slot: u64) -> PendingHeader {
        let mut hash = [0u8; 32];
        hash[0..8].copy_from_slice(&slot.to_be_bytes());
        PendingHeader {
            slot,
            hash,
            header_cbor: vec![0x82, 0x01],
        }
    }

    #[test]
    fn batch_headers_single_range() {
        let addr = test_addr(3001);
        let headers: Vec<(SocketAddr, PendingHeader)> =
            (10..=15).map(|slot| (addr, test_header(slot))).collect();

        let ranges = batch_headers_into_ranges(&headers);
        assert_eq!(ranges.len(), 1);

        // Verify the range covers slots 10-15.
        match (&ranges[0].from, &ranges[0].to) {
            (Point::Specific(from_slot, _), Point::Specific(to_slot, _)) => {
                assert_eq!(*from_slot, 10);
                assert_eq!(*to_slot, 15);
            }
            _ => panic!("expected Specific points"),
        }
    }

    #[test]
    fn batch_headers_gap_does_not_split_ranges() {
        // Slot gaps are normal in Cardano (Praos slots are sparse).
        // BlockFetch uses Points for range boundaries and walks the chain,
        // so gaps do NOT require splitting into separate ranges.
        let addr = test_addr(3001);
        let headers: Vec<(SocketAddr, PendingHeader)> = vec![
            (addr, test_header(10)),
            (addr, test_header(11)),
            // Gap: slot 12-19 missing (normal in Cardano)
            (addr, test_header(20)),
            (addr, test_header(21)),
        ];

        let ranges = batch_headers_into_ranges(&headers);
        // All four headers should be in a single range.
        assert_eq!(ranges.len(), 1);

        match (&ranges[0].from, &ranges[0].to) {
            (Point::Specific(from, _), Point::Specific(to, _)) => {
                assert_eq!(*from, 10);
                assert_eq!(*to, 21);
            }
            _ => panic!("expected Specific points"),
        }
    }

    #[test]
    fn batch_headers_empty() {
        let ranges = batch_headers_into_ranges(&[]);
        assert!(ranges.is_empty());
    }

    #[test]
    fn batch_headers_single_header() {
        let addr = test_addr(3001);
        let headers = vec![(addr, test_header(42))];
        let ranges = batch_headers_into_ranges(&headers);
        assert_eq!(ranges.len(), 1);

        match (&ranges[0].from, &ranges[0].to) {
            (Point::Specific(from, _), Point::Specific(to, _)) => {
                assert_eq!(*from, 42);
                assert_eq!(*to, 42);
            }
            _ => panic!("expected Specific points"),
        }
    }

    #[tokio::test]
    async fn task_register_deregister_peer() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains, tx, 21600, cancel);

        let addr = test_addr(3001);
        let (peer_tx, _peer_rx) = mpsc::channel(16);

        // Register.
        task.register_peer(addr, peer_tx);
        assert!(task.fetch_senders.contains_key(&addr));

        // Deregister.
        task.deregister_peer(&addr);
        assert!(!task.fetch_senders.contains_key(&addr));
    }

    #[tokio::test]
    async fn task_skips_blocks_at_or_below_tip() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        // Set current tip to slot 100.
        task.update_tip_slot(100);

        // Register a peer with a fetch channel.
        let addr = test_addr(3001);
        let (peer_tx, mut peer_rx) = mpsc::channel(16);
        task.register_peer(addr, peer_tx);

        // Add candidate chain with headers at slots 50 (below tip) and 150 (above tip).
        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    tip_hash: [0xAA; 32],
                    tip_block_number: 200,
                    pending_headers: vec![test_header(50), test_header(150)],
                    ..Default::default()
                },
            );
        }

        // Run one decision iteration.
        task.evaluate_and_fetch().await;

        // The peer should receive a fetch request for slot 150 only.
        match peer_rx.try_recv() {
            Ok(ranges) => {
                assert_eq!(ranges.len(), 1);
                match &ranges[0].from {
                    Point::Specific(slot, _) => assert_eq!(*slot, 150),
                    _ => panic!("expected Specific point"),
                }
            }
            Err(_) => panic!("expected fetch request"),
        }

        // No blocks should be sent to the run loop (worker isn't running).
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn task_no_dispatch_without_peers() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        // Add headers but no peers registered.
        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                test_addr(3001),
                CandidateChainState {
                    tip_slot: 100,
                    tip_hash: [0xBB; 32],
                    tip_block_number: 100,
                    pending_headers: vec![test_header(50)],
                    ..Default::default()
                },
            );
        }

        // Should complete without panicking.
        task.evaluate_and_fetch().await;
    }

    #[tokio::test]
    async fn task_marks_in_flight() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        let addr = test_addr(3001);
        let (peer_tx, _peer_rx) = mpsc::channel(16);
        task.register_peer(addr, peer_tx);

        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    tip_hash: [0xCC; 32],
                    tip_block_number: 200,
                    pending_headers: vec![test_header(150)],
                    ..Default::default()
                },
            );
        }

        // First evaluation should dispatch.
        task.evaluate_and_fetch().await;
        assert!(!task.in_flight.is_empty());

        // Second evaluation should skip (already in-flight).
        let addr2 = test_addr(3002);
        let (peer_tx2, mut peer_rx2) = mpsc::channel(16);
        task.register_peer(addr2, peer_tx2);

        task.evaluate_and_fetch().await;

        // Second peer should NOT receive a request (block already in-flight).
        assert!(peer_rx2.try_recv().is_err());

        // Mark as received — should be fetchable again.
        let hash = test_header(150).hash;
        task.mark_received(&hash);
        assert!(task.in_flight.is_empty());
    }

    #[tokio::test]
    async fn task_run_cancels_cleanly() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains, tx, 21600, cancel.clone());

        // Cancel immediately.
        cancel.cancel();
        // run() should return without hanging.
        task.run().await;
    }

    // ── Per-peer chain tracking (issue #702) ─────────────────────────────────

    /// Aberrant peers are excluded from fetch dispatch.
    ///
    /// When a peer's `CandidateChainState.fetch_status` is Aberrant,
    /// `evaluate_and_fetch` must not dispatch any ranges to it even if it has
    /// pending headers above the current tip.
    #[tokio::test]
    async fn aberrant_peer_not_dispatched() {
        use super::super::connection_lifecycle::{
            CandidateChainState, PeerFetchStatus, ABERRANT_FAILURE_THRESHOLD,
        };

        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        let addr = test_addr(3001);
        let (peer_tx, mut peer_rx) = mpsc::channel(16);
        task.register_peer(addr, peer_tx);

        // Build an Aberrant state for the peer.
        let mut aberrant_state = CandidateChainState {
            tip_slot: 200,
            tip_hash: [0xAA; 32],
            tip_block_number: 200,
            pending_headers: vec![test_header(150)],
            ..Default::default()
        };
        // Force Aberrant status by calling record_fetch_failed enough times.
        for _ in 0..ABERRANT_FAILURE_THRESHOLD {
            aberrant_state.record_fetch_failed(addr);
        }
        assert_eq!(aberrant_state.fetch_status, PeerFetchStatus::Aberrant);

        {
            let mut chains = candidate_chains.write().await;
            chains.insert(addr, aberrant_state);
        }

        // Run the decision task — the Aberrant peer must receive NOTHING.
        task.evaluate_and_fetch().await;

        assert!(
            peer_rx.try_recv().is_err(),
            "Aberrant peer must not receive fetch requests"
        );
    }

    /// Both Ready and Busy peers are eligible for fetch dispatch.
    ///
    /// Verifies that `in_flight_blocks` from `CandidateChainState` is propagated
    /// to `PeerFetchState.in_flight` and that Busy peers (unlike Aberrant peers)
    /// are still eligible and receive fetch requests.  The decision engine
    /// distributes work to whichever peer has capacity — we verify at least ONE
    /// of the two peers receives a fetch request (the exact choice is non-
    /// deterministic when latencies are equal).
    #[tokio::test]
    async fn busy_peer_is_eligible_unlike_aberrant() {
        use super::super::connection_lifecycle::{CandidateChainState, PeerFetchStatus};

        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        let ready_addr = test_addr(3011);
        let busy_addr = test_addr(3012);
        let (ready_tx, mut ready_rx) = mpsc::channel(16);
        let (busy_tx, mut busy_rx) = mpsc::channel(16);
        task.register_peer(ready_addr, ready_tx);
        task.register_peer(busy_addr, busy_tx);

        // Ready peer: zero in_flight.
        let ready_state = CandidateChainState {
            tip_slot: 200,
            tip_hash: [0xAA; 32],
            tip_block_number: 200,
            pending_headers: vec![test_header(150)],
            fetch_status: PeerFetchStatus::Ready,
            in_flight_blocks: 0,
            ..Default::default()
        };
        // Busy peer: non-zero in_flight, but still eligible.
        let busy_state = CandidateChainState {
            tip_slot: 200,
            tip_hash: [0xBB; 32],
            tip_block_number: 200,
            pending_headers: vec![test_header(151)],
            fetch_status: PeerFetchStatus::Busy,
            in_flight_blocks: 5,
            ..Default::default()
        };

        {
            let mut chains = candidate_chains.write().await;
            chains.insert(ready_addr, ready_state);
            chains.insert(busy_addr, busy_state);
        }

        task.evaluate_and_fetch().await;

        // At least one of the two eligible peers must have received a range.
        // Busy peers are NOT excluded (only Aberrant peers are).
        let ready_got = ready_rx.try_recv().is_ok();
        let busy_got = busy_rx.try_recv().is_ok();
        assert!(
            ready_got || busy_got,
            "at least one eligible peer (Ready or Busy) must receive a fetch request"
        );
    }

    /// PeerFetchStatus::default() is Ready.
    ///
    /// Regression lock: the default fetch status for a new peer must be Ready
    /// so it is immediately eligible for fetch decisions before any delivery.
    #[test]
    fn peer_fetch_status_default_ready() {
        use super::super::connection_lifecycle::PeerFetchStatus;
        assert_eq!(PeerFetchStatus::default(), PeerFetchStatus::Ready);
    }

    // ── Issue #706: EWMA latency from PeerManager ────────────────────────────

    /// Decision engine prefers lower-latency peer when in-flight counts tie.
    ///
    /// Two peers, same in_flight (0), different EWMA latency.  The decision
    /// engine must dispatch the single queued range to the faster peer.
    ///
    /// This validates the fix for issue #706 via the `BlockFetchDecision`
    /// engine directly (the decision layer is what `pump_decisions` calls,
    /// so testing here gives deterministic, sync-friendly coverage).
    #[test]
    fn decision_engine_prefers_lower_latency_on_tied_in_flight() {
        use dugite_network::protocol::blockfetch::decision::{BlockFetchDecision, PeerFetchState};

        let slow_addr = test_addr(4001);
        let fast_addr = test_addr(4002);

        let mut engine = BlockFetchDecision::with_defaults();
        engine.add_range(
            Point::Specific(100, [0x10; 32]),
            Point::Specific(200, [0x20; 32]),
        );

        // Both peers: zero in-flight (tie on capacity).
        // Fast peer has 20 ms EWMA RTT; slow peer has 500 ms EWMA RTT.
        let peers = vec![
            PeerFetchState {
                addr: slow_addr,
                latency_ms: 500.0,
                in_flight: 0,
                tip_slot: 300,
            },
            PeerFetchState {
                addr: fast_addr,
                latency_ms: 20.0,
                in_flight: 0,
                tip_slot: 300,
            },
        ];

        let (selected_addr, _range) = engine.select_peer(&peers).unwrap();
        assert_eq!(
            selected_addr, fast_addr,
            "decision engine must prefer the lower-latency peer when in-flight counts tie"
        );
    }

    /// When no PeerManager is provided the task falls back to PEER_LATENCY_DEFAULT_MS.
    ///
    /// Exercises the `peer_manager = None` path to ensure the fallback is
    /// exercised in evaluate_and_fetch without panicking, and that the fetch
    /// request is still dispatched correctly to the single peer.
    #[tokio::test]
    async fn ewma_fallback_to_default_when_no_peer_manager() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        // `new()` leaves peer_manager = None, exercising the default-latency path.
        let mut task = BlockFetchLogicTask::new(candidate_chains.clone(), tx, 21600, cancel);

        let addr = test_addr(5001);
        let (peer_tx, mut peer_rx) = mpsc::channel(16);
        task.register_peer(addr, peer_tx);

        // Use a slot above 0 so it won't be filtered by the fallback slot check.
        task.update_tip_slot(0);

        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 300,
                    tip_hash: [0xDE; 32],
                    tip_block_number: 300,
                    pending_headers: vec![test_header(250)],
                    ..Default::default()
                },
            );
        }

        // Must dispatch without panicking even with no peer manager.
        task.evaluate_and_fetch().await;

        assert!(
            peer_rx.try_recv().is_ok(),
            "fetch request must be dispatched even when peer_manager is None (default latency path)"
        );
    }

    // ─── F0 / F1 / F2 / F3 multi-peer fetch coverage ─────────────────────────
    //
    // The vetted multi-peer design removes the (unsound) ReorderBuffer and
    // instead delivers N-peer parallelism via four mechanical fixes in the
    // fetch-assignment layer:
    //   F0 — capacity-release wiring (per-peer decision-engine cap returns to 0)
    //   F1 — intra-tick cross-peer hash reservation (no double-assign in a tick)
    //   F2 — immediate Aberrant release (reassign in ~10 ms, not 60 s)
    //   F3 — 4-deep per-peer concurrency window
    //
    // Block *ordering* to the apply loop is preserved by construction: nothing
    // downstream of fetch changes (single-threaded `apply_fetched_block`, the
    // `connects_to_tip` gate, and `TriggeredFork` oldest-first reassembly).  The
    // tests below therefore assert the fetch-assignment invariants — disjoint
    // hash assignment across peers, bounded buffering of un-fetched gaps, the
    // unchanged fork-prune invariant, and rollback resetting the in-flight state
    // — which together guarantee chain-order delivery without any reorder buffer.

    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};

    /// Build a header whose hash encodes both `slot` and a `variant` byte, so
    /// distinct forks at the same slot get distinct hashes (Bug-I regression).
    fn fork_header(slot: u64, variant: u8) -> PendingHeader {
        let mut hash = [0u8; 32];
        hash[0..8].copy_from_slice(&slot.to_be_bytes());
        hash[31] = variant;
        PendingHeader {
            slot,
            hash,
            header_cbor: vec![0x82, 0x01],
        }
    }

    /// Open an empty on-disk ChainDB in a fresh temp dir.  The returned
    /// `TempDir` guard must be kept alive for the DB's lifetime.
    fn test_chain_db() -> (Arc<RwLock<ChainDB>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDB::open(dir.path()).unwrap();
        (Arc::new(RwLock::new(db)), dir)
    }

    /// Mark a block as "delivered" by inserting it into the ChainDB, exactly as
    /// `apply_fetched_block` would once the run loop applies it.
    async fn store_block(db: &Arc<RwLock<ChainDB>>, header: &PendingHeader, block_no: u64) {
        let mut g = db.write().await;
        g.add_block(
            Hash32::from_bytes(header.hash),
            SlotNo(header.slot),
            BlockNo(block_no),
            Hash32::ZERO,
            vec![0x01],
        )
        .unwrap();
    }

    fn task_with_db(
        candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
        fetched_tx: mpsc::Sender<FetchedBlock>,
        db: Arc<RwLock<ChainDB>>,
    ) -> BlockFetchLogicTask {
        BlockFetchLogicTask::new_with_chain_db(
            candidate_chains,
            fetched_tx,
            21600,
            CancellationToken::new(),
            Some(db),
        )
    }

    /// Drain a peer's worker channel into a flat list of (from_slot, to_slot).
    fn drain_ranges(rx: &mut mpsc::Receiver<Vec<FetchRange>>) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        while let Ok(ranges) = rx.try_recv() {
            for r in ranges {
                out.push(r.slot_bounds());
            }
        }
        out
    }

    // ── F0: per-peer capacity returns to zero ────────────────────────────────

    /// F0 regression: after a full round (dispatch → deliver all blocks), the
    /// decision engine's total in-flight capacity returns to zero.  Without the
    /// release wiring the per-peer `in_flight` map only ever grows.
    #[tokio::test]
    async fn decision_engine_inflight_returns_to_zero_after_round() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(64);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db.clone());

        let addr = test_addr(3001);
        let (peer_tx, mut peer_rx) = mpsc::channel(64);
        task.register_peer(addr, peer_tx);

        let headers = vec![
            fork_header(150, 1),
            fork_header(151, 1),
            fork_header(152, 1),
        ];
        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: headers.clone(),
                    ..Default::default()
                },
            );
        }

        // Tick 1: dispatch the range.
        task.evaluate_and_fetch().await;
        assert!(
            task.decision_engine.total_in_flight() > 0,
            "range dispatched"
        );
        assert!(!drain_ranges(&mut peer_rx).is_empty(), "peer got the range");

        // Deliver every block (run loop would apply them into ChainDB) and drop
        // them from the peer's pending set.
        for h in &headers {
            store_block(&db, h, 100).await;
        }
        {
            let mut chains = candidate_chains.write().await;
            chains.get_mut(&addr).unwrap().pending_headers.clear();
        }

        // Tick 2: the release pass observes all blocks stored → capacity freed.
        task.evaluate_and_fetch().await;
        assert_eq!(
            task.decision_engine.total_in_flight(),
            0,
            "per-peer decision-engine capacity must return to zero after delivery"
        );
    }

    /// F0: with a low per-peer cap and several peers, sustained delivery keeps
    /// dispatch flowing tick after tick and the decision queue stays bounded.
    /// This is the test that would *fail today* if F3 (low cap) landed without
    /// F0 (release) — the queue would grow monotonically and dispatch stall.
    #[tokio::test]
    async fn no_stall_under_low_cap() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(256);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db.clone());
        // Force the smallest possible window to provoke a stall if F0 is broken.
        task.decision_engine = BlockFetchDecision::new(1);

        let mut peer_rxs = Vec::new();
        for port in 3001..3005u16 {
            let addr = test_addr(port);
            let (ptx, prx) = mpsc::channel(256);
            task.register_peer(addr, ptx);
            peer_rxs.push((addr, prx));
            let mut chains = candidate_chains.write().await;
            chains.insert(addr, CandidateChainState::default());
        }

        let mut next_slot = 1000u64;
        let mut total_dispatched = 0usize;
        // M >> cap ticks: each tick adds fresh headers to every peer, dispatches,
        // then "delivers" everything so the next tick can proceed.
        for _tick in 0..40 {
            {
                let mut chains = candidate_chains.write().await;
                for (addr, _) in &peer_rxs {
                    let h = fork_header(next_slot, 1);
                    next_slot += 1;
                    chains.get_mut(addr).unwrap().pending_headers.push(h);
                }
            }
            task.evaluate_and_fetch().await;

            // Deliver everything that was dispatched this tick.
            let mut to_store = Vec::new();
            for (addr, rx) in &mut peer_rxs {
                let got = drain_ranges(rx);
                total_dispatched += got.len();
                // store the blocks now in ChainDB + clear pending
                let mut chains = candidate_chains.write().await;
                let st = chains.get_mut(addr).unwrap();
                for h in st.pending_headers.drain(..) {
                    to_store.push(h);
                }
            }
            for h in to_store {
                store_block(&db, &h, 1).await;
            }
            // Run the release pass (next tick's head) so capacity frees before
            // the bound assertion.
            task.evaluate_and_fetch().await;

            // The queue must stay bounded — never grow without limit.
            assert!(
                task.decision_engine.queue_len() <= peer_rxs.len(),
                "decision queue must stay bounded under low cap (F0 release working): \
                 queue_len={}",
                task.decision_engine.queue_len()
            );
        }

        assert!(
            total_dispatched >= 30,
            "dispatch must be sustained across ticks (got {total_dispatched})"
        );
        assert_eq!(
            task.decision_engine.total_in_flight(),
            0,
            "all capacity released after final delivery"
        );
    }

    /// F0 #4: a full worker channel rolls back BOTH the provisional cross-peer
    /// hash reservation AND the per-peer decision-engine capacity, so transient
    /// backpressure never permanently burns capacity.
    #[tokio::test]
    async fn channel_full_rolls_back_per_peer_capacity() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(16);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);

        // Capacity-1 worker channel; fill it so the next try_send returns Full.
        let addr = test_addr(3001);
        let (peer_tx, _peer_rx) = mpsc::channel::<Vec<FetchRange>>(1);
        peer_tx.try_send(vec![]).unwrap(); // occupy the single slot
        task.register_peer(addr, peer_tx);

        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: vec![fork_header(150, 1)],
                    ..Default::default()
                },
            );
        }

        task.evaluate_and_fetch().await;

        assert_eq!(
            task.decision_engine.total_in_flight(),
            0,
            "channel-Full must roll back the per-peer decision-engine capacity"
        );
        assert!(
            task.in_flight.is_empty(),
            "channel-Full must roll back the provisional cross-peer hash reservation"
        );
    }

    // ── F1: intra-tick reservation (no double-assign) ────────────────────────

    /// F1: out-of-order multi-peer reassembly.  Two peers advertise an
    /// *overlapping* set of headers (the same blocks fetched from either peer);
    /// within a single decision tick no block hash may be assigned to more than
    /// one peer.  Combined with the unchanged single-threaded apply loop +
    /// `connects_to_tip` gate, disjoint assignment is exactly what lets blocks
    /// arrive in any order yet be applied in chain order without a buffer.
    #[tokio::test]
    async fn intra_tick_no_double_assign() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(256);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);
        // 4-deep window so a single fast peer could greedily grab every range
        // unless the intra-tick reservation stops the second peer re-claiming
        // the same hashes.
        task.decision_engine = BlockFetchDecision::new(4);

        let peer_a = test_addr(3001);
        let peer_b = test_addr(3002);
        let (atx, mut arx) = mpsc::channel(256);
        let (btx, mut brx) = mpsc::channel(256);
        task.register_peer(peer_a, atx);
        task.register_peer(peer_b, btx);

        // BOTH peers offer the SAME blocks (slots 150..158, variant 1).
        let shared: Vec<PendingHeader> = (150..=158).map(|s| fork_header(s, 1)).collect();
        {
            let mut chains = candidate_chains.write().await;
            for addr in [peer_a, peer_b] {
                chains.insert(
                    addr,
                    CandidateChainState {
                        tip_slot: 200,
                        pending_headers: shared.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        task.evaluate_and_fetch().await;

        // Collect every (slot range) handed to each peer and verify the slot
        // windows of the two peers do not overlap — i.e. no hash double-assigned.
        let a_ranges = drain_ranges(&mut arx);
        let b_ranges = drain_ranges(&mut brx);

        // Reconstruct the set of slots covered by each peer.
        let slots_of = |ranges: &[(u64, u64)]| -> HashSet<u64> {
            let mut s = HashSet::new();
            for (from, to) in ranges {
                for slot in *from..=*to {
                    s.insert(slot);
                }
            }
            s
        };
        let a_slots = slots_of(&a_ranges);
        let b_slots = slots_of(&b_ranges);
        let overlap: Vec<u64> = a_slots.intersection(&b_slots).copied().collect();
        assert!(
            overlap.is_empty(),
            "no block may be assigned to two peers in one tick (F1): overlap={overlap:?}"
        );
        // Every reserved hash appears at most once in the cross-peer map.
        assert!(
            task.in_flight.len() <= shared.len(),
            "cross-peer in_flight must hold each hash at most once"
        );
    }

    /// Bug-I regression: two genuinely different blocks at the SAME slot
    /// (distinct hashes = a real fork) must BOTH be dispatched — the F1
    /// reservation keys on hash, not slot, so it must not collapse a fork.
    #[tokio::test]
    async fn different_hash_same_slot_both_fetched() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(64);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);

        let addr = test_addr(3001);
        let (peer_tx, mut peer_rx) = mpsc::channel(64);
        task.register_peer(addr, peer_tx);

        // Same slot 150, two distinct hashes (variant 1 and 2): a real fork.
        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: vec![fork_header(150, 1), fork_header(150, 2)],
                    ..Default::default()
                },
            );
        }

        task.evaluate_and_fetch().await;

        // Both distinct hashes must be reserved (neither collapsed away).
        assert_eq!(
            task.in_flight.len(),
            2,
            "both fork blocks at the same slot must be reserved for fetch"
        );
        assert!(!drain_ranges(&mut peer_rx).is_empty());
    }

    // ── F2: Aberrant fast-release / slow-peer reassignment ───────────────────

    /// F2 + gap/slow-peer bound: a slow peer holds a dispatched range, then is
    /// marked Aberrant.  Its un-fetched gap is released and reassigned to a
    /// healthy peer on the next tick — without waiting out the 60 s timeout.
    /// The reorder/in-flight buffer holds the gap (does not advance past it) and
    /// the cross-peer map stays bounded by the number of outstanding hashes.
    #[tokio::test]
    async fn aberrant_releases_in_flight_and_reassigns() {
        use super::super::connection_lifecycle::ABERRANT_FAILURE_THRESHOLD;

        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(64);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);

        let slow = test_addr(3001);
        let healthy = test_addr(3002);
        let (slow_tx, mut slow_rx) = mpsc::channel(64);
        let (healthy_tx, mut healthy_rx) = mpsc::channel(64);

        let gap = fork_header(150, 1);
        {
            let mut chains = candidate_chains.write().await;
            // Only the slow peer has the header initially.
            chains.insert(
                slow,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: vec![gap.clone()],
                    ..Default::default()
                },
            );
        }

        // Tick 1: register ONLY the slow peer so the gap is deterministically
        // dispatched to it (no peer-selection race) and reserved.
        task.register_peer(slow, slow_tx);
        task.evaluate_and_fetch().await;
        assert!(
            !drain_ranges(&mut slow_rx).is_empty(),
            "slow peer got the gap"
        );
        assert_eq!(
            task.in_flight.len(),
            1,
            "gap held in the reorder/in-flight buffer"
        );
        // The buffer is bounded: it holds exactly the one outstanding hash, no
        // unbounded growth while the gap is unfilled.
        assert!(task.decision_engine.total_in_flight() >= 1);
        let holder = task.in_flight.values().next().map(|(p, _)| *p);
        assert_eq!(
            holder,
            Some(slow),
            "gap reserved against the slow peer in tick 1"
        );

        // The slow peer never delivers and is escalated to Aberrant; the healthy
        // peer is now registered and also learns the header (it can serve the gap).
        {
            let mut chains = candidate_chains.write().await;
            let st = chains.get_mut(&slow).unwrap();
            for _ in 0..ABERRANT_FAILURE_THRESHOLD {
                st.record_fetch_failed(slow);
            }
            chains.insert(
                healthy,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: vec![gap.clone()],
                    ..Default::default()
                },
            );
        }
        task.register_peer(healthy, healthy_tx);

        // Tick 2: F2 purges the Aberrant peer's reservation + capacity, and the
        // gap reassigns to the healthy peer in the SAME tick (~10 ms cadence).
        task.evaluate_and_fetch().await;

        let healthy_got = drain_ranges(&mut healthy_rx);
        assert!(
            !healthy_got.is_empty(),
            "gap must reassign to the healthy peer immediately after Aberrant release"
        );
        // The cross-peer reservation is now held against the healthy peer only.
        assert_eq!(
            task.in_flight.len(),
            1,
            "buffer still holds exactly the one gap"
        );
        let holder = task.in_flight.values().next().map(|(p, _)| *p);
        assert_eq!(
            holder,
            Some(healthy),
            "gap now reserved against the healthy peer"
        );
    }

    // ── F3: multi-peer disjoint spread ───────────────────────────────────────

    /// F3: with multiple Ready peers all advertising the SAME long header set
    /// and a per-peer window of 4, the decision engine spreads ranges across
    /// distinct peers with no hash assigned twice — genuine N-peer parallelism.
    #[tokio::test]
    async fn multi_peer_disjoint_spread() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(512);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);
        // Production default window.
        task.decision_engine = BlockFetchDecision::new(BLOCKFETCH_PER_PEER_WINDOW);

        // 4 peers, all offering the same 400 headers → enough ranges (each range
        // is up to MAX_BATCH_SIZE=100 headers) to spread across all 4 peers.
        let headers: Vec<PendingHeader> = (1000..1400).map(|s| fork_header(s, 1)).collect();
        let mut rxs = Vec::new();
        for port in 4001..4005u16 {
            let addr = test_addr(port);
            let (ptx, prx) = mpsc::channel(512);
            task.register_peer(addr, ptx);
            rxs.push((addr, prx));
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 2000,
                    pending_headers: headers.clone(),
                    ..Default::default()
                },
            );
        }

        task.evaluate_and_fetch().await;

        // Gather the slot coverage per peer and assert global disjointness.
        let mut all_slots: HashSet<u64> = HashSet::new();
        let mut peers_with_work = 0;
        let mut total_assigned = 0usize;
        for (_, rx) in &mut rxs {
            let ranges = drain_ranges(rx);
            if !ranges.is_empty() {
                peers_with_work += 1;
            }
            for (from, to) in ranges {
                for slot in from..=to {
                    total_assigned += 1;
                    assert!(
                        all_slots.insert(slot),
                        "slot {slot} assigned to more than one peer (F3 must stay disjoint)"
                    );
                }
            }
        }
        assert!(
            peers_with_work >= 2,
            "work must spread across multiple peers (got {peers_with_work})"
        );
        assert_eq!(
            total_assigned,
            all_slots.len(),
            "every assigned slot is unique across all peers"
        );
    }

    // ── Rollback resets the in-flight / reorder state ────────────────────────

    /// Rollback-resets-buffer: `decision_engine.rollback_to` drops queued and
    /// in-flight ranges beyond the rollback point, and the cross-peer in-flight
    /// hash map is cleared for the rolled-back blocks.  This mirrors the
    /// `MsgRollBackward` path that must reset the fetch-assignment state so a
    /// competing fork's blocks are re-fetched cleanly rather than suppressed as
    /// stale late-duplicates.
    #[tokio::test]
    async fn rollback_resets_inflight_buffer() {
        let candidate_chains = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(64);
        let (db, _dir) = test_chain_db();
        let mut task = task_with_db(candidate_chains.clone(), tx, db);

        let addr = test_addr(3001);
        let (peer_tx, mut peer_rx) = mpsc::channel(64);
        task.register_peer(addr, peer_tx);

        // Dispatch headers spanning slots 150..158.
        let headers: Vec<PendingHeader> = (150..=158).map(|s| fork_header(s, 1)).collect();
        {
            let mut chains = candidate_chains.write().await;
            chains.insert(
                addr,
                CandidateChainState {
                    tip_slot: 200,
                    pending_headers: headers.clone(),
                    ..Default::default()
                },
            );
        }
        task.evaluate_and_fetch().await;
        assert!(!drain_ranges(&mut peer_rx).is_empty());
        assert!(
            !task.in_flight.is_empty(),
            "blocks reserved before rollback"
        );
        assert!(task.decision_engine.total_in_flight() >= 1);

        // Simulate the MsgRollBackward handler resetting the fetch-assignment
        // state to slot 100: drop the future ranges from the decision engine and
        // clear the cross-peer reservations for the rolled-back blocks.
        task.decision_engine
            .rollback_to(&Point::Specific(100, [0x00; 32]));
        // The MsgRollBackward path also clears the cross-peer hash map for any
        // block above the rollback point so it can be re-fetched.
        task.in_flight.retain(|hash, _| {
            // hash[0..8] encodes the slot (see fork_header)
            let mut slot_bytes = [0u8; 8];
            slot_bytes.copy_from_slice(&hash[0..8]);
            u64::from_be_bytes(slot_bytes) <= 100
        });
        task.dispatched_ranges.clear();

        assert_eq!(
            task.decision_engine.total_in_flight(),
            0,
            "rollback must clear in-flight ranges beyond the rollback point"
        );
        assert_eq!(
            task.decision_engine.queue_len(),
            0,
            "rollback must drop queued future ranges"
        );
        assert!(
            task.in_flight.is_empty(),
            "rollback must reset the cross-peer in-flight reservation for rolled-back blocks"
        );

        // After the reset, the same blocks can be re-fetched (no stale-duplicate
        // suppression): re-running a tick re-dispatches them.
        task.evaluate_and_fetch().await;
        assert!(
            !drain_ranges(&mut peer_rx).is_empty(),
            "blocks must be re-fetchable after a rollback reset (no stale suppression)"
        );
    }
}
