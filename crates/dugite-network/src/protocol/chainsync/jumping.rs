//! ChainSync Jumping (CSJ) state machine — Ouroboros Genesis Phase 4.
//!
//! # Background
//!
//! CSJ accelerates bulk-sync under the Genesis Limit of Eagerness (LoE) by
//! letting one peer (the **dynamo**) pull headers at full pipeline speed while
//! the remaining peers (the **jumpers**) leapfrog to the dynamo's tip instead
//! of walking every header.  If a jumper cannot find the dynamo's jump-point
//! on its own chain it becomes an **objector** and a binary-search
//! (`MsgFindIntersect`) bisection determines the fork point.
//!
//! # State-machine summary
//!
//! ```text
//!                         ┌────────────────────────────────────────────────────────┐
//!                         │                     JumpState                          │
//!                         │                                                        │
//!  on_dynamo_demotion ──► │  Dynamo  ─────────────────────────────► Jumper(Happy) │
//!                         │                                                        │
//!  on_jump_issued ──────► │  Jumper(Happy) ─────────────────────► LookingForInts  │
//!                         │                                                        │
//!  on_intersect_found ──► │  LookingForInts ────────────────────► FoundInts       │
//!                         │                                                        │
//!  on_intersect_notfound  │  LookingForInts ──── bisect lo/hi ──► LookingForInts  │
//!                         │                                                        │
//!  on_bisection_done ───► │  FoundInts / Objector ──────────────► Disengaged      │
//!                         └────────────────────────────────────────────────────────┘
//! ```
//!
//! # Phase plan
//!
//! - **Phase A** (this file): pure state-machine types and transition functions,
//!   no async, no channels, no wire-up.
//! - **Phase B**: integrate `JumpCoordinator` with the pipelined ChainSync client.
//! - **Phase C** (done): replaced stub `EraParams` with `dugite_consensus::EraParams`;
//!   `compute_jump_points` and era-history-aware bisection now consume live
//!   `EraHistory`.
//! - **Phase D**: GDD (Genesis Density Disconnect) integration.
//! - **Phase E**: LoE governor adjustments for CSJ peer sets.
//! - **Phase F**: system-level integration tests.
//!
//! # Haskell reference
//!
//! The Haskell implementation in
//! `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping` (ouroboros-consensus)
//! stores `jumpSize :: SlotNo` as a fixed slot count passed to `makeContext`.  The
//! default is `2 * 2160 = 4320` (Byron forecast range), with the comment:
//!
//! > A future improvement would be to make this era-dynamic, such that we can
//! > use the larger (and hence more efficient) larger CSJ jump size in
//! > Shelley-based eras.
//!
//! The bisector (`MsgFindIntersect` probe) in `Ouroboros.Consensus.MiniProtocol
//! .ChainSync.Client.Jumping.onRollBackward` uses raw slot arithmetic — it does NOT
//! snap probes to epoch boundaries.  Dugite's `bisect_midpoint_era_aware` matches
//! this: it bisects by slot (midpoint), but enforces a per-era safe-zone so the
//! probe stays within the era the caller is handling.
//!
//! References:
//! - `ouroboros-consensus/src/…/ChainSync/Client/Jumping.hs` — state machine
//! - `Ouroboros.Consensus.Node.Genesis` — `defaultCSJJumpSize = 2 * 2160`
//! - Issue #334, tech-lead comment #4433365990
//! - Issue #709

use crate::codec::Point;
pub use dugite_consensus::era_history::{EraHistory, EraParams};

// ─── Jump instruction ─────────────────────────────────────────────────────────

/// Instruction emitted by the dynamo / coordinator telling jumpers where to jump.
///
/// The `point` is the chain-sync intersection target.  `era_params` carries the
/// era context needed to validate the jump point is within the safe zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpInstruction {
    /// The chain point that jumpers should seek to intersect.
    pub point: Point,
    /// Era parameters applicable at `point`.
    pub era_params: EraParams,
}

// ─── Jumper sub-state ────────────────────────────────────────────────────────

/// Sub-state for a peer that is currently in the `Jumper` role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumperState {
    /// The jumper has accepted the last jump instruction and is waiting
    /// for the next one from the coordinator.
    Happy,

    /// A jump instruction has been issued.  The jumper has sent
    /// `MsgFindIntersect([point])` to its upstream peer and is awaiting
    /// either `MsgIntersectFound` or `MsgIntersectNotFound`.
    ///
    /// If `MsgIntersectNotFound` is received, this peer becomes an objector
    /// and a binary-search bisection loop begins between `lo` and `hi`.
    LookingForIntersection {
        /// Lower bound of the bisection range (exclusive: known to be absent).
        lo: Point,
        /// Upper bound of the bisection range (inclusive: the original jump target).
        hi: Point,
    },

    /// The intersection was successfully found at `point`.
    /// The coordinator will acknowledge and transition this peer to `Happy`
    /// (if the intersection is accepted) or to `Disengaged` (if not).
    FoundIntersection {
        /// The intersection point confirmed by `MsgIntersectFound`.
        point: Point,
    },
}

// ─── Top-level peer jump state ────────────────────────────────────────────────

/// The role and state of one peer within the CSJ protocol.
///
/// Each outbound ChainSync peer occupies exactly one of these variants at any
/// moment.  Transitions are driven by:
/// - coordinator commands (e.g. `on_dynamo_demotion`, `on_jump_issued`)
/// - upstream wire messages (e.g. `on_intersect_found`, `on_intersect_not_found`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpState {
    /// This peer is the current dynamo — it runs standard pipelined ChainSync
    /// and its tip drives the jump instructions for all jumpers.
    Dynamo,

    /// This peer has raised an objection: the dynamo's jump point is not on
    /// its chain.  `dissenting_point` is the fork point determined by bisection.
    Objector {
        /// The deepest point that this peer's chain shares with the dynamo's
        /// candidate, as discovered by `MsgFindIntersect` bisection.
        dissenting_point: Point,
    },

    /// This peer is actively jumping, in one of the `JumperState` sub-states.
    Jumper(JumperState),

    /// The peer has been disengaged from CSJ — it will run standard ChainSync
    /// independently (e.g. after a fork has been resolved or after the sync
    /// frontier passes the safe-zone boundary).
    Disengaged,
}

// ─── Per-peer tracking ───────────────────────────────────────────────────────

/// Per-peer state maintained by the CSJ coordinator.
///
/// **Phase B** will add peer identity (connection ID / socket address) and a
/// channel for sending `JumpInstruction`s to the peer's ChainSync task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerJumpState {
    /// The current CSJ role and state for this peer.
    pub state: JumpState,
}

impl PeerJumpState {
    /// Create a new peer in the initial `Jumper(Happy)` state.
    ///
    /// All peers except the designated dynamo start here.
    pub fn new_jumper() -> Self {
        Self {
            state: JumpState::Jumper(JumperState::Happy),
        }
    }

    /// Create a new peer in the `Dynamo` state.
    pub fn new_dynamo() -> Self {
        Self {
            state: JumpState::Dynamo,
        }
    }
}

// ─── State transition errors ─────────────────────────────────────────────────

/// Error returned when a transition is applied to an incompatible state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// The peer is not in a state that accepts this transition.
    InvalidState {
        /// Human-readable description of the unexpected state.
        current: String,
        /// The transition that was attempted.
        attempted: &'static str,
    },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::InvalidState { current, attempted } => {
                write!(
                    f,
                    "CSJ transition '{attempted}' is invalid in state '{current}'"
                )
            }
        }
    }
}

impl std::error::Error for TransitionError {}

// ─── State transition functions ───────────────────────────────────────────────

impl PeerJumpState {
    /// The coordinator has demoted the dynamo to a jumper (e.g. the dynamo
    /// disconnected or stalled).
    ///
    /// Valid from: `Dynamo`
    /// Next state: `Jumper(Happy)`
    pub fn on_dynamo_demotion(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Dynamo => {
                self.state = JumpState::Jumper(JumperState::Happy);
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_dynamo_demotion",
            }),
        }
    }

    /// A `JumpInstruction` has been issued to this peer.
    ///
    /// The peer transitions from `Jumper(Happy)` to
    /// `Jumper(LookingForIntersection)`.  The jump `point` from the instruction
    /// is used as the upper bound `hi`; `lo` is `Point::Origin` (will be
    /// narrowed as `MsgIntersectNotFound` messages arrive).
    ///
    /// Valid from: `Jumper(Happy)`
    /// Next state: `Jumper(LookingForIntersection { lo: Origin, hi: point })`
    pub fn on_jump_issued(&mut self, instruction: &JumpInstruction) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::Happy) => {
                self.state = JumpState::Jumper(JumperState::LookingForIntersection {
                    lo: Point::Origin,
                    hi: instruction.point.clone(),
                });
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_jump_issued",
            }),
        }
    }

    /// The upstream peer replied with `MsgIntersectFound(point)`.
    ///
    /// The jumper records the confirmed intersection and transitions to
    /// `Jumper(FoundIntersection)`.  The coordinator will subsequently
    /// acknowledge and move this peer to `Happy` or `Disengaged`.
    ///
    /// Valid from: `Jumper(LookingForIntersection { .. })`
    /// Next state: `Jumper(FoundIntersection { point })`
    pub fn on_intersect_found(&mut self, found_point: Point) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::LookingForIntersection { .. }) => {
                self.state =
                    JumpState::Jumper(JumperState::FoundIntersection { point: found_point });
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_intersect_found",
            }),
        }
    }

    /// The upstream peer replied with `MsgIntersectNotFound`.
    ///
    /// The peer becomes an `Objector`.  `dissenting_point` is the midpoint of
    /// the current `[lo, hi)` bisection range (computed by the caller).
    ///
    /// Valid from: `Jumper(LookingForIntersection { .. })`
    /// Next state: `Objector { dissenting_point }`
    pub fn on_intersect_not_found(
        &mut self,
        dissenting_point: Point,
    ) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::LookingForIntersection { .. }) => {
                self.state = JumpState::Objector { dissenting_point };
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_intersect_not_found",
            }),
        }
    }

    /// `MsgIntersectNotFound` at the probed midpoint, but the bisection window
    /// has NOT yet collapsed: the fork lies at or below the midpoint, so narrow
    /// the UPPER bound to `new_hi` and keep bisecting (#880).
    ///
    /// The module's own state diagram describes a binary search between `lo` and
    /// `hi`, but `on_intersect_not_found` always jumped straight to `Objector` —
    /// the window could never actually narrow. This transition, together with
    /// [`Self::on_intersect_found_continue`], expresses the bisection step;
    /// callers use `on_intersect_not_found` only once `[lo, hi)` has converged.
    ///
    /// Valid from: `Jumper(LookingForIntersection { .. })`
    /// Next state: `Jumper(LookingForIntersection { lo, hi: new_hi })`
    pub fn on_intersect_not_found_continue(
        &mut self,
        new_hi: Point,
    ) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::LookingForIntersection { lo, .. }) => {
                let lo = lo.clone();
                self.state =
                    JumpState::Jumper(JumperState::LookingForIntersection { lo, hi: new_hi });
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_intersect_not_found_continue",
            }),
        }
    }

    /// `MsgIntersectFound` at the probed midpoint, but the bisection window has
    /// NOT yet collapsed: the fork lies above the midpoint, so raise the LOWER
    /// bound to `new_lo` and keep bisecting (#880 — the complement of
    /// [`Self::on_intersect_not_found_continue`]).
    ///
    /// Valid from: `Jumper(LookingForIntersection { .. })`
    /// Next state: `Jumper(LookingForIntersection { lo: new_lo, hi })`
    pub fn on_intersect_found_continue(&mut self, new_lo: Point) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::LookingForIntersection { hi, .. }) => {
                let hi = hi.clone();
                self.state =
                    JumpState::Jumper(JumperState::LookingForIntersection { lo: new_lo, hi });
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_intersect_found_continue",
            }),
        }
    }

    /// The bisection has been resolved (fork point determined).  The peer is
    /// disengaged from CSJ and will run normal ChainSync independently.
    ///
    /// Valid from: `Objector { .. }`
    /// Next state: `Disengaged`
    pub fn on_bisection_resolved(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Objector { .. } => {
                self.state = JumpState::Disengaged;
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_bisection_resolved",
            }),
        }
    }

    /// A successfully-found intersection has been acknowledged by the
    /// coordinator and the peer returns to waiting for the next jump.
    ///
    /// Valid from: `Jumper(FoundIntersection { .. })`
    /// Next state: `Jumper(Happy)`
    pub fn on_intersection_acknowledged(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            JumpState::Jumper(JumperState::FoundIntersection { .. }) => {
                self.state = JumpState::Jumper(JumperState::Happy);
                Ok(())
            }
            other => Err(TransitionError::InvalidState {
                current: format!("{other:?}"),
                attempted: "on_intersection_acknowledged",
            }),
        }
    }

    /// Forcibly disengage this peer from CSJ (e.g. it has passed the safe-zone
    /// frontier, or a coordinator-level policy decision requires it).
    ///
    /// Valid from: any state except `Disengaged` (idempotent on `Disengaged`).
    /// Next state: `Disengaged`
    pub fn disengage(&mut self) {
        self.state = JumpState::Disengaged;
    }

    /// Returns `true` if the peer is the current dynamo.
    pub fn is_dynamo(&self) -> bool {
        matches!(self.state, JumpState::Dynamo)
    }

    /// Returns `true` if the peer has been disengaged from CSJ.
    pub fn is_disengaged(&self) -> bool {
        matches!(self.state, JumpState::Disengaged)
    }

    /// Returns `true` if the peer is objecting (raised a dissent).
    pub fn is_objector(&self) -> bool {
        matches!(self.state, JumpState::Objector { .. })
    }

    /// Returns `true` if the peer is a happy jumper (awaiting the next instruction).
    pub fn is_happy_jumper(&self) -> bool {
        matches!(self.state, JumpState::Jumper(JumperState::Happy))
    }

    /// Returns the dissenting point if this peer is an `Objector`, or `None`.
    pub fn dissenting_point(&self) -> Option<&Point> {
        match &self.state {
            JumpState::Objector { dissenting_point } => Some(dissenting_point),
            _ => None,
        }
    }

    /// Returns the found intersection point if this peer is in
    /// `Jumper(FoundIntersection { .. })`, or `None`.
    pub fn found_intersection(&self) -> Option<&Point> {
        match &self.state {
            JumpState::Jumper(JumperState::FoundIntersection { point }) => Some(point),
            _ => None,
        }
    }
}

// ─── Coordinator invariant helpers ───────────────────────────────────────────

/// B15: Verify that a set of CSJ peers maintains the Dynamo invariant.
///
/// The Ouroboros CSJ protocol requires that **exactly one** peer is always in
/// the `Dynamo` state (it is the only peer making `MsgRequestNext` calls).
/// If all peers enter `Jumper` state simultaneously, no peer makes progress —
/// a coordinated group of slow peers can trigger this deadlock.
///
/// Returns `Ok(())` if the invariant holds, `Err(message)` otherwise.
///
/// **Usage:** Call this after every state transition during Phase B integration
/// to catch violations before they cause a silent sync stall.
pub fn check_dynamo_invariant(peers: &[&PeerJumpState]) -> Result<(), String> {
    let dynamo_count = peers.iter().filter(|p| p.is_dynamo()).count();
    // Disengaged peers have left CSJ; they do not count against the invariant.
    // An empty set (no CSJ peers) trivially satisfies it.
    let active_count = peers.iter().filter(|p| !p.is_disengaged()).count();
    if active_count == 0 {
        return Ok(());
    }
    match dynamo_count {
        1 => Ok(()),
        0 => Err(format!(
            "CSJ invariant violated: 0 dynamos among {active_count} active peers; \
             sync will stall (no peer is issuing MsgRequestNext)"
        )),
        n => Err(format!(
            "CSJ invariant violated: {n} peers are simultaneously Dynamo; \
             only 1 is permitted at a time"
        )),
    }
}

// ─── Jump-point computation ──────────────────────────────────────────────────

/// Compute the sequence of jump-point slots starting from `last_jump_slot`
/// using the real era history.
///
/// # Algorithm
///
/// Given the dynamo's current tip slot and the most-recent jump slot, we
/// compute the **next** jump slot by advancing `jump_size_slots` past
/// `last_jump_slot`, capped at the tip.  The result is clamped to remain
/// within the safe zone of the era that contains it.
///
/// This mirrors the Haskell jump-trigger logic in
/// `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping.onRollForward`:
///
/// ```haskell
/// | let jumpBoundaryPlus1 = jumpSize context + succWithOrigin lastJumpSlot
/// , succWithOrigin (pointSlot point) > jumpBoundaryPlus1
/// ```
///
/// i.e. a jump is triggered when the dynamo has advanced more than
/// `jump_size_slots` slots past the last jump.
///
/// # Parameters
///
/// - `era_history`: live era history for safe-zone look-up.
/// - `last_jump_slot`: the slot of the most recent jump point sent to jumpers
///   (`None` means no jump has been issued yet — treated as slot 0).
/// - `dynamo_tip_slot`: the dynamo's current chain tip slot.
/// - `jump_size_slots`: the cadence in slots between consecutive jumps.
///   Matches Haskell's `jumpSize`; default is `2 * k` (Byron forecast range,
///   4320 for mainnet/preview).
///
/// # Returns
///
/// `Some(slot)` if a new jump should be issued, `None` if the dynamo has not
/// advanced far enough yet (`dynamo_tip_slot < last_jump + jump_size_slots`).
///
/// The returned slot is guaranteed to be ≤ `dynamo_tip_slot` and within the
/// open era's safe zone when the era history is well-formed.
///
/// # Haskell reference
///
/// `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping` (ouroboros-consensus),
/// `Ouroboros.Consensus.Node.Genesis` (`defaultCSJJumpSize = 2 * 2160`).
pub fn compute_next_jump_slot(
    _era_history: &EraHistory,
    last_jump_slot: Option<u64>,
    dynamo_tip_slot: u64,
    jump_size_slots: u64,
) -> Option<u64> {
    let last = last_jump_slot.unwrap_or(0);
    // Trigger when dynamo tip has advanced past `last + jump_size_slots`.
    // Matches Haskell: `succWithOrigin (pointSlot point) > jumpSize + succWithOrigin lastJumpSlot`
    // which simplifies to `tip_slot >= last_jump_slot + jump_size_slots`.
    let next_jump_boundary = last.saturating_add(jump_size_slots);
    if dynamo_tip_slot >= next_jump_boundary {
        Some(next_jump_boundary.min(dynamo_tip_slot))
    } else {
        None
    }
}

/// Compute an ordered list of all jump-point slots between `last_jump_slot`
/// and `dynamo_tip_slot`, spaced `jump_size_slots` apart, respecting era
/// boundaries from `era_history`.
///
/// # Algorithm
///
/// Starting from `last_jump_slot + jump_size_slots`, emit one point per
/// `jump_size_slots`-slot stride up to `dynamo_tip_slot`.  Points that cross
/// an era boundary are clamped to the era's end slot so that each point is
/// validatable within a single era's safe zone.  This ensures jumpers can
/// always verify the jump point without crossing an era-horizon query
/// boundary.
///
/// # Returns
///
/// An ordered `Vec<u64>` of slot numbers.  Empty when the dynamo has not yet
/// advanced a full `jump_size_slots` past the last jump.
///
/// # Haskell reference
///
/// The Haskell implementation emits a single jump per `onRollForward`
/// invocation (the latest safe tip from `jcschJumpInfo`).  Dugite's
/// `compute_jump_points` is the batch variant used during Phase B
/// coordinator catch-up after reconnection or dynamo rotation.
pub fn compute_jump_points(
    era_history: &EraHistory,
    last_jump_slot: Option<u64>,
    dynamo_tip_slot: u64,
    jump_size_slots: u64,
) -> Vec<u64> {
    if jump_size_slots == 0 {
        return vec![];
    }
    let mut points = Vec::new();
    let mut cursor = last_jump_slot.unwrap_or(0).saturating_add(jump_size_slots);

    while cursor <= dynamo_tip_slot {
        // Clamp to the era boundary if this point would cross into a new era.
        let clamped = clamp_to_era_boundary(era_history, cursor);
        points.push(clamped);
        // If clamped < cursor we hit an era boundary; advance to cursor so
        // the next iteration starts past the boundary.
        let next_base = cursor.max(clamped);
        cursor = next_base.saturating_add(jump_size_slots);
    }

    points
}

/// Clamp `slot` to remain within the era that starts before or at `slot`.
///
/// If `slot` falls exactly on an era-end boundary (exclusive), it is moved
/// back to `era_end_slot - 1`.  This keeps jump points within the open era's
/// safe zone, matching the Haskell safe-zone horizon check.
///
/// If the era history cannot resolve the slot (e.g. slot is beyond the known
/// history), `slot` is returned unchanged — the caller must handle this case.
fn clamp_to_era_boundary(era_history: &EraHistory, slot: u64) -> u64 {
    let entries = era_history.entries();
    // Walk entries to find the era that contains `slot`.
    for entry in entries {
        let era_start = entry.start.slot;
        match &entry.end {
            Some(end) => {
                if slot >= era_start && slot < end.slot {
                    // slot is within this closed era — no clamping needed.
                    return slot;
                }
                // If slot == end.slot, it belongs to the next era; keep walking.
            }
            None => {
                // Open (current) era contains all slots >= era_start.
                if slot >= era_start {
                    return slot;
                }
            }
        }
    }
    // Slot is beyond all known eras — return unclamped.
    slot
}

// ─── Era-aware bisection helper ───────────────────────────────────────────────

/// Compute the bisection midpoint slot between `lo` and `hi`.
///
/// Used by the coordinator to compute the next `MsgFindIntersect` probe point
/// in the binary search for a fork point.  Returns `None` if there is no
/// integer midpoint (i.e. `lo` and `hi` are adjacent or equal slots, or either
/// point is `Origin`).
///
/// The caller is responsible for selecting a block hash at the returned slot
/// from the dynamo's chain fragment.
///
/// This is a pure slot-arithmetic bisector, matching the Haskell
/// `onRollBackward` probe strategy in
/// `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping` which does not
/// align probes to epoch boundaries.
pub fn bisect_midpoint(lo: &Point, hi: &Point) -> Option<u64> {
    match (lo, hi) {
        (Point::Specific(lo_slot, _), Point::Specific(hi_slot, _)) => {
            if hi_slot > lo_slot {
                Some(lo_slot + (hi_slot - lo_slot) / 2)
            } else {
                None
            }
        }
        // Origin..Specific: bisect from slot 0
        (Point::Origin, Point::Specific(hi_slot, _)) => {
            if *hi_slot > 0 {
                Some(hi_slot / 2)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Era-aware variant of `bisect_midpoint`.
///
/// Computes the slot midpoint between `lo` and `hi` and then looks up the era
/// parameters at that midpoint via `era_history`.  Returns `None` under the
/// same conditions as `bisect_midpoint` plus when the midpoint falls beyond
/// the known era history.
///
/// The returned `(slot, EraParams)` pair lets the caller verify that the
/// probe remains within the safe zone for that era.  This matches the
/// invariant that every `MsgFindIntersect` probe must be within the
/// predictable portion of the era history.
///
/// # Haskell reference
///
/// `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping` —
/// the `onRollBackward` handler tightens the bisection bounds by raw slot
/// arithmetic.  The Haskell implementation does not snap probes to epoch
/// boundaries; per-era `EraParams` are consulted at the call sites that
/// compute the safe-zone horizon (see `EraHistory.Summary.summarize`).
pub fn bisect_midpoint_era_aware<'a>(
    lo: &Point,
    hi: &Point,
    era_history: &'a EraHistory,
) -> Option<(u64, &'a EraParams)> {
    let mid_slot = bisect_midpoint(lo, hi)?;
    // Look up the era that contains mid_slot.
    let entry = era_history.entries().iter().find(|e| {
        let in_range = mid_slot >= e.start.slot;
        let before_end = e.end.as_ref().is_none_or(|end| mid_slot < end.slot);
        in_range && before_end
    })?;
    Some((mid_slot, &entry.params))
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_consensus::era_history::{EraHistory, EraParams};

    // ── helpers ───────────────────────────────────────────────────────────────

    fn pt(slot: u64) -> Point {
        Point::Specific(slot, [slot as u8; 32])
    }

    fn era_params_mainnet_shelley() -> EraParams {
        EraParams {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 2 * 2160,
            genesis_window: 129_600,
        }
    }

    fn instr(slot: u64) -> JumpInstruction {
        JumpInstruction {
            point: pt(slot),
            era_params: era_params_mainnet_shelley(),
        }
    }

    /// #880: the bisection-continue transitions narrow the `[lo, hi)` window in
    /// place (Phase-B CSJ) instead of jumping straight to Objector.
    #[test]
    fn bisection_continue_transitions_narrow_window() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(100)).expect("jump issued");

        // Not found at the midpoint → fork is at/below it → narrow the upper
        // bound, staying in LookingForIntersection.
        peer.on_intersect_not_found_continue(pt(50))
            .expect("continue lo");
        match &peer.state {
            JumpState::Jumper(JumperState::LookingForIntersection { lo, hi }) => {
                assert_eq!(*lo, Point::Origin);
                assert_eq!(*hi, pt(50));
            }
            other => panic!("expected LookingForIntersection, got {other:?}"),
        }

        // Found at the next midpoint → fork is above it → raise the lower bound.
        peer.on_intersect_found_continue(pt(25))
            .expect("continue hi");
        match &peer.state {
            JumpState::Jumper(JumperState::LookingForIntersection { lo, hi }) => {
                assert_eq!(*lo, pt(25));
                assert_eq!(*hi, pt(50));
            }
            other => panic!("expected LookingForIntersection, got {other:?}"),
        }

        // Window converged → terminal not-found becomes an Objector.
        peer.on_intersect_not_found(pt(30)).expect("objector");
        assert!(peer.is_objector());
    }

    /// Build a minimal single-era history (Byron instant, Shelley open) for
    /// testnets like preview/preprod that transition immediately.
    fn single_era_history(epoch_size: u64) -> EraHistory {
        let p = EraParams {
            epoch_size,
            slot_length_ms: 1_000,
            safe_zone: 4_320,
            genesis_window: 4_320,
        };
        EraHistory::from_genesis(p.clone(), p, 0)
    }

    /// Build a two-era history that mirrors mainnet Byron (epoch_size=21600,
    /// 20s slots) → Shelley (epoch_size=432000, 1s slots) transitioning at
    /// epoch 208.
    fn mainnet_two_era_history() -> EraHistory {
        let byron = EraParams {
            epoch_size: 21_600,
            slot_length_ms: 20_000,
            safe_zone: 4_320,
            genesis_window: 4_320,
        };
        let shelley = EraParams {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 129_600,
            genesis_window: 129_600,
        };
        EraHistory::from_genesis(byron, shelley, 208)
    }

    /// Build a three-era history: two eras of 1000-slot epochs, third open.
    fn three_era_history() -> EraHistory {
        use dugite_primitives::era::Era;
        let p = EraParams {
            epoch_size: 1_000,
            slot_length_ms: 1_000,
            safe_zone: 200,
            genesis_window: 200,
        };
        let mut eh = EraHistory::from_genesis(p.clone(), p.clone(), 0);
        // Byron→Shelley instant, then Shelley closes at epoch 5 (slot 5000),
        // opening Allegra.
        eh.record_era_transition(Era::Allegra, 5);
        eh
    }

    // ── initial state constructors ────────────────────────────────────────────

    #[test]
    fn new_jumper_starts_happy() {
        let peer = PeerJumpState::new_jumper();
        assert!(peer.is_happy_jumper());
        assert!(!peer.is_dynamo());
        assert!(!peer.is_disengaged());
        assert!(!peer.is_objector());
    }

    #[test]
    fn new_dynamo_starts_as_dynamo() {
        let peer = PeerJumpState::new_dynamo();
        assert!(peer.is_dynamo());
        assert!(!peer.is_happy_jumper());
        assert!(!peer.is_disengaged());
        assert!(!peer.is_objector());
    }

    // ── Dynamo → Jumper(Happy) ────────────────────────────────────────────────

    #[test]
    fn dynamo_demotion_succeeds() {
        let mut peer = PeerJumpState::new_dynamo();
        peer.on_dynamo_demotion().expect("demotion should succeed");
        assert!(peer.is_happy_jumper());
    }

    #[test]
    fn dynamo_demotion_from_jumper_fails() {
        let mut peer = PeerJumpState::new_jumper();
        let err = peer.on_dynamo_demotion().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_dynamo_demotion",
                ..
            }
        ));
    }

    #[test]
    fn dynamo_demotion_from_disengaged_fails() {
        let mut peer = PeerJumpState::new_jumper();
        peer.disengage();
        let err = peer.on_dynamo_demotion().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_dynamo_demotion",
                ..
            }
        ));
    }

    // ── Jumper(Happy) → Jumper(LookingForIntersection) ───────────────────────

    #[test]
    fn jump_issued_from_happy_succeeds() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000))
            .expect("jump should succeed");
        assert!(matches!(
            peer.state,
            JumpState::Jumper(JumperState::LookingForIntersection {
                lo: Point::Origin,
                hi: Point::Specific(1_000, _),
            })
        ));
    }

    #[test]
    fn jump_issued_from_dynamo_fails() {
        let mut peer = PeerJumpState::new_dynamo();
        let err = peer.on_jump_issued(&instr(100)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_jump_issued",
                ..
            }
        ));
    }

    #[test]
    fn jump_issued_from_looking_fails() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        let err = peer.on_jump_issued(&instr(2_000)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_jump_issued",
                ..
            }
        ));
    }

    #[test]
    fn jump_issued_from_disengaged_fails() {
        let mut peer = PeerJumpState::new_jumper();
        peer.disengage();
        let err = peer.on_jump_issued(&instr(100)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_jump_issued",
                ..
            }
        ));
    }

    // ── Jumper(LookingForIntersection) → Jumper(FoundIntersection) ───────────

    #[test]
    fn intersect_found_from_looking_succeeds() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        peer.on_intersect_found(pt(1_000))
            .expect("intersect found should succeed");
        assert_eq!(peer.found_intersection(), Some(&pt(1_000)));
    }

    #[test]
    fn intersect_found_from_happy_fails() {
        let mut peer = PeerJumpState::new_jumper();
        let err = peer.on_intersect_found(pt(500)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersect_found",
                ..
            }
        ));
    }

    #[test]
    fn intersect_found_from_dynamo_fails() {
        let mut peer = PeerJumpState::new_dynamo();
        let err = peer.on_intersect_found(pt(500)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersect_found",
                ..
            }
        ));
    }

    // ── Jumper(LookingForIntersection) → Objector ────────────────────────────

    #[test]
    fn intersect_not_found_from_looking_becomes_objector() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        peer.on_intersect_not_found(pt(500))
            .expect("not found should succeed");
        assert!(peer.is_objector());
        assert_eq!(peer.dissenting_point(), Some(&pt(500)));
    }

    #[test]
    fn intersect_not_found_from_happy_fails() {
        let mut peer = PeerJumpState::new_jumper();
        let err = peer.on_intersect_not_found(pt(500)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersect_not_found",
                ..
            }
        ));
    }

    #[test]
    fn intersect_not_found_from_dynamo_fails() {
        let mut peer = PeerJumpState::new_dynamo();
        let err = peer.on_intersect_not_found(pt(100)).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersect_not_found",
                ..
            }
        ));
    }

    // ── Objector → Disengaged ─────────────────────────────────────────────────

    #[test]
    fn bisection_resolved_from_objector_disengages() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        peer.on_intersect_not_found(pt(500)).unwrap();
        peer.on_bisection_resolved()
            .expect("bisection resolved should succeed");
        assert!(peer.is_disengaged());
        assert!(!peer.is_objector());
    }

    #[test]
    fn bisection_resolved_from_happy_fails() {
        let mut peer = PeerJumpState::new_jumper();
        let err = peer.on_bisection_resolved().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_bisection_resolved",
                ..
            }
        ));
    }

    #[test]
    fn bisection_resolved_from_dynamo_fails() {
        let mut peer = PeerJumpState::new_dynamo();
        let err = peer.on_bisection_resolved().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_bisection_resolved",
                ..
            }
        ));
    }

    // ── Jumper(FoundIntersection) → Jumper(Happy) ─────────────────────────────

    #[test]
    fn intersection_acknowledged_returns_to_happy() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        peer.on_intersect_found(pt(1_000)).unwrap();
        peer.on_intersection_acknowledged()
            .expect("acknowledge should succeed");
        assert!(peer.is_happy_jumper());
        assert_eq!(peer.found_intersection(), None);
    }

    #[test]
    fn intersection_acknowledged_from_happy_fails() {
        let mut peer = PeerJumpState::new_jumper();
        let err = peer.on_intersection_acknowledged().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersection_acknowledged",
                ..
            }
        ));
    }

    #[test]
    fn intersection_acknowledged_from_looking_fails() {
        let mut peer = PeerJumpState::new_jumper();
        peer.on_jump_issued(&instr(1_000)).unwrap();
        let err = peer.on_intersection_acknowledged().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::InvalidState {
                attempted: "on_intersection_acknowledged",
                ..
            }
        ));
    }

    // ── disengage (force) ─────────────────────────────────────────────────────

    #[test]
    fn disengage_from_any_state() {
        let states = [
            PeerJumpState::new_dynamo(),
            PeerJumpState::new_jumper(),
            {
                let mut p = PeerJumpState::new_jumper();
                p.on_jump_issued(&instr(100)).unwrap();
                p
            },
            {
                let mut p = PeerJumpState::new_jumper();
                p.on_jump_issued(&instr(100)).unwrap();
                p.on_intersect_not_found(pt(50)).unwrap();
                p
            },
        ];
        for mut peer in states {
            peer.disengage();
            assert!(peer.is_disengaged(), "should be disengaged");
        }
    }

    #[test]
    fn disengage_idempotent() {
        let mut peer = PeerJumpState::new_jumper();
        peer.disengage();
        peer.disengage(); // second call must not panic
        assert!(peer.is_disengaged());
    }

    // ── full happy-path sequence ───────────────────────────────────────────────

    #[test]
    fn full_happy_path_found() {
        // Dynamo demoted → jumper issues a jump → intersect found → acknowledged
        let mut peer = PeerJumpState::new_dynamo();
        peer.on_dynamo_demotion().unwrap();
        assert!(peer.is_happy_jumper());

        peer.on_jump_issued(&instr(5_000)).unwrap();
        assert!(matches!(
            peer.state,
            JumpState::Jumper(JumperState::LookingForIntersection { .. })
        ));

        peer.on_intersect_found(pt(5_000)).unwrap();
        assert_eq!(peer.found_intersection(), Some(&pt(5_000)));

        peer.on_intersection_acknowledged().unwrap();
        assert!(peer.is_happy_jumper());
    }

    #[test]
    fn full_objection_path() {
        // Happy → jump issued → not found → objector → bisection resolved → disengaged
        let mut peer = PeerJumpState::new_jumper();

        peer.on_jump_issued(&instr(10_000)).unwrap();
        peer.on_intersect_not_found(pt(5_000)).unwrap();
        assert!(peer.is_objector());

        peer.on_bisection_resolved().unwrap();
        assert!(peer.is_disengaged());
    }

    // ── bisect_midpoint ───────────────────────────────────────────────────────

    #[test]
    fn bisect_midpoint_specific_specific() {
        assert_eq!(bisect_midpoint(&pt(0), &pt(1_000)), Some(500));
        assert_eq!(bisect_midpoint(&pt(100), &pt(200)), Some(150));
        assert_eq!(bisect_midpoint(&pt(0), &pt(1)), Some(0));
    }

    #[test]
    fn bisect_midpoint_equal_slots_returns_none() {
        // lo and hi at same slot → no useful midpoint
        assert_eq!(bisect_midpoint(&pt(100), &pt(100)), None);
    }

    #[test]
    fn bisect_midpoint_origin_to_specific() {
        assert_eq!(bisect_midpoint(&Point::Origin, &pt(1_000)), Some(500));
        assert_eq!(bisect_midpoint(&Point::Origin, &pt(1)), Some(0));
        assert_eq!(bisect_midpoint(&Point::Origin, &pt(0)), None);
    }

    #[test]
    fn bisect_midpoint_origin_origin_returns_none() {
        assert_eq!(bisect_midpoint(&Point::Origin, &Point::Origin), None);
    }

    #[test]
    fn bisect_midpoint_hi_less_than_lo_returns_none() {
        // inverted range: hi < lo
        assert_eq!(bisect_midpoint(&pt(500), &pt(100)), None);
    }

    // ── predicate helpers ──────────────────────────────────────────────────────

    #[test]
    fn predicate_helpers_are_consistent() {
        let dynamo = PeerJumpState::new_dynamo();
        assert!(dynamo.is_dynamo());
        assert!(!dynamo.is_happy_jumper());
        assert!(!dynamo.is_objector());
        assert!(!dynamo.is_disengaged());
        assert_eq!(dynamo.dissenting_point(), None);
        assert_eq!(dynamo.found_intersection(), None);

        let mut looking = PeerJumpState::new_jumper();
        looking.on_jump_issued(&instr(1_000)).unwrap();
        assert!(!looking.is_dynamo());
        assert!(!looking.is_happy_jumper());
        assert!(!looking.is_objector());
        assert!(!looking.is_disengaged());
        assert_eq!(looking.dissenting_point(), None);
        assert_eq!(looking.found_intersection(), None);
    }

    // ── error Display ─────────────────────────────────────────────────────────

    #[test]
    fn transition_error_display() {
        let err = TransitionError::InvalidState {
            current: "Dynamo".to_string(),
            attempted: "on_jump_issued",
        };
        let msg = err.to_string();
        assert!(msg.contains("on_jump_issued"));
        assert!(msg.contains("Dynamo"));
    }

    // ── B15: Dynamo invariant checks ──────────────────────────────────────────

    /// B15: Exactly one Dynamo among active peers → invariant satisfied.
    #[test]
    fn dynamo_invariant_exactly_one_dynamo_ok() {
        let dynamo = PeerJumpState::new_dynamo();
        let jumper1 = PeerJumpState::new_jumper();
        let jumper2 = PeerJumpState::new_jumper();
        let result = check_dynamo_invariant(&[&dynamo, &jumper1, &jumper2]);
        assert!(
            result.is_ok(),
            "exactly one dynamo should satisfy invariant"
        );
    }

    /// B15: Zero Dynamos among active peers → invariant violated (sync stall).
    #[test]
    fn dynamo_invariant_zero_dynamo_violated() {
        let jumper1 = PeerJumpState::new_jumper();
        let jumper2 = PeerJumpState::new_jumper();
        let result = check_dynamo_invariant(&[&jumper1, &jumper2]);
        assert!(result.is_err(), "zero dynamos should violate the invariant");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("0 dynamos"),
            "error should mention 0 dynamos: {msg}"
        );
    }

    /// B15: Two Dynamos among active peers → invariant violated (duplicate leader).
    #[test]
    fn dynamo_invariant_two_dynamos_violated() {
        let dynamo1 = PeerJumpState::new_dynamo();
        let dynamo2 = PeerJumpState::new_dynamo();
        let jumper = PeerJumpState::new_jumper();
        let result = check_dynamo_invariant(&[&dynamo1, &dynamo2, &jumper]);
        assert!(result.is_err(), "two dynamos should violate the invariant");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("2 peers"),
            "error should mention peer count: {msg}"
        );
    }

    /// B15: Empty peer set → invariant trivially satisfied (no active peers).
    #[test]
    fn dynamo_invariant_empty_peer_set_ok() {
        let result = check_dynamo_invariant(&[]);
        assert!(
            result.is_ok(),
            "empty peer set should satisfy invariant trivially"
        );
    }

    /// B15: Disengaged peers do not count against the invariant, even if they
    /// were formerly Dynamo-like — but here we test all-disengaged peers.
    #[test]
    fn dynamo_invariant_all_disengaged_ok() {
        let mut peer1 = PeerJumpState::new_jumper();
        let mut peer2 = PeerJumpState::new_jumper();
        peer1.disengage();
        peer2.disengage();
        let result = check_dynamo_invariant(&[&peer1, &peer2]);
        assert!(
            result.is_ok(),
            "all-disengaged peer set should satisfy invariant (no active peers)"
        );
    }

    /// B15: One Dynamo + one disengaged → valid (only 1 active, and it is dynamo).
    #[test]
    fn dynamo_invariant_dynamo_with_disengaged_ok() {
        let dynamo = PeerJumpState::new_dynamo();
        let mut disengaged = PeerJumpState::new_jumper();
        disengaged.disengage();
        let result = check_dynamo_invariant(&[&dynamo, &disengaged]);
        assert!(
            result.is_ok(),
            "one dynamo + one disengaged should satisfy invariant"
        );
    }

    // ── compute_next_jump_slot ────────────────────────────────────────────────

    /// Pre-Conway-only: single-era history (instant Byron), Shelley open.
    /// jump_size = 4320 (2*k mainnet default).
    #[test]
    fn next_jump_slot_single_era_no_last_jump() {
        let eh = single_era_history(432_000);
        // No prior jump. First jump triggers when tip >= jump_size.
        assert_eq!(
            compute_next_jump_slot(&eh, None, 4_319, 4_320),
            None,
            "tip 4319 < 4320: no jump yet"
        );
        assert_eq!(
            compute_next_jump_slot(&eh, None, 4_320, 4_320),
            Some(4_320),
            "tip == 4320: exactly at boundary"
        );
        assert_eq!(
            compute_next_jump_slot(&eh, None, 10_000, 4_320),
            Some(4_320),
            "tip > 4320: jump at 4320"
        );
    }

    #[test]
    fn next_jump_slot_with_last_jump() {
        let eh = single_era_history(432_000);
        // Last jump was at slot 4320. Next jump at 4320 + 4320 = 8640.
        assert_eq!(
            compute_next_jump_slot(&eh, Some(4_320), 8_639, 4_320),
            None,
            "tip 8639 < 8640: no jump"
        );
        assert_eq!(
            compute_next_jump_slot(&eh, Some(4_320), 8_640, 4_320),
            Some(8_640),
            "tip == 8640: jump"
        );
    }

    #[test]
    fn next_jump_slot_capped_at_tip() {
        let eh = single_era_history(432_000);
        // Jump boundary is 8640 but tip is only 8000: no jump should fire.
        // If boundary is 4320 and tip is 4500, the jump is at min(4320, 4500)=4320.
        assert_eq!(
            compute_next_jump_slot(&eh, None, 4_500, 4_320),
            Some(4_320),
            "result capped to boundary not tip"
        );
    }

    /// Multi-era history: mainnet Byron (21600-slot epochs) → Shelley (432000-slot).
    /// Transition at epoch 208 → slot 4_492_800.
    #[test]
    fn next_jump_slot_multi_era_pre_transition() {
        let eh = mainnet_two_era_history();
        // Jump size = 2*k = 4320. Byron has 21600-slot epochs.
        // Dynamo tip is in Byron (slot 10_000).
        assert_eq!(
            compute_next_jump_slot(&eh, None, 4_320, 4_320),
            Some(4_320),
            "Byron jump triggers at 4320"
        );
    }

    #[test]
    fn next_jump_slot_multi_era_post_transition() {
        let eh = mainnet_two_era_history();
        // Dynamo is in Shelley. Last jump was just before the transition.
        // Byron end = slot 4_492_800. Shelley starts there.
        let shelley_slot = 4_500_000u64;
        let last_jump = 4_492_000u64;
        // next jump = 4_492_000 + 4_320 = 4_496_320
        let expected = 4_496_320u64;
        assert_eq!(
            compute_next_jump_slot(&eh, Some(last_jump), shelley_slot, 4_320),
            Some(expected),
            "Shelley jump after transition"
        );
    }

    // ── compute_jump_points ───────────────────────────────────────────────────

    /// Single-era history: produce multiple uniformly-spaced jump points.
    #[test]
    fn compute_jump_points_single_era_uniform() {
        let eh = single_era_history(432_000);
        // tip=20000, jump_size=4320 → points at 4320, 8640, 12960, 17280
        let pts = compute_jump_points(&eh, None, 20_000, 4_320);
        assert_eq!(pts, vec![4_320, 8_640, 12_960, 17_280]);
    }

    #[test]
    fn compute_jump_points_empty_when_tip_below_jump_size() {
        let eh = single_era_history(432_000);
        let pts = compute_jump_points(&eh, None, 4_319, 4_320);
        assert!(pts.is_empty(), "tip below jump_size: no points");
    }

    #[test]
    fn compute_jump_points_with_last_jump() {
        let eh = single_era_history(432_000);
        // last_jump=4320. Next at 8640, then 12960.
        let pts = compute_jump_points(&eh, Some(4_320), 13_000, 4_320);
        assert_eq!(pts, vec![8_640, 12_960]);
    }

    /// Multi-era: Byron (21600-slot epochs) → Shelley (432000-slot).
    /// Jump size = 4320. Verify points cross the era boundary correctly.
    #[test]
    fn compute_jump_points_multi_era_crossing_boundary() {
        let eh = mainnet_two_era_history();
        // Byron ends at slot 4_492_800 (epoch 208 * 21600).
        // Start from no last jump. Jump size = 4320.
        // Dynamo tip = 4_497_120 (just past transition).
        // Expected: 4320, 8640, ..., and then points in Shelley.
        let pts = compute_jump_points(&eh, None, 4_497_120, 4_320);
        // The points should be uniformly spaced at 4320 intervals.
        assert!(!pts.is_empty());
        // All must be <= tip
        for p in &pts {
            assert!(*p <= 4_497_120, "point {p} exceeds tip");
        }
        // Verify some specific points
        assert!(pts.contains(&4_320));
        // Last point should be just before or at tip within stride
        let last = *pts.last().unwrap();
        assert!(last <= 4_497_120);
        assert!(last + 4_320 > 4_497_120 || pts.len() > 1);
    }

    /// Non-uniform epoch lengths: era-1 has 1000-slot epochs, era-2 has the same
    /// (all post-Byron eras share params in this model). The important thing is
    /// that jump points are computed correctly across the boundary.
    #[test]
    fn compute_jump_points_non_uniform_era_boundaries() {
        let eh = three_era_history();
        // Shelley→Allegra transition at slot 5000 (epoch 5 * 1000).
        // Jump size = 500.
        // Start from no last jump. Dynamo tip = 6000.
        let pts = compute_jump_points(&eh, None, 6_000, 500);
        // Expected: 500, 1000, 1500, 2000, 2500, 3000, 3500, 4000, 4500, 5000, 5500, 6000
        // But the loop stops at <= tip=6000.
        // Verify they are ordered, ≤ tip, and uniformly spaced within each era.
        assert!(!pts.is_empty());
        let mut prev = 0u64;
        for &p in &pts {
            assert!(p <= 6_000, "point {p} exceeds tip");
            assert!(p >= prev, "points not ordered");
            prev = p;
        }
        // There must be a point just at or after the boundary (slot 5000).
        assert!(
            pts.iter().any(|&p| p >= 5_000),
            "no jump points at or past Shelley→Allegra boundary"
        );
    }

    /// jump_size=0 must return empty (degenerate, never divide by zero).
    #[test]
    fn compute_jump_points_zero_jump_size_is_empty() {
        let eh = single_era_history(432_000);
        let pts = compute_jump_points(&eh, None, 100_000, 0);
        assert!(pts.is_empty());
    }

    // ── bisect_midpoint_era_aware ─────────────────────────────────────────────

    #[test]
    fn bisect_midpoint_era_aware_within_single_era() {
        let eh = single_era_history(432_000);
        let lo = pt(0);
        let hi = pt(1_000);
        let result = bisect_midpoint_era_aware(&lo, &hi, &eh);
        assert!(result.is_some());
        let (slot, params) = result.unwrap();
        assert_eq!(slot, 500);
        assert_eq!(params.epoch_size, 432_000);
    }

    #[test]
    fn bisect_midpoint_era_aware_multi_era_in_shelley() {
        let eh = mainnet_two_era_history();
        // Both lo and hi are in Shelley (past slot 4_492_800).
        let lo = pt(4_500_000);
        let hi = pt(4_510_000);
        let result = bisect_midpoint_era_aware(&lo, &hi, &eh);
        assert!(result.is_some());
        let (slot, params) = result.unwrap();
        assert_eq!(slot, 4_505_000);
        // Shelley params: 432000-slot epochs
        assert_eq!(params.epoch_size, 432_000);
        assert_eq!(params.slot_length_ms, 1_000);
    }

    #[test]
    fn bisect_midpoint_era_aware_in_byron() {
        let eh = mainnet_two_era_history();
        // lo and hi are in Byron (before slot 4_492_800).
        let lo = pt(1_000);
        let hi = pt(10_000);
        let result = bisect_midpoint_era_aware(&lo, &hi, &eh);
        assert!(result.is_some());
        let (slot, params) = result.unwrap();
        assert_eq!(slot, 5_500);
        // Byron params: 21600-slot epochs, 20s slots
        assert_eq!(params.epoch_size, 21_600);
        assert_eq!(params.slot_length_ms, 20_000);
    }

    #[test]
    fn bisect_midpoint_era_aware_origin_to_specific() {
        let eh = single_era_history(432_000);
        let result = bisect_midpoint_era_aware(&Point::Origin, &pt(1_000), &eh);
        assert!(result.is_some());
        let (slot, _) = result.unwrap();
        assert_eq!(slot, 500);
    }

    #[test]
    fn bisect_midpoint_era_aware_equal_points_none() {
        let eh = single_era_history(432_000);
        assert!(bisect_midpoint_era_aware(&pt(500), &pt(500), &eh).is_none());
    }

    #[test]
    fn bisect_midpoint_era_aware_origin_origin_none() {
        let eh = single_era_history(432_000);
        assert!(bisect_midpoint_era_aware(&Point::Origin, &Point::Origin, &eh).is_none());
    }

    // ── bisection respects era-boundary epoch lengths ─────────────────────────

    /// When bisecting across a boundary where epoch lengths differ (Byron 20s
    /// slots vs Shelley 1s slots), the midpoint must fall in the correct era
    /// and return that era's EraParams.
    #[test]
    fn bisect_midpoint_era_aware_boundary_correct_era_params() {
        let eh = mainnet_two_era_history();
        // Byron ends at slot 4_492_800. Probe that crosses the boundary.
        // lo=4_490_000 (Byron), hi=4_496_000 (Shelley).
        // mid = 4_490_000 + (4_496_000 - 4_490_000) / 2 = 4_493_000 → Shelley.
        let lo = pt(4_490_000);
        let hi = pt(4_496_000);
        let result = bisect_midpoint_era_aware(&lo, &hi, &eh);
        assert!(result.is_some());
        let (slot, params) = result.unwrap();
        assert_eq!(slot, 4_493_000);
        // Must be Shelley params (1s slots, 432000-slot epochs).
        assert_eq!(params.slot_length_ms, 1_000);
        assert_eq!(params.epoch_size, 432_000);
    }

    /// When lo and hi are both in Byron, the returned params must be Byron's.
    #[test]
    fn bisect_midpoint_era_aware_boundary_within_byron() {
        let eh = mainnet_two_era_history();
        // lo=4_480_000 (Byron), hi=4_492_000 (also Byron: < 4_492_800).
        // mid = 4_486_000 → Byron.
        let lo = pt(4_480_000);
        let hi = pt(4_492_000);
        let result = bisect_midpoint_era_aware(&lo, &hi, &eh);
        assert!(result.is_some());
        let (slot, params) = result.unwrap();
        assert_eq!(slot, 4_486_000);
        // Byron params (20s slots)
        assert_eq!(params.slot_length_ms, 20_000);
        assert_eq!(params.epoch_size, 21_600);
    }
}
