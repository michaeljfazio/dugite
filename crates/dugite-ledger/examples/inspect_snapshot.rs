//! One-shot diagnostic: load a ledger snapshot and dump per-credential
//! reward/delegation state for an account + pool delegators.
//!
//! Usage:
//!   cargo run --release -p dugite-ledger --example inspect_snapshot -- \
//!       <snapshot.bin> <28-byte-cred-hex> <28-byte-pool-id-hex>

use std::path::PathBuf;

use dugite_ledger::state::StakeSnapshot;
use dugite_ledger::LedgerState;
use dugite_primitives::hash::{Hash28, Hash32};

fn hex_decode(s: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char).to_digit(16).ok_or("bad hex digit")? as u8;
        let lo = (bytes[i + 1] as char).to_digit(16).ok_or("bad hex digit")? as u8;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!(
            "usage: {} <snapshot.bin> <28-byte-cred-hex> <28-byte-pool-id-hex>",
            args[0]
        );
        std::process::exit(1);
    }

    let snapshot_path = PathBuf::from(&args[1]);
    let cred_bytes = hex_decode(&args[2])?;
    let pool_bytes = hex_decode(&args[3])?;

    if cred_bytes.len() != 28 {
        return Err("cred must be 28 bytes".into());
    }
    if pool_bytes.len() != 28 {
        return Err("pool id must be 28 bytes".into());
    }

    let cred28 = Hash28::from_bytes(
        <[u8; 28]>::try_from(cred_bytes.as_slice()).expect("cred is 28 bytes"),
    );
    let cred32_key = cred28.to_hash32_padded();
    let pool_id = Hash28::from_bytes(
        <[u8; 28]>::try_from(pool_bytes.as_slice()).expect("pool is 28 bytes"),
    );

    println!("Loading snapshot: {}", snapshot_path.display());
    let state = LedgerState::load_snapshot(&snapshot_path)?;
    println!(
        "Loaded: epoch={} tip={:?} era={:?}",
        state.epoch.0, state.tip.point, state.era
    );

    println!("\n=== Credential {} ===", cred32_key.to_hex());

    let in_delegations = state.certs.delegations.get(&cred32_key);
    println!("certs.delegations:    {:?}", in_delegations);

    let reward_balance = state.certs.reward_accounts.get(&cred32_key).copied();
    println!("certs.reward_accounts: {:?}", reward_balance);

    let stake = state
        .certs
        .stake_distribution
        .stake_map
        .get(&cred32_key)
        .copied();
    println!("certs.stake_distribution.stake_map: {:?}", stake);

    println!("\n=== Pool {} ===", pool_id.to_hex());
    let pool_in_params = state.certs.pool_params.contains_key(&pool_id);
    println!("certs.pool_params contains pool: {}", pool_in_params);

    if let Some(pool_reg) = state.certs.pool_params.get(&pool_id) {
        println!(
            "certs.pool_params: pledge={} cost={} owners_count={}",
            pool_reg.pledge.0,
            pool_reg.cost.0,
            pool_reg.owners.len(),
        );
        for owner in &pool_reg.owners {
            let owner_h32 = owner.to_hash32_padded();
            let owner_deleg = state.certs.delegations.get(&owner_h32).copied();
            let owner_stake = state
                .certs
                .stake_distribution
                .stake_map
                .get(&owner_h32)
                .copied();
            let owner_reward = state.certs.reward_accounts.get(&owner_h32).copied();
            println!(
                "  owner {} (h32 {}): delegated_to={:?} stake={:?} reward_acc={:?}",
                owner.to_hex(),
                owner_h32.to_hex(),
                owner_deleg,
                owner_stake,
                owner_reward,
            );
        }
    }

    // bprev blocks by pool (used at THE NEXT boundary RUPD).
    let bprev = state
        .epochs
        .snapshots
        .bprev_blocks_by_pool
        .get(&pool_id)
        .copied();
    println!("epochs.snapshots.bprev_blocks_by_pool[pool]: {:?}", bprev);

    // current-epoch incremental blocks
    let cur_blocks = state.consensus.epoch_blocks_by_pool.get(&pool_id).copied();
    println!("consensus.epoch_blocks_by_pool[pool]: {:?}", cur_blocks);

    fn dump_snap(
        name: &str,
        snap: Option<&StakeSnapshot>,
        cred: &Hash32,
        pool: &Hash28,
    ) {
        match snap {
            None => println!("{:>5}: <None>", name),
            Some(s) => {
                let pool_stake = s.pool_stake.get(pool).copied();
                let pool_in_params = s.pool_params.contains_key(pool);
                let delegators_count = s
                    .delegations
                    .iter()
                    .filter(|(_, p)| *p == pool)
                    .count();
                let our_deleg = s.delegations.get(cred);
                let our_stake = s.stake_distribution.get(cred).copied();
                println!(
                    "{:>5}: epoch={} pool_in_params={} pool_stake={:?} delegators_of_pool={} cred->pool={:?} cred_stake={:?} fees={}",
                    name,
                    s.epoch.0,
                    pool_in_params,
                    pool_stake,
                    delegators_count,
                    our_deleg,
                    our_stake,
                    s.epoch_fees.0,
                );
                // Show owner stake aggregation per pool_reg
                if let Some(pool_reg) = s.pool_params.get(pool) {
                    println!(
                        "       SNAPSHOT pool_reg: pledge={} cost={} owners_count={} reward_acc_len={}",
                        pool_reg.pledge.0, pool_reg.cost.0, pool_reg.owners.len(), pool_reg.reward_account.len()
                    );
                    let mut owner_stake_total = 0u64;
                    for owner in &pool_reg.owners {
                        let owner_h32 = owner.to_hash32_padded();
                        let owner_deleg = s.delegations.get(&owner_h32);
                        let owner_st = s.stake_distribution.get(&owner_h32).map(|l| l.0).unwrap_or(0);
                        let in_this_pool = owner_deleg == Some(pool);
                        println!(
                            "       owner_h32={} delegated_to_this_pool={} stake_in_snap={}",
                            owner_h32.to_hex(),
                            in_this_pool,
                            owner_st,
                        );
                        if in_this_pool {
                            owner_stake_total += owner_st;
                        }
                    }
                    println!(
                        "       owner_stake_sum_in_pool={} pledge_required={}",
                        owner_stake_total, pool_reg.pledge.0,
                    );
                }
            }
        }
    }

    println!("\n=== Stake snapshots (mark/set/go) ===");
    dump_snap("mark", state.epochs.snapshots.mark.as_ref(), &cred32_key, &pool_id);
    dump_snap("set", state.epochs.snapshots.set.as_ref(), &cred32_key, &pool_id);
    dump_snap("go", state.epochs.snapshots.go.as_ref(), &cred32_key, &pool_id);

    println!("\n=== Aggregate bprev ===");
    {
        let bprev = &state.epochs.snapshots.bprev_blocks_by_pool;
        let pools_with_blocks = bprev.values().filter(|n| **n > 0).count();
        let total_blocks: u64 = bprev.values().sum();
        println!(
            "bprev: pools_with_blocks={} total_blocks={}",
            pools_with_blocks, total_blocks
        );
    }

    println!("\n=== Aggregate consensus.epoch_blocks_by_pool (current epoch) ===");
    {
        let cur = &state.consensus.epoch_blocks_by_pool;
        let pools_with_blocks = cur.values().filter(|n| **n > 0).count();
        let total_blocks: u64 = cur.values().sum();
        println!(
            "current: pools_with_blocks={} total_blocks={}",
            pools_with_blocks, total_blocks
        );
    }

    println!("\n=== certs.pool_params count ===");
    println!("pool_params total: {}", state.certs.pool_params.len());

    // Audit: how many pools have empty owners list in certs vs in each snapshot?
    println!("\n=== Pool params owners audit ===");
    {
        let certs_no_owners = state
            .certs
            .pool_params
            .values()
            .filter(|p| p.owners.is_empty())
            .count();
        let certs_total = state.certs.pool_params.len();
        println!("certs.pool_params: {}/{} pools have no owners", certs_no_owners, certs_total);
        for (name, snap) in [
            ("mark", state.epochs.snapshots.mark.as_ref()),
            ("set", state.epochs.snapshots.set.as_ref()),
            ("go", state.epochs.snapshots.go.as_ref()),
        ] {
            if let Some(s) = snap {
                let no_owners = s.pool_params.values().filter(|p| p.owners.is_empty()).count();
                let total = s.pool_params.len();
                println!("{}: {}/{} pools have no owners", name, no_owners, total);
            }
        }
    }

    println!("\n=== GO snapshot pool stats ===");
    if let Some(go) = state.epochs.snapshots.go.as_ref() {
        let pool_params_count = go.pool_params.len();
        let pool_stake_count = go.pool_stake.len();
        let total_active_stake: u64 = go.pool_stake.values().map(|l| l.0).sum();
        let pools_with_zero_stake = go.pool_stake.values().filter(|l| l.0 == 0).count();
        println!(
            "go: pool_params={} pool_stake_entries={} total_active_stake_lovelace={} pools_with_zero_stake={}",
            pool_params_count, pool_stake_count, total_active_stake, pools_with_zero_stake
        );
        // How many GO pools also appear in bprev with >0 blocks?
        let bprev = &state.epochs.snapshots.bprev_blocks_by_pool;
        let go_pools_in_bprev = go
            .pool_stake
            .keys()
            .filter(|p| bprev.get(*p).copied().unwrap_or(0) > 0)
            .count();
        let go_pools_in_pool_params = go
            .pool_stake
            .keys()
            .filter(|p| go.pool_params.contains_key(*p))
            .count();
        println!(
            "go pools active (have blocks AND in pool_params): {}",
            go.pool_stake
                .keys()
                .filter(|p| {
                    bprev.get(*p).copied().unwrap_or(0) > 0 && go.pool_params.contains_key(*p)
                })
                .count()
        );
        println!(
            "go pools in bprev: {} / go pools in pool_params: {}",
            go_pools_in_bprev, go_pools_in_pool_params
        );
    }

    println!("\n=== Reserves/Treasury ===");
    println!("reserves: {}", state.epochs.reserves.0);
    println!("treasury: {}", state.epochs.treasury.0);
    println!("epoch_fees: {}", state.utxo.epoch_fees.0);
    println!("ss_fee: {}", state.epochs.snapshots.ss_fee.0);

    // Last RUPD applied (set in conway.rs after applying RUPD).
    println!("\n=== Last applied RUPD ===");
    match &state.epochs.last_applied_rupd {
        None => println!("(no last_applied_rupd recorded — not persisted in snapshot)"),
        Some(rupd) => {
            let total_in_rupd: u64 = rupd.rewards.values().map(|l| l.0).sum();
            let has_our_cred = rupd.rewards.get(&cred32_key);
            println!(
                "rewards count: {}   total: {}   our cred reward: {:?}",
                rupd.rewards.len(),
                total_in_rupd,
                has_our_cred
            );
            println!(
                "delta_reserves: {}   delta_treasury: {}",
                rupd.delta_reserves, rupd.delta_treasury
            );
        }
    }

    // Manually recompute the RUPD that WOULD be applied at the next boundary
    // using the current GO snapshot + bprev + ss_fee — to see whether the
    // per-pool loop is correctly producing rewards for our pool/cred.
    println!("\n=== Recomputed RUPD (would apply at next boundary) ===");
    {
        let rupd = dugite_ledger::compute_reward_update(
            &state.epochs.prev_protocol_params,
            &state.epochs.prev_d,
            state.epochs.prev_protocol_version_major,
            state.epochs.snapshots.go.as_ref(),
            &state.epochs.snapshots.bprev_blocks_by_pool,
            state.epochs.snapshots.ss_fee,
            state.epochs.reserves,
            state.epochs.treasury,
            &state.certs.reward_accounts,
            state.epoch_length,
            state.shelley_transition_epoch,
            state.max_lovelace_supply,
        );

        let total: u64 = rupd.rewards.values().map(|l| l.0).sum();
        println!(
            "rewards count: {}   total_distributed: {}",
            rupd.rewards.len(),
            total
        );
        println!(
            "delta_reserves: {}   delta_treasury: {}",
            rupd.delta_reserves, rupd.delta_treasury
        );
        let our = rupd.rewards.get(&cred32_key).copied();
        println!("our cred reward in RUPD: {:?}", our);

        // How much went to our pool's delegators?
        if let Some(go) = state.epochs.snapshots.go.as_ref() {
            let mut pool_total = 0u64;
            let mut delegators_with_reward = 0;
            for (cred, p) in go.delegations.iter() {
                if p == &pool_id {
                    if let Some(r) = rupd.rewards.get(cred) {
                        pool_total += r.0;
                        delegators_with_reward += 1;
                    }
                }
            }
            println!(
                "pool delegator rewards in RUPD: total={} count={}",
                pool_total, delegators_with_reward
            );
        }

        // Also show the protocol params used
        println!(
            "RUPD pp: rho={}/{} tau={}/{} a0={}/{} n_opt={} d_num={} d_den={} pv_major={}",
            state.epochs.prev_protocol_params.rho.numerator,
            state.epochs.prev_protocol_params.rho.denominator,
            state.epochs.prev_protocol_params.tau.numerator,
            state.epochs.prev_protocol_params.tau.denominator,
            state.epochs.prev_protocol_params.a0.numerator,
            state.epochs.prev_protocol_params.a0.denominator,
            state.epochs.prev_protocol_params.n_opt,
            state.epochs.prev_d.numerator,
            state.epochs.prev_d.denominator,
            state.epochs.prev_protocol_version_major,
        );
    }

    Ok(())
}
