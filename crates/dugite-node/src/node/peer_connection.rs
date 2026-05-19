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
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use dugite_network::handshake::n2n::N2NVersionData;
use dugite_network::handshake::{run_n2n_handshake_client, run_n2n_handshake_server};
use dugite_network::mux::channel::MuxChannel;
use dugite_network::mux::segment::Direction;
use dugite_network::mux::Mux;
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
/// With 10 protocols × N peers × 4MB = 40MB per peer at worst, this is a large
/// reduction from the prior 64MB per channel (640MB per peer).
///
/// A-006 (security audit 2026-05-19): the prior 64MB limit enabled a slow-reader
/// attack where a peer could accumulate 640MB of ingress buffer before triggering
/// IngressQueueOverrun.  Reduced to match Haskell's limit.
const DEFAULT_INGRESS_LIMIT: usize = 4 * 1024 * 1024; // 4 MB (matches Haskell ingressQueueSize)

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

    /// PeerSharing client channel (protocol 10). Reserved for future use.
    /// In Haskell, PeerSharing responder starts on-demand when the remote peer
    /// sends a MsgShareRequest. The initiator sends requests periodically from
    /// the Governor. Currently subscribed but no task is spawned for it —
    /// PeerSharing integration is tracked separately.
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
            DEFAULT_INGRESS_LIMIT,
        );
        let blockfetch_client_ch = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let txsubmission_client_ch = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
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
            DEFAULT_INGRESS_LIMIT,
        );
        let bf_srv = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
        );
        let tx_srv = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::ResponderDir,
            DEFAULT_INGRESS_LIMIT,
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

        // Spawn mux task — runs until bearer closes or error.
        let cancel = CancellationToken::new();
        let mux_handle = tokio::spawn(async move { mux.run().await });

        // Run N2N handshake on the handshake channel.
        let our_data = N2NVersionData::new(network_magic, initiator_only, peer_sharing);
        let handshake_result = run_n2n_handshake_client(&mut handshake_ch, &our_data)
            .await
            .map_err(|e| PeerConnectionError::Handshake(addr, e.to_string()))?;

        let version = handshake_result.version;
        info!(%addr, version, "N2N handshake complete");

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

        // Spawn mux task.
        let cancel = CancellationToken::new();
        let mux_handle = tokio::spawn(async move { mux.run().await });

        // Run N2N handshake as server.
        let our_data = N2NVersionData::new(network_magic, initiator_only, peer_sharing);
        let handshake_result = run_n2n_handshake_server(&mut handshake_ch, &our_data)
            .await
            .map_err(|e| PeerConnectionError::Handshake(addr, e.to_string()))?;

        let version = handshake_result.version;
        info!(%addr, version, "N2N handshake complete (inbound)");

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

    /// Stop warm-temperature protocol tasks (KeepAlive).
    ///
    /// Same graceful-then-abort pattern as [`stop_hot_protocols`].
    pub async fn stop_warm_protocols(&mut self) {
        Self::stop_tasks(&mut self.warm_tasks, "warm", self.addr).await;
    }

    /// Start server-side (responder) protocol tasks.
    ///
    /// Takes the server channels and spawns each protocol as an independent
    /// tokio task using the provided factory functions. Each factory receives
    /// its channel and a cancellation token.
    ///
    /// Server protocols run the responder side: they wait for requests from
    /// the remote peer's client protocols and respond accordingly. In duplex
    /// mode, both client and server protocols run simultaneously on the same
    /// multiplexed connection.
    ///
    /// # Arguments
    ///
    /// * `chainsync_server_fn` — Factory for the ChainSync server task
    /// * `blockfetch_server_fn` — Factory for the BlockFetch server task
    /// * `txsubmission_server_fn` — Factory for the TxSubmission2 server task
    /// * `keepalive_server_fn` — Factory for the KeepAlive server task
    /// * `peersharing_server_fn` — Factory for the PeerSharing server task
    pub fn start_server_protocols(
        &mut self,
        chainsync_server_fn: ProtocolTaskFn,
        blockfetch_server_fn: ProtocolTaskFn,
        txsubmission_server_fn: ProtocolTaskFn,
        keepalive_server_fn: ProtocolTaskFn,
        peersharing_server_fn: ProtocolTaskFn,
    ) -> Result<(), PeerConnectionError> {
        let cs_ch = self
            .chainsync_server_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("chainsync_server"))?;
        let bf_ch = self
            .blockfetch_server_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("blockfetch_server"))?;
        let tx_ch = self.txsubmission_server_channel.take().ok_or(
            PeerConnectionError::ChannelUnavailable("txsubmission_server"),
        )?;
        let ka_ch = self
            .keepalive_server_channel
            .take()
            .ok_or(PeerConnectionError::ChannelUnavailable("keepalive_server"))?;
        let ps_ch = self.peersharing_server_channel.take().ok_or(
            PeerConnectionError::ChannelUnavailable("peersharing_server"),
        )?;

        // Spawn ChainSync server task.
        let cs_token = self.cancel.child_token();
        let cs_token_clone = cs_token.clone();
        let cs_handle = tokio::spawn(async move {
            (chainsync_server_fn)(cs_ch, cs_token_clone).await;
        });
        self.server_tasks.push((cs_handle, cs_token));

        // Spawn BlockFetch server task.
        let bf_token = self.cancel.child_token();
        let bf_token_clone = bf_token.clone();
        let bf_handle = tokio::spawn(async move {
            (blockfetch_server_fn)(bf_ch, bf_token_clone).await;
        });
        self.server_tasks.push((bf_handle, bf_token));

        // Spawn TxSubmission2 server task.
        let tx_token = self.cancel.child_token();
        let tx_token_clone = tx_token.clone();
        let tx_handle = tokio::spawn(async move {
            (txsubmission_server_fn)(tx_ch, tx_token_clone).await;
        });
        self.server_tasks.push((tx_handle, tx_token));

        // Spawn KeepAlive server task.
        let ka_token = self.cancel.child_token();
        let ka_token_clone = ka_token.clone();
        let ka_handle = tokio::spawn(async move {
            (keepalive_server_fn)(ka_ch, ka_token_clone).await;
        });
        self.server_tasks.push((ka_handle, ka_token));

        // Spawn PeerSharing server task.
        let ps_token = self.cancel.child_token();
        let ps_token_clone = ps_token.clone();
        let ps_handle = tokio::spawn(async move {
            (peersharing_server_fn)(ps_ch, ps_token_clone).await;
        });
        self.server_tasks.push((ps_handle, ps_token));

        debug!(addr = %self.addr, "started server protocols (ChainSync, BlockFetch, TxSubmission2, KeepAlive, PeerSharing)");
        Ok(())
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

#[cfg(test)]
impl PeerConnection {
    /// Create a minimal `PeerConnection` for use in unit tests.
    ///
    /// Spawns a no-op mux task so the `JoinHandle` is valid.  All protocol
    /// channels are `None`, and all task lists are empty.  The instance
    /// must be created inside a tokio runtime context (e.g. `#[tokio::test]`).
    pub(crate) fn fake_for_test(addr: SocketAddr) -> Self {
        let local_addr: SocketAddr = match addr {
            SocketAddr::V4(_) => "127.0.0.1:0".parse().unwrap(),
            SocketAddr::V6(_) => "[::1]:0".parse().unwrap(),
        };
        Self::fake_for_test_with_local(addr, local_addr, PeerConnectionDirection::Outbound)
    }

    /// Variant of [`fake_for_test`] that also pins the `local_addr` and
    /// direction so tests can build distinct `ConnectionId`s for the same
    /// remote (e.g. an outbound + inbound duplex pair).
    pub(crate) fn fake_for_test_with_local(
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
    pub(crate) fn fake_with_hot_channels(addr: SocketAddr) -> Self {
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
