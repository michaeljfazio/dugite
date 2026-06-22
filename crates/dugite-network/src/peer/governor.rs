//! Governor — target-driven peer promotion/demotion decisions.
//!
//! The Governor compares current peer state counts against configured targets
//! and emits actions (promote, demote, discover) to bring the counts in line.
//!
//! ## Churn
//! Periodically rotates peers to prevent stale connections and improve
//! network health (every 10-20 minutes, matching Haskell).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use super::manager::{PeerManager, PeerState};
use super::selection::{
    select_best_cold_eligible, select_best_warm, select_lowest_reputation_cold, select_worst_hot,
    select_worst_warm,
};

/// Target peer counts for the governor.
///
/// Aggregate targets (`target_warm`, `target_hot`, `max_cold`) apply to ALL
/// peers regardless of category. Big-ledger targets enforce a MINIMUM number
/// of warm/hot connections to "Big Ledger Peers" (the top-90% of stake — see
/// `gsm::identify_big_ledger_peers`). Big-ledger peers count toward the
/// aggregate targets too, so `target_hot_big_ledger <= target_hot` should
/// hold (typically 5 BLPs out of 20 total hot, matching cardano-node's
/// `TargetNumberOfActiveBigLedgerPeers` default).
#[derive(Debug, Clone)]
pub struct PeerTargets {
    /// Target number of warm peers (TCP connected, keepalive).
    pub target_warm: usize,
    /// Target number of hot peers (fully syncing).
    pub target_hot: usize,
    /// Maximum number of cold peers to track.
    pub max_cold: usize,
    /// Target number of BLPs that must be warm or hot.
    /// Default 0 disables the BLP-specific minimum.
    pub target_warm_big_ledger: usize,
    /// Target number of BLPs that must be hot.
    /// Default 0 disables the BLP-specific minimum.
    pub target_hot_big_ledger: usize,
}

impl Default for PeerTargets {
    fn default() -> Self {
        // Production defaults — match Haskell `cardano-diffusion`'s
        // `defaultDeadlineTargets` (lines in
        // `cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs`).
        // Previously dugite shipped `target_hot=5, target_warm=10` which is
        // far below Haskell's `targetNumberOfActivePeers=20,
        // targetNumberOfEstablishedPeers=30`.  The low cap caused
        // `aboveTargetOther` to fire continuously whenever any inbound
        // promoted peer pushed `hot_count > 5` — driving the OOM-on-churn
        // class of bugs documented in #703.
        Self {
            target_warm: 30,
            target_hot: 20,
            max_cold: 150,
            // Big-ledger active peers (Haskell `targetNumberOfActiveBigLedgerPeers = 5`).
            target_warm_big_ledger: 5,
            target_hot_big_ledger: 5,
        }
    }
}

/// Governor configuration.
#[derive(Debug, Clone)]
pub struct GovernorConfig {
    /// Peer count targets.
    pub targets: PeerTargets,
    /// Minimum interval between hot churn rotations (demote worst hot, promote
    /// best warm) while the node is caught up — the *deadline* churn cadence.
    ///
    /// Maps to Haskell `defaultDeadlineChurnInterval = 3300 s` (55 min). Used
    /// whenever the governor is not in bulk-sync mode (see [`Governor::set_bulk_sync_mode`]).
    pub hot_churn_interval: Duration,
    /// Minimum interval between hot churn rotations while the node is bulk
    /// syncing (behind the network tip) — the faster *bulk-sync* churn cadence
    /// that sheds slow peers quickly during catch-up.
    ///
    /// Maps to Haskell `defaultBulkChurnInterval = 900 s` (15 min). Selected
    /// over [`hot_churn_interval`] only while [`Governor::bulk_sync_mode`] is set.
    pub bulk_sync_churn_interval: Duration,
    /// Minimum interval between cold churn sweeps (forget lowest-reputation cold peers).
    pub cold_churn_interval: Duration,
    /// Minimum interval between warm churn rotations (demote worst warm, promote best cold).
    pub warm_churn_interval: Duration,
    /// Per-peer cooldown after a hot→warm demotion during which the peer
    /// is excluded from Cold→Warm and Warm→Hot re-promotion candidates.
    ///
    /// Mirrors Haskell's `policyPeerShareActivationDelay` (300s) and
    /// `minActivateTime` in `Ouroboros.Network.PeerSelection.Governor`.
    /// Without this, a peer demoted by an `aboveTarget` decision is
    /// reconnected on the next governor tick (~2s) via the `#516`
    /// single-use-channel workaround, immediately re-promoted, and
    /// re-demoted — a 4-second flap loop that the preprod soak surfaced
    /// in issue #671 (264 demotes / 31 minutes, worst peer churned 133×).
    pub demote_cooldown: Duration,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            targets: PeerTargets::default(),
            // Haskell `defaultDeadlineChurnInterval = 3300 s` + jitter of up
            // to 600 s.  3300 s = 55 min.  Using the lower bound here for
            // determinism; #703 fix B adds jitter via inbound maturation.
            // Previously this was 600 s — 5.5× more aggressive than Haskell,
            // contributing to the demote-promote-demote loop in #703.
            hot_churn_interval: Duration::from_secs(3300), // 55 minutes (Haskell deadline default)
            bulk_sync_churn_interval: Duration::from_secs(900), // 15 minutes (Haskell bulk-sync default)
            cold_churn_interval: Duration::from_secs(900),
            warm_churn_interval: Duration::from_secs(600),
            // 5 minutes — matches Haskell's `policyPeerShareActivationDelay`.
            // Long enough to prevent the 4-second flap loop reproduced in
            // issue #671 but short enough that legitimately-churned peers
            // re-enter the active set within a single churn cycle.
            demote_cooldown: Duration::from_secs(300),
        }
    }
}

/// Per-group local root target, passed to the governor so it can
/// independently ensure each group meets its warm/hot valency.
///
/// Matches Haskell's `belowTargetLocal` in `EstablishedPeers.hs` and
/// `ActivePeers.hs`: each local root group is checked independently
/// against its own warm and hot valency targets, regardless of aggregate
/// peer counts.
#[derive(Debug, Clone)]
pub struct LocalRootGroupTarget {
    /// Addresses belonging to this group.
    pub members: HashSet<SocketAddr>,
    /// Target warm (established) peers in this group.
    pub warm_valency: usize,
    /// Target hot (active) peers in this group.
    pub hot_valency: usize,
}

/// Actions the governor can emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernorAction {
    /// Promote a cold peer to warm (establish TCP connection).
    PromoteToWarm(SocketAddr),
    /// Promote a warm peer to hot (start sync protocols).
    PromoteToHot(SocketAddr),
    /// Demote a hot peer to warm (stop sync protocols).
    DemoteToWarm(SocketAddr),
    /// Demote a warm peer to cold (close TCP connection).
    DemoteToCold(SocketAddr),
    /// Discover more peers (not enough cold peers).
    DiscoverMore,
    /// Forget (remove) a cold peer from the registry.
    ///
    /// Emitted during cold churn to evict lowest-reputation non-topology peers
    /// when the cold pool exceeds 150% of `max_cold`.
    ForgetPeer(SocketAddr),
    /// Request peer addresses from a warm peer via PeerSharing protocol.
    ///
    /// Emitted when the cold pool is low and a warm peer with PeerSharing
    /// capability is available. The connection orchestrator should send
    /// `MsgShareRequest` and add discovered routable addresses to the cold set.
    PeerShareRequest(SocketAddr),
}

/// Peer governor — computes actions to bring peer counts to targets.
///
/// Implements three independent churn timers matching Haskell's
/// `Ouroboros.Network.PeerSelection.Governor`:
/// - Hot churn: rotates one hot peer (demote worst, promote best warm)
/// - Cold churn: forgets lowest-reputation cold peers when pool is oversized
/// - Warm churn: rotates warm peers based on quality scoring
pub struct Governor {
    config: GovernorConfig,
    last_hot_churn: Instant,
    last_cold_churn: Instant,
    last_warm_churn: Instant,
    /// Peers for which a cold→warm (connect) promotion is currently in
    /// flight asynchronously.  Cleared by `promotion_cold_completed()`.
    /// Prevents double-promotion across governor ticks when async
    /// connection attempts are slow to complete.
    in_progress_promote_cold: HashSet<SocketAddr>,
    /// Peers for which a warm→hot (activate) promotion is currently in
    /// flight asynchronously.  Cleared by `promotion_warm_completed()`.
    in_progress_promote_warm: HashSet<SocketAddr>,
    /// Per-peer post-demote cooldown timestamps. Each entry is the
    /// `Instant` at which the cooldown expires (i.e. demote time +
    /// `demote_cooldown`). Peers in this map are excluded from
    /// Cold→Warm and Warm→Hot promotion candidates until their entry
    /// expires; the lazy GC removes expired entries on each governor
    /// tick.
    ///
    /// See issue #671 — without this, a peer demoted by `aboveTarget`
    /// (or by the BLP-aggregate cap added in the same fix) is
    /// immediately re-promoted on the next 2-second governor tick via
    /// the Cold→Warm reconnect path that the `#516` single-use-channel
    /// workaround forces.
    recently_demoted: HashMap<SocketAddr, Instant>,
    /// When `true`, hot churn uses [`GovernorConfig::bulk_sync_churn_interval`]
    /// (faster) instead of [`GovernorConfig::hot_churn_interval`]. Set by the
    /// node from its at-tip signal: bulk-syncing (behind tip) → `true`,
    /// caught-up → `false`. Mirrors the Haskell `FetchMode` BulkSync/Deadline
    /// churn-cadence split.
    bulk_sync_mode: bool,
}

impl Governor {
    /// Create a new governor with the given configuration.
    pub fn new(config: GovernorConfig) -> Self {
        let now = Instant::now();
        Self {
            config,
            last_hot_churn: now,
            last_cold_churn: now,
            last_warm_churn: now,
            in_progress_promote_cold: HashSet::new(),
            in_progress_promote_warm: HashSet::new(),
            recently_demoted: HashMap::new(),
            bulk_sync_mode: false,
        }
    }

    /// Record a hot→warm demotion in the cooldown map so the peer is
    /// excluded from Cold→Warm and Warm→Hot promotion candidates for
    /// `demote_cooldown` (default 300s — matches Haskell's
    /// `policyPeerShareActivationDelay`).
    ///
    /// Called internally by every code path that pushes a
    /// `GovernorAction::DemoteToWarm` so that all demote sites benefit
    /// from the flap-prevention behaviour added for issue #671.
    fn record_demote(&mut self, addr: SocketAddr, now: Instant) {
        self.recently_demoted
            .insert(addr, now + self.config.demote_cooldown);
    }

    /// Return `true` if the peer is in post-demote cooldown.
    fn in_cooldown(&self, addr: &SocketAddr, now: Instant) -> bool {
        self.recently_demoted
            .get(addr)
            .is_some_and(|expiry| now < *expiry)
    }

    /// Number of peers currently in post-demote cooldown. Test helper.
    #[cfg(test)]
    pub fn cooldown_size(&self) -> usize {
        self.recently_demoted.len()
    }

    /// Replace the governor's peer targets with new values from a live config
    /// update (e.g. delivered via a `tokio::sync::watch` channel on SIGHUP).
    ///
    /// Only the target fields (`target_warm`, `target_hot`, `max_cold`,
    /// `target_warm_big_ledger`, `target_hot_big_ledger`) are updated; churn
    /// intervals remain unchanged by this call. The updated targets take effect
    /// on the *next* call to [`compute_actions_with_blp`] — there is no
    /// retroactive effect on in-flight promotions.
    pub fn update_targets(&mut self, new_targets: PeerTargets) {
        self.config.targets = new_targets;
    }

    /// Update the hot-churn cadences from a live config reload (SIGHUP).
    ///
    /// `deadline` is the caught-up cadence (`ChurnIntervalNormalSecs`,
    /// Haskell `defaultDeadlineChurnInterval`); `bulk_sync` is the catch-up
    /// cadence (`ChurnIntervalSyncSecs`, Haskell `defaultBulkChurnInterval`).
    /// Takes effect on the next churn evaluation; in-flight timers are not reset.
    pub fn update_churn_intervals(&mut self, deadline: Duration, bulk_sync: Duration) {
        self.config.hot_churn_interval = deadline;
        self.config.bulk_sync_churn_interval = bulk_sync;
    }

    /// Switch the hot-churn cadence between bulk-sync (catch-up) and deadline
    /// (caught-up). The node drives this from its at-tip signal so that, while
    /// catching up, slow hot peers are rotated out faster — matching the
    /// Haskell `FetchMode` BulkSync/Deadline churn split.
    pub fn set_bulk_sync_mode(&mut self, on: bool) {
        self.bulk_sync_mode = on;
    }

    /// The hot-churn interval currently in effect (bulk-sync vs deadline).
    fn effective_hot_churn_interval(&self) -> Duration {
        if self.bulk_sync_mode {
            self.config.bulk_sync_churn_interval
        } else {
            self.config.hot_churn_interval
        }
    }

    /// Signal that a cold→warm promotion has completed (succeeded or failed).
    ///
    /// The caller — typically the connection orchestrator — must invoke this
    /// after every `GovernorAction::PromoteToWarm` attempt regardless of
    /// outcome so the governor can re-evaluate the peer on the next tick.
    pub fn promotion_cold_completed(&mut self, addr: &SocketAddr) {
        self.in_progress_promote_cold.remove(addr);
    }

    /// Signal that a warm→hot promotion has completed (succeeded or failed).
    ///
    /// Must be called after every `GovernorAction::PromoteToHot` attempt.
    pub fn promotion_warm_completed(&mut self, addr: &SocketAddr) {
        self.in_progress_promote_warm.remove(addr);
    }

    /// Compute the actions needed to bring peer counts toward targets.
    ///
    /// This is the main decision function, called periodically by the
    /// connection manager. It evaluates target-driven promotions/demotions
    /// first, then three independent churn timers, then peer discovery.
    ///
    /// Backwards-compatible wrapper that calls [`compute_actions_with_blp`]
    /// with an empty big-ledger set and no active-fetch-peer exclusion.
    /// Existing callers (and unit tests) that don't track BLPs separately
    /// can keep using this entry point.
    pub fn compute_actions(
        &mut self,
        peer_manager: &PeerManager,
        local_root_groups: &[LocalRootGroupTarget],
    ) -> Vec<GovernorAction> {
        let empty: HashSet<SocketAddr> = HashSet::new();
        self.compute_actions_with_blp(peer_manager, local_root_groups, &empty, &empty, None)
    }

    /// Like [`compute_actions`] but with explicit knowledge of which peers
    /// are Big Ledger Peers (BLPs) — top-90%-stake pools per
    /// `gsm::identify_big_ledger_peers`. The governor enforces
    /// `target_warm_big_ledger` / `target_hot_big_ledger` minimums against
    /// this set after local-root targets and before aggregate targets.
    ///
    /// BLP minimums apply IN ADDITION to aggregate targets. A peer that's
    /// both a BLP and a local-root member is handled by the local-root pass
    /// first (highest priority), then the BLP pass tops up if needed.
    ///
    /// `fresh_inbound` is the set of inbound-duplex peers that have been
    /// connected for less than `inboundMaturePeerDelay = 15 min` (Haskell's
    /// `InboundGovernor.inboundMaturePeerDelay`).  These peers stay in the
    /// Warm state but are EXCLUDED from `Warm→Hot` promotion candidates and
    /// from BLP / non-BLP hot fill paths.  Issue #703 fix B.  Without this,
    /// every inbound connect that completes handshake is immediately
    /// promoted to Hot, exceeding `target_hot` and triggering
    /// `aboveTargetOther` demotions that drove the at-tip OOM.
    ///
    /// `active_fetch_peer` is the `SocketAddr` of the peer currently holding
    /// the BlockFetch slot (i.e. actively downloading blocks), or `None`.
    /// When set, that peer is EXCLUDED from the `aboveTargetOther`
    /// hot→warm demotion candidates regardless of score. This prevents the
    /// governor from killing a mid-download fetch every ~5 s when the hot
    /// count temporarily exceeds `target_hot` after a post-restart connect
    /// burst.  The peer is still subject to `aboveTargetLocal`, BLP-aggregate,
    /// and hot-churn demotion paths — those are lower-frequency and either
    /// topology-mandatory or long-interval (55 min churn); only the
    /// high-frequency `aboveTargetOther` path that fires every 2-second
    /// governor tick is guarded.
    pub fn compute_actions_with_blp(
        &mut self,
        peer_manager: &PeerManager,
        local_root_groups: &[LocalRootGroupTarget],
        big_ledger_peers: &HashSet<SocketAddr>,
        fresh_inbound: &HashSet<SocketAddr>,
        active_fetch_peer: Option<SocketAddr>,
    ) -> Vec<GovernorAction> {
        use rand::seq::IndexedRandom;

        let mut actions = Vec::new();

        let now = Instant::now();

        // ── Local root membership ───────────────────────────────────────
        // Computed once and used by every belowTargetOther/aboveTarget
        // path below so local root peers are excluded from aggregate
        // promotion AND demotion — they are managed exclusively by the
        // per-group belowTargetLocal/aboveTargetLocal paths. Matches
        // Haskell's `Set.\\ LocalRootPeers.keysSet` exclusion.
        let local_root_members: HashSet<SocketAddr> = local_root_groups
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect();

        // ── Post-demote cooldown GC ─────────────────────────────────────
        // Mirror Haskell's `minActivateTime` housekeeping: drop expired
        // entries so the cooldown map cannot grow unbounded.
        // Local root peers are NEVER excluded by cooldown — they are
        // managed by the per-group path and must always be reconnected.
        self.recently_demoted
            .retain(|addr, expiry| now < *expiry && !local_root_members.contains(addr));

        let warm_count = peer_manager.count_by_state(PeerState::Warm);
        let hot_count = peer_manager.count_by_state(PeerState::Hot);
        let cold_count = peer_manager.count_by_state(PeerState::Cold);

        // ── Per-group local root promotions (belowTargetLocal) ─────────
        // Each local root group is checked independently against its own
        // warm and hot valency targets, matching Haskell's belowTargetLocal.
        // This runs BEFORE aggregate targets so local root deficiencies are
        // addressed with highest priority — a block producer's relays must
        // always be reconnected immediately.
        let mut already_promoted: HashSet<SocketAddr> = HashSet::new();
        // Compute eligible-to-connect set once (expensive — allocates and filters).
        let eligible_to_connect: HashSet<SocketAddr> = if local_root_groups.is_empty() {
            HashSet::new()
        } else {
            peer_manager
                .peers_eligible_to_connect()
                .into_iter()
                .collect()
        };

        for group in local_root_groups {
            // Count members that are warm or hot (established).
            let warm_or_hot_count = group
                .members
                .iter()
                .filter(|addr| {
                    peer_manager
                        .get_peer(addr)
                        .map(|p| p.state == PeerState::Warm || p.state == PeerState::Hot)
                        .unwrap_or(false)
                })
                .count();

            // Promote cold → warm if below warm_valency.
            if warm_or_hot_count < group.warm_valency {
                let needed = group.warm_valency - warm_or_hot_count;
                let mut promoted = 0;
                for addr in &group.members {
                    if promoted >= needed {
                        break;
                    }
                    if already_promoted.contains(addr) {
                        continue;
                    }
                    // Skip peers whose cold→warm promotion is already in flight.
                    if self.in_progress_promote_cold.contains(addr) {
                        continue;
                    }
                    if eligible_to_connect.contains(addr) {
                        actions.push(GovernorAction::PromoteToWarm(*addr));
                        self.in_progress_promote_cold.insert(*addr);
                        already_promoted.insert(*addr);
                        promoted += 1;
                    }
                }
            }

            // Count members that are hot.
            let hot_member_count = group
                .members
                .iter()
                .filter(|addr| {
                    peer_manager
                        .get_peer(addr)
                        .map(|p| p.state == PeerState::Hot)
                        .unwrap_or(false)
                })
                .count();

            // Promote warm → hot if below hot_valency.
            if hot_member_count < group.hot_valency {
                let needed = group.hot_valency - hot_member_count;
                let mut promoted = 0;
                for addr in &group.members {
                    if promoted >= needed {
                        break;
                    }
                    if already_promoted.contains(addr) {
                        continue;
                    }
                    // Skip peers whose warm→hot promotion is already in flight.
                    if self.in_progress_promote_warm.contains(addr) {
                        continue;
                    }
                    if let Some(peer) = peer_manager.get_peer(addr) {
                        if peer.state == PeerState::Warm {
                            actions.push(GovernorAction::PromoteToHot(*addr));
                            self.in_progress_promote_warm.insert(*addr);
                            already_promoted.insert(*addr);
                            promoted += 1;
                        }
                    }
                }
            }
        }

        // ── Big-ledger peer targets (belowTargetBigLedger) ─────────────────
        //
        // Enforce minimums on connections to Big Ledger Peers (BLPs — the
        // top-90%-stake pools per `gsm::identify_big_ledger_peers`). These
        // run AFTER local-root targets but BEFORE aggregate targets so that
        // BLP minimums are honoured even when the aggregate hot/warm count
        // is at its limit; they then count toward the aggregate too.
        //
        // Matches Haskell's `belowTarget` for big ledger peers in
        // `Ouroboros.Network.PeerSelection.Governor.{Established,Active}Peers`.
        if !big_ledger_peers.is_empty() {
            // BLP warm target: ensure at least target_warm_big_ledger BLPs
            // are warm or hot.
            let blp_warm_or_hot = big_ledger_peers
                .iter()
                .filter(|addr| {
                    peer_manager
                        .get_peer(addr)
                        .map(|p| p.state == PeerState::Warm || p.state == PeerState::Hot)
                        .unwrap_or(false)
                })
                .count();
            if blp_warm_or_hot < self.config.targets.target_warm_big_ledger {
                let needed = self.config.targets.target_warm_big_ledger - blp_warm_or_hot;
                let mut promoted = 0usize;
                let eligible_blps: Vec<SocketAddr> = peer_manager
                    .peers_eligible_to_connect()
                    .into_iter()
                    .filter(|a| big_ledger_peers.contains(a))
                    .collect();
                for addr in eligible_blps {
                    if promoted >= needed {
                        break;
                    }
                    if already_promoted.contains(&addr) {
                        continue;
                    }
                    if self.in_progress_promote_cold.contains(&addr) {
                        continue;
                    }
                    // Skip peers in post-demote cooldown — see #671.
                    if self.in_cooldown(&addr, now) {
                        continue;
                    }
                    actions.push(GovernorAction::PromoteToWarm(addr));
                    self.in_progress_promote_cold.insert(addr);
                    already_promoted.insert(addr);
                    promoted += 1;
                }
            }

            // BLP hot target: ensure at least target_hot_big_ledger BLPs
            // are hot.
            //
            // Per Haskell's `belowTargetBigLedgerPeers` in
            // `Governor.ActivePeers.hs`: the predicate is purely
            // `numActiveBigLedgerPeers < targetNumberOfActiveBigLedgerPeers`
            // — there is NO check against the aggregate
            // `targetNumberOfActivePeers`. BLPs can be promoted above
            // the aggregate cap; `aboveTargetOther` then demotes
            // non-BLPs to bring the aggregate back down. This is the
            // correct semantic, and combined with the per-peer
            // post-demote cooldown added in this same fix (#671), the
            // demoted non-BLPs land in Cold with a cooldown applied,
            // which prevents the previously-observed 4-second flap
            // loop after the #516 single-use-channel TCP close.
            //
            // See:
            // https://github.com/IntersectMBO/ouroboros-network/blob/main/ouroboros-network/lib/Ouroboros/Network/PeerSelection/Governor/ActivePeers.hs#L143-L165
            let blp_hot = big_ledger_peers
                .iter()
                .filter(|addr| {
                    peer_manager
                        .get_peer(addr)
                        .map(|p| p.state == PeerState::Hot)
                        .unwrap_or(false)
                })
                .count();
            if blp_hot < self.config.targets.target_hot_big_ledger {
                let needed = self.config.targets.target_hot_big_ledger - blp_hot;
                let mut promoted = 0usize;
                let warm_blps: Vec<SocketAddr> = peer_manager
                    .peers_in_state(PeerState::Warm)
                    .into_iter()
                    .filter(|a| big_ledger_peers.contains(a))
                    .collect();
                for addr in warm_blps {
                    if promoted >= needed {
                        break;
                    }
                    if already_promoted.contains(&addr) {
                        continue;
                    }
                    if self.in_progress_promote_warm.contains(&addr) {
                        continue;
                    }
                    // Skip peers in post-demote cooldown — see #671.
                    if self.in_cooldown(&addr, now) {
                        continue;
                    }
                    // Skip immature inbound peers — must complete the
                    // 15-min `inboundMaturePeerDelay` before promotion.  #703 fix B.
                    if fresh_inbound.contains(&addr) {
                        continue;
                    }
                    actions.push(GovernorAction::PromoteToHot(addr));
                    self.in_progress_promote_warm.insert(addr);
                    already_promoted.insert(addr);
                    promoted += 1;
                }
            }
        }

        // ── Target-driven promotions/demotions ──────────────────────────

        // Promote cold → warm if below target (belowTargetOther cold→warm).
        // Only select peers whose exponential backoff window has elapsed
        // (matches Haskell `availableToConnect` filtered by `nextConnectTimes`).
        // Skip peers already promoted by per-group local root logic above.
        // Skip local root members — they are managed exclusively by
        // belowTargetLocal above.
        if warm_count + hot_count < self.config.targets.target_warm {
            let needed = self.config.targets.target_warm - (warm_count + hot_count);
            let cold_peers = peer_manager.peers_eligible_to_connect();
            let mut promoted = 0;
            for &addr in &cold_peers {
                if promoted >= needed {
                    break;
                }
                if already_promoted.contains(&addr) {
                    continue;
                }
                // Skip peers with an in-flight cold→warm promotion.
                if self.in_progress_promote_cold.contains(&addr) {
                    continue;
                }
                // Exclude local root members from aggregate promotion.
                if local_root_members.contains(&addr) {
                    continue;
                }
                // Skip peers in post-demote cooldown — see #671. This is
                // the primary anti-flap site: after a demote, the #516
                // single-use-channel workaround forces the peer to Cold,
                // and without this guard the very next governor tick
                // would re-spawn a TCP reconnect and resume the cycle.
                if self.in_cooldown(&addr, now) {
                    continue;
                }
                actions.push(GovernorAction::PromoteToWarm(addr));
                self.in_progress_promote_cold.insert(addr);
                already_promoted.insert(addr);
                promoted += 1;
            }
        }

        // Promote warm → hot if below target (belowTargetOther warm→hot).
        // Skip peers already promoted by per-group local root logic above.
        // Skip local root members — handled exclusively by belowTargetLocal.
        if hot_count < self.config.targets.target_hot {
            let needed = self.config.targets.target_hot - hot_count;
            let warm_peers = peer_manager.peers_in_state(PeerState::Warm);
            let mut promoted = 0;
            for &addr in &warm_peers {
                if promoted >= needed {
                    break;
                }
                if already_promoted.contains(&addr) {
                    continue;
                }
                // Skip peers with an in-flight warm→hot promotion.
                if self.in_progress_promote_warm.contains(&addr) {
                    continue;
                }
                // Exclude local root members from aggregate promotion.
                if local_root_members.contains(&addr) {
                    continue;
                }
                // Skip peers in post-demote cooldown — see #671.
                if self.in_cooldown(&addr, now) {
                    continue;
                }
                // Skip immature inbound peers — must complete the
                // 15-min `inboundMaturePeerDelay` before promotion.  #703 fix B.
                if fresh_inbound.contains(&addr) {
                    continue;
                }
                actions.push(GovernorAction::PromoteToHot(addr));
                self.in_progress_promote_warm.insert(addr);
                already_promoted.insert(addr);
                promoted += 1;
            }
        }

        // Demote hot → warm if above target.
        // Local root members are excluded — they must never be demoted by
        // aggregate targets, matching Haskell's `Set.\\ LocalRootPeers.keysSet`.
        // Remaining candidates are sorted by score so the worst are demoted first.
        //
        // Uses the EFFECTIVE hot count, which includes any warm→hot
        // promotions queued earlier in the same tick (BLP path, etc.).
        // This mirrors Haskell's `numActivePeers + numPromoteInProgressPeers`
        // accounting in `Governor.ActivePeers.hs` — without it, aggregate
        // demotions are delayed by one tick after a BLP swap, leaving the
        // cluster temporarily above target. The combined effect is the
        // atomic-swap behaviour the Haskell reference exhibits.
        let in_progress_warm_to_hot = self.in_progress_promote_warm.len();
        let effective_hot_count = hot_count + in_progress_warm_to_hot;
        if effective_hot_count > self.config.targets.target_hot {
            use super::selection::peer_score;

            let excess = effective_hot_count - self.config.targets.target_hot;
            // Candidates: already-Hot peers (not local root, not BLP — BLP
            // demotion is handled by aboveTargetBigLedgerPeers below).
            // Excluding BLPs from the non-BLP demote path mirrors Haskell's
            // `aboveTargetOther` which uses `activeNonBig`.
            //
            // Fetch-floor fix: also exclude the peer that is currently the
            // active BlockFetch downloader.  Without this exclusion the governor
            // fires every ~2 s, kills the in-progress fetch, and collapses
            // sustained throughput to ~5-10 blk/s (1 range every 5 s instead
            // of sustained back-to-back batches).  The exclusion only protects
            // the peer for the duration of a single download; as soon as it
            // releases the slot (after each batch) another peer may claim it and
            // receive the same protection on the next tick.
            let mut scored: Vec<(SocketAddr, f64)> = peer_manager
                .peers_in_state(PeerState::Hot)
                .into_iter()
                .filter_map(|addr| {
                    if local_root_members.contains(&addr) {
                        return None;
                    }
                    if big_ledger_peers.contains(&addr) {
                        return None;
                    }
                    // Never demote the peer currently holding the BlockFetch slot —
                    // killing a mid-download fetch resets throughput to near zero
                    // until another peer claims the slot after its 10ms poll tick.
                    if active_fetch_peer == Some(addr) {
                        return None;
                    }
                    peer_manager
                        .get_peer(&addr)
                        .map(|info| (addr, peer_score(info)))
                })
                .collect();
            scored.sort_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for (addr, _) in scored.into_iter().take(excess) {
                actions.push(GovernorAction::DemoteToWarm(addr));
                // Record cooldown so the demoted peer is not immediately
                // re-promoted on the next governor tick (#671).
                self.record_demote(addr, now);
            }
        }

        // ── aboveTargetLocal hot→warm for local root groups ────────────
        // When a local root group has MORE hot members than its hotValency,
        // the excess must be demoted. This is the ONLY path that can demote
        // topology (local root) peers — all other demotion paths exclude them.
        // Matches Haskell's `aboveTargetLocal` in `ActivePeers.hs`.
        //
        // The worst-scoring peer in the group is demoted first (ascending sort
        // so index 0 is the lowest score / highest latency).
        {
            use super::selection::peer_score;
            for group in local_root_groups {
                let mut hot_members: Vec<(SocketAddr, f64)> = group
                    .members
                    .iter()
                    .filter_map(|addr| {
                        peer_manager.get_peer(addr).and_then(|info| {
                            if info.state == PeerState::Hot {
                                Some((*addr, peer_score(info)))
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                if hot_members.len() > group.hot_valency {
                    let excess = hot_members.len() - group.hot_valency;
                    // Sort ascending — lowest score (worst peer) first.
                    hot_members.sort_by(|(_, a), (_, b)| {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    for (addr, _) in hot_members.into_iter().take(excess) {
                        actions.push(GovernorAction::DemoteToWarm(addr));
                        // NOTE: local root demotions intentionally do NOT
                        // record a cooldown — the per-group belowTargetLocal
                        // path must reconnect them immediately on the next
                        // tick to honour the topology's hot_valency target.
                        // Stale entries are cleared by the GC at the top of
                        // this function.
                    }
                }
            }
        }

        // ── Hot churn ───────────────────────────────────────────────────
        // Periodically rotate one hot peer to keep the active set fresh.
        // Uses scoring to demote the worst hot peer and promote the best warm.
        // The cadence is faster while bulk-syncing (catch-up) than when caught
        // up, mirroring Haskell's BulkSync vs Deadline churn intervals.
        if self.last_hot_churn.elapsed() >= self.effective_hot_churn_interval() && hot_count > 1 {
            if let Some(churn_out) = select_worst_hot(peer_manager) {
                actions.push(GovernorAction::DemoteToWarm(churn_out));
                self.record_demote(churn_out, now);
            }
            if let Some(churn_in) = select_best_warm(peer_manager) {
                // Don't promote a peer that's in cooldown — picking another
                // candidate keeps the churn cycle productive.
                // Also skip immature inbound peers (#703 fix B).
                if !self.in_cooldown(&churn_in, now) && !fresh_inbound.contains(&churn_in) {
                    actions.push(GovernorAction::PromoteToHot(churn_in));
                }
            }
            self.last_hot_churn = Instant::now();
        }

        // ── Cold churn ──────────────────────────────────────────────────
        // Forget lowest-reputation cold peers when the pool exceeds 150% of
        // max_cold. Topology peers are never forgotten (root peers from config).
        if self.last_cold_churn.elapsed() >= self.config.cold_churn_interval {
            let threshold = self.config.targets.max_cold * 3 / 2;
            if cold_count > threshold {
                let excess = cold_count - self.config.targets.max_cold;
                let to_forget = select_lowest_reputation_cold(peer_manager, excess);
                for addr in to_forget {
                    actions.push(GovernorAction::ForgetPeer(addr));
                }
            }
            self.last_cold_churn = Instant::now();
        }

        // ── Warm churn ──────────────────────────────────────────────────
        // Rotate warm peers based on quality: demote worst if above target,
        // promote best cold if below target.
        //
        // Note: warm-churn emits `DemoteToCold` (full disconnect), not
        // `DemoteToWarm`. This is the warm-pool turnover path, not the
        // hot-flap path #671 addresses, so we do NOT record a cooldown.
        if self.last_warm_churn.elapsed() >= self.config.warm_churn_interval {
            if warm_count > self.config.targets.target_warm {
                if let Some(worst) = select_worst_warm(peer_manager) {
                    actions.push(GovernorAction::DemoteToCold(worst));
                }
            }
            if warm_count < self.config.targets.target_warm {
                if let Some(best) = select_best_cold_eligible(peer_manager) {
                    if !self.in_cooldown(&best, now) {
                        actions.push(GovernorAction::PromoteToWarm(best));
                    }
                }
            }
            self.last_warm_churn = Instant::now();
        }

        // ── Peer discovery ──────────────────────────────────────────────
        // Request more peers via DNS/ledger and PeerSharing when cold pool is low.
        if cold_count < self.config.targets.max_cold / 2 {
            actions.push(GovernorAction::DiscoverMore);

            // PeerSharing active outreach: ask a random sharing-capable warm peer
            // for addresses. The orchestrator sends MsgShareRequest and adds
            // routable responses to the cold set.
            let sharing_peers = peer_manager.peers_with_peer_sharing(PeerState::Warm);
            if !sharing_peers.is_empty() {
                if let Some(&peer) = sharing_peers.choose(&mut rand::rng()) {
                    actions.push(GovernorAction::PeerShareRequest(peer));
                }
            }
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::manager::PeerSource;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), port)
    }

    #[test]
    fn promotes_cold_to_warm_when_below_target() {
        let mut pm = PeerManager::new();
        for i in 0..5u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 1,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions(&pm, &[]);
        let promote_warm_count = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToWarm(_)))
            .count();
        assert_eq!(promote_warm_count, 3);
    }

    /// #703 fix B regression lock: warm peers in the fresh_inbound filter
    /// are NOT selected for Warm→Hot promotion even when hot_count is
    /// below target_hot.  Without this, every inbound-promoted-to-warm
    /// peer would immediately be re-promoted to hot, driving the at-tip
    /// OOM cycle.
    #[test]
    fn fresh_inbound_excluded_from_warm_to_hot() {
        let mut pm = PeerManager::new();
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
        }
        // Mark all three peers as fresh inbound — none should be eligible.
        let mut fresh: HashSet<SocketAddr> = HashSet::new();
        for i in 0..3u16 {
            fresh.insert(test_addr(3000 + i));
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 3,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(&pm, &[], &HashSet::new(), &fresh, None);
        let promote_hot_count = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToHot(_)))
            .count();
        assert_eq!(
            promote_hot_count, 0,
            "fresh-inbound peers must not be promoted to hot"
        );
    }

    /// Once a peer matures out of the fresh-inbound set, the Warm→Hot
    /// promotion proceeds normally.
    #[test]
    fn matured_inbound_eligible_for_warm_to_hot() {
        let mut pm = PeerManager::new();
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
        }
        // Empty fresh set = all peers matured.
        let fresh: HashSet<SocketAddr> = HashSet::new();

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 3,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(&pm, &[], &HashSet::new(), &fresh, None);
        let promote_hot_count = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToHot(_)))
            .count();
        assert_eq!(promote_hot_count, 3);
    }

    /// Bulk-sync mode must select the faster bulk churn cadence; deadline mode
    /// the slower one. SIGHUP `update_churn_intervals` must update both.
    /// Mirrors Haskell's `defaultBulkChurnInterval` / `defaultDeadlineChurnInterval`.
    #[test]
    fn bulk_sync_mode_selects_bulk_churn_interval() {
        let config = GovernorConfig {
            hot_churn_interval: Duration::from_secs(3300),
            bulk_sync_churn_interval: Duration::from_secs(900),
            ..Default::default()
        };
        let mut gov = Governor::new(config);
        // Default = caught-up → deadline cadence.
        assert_eq!(
            gov.effective_hot_churn_interval(),
            Duration::from_secs(3300)
        );
        // Bulk-syncing → faster cadence.
        gov.set_bulk_sync_mode(true);
        assert_eq!(gov.effective_hot_churn_interval(), Duration::from_secs(900));
        // Back to caught-up.
        gov.set_bulk_sync_mode(false);
        assert_eq!(
            gov.effective_hot_churn_interval(),
            Duration::from_secs(3300)
        );
        // SIGHUP reload updates both cadences.
        gov.update_churn_intervals(Duration::from_secs(100), Duration::from_secs(50));
        assert_eq!(gov.effective_hot_churn_interval(), Duration::from_secs(100));
        gov.set_bulk_sync_mode(true);
        assert_eq!(gov.effective_hot_churn_interval(), Duration::from_secs(50));
    }

    /// #703 fix C: production defaults must match Haskell
    /// `defaultDeadlineTargets`.  This pins target_hot=20, target_warm=30,
    /// hot_churn_interval=3300s.
    #[test]
    fn peer_targets_haskell_defaults_pinned() {
        let t = PeerTargets::default();
        assert_eq!(t.target_hot, 20, "Haskell targetNumberOfActivePeers");
        assert_eq!(t.target_warm, 30, "Haskell targetNumberOfEstablishedPeers");
        assert_eq!(
            t.target_hot_big_ledger, 5,
            "Haskell targetNumberOfActiveBigLedgerPeers"
        );
        assert_eq!(t.max_cold, 150);

        let cfg = GovernorConfig::default();
        assert_eq!(
            cfg.hot_churn_interval,
            Duration::from_secs(3300),
            "Haskell defaultDeadlineChurnInterval (lower bound)"
        );
    }

    #[test]
    fn promotes_warm_to_hot_when_below_target() {
        let mut pm = PeerManager::new();
        for i in 0..5u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 5,
                target_hot: 2,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions(&pm, &[]);
        let promote_hot_count = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToHot(_)))
            .count();
        assert_eq!(promote_hot_count, 2);
    }

    #[test]
    fn discover_when_cold_pool_low() {
        let pm = PeerManager::new(); // empty = 0 cold peers

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 1,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions(&pm, &[]);
        assert!(actions.contains(&GovernorAction::DiscoverMore));
    }

    #[test]
    fn cold_churn_forgets_lowest_reputation() {
        let mut pm = PeerManager::new();
        // Add 160 cold peers (> 150% of max_cold=50 → threshold=75).
        // Use Dns/Ledger sources so they're eligible for eviction.
        for i in 0..160u16 {
            let source = if i % 2 == 0 {
                PeerSource::Dns
            } else {
                PeerSource::Ledger
            };
            pm.add_peer(test_addr(3000 + i), source);
            // Set reputation proportional to port: lower port = lower reputation.
            pm.get_peer_mut(&test_addr(3000 + i)).unwrap().reputation = i as f64 / 160.0;
        }
        // Add one topology peer with the lowest reputation — must not be forgotten.
        pm.add_peer(test_addr(2999), PeerSource::Topology);
        pm.get_peer_mut(&test_addr(2999)).unwrap().reputation = 0.0;

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 5,
                max_cold: 50,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            // Trigger cold churn immediately.
            cold_churn_interval: Duration::ZERO,
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        let forget_actions: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::ForgetPeer(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // Should forget excess peers: 160 cold (non-topology) - 50 max_cold = 110.
        // (topology peer at 2999 doesn't count toward cold_count in peers_in_state).
        // Actually the topology peer IS cold, so cold_count=161, excess=161-50=111.
        assert!(!forget_actions.is_empty());
        // Topology peer must never be forgotten.
        assert!(!forget_actions.contains(&test_addr(2999)));
        // All forgotten peers should be low-reputation.
        for addr in &forget_actions {
            let peer = pm.get_peer(addr).unwrap();
            assert_ne!(peer.source, PeerSource::Topology);
        }
    }

    #[test]
    fn warm_churn_demotes_worst_when_above_target() {
        let mut pm = PeerManager::new();
        // Create 15 warm peers with varying latency (target_warm=10).
        for i in 0..15u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
            pm.get_peer_mut(&test_addr(3000 + i))
                .unwrap()
                .update_latency((i as f64) * 100.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 5,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            // Trigger warm churn immediately.
            warm_churn_interval: Duration::ZERO,
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        let demote_cold: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToCold(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert_eq!(demote_cold.len(), 1, "should demote exactly one warm peer");
    }

    #[test]
    fn warm_churn_promotes_best_cold_when_below_target() {
        let mut pm = PeerManager::new();
        // 3 warm peers (below target_warm=10).
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
        }
        // 20 cold peers available for promotion.
        for i in 10..30u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 5,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::ZERO,
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        // Warm churn should emit at least one PromoteToWarm (from churn logic).
        // Note: the target-driven logic also emits promotions since warm+hot < target.
        let promote_warm_count = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToWarm(_)))
            .count();
        // Target-driven: needs 10 - (3+0) = 7. Warm churn also adds 1.
        assert!(promote_warm_count >= 7);
    }

    #[test]
    fn peer_share_request_emitted_when_cold_low() {
        let mut pm = PeerManager::new();
        // One warm peer with peer_sharing=true, no cold peers.
        let warm_addr = test_addr(3001);
        pm.add_peer(warm_addr, PeerSource::Dns);
        pm.promote_to_warm(&warm_addr);
        pm.get_peer_mut(&warm_addr).unwrap().peer_sharing = true;

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 1,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, GovernorAction::PeerShareRequest(_))),
            "should emit PeerShareRequest when cold pool is low and sharing peers exist"
        );
    }

    #[test]
    fn peer_share_request_not_emitted_when_no_sharing_peers() {
        let mut pm = PeerManager::new();
        // Warm peer without peer_sharing.
        let warm_addr = test_addr(3001);
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
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, GovernorAction::PeerShareRequest(_))),
            "should not emit PeerShareRequest when no sharing-capable peers"
        );
    }

    #[test]
    fn hot_churn_uses_scoring() {
        let mut pm = PeerManager::new();
        // Create 3 hot peers with different quality.
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
            pm.promote_to_hot(&test_addr(3000 + i));
        }
        // Make 3002 the worst (highest latency, lowest reputation).
        pm.get_peer_mut(&test_addr(3002))
            .unwrap()
            .update_latency(999.0);
        pm.get_peer_mut(&test_addr(3002)).unwrap().reputation = 0.1;
        // Make 3000 the best.
        pm.get_peer_mut(&test_addr(3000))
            .unwrap()
            .update_latency(5.0);
        pm.get_peer_mut(&test_addr(3000)).unwrap().reputation = 0.9;

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 5,
                max_cold: 100,
                ..Default::default()
            },
            // Trigger hot churn immediately.
            hot_churn_interval: Duration::ZERO,
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        // The worst hot peer (3002) should be demoted.
        let demoted: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert!(
            demoted.contains(&test_addr(3002)),
            "worst hot peer should be demoted during churn"
        );
    }

    #[test]
    fn local_root_peers_never_demoted_from_hot() {
        let mut pm = PeerManager::new();
        // 3 hot topology peers (local root members) + 2 hot ledger peers.
        // Target hot = 2.
        let local_root_addrs: Vec<_> = (0..3u16).map(|i| test_addr(4000 + i)).collect();
        for &addr in &local_root_addrs {
            pm.add_peer(addr, PeerSource::Topology);
            pm.promote_to_warm(&addr);
            pm.promote_to_hot(&addr);
        }
        for i in 0..2u16 {
            pm.add_peer(test_addr(5000 + i), PeerSource::Ledger);
            pm.promote_to_warm(&test_addr(5000 + i));
            pm.promote_to_hot(&test_addr(5000 + i));
        }

        // Register the topology peers as a local root group so they're protected.
        let local_root_group = LocalRootGroupTarget {
            members: local_root_addrs.iter().copied().collect(),
            warm_valency: 3,
            hot_valency: 3,
        };

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 2,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[local_root_group]);

        let demoted: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        // Local root members must never be demoted by aggregate targets.
        for addr in &demoted {
            assert!(
                !local_root_addrs.contains(addr),
                "local root peer should never be demoted by aggregate targets"
            );
        }
        // The 2 ledger peers should be demoted (5 hot, target 2, 3 protected).
        assert_eq!(demoted.len(), 2, "both ledger peers should be demoted");
    }

    #[test]
    fn non_local_root_topology_peers_can_be_demoted_from_hot() {
        let mut pm = PeerManager::new();
        // 2 hot topology peers (NOT local root members) + target hot = 1.
        // These are e.g. bootstrap or public root peers.
        for i in 0..2u16 {
            pm.add_peer(test_addr(4000 + i), PeerSource::Topology);
            pm.promote_to_warm(&test_addr(4000 + i));
            pm.promote_to_hot(&test_addr(4000 + i));
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 1,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        // No local root groups — these topology peers are bootstrap/public root.
        let actions = gov.compute_actions(&pm, &[]);

        let demoted: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        // Non-local-root topology peers CAN be demoted by aggregate targets.
        assert_eq!(demoted.len(), 1, "one excess hot peer should be demoted");
    }

    #[test]
    fn topology_peers_never_demoted_from_warm() {
        let mut pm = PeerManager::new();
        for i in 0..5u16 {
            pm.add_peer(test_addr(4000 + i), PeerSource::Topology);
            pm.promote_to_warm(&test_addr(4000 + i));
        }
        for i in 0..10u16 {
            pm.add_peer(test_addr(5000 + i), PeerSource::Ledger);
            pm.promote_to_warm(&test_addr(5000 + i));
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 5,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::ZERO,
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[]);

        let demoted: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToCold(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        for addr in &demoted {
            let peer = pm.get_peer(addr).unwrap();
            assert_ne!(
                peer.source,
                PeerSource::Topology,
                "topology peer should never be demoted to cold"
            );
        }
    }

    #[test]
    fn below_target_local_promotes_deficient_group() {
        let mut pm = PeerManager::new();
        // Group A: 3 cold peers, warm_valency=2 → needs 2 promotions.
        let group_a_addrs: Vec<SocketAddr> = (0..3u16).map(|i| test_addr(4000 + i)).collect();
        for &addr in &group_a_addrs {
            pm.add_peer(addr, PeerSource::Topology);
        }
        let group_a = LocalRootGroupTarget {
            members: group_a_addrs.iter().copied().collect(),
            warm_valency: 2,
            hot_valency: 0,
        };

        // Group B: 2 peers, 1 already warm, warm_valency=1 → satisfied.
        let group_b_addrs: Vec<SocketAddr> = (0..2u16).map(|i| test_addr(5000 + i)).collect();
        for &addr in &group_b_addrs {
            pm.add_peer(addr, PeerSource::Topology);
        }
        pm.promote_to_warm(&group_b_addrs[0]);
        let group_b = LocalRootGroupTarget {
            members: group_b_addrs.iter().copied().collect(),
            warm_valency: 1,
            hot_valency: 0,
        };

        // Aggregate targets at 0 so they don't interfere.
        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 0,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[group_a, group_b]);

        let promote_warm: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // Exactly 2 promotions, all from group A.
        assert_eq!(
            promote_warm.len(),
            2,
            "should promote exactly 2 from group A"
        );
        let group_a_set: HashSet<SocketAddr> = group_a_addrs.iter().copied().collect();
        for addr in &promote_warm {
            assert!(
                group_a_set.contains(addr),
                "promoted peer {addr} should be a group A member"
            );
        }
    }

    #[test]
    fn below_target_local_promotes_warm_to_hot() {
        let mut pm = PeerManager::new();
        // Group: 2 peers, both warm, hot_valency=1 → needs 1 PromoteToHot.
        let group_addrs: Vec<SocketAddr> = (0..2u16).map(|i| test_addr(6000 + i)).collect();
        for &addr in &group_addrs {
            pm.add_peer(addr, PeerSource::Topology);
            pm.promote_to_warm(&addr);
        }
        let group = LocalRootGroupTarget {
            members: group_addrs.iter().copied().collect(),
            warm_valency: 2,
            hot_valency: 1,
        };

        // Aggregate targets at 0.
        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 0,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[group]);

        let promote_hot: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToHot(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        assert_eq!(promote_hot.len(), 1, "should promote exactly 1 warm to hot");
        let group_set: HashSet<SocketAddr> = group_addrs.iter().copied().collect();
        assert!(
            group_set.contains(&promote_hot[0]),
            "promoted peer should be a group member"
        );
    }

    /// Aggregate cold→warm (belowTargetOther) must NOT promote topology peers —
    /// they are managed exclusively by the per-group belowTargetLocal path.
    /// This mirrors Haskell's `belowTargetOther` which excludes
    /// `LocalRootPeers.keysSet` from its candidate set.
    #[test]
    fn aggregate_warm_promotion_excludes_local_root_peers() {
        let mut pm = PeerManager::new();

        // One topology cold peer that IS a local root member — must NOT be
        // promoted by aggregate logic (handled by belowTargetLocal instead).
        let local_root_addr = test_addr(7000);
        pm.add_peer(local_root_addr, PeerSource::Topology);

        // One topology cold peer that is NOT a local root (e.g. bootstrap) —
        // eligible for aggregate promotion.
        let bootstrap_addr = test_addr(7002);
        pm.add_peer(bootstrap_addr, PeerSource::Topology);

        // One ledger cold peer — eligible for aggregate promotion.
        let ledger_addr = test_addr(7001);
        pm.add_peer(ledger_addr, PeerSource::Ledger);

        let local_root_group = LocalRootGroupTarget {
            members: [local_root_addr].into_iter().collect(),
            warm_valency: 1,
            hot_valency: 0,
        };

        // target_warm=3, aggregate logic must skip local root member
        // but promote bootstrap and ledger peers.
        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[local_root_group]);

        let promoted: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // Local root member promoted by belowTargetLocal, not aggregate.
        assert!(
            promoted.contains(&local_root_addr),
            "local root peer should be promoted (by belowTargetLocal)"
        );
        // Bootstrap peer promoted by aggregate belowTargetOther.
        assert!(
            promoted.contains(&bootstrap_addr),
            "bootstrap topology peer should be promoted by aggregate path"
        );
        // Ledger peer promoted by aggregate belowTargetOther.
        assert!(
            promoted.contains(&ledger_addr),
            "ledger peer should be promoted by aggregate path"
        );
    }

    /// Peers with an in-flight cold→warm promotion must not be promoted again
    /// on the next governor tick before `promotion_cold_completed()` is called.
    /// This prevents duplicate connection attempts when async promotions are slow.
    #[test]
    fn in_progress_cold_promotion_not_duplicated() {
        let mut pm = PeerManager::new();

        // Two ledger cold peers, both eligible.
        let addr_a = test_addr(8000);
        let addr_b = test_addr(8001);
        pm.add_peer(addr_a, PeerSource::Ledger);
        pm.add_peer(addr_b, PeerSource::Ledger);

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 2,
                target_hot: 0,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        // First tick — both peers are promoted (in-progress sets are populated).
        let actions1 = gov.compute_actions(&pm, &[]);
        let promoted1: Vec<SocketAddr> = actions1
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert_eq!(promoted1.len(), 2, "first tick should promote both peers");

        // Second tick without completing any promotions — must emit nothing new.
        // The peers are still cold (PeerManager not updated) but in-progress sets
        // prevent re-emission.
        let actions2 = gov.compute_actions(&pm, &[]);
        let promoted2: Vec<SocketAddr> = actions2
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert!(
            promoted2.is_empty(),
            "second tick must not re-emit PromoteToWarm while promotions are in flight"
        );

        // After completing addr_a's promotion the governor may re-evaluate it.
        gov.promotion_cold_completed(&addr_a);
        let actions3 = gov.compute_actions(&pm, &[]);
        let promoted3: Vec<SocketAddr> = actions3
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        // addr_a is eligible again; addr_b is still in-flight.
        assert_eq!(promoted3.len(), 1);
        assert_eq!(promoted3[0], addr_a);
    }

    /// Same guard for warm→hot: peers with an in-flight promotion must not be
    /// re-promoted on subsequent ticks.
    #[test]
    fn in_progress_warm_promotion_not_duplicated() {
        let mut pm = PeerManager::new();

        // Two warm ledger peers.
        let addr_a = test_addr(8100);
        let addr_b = test_addr(8101);
        pm.add_peer(addr_a, PeerSource::Ledger);
        pm.promote_to_warm(&addr_a);
        pm.add_peer(addr_b, PeerSource::Ledger);
        pm.promote_to_warm(&addr_b);

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 2,
                target_hot: 2,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        // First tick — both warm peers get PromoteToHot.
        let actions1 = gov.compute_actions(&pm, &[]);
        let promoted1: Vec<SocketAddr> = actions1
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToHot(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert_eq!(promoted1.len(), 2, "first tick should promote both to hot");

        // Second tick without update — must not re-emit.
        let actions2 = gov.compute_actions(&pm, &[]);
        let promoted2: Vec<SocketAddr> = actions2
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToHot(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert!(
            promoted2.is_empty(),
            "second tick must not re-emit PromoteToHot while promotions are in flight"
        );

        // After completing addr_b, it becomes eligible again.
        gov.promotion_warm_completed(&addr_b);
        let actions3 = gov.compute_actions(&pm, &[]);
        let promoted3: Vec<SocketAddr> = actions3
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToHot(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert_eq!(promoted3.len(), 1);
        assert_eq!(promoted3[0], addr_b);
    }

    /// aboveTargetLocal: when a local root group has more hot members than
    /// its hotValency, the excess (worst-scoring) must be demoted to warm.
    /// This is the ONLY path that can demote topology peers.
    #[test]
    fn above_target_local_hot_demotes_excess() {
        let mut pm = PeerManager::new();

        // Two topology peers both promoted to Hot.
        let good_addr = test_addr(9000); // low latency → high score → keep
        let bad_addr = test_addr(9001); // high latency → low score → demote

        pm.add_peer(good_addr, PeerSource::Topology);
        pm.promote_to_warm(&good_addr);
        pm.promote_to_hot(&good_addr);
        pm.get_peer_mut(&good_addr).unwrap().update_latency(5.0);
        pm.get_peer_mut(&good_addr).unwrap().reputation = 0.9;

        pm.add_peer(bad_addr, PeerSource::Topology);
        pm.promote_to_warm(&bad_addr);
        pm.promote_to_hot(&bad_addr);
        pm.get_peer_mut(&bad_addr).unwrap().update_latency(999.0);
        pm.get_peer_mut(&bad_addr).unwrap().reputation = 0.1;

        // Group hot_valency=1 → 2 hot members, excess=1.
        let group = LocalRootGroupTarget {
            members: [good_addr, bad_addr].iter().copied().collect(),
            warm_valency: 2,
            hot_valency: 1,
        };

        // Aggregate targets high so they don't interfere.
        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 10,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions = gov.compute_actions(&pm, &[group]);

        let demoted: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // Exactly one demotion: the worst-scoring peer.
        assert_eq!(demoted.len(), 1, "should demote exactly 1 excess hot peer");
        assert_eq!(
            demoted[0], bad_addr,
            "the worse-scoring peer should be demoted"
        );
        // The good peer must not be demoted.
        assert!(
            !demoted.contains(&good_addr),
            "better-scoring topology peer must be retained"
        );
    }

    /// Big-ledger peer minimum is honoured even when the aggregate target
    /// has been satisfied by non-BLP peers.
    #[test]
    fn blp_warm_target_promotes_blp_cold_peers() {
        let mut pm = PeerManager::new();

        // 3 non-BLP cold peers (already enough to satisfy aggregate warm=3)
        // — but the BLP minimum demands 2 BLP warm too.
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Ledger);
            pm.promote_to_warm(&test_addr(3000 + i));
        }

        // 4 BLP cold peers, none warm yet.
        let blp_addrs: Vec<SocketAddr> = (0..4u16).map(|i| test_addr(4000 + i)).collect();
        for &addr in &blp_addrs {
            pm.add_peer(addr, PeerSource::Ledger);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 3,
                target_hot: 0,
                max_cold: 100,
                target_warm_big_ledger: 2,
                target_hot_big_ledger: 0,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let blp_set: HashSet<SocketAddr> = blp_addrs.iter().copied().collect();
        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );

        let promoted_warm: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // The BLP minimum forces 2 promotions even though aggregate warm
        // (already 3) is at target_warm. Promotions must target BLPs.
        assert_eq!(promoted_warm.len(), 2, "BLP minimum should promote 2 BLPs");
        for addr in &promoted_warm {
            assert!(
                blp_set.contains(addr),
                "BLP-target promotions must select BLP candidates only"
            );
        }
    }

    /// Big-ledger HOT target promotes warm BLPs to hot until the minimum is met.
    #[test]
    fn blp_hot_target_promotes_warm_blps() {
        let mut pm = PeerManager::new();

        // 3 warm BLP peers, none hot yet.
        let blp_addrs: Vec<SocketAddr> = (0..3u16).map(|i| test_addr(5000 + i)).collect();
        for &addr in &blp_addrs {
            pm.add_peer(addr, PeerSource::Ledger);
            pm.promote_to_warm(&addr);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 0, // aggregate hot disabled
                max_cold: 100,
                target_warm_big_ledger: 0,
                target_hot_big_ledger: 2,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let blp_set: HashSet<SocketAddr> = blp_addrs.iter().copied().collect();
        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );

        let promoted_hot: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::PromoteToHot(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        assert_eq!(promoted_hot.len(), 2, "BLP hot minimum should promote 2");
        for addr in &promoted_hot {
            assert!(
                blp_set.contains(addr),
                "BLP-target promotions must target BLPs"
            );
        }
    }

    /// Empty BLP set must be a no-op — the governor must behave exactly as
    /// the legacy `compute_actions` entry point.
    #[test]
    fn blp_empty_set_behaves_like_legacy_compute_actions() {
        let mut pm = PeerManager::new();
        for i in 0..3u16 {
            pm.add_peer(test_addr(6000 + i), PeerSource::Ledger);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 2,
                target_hot: 0,
                max_cold: 100,
                target_warm_big_ledger: 5, // would be active if BLPs known
                target_hot_big_ledger: 5,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);
        let actions =
            gov.compute_actions_with_blp(&pm, &[], &HashSet::new(), &HashSet::new(), None);

        let promoted_warm = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToWarm(_)))
            .count();

        // Should respect target_warm=2 only (no BLP-driven promotions).
        assert_eq!(
            promoted_warm, 2,
            "empty BLP set must not trigger BLP promotions"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Issue #671 — hot-churn cadence regression tests
    //
    // These tests reproduce the bug where the governor demoted hot peers
    // every ~4 seconds (per peer) when the cluster was at steady-state with
    // target_hot reached. Root cause: the BLP hot-promotion path
    // (lines 380–403) has no aggregate guard, so it could push hot_count
    // above target_hot, which then made Site A (lines 483–503) demote on
    // the same tick. The demoted peer was reconnected on the next tick,
    // re-promoted, then re-demoted, producing a 4-second flap.
    //
    // Acceptance: in steady state (counts at target), the governor must
    // emit ZERO DemoteToWarm actions over many consecutive ticks.
    // ─────────────────────────────────────────────────────────────────────

    /// Helper: mark a peer as hot with measured (good) latency so it scores
    /// above the default-0.5 unknown-latency baseline.
    fn make_hot(pm: &mut PeerManager, addr: SocketAddr, latency_ms: f64) {
        pm.add_peer(addr, PeerSource::Ledger);
        pm.promote_to_warm(&addr);
        pm.promote_to_hot(&addr);
        if let Some(p) = pm.get_peer_mut(&addr) {
            p.update_latency(latency_ms);
        }
    }

    /// Helper: mark a peer as warm with measured latency.
    fn make_warm(pm: &mut PeerManager, addr: SocketAddr, latency_ms: f64) {
        pm.add_peer(addr, PeerSource::Ledger);
        pm.promote_to_warm(&addr);
        if let Some(p) = pm.get_peer_mut(&addr) {
            p.update_latency(latency_ms);
        }
    }

    /// At steady state (hot_count == target_hot, blp_hot == target_hot_blp),
    /// the governor must NOT demote any peer on a single tick.
    #[test]
    fn issue_671_steady_state_emits_no_demotions() {
        let mut pm = PeerManager::new();
        let mut blp_set: HashSet<SocketAddr> = HashSet::new();

        // 5 BLP hot peers
        for i in 0..5u16 {
            let addr = test_addr(4000 + i);
            make_hot(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        // 10 non-BLP hot peers
        for i in 0..10u16 {
            make_hot(&mut pm, test_addr(5000 + i), 100.0);
        }
        // Plus a healthy warm pool of additional BLPs so the BLP-promotion
        // path has somewhere to "want to" go if it were unconstrained
        for i in 0..10u16 {
            let addr = test_addr(6000 + i);
            make_warm(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        // Plus extra warm non-BLPs to satisfy target_warm
        for i in 0..30u16 {
            make_warm(&mut pm, test_addr(7000 + i), 100.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 40,
                target_hot: 15,
                max_cold: 85,
                target_warm_big_ledger: 10,
                target_hot_big_ledger: 5,
            },
            // Long intervals so timer-gated churn doesn't fire mid-test.
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );

        let demotes: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::DemoteToWarm(_)))
            .collect();
        assert!(
            demotes.is_empty(),
            "steady-state tick must not demote any peer — got {} demote(s): {:?}",
            demotes.len(),
            demotes
        );
    }

    /// BLP-vs-aggregate-target convergence (per Haskell's
    /// `belowTargetBigLedgerPeers`): BLPs CAN be promoted above the
    /// aggregate `target_hot`, and `aboveTargetOther` then demotes
    /// non-BLPs to restore the aggregate. The combined effect on tick 1
    /// is a swap (promote 5 BLPs, demote 5 non-BLPs) — net hot still
    /// equals `target_hot`. On tick 2, the demoted non-BLPs are in
    /// cooldown (per #671 fix), so they do NOT immediately reconnect,
    /// stopping the flap.
    ///
    /// Reference: IntersectMBO/ouroboros-network commit `d842a238`,
    /// `Governor.ActivePeers.hs:143-165`.
    #[test]
    fn issue_671_blp_under_target_swaps_with_non_blp_then_stabilises() {
        let mut pm = PeerManager::new();
        let mut blp_set: HashSet<SocketAddr> = HashSet::new();

        // 15 non-BLP hot peers — at aggregate target, but BLPs at 0.
        for i in 0..15u16 {
            make_hot(&mut pm, test_addr(5000 + i), 100.0);
        }
        // 5 warm BLPs available for promotion.
        for i in 0..5u16 {
            let addr = test_addr(4000 + i);
            make_warm(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        // Extra warm pool so cold→warm refill paths have somewhere to go.
        for i in 0..30u16 {
            make_warm(&mut pm, test_addr(7000 + i), 100.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 40,
                target_hot: 15,
                max_cold: 85,
                target_warm_big_ledger: 5,
                target_hot_big_ledger: 5,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            // Cooldown is what stops the flap.
            demote_cooldown: Duration::from_secs(300),
        };
        let mut gov = Governor::new(config);

        // Tick 1: BLP promotes 5 (hot=20), aboveTargetOther demotes 5 non-BLPs.
        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );
        let blp_promotes = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToHot(addr) if blp_set.contains(addr)))
            .count();
        let non_blp_demotes = actions
            .iter()
            .filter(|a| matches!(a, GovernorAction::DemoteToWarm(addr) if !blp_set.contains(addr)))
            .count();
        assert_eq!(
            blp_promotes, 5,
            "tick 1: expected 5 BLP promotions, got {blp_promotes}"
        );
        assert_eq!(
            non_blp_demotes, 5,
            "tick 1: expected 5 non-BLP demotions, got {non_blp_demotes}"
        );

        // Apply the tick 1 actions to the peer manager so tick 2 sees
        // the updated state. The #516 workaround moves demoted peers to
        // Cold (not Warm), so we mirror that here. We must also clear
        // the governor's `in_progress_promote_warm` set via
        // `promotion_warm_completed`, mirroring the lifecycle's
        // bookkeeping after the protocol task starts.
        for action in &actions {
            match action {
                GovernorAction::PromoteToHot(addr) => {
                    pm.promote_to_hot(addr);
                    gov.promotion_warm_completed(addr);
                }
                GovernorAction::DemoteToWarm(addr) => {
                    pm.demote_to_cold(addr); // #516 workaround behaviour
                }
                _ => {}
            }
        }

        assert_eq!(
            gov.cooldown_size(),
            5,
            "5 non-BLPs should now be in post-demote cooldown"
        );

        // Tick 2: cooldown blocks the demoted peers from reconnecting.
        // Governor should NOT emit any cold→warm promotion for them, and
        // should NOT emit any demote (hot is back at target).
        let actions2 = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );
        let bad_reconnects = actions2
            .iter()
            .filter(|a| match a {
                GovernorAction::PromoteToWarm(addr) => {
                    !blp_set.contains(addr) && (5000..5015).contains(&addr.port())
                }
                _ => false,
            })
            .count();
        assert_eq!(
            bad_reconnects, 0,
            "tick 2: no recently-demoted non-BLP should be reconnected (flap loop)"
        );
        let demotes_tick2 = actions2
            .iter()
            .filter(|a| matches!(a, GovernorAction::DemoteToWarm(_)))
            .count();
        assert_eq!(demotes_tick2, 0, "tick 2: hot at target, must not demote");
    }

    /// Multi-tick steady state must remain stable: across 30 consecutive
    /// governor ticks at target, the total demote count must be 0.
    /// This is the canonical regression test for the 4-second flap loop.
    #[test]
    fn issue_671_30_consecutive_ticks_at_steady_state_emit_no_demotions() {
        let mut pm = PeerManager::new();
        let mut blp_set: HashSet<SocketAddr> = HashSet::new();
        for i in 0..5u16 {
            let addr = test_addr(4000 + i);
            make_hot(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        for i in 0..10u16 {
            make_hot(&mut pm, test_addr(5000 + i), 100.0);
        }
        for i in 0..10u16 {
            let addr = test_addr(6000 + i);
            make_warm(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        for i in 0..30u16 {
            make_warm(&mut pm, test_addr(7000 + i), 100.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 40,
                target_hot: 15,
                max_cold: 85,
                target_warm_big_ledger: 10,
                target_hot_big_ledger: 5,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let mut total_demotes = 0usize;
        for tick in 0..30 {
            let actions = gov.compute_actions_with_blp(
                &pm,
                &[],
                &blp_set,
                &::std::collections::HashSet::new(),
                None,
            );
            let demotes = actions
                .iter()
                .filter(|a| matches!(a, GovernorAction::DemoteToWarm(_)))
                .count();
            if demotes > 0 {
                panic!(
                    "tick {}: governor emitted {} DemoteToWarm at steady state — full action list: {:?}",
                    tick, demotes, actions
                );
            }
            total_demotes += demotes;
        }
        assert_eq!(
            total_demotes, 0,
            "expected 0 demotes across 30 steady-state ticks"
        );
    }

    /// After a hot→warm demote, the #516 single-use-channel workaround
    /// closes the TCP connection — the peer transitions Hot→Cold in the
    /// PeerManager. On the next governor tick, the `belowTargetOther`
    /// cold→warm promotion path MUST exclude the recently-demoted peer
    /// during the cooldown window. Otherwise we re-open the TCP, the
    /// peer transitions Cold→Warm→Hot, the aboveTarget condition fires
    /// again, and we demote again — the canonical 4-second flap.
    ///
    /// This test exercises the realistic lifecycle: dugite never
    /// manually re-promotes a cooldowned peer, the GOVERNOR is what
    /// decides, and the cooldown filter in the cold→warm path is the
    /// thing that breaks the cycle.
    #[test]
    fn issue_671_cooldown_blocks_cold_to_warm_reconnect() {
        let mut pm = PeerManager::new();
        let blp_set: HashSet<SocketAddr> = HashSet::new();

        // 16 non-BLP hot peers — above target_hot=15. Varied latency so
        // peer_score gives a deterministic victim.
        for i in 0..16u16 {
            make_hot(&mut pm, test_addr(5000 + i), 100.0 + (i as f64) * 50.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 40,
                target_hot: 15,
                max_cold: 85,
                target_warm_big_ledger: 0,
                target_hot_big_ledger: 0,
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(300),
        };
        let mut gov = Governor::new(config);

        // Tick 1: governor sees 16 hot, target 15 — demote one.
        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );
        let victims: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();
        assert_eq!(victims.len(), 1, "tick 1: expected exactly 1 demote");
        let victim = victims[0];

        // Apply the #516 workaround: lifecycle closes TCP, peer goes Cold.
        pm.demote_to_cold(&victim);

        // Tick 2: governor sees only 15 hot. belowTargetOther cold→warm
        // would normally reconnect the cold peer to satisfy target_warm.
        // The cooldown filter MUST prevent that.
        let actions2 = gov.compute_actions_with_blp(
            &pm,
            &[],
            &blp_set,
            &::std::collections::HashSet::new(),
            None,
        );
        let victim_reconnects: Vec<_> = actions2
            .iter()
            .filter(|a| matches!(a, GovernorAction::PromoteToWarm(addr) if *addr == victim))
            .collect();
        assert!(
            victim_reconnects.is_empty(),
            "tick 2: recently-demoted peer {} was scheduled for reconnect — flap loop. \
             Actions: {:?}",
            victim,
            actions2
        );

        // And the governor must not emit another demote.
        let demotes2: Vec<_> = actions2
            .iter()
            .filter(|a| matches!(a, GovernorAction::DemoteToWarm(_)))
            .collect();
        assert!(
            demotes2.is_empty(),
            "tick 2: hot at target, must not demote — got {:?}",
            demotes2
        );
    }

    /// 1000-tick fuzz: drive a realistic steady-state cluster through
    /// many governor evaluations. With the #671 fix in place (cooldown +
    /// in-progress accounting), the total demote count across 1000
    /// ticks must be 0. Without the fix the preprod soak showed 264
    /// demotes in 31 minutes (~10 ticks × 60 sec = 600 ticks), so this
    /// test is the canonical regression for the production bug.
    #[test]
    fn issue_671_long_run_demote_rate_is_bounded() {
        let mut pm = PeerManager::new();
        let mut blp_set: HashSet<SocketAddr> = HashSet::new();
        for i in 0..5u16 {
            let addr = test_addr(4000 + i);
            make_hot(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        for i in 0..10u16 {
            make_hot(&mut pm, test_addr(5000 + i), 100.0);
        }
        for i in 0..10u16 {
            let addr = test_addr(6000 + i);
            make_warm(&mut pm, addr, 100.0);
            blp_set.insert(addr);
        }
        for i in 0..30u16 {
            make_warm(&mut pm, test_addr(7000 + i), 100.0);
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 40,
                target_hot: 15,
                max_cold: 85,
                target_warm_big_ledger: 10,
                target_hot_big_ledger: 5,
            },
            // Long enough that timer-gated churn never fires in this test.
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(300),
        };
        let mut gov = Governor::new(config);

        let mut total_demotes = 0usize;
        for _ in 0..1000 {
            let actions = gov.compute_actions_with_blp(
                &pm,
                &[],
                &blp_set,
                &::std::collections::HashSet::new(),
                None,
            );
            // Mirror lifecycle: apply promotions, treat DemoteToWarm as
            // Cold (the #516 single-use-channel workaround).
            for action in &actions {
                match action {
                    GovernorAction::PromoteToWarm(addr) => {
                        pm.promote_to_warm(addr);
                        gov.promotion_cold_completed(addr);
                    }
                    GovernorAction::PromoteToHot(addr) => {
                        pm.promote_to_hot(addr);
                        gov.promotion_warm_completed(addr);
                    }
                    GovernorAction::DemoteToWarm(addr) => {
                        pm.demote_to_cold(addr);
                        total_demotes += 1;
                    }
                    GovernorAction::DemoteToCold(addr) => {
                        pm.demote_to_cold(addr);
                    }
                    _ => {}
                }
            }
        }

        assert_eq!(
            total_demotes, 0,
            "1000-tick steady-state run produced {total_demotes} demote(s) — flap regression"
        );
    }

    // ── Fetch-floor fix tests ─────────────────────────────────────────────────
    //
    // These tests lock in the behaviour that `aboveTargetOther` does NOT demote
    // the peer currently holding the BlockFetch slot, while still demoting other
    // excess hot peers.  A regression here would re-introduce the ~5-second
    // fetch interruption that caps mainnet sync at ~5-10 blk/s.

    /// aboveTargetOther must NOT demote the active fetcher when hot_count is
    /// above target_hot, but MUST still demote other excess hot peers.
    ///
    /// Scenario: 3 hot peers, target_hot=2.  One peer is the active fetcher.
    /// Expected: the non-fetching excess peer is demoted; the fetcher is spared.
    #[test]
    fn above_target_other_spares_active_fetcher() {
        let mut pm = PeerManager::new();
        let fetcher_addr = test_addr(3001);
        let excess_addr = test_addr(3002);
        let kept_addr = test_addr(3003);

        for addr in [fetcher_addr, excess_addr, kept_addr] {
            pm.add_peer(addr, PeerSource::Dns);
            pm.promote_to_warm(&addr);
            pm.promote_to_hot(&addr);
        }
        // Give all peers identical latency/reputation so the only distinguishing
        // factor is the active-fetcher exclusion.
        for addr in [fetcher_addr, excess_addr, kept_addr] {
            pm.get_peer_mut(&addr).unwrap().update_latency(100.0);
            pm.get_peer_mut(&addr).unwrap().reputation = 0.5;
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 2,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &HashSet::new(),
            &HashSet::new(),
            Some(fetcher_addr), // active fetcher
        );

        let demoted: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        assert_eq!(
            demoted.len(),
            1,
            "exactly one excess peer should be demoted"
        );
        assert!(
            !demoted.contains(&fetcher_addr),
            "active fetcher must NOT be demoted by aboveTargetOther"
        );
    }

    /// When `active_fetch_peer` is `None` (no fetch in progress), `aboveTargetOther`
    /// demotes excess peers normally — the fix must not suppress legitimate demotions.
    #[test]
    fn above_target_other_demotes_normally_when_no_active_fetcher() {
        let mut pm = PeerManager::new();
        for i in 0..3u16 {
            pm.add_peer(test_addr(3000 + i), PeerSource::Dns);
            pm.promote_to_warm(&test_addr(3000 + i));
            pm.promote_to_hot(&test_addr(3000 + i));
        }

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 1,
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &HashSet::new(),
            &HashSet::new(),
            None, // no active fetcher
        );

        let demoted: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // 3 hot, target 1 → 2 excess should be demoted.
        assert_eq!(
            demoted.len(),
            2,
            "without an active fetcher, all excess hot peers should be demoted"
        );
    }

    /// When the active fetcher is the ONLY hot peer above target, the governor
    /// emits NO demotion (can't demote the fetcher, no other excess).
    #[test]
    fn above_target_other_emits_no_demotion_when_only_fetcher_is_excess() {
        let mut pm = PeerManager::new();
        let fetcher_addr = test_addr(3001);
        pm.add_peer(fetcher_addr, PeerSource::Dns);
        pm.promote_to_warm(&fetcher_addr);
        pm.promote_to_hot(&fetcher_addr);
        // One additional hot peer at target.
        let ok_addr = test_addr(3002);
        pm.add_peer(ok_addr, PeerSource::Dns);
        pm.promote_to_warm(&ok_addr);
        pm.promote_to_hot(&ok_addr);

        let config = GovernorConfig {
            targets: PeerTargets {
                target_warm: 10,
                target_hot: 1, // 2 hot, target 1 → 1 excess, but it's the fetcher
                max_cold: 100,
                ..Default::default()
            },
            hot_churn_interval: Duration::from_secs(3600),
            bulk_sync_churn_interval: Duration::from_secs(3600),
            cold_churn_interval: Duration::from_secs(3600),
            warm_churn_interval: Duration::from_secs(3600),
            demote_cooldown: Duration::from_secs(3600),
        };
        let mut gov = Governor::new(config);

        let actions = gov.compute_actions_with_blp(
            &pm,
            &[],
            &HashSet::new(),
            &HashSet::new(),
            Some(fetcher_addr),
        );

        let demoted: Vec<SocketAddr> = actions
            .iter()
            .filter_map(|a| match a {
                GovernorAction::DemoteToWarm(addr) => Some(*addr),
                _ => None,
            })
            .collect();

        // The excess is the fetcher (protected) and the non-fetcher (ok_addr).
        // Since excess=1 and the first candidate is excluded (fetcher), the next
        // candidate ok_addr should be demoted instead.
        assert_eq!(
            demoted.len(),
            1,
            "the non-fetcher excess peer should be demoted"
        );
        assert!(
            !demoted.contains(&fetcher_addr),
            "fetcher must not be demoted"
        );
        assert!(demoted.contains(&ok_addr), "non-fetcher should be demoted");
    }
}
