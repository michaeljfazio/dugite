//! `trimToLoE` — enforcing the Limit on Eagerness in chain selection.
//!
//! Haskell (`Ouroboros.Consensus.Storage.ChainDB.Impl.ChainSel`,
//! `constructPreferableCandidates`):
//!
//! ```haskell
//! trimToLoE LoEDisabled diff = diff
//! trimToLoE (LoEEnabled loe) diff =
//!   case AF.intersect cand loe of
//!     Just (candPrefix, _, candSuffix, loeSuffix) ->
//!       let trimmedCandSuffix = AF.takeOldest (fromIntegral k) candSuffix
//!           trimmedCand =
//!             if AF.null loeSuffix     -- LoE tip is ON the candidate
//!               then fromJust $ AF.join candPrefix trimmedCandSuffix
//!               else candPrefix       -- candidate diverges before the LoE tip
//!        in Diff.diff curChain trimmedCand
//! ```
//!
//! Semantics enforced here, expressed over dugite's VolatileDB block graph:
//!
//! - **LoE disabled** (praos mode / GSM CaughtUp): identity — zero-cost fast
//!   path; chain selection behaves exactly as before this module existed.
//! - **Candidate contains the LoE tip**: it may be adopted up to `k` blocks
//!   past the tip (`takeOldest k candSuffix`).
//! - **Candidate diverges from the LoE fragment before its tip**: it is
//!   trimmed to the divergence point (`candPrefix`) — i.e. nothing NEW from
//!   that candidate may be adopted; the block stays in the VolatileDB and
//!   re-enters selection when the LoE moves (`ChainSelReprocessLoEBlocks`).
//!
//! The walk consults the candidate's ancestry through VolatileDB `prev_hash`
//! links. A parent that has left the VolatileDB is, on any adoptable chain,
//! the immutable tip — which is exactly the LoE fragment's anchor — so the
//! walk treats "parent not in VolatileDB" as reaching the anchor; candidates
//! that actually diverge below the immutable tip are refused later by
//! `switch_chain` (unreachable intersection).

use crate::volatile_db::VolatileDB;
use dugite_consensus::loe::LoeState;
use dugite_primitives::hash::Hash32;
use std::collections::HashSet;

/// Pre-indexed view of a published [`LoeState`] for O(1) membership checks.
///
/// Rebuilt only when the published `Arc<LoeState>` pointer changes (the
/// governor republishes at most ~1/s).
#[derive(Debug)]
pub struct LoeView {
    /// Identity of the `Arc<LoeState>` this view was built from.
    pub source_ptr: usize,
    /// Disabled → selection is unconstrained.
    pub disabled: bool,
    /// All fragment member hashes INCLUDING the anchor hash (when concrete).
    members: HashSet<[u8; 32]>,
    /// The LoE tip hash (last entry, else anchor), `None` when the tip is
    /// Origin (fresh chain — every candidate trivially contains it).
    tip_hash: Option<[u8; 32]>,
    /// Security parameter `k` — max blocks past the LoE tip.
    pub k: u64,
}

impl LoeView {
    /// Build a view from a published state. `source_ptr` is the
    /// `Arc::as_ptr` of the published value, used for cache invalidation.
    pub fn build(state: &LoeState, source_ptr: usize) -> Self {
        match state {
            LoeState::Disabled => LoeView {
                source_ptr,
                disabled: true,
                members: HashSet::new(),
                tip_hash: None,
                k: u64::MAX,
            },
            LoeState::SelectionTip { k } => {
                // Degenerate zero-peer state: the governor anchors the LoE at
                // the selection tip it observed. Consumers treat it like an
                // empty fragment anchored there; the anchor is unknown here
                // (no point captured), so behave as "tip = Origin": every
                // candidate contains it, k-allowance from the walk exit.
                LoeView {
                    source_ptr,
                    disabled: false,
                    members: HashSet::new(),
                    tip_hash: None,
                    k: *k,
                }
            }
            LoeState::Fragment { anchor, entries, k } => {
                let mut members: HashSet<[u8; 32]> = entries.iter().map(|p| p.hash).collect();
                if let Some(a) = anchor {
                    members.insert(a.hash);
                }
                let tip_hash = entries.last().map(|p| p.hash).or(anchor.map(|a| a.hash));
                LoeView {
                    source_ptr,
                    disabled: false,
                    members,
                    tip_hash,
                    k: *k,
                }
            }
        }
    }

    fn is_member(&self, hash: &Hash32) -> bool {
        self.members.contains(hash.as_bytes())
    }

    fn is_tip(&self, hash: &Hash32) -> bool {
        self.tip_hash.is_some_and(|t| t == *hash.as_bytes())
    }
}

/// Verdict for a candidate tip under the LoE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoeVerdict {
    /// LoE disabled, or the candidate respects it — adoptable as-is.
    Allowed,
    /// The candidate contains the LoE tip but extends more than `k` blocks
    /// past it: adoptable only up to the ancestor `k` blocks past the tip
    /// (`candPrefix ++ takeOldest k candSuffix`).
    TrimmedTo(Hash32),
    /// The candidate diverges from the LoE fragment before its tip
    /// (`candPrefix` case — nothing new adoptable), or its ancestry cannot
    /// be resolved. The block(s) stay in the VolatileDB; selection
    /// re-evaluates when the LoE advances.
    Deferred,
}

/// `trimToLoE` for a candidate tip already stored in the VolatileDB.
///
/// Walks the candidate's ancestry until it reaches the LoE fragment:
/// - first hit IS the LoE tip and the walk depth `d ≤ k` → `Allowed`
///   (`candPrefix ++ takeOldest k candSuffix` covers the whole candidate);
/// - first hit is the tip with `d > k` → `Deferred` (the tip itself is more
///   than k past the LoE; the adoptable prefix is not this tip);
/// - first hit is a NON-tip member (candidate diverges from the fragment
///   before its tip — Haskell's non-null `loeSuffix`) → `Deferred`
///   (`trimmedCand = candPrefix`: nothing new adoptable from here);
/// - the walk exits the VolatileDB (reached the immutable tip = anchor
///   conceptually) — treated as an anchor hit.
pub fn loe_verdict_for_candidate(
    volatile: &VolatileDB,
    candidate_tip: &Hash32,
    view: &LoeView,
) -> LoeVerdict {
    if view.disabled {
        return LoeVerdict::Allowed;
    }

    // Tip = Origin (or zero-peer SelectionTip view): every candidate contains
    // it; only the k-allowance applies, counted from the walk exit (anchor).
    let tip_is_origin = view.tip_hash.is_none();

    // Depth 0: the candidate tip itself is on the fragment.
    if view.is_member(candidate_tip) {
        return LoeVerdict::Allowed;
    }

    // Walk ancestry until the LoE tip (or anchor) is found. The walk depth is
    // unbounded in principle: during bulk sync BlockFetch accumulates a
    // candidate chain arbitrarily far past a briefly-pinned LoE tip (mainnet
    // Byron 2026-07-28: 257k blocks), and Haskell's `trimToLoE` still trims it
    // to `candPrefix ++ takeOldest k candSuffix` — deferring it outright
    // froze selection until the LoE jumped. The only hard cap is a cycle
    // guard: a well-formed chain visits each VolatileDB block at most once.
    let cap = (volatile.len() as u64).saturating_add(1);
    // Sliding window of the last ≤ k path entries (newest at the back). At a
    // hit of depth d > k the adoptable tip is the ancestor exactly k blocks
    // past the hit — the FRONT of the window.
    let mut window: std::collections::VecDeque<Hash32> = std::collections::VecDeque::new();
    let mut depth: u64 = 0;
    let mut cur = *candidate_tip;
    loop {
        let Some(block) = volatile.get_block(&cur) else {
            // Candidate tip itself unknown — nothing to adopt.
            return LoeVerdict::Deferred;
        };
        if view.k > 0 && window.len() as u64 == view.k {
            window.pop_front();
        }
        window.push_back(cur);
        depth += 1; // blocks beyond a hit at `parent`
        let parent = block.prev_hash;

        let tip_hit = view.is_tip(&parent) || (tip_is_origin && !volatile.has_block(&parent));
        let anchor_exit = !tip_hit
            && view.members.is_empty()
            && !view.is_member(&parent)
            && !volatile.has_block(&parent);
        if tip_hit || anchor_exit {
            // Reached the LoE tip (or, for an empty fragment, its anchor at
            // the VolatileDB exit): the candidate contains it, `depth`
            // blocks beyond.
            return if depth <= view.k {
                LoeVerdict::Allowed
            } else {
                // Trim to the ancestor exactly k past the hit: with the
                // window holding the last k entries, that is its front.
                // (k = 0 leaves nothing adoptable.)
                match window.front() {
                    Some(t) if view.k > 0 => LoeVerdict::TrimmedTo(*t),
                    _ => LoeVerdict::Deferred,
                }
            };
        }
        if view.is_member(&parent) {
            // Hit the fragment BELOW its tip: the candidate diverges from
            // the LoE fragment (non-null loeSuffix) → candPrefix only.
            return LoeVerdict::Deferred;
        }
        if !volatile.has_block(&parent) {
            // Left the VolatileDB without meeting a non-empty fragment:
            // divergence at/below the anchor → candPrefix only.
            return LoeVerdict::Deferred;
        }

        if depth >= cap {
            // Cycle guard only — unreachable on a well-formed chain.
            return LoeVerdict::Deferred;
        }
        cur = parent;
    }
}

/// LoE gate for a pure EXTENSION of the selected chain (the new block's
/// parent is the current tip). Equivalent to [`loe_verdict_for_candidate`]
/// on the would-be new tip, evaluated BEFORE it is linked into the chain.
pub fn loe_allows_extension(
    volatile: &VolatileDB,
    new_hash: &Hash32,
    new_prev: &Hash32,
    view: &LoeView,
) -> bool {
    if view.disabled {
        return true;
    }
    // The new block on the fragment → always fine.
    if view.is_member(new_hash) {
        return true;
    }
    let tip_is_origin = view.tip_hash.is_none();
    // Parent is the LoE tip → depth 1 ≤ k (k ≥ 1 always in practice).
    if view.is_tip(new_prev) || (tip_is_origin && !volatile.has_block(new_prev)) {
        return 1 <= view.k;
    }
    if view.is_member(new_prev) {
        // Diverging from the fragment below its tip.
        return false;
    }
    // Otherwise the parent (current selection tip) is beyond the LoE tip —
    // the extension is allowed iff the parent's depth + 1 is within k.
    depth_past_loe_tip(volatile, new_prev, view)
        .map(|d| d < view.k)
        .unwrap_or(false)
}

/// Depth (block count) of `hash` beyond the LoE tip, when the candidate
/// contains the tip. `None` when it does not (divergence / unknown).
fn depth_past_loe_tip(volatile: &VolatileDB, hash: &Hash32, view: &LoeView) -> Option<u64> {
    if view.is_member(hash) {
        return Some(0);
    }
    let tip_is_origin = view.tip_hash.is_none();
    let cap = view
        .k
        .saturating_add(view.members.len() as u64)
        .saturating_add(2);
    let mut cur = *hash;
    let mut depth: u64 = 1;
    loop {
        let block = volatile.get_block(&cur)?;
        let parent = block.prev_hash;
        if view.is_tip(&parent) || (tip_is_origin && !volatile.has_block(&parent)) {
            return Some(depth);
        }
        if view.is_member(&parent) {
            return None; // divergence below the LoE tip
        }
        if !volatile.has_block(&parent) {
            return if view.members.is_empty() {
                Some(depth)
            } else {
                None
            };
        }
        depth += 1;
        if depth > cap {
            return None;
        }
        cur = parent;
    }
}
