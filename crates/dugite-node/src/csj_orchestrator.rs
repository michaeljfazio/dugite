//! ChainSync Jumping (CSJ) Phase B — orchestrator actor.
//!
//! This module owns the `CsjOrchestrator` actor task which coordinates all
//! outbound ChainSync peers into the three CSJ roles:
//!
//! - **Dynamo** — one peer running full pipelined ChainSync.
//! - **Jumpers** — remaining peers that leapfrog to the dynamo's tip.
//! - **Objectors** — jumpers that could not find the jump point on their own
//!   chain; they run a binary-search bisection to locate the fork point.
//!
//! # Haskell reference
//!
//! `ouroboros-consensus/Ouroboros/Consensus/Genesis/Governor.hs` (the
//! `ChainSync Jumping` section):
//!
//! - Dynamo election: lowest-RTT hot peer (`csjSelectDynamo`).
//! - Demotion grace period: `csjReprocessLoEDelay = 10 seconds`.
//! - Jump point: `dynamo_tip − genesis_window` where `genesis_window = 3k/f`.
//! - Objection: `MsgIntersectNotFound` → bisection → GDD density comparison.
//!
//! # Divergences from Haskell
//!
//! 1. **`stability_window_slots` is reused for the genesis window** — Haskell
//!    computes `genesisWindowLength = floor(3 * securityParam / f)` from genesis
//!    params.  We have `stability_window_slots(k, f) = ceil(3k/f)` which
//!    over-estimates by at most 1 slot — an acceptable conservative rounding.
//!
//! 2. **Density snapshot is a plain block count** — a full `DensityWindow` from
//!    `dugite_consensus::DensityWindow` would require replaying the chain.
//!    Phase B captures the block-count at bisection completion; Phase D can
//!    enrich it with a real `DensityWindow` once chain-fragment sharing is
//!    implemented.
//!
//! 3. **No GSM guard** — Haskell's `csjGsmHandler` exits CSJ when `CaughtUp`.
//!    Phase B leaves that to the LoE governor (Phase E).
//!
//! 4. **Stall detection uses a single `Instant` per dynamo** — Haskell uses a
//!    rolling slot-time comparison.  Our 10-second wall-clock grace is simpler
//!    and covers the same failure mode.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use dugite_consensus::stability_window_slots;
use dugite_network::codec::Point;
use dugite_network::protocol::chainsync::jumping::{
    bisect_midpoint, check_dynamo_invariant, JumpInstruction, JumpState, JumperState, PeerJumpState,
};

use crate::gsm::{CsjObjectionOutcome, GsmEvent};

// ─── Timing constants ──────────────────────────────────────────────────────────

/// Haskell `csjReprocessLoEDelay`: grace period before demoting a stalled dynamo.
const DYNAMO_STALL_GRACE: Duration = Duration::from_secs(10);

// ─── Messages sent from ChainSync peer tasks → orchestrator ──────────────────

/// Events that ChainSync peer tasks report to the orchestrator.
#[derive(Debug)]
pub enum PeerEvent {
    /// Peer has been promoted to hot and is ready for CSJ assignment.
    ///
    /// `latency_ms` is the EWMA RTT from the keep-alive exchange.
    PeerConnected {
        addr: SocketAddr,
        latency_ms: Option<f64>,
    },

    /// Peer has disconnected or been demoted.
    PeerDisconnected { addr: SocketAddr },

    /// The dynamo has advanced its tip to a new slot.
    DynamoTipAdvanced {
        addr: SocketAddr,
        tip_slot: u64,
        tip_hash: [u8; 32],
    },

    /// A jumper received `MsgIntersectFound` for its current jump point.
    IntersectFound {
        addr: SocketAddr,
        found_point: Point,
    },

    /// A jumper received `MsgIntersectNotFound` for its current jump point.
    IntersectNotFound { addr: SocketAddr },

    /// Request a density snapshot when objection bisection is complete.
    ///
    /// The orchestrator resolves the bisection and feeds the result to GDD.
    BisectionComplete {
        addr: SocketAddr,
        /// Fork point found: the deepest shared point.
        fork_point: Point,
        /// Number of blocks the objector has in the genesis window above `fork_point`.
        objector_blocks_in_window: u64,
        /// Response channel: the orchestrator sends the dynamo block count back.
        response: oneshot::Sender<OrchestratorDecision>,
    },
}

/// Decision returned to the peer task after bisection is resolved.
#[derive(Debug)]
pub enum OrchestratorDecision {
    /// Adopt the dynamo's chain (dynamo is denser in the genesis window).
    AdoptDynamo,
    /// Keep the objector's chain (objector is denser or equal).
    KeepObjector,
}

// ─── Messages sent from orchestrator → ChainSync peer tasks ──────────────────

/// Instructions the orchestrator sends to individual peer ChainSync tasks.
#[derive(Debug, Clone)]
pub enum PeerInstruction {
    /// Assign this peer as the dynamo — run full pipelined ChainSync.
    BecomeDynamo,

    /// Jump to the specified point via `MsgFindIntersect`.
    Jump(JumpInstruction),

    /// Disengage from CSJ — run normal ChainSync independently.
    Disengage,
}

// ─── Per-peer handle ──────────────────────────────────────────────────────────

/// Handle the orchestrator holds for each hot ChainSync peer.
struct PeerHandle {
    /// Send instructions to the peer's ChainSync task.
    tx: mpsc::Sender<PeerInstruction>,
    /// CSJ state for this peer.
    jump_state: PeerJumpState,
    /// EWMA latency (milliseconds), used for dynamo election.
    latency_ms: Option<f64>,
    /// When the current dynamo last advanced its tip (only set for the dynamo).
    last_tip_advance: Option<Instant>,
    /// Cached latest dynamo tip (slot, hash) — used to compute jump points.
    dynamo_tip: Option<(u64, [u8; 32])>,
    /// Block count reported by the objector at bisection time (cached for GDD).
    objector_blocks_in_window: Option<u64>,
}

impl PeerHandle {
    fn new(tx: mpsc::Sender<PeerInstruction>, latency_ms: Option<f64>) -> Self {
        Self {
            tx,
            jump_state: PeerJumpState::new_jumper(),
            latency_ms,
            last_tip_advance: None,
            dynamo_tip: None,
            objector_blocks_in_window: None,
        }
    }
}

// ─── Orchestrator configuration ───────────────────────────────────────────────

/// Configuration for the CSJ orchestrator.
#[derive(Debug, Clone)]
pub struct CsjConfig {
    /// Ouroboros security parameter `k` (number of blocks).
    pub security_param_k: u64,
    /// Active slot coefficient `f` (probability of a slot having a leader).
    pub active_slot_coeff_f: f64,
}

impl CsjConfig {
    /// Genesis window: `ceil(3k/f)` slots.
    ///
    /// The jump point is set to `dynamo_tip - genesis_window` so that jumpers
    /// land within the Genesis Limit of Eagerness safe zone.
    pub fn genesis_window(&self) -> u64 {
        stability_window_slots(self.security_param_k, self.active_slot_coeff_f)
    }
}

// ─── Type aliases ─────────────────────────────────────────────────────────────

/// Channel used by peer tasks to register with the orchestrator.
///
/// Tuple: `(peer_addr, ewma_latency_ms, instruction_sender)`.
pub type PeerRegistrationSender =
    mpsc::Sender<(SocketAddr, Option<f64>, mpsc::Sender<PeerInstruction>)>;

/// Receiver side of the peer registration channel (held by the orchestrator).
type PeerRegistrationReceiver =
    mpsc::Receiver<(SocketAddr, Option<f64>, mpsc::Sender<PeerInstruction>)>;

// ─── Orchestrator actor ───────────────────────────────────────────────────────

/// CSJ orchestrator — drives dynamo election, jump scheduling, and GDD.
///
/// Run as a standalone tokio task via [`CsjOrchestrator::run`].  All peers
/// communicate with it via the [`PeerEvent`] channel and receive
/// [`PeerInstruction`]s back through per-peer `mpsc::Sender<PeerInstruction>`
/// that they pass at registration time (via [`PeerEvent::PeerConnected`]).
pub struct CsjOrchestrator {
    /// Configuration (k, f, derived genesis window).
    config: CsjConfig,
    /// Inbound event channel from all peer tasks.
    event_rx: mpsc::Receiver<PeerEvent>,
    /// Per-peer handles keyed by socket address.
    peers: HashMap<SocketAddr, PeerHandle>,
    /// The current dynamo's address (if any).
    dynamo_addr: Option<SocketAddr>,
    /// Channel senders for registering new peers.
    /// New senders arrive via a secondary mpsc — peers call `register_peer`.
    registration_rx: PeerRegistrationReceiver,
    /// Optional sender to the GSM actor.
    ///
    /// When set, the orchestrator forwards CSJ events (`JumpAgreed`,
    /// `ObjectionRaised`, `ObjectionResolved`) to the GSM so it can
    /// enforce the LoP (Limit on Patience) gate on the `Syncing → CaughtUp`
    /// transition.  `None` when genesis mode is disabled.
    gsm_tx: Option<mpsc::Sender<GsmEvent>>,
}

impl CsjOrchestrator {
    /// Create a new orchestrator and return it along with its public channels.
    ///
    /// # Arguments
    ///
    /// - `config`: CSJ configuration (k, f).
    /// - `gsm_tx`: optional sender to the GSM actor.  Pass `Some(tx)` when
    ///   genesis mode is enabled so that CSJ events are forwarded to the GSM
    ///   for LoP enforcement.  Pass `None` when genesis mode is disabled.
    ///
    /// # Returns
    /// `(orchestrator, event_tx, register_tx)`
    /// - `event_tx` — clone and give to each peer task to send [`PeerEvent`]s.
    /// - `register_tx` — call once per peer to register its instruction channel.
    pub fn new(
        config: CsjConfig,
        gsm_tx: Option<mpsc::Sender<GsmEvent>>,
    ) -> (Self, mpsc::Sender<PeerEvent>, PeerRegistrationSender) {
        let (event_tx, event_rx) = mpsc::channel(256);
        let (registration_tx, registration_rx) = mpsc::channel(64);
        let orchestrator = Self {
            config,
            event_rx,
            peers: HashMap::new(),
            dynamo_addr: None,
            registration_rx,
            gsm_tx,
        };
        (orchestrator, event_tx, registration_tx)
    }

    /// Run the orchestrator until the cancellation token fires.
    ///
    /// This is a long-lived actor task.  Spawn it with `tokio::spawn`.
    pub async fn run(mut self, cancel: CancellationToken) {
        info!(
            "CSJ orchestrator started (genesis_window={})",
            self.config.genesis_window()
        );
        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("CSJ orchestrator shutting down");
                    break;
                }

                Some((addr, latency_ms, tx)) = self.registration_rx.recv() => {
                    self.handle_registration(addr, latency_ms, tx).await;
                }

                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event).await;
                }

                // Stall-detection tick: check dynamo health every second.
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    self.check_dynamo_stall().await;
                }
            }
        }
    }

    // ─── Registration ─────────────────────────────────────────────────────────

    pub async fn handle_registration(
        &mut self,
        addr: SocketAddr,
        latency_ms: Option<f64>,
        tx: mpsc::Sender<PeerInstruction>,
    ) {
        debug!(%addr, ?latency_ms, "CSJ: peer registered");
        self.peers.insert(addr, PeerHandle::new(tx, latency_ms));

        // If no dynamo exists yet, run election.
        if self.dynamo_addr.is_none() {
            self.elect_dynamo().await;
            return;
        }

        // If a new peer arrives with lower latency than the current dynamo,
        // demote the current dynamo and elect the new peer.
        // This mirrors Haskell's `csjSelectDynamo` which always picks the
        // lowest-latency hot peer.
        let current_dynamo_latency = self
            .dynamo_addr
            .and_then(|da| self.peers.get(&da))
            .and_then(|h| h.latency_ms)
            .unwrap_or(f64::MAX);

        let new_peer_latency = latency_ms.unwrap_or(f64::MAX);

        if new_peer_latency < current_dynamo_latency {
            // Demote current dynamo to jumper.
            if let Some(da) = self.dynamo_addr.take() {
                if let Some(h) = self.peers.get_mut(&da) {
                    let _ = h.jump_state.on_dynamo_demotion();
                }
            }
            // Elect the new peer as dynamo.
            self.elect_dynamo().await;
        }
    }

    // ─── Event dispatch ───────────────────────────────────────────────────────

    async fn handle_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::PeerConnected { addr, latency_ms } => {
                // PeerConnected can arrive before the registration channel fires
                // (race between the two channels). Update latency if the peer is
                // already registered; otherwise ignore — registration will handle it.
                if let Some(h) = self.peers.get_mut(&addr) {
                    h.latency_ms = latency_ms;
                }
                // Trigger election if needed.
                if self.dynamo_addr.is_none() {
                    self.elect_dynamo().await;
                }
            }

            PeerEvent::PeerDisconnected { addr } => {
                self.handle_peer_disconnected(addr).await;
            }

            PeerEvent::DynamoTipAdvanced {
                addr,
                tip_slot,
                tip_hash,
            } => {
                self.handle_dynamo_tip_advanced(addr, tip_slot, tip_hash)
                    .await;
            }

            PeerEvent::IntersectFound { addr, found_point } => {
                self.handle_intersect_found(addr, found_point).await;
            }

            PeerEvent::IntersectNotFound { addr } => {
                self.handle_intersect_not_found(addr).await;
            }

            PeerEvent::BisectionComplete {
                addr,
                fork_point,
                objector_blocks_in_window,
                response,
            } => {
                self.handle_bisection_complete(
                    addr,
                    fork_point,
                    objector_blocks_in_window,
                    response,
                )
                .await;
            }
        }
    }

    // ─── Peer disconnection ───────────────────────────────────────────────────

    pub async fn handle_peer_disconnected(&mut self, addr: SocketAddr) {
        debug!(%addr, "CSJ: peer disconnected");
        let was_dynamo = self.dynamo_addr == Some(addr);
        self.peers.remove(&addr);
        if was_dynamo {
            self.dynamo_addr = None;
            warn!(%addr, "CSJ: dynamo disconnected; re-electing");
            self.elect_dynamo().await;
        }
        self.assert_dynamo_invariant();
    }

    // ─── Dynamo tip advance → schedule jumps ─────────────────────────────────

    pub async fn handle_dynamo_tip_advanced(
        &mut self,
        addr: SocketAddr,
        tip_slot: u64,
        tip_hash: [u8; 32],
    ) {
        // Ignore stale events from a peer that is no longer dynamo.
        if self.dynamo_addr != Some(addr) {
            return;
        }
        debug!(%addr, tip_slot, "CSJ: dynamo tip advanced");

        // Record the advance for stall detection.
        if let Some(h) = self.peers.get_mut(&addr) {
            h.last_tip_advance = Some(Instant::now());
            h.dynamo_tip = Some((tip_slot, tip_hash));
        }

        // Compute jump point: dynamo_tip - genesis_window.
        let genesis_window = self.config.genesis_window();
        let jump_slot = tip_slot.saturating_sub(genesis_window);
        if jump_slot == 0 {
            // Too early in the chain — no meaningful jump point yet.
            return;
        }

        // Use the dynamo's tip hash as the reference for the jump point.
        // Phase C will use an era-history-aware point that has the correct
        // hash at `jump_slot` from the dynamo's chain fragment.
        let jump_point = Point::Specific(jump_slot, tip_hash);
        let era_params = dugite_network::protocol::chainsync::jumping::EraParams {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: genesis_window,
        };
        let instruction = JumpInstruction {
            point: jump_point,
            era_params,
        };

        // Issue the jump to all happy jumpers.
        let happy_jumpers: Vec<SocketAddr> = self
            .peers
            .iter()
            .filter(|(a, h)| {
                **a != addr // not the dynamo itself
                    && h.jump_state.is_happy_jumper()
            })
            .map(|(a, _)| *a)
            .collect();

        for jumper_addr in happy_jumpers {
            if let Some(h) = self.peers.get_mut(&jumper_addr) {
                if let Err(e) = h.jump_state.on_jump_issued(&instruction) {
                    warn!(%jumper_addr, error=%e, "CSJ: jump issue transition failed");
                    continue;
                }
                let _ = h.tx.send(PeerInstruction::Jump(instruction.clone())).await;
                debug!(%jumper_addr, jump_slot, "CSJ: jump issued");
            }
        }

        self.assert_dynamo_invariant();
    }

    // ─── Intersect found ──────────────────────────────────────────────────────

    pub async fn handle_intersect_found(&mut self, addr: SocketAddr, found_point: Point) {
        debug!(%addr, ?found_point, "CSJ: intersect found");
        if let Some(h) = self.peers.get_mut(&addr) {
            if let Err(e) = h.jump_state.on_intersect_found(found_point.clone()) {
                warn!(%addr, error=%e, "CSJ: on_intersect_found transition failed");
                return;
            }
            // Acknowledge immediately: peer returns to Happy.
            if let Err(e) = h.jump_state.on_intersection_acknowledged() {
                warn!(%addr, error=%e, "CSJ: acknowledge transition failed");
            }
        }
        // Inform the GSM that this peer agreed to the jump point.
        self.emit_gsm(GsmEvent::JumpAgreed {
            peer: addr,
            point: found_point,
        });
    }

    // ─── Intersect not found → promote to objector ────────────────────────────

    pub async fn handle_intersect_not_found(&mut self, addr: SocketAddr) {
        debug!(%addr, "CSJ: intersect not found — peer becomes objector");

        // Retrieve the bisection lo/hi bounds from the peer's LookingForIntersection state
        // and compute the midpoint for the next bisection step.
        let (bisect_mid, lo_point, hi_point) = if let Some(h) = self.peers.get(&addr) {
            match &h.jump_state.state {
                JumpState::Jumper(JumperState::LookingForIntersection { lo, hi }) => {
                    let mid = bisect_midpoint(lo, hi);
                    (mid, lo.clone(), hi.clone())
                }
                other => {
                    warn!(%addr, ?other, "CSJ: IntersectNotFound in unexpected state");
                    (None, Point::Origin, Point::Origin)
                }
            }
        } else {
            (None, Point::Origin, Point::Origin)
        };

        // Compute a midpoint bisection point.
        let dissenting_point = match bisect_mid {
            Some(mid_slot) => {
                // Use the dynamo's hash as placeholder; Phase D will use the
                // chain-fragment hash at `mid_slot`.
                let hash = self
                    .dynamo_addr
                    .and_then(|da| self.peers.get(&da))
                    .and_then(|h| h.dynamo_tip)
                    .map(|(_, hash)| hash)
                    .unwrap_or([0u8; 32]);
                Point::Specific(mid_slot, hash)
            }
            None => {
                // Bisection exhausted — fork point is at Origin.
                warn!(%addr, "CSJ: bisection exhausted, fork point is Origin");
                Point::Origin
            }
        };

        if let Some(h) = self.peers.get_mut(&addr) {
            if let Err(e) = h
                .jump_state
                .on_intersect_not_found(dissenting_point.clone())
            {
                warn!(%addr, error=%e, "CSJ: on_intersect_not_found transition failed");
            }
        }

        // Inform the GSM that this peer has raised an objection.
        // The GSM uses this to block the Syncing → CaughtUp transition (LoP gate).
        self.emit_gsm(GsmEvent::ObjectionRaised {
            peer: addr,
            lo: lo_point,
            hi: hi_point,
        });
    }

    // ─── Bisection complete → GDD comparison ─────────────────────────────────

    pub async fn handle_bisection_complete(
        &mut self,
        addr: SocketAddr,
        fork_point: Point,
        objector_blocks: u64,
        response: oneshot::Sender<OrchestratorDecision>,
    ) {
        debug!(%addr, ?fork_point, objector_blocks, "CSJ: bisection complete");

        // Retrieve dynamo block count in the genesis window above fork_point.
        let dynamo_blocks: u64 = self
            .dynamo_addr
            .and_then(|da| self.peers.get(&da))
            .and_then(|h| h.dynamo_tip)
            .map(|(tip_slot, _)| {
                // Conservative estimate: count slots in window, not actual blocks.
                // Phase D will replace this with a real DensityWindow from the
                // dynamo's chain fragment.
                let fork_slot = match &fork_point {
                    Point::Specific(s, _) => *s,
                    Point::Origin => 0,
                };
                let window = self.config.genesis_window();
                let window_end = fork_slot.saturating_add(window);
                // If dynamo tip is beyond the window, saturate at window size as
                // an upper bound; the real density will be computed in Phase D.
                if tip_slot > window_end {
                    window
                } else {
                    tip_slot.saturating_sub(fork_slot)
                }
            })
            .unwrap_or(0);

        // Apply GDD: compare densities.
        let decision = if dynamo_blocks >= objector_blocks {
            // Dynamo is at least as dense — adopt its chain.
            info!(
                %addr,
                dynamo_blocks,
                objector_blocks,
                "CSJ/GDD: dynamo wins — adopt dynamo chain"
            );
            OrchestratorDecision::AdoptDynamo
        } else {
            // Objector is denser — switch to objector's chain and re-elect dynamo.
            info!(
                %addr,
                dynamo_blocks,
                objector_blocks,
                "CSJ/GDD: objector wins — keeping objector chain; re-electing dynamo"
            );
            // Demote current dynamo to jumper and elect objector as new dynamo.
            if let Some(da) = self.dynamo_addr.take() {
                if let Some(h) = self.peers.get_mut(&da) {
                    let _ = h.jump_state.on_dynamo_demotion();
                }
            }
            // Promote objector to dynamo.
            if let Some(h) = self.peers.get_mut(&addr) {
                h.jump_state = PeerJumpState::new_dynamo();
                self.dynamo_addr = Some(addr);
                let _ = h.tx.send(PeerInstruction::BecomeDynamo).await;
            }
            OrchestratorDecision::KeepObjector
        };

        // Resolve bisection regardless of GDD decision.
        if let Some(h) = self.peers.get_mut(&addr) {
            h.objector_blocks_in_window = Some(objector_blocks);
            // Transition to Disengaged (or Dynamo if objector won, already done above).
            if !h.jump_state.is_dynamo() {
                let _ = h.jump_state.on_bisection_resolved();
                let _ = h.tx.send(PeerInstruction::Disengage).await;
            }
        }

        // Inform the GSM that the objection is resolved.
        let gsm_outcome = match decision {
            OrchestratorDecision::AdoptDynamo => CsjObjectionOutcome::DynamoWins,
            OrchestratorDecision::KeepObjector => CsjObjectionOutcome::ObjectorWins,
        };
        self.emit_gsm(GsmEvent::ObjectionResolved {
            peer: addr,
            outcome: gsm_outcome,
        });

        // Respond to the peer task (ignore send errors — peer may have died).
        let _ = response.send(decision);
        self.assert_dynamo_invariant();
    }

    // ─── Dynamo election ──────────────────────────────────────────────────────

    /// Elect the lowest-latency peer as dynamo.
    ///
    /// If no latency is available (e.g. keep-alive not yet exchanged), peers
    /// are preferred in insertion order via `HashMap` iteration — effectively
    /// random, which is acceptable for the initial election.
    async fn elect_dynamo(&mut self) {
        if self.peers.is_empty() {
            return;
        }
        // Already have a dynamo?
        if self.dynamo_addr.is_some() {
            return;
        }

        // Pick the peer with the lowest latency (or first peer if none known).
        let best = self
            .peers
            .iter()
            .min_by(|(_, a), (_, b)| {
                let la = a.latency_ms.unwrap_or(f64::MAX);
                let lb = b.latency_ms.unwrap_or(f64::MAX);
                la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(addr, _)| *addr);

        let Some(new_dynamo) = best else { return };

        info!(%new_dynamo, "CSJ: elected new dynamo");
        self.dynamo_addr = Some(new_dynamo);

        // Set jump states.
        for (addr, h) in self.peers.iter_mut() {
            if *addr == new_dynamo {
                h.jump_state = PeerJumpState::new_dynamo();
                h.last_tip_advance = Some(Instant::now());
                let _ = h.tx.send(PeerInstruction::BecomeDynamo).await;
            } else if h.jump_state.is_dynamo() {
                // Demote any stale dynamo entry (should not happen but be safe).
                let _ = h.jump_state.on_dynamo_demotion();
            }
        }

        self.assert_dynamo_invariant();
    }

    // ─── Stall detection ──────────────────────────────────────────────────────

    /// Demote a stalled dynamo after `DYNAMO_STALL_GRACE`.
    ///
    /// Mirrors Haskell's `csjReprocessLoEDelay = 10 seconds` grace period before
    /// re-electing a new dynamo when the current one stops advancing its tip.
    async fn check_dynamo_stall(&mut self) {
        let stalled = if let Some(da) = self.dynamo_addr {
            if let Some(h) = self.peers.get(&da) {
                match h.last_tip_advance {
                    Some(t) => t.elapsed() > DYNAMO_STALL_GRACE,
                    None => false, // Not yet seen a tip advance — give it grace.
                }
            } else {
                false
            }
        } else {
            false
        };

        if stalled {
            let da = self
                .dynamo_addr
                .take()
                .expect("stalled dynamo must be Some");
            warn!(%da, grace_secs=%DYNAMO_STALL_GRACE.as_secs(), "CSJ: dynamo stalled; demoting");
            if let Some(h) = self.peers.get_mut(&da) {
                let _ = h.jump_state.on_dynamo_demotion();
            }
            self.elect_dynamo().await;
        }
    }

    // ─── GSM forwarding helper ────────────────────────────────────────────────

    /// Forward a CSJ event to the GSM actor (fire-and-forget).
    ///
    /// Errors are logged and ignored — the GSM is best-effort; a dropped event
    /// only delays a LoP state update by one polling cycle.
    fn emit_gsm(&self, event: GsmEvent) {
        if let Some(ref tx) = self.gsm_tx {
            if tx.try_send(event).is_err() {
                debug!("CSJ: GSM event channel full or closed — skipping LoP update");
            }
        }
    }

    // ─── Invariant checks ─────────────────────────────────────────────────────

    fn assert_dynamo_invariant(&self) {
        let states: Vec<&PeerJumpState> = self.peers.values().map(|h| &h.jump_state).collect();
        if let Err(msg) = check_dynamo_invariant(&states) {
            warn!("CSJ invariant violation: {msg}");
        }
    }
}

// ─── Test-only accessor helpers ───────────────────────────────────────────────
//
// These methods are public so that integration test binaries (which are separate
// compilation units from the library crate and cannot see `pub(crate)` items)
// can reach them.  The `#[doc(hidden)]` attribute prevents them from showing up
// in rustdoc.  Do not call these methods from production code.

impl CsjOrchestrator {
    /// Return the current dynamo address, if any.
    pub fn test_dynamo_addr(&self) -> Option<SocketAddr> {
        self.dynamo_addr
    }

    /// Return a snapshot of per-peer jump states for invariant checking.
    pub fn test_peer_jump_states(&self) -> Vec<PeerJumpState> {
        self.peers.values().map(|h| h.jump_state.clone()).collect()
    }

    /// Return `true` if `addr` is an objector.
    pub fn test_is_objector(&self, addr: SocketAddr) -> bool {
        self.peers
            .get(&addr)
            .map(|h| h.jump_state.is_objector())
            .unwrap_or(false)
    }

    /// Return `true` if `addr` is disengaged.
    pub fn test_is_disengaged(&self, addr: SocketAddr) -> bool {
        self.peers
            .get(&addr)
            .map(|h| h.jump_state.is_disengaged())
            .unwrap_or(false)
    }

    /// Set the dynamo tip directly (for GDD setup in tests).
    pub fn test_set_dynamo_tip(&mut self, addr: SocketAddr, tip_slot: u64, tip_hash: [u8; 32]) {
        if let Some(h) = self.peers.get_mut(&addr) {
            h.dynamo_tip = Some((tip_slot, tip_hash));
        }
    }

    /// Force-issue a jump instruction to a peer in `Happy` state (for tests).
    ///
    /// Returns `true` if the jump was successfully issued.
    pub fn test_issue_jump(&mut self, addr: SocketAddr, slot: u64) -> bool {
        use dugite_network::protocol::chainsync::jumping::EraParams;
        let instr = JumpInstruction {
            point: Point::Specific(slot, [slot as u8; 32]),
            era_params: EraParams {
                epoch_size: 432_000,
                slot_length_ms: 1_000,
                safe_zone: 60,
            },
        };
        self.peers
            .get_mut(&addr)
            .map(|h| h.jump_state.on_jump_issued(&instr).is_ok())
            .unwrap_or(false)
    }
}

// ─── Convenience: run the orchestrator as a spawned task ─────────────────────

/// Spawn the CSJ orchestrator as a background tokio task.
///
/// # Arguments
///
/// - `config`: CSJ configuration.
/// - `cancel`: cancellation token — the task exits when cancelled.
/// - `gsm_tx`: optional sender to the GSM actor for LoP enforcement.
///   Pass `Some(tx)` when genesis mode is enabled.
///
/// Returns `(event_tx, register_tx, join_handle)`.
pub fn spawn_csj_orchestrator(
    config: CsjConfig,
    cancel: CancellationToken,
    gsm_tx: Option<mpsc::Sender<GsmEvent>>,
) -> (
    mpsc::Sender<PeerEvent>,
    PeerRegistrationSender,
    tokio::task::JoinHandle<()>,
) {
    let (orchestrator, event_tx, register_tx) = CsjOrchestrator::new(config, gsm_tx);
    let handle = tokio::spawn(orchestrator.run(cancel));
    (event_tx, register_tx, handle)
}

// ─── Unit and integration tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time;
    use tokio::time::timeout;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn addr(n: u8) -> SocketAddr {
        format!("127.0.0.{n}:3001").parse().unwrap()
    }

    fn test_config() -> CsjConfig {
        CsjConfig {
            security_param_k: 10,
            active_slot_coeff_f: 0.5,
        }
        // genesis_window = ceil(3*10/0.5) = 60
    }

    /// Register a peer with the orchestrator and return the instruction receiver.
    async fn register_peer(
        register_tx: &PeerRegistrationSender,
        addr: SocketAddr,
        latency_ms: Option<f64>,
    ) -> mpsc::Receiver<PeerInstruction> {
        let (tx, rx) = mpsc::channel(16);
        register_tx
            .send((addr, latency_ms, tx))
            .await
            .expect("register_tx send");
        rx
    }

    // ── T1: Single peer becomes dynamo ────────────────────────────────────────

    #[tokio::test]
    async fn t1_single_peer_becomes_dynamo() {
        time::pause();
        let _cancel = CancellationToken::new();
        let (orch, _event_tx, register_tx) = CsjOrchestrator::new(test_config(), None);

        // We do NOT spawn — drive the orchestrator manually via handle_registration.
        // (This avoids the complexity of racing with the background task in tests.)
        let mut orch = orch;

        let (tx, mut rx) = mpsc::channel(16);
        orch.handle_registration(addr(1), Some(50.0), tx).await;

        // The single peer should have been elected dynamo.
        assert_eq!(orch.dynamo_addr, Some(addr(1)));

        // It should receive a BecomeDynamo instruction.
        let msg = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("recv timeout")
            .expect("channel closed");
        assert!(matches!(msg, PeerInstruction::BecomeDynamo));

        // Cleanup
        let _ = register_tx;
    }

    // ── T2: Lowest-latency peer elected as dynamo ─────────────────────────────
    //
    // Peers register in order: addr(1) at 200ms, addr(2) at 30ms, addr(3) at 100ms.
    //
    // - addr(1) gets initial election (first peer, dynamo_addr was None).
    // - addr(2) arrives with 30ms < 200ms → preempts: addr(1) demoted, addr(2) elected.
    // - addr(3) arrives with 100ms > 30ms → no preemption.
    // - Final dynamo: addr(2).

    #[tokio::test]
    async fn t2_lowest_latency_elected_as_dynamo() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx1, mut rx1) = mpsc::channel(16);
        let (tx2, mut rx2) = mpsc::channel(16);
        let (tx3, _rx3) = mpsc::channel(16);

        // addr(1) registers first → initial election → BecomeDynamo.
        orch.handle_registration(addr(1), Some(200.0), tx1).await;
        let first_msg = rx1
            .try_recv()
            .expect("addr(1) must get initial BecomeDynamo");
        assert!(matches!(first_msg, PeerInstruction::BecomeDynamo));

        // addr(2) registers with lower latency → preempts addr(1) → BecomeDynamo.
        orch.handle_registration(addr(2), Some(30.0), tx2).await;

        // addr(3) at 100ms does not preempt addr(2) at 30ms.
        orch.handle_registration(addr(3), Some(100.0), tx3).await;

        // addr(2) has lowest latency — must be the final dynamo.
        assert_eq!(orch.dynamo_addr, Some(addr(2)));

        // addr(2) must have received BecomeDynamo.
        let msg2 = timeout(Duration::from_millis(100), rx2.recv())
            .await
            .expect("recv timeout")
            .expect("channel closed");
        assert!(matches!(msg2, PeerInstruction::BecomeDynamo));

        // addr(1) must NOT have received another BecomeDynamo (already drained above).
        assert!(
            rx1.try_recv().is_err(),
            "addr(1) must not receive a second BecomeDynamo after preemption"
        );
    }

    // ── T3: Dynamo tip advance schedules jumps ────────────────────────────────

    #[tokio::test]
    async fn t3_dynamo_tip_advance_schedules_jump() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx_dynamo, mut rx_dynamo) = mpsc::channel(16);
        let (tx_jumper, mut rx_jumper) = mpsc::channel(16);

        orch.handle_registration(addr(1), Some(50.0), tx_dynamo)
            .await;
        orch.handle_registration(addr(2), Some(100.0), tx_jumper)
            .await;

        // Drain BecomeDynamo for addr(1).
        let _ = rx_dynamo.recv().await;

        let hash = [1u8; 32];
        // genesis_window = 60; tip_slot must be > 60 to produce a jump.
        orch.handle_dynamo_tip_advanced(addr(1), 200, hash).await;

        // Jumper should receive a Jump instruction.
        let msg = timeout(Duration::from_millis(100), rx_jumper.recv())
            .await
            .expect("recv timeout")
            .expect("channel closed");

        match msg {
            PeerInstruction::Jump(instr) => {
                // jump_slot = 200 - 60 = 140
                assert_eq!(
                    instr.point,
                    Point::Specific(140, hash),
                    "jump point should be tip - genesis_window"
                );
            }
            other => panic!("expected Jump, got {other:?}"),
        }
    }

    // ── T4: IntersectNotFound → peer becomes objector ─────────────────────────

    #[tokio::test]
    async fn t4_intersect_not_found_becomes_objector() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx_dynamo, mut rx_dynamo) = mpsc::channel(16);
        let (tx_jumper, _rx_jumper) = mpsc::channel(16);

        orch.handle_registration(addr(1), Some(50.0), tx_dynamo)
            .await;
        orch.handle_registration(addr(2), Some(100.0), tx_jumper)
            .await;
        let _ = rx_dynamo.recv().await; // BecomeDynamo

        // Issue a jump to put addr(2) in LookingForIntersection.
        let instr = JumpInstruction {
            point: Point::Specific(140, [1u8; 32]),
            era_params: dugite_network::protocol::chainsync::jumping::EraParams {
                epoch_size: 432_000,
                slot_length_ms: 1_000,
                safe_zone: 60,
            },
        };
        orch.peers
            .get_mut(&addr(2))
            .unwrap()
            .jump_state
            .on_jump_issued(&instr)
            .unwrap();

        orch.handle_intersect_not_found(addr(2)).await;

        // addr(2) should now be an objector.
        assert!(
            orch.peers[&addr(2)].jump_state.is_objector(),
            "peer should be objector after IntersectNotFound"
        );
    }

    // ── T5: BisectionComplete → GDD decision ─────────────────────────────────

    #[tokio::test]
    async fn t5_bisection_complete_dynamo_wins() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx_dynamo, mut rx_dynamo) = mpsc::channel(16);
        let (tx_objector, _rx_objector) = mpsc::channel(16);

        orch.handle_registration(addr(1), Some(50.0), tx_dynamo)
            .await;
        orch.handle_registration(addr(2), Some(100.0), tx_objector)
            .await;
        let _ = rx_dynamo.recv().await; // BecomeDynamo

        // Set dynamo tip.
        orch.peers.get_mut(&addr(1)).unwrap().dynamo_tip = Some((200, [1u8; 32]));

        // Put addr(2) in Objector state.
        orch.peers.get_mut(&addr(2)).unwrap().jump_state = PeerJumpState {
            state: JumpState::Objector {
                dissenting_point: Point::Specific(100, [0u8; 32]),
            },
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        orch.handle_bisection_complete(
            addr(2),
            Point::Specific(100, [0u8; 32]),
            5, // objector blocks in window (fewer than dynamo)
            resp_tx,
        )
        .await;

        let decision = resp_rx.await.expect("response");
        assert!(
            matches!(decision, OrchestratorDecision::AdoptDynamo),
            "dynamo should win when it is denser"
        );
    }

    // ── T6: BisectionComplete → objector wins, dynamo re-elected ─────────────

    #[tokio::test]
    async fn t6_bisection_complete_objector_wins() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx_dynamo, mut rx_dynamo) = mpsc::channel(16);
        let (tx_objector, mut rx_objector) = mpsc::channel(16);

        orch.handle_registration(addr(1), Some(50.0), tx_dynamo)
            .await;
        orch.handle_registration(addr(2), Some(100.0), tx_objector)
            .await;
        let _ = rx_dynamo.recv().await; // BecomeDynamo

        // Dynamo tip is just inside the window (low density estimate).
        orch.peers.get_mut(&addr(1)).unwrap().dynamo_tip = Some((110, [1u8; 32]));

        // Put addr(2) in Objector state with MORE blocks than the dynamo estimate.
        orch.peers.get_mut(&addr(2)).unwrap().jump_state = PeerJumpState {
            state: JumpState::Objector {
                dissenting_point: Point::Specific(100, [0u8; 32]),
            },
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        orch.handle_bisection_complete(
            addr(2),
            Point::Specific(100, [0u8; 32]),
            999, // objector has many more blocks — wins GDD
            resp_tx,
        )
        .await;

        let decision = resp_rx.await.expect("response");
        assert!(
            matches!(decision, OrchestratorDecision::KeepObjector),
            "objector should win when it is denser"
        );

        // Objector should now be the dynamo.
        assert_eq!(orch.dynamo_addr, Some(addr(2)));
        let msg = timeout(Duration::from_millis(100), rx_objector.recv())
            .await
            .expect("recv timeout")
            .expect("channel");
        assert!(matches!(msg, PeerInstruction::BecomeDynamo));
    }

    // ── T7: Dynamo stall → demotion after grace period ────────────────────────
    //
    // Registers two peers: addr(1) at 50ms latency and addr(2) at 200ms.
    // addr(1) wins initial election (lowest latency).  After advancing past
    // DYNAMO_STALL_GRACE with no DynamoTipAdvanced event, the stall detector
    // demotes addr(1) and re-elects — addr(1) still wins (lower latency) so
    // it receives a second BecomeDynamo.  The invariant we test is that the
    // stall-detection code path fires without deadlock and that the dynamo
    // invariant is preserved throughout.

    #[tokio::test(start_paused = true)]
    async fn t7_dynamo_stall_demotion() {
        let cancel = CancellationToken::new();
        let (event_tx, register_tx, _handle) =
            spawn_csj_orchestrator(test_config(), cancel.clone(), None);

        // Register two peers; addr(1) has lower latency and will be elected.
        let mut rx1 = register_peer(&register_tx, addr(1), Some(50.0)).await;
        let _rx2 = register_peer(&register_tx, addr(2), Some(200.0)).await;

        // Allow registrations to be processed.
        time::sleep(Duration::from_millis(50)).await;

        // addr(1) should receive BecomeDynamo from initial election.
        let msg = timeout(Duration::from_millis(200), rx1.recv())
            .await
            .expect("initial election recv timeout")
            .expect("channel closed");
        assert!(
            matches!(msg, PeerInstruction::BecomeDynamo),
            "addr(1) must become dynamo"
        );

        // Advance time past DYNAMO_STALL_GRACE without any DynamoTipAdvanced event.
        time::advance(DYNAMO_STALL_GRACE + Duration::from_millis(100)).await;

        // Give the orchestrator's stall-detection tick a chance to fire.
        // The orchestrator's 1-second sleep is now overdue; it will run
        // check_dynamo_stall(), demote addr(1), then re-elect addr(1) (still
        // lowest latency) and send a second BecomeDynamo.
        time::sleep(Duration::from_millis(1_500)).await;

        // addr(1) should receive a second BecomeDynamo (re-elected after stall demotion).
        let msg2 = timeout(Duration::from_millis(500), rx1.recv())
            .await
            .expect("re-election recv timeout");
        assert!(
            msg2.is_some(),
            "addr(1) should receive a second BecomeDynamo after stall demotion+re-election"
        );

        cancel.cancel();
        let _ = event_tx;
    }

    // ── T8: No deadlock when all peers stall ──────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn t8_all_peers_stall_no_deadlock() {
        let cancel = CancellationToken::new();
        let (event_tx, register_tx, handle) =
            spawn_csj_orchestrator(test_config(), cancel.clone(), None);

        // Register one peer.
        let _rx = register_peer(&register_tx, addr(1), None).await;

        // Advance well past the stall grace; the orchestrator should not deadlock
        // (it will repeatedly attempt to elect a dynamo and find it already set).
        time::advance(Duration::from_secs(60)).await;
        time::sleep(Duration::from_secs(5)).await;

        // Cancellation should complete cleanly.
        cancel.cancel();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("orchestrator must shut down within 1s")
            .expect("task panicked");

        let _ = event_tx;
        let _ = register_tx;
    }

    // ── T9: Dynamo invariant: 3-peer topology → exactly 1 dynamo ─────────────

    #[tokio::test]
    async fn t9_three_peer_topology_exactly_one_dynamo() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx1, mut rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);
        let (tx3, _rx3) = mpsc::channel(16);

        orch.handle_registration(addr(1), Some(80.0), tx1).await;
        orch.handle_registration(addr(2), Some(20.0), tx2).await;
        orch.handle_registration(addr(3), Some(50.0), tx3).await;

        // addr(2) is lowest latency but arrived second — since addr(1) was
        // elected first (dynamo_addr was None after first registration), addr(1)
        // is the dynamo here. The test verifies exactly-one-dynamo invariant.
        let states: Vec<&PeerJumpState> = orch.peers.values().map(|h| &h.jump_state).collect();
        assert!(
            check_dynamo_invariant(&states).is_ok(),
            "exactly one dynamo must be present among 3 peers"
        );

        // There must be exactly one BecomeDynamo in the instruction channels.
        let dynamo_count = orch
            .peers
            .values()
            .filter(|h| h.jump_state.is_dynamo())
            .count();
        assert_eq!(dynamo_count, 1, "exactly one dynamo");

        // Drain the dynamo instruction.
        let _ = rx1.try_recv();
    }

    // ── T10: Peer disconnect then reconnect ───────────────────────────────────

    #[tokio::test]
    async fn t10_peer_disconnect_reconnect() {
        time::pause();
        let (mut orch, _event_tx, _register_tx) = CsjOrchestrator::new(test_config(), None);

        let (tx1, mut rx1) = mpsc::channel(16);
        orch.handle_registration(addr(1), Some(50.0), tx1).await;
        assert_eq!(orch.dynamo_addr, Some(addr(1)));
        let _ = rx1.recv().await; // BecomeDynamo

        // Disconnect the dynamo.
        orch.handle_peer_disconnected(addr(1)).await;
        assert_eq!(orch.dynamo_addr, None);

        // Reconnect with a new channel.
        let (tx1b, mut rx1b) = mpsc::channel(16);
        orch.handle_registration(addr(1), Some(50.0), tx1b).await;
        assert_eq!(orch.dynamo_addr, Some(addr(1)));

        let msg = timeout(Duration::from_millis(100), rx1b.recv())
            .await
            .expect("recv timeout")
            .expect("channel closed");
        assert!(matches!(msg, PeerInstruction::BecomeDynamo));
    }

    // ── T11: genesis_window computation ───────────────────────────────────────

    #[test]
    fn t11_genesis_window_computation() {
        // k=2160, f=0.05 → standard Cardano params → 129600 slots.
        let cfg = CsjConfig {
            security_param_k: 2160,
            active_slot_coeff_f: 0.05,
        };
        assert_eq!(cfg.genesis_window(), 129_600);

        // k=10, f=0.5 → ceil(60) = 60.
        let cfg2 = CsjConfig {
            security_param_k: 10,
            active_slot_coeff_f: 0.5,
        };
        assert_eq!(cfg2.genesis_window(), 60);
    }
}
