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

use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};

use dugite_network::peer::governor::GovernorAction;
use dugite_network::BlockAnnouncement;
use dugite_network::RollbackAnnouncement;

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

/// Candidate chain state from a peer's ChainSync.
///
/// Updated by per-peer ChainSync tasks as they receive headers. Read by the
/// BlockFetch decision task to determine which blocks to fetch and from which
/// peers. This is the coordination point between ChainSync and BlockFetch,
/// matching the Haskell `FetchClientRegistry` / `FetchDecisionPolicy` pattern.
#[derive(Debug, Clone)]
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
    /// Highest slot that has been fetched or is being fetched.
    /// Used to skip duplicate fetches from other peers.
    max_fetched_slot: Arc<std::sync::atomic::AtomicU64>,

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
    peer_failure_tx: mpsc::Sender<SocketAddr>,

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

    /// Shared block provider for server protocols (ChainSync server, BlockFetch server).
    block_provider: Arc<ChainDBBlockProvider>,

    /// Broadcast sender for rollback announcements to ChainSync servers.
    rollback_announcement_tx: broadcast::Sender<RollbackAnnouncement>,

    /// Shared peer manager for PeerSharing server to query connected peers.
    peer_manager_for_servers: Arc<RwLock<NodePeerManager>>,

    /// Our N2N listen address. When set, outbound connections bind their
    /// source port to it (SO_REUSEADDR + SO_REUSEPORT) so a remote peer
    /// observes the connection as duplex-paired from our listen port —
    /// matching Haskell ouroboros-network's `configureOutboundSocket`.
    local_listen_addr: Option<SocketAddr>,
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
        byron_epoch_length: u64,
        security_param: u64,
        active_slots_coeff: f64,
        metrics: Arc<NodeMetrics>,
        mempool: Arc<Mempool>,
        peer_failure_tx: mpsc::Sender<SocketAddr>,
        keepalive_rtt_tx: mpsc::Sender<(SocketAddr, f64)>,
        gsm_event_tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>,
        block_provider: Arc<ChainDBBlockProvider>,
        rollback_announcement_tx: broadcast::Sender<RollbackAnnouncement>,
        peer_manager_for_servers: Arc<RwLock<NodePeerManager>>,
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
            byron_epoch_length,
            security_param,
            active_slots_coeff,
            active_fetcher: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            max_fetched_slot: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            metrics,
            mempool,
            peer_failure_tx,
            keepalive_rtt_tx,
            gsm_event_tx,
            block_provider,
            rollback_announcement_tx,
            peer_manager_for_servers,
            local_listen_addr: None,
        }
    }

    /// Set our N2N listen address used by outbound connections for
    /// duplex-paired source-port binding. Call once after construction.
    pub fn set_local_listen_addr(&mut self, addr: SocketAddr) {
        self.local_listen_addr = Some(addr);
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

    /// Demote a hot peer to warm: stop ChainSync + BlockFetch + TxSubmission2.
    ///
    /// This is the Hot -> Warm transition from Haskell's `PeerStateActions`.
    /// Only the hot protocol tasks are stopped; the mux and KeepAlive continue
    /// running. The peer can be re-promoted to hot later without reconnecting.
    ///
    /// # Errors
    ///
    /// Returns `LifecycleError::NotConnected` if no connection exists.
    pub async fn demote_to_warm(
        &mut self,
        addr: SocketAddr,
        peer_manager: &mut NodePeerManager,
    ) -> Result<(), LifecycleError> {
        // Hot protocols run on the outbound connection of a duplex pair.
        let cid = self
            .find_outbound_cid(addr)
            .or_else(|| self.find_any_cid(addr))
            .ok_or(LifecycleError::NotConnected(addr))?;

        let conn = self.connections.get_mut(&cid).unwrap();

        info!(%cid, "demoting hot -> warm: stopping sync protocols");

        conn.stop_hot_protocols().await;

        // Clear candidate chain state for this peer (no longer syncing).
        {
            let mut chains = self.candidate_chains.write().await;
            chains.remove(&addr);
        }

        // Update peer manager: hot -> warm.
        peer_manager.inner.demote_to_warm(&addr);

        // Update connection state: active → idle (outbound or inbound).
        if peer_manager.is_inbound(&addr) {
            peer_manager.mark_inbound_idle(&addr);
        } else {
            peer_manager.mark_outbound_idle(&addr);
        }

        info!(%cid, "hot -> warm complete");
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
                // PeerSharing active outreach — handled by the peer discovery
                // subsystem which owns the PeerSharingClient. The lifecycle
                // manager only logs the request.
                debug!(%addr, "governor requested PeerSharing outreach (handled externally)");
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
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

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
                        let _ = peer_failure_tx.try_send(addr);
                    }
                    Err(e) => debug!(%addr, "keepalive error: {e}"),
                }
            })
        })
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
        let byron_epoch_length = self.byron_epoch_length;
        let security_param = self.security_param;
        let active_slots_coeff = self.active_slots_coeff;
        let metrics = self.metrics.clone();
        let gsm_event_tx = self.gsm_event_tx.clone();

        Box::new(move |channel, cancel| {
            Box::pin(async move {
                info!(%addr, "chainsync task started");
                if let Err(e) = super::sync::chainsync_client_task(
                    channel,
                    addr,
                    candidate_chains,
                    chain_db,
                    ledger_state,
                    byron_epoch_length,
                    security_param,
                    active_slots_coeff,
                    metrics,
                    cancel,
                    gsm_event_tx,
                )
                .await
                {
                    warn!(%addr, error = %e, "chainsync task failed");
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
        let _max_fetched_slot = self.max_fetched_slot.clone();
        let metrics_clone = self.metrics.clone();
        let peer_failure_tx = self.peer_failure_tx.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
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

                info!(%addr, "blockfetch worker started (waiting for turn)");

                let mut poll_ticker = tokio::time::interval(std::time::Duration::from_millis(500));
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
                            let _ = active_fetcher.compare_exchange(
                                cancel_id,
                                0,
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                            );
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
                            let headers_to_fetch = {
                                let chains = candidate_chains.read().await;
                                let cdb = chain_db.read().await;
                                use dugite_primitives::hash::Hash32;
                                if let Some(state) = chains.get(&addr) {
                                    let filtered = select_headers_to_fetch(
                                        &state.pending_headers,
                                        |h| cdb.has_block(&Hash32::from_bytes(*h)),
                                        &fetched_hashes,
                                    );
                                    if filtered.is_empty() {
                                        active_fetcher.store(0, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    filtered
                                } else {
                                    active_fetcher.store(0, std::sync::atomic::Ordering::SeqCst);
                                    continue;
                                }
                            };

                            if headers_to_fetch.is_empty() {
                                continue;
                            }

                            info!(
                                %addr,
                                count = headers_to_fetch.len(),
                                first_slot = headers_to_fetch.first().map(|h| h.slot).unwrap_or(0),
                                last_slot = headers_to_fetch.last().map(|h| h.slot).unwrap_or(0),
                                "BlockFetch: active fetcher, downloading blocks",
                            );

                            // Batch headers into ranges for efficient fetching.
                            // A single MsgRequestRange(from, to) fetches all blocks
                            // between two points, avoiding per-block round-trips.
                            let ranges: Vec<(CodecPoint, CodecPoint)> = {
                                let mut result = Vec::new();
                                let mut i = 0;
                                while i < headers_to_fetch.len() {
                                    let start = i;
                                    // Batch up to 100 consecutive headers per range
                                    let end = (i + 100).min(headers_to_fetch.len()) - 1;
                                    let from = CodecPoint::Specific(
                                        headers_to_fetch[start].slot,
                                        headers_to_fetch[start].hash,
                                    );
                                    let to = CodecPoint::Specific(
                                        headers_to_fetch[end].slot,
                                        headers_to_fetch[end].hash,
                                    );
                                    result.push((from, to));
                                    i = end + 1;
                                }
                                result
                            };

                            debug!(%addr, ranges = ranges.len(), headers = headers_to_fetch.len(), "BlockFetch: fetching in batched ranges");
                            for (from, to) in ranges {
                                let peer = addr;
                                let range_to_slot = match &to {
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

                                let fetch_start = std::time::Instant::now();
                                let fetch_result = tokio::time::timeout(
                                    FETCH_RANGE_TIMEOUT,
                                    BlockFetchClient::fetch_range(
                                        &mut channel,
                                        from,
                                        to,
                                        |block_cbor| {
                                            match dugite_serialization::multi_era::decode_block_with_byron_epoch_length(
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
                                                }
                                                Err(e) => {
                                                    warn!(%addr, "block decode error: {e}");
                                                }
                                            }
                                            Ok(())
                                        },
                                    ),
                                ).await;
                                match fetch_result {
                                    Ok(Ok(count)) => {
                                        let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;
                                        metrics_clone.record_block_fetch_latency(fetch_ms);
                                        debug!(%addr, count, fetch_ms, "BlockFetch: range complete");
                                    }
                                    Ok(Err(e)) => {
                                        warn!(%addr, "BlockFetch error: {e}");
                                        active_fetcher.store(0, std::sync::atomic::Ordering::SeqCst);
                                        let _ = peer_failure_tx.try_send(addr);
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
                                        active_fetcher.store(0, std::sync::atomic::Ordering::SeqCst);
                                        let _ = peer_failure_tx.try_send(addr);
                                        return;
                                    }
                                }

                                // Send all blocks collected for this range using
                                // `.send().await` — which correctly yields to the
                                // scheduler instead of blocking the thread.
                                for fetched in decoded_blocks {
                                    let slot = fetched.block.slot().0;
                                    if let Err(e) = fetched_blocks_tx.send(fetched).await {
                                        warn!(%addr, slot, "send to run loop failed (channel closed): {e}");
                                        // Channel closed means the run loop exited.
                                        // Release the active fetcher and stop.
                                        active_fetcher.store(0, std::sync::atomic::Ordering::SeqCst);
                                        return;
                                    }
                                }
                            }

                            // Record all fetched hashes in the per-worker dedup set
                            // so subsequent iterations of this worker's loop skip
                            // them without consulting the candidate_chains lock.
                            for h in &headers_to_fetch {
                                fetched_hashes.insert(h.hash);
                            }

                            // Note: we do NOT update max_fetched_slot here.
                            // Per-worker dedup uses fetched_hashes (hash-based).
                            // Cross-worker dedup uses the applied ChainDB tip.
                            // max_fetched_slot caused sync stalls by jumping to
                            // the chain tip and filtering out all gap blocks.
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
                tokio::select! {
                    result = dugite_network::TxSubmissionClient::run(&mut channel, &source) => {
                        match result {
                            Ok(()) => debug!(%addr, "txsubmission2 client completed"),
                            Err(e) => debug!(%addr, "txsubmission2 client error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "txsubmission2 task cancelled");
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
                tokio::select! {
                    result = server.run(&mut channel, block_provider.as_ref(), announcement_rx, rollback_rx) => {
                        match result {
                            Ok(()) => debug!(%addr, "chainsync server completed"),
                            Err(e) => debug!(%addr, "chainsync server error: {e}"),
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!(%addr, "chainsync server cancelled");
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
    /// supported eras (Conway=6 through Shelley=2), and adds valid ones to
    /// the mempool. Tracks received/validated/rejected metrics.
    fn make_txsubmission_server_task(&self, addr: SocketAddr) -> ProtocolTaskFn {
        let mempool = self.mempool.clone();
        let metrics = self.metrics.clone();

        Box::new(move |mut channel, cancel| {
            Box::pin(async move {
                let on_tx = {
                    let tx_mempool = mempool;
                    let tx_metrics = metrics;
                    move |tx_hash: [u8; 32], tx_bytes: Vec<u8>| -> bool {
                        // Track every transaction received from peers in real-time.
                        tx_metrics
                            .transactions_received
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        // Best-effort mempool admission: try all supported eras for decoding.
                        let size_bytes = tx_bytes.len();
                        for era_id in [6u16, 5, 4, 3, 2] {
                            if let Ok(tx) =
                                dugite_serialization::decode_transaction(era_id, &tx_bytes)
                            {
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
            block_provider,
            rollback_announcement_tx,
            peer_manager_for_servers,
        )
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
            }],
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
            },
            // Already in ChainDB — must be skipped.
            PendingHeader {
                slot: 50,
                hash: [0x01; 32],
                header_cbor: vec![],
            },
            // Above applied_slot — must be fetched.
            PendingHeader {
                slot: 101,
                hash: [0x03; 32],
                header_cbor: vec![],
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
            }, // in-flight
            PendingHeader {
                slot: 11,
                hash: [0xBB; 32],
                header_cbor: vec![],
            }, // new
        ];

        let out = select_headers_to_fetch(&pending, |h| known.contains(h), &fetched);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hash, [0xBB; 32]);
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
            },
            PendingHeader {
                slot: 2,
                hash: [0xCD; 32],
                header_cbor: vec![],
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
            }],
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
}
