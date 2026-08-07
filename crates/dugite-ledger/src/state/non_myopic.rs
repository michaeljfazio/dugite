//! `NonMyopic` — per-pool `Likelihood` history and the frozen reward pot.
//!
//! Mirrors Haskell `Cardano.Ledger.Shelley.PoolRank`:
//!
//! ```haskell
//! data NonMyopic = NonMyopic
//!   { likelihoodsNM :: !(VMap.VMap VMap.VB VMap.VB (KeyHash StakePool) Likelihood)
//!   , rewardPotNM :: !Coin
//!   }
//!
//! newtype LogWeight = LogWeight {unLogWeight :: Float}
//! newtype Likelihood = Likelihood {unLikelihood :: StrictSeq LogWeight}
//! ```
//!
//! # Why this needs its own arithmetic
//!
//! Every other number in the reward path is an exact `Rat` (`num_bigint`),
//! deliberately, because Haskell computes rewards in `Rational`. This module is
//! the one place where that is the WRONG tool: `LogWeight` is a Haskell `Float`
//! and travels on the wire as CBOR `0xfa` single-precision, so the stored value
//! is whatever f32 the reference implementation's floating-point pipeline
//! produces. Exact arithmetic would give a *better* answer and the wrong bytes.
//!
//! The precision split is deliberate and load-bearing (`realToFrac` narrows
//! exactly once):
//!
//! ```haskell
//! l :: Double -> Double
//! l x = n * log x + m * log (1 - t * x)
//! sample position = LogWeight (realToFrac $ l position)
//! ```
//!
//! `l x` is evaluated in `Double`; the result narrows to `Float` at `sample`;
//! and every subsequent operation — `applyDecay`'s multiply, `<>`'s
//! `zipWith (+)`, `normalizeLikelihood`'s `minimum` and subtract — is `Float`.
//! Accumulating in f64 and narrowing at the end produces different bytes for
//! the same inputs, so `f64` appears in exactly one function below and the
//! narrowing point is marked.

use dugite_primitives::hash::Hash28;
use dugite_primitives::value::Lovelace;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Correctly-rounded `Rational -> Double`, i.e. Haskell `realToFrac` on an
/// exact `Rational`.
///
/// This is NOT the same as `num as f64 / den as f64`, and the difference is
/// reachable on mainnet. `realToFrac` rounds the exact ratio to the nearest
/// `Double` **once**; the naive form rounds each operand to `f64` first and then
/// rounds again in the division. `totalStake` is `maxSupply - reserves`, which
/// on mainnet is ~3.7e16 — larger than 2^53 — so `total_stake as f64` is itself
/// lossy before the division even happens.
///
/// The resulting error is small (~1 ulp of f64), and it is *usually* far below
/// the f32 spacing that `LogWeight` finally rounds to. "Usually" is not a
/// property this codebase accepts for a value it must reproduce byte-for-byte,
/// and the cost of doing it exactly is one `BigRational` per pool per epoch.
///
/// A zero denominator yields 0.0, matching Haskell's `%?` safe division
/// (`poolTotalStake %? totalStake`).
fn rational_to_f64(num: u64, den: u64) -> f64 {
    if den == 0 {
        return 0.0;
    }
    BigRational::new(BigInt::from(num), BigInt::from(den))
        .to_f64()
        .unwrap_or(0.0)
}

/// ```haskell
/// leaderProbability :: ActiveSlotCoeff -> Rational -> UnitInterval -> Double
/// leaderProbability activeSlotCoeff relativeStake decentralizationParameter =
///   (1 - (1 - asc) ** s) * (1 - d')
///   where
///     d' = realToFrac . unboundRational $ decentralizationParameter
///     asc = realToFrac . unboundRational . activeSlotVal $ activeSlotCoeff
///     s = realToFrac relativeStake
/// ```
///
/// All three inputs reach this function as exact `Rational`s upstream
/// (`UnitInterval`/`PositiveUnitInterval` wrap a `Ratio Word64`, and `sigma` is
/// built by exact rational division), so the ONLY lossy step in the whole chain
/// is the single `realToFrac` at each of the three boundaries — which is what
/// [`rational_to_f64`] reproduces.
///
/// `**` is `Prelude.(**)` on `Double`, i.e. `f64::powf`.
///
/// # `relative_stake` is the UNCAPPED sigma
///
/// `poolRelativeStake` is `mkPoolRewardInfo`'s `sigma = poolTotalStake %?
/// totalStake`, **before** the `min sigma z0` capping that `maxPool'` applies.
/// Passing the capped value would silently flatten every pool above the
/// saturation point onto one likelihood. Note also that the denominator is
/// `totalStake` (circulating supply, `maxSupply - reserves`) and NOT
/// `totalActiveStake` — the latter is `sigmaA`'s denominator and is used only
/// inside `mkApparentPerformance`.
pub fn leader_probability(
    active_slot_coeff: (u64, u64),
    relative_stake: (u64, u64),
    decentralization: (u64, u64),
) -> f64 {
    let asc = rational_to_f64(active_slot_coeff.0, active_slot_coeff.1);
    let s = rational_to_f64(relative_stake.0, relative_stake.1);
    let d_prime = rational_to_f64(decentralization.0, decentralization.1);

    (1.0 - (1.0 - asc).powf(s)) * (1.0 - d_prime)
}

/// Number of sample positions in a `Likelihood`.
///
/// ```haskell
/// samplePositions :: StrictSeq Double
/// samplePositions = (\x -> (x + 0.5) / 100.0) <$> StrictSeq.fromList [0.0 .. 99.0]
/// ```
pub const SAMPLE_SIZE: usize = 100;

/// ```haskell
/// decayFactor :: Float
/// decayFactor = 0.9
/// ```
pub const DECAY_FACTOR: f32 = 0.9;

/// The 100 sample positions, computed in `Double` exactly as Haskell does:
/// `(x + 0.5) / 100.0` for `x` in `[0.0 .. 99.0]` — i.e. 0.005, 0.015, … 0.995.
///
/// Kept as f64 because it feeds `l x`, which is a `Double` computation.
fn sample_positions() -> impl Iterator<Item = f64> {
    (0..SAMPLE_SIZE).map(|i| (i as f64 + 0.5) / 100.0)
}

/// A per-pool likelihood: `SAMPLE_SIZE` log-weights, one per sample position.
///
/// Invariant: `0.len() == SAMPLE_SIZE`. Constructed only through the functions
/// in this module, all of which preserve it.
///
/// Stored as `Vec<f32>` rather than `[f32; SAMPLE_SIZE]` because serde only
/// derives `Deserialize` for arrays up to length 32.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Likelihood(pub Vec<f32>);

impl Default for Likelihood {
    /// ```haskell
    /// instance Monoid Likelihood where
    ///   mempty = Likelihood $ StrictSeq.forceToStrict $
    ///              Seq.replicate (length samplePositions) (LogWeight 0)
    /// ```
    fn default() -> Self {
        Likelihood(vec![0.0f32; SAMPLE_SIZE])
    }
}

impl Likelihood {
    /// Haskell `mempty` for `Likelihood`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// ```haskell
    /// likelihood :: Natural -> Double -> EpochSize -> Likelihood
    /// likelihood blocks t slotsPerEpoch =
    ///   Likelihood $ sample <$> samplePositions
    ///   where
    ///     n = fromIntegral blocks
    ///     m = fromIntegral $ unEpochSize slotsPerEpoch - fromIntegral blocks
    ///     l :: Double -> Double
    ///     l x = n * log x + m * log (1 - t * x)
    ///     sample position = LogWeight (realToFrac $ l position)
    /// ```
    ///
    /// NOTE the formula: `n * log x`, **not** `n * log (x * t)`. The latter was
    /// written down in dugite's own notes and is wrong; it would corrupt every
    /// stored value for a pool that produced any block.
    ///
    /// `m` is `slotsPerEpoch - blocks` computed in the integer domain before
    /// widening, matching Haskell's `unEpochSize slotsPerEpoch - fromIntegral
    /// blocks`. `blocks <= slots_per_epoch` always holds on a real chain; the
    /// saturating subtraction keeps a corrupt input from wrapping into a huge
    /// `m` rather than panicking inside a reward calculation.
    pub fn new(blocks: u64, t: f64, slots_per_epoch: u64) -> Self {
        let n = blocks as f64;
        let m = slots_per_epoch.saturating_sub(blocks) as f64;

        Likelihood(
            sample_positions()
                // `l x` in f64 …
                .map(|x| n * x.ln() + m * (1.0 - t * x).ln())
                // … narrowed to f32 exactly ONCE, here. Everything downstream
                // of this point stays f32.
                .map(|l| l as f32)
                .collect(),
        )
    }

    /// ```haskell
    /// normalizeLikelihood :: Likelihood -> Likelihood
    /// normalizeLikelihood (Likelihood xs) = Likelihood $ (\x -> x - m) <$> xs
    ///   where m = minimum xs
    /// ```
    ///
    /// Haskell's `minimum` is over `Ord LogWeight`, derived from `Ord Float`,
    /// which is a total order that sorts `NaN` as larger than everything. `f32::min`
    /// instead *ignores* NaN, so a NaN sample would change which value is chosen.
    /// `l x` is finite for every reachable input (`x ∈ (0,1)` so `log x` is finite,
    /// and `t * x < 1` so `log (1 - t*x)` is finite), so the two agree in practice;
    /// the fold below matches Haskell's ordering anyway rather than relying on that.
    fn normalize(self) -> Self {
        let min = self
            .0
            .iter()
            .copied()
            .fold(f32::INFINITY, |acc, x| if x < acc { x } else { acc });
        Likelihood(self.0.into_iter().map(|x| x - min).collect())
    }

    /// ```haskell
    /// applyDecay :: Float -> Likelihood -> Likelihood
    /// applyDecay decay (Likelihood logWeights) = Likelihood $ mul decay <$> logWeights
    ///   where mul x (LogWeight f) = LogWeight (x * f)
    /// ```
    ///
    /// f32 multiply — NOT f64. See the module docs.
    pub fn apply_decay(self, decay: f32) -> Self {
        Likelihood(self.0.into_iter().map(|f| decay * f).collect())
    }

    /// ```haskell
    /// instance Semigroup Likelihood where
    ///   (Likelihood x) <> (Likelihood y) =
    ///     normalizeLikelihood $ Likelihood (StrictSeq.zipWith (+) x y)
    /// ```
    ///
    /// The normalisation is part of `<>`, so it runs on EVERY combine —
    /// including the no-history case `mempty <> newPerf`, which is why a pool's
    /// very first epoch is already min-subtracted and the raw output of
    /// [`Likelihood::new`] is never what gets stored.
    ///
    /// `zipWith` truncates to the shorter sequence in Haskell. Both operands are
    /// always `SAMPLE_SIZE` long here, so `zip` matches.
    pub fn combine(self, other: &Likelihood) -> Self {
        Likelihood(
            self.0
                .into_iter()
                .zip(other.0.iter())
                .map(|(a, b)| a + b)
                .collect(),
        )
        .normalize()
    }
}

/// Haskell `NonMyopic`, the fourth field of `EpochState`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NonMyopic {
    /// `likelihoodsNM` — one entry per pool in the current epoch's
    /// `allPoolInfo`, keyed by pool id.
    pub likelihoods: HashMap<Hash28, Likelihood>,
    /// `rewardPotNM` — the reward pot `_R` frozen at the boundary this record
    /// was written. dugite computes this as `total_rewards_available -
    /// treasury_cut` in `state/rewards.rs`; Haskell computes the same quantity
    /// as `Coin (expansion - deltaT1)` and passes it to `updateNonMyopic` as
    /// `oldr`.
    pub reward_pot: Lovelace,
}

impl Default for NonMyopic {
    /// Haskell `emptyNonMyopic = NonMyopic Map.empty (Coin 0)` — the genesis
    /// value, and the value a node has before its first RUPD.
    ///
    /// Hand-written rather than derived because `Lovelace` has no `Default`.
    fn default() -> Self {
        NonMyopic {
            likelihoods: HashMap::new(),
            reward_pot: Lovelace(0),
        }
    }
}

impl NonMyopic {
    /// Convert the wire-shaped record decoded from a Haskell ledger snapshot
    /// (Mithril ancillary state / `--save-state` dump) into the ledger's own
    /// type.
    ///
    /// `dugite-serialization` sits below `dugite-ledger` in the dependency flow
    /// and so decodes into its own `HaskellNonMyopic` carrying bare `f32`s; this
    /// is where those become `Likelihood`s. The decoder has already enforced
    /// that every entry is exactly [`SAMPLE_SIZE`] long, so no length fixing
    /// happens here — a malformed snapshot fails at decode, not silently here.
    pub fn from_haskell_snapshot(
        hs: &dugite_serialization::haskell_snapshot::HaskellNonMyopic,
    ) -> Self {
        NonMyopic {
            likelihoods: hs
                .likelihoods
                .iter()
                .map(|(pool_id, weights)| (*pool_id, Likelihood(weights.clone())))
                .collect(),
            reward_pot: Lovelace(hs.reward_pot),
        }
    }

    /// ```haskell
    /// updateNonMyopic nm rPot_ newLikelihoods =
    ///   nm {likelihoodsNM = updatedLikelihoods, rewardPotNM = rPot_}
    ///   where
    ///     history = likelihoodsNM nm
    ///     performance kh newPerf =
    ///       maybe mempty (applyDecay decayFactor) (VMap.lookup kh history) <> newPerf
    ///     updatedLikelihoods = VMap.mapWithKey performance newLikelihoods
    /// ```
    ///
    /// Two consequences of `mapWithKey` running over `newLikelihoods` rather
    /// than over `history`:
    ///
    /// * a pool present in `history` but ABSENT from this epoch's
    ///   `newLikelihoods` is **dropped**, not carried forward decayed; and
    /// * a pool with no history starts from `mempty`, so its first stored value
    ///   is `normalizeLikelihood (zipWith (+) zeros newPerf)` — min-subtracted,
    ///   never the raw likelihood.
    pub fn update(
        &self,
        reward_pot: Lovelace,
        new_likelihoods: HashMap<Hash28, Likelihood>,
    ) -> Self {
        let likelihoods = new_likelihoods
            .into_iter()
            .map(|(pool_id, new_perf)| {
                let decayed = match self.likelihoods.get(&pool_id) {
                    Some(prev) => prev.clone().apply_decay(DECAY_FACTOR),
                    None => Likelihood::empty(),
                };
                (pool_id, decayed.combine(&new_perf))
            })
            .collect();

        NonMyopic {
            likelihoods,
            reward_pot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(b: u8) -> Hash28 {
        Hash28::from_bytes([b; 28])
    }

    /// The formula is `n * log x + m * log (1 - t*x)`.
    ///
    /// Expected values computed independently (Python `math.log` + an explicit
    /// f32 narrowing) rather than by re-running this module's own arithmetic.
    ///
    /// # Why this test asserts on the RAW likelihood
    ///
    /// The formula dugite's notes had written down — `n * log (x*t)` — differs
    /// from the correct one by `n * log t`, which is CONSTANT in `x`. And
    /// `normalizeLikelihood` subtracts the minimum, so a constant offset
    /// cancels: measured end-to-end, the two formulas agree to within one f32
    /// ulp (3.8e-06 after one epoch, 1.5e-05 after two). A test on the STORED
    /// value with any tolerance at all would pass under the wrong formula.
    ///
    /// So the assertion has to sit here, on the un-normalised output, where the
    /// two differ by ~30 absolute. Disarming the fix (swapping in
    /// `(x * t).ln()`) turns this red by a wide margin; the same swap leaves
    /// `combine_normalises_even_with_no_history` green.
    #[test]
    fn likelihood_is_n_log_x_not_n_log_x_times_t() {
        let l = Likelihood::new(10, 0.05, 400);
        assert_eq!(l.0.len(), SAMPLE_SIZE);
        assert_eq!(l.0[0], -53.080_685_f32);
        assert_eq!(l.0[1], -42.289_66_f32);
        assert_eq!(l.0[50], -16.805_927_f32);
        assert_eq!(l.0[99], -19.951_893_f32);
    }

    /// `m = slotsPerEpoch - blocks`, not `slotsPerEpoch`.
    ///
    /// Disarming by passing `slots_per_epoch` as `m` changes every element.
    #[test]
    fn m_is_slots_minus_blocks() {
        let with_blocks = Likelihood::new(10, 0.05, 400);
        let as_if_m_were_slots = Likelihood::new(10, 0.05, 410);
        assert_ne!(
            with_blocks.0[0], as_if_m_were_slots.0[0],
            "m must be slotsPerEpoch - blocks; using slotsPerEpoch directly is \
             the same as inflating the epoch length by `blocks`"
        );
    }

    /// A pool with zero stake gets `t = 0`, so every log-weight is exactly 0 —
    /// `n * log x` is `0 * finite = 0` and `m * log(1 - 0)` is `m * 0 = 0`.
    ///
    /// This is the case the wire capture showed: pool `4c9da6ff…` came back as
    /// 100 × `0.0` on the wire and 100 × `1` in cardano-cli's JSON, and
    /// `exp(0) = 1` reconciles the two views.
    #[test]
    fn zero_stake_pool_is_all_zero_log_weights() {
        let l = Likelihood::new(0, 0.0, 400);
        assert!(l.0.iter().all(|&w| w == 0.0), "{:?}", &l.0[..4]);
    }

    /// A pool with zero blocks but NON-zero stake is not all-zero — it decays
    /// monotonically in `x`. Guards against "0 blocks ⇒ skip the pool", which
    /// is what the reward-distribution loop does and what this must not.
    #[test]
    fn zero_block_pool_with_stake_is_not_all_zero() {
        let l = Likelihood::new(0, 0.05, 400);
        assert_eq!(l.0[0], -0.100_012_5_f32);
        assert_eq!(l.0[99], -20.412_07_f32);
    }

    /// `<>` normalises, and the no-history case is `mempty <> newPerf` — so
    /// even a pool's FIRST stored likelihood is min-subtracted.
    ///
    /// Disarming `combine` (dropping the `.normalize()`) turns this red.
    #[test]
    fn combine_normalises_even_with_no_history() {
        let fresh = Likelihood::new(10, 0.05, 400);
        let stored = Likelihood::empty().combine(&fresh);

        let min = stored.0.iter().copied().fold(f32::INFINITY, f32::min);
        assert_eq!(min, 0.0, "the minimum log-weight must be exactly 0");
        assert!(
            stored.0.iter().all(|&w| w >= 0.0),
            "normalisation makes every weight non-negative"
        );
        assert_ne!(
            stored.0[0], fresh.0[0],
            "the stored value must NOT be the raw likelihood"
        );
    }

    /// Decay multiplies by 0.9 in f32 and applies to the OLD value only, never
    /// to the incoming one.
    #[test]
    fn decay_is_f32_and_applies_only_to_history() {
        let l = Likelihood(vec![10.0f32; SAMPLE_SIZE]);
        let decayed = l.apply_decay(DECAY_FACTOR);
        assert_eq!(decayed.0[0], 0.9f32 * 10.0f32);

        // `mempty <> newPerf` — the incoming value is not decayed, so a
        // freshly-seen pool's stored weights are exactly the normalised raw
        // likelihood, not 0.9× it.
        let fresh = Likelihood::new(10, 0.05, 400);
        let stored = Likelihood::empty().combine(&fresh);
        let min = fresh.0.iter().copied().fold(f32::INFINITY, f32::min);
        assert_eq!(stored.0[0], fresh.0[0] - min);
    }

    /// `updateNonMyopic` maps over `newLikelihoods`, so a pool that was in the
    /// history but is absent this epoch is DROPPED — not carried forward
    /// decayed.
    ///
    /// Disarming by iterating the history instead turns this red.
    #[test]
    fn absent_pool_is_dropped_not_decayed_forward() {
        let mut history = HashMap::new();
        history.insert(pool(0xAA), Likelihood(vec![5.0f32; SAMPLE_SIZE]));
        history.insert(pool(0xBB), Likelihood(vec![7.0f32; SAMPLE_SIZE]));
        let prev = NonMyopic {
            likelihoods: history,
            reward_pot: Lovelace(1),
        };

        // Only 0xAA appears this epoch.
        let mut fresh = HashMap::new();
        fresh.insert(pool(0xAA), Likelihood::new(3, 0.05, 400));

        let next = prev.update(Lovelace(42), fresh);

        assert!(next.likelihoods.contains_key(&pool(0xAA)));
        assert!(
            !next.likelihoods.contains_key(&pool(0xBB)),
            "a pool absent from newLikelihoods must be dropped"
        );
        assert_eq!(next.likelihoods.len(), 1);
        assert_eq!(next.reward_pot, Lovelace(42));
    }

    /// A pool WITH history has its old value decayed before the combine, and
    /// the result is normalised.
    #[test]
    fn history_is_decayed_then_combined_then_normalised() {
        let mut history = HashMap::new();
        // A non-constant history, so the decay is observable after normalising.
        history.insert(
            pool(0xAA),
            Likelihood((0..SAMPLE_SIZE).map(|i| i as f32).collect()),
        );
        let prev = NonMyopic {
            likelihoods: history,
            reward_pot: Lovelace(0),
        };

        let mut fresh = HashMap::new();
        fresh.insert(pool(0xAA), Likelihood(vec![0.0f32; SAMPLE_SIZE]));

        let next = prev.update(Lovelace(9), fresh);
        let got = &next.likelihoods[&pool(0xAA)];

        // 0.9 * [0, 1, 2, …] + 0, then minus the minimum (which is 0).
        assert_eq!(got.0[0], 0.0);
        assert_eq!(got.0[1], 0.9f32);
        assert_eq!(got.0[99], 0.9f32 * 99.0f32);
    }

    /// `leaderProbability asc s d = (1 - (1 - asc) ** s) * (1 - d)`.
    ///
    /// Expected values computed independently.
    #[test]
    fn leader_probability_matches_haskell() {
        // asc = 1/20, sigma = 1/4, d = 0
        assert_eq!(
            leader_probability((1, 20), (1, 4), (0, 1)),
            0.012_741_455_098_566_168_f64
        );
        // the same with d = 1/2 — halves the result
        assert_eq!(
            leader_probability((1, 20), (1, 4), (1, 2)),
            0.006_370_727_549_283_084_f64
        );
        // d = 1 ⇒ no non-overlay slots ⇒ probability 0
        assert_eq!(leader_probability((1, 20), (1, 4), (1, 1)), 0.0);
        // zero relative stake ⇒ (1-asc)^0 = 1 ⇒ probability 0
        assert_eq!(leader_probability((1, 20), (0, 1), (0, 1)), 0.0);
    }

    /// `sigma`'s denominator can exceed 2^53 on mainnet, so the conversion has
    /// to round the exact ratio ONCE rather than rounding each operand first.
    ///
    /// Disarming `rational_to_f64` to `num as f64 / den as f64` turns this red.
    #[test]
    fn sigma_conversion_rounds_the_exact_ratio_once() {
        // A denominator just above 2^53 that is not representable in f64.
        let den: u64 = (1u64 << 53) + 1;
        let num: u64 = 3;

        let naive = num as f64 / den as f64;
        let exact = super::rational_to_f64(num, den);

        assert_ne!(
            naive, exact,
            "naive per-operand conversion must differ here — if it does not, \
             this test has stopped discriminating and needs a new denominator"
        );
        // 3 / 9007199254740993, correctly rounded to f64. Computed
        // independently with Python's `float(Fraction(3, 2**53 + 1))`:
        //   exact  0x1.7ffffffffffffp-52  = 3.330669073875469e-16
        //   naive  0x1.8000000000000p-52  = 3.3306690738754696e-16
        // — one ulp apart, and the exact one is what `realToFrac` produces.
        assert_eq!(exact, 3.330669073875469e-16_f64);
        assert_eq!(naive, 3.3306690738754696e-16_f64);
    }

    /// Haskell's `%?` is safe division: a zero denominator yields 0, not NaN.
    /// Reachable when `totalStake` is 0 (max supply fully in reserves).
    #[test]
    fn zero_denominator_is_zero_not_nan() {
        assert_eq!(super::rational_to_f64(5, 0), 0.0);
        assert_eq!(leader_probability((1, 20), (5, 0), (0, 1)), 0.0);
    }

    /// `dugite-serialization` sits BELOW this crate in the dependency flow and
    /// so cannot import [`SAMPLE_SIZE`]; it declares its own copy. If they ever
    /// drift, the Mithril import would accept a `Likelihood` this module cannot
    /// represent.
    #[test]
    fn sample_size_agrees_with_the_serialization_crate() {
        assert_eq!(
            SAMPLE_SIZE,
            dugite_serialization::haskell_snapshot::LIKELIHOOD_SAMPLE_SIZE
        );
    }

    /// `emptyNonMyopic = NonMyopic Map.empty (Coin 0)`.
    #[test]
    fn default_is_empty_non_myopic() {
        let d = NonMyopic::default();
        assert!(d.likelihoods.is_empty());
        assert_eq!(d.reward_pot, Lovelace(0));
    }
}
