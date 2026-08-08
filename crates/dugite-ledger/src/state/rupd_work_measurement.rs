//! Phase 0 — how long does the boundary reward fold actually take?
//!
//! The pulser design rests on a claim nobody had measured: that computing the
//! whole reward update at the epoch boundary produces a stall large enough to
//! matter, and that spreading it over the epoch is therefore worth
//! restructuring a consensus-critical path. §3.4 of the design says so in as
//! many words — *"That number does not exist yet and this design does not
//! pretend otherwise"*.
//!
//! This produces it:
//!
//! ```text
//! cargo nextest run -p dugite-ledger -E 'test(measure_boundary_fold)' \
//!     --run-ignored all --no-capture --release
//! ```
//!
//! `--release` matters. A debug-build number overstates the stall by roughly an
//! order of magnitude, and would argue for work that is not needed.
//!
//! Deliberately `#[ignore]`d rather than gated: a wall-clock assertion in CI
//! measures the runner's load as much as the code — the flake shape this repo
//! already hit in `dugite-monitor`'s probe timeout. The number belongs in the
//! design doc where a human decides what it means. Only a genuine algorithmic
//! blowup is asserted here, and that assertion is scale-relative, not absolute.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::PoolRegistration;
use super::{EpochNo, StakeSnapshot};
use dugite_primitives::{Hash, Hash28, Hash32, Lovelace};

/// Mainnet as of 2026: ~1.3M registered stake credentials across ~3.1k pools.
const MAINNET_CREDS: usize = 1_300_000;
const MAINNET_POOLS: usize = 3_100;

fn h28(i: usize) -> Hash28 {
    let mut b = [0u8; 28];
    b[..8].copy_from_slice(&(i as u64).to_be_bytes());
    Hash(b)
}

fn h32(i: usize) -> Hash32 {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&(i as u64).to_be_bytes());
    Hash(b)
}

fn pool_reg(i: usize) -> PoolRegistration {
    let mut reward_account = vec![0xe0u8];
    reward_account.extend_from_slice(&h28(i).0);
    PoolRegistration {
        pool_id: h28(i),
        vrf_keyhash: h32(0xff_ff + i),
        pledge: Lovelace(0),
        cost: Lovelace(170_000_000),
        margin_numerator: 1,
        margin_denominator: 50,
        reward_account,
        owners: vec![h28(i)],
        relays: Vec::new(),
        metadata_url: None,
        metadata_hash: None,
    }
}

/// Total delegated stake, held CONSTANT across scales.
///
/// ~25B ADA against a ~37.2B circulation (`maxSupply - reserves`), i.e. the
/// ~65% staked that mainnet actually runs at. Per-credential stake is derived
/// by division rather than fixed, for a reason the first version of this file
/// got wrong: with a fixed 1000 ADA per credential, `sigma` per pool was ~5e-10,
/// `maxPool'` floored to zero, and every MEMBER reward was dropped. The fold
/// then produced exactly one entry per pool and the timing described a loop
/// over 50 pools while claiming to describe 1000 credentials.
const TOTAL_DELEGATED: u64 = 25_000_000_000_000_000;

/// `creds` credentials spread evenly over `pools` pools, every pool minting so
/// that none short-circuits out of the reward loop.
fn synthetic(creds: usize, pools: usize) -> (StakeSnapshot, HashMap<Hash28, u64>) {
    let per_cred = Lovelace(TOTAL_DELEGATED / creds as u64);
    let mut delegations = HashMap::with_capacity(creds);
    let mut stake_distribution = HashMap::with_capacity(creds);
    let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::with_capacity(pools);
    let mut pool_params = HashMap::with_capacity(pools);
    let mut bprev: HashMap<Hash28, u64> = HashMap::with_capacity(pools);

    for p in 0..pools {
        let id = h28(p);
        pool_params.insert(id, pool_reg(p));
        bprev.insert(id, 7);
    }
    for c in 0..creds {
        let pool = h28(c % pools);
        delegations.insert(h32(c), pool);
        stake_distribution.insert(h32(c), per_cred);
        pool_stake.entry(pool).or_insert(Lovelace(0)).0 += per_cred.0;
    }

    (
        StakeSnapshot {
            epoch: EpochNo(500),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(1_000_000_000),
            epoch_block_count: (pools * 7) as u64,
            epoch_blocks_by_pool: Arc::new(bprev.clone()),
        },
        bprev,
    )
}

/// Wall time, and the number of reward entries the fold actually produced.
///
/// The count is not decoration. `compute_reward_update` has five early-return
/// paths, and a synthetic snapshot that trips one of them would time an empty
/// loop and report a reassuring number measured against nothing — the failure
/// family that produced #916/#917/#945 and the all-zero `cli_parity` reports.
/// Every caller here asserts on it.
fn time_fold_checked(creds: usize, pools: usize) -> (Duration, usize) {
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::transaction::Rational;

    let rat = |n: u64, d: u64| Rational {
        numerator: n,
        denominator: d,
    };
    let (go, bprev) = synthetic(creds, pools);
    let mut params = ProtocolParameters::mainnet_defaults();
    params.rho = rat(3, 1000);
    params.tau = rat(1, 5);
    params.a0 = rat(3, 10);
    params.n_opt = 500;
    params.active_slots_coeff = 0.05;

    let accounts: HashMap<Hash32, Lovelace> = (0..creds).map(|c| (h32(c), Lovelace(0))).collect();
    let addrs: HashSet<Hash32> = accounts.keys().copied().collect();

    let t0 = Instant::now();
    let out = super::rewards::compute_reward_update(
        &params,
        &rat(0, 1),
        11,
        Some(&go),
        &bprev,
        Lovelace(1_000_000_000),
        Lovelace(7_800_000_000_000_000),
        Lovelace(0),
        &accounts,
        Some(&addrs),
        432_000,
        0,
        super::MAX_LOVELACE_SUPPLY,
        &Default::default(),
        None,
        None,
    );
    let dt = t0.elapsed();
    let produced = out.rewards.len();
    std::hint::black_box(&out);
    (dt, produced)
}

/// Wall time only, after proving the fold rewarded a real share of the input.
fn time_fold(creds: usize, pools: usize) -> Duration {
    let (dt, produced) = time_fold_checked(creds, pools);
    assert!(
        produced * 2 >= creds,
        "the fold produced {produced} reward entries for {creds} credentials — \
         it hit an early return or a prefilter, so this timing measures an \
         empty loop rather than the boundary work"
    );
    dt
}

/// Measure at increasing credential counts and extrapolate to mainnet.
///
/// Reports per-credential cost so the mainnet figure is an extrapolation with
/// its basis visible, rather than a bare number to be quoted later without it.
#[test]
#[ignore = "measurement, not a gate — see module docs"]
fn measure_boundary_fold_cost() {
    println!("\n    creds    pools          wall     ns/cred");
    println!("    -------------------------------------------");
    let mut last = 0f64;
    for &(creds, pools) in &[
        (1_000usize, 50usize),
        (10_000, 500),
        (50_000, 1_500),
        (200_000, 3_100),
    ] {
        let dt = time_fold(creds, pools);
        let per = dt.as_secs_f64() / creds as f64;
        last = per;
        println!(
            "  {creds:>7}  {pools:>7}  {:>12.1?}  {:>10.0}",
            dt,
            per * 1e9
        );
    }
    let projected = last * MAINNET_CREDS as f64;
    println!("\n  extrapolated to mainnet ({MAINNET_CREDS} creds / {MAINNET_POOLS} pools):");
    println!("      {projected:.2} s inside ONE boundary block\n");
    println!("  Read against a 1 s slot and a ~20 s mainnet block interval: a fold");
    println!("  that fits inside a slot needs no pulser at all; one that does not");
    println!("  is what Phase 3 exists to spread over the epoch.\n");
}

/// The one thing worth asserting automatically: the fold is not super-linear.
///
/// A wall-clock ceiling would measure CI load. Super-linearity would not — a
/// per-credential cost that grows with N means an accidental inner scan (a
/// `pool_stake` lookup gone linear, say), which is a real defect AND makes the
/// extrapolation above wrong in the dangerous direction, understating mainnet.
#[test]
#[ignore = "measurement, not a gate — see module docs"]
fn the_fold_is_not_superlinear_in_credentials() {
    let small = time_fold(5_000, 250).as_secs_f64() / 5_000.0;
    let large = time_fold(50_000, 2_500).as_secs_f64() / 50_000.0;
    println!(
        "  ns/cred at 5k: {:.0}   at 50k: {:.0}   ratio {:.2}x",
        small * 1e9,
        large * 1e9,
        large / small
    );
    assert!(
        large < small * 4.0,
        "per-credential cost grew {:.1}x over a 10x size increase — the fold \
         has a super-linear term, so the mainnet extrapolation understates it",
        large / small
    );
}
