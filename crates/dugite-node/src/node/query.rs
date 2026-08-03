//! N2C LocalStateQuery response building.
//!
//! Contains `Node::update_query_state()` which assembles the `NodeStateSnapshot`
//! pushed into the `QueryHandler` on every block or periodically at tip, as well
//! as `build_era_summaries()` for `GetEraHistory` responses.

use super::Node;

// ─── Arithmetic helpers ───────────────────────────────────────────────────────

/// Convert an f64 to a (numerator, denominator) rational approximation.
///
/// Handles common Cardano genesis values like 0.05 → (1, 20).
pub(crate) fn float_to_rational(f: f64) -> (u64, u64) {
    if f == 0.0 {
        return (0, 1);
    }
    if f == 1.0 {
        return (1, 1);
    }
    // Try to find exact fraction with small denominators first
    for den in 1..=10000u64 {
        let num = (f * den as f64).round() as u64;
        let reconstructed = num as f64 / den as f64;
        if (reconstructed - f).abs() < 1e-12 {
            // Simplify by GCD
            let g = gcd(num, den);
            return (num / g, den / g);
        }
    }
    // Fallback: use large denominator
    let den = 1_000_000u64;
    let num = (f * den as f64).round() as u64;
    let g = gcd(num, den);
    (num / g, den / g)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Convert a Credential to (type, hash_bytes) for vote maps.
/// Returns (0, hash_28) for VerificationKey, (1, hash_28) for Script.
pub(crate) fn credential_to_bytes(
    cred: &dugite_primitives::credentials::Credential,
) -> (u8, Vec<u8>) {
    match cred {
        dugite_primitives::credentials::Credential::VerificationKey(h) => (0, h.as_ref().to_vec()),
        dugite_primitives::credentials::Credential::Script(h) => (1, h.as_ref().to_vec()),
    }
}

/// Truncate a padded `Hash32` key back to 28 bytes for N2C wire encoding.
///
/// The ledger uses `Hash32` (32 bytes) as HashMap keys for stake credentials,
/// DRep credential hashes, committee credential hashes, and pool voter keys.
/// These are Blake2b-224 (28-byte) hashes that were zero-padded to 32 bytes
/// to enable use as uniform HashMap keys (see `Hash28::to_hash32_padded()`).
///
/// The Cardano N2C wire format expects 28 bytes for all credential/pool-ID
/// hashes.  Sending 32 bytes causes cardano-cli to reject the response with
/// "hash bytes wrong size, expected 28 but got 32".
///
/// Only call this on `Hash32` values that are known to be padded 28-byte
/// hashes (credentials, pool IDs, DRep keys).  Do NOT call it on genuine
/// 32-byte hashes such as transaction IDs, block hashes, or VRF key hashes.
#[inline]
fn hash32_padded_to_28_bytes(h: &dugite_primitives::hash::Hash32) -> Vec<u8> {
    h.as_ref()[..28].to_vec()
}

/// The `drepExpiry` value that `GetDRepState` must report for one DRep.
///
/// Two distinct things are going on, and dugite previously did neither (#912).
///
/// **1. The stored expiry is the answer — never recompute it.**
/// `DRepRegistration::drep_expiry` mirrors Haskell's `drepExpiry` and is
/// maintained by every rule that may move it: `ConwayRegDRep` and
/// `ConwayUpdateDRep` (`Conway.Rules.GovCert`, via `computeDRepExpiry`) and
/// `updateVotingDRepExpiries` (`Conway.Rules.Certs`). Deriving the answer from
/// `registered_epoch + drep_activity` instead — as this query used to — pins
/// the reported expiry to registration time, so an `UpdateDRep` certificate or
/// a cast vote never moves it. It is also simply undefined on a
/// Haskell-snapshot import, where `registered_epoch` is not carried in the
/// snapshot and is stored as epoch 0.
///
/// **2. The dormant refund is applied at *query* time, not in the state.**
/// `queryDRepState` (cardano-ledger-api `Cardano.Ledger.Api.State.Query`)
/// returns the DRep map through `Conway.updateDormantDRepExpiry (nes ^. nesELL)`
/// applied to a **copy** of the `VState`:
///
/// ```haskell
/// queryDRepState nes creds
///   | null creds = updateDormantDRepExpiry' vState ^. vsDRepsL
///   | otherwise = updateDormantDRepExpiry' vStateFiltered ^. vsDRepsL
///   where
///     updateDormantDRepExpiry' = Conway.updateDormantDRepExpiry (nes ^. nesELL)
/// ```
///
/// So the epochs a DRep spent in a quiet-governance window are added back for
/// reporting purposes even though the ledger has not (yet) refunded them — the
/// in-state refund only happens when a proposal-carrying tx arrives
/// (`updateDormantDRepExpiries`). Between the dormant period starting and the
/// next proposal, the stored value is `num_dormant_epochs` lower than what
/// `cardano-cli query drep-state` prints.
///
/// The `actual < current_epoch` guard is Haskell's, verbatim: a DRep that is
/// already expired even after the refund is not revived by it.
///
/// ```haskell
/// updateExpiry =
///   drepExpiryL %~ \currentExpiry ->
///     let actualExpiry = binOpEpochNo (+) numDormantEpochs currentExpiry
///      in if actualExpiry < currentEpoch then currentExpiry else actualExpiry
/// ```
///
/// This is a reporting-only adjustment: it must NOT be written back into the
/// ledger state, or the refund would be applied twice.
pub(crate) fn query_drep_expiry(
    drep: &dugite_ledger::state::DRepRegistration,
    num_dormant_epochs: u64,
    current_epoch: dugite_primitives::EpochNo,
) -> u64 {
    let stored = drep.drep_expiry.0;
    let actual = stored.saturating_add(num_dormant_epochs);
    if actual < current_epoch.0 {
        stored
    } else {
        actual
    }
}

/// Build a `SnapshotStakeData` from a single `StakeSnapshot`.
///
/// The `script_creds` set distinguishes script-hash credentials (type=1) from
/// verification-key credentials (type=0).  We use the live ledger's
/// `script_stake_credentials` set as an approximation — credential types are
/// stable once registered, so this is accurate.
///
/// Each credential hash is truncated to 28 bytes (the snapshot stores Hash32
/// where the upper 4 bytes are zero-padding used for HashMap keying).
fn build_snapshot_stake_data(
    snap: &dugite_ledger::state::StakeSnapshot,
    script_creds: &std::collections::HashSet<dugite_primitives::hash::Hash32>,
) -> super::n2c_query::SnapshotStakeData {
    use super::n2c_query::{PoolParamsSnapshot, RelaySnapshot, SnapshotStakeData};

    // stake_entries: one entry per delegated credential
    let mut stake_entries = Vec::with_capacity(snap.stake_distribution.len());
    for (cred_hash, lovelace) in snap.stake_distribution.iter() {
        let cred_type = script_creds.contains(cred_hash) as u8;
        // Use the lower 28 bytes of the Hash32 key
        let hash28 = cred_hash.as_ref()[..28].to_vec();
        stake_entries.push((cred_type, hash28, lovelace.0));
    }

    // delegation_entries: map credential → pool_id
    let mut delegation_entries = Vec::with_capacity(snap.delegations.len());
    for (cred_hash, pool_id) in snap.delegations.iter() {
        let cred_type = script_creds.contains(cred_hash) as u8;
        let hash28 = cred_hash.as_ref()[..28].to_vec();
        delegation_entries.push((cred_type, hash28, pool_id.as_ref().to_vec()));
    }

    // pool_params: convert snapshot pool params to PoolParamsSnapshot
    let pool_params: Vec<PoolParamsSnapshot> = snap
        .pool_params
        .iter()
        .map(|(pool_id, reg)| {
            let relays: Vec<RelaySnapshot> = reg
                .relays
                .iter()
                .map(|r| match r {
                    dugite_primitives::transaction::Relay::SingleHostAddr { port, ipv4, ipv6 } => {
                        RelaySnapshot::SingleHostAddr {
                            port: *port,
                            ipv4: *ipv4,
                            ipv6: *ipv6,
                        }
                    }
                    dugite_primitives::transaction::Relay::SingleHostName { port, dns_name } => {
                        RelaySnapshot::SingleHostName {
                            port: *port,
                            dns_name: dns_name.clone(),
                        }
                    }
                    dugite_primitives::transaction::Relay::MultiHostName { dns_name } => {
                        RelaySnapshot::MultiHostName {
                            dns_name: dns_name.clone(),
                        }
                    }
                })
                .collect();
            PoolParamsSnapshot {
                pool_id: pool_id.as_ref().to_vec(),
                vrf_keyhash: reg.vrf_keyhash.as_ref().to_vec(),
                pledge: reg.pledge.0,
                cost: reg.cost.0,
                margin_num: reg.margin_numerator,
                margin_den: reg.margin_denominator,
                reward_account: reg.reward_account.clone(),
                owners: reg.owners.iter().map(|o| o.as_ref().to_vec()).collect(),
                relays,
                metadata_url: reg.metadata_url.clone(),
                metadata_hash: reg.metadata_hash.map(|h| h.as_ref().to_vec()),
            }
        })
        .collect();

    SnapshotStakeData {
        stake_entries,
        delegation_entries,
        pool_params,
    }
}

// ─── Node impl: query state ───────────────────────────────────────────────────

impl Node {
    /// Update the query handler with the current ledger state.
    ///
    /// Called whenever a block is applied at tip and periodically during sync
    /// (every 30 seconds) so that N2C `LocalStateQuery` requests reflect recent
    /// on-chain state.
    pub async fn update_query_state(&self) {
        use super::n2c_query::{
            CommitteeMemberSnapshot, CommitteeSnapshot, DRepDelegationGroup, DRepKey, DRepSnapshot,
            DRepStakeEntry, GenesisConfigSnapshot, PoolParamsSnapshot, PoolStakeSnapshotEntry,
            ProposalSnapshot, ShelleyPParamsSnapshot, StakeAddressSnapshot, StakeDelegDepositEntry,
            StakePoolSnapshot, StakeSnapshotsResult, VoteDelegateeEntry,
        };

        let ls = self.ledger_state.read().await;
        let eh = self.era_history.read().await;

        // Build per-pool stake map from delegations for accurate reporting.
        // Per Cardano spec, total stake = UTxO-delegated stake + reward account balance.
        let mut pool_stake_map: std::collections::HashMap<dugite_primitives::hash::Hash28, u64> =
            std::collections::HashMap::new();
        for (cred_hash, pool_id) in ls.certs.delegations.iter() {
            let utxo_stake = ls
                .certs
                .stake_distribution
                .stake_map
                .get(cred_hash)
                .map(|l| l.0)
                .unwrap_or(0);
            let reward_balance = ls
                .certs
                .reward_accounts
                .get(cred_hash)
                .map(|l| l.0)
                .unwrap_or(0);
            *pool_stake_map.entry(*pool_id).or_default() += utxo_stake + reward_balance;
        }

        // Build stake pool snapshots with actual per-pool stake.
        // `getTotalStake globals nes = circulation (nesEs nes) (maxLovelaceSupply
        // globals)` — circulation is maxLovelaceSupply MINUS RESERVES ONLY.
        // Treasury is NOT subtracted (verified numerically against cardano-node
        // 11.0.1: pool/(maxSupply-reserves) reproduces its fraction exactly,
        // pool/(maxSupply-reserves-treasury) does not). The same derivation is
        // already used by the non-myopic reward handler.
        //
        // This is the denominator `poolsByTotalStakeFraction` divides by; total
        // ACTIVE stake is NOT it, which is why every pool on a fully-delegated
        // chain used to report ~1.0 (#905).
        let total_circulation: u64 = ls.max_lovelace_supply.saturating_sub(ls.epochs.reserves.0);
        let stake_pools: Vec<StakePoolSnapshot> = ls
            .certs
            .pool_params
            .iter()
            .map(|(pool_id, reg)| StakePoolSnapshot {
                pool_id: pool_id.as_ref().to_vec(),
                stake: pool_stake_map.get(pool_id).copied().unwrap_or(0),
                vrf_keyhash: reg.vrf_keyhash.as_ref().to_vec(),
                total_circulation,
            })
            .collect();

        // `PoolDistr` as GetPoolDistr(2) must answer it (#964).
        //
        // NOT the live map above. `GetPoolDistr2 mPoolIds` is
        //   `calculatePoolDistr' pred (ssStakeSet . esSnapshots $ getEpochState st)`
        // — the FROZEN `set` snapshot — and its ratio is
        //   `spssStakeRatio = spssStake / ssTotalActiveStake`
        // *of that snapshot*, with no rescale to circulation. The circulation
        // rescale belongs to `poolsByTotalStakeFraction` (tag 37), which is a
        // different function over `currentSnapshot` (live instant stake); #905
        // established that for tag 37 and it was then applied to tag 36 as
        // well, which is half of #964.
        //
        // The consequence was operational, not cosmetic: `cardano-cli query
        // leadership-schedule` computes the schedule CLIENT-side and takes σ
        // straight from this answer, so a denominator inflated from active
        // stake to circulation shrinks σ and drops leader slots.
        let (pool_distr, pool_distr_total_active_stake) = {
            match ls.epochs.snapshots.set.as_ref() {
                None => (Vec::new(), 0u64),
                Some(snap) => {
                    // `spssNumDelegators` — the count of credentials delegating
                    // to the pool IN THIS SNAPSHOT.
                    let mut delegators: std::collections::HashMap<
                        dugite_primitives::hash::Hash28,
                        u64,
                    > = std::collections::HashMap::new();
                    for pool_id in snap.delegations.values() {
                        *delegators.entry(*pool_id).or_default() += 1;
                    }
                    // `ssTotalActiveStake` of the snapshot — the denominator of
                    // every ratio below and the `pdTotalActiveStake` field.
                    let total: u64 = snap.pool_stake.values().map(|l| l.0).sum();
                    let mut entries: Vec<crate::node::n2c_query::types::PoolDistrEntry> = snap
                        .pool_stake
                        .iter()
                        .map(
                            |(pool_id, stake)| crate::node::n2c_query::types::PoolDistrEntry {
                                pool_id: pool_id.as_ref().to_vec(),
                                stake: stake.0,
                                vrf_keyhash: snap
                                    .pool_params
                                    .get(pool_id)
                                    .map(|p| p.vrf_keyhash.as_ref().to_vec())
                                    .unwrap_or_default(),
                                delegator_count: delegators.get(pool_id).copied().unwrap_or(0),
                            },
                        )
                        .collect();
                    // `VMap.toMap` yields ascending key order; the response is a
                    // CBOR map, so a stable order keeps it byte-comparable.
                    entries.sort_by(|a, b| a.pool_id.cmp(&b.pool_id));
                    (entries, total)
                }
            }
        };

        // Build DRep snapshots with delegator lookup.
        //
        // Previously this was an O(N_dreps × N_delegations) scan — for each
        // DRep we filtered the entire `vote_delegations` map.  At preview
        // epoch 29 (~200 DReps × ~10 000 delegations) that was ~2 M ops per
        // snapshot rebuild, blocking `post_block_apply_updates` at 1 Hz.
        // Issue #702.
        //
        // Invert the iteration: a single O(N_delegations) pass over the
        // delegations map groups stake credentials by DRep hash; then the
        // per-DRep lookup is O(1).  Total: O(N_dreps + N_delegations).
        let mut delegators_by_drep: std::collections::HashMap<
            dugite_primitives::hash::Hash32,
            Vec<Vec<u8>>,
        > = std::collections::HashMap::with_capacity(ls.gov.governance.dreps.len());
        for (stake_cred, drep_target) in &ls.gov.governance.vote_delegations {
            // `DRep::credential_hash32` covers both Key and Script credentials
            // and matches the form used to key `dreps` (script-DRep delegators
            // would be silently mis-matched by `Hash28::to_hash32_padded`).
            if let Some(target_hash) = drep_target.credential_hash32() {
                delegators_by_drep
                    .entry(target_hash)
                    .or_default()
                    .push(hash32_padded_to_28_bytes(stake_cred));
            }
        }

        let drep_entries: Vec<DRepSnapshot> = ls
            .gov
            .governance
            .dreps
            .iter()
            .map(|(hash, drep)| {
                let expiry =
                    query_drep_expiry(drep, ls.gov.governance.num_dormant_epochs, ls.epoch);
                let delegator_hashes = delegators_by_drep.remove(hash).unwrap_or_default();
                DRepSnapshot {
                    // DRep hash keys are Hash32 padded from 28-byte credential hashes.
                    credential_hash: hash32_padded_to_28_bytes(hash),
                    // DRepRegistration stores the full Credential enum, so we can derive the type
                    // directly: 0 = VerificationKey (KeyHashObj), 1 = Script (ScriptHashObj).
                    credential_type: drep.credential.is_script() as u8,
                    deposit: drep.deposit.0,
                    anchor_url: drep.anchor.as_ref().map(|a| a.url.clone()),
                    anchor_hash: drep.anchor.as_ref().map(|a| a.data_hash.as_ref().to_vec()),
                    expiry_epoch: expiry,
                    delegator_hashes,
                }
            })
            .collect();

        // Build governance proposal snapshots — LIVE view (`GetGovState`'s
        // embedded `ConwayGovState.cgsProposals`, tag 24).
        //
        // ALL governance action types (InfoAction, ParameterChange, HardForkInitiation,
        // UpdateCommittee, NewConstitution, NoConfidence, TreasuryWithdrawals) are stored
        // in governance.proposals by process_proposal().  We faithfully convert every
        // one of them here, carrying the full GovAction enum so the CBOR encoder can
        // reproduce the complete action body on the wire (fixes issue #172).
        //
        // `build_proposal_snapshot_list` is shared with the FROZEN DRep-pulser
        // view built just below for `GetProposals` (tag 31, #922) so the two
        // call sites can never drift in sort/pruning logic.
        let governance_proposals: Vec<ProposalSnapshot> = build_proposal_snapshot_list(
            &ls.gov.governance.proposals,
            &ls.gov.governance.votes_by_action,
            ls.gov.governance.enacted_pparam_update.as_ref(),
            ls.gov.governance.enacted_hard_fork.as_ref(),
            ls.gov.governance.enacted_committee.as_ref(),
            ls.gov.governance.enacted_constitution.as_ref(),
        );

        // Build governance proposal snapshots — FROZEN DRep-pulser view,
        // answered by `GetProposals` (tag 31, #922).
        //
        // Proven Haskell mechanism: `Cardano.Ledger.Api.State.Query.queryProposals`
        // NEVER reads live `cgsProposals`. It reads `dpProposals` (while the
        // pulser is still pulsing) or `psProposals` (once `DRComplete`) from
        // the `DRepPulsingState` — the SAME frozen proposal list in both
        // cases, refreshed exactly once per epoch boundary by
        // `setFreshDRepPulsingState` inside `ConwayEPOCH`'s `epochTransition`.
        // A proposal submitted mid-epoch is therefore invisible to this query
        // until the *next* epoch boundary rotates the pulser, even though the
        // live `governance_proposals` view above (and the ledger itself)
        // already contains it.
        //
        // `ratification_snapshot` is dugite's existing `dpProposals`-equivalent,
        // captured at each epoch boundary for the #903 ratification-input fix
        // (`RatifySignal dpProposals`). Reusing it here means #903 and #922
        // share one source of truth for "what did the pulser last freeze".
        //
        // `None` only at genesis or when loading a ledger-state snapshot that
        // predates the field; `ratify_proposals()` itself falls back to the
        // live state in that case (see `state/mod.rs`'s `RatificationSnapshot`
        // doc comment), so mirror that fallback here rather than reporting an
        // empty list.
        let governance_proposals_frozen: Vec<ProposalSnapshot> =
            match ls.gov.governance.ratification_snapshot.as_ref() {
                Some(snap) => build_proposal_snapshot_list(
                    &snap.proposals,
                    &snap.votes_by_action,
                    snap.enacted_pparam_update.as_ref(),
                    snap.enacted_hard_fork.as_ref(),
                    snap.enacted_committee.as_ref(),
                    snap.enacted_constitution.as_ref(),
                ),
                None => governance_proposals.clone(),
            };

        // Build committee snapshot.
        // Iterate committee_expiration (the canonical member list) rather than
        // committee_hot_keys, so that members without hot key authorization
        // (MemberNotAuthorized) are included in the response.
        let resigned_set: std::collections::HashSet<_> =
            ls.gov.governance.committee_resigned.keys().collect();
        let committee = CommitteeSnapshot {
            members: ls
                .gov
                .governance
                .committee_expiration
                .iter()
                .map(|(cold, _expiry)| {
                    let is_resigned = resigned_set.contains(cold);
                    let hot_key = ls.gov.governance.committee_hot_keys.get(cold);

                    // Determine hot credential authorization status:
                    // 0 = MemberAuthorized (has hot key), 1 = MemberNotAuthorized, 2 = Resigned
                    let hot_status = if is_resigned {
                        2
                    } else if hot_key.is_some() {
                        0
                    } else {
                        1 // MemberNotAuthorized: in expiration map but no hot key
                    };

                    CommitteeMemberSnapshot {
                        // Committee cold/hot credentials are stored as Hash32 (padded from
                        // 28-byte Blake2b-224 hashes). Truncate to 28 bytes for N2C wire format.
                        cold_credential: hash32_padded_to_28_bytes(cold),
                        // Use the script_committee_credentials set to correctly distinguish
                        // key credentials (0) from script credentials (1).
                        cold_credential_type: ls
                            .gov
                            .governance
                            .script_committee_credentials
                            .contains(cold) as u8,
                        hot_status,
                        hot_credential: match hot_key {
                            Some(hk) if !is_resigned => Some(hash32_padded_to_28_bytes(hk)),
                            _ => None,
                        },
                        // Hot credential type: 0=KeyHash, 1=ScriptHash.
                        // Resolved by probing script_committee_hot_credentials with the current
                        // hot key hash.  The set is keyed by hot credential hash so that
                        // re-authorization with a different hot key is handled naturally: the
                        // new hot key either is or is not in the script set independently of
                        // any prior authorization for the same cold key.
                        hot_credential_type: match hot_key {
                            Some(hk) if !is_resigned => {
                                ls.gov
                                    .governance
                                    .script_committee_hot_credentials
                                    .contains(hk) as u8
                            }
                            _ => 0,
                        },
                        // MemberStatus: 0=Active, 1=Expired, 2=Unrecognized.
                        // Expiry is inclusive — member is active through their
                        // expiry epoch (matches `expire_committee_members` and
                        // ratification's `currentEpoch > validUntil` rule).
                        // Unrecognized doesn't apply here because we iterate the
                        // canonical committee_expiration map.
                        member_status: if ls.epoch.0 > _expiry.0 { 1 } else { 0 },
                        expiry_epoch: Some(_expiry.0),
                    }
                })
                .collect(),
            threshold: ls
                .gov
                .governance
                .committee_threshold
                .as_ref()
                .map(|r| (r.numerator, r.denominator))
                .or(Some((2, 3))), // Fallback to 2/3 if not set
            current_epoch: ls.epoch.0,
        };

        // Build stake address snapshots (delegations + rewards).
        // `cred_hash` is a Hash32 padded from a 28-byte stake key hash; truncate to 28 bytes.
        // `pool_id` from `delegations` is a Hash28, already the right size.
        let stake_addresses: Vec<StakeAddressSnapshot> = ls
            .certs
            .reward_accounts
            .iter()
            .map(|(cred_hash, rewards)| {
                let delegated_pool = ls
                    .certs
                    .delegations
                    .get(cred_hash)
                    .map(|pool_id| pool_id.as_ref().to_vec());
                StakeAddressSnapshot {
                    // reward_accounts keys are Hash32 padded from 28-byte credential hashes.
                    credential_hash: hash32_padded_to_28_bytes(cred_hash),
                    delegated_pool,
                    reward_balance: rewards.0,
                }
            })
            .collect();

        // Build stake snapshots (mark/set/go)
        let stake_snapshots = {
            // Collect all unique pool IDs across all snapshots
            let mut all_pool_ids = std::collections::BTreeSet::new();
            if let Some(ref snap) = ls.epochs.snapshots.mark {
                all_pool_ids.extend(snap.pool_stake.keys().cloned());
            }
            if let Some(ref snap) = ls.epochs.snapshots.set {
                all_pool_ids.extend(snap.pool_stake.keys().cloned());
            }
            if let Some(ref snap) = ls.epochs.snapshots.go {
                all_pool_ids.extend(snap.pool_stake.keys().cloned());
            }

            let pools: Vec<PoolStakeSnapshotEntry> = all_pool_ids
                .iter()
                .map(|pid| PoolStakeSnapshotEntry {
                    pool_id: pid.as_ref().to_vec(),
                    mark_stake: ls
                        .epochs
                        .snapshots
                        .mark
                        .as_ref()
                        .and_then(|s| s.pool_stake.get(pid))
                        .map(|l| l.0)
                        .unwrap_or(0),
                    set_stake: ls
                        .epochs
                        .snapshots
                        .set
                        .as_ref()
                        .and_then(|s| s.pool_stake.get(pid))
                        .map(|l| l.0)
                        .unwrap_or(0),
                    go_stake: ls
                        .epochs
                        .snapshots
                        .go
                        .as_ref()
                        .and_then(|s| s.pool_stake.get(pid))
                        .map(|l| l.0)
                        .unwrap_or(0),
                })
                .collect();

            let total_mark_stake = pools.iter().map(|p| p.mark_stake).sum();
            let total_set_stake = pools.iter().map(|p| p.set_stake).sum();
            let total_go_stake = pools.iter().map(|p| p.go_stake).sum();

            StakeSnapshotsResult {
                pools,
                total_mark_stake,
                total_set_stake,
                total_go_stake,
            }
        };

        // Build full per-credential snapshot data for DebugNewEpochState (cncli snapshot).
        // We use the live script_stake_credentials set to determine credential types.
        let snap_mark = ls
            .epochs
            .snapshots
            .mark
            .as_ref()
            .map(|s| build_snapshot_stake_data(s, &ls.certs.script_stake_credentials))
            .unwrap_or_default();
        let snap_set = ls
            .epochs
            .snapshots
            .set
            .as_ref()
            .map(|s| build_snapshot_stake_data(s, &ls.certs.script_stake_credentials))
            .unwrap_or_default();
        let snap_go = ls
            .epochs
            .snapshots
            .go
            .as_ref()
            .map(|s| build_snapshot_stake_data(s, &ls.certs.script_stake_credentials))
            .unwrap_or_default();

        // Build per-pool epoch block count map for NewEpochState [1]/[2] fields.
        // Haskell places the *previous* epoch's BlocksMade at [1] and the current
        // at [2].  We expose the current epoch's counts as [1] (best approximation
        // without a previous-epoch tracker), and leave [2] empty.
        let epoch_blocks_by_pool: Vec<(Vec<u8>, u64)> = ls
            .consensus
            .epoch_blocks_by_pool
            .iter()
            .map(|(pool_id, count)| (pool_id.as_ref().to_vec(), *count))
            .collect();

        // Build pool params entries
        let pool_params_entries: Vec<PoolParamsSnapshot> = ls
            .certs
            .pool_params
            .iter()
            .map(|(pool_id, reg)| {
                use super::n2c_query::RelaySnapshot;
                let relays: Vec<RelaySnapshot> = reg
                    .relays
                    .iter()
                    .map(|r| match r {
                        dugite_primitives::transaction::Relay::SingleHostAddr {
                            port,
                            ipv4,
                            ipv6,
                        } => RelaySnapshot::SingleHostAddr {
                            port: *port,
                            ipv4: *ipv4,
                            ipv6: *ipv6,
                        },
                        dugite_primitives::transaction::Relay::SingleHostName {
                            port,
                            dns_name,
                        } => RelaySnapshot::SingleHostName {
                            port: *port,
                            dns_name: dns_name.clone(),
                        },
                        dugite_primitives::transaction::Relay::MultiHostName { dns_name } => {
                            RelaySnapshot::MultiHostName {
                                dns_name: dns_name.clone(),
                            }
                        }
                    })
                    .collect();
                PoolParamsSnapshot {
                    pool_id: pool_id.as_ref().to_vec(),
                    vrf_keyhash: reg.vrf_keyhash.as_ref().to_vec(),
                    pledge: reg.pledge.0,
                    cost: reg.cost.0,
                    margin_num: reg.margin_numerator,
                    margin_den: reg.margin_denominator,
                    reward_account: reg.reward_account.clone(),
                    owners: reg.owners.iter().map(|o| o.as_ref().to_vec()).collect(),
                    relays,
                    metadata_url: reg.metadata_url.clone(),
                    metadata_hash: reg.metadata_hash.map(|h| h.as_ref().to_vec()),
                }
            })
            .collect();

        // Build protocol params snapshot for CBOR encoding
        let protocol_params = protocol_params_snapshot(&ls.epochs.protocol_params);
        // `previousPParams`: the params in force before the last enactment.
        let prev_protocol_params = protocol_params_snapshot(&ls.epochs.prev_protocol_params);

        // Build stake delegation deposits (registered stake credentials → per-credential deposit).
        // Uses the stored deposit paid at registration time for correct values when
        // key_deposit changes via governance. Falls back to current key_deposit for
        // credentials registered before per-credential tracking was added.
        let fallback_deposit = ls.epochs.protocol_params.key_deposit.0;
        let stake_deleg_deposits: Vec<StakeDelegDepositEntry> = ls
            .certs
            .reward_accounts
            .keys()
            .map(|cred_hash| StakeDelegDepositEntry {
                credential_hash: cred_hash.as_ref()[..28].to_vec(),
                // Use the script_stake_credentials set to distinguish key (0) from script (1).
                credential_type: ls.certs.script_stake_credentials.contains(cred_hash) as u8,
                deposit: ls
                    .certs
                    .stake_key_deposits
                    .get(cred_hash)
                    .copied()
                    .unwrap_or(fallback_deposit),
            })
            .collect();

        // Build DRep stake distribution (DRep -> total delegated stake).
        //
        // Answered from the FROZEN per-epoch snapshot, NOT live state (#950).
        // Haskell's `queryDRepStakeDistr` (cardano-ledger-api State/Query.hs) is
        //
        //     distr = psDRepDistr . fst $ finishedPulserState nes
        //
        // and the pulser's inputs (`dpAccounts`, `dpInstantStake`,
        // `dpDRepState`, `dpProposalDeposits`) are captured ONCE per epoch
        // boundary by `setFreshDRepPulsingState`. Forcing the pulser to
        // completion mid-epoch only folds chunks of that already-captured map;
        // it never re-reads `NewEpochState`. So a mid-epoch vote delegation is
        // invisible until the next boundary — and a credential REGISTERED
        // mid-epoch is not in `dpAccounts` at all, so no amount of pulsing can
        // surface it.
        //
        // This previously recomputed the distribution live on every snapshot
        // build, which over-reported any DRep that had just received a
        // delegation. Architecturally the same mistake as #922, where
        // `GetProposals` served live `cgsProposals` instead of the pulser's
        // frozen `dpProposals`.
        //
        // `drep_distribution_snapshot` + the two predefined-DRep companions are
        // dugite's `psDRepDistr`; they already sum
        // `InstantStake + ProposalDeposits + AccountBalance` per credential
        // (the deposit term was added in #949).
        let drep_stake_distr: Vec<DRepStakeEntry> = {
            let gov = &ls.gov.governance;
            let mut entries: Vec<DRepStakeEntry> = gov
                .drep_distribution_snapshot
                .iter()
                .map(|(hash32, stake)| DRepStakeEntry {
                    drep_type: 0,
                    // The snapshot key is a Hash32 padded from a 28-byte DRep
                    // key hash; the wire form is the bare 28 bytes.
                    drep_hash: Some(hash32.as_ref()[..28].to_vec()),
                    stake: *stake,
                })
                .collect();
            // Haskell keeps both predefined DReps in the SAME `psDRepDistr` map
            // and does not special-case them (`addToDRepDistr` takes
            // `updatedDistr` unconditionally for both), so emit them alongside.
            entries.push(DRepStakeEntry {
                drep_type: 2,
                drep_hash: None,
                stake: gov.drep_snapshot_abstain,
            });
            entries.push(DRepStakeEntry {
                drep_type: 3,
                drep_hash: None,
                stake: gov.drep_snapshot_no_confidence,
            });
            entries
        };

        // Build vote delegatee entries.
        // `stake_cred` is a Hash32 padded from a 28-byte stake key hash; truncate to 28 bytes.
        // DRep::KeyHash contains a Hash32 padded from a 28-byte DRep key hash; also truncate.
        // DRep::ScriptHash contains a Hash28 (ScriptHash); already correct size.
        let vote_delegatees: Vec<VoteDelegateeEntry> = {
            use dugite_primitives::transaction::DRep;
            ls.gov
                .governance
                .vote_delegations
                .iter()
                .map(|(stake_cred, drep)| {
                    let (drep_type, drep_hash) = match drep {
                        // DRep::KeyHash stores the DRep key as Hash32 (padded from 28 bytes).
                        DRep::KeyHash(h) => (0u8, Some(h.as_ref()[..28].to_vec())),
                        // DRep::ScriptHash stores the script hash as Hash28 (correct size).
                        DRep::ScriptHash(h) => (1u8, Some(h.as_ref().to_vec())),
                        DRep::Abstain => (2u8, None),
                        DRep::NoConfidence => (3u8, None),
                    };
                    VoteDelegateeEntry {
                        // vote_delegations keys are Hash32 padded from 28-byte stake key hashes.
                        credential_hash: hash32_padded_to_28_bytes(stake_cred),
                        // Use the script_stake_credentials set to distinguish key (0) from script (1).
                        credential_type: ls.certs.script_stake_credentials.contains(stake_cred)
                            as u8,
                        drep_type,
                        drep_hash,
                    }
                })
                .collect()
        };

        // Build DRep delegation groups for GetDRepDelegations (tag 39, V23+).
        //
        // Wire shape is Map<DRep, Set<Credential Staking>> — the OPPOSITE
        // orientation of FilteredVoteDelegatees (tag 28).  We invert the
        // ledger's stake-cred → DRep map into a DRep → [stake-cred] grouping
        // and produce one DRepDelegationGroup per DRep that has at least one
        // delegator.  Per-DRep credential lists are deterministically sorted
        // (type, hash) for canonical CBOR.
        let drep_delegations: Vec<DRepDelegationGroup> = {
            use dugite_primitives::transaction::DRep;
            use std::collections::BTreeMap;
            // Key DRepKey by (drep_type, drep_hash) so all four DRep variants
            // share one ordering and BTreeMap iteration is deterministic.
            // Key = (drep_type, optional 28-byte drep hash); Value = Vec<(cred_type, cred_hash)>
            type DRepKeyTuple = (u8, Option<Vec<u8>>);
            type CredEntry = (u8, Vec<u8>);
            let mut by_drep: BTreeMap<DRepKeyTuple, Vec<CredEntry>> = BTreeMap::new();
            for (stake_cred, drep) in &ls.gov.governance.vote_delegations {
                let (drep_type, drep_hash) = match drep {
                    DRep::KeyHash(h) => (0u8, Some(h.as_ref()[..28].to_vec())),
                    DRep::ScriptHash(h) => (1u8, Some(h.as_ref().to_vec())),
                    DRep::Abstain => (2u8, None),
                    DRep::NoConfidence => (3u8, None),
                };
                let cred_type = ls.certs.script_stake_credentials.contains(stake_cred) as u8;
                let cred_hash = hash32_padded_to_28_bytes(stake_cred);
                by_drep
                    .entry((drep_type, drep_hash))
                    .or_default()
                    .push((cred_type, cred_hash));
            }
            by_drep
                .into_iter()
                .map(|((drep_type, drep_hash), mut creds)| {
                    // Sort credentials deterministically by (type, hash).
                    creds.sort();
                    DRepDelegationGroup {
                        drep: DRepKey {
                            drep_type,
                            drep_hash,
                        },
                        credentials: creds,
                    }
                })
                .collect()
        };

        // Build ratify_enacted proposals from governance.last_ratified.
        // Include the full GovAction so the CBOR encoder can faithfully reproduce
        // the action body in GetRatifyState responses (same fix as for governance_proposals).
        let ratify_enacted = ls
            .gov
            .governance
            .last_ratified
            .iter()
            .map(|(action_id, state)| {
                let action_type = gov_action_type_str(&state.procedure.gov_action);
                let (committee_votes, drep_votes, spo_votes) =
                    build_vote_maps(&ls.gov.governance.votes_by_action, action_id);
                let proposal = ProposalSnapshot {
                    tx_id: action_id.transaction_id.as_ref().to_vec(),
                    action_index: action_id.action_index,
                    action_type: action_type.to_string(),
                    proposed_epoch: state.proposed_epoch.0,
                    expires_epoch: state.expires_epoch.0,
                    yes_votes: state.yes_votes,
                    no_votes: state.no_votes,
                    abstain_votes: state.abstain_votes,
                    deposit: state.procedure.deposit.0,
                    return_addr: state.procedure.return_addr.clone(),
                    anchor_url: state.procedure.anchor.url.clone(),
                    anchor_hash: state.procedure.anchor.data_hash.as_ref().to_vec(),
                    gov_action: state.procedure.gov_action.clone(),
                    committee_votes,
                    drep_votes,
                    spo_votes,
                };
                let gov_id = super::n2c_query::GovActionId {
                    tx_id: action_id.transaction_id.as_ref().to_vec(),
                    action_index: action_id.action_index,
                };
                (proposal, gov_id)
            })
            .collect();

        let snapshot = super::n2c_query::NodeStateSnapshot {
            tip: ls.tip.clone(),
            epoch: ls.epoch,
            era: ls.era.to_era_index(),
            block_number: ls.current_block_number(),
            system_start: self
                .shelley_genesis
                .as_ref()
                .map(|g| g.system_start.clone())
                .unwrap_or_else(|| self.config.network.system_start().to_string()),
            utxo_count: ls.utxo.utxo_set.len(),
            delegations_count: ls.certs.delegations.len(),
            pool_count: ls.certs.pool_params.len(),
            treasury: ls.epochs.treasury.0,
            reserves: ls.epochs.reserves.0,
            // Active DRep count: only DReps whose activity window has not expired.
            // Inactive DReps (active=false) remain registered in the map until
            // explicitly deregistered via UnregDRep, but external tools (Koios,
            // cardano-cli) report only the active count.
            drep_count: ls.gov.governance.active_drep_count(),
            proposal_count: ls.gov.governance.proposals.len(),
            protocol_params,
            prev_protocol_params,
            stake_pools,
            drep_entries,
            governance_proposals,
            governance_proposals_frozen,
            enacted_pparam_update: ls
                .gov
                .governance
                .enacted_pparam_update
                .as_ref()
                .map(|id| (id.transaction_id.as_ref().to_vec(), id.action_index)),
            enacted_hard_fork: ls
                .gov
                .governance
                .enacted_hard_fork
                .as_ref()
                .map(|id| (id.transaction_id.as_ref().to_vec(), id.action_index)),
            enacted_committee: ls
                .gov
                .governance
                .enacted_committee
                .as_ref()
                .map(|id| (id.transaction_id.as_ref().to_vec(), id.action_index)),
            enacted_constitution: ls
                .gov
                .governance
                .enacted_constitution
                .as_ref()
                .map(|id| (id.transaction_id.as_ref().to_vec(), id.action_index)),
            committee,
            constitution_url: ls
                .gov
                .governance
                .constitution
                .as_ref()
                .map(|c| c.anchor.url.clone())
                .unwrap_or_default(),
            constitution_hash: ls
                .gov
                .governance
                .constitution
                .as_ref()
                .map(|c| c.anchor.data_hash.as_ref().to_vec())
                .unwrap_or_else(|| vec![0u8; 32]),
            constitution_script: ls
                .gov
                .governance
                .constitution
                .as_ref()
                .and_then(|c| c.script_hash.as_ref().map(|h| h.as_ref().to_vec())),
            stake_addresses,
            stake_snapshots,
            pool_distr,
            pool_distr_total_active_stake,
            snap_mark,
            snap_set,
            snap_go,
            snap_fee: ls.epochs.snapshots.ss_fee.0,
            epoch_blocks_by_pool,
            pool_params_entries,
            pending_retirements: ls
                .certs
                .pending_retirements
                .iter()
                .map(|(pool_id, epoch)| (pool_id.as_ref().to_vec(), epoch.0))
                .collect(),
            pool_deposit: ls.epochs.protocol_params.pool_deposit.0,
            epoch_length: ls.epoch_length,
            slot_length_secs: self.shelley_genesis.as_ref().map_or(1, |g| g.slot_length),
            network_magic: self.network_magic as u32,
            security_param: self.consensus.security_param,
            stake_deleg_deposits,
            drep_stake_distr,
            vote_delegatees,
            drep_delegations,
            era_summaries: self.build_era_summaries(&ls, &eh),
            active_slots_coeff_num: self.shelley_genesis.as_ref().map_or(1, |g| {
                let (n, _) = float_to_rational(g.active_slots_coeff);
                n
            }),
            active_slots_coeff_den: self.shelley_genesis.as_ref().map_or(20, |g| {
                let (_, d) = float_to_rational(g.active_slots_coeff);
                d
            }),
            slots_per_kes_period: self
                .shelley_genesis
                .as_ref()
                .map_or(129600, |g| g.slots_per_k_e_s_period),
            max_kes_evolutions: self
                .shelley_genesis
                .as_ref()
                .map_or(62, |g| g.max_k_e_s_evolutions),
            update_quorum: self.shelley_genesis.as_ref().map_or(5, |g| g.update_quorum),
            max_lovelace_supply: self
                .shelley_genesis
                .as_ref()
                .map_or(45_000_000_000_000_000, |g| g.max_lovelace_supply),
            ratify_enacted,
            ratify_expired: ls
                .gov
                .governance
                .last_expired
                .iter()
                .map(|id| super::n2c_query::GovActionId {
                    tx_id: id.transaction_id.as_ref().to_vec(),
                    action_index: id.action_index,
                })
                .collect(),
            ratify_delayed: ls.gov.governance.last_ratify_delayed,
            epoch_nonce: ls.consensus.epoch_nonce.as_ref().to_vec(),
            previous_epoch_nonce: ls.consensus.previous_epoch_nonce.as_ref().to_vec(),
            last_epoch_block_nonce: ls.consensus.last_epoch_block_nonce.as_ref().to_vec(),
            opcert_counters: ls
                .consensus
                .opcert_counters
                .iter()
                .map(|(k, v)| (k.as_ref().to_vec(), *v))
                .collect(),
            evolving_nonce: ls.consensus.evolving_nonce.as_ref().to_vec(),
            candidate_nonce: ls.consensus.candidate_nonce.as_ref().to_vec(),
            lab_nonce: ls.consensus.lab_nonce.as_ref().to_vec(),
            total_active_stake: ls
                .certs
                .pool_params
                .keys()
                .filter_map(|pid| {
                    ls.epochs
                        .snapshots
                        .set
                        .as_ref()
                        .and_then(|s| s.pool_stake.get(pid))
                        .map(|s| s.0)
                })
                .sum(),
            total_rewards: ls.certs.reward_accounts.values().map(|r| r.0).sum(),
            active_delegations: ls.certs.delegations.len() as u64,
            protocol_version_major: ls.epochs.protocol_params.protocol_version_major,
            protocol_version_minor: ls.epochs.protocol_params.protocol_version_minor,
            genesis_config: self.shelley_genesis.as_ref().map(|g| {
                let gp = &g.protocol_params;
                // Convert a0 from f64 to rational
                let (a0_num, a0_den) = float_to_rational(gp.a0);
                let (rho_num, rho_den) = float_to_rational(gp.rho);
                let (tau_num, tau_den) = float_to_rational(gp.tau);
                let (asc_num, asc_den) = float_to_rational(g.active_slots_coeff);
                GenesisConfigSnapshot {
                    system_start: g.system_start.clone(),
                    network_magic: g.network_magic as u32,
                    network_id: if g.network_id == "Mainnet" { 1 } else { 0 },
                    active_slots_coeff_num: asc_num,
                    active_slots_coeff_den: asc_den,
                    security_param: g.security_param,
                    epoch_length: g.epoch_length,
                    slots_per_kes_period: g.slots_per_k_e_s_period,
                    max_kes_evolutions: g.max_k_e_s_evolutions,
                    slot_length_micros: g.slot_length * 1_000_000,
                    update_quorum: g.update_quorum,
                    max_lovelace_supply: g.max_lovelace_supply,
                    protocol_params: ShelleyPParamsSnapshot {
                        min_fee_a: gp.min_fee_a,
                        min_fee_b: gp.min_fee_b,
                        max_block_body_size: gp.max_block_body_size as u32,
                        max_tx_size: gp.max_tx_size as u32,
                        max_block_header_size: gp.max_block_header_size as u16,
                        key_deposit: gp.key_deposit,
                        pool_deposit: gp.pool_deposit,
                        e_max: gp.e_max as u32,
                        n_opt: gp.n_opt as u16,
                        a0_num,
                        a0_den,
                        rho_num,
                        rho_den,
                        tau_num,
                        tau_den,
                        d_num: 0,
                        d_den: 1,
                        protocol_version_major: gp.protocol_version.major,
                        protocol_version_minor: gp.protocol_version.minor,
                        min_utxo_value: gp.min_u_tx_o_value,
                        min_pool_cost: gp.min_pool_cost,
                    },
                    gen_delegs: g.gen_delegs_entries(),
                }
            }),
        };

        // Drop the ledger read lock before acquiring the query handler write lock
        drop(ls);

        let mut handler = self.query_handler.write().await;
        handler.update_state(snapshot);
    }

    /// Build era summaries for GetEraHistory responses.
    ///
    /// Converts the HFC `EraHistory` state machine entries into the N2C wire
    /// format `EraSummary` types for the `GetInterpreter` query response.
    pub fn build_era_summaries(
        &self,
        _ls: &dugite_ledger::LedgerState,
        eh: &dugite_consensus::era_history::EraHistory,
    ) -> Vec<super::n2c_query::EraSummary> {
        use super::n2c_query::{EraBound, EraSummary};
        eh.to_era_summary_exports()
            .into_iter()
            .map(|e| EraSummary {
                start_slot: e.start_slot,
                start_epoch: e.start_epoch,
                start_time_pico: e.start_time_pico,
                end: e.end.map(|b| EraBound {
                    slot: b.slot,
                    epoch: b.epoch,
                    time_pico: b.time_pico,
                }),
                epoch_size: e.epoch_size,
                slot_length_ms: e.slot_length_ms,
                safe_zone: e.safe_zone,
                genesis_window: e.genesis_window,
            })
            .collect()
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Return the canonical action-type string for a `GovAction`.
pub(crate) fn gov_action_type_str(
    action: &dugite_primitives::transaction::GovAction,
) -> &'static str {
    use dugite_primitives::transaction::GovAction;
    match action {
        GovAction::ParameterChange { .. } => "ParameterChange",
        GovAction::HardForkInitiation { .. } => "HardForkInitiation",
        GovAction::TreasuryWithdrawals { .. } => "TreasuryWithdrawals",
        GovAction::NoConfidence { .. } => "NoConfidence",
        GovAction::UpdateCommittee { .. } => "UpdateCommittee",
        GovAction::NewConstitution { .. } => "NewConstitution",
        GovAction::InfoAction => "InfoAction",
    }
}

/// Build a canonically-ordered, decode-safe list of `ProposalSnapshot`s from
/// a proposals map, its companion votes map, and the four enacted-purpose
/// roots active at that map's snapshot time.
///
/// Shared by both proposal views so their sort/pruning logic can never drift
/// apart:
///   * the LIVE view (`GetGovState`'s `ConwayGovState.cgsProposals`), fed
///     `ls.gov.governance.{proposals,votes_by_action,enacted_*}`.
///   * the FROZEN DRep-pulser view (`GetProposals`, tag 31, #922), fed the
///     equivalent fields off `RatificationSnapshot`.
///
/// Haskell `mkProposals` rebuilds the proposal forest by folding over the
/// OMap in insertion order: every child's `prev_action_id` must already be
/// either an enacted root or an ancestor present in the iteration so far.
/// Dugite stores proposals in a map keyed by `GovActionId` (hash order), so
/// this function re-derives submission order via `submission_index` (#799,
/// #906) and re-applies the same admission check Haskell's fold performs,
/// dropping any proposal whose `prev_action_id` doesn't resolve — this keeps
/// the wire response decodable when historical state outlives a root-update
/// event (see the inline comment on the `retain` call below).
#[allow(clippy::too_many_arguments)]
fn build_proposal_snapshot_list(
    proposals: &imbl::OrdMap<
        dugite_primitives::transaction::GovActionId,
        dugite_ledger::state::ProposalState,
    >,
    votes_by_action: &imbl::OrdMap<
        dugite_primitives::transaction::GovActionId,
        imbl::OrdMap<
            dugite_primitives::transaction::Voter,
            dugite_primitives::transaction::VotingProcedure,
        >,
    >,
    enacted_pparam_update: Option<&dugite_primitives::transaction::GovActionId>,
    enacted_hard_fork: Option<&dugite_primitives::transaction::GovActionId>,
    enacted_committee: Option<&dugite_primitives::transaction::GovActionId>,
    enacted_constitution: Option<&dugite_primitives::transaction::GovActionId>,
) -> Vec<super::n2c_query::ProposalSnapshot> {
    use super::n2c_query::ProposalSnapshot;
    use dugite_primitives::transaction::GovAction;

    // Haskell returns proposals in on-chain SUBMISSION order, not sorted by
    // GovActionId. `queryProposals` reads the pulser's `dpProposals` /
    // snapshot's `psProposals`, both of which come from
    // `proposalsActions = OMap.toStrictSeq pProps`, and `pProps` is appended
    // to (`OMap.||>`) by the GOV rule as each proposal is processed.
    //
    // dugite's `proposals` is an ImblOrdMap keyed by GovActionId, so its
    // natural iteration order is by hash. `submission_index` (#799) is the
    // monotonic counter that recovers submission order — the same field the
    // ratification tie-break already relies on. Sorting by (proposed_epoch,
    // txId) instead put the sequence in hash order within an epoch, which is
    // what made `proposals` and the proposals array inside `gov-state`
    // diverge from cardano-node (#906).
    let mut sorted_proposals: Vec<(
        &dugite_primitives::transaction::GovActionId,
        &dugite_ledger::state::ProposalState,
    )> = proposals.iter().collect();
    sorted_proposals.sort_by(|(a_id, a), (b_id, b)| {
        a.submission_index
            .cmp(&b.submission_index)
            .then_with(|| {
                a_id.transaction_id
                    .as_ref()
                    .cmp(b_id.transaction_id.as_ref())
            })
            .then_with(|| a_id.action_index.cmp(&b_id.action_index))
    });

    // Haskell's `mkProposals` invokes `proposalsAddAction` for every
    // GovActionState in the decoded OMap, and the fold bails the moment a
    // proposal's `prev_action_id` does not resolve to either:
    //   * the current enacted root for that purpose, or
    //   * an ancestor already inserted in this fold.
    //
    // Dugite's `apply_block` path inserts proposals into the map but does not
    // always prune stale siblings when a sibling of the same purpose enacts —
    // historical state from earlier epochs can outlive the root-update
    // event, leaving proposals whose `prev_action_id` points at a
    // now-superseded root. Sending those raw to cardano-cli surfaces as
    // `mkProposals: Could not add a proposal …` and aborts the entire decode.
    //
    // To keep the query response decodable we replay the same admission
    // check here and silently drop unresolvable proposals. This makes the
    // wire response self-consistent for `cardano-cli` while the underlying
    // source-of-truth bug (the sibling-cleanup gap on enactment) is tracked
    // separately.
    let enacted_roots: std::collections::HashSet<&dugite_primitives::transaction::GovActionId> = [
        enacted_pparam_update,
        enacted_hard_fork,
        enacted_committee,
        enacted_constitution,
    ]
    .into_iter()
    .flatten()
    .collect();
    let extract_prev = |a: &GovAction| -> Option<dugite_primitives::transaction::GovActionId> {
        match a {
            GovAction::ParameterChange { prev_action_id, .. }
            | GovAction::HardForkInitiation { prev_action_id, .. }
            | GovAction::NoConfidence { prev_action_id, .. }
            | GovAction::UpdateCommittee { prev_action_id, .. }
            | GovAction::NewConstitution { prev_action_id, .. } => prev_action_id.clone(),
            GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
        }
    };
    let mut admitted_ids: std::collections::HashSet<dugite_primitives::transaction::GovActionId> =
        std::collections::HashSet::new();
    sorted_proposals.retain(|(action_id, state)| {
        let prev = extract_prev(&state.procedure.gov_action);
        let ok = match prev {
            None => true, // Treasury/Info/root proposals always admitted.
            Some(ref p) => admitted_ids.contains(p) || enacted_roots.contains(p),
        };
        if ok {
            admitted_ids.insert((*action_id).clone());
        } else {
            tracing::debug!(
                action_id = %action_id.transaction_id.to_hex(),
                idx = action_id.action_index,
                prev = %prev.map(|p| p.transaction_id.to_hex()).unwrap_or_default(),
                "gov-state: dropping proposal with unresolved prev_action_id (stale sibling after root enactment)"
            );
        }
        ok
    });

    sorted_proposals
        .into_iter()
        .map(|(action_id, state)| {
            let action_type = gov_action_type_str(&state.procedure.gov_action);
            let (committee_votes, drep_votes, spo_votes) =
                build_vote_maps(votes_by_action, action_id);
            ProposalSnapshot {
                tx_id: action_id.transaction_id.as_ref().to_vec(),
                action_index: action_id.action_index,
                action_type: action_type.to_string(),
                proposed_epoch: state.proposed_epoch.0,
                expires_epoch: state.expires_epoch.0,
                yes_votes: state.yes_votes,
                no_votes: state.no_votes,
                abstain_votes: state.abstain_votes,
                deposit: state.procedure.deposit.0,
                return_addr: state.procedure.return_addr.clone(),
                anchor_url: state.procedure.anchor.url.clone(),
                anchor_hash: state.procedure.anchor.data_hash.as_ref().to_vec(),
                gov_action: state.procedure.gov_action.clone(),
                committee_votes,
                drep_votes,
                spo_votes,
            }
        })
        .collect()
}

/// Build per-credential committee/DRep/SPO vote vectors for a governance action.
///
/// `votes_by_action` is passed explicitly (rather than a `&LedgerState`) so
/// this can be sourced from either the LIVE `governance.votes_by_action` or a
/// FROZEN [`dugite_ledger::state::RatificationSnapshot::votes_by_action`] —
/// mirroring `count_votes_by_type`'s existing live/frozen duality and letting
/// `GetProposals` (#922) answer with the votes AS THEY WERE at the pulser
/// snapshot, not live votes cast after it.
#[allow(clippy::type_complexity)]
pub(crate) fn build_vote_maps(
    votes_by_action: &imbl::OrdMap<
        dugite_primitives::transaction::GovActionId,
        imbl::OrdMap<
            dugite_primitives::transaction::Voter,
            dugite_primitives::transaction::VotingProcedure,
        >,
    >,
    action_id: &dugite_primitives::transaction::GovActionId,
) -> (
    Vec<(Vec<u8>, u8, u8)>,
    Vec<(Vec<u8>, u8, u8)>,
    Vec<(Vec<u8>, u8)>,
) {
    use dugite_primitives::transaction::Voter;
    use std::collections::BTreeMap;
    // Haskell semantics: `proposalsAddVote` calls `Map.insert k vote`,
    // i.e. the latest vote from a given voter overwrites any prior vote
    // on the same governance action. Dugite stores votes as
    // `ImblOrdMap<Voter, VotingProcedure>` in `votes_by_action` (already
    // last-vote-wins per voter), so no duplicate voter can reach this point;
    // we still re-collect into per-credential `BTreeMap`s below to split by
    // voter role and produce a canonically-sorted, duplicate-free wire map.
    //
    // (A duplicate key on the wire would make Haskell's
    // `decodeMapEnforceNoDuplicates` fail with "Final number of elements:
    // <unique> does not match the total count that was decoded: <raw>",
    // rejecting the whole gov-state response as "Active proposals: 0".)
    let mut committee_map: BTreeMap<(Vec<u8>, u8), u8> = BTreeMap::new();
    let mut drep_map: BTreeMap<(Vec<u8>, u8), u8> = BTreeMap::new();
    let mut spo_map: BTreeMap<Vec<u8>, u8> = BTreeMap::new();
    if let Some(votes) = votes_by_action.get(action_id) {
        for (voter, procedure) in votes {
            let vote_u8 = match procedure.vote {
                dugite_primitives::transaction::Vote::No => 0u8,
                dugite_primitives::transaction::Vote::Yes => 1u8,
                dugite_primitives::transaction::Vote::Abstain => 2u8,
            };
            match voter {
                Voter::ConstitutionalCommittee(cred) => {
                    let (cred_type, hash) = credential_to_bytes(cred);
                    committee_map.insert((hash, cred_type), vote_u8);
                }
                Voter::DRep(cred) => {
                    let (cred_type, hash) = credential_to_bytes(cred);
                    drep_map.insert((hash, cred_type), vote_u8);
                }
                Voter::StakePool(pool_hash) => {
                    spo_map.insert(pool_hash.as_ref()[..28].to_vec(), vote_u8);
                }
            }
        }
    }
    let committee_votes = committee_map
        .into_iter()
        .map(|((hash, ct), v)| (hash, ct, v))
        .collect();
    let drep_votes = drep_map
        .into_iter()
        .map(|((hash, ct), v)| (hash, ct, v))
        .collect();
    let spo_votes = spo_map.into_iter().collect();
    (committee_votes, drep_votes, spo_votes)
}

/// Build a `ProtocolParamsSnapshot` from ledger protocol parameters.
///
/// Shared by the current-params snapshot and the previous-params snapshot
/// (`previousPParams` in `query gov-state`), which must be the parameters in
/// force BEFORE the most recent epoch-boundary enactment. Those used to be
/// filled in with the current params under a "best available" comment, so a
/// ParameterChange made gov-state's previousPParams wrong until the next
/// change happened to make it right again.
fn protocol_params_snapshot(
    pp: &dugite_primitives::protocol_params::ProtocolParameters,
) -> super::n2c_query::ProtocolParamsSnapshot {
    super::n2c_query::ProtocolParamsSnapshot {
        min_fee_a: pp.min_fee_a,
        min_fee_b: pp.min_fee_b,
        max_block_body_size: pp.max_block_body_size,
        max_tx_size: pp.max_tx_size,
        max_block_header_size: pp.max_block_header_size,
        key_deposit: pp.key_deposit.0,
        pool_deposit: pp.pool_deposit.0,
        e_max: pp.e_max,
        n_opt: pp.n_opt,
        a0_num: pp.a0.numerator,
        a0_den: pp.a0.denominator,
        rho_num: pp.rho.numerator,
        rho_den: pp.rho.denominator,
        tau_num: pp.tau.numerator,
        tau_den: pp.tau.denominator,
        min_pool_cost: pp.min_pool_cost.0,
        ada_per_utxo_byte: pp.ada_per_utxo_byte.0,
        cost_models_v1: pp.cost_models.plutus_v1.clone(),
        cost_models_v2: pp.cost_models.plutus_v2.clone(),
        cost_models_v3: pp.cost_models.plutus_v3.clone(),
        cost_models_v4: pp.cost_models.plutus_v4.clone(),
        cost_models_unknown: pp.cost_models.unknown_cost_models.clone(),
        execution_costs_mem_num: pp.execution_costs.mem_price.numerator,
        execution_costs_mem_den: pp.execution_costs.mem_price.denominator,
        execution_costs_step_num: pp.execution_costs.step_price.numerator,
        execution_costs_step_den: pp.execution_costs.step_price.denominator,
        max_tx_ex_mem: pp.max_tx_ex_units.mem,
        max_tx_ex_steps: pp.max_tx_ex_units.steps,
        max_block_ex_mem: pp.max_block_ex_units.mem,
        max_block_ex_steps: pp.max_block_ex_units.steps,
        max_val_size: pp.max_val_size,
        collateral_percentage: pp.collateral_percentage,
        max_collateral_inputs: pp.max_collateral_inputs,
        protocol_version_major: pp.protocol_version_major,
        protocol_version_minor: pp.protocol_version_minor,
        min_fee_ref_script_cost_per_byte_num: pp.min_fee_ref_script_cost_per_byte.numerator,
        min_fee_ref_script_cost_per_byte_den: pp.min_fee_ref_script_cost_per_byte.denominator,
        drep_deposit: pp.drep_deposit.0,
        drep_activity: pp.drep_activity,
        gov_action_deposit: pp.gov_action_deposit.0,
        gov_action_lifetime: pp.gov_action_lifetime,
        committee_min_size: pp.committee_min_size,
        committee_max_term_length: pp.committee_max_term_length,
        dvt_pp_network_group_num: pp.dvt_pp_network_group.numerator,
        dvt_pp_network_group_den: pp.dvt_pp_network_group.denominator,
        dvt_pp_economic_group_num: pp.dvt_pp_economic_group.numerator,
        dvt_pp_economic_group_den: pp.dvt_pp_economic_group.denominator,
        dvt_pp_technical_group_num: pp.dvt_pp_technical_group.numerator,
        dvt_pp_technical_group_den: pp.dvt_pp_technical_group.denominator,
        dvt_pp_gov_group_num: pp.dvt_pp_gov_group.numerator,
        dvt_pp_gov_group_den: pp.dvt_pp_gov_group.denominator,
        dvt_hard_fork_num: pp.dvt_hard_fork.numerator,
        dvt_hard_fork_den: pp.dvt_hard_fork.denominator,
        dvt_no_confidence_num: pp.dvt_no_confidence.numerator,
        dvt_no_confidence_den: pp.dvt_no_confidence.denominator,
        dvt_committee_normal_num: pp.dvt_committee_normal.numerator,
        dvt_committee_normal_den: pp.dvt_committee_normal.denominator,
        dvt_committee_no_confidence_num: pp.dvt_committee_no_confidence.numerator,
        dvt_committee_no_confidence_den: pp.dvt_committee_no_confidence.denominator,
        dvt_constitution_num: pp.dvt_constitution.numerator,
        dvt_constitution_den: pp.dvt_constitution.denominator,
        dvt_treasury_withdrawal_num: pp.dvt_treasury_withdrawal.numerator,
        dvt_treasury_withdrawal_den: pp.dvt_treasury_withdrawal.denominator,
        pvt_motion_no_confidence_num: pp.pvt_motion_no_confidence.numerator,
        pvt_motion_no_confidence_den: pp.pvt_motion_no_confidence.denominator,
        pvt_committee_normal_num: pp.pvt_committee_normal.numerator,
        pvt_committee_normal_den: pp.pvt_committee_normal.denominator,
        pvt_committee_no_confidence_num: pp.pvt_committee_no_confidence.numerator,
        pvt_committee_no_confidence_den: pp.pvt_committee_no_confidence.denominator,
        pvt_hard_fork_num: pp.pvt_hard_fork.numerator,
        pvt_hard_fork_den: pp.pvt_hard_fork.denominator,
        pvt_pp_security_group_num: pp.pvt_pp_security_group.numerator,
        pvt_pp_security_group_den: pp.pvt_pp_security_group.denominator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_ledger::state::DRepRegistration;
    use dugite_ledger::LedgerState;
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::transaction::{
        Anchor, Constitution, GovAction, GovActionId, Rational, Vote, Voter, VotingProcedure,
    };
    use dugite_primitives::{EpochNo, Lovelace};
    use std::sync::Arc;

    // ─── float_to_rational ───────────────────────────────────────────────────
    //
    // The genesis files store activeSlotsCoeff / a0 / rho / tau as decimals.
    // The N2C genesis-config response and several pparam fields require these
    // as `(num, den)` integer rationals — `float_to_rational` is the bridge.
    // Getting the bridge wrong silently changes pparams clients see.

    #[test]
    fn float_to_rational_handles_common_genesis_values() {
        // The exact values that ship in mainnet/preview shelley-genesis.json.
        assert_eq!(float_to_rational(0.0), (0, 1));
        assert_eq!(float_to_rational(1.0), (1, 1));
        assert_eq!(float_to_rational(0.05), (1, 20)); // active_slots_coeff
        assert_eq!(float_to_rational(0.3), (3, 10)); // a0
        assert_eq!(float_to_rational(0.003), (3, 1000)); // rho
        assert_eq!(float_to_rational(0.2), (1, 5)); // tau (simplified)
        assert_eq!(float_to_rational(0.5), (1, 2));
    }

    #[test]
    fn float_to_rational_simplifies_via_gcd() {
        // 0.4 = 4/10 → simplified to 2/5 (gcd = 2).
        assert_eq!(float_to_rational(0.4), (2, 5));
        // 0.25 = 25/100 → 1/4 (the search loop hits den=4 first).
        assert_eq!(float_to_rational(0.25), (1, 4));
    }

    #[test]
    fn float_to_rational_falls_back_for_unrepresentable_floats() {
        // A value with no exact small-denominator representation should still
        // produce a valid rational (the 1e6-denominator fallback path).
        // We don't assert exact (num, den) — the contract is: numeric value
        // matches the input, and denominator is non-zero.
        let (n, d) = float_to_rational(std::f64::consts::PI);
        assert!(d > 0);
        let approx = n as f64 / d as f64;
        assert!((approx - std::f64::consts::PI).abs() < 1e-3);
    }

    // ─── query_drep_expiry ───────────────────────────────────────────────────
    //
    // #912: `GetDRepState` reported `registered_epoch + drep_activity` instead
    // of the stored `drep_expiry`, and skipped the query-time dormant refund
    // that Haskell's `queryDRepState` applies.  `drepExpiry` gates DRep
    // activity, and an expired DRep drops out of the voting distribution, so a
    // divergence here can become a ratification divergence.

    /// A DRep whose stored expiry has moved away from `registered_epoch +
    /// drep_activity` — i.e. one that has since received an `UpdateDRep`
    /// certificate or cast a vote.  This is exactly the shape that #912
    /// mis-reported.
    fn drep_with(registered_epoch: u64, drep_expiry: u64) -> DRepRegistration {
        DRepRegistration {
            credential: Credential::VerificationKey(Hash28::from_bytes([0x11; 28])),
            deposit: Lovelace(500_000_000),
            anchor: None,
            registered_epoch: EpochNo(registered_epoch),
            drep_expiry: EpochNo(drep_expiry),
            active: true,
        }
    }

    #[test]
    fn query_drep_expiry_reports_stored_expiry_not_registration_epoch() {
        // Registered in epoch 1, then updated in epoch 2 with drep_activity=20
        // => stored expiry 22.  The old code answered 1 + 20 = 21.
        let drep = drep_with(1, 22);
        assert_eq!(
            query_drep_expiry(&drep, 0, EpochNo(3)),
            22,
            "UpdateDRep/vote must move the reported expiry"
        );
    }

    #[test]
    fn query_drep_expiry_adds_dormant_epochs_back() {
        // Haskell `queryDRepState` runs `updateDormantDRepExpiry` over a copy of
        // the VState, so quiet-governance epochs are refunded for reporting
        // even though the ledger has not refunded them yet.
        let drep = drep_with(1, 21);
        assert_eq!(query_drep_expiry(&drep, 1, EpochNo(3)), 22);
        assert_eq!(query_drep_expiry(&drep, 4, EpochNo(3)), 25);
    }

    #[test]
    fn query_drep_expiry_does_not_revive_an_already_expired_drep() {
        // Haskell: `if actualExpiry < currentEpoch then currentExpiry else actualExpiry`.
        // 5 + 2 = 7 < 20 => the refund is discarded, not applied.
        let drep = drep_with(1, 5);
        assert_eq!(query_drep_expiry(&drep, 2, EpochNo(20)), 5);
        // Boundary: `actual == current_epoch` is NOT "less than", so it applies.
        assert_eq!(query_drep_expiry(&drep, 2, EpochNo(7)), 7);
    }

    #[test]
    fn query_drep_expiry_is_defined_after_a_haskell_snapshot_import() {
        // A Mithril/Haskell-snapshot import cannot carry `registered_epoch` and
        // stores 0, but it does carry `drepExpiry` verbatim.  The old formula
        // answered `0 + drep_activity` for every DRep on such a database.
        let drep = drep_with(0, 431);
        assert_eq!(query_drep_expiry(&drep, 0, EpochNo(400)), 431);
    }

    #[test]
    fn query_drep_expiry_saturates_instead_of_overflowing() {
        let drep = drep_with(0, u64::MAX);
        assert_eq!(query_drep_expiry(&drep, 3, EpochNo(1)), u64::MAX);
    }

    // ─── credential_to_bytes ─────────────────────────────────────────────────
    //
    // Distinguishes key-hash (type=0) from script-hash (type=1) credentials in
    // the wire format used by GetGovState vote maps.  A flipped discriminator
    // would silently mis-categorize all script DReps in cardano-cli output.

    #[test]
    fn credential_to_bytes_discriminates_key_vs_script() {
        let key_hash = Hash28::from_bytes([0xAB; 28]);
        let cred = Credential::VerificationKey(key_hash);
        let (ty, bytes) = credential_to_bytes(&cred);
        assert_eq!(ty, 0);
        assert_eq!(bytes, vec![0xAB; 28]);

        let script_hash = Hash28::from_bytes([0xCD; 28]);
        let cred = Credential::Script(script_hash);
        let (ty, bytes) = credential_to_bytes(&cred);
        assert_eq!(ty, 1);
        assert_eq!(bytes, vec![0xCD; 28]);
    }

    // ─── gov_action_type_str ─────────────────────────────────────────────────
    //
    // String labels exposed via GetGovState — clients may match on these in
    // logs/dashboards, so the labels are part of our user-facing surface.

    #[test]
    fn gov_action_type_str_maps_each_variant() {
        let zero = Hash32::ZERO;
        // Construct one representative of each GovAction variant and pin its label.
        let cases: Vec<(GovAction, &str)> = vec![
            (
                GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::default(),
                    policy_hash: None,
                },
                "ParameterChange",
            ),
            (
                GovAction::HardForkInitiation {
                    prev_action_id: None,
                    protocol_version: (10, 0),
                },
                "HardForkInitiation",
            ),
            (
                GovAction::TreasuryWithdrawals {
                    withdrawals: Default::default(),
                    policy_hash: None,
                },
                "TreasuryWithdrawals",
            ),
            (
                GovAction::NoConfidence {
                    prev_action_id: None,
                },
                "NoConfidence",
            ),
            (
                GovAction::UpdateCommittee {
                    prev_action_id: None,
                    members_to_remove: Default::default(),
                    members_to_add: Default::default(),
                    threshold: Rational {
                        numerator: 2,
                        denominator: 3,
                    },
                },
                "UpdateCommittee",
            ),
            (
                GovAction::NewConstitution {
                    prev_action_id: None,
                    constitution: Constitution {
                        anchor: Anchor {
                            url: String::new(),
                            data_hash: zero,
                        },
                        script_hash: None,
                    },
                },
                "NewConstitution",
            ),
            (GovAction::InfoAction, "InfoAction"),
        ];
        for (action, expected) in cases {
            assert_eq!(gov_action_type_str(&action), expected, "{action:?}");
        }
    }

    // ─── build_vote_maps ─────────────────────────────────────────────────────
    //
    // build_vote_maps is the projection from the ledger's `votes_by_action`
    // map to the per-voter wire vectors consumed by GetGovState/GetRatifyState.
    // The function decides three things that are easy to get wrong:
    //  1. Voter type discrimination (CC vs DRep vs SPO)
    //  2. Vote-encoding (No=0, Yes=1, Abstain=2 — note the Yes/No swap)
    //  3. Hash truncation: SPO's StakePool(Hash32) is truncated to 28 bytes.
    // A single-action sentinel test catches silent breakage of any of those.

    #[test]
    fn build_vote_maps_classifies_voters_and_votes() {
        let mut ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x11; 32]),
            action_index: 3,
        };

        let cc_cred = Credential::VerificationKey(Hash28::from_bytes([0xC0; 28]));
        let drep_cred = Credential::Script(Hash28::from_bytes([0xD1; 28]));
        // SPO uses bare Hash32 (not a Credential).  We seed all 32 bytes with a
        // distinguishable pattern so the truncation assertion is meaningful.
        let mut spo_bytes = [0u8; 32];
        spo_bytes[..28].copy_from_slice(&[0xE2; 28]);
        spo_bytes[28..].copy_from_slice(&[0xFF; 4]); // padding bytes that must be discarded
        let spo_id = Hash32::from_bytes(spo_bytes);

        let v_yes = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };
        let v_no = VotingProcedure {
            vote: Vote::No,
            anchor: None,
        };
        let v_abstain = VotingProcedure {
            vote: Vote::Abstain,
            anchor: None,
        };

        {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            gov.votes_by_action.insert(
                action_id.clone(),
                vec![
                    (Voter::ConstitutionalCommittee(cc_cred.clone()), v_yes),
                    (Voter::DRep(drep_cred.clone()), v_no),
                    (Voter::StakePool(spo_id), v_abstain),
                ]
                .into(),
            );
        }

        let (cc, drep, spo) = build_vote_maps(&ledger.gov.governance.votes_by_action, &action_id);

        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0], (vec![0xC0; 28], 0u8, 1u8)); // VKey CC, Yes
        assert_eq!(drep.len(), 1);
        assert_eq!(drep[0], (vec![0xD1; 28], 1u8, 0u8)); // Script DRep, No
        assert_eq!(spo.len(), 1);
        // SPO hash is truncated to 28 bytes — the trailing 0xFF padding is
        // stripped, matching the Cardano wire format for StakePool key hashes.
        assert_eq!(spo[0], (vec![0xE2; 28], 2u8));
    }

    /// Regression test for issue #434.
    ///
    /// `votes_by_action`'s per-action votes are an `imbl::OrdMap<Voter, _>`
    /// (matching Haskell's `Map voter Vote`), so a re-vote by the same voter
    /// structurally overwrites the previous one (last-wins) — duplicates can no
    /// longer accumulate. Haskell's `proposalsAddVote` uses the same `Map.insert`
    /// semantics, and its CBOR decoder enforces no duplicate keys in the
    /// `gasDRepVotes` / `gasCommitteeVotes` / `gasStakePoolVotes` maps. Emitting
    /// duplicates on the wire causes cardano-cli to abort the whole `gov-state`
    /// response with `Final number of elements: <unique> does not match the
    /// total count that was decoded: <raw>`, surfacing as "0 active proposals".
    ///
    /// Building the map from a `Vec` with repeated voters must collapse them to
    /// the last observed vote (`OrdMap: From<Vec>` is last-wins), and
    /// `build_vote_maps` must then surface exactly one entry per voter.
    #[test]
    fn build_vote_maps_dedupes_repeat_voters_last_wins() {
        let mut ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x55; 32]),
            action_index: 0,
        };
        let drep_cred = Credential::VerificationKey(Hash28::from_bytes([0xAA; 28]));
        let cc_cred = Credential::VerificationKey(Hash28::from_bytes([0xBB; 28]));
        let mut spo_bytes = [0u8; 32];
        spo_bytes[..28].copy_from_slice(&[0xCC; 28]);
        let spo_id = Hash32::from_bytes(spo_bytes);
        let v_no = VotingProcedure {
            vote: Vote::No,
            anchor: None,
        };
        let v_yes = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };
        let v_abstain = VotingProcedure {
            vote: Vote::Abstain,
            anchor: None,
        };
        {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            gov.votes_by_action.insert(
                action_id.clone(),
                vec![
                    // Same DRep votes twice — second vote wins.
                    (Voter::DRep(drep_cred.clone()), v_no.clone()),
                    (Voter::DRep(drep_cred.clone()), v_yes.clone()),
                    // CC member votes three times — last wins.
                    (
                        Voter::ConstitutionalCommittee(cc_cred.clone()),
                        v_yes.clone(),
                    ),
                    (
                        Voter::ConstitutionalCommittee(cc_cred.clone()),
                        v_no.clone(),
                    ),
                    (
                        Voter::ConstitutionalCommittee(cc_cred.clone()),
                        v_abstain.clone(),
                    ),
                    // SPO votes twice — last wins.
                    (Voter::StakePool(spo_id), v_yes.clone()),
                    (Voter::StakePool(spo_id), v_no.clone()),
                ]
                .into(),
            );
        }
        let (cc, drep, spo) = build_vote_maps(&ledger.gov.governance.votes_by_action, &action_id);
        assert_eq!(drep.len(), 1, "drep duplicates must collapse to 1 entry");
        assert_eq!(drep[0], (vec![0xAA; 28], 0u8, 1u8), "last DRep vote=Yes");
        assert_eq!(cc.len(), 1, "cc duplicates must collapse to 1 entry");
        assert_eq!(cc[0], (vec![0xBB; 28], 0u8, 2u8), "last CC vote=Abstain");
        assert_eq!(spo.len(), 1, "spo duplicates must collapse to 1 entry");
        assert_eq!(spo[0], (vec![0xCC; 28], 0u8), "last SPO vote=No");
    }

    #[test]
    fn build_vote_maps_returns_empty_for_unknown_action() {
        // Sanity guard: an action id with no recorded votes must produce
        // empty vectors rather than panicking on the lookup.
        let ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let unknown = GovActionId {
            transaction_id: Hash32::from_bytes([0x99; 32]),
            action_index: 0,
        };
        let (cc, drep, spo) = build_vote_maps(&ledger.gov.governance.votes_by_action, &unknown);
        assert!(cc.is_empty());
        assert!(drep.is_empty());
        assert!(spo.is_empty());
    }

    // ─── build_proposal_snapshot_list (#922) ─────────────────────────────────
    //
    // `build_proposal_snapshot_list` is shared by the LIVE proposal view
    // (`GetGovState`, tag 24 — reads `ls.gov.governance.proposals` directly)
    // and the FROZEN DRep-pulser view (`GetProposals`, tag 31 — reads
    // `RatificationSnapshot::proposals`, refreshed only at epoch boundaries).
    // These tests exercise the function directly against synthetic
    // live/frozen maps to prove: (a) a mid-epoch submission present in the
    // live map is simply absent from a frozen map that predates it, and (b)
    // submission order (not `GovActionId`/hash order) is preserved even after
    // a proposal is removed (e.g. by enactment or expiry).

    fn info_action_proposal(
        submission_index: u64,
        proposed_epoch: u64,
    ) -> dugite_ledger::state::ProposalState {
        dugite_ledger::state::ProposalState {
            procedure: dugite_primitives::transaction::ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: Anchor {
                    url: "https://example.com/p".to_string(),
                    data_hash: Hash32::from_bytes([0xAA; 32]),
                },
            },
            proposed_epoch: EpochNo(proposed_epoch),
            expires_epoch: EpochNo(proposed_epoch + 6),
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
            submission_index,
        }
    }

    /// #922 core mechanism: a proposal present in the LIVE map but not yet
    /// folded into the FROZEN (pulser) map must not appear when
    /// `build_proposal_snapshot_list` is run against the frozen map — this is
    /// exactly what distinguishes `GetProposals` (frozen) from `GetGovState`
    /// (live) at the same tip.
    #[test]
    fn build_proposal_snapshot_list_mid_epoch_submission_invisible_in_frozen_map() {
        let empty_votes: imbl::OrdMap<GovActionId, imbl::OrdMap<Voter, VotingProcedure>> =
            imbl::OrdMap::new();

        let id_a = GovActionId {
            transaction_id: Hash32::from_bytes([0x01; 32]),
            action_index: 0,
        };
        let id_b_mid_epoch = GovActionId {
            transaction_id: Hash32::from_bytes([0x02; 32]),
            action_index: 0,
        };

        // Frozen (pulser) map as captured at the LAST epoch boundary: only
        // proposal A existed then.
        let mut frozen_proposals = imbl::OrdMap::new();
        frozen_proposals.insert(id_a.clone(), info_action_proposal(0, 100));
        let frozen =
            build_proposal_snapshot_list(&frozen_proposals, &empty_votes, None, None, None, None);
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen[0].tx_id, vec![0x01u8; 32]);

        // Live map: proposal B was submitted mid-epoch, after the pulser
        // snapshot was taken but before the next boundary.
        let mut live_proposals = frozen_proposals.clone();
        live_proposals.insert(id_b_mid_epoch.clone(), info_action_proposal(1, 100));
        let live =
            build_proposal_snapshot_list(&live_proposals, &empty_votes, None, None, None, None);
        assert_eq!(
            live.len(),
            2,
            "the live view (GetGovState) must see the mid-epoch submission immediately"
        );
        assert!(
            !frozen.iter().any(|p| p.tx_id == vec![0x02u8; 32]),
            "the frozen view (GetProposals) must NOT see it before the next epoch boundary"
        );

        // Simulate the epoch-boundary pulser refresh: the new frozen map is
        // rebuilt from what is now live, so the deferred proposal appears.
        let refreshed =
            build_proposal_snapshot_list(&live_proposals, &empty_votes, None, None, None, None);
        assert_eq!(
            refreshed.len(),
            2,
            "after the epoch boundary rotates the pulser, the deferred proposal must appear"
        );
    }

    /// Submission order — not `GovActionId` (hash) order — must be preserved,
    /// including after a proposal is removed from the map (e.g. by
    /// enactment/expiry at a prior boundary). This guards the #906 ordering
    /// fix for both the live and frozen call sites sharing this function.
    #[test]
    fn build_proposal_snapshot_list_preserves_submission_order_after_removal() {
        let empty_votes: imbl::OrdMap<GovActionId, imbl::OrdMap<Voter, VotingProcedure>> =
            imbl::OrdMap::new();

        // Three proposals submitted in order C, A, B (deliberately NOT hash
        // order: 0xCC > 0xAA in byte order, so a hash-order sort would put A
        // first).
        let id_c = GovActionId {
            transaction_id: Hash32::from_bytes([0xCC; 32]),
            action_index: 0,
        };
        let id_a = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 0,
        };
        let id_b = GovActionId {
            transaction_id: Hash32::from_bytes([0xBB; 32]),
            action_index: 0,
        };

        let mut proposals = imbl::OrdMap::new();
        proposals.insert(id_c.clone(), info_action_proposal(0, 100));
        proposals.insert(id_a.clone(), info_action_proposal(1, 100));
        proposals.insert(id_b.clone(), info_action_proposal(2, 100));

        let result = build_proposal_snapshot_list(&proposals, &empty_votes, None, None, None, None);
        assert_eq!(
            result.iter().map(|p| p.tx_id[0]).collect::<Vec<_>>(),
            vec![0xCC, 0xAA, 0xBB],
            "submission order must survive even though it's not hash order"
        );

        // Now remove the middle-submitted proposal (A) — as ratification
        // would after enactment/expiry — and confirm C, B keep their
        // relative submission order (no reordering/compaction artifacts).
        proposals.remove(&id_a);
        let after_removal =
            build_proposal_snapshot_list(&proposals, &empty_votes, None, None, None, None);
        assert_eq!(
            after_removal.iter().map(|p| p.tx_id[0]).collect::<Vec<_>>(),
            vec![0xCC, 0xBB]
        );
    }
}
