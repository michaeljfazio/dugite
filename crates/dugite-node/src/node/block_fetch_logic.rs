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

use std::collections::HashMap;
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
            decision_engine: BlockFetchDecision::with_defaults(),
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
        let mut dispatched: HashMap<SocketAddr, Vec<FetchRange>> = HashMap::new();

        while let Some((peer, range)) = self.decision_engine.select_peer(&peer_states) {
            dispatched.entry(peer).or_default().push(range);
        }

        // Send fetch requests to each peer's worker.
        //
        // In-flight tracking is only updated AFTER a successful dispatch.
        // If the peer's channel is full, the blocks are NOT marked as
        // in-flight, allowing them to be dispatched to a different peer
        // on the next decision tick.  Without this, a full channel would
        // lock the blocks in in-flight for 120 seconds with no actual
        // download happening.
        //
        // Per-peer chain tracking (issue #702): call `record_fetch_dispatched`
        // on the peer's `CandidateChainState` to set its status to Busy.  This
        // mirrors Haskell's `PeerFetchStatusBusy` transition in Decision.hs.
        let now = Instant::now();
        for (addr, peer_ranges) in dispatched {
            if let Some(sender) = self.fetch_senders.get(&addr) {
                let range_count = peer_ranges.len();
                match sender.try_send(peer_ranges.clone()) {
                    Ok(()) => {
                        debug!(%addr, range_count, "dispatched fetch ranges to peer");
                        // Mark ALL blocks in the successfully dispatched ranges
                        // as in-flight so they aren't re-dispatched on the next
                        // decision tick.
                        for range in &peer_ranges {
                            let from_slot = match &range.from {
                                Point::Specific(s, _) => *s,
                                Point::Origin => 0,
                            };
                            let to_slot = match &range.to {
                                Point::Specific(s, _) => *s,
                                Point::Origin => 0,
                            };
                            for (_, header) in &new_headers {
                                if header.slot >= from_slot && header.slot <= to_slot {
                                    self.in_flight.insert(header.hash, (addr, now));
                                }
                            }
                        }
                        // Transition peer to Busy in CandidateChainState.
                        {
                            let mut chains = self.candidate_chains.write().await;
                            if let Some(state) = chains.get_mut(&addr) {
                                state.record_fetch_dispatched();
                            }
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Channel full — do NOT mark as in-flight.  The blocks
                        // will be re-dispatched to a different peer on the next
                        // decision tick.
                        debug!(
                            %addr,
                            range_count,
                            "peer fetch channel full, will retry on another peer"
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        warn!(%addr, "peer fetch channel closed, deregistering");
                        self.fetch_senders.remove(&addr);
                    }
                }
            }
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
                            // Decode the block from raw CBOR. FULL decode —
                            // fetched blocks feed the ValidateAll apply
                            // pipeline whose phase-1/phase-2 oracle reads the
                            // witness set (#738); the minimal decoder is only
                            // safe for ApplyOnly replay.
                            match dugite_serialization::decode_block_with_byron_epoch_length(
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
            body_size: None,
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
}
