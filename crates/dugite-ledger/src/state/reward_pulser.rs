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
