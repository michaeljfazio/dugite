use super::{LedgerState, PendingRewardUpdate, StakeSnapshot};
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::value::Lovelace;
use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use std::collections::HashMap;
use tracing::{debug, warn};

/// Arbitrary-precision rational number matching Haskell's `Rational`.
///
/// Uses `num_bigint::BigInt` for exact arithmetic with no overflow risk.
/// All intermediate reward calculations produce exact results; `floor_u64()`
/// applies the single floor operation at the end, matching Haskell's
/// `rationalToCoinViaFloor`.
///
/// Previous implementation used i128 with BigInt fallback, but the fallback
/// saturated to i128::MAX when results didn't fit, silently producing wrong
/// answers for mainnet-scale values (~36T circulation denominator).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rat {
    pub n: BigInt,
    pub d: BigInt,
}

impl Rat {
    pub fn new(n: impl Into<BigInt>, d: impl Into<BigInt>) -> Self {
        let d = d.into();
        let n = n.into();
        if d.is_zero() {
            return Rat {
                n: BigInt::from(0),
                d: BigInt::from(1),
            };
        }
        let g = Self::bigint_gcd(&n, &d);
        let (n, d) = (&n / &g, &d / &g);
        // Normalize sign: denominator always positive
        if d < BigInt::from(0) {
            Rat { n: -n, d: -d }
        } else {
            Rat { n, d }
        }
    }

    fn bigint_gcd(a: &BigInt, b: &BigInt) -> BigInt {
        let (mut a, mut b) = (a.abs(), b.abs());
        while !b.is_zero() {
            let t = b.clone();
            b = &a % &t;
            a = t;
        }
        if a.is_zero() {
            BigInt::from(1)
        } else {
            a
        }
    }

    pub fn add(&self, other: &Rat) -> Rat {
        let n = &self.n * &other.d + &other.n * &self.d;
        let d = &self.d * &other.d;
        Rat::new(n, d)
    }

    pub fn sub(&self, other: &Rat) -> Rat {
        let n = &self.n * &other.d - &other.n * &self.d;
        let d = &self.d * &other.d;
        Rat::new(n, d)
    }

    pub fn mul(&self, other: &Rat) -> Rat {
        Rat::new(&self.n * &other.n, &self.d * &other.d)
    }

    pub fn div(&self, other: &Rat) -> Rat {
        if other.n.is_zero() {
            return Rat::new(0i128, 1i128);
        }
        Rat::new(&self.n * &other.d, &self.d * &other.n)
    }

    pub fn min_rat(&self, other: &Rat) -> Rat {
        // a/b <= c/d iff a*d <= c*b (when b, d > 0)
        if &self.n * &other.d <= &other.n * &self.d {
            self.clone()
        } else {
            other.clone()
        }
    }

    pub fn floor_u64(&self) -> u64 {
        if self.d.is_zero() || self.n <= BigInt::from(0) {
            return 0;
        }
        let result = &self.n / &self.d;
        // The result of floor(reward) must always fit in u64
        u64::try_from(result).unwrap_or_else(|_| {
            warn!("Rat::floor_u64 overflow — value exceeds u64::MAX, clamping");
            u64::MAX
        })
    }

    /// Helper: create from i128 values (convenience for the common case)
    pub fn from_i128(n: i128, d: i128) -> Self {
        Rat::new(BigInt::from(n), BigInt::from(d))
    }
}

/// Compute a reward update from explicit parameters, without requiring a `LedgerState`.
///
/// This is the standalone version of the reward calculation that was previously
/// only accessible via `LedgerState::calculate_rewards_inner`. It implements the
/// full Haskell `startStep` / RUPD formula:
///   - Monetary expansion with eta performance adjustment
///   - Treasury tau cut
///   - Per-pool maxPool' with pledge influence (a0)
///   - Apparent performance (mkApparentPerformance)
///   - Operator/member reward split
///
/// # Parameters
///
/// * `params` — Previous epoch's protocol parameters (Haskell's `prevPParams`)
/// * `prev_d` — Decentralisation parameter from the previous epoch
/// * `prev_protocol_version_major` — Major protocol version from the previous epoch (for pre-Babbage prefilter)
/// * `go_snapshot` — GO stake snapshot (delegations, pool params, stake distribution). `None` yields empty rewards.
/// * `bprev_blocks_by_pool` — Per-pool block production counts from the previous epoch (Haskell's `nesBprev`)
/// * `ss_fee` — Fee pot from SNAP at previous boundary (Haskell's `ssFee`)
/// * `reserves` — Current reserves
/// * `_treasury` — Current treasury (reserved for future use)
/// * `reward_accounts` — Registered reward accounts (for pre-Babbage prefilter check)
/// * `epoch_length` — Shelley epoch length in slots
/// * `_shelley_transition_epoch` — Number of Byron epochs before Shelley (reserved for future use)
#[allow(clippy::too_many_arguments)]
pub fn compute_reward_update(
    params: &ProtocolParameters,
    prev_d: &dugite_primitives::transaction::Rational,
    prev_protocol_version_major: u64,
    go_snapshot: Option<&StakeSnapshot>,
    bprev_blocks_by_pool: &HashMap<Hash28, u64>,
    ss_fee: Lovelace,
    reserves: Lovelace,
    _treasury: Lovelace,
    reward_accounts: &HashMap<Hash32, Lovelace>,
    startstep_addrs_rew: Option<&std::collections::HashSet<Hash32>>,
    epoch_length: u64,
    _shelley_transition_epoch: u64,
    max_lovelace_supply: u64,
) -> PendingRewardUpdate {
    // pv≤6 reward prefilter source set (`fvAddrsRew`). Haskell freezes
    // `Map.keysSet(accounts)` at `startStep` (mid-epoch, before that block's
    // certs); both the per-member (`rewardOnePoolMember`) and leader
    // (`collectLRs`) prefilters test THIS frozen set. We use it when captured;
    // otherwise fall back to the boundary-time `reward_accounts` keys (mirrors
    // Haskell's `RewardsTooLate` path that forces startStep at the boundary).
    let registered_at_startstep = |cred: &Hash32| -> bool {
        match startstep_addrs_rew {
            Some(set) => set.contains(cred),
            None => reward_accounts.contains_key(cred),
        }
    };
    // Issue #438 fix: compute expansion + treasury_cut BEFORE checking go.
    //
    // Haskell `Cardano.Ledger.Shelley.LedgerState.PulsingReward.startStep`
    // unconditionally drains `min(1,eta) × rho × reserves` from reserves and
    // routes the tau cut to treasury, even when ssStakeGo is empty (early
    // epochs before the first SNAP rotation has populated GO).  Returning
    // `PendingRewardUpdate::default()` on `go=None` here was the source of
    // the systematic ~0.27% per-boundary `pool_reward` overshoot tracked in
    // #438: dugite skipped 3 early-epoch RUPDs (boundaries 1→2, 2→3, plus
    // the missing one at the era transition) that Haskell's pulser fires
    // unconditionally, leaving dugite with +27M ADA more reserves and -27M
    // ADA less treasury than Haskell from epoch 4 onward.  That excess
    // reserve makes σ = pool_stake / (MAX − reserves) ~0.27% larger every
    // subsequent boundary, inflating max_pool by the same ratio.

    let pp = params;
    let rho_num = pp.rho.numerator as i128;
    let rho_den = pp.rho.denominator.max(1) as i128;
    let tau_num = pp.tau.numerator as i128;
    let tau_den = pp.tau.denominator.max(1) as i128;

    let actual_blocks: u64 = bprev_blocks_by_pool.values().sum();
    let epoch_fees = ss_fee.0;

    let rho = Rat::from_i128(rho_num, rho_den);

    // d is an exact Rational from prev_pparams.d (Haskell `prevPParams ^. ppDG`,
    // a `UnitInterval` = bounded `Rational`).  Issue #629 replaced the prior
    // f64 path; gate and expected-blocks math are now byte-exact with
    // Haskell `Cardano.Ledger.Shelley.LedgerState.PulsingReward.startStep`.
    let d_num = prev_d.numerator as i128;
    let d_den = prev_d.denominator.max(1) as i128;

    // Overlay gate: `d >= 4/5` in exact Rational (Haskell `d >= 0.8`).
    // 4/5 ≤ d_num/d_den  ⟺  4 * d_den ≤ 5 * d_num.
    let d_ge_4_5 = 5 * d_num >= 4 * d_den;

    let expansion = if d_ge_4_5 {
        // Full monetary expansion: floor(rho * reserves).
        rho.mul(&Rat::from_i128(reserves.0 as i128, 1)).floor_u64()
    } else {
        let (f_num, f_den) = pp.active_slot_coeff_rational();

        // expectedBlocks = floor((1 - d) * f * slotsPerEpoch), in exact
        // Rational arithmetic — multiply first, floor once at the very end.
        let one_minus_d_num = d_den - d_num;
        let one_minus_d = Rat::from_i128(one_minus_d_num, d_den);
        let f = Rat::from_i128(f_num as i128, f_den as i128);
        let slots = Rat::from_i128(epoch_length as i128, 1);
        let raw_expected_blocks = one_minus_d.mul(&f).mul(&slots).floor_u64();
        if raw_expected_blocks == 0 {
            warn!(
                "expected_blocks rounded to 0 (d={}/{}, f_num={f_num}, f_den={f_den}, \
                 epoch_length={epoch_length}), clamping to 1",
                d_num, d_den,
            );
        }
        let expected_blocks = raw_expected_blocks.max(1);

        // Haskell: eta = blocksMade % expectedBlocks; deltaR1 = floor(min(1, eta) * rho * reserves).
        // Capping actual_blocks at expected_blocks first is equivalent to
        // `min(1, eta)` because effective_blocks/expected_blocks <= 1.
        let effective_blocks = actual_blocks.min(expected_blocks);
        rho.mul(&Rat::from_i128(reserves.0 as i128, 1))
            .mul(&Rat::from_i128(
                effective_blocks as i128,
                expected_blocks as i128,
            ))
            .floor_u64()
    };

    let total_rewards_available = expansion + epoch_fees;

    if total_rewards_available == 0 {
        return PendingRewardUpdate::default();
    }

    let tau = Rat::from_i128(tau_num, tau_den);
    let treasury_cut = tau
        .mul(&Rat::from_i128(total_rewards_available as i128, 1))
        .floor_u64();

    let reward_pot = total_rewards_available - treasury_cut;

    let total_stake = max_lovelace_supply.saturating_sub(reserves.0);
    if total_stake == 0 {
        // #615b: Haskell's RewardUpdate carries only treasury_cut in deltaT;
        // the undistributed portion of reward_pot is refunded to reserves via
        // deltaR = -expansion + undistributed. With distributed=0, undistributed
        // = reward_pot, so delta_reserves is expansion - reward_pot =
        // treasury_cut - epoch_fees. #796: signed — a degraded epoch where
        // epoch_fees > treasury_cut credits reserves (delta_reserves < 0)
        // instead of saturating the credit away.
        let delta_treasury = treasury_cut;
        let delta_reserves = treasury_cut as i128 - epoch_fees as i128;
        return PendingRewardUpdate {
            delta_reserves,
            delta_treasury,
            rewards: HashMap::new(),
        };
    }

    // Issue #438 fix (continued): If the GO snapshot is empty (early epochs
    // before the first SNAP rotation has populated it, or genesis-like
    // conditions), Haskell's pulser still drains expansion and routes the
    // tau cut to treasury — only the per-pool distribution loop is skipped
    // because there are no pools to distribute to.  Match that behaviour.
    //
    // #615b: undistributed (=reward_pot when distributed=0) goes to reserves
    // via RewardUpdate.deltaR, NOT treasury.
    let go = match go_snapshot {
        Some(s) => s,
        None => {
            // #796: signed delta_reserves — see the total_stake==0 branch above.
            let delta_treasury = treasury_cut;
            let delta_reserves = treasury_cut as i128 - epoch_fees as i128;
            return PendingRewardUpdate {
                delta_reserves,
                delta_treasury,
                rewards: HashMap::new(),
            };
        }
    };

    // #898: `totalActiveStake` is the sum of the GO snapshot's ENTIRE
    // per-credential active-stake map — it is NOT restricted to pools that are
    // still registered.
    //
    //   -- Cardano.Ledger.State.SnapShots
    //   mkSnapShot ssActiveStake ssStakePoolsSnapShot =
    //     let ssTotalActiveStake = sumAllActiveStake ssActiveStake
    //      in SnapShot {ssActiveStake, ssTotalActiveStake, ssStakePoolsSnapShot}
    //
    //   -- Cardano.Ledger.State.Stake
    //   -- | Active stake: maps staking credentials to their non-zero stake
    //   -- paired with delegation. Only credentials that are registered,
    //   -- delegated, and have non-zero stake appear here.
    //   sumAllActiveStake (ActiveStake m) =
    //     VMap.foldMap (fromCompact . unNonZero . swdStake) m
    //       `nonZeroOr` knownNonZeroCoin @1
    //
    // Membership requires *registered + delegated + non-zero stake*; it does
    // NOT require the delegated-to pool to still exist. Pool retirement
    // (POOLREAP) removes the pool from `psStakePools` but leaves its
    // delegators' delegations dangling, so their stake keeps contributing to
    // `ssTotalActiveStake` while contributing to no pool's `spssStake`
    // (`ssStakePoolsSnapShot` is rebuilt from `psStakePools` alone).
    //
    // Filtering by `pool_params` understated `totalActiveStake`. Since
    // `appPerf = beta / sigmaA = beta * totalActiveStake / poolStake`, a low
    // total scales every pool's `poolPot` — and therefore every leader and
    // member reward — down proportionally. On preview epoch 1363 a retired
    // pool held 1000 ADA, so dugite's total was 1_000_000_000 lovelace low;
    // the member reward for account 8fab5f50… came out 4 lovelace short of
    // the on-chain value and the PV≥10 exact-drain withdrawal check then
    // halted chain advance permanently.
    //
    // `pool_stake` and `stake_distribution` are built from the same
    // `certs.delegations` walk in both `eras/shelley.rs` and `eras/conway.rs`
    // (UTxO stake + reward balance, plus the Shelley-only pointer-stake
    // resolution applied to both), so summing every `pool_stake` entry is
    // exactly `sumAllActiveStake` over `stake_distribution` — and is O(pools)
    // rather than O(credentials).
    let total_active_stake: u64 = go
        .pool_stake
        .values()
        .fold(0u64, |acc, s| acc.saturating_add(s.0));
    if total_active_stake == 0 {
        debug!(
            "No active stake: GO pools={}, GO pool_stake entries={}",
            go.pool_params.len(),
            go.pool_stake.len()
        );
        // #615b: distributed=0, undistributed=reward_pot → reserves (not treasury).
        // #796: signed delta_reserves — see the total_stake==0 branch above.
        let delta_treasury = treasury_cut;
        let delta_reserves = treasury_cut as i128 - epoch_fees as i128;
        return PendingRewardUpdate {
            delta_reserves,
            delta_treasury,
            rewards: HashMap::new(),
        };
    }

    // #898 diagnostic: stake still delegated to pools that are no longer
    // registered. It counts toward `totalActiveStake` (above) but earns
    // nothing. A non-zero value here is normal after a pool retires; it is
    // logged because a mismatch in this figure shifts EVERY pool's reward and
    // is otherwise invisible until a withdrawal fails the PV≥10 exact-drain
    // check (which is how #898 surfaced).
    if tracing::enabled!(tracing::Level::DEBUG) {
        let retired_stake: u64 = go
            .pool_stake
            .iter()
            .filter(|(pool_id, _)| !go.pool_params.contains_key(pool_id))
            .fold(0u64, |acc, (_, s)| acc.saturating_add(s.0));
        if retired_stake > 0 {
            debug!(
                total_active_stake,
                retired_stake,
                retired_pools = go
                    .pool_stake
                    .keys()
                    .filter(|p| !go.pool_params.contains_key(p))
                    .count(),
                "GO snapshot holds stake delegated to unregistered pools \
                 (counted in totalActiveStake per Haskell sumAllActiveStake)"
            );
        }
    }

    let total_blocks_in_epoch: u64 = bprev_blocks_by_pool.values().sum::<u64>().max(1);

    let n_opt = pp.n_opt.max(1);

    // Per-credential reward entries (leader + member), collected UNAGGREGATED so we
    // can apply Haskell's `filterRewards` single-selection at pv<=2 (Shelley era).
    // At pv<=2 a credential earning from multiple sources/pools is paid only ONE
    // reward (`Set.deleteFindMin`, min by Ord: LeaderReward < MemberReward, then
    // ascending pool-id); the rest are dropped (frShelleyIgnored) and return to
    // reserves via deltaR2. At pv>=3 (Allegra+) all rewards aggregate (sum). See
    // eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs `filterRewards` +
    // `hardforkAllegraAggregatedRewards pv = pvMajor pv > natVersion @2`.
    // Entry = (is_member, producing_pool_id, amount).
    let mut reward_entries: HashMap<Hash32, Vec<(bool, Hash28, u64)>> = HashMap::new();

    let mut delegators_by_pool: HashMap<Hash28, Vec<Hash32>> = HashMap::new();
    for (cred_hash, pool_id) in go.delegations.iter() {
        delegators_by_pool
            .entry(*pool_id)
            .or_default()
            .push(*cred_hash);
    }

    let mut owner_stake_by_pool: HashMap<Hash28, u64> = HashMap::new();
    for (pool_id, pool_reg) in go.pool_params.iter() {
        let mut owner_stake = 0u64;
        for owner in &pool_reg.owners {
            let owner_key = owner.to_hash32_padded();
            if go.delegations.get(&owner_key) == Some(pool_id) {
                owner_stake += go
                    .stake_distribution
                    .get(&owner_key)
                    .map(|l| l.0)
                    .unwrap_or(0);
            }
        }
        owner_stake_by_pool.insert(*pool_id, owner_stake);
    }

    for (pool_id, pool_active_stake) in &go.pool_stake {
        if bprev_blocks_by_pool.get(pool_id).copied().unwrap_or(0) == 0 {
            continue;
        }

        let pool_reg = match go.pool_params.get(pool_id) {
            Some(reg) => reg,
            None => continue,
        };

        // NOTE: the pre-Babbage (pv<=6) reward-account registration prefilter
        // gates ONLY the LEADER (operator) reward, NOT the whole pool. Haskell
        // `collectLRs` (Cardano/Ledger/Shelley/Rewards.hs): the leader reward is
        // included iff `hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered
        // account accounts`; member rewards are gated separately by their own
        // per-member prefilter (`hk ∈ addrsRew`) in `rewardOnePoolMember`. A
        // previous whole-pool `continue` here dropped the MEMBER rewards too,
        // under-distributing them back into reserves (mainnet ep213: 4 pools with
        // unregistered operators but registered members → +180,457,654,009 lovelace
        // reserves divergence, cross-checked byte-exact vs Koios). The leader gate
        // now lives at the operator-credit site below.
        let self_delegated = owner_stake_by_pool.get(pool_id).copied().unwrap_or(0);
        if self_delegated < pool_reg.pledge.0 {
            debug!(
                "Pool {} pledge not met: {} < {}",
                pool_id.to_hex(),
                self_delegated,
                pool_reg.pledge.0
            );
            continue;
        }

        let a0_r = Rat::from_i128(pp.a0.numerator as i128, pp.a0.denominator.max(1) as i128);
        let z0 = Rat::from_i128(1, n_opt as i128);
        let sigma_raw = Rat::from_i128(pool_active_stake.0 as i128, total_stake as i128);
        let p_raw = Rat::from_i128(pool_reg.pledge.0 as i128, total_stake as i128);
        let sigma = sigma_raw.min_rat(&z0);
        let p = p_raw.min_rat(&z0);

        let f4 = z0.sub(&sigma).div(&z0);
        let f3 = sigma.sub(&p.mul(&f4)).div(&z0);
        let f2 = sigma.add(&p.mul(&a0_r).mul(&f3));
        let f1 = Rat::from_i128(reward_pot as i128, 1).div(&Rat::from_i128(1, 1).add(&a0_r));
        let max_pool = f1.mul(&f2).floor_u64();

        let blocks_made = bprev_blocks_by_pool.get(pool_id).copied().unwrap_or(0);
        debug!(
            pool = ?pool_id.as_bytes()[..4],
            blocks_made,
            max_pool,
            pool_stake = pool_active_stake.0,
            total_stake,
            total_active_stake,
            total_blocks = total_blocks_in_epoch,
            reward_pot,
            self_delegated,
            pledge = pool_reg.pledge.0,
            n_opt,
            d_num = d_num as i64,
            d_den = d_den as i64,
            "Per-pool reward input"
        );

        let pool_reward = if pool_active_stake.0 == 0 {
            0u64
        } else if d_ge_4_5 {
            max_pool
        } else if blocks_made == 0 {
            0u64
        } else {
            let perf = Rat::from_i128(blocks_made as i128, total_blocks_in_epoch as i128).mul(
                &Rat::from_i128(total_active_stake as i128, pool_active_stake.0 as i128),
            );
            perf.mul(&Rat::from_i128(max_pool as i128, 1)).floor_u64()
        };

        if pool_reward == 0 {
            continue;
        }

        let cost = pool_reg.cost.0;
        let margin_num = pool_reg.margin_numerator as i128;
        let margin_den = pool_reg.margin_denominator.max(1) as i128;

        let operator_reward = if pool_reward <= cost {
            pool_reward
        } else {
            let remainder = pool_reward - cost;
            let margin = Rat::from_i128(margin_num, margin_den);
            let one_minus_margin = Rat::from_i128(margin_den - margin_num, margin_den);
            let s_over_sigma = Rat::from_i128(self_delegated as i128, pool_active_stake.0 as i128);
            let share = margin.add(&one_minus_margin.mul(&s_over_sigma));
            let op_extra = share.mul(&Rat::from_i128(remainder as i128, 1)).floor_u64();
            cost + op_extra
        };

        let owner_set: std::collections::HashSet<Hash32> = pool_reg
            .owners
            .iter()
            .map(|o| o.to_hash32_padded())
            .collect();

        if let Some(delegators) = delegators_by_pool.get(pool_id) {
            for cred_hash in delegators {
                if owner_set.contains(cred_hash) {
                    continue;
                }

                // Mirror Haskell `rewardOnePoolMember.prefilter`
                // (eras/shelley/impl/.../Rewards.hs:262):
                //   prefilter = hardforkBabbageForgoRewardPrefilter pv || hk ∈ addrsRew
                //
                // For pv ≤ 6 (Shelley-Alonzo), the member credential must be
                // currently registered in the reward-accounts set or the
                // computed reward is dropped at startStep time. For pv ≥ 7
                // (Babbage onward, ledger errata 17.2) the prefilter is
                // bypassed; routing of unregistered rewards happens at
                // applyRUpd time (frTotalUnregistered → treasury).
                if prev_protocol_version_major <= 6 && !registered_at_startstep(cred_hash) {
                    continue;
                }

                let member_stake = go
                    .stake_distribution
                    .get(cred_hash)
                    .copied()
                    .unwrap_or(Lovelace(0))
                    .0;

                if member_stake == 0 || pool_active_stake.0 == 0 {
                    continue;
                }

                let member_share = if pool_reward <= cost {
                    0u64
                } else {
                    let remainder = pool_reward - cost;
                    let one_minus_margin = Rat::from_i128(margin_den - margin_num, margin_den);
                    let member_frac =
                        Rat::from_i128(member_stake as i128, pool_active_stake.0 as i128);
                    Rat::from_i128(remainder as i128, 1)
                        .mul(&one_minus_margin)
                        .mul(&member_frac)
                        .floor_u64()
                };

                if member_share > 0 {
                    reward_entries.entry(*cred_hash).or_default().push((
                        true,
                        *pool_id,
                        member_share,
                    ));
                }
            }
        }

        if operator_reward > 0 {
            let op_key = LedgerState::reward_account_to_hash(&pool_reg.reward_account);
            // Pre-Babbage (pv<=6) leader-reward prefilter (Haskell `collectLRs`,
            // Cardano/Ledger/Shelley/Rewards.hs): include the leader reward iff
            // `hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered op`.
            // For pv>=7 (Babbage+, errata 17.2) the check is bypassed. A dropped
            // leader reward is never credited → stays in the pot → undistributed →
            // returned to reserves (matches Haskell deltaR2). Member rewards are
            // gated independently by the per-member prefilter above.
            let leader_included =
                prev_protocol_version_major >= 7 || registered_at_startstep(&op_key);
            if leader_included {
                reward_entries
                    .entry(op_key)
                    .or_default()
                    .push((false, *pool_id, operator_reward));
            }
        }
    }

    // Apply Haskell `filterRewards` (eras/shelley/.../Rewards.hs): at pv>2 sum all
    // rewards per credential (Allegra+ aggregation); at pv<=2 keep only the single
    // minimum reward per credential (`Set.deleteFindMin`: LeaderReward < MemberReward,
    // then ascending pool-id) and drop the rest. `total_distributed` counts only the
    // DELIVERED rewards so the dropped amounts flow back to reserves via `undistributed`
    // below — mirrors `sumRewards = fold (aggregateRewards pv rs)`, which sums only
    // the selected reward per credential (cardano-ledger master, verified).
    let aggregate_rewards = prev_protocol_version_major > 2;
    let mut reward_map: HashMap<Hash32, Lovelace> = HashMap::with_capacity(reward_entries.len());
    let mut total_distributed: u64 = 0;
    for (cred, mut entries) in reward_entries {
        let delivered = if aggregate_rewards {
            entries.iter().map(|(_, _, amt)| *amt).sum::<u64>()
        } else {
            // Set.deleteFindMin: leader (is_member=false) sorts before member, then
            // ascending pool-id; the minimum entry is the single delivered reward.
            entries.sort_unstable_by_key(|e| (e.0, e.1));
            entries.first().map(|(_, _, amt)| *amt).unwrap_or(0)
        };
        if delivered > 0 {
            reward_map.insert(cred, Lovelace(delivered));
            total_distributed += delivered;
        }
    }

    let undistributed = reward_pot.saturating_sub(total_distributed);

    // #615b: Haskell's `RewardUpdate` carries `deltaT = treasury_cut` only.
    // The undistributed portion of `reward_pot` (rewards never computed because
    // of below-pledge / zero-block / zero-stake pools) is refunded to RESERVES
    // via deltaR (not added to treasury). See `applyRUpdFiltered` in
    // eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/IncrementalStake.hs
    // and `completeStep` in eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs.
    //
    // The earlier #485-D2 fix mis-attributed undistributed to treasury based
    // on a misreading of completeStep — that hand-off only carries `deltaT1`
    // (the tau cut). The separate `frTotalUnregistered` adjustment (rewards
    // computed into rs'' for credentials that were deregistered between
    // snapshot and apply) IS routed to treasury, but that happens at apply
    // time in `apply_pending_reward_update` — see the unregistered→treasury
    // branch of the per-reward loop.
    //
    // Conservation (Haskell RUPD):
    //   deltaT = treasury_cut
    //   deltaR (Haskell signed: applied to reserves) = -expansion + undistributed
    //   ⇒ dugite delta_reserves (signed; positive DEBITS reserves, negative
    //     CREDITS reserves — see issue #796)
    //     = expansion - undistributed
    //     = (treasury_cut + reward_pot - epoch_fees) - undistributed
    //     = treasury_cut + (reward_pot - undistributed) - epoch_fees
    //     = treasury_cut + total_distributed - epoch_fees ✓
    //   In a degraded/low-block epoch, epoch_fees can exceed
    //   treasury_cut + total_distributed, making delta_reserves NEGATIVE —
    //   i.e. reserves grow. Haskell represents this with a signed
    //   `DeltaCoin`/`Integer`; dugite mirrors that with `i128` (#796). Prior
    //   to #796 this used `saturating_sub`, which floored the credit to 0
    //   and silently broke the six-pot conservation identity below.
    //
    // Six-pot: -delta_reserves + delta_treasury + total_distributed + undistributed
    //        − epoch_fees = -(expansion - undistributed) + treasury_cut
    //                       + total_distributed + undistributed - epoch_fees
    //                     = -expansion + treasury_cut + reward_pot - epoch_fees
    //                     = 0 ✓  (since expansion = treasury_cut + reward_pot - epoch_fees)
    let delta_treasury = treasury_cut;
    let delta_reserves = treasury_cut as i128 + total_distributed as i128 - epoch_fees as i128;

    debug!(
        "Rewards calculated: {} lovelace to {} accounts, \
         treasury_cut={} undistributed={} delta_treasury={} delta_reserves={} \
         (expansion: {}, fees: {})",
        total_distributed,
        reward_map.len(),
        treasury_cut,
        undistributed,
        delta_treasury,
        delta_reserves,
        expansion,
        epoch_fees
    );

    if epoch_fees > 0 {
        debug!(
            "Fee offset: delta_treasury={delta_treasury}, epoch_fees={epoch_fees}, \
             delta_reserves={delta_reserves}"
        );
    }
    PendingRewardUpdate {
        rewards: reward_map,
        delta_treasury,
        delta_reserves,
    }
}

/// Apply a signed RUPD `delta_reserves` (Haskell's `deltaR`, a signed
/// `DeltaCoin`/`Integer`) to the `u64` reserves pot.
///
/// `delta_reserves >= 0` DEBITS reserves (the normal monetary-expansion
/// case). `delta_reserves < 0` CREDITS reserves — reachable in a
/// degraded/low-block epoch where `epoch_fees` exceeds
/// `treasury_cut + total_distributed` (issue #796); Haskell's
/// `applyRUpdFiltered` refunds the difference to reserves via
/// `addDeltaCoin`, which can only ever increase a `u64` pot, never
/// underflow it, so the credit branch keeps a distinct overflow message
/// from the debit branch's underflow message.
pub(crate) fn apply_reserves_delta(reserves: u64, delta_reserves: i128) -> u64 {
    if delta_reserves >= 0 {
        reserves
            .checked_sub(delta_reserves as u64)
            .expect("RUPD delta_reserves exceeds reserves — ledger invariant broken")
    } else {
        reserves
            .checked_add((-delta_reserves) as u64)
            .expect("RUPD delta_reserves credit overflows reserves u64 — ledger invariant broken")
    }
}

impl LedgerState {
    /// Apply a pending reward update to the ledger state.
    ///
    /// This is called at the BEGINNING of an epoch transition to apply rewards
    /// computed during the previous epoch transition, matching Haskell's RUPD
    /// deferred application pattern.
    pub(crate) fn apply_pending_reward_update(&mut self) {
        if let Some(rupd) = self.epochs.pending_reward_update.take() {
            // Apply signed reserves adjustment (monetary expansion normally
            // debits reserves; a degraded epoch can credit them — #796).
            self.epochs.reserves.0 =
                apply_reserves_delta(self.epochs.reserves.0, rupd.delta_reserves);

            // Apply treasury increase (tau cut only; undistributed went to reserves
            // via delta_reserves above; per-reward unregistered → treasury below).
            self.epochs.treasury.0 = self
                .epochs
                .treasury
                .0
                .checked_add(rupd.delta_treasury)
                .expect("RUPD delta_treasury overflows treasury u64");

            // Apply per-account rewards (matching Haskell's applyRUpdFiltered):
            // registered credentials → reward account; unregistered → treasury.
            let mut total_applied = 0u64;
            let mut unregistered_total = 0u64;
            for (cred_hash, reward) in &rupd.rewards {
                if reward.0 > 0 {
                    if self.certs.reward_accounts.contains_key(cred_hash) {
                        *self
                            .certs
                            .reward_accounts
                            .entry(*cred_hash)
                            .or_insert(Lovelace(0)) += *reward;
                        total_applied += reward.0;
                    } else {
                        self.epochs.treasury.0 = self
                            .epochs
                            .treasury
                            .0
                            .checked_add(reward.0)
                            .expect("treasury overflow on undistributed reward");
                        unregistered_total += reward.0;
                    }
                }
            }

            debug!(
                "Applied pending reward update: {} lovelace to {} accounts \
                 ({} unregistered→treasury), treasury +{}, reserves -{}",
                total_applied,
                rupd.rewards.len(),
                unregistered_total,
                rupd.delta_treasury,
                rupd.delta_reserves,
            );
        }
    }

    /// Calculate rewards using the GO snapshot and a separate fee value.
    ///
    /// Legacy entry point that uses GO snapshot for both stake AND block data.
    #[allow(dead_code)]
    pub(crate) fn calculate_rewards_with_fee(
        &self,
        go_snapshot: &StakeSnapshot,
        ss_fee: Lovelace,
    ) -> PendingRewardUpdate {
        self.calculate_rewards_inner(go_snapshot, go_snapshot, ss_fee.0)
    }

    /// Calculate rewards matching Haskell's `startStep` exactly.
    ///
    /// Uses THREE separate data sources:
    /// - `go_snapshot`: ssStakeGo — stake distribution, delegations, pool params (2 epochs ago)
    /// - `bprev_snapshot`: nesBprev equivalent — block production counts (1 epoch ago, from SET)
    /// - `ss_fee`: ssFee — fee pot from SNAP at previous boundary
    pub(crate) fn calculate_rewards_full(
        &self,
        go_snapshot: &StakeSnapshot,
        bprev_snapshot: &StakeSnapshot,
        ss_fee: Lovelace,
    ) -> PendingRewardUpdate {
        self.calculate_rewards_inner(go_snapshot, bprev_snapshot, ss_fee.0)
    }

    /// Calculate rewards and return a PendingRewardUpdate for deferred application.
    ///
    /// Implements the formula from cardano-ledger-shelley:
    ///   - maxPool'(a0, nOpt, R, sigma, p) for pledge-influenced pool rewards
    ///   - mkApparentPerformance for beta/sigma performance calculation
    ///   - Pledge verification (pool gets zero if owner stake < declared pledge)
    ///   - Operator reward includes self-delegation share (margin + proportional)
    ///   - Operator reward goes to pool's registered reward account
    ///
    /// Legacy entry point that reads fees from the snapshot itself. New code
    /// should use `calculate_rewards_full` which separates GO/bprev/fees.
    #[cfg(test)]
    pub(crate) fn calculate_rewards(&self, rupd_snapshot: &StakeSnapshot) -> PendingRewardUpdate {
        self.calculate_rewards_inner(rupd_snapshot, rupd_snapshot, rupd_snapshot.epoch_fees.0)
    }

    /// Inner reward calculation — thin wrapper around [`compute_reward_update`].
    ///
    /// `stake_snapshot`: provides stake distribution, delegations, pool params (GO)
    /// `block_snapshot`: provides epoch_block_count, epoch_blocks_by_pool (nesBprev/SET)
    /// `epoch_fees`: ssFee from SNAP
    fn calculate_rewards_inner(
        &self,
        stake_snapshot: &StakeSnapshot,
        block_snapshot: &StakeSnapshot,
        epoch_fees: u64,
    ) -> PendingRewardUpdate {
        // compute_reward_update expects &std::HashMap; convert at call site.
        let reward_accounts_std: std::collections::HashMap<_, _> = self
            .certs
            .reward_accounts
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        compute_reward_update(
            &self.epochs.prev_protocol_params,
            &self.epochs.prev_d,
            self.epochs.prev_protocol_version_major,
            Some(stake_snapshot),
            &block_snapshot.epoch_blocks_by_pool,
            Lovelace(epoch_fees),
            self.epochs.reserves,
            self.epochs.treasury,
            &reward_accounts_std,
            self.epochs.rupd_addrs_rew.as_deref(),
            self.epoch_length,
            self.shelley_transition_epoch,
            self.max_lovelace_supply,
        )
    }

    /// Legacy compatibility: calculate and immediately distribute rewards.
    ///
    /// Used by tests that expect immediate reward application. New code should
    /// use `calculate_rewards()` + apply at the epoch boundary for correct
    /// Haskell-compatible RUPD timing.
    #[cfg(test)]
    pub(crate) fn calculate_and_distribute_rewards(&mut self, rupd_snapshot: StakeSnapshot) {
        // Use self.utxo.epoch_fees (matching the live path which uses ss_fee from SNAP).
        // Tests set state.utxo.epoch_fees before calling this function.
        let rupd =
            self.calculate_rewards_inner(&rupd_snapshot, &rupd_snapshot, self.utxo.epoch_fees.0);
        // Apply immediately (legacy behavior for test compatibility)
        self.epochs.reserves.0 = apply_reserves_delta(self.epochs.reserves.0, rupd.delta_reserves);
        self.epochs.treasury.0 = self
            .epochs
            .treasury
            .0
            .checked_add(rupd.delta_treasury)
            .expect("RUPD delta_treasury overflows treasury u64");
        for (cred_hash, reward) in &rupd.rewards {
            if reward.0 > 0 {
                *self
                    .certs
                    .reward_accounts
                    .entry(*cred_hash)
                    .or_insert(Lovelace(0)) += *reward;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_reserves_delta, Rat};
    use crate::state::{PoolRegistration, StakeSnapshot};
    use dugite_primitives::value::Lovelace;
    use dugite_primitives::{EpochNo, Hash28, Hash32};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Exact `Rational` literal (protocol parameters are exact rationals).
    fn rat(numerator: u64, denominator: u64) -> dugite_primitives::transaction::Rational {
        dugite_primitives::transaction::Rational {
            numerator,
            denominator,
        }
    }

    // -----------------------------------------------------------------------
    // GCD correctness
    // -----------------------------------------------------------------------

    #[test]
    fn test_gcd_coprime_numbers() {
        // 13 and 17 are coprime
        let r = Rat::from_i128(13, 17);
        assert_eq!(r.n, 13.into());
        assert_eq!(r.d, 17.into());
    }

    #[test]
    fn test_gcd_reduces_fractions() {
        let r = Rat::from_i128(6, 9);
        assert_eq!(r.n, 2.into());
        assert_eq!(r.d, 3.into());
    }

    #[test]
    fn test_gcd_large_values() {
        // GCD(2^60, 2^40) = 2^40
        let a = 1i128 << 60;
        let b = 1i128 << 40;
        let r = Rat::from_i128(a, b);
        assert_eq!(r.n, (1i128 << 20).into());
        assert_eq!(r.d, 1.into());
    }

    // -----------------------------------------------------------------------
    // Rat multiplication with large values
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_mul_near_i128_max() {
        // Two large values — BigInt handles this correctly
        let a = Rat::from_i128(i128::MAX / 2, 1);
        let b = Rat::from_i128(3, 1);
        let result = a.mul(&b);
        assert!(result.d > 0.into());
        assert!(result.n > 0.into());
        // Should be exactly (MAX/2)*3, no saturation
        let expected = num_bigint::BigInt::from(i128::MAX / 2) * num_bigint::BigInt::from(3);
        assert_eq!(result.n, expected);
    }

    #[test]
    fn test_rat_mul_cross_reduce_prevents_overflow() {
        let a = Rat::from_i128(1_000_000_000_000_000, 7);
        let b = Rat::from_i128(7, 1_000_000_000_000_000);
        let result = a.mul(&b);
        assert_eq!(result.n, 1.into());
        assert_eq!(result.d, 1.into());
    }

    // -----------------------------------------------------------------------
    // Rat addition with large values
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_add_near_i128_max() {
        let a = Rat::from_i128(i128::MAX / 2, 1);
        let b = Rat::from_i128(i128::MAX / 2, 1);
        let result = a.add(&b);
        assert!(result.n > 0.into());
        assert!(result.d > 0.into());
        // Should be exact, no saturation
        let expected = num_bigint::BigInt::from(i128::MAX / 2) * 2;
        assert_eq!(result.n, expected);
    }

    #[test]
    fn test_rat_add_different_denominators() {
        let a = Rat::from_i128(1, 3);
        let b = Rat::from_i128(1, 6);
        let result = a.add(&b);
        assert_eq!(result.n, 1.into());
        assert_eq!(result.d, 2.into());
    }

    // -----------------------------------------------------------------------
    // Division producing very small fractions
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_div_very_small_fraction() {
        let a = Rat::from_i128(1, 1_000_000_000);
        let b = Rat::from_i128(1_000_000_000, 1);
        let result = a.div(&b);
        assert_eq!(result.n, 1.into());
        assert_eq!(result.d, 1_000_000_000_000_000_000i128.into());
    }

    #[test]
    fn test_rat_div_by_zero_returns_zero() {
        let a = Rat::from_i128(5, 3);
        let b = Rat::from_i128(0, 1);
        let result = a.div(&b);
        assert_eq!(result.n, 0.into());
    }

    // -----------------------------------------------------------------------
    // Negative Rat values
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_negative_numerator() {
        let r = Rat::from_i128(-3, 4);
        assert_eq!(r.n, (-3).into());
        assert_eq!(r.d, 4.into());
    }

    #[test]
    fn test_rat_negative_denominator_normalized() {
        let r = Rat::from_i128(3, -4);
        assert_eq!(r.n, (-3).into());
        assert_eq!(r.d, 4.into());
    }

    #[test]
    fn test_rat_both_negative() {
        let r = Rat::from_i128(-6, -8);
        assert_eq!(r.n, 3.into());
        assert_eq!(r.d, 4.into());
    }

    #[test]
    fn test_rat_sub_produces_negative() {
        let a = Rat::from_i128(1, 4);
        let b = Rat::from_i128(3, 4);
        let result = a.sub(&b);
        assert_eq!(result.n, (-1).into());
        assert_eq!(result.d, 2.into());
    }

    // -----------------------------------------------------------------------
    // Floor
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_floor_u64_negative_returns_zero() {
        let r = Rat::from_i128(-5, 3);
        assert_eq!(r.floor_u64(), 0);
    }

    #[test]
    fn test_rat_floor_u64_exact_division() {
        let r = Rat::from_i128(10, 5);
        assert_eq!(r.floor_u64(), 2);
    }

    #[test]
    fn test_rat_floor_u64_truncates() {
        let r = Rat::from_i128(7, 3);
        assert_eq!(r.floor_u64(), 2); // 7/3 = 2.333...
    }

    // -----------------------------------------------------------------------
    // min_rat
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_min_rat() {
        let a = Rat::from_i128(1, 3);
        let b = Rat::from_i128(1, 2);
        assert_eq!(a.min_rat(&b), a);
        assert_eq!(b.min_rat(&a), a);
    }

    #[test]
    fn test_rat_min_rat_equal() {
        let a = Rat::from_i128(2, 4);
        let b = Rat::from_i128(1, 2);
        let result = a.min_rat(&b);
        assert_eq!(result.n, 1.into());
        assert_eq!(result.d, 2.into());
    }

    // -----------------------------------------------------------------------
    // Zero denominator
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_zero_denominator() {
        let r = Rat::from_i128(5, 0);
        assert_eq!(r.n, 0.into());
        assert_eq!(r.d, 1.into());
    }

    // -----------------------------------------------------------------------
    // Mainnet-scale precision tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rat_mainnet_scale_sigma_chain() {
        // Reproduce the exact computation chain from maxPool' with
        // mainnet-scale values (36T circulation denominator).
        // This MUST NOT overflow or saturate.
        let pool_stake: i128 = 4_733_011_000_060;
        let circulation: i128 = 36_706_064_193_192_852;
        let pledge: i128 = 100_000_000_000;
        let n_opt: i128 = 500;
        let reward_pot: i128 = 1_000_000_000_000; // 1T

        let a0 = Rat::from_i128(3, 10);
        let z0 = Rat::from_i128(1, n_opt);
        let sigma = Rat::from_i128(pool_stake, circulation).min_rat(&z0);
        let p = Rat::from_i128(pledge, circulation).min_rat(&z0);

        let f4 = z0.sub(&sigma).div(&z0);
        let f3 = sigma.sub(&p.mul(&f4)).div(&z0);
        let f2 = sigma.add(&p.mul(&a0).mul(&f3));
        let f1 = Rat::from_i128(reward_pot, 1).div(&Rat::from_i128(1, 1).add(&a0));
        let max_pool = f1.mul(&f2).floor_u64();

        // sigma ~ 0.000129 < z0 = 0.002, so sigma is NOT capped
        // maxPool should be approximately R/1.3 * sigma ~ 1T/1.3 * 0.000129 ~ 99M
        assert!(
            max_pool > 90_000_000 && max_pool < 110_000_000,
            "maxPool at mainnet scale should be ~99M for R=1T, got {max_pool}"
        );

        // Verify it's not the buggy saturated value (769B)
        assert!(
            max_pool < 1_000_000_000,
            "maxPool must not be the saturated value"
        );
    }

    // -----------------------------------------------------------------------
    // maxPool' formula unit tests
    // -----------------------------------------------------------------------

    fn max_pool_prime(
        a0_num: i128,
        a0_den: i128,
        n_opt: u64,
        reward_pot: u64,
        pool_stake: u64,
        pledge: u64,
        total_stake: u64,
    ) -> u64 {
        let a0 = Rat::from_i128(a0_num, a0_den);
        let z0 = Rat::from_i128(1, n_opt as i128);
        let sigma_raw = Rat::from_i128(pool_stake as i128, total_stake as i128);
        let p_raw = Rat::from_i128(pledge as i128, total_stake as i128);
        let sigma = sigma_raw.min_rat(&z0);
        let p = p_raw.min_rat(&z0);

        let f4 = z0.sub(&sigma).div(&z0);
        let f3 = sigma.sub(&p.mul(&f4)).div(&z0);
        let f2 = sigma.add(&p.mul(&a0).mul(&f3));
        let f1 = Rat::from_i128(reward_pot as i128, 1).div(&Rat::from_i128(1, 1).add(&a0));
        f1.mul(&f2).floor_u64()
    }

    #[test]
    fn test_max_pool_saturated_pool() {
        let result = max_pool_prime(3, 10, 500, 10_000_000_000, 10_000, 0, 1_000_000);
        assert_eq!(result, 15_384_615);
    }

    #[test]
    fn test_max_pool_unsaturated_zero_pledge() {
        let result = max_pool_prime(3, 10, 500, 10_000_000_000, 1_000, 0, 1_000_000);
        assert_eq!(result, 7_692_307);
    }

    #[test]
    fn test_max_pool_pledge_influence() {
        let no_pledge = max_pool_prime(3, 10, 500, 10_000_000_000, 1_000, 0, 1_000_000);
        let with_pledge = max_pool_prime(3, 10, 500, 10_000_000_000, 1_000, 500, 1_000_000);
        assert!(
            with_pledge > no_pledge,
            "Pledge should increase maxPool reward"
        );
    }

    #[test]
    fn test_max_pool_a0_zero_no_pledge_influence() {
        let no_pledge = max_pool_prime(0, 1, 500, 10_000_000_000, 1_000, 0, 1_000_000);
        let with_pledge = max_pool_prime(0, 1, 500, 10_000_000_000, 1_000, 500, 1_000_000);
        assert_eq!(no_pledge, with_pledge);
    }

    // -----------------------------------------------------------------------
    // Cross-validation against real Koios on-chain data (preview testnet)
    // -----------------------------------------------------------------------

    #[test]
    fn test_koios_pool_fee_split() {
        let total_pool_reward: u64 = 578_845_970 + 2_149_613_734;
        assert_eq!(total_pool_reward, 2_728_459_704);

        let cost = 340_000_000u64;
        let margin = Rat::from_i128(1, 10);
        let remainder = total_pool_reward - cost;

        let expected_pool_fees = cost
            + margin
                .mul(&Rat::from_i128(remainder as i128, 1))
                .floor_u64();
        assert_eq!(expected_pool_fees, 578_845_970);

        let one_minus_margin = Rat::from_i128(9, 10);
        let expected_deleg_rewards = one_minus_margin
            .mul(&Rat::from_i128(remainder as i128, 1))
            .floor_u64();
        // Koios: 2,149,613,734. floor(9/10 * 2,388,459,704) = 2,149,613,733.
        // 1 lovelace gap: cardano-node computes member_rewards = total - leader_share
        // (subtraction) rather than independent floor, avoiding double-floor loss.
        assert!(
            (expected_deleg_rewards as i64 - 2_149_613_734i64).unsigned_abs() <= 1,
            "deleg_rewards off by >1: got {expected_deleg_rewards}"
        );
    }

    #[test]
    fn test_koios_max_pool_and_performance() {
        let pool_stake: u64 = 4_733_011_000_060;
        let pledge: u64 = 100_000_000_000;
        let total_active_stake: u64 = 1_177_946_537_741_239;
        let circulation: u64 = 45_000_000_000_000_000 - 8_293_935_806_807_148;
        let blocks_made: u64 = 24;
        let total_blocks: u64 = 2578;

        // Apparent performance uses sigmaA (total_active_stake)
        let perf = Rat::from_i128(blocks_made as i128, total_blocks as i128).mul(&Rat::from_i128(
            total_active_stake as i128,
            pool_stake as i128,
        ));

        let perf_approx = {
            let n: i128 = (&perf.n).try_into().unwrap_or(i128::MAX);
            let d: i128 = (&perf.d).try_into().unwrap_or(i128::MAX);
            n as f64 / d as f64
        };
        assert!(
            (perf_approx - 2.317).abs() < 0.01,
            "Performance should be ~2.317, got {perf_approx}"
        );

        // maxPool uses sigma = pool_stake / circulation (NOT total_active_stake)
        let max_pool_1t = max_pool_prime(
            3,
            10,
            500,
            1_000_000_000_000,
            pool_stake,
            pledge,
            circulation,
        );

        let pool_reward_per_1t = perf
            .mul(&Rat::from_i128(max_pool_1t as i128, 1))
            .floor_u64();

        let known_total_pool_reward: u64 = 2_728_459_704;
        let reward_pot = Rat::from_i128(known_total_pool_reward as i128, 1)
            .mul(&Rat::from_i128(
                1_000_000_000_000,
                pool_reward_per_1t as i128,
            ))
            .floor_u64();

        let max_pool = max_pool_prime(3, 10, 500, reward_pot, pool_stake, pledge, circulation);
        let computed_pool_reward = perf.mul(&Rat::from_i128(max_pool as i128, 1)).floor_u64();

        // Back-computation through multiple floor() operations loses precision.
        // The actual forward calculation (with exact R from epoch data) is exact.
        let diff = (computed_pool_reward as i64 - known_total_pool_reward as i64).unsigned_abs();
        assert!(
            diff <= 10,
            "maxPool' * perf should reproduce Koios pool reward within tolerance: \
             computed={computed_pool_reward}, expected={known_total_pool_reward}, diff={diff}"
        );
    }

    #[test]
    fn test_koios_operator_member_split() {
        let total_reward = 2_728_459_704u64;
        let cost = 340_000_000u64;
        let margin = Rat::from_i128(1, 10);
        let one_minus_margin = Rat::from_i128(9, 10);
        let remainder = total_reward - cost;

        let deleg_rewards = one_minus_margin
            .mul(&Rat::from_i128(remainder as i128, 1))
            .floor_u64();
        assert!(
            (deleg_rewards as i64 - 2_149_613_734i64).unsigned_abs() <= 1,
            "deleg_rewards off by >1: got {deleg_rewards}"
        );

        let pool_fees = cost
            + margin
                .mul(&Rat::from_i128(remainder as i128, 1))
                .floor_u64();
        assert_eq!(pool_fees, 578_845_970, "pool_fees mismatch");
    }

    #[test]
    fn test_compute_reward_update_free_fn() {
        // Call the free function directly with no GO snapshot — should return empty rewards.
        let params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        let bprev_blocks_by_pool = std::collections::HashMap::new();
        let reward_accounts = std::collections::HashMap::new();

        let zero_d = dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        };
        let rupd = super::compute_reward_update(
            &params,
            &zero_d, // prev_d
            8,       // prev_protocol_version_major (Conway)
            None,    // no GO snapshot
            &bprev_blocks_by_pool,
            dugite_primitives::value::Lovelace(0), // ss_fee
            dugite_primitives::value::Lovelace(0), // reserves
            dugite_primitives::value::Lovelace(0), // treasury
            &reward_accounts,
            None,  // startstep_addrs_rew (fall back to boundary accounts)
            86400, // epoch_length
            0,     // shelley_transition_epoch
            super::super::MAX_LOVELACE_SUPPLY,
        );

        assert!(
            rupd.rewards.is_empty(),
            "No GO snapshot should yield empty rewards"
        );
        assert_eq!(rupd.delta_treasury, 0);
        assert_eq!(rupd.delta_reserves, 0);
    }

    /// Regression test for issue #796: in a degraded/low-block epoch where
    /// `epoch_fees` alone dwarfs `treasury_cut` (no pools ⇒
    /// `total_distributed == 0`), Haskell's signed `deltaR` CREDITS
    /// reserves rather than silently saturating the credit to 0. Hits the
    /// `total_stake == 0` branch of `compute_reward_update` (reserves ==
    /// max_lovelace_supply).
    #[test]
    fn test_degraded_epoch_credits_reserves_796() {
        let mut params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        // Round numbers so expected values are easy to hand-verify.
        params.rho = dugite_primitives::transaction::Rational {
            numerator: 1,
            denominator: 10,
        };
        params.tau = dugite_primitives::transaction::Rational {
            numerator: 1,
            denominator: 10,
        };

        let bprev_blocks_by_pool = std::collections::HashMap::new();
        let reward_accounts = std::collections::HashMap::new();
        let d_ge_4_5 = dugite_primitives::transaction::Rational {
            numerator: 1,
            denominator: 1,
        };

        const MAX_SUPPLY: u64 = 1_000_000;
        const RESERVES: u64 = 1_000_000; // == MAX_SUPPLY ⇒ total_stake == 0
        const EPOCH_FEES: u64 = 10_000_000; // dwarfs expansion ⇒ degraded epoch

        let rupd = super::compute_reward_update(
            &params,
            &d_ge_4_5, // prev_d >= 4/5 ⇒ full expansion, no eta scaling
            9,         // prev_protocol_version_major (Conway)
            None,      // no GO snapshot — irrelevant, total_stake==0 short-circuits first
            &bprev_blocks_by_pool,
            dugite_primitives::value::Lovelace(EPOCH_FEES),
            dugite_primitives::value::Lovelace(RESERVES),
            dugite_primitives::value::Lovelace(0),
            &reward_accounts,
            None,
            86_400,
            0,
            MAX_SUPPLY,
        );

        // expansion = floor(rho * reserves) = floor(0.1 * 1_000_000) = 100_000
        // total_rewards_available = expansion + epoch_fees = 10_100_000
        // treasury_cut = floor(tau * total_rewards_available) = 1_010_000
        // delta_reserves = treasury_cut - epoch_fees = 1_010_000 - 10_000_000 = -8_990_000
        assert_eq!(rupd.delta_treasury, 1_010_000);
        assert_eq!(
            rupd.delta_reserves, -8_990_000,
            "degraded epoch (fees >> expansion) must CREDIT reserves \
             (negative delta_reserves), not saturate the credit to 0"
        );
        assert!(
            rupd.delta_reserves < 0,
            "reserves must increase when epoch_fees exceeds treasury_cut"
        );

        // Pot-conservation identity: the only new lovelace entering the
        // (reserves, treasury, distributed-to-accounts) system this
        // boundary is `epoch_fees` (monetary expansion just moves reserves'
        // own money around, net of what comes back as `delta_reserves`).
        // So: -delta_reserves + delta_treasury + total_distributed ==
        // epoch_fees, for ANY split between treasury/reserves/distributed —
        // this holds independently of which branch of
        // `compute_reward_update` produced the values, and is exactly the
        // identity that `saturating_sub` used to silently violate by
        // flooring a would-be-negative `delta_reserves` to 0 (issue #796).
        let total_distributed = 0i128; // no pools in this branch
        let conservation = -rupd.delta_reserves + rupd.delta_treasury as i128 + total_distributed;
        assert_eq!(
            conservation, EPOCH_FEES as i128,
            "pot conservation identity (-delta_reserves + delta_treasury + total_distributed \
             == epoch_fees) must hold"
        );

        // Applying the signed delta must INCREASE the u64 reserves pot
        // without panicking (this is exactly what all 7 RUPD apply sites do).
        let new_reserves = apply_reserves_delta(RESERVES, rupd.delta_reserves);
        assert_eq!(new_reserves, RESERVES + 8_990_000);
    }

    /// Regression scaffold for GitHub issue #438 (preview epoch 1268 leader
    /// reward 3,505 lovelace too high).
    ///
    /// Koios oracle (preview, account
    /// stake_test1uz7xx6hy2xnnrmz0av0xl7qn9vdkhage7myf0nd49e7mvcg6z0smn,
    /// pool14rn9dq87dgj2z8g3lp4n0a78fewxff3gkgjkmz72ew44ym79xpp):
    ///
    ///   earned_epoch=1268 spendable_epoch=1270 leader_reward = 352_901_742
    ///
    /// Dugite at that boundary computed 352_905_247 (diff = +3505). This
    /// scaffold pins the known leader reward; a follow-up patch must load a
    /// real GO snapshot + bprev counts for epoch 1268 and feed them through
    /// `compute_reward_update` to assert byte-equality. Marked `#[ignore]`
    /// because the snapshot file is not yet checked into the repo.
    #[test]
    #[ignore = "needs preview epoch 1268 GO snapshot fixture — see #438"]
    fn test_koios_preview_epoch_1268_leader_reward_issue_438() {
        // Canonical Koios oracle values for the regression target.
        let expected_leader_reward: u64 = 352_901_742;
        let cost: u64 = 340_000_000; // pool14rn9dq... fixed cost
        let margin_num: i128 = 1; // 5%
        let margin_den: i128 = 20;

        // Placeholder — to be loaded from a checked-in snapshot fixture.
        // Once the fixture exists, the test should:
        //   1. Load GO snapshot + bprev_blocks_by_pool from epoch 1267.
        //   2. Load prev_protocol_params and prev_d from end of epoch 1267.
        //   3. Call compute_reward_update(...).
        //   4. Assert rupd.rewards[owner_cred] == expected_leader_reward.
        let _ = (expected_leader_reward, cost, margin_num, margin_den);
    }

    /// Synthetic, byte-exact replica of the Haskell `leaderRew` + `memberRew`(owner)
    /// computation for preview epoch 1268 (pool14rn9dq…, GitHub issue #438).
    ///
    /// Inputs are drawn from Koios oracle data (preview, epoch 1268):
    /// - `pool_history`: active_stake=1_597_168_222_937, block_cnt=5,
    ///   margin=1/20, fixed_cost=340_000_000, pool_fees=352_823_650,
    ///   deleg_rewards=243_649_340, member_rewards=243_571_248
    /// - `pool_info`: pledge=0 (preview pool with declared pledge 0)
    /// - `account_stake_history(epoch_no=1268)`: owner active_stake=511_912_077
    ///
    /// Decomposition (cross-validated against `cardano-ledger` `rewardOnePool`):
    /// - `R_pool = pool_fees + deleg_rewards = 596_472_990`
    /// - `remainder = R_pool − cost = 256_472_990`
    /// - `s/σ = owner_stake / pool_stake = 511_912_077 / 1_597_168_222_937`
    /// - `leaderRew = cost + floor(remainder × (m + (1−m) × s/σ))`
    ///   = 340_000_000 + floor(12_901_742.11…) = 352_901_742
    /// - `memberRew(owner) = floor(remainder × (1−m) × owner/pool) = 78_092`
    ///
    /// dugite folds `leaderRew + memberRew(owner)` into a single `operator_reward`
    /// for single-owner pools where owner credential == reward account credential
    /// (rewards.rs:367-414).  For this preview pool the owner is a single KeyHash
    /// equal to the reward account credential, so the dugite-computed
    /// `operator_reward` should equal Haskell's `leaderRew` directly (since the
    /// owner is then skipped in the member loop and the (1−m)·s/σ term IS the
    /// owner-as-member share, mathematically equal to `memberRew(owner)`).
    ///
    /// Expected dugite output: 352_901_742 (Koios-implied account credit).
    /// Observed dugite output on live preview replay: 352_905_247 (+3505).
    ///
    /// This test proves that, given the CORRECT owner stake input, the dugite
    /// formula reproduces Haskell exactly — so the 3505-lovelace divergence
    /// CANNOT live in the formula itself.  The bug therefore lives in the
    /// snapshot construction (issue #438 suspect 1: owner_stake_by_pool drifts).
    #[test]
    fn test_issue_438_pool_1268_synthetic_leader_reward() {
        let pool_stake: u64 = 1_597_168_222_937;
        let owner_stake: u64 = 511_912_077;
        let cost: u64 = 340_000_000;
        let margin = Rat::from_i128(1, 20);
        let one_minus_margin = Rat::from_i128(19, 20);
        // From Koios: pool_fees + deleg_rewards = pool reward pot R_pool.
        let r_pool: u64 = 352_823_650 + 243_649_340; // 596_472_990
        let remainder = r_pool - cost; // 256_472_990

        // Haskell leaderRew = cost + floor(remainder × (m + (1−m) × owner/pool))
        // dugite operator_reward computed identically when s = owner stake.
        let s_over_sigma = Rat::from_i128(owner_stake as i128, pool_stake as i128);
        let share = margin.add(&one_minus_margin.mul(&s_over_sigma));
        let op_extra = share.mul(&Rat::from_i128(remainder as i128, 1)).floor_u64();
        let leader_reward = cost + op_extra;

        // Koios account credit at epoch 1268 = pool_fees + owner_as_member
        //                                    = 352_823_650 + (243_649_340 − 243_571_248)
        //                                    = 352_823_650 + 78_092 = 352_901_742
        let koios_expected: u64 = 352_823_650 + (243_649_340 - 243_571_248);
        assert_eq!(
            koios_expected, 352_901_742,
            "Koios decomposition arithmetic must match the issue oracle"
        );

        assert_eq!(
            leader_reward, koios_expected,
            "dugite leader formula MUST reproduce Haskell when given correct \
             owner_stake=511_912_077; mismatch here would indicate a formula bug, \
             but a match isolates issue #438 to snapshot/owner-stake construction"
        );

        // Independent memberRew(owner) check — should match Koios `owner_as_member`.
        let mem_owner = one_minus_margin
            .mul(&Rat::from_i128(owner_stake as i128, pool_stake as i128))
            .mul(&Rat::from_i128(remainder as i128, 1))
            .floor_u64();
        assert_eq!(
            mem_owner, 78_092,
            "memberRew(owner) decomposition must equal Koios `deleg_rewards − member_rewards`"
        );

        // The dugite live-replay overshoot (3505 lovelace) implies owner_stake
        // is inflated by ≈ 22_980_000 lovelace at the GO snapshot read site.
        // We document the back-calculation here so a future fix can target the
        // exact owner-stake delta to eliminate.
        let observed_dugite: u64 = 352_905_247; // from issue #438
        let diff = observed_dugite - leader_reward;
        assert_eq!(diff, 3505, "Issue #438 observed overshoot is 3505 lovelace");
        // Implied owner_stake inflation:
        //   diff = (1−m) × remainder × Δowner / pool_stake
        //   Δowner = diff × pool_stake / ((1−m) × remainder)
        //          ≈ 22_979_768 lovelace ≈ 22.98 ADA
        let implied_delta_num = (diff as u128) * (pool_stake as u128) * 20;
        let implied_delta_den = (remainder as u128) * 19;
        let implied_delta = (implied_delta_num / implied_delta_den) as u64;
        // Loose band — exact value depends on floor truncation order.
        assert!(
            (22_900_000..=23_100_000).contains(&implied_delta),
            "implied owner-stake inflation should be ~22.98 ADA, got {implied_delta}"
        );
    }

    /// Byte-exact pin of `deltaR1` (the reserves-draw / monetary-expansion term)
    /// for the mainnet RUPD applied at the 245→246 epoch boundary.
    ///
    /// This is the LAST untested global term feeding every member reward
    /// (verification handoff for issue #438). The RUPD applied at boundary
    /// 245→246 is computed during epoch 245 and uses `prevPParams` = the params
    /// active during epoch 244 (Conway, d=0) plus `nesBprev` = blocks made
    /// during epoch 244.
    ///
    /// Canonical Haskell `startStep`
    /// (`eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs`):
    ///
    /// ```haskell
    /// expectedBlocks = floor $ (1 - d) * f * slotsPerEpoch
    /// blocksMade     = fromIntegral $ Map.foldr (+) 0 b'
    /// eta | d >= 0.8  = 1
    ///     | otherwise = blocksMade % expectedBlocks
    /// deltaR1 = rationalToCoinViaFloor $
    ///             min 1 eta * unboundRational (pr ^. ppRhoL) * fromIntegral reserves
    /// ```
    ///
    /// Koios oracle (mainnet):
    /// - epoch_info(244): `blk_count = 3920`, `fees = 587_936_590`
    /// - epoch_params(244): `decentralisation = 0`, `monetary_expand_rate = 0.003`
    /// - genesis: `activeSlotCoeff = 1/20`, `epochLength = 86_400`
    /// - totals(245): `reserves = 13_152_157_804_897_520`
    ///
    /// With d = 0 < 4/5:
    ///   expectedBlocks = floor((1−0) · (1/20) · 86_400) = floor(4320) = 4320
    ///   blocksMade     = 3920  (≤ expectedBlocks ⇒ min(1, eta) = eta)
    ///   eta            = 3920 / 4320  (EXACT Rational)
    ///   deltaR1        = floor( (3920/4320) · (3/1000) · 13_152_157_804_897_520 )
    ///                  = 35_803_096_246_665
    ///
    /// This reproduces the EXACT production code path (rewards.rs ~199-231):
    /// expectedBlocks is floored once; eta is kept as an exact Rational; the
    /// final `floor` is applied once after multiplying through. `min(1, eta)`
    /// is implemented by capping `effective_blocks = min(actual, expected)`,
    /// which is algebraically identical (proven below for both branches).
    #[test]
    fn test_mainnet_ep246_delta_r1_reserves_draw_byte_exact() {
        // prevPParams (epoch 244) + genesis inputs.
        let d_num: i128 = 0; // decentralisation = 0 (Conway)
        let d_den: i128 = 1;
        let rho = Rat::from_i128(3, 1000); // monetary_expand_rate = 0.003
        let f = Rat::from_i128(1, 20); // activeSlotCoeff = 0.05
        let epoch_length: i128 = 86_400;
        let reserves: i128 = 13_152_157_804_897_520;
        let actual_blocks: u64 = 3920; // nesBprev = blk_count(epoch 244)

        // Overlay gate: d >= 4/5  ⟺  5·d_num >= 4·d_den. Here 0 >= 4 is false.
        let d_ge_4_5 = 5 * d_num >= 4 * d_den;
        assert!(!d_ge_4_5, "epoch-244 d=0 must take the eta-scaled branch");

        // expectedBlocks = floor((1 − d) · f · slotsPerEpoch), exact then floor once.
        let one_minus_d = Rat::from_i128(d_den - d_num, d_den);
        let expected_blocks = one_minus_d
            .mul(&f)
            .mul(&Rat::from_i128(epoch_length, 1))
            .floor_u64()
            .max(1);
        assert_eq!(expected_blocks, 4320, "expectedBlocks must floor to 4320");

        // min(1, eta) via effective_blocks = min(actual, expected).
        let effective_blocks = actual_blocks.min(expected_blocks);
        assert_eq!(
            effective_blocks, 3920,
            "blocksMade ≤ expectedBlocks ⇒ effective = blocksMade"
        );

        // deltaR1 = floor( rho · reserves · (effective/expected) ), single floor.
        let delta_r1 = rho
            .mul(&Rat::from_i128(reserves, 1))
            .mul(&Rat::from_i128(
                effective_blocks as i128,
                expected_blocks as i128,
            ))
            .floor_u64();

        assert_eq!(
            delta_r1, 35_803_096_246_665,
            "deltaR1 (mainnet 245→246 reserves draw) MUST be byte-exact with \
             Haskell startStep: floor(min(1,eta)·rho·reserves)"
        );

        // Cross-check the algebraic identity used in production: the alternative
        // Haskell phrasing min(1, eta)·rho·reserves with eta = blocksMade %
        // expectedBlocks (exact Rational) yields the SAME floored value.
        let eta = Rat::from_i128(actual_blocks as i128, expected_blocks as i128);
        let min_1_eta = eta.min_rat(&Rat::from_i128(1, 1));
        let delta_r1_alt = min_1_eta
            .mul(&rho)
            .mul(&Rat::from_i128(reserves, 1))
            .floor_u64();
        assert_eq!(
            delta_r1, delta_r1_alt,
            "effective/expected form and min(1,eta) form must be byte-identical"
        );

        // Downstream reward-pot terms pinned for the same boundary (tau=0.2):
        //   rPot     = deltaR1 + ssFee
        //   deltaT1  = floor(tau · rPot)
        //   R        = rPot − deltaT1
        let ss_fee: u64 = 587_936_590; // go-snapshot ssFee (epoch-244 fees)
        let r_pot = delta_r1 + ss_fee;
        let tau = Rat::from_i128(2, 10);
        let delta_t1 = tau.mul(&Rat::from_i128(r_pot as i128, 1)).floor_u64();
        let big_r = r_pot - delta_t1;
        assert_eq!(r_pot, 35_803_684_183_255);
        assert_eq!(delta_t1, 7_160_736_836_651);
        assert_eq!(big_r, 28_642_947_346_604);
    }

    /// Issue #438 static-audit Suspect 2: pending-RUPD + fresh-RUPD double-credit.
    ///
    /// Claim under audit: "At PV9→PV10 cutover, dugite may apply BOTH an
    /// in-flight `pending_reward_update` AND a freshly-computed RUPD to
    /// reward_accounts in the same epoch boundary, double-crediting the
    /// owner credential."
    ///
    /// Result: EXONERATED. `pending_reward_update` is never written to
    /// `Some(_)` anywhere in the current source. It is only ever initialised
    /// to `None` (see all era init sites) and read once at each boundary
    /// (Conway line 342 / Shelley line 182 / state/epoch.rs line 70). On a
    /// preview run starting from any current snapshot the field is `None`,
    /// so `.take()` returns `None` and the legacy branch is a no-op — the
    /// fresh RUPD branch is the SOLE crediting path.
    ///
    /// This test scans the source tree to confirm no writer exists. A future
    /// patch that adds `pending_reward_update = Some(...)` (e.g. re-introducing
    /// the deferred RUPD pattern) must update the Conway/Shelley boundary
    /// code to avoid double-credit, and this test will fail-loud to flag it.
    #[test]
    fn test_issue_438_no_writer_for_pending_reward_update() {
        // Walk every Rust source file in dugite-ledger/src and assert that
        // no file (outside snapshot_format.rs, which clones an Option field)
        // contains a write `pending_reward_update = Some` or
        // `pending_reward_update: Some`.
        fn scan(dir: &std::path::Path, hits: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    scan(&p, hits);
                } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for line in s.lines() {
                        let l = line.trim_start();
                        if l.starts_with("//") || l.starts_with("///") || l.starts_with("*") {
                            continue;
                        }
                        // Skip lines that mention the pattern only inside a
                        // string literal (e.g. this test's own scanner).
                        if line.contains("contains(\"pending_reward_update") {
                            continue;
                        }
                        if line.contains("pending_reward_update = Some")
                            || line.contains("pending_reward_update: Some")
                        {
                            hits.push((p.clone(), line.to_string()));
                        }
                    }
                }
            }
        }
        let crate_src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        scan(&crate_src, &mut hits);
        // Filter snapshot_format.rs (pass-through clone of an Option field
        // for backward-compat snapshot loading) — that is NOT a writer of
        // a fresh Some, it's structural.
        let real: Vec<_> = hits
            .into_iter()
            .filter(|(p, _)| !p.ends_with("snapshot_format.rs"))
            .collect();
        assert!(
            real.is_empty(),
            "issue #438 suspect 2 invariant violated: a writer of \
             pending_reward_update appeared in source. Each hit MUST be \
             paired with audit of Conway/Shelley epoch transition to prevent \
             double-credit:\n{:#?}",
            real
        );
    }

    #[test]
    fn test_sigma_uses_circulation_not_active_stake() {
        let pool_stake: u64 = 4_733_011_000_060;
        let total_active_stake: u64 = 1_177_946_537_741_239;
        let circulation: u64 = 36_709_439_229_911_673;

        // sigma (for maxPool') = pool_stake / circulation ~ 0.000129 < z0 = 0.002
        let sigma = Rat::from_i128(pool_stake as i128, circulation as i128);
        let sigma_f64 = {
            let n: i128 = (&sigma.n).try_into().unwrap_or(i128::MAX);
            let d: i128 = (&sigma.d).try_into().unwrap_or(i128::MAX);
            n as f64 / d as f64
        };
        assert!(
            sigma_f64 < 0.002,
            "sigma relative to circulation should be below z0"
        );

        // sigmaA (for performance only) = pool_stake / total_active_stake ~ 0.004
        let sigma_a = Rat::from_i128(pool_stake as i128, total_active_stake as i128);
        let sigma_a_f64 = {
            let n: i128 = (&sigma_a.n).try_into().unwrap_or(i128::MAX);
            let d: i128 = (&sigma_a.d).try_into().unwrap_or(i128::MAX);
            n as f64 / d as f64
        };
        assert!(
            sigma_a_f64 > 0.002,
            "sigmaA relative to active stake exceeds z0"
        );

        // maxPool with circulation denominator must produce correct (modest) result
        let max_pool = max_pool_prime(
            3,
            10,
            500,
            1_000_000_000_000,
            pool_stake,
            100_000_000_000,
            circulation,
        );
        assert!(
            max_pool < 200_000_000,
            "maxPool with circulation denominator should be ~99M, got {max_pool}"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #898 — `totalActiveStake` must include stake delegated to pools
    // that are no longer registered.
    // -----------------------------------------------------------------------

    fn h28(b: u8) -> Hash28 {
        dugite_primitives::Hash([b; 28])
    }

    fn cred32(b: u8) -> Hash32 {
        h28(b).to_hash32_padded()
    }

    /// Build a `PoolRegistration` whose reward account resolves (via
    /// `reward_account_to_hash`) to `cred32(reward_owner)`.
    fn pool_reg(
        pool: Hash28,
        pledge: u64,
        cost: u64,
        margin: (u64, u64),
        owners: Vec<Hash28>,
        reward_owner: u8,
    ) -> PoolRegistration {
        let mut reward_account = vec![0xe0u8]; // stake-key header, testnet
        reward_account.extend_from_slice(&[reward_owner; 28]);
        PoolRegistration {
            pool_id: pool,
            vrf_keyhash: cred32(0xff),
            pledge: Lovelace(pledge),
            cost: Lovelace(cost),
            margin_numerator: margin.0,
            margin_denominator: margin.1,
            reward_account,
            owners,
            relays: Vec::new(),
            metadata_url: None,
            metadata_hash: None,
        }
    }

    /// Byte-exact replica of the **real** preview epoch-1363 reward calculation
    /// that wedged chain advance in issue #898.
    ///
    /// # Ground truth (all cross-checked against Koios, preview, PV11)
    ///
    /// | Input | Value | Source |
    /// |---|---|---|
    /// | pool | `pool1fw7yf4…` = `4bbc44d7…` | — |
    /// | pool active stake | `1_819_094_673_949` | `pool_history(1363).active_stake` |
    /// | pool blocks | 2 | `pool_history(1363).block_cnt` |
    /// | total blocks | 3559 | `epoch_info(1363).blk_count` |
    /// | **total active stake** | **`3_268_739_510_060_196`** | `epoch_info(1363).active_stake` |
    /// | fixed cost / margin | `340_000_000` / `7/100` | `pool_history(1363)` |
    /// | ρ / τ / a0 / nOpt / d | `3/1000` / `1/5` / `3/10` / 500 / 0 | `epoch_params(1363)` |
    /// | reserves | `7_804_831_720_526_939` | ⇒ `total_stake = 37_195_168_279_473_061` (logged by dugite) |
    /// | `ssFee` | `1_277_034_331` | `epoch_info(1363).fees` |
    ///
    /// Those inputs reproduce dugite's own logged intermediates exactly
    /// (`reward_pot = 15_432_908_345_996`, `max_pool = 580_606_552`), and then
    /// the full Koios payout table to the lovelace:
    /// `pool_fees = 357_239_965`, `member_rewards = 182_748_146`,
    /// `deleg_rewards = 229_045_254`, and for the account that wedged the
    /// chain (`8fab5f50…`, stake `52_270_631_990`) a member reward of
    /// **`6_581_482`** — the exact amount withdrawn on-chain by tx
    /// `9a96f16a…` at slot 117_936_318.
    ///
    /// # Why this test exists
    ///
    /// The wedge in #898 was NOT a reward-formula bug — it was 1_000_000_000
    /// lovelace of missing *stake*. A governance proposal deposit refund was
    /// never credited to its return account (see
    /// `haskell_snapshot::govstate::decode_proposals_with_roots`), so that
    /// account's snapshot stake stayed 1000 ADA below Haskell's, which lowered
    /// `totalActiveStake` from `3_268_739_510_060_196` to
    /// `3_268_738_510_060_196`. Because
    /// `appPerf = beta / sigmaA = (blocks/totalBlocks) × (totalActiveStake /
    /// poolStake)`, a low total scales every pool's pot down: this pool's pot
    /// fell 586_285_225 → 586_285_046 and this member's reward 6_581_482 →
    /// 6_581_478. Under the PV≥10 exact-drain rule the on-chain withdrawal of
    /// 6_581_482 then failed with `WithdrawalAmountMismatch`, permanently
    /// halting chain advance.
    ///
    /// This test pins the reward pipeline itself: given the *correct* inputs it
    /// must reproduce the on-chain payout to the lovelace. That makes it the
    /// discriminator that ruled the formula out as the cause — and a permanent
    /// guard against any future change to `maxPool'`, `mkApparentPerformance`,
    /// `leaderRew` or `memberRew` that would silently shift real payouts.
    #[test]
    fn test_preview_epoch_1363_reward_is_byte_exact_vs_chain() {
        const POOL: u8 = 0x4b; // pool1fw7yf4… (ours, produces blocks)
        const FILLER: u8 = 0x77; // registered pool holding the rest of preview's active stake
        const RETIRED: u8 = 0x99; // stake delegated to a pool absent from pool_params
        const BLOCKS: u8 = 0x55; // block-count filler; no stake, no rewards

        let pool_stake_ours: u64 = 1_819_094_673_949;
        let total_active_stake: u64 = 3_268_739_510_060_196; // Koios epoch_info(1363)
        let retired_stake: u64 = 1_000_000_000; // 1000 ADA delegated to a retired pool
        let filler_stake: u64 = total_active_stake - pool_stake_ours - retired_stake;

        // The 10 real delegators of pool1fw7yf4… at epoch 1363 (Koios
        // `pool_delegators_history`), which sum exactly to the pool's active stake.
        // Index 8 is the pool owner; index 3 is the account that wedged the chain.
        let deleg_stakes: [u64; 10] = [
            1_175_693_626_668,
            1_831_815,
            11_069_661_648,
            52_270_631_990, // 8fab5f50… ← the wedging account
            10_855_654_266,
            11_428_319_109,
            12_480_774_198,
            170_162_540_341,
            367_695_114_063, // owner
            7_436_519_851,
        ];
        assert_eq!(
            deleg_stakes.iter().sum::<u64>(),
            pool_stake_ours,
            "Koios delegator stakes must sum to the pool's active stake"
        );
        const OURS_IDX: usize = 3;
        const OWNER_IDX: usize = 8;
        let cred_of = |i: usize| cred32(0xa0 + i as u8);
        let owner28 = h28(0xa0 + OWNER_IDX as u8);

        // ---- GO snapshot ----------------------------------------------------
        let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
        let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
        for (i, s) in deleg_stakes.iter().enumerate() {
            delegations.insert(cred_of(i), h28(POOL));
            stake_distribution.insert(cred_of(i), Lovelace(*s));
        }
        delegations.insert(cred32(0xd1), h28(FILLER));
        stake_distribution.insert(cred32(0xd1), Lovelace(filler_stake));
        // The credential delegated to the now-retired pool. It is registered and
        // has non-zero stake, so Haskell counts it in `ssTotalActiveStake`.
        delegations.insert(cred32(0xd2), h28(RETIRED));
        stake_distribution.insert(cred32(0xd2), Lovelace(retired_stake));

        let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
        pool_stake.insert(h28(POOL), Lovelace(pool_stake_ours));
        pool_stake.insert(h28(FILLER), Lovelace(filler_stake));
        pool_stake.insert(h28(RETIRED), Lovelace(retired_stake));

        // `pool_params` deliberately omits RETIRED — that is the whole point.
        let mut pool_params: HashMap<Hash28, PoolRegistration> = HashMap::new();
        pool_params.insert(
            h28(POOL),
            pool_reg(
                h28(POOL),
                5_000_000_000,
                340_000_000,
                (7, 100),
                vec![owner28],
                0xa0 + OWNER_IDX as u8,
            ),
        );
        pool_params.insert(
            h28(FILLER),
            pool_reg(h28(FILLER), 0, 340_000_000, (1, 10), vec![], 0xd1),
        );

        let go = StakeSnapshot {
            epoch: EpochNo(1362),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };

        // ---- bprev: 2 blocks for our pool, 3557 elsewhere (total 3559) ------
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(h28(POOL), 2);
        bprev.insert(h28(BLOCKS), 3557); // no stake/params ⇒ contributes only to totals
        assert_eq!(bprev.values().sum::<u64>(), 3559);

        // ---- protocol parameters (Koios epoch_params(1363)) -----------------
        let mut params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        params.rho = rat(3, 1000);
        params.tau = rat(1, 5);
        params.a0 = rat(3, 10);
        params.n_opt = 500;
        params.active_slots_coeff = 0.05;

        let reserves = Lovelace(7_804_831_720_526_939);
        let ss_fee = Lovelace(1_277_034_331);

        let rupd = super::compute_reward_update(
            &params,
            &rat(0, 1), // prev_d — Conway: d = 0
            11,         // prev_protocol_version_major — van Rossem (PV11)
            Some(&go),
            &bprev,
            ss_fee,
            reserves,
            Lovelace(0),
            &HashMap::new(),
            None,
            86_400, // preview epoch length
            0,
            super::super::MAX_LOVELACE_SUPPLY,
        );

        // The account whose on-chain withdrawal wedged the chain.
        let ours = rupd
            .rewards
            .get(&cred_of(OURS_IDX))
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            ours.0,
            6_581_482,
            "member reward for 8fab5f50… must equal the amount withdrawn on-chain \
             by tx 9a96f16a… at slot 117936318. Got {} (off by {}). Any drift here \
             means the reward pipeline (maxPool' / mkApparentPerformance / \
             memberRew) no longer reproduces real preview payouts.",
            ours.0,
            6_581_482i64 - ours.0 as i64,
        );

        // Whole-pool payout table, byte-exact vs Koios `pool_history(1363)`.
        let member_total: u64 = (0..10)
            .filter(|i| *i != OWNER_IDX)
            .map(|i| {
                rupd.rewards
                    .get(&cred_of(i))
                    .copied()
                    .unwrap_or(Lovelace(0))
                    .0
            })
            .sum();
        assert_eq!(
            member_total, 182_748_146,
            "sum of non-owner member rewards must equal Koios `member_rewards`"
        );

        // dugite folds leaderRew + memberRew(owner) into one operator credit;
        // Koios reports them separately as `pool_fees` + (`deleg_rewards` −
        // `member_rewards`) = 357_239_965 + 46_297_108.
        let operator = rupd
            .rewards
            .get(&cred_of(OWNER_IDX))
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            operator.0,
            357_239_965 + 46_297_108,
            "operator credit must equal Koios pool_fees + owner-as-member"
        );
    }

    /// `totalActiveStake` must mirror Haskell `sumAllActiveStake` — the sum of
    /// the GO snapshot's ENTIRE per-credential active-stake map, not just the
    /// pools still present in `pool_params`:
    ///
    /// ```haskell
    /// -- Cardano.Ledger.State.SnapShots
    /// mkSnapShot ssActiveStake ssStakePoolsSnapShot =
    ///   let ssTotalActiveStake = sumAllActiveStake ssActiveStake
    ///    in SnapShot {ssActiveStake, ssTotalActiveStake, ssStakePoolsSnapShot}
    ///
    /// -- Cardano.Ledger.State.Stake
    /// -- | Active stake: maps staking credentials to their non-zero stake paired
    /// -- with delegation. Only credentials that are registered, delegated, and
    /// -- have non-zero stake appear here.
    /// sumAllActiveStake (ActiveStake m) =
    ///   VMap.foldMap (fromCompact . unNonZero . swdStake) m `nonZeroOr` knownNonZeroCoin @1
    /// ```
    ///
    /// Membership needs *registered + delegated + non-zero*; it does NOT need
    /// the delegated-to pool to still be registered. `ssStakePoolsSnapShot`
    /// (the per-pool aggregates) is rebuilt from `psStakePools` alone, so the
    /// two quantities are only equal because SNAP runs before POOLREAP.
    ///
    /// dugite previously summed `pool_stake` filtered to registered pools,
    /// which relied on that ordering invariant instead of the definition. This
    /// test pins the definition directly: the ONLY difference between the two
    /// runs is whether an unrelated pool is still in `pool_params`, and that
    /// must not move this pool's member reward by a single lovelace. Without
    /// the fix the reward changes by ~4× here.
    #[test]
    fn test_total_active_stake_matches_haskell_sum_all_active_stake() {
        const POOL: u8 = 0x11;
        const OTHER: u8 = 0x22;
        let member = cred32(0x33);
        let other_deleg = cred32(0x44);

        let build = |other_registered: bool| {
            let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
            delegations.insert(member, h28(POOL));
            delegations.insert(other_deleg, h28(OTHER));
            let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
            stake_distribution.insert(member, Lovelace(500_000_000_000));
            stake_distribution.insert(other_deleg, Lovelace(1_500_000_000_000));
            let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
            pool_stake.insert(h28(POOL), Lovelace(500_000_000_000));
            pool_stake.insert(h28(OTHER), Lovelace(1_500_000_000_000));

            let mut pool_params: HashMap<Hash28, PoolRegistration> = HashMap::new();
            pool_params.insert(
                h28(POOL),
                pool_reg(h28(POOL), 0, 1_000_000, (1, 10), vec![], 0x77),
            );
            if other_registered {
                pool_params.insert(
                    h28(OTHER),
                    pool_reg(h28(OTHER), 0, 1_000_000, (1, 10), vec![], 0x88),
                );
            }

            let go = StakeSnapshot {
                epoch: EpochNo(7),
                delegations: Arc::new(delegations),
                pool_stake,
                pool_params: Arc::new(pool_params),
                stake_distribution: Arc::new(stake_distribution),
                epoch_fees: Lovelace(0),
                epoch_block_count: 0,
                epoch_blocks_by_pool: Arc::new(HashMap::new()),
            };

            let mut bprev: HashMap<Hash28, u64> = HashMap::new();
            bprev.insert(h28(POOL), 100);

            let mut params =
                dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
            params.rho = rat(3, 1000);
            params.tau = rat(1, 5);
            params.a0 = rat(3, 10);
            params.n_opt = 500;
            params.active_slots_coeff = 0.05;

            super::compute_reward_update(
                &params,
                &rat(0, 1),
                11,
                Some(&go),
                &bprev,
                Lovelace(0),
                Lovelace(30_000_000_000_000_000),
                Lovelace(0),
                &HashMap::new(),
                None,
                86_400,
                0,
                super::super::MAX_LOVELACE_SUPPLY,
            )
            .rewards
            .get(&member)
            .copied()
            .unwrap_or(Lovelace(0))
            .0
        };

        let with_registered = build(true);
        let with_retired = build(false);
        assert!(with_registered > 0, "sanity: member must earn a reward");
        assert_eq!(
            with_registered, with_retired,
            "#898: retiring an unrelated pool must not change this pool's member \
             reward — its delegated stake stays in Haskell's `sumAllActiveStake`"
        );
    }
}
