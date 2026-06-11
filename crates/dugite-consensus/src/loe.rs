//! Limit on Eagerness (LoE) — the chain-selection constraint of Ouroboros
//! Genesis.
//!
//! Mirrors Haskell's `LoE a` (`Ouroboros.Consensus.Storage.ChainDB.API`):
//!
//! ```haskell
//! -- | The LoE tip is the youngest header that is present on all candidate
//! -- fragments. … The LoE restrains the current selection of the node to be
//! -- on the same chain as the LoE tip, and to not extend more than k blocks
//! -- from it.
//! data LoE a = LoEDisabled | LoEEnabled !a
//! ```
//!
//! The value carried by `LoEEnabled` is the LoE *fragment* — the shared
//! prefix of all peers' candidate fragments, anchored at the immutable tip
//! (computed by the GDD governor's `sharedCandidatePrefix`). This type is the
//! *consumer-facing* representation handed to chain selection
//! (`dugite-storage`'s ChainSelQueue); the producer (the GSM/GDD actor in
//! dugite-node) recomputes and republishes it on every governor evaluation.
//!
//! State mapping (Haskell `setGetLoEFragment`):
//! - Praos mode / GSM CaughtUp → [`LoeState::Disabled`] (`trimToLoE
//!   LoEDisabled = id` — the praos fast path).
//! - GSM PreSyncing → `Fragment` with empty entries anchored at the immutable
//!   tip (selection may still extend up to k past the anchor — PreSyncing is
//!   NOT a total freeze).
//! - GSM Syncing → `Fragment` (live shared candidate prefix); with ZERO
//!   eligible peers `sharedCandidatePrefix` degenerates to the current
//!   selection, expressed here as [`LoeState::SelectionTip`] (full k-block
//!   freedom past the selection tip — "losing all peers effectively disables
//!   the LoE constraint until new peers connect").

/// A `(slot, header-hash)` chain point on the LoE fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoePoint {
    pub slot: u64,
    pub hash: [u8; 32],
}

/// The Limit on Eagerness as consumed by chain selection.
#[derive(Debug, Clone)]
pub enum LoeState {
    /// LoE disabled — selection behaves exactly as Praos (`trimToLoE
    /// LoEDisabled diff = diff`).
    Disabled,
    /// Degenerate Syncing state with no eligible candidate fragments: the
    /// LoE fragment equals the current selection, so any candidate may
    /// extend at most `k` blocks past its intersection with the selection.
    SelectionTip {
        /// Security parameter `k` — max blocks past the LoE tip.
        k: u64,
    },
    /// A real LoE fragment anchored at the immutable tip.
    Fragment {
        /// The immutable tip the fragment is anchored at (`None` = Origin).
        anchor: Option<LoePoint>,
        /// Fragment members beyond the anchor, oldest first. The LoE *tip*
        /// is the last entry (or the anchor when empty).
        entries: Vec<LoePoint>,
        /// Security parameter `k` — max blocks past the LoE tip
        /// (`AF.takeOldest k candSuffix` in `trimToLoE`).
        k: u64,
    },
}

impl LoeState {
    /// The LoE tip point: last fragment entry, or the anchor.
    /// `None` means the tip is Origin (fresh chain) or the LoE is
    /// disabled/selection-shaped (no concrete point).
    pub fn fragment_tip(&self) -> Option<LoePoint> {
        match self {
            LoeState::Fragment {
                anchor, entries, ..
            } => entries.last().copied().or(*anchor),
            _ => None,
        }
    }

    /// True when chain selection is unconstrained (praos / CaughtUp).
    pub fn is_disabled(&self) -> bool {
        matches!(self, LoeState::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_tip_is_last_entry_or_anchor() {
        let anchor = LoePoint {
            slot: 10,
            hash: [1; 32],
        };
        let e2 = LoePoint {
            slot: 12,
            hash: [3; 32],
        };
        let frag = LoeState::Fragment {
            anchor: Some(anchor),
            entries: vec![
                LoePoint {
                    slot: 11,
                    hash: [2; 32],
                },
                e2,
            ],
            k: 2160,
        };
        assert_eq!(frag.fragment_tip(), Some(e2));

        let empty = LoeState::Fragment {
            anchor: Some(anchor),
            entries: vec![],
            k: 2160,
        };
        assert_eq!(empty.fragment_tip(), Some(anchor));

        let origin = LoeState::Fragment {
            anchor: None,
            entries: vec![],
            k: 2160,
        };
        assert_eq!(origin.fragment_tip(), None);
        assert!(LoeState::Disabled.is_disabled());
        assert!(!origin.is_disabled());
    }
}
