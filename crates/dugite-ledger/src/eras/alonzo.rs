/// Alonzo era ledger rules (protocol version 5-6).
///
/// Alonzo introduces:
/// - Plutus smart contracts (Phase-2 script validation)
/// - The IsValid flag on transactions
/// - Collateral inputs for failed script executions
/// - ExUnit budgets for Plutus execution
/// - Script data hash (datums + redeemers commitment)
///
/// The core LEDGER rule pipeline (drain withdrawals, process certs, apply UTxO)
/// is identical to Shelley/Allegra/Mary for valid transactions. The key addition
/// is `apply_invalid_tx` which now has a real implementation — consuming
/// collateral inputs when Plutus Phase-2 evaluation fails.
///
/// Alonzo does NOT have `collateral_return` — that was added in Babbage. When
/// a script fails, the entire collateral input value is forfeited.
use std::collections::HashSet;

use dugite_primitives::block::{Block, BlockHeader};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash28;
use dugite_primitives::transaction::{Certificate, Transaction};
use tracing::debug;

use super::common;
use super::shelley::ShelleyRules;
use super::{EraRules, RuleContext};
use crate::state::substates::*;
use crate::state::{BlockValidationMode, LedgerError};
use crate::utxo_diff::UtxoDiff;

/// Stateless Alonzo era rule strategy.
///
/// Delegates most methods to the same common helpers as ShelleyRules.
/// The epoch transition is delegated to ShelleyRules since the pre-Conway
/// epoch boundary logic is identical across Shelley through Babbage.
#[derive(Default, Debug, Clone, Copy)]
pub struct AlonzoRules;

impl AlonzoRules {
    pub fn new() -> Self {
        AlonzoRules
    }
}

impl EraRules for AlonzoRules {
    /// Alonzo block body validation.
    ///
    /// Validate Alonzo block body constraints.
    ///
    /// Checks that the total ExUnit budget (memory + steps) across all valid
    /// transactions does not exceed `max_block_ex_units` from protocol params.
    fn validate_block_body(
        &self,
        block: &Block,
        ctx: &RuleContext,
        _utxo: &UtxoSubState,
    ) -> Result<(), LedgerError> {
        common::validate_block_ex_units(block, ctx)
    }

    /// Apply a single valid Alonzo transaction (IsValid=true).
    ///
    /// The pipeline is identical to Shelley:
    /// 1. Drain withdrawal accounts
    /// 2. Process Shelley-era certificates (no governance certs in Alonzo)
    /// 3. Apply UTxO changes (consume inputs, produce outputs, accumulate fee)
    ///
    /// Alonzo's Plutus-specific features (script execution, ExUnit accounting)
    /// affect the validation path, not the apply pipeline for valid transactions.
    fn apply_valid_tx(
        &self,
        tx: &Transaction,
        _mode: BlockValidationMode,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        gov: &mut GovSubState,
        epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        // Step 1: Drain withdrawal accounts.
        common::drain_withdrawal_accounts(tx, certs);

        // Step 2: Process Shelley-era certificates.
        common::process_shelley_certs(tx, ctx.current_slot, ctx.tx_index, certs, epochs, gov);

        // Step 3: Apply UTxO changes.
        let diff = common::apply_utxo_changes(tx, utxo, certs, epochs);

        Ok(diff)
    }

    /// Apply an invalid Alonzo transaction (IsValid=false, collateral consumption).
    ///
    /// When a Plutus script fails Phase-2 validation, the block producer marks
    /// the transaction as invalid. Regular inputs/outputs/certificates are NOT
    /// applied. Instead, collateral inputs are consumed (forfeited to the block
    /// producer as the fee).
    ///
    /// Alonzo does NOT support `collateral_return` — the entire collateral input
    /// value is forfeited. The `apply_collateral_consumption` helper handles this
    /// correctly: when `collateral_return` is `None`, no return output is created.
    fn apply_invalid_tx(
        &self,
        tx: &Transaction,
        _mode: BlockValidationMode,
        _ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        // Alonzo collateral consumption: no collateral_return field.
        let diff = common::apply_collateral_consumption(tx, utxo, certs, epochs);
        Ok(diff)
    }

    /// Process an Alonzo epoch boundary transition.
    ///
    /// The pre-Conway epoch transition is identical across Shelley through Babbage:
    /// snapshot rotation, pool retirements, PPUP proposals, nonce evolution, etc.
    /// Delegates to `ShelleyRules` to avoid duplicating the logic.
    fn process_epoch_transition(
        &self,
        new_epoch: dugite_primitives::time::EpochNo,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        gov: &mut GovSubState,
        epochs: &mut EpochSubState,
        consensus: &mut ConsensusSubState,
    ) -> Result<(), LedgerError> {
        debug!("Alonzo epoch transition: -> {}", new_epoch.0);
        ShelleyRules.process_epoch_transition(new_epoch, ctx, utxo, certs, gov, epochs, consensus)
    }

    /// Evolve nonce state after an Alonzo block header.
    ///
    /// Same VRF-based nonce evolution as Shelley.
    fn evolve_nonce(
        &self,
        header: &BlockHeader,
        ctx: &RuleContext,
        consensus: &mut ConsensusSubState,
    ) {
        let first_slot_of_current_epoch = common::first_slot_of_shelley_epoch(
            ctx.current_epoch.0,
            ctx.shelley_transition_epoch,
            ctx.byron_epoch_length,
            ctx.epoch_length,
        );
        let first_slot_of_next_epoch = first_slot_of_current_epoch.saturating_add(ctx.epoch_length);

        let (d_num, d_den) = if ctx.params.protocol_version_major >= 7 {
            (0u64, 1u64)
        } else {
            (ctx.params.d.numerator, ctx.params.d.denominator.max(1))
        };

        // Alonzo uses 3k/f stability window (not 4k/f).
        common::compute_shelley_nonce(
            header,
            ctx.current_slot,
            first_slot_of_current_epoch,
            first_slot_of_next_epoch,
            ctx.stability_window_3kf,
            d_num,
            d_den,
            consensus,
        );
    }

    /// Alonzo minimum fee: `min_fee_a * tx_size + min_fee_b`.
    ///
    /// Same linear fee formula as Shelley. Alonzo adds Plutus execution cost
    /// to the fee calculation, but that is part of Phase-2 validation and the
    /// fee declared in the transaction body, not this base min_fee function.
    fn min_fee(&self, tx: &Transaction, ctx: &RuleContext, _utxo: &UtxoSubState) -> u64 {
        let tx_size = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);
        ctx.params
            .min_fee_a
            .checked_mul(tx_size)
            .and_then(|product| product.checked_add(ctx.params.min_fee_b))
            .unwrap_or(u64::MAX)
    }

    /// Handle hard fork state transformations when entering Alonzo.
    ///
    /// Mary -> Alonzo: bump the on-chain protocol version to (5, 0). Plutus
    /// scripts are new but do not change the ledger state shape. The new
    /// protocol parameters (cost models, execution costs, etc.) are set via
    /// the hard fork combinator genesis config, not via state transformation.
    ///
    /// In cardano-node the Mary→Alonzo era boundary is a hard fork. The HFC
    /// era-crossing tick performs the PV5 bump. Dugite has no separate HFC
    /// layer — era transitions are dispatched entirely in the ledger crate
    /// via `block.era`. So we replicate the HFC's PV write here at the
    /// No-op era transition for Alonzo.
    ///
    /// Haskell's `upgradeAlonzoPParams` carries `protocolVersion` forward verbatim
    /// via `coerce` (zero-cost type coercion). PV5 (and subsequently PV6) arrive
    /// via PPUP (protocol parameter update proposals) voted on in the Mary and Alonzo
    /// eras respectively — not from the era transition itself. The PPUP path is
    /// correctly decoded since #624.
    ///
    /// Mirrors the Babbage fix from commit `a09b7ce47` (issue #626). Closes #630.
    fn on_era_transition(
        &self,
        from_era: Era,
        ctx: &RuleContext,
        _utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _gov: &mut GovSubState,
        _consensus: &mut ConsensusSubState,
        _epochs: &mut EpochSubState,
    ) -> Result<(), LedgerError> {
        debug!(
            "{:?} -> Alonzo era transition (ctx.era={:?}): no ledger-side state \
             mutation; PV bump driven by PPUP",
            from_era, ctx.era
        );
        Ok(())
    }

    /// Compute the set of required VKey witnesses for an Alonzo transaction.
    ///
    /// Same sources as Shelley (spending inputs, withdrawals, certificates)
    /// plus Plutus script requirements. For now, matches Shelley's witness logic
    /// — Plutus-specific witness requirements (required_signers) can be added
    /// when Phase-2 validation is fully implemented.
    fn required_witnesses(
        &self,
        tx: &Transaction,
        _ctx: &RuleContext,
        utxo: &UtxoSubState,
        _certs: &CertSubState,
        _gov: &GovSubState,
    ) -> HashSet<Hash28> {
        let mut witnesses = HashSet::new();

        // 1. Spending input pubkey hashes.
        for input in &tx.body.inputs {
            if let Some(output) = utxo.utxo_set.lookup(input) {
                if let Some(Credential::VerificationKey(hash)) = output.address.payment_credential()
                {
                    witnesses.insert(*hash);
                }
            }
        }

        // 2. Withdrawal key hashes.
        for reward_account in tx.body.withdrawals.keys() {
            if reward_account.len() >= 29 && reward_account[0] & 0x10 == 0 {
                let mut key_bytes = [0u8; 28];
                key_bytes.copy_from_slice(&reward_account[1..29]);
                witnesses.insert(Hash28::from_bytes(key_bytes));
            }
        }

        // 3. Certificate key hashes.
        for cert in &tx.body.certificates {
            match cert {
                Certificate::StakeRegistration(Credential::VerificationKey(hash))
                | Certificate::StakeDeregistration(Credential::VerificationKey(hash)) => {
                    witnesses.insert(*hash);
                }
                Certificate::StakeDelegation {
                    credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }
                Certificate::PoolRegistration(params) => {
                    witnesses.insert(params.operator);
                    for owner in &params.pool_owners {
                        witnesses.insert(*owner);
                    }
                }
                Certificate::PoolRetirement { pool_hash, .. } => {
                    witnesses.insert(*pool_hash);
                }
                _ => {}
            }
        }

        // 4. Required signers (Alonzo addition for Plutus scripts).
        // Required signers are Hash32 (28-byte key hashes zero-padded to 32 bytes).
        // Extract the 28-byte key hash for witness matching.
        for signer in &tx.body.required_signers {
            let mut key_bytes = [0u8; 28];
            key_bytes.copy_from_slice(&signer.as_bytes()[..28]);
            witnesses.insert(Hash28::from_bytes(key_bytes));
        }

        witnesses
    }
}

// ---------------------------------------------------------------------------
// Internal helpers for collateral stub state (test only)
// ---------------------------------------------------------------------------

#[cfg(test)]
use crate::state::{EpochSnapshots, StakeDistributionState};
#[cfg(test)]
use dugite_primitives::protocol_params::ProtocolParameters;
#[cfg(test)]
use dugite_primitives::value::Lovelace;
#[cfg(test)]
use std::collections::{BTreeMap, HashMap};
#[cfg(test)]
use std::sync::Arc;

/// Create a minimal CertSubState for testing collateral consumption.
#[cfg(test)]
fn make_empty_cert_sub() -> CertSubState {
    CertSubState {
        delegations: Arc::new(HashMap::new()),
        pool_params: Arc::new(HashMap::new()),
        future_pool_params: HashMap::new(),
        pending_retirements: HashMap::new(),
        reward_accounts: Arc::new(HashMap::new()),
        stake_key_deposits: std::sync::Arc::new(HashMap::new()),
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

/// Create a minimal EpochSubState for testing collateral consumption.
#[cfg(test)]
fn make_empty_epoch_sub() -> EpochSubState {
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
        prev_protocol_version_major: 5,
        prev_d: dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        },
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eras::{EraRules, EraRulesImpl, RuleContext};
    use crate::state::{BlockValidationMode, GovernanceState, StakeDistributionState};
    use crate::utxo::UtxoSet;
    use crate::utxo_diff::DiffSeq;
    use dugite_primitives::address::Address;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{
        OutputDatum, TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
    };
    use dugite_primitives::value::Lovelace;
    use dugite_primitives::value::Value;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_alonzo_ctx(params: &ProtocolParameters) -> RuleContext<'_> {
        let delegates = Box::leak(Box::new(HashMap::new()));
        RuleContext {
            params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: Era::Alonzo,
            slot_config: None,
            node_network: None,
            genesis_delegates: delegates,
            update_quorum: 5,
            epoch_length: 432000,
            shelley_transition_epoch: 0,
            byron_epoch_length: 21600,
            stability_window: 129600,
            stability_window_3kf: 129600,
            randomness_stabilisation_window: 129600,
            tx_index: 0,
            conway_genesis: None,
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        }
    }

    fn make_utxo_sub(entries: Vec<(TransactionInput, TransactionOutput)>) -> UtxoSubState {
        let mut utxo_set = UtxoSet::new();
        for (input, output) in entries {
            utxo_set.insert(input, output);
        }
        UtxoSubState {
            utxo_set,
            diff_seq: DiffSeq::new(),
            epoch_fees: Lovelace(0),
            pending_donations: Lovelace(0),
        }
    }

    fn make_cert_sub() -> CertSubState {
        CertSubState {
            delegations: Arc::new(HashMap::new()),
            pool_params: Arc::new(HashMap::new()),
            future_pool_params: HashMap::new(),
            pending_retirements: HashMap::new(),
            reward_accounts: Arc::new(HashMap::new()),
            stake_key_deposits: std::sync::Arc::new(HashMap::new()),
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

    fn make_gov_sub() -> GovSubState {
        GovSubState {
            governance: Arc::new(GovernanceState::default()),
        }
    }

    fn make_epoch_sub() -> EpochSubState {
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
            prev_protocol_version_major: 5,
            prev_d: dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
        }
    }

    fn make_output(address: Address, coin: u64) -> TransactionOutput {
        TransactionOutput {
            address,
            value: Value::lovelace(coin),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        }
    }

    fn make_input(tx_id_byte: u8, index: u32) -> TransactionInput {
        TransactionInput {
            transaction_id: Hash32::from_bytes([tx_id_byte; 32]),
            index,
        }
    }

    fn make_tx(
        tx_id_byte: u8,
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        fee: u64,
    ) -> Transaction {
        let body = TransactionBody {
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
        };
        Transaction {
            era: Era::Alonzo,
            hash: Hash32::from_bytes([tx_id_byte; 32]),
            body,
            witness_set: TransactionWitnessSet {
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
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: Some(vec![0u8; 200]),
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    fn make_consensus_sub() -> ConsensusSubState {
        ConsensusSubState {
            evolving_nonce: Hash32::ZERO,
            candidate_nonce: Hash32::ZERO,
            epoch_nonce: Hash32::ZERO,
            lab_nonce: Hash32::ZERO,
            last_epoch_block_nonce: Hash32::ZERO,
            extra_entropy: Hash32::ZERO,
            rolling_nonce: Hash32::ZERO,
            first_block_hash_of_epoch: None,
            prev_epoch_first_block_hash: None,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
            epoch_block_count: 0,
            opcert_counters: HashMap::new(),
        }
    }

    fn make_enterprise_address(key_hash: Hash28) -> Address {
        let mut addr_bytes = vec![0x61]; // Enterprise, key credential, network 1
        addr_bytes.extend_from_slice(key_hash.as_bytes());
        Address::from_bytes(&addr_bytes).expect("valid enterprise address")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// Verify that EraRulesImpl::for_era correctly maps Alonzo.
    #[test]
    fn test_era_rules_impl_for_alonzo() {
        assert!(matches!(
            EraRulesImpl::for_era(Era::Alonzo),
            EraRulesImpl::Alonzo(_)
        ));
    }

    /// validate_block_body returns Ok for an empty Alonzo block (zero redeemers,
    /// so total ExUnits is 0 and the per-block budget check trivially passes).
    /// The budget enforcement itself is exercised by `common::validate_block_ex_units`
    /// tests.
    #[test]
    fn test_validate_block_body_succeeds() {
        let rules = AlonzoRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_alonzo_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let block = dugite_primitives::block::Block {
            era: Era::Alonzo,
            header: dugite_primitives::block::BlockHeader {
                header_hash: Hash32::ZERO,
                prev_hash: Hash32::ZERO,
                issuer_vkey: vec![],
                vrf_vkey: vec![],
                vrf_result: dugite_primitives::block::VrfOutput {
                    output: vec![],
                    proof: vec![],
                },
                block_number: dugite_primitives::time::BlockNo(0),
                slot: dugite_primitives::time::SlotNo(0),
                epoch_nonce: Hash32::ZERO,
                body_size: 0,
                body_hash: Hash32::ZERO,
                operational_cert: dugite_primitives::block::OperationalCert {
                    hot_vkey: vec![],
                    sequence_number: 0,
                    kes_period: 0,
                    sigma: vec![],
                },
                protocol_version: dugite_primitives::block::ProtocolVersion { major: 5, minor: 0 },
                kes_signature: vec![],
                nonce_vrf_output: vec![],
                nonce_vrf_proof: vec![],
                prev_nonce: None,
                raw_header_body: None,
            },
            transactions: vec![],
            raw_cbor: None,
        };

        assert!(rules.validate_block_body(&block, &ctx, &utxo).is_ok());
    }

    /// Apply a valid Alonzo transaction that spends a UTxO and produces a new one.
    #[test]
    fn test_apply_valid_tx_with_utxo() {
        let rules = AlonzoRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_alonzo_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAA, 0);
        let spent_output = make_output(addr.clone(), 5_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let new_output = make_output(addr, 4_800_000);
        let tx = make_tx(0x01, vec![input.clone()], vec![new_output], 200_000);
        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ApplyOnly,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );
        assert!(result.is_ok());
        let diff = result.unwrap();

        // Original input consumed.
        assert_eq!(diff.deletes.len(), 1);
        assert!(diff.deletes.iter().any(|(i, _)| *i == input));

        // New output produced.
        assert_eq!(diff.inserts.len(), 1);

        // Fee accumulated.
        assert_eq!(utxo.epoch_fees.0, 200_000);

        // Original input no longer in UTxO set.
        assert!(!utxo.utxo_set.contains(&input));
    }

    /// Apply an invalid Alonzo transaction — collateral is consumed, no return.
    #[test]
    fn test_apply_invalid_tx_consumes_collateral() {
        let rules = AlonzoRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_alonzo_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);

        // Set up collateral input in UTxO.
        let collateral_input = make_input(0xCC, 0);
        let collateral_output = make_output(addr, 10_000_000);
        let mut utxo = make_utxo_sub(vec![(collateral_input.clone(), collateral_output)]);

        // Build an invalid transaction with collateral.
        let mut tx = make_tx(0x02, vec![], vec![], 0);
        tx.is_valid = false;
        tx.body.collateral = vec![collateral_input.clone()];
        // Alonzo: no collateral_return, no total_collateral.

        let mut certs = make_empty_cert_sub();
        let mut epochs = make_empty_epoch_sub();
        let result = rules.apply_invalid_tx(
            &tx,
            BlockValidationMode::ApplyOnly,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut epochs,
        );
        assert!(result.is_ok());
        let diff = result.unwrap();

        // Collateral input consumed.
        assert_eq!(diff.deletes.len(), 1);
        assert!(diff.deletes.iter().any(|(i, _)| *i == collateral_input));

        // No collateral return in Alonzo.
        assert!(diff.inserts.is_empty());

        // Full collateral value forfeited as fee.
        assert_eq!(utxo.epoch_fees.0, 10_000_000);

        // Collateral input removed from UTxO set.
        assert!(!utxo.utxo_set.contains(&collateral_input));
    }

    /// Alonzo min_fee matches the linear formula.
    #[test]
    fn test_min_fee_linear() {
        let rules = AlonzoRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.min_fee_a = 44;
        params.min_fee_b = 155381;
        let ctx = make_alonzo_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0x01, vec![], vec![], 0);
        let fee = rules.min_fee(&tx, &ctx, &utxo);
        // tx has raw_cbor of 200 bytes.
        assert_eq!(fee, 44 * 200 + 155381);
    }

    /// After #630: on_era_transition must NOT write PV — PPUP via UPEC/NEWPP does that.
    /// PV5 (Mary→Alonzo) and PV6 (intra-Alonzo) both arrive via PPUP (#624).
    #[test]
    fn test_on_era_transition_mary_to_alonzo_does_not_bump_pv() {
        let rules = AlonzoRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 5;
        params.protocol_version_minor = 0;
        let ctx = make_alonzo_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params.protocol_version_major = 5;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Mary,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 5,
            "on_era_transition must NOT bump PV — PPUP via UPEC/NEWPP does that",
        );
        assert_eq!(epochs.protocol_params.protocol_version_minor, 0);
    }

    /// Defensive: AlonzoRules is a no-op regardless of ctx.era.
    #[test]
    fn test_on_era_transition_alonzo_no_pv_mutation() {
        let rules = AlonzoRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8;
        let mut ctx = make_alonzo_ctx(&params);
        ctx.era = Era::Babbage;
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params.protocol_version_major = 8;

        let result = rules.on_era_transition(
            Era::Mary,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 8,
            "AlonzoRules must never mutate PV",
        );
    }

    /// required_witnesses includes required_signers (Alonzo addition).
    #[test]
    fn test_required_witnesses_includes_required_signers() {
        let rules = AlonzoRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_alonzo_ctx(&params);
        let utxo = make_utxo_sub(vec![]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        // required_signers are Hash32 (28-byte key hash zero-padded to 32 bytes).
        let mut signer_bytes = [0u8; 32];
        signer_bytes[..28].copy_from_slice(&[0x99; 28]);
        let signer = Hash32::from_bytes(signer_bytes);
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.required_signers = vec![signer];

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);
        let expected_key = Hash28::from_bytes([0x99; 28]);
        assert!(witnesses.contains(&expected_key));
    }
}
