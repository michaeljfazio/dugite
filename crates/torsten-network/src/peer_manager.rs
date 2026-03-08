//! Peer manager for P2P networking.
//!
//! Manages outbound and inbound peer connections following cardano-node's
//! peer management model with cold/warm/hot peer sets.
//!
//! Supports both **InitiatorOnly** and **InitiatorAndResponder** (bidirectional)
//! diffusion modes, matching the Haskell cardano-node behavior.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Diffusion mode matching cardano-node
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffusionMode {
    /// Connect outbound only (typical non-relay nodes)
    InitiatorOnly,
    /// Both initiate and accept connections (relay nodes, stake pool nodes)
    #[default]
    InitiatorAndResponder,
}

/// Peer temperature classification (matching cardano-node)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerTemperature {
    /// Known but not connected
    Cold,
    /// Connected but not actively syncing
    Warm,
    /// Actively syncing/exchanging data
    Hot,
}

/// Peer source — how we learned about this peer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSource {
    /// From topology file (local roots, bootstrap peers)
    Config,
    /// From peer sharing protocol (gossip)
    PeerSharing,
    /// From ledger-based peer discovery
    Ledger,
}

/// Tracked peer state
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub address: SocketAddr,
    pub temperature: PeerTemperature,
    pub source: PeerSource,
    pub last_connected: Option<Instant>,
    pub last_failed: Option<Instant>,
    pub failure_count: u32,
    pub is_trustable: bool,
    pub advertise: bool,
    /// Negotiated protocol version (if connected)
    pub version: Option<u32>,
    /// Remote tip slot (if known)
    pub remote_tip_slot: Option<u64>,
    /// Connection direction
    pub is_initiator: Option<bool>,
}

impl PeerInfo {
    pub fn new(address: SocketAddr, source: PeerSource) -> Self {
        PeerInfo {
            address,
            temperature: PeerTemperature::Cold,
            source,
            last_connected: None,
            last_failed: None,
            failure_count: 0,
            is_trustable: false,
            advertise: false,
            version: None,
            remote_tip_slot: None,
            is_initiator: None,
        }
    }

    /// Whether this peer should be retried after failure
    pub fn should_retry(&self) -> bool {
        match self.last_failed {
            None => true,
            Some(t) => {
                // Exponential backoff: 5s, 10s, 20s, 40s, 60s max
                let delay = Duration::from_secs(
                    5u64.saturating_mul(2u64.saturating_pow(self.failure_count.min(4)))
                        .min(60),
                );
                t.elapsed() >= delay
            }
        }
    }
}

/// Configuration for the peer manager
#[derive(Debug, Clone)]
pub struct PeerManagerConfig {
    /// Target number of hot (actively syncing) peers
    pub target_hot_peers: usize,
    /// Target number of warm (connected, not syncing) peers
    pub target_warm_peers: usize,
    /// Target number of known peers (including cold)
    pub target_known_peers: usize,
    /// Maximum inbound connections to accept
    pub max_inbound_peers: usize,
    /// Whether to enable peer sharing
    pub peer_sharing_enabled: bool,
    /// Diffusion mode
    pub diffusion_mode: DiffusionMode,
    /// How often to churn peer connections (seconds)
    pub churn_interval_secs: u64,
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        PeerManagerConfig {
            target_hot_peers: 20,
            target_warm_peers: 20,
            target_known_peers: 100,
            max_inbound_peers: 100,
            peer_sharing_enabled: true,
            diffusion_mode: DiffusionMode::InitiatorAndResponder,
            churn_interval_secs: 300,
        }
    }
}

/// Events emitted by the peer manager
#[derive(Debug)]
pub enum PeerManagerEvent {
    /// Should connect to this peer
    Connect(SocketAddr),
    /// Should disconnect from this peer
    Disconnect(SocketAddr),
    /// Should promote warm peer to hot (start syncing)
    PromoteToHot(SocketAddr),
    /// Should demote hot peer to warm (stop syncing)
    DemoteToWarm(SocketAddr),
}

/// The peer manager tracks all known peers and drives connection decisions.
pub struct PeerManager {
    config: PeerManagerConfig,
    peers: HashMap<SocketAddr, PeerInfo>,
    hot_peers: HashSet<SocketAddr>,
    warm_peers: HashSet<SocketAddr>,
    cold_peers: HashSet<SocketAddr>,
    inbound_count: usize,
}

impl PeerManager {
    pub fn new(config: PeerManagerConfig) -> Self {
        PeerManager {
            config,
            peers: HashMap::new(),
            hot_peers: HashSet::new(),
            warm_peers: HashSet::new(),
            cold_peers: HashSet::new(),
            inbound_count: 0,
        }
    }

    /// Add a peer from the topology/config
    pub fn add_config_peer(&mut self, addr: SocketAddr, trustable: bool, advertise: bool) {
        let mut info = PeerInfo::new(addr, PeerSource::Config);
        info.is_trustable = trustable;
        info.advertise = advertise;
        self.cold_peers.insert(addr);
        self.peers.insert(addr, info);
    }

    /// Add a peer discovered from the ledger (SPO relay registrations)
    pub fn add_ledger_peer(&mut self, addr: SocketAddr) {
        if self.peers.contains_key(&addr) {
            return; // Already known
        }
        if self.peers.len() >= self.config.target_known_peers {
            return; // At capacity
        }
        let info = PeerInfo::new(addr, PeerSource::Ledger);
        self.cold_peers.insert(addr);
        self.peers.insert(addr, info);
        debug!(%addr, "Discovered peer from ledger");
    }

    /// Add a peer discovered via peer sharing
    pub fn add_shared_peer(&mut self, addr: SocketAddr) {
        if self.peers.contains_key(&addr) {
            return; // Already known
        }
        if self.peers.len() >= self.config.target_known_peers {
            return; // At capacity
        }
        let info = PeerInfo::new(addr, PeerSource::PeerSharing);
        self.cold_peers.insert(addr);
        self.peers.insert(addr, info);
        debug!(%addr, "Discovered peer via sharing");
    }

    /// Mark a peer as successfully connected (warm)
    pub fn peer_connected(&mut self, addr: &SocketAddr, version: u32, is_initiator: bool) {
        if let Some(info) = self.peers.get_mut(addr) {
            info.temperature = PeerTemperature::Warm;
            info.last_connected = Some(Instant::now());
            info.failure_count = 0;
            info.version = Some(version);
            info.is_initiator = Some(is_initiator);
            self.cold_peers.remove(addr);
            self.warm_peers.insert(*addr);
            if !is_initiator {
                self.inbound_count += 1;
            }
            info!(%addr, version, is_initiator, "Peer connected (warm)");
        }
    }

    /// Promote a warm peer to hot (start syncing)
    pub fn promote_to_hot(&mut self, addr: &SocketAddr) {
        if let Some(info) = self.peers.get_mut(addr) {
            if info.temperature == PeerTemperature::Warm {
                info.temperature = PeerTemperature::Hot;
                self.warm_peers.remove(addr);
                self.hot_peers.insert(*addr);
                debug!(%addr, "Peer promoted to hot");
            }
        }
    }

    /// Demote a hot peer to warm (stop syncing)
    pub fn demote_to_warm(&mut self, addr: &SocketAddr) {
        if let Some(info) = self.peers.get_mut(addr) {
            if info.temperature == PeerTemperature::Hot {
                info.temperature = PeerTemperature::Warm;
                self.hot_peers.remove(addr);
                self.warm_peers.insert(*addr);
                debug!(%addr, "Peer demoted to warm");
            }
        }
    }

    /// Mark a peer as disconnected
    pub fn peer_disconnected(&mut self, addr: &SocketAddr) {
        if let Some(info) = self.peers.get_mut(addr) {
            if info.is_initiator == Some(false) {
                self.inbound_count = self.inbound_count.saturating_sub(1);
            }
            info.temperature = PeerTemperature::Cold;
            info.version = None;
            info.is_initiator = None;
            self.hot_peers.remove(addr);
            self.warm_peers.remove(addr);
            self.cold_peers.insert(*addr);
        }
    }

    /// Mark a peer as failed (connection attempt failed)
    pub fn peer_failed(&mut self, addr: &SocketAddr) {
        if let Some(info) = self.peers.get_mut(addr) {
            info.last_failed = Some(Instant::now());
            info.failure_count += 1;
            info.temperature = PeerTemperature::Cold;
            info.version = None;
            info.is_initiator = None;
            self.hot_peers.remove(addr);
            self.warm_peers.remove(addr);
            self.cold_peers.insert(*addr);
        }
    }

    /// Update a peer's remote tip
    pub fn update_tip(&mut self, addr: &SocketAddr, tip_slot: u64) {
        if let Some(info) = self.peers.get_mut(addr) {
            info.remote_tip_slot = Some(tip_slot);
        }
    }

    /// Check if we should accept an inbound connection
    pub fn should_accept_inbound(&self) -> bool {
        self.config.diffusion_mode == DiffusionMode::InitiatorAndResponder
            && self.inbound_count < self.config.max_inbound_peers
    }

    /// Get peers that should be connected to (cold peers that need promotion)
    pub fn peers_to_connect(&self) -> Vec<SocketAddr> {
        let connected = self.hot_peers.len() + self.warm_peers.len();
        let target = self.config.target_hot_peers + self.config.target_warm_peers;
        if connected >= target {
            return vec![];
        }

        let needed = target - connected;
        self.cold_peers
            .iter()
            .filter(|addr| {
                self.peers
                    .get(addr)
                    .map(|p| p.should_retry())
                    .unwrap_or(false)
            })
            .take(needed)
            .copied()
            .collect()
    }

    /// Get warm peers that should be promoted to hot
    pub fn peers_to_promote(&self) -> Vec<SocketAddr> {
        if self.hot_peers.len() >= self.config.target_hot_peers {
            return vec![];
        }
        let needed = self.config.target_hot_peers - self.hot_peers.len();
        self.warm_peers.iter().take(needed).copied().collect()
    }

    /// Get the list of hot peer addresses
    pub fn hot_peer_addrs(&self) -> Vec<SocketAddr> {
        self.hot_peers.iter().copied().collect()
    }

    /// Get the list of all connected peer addresses
    pub fn connected_peer_addrs(&self) -> Vec<SocketAddr> {
        self.hot_peers
            .iter()
            .chain(self.warm_peers.iter())
            .copied()
            .collect()
    }

    /// Get peer addresses to share with a requesting peer
    pub fn peers_for_sharing(&self, max_count: usize) -> Vec<SocketAddr> {
        if !self.config.peer_sharing_enabled {
            return vec![];
        }
        self.peers
            .iter()
            .filter(|(_, info)| info.advertise && info.temperature != PeerTemperature::Cold)
            .map(|(addr, _)| *addr)
            .take(max_count)
            .collect()
    }

    /// Get the diffusion mode
    pub fn diffusion_mode(&self) -> DiffusionMode {
        self.config.diffusion_mode
    }

    /// Get statistics
    pub fn stats(&self) -> PeerManagerStats {
        PeerManagerStats {
            known_peers: self.peers.len(),
            cold_peers: self.cold_peers.len(),
            warm_peers: self.warm_peers.len(),
            hot_peers: self.hot_peers.len(),
            inbound_count: self.inbound_count,
        }
    }
}

/// Statistics for monitoring
#[derive(Debug, Clone)]
pub struct PeerManagerStats {
    pub known_peers: usize,
    pub cold_peers: usize,
    pub warm_peers: usize,
    pub hot_peers: usize,
    pub inbound_count: usize,
}

impl std::fmt::Display for PeerManagerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "peers: {} known ({} cold, {} warm, {} hot), {} inbound",
            self.known_peers, self.cold_peers, self.warm_peers, self.hot_peers, self.inbound_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    #[test]
    fn test_add_config_peer() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_config_peer(addr, true, false);

        assert_eq!(pm.peers.len(), 1);
        assert!(pm.cold_peers.contains(&addr));
        assert_eq!(pm.peers[&addr].source, PeerSource::Config);
        assert!(pm.peers[&addr].is_trustable);
    }

    #[test]
    fn test_peer_lifecycle() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_config_peer(addr, false, false);

        // Connect
        pm.peer_connected(&addr, 14, true);
        assert!(pm.warm_peers.contains(&addr));
        assert!(!pm.cold_peers.contains(&addr));

        // Promote to hot
        pm.promote_to_hot(&addr);
        assert!(pm.hot_peers.contains(&addr));
        assert!(!pm.warm_peers.contains(&addr));

        // Demote to warm
        pm.demote_to_warm(&addr);
        assert!(pm.warm_peers.contains(&addr));
        assert!(!pm.hot_peers.contains(&addr));

        // Disconnect
        pm.peer_disconnected(&addr);
        assert!(pm.cold_peers.contains(&addr));
        assert!(!pm.warm_peers.contains(&addr));
    }

    #[test]
    fn test_peers_to_connect() {
        let config = PeerManagerConfig {
            target_hot_peers: 2,
            target_warm_peers: 2,
            ..PeerManagerConfig::default()
        };
        let mut pm = PeerManager::new(config);

        for i in 0..5 {
            pm.add_config_peer(test_addr(3000 + i), false, false);
        }

        let to_connect = pm.peers_to_connect();
        assert_eq!(to_connect.len(), 4); // target_hot(2) + target_warm(2)
    }

    #[test]
    fn test_peers_to_promote() {
        let config = PeerManagerConfig {
            target_hot_peers: 2,
            ..PeerManagerConfig::default()
        };
        let mut pm = PeerManager::new(config);

        let a1 = test_addr(3001);
        let a2 = test_addr(3002);
        let a3 = test_addr(3003);
        pm.add_config_peer(a1, false, false);
        pm.add_config_peer(a2, false, false);
        pm.add_config_peer(a3, false, false);
        pm.peer_connected(&a1, 14, true);
        pm.peer_connected(&a2, 14, true);
        pm.peer_connected(&a3, 14, true);

        let to_promote = pm.peers_to_promote();
        assert_eq!(to_promote.len(), 2); // target_hot = 2
    }

    #[test]
    fn test_inbound_acceptance() {
        let config = PeerManagerConfig {
            max_inbound_peers: 2,
            ..PeerManagerConfig::default()
        };
        let mut pm = PeerManager::new(config);
        assert!(pm.should_accept_inbound());

        let a1 = test_addr(3001);
        let a2 = test_addr(3002);
        pm.add_config_peer(a1, false, false);
        pm.add_config_peer(a2, false, false);
        pm.peer_connected(&a1, 14, false); // inbound
        assert!(pm.should_accept_inbound());
        pm.peer_connected(&a2, 14, false); // inbound
        assert!(!pm.should_accept_inbound()); // at max
    }

    #[test]
    fn test_initiator_only_rejects_inbound() {
        let config = PeerManagerConfig {
            diffusion_mode: DiffusionMode::InitiatorOnly,
            ..PeerManagerConfig::default()
        };
        let pm = PeerManager::new(config);
        assert!(!pm.should_accept_inbound());
    }

    #[test]
    fn test_peer_sharing() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let a1 = test_addr(3001);
        let a2 = test_addr(3002);
        pm.add_config_peer(a1, false, true); // advertise=true
        pm.add_config_peer(a2, false, false); // advertise=false
        pm.peer_connected(&a1, 14, true);

        let shared = pm.peers_for_sharing(10);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0], a1);
    }

    #[test]
    fn test_peer_failure_backoff() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_config_peer(addr, false, false);

        // First failure
        pm.peer_failed(&addr);
        assert!(!pm.peers[&addr].should_retry()); // Just failed, shouldn't retry yet

        // After enough time, should retry
        // (Can't easily test time-based behavior in unit tests without mocking)
    }

    #[test]
    fn test_stats() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let a1 = test_addr(3001);
        let a2 = test_addr(3002);
        let a3 = test_addr(3003);
        pm.add_config_peer(a1, false, false);
        pm.add_config_peer(a2, false, false);
        pm.add_config_peer(a3, false, false);
        pm.peer_connected(&a1, 14, true);
        pm.promote_to_hot(&a1);

        let stats = pm.stats();
        assert_eq!(stats.known_peers, 3);
        assert_eq!(stats.cold_peers, 2);
        assert_eq!(stats.warm_peers, 0);
        assert_eq!(stats.hot_peers, 1);
    }

    #[test]
    fn test_add_shared_peer_dedup() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_config_peer(addr, false, false);
        pm.add_shared_peer(addr); // Already known
        assert_eq!(pm.peers.len(), 1);
    }

    #[test]
    fn test_add_shared_peer_capacity() {
        let config = PeerManagerConfig {
            target_known_peers: 2,
            ..PeerManagerConfig::default()
        };
        let mut pm = PeerManager::new(config);
        pm.add_config_peer(test_addr(3001), false, false);
        pm.add_config_peer(test_addr(3002), false, false);
        pm.add_shared_peer(test_addr(3003)); // At capacity
        assert_eq!(pm.peers.len(), 2);
    }

    #[test]
    fn test_add_ledger_peer() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_ledger_peer(addr);

        assert_eq!(pm.peers.len(), 1);
        assert!(pm.cold_peers.contains(&addr));
        assert_eq!(pm.peers[&addr].source, PeerSource::Ledger);
    }

    #[test]
    fn test_add_ledger_peer_dedup() {
        let mut pm = PeerManager::new(PeerManagerConfig::default());
        let addr = test_addr(3001);
        pm.add_config_peer(addr, false, false);
        pm.add_ledger_peer(addr); // Already known from config
        assert_eq!(pm.peers.len(), 1);
        assert_eq!(pm.peers[&addr].source, PeerSource::Config); // Source unchanged
    }

    #[test]
    fn test_add_ledger_peer_capacity() {
        let config = PeerManagerConfig {
            target_known_peers: 2,
            ..PeerManagerConfig::default()
        };
        let mut pm = PeerManager::new(config);
        pm.add_ledger_peer(test_addr(3001));
        pm.add_ledger_peer(test_addr(3002));
        pm.add_ledger_peer(test_addr(3003)); // At capacity
        assert_eq!(pm.peers.len(), 2);
    }
}
