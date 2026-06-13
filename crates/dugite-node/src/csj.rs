//! ChainSync Jumping — the Genesis bulk-sync header-bandwidth optimisation.
//!
//! Faithful port of `Ouroboros.Consensus.MiniProtocol.ChainSync.Client.Jumping`
//! (+ `State`) from ouroboros-consensus `release-ouroboros-consensus-3.0.1.0`.
//! One peer (the **dynamo**) streams headers; the others (**jumpers**) are
//! offered `MsgFindIntersect` jumps to the dynamo's tip every `jumpSize`
//! slots. A jumper that rejects a jump bisects to its divergence point and
//! becomes the (single) **objector**, streaming alongside the dynamo so the
//! GDD can adjudicate. At most two peers download headers concurrently.
//!
//! Invariants (Haskell module comment):
//! 1. exactly one dynamo among non-disengaged peers (when any exist);
//! 2. at most one objector;
//! 3. the objector has the OLDEST intersection among `FoundIntersection`
//!    jumpers (older intersections steal the role).
//!
//! Dynamo election order is the registration SEQUENCE: the first
//! non-disengaged peer in order (`ChainSyncClientHandleCollection`'s
//! `StrictSeq`); `rotateDynamo` moves the rotated peer to the back.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::genesis_peer_state::{CandidateFragment, WithOrigin};

/// Snapshot of the dynamo's candidate state carried by every jump
/// (Haskell `JumpInfo` — `jTheirFragment` is the candidate fragment; the
/// anchor stands in for `jMostRecentIntersection`).
#[derive(Debug, Clone)]
pub struct JumpInfo {
    pub fragment: CandidateFragment,
}

impl JumpInfo {
    /// The point a jumper probes with `MsgFindIntersect` — the fragment
    /// head (Haskell: `AF.headPoint $ jTheirFragment jumpInfo`).
    pub fn tip_point(&self) -> Option<(u64, [u8; 32])> {
        match self.fragment.head() {
            crate::genesis_peer_state::FragAnchor::Point(s, h) => Some((s, h)),
            crate::genesis_peer_state::FragAnchor::Origin => None,
        }
    }

    fn head_slot(&self) -> WithOrigin {
        self.fragment.head_slot()
    }
}

/// Haskell `ChainSyncJumpingJumperState`.
#[derive(Debug, Clone)]
pub enum JumperRole {
    /// Waiting for (or never offered) a jump. `fresh` = never jumped
    /// (`FreshJumper`); `last_good` = most recent accepted jump
    /// (`Happy StartedJumper (Just _)`).
    Happy {
        fresh: bool,
        last_good: Option<JumpInfo>,
    },
    /// Bisecting between the accepted `good` prefix and the rejected `bad`
    /// fragment (INVARIANT: good's tip is an ancestor of bad's fragment).
    LookingForIntersection { good: JumpInfo, bad: JumpInfo },
    /// Bisection converged; queued to become objector when the seat frees
    /// (`FoundIntersection`). `bad_point` = point of first disagreement.
    FoundIntersection {
        good: JumpInfo,
        bad_point: (u64, [u8; 32]),
    },
}

/// Haskell `ChainSyncJumpingState`.
#[derive(Debug, Clone)]
pub enum CsjRole {
    /// Streams headers; its tip drives jumps. `starting` carries the
    /// `DynamoStarting goodJumpInfo` promotion handshake (a
    /// `JumpToGoodPoint` must be offered before normal streaming).
    Dynamo {
        starting: Option<JumpInfo>,
        last_jump_slot: WithOrigin,
    },
    /// Streams headers alongside the dynamo from its dissent point so the
    /// GDD can compare densities. `starting` = must still offer its
    /// `JumpToGoodPoint`.
    Objector {
        starting: bool,
        good: JumpInfo,
        bad_point: (u64, [u8; 32]),
    },
    /// Plain ChainSync, out of CSJ forever. `restarting` = the next
    /// instruction is `Restart` (re-run FindIntersect — the peer's server
    /// cursor is unknown after a mid-jump demotion), then `DisengagedDone`.
    Disengaged { restarting: bool },
    /// Jump-driven peer (no header streaming).
    Jumper(JumperRole),
}

/// Instruction returned to a peer's ChainSync task
/// (Haskell `Instruction` via `nextInstruction`).
#[derive(Debug, Clone)]
pub enum CsjInstruction {
    /// Run the normal (pipelined) ChainSync loop.
    RunNormally,
    /// Offer `MsgFindIntersect [tip]`; on acceptance the candidate fragment
    /// is REPLACED by the jump fragment (`updateChainSyncState`).
    Jump(JumpInfo),
    /// Promotion handshake (`JumpToGoodPoint`): re-establish the server
    /// cursor at the good point, then stream.
    JumpToGoodPoint(JumpInfo),
    /// Drain and re-run the intersection phase (`Instruction.Restart`).
    Restart,
    /// No instruction pending — block until `notify` fires
    /// (jumpers waiting between jumps).
    Wait,
}

struct CsjPeer {
    role: CsjRole,
    /// Pending jump for a Happy jumper (Haskell `nextJumpVar`).
    next_jump: Option<JumpInfo>,
    /// Latest jump-info snapshot for this peer (Haskell `cschJumpInfo`) —
    /// the source for jumps when this peer is (or becomes) the dynamo.
    jump_info: Option<JumpInfo>,
    /// Wakes the peer's task when its role / pending jump changes.
    notify: Arc<tokio::sync::Notify>,
}

struct Inner {
    peers: HashMap<SocketAddr, CsjPeer>,
    /// Registration sequence — dynamo election order
    /// (`cschcSeq`; `rotateDynamo` moves to the back).
    order: VecDeque<SocketAddr>,
}

/// The CSJ coordinator (Haskell `Context` + handle collection).
pub struct CsjRegistry {
    inner: Mutex<Inner>,
    /// `csjcJumpSize` — slots between jumps (default 4320 = 2×2160).
    jump_size: u64,
    /// `CSJDisabled` ⇒ every hook is a no-op and every peer runs normally
    /// (Haskell `noJumping`).
    enabled: bool,
}

/// What a state change implies for OTHER peers' tasks (the caller wakes
/// them via the registry; the per-peer task reads its own instruction).
impl CsjRegistry {
    pub fn new(enabled: bool, jump_size: u64) -> Arc<Self> {
        Arc::new(CsjRegistry {
            inner: Mutex::new(Inner {
                peers: HashMap::new(),
                order: VecDeque::new(),
            }),
            jump_size,
            enabled,
        })
    }

    /// Whether CSJ is active at all.
    #[allow(dead_code)] // consulted by the genesis fetch-mode gate (future T10 PeersOrder)
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Head slot of a peer's current candidate (jump-info) fragment, if any.
    ///
    /// Used by the BlockFetch unproductive-dynamo watchdog (#760-A) to tell a
    /// GENUINELY-SILENT dynamo (no headers ahead of our selected chain — the
    /// #742 rotation target) from one that fed headers and is now legitimately
    /// PARKED on the forecast horizon (its fragment leads our chain by ~a
    /// stability window). Rotating the latter merely re-intersects a fresh
    /// dynamo at the same frontier and re-parks it, producing the ~1 blk/min
    /// cold-restart churn. Mirrors Haskell: a peer blocked at the forecast
    /// horizon is not starving us — the ledger is catching up.
    ///
    /// `None` when the peer is unknown or has never delivered a header
    /// (no `jump_info`, or its fragment is anchored at Origin).
    pub fn fragment_head_slot(&self, addr: &SocketAddr) -> Option<u64> {
        let inner = self.inner.lock().expect("csj lock");
        let peer = inner.peers.get(addr)?;
        match peer.jump_info.as_ref()?.fragment.head_slot() {
            WithOrigin::At(s) => Some(s),
            WithOrigin::Origin => None,
        }
    }

    /// `registerClient`: first peer (while not CaughtUp) becomes the
    /// dynamo; later peers become fresh jumpers pre-loaded with the
    /// dynamo's current jump info; peers connecting while CaughtUp are
    /// `Disengaged DisengagedDone` immediately. Disabled ⇒ `RunNormally`
    /// forever (`noJumping`).
    pub fn register(
        &self,
        addr: SocketAddr,
        gsm_caught_up: bool,
        candidate_anchor_slot: WithOrigin,
    ) -> Arc<tokio::sync::Notify> {
        let notify = Arc::new(tokio::sync::Notify::new());
        if !self.enabled {
            return notify;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let role = if gsm_caught_up {
            CsjRole::Disengaged { restarting: false }
        } else if !Self::has_dynamo(&inner) {
            CsjRole::Dynamo {
                starting: None,
                last_jump_slot: candidate_anchor_slot,
            }
        } else {
            CsjRole::Jumper(JumperRole::Happy {
                fresh: true,
                last_good: None,
            })
        };
        let next_jump = if matches!(role, CsjRole::Jumper(_)) {
            // Pre-load the dynamo's current jump info (Haskell engageClient
            // reads `cschJumpInfo handle` of the dynamo).
            Self::dynamo_of(&inner).and_then(|d| inner.peers[&d].jump_info.clone())
        } else {
            None
        };
        inner.order.retain(|a| a != &addr);
        inner.order.push_back(addr);
        inner.peers.insert(
            addr,
            CsjPeer {
                role,
                next_jump,
                jump_info: None,
                notify: notify.clone(),
            },
        );
        notify
    }

    /// `unregisterClient`: a departing dynamo triggers `backfillDynamo`, a
    /// departing objector `electNewObjector`.
    pub fn unregister(&self, addr: &SocketAddr) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.remove(addr) else {
            return;
        };
        inner.order.retain(|a| a != addr);
        match peer.role {
            CsjRole::Dynamo { .. } => Self::backfill_dynamo(&mut inner),
            CsjRole::Objector { .. } => Self::elect_new_objector(&mut inner),
            _ => {}
        }
    }

    /// Current instruction for `addr` (non-blocking; `Wait` = park on the
    /// notify handle). Consumes one-shot promotion instructions
    /// (Haskell `nextInstruction`'s state writes).
    pub fn next_instruction(&self, addr: &SocketAddr) -> CsjInstruction {
        if !self.enabled {
            return CsjInstruction::RunNormally;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return CsjInstruction::RunNormally;
        };
        match &mut peer.role {
            CsjRole::Disengaged { restarting } => {
                if *restarting {
                    *restarting = false; // Disengaging → DisengagedDone
                    CsjInstruction::Restart
                } else {
                    CsjInstruction::RunNormally
                }
            }
            CsjRole::Dynamo { starting, .. } => match starting.take() {
                Some(good) => CsjInstruction::JumpToGoodPoint(good),
                None => CsjInstruction::RunNormally,
            },
            CsjRole::Objector { starting, good, .. } => {
                if *starting {
                    *starting = false;
                    CsjInstruction::JumpToGoodPoint(good.clone())
                } else {
                    CsjInstruction::RunNormally
                }
            }
            CsjRole::Jumper(jr) => match peer.next_jump.take() {
                Some(ji) => {
                    if let JumperRole::Happy { fresh, .. } = jr {
                        *fresh = false; // FreshJumper → StartedJumper
                    }
                    CsjInstruction::Jump(ji)
                }
                None => CsjInstruction::Wait,
            },
        }
    }

    /// Dynamo `onRollForward` (called BEFORE header validation): when the
    /// incoming header's slot passes `lastJumpSlot + jumpSize`, broadcast
    /// the dynamo's current jump info to every Happy jumper.
    ///
    /// ```haskell
    /// | let jumpBoundaryPlus1 = jumpSize + succWithOrigin lastJumpSlot
    /// , succWithOrigin (pointSlot point) > jumpBoundaryPlus1 -> setJumps
    /// ```
    ///
    /// Objector `onRollForward`: if the dynamo's header point equals the
    /// objector's `badPoint`, the objector now AGREES with the chain it
    /// disputed — disengage it and elect a successor.
    pub fn on_roll_forward(&self, addr: &SocketAddr, point: (u64, [u8; 32])) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return;
        };
        match &peer.role {
            CsjRole::Objector { bad_point, .. } if *bad_point == point => {
                Self::disengage_peer(peer, false);
                Self::elect_new_objector(&mut inner);
            }
            CsjRole::Dynamo { last_jump_slot, .. } => {
                let boundary_plus_1 = self
                    .jump_size
                    .saturating_add(crate::genesis_governor::succ_with_origin(*last_jump_slot));
                let succ_point = point.0.saturating_add(1);
                if succ_point > boundary_plus_1 {
                    let Some(ji) = peer.jump_info.clone() else {
                        return;
                    };
                    // lastJumpSlot := head of the jump fragment.
                    if let CsjRole::Dynamo { last_jump_slot, .. } = &mut peer.role {
                        *last_jump_slot = ji.head_slot();
                    }
                    Self::set_jumps(&mut inner, ji);
                }
            }
            _ => {}
        }
    }

    /// Dynamo/objector rollback guards (`onRollBackward`):
    /// - dynamo rolling back strictly before `lastJumpSlot` is disengaged
    ///   (it rewound a point jumpers already confirmed) + backfill;
    /// - objector rolling back strictly before its `badPoint` is
    ///   disengaged + elect successor.
    pub fn on_roll_backward(&self, addr: &SocketAddr, slot: WithOrigin) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return;
        };
        match &peer.role {
            CsjRole::Dynamo { last_jump_slot, .. } if slot < *last_jump_slot => {
                Self::disengage_peer(peer, true); // server cursor moved: Restart
                Self::backfill_dynamo(&mut inner);
            }
            CsjRole::Objector { bad_point, .. } if slot < WithOrigin::At(bad_point.0) => {
                Self::disengage_peer(peer, true);
                Self::elect_new_objector(&mut inner);
            }
            _ => {}
        }
    }

    /// `onAwaitReply`: ANY role that claims it has no more headers leaves
    /// CSJ ("CSJ finishes when all peers have been disengaged").
    pub fn on_await_reply(&self, addr: &SocketAddr) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return;
        };
        let role = std::mem::replace(&mut peer.role, CsjRole::Disengaged { restarting: false });
        match role {
            CsjRole::Dynamo { .. } => {
                peer.notify.notify_one();
                Self::backfill_dynamo(&mut inner);
            }
            CsjRole::Objector { .. } => {
                peer.notify.notify_one();
                Self::elect_new_objector(&mut inner);
            }
            CsjRole::Jumper(_) => {
                peer.notify.notify_one();
            }
            CsjRole::Disengaged { .. } => {
                peer.role = role; // restore (already disengaged)
            }
        }
    }

    /// `updateJumpInfo` — called whenever the peer's candidate fragment
    /// advances (any non-disengaged role keeps its snapshot current).
    pub fn update_jump_info(&self, addr: &SocketAddr, fragment: CandidateFragment) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return;
        };
        if matches!(peer.role, CsjRole::Disengaged { .. }) {
            return;
        }
        peer.jump_info = Some(JumpInfo { fragment });
    }

    /// A jumper's `MsgFindIntersect` round-trip completed
    /// (`processJumpResult` for `JumpTo`). `accepted` = `MsgIntersectFound`
    /// at exactly the offered point.
    ///
    /// Returns `true` when the jumper's candidate fragment should be
    /// REPLACED by the accepted jump's fragment (`updateChainSyncState`) —
    /// the caller owns the peer's `PeerChainState`.
    pub fn process_jump_result(
        &self,
        addr: &SocketAddr,
        offered: JumpInfo,
        accepted: bool,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return false;
        };
        let CsjRole::Jumper(jr) = peer.role.clone() else {
            return false; // dynamo/objector handshakes use process_good_point_result
        };
        if accepted {
            match jr {
                JumperRole::LookingForIntersection { bad, .. } => {
                    // Accepted midpoint: the divergence is above it.
                    Self::look_for_intersection(&mut inner, *addr, offered, bad);
                }
                JumperRole::Happy { .. } => {
                    peer.role = CsjRole::Jumper(JumperRole::Happy {
                        fresh: false,
                        last_good: Some(offered),
                    });
                }
                JumperRole::FoundIntersection { .. } => {}
            }
            true
        } else {
            match jr {
                JumperRole::LookingForIntersection { good, .. } => {
                    Self::look_for_intersection(&mut inner, *addr, good, offered);
                }
                JumperRole::Happy { last_good, .. } => {
                    // `mkGoodJumpInfo`: with no prior accepted jump, the
                    // "good" start is the bad fragment's anchor
                    // (takeOldest 0 badFragment).
                    let good = last_good.unwrap_or_else(|| JumpInfo {
                        fragment: CandidateFragment::new(offered.fragment.anchor),
                    });
                    Self::look_for_intersection(&mut inner, *addr, good, offered);
                }
                JumperRole::FoundIntersection { .. } => {}
            }
            false
        }
    }

    /// A promoted dynamo/objector's `JumpToGoodPoint` round-trip completed.
    /// Rejection means the peer rolled back behind its last accepted jump —
    /// disengage and elect a replacement (`processJumpResult` for
    /// `JumpToGoodPoint`).
    ///
    /// Returns `true` when the peer's candidate fragment should be replaced
    /// by the good fragment (acceptance).
    pub fn process_good_point_result(&self, addr: &SocketAddr, accepted: bool) -> bool {
        if !self.enabled {
            return false;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get_mut(addr) else {
            return false;
        };
        if accepted {
            return true;
        }
        let role = std::mem::replace(&mut peer.role, CsjRole::Disengaged { restarting: true });
        peer.notify.notify_one();
        match role {
            CsjRole::Dynamo { .. } => Self::backfill_dynamo(&mut inner),
            CsjRole::Objector { .. } => Self::elect_new_objector(&mut inner),
            other => {
                // Not a promotion state — restore.
                if let Some(p) = inner.peers.get_mut(addr) {
                    p.role = other;
                }
            }
        }
        false
    }

    /// `rotateDynamo` — BlockFetch starved on this peer: demote it to a
    /// fresh jumper, move it to the back of the order, promote the next
    /// non-disengaged peer. No-op unless the peer is the current dynamo or
    /// it is the only engaged peer.
    /// Returns `true` iff the named peer WAS the dynamo and a rotation
    /// actually happened (used by callers to log accurately; the call is a
    /// cheap no-op for non-dynamo peers, matching Haskell `rotateDynamo`).
    pub fn rotate_dynamo(&self, addr: &SocketAddr) -> bool {
        if !self.enabled {
            return false;
        }
        let mut inner = self.inner.lock().expect("csj lock");
        let Some(peer) = inner.peers.get(addr) else {
            return false;
        };
        if !matches!(peer.role, CsjRole::Dynamo { .. }) {
            return false;
        }
        // Move to the back of the election order.
        inner.order.retain(|a| a != addr);
        inner.order.push_back(*addr);
        // Any other engaged peer?
        let successor = inner
            .order
            .iter()
            .find(|a| {
                *a != addr
                    && inner
                        .peers
                        .get(a)
                        .is_some_and(|p| !matches!(p.role, CsjRole::Disengaged { .. }))
            })
            .copied();
        let Some(_next) = successor else {
            return false; // only engaged peer — keep it as dynamo
        };
        if let Some(p) = inner.peers.get_mut(addr) {
            p.role = CsjRole::Jumper(JumperRole::Happy {
                fresh: true,
                last_good: None,
            });
            p.next_jump = None;
            p.notify.notify_one();
        }
        Self::backfill_dynamo(&mut inner);
        true
    }

    // ── internal: elections & helpers ───────────────────────────────────

    fn has_dynamo(inner: &Inner) -> bool {
        Self::dynamo_of(inner).is_some()
    }

    fn dynamo_of(inner: &Inner) -> Option<SocketAddr> {
        inner
            .peers
            .iter()
            .find(|(_, p)| matches!(p.role, CsjRole::Dynamo { .. }))
            .map(|(a, _)| *a)
    }

    /// Broadcast a jump to every Happy jumper (`setJumps`).
    fn set_jumps(inner: &mut Inner, ji: JumpInfo) {
        for peer in inner.peers.values_mut() {
            if matches!(peer.role, CsjRole::Jumper(JumperRole::Happy { .. })) {
                peer.next_jump = Some(ji.clone());
                peer.notify.notify_one();
            }
        }
    }

    fn disengage_peer(peer: &mut CsjPeer, restarting: bool) {
        peer.role = CsjRole::Disengaged { restarting };
        peer.jump_info = None;
        peer.next_jump = None;
        peer.notify.notify_one();
    }

    /// `backfillDynamo`: prefer a STARTED objector (its server cursor is
    /// already established), else the first non-disengaged peer in
    /// registration order. The new dynamo gets `DynamoStarting` when it has
    /// jump info to re-establish; every other engaged peer is reset to
    /// `Happy FreshJumper` (`promoteToDynamo`'s loop).
    fn backfill_dynamo(inner: &mut Inner) {
        let started_objector = inner
            .peers
            .iter()
            .find(|(_, p)| {
                matches!(
                    &p.role,
                    CsjRole::Objector {
                        starting: false,
                        ..
                    }
                )
            })
            .map(|(a, _)| *a);
        let chosen = started_objector.or_else(|| {
            inner
                .order
                .iter()
                .find(|a| {
                    inner
                        .peers
                        .get(a)
                        .is_some_and(|p| !matches!(p.role, CsjRole::Disengaged { .. }))
                })
                .copied()
        });
        let Some(dyn_addr) = chosen else { return };
        Self::promote_to_dynamo(inner, dyn_addr);
    }

    /// `promoteToDynamo`.
    fn promote_to_dynamo(inner: &mut Inner, dyn_addr: SocketAddr) {
        // Compute the new dynamo's initial state.
        let (starting, last_jump_slot) = {
            let p = &inner.peers[&dyn_addr];
            match &p.role {
                CsjRole::Objector {
                    starting: false,
                    good,
                    ..
                } => {
                    // Started objector: cursor established at `good`.
                    (None, good.head_slot())
                }
                _ => {
                    let ji = p.jump_info.clone();
                    let slot = ji
                        .as_ref()
                        .map(|j| j.head_slot())
                        .unwrap_or(WithOrigin::Origin);
                    (ji, slot)
                }
            }
        };
        if let Some(p) = inner.peers.get_mut(&dyn_addr) {
            p.role = CsjRole::Dynamo {
                starting,
                last_jump_slot,
            };
            p.next_jump = None;
            p.notify.notify_one();
        }
        // Demote every other engaged peer to a fresh jumper, clearing all
        // FoundIntersection / LookingForIntersection state.
        let dynamo_ji = inner.peers[&dyn_addr].jump_info.clone();
        let others: Vec<SocketAddr> = inner.peers.keys().copied().collect();
        for a in others {
            if a == dyn_addr {
                continue;
            }
            let p = inner.peers.get_mut(&a).expect("present");
            if matches!(p.role, CsjRole::Disengaged { .. }) {
                continue;
            }
            p.role = CsjRole::Jumper(JumperRole::Happy {
                fresh: true,
                last_good: None,
            });
            p.next_jump = dynamo_ji.clone();
            p.notify.notify_one();
        }
    }

    /// `lookForIntersection` — binary search on the BAD fragment between
    /// the accepted `good` tip and the rejected `bad` tip:
    ///
    /// ```haskell
    /// searchFragment = maybe badFragment snd $
    ///   AF.splitAfterPoint badFragment (AF.headPoint goodFragment)
    /// len <= 1 -> intersection found (good tip; bad point = bad head)
    /// otherwise -> jump to AF.dropNewest (len `div` 2) badFragment
    /// ```
    fn look_for_intersection(inner: &mut Inner, addr: SocketAddr, good: JumpInfo, bad: JumpInfo) {
        // Entries of `bad` strictly after good's tip.
        let good_tip = good.tip_point();
        let search_len = match good_tip {
            None => bad.fragment.len(),
            Some((gs, gh)) => {
                let idx = bad
                    .fragment
                    .entries
                    .iter()
                    .position(|e| e.slot == gs && e.hash == gh);
                match idx {
                    Some(i) => bad.fragment.len() - (i + 1),
                    // Good tip not ON bad's entries — it is bad's anchor
                    // (initial bisection with anchor-only good).
                    None => bad.fragment.len(),
                }
            }
        };
        if search_len <= 1 {
            let bad_point = match bad.fragment.head() {
                crate::genesis_peer_state::FragAnchor::Point(s, h) => (s, h),
                crate::genesis_peer_state::FragAnchor::Origin => (0, [0u8; 32]),
            };
            let Some(peer) = inner.peers.get_mut(&addr) else {
                return;
            };
            peer.role = CsjRole::Jumper(JumperRole::FoundIntersection {
                good: good.clone(),
                bad_point,
            });
            peer.next_jump = None;
            Self::maybe_elect_new_objector(inner, addr);
            return;
        }
        // Midpoint: drop the newest half of the bad fragment.
        let keep = bad.fragment.len() - search_len / 2;
        let mut mid_frag = bad.fragment.clone();
        mid_frag.entries.truncate(keep);
        let Some(peer) = inner.peers.get_mut(&addr) else {
            return;
        };
        peer.role = CsjRole::Jumper(JumperRole::LookingForIntersection {
            good,
            bad: bad.clone(),
        });
        peer.next_jump = Some(JumpInfo { fragment: mid_frag });
        peer.notify.notify_one();
    }

    /// `maybeElectNewObjector`: the objector seat goes to the
    /// FoundIntersection jumper with the OLDEST bad point; a younger
    /// incumbent is demoted back to the queue.
    fn maybe_elect_new_objector(inner: &mut Inner, candidate: SocketAddr) {
        let cand_bad = match &inner.peers[&candidate].role {
            CsjRole::Jumper(JumperRole::FoundIntersection { bad_point, .. }) => *bad_point,
            _ => return,
        };
        let incumbent = inner
            .peers
            .iter()
            .find(|(_, p)| matches!(p.role, CsjRole::Objector { .. }))
            .map(|(a, p)| match &p.role {
                CsjRole::Objector { bad_point, .. } => (*a, *bad_point),
                _ => unreachable!(),
            });
        match incumbent {
            None => Self::promote_objector(inner, candidate),
            Some((inc_addr, inc_bad)) => {
                if cand_bad.0 < inc_bad.0 {
                    // Candidate has the older intersection: demote the
                    // incumbent to FoundIntersection and take the seat.
                    if let Some(p) = inner.peers.get_mut(&inc_addr) {
                        if let CsjRole::Objector {
                            good, bad_point, ..
                        } = p.role.clone()
                        {
                            p.role =
                                CsjRole::Jumper(JumperRole::FoundIntersection { good, bad_point });
                            p.notify.notify_one();
                        }
                    }
                    Self::promote_objector(inner, candidate);
                }
                // else: incumbent keeps the seat; candidate stays queued.
            }
        }
    }

    /// `electNewObjector`: scan FoundIntersection jumpers, promote the one
    /// with the oldest bad point.
    fn elect_new_objector(inner: &mut Inner) {
        let chosen = inner
            .peers
            .iter()
            .filter_map(|(a, p)| match &p.role {
                CsjRole::Jumper(JumperRole::FoundIntersection { bad_point, .. }) => {
                    Some((*a, bad_point.0))
                }
                _ => None,
            })
            .min_by_key(|(_, slot)| *slot)
            .map(|(a, _)| a);
        if let Some(a) = chosen {
            Self::promote_objector(inner, a);
        }
    }

    fn promote_objector(inner: &mut Inner, addr: SocketAddr) {
        if let Some(p) = inner.peers.get_mut(&addr) {
            if let CsjRole::Jumper(JumperRole::FoundIntersection { good, bad_point }) =
                p.role.clone()
            {
                p.role = CsjRole::Objector {
                    starting: true,
                    good,
                    bad_point,
                };
                p.next_jump = None;
                p.notify.notify_one();
            }
        }
    }

    // ── diagnostics ─────────────────────────────────────────────────────

    /// (dynamo, objector, jumpers, disengaged) counts for metrics.
    pub fn role_counts(&self) -> (usize, usize, usize, usize) {
        let inner = self.inner.lock().expect("csj lock");
        let mut c = (0, 0, 0, 0);
        for p in inner.peers.values() {
            match p.role {
                CsjRole::Dynamo { .. } => c.0 += 1,
                CsjRole::Objector { .. } => c.1 += 1,
                CsjRole::Jumper(_) => c.2 += 1,
                CsjRole::Disengaged { .. } => c.3 += 1,
            }
        }
        c
    }

    /// Test/diagnostic accessor: the current role discriminant for a peer.
    #[allow(dead_code)] // diagnostics + transition-table tests
    pub fn role_of(&self, addr: &SocketAddr) -> Option<&'static str> {
        let inner = self.inner.lock().expect("csj lock");
        inner.peers.get(addr).map(|p| match p.role {
            CsjRole::Dynamo { .. } => "dynamo",
            CsjRole::Objector { .. } => "objector",
            CsjRole::Jumper(JumperRole::Happy { .. }) => "jumper-happy",
            CsjRole::Jumper(JumperRole::LookingForIntersection { .. }) => "jumper-bisecting",
            CsjRole::Jumper(JumperRole::FoundIntersection { .. }) => "jumper-found",
            CsjRole::Disengaged { .. } => "disengaged",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genesis_peer_state::{FragAnchor, FragEntry};

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn addr(n: u8) -> SocketAddr {
        format!("10.2.0.{n}:3001").parse().unwrap()
    }

    fn frag(anchor: FragAnchor, slots: &[(u64, u8)]) -> CandidateFragment {
        let mut f = CandidateFragment::new(anchor);
        for (slot, b) in slots {
            f.entries.push_back(FragEntry {
                slot: *slot,
                hash: h(*b),
                block_no: *slot,
            });
        }
        f
    }

    fn ji(anchor: FragAnchor, slots: &[(u64, u8)]) -> JumpInfo {
        JumpInfo {
            fragment: frag(anchor, slots),
        }
    }

    /// Registry with jump size 100, two registered peers: p1 = dynamo,
    /// p2 = jumper.
    fn two_peer_registry() -> Arc<CsjRegistry> {
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(1), false, WithOrigin::Origin);
        reg.register(addr(2), false, WithOrigin::Origin);
        assert_eq!(reg.role_of(&addr(1)), Some("dynamo"));
        assert_eq!(reg.role_of(&addr(2)), Some("jumper-happy"));
        reg
    }

    #[test]
    fn first_peer_is_dynamo_later_peers_jumpers_caught_up_disengaged() {
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(1), false, WithOrigin::Origin);
        reg.register(addr(2), false, WithOrigin::Origin);
        reg.register(addr(3), true, WithOrigin::Origin); // CaughtUp at connect
        assert_eq!(reg.role_of(&addr(1)), Some("dynamo"));
        assert_eq!(reg.role_of(&addr(2)), Some("jumper-happy"));
        assert_eq!(reg.role_of(&addr(3)), Some("disengaged"));
        assert!(matches!(
            reg.next_instruction(&addr(3)),
            CsjInstruction::RunNormally
        ));
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::Wait
        ));
        assert!(matches!(
            reg.next_instruction(&addr(1)),
            CsjInstruction::RunNormally
        ));
    }

    #[test]
    fn disabled_registry_runs_everyone_normally() {
        let reg = CsjRegistry::new(false, 100);
        reg.register(addr(1), false, WithOrigin::Origin);
        assert_eq!(reg.role_of(&addr(1)), None, "noJumping: not tracked");
        assert!(matches!(
            reg.next_instruction(&addr(1)),
            CsjInstruction::RunNormally
        ));
    }

    #[test]
    fn dynamo_jump_trigger_at_jump_size_cadence() {
        let reg = two_peer_registry();
        // Dynamo candidate grows to slot 50: below the boundary (0+100).
        reg.update_jump_info(&addr(1), frag(FragAnchor::Origin, &[(50, 1)]));
        reg.on_roll_forward(&addr(1), (50, h(1)));
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::Wait
        ));
        // Header at slot 101: succ(101)=102 > jumpSize+succ(Origin)=101 →
        // jump fires with the CURRENT jump info.
        reg.update_jump_info(&addr(1), frag(FragAnchor::Origin, &[(50, 1), (101, 2)]));
        reg.on_roll_forward(&addr(1), (101, h(2)));
        match reg.next_instruction(&addr(2)) {
            CsjInstruction::Jump(ji) => {
                assert_eq!(ji.tip_point(), Some((101, h(2))));
            }
            other => panic!("expected Jump, got {other:?}"),
        }
        // lastJumpSlot advanced to the jump tip: the next header below
        // 101+100 does not re-fire.
        reg.update_jump_info(
            &addr(1),
            frag(FragAnchor::Origin, &[(50, 1), (101, 2), (150, 3)]),
        );
        reg.on_roll_forward(&addr(1), (150, h(3)));
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::Wait
        ));
    }

    #[test]
    fn accepted_jump_keeps_jumper_happy_and_replaces_fragment() {
        let reg = two_peer_registry();
        let offered = ji(FragAnchor::Origin, &[(101, 2)]);
        reg.update_jump_info(&addr(1), offered.fragment.clone());
        reg.on_roll_forward(&addr(1), (101, h(2)));
        let CsjInstruction::Jump(offered) = reg.next_instruction(&addr(2)) else {
            panic!("expected jump");
        };
        let replace = reg.process_jump_result(&addr(2), offered, true);
        assert!(replace, "accepted JumpTo replaces the candidate fragment");
        assert_eq!(reg.role_of(&addr(2)), Some("jumper-happy"));
    }

    #[test]
    fn rejected_jump_bisects_then_objects() {
        let reg = two_peer_registry();
        // Dynamo fragment: 4 blocks at slots 10,20,30,40.
        let full = ji(FragAnchor::Origin, &[(10, 1), (20, 2), (30, 3), (40, 4)]);
        reg.update_jump_info(&addr(1), full.fragment.clone());
        reg.on_roll_forward(&addr(1), (200, h(9))); // force jump broadcast
        let CsjInstruction::Jump(offer1) = reg.next_instruction(&addr(2)) else {
            panic!("expected jump")
        };
        // Jumper rejects the full fragment (its chain diverges at slot 20).
        assert!(!reg.process_jump_result(&addr(2), offer1, false));
        assert_eq!(reg.role_of(&addr(2)), Some("jumper-bisecting"));
        // Bisection midpoint: drop newest 2 → fragment tip (20,2).
        let CsjInstruction::Jump(mid) = reg.next_instruction(&addr(2)) else {
            panic!("expected bisection jump")
        };
        assert_eq!(mid.tip_point(), Some((20, h(2))));
        // Jumper ACCEPTS (20,2): divergence is above; next midpoint (30,3).
        assert!(reg.process_jump_result(&addr(2), mid, true));
        let CsjInstruction::Jump(mid2) = reg.next_instruction(&addr(2)) else {
            panic!("expected second bisection jump")
        };
        assert_eq!(mid2.tip_point(), Some((30, h(3))));
        // Jumper REJECTS (30,3): gap (20..30] has length 1 → intersection
        // found at (20,2), bad point (30,3) → promoted straight to objector
        // (no incumbent).
        assert!(!reg.process_jump_result(&addr(2), mid2, false));
        assert_eq!(reg.role_of(&addr(2)), Some("objector"));
        // Its first instruction is the JumpToGoodPoint handshake.
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::JumpToGoodPoint(_)
        ));
        // Then it runs normally.
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::RunNormally
        ));
    }

    #[test]
    fn objector_seat_goes_to_oldest_intersection() {
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(1), false, WithOrigin::Origin); // dynamo
        reg.register(addr(2), false, WithOrigin::Origin);
        reg.register(addr(3), false, WithOrigin::Origin);
        let full = ji(FragAnchor::Origin, &[(10, 1), (20, 2), (30, 3), (40, 4)]);
        reg.update_jump_info(&addr(1), full.fragment.clone());
        reg.on_roll_forward(&addr(1), (200, h(9)));

        // Peer 2 converges to bad point (30,3) → objector.
        let CsjInstruction::Jump(o2) = reg.next_instruction(&addr(2)) else {
            panic!()
        };
        assert!(!reg.process_jump_result(&addr(2), o2, false));
        let CsjInstruction::Jump(m2) = reg.next_instruction(&addr(2)) else {
            panic!()
        };
        assert!(reg.process_jump_result(&addr(2), m2, true)); // accepts (20,2)
        let CsjInstruction::Jump(m2b) = reg.next_instruction(&addr(2)) else {
            panic!()
        };
        assert!(!reg.process_jump_result(&addr(2), m2b, false)); // rejects (30,3)
        assert_eq!(reg.role_of(&addr(2)), Some("objector"));

        // Peer 3 converges to an OLDER bad point (10,1): it must STEAL the
        // seat; peer 2 returns to the FoundIntersection queue.
        let CsjInstruction::Jump(o3) = reg.next_instruction(&addr(3)) else {
            panic!()
        };
        assert!(!reg.process_jump_result(&addr(3), o3, false));
        // Bisection: midpoint (20,2) — peer 3 rejects (diverges below).
        let CsjInstruction::Jump(m3) = reg.next_instruction(&addr(3)) else {
            panic!()
        };
        assert_eq!(m3.tip_point(), Some((20, h(2))));
        assert!(!reg.process_jump_result(&addr(3), m3, false));
        // Next midpoint (10,1) — peer 3 rejects again → gap ≤1 →
        // FoundIntersection with bad point (10,1) → steals the seat.
        let CsjInstruction::Jump(m3b) = reg.next_instruction(&addr(3)) else {
            panic!()
        };
        assert_eq!(m3b.tip_point(), Some((10, h(1))));
        assert!(!reg.process_jump_result(&addr(3), m3b, false));
        assert_eq!(reg.role_of(&addr(3)), Some("objector"));
        assert_eq!(reg.role_of(&addr(2)), Some("jumper-found"));
    }

    #[test]
    fn await_reply_disengages_and_backfills() {
        let reg = two_peer_registry();
        // Dynamo says it has no more headers → disengaged; the jumper is
        // promoted (fresh dynamo).
        reg.on_await_reply(&addr(1));
        assert_eq!(reg.role_of(&addr(1)), Some("disengaged"));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
        // The last engaged peer disengaging ends CSJ for everyone.
        reg.on_await_reply(&addr(2));
        assert_eq!(reg.role_of(&addr(2)), Some("disengaged"));
        assert!(matches!(
            reg.next_instruction(&addr(1)),
            CsjInstruction::RunNormally
        ));
    }

    #[test]
    fn dynamo_rollback_behind_last_jump_disengages_with_restart() {
        let reg = two_peer_registry();
        let full = ji(FragAnchor::Origin, &[(101, 2)]);
        reg.update_jump_info(&addr(1), full.fragment.clone());
        reg.on_roll_forward(&addr(1), (101, h(2))); // jump fired; lastJump=101
                                                    // Rollback to slot 50 < lastJumpSlot 101 → disengage + Restart;
                                                    // the jumper backfills as dynamo.
        reg.on_roll_backward(&addr(1), WithOrigin::At(50));
        assert_eq!(reg.role_of(&addr(1)), Some("disengaged"));
        assert!(matches!(
            reg.next_instruction(&addr(1)),
            CsjInstruction::Restart
        ));
        // After the restart instruction: DisengagedDone → RunNormally.
        assert!(matches!(
            reg.next_instruction(&addr(1)),
            CsjInstruction::RunNormally
        ));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
    }

    #[test]
    fn rotate_dynamo_moves_to_back_and_promotes_next() {
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(1), false, WithOrigin::Origin);
        reg.register(addr(2), false, WithOrigin::Origin);
        reg.register(addr(3), false, WithOrigin::Origin);
        reg.rotate_dynamo(&addr(1));
        assert_eq!(reg.role_of(&addr(1)), Some("jumper-happy"));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
        // Rotating a non-dynamo is a no-op.
        reg.rotate_dynamo(&addr(3));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
        // Sole engaged peer is never rotated away.
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(7), false, WithOrigin::Origin);
        reg.rotate_dynamo(&addr(7));
        assert_eq!(reg.role_of(&addr(7)), Some("dynamo"));
    }

    #[test]
    fn unregister_dynamo_backfills_unregister_objector_reelects() {
        let reg = CsjRegistry::new(true, 100);
        reg.register(addr(1), false, WithOrigin::Origin);
        reg.register(addr(2), false, WithOrigin::Origin);
        reg.unregister(&addr(1));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
        assert_eq!(reg.role_of(&addr(1)), None);
    }

    #[test]
    fn rejected_good_point_disengages_promoted_peer() {
        let reg = two_peer_registry();
        // Promote the jumper via dynamo await-reply.
        reg.on_await_reply(&addr(1));
        assert_eq!(reg.role_of(&addr(2)), Some("dynamo"));
        // The promoted dynamo's JumpToGoodPoint is REJECTED → disengage
        // with Restart (no other engaged peer to backfill).
        assert!(!reg.process_good_point_result(&addr(2), false));
        assert_eq!(reg.role_of(&addr(2)), Some("disengaged"));
        assert!(matches!(
            reg.next_instruction(&addr(2)),
            CsjInstruction::Restart
        ));
    }

    // #760-A: `fragment_head_slot` is the discriminator the BlockFetch
    // unproductive-dynamo watchdog uses to tell a silent dynamo (rotate) from
    // one parked on the forecast horizon (keep).
    #[test]
    fn fragment_head_slot_reports_fed_frontier_else_none() {
        let reg = two_peer_registry();
        // Dynamo p1 has not delivered a header yet → no jump_info → None.
        assert_eq!(reg.fragment_head_slot(&addr(1)), None);
        // Feed p1 a candidate fragment whose head is slot 5_000.
        reg.update_jump_info(&addr(1), frag(FragAnchor::Origin, &[(100, 1), (5_000, 2)]));
        assert_eq!(reg.fragment_head_slot(&addr(1)), Some(5_000));
        // An unknown peer → None.
        assert_eq!(reg.fragment_head_slot(&addr(99)), None);
        // A jump_info whose fragment is empty (anchored at Origin) → None.
        reg.update_jump_info(&addr(2), frag(FragAnchor::Origin, &[]));
        assert_eq!(reg.fragment_head_slot(&addr(2)), None);
    }
}
