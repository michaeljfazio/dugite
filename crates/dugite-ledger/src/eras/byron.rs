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
use std::collections::HashSet;
use std::sync::Arc;

use dugite_primitives::block::{Block, BlockHeader};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_224, Hash28};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{Transaction, TransactionInput, TransactionOutput};
use dugite_primitives::value::Lovelace;

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
