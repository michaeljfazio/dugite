//! Genesis State Machine (GSM) for bootstrap from genesis.
//!
//! Manages the node's sync progression through three states:
//! - **PreSyncing**: Waiting for enough trusted big ledger peers (HAA)
//! - **Syncing**: Active block download with LoE/GDD protection
//! - **CaughtUp**: Normal Praos operation at chain tip
//!
//! The GSM runs as a background task, monitoring peer counts and tip age
//! to drive state transitions. It also enforces the Limit on Eagerness (LoE)
//! and runs the Genesis Density Disconnector (GDD).
//!
//! ## LoE semantics
//!
//! The Limit on Eagerness constrains chain **selection** — how far the selected
//! chain may extend beyond the intersection of the current candidate fragments
//! — NOT immutable finalisation. In `ouroboros-consensus`, `copyToImmutableDB`
//! runs unconditionally on k-depth alone and is never gated by the LoE/GSM
//! state; PreSyncing freezes the selected tip (so there is nothing new to
//! finalise), but the flush mechanism itself is always live. dugite finalises
//! k-deep in EVERY consensus mode (`flush_to_immutable_batch_retain`); the LoE
//! must NOT gate the volatile→immutable flush (an earlier build did, which froze
//! the immutable tip during Byron PreSyncing and grew the VolatileDB without
//! bound).
//!
//! `compute_loe_slot()` reports the selection-eagerness ceiling for diagnostics
//! and for future selection-side wiring (GDD trimming of candidate fragments):
//! - **PreSyncing**: `Some(0)` — no eager selection until the HAA holds.
//! - **Syncing**: `Some(min_intersection)` — the minimum intersection slot
//!   across all tracked peers.
//! - **CaughtUp**: `None` — unconstrained.
//!
//! NOTE: dugite's live chain-selection path does not yet apply this LoE to
//! candidate selection (the GDD selection-trimming is incomplete), so genesis
//! mode currently selects + finalises like Praos (longest chain, k-final).
//!
//! ## GDD (Genesis Density Disconnector)
//!
//! During Syncing state the GSM maintains a per-peer `DensityWindow` tracking
//! how many blocks each peer's chain contains within the genesis window
//! `(intersection_slot, intersection_slot + sgen]`.  On each GDD evaluation
//! the 4-guard Haskell `densityDisconnect` algorithm is applied pairwise:
//! a peer is disconnected when its density upper-bound is dominated by
//! another peer's density lower-bound, subject to idling, signal, and
//! meaningful-comparison guards.
//!
//! ## GSM Actor
//!
//! `run_gsm_actor` owns the `GenesisStateMachine` and communicates with
//! the rest of the node via channels:
//! - **`GsmEvent`** (mpsc): events from ChainSync, BlockFetch, networking
//! - **`GsmSnapshot`** (watch): current state broadcast to consumers
//! - **`GddAction`** (mpsc): disconnect commands sent to the peer manager
//!
//! ## Current Limitations
//!
//! - **Lightweight checkpointing**: The Ouroboros Genesis specification calls
//!   for lightweight checkpoints to speed up initial sync. Not yet implemented.
//!
//! - **Genesis-specific peer selection**: Full Genesis requires a dedicated
//!   peer selection policy that prioritises big ledger peers (BLPs). Currently,
//!   peer selection uses the standard P2P governor policy.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dugite_network::codec::Point;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

// ── Sync state ──────────────────────────────────────────────────────────────

/// Genesis sync state matching Ouroboros Genesis specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisSyncState {
    /// Waiting for enough trusted big ledger peers (HAA satisfied)
    PreSyncing,
    /// Active block download with LoE/GDD protection
    Syncing,
    /// Normal Praos operation — node is at or near chain tip
    CaughtUp,
}

impl std::fmt::Display for GenesisSyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenesisSyncState::PreSyncing => write!(f, "PreSyncing"),
            GenesisSyncState::Syncing => write!(f, "Syncing"),
            GenesisSyncState::CaughtUp => write!(f, "CaughtUp"),
        }
    }
}

// ── Event / snapshot / action types ─────────────────────────────────────────

/// Outcome of a resolved CSJ objection.
///
/// Mirrors Haskell's `CsjOutcome` used in `csjGsmHandler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // diagnostics-only CSJ-objection events; objectors are also caught by the all-idling CaughtUp check
pub enum CsjObjectionOutcome {
    /// GDD comparison resolved in the dynamo's favour — adopt the dynamo chain.
    DynamoWins,
    /// GDD comparison resolved in the objector's favour — retain the objector chain.
    ObjectorWins,
}

/// Event sent to the GSM actor from producers (ChainSync, BlockFetch, networking).
// CSJ diagnostics events (JumpAgreed / ObjectionRaised / ObjectionResolved).
// The GSM CaughtUp predicate already blocks on objectors via the all-peers-
// idling check (an objector is streaming, not idling), so these events are a
// diagnostics-grade gate; they have no live producer at present.
#[allow(dead_code)]
#[derive(Debug)]
pub enum GsmEvent {
    /// A new peer has been registered with known intersection and tip.
    PeerRegistered {
        addr: SocketAddr,
        intersection_slot: u64,
        tip_slot: u64,
    },
    /// A peer has disconnected.
    PeerDisconnected { addr: SocketAddr },
    /// A block was received from a peer at the given slot.
    BlockReceived { addr: SocketAddr, slot: u64 },
    /// A peer's tip slot was updated (e.g., new header announcement).
    PeerTipUpdated { addr: SocketAddr, tip_slot: u64 },
    /// A peer's ChainSync client has become idle (awaiting next header).
    PeerIdling { addr: SocketAddr },
    /// A peer's ChainSync client has become active again.
    PeerActive { addr: SocketAddr },
    /// Periodic status update from the sync pipeline.
    SyncStatus {
        /// Number of ACTIVE (hot) big-ledger peers — the HAA input
        /// (Haskell `activeNumBigLedgerPeers`).
        active_blp_count: usize,
        /// Block number of the current selection tip — the
        /// candidate-vs-selection comparison baseline (Haskell
        /// `getCurrentSelection` in `blockUntilCaughtUp`).
        selection_block_no: u64,
        /// LIVE age of the selection tip in seconds (now − slot wallclock):
        /// the `durationUntilTooOld` input. Computed by the emitter each
        /// tick — never a scrape-refreshed gauge (audit gsm-11).
        tip_age_secs: u64,
    },

    // ── CSJ Phase C events — constructed in csj_orchestrator (lib target) ───
    // The binary target's dead-code pass cannot see csj_orchestrator so it
    // reports these variants as unused; suppress that false positive.
    /// A jumper peer agreed to the proposed jump point and has found the
    /// intersection on its own chain (CSJ `IntersectFound`).
    ///
    /// (Diagnostics.) Indicates a jumper transitioned
    /// `LookingForIntersection → FoundIntersection`.  Informational only —
    /// the GSM records it for diagnostics and future LoP extension points.
    JumpAgreed {
        /// The peer that agreed.
        peer: SocketAddr,
        /// The agreed intersection point.
        point: Point,
    },

    /// A jumper peer could **not** find the jump point on its own chain and
    /// has entered objector/bisection mode (`MsgIntersectNotFound`).
    ///
    /// The GSM records the objection and **blocks the `Syncing → CaughtUp`
    /// transition** until it is resolved (Haskell LoP — Limit on Patience).
    ObjectionRaised {
        /// The objecting peer.
        peer: SocketAddr,
        /// Lower bound of the bisection range (inclusive).
        lo: Point,
        /// Upper bound of the bisection range (inclusive).
        hi: Point,
    },

    /// An earlier objection has been resolved by the GDD density comparison.
    ///
    /// (Diagnostics.) Indicates an objection was resolved by the GDD.
    /// Once all outstanding objections are resolved, the LoP gate is lifted
    /// and `CaughtUp` transitions become possible again.
    ObjectionResolved {
        /// The previously objecting peer.
        peer: SocketAddr,
        /// How the GDD decided.
        outcome: CsjObjectionOutcome,
    },
}

/// Broadcast snapshot of the current GSM state.
///
/// Published via a `watch` channel so consumers always see the latest value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsmSnapshot {
    /// Current sync state.
    pub state: GenesisSyncState,
    /// Limit on Eagerness slot, or `None` if unconstrained (CaughtUp).
    pub loe_slot: Option<u64>,
}

/// Actions the GSM actor emits for the peer manager to execute.
#[derive(Debug)]
pub enum GddAction {
    /// Disconnect this peer — GDD determined it is on a sparse chain.
    DisconnectPeer(SocketAddr),
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Configuration for the Genesis State Machine.
#[derive(Debug, Clone)]
pub struct GsmConfig {
    /// Minimum active big ledger peers to transition PreSyncing → Syncing.
    pub min_active_blp: usize,
    /// Maximum tip age (seconds) to consider the node "caught up".
    /// Used in the Syncing → CaughtUp transition guard.
    pub max_caught_up_age_secs: u64,
    /// Minimum time (seconds) to stay in CaughtUp before allowing regression.
    /// Prevents thundering-herd oscillations between CaughtUp and PreSyncing.
    pub min_caught_up_dwell_secs: u64,
    /// Maximum random jitter (seconds) added to the dwell time.
    /// Prevents multiple nodes from regressing simultaneously.
    pub anti_thundering_herd_max_secs: u64,
    /// Minimum interval (milliseconds) between GDD evaluations.
    /// Rate-limits the pairwise comparison to avoid CPU spikes with many peers.
    pub gdd_rate_limit_ms: u64,
    /// Security parameter `k` — the maximum rollback depth.
    /// Used by GDD guard 3 ("offers more than k").
    pub security_param_k: u64,
    /// Path for the caught_up marker file.
    pub marker_path: PathBuf,
    /// Stability window in seconds (`sgen × slot_length_secs`, i.e.
    /// `ceil(3k/f) × slot_length_secs`).
    ///
    /// When the node starts without a `caught_up.marker` **and** the tip age
    /// at startup is less than this threshold, it transitions directly to
    /// **Syncing** instead of staying in PreSyncing.
    ///
    /// Rationale: a Mithril-bootstrapped node near the live tip has already
    /// had its chain certified by the Mithril certificate chain; the HAA
    /// (honest-availability-assumption) check that normally gates
    /// PreSyncing → Syncing is therefore satisfied by construction, and
    /// requiring additional big-ledger-peer connections before extending the
    /// chain would cause a permanent k-block stall (issue #757).
    ///
    /// This is a Dugite extension — the Haskell node relies on a
    /// `peerSnapshotFile` to seed BLPs instantly at startup and so never
    /// encounters this scenario.  A value of `0` disables the optimisation
    /// (keeps strict Haskell semantics: absent marker → PreSyncing always).
    pub syncing_startup_threshold_secs: u64,
}

impl Default for GsmConfig {
    fn default() -> Self {
        GsmConfig {
            min_active_blp: 5,
            max_caught_up_age_secs: 1200,       // 20 minutes
            min_caught_up_dwell_secs: 1200,     // 20 minutes
            anti_thundering_herd_max_secs: 300, // up to 5 minutes jitter
            gdd_rate_limit_ms: 1000,            // 1 GDD tick per second
            security_param_k: 2160,             // mainnet default
            marker_path: PathBuf::from("caught_up.marker"),
            // Default: sgen for mainnet/preprod = ceil(3×2160/0.05) × 1s = 129 600 s ≈ 36 h.
            // This is a safe upper bound; real value is set from the genesis file.
            syncing_startup_threshold_secs: 129_600,
        }
    }
}

// ── GenesisStateMachine ─────────────────────────────────────────────────────

/// The Genesis State Machine.
///
/// Tracks sync state from the lossless per-peer registry
/// (`genesis_peer_state::PeerStateRegistry` — the Haskell per-peer
/// `ChainSyncState` TVar analogue). The GDD density evaluation and the LoE
/// fragment computation live in `genesis_governor` (pure) and are driven by
/// `run_gsm_actor` below.
pub struct GenesisStateMachine {
    config: GsmConfig,
    state: GenesisSyncState,
    /// Whether genesis mode is enabled (opt-in via --consensus-mode genesis).
    enabled: bool,
    /// Lossless per-peer chain state, written by the ChainSync tasks.
    registry: std::sync::Arc<crate::genesis_peer_state::PeerStateRegistry>,
    /// Timestamp of when the GSM entered CaughtUp state, or `None` if it
    /// has never been CaughtUp (or has since regressed).
    caught_up_since: Option<Instant>,
    /// Random jitter (seconds) added to `min_caught_up_dwell_secs` to
    /// prevent multiple nodes from regressing simultaneously.
    anti_thundering_herd_jitter_secs: u64,
    /// Set of peers with an unresolved CSJ objection (diagnostics-grade CSJ
    /// gate; NOT Haskell's Limit on Patience — the real LoP is the leaky
    /// bucket in the ChainSync client).
    pending_objections: HashSet<SocketAddr>,
    /// LoE tip slot from the last governor evaluation (Syncing only) —
    /// feeds the `GsmSnapshot.loe_slot` metric.
    last_loe_tip_slot: u64,
}

/// Operator opt-in (`DUGITE_GENESIS_BOOTSTRAP_SYNCING=1`): start a from-genesis /
/// far-behind genesis-mode node directly in `Syncing` instead of `PreSyncing`.
///
/// Default OFF. With it OFF (and no Mithril/recent-tip #757 fast-path applying), a
/// node with no `peerSnapshotFile` whose tip is below `useLedgerAfterSlot` can
/// never satisfy the Honest-Availability-Assumption gate — no big-ledger-peers are
/// ever classified — so it stalls permanently in `PreSyncing` with the LoE frozen
/// at the immutable tip (the genesis bulk-sync stall: ledger wedged ~k blocks past
/// origin). Setting this flag bypasses that gate so the live GDD/LoE-advance path
/// runs and the ledger can progress.
///
/// SECURITY TRADEOFF: this trusts the bootstrap/public peers' density-selected
/// chain WITHOUT the honest-availability assumption — unlike the #757 recent-tip
/// path, which is backed by a Mithril certificate or a prior caught-up run. It is
/// intended for trusted-bootstrap from-genesis sync (e.g. testnet soak); the
/// faithful production fix is to seed big-ledger-peers via a `peerSnapshotFile`.
/// Kept OFF by default so mainnet / default genesis behaviour is unchanged.
pub fn bootstrap_syncing_override() -> bool {
    matches!(
        std::env::var("DUGITE_GENESIS_BOOTSTRAP_SYNCING").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

impl GenesisStateMachine {
    /// Create a new GSM. If not enabled, it immediately enters CaughtUp
    /// and all constraints are disabled.
    /// `initial_tip_age_secs`: live age of the selection tip at startup —
    /// the `durationUntilTooOld` input for Haskell's
    /// `initializationGsmState` marker-staleness table (extended for #757):
    ///
    /// | marker  | tip age                                  | initial state                |
    /// |---------|------------------------------------------|------------------------------|
    /// | absent  | `None` or ≥ `syncing_startup_threshold`  | PreSyncing                   |
    /// | absent  | < `syncing_startup_threshold` (recent)   | Syncing  (Dugite ext. #757)  |
    /// | present | `None` (no age limit)                    | CaughtUp                     |
    /// | present | young enough                             | CaughtUp                     |
    /// | present | already too old                          | PreSyncing + marker DELETED  |
    ///
    /// The "absent marker + recent tip → Syncing" row is a Dugite extension
    /// that handles Mithril snapshot bootstrap near the live tip.  In Haskell
    /// an absent marker always yields PreSyncing; Haskell avoids the
    /// resulting k-block stall by requiring a `peerSnapshotFile` in the
    /// topology so that big-ledger-peers are seeded instantly.  Dugite adds
    /// this shortcut for deployments without a peer snapshot file.
    pub fn new(
        config: GsmConfig,
        enabled: bool,
        registry: std::sync::Arc<crate::genesis_peer_state::PeerStateRegistry>,
        initial_tip_age_secs: Option<u64>,
    ) -> Self {
        let initial_state = if enabled {
            if config.marker_path.exists() {
                let too_old = initial_tip_age_secs
                    .map(|age| age > config.max_caught_up_age_secs)
                    .unwrap_or(false);
                if too_old {
                    info!(
                        age_secs = initial_tip_age_secs,
                        "Genesis: caught_up marker is STALE (tip too old) — \
                         removing marker, starting in PreSyncing"
                    );
                    let _ = std::fs::remove_file(&config.marker_path);
                    GenesisSyncState::PreSyncing
                } else {
                    info!("Genesis: caught_up marker found, starting in CaughtUp state");
                    GenesisSyncState::CaughtUp
                }
            } else {
                // No marker file.
                // Haskell: always PreSyncing (requires peerSnapshotFile to
                // seed BLPs so the HAA gate fires quickly).
                //
                // Dugite extension (issue #757): when the selection tip is
                // recent enough (age < syncing_startup_threshold_secs, i.e.
                // within one stability window of the current time), the node
                // was either just bootstrapped from a Mithril snapshot near
                // the live tip, or restarted shortly after being caught up.
                // In both cases the Mithril certificate chain (or the prior
                // run's chain-state) has already proven chain validity, so
                // the HAA is satisfied by construction.  Skip straight to
                // Syncing so the LoE fragment is computed from live peer
                // candidate prefixes rather than the restrictive
                // immutable-tip anchor that caps selection at k blocks and
                // permanently stalls the node when no peer-snapshot-file is
                // configured and ledger-peer discovery hasn't fired yet.
                let recent = matches!(
                    initial_tip_age_secs,
                    Some(age) if config.syncing_startup_threshold_secs > 0
                        && age < config.syncing_startup_threshold_secs
                );
                if recent {
                    info!(
                        tip_age_secs = initial_tip_age_secs,
                        threshold_secs = config.syncing_startup_threshold_secs,
                        "Genesis: tip is recent (no marker) — starting in \
                         Syncing (Mithril snapshot / fast-restart path, issue #757)"
                    );
                    GenesisSyncState::Syncing
                } else if bootstrap_syncing_override() {
                    // From-genesis / far-behind bootstrap with no peerSnapshotFile:
                    // the HAA gate (>= min_active_blp BLPs) can never fire (no BLPs
                    // are classified before useLedgerAfterSlot, which the stalled
                    // ledger never reaches), so PreSyncing would freeze the LoE at
                    // the immutable tip forever. This explicit opt-in starts in
                    // Syncing so the live GDD/LoE-advance path runs. See
                    // `bootstrap_syncing_override` for the security tradeoff.
                    info!(
                        tip_age_secs = initial_tip_age_secs,
                        "Genesis: DUGITE_GENESIS_BOOTSTRAP_SYNCING set — starting in \
                         Syncing (bootstrap HAA bypass; trusts peer-density chain \
                         selection without the honest-availability assumption)"
                    );
                    GenesisSyncState::Syncing
                } else {
                    GenesisSyncState::PreSyncing
                }
            }
        } else {
            GenesisSyncState::CaughtUp
        };

        // Compute anti-thundering-herd jitter using PID + high-resolution
        // timestamp. This provides good per-node uniqueness without needing
        // an RNG dependency. Different from Haskell's randomRIO but achieves
        // the same goal: preventing a fleet of nodes from regressing simultaneously.
        let jitter = if config.anti_thundering_herd_max_secs > 0 {
            let pid = std::process::id() as u64;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as u64;
            let seed = pid.wrapping_mul(6364136223846793005).wrapping_add(nanos);
            seed % (config.anti_thundering_herd_max_secs + 1)
        } else {
            0
        };

        let caught_up_since = if initial_state == GenesisSyncState::CaughtUp {
            Some(Instant::now())
        } else {
            None
        };

        GenesisStateMachine {
            config,
            state: initial_state,
            enabled,
            registry,
            caught_up_since,
            anti_thundering_herd_jitter_secs: jitter,
            pending_objections: HashSet::new(),
            last_loe_tip_slot: 0,
        }
    }

    /// Current sync state.
    pub fn state(&self) -> GenesisSyncState {
        self.state
    }

    /// Whether genesis mode is enabled.
    #[allow(dead_code)] // public API for diagnostics
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record the LoE tip slot from the latest governor evaluation
    /// (metric for `GsmSnapshot.loe_slot`).
    pub fn set_loe_tip_slot(&mut self, slot: u64) {
        self.last_loe_tip_slot = slot;
    }

    // ── State transitions ────────────────────────────────────────────────────

    /// Evaluate state transitions based on current conditions.
    ///
    /// Returns `Some(new_state)` if a transition occurred, `None` if unchanged.
    ///
    /// # Arguments
    /// - `active_blp_count`: number of ACTIVE (hot) big ledger peers (HAA)
    /// - `selection_block_no`: block number of the current selection tip
    /// - `tip_age_secs`: LIVE age of the selection tip in seconds
    pub fn evaluate(
        &mut self,
        active_blp_count: usize,
        selection_block_no: u64,
        tip_age_secs: u64,
    ) -> Option<GenesisSyncState> {
        if !self.enabled {
            return None;
        }

        let old_state = self.state;

        match self.state {
            GenesisSyncState::PreSyncing => {
                // Transition to Syncing when HAA (Honest Availability Assumption) is satisfied:
                // we have enough active big ledger peers.
                if active_blp_count >= self.config.min_active_blp {
                    self.state = GenesisSyncState::Syncing;
                    info!(
                        active_blp = active_blp_count,
                        min = self.config.min_active_blp,
                        "Genesis: HAA satisfied, transitioning to Syncing"
                    );
                }
            }
            GenesisSyncState::Syncing => {
                // HAA LOSS: if we drop below the minimum BLP count, regress to
                // PreSyncing — but ONLY when the tip is also stale.
                //
                // Haskell alignment: in `UseBootstrapPeers` mode the HAA
                // (`outboundConnectionsState = TrustedStateWithExternalPeers`)
                // is satisfied by ≥1 active BOOTSTRAP PEER, not by big-ledger
                // peers.  Dugite does not yet track bootstrap peers in
                // `haa_satisfied`, so `active_blp_count` always reads 0 in a
                // topology with only bootstrap peers (preprod, preview with no
                // peerSnapshotFile) — `haa_satisfied` returns the synthetic
                // `min` only via the trusted-local-roots path, which is also
                // empty when `localRoots = []`.
                //
                // When the node started in Syncing because its tip was recent
                // (the Mithril-bootstrap path — `syncing_startup_threshold_secs`
                // > 0 and tip age < threshold) it already passed the
                // certified-chain check at startup.  GDD provides safety during
                // the bounded gap to the live tip.  HAA-loss regression here
                // would re-enter the k-block stall that issue #757 was opened to
                // fix.
                //
                // Rule (mirrors Haskell `enterSyncing'` semantics for the
                // UseBootstrapPeers case): regress to PreSyncing ONLY when BOTH
                // conditions hold simultaneously:
                //   (a) HAA is lost (BLP count < min), AND
                //   (b) the tip is STALE (age ≥ syncing_startup_threshold_secs)
                //
                // When the tip is still recent (a) alone does NOT trigger
                // regression.  Once the tip goes stale the node IS in the
                // from-genesis-vulnerable regime and must re-establish the HAA.
                // When `syncing_startup_threshold_secs = 0` the bypass is
                // disabled and the original strict behaviour is preserved.
                let haa_lost = active_blp_count < self.config.min_active_blp;
                let tip_stale = self.config.syncing_startup_threshold_secs == 0
                    || tip_age_secs >= self.config.syncing_startup_threshold_secs;

                if haa_lost && tip_stale {
                    self.state = GenesisSyncState::PreSyncing;
                    self.remove_marker();
                    warn!(
                        active_blp = active_blp_count,
                        min = self.config.min_active_blp,
                        tip_age_secs,
                        "Genesis: HAA lost (tip stale), regressing to PreSyncing"
                    );
                } else if haa_lost {
                    // HAA temporarily lost but tip is still recent — suppress
                    // regression; log at DEBUG so it is observable without noise.
                    debug!(
                        active_blp = active_blp_count,
                        min = self.config.min_active_blp,
                        tip_age_secs,
                        threshold = self.config.syncing_startup_threshold_secs,
                        "Genesis: HAA transiently lost but tip is recent — \
                         staying in Syncing (UseBootstrapPeers path, issue #757)"
                    );
                } else if self.caught_up_predicate(selection_block_no) {
                    // Haskell `blockUntilCaughtUp`: at least one peer, every
                    // peer idling (MsgAwaitReply), and no candidate better
                    // than the selection. (Plus the dugite CSJ-diagnostics
                    // gate, folded into the predicate.) On entry: write the
                    // marker; the dwell below enforces minCaughtUpDuration.
                    self.state = GenesisSyncState::CaughtUp;
                    self.caught_up_since = Some(Instant::now());
                    self.write_marker();
                    info!(
                        selection_block_no,
                        "Genesis: all peers idle and no better candidate — CaughtUp"
                    );
                }
            }
            GenesisSyncState::CaughtUp => {
                // Regress to PreSyncing if tip becomes stale, but only after
                // the minimum dwell time has elapsed. Jitter is added to the
                // tip-age threshold (not dwell) matching Haskell's antiThunderingHerd.
                let dwell_ok = self
                    .caught_up_since
                    .map(|t| t.elapsed().as_secs() >= self.config.min_caught_up_dwell_secs)
                    .unwrap_or(true);

                if dwell_ok {
                    let threshold =
                        self.config.max_caught_up_age_secs + self.anti_thundering_herd_jitter_secs;
                    if tip_age_secs > threshold {
                        self.state = GenesisSyncState::PreSyncing;
                        self.caught_up_since = None;
                        self.remove_marker();
                        warn!(
                            tip_age_secs,
                            threshold, "Genesis: tip stale, regressing to PreSyncing"
                        );
                    }
                }
            }
        }

        if self.state != old_state {
            Some(self.state)
        } else {
            None
        }
    }

    /// The `Syncing → CaughtUp` entry predicate.
    ///
    /// Haskell `blockUntilCaughtUp` (GSM.hs), checked atomically:
    ///
    /// ```haskell
    /// check $ not (Map.null states) && all peerIsIdle states
    /// ...
    /// let ok candidate =
    ///       WhetherCandidateIsBetter False
    ///         == candidateOverSelection selection candidate
    /// check $ all ok candidates
    /// ```
    ///
    /// The candidate-vs-selection comparison uses the candidate fragment
    /// HEAD's block number against the selection tip's (the dominant,
    /// longest-chain term of `preferAnchoredCandidate`). A candidate with
    /// EQUAL block number that would win only on the VRF tiebreaker is
    /// treated as not-better — the only effect is entering CaughtUp moments
    /// before adopting that block, after which selection behaves
    /// identically; no safety impact.
    fn caught_up_predicate(&self, selection_block_no: u64) -> bool {
        let peers = self.registry.all();
        if peers.is_empty() {
            // Haskell: `not (Map.null states)` — a node with no peers can
            // NEVER declare itself caught up.
            return false;
        }
        if !peers.iter().all(|(_, st)| st.is_idling()) {
            return false;
        }
        let any_better = peers.iter().any(|(_, st)| {
            let frag = st.fragment_snapshot();
            frag.entries
                .last()
                .map(|e| e.block_no > selection_block_no)
                .unwrap_or(false)
        });
        !any_better && self.csj_gate_satisfied()
    }

    // ── CSJ objection tracking (diagnostics gate) ───────────────────────────

    /// Record that a peer has raised a CSJ objection.
    ///
    /// An objection means the jumper could not find the jump point on its own
    /// chain and has entered bisection mode.  The GSM blocks the
    /// `Syncing → CaughtUp` transition until the objection is resolved.
    pub fn raise_objection(&mut self, peer: SocketAddr) {
        if !self.enabled {
            return;
        }
        let inserted = self.pending_objections.insert(peer);
        if inserted {
            debug!(
                %peer,
                pending = self.pending_objections.len(),
                "CSJ: objection raised — CaughtUp blocked"
            );
        }
    }

    /// Resolve a previously raised CSJ objection.
    ///
    /// Once resolved (via GDD density comparison), the peer is removed from
    /// `pending_objections`.  When the set becomes empty the gate lifts.
    pub fn resolve_objection(&mut self, peer: &SocketAddr, outcome: CsjObjectionOutcome) {
        if !self.enabled {
            return;
        }
        let removed = self.pending_objections.remove(peer);
        if removed {
            debug!(
                %peer,
                ?outcome,
                pending = self.pending_objections.len(),
                "CSJ: objection resolved"
            );
            if self.pending_objections.is_empty() {
                debug!("CSJ: all objections resolved — CaughtUp gate lifted");
            }
        }
    }

    /// Returns `true` when no CSJ objections are pending.
    ///
    /// NOTE: this is a dugite-specific CSJ diagnostics gate, NOT Haskell's
    /// Limit on Patience (the LoP is the per-peer leaky bucket in the
    /// ChainSync client). Haskell's GSM has no objection gate — the
    /// equivalent effect arises because CSJ objectors are not idling and so
    /// fail the all-idle check.
    pub fn csj_gate_satisfied(&self) -> bool {
        self.pending_objections.is_empty()
    }

    /// Read-only view of the pending objection set (for diagnostics / tests).
    #[allow(dead_code)] // public API for diagnostics and tests
    pub fn pending_objections(&self) -> &HashSet<SocketAddr> {
        &self.pending_objections
    }

    // ── LoE (metric view) ───────────────────────────────────────────────────

    /// LoE tip slot for the `GsmSnapshot` metric.
    ///
    /// The REAL LoE (an anchored fragment) is published by the actor to
    /// chain selection via `arc_swap` — see `run_gsm_actor`. This scalar is
    /// the fragment's tip slot, for observability only:
    /// - **PreSyncing**: `Some(0)` — selection frozen near the immutable tip.
    /// - **Syncing**: tip slot of the last computed shared candidate prefix.
    /// - **CaughtUp**: `None` — unconstrained.
    pub fn compute_loe_slot(&self) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        match self.state {
            GenesisSyncState::PreSyncing => Some(0),
            GenesisSyncState::Syncing => Some(self.last_loe_tip_slot),
            GenesisSyncState::CaughtUp => None,
        }
    }

    // ── Marker file helpers ──────────────────────────────────────────────────

    /// Write the caught_up marker file.
    fn write_marker(&self) {
        if let Err(e) = std::fs::write(&self.config.marker_path, "caught_up") {
            warn!(
                path = %self.config.marker_path.display(),
                "Failed to write caught_up marker: {e}"
            );
        }
    }

    /// Remove the caught_up marker file.
    fn remove_marker(&self) {
        if self.config.marker_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.config.marker_path) {
                warn!(
                    path = %self.config.marker_path.display(),
                    "Failed to remove caught_up marker: {e}"
                );
            }
        }
    }

    /// Override the jitter value (for deterministic testing).
    #[cfg(test)]
    fn set_jitter(&mut self, jitter: u64) {
        self.anti_thundering_herd_jitter_secs = jitter;
    }
}

// ── GSM Actor ───────────────────────────────────────────────────────────────

/// Run the GSM actor as a background task.
///
/// Owns the `GenesisStateMachine` and processes events from the sync
/// pipeline. On a rate-limited cadence (Haskell `defaultGDDRateLimit` 1 s,
/// re-armed only when per-peer state changed — the `gddWatcher` fingerprint)
/// it recomputes the LoE fragment (`sharedCandidatePrefix`) and the GDD
/// verdicts (`densityDisconnect`), publishes the LoE to chain selection via
/// `loe_out`, and emits `GddAction::DisconnectPeer` for density losers.
///
/// State → LoE mapping (Haskell `setGetLoEFragment`):
/// - disabled (praos) → `LoeState::Disabled`
/// - PreSyncing → empty fragment anchored at the immutable tip
/// - Syncing → live shared candidate prefix (zero peers → `SelectionTip`)
/// - CaughtUp → `LoeState::Disabled`
#[allow(clippy::too_many_arguments)]
pub async fn run_gsm_actor(
    config: GsmConfig,
    enabled: bool,
    registry: std::sync::Arc<crate::genesis_peer_state::PeerStateRegistry>,
    chain_db: std::sync::Arc<tokio::sync::RwLock<dugite_storage::ChainDB>>,
    era_history: std::sync::Arc<tokio::sync::RwLock<dugite_consensus::EraHistory>>,
    loe_out: std::sync::Arc<arc_swap::ArcSwap<dugite_consensus::loe::LoeState>>,
    initial_tip_age_secs: Option<u64>,
    mut event_rx: mpsc::Receiver<GsmEvent>,
    snapshot_tx: watch::Sender<GsmSnapshot>,
    action_tx: mpsc::Sender<GddAction>,
) {
    use dugite_consensus::loe::{LoePoint, LoeState};

    let gdd_interval_ms = config.gdd_rate_limit_ms;
    let k = config.security_param_k;
    let mut gsm = GenesisStateMachine::new(config, enabled, registry.clone(), initial_tip_age_secs);

    // Publish initial snapshot.
    let initial_snapshot = GsmSnapshot {
        state: gsm.state(),
        loe_slot: gsm.compute_loe_slot(),
    };
    let _ = snapshot_tx.send(initial_snapshot);

    let mut gdd_interval = tokio::time::interval(Duration::from_millis(gdd_interval_ms));
    // Don't compensate for missed ticks — just skip them.
    gdd_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Haskell gddWatcher fingerprint: re-evaluate only when per-peer state
    // (csLatestSlot, csIdling) changed. Our GsmEvents are emitted at exactly
    // those sites, so any received event marks the governor dirty. The first
    // tick always evaluates (wInitial = Nothing — fires once on startup).
    let mut dirty = true;
    let mut last_published_loe_tip: Option<Option<LoePoint>> = None;

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — all producers dropped. Shut down.
                    info!("GSM actor: event channel closed, shutting down");
                    break;
                };

                let mut state_changed = false;
                dirty = true;

                match event {
                    GsmEvent::PeerRegistered { .. }
                    | GsmEvent::PeerDisconnected { .. }
                    | GsmEvent::BlockReceived { .. }
                    | GsmEvent::PeerTipUpdated { .. }
                    | GsmEvent::PeerIdling { .. }
                    | GsmEvent::PeerActive { .. } => {
                        // Pure wakeup hints — the lossless truth lives in the
                        // per-peer registry written synchronously by the
                        // ChainSync tasks.
                    }
                    GsmEvent::SyncStatus {
                        active_blp_count,
                        selection_block_no,
                        tip_age_secs,
                    } => {
                        if gsm
                            .evaluate(active_blp_count, selection_block_no, tip_age_secs)
                            .is_some()
                        {
                            state_changed = true;
                        }
                    }

                    // ── CSJ events ────────────────────────────────────────

                    GsmEvent::JumpAgreed { peer, point } => {
                        // Informational — the peer found the jump point on
                        // its own chain.  No state change required; logged
                        // for diagnostics.
                        debug!(%peer, ?point, "GSM: jump agreed by peer");
                    }

                    GsmEvent::ObjectionRaised { peer, lo, hi } => {
                        debug!(%peer, ?lo, ?hi, "GSM: CSJ objection raised");
                        gsm.raise_objection(peer);
                        state_changed = true;
                    }

                    GsmEvent::ObjectionResolved { peer, outcome } => {
                        debug!(%peer, ?outcome, "GSM: CSJ objection resolved");
                        gsm.resolve_objection(&peer, outcome);
                        state_changed = true;
                    }
                }

                if state_changed {
                    let snapshot = GsmSnapshot {
                        state: gsm.state(),
                        loe_slot: gsm.compute_loe_slot(),
                    };
                    let _ = snapshot_tx.send(snapshot);
                }
            }

            _ = gdd_interval.tick() => {
                if !gsm.enabled {
                    continue;
                }
                // Fingerprint gate: skip the evaluation when nothing changed
                // since the last one (Haskell wFingerprint on
                // {peer → (csLatestSlot, csIdling)}).
                if !dirty {
                    continue;
                }
                dirty = false;

                // ── LoE per state (setGetLoEFragment) ────────────────────
                // The Genesis LoE is anchored at the immutable tip but INCLUDES
                // the node's own volatile selected chain (immutable tip →
                // selection tip) as its base: the peers intersect at/above the
                // selection tip, so that whole window is agreed. The peers'
                // candidate fragments are anchored at the intersection (the
                // selection tip) and provide the forward extension.
                #[allow(clippy::type_complexity)]
                let (imm_tip, selection_tip, volatile_window): (
                    Option<(u64, [u8; 32])>,
                    Option<(u64, [u8; 32])>,
                    Vec<(u64, [u8; 32])>,
                ) = {
                    let db = chain_db.read().await;
                    let imm = match db.get_immutable_tip_point() {
                        None | Some(dugite_primitives::block::Point::Origin) => None,
                        Some(dugite_primitives::block::Point::Specific(slot, hash)) => {
                            Some((slot.0, *hash.as_bytes()))
                        }
                    };
                    let sel = db
                        .get_tip_info()
                        .map(|(slot, hash, _bn)| (slot.0, *hash.as_bytes()));
                    let win = db.volatile_selected_points();
                    (imm, sel, win)
                };

                match gsm.state() {
                    GenesisSyncState::CaughtUp => {
                        loe_out.store(std::sync::Arc::new(LoeState::Disabled));
                    }
                    GenesisSyncState::PreSyncing => {
                        // Empty fragment anchored at the immutable tip —
                        // selection may extend at most k past it (NOT a
                        // total freeze).
                        loe_out.store(std::sync::Arc::new(LoeState::Fragment {
                            anchor: imm_tip.map(|(s, h)| LoePoint { slot: s, hash: h }),
                            entries: Vec::new(),
                            k,
                        }));
                    }
                    GenesisSyncState::Syncing => {
                        // Re-anchor peer fragments to the selection tip (the
                        // intersection point) so they all align, then snapshot.
                        let peers = registry.all();
                        if let Some((s, h)) = selection_tip {
                            for (_, st) in &peers {
                                let _ = st.reanchor_to_immutable_tip(s, &h);
                            }
                        }
                        let frags: Vec<(SocketAddr, crate::genesis_peer_state::CandidateFragment)> =
                            peers
                                .iter()
                                .map(|(a, st)| (*a, st.fragment_snapshot()))
                                .collect();

                        if frags.is_empty() {
                            // "Losing all peers effectively disables the LoE
                            // constraint": LoE = current selection.
                            loe_out.store(std::sync::Arc::new(LoeState::SelectionTip { k }));
                            gsm.set_loe_tip_slot(selection_tip.map(|(s, _)| s).unwrap_or(0));
                        } else {
                            // Shared FORWARD extension among peers, anchored at
                            // the selection tip.
                            let sp = crate::genesis_governor::shared_candidate_prefix(
                                selection_tip, &frags,
                            );
                            // The LoE fragment = volatile window (immutable →
                            // selection) ++ shared forward. The LoE tip is the
                            // furthest peer agreement.
                            let mut loe_entries: Vec<LoePoint> = volatile_window
                                .iter()
                                .map(|(slot, hash)| LoePoint {
                                    slot: *slot,
                                    hash: *hash,
                                })
                                .collect();
                            loe_entries.extend(sp.prefix.iter().copied());
                            let loe_tip = match loe_entries.last() {
                                Some(p) => crate::genesis_peer_state::WithOrigin::At(p.slot),
                                None => match imm_tip {
                                    Some((s, _)) => {
                                        crate::genesis_peer_state::WithOrigin::At(s)
                                    }
                                    None => crate::genesis_peer_state::WithOrigin::Origin,
                                },
                            };

                            // Per-era genesis window at (LoE tip + 1) —
                            // PastHorizon ⇒ skip this entire evaluation
                            // (Haskell: msgen = Nothing).
                            let next_slot = match loe_tip {
                                crate::genesis_peer_state::WithOrigin::Origin => 0,
                                crate::genesis_peer_state::WithOrigin::At(s) => {
                                    s.saturating_add(1)
                                }
                            };
                            let sgen = {
                                let eh = era_history.read().await;
                                eh.genesis_window_for_slot(dugite_primitives::time::SlotNo(
                                    next_slot,
                                ))
                            };
                            let Ok(sgen) = sgen else {
                                debug!(
                                    next_slot,
                                    "GDD: genesis window past horizon — skipping evaluation"
                                );
                                continue;
                            };

                            let new_loe = LoeState::Fragment {
                                anchor: imm_tip.map(|(s, h)| LoePoint { slot: s, hash: h }),
                                entries: loe_entries,
                                k,
                            };
                            let new_tip = new_loe.fragment_tip();
                            loe_out.store(std::sync::Arc::new(new_loe));
                            gsm.set_loe_tip_slot(match loe_tip {
                                crate::genesis_peer_state::WithOrigin::Origin => 0,
                                crate::genesis_peer_state::WithOrigin::At(s) => s,
                            });
                            if last_published_loe_tip != Some(new_tip) {
                                last_published_loe_tip = Some(new_tip);
                                // LoE tip advanced — chain selection should
                                // reprocess deferred blocks (Haskell
                                // triggerChainSelectionAsync). Wired to the
                                // ChainSelQueue in the trimToLoE task.
                            }

                            // ── GDD (densityDisconnect) ──────────────────
                            let gdd_peers: Vec<crate::genesis_governor::GddPeer> = sp
                                .suffixes
                                .iter()
                                .map(|(addr, suffix)| {
                                    let st = peers
                                        .iter()
                                        .find(|(a, _)| a == addr)
                                        .map(|(_, st)| st.clone());
                                    crate::genesis_governor::GddPeer {
                                        addr: *addr,
                                        suffix: suffix.clone(),
                                        idling: st
                                            .as_ref()
                                            .map(|s| s.is_idling())
                                            .unwrap_or(false),
                                        latest_slot: st.as_ref().and_then(|s| s.latest_slot()),
                                    }
                                })
                                .collect();
                            let bounds = crate::genesis_governor::density_bounds(
                                loe_tip, sgen, k, &gdd_peers,
                            );
                            let losers = crate::genesis_governor::losing_peers(&bounds);
                            if !losers.is_empty() {
                                info!(
                                    disconnecting = losers.len(),
                                    total_peers = gdd_peers.len(),
                                    "GDD: disconnecting peers with insufficient chain density"
                                );
                            }
                            for addr in losers {
                                if action_tx.send(GddAction::DisconnectPeer(addr)).await.is_err() {
                                    warn!("GSM actor: action channel closed, stopping GDD");
                                    return;
                                }
                                registry.deregister(&addr);
                            }
                        }
                    }
                }

                let snapshot = GsmSnapshot {
                    state: gsm.state(),
                    loe_slot: gsm.compute_loe_slot(),
                };
                if *snapshot_tx.borrow() != snapshot {
                    let _ = snapshot_tx.send(snapshot);
                }
            }
        }
    }
}

// ── Big ledger peer identification ──────────────────────────────────────────

/// Identify big ledger peers from the stake distribution.
///
/// Sorts pools by active stake descending and accumulates until 90 % of total
/// active stake is covered. Pools in the top 90 % are "big ledger peers" (BLPs).
///
/// Returns `(big_ledger_pool_ids, remaining_pool_ids)`.
#[allow(dead_code)] // future use: Genesis peer selection will use BLP classification
pub fn identify_big_ledger_peers(pool_stakes: &[(Vec<u8>, u64)]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    if pool_stakes.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let total_stake: u64 = pool_stakes.iter().map(|(_, s)| s).sum();
    let threshold = (total_stake as f64 * 0.9) as u64;

    let mut sorted: Vec<_> = pool_stakes.to_vec();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.1)); // descending by stake

    let mut accumulated = 0u64;
    let mut big_ledger = Vec::new();
    let mut remaining = Vec::new();

    for (pool_id, stake) in sorted {
        if accumulated < threshold {
            accumulated += stake;
            big_ledger.push(pool_id);
        } else {
            remaining.push(pool_id);
        }
    }

    (big_ledger, remaining)
}

// ── Peer snapshot loader ────────────────────────────────────────────────────

/// One peer-relay candidate parsed from a snapshot file.
#[derive(Debug, Clone)]
pub struct PeerSnapshotEntry {
    /// DNS name or stringified IP. May need DNS resolution.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// True if this entry is a Big Ledger Peer (top-90% stake) per the
    /// snapshot's classification. The IOG-distributed
    /// `peer-snapshot.json` lists only big-ledger pools, so every entry
    /// derived from `bigLedgerPools` is a BLP.
    pub is_big_ledger: bool,
}

/// Load a peer snapshot from a JSON file.
///
/// Supports two formats:
///
/// 1. **IOG cardano-node format** (top-level object with `bigLedgerPools`):
///    ```json
///    { "NetworkMagic": 2, "Point": {...},
///      "bigLedgerPools": [
///        { "accumulatedStake": 0.05, "relativeStake": 0.05,
///          "relays": [
///            { "address": "node.example.com", "port": 6501 }, ...
///          ]
///        }, ...
///      ]
///    }
///    ```
///    Each relay becomes one `PeerSnapshotEntry` with `is_big_ledger=true`.
///
/// 2. **Legacy flat format** (array of `{addr,port}` objects):
///    ```json
///    [ { "addr": "1.2.3.4", "port": 3001 }, ... ]
///    ```
///    Used for tests and pre-fetched peer lists. Entries are marked
///    `is_big_ledger=true` (legacy format is assumed to be BLPs since that's
///    what the field is for).
///
/// Returns the list of relay endpoints in declaration order. Hostnames are
/// kept as-is — DNS resolution happens later in the discovery loop.
pub fn load_peer_snapshot(path: &std::path::Path) -> Result<Vec<PeerSnapshotEntry>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read peer snapshot file {}: {e}", path.display()))?;

    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse peer snapshot JSON: {e}"))?;

    // Format 1: IOG cardano-node 10.x format with `bigLedgerPools`.
    if let Some(pools) = value.get("bigLedgerPools").and_then(|v| v.as_array()) {
        let mut peers = Vec::new();
        for pool in pools {
            let Some(relays) = pool.get("relays").and_then(|v| v.as_array()) else {
                continue;
            };
            for relay in relays {
                let host = relay.get("address").and_then(|v| v.as_str());
                let port = relay.get("port").and_then(|v| v.as_u64());
                if let (Some(host), Some(port)) = (host, port) {
                    if let Ok(port_u16) = u16::try_from(port) {
                        peers.push(PeerSnapshotEntry {
                            host: host.to_string(),
                            port: port_u16,
                            is_big_ledger: true,
                        });
                    }
                }
            }
        }
        return Ok(peers);
    }

    // Format 2: legacy flat array of `{addr,port}` objects.
    if let Some(entries) = value.as_array() {
        let mut peers = Vec::new();
        for entry in entries {
            let host = entry.get("addr").and_then(|v| v.as_str());
            let port = entry.get("port").and_then(|v| v.as_u64());
            if let (Some(host), Some(port)) = (host, port) {
                if let Ok(port_u16) = u16::try_from(port) {
                    peers.push(PeerSnapshotEntry {
                        host: host.to_string(),
                        port: port_u16,
                        is_big_ledger: true,
                    });
                }
            }
        }
        return Ok(peers);
    }

    Err(format!(
        "peer snapshot {} is neither a `bigLedgerPools` object nor a legacy array",
        path.display()
    ))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genesis_peer_state::{FragAnchor, FragEntry, PeerStateRegistry};
    use std::sync::Arc;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn taddr(n: u8) -> SocketAddr {
        format!("10.1.0.{n}:3001").parse().unwrap()
    }

    /// Helper: create a GSM with test-friendly defaults and a fresh registry.
    fn make_gsm(enabled: bool, marker_path: &str) -> (GenesisStateMachine, Arc<PeerStateRegistry>) {
        let _ = std::fs::remove_file(marker_path);
        let config = GsmConfig {
            min_active_blp: 3,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0, // no dwell for most tests
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker_path),
            ..Default::default()
        };
        let registry = PeerStateRegistry::new();
        let mut gsm = GenesisStateMachine::new(config, enabled, registry.clone(), None);
        gsm.set_jitter(0); // deterministic
        (gsm, registry)
    }

    /// Register an idling peer whose fragment head is at `block_no` —
    /// the minimum CaughtUp-eligible peer (Haskell: nonempty handle map,
    /// all idling, candidate not better than selection).
    fn add_idling_peer(reg: &Arc<PeerStateRegistry>, n: u8, block_no: u64) {
        let st = reg.register(taddr(n), FragAnchor::Origin);
        st.on_roll_forward(FragEntry {
            slot: block_no * 10,
            hash: h(n),
            block_no,
        });
        st.on_await_reply();
    }

    // ── State transitions ────────────────────────────────────────────────

    #[test]
    fn test_state_presyncing_to_syncing() {
        let (mut gsm, _reg) = make_gsm(true, "/tmp/gsm_t1.marker");
        assert_eq!(gsm.state(), GenesisSyncState::PreSyncing);
        // Below HAA: stays.
        assert!(gsm.evaluate(2, 0, 9_999).is_none());
        assert_eq!(gsm.state(), GenesisSyncState::PreSyncing);
        // HAA satisfied: Syncing.
        assert_eq!(gsm.evaluate(3, 0, 9_999), Some(GenesisSyncState::Syncing));
    }

    #[test]
    fn test_state_haa_loss_regresses_to_presyncing() {
        // HAA loss must regress when the tip is STALE (≥ syncing_startup_threshold).
        // `make_gsm` uses Default which sets syncing_startup_threshold_secs=129_600;
        // a tip age of 200_000 s is well past the threshold.
        let (mut gsm, _reg) = make_gsm(true, "/tmp/gsm_t2.marker");
        gsm.evaluate(5, 0, 200_000);
        assert_eq!(gsm.state(), GenesisSyncState::Syncing);
        assert_eq!(
            gsm.evaluate(1, 0, 200_000),
            Some(GenesisSyncState::PreSyncing)
        );
    }

    #[test]
    fn test_state_syncing_to_caught_up() {
        let (mut gsm, reg) = make_gsm(true, "/tmp/gsm_t3.marker");
        gsm.evaluate(5, 0, 9_999);
        // ZERO peers → never CaughtUp (Haskell `not (Map.null states)`).
        assert!(gsm.evaluate(5, 100, 10).is_none());
        // One idling peer whose candidate (bn 90) is not better than the
        // selection (bn 100) → CaughtUp.
        add_idling_peer(&reg, 1, 90);
        assert_eq!(gsm.evaluate(5, 100, 10), Some(GenesisSyncState::CaughtUp));
        // Marker written.
        assert!(PathBuf::from("/tmp/gsm_t3.marker").exists());
        let _ = std::fs::remove_file("/tmp/gsm_t3.marker");
    }

    #[test]
    fn test_better_candidate_blocks_caught_up() {
        // Haskell stage 2: a candidate fragment head with a HIGHER block
        // number than the selection blocks CaughtUp.
        let (mut gsm, reg) = make_gsm(true, "/tmp/gsm_t3b.marker");
        gsm.evaluate(5, 0, 9_999);
        add_idling_peer(&reg, 1, 150); // candidate bn 150 > selection bn 100
        assert!(
            gsm.evaluate(5, 100, 10).is_none(),
            "better candidate blocks"
        );
        // Selection catches up to bn 150 → no candidate better → CaughtUp.
        assert_eq!(gsm.evaluate(5, 150, 10), Some(GenesisSyncState::CaughtUp));
        let _ = std::fs::remove_file("/tmp/gsm_t3b.marker");
    }

    #[test]
    fn test_non_idling_peer_blocks_caught_up() {
        let (mut gsm, reg) = make_gsm(true, "/tmp/gsm_t4.marker");
        gsm.evaluate(5, 0, 9_999);
        // One peer, NOT idling → blocks CaughtUp.
        let st = reg.register(taddr(1), FragAnchor::Origin);
        assert!(gsm.evaluate(5, 100, 10).is_none());
        // Peer goes idle (candidate empty → not better) → CaughtUp.
        st.on_await_reply();
        assert_eq!(gsm.evaluate(5, 100, 10), Some(GenesisSyncState::CaughtUp));
        let _ = std::fs::remove_file("/tmp/gsm_t4.marker");
    }

    #[test]
    fn test_startup_marker_staleness() {
        // Haskell initializationGsmState: marker + too-old tip → marker
        // deleted, PreSyncing; marker + fresh tip → CaughtUp; marker +
        // unknown age → CaughtUp.
        let marker = "/tmp/gsm_t5.marker";
        std::fs::write(marker, "caught_up").unwrap();
        let config = GsmConfig {
            max_caught_up_age_secs: 600,
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };
        // Stale tip (age 601 > 600): PreSyncing + marker removed.
        let gsm =
            GenesisStateMachine::new(config.clone(), true, PeerStateRegistry::new(), Some(601));
        assert_eq!(gsm.state(), GenesisSyncState::PreSyncing);
        assert!(!PathBuf::from(marker).exists(), "stale marker deleted");

        // Fresh tip: CaughtUp.
        std::fs::write(marker, "caught_up").unwrap();
        let gsm =
            GenesisStateMachine::new(config.clone(), true, PeerStateRegistry::new(), Some(10));
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);

        // Unknown age: trust the marker.
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), None);
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_bootstrap_syncing_override() {
        // From-genesis / far-behind, no marker, OLD tip (age >> threshold so the
        // #757 recent-tip path does NOT fire): default = PreSyncing (the stall);
        // with DUGITE_GENESIS_BOOTSTRAP_SYNCING set = Syncing (bootstrap bypass).
        let marker = "/tmp/gsm_bootstrap_override.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            syncing_startup_threshold_secs: 100,
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };

        std::env::remove_var("DUGITE_GENESIS_BOOTSTRAP_SYNCING");
        assert!(!bootstrap_syncing_override());
        let gsm = GenesisStateMachine::new(
            config.clone(),
            true,
            PeerStateRegistry::new(),
            Some(1_000_000),
        );
        assert_eq!(
            gsm.state(),
            GenesisSyncState::PreSyncing,
            "default (env unset): far-behind from-genesis stalls in PreSyncing"
        );

        std::env::set_var("DUGITE_GENESIS_BOOTSTRAP_SYNCING", "1");
        assert!(bootstrap_syncing_override());
        let gsm = GenesisStateMachine::new(
            config.clone(),
            true,
            PeerStateRegistry::new(),
            Some(1_000_000),
        );
        assert_eq!(
            gsm.state(),
            GenesisSyncState::Syncing,
            "opt-in: starts in Syncing (bootstrap HAA bypass)"
        );

        // The override lives ONLY in the no-marker branch: a caught_up marker must
        // still win (the bypass must never downgrade a caught-up node).
        std::fs::write(marker, "caught_up").unwrap();
        let config2 = GsmConfig {
            max_caught_up_age_secs: 10_000_000,
            syncing_startup_threshold_secs: 100,
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };
        let gsm = GenesisStateMachine::new(config2, true, PeerStateRegistry::new(), Some(10));
        assert_eq!(
            gsm.state(),
            GenesisSyncState::CaughtUp,
            "caught_up marker still wins over the override"
        );

        std::env::remove_var("DUGITE_GENESIS_BOOTSTRAP_SYNCING");
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_caught_up_dwell_blocks_regression() {
        let marker = "/tmp/gsm_t6.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            min_active_blp: 1,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 10_000, // long dwell
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };
        let registry = PeerStateRegistry::new();
        add_idling_peer(&registry, 1, 50);
        let mut gsm = GenesisStateMachine::new(config, true, registry, None);
        gsm.set_jitter(0);
        gsm.evaluate(5, 100, 9_999);
        gsm.evaluate(5, 100, 10);
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        // Stale tip but dwell not elapsed → stays CaughtUp.
        assert!(gsm.evaluate(5, 100, 99_999).is_none());
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn test_caught_up_stale_tip_regresses_and_removes_marker() {
        let marker = "/tmp/gsm_t7.marker";
        let (mut gsm, reg) = make_gsm(true, marker);
        add_idling_peer(&reg, 1, 50);
        gsm.evaluate(5, 100, 9_999);
        gsm.evaluate(5, 100, 10);
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        assert!(PathBuf::from(marker).exists());
        assert_eq!(
            gsm.evaluate(5, 100, 601),
            Some(GenesisSyncState::PreSyncing)
        );
        assert!(!PathBuf::from(marker).exists(), "marker removed on regress");
    }

    #[test]
    fn test_marker_fast_restart() {
        let marker = "/tmp/gsm_t8.marker";
        std::fs::write(marker, "caught_up").unwrap();
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), None);
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        let _ = std::fs::remove_file(marker);
    }

    // ── Issue #757: Mithril snapshot bootstrap startup state ─────────────

    /// A node that has no marker file but whose tip is very recent (well within
    /// the stability window) should start in **Syncing**, not PreSyncing.
    ///
    /// Root cause: PreSyncing LoE caps selection at k blocks past the snapshot
    /// tip. For a Mithril snapshot (~2h old) BLPs never arrive fast enough to
    /// satisfy the HAA before LoE fires, so the node stalls at exactly
    /// k=snapshot_tip+2160 blocks.
    ///
    /// Fix: `syncing_startup_threshold_secs` (≈ sgen × slot_length = 129,600 s
    /// on mainnet/preprod) allows the no-marker path to start in Syncing when
    /// the tip age is < threshold.
    #[test]
    fn test_mithril_snapshot_bootstrap_starts_in_syncing() {
        let marker = "/tmp/gsm_t757a.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            // Default threshold is 129_600s; tip age 7_200s (2 h) << threshold.
            syncing_startup_threshold_secs: 129_600,
            ..Default::default()
        };
        // Tip age 2 hours — typical for a fresh Mithril snapshot.
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(7_200));
        assert_eq!(
            gsm.state(),
            GenesisSyncState::Syncing,
            "recent tip (7200s) with no marker should start in Syncing, not PreSyncing"
        );
    }

    /// A node whose tip is STALE (age ≥ syncing_startup_threshold) should still
    /// start in PreSyncing — it requires proper HAA bootstrap.
    #[test]
    fn test_stale_tip_no_marker_starts_in_presyncing() {
        let marker = "/tmp/gsm_t757b.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
            ..Default::default()
        };
        // Tip age > stability window — this is a genuine from-genesis sync.
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(200_000));
        assert_eq!(
            gsm.state(),
            GenesisSyncState::PreSyncing,
            "stale tip (200000s > 129600s threshold) with no marker should be PreSyncing"
        );
    }

    /// When `initial_tip_age_secs` is `None` (unknown, e.g. empty ChainDB),
    /// the node must NOT start in Syncing — default to PreSyncing.
    #[test]
    fn test_unknown_tip_age_no_marker_starts_in_presyncing() {
        let marker = "/tmp/gsm_t757c.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
            ..Default::default()
        };
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), None);
        assert_eq!(
            gsm.state(),
            GenesisSyncState::PreSyncing,
            "unknown tip age with no marker must be PreSyncing (cannot trust recent heuristic)"
        );
    }

    /// Setting `syncing_startup_threshold_secs = 0` disables the optimisation
    /// entirely — strict Haskell semantics; even a very recent tip starts PreSyncing.
    #[test]
    fn test_threshold_zero_disables_syncing_startup() {
        let marker = "/tmp/gsm_t757d.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 0, // disabled
            ..Default::default()
        };
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(100));
        assert_eq!(
            gsm.state(),
            GenesisSyncState::PreSyncing,
            "threshold=0 must disable the Mithril-bootstrap optimisation"
        );
    }

    /// Tip age exactly at the boundary (age == threshold) must be PreSyncing —
    /// the condition is strict `<`, not `<=`.
    #[test]
    fn test_tip_age_at_threshold_boundary_is_presyncing() {
        let marker = "/tmp/gsm_t757e.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
            ..Default::default()
        };
        // Age exactly equal to threshold — should be PreSyncing (not recent).
        let gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(129_600));
        assert_eq!(
            gsm.state(),
            GenesisSyncState::PreSyncing,
            "tip age == threshold is NOT recent; boundary must be exclusive"
        );
    }

    // ── Issue #757 corrected fix: evaluate() HAA-loss suppression ────────

    /// LIVE-DISPROVEN v1 regression: a node that started in Syncing (recent tip,
    /// no marker) must NOT regress to PreSyncing when HAA is transiently lost
    /// while the tip is still recent.
    ///
    /// Scenario: Mithril-bootstrapped preprod node — 0 BLPs (only bootstrap
    /// peers in topology), 0 local roots.  `active_blp_count` reported as 0 by
    /// the SyncStatus emitter.  Before this fix `evaluate()` immediately
    /// regressed to PreSyncing on the first tick after startup, re-entering the
    /// k-block stall.
    #[test]
    fn test_recent_tip_syncing_haa_loss_suppressed() {
        // Start in Syncing (recent tip path).
        let marker = "/tmp/gsm_t757f.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            min_active_blp: 5,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0,
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
        };
        // Tip age 7 200 s — node started in Syncing via recent-tip path.
        let mut gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(7_200));
        gsm.set_jitter(0);
        assert_eq!(
            gsm.state(),
            GenesisSyncState::Syncing,
            "should start Syncing"
        );

        // First evaluate: active_blp=0, tip still recent (7 300 s < 129 600 s).
        // Must NOT regress.
        assert!(
            gsm.evaluate(0, 100, 7_300).is_none(),
            "HAA lost but tip recent — must stay Syncing"
        );
        assert_eq!(gsm.state(), GenesisSyncState::Syncing);

        // Repeated ticks with active_blp=0, tip still recent.
        assert!(gsm.evaluate(0, 200, 10_000).is_none());
        assert!(gsm.evaluate(0, 300, 50_000).is_none());
        assert_eq!(gsm.state(), GenesisSyncState::Syncing);
        let _ = std::fs::remove_file(marker);
    }

    /// Once the tip goes stale (age ≥ syncing_startup_threshold_secs) while
    /// HAA is still unmet, the node MUST regress to PreSyncing — this is the
    /// from-genesis cold-start scenario where real HAA protection is required.
    #[test]
    fn test_syncing_haa_loss_with_stale_tip_regresses() {
        let marker = "/tmp/gsm_t757g.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            min_active_blp: 5,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0,
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
        };
        // Start in Syncing via recent-tip path.
        let mut gsm = GenesisStateMachine::new(config, true, PeerStateRegistry::new(), Some(7_200));
        gsm.set_jitter(0);
        assert_eq!(gsm.state(), GenesisSyncState::Syncing);

        // Tip goes stale AND HAA is lost → must regress.
        let result = gsm.evaluate(0, 100, 130_000); // age > threshold
        assert_eq!(
            result,
            Some(GenesisSyncState::PreSyncing),
            "HAA lost AND tip stale must regress to PreSyncing"
        );
        assert_eq!(gsm.state(), GenesisSyncState::PreSyncing);
        let _ = std::fs::remove_file(marker);
    }

    /// PRAOS mode: `evaluate()` must be a no-op regardless of active_blp or
    /// tip age — enabled=false means genesis constraints are off entirely.
    #[test]
    fn test_praos_evaluate_noop_with_recent_tip() {
        let marker = "/tmp/gsm_t757h.marker";
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            min_active_blp: 5,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0,
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker),
            syncing_startup_threshold_secs: 129_600,
        };
        // Praos mode (enabled=false) — always starts CaughtUp, evaluate is inert.
        let mut gsm = GenesisStateMachine::new(config, false, PeerStateRegistry::new(), Some(100));
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        assert!(gsm.evaluate(0, 0, 100).is_none(), "praos: evaluate noop");
        assert!(
            gsm.evaluate(0, 0, 500_000).is_none(),
            "praos: evaluate noop even with stale tip"
        );
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        assert!(
            !PathBuf::from(marker).exists(),
            "praos: marker never written"
        );
    }

    #[test]
    fn test_disabled_mode_is_inert() {
        let (mut gsm, _reg) = make_gsm(false, "/tmp/gsm_t9.marker");
        assert_eq!(gsm.state(), GenesisSyncState::CaughtUp);
        assert!(gsm.evaluate(0, 0, 99_999).is_none());
        assert_eq!(gsm.compute_loe_slot(), None);
        // Marker never written in praos mode.
        assert!(!PathBuf::from("/tmp/gsm_t9.marker").exists());
    }

    // ── LoE metric view ──────────────────────────────────────────────────

    #[test]
    fn test_loe_slot_metric_per_state() {
        let (mut gsm, reg) = make_gsm(true, "/tmp/gsm_t10.marker");
        assert_eq!(gsm.compute_loe_slot(), Some(0), "PreSyncing pins 0");
        gsm.evaluate(5, 100, 9_999);
        gsm.set_loe_tip_slot(1234);
        assert_eq!(gsm.compute_loe_slot(), Some(1234), "Syncing reports tip");
        add_idling_peer(&reg, 1, 50);
        gsm.evaluate(5, 100, 10);
        assert_eq!(gsm.compute_loe_slot(), None, "CaughtUp unconstrained");
        let _ = std::fs::remove_file("/tmp/gsm_t10.marker");
    }

    // ── CSJ objection gate ───────────────────────────────────────────────

    fn make_syncing_gsm(marker: &str) -> GenesisStateMachine {
        let _ = std::fs::remove_file(marker);
        let config = GsmConfig {
            min_active_blp: 1,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0,
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 100,
            security_param_k: 2160,
            marker_path: PathBuf::from(marker),
            ..Default::default()
        };
        let registry = PeerStateRegistry::new();
        // CaughtUp needs ≥1 idling, not-better peer.
        add_idling_peer(&registry, 9, 50);
        let mut gsm = GenesisStateMachine::new(config, true, registry, None);
        gsm.set_jitter(0);
        gsm.evaluate(5, 100, 0);
        assert_eq!(gsm.state(), GenesisSyncState::Syncing);
        gsm
    }

    #[test]
    fn test_csj_objection_blocks_caught_up() {
        let mut gsm = make_syncing_gsm("/tmp/gsm_t11.marker");
        gsm.raise_objection(taddr(1));
        assert!(!gsm.csj_gate_satisfied());
        assert!(gsm.evaluate(5, 100, 10).is_none(), "objection blocks");
        gsm.resolve_objection(&taddr(1), CsjObjectionOutcome::DynamoWins);
        assert!(gsm.csj_gate_satisfied());
        assert_eq!(gsm.evaluate(5, 100, 10), Some(GenesisSyncState::CaughtUp));
        let _ = std::fs::remove_file("/tmp/gsm_t11.marker");
    }

    #[test]
    fn test_csj_multiple_objections_all_must_resolve() {
        let mut gsm = make_syncing_gsm("/tmp/gsm_t12.marker");
        gsm.raise_objection(taddr(1));
        gsm.raise_objection(taddr(2));
        gsm.resolve_objection(&taddr(1), CsjObjectionOutcome::ObjectorWins);
        assert!(!gsm.csj_gate_satisfied());
        gsm.resolve_objection(&taddr(2), CsjObjectionOutcome::DynamoWins);
        assert!(gsm.csj_gate_satisfied());
        let _ = std::fs::remove_file("/tmp/gsm_t12.marker");
    }

    // ── Actor integration (governor: LoE publication + GDD kills) ───────

    struct ActorHarness {
        event_tx: mpsc::Sender<GsmEvent>,
        snapshot_rx: watch::Receiver<GsmSnapshot>,
        action_rx: mpsc::Receiver<GddAction>,
        registry: Arc<PeerStateRegistry>,
        loe: Arc<arc_swap::ArcSwap<dugite_consensus::loe::LoeState>>,
        _tmp: tempfile::TempDir,
    }

    fn spawn_actor(enabled: bool, min_blp: usize, k: u64) -> ActorHarness {
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = tmp.path().join("caught_up.marker");
        let config = GsmConfig {
            min_active_blp: min_blp,
            max_caught_up_age_secs: 600,
            min_caught_up_dwell_secs: 0,
            anti_thundering_herd_max_secs: 0,
            gdd_rate_limit_ms: 20, // fast ticks for tests
            security_param_k: k,
            marker_path: marker,
            ..Default::default()
        };
        let registry = PeerStateRegistry::new();
        let chain_db = Arc::new(tokio::sync::RwLock::new(
            dugite_storage::ChainDB::open(tmp.path()).expect("chaindb"),
        ));
        let params = dugite_consensus::EraParams {
            epoch_size: 1_000,
            slot_length_ms: 1_000,
            safe_zone: 200,
            genesis_window: 50, // small sgen for tests
        };
        let era_history = Arc::new(tokio::sync::RwLock::new(
            dugite_consensus::EraHistory::from_genesis(params.clone(), params, 0),
        ));
        let loe = Arc::new(arc_swap::ArcSwap::from_pointee(
            dugite_consensus::loe::LoeState::Disabled,
        ));
        let (event_tx, event_rx) = mpsc::channel(256);
        let (snapshot_tx, snapshot_rx) = watch::channel(GsmSnapshot {
            state: GenesisSyncState::PreSyncing,
            loe_slot: Some(0),
        });
        let (action_tx, action_rx) = mpsc::channel(64);
        tokio::spawn(run_gsm_actor(
            config,
            enabled,
            registry.clone(),
            chain_db,
            era_history,
            loe.clone(),
            None,
            event_rx,
            snapshot_tx,
            action_tx,
        ));
        ActorHarness {
            event_tx,
            snapshot_rx,
            action_rx,
            registry,
            loe,
            _tmp: tmp,
        }
    }

    async fn drive_to_syncing(h: &ActorHarness) {
        h.event_tx
            .send(GsmEvent::SyncStatus {
                active_blp_count: 5,
                selection_block_no: 0,
                tip_age_secs: 9_999,
            })
            .await
            .unwrap();
        let mut rx = h.snapshot_rx.clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if rx.borrow().state == GenesisSyncState::Syncing {
                    break;
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("reached Syncing");
    }

    #[tokio::test]
    async fn test_actor_praos_mode_publishes_disabled_loe() {
        let h = spawn_actor(false, 1, 10);
        // Give the actor a couple of ticks.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(h.loe.load().is_disabled(), "praos: LoE must stay Disabled");
    }

    #[tokio::test]
    async fn test_actor_presyncing_publishes_empty_fragment() {
        let h = spawn_actor(true, 5, 10);
        // Nudge an event so a (dirty) tick fires.
        h.event_tx
            .send(GsmEvent::PeerActive { addr: taddr(1) })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        match &**h.loe.load() {
            dugite_consensus::loe::LoeState::Fragment { entries, k, .. } => {
                assert!(
                    entries.is_empty(),
                    "PreSyncing: empty fragment (k allowance)"
                );
                assert_eq!(*k, 10);
            }
            other => panic!("expected Fragment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_actor_syncing_zero_peers_publishes_selection_tip() {
        let h = spawn_actor(true, 1, 10);
        drive_to_syncing(&h).await;
        h.event_tx
            .send(GsmEvent::PeerActive { addr: taddr(1) })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            matches!(
                &**h.loe.load(),
                dugite_consensus::loe::LoeState::SelectionTip { k: 10 }
            ),
            "zero peers: LoE = selection (constraint effectively lifted)"
        );
    }

    #[tokio::test]
    async fn test_actor_syncing_publishes_shared_prefix_and_kills_sparse_peer() {
        let h = spawn_actor(true, 1, 3);
        drive_to_syncing(&h).await;

        // Two peers anchored at Origin (the test ChainDB's immutable tip is
        // Origin). Both serve the same first block, then diverge: the dense
        // peer serves k+1 = 4 more blocks in the window; the sparse peer
        // serves one block on a different fork and goes idle.
        let dense = h.registry.register(taddr(1), FragAnchor::Origin);
        let sparse = h.registry.register(taddr(2), FragAnchor::Origin);
        for st in [&dense, &sparse] {
            st.on_roll_forward(FragEntry {
                slot: 1,
                hash: h2(0xaa),
                block_no: 1,
            });
        }
        for (i, slot) in [(2u8, 5u64), (3, 6), (4, 7), (5, 8)] {
            dense.on_roll_forward(FragEntry {
                slot,
                hash: h2(i),
                block_no: slot,
            });
        }
        sparse.on_roll_forward(FragEntry {
            slot: 9,
            hash: h2(0xbb),
            block_no: 2,
        });
        sparse.on_await_reply();

        // Wake the governor.
        h.event_tx
            .send(GsmEvent::PeerIdling { addr: taddr(2) })
            .await
            .unwrap();

        // Expect the sparse peer to be killed by the GDD.
        let mut action_rx = h.action_rx;
        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("GDD verdict within 2s")
            .expect("channel open");
        match action {
            GddAction::DisconnectPeer(addr) => assert_eq!(addr, taddr(2)),
        }

        // The published LoE fragment is the shared prefix: exactly the one
        // common block at slot 1.
        match &**h.loe.load() {
            dugite_consensus::loe::LoeState::Fragment { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].slot, 1);
            }
            other => panic!("expected Fragment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_actor_caught_up_disables_loe() {
        let h = spawn_actor(true, 1, 10);
        drive_to_syncing(&h).await;
        // CaughtUp needs ≥1 idling, not-better peer in the registry.
        add_idling_peer(&h.registry, 9, 50);
        h.event_tx
            .send(GsmEvent::SyncStatus {
                active_blp_count: 5,
                selection_block_no: 1_000,
                tip_age_secs: 10,
            })
            .await
            .unwrap();
        let mut rx = h.snapshot_rx.clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if rx.borrow().state == GenesisSyncState::CaughtUp {
                    break;
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("reached CaughtUp");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(h.loe.load().is_disabled(), "CaughtUp: LoE disabled");
    }

    #[tokio::test]
    async fn test_csj_events_via_actor_channel() {
        let h = spawn_actor(true, 1, 10);
        drive_to_syncing(&h).await;
        // CaughtUp needs ≥1 idling, not-better peer in the registry.
        add_idling_peer(&h.registry, 9, 50);
        // Raise an objection, verify CaughtUp is blocked, resolve, verify
        // CaughtUp becomes reachable.
        h.event_tx
            .send(GsmEvent::ObjectionRaised {
                peer: taddr(7),
                lo: Point::Origin,
                hi: Point::Specific(100, [1; 32]),
            })
            .await
            .unwrap();
        h.event_tx
            .send(GsmEvent::SyncStatus {
                active_blp_count: 5,
                selection_block_no: 1_000,
                tip_age_secs: 10,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(h.snapshot_rx.borrow().state, GenesisSyncState::Syncing);

        h.event_tx
            .send(GsmEvent::ObjectionResolved {
                peer: taddr(7),
                outcome: CsjObjectionOutcome::DynamoWins,
            })
            .await
            .unwrap();
        h.event_tx
            .send(GsmEvent::SyncStatus {
                active_blp_count: 5,
                selection_block_no: 1_000,
                tip_age_secs: 10,
            })
            .await
            .unwrap();
        let mut rx = h.snapshot_rx.clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if rx.borrow().state == GenesisSyncState::CaughtUp {
                    break;
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("CaughtUp after objection resolution");
    }

    fn h2(b: u8) -> [u8; 32] {
        [b; 32]
    }

    // ── Big ledger peer identification ───────────────────────────────────

    #[test]
    fn test_identify_big_ledger_peers() {
        let pools = vec![
            (vec![1], 1000), // 50%
            (vec![2], 500),  // 25%
            (vec![3], 300),  // 15%
            (vec![4], 100),  // 5%
            (vec![5], 100),  // 5%
        ];

        let (big, remaining) = identify_big_ledger_peers(&pools);
        assert!(big.len() >= 2, "Should have at least 2 big ledger peers");
        assert!(!remaining.is_empty(), "Should have remaining small pools");
    }

    #[test]
    fn test_identify_big_ledger_peers_empty() {
        let (big, remaining) = identify_big_ledger_peers(&[]);
        assert!(big.is_empty());
        assert!(remaining.is_empty());
    }

    // ── Peer snapshot loader ─────────────────────────────────────────────

    #[test]
    fn test_load_peer_snapshot() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_peer_snapshot.json");
        std::fs::write(
            &path,
            r#"[{"addr": "1.2.3.4", "port": 3001}, {"addr": "5.6.7.8", "port": 3002}]"#,
        )
        .unwrap();

        let peers = load_peer_snapshot(&path).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].host, "1.2.3.4");
        assert_eq!(peers[0].port, 3001);
        assert!(peers[0].is_big_ledger);
        assert_eq!(peers[1].host, "5.6.7.8");
        assert_eq!(peers[1].port, 3002);
        assert!(peers[1].is_big_ledger);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_peer_snapshot_iog_format() {
        // Mirrors the structure of IOG's official peer-snapshot.json
        // distributed at book.world.dev.cardano.org/environments/preview.
        let dir = std::env::temp_dir();
        let path = dir.join("test_peer_snapshot_iog.json");
        std::fs::write(
            &path,
            r#"{
              "NetworkMagic": 2,
              "NodeToClientVersion": 23,
              "Point": { "blockPointHash": "deadbeef", "blockPointSlot": 100 },
              "bigLedgerPools": [
                {
                  "accumulatedStake": 0.05,
                  "relativeStake": 0.05,
                  "relays": [
                    { "address": "node1.example.com", "port": 6501 },
                    { "address": "node2.example.com", "port": 6502 }
                  ]
                },
                {
                  "accumulatedStake": 0.10,
                  "relativeStake": 0.05,
                  "relays": [
                    { "address": "10.0.0.1", "port": 3001 }
                  ]
                }
              ]
            }"#,
        )
        .unwrap();

        let peers = load_peer_snapshot(&path).unwrap();
        assert_eq!(peers.len(), 3);
        assert_eq!(peers[0].host, "node1.example.com");
        assert_eq!(peers[0].port, 6501);
        assert!(peers[0].is_big_ledger);
        assert_eq!(peers[1].host, "node2.example.com");
        assert_eq!(peers[1].port, 6502);
        assert_eq!(peers[2].host, "10.0.0.1");
        assert_eq!(peers[2].port, 3001);
        assert!(peers[2].is_big_ledger);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_peer_snapshot_unrecognised_format_errors() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_peer_snapshot_bad.json");
        // Object without bigLedgerPools — must not silently return empty
        std::fs::write(&path, r#"{ "NetworkMagic": 2 }"#).unwrap();
        let result = load_peer_snapshot(&path);
        assert!(result.is_err(), "unrecognised format must return an error");
        let _ = std::fs::remove_file(&path);
    }
}
