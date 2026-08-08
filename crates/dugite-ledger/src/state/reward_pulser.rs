//! The RUPD pulser — Haskell `Cardano.Ledger.Shelley.Rules.Rupd` and
//! `LedgerState.PulsingReward`.
//!
//! # Why this exists
//!
//! dugite computed the entire reward update in one pass at the epoch boundary.
//! Haskell freezes its inputs **mid-epoch** at `4k/f`, spreads the member-reward
//! fold across the remaining blocks, and applies the result at the next
//! boundary. Two consequences that are NOT merely stylistic:
//!
//! * **`nesRu` is ledger state.** `NewEpochState[4]` carries the pulser, so two
//!   nodes that disagree about whether it is `SNothing`, `Pulsing` or `Complete`
//!   at a given slot disagree about the ledger state itself, even when the
//!   eventual rewards match.
//! * **A boundary with no pulser applies NOTHING** (#1072). See
//!   [`RewardTiming`] and the `SNothing` arm of NEWEPOCH:
//!
//!   ```haskell
//!   -- NewEpoch.hs:161, identically ConwayNewEpoch.hs:172
//!   es' <- case ru of
//!     SNothing -> pure es          -- no deltaR, no deltaT, no rewards, no fee drain
//!     SJust p@(Pulsing _ _) -> ... completeRupd p ... updateRewards
//!     SJust (Complete ru') -> updateRewards es eNo ru'
//!   ```
//!
//!   dugite applied a full reward update unconditionally, which diverges
//!   permanently whenever no block lands in `(first + 4k/f, epoch_end]`.
//!
//! # What is frozen where
//!
//! `startStep` freezes `casReserves`, `accounts`, `ssStakeGo`, `prevPParams`,
//! `ssFee` and `BlocksMade`. Of those only `accounts` is mutable mid-epoch;
//! the rest are written solely by SNAP/EPOCH/NEWEPOCH and so cannot move
//! between the freeze and the boundary. Reserves in particular cannot: MIR
//! certificates QUEUE into `dsIRewards` and drain at the boundary, and
//! `applyRUpd` is itself the boundary. That is why dugite's previous
//! boundary-time read produced the same numbers — it was right by accident,
//! and this module makes it right by construction.

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::time::SlotNo;
use dugite_primitives::value::Lovelace;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Haskell's "Goldilocks labeling of when to do the reward calculation".
///
/// ```haskell
/// -- Rupd.hs:110-116
/// data RewardTiming = RewardsTooEarly | RewardsJustRight | RewardsTooLate
///
/// determineRewardTiming currentSlot startAfterSlot endSlot
///   | currentSlot > endSlot         = RewardsTooLate
///   | currentSlot <= startAfterSlot = RewardsTooEarly
///   | otherwise                     = RewardsJustRight
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewardTiming {
    /// Before the stability point — the pulser stays unset (`SNothing`).
    TooEarly,
    /// The pulsing window: start the pulser, or take one more pulse.
    JustRight,
    /// Past the deadline — force the pulser to completion so downstream tools
    /// see the reward update before the boundary rolls over.
    TooLate,
}

/// The two window edges, derived from the epoch's first slot.
///
/// ```haskell
/// -- Rupd.hs:121-130
/// sr <- asks randomnessStabilisationWindow
/// let slot = epochInfoFirst ei e +* Duration sr     -- startAfterSlot
/// return (..., slot, slot +* Duration sr, ...)      -- endSlot = first + 2*sr
/// ```
///
/// Note both edges are derived from the SAME `sr`, so the window is
/// `[first+sr, first+2sr]` — one stabilisation window wide, not two.
///
/// On a devnet with `epochLength=400`, `k=40`, `f=0.5`: `sr = 4k/f = 320`, so
/// `start_after = 320` and `end = 640`. **`end` exceeds the epoch length**,
/// which means [`RewardTiming::TooLate`] is UNREACHABLE there and the pulser is
/// completed instead by NEWEPOCH's `Pulsing -> completeRupd` arm. Any test that
/// needs the force-complete path must construct the window directly rather than
/// expecting the devnet to produce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewardWindow {
    pub start_after: SlotNo,
    pub end: SlotNo,
}

impl RewardWindow {
    /// `start_after = first + sr`, `end = first + 2*sr`.
    pub fn new(epoch_first_slot: u64, randomness_stabilisation_window: u64) -> Self {
        let sr = randomness_stabilisation_window;
        RewardWindow {
            start_after: SlotNo(epoch_first_slot.saturating_add(sr)),
            end: SlotNo(epoch_first_slot.saturating_add(sr).saturating_add(sr)),
        }
    }

    /// Classify a slot. Mirrors `determineRewardTiming` exactly, including the
    /// asymmetric comparisons: `>` for the late edge, `<=` for the early one.
    ///
    /// The asymmetry is load-bearing. At exactly `start_after` the answer is
    /// TooEarly (so the freeze happens at the first slot STRICTLY after the
    /// stabilisation window), and at exactly `end` it is still JustRight.
    /// Reversing either comparison shifts the freeze instant by one slot and
    /// changes which certificates are inside the frozen `accounts` set.
    pub fn classify(&self, current: SlotNo) -> RewardTiming {
        if current.0 > self.end.0 {
            RewardTiming::TooLate
        } else if current.0 <= self.start_after.0 {
            RewardTiming::TooEarly
        } else {
            RewardTiming::JustRight
        }
    }
}

/// ```haskell
/// -- PulsingReward.hs:114
/// pulseSize = max 1 (ceiling (numStakeCreds %. (knownNonZero @4 `mulNonZero` k)))
/// ```
///
/// How many stake credentials to reward per pulse: the credential count divided
/// by `4k`, since roughly `4k` blocks are produced during the pulsing window.
/// Never zero — a zero pulse size would never terminate.
pub fn pulse_size(num_stake_creds: u64, security_param: u64) -> u64 {
    let denom = security_param.saturating_mul(4).max(1);
    num_stake_creds.div_ceil(denom).max(1)
}

/// The monetary half of `startStep`: everything derivable before any per-pool
/// or per-credential work.
///
/// ```haskell
/// -- PulsingReward.hs:117-141
/// Coin reserves = acnt ^. casReservesL
/// pr = es ^. prevPParamsEpochStateL
/// deltaR1 = rationalToCoinViaFloor $ min 1 eta * unboundRational (pr ^. ppRhoL) * fromIntegral reserves
/// d = unboundRational (pr ^. ppDG)
/// expectedBlocks = floor $ (1 - d) * unboundRational (activeSlotVal asc) * fromIntegral (unEpochSize slotsPerEpoch)
/// eta | d >= 0.8 = 1
///     | otherwise = blocksMade % expectedBlocks
/// Coin rPot = ssFee ss <> deltaR1
/// deltaT1 = floor $ unboundRational (pr ^. ppTauL) * fromIntegral rPot
/// _R = Coin $ rPot - deltaT1
/// ```
///
/// Extracted so the pulser and the current single-pass path share ONE
/// implementation. A second copy of this arithmetic is the N-copies trap that
/// produced #985, #1015 and #977 here — the copy nobody edits is the one that
/// goes wrong.
///
/// All arithmetic stays in exact `Rat`: Haskell computes these in `Rational`
/// and floors once per stage (`rationalToCoinViaFloor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonetaryStep {
    /// `deltaR1` — monetary expansion drawn from reserves.
    pub delta_r1: u64,
    /// `deltaT1` — the treasury cut, `floor(tau * rPot)`.
    pub delta_t1: u64,
    /// `_R = rPot - deltaT1`, the pot AFTER the treasury cut. Becomes
    /// `rewardPotNM` (#1067) and the numerator of every pool's `maxPool'`.
    pub r: u64,
    /// `expectedBlocks`, clamped to >= 1; `0` when the `d >= 4/5` branch made
    /// it irrelevant. Retained for diagnostics — a value of 1 means the clamp
    /// fired, which distorts `eta`.
    pub expected_blocks: u64,
}

/// Compute [`MonetaryStep`] from the inputs `startStep` freezes.
///
/// `d >= 4/5` is tested in exact rational form (`5*d_num >= 4*d_den`), matching
/// Haskell's `d >= 0.8` on a `UnitInterval`. A float comparison here is the
/// #629 defect.
#[allow(clippy::too_many_arguments)]
pub fn start_step_monetary(
    rho: (u64, u64),
    tau: (u64, u64),
    d: (u64, u64),
    active_slot_coeff: (u64, u64),
    reserves: u64,
    epoch_fees: u64,
    blocks_made: u64,
    slots_per_epoch: u64,
) -> MonetaryStep {
    use super::Rat;

    let rho_r = Rat::from_i128(rho.0 as i128, rho.1.max(1) as i128);
    let (d_num, d_den) = (d.0 as i128, d.1.max(1) as i128);

    // Overlay gate: `d >= 4/5` <=> `5*d_num >= 4*d_den`, exact.
    let d_ge_4_5 = 5 * d_num >= 4 * d_den;

    let (expansion, expected_blocks) = if d_ge_4_5 {
        // eta = 1: full expansion, no block-production adjustment.
        (
            rho_r.mul(&Rat::from_i128(reserves as i128, 1)).floor_u64(),
            0,
        )
    } else {
        let one_minus_d = Rat::from_i128(d_den - d_num, d_den);
        let f = Rat::from_i128(
            active_slot_coeff.0 as i128,
            active_slot_coeff.1.max(1) as i128,
        );
        let slots = Rat::from_i128(slots_per_epoch as i128, 1);
        let expected = one_minus_d.mul(&f).mul(&slots).floor_u64().max(1);
        // Capping blocks_made at expected is equivalent to `min 1 eta`.
        let effective = blocks_made.min(expected);
        (
            rho_r
                .mul(&Rat::from_i128(reserves as i128, 1))
                .mul(&Rat::from_i128(effective as i128, expected as i128))
                .floor_u64(),
            expected,
        )
    };

    let r_pot = expansion + epoch_fees;
    let delta_t1 = Rat::from_i128(tau.0 as i128, tau.1.max(1) as i128)
        .mul(&Rat::from_i128(r_pot as i128, 1))
        .floor_u64();

    MonetaryStep {
        delta_r1: expansion,
        delta_t1,
        r: r_pot - delta_t1,
        expected_blocks,
    }
}

/// Haskell `PulsingRewUpdate` — `NewEpochState[4]` (`nesRu`).
///
/// ```haskell
/// data PulsingRewUpdate
///   = Pulsing !RewardSnapShot !Pulser
///   | Complete !RewardUpdate
///
/// encCBOR (Pulsing s p) = encode (Sum Pulsing 0 !> To s !> To p)
/// encCBOR (Complete r)  = encode (Sum Complete 1 !> To r)
///
/// instance ToJSON PulsingRewUpdate where
///   toJSON = \case
///     Pulsing _ _ -> Null          -- renders the SAME as SNothing
///     Complete ru -> toJSON ru
/// ```
///
/// This is LEDGER STATE, not a query convenience: two nodes that disagree
/// about which constructor holds at a given slot disagree about
/// `NewEpochState`, even when the eventual rewards match.
///
/// `Pulsing` renders as JSON `null`, identically to `SNothing` — which is why
/// #1071's observed divergence rate was ~20% (the `Complete` window) rather
/// than ~80% (the whole post-4k/f span). On the CBOR wire they are distinct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PulsingRewUpdate {
    /// Inputs frozen, fold still outstanding.
    ///
    /// Carries the frozen snapshot so the boundary can finish without
    /// re-reading anything — and so a tick that crosses a boundary while past
    /// the NEW epoch's mark keeps the PRE-rotation environment. That is the F5
    /// case a bool could not express: `Tick.hs`'s `bheadTransition` builds
    /// `RupdEnv bprev es` from `nes0`, pre-NEWEPOCH.
    Pulsing(Box<RewardSnapShot>),
    /// Fold finished; ready to apply at the boundary.
    Complete(Box<RewardSnapShot>),
}

impl PulsingRewUpdate {
    /// The frozen snapshot, whichever constructor holds.
    pub fn snapshot(&self) -> &RewardSnapShot {
        match self {
            PulsingRewUpdate::Pulsing(s) | PulsingRewUpdate::Complete(s) => s,
        }
    }

    /// `completeStep` — force the pulser to completion. Idempotent on
    /// `Complete`, matching `completeRupd (Complete x) = pure (x, mempty)`.
    pub fn complete(self) -> Self {
        match self {
            PulsingRewUpdate::Pulsing(s) => PulsingRewUpdate::Complete(s),
            done @ PulsingRewUpdate::Complete(_) => done,
        }
    }

    /// Whether a boundary reached now would apply a reward update.
    ///
    /// True for BOTH constructors — `NewEpoch.hs:163-166` completes a
    /// `Pulsing` and applies it. Only `SNothing`, i.e. `Option::None`, applies
    /// nothing. Naming this explicitly stops the #1072 mistake being made
    /// again in the opposite direction: skipping the update while a pulser is
    /// merely unfinished would be just as wrong as applying one with none.
    pub fn applies_at_boundary(&self) -> bool {
        true
    }

    /// cardano-cli renders `Pulsing` as JSON `null`, like `SNothing`.
    pub fn is_json_visible(&self) -> bool {
        matches!(self, PulsingRewUpdate::Complete(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Devnet geometry: `epochLength=400`, `k=40`, `f=0.5` => `sr = 4k/f = 320`.
    fn devnet_window(epoch_first: u64) -> RewardWindow {
        RewardWindow::new(epoch_first, 320)
    }

    #[test]
    fn window_edges_are_first_plus_sr_and_first_plus_2sr() {
        let w = RewardWindow::new(800, 320);
        assert_eq!(w.start_after, SlotNo(1120));
        assert_eq!(w.end, SlotNo(1440));
    }

    /// The comparisons are asymmetric on purpose: `<=` early, `>` late.
    /// Disarming either boundary (flipping to `<` or `>=`) turns this red.
    #[test]
    fn classification_boundaries_match_determine_reward_timing() {
        let w = RewardWindow::new(0, 320);

        // strictly before, and AT, start_after => TooEarly
        assert_eq!(w.classify(SlotNo(0)), RewardTiming::TooEarly);
        assert_eq!(w.classify(SlotNo(319)), RewardTiming::TooEarly);
        assert_eq!(w.classify(SlotNo(320)), RewardTiming::TooEarly);

        // the first slot STRICTLY after start_after is where the freeze happens
        assert_eq!(w.classify(SlotNo(321)), RewardTiming::JustRight);
        assert_eq!(w.classify(SlotNo(639)), RewardTiming::JustRight);
        // AT end is still JustRight
        assert_eq!(w.classify(SlotNo(640)), RewardTiming::JustRight);

        // strictly after end => TooLate
        assert_eq!(w.classify(SlotNo(641)), RewardTiming::TooLate);
    }

    /// On the devnet the force-complete arm cannot be reached, because the
    /// late edge lies beyond the end of the epoch. Recorded as a test so the
    /// validation plan cannot quietly assume the gate exercises it.
    #[test]
    fn devnet_cannot_reach_the_force_complete_arm() {
        const EPOCH_LENGTH: u64 = 400;
        let w = devnet_window(0);
        assert!(
            w.end.0 > EPOCH_LENGTH,
            "devnet late edge {} must exceed epochLength {EPOCH_LENGTH}; if this \
             ever becomes false the force-complete arm IS reachable on the gate \
             and its unit-only coverage can be revisited",
            w.end.0
        );
        // Every in-epoch slot is TooEarly or JustRight, never TooLate.
        for s in 0..EPOCH_LENGTH {
            assert_ne!(w.classify(SlotNo(s)), RewardTiming::TooLate, "slot {s}");
        }
    }

    /// #1072: an epoch in which no block lands after the stabilisation window
    /// never leaves TooEarly, so no pulser is ever started and the boundary
    /// must apply NOTHING.
    #[test]
    fn no_block_after_the_window_means_no_pulser_was_ever_started() {
        let w = devnet_window(0);
        // Blocks at slots 10, 100, 300 — an outage covers 300..400.
        let block_slots = [10u64, 100, 300];
        assert!(
            block_slots
                .iter()
                .all(|s| w.classify(SlotNo(*s)) == RewardTiming::TooEarly),
            "no block in this set may start the pulser"
        );
    }

    /// Haskell picks a PV<3 credential's single reward with
    /// `Set.deleteFindMin` over `Ord Reward` (`Rewards.hs:176-181`):
    /// LeaderReward sorts before MemberReward, then ascending pool id.
    ///
    /// dugite stores entries in a `Vec`, so the ORDER must be reproduced
    /// explicitly. Getting it wrong pays the wrong reward to any credential
    /// earning from more than one source — a mainnet Shelley-era divergence,
    /// silent because both choices are plausible amounts.
    #[test]
    fn reward_entry_ord_is_leader_first_then_pool_id() {
        let p = |b: u8| Hash28::from_bytes([b; 28]);
        let leader_hi = RewardEntry {
            is_member: false,
            pool_id: p(0xFF),
            amount: 1,
        };
        let member_lo = RewardEntry {
            is_member: true,
            pool_id: p(0x00),
            amount: 999,
        };
        // Leader wins even with the LARGER pool id and a smaller amount:
        // the discriminator dominates, and amount is not part of the key.
        assert!(leader_hi.ord_key() < member_lo.ord_key());

        let a = RewardEntry {
            is_member: true,
            pool_id: p(0x01),
            amount: 5,
        };
        let b = RewardEntry {
            is_member: true,
            pool_id: p(0x02),
            amount: 5,
        };
        assert!(a.ord_key() < b.ord_key(), "ties break on ASCENDING pool id");

        // `deleteFindMin` semantics over a mixed set.
        let mut v = [member_lo.clone(), b.clone(), leader_hi.clone(), a.clone()];
        v.sort_by_key(|e| e.ord_key());
        assert_eq!(v[0], leader_hi, "the minimum must be the leader reward");
    }

    /// Mainnet-shaped: rho=3/1000, tau=1/5, d=0, f=1/20, epoch=432000.
    ///
    /// Values computed by hand from the Haskell formula rather than by running
    /// this function — a test that re-derives its expectation from the code
    /// under test asserts only that the code is deterministic.
    ///
    ///   expectedBlocks = floor(1 * 1/20 * 432000)        = 21600
    ///   deltaR1        = floor(3/1000 * 1e15 * 21600/21600) = 3_000_000_000_000
    ///   rPot           = deltaR1 + fees                  = 3_000_000_001_000
    ///   deltaT1        = floor(1/5 * rPot)               =   600_000_000_200
    ///   _R             = rPot - deltaT1                  = 2_400_000_000_800
    #[test]
    fn monetary_step_matches_hand_computed_haskell() {
        let m = start_step_monetary(
            (3, 1000),             // rho
            (1, 5),                // tau
            (0, 1),                // d
            (1, 20),               // f
            1_000_000_000_000_000, // reserves
            1_000,                 // fees
            21_600,                // blocks made == expected => eta = 1
            432_000,               // slots per epoch
        );
        assert_eq!(m.expected_blocks, 21_600);
        assert_eq!(m.delta_r1, 3_000_000_000_000);
        assert_eq!(m.delta_t1, 600_000_000_200);
        assert_eq!(m.r, 2_400_000_000_800);
        assert_eq!(
            m.r + m.delta_t1,
            m.delta_r1 + 1_000,
            "rPot must be conserved"
        );
    }

    /// `d >= 4/5` short-circuits eta to 1 and skips the expected-blocks path.
    /// Tested in EXACT rational form: `d = 4/5` is the boundary and must take
    /// the overlay branch (`>=`, not `>`). A float comparison here is #629.
    #[test]
    fn overlay_gate_is_exact_at_four_fifths() {
        let at = start_step_monetary((3, 1000), (1, 5), (4, 5), (1, 20), 1_000_000, 0, 0, 432_000);
        assert_eq!(at.expected_blocks, 0, "d == 4/5 must take the eta=1 branch");
        assert_eq!(at.delta_r1, 3_000, "full expansion, unadjusted by blocks");

        // Just below the boundary: eta applies, and with 0 blocks made the
        // expansion collapses to 0 — a materially different answer, which is
        // what makes the exact comparison load-bearing.
        let below = start_step_monetary(
            (3, 1000),
            (1, 5),
            (79, 100),
            (1, 20),
            1_000_000,
            0,
            0,
            432_000,
        );
        assert!(below.expected_blocks > 0);
        assert_eq!(below.delta_r1, 0);
    }

    /// `expectedBlocks` is clamped to >= 1 so `eta` cannot divide by zero.
    #[test]
    fn expected_blocks_never_zero_on_the_eta_path() {
        // (1 - d) * f * slots rounds to 0 for a tiny epoch.
        let m = start_step_monetary((3, 1000), (1, 5), (0, 1), (1, 20), 1_000_000, 0, 0, 1);
        assert_eq!(m.expected_blocks, 1);
    }

    fn sample_snapshot() -> RewardSnapShot {
        RewardSnapShot {
            fees: Lovelace(1_000),
            protocol_version: 10,
            non_myopic: super::super::non_myopic::NonMyopic::default(),
            delta_r1: Lovelace(3_000_000_000_000),
            r: Lovelace(2_400_000_000_800),
            delta_t1: Lovelace(600_000_000_200),
            likelihoods: HashMap::new(),
            leaders: HashMap::new(),
            free_vars: FreeVars {
                addrs_rew: None,
                total_stake: 1_000_000,
                prot_ver: 10,
            },
        }
    }

    /// BOTH constructors apply at the boundary — `NewEpoch.hs:163-166`
    /// completes a `Pulsing` and applies it. Only `SNothing` applies nothing.
    ///
    /// This pins #1072 from the OTHER side: having fixed "applies with no
    /// pulser", the symmetric mistake is "skips because the pulser is merely
    /// unfinished". Both are divergences.
    #[test]
    fn both_constructors_apply_at_the_boundary_only_none_does_not() {
        let p = PulsingRewUpdate::Pulsing(Box::new(sample_snapshot()));
        let c = PulsingRewUpdate::Complete(Box::new(sample_snapshot()));
        assert!(p.applies_at_boundary());
        assert!(c.applies_at_boundary());
        // `SNothing` is `Option::None`; there is no constructor for it, which
        // is the type-level statement that "no pulser" is not a pulser state.
        let none: Option<PulsingRewUpdate> = None;
        assert!(none.is_none());
    }

    /// `completeStep` is idempotent on `Complete`
    /// (`completeRupd (Complete x) = pure (x, mempty)`), and preserves the
    /// frozen snapshot rather than recomputing it.
    #[test]
    fn complete_is_idempotent_and_preserves_the_frozen_snapshot() {
        let p = PulsingRewUpdate::Pulsing(Box::new(sample_snapshot()));
        let once = p.clone().complete();
        assert!(matches!(once, PulsingRewUpdate::Complete(_)));
        assert_eq!(
            once.snapshot(),
            p.snapshot(),
            "completing must not alter the freeze"
        );
        let twice = once.clone().complete();
        assert_eq!(twice, once, "completeStep must be idempotent");
    }

    /// `Pulsing` renders as JSON `null`, identically to `SNothing`
    /// (`RewardUpdate.hs:359-365`). This is why #1071 measured ~20% and not
    /// ~80%: only the `Complete` window is JSON-observable.
    #[test]
    fn pulsing_is_json_invisible_like_snothing() {
        assert!(!PulsingRewUpdate::Pulsing(Box::new(sample_snapshot())).is_json_visible());
        assert!(PulsingRewUpdate::Complete(Box::new(sample_snapshot())).is_json_visible());
    }

    #[test]
    fn pulse_size_is_creds_over_4k_never_zero() {
        // mainnet-ish: 1.3M creds, k=2160 => 4k = 8640 => ceil(1_300_000/8640) = 151
        assert_eq!(pulse_size(1_300_000, 2160), 151);
        // devnet: a handful of creds, k=40 => 4k = 160 => ceil => 1
        assert_eq!(pulse_size(5, 40), 1);
        // never zero even with no credentials
        assert_eq!(pulse_size(0, 2160), 1);
        // exact multiples do not round up
        assert_eq!(pulse_size(8640, 2160), 1);
        assert_eq!(pulse_size(8641, 2160), 2);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 1(a): the frozen inputs
// ─────────────────────────────────────────────────────────────────────────────

/// One entry of Haskell's `Map (Credential Staking) (Set Reward)`.
///
/// dugite keeps rewards UNAGGREGATED until `filterRewards` runs, because at
/// PV<3 a credential earning from several sources is paid only ONE reward,
/// selected by `Set.deleteFindMin` — Ord on `Reward`, which orders
/// `LeaderReward` before `MemberReward` and then by ascending pool id
/// (`Rewards.hs:176-181`). A plain `u64` total cannot express that choice.
///
/// This mirrors the tuple the existing reward loop already builds, so the
/// established `filterRewards` implementation keeps working unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardEntry {
    /// `false` = LeaderReward, `true` = MemberReward. Named for the Ord.
    pub is_member: bool,
    pub pool_id: Hash28,
    pub amount: u64,
}

impl RewardEntry {
    /// Haskell's `Ord Reward` key: leader before member, then pool id.
    ///
    /// `Set.deleteFindMin` picks the minimum under exactly this order, so any
    /// container dugite uses must reproduce it or the mainnet Shelley-era
    /// replay diverges on which reward a multi-source credential is paid.
    pub fn ord_key(&self) -> (bool, Hash28) {
        (self.is_member, self.pool_id)
    }
}

/// Haskell `FreeVars` — the per-pool data the member-reward fold closes over.
///
/// ```haskell
/// data FreeVars = FreeVars
///   { fvAddrsRew :: !(Set (Credential Staking))
///   , fvTotalStake :: !Coin
///   , fvProtVer :: !ProtVer
///   , fvPoolRewardInfo :: !(VMap VB VB (KeyHash StakePool) PoolRewardInfo) }
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreeVars {
    /// `fvAddrsRew`. `None` above PV6, where the prefilter short-circuits
    /// (`Rewards.hs:315`) — capturing ~1.3M mainnet credentials into persisted
    /// state for a set nothing reads would be pure cost.
    pub addrs_rew: Option<std::collections::HashSet<Hash32>>,
    /// `fvTotalStake` = `circulation es maxSupply` = `maxSupply - reserves`,
    /// read at the FREEZE instant, not at the boundary.
    pub total_stake: u64,
    pub prot_ver: u64,
}

/// Haskell `RewardSnapShot` — everything `startStep` freezes, so the boundary
/// only has to finish the fold.
///
/// ```haskell
/// data RewardSnapShot = RewardSnapShot
///   { rewFees, rewProtocolVersion, rewNonMyopic, rewDeltaR1, rewR, rewDeltaT1
///   , rewLikelihoods :: !(VMap VB VB (KeyHash StakePool) Likelihood)
///   , rewLeaders     :: !(Map (Credential Staking) (Set Reward)) }
/// ```
///
/// `rewR` is `_R = rPot - deltaT1`, the pot AFTER the treasury cut — the same
/// value that becomes `rewardPotNM` (#1067).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RewardSnapShot {
    pub fees: Lovelace,
    pub protocol_version: u64,
    pub non_myopic: super::non_myopic::NonMyopic,
    pub delta_r1: Lovelace,
    pub r: Lovelace,
    pub delta_t1: Lovelace,
    pub likelihoods: HashMap<Hash28, super::non_myopic::Likelihood>,
    /// `rewLeaders` — leader rewards, computed at the freeze and merged with
    /// the member fold by `completeRupd`.
    pub leaders: HashMap<Hash32, Vec<RewardEntry>>,
    pub free_vars: FreeVars,
}

/// Haskell `PoolRewardInfo` — the per-pool terms computed ONCE, then read by
/// the per-credential fold.
///
/// ```haskell
/// data PoolRewardInfo = PoolRewardInfo
///   { poolRelativeStake :: !StakeShare
///   , poolPot           :: !Coin
///   , poolPs            :: !PoolParams
///   , poolBlocks        :: !Natural
///   , poolLeaderReward  :: !LeaderOnlyReward
///   }
/// ```
///
/// This exists so the reward fold can be **credential-major**. dugite folded
/// pool-major with an inner delegator loop, which cannot be chunked to match
/// upstream: the pulser's work queue is a set of `Credential 'Staking` (see
/// `tests/fixtures/nesru/pulsing.hex`, whose remaining set is `8200581c…`),
/// and one pool can hold hundreds of thousands of delegators, so a pool-granular
/// "pulse" would be unbounded.
///
/// Splitting the computation this way is also what makes the frozen/live
/// distinction enforceable: every term here is derived from the `startStep`
/// snapshot and is `&self` at fold time, so a per-credential recomputation
/// against mutating state does not typecheck.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolRewardInfo {
    /// The pool these terms belong to.
    pub pool_id: Hash28,
    /// `ssPoolStake` — the denominator of every member's share.
    pub pool_active_stake: u64,
    /// `poolPot` — the pool's whole reward before the operator's cut.
    pub pool_reward: u64,
    /// `ppCost`, and the margin as an exact fraction.
    pub cost: u64,
    pub margin_num: i128,
    pub margin_den: i128,
    /// Owners are paid through the leader reward, never as members.
    pub owner_set: std::collections::HashSet<Hash32>,
    /// `poolLeaderReward`, already gated by the pv<=6 registration prefilter.
    /// `None` when the leader reward is dropped or zero.
    pub leader: Option<(Hash32, u64)>,
}

impl PoolRewardInfo {
    /// Haskell `rewardOnePoolMember` — a member's share of the pool pot.
    ///
    /// ```haskell
    /// rewardOnePoolMember pp totalStake (RewardInfo ...) hk (StakeShare t) =
    ///   ... memberRew poolPot poolPs (StakeShare sigma) (StakeShare t)
    /// memberRew (Coin f') pool (StakeShare m) (StakeShare sigma)
    ///   | f' <= c = mempty
    ///   | otherwise = rationalToCoinViaFloor $
    ///       fromIntegral (f' - c) * (1 - m') * sigma / m
    /// ```
    ///
    /// The `f' <= c` short-circuit is load-bearing and NOT the same as clamping
    /// a negative remainder to zero: it must be tested before the subtraction.
    pub fn member_reward(&self, member_stake: u64) -> u64 {
        use super::Rat;
        if member_stake == 0 || self.pool_active_stake == 0 || self.pool_reward <= self.cost {
            return 0;
        }
        let remainder = self.pool_reward - self.cost;
        let one_minus_margin = Rat::from_i128(self.margin_den - self.margin_num, self.margin_den);
        let member_frac = Rat::from_i128(member_stake as i128, self.pool_active_stake as i128);
        Rat::from_i128(remainder as i128, 1)
            .mul(&one_minus_margin)
            .mul(&member_frac)
            .floor_u64()
    }
}
