/// Byron era ledger rules
///
/// The Byron era uses OBFT (Optimistic Byzantine Fault Tolerance) consensus
/// and has a simpler transaction model compared to all post-Shelley eras:
///
/// - Inputs: `TxIn(TxId, index)` — simple UTxO set lookup, no scripts
/// - Outputs: `TxOut(ByronAddress, Lovelace)` — no multi-asset, no datums
/// - Fees: `fee = sum(inputs) - sum(outputs)`, must satisfy `fee >= a + ceiling(size * b)`
/// - No certificates, withdrawals, staking, Plutus scripts, or governance
///
/// Byron transactions always succeed when structurally valid — there is no
/// `is_valid` flag and no collateral mechanism.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use dugite_primitives::block::{
    Block, BlockHeader, ByronBlockAux, ByronParamsUpdate, ByronUpdVote,
};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_224, Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{Transaction, TransactionInput, TransactionOutput};
use dugite_primitives::value::Lovelace;
use sha3::Digest;

use crate::state::substates::*;
use crate::state::{BlockValidationMode, LedgerError};
use crate::utxo_diff::UtxoDiff;

use super::{EraRules, RuleContext};

/// Byron-specific validation error
#[derive(Debug, thiserror::Error)]
pub enum ByronError {
    /// An input referenced in the transaction body is not present in the UTxO set.
    #[error("Input not found in UTxO set: {0}")]
    InputNotFound(String),

    /// `sum(inputs) != sum(outputs) + fee`
    #[error("Value not conserved: inputs={inputs}, outputs={outputs}, fee={fee}")]
    ValueNotConserved { inputs: u64, outputs: u64, fee: u64 },

    /// `fee < a + ceiling(size * b)` (and not exempt via the AVVM-redeem rule)
    #[error("Fee too small: minimum={minimum}, actual={actual}")]
    FeeTooSmall { minimum: u64, actual: u64 },

    /// A transaction output contains multi-asset tokens, which are not valid in Byron
    #[error("Byron output contains multi-asset tokens (only ADA is valid in Byron)")]
    MultiAssetInOutput,

    /// A transaction has no inputs, which is structurally invalid
    #[error("Byron transaction has no inputs")]
    NoInputs,

    /// Integer overflow while summing input or output values
    #[error("Value overflow in Byron transaction accounting")]
    ValueOverflow,
}

/// Byron fee policy: `minFee = a + ceiling(size * b)`.
///
/// Byron encodes the fee policy as
/// `TxFeePolicy (TxFeePolicyTxSizeLinear (TxSizeLinear a b))` where `a :: Lovelace`
/// is a constant summand and `b :: Rational` is the per-byte multiplier — note `b`
/// is an EXACT rational, not an integer. Cross-validated byte-for-byte against
/// cardano-ledger `Cardano.Chain.Common.TxSizeLinear.calculateTxSizeLinear`:
///
/// ```haskell
/// calculateTxSizeLinear (TxSizeLinear a b) sz =
///   addLovelace a =<< flip scaleLovelaceRationalUp b <$> integerToLovelace sz
/// -- scaleLovelaceRationalUp (Lovelace x) b = Lovelace $ ceiling (toRational x * b)
/// ```
///
/// so `minFee = a + ceiling(size * b)` with CEILING rounding over exact rational
/// arithmetic (NOT floor, NOT round-half, NOT integer truncation of `b`).
/// <https://github.com/IntersectMBO/cardano-ledger/blob/d0e208885b8c7927aed758e003749fb3317612d3/eras/byron/ledger/impl/src/Cardano/Chain/Common/TxSizeLinear.hs#L55-L59>
///
/// The Byron genesis `blockVersionData.txFeePolicy` stores both values in Nano
/// (10^-9) scale. mainnet, preprod and preview all share the same values:
/// `summand = 155381000000000 / 1e9 = 155381` lovelace and
/// `multiplier = 43946000000 / 1e9 = 21973/500` (43.946 exactly). We hold the
/// multiplier as a reduced rational so the ceiling is computed with no precision
/// loss. (cardano-node reads these from genesis; dugite does not yet parse the
/// Byron `txFeePolicy`, so we pin the canonical network constants — these are the
/// exact genesis values, not an approximation.)
#[derive(Debug, Clone, Copy)]
pub struct ByronFeePolicy {
    /// Constant summand `a` (lovelace), always charged regardless of tx size.
    pub summand: u64,
    /// Per-byte multiplier `b` numerator (exact rational `mult_num / mult_den`).
    pub mult_num: u64,
    /// Per-byte multiplier `b` denominator.
    pub mult_den: u64,
}

impl ByronFeePolicy {
    /// Canonical Byron `txFeePolicy` summand for the public Cardano networks.
    pub const BYRON_SUMMAND: u64 = 155_381;
    /// Canonical Byron multiplier numerator (`21973/500 = 43.946`).
    pub const BYRON_MULT_NUM: u64 = 21_973;
    /// Canonical Byron multiplier denominator.
    pub const BYRON_MULT_DEN: u64 = 500;

    /// The canonical Byron fee policy shared by mainnet/preprod/preview.
    pub const fn canonical() -> Self {
        ByronFeePolicy {
            summand: Self::BYRON_SUMMAND,
            mult_num: Self::BYRON_MULT_NUM,
            mult_den: Self::BYRON_MULT_DEN,
        }
    }

    /// Compute the minimum fee for a transaction of the given serialized byte
    /// length: `a + ceiling(size * mult_num / mult_den)`.
    ///
    /// `size` MUST be the full `ATxAux` CBOR byte length (tx body + witnesses),
    /// matching Haskell `validateTxAux`'s `txSize = BS.length txBytes` where
    /// `txBytes` is the `ATxAux` annotation.
    ///
    /// Uses exact `u128` integer arithmetic for the ceiling division so the
    /// result is identical to Haskell's `ceiling (toRational size * b)`. Returns
    /// `None` on overflow or a zero denominator (defends against corrupt params).
    pub fn min_fee(&self, tx_size_bytes: u64) -> Option<u64> {
        let den = self.mult_den as u128;
        if den == 0 {
            return None;
        }
        // ceiling(size * num / den) == (size * num + den - 1) / den
        let scaled = (tx_size_bytes as u128).checked_mul(self.mult_num as u128)?;
        let ceil = scaled.checked_add(den - 1)? / den;
        (self.summand as u128)
            .checked_add(ceil)
            .and_then(|v| u64::try_from(v).ok())
    }
}

/// Result of validating and applying a single Byron transaction.
#[derive(Debug)]
pub struct ByronTxEffect {
    /// UTxO entries to remove (spent inputs)
    pub consumed: Vec<TransactionInput>,
    /// UTxO entries to add (new outputs indexed by the tx hash and output index)
    pub produced: Vec<(TransactionInput, TransactionOutput)>,
    /// Fee collected from this transaction
    pub fee: Lovelace,
}

/// Validate a single Byron-era transaction against the current UTxO set.
///
/// Returns a [`ByronTxEffect`] describing the state changes on success, or a
/// [`ByronError`] describing the first violation found.
///
/// # Byron UTxO rules validated here
///
/// 1. **At least one input** — structurally required by the Byron spec.
/// 2. **All inputs exist** — every `TxIn` must resolve to a UTxO entry.
/// 3. **No multi-asset outputs** — Byron outputs must be ADA-only.
/// 4. **Value conservation** — `sum(input values) == sum(output values) + fee`.
/// 5. **Minimum fee** — `fee >= fee_policy.min_fee(tx_size_bytes)`.
///
/// # Missing inputs during bootstrap
///
/// When replaying from genesis without the full UTxO history (e.g. after a
/// Mithril snapshot import that starts mid-chain), some inputs may be absent.
/// The caller (`apply_byron_block`) handles this gracefully by logging and
/// skipping the UTxO changes while still accumulating fees.
pub fn validate_byron_tx<F>(
    tx: &Transaction,
    mut lookup_utxo: F,
    fee_policy: ByronFeePolicy,
    tx_size_bytes: u64,
) -> Result<ByronTxEffect, ByronError>
where
    F: FnMut(&TransactionInput) -> Option<TransactionOutput>,
{
    // Rule 1: must have at least one input
    if tx.body.inputs.is_empty() {
        return Err(ByronError::NoInputs);
    }

    // Rule 2: resolve all inputs and accumulate their ADA value.
    //
    // Also track whether EVERY consumed UTxO is at a Byron redeem (AVVM)
    // address — Haskell `isRedeemUTxO`. When true, the minimum fee is 0
    // (validateTxAux: `if isRedeemUTxO inputUTxO then mkKnownLovelace @0`),
    // so AVVM voucher-redemption txs (which sweep the full balance with no
    // fee) validate with fee == 0.
    let mut input_sum: u64 = 0;
    let mut all_inputs_redeem = true;
    let mut consumed = Vec::with_capacity(tx.body.inputs.len());
    for input in &tx.body.inputs {
        let output = lookup_utxo(input).ok_or_else(|| {
            ByronError::InputNotFound(format!("{}#{}", input.transaction_id.to_hex(), input.index))
        })?;
        all_inputs_redeem &= matches!(
            &output.address,
            dugite_primitives::address::Address::Byron(b) if b.is_redeem()
        );
        // Sum only the coin component. Byron UTxOs are ADA-only; any multi-asset
        // entries (theoretically impossible in Byron but we are defensive) are ignored
        // for the purposes of value conservation — the multi-asset check on outputs
        // (Rule 3) will reject such transactions before they can steal value.
        input_sum = input_sum
            .checked_add(output.value.coin.0)
            .ok_or(ByronError::ValueOverflow)?;
        consumed.push(input.clone());
    }

    // Rule 3: outputs must be ADA-only (no multi-asset in Byron)
    for output in &tx.body.outputs {
        if !output.value.multi_asset.is_empty() {
            return Err(ByronError::MultiAssetInOutput);
        }
    }

    // Accumulate output value and build produced list
    let mut output_sum: u64 = 0;
    let mut produced = Vec::with_capacity(tx.body.outputs.len());
    for (idx, output) in tx.body.outputs.iter().enumerate() {
        output_sum = output_sum
            .checked_add(output.value.coin.0)
            .ok_or(ByronError::ValueOverflow)?;
        let out_input = TransactionInput {
            transaction_id: tx.hash,
            index: idx as u32,
        };
        produced.push((out_input, output.clone()));
    }

    // Rule 4 + fee: the Byron fee is IMPLICIT — `inputs - outputs`. Byron
    // transactions carry NO explicit fee field on the wire, so `tx.body.fee`
    // is always 0 for a decoded Byron tx (the previous code read it directly,
    // making the min-fee check see actual=0 and reject every fee-paying Byron
    // tx under full validation). Value conservation = inputs must cover
    // outputs; the surplus is the fee.
    let fee = input_sum
        .checked_sub(output_sum)
        .ok_or(ByronError::ValueNotConserved {
            inputs: input_sum,
            outputs: output_sum,
            fee: 0,
        })?;

    // Rule 5: minimum fee must be satisfied. AVVM-redeem txs are exempt
    // (minFee = 0) — Haskell `isRedeemUTxO` → `mkKnownLovelace @0`.
    let min_fee = if all_inputs_redeem {
        0
    } else {
        fee_policy
            .min_fee(tx_size_bytes)
            .ok_or(ByronError::ValueOverflow)?
    };
    if fee < min_fee {
        tracing::warn!(
            tx = %tx.hash.to_hex(),
            input_sum,
            output_sum,
            fee,
            min_fee,
            tx_size_bytes,
            "Byron min-fee check failed — diagnostic (input/output sums)"
        );
        return Err(ByronError::FeeTooSmall {
            minimum: min_fee,
            actual: fee,
        });
    }

    Ok(ByronTxEffect {
        consumed,
        produced,
        fee: Lovelace(fee),
    })
}

/// Whether to enforce Byron validation rules or just apply UTxO changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByronApplyMode {
    /// Enforce all Byron UTxO rules; return an error on any rule violation.
    /// Used when receiving new blocks from the network.
    ValidateAll,
    /// Trust the block content; skip validation and apply changes directly.
    /// Used for immutable DB replay, Mithril import, and rollback replay.
    ApplyOnly,
}

/// Error returned when a Byron block cannot be applied.
#[derive(Debug, thiserror::Error)]
#[error("Byron block error at slot {slot} tx {tx_hash}: {reason}")]
pub struct ByronBlockError {
    pub slot: u64,
    pub tx_hash: String,
    pub reason: ByronError,
}

/// Collected UTxO changes and fees for an entire Byron block.
///
/// Returned by [`apply_byron_block`] so the caller can apply the changes to
/// the UTxO store without overlapping borrows.
#[derive(Debug)]
pub struct ByronBlockEffect {
    /// Inputs to remove from the UTxO set (spent)
    pub spent: Vec<TransactionInput>,
    /// Outputs to add to the UTxO set (created)
    pub created: Vec<(TransactionInput, TransactionOutput)>,
    /// Total fees collected from this block
    pub fees: Lovelace,
}

impl Default for ByronBlockEffect {
    fn default() -> Self {
        Self {
            spent: Vec::new(),
            created: Vec::new(),
            fees: Lovelace(0),
        }
    }
}

/// Apply a Byron-era block's transactions and return the net UTxO changes.
///
/// This function validates or applies (depending on `mode`) each transaction
/// and accumulates the resulting changes into a [`ByronBlockEffect`] that the
/// caller applies to the UTxO store. Separating computation from mutation
/// avoids multiple mutable borrows of the same store.
///
/// # Byron rules (enforced in `ValidateAll` mode)
///
/// 1. Each transaction must have at least one input.
/// 2. Every input must be present in the UTxO set (`lookup_utxo`).
/// 3. Outputs may only contain ADA (no multi-asset).
/// 4. `sum(inputs) == sum(outputs) + fee` (value conservation).
/// 5. `fee >= min_fee_a * tx_size_bytes + min_fee_b` (minimum fee).
///
/// # Behavior on missing inputs (ApplyOnly mode)
///
/// During initial sync from a Mithril snapshot or partial history, some UTxO
/// inputs may not yet be present in the store. Rather than aborting the block,
/// the UTxO update for that transaction is skipped and only the fee is counted.
/// This matches the behavior of the Shelley+ path in `apply_block`.
///
/// # In-block dependencies
///
/// Transactions are processed in order. The lookup closure sees UTxO changes
/// made by earlier transactions in the same block because `ByronBlockEffect`
/// is built incrementally and the caller is expected to pass a closure that
/// reads from the in-flight effect in addition to the persistent store.
///
/// In the current implementation the caller (`apply_block`) applies the entire
/// effect after the function returns, which means within-block spending chains
/// work naturally: the lookup for tx N will see the outputs of tx 0..N-1
/// because those outputs were inserted into the UTxO store by the earlier
/// iteration of the loop in the caller.
pub fn apply_byron_block<FLookup>(
    transactions: &[Transaction],
    fee_policy: ByronFeePolicy,
    slot: u64,
    mode: ByronApplyMode,
    mut lookup_utxo: FLookup,
) -> Result<ByronBlockEffect, ByronBlockError>
where
    FLookup: FnMut(&TransactionInput) -> Option<TransactionOutput>,
{
    let mut effect = ByronBlockEffect::default();

    // Duplicate-tx guard (defensive; Byron blocks should not have them, but we
    // match the behaviour of the Shelley+ path).
    let mut seen = std::collections::HashSet::with_capacity(transactions.len());

    for tx in transactions {
        if !seen.insert(tx.hash) {
            tracing::warn!(
                tx_hash = %tx.hash.to_hex(),
                slot,
                "Byron: duplicate transaction hash in block, skipping"
            );
            continue;
        }

        // Derive serialized transaction size for fee calculation.
        // We use raw_cbor bytes when available (exact on-wire size).
        // Fall back to 0 when absent, making the minimum-fee check lenient —
        // acceptable in ApplyOnly mode where the block is already confirmed.
        let tx_size_bytes = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);

        match mode {
            ByronApplyMode::ValidateAll => {
                // Strict mode: validate all Byron rules; reject on any violation.
                let tx_effect = validate_byron_tx(tx, &mut lookup_utxo, fee_policy, tx_size_bytes)
                    .map_err(|reason| ByronBlockError {
                        slot,
                        tx_hash: tx.hash.to_hex(),
                        reason,
                    })?;

                effect.spent.extend(tx_effect.consumed);
                effect.created.extend(tx_effect.produced);
                effect.fees.0 = effect.fees.0.saturating_add(tx_effect.fee.0);
            }

            ByronApplyMode::ApplyOnly => {
                // Replay mode: trust the on-chain block; collect UTxO changes without
                // full validation. If inputs are missing (partial history), skip the
                // UTxO update for this tx but still count the fee.
                let mut all_inputs_present = true;
                let mut tx_consumed: Vec<TransactionInput> =
                    Vec::with_capacity(tx.body.inputs.len());

                for input in &tx.body.inputs {
                    if lookup_utxo(input).is_some() {
                        tx_consumed.push(input.clone());
                    } else {
                        tracing::debug!(
                            tx_hash = %tx.hash.to_hex(),
                            slot,
                            input = %format!("{}#{}", input.transaction_id.to_hex(), input.index),
                            "Byron ApplyOnly: input not in UTxO set, skipping UTxO update for tx"
                        );
                        all_inputs_present = false;
                        break;
                    }
                }

                if all_inputs_present {
                    effect.spent.extend(tx_consumed);
                    for (idx, output) in tx.body.outputs.iter().enumerate() {
                        let out_input = TransactionInput {
                            transaction_id: tx.hash,
                            index: idx as u32,
                        };
                        effect.created.push((out_input, output.clone()));
                    }
                }

                // Always accumulate fees (epoch accounting is independent of UTxO availability)
                effect.fees.0 = effect.fees.0.saturating_add(tx.body.fee.0);
            }
        }
    }

    Ok(effect)
}

// ---------------------------------------------------------------------------
// ByronRules — EraRules implementation for the Byron era
// ---------------------------------------------------------------------------

/// Stateless Byron era rule strategy.
///
/// Byron is the simplest era: no scripts, no certificates, no governance,
/// no multi-asset. This implementation delegates to the existing Byron
/// validation functions (`validate_byron_tx`, `apply_byron_block`) and
/// provides trivial (no-op) implementations for features that do not exist
/// in the Byron era.
#[derive(Default, Debug, Clone, Copy)]
pub struct ByronRules;

impl ByronRules {
    pub fn new() -> Self {
        ByronRules
    }
}

impl EraRules for ByronRules {
    /// Byron has no ExUnit budgets or reference scripts — always succeeds.
    fn validate_block_body(
        &self,
        _block: &Block,
        _ctx: &RuleContext,
        _utxo: &UtxoSubState,
    ) -> Result<(), LedgerError> {
        Ok(())
    }

    /// Apply a single valid Byron transaction.
    ///
    /// Byron has no `is_valid` flag — all structurally valid transactions are
    /// considered valid. Delegates to `validate_byron_tx` for validation (in
    /// `ValidateAll` mode) or directly computes UTxO changes (in `ApplyOnly`).
    ///
    /// Returns the [`UtxoDiff`] recording consumed inputs and produced outputs.
    fn apply_valid_tx(
        &self,
        tx: &Transaction,
        mode: BlockValidationMode,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _gov: &mut GovSubState,
        _epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        // Byron fee policy is a network-wide genesis constant (a + ceiling(size*b),
        // b an exact rational). It is NOT the Shelley integer projection carried in
        // `ctx.params.min_fee_a/b`. See `ByronFeePolicy` for the Haskell cross-ref.
        let fee_policy = ByronFeePolicy::canonical();

        // NOTE: Haskell sizes the full `ATxAux` (tx body + witnesses); dugite's
        // `raw_cbor` is the tx body only, so this is a (lenient) lower bound on the
        // true size. Real historical blocks always pay >= the true minimum, so this
        // never falsely rejects; full-ATxAux sizing is tracked for byte-exact parity.
        let tx_size_bytes = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);
        let mut diff = UtxoDiff::new();

        match mode {
            BlockValidationMode::ValidateAll => {
                // Full validation: delegate to validate_byron_tx
                let effect = validate_byron_tx(
                    tx,
                    |input| utxo.utxo_set.lookup(input),
                    fee_policy,
                    tx_size_bytes,
                )
                .map_err(|e| LedgerError::BlockTxValidationFailed {
                    slot: ctx.current_slot,
                    tx_hash: tx.hash.to_hex(),
                    errors: e.to_string(),
                })?;

                // Apply UTxO changes
                for input in &effect.consumed {
                    if let Some(spent_output) = utxo.utxo_set.lookup(input) {
                        diff.record_delete(input.clone(), spent_output);
                    }
                    utxo.utxo_set.remove(input);
                }
                for (input, output) in effect.produced {
                    diff.record_insert(input.clone(), output.clone());
                    utxo.utxo_set.insert(input, output);
                }

                // Accumulate fees
                utxo.epoch_fees.0 = utxo.epoch_fees.0.saturating_add(effect.fee.0);
            }

            BlockValidationMode::ApplyOnly => {
                // Replay mode: trust the block, collect UTxO changes without
                // full validation. If inputs are missing, skip the UTxO update
                // but still count the fee.
                let mut all_inputs_present = true;
                let mut consumed = Vec::with_capacity(tx.body.inputs.len());

                for input in &tx.body.inputs {
                    if let Some(output) = utxo.utxo_set.lookup(input) {
                        consumed.push((input.clone(), output));
                    } else {
                        all_inputs_present = false;
                        break;
                    }
                }

                if all_inputs_present {
                    for (input, output) in &consumed {
                        diff.record_delete(input.clone(), output.clone());
                        utxo.utxo_set.remove(input);
                    }
                    for (idx, output) in tx.body.outputs.iter().enumerate() {
                        let out_input = TransactionInput {
                            transaction_id: tx.hash,
                            index: idx as u32,
                        };
                        diff.record_insert(out_input.clone(), output.clone());
                        utxo.utxo_set.insert(out_input, output.clone());
                    }
                }

                // Always accumulate fees (epoch accounting is independent of UTxO availability)
                utxo.epoch_fees.0 = utxo.epoch_fees.0.saturating_add(tx.body.fee.0);
            }
        }

        Ok(diff)
    }

    /// Byron has no `is_valid` concept — all transactions are structurally valid
    /// or rejected. Calling this for a Byron transaction is a programming error.
    fn apply_invalid_tx(
        &self,
        tx: &Transaction,
        _mode: BlockValidationMode,
        _ctx: &RuleContext,
        _utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        Err(LedgerError::InvalidTransaction(format!(
            "Byron era does not support invalid transactions (is_valid flag). \
             Transaction {} should not reach apply_invalid_tx.",
            tx.hash.to_hex()
        )))
    }

    /// Byron epoch transition is minimal.
    ///
    /// In Byron there is no staking, no governance, no reward distribution,
    /// and no protocol parameter update mechanism. The epoch transition only
    /// needs to advance the epoch counter and reset block production counters.
    ///
    /// Snapshot rotation and reward calculation are deferred to the Shelley
    /// era transition, which will pick up the accumulated state.
    fn process_epoch_transition(
        &self,
        new_epoch: EpochNo,
        _ctx: &RuleContext,
        _utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _gov: &mut GovSubState,
        _epochs: &mut EpochSubState,
        consensus: &mut ConsensusSubState,
    ) -> Result<(), LedgerError> {
        // Reset block production counters for the new epoch.
        // Store the previous epoch's counts for potential Shelley transition use.
        consensus.epoch_blocks_by_pool = Arc::new(std::collections::HashMap::new());
        consensus.epoch_block_count = 0;

        tracing::debug!(
            epoch = new_epoch.0,
            "Byron epoch transition: reset block counters"
        );

        Ok(())
    }

    /// Evolve nonce state after a Byron block header.
    ///
    /// Byron uses OBFT (not VRF), so:
    /// - `lab_nonce` = `block.prev_hash` (prevHashToNonce from Haskell)
    /// - `evolving_nonce` does NOT advance (no VRF output in Byron)
    /// - Block production is tracked per issuer key hash
    fn evolve_nonce(
        &self,
        header: &BlockHeader,
        _ctx: &RuleContext,
        consensus: &mut ConsensusSubState,
    ) {
        // Byron (PBFT) does NOT maintain the TPraos `csLabNonce`. In Haskell the
        // Byron ChainDepState is `PBftState` (no nonce fields), and
        // `translateChainDepStateByronToShelley` initialises `csLabNonce` and
        // `ticknStatePrevHashNonce` to `NeutralNonce`. So `lab_nonce` must stay
        // NeutralNonce (ZERO) through Byron — the FIRST Shelley block is what
        // first sets it (see common.rs).
        //
        // Setting it to the Byron block's prev-hash here (as we used to) poisoned
        // the first Shelley epoch-nonce TICKN: the 207->208 transition copied this
        // Byron hash into `last_epoch_block_nonce`, so η0(209) was computed as
        // `candidate(208) ⭒ byron_hash` instead of `candidate(208) ⭒ NeutralNonce
        // = candidate(208)`, breaking VRF on the first block of epoch 209.
        consensus.lab_nonce = dugite_primitives::hash::Hash32::ZERO;

        // Track block production by issuer key hash
        if !header.issuer_vkey.is_empty() {
            let pool_id = blake2b_224(&header.issuer_vkey);
            *Arc::make_mut(&mut consensus.epoch_blocks_by_pool)
                .entry(pool_id)
                .or_insert(0) += 1;
        }
        consensus.epoch_block_count += 1;
    }

    /// Byron minimum fee: `a + ceiling(size * b)` (canonical genesis policy).
    ///
    /// Delegates to [`ByronFeePolicy::min_fee`]. `ctx` is unused because the
    /// Byron fee policy is a network-wide genesis constant, not the Shelley
    /// integer projection carried in `ctx.params`.
    fn min_fee(&self, tx: &Transaction, _ctx: &RuleContext, _utxo: &UtxoSubState) -> u64 {
        let policy = ByronFeePolicy::canonical();
        let tx_size = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);
        policy.min_fee(tx_size).unwrap_or(u64::MAX)
    }

    /// Byron is the first era — no hard fork transformation needed.
    fn on_era_transition(
        &self,
        _from_era: Era,
        _ctx: &RuleContext,
        _utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _gov: &mut GovSubState,
        _consensus: &mut ConsensusSubState,
        _epochs: &mut EpochSubState,
    ) -> Result<(), LedgerError> {
        Ok(())
    }

    /// Compute required VKey witnesses for a Byron transaction.
    ///
    /// In Byron, only spending input keys are required (no scripts, no certs,
    /// no withdrawals). For each input, we look up the UTxO output address
    /// and, if it is a Shelley-type address with a verification key payment
    /// credential, extract the pubkey hash.
    ///
    /// Byron addresses (`Address::Byron`) use bootstrap witnesses which are
    /// verified separately — they don't contribute to the VKey witness set.
    fn required_witnesses(
        &self,
        tx: &Transaction,
        _ctx: &RuleContext,
        utxo: &UtxoSubState,
        _certs: &CertSubState,
        _gov: &GovSubState,
    ) -> HashSet<Hash28> {
        let mut witnesses = HashSet::new();

        for input in &tx.body.inputs {
            if let Some(output) = utxo.utxo_set.lookup(input) {
                // Extract the payment credential's key hash, if present.
                // Byron addresses return None from payment_credential() —
                // they use bootstrap witnesses verified by a different mechanism.
                if let Some(dugite_primitives::credentials::Credential::VerificationKey(hash)) =
                    output.address.payment_credential()
                {
                    witnesses.insert(*hash);
                }
            }
        }

        witnesses
    }
}

// Keep the old name as an alias for backward compatibility during migration.
pub type ByronLedger = ByronRules;

// ============================================================================
// Byron delegation + update-proposal state (#1084)
// ============================================================================
//
// Models `Cardano.Chain.Delegation.Interface::State` (`DI.State`) and
// `Cardano.Chain.Update.Validation.Interface::State` (`UPI.State`) — the two
// `ChainValidationState` fields Byron carries beyond the UTxO set already
// modelled elsewhere in this file. Grounded in `cardano-ledger-byron`
// 1.2.0.0; see
// `docs/superpowers/specs/2026-08-20-byron-delegation-update-state-design.md`
// for the full derivation.
//
// Deliberately NOT threaded through the `EraRules` trait: `genesis_delegates`
// / `future_gen_delegs` (Shelley's analogous top-level `LedgerState` fields)
// are handled the same way — dedicated functions called directly from
// `state/apply.rs`'s Byron branch — rather than widening every era's
// `process_epoch_transition` signature (Shelley/Alonzo/Babbage/Conway/
// Dijkstra all implement it) for a field only Byron ever touches.

/// `Cardano.Chain.Delegation.Scheduling::ScheduledDelegation` — one
/// heavyweight delegation certificate awaiting activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledDelegation {
    pub slot: u64,
    pub delegator: Hash28,
    pub delegate: Hash28,
}

/// `Cardano.Chain.Delegation.Interface::State` — scheduling + activation
/// halves combined (Scheduling.hs + Activation.hs).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByronDelegationState {
    /// Certificates scheduled but not yet activated, in append (`Seq`) order.
    pub scheduled: Vec<ScheduledDelegation>,
    /// `(epoch, issuer)` pairs a certificate has already been scheduled for —
    /// enforces "one certificate per (epoch, issuer)".
    pub key_epoch_delegations: BTreeSet<(u64, Hash28)>,
    /// The active delegation `Bimap`, forward half: delegator -> delegate.
    pub delegation_map: BTreeMap<Hash28, Hash28>,
    /// The active delegation `Bimap`, reverse half: delegate -> delegator.
    /// Both directions are load-bearing: `lookupR` resolves votes and
    /// endorsements to their genesis key, `notMemberR` gates activation.
    pub delegation_map_rev: BTreeMap<Hash28, Hash28>,
    /// The activation slot most recently accepted for each delegator —
    /// enforces `prevDelegationSlot < slot`.
    pub delegation_slots: BTreeMap<Hash28, u64>,
}

/// One entry of `UPI.State.candidateProtocolUpdates` — a version that has
/// been endorsed by enough genesis keys, confirmed, and stable, and is
/// awaiting adoption at the next epoch boundary that clears its own
/// stability window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByronCandidate {
    /// The slot at which this candidate was CREATED (`cpuSlot`) — adoption
    /// requires `cpuSlot + 4k <= epochFirstSlot`.
    pub slot: u64,
    pub protocol_version: (u16, u16, u8),
    pub protocol_parameters: ByronProtocolParameters,
}

/// Byron's full ADOPTED protocol-parameter record — `Update.ProtocolParameters`.
/// All 14 fields; see `ByronParamsUpdate` (dugite-primitives) for the sparse
/// wire counterpart this overlays onto.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByronProtocolParameters {
    pub script_version: u16,
    pub slot_duration: u64,
    pub max_block_size: u64,
    pub max_header_size: u64,
    pub max_tx_size: u64,
    pub max_proposal_size: u64,
    pub mpc_thd: u64,
    pub heavy_del_thd: u64,
    pub update_vote_thd: u64,
    pub update_proposal_thd: u64,
    pub update_implicit: u64,
    /// `SoftforkRule { srInitThd, srMinThd, srThdDecrement }` — each a
    /// `LovelacePortion` numerator over the implicit 1e15 denominator.
    pub soft_fork_rule: (u64, u64, u64),
    /// `(summand_lovelace, (mult_num, mult_den))` — the same exact-rational
    /// shape `ByronTxFeePolicy::to_exact` produces from genesis JSON.
    pub tx_fee_policy: (u64, (u64, u64)),
    pub unlock_stake_epoch: u64,
}

/// `Cardano.Chain.Update.Validation.Interface::State` (`UPI.State`,
/// Interface.hs:108) — eleven fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByronUpdateState {
    /// Vestigial upstream: `initialState` is the ONLY writer in the whole
    /// package (`registerEpoch`'s record update does not name this field).
    /// Modelled as what it is — a state field with no writer after seeding —
    /// rather than a hardcoded dump constant, so a future upstream writer
    /// has somewhere to land. See the design doc §2.3.
    pub current_epoch: u64,
    pub adopted_protocol_version: (u16, u16, u8),
    pub adopted_protocol_parameters: ByronProtocolParameters,
    /// Newest-first (FADS order) — `tryBumpVersion` scans from the front.
    pub candidate_protocol_updates: Vec<ByronCandidate>,
    /// `name -> (version, slot)`. Modelled because the state MACHINE routes
    /// through it (the null-update check), not because anything downstream
    /// reads it — see the design doc §4.
    pub app_versions: BTreeMap<String, (u32, u64)>,
    /// `UpId -> (protocol_version, FULL overlaid parameters)`. The overlay
    /// (`PPU.apply`) happens at REGISTRATION time, not adoption time.
    pub registered_protocol_update_proposals:
        BTreeMap<Hash32, ((u16, u16, u8), ByronProtocolParameters)>,
    pub registered_software_update_proposals: BTreeMap<Hash32, (String, u32)>,
    /// `UpId -> confirming slot`.
    pub confirmed_proposals: BTreeMap<Hash32, u64>,
    /// `UpId -> {genesis keys that voted}`.
    pub proposal_votes: BTreeMap<Hash32, BTreeSet<Hash28>>,
    /// `(endorsed_version, genesis_key)` pairs.
    pub registered_endorsements: BTreeSet<((u16, u16, u8), Hash28)>,
    /// `UpId -> registration slot` — the TTL clock for `prune_stale_proposals`.
    pub proposal_registration_slot: BTreeMap<Hash32, u64>,
}

/// The two Byron `ChainValidationState` fields beyond the UTxO set, plus the
/// genesis-derived constant the rules need at apply time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByronSubState {
    pub delegation: ByronDelegationState,
    pub update: ByronUpdateState,
    /// `bootStakeholders`' key set — the genesis-key authority both the
    /// delegation and update-proposal state machines gate on.
    pub allowed_delegators: BTreeSet<Hash28>,
}

impl Default for ByronSubState {
    /// A `ByronSubState` for a network with NO Byron era (Shelley-from-genesis
    /// devnets/testnets) — never mutated, never dumped (the dump only emits
    /// Byron fields while `ledger.era == Era::Byron`).
    fn default() -> Self {
        ByronSubState {
            delegation: ByronDelegationState::default(),
            update: ByronUpdateState {
                current_epoch: 0,
                adopted_protocol_version: (0, 0, 0),
                adopted_protocol_parameters: ByronProtocolParameters {
                    script_version: 0,
                    slot_duration: 20_000,
                    max_block_size: 2_000_000,
                    max_header_size: 2_000_000,
                    max_tx_size: 4_096,
                    max_proposal_size: 700,
                    mpc_thd: 0,
                    heavy_del_thd: 0,
                    update_vote_thd: 0,
                    update_proposal_thd: 0,
                    update_implicit: 10_000,
                    soft_fork_rule: (0, 0, 0),
                    tx_fee_policy: (0, (0, 1)),
                    unlock_stake_epoch: u64::MAX,
                },
                candidate_protocol_updates: Vec::new(),
                app_versions: BTreeMap::new(),
                registered_protocol_update_proposals: BTreeMap::new(),
                registered_software_update_proposals: BTreeMap::new(),
                confirmed_proposals: BTreeMap::new(),
                proposal_votes: BTreeMap::new(),
                registered_endorsements: BTreeSet::new(),
                proposal_registration_slot: BTreeMap::new(),
            },
            allowed_delegators: BTreeSet::new(),
        }
    }
}

/// Errors from the Byron delegation-certificate scheduling rules
/// (`Delegation.Validation.Scheduling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ByronDelegationError {
    #[error("delegation cert issuer is not a genesis (bootStakeholders) key")]
    NotGenesisKey,
    #[error("delegation cert epoch is not the current or next epoch")]
    WrongEpoch,
    #[error("issuer already has a delegation certificate for this epoch")]
    AlreadyDelegated,
    #[error("issuer already has a scheduled delegation activating at this slot")]
    DuplicateActivationSlot,
    /// `Certificate.hs::isValid` (issue #1092, design doc §3.2) — the FIFTH
    /// and last check `scheduleCertificate` runs, after the four state
    /// checks above (order is observable: a doubly-invalid cert reports
    /// whichever of the two fires first, and this is always last).
    #[error("delegation certificate signature does not verify")]
    InvalidSignature,
}

/// Errors from the Byron update-proposal-system rules (registration + voting).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ByronUpdateError {
    #[error("proposer/voter key does not resolve to a registered genesis delegate")]
    NotGenesisDelegate,
    #[error("proposal is a null update (no protocol-version, parameter, or software change)")]
    NullUpdate,
    #[error("a proposal for this protocol version is already registered")]
    DuplicateVersion,
    #[error("proposed protocol version does not follow the adopted version (pvCanFollow)")]
    VersionCannotFollow,
    #[error("proposal fails canUpdate (size/maxBlockSize/maxTxSize/scriptVersion bounds)")]
    CannotUpdate,
    #[error(
        "proposal sets txFeePolicy, which this implementation captures raw and cannot apply \
         (unreachable on every real network — mainnet/preprod/preview never change it on-chain)"
    )]
    UnsupportedTxFeePolicyOverride,
    #[error("proposal id is not registered")]
    ProposalNotRegistered,
    #[error("this genesis key has already voted on this proposal")]
    VoteAlreadyCast,
    /// `Registration.hs::registerProposal` / `Voting.hs::registerVote`
    /// (issue #1092, design doc §4.1/§4.2) — the signature check, which
    /// upstream runs AFTER the state-rule checks above (registered/genesis
    /// key resolution etc.), matching this variant's position as the last
    /// arm checked by both `register_proposal` and `register_vote`.
    #[error("proposal/vote signature does not verify")]
    InvalidSignature,
}

/// `hashKey = blake2b_224 . sha3_256 . cbor(vk)` (`Common/KeyHash.hs:53`) —
/// the bare-pubkey variant, distinct from `dugite-primitives`'
/// `addr_root_hash` (which hashes an ADDRESS SPEC, not a raw key). CBOR
/// encodes `vk` as a definite-length bytestring.
///
/// `pub`: the node layer needs it too, to turn a genesis `heavyDelegation`
/// entry's base64 `delegatePk` into the `Hash28` [`seed_byron_genesis`]
/// expects (the genesis JSON's own map KEY is already the issuer's KeyHash,
/// but the delegate is given as a raw pubkey).
pub fn byron_key_hash(vk: &[u8]) -> Hash28 {
    let mut cbor = Vec::with_capacity(vk.len() + 3);
    if vk.len() <= 23 {
        cbor.push(0x40 | vk.len() as u8);
    } else if vk.len() <= 0xff {
        cbor.push(0x58);
        cbor.push(vk.len() as u8);
    } else {
        cbor.push(0x59);
        cbor.extend_from_slice(&(vk.len() as u16).to_be_bytes());
    }
    cbor.extend_from_slice(vk);
    let sha3_digest: [u8; 32] = sha3::Sha3_256::digest(&cbor).into();
    blake2b_224(&sha3_digest)
}

// ============================================================================
// Signature verification (issue #1092)
// ============================================================================
//
// Message construction for the three signed surfaces #1084 decodes but does
// not verify: heavyweight delegation certificates, update proposals, and
// update votes, plus the block signature itself (verified from `apply.rs`,
// see `verify_block_signature` below). Grounded in
// `docs/superpowers/specs/2026-08-21-byron-signature-verification-design.md`.
// The crypto PRIMITIVE (`verify_xsig`, donna-exact — NOT dalek) and the four
// sign-tag builders live in `dugite_crypto::byron`; everything here is
// message assembly fed by the raw spans `era_byron.rs`'s decoder captures.

/// CBOR bytestring header for a payload of `len` bytes — major type 2,
/// minimal (canonical) length encoding. The same three-tier scheme as
/// [`byron_key_hash`]'s inline CBOR-wrap, factored out here because the
/// certificate signature message (design doc §3.2) wraps its payload as a
/// CBOR bytestring TOKEN (`serialize'` of a `ByteString`), not as a bare
/// concatenation.
fn cbor_bstr_header(len: usize) -> Vec<u8> {
    if len <= 23 {
        vec![0x40 | len as u8]
    } else if len <= 0xff {
        vec![0x58, len as u8]
    } else {
        let mut h = vec![0x59];
        h.extend_from_slice(&(len as u16).to_be_bytes());
        h
    }
}

/// Byron's `ProtocolMagicId` (a `Word32`), CBOR-encoded as a canonical
/// unsigned integer — `Cardano.Crypto.Signing.Tag::signTagRaw`'s `network`
/// argument, `serialize' byronProtVer (unProtocolMagicId pm)` (design doc
/// §1.2). mainnet 764824073 -> `1A 2D 96 4A 09`; preprod 1 -> `01`; preview
/// 2 -> `02`. `as u32` matches the AVVM redeem-address network tag already
/// computed this way in `dugite-node/src/genesis.rs::avvm_to_address` — the
/// same quantity, same encoding, proven correct there.
pub fn network_magic_cbor(magic: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    minicbor::encode(magic as u32, &mut buf).expect("encoding a u32 into a Vec cannot fail");
    buf
}

/// The exact bytes signed for a heavyweight delegation certificate
/// (`Certificate.hs::isValid`, design doc §3.2): `SignCertificate‖magic`
/// followed by a CBOR-bytestring-wrapped `"00"‖delegate_vk‖epoch_bytes`.
/// `epoch_bytes` is the epoch's raw wire annotation for an on-chain cert, or
/// a fresh canonical CBOR encoding for a genesis cert (`DI.initialState`'s
/// re-annotation, §3.3) — the caller decides which. Factored out of
/// [`verify_dlg_cert_signature`] so the test module can build the identical
/// message to SIGN, rather than maintaining an independent copy that could
/// silently drift from what verification actually checks.
fn build_dlg_cert_message(delegate_vk: &[u8], epoch_bytes: &[u8], magic_cbor: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + delegate_vk.len() + epoch_bytes.len());
    payload.extend_from_slice(b"00"); // ASCII 0x30 0x30 — see design doc §1.2 Trap 2
    payload.extend_from_slice(delegate_vk);
    payload.extend_from_slice(epoch_bytes);

    let mut msg = dugite_crypto::byron::sign_tag_certificate(magic_cbor);
    msg.extend_from_slice(&cbor_bstr_header(payload.len()));
    msg.extend_from_slice(&payload);
    msg
}

/// Verify a heavyweight delegation certificate's signature, verified with
/// the certificate's own `issuer_vk`. See [`build_dlg_cert_message`] for the
/// message.
fn verify_dlg_cert_signature(
    issuer_vk: &[u8],
    delegate_vk: &[u8],
    epoch_bytes: &[u8],
    signature: &[u8],
    magic_cbor: &[u8],
) -> bool {
    let (Ok(issuer_xpub), Ok(sig)) = (
        <[u8; 64]>::try_from(issuer_vk),
        <[u8; 64]>::try_from(signature),
    ) else {
        return false;
    };
    let msg = build_dlg_cert_message(delegate_vk, epoch_bytes, magic_cbor);
    dugite_crypto::byron::verify_xsig(&issuer_xpub, &msg, &sig)
}

/// The exact bytes signed for an update proposal (`Registration.hs:328`,
/// `Proposal.hs::recoverProposalSignedBytes`, design doc §4.1):
/// `SignUSProposal‖magic‖0x85‖body_span`. `0x85` is a synthetic bare
/// array(5) header byte — an "implementation artifact" per upstream's own
/// comment, not a real wire header. Factored out for the same reason as
/// [`build_dlg_cert_message`].
fn build_proposal_message(body_span: &[u8], magic_cbor: &[u8]) -> Vec<u8> {
    let mut msg = dugite_crypto::byron::sign_tag_us_proposal(magic_cbor);
    msg.push(0x85);
    msg.extend_from_slice(body_span);
    msg
}

/// Verify an update proposal's signature, verified with the proposal's own
/// `proposer_vk`. See [`build_proposal_message`] for the message.
fn verify_proposal_signature(
    proposer_vk: &[u8],
    body_span: &[u8],
    signature: &[u8],
    magic_cbor: &[u8],
) -> bool {
    let (Ok(xpub), Ok(sig)) = (
        <[u8; 64]>::try_from(proposer_vk),
        <[u8; 64]>::try_from(signature),
    ) else {
        return false;
    };
    let msg = build_proposal_message(body_span, magic_cbor);
    dugite_crypto::byron::verify_xsig(&xpub, &msg, &sig)
}

/// The exact bytes signed for an update vote (`Voting.hs:161`,
/// `Vote.hs::recoverSignedBytes`, design doc §4.2):
/// `SignUSVote‖magic‖0x82‖proposal_id_raw‖0xF5`. The vote signs the
/// synthetic pair `(proposalId, True)` — `0x82` is array(2), `0xF5` is
/// canonical CBOR `true`; the wire `vote: bool` element itself is never
/// part of the message (it is always `True` on the wire and is
/// decoded-and-discarded, matching upstream — see `read_byron_upvote`'s
/// doc). Factored out for the same reason as [`build_dlg_cert_message`].
fn build_vote_message(proposal_id_raw: &[u8], magic_cbor: &[u8]) -> Vec<u8> {
    let mut msg = dugite_crypto::byron::sign_tag_us_vote(magic_cbor);
    msg.push(0x82);
    msg.extend_from_slice(proposal_id_raw);
    msg.push(0xF5);
    msg
}

/// Verify an update vote's signature, verified with the vote's own
/// `voter_vk`. See [`build_vote_message`] for the message.
fn verify_vote_signature(
    voter_vk: &[u8],
    proposal_id_raw: &[u8],
    signature: &[u8],
    magic_cbor: &[u8],
) -> bool {
    let (Ok(xpub), Ok(sig)) = (
        <[u8; 64]>::try_from(voter_vk),
        <[u8; 64]>::try_from(signature),
    ) else {
        return false;
    };
    let msg = build_vote_message(proposal_id_raw, magic_cbor);
    dugite_crypto::byron::verify_xsig(&xpub, &msg, &sig)
}

/// Verify a Byron main block's own signature and delegate-membership
/// (`Ouroboros.Consensus.Protocol.PBFT::updateChainDepState`'s steps 1 and
/// 3 — design doc §2.1). Steps 2 (slot monotonicity) and 4 (the signing
/// window threshold) are tier B (design doc §8) and are NOT implemented
/// here.
///
/// **`ValidateAll`-mode only.** Block signatures are first-validation-only
/// upstream — `reupdateChainDepState` skips re-verifying them on replay
/// (design doc §2.1's "Re-application skips the signature") — so this must
/// never be called from an `ApplyOnly` path; that is parity with upstream,
/// not a shortcut.
///
/// `delegation_map_rev` MUST be the delegation map AS OF the tick, i.e.
/// captured BEFORE this block's own `dlgPayload` is applied — the same
/// discipline [`apply_update_payload`]'s doc explains for the identical
/// reason (`updateEnv`'s resolver reads the *original* `delegationState`
/// binding).
///
/// **Deliberately does NOT re-verify `block_sig`'s EMBEDDED delegation
/// certificate's own signature.** Upstream does not either (design doc
/// §2.1 step 3's note: *"the embedded certificate's own signature is never
/// verified on this path — its only roles are carrying `delegateVK` and
/// feeding the sign tag"*), matching this function exactly: it reads
/// `aux.delegate_pubkey` and `aux.issuer_pubkey` (both sourced from that
/// embedded cert, decoded but not re-checked here) and relies on the
/// delegate-membership lookup below — i.e. on that certificate having been
/// signature-verified WHEN IT WAS FIRST ADMITTED, by
/// `schedule_delegation_cert`'s check 5, wherever it entered the ledger
/// (an on-chain `dlgPayload` entry, or a genesis `heavyDelegation` entry).
/// Adding a second verification here would be stricter than upstream, not
/// merely redundant — design doc §10c recommends matching upstream exactly.
pub fn verify_block_signature(
    aux: &ByronBlockAux,
    network_magic: u64,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
    current_slot: u64,
) -> Result<(), LedgerError> {
    let bad_shape = || LedgerError::ByronSignatureInvalid {
        slot: current_slot,
        kind: "block (malformed key or signature length)".to_string(),
    };
    let genesis_xpub =
        <[u8; 64]>::try_from(aux.issuer_pubkey.as_slice()).map_err(|_| bad_shape())?;
    let delegate_xpub =
        <[u8; 64]>::try_from(aux.delegate_pubkey.as_slice()).map_err(|_| bad_shape())?;
    let sig = <[u8; 64]>::try_from(aux.block_signature.as_slice()).map_err(|_| bad_shape())?;

    let magic_cbor = network_magic_cbor(network_magic);
    // Tagged with the header's CLAIMED genesis key (`issuer_pubkey`) — see
    // the design doc §2.1 point 1: this is otherwise-unauthenticated input,
    // by design (the delegate-membership check below is what authenticates
    // the delegate key that ACTUALLY verifies the signature).
    let mut msg = dugite_crypto::byron::sign_tag_block(&genesis_xpub, &magic_cbor);
    msg.extend_from_slice(&aux.block_signed_bytes);

    if !dugite_crypto::byron::verify_xsig(&delegate_xpub, &msg, &sig) {
        return Err(LedgerError::ByronSignatureInvalid {
            slot: current_slot,
            kind: "block".to_string(),
        });
    }

    // Step 3: `Bimap.lookupR (hashVerKey pbftIssuer) dms` — the delegate
    // must be SOMEONE's delegate in the ledger's activation map. Note what
    // this does NOT check: the resolved genesis key is never compared
    // against `issuer_pubkey` (design doc §2.1 point 3).
    let delegate_hash = byron_key_hash(&aux.delegate_pubkey);
    if resolve_genesis_authority(&delegate_hash, delegation_map_rev).is_none() {
        return Err(LedgerError::ByronSignatureInvalid {
            slot: current_slot,
            kind: "block (delegate is not a registered genesis delegate)".to_string(),
        });
    }

    Ok(())
}

/// `floor(srMinThd/1e15 * numGenKeys)` (`ProtocolParameters.hs:217`,
/// `upAdptThd`) — the confirmation/endorsement threshold. Integer floor of
/// an exact rational; no floats. On mainnet/preprod/preview,
/// `srMinThd = 0.6e15` and `numGenKeys = 7`, giving `floor(4.2) = 4`.
fn up_adpt_thd(num_gen_keys: u64, pp: &ByronProtocolParameters) -> u64 {
    let min_thd = pp.soft_fork_rule.1 as u128;
    (min_thd * num_gen_keys as u128 / 1_000_000_000_000_000u128) as u64
}

/// `PPU.apply` — overlay a sparse `ByronParamsUpdate` onto a full adopted
/// record. The caller must have already rejected a `Some` `tx_fee_policy`
/// (see [`ByronUpdateError::UnsupportedTxFeePolicyOverride`]); this function
/// therefore always carries `base.tx_fee_policy` through unchanged.
fn apply_params_update(
    base: &ByronProtocolParameters,
    update: &ByronParamsUpdate,
) -> ByronProtocolParameters {
    ByronProtocolParameters {
        script_version: update.script_version.unwrap_or(base.script_version),
        slot_duration: update.slot_duration.unwrap_or(base.slot_duration),
        max_block_size: update.max_block_size.unwrap_or(base.max_block_size),
        max_header_size: update.max_header_size.unwrap_or(base.max_header_size),
        max_tx_size: update.max_tx_size.unwrap_or(base.max_tx_size),
        max_proposal_size: update.max_proposal_size.unwrap_or(base.max_proposal_size),
        mpc_thd: update.mpc_thd.unwrap_or(base.mpc_thd),
        heavy_del_thd: update.heavy_del_thd.unwrap_or(base.heavy_del_thd),
        update_vote_thd: update.update_vote_thd.unwrap_or(base.update_vote_thd),
        update_proposal_thd: update
            .update_proposal_thd
            .unwrap_or(base.update_proposal_thd),
        update_implicit: update.update_implicit.unwrap_or(base.update_implicit),
        soft_fork_rule: update.soft_fork_rule.unwrap_or(base.soft_fork_rule),
        tx_fee_policy: base.tx_fee_policy,
        unlock_stake_epoch: update.unlock_stake_epoch.unwrap_or(base.unlock_stake_epoch),
    }
}

/// `pvCanFollow`: same major ⇒ minor+1; major+1 ⇒ minor 0.
fn pv_can_follow(new: (u16, u16, u8), old: (u16, u16, u8)) -> bool {
    (new.0 == old.0 && new.1 == old.1 + 1) || (new.0 == old.0 + 1 && new.1 == 0)
}

/// `canUpdate` (`Registration.hs`): proposal size bound, block/tx size
/// growth bounds, script-version step bound.
fn can_update(
    base: &ByronProtocolParameters,
    applied: &ByronProtocolParameters,
    proposal_size: u64,
) -> bool {
    proposal_size <= base.max_proposal_size
        && applied.max_block_size <= base.max_block_size.saturating_mul(2)
        && applied.max_tx_size < applied.max_block_size
        && applied.script_version.abs_diff(base.script_version) <= 1
}

/// `Delegation.Scheduling::scheduleCertificate` (Scheduling.hs:176+).
///
/// `k` is the activation delay divisor (`activation_slot = current_slot +
/// 2*k`); genesis seeding passes `k = 0` for immediate activation, on-chain
/// certificates pass the real security parameter.
#[allow(clippy::too_many_arguments)]
fn schedule_delegation_cert(
    state: &mut ByronDelegationState,
    allowed_delegators: &BTreeSet<Hash28>,
    current_slot: u64,
    current_epoch: u64,
    k: u64,
    cert_epoch: u64,
    issuer: Hash28,
    delegate: Hash28,
    issuer_vk: &[u8],
    delegate_vk: &[u8],
    epoch_bytes: &[u8],
    signature: &[u8],
    magic_cbor: &[u8],
) -> Result<(), ByronDelegationError> {
    if !allowed_delegators.contains(&issuer) {
        return Err(ByronDelegationError::NotGenesisKey);
    }
    if cert_epoch != current_epoch && cert_epoch != current_epoch.saturating_add(1) {
        return Err(ByronDelegationError::WrongEpoch);
    }
    if state.key_epoch_delegations.contains(&(cert_epoch, issuer)) {
        return Err(ByronDelegationError::AlreadyDelegated);
    }
    let activation_slot = current_slot.saturating_add(2 * k);
    if state
        .scheduled
        .iter()
        .any(|s| s.delegator == issuer && s.slot == activation_slot)
    {
        return Err(ByronDelegationError::DuplicateActivationSlot);
    }
    // Check 5, last (`Certificate.hs::isValid`, design doc §3.2/§3.4).
    if !verify_dlg_cert_signature(issuer_vk, delegate_vk, epoch_bytes, signature, magic_cbor) {
        return Err(ByronDelegationError::InvalidSignature);
    }
    state.scheduled.push(ScheduledDelegation {
        slot: activation_slot,
        delegator: issuer,
        delegate,
    });
    state.key_epoch_delegations.insert((cert_epoch, issuer));
    Ok(())
}

/// `Delegation.Activation::activateDelegation`, folded over every scheduled
/// entry with `sdSlot <= currentSlot` (Haskell folds the whole `Seq` on
/// every tick; entries not yet due are simply left in place — the caller's
/// prune step removes them once they age out, not this function).
fn activate_delegations(state: &mut ByronDelegationState, current_slot: u64) {
    let due: Vec<ScheduledDelegation> = state
        .scheduled
        .iter()
        .filter(|s| s.slot <= current_slot)
        .cloned()
        .collect();
    for sd in due {
        // notMemberR: the DELEGATE must not currently be anyone's delegate.
        let delegate_free = !state.delegation_map_rev.contains_key(&sd.delegate);
        let prev_slot = state.delegation_slots.get(&sd.delegator).copied();
        let slot_ok = match prev_slot {
            None => true,
            Some(prev) => prev < sd.slot || sd.slot == 0,
        };
        if !(delegate_free && slot_ok) {
            continue;
        }
        // Bimap insert: replaces the delegator's previous pair.
        if let Some(old_delegate) = state.delegation_map.get(&sd.delegator).copied() {
            if old_delegate != sd.delegate {
                state.delegation_map_rev.remove(&old_delegate);
            }
        }
        state.delegation_map.insert(sd.delegator, sd.delegate);
        state.delegation_map_rev.insert(sd.delegate, sd.delegator);
        state.delegation_slots.insert(sd.delegator, sd.slot);
    }
}

/// `tickDelegation = prune . activateDelegations currentSlot`
/// (Interface.hs:181-217). Runs at EVERY consensus tick (every Byron block,
/// EBBs included) and as the tail of `updateDelegation` after scheduling a
/// block's certificates — idempotent, so calling it twice on the same block
/// is harmless.
pub fn tick_delegation(state: &mut ByronDelegationState, current_slot: u64, current_epoch: u64) {
    activate_delegations(state, current_slot);
    state.scheduled.retain(|sd| sd.slot > current_slot);
    state
        .key_epoch_delegations
        .retain(|(epoch, _)| *epoch >= current_epoch);
}

/// One `heavyDelegation` entry from the Byron genesis file, with the raw
/// key/signature material [`seed_byron_genesis`] needs to verify it
/// (`Certificate.hs::isValid`, design doc §3.3) alongside the resolved
/// `Hash28`s [`schedule_delegation_cert`]'s state rules need.
#[derive(Debug, Clone)]
pub struct GenesisHeavyDelegationCert {
    pub issuer: Hash28,
    pub delegate: Hash28,
    /// The issuer's (genesis key's) 64-byte extended verification key —
    /// the certificate's VERIFYING key.
    pub issuer_vk: Vec<u8>,
    pub delegate_vk: Vec<u8>,
    pub signature: Vec<u8>,
    /// `omega` — the certificate's target epoch. Always 0 on every real
    /// genesis file (mainnet/preprod/preview).
    pub omega: u64,
}

/// `DI.initialState` + `UPI.initialState` (Interface.hs:94-137, :108+):
/// start the delegation map from the IDENTITY (every genesis key delegates
/// to itself), then apply the genesis `heavyDelegation` certificates through
/// the REAL schedule/activate path with `k = 0` (immediate activation) — a
/// shortcut that just inserted pairs would skip the `notMemberR` rule and
/// drift on any genesis where a delegate collides.
///
/// `network_magic` is threaded through to the signature check
/// (`DI.initialState` routes genesis certificates through the SAME
/// `updateDelegation` path real on-chain certificates use — design doc
/// §3.3 — so upstream signature-verifies them at every node start, and this
/// does too).
pub fn seed_byron_genesis(
    allowed_delegators: BTreeSet<Hash28>,
    heavy_delegation: &[GenesisHeavyDelegationCert],
    adopted_protocol_parameters: ByronProtocolParameters,
    network_magic: u64,
) -> ByronSubState {
    let mut delegation = ByronDelegationState::default();
    for key in &allowed_delegators {
        delegation.delegation_map.insert(*key, *key);
        delegation.delegation_map_rev.insert(*key, *key);
        delegation.delegation_slots.insert(*key, 0);
    }
    let magic_cbor = network_magic_cbor(network_magic);
    for cert in heavy_delegation {
        // `omega`'s FRESH canonical CBOR encoding — `DI.initialState`'s
        // `annotateCertificate` re-annotation (design doc §3.3). Genesis is
        // a JSON file, not CBOR, so there is no wire span to capture here
        // the way `read_byron_dlg_cert` captures `epoch_raw` for an
        // on-chain certificate; a canonical re-encode is exactly what
        // upstream itself does at this call site.
        let mut epoch_bytes = Vec::new();
        minicbor::encode(cert.omega, &mut epoch_bytes)
            .expect("encoding a u64 into a Vec cannot fail");

        // Genesis certificates are trusted (they define the genesis itself);
        // a scheduling failure here is a genesis-file defect, logged rather
        // than fatal to node startup — the identity mapping is still a valid
        // (if less accurate) starting point.
        if let Err(e) = schedule_delegation_cert(
            &mut delegation,
            &allowed_delegators,
            0,
            0,
            0,
            cert.omega,
            cert.issuer,
            cert.delegate,
            &cert.issuer_vk,
            &cert.delegate_vk,
            &epoch_bytes,
            &cert.signature,
            &magic_cbor,
        ) {
            tracing::warn!(
                issuer = %cert.issuer.to_hex(),
                delegate = %cert.delegate.to_hex(),
                error = %e,
                "Byron genesis heavyDelegation certificate rejected by the scheduling rules"
            );
        }
    }
    tick_delegation(&mut delegation, 0, 0);

    let update = ByronUpdateState {
        current_epoch: 0,
        adopted_protocol_version: (0, 0, 0),
        adopted_protocol_parameters,
        candidate_protocol_updates: Vec::new(),
        app_versions: BTreeMap::new(),
        registered_protocol_update_proposals: BTreeMap::new(),
        registered_software_update_proposals: BTreeMap::new(),
        confirmed_proposals: BTreeMap::new(),
        proposal_votes: BTreeMap::new(),
        registered_endorsements: BTreeSet::new(),
        proposal_registration_slot: BTreeMap::new(),
    };

    ByronSubState {
        delegation,
        update,
        allowed_delegators,
    }
}

/// Whether `key_hash` currently carries genesis-key standing to act in the
/// update-proposal system — i.e. whether it is CURRENTLY the active delegate
/// of some genesis key, per the delegation bimap's reverse map.
///
/// **Independently verified against `cardano-ledger-byron-1.2.0.0`'s
/// `Registration.hs`/`Voting.hs`/`Endorsement.hs` (pinned tarball).** All
/// three rules are PURE bimap reverse-map lookups over `Delegation.Map =
/// Bimap KeyHash KeyHash` (forward: genesis key -> delegate key):
///
/// - `Registration.hs:338`: `Delegation.memberR proposerId delegationMap`
/// - `Voting.hs:184`: `Delegation.lookupR voter delegationMap`
/// - `Endorsement.hs:210`: `Delegation.lookupR vk delegationMap`
///
/// `memberR` and `lookupR` are the SAME query, Bool vs `Maybe` forms of it:
/// the `bimap-0.5.0` package (`Data/Bimap.hs:177-178,368-373`, the exact
/// version `cardano-ledger-byron` pins) defines
/// `memberR y (MkBimap _ right) = M.member y right` and
/// `lookupR y (MkBimap _ right) = ... M.lookup y right` — both query the
/// identical `right :: Map KeyHash KeyHash` field with the identical key, so
/// `memberR y m == isJust (lookupR y m)` by construction. There is no
/// separate "is this key a direct genesis-key member" rule anywhere in
/// upstream; `dugite`'s `delegation_map_rev` IS that `right` map (keyed
/// delegate -> genesis key, `eras/byron.rs`'s `activate_delegations`), so a
/// single `.get()` implements every one of the three call sites above.
///
/// An UNDELEGATED genesis key still resolves via this alone: `DI.initialState`
/// (Interface.hs — the delegation-state one, not the update one) seeds the
/// bimap with `zip allowedDelegators allowedDelegators`, i.e. every genesis
/// key mapped to itself, and `seed_byron_genesis` mirrors that identity
/// seeding into `delegation_map_rev`. Once a genesis key delegates away,
/// `Bimap.insert` (via `activate_delegations`'s `old_delegate` removal)
/// destroys that self-pair, and the raw genesis key correctly loses ALL
/// standing from that point on — permanently, for both proposals and votes.
/// The previous version of this function had an `allowed_delegators.contains`
/// fallback that made that standing permanent instead, which is a genuine
/// accept-where-Haskell-rejects gap: real on mainnet, where all 7 genesis
/// keys delegate away at slot 0
/// (`config/mainnet/byron-genesis.json`'s `heavyDelegation`).
fn resolve_genesis_authority(
    key_hash: &Hash28,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
) -> Option<Hash28> {
    delegation_map_rev.get(key_hash).copied()
}

/// `Update.Validation.Registration::registerProposal` (Registration.hs:330+).
fn register_proposal(
    update: &mut ByronUpdateState,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
    current_slot: u64,
    proposal: &dugite_primitives::block::ByronUpdProposal,
    magic_cbor: &[u8],
) -> Result<(), ByronUpdateError> {
    let proposer_key_hash = byron_key_hash(&proposal.proposer_vk);
    if resolve_genesis_authority(&proposer_key_hash, delegation_map_rev).is_none() {
        return Err(ByronUpdateError::NotGenesisDelegate);
    }
    // Check 2 (design doc §4.1) — `verifySignatureDecoded ...
    // \`orThrowError\` InvalidSignature`, immediately after genesis-delegate
    // resolution and before every component check below (upstream's own
    // order; `UnsupportedTxFeePolicyOverride` right after it is a
    // dugite-only modelling guard with no direct upstream counterpart, so
    // its position relative to this check carries no fidelity claim).
    if !verify_proposal_signature(
        &proposal.proposer_vk,
        &proposal.body_span,
        &proposal.signature,
        magic_cbor,
    ) {
        return Err(ByronUpdateError::InvalidSignature);
    }
    if proposal.params_update.tx_fee_policy.is_some() {
        return Err(ByronUpdateError::UnsupportedTxFeePolicyOverride);
    }

    let applied_params =
        apply_params_update(&update.adopted_protocol_parameters, &proposal.params_update);
    let protocol_version_changed = proposal.protocol_version != update.adopted_protocol_version
        || applied_params != update.adopted_protocol_parameters;
    // "New" relative to whatever `app_versions` currently records for this
    // app name — `None` (nothing recorded yet) counts as new. Note
    // `register_vote`'s confirmation branch does NOT promote into
    // `app_versions` (design doc §4: nothing downstream of it is modelled,
    // since none of #1084's five dump fields read it), so in the CURRENT
    // implementation this is always `true` in practice and the null-update
    // rejection below is reachable only via its PROTOCOL half. That is a
    // deliberate scope limit, not a bug: a software-only null proposal that
    // wrongly registers here touches no field this implementation reports.
    let software_version_is_new = update
        .app_versions
        .get(&proposal.software_version.0)
        .map(|(v, _)| *v != proposal.software_version.1)
        .unwrap_or(true);

    if !protocol_version_changed && !software_version_is_new {
        return Err(ByronUpdateError::NullUpdate);
    }

    if protocol_version_changed {
        let duplicate_version = update
            .registered_protocol_update_proposals
            .values()
            .any(|(pv, _)| *pv == proposal.protocol_version);
        if duplicate_version {
            return Err(ByronUpdateError::DuplicateVersion);
        }
        if !pv_can_follow(proposal.protocol_version, update.adopted_protocol_version) {
            return Err(ByronUpdateError::VersionCannotFollow);
        }
        if !can_update(
            &update.adopted_protocol_parameters,
            &applied_params,
            proposal.encoded_len,
        ) {
            return Err(ByronUpdateError::CannotUpdate);
        }
        tracing::debug!(
            slot = current_slot,
            up_id = %proposal.up_id.to_hex(),
            protocol_version = ?proposal.protocol_version,
            max_tx_size = ?proposal.params_update.max_tx_size,
            max_block_size = ?proposal.params_update.max_block_size,
            "byron update proposal REGISTERED"
        );
        update
            .registered_protocol_update_proposals
            .insert(proposal.up_id, (proposal.protocol_version, applied_params));
    }
    if software_version_is_new {
        update
            .registered_software_update_proposals
            .insert(proposal.up_id, proposal.software_version.clone());
    }
    update
        .proposal_registration_slot
        .insert(proposal.up_id, current_slot);
    Ok(())
}

/// `Voting::registerVote` + `pastThreshold` + confirmation
/// (Voting.hs:126-208).
///
/// The registered-proposal membership check (`registerVote`'s
/// `upId `Set.member` registeredProposals`, Voting.hs:180-181) is against
/// `vreRegisteredUpdateProposal`, which `Interface.hs::registerVote`
/// (:390,:396) sets to `M.keysSet proposalRegistrationSlot` — NOT
/// `registeredProtocolUpdateProposals`. `proposalRegistrationSlot` gets an
/// entry for EVERY successful registration regardless of whether it was a
/// protocol update, a software update, or both
/// (`Interface.hs::registerProposal`'s unconditional
/// `M.insert (recoverUpId proposal) currentSlot proposalRegistrationSlot`,
/// :274-276) — `registeredProtocolUpdateProposals` only gets an entry when
/// the PROTOCOL half changed. A vote for a software-only proposal therefore
/// exists in `proposalRegistrationSlot` but never in
/// `registeredProtocolUpdateProposals`, and checking the narrower map
/// wrongly rejects it (issue #1093).
fn register_vote(
    update: &mut ByronUpdateState,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
    num_gen_keys: usize,
    current_slot: u64,
    vote: &ByronUpdVote,
    magic_cbor: &[u8],
) -> Result<(), ByronUpdateError> {
    if !update
        .proposal_registration_slot
        .contains_key(&vote.proposal_id)
    {
        return Err(ByronUpdateError::ProposalNotRegistered);
    }
    let voter_key_hash = byron_key_hash(&vote.voter_vk);
    let genesis_key = resolve_genesis_authority(&voter_key_hash, delegation_map_rev)
        .ok_or(ByronUpdateError::NotGenesisDelegate)?;

    // Check 3 (design doc §4.2): "not already cast" — a READ-ONLY lookup.
    // The actual insert is deferred to AFTER the signature check below, so
    // an invalid-signature vote can never poison the votes set and cause a
    // LATER genuinely-signed vote from the same genesis key to be wrongly
    // reported as a duplicate.
    let already_cast = update
        .proposal_votes
        .get(&vote.proposal_id)
        .is_some_and(|votes| votes.contains(&genesis_key));
    if already_cast {
        return Err(ByronUpdateError::VoteAlreadyCast);
    }

    // Check 4, last (design doc §4.2): `verifySignatureDecoded ...
    // \`orThrowError\` VotingInvalidSignature`.
    if !verify_vote_signature(
        &vote.voter_vk,
        &vote.proposal_id_raw,
        &vote.signature,
        magic_cbor,
    ) {
        return Err(ByronUpdateError::InvalidSignature);
    }

    update
        .proposal_votes
        .entry(vote.proposal_id)
        .or_default()
        .insert(genesis_key);

    if !update.confirmed_proposals.contains_key(&vote.proposal_id) {
        let threshold = up_adpt_thd(num_gen_keys as u64, &update.adopted_protocol_parameters);
        if update.proposal_votes[&vote.proposal_id].len() as u64 >= threshold {
            tracing::debug!(
                slot = current_slot,
                up_id = %vote.proposal_id.to_hex(),
                votes = update.proposal_votes[&vote.proposal_id].len(),
                threshold,
                "byron update proposal CONFIRMED"
            );
            update
                .confirmed_proposals
                .insert(vote.proposal_id, current_slot);
            // Promotes the software half into `app_versions` upstream
            // (`registerVotes`, Interface.hs:315-355). Not modelled: nothing
            // downstream of `app_versions` exists in this implementation
            // (design doc §4), and the five #1084 dump fields never read it.
        }
    }
    Ok(())
}

/// `Endorsement::register` + `Interface.hs::registerEndorsement`
/// (Endorsement.hs:148-236, Interface.hs:408-479).
///
/// Ordering matches upstream exactly, which matters for two independent
/// reasons (both found by a review of the previous version of this
/// function):
///
/// 1. `Endorsement.register`'s confirm/threshold/candidate logic
///    (`isConfirmedAndStable`, `numberOfEndorsements`, FADS) runs against
///    the PRE-prune state — `Interface.hs::registerEndorsement`'s `subEnv`
///    is built from `st` BEFORE this call's own prune, since the prune is a
///    separate step the WRAPPER performs afterward.
/// 2. That wrapper's prune of `pidsKeep` (Interface.hs:418-444) is
///    UNCONDITIONAL: it runs after `Endorsement.register` regardless of
///    which of that function's branches fired — including the `[] -> pure
///    st` "no proposal registered for this protocol version" branch, and
///    regardless of whether `issuer_key_hash` resolved to a genesis key at
///    all. `Endorsement.hs:210-218`'s own comment: *"we do not throw an
///    error if there is no corresponding delegate for the given endorsement
///    keyHash. This is consistent with the @UPEND@ rules."* The previous
///    version of this function pruned only on its own success path (an
///    early `return` on an unresolvable key skipped pruning entirely),
///    which silently disabled the proposal TTL whenever an endorsement's
///    key did not resolve.
#[allow(clippy::too_many_arguments)]
fn register_endorsement(
    update: &mut ByronUpdateState,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
    current_slot: u64,
    security_param_k: u64,
    num_gen_keys: usize,
    endorsed_version: (u16, u16, u8),
    issuer_key_hash: Hash28,
) {
    // `Endorsement.register`'s `case M.toList (M.filter ...) of` — a
    // registered protocol proposal that DOES propose `endorsed_version`.
    // `[] -> pure st`: no match means nothing below is even attempted
    // (Haskell never forces the `registeredEndorsements'` where-binding in
    // that branch, so the endorsement is not recorded either).
    let matching = update
        .registered_protocol_update_proposals
        .iter()
        .find(|(_, (pv, _))| *pv == endorsed_version)
        .map(|(upid, (pv, params))| (*upid, *pv, params.clone()));

    if let Some((up_id, pv, params)) = matching {
        // `isConfirmedAndStable upId` (Endorsement.hs:194-197):
        // `addSlotCount (kSlotSecurityParam k) confirmedSlot <= currentSlot`,
        // `kSlotSecurityParam = 2 * k` (ProtocolConstants.hs:19).
        let is_confirmed_and_stable = update
            .confirmed_proposals
            .get(&up_id)
            .is_some_and(|&s| s.saturating_add(2 * security_param_k) <= current_slot);

        if is_confirmed_and_stable {
            // `registeredEndorsements'` (Endorsement.hs:210-218) — forced
            // only in this branch. Unresolvable issuer key: silently
            // ignored per the UPEND comment quoted above.
            if let Some(genesis_key) =
                resolve_genesis_authority(&issuer_key_hash, delegation_map_rev)
            {
                update
                    .registered_endorsements
                    .insert((endorsed_version, genesis_key));
            }
            let endorsement_count = update
                .registered_endorsements
                .iter()
                .filter(|(v, _)| *v == endorsed_version)
                .count() as u64;
            let threshold = up_adpt_thd(num_gen_keys as u64, &update.adopted_protocol_parameters);
            if endorsement_count >= threshold {
                // FADS: prepend only if this version strictly exceeds the
                // current head's.
                let should_prepend = update
                    .candidate_protocol_updates
                    .first()
                    .map(|c| pv > c.protocol_version)
                    .unwrap_or(true);
                if should_prepend {
                    tracing::debug!(
                        slot = current_slot,
                        protocol_version = ?pv,
                        endorsement_count,
                        "byron CANDIDATE created"
                    );
                    update.candidate_protocol_updates.insert(
                        0,
                        ByronCandidate {
                            slot: current_slot,
                            protocol_version: pv,
                            protocol_parameters: params,
                        },
                    );
                }
            }
        }
        // else: not yet confirmed-and-stable — `pure st`, unchanged.
    }

    // `Interface.hs::registerEndorsement`'s UNCONDITIONAL prune — always
    // runs, regardless of every branch above.
    prune_stale_proposals(update, current_slot);
}

/// Per-endorsement-registration pruning (`Interface.hs`): proposals older
/// than `ppUpdateProposalTTL` (genesis `updateImplicit`) and not confirmed
/// are dropped from every proposal-tracking map; endorsements for versions
/// no longer registered are dropped too.
fn prune_stale_proposals(update: &mut ByronUpdateState, current_slot: u64) {
    let ttl = update.adopted_protocol_parameters.update_implicit;
    let stale: Vec<Hash32> = update
        .proposal_registration_slot
        .iter()
        .filter(|(upid, &reg_slot)| {
            !update.confirmed_proposals.contains_key(*upid)
                && reg_slot.saturating_add(ttl) < current_slot
        })
        .map(|(upid, _)| *upid)
        .collect();
    for upid in &stale {
        update.registered_protocol_update_proposals.remove(upid);
        update.registered_software_update_proposals.remove(upid);
        update.proposal_votes.remove(upid);
        update.proposal_registration_slot.remove(upid);
    }
    let live_versions: BTreeSet<(u16, u16, u8)> = update
        .registered_protocol_update_proposals
        .values()
        .map(|(pv, _)| *pv)
        .collect();
    update
        .registered_endorsements
        .retain(|(v, _)| live_versions.contains(v));
}

/// `UPI.registerEpoch` -> `PVBump.tryBumpVersion`
/// (Interface/ProtocolVersionBump.hs:41-63).
///
/// Called ONCE per applied block that crosses an epoch boundary (see
/// `state/apply.rs`'s call site), evaluated against the FINAL epoch reached
/// by the tick — matching upstream's `applyChainTick`, which calls
/// `epochTransition` once with `nextEpoch = slotNumberEpoch slot`, never
/// once per intermediate epoch (design doc's open question §6a; unobservable
/// on any real Byron chain, none of which has an empty epoch).
pub fn upiec_epoch_transition(
    update: &mut ByronUpdateState,
    new_epoch: u64,
    byron_epoch_length: u64,
    security_param_k: u64,
) {
    let epoch_first_slot = new_epoch.saturating_mul(byron_epoch_length);
    let stability = 4 * security_param_k;
    let winner = update
        .candidate_protocol_updates
        .iter()
        .find(|c| c.slot.saturating_add(stability) <= epoch_first_slot)
        .cloned();
    if let Some(candidate) = winner {
        tracing::debug!(
            new_epoch,
            protocol_version = ?candidate.protocol_version,
            max_tx_size = candidate.protocol_parameters.max_tx_size,
            max_block_size = candidate.protocol_parameters.max_block_size,
            "byron ADOPTED"
        );
        update.adopted_protocol_version = candidate.protocol_version;
        update.adopted_protocol_parameters = candidate.protocol_parameters;
        update.candidate_protocol_updates.clear();
        update.registered_protocol_update_proposals.clear();
        update.registered_software_update_proposals.clear();
        update.confirmed_proposals.clear();
        update.proposal_votes.clear();
        update.registered_endorsements.clear();
        update.proposal_registration_slot.clear();
    }
    // `current_epoch` is NOT touched — see the field's doc comment (§2.3).
}

/// Fold a block's `dlgPayload` into the delegation state
/// (`Block/Validation.hs:362-448` step 2: `DI.updateDelegation`).
///
/// # Failure posture
///
/// A per-certificate rule violation is logged and that ONE certificate is
/// SKIPPED — the rest of the block (UTxO, everything else) still applies.
/// This is narrower than #914's "hard error" precedent, and deliberately so:
/// unlike #914's governance case, a Byron delegation-state modelling gap
/// cannot corrupt anything CONSENSUS-CRITICAL, because nothing in dugite's
/// validation or block-production path reads `ByronSubState` yet (the design
/// doc §3.6 names wiring it into validation as explicitly future work).
/// Treating a violation as block-fatal was tried first and measured wrong on
/// real mainnet data: it aborts `apply_block` entirely, which discards the
/// block's UTxO changes too and desyncs every block after it — a far worse
/// outcome than one mis-modelled delegation-state entry. The five #1084 dump
/// fields degrade gracefully (a skipped certificate simply does not move
/// `byronDelegation.count`), which is preferable to corrupting the whole
/// replay over a Byron sub-rule this implementation may not have modelled
/// byte-for-byte in some historical corner case.
///
/// # Signature-failure posture (issue #1092, design doc §8 item 4)
///
/// Unlike the STATE-rule violations above (always log-and-skip, both
/// modes), an [`ByronDelegationError::InvalidSignature`] is escalated to a
/// hard [`LedgerError`] in `ValidateAll` mode — matching upstream, where
/// `Certificate.hs::isValid` is unconditional (§3.1). In `ApplyOnly` it
/// keeps the same log-and-skip posture as every other rule here, per
/// #1084's precedent.
#[allow(clippy::too_many_arguments)]
pub fn apply_delegation_payload(
    delegation: &mut ByronDelegationState,
    allowed_delegators: &BTreeSet<Hash28>,
    aux: &ByronBlockAux,
    current_slot: u64,
    current_epoch: u64,
    security_param_k: u64,
    network_magic: u64,
    mode: ByronApplyMode,
) -> Result<(), LedgerError> {
    let magic_cbor = network_magic_cbor(network_magic);
    for cert in &aux.dlg_certs {
        let issuer = byron_key_hash(&cert.issuer_vk);
        let delegate = byron_key_hash(&cert.delegate_vk);
        match schedule_delegation_cert(
            delegation,
            allowed_delegators,
            current_slot,
            current_epoch,
            security_param_k,
            cert.epoch,
            issuer,
            delegate,
            &cert.issuer_vk,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &cert.signature,
            &magic_cbor,
        ) {
            Ok(()) => {}
            Err(ByronDelegationError::InvalidSignature) if mode == ByronApplyMode::ValidateAll => {
                return Err(LedgerError::ByronSignatureInvalid {
                    slot: current_slot,
                    kind: "delegation certificate".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    slot = current_slot,
                    issuer = %issuer.to_hex(),
                    error = %e,
                    "byron delegation certificate rejected by the scheduling rules — skipped"
                );
            }
        }
    }
    // Tail of `updateDelegation`: tick immediately after scheduling.
    tick_delegation(delegation, current_slot, current_epoch);
    Ok(())
}

/// Fold a block's `updPayload` into the update state (`Block/Validation.hs`
/// step 4: `UPI.registerUpdate`), plus the per-block endorsement every main
/// block registers of its own header's version.
///
/// `delegation_map_rev` MUST be the delegation map AS OF the tick (i.e.
/// captured BEFORE [`apply_delegation_payload`] ran on this same block) —
/// `updateEnv`'s resolver reads the `BodyState`'s ORIGINAL `delegationState`
/// binding, not the post-certificate one (design doc §2.4's ordering
/// subtlety, #1074's lesson in miniature).
///
/// Same failure posture as [`apply_delegation_payload`]: a rule violation on
/// a single proposal or vote is logged and skipped rather than failing the
/// whole block. Measured necessary, not merely convenient — treating a
/// single item as block-fatal desyncs every subsequent block. Issue #1093
/// found a real mainnet block (slot 73486) tripping exactly this path, with
/// two suspected causes, both since fixed here: `resolve_genesis_authority`
/// carried an OR-clause with no upstream counterpart (see its doc), and
/// `register_vote`'s registered-proposal membership check read the wrong
/// map (`registered_protocol_update_proposals`, protocol-only, instead of
/// `proposal_registration_slot`, every successful registration — see
/// `register_vote`'s doc). This skip-and-log posture is kept regardless,
/// since a Byron sub-rule this implementation may still not have modelled
/// byte-for-byte in some historical corner is a lesser failure than
/// desyncing the whole replay over `ByronSubState`, which nothing in
/// validation or block production reads yet (design doc §3.6).
///
/// # Signature-failure posture (issue #1092, design doc §8 item 4)
///
/// Same split as [`apply_delegation_payload`]: state-rule violations always
/// log-and-skip; a proposal/vote [`ByronUpdateError::InvalidSignature`] is
/// escalated to a hard [`LedgerError`] in `ValidateAll` mode (upstream's
/// `Registration.hs`/`Voting.hs` signature checks are unconditional, §4.1),
/// and log-and-skip in `ApplyOnly`.
#[allow(clippy::too_many_arguments)]
pub fn apply_update_payload(
    update: &mut ByronUpdateState,
    allowed_delegators: &BTreeSet<Hash28>,
    delegation_map_rev: &BTreeMap<Hash28, Hash28>,
    aux: &ByronBlockAux,
    current_slot: u64,
    security_param_k: u64,
    network_magic: u64,
    mode: ByronApplyMode,
) -> Result<(), LedgerError> {
    let num_gen_keys = allowed_delegators.len();
    let magic_cbor = network_magic_cbor(network_magic);

    if let Some(proposal) = &aux.upd_proposal {
        match register_proposal(
            update,
            delegation_map_rev,
            current_slot,
            proposal,
            &magic_cbor,
        ) {
            Ok(()) => {}
            Err(ByronUpdateError::InvalidSignature) if mode == ByronApplyMode::ValidateAll => {
                return Err(LedgerError::ByronSignatureInvalid {
                    slot: current_slot,
                    kind: "update proposal".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    slot = current_slot,
                    up_id = %proposal.up_id.to_hex(),
                    error = %e,
                    "byron update proposal rejected by the registration rules — skipped"
                );
            }
        }
    }
    for vote in &aux.upd_votes {
        match register_vote(
            update,
            delegation_map_rev,
            num_gen_keys,
            current_slot,
            vote,
            &magic_cbor,
        ) {
            Ok(()) => {}
            Err(ByronUpdateError::InvalidSignature) if mode == ByronApplyMode::ValidateAll => {
                return Err(LedgerError::ByronSignatureInvalid {
                    slot: current_slot,
                    kind: "update vote".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    slot = current_slot,
                    proposal_id = %vote.proposal_id.to_hex(),
                    error = %e,
                    "byron update vote rejected by the voting rules — skipped"
                );
            }
        }
    }
    // Every main block registers an endorsement of the protocol version its
    // OWN header advertises, keyed by `headerIssuer` — the DELEGATE key
    // recovered from `block_sig`'s embedded certificate, NOT the raw
    // `issuer_pubkey` (the genesis key doing the delegating) read from
    // consensus-data field 1. See `ByronBlockAux::delegate_pubkey`'s doc.
    let delegate_key_hash = byron_key_hash(&aux.delegate_pubkey);
    register_endorsement(
        update,
        delegation_map_rev,
        current_slot,
        security_param_k,
        num_gen_keys,
        aux.protocol_version,
        delegate_key_hash,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::{
        address::{Address, ByronAddress},
        hash::Hash32,
        transaction::{
            OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
            TransactionWitnessSet,
        },
        value::{Lovelace, Value},
    };
    use std::collections::{BTreeMap, HashMap};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// The canonical Byron mainnet/preprod/preview fee policy
    /// (`155381 + ceiling(size * 21973/500)`).
    const TEST_POLICY: ByronFeePolicy = ByronFeePolicy::canonical();

    fn make_byron_address(byte: u8) -> Address {
        Address::Byron(ByronAddress {
            payload: vec![byte; 32],
        })
    }

    /// Build a Byron address whose inner payload is the canonical
    /// `[ root(28B), attrs(empty map), addr_type ]` array, so `is_redeem()`
    /// decodes it. `addr_type`: 0 = PubKey, 1 = Script, 2 = Redeem (AVVM).
    fn make_byron_typed_address(root_byte: u8, addr_type: u8) -> Address {
        let mut payload = vec![0x83u8, 0x58, 0x1c]; // array(3), bytes(len=28)
        payload.extend(std::iter::repeat_n(root_byte, 28));
        payload.push(0xa0); // attrs: empty map
        payload.push(addr_type); // CBOR uint 0..=23 == literal byte
        Address::Byron(ByronAddress { payload })
    }

    fn make_redeem_address(root_byte: u8) -> Address {
        make_byron_typed_address(root_byte, 0x02)
    }

    fn make_pubkey_byron_address(root_byte: u8) -> Address {
        make_byron_typed_address(root_byte, 0x00)
    }

    fn make_output(address: Address, coin: u64) -> TransactionOutput {
        TransactionOutput {
            address,
            value: Value {
                coin: Lovelace(coin),
                multi_asset: Default::default(),
            },
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
            era: dugite_primitives::era::Era::Conway,
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
            // 200-byte dummy raw_cbor so the fee size calculation is deterministic in tests
            raw_cbor: Some(vec![0u8; 200]),
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    fn utxo_map(
        entries: Vec<(TransactionInput, TransactionOutput)>,
    ) -> HashMap<TransactionInput, TransactionOutput> {
        entries.into_iter().collect()
    }

    // -----------------------------------------------------------------------
    // validate_byron_tx tests
    // -----------------------------------------------------------------------

    /// A valid single-input / single-output transaction where the fee exactly
    /// equals the minimum and value is conserved should pass all rules.
    #[test]
    fn test_valid_byron_tx() {
        let input = make_input(0xAA, 0);
        let input_coin = 10_000_000u64; // 10 ADA
                                        // min_fee(200) = 155_381 + ceil(200 * 21973/500) = 164_171 lovelace
        let fee = TEST_POLICY.min_fee(200).unwrap();
        let output_coin = input_coin - fee;

        let utxo = utxo_map(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), input_coin),
        )]);

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), output_coin)],
            fee,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let effect = result.unwrap();
        assert_eq!(effect.fee, Lovelace(fee));
        assert_eq!(effect.consumed.len(), 1);
        assert_eq!(effect.produced.len(), 1);
    }

    /// AVVM voucher-redemption: a tx whose inputs are ALL at redeem (AVVM)
    /// addresses is exempt from the minimum fee (Haskell `isRedeemUTxO` =>
    /// `minFee = mkKnownLovelace @0`), so it may sweep the full balance with a
    /// zero fee. Cross-validated against cardano-ledger `validateTxAux`.
    #[test]
    fn test_avvm_redeem_inputs_are_fee_exempt() {
        let in0 = make_input(0xAA, 0);
        let in1 = make_input(0xAB, 1);
        let utxo = utxo_map(vec![
            (
                in0.clone(),
                make_output(make_redeem_address(0x11), 6_000_000),
            ),
            (
                in1.clone(),
                make_output(make_redeem_address(0x22), 4_000_000),
            ),
        ]);
        // fee = inputs - outputs = 10 ADA - 10 ADA = 0.
        let tx = make_tx(
            0xCC,
            vec![in0, in1],
            vec![make_output(make_pubkey_byron_address(0x33), 10_000_000)],
            0,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);
        assert!(
            result.is_ok(),
            "all-redeem tx with fee=0 must validate (minFee=0), got {result:?}"
        );
        assert_eq!(result.unwrap().fee, Lovelace(0));
    }

    /// A SINGLE non-redeem input breaks the exemption — `isRedeemUTxO` requires
    /// EVERY input to be a redeem address — so the full min-fee is enforced and a
    /// zero-fee tx is rejected.
    #[test]
    fn test_mixed_redeem_and_pubkey_inputs_require_fee() {
        let in0 = make_input(0xAA, 0);
        let in1 = make_input(0xAB, 1);
        let utxo = utxo_map(vec![
            (
                in0.clone(),
                make_output(make_redeem_address(0x11), 6_000_000),
            ),
            (
                in1.clone(),
                make_output(make_pubkey_byron_address(0x22), 4_000_000),
            ),
        ]);
        let tx = make_tx(
            0xCC,
            vec![in0, in1],
            vec![make_output(make_pubkey_byron_address(0x33), 10_000_000)],
            0,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);
        assert!(
            matches!(result, Err(ByronError::FeeTooSmall { .. })),
            "mixed redeem/pubkey inputs with fee=0 must be rejected, got {result:?}"
        );
    }

    /// All-regular (pubkey) inputs are NOT exempt: a zero-fee tx is rejected.
    #[test]
    fn test_pubkey_inputs_require_minimum_fee() {
        let in0 = make_input(0xAA, 0);
        let utxo = utxo_map(vec![(
            in0.clone(),
            make_output(make_pubkey_byron_address(0x11), 10_000_000),
        )]);
        let tx = make_tx(
            0xCC,
            vec![in0],
            vec![make_output(make_pubkey_byron_address(0x33), 10_000_000)],
            0,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);
        assert!(
            matches!(result, Err(ByronError::FeeTooSmall { .. })),
            "pubkey inputs with fee=0 must be rejected, got {result:?}"
        );
    }

    /// A transaction whose inputs are not in the UTxO set returns `InputNotFound`.
    #[test]
    fn test_missing_input_returns_error() {
        let input = make_input(0xAA, 0);
        // Empty UTxO set — input will not be found
        let utxo: HashMap<TransactionInput, TransactionOutput> = HashMap::new();

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), 1_000_000)],
            155_381,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);

        assert!(
            matches!(result, Err(ByronError::InputNotFound(_))),
            "expected InputNotFound, got {result:?}"
        );
    }

    /// A transaction that pays less than the minimum fee is rejected with `FeeTooSmall`.
    #[test]
    fn test_insufficient_fee_returns_error() {
        let input = make_input(0xAA, 0);
        let input_coin = 10_000_000u64;
        let min_fee = TEST_POLICY.min_fee(200).unwrap();
        // Pay one lovelace less than the minimum
        let fee = min_fee - 1;
        let output_coin = input_coin - fee;

        let utxo = utxo_map(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), input_coin),
        )]);

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), output_coin)],
            fee,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);

        assert!(
            matches!(result, Err(ByronError::FeeTooSmall { .. })),
            "expected FeeTooSmall, got {result:?}"
        );
    }

    /// A transaction whose outputs EXCEED its inputs is rejected with
    /// `ValueNotConserved`. Byron fees are implicit (`fee = inputs - outputs`),
    /// so non-conservation means `outputs > inputs` (the implicit fee would be
    /// negative, i.e. the `checked_sub` underflows).
    #[test]
    fn test_value_not_conserved_returns_error() {
        let input = make_input(0xAA, 0);
        let input_coin = 10_000_000u64;
        // Outputs exceed inputs by 1 ADA — impossible to conserve value.
        let output_coin = input_coin + 1_000_000;

        let utxo = utxo_map(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), input_coin),
        )]);

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), output_coin)],
            0,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);

        assert!(
            matches!(result, Err(ByronError::ValueNotConserved { .. })),
            "expected ValueNotConserved, got {result:?}"
        );
    }

    /// A transaction with zero inputs is rejected with `NoInputs`.
    #[test]
    fn test_no_inputs_returns_error() {
        let utxo: HashMap<TransactionInput, TransactionOutput> = HashMap::new();
        let tx = make_tx(
            0xBB,
            vec![],
            vec![make_output(make_byron_address(0x02), 1_000_000)],
            155_381,
        );

        let result = validate_byron_tx(&tx, |i| utxo.get(i).cloned(), TEST_POLICY, 200);

        assert!(
            matches!(result, Err(ByronError::NoInputs)),
            "expected NoInputs, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // apply_byron_block tests
    //
    // apply_byron_block returns a ByronBlockEffect. Tests apply it to a local
    // HashMap via a helper to verify the correct UTxO changes were produced.
    // -----------------------------------------------------------------------

    /// Helper: apply a ByronBlockEffect to a HashMap UTxO store.
    fn apply_effect(
        utxo: &mut HashMap<TransactionInput, TransactionOutput>,
        effect: ByronBlockEffect,
    ) {
        for input in &effect.spent {
            utxo.remove(input);
        }
        for (input, output) in effect.created {
            utxo.insert(input, output);
        }
    }

    /// A block with a single valid transaction is applied correctly: inputs are
    /// removed, outputs are added, and the fee is returned.
    #[test]
    fn test_apply_valid_block() {
        let input = make_input(0xAA, 0);
        let input_coin = 10_000_000u64;
        let fee = TEST_POLICY.min_fee(200).unwrap();
        let output_coin = input_coin - fee;

        let mut utxo: HashMap<TransactionInput, TransactionOutput> = utxo_map(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), input_coin),
        )]);

        let tx = make_tx(
            0xBB,
            vec![input.clone()],
            vec![make_output(make_byron_address(0x02), output_coin)],
            fee,
        );

        // apply_byron_block takes only a lookup closure; mutation is done by the caller
        let result =
            apply_byron_block(&[tx], TEST_POLICY, 1000, ByronApplyMode::ValidateAll, |i| {
                utxo.get(i).cloned()
            });

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let effect = result.unwrap();
        assert_eq!(effect.fees, Lovelace(fee));

        apply_effect(&mut utxo, effect);

        // Input must be consumed
        assert!(
            !utxo.contains_key(&input),
            "spent input should be removed from UTxO set"
        );
        // New output must be present
        let out_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xBBu8; 32]),
            index: 0,
        };
        assert!(
            utxo.contains_key(&out_input),
            "new output should be present in UTxO set"
        );
    }

    /// In ValidateAll mode, a transaction referencing a missing input causes the
    /// entire block application to fail.
    #[test]
    fn test_apply_missing_input_fails_in_validate_mode() {
        let input = make_input(0xAA, 0);
        let utxo: HashMap<TransactionInput, TransactionOutput> = HashMap::new();

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), 1_000_000)],
            155_381,
        );

        let result =
            apply_byron_block(&[tx], TEST_POLICY, 1000, ByronApplyMode::ValidateAll, |i| {
                utxo.get(i).cloned()
            });

        assert!(
            result.is_err(),
            "expected Err for missing input in ValidateAll mode"
        );
    }

    /// In ApplyOnly mode, a missing input causes the UTxO change to be skipped
    /// but the fee is still accumulated — matching bootstrap behavior.
    #[test]
    fn test_apply_missing_input_skipped_in_apply_only_mode() {
        let input = make_input(0xAA, 0);
        let utxo: HashMap<TransactionInput, TransactionOutput> = HashMap::new();
        let fee = 200_000u64;

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), 800_000)],
            fee,
        );

        let result = apply_byron_block(&[tx], TEST_POLICY, 1000, ByronApplyMode::ApplyOnly, |i| {
            utxo.get(i).cloned()
        });

        // The block is confirmed on-chain; we succeed and skip the UTxO change
        assert!(
            result.is_ok(),
            "expected Ok in ApplyOnly mode, got {result:?}"
        );
        let effect = result.unwrap();
        // Fee is still accumulated even when UTxO changes are skipped
        assert_eq!(effect.fees, Lovelace(fee));
        // No UTxO changes produced
        assert!(effect.spent.is_empty(), "no inputs should be consumed");
        assert!(effect.created.is_empty(), "no outputs should be created");
    }

    /// Two independent transactions in the same block both apply correctly.
    #[test]
    fn test_multi_tx_block_applies_in_sequence() {
        let genesis_input1 = make_input(0x11, 0);
        let genesis_input2 = make_input(0x22, 0);
        let coin1 = 10_000_000u64;
        let coin2 = 8_000_000u64;
        let fee1 = TEST_POLICY.min_fee(200).unwrap();
        let fee2 = TEST_POLICY.min_fee(200).unwrap();

        let mut utxo: HashMap<TransactionInput, TransactionOutput> = utxo_map(vec![
            (
                genesis_input1.clone(),
                make_output(make_byron_address(0x01), coin1),
            ),
            (
                genesis_input2.clone(),
                make_output(make_byron_address(0x02), coin2),
            ),
        ]);

        // Tx1 spends genesis_input1
        let tx1 = make_tx(
            0xAA,
            vec![genesis_input1.clone()],
            vec![make_output(make_byron_address(0x03), coin1 - fee1)],
            fee1,
        );

        // Tx2 spends genesis_input2 (independent — no within-block dependency)
        let tx2 = make_tx(
            0xBB,
            vec![genesis_input2.clone()],
            vec![make_output(make_byron_address(0x04), coin2 - fee2)],
            fee2,
        );

        let result = apply_byron_block(
            &[tx1, tx2],
            TEST_POLICY,
            2000,
            ByronApplyMode::ValidateAll,
            |i| utxo.get(i).cloned(),
        );

        assert!(
            result.is_ok(),
            "expected Ok for multi-tx block, got {result:?}"
        );
        let effect = result.unwrap();
        assert_eq!(
            effect.fees,
            Lovelace(fee1 + fee2),
            "total fees should be the sum of both transaction fees"
        );
        assert_eq!(effect.spent.len(), 2, "two inputs consumed");
        assert_eq!(effect.created.len(), 2, "two outputs created");

        apply_effect(&mut utxo, effect);

        assert!(
            !utxo.contains_key(&genesis_input1),
            "genesis_input1 consumed"
        );
        assert!(
            !utxo.contains_key(&genesis_input2),
            "genesis_input2 consumed"
        );
        let out1 = TransactionInput {
            transaction_id: Hash32::from_bytes([0xAAu8; 32]),
            index: 0,
        };
        let out2 = TransactionInput {
            transaction_id: Hash32::from_bytes([0xBBu8; 32]),
            index: 0,
        };
        assert!(utxo.contains_key(&out1), "tx1 output present");
        assert!(utxo.contains_key(&out2), "tx2 output present");
    }

    /// `ByronFeePolicy::min_fee` matches Haskell `calculateTxSizeLinear`:
    /// `a + ceiling(size * b)` with `a = 155381`, `b = 21973/500` (43.946 exact)
    /// and CEILING rounding over exact rational arithmetic. Cross-validated against
    /// cardano-ledger `Cardano.Chain.Common.{TxSizeLinear,Lovelace}`.
    #[test]
    fn test_fee_policy_min_fee() {
        let policy = ByronFeePolicy::canonical();

        // Zero-size tx — only the constant summand.
        assert_eq!(policy.min_fee(0), Some(155_381));

        // size 200: 200 * 21973/500 = 8789.2 -> ceiling 8790; 155381 + 8790.
        // (Integer-projection `44*200 + 155381 = 164181` would be WRONG by +10.)
        assert_eq!(policy.min_fee(200), Some(164_171));

        // size 500: an exact multiple of the denominator -> no rounding.
        // 500 * 21973/500 = 21973 exactly; 155381 + 21973.
        assert_eq!(policy.min_fee(500), Some(177_354));

        // size 1: 21973/500 = 43.946 -> ceiling 44 (floor would give 43).
        assert_eq!(policy.min_fee(1), Some(155_425));

        // size 501: 501 * 21973/500 = 22016.946 -> ceiling 22017.
        assert_eq!(policy.min_fee(501), Some(177_398));

        // A redeem-input tx is exempt (min fee 0) — verified in the AVVM test;
        // here we only confirm the linear formula. Overflow guard:
        assert_eq!(policy.min_fee(u64::MAX), None);
    }

    // -----------------------------------------------------------------------
    // EraRules trait tests
    //
    // These tests verify the ByronRules EraRules implementation is callable
    // and produces correct results when invoked through the trait interface.
    // -----------------------------------------------------------------------

    use crate::eras::{EraRules, EraRulesImpl, RuleContext};
    use crate::state::{
        BlockValidationMode, EpochSnapshots, GovernanceState, StakeDistributionState,
    };
    use crate::utxo::UtxoSet;
    use crate::utxo_diff::DiffSeq;
    use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::era::Era;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
    use std::sync::Arc;

    /// Build a minimal BlockHeader for tests.
    fn make_block_header(prev_hash: Hash32, issuer_vkey: Vec<u8>) -> BlockHeader {
        BlockHeader {
            header_hash: Hash32::ZERO,
            prev_hash,
            issuer_vkey,
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            block_number: BlockNo(0),
            slot: SlotNo(0),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            kes_signature: vec![],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
        }
    }

    /// Build a minimal RuleContext for Byron era tests.
    fn make_byron_ctx(params: &ProtocolParameters) -> RuleContext<'_> {
        // Leak a static empty map for genesis_delegates since RuleContext
        // borrows it, and we need it to live long enough for the test.
        let delegates = Box::leak(Box::new(HashMap::new()));
        RuleContext {
            params,
            current_slot: 1000,
            current_epoch: EpochNo(0),
            era: Era::Byron,
            slot_config: None,
            node_network: None,
            genesis_delegates: delegates,
            update_quorum: 5,
            epoch_length: 21600,
            shelley_transition_epoch: 0,
            byron_epoch_length: 21600,
            stability_window: 0,
            stability_window_3kf: 0,
            randomness_stabilisation_window: 0,
            tx_index: 0,
            conway_genesis: None,
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        }
    }

    /// Build a minimal UtxoSubState with given entries.
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
            delegations: imbl::HashMap::new(),
            pool_params: Arc::new(HashMap::new()),
            future_pool_params: HashMap::new(),
            pending_retirements: HashMap::new(),
            vrf_key_hashes: Default::default(),
            reward_accounts: imbl::HashMap::new(),
            stake_key_deposits: imbl::HashMap::new(),
            pool_deposits: HashMap::new(),
            total_stake_key_deposits: 0,
            pointer_map: HashMap::new(),
            stake_distribution: StakeDistributionState {
                stake_map: HashMap::new(),
            },
            script_stake_credentials: std::collections::HashSet::new(),
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
        EpochSubState {
            snapshots: EpochSnapshots::default(),
            treasury: Lovelace(0),
            reserves: Lovelace(0),
            pending_reward_update: None,
            non_myopic: Default::default(),
            last_applied_rupd: None,
            pending_pp_updates: BTreeMap::new(),
            future_pp_updates: BTreeMap::new(),
            needs_stake_rebuild: false,
            ptr_stake: HashMap::new(),
            ptr_stake_excluded: false,
            protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_version_major: 1,
            prev_d: dugite_primitives::transaction::Rational {
                numerator: 1,
                denominator: 1,
            },
            rupd_addrs_rew: None,
            rupd_pulser_started: false,
            rupd_monetary: None,
            rupd_snapshot: None,
            rupd_fold: Default::default(),
            pending_avvm_return: 0,
        }
    }

    fn make_consensus_sub() -> ConsensusSubState {
        use dugite_primitives::hash::Hash32;
        ConsensusSubState {
            evolving_nonce: Hash32::ZERO,
            candidate_nonce: Hash32::ZERO,
            epoch_nonce: Hash32::ZERO,
            previous_epoch_nonce: Hash32::ZERO,
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

    /// Verify that ByronRules can be constructed via EraRulesImpl::for_era(Byron).
    #[test]
    fn test_era_rules_impl_for_byron() {
        let rules = EraRulesImpl::for_era(Era::Byron);
        assert!(matches!(rules, EraRulesImpl::Byron(_)));
    }

    /// Verify validate_block_body always succeeds for Byron.
    #[test]
    fn test_byron_validate_block_body_succeeds() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        // We need a minimal Block; since validate_block_body is a no-op for Byron,
        // we just verify it doesn't panic or error.
        let block = dugite_primitives::block::Block {
            era: Era::Byron,
            header: make_block_header(Hash32::ZERO, vec![]),
            transactions: vec![],
            raw_cbor: None,
            byron: None,
        };

        let result = rules.validate_block_body(&block, &ctx, &utxo);
        assert!(
            result.is_ok(),
            "Byron validate_block_body should always succeed"
        );
    }

    /// Verify apply_valid_tx through the EraRules trait processes a valid Byron tx.
    #[test]
    fn test_byron_era_rules_apply_valid_tx() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);

        let input = make_input(0xAA, 0);
        let input_coin = 10_000_000u64;
        let fee = TEST_POLICY.min_fee(200).unwrap();
        let output_coin = input_coin - fee;

        let mut utxo = make_utxo_sub(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), input_coin),
        )]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let tx = make_tx(
            0xBB,
            vec![input.clone()],
            vec![make_output(make_byron_address(0x02), output_coin)],
            fee,
        );

        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ValidateAll,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let diff = result.unwrap();

        // Verify UTxO changes
        assert_eq!(diff.deletes.len(), 1, "one input consumed");
        assert_eq!(diff.inserts.len(), 1, "one output produced");

        // Input should be removed from the UTxO set
        assert!(utxo.utxo_set.lookup(&input).is_none(), "input consumed");

        // New output should be present
        let out = TransactionInput {
            transaction_id: Hash32::from_bytes([0xBBu8; 32]),
            index: 0,
        };
        assert!(utxo.utxo_set.lookup(&out).is_some(), "output produced");

        // Fees accumulated
        assert_eq!(utxo.epoch_fees, Lovelace(fee));
    }

    /// Verify apply_invalid_tx returns an error for Byron.
    #[test]
    fn test_byron_apply_invalid_tx_errors() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0xAA, vec![], vec![], 0);

        let mut certs = make_cert_sub();
        let mut epochs = make_epoch_sub();
        let result = rules.apply_invalid_tx(
            &tx,
            BlockValidationMode::ValidateAll,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut epochs,
        );

        assert!(
            result.is_err(),
            "Byron apply_invalid_tx should always error"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Byron era does not support invalid transactions"),
            "Error message should mention Byron: {err_msg}"
        );
    }

    /// Verify evolve_nonce sets lab_nonce and tracks block production.
    #[test]
    fn test_byron_evolve_nonce() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);
        let mut consensus = make_consensus_sub();

        let prev_hash = Hash32::from_bytes([0xABu8; 32]);
        let issuer_vkey = vec![0x01u8; 32]; // 32 bytes = valid vkey

        let header = make_block_header(prev_hash, issuer_vkey);

        rules.evolve_nonce(&header, &ctx, &mut consensus);

        // Byron keeps lab_nonce at NeutralNonce (ZERO): PBFT does not maintain the
        // TPraos csLabNonce. Setting it to a Byron prev-hash would poison the first
        // Shelley epoch-nonce TICKN (see apply.rs / byron::evolve_nonce).
        let _ = prev_hash;
        assert_eq!(
            consensus.lab_nonce,
            dugite_primitives::hash::Hash32::ZERO,
            "Byron lab_nonce stays NeutralNonce"
        );

        // Block count should be incremented
        assert_eq!(consensus.epoch_block_count, 1);

        // Pool ID (blake2b-224 of issuer_vkey) should have 1 block
        assert_eq!(consensus.epoch_blocks_by_pool.len(), 1);
    }

    /// Verify min_fee returns the correct Byron linear fee.
    #[test]
    fn test_byron_min_fee_via_trait() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0xAA, vec![], vec![], 0);
        // tx has 200 bytes of raw_cbor
        let min = rules.min_fee(&tx, &ctx, &utxo);
        // 155381 + ceil(200 * 21973/500) = 155381 + 8790 = 164171 (Byron rational policy)
        assert_eq!(min, 164_171);
    }

    /// Verify process_epoch_transition resets block counters.
    #[test]
    fn test_byron_process_epoch_transition() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        // Simulate some block production
        consensus.epoch_block_count = 42;
        let mut blocks = HashMap::new();
        blocks.insert(
            dugite_primitives::hash::Hash28::from_bytes([1u8; 28]),
            10u64,
        );
        consensus.epoch_blocks_by_pool = Arc::new(blocks);

        let result = rules.process_epoch_transition(
            EpochNo(1),
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
            &mut consensus,
        );

        assert!(result.is_ok());

        // Block counters should be reset
        assert_eq!(consensus.epoch_block_count, 0);
        assert!(consensus.epoch_blocks_by_pool.is_empty());
    }

    /// Verify on_era_transition is a no-op for Byron.
    #[test]
    fn test_byron_on_era_transition_noop() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();

        let result = rules.on_era_transition(
            Era::Byron,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );

        assert!(result.is_ok(), "Byron on_era_transition should be no-op");
    }

    /// Verify required_witnesses returns an empty set for Byron addresses.
    ///
    /// Byron addresses use bootstrap witnesses (not VKey witnesses), so
    /// `required_witnesses` should return empty for pure Byron transactions.
    #[test]
    fn test_byron_required_witnesses_empty_for_byron_addresses() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);

        let input = make_input(0xAA, 0);
        let utxo = make_utxo_sub(vec![(
            input.clone(),
            make_output(make_byron_address(0x01), 10_000_000),
        )]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let tx = make_tx(0xBB, vec![input], vec![], 0);

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);

        // Byron addresses don't have a payment_credential — empty set expected.
        assert!(
            witnesses.is_empty(),
            "Byron addresses use bootstrap witnesses, not VKey"
        );
    }

    /// Verify the EraRulesImpl enum correctly forwards to ByronRules.
    #[test]
    fn test_era_rules_impl_forwards_min_fee() {
        let rules = EraRulesImpl::for_era(Era::Byron);
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0xAA, vec![], vec![], 0);
        let min = rules.min_fee(&tx, &ctx, &utxo);
        assert_eq!(min, 164_171, "EraRulesImpl should forward to ByronRules");
    }

    /// Verify apply_valid_tx in ApplyOnly mode with missing inputs accumulates fees.
    #[test]
    fn test_byron_era_rules_apply_only_missing_input() {
        let rules = ByronRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_byron_ctx(&params);

        let input = make_input(0xAA, 0);
        let fee = 200_000u64;

        // Empty UTxO set — input will not be found
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(make_byron_address(0x02), 800_000)],
            fee,
        );

        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ApplyOnly,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );

        assert!(
            result.is_ok(),
            "ApplyOnly should succeed even with missing inputs"
        );
        let diff = result.unwrap();

        // No UTxO changes but fee is accumulated
        assert!(diff.inserts.is_empty());
        assert!(diff.deletes.is_empty());
        assert_eq!(utxo.epoch_fees, Lovelace(fee));
    }
}

// ============================================================================
// Byron delegation + update-proposal state machine tests (#1084)
// ============================================================================
//
// Constructed synthetic scenarios matching the design doc's worked
// constants: mainnet k=2160 (2k=4320, 4k=8640), 7 genesis keys, confirmation/
// endorsement threshold floor(0.6*7)=4, epoch_length=21600. The end-to-end
// test reproduces the SHAPE of the real mainnet epoch-16 event (a
// maxTxSize-only bump, 4096 -> 65536) that the mainnet archived-dump
// comparison (see the design doc §5) validates against real chain data —
// this test is the mechanism-level proof that does not need that data.
#[cfg(test)]
mod byron_update_state_tests {
    use super::*;
    use dugite_primitives::block::ByronUpdProposal;
    use std::collections::BTreeSet;

    const MAINNET_K: u64 = 2160;
    const MAINNET_EPOCH_LENGTH: u64 = 21_600;

    fn pk(seed: u8) -> Vec<u8> {
        vec![seed; 64]
    }

    // -----------------------------------------------------------------------
    // #1092: genuinely-signed test fixtures.
    //
    // `pk()` above returns a fake 64-byte fill pattern — fine as a HASH input
    // (every state-rule test above needs only `byron_key_hash` identities,
    // never a curve point), but not a valid Ed25519 public key, so it cannot
    // be used to exercise the NEW signature checks. These helpers build a
    // REAL keypair and sign the EXACT message production verifies (via the
    // shared `build_*_message` functions, so a wire-format change can never
    // make this file's own tests silently drift from what verification
    // checks), matching the design doc's RED-proof requirement: reject a
    // wrong key/corrupted signature/wrong message, accept a genuine one.
    // -----------------------------------------------------------------------

    /// A real Ed25519 keypair, distinct from `pk()`'s fake fill pattern.
    /// `xpub` is the 64-byte extended key (32-byte real public key ‖
    /// 32-byte arbitrary chain-code filler) — `verify_xsig` ignores the
    /// chain code, matching `CC.verify`'s own behaviour (dugite-crypto's
    /// `byron::tests::chain_code_bytes_do_not_affect_verification` proves
    /// this at the primitive level; nothing here needs to re-prove it).
    struct TestKeypair {
        signing_key: ed25519_dalek::SigningKey,
        xpub: [u8; 64],
    }

    impl TestKeypair {
        fn new(seed: u8) -> Self {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
            let mut xpub = [0u8; 64];
            xpub[..32].copy_from_slice(signing_key.verifying_key().as_bytes());
            xpub[32..].copy_from_slice(&[0xCC; 32]); // arbitrary chain code
            TestKeypair { signing_key, xpub }
        }

        fn sign(&self, msg: &[u8]) -> Vec<u8> {
            use ed25519_dalek::Signer as _;
            self.signing_key.sign(msg).to_bytes().to_vec()
        }
    }

    /// Test-network magic (an arbitrary non-mainnet value) and its CBOR
    /// encoding, shared by every signed-fixture builder below.
    const TEST_MAGIC: u64 = 42;
    fn test_magic_cbor() -> Vec<u8> {
        network_magic_cbor(TEST_MAGIC)
    }

    /// Build a genuinely-signed `ByronDlgCert`.
    fn signed_dlg_cert(
        issuer: &TestKeypair,
        delegate_xpub: &[u8; 64],
        epoch: u64,
    ) -> dugite_primitives::block::ByronDlgCert {
        let mut epoch_raw = Vec::new();
        minicbor::encode(epoch, &mut epoch_raw).expect("u64 encode cannot fail");
        let msg = build_dlg_cert_message(delegate_xpub, &epoch_raw, &test_magic_cbor());
        dugite_primitives::block::ByronDlgCert {
            epoch,
            issuer_vk: issuer.xpub.to_vec(),
            delegate_vk: delegate_xpub.to_vec(),
            signature: issuer.sign(&msg),
            epoch_raw,
        }
    }

    /// Build a genuinely-signed [`GenesisHeavyDelegationCert`], reusing
    /// [`signed_dlg_cert`]'s message construction.
    fn signed_genesis_cert(
        issuer: &TestKeypair,
        issuer_hash: Hash28,
        delegate: &TestKeypair,
        delegate_hash: Hash28,
        omega: u64,
    ) -> GenesisHeavyDelegationCert {
        let cert = signed_dlg_cert(issuer, &delegate.xpub, omega);
        GenesisHeavyDelegationCert {
            issuer: issuer_hash,
            delegate: delegate_hash,
            issuer_vk: cert.issuer_vk,
            delegate_vk: cert.delegate_vk,
            signature: cert.signature,
            omega,
        }
    }

    /// Build a genuinely-signed `ByronUpdProposal`. `body_span` must be the
    /// EXACT bytes a real decode would capture (elements 0-4, no enclosing
    /// array header) — callers construct it directly since these tests
    /// don't decode real wire bytes.
    fn signed_proposal(
        proposer: &TestKeypair,
        up_id: Hash32,
        body_span: Vec<u8>,
        protocol_version: (u16, u16, u8),
        params_update: ByronParamsUpdate,
        software_version: (String, u32),
    ) -> ByronUpdProposal {
        let msg = build_proposal_message(&body_span, &test_magic_cbor());
        let encoded_len = body_span.len() as u64;
        ByronUpdProposal {
            up_id,
            encoded_len,
            protocol_version,
            params_update,
            software_version,
            proposer_vk: proposer.xpub.to_vec(),
            signature: proposer.sign(&msg),
            body_span,
        }
    }

    /// Build a genuinely-signed `ByronUpdVote`. `proposal_id_raw` must be
    /// the exact CBOR wire bytes of the proposal id (a canonical 32-byte
    /// bstr: `0x58 0x20 ‖ 32 bytes` — the header threshold means anything
    /// below 24 bytes would use the short form instead, but 32 always uses
    /// `0x58`).
    fn signed_vote(voter: &TestKeypair, proposal_id: Hash32) -> ByronUpdVote {
        let mut proposal_id_raw = vec![0x58, 0x20];
        proposal_id_raw.extend_from_slice(proposal_id.as_bytes());
        let msg = build_vote_message(&proposal_id_raw, &test_magic_cbor());
        ByronUpdVote {
            voter_vk: voter.xpub.to_vec(),
            proposal_id,
            signature: voter.sign(&msg),
            proposal_id_raw,
        }
    }

    /// The exact mainnet Byron genesis `blockVersionData`, as read from a
    /// real mainnet `byron-genesis.json` (`cn-mainnet-config/`).
    fn mainnet_genesis_params() -> ByronProtocolParameters {
        ByronProtocolParameters {
            script_version: 0,
            slot_duration: 20_000,
            max_block_size: 2_000_000,
            max_header_size: 2_000_000,
            max_tx_size: 4_096,
            max_proposal_size: 700,
            mpc_thd: 20_000_000_000_000,
            heavy_del_thd: 300_000_000_000,
            update_vote_thd: 1_000_000_000_000,
            update_proposal_thd: 100_000_000_000_000,
            update_implicit: 10_000,
            soft_fork_rule: (900_000_000_000_000, 600_000_000_000_000, 50_000_000_000_000),
            tx_fee_policy: (155_381, (21_973, 500)),
            unlock_stake_epoch: u64::MAX,
        }
    }

    #[test]
    fn up_adpt_thd_matches_mainnet_4_of_7() {
        let pp = mainnet_genesis_params();
        assert_eq!(up_adpt_thd(7, &pp), 4);
    }

    #[test]
    fn genesis_seeding_builds_identity_then_overlays_heavy_delegation() {
        // #1092: `seed_byron_genesis` now signature-verifies every cert, so
        // this needs REAL keypairs (`pk()`'s fake fill pattern is not a
        // valid curve point) — see `TestKeypair`/`signed_genesis_cert`.
        let g: Vec<TestKeypair> = (0..3u8).map(TestKeypair::new).collect();
        let d: Vec<TestKeypair> = (10..12u8).map(TestKeypair::new).collect();
        let g_hash: Vec<Hash28> = g.iter().map(|k| byron_key_hash(&k.xpub)).collect();
        let d_hash: Vec<Hash28> = d.iter().map(|k| byron_key_hash(&k.xpub)).collect();
        let allowed: BTreeSet<Hash28> = g_hash.iter().copied().collect();
        // g[0] delegates to d[0]; g[1] delegates to d[1]; g[2] never delegates.
        let heavy = vec![
            signed_genesis_cert(&g[0], g_hash[0], &d[0], d_hash[0], 0),
            signed_genesis_cert(&g[1], g_hash[1], &d[1], d_hash[1], 0),
        ];
        let sub = seed_byron_genesis(allowed, &heavy, mainnet_genesis_params(), TEST_MAGIC);

        assert_eq!(
            sub.delegation.delegation_map.get(&g_hash[0]),
            Some(&d_hash[0])
        );
        assert_eq!(
            sub.delegation.delegation_map.get(&g_hash[1]),
            Some(&d_hash[1])
        );
        // g[2] keeps the identity mapping — never overridden.
        assert_eq!(
            sub.delegation.delegation_map.get(&g_hash[2]),
            Some(&g_hash[2])
        );
        assert_eq!(
            sub.delegation.delegation_map_rev.get(&d_hash[0]),
            Some(&g_hash[0])
        );
        assert_eq!(
            sub.delegation.delegation_map_rev.get(&d_hash[1]),
            Some(&g_hash[1])
        );
        assert_eq!(
            sub.delegation.delegation_map_rev.get(&g_hash[2]),
            Some(&g_hash[2])
        );
        // The identity pairs for g[0]/g[1] must be GONE from the reverse map
        // — otherwise g[0]/g[1] would (wrongly) still resolve as their own
        // delegate too.
        assert!(!sub.delegation.delegation_map_rev.contains_key(&g_hash[0]));
        assert!(!sub.delegation.delegation_map_rev.contains_key(&g_hash[1]));
        assert_eq!(sub.update.current_epoch, 0, "§2.3: always 0 after seeding");
        assert_eq!(sub.update.adopted_protocol_version, (0, 0, 0));
    }

    #[test]
    fn schedule_delegation_cert_rejects_non_genesis_issuer() {
        let allowed: BTreeSet<Hash28> = [byron_key_hash(&pk(0))].into_iter().collect();
        let mut state = ByronDelegationState::default();
        let not_genesis = byron_key_hash(&pk(99));
        // NotGenesisKey fires before the signature check (check 1 of 5), so
        // the signature material below is never read — garbage is fine.
        let err = schedule_delegation_cert(
            &mut state,
            &allowed,
            1000,
            0,
            MAINNET_K,
            0,
            not_genesis,
            byron_key_hash(&pk(1)),
            &[],
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(err, ByronDelegationError::NotGenesisKey);
    }

    #[test]
    fn schedule_delegation_cert_rejects_duplicate_epoch_issuer() {
        let issuer_kp = TestKeypair::new(0);
        let issuer = byron_key_hash(&issuer_kp.xpub);
        let allowed: BTreeSet<Hash28> = [issuer].into_iter().collect();
        let mut state = ByronDelegationState::default();
        let delegate0 = TestKeypair::new(1);
        let cert0 = signed_dlg_cert(&issuer_kp, &delegate0.xpub, 0);
        let magic_cbor = test_magic_cbor();
        schedule_delegation_cert(
            &mut state,
            &allowed,
            1000,
            0,
            MAINNET_K,
            0,
            issuer,
            byron_key_hash(&delegate0.xpub),
            &cert0.issuer_vk,
            &cert0.delegate_vk,
            &cert0.epoch_raw,
            &cert0.signature,
            &magic_cbor,
        )
        .expect("first cert accepted");
        // AlreadyDelegated fires before the signature check (check 3 of 5),
        // so garbage signature material below is fine.
        let err = schedule_delegation_cert(
            &mut state,
            &allowed,
            2000,
            0,
            MAINNET_K,
            0,
            issuer,
            byron_key_hash(&pk(2)),
            &[],
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(err, ByronDelegationError::AlreadyDelegated);
    }

    /// A certificate scheduled at slot S activates only once the tick
    /// reaches `S + 2k` — not one slot earlier (design doc §2.5).
    #[test]
    fn delegation_cert_activates_exactly_at_2k_not_before() {
        let issuer_kp = TestKeypair::new(0);
        let delegate_kp = TestKeypair::new(1);
        let issuer = byron_key_hash(&issuer_kp.xpub);
        let delegate = byron_key_hash(&delegate_kp.xpub);
        let allowed: BTreeSet<Hash28> = [issuer].into_iter().collect();
        let mut state = ByronDelegationState::default();
        state.delegation_map.insert(issuer, issuer);
        state.delegation_map_rev.insert(issuer, issuer);
        state.delegation_slots.insert(issuer, 0);

        let cert = signed_dlg_cert(&issuer_kp, &delegate_kp.xpub, 0);
        let magic_cbor = test_magic_cbor();
        schedule_delegation_cert(
            &mut state,
            &allowed,
            1000,
            0,
            MAINNET_K,
            0,
            issuer,
            delegate,
            &cert.issuer_vk,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &cert.signature,
            &magic_cbor,
        )
        .expect("scheduled");
        let activation_slot = 1000 + 2 * MAINNET_K;

        tick_delegation(&mut state, activation_slot - 1, 0);
        assert_eq!(
            state.delegation_map.get(&issuer),
            Some(&issuer),
            "must NOT activate one slot early"
        );

        tick_delegation(&mut state, activation_slot, 0);
        assert_eq!(
            state.delegation_map.get(&issuer),
            Some(&delegate),
            "must activate exactly at slot+2k"
        );
        assert_eq!(state.delegation_map_rev.get(&delegate), Some(&issuer));
        assert!(
            !state.delegation_map_rev.contains_key(&issuer),
            "the old identity pair must be replaced, not left dangling"
        );
    }

    /// End-to-end: a proposal changing ONLY `maxTxSize` (the exact shape of
    /// the real mainnet epoch-16 event) registers, confirms at the 4th vote
    /// (of 7 genesis keys), creates a candidate at the 4th endorsement once
    /// stable (2k slots after confirmation), and adopts at the first epoch
    /// boundary whose first slot is >= candidate_slot + 4k.
    #[test]
    fn proposal_confirms_and_adopts_matching_mainnet_epoch_16_shape() {
        // #1092: `register_proposal`/`register_vote` now signature-verify,
        // so the 7 genesis keys need to be REAL keypairs (`pk()`'s fake fill
        // pattern is not a valid curve point) — see `TestKeypair`.
        let g: Vec<TestKeypair> = (0..7u8).map(TestKeypair::new).collect();
        let g_hash: Vec<Hash28> = g.iter().map(|k| byron_key_hash(&k.xpub)).collect();
        let allowed: BTreeSet<Hash28> = g_hash.iter().copied().collect();
        let mut sub =
            seed_byron_genesis(allowed.clone(), &[], mainnet_genesis_params(), TEST_MAGIC);
        // Every genesis key starts at protocol version 0.0.0 per seeding;
        // bump to 1.0.0 so the proposal's 1.1.0 satisfies `pvCanFollow`.
        sub.update.adopted_protocol_version = (1, 0, 0);
        let magic_cbor = test_magic_cbor();

        let proposal = signed_proposal(
            &g[0],
            Hash32::from_bytes([0xAA; 32]),
            b"mainnet-epoch-16-shape body span placeholder".to_vec(),
            (1, 1, 0),
            ByronParamsUpdate {
                max_tx_size: Some(65_536),
                ..Default::default()
            },
            ("cardano-sl".to_string(), 1),
        );

        register_proposal(
            &mut sub.update,
            &sub.delegation.delegation_map_rev,
            100,
            &proposal,
            &magic_cbor,
        )
        .expect("registration must succeed");
        assert!(sub
            .update
            .registered_protocol_update_proposals
            .contains_key(&proposal.up_id));
        let (_, registered_params) =
            &sub.update.registered_protocol_update_proposals[&proposal.up_id];
        assert_eq!(
            registered_params.max_tx_size, 65_536,
            "PPU.apply must overlay max_tx_size onto the FULL adopted record"
        );
        assert_eq!(
            registered_params.max_block_size, 2_000_000,
            "every untouched field must carry over from the adopted record"
        );

        // Votes from genesis keys 0..3 — the 4th (index 3) crosses floor(0.6*7)=4.
        for (i, slot) in (0u8..3).zip(101u64..104) {
            let vote = signed_vote(&g[i as usize], proposal.up_id);
            register_vote(
                &mut sub.update,
                &sub.delegation.delegation_map_rev,
                7,
                slot,
                &vote,
                &magic_cbor,
            )
            .unwrap_or_else(|e| panic!("vote {i} must succeed: {e}"));
            assert!(
                !sub.update.confirmed_proposals.contains_key(&proposal.up_id),
                "must not confirm before the 4th vote (only {} cast)",
                i + 1
            );
        }
        let vote4 = signed_vote(&g[3], proposal.up_id);
        register_vote(
            &mut sub.update,
            &sub.delegation.delegation_map_rev,
            7,
            104,
            &vote4,
            &magic_cbor,
        )
        .expect("4th vote must succeed");
        assert_eq!(
            sub.update.confirmed_proposals.get(&proposal.up_id),
            Some(&104),
            "must confirm at exactly the 4th vote's slot"
        );

        // A 5th vote from an already-registered voter is rejected...
        let dup = signed_vote(&g[0], proposal.up_id);
        let err = register_vote(
            &mut sub.update,
            &sub.delegation.delegation_map_rev,
            7,
            105,
            &dup,
            &magic_cbor,
        )
        .unwrap_err();
        assert_eq!(err, ByronUpdateError::VoteAlreadyCast);

        // Endorsements: not stable until confirmed_slot(104) + 2k(4320) = 4424.
        let endorse_stable_slot = 104 + 2 * MAINNET_K;
        for issuer_hash in g_hash.iter().take(4).copied() {
            register_endorsement(
                &mut sub.update,
                &sub.delegation.delegation_map_rev,
                endorse_stable_slot - 1,
                MAINNET_K,
                7,
                proposal.protocol_version,
                issuer_hash,
            );
        }
        assert!(
            sub.update.candidate_protocol_updates.is_empty(),
            "must not create a candidate before stability (2k after confirmation)"
        );

        for issuer_hash in g_hash.iter().take(4).copied() {
            register_endorsement(
                &mut sub.update,
                &sub.delegation.delegation_map_rev,
                endorse_stable_slot,
                MAINNET_K,
                7,
                proposal.protocol_version,
                issuer_hash,
            );
        }
        assert_eq!(
            sub.update.candidate_protocol_updates.len(),
            1,
            "the 4th endorsement (of 4, past stability) must create exactly one candidate"
        );
        let candidate = &sub.update.candidate_protocol_updates[0];
        assert_eq!(candidate.protocol_version, (1, 1, 0));
        assert_eq!(candidate.slot, endorse_stable_slot);
        assert_eq!(candidate.protocol_parameters.max_tx_size, 65_536);

        // Adoption: NOT yet at epoch 0 (candidate.slot + 4k > 0's first slot).
        upiec_epoch_transition(&mut sub.update, 0, MAINNET_EPOCH_LENGTH, MAINNET_K);
        assert_eq!(
            sub.update.adopted_protocol_parameters.max_tx_size, 4_096,
            "must not adopt before candidate.slot + 4k <= the boundary's first slot"
        );

        // Epoch 1's first slot (21600) IS >= candidate.slot(4424) + 4k(8640) = 13064.
        upiec_epoch_transition(&mut sub.update, 1, MAINNET_EPOCH_LENGTH, MAINNET_K);
        assert_eq!(
            sub.update.adopted_protocol_parameters.max_tx_size, 65_536,
            "must adopt at the first epoch boundary clearing candidate.slot + 4k"
        );
        assert_eq!(sub.update.adopted_protocol_version, (1, 1, 0));
        assert_eq!(
            sub.update.adopted_protocol_parameters.max_block_size, 2_000_000,
            "an untouched field must survive adoption unchanged"
        );
        assert!(
            sub.update.candidate_protocol_updates.is_empty(),
            "adoption must clear the candidate list"
        );
        assert!(sub.update.registered_protocol_update_proposals.is_empty());
        assert!(sub.update.confirmed_proposals.is_empty());
        assert!(sub.update.proposal_votes.is_empty());
        assert!(sub.update.registered_endorsements.is_empty());
        assert_eq!(
            sub.update.current_epoch, 0,
            "§2.3: current_epoch is NEVER touched by registerEpoch"
        );
    }

    #[test]
    fn register_proposal_rejects_non_genesis_proposer() {
        let g0 = byron_key_hash(&pk(0));
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(g0, g0);
        let mut update = ByronUpdateState {
            current_epoch: 0,
            adopted_protocol_version: (1, 0, 0),
            adopted_protocol_parameters: mainnet_genesis_params(),
            candidate_protocol_updates: Vec::new(),
            app_versions: BTreeMap::new(),
            registered_protocol_update_proposals: BTreeMap::new(),
            registered_software_update_proposals: BTreeMap::new(),
            confirmed_proposals: BTreeMap::new(),
            proposal_votes: BTreeMap::new(),
            registered_endorsements: BTreeSet::new(),
            proposal_registration_slot: BTreeMap::new(),
        };
        let proposal = ByronUpdProposal {
            up_id: Hash32::from_bytes([0xBB; 32]),
            encoded_len: 100,
            protocol_version: (1, 1, 0),
            params_update: ByronParamsUpdate {
                max_tx_size: Some(65_536),
                ..Default::default()
            },
            software_version: ("x".to_string(), 1),
            proposer_vk: pk(99), // not a genesis delegate
            body_span: Vec::new(),
            signature: Vec::new(),
        };
        // NotGenesisDelegate fires before the signature check (check 1 of
        // 2), so the body/signature above and `magic_cbor` below are never
        // read — garbage/empty is fine.
        let err =
            register_proposal(&mut update, &delegation_map_rev, 100, &proposal, &[]).unwrap_err();
        assert_eq!(err, ByronUpdateError::NotGenesisDelegate);
    }

    #[test]
    fn register_proposal_rejects_tx_fee_policy_override() {
        // #1092: the signature check (check 2) now runs BEFORE
        // `UnsupportedTxFeePolicyOverride` (check 3), so the proposer needs
        // a REAL keypair to reach the check this test targets.
        let g0_kp = TestKeypair::new(0);
        let g0 = byron_key_hash(&g0_kp.xpub);
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(g0, g0);
        let mut update = ByronUpdateState {
            current_epoch: 0,
            adopted_protocol_version: (1, 0, 0),
            adopted_protocol_parameters: mainnet_genesis_params(),
            candidate_protocol_updates: Vec::new(),
            app_versions: BTreeMap::new(),
            registered_protocol_update_proposals: BTreeMap::new(),
            registered_software_update_proposals: BTreeMap::new(),
            confirmed_proposals: BTreeMap::new(),
            proposal_votes: BTreeMap::new(),
            registered_endorsements: BTreeSet::new(),
            proposal_registration_slot: BTreeMap::new(),
        };
        let proposal = signed_proposal(
            &g0_kp,
            Hash32::from_bytes([0xCC; 32]),
            b"tx-fee-policy-override body span placeholder".to_vec(),
            (1, 1, 0),
            ByronParamsUpdate {
                tx_fee_policy: Some(vec![0x00]),
                ..Default::default()
            },
            ("x".to_string(), 1),
        );
        let err = register_proposal(
            &mut update,
            &delegation_map_rev,
            100,
            &proposal,
            &test_magic_cbor(),
        )
        .unwrap_err();
        assert_eq!(err, ByronUpdateError::UnsupportedTxFeePolicyOverride);
    }

    #[test]
    fn register_proposal_rejects_null_update() {
        // #1092: same reason as the tx-fee-policy test above — NullUpdate
        // (check 4) is now reached only after the signature check (check 2).
        let g0_kp = TestKeypair::new(0);
        let g0 = byron_key_hash(&g0_kp.xpub);
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(g0, g0);
        let params = mainnet_genesis_params();
        let mut update = ByronUpdateState {
            current_epoch: 0,
            adopted_protocol_version: (1, 0, 0),
            adopted_protocol_parameters: params,
            candidate_protocol_updates: Vec::new(),
            // Seeded so `software_version_is_new` correctly evaluates to
            // `false` — real operation never populates `app_versions` (see
            // `register_proposal`'s doc comment), so this test drives the
            // check's LOGIC directly rather than through the (currently
            // dormant) promotion path.
            app_versions: [("cardano-sl".to_string(), (0u32, 50u64))]
                .into_iter()
                .collect(),
            registered_protocol_update_proposals: BTreeMap::new(),
            registered_software_update_proposals: BTreeMap::new(),
            confirmed_proposals: BTreeMap::new(),
            proposal_votes: BTreeMap::new(),
            registered_endorsements: BTreeSet::new(),
            proposal_registration_slot: BTreeMap::new(),
        };
        // Same version, no parameter change, no new software version.
        let proposal = signed_proposal(
            &g0_kp,
            Hash32::from_bytes([0xDD; 32]),
            b"null-update body span placeholder".to_vec(),
            (1, 0, 0),
            ByronParamsUpdate::default(),
            ("cardano-sl".to_string(), 0),
        );
        let err = register_proposal(
            &mut update,
            &delegation_map_rev,
            100,
            &proposal,
            &test_magic_cbor(),
        )
        .unwrap_err();
        assert_eq!(err, ByronUpdateError::NullUpdate);
    }

    #[test]
    fn pv_can_follow_matches_haskell_rule() {
        assert!(pv_can_follow((1, 1, 0), (1, 0, 0)), "same major, minor+1");
        assert!(
            pv_can_follow((2, 0, 0), (1, 5, 0)),
            "major+1, minor reset to 0"
        );
        assert!(!pv_can_follow((1, 2, 0), (1, 0, 0)), "minor skipped a step");
        assert!(!pv_can_follow((3, 0, 0), (1, 0, 0)), "major skipped a step");
        assert!(
            !pv_can_follow((2, 1, 0), (1, 5, 0)),
            "major+1 must reset minor to 0"
        );
    }

    #[test]
    fn endorsement_from_unresolvable_key_is_silently_ignored() {
        // Endorsement.hs:210-218's own comment: an endorsement whose issuer
        // key does not resolve via `lookupR` is silently ignored, NOT an
        // error — but that branch is only reached once a MATCHING,
        // confirmed-and-stable proposal exists (`Endorsement.register`'s
        // `[] -> pure st` short-circuits before ever consulting the
        // delegation map at all). So this fixture must seed exactly that,
        // with an EMPTY reverse delegation map, to actually exercise the
        // silent-ignore path rather than the unrelated no-match path.
        let up_id = Hash32::from_bytes([0x11; 32]);
        let registered_protocol_update_proposals: BTreeMap<_, _> =
            [(up_id, ((1u16, 1u16, 0u8), mainnet_genesis_params()))]
                .into_iter()
                .collect();
        let mut confirmed_proposals = BTreeMap::new();
        confirmed_proposals.insert(up_id, 0); // confirmed well before current_slot -> stable
        let mut update = ByronUpdateState {
            current_epoch: 0,
            adopted_protocol_version: (1, 0, 0),
            adopted_protocol_parameters: mainnet_genesis_params(),
            candidate_protocol_updates: Vec::new(),
            app_versions: BTreeMap::new(),
            registered_protocol_update_proposals,
            registered_software_update_proposals: BTreeMap::new(),
            confirmed_proposals,
            proposal_votes: BTreeMap::new(),
            registered_endorsements: BTreeSet::new(),
            proposal_registration_slot: BTreeMap::new(),
        };
        let empty_rev = BTreeMap::new();
        // No panic, no error type to check — the point IS that nothing
        // observable happens: the endorsement is not recorded and no
        // candidate is created, even though a matching stable proposal
        // exists.
        register_endorsement(
            &mut update,
            &empty_rev,
            10_000,
            MAINNET_K,
            7,
            (1, 1, 0),
            byron_key_hash(&pk(0)),
        );
        assert!(update.registered_endorsements.is_empty());
        assert!(update.candidate_protocol_updates.is_empty());
    }

    // =========================================================================
    // #1092: signature verification
    // =========================================================================

    /// Mainnet's real protocol magic — the sign-tag `network` bytes for
    /// every test in this section that uses real mainnet genesis data.
    const MAINNET_MAGIC: u64 = 764_824_073;

    /// The design doc's named PRIMARY test vector (§6): "The 7 mainnet
    /// `heavyDelegation` genesis certificates — offline, in-repo today
    /// (`config/mainnet/byron-genesis.json`), epoch 0, canonical bytes."
    /// This is the one test in this file grounded in real on-chain data
    /// rather than a self-signed synthetic fixture: every one of these 7
    /// certificates was accepted by every Byron node that has ever synced
    /// mainnet, so a correct message construction MUST verify all 7 with no
    /// chain sync required.
    #[test]
    fn mainnet_genesis_heavy_delegation_certs_verify() {
        use base64::Engine;

        let path = std::path::Path::new("../../config/mainnet/byron-genesis.json");
        if !path.exists() {
            return; // skip if config files not available (matches genesis.rs's convention)
        }
        let content = std::fs::read_to_string(path).expect("read mainnet byron-genesis.json");
        let json: serde_json::Value =
            serde_json::from_str(&content).expect("parse mainnet byron-genesis.json");
        let heavy = json["heavyDelegation"]
            .as_object()
            .expect("heavyDelegation must be a JSON object");
        assert_eq!(
            heavy.len(),
            7,
            "mainnet has exactly 7 heavyDelegation certs"
        );

        let magic_cbor = network_magic_cbor(MAINNET_MAGIC);
        for (issuer_hex, entry) in heavy {
            let issuer_pk = base64::engine::general_purpose::STANDARD
                .decode(entry["issuerPk"].as_str().expect("issuerPk is a string"))
                .expect("issuerPk is valid base64");
            let delegate_pk = base64::engine::general_purpose::STANDARD
                .decode(
                    entry["delegatePk"]
                        .as_str()
                        .expect("delegatePk is a string"),
                )
                .expect("delegatePk is valid base64");
            let signature = hex::decode(entry["cert"].as_str().expect("cert is a string"))
                .expect("cert is valid hex");
            let omega = entry["omega"].as_u64().expect("omega is a u64");
            assert_eq!(
                omega, 0,
                "every real genesis heavyDelegation cert has omega=0 (issuer {issuer_hex})"
            );
            assert_eq!(issuer_pk.len(), 64, "issuerPk must be a 64-byte XPub");
            assert_eq!(delegate_pk.len(), 64, "delegatePk must be a 64-byte XPub");
            assert_eq!(signature.len(), 64, "cert must be a 64-byte signature");

            let mut epoch_bytes = Vec::new();
            minicbor::encode(omega, &mut epoch_bytes).expect("u64 encode cannot fail");

            assert!(
                verify_dlg_cert_signature(
                    &issuer_pk,
                    &delegate_pk,
                    &epoch_bytes,
                    &signature,
                    &magic_cbor,
                ),
                "mainnet genesis heavyDelegation cert for issuer {issuer_hex} must verify \
                 — if this fails, the message construction in `build_dlg_cert_message` \
                 disagrees with real on-chain data, not just a self-signed test fixture"
            );
        }
    }

    /// RED proof on the SAME real mainnet data as the test above: corrupting
    /// one signature byte of a genuinely-issued certificate must reject.
    #[test]
    fn mainnet_genesis_heavy_delegation_cert_corrupted_signature_is_rejected() {
        use base64::Engine;

        let path = std::path::Path::new("../../config/mainnet/byron-genesis.json");
        if !path.exists() {
            return;
        }
        let content = std::fs::read_to_string(path).expect("read mainnet byron-genesis.json");
        let json: serde_json::Value =
            serde_json::from_str(&content).expect("parse mainnet byron-genesis.json");
        let heavy = json["heavyDelegation"].as_object().unwrap();
        let (_, entry) = heavy
            .iter()
            .next()
            .expect("at least one heavyDelegation entry");

        let issuer_pk = base64::engine::general_purpose::STANDARD
            .decode(entry["issuerPk"].as_str().unwrap())
            .unwrap();
        let delegate_pk = base64::engine::general_purpose::STANDARD
            .decode(entry["delegatePk"].as_str().unwrap())
            .unwrap();
        let mut signature = hex::decode(entry["cert"].as_str().unwrap()).unwrap();
        signature[0] ^= 0x01; // corrupt one byte

        let mut epoch_bytes = Vec::new();
        minicbor::encode(0u64, &mut epoch_bytes).unwrap();
        let magic_cbor = network_magic_cbor(MAINNET_MAGIC);

        assert!(!verify_dlg_cert_signature(
            &issuer_pk,
            &delegate_pk,
            &epoch_bytes,
            &signature,
            &magic_cbor,
        ));
    }

    /// Design doc §3.2's exact worked example: "CBOR-bstr-header(2 + 64 +
    /// len(epochBytes)) -- e.g. `0x58 0x43` for epoch 0" — `2 + 64 + 1 =
    /// 67 = 0x43`, and 67 > 23 selects the one-byte-length form (`0x58`).
    /// Pinned literally against `build_dlg_cert_message`'s actual output,
    /// not just `cbor_bstr_header` in isolation.
    #[test]
    fn dlg_cert_message_bstr_header_matches_design_doc_epoch_0_example() {
        let mut epoch_bytes = Vec::new();
        minicbor::encode(0u64, &mut epoch_bytes).unwrap();
        assert_eq!(epoch_bytes, vec![0x00], "canonical CBOR uint 0 is one byte");

        let delegate_xpub = [0x11u8; 64];
        let magic_cbor = vec![0x01]; // preprod, arbitrary for this check
        let msg = build_dlg_cert_message(&delegate_xpub, &epoch_bytes, &magic_cbor);

        // 0x0A (SignCertificate) ‖ magic(1) ‖ 0x58 0x43 (bstr header) ‖ payload(67)
        assert_eq!(msg[0], 0x0A);
        assert_eq!(&msg[1..2], &magic_cbor[..]);
        assert_eq!(&msg[2..4], &[0x58, 0x43]);
        assert_eq!(msg.len(), 1 + 1 + 2 + 67);
        // payload = "00" ‖ delegate_xpub ‖ epoch_bytes
        assert_eq!(&msg[4..6], b"00");
        assert_eq!(&msg[6..70], &delegate_xpub[..]);
        assert_eq!(&msg[70..], &epoch_bytes[..]);
    }

    /// Design doc §1.2's exact claimed byte sequences for the sign-tag
    /// `network` bytes — pinned literally, not just spot-checked via a
    /// round-trip, since a wrong-but-self-consistent encoding could pass
    /// every other test in this file.
    #[test]
    fn network_magic_cbor_matches_design_doc_examples() {
        assert_eq!(
            network_magic_cbor(764_824_073), // mainnet
            vec![0x1A, 0x2D, 0x96, 0x4A, 0x09]
        );
        assert_eq!(network_magic_cbor(1), vec![0x01]); // preprod
        assert_eq!(network_magic_cbor(2), vec![0x02]); // preview
    }

    // -------------------------------------------------------------------------
    // Synthetic RED/GREEN matrix: each of the four signed surfaces, each
    // component of its message independently corrupted. The mainnet test
    // above proves the message format matches real Byron on-chain data;
    // these prove the WIRING (which bytes each `verify_*` function reads and
    // in what role) is right, including surfaces mainnet's genesis file
    // carries no example of (proposals, votes, block signatures).
    // -------------------------------------------------------------------------

    #[test]
    fn verify_dlg_cert_signature_accepts_genuine_rejects_each_corruption() {
        let issuer = TestKeypair::new(1);
        let delegate = TestKeypair::new(2);
        let other = TestKeypair::new(3);
        let magic_cbor = test_magic_cbor();
        let cert = signed_dlg_cert(&issuer, &delegate.xpub, 0);

        assert!(verify_dlg_cert_signature(
            &cert.issuer_vk,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &cert.signature,
            &magic_cbor,
        ));

        // Wrong verifying key.
        assert!(!verify_dlg_cert_signature(
            &other.xpub,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &cert.signature,
            &magic_cbor,
        ));
        // Wrong delegate (a message component).
        assert!(!verify_dlg_cert_signature(
            &cert.issuer_vk,
            &other.xpub,
            &cert.epoch_raw,
            &cert.signature,
            &magic_cbor,
        ));
        // Wrong epoch bytes (a message component).
        let mut wrong_epoch = Vec::new();
        minicbor::encode(1u64, &mut wrong_epoch).unwrap();
        assert!(!verify_dlg_cert_signature(
            &cert.issuer_vk,
            &cert.delegate_vk,
            &wrong_epoch,
            &cert.signature,
            &magic_cbor,
        ));
        // Corrupted signature.
        let mut bad_sig = cert.signature.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!verify_dlg_cert_signature(
            &cert.issuer_vk,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &bad_sig,
            &magic_cbor,
        ));
        // Wrong network magic (the message's tag suffix).
        assert!(!verify_dlg_cert_signature(
            &cert.issuer_vk,
            &cert.delegate_vk,
            &cert.epoch_raw,
            &cert.signature,
            &network_magic_cbor(MAINNET_MAGIC),
        ));
    }

    #[test]
    fn verify_proposal_signature_accepts_genuine_rejects_each_corruption() {
        let proposer = TestKeypair::new(4);
        let other = TestKeypair::new(5);
        let body_span = b"real proposal body span bytes".to_vec();
        let magic_cbor = test_magic_cbor();
        let proposal = signed_proposal(
            &proposer,
            Hash32::from_bytes([0x22; 32]),
            body_span.clone(),
            (1, 1, 0),
            ByronParamsUpdate::default(),
            ("x".to_string(), 1),
        );

        assert!(verify_proposal_signature(
            &proposal.proposer_vk,
            &proposal.body_span,
            &proposal.signature,
            &magic_cbor,
        ));
        assert!(!verify_proposal_signature(
            &other.xpub,
            &proposal.body_span,
            &proposal.signature,
            &magic_cbor,
        ));
        assert!(!verify_proposal_signature(
            &proposal.proposer_vk,
            b"a different body span",
            &proposal.signature,
            &magic_cbor,
        ));
        let mut bad_sig = proposal.signature.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!verify_proposal_signature(
            &proposal.proposer_vk,
            &proposal.body_span,
            &bad_sig,
            &magic_cbor,
        ));
        assert!(!verify_proposal_signature(
            &proposal.proposer_vk,
            &proposal.body_span,
            &proposal.signature,
            &network_magic_cbor(MAINNET_MAGIC),
        ));
    }

    #[test]
    fn verify_vote_signature_accepts_genuine_rejects_each_corruption() {
        let voter = TestKeypair::new(6);
        let other = TestKeypair::new(7);
        let up_id = Hash32::from_bytes([0x33; 32]);
        let other_up_id = Hash32::from_bytes([0x44; 32]);
        let magic_cbor = test_magic_cbor();
        let vote = signed_vote(&voter, up_id);

        assert!(verify_vote_signature(
            &vote.voter_vk,
            &vote.proposal_id_raw,
            &vote.signature,
            &magic_cbor,
        ));
        assert!(!verify_vote_signature(
            &other.xpub,
            &vote.proposal_id_raw,
            &vote.signature,
            &magic_cbor,
        ));
        // A vote signed for a DIFFERENT proposal id must not verify against
        // this one's raw bytes.
        let other_vote = signed_vote(&voter, other_up_id);
        assert!(!verify_vote_signature(
            &vote.voter_vk,
            &other_vote.proposal_id_raw,
            &vote.signature,
            &magic_cbor,
        ));
        let mut bad_sig = vote.signature.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!verify_vote_signature(
            &vote.voter_vk,
            &vote.proposal_id_raw,
            &bad_sig,
            &magic_cbor,
        ));
        assert!(!verify_vote_signature(
            &vote.voter_vk,
            &vote.proposal_id_raw,
            &vote.signature,
            &network_magic_cbor(MAINNET_MAGIC),
        ));
    }

    /// Build a signed `ByronBlockAux` fixture for [`verify_block_signature`]:
    /// `genesis` is the header's CLAIMED (unauthenticated) genesis key,
    /// `delegate` is the key that actually signs. Mirrors
    /// `read_byron_block_sig`'s decode-time construction of
    /// `block_signed_bytes` (design doc §2.2) with an arbitrary but fixed
    /// payload, since these tests never decode real wire bytes.
    fn signed_block_aux(genesis: &TestKeypair, delegate: &TestKeypair) -> ByronBlockAux {
        let magic_cbor = test_magic_cbor();
        let mut block_signed_bytes = vec![0x85u8];
        block_signed_bytes.extend_from_slice(b"prev_hash+body_proof+slot_id+difficulty+extra");
        let tag = dugite_crypto::byron::sign_tag_block(&genesis.xpub, &magic_cbor);
        let mut msg = tag;
        msg.extend_from_slice(&block_signed_bytes);
        let block_signature = delegate.sign(&msg);
        ByronBlockAux {
            protocol_version: (1, 0, 0),
            issuer_pubkey: genesis.xpub.to_vec(),
            delegate_pubkey: delegate.xpub.to_vec(),
            block_signature,
            block_signed_bytes,
            dlg_certs: Vec::new(),
            upd_proposal: None,
            upd_votes: Vec::new(),
        }
    }

    #[test]
    fn verify_block_signature_accepts_genuine_registered_delegate() {
        let genesis = TestKeypair::new(8);
        let delegate = TestKeypair::new(9);
        let aux = signed_block_aux(&genesis, &delegate);
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(
            byron_key_hash(&delegate.xpub),
            byron_key_hash(&genesis.xpub),
        );

        verify_block_signature(&aux, TEST_MAGIC, &delegation_map_rev, 1000)
            .expect("genuine signature + registered delegate must verify");
    }

    #[test]
    fn verify_block_signature_rejects_corrupted_signature() {
        let genesis = TestKeypair::new(8);
        let delegate = TestKeypair::new(9);
        let mut aux = signed_block_aux(&genesis, &delegate);
        aux.block_signature[0] ^= 0xFF;
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(
            byron_key_hash(&delegate.xpub),
            byron_key_hash(&genesis.xpub),
        );

        let err = verify_block_signature(&aux, TEST_MAGIC, &delegation_map_rev, 1000).unwrap_err();
        assert!(matches!(err, LedgerError::ByronSignatureInvalid { .. }));
    }

    #[test]
    fn verify_block_signature_rejects_tampered_signed_bytes() {
        let genesis = TestKeypair::new(8);
        let delegate = TestKeypair::new(9);
        let mut aux = signed_block_aux(&genesis, &delegate);
        // Tamper with the header content the signature covers, WITHOUT
        // re-signing — simulates a body swapped under a genuine signature.
        aux.block_signed_bytes[5] ^= 0xFF;
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(
            byron_key_hash(&delegate.xpub),
            byron_key_hash(&genesis.xpub),
        );

        let err = verify_block_signature(&aux, TEST_MAGIC, &delegation_map_rev, 1000).unwrap_err();
        assert!(matches!(err, LedgerError::ByronSignatureInvalid { .. }));
    }

    #[test]
    fn verify_block_signature_rejects_unregistered_delegate() {
        // Signature verifies fine, but the delegate never appears in the
        // ledger's activation map — design doc §2.1 step 3.
        let genesis = TestKeypair::new(8);
        let delegate = TestKeypair::new(9);
        let aux = signed_block_aux(&genesis, &delegate);
        let empty_delegation_map_rev = BTreeMap::new();

        let err =
            verify_block_signature(&aux, TEST_MAGIC, &empty_delegation_map_rev, 1000).unwrap_err();
        assert!(matches!(err, LedgerError::ByronSignatureInvalid { .. }));
    }

    #[test]
    fn verify_block_signature_rejects_wrong_network_magic() {
        let genesis = TestKeypair::new(8);
        let delegate = TestKeypair::new(9);
        let aux = signed_block_aux(&genesis, &delegate);
        let mut delegation_map_rev = BTreeMap::new();
        delegation_map_rev.insert(
            byron_key_hash(&delegate.xpub),
            byron_key_hash(&genesis.xpub),
        );

        // Signed under TEST_MAGIC; verified against mainnet's magic — the
        // tag differs, so the message differs, so it must reject.
        let err =
            verify_block_signature(&aux, MAINNET_MAGIC, &delegation_map_rev, 1000).unwrap_err();
        assert!(matches!(err, LedgerError::ByronSignatureInvalid { .. }));
    }
}
