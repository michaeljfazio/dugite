//! Protocol parameter, genesis, and reward query handlers (tags 2, 3, 4, 5, 8, 11, 12, 13, 14, 29).

use tracing::debug;

use super::types::{
    GenesisConfigSnapshot, NodeStateSnapshot, NonMyopicRewardEntry, QueryResult,
    ShelleyPParamsSnapshot,
};

/// Handle GetNonMyopicMemberRewards (tag 2).
pub(crate) fn handle_non_myopic_rewards(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetNonMyopicMemberRewards");
    let mut amounts = Vec::new();
    if let Ok(Some(n)) = decoder.array() {
        for _ in 0..n {
            if let Ok(amt) = decoder.u64() {
                amounts.push(amt);
            } else {
                decoder.skip().ok();
            }
        }
    }
    let stake_amounts = if amounts.is_empty() {
        vec![1_000_000_000_000]
    } else {
        amounts
    };
    let total_stake: u64 = state.stake_pools.iter().map(|p| p.stake).sum();
    let rewards_pot = state.reserves / 200;
    // Build a cost/margin lookup from pool params
    let pool_params_map: std::collections::HashMap<&[u8], &super::types::PoolParamsSnapshot> =
        state
            .pool_params_entries
            .iter()
            .map(|pp| (pp.pool_id.as_slice(), pp))
            .collect();
    let mut result = Vec::new();
    for amount in &stake_amounts {
        let mut pool_rewards = Vec::new();
        for pool in &state.stake_pools {
            if pool.stake == 0 || total_stake == 0 {
                continue;
            }
            let pool_reward =
                (pool.stake as u128 * rewards_pot as u128 / total_stake as u128) as u64;
            // Look up cost/margin from pool params
            let (cost, margin) = if let Some(pp) = pool_params_map.get(pool.pool_id.as_slice()) {
                let m = pp.margin_num as f64 / pp.margin_den.max(1) as f64;
                (pp.cost, m)
            } else {
                (340_000_000, 0.0) // defaults
            };
            let after_cost = pool_reward.saturating_sub(cost);
            let delegator_share = (after_cost as f64 * (1.0 - margin)) as u64;
            let delegator_reward =
                (*amount as u128 * delegator_share as u128 / pool.stake.max(1) as u128) as u64;
            pool_rewards.push((pool.pool_id.clone(), delegator_reward));
        }
        result.push(NonMyopicRewardEntry {
            stake_amount: *amount,
            pool_rewards,
        });
    }
    QueryResult::NonMyopicMemberRewards(result)
}

/// Handle GetCurrentPParams (tag 3).
pub(crate) fn handle_current_pparams(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetCurrentPParams");
    QueryResult::ProtocolParams(Box::new(state.protocol_params.clone()))
}

/// Handle GetProposedPParamsUpdates (tag 4) -- deprecated in Conway.
pub(crate) fn handle_proposed_pparams_updates() -> QueryResult {
    debug!("Query: GetProposedPParamsUpdates");
    QueryResult::ProposedPParamsUpdates
}

/// Handle GetStakeDistribution (tag 5).
pub(crate) fn handle_stake_distribution(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetStakeDistribution");
    QueryResult::StakeDistribution(state.stake_pools.clone())
}

/// Handle GetGenesisConfig (tag 11) -- CompactGenesis.
pub(crate) fn handle_genesis_config(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetGenesisConfig");
    if let Some(ref gc) = state.genesis_config {
        QueryResult::GenesisConfig(Box::new(gc.clone()))
    } else {
        // Fallback: genesis config from node state fields
        QueryResult::GenesisConfig(Box::new(GenesisConfigSnapshot {
            system_start: state.system_start.clone(),
            network_magic: state.network_magic,
            network_id: if state.network_magic == 764824073 {
                1
            } else {
                0
            },
            active_slots_coeff_num: state.active_slots_coeff_num,
            active_slots_coeff_den: state.active_slots_coeff_den,
            security_param: state.security_param,
            epoch_length: state.epoch_length,
            slots_per_kes_period: state.slots_per_kes_period,
            max_kes_evolutions: state.max_kes_evolutions,
            slot_length_micros: state.slot_length_secs * 1_000_000,
            update_quorum: state.update_quorum,
            max_lovelace_supply: state.max_lovelace_supply,
            protocol_params: ShelleyPParamsSnapshot {
                min_fee_a: state.protocol_params.min_fee_a,
                min_fee_b: state.protocol_params.min_fee_b,
                max_block_body_size: state.protocol_params.max_block_body_size as u32,
                max_tx_size: state.protocol_params.max_tx_size as u32,
                max_block_header_size: state.protocol_params.max_block_header_size as u16,
                key_deposit: state.protocol_params.key_deposit,
                pool_deposit: state.protocol_params.pool_deposit,
                e_max: state.protocol_params.e_max as u32,
                n_opt: state.protocol_params.n_opt as u16,
                a0_num: state.protocol_params.a0_num,
                a0_den: state.protocol_params.a0_den,
                rho_num: state.protocol_params.rho_num,
                rho_den: state.protocol_params.rho_den,
                tau_num: state.protocol_params.tau_num,
                tau_den: state.protocol_params.tau_den,
                d_num: 0,
                d_den: 1,
                protocol_version_major: state.protocol_params.protocol_version_major,
                protocol_version_minor: state.protocol_params.protocol_version_minor,
                min_utxo_value: 0,
                min_pool_cost: state.protocol_params.min_pool_cost,
            },
            gen_delegs: Vec::new(),
        }))
    }
}

/// Handle GetAccountState (tag 29) -- treasury + reserves.
pub(crate) fn handle_account_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetAccountState");
    QueryResult::AccountState {
        treasury: state.treasury,
        reserves: state.reserves,
    }
}

/// Handle DebugEpochState (tag 8) — epoch state summary.
///
/// In the Haskell node this returns the full serialized EpochState.
/// We return the key fields that tools typically inspect.
pub(crate) fn handle_debug_epoch_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: DebugEpochState");
    QueryResult::DebugEpochState {
        epoch: state.epoch.0,
        treasury: state.treasury,
        reserves: state.reserves,
        stake_pool_count: state.pool_count as u64,
        utxo_count: state.utxo_count as u64,
    }
}

/// Handle DebugNewEpochState (tag 12) — new epoch state summary.
///
/// In the Haskell node this returns the full serialized NewEpochState.
/// We return the key tracking fields.
pub(crate) fn handle_debug_new_epoch_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: DebugNewEpochState");
    let slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
    QueryResult::DebugNewEpochState {
        epoch: state.epoch.0,
        block_number: state.block_number.0,
        slot,
    }
}

/// Handle DebugChainDepState (tag 13) — consensus chain-dependent state.
///
/// In the Haskell node this returns the Praos ChainDepState (nonce, etc).
/// We return the last applied slot.
pub(crate) fn handle_debug_chain_dep_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: DebugChainDepState");
    let last_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
    QueryResult::DebugChainDepState { last_slot }
}

/// Handle GetRewardProvenance (tag 14) — reward calculation provenance.
///
/// Returns aggregate reward provenance data: total rewards pot, treasury tax,
/// and total active stake for the current epoch.
pub(crate) fn handle_reward_provenance(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetRewardProvenance");
    let total_active_stake: u64 = state.stake_pools.iter().map(|p| p.stake).sum();
    // Reward pot = reserves * rho (monetary expansion)
    let rho_num = state.protocol_params.rho_num;
    let rho_den = state.protocol_params.rho_den.max(1);
    let total_rewards_pot = (state.reserves as u128 * rho_num as u128 / rho_den as u128) as u64;
    // Treasury tax = reward_pot * tau
    let tau_num = state.protocol_params.tau_num;
    let tau_den = state.protocol_params.tau_den.max(1);
    let treasury_tax = (total_rewards_pot as u128 * tau_num as u128 / tau_den as u128) as u64;
    QueryResult::RewardProvenance {
        epoch: state.epoch.0,
        total_rewards_pot,
        treasury_tax,
        active_stake: total_active_stake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_handler::types::{
        NodeStateSnapshot, ProtocolParamsSnapshot, StakePoolSnapshot,
    };

    fn make_state() -> NodeStateSnapshot {
        NodeStateSnapshot {
            epoch: torsten_primitives::time::EpochNo(42),
            treasury: 1_000_000_000,
            reserves: 10_000_000_000,
            pool_count: 3,
            utxo_count: 5000,
            block_number: torsten_primitives::time::BlockNo(999),
            protocol_params: ProtocolParamsSnapshot {
                rho_num: 3,
                rho_den: 1000,
                tau_num: 2,
                tau_den: 10,
                ..ProtocolParamsSnapshot::default()
            },
            stake_pools: vec![
                StakePoolSnapshot {
                    pool_id: vec![1u8; 28],
                    stake: 500_000_000,
                    vrf_keyhash: vec![0u8; 32],
                    total_active_stake: 1_000_000_000,
                },
                StakePoolSnapshot {
                    pool_id: vec![2u8; 28],
                    stake: 500_000_000,
                    vrf_keyhash: vec![0u8; 32],
                    total_active_stake: 1_000_000_000,
                },
            ],
            ..NodeStateSnapshot::default()
        }
    }

    #[test]
    fn test_debug_epoch_state() {
        let state = make_state();
        let result = handle_debug_epoch_state(&state);
        match result {
            QueryResult::DebugEpochState {
                epoch,
                treasury,
                reserves,
                stake_pool_count,
                utxo_count,
            } => {
                assert_eq!(epoch, 42);
                assert_eq!(treasury, 1_000_000_000);
                assert_eq!(reserves, 10_000_000_000);
                assert_eq!(stake_pool_count, 3);
                assert_eq!(utxo_count, 5000);
            }
            _ => panic!("Expected DebugEpochState"),
        }
    }

    #[test]
    fn test_debug_new_epoch_state() {
        let state = make_state();
        let result = handle_debug_new_epoch_state(&state);
        match result {
            QueryResult::DebugNewEpochState {
                epoch,
                block_number,
                slot,
            } => {
                assert_eq!(epoch, 42);
                assert_eq!(block_number, 999);
                assert_eq!(slot, 0); // origin tip
            }
            _ => panic!("Expected DebugNewEpochState"),
        }
    }

    #[test]
    fn test_debug_chain_dep_state() {
        let state = make_state();
        let result = handle_debug_chain_dep_state(&state);
        match result {
            QueryResult::DebugChainDepState { last_slot } => {
                assert_eq!(last_slot, 0); // origin tip
            }
            _ => panic!("Expected DebugChainDepState"),
        }
    }

    #[test]
    fn test_reward_provenance() {
        let state = make_state();
        let result = handle_reward_provenance(&state);
        match result {
            QueryResult::RewardProvenance {
                epoch,
                total_rewards_pot,
                treasury_tax,
                active_stake,
            } => {
                assert_eq!(epoch, 42);
                // reserves=10B, rho=3/1000 => pot=30M
                assert_eq!(total_rewards_pot, 30_000_000);
                // pot=30M, tau=2/10 => tax=6M
                assert_eq!(treasury_tax, 6_000_000);
                assert_eq!(active_stake, 1_000_000_000);
            }
            _ => panic!("Expected RewardProvenance"),
        }
    }

    #[test]
    fn test_reward_provenance_zero_reserves() {
        let mut state = make_state();
        state.reserves = 0;
        let result = handle_reward_provenance(&state);
        match result {
            QueryResult::RewardProvenance {
                total_rewards_pot,
                treasury_tax,
                ..
            } => {
                assert_eq!(total_rewards_pot, 0);
                assert_eq!(treasury_tax, 0);
            }
            _ => panic!("Expected RewardProvenance"),
        }
    }
}
