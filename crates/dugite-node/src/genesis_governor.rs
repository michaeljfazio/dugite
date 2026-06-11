//! Pure algorithms of the Genesis governor: `sharedCandidatePrefix` (the LoE
//! fragment) and `densityDisconnect` (the GDD).
//!
//! Byte-faithful ports of `Ouroboros.Consensus.Genesis.Governor`
//! (ouroboros-consensus `release-ouroboros-consensus-3.0.1.0`). Every
//! predicate below quotes the Haskell it implements. The impure half (the
//! actor that schedules evaluations, publishes the LoE and kills losers)
//! lives in `gsm.rs`.

use std::net::SocketAddr;

use crate::genesis_peer_state::{CandidateFragment, FragAnchor, FragEntry, WithOrigin};
use dugite_consensus::loe::LoePoint;

/// `succWithOrigin`: `Origin → 0`, `At s → s + 1`.
fn succ_with_origin(w: WithOrigin) -> u64 {
    match w {
        WithOrigin::Origin => 0,
        WithOrigin::At(s) => s.saturating_add(1),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// sharedCandidatePrefix
// ───────────────────────────────────────────────────────────────────────────

/// Result of [`shared_candidate_prefix`].
#[derive(Debug)]
pub struct SharedPrefix {
    /// The LoE fragment: members beyond the immutable tip, oldest first.
    pub prefix: Vec<LoePoint>,
    /// Per-peer candidate suffixes BEYOND the LoE tip (Haskell's
    /// `candidateSuffixes` — the second component of `sharedCandidatePrefix`,
    /// consumed by `densityDisconnect`). Peers whose fragment does not reach
    /// the immutable tip contribute an empty suffix (Haskell: fragment
    /// replaced by `AF.takeOldest 0 curChain`, i.e. empty at the tip).
    pub suffixes: Vec<(SocketAddr, imbl::Vector<FragEntry>)>,
}

/// Slot of the LoE tip given the prefix (or the immutable-tip anchor).
pub fn loe_tip_slot(imm_tip: Option<(u64, [u8; 32])>, prefix: &[LoePoint]) -> WithOrigin {
    match prefix.last() {
        Some(p) => WithOrigin::At(p.slot),
        None => match imm_tip {
            Some((s, _)) => WithOrigin::At(s),
            None => WithOrigin::Origin,
        },
    }
}

/// Per-peer candidate suffix anchored at the immutable tip.
///
/// Haskell (`sharedCandidatePrefix.splitAfterImmutableTip`):
/// ```haskell
/// case AF.splitAfterPoint frag immutableTip of
///   Nothing          -> (peer, AF.takeOldest 0 curChain)  -- empty at tip
///   Just (_, suffix) -> (peer, suffix)
/// ```
fn suffix_at_imm_tip(
    frag: &CandidateFragment,
    imm_tip: Option<(u64, [u8; 32])>,
) -> imbl::Vector<FragEntry> {
    match imm_tip {
        None => {
            // Immutable tip is Origin: a fragment anchored at Origin contains
            // it trivially; any other anchor cannot precede Origin.
            match frag.anchor {
                FragAnchor::Origin => frag.entries.clone(),
                FragAnchor::Point(..) => {
                    // Anchored above Origin: the fragment "contains" the
                    // origin point conceptually (its history passes through
                    // it), but we cannot enumerate the members between Origin
                    // and the anchor. Treat as not reaching: empty.
                    imbl::Vector::new()
                }
            }
        }
        Some((imm_slot, imm_hash)) => {
            if let FragAnchor::Point(a_slot, a_hash) = frag.anchor {
                if a_slot == imm_slot && a_hash == imm_hash {
                    return frag.entries.clone();
                }
            }
            // Find the immutable tip among the entries.
            let idx = frag
                .entries
                .iter()
                .position(|e| e.slot == imm_slot && e.hash == imm_hash);
            match idx {
                Some(i) => frag.entries.clone().split_off(i + 1),
                None => imbl::Vector::new(),
            }
        }
    }
}

/// `sharedCandidatePrefix`: the longest common prefix of all candidate
/// fragments (anchored at the immutable tip), plus the per-peer suffixes
/// beyond it.
///
/// Haskell:
/// ```haskell
/// sharedCandidatePrefix curChain candidates =
///   second getCompose $
///     stripCommonPrefix (AF.castAnchor $ AF.anchor curChain) $
///       Compose immutableTipSuffixes
/// ```
///
/// The degenerate zero-candidate case ("losing all peers effectively
/// disables the LoE constraint") is handled by the CALLER — `LoeState::
/// SelectionTip` — because this function has no view of the selection.
pub fn shared_candidate_prefix(
    imm_tip: Option<(u64, [u8; 32])>,
    peers: &[(SocketAddr, CandidateFragment)],
) -> SharedPrefix {
    let tip_suffixes: Vec<(SocketAddr, imbl::Vector<FragEntry>)> = peers
        .iter()
        .map(|(addr, frag)| (*addr, suffix_at_imm_tip(frag, imm_tip)))
        .collect();

    // Longest common prefix by position-wise (slot, hash) equality.
    let mut prefix: Vec<LoePoint> = Vec::new();
    if !tip_suffixes.is_empty() {
        let first = &tip_suffixes[0].1;
        'outer: for (i, e) in first.iter().enumerate() {
            for (_, other) in &tip_suffixes[1..] {
                match other.get(i) {
                    Some(o) if o.slot == e.slot && o.hash == e.hash => {}
                    _ => break 'outer,
                }
            }
            prefix.push(LoePoint {
                slot: e.slot,
                hash: e.hash,
            });
        }
    }

    let plen = prefix.len();
    let suffixes = tip_suffixes
        .into_iter()
        .map(|(addr, suf)| {
            let beyond = if suf.len() > plen {
                suf.clone().split_off(plen)
            } else {
                imbl::Vector::new()
            };
            (addr, beyond)
        })
        .collect();

    SharedPrefix { prefix, suffixes }
}

// ───────────────────────────────────────────────────────────────────────────
// densityDisconnect
// ───────────────────────────────────────────────────────────────────────────

/// Per-peer input to the GDD (Haskell `ChainSyncState` + candidate suffix).
#[derive(Debug, Clone)]
pub struct GddPeer {
    pub addr: SocketAddr,
    /// Candidate suffix beyond the LoE tip (`candidateSuffixes` entry).
    pub suffix: imbl::Vector<FragEntry>,
    /// `csIdling`.
    pub idling: bool,
    /// `csLatestSlot` — `None` (Haskell `SNothing`) excludes the peer from
    /// the density comparison entirely (it has never spoken):
    /// ```haskell
    /// latestSlot <- toList (csLatestSlot state)  -- SNothing → skip peer
    /// ```
    pub latest_slot: Option<WithOrigin>,
}

/// `DensityBounds` (subset needed for the guards).
#[derive(Debug, Clone, PartialEq)]
pub struct DensityBounds {
    pub addr: SocketAddr,
    /// `lowerBound = AF.length clippedFragment`.
    pub lower_bound: u64,
    /// `upperBound = lowerBound + potentialSlots`.
    pub upper_bound: u64,
    /// `AF.lastPoint clippedFragment` — `None` = the fragment is empty, so
    /// its last point IS the LoE tip (the suffix anchor). Two peers with
    /// empty clipped fragments therefore compare EQUAL (guard 2 fails — no
    /// disconnect), exactly as in Haskell where both last points are the
    /// shared anchor.
    pub last_point: Option<(u64, [u8; 32])>,
    /// `offersMoreThanK = totalBlockCount > k` over the FULL suffix.
    pub offers_more_than_k: bool,
    /// `hasBlockAfter`.
    pub has_block_after: bool,
    /// `csIdling`.
    pub idling: bool,
}

/// Compute each eligible peer's `DensityBounds`.
///
/// Haskell (`densityDisconnect`):
/// ```haskell
/// (clippedFragment, _) = AF.splitAtSlot firstSlotAfterGenesisWindow candidateSuffix
/// hasBlockAfter = max (AF.headSlot candidateSuffix) latestSlot
///                   >= NotOrigin firstSlotAfterGenesisWindow
/// potentialSlots = if hasBlockAfter then 0
///                  else unSlotNo (firstSlotAfterGenesisWindow -
///                                 succWithOrigin (AF.headSlot clippedFragment))
/// lowerBound = AF.length clippedFragment
/// upperBound = lowerBound + potentialSlots
/// offersMoreThanK = AF.length candidateSuffix > k
/// ```
pub fn density_bounds(
    loe_tip: WithOrigin,
    sgen: u64,
    k: u64,
    peers: &[GddPeer],
) -> Vec<DensityBounds> {
    let first_slot_after_window = succ_with_origin(loe_tip).saturating_add(sgen);

    peers
        .iter()
        .filter_map(|p| {
            // Gate 0: peers that never spoke are excluded.
            let latest_slot = p.latest_slot?;

            let clipped_len = p
                .suffix
                .iter()
                .take_while(|e| e.slot < first_slot_after_window)
                .count() as u64;
            let clipped_last = if clipped_len == 0 {
                None
            } else {
                p.suffix
                    .get(clipped_len as usize - 1)
                    .map(|e| (e.slot, e.hash))
            };

            // AF.headSlot candidateSuffix: anchor (= LoE tip) when empty.
            let suffix_head_slot = match p.suffix.last() {
                Some(e) => WithOrigin::At(e.slot),
                None => loe_tip,
            };
            let has_block_after = std::cmp::max(suffix_head_slot, latest_slot)
                >= WithOrigin::At(first_slot_after_window);

            // AF.headSlot clippedFragment: anchor (= LoE tip) when empty.
            let clipped_head_slot = match clipped_last {
                Some((s, _)) => WithOrigin::At(s),
                None => loe_tip,
            };
            let potential_slots = if has_block_after {
                0
            } else {
                first_slot_after_window.saturating_sub(succ_with_origin(clipped_head_slot))
            };

            Some(DensityBounds {
                addr: p.addr,
                lower_bound: clipped_len,
                upper_bound: clipped_len + potential_slots,
                last_point: clipped_last,
                offers_more_than_k: (p.suffix.len() as u64) > k,
                has_block_after,
                idling: p.idling,
            })
        })
        .collect()
}

/// `losingPeers` — which peers to disconnect (`DensityTooLow`).
///
/// Haskell guards, in order:
/// ```haskell
/// -- 1: peer0 has committed to a chain position
/// guard $ idling0 || not (AF.null frag0) || hasBlockAfter0
/// -- 2: the two peers serve different chains
/// guard $ AF.lastPoint frag0 /= AF.lastPoint frag1
/// -- 3: peer1 is a credible reference
/// guard $ offersMoreThanK || lb0 == ub0
/// -- 4: peer1 at least as dense as peer0's best case
/// guard $ lb1 >= (if idling0 then lb0 else ub0)
/// ```
/// Deduplicated (`nubOrd`).
pub fn losing_peers(bounds: &[DensityBounds]) -> Vec<SocketAddr> {
    let mut losers: Vec<SocketAddr> = Vec::new();
    for b0 in bounds {
        if losers.contains(&b0.addr) {
            continue;
        }
        for b1 in bounds {
            if b0.addr == b1.addr {
                continue;
            }
            // Guard 1
            if !(b0.idling || b0.lower_bound > 0 || b0.has_block_after) {
                continue;
            }
            // Guard 2 — POINT comparison (slot AND hash; None = LoE tip)
            if b0.last_point == b1.last_point {
                continue;
            }
            // Guard 3
            if !(b1.offers_more_than_k || b0.lower_bound == b0.upper_bound) {
                continue;
            }
            // Guard 4
            let ceiling0 = if b0.idling {
                b0.lower_bound
            } else {
                b0.upper_bound
            };
            if b1.lower_bound >= ceiling0 {
                losers.push(b0.addr);
                break;
            }
        }
    }
    losers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn addr(n: u8) -> SocketAddr {
        format!("10.0.0.{n}:3001").parse().unwrap()
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

    fn suffix(slots: &[(u64, u8)]) -> imbl::Vector<FragEntry> {
        slots
            .iter()
            .map(|(slot, b)| FragEntry {
                slot: *slot,
                hash: h(*b),
                block_no: *slot,
            })
            .collect()
    }

    // ── sharedCandidatePrefix ────────────────────────────────────────────

    #[test]
    fn prefix_of_agreeing_peers_is_shortest_candidate() {
        let imm = Some((10, h(0xff)));
        let a = FragAnchor::Point(10, h(0xff));
        let peers = vec![
            (addr(1), frag(a, &[(11, 1), (12, 2), (13, 3)])),
            (addr(2), frag(a, &[(11, 1), (12, 2)])),
        ];
        let sp = shared_candidate_prefix(imm, &peers);
        assert_eq!(sp.prefix.len(), 2);
        assert_eq!(sp.prefix[1].slot, 12);
        // Suffixes beyond the LoE tip.
        assert_eq!(sp.suffixes[0].1.len(), 1); // peer1 has (13,3) beyond
        assert_eq!(sp.suffixes[1].1.len(), 0);
        assert_eq!(loe_tip_slot(imm, &sp.prefix), WithOrigin::At(12));
    }

    #[test]
    fn prefix_stops_at_same_slot_different_hash() {
        // Two forks diverging AT slot 12 — slot equality is NOT agreement
        // (the gdd-07 / gsm-13 class of bug).
        let imm = Some((10, h(0xff)));
        let a = FragAnchor::Point(10, h(0xff));
        let peers = vec![
            (addr(1), frag(a, &[(11, 1), (12, 2)])),
            (addr(2), frag(a, &[(11, 1), (12, 9)])),
        ];
        let sp = shared_candidate_prefix(imm, &peers);
        assert_eq!(sp.prefix.len(), 1, "divergence at slot 12 detected by hash");
        assert_eq!(sp.suffixes[0].1.len(), 1);
        assert_eq!(sp.suffixes[1].1.len(), 1);
    }

    #[test]
    fn peer_not_reaching_imm_tip_pins_prefix_empty() {
        // Haskell: fragment without the immutable tip → empty fragment at the
        // tip → common prefix collapses to empty (LoE pinned at imm tip).
        let imm = Some((10, h(0xff)));
        let peers = vec![
            (
                addr(1),
                frag(FragAnchor::Point(10, h(0xff)), &[(11, 1), (12, 2)]),
            ),
            // Anchored above the imm tip without containing it (jumper-like).
            (addr(2), frag(FragAnchor::Point(20, h(0xbb)), &[(21, 5)])),
        ];
        let sp = shared_candidate_prefix(imm, &peers);
        assert!(sp.prefix.is_empty());
        assert_eq!(loe_tip_slot(imm, &sp.prefix), WithOrigin::At(10));
        // Peer 1's full tip-suffix is beyond the (empty) prefix.
        assert_eq!(sp.suffixes[0].1.len(), 2);
        assert_eq!(sp.suffixes[1].1.len(), 0);
    }

    #[test]
    fn peer_anchored_mid_chain_contributes_entries_after_imm_tip() {
        // Peer anchored BELOW the imm tip whose fragment passes through it.
        let imm = Some((12, h(2)));
        let peers = vec![(
            addr(1),
            frag(FragAnchor::Point(10, h(0xff)), &[(11, 1), (12, 2), (13, 3)]),
        )];
        let sp = shared_candidate_prefix(imm, &peers);
        assert_eq!(sp.prefix.len(), 1);
        assert_eq!(sp.prefix[0].slot, 13);
    }

    #[test]
    fn origin_imm_tip_takes_origin_anchored_fragments() {
        let peers = vec![
            (addr(1), frag(FragAnchor::Origin, &[(1, 1), (2, 2)])),
            (addr(2), frag(FragAnchor::Origin, &[(1, 1)])),
        ];
        let sp = shared_candidate_prefix(None, &peers);
        assert_eq!(sp.prefix.len(), 1);
        assert_eq!(loe_tip_slot(None, &[]), WithOrigin::Origin);
    }

    // ── densityDisconnect ────────────────────────────────────────────────

    /// Standard scenario frame: LoE tip at slot 100, sgen = 50
    /// → window covers slots [101, 150]; first slot after = 151.
    const LOE: WithOrigin = WithOrigin::At(100);
    const SGEN: u64 = 50;
    const K: u64 = 3;

    fn peer(n: u8, suffix_slots: &[(u64, u8)], idling: bool, latest: Option<u64>) -> GddPeer {
        GddPeer {
            addr: addr(n),
            suffix: suffix(suffix_slots),
            idling,
            latest_slot: latest.map(WithOrigin::At),
        }
    }

    #[test]
    fn dense_peer_kills_sparse_idling_peer() {
        // peer0: 1 block in window then silent (idling) → ceiling = lb = 1.
        // peer1: k+1 = 4 blocks in window on a different chain → lb = 4 ≥ 1.
        let peers = vec![
            peer(0, &[(110, 1)], true, Some(110)),
            peer(
                1,
                &[(105, 2), (106, 3), (107, 4), (108, 5)],
                false,
                Some(108),
            ),
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(losing_peers(&b), vec![addr(0)]);
    }

    #[test]
    fn same_chain_peers_never_disconnect() {
        // Guard 2: identical last point in the window — one peer is simply
        // ahead; never a density disagreement.
        let peers = vec![
            peer(0, &[(105, 2)], true, Some(105)),
            peer(1, &[(105, 2), (160, 9)], false, Some(160)),
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        // peer1's clipped fragment = [(105,2)] — same last point as peer0.
        assert!(losing_peers(&b).is_empty());
    }

    #[test]
    fn same_slot_different_hash_is_a_disagreement() {
        // gdd-07: slot-only comparison would treat these as the same chain.
        let peers = vec![
            peer(0, &[(105, 1)], true, Some(105)),
            peer(
                1,
                &[(105, 2), (106, 3), (107, 4), (108, 5)],
                false,
                Some(108),
            ),
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(losing_peers(&b), vec![addr(0)]);
    }

    #[test]
    fn silent_peer_gets_benefit_of_the_doubt() {
        // Guard 1: no blocks in window, not idling, no block after → spared
        // even against a dominant peer1.
        let peers = vec![
            peer(0, &[], false, Some(100)),
            peer(
                1,
                &[(105, 2), (106, 3), (107, 4), (108, 5)],
                false,
                Some(108),
            ),
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert!(losing_peers(&b).is_empty());
    }

    #[test]
    fn never_spoke_peer_is_excluded_entirely() {
        // Gate 0 (gdd-08): csLatestSlot = SNothing → not even compared, and
        // cannot kill others.
        let peers = vec![
            peer(0, &[(110, 1)], true, Some(110)),
            GddPeer {
                addr: addr(1),
                suffix: suffix(&[(105, 2), (106, 3), (107, 4), (108, 5)]),
                idling: false,
                latest_slot: None,
            },
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(b.len(), 1, "peer1 excluded from bounds");
        assert!(losing_peers(&b).is_empty());
    }

    #[test]
    fn guard3_spares_when_reference_not_credible() {
        // peer1 offers ≤ k blocks total AND peer0 still has potential
        // (lb < ub) → not a meaningful comparison.
        let peers = vec![
            peer(0, &[(110, 1)], false, Some(110)), // not idling → has potential
            peer(1, &[(105, 2), (106, 3)], false, Some(106)), // 2 ≤ k=3
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert!(losing_peers(&b).is_empty());
    }

    #[test]
    fn guard3_fires_on_exhausted_peer0_even_with_small_reference() {
        // peer0 idling with hasBlockAfter via latestSlot → potential = 0 →
        // lb0 == ub0 → guard 3 satisfied without offersMoreThanK.
        // peer0: 1 block in window, latest_slot says it has a block beyond
        // the window (claims a sparse chain), idling.
        let peers = vec![
            peer(0, &[(110, 1)], true, Some(200)),
            peer(1, &[(105, 2), (106, 3)], false, Some(106)), // lb=2 ≥ lb0=1
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(losing_peers(&b), vec![addr(0)]);
    }

    #[test]
    fn guard4_equal_density_kills_only_idling_peer() {
        // Both serve 2-block windows on different forks; peer0 idling.
        // Haskell: "the honest chain is expected to be strictly denser …
        // equal density implies peer0 is adversarial" — but ONLY when peer0
        // has declared completion (idling).
        let idling_case = vec![
            peer(0, &[(105, 1), (106, 2)], true, Some(106)),
            peer(1, &[(105, 7), (106, 8), (200, 9)], false, Some(200)),
        ];
        // peer1 clipped = 2 blocks (105,106), hasBlockAfter → ub=lb=2 too,
        // offersMoreThanK false (3 ≤ k=3)… make peer1 credible: 4 entries.
        let idling_case = {
            let mut v = idling_case;
            v[1] = peer(
                1,
                &[(105, 7), (106, 8), (107, 9), (200, 10)],
                false,
                Some(200),
            );
            v
        };
        let b = density_bounds(LOE, SGEN, K, &idling_case);
        // peer0: lb=2, idling → ceiling 2. peer1: lb=3 ≥ 2 → kill peer0.
        // peer1 is not killed: ceiling1 = ub1 (= lb1, hasBlockAfter) = 3 >
        // lb0 = 2.
        assert_eq!(losing_peers(&b), vec![addr(0)]);

        // NOT idling → ceiling is ub0 (includes potential) → spared.
        let active_case = vec![
            peer(0, &[(105, 1), (106, 2)], false, Some(106)),
            peer(
                1,
                &[(105, 7), (106, 8), (107, 9), (200, 10)],
                false,
                Some(200),
            ),
        ];
        let b = density_bounds(LOE, SGEN, K, &active_case);
        assert!(losing_peers(&b).is_empty());
    }

    #[test]
    fn loser_listed_once_even_with_multiple_dominators() {
        let peers = vec![
            peer(0, &[(110, 1)], true, Some(110)),
            peer(
                1,
                &[(105, 2), (106, 3), (107, 4), (108, 5)],
                false,
                Some(108),
            ),
            peer(
                2,
                &[(105, 12), (106, 13), (107, 14), (108, 15)],
                false,
                Some(108),
            ),
        ];
        let b = density_bounds(LOE, SGEN, K, &peers);
        let losers = losing_peers(&b);
        assert_eq!(losers.iter().filter(|a| **a == addr(0)).count(), 1);
    }

    #[test]
    fn bounds_math_matches_haskell_formulae() {
        // peer: 2 blocks in window at 105,110; nothing after; not idling;
        // latest = 110.
        // firstSlotAfter = 101 + 50 = 151.
        // clipped = both; lb = 2. headSlot clipped = 110 → succ = 111.
        // potential = 151 − 111 = 40. ub = 42. hasBlockAfter = false.
        let peers = vec![peer(0, &[(105, 1), (110, 2)], false, Some(110))];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(b[0].lower_bound, 2);
        assert_eq!(b[0].upper_bound, 42);
        assert!(!b[0].has_block_after);
        assert_eq!(b[0].last_point, Some((110, h(2))));

        // Same peer with a block AT the window boundary (151) on its suffix:
        // hasBlockAfter, potential = 0, clipped still 2.
        let peers = vec![peer(0, &[(105, 1), (110, 2), (151, 3)], false, Some(151))];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(b[0].lower_bound, 2);
        assert_eq!(b[0].upper_bound, 2);
        assert!(b[0].has_block_after);

        // Empty suffix, latest within window, not idling: lb=0,
        // potential = 151 − succ(100) = 50 → ub = 50, last_point None.
        let peers = vec![peer(0, &[], false, Some(120))];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert_eq!(b[0].lower_bound, 0);
        assert_eq!(b[0].upper_bound, 50);
        assert_eq!(b[0].last_point, None);
    }

    #[test]
    fn empty_clipped_fragments_share_the_loe_tip_point() {
        // Both peers idle with nothing beyond the LoE tip: their clipped
        // last points are both the anchor (None) → guard 2 fails → no kill,
        // even though guard 4 (lb1=0 ≥ lb0=0) would fire.
        let peers = vec![peer(0, &[], true, Some(100)), peer(1, &[], true, Some(100))];
        let b = density_bounds(LOE, SGEN, K, &peers);
        assert!(losing_peers(&b).is_empty());
    }
}
