use super::LedgerState;
use dugite_primitives::hash::Hash32;
use dugite_primitives::transaction::Certificate;
use dugite_primitives::value::Lovelace;
use tracing::{debug, warn};
// Imports used ONLY by the `#[cfg(test)]` `process_certificate*` helpers (#813
// item 2 — those are gated out of release builds, so their imports must be too).
#[cfg(test)]
use super::{credential_to_hash, DRepRegistration, PoolRegistration};
#[cfg(test)]
use dugite_primitives::credentials::Credential;
#[cfg(test)]
use dugite_primitives::hash::Hash28;
#[cfg(test)]
use dugite_primitives::transaction::{MIRSource, MIRTarget};
#[cfg(test)]
use std::sync::Arc;

/// Returns true if the certificate is Conway-only and requires protocol version >= 9.
#[allow(dead_code)]
pub(crate) fn is_conway_only_certificate(cert: &Certificate) -> bool {
    matches!(
        cert,
        Certificate::RegDRep { .. }
            | Certificate::UnregDRep { .. }
            | Certificate::UpdateDRep { .. }
            | Certificate::VoteDelegation { .. }
            | Certificate::StakeVoteDelegation { .. }
            | Certificate::CommitteeHotAuth { .. }
            | Certificate::CommitteeColdResign { .. }
            | Certificate::RegStakeVoteDeleg { .. }
            | Certificate::VoteRegDeleg { .. }
            | Certificate::ConwayStakeRegistration { .. }
            | Certificate::ConwayStakeDeregistration { .. }
            | Certificate::RegStakeDeleg { .. }
    )
}

/// Drain the pending MIR (`dsIRewards`) map and apply it to reward
/// accounts and pots per Haskell `Cardano.Ledger.Shelley.Rules.Mir.mirTransition`
/// (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Mir.hs`).
///
/// Called at every epoch boundary AFTER SNAP/POOLREAP and BEFORE NEWPP
/// (matching the Haskell EPOCH STS sequence).  In Conway+ MIR certs
/// are no longer reachable, so the pending maps stay empty and the
/// function is a no-op.
///
/// Per Haskell (issue #803 — cardano-ledger-oracle-verified, see
/// `.claude/agent-memory/cardano-ledger-oracle/mir-pot-transfer-semantics.md`):
/// 1. Filter `dsIRewards.irwdSrcReserves` / `irwdSrcTreasury` to currently
///    REGISTERED credentials (`Map.intersection accountsMap`) — payments to
///    deregistered credentials never count and are discarded, not refunded.
/// 2. `totR` / `totT` = the (filtered) per-credential deltas summed per pot.
/// 3. Fold the pot-to-pot transfer accumulator into an "available" balance
///    per pot (Haskell: `reserves \`addDeltaCoin\` deltaReserves`, own-pot
///    delta only — no cross term in Haskell's *single signed* accumulator).
///    dugite instead tracks the two transfer directions as independent
///    non-negative magnitudes (`pending_mir_delta_reserves` = reserves→treasury,
///    `pending_mir_delta_treasury` = treasury→reserves) rather than one
///    signed `deltaReserves = -deltaTreasury` pair, so translated into
///    dugite's fields the equivalent formula is
///    `available_reserves = reserves - dr + dt` and
///    `available_treasury = treasury + dr - dt` (this is NOT a Haskell cross
///    term — it falls out of dugite's two-magnitude encoding of the same
///    net signed value).
/// 4. All-or-nothing solvency: `totR <= available_reserves && totT <=
///    available_treasury`. On failure (`NoMirTransfer`): leave BOTH pots
///    byte-identical, still drop all four pending accumulators, warn — never
///    panic, never partially apply. This is a total, non-throwing STS
///    (`PredicateFailure (MIR era) = Void`).
/// 5. On success (`MirTransfer`): apply exactly as before — credit each
///    registered credential, debit `totR`/`totT` from their source pots,
///    then move `dr`/`dt` between pots.
///
/// Defense-in-depth beyond the Haskell boundary check (issue #803): Haskell
/// itself does not re-verify per-credential non-negativity at this
/// boundary (that is enforced by the earlier, separate DELEG-time
/// `MIRProducesNegativeUpdate` / `InsufficientForInstantaneousRewards`
/// checks — see `validation/mir.rs`, which documents known Phase-1
/// admission gaps for this). Since dugite cannot yet guarantee those
/// checks always ran, this function also requires every registered
/// credential's resulting balance to stay non-negative before committing;
/// any violation is folded into the same `NoMirTransfer` no-op path rather
/// than panicking. This never changes behavior on valid history (the
/// condition is vacuously true whenever Phase-1 admission is correct).
pub(crate) fn apply_pending_mir(
    certs: &mut super::substates::CertSubState,
    epochs: &mut super::substates::EpochSubState,
) {
    let pending_reserves = std::mem::take(&mut certs.pending_mir_reserves);
    let pending_treasury = std::mem::take(&mut certs.pending_mir_treasury);
    let dr = std::mem::take(&mut certs.pending_mir_delta_reserves);
    let dt = std::mem::take(&mut certs.pending_mir_delta_treasury);

    if pending_reserves.is_empty() && pending_treasury.is_empty() && dr == 0 && dt == 0 {
        return;
    }

    // Step 1: filter to registered credentials only (Map.intersection).
    let filtered_reserves: Vec<(Hash32, i128)> = pending_reserves
        .into_iter()
        .filter(|(cred, _)| certs.reward_accounts.contains_key(cred))
        .collect();
    let filtered_treasury: Vec<(Hash32, i128)> = pending_treasury
        .into_iter()
        .filter(|(cred, _)| certs.reward_accounts.contains_key(cred))
        .collect();

    // Step 2: totR / totT over the filtered deltas.
    let tot_r: i128 = filtered_reserves.iter().map(|(_, d)| *d).sum();
    let tot_t: i128 = filtered_treasury.iter().map(|(_, d)| *d).sum();

    // Step 3: fold pot-to-pot accumulators into availability (see doc comment
    // above for the dugite-representation derivation of these formulas).
    let available_reserves = epochs.reserves.0 as i128 - dr + dt;
    let available_treasury = epochs.treasury.0 as i128 + dr - dt;

    // A credential may appear in BOTH the reserves and treasury maps; its
    // reward balance moves by the SUM of the two deltas. Haskell credits
    // accounts via the union (`Map.unionWith (+)`) of the two reward maps,
    // in one step. Fold to the per-credential net delta here so both the
    // non-negativity check (Step 4) and the credit (Step 5) operate on that
    // net: checking or applying each map independently against the
    // pre-apply balance lets a credential that is individually solvent in
    // each map but jointly negative pass the guard and then wrap `as u64`
    // in Step 5's sequential apply (issue #803 follow-up).
    let mut combined_deltas: std::collections::HashMap<Hash32, i128> =
        std::collections::HashMap::with_capacity(filtered_reserves.len() + filtered_treasury.len());
    for (cred, delta) in filtered_reserves.iter().chain(filtered_treasury.iter()) {
        *combined_deltas.entry(*cred).or_insert(0) += delta;
    }

    // Step 4: all-or-nothing solvency, plus the defensive per-credential
    // non-negativity check described above (over the combined net delta).
    let solvent = tot_r <= available_reserves && tot_t <= available_treasury;
    let all_non_negative = combined_deltas.iter().all(|(cred, delta)| {
        let existing = certs
            .reward_accounts
            .get(cred)
            .map(|l| l.0 as i128)
            .unwrap_or(0);
        existing + delta >= 0
    });

    if !solvent || !all_non_negative {
        warn!(
            tot_r,
            tot_t,
            available_reserves,
            available_treasury,
            solvent,
            all_non_negative,
            "MIR: NoMirTransfer — insolvent or would drive a credential negative; \
             pots left unchanged, pending MIR maps cleared (issue #803, matches \
             Haskell Mir.hs mirTransition's non-throwing NoMirTransfer path)"
        );
        // Pending maps were already drained via `mem::take` above, matching
        // Haskell's unconditional `dsIRewardsL .~ emptyInstantaneousRewards`
        // (fires on BOTH branches). Pots are left untouched.
        return;
    }

    // Step 5: apply the per-credential *combined* net delta exactly once,
    // so a credential present in both maps never transiently wraps through
    // an intermediate negative `as u64`. Byte-identical to the previous
    // per-map sequential path for all valid (solvent) history — associativity
    // of the two credits, which never underflow on correct history. Iteration
    // order over the map is irrelevant: each credential is assigned exactly
    // once from its own pre-apply balance.
    for (cred, delta) in &combined_deltas {
        if let Some(entry) = certs.reward_accounts.get_mut(cred) {
            entry.0 = (entry.0 as i128 + delta) as u64;
        }
    }
    // Solvency above guarantees these are all non-negative once combined —
    // compute the final pot values in i128 and cast once, rather than
    // stepping through intermediate u64 subtractions that could underflow
    // even though the net result is safe.
    epochs.reserves.0 = (available_reserves - tot_r) as u64;
    epochs.treasury.0 = (available_treasury - tot_t) as u64;
}

impl LedgerState {
    /// Process a certificate with pointer tracking for Pointer address resolution.
    ///
    /// StakeRegistration certificates create entries in the pointer_map,
    /// mapping (slot, tx_index, cert_index) → credential hash. This enables
    /// resolution of Pointer addresses (type 4/5) in stake_credential_hash.
    // TEST-ONLY (#813 item 2): every caller lives in a `#[cfg(test)]` module,
    // so this is gated out of release builds entirely rather than left as
    // `#[allow(dead_code)]`. It APPROXIMATES certificate application for tests;
    // the authoritative consensus path is `eras::common::apply_shelley_cert` /
    // `eras::conway::apply_conway_cert`. Do not treat it as a behaviour-parity
    // oracle for the live path. Fully re-pointing the ~236 test call sites onto
    // the live dispatch is deferred as high-risk/low-value for a test-only helper.
    #[cfg(test)]
    pub(crate) fn process_certificate_with_pointer(
        &mut self,
        cert: &Certificate,
        slot: u64,
        tx_index: u64,
        cert_index: u64,
    ) {
        // Populate pointer_map for StakeRegistration certificates
        if let Certificate::StakeRegistration(credential)
        | Certificate::ConwayStakeRegistration {
            credential,
            deposit: _,
        } = cert
        {
            let key = credential_to_hash(credential);
            let pointer = dugite_primitives::credentials::Pointer {
                slot,
                tx_index,
                cert_index,
            };
            self.certs.pointer_map.insert(pointer, key);
        }
        // Also handle combined registration certificates
        if let Certificate::RegStakeDeleg { credential, .. }
        | Certificate::RegStakeVoteDeleg { credential, .. }
        | Certificate::VoteRegDeleg { credential, .. } = cert
        {
            let key = credential_to_hash(credential);
            let pointer = dugite_primitives::credentials::Pointer {
                slot,
                tx_index,
                cert_index,
            };
            self.certs.pointer_map.insert(pointer, key);
        }

        // Delegate to the existing process_certificate for the actual state updates
        self.process_certificate(cert);
    }

    /// Process a certificate and update the ledger state accordingly.
    ///
    /// Certificates are applied unconditionally during block application.
    /// Era-gating (e.g., Conway-only certs in pre-Conway era) is a Phase-1
    /// tx validation rule, not a block application rule. The block producer
    /// already validated era compatibility. During replay, the in-state
    /// protocol version may lag behind the block's actual era.
    // TEST-ONLY (#813 item 2): gated to test builds (all callers are in
    // `#[cfg(test)]` modules). Authoritative live path is
    // `eras::common::apply_shelley_cert` / `eras::conway::apply_conway_cert`.
    #[cfg(test)]
    pub(crate) fn process_certificate(&mut self, cert: &Certificate) {
        match cert {
            Certificate::StakeRegistration(credential) => {
                let key = credential_to_hash(credential);
                self.certs
                    .stake_distribution
                    .stake_map
                    .entry(key)
                    .or_insert(Lovelace(0));
                self.certs.reward_accounts.entry(key).or_insert(Lovelace(0));
                // Track script credentials so N2C query responses can set credential_type correctly.
                if matches!(credential, Credential::Script(_)) {
                    self.certs.script_stake_credentials.insert(key);
                }
                self.certs.total_stake_key_deposits += self.epochs.protocol_params.key_deposit.0;
                self.certs
                    .stake_key_deposits
                    .insert(key, self.epochs.protocol_params.key_deposit.0);
                debug!("Stake key registered: {}", key.to_hex());
            }
            Certificate::StakeDeregistration(credential) => {
                let key = credential_to_hash(credential);
                // Do NOT remove from stake_distribution.stake_map — the credential
                // may still have UTxOs. The stake_map is a UTxO accounting structure;
                // deregistration is a delegation-layer concept. The ground truth
                // (rebuild_stake_distribution) sums ALL UTxOs by credential regardless
                // of registration status.
                // Use the stored deposit for correct refund when key_deposit changes.
                let stored_deposit = self
                    .certs
                    .stake_key_deposits
                    .remove(&key)
                    .unwrap_or(self.epochs.protocol_params.key_deposit.0);
                self.certs.total_stake_key_deposits = self
                    .certs
                    .total_stake_key_deposits
                    .saturating_sub(stored_deposit);
                self.certs.delegations.remove(&key);
                self.certs.reward_accounts.remove(&key);
                // Remove DRep delegation — Haskell's unified map clears all credential
                // data on deregistration, including vote delegations.
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .remove(&key);
                self.certs.script_stake_credentials.remove(&key);
                // Remove pointer entries for this credential
                self.certs.pointer_map.retain(|_, v| *v != key);
                debug!("Stake key deregistered: {}", key.to_hex());
            }
            Certificate::ConwayStakeRegistration {
                credential,
                deposit: _,
            } => {
                // Conway cert tag 7: same behavior as StakeRegistration
                let key = credential_to_hash(credential);
                self.certs
                    .stake_distribution
                    .stake_map
                    .entry(key)
                    .or_insert(Lovelace(0));
                self.certs.reward_accounts.entry(key).or_insert(Lovelace(0));
                if matches!(credential, Credential::Script(_)) {
                    self.certs.script_stake_credentials.insert(key);
                }
                self.certs.total_stake_key_deposits += self.epochs.protocol_params.key_deposit.0;
                self.certs
                    .stake_key_deposits
                    .insert(key, self.epochs.protocol_params.key_deposit.0);
                debug!("Stake key registered (Conway): {}", key.to_hex());
            }
            Certificate::ConwayStakeDeregistration {
                credential,
                refund: _,
            } => {
                // Conway cert tag 8: deregistration refunds the stored deposit.
                // Phase-1 validation enforces that the reward balance is zero
                // (StakeKeyHasNonZeroAccountBalanceDELEG) before this point.
                // Remove from delegations/rewards but keep the stake_map entry —
                // UTxOs may still exist at this credential.
                let key = credential_to_hash(credential);
                // Use the stored deposit for correct refund when key_deposit changes.
                let stored_deposit = self
                    .certs
                    .stake_key_deposits
                    .remove(&key)
                    .unwrap_or(self.epochs.protocol_params.key_deposit.0);
                self.certs.total_stake_key_deposits = self
                    .certs
                    .total_stake_key_deposits
                    .saturating_sub(stored_deposit);
                self.certs.delegations.remove(&key);
                self.certs.reward_accounts.remove(&key);
                // Remove DRep delegation — Haskell's unified map clears all credential
                // data on deregistration, including vote delegations.
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .remove(&key);
                self.certs.script_stake_credentials.remove(&key);
                // Remove pointer entries for this credential (matching StakeDeregistration).
                // Even though ptr_stake is empty in Conway, the pointer_map should reflect
                // the actual registration state for correctness.
                self.certs.pointer_map.retain(|_, v| *v != key);
                debug!("Stake key deregistered (Conway): {}", key.to_hex());
            }
            Certificate::StakeDelegation {
                credential,
                pool_hash,
            } => {
                let key = credential_to_hash(credential);
                self.certs.delegations.insert(key, *pool_hash);
                debug!("Stake delegated to pool: {}", pool_hash.to_hex());
            }
            Certificate::PoolRegistration(params) => {
                let pool_reg = PoolRegistration {
                    pool_id: params.operator,
                    vrf_keyhash: params.vrf_keyhash,
                    pledge: params.pledge,
                    cost: params.cost,
                    margin_numerator: params.margin.numerator,
                    margin_denominator: params.margin.denominator,
                    reward_account: params.reward_account.clone(),
                    owners: params.pool_owners.clone(),
                    relays: params.relays.clone(),
                    metadata_url: params.pool_metadata.as_ref().map(|m| m.url.clone()),
                    metadata_hash: params.pool_metadata.as_ref().map(|m| m.hash),
                };
                // If the pool is re-registering, cancel any pending retirement
                // and store new params in future_pool_params (applied at next epoch
                // boundary, matching Haskell's POOL STS futurePoolParams mechanism).
                // First registrations go directly to pool_params.
                if self.certs.pool_params.contains_key(&params.operator) {
                    // Cancel any pending retirement (matching Haskell's
                    // psRetiringL %~ Map.delete sppId).
                    self.certs.pending_retirements.remove(&params.operator);
                    // Re-registration: defer to future_pool_params
                    self.certs
                        .future_pool_params
                        .insert(params.operator, pool_reg);
                    debug!(
                        "Pool re-registered (deferred to next epoch, pending retirement cancelled): {}",
                        params.operator.to_hex()
                    );
                } else {
                    // First registration: apply immediately and record deposit.
                    Arc::make_mut(&mut self.certs.pool_params).insert(params.operator, pool_reg);
                    self.certs
                        .pool_deposits
                        .insert(params.operator, self.epochs.protocol_params.pool_deposit.0);
                    debug!("Pool registered: {}", params.operator.to_hex());
                }
            }
            Certificate::PoolRetirement { pool_hash, epoch } => {
                // Apply the retirement unconditionally. The e_max check
                // (retirement_epoch <= current_epoch + e_max) is a Phase-1
                // transaction validation rule, NOT a block application rule.
                // Blocks already on-chain have passed validation — re-checking
                // during replay with the wrong "current epoch" causes false
                // rejections and ledger state divergence.
                debug!(
                    "Pool retirement scheduled at epoch {}: {}",
                    epoch,
                    pool_hash.to_hex()
                );
                // Insert or replace the retirement epoch for this pool.
                // Haskell: psRetiringL %~ Map.insert sppId epoch
                // A second retirement for the same pool replaces the first.
                self.certs
                    .pending_retirements
                    .insert(*pool_hash, dugite_primitives::time::EpochNo(*epoch));
            }
            Certificate::RegStakeDeleg {
                credential,
                pool_hash,
                ..
            } => {
                let key = credential_to_hash(credential);
                self.certs
                    .stake_distribution
                    .stake_map
                    .entry(key)
                    .or_insert(Lovelace(0));
                self.certs.reward_accounts.entry(key).or_insert(Lovelace(0));
                self.certs.delegations.insert(key, *pool_hash);
                self.certs.total_stake_key_deposits += self.epochs.protocol_params.key_deposit.0;
                self.certs
                    .stake_key_deposits
                    .insert(key, self.epochs.protocol_params.key_deposit.0);
                if matches!(credential, Credential::Script(_)) {
                    self.certs.script_stake_credentials.insert(key);
                }
            }
            Certificate::RegDRep {
                credential,
                deposit,
                anchor,
            } => {
                let key = credential_to_hash(credential);
                let expiry = self.compute_drep_expiry();
                Arc::make_mut(&mut self.gov.governance).dreps.insert(
                    key,
                    DRepRegistration {
                        credential: credential.clone(),
                        deposit: *deposit,
                        anchor: anchor.clone(),
                        registered_epoch: self.epoch,
                        drep_expiry: expiry,
                        active: true,
                        delegs: Default::default(),
                    },
                );
                Arc::make_mut(&mut self.gov.governance).drep_registration_count += 1;
                debug!("DRep registered: {}", key.to_hex());
            }
            Certificate::UnregDRep {
                credential,
                refund: _,
            } => {
                // Per `Cardano.Ledger.Conway.Rules.GovCert` (`ConwayUnRegDRep`):
                // the only state mutation is removing `cred` from `vsDReps` and
                // clearing the DRep delegation field of every account that had
                // delegated to this DRep.  The deposit refund is **NOT** credited
                // to any reward account — it appears on the `consumed` side of
                // the tx balance equation (via `conwayTotalRefundsTxCerts`) and
                // is routed wherever the tx body's outputs send it.
                //
                // The previous implementation here unconditionally credited
                // `reward_accounts[credential_hash] += deposit` and used
                // `or_insert(Lovelace(0))` which fabricated a phantom reward
                // account whenever the DRep credential was never separately
                // registered as a stake credential. This violates the Conway
                // GOVCERT semantics and inflates total ADA in the ledger state.
                //
                // (This `process_certificate` path is not currently invoked from
                // the production block-apply pipeline — that path runs
                // `apply_conway_cert` in `eras/conway.rs`, which has always
                // matched Haskell. The fix here brings the legacy helper in line
                // so any future caller can rely on identical semantics.)
                //
                // Permalink:
                // https://github.com/IntersectMBO/cardano-ledger/blob/master/eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs
                let key = credential_to_hash(credential);
                let drep_state = Arc::make_mut(&mut self.gov.governance).dreps.remove(&key);

                // Clear the DRep delegation field for each delegator of this
                // DRep.  Haskell's `clearDRepDelegations` uses `Map.adjust`,
                // which is a no-op for absent keys — do not create entries.
                if drep_state.is_some() {
                    let drep_id_key = dugite_primitives::transaction::DRep::KeyHash(key);
                    let drep_id_script =
                        dugite_primitives::transaction::DRep::ScriptHash(*credential.to_hash());
                    Arc::make_mut(&mut self.gov.governance)
                        .vote_delegations
                        .retain(|_, drep| drep != &drep_id_key && drep != &drep_id_script);
                }
                debug!("DRep deregistered: {}", key.to_hex());
            }
            Certificate::UpdateDRep { credential, anchor } => {
                let key = credential_to_hash(credential);
                let expiry = self.compute_drep_expiry();
                if let Some(drep) = Arc::make_mut(&mut self.gov.governance).dreps.get_mut(&key) {
                    drep.anchor = anchor.clone();
                    drep.drep_expiry = expiry;
                    debug!("DRep updated: {}", key.to_hex());
                }
            }
            Certificate::VoteDelegation { credential, drep } => {
                let key = credential_to_hash(credential);
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .insert(key, drep.clone());
                debug!("Vote delegated to {:?}", drep);
            }
            Certificate::StakeVoteDelegation {
                credential,
                pool_hash,
                drep,
            } => {
                let key = credential_to_hash(credential);
                // Stake delegation
                self.certs.delegations.insert(key, *pool_hash);
                // Vote delegation
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .insert(key, drep.clone());
                debug!(
                    "Stake+vote delegated to pool {} and drep {:?}",
                    pool_hash.to_hex(),
                    drep
                );
            }
            Certificate::CommitteeHotAuth {
                cold_credential,
                hot_credential,
            } => {
                let cold_key = credential_to_hash(cold_credential);
                let hot_key = credential_to_hash(hot_credential);
                let gov = Arc::make_mut(&mut self.gov.governance);
                gov.committee_hot_keys.insert(cold_key, hot_key);
                // NOTE: Do NOT remove from committee_resigned here. Resignation is
                // permanent per Haskell's checkAndOverwriteCommitteeMemberState.
                // ConwayCommitteeHasPreviouslyResigned rejects this cert at validation.
                // Track script cold credentials for correct cold_credential_type in N2C responses.
                if matches!(cold_credential, Credential::Script(_)) {
                    gov.script_committee_credentials.insert(cold_key);
                }
                // Track script hot credentials for correct hot_credential_type in N2C responses
                // (GetCommitteeState tag 27).
                //
                // The set is keyed by hot credential hash.  When querying, we resolve the
                // current hot key for a cold key via committee_hot_keys, then probe this set.
                // Therefore stale entries from a superseded hot key can never be reached:
                // once committee_hot_keys[cold_key] points to a new hot key hash, the old
                // hash is simply never looked up again.  There is no need to remove the
                // displaced hash here.
                if matches!(hot_credential, Credential::Script(_)) {
                    gov.script_committee_hot_credentials.insert(hot_key);
                }
                debug!(
                    "Committee hot key authorized: {} -> {}",
                    cold_key.to_hex(),
                    hot_key.to_hex()
                );
            }
            Certificate::CommitteeColdResign {
                cold_credential,
                anchor,
            } => {
                let cold_key = credential_to_hash(cold_credential);
                let gov = Arc::make_mut(&mut self.gov.governance);
                gov.committee_resigned.insert(cold_key, anchor.clone());
                gov.committee_hot_keys.remove(&cold_key);
                // Track script cold credentials for correct credential_type in N2C responses.
                if matches!(cold_credential, Credential::Script(_)) {
                    gov.script_committee_credentials.insert(cold_key);
                }
                debug!("Committee member resigned: {}", cold_key.to_hex());
            }
            Certificate::RegStakeVoteDeleg {
                credential,
                pool_hash,
                drep,
                ..
            } => {
                let key = credential_to_hash(credential);
                // Register stake credential
                self.certs
                    .stake_distribution
                    .stake_map
                    .entry(key)
                    .or_insert(Lovelace(0));
                self.certs.reward_accounts.entry(key).or_insert(Lovelace(0));
                // Stake delegation
                self.certs.delegations.insert(key, *pool_hash);
                // Vote delegation
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .insert(key, drep.clone());
                self.certs.total_stake_key_deposits += self.epochs.protocol_params.key_deposit.0;
                self.certs
                    .stake_key_deposits
                    .insert(key, self.epochs.protocol_params.key_deposit.0);
                if matches!(credential, Credential::Script(_)) {
                    self.certs.script_stake_credentials.insert(key);
                }
                debug!(
                    "Reg+stake+vote delegated: pool={}, drep={:?}",
                    pool_hash.to_hex(),
                    drep
                );
            }
            Certificate::VoteRegDeleg {
                credential, drep, ..
            } => {
                let key = credential_to_hash(credential);
                // Register stake credential
                self.certs
                    .stake_distribution
                    .stake_map
                    .entry(key)
                    .or_insert(Lovelace(0));
                self.certs.reward_accounts.entry(key).or_insert(Lovelace(0));
                // Vote delegation
                Arc::make_mut(&mut self.gov.governance)
                    .vote_delegations
                    .insert(key, drep.clone());
                self.certs.total_stake_key_deposits += self.epochs.protocol_params.key_deposit.0;
                self.certs
                    .stake_key_deposits
                    .insert(key, self.epochs.protocol_params.key_deposit.0);
                if matches!(credential, Credential::Script(_)) {
                    self.certs.script_stake_credentials.insert(key);
                }
                debug!("Reg+vote delegated to {:?}", drep);
            }
            Certificate::GenesisKeyDelegation {
                genesis_hash,
                genesis_delegate_hash,
                vrf_keyhash,
            } => {
                // Shelley-era genesis key delegation. Update the active gen-delegate
                // mapping directly. This function (`process_certificate`) is test-only
                // dead code (no production callers) and intentionally keeps the
                // simplified immediate-apply model.
                //
                // Haskell actually models this as a two-phase queue
                // (`dsFutureGenDelegs` -> `dsGenDelegs`, matured after
                // `stability_window` slots — ceil(3k/f), NOT doubled; the
                // "2 * stability_window" figure that appears elsewhere in the
                // ledger is a different mechanism, the PPUP/HFC "point of no
                // return" deadline). The LIVE apply path implements the real
                // two-phase queue in `eras::common::enqueue_genesis_key_delegations`
                // / `adopt_matured_genesis_delegs` (see issue #804); this
                // dead handler is NOT observationally equivalent to that and
                // must not be treated as a reference for production behavior.
                //
                // The cert fields (genesis_hash, genesis_delegate_hash) are stored as
                // Hash32 in our enum (zero-padded from the on-wire 28-byte hashes),
                // while genesis_delegates uses Hash28 keys — truncate to first 28 bytes.
                let gkey = Hash28::from_bytes({
                    let mut buf = [0u8; 28];
                    buf.copy_from_slice(&genesis_hash.as_bytes()[..28]);
                    buf
                });
                let dkey = Hash28::from_bytes({
                    let mut buf = [0u8; 28];
                    buf.copy_from_slice(&genesis_delegate_hash.as_bytes()[..28]);
                    buf
                });
                self.genesis_delegates.insert(gkey, (dkey, *vrf_keyhash));
                debug!(
                    "Genesis key delegation applied: {} -> delegate={}, vrf={}",
                    genesis_hash.to_hex(),
                    genesis_delegate_hash.to_hex(),
                    vrf_keyhash.to_hex()
                );
            }
            Certificate::MoveInstantaneousRewards { source, target } => {
                // Per Haskell `Cardano.Ledger.Shelley.Rules.Mir.applyMIRCert`
                // MIR certs do NOT credit reward_accounts or move pots during
                // LEDGER STS — they accumulate into the `InstantaneousRewards`
                // (`dsIRewards`) pending-delta map on `DState`. The actual
                // application happens at the next epoch boundary via the
                // MIR sub-rule of EPOCH.  See issue #631.
                match target {
                    MIRTarget::StakeCredentials(creds) => {
                        // Haskell `Cardano.Ledger.Shelley.Rules.Deleg.hs` `applyMIRCert`:
                        //   pvMajor <= 4 (Shelley/Allegra/Mary): `Map.union credCoinMap' ir`
                        //     — left-biased → later cert for same credential OVERWRITES (last-wins).
                        //   pvMajor >  4 (Alonzo+): `Map.unionWith (<>) credCoinMap' ir`
                        //     — additive: amounts for the same credential are summed.
                        // Guard: `hardforkAlonzoAllowMIRTransfer pv = pvMajor pv > natVersion @4`
                        let pv = self.epochs.protocol_params.protocol_version_major;
                        let additive = pv > 4;
                        let pending = match source {
                            MIRSource::Reserves => &mut self.certs.pending_mir_reserves,
                            MIRSource::Treasury => &mut self.certs.pending_mir_treasury,
                        };
                        for (cred, amount) in creds {
                            let key = credential_to_hash(cred);
                            if additive {
                                let entry = pending.entry(key).or_insert(0i128);
                                *entry += *amount as i128;
                            } else {
                                // Last-wins: overwrite any previous entry for this credential.
                                pending.insert(key, *amount as i128);
                            }
                            debug!(
                                "MIR: pending {} lovelace from {:?} to {} ({})",
                                amount,
                                source,
                                key.to_hex(),
                                if additive { "additive" } else { "last-wins" }
                            );
                        }
                    }
                    MIRTarget::OtherAccountingPot(coin) => {
                        // Pot-to-pot transfer: accumulate the delta, apply at
                        // epoch boundary.  Per Haskell `dsIRewards . deltaReserves`
                        // / `deltaTreasury`.
                        match source {
                            MIRSource::Reserves => {
                                self.certs.pending_mir_delta_reserves += *coin as i128;
                                debug!(
                                    "MIR: pending {} lovelace pot-transfer reserves -> treasury",
                                    coin
                                );
                            }
                            MIRSource::Treasury => {
                                self.certs.pending_mir_delta_treasury += *coin as i128;
                                debug!(
                                    "MIR: pending {} lovelace pot-transfer treasury -> reserves",
                                    coin
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Process a withdrawal from a reward account.
    /// Per Cardano spec, the withdrawal amount must exactly match the reward balance.
    /// After withdrawal, the balance is reduced by the withdrawal amount.
    #[allow(dead_code)]
    pub(crate) fn process_withdrawal(&mut self, reward_account: &[u8], amount: Lovelace) {
        let key = Self::reward_account_to_hash(reward_account);
        if let Some(balance) = self.certs.reward_accounts.get_mut(&key) {
            // Per Cardano spec, withdrawal amount must exactly equal the reward balance.
            // During sync from genesis, we may not have accumulated all rewards yet,
            // so we only warn and process as best-effort.
            if balance.0 != amount.0 {
                debug!(
                    account = %key.to_hex(),
                    balance = balance.0,
                    withdrawal = amount.0,
                    "Withdrawal amount does not match reward balance"
                );
            }
            // Always process the withdrawal: set balance to 0
            // (rewards were consumed in the on-chain transaction)
            balance.0 = 0;
        }
    }

    /// Convert a reward account (raw bytes with network header) to a Hash32 key.
    ///
    /// Reward addresses are 29 bytes: 1 byte network header + 28 byte credential hash.
    /// We extract exactly the 28-byte credential and zero-pad to 32 bytes for Hash32.
    pub fn reward_account_to_hash(reward_account: &[u8]) -> Hash32 {
        let mut key_bytes = [0u8; 32];
        if reward_account.len() >= 29 {
            // Copy exactly 28 bytes of the credential (skip the 1-byte header)
            key_bytes[..28].copy_from_slice(&reward_account[1..29]);
            // Encode credential type from the header byte:
            // Bit 4 of the header: 0 = key hash, 1 = script hash
            // Reward address headers: 0xe0/0xe1 = key, 0xf0/0xf1 = script
            if reward_account[0] & 0x10 != 0 {
                key_bytes[28] = 0x01; // script credential
            }
        }
        Hash32::from_bytes(key_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{credential_to_hash, LedgerState};
    use dugite_primitives::credentials::{Credential, Pointer};
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{Anchor, Certificate, DRep, PoolParams, Rational};
    use dugite_primitives::value::Lovelace;
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// A deterministic 28-byte key credential for use in tests.
    fn test_credential() -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([0x01u8; 28]))
    }

    /// A second credential used when a test needs two distinct credentials.
    fn test_credential_2() -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([0x02u8; 28]))
    }

    /// A deterministic pool hash used in pool-related tests.
    fn test_pool_hash() -> Hash28 {
        Hash28::from_bytes([0xAAu8; 28])
    }

    /// Minimal valid PoolParams for pool registration tests.
    fn test_pool_params(operator: Hash28) -> PoolParams {
        PoolParams {
            operator,
            vrf_keyhash: Hash32::from_bytes([0xBBu8; 32]),
            pledge: Lovelace(500_000_000),
            cost: Lovelace(340_000_000),
            margin: Rational {
                numerator: 1,
                denominator: 20,
            },
            reward_account: vec![0xe0u8; 29], // key-type reward address placeholder
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        }
    }

    /// Create a fresh `LedgerState` with mainnet default protocol parameters.
    fn make_state() -> LedgerState {
        LedgerState::new(ProtocolParameters::mainnet_defaults())
    }

    // -----------------------------------------------------------------------
    // Test 1 — StakeRegistration
    // -----------------------------------------------------------------------

    /// Processing a StakeRegistration certificate must:
    ///  - insert the credential hash into `reward_accounts` with a zero balance;
    ///  - insert the same hash into `stake_key_deposits` with the current
    ///    `key_deposit` amount (2 ADA = 2_000_000 lovelace on mainnet).
    #[test]
    fn test_stake_registration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred));

        // reward_accounts: key present, balance = 0
        assert_eq!(
            state.certs.reward_accounts.get(&key).copied(),
            Some(Lovelace(0)),
            "reward_accounts should contain the registered credential with zero balance"
        );
        // stake_key_deposits: key present, amount = key_deposit
        assert_eq!(
            state.certs.stake_key_deposits.get(&key).copied(),
            Some(state.epochs.protocol_params.key_deposit.0),
            "stake_key_deposits should record the 2 ADA deposit"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2 — StakeDeregistration
    // -----------------------------------------------------------------------

    /// Deregistering a previously-registered credential must:
    ///  - remove the entry from `delegations`;
    ///  - remove the entry from `reward_accounts`;
    ///  - leave `stake_distribution.stake_map` intact (UTxOs may still exist);
    ///  - remove the entry from `stake_key_deposits`.
    #[test]
    fn test_stake_deregistration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Pre-register
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        // Also add a delegation so we can verify it gets removed
        state.certs.delegations.insert(key, test_pool_hash());
        // Manually plant a stake_map entry (simulating a UTxO at this credential)
        state
            .certs
            .stake_distribution
            .stake_map
            .insert(key, Lovelace(5_000_000));

        // Now deregister
        state.process_certificate(&Certificate::StakeDeregistration(cred));

        // delegations: removed
        assert!(
            !state.certs.delegations.contains_key(&key),
            "delegation should be removed on deregistration"
        );
        // reward_accounts: removed
        assert!(
            !state.certs.reward_accounts.contains_key(&key),
            "reward_accounts entry should be removed on deregistration"
        );
        // stake_map: NOT removed — UTxOs can still exist
        assert!(
            state.certs.stake_distribution.stake_map.contains_key(&key),
            "stake_map should NOT be removed on deregistration (UTxOs may still exist)"
        );
        // stake_key_deposits: removed
        assert!(
            !state.certs.stake_key_deposits.contains_key(&key),
            "stake_key_deposits should be removed after deregistration"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3 — StakeDelegation
    // -----------------------------------------------------------------------

    /// A StakeDelegation certificate must insert `pool_hash` into
    /// `delegations` keyed by the credential hash.
    #[test]
    fn test_stake_delegation() {
        let mut state = make_state();
        let cred = test_credential();
        let pool = test_pool_hash();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeDelegation {
            credential: cred,
            pool_hash: pool,
        });

        assert_eq!(
            state.certs.delegations.get(&key).copied(),
            Some(pool),
            "delegations should map the credential hash to the target pool"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4 — PoolRegistration (first registration)
    // -----------------------------------------------------------------------

    /// Registering a pool for the first time must:
    ///  - insert the pool into `pool_params` immediately;
    ///  - record the pool deposit in `pool_deposits`.
    #[test]
    fn test_pool_registration() {
        let mut state = make_state();
        let operator = test_pool_hash();
        let params = test_pool_params(operator);

        state.process_certificate(&Certificate::PoolRegistration(params));

        assert!(
            state.certs.pool_params.contains_key(&operator),
            "pool_params should contain the newly registered pool"
        );
        assert_eq!(
            state.certs.pool_deposits.get(&operator).copied(),
            Some(state.epochs.protocol_params.pool_deposit.0),
            "pool_deposits should record the 500 ADA deposit"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5 — PoolRegistration re-registration (staged)
    // -----------------------------------------------------------------------

    /// Re-registering an existing pool must defer the new parameters to
    /// `future_pool_params` (applied at the next epoch boundary) and must
    /// NOT record an additional deposit entry.
    #[test]
    fn test_pool_reregistration_staged() {
        let mut state = make_state();
        let operator = test_pool_hash();
        let params = test_pool_params(operator);

        // First registration — goes to pool_params directly
        state.process_certificate(&Certificate::PoolRegistration(params.clone()));
        let initial_deposit = state.certs.pool_deposits.get(&operator).copied();

        // Second registration — must be deferred
        let mut updated_params = params;
        updated_params.pledge = Lovelace(1_000_000_000);
        state.process_certificate(&Certificate::PoolRegistration(updated_params.clone()));

        // New params must appear in future_pool_params
        assert!(
            state.certs.future_pool_params.contains_key(&operator),
            "re-registration should stage params in future_pool_params"
        );
        // pool_deposits must not gain a new entry (deposit was already paid)
        assert_eq!(
            state.certs.pool_deposits.get(&operator).copied(),
            initial_deposit,
            "re-registration should not record a second deposit"
        );
        // The staged pledge must match what we submitted
        assert_eq!(
            state
                .certs
                .future_pool_params
                .get(&operator)
                .map(|r| r.pledge),
            Some(Lovelace(1_000_000_000)),
            "future_pool_params should hold the updated pledge"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6 — PoolRetirement
    // -----------------------------------------------------------------------

    /// A PoolRetirement certificate must add the pool to `pending_retirements`
    /// at the requested epoch.
    #[test]
    fn test_pool_retirement() {
        let mut state = make_state();
        let pool = test_pool_hash();
        let retirement_epoch: u64 = 500;

        state.process_certificate(&Certificate::PoolRetirement {
            pool_hash: pool,
            epoch: retirement_epoch,
        });

        assert_eq!(
            state.certs.pending_retirements.get(&pool).copied(),
            Some(EpochNo(retirement_epoch)),
            "pending_retirements should contain the pool at the requested epoch"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7 — ConwayStakeRegistration (deposit from cert, not key_deposit)
    // -----------------------------------------------------------------------

    /// A ConwayStakeRegistration with an explicit deposit must store the
    /// current `key_deposit` in `stake_key_deposits`, NOT the explicit cert
    /// deposit amount.  (The production code stores `protocol_params.key_deposit`
    /// for this cert variant, matching Haskell's Conway DELEG rule: the deposit
    /// recorded in the UMap is always the current key_deposit, independent of
    /// the explicit cert field.)
    #[test]
    fn test_conway_stake_registration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Emit a Conway registration cert with a different explicit deposit
        state.process_certificate(&Certificate::ConwayStakeRegistration {
            credential: cred,
            deposit: Lovelace(3_000_000),
        });

        // The deposit stored is key_deposit (2 ADA), not the cert's 3 ADA
        assert_eq!(
            state.certs.stake_key_deposits.get(&key).copied(),
            Some(state.epochs.protocol_params.key_deposit.0),
            "stake_key_deposits should store key_deposit, not the cert's explicit deposit"
        );
        // reward_accounts must be populated
        assert!(
            state.certs.reward_accounts.contains_key(&key),
            "reward_accounts should contain the registered credential"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8 — ConwayStakeDeregistration
    // -----------------------------------------------------------------------

    /// Deregistering via a Conway cert must behave identically to classic
    /// deregistration: delegations and reward_accounts are removed.
    #[test]
    fn test_conway_stake_deregistration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Pre-register using Conway cert
        state.process_certificate(&Certificate::ConwayStakeRegistration {
            credential: cred.clone(),
            deposit: Lovelace(2_000_000),
        });
        state.certs.delegations.insert(key, test_pool_hash());

        // Deregister
        state.process_certificate(&Certificate::ConwayStakeDeregistration {
            credential: cred,
            refund: Lovelace(2_000_000),
        });

        assert!(
            !state.certs.delegations.contains_key(&key),
            "delegation should be removed after ConwayStakeDeregistration"
        );
        assert!(
            !state.certs.reward_accounts.contains_key(&key),
            "reward_accounts entry should be removed after ConwayStakeDeregistration"
        );
        assert!(
            !state.certs.stake_key_deposits.contains_key(&key),
            "stake_key_deposits should be removed after ConwayStakeDeregistration"
        );
    }

    // -----------------------------------------------------------------------
    // Test 9 — RegDRep (DRep registration)
    // -----------------------------------------------------------------------

    /// Registering a DRep must insert a DRepRegistration entry into
    /// `governance.dreps` with the correct deposit.
    #[test]
    fn test_drep_registration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let deposit = Lovelace(500_000_000);

        state.process_certificate(&Certificate::RegDRep {
            credential: cred,
            deposit,
            anchor: None,
        });

        let gov = &state.gov.governance;
        let drep = gov
            .dreps
            .get(&key)
            .expect("governance.dreps should contain the registered DRep");

        assert_eq!(
            drep.deposit, deposit,
            "DRep deposit should match the cert value"
        );
        assert!(drep.active, "newly-registered DRep should be active");
    }

    // -----------------------------------------------------------------------
    // Test 10 — UnregDRep (DRep deregistration)
    // -----------------------------------------------------------------------

    /// Deregistering a DRep must remove the entry from `governance.dreps` and
    /// MUST NOT touch any reward account balance. Per Haskell
    /// `Cardano.Ledger.Conway.Rules.GovCert::ConwayUnRegDRep`, the deposit is
    /// returned via the tx balance equation (`conwayTotalRefundsTxCerts` adds
    /// it to the `consumed` side) — the GOVCERT rule itself only mutates the
    /// DRep registry and clears DRep delegations of voters. Issue #685.
    #[test]
    fn test_drep_unregistration() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let deposit = Lovelace(500_000_000);

        // Register first
        state.process_certificate(&Certificate::RegDRep {
            credential: cred.clone(),
            deposit,
            anchor: None,
        });
        // Also register as a stake key so the reward_accounts entry exists at zero.
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        let before = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));

        // Deregister DRep
        state.process_certificate(&Certificate::UnregDRep {
            credential: cred,
            refund: deposit,
        });

        // dreps entry must be gone
        assert!(
            !state.gov.governance.dreps.contains_key(&key),
            "governance.dreps should not contain the deregistered DRep"
        );
        // reward account balance must be UNCHANGED (the deposit goes back via
        // the tx balance equation, not via the GOVCERT rule).
        let after = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            after, before,
            "UnregDRep must NOT credit the DRep credential's reward account \
             (Haskell GOVCERT rule does not touch dsAccounts)"
        );
    }

    /// Regression for the phantom-reward-account bug: when the DRep credential
    /// has no separately-registered stake account, UnregDRep must NOT create
    /// one. Issue #685 — Haskell `Map.adjust` is a no-op on missing keys; our
    /// previous `entry(...).or_insert(0)` fabricated a zero-balance phantom
    /// account that polluted snapshot stake distributions.
    #[test]
    fn test_drep_unregistration_does_not_create_phantom_reward_account() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let deposit = Lovelace(500_000_000);

        // Register DRep WITHOUT a corresponding stake registration. No
        // reward_accounts entry exists at this point.
        state.process_certificate(&Certificate::RegDRep {
            credential: cred.clone(),
            deposit,
            anchor: None,
        });
        assert!(
            !state.certs.reward_accounts.contains_key(&key),
            "precondition: no reward account before UnregDRep"
        );

        state.process_certificate(&Certificate::UnregDRep {
            credential: cred,
            refund: deposit,
        });

        assert!(
            !state.certs.reward_accounts.contains_key(&key),
            "UnregDRep must not fabricate a reward account entry"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11 — UpdateDRep
    // -----------------------------------------------------------------------

    /// UpdateDRep must update the anchor on the DRep's registration record
    /// without changing the deposit.
    #[test]
    fn test_drep_update() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let deposit = Lovelace(500_000_000);

        // Register first
        state.process_certificate(&Certificate::RegDRep {
            credential: cred.clone(),
            deposit,
            anchor: None,
        });

        let new_anchor = Anchor {
            url: "https://example.com/drep-metadata.json".to_string(),
            data_hash: Hash32::from_bytes([0xCCu8; 32]),
        };

        state.process_certificate(&Certificate::UpdateDRep {
            credential: cred,
            anchor: Some(new_anchor.clone()),
        });

        let drep = state
            .gov
            .governance
            .dreps
            .get(&key)
            .expect("DRep should still be registered after UpdateDRep");

        assert_eq!(
            drep.anchor.as_ref(),
            Some(&new_anchor),
            "DRep anchor should be updated"
        );
        assert_eq!(
            drep.deposit, deposit,
            "DRep deposit must be unchanged after update"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12 — VoteDelegation
    // -----------------------------------------------------------------------

    /// A VoteDelegation cert must insert the DRep into
    /// `governance.vote_delegations` keyed by the credential hash.
    #[test]
    fn test_vote_delegation() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::VoteDelegation {
            credential: cred,
            drep: DRep::Abstain,
        });

        assert_eq!(
            state.gov.governance.vote_delegations.get(&key).cloned(),
            Some(DRep::Abstain),
            "governance.vote_delegations should map the credential to DRep::Abstain"
        );
    }

    // -----------------------------------------------------------------------
    // Test 13 — CommitteeHotAuth
    // -----------------------------------------------------------------------

    /// A CommitteeHotAuth cert must insert the hot credential hash into
    /// `governance.committee_hot_keys` keyed by the cold credential hash.
    #[test]
    fn test_committee_hot_auth() {
        let mut state = make_state();
        let cold_cred = test_credential();
        let hot_cred = test_credential_2();
        let cold_key = credential_to_hash(&cold_cred);
        let hot_key = credential_to_hash(&hot_cred);

        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold_cred,
            hot_credential: hot_cred,
        });

        assert_eq!(
            state
                .gov
                .governance
                .committee_hot_keys
                .get(&cold_key)
                .copied(),
            Some(hot_key),
            "committee_hot_keys should map cold credential hash to hot credential hash"
        );
    }

    // -----------------------------------------------------------------------
    // Test 14 — CommitteeColdResign
    // -----------------------------------------------------------------------

    /// A CommitteeColdResign cert must insert the cold credential hash into
    /// `governance.committee_resigned` and remove any hot key mapping.
    #[test]
    fn test_committee_cold_resign() {
        let mut state = make_state();
        let cold_cred = test_credential();
        let hot_cred = test_credential_2();
        let cold_key = credential_to_hash(&cold_cred);

        // First authorize a hot key so we can verify it gets cleared
        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold_cred.clone(),
            hot_credential: hot_cred,
        });
        assert!(
            state
                .gov
                .governance
                .committee_hot_keys
                .contains_key(&cold_key),
            "hot key should be present before resignation"
        );

        // Now resign
        state.process_certificate(&Certificate::CommitteeColdResign {
            cold_credential: cold_cred,
            anchor: None,
        });

        // committee_resigned must contain the cold key
        assert!(
            state
                .gov
                .governance
                .committee_resigned
                .contains_key(&cold_key),
            "governance.committee_resigned should contain the cold credential hash"
        );
        // committee_hot_keys must no longer contain the cold key
        assert!(
            !state
                .gov
                .governance
                .committee_hot_keys
                .contains_key(&cold_key),
            "committee_hot_keys should be cleared on resignation"
        );
    }

    // -----------------------------------------------------------------------
    // Test 15 — process_certificate_with_pointer
    // -----------------------------------------------------------------------

    /// Processing a StakeRegistration via `process_certificate_with_pointer`
    /// must create a pointer_map entry for (slot, tx_index, cert_index) →
    /// credential_hash.
    #[test]
    fn test_pointer_address_tracking() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        let slot: u64 = 100;
        let tx_index: u64 = 2;
        let cert_index: u64 = 0;

        state.process_certificate_with_pointer(
            &Certificate::StakeRegistration(cred),
            slot,
            tx_index,
            cert_index,
        );

        let expected_pointer = Pointer {
            slot,
            tx_index,
            cert_index,
        };

        assert_eq!(
            state.certs.pointer_map.get(&expected_pointer).copied(),
            Some(key),
            "pointer_map should map the (slot, tx_index, cert_index) pointer to the credential hash"
        );
        // The standard registration side-effects must also occur
        assert!(
            state.certs.reward_accounts.contains_key(&key),
            "reward_accounts should be populated by process_certificate_with_pointer"
        );
    }

    // -----------------------------------------------------------------------
    // Test 16 — CommitteeHotAuth does not clear resigned state (#381)
    // -----------------------------------------------------------------------

    /// Resignation must be permanent: a subsequent CommitteeHotAuth must NOT
    /// remove the cold key from `committee_resigned`.  Haskell's
    /// `checkAndOverwriteCommitteeMemberState` rejects the cert outright via
    /// `ConwayCommitteeHasPreviouslyResigned`; Dugite enforces the same
    /// invariant at the validation layer (validation/mod.rs:1038-1066) and
    /// must NOT undo it during state application.
    #[test]
    fn committee_resignation_is_permanent() {
        let mut state = make_state();
        let cold_cred = Credential::VerificationKey(Hash28::from_bytes([0xCC; 28]));
        let hot_cred1 = Credential::VerificationKey(Hash28::from_bytes([0xAA; 28]));

        // Put cold key in committee expiration so CommitteeHotAuth has a target.
        let cold_key = credential_to_hash(&cold_cred);
        let gov = Arc::make_mut(&mut state.gov.governance);
        gov.committee_expiration.insert(cold_key, EpochNo(100));

        // Authorize hot key.
        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold_cred.clone(),
            hot_credential: hot_cred1,
        });
        assert!(
            state
                .gov
                .governance
                .committee_hot_keys
                .contains_key(&cold_key),
            "committee_hot_keys should contain cold key after CommitteeHotAuth"
        );

        // Resign.
        state.process_certificate(&Certificate::CommitteeColdResign {
            cold_credential: cold_cred.clone(),
            anchor: None,
        });
        assert!(
            state
                .gov
                .governance
                .committee_resigned
                .contains_key(&cold_key),
            "committee_resigned should contain cold key after CommitteeColdResign"
        );

        // Attempt re-authorization — resigned set must NOT be cleared.
        let hot_cred2 = Credential::VerificationKey(Hash28::from_bytes([0xBB; 28]));
        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold_cred.clone(),
            hot_credential: hot_cred2,
        });
        assert!(
            state.gov.governance.committee_resigned.contains_key(&cold_key),
            "Committee resignation must be permanent — resigned set should not be cleared by CommitteeHotAuth"
        );
    }

    // -----------------------------------------------------------------------
    // Test 17 — double StakeRegistration (idempotent — or_insert semantics)
    // -----------------------------------------------------------------------

    /// Registering the same stake credential twice must NOT overwrite an
    /// existing reward balance.  The `entry().or_insert()` pattern ensures
    /// idempotency.  Deposit accounting DOES double-count (matching Haskell
    /// — a second duplicate registration in the same tx is a Phase-1 error;
    /// this test documents the state-application behaviour).
    #[test]
    fn test_double_stake_registration_reward_idempotent() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        // Manually credit some rewards to prove or_insert doesn't reset them.
        state
            .certs
            .reward_accounts
            .entry(key)
            .and_modify(|b| b.0 = 5_000_000);

        state.process_certificate(&Certificate::StakeRegistration(cred));

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 5_000_000,
            "Second StakeRegistration must not overwrite existing reward balance"
        );
    }

    // -----------------------------------------------------------------------
    // Test 18 — deregistration of a non-existent stake key (graceful)
    // -----------------------------------------------------------------------

    /// Deregistering a credential that was never registered must not panic.
    /// The stored-deposit lookup falls back to the current key_deposit,
    /// total_stake_key_deposits saturating-subtracts (no underflow), and
    /// there is nothing to remove from delegations/reward_accounts.
    #[test]
    fn test_stake_deregistration_of_nonexistent_key() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let initial_total = state.certs.total_stake_key_deposits;

        state.process_certificate(&Certificate::StakeDeregistration(cred));

        assert_eq!(
            state.certs.total_stake_key_deposits, initial_total,
            "total_stake_key_deposits should not underflow on dereg of unregistered key"
        );
        assert!(
            !state.certs.reward_accounts.contains_key(&key),
            "reward_accounts should still not contain unregistered key"
        );
    }

    // -----------------------------------------------------------------------
    // Test 19 — total_stake_key_deposits accounting across register/dereg
    // -----------------------------------------------------------------------

    /// Registering N credentials must increment total_stake_key_deposits by
    /// N × key_deposit; deregistering them brings it back to zero.
    #[test]
    fn test_total_stake_key_deposits_accounting() {
        let mut state = make_state();
        let deposit = state.epochs.protocol_params.key_deposit.0;

        let creds: Vec<Credential> = (0u8..5)
            .map(|i| Credential::VerificationKey(Hash28::from_bytes([i; 28])))
            .collect();

        for c in &creds {
            state.process_certificate(&Certificate::StakeRegistration(c.clone()));
        }
        assert_eq!(
            state.certs.total_stake_key_deposits,
            5 * deposit,
            "After 5 registrations, total_stake_key_deposits must equal 5 × key_deposit"
        );

        for c in &creds {
            state.process_certificate(&Certificate::StakeDeregistration(c.clone()));
        }
        assert_eq!(
            state.certs.total_stake_key_deposits, 0,
            "After deregistering all 5, total_stake_key_deposits must be 0"
        );
    }

    // -----------------------------------------------------------------------
    // Test 20 — deposit uses stored amount even if key_deposit changes
    // -----------------------------------------------------------------------

    /// When key_deposit changes after registration, deregistration must refund
    /// the originally-paid deposit, not the new protocol parameter value.
    #[test]
    fn test_deregistration_uses_stored_deposit_not_current_param() {
        let mut state = make_state();
        let original_deposit = state.epochs.protocol_params.key_deposit.0;
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        assert_eq!(
            state.certs.stake_key_deposits.get(&key).copied(),
            Some(original_deposit)
        );
        assert_eq!(state.certs.total_stake_key_deposits, original_deposit);

        // Simulate a protocol parameter update that doubles key_deposit.
        let new_deposit = original_deposit * 2;
        state.epochs.protocol_params.key_deposit.0 = new_deposit;

        state.process_certificate(&Certificate::StakeDeregistration(cred));

        assert_eq!(
            state.certs.total_stake_key_deposits,
            0,
            "Must subtract the originally-stored deposit ({original_deposit}), not the new value ({new_deposit})"
        );
    }

    // -----------------------------------------------------------------------
    // Test 21 — pool retirement at future epoch is stored, not applied
    // -----------------------------------------------------------------------

    /// PoolRetirement must record the future epoch in `pending_retirements`
    /// and not remove the pool from `pool_params`.
    #[test]
    fn test_pool_retirement_future_epoch() {
        let mut state = make_state();
        let pool_id = test_pool_hash();
        let params = test_pool_params(pool_id);

        state.process_certificate(&Certificate::PoolRegistration(params));
        assert!(state.certs.pool_params.contains_key(&pool_id));

        // Schedule retirement at epoch 999 (far future).
        state.process_certificate(&Certificate::PoolRetirement {
            pool_hash: pool_id,
            epoch: 999,
        });

        assert_eq!(
            state.certs.pending_retirements.get(&pool_id).map(|e| e.0),
            Some(999),
            "Retirement epoch must be stored in pending_retirements"
        );
        assert!(
            state.certs.pool_params.contains_key(&pool_id),
            "Pool must still be in pool_params — retirement is deferred"
        );
    }

    // -----------------------------------------------------------------------
    // Test 22 — second PoolRetirement replaces the first
    // -----------------------------------------------------------------------

    /// A later PoolRetirement for the same pool must replace the earlier one
    /// (Haskell: `Map.insert sppId epoch`).
    #[test]
    fn test_pool_retirement_replaced_by_later_cert() {
        let mut state = make_state();
        let pool_id = test_pool_hash();
        let params = test_pool_params(pool_id);
        state.process_certificate(&Certificate::PoolRegistration(params));

        state.process_certificate(&Certificate::PoolRetirement {
            pool_hash: pool_id,
            epoch: 10,
        });
        state.process_certificate(&Certificate::PoolRetirement {
            pool_hash: pool_id,
            epoch: 20,
        });

        assert_eq!(
            state.certs.pending_retirements.get(&pool_id).map(|e| e.0),
            Some(20),
            "Second PoolRetirement must overwrite the first"
        );
    }

    // -----------------------------------------------------------------------
    // Test 23 — re-registration cancels pending retirement
    // -----------------------------------------------------------------------

    /// When a pool that has a pending retirement re-registers, the retirement
    /// must be cancelled (matching Haskell `psRetiringL %~ Map.delete sppId`).
    #[test]
    fn test_pool_reregistration_cancels_retirement() {
        let mut state = make_state();
        let pool_id = test_pool_hash();
        let params = test_pool_params(pool_id);

        state.process_certificate(&Certificate::PoolRegistration(params.clone()));
        state.process_certificate(&Certificate::PoolRetirement {
            pool_hash: pool_id,
            epoch: 5,
        });
        assert!(state.certs.pending_retirements.contains_key(&pool_id));

        // Re-register
        state.process_certificate(&Certificate::PoolRegistration(params));

        assert!(
            !state.certs.pending_retirements.contains_key(&pool_id),
            "Re-registration must cancel the pending retirement"
        );
        assert!(
            state.certs.future_pool_params.contains_key(&pool_id),
            "Re-registration params must land in future_pool_params"
        );
    }

    // -----------------------------------------------------------------------
    // Test 24 — MIR from reserves to stake credential
    // -----------------------------------------------------------------------

    /// A MIR distributing lovelace from reserves to a stake credential must:
    ///  - credit the reward account;
    ///  - debit the reserves by the same amount.
    #[test]
    fn test_mir_reserves_to_stake_credential() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        let initial_reserves: u64 = 100_000_000_000;
        state.epochs.reserves.0 = initial_reserves;

        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Pre-register so reward_accounts has the entry.
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        let amount: i64 = 5_000_000;
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred, amount)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, amount as u64,
            "MIR reward account credit must equal the distributed amount"
        );
        assert_eq!(
            state.epochs.reserves.0,
            initial_reserves - amount as u64,
            "Reserves must be debited by the distributed amount"
        );
    }

    // -----------------------------------------------------------------------
    // Test 25 — MIR from treasury to stake credential
    // -----------------------------------------------------------------------

    #[test]
    fn test_mir_treasury_to_stake_credential() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        let initial_treasury: u64 = 50_000_000_000;
        state.epochs.treasury.0 = initial_treasury;

        let cred = test_credential();
        let key = credential_to_hash(&cred);
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        let amount: i64 = 2_000_000;
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Treasury,
            target: MIRTarget::StakeCredentials(vec![(cred, amount)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, amount as u64,
            "reward account must receive the MIR amount"
        );
        assert_eq!(
            state.epochs.treasury.0,
            initial_treasury - amount as u64,
            "Treasury must be debited by the distributed amount"
        );
    }

    // -----------------------------------------------------------------------
    // Test 26 — MIR pot transfer: reserves → treasury
    // -----------------------------------------------------------------------

    #[test]
    fn test_mir_pot_transfer_reserves_to_treasury() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        state.epochs.reserves.0 = 10_000_000_000;
        state.epochs.treasury.0 = 0;

        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::OtherAccountingPot(3_000_000_000),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        assert_eq!(
            state.epochs.reserves.0, 7_000_000_000,
            "Reserves must decrease"
        );
        assert_eq!(
            state.epochs.treasury.0, 3_000_000_000,
            "Treasury must increase"
        );
    }

    // -----------------------------------------------------------------------
    // Test 27 — MIR pot transfer: treasury → reserves
    // -----------------------------------------------------------------------

    #[test]
    fn test_mir_pot_transfer_treasury_to_reserves() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        state.epochs.reserves.0 = 0;
        state.epochs.treasury.0 = 8_000_000_000;

        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Treasury,
            target: MIRTarget::OtherAccountingPot(2_000_000_000),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        assert_eq!(
            state.epochs.treasury.0, 6_000_000_000,
            "Treasury must decrease"
        );
        assert_eq!(
            state.epochs.reserves.0, 2_000_000_000,
            "Reserves must increase"
        );
    }

    // -----------------------------------------------------------------------
    // Test 28 — MIR capped at available pot: NoMirTransfer, not a panic
    // (issue #803 — updated 2026-07-06; previously this test PINNED the
    // panic!() that #803 reports as the bug. Haskell's `Mir.hs`
    // `mirTransition` is a total, non-throwing STS
    // (`PredicateFailure (MIR era) = Void`): an insolvent transfer emits
    // `NoMirTransfer` and leaves both pots byte-identical rather than
    // crashing. `validateMIRCert`/`InsufficientForInstantaneousRewards`
    // *should* reject this upstream, but dugite's Phase-1 admission has
    // documented gaps (see `validation/mir.rs`), so this boundary function
    // must fail safe rather than assume the invariant always holds.)
    // -----------------------------------------------------------------------

    #[test]
    fn test_mir_pot_transfer_capped_at_available() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        state.epochs.reserves.0 = 1_000_000;
        state.epochs.treasury.0 = 0;

        // Try to move 5B from a 1M reserve — insolvent.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::OtherAccountingPot(5_000_000_000),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        assert_eq!(
            state.epochs.reserves.0, 1_000_000,
            "NoMirTransfer: reserves must be left byte-unchanged, not underflowed/panicked"
        );
        assert_eq!(
            state.epochs.treasury.0, 0,
            "NoMirTransfer: treasury must be left byte-unchanged"
        );
        assert_eq!(
            state.certs.pending_mir_delta_reserves, 0,
            "pending MIR accumulators must still be cleared on the NoMirTransfer path"
        );
    }

    // -----------------------------------------------------------------------
    // Test 28b — MIR per-credential negative update: NoMirTransfer (#803)
    // -----------------------------------------------------------------------

    /// A negative MIR delta larger in magnitude than a credential's existing
    /// balance would drive that credential negative. Haskell's `Mir.hs`
    /// boundary check alone (`totR <= availableReserves`) does NOT catch
    /// this — a negative per-credential delta only makes `totR` easier to
    /// satisfy — so this is dugite's defense-in-depth guard (issue #803)
    /// against the documented Phase-1 gap where
    /// `MIRProducesNegativeUpdate` is silently skipped
    /// (`validation/mir.rs` limitation #1). Must not panic; must leave
    /// pots AND the credential's balance byte-unchanged.
    #[test]
    fn test_mir_insolvent_per_credential_no_mir_transfer_803() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        let initial_reserves: u64 = 1_000_000_000_000;
        let initial_treasury: u64 = 0;
        state.epochs.reserves.0 = initial_reserves;
        state.epochs.treasury.0 = initial_treasury;

        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Register with a zero starting balance.
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        // Aggregate solvency alone is satisfied here (tot_r = -5_000_000 <<
        // available_reserves), but crediting this delta to the (zero-balance)
        // credential would drive its account negative.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred, -5_000_000i64)]),
        });

        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        assert_eq!(
            state.epochs.reserves.0, initial_reserves,
            "NoMirTransfer: reserves must be unchanged when a per-credential update would go negative"
        );
        assert_eq!(
            state.epochs.treasury.0, initial_treasury,
            "NoMirTransfer: treasury must be unchanged"
        );
        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "credential balance must be untouched on the NoMirTransfer path"
        );
    }

    // -----------------------------------------------------------------------
    // Test 28c — MIR cross-map (reserves + treasury) joint underflow (#803
    // follow-up): a credential present in BOTH pending maps whose deltas each
    // pass the per-map non-negativity guard against the pre-apply balance but
    // are JOINTLY negative must be caught (NoMirTransfer), not applied.
    // -----------------------------------------------------------------------

    /// Before the fix, Step 4 checked each map entry independently against the
    /// pre-apply balance and Step 5 applied reserves-then-treasury
    /// sequentially. A credential in both maps could pass both checks yet go
    /// negative mid-apply, wrapping `(balance as i128 + delta) as u64` into a
    /// ~1.8e19-lovelace reward balance (main panicked here; the #803 rewrite
    /// turned that panic into a silent wrap). The guard and the credit must
    /// operate on the per-credential COMBINED net delta (Haskell credits
    /// accounts via the union of the two reward maps).
    #[test]
    fn test_mir_cross_map_joint_underflow_no_mir_transfer_803() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        let initial_reserves: u64 = 1_000_000_000_000;
        let initial_treasury: u64 = 1_000_000_000_000;
        state.epochs.reserves.0 = initial_reserves;
        state.epochs.treasury.0 = initial_treasury;

        let cred = test_credential();
        let key = credential_to_hash(&cred);
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        // Round 1: credit the credential to a starting balance of 8_000_000.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred.clone(), 8_000_000i64)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);
        assert_eq!(
            state
                .certs
                .reward_accounts
                .get(&key)
                .copied()
                .unwrap_or(Lovelace(0))
                .0,
            8_000_000,
            "precondition: credential should hold 8_000_000 after round 1"
        );
        let reserves_after_r1 = state.epochs.reserves.0;

        // Round 2: -5_000_000 from reserves AND -5_000_000 from treasury for the
        // same credential. Each passes independently (8M - 5M = 3M >= 0), but
        // combined 8M - 10M = -2M < 0.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred.clone(), -5_000_000i64)]),
        });
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Treasury,
            target: MIRTarget::StakeCredentials(vec![(cred, -5_000_000i64)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        // NoMirTransfer: pots and the credential balance are all byte-unchanged,
        // and crucially the balance did NOT wrap to a ~1.8e19 value.
        assert_eq!(
            state
                .certs
                .reward_accounts
                .get(&key)
                .copied()
                .unwrap_or(Lovelace(0))
                .0,
            8_000_000,
            "cross-map joint underflow must be caught: balance unchanged, no u64 wrap"
        );
        assert_eq!(
            state.epochs.reserves.0, reserves_after_r1,
            "NoMirTransfer: reserves must be unchanged"
        );
        assert_eq!(
            state.epochs.treasury.0, initial_treasury,
            "NoMirTransfer: treasury must be unchanged"
        );
    }

    // -----------------------------------------------------------------------
    // Test 29a — MIR last-wins semantics for pre-Alonzo (pvMajor <= 4)
    // -----------------------------------------------------------------------

    /// Two MIR reserve certs for the same credential, pre-Alonzo (PV=4).
    /// Per Haskell `applyMIRCert` (`Map.union` left-biased, processed in cert order):
    /// the LAST cert's amount must win — earlier amounts are overwritten, NOT summed.
    ///
    /// cert 1: 500_000_000 lovelace (processed first → overwritten)
    /// cert 2: 200_000_000 lovelace (processed second → wins)
    /// expected reward credit = 200_000_000, NOT 700_000_000
    #[test]
    fn mir_last_wins_pre_alonzo() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        // Downgrade to Mary / protocol version 4 (pre-Alonzo).
        state.epochs.protocol_params.protocol_version_major = 4;
        let initial_reserves: u64 = 1_000_000_000_000;
        state.epochs.reserves.0 = initial_reserves;

        let cred = test_credential();
        let key = credential_to_hash(&cred);
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        // First cert: 500_000_000 — must be overwritten by the second.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred.clone(), 500_000_000i64)]),
        });
        // Second cert: 200_000_000 — this one must win.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred, 200_000_000i64)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 200_000_000,
            "pre-Alonzo last-wins: reward must equal the LAST cert amount (200_000_000), not the sum (700_000_000)"
        );
        assert_eq!(
            state.epochs.reserves.0,
            initial_reserves - 200_000_000,
            "pre-Alonzo last-wins: reserves must be debited by the LAST cert amount only"
        );
    }

    // -----------------------------------------------------------------------
    // Test 29b — MIR additive semantics for Alonzo+ (pvMajor >= 5)
    // -----------------------------------------------------------------------

    /// Two MIR reserve certs for the same credential, Alonzo+ (PV=6).
    /// Per Haskell `applyMIRCert` (`Map.unionWith (<>)` additive):
    /// both cert amounts are summed → 700_000_000.
    #[test]
    fn mir_additive_alonzo_plus() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        // Set to Alonzo+ protocol version 6.
        state.epochs.protocol_params.protocol_version_major = 6;
        let initial_reserves: u64 = 1_000_000_000_000;
        state.epochs.reserves.0 = initial_reserves;

        let cred = test_credential();
        let key = credential_to_hash(&cred);
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        // First cert: 500_000_000.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred.clone(), 500_000_000i64)]),
        });
        // Second cert: 200_000_000.  Together they must sum to 700_000_000.
        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred, 200_000_000i64)]),
        });
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 700_000_000,
            "Alonzo+ additive: reward must equal the SUM of both cert amounts (700_000_000)"
        );
        assert_eq!(
            state.epochs.reserves.0,
            initial_reserves - 700_000_000,
            "Alonzo+ additive: reserves must be debited by the full sum"
        );
    }

    // -----------------------------------------------------------------------
    // Test 29c — MIR to an unregistered credential is silently dropped
    // -----------------------------------------------------------------------

    /// A MIR cert for an UNregistered credential must not debit reserves and
    /// must not panic.  `apply_pending_mir` only credits registered reward accounts.
    #[test]
    fn mir_unregistered_credential_dropped() {
        use dugite_primitives::transaction::{MIRSource, MIRTarget};

        let mut state = make_state();
        // Use PV=4 (pre-Alonzo) — exercises last-wins path; same behaviour at any PV.
        state.epochs.protocol_params.protocol_version_major = 4;
        let initial_reserves: u64 = 1_000_000_000_000;
        state.epochs.reserves.0 = initial_reserves;

        // Intentionally do NOT register the credential.
        let cred = test_credential();

        state.process_certificate(&Certificate::MoveInstantaneousRewards {
            source: MIRSource::Reserves,
            target: MIRTarget::StakeCredentials(vec![(cred, 500_000_000i64)]),
        });
        // Must not panic.
        super::apply_pending_mir(&mut state.certs, &mut state.epochs);

        // Reserves must be unchanged — unregistered credential is filtered out
        // by apply_pending_mir's registered-only loop.
        assert_eq!(
            state.epochs.reserves.0, initial_reserves,
            "Reserves must not be debited when the MIR target is an unregistered credential"
        );
    }

    // -----------------------------------------------------------------------
    // Test 29 — StakeDelegation to a new pool
    // -----------------------------------------------------------------------

    /// A StakeDelegation cert must insert/update the delegation map entry.
    #[test]
    fn test_stake_delegation_updates_delegation_map() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let pool1 = test_pool_hash();
        let pool2 = Hash28::from_bytes([0xBBu8; 28]);

        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        state.process_certificate(&Certificate::StakeDelegation {
            credential: cred.clone(),
            pool_hash: pool1,
        });
        assert_eq!(
            state.certs.delegations.get(&key).copied(),
            Some(pool1),
            "Delegation must point to pool1"
        );

        // Re-delegate to pool2
        state.process_certificate(&Certificate::StakeDelegation {
            credential: cred,
            pool_hash: pool2,
        });
        assert_eq!(
            state.certs.delegations.get(&key).copied(),
            Some(pool2),
            "Delegation must be updated to pool2"
        );
    }

    // -----------------------------------------------------------------------
    // Test 30 — StakeVoteDelegation sets both delegation and vote_delegation
    // -----------------------------------------------------------------------

    #[test]
    fn test_stake_vote_delegation_sets_both_maps() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let pool = test_pool_hash();

        state.process_certificate(&Certificate::StakeVoteDelegation {
            credential: cred,
            pool_hash: pool,
            drep: DRep::Abstain,
        });

        assert_eq!(
            state.certs.delegations.get(&key).copied(),
            Some(pool),
            "StakeVoteDelegation must set pool delegation"
        );
        assert_eq!(
            state.gov.governance.vote_delegations.get(&key).cloned(),
            Some(DRep::Abstain),
            "StakeVoteDelegation must set vote delegation"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31 — StakeDeregistration removes vote_delegation (Haskell unified map)
    // -----------------------------------------------------------------------

    /// Per Haskell's Conway DELEG rule, deregistration removes ALL credential
    /// data from the unified map, including vote delegations.
    #[test]
    fn test_stake_deregistration_removes_vote_delegation() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));
        state.process_certificate(&Certificate::VoteDelegation {
            credential: cred.clone(),
            drep: DRep::NoConfidence,
        });
        assert!(
            state.gov.governance.vote_delegations.contains_key(&key),
            "Vote delegation should be set before deregistration"
        );

        state.process_certificate(&Certificate::StakeDeregistration(cred));

        assert!(
            !state.gov.governance.vote_delegations.contains_key(&key),
            "Deregistration must clear vote_delegations (Haskell unified map)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 32 — script credential tracking in stake registration
    // -----------------------------------------------------------------------

    /// When a Script credential is used in StakeRegistration, it must be
    /// added to `script_stake_credentials`.
    #[test]
    fn test_script_credential_tracked_on_registration() {
        let mut state = make_state();
        let cred = Credential::Script(Hash28::from_bytes([0x99u8; 28]));
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        assert!(
            state.certs.script_stake_credentials.contains(&key),
            "Script credential must be added to script_stake_credentials on registration"
        );

        // Deregistration must clean it up.
        state.process_certificate(&Certificate::StakeDeregistration(cred));
        assert!(
            !state.certs.script_stake_credentials.contains(&key),
            "Script credential must be removed from script_stake_credentials on deregistration"
        );
    }

    // -----------------------------------------------------------------------
    // Test 33 — RegStakeDeleg: register + delegate atomically
    // -----------------------------------------------------------------------

    /// RegStakeDeleg (Conway combined cert) must register the stake key AND
    /// set the delegation in a single application.
    #[test]
    fn test_reg_stake_deleg_registers_and_delegates() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);
        let pool = test_pool_hash();

        state.process_certificate(&Certificate::RegStakeDeleg {
            credential: cred,
            pool_hash: pool,
            deposit: Lovelace(2_000_000),
        });

        assert!(
            state.certs.reward_accounts.contains_key(&key),
            "RegStakeDeleg must create reward account"
        );
        assert_eq!(
            state.certs.delegations.get(&key).copied(),
            Some(pool),
            "RegStakeDeleg must set delegation"
        );
        assert_eq!(
            state.certs.stake_key_deposits.get(&key).copied(),
            Some(state.epochs.protocol_params.key_deposit.0),
            "RegStakeDeleg must record deposit"
        );
    }

    // -----------------------------------------------------------------------
    // Test 34 — ConwayStakeRegistration removes pointer on deregistration
    // -----------------------------------------------------------------------

    /// Processing ConwayStakeRegistration via process_certificate_with_pointer
    /// then deregistering must remove all pointer_map entries for that credential.
    #[test]
    fn test_conway_registration_pointer_then_dereg_clears_pointer() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate_with_pointer(
            &Certificate::ConwayStakeRegistration {
                credential: cred.clone(),
                deposit: Lovelace(2_000_000),
            },
            42,
            1,
            0,
        );

        let pointer = dugite_primitives::credentials::Pointer {
            slot: 42,
            tx_index: 1,
            cert_index: 0,
        };
        assert_eq!(
            state.certs.pointer_map.get(&pointer).copied(),
            Some(key),
            "pointer_map entry must exist after ConwayStakeRegistration"
        );

        state.process_certificate(&Certificate::ConwayStakeDeregistration {
            credential: cred,
            refund: Lovelace(2_000_000),
        });

        assert!(
            !state.certs.pointer_map.contains_key(&pointer),
            "pointer_map entry must be cleared on ConwayStakeDeregistration"
        );
    }

    // -----------------------------------------------------------------------
    // Test 35 — DRep drep_registration_count incremented
    // -----------------------------------------------------------------------

    #[test]
    fn test_drep_registration_count_incremented() {
        let mut state = make_state();

        for i in 0u8..3 {
            let cred = Credential::VerificationKey(Hash28::from_bytes([i; 28]));
            state.process_certificate(&Certificate::RegDRep {
                credential: cred,
                deposit: Lovelace(500_000_000),
                anchor: None,
            });
        }

        assert_eq!(
            state.gov.governance.drep_registration_count, 3,
            "drep_registration_count must be incremented once per RegDRep cert"
        );
    }

    // -----------------------------------------------------------------------
    // Test 36 — UnregDRep with zero deposit does not credit reward account
    // -----------------------------------------------------------------------

    #[test]
    fn test_drep_unregistration_zero_deposit_no_reward_credit() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        // Register with zero deposit (non-standard but must be handled gracefully)
        state.process_certificate(&Certificate::RegDRep {
            credential: cred.clone(),
            deposit: Lovelace(0),
            anchor: None,
        });
        state.process_certificate(&Certificate::StakeRegistration(cred.clone()));

        state.process_certificate(&Certificate::UnregDRep {
            credential: cred,
            refund: Lovelace(0),
        });

        // DRep must be gone.
        assert!(
            !state.gov.governance.dreps.contains_key(&key),
            "DRep must be removed after UnregDRep"
        );

        // Reward account should still be at zero (no phantom credit).
        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "Zero-deposit UnregDRep must not credit the reward account"
        );
    }

    // -----------------------------------------------------------------------
    // Test 37 — withdrawal zeroes the reward balance
    // -----------------------------------------------------------------------

    /// process_withdrawal must zero the reward account balance.
    #[test]
    fn test_withdrawal_zeroes_reward_balance() {
        let mut state = make_state();
        let cred = test_credential();
        let key = credential_to_hash(&cred);

        state.process_certificate(&Certificate::StakeRegistration(cred));
        // Manually credit rewards.
        state
            .certs
            .reward_accounts
            .entry(key)
            .and_modify(|b| b.0 = 10_000_000);

        // Construct a reward account byte string (mainnet key: header=0xE1, then 28 bytes).
        let mut reward_account = vec![0xE1u8];
        reward_account.extend_from_slice(key.as_bytes());

        state.process_withdrawal(&reward_account, Lovelace(10_000_000));

        let balance = state
            .certs
            .reward_accounts
            .get(&key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "After withdrawal, reward account balance must be 0"
        );
    }
}
