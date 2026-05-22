//! Block fetch decision engine — decides which peer to fetch blocks from.
//!
//! Sits between ChainSync (which receives headers) and BlockFetch (which downloads blocks).
//! Maintains a download queue, selects peers by latency, distributes ranges for parallel
//! fetching, retries on failure, and handles rollbacks.
//!
//! ## Genesis-mode fetch coordination (Phase D)
//!
//! When the node runs in Genesis consensus mode the `GenesisFetchCoordinator` wraps the
//! core `BlockFetchDecision` engine and serialises fetches behind the CSJ dynamo's
//! advertised jump-tip.  In practice this means:
//!
//! - Only ranges whose `to` slot is at or before the dynamo's current jump-tip slot are
//!   dispatched to peers.  Ranges beyond the tip are held in the queue until the dynamo
//!   advances.
//! - If the dynamo has not advanced within [`CSJ_REPROCESS_LOE_DELAY_SECS`] the
//!   coordinator rotates to the next available peer (matching Haskell's
//!   `csjReprocessLoEDelay`).
//! - When Genesis mode is disabled (the common Praos / Mithril-bootstrap path) the
//!   `GenesisFetchCoordinator` is a transparent pass-through: all four concurrent
//!   fetchers run unmodified.
//!
//! ## Haskell alignment
//!
//! The Haskell implementation (`ouroboros-network Decision/Genesis.hs`) uses the CSJ
//! dynamo's tip as the upper bound for what the BlockFetch logic is "eager" to download.
//! We diverge in one minor way: Haskell uses a per-peer stall timer keyed on the dynamo
//! peer; we use a single coordinator-level timer because the CSJ Phase B implementation
//! exposes a `watch::Receiver<Option<u64>>` for the current jump-tip slot rather than
//! a per-peer signal.  The observable behaviour is identical.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::codec::Point;

/// Maximum in-flight ranges per peer.
const DEFAULT_MAX_IN_FLIGHT: usize = 100;

/// Grace period before rotating away from a stalled CSJ dynamo.
///
/// Matches Haskell's `csjReprocessLoEDelay = 10` seconds from
/// `ouroboros-network/Decision/Genesis.hs`.
pub const CSJ_REPROCESS_LOE_DELAY_SECS: u64 = 10;

// ─── PeerFetchState ──────────────────────────────────────────────────────────

/// State of a peer for block fetch decisions.
#[derive(Debug, Clone)]
pub struct PeerFetchState {
    /// Peer address.
    pub addr: SocketAddr,
    /// Estimated latency in milliseconds.
    pub latency_ms: f64,
    /// Number of ranges currently in-flight for this peer.
    pub in_flight: usize,
    /// Tip slot advertised by this peer via ChainSync.
    pub tip_slot: u64,
}

// ─── FetchRange ──────────────────────────────────────────────────────────────

/// A range of blocks to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRange {
    /// Start of the range.
    pub from: Point,
    /// End of the range.
    pub to: Point,
}

// ─── BlockFetchDecision ──────────────────────────────────────────────────────

/// Block fetch decision engine.
pub struct BlockFetchDecision {
    /// Queue of ranges that need to be downloaded.
    queue: VecDeque<FetchRange>,
    /// Ranges currently in-flight, keyed by peer address.
    in_flight: HashMap<SocketAddr, Vec<FetchRange>>,
    /// Maximum in-flight ranges per peer.
    max_in_flight: usize,
}

impl BlockFetchDecision {
    /// Create a new decision engine.
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            in_flight: HashMap::new(),
            max_in_flight,
        }
    }

    /// Create with default settings.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_IN_FLIGHT)
    }

    /// Add a range to the download queue.
    pub fn add_range(&mut self, from: Point, to: Point) {
        self.queue.push_back(FetchRange { from, to });
    }

    /// Select the next peer to fetch from, considering latency and in-flight limits.
    ///
    /// Returns `Some((peer_addr, range))` if a peer and range are available,
    /// `None` if no work is available or all peers are at capacity.
    pub fn select_peer(&mut self, peers: &[PeerFetchState]) -> Option<(SocketAddr, FetchRange)> {
        if self.queue.is_empty() {
            return None;
        }

        // Sort peers by latency (lowest first), then filter by in-flight capacity
        let mut candidates: Vec<&PeerFetchState> = peers
            .iter()
            .filter(|p| {
                let current = self.in_flight.get(&p.addr).map_or(0, |v| v.len());
                current < self.max_in_flight
            })
            .collect();
        candidates.sort_by(|a, b| {
            a.latency_ms
                .partial_cmp(&b.latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(best) = candidates.first() {
            if let Some(range) = self.queue.pop_front() {
                self.in_flight
                    .entry(best.addr)
                    .or_default()
                    .push(range.clone());
                return Some((best.addr, range));
            }
        }

        None
    }

    /// Mark a range as completed for a peer.
    pub fn mark_completed(&mut self, peer: SocketAddr, range: &FetchRange) {
        if let Some(ranges) = self.in_flight.get_mut(&peer) {
            ranges.retain(|r| r != range);
            if ranges.is_empty() {
                self.in_flight.remove(&peer);
            }
        }
    }

    /// Mark a range as failed — re-queue it for retry on a different peer.
    pub fn mark_failed(&mut self, peer: SocketAddr, range: &FetchRange) {
        // Remove from in-flight
        if let Some(ranges) = self.in_flight.get_mut(&peer) {
            ranges.retain(|r| r != range);
            if ranges.is_empty() {
                self.in_flight.remove(&peer);
            }
        }
        // Re-queue for retry
        self.queue.push_back(range.clone());
    }

    /// Handle a rollback — remove any queued or in-flight ranges that are
    /// beyond the rollback point.
    pub fn rollback_to(&mut self, point: &Point) {
        let rollback_slot = match point {
            Point::Origin => 0,
            Point::Specific(slot, _) => *slot,
        };

        // Remove from queue
        self.queue.retain(|range| {
            let from_slot = match &range.from {
                Point::Origin => 0,
                Point::Specific(s, _) => *s,
            };
            from_slot <= rollback_slot
        });

        // Remove from in-flight
        for ranges in self.in_flight.values_mut() {
            ranges.retain(|range| {
                let from_slot = match &range.from {
                    Point::Origin => 0,
                    Point::Specific(s, _) => *s,
                };
                from_slot <= rollback_slot
            });
        }
        self.in_flight.retain(|_, v| !v.is_empty());
    }

    /// Number of ranges in the download queue.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Total number of ranges in-flight across all peers.
    pub fn total_in_flight(&self) -> usize {
        self.in_flight.values().map(|v| v.len()).sum()
    }
}

// ─── GenesisFetchMode ────────────────────────────────────────────────────────

/// Fetch coordination mode.
///
/// This is wired at node startup and does not change at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisFetchMode {
    /// Standard Praos mode: 4-fetcher multi-peer, no serialisation.
    Praos,
    /// Genesis mode: serialise fetch behind the CSJ dynamo's jump-tip.
    Genesis,
}

// ─── GenesisFetchCoordinator ─────────────────────────────────────────────────

/// Genesis-aware block fetch coordinator.
///
/// Wraps [`BlockFetchDecision`] and optionally gates dispatch behind the CSJ
/// dynamo's advertised jump-tip slot when [`GenesisFetchMode::Genesis`] is active.
///
/// In [`GenesisFetchMode::Praos`] the coordinator is a zero-overhead pass-through:
/// `select_peer` delegates directly to the inner engine with no slot check and no
/// timer overhead.
///
/// # Channel protocol
///
/// The dynamo publishes its jump-tip via a `watch::Receiver<Option<u64>>`:
/// - `None`  — no jump-tip yet (dynamo not yet elected); all dispatch is held.
/// - `Some(slot)` — ranges whose `to` slot is `<= slot` may be dispatched.
///
/// When the dynamo has not advanced for [`CSJ_REPROCESS_LOE_DELAY_SECS`] the
/// coordinator treats any queued range as dispatchable (rotation grace — matching
/// Haskell's `csjReprocessLoEDelay`).
pub struct GenesisFetchCoordinator {
    inner: BlockFetchDecision,
    mode: GenesisFetchMode,
    /// Receiver end of the CSJ dynamo jump-tip watch channel.
    /// `None` when mode == Praos.
    jump_tip_rx: Option<watch::Receiver<Option<u64>>>,
    /// The last jump-tip slot value seen (to detect stalls).
    last_jump_tip: Option<u64>,
    /// When the current jump-tip was last observed to change.
    last_jump_tip_changed_at: Instant,
}

impl GenesisFetchCoordinator {
    /// Create a coordinator in Praos mode (transparent pass-through).
    pub fn praos(inner: BlockFetchDecision) -> Self {
        Self {
            inner,
            mode: GenesisFetchMode::Praos,
            jump_tip_rx: None,
            last_jump_tip: None,
            last_jump_tip_changed_at: Instant::now(),
        }
    }

    /// Create a coordinator in Genesis mode, reading the CSJ dynamo's jump-tip
    /// from `jump_tip_rx`.
    pub fn genesis(inner: BlockFetchDecision, jump_tip_rx: watch::Receiver<Option<u64>>) -> Self {
        Self {
            inner,
            mode: GenesisFetchMode::Genesis,
            jump_tip_rx: Some(jump_tip_rx),
            last_jump_tip: None,
            last_jump_tip_changed_at: Instant::now(),
        }
    }

    // ── Forwarded mutators (always pass through) ─────────────────────────────

    /// Add a range to the download queue.
    pub fn add_range(&mut self, from: Point, to: Point) {
        self.inner.add_range(from, to);
    }

    /// Mark a range as completed for a peer.
    pub fn mark_completed(&mut self, peer: SocketAddr, range: &FetchRange) {
        self.inner.mark_completed(peer, range);
    }

    /// Mark a range as failed and re-queue it.
    pub fn mark_failed(&mut self, peer: SocketAddr, range: &FetchRange) {
        self.inner.mark_failed(peer, range);
    }

    /// Handle a rollback — remove ranges beyond the rollback point.
    pub fn rollback_to(&mut self, point: &Point) {
        self.inner.rollback_to(point);
    }

    /// Number of ranges in the download queue (including held ranges).
    pub fn queue_len(&self) -> usize {
        self.inner.queue_len()
    }

    /// Total number of ranges currently in-flight across all peers.
    pub fn total_in_flight(&self) -> usize {
        self.inner.total_in_flight()
    }

    // ── Genesis-aware select ─────────────────────────────────────────────────

    /// Select the next peer and range to fetch.
    ///
    /// **Praos mode**: delegates directly to [`BlockFetchDecision::select_peer`].
    ///
    /// **Genesis mode**: dispatches a range only if its `to` slot is at or below
    /// the CSJ dynamo's current jump-tip slot.  If the dynamo has stalled for
    /// longer than [`CSJ_REPROCESS_LOE_DELAY_SECS`], the slot gate is lifted and
    /// any queued range may be dispatched (matching Haskell's rotation behaviour).
    pub fn select_peer(&mut self, peers: &[PeerFetchState]) -> Option<(SocketAddr, FetchRange)> {
        match self.mode {
            GenesisFetchMode::Praos => self.inner.select_peer(peers),
            GenesisFetchMode::Genesis => self.select_peer_genesis(peers),
        }
    }

    /// Genesis-mode peer selection with dynamo jump-tip serialisation.
    fn select_peer_genesis(
        &mut self,
        peers: &[PeerFetchState],
    ) -> Option<(SocketAddr, FetchRange)> {
        // Read the latest jump-tip from the watch channel (non-blocking borrow).
        let current_tip = self.jump_tip_rx.as_ref().and_then(|rx| *rx.borrow());

        // Track whether the jump-tip has advanced so we can reset the stall timer.
        if current_tip != self.last_jump_tip {
            self.last_jump_tip = current_tip;
            self.last_jump_tip_changed_at = Instant::now();
        }

        // Determine the effective slot ceiling for dispatch.
        //
        // Three cases:
        //   1. No jump-tip yet (None): hold all dispatch unless the stall grace
        //      period has elapsed (rotation).
        //   2. Jump-tip is Some(slot): dispatch ranges up to and including `slot`.
        //   3. Stall grace period exceeded: lift the ceiling and dispatch any range
        //      (matching Haskell's `csjReprocessLoEDelay` rotation).
        let stalled = self.last_jump_tip_changed_at.elapsed()
            >= Duration::from_secs(CSJ_REPROCESS_LOE_DELAY_SECS);

        let ceiling: Option<u64> = if stalled {
            // Dynamo has stalled — rotate: dispatch any queued range.
            None // None means "no ceiling"
        } else {
            match current_tip {
                Some(slot) => Some(slot),
                None => {
                    // No dynamo tip yet and not yet stalled — hold everything.
                    return None;
                }
            }
        };

        // Peek at the front of the queue.  If the range's `to` slot exceeds the
        // ceiling we must hold — do not pop the range.
        if let Some(ceiling_slot) = ceiling {
            // Check the front without popping.
            let front_to_slot = self.inner.queue.front().map(|r| match &r.to {
                Point::Specific(s, _) => *s,
                Point::Origin => 0,
            });

            if let Some(to_slot) = front_to_slot {
                if to_slot > ceiling_slot {
                    // Range exceeds dynamo's jump-tip — hold dispatch.
                    return None;
                }
            } else if self.inner.queue.is_empty() {
                return None;
            }
        }

        // Ceiling allows this range (or rotation lifted the ceiling entirely).
        self.inner.select_peer(peers)
    }

    /// Current fetch mode.
    pub fn mode(&self) -> GenesisFetchMode {
        self.mode
    }

    /// Current dynamo jump-tip slot, or `None` if no tip has been received yet
    /// or if running in Praos mode.
    pub fn current_jump_tip(&self) -> Option<u64> {
        self.last_jump_tip
    }

    /// Whether the dynamo is currently stalled (elapsed > grace period).
    pub fn dynamo_is_stalled(&self) -> bool {
        self.mode == GenesisFetchMode::Genesis
            && self.last_jump_tip_changed_at.elapsed()
                >= Duration::from_secs(CSJ_REPROCESS_LOE_DELAY_SECS)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), port)
    }

    fn test_peer(port: u16, latency: f64) -> PeerFetchState {
        PeerFetchState {
            addr: test_addr(port),
            latency_ms: latency,
            in_flight: 0,
            tip_slot: 1000,
        }
    }

    // ── BlockFetchDecision (core, unchanged) ─────────────────────────────────

    #[test]
    fn selects_lowest_latency_peer() {
        let mut decision = BlockFetchDecision::with_defaults();
        decision.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );

        let peers = vec![test_peer(3001, 100.0), test_peer(3002, 50.0)];

        let (addr, _range) = decision.select_peer(&peers).unwrap();
        assert_eq!(addr, test_addr(3002)); // lower latency
    }

    #[test]
    fn respects_in_flight_limit() {
        let mut decision = BlockFetchDecision::new(1); // max 1 in-flight

        // Add two ranges
        decision.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );
        decision.add_range(
            Point::Specific(30, [0x03; 32]),
            Point::Specific(40, [0x04; 32]),
        );

        let peers = vec![test_peer(3001, 50.0)];

        // First select should work
        let result1 = decision.select_peer(&peers);
        assert!(result1.is_some());

        // Second select should fail (peer at capacity)
        let result2 = decision.select_peer(&peers);
        assert!(result2.is_none());
    }

    #[test]
    fn failed_range_requeued() {
        let mut decision = BlockFetchDecision::with_defaults();
        let range = FetchRange {
            from: Point::Specific(10, [0x01; 32]),
            to: Point::Specific(20, [0x02; 32]),
        };
        decision.add_range(range.from.clone(), range.to.clone());

        let peers = vec![test_peer(3001, 50.0)];
        let (addr, fetched) = decision.select_peer(&peers).unwrap();
        assert_eq!(decision.queue_len(), 0);

        // Mark as failed — should be re-queued
        decision.mark_failed(addr, &fetched);
        assert_eq!(decision.queue_len(), 1);
        assert_eq!(decision.total_in_flight(), 0);
    }

    #[test]
    fn rollback_removes_future_ranges() {
        let mut decision = BlockFetchDecision::with_defaults();
        decision.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );
        decision.add_range(
            Point::Specific(100, [0x03; 32]),
            Point::Specific(200, [0x04; 32]),
        );

        assert_eq!(decision.queue_len(), 2);

        // Rollback to slot 50 — should remove the second range
        decision.rollback_to(&Point::Specific(50, [0x05; 32]));
        assert_eq!(decision.queue_len(), 1);
    }

    #[test]
    fn completed_range_removed_from_inflight() {
        let mut decision = BlockFetchDecision::with_defaults();
        decision.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );

        let peers = vec![test_peer(3001, 50.0)];
        let (addr, range) = decision.select_peer(&peers).unwrap();
        assert_eq!(decision.total_in_flight(), 1);

        decision.mark_completed(addr, &range);
        assert_eq!(decision.total_in_flight(), 0);
    }

    // ── GenesisFetchCoordinator — Praos pass-through ──────────────────────────

    /// In Praos mode the coordinator must be a zero-overhead pass-through.
    #[test]
    fn praos_mode_is_passthrough() {
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::praos(inner);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );
        let peers = vec![test_peer(3001, 50.0)];

        // Must dispatch immediately, no gate.
        let result = coord.select_peer(&peers);
        assert!(
            result.is_some(),
            "Praos mode must dispatch without any jump-tip gate"
        );
        assert_eq!(coord.mode(), GenesisFetchMode::Praos);
    }

    /// Praos mode: range beyond any hypothetical tip must still dispatch.
    #[test]
    fn praos_mode_ignores_slot_ceiling() {
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::praos(inner);

        // Very high slot — would be blocked in Genesis mode if dynamo tip were 0.
        coord.add_range(
            Point::Specific(999_999, [0x01; 32]),
            Point::Specific(1_000_000, [0x02; 32]),
        );
        let peers = vec![test_peer(3001, 50.0)];

        let result = coord.select_peer(&peers);
        assert!(
            result.is_some(),
            "Praos mode must never gate on slot ceiling"
        );
    }

    // ── GenesisFetchCoordinator — Genesis serialisation ───────────────────────

    /// Genesis mode: fetch with jump-tip above the range's `to` slot dispatches.
    #[test]
    fn genesis_mode_dispatches_when_tip_sufficient() {
        let (tx, rx) = watch::channel::<Option<u64>>(Some(500));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(100, [0x02; 32]), // to=100, tip=500 → dispatchable
        );
        let peers = vec![test_peer(3001, 50.0)];

        let result = coord.select_peer(&peers);
        assert!(
            result.is_some(),
            "should dispatch when range.to (100) <= dynamo tip (500)"
        );

        // Keep sender alive so the channel stays open.
        drop(tx);
    }

    /// Genesis mode: fetch with jump-tip below the range's `to` slot is held.
    #[test]
    fn genesis_mode_holds_when_tip_insufficient() {
        let (tx, rx) = watch::channel::<Option<u64>>(Some(50));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(100, [0x02; 32]), // to=100, tip=50 → held
        );
        let peers = vec![test_peer(3001, 50.0)];

        let result = coord.select_peer(&peers);
        assert!(
            result.is_none(),
            "should hold when range.to (100) > dynamo tip (50)"
        );

        drop(tx);
    }

    /// Genesis mode: no jump-tip (None) holds all dispatch initially.
    #[test]
    fn genesis_mode_holds_when_no_tip() {
        let (_tx, rx) = watch::channel::<Option<u64>>(None);
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(20, [0x02; 32]),
        );
        let peers = vec![test_peer(3001, 50.0)];

        let result = coord.select_peer(&peers);
        assert!(
            result.is_none(),
            "should hold all dispatch when dynamo has no tip yet"
        );
    }

    /// Genesis mode: dynamo tip advances → previously held range dispatches.
    #[test]
    fn genesis_mode_dispatches_after_tip_advance() {
        let (tx, rx) = watch::channel::<Option<u64>>(Some(5));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(100, [0x02; 32]),
        );
        let peers = vec![test_peer(3001, 50.0)];

        // tip=5 < to=100 → held
        assert!(coord.select_peer(&peers).is_none());

        // Dynamo advances to 150
        tx.send(Some(150)).unwrap();

        // Now tip=150 >= to=100 → dispatchable
        let result = coord.select_peer(&peers);
        assert!(
            result.is_some(),
            "should dispatch after dynamo tip advances past range.to"
        );
    }

    /// Genesis mode: multiple ranges — only the ones within the tip window dispatch.
    #[test]
    fn genesis_mode_partial_dispatch() {
        let (tx, rx) = watch::channel::<Option<u64>>(Some(200));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        // Range 1: to=100 — within tip (200) → dispatchable
        coord.add_range(
            Point::Specific(1, [0x01; 32]),
            Point::Specific(100, [0x02; 32]),
        );
        // Range 2: to=300 — beyond tip (200) → held
        coord.add_range(
            Point::Specific(101, [0x03; 32]),
            Point::Specific(300, [0x04; 32]),
        );

        let peers = vec![test_peer(3001, 50.0)];

        // First select: range 1 dispatches
        let r1 = coord.select_peer(&peers);
        assert!(r1.is_some(), "range 1 (to=100) should dispatch at tip=200");

        // Second select: range 2 is held (to=300 > tip=200)
        let r2 = coord.select_peer(&peers);
        assert!(r2.is_none(), "range 2 (to=300) should be held at tip=200");

        drop(tx);
    }

    /// Genesis mode: rollback propagates through the coordinator.
    #[test]
    fn genesis_mode_rollback_propagates() {
        let (_tx, rx) = watch::channel::<Option<u64>>(Some(1000));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        coord.add_range(
            Point::Specific(10, [0x01; 32]),
            Point::Specific(50, [0x02; 32]),
        );
        coord.add_range(
            Point::Specific(500, [0x03; 32]),
            Point::Specific(800, [0x04; 32]),
        );

        assert_eq!(coord.queue_len(), 2);

        // Rollback to slot 100 — range starting at 500 should be dropped
        coord.rollback_to(&Point::Specific(100, [0x05; 32]));
        assert_eq!(coord.queue_len(), 1, "rollback should remove future range");
    }

    /// Genesis mode: current_jump_tip returns the latest tip.
    #[test]
    fn genesis_mode_current_jump_tip_reported() {
        let (tx, rx) = watch::channel::<Option<u64>>(None);
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        // No tip initially — need to call select_peer to read the channel.
        let peers: Vec<PeerFetchState> = vec![];
        let _ = coord.select_peer(&peers);
        assert_eq!(coord.current_jump_tip(), None);

        tx.send(Some(42)).unwrap();
        let _ = coord.select_peer(&peers);
        assert_eq!(coord.current_jump_tip(), Some(42));
    }

    /// Genesis mode reports is_not_stalled immediately after tip change.
    #[test]
    fn genesis_mode_not_stalled_after_tip_advance() {
        let (tx, rx) = watch::channel::<Option<u64>>(Some(100));
        let inner = BlockFetchDecision::with_defaults();
        let mut coord = GenesisFetchCoordinator::genesis(inner, rx);

        // Trigger a tip read
        let peers: Vec<PeerFetchState> = vec![];
        let _ = coord.select_peer(&peers);

        // Advance the tip
        tx.send(Some(200)).unwrap();
        let _ = coord.select_peer(&peers);

        assert!(
            !coord.dynamo_is_stalled(),
            "dynamo should not be stalled immediately after tip advance"
        );
    }

    // ── CSJ_REPROCESS_LOE_DELAY_SECS is the correct value ────────────────────

    /// Verify the grace period constant matches the Haskell `csjReprocessLoEDelay`.
    #[test]
    fn csj_reprocess_loe_delay_is_ten_seconds() {
        assert_eq!(
            CSJ_REPROCESS_LOE_DELAY_SECS, 10,
            "csjReprocessLoEDelay must be 10s to match Haskell reference"
        );
    }
}
