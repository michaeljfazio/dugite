//! Per-epoch-boundary reward-debug dumper.
//!
//! Gated on the `reward-debug-dump` Cargo feature.  When the feature is
//! enabled at compile time AND `DUGITE_REWARD_DEBUG_DUMP=<dir>` is set at
//! runtime, this module writes one JSON file per epoch boundary capturing
//! the inputs and outputs of the reward-update computation, restricted to
//! the active pools (and optionally further filtered via
//! `DUGITE_REWARD_DEBUG_POOL_FILTER`).
//!
//! Production builds NEVER compile this code — zero runtime overhead.
//!
//! Tracking: issues #438 (parent) / #471 (this instrumentation).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::value::Lovelace;
use serde::{Deserialize, Serialize};

use super::{PendingRewardUpdate, StakeSnapshot};

const ENV_OUTPUT_DIR: &str = "DUGITE_REWARD_DEBUG_DUMP";
const ENV_POOL_FILTER: &str = "DUGITE_REWARD_DEBUG_POOL_FILTER";

/// Top-level dump for one epoch boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardDebugDump {
    pub epoch_from: u64,
    pub epoch_to: u64,
    pub boundary: String,
    pub scalars: Scalars,
    pub prev_protocol_params: ProtocolParamsDump,
    pub pools: Vec<PoolDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scalars {
    pub reserves_pre_rupd: u64,
    pub treasury_pre_rupd: u64,
    pub ss_fee: u64,
    pub bprev_total_blocks: u64,
    /// Signed RUPD reserves adjustment (see `PendingRewardUpdate::delta_reserves`
    /// and issue #796) — negative means reserves increased this boundary.
    pub delta_reserves: i128,
    pub delta_treasury: u64,
    pub total_rupd_credits: u64,
    /// Decentralisation `d` captured from the previous epoch — exact
    /// `Rational` (`prev_d_num / prev_d_den`); see issue #629.
    pub prev_d_num: u64,
    pub prev_d_den: u64,
    pub prev_protocol_version_major: u64,
    /// Epoch label of the GO snapshot used in this RUPD calculation.
    /// Diagnostic field for issue #438 — confirms which past epoch's stake
    /// data dugite is feeding into `compute_reward_update`.  Per the
    /// snapshot model, this should equal `epoch_to - 2` (Haskell's
    /// `ssStakeGo` semantics).  If it differs, dugite has a snapshot
    /// rotation off-by-one.
    #[serde(default)]
    pub go_snapshot_epoch: u64,
    /// Sum of all pool_stake values in the GO snapshot, filtered to
    /// pools with registered params.  This is what dugite passes as
    /// `total_active_stake` to the per-pool perf calculation.
    #[serde(default)]
    pub go_total_active_stake: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolParamsDump {
    pub protocol_version_major: u64,
    pub protocol_version_minor: u64,
    pub n_opt: u64,
    pub a0_num: u64,
    pub a0_den: u64,
    pub rho_num: u64,
    pub rho_den: u64,
    pub tau_num: u64,
    pub tau_den: u64,
    pub d_num: u64,
    pub d_den: u64,
    pub active_slots_coeff: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolDump {
    pub pool_id_hex: String,
    pub blocks_in_prev_epoch: u64,
    pub pool_stake_go: u64,
    pub pool_reg: PoolRegDump,
    pub credentials: Vec<CredDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolRegDump {
    pub pledge: u64,
    pub cost: u64,
    pub margin_numerator: u64,
    pub margin_denominator: u64,
    pub owners_hex: Vec<String>,
    pub reward_account_hex: String,
    pub vrf_keyhash_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredDump {
    /// 28-byte hex of the underlying credential hash (no type byte).
    pub cred_hash_hex: String,
    /// `"key_hash"` if Hash32's type-discriminator byte (offset 28) is 0;
    /// `"script"` if 1; `"unknown"` otherwise.  Matches
    /// `Credential::to_typed_hash32` convention.
    pub cred_type: String,
    /// True if this credential is listed in `pool_reg.owners`.
    pub is_owner: bool,
    /// Stake from the GO snapshot's stake_distribution (UTxO + reward
    /// balance at the moment the GO snapshot was built — i.e. two epoch
    /// boundaries ago).
    pub go_stake_distribution: u64,
    /// Reward account balance AT the moment of this boundary, BEFORE this
    /// boundary's RUPD credit is applied.
    pub reward_balance_pre_rupd: u64,
    /// RUPD credit produced by this boundary's reward-update computation.
    /// Will be added to `reward_balance_pre_rupd` if the credential is
    /// registered, or forwarded to treasury if not.
    pub rupd_credit: u64,
}

/// Output directory if dumping is enabled (env var set and non-empty).
fn output_dir() -> Option<PathBuf> {
    env::var(ENV_OUTPUT_DIR).ok().and_then(|s| {
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    })
}

/// Set of pool-id hex strings (lowercase) to include.  Empty set means
/// "include all pools".  Reads `DUGITE_REWARD_DEBUG_POOL_FILTER=hex,hex,…`.
fn pool_filter() -> HashSet<String> {
    env::var(ENV_POOL_FILTER)
        .ok()
        .map(|s| {
            s.split(',')
                .map(|hex| hex.trim().to_ascii_lowercase())
                .filter(|hex| !hex.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn include_pool(pool_id_hex: &str, filter: &HashSet<String>) -> bool {
    filter.is_empty() || filter.contains(pool_id_hex)
}

fn cred_type_for(hash32: &Hash32) -> &'static str {
    match hash32.as_bytes()[28] {
        0 => "key_hash",
        1 => "script",
        _ => "unknown",
    }
}

fn cred_hash_hex(hash32: &Hash32) -> String {
    hex_encode(&hash32.as_bytes()[..28])
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Build the JSON dump for one epoch boundary.
///
/// Pure function — no I/O.  O(pools × delegators-per-pool).
#[allow(clippy::too_many_arguments)]
pub fn capture(
    epoch_from: u64,
    epoch_to: u64,
    params_used: &ProtocolParameters,
    prev_d: &dugite_primitives::transaction::Rational,
    prev_protocol_version_major: u64,
    reserves_pre_rupd: Lovelace,
    treasury_pre_rupd: Lovelace,
    ss_fee: Lovelace,
    go: &StakeSnapshot,
    bprev_blocks_by_pool: &HashMap<Hash28, u64>,
    reward_accounts_pre_rupd: &HashMap<Hash32, Lovelace>,
    rupd: &PendingRewardUpdate,
    filter: &HashSet<String>,
) -> RewardDebugDump {
    let mut delegators_by_pool: HashMap<Hash28, Vec<Hash32>> = HashMap::new();
    for (cred, pool_id) in go.delegations.iter() {
        delegators_by_pool.entry(*pool_id).or_default().push(*cred);
    }

    let bprev_total_blocks: u64 = bprev_blocks_by_pool.values().sum();
    let total_rupd_credits: u64 = rupd.rewards.values().map(|l| l.0).sum();
    let go_total_active_stake: u64 = go
        .pool_stake
        .iter()
        .filter(|(pool_id, _)| go.pool_params.contains_key(pool_id))
        .fold(0u64, |acc, (_, s)| acc.saturating_add(s.0));

    let mut pools = Vec::with_capacity(go.pool_params.len());

    for (pool_id, pool_reg) in go.pool_params.iter() {
        let pool_id_hex = pool_id.to_hex();
        if !include_pool(&pool_id_hex, filter) {
            continue;
        }

        let blocks_in_prev_epoch = bprev_blocks_by_pool.get(pool_id).copied().unwrap_or(0);
        let pool_stake_go = go.pool_stake.get(pool_id).copied().unwrap_or(Lovelace(0)).0;

        // Owners — convert each Hash28 to the Hash32 key dugite uses
        // (zero-pad; byte 28 = 0, matching `Credential::to_typed_hash32`
        // for verification-key credentials, which is what pool owners
        // always are).
        let owner_set: HashSet<Hash32> = pool_reg
            .owners
            .iter()
            .map(|h| h.to_hash32_padded())
            .collect();

        let mut creds: Vec<Hash32> = delegators_by_pool.get(pool_id).cloned().unwrap_or_default();
        // Include any owners not present in delegations (rare but possible
        // when an owner has not yet delegated to its own pool).
        for owner in &owner_set {
            if !creds.contains(owner) {
                creds.push(*owner);
            }
        }
        creds.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        let mut cred_dumps = Vec::with_capacity(creds.len());
        for cred in creds {
            let go_stake = go
                .stake_distribution
                .get(&cred)
                .copied()
                .unwrap_or(Lovelace(0))
                .0;
            let reward_bal = reward_accounts_pre_rupd
                .get(&cred)
                .copied()
                .unwrap_or(Lovelace(0))
                .0;
            let credit = rupd.rewards.get(&cred).copied().unwrap_or(Lovelace(0)).0;
            cred_dumps.push(CredDump {
                cred_hash_hex: cred_hash_hex(&cred),
                cred_type: cred_type_for(&cred).to_string(),
                is_owner: owner_set.contains(&cred),
                go_stake_distribution: go_stake,
                reward_balance_pre_rupd: reward_bal,
                rupd_credit: credit,
            });
        }

        pools.push(PoolDump {
            pool_id_hex,
            blocks_in_prev_epoch,
            pool_stake_go,
            pool_reg: PoolRegDump {
                pledge: pool_reg.pledge.0,
                cost: pool_reg.cost.0,
                margin_numerator: pool_reg.margin_numerator,
                margin_denominator: pool_reg.margin_denominator,
                owners_hex: pool_reg.owners.iter().map(|h| h.to_hex()).collect(),
                reward_account_hex: hex_encode(&pool_reg.reward_account),
                vrf_keyhash_hex: pool_reg.vrf_keyhash.to_hex(),
            },
            credentials: cred_dumps,
        });
    }

    pools.sort_by(|a, b| a.pool_id_hex.cmp(&b.pool_id_hex));

    RewardDebugDump {
        epoch_from,
        epoch_to,
        boundary: format!("{}->{}", epoch_from, epoch_to),
        scalars: Scalars {
            reserves_pre_rupd: reserves_pre_rupd.0,
            treasury_pre_rupd: treasury_pre_rupd.0,
            ss_fee: ss_fee.0,
            bprev_total_blocks,
            delta_reserves: rupd.delta_reserves,
            delta_treasury: rupd.delta_treasury,
            total_rupd_credits,
            prev_d_num: prev_d.numerator,
            prev_d_den: prev_d.denominator,
            prev_protocol_version_major,
            go_snapshot_epoch: go.epoch.0,
            go_total_active_stake,
        },
        prev_protocol_params: ProtocolParamsDump {
            protocol_version_major: params_used.protocol_version_major,
            protocol_version_minor: params_used.protocol_version_minor,
            n_opt: params_used.n_opt,
            a0_num: params_used.a0.numerator,
            a0_den: params_used.a0.denominator,
            rho_num: params_used.rho.numerator,
            rho_den: params_used.rho.denominator,
            tau_num: params_used.tau.numerator,
            tau_den: params_used.tau.denominator,
            d_num: params_used.d.numerator,
            d_den: params_used.d.denominator,
            active_slots_coeff: params_used.active_slots_coeff,
        },
        pools,
    }
}

/// Write the dump as pretty JSON to `<dir>/epoch_<from>_to_<to>.json`,
/// creating `<dir>` if it does not exist.
pub fn write(dump: &RewardDebugDump, dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let filename = format!("epoch_{:06}_to_{:06}.json", dump.epoch_from, dump.epoch_to);
    let path = dir.join(filename);
    let file = fs::File::create(&path)?;
    let writer = io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, dump).map_err(io::Error::other)?;
    Ok(path)
}

/// Capture + write iff `DUGITE_REWARD_DEBUG_DUMP=<dir>` is set; otherwise
/// no-op.  Designed to be called once per epoch boundary, immediately
/// after the boundary's RUPD has been computed but before it has been
/// applied to reward_accounts.
#[allow(clippy::too_many_arguments)]
pub fn maybe_dump(
    epoch_from: u64,
    epoch_to: u64,
    params_used: &ProtocolParameters,
    prev_d: &dugite_primitives::transaction::Rational,
    prev_protocol_version_major: u64,
    reserves_pre_rupd: Lovelace,
    treasury_pre_rupd: Lovelace,
    ss_fee: Lovelace,
    go: &StakeSnapshot,
    bprev_blocks_by_pool: &HashMap<Hash28, u64>,
    reward_accounts_pre_rupd: &HashMap<Hash32, Lovelace>,
    rupd: &PendingRewardUpdate,
) {
    let Some(dir) = output_dir() else {
        return;
    };
    let filter = pool_filter();
    let dump = capture(
        epoch_from,
        epoch_to,
        params_used,
        prev_d,
        prev_protocol_version_major,
        reserves_pre_rupd,
        treasury_pre_rupd,
        ss_fee,
        go,
        bprev_blocks_by_pool,
        reward_accounts_pre_rupd,
        rupd,
        &filter,
    );
    match write(&dump, &dir) {
        Ok(path) => tracing::info!(
            ?path,
            epoch_from,
            epoch_to,
            pools = dump.pools.len(),
            "reward-debug dump written"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            dir = ?dir,
            epoch_from,
            epoch_to,
            "failed to write reward-debug dump"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::time::EpochNo;
    use std::sync::Arc;

    fn hash28(byte: u8) -> Hash28 {
        Hash::from_bytes([byte; 28])
    }

    fn key_cred_hash32(byte: u8) -> Hash32 {
        let mut b = [0u8; 32];
        b[..28].fill(byte);
        // byte[28] = 0 marks a key hash, matching `Credential::to_typed_hash32`.
        Hash::from_bytes(b)
    }

    fn script_cred_hash32(byte: u8) -> Hash32 {
        let mut b = [0u8; 32];
        b[..28].fill(byte);
        b[28] = 0x01;
        Hash::from_bytes(b)
    }

    fn make_go_snapshot() -> StakeSnapshot {
        let pool_id = hash28(0xaa);
        let owner_h28 = hash28(0xbc);
        let owner_h32 = owner_h28.to_hash32_padded();
        let delegator_h32 = key_cred_hash32(0xcd);

        let mut delegations: HashMap<Hash32, Hash28> = HashMap::new();
        delegations.insert(owner_h32, pool_id);
        delegations.insert(delegator_h32, pool_id);

        let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();
        pool_stake.insert(pool_id, Lovelace(1_000_000_000));

        let mut stake_distribution: HashMap<Hash32, Lovelace> = HashMap::new();
        stake_distribution.insert(owner_h32, Lovelace(400_000_000));
        stake_distribution.insert(delegator_h32, Lovelace(600_000_000));

        let mut pool_params: HashMap<Hash28, super::super::PoolRegistration> = HashMap::new();
        pool_params.insert(
            pool_id,
            super::super::PoolRegistration {
                pool_id,
                vrf_keyhash: Hash::from_bytes([0xee; 32]),
                pledge: Lovelace(0),
                cost: Lovelace(340_000_000),
                margin_numerator: 1,
                margin_denominator: 20,
                reward_account: vec![0xe0; 29],
                owners: vec![owner_h28],
                relays: vec![],
                metadata_url: None,
                metadata_hash: None,
            },
        );

        StakeSnapshot {
            epoch: EpochNo(1267),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(pool_params),
            stake_distribution: Arc::new(stake_distribution),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        }
    }

    #[test]
    fn capture_includes_pool_with_owner_marked() {
        let go = make_go_snapshot();
        let pool_id = hash28(0xaa);
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(pool_id, 5);

        let owner_h32 = hash28(0xbc).to_hash32_padded();
        let mut reward_accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        reward_accounts.insert(owner_h32, Lovelace(22_980_000));

        let mut rupd_rewards: HashMap<Hash32, Lovelace> = HashMap::new();
        rupd_rewards.insert(owner_h32, Lovelace(352_905_247));
        let rupd = PendingRewardUpdate {
            rewards: rupd_rewards,
            delta_treasury: 12_345_678,
            delta_reserves: 999_999_999,
        };

        let params = ProtocolParameters::mainnet_defaults();
        let dump = capture(
            1267,
            1268,
            &params,
            &dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            9,
            Lovelace(13_000_000_000_000_000),
            Lovelace(123_456_789_000),
            Lovelace(7_777_000_000),
            &go,
            &bprev,
            &reward_accounts,
            &rupd,
            &HashSet::new(),
        );

        assert_eq!(dump.epoch_from, 1267);
        assert_eq!(dump.epoch_to, 1268);
        assert_eq!(dump.boundary, "1267->1268");
        assert_eq!(dump.pools.len(), 1);

        let pool = &dump.pools[0];
        assert_eq!(pool.pool_id_hex, pool_id.to_hex());
        assert_eq!(pool.blocks_in_prev_epoch, 5);
        assert_eq!(pool.pool_stake_go, 1_000_000_000);
        assert_eq!(pool.pool_reg.cost, 340_000_000);
        assert_eq!(pool.pool_reg.margin_numerator, 1);
        assert_eq!(pool.pool_reg.margin_denominator, 20);
        assert_eq!(pool.credentials.len(), 2);

        let owner_entry = pool
            .credentials
            .iter()
            .find(|c| c.is_owner)
            .expect("owner present");
        assert_eq!(owner_entry.cred_type, "key_hash");
        assert_eq!(owner_entry.cred_hash_hex.len(), 56); // 28 bytes hex
        assert_eq!(owner_entry.go_stake_distribution, 400_000_000);
        assert_eq!(owner_entry.reward_balance_pre_rupd, 22_980_000);
        assert_eq!(owner_entry.rupd_credit, 352_905_247);

        assert_eq!(dump.scalars.bprev_total_blocks, 5);
        assert_eq!(dump.scalars.delta_treasury, 12_345_678);
        assert_eq!(dump.scalars.delta_reserves, 999_999_999);
        assert_eq!(dump.scalars.total_rupd_credits, 352_905_247);
    }

    #[test]
    fn capture_honours_pool_filter() {
        let go = make_go_snapshot();
        let pool_id = hash28(0xaa);
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(pool_id, 5);
        let reward_accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        let rupd = PendingRewardUpdate::default();
        let params = ProtocolParameters::mainnet_defaults();

        // Filter that excludes our pool.
        let mut filter = HashSet::new();
        filter.insert("0000000000000000000000000000000000000000000000000000000a".to_string());
        let dump = capture(
            1267,
            1268,
            &params,
            &dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            9,
            Lovelace(0),
            Lovelace(0),
            Lovelace(0),
            &go,
            &bprev,
            &reward_accounts,
            &rupd,
            &filter,
        );
        assert!(
            dump.pools.is_empty(),
            "pool filter should have excluded the only pool"
        );

        // Filter that includes our pool by exact hex.
        let mut filter = HashSet::new();
        filter.insert(pool_id.to_hex());
        let dump = capture(
            1267,
            1268,
            &params,
            &dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            9,
            Lovelace(0),
            Lovelace(0),
            Lovelace(0),
            &go,
            &bprev,
            &reward_accounts,
            &rupd,
            &filter,
        );
        assert_eq!(dump.pools.len(), 1);
    }

    #[test]
    fn capture_records_script_credential_type() {
        let mut go = make_go_snapshot();

        // Add a script-type delegator to the existing pool.
        let pool_id = hash28(0xaa);
        let script_h32 = script_cred_hash32(0x55);
        let delegations_mut = Arc::make_mut(&mut go.delegations);
        delegations_mut.insert(script_h32, pool_id);
        let stake_mut = Arc::make_mut(&mut go.stake_distribution);
        stake_mut.insert(script_h32, Lovelace(50_000_000));

        let bprev: HashMap<Hash28, u64> = HashMap::new();
        let reward_accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        let rupd = PendingRewardUpdate::default();
        let params = ProtocolParameters::mainnet_defaults();

        let dump = capture(
            1267,
            1268,
            &params,
            &dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            9,
            Lovelace(0),
            Lovelace(0),
            Lovelace(0),
            &go,
            &bprev,
            &reward_accounts,
            &rupd,
            &HashSet::new(),
        );

        let pool = &dump.pools[0];
        let script_entry = pool
            .credentials
            .iter()
            .find(|c| c.cred_type == "script")
            .expect("script-type credential present");
        assert!(!script_entry.is_owner);
        assert_eq!(script_entry.go_stake_distribution, 50_000_000);
    }

    #[test]
    fn write_produces_readable_json_roundtrip() {
        let go = make_go_snapshot();
        let pool_id = hash28(0xaa);
        let mut bprev: HashMap<Hash28, u64> = HashMap::new();
        bprev.insert(pool_id, 5);
        let reward_accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        let rupd = PendingRewardUpdate::default();
        let params = ProtocolParameters::mainnet_defaults();

        let dump = capture(
            1267,
            1268,
            &params,
            &dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            9,
            Lovelace(0),
            Lovelace(0),
            Lovelace(0),
            &go,
            &bprev,
            &reward_accounts,
            &rupd,
            &HashSet::new(),
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(&dump, tmp.path()).expect("write");
        assert!(path.exists());
        assert!(path.file_name().unwrap().to_string_lossy().contains("1267"));
        assert!(path.file_name().unwrap().to_string_lossy().contains("1268"));

        let contents = fs::read_to_string(&path).expect("read");
        let roundtrip: RewardDebugDump = serde_json::from_str(&contents).expect("parse");
        assert_eq!(roundtrip.epoch_from, dump.epoch_from);
        assert_eq!(roundtrip.epoch_to, dump.epoch_to);
        assert_eq!(roundtrip.pools.len(), dump.pools.len());
        assert_eq!(roundtrip.pools[0].pool_id_hex, dump.pools[0].pool_id_hex);
    }
}
