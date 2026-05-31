/// Shelley era ledger rules (covers Shelley, Allegra, and Mary eras).
///
/// Shelley (protocol version 2) introduces:
/// - Ouroboros Praos consensus (VRF-based leader election)
/// - Staking and delegation (stake credentials, pool registration)
/// - Reward distribution (monetary expansion, pool rewards)
/// - Multi-signature scripts
///
/// Allegra (protocol version 3) adds:
/// - Transaction validity intervals (valid_from / ttl)
/// - Timelock script primitives
///
/// Mary (protocol version 4) adds:
/// - Multi-asset (native tokens in UTxO outputs)
/// - Minting/burning policies
///
/// The core ledger pipeline (UTXOW/UTXO/DELEG/POOL rules) is shared
/// across all three eras, so a single `ShelleyRules` implementation covers
/// them all. The differences are in transaction body fields and script
/// capabilities, not in the LEDGER rule application order.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::address::Address;
use dugite_primitives::block::{Block, BlockHeader};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{Certificate, Transaction, TransactionInput};
use dugite_primitives::value::Lovelace;
use tracing::{debug, info};

use super::common;
use super::{EraRules, RuleContext};
use crate::state::substates::*;
use crate::state::{BlockValidationMode, LedgerError, StakeSnapshot};
use crate::utxo_diff::UtxoDiff;

/// Stateless Shelley/Allegra/Mary era rule strategy.
///
/// This struct carries no mutable state. All state lives in the component
/// sub-states passed as parameters to each method.
#[derive(Default, Debug, Clone, Copy)]
pub struct ShelleyRules;

impl ShelleyRules {
    pub fn new() -> Self {
        ShelleyRules
    }
}

// ---------------------------------------------------------------------------
// Shelley → Allegra hard-fork state transformation
// ---------------------------------------------------------------------------

/// Implements `returnRedeemAddrsToReserves` from
/// `Cardano.Ledger.Allegra.Translation` (cardano-ledger).
///
/// Scans every live UTxO entry.  Any entry whose address is a Byron
/// bootstrap address with `addr_type == 2` (AVVM / ATRedeem) is removed
/// from the UTxO set and its coin is accumulated into reserves.
///
/// Haskell reference (verbatim):
/// ```haskell
/// returnRedeemAddrsToReserves es = es { esChainAccountState = acnt'
///                                     , esLState = ls' }
///   where
///     UTxO utxo = utxosUtxo us
///     (redeemers, nonredeemers) =
///       Map.partition
///         (maybe False isBootstrapRedeemer . view bootAddrTxOutF)
///         utxo
///     acnt' = acnt { casReserves = casReserves acnt <+> sumCoinUTxO (UTxO redeemers) }
///     us'   = us   { utxosUtxo   = UTxO nonredeemers }
/// ```
///
/// `isBootstrapRedeemer addr = addrType addr == ATRedeem`
/// (`Cardano.Chain.Common.AddrType` — value `2` in the CBOR encoding).
///
/// `sumCoinUTxO` sums the `coin` field only (lovelace); multi-asset UTxOs
/// did not exist in the Shelley era so this is equivalent to summing the
/// entire value.
fn return_redeem_addrs_to_reserves(utxo: &mut UtxoSubState, epochs: &mut EpochSubState) {
    // Phase 1: scan the full UTxO set and collect every redeem-address input.
    // We cannot mutate the UTxO set while scanning (borrow-checker), so we
    // collect the inputs first, then remove in a second pass.
    let mut redeem_inputs: Vec<TransactionInput> = Vec::new();
    let mut redeem_coin: u64 = 0;

    utxo.utxo_set.scan_all(|input, output| {
        let is_redeem = matches!(&output.address, Address::Byron(b) if b.is_redeem());
        if is_redeem {
            redeem_inputs.push(input.clone());
            // sumCoinUTxO: add the coin (lovelace) component only.
            // All Shelley-era outputs are ADA-only so value.coin == full value.
            redeem_coin = redeem_coin.saturating_add(output.value.coin.0);
        }
    });

    let redeem_count = redeem_inputs.len();

    // Phase 2: remove them from the UTxO set.
    for input in &redeem_inputs {
        utxo.utxo_set.remove(input);
    }

    // Phase 3: credit the total coin to reserves.
    // Haskell: casReserves acnt <+> sumCoinUTxO (UTxO redeemers)
    // `<+>` is `Coin` addition (saturating).
    epochs.reserves = Lovelace(epochs.reserves.0.saturating_add(redeem_coin));

    let redeem_ada = redeem_coin / 1_000_000;
    info!(
        redeem_utxo_count = redeem_count,
        redeem_coin_lovelace = redeem_coin,
        redeem_coin_ada = redeem_ada,
        reserves_after = epochs.reserves.0,
        "Shelley→Allegra: returnRedeemAddrsToReserves — purged AVVM redeem UTxOs, \
         credited coin to reserves",
    );
}

impl EraRules for ShelleyRules {
    /// Shelley/Allegra/Mary have no ExUnit budgets or reference scripts.
    ///
    /// Block body validation is trivially successful — Plutus scripts were
    /// not introduced until the Alonzo era.
    fn validate_block_body(
        &self,
        _block: &Block,
        _ctx: &RuleContext,
        _utxo: &UtxoSubState,
    ) -> Result<(), LedgerError> {
        Ok(())
    }

    /// Apply a single valid Shelley/Allegra/Mary transaction.
    ///
    /// Implements the Shelley LEDGER rule pipeline:
    /// 1. Drain withdrawal accounts (zero reward balances consumed by tx).
    /// 2. Process Shelley-era certificates (registrations, delegations, pools).
    /// 3. Apply UTxO changes (consume inputs, produce outputs, accumulate fee).
    ///
    /// In Shelley/Allegra/Mary ALL transactions are valid — there is no
    /// `is_valid` flag. The IsValid concept was introduced in Alonzo.
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
        // Per the Cardano spec, the withdrawal amount must exactly equal the
        // reward balance. During sync we may not have accumulated all rewards,
        // so mismatches are logged at DEBUG level (best-effort).
        common::drain_withdrawal_accounts(tx, certs);

        // Step 2: Process certificates (StakeReg, StakeDeReg, Delegation,
        // PoolRegistration, PoolRetirement).
        // The tx_index is derived from the slot — callers set this to the
        // transaction's position within the block. For the era-rules interface
        // we use 0 since the orchestrator (Task 12) will provide the correct
        // index. Certificate pointer map entries are not critical for correctness
        // (only used for pointer address resolution in snapshots).
        common::process_shelley_certs(tx, ctx.current_slot, ctx.tx_index, certs, epochs, gov);

        // Step 3: Apply UTxO changes (consume inputs, produce outputs).
        let diff = common::apply_utxo_changes(tx, utxo, certs, epochs);

        Ok(diff)
    }

    /// Shelley/Allegra/Mary have no IsValid concept.
    ///
    /// All transactions in these eras are structurally valid or rejected
    /// outright. Calling this method for a Shelley-era transaction is a
    /// programming error — the IsValid=false path was introduced in Alonzo
    /// with Plutus Phase-2 evaluation.
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
            "Shelley/Allegra/Mary eras do not support invalid transactions \
             (is_valid flag). Transaction {} should not reach apply_invalid_tx.",
            tx.hash.to_hex()
        )))
    }

    /// Process a Shelley/Allegra/Mary epoch boundary transition.
    ///
    /// Implements the pre-Conway subset of Haskell's NEWEPOCH STS rule:
    ///
    /// 1. Flush pending treasury donations.
    /// 2. Apply pending reward update (legacy compatibility).
    /// 3. Rotate snapshots (mark -> set -> go) and take new mark snapshot.
    /// 4. Apply future pool parameter updates (re-registrations).
    /// 5. Process pool retirements for this epoch.
    /// 6. Apply pre-Conway PP update proposals (genesis key votes).
    /// 7. Recalculate totalObligation from scratch.
    /// 8. Compute new epoch nonce (TICKN rule).
    /// 9. Reset per-epoch accumulators (fees, block counters).
    ///
    /// NOTE: Reward calculation (`calculate_rewards_full`) is NOT performed
    /// here because it requires access to the full `LedgerState` (for reading
    /// total ADA supply, protocol params, etc.). The existing `LedgerState::
    /// process_epoch_transition()` handles rewards. When the orchestrator
    /// (Task 12) wires era rules, it will need to either:
    /// - Continue using the LedgerState method for reward calculation, or
    /// - Extract reward calculation into a standalone function.
    ///
    /// This implementation covers all non-reward epoch transition operations
    /// faithfully, matching the Haskell ordering.
    fn process_epoch_transition(
        &self,
        new_epoch: EpochNo,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        gov: &mut GovSubState,
        epochs: &mut EpochSubState,
        consensus: &mut ConsensusSubState,
    ) -> Result<(), LedgerError> {
        debug!("Shelley epoch transition: -> {}", new_epoch.0);

        // Capture bprev BEFORE any param updates (nesBprev = nesBcur).
        let bprev_block_count = consensus.epoch_block_count;
        let bprev_blocks_by_pool = Arc::clone(&consensus.epoch_blocks_by_pool);

        // Step 0: Flush pending treasury donations.
        if utxo.pending_donations.0 > 0 {
            let flushed = utxo.pending_donations;
            epochs.treasury.0 = epochs
                .treasury
                .0
                .checked_add(flushed.0)
                .expect("treasury overflow on pending-donations flush");
            utxo.pending_donations = Lovelace(0);
            debug!(
                epoch = new_epoch.0,
                donations_lovelace = flushed.0,
                "Flushed pending treasury donations"
            );
        }

        // Step 1: Apply pending reward update (backward compat for old snapshots).
        if let Some(rupd) = epochs.pending_reward_update.take() {
            epochs.reserves.0 = epochs
                .reserves
                .0
                .checked_sub(rupd.delta_reserves)
                .expect("RUPD delta_reserves exceeds reserves — ledger invariant broken");
            epochs.treasury.0 = epochs
                .treasury
                .0
                .checked_add(rupd.delta_treasury)
                .expect("RUPD delta_treasury overflows treasury u64");
            for (cred_hash, reward) in &rupd.rewards {
                if reward.0 > 0 {
                    if certs.reward_accounts.contains_key(cred_hash) {
                        *Arc::make_mut(&mut certs.reward_accounts)
                            .entry(*cred_hash)
                            .or_insert(Lovelace(0)) += *reward;
                    } else {
                        epochs.treasury.0 = epochs
                            .treasury
                            .0
                            .checked_add(reward.0)
                            .expect("treasury overflow on undistributed reward");
                    }
                }
            }
            // Capture for post-boundary debug dumpers (see
            // `epoch_state_debug::maybe_dump`).  Mirror in Conway path.
            epochs.last_applied_rupd = Some(rupd);
        } else {
            // Clear any stale entry from a previous boundary so dumpers do
            // not double-report.
            epochs.last_applied_rupd = None;
        }

        // Step 2: Compute and apply RUPD using GO snapshot + bprev + ss_fee.
        //
        // Issue #438: fire RUPD unconditionally at every boundary, even at
        // boundary 0→1 when GO/bprev/ss_fee are still empty.  Haskell's
        // `startStep` runs mid-epoch starting in epoch 0 (it produces a
        // `RewardUpdate` with `ssFee = 0` from `emptySnapShots`), and
        // `applyRUpd` applies that RewardUpdate at boundary 0→1 — draining
        // the genesis monetary expansion's tau cut from reserves to
        // treasury (~9M ADA on preview).  Previously gating on
        // `rupd_ready` left dugite with that 9M ADA in reserves instead of
        // treasury, which compounded geometrically into +4.887M ADA
        // reserves excess by preview epoch 1269 and a +25K-lovelace per-pool
        // reward overshoot at every subsequent boundary.
        {
            let go_ref = epochs.snapshots.go.as_ref();
            // Issue #438: RUPD uses Haskell's `prevPParams` (= the protocol
            // parameters that were active in the PREVIOUS epoch), NOT
            // `curPParams`.  When `n_opt`, `a0`, `rho`, `tau`, `asc`, or
            // `protocolVersion` changes via PPUP/governance, the change
            // takes effect at this boundary as `curPParams` but Haskell's
            // pulser already ran during the just-ending epoch with the
            // pre-change values.  Cross-validated against cardano-node
            // `cardano-cli debug log-epoch-state` at preview boundary 9→10
            // where n_opt change 150→500 caused dugite max_pool to shrink
            // by ratio 0.728, missing 60.679K ADA per boundary.
            let rupd_pp = &epochs.prev_protocol_params;
            let rupd = crate::compute_reward_update(
                rupd_pp,
                &epochs.prev_d,
                epochs.prev_protocol_version_major,
                go_ref,
                &epochs.snapshots.bprev_blocks_by_pool,
                epochs.snapshots.ss_fee,
                epochs.reserves,
                epochs.treasury,
                &certs.reward_accounts,
                ctx.epoch_length,
                ctx.shelley_transition_epoch,
                ctx.max_lovelace_supply,
            );

            // Issue #438/#471: per-boundary reward-debug dump.  No-op
            // unless the crate is built with `--features
            // reward-debug-dump` AND `DUGITE_REWARD_DEBUG_DUMP=<dir>` is
            // set at runtime.  Only fires when GO is non-empty.
            #[cfg(feature = "reward-debug-dump")]
            if let Some(go) = go_ref {
                crate::state::reward_debug::maybe_dump(
                    ctx.current_epoch.0,
                    new_epoch.0,
                    rupd_pp,
                    &epochs.prev_d,
                    epochs.prev_protocol_version_major,
                    epochs.reserves,
                    epochs.treasury,
                    epochs.snapshots.ss_fee,
                    go,
                    &epochs.snapshots.bprev_blocks_by_pool,
                    &certs.reward_accounts,
                    &rupd,
                );
            }

            // Apply RUPD: adjust reserves and treasury
            epochs.reserves.0 = epochs
                .reserves
                .0
                .checked_sub(rupd.delta_reserves)
                .expect("RUPD delta_reserves exceeds reserves — ledger invariant broken");
            epochs.treasury.0 = epochs
                .treasury
                .0
                .checked_add(rupd.delta_treasury)
                .expect("RUPD delta_treasury overflows treasury u64");

            // Distribute rewards to registered accounts; unregistered → treasury
            for (cred_hash, reward) in &rupd.rewards {
                if reward.0 > 0 {
                    if certs.reward_accounts.contains_key(cred_hash) {
                        *Arc::make_mut(&mut certs.reward_accounts)
                            .entry(*cred_hash)
                            .or_insert(Lovelace(0)) += *reward;
                    } else {
                        epochs.treasury.0 = epochs
                            .treasury
                            .0
                            .checked_add(reward.0)
                            .expect("treasury overflow on undistributed reward");
                    }
                }
            }

            // #615d: expose the freshly-applied RUPD to the epoch-state dumper.
            // Without this, `epoch_state_debug::rewards_summary` sees `None` and
            // emits `total_distributed = 0`, masking the real per-pool payout.
            epochs.last_applied_rupd = Some(rupd);
        }

        // Issue #670: drain `utxo.epoch_fees` by the same `ssFee` that the
        // RUPD just consumed. Haskell `applyRUpd` does
        // `lsUTxOStateL . utxosFeesL %~ (`addDeltaCoin` deltaF ru)` where
        // `deltaF = -ssFee` (the snapshot fee captured at startStep). The
        // residual stays in `utxosFees` and only the current `ssFee` portion
        // leaves — `utxosFees` is therefore a multi-epoch running total, not
        // a per-epoch counter. Mirror the drain here so dugite's
        // `epoch_fees` reaches the same final value as the Haskell ancillary
        // import at the same anchor (verify-ledger-snapshot field
        // `epoch_fees`). The reset to zero at the end of the function is
        // removed below.
        let ss_fee_drained = epochs.snapshots.ss_fee;
        utxo.epoch_fees = Lovelace(utxo.epoch_fees.0.saturating_sub(ss_fee_drained.0));

        // Step 3: SNAP — rotate snapshots, capture fees, update bprev.
        //
        // Per Haskell SNAP rule: `ssFee = utxosFees` of the **post-applyRUpd**
        // state. We already applied RUPD and drained above, so `epoch_fees`
        // is the post-drain residual; capturing it here matches Haskell's
        // ordering exactly.
        let captured_fees = utxo.epoch_fees;
        epochs.snapshots.go = epochs.snapshots.set.take();
        epochs.snapshots.set = epochs.snapshots.mark.take();
        epochs.snapshots.ss_fee = captured_fees;
        epochs.snapshots.bprev_block_count = bprev_block_count;
        epochs.snapshots.bprev_blocks_by_pool = bprev_blocks_by_pool;
        epochs.snapshots.rupd_ready = true;

        // Rebuild stake distribution if needed (post-Mithril import).
        if epochs.needs_stake_rebuild {
            // Full UTxO rebuild requires access to the UTxO set and is typically
            // done by the orchestrator. Mark as no longer needed so subsequent
            // boundaries use incremental tracking.
            epochs.needs_stake_rebuild = false;
            debug!(
                epoch = new_epoch.0,
                "Shelley epoch: needs_stake_rebuild flag cleared (rebuild deferred to orchestrator)"
            );
        }

        // Build pool_stake from current stake distribution + delegations.
        let mut pool_stake: HashMap<Hash28, Lovelace> =
            HashMap::with_capacity(certs.pool_params.len());
        for (cred_hash, pool_id) in certs.delegations.iter() {
            let utxo_stake = certs
                .stake_distribution
                .stake_map
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let reward_balance = certs
                .reward_accounts
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let total_stake = Lovelace(utxo_stake.0 + reward_balance.0);
            *pool_stake.entry(*pool_id).or_insert(Lovelace(0)) += total_stake;
        }

        // Resolve deferred pointer-addressed UTxO stake at SNAP time.
        if !epochs.ptr_stake.is_empty() {
            for (pointer, &coin) in &epochs.ptr_stake {
                if coin == 0 {
                    continue;
                }
                if let Some(cred_hash) = certs.pointer_map.get(pointer) {
                    if certs.reward_accounts.contains_key(cred_hash) {
                        if let Some(pool_id) = certs.delegations.get(cred_hash) {
                            *pool_stake.entry(*pool_id).or_insert(Lovelace(0)) += Lovelace(coin);
                        }
                    }
                }
            }
        }

        // Build per-credential snapshot_stake (only delegated credentials).
        let mut snapshot_stake: HashMap<Hash32, Lovelace> =
            HashMap::with_capacity(certs.delegations.len());
        for cred_hash in certs.delegations.keys() {
            let utxo_stake = certs
                .stake_distribution
                .stake_map
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let reward_balance = certs
                .reward_accounts
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let total = Lovelace(utxo_stake.0.saturating_add(reward_balance.0));
            if total.0 > 0 {
                snapshot_stake.insert(*cred_hash, total);
            }
        }

        // Resolve pointer-addressed UTxO coins into per-credential snapshot_stake.
        if !epochs.ptr_stake.is_empty() {
            for (pointer, &coin) in &epochs.ptr_stake {
                if coin == 0 {
                    continue;
                }
                if let Some(cred_hash) = certs.pointer_map.get(pointer) {
                    if certs.reward_accounts.contains_key(cred_hash)
                        && certs.delegations.contains_key(cred_hash)
                    {
                        *snapshot_stake.entry(*cred_hash).or_insert(Lovelace(0)) += Lovelace(coin);
                    }
                }
            }
        }

        // Create the new mark snapshot.
        epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: new_epoch,
            delegations: Arc::clone(&certs.delegations),
            pool_stake,
            pool_params: Arc::clone(&certs.pool_params),
            stake_distribution: Arc::new(snapshot_stake),
            epoch_fees: utxo.epoch_fees,
            epoch_block_count: consensus.epoch_block_count,
            epoch_blocks_by_pool: Arc::clone(&consensus.epoch_blocks_by_pool),
        });

        // Apply future pool parameters (re-registrations deferred from previous epoch).
        if !certs.future_pool_params.is_empty() {
            let pool_params = Arc::make_mut(&mut certs.pool_params);
            for (pool_id, pool_reg) in certs.future_pool_params.drain() {
                if pool_params.contains_key(&pool_id) {
                    pool_params.insert(pool_id, pool_reg);
                }
                // Pools only in future (retired between re-reg and boundary): dropped.
            }
        }

        // Process pending pool retirements for this epoch.
        let retiring_pools: Vec<Hash28> = certs
            .pending_retirements
            .iter()
            .filter_map(|(pool_id, epoch)| {
                if *epoch == new_epoch {
                    Some(*pool_id)
                } else {
                    None
                }
            })
            .collect();
        if !retiring_pools.is_empty() {
            for pool_id in &retiring_pools {
                certs.pending_retirements.remove(pool_id);
            }
            for pool_id in &retiring_pools {
                if let Some(pool_reg) = Arc::make_mut(&mut certs.pool_params).remove(pool_id) {
                    let pool_deposit = certs
                        .pool_deposits
                        .remove(pool_id)
                        .map(Lovelace)
                        .unwrap_or(epochs.protocol_params.pool_deposit);
                    let op_key = reward_account_to_hash(&pool_reg.reward_account);
                    if certs.reward_accounts.contains_key(&op_key) {
                        *Arc::make_mut(&mut certs.reward_accounts)
                            .entry(op_key)
                            .or_insert(Lovelace(0)) += pool_deposit;
                    } else {
                        epochs.treasury.0 = epochs
                            .treasury
                            .0
                            .checked_add(pool_deposit.0)
                            .expect("treasury overflow on pool deposit refund");
                    }
                    Arc::make_mut(&mut certs.delegations)
                        .retain(|_, delegated_pool| delegated_pool != pool_id);
                    debug!(
                        "Pool retired at epoch {}: {} (deposit {} refunded)",
                        new_epoch.0,
                        pool_id.to_hex(),
                        pool_deposit.0
                    );
                }
            }
        }
        // Clean up retirements from past epochs.
        certs
            .pending_retirements
            .retain(|_, epoch| *epoch > new_epoch);

        // MIR rule (Haskell EPOCH ordering: SNAP → POOLREAP → MIR → NEWPP).
        // Drain pending `dsIRewards` map + pot-transfer accumulators into
        // reward_accounts + reserves/treasury per
        // `Cardano.Ledger.Shelley.Rules.Mir.applyMIR`.  See issue #631.
        crate::state::certificates::apply_pending_mir(certs, epochs);

        // Capture prevPParams BEFORE PPUP updates.
        //
        // Post-Babbage (PV >= 7) `d` is conceptually 0 (no overlay slots —
        // Babbage drops `ppDG` from PParams). dugite carries a flat
        // `ProtocolParameters` across all eras, so we synthesise the
        // post-Babbage Rational zero here.  Pre-Babbage we keep the exact
        // rational from PParams — never go through f64 (issue #629).
        let old_d = if epochs.protocol_params.protocol_version_major >= 7 {
            dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            }
        } else {
            epochs.protocol_params.d.clone()
        };
        let old_proto_major = epochs.protocol_params.protocol_version_major;
        let old_params = epochs.protocol_params.clone();

        // Apply pre-Conway PP update proposals (PPUP/UPEC rule).
        let lookup_epoch = EpochNo(new_epoch.0.saturating_sub(1));
        if let Some(proposals) = epochs.pending_pp_updates.remove(&lookup_epoch) {
            let mut proposer_set: HashSet<Hash32> = HashSet::with_capacity(proposals.len());
            for (genesis_hash, _) in &proposals {
                proposer_set.insert(*genesis_hash);
            }
            let distinct_proposers = proposer_set.len() as u64;

            if distinct_proposers >= ctx.update_quorum {
                // Merge all proposals.
                let mut merged = dugite_primitives::transaction::ProtocolParamUpdate::default();
                for (_, ppu) in &proposals {
                    macro_rules! merge_field {
                        ($field:ident) => {
                            if ppu.$field.is_some() {
                                merged.$field = ppu.$field.clone();
                            }
                        };
                    }
                    merge_field!(min_fee_a);
                    merge_field!(min_fee_b);
                    merge_field!(max_block_body_size);
                    merge_field!(max_tx_size);
                    merge_field!(max_block_header_size);
                    merge_field!(key_deposit);
                    merge_field!(pool_deposit);
                    merge_field!(e_max);
                    merge_field!(n_opt);
                    merge_field!(a0);
                    merge_field!(rho);
                    merge_field!(tau);
                    merge_field!(d);
                    merge_field!(extra_entropy);
                    merge_field!(min_pool_cost);
                    merge_field!(ada_per_utxo_byte);
                    merge_field!(cost_models);
                    merge_field!(execution_costs);
                    merge_field!(max_tx_ex_units);
                    merge_field!(max_block_ex_units);
                    merge_field!(max_val_size);
                    merge_field!(collateral_percentage);
                    merge_field!(max_collateral_inputs);
                    merge_field!(protocol_version_major);
                    merge_field!(protocol_version_minor);
                }
                // Apply the merged update to protocol params.
                apply_pp_update(&mut epochs.protocol_params, &merged);
                // ppExtraEntropy is consumed only by the epoch-nonce TICKN, so
                // it lives in the consensus sub-state rather than
                // ProtocolParameters. Sticky: only an update that carries the
                // field changes it (Some(ZERO) explicitly resets to neutral).
                if let Some(extra) = merged.extra_entropy {
                    consensus.extra_entropy = extra;
                }
                debug!(
                    epoch = new_epoch.0,
                    proposers = distinct_proposers,
                    "Pre-Conway protocol parameter update applied"
                );
            }
        }
        // Clean up past-epoch proposals.
        epochs
            .pending_pp_updates
            .retain(|epoch, _| *epoch >= lookup_epoch);

        // Promote future proposals -> current.
        if !epochs.future_pp_updates.is_empty() {
            let promoted = std::mem::take(&mut epochs.future_pp_updates);
            for (epoch, proposals) in promoted {
                epochs
                    .pending_pp_updates
                    .entry(epoch)
                    .or_default()
                    .extend(proposals);
            }
        }

        // Recalculate totalObligation (deposits) from scratch.
        {
            let obl_stake: u64 = certs.stake_key_deposits.values().sum();
            let obl_pool: u64 = certs.pool_deposits.values().sum();
            let obl_drep: u64 = gov.governance.dreps.values().map(|d| d.deposit.0).sum();
            let obl_proposal: u64 = gov
                .governance
                .proposals
                .values()
                .map(|p| p.procedure.deposit.0)
                .sum();
            certs.total_stake_key_deposits = obl_stake;
            debug!(
                epoch = new_epoch.0,
                obl_stake, obl_pool, obl_drep, obl_proposal, "totalObligation recalculated"
            );
        }

        // Compute new epoch nonce (TICKN rule):
        //   η0 = candidateNonce ⭒ prevHashNonce ⭒ extraEntropy
        // (Haskell `Cardano.Protocol.TPraos.Rules.Tickn.tickTransition`).
        // extraEntropy is NeutralNonce on virtually every epoch, but mainnet
        // injected a non-neutral value effective epoch 259 — omitting it
        // desynchronises the epoch nonce (and thus every VRF check) from there.
        let candidate = consensus.candidate_nonce;
        let prev_hash_nonce = consensus.last_epoch_block_nonce;
        consensus.epoch_nonce = super::common::combine_nonce(
            super::common::combine_nonce(candidate, prev_hash_nonce),
            consensus.extra_entropy,
        );

        // DIAGNOSTIC (#15 epoch-nonce debug): log the TICKN inputs + result so we
        // can cross-validate the epoch nonce against the live mainnet value.
        tracing::info!(
            epoch = new_epoch.0,
            candidate = %candidate.to_hex(),
            prev_hash_nonce = %prev_hash_nonce.to_hex(),
            lab_nonce = %consensus.lab_nonce.to_hex(),
            epoch_nonce = %consensus.epoch_nonce.to_hex(),
            old_d = %format!("{}/{}", old_d.numerator, old_d.denominator),
            new_d = %format!(
                "{}/{}",
                epochs.protocol_params.d.numerator, epochs.protocol_params.d.denominator
            ),
            evolving = %consensus.evolving_nonce.to_hex(),
            frozen = %(consensus.candidate_nonce != consensus.evolving_nonce),
            extra_entropy = %consensus.extra_entropy.to_hex(),
            "TICKN epoch nonce computed"
        );

        // Update prevHashNonce to current labNonce for NEXT epoch.
        consensus.last_epoch_block_nonce = consensus.lab_nonce;

        // Set prevPParams from values captured BEFORE PPUP.
        epochs.prev_d = old_d;
        epochs.prev_protocol_version_major = old_proto_major;
        epochs.prev_protocol_params = old_params;

        // Reset per-epoch accumulators.
        //
        // Issue #670: `utxo.epoch_fees` is NOT reset here — it was drained
        // earlier by `ssFee` (matching Haskell `applyRUpd`'s
        // `utxosFees -= ssFee` semantics). The residual carries forward as
        // the multi-epoch running total Haskell tracks in `utxosFees`.
        Arc::make_mut(&mut consensus.epoch_blocks_by_pool).clear();
        consensus.epoch_block_count = 0;

        Ok(())
    }

    /// Evolve nonce state after a Shelley+ block header.
    ///
    /// Delegates to `common::compute_shelley_nonce` which implements Haskell's
    /// `reupdateChainDepState` nonce state machine: evolving nonce, candidate
    /// nonce freeze, lab nonce, and block production tracking.
    fn evolve_nonce(
        &self,
        header: &BlockHeader,
        ctx: &RuleContext,
        consensus: &mut ConsensusSubState,
    ) {
        // Compute first slots of current and next epochs.
        let first_slot_of_current_epoch = common::first_slot_of_shelley_epoch(
            ctx.current_epoch.0,
            ctx.shelley_transition_epoch,
            ctx.byron_epoch_length,
            ctx.epoch_length,
        );
        let first_slot_of_next_epoch = first_slot_of_current_epoch.saturating_add(ctx.epoch_length);

        // For Babbage+ (proto >= 7), Haskell forces d=0 (full Praos).
        // Pre-Babbage uses the current pparams d as a rational.
        let (d_num, d_den) = if ctx.params.protocol_version_major >= 7 {
            (0u64, 1u64)
        } else {
            (ctx.params.d.numerator, ctx.params.d.denominator.max(1))
        };

        // Shelley through Mary use 3k/f stability window (not 4k/f).
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

    /// Shelley minimum fee: `min_fee_a * tx_size + min_fee_b`.
    ///
    /// Simple linear fee formula. Same formula applies across Shelley, Allegra,
    /// and Mary eras — the coefficients come from the current protocol params.
    fn min_fee(&self, tx: &Transaction, ctx: &RuleContext, _utxo: &UtxoSubState) -> u64 {
        let tx_size = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);
        ctx.params
            .min_fee_a
            .checked_mul(tx_size)
            .and_then(|product| product.checked_add(ctx.params.min_fee_b))
            .unwrap_or(u64::MAX)
    }

    /// Era transition handler for Shelley/Allegra/Mary boundaries.
    ///
    /// **Shelley → Allegra (`from_era == Shelley, ctx.era == Allegra`)**
    ///
    /// Implements `returnRedeemAddrsToReserves` from
    /// `Cardano.Ledger.Allegra.Translation` (cardano-ledger):
    ///
    /// ```haskell
    /// returnRedeemAddrsToReserves es = es { esChainAccountState = acnt'
    ///                                     , esLState = ls' }
    ///   where
    ///     UTxO utxo = utxosUtxo us
    ///     (redeemers, nonredeemers) =
    ///       Map.partition (maybe False isBootstrapRedeemer . view bootAddrTxOutF) utxo
    ///     acnt' = acnt { casReserves = casReserves acnt <+> sumCoinUTxO (UTxO redeemers) }
    ///     us'   = us   { utxosUtxo   = UTxO nonredeemers }
    /// ```
    ///
    /// `isBootstrapRedeemer` = the TxOut's address is a Byron `BootstrapAddress`
    /// whose inner `addr_type` field is `2` (ATRedeem / AVVM voucher-redemption
    /// address).  All such UTxOs are removed from the live set and their coin
    /// (lovelace only — multi-asset did not exist in Shelley) is added to reserves.
    ///
    /// This runs exactly ONCE at the Shelley→Allegra hard fork. On mainnet
    /// this purges the remaining unredeemed AVVM vouchers (~299 M ADA) and
    /// adds them back to reserves.
    ///
    /// **All other boundaries (Byron→Shelley, Allegra→Mary)**
    ///
    /// PV carry-forward is a no-op: `upgradeXxxPParams` in Haskell does a
    /// zero-cost `coerce`; PV bumps are driven by PPUP proposals, not by the
    /// era transition itself.  Byron→Shelley staking state (genesis delegates,
    /// initial funds) is already initialised during `LedgerState` construction.
    fn on_era_transition(
        &self,
        from_era: Era,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        _certs: &mut CertSubState,
        _gov: &mut GovSubState,
        _consensus: &mut ConsensusSubState,
        epochs: &mut EpochSubState,
    ) -> Result<(), LedgerError> {
        // Shelley → Allegra: returnRedeemAddrsToReserves
        // Haskell: Cardano.Ledger.Allegra.Translation (TranslateEra AllegraEra ShelleyEra)
        if from_era == Era::Shelley && ctx.era == Era::Allegra {
            return_redeem_addrs_to_reserves(utxo, epochs);
            return Ok(());
        }

        debug!(
            "{:?} -> {:?} era transition (ctx.era={:?}): no ledger-side state \
             mutation; PV carry-forward is a no-op, PPUP drives all bumps",
            from_era, ctx.era, ctx.era,
        );
        Ok(())
    }

    /// Compute the set of required VKey witnesses for a Shelley/Allegra/Mary transaction.
    ///
    /// Required witnesses come from three sources:
    /// 1. **Spending inputs**: payment credential key hashes from UTxO outputs being consumed.
    /// 2. **Withdrawals**: reward account credential key hashes.
    /// 3. **Certificates**: key hashes from stake credential operations and pool operations.
    ///
    /// Script credentials (hash prefix 0x01 in byte 28) are excluded — they require
    /// script witnesses, not VKey witnesses.
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
            if reward_account.len() >= 29 {
                // Bit 4 of header byte: 0 = key, 1 = script.
                // Only require VKey witness for key-based reward accounts.
                if reward_account[0] & 0x10 == 0 {
                    let mut key_bytes = [0u8; 28];
                    key_bytes.copy_from_slice(&reward_account[1..29]);
                    witnesses.insert(Hash28::from_bytes(key_bytes));
                }
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
                    // Pool operator key hash is required.
                    witnesses.insert(params.operator);
                    // Pool owner key hashes are required.
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

        witnesses
    }
}

// ---------------------------------------------------------------------------
// Helper: extract Hash32 from reward account bytes
// ---------------------------------------------------------------------------

/// Extract a Hash32 from a raw reward account byte string (29 bytes:
/// 1-byte header + 28-byte credential hash).
///
/// Mirrors `LedgerState::reward_account_to_hash` and
/// `common::reward_account_to_hash`.
fn reward_account_to_hash(reward_account: &[u8]) -> Hash32 {
    let mut key_bytes = [0u8; 32];
    if reward_account.len() >= 29 {
        key_bytes[..28].copy_from_slice(&reward_account[1..29]);
        if reward_account[0] & 0x10 != 0 {
            key_bytes[28] = 0x01;
        }
    }
    Hash32::from_bytes(key_bytes)
}

// ---------------------------------------------------------------------------
// Helper: apply protocol parameter update to ProtocolParameters
// ---------------------------------------------------------------------------

/// Apply a merged ProtocolParamUpdate to the current ProtocolParameters.
///
/// Each field in the update, if `Some`, overrides the corresponding field
/// in the protocol parameters.
pub(crate) fn apply_pp_update(
    params: &mut ProtocolParameters,
    update: &dugite_primitives::transaction::ProtocolParamUpdate,
) {
    if let Some(v) = update.min_fee_a {
        params.min_fee_a = v;
    }
    if let Some(v) = update.min_fee_b {
        params.min_fee_b = v;
    }
    if let Some(v) = update.max_block_body_size {
        params.max_block_body_size = v;
    }
    if let Some(v) = update.max_tx_size {
        params.max_tx_size = v;
    }
    if let Some(v) = update.max_block_header_size {
        params.max_block_header_size = v;
    }
    if let Some(v) = &update.key_deposit {
        params.key_deposit = *v;
    }
    if let Some(v) = &update.pool_deposit {
        params.pool_deposit = *v;
    }
    if let Some(v) = update.e_max {
        params.e_max = v;
    }
    if let Some(v) = update.n_opt {
        params.n_opt = v;
    }
    if let Some(v) = &update.a0 {
        params.a0 = v.clone();
    }
    if let Some(v) = &update.rho {
        params.rho = v.clone();
    }
    if let Some(v) = &update.tau {
        params.tau = v.clone();
    }
    if let Some(v) = &update.d {
        params.d = v.clone();
    }
    if let Some(v) = &update.min_pool_cost {
        params.min_pool_cost = *v;
    }
    if let Some(v) = &update.ada_per_utxo_byte {
        params.ada_per_utxo_byte = *v;
    }
    if let Some(v) = &update.cost_models {
        params.cost_models = v.clone();
    }
    if let Some(v) = &update.execution_costs {
        params.execution_costs = v.clone();
    }
    if let Some(v) = &update.max_tx_ex_units {
        params.max_tx_ex_units = *v;
    }
    if let Some(v) = &update.max_block_ex_units {
        params.max_block_ex_units = *v;
    }
    if let Some(v) = update.max_val_size {
        params.max_val_size = v;
    }
    if let Some(v) = update.collateral_percentage {
        params.collateral_percentage = v;
    }
    if let Some(v) = update.max_collateral_inputs {
        params.max_collateral_inputs = v;
    }
    if let Some(v) = update.protocol_version_major {
        params.protocol_version_major = v;
    }
    if let Some(v) = update.protocol_version_minor {
        params.protocol_version_minor = v;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eras::{EraRules, EraRulesImpl, RuleContext};
    use crate::state::{
        BlockValidationMode, EpochSnapshots, GovernanceState, PoolRegistration,
        StakeDistributionState,
    };
    use crate::utxo::UtxoSet;
    use crate::utxo_diff::DiffSeq;
    use dugite_primitives::address::Address;
    use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::{BlockNo, SlotNo};
    use dugite_primitives::transaction::{
        OutputDatum, TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
    };
    use dugite_primitives::value::Value;
    use std::collections::BTreeMap;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_shelley_ctx(params: &ProtocolParameters) -> RuleContext<'_> {
        let delegates = Box::leak(Box::new(HashMap::new()));
        RuleContext {
            params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: Era::Shelley,
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
            prev_protocol_version_major: 2,
            prev_d: dugite_primitives::transaction::Rational {
                numerator: 1,
                denominator: 1,
            },
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
            protocol_version: ProtocolVersion { major: 2, minor: 0 },
            kes_signature: vec![],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
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
            era: Era::Shelley,
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

    fn make_enterprise_address(key_hash: Hash28) -> Address {
        let mut addr_bytes = vec![0x61]; // Enterprise, key credential, network 1
        addr_bytes.extend_from_slice(key_hash.as_bytes());
        Address::from_bytes(&addr_bytes).expect("valid enterprise address")
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// Verify that EraRulesImpl::for_era correctly maps Shelley/Allegra/Mary.
    #[test]
    fn test_era_rules_impl_for_shelley_allegra_mary() {
        assert!(matches!(
            EraRulesImpl::for_era(Era::Shelley),
            EraRulesImpl::Shelley(_)
        ));
        assert!(matches!(
            EraRulesImpl::for_era(Era::Allegra),
            EraRulesImpl::Shelley(_)
        ));
        assert!(matches!(
            EraRulesImpl::for_era(Era::Mary),
            EraRulesImpl::Shelley(_)
        ));
    }

    /// validate_block_body always succeeds for Shelley (no ExUnit checks).
    #[test]
    fn test_validate_block_body_always_succeeds() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let block = dugite_primitives::block::Block {
            era: Era::Shelley,
            header: make_block_header(Hash32::ZERO, vec![]),
            transactions: vec![],
            raw_cbor: None,
        };

        assert!(rules.validate_block_body(&block, &ctx, &utxo).is_ok());
    }

    /// Apply an empty valid transaction — no inputs consumed, no outputs produced.
    #[test]
    fn test_apply_valid_tx_empty_tx() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let tx = make_tx(0x01, vec![], vec![], 0);
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
        assert!(diff.inserts.is_empty());
        assert!(diff.deletes.is_empty());
    }

    /// Apply a valid transaction that spends a UTxO and produces a new one.
    #[test]
    fn test_apply_valid_tx_with_utxo() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAA, 0);
        let spent_output = make_output(addr.clone(), 5_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let tx = make_tx(
            0xBB,
            vec![input],
            vec![make_output(addr, 4_800_000)],
            200_000,
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
        assert!(result.is_ok());
        let diff = result.unwrap();
        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 1);
        assert_eq!(utxo.epoch_fees.0, 200_000);
    }

    /// Shelley apply_invalid_tx must return an error.
    #[test]
    fn test_apply_invalid_tx_returns_error() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0x01, vec![], vec![], 0);
        let mut certs = make_cert_sub();
        let mut epochs = make_epoch_sub();
        let result = rules.apply_invalid_tx(
            &tx,
            BlockValidationMode::ApplyOnly,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut epochs,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LedgerError::InvalidTransaction(_)
        ));
    }

    /// Minimum fee computation: min_fee_a * tx_size + min_fee_b.
    #[test]
    fn test_min_fee_linear() {
        let rules = ShelleyRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.min_fee_a = 44;
        params.min_fee_b = 155_381;
        let ctx = make_shelley_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0x01, vec![], vec![], 0); // 200 bytes raw_cbor
        let fee = rules.min_fee(&tx, &ctx, &utxo);
        // 44 * 200 + 155_381 = 8_800 + 155_381 = 164_181
        assert_eq!(fee, 164_181);
    }

    /// Byron -> Shelley bumps protocol_version to (2, 0). Mirrors the HFC
    /// era-crossing tick (issue #615, same class as resolved #481).
    #[test]
    fn test_on_era_transition_byron_to_shelley_does_not_bump_pv() {
        // After #630: on_era_transition must NOT write PV — PPUP via UPEC/NEWPP does that.
        // PV at Byron→Shelley comes from shelley-genesis.json::protocolParams::protocolVersion.
        let rules = ShelleyRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 2;
        params.protocol_version_minor = 0;
        let ctx = make_shelley_ctx(&params); // ctx.era = Era::Shelley
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();
        epochs.protocol_params.protocol_version_major = 2;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Byron,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 2,
            "on_era_transition must NOT bump PV — PPUP via UPEC/NEWPP does that",
        );
        assert_eq!(epochs.protocol_params.protocol_version_minor, 0);
    }

    /// Shelley -> Allegra preserves protocol_version (PPUP drives bumps).
    #[test]
    fn test_on_era_transition_shelley_to_allegra_does_not_bump_pv() {
        let rules = ShelleyRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 3;
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Allegra;
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();
        epochs.protocol_params.protocol_version_major = 3;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Shelley,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 3,
            "on_era_transition must NOT bump PV — PPUP via UPEC/NEWPP does that",
        );
        assert_eq!(epochs.protocol_params.protocol_version_minor, 0);
    }

    /// Allegra -> Mary preserves protocol_version (PPUP drives bumps).
    #[test]
    fn test_on_era_transition_allegra_to_mary_does_not_bump_pv() {
        let rules = ShelleyRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 4;
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Mary;
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();
        epochs.protocol_params.protocol_version_major = 4;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Allegra,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 4,
            "on_era_transition must NOT bump PV — PPUP via UPEC/NEWPP does that",
        );
        assert_eq!(epochs.protocol_params.protocol_version_minor, 0);
    }

    /// Defensive: ShelleyRules is a no-op regardless of ctx.era.
    #[test]
    fn test_on_era_transition_shelleyrules_no_pv_mutation() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Conway; // shouldn't happen in production, but must not mutate
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();
        epochs.protocol_params.protocol_version_major = 9;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Byron,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(
            epochs.protocol_params.protocol_version_major, 9,
            "ShelleyRules must never mutate PV",
        );
    }

    /// Basic epoch transition: fees reset, block count reset, mark snapshot created.
    #[test]
    fn test_process_epoch_transition_basic() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        utxo.epoch_fees = Lovelace(1_000_000);
        consensus.epoch_block_count = 100;

        let result = rules.process_epoch_transition(
            EpochNo(6),
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
            &mut consensus,
        );
        assert!(result.is_ok());

        // Issue #670: `epoch_fees` mirrors Haskell `utxosFees` (multi-epoch
        // cumulative). applyRUpd drains by the prior `ssFee` (zero here
        // since `make_epoch_sub` seeds it that way), so the 1_000_000
        // lovelace carries forward and is also captured into the new
        // `ssFee` for the next boundary's RUPD.
        assert_eq!(utxo.epoch_fees.0, 1_000_000);
        assert_eq!(consensus.epoch_block_count, 0);
        assert!(epochs.snapshots.mark.is_some());
        assert_eq!(epochs.snapshots.ss_fee.0, 1_000_000);
    }

    /// Epoch transition with pool retirement: pool removed, deposit refunded.
    #[test]
    fn test_process_epoch_transition_pool_retirement() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        // Register a pool.
        let pool_id = Hash28::from_bytes([0xAA; 28]);
        let pool_reg = PoolRegistration {
            pool_id,
            vrf_keyhash: Hash32::ZERO,
            pledge: Lovelace(10_000_000_000),
            cost: Lovelace(340_000_000),
            margin_numerator: 1,
            margin_denominator: 100,
            reward_account: vec![0xe0; 29],
            owners: vec![Hash28::from_bytes([0xCC; 28])],
            relays: vec![],
            metadata_url: None,
            metadata_hash: None,
        };
        Arc::make_mut(&mut certs.pool_params).insert(pool_id, pool_reg);
        certs.pool_deposits.insert(pool_id, 500_000_000);

        // Register the operator's reward account.
        let op_key = reward_account_to_hash(&[0xe0; 29]);
        Arc::make_mut(&mut certs.reward_accounts).insert(op_key, Lovelace(0));

        // Schedule retirement at epoch 6.
        certs.pending_retirements.insert(pool_id, EpochNo(6));

        let result = rules.process_epoch_transition(
            EpochNo(6),
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
            &mut consensus,
        );
        assert!(result.is_ok());

        assert!(!certs.pool_params.contains_key(&pool_id));
        assert_eq!(certs.reward_accounts.get(&op_key).unwrap().0, 500_000_000);
        assert!(certs.pending_retirements.is_empty());
    }

    /// Evolve nonce with VRF output updates evolving_nonce, lab_nonce, block count.
    #[test]
    fn test_evolve_nonce_with_vrf_output() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut consensus = make_consensus_sub();

        let mut header = make_block_header(Hash32::from_bytes([0x01; 32]), vec![0x99; 32]);
        header.nonce_vrf_output = vec![0x42; 32];

        rules.evolve_nonce(&header, &ctx, &mut consensus);

        assert_ne!(consensus.evolving_nonce, Hash32::ZERO);
        assert_eq!(consensus.lab_nonce, header.prev_hash);
        assert_eq!(consensus.epoch_block_count, 1);
    }

    /// Required witnesses include spending input key hashes.
    #[test]
    fn test_required_witnesses_spending_inputs() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAA, 0);
        let utxo = make_utxo_sub(vec![(input.clone(), make_output(addr, 5_000_000))]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let tx = make_tx(0xBB, vec![input], vec![], 0);
        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);
        assert!(witnesses.contains(&key_hash));
    }

    /// Required witnesses include withdrawal key hashes.
    #[test]
    fn test_required_witnesses_withdrawals() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let utxo = make_utxo_sub(vec![]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let key_hash = Hash28::from_bytes([0x55; 28]);
        let mut reward_account = vec![0xe0]; // key-based
        reward_account.extend_from_slice(key_hash.as_bytes());

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body
            .withdrawals
            .insert(reward_account, Lovelace(1_000_000));

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);
        assert!(witnesses.contains(&key_hash));
    }

    /// Required witnesses include certificate key hashes.
    #[test]
    fn test_required_witnesses_certificates() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let utxo = make_utxo_sub(vec![]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let key_hash = Hash28::from_bytes([0x77; 28]);
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::StakeDelegation {
            credential: Credential::VerificationKey(key_hash),
            pool_hash: Hash28::from_bytes([0x88; 28]),
        }];

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);
        assert!(witnesses.contains(&key_hash));
    }

    /// Script-based withdrawal should NOT be in required witnesses (needs script witness instead).
    #[test]
    fn test_required_witnesses_script_withdrawal_excluded() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let utxo = make_utxo_sub(vec![]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let key_hash = Hash28::from_bytes([0x55; 28]);
        let mut reward_account = vec![0xf0]; // script-based (bit 4 set)
        reward_account.extend_from_slice(key_hash.as_bytes());

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body
            .withdrawals
            .insert(reward_account, Lovelace(1_000_000));

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);
        // Script-based withdrawal should NOT produce a VKey witness requirement.
        assert!(!witnesses.contains(&key_hash));
    }

    /// Epoch transition flushes pending treasury donations.
    #[test]
    fn test_process_epoch_transition_flushes_donations() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        utxo.pending_donations = Lovelace(5_000_000);
        epochs.treasury = Lovelace(100_000_000);

        let result = rules.process_epoch_transition(
            EpochNo(6),
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
            &mut consensus,
        );
        assert!(result.is_ok());
        assert_eq!(utxo.pending_donations.0, 0);
        assert_eq!(epochs.treasury.0, 105_000_000);
    }

    #[test]
    fn test_shelley_epoch_transition_computes_rupd() {
        // Set up state with a GO snapshot containing one pool that produced blocks.
        // After epoch transition, reserves should decrease (indicating RUPD ran).
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_shelley_ctx(&params);
        let rules = ShelleyRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        let pool_id = Hash28::from_bytes([1u8; 28]);
        let owner_key = Hash28::from_bytes([2u8; 28]);
        let delegator_cred = Hash32::from_bytes([3u8; 32]);

        // Set up pool registration with pledge met via delegator stake.
        let pool_reg = PoolRegistration {
            pool_id,
            vrf_keyhash: Hash32::ZERO,
            pledge: Lovelace(1_000_000_000), // 1000 ADA pledge
            cost: Lovelace(340_000_000),     // 340 ADA cost
            margin_numerator: 1,
            margin_denominator: 100,
            reward_account: vec![],
            owners: vec![owner_key],
            relays: vec![],
            metadata_url: None,
            metadata_hash: None,
        };

        // Register the pool and set up delegation.
        Arc::make_mut(&mut certs.pool_params).insert(pool_id, pool_reg.clone());
        Arc::make_mut(&mut certs.delegations).insert(delegator_cred, pool_id);

        // Register a reward account for the delegator.
        Arc::make_mut(&mut certs.reward_accounts).insert(delegator_cred, Lovelace(0));

        // Build a GO snapshot with the pool having stake.
        let mut pool_stake = HashMap::new();
        pool_stake.insert(pool_id, Lovelace(10_000_000_000_000)); // 10M ADA
        let mut stake_dist = HashMap::new();
        stake_dist.insert(delegator_cred, Lovelace(10_000_000_000_000));

        let go_snapshot = crate::state::StakeSnapshot {
            epoch: EpochNo(3),
            delegations: Arc::clone(&certs.delegations),
            pool_stake,
            pool_params: Arc::clone(&certs.pool_params),
            stake_distribution: Arc::new(stake_dist),
            epoch_fees: Lovelace(500_000_000), // 500 ADA fees
            epoch_block_count: 100,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };

        epochs.snapshots.go = Some(go_snapshot);
        epochs.snapshots.rupd_ready = true;

        // Pool produced blocks in previous epoch.
        let mut blocks_by_pool = HashMap::new();
        blocks_by_pool.insert(pool_id, 50);
        epochs.snapshots.bprev_blocks_by_pool = Arc::new(blocks_by_pool);
        epochs.snapshots.ss_fee = Lovelace(500_000_000);

        // Set reserves high enough for expansion.
        let initial_reserves = 10_000_000_000_000_000u64; // 10B ADA
        epochs.reserves = Lovelace(initial_reserves);
        let initial_treasury = epochs.treasury.0;

        // Set d < 0.8 so decentralisation allows pool rewards.
        epochs.prev_d = dugite_primitives::transaction::Rational {
            numerator: 1,
            denominator: 2,
        };

        let result = rules.process_epoch_transition(
            EpochNo(6),
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
            &mut consensus,
        );
        assert!(result.is_ok());

        // Reserves should have decreased (monetary expansion distributed).
        assert!(
            epochs.reserves.0 < initial_reserves,
            "Reserves should decrease after RUPD: was {}, now {}",
            initial_reserves,
            epochs.reserves.0
        );

        // Treasury should have increased (tau fraction of expansion).
        assert!(
            epochs.treasury.0 > initial_treasury,
            "Treasury should increase after RUPD: was {}, now {}",
            initial_treasury,
            epochs.treasury.0
        );
    }

    // -----------------------------------------------------------------------
    // returnRedeemAddrsToReserves tests (Shelley → Allegra transition)
    // -----------------------------------------------------------------------

    /// Build a minimal Byron address payload in the outer wire format
    /// (`array(2)[tag(24, bstr(inner)), crc32]`).
    ///
    /// `addr_type`: 0 = PubKey, 1 = Script, 2 = Redeem (ATRedeem/AVVM).
    fn make_byron_payload(addr_type: u32) -> Vec<u8> {
        // Inner: array(3)[bstr(28), map(0), u32(addr_type)]
        let mut inner = Vec::new();
        let mut e = minicbor::Encoder::new(&mut inner);
        e.array(3).unwrap();
        e.bytes(&[0xABu8; 28]).unwrap(); // 28-byte root hash (synthetic)
        e.map(0).unwrap(); // empty attributes (mainnet)
        e.u32(addr_type).unwrap();

        // Outer: array(2)[tag(24, bstr(inner)), crc32_placeholder]
        let mut outer = Vec::new();
        let mut oe = minicbor::Encoder::new(&mut outer);
        oe.array(2).unwrap();
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner).unwrap();
        oe.u32(0xDEAD_BEEFu32).unwrap(); // CRC not checked in is_redeem()
        outer
    }

    fn make_byron_output(addr_type: u32, lovelace: u64) -> TransactionOutput {
        use dugite_primitives::address::ByronAddress;
        TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: make_byron_payload(addr_type),
            }),
            value: Value::lovelace(lovelace),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    fn make_redeem_input(tx_id: u8, index: u32) -> TransactionInput {
        use dugite_primitives::hash::Hash32;
        TransactionInput {
            transaction_id: Hash32::from_bytes([tx_id; 32]),
            index,
        }
    }

    /// Shelley → Allegra transition purges AVVM (redeem) UTxOs from the UTxO
    /// set and credits their coin to reserves.
    ///
    /// Haskell: `Cardano.Ledger.Allegra.Translation.returnRedeemAddrsToReserves`
    #[test]
    fn test_shelley_to_allegra_purges_redeem_utxos_and_credits_reserves() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Allegra;

        // UTxO set:
        //   - 2 × redeem (AVVM, addr_type=2), 100 ADA each
        //   - 1 × PubKey (addr_type=0), 500 ADA  — must survive
        let redeem_1 = (
            make_redeem_input(0x01, 0),
            make_byron_output(2, 100_000_000),
        );
        let redeem_2 = (
            make_redeem_input(0x02, 0),
            make_byron_output(2, 100_000_000),
        );
        let pubkey = (
            make_redeem_input(0x03, 0),
            make_byron_output(0, 500_000_000),
        );
        let mut utxo = make_utxo_sub(vec![redeem_1.clone(), redeem_2.clone(), pubkey.clone()]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.reserves = Lovelace(1_000_000_000_000); // 1M ADA baseline
        let mut consensus = make_consensus_sub();

        let result = rules.on_era_transition(
            Era::Shelley,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok(), "on_era_transition must not error");

        // Only the PubKey UTxO survives.
        assert_eq!(
            utxo.utxo_set.len(),
            1,
            "only the non-redeem UTxO should remain"
        );
        assert!(
            utxo.utxo_set.lookup(&pubkey.0).is_some(),
            "PubKey UTxO must survive"
        );
        assert!(
            utxo.utxo_set.lookup(&redeem_1.0).is_none(),
            "redeem UTxO 1 must be purged"
        );
        assert!(
            utxo.utxo_set.lookup(&redeem_2.0).is_none(),
            "redeem UTxO 2 must be purged"
        );

        // Reserves: 1_000_000_000_000 + 200_000_000 (two × 100 ADA).
        assert_eq!(
            epochs.reserves.0, 1_000_200_000_000,
            "reserves must be credited with sumCoinUTxO of redeem entries"
        );
    }

    /// Non-redeem Byron addresses (PubKey addr_type=0, Script addr_type=1) are
    /// NOT purged by returnRedeemAddrsToReserves.
    #[test]
    fn test_shelley_to_allegra_preserves_non_redeem_byron_utxos() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Allegra;

        let pubkey_utxo = (make_redeem_input(0x10, 0), make_byron_output(0, 50_000_000));
        let script_utxo = (make_redeem_input(0x11, 0), make_byron_output(1, 75_000_000));
        let mut utxo = make_utxo_sub(vec![pubkey_utxo.clone(), script_utxo.clone()]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.reserves = Lovelace(0);
        let mut consensus = make_consensus_sub();

        let result = rules.on_era_transition(
            Era::Shelley,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());

        // All entries survive — none are redeem type.
        assert_eq!(utxo.utxo_set.len(), 2);
        assert_eq!(
            epochs.reserves.0, 0,
            "reserves unchanged when no redeem UTxOs present"
        );
    }

    /// When the UTxO set is empty the transition is a no-op.
    #[test]
    fn test_shelley_to_allegra_empty_utxo_is_noop() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Allegra;

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.reserves = Lovelace(42_000_000_000_000);
        let mut consensus = make_consensus_sub();

        let result = rules.on_era_transition(
            Era::Shelley,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        assert_eq!(utxo.utxo_set.len(), 0);
        assert_eq!(epochs.reserves.0, 42_000_000_000_000, "reserves unchanged");
    }

    /// Allegra → Mary transition must NOT invoke returnRedeemAddrsToReserves.
    /// (This is a guard: the purge only fires at Shelley→Allegra.)
    #[test]
    fn test_allegra_to_mary_does_not_purge_redeem_utxos() {
        let rules = ShelleyRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut ctx = make_shelley_ctx(&params);
        ctx.era = Era::Mary;

        let redeem = (
            make_redeem_input(0x20, 0),
            make_byron_output(2, 100_000_000),
        );
        let mut utxo = make_utxo_sub(vec![redeem.clone()]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.reserves = Lovelace(0);
        let mut consensus = make_consensus_sub();

        let result = rules.on_era_transition(
            Era::Allegra,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());

        // Redeem UTxO must still be present — purge only happens at Shelley→Allegra.
        assert_eq!(
            utxo.utxo_set.len(),
            1,
            "Allegra→Mary must NOT purge redeem UTxOs"
        );
        assert_eq!(epochs.reserves.0, 0, "reserves must be unchanged");
    }
}
