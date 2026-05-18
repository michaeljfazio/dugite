//! N2N and N2C server setup: connection-facing adapters that bridge the node's
//! internal state (ChainDB, LedgerState, Mempool) to the network protocol
//! implementations in dugite-network.

use std::sync::Arc;
use tokio::sync::RwLock;

use dugite_ledger::LedgerState;
use dugite_network::{
    BlockProvider, TipInfo, TxValidationError, TxValidator, UtxoQueryProvider, UtxoSnapshot,
};
use dugite_storage::ChainDB;

// ─── ChainDBBlockProvider ────────────────────────────────────────────────────

/// Provides block data from ChainDB for the N2N server.
pub(crate) struct ChainDBBlockProvider {
    pub chain_db: Arc<RwLock<ChainDB>>,
}

impl BlockProvider for ChainDBBlockProvider {
    fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            db.get_block(&block_hash).ok().flatten()
        })
    }

    fn has_block(&self, hash: &[u8; 32]) -> bool {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            db.has_block(&block_hash)
        })
    }

    fn get_tip(&self) -> TipInfo {
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            let tip = db.get_tip();
            let slot = tip.point.slot().map(|s| s.0).unwrap_or(0);
            let hash = tip
                .point
                .hash()
                .map(|h| {
                    let bytes: &[u8] = h.as_ref();
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(bytes);
                    arr
                })
                .unwrap_or([0u8; 32]);
            let block_no = tip.block_number.0;
            TipInfo {
                slot,
                hash,
                block_number: block_no,
            }
        })
    }

    fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            let slot = dugite_primitives::time::SlotNo(after_slot);
            match db.get_next_block_after_slot(slot) {
                Ok(Some((s, hash, cbor))) => {
                    let mut hash_arr = [0u8; 32];
                    hash_arr.copy_from_slice(hash.as_bytes());
                    Some((s.0, hash_arr, cbor))
                }
                _ => None,
            }
        })
    }

    fn is_on_chain(&self, hash: &[u8; 32]) -> bool {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            db.is_on_chain(&block_hash)
        })
    }

    fn find_chain_ancestor(&self, start_hash: &[u8; 32]) -> Option<(u64, [u8; 32], u64)> {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*start_hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            db.find_chain_ancestor(&block_hash).map(|(slot, h, bn)| {
                let mut hash_arr = [0u8; 32];
                hash_arr.copy_from_slice(h.as_bytes());
                (slot.0, hash_arr, bn.0)
            })
        })
    }

    fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            let slot_no = dugite_primitives::time::SlotNo(slot);
            match db.get_block_at_or_after_slot(slot_no) {
                Ok(Some((s, hash, cbor))) => {
                    let mut hash_arr = [0u8; 32];
                    hash_arr.copy_from_slice(hash.as_bytes());
                    Some((s.0, hash_arr, cbor))
                }
                _ => None,
            }
        })
    }

    /// Collect blocks in [`from_slot`, `to_slot`] with chunked lock acquisition.
    ///
    /// # Why this override is critical
    ///
    /// The default trait implementation calls `get_next_block_after_slot()` in a
    /// loop, each of which does `block_in_place(|| chain_db.blocking_read())`.
    /// For a batch of N blocks that means N separate lock-acquire/release cycles —
    /// each one parks the calling tokio worker thread until the lock is available.
    /// When `ChainSelQueue` holds `chain_db.write()` during block storage, all N
    /// parked threads stack up and starve the async worker pool, freezing the
    /// metrics endpoint and slowing the main run loop.
    ///
    /// # Chunked locking strategy
    ///
    /// We do NOT hold the lock for the entire batch because
    /// `ImmutableDB::get_next_block_after_slot()` performs synchronous disk I/O
    /// (reads `.secondary` index + `.chunk` data files).  Holding the read lock
    /// for 2000 sequential disk reads would block `ChainSelQueue.write()` for
    /// seconds and stall the main sync loop.
    ///
    /// Instead, we acquire the read lock in chunks of `BATCH_CHUNK_SIZE` blocks.
    /// This reduces lock overhead by ~50× compared to per-block locking while
    /// keeping the critical section short enough (≈50 disk reads ≈ a few ms)
    /// for the writer to make progress between chunks.
    fn get_blocks_in_range(
        &self,
        from_slot: u64,
        to_slot: u64,
        limit: usize,
    ) -> Vec<(u64, [u8; 32], Vec<u8>)> {
        /// Number of blocks to collect per lock acquisition.  Each block read
        /// may hit disk (ImmutableDB), so keep this small enough that the
        /// ChainSelQueue writer is never starved for more than a few ms.
        const BATCH_CHUNK_SIZE: usize = 50;

        tokio::task::block_in_place(|| {
            let mut blocks = Vec::new();
            let mut current_slot = from_slot;
            let mut first = true;

            while current_slot <= to_slot && blocks.len() < limit {
                // Acquire the read lock for a chunk of blocks.
                let db = self.chain_db.blocking_read();
                let chunk_limit = BATCH_CHUNK_SIZE.min(limit - blocks.len());

                for _ in 0..chunk_limit {
                    if current_slot > to_slot {
                        break;
                    }
                    let slot_no = dugite_primitives::time::SlotNo(current_slot);
                    let result = if first {
                        first = false;
                        db.get_block_at_or_after_slot(slot_no)
                    } else {
                        db.get_next_block_after_slot(slot_no)
                    };
                    match result {
                        Ok(Some((s, hash, cbor))) if s.0 <= to_slot => {
                            let mut hash_arr = [0u8; 32];
                            hash_arr.copy_from_slice(hash.as_bytes());
                            current_slot = s.0;
                            blocks.push((s.0, hash_arr, cbor));
                        }
                        _ => return blocks, // No more blocks — done
                    }
                }
                // Read lock dropped here; ChainSelQueue writer can proceed.
            }
            blocks
        })
    }
}

// ─── LedgerUtxoProvider ──────────────────────────────────────────────────────

/// Provides UTxO lookups from the live ledger state.
pub(crate) struct LedgerUtxoProvider {
    pub ledger: Arc<RwLock<LedgerState>>,
}

impl UtxoQueryProvider for LedgerUtxoProvider {
    fn utxos_at_address_bytes(&self, addr_bytes: &[u8]) -> Vec<UtxoSnapshot> {
        let addr = match dugite_primitives::address::Address::from_bytes(addr_bytes) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    "UTxO query: address decode failed: {e} (bytes len={})",
                    addr_bytes.len()
                );
                return vec![];
            }
        };
        // Use block_in_place + blocking_read so this works correctly even when
        // called from within a tokio async runtime (avoids "cannot block" panic).
        tokio::task::block_in_place(|| {
            let ledger = self.ledger.blocking_read();
            let results: Vec<_> = ledger
                .utxo
                .utxo_set
                .utxos_at_address(&addr)
                .into_iter()
                .map(|(input, output)| utxo_to_snapshot(&input, &output))
                .collect();
            tracing::debug!(
                addr_type = ?std::mem::discriminant(&addr),
                index_size = ledger.utxo.utxo_set.address_index_size(),
                utxos_found = results.len(),
                "UTxO query by address"
            );
            results
        })
    }

    fn utxos_by_tx_inputs(&self, inputs: &[(Vec<u8>, u32)]) -> Vec<UtxoSnapshot> {
        tokio::task::block_in_place(|| {
            let ledger = self.ledger.blocking_read();
            let mut results = Vec::new();
            for (tx_hash_bytes, idx) in inputs {
                if tx_hash_bytes.len() == 32 {
                    let mut hash_arr = [0u8; 32];
                    hash_arr.copy_from_slice(tx_hash_bytes);
                    let tx_input = dugite_primitives::transaction::TransactionInput {
                        transaction_id: dugite_primitives::hash::Hash32::from_bytes(hash_arr),
                        index: *idx,
                    };
                    if let Some(output) = ledger.utxo.utxo_set.lookup(&tx_input) {
                        results.push(utxo_to_snapshot(&tx_input, &output));
                    }
                }
            }
            results
        })
    }

    fn utxos_all(&self) -> Vec<UtxoSnapshot> {
        tokio::task::block_in_place(|| {
            let ledger = self.ledger.blocking_read();
            let results: Vec<_> = ledger
                .utxo
                .utxo_set
                .iter()
                .into_iter()
                .map(|(input, output)| utxo_to_snapshot(&input, &output))
                .collect();
            tracing::debug!(utxos_found = results.len(), "UTxO query: whole set");
            results
        })
    }
}

// ─── LedgerTxValidator ───────────────────────────────────────────────────────

/// Project the live `LedgerState`'s Conway governance fields into the shapes
/// expected by [`dugite_ledger::validation::ValidationContext`]:
///
/// * `active_proposals`: every active on-chain governance proposal keyed by
///   its `GovActionId`, carrying enough state for the cross-tx voting
///   predicates (`DisallowedVoters`, `VotingOnExpiredGovAction`,
///   `ProposalReturnAccountDoesNotExist`).
/// * `committee_authorized_hot_keys`: the set of hot credentials that have
///   been authorised by Constitutional Committee members.  Used by
///   `VotersDoNotExist` to reject votes from a CC voter whose hot
///   credential is unknown to the ledger.
///
/// Mirrors Haskell's `GovEnv` exposing both `proposals` and
/// `authorizedHotCommitteeCredentials` to the GOV rule.
fn build_governance_validation_state(
    ledger: &LedgerState,
) -> (
    std::collections::HashMap<
        dugite_primitives::transaction::GovActionId,
        dugite_ledger::validation::ActiveProposal,
    >,
    std::collections::HashSet<dugite_primitives::hash::Hash32>,
) {
    let active_proposals = ledger
        .gov
        .governance
        .proposals
        .iter()
        .map(|(id, state)| {
            (
                id.clone(),
                dugite_ledger::validation::ActiveProposal {
                    gov_action: state.procedure.gov_action.clone(),
                    return_addr: state.procedure.return_addr.clone(),
                    deposit: state.procedure.deposit,
                    expires_after_epoch: state.expires_epoch,
                    proposed_in_epoch: state.proposed_epoch,
                },
            )
        })
        .collect();
    let committee_hot_keys = ledger
        .gov
        .governance
        .committee_hot_keys
        .values()
        .copied()
        .collect();
    (active_proposals, committee_hot_keys)
}

/// Validates transactions against the live ledger state (Phase-1 + Phase-2 Plutus).
///
/// When `mempool` is provided, validation uses a `CompositeUtxoView` that
/// overlays mempool virtual UTxOs on top of the on-chain set.  This enables
/// chained/dependent transaction submission (spending outputs of unconfirmed
/// mempool txs).
pub(crate) struct LedgerTxValidator {
    pub ledger: Arc<RwLock<LedgerState>>,
    pub slot_config: dugite_ledger::plutus::SlotConfig,
    pub metrics: Arc<crate::metrics::NodeMetrics>,
    pub mempool: Option<Arc<dugite_mempool::Mempool>>,
}

impl TxValidator for LedgerTxValidator {
    fn validate_tx(&self, era_id: u16, tx_bytes: &[u8]) -> Result<(), TxValidationError> {
        let tx = dugite_serialization::decode_transaction(era_id, tx_bytes).map_err(|e| {
            TxValidationError::DecodeFailed {
                reason: e.to_string(),
            }
        })?;

        let ledger = self
            .ledger
            .try_read()
            .map_err(|_| TxValidationError::LedgerStateUnavailable)?;
        let tx_size = tx_bytes.len() as u64;
        let current_slot = ledger.tip.point.slot().map(|s| s.0).unwrap_or(0);

        // Build the UTxO view: on-chain set + optional mempool virtual overlay.
        // This enables chained tx submission (spending unconfirmed mempool outputs).
        let virtual_utxos = self
            .mempool
            .as_ref()
            .map(|mp| mp.virtual_utxo_snapshot())
            .unwrap_or_default();
        let utxo_view =
            dugite_ledger::utxo::CompositeUtxoView::new(&ledger.utxo.utxo_set, virtual_utxos);

        // Build ledger context for validation — pool re-registrations need the
        // registered pool set to avoid charging a duplicate deposit (#436).
        let registered_pool_ids: std::collections::HashSet<dugite_primitives::hash::Hash28> =
            ledger.certs.pool_params.keys().copied().collect();

        // Plumb the on-chain Conway governance state into the validator so the
        // cross-tx voting predicates (`DisallowedVoters`, `VotersDoNotExist`,
        // `VotingOnExpiredGovAction`) can reject votes against active on-chain
        // proposals — not just proposals submitted in the same transaction.
        //
        // Mirrors Haskell's GovEnv exposing both `proposals` and
        // `authorizedHotCommitteeCredentials` to the GOV rule.
        let (active_proposals, committee_hot_keys) = build_governance_validation_state(&ledger);

        let context = dugite_ledger::validation::ValidationContext::new()
            .with_pools(registered_pool_ids)
            .with_active_proposals(active_proposals)
            .with_committee_authorized_hot_keys(committee_hot_keys);

        dugite_ledger::validation::validate_transaction_with_context(
            &tx,
            &utxo_view,
            &ledger.epochs.protocol_params,
            current_slot,
            tx_size,
            Some(&self.slot_config),
            context,
        )
        .map_err(|errors| {
            // Increment the rejection counter so the TUI and Prometheus show it.
            self.metrics
                .transactions_rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for err in &errors {
                self.metrics.record_validation_error(&format!("{:?}", err));
            }
            let mut mapped: Vec<TxValidationError> =
                errors.into_iter().map(convert_validation_error).collect();
            if mapped.len() == 1 {
                mapped.pop().expect("vec has exactly one element")
            } else {
                TxValidationError::Multiple(mapped)
            }
        })
    }
}

/// Convert a ledger `ValidationError` into the network-facing `TxValidationError`.
pub(crate) fn convert_validation_error(
    e: dugite_ledger::validation::ValidationError,
) -> TxValidationError {
    use dugite_ledger::validation::ValidationError as VE;
    match e {
        VE::NoInputs => TxValidationError::NoInputs,
        VE::InputNotFound(input) => TxValidationError::InputNotFound { input },
        VE::ValueNotConserved {
            inputs,
            outputs,
            fee,
        } => TxValidationError::ValueNotConserved {
            inputs,
            outputs,
            fee,
        },
        VE::FeeTooSmall { minimum, actual } => TxValidationError::FeeTooSmall { minimum, actual },
        VE::OutputTooSmall { minimum, actual } => {
            TxValidationError::OutputTooSmall { minimum, actual }
        }
        VE::TxTooLarge { maximum, actual } => TxValidationError::TxTooLarge { maximum, actual },
        VE::MissingRequiredSigner(signer) => TxValidationError::MissingRequiredSigner { signer },
        VE::MissingWitness(input) => TxValidationError::MissingWitness { input },
        VE::TtlExpired { current_slot, ttl } => TxValidationError::TtlExpired { current_slot, ttl },
        VE::NotYetValid {
            current_slot,
            valid_from,
        } => TxValidationError::NotYetValid {
            current_slot,
            valid_from,
        },
        VE::ScriptFailed(reason) => TxValidationError::ScriptFailed { reason },
        VE::InsufficientCollateral => TxValidationError::InsufficientCollateral,
        VE::TooManyCollateralInputs { max, actual } => {
            TxValidationError::TooManyCollateralInputs { max, actual }
        }
        VE::CollateralNotFound(input) => TxValidationError::CollateralNotFound { input },
        VE::CollateralHasTokens(input) => TxValidationError::CollateralHasTokens { input },
        VE::CollateralMismatch { declared, computed } => {
            TxValidationError::CollateralMismatch { declared, computed }
        }
        VE::ReferenceInputNotFound(input) => TxValidationError::ReferenceInputNotFound { input },
        VE::ReferenceInputOverlapsInput(input) => {
            TxValidationError::ReferenceInputOverlapsInput { input }
        }
        VE::ReferenceInputsNotDisjointFromInputs(inputs) => {
            TxValidationError::ReferenceInputsNotDisjointFromInputs { inputs }
        }
        VE::MultiAssetNotConserved {
            policy,
            input_side,
            output_side,
        } => TxValidationError::MultiAssetNotConserved {
            policy,
            input_side,
            output_side,
        },
        VE::InvalidMint => TxValidationError::InvalidMint,
        VE::ExUnitsExceeded => TxValidationError::ExUnitsExceeded,
        VE::ScriptDataHashMismatch { expected, actual } => {
            TxValidationError::ScriptDataHashMismatch { expected, actual }
        }
        VE::UnexpectedScriptDataHash => TxValidationError::UnexpectedScriptDataHash,
        VE::MissingScriptDataHash => TxValidationError::MissingScriptDataHash,
        VE::DuplicateInput(input) => TxValidationError::DuplicateInput { input },
        VE::NativeScriptFailed => TxValidationError::NativeScriptFailed,
        VE::InvalidWitnessSignature(vkey) => TxValidationError::InvalidWitnessSignature { vkey },
        VE::NetworkMismatch { expected, actual } => TxValidationError::NetworkMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        },
        VE::AuxiliaryDataHashWithoutData => TxValidationError::AuxiliaryDataHashWithoutData,
        VE::AuxiliaryDataWithoutHash => TxValidationError::AuxiliaryDataWithoutHash,
        VE::BlockExUnitsExceeded {
            resource,
            limit,
            total,
        } => TxValidationError::BlockExUnitsExceeded {
            resource,
            limit,
            total,
        },
        VE::OutputValueTooLarge { maximum, actual } => {
            TxValidationError::OutputValueTooLarge { maximum, actual }
        }
        VE::MissingRawCbor => TxValidationError::MissingRawCbor,
        VE::MissingSlotConfig => TxValidationError::MissingSlotConfig,
        VE::IsValidTagMismatch { declared, evaluated } => {
            TxValidationError::IsValidTagMismatch { declared, evaluated }
        }
        VE::MissingSpendRedeemer { index } => TxValidationError::MissingSpendRedeemer { index },
        VE::RedeemerIndexOutOfRange { tag, index, max } => {
            TxValidationError::RedeemerIndexOutOfRange { tag, index, max: max as u32 }
        }
        VE::MissingInputWitness(credential) => {
            TxValidationError::MissingInputWitness { credential }
        }
        VE::MissingScriptWitness(credential) => {
            TxValidationError::MissingScriptWitness { credential }
        }
        VE::MissingWithdrawalWitness(credential) => {
            TxValidationError::MissingWithdrawalWitness { credential }
        }
        VE::MissingWithdrawalScriptWitness(credential) => {
            TxValidationError::MissingWithdrawalScriptWitness { credential }
        }
        VE::MissingCertificateWitness(credential) => {
            TxValidationError::MissingCertificateWitness { credential }
        }
        VE::ValueOverflow => TxValidationError::ValueOverflow,
        VE::EraGatingViolation {
            certificate_type,
            required_era,
            current_era,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "Era gating violation: {certificate_type} requires {required_era}, current era is {current_era}"
            ),
        },
        VE::GovernancePreConway { current_version } => TxValidationError::ScriptFailed {
            reason: format!(
                "Governance features not available pre-Conway (current protocol version: {current_version})"
            ),
        },
        VE::TreasuryValueMismatch { declared, actual } => TxValidationError::ScriptFailed {
            reason: format!("Treasury value mismatch: declared {declared}, actual {actual}"),
        },
        VE::UnelectedCommitteeMember { cold_credential_hash } => TxValidationError::ScriptFailed {
            reason: format!("Unelected committee member: {cold_credential_hash}"),
        },
        VE::MissingRedeemer { tag, index } => TxValidationError::ScriptFailed {
            reason: format!("Missing redeemer for {tag} at index {index}"),
        },
        VE::MissingDatumWitness(datum_hash) => TxValidationError::ScriptFailed {
            reason: format!("Missing datum witness for script-locked input: datum hash {datum_hash}"),
        },
        VE::ExtraDatumWitness(datum_hash) => TxValidationError::ScriptFailed {
            reason: format!("Extra (unreferenced) datum witness in transaction: datum hash {datum_hash}"),
        },
        VE::TxRefScriptSizeTooLarge { actual, limit } => TxValidationError::TxTooLarge {
            // Map to TxTooLarge — closest semantic match for a transaction that
            // exceeds a size-based limit (ppMaxRefScriptSizePerTxG, Conway+).
            maximum: limit,
            actual,
        },
        VE::ZeroWithdrawal { account } => TxValidationError::ScriptFailed {
            reason: format!("Zero withdrawal amount for reward account: {account}"),
        },
        VE::WithdrawalsNotInRewardsCERTS { bad } => TxValidationError::ScriptFailed {
            reason: format!("WithdrawalsNotInRewardsCERTS: {bad:?}"),
        },
        VE::ConwayWithdrawalsMissingAccounts { missing } => {
            TxValidationError::WithdrawalsMissingAccounts {
                missing: missing.clone(),
            }
        }
        VE::ConwayIncompleteWithdrawals { incomplete } => {
            TxValidationError::IncompleteWithdrawals {
                mismatches: incomplete.clone(),
            }
        }
        VE::PoolRetirementTooLate {
            retirement_epoch,
            current_epoch,
            e_max,
            ..
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "Pool retirement epoch {retirement_epoch} exceeds max (current {current_epoch} + e_max {e_max})"
            ),
        },
        VE::StakeRegistrationDepositMismatch { declared, expected } => {
            TxValidationError::ScriptFailed {
                reason: format!(
                    "Conway stake registration deposit mismatch: declared={declared}, expected={expected}"
                ),
            }
        }
        VE::StakeKeyHasNonZeroBalance {
            credential_hash,
            balance,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "Stake deregistration rejected: credential {credential_hash} has non-zero balance ({balance} lovelace)"
            ),
        },
        VE::StakeDeregistrationRefundMismatch { declared, expected } => {
            TxValidationError::ScriptFailed {
                reason: format!(
                    "Conway stake deregistration refund mismatch: declared={declared}, expected={expected}"
                ),
            }
        }
        VE::StakeKeyAlreadyRegistered { credential_hash } => TxValidationError::ScriptFailed {
            reason: format!(
                "Stake registration rejected: credential {credential_hash} is already registered"
            ),
        },
        VE::DelegateePoolNotRegistered { pool_id } => TxValidationError::ScriptFailed {
            reason: format!(
                "Stake delegation rejected: target pool {pool_id} is not registered"
            ),
        },
        VE::DRepAlreadyRegistered { credential_hash } => TxValidationError::ScriptFailed {
            reason: format!(
                "DRep registration rejected: credential {credential_hash} is already registered"
            ),
        },
        VE::DRepIncorrectDeposit { declared, expected } => TxValidationError::ScriptFailed {
            reason: format!(
                "DRep registration rejected: declared deposit {declared} does not match \
                 drep_deposit parameter {expected} (ConwayDRepIncorrectDeposit)"
            ),
        },
        VE::ProposalDepositIncorrect { declared, expected } => TxValidationError::ScriptFailed {
            reason: format!(
                "Governance proposal rejected: declared deposit {declared} does not match \
                 gov_action_deposit parameter {expected} (ProposalDepositIncorrect)"
            ),
        },
        VE::CommitteeHasPreviouslyResigned { cold_credential_hash } => {
            TxValidationError::ScriptFailed {
                reason: format!(
                    "CommitteeHotAuth rejected: cold credential {cold_credential_hash} has previously resigned \
                     (ConwayCommitteeHasPreviouslyResigned)"
                ),
            }
        }
        VE::VrfKeyHashAlreadyRegistered {
            vrf_keyhash,
            existing_pool_id,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "VRF key {vrf_keyhash} is already registered to pool {existing_pool_id}"
            ),
        },
        VE::StakePoolCostTooLow { actual, minimum } => TxValidationError::ScriptFailed {
            reason: format!(
                "Pool registration rejected: cost {actual} is below minimum pool cost {minimum} \
                 (StakePoolCostTooLowPOOL)"
            ),
        },
        VE::PoolRewardAccountWrongNetwork { expected, actual } => TxValidationError::ScriptFailed {
            reason: format!(
                "Pool registration rejected: reward account network {actual:?} does not match \
                 transaction network {expected:?} (WrongNetworkInTxBody)"
            ),
        },
        VE::AuxiliaryDataHashMismatch => TxValidationError::ScriptFailed {
            reason: "Auxiliary data hash mismatch: declared hash does not match blake2b_256 of \
                     aux data bytes (AuxDataHashMismatch)"
                .to_string(),
        },
        VE::WrongNetworkInOutput { expected, actual } => TxValidationError::ScriptFailed {
            reason: format!(
                "Output address network {actual:?} does not match node network {expected:?} \
                 (WrongNetworkInOutput)"
            ),
        },
        VE::WrongNetworkWithdrawal { expected, actual } => TxValidationError::ScriptFailed {
            reason: format!(
                "Withdrawal reward address network {actual:?} does not match node network \
                 {expected:?} (WrongNetworkWithdrawal)"
            ),
        },
        VE::ConstitutionPolicyMismatch { expected, actual } => TxValidationError::ScriptFailed {
            reason: format!(
                "Governance proposal policy_hash mismatch: constitution requires {expected}, \
                 proposal has {actual} (ConstitutionPolicyMismatch)"
            ),
        },
        VE::UnspendableUTxONoDatumHash { input, language } => TxValidationError::ScriptFailed {
            reason: format!(
                "Script-locked input {input} has no datum hash but uses {language} \
                 (UnspendableUTxONoDatumHash)"
            ),
        },
        VE::WdrlNotDelegatedToDRep { credential_hash } => TxValidationError::ScriptFailed {
            reason: format!(
                "Withdrawal rejected: KeyHash reward account {credential_hash} has no DRep \
                 delegation (ConwayWdrlNotDelegatedToDRep)"
            ),
        },
        VE::MalformedProposal { reason } => TxValidationError::ScriptFailed {
            reason: format!("Governance proposal rejected: malformed PParamsUpdate ({reason})"),
        },
        VE::DisallowedVoters { violations } => TxValidationError::ScriptFailed {
            reason: format!("DisallowedVoters: {violations:?}"),
        },
        VE::VotersDoNotExist { voters } => TxValidationError::ScriptFailed {
            reason: format!("VotersDoNotExist: {voters:?}"),
        },
        VE::VotingOnExpiredGovAction { expired_votes } => TxValidationError::ScriptFailed {
            reason: format!("VotingOnExpiredGovAction: {expired_votes:?}"),
        },
        VE::ProposalReturnAccountDoesNotExist { bad_addrs } => TxValidationError::ScriptFailed {
            reason: format!("ProposalReturnAccountDoesNotExist: {bad_addrs:?}"),
        },
        VE::ProposalProcedureNetworkIdMismatch {
            expected,
            mismatched,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "ProposalProcedureNetworkIdMismatch: expected={expected}, mismatched={mismatched:?}"
            ),
        },
        VE::TreasuryWithdrawalsNetworkIdMismatch {
            expected,
            mismatched,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "TreasuryWithdrawalsNetworkIdMismatch: expected={expected}, mismatched={mismatched:?}"
            ),
        },
        VE::ZeroTreasuryWithdrawals {
            offending_proposals,
        } => TxValidationError::ScriptFailed {
            reason: format!("ZeroTreasuryWithdrawals: {offending_proposals:?}"),
        },
        VE::ConflictingCommitteeUpdate { conflicts } => TxValidationError::ScriptFailed {
            reason: format!("ConflictingCommitteeUpdate: {conflicts:?}"),
        },
        VE::ExpirationEpochTooSmall { invalid_members } => TxValidationError::ScriptFailed {
            reason: format!("ExpirationEpochTooSmall: {invalid_members:?}"),
        },
        VE::ExtraRedeemer { tag, index } => TxValidationError::ScriptFailed {
            reason: format!(
                "Extra redeemer with no matching script purpose: tag={tag}, index={index}"
            ),
        },
        VE::ScriptLockedCollateral { inputs } => TxValidationError::ScriptFailed {
            reason: format!("Collateral input(s) at script-locked addresses: {inputs:?}"),
        },
        VE::ExtraneousScriptWitness { hashes } => TxValidationError::ScriptFailed {
            reason: format!("Extraneous script witness(es) not needed by transaction: {hashes:?}"),
        },
        VE::PoolMedataHashTooBig { pool, hash_size } => TxValidationError::ScriptFailed {
            reason: format!("PoolMedataHashTooBig: pool={pool}, hash_size={hash_size}"),
        },
        VE::OutputBootAddrAttrsTooBig { oversized_outputs } => TxValidationError::ScriptFailed {
            reason: format!("OutputBootAddrAttrsTooBig: {oversized_outputs:?}"),
        },
        VE::MIRCertificateTooLateInEpoch {
            current_slot,
            deadline,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "MIRCertificateTooLateInEpoch: current_slot={current_slot}, deadline={deadline}"
            ),
        },
        VE::InsufficientForInstantaneousRewards {
            pot,
            required,
            available,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "InsufficientForInstantaneousRewards: pot={pot:?}, required={required}, available={available}"
            ),
        },
        VE::MIRTransferNotCurrentlyAllowed => TxValidationError::ScriptFailed {
            reason: "MIRTransferNotCurrentlyAllowed (pre-Alonzo MIR pot-to-pot transfer)"
                .to_string(),
        },
        VE::MIRNegativesNotCurrentlyAllowed => TxValidationError::ScriptFailed {
            reason: "MIRNegativesNotCurrentlyAllowed (pre-Alonzo negative MIR delta)".to_string(),
        },
        VE::MIRProducesNegativeUpdate { credentials } => TxValidationError::ScriptFailed {
            reason: format!("MIRProducesNegativeUpdate: credentials={credentials:?}"),
        },
        VE::InsufficientForTransferDELEG {
            pot,
            requested,
            available,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "InsufficientForTransferDELEG: pot={pot:?}, requested={requested}, available={available}"
            ),
        },
        VE::MIRNegativeTransfer { pot, amount } => TxValidationError::ScriptFailed {
            reason: format!("MIRNegativeTransfer: pot={pot:?}, amount={amount}"),
        },
        VE::NonGenesisUpdatePPUP { proposed, genesis } => TxValidationError::ScriptFailed {
            reason: format!(
                "NonGenesisUpdatePPUP: proposed_keys not subset of genesis_delegates \
                 ({proposed:?} ∉ {genesis:?})"
            ),
        },
        VE::PPUpdateWrongEpoch {
            current,
            target,
            period,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "PPUpdateWrongEpoch: current={current}, target={target}, period={period:?}"
            ),
        },
        VE::PVCannotFollowPPUP { bad_pv } => TxValidationError::ScriptFailed {
            reason: format!("PVCannotFollowPPUP: bad_pv={bad_pv:?}"),
        },
    }
}

// ─── Connection metrics bridges ──────────────────────────────────────────────

/// Bridges N2N server connection events to the node metrics system.
#[allow(dead_code)] // used by networking rewrite
pub(crate) struct N2NConnectionMetrics {
    pub metrics: Arc<crate::metrics::NodeMetrics>,
}

impl dugite_network::ConnectionMetrics for N2NConnectionMetrics {
    fn on_connect(&self) {
        self.metrics
            .n2n_connections_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // n2n_connections_active is NOT updated here — it is a derived metric
        // sourced from ConnectionLifecycleManager::connection_count() via
        // update_peer_metrics(). Maintaining it via fetch_add/fetch_sub causes
        // drift (outbound paths were never increment-paired with decrements).
    }
    fn on_disconnect(&self) {
        // n2n_connections_active is NOT updated here — see on_connect comment.
    }
    fn on_error(&self, label: &str) {
        self.metrics.record_protocol_error(label);
    }
}

/// Bridges N2C server connection events to the node metrics system.
#[allow(dead_code)] // used by networking rewrite
pub(crate) struct N2CConnectionMetrics {
    pub metrics: Arc<crate::metrics::NodeMetrics>,
}

impl dugite_network::ConnectionMetrics for N2CConnectionMetrics {
    fn on_connect(&self) {
        self.metrics
            .n2c_connections_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics
            .n2c_connections_active
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn on_disconnect(&self) {
        self.metrics
            .n2c_connections_active
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn on_error(&self, label: &str) {
        self.metrics.record_protocol_error(label);
    }
}

// ─── UTxO snapshot helper ────────────────────────────────────────────────────

/// Convert a UTxO entry to a snapshot for N2C queries.
pub(crate) fn utxo_to_snapshot(
    input: &dugite_primitives::transaction::TransactionInput,
    output: &dugite_primitives::transaction::TransactionOutput,
) -> UtxoSnapshot {
    let multi_asset: dugite_network::MultiAssetSnapshot = output
        .value
        .multi_asset
        .iter()
        .map(|(policy, assets)| {
            let assets_vec: Vec<(Vec<u8>, u64)> = assets
                .iter()
                .map(|(name, qty)| (name.0.clone(), *qty))
                .collect();
            (policy.as_ref().to_vec(), assets_vec)
        })
        .collect();

    let (datum_hash, inline_datum) = match &output.datum {
        dugite_primitives::transaction::OutputDatum::DatumHash(h) => {
            (Some(h.as_ref().to_vec()), None)
        }
        dugite_primitives::transaction::OutputDatum::InlineDatum { data, raw_cbor } => {
            // Prefer the preserved CBOR (byte-exact original) — falls back to
            // a fresh deterministic re-encoding of `data` only when raw_cbor
            // wasn't kept (e.g., locally constructed `TransactionOutput`s
            // that never round-tripped through the wire). The cardano-cli
            // auto-balance evaluator computes the per-redeemer `ex_units`
            // budget from the bytes we return here, so any byte-level drift
            // surfaces as an `IsValid True / FailedUnexpectedly / PlutusFailure`
            // when cardano-node validates the dugite-forged block.
            let cbor = raw_cbor
                .clone()
                .unwrap_or_else(|| dugite_serialization::encode_plutus_data(data));
            (None, Some(cbor))
        }
        dugite_primitives::transaction::OutputDatum::None => (None, None),
    };

    UtxoSnapshot {
        tx_hash: input.transaction_id.as_ref().to_vec(),
        output_index: input.index,
        address_bytes: output.address.to_bytes(),
        lovelace: output.value.coin.0,
        multi_asset,
        datum_hash,
        inline_datum,
        script_ref: output.script_ref.clone(),
        raw_cbor: output.raw_cbor.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_ledger::state::ProposalState;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{Anchor, GovAction, GovActionId, ProposalProcedure};
    use dugite_primitives::value::Lovelace;

    /// Verifies that `build_governance_validation_state` projects the live
    /// `LedgerState` Conway governance fields into the shapes expected by
    /// `ValidationContext`.  This is the production wiring used by the N2C
    /// tx-submission path; getting the projection wrong would silently
    /// disable the cross-tx voting predicates (`DisallowedVoters` etc.).
    #[test]
    fn build_governance_validation_state_projects_proposals_and_hot_keys() {
        let mut ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Seed one active proposal and one committee hot-key authorisation.
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 7,
        };
        let proposal_state = ProposalState {
            procedure: ProposalProcedure {
                deposit: Lovelace(123_456_789),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::InfoAction,
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::ZERO,
                },
            },
            proposed_epoch: EpochNo(40),
            expires_epoch: EpochNo(50),
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
        };
        let cold_cred = Hash32::from_bytes([0xC0; 32]);
        let hot_cred = Hash32::from_bytes([0x77; 32]);

        {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            gov.proposals.insert(action_id.clone(), proposal_state);
            gov.committee_hot_keys.insert(cold_cred, hot_cred);
        }

        let (active, hot_keys) = build_governance_validation_state(&ledger);

        // Proposal projection: every ActiveProposal field must mirror the
        // ProposalState/ProposalProcedure source.
        assert_eq!(active.len(), 1);
        let ap = active.get(&action_id).expect("proposal must be projected");
        assert!(matches!(ap.gov_action, GovAction::InfoAction));
        assert_eq!(ap.return_addr, vec![0xe0; 29]);
        assert_eq!(ap.deposit.0, 123_456_789);
        assert_eq!(ap.expires_after_epoch.0, 50);
        assert_eq!(ap.proposed_in_epoch.0, 40);

        // Committee hot-key projection: only the values (hot creds), not
        // the cold-cred keys, are exposed to the validator (matches
        // Haskell `authorizedHotCommitteeCredentials`).
        assert_eq!(hot_keys.len(), 1);
        assert!(hot_keys.contains(&hot_cred));
        assert!(!hot_keys.contains(&cold_cred));
    }

    /// On a freshly-constructed `LedgerState` (no on-chain proposals or
    /// committee authorisations) the projection must yield empty maps —
    /// i.e. the cross-tx predicates default to no-op, matching the
    /// pre-task behaviour for chains that haven't entered Conway.
    #[test]
    fn build_governance_validation_state_empty_for_fresh_ledger() {
        let ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let (active, hot_keys) = build_governance_validation_state(&ledger);
        assert!(active.is_empty());
        assert!(hot_keys.is_empty());
    }

    // ─── convert_validation_error ────────────────────────────────────────────
    //
    // The N2C tx-submission path calls `convert_validation_error` on every
    // ledger rejection.  cardano-cli surfaces the resulting `TxValidationError`
    // discriminant to operators and to scripts that match on it, so the
    // ledger→network mapping is part of our wire contract.  A regression here
    // would silently rebrand "fee too small" rejections as generic script
    // failures (etc.) — exactly the class of bug that bites operators in
    // production.

    #[test]
    fn convert_validation_error_preserves_known_variants() {
        use dugite_ledger::validation::ValidationError as VE;

        // Structural pass-throughs: discriminant + payload must be preserved.
        let e = convert_validation_error(VE::FeeTooSmall {
            minimum: 100,
            actual: 50,
        });
        assert!(matches!(
            e,
            TxValidationError::FeeTooSmall {
                minimum: 100,
                actual: 50
            }
        ));

        let e = convert_validation_error(VE::TxTooLarge {
            maximum: 16384,
            actual: 20000,
        });
        assert!(matches!(
            e,
            TxValidationError::TxTooLarge {
                maximum: 16384,
                actual: 20000
            }
        ));

        let e = convert_validation_error(VE::ValueNotConserved {
            inputs: 10,
            outputs: 9,
            fee: 0,
        });
        assert!(matches!(
            e,
            TxValidationError::ValueNotConserved {
                inputs: 10,
                outputs: 9,
                fee: 0
            }
        ));

        let e = convert_validation_error(VE::InsufficientCollateral);
        assert!(matches!(e, TxValidationError::InsufficientCollateral));

        let e = convert_validation_error(VE::NoInputs);
        assert!(matches!(e, TxValidationError::NoInputs));

        let e = convert_validation_error(VE::ValueOverflow);
        assert!(matches!(e, TxValidationError::ValueOverflow));

        // IsValidTagMismatch (#522) must map 1:1 to the network variant so
        // the rejection reason is visible to the submitting client.
        let e = convert_validation_error(VE::IsValidTagMismatch {
            declared: false,
            evaluated: true,
        });
        assert!(
            matches!(
                e,
                TxValidationError::IsValidTagMismatch {
                    declared: false,
                    evaluated: true
                }
            ),
            "IsValidTagMismatch must map 1:1 to network variant, got {e:?}"
        );
    }

    #[test]
    fn convert_validation_error_collapses_conway_predicates_to_script_failed() {
        use dugite_ledger::validation::ValidationError as VE;

        // Many Conway-only predicates have no dedicated network-side variant
        // and intentionally fold into `ScriptFailed { reason }` so cardano-cli
        // shows a stable rejection class.  Verify a few representatives so
        // refactoring the convert table doesn't quietly drop these branches.
        let cases: Vec<VE> = vec![
            VE::ZeroWithdrawal {
                account: "stake_test1abc".to_string(),
            },
            VE::DRepIncorrectDeposit {
                declared: 1,
                expected: 500_000_000,
            },
            VE::ProposalDepositIncorrect {
                declared: 1,
                expected: 100_000_000_000,
            },
            VE::DisallowedVoters { violations: vec![] },
            VE::VotersDoNotExist { voters: vec![] },
            VE::VotingOnExpiredGovAction {
                expired_votes: vec![],
            },
            VE::ExtraRedeemer {
                tag: "Spend".to_string(),
                index: 0,
            },
        ];
        for v in cases {
            let mapped = convert_validation_error(v);
            assert!(
                matches!(mapped, TxValidationError::ScriptFailed { .. }),
                "Conway predicate did not collapse to ScriptFailed: {mapped:?}"
            );
        }
    }

    #[test]
    fn convert_validation_error_maps_size_predicates_to_size_variants() {
        use dugite_ledger::validation::ValidationError as VE;

        // `TxRefScriptSizeTooLarge` is intentionally remapped to `TxTooLarge`
        // because that is the closest semantic match — exercised here so
        // future refactors keep the mapping intentional rather than accidental.
        let e = convert_validation_error(VE::TxRefScriptSizeTooLarge {
            actual: 2048,
            limit: 1024,
        });
        assert!(matches!(
            e,
            TxValidationError::TxTooLarge {
                maximum: 1024,
                actual: 2048
            }
        ));

        let e = convert_validation_error(VE::OutputValueTooLarge {
            maximum: 5000,
            actual: 6000,
        });
        assert!(matches!(
            e,
            TxValidationError::OutputValueTooLarge {
                maximum: 5000,
                actual: 6000
            }
        ));
    }

    // ─── utxo_to_snapshot ────────────────────────────────────────────────────

    fn enterprise_addr_bytes() -> Vec<u8> {
        // Type-6 (Enterprise, key-hash payment) on mainnet: header = 0b0110_0001
        // followed by a 28-byte payment key hash.
        let mut bytes = vec![0b0110_0001];
        bytes.extend_from_slice(&[0x11; 28]);
        bytes
    }

    #[test]
    fn utxo_to_snapshot_lovelace_only() {
        use dugite_primitives::address::Address;
        use dugite_primitives::transaction::{OutputDatum, TransactionInput, TransactionOutput};
        use dugite_primitives::value::Value;

        let addr_bytes = enterprise_addr_bytes();
        let address = Address::from_bytes(&addr_bytes).expect("decode enterprise addr");

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x42; 32]),
            index: 1,
        };
        let output = TransactionOutput {
            address,
            value: Value::lovelace(7_654_321),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let snap = utxo_to_snapshot(&input, &output);
        assert_eq!(snap.tx_hash, [0x42u8; 32]);
        assert_eq!(snap.output_index, 1);
        assert_eq!(snap.address_bytes, addr_bytes);
        assert_eq!(snap.lovelace, 7_654_321);
        assert!(snap.multi_asset.is_empty());
        assert!(snap.datum_hash.is_none());
        assert!(snap.raw_cbor.is_none());
    }

    #[test]
    fn utxo_to_snapshot_propagates_datum_hash_and_raw_cbor() {
        use dugite_primitives::address::Address;
        use dugite_primitives::hash::DatumHash;
        use dugite_primitives::transaction::{OutputDatum, TransactionInput, TransactionOutput};
        use dugite_primitives::value::Value;

        let addr_bytes = enterprise_addr_bytes();
        let address = Address::from_bytes(&addr_bytes).unwrap();
        let datum_hash = DatumHash::from_bytes([0xCD; 32]);
        let raw = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let output = TransactionOutput {
            address,
            value: Value::lovelace(1_000_000),
            datum: OutputDatum::DatumHash(datum_hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: Some(raw.clone()),
        };
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x01; 32]),
            index: 0,
        };

        let snap = utxo_to_snapshot(&input, &output);
        assert_eq!(snap.datum_hash.as_deref(), Some(&[0xCDu8; 32][..]));
        assert_eq!(snap.raw_cbor.as_deref(), Some(raw.as_slice()));
    }

    #[test]
    fn utxo_to_snapshot_inline_datum_does_not_set_datum_hash() {
        use dugite_primitives::address::Address;
        use dugite_primitives::transaction::{
            OutputDatum, PlutusData, TransactionInput, TransactionOutput,
        };
        use dugite_primitives::value::Value;

        // Only `OutputDatum::DatumHash` populates `snap.datum_hash`; inline
        // datums travel via `raw_cbor` instead.  Locking that in prevents
        // accidental hash leakage from inline-datum outputs.
        let addr = Address::from_bytes(&enterprise_addr_bytes()).unwrap();
        let output = TransactionOutput {
            address: addr,
            value: Value::lovelace(1),
            datum: OutputDatum::InlineDatum {
                data: PlutusData::Integer(42i128),
                raw_cbor: Some(vec![0x18, 0x2A]),
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0; 32]),
            index: 0,
        };
        let snap = utxo_to_snapshot(&input, &output);
        assert!(snap.datum_hash.is_none());
    }

    /// `utxo_to_snapshot` must propagate `script_ref` from the output so that the
    /// N2C encoder can emit CBOR key 3 even after an LSM round-trip (where
    /// `TransactionOutput.raw_cbor` is cleared by `#[serde(skip)]`).
    #[test]
    fn utxo_to_snapshot_propagates_script_ref() {
        use dugite_primitives::address::Address;
        use dugite_primitives::transaction::{
            OutputDatum, ScriptRef, TransactionInput, TransactionOutput,
        };
        use dugite_primitives::value::Value;

        let addr = Address::from_bytes(&enterprise_addr_bytes()).unwrap();
        let script_bytes = vec![0x01, 0x00, 0x00, 0x22, 0x21, 0x20, 0x01, 0x01]; // always-true-v2
        let output = TransactionOutput {
            address: addr,
            value: Value::lovelace(3_000_000),
            datum: OutputDatum::None,
            script_ref: Some(ScriptRef::PlutusV2(script_bytes.clone())),
            is_legacy: false,
            raw_cbor: None, // Simulates post-LSM state: raw_cbor is #[serde(skip)]
        };
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xAB; 32]),
            index: 2,
        };

        let snap = utxo_to_snapshot(&input, &output);

        // script_ref must be propagated even though raw_cbor is None
        match snap.script_ref {
            Some(ScriptRef::PlutusV2(bytes)) => {
                assert_eq!(
                    bytes, script_bytes,
                    "PlutusV2 script bytes must survive utxo_to_snapshot"
                );
            }
            other => panic!("Expected Some(PlutusV2), got {other:?}"),
        }
        // raw_cbor must still be None (we didn't set it)
        assert!(snap.raw_cbor.is_none());
    }

    // ─── Connection metric bridges ───────────────────────────────────────────

    #[test]
    fn n2n_connection_metrics_increments_total_only() {
        use dugite_network::ConnectionMetrics as _;
        use std::sync::atomic::Ordering::Relaxed;

        // The active gauge is intentionally NOT bumped here — it is sourced
        // from `ConnectionLifecycleManager` to avoid drift.  This test pins
        // that contract: see the inline comment in `N2NConnectionMetrics`.
        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        let bridge = N2NConnectionMetrics {
            metrics: metrics.clone(),
        };
        bridge.on_connect();
        bridge.on_connect();
        assert_eq!(metrics.n2n_connections_total.load(Relaxed), 2);
        assert_eq!(metrics.n2n_connections_active.load(Relaxed), 0);
        bridge.on_disconnect();
        // disconnect must not decrement either counter.
        assert_eq!(metrics.n2n_connections_total.load(Relaxed), 2);
        assert_eq!(metrics.n2n_connections_active.load(Relaxed), 0);
    }

    #[test]
    fn n2c_connection_metrics_tracks_active_gauge() {
        use dugite_network::ConnectionMetrics as _;
        use std::sync::atomic::Ordering::Relaxed;

        // N2C connections are short-lived (one per cardano-cli invocation) and
        // self-contained, so the bridge maintains the active gauge directly.
        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        let bridge = N2CConnectionMetrics {
            metrics: metrics.clone(),
        };
        bridge.on_connect();
        bridge.on_connect();
        assert_eq!(metrics.n2c_connections_total.load(Relaxed), 2);
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 2);
        bridge.on_disconnect();
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 1);
        bridge.on_disconnect();
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 0);
        // total is monotonic — disconnects must not roll it back.
        assert_eq!(metrics.n2c_connections_total.load(Relaxed), 2);
    }

    #[test]
    fn connection_metrics_record_protocol_error() {
        use dugite_network::ConnectionMetrics as _;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        let bridge = N2NConnectionMetrics {
            metrics: metrics.clone(),
        };
        bridge.on_error("handshake_refused");
        bridge.on_error("handshake_refused");
        bridge.on_error("decode_failed");
        // Sanity-check that distinct labels accumulate independently in the
        // protocol-error map exposed via Prometheus.
        let prometheus = metrics.to_prometheus();
        assert!(
            prometheus.contains("handshake_refused\"} 2"),
            "expected handshake_refused=2 in:\n{prometheus}"
        );
        assert!(
            prometheus.contains("decode_failed\"} 1"),
            "expected decode_failed=1 in:\n{prometheus}"
        );
    }
}
