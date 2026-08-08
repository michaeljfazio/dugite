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
    /// `fvTotalStake` — `maxSupply - casReserves` AT THE FREEZE INSTANT.
    ///
    /// Haskell freezes this into `FreeVars`, and the captured mainnet-shaped
    /// pulser carries it verbatim (`fvTotalStake = 54003425994184880` in
    /// `tests/fixtures/nesru/pulsing.hex`). dugite recomputed it at the
    /// boundary instead.
    ///
    /// Those agree everywhere reserves are immobile mid-epoch (§3.1), which is
    /// everywhere EXCEPT a boundary whose era translation moves reserves —
    /// i.e. Shelley→Allegra, where `returnRedeemAddrsToReserves` credits the
    /// unredeemed AVVM coin BEFORE the reward update is applied. dugite
    /// patched that one case with `pending_avvm_return`, a bespoke correction
    /// carried in `EpochSubState` and subtracted back off at exactly one
    /// boundary.
    ///
    /// Freezing the value removes the patch AND its unstated invariant. Note
    /// this is not only about `deltaR1`: `total_stake` is `sigma`'s
    /// denominator, so it reaches `maxPool'` and every pool's likelihood. A
    /// version of this change that froze only the monetary terms would have
    /// left the reward DISTRIBUTION reading post-AVVM reserves while the pot
    /// read pre-AVVM — worse than the patch it removed.
    pub total_stake: u64,
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
    max_lovelace_supply: u64,
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
        // `fvTotalStake` — frozen here rather than recomputed at the boundary,
        // which is what makes `pending_avvm_return` unnecessary.
        total_stake: max_lovelace_supply.saturating_sub(reserves),
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

    /// `maxLovelaceSupply`, mainnet.
    const MAX_SUPPLY: u64 = 45_000_000_000_000_000;

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
            MAX_SUPPLY,            // maxLovelaceSupply
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
        let at = start_step_monetary(
            (3, 1000),
            (1, 5),
            (4, 5),
            (1, 20),
            1_000_000,
            0,
            0,
            432_000,
            MAX_SUPPLY,
        );
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
            MAX_SUPPLY,
        );
        assert!(below.expected_blocks > 0);
        assert_eq!(below.delta_r1, 0);
    }

    /// `expectedBlocks` is clamped to >= 1 so `eta` cannot divide by zero.
    #[test]
    fn expected_blocks_never_zero_on_the_eta_path() {
        // (1 - d) * f * slots rounds to 0 for a tiny epoch.
        let m = start_step_monetary(
            (3, 1000),
            (1, 5),
            (0, 1),
            (1, 20),
            1_000_000,
            0,
            0,
            1,
            MAX_SUPPLY,
        );
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

/// Everything the member fold reads. All of it frozen at `startStep`.
///
/// Bundled into one borrow so a pulse cannot be handed a mix of frozen and
/// live inputs — the defect shape behind #988 (a reader mixing frozen with
/// live terms) and #949 (a term fixed in the live path while the frozen path
/// kept the old value).
pub struct MemberFoldCtx<'a, F: Fn(&Hash32) -> bool> {
    /// Per-pool terms, keyed by pool.
    pub table: &'a HashMap<Hash28, PoolRewardInfo>,
    /// `ssDelegations` — credential to pool.
    pub delegations: &'a HashMap<Hash32, Hash28>,
    /// `ssStake` — active stake per credential.
    pub stake: &'a HashMap<Hash32, Lovelace>,
    /// Protocol major version in force BEFORE the boundary.
    pub pv_major: u64,
    /// `hk ∈ addrsRew`, the pv<=6 member prefilter.
    pub registered: F,
}

/// A reward entry: `(is_member, producing_pool, amount)`.
pub type RewardEntryTriple = (bool, Hash28, u64);

/// Haskell's `RewardPulser` — a work queue of credentials plus the answer so far.
///
/// ```haskell
/// data RewardPulser c ... = RSLP
///   !Int                                 -- pulse size
///   !(FreeVars c)                        -- frozen inputs
///   !(VMap.KVVector ...)                 -- the balance: work remaining
///   !(RewardAns c)                       -- the answer accumulated so far
/// ```
///
/// The queue is **sorted**, which is not cosmetic. Upstream's balance is a
/// `Set (Credential 'Staking)` and pulses consume it in `Ord` order; dugite's
/// source is a `HashMap`, whose iteration order varies per process. An unsorted
/// queue would make the split between "already folded" and "still pending"
/// differ across restarts and rollbacks — two nodes, or one node before and
/// after a restart, would disagree about `nesRu` even while computing identical
/// rewards. Sorting also makes `remaining()` directly encodable as the wire
/// arm's tag-258 set.
#[derive(Clone, Debug)]
pub struct RewardFold {
    queue: Vec<Hash32>,
    cursor: usize,
    acc: HashMap<Hash32, Vec<RewardEntryTriple>>,
}

impl RewardFold {
    /// Freeze the work queue from the delegation map.
    pub fn new(delegations: &HashMap<Hash32, Hash28>) -> Self {
        let mut queue: Vec<Hash32> = delegations.keys().copied().collect();
        queue.sort_unstable();
        RewardFold {
            queue,
            cursor: 0,
            acc: HashMap::new(),
        }
    }

    /// `pulseSize = max 1 (ceil (size balance / (4 * k)))`, per `startStep`.
    pub fn pulse_size(num_credentials: usize, security_param_k: u64) -> usize {
        let denom = (4 * security_param_k).max(1) as usize;
        num_credentials.div_ceil(denom).max(1)
    }

    /// Credentials still to fold — upstream's `balance`, in `Ord` order.
    pub fn remaining(&self) -> &[Hash32] {
        &self.queue[self.cursor..]
    }

    /// `done` — the balance is exhausted.
    pub fn is_done(&self) -> bool {
        self.cursor >= self.queue.len()
    }

    /// Fold at most `n` more credentials. Returns how many were consumed.
    ///
    /// Chunking is unobservable in the result: each credential contributes only
    /// to its own key, and the caller aggregates with a sort. That property is
    /// what `fold_incremental == fold_batch` asserts, and it is the ONLY
    /// correctness claim incremental pulsing makes.
    pub fn pulse<F: Fn(&Hash32) -> bool>(&mut self, n: usize, ctx: &MemberFoldCtx<'_, F>) -> usize {
        let end = (self.cursor + n).min(self.queue.len());
        let consumed = end - self.cursor;
        for idx in self.cursor..end {
            let cred = self.queue[idx];
            let Some(pool_id) = ctx.delegations.get(&cred) else {
                continue;
            };
            let Some(info) = ctx.table.get(pool_id) else {
                continue;
            };
            if info.owner_set.contains(&cred) {
                continue;
            }
            if ctx.pv_major <= 6 && !(ctx.registered)(&cred) {
                continue;
            }
            let member_stake = ctx.stake.get(&cred).copied().unwrap_or(Lovelace(0)).0;
            let share = info.member_reward(member_stake);
            if share > 0 {
                self.acc
                    .entry(cred)
                    .or_default()
                    .push((true, *pool_id, share));
            }
        }
        self.cursor = end;
        consumed
    }

    /// `completeM` — fold whatever is left in one go.
    pub fn complete<F: Fn(&Hash32) -> bool>(&mut self, ctx: &MemberFoldCtx<'_, F>) {
        let left = self.queue.len() - self.cursor;
        self.pulse(left, ctx);
    }

    /// The accumulated member entries. Panics if the fold is unfinished — a
    /// partial answer read as a complete one would under-pay silently, which
    /// is exactly the class of bug the whole pulser design exists to avoid.
    pub fn into_entries(self) -> HashMap<Hash32, Vec<RewardEntryTriple>> {
        assert!(
            self.is_done(),
            "reward fold read at {}/{} credentials — a partial answer is not a \
             reward update, and treating it as one silently under-pays every \
             credential past the cursor",
            self.cursor,
            self.queue.len()
        );
        self.acc
    }
}

/// The differential gate on incremental pulsing.
///
/// Incremental pulsing makes exactly ONE correctness claim: it changes *when*
/// the reward fold runs, never *what* it computes. These tests assert that
/// claim directly, which is stronger than a replay — a passing replay tells you
/// the answers matched, not that chunking was irrelevant to them.
#[cfg(test)]
mod fold_differential {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn h28(i: u64) -> Hash28 {
        let mut b = [0u8; 28];
        b[..8].copy_from_slice(&i.to_be_bytes());
        dugite_primitives::Hash(b)
    }

    fn h32(i: u64) -> Hash32 {
        let mut b = [0u8; 32];
        // Scatter, so sorted order is not insertion order and a fold that
        // accidentally depends on one is not saved by them coinciding.
        b[..8].copy_from_slice(&i.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes());
        dugite_primitives::Hash(b)
    }

    #[allow(clippy::type_complexity)]
    fn synth(
        creds: usize,
        pools: usize,
    ) -> (
        HashMap<Hash28, PoolRewardInfo>,
        HashMap<Hash32, Hash28>,
        HashMap<Hash32, Lovelace>,
    ) {
        let mut table = HashMap::new();
        for p in 0..pools {
            let id = h28(p as u64);
            table.insert(
                id,
                PoolRewardInfo {
                    pool_id: id,
                    pool_active_stake: 1_000_000_000_000,
                    pool_reward: 500_000_000 + p as u64 * 7,
                    cost: 170_000_000,
                    margin_num: 1,
                    margin_den: 50,
                    // Make one credential per pool an owner, so the owner skip
                    // is exercised at chunk boundaries too.
                    owner_set: HashSet::from([h32(p as u64)]),
                    leader: None,
                },
            );
        }
        let mut delegations = HashMap::new();
        let mut stake = HashMap::new();
        for c in 0..creds {
            let cred = h32(c as u64);
            delegations.insert(cred, h28((c % pools) as u64));
            // Some zero-stake credentials, which the fold must skip identically
            // no matter which chunk they land in.
            stake.insert(
                cred,
                Lovelace(if c % 11 == 0 { 0 } else { 1_000_000 + c as u64 }),
            );
        }
        (table, delegations, stake)
    }

    proptest! {
        /// `fold_incremental(frozen, any pulse_size) == fold_batch(frozen)`.
        #[test]
        fn chunking_is_unobservable_in_the_result(
            creds in 1usize..300,
            pools in 1usize..12,
            pulse in 1usize..64,
        ) {
            let (table, delegations, stake) = synth(creds, pools);
            let ctx = MemberFoldCtx {
                table: &table,
                delegations: &delegations,
                stake: &stake,
                pv_major: 11,
                registered: |_: &Hash32| true,
            };

            let mut batch = RewardFold::new(&delegations);
            batch.complete(&ctx);

            let mut inc = RewardFold::new(&delegations);
            let mut guard = 0;
            while !inc.is_done() {
                inc.pulse(pulse, &ctx);
                guard += 1;
                prop_assert!(guard <= creds + 1, "pulse failed to make progress");
            }

            prop_assert_eq!(batch.into_entries(), inc.into_entries());
        }

        /// Pulsing must always make progress, or a node wedges mid-epoch.
        #[test]
        fn every_pulse_advances_the_cursor(creds in 1usize..100, pools in 1usize..6) {
            let (table, delegations, stake) = synth(creds, pools);
            let ctx = MemberFoldCtx {
                table: &table, delegations: &delegations, stake: &stake,
                pv_major: 11, registered: |_: &Hash32| true,
            };
            let mut f = RewardFold::new(&delegations);
            let before = f.remaining().len();
            let consumed = f.pulse(1, &ctx);
            prop_assert_eq!(consumed, 1);
            prop_assert_eq!(f.remaining().len(), before - 1);
        }
    }

    /// The work queue must not depend on `HashMap` iteration order.
    ///
    /// Upstream's balance is a `Set (Credential 'Staking)` consumed in `Ord`
    /// order. dugite's source is a `HashMap`, whose order varies per process —
    /// so without the sort, the split between folded and pending would differ
    /// across a restart or a rollback, and two nodes computing identical
    /// rewards would still disagree about `nesRu`.
    #[test]
    fn the_queue_is_deterministic_regardless_of_insertion_order() {
        let n = 200u64;
        let mut a: HashMap<Hash32, Hash28> = HashMap::new();
        for i in 0..n {
            a.insert(h32(i), h28(i % 3));
        }
        let mut b: HashMap<Hash32, Hash28> = HashMap::new();
        for i in (0..n).rev() {
            b.insert(h32(i), h28(i % 3));
        }
        let qa = RewardFold::new(&a);
        let qb = RewardFold::new(&b);
        assert_eq!(qa.remaining(), qb.remaining());
        assert!(
            qa.remaining().windows(2).all(|w| w[0] < w[1]),
            "the queue must be strictly ascending — the wire arm encodes it as \
             a Set and a Haskell peer decodes it as one"
        );
    }

    /// A partial fold must never be mistaken for a finished one.
    #[test]
    #[should_panic(expected = "a partial answer is not a reward update")]
    fn reading_an_unfinished_fold_panics() {
        let (table, delegations, stake) = synth(50, 3);
        let ctx = MemberFoldCtx {
            table: &table,
            delegations: &delegations,
            stake: &stake,
            pv_major: 11,
            registered: |_: &Hash32| true,
        };
        let mut f = RewardFold::new(&delegations);
        f.pulse(10, &ctx);
        let _ = f.into_entries();
    }

    /// `pulseSize = max 1 (ceil (size balance / (4 * k)))`.
    #[test]
    fn pulse_size_matches_start_step() {
        assert_eq!(RewardFold::pulse_size(0, 40), 1, "never zero — would wedge");
        assert_eq!(RewardFold::pulse_size(1, 40), 1);
        assert_eq!(RewardFold::pulse_size(160, 40), 1, "at 4k exactly");
        assert_eq!(RewardFold::pulse_size(161, 40), 2, "just past 4k");
        // Mainnet: 1.3M credentials, k = 2160 => 4k = 8640.
        assert_eq!(RewardFold::pulse_size(1_300_000, 2160), 151);
    }
}

/// The frozen `total_stake` must WIN over a boundary-time recomputation.
///
/// This is what retires `pending_avvm_return`. At the Shelley->Allegra boundary
/// `returnRedeemAddrsToReserves` credits the unredeemed AVVM coin to reserves
/// BEFORE the reward update is applied, so a boundary-time
/// `maxSupply - reserves` is post-AVVM while Haskell's `fvTotalStake` — frozen
/// at the 4k/f mark — is pre-AVVM. dugite corrected the gap with a bespoke
/// field subtracted back off at exactly one boundary.
#[cfg(test)]
mod frozen_total_stake {
    use super::*;

    const MAX_SUPPLY: u64 = 45_000_000_000_000_000;

    /// `start_step_monetary` records `maxSupply - reserves` as it stood at the
    /// freeze, not as it stands later.
    #[test]
    fn start_step_freezes_total_stake_at_the_mark() {
        let pre_avvm_reserves = 14_000_000_000_000_000u64;
        let m = start_step_monetary(
            (3, 1000),
            (1, 5),
            (0, 1),
            (1, 20),
            pre_avvm_reserves,
            0,
            21_600,
            432_000,
            MAX_SUPPLY,
        );
        assert_eq!(
            m.total_stake,
            MAX_SUPPLY - pre_avvm_reserves,
            "fvTotalStake is maxSupply - casReserves at the FREEZE instant"
        );
    }

    /// The frozen value differs from a post-AVVM recomputation, and that
    /// difference is exactly the returned coin.
    ///
    /// Without this the whole change is untestable on any network dugite runs:
    /// the devnet starts in Conway and has no Shelley->Allegra boundary, and
    /// the mainnet replay that would show it is blocked on disk (§5b). So the
    /// property is asserted arithmetically instead of observed — stated plainly
    /// rather than dressed up as an end-to-end result.
    #[test]
    fn the_frozen_value_differs_from_a_post_avvm_recomputation() {
        let pre_avvm_reserves = 14_000_000_000_000_000u64;
        let avvm_returned = 318_200_635_000_000u64; // mainnet order of magnitude
        let post_avvm_reserves = pre_avvm_reserves + avvm_returned;

        let m = start_step_monetary(
            (3, 1000),
            (1, 5),
            (0, 1),
            (1, 20),
            pre_avvm_reserves,
            0,
            21_600,
            432_000,
            MAX_SUPPLY,
        );
        let recomputed_at_boundary = MAX_SUPPLY - post_avvm_reserves;

        assert_ne!(
            m.total_stake, recomputed_at_boundary,
            "if these were equal the fixture would not exercise the AVVM case \
             at all, and the test would pass for a build that ignored the \
             frozen value entirely"
        );
        assert_eq!(
            m.total_stake - recomputed_at_boundary,
            avvm_returned,
            "the gap between frozen and recomputed IS the returned AVVM coin — \
             which is precisely what pending_avvm_return used to subtract back \
             off by hand"
        );
    }

    /// `total_stake` is `sigma`'s denominator, so getting it wrong moves every
    /// pool's reward, not just the pot.
    ///
    /// Recorded as an assertion because the tempting smaller change — freeze
    /// only the monetary terms, keep recomputing `total_stake` — would leave
    /// the pot pre-AVVM and the DISTRIBUTION post-AVVM. That is worse than the
    /// patch it replaces: the patch at least kept the two consistent.
    #[test]
    fn total_stake_moves_sigma_not_just_the_pot() {
        let a = start_step_monetary(
            (3, 1000),
            (1, 5),
            (0, 1),
            (1, 20),
            14_000_000_000_000_000,
            0,
            21_600,
            432_000,
            MAX_SUPPLY,
        );
        let b = start_step_monetary(
            (3, 1000),
            (1, 5),
            (0, 1),
            (1, 20),
            14_318_200_635_000_000,
            0,
            21_600,
            432_000,
            MAX_SUPPLY,
        );
        assert_ne!(a.total_stake, b.total_stake, "sigma denominator moved");
        assert_ne!(a.delta_r1, b.delta_r1, "and so did the pot");
    }
}

/// The RUPD member fold in flight: the frozen per-pool table plus the pulser.
///
/// TRANSIENT — deliberately not part of `LedgerStateSnapshot`. A node that
/// restarts mid-epoch rebuilds it at the next block and completes it at the
/// boundary, which by the differential property (`fold_incremental(any
/// pulse_size) == fold_batch`) yields the identical reward update. Persisting
/// it would add a large, consensus-adjacent structure to the snapshot and to
/// rollback for no change in any computed value.
///
/// The one thing it WOULD buy is `nesRu` cursor fidelity across a restart, and
/// that only becomes observable once the `Pulsing` wire arm exists. Recorded
/// here so the trade-off is revisited then, rather than rediscovered.
#[derive(Debug, Clone, Default)]
pub struct InFlightFold {
    /// `fvPoolRewardInfo` — built once at the mark from frozen inputs.
    pub table: HashMap<Hash28, PoolRewardInfo>,
    /// The pulser. `None` until the first pulse builds it.
    pub fold: Option<RewardFold>,
}

impl InFlightFold {
    /// Whether the fold has run to completion and can be applied as-is.
    pub fn is_complete(&self) -> bool {
        self.fold.as_ref().is_some_and(|f| f.is_done())
    }

    /// Credentials still to fold — upstream's `balance`, for the wire arm.
    pub fn remaining(&self) -> usize {
        self.fold.as_ref().map_or(0, |f| f.remaining().len())
    }
}

/// Per-block pulsing must actually ADVANCE the fold.
///
/// The differential property makes chunking unobservable in the RESULT, which
/// means no value comparison can tell a pulsing node from a batching one —
/// disarming the boundary so it discards the pulses and refolds from scratch
/// leaves all 1812 ledger tests green. That is correct, and it is exactly why
/// the assertion here is on WORK DONE rather than on rewards.
///
/// Without it, per-block pulsing could be wired to do nothing — building the
/// fold and never pulsing, or pulsing a fold nobody reads — and every existing
/// test would still pass while the ~2.55 s boundary stall (Phase 0) remained
/// exactly where it was.
#[cfg(test)]
mod pulse_advances {
    use super::*;

    fn h28(i: u64) -> Hash28 {
        let mut b = [0u8; 28];
        b[..8].copy_from_slice(&i.to_be_bytes());
        dugite_primitives::Hash(b)
    }
    fn h32(i: u64) -> Hash32 {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&i.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes());
        dugite_primitives::Hash(b)
    }

    /// Successive pulses consume the queue and the fold eventually completes.
    #[test]
    fn successive_pulses_drain_the_queue() {
        let mut delegations = HashMap::new();
        for c in 0..40u64 {
            delegations.insert(h32(c), h28(c % 4));
        }
        let table: HashMap<Hash28, PoolRewardInfo> = HashMap::new();
        let stake: HashMap<Hash32, Lovelace> = HashMap::new();
        let ctx = MemberFoldCtx {
            table: &table,
            delegations: &delegations,
            stake: &stake,
            pv_major: 11,
            registered: |_: &Hash32| true,
        };

        let mut fold = RewardFold::new(&delegations);
        assert_eq!(fold.remaining().len(), 40, "queue starts full");

        let mut seen = vec![fold.remaining().len()];
        let mut guard = 0;
        while !fold.is_done() {
            fold.pulse(7, &ctx);
            seen.push(fold.remaining().len());
            guard += 1;
            assert!(guard <= 41, "pulsing failed to terminate");
        }

        assert!(
            seen.windows(2).all(|w| w[1] < w[0]),
            "every pulse must strictly reduce the queue; saw {seen:?}"
        );
        assert_eq!(*seen.last().unwrap(), 0, "the fold drains to empty");
        assert!(
            seen.len() > 2,
            "with pulse=7 over 40 credentials this must take several pulses — \
             a single step would mean the pulse size was ignored and the whole \
             point (spreading the work) is lost"
        );
    }

    /// `InFlightFold` reports progress, which is what the `Pulsing` wire arm
    /// will encode as `balance`.
    #[test]
    fn in_flight_fold_reports_remaining_and_completion() {
        let mut delegations = HashMap::new();
        for c in 0..10u64 {
            delegations.insert(h32(c), h28(0));
        }
        let mut f = InFlightFold::default();
        assert_eq!(f.remaining(), 0, "no fold yet");
        assert!(
            !f.is_complete(),
            "absent is not complete — the #1072 distinction"
        );

        f.fold = Some(RewardFold::new(&delegations));
        assert_eq!(f.remaining(), 10);
        assert!(!f.is_complete());

        let table: HashMap<Hash28, PoolRewardInfo> = HashMap::new();
        let stake: HashMap<Hash32, Lovelace> = HashMap::new();
        let ctx = MemberFoldCtx {
            table: &table,
            delegations: &delegations,
            stake: &stake,
            pv_major: 11,
            registered: |_: &Hash32| true,
        };
        f.fold.as_mut().unwrap().complete(&ctx);
        assert_eq!(f.remaining(), 0);
        assert!(f.is_complete());
    }
}
