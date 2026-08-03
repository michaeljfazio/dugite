//! Single multiplexed connection to one Cardano peer.
//!
//! # Haskell Architecture Reference
//!
//! In the Haskell cardano-node, `ConnectionHandler` (ouroboros-network-framework) creates
//! exactly **one TCP connection per peer**. All Ouroboros mini-protocols share that single
//! multiplexed connection via `TemperatureBundle` in `Cardano.Network.NodeToNode`:
//!
//! - **ChainSync** (protocol 2) — block header synchronization
//! - **BlockFetch** (protocol 3) — full block download
//! - **TxSubmission2** (protocol 4) — transaction relay
//! - **KeepAlive** (protocol 8) — liveness probing
//! - **PeerSharing** (protocol 10) — peer address exchange
//!
//! Protocol tasks are started and stopped based on peer temperature transitions
//! (Cold -> Warm -> Hot) WITHOUT creating new TCP connections. The mux stays alive
//! across temperature changes; only the protocol tasks on top of it change.
//!
//! ## Temperature Lifecycle
//!
//! - **Cold -> Warm**: TCP connect + handshake, then start KeepAlive
//! - **Warm -> Hot**: Start ChainSync + BlockFetch + TxSubmission2 (channels already exist)
//! - **Hot -> Warm**: Stop hot protocol tasks, keep mux + KeepAlive alive
//! - **Warm -> Cold**: Stop KeepAlive, close mux + TCP connection
//!
//! This module provides `PeerConnection` — the struct that owns the single mux and
//! manages protocol channel lifecycle. The actual protocol logic (what ChainSync does
//! with blocks, etc.) is NOT in this file; external task functions receive the channels.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use dugite_network::handshake::n2n::N2NVersionData;
use dugite_network::handshake::{run_n2n_handshake_client, run_n2n_handshake_server};
use dugite_network::mux::channel::MuxChannel;
use dugite_network::mux::segment::Direction;
use dugite_network::mux::{Mux, MuxHandle};
use dugite_network::protocol::{
    PROTOCOL_HANDSHAKE, PROTOCOL_N2N_BLOCKFETCH, PROTOCOL_N2N_CHAINSYNC, PROTOCOL_N2N_KEEPALIVE,
    PROTOCOL_N2N_PEERSHARING, PROTOCOL_N2N_TXSUBMISSION,
};
use dugite_network::{MuxError, TcpBearer};

/// Ingress queue byte limit per protocol channel.
///
/// Matches Haskell's `Ouroboros.Network.Mux` `ingressQueueSize = 4 * 1024 * 1024`
/// (4 MiB).  The `bytes_in_flight` counter is decremented by `MuxChannel::recv()`
/// as data is consumed, so this limit correctly represents current buffered bytes —
/// not total bytes ever received.
///
/// Per-protocol byte limits are now derived from the Haskell
/// `network-mux` `maximumIngressQueue` formula (see
/// `cardano-diffusion/lib/Cardano/Network/NodeToNode.hs`):
/// `addSafetyMargin (pipelining * frame_size)`.  See the per-protocol
/// constants below; the default (used for handshake/keepalive/peersharing
/// where Haskell does not publish a specific limit) is conservative.
///
/// A-006 (security audit 2026-05-19): the prior 64MB limit enabled a slow-reader
/// attack where a peer could accumulate 640MB of ingress buffer before triggering
/// IngressQueueOverrun.  Per-protocol limits below cap this for slow-reader
/// scenarios while matching Haskell's published throughput targets.
const DEFAULT_INGRESS_LIMIT: usize = 4 * 1024 * 1024; // 4 MB (handshake/keepalive/peersharing)

/// ChainSync ingress queue limit — matches Haskell
/// `chainSyncProtocolLimits = addSafetyMargin (300 * 1400)` ≈ 462 KB.
/// Bumped to 512 KB for round-number alignment + small headroom.
const CHAINSYNC_INGRESS_LIMIT: usize = 512 * 1024;

/// BlockFetch ingress queue limit — Haskell's analogue is
/// `blockFetchProtocolLimits = max(10 * 2 MiB, 100 * 90112) * 1.1` ≈ 22 MB.
/// Sized (#747) to exceed the maximum pipelined in-flight budget
/// (`BLOCKFETCH_PIPELINE_WINDOW × BLOCKFETCH_RANGE_BYTE_BUDGET` in
/// `connection_lifecycle.rs` = 2 × 8 MB = 16 MB) with ~3× headroom: live
/// mainnet observation (2026-06-11) recorded transient queue peaks of
/// ~33.5 MB under estimate slack, and instrumentation in
/// `connection_lifecycle.rs` (actual-vs-estimated range bytes WARN) tracks
/// the residual gap. A compile-time assert in `connection_lifecycle.rs`
/// references THIS constant directly (pub(crate)) so the invariant cannot
/// drift.
pub(crate) const BLOCKFETCH_INGRESS_LIMIT: usize = 48 * 1024 * 1024;

/// TxSubmission ingress queue limit — matches Haskell
/// `txSubmissionProtocolLimits = addSafetyMargin (100 * 65540)` ≈ 7.2 MB.
const TXSUBMISSION_INGRESS_LIMIT: usize = 8 * 1024 * 1024;

/// Timeout for graceful protocol task shutdown (seconds).
///
/// Matches the Haskell `spsDeactivateTimeout` (5 seconds). If a protocol
/// task does not terminate within this window after cancellation, it is
/// forcibly aborted.
const PROTOCOL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Default TCP connect timeout.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A single multiplexed TCP connection to one Cardano peer.
///
/// Owns the mux task and provides protocol channels that can be taken
/// by protocol tasks when peer temperature changes. The mux stays alive
/// across Warm <-> Hot transitions; only the protocol tasks change.
///
/// # Channel Lifecycle
///
/// Channels are created during `connect()` / `accept()` and stored as
/// `Option<MuxChannel>`. When a protocol task starts, it takes the channel
/// (`Option::take`). When the task stops, the channel is consumed (mux
/// channels are not reusable after protocol completion — the mux handles
/// cleanup internally).
/// Aborts a spawned mux task unless explicitly disarmed (#924).
///
/// The mux task owns the `TcpBearer`, so simply dropping its `JoinHandle` on an
/// early return does **not** stop it — tokio detaches the task and it keeps the
/// socket open indefinitely. Every failed handshake therefore leaked a live
/// connection: an unauthenticated peer could send one malformed handshake per
/// socket and hold them all open. cardano-node closes the connection on every
/// refused handshake; dugite closed none of them.
///
/// Wrap the handle immediately after `tokio::spawn` and call [`Self::disarm`]
/// only once the connection is fully established and ownership moves into
/// `PeerConnection`.
struct MuxAbortGuard(Option<JoinHandle<Result<(), MuxError>>>);

impl MuxAbortGuard {
    fn new(handle: JoinHandle<Result<(), MuxError>>) -> Self {
        Self(Some(handle))
    }

    /// Take the handle back, cancelling the abort-on-drop behaviour.
    fn disarm(mut self) -> JoinHandle<Result<(), MuxError>> {
        self.0.take().expect("guard disarmed exactly once")
    }
}

impl Drop for MuxAbortGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

pub struct PeerConnection {
    /// Remote peer address.
    pub addr: SocketAddr,

    /// Local socket address (source endpoint of this connection).
    ///
    /// Together with `addr`, this forms the `ConnectionId` used by the
    /// lifecycle manager to distinguish concurrent connections to the same
    /// remote. For outbound connections this is the OS-chosen ephemeral
    /// source (or a bound listen-port when [`PeerConnection::connect`] was
    /// given a `local_listen_addr`). For inbound connections this is our
    /// listen address. Matches Haskell ouroboros-network's
    /// `ConnectionId { localAddress, remoteAddress }`.
    pub local_addr: SocketAddr,

    /// Whether we initiated this TCP connection (outbound) or accepted it (inbound).
    ///
    /// Determines which connection in a duplex pair runs initiator-side hot
    /// protocols (ChainSync client / BlockFetch client / TxSubmission2 client).
    /// In Haskell, the outbound side runs the client bundle; the inbound side
    /// only runs the responder bundle (servers).
    pub direction: PeerConnectionDirection,

    /// Negotiated N2N protocol version (14 or 15).
    pub version: u16,

    /// Cardano network magic (e.g. 2 for preview, 764824073 for mainnet).
    pub network_magic: u64,

    // ── Client protocol channels ──
    // Created during mux setup, taken when protocol tasks start.
    // `None` means the channel is currently in use by a running task.
    // For outbound connections: subscribed on InitiatorDir.
    // For inbound connections: subscribed on InitiatorDir (we act as client on initiator's direction).
    /// ChainSync client channel (protocol 2).
    pub(crate) chainsync_client_channel: Option<MuxChannel>,

    /// BlockFetch client channel (protocol 3).
    pub(crate) blockfetch_client_channel: Option<MuxChannel>,

    /// TxSubmission2 client channel (protocol 4).
    pub(crate) txsubmission_client_channel: Option<MuxChannel>,

    /// KeepAlive client channel (protocol 8).
    pub(crate) keepalive_client_channel: Option<MuxChannel>,

    /// PeerSharing client channel (protocol 10).
    ///
    /// In Haskell, the PeerSharing initiator is part of the `Established`
    /// mini-protocol bundle — it runs for the lifetime of the warm (or hotter)
    /// connection and loops on a per-peer mailbox waiting for governor-driven
    /// share requests.  In dugite we mirror this: when a connection is promoted
    /// to warm, `take_peersharing_client_channel()` hands this channel to the
    /// spawned `peersharing_client_task` which loops waiting for request amounts
    /// from `ConnectionLifecycleManager::peersharing_request_txs`.  The task
    /// terminates (and the channel is consumed) when the connection is torn down
    /// or the cancellation token fires.
    ///
    /// Reference:
    /// `ouroboros-network/lib/Ouroboros/Network/PeerSharing.hs` —
    /// `peerSharingClient` / `PeerSharingController`.
    // Allow dead-code under `feature = "test-utils"` — the field is read by
    // the binary's `connection_lifecycle` module, which is not exposed via
    // the lib crate when `test-utils` re-exports `peer_connection`.
    #[allow(dead_code)]
    pub(crate) peersharing_client_channel: Option<MuxChannel>,

    // ── Server protocol channels ──
    // Always populated on both outbound and inbound connections.
    // For outbound connections: subscribed on ResponderDir (remote initiates, we respond).
    // For inbound connections: subscribed on ResponderDir (remote initiates, we respond).
    /// ChainSync server channel (protocol 2, ResponderDir).
    pub(crate) chainsync_server_channel: Option<MuxChannel>,

    /// BlockFetch server channel (protocol 3, ResponderDir).
    pub(crate) blockfetch_server_channel: Option<MuxChannel>,

    /// TxSubmission2 server channel (protocol 4, ResponderDir).
    pub(crate) txsubmission_server_channel: Option<MuxChannel>,

    /// KeepAlive server channel (protocol 8, ResponderDir).
    pub(crate) keepalive_server_channel: Option<MuxChannel>,

    /// PeerSharing server channel (protocol 10, ResponderDir).
    pub(crate) peersharing_server_channel: Option<MuxChannel>,

    // ── Mux lifecycle ──
    /// Handle to the spawned mux task. When this completes, the connection is dead.
    mux_handle: JoinHandle<Result<(), MuxError>>,

    /// Re-subscription handle for Hot→Warm demotion without TCP close.
    ///
    /// Populated at connect/accept time. Used by `stop_hot_protocols_and_recover()` to
    /// create fresh `MuxChannel` instances after hot protocol tasks have exited,
    /// mirroring Haskell's `deactivatePeerConnection` which keeps the mux alive.
    ///
    /// Reference: <https://github.com/IntersectMBO/ouroboros-network/blob/main/ouroboros-network/lib/Ouroboros/Network/PeerSelection/PeerStateActions.hs#L978>
    mux_resubscribe: MuxHandle,

    /// Top-level cancellation token for the entire connection.
    cancel: CancellationToken,

    // ── Running protocol task handles ──
    /// Warm-temperature protocol tasks (currently: KeepAlive).
    warm_tasks: Vec<(JoinHandle<()>, CancellationToken)>,

    /// Hot-temperature protocol tasks (ChainSync, BlockFetch, TxSubmission2).
    hot_tasks: Vec<(JoinHandle<()>, CancellationToken)>,

    /// Server protocol tasks (ChainSync, BlockFetch, TxSubmission2, KeepAlive, PeerSharing responders).
    /// Always populated — server protocols run on all connections so remote peers can pull blocks.
    server_tasks: Vec<(JoinHandle<()>, CancellationToken)>,
}

/// Direction of a peer connection at the TCP level.
///
/// Mirrors `Ouroboros.Network.ConnectionManager.Types.Provenance` and is used
/// by the lifecycle manager to pick the correct connection for client-side
/// hot-protocol promotion when both an outbound and an inbound connection
/// exist to the same remote (duplex peer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerConnectionDirection {
    /// We dialed the peer (TCP `connect`).
    Outbound,
    /// The peer dialed us (TCP `accept`).
    Inbound,
}

/// A boxed future type for protocol task factories.
///
/// Protocol task factories are async closures that receive a `MuxChannel`
/// and a `CancellationToken`, and run the protocol until cancelled or
/// the channel closes. Used by `start_warm_protocols` and `start_hot_protocols`.
pub type ProtocolTaskFn = Box<
    dyn FnOnce(MuxChannel, CancellationToken) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send,
>;

/// How a server-side (responder) mini-protocol instance ended.
///
/// Upstream `ouroboros-network` has exactly two terminal outcomes for a
/// responder, and dugite must reproduce both — see [`ServerProtocolTaskFn`].
#[derive(Debug)]
pub enum ServerProtocolOutcome {
    /// The remote ended this mini-protocol normally — its `MsgDone`.
    ///
    /// This is NOT the end of the connection. cardano-node sends ChainSync
    /// `MsgDone` on every Hot->Warm demotion (`deactivatePeerConnection`) and
    /// starts a fresh ChainSync session on the SAME bearer when it re-promotes.
    ClientDone,
    /// The task was cancelled through its token — the connection is being torn
    /// down by us, so there is nothing to re-arm.
    Cancelled,
    /// A protocol error. Fatal to the whole connection, not just this route.
    Failed(String),
}

/// A RESTARTABLE server-side protocol task factory.
///
/// `Fn`, not `FnOnce`: a responder must be re-runnable on the same connection.
/// That is the whole point — see [`PeerConnection::start_server_protocols`].
pub type ServerProtocolTaskFn = Arc<
    dyn Fn(
            MuxChannel,
            CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ServerProtocolOutcome> + Send>>
        + Send
        + Sync,
>;

/// More than this many responder restarts inside
/// [`SERVER_RESTART_WINDOW`] is treated as abuse and kills the connection.
///
/// Legitimate Hot->Warm->Hot churn runs at roughly one demotion per tens of
/// seconds. A peer that can make us re-arm a route hundreds of times a second
/// is spamming `MsgDone`, and each re-arm allocates a fresh ingress channel.
/// Upstream needs no such guard because its re-arm is lazy (`StartOnDemand`
/// costs nothing until a byte arrives); dugite's spawns a task, so it does.
const SERVER_RESTART_BURST: u32 = 64;

/// Window over which [`SERVER_RESTART_BURST`] is measured.
const SERVER_RESTART_WINDOW: Duration = Duration::from_secs(10);

impl PeerConnection {
    /// Returns `true` if this connection has server-side (responder) channels.
    ///
    /// Server channels are always present on both outbound and inbound connections.
    /// This check exists for defensive correctness — it should always return true.
    pub fn has_server_channels(&self) -> bool {
        self.chainsync_server_channel.is_some()
    }

    /// Establish an outbound connection to a peer.
    ///
    /// Performs TCP connect (with timeout), creates the mux, subscribes all
    /// protocol channels on `InitiatorDir`, spawns the mux task, and runs
    /// the N2N handshake. Returns a `PeerConnection` with channels ready
    /// for protocol tasks.
    ///
    /// # Arguments
    ///
    /// * `addr` — Remote peer socket address
    /// * `network_magic` — Cardano network identifier
    /// * `initiator_only` — True when DiffusionMode is InitiatorOnly
    /// * `peer_sharing` — Whether to advertise peer sharing support
    /// * `timeout` — Optional TCP connect timeout (defaults to 10s)
    ///
    /// # Errors
    ///
    /// Returns `NetworkError` on TCP connect failure, mux error, or
    /// handshake failure (version mismatch, network magic mismatch, etc.).
    pub async fn connect(
        addr: SocketAddr,
        network_magic: u64,
        initiator_only: bool,
        peer_sharing: bool,
        timeout: Option<Duration>,
        local_listen_addr: Option<SocketAddr>,
    ) -> Result<Self, PeerConnectionError> {
        let connect_timeout = timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT);

        // TCP connect with timeout. When the caller provides our N2N listen
        // address, we bind the outbound source port to it so remote peers see
        // a duplex-paired connection from (our_ip, our_listen_port) — matching
        // Haskell ouroboros-network's `configureOutboundSocket` convention.
        let bearer_fut = async {
            match local_listen_addr {
                Some(local) => TcpBearer::connect_from(addr, local).await,
                None => TcpBearer::connect(addr).await,
            }
        };
        let bearer = tokio::time::timeout(connect_timeout, bearer_fut)
            .await
            .map_err(|_| PeerConnectionError::ConnectTimeout(addr))?
            .map_err(|e| PeerConnectionError::Connect(addr, e.to_string()))?;

        // Capture the OS-assigned (or REUSEPORT-bound) local source address
        // before the bearer moves into the mux. Used to build `ConnectionId`
        // — matches Haskell `Ouroboros.Network.ConnectionId` keying.
        let local_addr = bearer.local_addr().unwrap_or_else(|_| match addr {
            SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
            SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
        });

        info!(%addr, %local_addr, "TCP connected, starting mux + handshake");

        // Create mux (we are initiator).
        let mut mux = Mux::new(bearer, true);

        // Subscribe handshake channel (protocol 0) — consumed during handshake.
        let mut handshake_ch = mux.subscribe(
            PROTOCOL_HANDSHAKE,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
        );

        // Subscribe all N2N client protocol channels on InitiatorDir.
        // For outbound connections, we are the TCP initiator so our client
        // protocols use InitiatorDir.
        let chainsync_client_ch = mux.subscribe(
            PROTOCOL_N2N_CHAINSYNC,
            Direction::InitiatorDir,
            CHAINSYNC_INGRESS_LIMIT,
        );
        let blockfetch_client_ch = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::InitiatorDir,
            BLOCKFETCH_INGRESS_LIMIT,
        );
        let txsubmission_client_ch = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::InitiatorDir,
            TXSUBMISSION_INGRESS_LIMIT,
        );
        let keepalive_client_ch = mux.subscribe(
            PROTOCOL_N2N_KEEPALIVE,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let peersharing_client_ch = mux.subscribe(
            PROTOCOL_N2N_PEERSHARING,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
        );

        // Always subscribe ResponderDir channels for server protocols, regardless
        // of initiator_only. The mux flips direction on ingress: remote's InitiatorDir
        // messages arrive on our ResponderDir. The remote peer needs to pull blocks
        // from us via ChainSync/BlockFetch on this connection.
        //
        // InitiatorOnly only controls whether the node opens a TCP listener — it does
        // NOT prevent server protocols from running on outbound connections. This
        // matches Haskell's behavior where a BP with InitiatorOnly still serves
        // blocks to its relay over the outbound connection.
        let cs_srv = mux.subscribe(
            PROTOCOL_N2N_CHAINSYNC,
            Direction::ResponderDir,
            CHAINSYNC_INGRESS_LIMIT,
        );
        let bf_srv = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::ResponderDir,
            BLOCKFETCH_INGRESS_LIMIT,
        );
        let tx_srv = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::ResponderDir,
            TXSUBMISSION_INGRESS_LIMIT,
        );
        let ka_srv = mux.subscribe(
            PROTOCOL_N2N_KEEPALIVE,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let ps_srv = mux.subscribe(
            PROTOCOL_N2N_PEERSHARING,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );

        // Take the re-subscription handle BEFORE consuming the mux into run().
        // This must happen after all subscribe() calls and before spawning run().
        let mux_resubscribe = mux.take_handle();

        // Spawn mux task — runs until bearer closes or error.
        // Issue #747: log WARN on unexpected mux exit so ingress-overflow and
        // other mux-level failures are not silently swallowed (they previously
        // produced no log output — the protocol tasks simply saw closed channels
        // and reported generic "send failed" errors, making root-cause analysis
        // very difficult).
        let cancel = CancellationToken::new();
        let mux_addr_for_log = addr;
        let mux_handle = tokio::spawn(async move {
            let result = mux.run().await;
            match &result {
                Ok(()) => debug!(%mux_addr_for_log, "mux exited cleanly"),
                Err(e) => warn!(%mux_addr_for_log, error = %e, "mux exited with error (#747)"),
            }
            result
        });

        // Abort the mux task (and so close the socket) if the handshake below
        // fails — see `MuxAbortGuard` (#924).
        let mux_guard = MuxAbortGuard::new(mux_handle);

        // Run N2N handshake on the handshake channel.
        let our_data = N2NVersionData::new(network_magic, initiator_only, peer_sharing);
        let handshake_result = run_n2n_handshake_client(&mut handshake_ch, &our_data)
            .await
            .map_err(|e| PeerConnectionError::Handshake(addr, e.to_string()))?;

        let version = handshake_result.version;
        info!(%addr, version, "N2N handshake complete");
        let mux_handle = mux_guard.disarm();

        Ok(Self {
            addr,
            local_addr,
            direction: PeerConnectionDirection::Outbound,
            version,
            network_magic,
            chainsync_client_channel: Some(chainsync_client_ch),
            blockfetch_client_channel: Some(blockfetch_client_ch),
            txsubmission_client_channel: Some(txsubmission_client_ch),
            keepalive_client_channel: Some(keepalive_client_ch),
            peersharing_client_channel: Some(peersharing_client_ch),
            chainsync_server_channel: Some(cs_srv),
            blockfetch_server_channel: Some(bf_srv),
            txsubmission_server_channel: Some(tx_srv),
            keepalive_server_channel: Some(ka_srv),
            peersharing_server_channel: Some(ps_srv),
            mux_handle,
            mux_resubscribe,
            cancel,
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        })
    }

    /// Accept an inbound connection from a peer.
    ///
    /// Creates the mux from an already-accepted `TcpStream`, subscribes all
    /// protocol channels on `ResponderDir`, spawns the mux task, and runs
    /// the N2N handshake as server. Returns a `PeerConnection` with channels
    /// ready for protocol tasks.
    ///
    /// # Arguments
    ///
    /// * `stream` — Already-accepted TCP stream
    /// * `addr` — Remote peer socket address (for logging/identification)
    /// * `network_magic` — Cardano network identifier
    /// * `initiator_only` — True when DiffusionMode is InitiatorOnly
    /// * `peer_sharing` — Whether to advertise peer sharing support
    pub async fn accept(
        stream: tokio::net::TcpStream,
        addr: SocketAddr,
        network_magic: u64,
        initiator_only: bool,
        peer_sharing: bool,
    ) -> Result<Self, PeerConnectionError> {
        // Capture the local (listen-side) address from the accepted socket
        // before it moves into the bearer. Pairs with `addr` (peer) to form
        // the `ConnectionId`. Matches Haskell `Ouroboros.Network.Server.Sock`
        // which records `localAddress` from the accepted socket.
        let local_addr = stream.local_addr().unwrap_or_else(|_| match addr {
            SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
            SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
        });

        let bearer = TcpBearer::new(stream)
            .map_err(|e| PeerConnectionError::Connect(addr, e.to_string()))?;

        info!(%addr, %local_addr, "accepted inbound connection, starting mux + handshake");

        // Create mux (we are responder).
        let mut mux = Mux::new(bearer, false);

        // Subscribe handshake channel on ResponderDir.
        let mut handshake_ch = mux.subscribe(
            PROTOCOL_HANDSHAKE,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );

        // Subscribe all N2N server protocol channels on ResponderDir.
        // For inbound connections, we are the TCP responder. The remote peer's
        // InitiatorDir messages arrive on our ResponderDir, so our server
        // protocols use ResponderDir.
        let chainsync_server_ch = mux.subscribe(
            PROTOCOL_N2N_CHAINSYNC,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let blockfetch_server_ch = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let txsubmission_server_ch = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let keepalive_server_ch = mux.subscribe(
            PROTOCOL_N2N_KEEPALIVE,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let peersharing_server_ch = mux.subscribe(
            PROTOCOL_N2N_PEERSHARING,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );

        // In duplex mode, also subscribe InitiatorDir channels for client protocols.
        // For inbound connections in duplex mode, we can also act as client by
        // sending on InitiatorDir. The mux flips direction on egress so our
        // InitiatorDir messages reach the remote's ResponderDir.
        let (cs_cli, bf_cli, tx_cli, ka_cli, ps_cli) = if !initiator_only {
            let cs = mux.subscribe(
                PROTOCOL_N2N_CHAINSYNC,
                Direction::InitiatorDir,
                DEFAULT_INGRESS_LIMIT,
            );
            let bf = mux.subscribe(
                PROTOCOL_N2N_BLOCKFETCH,
                Direction::InitiatorDir,
                DEFAULT_INGRESS_LIMIT,
            );
            let tx = mux.subscribe(
                PROTOCOL_N2N_TXSUBMISSION,
                Direction::InitiatorDir,
                DEFAULT_INGRESS_LIMIT,
            );
            let ka = mux.subscribe(
                PROTOCOL_N2N_KEEPALIVE,
                Direction::InitiatorDir,
                DEFAULT_INGRESS_LIMIT,
            );
            let ps = mux.subscribe(
                PROTOCOL_N2N_PEERSHARING,
                Direction::InitiatorDir,
                DEFAULT_INGRESS_LIMIT,
            );
            (Some(cs), Some(bf), Some(tx), Some(ka), Some(ps))
        } else {
            (None, None, None, None, None)
        };

        // Take the re-subscription handle BEFORE consuming the mux into run().
        let mux_resubscribe = mux.take_handle();

        // Spawn mux task.
        // Issue #747: log WARN on unexpected mux exit (same as outbound path).
        let cancel = CancellationToken::new();
        let mux_addr_for_log = addr;
        let mux_handle = tokio::spawn(async move {
            let result = mux.run().await;
            match &result {
                Ok(()) => debug!(%mux_addr_for_log, "mux exited cleanly"),
                Err(e) => warn!(%mux_addr_for_log, error = %e, "mux exited with error (#747)"),
            }
            result
        });

        // Abort the mux task (and so close the socket) on any early return
        // below — see `MuxAbortGuard` (#924). This is the inbound path, so a
        // leak here is remotely triggerable by an unauthenticated peer.
        let mux_guard = MuxAbortGuard::new(mux_handle);

        // Run N2N handshake as server.
        let our_data = N2NVersionData::new(network_magic, initiator_only, peer_sharing);
        let handshake_result = run_n2n_handshake_server(&mut handshake_ch, &our_data)
            .await
            .map_err(|e| PeerConnectionError::Handshake(addr, e.to_string()))?;

        // #880: a query-mode handshake enumerated our versions (we already sent
        // MsgQueryReply) and the peer closes — no mini-protocols run on it, so
        // drop the connection instead of spinning up protocol tasks against a
        // socket the peer is closing.
        if handshake_result.query {
            info!(%addr, "N2N query-mode handshake — closing connection (no protocols)");
            return Err(PeerConnectionError::Handshake(
                addr,
                "query-mode connection (version enumeration only)".to_string(),
            ));
        }

        let version = handshake_result.version;
        info!(%addr, version, "N2N handshake complete (inbound)");
        let mux_handle = mux_guard.disarm();

        Ok(Self {
            addr,
            local_addr,
            direction: PeerConnectionDirection::Inbound,
            version,
            network_magic,
            chainsync_client_channel: cs_cli,
            blockfetch_client_channel: bf_cli,
            txsubmission_client_channel: tx_cli,
            keepalive_client_channel: ka_cli,
            peersharing_client_channel: ps_cli,
            chainsync_server_channel: Some(chainsync_server_ch),
            blockfetch_server_channel: Some(blockfetch_server_ch),
            txsubmission_server_channel: Some(txsubmission_server_ch),
            keepalive_server_channel: Some(keepalive_server_ch),
            peersharing_server_channel: Some(peersharing_server_ch),
            mux_handle,
            mux_resubscribe,
            cancel,
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        })
    }

    /// Start warm-temperature protocols (KeepAlive).
    ///
    /// Takes the `keepalive_channel` and spawns a protocol task using the
    /// provided factory function. The factory receives the channel and a
    /// cancellation token, and should run the KeepAlive protocol until
    /// cancelled.
    ///
    /// # Panics
    ///
    /// Returns `Err` if the keepalive channel has already been taken
    /// (protocols already running).
    pub fn start_warm_protocols(
        &mut self,
        keepalive_fn: ProtocolTaskFn,
    ) -> Result<(), PeerConnectionError> {
        let ch = self
            .keepalive_client_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("keepalive"))?;

        let token = self.cancel.child_token();
        let token_clone = token.clone();

        let handle = tokio::spawn(async move {
            (keepalive_fn)(ch, token_clone).await;
        });

        self.warm_tasks.push((handle, token));
        debug!(addr = %self.addr, "started warm protocols (KeepAlive)");
        Ok(())
    }

    /// Start hot-temperature protocols (ChainSync, BlockFetch, TxSubmission2).
    ///
    /// Takes the corresponding channels and spawns each protocol as an
    /// independent tokio task using the provided factory functions. Each
    /// factory receives its channel and a cancellation token.
    ///
    /// The actual protocol logic (block processing, tx relay, etc.) is
    /// provided by the caller — this struct only manages the lifecycle.
    ///
    /// # Arguments
    ///
    /// * `chainsync_fn` — Factory for the ChainSync protocol task
    /// * `blockfetch_fn` — Factory for the BlockFetch protocol task
    /// * `txsubmission_fn` — Factory for the TxSubmission2 protocol task
    pub fn start_hot_protocols(
        &mut self,
        chainsync_fn: ProtocolTaskFn,
        blockfetch_fn: ProtocolTaskFn,
        txsubmission_fn: ProtocolTaskFn,
    ) -> Result<(), PeerConnectionError> {
        let cs_ch = self
            .chainsync_client_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("chainsync"))?;
        let bf_ch = self
            .blockfetch_client_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("blockfetch"))?;
        let tx_ch = self
            .txsubmission_client_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("txsubmission"))?;

        // Spawn ChainSync task.
        let cs_token = self.cancel.child_token();
        let cs_token_clone = cs_token.clone();
        let cs_handle = tokio::spawn(async move {
            (chainsync_fn)(cs_ch, cs_token_clone).await;
        });
        self.hot_tasks.push((cs_handle, cs_token));

        // Spawn BlockFetch task.
        let bf_token = self.cancel.child_token();
        let bf_token_clone = bf_token.clone();
        let bf_handle = tokio::spawn(async move {
            (blockfetch_fn)(bf_ch, bf_token_clone).await;
        });
        self.hot_tasks.push((bf_handle, bf_token));

        // Spawn TxSubmission2 task.
        let tx_token = self.cancel.child_token();
        let tx_token_clone = tx_token.clone();
        let tx_handle = tokio::spawn(async move {
            (txsubmission_fn)(tx_ch, tx_token_clone).await;
        });
        self.hot_tasks.push((tx_handle, tx_token));

        debug!(addr = %self.addr, "started hot protocols (ChainSync, BlockFetch, TxSubmission2)");
        Ok(())
    }

    /// Stop hot-temperature protocol tasks.
    ///
    /// Cancels all hot protocol tasks via their individual cancellation tokens
    /// and waits up to [`PROTOCOL_SHUTDOWN_TIMEOUT`] (5 seconds, matching
    /// Haskell `spsDeactivateTimeout`) for graceful shutdown. Any tasks that
    /// do not finish in time are forcibly aborted.
    pub async fn stop_hot_protocols(&mut self) {
        Self::stop_tasks(&mut self.hot_tasks, "hot", self.addr).await;
    }

    /// Stop hot-temperature protocol tasks and recover their channels.
    ///
    /// Mirrors Haskell's `deactivatePeerConnection` (link below): cancels the
    /// hot mini-protocols via their cancellation tokens, awaits graceful exit
    /// with `spsDeactivateTimeout = 5s`, then uses `MuxHandle::resubscribe()`
    /// to atomically install fresh ingress receivers — keeping TCP+Mux alive.
    ///
    /// On success, `chainsync_client_channel`, `blockfetch_client_channel`, and
    /// `txsubmission_client_channel` are `Some` again and `start_hot_protocols`
    /// can be called immediately on the NEXT governor `PromoteToHot`.
    ///
    /// On timeout (task did not stop within 5 s), the task is forcibly aborted
    /// and the channel for that slot is left as `None` — the caller must fall
    /// back to full TCP close (matching Haskell's `Mux.stop pchMux` fallback).
    ///
    /// Reference:
    /// <https://github.com/IntersectMBO/ouroboros-network/blob/main/ouroboros-network/lib/Ouroboros/Network/PeerSelection/PeerStateActions.hs#L978>
    pub async fn stop_hot_protocols_and_recover(&mut self) -> bool {
        // Signal cancellation to all hot tasks.
        for (_, token) in &self.hot_tasks {
            token.cancel();
        }

        // Await graceful shutdown with spsDeactivateTimeout (5 s).
        // Must await each task individually so we can detect timeouts.
        let mut all_stopped = true;
        let drain = std::mem::take(&mut self.hot_tasks);
        for (handle, _) in drain {
            let abort_handle = handle.abort_handle();
            match tokio::time::timeout(PROTOCOL_SHUTDOWN_TIMEOUT, handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    warn!(addr = %self.addr, error = %e, "hot protocol task join error during deactivation");
                }
                Err(_) => {
                    warn!(
                        addr = %self.addr,
                        "hot protocol task timed out during deactivation (spsDeactivateTimeout 5s), aborting"
                    );
                    abort_handle.abort();
                    all_stopped = false;
                }
            }
        }

        if !all_stopped {
            // Haskell fallback: if any task refused to stop, the connection is
            // considered corrupt — caller must do a full TCP close.
            warn!(addr = %self.addr, "hot protocol tasks did not stop cleanly — channel recovery skipped");
            return false;
        }

        // Re-subscribe hot client channels via the running mux's swappable senders.
        // This atomically installs new ingress receivers without touching TCP.
        // Both outbound and inbound-duplex connections use InitiatorDir for client
        // protocols (ChainSync/BlockFetch/TxSubmission are always client-side).
        let dir = Direction::InitiatorDir;

        let cs_ch = self
            .mux_resubscribe
            .resubscribe(PROTOCOL_N2N_CHAINSYNC, dir);
        let bf_ch = self
            .mux_resubscribe
            .resubscribe(PROTOCOL_N2N_BLOCKFETCH, dir);
        let tx_ch = self
            .mux_resubscribe
            .resubscribe(PROTOCOL_N2N_TXSUBMISSION, dir);

        match (cs_ch, bf_ch, tx_ch) {
            (Some(cs), Some(bf), Some(tx)) => {
                self.chainsync_client_channel = Some(cs);
                self.blockfetch_client_channel = Some(bf);
                self.txsubmission_client_channel = Some(tx);
                debug!(addr = %self.addr, "hot protocol channels recovered via mux resubscribe");
                true
            }
            _ => {
                // Defensive: channels not in the handle (e.g. inbound-only connection
                // without client subscriptions). Caller falls back to TCP close.
                warn!(addr = %self.addr, "mux resubscribe returned None for hot channels — falling back to TCP close");
                false
            }
        }
    }

    /// Stop warm-temperature protocol tasks (KeepAlive).
    ///
    /// Same graceful-then-abort pattern as [`stop_hot_protocols`].
    pub async fn stop_warm_protocols(&mut self) {
        Self::stop_tasks(&mut self.warm_tasks, "warm", self.addr).await;
    }

    /// Start all five server-side (responder) protocol tasks on this
    /// connection, each under a supervisor that reproduces upstream's
    /// responder-termination policy (#980).
    ///
    /// Server protocols run the responder side: they wait for requests from
    /// the remote peer's client protocols and respond accordingly. In duplex
    /// mode, both client and server protocols run simultaneously on the same
    /// multiplexed connection.
    ///
    /// # Why a supervisor at all
    ///
    /// These tasks used to be spawned bare: whatever the protocol returned was
    /// logged and the task exited. Nothing observed the handles. The mux, the
    /// bearer and the TCP socket all stayed up, and
    /// [`crate::node::peer_connection`]'s ingress task silently discards frames
    /// for a route whose receiver has been dropped. So a downstream peer kept a
    /// live connection on which one mini-protocol answered nothing, forever,
    /// with no error and no disconnect — issue #980's exact fingerprint,
    /// including "restarting the downstream fixes it" (a new connection gets a
    /// new task).
    ///
    /// The trigger is ordinary, not exotic: cardano-node sends ChainSync
    /// `MsgDone` on every Hot->Warm demotion, dugite's server returned
    /// `Ok(())`, and the route was gone for the life of the connection. Under
    /// load the peer governor churns more, so it reproduced as "load-dependent".
    ///
    /// # The policy, from ouroboros-network
    ///
    /// `network-mux/src/Network/Mux.hs` on an exception in a mini-protocol:
    ///
    /// ```text
    /// | MiniProtocolException MiniProtocolNum MiniProtocolDir SomeException
    ///   -- ^ A mini-protocol thread terminated with an exception. We always
    ///   -- respond by terminating the whole mux.
    /// ```
    ///
    /// and `Ouroboros/Network/InboundGovernor.hs` on a clean termination:
    ///
    /// ```text
    /// MiniProtocolTerminated Terminated { tConnId, tMux, tMiniProtocolData = mpd, tResult } -> do
    ///   case tResult' of
    ///     Left e  -> ...  -- mux will shutdown, connection manager tears down the socket
    ///     Right _ -> runResponder tMux mpd >>= ...  -- TrResponderRestarted
    /// ```
    ///
    /// So: **error => kill the connection, clean exit => re-arm the route.**
    /// Upstream cannot even express the third state dugite was in — its
    /// responders are typed so that returning mid-protocol is not constructible,
    /// and an orphaned ingress queue eventually overruns and kills the mux
    /// anyway.
    ///
    /// Re-arming uses [`MuxHandle::resubscribe`], the same mechanism already
    /// used for initiator-side Hot->Warm recovery; only the responder half was
    /// missing.
    pub fn start_server_protocols(
        &mut self,
        chainsync_server_fn: ServerProtocolTaskFn,
        blockfetch_server_fn: ServerProtocolTaskFn,
        txsubmission_server_fn: ServerProtocolTaskFn,
        keepalive_server_fn: ServerProtocolTaskFn,
        peersharing_server_fn: ServerProtocolTaskFn,
    ) -> Result<(), PeerConnectionError> {
        let specs: [(&'static str, u16, ServerProtocolTaskFn, Option<MuxChannel>); 5] = [
            (
                "chainsync",
                PROTOCOL_N2N_CHAINSYNC,
                chainsync_server_fn,
                self.chainsync_server_channel.take(),
            ),
            (
                "blockfetch",
                PROTOCOL_N2N_BLOCKFETCH,
                blockfetch_server_fn,
                self.blockfetch_server_channel.take(),
            ),
            (
                "txsubmission",
                PROTOCOL_N2N_TXSUBMISSION,
                txsubmission_server_fn,
                self.txsubmission_server_channel.take(),
            ),
            (
                "keepalive",
                PROTOCOL_N2N_KEEPALIVE,
                keepalive_server_fn,
                self.keepalive_server_channel.take(),
            ),
            (
                "peersharing",
                PROTOCOL_N2N_PEERSHARING,
                peersharing_server_fn,
                self.peersharing_server_channel.take(),
            ),
        ];

        for (label, protocol_id, factory, channel) in specs {
            let channel = channel.ok_or(PeerConnectionError::ChannelUnavailable(label))?;
            let task_token = self.cancel.child_token();
            let handle = Self::spawn_server_supervisor(
                label,
                protocol_id,
                factory,
                channel,
                self.mux_resubscribe.clone(),
                task_token.clone(),
                self.cancel.clone(),
                self.mux_handle.abort_handle(),
                self.addr,
            );
            self.server_tasks.push((handle, task_token));
        }

        debug!(addr = %self.addr, "started server protocols (ChainSync, BlockFetch, TxSubmission2, KeepAlive, PeerSharing)");
        Ok(())
    }

    /// Run one responder to termination, then apply the upstream policy.
    ///
    /// Tearing down means BOTH `conn_cancel.cancel()` and `mux_abort.abort()`.
    /// The token alone is not enough and would have been a silent no-op for the
    /// purpose at hand: nothing in the node aborts the mux when the connection
    /// token fires — `shutdown()` cancels the token and aborts `mux_handle` as
    /// two separate steps, and `is_alive()` reads only the mux handle. Cancel
    /// on its own would therefore stop the five protocol tasks while leaving
    /// the bearer and TCP socket open and the connection still "alive" to the
    /// reaper, which is a strictly worse version of the silence this whole fix
    /// exists to remove.
    ///
    /// Aborting the mux drops the bearer, closes the socket, and lets the
    /// downstream peer observe a disconnect and reconnect — the same
    /// observable outcome as upstream's mux dying on a `MiniProtocolException`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_server_supervisor(
        label: &'static str,
        protocol_id: u16,
        factory: ServerProtocolTaskFn,
        initial_channel: MuxChannel,
        mux_resubscribe: MuxHandle,
        task_token: CancellationToken,
        conn_cancel: CancellationToken,
        mux_abort: tokio::task::AbortHandle,
        addr: SocketAddr,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut channel = initial_channel;
            let mut window_start = Instant::now();
            let mut restarts_in_window: u32 = 0;

            loop {
                match (factory)(channel, task_token.clone()).await {
                    ServerProtocolOutcome::Cancelled => {
                        debug!(%addr, protocol = label, "server protocol cancelled");
                        return;
                    }
                    ServerProtocolOutcome::Failed(reason) => {
                        // Haskell: "We always respond by terminating the whole
                        // mux." Reject loudly beats going quiet — a peer that
                        // sees a disconnect reconnects; a peer that sees
                        // silence waits forever.
                        warn!(
                            %addr,
                            protocol = label,
                            reason = %reason,
                            "server protocol failed — tearing down the connection \
                             (a mini-protocol error is fatal to the whole mux upstream)"
                        );
                        conn_cancel.cancel();
                        mux_abort.abort();
                        return;
                    }
                    ServerProtocolOutcome::ClientDone => {
                        if task_token.is_cancelled() {
                            return;
                        }
                        // Rate guard — see SERVER_RESTART_BURST.
                        if window_start.elapsed() > SERVER_RESTART_WINDOW {
                            window_start = Instant::now();
                            restarts_in_window = 0;
                        }
                        restarts_in_window += 1;
                        if restarts_in_window > SERVER_RESTART_BURST {
                            warn!(
                                %addr,
                                protocol = label,
                                restarts = restarts_in_window,
                                window_secs = SERVER_RESTART_WINDOW.as_secs(),
                                "server protocol restarted too often — treating as \
                                 abusive and tearing down the connection"
                            );
                            conn_cancel.cancel();
                            mux_abort.abort();
                            return;
                        }

                        // Between the responder returning and the swap below,
                        // frames for this route land on the dropped receiver
                        // and are discarded. The window is a few microseconds
                        // and the peer has just said it is done with this
                        // mini-protocol, so it cannot legitimately be sending;
                        // upstream's `StartOnDemand` re-arm has the same shape.
                        match mux_resubscribe.resubscribe(protocol_id, Direction::ResponderDir) {
                            Some(fresh) => {
                                debug!(
                                    %addr,
                                    protocol = label,
                                    "responder re-armed after client MsgDone \
                                     (InboundGovernor TrResponderRestarted)"
                                );
                                channel = fresh;
                            }
                            None => {
                                // The route is not registered, so it cannot be
                                // re-armed and would be silent from here on.
                                // Silence is the one outcome that is never
                                // acceptable: kill the connection instead.
                                warn!(
                                    %addr,
                                    protocol = label,
                                    "responder ended but the route cannot be re-armed \
                                     — tearing down rather than going silent"
                                );
                                conn_cancel.cancel();
                                mux_abort.abort();
                                return;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Stop server-side protocol tasks.
    ///
    /// Same graceful-then-abort pattern as [`stop_hot_protocols`].
    pub async fn stop_server_protocols(&mut self) {
        Self::stop_tasks(&mut self.server_tasks, "server", self.addr).await;
    }

    /// Internal helper: cancel tasks, wait with timeout, abort stragglers.
    async fn stop_tasks(
        tasks: &mut Vec<(JoinHandle<()>, CancellationToken)>,
        label: &str,
        addr: SocketAddr,
    ) {
        if tasks.is_empty() {
            return;
        }

        debug!(%addr, label, count = tasks.len(), "stopping protocol tasks");

        // Signal cancellation to all tasks.
        for (_, token) in tasks.iter() {
            token.cancel();
        }

        // Wait for graceful shutdown with timeout.
        // Get abort handles BEFORE moving JoinHandles into the timeout future,
        // so we can forcibly abort tasks that don't stop within the timeout.
        let drain = std::mem::take(tasks);
        for (handle, _) in drain {
            let abort_handle = handle.abort_handle();
            match tokio::time::timeout(PROTOCOL_SHUTDOWN_TIMEOUT, handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // JoinError — task panicked or was cancelled.
                    warn!(%addr, label, error = %e, "protocol task join error");
                }
                Err(_) => {
                    // Timeout expired — forcibly abort the task.
                    warn!(%addr, label, "protocol task did not stop within timeout, aborting");
                    abort_handle.abort();
                }
            }
        }
    }

    /// Shut down the entire connection: stop all protocols, cancel the mux.
    ///
    /// This is the clean teardown path for Cold transition. After this call,
    /// the `PeerConnection` is no longer usable.
    pub async fn shutdown(&mut self) {
        info!(addr = %self.addr, "shutting down peer connection");

        // Stop protocol tasks first (graceful).
        self.stop_hot_protocols().await;
        self.stop_warm_protocols().await;
        self.stop_server_protocols().await;

        // Cancel the top-level token — this will signal any remaining child tasks.
        self.cancel.cancel();

        // Abort the mux task (drops the bearer, closing the TCP connection).
        self.mux_handle.abort();
        let _ = (&mut self.mux_handle).await;
    }

    /// Check if the underlying mux is still running.
    ///
    /// Returns `false` if the mux task has completed (connection is dead).
    /// The node should treat this as a connection failure and clean up.
    pub fn is_alive(&self) -> bool {
        !self.mux_handle.is_finished()
    }

    /// Get the top-level cancellation token for this connection.
    ///
    /// Child tokens derived from this are used by individual protocol tasks.
    /// Cancelling this token will signal all protocol tasks to stop.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Check if warm protocols are currently running.
    pub fn has_warm_protocols(&self) -> bool {
        !self.warm_tasks.is_empty()
    }

    /// Check if hot protocols are currently running.
    pub fn has_hot_protocols(&self) -> bool {
        !self.hot_tasks.is_empty()
    }

    /// Take the PeerSharing client channel for use by the peersharing client task.
    ///
    /// Returns `None` if the channel is unavailable (not subscribed on this
    /// connection, or already taken by a running task).  For inbound connections
    /// that were accepted with `initiator_only = true`, the initiator-direction
    /// channels are `None` and this correctly returns `None`.
    // Allow dead-code: only called from the binary's
    // `connection_lifecycle.rs`, which is not part of the lib crate when
    // built with `--all-features` (test-utils re-exports peer_connection
    // only).
    #[allow(dead_code)]
    pub(crate) fn take_peersharing_client_channel(&mut self) -> Option<MuxChannel> {
        self.peersharing_client_channel.take()
    }

    /// Check whether the hot client protocol channels are still available for use.
    ///
    /// Returns `true` only if ALL three hot client channels (ChainSync, BlockFetch,
    /// TxSubmission2) are present.  Once `start_hot_protocols` has been called,
    /// all three channels are moved (`Option::take`) into their respective tasks
    /// and this method returns `false` — the channels cannot be reclaimed without
    /// closing the TCP connection and opening a fresh `PeerConnection`.
    ///
    /// Used as a pre-flight check in `promote_to_hot` to detect the warm→hot
    /// promotion race (#516) early and emit a clear diagnostic before returning
    /// `PeerConnectionError::ChannelUnavailable`.
    pub fn has_hot_client_channels(&self) -> bool {
        self.chainsync_client_channel.is_some()
            && self.blockfetch_client_channel.is_some()
            && self.txsubmission_client_channel.is_some()
    }
}

/// Errors specific to `PeerConnection` lifecycle operations.
#[derive(Debug)]
pub enum PeerConnectionError {
    /// TCP connect timed out.
    ConnectTimeout(SocketAddr),
    /// TCP connect or bearer creation failed.
    Connect(SocketAddr, String),
    /// N2N handshake failed.
    Handshake(SocketAddr, String),
    /// Requested protocol channel is unavailable (already taken or not subscribed).
    ChannelUnavailable(&'static str),
    /// Mux error during operation.
    Mux(MuxError),
}

impl std::fmt::Display for PeerConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectTimeout(addr) => write!(f, "TCP connect timeout to {addr}"),
            Self::Connect(addr, reason) => write!(f, "connect to {addr} failed: {reason}"),
            Self::Handshake(addr, reason) => write!(f, "handshake with {addr} failed: {reason}"),
            Self::ChannelUnavailable(proto) => {
                write!(f, "{proto} channel unavailable (already taken)")
            }
            Self::Mux(e) => write!(f, "mux error: {e}"),
        }
    }
}

impl std::error::Error for PeerConnectionError {}

impl From<MuxError> for PeerConnectionError {
    fn from(e: MuxError) -> Self {
        Self::Mux(e)
    }
}

/// Test-only and `test-utils`-feature helpers for constructing fake
/// `PeerConnection` instances in unit and integration tests.
///
/// All methods in this block are excluded from production builds.  They are
/// compiled when running `cargo test` (the crate's own unit tests) OR when
/// the `test-utils` feature is enabled (integration tests in
/// `tests/lifecycle_invariants.rs`).
#[cfg(any(test, feature = "test-utils"))]
impl PeerConnection {
    /// Create a minimal `PeerConnection` for use in unit tests.
    ///
    /// Spawns a no-op mux task so the `JoinHandle` is valid.  All protocol
    /// channels are `None`, and all task lists are empty.  The instance
    /// must be created inside a tokio runtime context (e.g. `#[tokio::test]`).
    pub fn fake_for_test(addr: SocketAddr) -> Self {
        let local_addr: SocketAddr = match addr {
            SocketAddr::V4(_) => "127.0.0.1:0".parse().unwrap(),
            SocketAddr::V6(_) => "[::1]:0".parse().unwrap(),
        };
        Self::fake_for_test_with_local(addr, local_addr, PeerConnectionDirection::Outbound)
    }

    /// Variant of [`fake_for_test`] that also pins the `local_addr` and
    /// direction so tests can build distinct `ConnectionId`s for the same
    /// remote (e.g. an outbound + inbound duplex pair).
    pub fn fake_for_test_with_local(
        addr: SocketAddr,
        local_addr: SocketAddr,
        direction: PeerConnectionDirection,
    ) -> Self {
        let mux_handle = tokio::spawn(async { Ok::<(), MuxError>(()) });
        Self {
            addr,
            local_addr,
            direction,
            version: 0,
            network_magic: 0,
            chainsync_client_channel: None,
            blockfetch_client_channel: None,
            txsubmission_client_channel: None,
            keepalive_client_channel: None,
            peersharing_client_channel: None,
            chainsync_server_channel: None,
            blockfetch_server_channel: None,
            txsubmission_server_channel: None,
            keepalive_server_channel: None,
            peersharing_server_channel: None,
            mux_handle,
            mux_resubscribe: MuxHandle::empty(),
            cancel: tokio_util::sync::CancellationToken::new(),
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        }
    }

    /// Create a `PeerConnection` with **fake but present** hot client channels.
    ///
    /// Each channel is backed by a disconnected `mpsc` pair (the sender side is
    /// kept alive for the lifetime of this helper).  The channels are functional
    /// enough for `start_hot_protocols` / `has_hot_client_channels` checks; they
    /// will return `MuxError::BearerClosed` on the first actual `recv()`.
    ///
    /// Used by regression tests for the warm→hot promotion race (#516).
    pub fn fake_with_hot_channels(addr: SocketAddr) -> Self {
        use std::sync::atomic::AtomicUsize;
        use tokio::sync::mpsc;

        fn make_channel(protocol_id: u16) -> MuxChannel {
            // Shared egress queue — the sender end is kept alive by the channel itself.
            let (egress_tx, _egress_rx) = mpsc::channel(32);
            // Ingress: receiver lives inside MuxChannel; sender dropped immediately so
            // recv() returns BearerClosed, mimicking a closed connection.
            let (_ingress_tx, ingress_rx) = mpsc::channel(32);
            MuxChannel::new(
                protocol_id,
                dugite_network::mux::segment::Direction::InitiatorDir,
                egress_tx,
                ingress_rx,
                65536,
                std::sync::Arc::new(AtomicUsize::new(0)),
            )
        }

        let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mux_handle = tokio::spawn(async { Ok::<(), MuxError>(()) });

        use dugite_network::protocol::{
            PROTOCOL_N2N_BLOCKFETCH, PROTOCOL_N2N_CHAINSYNC, PROTOCOL_N2N_KEEPALIVE,
            PROTOCOL_N2N_TXSUBMISSION,
        };

        Self {
            addr,
            local_addr,
            direction: PeerConnectionDirection::Outbound,
            version: 0,
            network_magic: 0,
            chainsync_client_channel: Some(make_channel(PROTOCOL_N2N_CHAINSYNC)),
            blockfetch_client_channel: Some(make_channel(PROTOCOL_N2N_BLOCKFETCH)),
            txsubmission_client_channel: Some(make_channel(PROTOCOL_N2N_TXSUBMISSION)),
            keepalive_client_channel: Some(make_channel(PROTOCOL_N2N_KEEPALIVE)),
            peersharing_client_channel: None,
            chainsync_server_channel: None,
            blockfetch_server_channel: None,
            txsubmission_server_channel: None,
            keepalive_server_channel: None,
            peersharing_server_channel: None,
            mux_handle,
            mux_resubscribe: MuxHandle::empty(),
            cancel: tokio_util::sync::CancellationToken::new(),
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        }
    }

    /// Build a `PeerConnection` with real, mux-backed SERVER (responder)
    /// channels, for the #980 responder-restart integration test.
    ///
    /// The initiator-side twin already exists ([`Self::fake_with_mux_resubscribe`]).
    /// This one is what makes the responder half testable at all: the property
    /// under test — "a responder that ends cleanly is re-armed on the same
    /// bearer" — is only observable when `MuxHandle::resubscribe` has genuine
    /// `SwappableSender` entries for `ResponderDir`.
    #[allow(clippy::too_many_arguments)]
    pub fn fake_with_server_channels(
        addr: SocketAddr,
        local_addr: SocketAddr,
        cs_ch: MuxChannel,
        bf_ch: MuxChannel,
        tx_ch: MuxChannel,
        ka_ch: MuxChannel,
        ps_ch: MuxChannel,
        mux_task: JoinHandle<Result<(), MuxError>>,
        mux_resubscribe: MuxHandle,
    ) -> Self {
        Self {
            addr,
            local_addr,
            direction: PeerConnectionDirection::Inbound,
            version: 0,
            network_magic: 0,
            chainsync_client_channel: None,
            blockfetch_client_channel: None,
            txsubmission_client_channel: None,
            keepalive_client_channel: None,
            peersharing_client_channel: None,
            chainsync_server_channel: Some(cs_ch),
            blockfetch_server_channel: Some(bf_ch),
            txsubmission_server_channel: Some(tx_ch),
            keepalive_server_channel: Some(ka_ch),
            peersharing_server_channel: Some(ps_ch),
            mux_handle: mux_task,
            mux_resubscribe,
            cancel: tokio_util::sync::CancellationToken::new(),
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        }
    }

    /// The connection-wide cancellation token, so a test can observe that a
    /// failing responder tore the whole connection down (#980).
    pub fn cancel_token_for_test(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Create a `PeerConnection` backed by caller-supplied channels and a real
    /// `MuxHandle` for the `stop_hot_protocols_and_recover()` integration test.
    ///
    /// Used exclusively by `tests/lifecycle_invariants.rs` to construct a
    /// connection from a manually-built mux (bypassing the N2N handshake) so
    /// that `MuxHandle::resubscribe()` has real `SwappableSender` entries.
    ///
    /// # Parameters
    ///
    /// * `addr` — Nominal remote peer address (used for logging).
    /// * `local_addr` — Local socket address.
    /// * `direction` — `Outbound` for initiator-side connections.
    /// * `cs_ch`, `bf_ch`, `tx_ch`, `ka_ch` — Pre-subscribed client channels.
    /// * `mux_task` — `JoinHandle` for the spawned mux task.
    /// * `mux_resubscribe` — `MuxHandle` taken from the same mux before `run()`.
    #[allow(clippy::too_many_arguments)]
    pub fn fake_with_mux_resubscribe(
        addr: SocketAddr,
        local_addr: SocketAddr,
        direction: PeerConnectionDirection,
        cs_ch: MuxChannel,
        bf_ch: MuxChannel,
        tx_ch: MuxChannel,
        ka_ch: MuxChannel,
        mux_task: tokio::task::JoinHandle<Result<(), MuxError>>,
        mux_resubscribe: MuxHandle,
    ) -> Self {
        Self {
            addr,
            local_addr,
            direction,
            version: 0,
            network_magic: 0,
            chainsync_client_channel: Some(cs_ch),
            blockfetch_client_channel: Some(bf_ch),
            txsubmission_client_channel: Some(tx_ch),
            keepalive_client_channel: Some(ka_ch),
            peersharing_client_channel: None,
            chainsync_server_channel: None,
            blockfetch_server_channel: None,
            txsubmission_server_channel: None,
            keepalive_server_channel: None,
            peersharing_server_channel: None,
            mux_handle: mux_task,
            mux_resubscribe,
            cancel: tokio_util::sync::CancellationToken::new(),
            warm_tasks: Vec::new(),
            hot_tasks: Vec::new(),
            server_tasks: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #924: a failed inbound handshake must CLOSE the socket.
    ///
    /// The mux task owns the `TcpBearer`. Dropping its `JoinHandle` on the
    /// handshake-failure early return detaches the task rather than aborting
    /// it, so the socket stayed open for the process lifetime — an
    /// unauthenticated peer could hold one connection per malformed handshake.
    /// cardano-node closes the connection on every refused handshake; before
    /// this fix dugite closed none of them.
    ///
    /// The test drives the real `accept()` path with a peer that sends garbage
    /// instead of a handshake, then asserts the client side observes EOF.
    #[tokio::test]
    async fn failed_inbound_handshake_closes_the_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            // Garbage payload => handshake fails => early return.
            PeerConnection::accept(stream, peer, 42, false, false).await
        });

        let mut client = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        // Mux frame (ts=0, proto=0, len=8) carrying non-CBOR garbage.
        let payload: [u8; 8] = [0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
        let mut frame = vec![0u8, 0, 0, 0, 0, 0, 0, payload.len() as u8];
        frame.extend_from_slice(&payload);
        client.write_all(&frame).await.unwrap();

        let accept_result = server.await.unwrap();
        assert!(
            accept_result.is_err(),
            "garbage handshake must fail the accept"
        );

        // The peer must see EOF (read returns 0), not a connection left open.
        let mut buf = [0u8; 64];
        let eof = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match client.read(&mut buf).await {
                    Ok(0) => return true,  // clean close
                    Ok(_) => continue,     // refusal bytes, keep reading
                    Err(_) => return true, // reset also counts as closed
                }
            }
        })
        .await;

        assert!(
            matches!(eof, Ok(true)),
            "socket still open 5s after a failed handshake — the mux task was \
             detached instead of aborted (#924)"
        );
    }

    /// Verify PeerConnectionError Display formatting.
    #[test]
    fn error_display() {
        let addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        let err = PeerConnectionError::ConnectTimeout(addr);
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("127.0.0.1:3001"));

        let err = PeerConnectionError::Handshake(addr, "version mismatch".into());
        assert!(err.to_string().contains("handshake"));
        assert!(err.to_string().contains("version mismatch"));

        let err = PeerConnectionError::ChannelUnavailable("chainsync");
        assert!(err.to_string().contains("chainsync"));
        assert!(err.to_string().contains("unavailable"));
    }

    /// Verify protocol shutdown timeout constant matches Haskell's spsDeactivateTimeout.
    #[test]
    fn shutdown_timeout_matches_haskell() {
        assert_eq!(PROTOCOL_SHUTDOWN_TIMEOUT, Duration::from_secs(5));
    }

    /// Verify default constants are reasonable.
    ///
    /// A-006 (security audit 2026-05-19): DEFAULT_INGRESS_LIMIT was reduced
    /// from 64 MB to 4 MB to match Haskell's `ingressQueueSize = 4 * 1024 * 1024`.
    #[test]
    fn default_constants() {
        assert_eq!(DEFAULT_INGRESS_LIMIT, 4 * 1024 * 1024); // 4 MB — matches Haskell
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
    }

    // ── warm→hot promotion race regression tests (#516) ─────────────────────
    //
    // Root cause: `MuxChannel` is single-use.  Once `start_hot_protocols`
    // moves the three client channels into their tasks, the `Option` fields are
    // `None`.  Any subsequent `start_hot_protocols` call (e.g. after a
    // governor-initiated Hot→Warm demotion followed by a PromoteToHot action)
    // fails with `ChannelUnavailable("chainsync")`, producing the churn loop
    // observed in the soak logs (#516).
    //
    // The fix is to close the TCP connection entirely on Hot→Warm demotion so
    // that the next PromoteToWarm creates a fresh `PeerConnection` (and fresh
    // channels).  These tests pin the channel-lifecycle invariants so a
    // regression in the fix path is caught immediately.

    /// Fresh connection: all hot client channels are present.
    #[tokio::test]
    async fn hot_channels_available_on_fresh_connection() {
        let addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let conn = PeerConnection::fake_with_hot_channels(addr);
        assert!(
            conn.has_hot_client_channels(),
            "fresh connection must have all hot client channels available"
        );
    }

    /// After `start_hot_protocols`, channels are consumed: subsequent call fails.
    ///
    /// This is the exact precondition that caused the churn loop in #516.
    /// Without the `demote_to_warm` fix, a Hot→Warm demotion left channels
    /// consumed, making the next `PromoteToHot` fail with ChannelUnavailable.
    #[tokio::test]
    async fn start_hot_protocols_consumes_channels_second_call_fails() {
        let addr: SocketAddr = "10.0.0.2:3001".parse().unwrap();
        let mut conn = PeerConnection::fake_with_hot_channels(addr);

        assert!(
            conn.has_hot_client_channels(),
            "channels must be Some before first promotion"
        );

        // Protocol task factories that immediately return without reading/writing.
        fn noop_fn() -> ProtocolTaskFn {
            Box::new(move |_ch, _cancel| Box::pin(async {}))
        }

        // First promotion succeeds and consumes the channels.
        conn.start_hot_protocols(noop_fn(), noop_fn(), noop_fn())
            .expect("first start_hot_protocols must succeed");

        assert!(
            !conn.has_hot_client_channels(),
            "channels must be None after start_hot_protocols (they were moved into tasks)"
        );

        // Second promotion — simulates PromoteToHot on a demoted-but-not-reconnected
        // connection — must fail with ChannelUnavailable.
        let err = conn
            .start_hot_protocols(noop_fn(), noop_fn(), noop_fn())
            .expect_err("second start_hot_protocols must fail (channels already consumed)");

        assert!(
            matches!(err, PeerConnectionError::ChannelUnavailable("chainsync")),
            "expected ChannelUnavailable(\"chainsync\"), got: {err}"
        );

        // The error message must contain the string observed in the soak logs (#516).
        assert!(
            err.to_string()
                .contains("chainsync channel unavailable (already taken)"),
            "error Display must match the log string from #516: {err}"
        );
    }

    /// After `stop_hot_protocols`, channels remain `None` — confirming that
    /// stopping tasks does NOT restore channels and that reconnection is required.
    ///
    /// This pins the invariant that motivates the `demote_to_warm` fix:
    /// closing the TCP connection on Hot→Warm is necessary because there
    /// is no way to recover channels after the tasks have consumed them.
    #[tokio::test]
    async fn stop_hot_protocols_does_not_restore_channels() {
        let addr: SocketAddr = "10.0.0.3:3001".parse().unwrap();
        let mut conn = PeerConnection::fake_with_hot_channels(addr);

        fn noop_fn() -> ProtocolTaskFn {
            Box::new(move |_ch, _cancel| Box::pin(async {}))
        }

        conn.start_hot_protocols(noop_fn(), noop_fn(), noop_fn())
            .expect("start_hot_protocols must succeed");

        // Wait for the no-op tasks to exit naturally.
        conn.stop_hot_protocols().await;

        assert!(
            !conn.has_hot_client_channels(),
            "channels must remain None after stop_hot_protocols — \
             stopping tasks does not restore consumed channels; \
             reconnection is the only recovery path"
        );
    }
}
