//! Lossless per-peer ChainSync state for Ouroboros Genesis.
//!
//! The Rust analogue of Haskell's per-peer
//! `StrictTVar m (ChainSyncState blk)` (ouroboros-consensus
//! `MiniProtocol/ChainSync/Client/State.hs`):
//!
//! ```haskell
//! data ChainSyncState blk = ChainSyncState
//!   { csCandidate  :: !(AnchoredFragment (HeaderWithTime blk))
//!   , csIdling     :: !Bool
//!   , csLatestSlot :: !(StrictMaybe (WithOrigin SlotNo))
//!   }
//! ```
//!
//! The GSM's CaughtUp predicate, the GDD's density bounds, and the LoE
//! fragment (`sharedCandidatePrefix`) all read this state. In Haskell those
//! reads are STM — lossless by construction. dugite previously transported
//! the equivalent facts over a lossy `try_send` channel (audit findings
//! gdd-10 / gsm-13 / lop-historicity-04); this module replaces that with
//! shared state written *synchronously* by `chainsync_client_task` at the
//! protocol-message sites, so the governor always reads current truth.
//! `GsmEvent`s remain as wakeup hints only.
//!
//! # Fragment maintenance contract (writer = the peer's ChainSync task)
//!
//! - `set_anchor` once the intersection is negotiated (fragment anchored at
//!   the intersection point, Haskell: candidate anchored at `intersection`).
//! - `append_header` per validated `MsgRollForward` header. `csLatestSlot`
//!   is updated **before** the fragment is extended and even when the header
//!   is *not* appended (Haskell updates `csLatestSlot` ahead of the fragment
//!   so the GDD can see headers past the forecast horizon).
//! - `rollback_to` per `MsgRollBackward` (truncate; Haskell rolls the
//!   candidate fragment back).
//! - `set_idling(true)` on `MsgAwaitReply`; `set_idling(false)` on BOTH
//!   `MsgRollForward` AND `MsgRollBackward` (Haskell calls `idlingStop` in
//!   both receive arms — audit finding lop-historicity-03).
//!
//! The governor side (GSM actor) prunes fragments behind the advancing
//! immutable tip via `reanchor_to_immutable_tip`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// `WithOrigin SlotNo` — a slot that may be the pre-genesis origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // governor-side readers land with the LoE/GDD rewrite (T2/T3)
pub enum WithOrigin {
    /// Chain origin (before any block).
    Origin,
    /// A real slot.
    At(u64),
}

/// One header on a candidate fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragEntry {
    pub slot: u64,
    pub hash: [u8; 32],
    pub block_no: u64,
}

/// The anchor of a candidate fragment — the point *before* its first entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragAnchor {
    /// Anchored at the chain origin.
    Origin,
    /// Anchored at a specific point (slot, hash).
    Point(u64, [u8; 32]),
}

#[allow(dead_code)] // governor-side readers land with the LoE/GDD rewrite (T2/T3)
impl FragAnchor {
    /// Anchor slot as `WithOrigin`.
    pub fn slot(&self) -> WithOrigin {
        match self {
            FragAnchor::Origin => WithOrigin::Origin,
            FragAnchor::Point(slot, _) => WithOrigin::At(*slot),
        }
    }
}

/// An anchored candidate fragment (Haskell `AnchoredFragment`).
///
/// `entries` are strictly slot-ascending and hash-linked from `anchor`.
/// Uses `imbl::Vector` so snapshots taken by the governor are O(1).
#[derive(Debug, Clone)]
pub struct CandidateFragment {
    pub anchor: FragAnchor,
    pub entries: imbl::Vector<FragEntry>,
}

/// First index whose entry slot is >= `slot` (entries are slot-ascending).
fn lower_bound(entries: &imbl::Vector<FragEntry>, slot: u64) -> usize {
    let (mut lo, mut hi) = (0usize, entries.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if entries[mid].slot < slot {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[allow(dead_code)] // governor-side readers land with the LoE/GDD rewrite (T2/T3)
impl CandidateFragment {
    /// Empty fragment at the given anchor.
    pub fn new(anchor: FragAnchor) -> Self {
        CandidateFragment {
            anchor,
            entries: imbl::Vector::new(),
        }
    }

    /// The head (tip) point of the fragment: last entry, or the anchor.
    pub fn head(&self) -> FragAnchor {
        match self.entries.last() {
            Some(e) => FragAnchor::Point(e.slot, e.hash),
            None => self.anchor,
        }
    }

    /// Head slot as `WithOrigin` (Haskell `AF.headSlot`).
    pub fn head_slot(&self) -> WithOrigin {
        self.head().slot()
    }

    /// Number of headers on the fragment (Haskell `AF.length`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the fragment carries no headers.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `point` is the anchor or one of the entries.
    pub fn contains_point(&self, slot: u64, hash: &[u8; 32]) -> bool {
        if let FragAnchor::Point(a_slot, a_hash) = self.anchor {
            if a_slot == slot && a_hash == *hash {
                return true;
            }
        }
        // Entries are slot-sorted: binary search the first entry at `slot`.
        // Same-slot entries cannot occur post-Byron (and Byron EBB/main pairs
        // share a slot only at era boundaries) — scan the narrow window.
        let idx = lower_bound(&self.entries, slot);
        self.entries
            .iter()
            .skip(idx)
            .take_while(|e| e.slot == slot)
            .any(|e| e.hash == *hash)
    }

    /// Truncate the fragment so its head becomes `(slot, hash)`.
    ///
    /// Returns `false` when the point is neither the anchor nor on the
    /// fragment (protocol violation — the caller disconnects the peer).
    /// Rolling back to the anchor empties the fragment.
    pub fn rollback_to(&mut self, slot: u64, hash: &[u8; 32]) -> bool {
        if let FragAnchor::Point(a_slot, a_hash) = self.anchor {
            if a_slot == slot && a_hash == *hash {
                self.entries.clear();
                return true;
            }
        }
        if matches!(self.anchor, FragAnchor::Origin) && slot == 0 {
            // Rollback to origin point (slot 0 sentinel with zero hash is the
            // wire encoding of Origin handled by the caller; an explicit
            // origin rollback empties the fragment).
            if hash == &[0u8; 32] {
                self.entries.clear();
                return true;
            }
        }
        let idx = lower_bound(&self.entries, slot);
        for i in idx..self.entries.len() {
            let e = self.entries[i];
            if e.slot != slot {
                break;
            }
            if e.hash == *hash {
                self.entries.truncate(i + 1);
                return true;
            }
        }
        false
    }
}

/// Shared per-peer ChainSync state (Haskell `ChainSyncState` in a TVar).
#[derive(Debug)]
pub struct PeerChainState {
    /// The candidate fragment (`csCandidate`).
    fragment: Mutex<CandidateFragment>,
    /// `csIdling` — peer sent `MsgAwaitReply` and nothing since.
    idling: AtomicBool,
    /// `csLatestSlot` — most recent slot the peer has told us about,
    /// `None` until the peer first speaks (Haskell `SNothing`; GDD Gate 0
    /// excludes such peers entirely).
    latest_slot: Mutex<Option<WithOrigin>>,
}

#[allow(dead_code)] // governor-side readers land with the LoE/GDD rewrite (T2/T3)
impl PeerChainState {
    fn new(anchor: FragAnchor) -> Self {
        PeerChainState {
            fragment: Mutex::new(CandidateFragment::new(anchor)),
            idling: AtomicBool::new(false),
            latest_slot: Mutex::new(None),
        }
    }

    /// Re-anchor the fragment at the negotiated intersection (called once
    /// after `MsgIntersectFound`, before any headers arrive).
    pub fn set_anchor(&self, anchor: FragAnchor) {
        let mut frag = self.fragment.lock().expect("peer fragment lock");
        *frag = CandidateFragment::new(anchor);
    }

    /// Record a validated RollForward header.
    ///
    /// Updates `csLatestSlot` FIRST (unconditionally), clears idling, then
    /// appends to the fragment when it extends the head (hash-linked check is
    /// the caller's job; out-of-order duplicates are ignored idempotently).
    pub fn on_roll_forward(&self, entry: FragEntry) {
        *self.latest_slot.lock().expect("latest_slot lock") = Some(WithOrigin::At(entry.slot));
        self.idling.store(false, Ordering::Release);
        let mut frag = self.fragment.lock().expect("peer fragment lock");
        // Idempotency: ignore a header we already hold at the head.
        if let Some(last) = frag.entries.last() {
            if last.hash == entry.hash {
                return;
            }
            if entry.slot <= last.slot {
                // Stale / out-of-order header (the wire task delivers
                // in-order per peer; this only happens after an unprocessed
                // rollback). Drop — the rollback handler resyncs the
                // fragment.
                return;
            }
        }
        frag.entries.push_back(entry);
    }

    /// Record `MsgRollBackward` to `(slot, hash)`.
    ///
    /// Clears idling (Haskell `idlingStop` runs in the rollback arm too) and
    /// truncates the fragment. Also records the rollback target slot as
    /// `csLatestSlot` (the peer "spoke", and its latest claim is the
    /// rollback point — Haskell sets `csLatestSlot` on rollbacks via
    /// `chainSyncStateFor`).
    ///
    /// Returns `false` when the point is not on the fragment/anchor.
    pub fn on_roll_backward(&self, slot: u64, hash: &[u8; 32]) -> bool {
        *self.latest_slot.lock().expect("latest_slot lock") = Some(WithOrigin::At(slot));
        self.idling.store(false, Ordering::Release);
        self.fragment
            .lock()
            .expect("peer fragment lock")
            .rollback_to(slot, hash)
    }

    /// Record `MsgAwaitReply` (`idlingStart`).
    pub fn on_await_reply(&self) {
        self.idling.store(true, Ordering::Release);
    }

    /// Current idling flag.
    pub fn is_idling(&self) -> bool {
        self.idling.load(Ordering::Acquire)
    }

    /// Current `csLatestSlot`.
    pub fn latest_slot(&self) -> Option<WithOrigin> {
        *self.latest_slot.lock().expect("latest_slot lock")
    }

    /// O(1) snapshot of the candidate fragment (imbl clone).
    pub fn fragment_snapshot(&self) -> CandidateFragment {
        self.fragment.lock().expect("peer fragment lock").clone()
    }

    /// Replace the candidate fragment wholesale and update `csLatestSlot`
    /// (Haskell `updateChainSyncState`: a jumper that accepts a jump takes
    /// the dynamo's fragment so the GDD sees it).
    pub fn replace_fragment(&self, fragment: CandidateFragment) {
        *self.latest_slot.lock().expect("latest_slot lock") = Some(fragment.head_slot());
        *self.fragment.lock().expect("peer fragment lock") = fragment;
    }

    /// Drop fragment entries at or below the immutable tip and re-anchor.
    ///
    /// Mirrors how Haskell candidate fragments are always compared after
    /// `splitAfterPoint frag immutableTip` (`sharedCandidatePrefix`):
    /// - immutable tip on the fragment → drop everything up to and including
    ///   it; anchor becomes the immutable tip; returns `true`.
    /// - immutable tip IS the anchor → no-op; returns `true`.
    /// - fragment does not reach the immutable tip (e.g. its anchor is newer,
    ///   the CSJ-jumper case, or it's on a stale fork below the tip) →
    ///   returns `false`; the caller treats the candidate as empty-at-tip
    ///   for LoE purposes but keeps the state for GDD signals.
    pub fn reanchor_to_immutable_tip(&self, imm_slot: u64, imm_hash: &[u8; 32]) -> bool {
        let mut frag = self.fragment.lock().expect("peer fragment lock");
        if let FragAnchor::Point(a_slot, a_hash) = frag.anchor {
            if a_slot == imm_slot && a_hash == *imm_hash {
                return true;
            }
        }
        let idx = lower_bound(&frag.entries, imm_slot);
        for i in idx..frag.entries.len() {
            let e = frag.entries[i];
            if e.slot != imm_slot {
                break;
            }
            if e.hash == *imm_hash {
                // Keep entries strictly after the immutable tip.
                let kept = frag.entries.skip(i + 1);
                frag.entries = kept;
                frag.anchor = FragAnchor::Point(imm_slot, *imm_hash);
                return true;
            }
        }
        false
    }
}

/// Registry of all connected peers' ChainSync states (Haskell
/// `ChainSyncClientHandleCollection`'s map half).
#[derive(Debug, Default)]
pub struct PeerStateRegistry {
    peers: RwLock<HashMap<SocketAddr, Arc<PeerChainState>>>,
}

#[allow(dead_code)] // governor-side readers land with the LoE/GDD rewrite (T2/T3)
impl PeerStateRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a peer at its negotiated intersection. Replaces any previous
    /// state for the address (reconnect).
    pub fn register(&self, addr: SocketAddr, anchor: FragAnchor) -> Arc<PeerChainState> {
        let state = Arc::new(PeerChainState::new(anchor));
        self.peers
            .write()
            .expect("registry lock")
            .insert(addr, state.clone());
        state
    }

    /// Remove a disconnected peer.
    pub fn deregister(&self, addr: &SocketAddr) {
        self.peers.write().expect("registry lock").remove(addr);
    }

    /// Look up a peer's state.
    pub fn get(&self, addr: &SocketAddr) -> Option<Arc<PeerChainState>> {
        self.peers.read().expect("registry lock").get(addr).cloned()
    }

    /// All peers (states are Arcs — cheap).
    pub fn all(&self) -> Vec<(SocketAddr, Arc<PeerChainState>)> {
        self.peers
            .read()
            .expect("registry lock")
            .iter()
            .map(|(a, s)| (*a, s.clone()))
            .collect()
    }

    /// Number of registered peers.
    pub fn len(&self) -> usize {
        self.peers.read().expect("registry lock").len()
    }

    /// True when no peers are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn e(slot: u64, b: u8) -> FragEntry {
        FragEntry {
            slot,
            hash: h(b),
            block_no: slot, // tests: block_no tracks slot
        }
    }

    fn addr(n: u8) -> SocketAddr {
        format!("10.0.0.{n}:3001").parse().unwrap()
    }

    #[test]
    fn fragment_append_and_head() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        assert_eq!(st.latest_slot(), None, "csLatestSlot starts SNothing");
        st.on_roll_forward(e(101, 1));
        st.on_roll_forward(e(105, 2));
        let frag = st.fragment_snapshot();
        assert_eq!(frag.len(), 2);
        assert_eq!(frag.head(), FragAnchor::Point(105, h(2)));
        assert_eq!(st.latest_slot(), Some(WithOrigin::At(105)));
    }

    #[test]
    fn latest_slot_updates_even_when_header_not_appended() {
        // Haskell updates csLatestSlot BEFORE extending the fragment, and it
        // advances even for headers the fragment logic drops.
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(105, 1));
        // Stale duplicate (same slot, different hash) is not appended…
        st.on_roll_forward(e(105, 9));
        let frag = st.fragment_snapshot();
        assert_eq!(frag.len(), 1);
        // …but csLatestSlot still reflects the peer's latest claim.
        assert_eq!(st.latest_slot(), Some(WithOrigin::At(105)));
    }

    #[test]
    fn duplicate_head_header_is_idempotent() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        st.on_roll_forward(e(101, 1));
        assert_eq!(st.fragment_snapshot().len(), 1, "no double-count (gsm-06)");
    }

    #[test]
    fn rollback_truncates_fragment() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        st.on_roll_forward(e(102, 2));
        st.on_roll_forward(e(103, 3));
        assert!(st.on_roll_backward(101, &h(1)));
        let frag = st.fragment_snapshot();
        assert_eq!(frag.len(), 1);
        assert_eq!(frag.head(), FragAnchor::Point(101, h(1)));
        // Rollback to the anchor empties the fragment.
        assert!(st.on_roll_backward(100, &h(0xaa)));
        assert!(st.fragment_snapshot().is_empty());
    }

    #[test]
    fn rollback_to_unknown_point_fails() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        assert!(!st.on_roll_backward(101, &h(0x77)), "wrong hash at slot");
        assert!(!st.on_roll_backward(50, &h(0x77)), "below anchor");
    }

    #[test]
    fn idling_set_on_await_cleared_on_both_roll_directions() {
        // lop-historicity-03: Haskell calls idlingStop in recvMsgRollForward
        // AND recvMsgRollBackward.
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_await_reply();
        assert!(st.is_idling());
        st.on_roll_forward(e(101, 1));
        assert!(!st.is_idling(), "RollForward clears idling");
        st.on_await_reply();
        assert!(st.is_idling());
        st.on_roll_backward(101, &h(1));
        assert!(!st.is_idling(), "RollBackward clears idling too");
    }

    #[test]
    fn reanchor_drops_prefix_when_tip_on_fragment() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        st.on_roll_forward(e(102, 2));
        st.on_roll_forward(e(103, 3));
        assert!(st.reanchor_to_immutable_tip(102, &h(2)));
        let frag = st.fragment_snapshot();
        assert_eq!(frag.anchor, FragAnchor::Point(102, h(2)));
        assert_eq!(frag.len(), 1);
        assert_eq!(frag.head(), FragAnchor::Point(103, h(3)));
        // Idempotent when tip == anchor.
        assert!(st.reanchor_to_immutable_tip(102, &h(2)));
        assert_eq!(st.fragment_snapshot().len(), 1);
    }

    #[test]
    fn reanchor_fails_when_fragment_does_not_reach_tip() {
        // CSJ-jumper / stale-fork case: candidate doesn't contain the
        // immutable tip → caller treats as empty-at-tip for the LoE.
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        assert!(!st.reanchor_to_immutable_tip(102, &h(0x55)));
        // Fragment untouched.
        assert_eq!(st.fragment_snapshot().len(), 1);
    }

    #[test]
    fn registry_register_replaces_on_reconnect() {
        let reg = PeerStateRegistry::new();
        let st1 = reg.register(addr(1), FragAnchor::Origin);
        st1.on_roll_forward(e(5, 1));
        let st2 = reg.register(addr(1), FragAnchor::Point(5, h(1)));
        assert_eq!(reg.len(), 1);
        assert!(
            st2.fragment_snapshot().is_empty(),
            "fresh state on reconnect"
        );
        // Old Arc still readable by its holder but detached from the registry.
        assert_eq!(reg.get(&addr(1)).unwrap().fragment_snapshot().len(), 0);
        reg.deregister(&addr(1));
        assert!(reg.is_empty());
    }

    #[test]
    fn contains_point_checks_anchor_and_entries() {
        let st = PeerChainState::new(FragAnchor::Point(100, h(0xaa)));
        st.on_roll_forward(e(101, 1));
        let frag = st.fragment_snapshot();
        assert!(frag.contains_point(100, &h(0xaa)), "anchor");
        assert!(frag.contains_point(101, &h(1)), "entry");
        assert!(!frag.contains_point(101, &h(2)), "same slot wrong hash");
        assert!(!frag.contains_point(102, &h(1)), "absent slot");
    }
}
