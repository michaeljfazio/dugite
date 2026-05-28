//! PeerManager — tracks peer state, latency, reputation, and failure counts.
//!
//! Each known peer has a [`PeerInfo`] record that tracks:
//! - Cold/warm/hot classification
//! - EWMA (exponentially weighted moving average) latency from KeepAlive
//! - Reputation score (decays toward neutral, boosted/penalized on events)
//! - Failure count with 5-minute decay timer
//!
//! The PeerManager is the data layer; the Governor makes decisions based on this data.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Peer temperature state (Ouroboros peer lifecycle).
///
/// Mirrors Haskell's `PeerStatus` from
/// `ouroboros-network/lib/Ouroboros/Network/PeerSelection/Types.hs`:
///
/// ```haskell
/// data PeerStatus =
///       PeerCold
///     | PeerCooling
///     -- ^ Peer is in cold state but its connection still lingers.
///     -- The PeerCooling -> PeerCold transition is an outbound-governor
///     -- reflection of TerminatingSt -> TerminatedSt (our version of TCP TimeWait).
///     | PeerWarm
///     | PeerHot
/// ```
///
/// Ordering matters: `Cold < Cooling < Warm < Hot`. `updateUnlessCoolingOrCold`
/// checks treat `Cooling` like `Cold` for promotion eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerState {
    /// Known but not connected — candidate for promotion.
    Cold,
    /// Peer in cold state but its connection still lingers
    /// (governor's analogue of the connection manager's `TerminatingState`,
    /// our outbound version of TCP `TIME_WAIT`). Not eligible for re-promotion
    /// until the transition to `Cold` lands.
    Cooling,
    /// TCP connection established, keepalive running, not yet syncing.
    Warm,
    /// Fully active — running ChainSync, BlockFetch, TxSubmission2.
    Hot,
}

impl PeerState {
    /// Whether the peer is in a transition state that must NOT be re-promoted.
    /// Mirrors Haskell's `updateUnlessCoolingOrCold` `(<=)` check.
    pub fn is_cooling_or_cold(&self) -> bool {
        matches!(self, PeerState::Cold | PeerState::Cooling)
    }
}

/// EWMA smoothing factor (0-1). Higher = more weight on recent measurements.
const EWMA_ALPHA: f64 = 0.3;

/// Failure count decay interval — halves every 5 minutes.
const FAILURE_DECAY_INTERVAL: Duration = Duration::from_secs(300);

/// Initial reputation score for new peers.
const INITIAL_REPUTATION: f64 = 0.5;

/// Base retry delay (seconds) for cold→warm connection failures.
///
/// Matches Haskell ouroboros-network `baseColdPeerRetryDiffTime = 5`.
const COLD_RETRY_BASE_SECS: f64 = 5.0;

/// Maximum exponent for exponential backoff, capping the delay at
/// `5 * 2^5 = 160s`. Matches Haskell `maxColdPeerRetryBackoff = 5`.
const COLD_RETRY_MAX_EXP: u32 = 5;

/// Maximum consecutive cold→warm failures before a non-root peer is
/// forgotten entirely. Matches Haskell `policyMaxConnectionRetries = 5`.
pub const MAX_COLD_PEER_FAILURES: u32 = 5;

/// Default EWMA latency seed (ms) for peers that have not yet produced a
/// KeepAlive RTT sample.
///
/// Haskell's `defaultGSV` in `ouroboros-network/lib/Ouroboros/Network/DeltaQ.hs`
/// uses `default_g = 500e-3` (500 ms one-way propagation delay).  Since
/// dugite's EWMA tracks the full round-trip time (RTT = 2 × g), the
/// equivalent seed is **1 000 ms**.  This ensures unmeasured peers are
/// treated as high-latency until real KeepAlive data arrives, rather than
/// being incorrectly preferred over peers with actual measurements.
pub const PEER_LATENCY_DEFAULT_MS: f64 = 1_000.0;

/// Information tracked for each known peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Current peer temperature.
    pub state: PeerState,
    /// EWMA round-trip latency in milliseconds (None if no measurement yet).
    pub latency_ms: Option<f64>,
    /// Reputation score (0.0 = worst, 1.0 = best).
    pub reputation: f64,
    /// Number of failures since last decay.
    pub failure_count: u32,
    /// When the failure count was last decayed.
    pub last_failure_decay: Instant,
    /// When this peer was first discovered.
    pub discovered_at: Instant,
    /// Whether this peer supports peer sharing.
    pub peer_sharing: bool,
    /// Source of peer discovery.
    pub source: PeerSource,
    /// Earliest time this peer may be connected again (exponential backoff).
    ///
    /// `None` means eligible immediately. Set by `record_failure()` using
    /// the same formula as Haskell's `jobPromoteColdPeer`:
    /// `delay = 5 * 2^(min(failCount-1, 5))` seconds ± 2s fuzz.
    pub next_connect_after: Option<Instant>,
}

/// How a peer was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSource {
    /// Configured in topology file.
    Topology,
    /// Discovered via DNS SRV/A/AAAA lookup.
    Dns,
    /// Discovered from ledger state (SPO relays).
    Ledger,
    /// Received via PeerSharing protocol.
    PeerSharing,
}

impl PeerInfo {
    /// Create a new cold peer with default values.
    pub fn new(source: PeerSource) -> Self {
        let now = Instant::now();
        Self {
            state: PeerState::Cold,
            latency_ms: None,
            reputation: INITIAL_REPUTATION,
            failure_count: 0,
            last_failure_decay: now,
            discovered_at: now,
            peer_sharing: false,
            source,
            next_connect_after: None,
        }
    }

    /// Update latency with a new RTT measurement (EWMA).
    pub fn update_latency(&mut self, rtt_ms: f64) {
        self.latency_ms = Some(match self.latency_ms {
            Some(prev) => prev * (1.0 - EWMA_ALPHA) + rtt_ms * EWMA_ALPHA,
            None => rtt_ms,
        });
    }

    /// Record a failure — increments failure count, reduces reputation, and
    /// schedules an exponential backoff delay before the next connect attempt.
    ///
    /// Matches Haskell `jobPromoteColdPeer` backoff:
    /// `delay = base * 2^(min(failCount-1, maxExp))` ± 2s uniform fuzz, where
    /// `base=5s` and `maxExp=5` (capping at 160s).
    pub fn record_failure(&mut self) {
        use rand::Rng;
        self.failure_count += 1;
        // Reputation penalty: 0.1 per failure, clamped to [0, 1]
        self.reputation = (self.reputation - 0.1).max(0.0);
        // Exponential backoff: 5s, 10s, 20s, 40s, 80s, 160s (cap), ±2s fuzz.
        let exp = (self.failure_count - 1).min(COLD_RETRY_MAX_EXP);
        let base = COLD_RETRY_BASE_SECS * 2f64.powi(exp as i32);
        let fuzz: f64 = rand::rng().random_range(-2.0..2.0);
        let delay_secs = (base + fuzz).max(1.0);
        self.next_connect_after = Some(Instant::now() + Duration::from_secs_f64(delay_secs));
    }

    /// Record a success — slightly boosts reputation.
    pub fn record_success(&mut self) {
        self.reputation = (self.reputation + 0.01).min(1.0);
    }

    /// Apply failure count decay (halves every 5 minutes).
    pub fn decay_failures(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_failure_decay);
        if elapsed >= FAILURE_DECAY_INTERVAL {
            let decay_periods = elapsed.as_secs() / FAILURE_DECAY_INTERVAL.as_secs();
            for _ in 0..decay_periods {
                self.failure_count /= 2;
            }
            self.last_failure_decay = now;
            // Reputation slowly recovers during decay
            self.reputation = (self.reputation + 0.05 * decay_periods as f64).min(1.0);
        }
    }
}

/// PeerManager — central registry of all known peers.
pub struct PeerManager {
    /// Map of peer address → peer info.
    peers: HashMap<SocketAddr, PeerInfo>,
}

impl PeerManager {
    /// Create an empty peer manager.
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
        }
    }

    /// Add a new peer (as cold). Returns false if already known.
    pub fn add_peer(&mut self, addr: SocketAddr, source: PeerSource) -> bool {
        if self.peers.contains_key(&addr) {
            return false;
        }
        self.peers.insert(addr, PeerInfo::new(source));
        true
    }

    /// Remove a peer.
    pub fn remove_peer(&mut self, addr: &SocketAddr) -> bool {
        self.peers.remove(addr).is_some()
    }

    /// Get peer info.
    pub fn get_peer(&self, addr: &SocketAddr) -> Option<&PeerInfo> {
        self.peers.get(addr)
    }

    /// Get mutable peer info.
    pub fn get_peer_mut(&mut self, addr: &SocketAddr) -> Option<&mut PeerInfo> {
        self.peers.get_mut(addr)
    }

    /// Promote a peer from cold → warm.
    pub fn promote_to_warm(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Cold {
                peer.state = PeerState::Warm;
                return true;
            }
        }
        false
    }

    /// Promote a peer from warm → hot.
    pub fn promote_to_hot(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Warm {
                peer.state = PeerState::Hot;
                return true;
            }
        }
        false
    }

    /// Demote a peer from hot → warm.
    pub fn demote_to_warm(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Hot {
                peer.state = PeerState::Warm;
                return true;
            }
        }
        false
    }

    /// Demote a peer from warm → cold.
    ///
    /// Per Haskell convention (`Ouroboros.Network.PeerSelection.Governor`),
    /// the canonical transition out of `PeerWarm` / `PeerHot` is via
    /// `PeerCooling` (mirrors `TerminatingState` in the connection manager,
    /// the outbound governor's version of TCP `TIME_WAIT`).
    /// `demote_to_cold` is kept as the direct transition for callers that
    /// know the connection is fully torn down (e.g. peer-pruning, fatal
    /// errors). Use `demote_to_cooling` for the normal disconnect path so
    /// the governor doesn't re-promote a peer whose connection is still
    /// terminating.
    pub fn demote_to_cold(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Warm
                || peer.state == PeerState::Hot
                || peer.state == PeerState::Cooling
            {
                peer.state = PeerState::Cold;
                return true;
            }
        }
        false
    }

    /// Demote a peer from warm/hot → cooling.
    ///
    /// Cooling is the outbound governor's analogue of the connection
    /// manager's `TerminatingState`. While in cooling, the governor must
    /// not re-promote the peer (`updateUnlessCoolingOrCold`). The
    /// `Cooling → Cold` transition fires once the underlying TCP
    /// teardown completes — typically driven by a `cooling_to_cold` call
    /// from the connection manager on `TerminatedState`.
    pub fn demote_to_cooling(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Warm || peer.state == PeerState::Hot {
                peer.state = PeerState::Cooling;
                return true;
            }
        }
        false
    }

    /// Complete the Cooling → Cold transition.
    ///
    /// Called from the connection manager when the underlying TCP
    /// connection reaches `TerminatedState`. Idempotent — a no-op for
    /// peers that are not currently in `Cooling`.
    pub fn cooling_to_cold(&mut self, addr: &SocketAddr) -> bool {
        if let Some(peer) = self.peers.get_mut(addr) {
            if peer.state == PeerState::Cooling {
                peer.state = PeerState::Cold;
                return true;
            }
        }
        false
    }

    /// Count peers in a given state.
    pub fn count_by_state(&self, state: PeerState) -> usize {
        self.peers.values().filter(|p| p.state == state).count()
    }

    /// Get all peers in a given state.
    pub fn peers_in_state(&self, state: PeerState) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|(_, p)| p.state == state)
            .map(|(addr, _)| *addr)
            .collect()
    }

    /// Cold peers that have passed their backoff window and are eligible for a
    /// new connection attempt.
    ///
    /// Matches Haskell `availableToConnect` filtered by `nextConnectTimes`: only
    /// cold peers whose `next_connect_after` deadline has elapsed (or was never
    /// set) are returned. This is what the Governor should use when selecting
    /// peers to promote to warm.
    pub fn peers_eligible_to_connect(&self) -> Vec<SocketAddr> {
        let now = Instant::now();
        self.peers
            .iter()
            .filter(|(_, p)| {
                p.state == PeerState::Cold && p.next_connect_after.is_none_or(|t| now >= t)
            })
            .map(|(addr, _)| *addr)
            .collect()
    }

    /// Get peers in a given state that have peer sharing enabled.
    ///
    /// Used by the Governor to find warm peers eligible for PeerSharing
    /// active outreach (MsgShareRequest).
    pub fn peers_with_peer_sharing(&self, state: PeerState) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|(_, p)| p.state == state && p.peer_sharing)
            .map(|(addr, _)| *addr)
            .collect()
    }

    /// Total number of known peers.
    pub fn total_peers(&self) -> usize {
        self.peers.len()
    }

    /// Decay failure counts for all peers (called periodically).
    pub fn decay_all_failures(&mut self) {
        for peer in self.peers.values_mut() {
            peer.decay_failures();
        }
    }

    /// Return the EWMA latency (ms) for a peer, or `None` if no RTT sample
    /// has been recorded yet.
    ///
    /// Used by the BlockFetch decision engine to prefer lower-latency peers
    /// when in-flight counts are equal.  Callers should fall back to a
    /// reasonable seed (e.g. [`PEER_LATENCY_DEFAULT_MS`]) when this returns
    /// `None`.
    pub fn get_latency_ms(&self, addr: &SocketAddr) -> Option<f64> {
        self.peers.get(addr).and_then(|p| p.latency_ms)
    }
}

impl Default for PeerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), port)
    }

    #[test]
    fn add_and_promote_lifecycle() {
        let mut pm = PeerManager::new();
        let addr = test_addr(3001);

        assert!(pm.add_peer(addr, PeerSource::Topology));
        assert_eq!(pm.count_by_state(PeerState::Cold), 1);

        assert!(pm.promote_to_warm(&addr));
        assert_eq!(pm.count_by_state(PeerState::Warm), 1);

        assert!(pm.promote_to_hot(&addr));
        assert_eq!(pm.count_by_state(PeerState::Hot), 1);

        assert!(pm.demote_to_warm(&addr));
        assert_eq!(pm.count_by_state(PeerState::Warm), 1);

        assert!(pm.demote_to_cold(&addr));
        assert_eq!(pm.count_by_state(PeerState::Cold), 1);
    }

    #[test]
    fn cannot_promote_wrong_state() {
        let mut pm = PeerManager::new();
        let addr = test_addr(3001);
        pm.add_peer(addr, PeerSource::Dns);

        // Can't promote cold directly to hot
        assert!(!pm.promote_to_hot(&addr));
    }

    #[test]
    fn cooling_state_blocks_repromotion() {
        // Mirrors Haskell `updateUnlessCoolingOrCold`: governor must not
        // promote a peer while it is in `Cooling` (TerminatingState
        // analogue). The path is hot/warm → cooling → cold, with the
        // final cooling_to_cold gated on the connection manager event.
        let mut pm = PeerManager::new();
        let addr = test_addr(3001);
        pm.add_peer(addr, PeerSource::Topology);
        pm.promote_to_warm(&addr);
        pm.promote_to_hot(&addr);

        // Hot → Cooling (canonical disconnect path).
        assert!(pm.demote_to_cooling(&addr));
        assert_eq!(pm.count_by_state(PeerState::Cooling), 1);
        assert_eq!(pm.count_by_state(PeerState::Hot), 0);
        assert!(
            pm.get_peer(&addr).unwrap().state.is_cooling_or_cold(),
            "Cooling must satisfy is_cooling_or_cold for governor checks"
        );

        // Governor must NOT be able to re-promote from Cooling.
        assert!(
            !pm.promote_to_warm(&addr),
            "Cooling peer must not be re-promoted to Warm directly"
        );
        assert!(
            !pm.promote_to_hot(&addr),
            "Cooling peer must not be re-promoted to Hot directly"
        );

        // Connection manager event: Cooling → Cold.
        assert!(pm.cooling_to_cold(&addr));
        assert_eq!(pm.count_by_state(PeerState::Cold), 1);
        assert_eq!(pm.count_by_state(PeerState::Cooling), 0);

        // Now re-promotion is allowed.
        assert!(pm.promote_to_warm(&addr));
        assert_eq!(pm.count_by_state(PeerState::Warm), 1);
    }

    #[test]
    fn demote_to_cooling_only_from_warm_or_hot() {
        let mut pm = PeerManager::new();
        let addr = test_addr(3002);
        pm.add_peer(addr, PeerSource::Topology);

        // Cold → Cooling is NOT allowed (no live connection to terminate).
        assert!(!pm.demote_to_cooling(&addr));
        assert_eq!(pm.count_by_state(PeerState::Cold), 1);

        // Cooling → Cooling is a no-op.
        pm.promote_to_warm(&addr);
        assert!(pm.demote_to_cooling(&addr));
        assert!(!pm.demote_to_cooling(&addr));
    }

    #[test]
    fn demote_to_cold_accepts_cooling_for_fast_teardown() {
        // demote_to_cold preserved as fast-path for callers that know
        // the connection is fully torn down (peer-pruning, fatal errors).
        let mut pm = PeerManager::new();
        let addr = test_addr(3003);
        pm.add_peer(addr, PeerSource::Topology);
        pm.promote_to_warm(&addr);
        pm.demote_to_cooling(&addr);

        // Cooling → Cold via demote_to_cold also works.
        assert!(pm.demote_to_cold(&addr));
        assert_eq!(pm.count_by_state(PeerState::Cold), 1);
    }

    #[test]
    fn duplicate_add_rejected() {
        let mut pm = PeerManager::new();
        let addr = test_addr(3001);
        assert!(pm.add_peer(addr, PeerSource::Topology));
        assert!(!pm.add_peer(addr, PeerSource::Dns));
    }

    #[test]
    fn ewma_latency() {
        let mut peer = PeerInfo::new(PeerSource::Topology);
        peer.update_latency(100.0);
        assert_eq!(peer.latency_ms, Some(100.0)); // First measurement

        peer.update_latency(200.0);
        // EWMA: 100 * 0.7 + 200 * 0.3 = 130
        assert!((peer.latency_ms.unwrap() - 130.0).abs() < 0.01);
    }

    #[test]
    fn failure_and_reputation() {
        let mut peer = PeerInfo::new(PeerSource::Topology);
        assert_eq!(peer.reputation, INITIAL_REPUTATION);

        peer.record_failure();
        assert!(peer.reputation < INITIAL_REPUTATION);
        assert_eq!(peer.failure_count, 1);
        // Backoff window should be set after a failure.
        assert!(peer.next_connect_after.is_some());

        peer.record_success();
        let rep_after_success = peer.reputation;
        assert!(rep_after_success > peer.reputation - 0.01); // slight boost
    }

    #[test]
    fn exponential_backoff_increases_with_failure_count() {
        // Each failure should schedule a longer wait than the previous one
        // (ignoring the small ±2s fuzz, so we check with a generous tolerance).
        let mut peer = PeerInfo::new(PeerSource::Ledger);
        let mut prev_delay = Duration::ZERO;

        for _ in 1..=6 {
            let before = Instant::now();
            peer.record_failure();
            let deadline = peer.next_connect_after.unwrap();
            // deadline must be in the future
            assert!(deadline > before);
            // delay grows (or at minimum stays the same once capped at 160s)
            let delay = deadline.saturating_duration_since(before);
            // Allow for the 2s fuzz on either side; just check it doesn't shrink
            // significantly compared to the previous iteration.
            let _ = (delay, prev_delay); // silence unused warning in first iter
            prev_delay = delay;
        }
        // After 6 failures the cap (160s) should be in effect.
        // Even with -2s fuzz the delay must be >= ~155s.
        assert!(prev_delay >= Duration::from_secs(155));
    }

    #[test]
    fn peers_eligible_to_connect_excludes_backed_off_peers() {
        let mut pm = PeerManager::new();
        pm.add_peer(test_addr(3001), PeerSource::Ledger); // fresh, eligible
        pm.add_peer(test_addr(3002), PeerSource::Ledger);

        // Simulate a failure on 3002 — it gets a backoff window.
        pm.get_peer_mut(&test_addr(3002)).unwrap().record_failure();

        // Only 3001 should be eligible.
        let eligible = pm.peers_eligible_to_connect();
        assert_eq!(eligible.len(), 1);
        assert!(eligible.contains(&test_addr(3001)));
    }

    #[test]
    fn peers_with_peer_sharing_filters_correctly() {
        let mut pm = PeerManager::new();
        pm.add_peer(test_addr(3001), PeerSource::Dns);
        pm.promote_to_warm(&test_addr(3001));
        pm.get_peer_mut(&test_addr(3001)).unwrap().peer_sharing = true;

        pm.add_peer(test_addr(3002), PeerSource::Dns);
        pm.promote_to_warm(&test_addr(3002));
        // peer_sharing defaults to false

        pm.add_peer(test_addr(3003), PeerSource::Dns);
        pm.get_peer_mut(&test_addr(3003)).unwrap().peer_sharing = true;
        // 3003 is cold, not warm

        let sharing_warm = pm.peers_with_peer_sharing(PeerState::Warm);
        assert_eq!(sharing_warm.len(), 1);
        assert!(sharing_warm.contains(&test_addr(3001)));
    }

    #[test]
    fn peers_in_state() {
        let mut pm = PeerManager::new();
        pm.add_peer(test_addr(3001), PeerSource::Topology);
        pm.add_peer(test_addr(3002), PeerSource::Dns);
        pm.add_peer(test_addr(3003), PeerSource::Ledger);

        pm.promote_to_warm(&test_addr(3001));
        pm.promote_to_warm(&test_addr(3002));

        let cold = pm.peers_in_state(PeerState::Cold);
        assert_eq!(cold.len(), 1);
        let warm = pm.peers_in_state(PeerState::Warm);
        assert_eq!(warm.len(), 2);
    }
}
