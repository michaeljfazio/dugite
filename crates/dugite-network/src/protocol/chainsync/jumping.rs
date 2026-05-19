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
//! - **Phase C**: replace stub `EraParams` with `dugite_consensus::EraParams` and
//!   add era-boundary jump-point selection.
//! - **Phase D**: GDD (Genesis Density Disconnect) integration.
//! - **Phase E**: LoE governor adjustments for CSJ peer sets.
//! - **Phase F**: system-level integration tests.
//!
//! # References
//!
//! - `ouroboros-consensus-diffusion/src/…/ChainSync/Jumping.hs` (Haskell reference)
//! - Ouroboros Genesis paper §6 "Chain-Sync Jumping"
//! - Issue #334, prior tech-lead investigation comment #4433365990

use crate::codec::Point;

// ─── Era parameters stub ─────────────────────────────────────────────────────
//
// TODO Phase C: remove this stub and use `dugite_consensus::EraParams` directly
// once dugite-network declares a dependency on dugite-consensus (or a shared
// primitives crate exports EraParams).
//
// The fields mirror `dugite_consensus::era_history::EraParams` exactly so that
// the migration is a search-and-replace of the import path.

/// Minimal era-parameter stub for jump-point computation.
///
/// **TODO Phase C**: replace with `dugite_consensus::EraParams`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EraParams {
    /// Number of slots per epoch in this era.
    pub epoch_size: u64,
    /// Nominal slot length in milliseconds.
    pub slot_length_ms: u64,
    /// Number of slots in the safe zone (2 * security parameter k for post-Byron).
    pub safe_zone: u64,
}

// ─── Jump instruction ─────────────────────────────────────────────────────────

/// Instruction emitted by the dynamo / coordinator telling jumpers where to jump.
///
/// The `point` is the chain-sync intersection target.  `era_params` carries the
/// era context needed to validate the jump point is within the safe zone.
///
/// **TODO Phase C**: `era_params` will be sourced from `EraHistory` once the
/// consensus crate integration is wired up.
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

// ─── Bisection helper ─────────────────────────────────────────────────────────

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
/// **TODO Phase C**: this will be replaced by an era-history-aware bisector
/// that picks jump-points aligned to safe-zone boundaries.
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

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn pt(slot: u64) -> Point {
        Point::Specific(slot, [slot as u8; 32])
    }

    fn era() -> EraParams {
        EraParams {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 2 * 2160,
        }
    }

    fn instr(slot: u64) -> JumpInstruction {
        JumpInstruction {
            point: pt(slot),
            era_params: era(),
        }
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
}
