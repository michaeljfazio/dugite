//! Stake and delegation query handlers (tags 10, 16, 17, 18, 19, 20, 21, 22).

use tracing::debug;

use super::filter::{filter_arg, read_credential, read_pool_id, OnEmptySet, SetArgShape};
use crate::node::n2c_query::types::{
    LedgerPeerEntry, NodeStateSnapshot, PoolRewardInfo, QueryResult,
};

/// Handle GetFilteredDelegationsAndRewardAccounts (tag 10).
///
/// Argument: tag(258) Set<Credential> where Credential = [0|1, hash(28)]
pub(crate) fn handle_filtered_delegations(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetFilteredDelegationsAndRewardAccounts");
    // `queryStakePoolDelegsAndRewards nes creds` is `accountsMap
    // \`Map.restrictKeys\` creds` with no `null` guard, so an empty set selects
    // nothing. dugite answered with every account until #963.
    let filter_creds = match filter_arg(
        decoder,
        "GetFilteredDelegationsAndRewardAccounts",
        SetArgShape::Required,
        OnEmptySet::NoItems,
        read_credential,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_creds {
        None => QueryResult::StakeAddressInfo(state.stake_addresses.clone()),
        Some(creds) => {
            // `StakeAddressSnapshot` does not carry the credential
            // discriminator, so the match is on the hash alone. Distinguishing
            // them would need a blake2b-224 collision between a key hash and a
            // script hash, which is a cryptographic break rather than a wire
            // case, but the discriminator is still *parsed* and validated so a
            // malformed credential cannot slip through as a bare hash.
            let filtered = state
                .stake_addresses
                .iter()
                .filter(|s| creds.iter().any(|(_, h)| h == &s.credential_hash))
                .cloned()
                .collect();
            QueryResult::StakeAddressInfo(filtered)
        }
    }
}

/// Handle GetStakePools (tag 16) -- returns Set<KeyHash StakePool>.
pub(crate) fn handle_stake_pools(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetStakePools");
    let pool_ids: Vec<Vec<u8>> = state
        .stake_pools
        .iter()
        .map(|p| p.pool_id.clone())
        .collect();
    QueryResult::StakePools(pool_ids)
}

/// Handle GetStakePoolParams (tag 17).
///
/// Argument: `Set (KeyHash StakePool)` — `tag(258)` + array, no `Maybe`.
///
/// An empty set selects **no** pools: `queryPoolParameters` is
/// `Map.restrictKeys pools poolKeys`, with no `null` guard
/// (cardano-ledger `Cardano/Ledger/Api/State/Query.hs`).
pub(crate) fn handle_stake_pool_params(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetStakePoolParams");
    let filter_pools = match filter_arg(
        decoder,
        "GetStakePoolParams",
        SetArgShape::Required,
        OnEmptySet::NoItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_pools {
        None => QueryResult::PoolParams(state.pool_params_entries.clone()),
        Some(ids) => {
            let filtered = state
                .pool_params_entries
                .iter()
                .filter(|p| ids.iter().any(|h| h == &p.pool_id))
                .cloned()
                .collect();
            QueryResult::PoolParams(filtered)
        }
    }
}

/// Handle GetPoolState (tag 19) -- returns QueryPoolStateResult.
///
/// Wire format: array(4) [poolParams_map, futurePoolParams_map, retiring_map, deposits_map]
///
/// Argument: `Maybe (Set (KeyHash StakePool))` — `array(0)` for `Nothing`,
/// `array(1) <set>` for `Just`.
///
/// `queryPoolState nes mPoolKeys` builds
/// `f = case mPoolKeys of Nothing -> id; Just keys -> (`Map.restrictKeys` keys)`
/// and applies that **one** `f` to every map in the result, so an empty `Just`
/// selects nothing (cardano-ledger `Cardano/Ledger/Api/State/Query.hs`).
pub(crate) fn handle_pool_state(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetPoolState");
    let filter_pools = match filter_arg(
        decoder,
        "GetPoolState",
        SetArgShape::Optional,
        OnEmptySet::NoItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };

    let pool_params = match &filter_pools {
        None => state.pool_params_entries.clone(),
        Some(ids) => state
            .pool_params_entries
            .iter()
            .filter(|p| ids.iter().any(|h| h == &p.pool_id))
            .cloned()
            .collect(),
    };

    // `mkQueryPoolStateResult` applies the same restriction to `psRetiring`.
    let retiring: Vec<(Vec<u8>, u64)> = state
        .pending_retirements
        .iter()
        .filter(|(pool_id, _)| match &filter_pools {
            None => true,
            Some(ids) => ids.iter().any(|h| h == pool_id),
        })
        .cloned()
        .collect();

    // Build deposits map: each registered pool has pool_deposit
    let deposits: Vec<(Vec<u8>, u64)> = pool_params
        .iter()
        .map(|p| (p.pool_id.clone(), state.pool_deposit))
        .collect();

    QueryResult::PoolState {
        pool_params,
        future_pool_params: Vec::new(), // No future params tracking yet
        retiring,
        deposits,
    }
}

/// Handle GetStakeDistribution2 (tag 37) — new PoolDistr format.
///
/// Returns: array(2)[pool_map, total_active_stake]
/// Each pool entry: array(3)[stake_rational, compact_lovelace, vrf_hash]
///
/// total_active_stake is the sum of ALL delegated stake including orphaned
/// delegations to retired pools — matching Haskell's PoolDistr total field.
/// Using stake_pools.iter().map(|p| p.stake).sum() would undercount because
/// stake_pools only contains active (non-retired) pools.
pub(crate) fn handle_stake_distribution2(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetStakeDistribution2");
    // Use the pre-computed total that includes orphaned delegations to retired pools.
    let total_active_stake = state.total_active_stake.max(1); // NonZero
    QueryResult::PoolDistr2 {
        pools: state.stake_pools.clone(),
        total_active_stake,
    }
}

/// Handle GetPoolDistr2 (tag 36) — filtered new PoolDistr format.
///
/// Argument: Maybe (tag(258) Set<KeyHash StakePool>)
///
/// total_active_stake is the denominator for the per-pool stake rational and
/// always reflects ALL delegated stake (including orphaned delegations to retired
/// pools), regardless of whether a pool filter was applied.  This matches the
/// Haskell PoolDistr total field which is global, not filtered.
pub(crate) fn handle_pool_distr2(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetPoolDistr2");
    let filter_pools = match filter_arg(
        decoder,
        "GetPoolDistr2",
        SetArgShape::Optional,
        OnEmptySet::NoItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    // Use the pre-computed total that includes orphaned delegations to retired pools.
    let total_active_stake = state.total_active_stake.max(1); // NonZero
    match filter_pools {
        None => QueryResult::PoolDistr2 {
            pools: state.stake_pools.clone(),
            total_active_stake,
        },
        Some(ids) => {
            let filtered: Vec<_> = state
                .stake_pools
                .iter()
                .filter(|p| ids.iter().any(|h| h == &p.pool_id))
                .cloned()
                .collect();
            QueryResult::PoolDistr2 {
                pools: filtered,
                total_active_stake,
            }
        }
    }
}

/// Handle GetSPOStakeDistr (tag 30) — filtered SPO stake distribution.
///
/// Argument: tag(258) Set<KeyHash StakePool>
/// Returns: Map<pool_hash(28), Coin> — SPO voting power per pool (lovelace).
///
/// NOTE: This is NOT the same as GetStakeDistribution (tag 5) which uses
/// IndividualPoolStake (rational + VRF hash). GetSPOStakeDistr returns a plain
/// map from pool key hash to absolute stake in lovelace, used for governance
/// vote tallying.
pub(crate) fn handle_spo_stake_distr(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetSPOStakeDistr");
    // `querySPOStakeDistr nes keys | null keys = <every pool>` — this query does
    // carry the explicit `null` guard, so unlike GetStakePoolParams (tag 17) an
    // empty set here means everything.
    let filter_pools = match filter_arg(
        decoder,
        "GetSPOStakeDistr",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    let entries: Vec<(Vec<u8>, u64)> = match filter_pools {
        None => state
            .stake_pools
            .iter()
            .map(|p| (p.pool_id.clone(), p.stake))
            .collect(),
        Some(ids) => state
            .stake_pools
            .iter()
            .filter(|p| ids.iter().any(|h| h == &p.pool_id))
            .map(|p| (p.pool_id.clone(), p.stake))
            .collect(),
    };
    QueryResult::SPOStakeDistr(entries)
}

/// Handle GetStakeSnapshots (tag 20).
///
/// Argument: `Maybe (Set (KeyHash StakePool))` — `array(0)` for `Nothing`,
/// `array(1) <set>` for `Just`.
///
/// `queryStakeSnapshots nes mPoolIds` picks the pool set as
/// `Nothing -> <every pool with stake across mark/set/go>; Just ids -> ids`,
/// so an empty `Just` yields an empty map (cardano-ledger
/// `Cardano/Ledger/Api/State/Query.hs`).
///
/// Before the fix for issue #406 the argument was ignored entirely and the
/// response always contained every pool. This both violated the wire protocol
/// (trailing CBOR bytes in the decoder) and broke compatibility with
/// `cardano-cli query stake-snapshot --stake-pool-id <id>`. #963 then found the
/// filter still inert on the wire, because the `Maybe` wrapper was unhandled.
pub(crate) fn handle_stake_snapshots(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetStakeSnapshots");
    let filter_pools = match filter_arg(
        decoder,
        "GetStakeSnapshots",
        SetArgShape::Optional,
        OnEmptySet::NoItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    let snapshots = &state.stake_snapshots;
    match filter_pools {
        None => QueryResult::StakeSnapshots(snapshots.clone()),
        Some(ids) => {
            let filtered_pools = snapshots
                .pools
                .iter()
                .filter(|p| ids.iter().any(|h| h == &p.pool_id))
                .cloned()
                .collect();
            // Totals are global — matching Haskell's `StakeSnapshots` record, where
            // `ssMarkTotal` / `ssSetTotal` / `ssGoTotal` are the total active stake
            // across the whole epoch snapshot regardless of any pool filter.
            QueryResult::StakeSnapshots(crate::node::n2c_query::types::StakeSnapshotsResult {
                pools: filtered_pools,
                total_mark_stake: snapshots.total_mark_stake,
                total_set_stake: snapshots.total_set_stake,
                total_go_stake: snapshots.total_go_stake,
            })
        }
    }
}

/// Handle GetPoolDistr (tag 21) -- returns pool stake distribution.
///
/// Argument: `Maybe (Set (KeyHash StakePool))`. Upstream answers this one by
/// delegating straight to `GetPoolDistr2` with the same argument
/// (`fromLedgerPoolDistr $ answerPureBlockQuery cfg (GetPoolDistr2 mPoolIds)`),
/// whose filter is `maybe (const True) (flip Set.member) mPoolIds` — so an
/// empty `Just` selects nothing.
pub(crate) fn handle_pool_distr(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetPoolDistr");
    let filter_pools = match filter_arg(
        decoder,
        "GetPoolDistr",
        SetArgShape::Optional,
        OnEmptySet::NoItems,
        read_pool_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_pools {
        None => QueryResult::PoolDistr(state.stake_pools.clone()),
        Some(ids) => {
            let filtered = state
                .stake_pools
                .iter()
                .filter(|p| ids.iter().any(|h| h == &p.pool_id))
                .cloned()
                .collect();
            QueryResult::PoolDistr(filtered)
        }
    }
}

/// Handle GetStakeDelegDeposits (tag 22).
///
/// Argument: tag(258) Set<Credential>
/// Returns: Map<Credential, Coin> -- deposit amount per registered stake credential
pub(crate) fn handle_stake_deleg_deposits(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetStakeDelegDeposits");
    // Upstream answers this one with `Set.foldl\' lookupInsert Map.empty
    // stakeCreds` — it iterates the *requested* set, so an empty set yields an
    // empty map. dugite answered with every deposit until #963.
    let filter_creds = match filter_arg(
        decoder,
        "GetStakeDelegDeposits",
        SetArgShape::Required,
        OnEmptySet::NoItems,
        read_credential,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_creds {
        None => QueryResult::StakeDelegDeposits(state.stake_deleg_deposits.clone()),
        Some(creds) => {
            let filtered = state
                .stake_deleg_deposits
                .iter()
                .filter(|d| {
                    creds
                        .iter()
                        .any(|(k, h)| *k == d.credential_type && h == &d.credential_hash)
                })
                .cloned()
                .collect();
            QueryResult::StakeDelegDeposits(filtered)
        }
    }
}

/// Handle GetRewardInfoPools (tag 18) — per-pool reward provenance data.
///
/// Returns estimated reward breakdown for each active pool: leader/member rewards,
/// margin, cost, and stake.
///
/// total_active_stake is the denominator for the per-pool stake fraction and must
/// include orphaned delegations to retired pools, matching Haskell semantics.
pub(crate) fn handle_reward_info_pools(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetRewardInfoPools");
    // Use the pre-computed total that includes orphaned delegations to retired pools.
    let total_active_stake = state.total_active_stake;
    // Compute reward pot from reserves * rho
    let rho_num = state.protocol_params.rho_num;
    let rho_den = state.protocol_params.rho_den.max(1);
    let total_rewards_pot = (state.reserves as u128 * rho_num as u128 / rho_den as u128) as u64;
    // Treasury tax
    let tau_num = state.protocol_params.tau_num;
    let tau_den = state.protocol_params.tau_den.max(1);
    let treasury_tax = (total_rewards_pot as u128 * tau_num as u128 / tau_den as u128) as u64;
    let distributable = total_rewards_pot.saturating_sub(treasury_tax);

    // Build pool params lookup for cost/margin
    let pool_params_map: std::collections::HashMap<
        &[u8],
        &crate::node::n2c_query::types::PoolParamsSnapshot,
    > = state
        .pool_params_entries
        .iter()
        .map(|pp| (pp.pool_id.as_slice(), pp))
        .collect();

    let mut entries = Vec::new();
    for pool in &state.stake_pools {
        if pool.stake == 0 || total_active_stake == 0 {
            continue;
        }
        let pool_reward =
            (pool.stake as u128 * distributable as u128 / total_active_stake as u128) as u64;
        let (cost, margin_num, margin_den, owner_stake) =
            if let Some(pp) = pool_params_map.get(pool.pool_id.as_slice()) {
                let os: u64 = pp
                    .owners
                    .iter()
                    .filter_map(|owner_hash| {
                        state
                            .stake_addresses
                            .iter()
                            .find(|sa| sa.credential_hash == *owner_hash)
                            .and_then(|sa| {
                                sa.delegated_pool
                                    .as_ref()
                                    .filter(|dp| dp.as_slice() == pool.pool_id.as_slice())
                                    .map(|_| sa.reward_balance)
                            })
                    })
                    .sum();
                (pp.cost, pp.margin_num, pp.margin_den, os)
            } else {
                (340_000_000, 0u64, 1u64, 0u64)
            };
        let after_cost = pool_reward.saturating_sub(cost);
        let margin_take =
            (after_cost as u128 * margin_num as u128 / margin_den.max(1) as u128) as u64;
        // Leader gets cost + margin; cap at pool_reward to prevent overflow
        let leader_reward = (cost + margin_take).min(pool_reward);
        let member_reward = pool_reward.saturating_sub(leader_reward);
        entries.push(PoolRewardInfo {
            pool_id: pool.pool_id.clone(),
            stake: pool.stake,
            owner_stake,
            pool_reward,
            leader_reward,
            member_reward,
            margin: (margin_num, margin_den),
            cost,
        });
    }
    QueryResult::RewardInfoPools(entries)
}

/// Handle QueryStakePoolDefaultVote (tag 35) — single pool default vote.
///
/// Per CIP-1694, the default vote depends on the pool operator's DRep delegation:
/// - AlwaysAbstain (drep_type=2) → DefaultAbstain = 1
/// - AlwaysNoConfidence (drep_type=3) → DefaultNoConfidence = 2
/// - Specific DRep (drep_type=0|1) → DefaultNo = 0
/// - No delegation → DefaultNo = 0
///
/// Argument: single KeyHash StakePool (28 bytes) — NOT a Set
/// Returns: bare word8 (DefaultVote)
pub(crate) fn handle_pool_default_vote(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: QueryStakePoolDefaultVote");

    // Parse single pool hash (28 bytes), NOT a Set
    let pool_hash = decoder.bytes().map(|b| b.to_vec()).unwrap_or_default();

    // Build lookup: owner credential hash → DRep delegation type
    let vote_deleg_map: std::collections::HashMap<&[u8], u8> = state
        .vote_delegatees
        .iter()
        .map(|v| (v.credential_hash.as_slice(), v.drep_type))
        .collect();

    // Find the pool params for the requested pool
    let default_vote = state
        .pool_params_entries
        .iter()
        .find(|pp| pp.pool_id == pool_hash)
        .map(|pp| {
            // Check if any pool owner has a vote delegation
            pp.owners
                .iter()
                .find_map(|owner| vote_deleg_map.get(owner.as_slice()))
                .map(|drep_type| match drep_type {
                    // Haskell DefaultVote encoding:
                    // 0 = DefaultNo, 1 = DefaultAbstain, 2 = DefaultNoConfidence
                    2 => 1, // AlwaysAbstain → DefaultAbstain
                    3 => 2, // AlwaysNoConfidence → DefaultNoConfidence
                    _ => 0, // Specific DRep or other → DefaultNo
                })
                .unwrap_or(0) // No delegation → DefaultNo
        })
        .unwrap_or(0); // Pool not found → DefaultNo

    QueryResult::StakePoolDefaultVote(default_vote)
}

/// Handle GetLedgerPeerSnapshot (tag 34) — relay peers from pool registrations.
///
/// Builds a snapshot of pool relay addresses weighted by stake for peer discovery.
/// Returns: array(2) [version, peers_list]
pub(crate) fn handle_ledger_peer_snapshot(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetLedgerPeerSnapshot");
    // Build a stake lookup from stake_pools
    let stake_map: std::collections::HashMap<&[u8], u64> = state
        .stake_pools
        .iter()
        .map(|p| (p.pool_id.as_slice(), p.stake))
        .collect();

    let entries: Vec<LedgerPeerEntry> = state
        .pool_params_entries
        .iter()
        .filter(|pp| !pp.relays.is_empty())
        .map(|pp| LedgerPeerEntry {
            pool_id: pp.pool_id.clone(),
            stake: stake_map.get(pp.pool_id.as_slice()).copied().unwrap_or(0),
            relays: pp.relays.clone(),
        })
        .collect();

    QueryResult::LedgerPeerSnapshot(entries)
}

/// Handle GetLedgerPeerSnapshot (tag 34) for N2C V23+.
///
/// `big = true`  → `LedgerBigPeerSnapshotV23` (outer discriminator `uint(2)`);
/// `big = false` → `LedgerAllPeerSnapshotV23` (outer discriminator `uint(3)`).
///
/// Both variants prepend `Point RawBlockHash` and `NetworkMagic` to the pool
/// list. The Big variant retains the per-pool `AccPoolStake`; the All variant
/// omits it.
pub(crate) fn handle_ledger_peer_snapshot_v23(state: &NodeStateSnapshot, big: bool) -> QueryResult {
    debug!(big, "Query: GetLedgerPeerSnapshot (V23+)");
    let stake_map: std::collections::HashMap<&[u8], u64> = state
        .stake_pools
        .iter()
        .map(|p| (p.pool_id.as_slice(), p.stake))
        .collect();

    let peers: Vec<LedgerPeerEntry> = state
        .pool_params_entries
        .iter()
        .filter(|pp| !pp.relays.is_empty())
        .map(|pp| LedgerPeerEntry {
            pool_id: pp.pool_id.clone(),
            stake: stake_map.get(pp.pool_id.as_slice()).copied().unwrap_or(0),
            relays: pp.relays.clone(),
        })
        .collect();

    QueryResult::LedgerPeerSnapshotV23 {
        big,
        anchor: state.tip.point.clone(),
        network_magic: state.network_magic,
        peers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::n2c_query::types::{
        NodeStateSnapshot, PoolParamsSnapshot, ProtocolParamsSnapshot, StakePoolSnapshot,
    };

    fn make_state_with_pools() -> NodeStateSnapshot {
        // total_active_stake must include ALL delegated stake (including orphaned
        // delegations to retired pools).  In this fixture there are no retired pools,
        // so it equals the sum of the two active pool stakes: 600M + 400M = 1B.
        // The NodeStateSnapshot.total_active_stake field drives GetStakeDistribution2,
        // GetPoolDistr2, and GetRewardInfoPools — it must be set explicitly here.
        let total_active_stake = 1_000_000_000u64;
        NodeStateSnapshot {
            reserves: 10_000_000_000,
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
                    stake: 600_000_000,
                    vrf_keyhash: vec![0u8; 32],
                    total_active_stake,
                    total_circulation: 54_000_000_000_000_000,
                },
                StakePoolSnapshot {
                    pool_id: vec![2u8; 28],
                    stake: 400_000_000,
                    vrf_keyhash: vec![0u8; 32],
                    total_active_stake,
                    total_circulation: 54_000_000_000_000_000,
                },
            ],
            total_active_stake,
            pool_params_entries: vec![
                PoolParamsSnapshot {
                    pool_id: vec![1u8; 28],
                    vrf_keyhash: vec![0u8; 32],
                    pledge: 100_000_000,
                    cost: 340_000_000,
                    margin_num: 5,
                    margin_den: 100,
                    reward_account: vec![0u8; 29],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
                PoolParamsSnapshot {
                    pool_id: vec![2u8; 28],
                    vrf_keyhash: vec![0u8; 32],
                    pledge: 50_000_000,
                    cost: 170_000_000,
                    margin_num: 10,
                    margin_den: 100,
                    reward_account: vec![0u8; 29],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            ],
            ..NodeStateSnapshot::default()
        }
    }

    #[test]
    fn test_reward_info_pools_returns_all_pools() {
        let state = make_state_with_pools();
        let result = handle_reward_info_pools(&state);
        match result {
            QueryResult::RewardInfoPools(pools) => {
                assert_eq!(pools.len(), 2);
                // Pool 1 has 60% stake, pool 2 has 40%
                assert_eq!(pools[0].pool_id, vec![1u8; 28]);
                assert_eq!(pools[1].pool_id, vec![2u8; 28]);
                assert_eq!(pools[0].stake, 600_000_000);
                assert_eq!(pools[1].stake, 400_000_000);
                assert_eq!(pools[0].margin, (5, 100));
                assert_eq!(pools[1].margin, (10, 100));
                assert_eq!(pools[0].cost, 340_000_000);
                assert_eq!(pools[1].cost, 170_000_000);
            }
            _ => panic!("Expected RewardInfoPools"),
        }
    }

    #[test]
    fn test_reward_info_pools_reward_split() {
        let state = make_state_with_pools();
        let result = handle_reward_info_pools(&state);
        match result {
            QueryResult::RewardInfoPools(pools) => {
                for pool in &pools {
                    // leader_reward + member_reward = pool_reward
                    assert_eq!(
                        pool.leader_reward + pool.member_reward,
                        pool.pool_reward,
                        "leader + member should equal pool reward for pool {:?}",
                        pool.pool_id[0]
                    );
                    // pool_reward should be > 0
                    assert!(pool.pool_reward > 0);
                }
            }
            _ => panic!("Expected RewardInfoPools"),
        }
    }

    #[test]
    fn test_reward_info_pools_empty() {
        let state = NodeStateSnapshot::default();
        let result = handle_reward_info_pools(&state);
        match result {
            QueryResult::RewardInfoPools(pools) => {
                assert!(pools.is_empty());
            }
            _ => panic!("Expected RewardInfoPools"),
        }
    }

    #[test]
    fn test_reward_info_pools_zero_stake_pool_excluded() {
        let mut state = make_state_with_pools();
        // Set one pool's stake to 0
        state.stake_pools[1].stake = 0;
        let result = handle_reward_info_pools(&state);
        match result {
            QueryResult::RewardInfoPools(pools) => {
                assert_eq!(pools.len(), 1);
                assert_eq!(pools[0].pool_id, vec![1u8; 28]);
            }
            _ => panic!("Expected RewardInfoPools"),
        }
    }

    #[test]
    fn test_spo_stake_distr_no_filter() {
        let state = make_state_with_pools();
        // Empty CBOR: tag(258) + empty array
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_spo_stake_distr(&state, &mut dec);
        match result {
            QueryResult::SPOStakeDistr(entries) => {
                assert_eq!(entries.len(), 2);
                // Should be (pool_hash, stake_lovelace) pairs
                assert_eq!(entries[0].0, vec![1u8; 28]);
                assert_eq!(entries[0].1, 600_000_000);
                assert_eq!(entries[1].0, vec![2u8; 28]);
                assert_eq!(entries[1].1, 400_000_000);
            }
            _ => panic!("Expected SPOStakeDistr"),
        }
    }

    #[test]
    fn test_spo_stake_distr_filtered() {
        let state = make_state_with_pools();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.bytes(&[1u8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_spo_stake_distr(&state, &mut dec);
        match result {
            QueryResult::SPOStakeDistr(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, vec![1u8; 28]);
                assert_eq!(entries[0].1, 600_000_000);
            }
            _ => panic!("Expected SPOStakeDistr"),
        }
    }

    #[test]
    fn test_ledger_peer_snapshot_with_relays() {
        use crate::node::n2c_query::types::RelaySnapshot;
        let mut state = make_state_with_pools();
        state.pool_params_entries[0].relays = vec![RelaySnapshot::SingleHostName {
            port: Some(3001),
            dns_name: "relay1.example.com".to_string(),
        }];
        let result = handle_ledger_peer_snapshot(&state);
        match result {
            QueryResult::LedgerPeerSnapshot(peers) => {
                // Only pool 1 has relays
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].pool_id, vec![1u8; 28]);
                assert_eq!(peers[0].stake, 600_000_000);
                assert_eq!(peers[0].relays.len(), 1);
            }
            _ => panic!("Expected LedgerPeerSnapshot"),
        }
    }

    #[test]
    fn test_ledger_peer_snapshot_no_relays() {
        let state = make_state_with_pools();
        let result = handle_ledger_peer_snapshot(&state);
        match result {
            QueryResult::LedgerPeerSnapshot(peers) => {
                // No pools have relays in the default fixture
                assert!(peers.is_empty());
            }
            _ => panic!("Expected LedgerPeerSnapshot"),
        }
    }

    #[test]
    fn test_ledger_peer_snapshot_v23_big_carries_anchor_and_magic() {
        use crate::node::n2c_query::types::RelaySnapshot;
        use dugite_primitives::block::Point;
        let mut state = make_state_with_pools();
        state.network_magic = 2;
        state.tip.point = Point::Origin;
        state.pool_params_entries[0].relays = vec![RelaySnapshot::SingleHostName {
            port: Some(3001),
            dns_name: "relay1.example.com".to_string(),
        }];
        let result = handle_ledger_peer_snapshot_v23(&state, true);
        match result {
            QueryResult::LedgerPeerSnapshotV23 {
                big,
                anchor,
                network_magic,
                peers,
            } => {
                assert!(big);
                assert_eq!(network_magic, 2);
                assert!(matches!(anchor, Point::Origin));
                assert_eq!(peers.len(), 1);
            }
            _ => panic!("Expected LedgerPeerSnapshotV23"),
        }
    }

    #[test]
    fn test_ledger_peer_snapshot_v23_all_variant() {
        let state = make_state_with_pools();
        let result = handle_ledger_peer_snapshot_v23(&state, false);
        match result {
            QueryResult::LedgerPeerSnapshotV23 { big, .. } => {
                assert!(!big, "All variant must have big=false");
            }
            _ => panic!("Expected LedgerPeerSnapshotV23"),
        }
    }

    #[test]
    fn test_pool_default_vote_no_delegation_with_owners() {
        let mut state = make_state_with_pools();
        // Give pools owners but no vote delegatees
        state.pool_params_entries[0].owners = vec![vec![10u8; 28]];
        state.pool_params_entries[1].owners = vec![vec![20u8; 28]];
        // Query pool 1: no delegation → DefaultNo (0)
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.bytes(&[1u8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_default_vote(&state, &mut dec);
        match result {
            QueryResult::StakePoolDefaultVote(vote) => {
                assert_eq!(vote, 0, "No delegation → DefaultNo (0)");
            }
            _ => panic!("Expected StakePoolDefaultVote"),
        }
    }

    /// Haskell `Nothing` — `encodeMaybe Nothing = encodeListLen 0`.
    /// This is what `cardano-cli` sends when no `--stake-pool-id` is given, and
    /// the only encoding that means "every pool" for tags 19/20/21/36.
    fn make_nothing_filter_cbor() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(0).ok();
        buf
    }

    /// An explicitly **empty** `Set` — `tag(258) array(0)`.
    ///
    /// Not the same thing as `Nothing`: `Map.restrictKeys m mempty` is empty, so
    /// for every query whose Haskell handler restricts rather than guarding on
    /// `null`, this selects no pools at all.
    fn make_empty_set_filter_cbor() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(0).ok();
        buf
    }

    /// A bare `Set` of one pool id — `tag(258) array(1) bstr(28)`. This is the
    /// `toCBOR (Set …)` argument of tags 17 and 30.
    fn make_pool_filter_cbor(pool_id: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(1).ok();
        enc.bytes(pool_id).ok();
        buf
    }

    /// The **live** argument `cardano-cli` sends for tags 19/20/21/36:
    /// `toCBOR (Just (Set.singleton poolid))`
    ///   = `array(1)` (the `Maybe`) `tag(258) array(1) bstr(28)` (the `Set`).
    ///
    /// #963: dugite never handled the `array(1)` `Maybe` wrapper, so this exact
    /// shape decoded to an empty filter and every caller answered with *all*
    /// pools. The pre-existing tests all fed the bare-`Set` form and so could
    /// not see it.
    fn make_just_pools_filter_cbor(pool_ids: &[&[u8]]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).ok();
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(pool_ids.len() as u64).ok();
        for id in pool_ids {
            enc.bytes(id).ok();
        }
        buf
    }

    fn make_credential_filter_cbor(cred_hash: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(1).ok();
        enc.array(2).ok();
        enc.u8(0).ok(); // KeyHash
        enc.bytes(cred_hash).ok();
        buf
    }

    // ─── GetFilteredDelegations (tag 10) ──────────────────────────────

    #[test]
    fn test_filtered_delegations_no_filter() {
        use crate::node::n2c_query::types::StakeAddressSnapshot;
        let state = NodeStateSnapshot {
            stake_addresses: vec![
                StakeAddressSnapshot {
                    credential_hash: vec![0xAA; 28],
                    delegated_pool: Some(vec![1u8; 28]),
                    reward_balance: 1_000_000,
                },
                StakeAddressSnapshot {
                    credential_hash: vec![0xBB; 28],
                    delegated_pool: None,
                    reward_balance: 0,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        // `queryStakePoolDelegsAndRewards` is `restrictKeys creds` with no
        // `null` guard, so an explicitly empty set selects nothing. dugite
        // answered with every account until #963.
        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_filtered_delegations(&state, &mut dec) {
            QueryResult::StakeAddressInfo(addrs) => assert!(addrs.is_empty()),
            other => panic!("Expected StakeAddressInfo, got {other:?}"),
        }

        // No argument bytes at all is still tolerated as "no filter".
        let mut dec = minicbor::Decoder::new(&[]);
        match handle_filtered_delegations(&state, &mut dec) {
            QueryResult::StakeAddressInfo(addrs) => assert_eq!(addrs.len(), 2),
            other => panic!("Expected StakeAddressInfo, got {other:?}"),
        }

        // A malformed credential set must not degrade to "every account".
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).u32(4).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert!(matches!(
            handle_filtered_delegations(&state, &mut dec),
            QueryResult::Error(_)
        ));
    }

    #[test]
    fn test_filtered_delegations_filtered() {
        use crate::node::n2c_query::types::StakeAddressSnapshot;
        let state = NodeStateSnapshot {
            stake_addresses: vec![
                StakeAddressSnapshot {
                    credential_hash: vec![0xAA; 28],
                    delegated_pool: Some(vec![1u8; 28]),
                    reward_balance: 1_000_000,
                },
                StakeAddressSnapshot {
                    credential_hash: vec![0xBB; 28],
                    delegated_pool: None,
                    reward_balance: 0,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        let cbor = make_credential_filter_cbor(&[0xAA; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_filtered_delegations(&state, &mut dec);
        match result {
            QueryResult::StakeAddressInfo(addrs) => {
                assert_eq!(addrs.len(), 1);
                assert_eq!(addrs[0].credential_hash, vec![0xAA; 28]);
                assert_eq!(addrs[0].reward_balance, 1_000_000);
            }
            _ => panic!("Expected StakeAddressInfo"),
        }
    }

    // ─── GetStakePools (tag 16) ────────────────────────────────────────

    #[test]
    fn test_stake_pools() {
        let state = make_state_with_pools();
        let result = handle_stake_pools(&state);
        match result {
            QueryResult::StakePools(pool_ids) => {
                assert_eq!(pool_ids.len(), 2);
                assert_eq!(pool_ids[0], vec![1u8; 28]);
                assert_eq!(pool_ids[1], vec![2u8; 28]);
            }
            _ => panic!("Expected StakePools"),
        }
    }

    #[test]
    fn test_stake_pools_empty() {
        let state = NodeStateSnapshot::default();
        let result = handle_stake_pools(&state);
        match result {
            QueryResult::StakePools(pool_ids) => assert!(pool_ids.is_empty()),
            _ => panic!("Expected StakePools"),
        }
    }

    // ─── #963: the pool-id filter argument ──────────────────────────────
    //
    // The filter was not merely wrong, it was *inert*: `parse_pool_id_set`
    // degraded to an empty vector on every failure path and every caller read
    // an empty vector as "all pools", so asking for pool A returned A and B.
    // These tests drive the exact bytes `cardano-cli` emits.

    /// The live shape: `toCBOR (Just (Set.singleton poolid))`. A one-element
    /// filter must produce a one-element answer.
    #[test]
    fn test_pool_state_just_single_pool_filters_to_that_pool() {
        let mut state = make_state_with_pools();
        state.pending_retirements = vec![(vec![2u8; 28], 150)];
        state.pool_deposit = 500_000_000;
        let cbor = make_just_pools_filter_cbor(&[&[1u8; 28]]);
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_pool_state(&state, &mut dec) {
            QueryResult::PoolState {
                pool_params,
                retiring,
                deposits,
                ..
            } => {
                assert_eq!(pool_params.len(), 1, "asked for one pool, got a superset");
                assert_eq!(pool_params[0].pool_id, vec![1u8; 28]);
                // pool 2's pending retirement must not leak through either.
                assert!(retiring.is_empty());
                assert_eq!(deposits.len(), 1);
            }
            other => panic!("Expected PoolState, got {other:?}"),
        }
    }

    /// Same for tag 20 — the other query #963 reproduced on the wire.
    #[test]
    fn test_stake_snapshots_just_single_pool_filters_to_that_pool() {
        let state = make_stake_snapshots_state();
        let cbor = make_just_pools_filter_cbor(&[&[2u8; 28]]);
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_stake_snapshots(&state, &mut dec) {
            QueryResult::StakeSnapshots(ss) => {
                assert_eq!(ss.pools.len(), 1, "asked for one pool, got a superset");
                assert_eq!(ss.pools[0].pool_id, vec![2u8; 28]);
            }
            other => panic!("Expected StakeSnapshots, got {other:?}"),
        }
    }

    /// Asking for a *different* pool must produce a different answer. Under the
    /// #963 defect the two responses were byte-identical, which is what made
    /// the filter provably inert rather than merely mis-applied.
    #[test]
    fn test_pool_state_different_filters_give_different_answers() {
        let state = make_state_with_pools();

        let a = make_just_pools_filter_cbor(&[&[1u8; 28]]);
        let b = make_just_pools_filter_cbor(&[&[2u8; 28]]);
        let mut da = minicbor::Decoder::new(&a);
        let mut db = minicbor::Decoder::new(&b);

        let ids = |r: QueryResult| match r {
            QueryResult::PoolState { pool_params, .. } => pool_params
                .into_iter()
                .map(|p| p.pool_id)
                .collect::<Vec<_>>(),
            other => panic!("Expected PoolState, got {other:?}"),
        };

        assert_eq!(ids(handle_pool_state(&state, &mut da)), vec![vec![1u8; 28]]);
        assert_eq!(ids(handle_pool_state(&state, &mut db)), vec![vec![2u8; 28]]);
    }

    /// Every one of the four `Maybe`-carrying tags takes the same wrapper.
    #[test]
    fn test_all_optional_arg_queries_accept_the_maybe_wrapper() {
        let state = make_state_with_pools();
        let just_one = make_just_pools_filter_cbor(&[&[1u8; 28]]);
        let nothing = make_nothing_filter_cbor();

        let mut d = minicbor::Decoder::new(&just_one);
        match handle_pool_distr(&state, &mut d) {
            QueryResult::PoolDistr(p) => assert_eq!(p.len(), 1),
            other => panic!("tag 21: {other:?}"),
        }
        let mut d = minicbor::Decoder::new(&just_one);
        match handle_pool_distr2(&state, &mut d) {
            QueryResult::PoolDistr2 { pools, .. } => assert_eq!(pools.len(), 1),
            other => panic!("tag 36: {other:?}"),
        }

        let mut d = minicbor::Decoder::new(&nothing);
        match handle_pool_distr(&state, &mut d) {
            QueryResult::PoolDistr(p) => assert_eq!(p.len(), 2),
            other => panic!("tag 21 Nothing: {other:?}"),
        }
        let mut d = minicbor::Decoder::new(&nothing);
        match handle_pool_distr2(&state, &mut d) {
            QueryResult::PoolDistr2 { pools, .. } => assert_eq!(pools.len(), 2),
            other => panic!("tag 36 Nothing: {other:?}"),
        }
    }

    /// The multi-element `Just` form.
    #[test]
    fn test_just_multiple_pools_filters_to_exactly_those() {
        let state = make_state_with_pools();
        let cbor = make_just_pools_filter_cbor(&[&[1u8; 28], &[2u8; 28]]);
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_pool_state(&state, &mut dec) {
            QueryResult::PoolState { pool_params, .. } => assert_eq!(pool_params.len(), 2),
            other => panic!("Expected PoolState, got {other:?}"),
        }
    }

    /// `encodeContainerSkel` always writes a definite array, but #938 settled
    /// that dugite reads both framings wherever upstream might produce either.
    #[test]
    fn test_indefinite_length_set_is_accepted() {
        let state = make_state_with_pools();
        let mut cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cbor);
        enc.array(1).unwrap(); // Just
        enc.tag(minicbor::data::Tag::new(258)).unwrap();
        enc.begin_array().unwrap();
        enc.bytes(&[1u8; 28]).unwrap();
        enc.end().unwrap();

        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_pool_state(&state, &mut dec) {
            QueryResult::PoolState { pool_params, .. } => {
                assert_eq!(pool_params.len(), 1);
                assert_eq!(pool_params[0].pool_id, vec![1u8; 28]);
            }
            other => panic!("Expected PoolState, got {other:?}"),
        }
    }

    /// A malformed argument must answer with an error, never with a superset.
    /// This is the actual severity of #963: the filter failed **open**, so a
    /// parse failure was indistinguishable from "give me everything".
    #[test]
    fn test_malformed_filter_argument_errors_rather_than_returning_all_pools() {
        let state = make_state_with_pools();

        // A bare integer where the argument should be.
        let mut cbor = Vec::new();
        minicbor::Encoder::new(&mut cbor).u32(7).unwrap();
        let mut dec = minicbor::Decoder::new(&cbor);
        assert!(
            matches!(handle_pool_state(&state, &mut dec), QueryResult::Error(_)),
            "malformed argument must not answer with every pool"
        );

        // A `Maybe` wrapper of an impossible length.
        let mut cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cbor);
        enc.array(3).unwrap();
        enc.u32(1).unwrap();
        enc.u32(2).unwrap();
        enc.u32(3).unwrap();
        let mut dec = minicbor::Decoder::new(&cbor);
        assert!(matches!(
            handle_stake_snapshots(&state, &mut dec),
            QueryResult::Error(_)
        ));

        // The wrong set tag.
        let mut cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cbor);
        enc.array(1).unwrap();
        enc.tag(minicbor::data::Tag::new(259)).unwrap();
        enc.array(1).unwrap();
        enc.bytes(&[1u8; 28]).unwrap();
        let mut dec = minicbor::Decoder::new(&cbor);
        assert!(matches!(
            handle_pool_distr(&state, &mut dec),
            QueryResult::Error(_)
        ));

        // A set element that is not a 28-byte pool key hash. Silently keeping a
        // hash that can never match would report "no such pool" for a pool that
        // exists — the same quiet wrong answer in the other direction.
        let mut cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cbor);
        enc.array(1).unwrap();
        enc.tag(minicbor::data::Tag::new(258)).unwrap();
        enc.array(1).unwrap();
        enc.bytes(&[1u8; 32]).unwrap();
        let mut dec = minicbor::Decoder::new(&cbor);
        assert!(matches!(
            handle_pool_distr2(&state, &mut dec),
            QueryResult::Error(_)
        ));
    }

    /// tag 30 is the one pool-id query whose Haskell handler *does* guard on
    /// `null keys`, so its empty set means everything. Pinned so the two rules
    /// cannot be collapsed into one by a later refactor.
    #[test]
    fn test_spo_stake_distr_empty_set_means_all_pools_unlike_tag_17() {
        let state = make_state_with_pools();

        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_spo_stake_distr(&state, &mut dec) {
            QueryResult::SPOStakeDistr(e) => {
                assert_eq!(e.len(), 2, "querySPOStakeDistr: null keys")
            }
            other => panic!("Expected SPOStakeDistr, got {other:?}"),
        }

        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_stake_pool_params(&state, &mut dec) {
            QueryResult::PoolParams(p) => {
                assert!(p.is_empty(), "queryPoolParameters: restrictKeys")
            }
            other => panic!("Expected PoolParams, got {other:?}"),
        }
    }

    // ─── GetStakePoolParams (tag 17) ──────────────────────────────────

    /// `queryPoolParameters` is `Map.restrictKeys pools poolKeys` with no `null`
    /// guard, so an empty set selects nothing. dugite answered with every pool
    /// until #963.
    #[test]
    fn test_stake_pool_params_empty_set_selects_no_pools() {
        let state = make_state_with_pools();
        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_pool_params(&state, &mut dec);
        match result {
            QueryResult::PoolParams(params) => assert!(params.is_empty()),
            _ => panic!("Expected PoolParams"),
        }
    }

    /// Tag 17 has no `Maybe`, so "no argument at all" is not a shape any client
    /// sends; dugite tolerates it as "no filter" rather than erroring.
    #[test]
    fn test_stake_pool_params_absent_argument_returns_all_pools() {
        let state = make_state_with_pools();
        let mut dec = minicbor::Decoder::new(&[]);
        let result = handle_stake_pool_params(&state, &mut dec);
        match result {
            QueryResult::PoolParams(params) => assert_eq!(params.len(), 2),
            _ => panic!("Expected PoolParams"),
        }
    }

    #[test]
    fn test_stake_pool_params_filtered() {
        let state = make_state_with_pools();
        let cbor = make_pool_filter_cbor(&[1u8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_pool_params(&state, &mut dec);
        match result {
            QueryResult::PoolParams(params) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].pool_id, vec![1u8; 28]);
                assert_eq!(params[0].cost, 340_000_000);
            }
            _ => panic!("Expected PoolParams"),
        }
    }

    // ─── GetPoolState (tag 19) ──────────────────────────────────────────

    #[test]
    fn test_pool_state_no_filter() {
        let mut state = make_state_with_pools();
        state.pending_retirements = vec![(vec![2u8; 28], 150)];
        state.pool_deposit = 500_000_000;
        let cbor = make_nothing_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_state(&state, &mut dec);
        match result {
            QueryResult::PoolState {
                pool_params,
                future_pool_params,
                retiring,
                deposits,
            } => {
                assert_eq!(pool_params.len(), 2);
                assert!(future_pool_params.is_empty());
                assert_eq!(retiring.len(), 1);
                assert_eq!(retiring[0].0, vec![2u8; 28]);
                assert_eq!(retiring[0].1, 150);
                assert_eq!(deposits.len(), 2);
                assert!(deposits.iter().all(|(_, d)| *d == 500_000_000));
            }
            _ => panic!("Expected PoolState"),
        }
    }

    #[test]
    fn test_pool_state_filtered() {
        let mut state = make_state_with_pools();
        state.pending_retirements = vec![(vec![1u8; 28], 150), (vec![2u8; 28], 150)];
        let cbor = make_pool_filter_cbor(&[1u8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_state(&state, &mut dec);
        match result {
            QueryResult::PoolState {
                pool_params,
                retiring,
                deposits,
                ..
            } => {
                assert_eq!(pool_params.len(), 1);
                assert_eq!(pool_params[0].pool_id, vec![1u8; 28]);
                // Only pool 1 retirement should be included
                assert_eq!(retiring.len(), 1);
                assert_eq!(retiring[0].0, vec![1u8; 28]);
                assert_eq!(deposits.len(), 1);
            }
            _ => panic!("Expected PoolState"),
        }
    }

    // ─── GetStakeSnapshots (tag 20) ────────────────────────────────────

    fn make_stake_snapshots_state() -> NodeStateSnapshot {
        use crate::node::n2c_query::types::{PoolStakeSnapshotEntry, StakeSnapshotsResult};
        NodeStateSnapshot {
            stake_snapshots: StakeSnapshotsResult {
                pools: vec![
                    PoolStakeSnapshotEntry {
                        pool_id: vec![1u8; 28],
                        mark_stake: 100,
                        set_stake: 200,
                        go_stake: 300,
                    },
                    PoolStakeSnapshotEntry {
                        pool_id: vec![2u8; 28],
                        mark_stake: 1_000,
                        set_stake: 2_000,
                        go_stake: 3_000,
                    },
                ],
                total_mark_stake: 10_100,
                total_set_stake: 20_200,
                total_go_stake: 30_300,
            },
            ..NodeStateSnapshot::default()
        }
    }

    /// `Nothing` — every pool.
    #[test]
    fn test_stake_snapshots_nothing_returns_all_pools() {
        let state = make_stake_snapshots_state();
        let cbor = make_nothing_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_snapshots(&state, &mut dec);
        match result {
            QueryResult::StakeSnapshots(ss) => {
                assert_eq!(ss.pools.len(), 2);
                assert_eq!(ss.total_mark_stake, 10_100);
                assert_eq!(ss.total_set_stake, 20_200);
                assert_eq!(ss.total_go_stake, 30_300);
            }
            _ => panic!("Expected StakeSnapshots"),
        }
    }

    /// `Just mempty` — no pools. `queryStakeSnapshots` takes `poolIds = ids`
    /// straight from the `Just`, and `Map.fromSet f mempty` is empty. The
    /// totals stay global either way.
    #[test]
    fn test_stake_snapshots_empty_set_selects_no_pools() {
        let state = make_stake_snapshots_state();
        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_snapshots(&state, &mut dec);
        match result {
            QueryResult::StakeSnapshots(ss) => {
                assert!(ss.pools.is_empty());
                assert_eq!(ss.total_mark_stake, 10_100);
            }
            _ => panic!("Expected StakeSnapshots"),
        }
    }

    #[test]
    fn test_stake_snapshots_filter_single_pool() {
        let state = make_stake_snapshots_state();
        let cbor = make_pool_filter_cbor(&[2u8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_snapshots(&state, &mut dec);
        match result {
            QueryResult::StakeSnapshots(ss) => {
                assert_eq!(ss.pools.len(), 1);
                assert_eq!(ss.pools[0].pool_id, vec![2u8; 28]);
                assert_eq!(ss.pools[0].mark_stake, 1_000);
                // Totals are global even when filtered (matches Haskell semantics)
                assert_eq!(ss.total_mark_stake, 10_100);
                assert_eq!(ss.total_set_stake, 20_200);
                assert_eq!(ss.total_go_stake, 30_300);
            }
            _ => panic!("Expected StakeSnapshots"),
        }
    }

    /// Feed the full Shelley inner query body `[20, tag(258) [hash1, hash2]]`
    /// through the same parsing the dispatcher would apply — verifies that the
    /// argument is consumed correctly and filtering hits the expected pool set.
    #[test]
    fn test_stake_snapshots_parses_tagged_set_from_query_body() {
        let state = make_stake_snapshots_state();

        // Build the inner Shelley query body: [20, tag(258) [p1, p2]]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(20).unwrap();
        enc.tag(minicbor::data::Tag::new(258)).unwrap();
        enc.array(2).unwrap();
        enc.bytes(&[1u8; 28]).unwrap();
        enc.bytes(&[2u8; 28]).unwrap();

        // Dispatcher would peel the outer array and read the tag u32 first.
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 20);
        // Handler receives the decoder positioned at the argument set.
        let result = handle_stake_snapshots(&state, &mut dec);
        match result {
            QueryResult::StakeSnapshots(ss) => {
                assert_eq!(ss.pools.len(), 2);
                assert_eq!(ss.pools[0].pool_id, vec![1u8; 28]);
                assert_eq!(ss.pools[1].pool_id, vec![2u8; 28]);
            }
            _ => panic!("Expected StakeSnapshots"),
        }
    }

    #[test]
    fn test_stake_snapshots_filter_unknown_pool_returns_empty_list() {
        let state = make_stake_snapshots_state();
        let cbor = make_pool_filter_cbor(&[0xFFu8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_snapshots(&state, &mut dec);
        match result {
            QueryResult::StakeSnapshots(ss) => {
                assert!(ss.pools.is_empty());
                // Totals remain global
                assert_eq!(ss.total_mark_stake, 10_100);
            }
            _ => panic!("Expected StakeSnapshots"),
        }
    }

    // ─── GetPoolDistr (tag 21) ──────────────────────────────────────────

    #[test]
    fn test_pool_distr_no_filter() {
        let state = make_state_with_pools();
        let cbor = make_nothing_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_distr(&state, &mut dec);
        match result {
            QueryResult::PoolDistr(pools) => assert_eq!(pools.len(), 2),
            _ => panic!("Expected PoolDistr"),
        }
    }

    #[test]
    fn test_pool_distr_filtered() {
        let state = make_state_with_pools();
        let cbor = make_pool_filter_cbor(&[2u8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_distr(&state, &mut dec);
        match result {
            QueryResult::PoolDistr(pools) => {
                assert_eq!(pools.len(), 1);
                assert_eq!(pools[0].pool_id, vec![2u8; 28]);
            }
            _ => panic!("Expected PoolDistr"),
        }
    }

    // ─── GetStakeDelegDeposits (tag 22) ─────────────────────────────────

    #[test]
    fn test_stake_deleg_deposits_no_filter() {
        use crate::node::n2c_query::types::StakeDelegDepositEntry;
        let state = NodeStateSnapshot {
            stake_deleg_deposits: vec![
                StakeDelegDepositEntry {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    deposit: 2_000_000,
                },
                StakeDelegDepositEntry {
                    credential_hash: vec![0xBB; 28],
                    credential_type: 1,
                    deposit: 2_000_000,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        // Upstream iterates the *requested* set (`Set.foldl' lookupInsert
        // Map.empty stakeCreds`), so an empty set yields an empty map.
        let cbor = make_empty_set_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        match handle_stake_deleg_deposits(&state, &mut dec) {
            QueryResult::StakeDelegDeposits(deps) => assert!(deps.is_empty()),
            other => panic!("Expected StakeDelegDeposits, got {other:?}"),
        }

        let mut dec = minicbor::Decoder::new(&[]);
        match handle_stake_deleg_deposits(&state, &mut dec) {
            QueryResult::StakeDelegDeposits(deps) => assert_eq!(deps.len(), 2),
            other => panic!("Expected StakeDelegDeposits, got {other:?}"),
        }
    }

    #[test]
    fn test_stake_deleg_deposits_filtered() {
        use crate::node::n2c_query::types::StakeDelegDepositEntry;
        let state = NodeStateSnapshot {
            stake_deleg_deposits: vec![
                StakeDelegDepositEntry {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    deposit: 2_000_000,
                },
                StakeDelegDepositEntry {
                    credential_hash: vec![0xBB; 28],
                    credential_type: 0,
                    deposit: 2_000_000,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        let cbor = make_credential_filter_cbor(&[0xBB; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_stake_deleg_deposits(&state, &mut dec);
        match result {
            QueryResult::StakeDelegDeposits(deps) => {
                assert_eq!(deps.len(), 1);
                assert_eq!(deps[0].credential_hash, vec![0xBB; 28]);
            }
            _ => panic!("Expected StakeDelegDeposits"),
        }
    }

    // ─── GetStakeDistribution2 (tag 37) / GetPoolDistr2 (tag 36) ──────

    #[test]
    fn test_stake_distribution2() {
        let state = make_state_with_pools();
        let result = handle_stake_distribution2(&state);
        match result {
            QueryResult::PoolDistr2 {
                pools,
                total_active_stake,
            } => {
                assert_eq!(pools.len(), 2);
                assert_eq!(total_active_stake, 1_000_000_000);
            }
            _ => panic!("Expected PoolDistr2"),
        }
    }

    #[test]
    fn test_stake_distribution2_empty() {
        let state = NodeStateSnapshot::default();
        let result = handle_stake_distribution2(&state);
        match result {
            QueryResult::PoolDistr2 {
                pools,
                total_active_stake,
            } => {
                assert!(pools.is_empty());
                assert_eq!(total_active_stake, 1); // NonZero
            }
            _ => panic!("Expected PoolDistr2"),
        }
    }

    #[test]
    fn test_pool_distr2_no_filter() {
        let state = make_state_with_pools();
        let cbor = make_nothing_filter_cbor();
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_distr2(&state, &mut dec);
        match result {
            QueryResult::PoolDistr2 {
                pools,
                total_active_stake,
            } => {
                assert_eq!(pools.len(), 2);
                assert_eq!(total_active_stake, 1_000_000_000);
            }
            _ => panic!("Expected PoolDistr2"),
        }
    }

    #[test]
    fn test_pool_distr2_filtered() {
        let state = make_state_with_pools();
        let cbor = make_pool_filter_cbor(&[2u8; 28]);
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_distr2(&state, &mut dec);
        match result {
            QueryResult::PoolDistr2 {
                pools,
                total_active_stake,
            } => {
                assert_eq!(pools.len(), 1);
                assert_eq!(pools[0].pool_id, vec![2u8; 28]);
                // total_active_stake is sum of ALL pools, not filtered
                assert_eq!(total_active_stake, 1_000_000_000);
            }
            _ => panic!("Expected PoolDistr2"),
        }
    }

    #[test]
    fn test_pool_default_vote_with_delegations() {
        use crate::node::n2c_query::types::VoteDelegateeEntry;
        let mut state = make_state_with_pools();
        state.pool_params_entries[0].owners = vec![vec![10u8; 28]];
        state.pool_params_entries[1].owners = vec![vec![20u8; 28]];
        // Owner of pool 1 delegates to AlwaysNoConfidence (type 3)
        // Owner of pool 2 delegates to a specific DRep (type 0)
        state.vote_delegatees = vec![
            VoteDelegateeEntry {
                credential_hash: vec![10u8; 28],
                credential_type: 0,
                drep_type: 3, // AlwaysNoConfidence
                drep_hash: None,
            },
            VoteDelegateeEntry {
                credential_hash: vec![20u8; 28],
                credential_type: 0,
                drep_type: 0, // KeyHash DRep
                drep_hash: Some(vec![30u8; 28]),
            },
        ];

        // Query pool 1 (owner delegates to AlwaysNoConfidence → DefaultNoConfidence = 2)
        let cbor1 = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.bytes(&[1u8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor1);
        let result = handle_pool_default_vote(&state, &mut dec);
        match result {
            QueryResult::StakePoolDefaultVote(vote) => {
                assert_eq!(
                    vote, 2,
                    "AlwaysNoConfidence delegation → DefaultNoConfidence (2)"
                );
            }
            _ => panic!("Expected StakePoolDefaultVote"),
        }

        // Query pool 2 (owner delegates to specific DRep → DefaultNo = 0)
        let cbor2 = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.bytes(&[2u8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor2);
        let result = handle_pool_default_vote(&state, &mut dec);
        match result {
            QueryResult::StakePoolDefaultVote(vote) => {
                assert_eq!(vote, 0, "Specific DRep delegation → DefaultNo (0)");
            }
            _ => panic!("Expected StakePoolDefaultVote"),
        }
    }

    #[test]
    fn test_pool_default_vote_no_delegation() {
        let state = make_state_with_pools();
        // Pool 1 has no owners with vote delegation → DefaultNo = 0
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.bytes(&[1u8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_default_vote(&state, &mut dec);
        match result {
            QueryResult::StakePoolDefaultVote(vote) => {
                assert_eq!(vote, 0, "No delegation → DefaultNo (0)");
            }
            _ => panic!("Expected StakePoolDefaultVote"),
        }
    }

    #[test]
    fn test_pool_default_vote_unknown_pool() {
        let state = make_state_with_pools();
        // Unknown pool → DefaultNo = 0
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.bytes(&[0xFFu8; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_pool_default_vote(&state, &mut dec);
        match result {
            QueryResult::StakePoolDefaultVote(vote) => {
                assert_eq!(vote, 0, "Unknown pool → DefaultNo (0)");
            }
            _ => panic!("Expected StakePoolDefaultVote"),
        }
    }
}
