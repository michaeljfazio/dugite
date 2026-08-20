use super::non_myopic::{leader_probability, Likelihood, NonMyopic};
use super::reward_pulser::{MemberFoldCtx, PoolRewardInfo, RewardEntry, RewardFold};
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

/// Build this epoch's `newLikelihoods` — one `Likelihood` per pool in the GO
/// snapshot's pool set.
///
/// ```haskell
/// -- startStep, Cardano.Ledger.Shelley.LedgerState.PulsingReward
/// let SnapShot activeStake totalActiveStake stakePoolSnapShots = ssStakeGo ss
///     mkPoolRewardInfoCurry =
///       mkPoolRewardInfo pr _R b (fromIntegral blocksMade) totalStake totalActiveStake
///     allPoolInfo = VMap.mapWithKey mkPoolRewardInfoCurry stakePoolSnapShots
///
/// makeLikelihoods = \case
///   Left (StakeShare sigma) ->
///     likelihood 0 (leaderProbability asc sigma $ pr ^. ppDG) slotsPerEpoch
///   Right info ->
///     likelihood (poolBlocks info) (leaderProbability asc (getSigma info) $ pr ^. ppDG) slotsPerEpoch
/// newLikelihoods = VMap.map makeLikelihoods allPoolInfo
/// ```
///
/// # This is NOT the reward-distribution loop's pool set
///
/// The key set is **every** pool in `stakePoolSnapShots`, unconditionally — no
/// filter on stake and no filter on blocks produced. The reward loop below
/// deliberately `continue`s past pools that made zero blocks and past pools
/// whose pledge is unmet; both of those still get a likelihood entry here.
/// Piggy-backing this onto that loop would silently drop exactly the pools the
/// `Left` branch exists to serve.
///
/// The `Left`/`Right` split needs no branch in Rust: it is decided by
/// `Map.lookup stakePoolId (unBlocksMade blocks)`, and `BlocksMade` is
/// increment-only, so "absent" ⟺ "zero blocks". Reading the count with
/// `unwrap_or(0)` reproduces both arms — `Left` passes a literal `0`, `Right`
/// passes `poolBlocks info`, and both compute `t` from the same `sigma`.
fn build_new_likelihoods(
    go_snapshot: Option<&StakeSnapshot>,
    bprev_blocks_by_pool: &HashMap<Hash28, u64>,
    active_slot_coeff: (u64, u64),
    prev_d: &dugite_primitives::transaction::Rational,
    total_stake: u64,
    epoch_length: u64,
) -> HashMap<Hash28, Likelihood> {
    let go = match go_snapshot {
        Some(g) => g,
        None => return HashMap::new(),
    };

    let d = (prev_d.numerator, prev_d.denominator.max(1));

    go.pool_params
        .keys()
        .map(|pool_id| {
            let pool_stake = go.pool_stake.get(pool_id).map(|l| l.0).unwrap_or(0);
            let blocks = bprev_blocks_by_pool.get(pool_id).copied().unwrap_or(0);

            // `sigma = poolTotalStake %? totalStake` — the UNCAPPED relative
            // stake over circulating supply. NOT `min sigma z0` (that capping
            // belongs to `maxPool'`), and NOT over `totalActiveStake` (that is
            // `sigmaA`, used only by `mkApparentPerformance`).
            let t = leader_probability(active_slot_coeff, (pool_stake, total_stake), d);

            (*pool_id, Likelihood::new(blocks, t, epoch_length))
        })
        .collect()
}

/// One pulse of the RUPD member fold — Haskell `pulseStep`, driven per block.
///
/// ```haskell
/// -- Rupd.hs, the JustRight arm
/// SJust p@(Pulsing _ _) -> SJust <$> pulseStep p
///
/// -- PulsingReward.hs — pulseStep's clause order, verbatim:
/// pulseStep (Complete r_) = pure (Complete r_, mempty)
/// pulseStep p@(Pulsing _ pulser) | done pulser = completeStep p   -- checked BEFORE pulsing
/// pulseStep (Pulsing rewsnap pulser) = do
///   p2 <- pulseM pulser
///   pure (Pulsing rewsnap p2, event)                              -- stays Pulsing even if THIS pulse drains it
/// ```
///
/// Two timing facts fall directly out of that clause order, both oracle-
/// verified against real source (not inferred): the tick that CREATES the
/// pulser (`SNothing -> SJust (Pulsing ...)`, in `apply.rs`) performs ZERO
/// pulses — `startStep` is a pure constructor, and `pulseStep`'s advancing
/// clause only matches when `ru` is ALREADY `SJust (Pulsing ...)` on entry,
/// which is only true starting the NEXT tick. And completion has a ONE-TICK
/// LAG: `done` is checked BEFORE pulsing, so a pulse that exhausts the queue
/// still returns wrapped in `Pulsing` for THAT tick; only the FOLLOWING
/// tick's `done` check (with no new pulse) promotes to `Complete`. This
/// function's structure below mirrors both: the "just built the fold" branch
/// returns without pulsing, and the `is_complete()` check happens BEFORE any
/// call to `fold.pulse(...)`, never after.
///
/// Runs only inside the pulsing window: `rupd_monetary` is `Some` exactly when
/// the epoch has passed its `4k/f` mark, so this is a no-op before the freeze
/// and idempotent once the balance is exhausted.
///
/// Every input is read from the freeze, never from live state — the per-pool
/// table is built once from `rupd_monetary`'s `r` and `total_stake`, and the
/// GO snapshot it folds over is the PRE-rotation one that the boundary will
/// also use (`compute_reward_update` runs before SNAP). So a pulse taken at
/// block N and a pulse taken at block N+1 see identical inputs, which is what
/// makes the differential property meaningful in production and not just in
/// the proptest.
///
/// Also promotes `epochs.rupd_snapshot` from `Pulsing` to `Complete` — ONE
/// TICK after the fold internally finishes, never on the same tick, per the
/// `pulseStep` clause order quoted above — and, on the tick the fold is
/// BUILT (never the same tick as a pulse), fills in the frozen snapshot's
/// `likelihoods`/`leaders` (`rewLikelihoods`/`rewLeaders`), which cannot be
/// known until the per-pool table exists. Both are wire-only (#1071):
/// `rupd_snapshot` is a read-only mirror of `rupd_monetary`'s gate (see
/// `EpochSubState::rupd_snapshot`'s doc for why it is a separate field), and
/// neither feeds `compute_reward_update`, which still derives its own answer
/// from the GO snapshot at the boundary — this function's timing affects only
/// what `nesRu` reports mid-epoch, never a credited amount.
pub(crate) fn pulse_rupd_member_fold(
    epochs: &mut super::substates::EpochSubState,
    prev_d: &dugite_primitives::transaction::Rational,
    prev_protocol_version_major: u64,
    security_param_k: u64,
    epoch_length: u64,
) {
    let Some(monetary) = epochs.rupd_monetary else {
        return; // before the mark — nothing frozen, nothing to pulse
    };
    let Some(go) = epochs.snapshots.go.clone() else {
        return; // no GO snapshot yet (first two epochs)
    };
    if epochs.rupd_fold.is_complete() {
        // `completeStep`'s Pulsing -> Complete transition, for the WIRE arm
        // (#1071) ONLY — `epochs.rupd_fold`'s own `is_done()` already turned
        // true the instant the LAST pulse drained the queue, on a PREVIOUS
        // call to this function; that internal bookkeeping is immediate and
        // deliberately untouched by this fix (`compute_reward_update`'s
        // `prepulsed` consumer needs it to be). What was wrong is *this* wire
        // promotion happening on the SAME tick as that draining pulse — see
        // the `pulseStep` clause order quoted above the doc comment. `.take()`
        // + `.complete()` is idempotent (`PulsingRewUpdate::complete` is a
        // no-op on an already-`Complete` value), so repeating it on every
        // subsequent tick is harmless; it only actually flips state once.
        if let Some(snap_state) = epochs.rupd_snapshot.take() {
            epochs.rupd_snapshot = Some(snap_state.complete());
        }
        return; // `completeStep` on an already-complete fold does no fold work
    }
    // At pv<=6, `rupd_addrs_rew == None` does NOT mean "no prefilter" — it means
    // the frozen `fvAddrsRew` has not been captured yet, and folding under it
    // pays member rewards `rewardOnePoolMember` never creates (Rewards.hs:315).
    // That was #1074. Unreachable now that the capture is ordered before this
    // call (`apply.rs`), but kept because the failure mode is silent phantom
    // rewards on a consensus path AND invisible everywhere except a pv<=6
    // mainnet replay: `hardforkBabbageForgoRewardPrefilter` drops the prefilter
    // at pv>=7, so permissive IS correct on devnet, preview and preprod.
    //
    // Declining the pulse is safe where guessing is not: the fold is driven
    // per block and idempotent, so the work simply happens on the next one.
    debug_assert!(
        !(prev_protocol_version_major <= 6 && epochs.rupd_addrs_rew.is_none()),
        "pv<=6 member fold pulsed before fvAddrsRew was captured"
    );
    if prev_protocol_version_major <= 6 && epochs.rupd_addrs_rew.is_none() {
        return;
    }

    let pp = epochs.prev_protocol_params.clone();
    let bprev = epochs.snapshots.bprev_blocks_by_pool.clone();
    let addrs = epochs.rupd_addrs_rew.clone();
    let registered = move |c: &Hash32| -> bool { addrs.as_ref().is_none_or(|set| set.contains(c)) };

    let just_built_table = epochs.rupd_fold.fold.is_none();
    if just_built_table {
        // Build the frozen per-pool table ONCE, from the frozen terms.
        let (d_num, d_den) = (prev_d.numerator as i128, prev_d.denominator.max(1) as i128);
        let total_active_stake: u64 = go
            .pool_stake
            .values()
            .fold(0u64, |acc, s| acc.saturating_add(s.0));
        let total_blocks_in_epoch: u64 = bprev.values().sum::<u64>().max(1);
        let table = build_pool_reward_table(
            &go,
            &bprev,
            &pp,
            pp.n_opt.max(1),
            monetary.r,
            monetary.total_stake,
            total_active_stake,
            total_blocks_in_epoch,
            5 * d_num >= 4 * d_den,
            d_num,
            d_den,
            prev_protocol_version_major,
            &registered,
        );

        // #1071: `rewLikelihoods`/`rewLeaders` — the two `RewardSnapShot`
        // fields the freeze itself could not populate (`apply.rs`'s capture
        // runs before the per-pool table exists). Computed HERE from the SAME
        // table built above, rather than a third copy of
        // `build_pool_reward_table` (`compute_reward_update` at the boundary
        // is the second).
        let likelihoods = build_new_likelihoods(
            Some(&go),
            &bprev,
            pp.active_slot_coeff_rational(),
            prev_d,
            monetary.total_stake,
            epoch_length,
        );
        let mut leaders: HashMap<Hash32, Vec<RewardEntry>> = HashMap::new();
        for info in table.values() {
            if let Some((op_key, amount)) = info.leader {
                leaders.entry(op_key).or_default().push(RewardEntry {
                    is_member: false,
                    pool_id: info.pool_id,
                    amount,
                });
            }
        }
        // A credential earning leader rewards from more than one pool is rare
        // but possible, and `table.values()` iterates a `HashMap` — sort each
        // credential's entries so the persisted bytes do not depend on
        // iteration order (the #1088 shape, one level up).
        for entries in leaders.values_mut() {
            entries.sort_unstable_by_key(|e| e.pool_id);
        }

        epochs.rupd_fold.table = table;
        epochs.rupd_fold.fold = Some(RewardFold::new(&go.delegations));
        if let Some(snap_state) = epochs.rupd_snapshot.as_mut() {
            let snap = snap_state.snapshot_mut();
            snap.likelihoods = likelihoods;
            snap.leaders = leaders;
        }
        // Haskell's `startStep` (the `SNothing -> SJust` arm in `Rupd.hs`) is
        // a PURE CONSTRUCTOR: it builds the unadvanced pulser and returns
        // immediately. `pulseStep`'s fold-advancing clause only matches when
        // `ru` is ALREADY `SJust (Pulsing ...)` on entry to a tick — true
        // starting the NEXT tick, never this one. Falling through to the
        // pulse below on the SAME call that just built the fold would
        // perform a pulse Haskell's creation tick never performs.
        return;
    }

    let fold_state = &mut epochs.rupd_fold;
    let Some(fold) = fold_state.fold.as_mut() else {
        return;
    };
    let n = RewardFold::pulse_size(go.delegations.len(), security_param_k);
    let ctx = MemberFoldCtx {
        table: &fold_state.table,
        delegations: &go.delegations,
        stake: &go.stake_distribution,
        pv_major: prev_protocol_version_major,
        registered: &registered,
    };
    fold.pulse(n, &ctx);
    // Deliberately NO `is_done()` check/promotion here. Haskell's `pulseStep`
    // wraps the post-pulse state in `Pulsing` UNCONDITIONALLY, even when this
    // exact pulse exhausts the queue — the wire only becomes `Complete`
    // starting the tick AFTER this one, via the `is_complete()` check at the
    // top of this function. Promoting here would be the one-tick-early bug
    // this function exists to avoid.
}

/// Haskell `mkPoolRewardInfo` over every pool in the GO snapshot.
///
/// Split out of `compute_reward_update` so it can run at the 4k/f MARK
/// rather than only at the boundary. Per-block pulsing needs the per-pool
/// terms to exist before the first pulse — the credential fold reads them —
/// and they are all derived from inputs `startStep` has already frozen, so
/// building them early is a relocation, not a semantic change.
///
/// Returns the table keyed by pool. Pools that minted no block, or whose
/// pledge is unmet, or whose reward rounds to zero, are ABSENT — the fold
/// then skips their delegators by lookup miss, exactly as the pool-major
/// loop skipped them by `continue`.
#[allow(clippy::too_many_arguments)]
fn build_pool_reward_table(
    go: &StakeSnapshot,
    bprev_blocks_by_pool: &HashMap<Hash28, u64>,
    pp: &ProtocolParameters,
    n_opt: u64,
    reward_pot: u64,
    total_stake: u64,
    total_active_stake: u64,
    total_blocks_in_epoch: u64,
    d_ge_4_5: bool,
    d_num: i128,
    d_den: i128,
    prev_protocol_version_major: u64,
    registered_at_startstep: &dyn Fn(&Hash32) -> bool,
) -> HashMap<Hash28, PoolRewardInfo> {
    let mut pool_reward_table: HashMap<Hash28, PoolRewardInfo> =
        HashMap::with_capacity(go.pool_stake.len());
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

        // Pre-Babbage (pv<=6) leader-reward prefilter (Haskell `collectLRs`,
        // Cardano/Ledger/Shelley/Rewards.hs): include the leader reward iff
        // `hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered op`.
        // For pv>=7 (Babbage+, errata 17.2) the check is bypassed. A dropped
        // leader reward is never credited → stays in the pot → undistributed →
        // returned to reserves (matches Haskell deltaR2). Member rewards are
        // gated independently by the per-member prefilter in the fold below.
        //
        // Deliberately NOT gated on `operator_reward > 0`: Haskell's
        // `collectLRs` inserts the `Reward RewardLeader` Set element whenever
        // the prefilter passes, with no amount check — a real cardano-node
        // 11.0.1 capture carries a `[1, pool, 0]` leader entry
        // (`tests/fixtures/nesru/{pulsing,complete-nonzero}.hex`, #1071
        // follow-up). `compute_reward_update`'s aggregation step is what
        // decides whether a zero-amount entry here is allowed to affect the
        // credited total — see the comment at its leader-rewards loop.
        let op_key = LedgerState::reward_account_to_hash(&pool_reg.reward_account);
        let included = prev_protocol_version_major >= 7 || registered_at_startstep(&op_key);
        let leader = included.then_some((op_key, operator_reward));

        pool_reward_table.insert(
            *pool_id,
            PoolRewardInfo {
                pool_id: *pool_id,
                pool_active_stake: pool_active_stake.0,
                pool_reward,
                cost,
                margin_num,
                margin_den,
                owner_set,
                leader,
            },
        );
    }
    pool_reward_table
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
    prev_non_myopic: &NonMyopic,
    frozen_monetary: Option<crate::state::reward_pulser::MonetaryStep>,
    // The member fold as the per-block pulses left it, if any. `None` falls
    // back to folding everything here, which is what a node that restarted
    // mid-epoch does — and by the differential property it reaches the same
    // answer, just without the work having been spread out.
    prepulsed: Option<RewardFold>,
) -> PendingRewardUpdate {
    // `totalStake` = `circulation es maxSupply = maxSupply <-> casReserves acnt`,
    // the current circulating supply. Hoisted above every early return because
    // it is `sigma`'s denominator and so is needed to build `newLikelihoods`,
    // which Haskell produces unconditionally in `startStep`.
    //
    // Read from the FREEZE when one exists — Haskell's `fvTotalStake`, captured
    // at the 4k/f mark rather than recomputed here. The two agree wherever
    // reserves are immobile mid-epoch, which is everywhere except a boundary
    // whose era translation moves them: Shelley→Allegra, where
    // `returnRedeemAddrsToReserves` credits the unredeemed AVVM coin before the
    // reward update is applied.
    //
    // dugite patched that single boundary with `pending_avvm_return`, subtracted
    // back off the reserves passed in here. Reading the frozen value removes
    // both the patch and the unstated invariant it encoded. It also has to be
    // this field and not just `deltaR1`: `total_stake` is `sigma`'s denominator,
    // so freezing only the monetary terms would have left the reward
    // DISTRIBUTION reading post-AVVM reserves while the pot read pre-AVVM —
    // strictly worse than the patch.
    let total_stake = match frozen_monetary {
        Some(m) => m.total_stake,
        None => max_lovelace_supply.saturating_sub(reserves.0),
    };

    // Haskell has no early return here: `startStep` always computes
    // `newLikelihoods`, and `completeRupd` always folds it through
    // `updateNonMyopic`. Every `return` below therefore has to carry a real
    // `NonMyopic`, not a default one — an empty `likelihoodsNM` is only correct
    // when the GO snapshot genuinely has no pools.
    let new_likelihoods = build_new_likelihoods(
        go_snapshot,
        bprev_blocks_by_pool,
        params.active_slot_coeff_rational(),
        prev_d,
        total_stake,
        epoch_length,
    );
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
        // Shared with the pulser's freeze and the dump's reported value: this
        // was the third hand-written copy of the formula.
        let raw_expected_blocks = crate::state::reward_pulser::expected_blocks_raw(
            (prev_d.numerator, prev_d.denominator),
            (f_num, f_den),
            epoch_length,
        );
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

    // Phase 1a: prefer the step FROZEN at 4k/f. Haskell computes deltaR1 /
    // deltaT1 / _R inside `startStep`, mid-epoch; recomputing them here reads
    // the same numbers only because reserves happen not to move mid-epoch.
    // Consuming the frozen value makes that structural instead of accidental.
    let (expansion, frozen_delta_t1, frozen_r) = match frozen_monetary {
        Some(m) => {
            debug_assert_eq!(
                m.delta_r1, expansion,
                "frozen deltaR1 must equal the boundary-recomputed value; a \
                 mismatch means an input moved between 4k/f and the boundary, \
                 which is exactly the invariant this freeze exists to enforce"
            );
            (m.delta_r1, Some(m.delta_t1), Some(m.r))
        }
        None => (expansion, None, None),
    };

    let total_rewards_available = expansion + epoch_fees;

    if total_rewards_available == 0 {
        // `rPot = ssFee <> deltaR1 = 0`, so `deltaT1 = floor(tau * 0) = 0` and
        // `_R = rPot - deltaT1 = 0`. The deltas are all zero, but the
        // likelihoods are not: upstream still ranks pools in an epoch that
        // minted nothing.
        return PendingRewardUpdate {
            non_myopic: prev_non_myopic.update(Lovelace(0), new_likelihoods),
            ..Default::default()
        };
    }

    let treasury_cut = frozen_delta_t1.unwrap_or_else(|| {
        Rat::from_i128(tau_num, tau_den)
            .mul(&Rat::from_i128(total_rewards_available as i128, 1))
            .floor_u64()
    });

    let reward_pot = frozen_r.unwrap_or(total_rewards_available - treasury_cut);

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
            non_myopic: prev_non_myopic.update(Lovelace(reward_pot), new_likelihoods),
            ..Default::default()
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
                // `new_likelihoods` is empty on this path by construction
                // (`build_new_likelihoods` returns empty for `None`), so this
                // drops any prior history — which is what `mapWithKey` over an
                // empty `newLikelihoods` does upstream.
                non_myopic: prev_non_myopic.update(Lovelace(reward_pot), new_likelihoods),
                ..Default::default()
            };
        }
    };

    // `totalActiveStake` is the sum of the GO snapshot's ENTIRE
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
    // NOT require the delegated-to pool to still exist OR to have ever
    // existed — a `StakeDelegation`/`VoteDelegation` certificate is never
    // required to name a currently-registered pool, so a credential can stay
    // "active" against a `pool_id` that has retired, or one that was never
    // registered at all.
    //
    // A prior version of this comment additionally claimed POOLREAP "leaves
    // [a retiring pool's] delegators' delegations dangling" as the mechanism.
    // That is WRONG and was oracle-refuted against cardano-node 11.0.1's
    // actual pinned cardano-ledger source (2026-08-05): POOLREAP clears a
    // retiring pool's delegators' `stakePoolDelegationAccountStateL` in the
    // SAME transition that removes the pool
    // (`Cardano.Ledger.State.Account.removeStakePoolDelegations`,
    // `Cardano.Ledger.Shelley.Rules.PoolReap.poolReapTransition`), which is
    // exactly what `certs.delegations.retain(|_, p| p != pool_id)` does
    // below in `state/epoch.rs`/`eras/conway.rs` at retirement. Since SNAP
    // always runs BEFORE POOLREAP within one boundary, a given snapshot's
    // `pool_stake` and `pool_params` are captured together, in sync, and
    // stay mutually consistent for that snapshot's whole lifetime — the
    // "orphan" case this function guards against is a delegation whose
    // target pool_id was NEVER registered (always legal, per the paragraph
    // above), not a delegation surviving its target pool's retirement.
    //
    // Filtering by `pool_params` understates `totalActiveStake` relative to
    // the Haskell definition either way. Since `appPerf = beta / sigmaA =
    // beta * totalActiveStake / poolStake`, a low total scales every pool's
    // `poolPot` — and therefore every leader and member reward — down
    // proportionally, so getting this exactly right matters even though (per
    // commit 5c9d833b52, the fix that removed the `pool_params` filter) it
    // is "a defensive alignment [with the Haskell definition], not a
    // behaviour change on any reachable state" under dugite's own
    // active-purge POOLREAP — unfiltered and pool_params-filtered sums
    // coincide today. That commit's own byte-exact preview-epoch-1363
    // regression test (Koios-cross-checked pool_fees/member_rewards/
    // deleg_rewards) is what ruled this OUT as a cause of issue #898 (which
    // was a separate Mithril-import governance-roots bug); do not re-attach
    // #898 to this code path.
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
            // Non-empty whenever the GO snapshot has pools: zero ACTIVE stake
            // does not mean zero pools, and every pool in `stakePoolSnapShots`
            // still gets a `likelihood 0 …` entry (all-zero after
            // normalisation, since sigma = 0 makes t = 0).
            non_myopic: prev_non_myopic.update(Lovelace(reward_pot), new_likelihoods),
            ..Default::default()
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

    // Haskell `PoolRewardInfo`, keyed by pool: every per-pool term the
    // credential fold reads. Built pool-major, consumed credential-major.

    let pool_reward_table = build_pool_reward_table(
        go,
        bprev_blocks_by_pool,
        pp,
        n_opt,
        reward_pot,
        total_stake,
        total_active_stake,
        total_blocks_in_epoch,
        d_ge_4_5,
        d_num,
        d_den,
        prev_protocol_version_major,
        &registered_at_startstep,
    );

    // ---- the CREDENTIAL-MAJOR member fold ---------------------------------
    //
    // Haskell folds `rewardOnePoolMember` over `Credential 'Staking`, which is
    // why the captured pulser's work queue is a set of credentials rather than
    // of pools (`tests/fixtures/nesru/pulsing.hex`). dugite folded pool-major
    // with an inner delegator loop; that produces identical output — the
    // aggregation below sorts, so entry order is unobservable — but it cannot
    // be chunked to match upstream's pulse, because a single pool can hold
    // hundreds of thousands of delegators.
    //
    // Every per-pool term is now read from the frozen `PoolRewardInfo` table
    // through `&self`, so the classic incremental-fold hazard — a per-pool
    // quantity silently recomputed per credential against state that has moved
    // on — is not expressible here.
    // The pv<=6 member prefilter (Haskell `rewardOnePoolMember.prefilter`,
    // eras/shelley/impl/.../Rewards.hs:262):
    //   prefilter = hardforkBabbageForgoRewardPrefilter pv || hk ∈ addrsRew
    // For pv ≤ 6 (Shelley-Alonzo) the member credential must be registered in
    // the reward-accounts set frozen at startStep, or the computed reward is
    // dropped. For pv ≥ 7 (Babbage onward, ledger errata 17.2) the prefilter is
    // bypassed; unregistered rewards are routed at applyRUpd time
    // (frTotalUnregistered → treasury). Applied inside `RewardFold::pulse`.
    let ctx = MemberFoldCtx {
        table: &pool_reward_table,
        delegations: &go.delegations,
        stake: &go.stake_distribution,
        pv_major: prev_protocol_version_major,
        registered: &registered_at_startstep,
    };

    // Production runs the fold to completion in one call. That is deliberate:
    // this is the SAME code path an incrementally-pulsed fold takes, just with
    // a single maximal pulse, so there is no second implementation to drift.
    // #985/#932/#938 were all N-copies defects where the copy nobody edited was
    // the live one; a batch path kept beside a pulse path would be the same
    // trap with a consensus-critical fold inside it.
    // Resume the pulser if blocks already advanced it; otherwise start fresh.
    // `complete` folds whatever remains, so a fold that is already done costs
    // nothing here and one that never started is folded in full — the two ends
    // of the same code path, which is why `fold_incremental == fold_batch` is
    // the only property this needs to be correct.
    let mut fold = prepulsed.unwrap_or_else(|| RewardFold::new(&go.delegations));
    fold.complete(&ctx);
    for (cred, entries) in fold.into_entries() {
        reward_entries.entry(cred).or_default().extend(entries);
    }

    // `raw_rewards` (the WIRE `rs` field, #1071) and `reward_entries` (which
    // feeds the CREDITED-amount aggregation below) diverge starting here, and
    // deliberately: a real cardano-node 11.0.1 capture
    // (`tests/fixtures/nesru/{pulsing,complete-nonzero}.hex`) shows Haskell's
    // `Set Reward` carries a LeaderReward entry with amount **0** whenever the
    // pv<=6 registration prefilter passes, regardless of the computed amount —
    // `collectLRs` never filters on amount, only on registration. Cloned
    // BEFORE the leader-rewards loop below adds anything, so this starts as
    // exactly the member-fold entries both collections share.
    let mut raw_reward_entries = reward_entries.clone();

    // ---- leader rewards ---------------------------------------------------
    //
    // Separate from the member fold, exactly as upstream keeps `collectLRs`
    // separate from `rewardOnePoolMember`: a leader reward is a property of the
    // pool, not of a delegating credential, and an operator who also delegates
    // must not be paid twice.
    for info in pool_reward_table.values() {
        if let Some((op_key, amount)) = info.leader {
            // Wire-only (`rs`): unconditional, matching Haskell's `Set Reward`
            // exactly — the entry exists whenever the prefilter passes, zero
            // amount included.
            raw_reward_entries
                .entry(op_key)
                .or_default()
                .push((false, info.pool_id, amount));
            // Aggregation (`reward_map`/`total_distributed` below, including
            // the pv<=2 `Set.deleteFindMin` selection a few lines down): a
            // zero-amount leader entry must NOT enter it. For pv>2 this would
            // be a no-op (summing a 0 changes nothing), but at pv<=2 the
            // selection picks the LOWEST `(is_member, pool_id)` entry
            // regardless of amount, so an always-present zero leader entry
            // would win that selection over a nonzero member entry for the
            // same credential — a real behaviour change to an already-
            // validated consensus path (mainnet epochs 208-236 run pv<=2) that
            // is explicitly OUT OF SCOPE for this wire-shape fix. Excluding it
            // here reproduces today's pre-#1071-followup aggregation exactly.
            if amount > 0 {
                reward_entries
                    .entry(op_key)
                    .or_default()
                    .push((false, info.pool_id, amount));
            }
        }
    }

    // #1071: `rs` — the UNAGGREGATED per-source entries, matching Haskell's
    // wire `RewardUpdate.rs :: Map (Credential Staking) (Set Reward)`. Built
    // from `raw_reward_entries`, NOT `reward_entries` — see above for why the
    // two differ (zero-amount leader entries). This is the one and only place
    // either form is derived — the aggregation logic itself is not duplicated
    // (the #932/#938/#985 N-copies trap).
    let raw_rewards: HashMap<Hash32, Vec<RewardEntry>> = raw_reward_entries
        .iter()
        .map(|(cred, entries)| {
            (
                *cred,
                entries
                    .iter()
                    .map(|(is_member, pool_id, amount)| RewardEntry {
                        is_member: *is_member,
                        pool_id: *pool_id,
                        amount: *amount,
                    })
                    .collect(),
            )
        })
        .collect();

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
        // `nonMyopic = updateNonMyopic nm oldr newLikelihoods`, where `oldr` is
        // `rewR` = `_R` = `rPot - deltaT1` — the pot AFTER the treasury cut,
        // which is exactly `reward_pot` here. Not `total_rewards_available`,
        // and not `expansion`.
        non_myopic: prev_non_myopic.update(Lovelace(reward_pot), new_likelihoods),
        raw_rewards,
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
            &self.epochs.non_myopic,
            self.epochs.rupd_monetary,
            // This helper computes a reward update on demand rather than at a
            // boundary, so there is no pulse history to resume from — it folds
            // in full, which the differential property makes equivalent.
            None,
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

/// The monetary decomposition of a reward update, in the shape
/// `cardano-streamer` reports as `rupdNext`.
///
/// dugite's own [`PendingRewardUpdate`](super::PendingRewardUpdate) carries
/// only the NET terms it needs to apply (`delta_treasury`, a signed
/// `delta_reserves`). The cross-validation dataset wants the decomposition
/// each of those nets out of, so the stages can be compared individually —
/// a matching net with two compensating errors inside it is exactly what a
/// single number cannot distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForcedRewardUpdate {
    /// `deltaR1` — monetary expansion drawn from reserves, `rho * reserves`
    /// scaled by `eta`.
    pub delta_r1: u64,
    /// `deltaR2` — the undistributed remainder returned to reserves,
    /// `rewardPot - totalDistributed`.
    pub delta_r2: u64,
    /// `deltaT1` — the treasury cut, `floor(tau * rPot)`.
    pub delta_t1: u64,
    /// `rPot = epochFees + deltaR1`, the pot before the treasury cut.
    pub r_pot: u64,
    /// `rewardPot = rPot - deltaT1`, what is available to distribute.
    pub reward_pot: u64,
    /// Total actually distributed to stake credentials.
    pub total_distributed: u64,
    //
    // There is deliberately NO `expected_blocks` here. It used to be carried
    // through from `MonetaryStep`, whose copy is post-processed for the division
    // it feeds — clamped to `>= 1`, and `0` as a marker for the `d >= 4/5`
    // branch — and the dump published that instead of upstream's raw binding.
    // The field's only consumer was that dump; removing it makes reading the
    // marker as a reported value inexpressible rather than merely discouraged.
    // Use `reward_pulser::start_step_eta` to REPORT `eta`/`expectedBlocks`.
}

/// Force a complete reward update from the CURRENT epoch state — dugite's
/// equivalent of `cardano-streamer` forcing its pulser before dumping.
///
/// # Why this is well-defined at an epoch-boundary dump point
///
/// Every input `startStep` freezes already has its final value for the whole
/// of the epoch by the time its first block lands:
///
/// * `nesBprev` (blocks made) was rotated AT the boundary,
/// * `ssFee` was frozen by SNAP AT the boundary,
/// * `casReserves` moved at the boundary and is immobile mid-epoch,
/// * the `go` snapshot rotated at the boundary, and the RUPD computed during
///   this epoch is the one that reads it (`compute_reward_update` runs before
///   SNAP).
///
/// So forcing the fold at the first block of epoch N yields the SAME answer
/// the pulser reaches at epoch N's `4k/f` mark. That is what makes a
/// boundary-time dump comparable against cardano-streamer's, which forces its
/// own pulser at the identical instant.
///
/// This is deliberately NOT how production computes rewards — production
/// pulses incrementally across the epoch ([`pulse_rupd_member_fold`]) and
/// applies the result at the next boundary. This is a read-only observation
/// of what that pulser is going to conclude, for cross-validation only.
///
/// Returns `None` before there is a `go` snapshot to fold over (Byron, and the
/// first Shelley epochs).
pub fn forced_reward_update(state: &LedgerState) -> Option<ForcedRewardUpdate> {
    let go = state.epochs.snapshots.go.as_ref()?;
    let pp = &state.epochs.prev_protocol_params;
    let blocks: u64 = state.epochs.snapshots.bprev_blocks_by_pool.values().sum();

    // Identical call to the one `apply.rs` makes at the 4k/f mark — the shared
    // `start_step_monetary` rather than a second copy of the arithmetic, which
    // is the N-copies trap that produced #985/#1015/#977.
    let monetary = crate::state::reward_pulser::start_step_monetary(
        (pp.rho.numerator, pp.rho.denominator),
        (pp.tau.numerator, pp.tau.denominator),
        (
            state.epochs.prev_d.numerator,
            state.epochs.prev_d.denominator,
        ),
        pp.active_slot_coeff_rational(),
        state.epochs.reserves.0,
        state.epochs.snapshots.ss_fee.0,
        blocks,
        state.epoch_length,
        state.max_lovelace_supply,
    );

    let reward_accounts: HashMap<Hash32, Lovelace> = state
        .certs
        .reward_accounts
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect();

    // `prepulsed: None` forces a full fold here rather than consuming the
    // in-flight pulse state: this is an observation, and draining the
    // production fold would change what the next boundary applies.
    let rupd = compute_reward_update(
        pp,
        &state.epochs.prev_d,
        state.epochs.prev_protocol_version_major,
        Some(go),
        &state.epochs.snapshots.bprev_blocks_by_pool,
        state.epochs.snapshots.ss_fee,
        state.epochs.reserves,
        state.epochs.treasury,
        &reward_accounts,
        state.epochs.rupd_addrs_rew.as_deref(),
        state.epoch_length,
        state.shelley_transition_epoch,
        state.max_lovelace_supply,
        &state.epochs.non_myopic,
        Some(monetary),
        None,
    );

    let total_distributed: u64 = rupd.rewards.values().map(|v| v.0).sum();
    Some(ForcedRewardUpdate {
        delta_r1: monetary.delta_r1,
        delta_r2: monetary.r.saturating_sub(total_distributed),
        delta_t1: monetary.delta_t1,
        r_pot: monetary
            .delta_r1
            .saturating_add(state.epochs.snapshots.ss_fee.0),
        reward_pot: monetary.r,
        total_distributed,
    })
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

    /// #1071 / #1073: the cstreamer-format dump's `rupdNext` read
    /// `EpochSubState::pending_reward_update`, which has NO writer on the
    /// modern path — so it was unconditionally `null` while cardano-streamer
    /// populated it at every epoch, and the single most important field of a
    /// reward cross-validation dataset compared vacuously.
    ///
    /// The load-bearing assertion is `is_some()`: a `None` here is exactly the
    /// old always-null behaviour wearing a new name. The identities are
    /// asserted alongside because `deltaR1` previously carried dugite's NET
    /// signed `delta_reserves` under the name of the GROSS expansion, and a
    /// non-null value with the wrong term inside it is worse than a null.
    #[test]
    fn forced_reward_update_is_non_vacuous_and_internally_consistent() {
        use crate::state::LedgerState;
        use dugite_primitives::protocol_params::ProtocolParameters;

        let mut params = ProtocolParameters::mainnet_defaults();
        params.rho = rat(3, 1000);
        params.tau = rat(20, 100);
        params.n_opt = 150;

        let mut state = LedgerState::new(params.clone());
        state.epoch_length = 432_000;
        state.max_lovelace_supply = 45_000_000_000_000_000;
        state.epochs.prev_protocol_params = params;
        state.epochs.prev_d = rat(0, 1);
        state.epochs.prev_protocol_version_major = 6;
        state.epochs.reserves = Lovelace(13_000_000_000_000_000);
        state.epochs.treasury = Lovelace(1_000_000_000_000);
        state.epochs.snapshots.ss_fee = Lovelace(50_000_000_000);

        // A `go` snapshot with one pool that actually minted blocks — without
        // this the fold has nothing to distribute and the test would pass
        // vacuously on an empty result. The pool must appear in `bprev` too,
        // or the reward-distribution loop skips it as a zero-block pool.
        let pool = Hash28::from_bytes([7u8; 28]);
        let cred = Hash32::from_bytes([9u8; 32]);
        let mut pool_stake = HashMap::new();
        pool_stake.insert(pool, Lovelace(10_000_000_000_000));
        let mut delegations = HashMap::new();
        delegations.insert(cred, pool);
        let mut stake_distribution = HashMap::new();
        stake_distribution.insert(cred, Lovelace(10_000_000_000_000));
        let mut pool_params = HashMap::new();
        pool_params.insert(pool, pool_reg(pool, 0, 340_000_000, (1, 100), vec![], 9));
        // pv 6 applies the `fvAddrsRew` prefilter (mainnet epochs 208-271 are
        // pv 2-4, so this IS the reachable path). With `rupd_addrs_rew` unset
        // the fold falls back to the boundary accounts, so the credential has
        // to be registered or every member reward is filtered out and the
        // test measures nothing.
        state.certs.reward_accounts.insert(cred, Lovelace(0));
        state.epochs.snapshots.go = Some(StakeSnapshot {
            epoch: EpochNo(10),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(50_000_000_000),
            epoch_block_count: 21_000,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });
        let mut bprev = HashMap::new();
        bprev.insert(pool, 21_000u64);
        state.epochs.snapshots.bprev_blocks_by_pool = Arc::new(bprev);

        let r = super::forced_reward_update(&state)
            .expect("a go snapshot is present, so a reward update must be computable");

        // The decomposition cardano-streamer reports, checked as identities
        // rather than as literals — the literals depend on the fold, the
        // identities are what make the six fields a decomposition at all.
        assert_eq!(
            r.r_pot,
            r.delta_r1 + state.epochs.snapshots.ss_fee.0,
            "rPot = deltaR1 + epochFees"
        );
        assert_eq!(
            r.reward_pot,
            r.r_pot - r.delta_t1,
            "rewardPot = rPot - deltaT1"
        );
        assert_eq!(
            r.delta_r2,
            r.reward_pot - r.total_distributed,
            "deltaR2 = rewardPot - totalDistributed"
        );
        assert!(
            r.delta_r1 > 0,
            "rho * reserves must be non-zero for a 13e15 reserve — a zero here \
             means the monetary step never ran"
        );
        assert!(
            r.total_distributed > 0,
            "the fold must actually reward the seeded credential; zero means it \
             skipped every pool and the test is measuring nothing"
        );
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
            &Default::default(),
            None,
            None,
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
            &Default::default(),
            None,
            None,
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
    /// Dugite at that boundary computed 352_905_247 (diff = +3505). The
    /// arithmetic discrepancy itself was resolved (#438); what this scaffold
    /// still cannot do is assert byte-equality end-to-end, because that needs
    /// a real GO snapshot + `bprev` counts for epoch 1268 fed through
    /// `compute_reward_update`, and that fixture is multi-GB and not checked
    /// into the repo.
    ///
    /// Unblock condition: commit (or fetch on demand) a preview epoch-1268 GO
    /// snapshot, then drop the `#[ignore]`. Until then this pins the oracle
    /// values only.
    #[test]
    #[ignore = "needs an uncommitted preview epoch-1268 GO snapshot fixture"]
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
        // Two files are structurally exempt, and neither is a *production*
        // writer — which is what this invariant is actually about:
        //
        //   snapshot_format.rs  pass-through clone of an `Option` field for
        //                       backward-compat snapshot loading.
        //   test_fixtures.rs    the #967 snapshot-layout fixture, which must
        //                       set every field to `Some` precisely so the
        //                       format hash covers the layout INSIDE it.
        //                       bincode writes no payload for a `None`, so a
        //                       fixture that left this field `None` would make
        //                       `PendingRewardUpdate`'s layout invisible to the
        //                       guard — the exact blind spot #967 closed. It is
        //                       never linked into a running node.
        let real: Vec<_> = hits
            .into_iter()
            .filter(|(p, _)| !p.ends_with("snapshot_format.rs") && !p.ends_with("test_fixtures.rs"))
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

    // ─── #1071 follow-up (defect 3): zero-amount leader rewards ────────────

    /// A leader reward computing to EXACTLY ZERO must still produce a
    /// `PoolRewardInfo::leader` entry, not `None`. Matches a REAL
    /// cardano-node 11.0.1 capture: both `tests/fixtures/nesru/pulsing.hex`
    /// (`rewLeaders`) and `complete-nonzero.hex` (`rs`) carry a
    /// `[1, pool, 0]` LeaderReward entry. Haskell's `collectLRs` never gates
    /// on amount, only on the pv<=6 registration prefilter
    /// (`hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered op`)
    /// — dugite previously ALSO gated on `operator_reward > 0`, silently
    /// dropping this real case.
    ///
    /// Cost=0, margin=0, and no self-delegation is the concrete scenario
    /// that makes `operator_reward` compute to exactly 0: with
    /// `pool_reward > cost`, `operator_reward = cost + floor((margin +
    /// (1-margin)*(self/σ)) * (pool_reward - cost))`, and `cost=0, margin=0,
    /// self=0` makes the whole bracket 0 regardless of how large
    /// `pool_reward` is.
    #[test]
    fn zero_amount_leader_reward_is_a_present_table_entry_not_none() {
        const POOL: u8 = 0x50;
        const MEMBER: u8 = 0x51;
        let pool_id = h28(POOL);
        let member_cred = cred32(MEMBER);
        let op_key = cred32(0xd0);

        let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
        let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
        delegations.insert(member_cred, pool_id);
        stake_distribution.insert(member_cred, Lovelace(1_000_000_000_000));

        let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
        pool_stake.insert(pool_id, Lovelace(1_000_000_000_000));

        let mut pool_params: HashMap<Hash28, PoolRegistration> = HashMap::new();
        // cost=0, margin=0/1, pledge=0, owners=[] (no self-delegation).
        pool_params.insert(pool_id, pool_reg(pool_id, 0, 0, (0, 1), vec![], 0xd0));

        let go = StakeSnapshot {
            epoch: EpochNo(10),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(pool_id, 10);

        let mut params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        params.a0 = rat(3, 10);
        params.n_opt = 500;

        let registered = |_: &Hash32| true;
        let table = super::build_pool_reward_table(
            &go,
            &bprev,
            &params,
            params.n_opt.max(1),
            1_000_000_000,     // reward_pot
            2_000_000_000_000, // total_stake
            1_000_000_000_000, // total_active_stake
            10,                // total_blocks_in_epoch
            true,              // d_ge_4_5 — bypasses blocks-made dependence
            1,
            1,
            11, // prev_protocol_version_major
            &registered,
        );

        let info = table.get(&pool_id).expect(
            "pool must produce a table entry: pool_active_stake>0, \
             reward_pot>0, d_ge_4_5=true makes pool_reward=max_pool \
             regardless of blocks_made",
        );
        assert!(
            info.pool_reward > 0,
            "sanity: the pool's whole reward pot must be nonzero for this \
             test to mean anything (got {})",
            info.pool_reward
        );
        assert_eq!(info.cost, 0);
        assert_eq!(info.margin_num, 0);
        assert_eq!(
            info.leader,
            Some((op_key, 0)),
            "cost=0, margin=0, self_delegated=0 must produce \
             operator_reward=0 — and the table entry must be PRESENT \
             (Some((op_key, 0))), matching Haskell's Set Reward always \
             carrying the LeaderReward entry once the prefilter passes, \
             not None"
        );
    }

    /// End to end: the zero-amount leader entry from the test above must
    /// reach `raw_rewards` (the WIRE `rs` field's source) while being
    /// EXCLUDED from `rewards` (the credited-amount map) — a 0 lovelace
    /// credit is not a credit, and this is the aggregation-logic boundary
    /// the fix deliberately does not cross (see the comment at
    /// `compute_reward_update`'s leader-rewards loop).
    #[test]
    fn zero_amount_leader_reward_reaches_raw_rewards_but_not_credited_rewards() {
        const POOL: u8 = 0x52;
        const MEMBER: u8 = 0x53;
        let pool_id = h28(POOL);
        let member_cred = cred32(MEMBER);
        let op_key = cred32(0xd3);

        let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
        let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
        delegations.insert(member_cred, pool_id);
        stake_distribution.insert(member_cred, Lovelace(1_000_000_000_000));

        let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
        pool_stake.insert(pool_id, Lovelace(1_000_000_000_000));

        let mut pool_params: HashMap<Hash28, PoolRegistration> = HashMap::new();
        pool_params.insert(pool_id, pool_reg(pool_id, 0, 0, (0, 1), vec![], 0xd3));

        let go = StakeSnapshot {
            epoch: EpochNo(10),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(pool_id, 10);

        let mut params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        params.a0 = rat(3, 10);
        params.n_opt = 500;
        // rho=1/1 makes `expansion = floor(rho * reserves) = reserves`
        // trivially, so the frozen `MonetaryStep.delta_r1` below can be
        // chosen freely and still satisfy `compute_reward_update`'s
        // consistency `debug_assert_eq!`.
        params.rho = rat(1, 1);

        let monetary = crate::state::reward_pulser::MonetaryStep {
            delta_r1: 1_000, // must equal floor(rho * reserves) = 1 * 1_000
            delta_t1: 0,
            r: 1_000_000_000, // the pool-distribution reward pot, chosen freely
            expected_blocks: 0,
            total_stake: 2_000_000_000_000,
        };

        let rupd = super::compute_reward_update(
            &params,
            &rat(1, 1), // prev_d — d_ge_4_5=true, matching the frozen expansion
            11,         // prev_protocol_version_major (pv>2: aggregation, not deleteFindMin)
            Some(&go),
            &bprev,
            Lovelace(0), // ss_fee
            Lovelace(1_000),
            Lovelace(0),
            &HashMap::new(),
            None,
            86_400,
            0,
            super::super::MAX_LOVELACE_SUPPLY,
            &Default::default(),
            Some(monetary),
            None,
        );

        let raw = rupd.raw_rewards.get(&op_key).expect(
            "raw_rewards (the WIRE rs field's source) must carry the \
             zero-amount leader entry",
        );
        assert_eq!(raw.len(), 1);
        assert!(!raw[0].is_member, "LeaderReward, not MemberReward");
        assert_eq!(raw[0].pool_id, pool_id);
        assert_eq!(raw[0].amount, 0);

        assert!(
            !rupd.rewards.contains_key(&op_key),
            "a ZERO-lovelace entry must never appear in the CREDITED \
             rewards map — this is the aggregation-logic boundary this fix \
             deliberately does not cross"
        );
    }

    // ─── #1071 follow-up (defect 2): pulse/completion TIMING ───────────────

    /// The tick that CREATES the pulser performs ZERO pulses, and the wire
    /// only shows `Complete` starting the tick AFTER the one whose pulse
    /// drains the queue — never on that same tick. Both facts are oracle-
    /// verified against `PulsingReward.hs`'s `pulseStep` clause order (see
    /// `pulse_rupd_member_fold`'s own doc for the quoted source), not
    /// inferred.
    ///
    /// Drives FOUR successive calls to `pulse_rupd_member_fold`, each one
    /// standing in for one block/tick, over a 3-credential queue with
    /// `pulse_size` forced to 1 (via `security_param_k=1`) so each tick can
    /// pulse at most one credential — three ticks to drain, a fourth to
    /// observe the completion lag. Before this fix, tick 0 also pulsed
    /// (draining the queue one tick early) and tick "3" (the draining tick)
    /// already showed `Complete`.
    #[test]
    fn pulse_rupd_member_fold_matches_haskells_creation_and_completion_timing() {
        use crate::state::reward_pulser::{
            FreeVars, MonetaryStep, PulsingRewUpdate, RewardSnapShot,
        };
        use crate::state::LedgerState;

        const POOL: u8 = 0x60;
        let pool_id = h28(POOL);

        let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
        let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
        for i in 0..3u8 {
            let cred = cred32(0x61 + i);
            delegations.insert(cred, pool_id);
            stake_distribution.insert(cred, Lovelace(1_000_000_000));
        }
        let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
        pool_stake.insert(pool_id, Lovelace(3_000_000_000));
        let mut pool_params: HashMap<Hash28, PoolRegistration> = HashMap::new();
        pool_params.insert(
            pool_id,
            pool_reg(pool_id, 0, 340_000_000, (1, 20), vec![], 0x70),
        );

        let go = StakeSnapshot {
            epoch: EpochNo(10),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };

        let params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        let mut state = LedgerState::new(params.clone());
        state.epochs.snapshots.go = Some(go);
        state.epochs.prev_protocol_params = params;
        state.epochs.prev_protocol_version_major = 11; // pv>=7: no pv<=6 prefilter gate
        state.epochs.prev_d = rat(0, 1);

        // Simulate the CREATION tick's freeze (mirrors `apply.rs`'s
        // mark-crossing block, BEFORE it calls `pulse_rupd_member_fold`):
        // `rupd_monetary`/`rupd_snapshot = Some(Pulsing(...))` are set, but
        // the fold itself (`rupd_fold.fold`) does not exist yet.
        let monetary = MonetaryStep {
            delta_r1: 0,
            delta_t1: 0,
            r: 1_000_000_000,
            expected_blocks: 0,
            total_stake: 10_000_000_000_000,
        };
        state.epochs.rupd_monetary = Some(monetary);
        state.epochs.rupd_pulser_started = true;
        state.epochs.rupd_snapshot = Some(PulsingRewUpdate::Pulsing(Box::new(RewardSnapShot {
            fees: Lovelace(0),
            protocol_version: (11, 0),
            non_myopic: Default::default(),
            delta_r1: Lovelace(0),
            r: Lovelace(1_000_000_000),
            delta_t1: Lovelace(0),
            likelihoods: HashMap::new(),
            leaders: HashMap::new(),
            free_vars: FreeVars {
                addrs_rew: None,
                total_stake: 10_000_000_000_000,
                prot_ver: (11, 0),
            },
        })));

        let k: u64 = 1; // pulse_size = max(1, ceil(3 / (4*1))) = 1
        let call = |state: &mut LedgerState| {
            let prev_d = state.epochs.prev_d.clone();
            let pv = state.epochs.prev_protocol_version_major;
            let epoch_length = state.epoch_length;
            super::pulse_rupd_member_fold(&mut state.epochs, &prev_d, pv, k, epoch_length);
        };
        let is_pulsing = |state: &LedgerState| {
            matches!(
                state.epochs.rupd_snapshot,
                Some(PulsingRewUpdate::Pulsing(_))
            )
        };
        let is_complete = |state: &LedgerState| {
            matches!(
                state.epochs.rupd_snapshot,
                Some(PulsingRewUpdate::Complete(_))
            )
        };

        // Tick 0 — the SAME tick that just froze the pulser above: must
        // build the fold WITHOUT pulsing it.
        call(&mut state);
        assert_eq!(
            state.epochs.rupd_fold.remaining(),
            3,
            "the creation tick must perform ZERO pulses — Haskell's \
             `startStep` is a pure constructor; `pulseStep`'s advancing \
             clause only matches starting the NEXT tick"
        );
        assert!(
            is_pulsing(&state),
            "wire must still read Pulsing after the creation tick"
        );

        // Tick 1 — the first REAL pulse.
        call(&mut state);
        assert_eq!(
            state.epochs.rupd_fold.remaining(),
            2,
            "tick 1 is the first tick that actually pulses"
        );
        assert!(is_pulsing(&state));

        // Tick 2.
        call(&mut state);
        assert_eq!(state.epochs.rupd_fold.remaining(), 1);
        assert!(is_pulsing(&state));

        // Tick 3 — this pulse DRAINS the queue (the 3rd and last credential).
        // The internal fold becomes done as a RESULT of this call, but the
        // wire must still read Pulsing: completion has a one-tick lag.
        call(&mut state);
        assert_eq!(state.epochs.rupd_fold.remaining(), 0, "queue now drained");
        assert!(
            state.epochs.rupd_fold.is_complete(),
            "internal bookkeeping (`InFlightFold::is_complete`) is \
             immediate — that is correct and untouched by this fix"
        );
        assert!(
            is_pulsing(&state),
            "the WIRE must NOT show Complete on the SAME tick that drained \
             the queue — this is the one-tick lag `pulseStep`'s clause \
             order requires (`done` is checked BEFORE pulsing, never after)"
        );

        // Tick 4 — no new pulse (the fold is already exhausted); THIS tick
        // promotes the wire to Complete.
        call(&mut state);
        assert!(
            is_complete(&state),
            "one tick after the draining pulse, the wire must show Complete"
        );
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
            &Default::default(),
            None,
            None,
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
                &Default::default(),
                None,
                None,
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

    /// `compute_reward_update` must READ the frozen `total_stake`, not
    /// recompute it.
    ///
    /// The unit tests on `start_step_monetary` prove the value is FROZEN
    /// correctly. They say nothing about whether the consumer reads it — and
    /// disarming the consumer left all of them green, which is #1057's lesson
    /// verbatim: a RED-proven unit test bounds the function, not the system.
    ///
    /// This drives the real entry point twice with identical inputs EXCEPT the
    /// frozen `total_stake`, and asserts the rewards differ. A build that
    /// recomputes `maxSupply - reserves` produces identical output both times,
    /// because `reserves` is the same in both calls.
    #[test]
    fn compute_reward_update_reads_the_frozen_total_stake() {
        use crate::state::reward_pulser::MonetaryStep;

        const POOL: u8 = 0x11;
        const MEMBER: u8 = 0x22;

        let mut delegations = HashMap::new();
        let mut stake_distribution = HashMap::new();
        let mut pool_stake = HashMap::new();
        let mut pool_params = HashMap::new();

        // Pool stake must sit BELOW saturation or `sigma` clamps to z0 = 1/nOpt
        // and the total_stake difference becomes invisible — which is exactly
        // what the first version of this fixture did, at 500e12 against a
        // 31e15 total (sigma_raw = 0.016 vs z0 = 0.002, clamped in both runs
        // and so identical rewards for the wrong reason).
        // 30e12 / 31e15 = 0.00097 < 0.002, so sigma tracks total_stake.
        delegations.insert(cred32(MEMBER), h28(POOL));
        stake_distribution.insert(cred32(MEMBER), Lovelace(30_000_000_000_000));
        pool_stake.insert(h28(POOL), Lovelace(30_000_000_000_000));
        pool_params.insert(
            h28(POOL),
            pool_reg(h28(POOL), 0, 340_000_000, (1, 10), vec![], 0xd1),
        );

        let go = StakeSnapshot {
            epoch: EpochNo(200),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(h28(POOL), 21_600);

        let mut params = dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
        params.rho = rat(3, 1000);
        params.tau = rat(1, 5);
        params.a0 = rat(3, 10);
        params.n_opt = 500;
        params.active_slots_coeff = 0.05;

        let reserves = Lovelace(14_000_000_000_000_000);
        // Same monetary terms in both runs; ONLY total_stake differs, so any
        // difference in the output has exactly one possible cause.
        let base = MonetaryStep {
            delta_r1: 42_000_000_000_000,
            delta_t1: 8_400_000_000_000,
            r: 33_600_000_000_000,
            expected_blocks: 21_600,
            total_stake: 31_000_000_000_000_000,
        };
        let shifted = MonetaryStep {
            // The AVVM return, at mainnet's order of magnitude.
            total_stake: 31_000_000_000_000_000 - 318_200_635_000_000,
            ..base
        };

        let run = |m: MonetaryStep| {
            super::compute_reward_update(
                &params,
                &rat(0, 1),
                11,
                Some(&go),
                &bprev,
                Lovelace(0),
                reserves,
                Lovelace(0),
                &HashMap::new(),
                None,
                432_000,
                0,
                super::super::MAX_LOVELACE_SUPPLY,
                &Default::default(),
                Some(m),
                None,
            )
        };

        let a = run(base);
        let b = run(shifted);

        let ra = a.rewards.get(&cred32(MEMBER)).map(|l| l.0).unwrap_or(0);
        let rb = b.rewards.get(&cred32(MEMBER)).map(|l| l.0).unwrap_or(0);
        assert!(
            ra > 0 && rb > 0,
            "both runs must actually pay the member ({ra}, {rb}) — a fixture \
             that pays nothing would satisfy the inequality below vacuously"
        );
        assert_ne!(
            ra, rb,
            "total_stake is sigma's denominator, so changing ONLY the frozen \
             value must move the member reward. Identical rewards mean the \
             frozen value was ignored and maxSupply - reserves was recomputed \
             — which is exactly the AVVM divergence pending_avvm_return used \
             to patch"
        );
    }
}
