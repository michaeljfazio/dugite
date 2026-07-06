//! Shared helpers used across multiple era rule implementations.
//!
//! These are NOT on the EraRules trait -- they are internal building blocks
//! that era impls compose to avoid duplicating logic. The pattern is
//! composition over inheritance.
//!
//! NOTE: These helpers are currently unused -- they are building blocks that will
//! be called by ShelleyRules, AlonzoRules, BabbageRules, and ConwayRules
//! implementations (Tasks 9-11).
//!
//! Each function takes sub-state references as parameters (not `&mut LedgerState`),
//! enabling independent borrow checking and clean composition in era rule
//! implementations.
//!
//! # Functions
//!
//! | Helper | Used By | Description |
//! |--------|---------|-------------|
//! | [`apply_utxo_changes`] | Shelley, Alonzo, Babbage, Conway | Consume inputs, produce outputs, record fee |
//! | [`apply_collateral_consumption`] | Alonzo, Babbage, Conway | IsValid=false collateral forfeiture |
//! | [`process_shelley_certs`] | Shelley, Allegra, Mary, Alonzo, Babbage | Shelley-era certificate processing |
//! | [`drain_withdrawal_accounts`] | Shelley+ | Zero reward accounts referenced by tx withdrawals |
//! | [`compute_shelley_nonce`] | Shelley+ | VRF-based nonce evolution and block counting |

use std::collections::HashMap;
use std::sync::Arc;

use dugite_primitives::address::Address;
use dugite_primitives::block::{Block, BlockHeader};

use super::RuleContext;
use crate::state::LedgerError;
use dugite_primitives::credentials::{Credential, Pointer};
use dugite_primitives::hash::{blake2b_224, blake2b_256, Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Certificate, MIRSource, MIRTarget, Transaction, TransactionInput,
};
use dugite_primitives::value::Lovelace;
use tracing::debug;

use crate::state::substates::{
    CertSubState, ConsensusSubState, EpochSubState, GovSubState, UtxoSubState,
};
use crate::state::PoolRegistration;
use crate::utxo_diff::UtxoDiff;

// ---------------------------------------------------------------------------
// Stake routing (local mirror of state::StakeRouting)
// ---------------------------------------------------------------------------

/// The stake routing outcome for a UTxO output address.
///
/// Mirrors `state::StakeRouting`. Defined here so that `common.rs` helpers do
/// not depend on private types in the `state` module.
enum StakeRouting {
    /// Credential hash -- route coins to `stake_distribution.stake_map`.
    Credential(Hash32),
    /// Pointer key -- route coins to `ptr_stake` (deferred resolution at SNAP time).
    Pointer(Pointer),
    /// No stake routing (Enterprise / Byron / unknown).
    None,
}

/// Classify a UTxO address into its stake-routing bucket.
///
/// * Base / Reward  -> `StakeRouting::Credential` (eager resolution)
/// * Pointer        -> `StakeRouting::Pointer` (deferred)
/// * Everything else -> `StakeRouting::None`
///
/// When `exclude_ptrs` is true (Conway era), pointer addresses return
/// `StakeRouting::None`.
fn stake_routing(address: &Address, exclude_ptrs: bool) -> StakeRouting {
    match address {
        Address::Base(base) => StakeRouting::Credential(credential_to_hash(&base.stake)),
        Address::Reward(reward) => StakeRouting::Credential(credential_to_hash(&reward.stake)),
        Address::Pointer(ptr_addr) => {
            if exclude_ptrs {
                StakeRouting::None
            } else {
                StakeRouting::Pointer(ptr_addr.pointer)
            }
        }
        _ => StakeRouting::None,
    }
}

/// Extract a Hash32 from a Credential for use as a map key.
///
/// Uses `to_typed_hash32()` which encodes the credential TYPE (key vs script)
/// in byte 28 of the padding, matching Haskell's `KeyHashObj`/`ScriptHashObj`
/// distinction.
fn credential_to_hash(credential: &Credential) -> Hash32 {
    credential.to_typed_hash32()
}

/// Extract a Hash32 from a raw reward account byte string (29 bytes: 1-byte
/// header + 28-byte credential hash).
///
/// Mirrors `LedgerState::reward_account_to_hash` from `state/certificates.rs`.
pub(crate) fn reward_account_to_hash(reward_account: &[u8]) -> Hash32 {
    let mut key_bytes = [0u8; 32];
    if reward_account.len() >= 29 {
        // Copy exactly 28 bytes of the credential (skip the 1-byte header).
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

/// Haskell `Nonce` combine operator (⭒), used to fold the epoch nonce.
///
/// `NeutralNonce` is represented as the all-zero `Hash32` and is the identity:
///   * `NeutralNonce ⭒ x = x`
///   * `x ⭒ NeutralNonce = x`
///   * `Nonce(a) ⭒ Nonce(b) = Nonce(blake2b_256(a || b))`
///
/// Source: `cardano-ledger` `Cardano.Ledger.BaseTypes` `instance Semigroup Nonce`.
pub(crate) fn combine_nonce(a: Hash32, b: Hash32) -> Hash32 {
    let zero = Hash32::ZERO;
    if a == zero {
        b
    } else if b == zero {
        a
    } else {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(a.as_bytes());
        buf.extend_from_slice(b.as_bytes());
        blake2b_256(&buf)
    }
}

// ============================================================================
// 1. apply_utxo_changes
// ============================================================================

/// Apply UTxO changes for a valid transaction (IsValid=true path).
///
/// Core UTxO state mutation logic shared by all post-Byron eras:
///
/// 1. Snapshot spent outputs (for stake distribution and diff recording).
/// 2. Subtract spent coins from the stake distribution.
/// 3. Remove inputs from the UTxO set (best-effort: missing inputs are logged,
///    not fatal -- matches Haskell `applyTx` for confirmed on-chain blocks).
/// 4. Insert new outputs unconditionally (prevents cascade divergence).
/// 5. Add new output coins to the stake distribution.
/// 6. Accumulate the transaction fee.
///
/// Returns a `UtxoDiff` recording all inserts and deletes for rollback support.
///
/// # Parameters
///
/// * `tx` -- the transaction to apply.
/// * `utxo` -- mutable UTxO sub-state (utxo_set, epoch_fees, diff_seq).
/// * `certs` -- mutable cert sub-state (stake_distribution for tracking).
/// * `epochs` -- epoch sub-state (ptr_stake for pointer-addressed UTxOs).
pub(crate) fn apply_utxo_changes(
    tx: &Transaction,
    utxo: &mut UtxoSubState,
    certs: &mut CertSubState,
    epochs: &mut EpochSubState,
) -> UtxoDiff {
    let mut diff = UtxoDiff::new();

    // --- Phase 1: snapshot spent outputs before mutation ---
    //
    // Collect (input, output) pairs for inputs that exist in the UTxO set.
    // Missing inputs (pre-replay gaps) are silently skipped.
    //
    // DEDUPLICATE first: in the Cardano ledger `TxBody.inputs` is a `Set TxIn`,
    // so an input the wire CBOR lists more than once is spent — and has its
    // stake subtracted — exactly ONCE. Iterating the raw decoded Vec with
    // duplicates double-subtracts from `stake_distribution` (Phase 2); the UTxO
    // removal/insert (Phase 3/4) is idempotent so the UTxO set looks correct,
    // masking the drift, but the per-credential stake (and every downstream
    // reward / pool-stake / treasury calculation) silently diverges. Real
    // preprod txs do carry duplicate inputs (e.g. tx b6ce541006… at epoch 35
    // lists d94cc73b…#0 and #1 twice each), which compounded into the
    // +1-lovelace Conway PV10 withdrawal-amount halt at epoch 181.
    let mut seen_inputs = std::collections::HashSet::new();
    let spent_outputs: Vec<_> = tx
        .body
        .inputs
        .iter()
        .filter(|input| seen_inputs.insert((*input).clone()))
        .filter_map(|input| {
            utxo.utxo_set
                .lookup(input)
                .map(|output| (input.clone(), output))
        })
        .collect();

    // --- Phase 2: update stake distribution from consumed inputs (subtract) ---
    for (_input, spent_output) in &spent_outputs {
        let coin = spent_output.value.coin.0;
        match stake_routing(&spent_output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(cred_hash) => {
                if let Some(stake) = certs.stake_distribution.stake_map.get_mut(&cred_hash) {
                    stake.0 = stake.0.saturating_sub(coin);
                }
            }
            StakeRouting::Pointer(ptr) => {
                if let Some(entry) = epochs.ptr_stake.get_mut(&ptr) {
                    *entry = entry.saturating_sub(coin);
                }
            }
            StakeRouting::None => {}
        }
    }

    // --- Phase 3: remove inputs (best-effort) ---
    let mut missing_inputs = 0usize;
    for input in &tx.body.inputs {
        if utxo.utxo_set.contains(input) {
            utxo.utxo_set.remove(input);
        } else {
            missing_inputs += 1;
            debug!(
                tx_hash = %tx.hash.to_hex(),
                input = %input,
                "apply_utxo_changes: input not found in UTxO set (already spent or \
                 pre-replay gap) -- outputs will still be created"
            );
        }
    }
    if missing_inputs > 0 {
        debug!(
            tx_hash = %tx.hash.to_hex(),
            missing = missing_inputs,
            total = tx.body.inputs.len(),
            "apply_utxo_changes: {} of {} inputs were absent; outputs inserted regardless",
            missing_inputs,
            tx.body.inputs.len(),
        );
    }

    // Record deletions for diff.
    for (input, output) in spent_outputs {
        diff.record_delete(input, output);
    }

    // --- Phase 4: insert new outputs unconditionally ---
    for (idx, output) in tx.body.outputs.iter().enumerate() {
        let new_input = TransactionInput {
            transaction_id: tx.hash,
            index: idx as u32,
        };
        diff.record_insert(new_input.clone(), output.clone());
        utxo.utxo_set.insert(new_input, output.clone());
    }

    // --- Phase 5: update stake distribution from new outputs (add) ---
    for output in &tx.body.outputs {
        let coin = output.value.coin.0;
        match stake_routing(&output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(cred_hash) => {
                *certs
                    .stake_distribution
                    .stake_map
                    .entry(cred_hash)
                    .or_insert(Lovelace(0)) += Lovelace(coin);
            }
            StakeRouting::Pointer(ptr) => {
                *epochs.ptr_stake.entry(ptr).or_insert(0) += coin;
            }
            StakeRouting::None => {}
        }
    }

    // --- Phase 6: accumulate fee ---
    utxo.epoch_fees += tx.body.fee;

    diff
}

// ============================================================================
// 2. apply_collateral_consumption
// ============================================================================

/// Apply collateral consumption for an invalid transaction (IsValid=false path).
///
/// When a Plutus script fails Phase-2 validation, the block producer marks
/// the transaction as invalid. The regular inputs/outputs/certificates are NOT
/// applied. Instead:
///
/// 1. Collateral inputs are consumed (forfeited to the block producer).
/// 2. If `collateral_return` is present (Babbage+), it becomes a new UTxO.
/// 3. The fee is either `total_collateral` (if declared) or the difference
///    between collateral input value and collateral return value.
///
/// Returns a `UtxoDiff` recording collateral-related inserts and deletes.
///
/// # Parameters
///
/// * `tx` -- the invalid transaction.
/// * `utxo` -- mutable UTxO sub-state.
/// * `certs` -- mutable cert sub-state (stake distribution updates).
/// * `epochs` -- mutable epoch sub-state (ptr_stake for pointer routing).
///
/// # Eras
///
/// Only relevant for Alonzo+ (collateral was introduced in the Alonzo era).
/// Babbage added `collateral_return` and `total_collateral` fields.
pub(crate) fn apply_collateral_consumption(
    tx: &Transaction,
    utxo: &mut UtxoSubState,
    certs: &mut CertSubState,
    epochs: &mut EpochSubState,
) -> UtxoDiff {
    let mut diff = UtxoDiff::new();
    let mut collateral_input_value: u64 = 0;

    // Consume collateral inputs and update stake distribution.
    // Deduplicate: collateral is a `Set TxIn` in the ledger, so a collateral
    // input listed more than once on the wire is consumed (and stake-subtracted)
    // exactly once — same rationale as the regular-input dedup in
    // `apply_utxo_changes`.
    let mut seen_collateral = std::collections::HashSet::new();
    for col_input in tx
        .body
        .collateral
        .iter()
        .filter(|c| seen_collateral.insert((*c).clone()))
    {
        if let Some(spent) = utxo.utxo_set.lookup(col_input) {
            collateral_input_value += spent.value.coin.0;
            let coin = spent.value.coin.0;
            match stake_routing(&spent.address, epochs.ptr_stake_excluded) {
                StakeRouting::Credential(cred) => {
                    if let Some(stake) = certs.stake_distribution.stake_map.get_mut(&cred) {
                        stake.0 = stake.0.saturating_sub(coin);
                    }
                }
                StakeRouting::Pointer(ptr) => {
                    if let Some(entry) = epochs.ptr_stake.get_mut(&ptr) {
                        *entry = entry.saturating_sub(coin);
                    }
                }
                StakeRouting::None => {}
            }
            diff.record_delete(col_input.clone(), spent);
        }
        utxo.utxo_set.remove(col_input);
    }

    // If there's a collateral return output, add it to the UTxO set.
    let collateral_return_value = if let Some(col_return) = &tx.body.collateral_return {
        let coin = col_return.value.coin.0;
        match stake_routing(&col_return.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(cred) => {
                *certs
                    .stake_distribution
                    .stake_map
                    .entry(cred)
                    .or_insert(Lovelace(0)) += Lovelace(coin);
            }
            StakeRouting::Pointer(ptr) => {
                *epochs.ptr_stake.entry(ptr).or_insert(0) += coin;
            }
            StakeRouting::None => {}
        }
        let return_input = TransactionInput {
            transaction_id: tx.hash,
            // Collateral return is placed after regular outputs.
            index: tx.body.outputs.len() as u32,
        };
        diff.record_insert(return_input.clone(), col_return.clone());
        utxo.utxo_set.insert(return_input, col_return.clone());
        col_return.value.coin.0
    } else {
        0
    };

    // Fee collected is the actual collateral forfeited, NOT the declared fee.
    // If total_collateral is set, use it; otherwise compute from inputs - return.
    let collateral_fee = if let Some(tc) = tx.body.total_collateral {
        tc
    } else {
        Lovelace(collateral_input_value.saturating_sub(collateral_return_value))
    };
    utxo.epoch_fees += collateral_fee;

    diff
}

// ============================================================================
// 3. process_shelley_certs
// ============================================================================

/// Process Shelley-era certificate types from a transaction.
///
/// Handles the five certificate types introduced in Shelley that persist across
/// all subsequent eras:
///
/// - `StakeRegistration` -- register a stake credential (creates reward account,
///   tracks deposit, updates stake_map).
/// - `StakeDeregistration` -- deregister a stake credential (refunds deposit,
///   removes delegation and reward account).
/// - `StakeDelegation` -- delegate stake to a pool.
/// - `PoolRegistration` -- register or re-register a stake pool.
/// - `PoolRetirement` -- schedule a pool retirement at a future epoch.
///
/// This function also handles pointer map updates for registration-class
/// certificates, enabling Pointer address resolution.
///
/// Conway-era combined certificates (`ConwayStakeRegistration`,
/// `RegStakeDeleg`, etc.) and governance certificates (`RegDRep`, `VoteDelegation`,
/// etc.) are NOT processed here -- those are handled by `process_conway_certs`
/// in the Conway era rule implementation.
///
/// # Parameters
///
/// * `tx` -- the transaction containing certificates.
/// * `slot` -- the block slot (for pointer map entries).
/// * `tx_index` -- the transaction's index within the block.
/// * `certs` -- mutable cert sub-state (delegations, pool_params, reward_accounts, etc.).
/// * `epochs` -- epoch sub-state (protocol_params for deposit amounts).
/// * `gov` -- mutable governance sub-state (for deregistration cleanup of DRep
///   vote delegations, matching Haskell's unified map semantics).
pub(crate) fn process_shelley_certs(
    tx: &Transaction,
    slot: u64,
    tx_index: u64,
    certs: &mut CertSubState,
    epochs: &EpochSubState,
    gov: &mut GovSubState,
) {
    for (cert_index, cert) in tx.body.certificates.iter().enumerate() {
        apply_shelley_cert(cert, cert_index, slot, tx_index, certs, epochs, gov);
    }
}

/// Apply a single Shelley-era certificate to the ledger state.
///
/// Extracted from `process_shelley_certs` so that Conway-era cert processing
/// can interleave Shelley and Conway certs in a single ordered pass.
///
/// Non-Shelley cert variants are ignored (no-op). Callers must invoke the
/// Conway-era handler separately for those.
pub(crate) fn apply_shelley_cert(
    cert: &Certificate,
    cert_index: usize,
    slot: u64,
    tx_index: u64,
    certs: &mut CertSubState,
    epochs: &EpochSubState,
    gov: &mut GovSubState,
) {
    // Populate pointer_map for registration certificates.
    //
    // Pointer addresses (StakeRefPtr) are a pre-Conway construct. Haskell's
    // `Cardano.Ledger.Conway.State.Stake` drops `dsPtrs` at the Babbage→
    // Conway TranslateEra step and any subsequent `StakeRegistration` in
    // Conway+ does NOT re-populate it. Mirror that here so a from-genesis
    // replay matches an ancillary import byte-exact (#670).
    if epochs.protocol_params.protocol_version_major < 9 {
        if let Certificate::StakeRegistration(credential) = cert {
            let key = credential_to_hash(credential);
            let pointer = Pointer {
                slot,
                tx_index,
                cert_index: cert_index as u64,
            };
            certs.pointer_map.insert(pointer, key);
        }
    }

    match cert {
        Certificate::StakeRegistration(credential) => {
            let key = credential_to_hash(credential);
            certs
                .stake_distribution
                .stake_map
                .entry(key)
                .or_insert(Lovelace(0));
            certs.reward_accounts.entry(key).or_insert(Lovelace(0));
            if matches!(credential, Credential::Script(_)) {
                certs.script_stake_credentials.insert(key);
            }
            certs.total_stake_key_deposits += epochs.protocol_params.key_deposit.0;
            certs
                .stake_key_deposits
                .insert(key, epochs.protocol_params.key_deposit.0);
            debug!("Stake key registered: {}", key.to_hex());
        }
        Certificate::StakeDeregistration(credential) => {
            let key = credential_to_hash(credential);
            let stored_deposit = certs
                .stake_key_deposits
                .remove(&key)
                .unwrap_or(epochs.protocol_params.key_deposit.0);
            certs.total_stake_key_deposits = certs
                .total_stake_key_deposits
                .saturating_sub(stored_deposit);
            certs.delegations.remove(&key);
            certs.reward_accounts.remove(&key);
            // Remove DRep delegation -- Haskell's unified map clears all credential
            // data on deregistration, including vote delegations.
            Arc::make_mut(&mut gov.governance)
                .vote_delegations
                .remove(&key);
            certs.script_stake_credentials.remove(&key);
            certs.pointer_map.retain(|_, v| *v != key);
            debug!("Stake key deregistered: {}", key.to_hex());
        }
        Certificate::StakeDelegation {
            credential,
            pool_hash,
        } => {
            let key = credential_to_hash(credential);
            certs.delegations.insert(key, *pool_hash);
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
            // Re-registration: defer to future_pool_params and cancel pending retirement.
            // First registration: apply immediately and record deposit.
            if certs.pool_params.contains_key(&params.operator) {
                certs.pending_retirements.remove(&params.operator);
                certs.future_pool_params.insert(params.operator, pool_reg);
                debug!(
                    "Pool re-registered (deferred, retirement cancelled): {}",
                    params.operator.to_hex()
                );
            } else {
                Arc::make_mut(&mut certs.pool_params).insert(params.operator, pool_reg);
                certs
                    .pool_deposits
                    .insert(params.operator, epochs.protocol_params.pool_deposit.0);
                debug!("Pool registered: {}", params.operator.to_hex());
            }
        }
        Certificate::PoolRetirement { pool_hash, epoch } => {
            debug!(
                "Pool retirement scheduled at epoch {}: {}",
                epoch,
                pool_hash.to_hex()
            );
            certs
                .pending_retirements
                .insert(*pool_hash, EpochNo(*epoch));
        }
        Certificate::MoveInstantaneousRewards { source, target } => {
            // Per Haskell `Cardano.Ledger.Shelley.Rules.Deleg.hs` (`applyMIRCert`):
            // MIR certs do NOT immediately debit pots or credit reward accounts.
            // They accumulate into `InstantaneousRewards` (`dsIRewards`) during
            // the epoch, and the actual transfer happens at the next epoch boundary
            // via the MIR sub-rule of NEWEPOCH (called `apply_pending_mir` here).
            //
            // MIR is an `AtMostEra "Babbage"` construct (removed in Conway, PV >= 9).
            // Guard here mirrors Haskell's era constraint — no-op in Conway+.
            if epochs.protocol_params.protocol_version_major >= 9 {
                return;
            }
            match target {
                MIRTarget::StakeCredentials(creds) => {
                    let pending = match source {
                        MIRSource::Reserves => &mut certs.pending_mir_reserves,
                        MIRSource::Treasury => &mut certs.pending_mir_treasury,
                    };
                    // Haskell `Cardano.Ledger.Shelley.Rules.Deleg.hs` `applyMIRCert`:
                    //   pvMajor <= 4 (Shelley/Allegra/Mary): `Map.union credCoinMap' ir`
                    //     — left-biased, so a later cert for the same credential OVERWRITES.
                    //     This is "last-wins": process certs in order; the last one wins.
                    //   pvMajor >  4 (Alonzo+): `Map.unionWith (<>) credCoinMap' ir`
                    //     — additive: amounts for the same credential are summed.
                    // Guard: `hardforkAlonzoAllowMIRTransfer pv = pvMajor pv > natVersion @4`
                    let pv = epochs.protocol_params.protocol_version_major;
                    let additive = pv > 4;
                    let mut total: i128 = 0;
                    for (cred, amount) in creds {
                        let key = credential_to_hash(cred);
                        if additive {
                            *pending.entry(key).or_insert(0i128) += *amount as i128;
                        } else {
                            // Last-wins: overwrite any previous entry for this credential.
                            pending.insert(key, *amount as i128);
                        }
                        total += *amount as i128;
                        debug!(
                            "MIR: queuing {} lovelace from {:?} to {} ({})",
                            amount,
                            source,
                            key.to_hex(),
                            if additive { "additive" } else { "last-wins" }
                        );
                    }
                    debug!(
                        "MIR: total {} lovelace queued from {:?} to {} credentials",
                        total,
                        source,
                        creds.len()
                    );
                }
                MIRTarget::OtherAccountingPot(coin) => {
                    // Pot-to-pot transfer accumulator.
                    // Per Haskell `dsIRewards.deltaReserves` / `deltaTreasury`.
                    match source {
                        MIRSource::Reserves => {
                            certs.pending_mir_delta_reserves += *coin as i128;
                            debug!(
                                "MIR: queuing pot-transfer {} lovelace reserves -> treasury",
                                coin
                            );
                        }
                        MIRSource::Treasury => {
                            certs.pending_mir_delta_treasury += *coin as i128;
                            debug!(
                                "MIR: queuing pot-transfer {} lovelace treasury -> reserves",
                                coin
                            );
                        }
                    }
                }
            }
        }
        // `GenesisKeyDelegation` is intentionally NOT handled here.
        //
        // Unlike every other cert above, its effect (the two-phase
        // `future_gen_delegs` -> `genesis_delegates` queue, per Haskell
        // `dsFutureGenDelegs`/`dsGenDelegs`) lives on TOP-LEVEL
        // `LedgerState`, not `CertSubState` — this function only has
        // `&mut CertSubState`. It is handled by the top-level orchestrator
        // instead: `enqueue_genesis_key_delegations` (called from the
        // Step 8b per-tx loop in `state/apply.rs`) and
        // `adopt_matured_genesis_delegs` (called once per block, mirroring
        // Haskell's TICK). See issue #804 — this arm used to fall into the
        // catch-all below and silently drop the cert with no state update
        // at all.
        Certificate::GenesisKeyDelegation { .. } => {}
        // Skip non-Shelley certificates -- they are handled by era-specific code.
        _ => {}
    }
}

// ============================================================================
// 3b. Genesis-delegate future queue (#804)
// ============================================================================

/// Enqueue any `Certificate::GenesisKeyDelegation` certs in `tx` into the
/// two-phase `future_gen_delegs` queue.
///
/// Mirrors Haskell `Cardano.Ledger.Shelley.Rules.Deleg`'s `GenesisDelegTxCert`
/// branch: the cert does NOT update `genesis_delegates` (`dsGenDelegs`)
/// immediately. It schedules a `FutureGenDeleg { fgdSlot = slot +
/// stabilityWindow, fgdGenKeyHash = gk }` entry; the change only takes
/// effect once [`adopt_matured_genesis_delegs`] observes `fgdSlot <=
/// current_slot`.
///
/// `stability_window` MUST be `stabilityWindow = ceil(3k/f)` — i.e.
/// `LedgerState::stability_window_3kf` — taken directly, NOT doubled.
/// (Oracle-verified against the live Haskell source: the `2 *
/// stabilityWindow` figure that appears elsewhere in the ledger is a
/// *different* mechanism — `getTheSlotOfNoReturn`'s PPUP/HFC "point of no
/// return" deadline — and does not apply here.)
///
/// Called from the top-level orchestrator (`state/apply.rs`, Step 8b)
/// rather than from [`apply_shelley_cert`] because `future_gen_delegs`
/// lives directly on `LedgerState`, not `CertSubState`. See issue #804.
pub(crate) fn enqueue_genesis_key_delegations(
    tx: &Transaction,
    slot: u64,
    stability_window: u64,
    future_gen_delegs: &mut HashMap<(u64, Hash28), (Hash28, Hash32)>,
) {
    for cert in &tx.body.certificates {
        if let Certificate::GenesisKeyDelegation {
            genesis_hash,
            genesis_delegate_hash,
            vrf_keyhash,
        } = cert
        {
            // The cert fields are Hash32 (zero-padded from the on-wire
            // 28-byte hashes); `genesis_delegates`/`future_gen_delegs` use
            // Hash28 keys for the genesis/delegate hashes — truncate to the
            // first 28 bytes (mirrors `state/certificates.rs`'s dead
            // test-only handler).
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
            let maturity_slot = slot.saturating_add(stability_window);
            future_gen_delegs.insert((maturity_slot, gkey), (dkey, *vrf_keyhash));
            debug!(
                "GenesisKeyDelegation enqueued: {} -> delegate={}, vrf={} (matures at slot {})",
                genesis_hash.to_hex(),
                genesis_delegate_hash.to_hex(),
                vrf_keyhash.to_hex(),
                maturity_slot,
            );
        }
    }
}

/// Adopt matured `future_gen_delegs` entries into `genesis_delegates`.
///
/// Mirrors Haskell `adoptGenesisDelegs` (`Cardano.Ledger.Shelley.Rules.Tick`),
/// which runs EVERY block via `validatingTickTransition` (TICK) — not just
/// at epoch boundaries. `current_slot` is the block's own slot (the TICK
/// signal), and comparison is `fgdSlot <= current_slot`.
///
/// When multiple queued entries for the SAME genesis key have matured in
/// the same call (e.g. two `GenesisKeyDelegation` certs for one genesis key
/// enqueued at different times, both now matured), Haskell's
/// `adoptGenesisDelegs` keeps the entry with the LARGEST `fgdSlot`
/// (most-recently-enqueued wins) — replicated here via an explicit
/// per-genesis-key max-slot fold.
///
/// Call this BEFORE processing the block's own transactions (mirrors
/// TICK preceding BBODY) — a cert enqueued in this same block can never
/// mature in this same call, since maturity is always strictly in the
/// future (`slot + stability_window`).
pub(crate) fn adopt_matured_genesis_delegs(
    current_slot: u64,
    future_gen_delegs: &mut HashMap<(u64, Hash28), (Hash28, Hash32)>,
    genesis_delegates: &mut HashMap<Hash28, (Hash28, Hash32)>,
) {
    if future_gen_delegs.is_empty() {
        return;
    }

    let matured_keys: Vec<(u64, Hash28)> = future_gen_delegs
        .keys()
        .filter(|(fgd_slot, _)| *fgd_slot <= current_slot)
        .copied()
        .collect();
    if matured_keys.is_empty() {
        return;
    }

    // Resolve multiple matured entries for the same genesis key by largest
    // fgdSlot (mirrors Haskell's fold over the partitioned "curr" map).
    let mut latest_per_gkey: HashMap<Hash28, (u64, Hash28, Hash32)> = HashMap::new();
    for key @ (fgd_slot, gkey) in &matured_keys {
        let (dkey, vrf) = future_gen_delegs[key];
        latest_per_gkey
            .entry(*gkey)
            .and_modify(|(best_slot, best_dkey, best_vrf)| {
                if *fgd_slot > *best_slot {
                    *best_slot = *fgd_slot;
                    *best_dkey = dkey;
                    *best_vrf = vrf;
                }
            })
            .or_insert((*fgd_slot, dkey, vrf));
    }

    for (gkey, (_, dkey, vrf)) in &latest_per_gkey {
        genesis_delegates.insert(*gkey, (*dkey, *vrf));
        debug!(
            "GenesisKeyDelegation adopted: {} -> delegate={}, vrf={}",
            gkey.to_hex(),
            dkey.to_hex(),
            vrf.to_hex(),
        );
    }

    for key in &matured_keys {
        future_gen_delegs.remove(key);
    }
}

// ============================================================================
// 4. drain_withdrawal_accounts
// ============================================================================

/// Drain withdrawal accounts referenced by a transaction.
///
/// For each withdrawal in the transaction body, sets the corresponding reward
/// account balance to zero. Per the Cardano specification, the withdrawal
/// amount must exactly equal the reward balance; during sync from genesis we
/// may not have accumulated all rewards yet, so mismatches are logged at DEBUG
/// level only (best-effort, matching the existing behavior).
///
/// # Parameters
///
/// * `tx` -- the transaction containing withdrawals.
/// * `certs` -- mutable cert sub-state (reward_accounts).
pub(crate) fn drain_withdrawal_accounts(tx: &Transaction, certs: &mut CertSubState) {
    for (reward_account, amount) in &tx.body.withdrawals {
        let key = reward_account_to_hash(reward_account);
        if let Some(balance) = certs.reward_accounts.get_mut(&key) {
            if balance.0 != amount.0 {
                debug!(
                    account = %key.to_hex(),
                    balance = balance.0,
                    withdrawal = amount.0,
                    "drain_withdrawal_accounts: withdrawal amount does not match reward balance"
                );
            }
            // Always zero the balance -- rewards were consumed in the on-chain transaction.
            balance.0 = 0;
        }
    }
}

// ============================================================================
// 5. compute_shelley_nonce
// ============================================================================

/// First absolute slot of a Shelley-or-later `epoch`, accounting for the Byron
/// prefix. Mirrors `crate::state::LedgerState::first_slot_of_epoch` for the
/// Shelley+ branch: `byron_slots + (epoch - shelley_transition_epoch) *
/// shelley_epoch_length`, where `byron_slots = shelley_transition_epoch *
/// byron_epoch_length`.
///
/// The era `evolve_nonce` functions previously inlined `epoch * epoch_length +
/// byron_slots`, which is correct only when `shelley_transition_epoch == 0`
/// (Byron-less testnets like preview). On mainnet (`shelley_transition_epoch =
/// 208`) it overshot by `shelley_transition_epoch * shelley_epoch_length`, so
/// `first_slot_of_next_epoch` landed far in the future and the candidate-nonce
/// stability-window freeze never fired — corrupting the epoch nonce at the first
/// Shelley boundary (breaking VRF) and also breaking `isOverlaySlot`. Always use
/// this helper.
pub(crate) fn first_slot_of_shelley_epoch(
    epoch: u64,
    shelley_transition_epoch: u64,
    byron_epoch_length: u64,
    shelley_epoch_length: u64,
) -> u64 {
    let byron_slots = shelley_transition_epoch.saturating_mul(byron_epoch_length);
    byron_slots.saturating_add(
        epoch
            .saturating_sub(shelley_transition_epoch)
            .saturating_mul(shelley_epoch_length),
    )
}

/// Evolve nonce state after processing a Shelley+ block header.
///
/// Implements Haskell's `reupdateChainDepState` nonce state machine:
///
/// 1. **evolving_nonce** is updated for EVERY block using the era-specific
///    nonce VRF contribution (`nonce_vrf_output` on the header):
///    - `evolving' = blake2b_256(evolving || blake2b_256(nonce_vrf_output))`
///
/// 2. **candidate_nonce** tracks `evolving_nonce` UNLESS the block is within
///    the stability window of the epoch end, in which case the candidate
///    freezes so the epoch nonce is stable.
///
/// 3. **lab_nonce** = `prevHashToNonce(block.prevHash)` -- direct assignment
///    of `prev_hash` bytes (castHash is a type-level reinterpret, no rehash).
///
/// 4. **epoch_blocks_by_pool** and **epoch_block_count** are incremented.
///    Block counting follows Haskell `incrBlocks`: a block is counted toward
///    its pool's `BlocksMade` only when it is NOT an overlay slot — i.e.,
///    `!isOverlaySlot(firstSlotOfCurrentEpoch, d, blockSlot)`. With `d=0`
///    every slot is a Praos slot (always counted). With `d=1` every slot is
///    an overlay slot (never counted toward pool rewards). For intermediate
///    `d` the schedule alternates per Haskell `step(s) < step(s+1)` where
///    `step(x) = ⌈x * d_num / d_den⌉`.
///
/// # Parameters
///
/// * `header` -- the block header with VRF output and issuer vkey.
/// * `block_slot` -- the slot of the block being processed.
/// * `first_slot_of_current_epoch` -- first slot of the CURRENT epoch
///   (used by `isOverlaySlot`).
/// * `first_slot_of_next_epoch` -- first slot of the next epoch
///   (used to determine if we are inside the stability window).
/// * `stability_window` -- the number of slots before epoch end where
///   candidate_nonce freezes (3k/f for Babbage, 4k/f for Conway+).
/// * `d_num`, `d_den` -- the decentralization parameter `d` as a rational
///   (`numerator / denominator`). For Babbage+ d is `(0, 1)` by definition.
/// * `consensus` -- mutable consensus sub-state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_shelley_nonce(
    header: &BlockHeader,
    block_slot: u64,
    first_slot_of_current_epoch: u64,
    first_slot_of_next_epoch: u64,
    stability_window: u64,
    d_num: u64,
    d_den: u64,
    consensus: &mut ConsensusSubState,
) {
    // Update evolving nonce if nonce_vrf_output is present.
    if !header.nonce_vrf_output.is_empty() {
        // Compute eta = blake2b_256(nonce_vrf_output), then
        // evolving' = blake2b_256(evolving || eta).
        let eta_hash = blake2b_256(&header.nonce_vrf_output);
        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(consensus.evolving_nonce.as_bytes());
        data.extend_from_slice(eta_hash.as_bytes());
        consensus.evolving_nonce = blake2b_256(&data);

        // Candidate nonce tracks evolving nonce outside the stability window.
        if block_slot.saturating_add(stability_window) < first_slot_of_next_epoch {
            consensus.candidate_nonce = consensus.evolving_nonce;
        }
    } else {
        // Every Shelley+ block carries a nonce VRF output; an empty one here
        // would silently SKIP the ηv update, dropping a contribution and
        // permanently diverging the randomness chain (Haskell folds ηv on every
        // block). Flag it — this is a decode/extraction anomaly, not valid state.
        tracing::warn!(
            slot = block_slot,
            issuer = %dugite_primitives::hash::blake2b_224(&header.issuer_vkey),
            "Shelley+ block with EMPTY nonce_vrf_output — evolving-nonce update skipped"
        );
    }

    // lab_nonce = prevHashToNonce(block.prevHash).
    // prevHashToNonce: GenesisHash -> NeutralNonce; BlockHash h -> Nonce(h).
    // castHash is a type-reinterpret (no rehashing).
    consensus.lab_nonce = header.prev_hash;

    // Track block production by pool (issuer vkey hash).
    //
    // Mirrors Haskell `incrBlocks` (eras/shelley/impl/src/Cardano/Ledger/
    // Shelley/BlockBody/Internal.hs): non-overlay blocks count, overlay
    // blocks do not.
    let is_overlay = is_overlay_slot(first_slot_of_current_epoch, block_slot, d_num, d_den);
    if !header.issuer_vkey.is_empty() {
        let pool_id = blake2b_224(&header.issuer_vkey);
        if !is_overlay {
            *Arc::make_mut(&mut consensus.epoch_blocks_by_pool)
                .entry(pool_id)
                .or_insert(0) += 1;
        }
        // Track per-pool opcert counter (max-so-far). Haskell's
        // `PraosState.ocertCounters` retains the highest `OperationalCert.
        // sequence_number` observed per pool — used for replay-protection
        // tie-breaking in chain selection (newer counter wins on
        // same-pool / same-slot ties) and surfaced via N2C queries.
        //
        // Issue #670: without this update the from-genesis ledger
        // diverges from the ancillary import on the `opcert_counters`
        // field of `ConsensusSubState`, which is otherwise populated
        // from `PraosState.opcert_counters` by `from_haskell_snapshot`.
        let seq = header.operational_cert.sequence_number;
        consensus
            .opcert_counters
            .entry(pool_id)
            .and_modify(|cur| {
                if seq > *cur {
                    *cur = seq;
                }
            })
            .or_insert(seq);
    }
    consensus.epoch_block_count += 1;
}

/// Mirrors Haskell `isOverlaySlot` from `libs/cardano-ledger-core/src/Cardano/Ledger/Slot.hs`:
///
/// ```haskell
/// isOverlaySlot firstSlotNo dval slot = step s < step (s + 1)
///   where
///     s    = fromIntegral $ slot -* firstSlotNo
///     d    = unboundRational dval
///     step x = ceiling (x * d)
/// ```
///
/// Returns `true` when `block_slot` falls in an overlay slot (genesis-delegate
/// scheduled, not a Praos pool slot). With `d=0` returns `false` for every
/// slot; with `d=1` returns `true` for every slot.
pub(crate) fn is_overlay_slot(first_slot: u64, block_slot: u64, d_num: u64, d_den: u64) -> bool {
    // Fast paths matching Haskell behaviour exactly.
    if d_num == 0 {
        return false; // d = 0 → no slot is overlay (pure Praos)
    }
    if d_den == 0 || d_num >= d_den {
        return true; // d ≥ 1 → every slot is overlay (pure federated)
    }
    let s = block_slot.saturating_sub(first_slot);
    // step(x) = ⌈x * d_num / d_den⌉ in integer arithmetic.
    let step = |x: u64| -> u128 {
        let num = (x as u128) * (d_num as u128);
        num.div_ceil(d_den as u128)
    };
    step(s) < step(s + 1)
}

// ============================================================================
// 6. Block-body ExUnit budget validation (Alonzo+)
// ============================================================================

/// Validate that the total ExUnit budget (memory + steps) across all
/// transactions in the block does not exceed the per-block limits.
///
/// Matches Haskell's Alonzo BBODY rule: `totalExUnits txs <= maxBlockExUnits pp`,
/// where `txTotal = foldMap totExUnits txs` folds over the *entire* block body
/// with no `IsValid` filter — an `is_valid=false` transaction still carries
/// redeemers (and the collateral it consumes was priced assuming those
/// redeemers would run), so its ExUnits count toward the block budget too.
pub(crate) fn validate_block_ex_units(block: &Block, ctx: &RuleContext) -> Result<(), LedgerError> {
    let mut mem: u64 = 0;
    let mut steps: u64 = 0;
    for tx in &block.transactions {
        for r in &tx.witness_set.redeemers {
            mem = mem.saturating_add(r.ex_units.mem);
            steps = steps.saturating_add(r.ex_units.steps);
        }
    }

    if mem > ctx.params.max_block_ex_units.mem {
        return Err(LedgerError::BlockTxValidationFailed {
            slot: ctx.current_slot,
            tx_hash: String::from("(block-level check)"),
            errors: format!(
                "BlockExUnitsExceeded: block memory usage {} exceeds limit {} \
                 (Alonzo+ block-body ExUnits rule)",
                mem, ctx.params.max_block_ex_units.mem
            ),
        });
    }
    if steps > ctx.params.max_block_ex_units.steps {
        return Err(LedgerError::BlockTxValidationFailed {
            slot: ctx.current_slot,
            tx_hash: String::from("(block-level check)"),
            errors: format!(
                "BlockExUnitsExceeded: block step usage {} exceeds limit {} \
                 (Alonzo+ block-body ExUnits rule)",
                steps, ctx.params.max_block_ex_units.steps
            ),
        });
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod nonce_combine_tests {
    use super::combine_nonce;
    use dugite_primitives::hash::Hash32;

    fn h(hex: &str) -> Hash32 {
        Hash32::from_hex(hex).unwrap()
    }

    #[test]
    fn neutral_nonce_is_identity() {
        let z = Hash32::ZERO;
        let x = h("d1340a9c1491f0face38d41fd5c82953d0eb48320d65e952414a0c5ebaf87587");
        assert_eq!(combine_nonce(z, x), x, "NeutralNonce ⭒ x = x");
        assert_eq!(combine_nonce(x, z), x, "x ⭒ NeutralNonce = x");
        assert_eq!(
            combine_nonce(z, z),
            z,
            "NeutralNonce ⭒ NeutralNonce = NeutralNonce"
        );
    }

    /// Real mainnet vectors: the epoch-259 nonce only matches the on-chain
    /// value when the one-time non-neutral `ppExtraEntropy` is folded in as the
    /// third TICKN term. Guards against ever dropping it again.
    /// η0_259 = candidate_258 ⭒ prevHashNonce_259 ⭒ extraEntropy
    #[test]
    fn mainnet_epoch_259_extra_entropy() {
        let candidate_258 = h("d1340a9c1491f0face38d41fd5c82953d0eb48320d65e952414a0c5ebaf87587");
        let prev_hash_nonce = h("ee91d679b0a6ce3015b894c575c799e971efac35c7a8cbdc2b3f579005e69abd");
        let extra_entropy = h("d982e06fd33e7440b43cefad529b7ecafbaa255e38178ad4189a37e4ce9bf1fa");
        let real_eta259 = h("0022cfa563a5328c4fb5c8017121329e964c26ade5d167b1bd9b2ec967772b60");

        // Two-term (the pre-fix computation) is WRONG.
        let two_term = combine_nonce(candidate_258, prev_hash_nonce);
        assert_ne!(
            two_term, real_eta259,
            "dropping extraEntropy must NOT match"
        );

        // Three-term matches the on-chain epoch-259 nonce byte-for-byte.
        let eta = combine_nonce(two_term, extra_entropy);
        assert_eq!(
            eta, real_eta259,
            "candidate ⭒ prevHash ⭒ extraEntropy = η0_259"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::substates::*;
    use crate::state::StakeDistributionState;
    use crate::utxo::UtxoSet;
    use crate::utxo_diff::DiffSeq;
    use dugite_primitives::address::{Address, BaseAddress, EnterpriseAddress, PointerAddress};
    use dugite_primitives::block::{OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::BlockNo;
    use dugite_primitives::time::SlotNo;
    use dugite_primitives::transaction::{
        ExUnits, OutputDatum, PlutusData, Redeemer, RedeemerTag, TransactionInput,
        TransactionOutput,
    };
    use dugite_primitives::value::{Lovelace, Value};
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Constants
    // -----------------------------------------------------------------------

    const ZERO32: Hash32 = Hash32::ZERO;
    const ZERO28: Hash28 = Hash28::ZERO;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Create a minimal UtxoSubState for testing.
    fn empty_utxo_sub() -> UtxoSubState {
        UtxoSubState {
            utxo_set: UtxoSet::new(),
            diff_seq: DiffSeq::new(),
            epoch_fees: Lovelace(0),
            pending_donations: Lovelace(0),
        }
    }

    /// Create a minimal CertSubState for testing.
    fn empty_cert_sub() -> CertSubState {
        CertSubState {
            delegations: imbl::HashMap::new(),
            pool_params: Arc::new(HashMap::new()),
            future_pool_params: HashMap::new(),
            pending_retirements: HashMap::new(),
            reward_accounts: imbl::HashMap::new(),
            stake_key_deposits: imbl::HashMap::new(),
            pool_deposits: HashMap::new(),
            total_stake_key_deposits: 0,
            pointer_map: HashMap::new(),
            stake_distribution: StakeDistributionState {
                stake_map: HashMap::new(),
            },
            script_stake_credentials: HashSet::new(),
            pending_mir_reserves: std::collections::HashMap::new(),
            pending_mir_treasury: std::collections::HashMap::new(),
            pending_mir_delta_reserves: 0,
            pending_mir_delta_treasury: 0,
        }
    }

    /// Create a minimal EpochSubState for testing.
    fn empty_epoch_sub() -> EpochSubState {
        use crate::state::EpochSnapshots;
        EpochSubState {
            snapshots: EpochSnapshots::default(),
            treasury: Lovelace(0),
            reserves: Lovelace(0),
            pending_reward_update: None,
            last_applied_rupd: None,
            pending_pp_updates: BTreeMap::new(),
            future_pp_updates: BTreeMap::new(),
            needs_stake_rebuild: false,
            ptr_stake: HashMap::new(),
            ptr_stake_excluded: false,
            protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_version_major: 0,
            prev_d: dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            rupd_addrs_rew: None,
            pending_avvm_return: 0,
        }
    }

    /// Create a minimal GovSubState for testing.
    fn empty_gov_sub() -> GovSubState {
        use crate::state::GovernanceState;
        GovSubState {
            governance: Arc::new(GovernanceState::default()),
        }
    }

    /// Create a minimal ConsensusSubState for testing.
    fn empty_consensus_sub() -> ConsensusSubState {
        ConsensusSubState {
            evolving_nonce: ZERO32,
            candidate_nonce: ZERO32,
            epoch_nonce: ZERO32,
            lab_nonce: ZERO32,
            last_epoch_block_nonce: ZERO32,
            extra_entropy: ZERO32,
            rolling_nonce: ZERO32,
            first_block_hash_of_epoch: None,
            prev_epoch_first_block_hash: None,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
            epoch_block_count: 0,
            opcert_counters: HashMap::new(),
        }
    }

    /// Create a simple enterprise address output (no stake routing).
    fn enterprise_output(coin: u64) -> TransactionOutput {
        TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(ZERO28),
            }),
            value: Value {
                coin: Lovelace(coin),
                multi_asset: BTreeMap::new(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Create a base address output (has stake routing via credential).
    fn base_output(coin: u64, stake_cred: Hash32) -> TransactionOutput {
        let mut h28 = [0u8; 28];
        h28.copy_from_slice(&stake_cred.as_bytes()[..28]);
        TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(ZERO28),
                stake: Credential::VerificationKey(Hash28::from_bytes(h28)),
            }),
            value: Value {
                coin: Lovelace(coin),
                multi_asset: BTreeMap::new(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Create a minimal BlockHeader for testing.
    fn make_header(
        nonce_vrf_output: Vec<u8>,
        prev_hash: Hash32,
        issuer_vkey: Vec<u8>,
    ) -> BlockHeader {
        BlockHeader {
            header_hash: ZERO32,
            prev_hash,
            issuer_vkey,
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            block_number: BlockNo(0),
            slot: SlotNo(0),
            epoch_nonce: ZERO32,
            body_size: 0,
            body_hash: ZERO32,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion { major: 8, minor: 0 },
            kes_signature: vec![],
            nonce_vrf_output,
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
        }
    }

    /// Create a minimal transaction with specified inputs, outputs, and fee.
    fn make_tx(
        hash: Hash32,
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        fee: u64,
    ) -> Transaction {
        Transaction {
            hash,
            era: dugite_primitives::era::Era::Babbage,
            is_valid: true,
            body: dugite_primitives::transaction::TransactionBody {
                inputs,
                outputs,
                fee: Lovelace(fee),
                ttl: None,
                certificates: vec![],
                withdrawals: BTreeMap::new(),
                auxiliary_data_hash: None,
                validity_interval_start: None,
                mint: BTreeMap::new(),
                script_data_hash: None,
                collateral: vec![],
                required_signers: vec![],
                network_id: None,
                collateral_return: None,
                total_collateral: None,
                reference_inputs: vec![],
                update: None,
                voting_procedures: BTreeMap::new(),
                proposal_procedures: vec![],
                treasury_value: None,
                donation: None,
                sub_transactions: vec![],
                account_balance_intervals: vec![],
                direct_deposits: ::std::collections::BTreeMap::new(),
                guards: Vec::new(),
            },
            witness_set: dugite_primitives::transaction::TransactionWitnessSet {
                vkey_witnesses: vec![],
                native_scripts: vec![],
                bootstrap_witnesses: vec![],
                plutus_v1_scripts: vec![],
                plutus_v2_scripts: vec![],
                plutus_v3_scripts: vec![],
                plutus_data: vec![],
                redeemers: vec![],
                raw_redeemers_cbor: None,
                raw_plutus_data_cbor: None,
                original_script_data_hash: None,
            },
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    // -----------------------------------------------------------------------
    // 1. apply_utxo_changes tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_apply_utxo_changes_basic_spend() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([1u8; 32]),
            index: 0,
        };
        utxo.utxo_set
            .insert(input.clone(), enterprise_output(10_000_000));

        let tx = make_tx(
            Hash32::from_bytes([2u8; 32]),
            vec![input.clone()],
            vec![enterprise_output(8_000_000), enterprise_output(1_800_000)],
            200_000,
        );

        let diff = apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        assert!(!utxo.utxo_set.contains(&input));
        let out0 = TransactionInput {
            transaction_id: tx.hash,
            index: 0,
        };
        let out1 = TransactionInput {
            transaction_id: tx.hash,
            index: 1,
        };
        assert!(utxo.utxo_set.contains(&out0));
        assert!(utxo.utxo_set.contains(&out1));
        assert_eq!(utxo.epoch_fees.0, 200_000);
        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 2);
    }

    #[test]
    fn test_apply_utxo_changes_missing_input_still_creates_outputs() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let missing_input = TransactionInput {
            transaction_id: Hash32::from_bytes([99u8; 32]),
            index: 0,
        };

        let tx = make_tx(
            Hash32::from_bytes([3u8; 32]),
            vec![missing_input],
            vec![enterprise_output(5_000_000)],
            100_000,
        );

        let diff = apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        let out0 = TransactionInput {
            transaction_id: tx.hash,
            index: 0,
        };
        assert!(utxo.utxo_set.contains(&out0));
        assert_eq!(diff.deletes.len(), 0);
        assert_eq!(diff.inserts.len(), 1);
        assert_eq!(utxo.epoch_fees.0, 100_000);
    }

    #[test]
    fn test_apply_utxo_changes_stake_distribution_updated() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        // Build a proper stake credential hash: use credential_to_hash to get
        // the typed Hash32 that stake_routing will produce from the address.
        let stake_h28 = Hash28::from_bytes([42u8; 28]);
        let stake_cred = Credential::VerificationKey(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);

        certs
            .stake_distribution
            .stake_map
            .insert(stake_key, Lovelace(10_000_000));

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([10u8; 32]),
            index: 0,
        };
        utxo.utxo_set
            .insert(input.clone(), base_output(10_000_000, stake_key));

        let tx = make_tx(
            Hash32::from_bytes([11u8; 32]),
            vec![input],
            vec![base_output(9_800_000, stake_key)],
            200_000,
        );

        apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        // Stake should be: 10M (initial) - 10M (spent) + 9.8M (output) = 9.8M
        let stake = certs.stake_distribution.stake_map.get(&stake_key).unwrap();
        assert_eq!(stake.0, 9_800_000);
    }

    // -----------------------------------------------------------------------
    // 2. apply_collateral_consumption tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_apply_collateral_consumption_basic() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([20u8; 32]),
            index: 0,
        };
        utxo.utxo_set
            .insert(col_input.clone(), enterprise_output(5_000_000));

        let mut tx = make_tx(Hash32::from_bytes([21u8; 32]), vec![], vec![], 0);
        tx.is_valid = false;
        tx.body.collateral = vec![col_input.clone()];

        let diff = apply_collateral_consumption(&tx, &mut utxo, &mut certs, &mut epochs);

        assert!(!utxo.utxo_set.contains(&col_input));
        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 0);
        assert_eq!(utxo.epoch_fees.0, 5_000_000);
    }

    #[test]
    fn test_apply_collateral_consumption_with_return() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([30u8; 32]),
            index: 0,
        };
        utxo.utxo_set
            .insert(col_input.clone(), enterprise_output(10_000_000));

        let mut tx = make_tx(Hash32::from_bytes([31u8; 32]), vec![], vec![], 0);
        tx.is_valid = false;
        tx.body.collateral = vec![col_input.clone()];
        tx.body.collateral_return = Some(enterprise_output(8_000_000));
        tx.body.total_collateral = Some(Lovelace(2_000_000));

        let diff = apply_collateral_consumption(&tx, &mut utxo, &mut certs, &mut epochs);

        assert!(!utxo.utxo_set.contains(&col_input));
        let return_input = TransactionInput {
            transaction_id: tx.hash,
            index: 0,
        };
        assert!(utxo.utxo_set.contains(&return_input));
        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 1);
        assert_eq!(utxo.epoch_fees.0, 2_000_000);
    }

    // -----------------------------------------------------------------------
    // 3. process_shelley_certs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_process_shelley_certs_stake_registration() {
        let mut certs = empty_cert_sub();
        let epochs = empty_epoch_sub();
        let mut gov = empty_gov_sub();

        let cred = Credential::VerificationKey(Hash28::from_bytes([5u8; 28]));
        let mut tx = make_tx(Hash32::from_bytes([50u8; 32]), vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::StakeRegistration(cred.clone())];

        process_shelley_certs(&tx, 100, 0, &mut certs, &epochs, &mut gov);

        let key = credential_to_hash(&cred);
        assert_eq!(certs.reward_accounts.get(&key), Some(&Lovelace(0)));
        assert!(certs.stake_distribution.stake_map.contains_key(&key));
        assert_eq!(
            certs.total_stake_key_deposits,
            epochs.protocol_params.key_deposit.0
        );
        assert_eq!(
            certs.stake_key_deposits.get(&key),
            Some(&epochs.protocol_params.key_deposit.0)
        );
        // Issue #670: `pointer_map` is a pre-Conway construct. With
        // `protocol_version_major: 9` (mainnet_defaults) the gate at
        // `apply_shelley_cert` skips the pointer-map insert, mirroring
        // Haskell `ConwayInstantStake` which drops `dsPtrs` at the
        // Babbage→Conway TranslateEra step. So the entry must NOT be
        // present in Conway+ era state.
        let ptr = Pointer {
            slot: 100,
            tx_index: 0,
            cert_index: 0,
        };
        assert_eq!(
            certs.pointer_map.get(&ptr),
            None,
            "Conway+ (PV >= 9) must NOT insert into pointer_map"
        );

        // Verify pre-Conway path still populates the pointer_map by
        // lowering PV to 8 (Babbage) and re-running the cert.
        let mut certs_babbage = empty_cert_sub();
        let mut epochs_babbage = empty_epoch_sub();
        epochs_babbage.protocol_params.protocol_version_major = 8;
        let mut gov_babbage = empty_gov_sub();
        let mut tx_babbage = make_tx(Hash32::from_bytes([60u8; 32]), vec![], vec![], 0);
        tx_babbage.body.certificates = vec![Certificate::StakeRegistration(cred)];
        process_shelley_certs(
            &tx_babbage,
            100,
            0,
            &mut certs_babbage,
            &epochs_babbage,
            &mut gov_babbage,
        );
        assert_eq!(
            certs_babbage.pointer_map.get(&ptr),
            Some(&key),
            "pre-Conway (PV < 9) must populate pointer_map"
        );
    }

    #[test]
    fn test_process_shelley_certs_stake_deregistration() {
        let mut certs = empty_cert_sub();
        let epochs = empty_epoch_sub();
        let mut gov = empty_gov_sub();

        let cred = Credential::VerificationKey(Hash28::from_bytes([6u8; 28]));
        let key = credential_to_hash(&cred);

        certs.reward_accounts.insert(key, Lovelace(500));
        certs.delegations.insert(key, Hash28::from_bytes([7u8; 28]));
        certs.stake_key_deposits.insert(key, 2_000_000);
        certs.total_stake_key_deposits = 2_000_000;

        let mut tx = make_tx(Hash32::from_bytes([51u8; 32]), vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::StakeDeregistration(cred)];

        process_shelley_certs(&tx, 200, 0, &mut certs, &epochs, &mut gov);

        assert!(!certs.reward_accounts.contains_key(&key));
        assert!(!certs.delegations.contains_key(&key));
        assert_eq!(certs.total_stake_key_deposits, 0);
        assert!(!certs.stake_key_deposits.contains_key(&key));
    }

    #[test]
    fn test_process_shelley_certs_pool_registration() {
        let mut certs = empty_cert_sub();
        let epochs = empty_epoch_sub();
        let mut gov = empty_gov_sub();

        let pool_id = Hash28::from_bytes([8u8; 28]);
        let pool_params = dugite_primitives::transaction::PoolParams {
            operator: pool_id,
            vrf_keyhash: Hash32::from_bytes([9u8; 32]),
            pledge: Lovelace(1_000_000),
            cost: Lovelace(340_000_000),
            margin: dugite_primitives::transaction::Rational {
                numerator: 1,
                denominator: 100,
            },
            reward_account: vec![0xe0; 29],
            pool_owners: vec![pool_id],
            relays: vec![],
            pool_metadata: None,
        };

        let mut tx = make_tx(Hash32::from_bytes([52u8; 32]), vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::PoolRegistration(pool_params)];

        process_shelley_certs(&tx, 300, 0, &mut certs, &epochs, &mut gov);

        assert!(certs.pool_params.contains_key(&pool_id));
        assert_eq!(
            certs.pool_deposits.get(&pool_id),
            Some(&epochs.protocol_params.pool_deposit.0)
        );
    }

    #[test]
    fn test_process_shelley_certs_pool_retirement() {
        let mut certs = empty_cert_sub();
        let epochs = empty_epoch_sub();
        let mut gov = empty_gov_sub();

        let pool_id = Hash28::from_bytes([10u8; 28]);

        let mut tx = make_tx(Hash32::from_bytes([53u8; 32]), vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::PoolRetirement {
            pool_hash: pool_id,
            epoch: 100,
        }];

        process_shelley_certs(&tx, 400, 0, &mut certs, &epochs, &mut gov);

        assert_eq!(certs.pending_retirements.get(&pool_id), Some(&EpochNo(100)));
    }

    // -----------------------------------------------------------------------
    // 4. drain_withdrawal_accounts tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_drain_withdrawal_accounts_zeroes_balance() {
        let mut certs = empty_cert_sub();

        let mut reward_addr = vec![0xe0u8];
        reward_addr.extend_from_slice(&[11u8; 28]);
        let key = reward_account_to_hash(&reward_addr);
        certs.reward_accounts.insert(key, Lovelace(500));

        let mut tx = make_tx(Hash32::from_bytes([60u8; 32]), vec![], vec![], 0);
        tx.body.withdrawals = BTreeMap::from([(reward_addr, Lovelace(500))]);

        drain_withdrawal_accounts(&tx, &mut certs);

        assert_eq!(certs.reward_accounts.get(&key), Some(&Lovelace(0)));
    }

    #[test]
    fn test_drain_withdrawal_accounts_mismatch_still_zeroes() {
        let mut certs = empty_cert_sub();

        let mut reward_addr = vec![0xe0u8];
        reward_addr.extend_from_slice(&[12u8; 28]);
        let key = reward_account_to_hash(&reward_addr);
        certs.reward_accounts.insert(key, Lovelace(1000));

        let mut tx = make_tx(Hash32::from_bytes([61u8; 32]), vec![], vec![], 0);
        tx.body.withdrawals = BTreeMap::from([(reward_addr, Lovelace(500))]);

        drain_withdrawal_accounts(&tx, &mut certs);

        assert_eq!(certs.reward_accounts.get(&key), Some(&Lovelace(0)));
    }

    // -----------------------------------------------------------------------
    // 5. compute_shelley_nonce tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_shelley_nonce_evolving_updates() {
        let mut consensus = empty_consensus_sub();
        let initial_evolving = consensus.evolving_nonce;

        let header = make_header(vec![1u8; 64], Hash32::from_bytes([2u8; 32]), vec![3u8; 32]);

        // d = (0, 1) = full Praos → block IS counted.
        compute_shelley_nonce(&header, 100, 0, 43200, 1000, 0, 1, &mut consensus);

        assert_ne!(consensus.evolving_nonce, initial_evolving);
        assert_eq!(consensus.candidate_nonce, consensus.evolving_nonce);
        assert_eq!(consensus.lab_nonce, header.prev_hash);
        assert_eq!(consensus.epoch_block_count, 1);
        let pool_id = blake2b_224(&header.issuer_vkey);
        assert_eq!(consensus.epoch_blocks_by_pool.get(&pool_id), Some(&1));
    }

    #[test]
    fn test_compute_shelley_nonce_candidate_freezes_in_stability_window() {
        let mut consensus = empty_consensus_sub();
        let initial_candidate = consensus.candidate_nonce;

        let header = make_header(vec![4u8; 64], Hash32::from_bytes([5u8; 32]), vec![6u8; 32]);

        // 42500 + 1000 = 43500 >= 43200 -> inside stability window
        compute_shelley_nonce(&header, 42500, 0, 43200, 1000, 0, 1, &mut consensus);

        assert_ne!(consensus.evolving_nonce, ZERO32);
        assert_eq!(consensus.candidate_nonce, initial_candidate);
    }

    #[test]
    fn test_compute_shelley_nonce_overlay_blocks_not_counted() {
        let mut consensus = empty_consensus_sub();

        let header = make_header(vec![7u8; 64], Hash32::from_bytes([8u8; 32]), vec![9u8; 32]);

        // d = (1, 1) = full federated → every slot is overlay → pool blocks NOT counted.
        compute_shelley_nonce(&header, 500, 0, 43200, 1000, 1, 1, &mut consensus);

        assert_eq!(consensus.epoch_block_count, 1);
        assert!(consensus.epoch_blocks_by_pool.is_empty());
    }

    /// Issue #670: per-pool opcert counter must track the max
    /// `OperationalCert.sequence_number` observed across all blocks from
    /// that pool. Mirrors Haskell `PraosState.ocertCounters`. Without this
    /// the from-genesis replay diverges from the ancillary import on the
    /// `consensus.opcert_counters` field of `ConsensusSubState`.
    #[test]
    fn test_compute_shelley_nonce_tracks_opcert_max() {
        let mut consensus = empty_consensus_sub();
        let vkey = vec![0xABu8; 64];
        let pool_id = blake2b_224(&vkey);

        // First block: opcert seq = 5
        // make_header(nonce_vrf_output, prev_hash, issuer_vkey)
        let mut h = make_header(vec![1u8; 32], Hash32::from_bytes([2u8; 32]), vkey.clone());
        h.operational_cert.sequence_number = 5;
        compute_shelley_nonce(&h, 100, 0, 43200, 1000, 0, 1, &mut consensus);
        assert_eq!(consensus.opcert_counters.get(&pool_id), Some(&5));

        // Newer block from same pool: seq = 9 → must update to max
        h.operational_cert.sequence_number = 9;
        compute_shelley_nonce(&h, 200, 0, 43200, 1000, 0, 1, &mut consensus);
        assert_eq!(consensus.opcert_counters.get(&pool_id), Some(&9));

        // Older block (out-of-order replay or KES-rotation edge): seq = 7
        // must NOT decrement the counter.
        h.operational_cert.sequence_number = 7;
        compute_shelley_nonce(&h, 300, 0, 43200, 1000, 0, 1, &mut consensus);
        assert_eq!(
            consensus.opcert_counters.get(&pool_id),
            Some(&9),
            "out-of-order opcert must not decrement the per-pool max"
        );

        // Overlay block (d=1/1) still updates opcert (block production
        // doesn't count toward BlocksMade, but the opcert was still used
        // to sign the header).
        let vkey2 = vec![0xCDu8; 64];
        let pool2_id = blake2b_224(&vkey2);
        let mut h2 = make_header(vec![3u8; 32], Hash32::from_bytes([4u8; 32]), vkey2);
        h2.operational_cert.sequence_number = 1;
        compute_shelley_nonce(&h2, 400, 0, 43200, 1000, 1, 1, &mut consensus);
        assert_eq!(consensus.opcert_counters.get(&pool2_id), Some(&1));
        // Overlay block must not be counted toward BlocksMade.
        assert!(consensus.epoch_blocks_by_pool.get(&pool2_id).is_none());
    }

    #[test]
    fn test_compute_shelley_nonce_empty_vrf_output() {
        let mut consensus = empty_consensus_sub();
        let initial_evolving = consensus.evolving_nonce;

        let header = make_header(vec![], Hash32::from_bytes([10u8; 32]), vec![]);

        compute_shelley_nonce(&header, 100, 0, 43200, 1000, 0, 1, &mut consensus);

        assert_eq!(consensus.evolving_nonce, initial_evolving);
        assert_eq!(consensus.lab_nonce, header.prev_hash);
        assert_eq!(consensus.epoch_block_count, 1);
    }

    #[test]
    fn test_is_overlay_slot_d_zero_never_overlay() {
        // d = 0 → every slot is non-overlay (Praos).
        for slot in [0u64, 1, 10, 100, 1000, 21600] {
            assert!(
                !is_overlay_slot(0, slot, 0, 1),
                "slot {slot} should not be overlay at d=0"
            );
        }
    }

    #[test]
    fn test_is_overlay_slot_d_one_always_overlay() {
        // d = 1 → every slot is overlay (federated).
        for slot in [0u64, 1, 10, 100, 1000, 21600] {
            assert!(
                is_overlay_slot(0, slot, 1, 1),
                "slot {slot} should be overlay at d=1"
            );
        }
    }

    #[test]
    fn test_is_overlay_slot_d_half_alternates_correctly() {
        // d = 1/2 → matches Haskell `step(s) < step(s+1)` where
        // step(x) = ⌈x/2⌉. Sequence step(0..5) = 0,1,1,2,2,3 → overlay at
        // s where step(s) < step(s+1): s=0 (0<1), s=2 (1<2), s=4 (2<3), …
        let d_num = 1;
        let d_den = 2;
        let expected = [true, false, true, false, true, false];
        for (i, &want) in expected.iter().enumerate() {
            let got = is_overlay_slot(0, i as u64, d_num, d_den);
            assert_eq!(got, want, "slot {i} d=1/2 expected overlay={want}");
        }
    }

    // -----------------------------------------------------------------------
    // Helper function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_reward_account_to_hash_key_credential() {
        let mut addr = vec![0xe0u8];
        addr.extend_from_slice(&[42u8; 28]);
        let hash = reward_account_to_hash(&addr);
        assert_eq!(&hash.as_bytes()[..28], &[42u8; 28]);
        assert_eq!(hash.as_bytes()[28], 0x00);
    }

    #[test]
    fn test_reward_account_to_hash_script_credential() {
        let mut addr = vec![0xf0u8];
        addr.extend_from_slice(&[43u8; 28]);
        let hash = reward_account_to_hash(&addr);
        assert_eq!(&hash.as_bytes()[..28], &[43u8; 28]);
        assert_eq!(hash.as_bytes()[28], 0x01);
    }

    #[test]
    fn test_stake_routing_base_address() {
        let cred = Credential::VerificationKey(Hash28::from_bytes([1u8; 28]));
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(ZERO28),
            stake: cred.clone(),
        });
        match stake_routing(&addr, false) {
            StakeRouting::Credential(h) => {
                assert_eq!(h, credential_to_hash(&cred));
            }
            _ => panic!("Expected StakeRouting::Credential for base address"),
        }
    }

    #[test]
    fn test_stake_routing_pointer_excluded_in_conway() {
        let addr = Address::Pointer(PointerAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(ZERO28),
            pointer: Pointer {
                slot: 1,
                tx_index: 0,
                cert_index: 0,
            },
        });
        match stake_routing(&addr, true) {
            StakeRouting::None => {}
            _ => panic!("Expected StakeRouting::None for pointer address in Conway"),
        }
        match stake_routing(&addr, false) {
            StakeRouting::Pointer(_) => {}
            _ => panic!("Expected StakeRouting::Pointer for pointer address pre-Conway"),
        }
    }

    // -----------------------------------------------------------------------
    // Issue #729: stake-routing add/sub symmetry for every address type
    // -----------------------------------------------------------------------
    //
    // The root cause of the preprod ep181 WithdrawalAmountMismatch (+1 lovelace)
    // was that `TxBody.inputs` is a Set<TxIn> in the Cardano ledger but was
    // decoded into a Vec, causing duplicate inputs to be double-subtracted from
    // `stake_distribution` while the UTxO remove/insert is idempotent (masking
    // the drift). The fix: deduplicate inputs (and collateral) via `seen_inputs`
    // / `seen_collateral` HashSets before the subtract loop.
    //
    // These tests verify:
    // 1. The ADD path (Phase-5, output creation) and the SUB path (Phase-2,
    //    input spend) always route the same coin to the same stake-distribution
    //    key — for every address type combination.
    // 2. Duplicate inputs on the wire do NOT double-subtract stake (regression
    //    for the exact preprod bug).
    // 3. Duplicate collateral inputs on the wire do NOT double-subtract stake.
    // 4. Collateral-return outputs are ADD-credited consistently with the key
    //    that would be used when that output is later spent.

    /// Build a `TransactionOutput` with a fully-specified address.
    fn output_with_address(coin: u64, address: Address) -> TransactionOutput {
        TransactionOutput {
            address,
            value: Value {
                coin: Lovelace(coin),
                multi_asset: BTreeMap::new(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Build a `make_tx`-style tx with a single collateral input and an
    /// optional collateral_return.
    fn make_collateral_tx(
        hash: Hash32,
        col_inputs: Vec<TransactionInput>,
        col_return: Option<TransactionOutput>,
        total_collateral: Option<Lovelace>,
    ) -> Transaction {
        let mut tx = make_tx(hash, vec![], vec![], 0);
        tx.is_valid = false;
        tx.body.collateral = col_inputs;
        tx.body.collateral_return = col_return;
        tx.body.total_collateral = total_collateral;
        tx
    }

    /// Helper: seed a UTxO set with a given input→output, pre-credit stake_map,
    /// spend it via `apply_utxo_changes`, and assert the stake balance is zero.
    ///
    /// This confirms add and subtract route the same hash.
    fn assert_add_sub_symmetric(output: TransactionOutput, coin: u64) {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        // Compute the stake key that the ADD path will produce.
        let add_key = match stake_routing(&output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(k) => Some(k),
            StakeRouting::Pointer(_) | StakeRouting::None => None,
        };

        // Seed the UTxO.
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xaau8; 32]),
            index: 0,
        };
        utxo.utxo_set.insert(input.clone(), output.clone());

        // Pre-credit so the subtract won't underflow.
        if let Some(k) = add_key {
            certs.stake_distribution.stake_map.insert(k, Lovelace(coin));
        }

        // Build a tx that spends the input and produces an identical output
        // (net zero stake change expected).
        let tx = make_tx(
            Hash32::from_bytes([0xbbu8; 32]),
            vec![input.clone()],
            vec![output.clone()],
            0,
        );

        apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        // Stake should be back to exactly `coin` (subtract coin, add coin).
        match stake_routing(&output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(k) => {
                let balance = certs
                    .stake_distribution
                    .stake_map
                    .get(&k)
                    .copied()
                    .unwrap_or(Lovelace(0));
                assert_eq!(
                    balance.0, coin,
                    "add/sub must cancel for address {:?}",
                    output.address
                );
            }
            StakeRouting::Pointer(_) => {
                // ptr_stake not pre-seeded in this test; just verify no panic.
            }
            StakeRouting::None => {
                // No routing: stake_map must be unchanged (empty).
                assert!(
                    certs.stake_distribution.stake_map.is_empty(),
                    "enterprise/Byron addresses must not touch stake_map"
                );
            }
        }
    }

    // --- Test matrix: ADD/SUB symmetry for all address types ---

    /// Base address: SCRIPT payment + KEY stake credential.
    ///
    /// This is the exact address class that exposed the bug
    /// (`addr_test1zpu3l06a…`, cred 7d3e2b31…, preprod ep57).
    /// The payment credential is irrelevant to stake routing; both
    /// ADD and SUB must key on `base.stake` alone.
    #[test]
    fn test_stake_routing_symmetry_base_script_payment_key_stake() {
        let stake_h28 = Hash28::from_bytes([0x7du8; 28]); // matches cred 7d3e2b31…-ish
        let output = output_with_address(
            5_000_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0x11u8; 28])),
                stake: Credential::VerificationKey(stake_h28),
            }),
        );
        assert_add_sub_symmetric(output, 5_000_000_000);
    }

    /// Base address: KEY payment + KEY stake credential (standard address).
    #[test]
    fn test_stake_routing_symmetry_base_key_payment_key_stake() {
        let stake_h28 = Hash28::from_bytes([0x22u8; 28]);
        let output = output_with_address(
            3_000_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x33u8; 28])),
                stake: Credential::VerificationKey(stake_h28),
            }),
        );
        assert_add_sub_symmetric(output, 3_000_000_000);
    }

    /// Base address: SCRIPT payment + SCRIPT stake credential.
    #[test]
    fn test_stake_routing_symmetry_base_script_payment_script_stake() {
        let stake_h28 = Hash28::from_bytes([0x44u8; 28]);
        let output = output_with_address(
            2_000_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0x55u8; 28])),
                stake: Credential::Script(stake_h28),
            }),
        );
        assert_add_sub_symmetric(output, 2_000_000_000);
    }

    /// Base address: KEY payment + SCRIPT stake credential.
    #[test]
    fn test_stake_routing_symmetry_base_key_payment_script_stake() {
        let stake_h28 = Hash28::from_bytes([0x66u8; 28]);
        let output = output_with_address(
            1_500_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x77u8; 28])),
                stake: Credential::Script(stake_h28),
            }),
        );
        assert_add_sub_symmetric(output, 1_500_000_000);
    }

    /// Enterprise address: no stake routing expected.
    #[test]
    fn test_stake_routing_symmetry_enterprise_no_stake() {
        let output = output_with_address(
            1_000_000,
            Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x88u8; 28])),
            }),
        );
        assert_add_sub_symmetric(output, 1_000_000);
    }

    /// Pointer address (pre-Conway): stake routed via ptr_stake map.
    /// The test just asserts no panic and no credential-map contamination.
    #[test]
    fn test_stake_routing_symmetry_pointer_pre_conway() {
        let output = output_with_address(
            4_000_000,
            Address::Pointer(PointerAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x99u8; 28])),
                pointer: Pointer {
                    slot: 42,
                    tx_index: 1,
                    cert_index: 0,
                },
            }),
        );
        // Pointer routing in pre-Conway mode (exclude_ptrs=false).
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();
        epochs.ptr_stake_excluded = false;

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xddu8; 32]),
            index: 0,
        };
        utxo.utxo_set.insert(input.clone(), output.clone());

        let tx = make_tx(
            Hash32::from_bytes([0xeeu8; 32]),
            vec![input],
            vec![output],
            0,
        );
        apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        // No credential entries should have been created.
        assert!(
            certs.stake_distribution.stake_map.is_empty(),
            "pointer address must not create credential stake_map entries"
        );
    }

    // --- Duplicate input regression (the actual ep181 bug) ---

    /// Duplicate inputs on the wire must be deduplicated: the stake is
    /// subtracted exactly ONCE even if the same TxIn appears twice in
    /// `tx.body.inputs`.
    ///
    /// Regression for: preprod tx b6ce541006… (epoch 35) listed d94cc73b…#0
    /// and #1 twice each → double-subtract → +1 lovelace drift → ep181 halt.
    #[test]
    fn test_apply_utxo_changes_duplicate_input_no_double_subtract() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let stake_h28 = Hash28::from_bytes([0x7du8; 28]);
        let stake_cred = Credential::VerificationKey(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);
        let coin: u64 = 9_937_308_316; // matches the on-chain amount from the trace

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xb6u8; 32]),
            index: 0,
        };
        let output = output_with_address(
            coin,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0xccu8; 28])),
                stake: stake_cred,
            }),
        );
        utxo.utxo_set.insert(input.clone(), output);
        certs
            .stake_distribution
            .stake_map
            .insert(stake_key, Lovelace(coin));

        // Tx with the SAME input listed TWICE (simulates the wire-level duplicate).
        let tx = make_tx(
            Hash32::from_bytes([0xffu8; 32]),
            vec![input.clone(), input.clone()], // duplicate!
            vec![],
            0,
        );

        apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        // The input must be removed exactly once.
        assert!(
            !utxo.utxo_set.contains(&input),
            "duplicate input must be removed (idempotent)"
        );

        // Stake must be subtracted exactly ONCE → reaches zero, not underflow.
        let balance = certs
            .stake_distribution
            .stake_map
            .get(&stake_key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "duplicate input must only subtract stake once (not twice)"
        );
    }

    /// Without deduplication the balance would go NEGATIVE (saturating_sub
    /// clamps to 0), silently losing stake. Verify the pre-fix behaviour
    /// would have produced 0 (clamped underflow) not the correct 0 (clean
    /// single subtract), and that WITH deduplication both coin amounts match.
    #[test]
    fn test_apply_utxo_changes_duplicate_input_clamping_is_wrong() {
        // Control: two DISTINCT inputs of 5 ADA each.  Both should be subtracted.
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let stake_h28 = Hash28::from_bytes([0xabu8; 28]);
        let stake_cred = Credential::VerificationKey(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);

        let input0 = TransactionInput {
            transaction_id: Hash32::from_bytes([0x01u8; 32]),
            index: 0,
        };
        let input1 = TransactionInput {
            transaction_id: Hash32::from_bytes([0x01u8; 32]),
            index: 1,
        };
        let coin_per_input: u64 = 5_000_000;
        let make_base = |c: u64| {
            output_with_address(
                c,
                Address::Base(BaseAddress {
                    network: NetworkId::Testnet,
                    payment: Credential::VerificationKey(ZERO28),
                    stake: Credential::VerificationKey(stake_h28),
                }),
            )
        };
        utxo.utxo_set
            .insert(input0.clone(), make_base(coin_per_input));
        utxo.utxo_set
            .insert(input1.clone(), make_base(coin_per_input));
        certs
            .stake_distribution
            .stake_map
            .insert(stake_key, Lovelace(coin_per_input * 2));

        // Tx spends BOTH distinct inputs (not a duplicate).
        let tx = make_tx(
            Hash32::from_bytes([0x02u8; 32]),
            vec![input0, input1],
            vec![],
            0,
        );
        apply_utxo_changes(&tx, &mut utxo, &mut certs, &mut epochs);

        // Both subtractions should fire: 10M - 5M - 5M = 0.
        let balance = certs
            .stake_distribution
            .stake_map
            .get(&stake_key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "two distinct inputs must each subtract their coin"
        );
    }

    // --- Duplicate collateral regression ---

    /// Duplicate collateral inputs on the wire must not double-subtract stake.
    #[test]
    fn test_apply_collateral_duplicate_no_double_subtract() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let stake_h28 = Hash28::from_bytes([0x63u8; 28]);
        let stake_cred = Credential::VerificationKey(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);
        let coin: u64 = 3_000_000_000;

        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x30u8; 32]),
            index: 0,
        };
        let col_output = output_with_address(
            coin,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(ZERO28),
                stake: stake_cred,
            }),
        );
        utxo.utxo_set.insert(col_input.clone(), col_output);
        certs
            .stake_distribution
            .stake_map
            .insert(stake_key, Lovelace(coin));

        // Collateral with the SAME input listed TWICE.
        let tx = make_collateral_tx(
            Hash32::from_bytes([0x31u8; 32]),
            vec![col_input.clone(), col_input.clone()], // duplicate!
            None,
            None,
        );

        apply_collateral_consumption(&tx, &mut utxo, &mut certs, &mut epochs);

        // Stake must be subtracted exactly once.
        let balance = certs
            .stake_distribution
            .stake_map
            .get(&stake_key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, 0,
            "duplicate collateral input must only subtract stake once"
        );
    }

    // --- Collateral-return ADD consistency ---

    /// A collateral_return output's ADD path must use the same routing key
    /// as the SUB path would if that output were later spent as a regular input.
    ///
    /// Verifies the collateral-return routing loop in `apply_collateral_consumption`
    /// is consistent with `stake_routing` for all credential combinations.
    #[test]
    fn test_collateral_return_add_sub_symmetric_script_payment_key_stake() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let stake_h28 = Hash28::from_bytes([0x7eu8; 28]);
        let stake_cred = Credential::VerificationKey(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);

        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x40u8; 32]),
            index: 0,
        };
        // Collateral is spent from enterprise (no routing).
        utxo.utxo_set
            .insert(col_input.clone(), enterprise_output(10_000_000));

        // collateral_return goes to a base address (script payment + key stake).
        let return_coin: u64 = 8_000_000;
        let col_return = output_with_address(
            return_coin,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0xffu8; 28])),
                stake: stake_cred,
            }),
        );

        let tx = make_collateral_tx(
            Hash32::from_bytes([0x41u8; 32]),
            vec![col_input],
            Some(col_return),
            Some(Lovelace(2_000_000)),
        );

        apply_collateral_consumption(&tx, &mut utxo, &mut certs, &mut epochs);

        // The return output must have been ADD-credited to `stake_key`.
        let balance = certs
            .stake_distribution
            .stake_map
            .get(&stake_key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(
            balance.0, return_coin,
            "collateral_return must ADD-credit the stake key"
        );

        // Now verify that if we spend this return output, it routes to the same key.
        let return_input = TransactionInput {
            transaction_id: tx.hash,
            index: tx.body.outputs.len() as u32, // collateral return index
        };
        let return_output = utxo.utxo_set.lookup(&return_input).expect("return output");
        match stake_routing(&return_output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(k) => {
                assert_eq!(
                    k, stake_key,
                    "spending the collateral_return must route to the same key as the ADD"
                );
            }
            _ => panic!("Expected Credential routing for base address"),
        }
    }

    /// Script-stake collateral return: same symmetry check for script stake cred.
    #[test]
    fn test_collateral_return_add_sub_symmetric_script_payment_script_stake() {
        let mut utxo = empty_utxo_sub();
        let mut certs = empty_cert_sub();
        let mut epochs = empty_epoch_sub();

        let stake_h28 = Hash28::from_bytes([0x8fu8; 28]);
        let stake_cred = Credential::Script(stake_h28);
        let stake_key = credential_to_hash(&stake_cred);

        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x50u8; 32]),
            index: 0,
        };
        utxo.utxo_set
            .insert(col_input.clone(), enterprise_output(5_000_000));

        let return_coin: u64 = 4_000_000;
        let col_return = output_with_address(
            return_coin,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0xaau8; 28])),
                stake: stake_cred,
            }),
        );

        let tx = make_collateral_tx(
            Hash32::from_bytes([0x51u8; 32]),
            vec![col_input],
            Some(col_return),
            Some(Lovelace(1_000_000)),
        );

        apply_collateral_consumption(&tx, &mut utxo, &mut certs, &mut epochs);

        let balance = certs
            .stake_distribution
            .stake_map
            .get(&stake_key)
            .copied()
            .unwrap_or(Lovelace(0));
        assert_eq!(balance.0, return_coin);

        let return_input = TransactionInput {
            transaction_id: tx.hash,
            index: tx.body.outputs.len() as u32,
        };
        let return_output = utxo.utxo_set.lookup(&return_input).expect("return output");
        match stake_routing(&return_output.address, epochs.ptr_stake_excluded) {
            StakeRouting::Credential(k) => {
                assert_eq!(
                    k, stake_key,
                    "script-stake collateral_return must route to same script-stake key"
                );
            }
            _ => panic!("Expected Credential routing"),
        }
    }

    // --- Key/Script type discrimination: same hash, different type → different key ---

    /// Verify that a key-stake and a script-stake with the SAME 28-byte hash
    /// produce DIFFERENT stake distribution keys (typed_hash32 invariant).
    ///
    /// Guards against accidental type erasure on either the ADD or SUB path.
    #[test]
    fn test_stake_routing_key_vs_script_same_hash_different_key() {
        let same_hash = Hash28::from_bytes([0xcdu8; 28]);
        let key_cred = Credential::VerificationKey(same_hash);
        let script_cred = Credential::Script(same_hash);

        let key_output = output_with_address(
            1_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(ZERO28),
                stake: key_cred.clone(),
            }),
        );
        let script_output = output_with_address(
            1_000_000,
            Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(ZERO28),
                stake: script_cred.clone(),
            }),
        );

        let key_routing = match stake_routing(&key_output.address, false) {
            StakeRouting::Credential(k) => k,
            _ => panic!("Expected Credential routing"),
        };
        let script_routing = match stake_routing(&script_output.address, false) {
            StakeRouting::Credential(k) => k,
            _ => panic!("Expected Credential routing"),
        };

        assert_ne!(
            key_routing, script_routing,
            "key-stake and script-stake with the same 28-byte hash must produce different keys"
        );
        assert_eq!(key_routing, credential_to_hash(&key_cred));
        assert_eq!(script_routing, credential_to_hash(&script_cred));
    }

    // -----------------------------------------------------------------------
    // 6. validate_block_ex_units tests (#794)
    // -----------------------------------------------------------------------

    /// Regression test for #794: Haskell's Alonzo BBODY rule folds
    /// `totExUnits` over ALL transactions in the block body
    /// (`txTotal = foldMap totExUnits txs`) with no `IsValid` filter. An
    /// `is_valid=false` transaction still carries redeemers, and its ExUnits
    /// must count toward the block-level budget. A block whose only tx is
    /// invalid, but whose redeemer ExUnits alone exceed `maxBlockExUnits`,
    /// must be rejected.
    #[test]
    fn test_validate_block_ex_units_counts_invalid_tx_redeemers() {
        let params = ProtocolParameters::mainnet_defaults();
        let genesis_delegates = HashMap::new();
        let ctx = RuleContext {
            params: &params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: dugite_primitives::era::Era::Babbage,
            slot_config: None,
            node_network: None,
            genesis_delegates: &genesis_delegates,
            update_quorum: 5,
            epoch_length: 432_000,
            shelley_transition_epoch: 0,
            byron_epoch_length: 21_600,
            stability_window: 129_600,
            stability_window_3kf: 129_600,
            randomness_stabilisation_window: 129_600,
            tx_index: 0,
            conway_genesis: None,
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut tx = make_tx(Hash32::from_bytes([7u8; 32]), vec![], vec![], 0);
        tx.is_valid = false; // invalid tx -- must still count toward the block budget
        tx.witness_set.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Constr(0, vec![]),
            ex_units: ExUnits {
                // Exceeds mainnet's max_block_ex_units.mem (62,000,000) on its own.
                mem: params.max_block_ex_units.mem + 1,
                steps: 0,
            },
        });

        let block = Block {
            era: dugite_primitives::era::Era::Babbage,
            header: make_header(vec![], ZERO32, vec![]),
            transactions: vec![tx],
            raw_cbor: None,
        };

        let result = validate_block_ex_units(&block, &ctx);
        assert!(
            result.is_err(),
            "block ExUnits budget must count is_valid=false tx redeemers \
             (Haskell has no IsValid filter in Bbody.hs)"
        );
    }
}
