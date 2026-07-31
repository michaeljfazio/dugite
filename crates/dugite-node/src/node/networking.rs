//! Node-level networking orchestration.
//!
//! This module bridges the new dugite-network crate's protocol primitives
//! with the node's high-level connection management. It defines the types
//! and orchestration logic that are specific to the node's needs (topology
//! management, peer lifecycle, block fetch coordination, N2C server, etc.)
//! without polluting the network protocol crate with node concerns.
//!
//! ## Architecture
//! ```text
//! dugite-network (protocol primitives)
//!   ├── Bearer (TCP/Unix transport)
//!   ├── Mux (multiplexer)
//!   ├── Protocols (ChainSync, BlockFetch, etc.)
//!   └── PeerManager (basic cold/warm/hot lifecycle)
//!
//! dugite-node::networking (this module, node-level orchestration)
//!   ├── NodePeerManager (wraps PeerManager + connection tracking)
//!   ├── NodeServer (TCP/Unix listener orchestration)
//!   ├── PeerConnection (per-peer protocol bundle)
//!   └── SyncClient (pipelined ChainSync + BlockFetch coordination)
//! ```

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// Returns true for IPs that should NEVER be accepted from P2P peer-sharing
/// or ledger-published relay records: loopback, unspecified (0.0.0.0/::),
/// link-local, private RFC1918 ranges, multicast, IPv4 broadcast, IPv6
/// unique-local (fc00::/7) and documentation ranges.
///
/// Such addresses on a public diffusion topology indicate either a
/// misconfigured operator or an adversarial attempt to redirect peers to
/// internal/intranet hosts. Static topology entries can still reference
/// them (e.g. a co-located BP+relay over 127.0.0.1) but they must never
/// be propagated as candidate peers.
pub fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => is_non_public_ipv6(v6),
    }
}

fn is_non_public_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        // 100.64.0.0/10 — Carrier-grade NAT (RFC 6598)
        || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64)
        // 0.0.0.0/8 — "this network"
        || ip.octets()[0] == 0
}

fn is_non_public_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fe80::/10 — link-local
        || (ip.segments()[0] & 0xffc0) == 0xfe80
        // fc00::/7 — unique-local
        || (ip.segments()[0] & 0xfe00) == 0xfc00
        // 2001:db8::/32 — documentation
        || (ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
}

use dugite_network::connection::state::{
    ConnectionManagerCounters, ConnectionState, DataFlow, Provenance,
};
use dugite_network::peer::manager::MAX_COLD_PEER_FAILURES;
use dugite_network::{PeerManager, PeerSource, PeerState};

// ─── Configuration Types ─────────────────────────────────────────────────────

/// Diffusion mode — whether the node accepts inbound connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // InitiatorOnly variant for networking rewrite
pub enum DiffusionMode {
    /// Only initiate outbound connections (relay behind NAT).
    InitiatorOnly,
    /// Both initiate outbound and accept inbound connections.
    InitiatorAndResponder,
}

/// Network timeout configuration.
#[derive(Debug, Clone)]
#[allow(dead_code)] // used by networking rewrite
pub struct TimeoutConfig {
    /// Timeout for TCP connection establishment.
    pub connect_timeout: Duration,
    /// Timeout for handshake negotiation.
    pub handshake_timeout: Duration,
    /// KeepAlive ping interval.
    pub keepalive_interval: Duration,
    /// Timeout before closing an idle connection at tip.
    pub await_reply_timeout: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(30),
            keepalive_interval: Duration::from_secs(30),
            await_reply_timeout: Duration::from_secs(135),
        }
    }
}

/// Configuration for the node's peer management.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields used by networking rewrite
pub struct PeerManagerConfig {
    /// Diffusion mode (InitiatorOnly or InitiatorAndResponder).
    pub diffusion_mode: DiffusionMode,
    /// Whether peer sharing is enabled.
    pub peer_sharing_enabled: bool,
    /// Target number of hot (active) peers.
    pub target_hot_peers: usize,
    /// Target number of warm (established but not active) peers.
    pub target_warm_peers: usize,
    /// Target number of known (cold + warm + hot) peers.
    pub target_known_peers: usize,
    /// Network magic for handshake validation.
    pub network_magic: u64,
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        Self {
            diffusion_mode: DiffusionMode::InitiatorAndResponder,
            peer_sharing_enabled: true,
            target_hot_peers: 5,
            target_warm_peers: 10,
            target_known_peers: 100,
            network_magic: 2,
        }
    }
}

/// Direction of a peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    Outbound,
    Inbound,
}

/// Category of a peer for big ledger peer tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // used by networking rewrite
pub enum PeerCategory {
    Normal,
    BigLedgerPeer,
    LocalRoot,
}

/// A local root peer group from the topology configuration.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields used by networking rewrite
pub struct LocalRootGroupInfo {
    /// Name of the group (for logging/display).
    pub name: String,
    /// Resolved addresses of peers in this group.
    pub addrs: Vec<SocketAddr>,
    /// Target number of hot peers in this group.
    pub hot_valency: usize,
    /// Target number of warm peers in this group.
    pub warm_valency: usize,
    /// Per-group diffusion mode override. None = use node-level default.
    pub diffusion_mode: Option<DiffusionMode>,
    /// Whether peers in this group are behind a firewall (inbound-only).
    pub behind_firewall: bool,
    /// Whether peers in this group can be shared via the peer sharing protocol.
    pub advertise: bool,
}

// ─── Announcement Types ──────────────────────────────────────────────────────

// RollbackAnnouncement is defined in dugite-network alongside BlockAnnouncement
// and re-exported as `dugite_network::RollbackAnnouncement`.  The node crate
// uses that type directly for broadcast channels shared between sync and the
// ChainSync/LocalChainSync servers.

// ─── Sync Client Types ───────────────────────────────────────────────────────

/// Information about a Byron Epoch Boundary Block (EBB).
///
/// Byron EBBs share a slot with the first block of the epoch and need
/// special handling during sync.
#[derive(Debug, Clone)]
#[allow(dead_code)] // used by networking rewrite
pub struct EbbInfo {
    /// Slot of the EBB (same as the first block of the epoch).
    pub slot: u64,
    /// Hash of the EBB.
    pub hash: [u8; 32],
    /// Epoch number this EBB marks the boundary of.
    pub epoch: u64,
}

/// Result from a pipelined header batch request.
#[derive(Debug)]
#[allow(dead_code)] // used by networking rewrite
pub enum HeaderBatchResult {
    /// A batch of headers was received.
    Headers(Vec<HeaderInfo>),
    /// The chain rolled backward to a point.
    RollBack { slot: u64, hash: [u8; 32] },
    /// We're at the chain tip — waiting for new blocks.
    Await,
}

/// Information about a received block header.
#[derive(Debug, Clone)]
#[allow(dead_code)] // used by networking rewrite
pub struct HeaderInfo {
    /// Raw header CBOR bytes.
    pub header: Vec<u8>,
    /// Slot number.
    pub slot: u64,
    /// Block header hash.
    pub hash: [u8; 32],
    /// Block number (height).
    pub block_number: u64,
    /// Tip slot reported by the server.
    pub tip_slot: u64,
}

// ─── Error Types ─────────────────────────────────────────────────────────────

/// Errors from N2N client operations.
#[derive(Debug)]
#[allow(dead_code)] // used by networking rewrite
pub enum ClientError {
    /// TCP connection failed.
    Connection(String),
    /// Handshake negotiation failed.
    Handshake(String),
    /// Protocol error during operation.
    Protocol(String),
    /// Connection timed out.
    Timeout,
    /// Connection was closed by the remote peer.
    Closed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(e) => write!(f, "connection: {e}"),
            Self::Handshake(e) => write!(f, "handshake: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Timeout => write!(f, "timeout"),
            Self::Closed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Errors from duplex peer connection operations.
#[derive(Debug)]
#[allow(dead_code)] // used by networking rewrite
pub enum DuplexError {
    /// The underlying client connection failed.
    Connection(ClientError),
    /// The peer was disconnected.
    Disconnected,
}

impl std::fmt::Display for DuplexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(e) => write!(f, "duplex: {e}"),
            Self::Disconnected => write!(f, "duplex: peer disconnected"),
        }
    }
}

impl std::error::Error for DuplexError {}

// ─── Node Peer Manager ───────────────────────────────────────────────────────

/// Node-level peer manager wrapping the network crate's PeerManager.
///
/// Adds connection state tracking (matching Haskell's `ConnectionManager`),
/// big ledger peer classification, local root group management, diffusion
/// mode, rate limiting, and other node-specific peer management concerns.
///
/// Each connection is tracked via a `ConnectionState` from the network crate,
/// enabling correct `ConnectionManagerCounters` computation that matches
/// the Haskell `connectionStateToCounters` behaviour.
pub struct NodePeerManager {
    /// The underlying protocol-level peer manager.
    pub inner: PeerManager,
    /// Configuration.
    pub config: PeerManagerConfig,
    /// Unix-seconds of the last HAA-failure diagnostic (0 = never) — throttles
    /// the `haa_satisfied` diagnostics to one per 30 s; the predicate is called
    /// from several hot paths per tick and would otherwise log-storm.
    haa_warn_last_secs: std::sync::atomic::AtomicU64,
    /// Whether the sync-time trusted-only clamp is currently in force,
    /// mirrored from the governor tick's `compute_sync_trusted_restriction`
    /// result (`Some` ⇒ true). Read ONLY by `haa_satisfied`'s failure
    /// diagnostics to pick WARN vs debug severity (#931); it has no effect
    /// on the predicate's return value or on clamp enforcement (which lives
    /// in the governor filter and the connection-lifecycle chokepoints).
    sync_trusted_clamp_active: std::sync::atomic::AtomicBool,
    /// Whether the node runs in Ouroboros Genesis consensus mode
    /// (`ConsensusMode = Genesis`) — the `consensusMode` dimension of the
    /// Haskell `outboundConnectionsState` case split that
    /// [`Self::haa_satisfied`] mirrors (#933). Set once at startup from
    /// `node/mod.rs` via [`Self::set_genesis_mode`] (same mirroring pattern
    /// as the #931 clamp flag above); `false` = Praos, the dugite default,
    /// matching `ConsensusMode::default()`.
    genesis_mode: std::sync::atomic::AtomicBool,
    /// Per-connection state machine (Haskell ConnectionManager state).
    ///
    /// Tracks the lifecycle state of each connection. Used to compute
    /// `ConnectionManagerCounters` via `to_counters()`.
    conn_states: HashMap<SocketAddr, ConnectionState>,
    /// Our own listen address (to prevent self-connections).
    local_addr: Option<SocketAddr>,
    /// Configured local root peer groups.
    local_root_groups: Vec<LocalRootGroupInfo>,
    /// Set of big ledger peer addresses.
    big_ledger_peers: std::collections::HashSet<SocketAddr>,
    /// Set of bootstrap peer addresses (topology `bootstrapPeers`, always
    /// trustable). Mirrors Haskell's bootstrap-peer set used by
    /// `outboundConnectionsState`: in `UseBootstrapPeers` mode the
    /// Honest-Availability-Assumption is satisfied by >=1 active (hot) bootstrap
    /// peer (plus the "all established peers are trusted" invariant). Without
    /// this, a from-genesis node with only bootstrap peers and no
    /// `peerSnapshotFile` can never satisfy the HAA and stalls in PreSyncing.
    bootstrap_peer_addrs: std::collections::HashSet<SocketAddr>,
    /// Inbound-duplex peers awaiting `inboundMaturePeerDelay`.
    ///
    /// Mirrors Haskell's `freshDuplexPeers` (OrdPSQ keyed by `(SocketAddr,
    /// arrival_time)`).  Peers are inserted on the first inbound `Duplex`
    /// handshake and removed once 15 minutes have elapsed.  While in this
    /// set, the governor excludes them from `Warm→Hot` promotion candidates
    /// to prevent the at-tip OOM cycle (#703).  Outbound-initiated peers do
    /// NOT enter this set — they go straight through the maturation window.
    fresh_inbound: HashMap<SocketAddr, std::time::Instant>,
    /// Per-peer record of recent rollback-below-immutable witnesses.
    ///
    /// When a peer's `MsgRollBackward` targets a slot older than our
    /// ImmutableDB tip, either the peer is stale/lying OR our local chain
    /// has diverged from the canonical network chain (Ouroboros k-block
    /// finality says one of these must be true).
    ///
    /// Each entry stores `(timestamp, rollback_slot, immutable_slot)`.  The
    /// run-loop polls `divergence_witness_count(now, window)` to detect a
    /// multi-peer divergence consensus and surface a clear operator error.
    /// Issue #699.
    rollback_below_immutable_witnesses: HashMap<SocketAddr, (std::time::Instant, u64, u64)>,
    /// Optional GSM event sender — when set, `peer_disconnected()` emits
    /// `GsmEvent::PeerDisconnected` so the GSM actor can update its peer
    /// tracking (LoE, GDD). `None` when GSM is not wired (e.g. in tests).
    gsm_event_tx: Option<tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>>,
    /// When each peer's last failure was recorded, for the
    /// one-failure-per-connection-attempt dedupe in [`Self::peer_failed`] (#908).
    last_failure_at: HashMap<SocketAddr, std::time::Instant>,
}

/// Collapse window for [`NodePeerManager::peer_failed`] (#908).
///
/// The unit Haskell's outbound governor backs off on is a failed *connection
/// attempt*, not an error report: `jobPromoteColdPeer` applies exactly one
/// `nextConnectTimes` entry per attempt. A single dugite teardown can raise two
/// reports — the protocol task's `peer_failure_tx` and the connection-lifecycle
/// GC that reaps the now-dead mux — so without a collapse window one death would
/// double-count towards both the exponential backoff exponent and the
/// `MAX_COLD_PEER_FAILURES` forget threshold.
///
/// Distinct attempts are always separated by at least `COLD_RETRY_BASE_SECS`
/// (5 s) of backoff, so this window can never merge two genuine attempts.
const PEER_FAILURE_COLLAPSE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Maturation delay applied to inbound-duplex peers before they become
/// eligible for `Warm→Hot` promotion by the outbound peer-selection
/// governor.  Matches Haskell `inboundMaturePeerDelay = 15 * 60` in
/// `ouroboros-network/framework/lib/Ouroboros/Network/InboundGovernor.hs`
/// (#703 fix B).
pub const INBOUND_MATURE_PEER_DELAY: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl NodePeerManager {
    /// Create a new node peer manager with the given configuration.
    pub fn new(config: PeerManagerConfig) -> Self {
        Self {
            inner: PeerManager::new(),
            config,
            haa_warn_last_secs: std::sync::atomic::AtomicU64::new(0),
            sync_trusted_clamp_active: std::sync::atomic::AtomicBool::new(false),
            genesis_mode: std::sync::atomic::AtomicBool::new(false),
            conn_states: HashMap::new(),
            local_addr: None,
            local_root_groups: Vec::new(),
            big_ledger_peers: std::collections::HashSet::new(),
            bootstrap_peer_addrs: std::collections::HashSet::new(),
            fresh_inbound: HashMap::new(),
            rollback_below_immutable_witnesses: HashMap::new(),
            gsm_event_tx: None,
            last_failure_at: HashMap::new(),
        }
    }

    /// Set of inbound peers still within the `inboundMaturePeerDelay` window,
    /// computed against `now`.  This is a read-only filter — matured entries
    /// remain in the underlying map but are excluded from the returned set
    /// (they are cleared on `peer_disconnected`, so the map size is bounded
    /// by the connected-peer count).
    ///
    /// Used by the governor to exclude immature peers from `Warm→Hot`
    /// promotion.  O(N) where N = number of in-flight inbound peers,
    /// bounded by the established-peer cap.  Issue #703 fix B.
    pub fn fresh_inbound_set(
        &self,
        now: std::time::Instant,
    ) -> std::collections::HashSet<SocketAddr> {
        self.fresh_inbound
            .iter()
            .filter_map(|(addr, &arrival)| {
                if now.duration_since(arrival) < INBOUND_MATURE_PEER_DELAY {
                    Some(*addr)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Record that `peer` offered a `MsgRollBackward` targeting a slot older
    /// than our ImmutableDB tip.  Used by the run-loop to detect a
    /// multi-peer divergence consensus.  Issue #699.
    pub fn record_rollback_below_immutable(
        &mut self,
        peer: SocketAddr,
        rollback_slot: u64,
        immutable_slot: u64,
    ) {
        self.rollback_below_immutable_witnesses.insert(
            peer,
            (std::time::Instant::now(), rollback_slot, immutable_slot),
        );
    }

    /// Number of DISTINCT peers that have witnessed a rollback-below-immutable
    /// event in the last `window` duration.
    ///
    /// If this reaches the divergence-witness threshold (typically 3-5), the
    /// node has likely diverged from the canonical chain.  Operator action
    /// required: wipe the DB and re-sync from genesis, OR import a fresh
    /// Mithril snapshot.  Issue #699.
    pub fn divergence_witness_count(
        &self,
        now: std::time::Instant,
        window: std::time::Duration,
    ) -> usize {
        self.rollback_below_immutable_witnesses
            .values()
            .filter(|(t, _, _)| now.duration_since(*t) < window)
            .count()
    }

    /// Drop divergence witnesses older than `window` (housekeeping).
    /// Issue #699.
    pub fn gc_divergence_witnesses(
        &mut self,
        now: std::time::Instant,
        window: std::time::Duration,
    ) {
        self.rollback_below_immutable_witnesses
            .retain(|_, (t, _, _)| now.duration_since(*t) < window);
    }

    /// Read-only iterator over divergence witnesses for diagnostics.
    pub fn divergence_witnesses(
        &self,
    ) -> impl Iterator<Item = (&SocketAddr, &(std::time::Instant, u64, u64))> {
        self.rollback_below_immutable_witnesses.iter()
    }

    /// Drop matured entries from the fresh-inbound map (optional GC pass).
    /// `peer_disconnected` already clears entries when peers leave, so this
    /// is mainly useful for long-running connections.  Exposed for tests
    /// and operator-driven introspection.
    #[allow(dead_code)]
    pub fn gc_fresh_inbound(&mut self, now: std::time::Instant) -> usize {
        let before = self.fresh_inbound.len();
        self.fresh_inbound
            .retain(|_, &mut arrival| now.duration_since(arrival) < INBOUND_MATURE_PEER_DELAY);
        before - self.fresh_inbound.len()
    }

    /// Read-only count of in-flight (still-immature) inbound peers — for
    /// diagnostics + metrics.
    #[allow(dead_code)]
    pub fn fresh_inbound_count(&self) -> usize {
        self.fresh_inbound.len()
    }

    /// Set the GSM event sender so `peer_disconnected()` emits events.
    pub fn set_gsm_event_tx(&mut self, tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>) {
        self.gsm_event_tx = Some(tx);
    }

    /// Set our own listen address.
    pub fn set_local_addr(&mut self, addr: SocketAddr) {
        self.local_addr = Some(addr);
    }

    /// Check whether `addr` is our own listen address.
    ///
    /// Handles the wildcard bind case: when we listen on `0.0.0.0:P`, any
    /// address with a loopback or unspecified IP and the same port is treated
    /// as self (e.g. `127.0.0.1:P`, `[::1]:P`).  An exact match is also
    /// accepted for non-wildcard binds.
    fn is_self_addr(&self, addr: SocketAddr) -> bool {
        let Some(local) = self.local_addr else {
            return false;
        };
        if local == addr {
            return true;
        }
        if local.port() == addr.port()
            && local.ip().is_unspecified()
            && (addr.ip().is_loopback() || addr.ip().is_unspecified())
        {
            return true;
        }
        false
    }

    /// Get the diffusion mode.
    pub fn diffusion_mode(&self) -> DiffusionMode {
        self.config.diffusion_mode
    }

    /// Add a peer from the topology configuration.
    pub fn add_config_peer(&mut self, addr: SocketAddr) {
        if self.is_self_addr(addr) {
            return;
        }
        self.inner.add_peer(addr, PeerSource::Topology);
    }

    /// Add a local root peer group.
    pub fn add_local_root_group(&mut self, group: LocalRootGroupInfo) {
        for &addr in &group.addrs {
            self.add_config_peer(addr);
        }
        // #871: upsert by (non-empty) name so periodic DNS re-resolution can
        // refresh a group's resolved addresses in place instead of pushing a
        // duplicate every pass. A blank name (legacy callers) always appends.
        if !group.name.is_empty() {
            if let Some(existing) = self
                .local_root_groups
                .iter_mut()
                .find(|g| g.name == group.name)
            {
                *existing = group;
                return;
            }
        }
        self.local_root_groups.push(group);
    }

    /// Get the configured local root groups.
    pub fn local_root_groups(&self) -> &[LocalRootGroupInfo] {
        &self.local_root_groups
    }

    /// Snapshot of all IPs explicitly listed in the static topology
    /// (local root groups). Used by the N2N inbound accept handler to
    /// decide whether a non-public-IP peer is permitted: only IPs an
    /// operator put in the topology file are allowed to connect from
    /// loopback / RFC1918 / link-local addresses; everything else is
    /// rejected to prevent peer-sharing- or routing-based abuse.
    pub fn static_topology_ips(&self) -> std::collections::HashSet<IpAddr> {
        self.local_root_groups
            .iter()
            .flat_map(|g| g.addrs.iter().map(|s| s.ip()))
            .collect()
    }

    /// Add a peer discovered from ledger state.
    ///
    /// Pool-registered relay addresses on the public network must be
    /// publicly routable. Reject any non-public IP (loopback, RFC1918,
    /// link-local, etc.) — accepting them would let a misconfigured or
    /// adversarial pool registration redirect us to internal hosts.
    pub fn add_ledger_peer(&mut self, addr: SocketAddr) {
        if self.is_self_addr(addr) {
            return;
        }
        if is_non_public_ip(addr.ip()) {
            return;
        }
        // #880: reject port 0 — an undialable address the governor would
        // otherwise burn a connect attempt on every promotion tick.
        if addr.port() == 0 {
            return;
        }
        self.inner.add_peer(addr, PeerSource::Ledger);
    }

    /// Add a peer received via PeerSharing.
    ///
    /// Peer-sharing publishes candidate peers across the public diffusion
    /// network. Non-public IPs (loopback, RFC1918, link-local, multicast,
    /// CGNAT, etc.) MUST NEVER be accepted from this source — an
    /// adversarial peer could otherwise advertise internal addresses to
    /// redirect honest nodes onto local intranet hosts. Static-topology
    /// loopback peers (co-located BP+relay) are added via
    /// [`add_local_root_group`] and are not subject to this filter.
    #[allow(dead_code)] // used by networking rewrite
    pub fn add_shared_peer(&mut self, addr: SocketAddr) {
        if self.is_self_addr(addr) {
            return;
        }
        if is_non_public_ip(addr.ip()) {
            return;
        }
        // #880: reject port 0 — an undialable address the governor would
        // otherwise burn a connect attempt on.
        if addr.port() == 0 {
            return;
        }
        self.inner.add_peer(addr, PeerSource::PeerSharing);
    }

    /// Mark a peer as a big ledger peer.
    pub fn add_big_ledger_peer(&mut self, addr: SocketAddr) {
        self.big_ledger_peers.insert(addr);
    }

    /// Clear the big-ledger-peer set so the next ledger-discovery pass can
    /// rebuild it from scratch (#879).
    ///
    /// The BLP set was previously insert-only, so cold-churn/`peer_failed`
    /// forgets and rotating-DNS re-adds under fresh `SocketAddr`s left it
    /// growing unbounded with stale/duplicate membership that distorted
    /// governor decisions. Rebuilding each pass keeps it a faithful snapshot of
    /// the current top-stake relays.
    pub fn clear_big_ledger_peers(&mut self) {
        self.big_ledger_peers.clear();
    }

    /// Mark a peer as a bootstrap peer (topology `bootstrapPeers`). Bootstrap
    /// peers are always trustable and satisfy the `UseBootstrapPeers` HAA path
    /// (see [`Self::haa_satisfied`]).
    pub fn add_bootstrap_peer(&mut self, addr: SocketAddr) {
        self.bootstrap_peer_addrs.insert(addr);
    }

    /// Read-only view of all known big-ledger peers.
    ///
    /// Consumed by the governor to enforce
    /// `target_warm_big_ledger`/`target_hot_big_ledger` minimums separately
    /// from the aggregate peer targets.
    pub fn big_ledger_peers(&self) -> &std::collections::HashSet<SocketAddr> {
        &self.big_ledger_peers
    }

    /// Mark a peer as connected.
    ///
    /// Sets the connection state to `OutboundIdle(Duplex)` or
    /// `InboundIdle(Duplex)` depending on direction. All N2N P2P
    /// connections negotiate `Duplex` data flow.
    pub fn peer_connected(&mut self, addr: &SocketAddr, direction: ConnectionDirection) {
        if self.inner.get_peer(addr).is_none() {
            let source = match direction {
                ConnectionDirection::Inbound => PeerSource::PeerSharing,
                ConnectionDirection::Outbound => PeerSource::Topology,
            };
            self.inner.add_peer(*addr, source);
        }
        self.inner.promote_to_warm(addr);
        let state = match direction {
            ConnectionDirection::Outbound => ConnectionState::OutboundIdle(DataFlow::Duplex),
            ConnectionDirection::Inbound => ConnectionState::InboundIdle(DataFlow::Duplex),
        };
        self.conn_states.insert(*addr, state);
        // Track inbound-duplex arrival for the maturation gate (#703 fix B).
        // Outbound peers always enter via known-topology / ledger / DNS
        // sources, so they have already had time to be evaluated.  Inbound
        // peers can arrive from anywhere on the public internet and need
        // the 15-min cooling-off before becoming eligible for hot
        // promotion.  Local-root inbound peers are exempt — the topology
        // file is the operator's trusted whitelist and hot_valency must be
        // honoured immediately.
        if direction == ConnectionDirection::Inbound
            && !self
                .local_root_groups
                .iter()
                .any(|g| g.addrs.iter().any(|a| a == addr))
        {
            self.fresh_inbound
                .entry(*addr)
                .or_insert_with(std::time::Instant::now);
        }
    }

    /// Mark a peer as disconnected.
    ///
    /// Demotes the peer to cold and removes connection state. Also emits
    /// `GsmEvent::PeerDisconnected` to the GSM actor (if wired) so that
    /// the LoE and GDD peer tracking are updated.
    ///
    /// Mirrors Haskell's `ConnectionState::TerminatedState` transition.
    /// On the governor side this is the `PeerCooling → PeerCold`
    /// completion (matching `cooling_to_cold`). For paths that bypass
    /// `mark_terminating` (e.g. remote-initiated TCP close detected by
    /// `cleanup_dead_connections`, or peer-forget without prior
    /// terminating), the underlying `demote_to_cold` accepts every
    /// transition source (Warm/Hot/Cooling/Cold), so this is robust
    /// against the entry path.
    pub fn peer_disconnected(&mut self, addr: &SocketAddr) {
        // Fast path: if the peer is in Cooling (mark_terminating was
        // called first), this completes the Cooling → Cold transition
        // cleanly. Otherwise (e.g. detected from cleanup_dead_connections
        // without a prior governor-initiated mark_terminating),
        // demote_to_cold handles Hot/Warm → Cold directly.
        if !self.inner.cooling_to_cold(addr) {
            self.inner.demote_to_cold(addr);
        }
        self.conn_states.remove(addr);
        // Clear any fresh-inbound tracking — if they reconnect we restart
        // the maturation window cleanly.
        self.fresh_inbound.remove(addr);
        // Notify the GSM actor so it can deregister the peer from density tracking.
        if let Some(ref tx) = self.gsm_event_tx {
            if let Err(e) = tx.try_send(crate::gsm::GsmEvent::PeerDisconnected { addr: *addr }) {
                tracing::debug!(%addr, "GSM PeerDisconnected event dropped: {e}");
            }
        }
    }

    /// Record a connection failure.
    ///
    /// Applies exponential backoff via `PeerInfo::record_failure()`. For
    /// non-root peers (Ledger, PeerSharing), permanently removes the peer
    /// after `MAX_COLD_PEER_FAILURES` consecutive failures, matching Haskell's
    /// `policyMaxConnectionRetries = 5` forget policy.
    ///
    /// Topology and Dns peers are never forgotten — they are retried
    /// indefinitely at the 160s backoff cap (matching Haskell local/public
    /// root peer behaviour).
    ///
    /// Repeat calls within [`PEER_FAILURE_COLLAPSE_WINDOW`] collapse into the
    /// first: one teardown can be reported by both the protocol task and the
    /// dead-connection GC, and Haskell backs off per connection *attempt* (#908).
    pub fn peer_failed(&mut self, addr: &SocketAddr) {
        let now = std::time::Instant::now();
        if let Some(&last) = self.last_failure_at.get(addr) {
            if now.duration_since(last) < PEER_FAILURE_COLLAPSE_WINDOW {
                // Same teardown, second reporter — the backoff and forget
                // accounting have already been applied for this attempt.
                // Still ensure the peer is out of the established pool.
                self.conn_states.remove(addr);
                self.inner.demote_to_cold(addr);
                return;
            }
        }
        // Bound the map: entries older than the collapse window carry no
        // information, and cold-churn `ForgetPeer` removes peers via
        // `inner.remove_peer` without passing through here.
        if self.last_failure_at.len() > 1_024 {
            self.last_failure_at
                .retain(|_, &mut t| now.duration_since(t) < PEER_FAILURE_COLLAPSE_WINDOW);
        }
        self.last_failure_at.insert(*addr, now);

        // Check whether this failure pushes the peer over the forget threshold.
        // We read failure_count *before* calling record_failure() (+1 below).
        let should_forget = self.inner.get_peer(addr).is_some_and(|p| {
            p.failure_count + 1 >= MAX_COLD_PEER_FAILURES
                && !matches!(p.source, PeerSource::Topology | PeerSource::Dns)
        });

        if let Some(peer) = self.inner.get_peer_mut(addr) {
            peer.record_failure();
        }

        self.conn_states.remove(addr);

        if should_forget {
            // Non-root peer exceeded max retries — remove from known set entirely.
            // It will only re-appear if re-discovered via ledger or peer sharing.
            self.inner.remove_peer(addr);
            // #879: keep the BLP/bootstrap address sets in sync with the peer
            // table. Leaving a forgotten peer in `big_ledger_peers` bloats the
            // set with stale entries that mis-feed `is_big_ledger_peer` and the
            // demote-exclusion set.
            self.big_ledger_peers.remove(addr);
            self.bootstrap_peer_addrs.remove(addr);
            self.last_failure_at.remove(addr);
        } else {
            self.inner.demote_to_cold(addr);
        }
    }

    /// Mark a connection as duplex (both initiator and responder active).
    ///
    /// Called during simultaneous open detection — an inbound connection arrives
    /// while we already have an outbound connection to the same peer. The
    /// connection transitions to `DuplexConn` matching Haskell's `DuplexState`.
    #[allow(dead_code)] // Will be used when full simultaneous-open handling is implemented
    pub fn mark_peer_duplex(&mut self, addr: &SocketAddr) {
        if self.conn_states.contains_key(addr) {
            self.conn_states.insert(*addr, ConnectionState::DuplexConn);
        }
    }

    /// Transition an outbound idle connection to active (initiator protocols running).
    ///
    /// Called when promoting warm → hot on an outbound connection.
    /// `OutboundIdle(Duplex)` → `OutboundDup`, `OutboundIdle(Unidirectional)` → `OutboundUni`.
    pub fn mark_outbound_active(&mut self, addr: &SocketAddr) {
        if let Some(state) = self.conn_states.get(addr) {
            let new_state = match state {
                ConnectionState::OutboundIdle(DataFlow::Duplex) => ConnectionState::OutboundDup,
                ConnectionState::OutboundIdle(DataFlow::Unidirectional) => {
                    ConnectionState::OutboundUni
                }
                // Already active or duplex — leave unchanged.
                _ => return,
            };
            self.conn_states.insert(*addr, new_state);
        }
    }

    /// Transition an inbound idle connection to active (responder protocols running).
    ///
    /// Called when promoting warm → hot on an inbound connection.
    /// `InboundIdle(df)` → `InboundState(df)`.
    pub fn mark_inbound_active(&mut self, addr: &SocketAddr) {
        if let Some(state) = self.conn_states.get(addr) {
            let new_state = match state {
                ConnectionState::InboundIdle(df) => ConnectionState::InboundState(*df),
                _ => return,
            };
            self.conn_states.insert(*addr, new_state);
        }
    }

    /// Transition a connection to terminating state.
    ///
    /// Called before `conn.shutdown()` during demotion to cold or cleanup.
    /// The connection will be removed via `peer_disconnected()` after shutdown.
    ///
    /// Mirrors Haskell's `ConnectionState::TerminatingState` transition. The
    /// outbound governor's analogue (`PeerStatus::PeerCooling`) is set
    /// here on the `PeerManager` side so `updateUnlessCoolingOrCold`
    /// blocks re-promotion while the shutdown is in flight. The
    /// `PeerCooling → PeerCold` transition fires later in
    /// `peer_disconnected`, which mirrors Haskell's
    /// `TerminatingState → TerminatedState`.
    pub fn mark_terminating(&mut self, addr: &SocketAddr) {
        if self.conn_states.contains_key(addr) {
            self.conn_states
                .insert(*addr, ConnectionState::TerminatingConn);
        }
        // Hot/Warm → Cooling (governor view). No-op for peers that
        // are already in Cold or Cooling (e.g. forget-peer path that
        // hit a previously-failed cold peer).
        self.inner.demote_to_cooling(addr);
    }

    /// Check if a connection is inbound (for directing state transitions).
    pub fn is_inbound(&self, addr: &SocketAddr) -> bool {
        self.conn_states
            .get(addr)
            .and_then(|s| s.provenance())
            .is_some_and(|p| p == Provenance::Inbound)
    }

    /// Record a handshake RTT measurement.
    #[allow(dead_code)] // used by networking rewrite
    pub fn record_handshake_rtt(&mut self, addr: &SocketAddr, rtt_ms: f64) {
        if let Some(peer) = self.inner.get_peer_mut(addr) {
            peer.update_latency(rtt_ms);
        }
    }

    /// Record a measured BlockFetch throughput sample (bytes/second) for a peer
    /// — the bandwidth half of the GSV estimate used to pick the fetch peer.
    pub fn update_peer_fetch_bandwidth(&mut self, addr: &SocketAddr, bytes_per_sec: f64) {
        self.inner.update_fetch_bandwidth(addr, bytes_per_sec);
    }

    /// Record `bytes` delivered by a completed BlockFetch range — the
    /// `fetchynessBytes` metric that ranks hot-demotion candidates while
    /// bulk-syncing (#909).
    pub fn record_peer_fetched_bytes(&mut self, addr: &SocketAddr, bytes: u64) {
        self.inner.record_fetched_bytes(addr, bytes);
    }

    /// Rolling-window bytes fetched from a peer (Haskell `fetchynessBytes`).
    pub fn peer_fetchyness_bytes(&self, addr: &SocketAddr) -> u64 {
        self.inner.get_fetchyness_bytes(addr)
    }

    /// Whether `addr` should contest the single fetch slot right now, ranking it
    /// against the current HOT peers by GSV (fetch bandwidth). Self-contained
    /// candidate derivation for the BlockFetch worker's claim loop.
    pub fn should_claim_fetch_slot(&self, addr: &SocketAddr, top_k: usize) -> bool {
        use dugite_network::peer::PeerState;
        let candidates = self.inner.peers_in_state(PeerState::Hot);
        self.inner.is_preferred_fetch_peer(addr, &candidates, top_k)
    }

    /// Record blocks fetched from a peer.
    #[allow(dead_code)] // used by networking rewrite
    pub fn record_block_fetch(&mut self, addr: &SocketAddr, blocks: usize) {
        if let Some(peer) = self.inner.get_peer_mut(addr) {
            peer.record_success();
            let _ = blocks; // future: track per-peer block counts
        }
    }

    /// Collect current EWMA latency values (ms) for all connected peers
    /// (warm or hot) that have at least one RTT measurement.
    pub fn connected_peer_latencies(&self) -> Vec<f64> {
        use dugite_network::peer::PeerState;
        self.inner
            .peers_in_state(PeerState::Warm)
            .iter()
            .chain(self.inner.peers_in_state(PeerState::Hot).iter())
            .filter_map(|addr| self.inner.get_peer(addr).and_then(|p| p.latency_ms))
            .collect()
    }

    /// Recompute reputation scores for all peers.
    pub fn recompute_reputations(&mut self) {
        self.inner.decay_all_failures();
    }

    // ─── Counting ───

    pub fn cold_peer_count(&self) -> usize {
        self.inner.count_by_state(PeerState::Cold)
    }
    pub fn warm_peer_count(&self) -> usize {
        self.inner.count_by_state(PeerState::Warm)
    }
    pub fn hot_peer_count(&self) -> usize {
        self.inner.count_by_state(PeerState::Hot)
    }
    /// Count outbound connections (including DuplexConn, which counts as both).
    pub fn outbound_peer_count(&self) -> usize {
        self.conn_states
            .values()
            .filter(|s| {
                matches!(
                    s,
                    ConnectionState::OutboundIdle(_)
                        | ConnectionState::OutboundUni
                        | ConnectionState::OutboundDup
                        | ConnectionState::DuplexConn
                )
            })
            .count()
    }
    /// Count inbound connections (including DuplexConn, which counts as both).
    pub fn inbound_peer_count(&self) -> usize {
        self.conn_states
            .values()
            .filter(|s| {
                matches!(
                    s,
                    ConnectionState::InboundIdle(_)
                        | ConnectionState::InboundState(_)
                        | ConnectionState::DuplexConn
                )
            })
            .count()
    }
    /// Count duplex connections (negotiated Duplex DataFlow or in DuplexConn state).
    pub fn duplex_peer_count(&self) -> usize {
        self.conn_states
            .values()
            .filter(|s| s.data_flow() == Some(DataFlow::Duplex))
            .count()
    }

    /// Compute aggregated connection manager counters matching Haskell's
    /// `ConnectionManagerCounters`.
    pub fn connection_manager_counters(&self) -> ConnectionManagerCounters {
        self.conn_states.values().map(|s| s.to_counters()).sum()
    }
    pub fn active_big_ledger_peer_count(&self) -> usize {
        // Haskell HAA: `activeNumBigLedgerPeers >= minNumberOfBigLedgerPeers`
        // counts ACTIVE (hot) big-ledger connections only — a warm BLP is
        // established but not serving ChainSync and gives no availability
        // guarantee (audit gsm-07/blockfetch-08).
        self.big_ledger_peers
            .iter()
            .filter(|addr| {
                self.inner
                    .get_peer(addr)
                    .is_some_and(|p| p.state == PeerState::Hot)
            })
            .count()
    }

    /// The set of trusted local-root addresses (topology `localRoots`).
    fn local_root_addrs(&self) -> std::collections::HashSet<SocketAddr> {
        self.local_root_groups
            .iter()
            .flat_map(|g| g.addrs.iter().copied())
            .collect()
    }

    /// The trusted external peer set: bootstrap peers ∪ local roots
    /// (Haskell `viewEstablishedBootstrapPeers ∪ trustableLocalRootSet`).
    ///
    /// This is the SAME set `haa_satisfied` checks its closure against and
    /// the set the governor's sync-time trusted-only clamp
    /// (`Governor::set_sync_trusted_restriction`) establishes from — the two
    /// must stay in lockstep or the HAA closure becomes unsatisfiable again.
    pub fn trusted_peer_addrs(&self) -> std::collections::HashSet<SocketAddr> {
        let mut trusted = self.local_root_addrs();
        trusted.extend(self.bootstrap_peer_addrs.iter().copied());
        trusted
    }

    /// The set of OUTBOUND established (warm + hot) peers that are NOT in
    /// the trusted set (bootstrap peers ∪ trustable local roots).
    ///
    /// This is exactly the HAA clause-(b) failure set (Haskell
    /// `viewEstablishedPeers \ (viewEstablishedBootstrapPeers ∪
    /// trustableLocalRootSet)`) — factored out so [`Self::haa_satisfied`]
    /// and the governor's self-healing demotion sweep (#920) can never
    /// diverge on which peers violate the trusted-only clamp. `haa_satisfied`
    /// clause (b) fails if and only if this returns non-empty.
    ///
    /// Used both by the per-tick governor enforcement (which demotes every
    /// entry straight to Cold while the sync-time clamp holds — see
    /// `Governor::compute_actions_with_blp`) and by the one-shot sweep run
    /// on the CaughtUp→Syncing/PreSyncing boundary edge.
    pub fn untrusted_established_outbound(&self) -> Vec<SocketAddr> {
        let trusted = self.trusted_peer_addrs();
        let is_outbound = |a: &SocketAddr| {
            matches!(
                self.conn_states.get(a),
                Some(
                    ConnectionState::OutboundIdle(_)
                        | ConnectionState::OutboundUni
                        | ConnectionState::OutboundDup
                        | ConnectionState::DuplexConn
                )
            )
        };
        let mut established = self.inner.peers_in_state(PeerState::Warm);
        established.extend(self.inner.peers_in_state(PeerState::Hot));
        established
            .into_iter()
            .filter(|a| is_outbound(a) && !trusted.contains(a))
            .collect()
    }

    /// Honest-Availability-Assumption satisfaction (Haskell
    /// `outboundConnectionsState` → `TrustedStateWithExternalPeers`).
    ///
    /// Mirrors Haskell's INDEPENDENT case split over `(associationMode,
    /// bootstrapPeersFlag, consensusMode)` — cardano-diffusion
    /// `Cardano.Network.PeerSelection.Governor.Types.outboundConnectionsState`
    /// at rev a98c885 (the exact pin cardano-node 11.0.1 builds against).
    /// The branches are genuinely alternative code paths, NOT layered
    /// AND/OR clauses (#933 — the pre-#933 layering ran the BLP count
    /// first and fell through to the bootstrap clauses, which made the BLP
    /// criterion structurally unreachable during clamped Genesis sync and
    /// let a Genesis node with bootstrap peers "fall through" between
    /// criteria in ways Haskell cannot):
    ///
    /// - `(LocalRootsOnly, _, _)` — NOT APPLICABLE: dugite has no
    ///   LocalRootsOnly association mode (no configuration restricts
    ///   dialing to local roots only — ledger-peer discovery and peer
    ///   sharing are always available), so dugite's `associationMode` is
    ///   always `Unrestricted`. Implement this branch (established ⊆
    ///   trustable local roots) if such a mode is ever introduced.
    /// - `(Unrestricted, UseBootstrapPeers{}, _)` — bootstrap peers
    ///   configured, EITHER consensus mode: every OUTBOUND established
    ///   (warm+hot) peer ∈ bootstrap ∪ trustable local roots AND ≥1 hot
    ///   outbound trusted peer — see [`Self::haa_satisfied_via_bootstrap`].
    ///   This is how a from-genesis node (whose ledger has not reached
    ///   `useLedgerAfterSlot`, so no big-ledger peers are classified)
    ///   satisfies the HAA via its bootstrap relays; without it such a
    ///   node stalls permanently in PreSyncing.
    /// - `(Unrestricted, DontUseBootstrapPeers, PraosMode)` →
    ///   `UntrustedState`, unconditionally — and SILENTLY: this is normal
    ///   cardano-node operation (nothing in Haskell warns here), see #931.
    /// - `(Unrestricted, DontUseBootstrapPeers, GenesisMode)` → satisfied
    ///   iff ≥ `min_active_blp` ACTIVE (hot) big-ledger peers, and NOTHING
    ///   else — Haskell's branch reads only `activeNumBigLedgerPeers`,
    ///   ignoring `viewEstablishedPeers` and local-root trust completely
    ///   (big-ledger peers ARE the trust source in this mode).
    pub fn haa_satisfied(&self, min_active_blp: usize) -> bool {
        // bootstrapPeersFlag ≙ topology `bootstrapPeers` resolved to a
        // non-empty address set; consensusMode ≙ the startup mirror set by
        // `set_genesis_mode`. associationMode is always Unrestricted (see
        // the doc comment above).
        let use_bootstrap_peers = !self.bootstrap_peer_addrs.is_empty();
        match (use_bootstrap_peers, self.genesis_mode()) {
            // (Unrestricted, UseBootstrapPeers{}, _)
            (true, _) => self.haa_satisfied_via_bootstrap(),
            // (Unrestricted, DontUseBootstrapPeers, PraosMode) →
            // UntrustedState. Silent — Haskell treats this as normal
            // operation; at most a throttled debug for observability.
            (false, false) => {
                if self.haa_warn_permitted() {
                    tracing::debug!(
                        "HAA not satisfied: Praos mode without bootstrap \
                         peers is UntrustedState unconditionally (Haskell \
                         outboundConnectionsState — normal, silent)"
                    );
                }
                false
            }
            // (Unrestricted, DontUseBootstrapPeers, GenesisMode) →
            // active-BLP quorum only.
            (false, true) => self.active_big_ledger_peer_count() >= min_active_blp,
        }
    }

    /// The `(Unrestricted, UseBootstrapPeers{}, _)` branch of
    /// [`Self::haa_satisfied`]: every OUTBOUND established (warm+hot) peer
    /// must be in `bootstrap ∪ trustable local roots` (Haskell
    /// `viewEstablishedPeers ⊆ viewEstablishedBootstrapPeers <>
    /// trustableLocalRootSet`) AND at least one hot outbound peer must be
    /// trusted. Note the deliberate (pre-#933, kept verbatim) widening of
    /// the second condition vs Haskell's `not (Set.null
    /// viewActiveBootstrapPeers)`: dugite accepts a hot trustable LOCAL
    /// ROOT too, not only a hot bootstrap peer.
    ///
    /// Carries the #931 clamp-gated clause (a)/(b) failure diagnostics —
    /// they only ever made sense in this branch (the clamp exists exactly
    /// when this branch's closure is being enforced by the governor).
    fn haa_satisfied_via_bootstrap(&self) -> bool {
        // Trusted external set = bootstrap peers ∪ trustable local roots
        // (Haskell `viewEstablishedBootstrapPeers ∪ trustableLocalRootSet`).
        // Kept in lockstep with the governor's sync-time trusted-only clamp
        // via the shared `trusted_peer_addrs()` — see that method's docs.
        // Never empty here: this branch requires `bootstrap_peer_addrs`
        // non-empty, and the trusted set is a superset of it.
        let trusted = self.trusted_peer_addrs();
        // Haskell `outboundConnectionsState` assesses only OUTBOUND-initiated
        // connections — peers that connect TO us (inbound) are not part of
        // the honest-availability assessment of our own chain. Without this
        // restriction an inbound peer (e.g. a downstream node) flaps the HAA.
        let is_outbound = |a: &SocketAddr| {
            matches!(
                self.conn_states.get(a),
                Some(
                    ConnectionState::OutboundIdle(_)
                        | ConnectionState::OutboundUni
                        | ConnectionState::OutboundDup
                        | ConnectionState::DuplexConn
                )
            )
        };
        // At least one ACTIVE (hot) outbound BOOTSTRAP peer — Haskell
        // `not (Set.null viewActiveBootstrapPeers)` is specific to bootstrap
        // peers; a hot trustable local root satisfies the CLOSURE clause but
        // not this one (tightened to exact Haskell semantics in #933's
        // integration pass — previously any hot trusted peer counted).
        let hot_peers = self.inner.peers_in_state(PeerState::Hot);
        let any_hot_trusted = hot_peers
            .iter()
            .any(|a| is_outbound(a) && self.bootstrap_peer_addrs.contains(a));
        // #931: the clause (a)/(b) failure diagnostics WARN only while the
        // sync-time trusted-only clamp is actually in force. Haskell
        // reference: `outboundConnectionsState`'s `(Unrestricted,
        // DontUseBootstrapPeers, PraosMode)` branch is `UntrustedState` — a
        // normal, SILENT state in cardano-node (nothing warns about
        // untrusted established peers there). Praos mode (the dugite
        // default) never has a clamp, so an unconditional WARN here fired on
        // perfectly normal ledger-peer establishment.
        let diag_level = haa_diagnostic_level(self.sync_trusted_clamp_active());
        if !any_hot_trusted {
            if self.haa_warn_permitted() {
                match diag_level {
                    HaaDiagnosticLevel::Warn => tracing::warn!(
                        hot_total = hot_peers.len(),
                        trusted_total = trusted.len(),
                        hot_sample = ?hot_peers.iter().take(5).collect::<Vec<_>>(),
                        "HAA clause (a) failed while the sync-time \
                         trusted-only clamp is active: no hot outbound peer \
                         is in the trusted set (bootstrap ∪ local roots)"
                    ),
                    HaaDiagnosticLevel::Debug => tracing::debug!(
                        hot_total = hot_peers.len(),
                        trusted_total = trusted.len(),
                        hot_sample = ?hot_peers.iter().take(5).collect::<Vec<_>>(),
                        "HAA clause (a) not satisfied: no hot outbound peer \
                         in the trusted set (bootstrap ∪ local roots) — no \
                         sync-time clamp active; normal outside clamped \
                         genesis-mode sync (Haskell UntrustedState is silent)"
                    ),
                }
            }
            return false;
        }
        // Every OUTBOUND established (warm + hot) peer must be trusted (Haskell
        // `viewEstablishedPeers ⊆ viewEstablishedBootstrapPeers ∪ trustableLocalRootSet`).
        let untrusted_established = self.untrusted_established_outbound();
        if !untrusted_established.is_empty() {
            if self.haa_warn_permitted() {
                match diag_level {
                    HaaDiagnosticLevel::Warn => tracing::warn!(
                        untrusted_count = untrusted_established.len(),
                        trusted_total = trusted.len(),
                        untrusted_sample =
                            ?untrusted_established.iter().take(5).collect::<Vec<_>>(),
                        "HAA clause (b) failed: untrusted outbound peer(s) \
                         are established while the sync-time trusted-only \
                         clamp is active — the per-tick governor sweep \
                         (#920) should demote them next tick"
                    ),
                    HaaDiagnosticLevel::Debug => tracing::debug!(
                        untrusted_count = untrusted_established.len(),
                        trusted_total = trusted.len(),
                        untrusted_sample =
                            ?untrusted_established.iter().take(5).collect::<Vec<_>>(),
                        "HAA clause (b) not satisfied: untrusted outbound \
                         peer(s) established with no sync-time clamp active \
                         — normal ledger-peer operation (Haskell \
                         UntrustedState is silent in Praos mode)"
                    ),
                }
            }
            return false;
        }
        true
    }

    /// Mirror of the governor tick's sync-time trusted-only clamp state
    /// (#931): `active` = `compute_sync_trusted_restriction(...)` returned
    /// `Some`. Diagnostics-only — see the `sync_trusted_clamp_active` field
    /// docs; this never feeds back into clamp enforcement or the
    /// `haa_satisfied` return value.
    pub fn set_sync_trusted_clamp_active(&self, active: bool) {
        self.sync_trusted_clamp_active
            .store(active, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the sync-time trusted-only clamp was in force as of the last
    /// governor tick (#931). Diagnostics-only.
    fn sync_trusted_clamp_active(&self) -> bool {
        self.sync_trusted_clamp_active
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Mirror of the node's consensus mode (`ConsensusMode = Genesis` ⇒
    /// `true`), set once at startup from `node/mod.rs` (#933). This is the
    /// `consensusMode` dimension of the Haskell `outboundConnectionsState`
    /// case split in [`Self::haa_satisfied`]; it is never flipped at
    /// runtime (cardano-node's consensus mode is a boot-time constant too).
    pub fn set_genesis_mode(&self, genesis: bool) {
        self.genesis_mode
            .store(genesis, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the node runs in Ouroboros Genesis consensus mode (#933).
    fn genesis_mode(&self) -> bool {
        self.genesis_mode.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Rate-limit gate for the `haa_satisfied` failure diagnostics: at most
    /// one per 30 s (the predicate runs on several hot paths per governor
    /// tick).
    fn haa_warn_permitted(&self) -> bool {
        use std::sync::atomic::Ordering;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = self.haa_warn_last_secs.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 30 {
            return false;
        }
        self.haa_warn_last_secs
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Get connected peer addresses.
    pub fn connected_peer_addrs(&self) -> Vec<SocketAddr> {
        self.conn_states.keys().copied().collect()
    }

    /// Get the category of a peer.
    #[allow(dead_code)] // used by networking rewrite
    pub fn peer_category(&self, addr: &SocketAddr) -> Option<PeerCategory> {
        self.inner.get_peer(addr)?;
        for group in &self.local_root_groups {
            if group.addrs.contains(addr) {
                return Some(PeerCategory::LocalRoot);
            }
        }
        if self.big_ledger_peers.contains(addr) {
            return Some(PeerCategory::BigLedgerPeer);
        }
        Some(PeerCategory::Normal)
    }

    /// Find an inbound duplex connection from the same IP.
    #[allow(dead_code)] // used by networking rewrite
    pub fn find_inbound_duplex_by_ip(&self, ip: std::net::IpAddr) -> Option<SocketAddr> {
        self.conn_states
            .iter()
            .find(|(addr, state)| {
                addr.ip() == ip
                    && matches!(
                        state,
                        ConnectionState::InboundIdle(DataFlow::Duplex)
                            | ConnectionState::InboundState(DataFlow::Duplex)
                            | ConnectionState::DuplexConn
                    )
            })
            .map(|(addr, _)| *addr)
    }

    /// Get the effective diffusion mode for a specific peer.
    ///
    /// If the peer belongs to a local root group with an explicit diffusion mode,
    /// that override is used. Otherwise, falls back to the node-level config.
    #[allow(dead_code)] // will be used by P2P governor handshake logic
    pub fn effective_diffusion_mode(&self, addr: &SocketAddr) -> DiffusionMode {
        for group in &self.local_root_groups {
            if group.addrs.contains(addr) {
                if let Some(mode) = group.diffusion_mode {
                    return mode;
                }
            }
        }
        self.config.diffusion_mode
    }

    /// Whether a peer is behind a firewall (should not initiate outbound connections).
    #[allow(dead_code)] // will be used by P2P governor connection logic
    pub fn is_behind_firewall(&self, addr: &SocketAddr) -> bool {
        self.local_root_groups
            .iter()
            .any(|g| g.behind_firewall && g.addrs.contains(addr))
    }

    /// Whether a peer can be shared via the PeerSharing protocol.
    /// Returns false for peers in local root groups with advertise=false.
    #[allow(dead_code)] // will be used by PeerSharing protocol
    pub fn is_advertisable(&self, addr: &SocketAddr) -> bool {
        for group in &self.local_root_groups {
            if group.addrs.contains(addr) {
                return group.advertise;
            }
        }
        true // non-topology peers are advertisable by default
    }

    /// Summary statistics.
    pub fn stats(&self) -> PeerManagerStats {
        PeerManagerStats {
            cold: self.cold_peer_count(),
            warm: self.warm_peer_count(),
            hot: self.hot_peer_count(),
            outbound: self.outbound_peer_count(),
            inbound: self.inbound_peer_count(),
            duplex: self.duplex_peer_count(),
            big_ledger: self.active_big_ledger_peer_count(),
        }
    }
}

/// Summary statistics from the node peer manager.
#[derive(Debug, Clone, Default)]
pub struct PeerManagerStats {
    pub cold: usize,
    pub warm: usize,
    pub hot: usize,
    pub outbound: usize,
    pub inbound: usize,
    pub duplex: usize,
    pub big_ledger: usize,
}

impl std::fmt::Display for PeerManagerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cold={} warm={} hot={} out={} in={} duplex={} blp={}",
            self.cold,
            self.warm,
            self.hot,
            self.outbound,
            self.inbound,
            self.duplex,
            self.big_ledger,
        )
    }
}

// ─── #931 — HAA failure-diagnostic severity ──────────────────────────────────

/// Severity for [`NodePeerManager::haa_satisfied`]'s clause (a)/(b) failure
/// diagnostics.
///
/// WARN is reserved for the one state where the failure is actionable: the
/// sync-time trusted-only clamp is in force, so an untrusted established
/// outbound peer should not (or should no longer) be there — the #920
/// per-tick governor sweep demotes it on the next tick. In every other
/// state an unsatisfied HAA is normal operation: Haskell's
/// `outboundConnectionsState` maps `(Unrestricted, DontUseBootstrapPeers,
/// PraosMode)` to `UntrustedState` unconditionally, and cardano-node is
/// silent about it (issue #931) — dugite must not WARN there either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HaaDiagnosticLevel {
    /// Sync-time trusted-only clamp active — operator-relevant WARN.
    Warn,
    /// No clamp in force (e.g. Praos mode, or Genesis mode at CaughtUp) —
    /// normal state, debug-level visibility only.
    Debug,
}

/// Pure severity decision for the HAA failure diagnostics — factored out of
/// [`NodePeerManager::haa_satisfied`] so tests can pin the mapping (#931).
fn haa_diagnostic_level(sync_trusted_clamp_active: bool) -> HaaDiagnosticLevel {
    if sync_trusted_clamp_active {
        HaaDiagnosticLevel::Warn
    } else {
        HaaDiagnosticLevel::Debug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    // ─── #908 — failure accounting on unexpected connection death ──────────

    /// A dead-connection reap must apply real backoff. Pre-#908 that path
    /// called only `peer_disconnected()`, so `next_connect_after` stayed unset
    /// and the governor's `eligible_cold` re-offered the peer on the very next
    /// tick — the observed 2-6 s reconnect loop.
    #[test]
    fn peer_failed_arms_the_reconnect_backoff() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.9:3001".parse().unwrap();
        pm.inner.add_peer(addr, PeerSource::Ledger);
        pm.peer_connected(&addr, ConnectionDirection::Outbound);

        // The pre-#908 reap path: no backoff, immediately re-offered.
        pm.peer_disconnected(&addr);
        assert!(
            pm.inner.peers_eligible_to_connect().contains(&addr),
            "precondition: a bare disconnect leaves the peer instantly eligible"
        );

        pm.peer_failed(&addr);
        assert_eq!(pm.inner.get_peer(&addr).unwrap().failure_count, 1);
        assert!(
            !pm.inner.peers_eligible_to_connect().contains(&addr),
            "a recorded failure must hold the peer out for its backoff window"
        );
    }

    // ─── #920 — trusted-only clamp demotion must NOT charge backoff ───────

    /// The clamp-reactivation demotion sweep is a planned policy teardown,
    /// not a connection failure — the peer must be immediately re-eligible
    /// the moment the clamp lifts. Reconciling through `peer_disconnected`
    /// (as the sweep and the governor's `DemoteToCold` handler both do) must
    /// leave `next_connect_after` unset and `failure_count` untouched, unlike
    /// `peer_failed` (mirrors `peer_failed_arms_the_reconnect_backoff` above,
    /// asserting the opposite outcome).
    #[test]
    fn trusted_clamp_demotion_does_not_arm_backoff() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.14:3001".parse().unwrap();
        pm.inner.add_peer(addr, PeerSource::Ledger);
        pm.peer_connected(&addr, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&addr);

        pm.peer_disconnected(&addr);

        assert_eq!(
            pm.inner.get_peer(&addr).unwrap().failure_count,
            0,
            "a clamp-driven demotion must not increment the failure count"
        );
        assert!(
            pm.inner
                .get_peer(&addr)
                .unwrap()
                .next_connect_after
                .is_none(),
            "a clamp-driven demotion must not arm the reconnect backoff"
        );
        assert!(
            pm.inner.peers_eligible_to_connect().contains(&addr),
            "the peer must be immediately re-eligible once the clamp lifts"
        );
    }

    /// One teardown can be reported twice — by the protocol task AND by the
    /// dead-connection GC. Haskell backs off per connection *attempt*, so the
    /// second report must not double the exponent or the forget counter.
    #[test]
    fn peer_failed_collapses_duplicate_reports_for_one_teardown() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.10:3001".parse().unwrap();
        pm.inner.add_peer(addr, PeerSource::Ledger);

        pm.peer_failed(&addr);
        pm.peer_failed(&addr);
        pm.peer_failed(&addr);
        assert_eq!(
            pm.inner.get_peer(&addr).unwrap().failure_count,
            1,
            "three reports of the same teardown are one failed attempt"
        );
        assert!(
            !pm.inner.peers_eligible_to_connect().contains(&addr),
            "the collapsed reports must still leave the backoff armed"
        );
    }

    /// The forget policy (`MAX_COLD_PEER_FAILURES`) must count attempts, not
    /// reports — otherwise the doubled accounting from the GC + protocol-task
    /// pair would evict non-root peers after ~3 real failures instead of 5.
    #[test]
    fn duplicate_failure_reports_do_not_accelerate_the_forget_policy() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.11:3001".parse().unwrap();
        pm.inner.add_peer(addr, PeerSource::Ledger);

        // Two reports per teardown, three teardowns.
        for _ in 0..3 {
            pm.peer_failed(&addr);
            pm.peer_failed(&addr);
            // Age past the collapse window so the next pair is a new attempt.
            pm.last_failure_at.remove(&addr);
        }
        assert_eq!(pm.inner.get_peer(&addr).unwrap().failure_count, 3);
        assert!(
            pm.inner.get_peer(&addr).is_some(),
            "a Ledger peer must survive 3 attempts (forget threshold is 5)"
        );
    }

    // ─── #909 — fetchynessBytes accounting ────────────────────────────────

    /// BlockFetch deliveries must land in the rolling window that ranks
    /// hot-demotion candidates during bulk sync.
    #[test]
    fn fetched_bytes_are_recorded_per_peer() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let busy: SocketAddr = "203.0.113.12:3001".parse().unwrap();
        let idle: SocketAddr = "203.0.113.13:3001".parse().unwrap();
        pm.inner.add_peer(busy, PeerSource::Ledger);
        pm.inner.add_peer(idle, PeerSource::Ledger);

        pm.record_peer_fetched_bytes(&busy, 2_048_000);
        pm.record_peer_fetched_bytes(&busy, 1_024_000);

        assert_eq!(pm.peer_fetchyness_bytes(&busy), 3_072_000);
        assert_eq!(pm.peer_fetchyness_bytes(&idle), 0);
        assert_eq!(
            pm.peer_fetchyness_bytes(&"203.0.113.99:3001".parse().unwrap()),
            0,
            "an unknown peer contributes nothing"
        );
    }

    /// #871: re-adding a named local-root group (periodic DNS re-resolution)
    /// must UPSERT it in place — refreshing its addresses — not push a
    /// duplicate. A blank-named group always appends (legacy behaviour).
    #[test]
    fn add_local_root_group_upserts_by_name() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let a1: SocketAddr = "203.0.113.1:3001".parse().unwrap();
        let a2: SocketAddr = "203.0.113.2:3001".parse().unwrap();

        pm.add_local_root_group(LocalRootGroupInfo {
            name: "local-root-0".into(),
            addrs: vec![a1],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        assert_eq!(pm.local_root_groups().len(), 1);

        // Re-resolution returns a rotated address for the SAME group name.
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "local-root-0".into(),
            addrs: vec![a2],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        assert_eq!(
            pm.local_root_groups().len(),
            1,
            "same-named group must be replaced, not duplicated"
        );
        assert_eq!(
            pm.local_root_groups()[0].addrs,
            vec![a2],
            "group addresses must be refreshed to the newly-resolved set"
        );

        // A distinct name is a distinct group.
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "local-root-1".into(),
            addrs: vec![a1],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        assert_eq!(pm.local_root_groups().len(), 2);
    }

    #[test]
    fn test_haa_local_roots_alone_do_not_satisfy_without_bootstrap() {
        // #933: with NO bootstrap peers configured, the case split lands in
        // the `DontUseBootstrapPeers` branches — Praos is UntrustedState
        // unconditionally, so a hot trustable local root alone can no longer
        // satisfy the HAA. Haskell `outboundConnectionsState`:
        // `(Unrestricted, DontUseBootstrapPeers, PraosMode) -> UntrustedState`
        // (the local-root closure only appears in the LocalRootsOnly and
        // UseBootstrapPeers branches, neither of which applies here).
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let root: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "relays".into(),
            addrs: vec![root],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        pm.peer_connected(&root, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&root);
        assert!(
            !pm.haa_satisfied(5),
            "Praos + DontUseBootstrapPeers is UntrustedState regardless of \
             hot trusted local roots"
        );
    }

    #[test]
    fn test_haa_bootstrap_branch_requires_active_bootstrap_peer_specifically() {
        // Haskell `(Unrestricted, UseBootstrapPeers{}, _)` requires
        // `not (Set.null viewActiveBootstrapPeers)` — an ACTIVE (hot)
        // BOOTSTRAP peer specifically. A hot trustable local root keeps the
        // closure clause happy but does NOT meet the active requirement.
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        let root: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "relays".into(),
            addrs: vec![root],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        // Warm local root only → not satisfied (needs an ACTIVE peer).
        pm.peer_connected(&root, ConnectionDirection::Outbound);
        assert!(!pm.haa_satisfied(5));
        // Hot LOCAL ROOT alone → still not satisfied: Haskell demands the
        // active peer be a BOOTSTRAP peer (`viewActiveBootstrapPeers`).
        pm.inner.promote_to_hot(&root);
        assert!(
            !pm.haa_satisfied(5),
            "hot local root must not satisfy the active-bootstrap requirement"
        );
        // Hot BOOTSTRAP peer → satisfied (closure holds: both are trusted).
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);
        assert!(pm.haa_satisfied(5));
    }

    #[test]
    fn test_haa_ignores_inbound_peers() {
        // An INBOUND connection (a downstream node connecting to us) must not
        // affect the HAA — Haskell's outboundConnectionsState assesses only
        // outbound peers. Otherwise a relay's inbound downstream flaps the HAA.
        // (Bootstrap-configured branch: the only branch with an established-
        // peer closure the inbound peer could otherwise violate.)
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        let downstream: SocketAddr = "127.0.0.1:3003".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);
        // A non-trusted INBOUND peer is established but must be ignored.
        pm.peer_connected(&downstream, ConnectionDirection::Inbound);
        pm.inner.promote_to_hot(&downstream);
        assert!(
            pm.haa_satisfied(5),
            "inbound downstream must not break the HAA"
        );
    }

    #[test]
    fn test_haa_not_satisfied_with_untrusted_hot_peer() {
        // A hot peer that is NOT a local root (e.g. a public/ledger peer)
        // does not satisfy the HAA in any no-bootstrap branch.
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let root: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "relays".into(),
            addrs: vec![root],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        pm.peer_connected(&public, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&public);
        // OUTBOUND established set includes a non-local-root hot peer → not all
        // trusted, and the hot peer is not a local root → not satisfied.
        assert!(!pm.haa_satisfied(5));
        // No local roots at all → never satisfied via that path.
        let mut pm2 = NodePeerManager::new(PeerManagerConfig::default());
        pm2.peer_connected(&public, ConnectionDirection::Outbound);
        pm2.inner.promote_to_hot(&public);
        assert!(!pm2.haa_satisfied(5));
    }

    // ─── #920 — untrusted_established_outbound() enumeration helper ───────

    /// The helper must return an established (warm or hot) OUTBOUND public
    /// peer that is not in the trusted set.
    #[test]
    fn untrusted_established_outbound_returns_outbound_public_peer() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.peer_connected(&public, ConnectionDirection::Outbound);
        assert_eq!(pm.untrusted_established_outbound(), vec![public]);

        pm.inner.promote_to_hot(&public);
        assert_eq!(
            pm.untrusted_established_outbound(),
            vec![public],
            "a hot untrusted outbound peer must also be enumerated"
        );
    }

    /// Inbound peers are never part of the HAA's outbound assessment, and
    /// trusted peers (bootstrap ∪ local roots) are by definition not a
    /// violation — both must be excluded from the enumeration.
    #[test]
    fn untrusted_established_outbound_excludes_inbound_and_trusted() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());

        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);

        let downstream: SocketAddr = "8.8.4.4:3001".parse().unwrap();
        pm.peer_connected(&downstream, ConnectionDirection::Inbound);

        assert!(
            pm.untrusted_established_outbound().is_empty(),
            "trusted outbound peer and untrusted INBOUND peer must both be excluded"
        );
    }

    /// Property: `haa_satisfied` clause (b) fails if and only if the
    /// enumeration helper is non-empty. The two must never diverge — the
    /// governor's self-healing demotion sweep (#920) uses the helper as its
    /// candidate set, and `haa_satisfied` uses it to decide clause (b).
    #[test]
    fn untrusted_established_outbound_matches_haa_clause_b() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);

        // Before: clause (a)+(b) both satisfied, helper empty.
        assert!(pm.untrusted_established_outbound().is_empty());
        assert!(pm.haa_satisfied(5));

        // After establishing an untrusted outbound peer: helper non-empty,
        // and haa_satisfied's clause (b) must now fail (overall false).
        let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.peer_connected(&public, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&public);
        assert!(!pm.untrusted_established_outbound().is_empty());
        assert!(!pm.haa_satisfied(5));
    }

    #[test]
    fn test_haa_satisfied_via_bootstrap_peers() {
        // From-genesis genesis-mode path (Haskell UseBootstrapPeers →
        // TrustedStateWithExternalPeers): HAA holds when there is ≥1 hot
        // bootstrap peer AND every established outbound peer is trusted, with
        // ZERO big-ledger peers and NO local roots. This is exactly what a
        // from-genesis node needs to leave PreSyncing; without the bootstrap
        // term in the trusted set it stalls forever (the genesis bulk-sync bug).
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        // Warm only → not satisfied (Haskell needs an ACTIVE/hot peer).
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        assert!(!pm.haa_satisfied(5));
        // Hot outbound bootstrap peer → satisfied (no BLPs, no local roots).
        pm.inner.promote_to_hot(&boot);
        assert!(
            pm.haa_satisfied(5),
            "a hot bootstrap peer satisfies the UseBootstrapPeers HAA path"
        );
        // Condition 2 (established ⊆ trusted): an untrusted outbound hot peer
        // must break the HAA, mirroring Haskell outboundConnectionsState.
        let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.peer_connected(&public, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&public);
        assert!(
            !pm.haa_satisfied(5),
            "an established untrusted outbound peer must break the HAA"
        );
    }

    // ─── #931 — HAA failure diagnostics gated on the sync-time clamp ──────

    /// Captures `(level, message)` for every tracing event this module emits
    /// while `f` runs. Same lightweight local-subscriber pattern as
    /// `logging.rs::test_reload_filter_swaps_live` — nextest's
    /// process-per-test isolation means no global subscriber interferes.
    fn capture_networking_events(f: impl FnOnce()) -> Vec<(tracing::Level, String)> {
        use std::sync::{Arc, Mutex};
        use tracing::Subscriber;
        use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
        use tracing_subscriber::registry::Registry;

        struct MsgVisitor(Option<String>);
        impl tracing::field::Visit for MsgVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }

        struct CaptureLayer(Arc<Mutex<Vec<(tracing::Level, String)>>>);
        impl<S: Subscriber> Layer<S> for CaptureLayer {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                if event.metadata().target() == "dugite_node::node::networking" {
                    let mut v = MsgVisitor(None);
                    event.record(&mut v);
                    self.0
                        .lock()
                        .unwrap()
                        .push((*event.metadata().level(), v.0.unwrap_or_default()));
                }
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default().with(CaptureLayer(Arc::clone(&captured)));
        tracing::subscriber::with_default(subscriber, f);
        let events = captured.lock().unwrap().clone();
        events
    }

    /// A peer manager whose clause (b) fails: one hot trusted bootstrap peer
    /// (clause (a) satisfied) plus one established untrusted public peer.
    fn pm_with_clause_b_failure() -> NodePeerManager {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);
        let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.peer_connected(&public, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&public);
        pm
    }

    /// Pure decision table (#931): WARN only while the sync-time trusted-only
    /// clamp is in force; debug otherwise (Haskell's `(Unrestricted,
    /// DontUseBootstrapPeers, PraosMode) → UntrustedState` is normal+silent).
    #[test]
    fn haa_diagnostic_level_decision_table() {
        assert_eq!(haa_diagnostic_level(true), HaaDiagnosticLevel::Warn);
        assert_eq!(haa_diagnostic_level(false), HaaDiagnosticLevel::Debug);
    }

    /// #931 regression: with NO sync-time clamp active (Praos mode / Genesis
    /// CaughtUp — the preprod incident state), a clause (b) failure must NOT
    /// emit a WARN. It stays visible at debug level, without any "bypassed"
    /// claim.
    #[test]
    fn haa_clause_b_is_debug_not_warn_when_clamp_inactive() {
        let pm = pm_with_clause_b_failure();
        // Default clamp state is inactive — exactly the Praos-mode reality.
        let events = capture_networking_events(|| {
            assert!(!pm.haa_satisfied(5));
        });
        let haa_events: Vec<_> = events
            .iter()
            .filter(|(_, msg)| msg.contains("HAA clause"))
            .collect();
        assert!(
            !haa_events
                .iter()
                .any(|(lvl, _)| *lvl == tracing::Level::WARN),
            "no WARN may fire when the sync-time clamp is not active — got {haa_events:?}"
        );
        assert!(
            haa_events
                .iter()
                .any(|(lvl, msg)| *lvl == tracing::Level::DEBUG && msg.contains("clause (b)")),
            "the clause (b) diagnostic must still be visible at debug level — got {events:?}"
        );
        assert!(
            !haa_events.iter().any(|(_, msg)| msg.contains("bypass")),
            "no diagnostic may claim a clamp bypass — got {haa_events:?}"
        );
    }

    /// #931: while the sync-time clamp IS active, the clause (b) diagnostic
    /// keeps WARN severity — but states what is known (untrusted peers
    /// established while the clamp is active; the #920 per-tick sweep demotes
    /// them next tick) instead of asserting a bypass.
    #[test]
    fn haa_clause_b_warns_without_bypass_claim_when_clamp_active() {
        let pm = pm_with_clause_b_failure();
        pm.set_sync_trusted_clamp_active(true);
        let events = capture_networking_events(|| {
            assert!(!pm.haa_satisfied(5));
        });
        let warn: Vec<_> = events
            .iter()
            .filter(|(lvl, msg)| *lvl == tracing::Level::WARN && msg.contains("clause (b)"))
            .collect();
        assert_eq!(
            warn.len(),
            1,
            "exactly one clause (b) WARN while the clamp is active — got {events:?}"
        );
        assert!(
            !warn[0].1.contains("bypass"),
            "the WARN must not claim a bypass — got {:?}",
            warn[0].1
        );
        assert!(
            warn[0].1.contains("clamp is active"),
            "the WARN must state the clamp is active — got {:?}",
            warn[0].1
        );
    }

    /// #931: clause (a) gets the same gating — debug when no clamp is
    /// active, WARN when it is.
    #[test]
    fn haa_clause_a_diagnostic_severity_follows_clamp() {
        // Clause (a) failure: a bootstrap peer is configured (so the
        // UseBootstrapPeers branch — the only branch with clause
        // diagnostics — governs) but not hot; the only hot outbound peer
        // is untrusted.
        let make_pm = || {
            let mut pm = NodePeerManager::new(PeerManagerConfig::default());
            pm.add_bootstrap_peer("3.74.40.92:3001".parse().unwrap());
            pm.add_local_root_group(LocalRootGroupInfo {
                name: "relays".into(),
                addrs: vec!["127.0.0.1:3002".parse().unwrap()],
                hot_valency: 1,
                warm_valency: 1,
                diffusion_mode: None,
                behind_firewall: false,
                advertise: false,
            });
            let public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
            pm.peer_connected(&public, ConnectionDirection::Outbound);
            pm.inner.promote_to_hot(&public);
            pm
        };

        let pm = make_pm();
        let events = capture_networking_events(|| {
            assert!(!pm.haa_satisfied(5));
        });
        assert!(
            !events
                .iter()
                .any(|(lvl, msg)| *lvl == tracing::Level::WARN && msg.contains("HAA clause")),
            "clause (a) must not WARN without an active clamp — got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(lvl, msg)| *lvl == tracing::Level::DEBUG && msg.contains("clause (a)")),
            "clause (a) must stay visible at debug level — got {events:?}"
        );

        let pm = make_pm();
        pm.set_sync_trusted_clamp_active(true);
        let events = capture_networking_events(|| {
            assert!(!pm.haa_satisfied(5));
        });
        assert!(
            events
                .iter()
                .any(|(lvl, msg)| *lvl == tracing::Level::WARN && msg.contains("clause (a)")),
            "clause (a) must WARN while the clamp is active — got {events:?}"
        );
    }

    /// #931 behavior guard: the clamp-active flag is diagnostics-only —
    /// `haa_satisfied`'s RETURN VALUE must be identical in both clamp states,
    /// for both a failing and a satisfied peer set.
    #[test]
    fn haa_satisfied_return_value_independent_of_clamp_flag() {
        // Failing set (clause (b) violated): false regardless of the flag.
        let pm = pm_with_clause_b_failure();
        assert!(!pm.haa_satisfied(5), "clamp inactive: unsatisfied");
        pm.set_sync_trusted_clamp_active(true);
        assert!(!pm.haa_satisfied(5), "clamp active: still unsatisfied");

        // Satisfied set (hot trusted bootstrap only): true regardless.
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
        pm.add_bootstrap_peer(boot);
        pm.peer_connected(&boot, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&boot);
        assert!(pm.haa_satisfied(5), "clamp inactive: satisfied");
        pm.set_sync_trusted_clamp_active(true);
        assert!(pm.haa_satisfied(5), "clamp active: still satisfied");
    }

    // ─── #933 — Haskell (associationMode, bootstrapPeersFlag, consensusMode)
    //     case split ──────────────────────────────────────────────────────────

    /// Establish hot outbound big-ledger peers at indices `range`.
    fn add_hot_blps(pm: &mut NodePeerManager, range: std::ops::Range<usize>) {
        for i in range {
            let addr: SocketAddr = format!("203.0.113.{}:3001", 10 + i).parse().unwrap();
            pm.add_big_ledger_peer(addr);
            pm.peer_connected(&addr, ConnectionDirection::Outbound);
            pm.inner.promote_to_hot(&addr);
        }
    }

    /// Haskell `(Unrestricted, DontUseBootstrapPeers, PraosMode) ->
    /// UntrustedState` — unconditional. Even a full hot-BLP quorum must not
    /// satisfy the HAA in Praos mode (pre-#933 the layered BLP clause
    /// returned true here), and the failure must be SILENT (no WARN):
    /// cardano-node treats Praos UntrustedState as normal operation.
    #[test]
    fn haa_praos_no_bootstrap_false_and_silent_even_with_blp_quorum() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        add_hot_blps(&mut pm, 0..5);
        let events = capture_networking_events(|| {
            assert!(
                !pm.haa_satisfied(5),
                "Praos + DontUseBootstrapPeers is UntrustedState even with \
                 a hot-BLP quorum"
            );
        });
        assert!(
            !events.iter().any(|(lvl, _)| *lvl == tracing::Level::WARN),
            "Praos UntrustedState is normal and must be silent — got {events:?}"
        );
    }

    /// Haskell `(Unrestricted, DontUseBootstrapPeers, GenesisMode)` —
    /// satisfied iff `activeNumBigLedgerPeers >= minNumberOfBigLedgerPeers`
    /// (hot only), nothing else.
    #[test]
    fn haa_genesis_no_bootstrap_blp_quorum() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.set_genesis_mode(true);
        add_hot_blps(&mut pm, 0..4);
        assert!(!pm.haa_satisfied(5), "4 hot BLPs < min 5 → unsatisfied");
        add_hot_blps(&mut pm, 4..5);
        assert!(pm.haa_satisfied(5), "5 hot BLPs >= min 5 → satisfied");
    }

    /// The Genesis `DontUseBootstrapPeers` branch ignores
    /// `viewEstablishedPeers` and local-root trust COMPLETELY (Haskell branch
    /// 4 reads only `activeNumBigLedgerPeers`) — established untrusted
    /// outbound peers must NOT fail it. This is the behavior change vs the
    /// old layering, whose clause (b) ("every established outbound peer is
    /// trusted") could fail a Genesis node that was HAA-assessable via BLPs.
    #[test]
    fn haa_genesis_no_bootstrap_untrusted_established_irrelevant() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.set_genesis_mode(true);
        add_hot_blps(&mut pm, 0..5);
        // Establish untrusted public outbound peers (one hot, one warm).
        let hot_public: SocketAddr = "8.8.8.8:3001".parse().unwrap();
        pm.peer_connected(&hot_public, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&hot_public);
        let warm_public: SocketAddr = "8.8.4.4:3001".parse().unwrap();
        pm.peer_connected(&warm_public, ConnectionDirection::Outbound);
        assert!(!pm.untrusted_established_outbound().is_empty());
        assert!(
            pm.haa_satisfied(5),
            "established untrusted outbound peers are IRRELEVANT in the \
             Genesis DontUseBootstrapPeers branch"
        );
    }

    /// In the Genesis `DontUseBootstrapPeers` branch, hot trustable local
    /// roots contribute NOTHING (Haskell branch 4 has no trusted-closure
    /// alternative) — pre-#933 the layered clause pair returned true here.
    #[test]
    fn haa_genesis_no_bootstrap_local_roots_cannot_satisfy() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.set_genesis_mode(true);
        let root: SocketAddr = "127.0.0.1:3002".parse().unwrap();
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "relays".into(),
            addrs: vec![root],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        pm.peer_connected(&root, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&root);
        assert!(
            !pm.haa_satisfied(5),
            "Genesis + DontUseBootstrapPeers is BLP-quorum-only; hot local \
             roots do not satisfy it"
        );
    }

    /// With bootstrap peers configured the `UseBootstrapPeers` branch governs
    /// in BOTH consensus modes — a hot-BLP quorum must NOT short-circuit past
    /// its closure (the pre-#933 layering did exactly that: the BLP clause
    /// ran first). Haskell `(Unrestricted, UseBootstrapPeers{}, _)` requires
    /// established ⊆ bootstrap ∪ trustable-local AND an active trusted peer,
    /// regardless of `activeNumBigLedgerPeers`.
    #[test]
    fn haa_bootstrap_branch_governs_even_with_blp_quorum() {
        for genesis in [false, true] {
            let mut pm = NodePeerManager::new(PeerManagerConfig::default());
            pm.set_genesis_mode(genesis);
            let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
            pm.add_bootstrap_peer(boot);
            pm.peer_connected(&boot, ConnectionDirection::Outbound);
            pm.inner.promote_to_hot(&boot);
            add_hot_blps(&mut pm, 0..5);
            // The hot BLPs are untrusted established outbound peers → the
            // bootstrap branch's closure fails, BLP count notwithstanding.
            assert!(
                !pm.haa_satisfied(5),
                "genesis={genesis}: bootstrap branch must govern (closure \
                 violated by untrusted BLPs) even at BLP quorum"
            );
        }
    }

    /// The documented from-genesis property: a node with bootstrap relays
    /// configured satisfies the HAA via them — with ZERO big-ledger peers —
    /// in both consensus modes (Haskell `(Unrestricted, UseBootstrapPeers{},
    /// _)` matches `_` for the mode).
    #[test]
    fn haa_bootstrap_branch_satisfied_with_zero_blps_both_modes() {
        for genesis in [false, true] {
            let mut pm = NodePeerManager::new(PeerManagerConfig::default());
            pm.set_genesis_mode(genesis);
            let boot: SocketAddr = "3.74.40.92:3001".parse().unwrap();
            pm.add_bootstrap_peer(boot);
            pm.peer_connected(&boot, ConnectionDirection::Outbound);
            pm.inner.promote_to_hot(&boot);
            assert!(
                pm.haa_satisfied(5),
                "genesis={genesis}: a hot bootstrap relay satisfies the HAA \
                 with zero BLPs"
            );
        }
    }

    #[test]
    fn test_effective_diffusion_mode_per_group() {
        let mut pm = NodePeerManager::new(PeerManagerConfig {
            diffusion_mode: DiffusionMode::InitiatorAndResponder,
            ..PeerManagerConfig::default()
        });

        let relay: SocketAddr = "1.2.3.4:3001".parse().unwrap();
        let bp_relay: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let unknown: SocketAddr = "8.8.8.8:3001".parse().unwrap();

        pm.add_local_root_group(LocalRootGroupInfo {
            name: "relays".into(),
            addrs: vec![relay],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: true,
        });
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "bp-relays".into(),
            addrs: vec![bp_relay],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: Some(DiffusionMode::InitiatorOnly),
            behind_firewall: false,
            advertise: false,
        });

        // Relay inherits node-level default.
        assert_eq!(
            pm.effective_diffusion_mode(&relay),
            DiffusionMode::InitiatorAndResponder
        );
        // BP relay uses per-group override.
        assert_eq!(
            pm.effective_diffusion_mode(&bp_relay),
            DiffusionMode::InitiatorOnly
        );
        // Unknown peer falls back to node-level.
        assert_eq!(
            pm.effective_diffusion_mode(&unknown),
            DiffusionMode::InitiatorAndResponder
        );
    }

    #[test]
    fn test_behind_firewall_and_advertise() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let fw_addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        let normal_addr: SocketAddr = "1.2.3.4:3001".parse().unwrap();
        let unknown_addr: SocketAddr = "8.8.8.8:3001".parse().unwrap();

        pm.add_local_root_group(LocalRootGroupInfo {
            name: "firewall-group".into(),
            addrs: vec![fw_addr],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: true,
            advertise: false,
        });
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "normal-group".into(),
            addrs: vec![normal_addr],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: true,
        });

        assert!(pm.is_behind_firewall(&fw_addr));
        assert!(!pm.is_behind_firewall(&normal_addr));
        assert!(!pm.is_behind_firewall(&unknown_addr));

        assert!(!pm.is_advertisable(&fw_addr));
        assert!(pm.is_advertisable(&normal_addr));
        assert!(pm.is_advertisable(&unknown_addr)); // unknown defaults to true
    }

    #[test]
    fn test_is_self_addr_exact_match() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "1.2.3.4:3001".parse().unwrap();
        pm.set_local_addr(addr);
        assert!(pm.is_self_addr(addr));
        assert!(!pm.is_self_addr("5.6.7.8:3001".parse().unwrap()));
    }

    #[test]
    fn test_is_self_addr_wildcard_loopback() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        // Bind on 0.0.0.0:3001 — loopback on same port is self.
        pm.set_local_addr("0.0.0.0:3001".parse().unwrap());
        assert!(pm.is_self_addr("127.0.0.1:3001".parse().unwrap()));
        assert!(pm.is_self_addr("0.0.0.0:3001".parse().unwrap()));
        // Different port is not self.
        assert!(!pm.is_self_addr("127.0.0.1:3002".parse().unwrap()));
        // Non-loopback on same port is not self (could be a real peer).
        assert!(!pm.is_self_addr("1.2.3.4:3001".parse().unwrap()));
    }

    #[test]
    fn test_is_self_addr_no_local_addr() {
        let pm = NodePeerManager::new(PeerManagerConfig::default());
        assert!(!pm.is_self_addr("127.0.0.1:3001".parse().unwrap()));
    }

    #[test]
    fn test_add_ledger_peer_rejects_self_loopback() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.set_local_addr("0.0.0.0:3001".parse().unwrap());
        pm.add_ledger_peer("127.0.0.1:3001".parse().unwrap());
        // Should not be added — it's us.
        assert_eq!(pm.inner.count_by_state(PeerState::Cold), 0);
    }

    #[test]
    fn test_is_non_public_ip_v4_classes() {
        // Non-public — must reject from peer-sharing / ledger sources.
        assert!(is_non_public_ip("127.0.0.1".parse().unwrap()));
        assert!(is_non_public_ip("0.0.0.0".parse().unwrap()));
        assert!(is_non_public_ip("10.0.0.1".parse().unwrap()));
        assert!(is_non_public_ip("172.16.5.5".parse().unwrap()));
        assert!(is_non_public_ip("192.168.1.1".parse().unwrap()));
        assert!(is_non_public_ip("169.254.1.1".parse().unwrap())); // link-local
        assert!(is_non_public_ip("224.0.0.1".parse().unwrap())); // multicast
        assert!(is_non_public_ip("255.255.255.255".parse().unwrap())); // broadcast
        assert!(is_non_public_ip("100.64.0.1".parse().unwrap())); // CGNAT
        assert!(is_non_public_ip("192.0.2.1".parse().unwrap())); // doc TEST-NET-1

        // Public — must accept.
        assert!(!is_non_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_non_public_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_non_public_ip("220.240.140.41".parse().unwrap()));
    }

    #[test]
    fn test_is_non_public_ip_v6_classes() {
        assert!(is_non_public_ip("::1".parse().unwrap()));
        assert!(is_non_public_ip("::".parse().unwrap()));
        assert!(is_non_public_ip("fe80::1".parse().unwrap())); // link-local
        assert!(is_non_public_ip("fc00::1".parse().unwrap())); // unique-local
        assert!(is_non_public_ip("ff02::1".parse().unwrap())); // multicast
        assert!(is_non_public_ip("2001:db8::1".parse().unwrap())); // documentation

        assert!(!is_non_public_ip("2606:4700:4700::1111".parse().unwrap())); // public
    }

    #[test]
    fn test_add_shared_peer_rejects_non_public() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        // None of these should be admitted as peer-sharing candidates.
        pm.add_shared_peer("127.0.0.1:3001".parse().unwrap());
        pm.add_shared_peer("10.0.0.1:3001".parse().unwrap());
        pm.add_shared_peer("192.168.1.1:3001".parse().unwrap());
        pm.add_shared_peer("169.254.1.1:3001".parse().unwrap());
        pm.add_shared_peer("[fe80::1]:3001".parse().unwrap());
        assert_eq!(pm.inner.count_by_state(PeerState::Cold), 0);

        // A public IP IS admitted.
        pm.add_shared_peer("8.8.8.8:3001".parse().unwrap());
        assert_eq!(pm.inner.count_by_state(PeerState::Cold), 1);
    }

    #[test]
    fn test_add_ledger_peer_rejects_non_public() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.set_local_addr("0.0.0.0:3001".parse().unwrap());
        pm.add_ledger_peer("10.0.0.1:3001".parse().unwrap());
        pm.add_ledger_peer("192.168.1.1:3001".parse().unwrap());
        assert_eq!(pm.inner.count_by_state(PeerState::Cold), 0);

        pm.add_ledger_peer("8.8.8.8:3001".parse().unwrap());
        assert_eq!(pm.inner.count_by_state(PeerState::Cold), 1);
    }

    #[test]
    fn test_static_topology_ips_collects_local_roots() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "co-located-relay".into(),
            addrs: vec!["127.0.0.1:3002".parse().unwrap()],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "external".into(),
            addrs: vec!["8.8.8.8:3001".parse().unwrap()],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: true,
        });

        let ips = pm.static_topology_ips();
        assert!(ips.contains(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(ips.contains(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert_eq!(ips.len(), 2);
    }

    /// #703 fix B — inbound connection marks the peer as fresh-inbound and
    /// `fresh_inbound_set` reports it during the maturation window.
    #[test]
    fn inbound_peer_marked_fresh_within_window() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Inbound);
        assert_eq!(pm.fresh_inbound_count(), 1);

        // 14:59 — still fresh.
        let now = std::time::Instant::now();
        let fresh = pm.fresh_inbound_set(now + std::time::Duration::from_secs(14 * 60 + 59));
        assert!(fresh.contains(&addr));
    }

    /// After the maturation window passes, the peer is no longer reported
    /// as fresh (filtered out at query time).
    #[test]
    fn inbound_peer_matures_after_15_minutes() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Inbound);

        // 15:01 — matured.
        let now = std::time::Instant::now();
        let fresh = pm.fresh_inbound_set(now + std::time::Duration::from_secs(15 * 60 + 1));
        assert!(!fresh.contains(&addr));
    }

    /// Governor-initiated disconnect goes Hot/Warm → Cooling → Cold:
    /// `mark_terminating` enters Cooling, `peer_disconnected` completes
    /// the Cooling → Cold transition. Re-promotion is blocked while in
    /// Cooling (mirrors Haskell `updateUnlessCoolingOrCold`).
    #[test]
    fn governor_disconnect_goes_through_cooling() {
        use dugite_network::peer::manager::PeerState;
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Outbound);
        // Warm → Hot to set up the canonical demotion path.
        pm.inner.promote_to_hot(&addr);
        assert_eq!(pm.hot_peer_count(), 1);

        // Governor decides to disconnect → mark_terminating fires.
        pm.mark_terminating(&addr);
        assert_eq!(
            pm.inner.count_by_state(PeerState::Cooling),
            1,
            "mark_terminating must demote Hot → Cooling"
        );
        assert_eq!(
            pm.hot_peer_count(),
            0,
            "Hot count drops immediately on mark_terminating"
        );
        assert!(
            pm.inner.get_peer(&addr).unwrap().state.is_cooling_or_cold(),
            "Cooling must satisfy is_cooling_or_cold for governor checks"
        );

        // Connection torn down → peer_disconnected completes Cooling → Cold.
        pm.peer_disconnected(&addr);
        assert_eq!(
            pm.inner.count_by_state(PeerState::Cold),
            1,
            "peer_disconnected must complete the Cooling → Cold transition"
        );
        assert_eq!(pm.inner.count_by_state(PeerState::Cooling), 0);
    }

    /// Remote-initiated disconnect (TCP RST detected by cleanup_dead_connections)
    /// bypasses `mark_terminating` and lands directly in `peer_disconnected`.
    /// `peer_disconnected` must still handle Warm/Hot → Cold via the
    /// `demote_to_cold` fallback in that case (no prior Cooling state).
    #[test]
    fn remote_initiated_disconnect_skips_cooling_gracefully() {
        use dugite_network::peer::manager::PeerState;
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Outbound);
        pm.inner.promote_to_hot(&addr);

        // No mark_terminating — connection died unexpectedly, detected at
        // cleanup_dead_connections time.
        pm.peer_disconnected(&addr);
        assert_eq!(
            pm.inner.count_by_state(PeerState::Cold),
            1,
            "Remote-initiated close must still land Cold"
        );
    }

    /// Outbound peers MUST NOT enter the maturation window — they come from
    /// known topology / ledger / DNS sources and have already been vetted.
    #[test]
    fn outbound_peer_not_marked_fresh() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Outbound);
        assert_eq!(pm.fresh_inbound_count(), 0);
    }

    /// Local root inbound peers are exempt — the topology file is the
    /// operator's trusted whitelist and hot_valency must be honoured
    /// immediately.
    #[test]
    fn inbound_local_root_peer_exempt_from_maturation() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        pm.add_local_root_group(LocalRootGroupInfo {
            name: "bp".into(),
            addrs: vec![addr],
            hot_valency: 1,
            warm_valency: 1,
            diffusion_mode: None,
            behind_firewall: false,
            advertise: false,
        });
        pm.peer_connected(&addr, ConnectionDirection::Inbound);
        assert_eq!(
            pm.fresh_inbound_count(),
            0,
            "local-root inbound must skip maturation gate"
        );
    }

    /// `peer_disconnected` cleans up the fresh-inbound entry so a reconnect
    /// restarts the maturation window from zero.
    #[test]
    fn disconnect_clears_fresh_inbound() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "203.0.113.5:3001".parse().unwrap();
        pm.peer_connected(&addr, ConnectionDirection::Inbound);
        assert_eq!(pm.fresh_inbound_count(), 1);
        pm.peer_disconnected(&addr);
        assert_eq!(pm.fresh_inbound_count(), 0);
    }

    /// Haskell parity: maturation delay is exactly 15 min.
    #[test]
    fn inbound_mature_peer_delay_pinned() {
        assert_eq!(
            INBOUND_MATURE_PEER_DELAY,
            std::time::Duration::from_secs(15 * 60),
            "Haskell InboundGovernor.inboundMaturePeerDelay"
        );
    }

    /// #699 — recording rollback-below-immutable from a single peer counts as 1.
    #[test]
    fn divergence_witness_single_peer_counts_once() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let a: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        pm.record_rollback_below_immutable(a, 4_500_000, 7_500_000);
        let n = pm.divergence_witness_count(
            std::time::Instant::now(),
            std::time::Duration::from_secs(60),
        );
        assert_eq!(n, 1);
    }

    /// Multiple peers reporting divergence are counted as distinct witnesses.
    #[test]
    fn divergence_witness_counts_distinct_peers() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        for i in 1..=5u16 {
            let a: SocketAddr = format!("10.0.0.{i}:3001").parse().unwrap();
            pm.record_rollback_below_immutable(a, 4_500_000, 7_500_000);
        }
        let n = pm.divergence_witness_count(
            std::time::Instant::now(),
            std::time::Duration::from_secs(60),
        );
        assert_eq!(n, 5);
    }

    /// Repeated reports from the same peer don't inflate the count.
    #[test]
    fn divergence_witness_idempotent_per_peer() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let a: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        for _ in 0..10 {
            pm.record_rollback_below_immutable(a, 4_500_000, 7_500_000);
        }
        let n = pm.divergence_witness_count(
            std::time::Instant::now(),
            std::time::Duration::from_secs(60),
        );
        assert_eq!(n, 1);
    }

    /// Stale witnesses (outside the window) are not counted.
    #[test]
    fn divergence_witness_window_excludes_stale_entries() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let a: SocketAddr = "10.0.0.1:3001".parse().unwrap();
        pm.record_rollback_below_immutable(a, 4_500_000, 7_500_000);

        // Query 10 minutes later — still inside a 15-min window.
        let now = std::time::Instant::now();
        let n = pm.divergence_witness_count(
            now + std::time::Duration::from_secs(10 * 60),
            std::time::Duration::from_secs(15 * 60),
        );
        assert_eq!(n, 1);

        // 20 minutes later — outside.
        let n = pm.divergence_witness_count(
            now + std::time::Duration::from_secs(20 * 60),
            std::time::Duration::from_secs(15 * 60),
        );
        assert_eq!(n, 0);
    }

    /// `gc_divergence_witnesses` drops stale entries.
    #[test]
    fn divergence_witness_gc() {
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        for i in 1..=3u16 {
            let a: SocketAddr = format!("10.0.0.{i}:3001").parse().unwrap();
            pm.record_rollback_below_immutable(a, 4_500_000, 7_500_000);
        }
        assert_eq!(pm.rollback_below_immutable_witnesses.len(), 3);

        let now = std::time::Instant::now();
        pm.gc_divergence_witnesses(
            now + std::time::Duration::from_secs(20 * 60),
            std::time::Duration::from_secs(15 * 60),
        );
        assert_eq!(pm.rollback_below_immutable_witnesses.len(), 0);
    }
}
