/// Conway era ledger rules (protocol version 9+).
///
/// Conway (CIP-1694) introduces on-chain governance:
/// - DRep (Delegated Representatives) registration, delegation, and voting
/// - Constitutional Committee hot key authorization and resignation
/// - Governance actions (proposals) and voting by DReps, SPOs, and CC members
/// - Treasury withdrawals via governance actions
/// - Protocol parameter updates via governance (replaces pre-Conway PPUP)
/// - Tiered reference script fees (25KiB tiers, 1.2x multiplier)
/// - Plutus V3
///
/// The Conway LEDGER rule pipeline has 9 steps (compared to Babbage's 3):
/// 1. validateTreasuryValue
/// 2. validateRefScriptSize
/// 3. validateWithdrawalsDelegated (PV >= 10)
/// 4. testIncompleteAndMissingWithdrawals (PV >= 10)
/// 5. updateDormantDRepExpiries / updateVotingDRepExpiries
/// 6. drainAccounts (same as Shelley)
/// 7. Apply CERTS rule (Shelley certs + Conway governance certs)
/// 8. Apply GOV rule (votes + proposals)
/// 9. Apply UTXOW/UTXO/UTXOS rule (consume inputs, produce outputs)
///
/// The Conway epoch transition has 13 steps (compared to Shelley's ~8):
/// 1. SNAP (snapshot rotation)
/// 2. POOLREAP (pool retirements with deposit refunds)
/// 3. DRep pulser completion
/// 4. Treasury withdrawals (enact approved withdrawals)
/// 5. proposalsApplyEnactment (ratify & enact governance actions)
/// 6. Return deposits from expired/enacted proposals
/// 7. Update GovState (advance proposal epochs, remove enacted/expired)
/// 8. numDormantEpochs computation
/// 9. Prune expired committee members
/// 10. Flush donations (pending_donations -> treasury)
/// 11. totalObligation recalculation
/// 12. HARDFORK check
/// 13. setFreshDRepPulsingState
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::block::{Block, BlockHeader};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{Certificate, GovActionId, Transaction, Voter};
use dugite_primitives::value::Lovelace;
use tracing::{debug, warn};

use super::common;
use super::{EraRules, RuleContext};
use crate::state::governance::{
    capture_governance_snapshots, expire_committee_members, forest_add_proposal,
    genesis_root_is_valid, gov_action_purpose_tag, gov_action_raw_prev_id,
    hardfork_proposal_cant_follow, prev_action_matches_enacted_root, ratify_proposals_impl,
    update_dormant_epochs, update_drep_activity,
};
use crate::state::substates::*;
use crate::state::{
    apply_reserves_delta, BlockValidationMode, DRepRegistration, LedgerError, ProposalState,
    StakeSnapshot,
};
use crate::utxo_diff::UtxoDiff;

/// Stateless Conway era rule strategy.
///
/// Implements the Conway-specific LEDGER pipeline and epoch transition.
/// Delegates shared logic (drain withdrawals, UTxO changes, nonce evolution)
/// to common helpers, and adds Conway-specific steps for governance
/// certificates, proposals, votes, and the extended epoch transition.
#[derive(Default, Debug, Clone, Copy)]
pub struct ConwayRules;

impl ConwayRules {
    pub fn new() -> Self {
        ConwayRules
    }
}

impl EraRules for ConwayRules {
    /// Validate Conway block body constraints.
    ///
    /// Checks:
    /// 1. Total ExUnit budget (memory + steps) does not exceed block limits.
    /// 2. Total reference script size across all transactions does not exceed
    ///    1 MiB (Conway `ppMaxRefScriptSizePerBlockG`).
    fn validate_block_body(
        &self,
        block: &Block,
        ctx: &RuleContext,
        utxo: &UtxoSubState,
    ) -> Result<(), LedgerError> {
        // Step 1: ExUnit budget check (shared with Alonzo/Babbage).
        common::validate_block_ex_units(block, ctx)?;

        // Step 2: Block-level reference script size check.
        // Build within-block UTxO overlay for ref script resolution (outputs
        // created earlier in the block may be referenced later).
        let mut block_utxo_overlay: std::collections::HashMap<
            dugite_primitives::transaction::TransactionInput,
            dugite_primitives::transaction::TransactionOutput,
        > = std::collections::HashMap::new();
        for tx in &block.transactions {
            if tx.is_valid {
                for (idx, output) in tx.body.outputs.iter().enumerate() {
                    block_utxo_overlay.insert(
                        dugite_primitives::transaction::TransactionInput {
                            transaction_id: tx.hash,
                            index: idx as u32,
                        },
                        output.clone(),
                    );
                }
            }
        }

        let lookup_with_overlay = |input: &dugite_primitives::transaction::TransactionInput| {
            block_utxo_overlay
                .get(input)
                .cloned()
                .or_else(|| utxo.utxo_set.lookup(input))
        };

        let total_ref_script_size: u64 = block
            .transactions
            .iter()
            .map(|tx| {
                let spending_size: u64 = tx
                    .body
                    .inputs
                    .iter()
                    .filter_map(|inp| {
                        lookup_with_overlay(inp).and_then(|utxo_out| {
                            utxo_out
                                .script_ref
                                .as_ref()
                                .map(crate::validation::script_ref_byte_size)
                        })
                    })
                    .sum();
                let reference_size: u64 = tx
                    .body
                    .reference_inputs
                    .iter()
                    .filter_map(|inp| {
                        lookup_with_overlay(inp).and_then(|utxo_out| {
                            utxo_out
                                .script_ref
                                .as_ref()
                                .map(crate::validation::script_ref_byte_size)
                        })
                    })
                    .sum();
                spending_size.saturating_add(reference_size)
            })
            .fold(0u64, |acc, x| acc.saturating_add(x));

        // Conway block body limit: 1 MiB (hardcoded, not governance-updateable).
        const MAX_REF_SCRIPT_SIZE_PER_BLOCK: u64 = 1024 * 1024;
        if total_ref_script_size > MAX_REF_SCRIPT_SIZE_PER_BLOCK {
            return Err(LedgerError::BlockTxValidationFailed {
                slot: ctx.current_slot,
                tx_hash: String::from("(block-level check)"),
                errors: format!(
                    "BodyRefScriptsSizeTooBig: totalRefScriptSize={} exceeds \
                     maxRefScriptSizePerBlock={} (Conway Bbody rule)",
                    total_ref_script_size, MAX_REF_SCRIPT_SIZE_PER_BLOCK
                ),
            });
        }

        Ok(())
    }

    /// Apply a single valid Conway transaction (IsValid=true).
    ///
    /// Implements the Conway 9-step LEDGER pipeline:
    ///
    /// 1. **validateTreasuryValue** -- if tx sets currentTreasuryValue,
    ///    verify it matches the actual treasury balance. Gated on
    ///    `BlockValidationMode::ValidateAll` per Haskell — replay
    ///    (`reapplySTS` / `ValidateNone`) skips the check.
    /// 2. **validateRefScriptSize** -- total ref script size <= maxRefScriptSizePerTx.
    ///    (Stub: checked during tx validation, not during apply.)
    /// 3. **validateWithdrawalsDelegated** (PV >= 10) -- all withdrawal KeyHash
    ///    accounts must be DRep-delegated. Fires unconditionally on every
    ///    block apply per Haskell.
    /// 4. **testIncompleteAndMissingWithdrawals** (PV >= 10) -- withdrawals
    ///    drain accounts exactly. Fires unconditionally on every block
    ///    apply per Haskell.
    /// 5. **updateDormantDRepExpiries / updateVotingDRepExpiries** -- update DRep
    ///    last-active epoch for voting DReps in this transaction.
    /// 6. **drainAccounts** -- apply withdrawals to balances (same as Shelley).
    /// 7. **Apply CERTS rule** -- process both Shelley certs AND Conway governance
    ///    certs (DRep registration, DRep update, DRep deregistration, CC hot key
    ///    auth, CC resignation, combined delegation certs).
    /// 8. **Apply GOV rule** -- process governance votes and proposals.
    /// 9. **Apply UTXOW/UTXO/UTXOS rule** -- consume inputs, produce outputs.
    fn apply_valid_tx(
        &self,
        tx: &Transaction,
        mode: BlockValidationMode,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        gov: &mut GovSubState,
        epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        // Step 1: validateTreasuryValue.
        //
        // Per Haskell `Cardano.Ledger.Conway.Rules.Ledger.validateTreasuryValue`
        // (eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:364), the
        // tx body's optional `currentTreasuryValue` (CBOR key 21) is compared
        // against the actual `ChainAccountState.casTreasury` via
        // `failureUnless`, producing the `ConwayTreasuryValueMismatch`
        // predicate failure on mismatch.
        //
        // Critically, this failure is gated by the STS `ValidationPolicy`:
        //   - `ValidateAll`  (live `tickThenApply`)   — check fires, block rejected on mismatch.
        //   - `ValidateNone` (replay `tickThenReapply`/`reapplySTS`)
        //                                              — check is SKIPPED entirely.
        //
        // The consensus layer uses `reapplySTS` (ValidateNone) for any block
        // that is already in the ImmutableDB: ChainDB replay, Mithril import,
        // rollback replay, self-forged blocks. Those blocks are trusted by
        // construction — they were validated when first received. dugite's
        // equivalent is `BlockValidationMode::ApplyOnly`.
        //
        // Issue #678: previously this check ran unconditionally, which broke
        // from-genesis preview replay at slot 76172461 (a TreasuryDonation tx
        // declaring `currentTreasuryValue = 5216453806026839`). Even with a
        // hypothetical per-epoch accounting drift in dugite, Haskell would
        // not reject the replay. The check is now correctly gated on
        // `ValidateAll`; in `ApplyOnly` mode we WARN so any underlying drift
        // is still surfaced for follow-up investigation without halting sync.
        if let Some(declared_treasury) = tx.body.treasury_value {
            if declared_treasury != epochs.treasury {
                match mode {
                    BlockValidationMode::ValidateAll => {
                        return Err(LedgerError::BlockTxValidationFailed {
                            slot: ctx.current_slot,
                            tx_hash: tx.hash.to_hex(),
                            errors: format!(
                                "MismatchedTreasuryValue: declared {} != actual {}",
                                declared_treasury.0, epochs.treasury.0
                            ),
                        });
                    }
                    BlockValidationMode::ApplyOnly => {
                        let declared = declared_treasury.0 as i128;
                        let actual = epochs.treasury.0 as i128;
                        warn!(
                            tx_hash = %tx.hash.to_hex(),
                            slot = ctx.current_slot,
                            declared = declared_treasury.0,
                            actual = epochs.treasury.0,
                            delta_lovelace = actual - declared,
                            "TreasuryValueMismatch in ApplyOnly mode (issue #678) — \
                             Haskell skips this check under ValidateNone, so replay \
                             continues. A non-zero delta indicates a real per-epoch \
                             treasury accounting divergence to investigate."
                        );
                    }
                }
            }
        }

        // Step 2: validateRefScriptSize (stub).
        // Reference script size validation is performed during Phase-1 validation,
        // not during apply. The tiered fee check lives in validation/conway.rs.

        // Steps 3 & 4: PV10 withdrawal checks.
        //
        // Per Haskell `Cardano.Ledger.Conway.Rules.Ledger.conwayWithdrawals`,
        // `validateWithdrawalsDelegated` (step 3) and
        // `testIncompleteAndMissingWithdrawals` (step 4) fire UNCONDITIONALLY
        // on every block apply — there is no mode gate in Haskell.  Reward
        // balances at the point of application are byte-exact with the
        // prior RUPD; #629 closed the f64→Rational gap that previously
        // caused dugite's reward accumulation to drift by a few thousand
        // lovelace on long-horizon replays and made these checks
        // false-positive in `ApplyOnly` mode.
        if ctx.params.protocol_version_major >= 10 {
            // Step 3: validateWithdrawalsDelegated.
            // Verify that all key-hash withdrawal accounts have a DRep
            // delegation in gov.governance.vote_delegations.
            for reward_account in tx.body.withdrawals.keys() {
                if reward_account.len() >= 29 {
                    // Bit 4 of header: 0 = key credential, 1 = script credential.
                    // Reward address headers: 0xe0/0xe1 = key, 0xf0/0xf1 = script.
                    let is_script = reward_account[0] & 0x10 != 0;
                    if !is_script {
                        let key = common::reward_account_to_hash(reward_account);
                        if !gov.governance.vote_delegations.contains_key(&key) {
                            return Err(LedgerError::BlockTxValidationFailed {
                                slot: ctx.current_slot,
                                tx_hash: tx.hash.to_hex(),
                                errors: format!(
                                    "WithdrawalNotDelegated: key-hash credential {} \
                                     has no DRep delegation (PV10 requirement)",
                                    key.to_hex()
                                ),
                            });
                        }
                    }
                }
            }

            // Step 4: testIncompleteAndMissingWithdrawals.
            // Verify withdrawal amounts exactly match reward balances.
            for (reward_account, amount) in &tx.body.withdrawals {
                let key = common::reward_account_to_hash(reward_account);
                let balance = certs
                    .reward_accounts
                    .get(&key)
                    .copied()
                    .unwrap_or(Lovelace(0));
                if *amount != balance {
                    return Err(LedgerError::BlockTxValidationFailed {
                        slot: ctx.current_slot,
                        tx_hash: tx.hash.to_hex(),
                        errors: format!(
                            "WithdrawalAmountMismatch: withdrawal {} != reward balance {} \
                             for account {} (PV10 requirement)",
                            amount.0,
                            balance.0,
                            key.to_hex()
                        ),
                    });
                }
            }
        }

        // Step 5: Update DRep activity for voting DReps in this transaction.
        update_drep_expiries_for_tx(tx, ctx.current_epoch, gov, epochs);

        // Step 5b: updateDormantDRepExpiry — a tx carrying governance proposals
        // "refunds" accumulated dormant epochs to every DRep and resets the
        // dormant counter (Haskell `Cardano.Ledger.Conway.Rules.Certs`).
        update_dormant_drep_expiry_for_tx(tx, ctx.current_epoch, gov);

        // Step 6: Drain withdrawal accounts.
        common::drain_withdrawal_accounts(tx, certs);

        // Step 7: Process certificates (Shelley + Conway governance certs).
        //
        // Haskell processes certs in a single ordered pass per tx. Dugite
        // previously split this into two passes (Shelley then Conway) which
        // broke tx cert sequences that interleave the two cert families, for
        // example `[ConwayStakeDeregistration, ConwayStakeRegistration,
        // StakeDelegation]`: the Shelley pass inserted the delegation first,
        // then the Conway pass's DEREG wiped it. Now we walk certs in order
        // and dispatch each one to both handlers (non-matching cert variants
        // are no-ops), preserving Haskell's sequential semantics.
        for (cert_index, cert) in tx.body.certificates.iter().enumerate() {
            common::apply_shelley_cert(
                cert,
                cert_index,
                ctx.current_slot,
                ctx.tx_index,
                certs,
                epochs,
                gov,
            );
            apply_conway_cert(cert, ctx.current_epoch, certs, gov, epochs);
        }

        // Step 8: Apply GOV rule (votes + proposals).
        process_governance_votes_and_proposals(tx, ctx, gov, epochs);

        // Step 9: Apply UTxO changes and accumulate donation.
        let diff = common::apply_utxo_changes(tx, utxo, certs, epochs);

        // Conway-specific: accumulate treasury donations from this transaction.
        if let Some(donation) = tx.body.donation {
            utxo.pending_donations += donation;
        }

        Ok(diff)
    }

    /// Apply an invalid Conway transaction (IsValid=false, collateral consumption).
    ///
    /// Same as Babbage: collateral inputs are consumed, collateral_return creates
    /// a new UTxO if present, and the fee is total_collateral or computed from
    /// the difference.
    fn apply_invalid_tx(
        &self,
        tx: &Transaction,
        _mode: BlockValidationMode,
        _ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        epochs: &mut EpochSubState,
    ) -> Result<UtxoDiff, LedgerError> {
        let diff = common::apply_collateral_consumption(tx, utxo, certs, epochs);
        Ok(diff)
    }

    /// Process a Conway epoch boundary transition.
    ///
    /// Implements the Conway 13-step epoch transition pipeline. Steps that
    /// share logic with Shelley/Babbage delegate to the same code. Steps
    /// specific to Conway governance are implemented where possible and
    /// stubbed with deferral notes where the full logic is too complex to
    /// extract inline (governance ratification/enactment).
    ///
    /// The full governance ratification/enactment pipeline (~600 lines in
    /// `state/governance.rs`) will continue to be used by the old apply_block
    /// path until Task 12 migrates it.
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
        debug!("Conway epoch transition: -> {}", new_epoch.0);

        // Capture bprev BEFORE any param updates (nesBprev = nesBcur).
        let bprev_block_count = consensus.epoch_block_count;
        let bprev_blocks_by_pool = Arc::clone(&consensus.epoch_blocks_by_pool);

        // === Capture prevPParams BEFORE any PP updates ===
        //
        // Per Haskell `Cardano.Ledger.Conway.Rules.Epoch`, the new epoch's
        // `cgsPrevPParamsL` is set to `curPParams` BEFORE governance enactment
        // (`enactStateTransition`) updates `curPParams`. dugite previously
        // captured `old_proto_major` AFTER `ratify_proposals_impl` (line 779),
        // which read the POST-enactment value and caused `prev_pp_major` to
        // race ahead of Haskell by one boundary — silently breaking the RUPD
        // `prevPParams` semantic for boundaries that enact a ParameterChange
        // or HardForkInitiation. Issue #685.
        //
        // Conway: d is always 0 (fully decentralized) — Conway has no overlay
        // slots and PParams no longer carries `ppDG`.
        let old_d = dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        };
        let old_proto_major = epochs.protocol_params.protocol_version_major;
        let old_params = epochs.protocol_params.clone();

        // === Apply pre-Conway PPUP (era-crossing edge case) ===
        //
        // The Babbage→Conway HFC translation does NOT itself bump the
        // on-chain protocol version. The PV bump from 8 to 9 is carried by
        // the same Babbage-era `Update_Proposal` (TxBody key 6 PPUP) that
        // triggered the era boundary in the first place — its `protVer`
        // field encodes the target PV (`(9, 0)` on preview epoch 645).
        //
        // The shelley/babbage `process_epoch_transition` applies this PPUP
        // from `pending_pp_updates[new_epoch - 1]`. But the orchestrator
        // dispatches by `block.era`, so the first Conway block triggers
        // Conway's `process_epoch_transition` for the era-crossing boundary,
        // which previously did NOT process the legacy PPUP — leaving PV
        // stuck at 8 and breaking the Conway bootstrap-period semantics
        // (`protocol_version_major == 9` gate in governance/ratification).
        // That in turn caused the boundary-735→736 ParameterChange to never
        // ratify, dropping its 100K-ADA proposal-deposit refund and
        // cascading into the +17.23 ADA preview-epoch-881 treasury drift
        // reported in #685.
        //
        // After Conway era starts no further pre-Conway PPUP proposals can
        // be submitted (Conway TxBody silently drops key 6), so this block
        // is effectively a no-op on every Conway boundary except the
        // era-crossing one. Issue #685.
        // Haskell's `votedFuturePParams` (Shelley.Rules.Ppup) enacts a
        // pre-Conway PPUP only when a quorum of genesis delegates voted for
        // the byte-identical `PParamsUpdate` value; it never counts
        // distinct proposers and field-merges their proposals together.
        // Ties or a value short of quorum enact nothing. Issue #784.
        let lookup_epoch = EpochNo(new_epoch.0.saturating_sub(1));
        if let Some(proposals) = epochs.pending_pp_updates.remove(&lookup_epoch) {
            let proposal_map = crate::validation::ppup::fold_pp_proposals(&proposals);
            if let Some(winner) = crate::validation::ppup::voted_future_pparams(
                &proposal_map,
                ctx.update_quorum,
                &epochs.protocol_params,
            ) {
                super::shelley::apply_pp_update(&mut epochs.protocol_params, &winner);
                // #764 Part A — re-seed PlutusV3 after a pre-Conway PPUP wipe.
                //
                // At the Babbage→Conway boundary `on_era_transition` (block-apply
                // Step 2) seeds `cost_models.plutus_v3` from the Conway genesis,
                // but `process_epoch_transition` (Step 3, here) then applies any
                // pending PRE-CONWAY (Babbage) PPUP. A Babbage PPUP that carries
                // `cost_models` (mainnet ep506 has two such quorum-meeting PPUPs,
                // each `{PlutusV1, PlutusV2}` + protocol_major=9) makes
                // `shelley::apply_pp_update` WHOLESALE-REPLACE the cost_models
                // field (shelley.rs `params.cost_models = v.clone()`), wiping the
                // just-seeded V3 → `plutus_v3 = None` for the whole Conway era →
                // every PlutusV3 phase-2 eval falls back to the DEFAULT cost
                // model (budget-exhausted false rejections).
                //
                // Haskell's order is PPUP-first, translateEra-second:
                // `nextEpochPParams` applies the Babbage PPUP, THEN the hardfork's
                // `translateEra @Conway` → `upgradeConwayPParams` →
                // `updateCostModels` does `Map.union {V3_genesis} {V1_new,V2_new}`,
                // so V3 SURVIVES. We mirror that final state by re-inserting V3
                // here. We do NOT change `shelley::apply_pp_update` to per-language
                // merge: the oracle confirmed pre-Conway generic `applyPPUpdates`
                // REPLACES the whole cost_models field, so merging there would
                // itself diverge for an all-pre-Conway sync.
                if epochs.protocol_params.cost_models.plutus_v3.is_none() {
                    if let Some(genesis) = ctx.conway_genesis {
                        if let Some(ref v3) = genesis.plutus_v3_cost_model {
                            epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone());
                            debug!(
                                entries = v3.len(),
                                epoch = new_epoch.0,
                                "Re-seeded PlutusV3 cost model after pre-Conway PPUP \
                                 wholesale cost_models replace (Haskell translateEra \
                                 per-language insert; #764)"
                            );
                        }
                    }
                }
                debug!(
                    epoch = new_epoch.0,
                    new_pv_major = epochs.protocol_params.protocol_version_major,
                    "Pre-Conway PPUP applied at Conway epoch boundary (voted quorum, \
                     era-crossing edge case)"
                );
            }
        }
        // Clean up past-epoch proposals.
        epochs
            .pending_pp_updates
            .retain(|epoch, _| *epoch >= lookup_epoch);
        // Promote future proposals -> current (matches shelley.rs behaviour).
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

        // === Step 1: SNAP (snapshot rotation) ===
        // Flush pending treasury donations BEFORE snapshot.
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
                "Conway: flushed pending treasury donations"
            );
        }

        // Apply pending reward update (backward compat for old snapshots).
        if let Some(rupd) = epochs.pending_reward_update.take() {
            // Signed reserves adjustment — see issue #796.
            epochs.reserves.0 = apply_reserves_delta(epochs.reserves.0, rupd.delta_reserves);
            epochs.treasury.0 = epochs
                .treasury
                .0
                .checked_add(rupd.delta_treasury)
                .expect("RUPD delta_treasury overflows treasury u64");
            for (cred_hash, reward) in &rupd.rewards {
                if reward.0 > 0 {
                    if certs.reward_accounts.contains_key(cred_hash) {
                        *certs
                            .reward_accounts
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
            // `epoch_state_debug::maybe_dump`).  Mirrors the Shelley path.
            epochs.last_applied_rupd = Some(rupd);
        } else {
            // Clear any stale entry from a previous boundary so dumpers do
            // not double-report.
            epochs.last_applied_rupd = None;
        }

        // Compute and apply RUPD using GO snapshot + bprev + ss_fee.
        // Same logic as Shelley/Babbage — monetary expansion and per-pool reward
        // distribution using the GO snapshot captured two epochs ago.
        //
        // Issue #438: fire RUPD unconditionally at every boundary, even at
        // boundary 0→1 when GO/bprev/ss_fee are still empty.  Haskell's
        // `startStep` runs mid-epoch starting in epoch 0 and produces a
        // `RewardUpdate` with `ssFee = 0` (from `emptySnapShots`); that
        // update is applied at boundary 0→1, draining the genesis monetary
        // expansion's tau cut from reserves to treasury (~9M ADA on
        // preview).  Previously gating on `rupd_ready` left dugite with
        // that 9M ADA in reserves instead of treasury, compounding into
        // +4.887M ADA reserves excess by preview epoch 1269 and a
        // +25K-lovelace per-pool reward overshoot at every subsequent
        // boundary.
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
            // compute_reward_update expects &std::HashMap; convert once per epoch boundary.
            let reward_accounts_std: std::collections::HashMap<_, _> = certs
                .reward_accounts
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect();
            let rupd = crate::compute_reward_update(
                rupd_pp,
                &epochs.prev_d,
                epochs.prev_protocol_version_major,
                go_ref,
                &epochs.snapshots.bprev_blocks_by_pool,
                epochs.snapshots.ss_fee,
                epochs.reserves,
                epochs.treasury,
                &reward_accounts_std,
                // #11: pv≤6 prefilter uses the startStep-frozen fvAddrsRew set,
                // not boundary-time accounts (None ⇒ fall back to boundary).
                epochs.rupd_addrs_rew.as_deref(),
                ctx.epoch_length,
                ctx.shelley_transition_epoch,
                ctx.max_lovelace_supply,
            );

            // Issue #438/#471: per-boundary reward-debug dump.  No-op
            // unless the crate is built with `--features
            // reward-debug-dump` AND `DUGITE_REWARD_DEBUG_DUMP=<dir>` is
            // set at runtime.  Only fires when GO is non-empty (the
            // diagnostic only makes sense when there are pools to inspect).
            #[cfg(feature = "reward-debug-dump")]
            if let Some(go) = go_ref {
                // `certs.reward_accounts` is an imbl map (k-window sharing); the
                // debug dumper takes a plain `&HashMap`, so collect a snapshot.
                let ra_std: std::collections::HashMap<Hash32, Lovelace> = certs
                    .reward_accounts
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect();
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
                    &ra_std,
                    &rupd,
                );
            }

            // Apply RUPD: adjust reserves and treasury (signed — issue #796)
            epochs.reserves.0 = apply_reserves_delta(epochs.reserves.0, rupd.delta_reserves);
            epochs.treasury.0 = epochs
                .treasury
                .0
                .checked_add(rupd.delta_treasury)
                .expect("RUPD delta_treasury overflows treasury u64");

            // Distribute rewards to registered accounts; unregistered -> treasury
            for (cred_hash, reward) in &rupd.rewards {
                if reward.0 > 0 {
                    if certs.reward_accounts.contains_key(cred_hash) {
                        *certs
                            .reward_accounts
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
        // RUPD just consumed. Mirrors Haskell `applyRUpd`'s
        // `utxosFees -= ssFee` semantics so `utxosFees` is a multi-epoch
        // running total. See `shelley.rs` for the full rationale; the
        // reset to zero at the end of this function is removed below.
        let ss_fee_drained = epochs.snapshots.ss_fee;
        utxo.epoch_fees = Lovelace(utxo.epoch_fees.0.saturating_sub(ss_fee_drained.0));

        // Rotate snapshots: go <- set <- mark, capture fees.
        //
        // SNAP captures `ssFee` from the post-applyRUpd `utxosFees`, which
        // is `epoch_fees` after the drain above.
        let captured_fees = utxo.epoch_fees;
        epochs.snapshots.go = epochs.snapshots.set.take();
        epochs.snapshots.set = epochs.snapshots.mark.take();
        epochs.snapshots.ss_fee = captured_fees;
        epochs.snapshots.bprev_block_count = bprev_block_count;
        epochs.snapshots.bprev_blocks_by_pool = bprev_blocks_by_pool;
        epochs.snapshots.rupd_ready = true;

        // Handle needs_stake_rebuild flag.
        if epochs.needs_stake_rebuild {
            epochs.needs_stake_rebuild = false;
            debug!(
                epoch = new_epoch.0,
                "Conway epoch: needs_stake_rebuild flag cleared (rebuild deferred to orchestrator)"
            );
        }

        // Build pool_stake from current stake distribution + delegations.
        // Conway excludes pointer-addressed UTxO stake (ptr_stake_excluded = true).
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
            let total_stake = Lovelace(utxo_stake.0.saturating_add(reward_balance.0));
            *pool_stake.entry(*pool_id).or_insert(Lovelace(0)) += total_stake;
        }

        // Conway does NOT resolve pointer-addressed UTxO stake (excluded by TranslateEra).

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

        // Create the new mark snapshot.
        // Convert imbl::HashMap delegations → Arc<std::HashMap> for StakeSnapshot.
        let mark_delegations = std::sync::Arc::new(
            certs
                .delegations
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect::<std::collections::HashMap<_, _>>(),
        );
        epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: new_epoch,
            delegations: mark_delegations,
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
            }
        }

        // === Step 2: POOLREAP (pool retirements with deposit refunds) ===
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
                        *certs.reward_accounts.entry(op_key).or_insert(Lovelace(0)) += pool_deposit;
                    } else {
                        epochs.treasury.0 = epochs
                            .treasury
                            .0
                            .checked_add(pool_deposit.0)
                            .expect("treasury overflow on pool deposit refund");
                    }
                    certs
                        .delegations
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
        // Conway has no MIR certs so the pending maps stay empty and this
        // is a no-op; included only so a Babbage-loaded snapshot with
        // pending MIR is drained on the first Conway-era boundary.  See
        // issue #631.
        crate::state::certificates::apply_pending_mir(certs, epochs);

        // === Step 3: DRep pulser completion ===
        // The DRep distribution (voting power) is computed live within
        // ratify_proposals_impl, matching Haskell's pulser completion that
        // produces dpDRepDistr before ratification begins.

        // === Steps 4+5: Ratification, enactment, treasury withdrawals ===
        // Pass the OLD epoch (ctx.current_epoch) to ratify_proposals_impl, matching
        // Haskell's reCurrentEpoch from the DRep pulser. Proposals with
        // expires_epoch == current_epoch are still eligible for ratification;
        // they only expire AFTER ratification fails at this boundary.
        ratify_proposals_impl(ctx.current_epoch, epochs, certs, gov);

        // === Step 6: Deposit returns handled by ratify_proposals_impl above ===

        // === Step 7: GovState update handled by ratify_proposals_impl above ===

        // === Step 8: numDormantEpochs computation ===
        update_dormant_epochs(new_epoch, epochs, gov);

        // === Step 9: Prune expired committee members ===
        expire_committee_members(new_epoch, gov);
        // Haskell `updateCommitteeState` (Epoch.hs): prune hot-key auths +
        // resignations to the post-enactment committee membership (wipes all
        // on an enacted NoConfidence).
        crate::state::governance::prune_committee_state(gov);

        // DRep activity marking (mark inactive DReps)
        update_drep_activity(new_epoch, epochs, gov);

        // === Step 10: Flush donations (pending_donations -> treasury) ===
        // Already handled above in step 1 (before snapshot rotation), matching
        // Haskell's ordering where donations are flushed early.

        // === Step 11: totalObligation recalculation ===
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
                obl_stake, obl_pool, obl_drep, obl_proposal, "Conway: totalObligation recalculated"
            );
        }

        // === Step 12: HARDFORK check ===
        // HardForkInitiation actions set protocol_version during enactment
        // in ratify_proposals_impl. The consensus layer detects the version
        // bump and triggers the actual hardfork. No additional logic needed.

        // === Step 13: setFreshDRepPulsingState ===
        // Pass the OLD epoch (ctx.current_epoch) for the snapshot label, matching
        // the old LedgerState::process_epoch_transition which passes self.epoch
        // (before self.epoch = new_epoch).
        capture_governance_snapshots(ctx.current_epoch, epochs, certs, gov);

        // prevPParams was already captured at the top of this function
        // (BEFORE pre-Conway PPUP application and BEFORE
        // `ratify_proposals_impl` enactment), so `old_d` / `old_proto_major`
        // / `old_params` are bound in this scope.  Per Haskell
        // `Cardano.Ledger.Conway.Rules.Epoch`, `cgsPrevPParamsL` is set from
        // `curPParams` BEFORE `enactStateTransition` updates it — the
        // capture position is load-bearing for byte-exact RUPD math at
        // boundaries that enact a ParameterChange / HardForkInitiation.

        // Compute new epoch nonce (TICKN rule).
        let candidate = consensus.candidate_nonce;
        let prev_hash_nonce = consensus.last_epoch_block_nonce;

        let zero = Hash32::ZERO;
        consensus.epoch_nonce = if candidate == zero && prev_hash_nonce == zero {
            zero
        } else if candidate == zero {
            prev_hash_nonce
        } else if prev_hash_nonce == zero {
            candidate
        } else {
            let mut nonce_input = Vec::with_capacity(64);
            nonce_input.extend_from_slice(candidate.as_bytes());
            nonce_input.extend_from_slice(prev_hash_nonce.as_bytes());
            blake2b_256(&nonce_input)
        };

        // Update prevHashNonce to current labNonce for NEXT epoch.
        consensus.last_epoch_block_nonce = consensus.lab_nonce;

        // Set prevPParams from values captured BEFORE governance enactment.
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

    /// Evolve nonce state after a Conway block header.
    ///
    /// Same VRF-based nonce evolution as Babbage. Conway (proto >= 9) always
    /// has d = 0 (fully decentralized).
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

        // Conway (proto >= 9): d is always (0, 1) (fully decentralized).
        common::compute_shelley_nonce(
            header,
            ctx.current_slot,
            first_slot_of_current_epoch,
            first_slot_of_next_epoch,
            ctx.stability_window,
            0u64,
            1u64,
            consensus,
        );
    }

    /// Conway minimum fee: `min_fee_a * tx_size + min_fee_b`.
    ///
    /// Same linear fee formula as previous eras. Conway's tiered reference
    /// script fee (25KiB tiers, 1.2x multiplier) is an additional adjustment
    /// applied during transaction validation, not in this base min_fee method.
    fn min_fee(&self, tx: &Transaction, ctx: &RuleContext, _utxo: &UtxoSubState) -> u64 {
        let tx_size = tx.raw_cbor.as_ref().map_or(0, |b| b.len() as u64);
        ctx.params
            .min_fee_a
            .checked_mul(tx_size)
            .and_then(|product| product.checked_add(ctx.params.min_fee_b))
            .unwrap_or(u64::MAX)
    }

    /// Handle hard fork state transformations when entering Conway.
    ///
    /// Babbage -> Conway (TranslateEra). The Haskell spec lists 7 transformations:
    ///
    /// 1. Purge pointer-based stake from stake distribution (ptr_stake_excluded = true).
    /// 2. Create initial VState (DRep state) from ConwayGenesis.
    /// 3. Build VRF key hash -> pool ID map.
    /// 4. Create initial ConwayGovState (committee, constitution from genesis).
    /// 5. Reset utxosDonation to 0.
    /// 6. Recompute InstantStake (without pointer addresses).
    /// 7. Set initial DRep pulser state.
    ///
    /// All steps are implemented. Steps 2-4 seed governance state from
    /// ConwayGenesis when available. Steps 6-7 are handled implicitly by
    /// the incremental stake tracker and epoch boundary logic.
    fn on_era_transition(
        &self,
        from_era: Era,
        ctx: &RuleContext,
        utxo: &mut UtxoSubState,
        certs: &mut CertSubState,
        gov: &mut GovSubState,
        _consensus: &mut ConsensusSubState,
        epochs: &mut EpochSubState,
    ) -> Result<(), LedgerError> {
        // Guard: this rule implements the Babbage -> Conway TranslateEra only.
        //
        // Historically (pre-issue #462) Dijkstra was aliased to `ConwayRules`
        // in `crates/dugite-ledger/src/eras/mod.rs`, so this method was also
        // dispatched for the Conway -> Dijkstra boundary; re-running the
        // Babbage->Conway init steps there would have been destructive
        // (re-seeded DReps, overwritten committee/threshold/constitution,
        // zeroed in-flight donations).
        //
        // The alias is now removed and Dijkstra has its own `DijkstraRules`
        // with an explicit identity translation (issue #462), so this guard
        // is belt-and-braces. We keep it as defense-in-depth in case a
        // future orchestrator bug routes a non-Babbage from_era here
        // (issue #467 regression).
        if from_era != Era::Babbage {
            debug!(
                "Conway::on_era_transition called with from_era={:?}; skipping \
                 (only Babbage->Conway runs init logic; see issue #467)",
                from_era
            );
            return Ok(());
        }

        debug!(
            "{:?} -> Conway era transition: excluding pointer stake, resetting donations",
            from_era
        );

        // Step 0 (issue #481): Bump the on-chain protocol version to (9, 0).
        //
        // In cardano-node the Babbage→Conway era boundary is a hard fork.
        // `translateEra @ConwayEra @BabbageEra` for `PParams` copies the old
        // `ProtVer` across unchanged (`cppProtocolVersion = toNoUpdate
        // bppProtocolVersion`); the actual bump to PV9 is performed by the
        // consensus Hard Fork Combinator in the era-crossing tick.
        //
        // Issue #626: do NOT bump PV here. Haskell's HFC tick reacts to a
        // HardForkInitiation gov action that drives the PV bump (typically
        // 8→9) via UPEC/NEWPP. The PV write happens via the gov-action
        // enactment path, AFTER `prevPParams` is captured. Bumping in
        // `on_era_transition` (which fires at apply.rs Step 2, before
        // `process_epoch_transition`'s capture at Step 3) races ahead and
        // leaves `prev_pp.pv` one boundary too high — breaking
        // `hardforkBabbageForgoRewardPrefilter` semantics.
        //
        // Previously this was a workaround for the missing PPUP / gov-action
        // decoder which has now been fixed via #624. The HardForkInitiation
        // enactment path correctly drives the PV bump during normal
        // ratification — no on_era_transition write needed.

        // Step 1: Purge pointer-based stake from stake distribution.
        // Setting ptr_stake_excluded = true causes stake_routing() in common.rs
        // to return StakeRouting::None for pointer addresses, effectively
        // excluding pointer-addressed UTxO coins from the stake distribution
        // going forward.
        //
        // Also clear the ptr_stake map itself — matching Haskell's TranslateEra
        // which converts ShelleyInstantStake → ConwayInstantStake at the ERA
        // boundary, discarding `sisPtrStake` BEFORE the TICK/SNAP rules run.
        if !epochs.ptr_stake.is_empty() {
            let excluded_count = epochs.ptr_stake.len() as u64;
            let excluded_total: u64 = epochs.ptr_stake.values().sum();
            epochs.ptr_stake.clear();
            tracing::info!(
                excluded_count,
                excluded_total,
                excluded_ada = excluded_total / 1_000_000,
                "Conway: discarded pointer-addressed UTxO stake — \
                 matching TranslateEra ConwayInstantStake semantics"
            );
        }
        epochs.ptr_stake_excluded = true;

        // Issue #670: also discard the pointer→credential resolution map.
        // Haskell's `TranslateEra Babbage→Conway` for `DState` carries the
        // unified accounts forward but drops the legacy `dsPtrs` mapping
        // (pointer addresses are not modelled in Conway). dugite's
        // `from_haskell_snapshot` adapter mirrors this with
        // `pointer_map: HashMap::new()`; the live era-transition path
        // must do the same so a from-genesis replay matches an
        // ancillary-import byte-exact on `CertSubState::pointer_map`.
        if !certs.pointer_map.is_empty() {
            let pointer_count = certs.pointer_map.len();
            certs.pointer_map.clear();
            tracing::info!(
                pointer_count,
                "Conway: cleared pointer→credential resolution map \
                 (Haskell TranslateEra drops dsPtrs at the era boundary)"
            );
        }

        // Step 5: Reset utxosDonation to 0.
        utxo.pending_donations = Lovelace(0);

        // Steps 2 & 4: Seed initial DRep state, committee, and constitution
        // from ConwayGenesis config (matches Haskell's TranslateEra VState +
        // ConwayGovState construction).
        if let Some(genesis) = ctx.conway_genesis {
            let governance = Arc::make_mut(&mut gov.governance);

            // Step 2: Seed initial DReps from genesis.
            for (hash28, deposit) in &genesis.initial_dreps {
                // Hash28 -> Hash32: pad with trailing zeros (matching 28-byte
                // credential convention — VerificationKey type, last 4 bytes 0).
                let cred_hash = Hash32::from_bytes({
                    let mut buf = [0u8; 32];
                    buf[..28].copy_from_slice(hash28.as_bytes());
                    buf
                });
                governance
                    .dreps
                    .entry(cred_hash)
                    .or_insert(DRepRegistration {
                        credential: Credential::VerificationKey(*hash28),
                        deposit: Lovelace(*deposit),
                        drep_expiry: EpochNo(ctx.current_epoch.0 + ctx.params.drep_activity),
                        anchor: None,
                        registered_epoch: ctx.current_epoch,
                        active: true,
                    });
            }

            // Step 4a: Seed committee members from genesis.
            for (cred_bytes, expiry) in &genesis.committee_members {
                let cred = Hash32::from_bytes(*cred_bytes);
                governance
                    .committee_expiration
                    .insert(cred, EpochNo(*expiry));
            }

            // Step 4b: Set committee threshold from genesis.
            if let Some((num, den)) = genesis.committee_threshold {
                governance.committee_threshold = Some(dugite_primitives::transaction::Rational {
                    numerator: num,
                    denominator: den,
                });
            }

            // Step 4c: Seed constitution from genesis.
            if let Some(ref constitution) = genesis.constitution {
                governance.constitution = Some(constitution.clone());
            }

            let drep_count = genesis.initial_dreps.len();
            let committee_count = genesis.committee_members.len();
            tracing::info!(
                drep_count,
                committee_count,
                has_constitution = genesis.constitution.is_some(),
                "Conway: seeded governance state from ConwayGenesis"
            );
        }

        // Step 1b (PParams upgrade): seed the PlutusV3 cost model.
        //
        // Haskell `upgradeConwayPParams` builds the Conway `cppCostModels` as
        //   updateCostModels bppCostModels (mkCostModels {PlutusV3 -> ucppPlutusV3CostModel})
        // i.e. a per-language INSERT of V3 over the Babbage {V1,V2} map — V1 and
        // V2 are carried across unchanged; V3 comes from
        // `cgUpgradePParams.ucppPlutusV3CostModel` in conway-genesis.json.
        // (cardano-ledger eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs.)
        //
        // Without this, `cost_models.plutus_v3` stays `None` for the whole
        // session: `encode_language_views(has_v3=true)` emits an empty map and
        // every PlutusV3 tx gets a wrong `script_data_hash` (ScriptDataHashMismatch)
        // plus default-cost-model budgets (spurious "budget exhausted"). The
        // `is_none()` guard keeps a governance-updated V3 (e.g. the Plomin 297-entry
        // expansion) from being overwritten on a re-entry, and leaves V1/V2 alone.
        if let Some(genesis) = ctx.conway_genesis {
            if let Some(ref v3) = genesis.plutus_v3_cost_model {
                if epochs.protocol_params.cost_models.plutus_v3.is_none() {
                    epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone());
                    tracing::info!(
                        entries = v3.len(),
                        "Conway: seeded PlutusV3 cost model from genesis upgrade params \
                         (per-language insert preserving V1/V2)"
                    );
                }
            }
        }

        // Step 3: VRF key hash -> pool ID map.
        // In Haskell this is built for the DRep pulser to identify which pool
        // produced a block. Dugite uses pool_id directly from block headers,
        // so no VRF-to-pool map is needed.

        // Step 6: Recompute InstantStake without pointer addresses.
        // With ptr_stake_excluded=true (Step 1), stake_routing() returns None for
        // pointer addresses. The incremental stake tracker won't add pointer stake
        // going forward. The next SNAP's mark snapshot will be built without pointer
        // stake. No explicit full-UTxO-walk recompute is needed.

        // Step 7: Initial DRep pulser state.
        // The DRep distribution snapshot will be captured at the first Conway epoch
        // boundary (process_epoch_transition Step 13). No pre-seeding needed —
        // ratify_proposals falls back to live state when no snapshot exists.

        Ok(())
    }

    /// Compute the set of required VKey witnesses for a Conway transaction.
    ///
    /// Conway adds witness requirements beyond Babbage:
    /// - DRep voter key hashes (for DRep votes in voting_procedures)
    /// - CC hot key hashes (for committee votes in voting_procedures)
    /// - Proposer key hashes (for governance proposals)
    /// - Conway governance cert key hashes (DRep reg/unreg, CC auth/resign)
    /// - Plus all Babbage witness requirements (spending inputs, withdrawals,
    ///   Shelley certs, required_signers)
    fn required_witnesses(
        &self,
        tx: &Transaction,
        _ctx: &RuleContext,
        utxo: &UtxoSubState,
        _certs: &CertSubState,
        _gov: &GovSubState,
    ) -> HashSet<Hash28> {
        let mut witnesses = HashSet::new();

        // 1. Spending input pubkey hashes (reference_inputs excluded).
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

        // 3. Certificate key hashes (both Shelley and Conway certs).
        for cert in &tx.body.certificates {
            match cert {
                // Shelley certs
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

                // Conway governance certs: DRep registration/update/deregistration
                Certificate::RegDRep {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::UnregDRep {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::UpdateDRep {
                    credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                // Conway governance certs: vote delegation
                Certificate::VoteDelegation {
                    credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                // Conway governance certs: combined delegation certs
                Certificate::StakeVoteDelegation {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::RegStakeDeleg {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::RegStakeVoteDeleg {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::VoteRegDeleg {
                    credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                // Conway governance certs: stake registration/deregistration with deposit
                Certificate::ConwayStakeRegistration {
                    credential: Credential::VerificationKey(hash),
                    ..
                }
                | Certificate::ConwayStakeDeregistration {
                    credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                // Conway governance certs: CC hot key auth (cold key signs)
                Certificate::CommitteeHotAuth {
                    cold_credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                // Conway governance certs: CC resignation (cold key signs)
                Certificate::CommitteeColdResign {
                    cold_credential: Credential::VerificationKey(hash),
                    ..
                } => {
                    witnesses.insert(*hash);
                }

                _ => {}
            }
        }

        // 4. Required signers (Alonzo+ feature).
        for signer in &tx.body.required_signers {
            let mut key_bytes = [0u8; 28];
            key_bytes.copy_from_slice(&signer.as_bytes()[..28]);
            witnesses.insert(Hash28::from_bytes(key_bytes));
        }

        // 5. Voter key hashes from voting_procedures (Conway-specific).
        for voter in tx.body.voting_procedures.keys() {
            match voter {
                Voter::DRep(Credential::VerificationKey(hash)) => {
                    witnesses.insert(*hash);
                }
                Voter::ConstitutionalCommittee(Credential::VerificationKey(hash)) => {
                    // CC votes use the HOT key to sign, but the voter field
                    // contains the hot credential. The hot key hash IS the
                    // witness requirement.
                    witnesses.insert(*hash);
                }
                Voter::StakePool(pool_hash) => {
                    // SPO votes require the pool operator key hash (28-byte).
                    let mut key_bytes = [0u8; 28];
                    key_bytes.copy_from_slice(&pool_hash.as_bytes()[..28]);
                    witnesses.insert(Hash28::from_bytes(key_bytes));
                }
                _ => {}
            }
        }

        // 6. Proposer key hashes from proposal_procedures (Conway-specific).
        for proposal in &tx.body.proposal_procedures {
            // The return address is a reward account (29 bytes: header + 28-byte hash).
            // If it's a key credential (bit 4 of header = 0), the key hash is a
            // required witness.
            if proposal.return_addr.len() >= 29 && proposal.return_addr[0] & 0x10 == 0 {
                let mut key_bytes = [0u8; 28];
                key_bytes.copy_from_slice(&proposal.return_addr[1..29]);
                witnesses.insert(Hash28::from_bytes(key_bytes));
            }
        }

        witnesses
    }
}

// ---------------------------------------------------------------------------
// Conway-specific helper functions
// ---------------------------------------------------------------------------

/// Process Conway-era governance certificates from a transaction.
///
/// Handles certificate types introduced in the Conway era (CIP-1694):
/// - `ConwayStakeRegistration` / `ConwayStakeDeregistration` -- same as Shelley
///   but with explicit deposit amount.
/// - `RegDRep` -- register a new DRep.
/// - `UnregDRep` -- deregister a DRep (refund deposit).
/// - `UpdateDRep` -- update DRep metadata anchor.
/// - `VoteDelegation` -- delegate vote to a DRep.
/// - `StakeVoteDelegation` -- combined: delegate stake to pool + vote to DRep.
/// - `RegStakeDeleg` -- combined: register stake + delegate to pool.
/// - `RegStakeVoteDeleg` -- combined: register stake + delegate to pool + vote to DRep.
/// - `VoteRegDeleg` -- combined: register stake + delegate vote.
/// - `CommitteeHotAuth` -- authorize a hot key for a CC cold credential.
/// - `CommitteeColdResign` -- resign from the constitutional committee.
///
/// Shelley-era certificate types (StakeRegistration, StakeDeregistration, etc.)
/// are handled by `common::process_shelley_certs` and are NOT processed here.
/// Apply a single Conway-era certificate to the ledger state.
///
/// Non-Conway cert variants are ignored (no-op). Callers must invoke the
/// Shelley-era handler separately for those.
fn apply_conway_cert(
    cert: &Certificate,
    current_epoch: EpochNo,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
    epochs: &EpochSubState,
) {
    let drep_activity = epochs.protocol_params.drep_activity;
    let pv_major = epochs.protocol_params.protocol_version_major;
    let governance = Arc::make_mut(&mut gov.governance);
    let drep_expiry = {
        let base = current_epoch.0 + drep_activity;
        if pv_major >= 10 {
            EpochNo(base.saturating_sub(governance.num_dormant_epochs))
        } else {
            EpochNo(base)
        }
    };

    match cert {
        // Conway stake registration with explicit deposit (cert tag 7).
        // Same effect as Shelley StakeRegistration but the deposit is in the cert.
        Certificate::ConwayStakeRegistration {
            credential,
            deposit,
        } => {
            let key = credential.to_typed_hash32();
            certs
                .stake_distribution
                .stake_map
                .entry(key)
                .or_insert(Lovelace(0));
            certs.reward_accounts.entry(key).or_insert(Lovelace(0));
            if matches!(credential, Credential::Script(_)) {
                certs.script_stake_credentials.insert(key);
            }
            certs.total_stake_key_deposits += deposit.0;
            certs.stake_key_deposits.insert(key, deposit.0);
            debug!("Conway stake key registered: {}", key.to_hex());
        }

        // Conway stake deregistration with explicit refund (cert tag 8).
        Certificate::ConwayStakeDeregistration { credential, refund } => {
            let key = credential.to_typed_hash32();
            let stored_deposit = certs.stake_key_deposits.remove(&key).unwrap_or(refund.0);
            certs.total_stake_key_deposits = certs
                .total_stake_key_deposits
                .saturating_sub(stored_deposit);
            certs.delegations.remove(&key);
            certs.reward_accounts.remove(&key);
            governance.vote_delegations.remove(&key);
            certs.script_stake_credentials.remove(&key);
            certs.pointer_map.retain(|_, v| *v != key);
            debug!("Conway stake key deregistered: {}", key.to_hex());
        }

        // DRep registration.
        Certificate::RegDRep {
            credential,
            deposit,
            anchor,
        } => {
            let key = credential.to_typed_hash32();
            governance.dreps.insert(
                key,
                DRepRegistration {
                    credential: credential.clone(),
                    deposit: *deposit,
                    anchor: anchor.clone(),
                    registered_epoch: current_epoch,
                    drep_expiry,
                    active: true,
                },
            );
            governance.drep_registration_count += 1;
            debug!("DRep registered: {}", key.to_hex());
        }

        // DRep deregistration.
        Certificate::UnregDRep {
            credential,
            refund: _,
        } => {
            let key = credential.to_typed_hash32();
            governance.dreps.remove(&key);
            debug!("DRep deregistered: {}", key.to_hex());
        }

        // DRep metadata update.
        Certificate::UpdateDRep {
            credential, anchor, ..
        } => {
            let key = credential.to_typed_hash32();
            if let Some(drep) = governance.dreps.get_mut(&key) {
                drep.anchor = anchor.clone();
                drep.drep_expiry = drep_expiry;
                drep.active = true;
                debug!("DRep updated: {}", key.to_hex());
            }
        }

        // Vote delegation: delegate stake credential's vote to a DRep.
        Certificate::VoteDelegation { credential, drep } => {
            let key = credential.to_typed_hash32();
            governance.vote_delegations.insert(key, drep.clone());
            debug!("Vote delegated to DRep: {}", key.to_hex());
        }

        // Combined: delegate stake to pool + vote to DRep.
        Certificate::StakeVoteDelegation {
            credential,
            pool_hash,
            drep,
        } => {
            let key = credential.to_typed_hash32();
            certs.delegations.insert(key, *pool_hash);
            governance.vote_delegations.insert(key, drep.clone());
            debug!(
                "Stake+vote delegated: {} -> pool {} + DRep",
                key.to_hex(),
                pool_hash.to_hex()
            );
        }

        // Combined: register stake + delegate to pool.
        Certificate::RegStakeDeleg {
            credential,
            pool_hash,
            deposit,
        } => {
            let key = credential.to_typed_hash32();
            // Register stake.
            certs
                .stake_distribution
                .stake_map
                .entry(key)
                .or_insert(Lovelace(0));
            certs.reward_accounts.entry(key).or_insert(Lovelace(0));
            if matches!(credential, Credential::Script(_)) {
                certs.script_stake_credentials.insert(key);
            }
            certs.total_stake_key_deposits += deposit.0;
            certs.stake_key_deposits.insert(key, deposit.0);
            // Delegate to pool.
            certs.delegations.insert(key, *pool_hash);
            debug!(
                "RegStakeDeleg: {} -> pool {}",
                key.to_hex(),
                pool_hash.to_hex()
            );
        }

        // Combined: register stake + delegate to pool + delegate vote.
        Certificate::RegStakeVoteDeleg {
            credential,
            pool_hash,
            drep,
            deposit,
        } => {
            let key = credential.to_typed_hash32();
            // Register stake.
            certs
                .stake_distribution
                .stake_map
                .entry(key)
                .or_insert(Lovelace(0));
            certs.reward_accounts.entry(key).or_insert(Lovelace(0));
            if matches!(credential, Credential::Script(_)) {
                certs.script_stake_credentials.insert(key);
            }
            certs.total_stake_key_deposits += deposit.0;
            certs.stake_key_deposits.insert(key, deposit.0);
            // Delegate to pool + DRep.
            certs.delegations.insert(key, *pool_hash);
            governance.vote_delegations.insert(key, drep.clone());
            debug!(
                "RegStakeVoteDeleg: {} -> pool {} + DRep",
                key.to_hex(),
                pool_hash.to_hex()
            );
        }

        // Combined: register stake + delegate vote.
        Certificate::VoteRegDeleg {
            credential,
            drep,
            deposit,
        } => {
            let key = credential.to_typed_hash32();
            // Register stake.
            certs
                .stake_distribution
                .stake_map
                .entry(key)
                .or_insert(Lovelace(0));
            certs.reward_accounts.entry(key).or_insert(Lovelace(0));
            if matches!(credential, Credential::Script(_)) {
                certs.script_stake_credentials.insert(key);
            }
            certs.total_stake_key_deposits += deposit.0;
            certs.stake_key_deposits.insert(key, deposit.0);
            // Delegate vote.
            governance.vote_delegations.insert(key, drep.clone());
            debug!("VoteRegDeleg: {} + DRep", key.to_hex());
        }

        // CC hot key authorization.
        Certificate::CommitteeHotAuth {
            cold_credential,
            hot_credential,
        } => {
            let cold_key = cold_credential.to_typed_hash32();
            let hot_key = hot_credential.to_typed_hash32();
            governance.committee_hot_keys.insert(cold_key, hot_key);
            // Track script credentials for N2C query type fields.
            if matches!(cold_credential, Credential::Script(_)) {
                governance.script_committee_credentials.insert(cold_key);
            }
            if matches!(hot_credential, Credential::Script(_)) {
                governance.script_committee_hot_credentials.insert(cold_key);
            } else {
                // Re-auth with key hot key: remove previous script tracking.
                governance
                    .script_committee_hot_credentials
                    .remove(&cold_key);
            }
            debug!(
                "CC hot key authorized: {} -> {}",
                cold_key.to_hex(),
                hot_key.to_hex()
            );
        }

        // CC cold key resignation.
        Certificate::CommitteeColdResign {
            cold_credential,
            anchor,
        } => {
            let cold_key = cold_credential.to_typed_hash32();
            governance
                .committee_resigned
                .insert(cold_key, anchor.clone());
            if matches!(cold_credential, Credential::Script(_)) {
                governance.script_committee_credentials.insert(cold_key);
            }
            debug!("CC member resigned: {}", cold_key.to_hex());
        }

        // Skip Shelley certs and any unrecognized variants -- handled by
        // apply_shelley_cert or not relevant.
        _ => {}
    }
}

/// Update DRep expiry for DReps that vote in this transaction.
///
/// Per CIP-1694, a DRep's activity timer resets whenever they cast a vote.
/// This implements step 5 of the Conway LEDGER pipeline
/// (updateVotingDRepExpiries).
fn update_drep_expiries_for_tx(
    tx: &Transaction,
    current_epoch: EpochNo,
    gov: &mut GovSubState,
    epochs: &EpochSubState,
) {
    if tx.body.voting_procedures.is_empty() {
        return;
    }

    let activity = epochs.protocol_params.drep_activity;
    let base = current_epoch.0 + activity;
    let governance = Arc::make_mut(&mut gov.governance);
    let expiry = if epochs.protocol_params.protocol_version_major >= 10 {
        EpochNo(base.saturating_sub(governance.num_dormant_epochs))
    } else {
        EpochNo(base)
    };
    for voter in tx.body.voting_procedures.keys() {
        if let Voter::DRep(credential) = voter {
            let key = credential.to_typed_hash32();
            if let Some(drep) = governance.dreps.get_mut(&key) {
                drep.drep_expiry = expiry;
                drep.active = true;
            }
        }
    }
}

/// `updateDormantDRepExpiry` — Haskell `Cardano.Ledger.Conway.Rules.Certs`
/// (`updateDormantDRepExpiry`, cardano-ledger master @8595dbef): a transaction
/// that carries at least one governance proposal "refunds" the accumulated
/// dormant epochs to every registered DRep and resets the dormant counter.
///
/// Dormant epochs (boundaries with no live proposal, tracked by
/// `update_dormant_epochs`) are pre-subtracted from a DRep's expiry at
/// registration/vote time (`compute_drep_expiry` = `epoch + drep_activity -
/// num_dormant`). When governance resumes, each DRep's expiry is bumped back by
/// `num_dormant_epochs` so those quiet epochs don't count against it — guarded so
/// an already far-expired DRep (`expiry + num_dormant < current_epoch`) is NOT
/// revived. Then `num_dormant_epochs` is reset to 0.
///
/// Without this, `num_dormant_epochs` grows unbounded and EVERY DRep eventually
/// expires during quiet governance periods and never re-activates, emptying the
/// DRep voting distribution. That is invisible during Conway bootstrap (PV9,
/// where the DRep ratification threshold is 0) but at PV10 it makes every
/// DRep-gated action (ParameterChange / HardForkInitiation / …) fail to ratify —
/// which, via the un-refunded proposal deposit, was the true root cause of the
/// systematic preprod reward-calc divergence (the missing deposit understated the
/// proposer's stake, skewing `sigmaA` and every reward that epoch).
///
/// Fires once per proposal-carrying tx (Haskell runs it in the CERTS rule before
/// GOV validates the proposal; the tx fails atomically if GOV later rejects it,
/// so running it here is safe). Order vs `update_drep_expiries_for_tx` is
/// immaterial: the vote-time `- num_dormant` and this `+ num_dormant` cancel.
fn update_dormant_drep_expiry_for_tx(
    tx: &Transaction,
    current_epoch: EpochNo,
    gov: &mut GovSubState,
) {
    if tx.body.proposal_procedures.is_empty() {
        return;
    }
    let num_dormant = gov.governance.num_dormant_epochs;
    if num_dormant == 0 {
        return;
    }
    let governance = Arc::make_mut(&mut gov.governance);
    governance.num_dormant_epochs = 0;
    let keys: Vec<Hash32> = governance.dreps.keys().cloned().collect();
    for k in keys {
        if let Some(drep) = governance.dreps.get_mut(&k) {
            let actual = drep.drep_expiry.0.saturating_add(num_dormant);
            // Haskell: `if actualExpiry < currentEpoch then currentExpiry else actualExpiry`
            if actual >= current_epoch.0 {
                drep.drep_expiry = EpochNo(actual);
            }
        }
    }
}

/// Process governance votes and proposals from a transaction (GOV rule).
///
/// Implements step 8 of the Conway LEDGER pipeline:
/// - Record votes from voting_procedures.
/// - Register new governance proposals from proposal_procedures.
fn process_governance_votes_and_proposals(
    tx: &Transaction,
    ctx: &RuleContext,
    gov: &mut GovSubState,
    epochs: &EpochSubState,
) {
    // Fast path: a tx with neither votes nor proposals has nothing to do here.
    // `Arc::make_mut(&mut gov.governance)` below forces a clone of the entire
    // GovernanceState whenever the Arc is shared (e.g. a mark/set snapshot holds
    // a reference), so this guard skips the whole GOV rule for the overwhelming
    // majority of Conway txs that carry neither votes nor proposals.
    if tx.body.voting_procedures.is_empty() && tx.body.proposal_procedures.is_empty() {
        return;
    }

    let governance = Arc::make_mut(&mut gov.governance);

    // Process votes.
    //
    // Last-vote-wins per voter: Haskell GOV stores votes in per-action maps
    // keyed by voter (`gasCommitteeVotes` / `gasDRepVotes` /
    // `gasStakePoolVotes` are `Map voter Vote`, updated via `Map.insert`), so
    // a re-vote OVERWRITES the previous one. `votes_by_action`'s inner map is
    // keyed by `Voter`, so `insert` is an O(log n) last-vote-wins overwrite.
    for (voter, action_votes) in &tx.body.voting_procedures {
        for (action_id, vote_proc) in action_votes {
            governance
                .votes_by_action
                .entry(action_id.clone())
                .or_default()
                .insert(voter.clone(), vote_proc.clone());
        }
    }

    // Process proposals.
    //
    // Per Haskell `proposalsAddAction` (Proposals.hs), each proposal's
    // `prev_action_id` is validated before insertion:
    //
    //   (a) prev_action_id = None  AND  enacted root for this purpose is also None
    //       (genesis root — first ever proposal of this type)
    //   (b) prev_action_id = Some(id)  AND  id matches the last enacted root of the
    //       same purpose  OR  id is an active (in-flight) proposal
    //
    // Anything else is `InvalidPrevGovActionId` (tag 8) — the proposal is dropped.
    // This mirrors the identical check in `LedgerState::process_proposal` (governance.rs)
    // and ensures that stale cross-round `prev_action_id` values (e.g. an id from a
    // previous devnet boot) cannot sneak past the GOV rule via block-apply path.
    for (idx, proposal) in tx.body.proposal_procedures.iter().enumerate() {
        let action_id = GovActionId {
            transaction_id: tx.hash,
            action_index: idx as u32,
        };

        // Extract the prev_action_id for lineal-chain-purpose actions.
        let prev_id = match &proposal.gov_action {
            dugite_primitives::transaction::GovAction::ParameterChange {
                prev_action_id, ..
            }
            | dugite_primitives::transaction::GovAction::HardForkInitiation {
                prev_action_id,
                ..
            }
            | dugite_primitives::transaction::GovAction::NoConfidence { prev_action_id }
            | dugite_primitives::transaction::GovAction::UpdateCommittee {
                prev_action_id, ..
            }
            | dugite_primitives::transaction::GovAction::NewConstitution {
                prev_action_id, ..
            } => prev_action_id.as_ref(),
            dugite_primitives::transaction::GovAction::TreasuryWithdrawals { .. }
            | dugite_primitives::transaction::GovAction::InfoAction => None,
        };

        // Validate prev_action_id per Haskell `proposalsAddAction`.
        let valid = match prev_id {
            None => {
                // Case (a): genesis root — valid only when no prior action of this purpose
                // has been enacted (enacted root is None for this purpose).
                genesis_root_is_valid(&proposal.gov_action, governance)
            }
            Some(prev) => {
                // Case (b): prev must reference either the last enacted root for this
                // purpose OR an active (in-flight) proposal.
                let valid_root =
                    prev_action_matches_enacted_root(&proposal.gov_action, prev, governance);
                let in_flight = governance.proposals.contains_key(prev);
                valid_root || in_flight
            }
        };

        if !valid {
            // WARN, not debug: this runs on a block that consensus already
            // accepted, so a drop here is consequential and easy to miss.
            // Dropping a proposal strands its deposit (never refunded to the
            // return account, which silently lowers that account's stake and
            // therefore every pool's rewards) and leaves the corresponding
            // enacted root un-advanced — issue #898, where a Mithril import had
            // discarded `Proposals.pRoots` so every real `prev_action_id`
            // mismatched, wedging chain advance ~2 epochs later with a
            // 4-lovelace `WithdrawalAmountMismatch`.
            //
            // Haskell also drops such proposals without failing the tx, so this
            // is not necessarily a divergence — but on a confirmed block it is
            // rare enough, and expensive enough when wrong, to be worth a WARN.
            // The enacted roots are logged alongside so a `None` root (the #898
            // signature) is immediately visible.
            warn!(
                tx = %tx.hash.to_hex(),
                action_index = idx,
                action_type = ?std::mem::discriminant(&proposal.gov_action),
                prev_tx = prev_id.map(|p| p.transaction_id.to_hex()),
                prev_index = prev_id.map(|p| p.action_index),
                deposit = proposal.deposit.0,
                enacted_pparam_update = ?governance.enacted_pparam_update.as_ref().map(|i| i.transaction_id.to_hex()),
                enacted_hard_fork = ?governance.enacted_hard_fork.as_ref().map(|i| i.transaction_id.to_hex()),
                enacted_committee = ?governance.enacted_committee.as_ref().map(|i| i.transaction_id.to_hex()),
                enacted_constitution = ?governance.enacted_constitution.as_ref().map(|i| i.transaction_id.to_hex()),
                active_proposals = governance.proposals.len(),
                "InvalidPrevGovActionId (GOV rule): proposal dropped on a confirmed block — \
                 prev_action_id is neither a genesis root, the last enacted root, nor an \
                 active in-flight proposal. The deposit will never be refunded. If the \
                 enacted root for this action's purpose is None on a chain that has \
                 already enacted one, the ledger state was imported without \
                 `Proposals.pRoots` (see #898) and must be re-imported."
            );
            continue;
        }

        // #858: pvCanFollow gate for HardForkInitiation on the LIVE block-apply GOV
        // rule (previously absent — a skip-minor / skip-major target was admitted).
        // Uses the shared `preceedingHardFork` + `pvCanFollow` reachability check
        // (single source of truth with the dead-path processors, #812).
        {
            let cur_major = epochs.protocol_params.protocol_version_major;
            let cur_minor = epochs.protocol_params.protocol_version_minor;
            if hardfork_proposal_cant_follow(&proposal.gov_action, governance, cur_major, cur_minor)
            {
                debug!(
                    tx = %tx.hash.to_hex(),
                    action_index = idx,
                    cur_version = %format!("{cur_major}.{cur_minor}"),
                    "ProposalCantFollow (GOV rule): HardForkInitiation target does not follow \
                     the base protocol version — proposal dropped (#858)"
                );
                continue;
            }
        }

        let gov_action_lifetime = epochs.protocol_params.gov_action_lifetime;
        let proposal_state = ProposalState {
            procedure: proposal.clone(),
            proposed_epoch: ctx.current_epoch,
            expires_epoch: EpochNo(ctx.current_epoch.0 + gov_action_lifetime),
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
            // #799: monotonic submission index, read BEFORE the counter below
            // is incremented, so ties in ratification priority sort by
            // on-chain submission order (matching Haskell's stable
            // `reorderActions`), not by GovActionId (hash) order.
            submission_index: governance.proposal_count,
        };
        governance
            .proposals
            .insert(action_id.clone(), proposal_state);
        governance.proposal_count += 1;

        if let Some(tag) = gov_action_purpose_tag(&proposal.gov_action) {
            let prev = gov_action_raw_prev_id(&proposal.gov_action);
            forest_add_proposal(
                &action_id,
                prev.as_ref(),
                tag,
                &mut governance.proposal_roots,
                &mut governance.proposal_graph,
            );
        }
    }
}

/// Extract a Hash32 from a raw reward account byte string (29 bytes).
///
/// Mirrors the logic in common.rs but is kept local to avoid circular deps.
fn reward_account_to_hash(reward_account: &[u8]) -> Hash32 {
    let mut key_bytes = [0u8; 32];
    if reward_account.len() >= 29 {
        key_bytes[..28].copy_from_slice(&reward_account[1..29]);
        if reward_account[0] & 0x10 != 0 {
            key_bytes[28] = 0x01; // script credential
        }
    }
    Hash32::from_bytes(key_bytes)
}

// ---------------------------------------------------------------------------
// Internal helpers for collateral stub state
// ---------------------------------------------------------------------------

#[cfg(test)]
use crate::state::{EpochSnapshots, StakeDistributionState};
#[cfg(test)]
use dugite_primitives::protocol_params::ProtocolParameters;

/// Create a minimal CertSubState for testing collateral consumption.
#[cfg(test)]
fn make_empty_cert_sub() -> CertSubState {
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

/// Create a minimal EpochSubState for testing collateral consumption.
#[cfg(test)]
fn make_empty_epoch_sub() -> EpochSubState {
    EpochSubState {
        snapshots: EpochSnapshots::default(),
        treasury: Lovelace(0),
        reserves: Lovelace(0),
        pending_reward_update: None,
        last_applied_rupd: None,
        pending_pp_updates: std::collections::BTreeMap::new(),
        future_pp_updates: std::collections::BTreeMap::new(),
        needs_stake_rebuild: false,
        ptr_stake: HashMap::new(),
        ptr_stake_excluded: true, // Conway always excludes pointer stake.
        protocol_params: ProtocolParameters::mainnet_defaults(),
        prev_protocol_params: ProtocolParameters::mainnet_defaults(),
        prev_protocol_version_major: 9,
        prev_d: dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        },
        rupd_addrs_rew: None,
        pending_avvm_return: 0,
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
        BlockValidationMode, GovernanceState, PoolRegistration, StakeDistributionState,
    };
    use crate::utxo::UtxoSet;
    use crate::utxo_diff::DiffSeq;
    use dugite_primitives::address::Address;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{
        Anchor, DRep, OutputDatum, ProposalProcedure, TransactionBody, TransactionInput,
        TransactionOutput, TransactionWitnessSet, Vote, VotingProcedure,
    };
    use dugite_primitives::value::Lovelace;
    use dugite_primitives::value::Value;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_conway_ctx(params: &ProtocolParameters) -> RuleContext<'_> {
        let delegates = Box::leak(Box::new(HashMap::new()));
        RuleContext {
            params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: Era::Conway,
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
            ptr_stake_excluded: true, // Conway
            protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_params: ProtocolParameters::mainnet_defaults(),
            prev_protocol_version_major: 9,
            prev_d: dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            rupd_addrs_rew: None,
            pending_avvm_return: 0,
        }
    }

    fn make_consensus_sub() -> ConsensusSubState {
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
            era: Era::Conway,
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

    /// Verify that EraRulesImpl::for_era correctly maps Conway.
    #[test]
    fn test_era_rules_impl_for_conway() {
        assert!(matches!(
            EraRulesImpl::for_era(Era::Conway),
            EraRulesImpl::Conway(_)
        ));
    }

    /// validate_block_body returns Ok for an empty Conway block (zero redeemers,
    /// so total ExUnits is 0 and the per-block budget + ref-script-size checks
    /// trivially pass). The budget enforcement itself is exercised by
    /// `common::validate_block_ex_units` tests.
    #[test]
    fn test_validate_block_body_succeeds() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let block = Block {
            era: Era::Conway,
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
                protocol_version: dugite_primitives::block::ProtocolVersion { major: 9, minor: 0 },
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

    /// Apply a valid Conway transaction that spends a UTxO and produces a new one.
    #[test]
    fn test_apply_valid_tx_basic_utxo() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

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

        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 1);
        assert_eq!(utxo.epoch_fees.0, 200_000);
        assert!(!utxo.utxo_set.contains(&input));
    }

    /// Apply a valid Conway transaction with a treasury donation.
    #[test]
    fn test_apply_valid_tx_with_donation() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAA, 0);
        let spent_output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let new_output = make_output(addr, 9_500_000);
        let mut tx = make_tx(0x01, vec![input], vec![new_output], 200_000);
        tx.body.donation = Some(Lovelace(300_000));

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
        // Donation should be accumulated in pending_donations.
        assert_eq!(utxo.pending_donations.0, 300_000);
    }

    /// Issue #678: a Conway tx declaring `currentTreasuryValue` that does NOT
    /// match `casTreasury` must fail HARD in `ValidateAll` mode. This mirrors
    /// Haskell's `validateTreasuryValue` under `ApplySTSOpts { asoValidation =
    /// ValidateAll }` (live `tickThenApply` path).
    #[test]
    fn test_apply_valid_tx_treasury_mismatch_validate_all_fails() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAB, 0);
        let spent_output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.treasury = Lovelace(5_216_453_823_257_770);

        let new_output = make_output(addr, 9_700_000);
        let mut tx = make_tx(0x01, vec![input], vec![new_output], 100_000);
        // Declared treasury intentionally differs from epochs.treasury by 17.23 ADA.
        tx.body.treasury_value = Some(Lovelace(5_216_453_806_026_839));

        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ValidateAll,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );
        let err = result.expect_err("ValidateAll must reject treasury mismatch");
        let msg = err.to_string();
        assert!(
            msg.contains("MismatchedTreasuryValue"),
            "expected MismatchedTreasuryValue, got: {msg}"
        );
    }

    /// Issue #678: in `ApplyOnly` mode (= Haskell `reapplySTS` /
    /// `ValidateNone`), a `currentTreasuryValue` mismatch must NOT block
    /// block application. Replay of blocks already in the ImmutableDB —
    /// including Mithril import, rollback replay, self-forged blocks, and
    /// from-genesis chunk replay — proceeds normally. This was the
    /// regression that halted preview replay at slot 76172461.
    #[test]
    fn test_apply_valid_tx_treasury_mismatch_apply_only_succeeds() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);
        let input = make_input(0xAB, 0);
        let spent_output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.treasury = Lovelace(5_216_453_823_257_770);

        let new_output = make_output(addr, 9_700_000);
        let mut tx = make_tx(0x02, vec![input], vec![new_output], 100_000);
        tx.body.treasury_value = Some(Lovelace(5_216_453_806_026_839));

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
            "ApplyOnly must skip MismatchedTreasuryValue (replay parity \
             with Haskell reapplySTS): {result:?}"
        );
        // Treasury is NOT mutated by the check — dugite's value stands.
        assert_eq!(epochs.treasury.0, 5_216_453_823_257_770);
    }

    /// Byte-exact match: when the declared value equals `casTreasury`, both
    /// modes accept the tx without error.
    #[test]
    fn test_apply_valid_tx_treasury_match_both_modes_succeed() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);

        for mode in [
            BlockValidationMode::ValidateAll,
            BlockValidationMode::ApplyOnly,
        ] {
            let input = make_input(0xAC, 0);
            let spent_output = make_output(addr.clone(), 10_000_000);
            let mut utxo = make_utxo_sub(vec![(input.clone(), spent_output)]);
            let mut certs = make_cert_sub();
            let mut gov = make_gov_sub();
            let mut epochs = make_epoch_sub();
            epochs.treasury = Lovelace(5_216_453_806_026_839);

            let new_output = make_output(addr.clone(), 9_700_000);
            let mut tx = make_tx(0x03, vec![input], vec![new_output], 100_000);
            tx.body.treasury_value = Some(Lovelace(5_216_453_806_026_839));

            let result = rules.apply_valid_tx(
                &tx,
                mode,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            );
            assert!(result.is_ok(), "mode={mode:?}: {result:?}");
        }
    }

    /// Apply an invalid Conway transaction with collateral return.
    #[test]
    fn test_apply_invalid_tx_with_collateral_return() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let key_hash = Hash28::from_bytes([0x42; 28]);
        let addr = make_enterprise_address(key_hash);

        let collateral_input = make_input(0xCC, 0);
        let collateral_output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(collateral_input.clone(), collateral_output)]);

        let mut tx = make_tx(0x02, vec![], vec![], 0);
        tx.is_valid = false;
        tx.body.collateral = vec![collateral_input.clone()];
        tx.body.collateral_return = Some(make_output(addr, 8_000_000));
        tx.body.total_collateral = Some(Lovelace(2_000_000));

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

        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.inserts.len(), 1);
        assert_eq!(utxo.epoch_fees.0, 2_000_000);
    }

    /// Conway min_fee matches the linear formula.
    #[test]
    fn test_min_fee_linear() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.min_fee_a = 44;
        params.min_fee_b = 155381;
        let ctx = make_conway_ctx(&params);
        let utxo = make_utxo_sub(vec![]);

        let tx = make_tx(0x01, vec![], vec![], 0);
        let fee = rules.min_fee(&tx, &ctx, &utxo);
        assert_eq!(fee, 44 * 200 + 155381);
    }

    /// on_era_transition sets ptr_stake_excluded and resets donations.
    #[test]
    fn test_on_era_transition_excludes_pointer_stake() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        utxo.pending_donations = Lovelace(500_000);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = false;

        let result = rules.on_era_transition(
            Era::Babbage,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut consensus,
            &mut epochs,
        );
        assert!(result.is_ok());
        // Pointer stake should be excluded after transition.
        assert!(epochs.ptr_stake_excluded);
        // Donations should be reset.
        assert_eq!(utxo.pending_donations.0, 0);
    }

    /// Issue #481: the Babbage→Conway hard fork bumps protocol_version to (9,0).
    ///
    /// In cardano-node this bump is driven by the consensus HFC (the ledger
    /// translation itself just copies the old PV across), but in dugite the
    /// ledger owns era transitions, so it has to write the new PV here.
    /// Without this, dugite stays at PV8 forever (no governance HardForkInitiation
    /// before PV10) → the bootstrap branch (`pv == 9`) never fires, so any
    /// preview-era ParameterChange ratified with only a CC vote (which is the
    /// only thing that should pass in bootstrap) is silently dropped, leaving
    /// 100K-ADA proposal deposits unrefunded at every such boundary.
    ///
    /// Concrete observed symptom: at preview boundary 735→736 a ParameterChange
    /// ratified with only a CC vote (DRep+SPO=0) was enacted in Haskell, with
    /// its and its two siblings' 100K-ADA deposits refunded.  Dugite never
    /// enacted any of the three because it was stuck on PV8 → treasury and
    /// stake-snapshot drift versus Koios beginning at e736 and e738
    /// respectively.
    #[test]
    fn test_on_era_transition_babbage_to_conway_does_not_bump_pv() {
        // After issue #626 fix: on_era_transition must NOT bump PV. Haskell's
        // Babbage→Conway HFC tick is driven by a HardForkInitiation governance
        // action ratified via the Conway-era governance pipeline (RATIFY →
        // ENACT). The PV bump (typically 8→9) happens in `enactmentTransition`
        // (`Conway/Rules/Enact.hs`) and is written into `curPParams` via
        // `updateRewards`, AFTER `prevPParams` is captured. Bumping in
        // `on_era_transition` (which fires at apply.rs Step 2, before
        // `process_epoch_transition`'s capture at Step 3) races ahead and
        // leaves `prev_pp.pv` one boundary too high — same class of bug as
        // the Babbage on_era_transition workaround that was removed.
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8;
        params.protocol_version_minor = 0;
        let ctx = make_conway_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params.protocol_version_major = 8;
        epochs.protocol_params.protocol_version_minor = 0;

        let result = rules.on_era_transition(
            Era::Babbage,
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
            "on_era_transition must NOT bump PV — HardForkInitiation gov action does that",
        );
        assert_eq!(
            epochs.protocol_params.protocol_version_minor, 0,
            "Babbage→Conway translation sets protocol_version_minor to 0",
        );
    }

    /// Defensive: a non-Babbage from_era must NOT bump the protocol version
    /// (we already guard against re-running init logic; this confirms the new
    /// PV write is similarly gated).
    #[test]
    fn test_on_era_transition_non_babbage_does_not_set_pv9() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params.protocol_version_major = 10;
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
            epochs.protocol_params.protocol_version_major, 10,
            "non-Babbage from_era must not clobber the protocol version",
        );
    }

    /// Process Conway DRep registration certificate.
    #[test]
    fn test_conway_drep_registration() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let drep_key = Hash28::from_bytes([0xDD; 28]);
        let drep_cred = Credential::VerificationKey(drep_key);
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::RegDRep {
            credential: drep_cred.clone(),
            deposit: Lovelace(500_000_000),
            anchor: None,
        }];

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

        let key = drep_cred.to_typed_hash32();
        assert!(gov.governance.dreps.contains_key(&key));
        let drep = &gov.governance.dreps[&key];
        assert_eq!(drep.deposit.0, 500_000_000);
        assert!(drep.active);
        assert_eq!(drep.registered_epoch, EpochNo(5));
    }

    /// Process Conway vote delegation certificate.
    #[test]
    fn test_conway_vote_delegation() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let stake_key = Hash28::from_bytes([0xAA; 28]);
        let cred = Credential::VerificationKey(stake_key);
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::VoteDelegation {
            credential: cred.clone(),
            drep: DRep::Abstain,
        }];

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

        let key = cred.to_typed_hash32();
        assert_eq!(
            gov.governance.vote_delegations.get(&key),
            Some(&DRep::Abstain)
        );
    }

    /// Process Conway committee hot key authorization.
    #[test]
    fn test_conway_committee_hot_auth() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let cold_key = Hash28::from_bytes([0xC0; 28]);
        let hot_key = Hash28::from_bytes([0xBB; 28]);
        let cold_cred = Credential::VerificationKey(cold_key);
        let hot_cred = Credential::VerificationKey(hot_key);
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::CommitteeHotAuth {
            cold_credential: cold_cred.clone(),
            hot_credential: hot_cred.clone(),
        }];

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

        let cold_hash = cold_cred.to_typed_hash32();
        let hot_hash = hot_cred.to_typed_hash32();
        assert_eq!(
            gov.governance.committee_hot_keys.get(&cold_hash),
            Some(&hot_hash)
        );
    }

    /// Governance votes are recorded correctly.
    #[test]
    fn test_conway_governance_votes() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let drep_key = Hash28::from_bytes([0xDD; 28]);
        let drep_cred = Credential::VerificationKey(drep_key);
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 0,
        };
        let vote_proc = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        let mut action_votes = BTreeMap::new();
        action_votes.insert(action_id.clone(), vote_proc);
        tx.body
            .voting_procedures
            .insert(Voter::DRep(drep_cred), action_votes);

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

        assert!(gov.governance.votes_by_action.contains_key(&action_id));
        assert_eq!(gov.governance.votes_by_action[&action_id].len(), 1);
    }

    /// Governance proposals are recorded correctly.
    #[test]
    fn test_conway_governance_proposals() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.gov_action_lifetime = 6;
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params.gov_action_lifetime = 6;

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.proposal_procedures = vec![ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0xe0; 29],
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            anchor: Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::ZERO,
            },
        }];

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

        assert_eq!(gov.governance.proposals.len(), 1);
        assert_eq!(gov.governance.proposal_count, 1);
        let (_, ps) = gov.governance.proposals.iter().next().unwrap();
        assert_eq!(ps.proposed_epoch, EpochNo(5));
        assert_eq!(ps.expires_epoch, EpochNo(11));
    }

    /// Conway epoch transition flushes donations and prunes expired CC members.
    #[test]
    fn test_process_epoch_transition_conway() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        utxo.pending_donations = Lovelace(1_000_000);
        utxo.epoch_fees = Lovelace(500_000);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.treasury = Lovelace(100_000_000);
        let mut consensus = make_consensus_sub();

        // Add an expired CC member (expired at epoch 4, we are transitioning to 6).
        let expired_cc = Hash32::from_bytes([0xCC; 32]);
        Arc::make_mut(&mut gov.governance)
            .committee_expiration
            .insert(expired_cc, EpochNo(4));
        Arc::make_mut(&mut gov.governance)
            .committee_hot_keys
            .insert(expired_cc, Hash32::from_bytes([0xBB; 32]));

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

        // Donations flushed to treasury.
        assert_eq!(utxo.pending_donations.0, 0);
        assert_eq!(epochs.treasury.0, 101_000_000);

        // Issue #670: `epoch_fees` mirrors Haskell `utxosFees` and is a
        // multi-epoch running total — it is NOT reset at every boundary.
        // applyRUpd drains it by the prior boundary's `ssFee` (zero here
        // because `make_epoch_sub` has `ss_fee = Lovelace(0)`), so the
        // 500_000 lovelace we seeded carries forward verbatim and is now
        // also reflected in `epochs.snapshots.ss_fee` (the post-drain
        // capture for the next RUPD's `startStep`).
        assert_eq!(utxo.epoch_fees.0, 500_000);
        assert_eq!(epochs.snapshots.ss_fee.0, 500_000);

        // Expired CC member RETAINED in the map (issue #433): Haskell ledger
        // keeps the entry and surfaces it as `MemberStatus=Expired` at query
        // time; physical removal would (a) make the CC state query undercount,
        // and (b) drop authorization context if the same cold credential is
        // later re-elected with a fresh `validUntil`. Ratification filters
        // expired members in-place, so retention does not affect voting weight.
        assert!(
            gov.governance
                .committee_expiration
                .contains_key(&expired_cc),
            "expired CC member must be retained for status=Expired surfacing"
        );
    }

    /// Conway epoch transition handles pool retirement.
    #[test]
    fn test_process_epoch_transition_pool_retirement() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        // Register a pool and schedule retirement at epoch 6.
        let pool_id = Hash28::from_bytes([0xAA; 28]);
        let mut reward_addr = vec![0xe0u8];
        reward_addr.extend_from_slice(&[0xBB; 28]);
        let pool_reg = PoolRegistration {
            pool_id,
            vrf_keyhash: Hash32::ZERO,
            pledge: Lovelace(100_000),
            cost: Lovelace(340_000_000),
            margin_numerator: 1,
            margin_denominator: 100,
            reward_account: reward_addr.clone(),
            owners: vec![pool_id],
            relays: vec![],
            metadata_url: None,
            metadata_hash: None,
        };
        Arc::make_mut(&mut certs.pool_params).insert(pool_id, pool_reg);
        certs.pool_deposits.insert(pool_id, 500_000_000);
        certs.pending_retirements.insert(pool_id, EpochNo(6));

        // Create the reward account so the deposit can be refunded.
        let op_key = reward_account_to_hash(&reward_addr);
        certs.reward_accounts.insert(op_key, Lovelace(0));

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

        // Pool should be removed.
        assert!(!certs.pool_params.contains_key(&pool_id));
        assert!(!certs.pool_deposits.contains_key(&pool_id));
        // Deposit refunded to reward account.
        assert_eq!(
            certs.reward_accounts.get(&op_key),
            Some(&Lovelace(500_000_000))
        );
    }

    /// required_witnesses includes DRep voter keys and proposer keys.
    #[test]
    fn test_required_witnesses_conway_governance() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let utxo = make_utxo_sub(vec![]);
        let certs = make_cert_sub();
        let gov = make_gov_sub();

        let drep_key = Hash28::from_bytes([0xDD; 28]);
        let drep_cred = Credential::VerificationKey(drep_key);
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 0,
        };
        let vote_proc = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        let mut action_votes = BTreeMap::new();
        action_votes.insert(action_id, vote_proc);
        tx.body
            .voting_procedures
            .insert(Voter::DRep(drep_cred), action_votes);

        // Add a proposal with a key-hash return address.
        let mut return_addr = vec![0xe0u8];
        return_addr.extend_from_slice(&[0xBB; 28]);
        tx.body.proposal_procedures = vec![ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr,
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            anchor: Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::ZERO,
            },
        }];

        let witnesses = rules.required_witnesses(&tx, &ctx, &utxo, &certs, &gov);

        // DRep voter key should be required.
        assert!(witnesses.contains(&drep_key));
        // Proposer return address key should be required.
        assert!(witnesses.contains(&Hash28::from_bytes([0xBB; 28])));
        assert_eq!(witnesses.len(), 2);
    }

    /// DRep activity is updated when they cast a vote.
    #[test]
    fn test_drep_activity_updated_on_vote() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        // Register a DRep at epoch 2.
        let drep_key = Hash28::from_bytes([0xDD; 28]);
        let drep_cred = Credential::VerificationKey(drep_key);
        let key = drep_cred.to_typed_hash32();
        Arc::make_mut(&mut gov.governance).dreps.insert(
            key,
            DRepRegistration {
                credential: drep_cred.clone(),
                deposit: Lovelace(500_000_000),
                anchor: None,
                registered_epoch: EpochNo(2),
                drep_expiry: EpochNo(22), // epoch 2 + drep_activity 20
                active: true,
            },
        );

        // DRep casts a vote at epoch 5.
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 0,
        };
        let vote_proc = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        let mut action_votes = BTreeMap::new();
        action_votes.insert(action_id, vote_proc);
        tx.body
            .voting_procedures
            .insert(Voter::DRep(drep_cred), action_votes);

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

        // DRep expiry should be updated: epoch 5 + drep_activity 20 = 25.
        let drep = &gov.governance.dreps[&key];
        assert_eq!(drep.drep_expiry, EpochNo(25));
    }

    /// `updateDormantDRepExpiry`: a proposal-carrying tx refunds accumulated
    /// dormant epochs to every DRep and resets `num_dormant_epochs`, but does NOT
    /// revive a DRep whose bumped expiry would still be in the past. Regression
    /// for the systematic preprod reward-calc divergence: without the refund,
    /// `num_dormant_epochs` grew unbounded and every DRep expired during quiet
    /// governance periods, emptying the DRep distribution and blocking every
    /// PV10 ParameterChange from ratifying (its unrefunded deposit then skewed
    /// stake and rewards).
    #[test]
    fn test_dormant_drep_expiry_refunded_on_proposal() {
        let mut gov = make_gov_sub();
        let cred_a = Credential::VerificationKey(Hash28::from_bytes([0xA1; 28]));
        let cred_b = Credential::VerificationKey(Hash28::from_bytes([0xB2; 28]));
        let key_a = cred_a.to_typed_hash32();
        let key_b = cred_b.to_typed_hash32();
        {
            let g = Arc::make_mut(&mut gov.governance);
            g.num_dormant_epochs = 29;
            // DRep A: expiry 22 → bumped 22+29=51 (>= current_epoch 40) → refunded.
            g.dreps.insert(
                key_a,
                DRepRegistration {
                    credential: cred_a,
                    deposit: Lovelace(500_000_000),
                    anchor: None,
                    registered_epoch: EpochNo(2),
                    drep_expiry: EpochNo(22),
                    active: false,
                },
            );
            // DRep B: far-expired, expiry 3 → bumped 3+29=32 (< 40) → NOT revived.
            g.dreps.insert(
                key_b,
                DRepRegistration {
                    credential: cred_b,
                    deposit: Lovelace(500_000_000),
                    anchor: None,
                    registered_epoch: EpochNo(2),
                    drep_expiry: EpochNo(3),
                    active: false,
                },
            );
        }

        // A tx WITHOUT proposals must not touch anything.
        let no_prop = make_tx(0x01, vec![], vec![], 0);
        update_dormant_drep_expiry_for_tx(&no_prop, EpochNo(40), &mut gov);
        assert_eq!(gov.governance.num_dormant_epochs, 29);
        assert_eq!(gov.governance.dreps[&key_a].drep_expiry, EpochNo(22));

        // A tx carrying a proposal refunds the dormant epochs and resets.
        let mut tx = make_tx(0x01, vec![], vec![], 0);
        let mut return_addr = vec![0xe0u8];
        return_addr.extend_from_slice(&[0xBB; 28]);
        tx.body.proposal_procedures = vec![ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr,
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            anchor: Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::ZERO,
            },
        }];
        update_dormant_drep_expiry_for_tx(&tx, EpochNo(40), &mut gov);

        assert_eq!(gov.governance.num_dormant_epochs, 0, "counter reset");
        assert_eq!(
            gov.governance.dreps[&key_a].drep_expiry,
            EpochNo(51),
            "in-window DRep expiry bumped by num_dormant"
        );
        assert_eq!(
            gov.governance.dreps[&key_b].drep_expiry,
            EpochNo(3),
            "far-expired DRep NOT revived (guard: bumped 32 < current 40)"
        );

        // Idempotent: a second proposal tx (num_dormant now 0) is a no-op.
        update_dormant_drep_expiry_for_tx(&tx, EpochNo(41), &mut gov);
        assert_eq!(gov.governance.dreps[&key_a].drep_expiry, EpochNo(51));
    }

    /// Conway DRep deregistration removes the DRep.
    #[test]
    fn test_conway_drep_deregistration() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let drep_key = Hash28::from_bytes([0xDD; 28]);
        let drep_cred = Credential::VerificationKey(drep_key);
        let key = drep_cred.to_typed_hash32();
        Arc::make_mut(&mut gov.governance).dreps.insert(
            key,
            DRepRegistration {
                credential: drep_cred.clone(),
                deposit: Lovelace(500_000_000),
                anchor: None,
                registered_epoch: EpochNo(2),
                drep_expiry: EpochNo(22),
                active: true,
            },
        );

        let mut tx = make_tx(0x01, vec![], vec![], 0);
        tx.body.certificates = vec![Certificate::UnregDRep {
            credential: drep_cred,
            refund: Lovelace(500_000_000),
        }];

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
        assert!(!gov.governance.dreps.contains_key(&key));
    }

    /// Regression test for the cstreamer epoch-890 divergence: in Conway era,
    /// a tx containing `[ConwayStakeDeregistration, ConwayStakeRegistration,
    /// StakeDelegation]` (in that cert order) must end with the credential
    /// still registered AND delegated to the pool.
    ///
    /// Previously the Conway era applied all Shelley certs in one pass then
    /// all Conway certs in a second pass. That made the Shelley `StakeDelegation`
    /// fire before the Conway `ConwayStakeDeregistration` even though the
    /// on-chain cert order was the opposite, so the subsequent DEREG wiped
    /// the just-inserted delegation. A script stake credential on preview
    /// (`c6a4349e...`, 1.39 B lovelace) dropped out of `delegations` as a
    /// result and never re-entered any mark snapshot, compounding into a
    /// persistent activeStake/reserves divergence vs the Haskell reference.
    #[test]
    fn test_interleaved_dereg_reg_deleg_preserves_delegation() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);

        let script_hash = Hash28::from_bytes([0xc6; 28]);
        let cred = Credential::Script(script_hash);
        let key = cred.to_typed_hash32();
        let pool_id = Hash28::from_bytes([0x24; 28]);

        let mut certs = make_cert_sub();
        // Pre-state: credential is registered + delegated to the pool (as it
        // would be at the start of the suspect tx on-chain).
        certs.delegations.insert(key, pool_id);
        certs.reward_accounts.insert(key, Lovelace(0));
        certs.stake_key_deposits.insert(key, 2_000_000);
        certs.total_stake_key_deposits = 2_000_000;
        certs.script_stake_credentials.insert(key);
        certs
            .stake_distribution
            .stake_map
            .insert(key, Lovelace(1_731_936_015));
        // Register the pool so it has a valid PoolRegistration entry -- the
        // delegation map is otherwise ignored by pool_stake aggregation.
        Arc::make_mut(&mut certs.pool_params).insert(
            pool_id,
            PoolRegistration {
                pool_id,
                vrf_keyhash: Hash32::ZERO,
                pledge: Lovelace(0),
                cost: Lovelace(0),
                margin_numerator: 0,
                margin_denominator: 1,
                reward_account: vec![0u8; 29],
                owners: vec![],
                relays: vec![],
                metadata_url: None,
                metadata_hash: None,
            },
        );

        let mut utxo = make_utxo_sub(vec![]);
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let mut tx = make_tx(0x94, vec![], vec![], 0);
        tx.body.certificates = vec![
            Certificate::ConwayStakeDeregistration {
                credential: cred.clone(),
                refund: Lovelace(2_000_000),
            },
            Certificate::ConwayStakeRegistration {
                credential: cred.clone(),
                deposit: Lovelace(2_000_000),
            },
            Certificate::StakeDelegation {
                credential: cred.clone(),
                pool_hash: pool_id,
            },
        ];

        rules
            .apply_valid_tx(
                &tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("valid cert sequence should apply");

        assert_eq!(
            certs.delegations.get(&key).copied(),
            Some(pool_id),
            "credential must remain delegated after [DEREG, REG, DELEG] sequence"
        );
        assert!(
            certs.stake_distribution.stake_map.contains_key(&key),
            "credential must retain its stake_map entry"
        );
        assert_eq!(
            certs.stake_distribution.stake_map.get(&key).copied(),
            Some(Lovelace(1_731_936_015)),
            "stake_map value must be preserved through DEREG/REG cycle"
        );
        assert!(
            certs.script_stake_credentials.contains(&key),
            "script credential flag must be restored by the REG"
        );
        assert_eq!(
            certs.stake_key_deposits.get(&key).copied(),
            Some(2_000_000),
            "re-registered deposit must be tracked"
        );
    }

    // -----------------------------------------------------------------------
    // on_era_transition — ConwayGenesis seeding tests
    // -----------------------------------------------------------------------

    /// Verify DReps are populated from ConwayGenesis during era transition.
    #[test]
    fn test_on_era_transition_seeds_initial_dreps() {
        use crate::eras::ConwayGenesisInit;

        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.drep_activity = 100;
        let genesis = ConwayGenesisInit {
            initial_dreps: vec![
                (Hash28::from_bytes([0x01; 28]), 500_000_000),
                (Hash28::from_bytes([0x02; 28]), 1_000_000_000),
            ],
            ..Default::default()
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 100,
            current_epoch: EpochNo(10),
            era: Era::Conway,
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
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = false;

        rules
            .on_era_transition(
                Era::Babbage,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut consensus,
                &mut epochs,
            )
            .expect("era transition should succeed");

        // Verify two DReps were seeded.
        assert_eq!(gov.governance.dreps.len(), 2);

        // Check first DRep.
        let key1 = Hash32::from_bytes({
            let mut buf = [0u8; 32];
            buf[..28].copy_from_slice(&[0x01; 28]);
            buf
        });
        let drep1 = gov.governance.dreps.get(&key1).expect("drep1 must exist");
        assert_eq!(drep1.deposit.0, 500_000_000);
        assert_eq!(drep1.drep_expiry, EpochNo(110)); // 10 + 100
        assert!(drep1.active);
        assert_eq!(drep1.registered_epoch, EpochNo(10));

        // Check second DRep.
        let key2 = Hash32::from_bytes({
            let mut buf = [0u8; 32];
            buf[..28].copy_from_slice(&[0x02; 28]);
            buf
        });
        let drep2 = gov.governance.dreps.get(&key2).expect("drep2 must exist");
        assert_eq!(drep2.deposit.0, 1_000_000_000);
    }

    /// Babbage→Conway must seed the PlutusV3 cost model from ConwayGenesis as a
    /// per-language INSERT: V3 is set, V1/V2 carried from Babbage are untouched,
    /// and an already-present V3 (e.g. a governance update) is NOT overwritten.
    ///
    /// Regression for the Conway ScriptDataHashMismatch + budget-exhausted
    /// divergence cluster (mainnet epoch 507+, e.g. tx 31b6732d…): when V3 is
    /// `None`, `encode_language_views` emits an empty map and every PlutusV3 tx
    /// gets the wrong script_data_hash and default-cost-model budgets.
    #[test]
    fn test_on_era_transition_seeds_plutus_v3_cost_model() {
        use crate::eras::ConwayGenesisInit;

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();

        // The first four mainnet V3 entries (conway-genesis.json plutusV3CostModel).
        let v3_model = vec![100788i64, 420, 1, 1000];
        let genesis = ConwayGenesisInit {
            plutus_v3_cost_model: Some(v3_model.clone()),
            ..Default::default()
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 100,
            current_epoch: EpochNo(507),
            era: Era::Conway,
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
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let run = |v3_pre: Option<Vec<i64>>| {
            let mut utxo = make_utxo_sub(vec![]);
            let mut certs = make_cert_sub();
            let mut gov = make_gov_sub();
            let mut consensus = make_consensus_sub();
            let mut epochs = make_epoch_sub();
            epochs.ptr_stake_excluded = false;
            // Babbage carried V1/V2; V3 state varies per case.
            epochs.protocol_params.cost_models.plutus_v1 = Some(vec![1, 2, 3]);
            epochs.protocol_params.cost_models.plutus_v2 = Some(vec![4, 5, 6]);
            epochs.protocol_params.cost_models.plutus_v3 = v3_pre;
            rules
                .on_era_transition(
                    Era::Babbage,
                    &ctx,
                    &mut utxo,
                    &mut certs,
                    &mut gov,
                    &mut consensus,
                    &mut epochs,
                )
                .expect("era transition should succeed");
            epochs.protocol_params.cost_models
        };

        // Case 1 — V3 absent: inserted from genesis, V1/V2 preserved.
        let cm = run(None);
        assert_eq!(
            cm.plutus_v3,
            Some(v3_model.clone()),
            "V3 must be seeded from Conway genesis"
        );
        assert_eq!(cm.plutus_v1, Some(vec![1, 2, 3]), "V1 must be preserved");
        assert_eq!(cm.plutus_v2, Some(vec![4, 5, 6]), "V2 must be preserved");

        // Case 2 — V3 already present (e.g. governance-updated): NOT overwritten.
        let existing = vec![9, 9, 9, 9, 9];
        let cm = run(Some(existing.clone()));
        assert_eq!(
            cm.plutus_v3,
            Some(existing),
            "an already-present V3 cost model must not be clobbered by genesis"
        );
    }

    /// #764 Part A regression: a pending PRE-CONWAY (Babbage) PPUP carrying
    /// `cost_models = {V1, V2}` (no V3) applied at the Babbage→Conway boundary
    /// must NOT leave `plutus_v3 = None`. `on_era_transition` seeds V3 (Step 2);
    /// `process_epoch_transition` then applies the PPUP (Step 3) which wholesale-
    /// replaces `cost_models` and would wipe V3. The fix re-seeds V3 from genesis,
    /// matching Haskell's PPUP-then-translateEra order (mainnet ep506 has two
    /// quorum-meeting PPUPs carrying `{PlutusV1, PlutusV2}` + protocol_major=9).
    #[test]
    fn test_process_epoch_transition_ppup_does_not_wipe_v3() {
        use crate::eras::ConwayGenesisInit;
        use dugite_primitives::transaction::{CostModels, ProtocolParamUpdate};

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();

        let genesis_v3 = vec![100788i64, 420, 1, 1000];
        let genesis = ConwayGenesisInit {
            plutus_v3_cost_model: Some(genesis_v3.clone()),
            ..Default::default()
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 133_660_800,
            current_epoch: EpochNo(506),
            era: Era::Conway,
            slot_config: None,
            node_network: None,
            genesis_delegates: delegates,
            update_quorum: 5,
            epoch_length: 432000,
            shelley_transition_epoch: 208,
            byron_epoch_length: 21600,
            stability_window: 129600,
            stability_window_3kf: 129600,
            randomness_stabilisation_window: 129600,
            tx_index: 0,
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        // Simulate the state AFTER on_era_transition seeded V3 (Step 2):
        epochs.protocol_params.cost_models.plutus_v1 = Some(vec![1, 2, 3]);
        epochs.protocol_params.cost_models.plutus_v2 = Some(vec![4, 5, 6]);
        epochs.protocol_params.cost_models.plutus_v3 = Some(genesis_v3.clone());

        // Pending Babbage PPUP for epoch 506 (lookup = new_epoch-1 = 506) with
        // cost_models = {V1_new, V2_new} (no V3) + PV9 bump; 5 distinct proposers.
        let ppup = ProtocolParamUpdate {
            cost_models: Some(CostModels {
                plutus_v1: Some(vec![10, 20, 30]),
                plutus_v2: Some(vec![40, 50, 60]),
                plutus_v3: None,
                plutus_v4: None,
                ..Default::default()
            }),
            protocol_version_major: Some(9),
            ..Default::default()
        };
        let mut proposals = Vec::new();
        for i in 0u8..5 {
            let mut key = [0u8; 32];
            key[0] = i;
            proposals.push((Hash32::from_bytes(key), ppup.clone()));
        }
        epochs.pending_pp_updates.insert(EpochNo(506), proposals);

        rules
            .process_epoch_transition(
                EpochNo(507),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition should succeed");

        // V3 must survive — re-seeded from genesis after the PPUP wipe.
        assert_eq!(
            epochs.protocol_params.cost_models.plutus_v3,
            Some(genesis_v3),
            "PlutusV3 cost model must survive a Babbage PPUP that carries only V1/V2 (#764)"
        );
        // V1/V2 updated by the PPUP.
        assert_eq!(
            epochs.protocol_params.cost_models.plutus_v1,
            Some(vec![10, 20, 30])
        );
        assert_eq!(
            epochs.protocol_params.cost_models.plutus_v2,
            Some(vec![40, 50, 60])
        );
        // PV bumped to 9 by the PPUP.
        assert_eq!(epochs.protocol_params.protocol_version_major, 9);
    }

    /// Issue #784: same fix at the Conway era-crossing PPUP enactment site.
    /// 3 genesis delegates vote for a PV9 bump, 2 vote for a distinct PV9
    /// bump-plus-cost-model update. The union of proposers (5) meets the
    /// old buggy distinct-proposer quorum, but neither value alone reaches
    /// quorum under `votedFuturePParams` — nothing should be enacted, and
    /// in particular the protocol version must NOT bump to 9.
    #[test]
    fn test_process_epoch_transition_ppup_split_vote_enacts_nothing_era_crossing() {
        use crate::eras::ConwayGenesisInit;

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let genesis = ConwayGenesisInit::default();
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 133_660_800,
            current_epoch: EpochNo(506),
            era: Era::Conway,
            slot_config: None,
            node_network: None,
            genesis_delegates: delegates,
            update_quorum: 5,
            epoch_length: 432000,
            shelley_transition_epoch: 208,
            byron_epoch_length: 21600,
            stability_window: 129600,
            stability_window_3kf: 129600,
            randomness_stabilisation_window: 129600,
            tx_index: 0,
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        let original_pv_major = epochs.protocol_params.protocol_version_major;

        let ppu_a = dugite_primitives::transaction::ProtocolParamUpdate {
            protocol_version_major: Some(9),
            protocol_version_minor: Some(0),
            ..Default::default()
        };
        let ppu_b = dugite_primitives::transaction::ProtocolParamUpdate {
            protocol_version_major: Some(9),
            protocol_version_minor: Some(0),
            min_fee_a: Some(55),
            ..Default::default()
        };
        let mut proposals = Vec::new();
        for i in 0u8..3 {
            proposals.push((Hash32::from_bytes([i; 32]), ppu_a.clone()));
        }
        for i in 3u8..5 {
            proposals.push((Hash32::from_bytes([i; 32]), ppu_b.clone()));
        }
        epochs.pending_pp_updates.insert(EpochNo(506), proposals);

        rules
            .process_epoch_transition(
                EpochNo(507),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition should succeed");

        assert_eq!(
            epochs.protocol_params.protocol_version_major, original_pv_major,
            "neither ppu_a nor ppu_b reached quorum alone; nothing should enact (#784)"
        );
    }

    /// Verify committee members and threshold are set from ConwayGenesis.
    #[test]
    fn test_on_era_transition_seeds_committee() {
        use crate::eras::ConwayGenesisInit;

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let cred1_bytes = [0xCC; 32];
        let cred2_bytes = [0xDD; 32];
        let genesis = ConwayGenesisInit {
            committee_members: vec![(cred1_bytes, 200), (cred2_bytes, 300)],
            committee_threshold: Some((2, 3)),
            ..Default::default()
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: Era::Conway,
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
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = false;

        rules
            .on_era_transition(
                Era::Babbage,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut consensus,
                &mut epochs,
            )
            .expect("era transition should succeed");

        // Verify committee members.
        assert_eq!(gov.governance.committee_expiration.len(), 2);
        let c1 = Hash32::from_bytes(cred1_bytes);
        let c2 = Hash32::from_bytes(cred2_bytes);
        assert_eq!(
            gov.governance.committee_expiration.get(&c1),
            Some(&EpochNo(200))
        );
        assert_eq!(
            gov.governance.committee_expiration.get(&c2),
            Some(&EpochNo(300))
        );

        // Verify threshold.
        let threshold = gov
            .governance
            .committee_threshold
            .as_ref()
            .expect("threshold must be set");
        assert_eq!(threshold.numerator, 2);
        assert_eq!(threshold.denominator, 3);
    }

    /// Verify constitution is set from ConwayGenesis.
    #[test]
    fn test_on_era_transition_seeds_constitution() {
        use crate::eras::ConwayGenesisInit;
        use dugite_primitives::transaction::Constitution;

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let constitution = Constitution {
            anchor: Anchor {
                url: "https://constitution.example.com".to_string(),
                data_hash: Hash32::from_bytes([0xAB; 32]),
            },
            script_hash: None,
        };
        let genesis = ConwayGenesisInit {
            constitution: Some(constitution.clone()),
            ..Default::default()
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 100,
            current_epoch: EpochNo(5),
            era: Era::Conway,
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
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = false;

        rules
            .on_era_transition(
                Era::Babbage,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut consensus,
                &mut epochs,
            )
            .expect("era transition should succeed");

        let stored = gov
            .governance
            .constitution
            .as_ref()
            .expect("constitution must be set");
        assert_eq!(stored.anchor.url, "https://constitution.example.com");
        assert_eq!(stored.anchor.data_hash, Hash32::from_bytes([0xAB; 32]));
    }

    /// Verify that `None` conway_genesis doesn't panic and is a no-op for governance.
    #[test]
    fn test_on_era_transition_no_genesis_is_noop() {
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params); // conway_genesis = None

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = false;

        rules
            .on_era_transition(
                Era::Babbage,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut consensus,
                &mut epochs,
            )
            .expect("era transition with no genesis should succeed");

        // Governance should remain empty.
        assert!(gov.governance.dreps.is_empty());
        assert!(gov.governance.committee_expiration.is_empty());
        assert!(gov.governance.committee_threshold.is_none());
        assert!(gov.governance.constitution.is_none());

        // But steps 1 and 5 should still apply.
        assert!(epochs.ptr_stake_excluded);
        assert_eq!(utxo.pending_donations.0, 0);
    }

    /// Issue #467 regression: when `ConwayRules` is dispatched for the
    /// Conway -> Dijkstra boundary (because Dijkstra is aliased to ConwayRules
    /// in `eras/mod.rs`), the Babbage->Conway init steps must NOT run.
    /// Governance state (DReps, committee, constitution, threshold) and
    /// in-flight donations must be preserved verbatim.
    #[test]
    fn test_on_era_transition_conway_to_dijkstra_preserves_state() {
        use crate::eras::ConwayGenesisInit;
        use dugite_primitives::transaction::Constitution;

        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults();

        // Provide a non-empty ConwayGenesis so that, if the guard were missing,
        // the Babbage->Conway re-seeding would overwrite our mutated state and
        // the assertions below would fail loudly.
        let genesis = ConwayGenesisInit {
            initial_dreps: vec![(Hash28::from_bytes([0xAA; 28]), 12_345_000)],
            committee_members: vec![([0xBB; 32], 99)],
            committee_threshold: Some((1, 7)),
            constitution: Some(Constitution {
                anchor: Anchor {
                    url: "https://genesis-constitution.example".to_string(),
                    data_hash: Hash32::from_bytes([0xEE; 32]),
                },
                script_hash: None,
            }),
            plutus_v3_cost_model: None,
        };
        let delegates = Box::leak(Box::new(HashMap::new()));
        let ctx = RuleContext {
            params: &params,
            current_slot: 1_000_000,
            current_epoch: EpochNo(500),
            era: Era::Dijkstra,
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
            conway_genesis: Some(&genesis),
            max_lovelace_supply: crate::state::MAX_LOVELACE_SUPPLY,
        };

        // Build a Conway-era ledger sub-state with governance that diverges
        // from the ConwayGenesis values above.
        let mut utxo = make_utxo_sub(vec![]);
        utxo.pending_donations = Lovelace(7_777_777);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut consensus = make_consensus_sub();
        let mut epochs = make_epoch_sub();
        epochs.ptr_stake_excluded = true;

        // Mutated DRep (different hash + deposit + expiry than genesis).
        let live_drep_hash = Hash28::from_bytes([0x11; 28]);
        let live_drep_key = Hash32::from_bytes({
            let mut buf = [0u8; 32];
            buf[..28].copy_from_slice(live_drep_hash.as_bytes());
            buf
        });
        {
            let governance = Arc::make_mut(&mut gov.governance);
            governance.dreps.insert(
                live_drep_key,
                DRepRegistration {
                    credential: Credential::VerificationKey(live_drep_hash),
                    deposit: Lovelace(999_000_000),
                    drep_expiry: EpochNo(550),
                    anchor: None,
                    registered_epoch: EpochNo(450),
                    active: true,
                },
            );
            // Mutated committee (different cred + expiry than genesis).
            governance
                .committee_expiration
                .insert(Hash32::from_bytes([0x22; 32]), EpochNo(600));
            // Mutated threshold (1/2 != genesis 1/7).
            governance.committee_threshold = Some(dugite_primitives::transaction::Rational {
                numerator: 1,
                denominator: 2,
            });
            // Mutated constitution (different URL than genesis).
            governance.constitution = Some(Constitution {
                anchor: Anchor {
                    url: "https://post-conway-ratified.example".to_string(),
                    data_hash: Hash32::from_bytes([0x33; 32]),
                },
                script_hash: None,
            });
        }

        // Fire transition with from_era = Conway (target dispatched as ConwayRules
        // because Dijkstra is currently aliased).
        rules
            .on_era_transition(
                Era::Conway,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut consensus,
                &mut epochs,
            )
            .expect("Conway->Dijkstra transition should be a no-op and succeed");

        // Assertions: every piece of mutated state survives intact.
        assert_eq!(gov.governance.dreps.len(), 1, "DReps must not be re-seeded");
        let drep = gov
            .governance
            .dreps
            .get(&live_drep_key)
            .expect("live DRep must still be present");
        assert_eq!(drep.deposit.0, 999_000_000);
        assert_eq!(drep.drep_expiry, EpochNo(550));

        assert_eq!(
            gov.governance.committee_expiration.len(),
            1,
            "committee must not be re-seeded from genesis"
        );
        assert_eq!(
            gov.governance
                .committee_expiration
                .get(&Hash32::from_bytes([0x22; 32])),
            Some(&EpochNo(600))
        );

        let threshold = gov
            .governance
            .committee_threshold
            .as_ref()
            .expect("threshold must still be set");
        assert_eq!(threshold.numerator, 1);
        assert_eq!(threshold.denominator, 2);

        let constitution = gov
            .governance
            .constitution
            .as_ref()
            .expect("constitution must still be set");
        assert_eq!(
            constitution.anchor.url,
            "https://post-conway-ratified.example"
        );

        // In-flight donations must NOT be zeroed.
        assert_eq!(utxo.pending_donations.0, 7_777_777);
    }

    /// Verify that `process_epoch_transition` calls `ratify_proposals_impl`
    /// without panicking when there are no proposals.
    #[test]
    fn test_conway_epoch_transition_runs_ratification() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        // Empty proposals — ratification should be a no-op and not panic.
        assert!(gov.governance.proposals.is_empty());

        rules
            .process_epoch_transition(
                EpochNo(10),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition with empty proposals should not panic");

        // Proposals should still be empty after ratification.
        assert!(gov.governance.proposals.is_empty());
    }

    /// Verify that `update_dormant_epochs` is called during epoch transition:
    /// when there are no proposals, the dormant epoch counter increments.
    #[test]
    fn test_conway_epoch_transition_dormant_epoch_tracking() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        let mut consensus = make_consensus_sub();

        // Initial dormant epoch count is 0.
        assert_eq!(gov.governance.num_dormant_epochs, 0);

        // First epoch transition with empty proposals -> dormant incremented.
        rules
            .process_epoch_transition(
                EpochNo(10),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("first epoch transition should succeed");
        assert_eq!(gov.governance.num_dormant_epochs, 1);

        // Second epoch transition still empty -> dormant incremented again.
        rules
            .process_epoch_transition(
                EpochNo(11),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("second epoch transition should succeed");
        assert_eq!(gov.governance.num_dormant_epochs, 2);
    }

    // -----------------------------------------------------------------------
    // PV10 withdrawal delegation and amount validation tests
    // -----------------------------------------------------------------------

    /// Build a key-hash reward address (header 0xe1 = key credential, network 1).
    fn make_key_reward_address(cred: [u8; 28]) -> Vec<u8> {
        let mut addr = vec![0xe1]; // key credential, network 1
        addr.extend_from_slice(&cred);
        addr
    }

    /// Build a script-hash reward address (header 0xf1 = script credential, network 1).
    fn make_script_reward_address(cred: [u8; 28]) -> Vec<u8> {
        let mut addr = vec![0xf1]; // script credential, network 1
        addr.extend_from_slice(&cred);
        addr
    }

    /// Build a Hash32 key for a key-hash credential (byte 28 = 0x00).
    fn key_cred_hash(cred: [u8; 28]) -> Hash32 {
        let mut bytes = [0u8; 32];
        bytes[..28].copy_from_slice(&cred);
        // bytes[28] = 0x00 for key credential (default)
        Hash32::from_bytes(bytes)
    }

    /// Helper to build a transaction with withdrawals.
    fn make_tx_with_withdrawals(
        tx_id_byte: u8,
        withdrawals: BTreeMap<Vec<u8>, Lovelace>,
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        fee: u64,
    ) -> Transaction {
        let mut tx = make_tx(tx_id_byte, inputs, outputs, fee);
        tx.body.withdrawals = withdrawals;
        tx
    }

    // NOTE: PV10 `validateWithdrawalsDelegated` (step 3) and
    // `testIncompleteAndMissingWithdrawals` (step 4) are enforced in
    // `ConwayRules::apply_valid_tx` only when `mode == ValidateAll`.
    //
    // History:
    //   9a631979e — removed both checks from block-apply entirely to fix false
    //               rejections on preview testnet (reward-balance divergence).
    //   a7591523b — re-enabled both checks unconditionally after the #438
    //               reserve-inflation root cause was fixed.
    //   current   — gated on ValidateAll (issue #503): on preprod the same
    //               3,727-lovelace divergence resurfaced because the #438 fix
    //               corrected preview but not preprod's protocol-param history.
    //               Historical (ApplyOnly) blocks were accepted on-chain and
    //               must not be re-validated; live and forged blocks still get
    //               the full check.
    //
    // Tests below verify:
    //   - ValidateAll + PV10 + mismatch  → Err
    //   - ApplyOnly  + PV10 + mismatch   → Ok  (issue #503 regression guard)
    //   - ValidateAll + PV10 + match     → Ok
    //   - ValidateAll + PV10 + undelegated keyHash → Err
    //   - ApplyOnly  + PV10 + undelegated keyHash  → Ok
    //   - ValidateAll + PV9              → Ok (check not active)
    //   - ApplyOnly  + PV10 + script cred → Ok (script skips delegation check)

    #[test]
    fn test_pv10_withdrawal_delegated_succeeds() {
        // PV=10, delegated key-hash withdrawal -> OK
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        let cred = [0xBB; 28];
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(500_000));

        // Add vote delegation for this credential.
        let mut gov = make_gov_sub();
        Arc::make_mut(&mut gov.governance)
            .vote_delegations
            .insert(cred_hash, DRep::Abstain);

        let mut epochs = make_epoch_sub();

        let input = make_input(0x02, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0x02; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(500_000));

        let tx = make_tx_with_withdrawals(
            0x11,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_500_000)],
            1_000_000,
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
            "Delegated withdrawal should succeed: {result:?}"
        );
    }

    #[test]
    fn test_pv9_withdrawal_not_checked() {
        // PV=9, undelegated key-hash withdrawal -> still succeeds (check not active)
        let rules = ConwayRules::new();
        let params = ProtocolParameters::mainnet_defaults(); // PV=9
        assert!(params.protocol_version_major < 10);
        let ctx = make_conway_ctx(&params);

        let cred = [0xCC; 28];
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(200_000));

        // No vote delegation — but PV < 10 so check should be skipped.
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let input = make_input(0x03, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0x03; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(200_000));

        let tx = make_tx_with_withdrawals(
            0x12,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_800_000)],
            1_000_000,
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
            "PV9 should not enforce delegation check: {result:?}"
        );
    }

    /// Issue #503 regression: ApplyOnly (chunk-replay) must skip the PV10
    /// PV10 amount-mismatch check fires in ApplyOnly mode too (#634).
    ///
    /// Before #629 fixed the f64→Rational reward calc gap, preprod
    /// chunk-replay failed at slot 76554214 with WithdrawalAmountMismatch
    /// (balance 4,127,037 vs on-chain 4,130,764) — a 3,727-lovelace
    /// cumulative reward divergence that the `ApplyOnly` gate masked.
    /// With #629 reward accumulation is byte-exact, so the check is now
    /// unconditional per Haskell `conwayWithdrawals`; a genuine mismatch
    /// is a real error, not a workaround target.
    #[test]
    fn test_pv10_withdrawal_amount_mismatch_apply_only_succeeds() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        // Simulate the issue #503 scenario: balance is 3,727 less than on-chain.
        let cred = [0xBF; 28]; // arbitrary cred representing bfaa385c...
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(4_127_037));

        // The credential IS delegated (satisfying step 3) but the amount differs.
        let mut gov = make_gov_sub();
        Arc::make_mut(&mut gov.governance)
            .vote_delegations
            .insert(cred_hash, DRep::Abstain);

        let mut epochs = make_epoch_sub();

        let input = make_input(0xBF, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0xBF; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        // On-chain the tx withdraws 4,130,764 (3,727 more than dugite's balance).
        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(4_130_764));

        let tx = make_tx_with_withdrawals(
            0xBF,
            withdrawals,
            vec![input],
            vec![make_output(addr, 5_869_236)],
            1_000_000,
        );

        // ApplyOnly mode no longer masks the mismatch (#634).
        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ApplyOnly,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );
        let err = result
            .expect_err("ApplyOnly must fail with WithdrawalAmountMismatch per Haskell (#634)");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("WithdrawalAmountMismatch"),
            "expected WithdrawalAmountMismatch, got: {msg}",
        );
    }

    /// Verify that ValidateAll mode DOES enforce the PV10 amount-mismatch check.
    ///
    /// This ensures the guard added for #503 doesn't accidentally drop the check
    /// for live blocks (mempool admission and forged-block validation).
    #[test]
    fn test_pv10_withdrawal_amount_mismatch_validate_all_fails() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        let cred = [0xAA; 28];
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(100_000));

        let mut gov = make_gov_sub();
        Arc::make_mut(&mut gov.governance)
            .vote_delegations
            .insert(cred_hash, DRep::Abstain);

        let mut epochs = make_epoch_sub();

        let input = make_input(0xAA, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0xAA; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        // Withdraw 200_000 but balance is only 100_000.
        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(200_000));

        let tx = make_tx_with_withdrawals(
            0xAA,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_800_000)],
            1_000_000,
        );

        // ValidateAll mode: must reject the amount mismatch.
        let result = rules.apply_valid_tx(
            &tx,
            BlockValidationMode::ValidateAll,
            &ctx,
            &mut utxo,
            &mut certs,
            &mut gov,
            &mut epochs,
        );
        assert!(
            result.is_err(),
            "ValidateAll must reject PV10 amount mismatch for live blocks"
        );
        let err_str = format!("{result:?}");
        assert!(
            err_str.contains("WithdrawalAmountMismatch"),
            "Error must mention WithdrawalAmountMismatch, got: {err_str}"
        );
    }

    /// Verify that ValidateAll mode enforces the PV10 delegation check.
    #[test]
    fn test_pv10_withdrawal_undelegated_validate_all_fails() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        let cred = [0xCC; 28];
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(500_000));

        // No DRep delegation.
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let input = make_input(0xCC, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0xCC; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(500_000));

        let tx = make_tx_with_withdrawals(
            0xCC,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_500_000)],
            1_000_000,
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
        assert!(
            result.is_err(),
            "ValidateAll must reject undelegated PV10 withdrawal"
        );
        let err_str = format!("{result:?}");
        assert!(
            err_str.contains("WithdrawalNotDelegated"),
            "Error must mention WithdrawalNotDelegated, got: {err_str}"
        );
    }

    /// Verify the PV10 delegation check fires in ApplyOnly mode too (#634).
    ///
    /// Per Haskell `conwayWithdrawals` this predicate is unconditional —
    /// any historical block whose witness drained an undelegated key-hash
    /// account is invalid and must be rejected on replay.
    #[test]
    fn test_pv10_withdrawal_undelegated_apply_only_succeeds() {
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        let cred = [0xDD; 28];
        let reward_addr = make_key_reward_address(cred);
        let cred_hash = key_cred_hash(cred);

        let mut certs = make_cert_sub();
        certs.reward_accounts.insert(cred_hash, Lovelace(400_000));

        // No DRep delegation — must be rejected unconditionally per Haskell.
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let input = make_input(0xDD, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0xDD; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(400_000));

        let tx = make_tx_with_withdrawals(
            0xDD,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_600_000)],
            1_000_000,
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
        let err =
            result.expect_err("ApplyOnly must fail with WithdrawalNotDelegated per Haskell (#634)");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("WithdrawalNotDelegated"),
            "expected WithdrawalNotDelegated, got: {msg}",
        );
    }

    #[test]
    fn test_pv10_script_withdrawal_not_checked() {
        // PV=10, script credential -> delegation check skipped
        let rules = ConwayRules::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let ctx = make_conway_ctx(&params);

        let cred = [0xEE; 28];
        let reward_addr = make_script_reward_address(cred);

        // Build the script credential hash (byte 28 = 0x01 for script).
        let mut script_key_bytes = [0u8; 32];
        script_key_bytes[..28].copy_from_slice(&cred);
        script_key_bytes[28] = 0x01; // script credential marker
        let script_cred_hash = Hash32::from_bytes(script_key_bytes);

        let mut certs = make_cert_sub();
        certs
            .reward_accounts
            .insert(script_cred_hash, Lovelace(300_000));

        // No vote delegation for this script credential — should still succeed
        // because script credentials skip the delegation check.
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();

        let input = make_input(0x05, 0);
        let addr = make_enterprise_address(Hash28::from_bytes([0x05; 28]));
        let output = make_output(addr.clone(), 10_000_000);
        let mut utxo = make_utxo_sub(vec![(input.clone(), output)]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(reward_addr, Lovelace(300_000));

        let tx = make_tx_with_withdrawals(
            0x14,
            withdrawals,
            vec![input],
            vec![make_output(addr, 9_700_000)],
            1_000_000,
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
            "Script credential withdrawal should skip delegation check: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // #496 — UpdateCommittee enactment during chunk-file replay
    //
    // The reported bug: in a from-genesis chunk-file replay, on-chain
    // UpdateCommittee proposals never enact, even though they DO enact on
    // the real Cardano chain (e.g. preview tx f4188b…, ratified epoch 993,
    // enacted epoch 994).  The unit test
    // `test_update_committee_no_cc_required_when_confidence` in
    // state/governance.rs already proves the basic enactment loop works
    // when proposals + votes are added via `process_proposal` /
    // `process_vote` directly on `LedgerState`.  The chunk-replay path
    // dispatches through `ConwayRules::apply_valid_tx →
    // process_governance_votes_and_proposals`, which is a different code
    // path with no bootstrap guard, no `prev_action_id` chain validation,
    // and no constitution check.  This test exercises THAT path end-to-end
    // and asserts the same outcome: a freshly-submitted UpdateCommittee
    // with passing DRep+SPO votes enacts at the next eligible epoch
    // boundary.
    // -----------------------------------------------------------------------

    #[test]
    fn test_conway_apply_tx_updatecommittee_enacts_through_era_rules() {
        use crate::state::DRepRegistration;
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::transaction::{GovAction, GovActionId, Rational, Voter};

        // PV10 (post-bootstrap) on mainnet defaults — real DRep+SPO thresholds apply.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        params.committee_min_size = 0;
        params.gov_action_lifetime = 10;
        params.committee_max_term_length = 200;
        // Lower thresholds so a small synthetic electorate clears them.
        params.dvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };
        params.pvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };

        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();
        epochs.prev_protocol_params = params.clone();
        epochs.prev_protocol_version_major = 10;
        let mut consensus = make_consensus_sub();

        // ── Seed DRep electorate (10 DReps, 1B stake each delegated to them).
        for i in 0..10u8 {
            let drep_cred = Credential::VerificationKey(Hash28::from_bytes([i; 28]));
            let drep_key = dugite_primitives::credentials::Credential::to_typed_hash32(&drep_cred);
            Arc::make_mut(&mut gov.governance).dreps.insert(
                drep_key,
                DRepRegistration {
                    credential: drep_cred.clone(),
                    deposit: Lovelace(500_000_000),
                    anchor: None,
                    registered_epoch: EpochNo(0),
                    drep_expiry: EpochNo(100),
                    active: true,
                },
            );
            let stake_key = Hash32::from_bytes([200u8 + i; 32]);
            Arc::make_mut(&mut gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::KeyHash(drep_key));
            certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }

        // ── Seed SPO electorate (5 pools, 1B stake each).
        for i in 0..5u8 {
            let pool_id = Hash28::from_bytes([100u8 + i; 28]);
            Arc::make_mut(&mut certs.pool_params).insert(
                pool_id,
                PoolRegistration {
                    pool_id,
                    vrf_keyhash: Hash32::ZERO,
                    pledge: Lovelace(1_000_000),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 100,
                    reward_account: vec![],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
            let stake_key = Hash32::from_bytes([150u8 + i; 32]);
            certs.delegations.insert(stake_key, pool_id);
            certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }
        // Prime the mark snapshot with this SPO stake so ratification's SPO
        // denominator matches (ratify_proposals_impl reads `snapshots.set`
        // for total_spo_stake; absent that, falls back to live).
        let mark_pool_stake: HashMap<Hash28, Lovelace> = (0..5u8)
            .map(|i| (Hash28::from_bytes([100u8 + i; 28]), Lovelace(1_000_000_000)))
            .collect();
        let pool_params_snap = certs.pool_params.clone();
        let make_snap = || crate::state::StakeSnapshot {
            epoch: ctx.current_epoch,
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_pool_stake.clone(),
            pool_params: pool_params_snap.clone(),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        epochs.snapshots.mark = Some(make_snap());
        epochs.snapshots.set = Some(make_snap());
        epochs.snapshots.go = Some(make_snap());

        // ── Build a tx with a single UpdateCommittee proposal (chain-link 1).
        let new_member_cred = Credential::VerificationKey(Hash28::from_bytes([0x99; 28]));
        let new_member_key =
            dugite_primitives::credentials::Credential::to_typed_hash32(&new_member_cred);
        let mut members_to_add = BTreeMap::new();
        members_to_add.insert(new_member_cred.clone(), 100u64);

        let proposal = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove: vec![],
                members_to_add,
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            anchor: Anchor {
                url: "https://test".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let mut prop_tx = make_tx(0x40, vec![], vec![], 0);
        prop_tx.body.proposal_procedures = vec![proposal];
        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply UpdateCommittee proposal tx");

        let action_id = GovActionId {
            transaction_id: prop_tx.hash,
            action_index: 0,
        };
        assert!(
            gov.governance.proposals.contains_key(&action_id),
            "proposal must be ingested by process_governance_votes_and_proposals"
        );

        // ── Build a tx with the DRep + SPO votes.
        let mut vote_tx = make_tx(0x41, vec![], vec![], 0);
        for i in 0..10u8 {
            let voter = Voter::DRep(Credential::VerificationKey(Hash28::from_bytes([i; 28])));
            vote_tx
                .body
                .voting_procedures
                .entry(voter)
                .or_default()
                .insert(
                    action_id.clone(),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                );
        }
        for i in 0..5u8 {
            let pool_hash32 = Hash28::from_bytes([100u8 + i; 28]).to_hash32_padded();
            let voter = Voter::StakePool(pool_hash32);
            vote_tx
                .body
                .voting_procedures
                .entry(voter)
                .or_default()
                .insert(
                    action_id.clone(),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                );
        }
        rules
            .apply_valid_tx(
                &vote_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply votes tx");

        // Sanity: votes recorded for our proposal.
        let recorded = gov
            .governance
            .votes_by_action
            .get(&action_id)
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(
            recorded, 15,
            "all 10 DRep + 5 SPO votes must be in votes_by_action"
        );

        // ── Cross epoch boundaries until either ratification fires or the
        // proposal expires.  In Conway timing, ratification reads from the
        // snapshot captured at the *previous* boundary, so the proposal
        // becomes eligible at boundary E+1→E+2 (where E is the proposal's
        // proposed_epoch).  Loop up to 8 boundaries to be defensive.
        let starting_epoch = ctx.current_epoch.0;
        for next_epoch in (starting_epoch + 1)..=(starting_epoch + 8) {
            rules
                .process_epoch_transition(
                    EpochNo(next_epoch),
                    &ctx,
                    &mut utxo,
                    &mut certs,
                    &mut gov,
                    &mut epochs,
                    &mut consensus,
                )
                .expect("epoch transition must not fail");
            if gov
                .governance
                .committee_expiration
                .contains_key(&new_member_key)
            {
                return; // success
            }
        }

        panic!(
            "UpdateCommittee proposal applied through Conway::apply_valid_tx + \
             process_epoch_transition never enacted after 8 epoch boundaries; \
             committee_expiration={:?}, proposals_remaining={:?}, \
             enacted_committee={:?}",
            gov.governance.committee_expiration,
            gov.governance.proposals.keys().collect::<Vec<_>>(),
            gov.governance.enacted_committee,
        );
    }

    /// Companion to `test_conway_apply_tx_updatecommittee_enacts_through_era_rules`:
    /// a Plomin-style HardForkInitiation (PV9 → PV10) must enact through the
    /// chunk-replay code path.  If THIS fails, dugite stays at PV9 (bootstrap)
    /// after replaying past preview epoch 743, which in turn blocks subsequent
    /// UpdateCommittee proposals from enacting because the bootstrap phase
    /// disallows the disposition of any DRep votes for non-PParam, non-HF,
    /// non-Info actions (CC also has no voters at PV9 boot).
    #[test]
    fn test_conway_hardfork_pv9_to_pv10_enacts_through_era_rules() {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::transaction::{GovAction, GovActionId, Rational, Voter};

        // PV9 bootstrap.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        params.committee_min_size = 0;
        params.gov_action_lifetime = 10;
        // Realistic Plomin SPO threshold is around 0.6; lower it so a small
        // synthetic SPO electorate clears.  DRep threshold is 0 at bootstrap.
        params.pvt_hard_fork = Rational {
            numerator: 1,
            denominator: 2,
        };

        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();
        epochs.prev_protocol_params = params.clone();
        epochs.prev_protocol_version_major = 9;
        let mut consensus = make_consensus_sub();

        // Seed 1 committee member (genesis member) + threshold so CC approval
        // path has a quorum (committee_min_size=0 already lets it pass; the
        // member is here to mirror the Conway-genesis seed at era transition).
        let cc_cold = Credential::VerificationKey(Hash28::from_bytes([0x10; 28]));
        let cc_hot = Credential::VerificationKey(Hash28::from_bytes([0x20; 28]));
        let cc_cold_key = cc_cold.to_typed_hash32();
        let cc_hot_key = cc_hot.to_typed_hash32();
        {
            let g = Arc::make_mut(&mut gov.governance);
            g.committee_expiration.insert(cc_cold_key, EpochNo(10_000));
            g.committee_hot_keys.insert(cc_cold_key, cc_hot_key);
            g.committee_threshold = Some(Rational {
                numerator: 1,
                denominator: 2,
            });
        }

        // Seed 5 SPOs with stake.
        for i in 0..5u8 {
            let pool_id = Hash28::from_bytes([100u8 + i; 28]);
            Arc::make_mut(&mut certs.pool_params).insert(
                pool_id,
                PoolRegistration {
                    pool_id,
                    vrf_keyhash: Hash32::ZERO,
                    pledge: Lovelace(1_000_000),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 100,
                    reward_account: vec![],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
            let stake_key = Hash32::from_bytes([150u8 + i; 32]);
            certs.delegations.insert(stake_key, pool_id);
            certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }
        let pool_stake_snap: HashMap<Hash28, Lovelace> = (0..5u8)
            .map(|i| (Hash28::from_bytes([100u8 + i; 28]), Lovelace(1_000_000_000)))
            .collect();
        let pool_params_snap = certs.pool_params.clone();
        let make_snap = || crate::state::StakeSnapshot {
            epoch: ctx.current_epoch,
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: pool_stake_snap.clone(),
            pool_params: pool_params_snap.clone(),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        epochs.snapshots.mark = Some(make_snap());
        epochs.snapshots.set = Some(make_snap());
        epochs.snapshots.go = Some(make_snap());

        // Build a HardForkInitiation proposal: PV9 → PV10 (Plomin shape).
        let proposal = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            },
            anchor: Anchor {
                url: "https://test".to_string(),
                data_hash: Hash32::ZERO,
            },
        };
        let mut prop_tx = make_tx(0x50, vec![], vec![], 0);
        prop_tx.body.proposal_procedures = vec![proposal];
        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply HardForkInitiation proposal tx");

        let action_id = GovActionId {
            transaction_id: prop_tx.hash,
            action_index: 0,
        };
        assert!(
            gov.governance.proposals.contains_key(&action_id),
            "HF proposal must be ingested at PV9 bootstrap (HFs ARE allowed during boot)"
        );

        // SPO votes + CC vote.
        let mut vote_tx = make_tx(0x51, vec![], vec![], 0);
        for i in 0..5u8 {
            let pool_hash32 = Hash28::from_bytes([100u8 + i; 28]).to_hash32_padded();
            let voter = Voter::StakePool(pool_hash32);
            vote_tx
                .body
                .voting_procedures
                .entry(voter)
                .or_default()
                .insert(
                    action_id.clone(),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                );
        }
        // CC hot-key vote (HardFork needs CC approval too).
        let cc_voter = Voter::ConstitutionalCommittee(cc_hot);
        vote_tx
            .body
            .voting_procedures
            .entry(cc_voter)
            .or_default()
            .insert(
                action_id.clone(),
                VotingProcedure {
                    vote: Vote::Yes,
                    anchor: None,
                },
            );
        rules
            .apply_valid_tx(
                &vote_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply HF votes tx");

        // Cross boundaries.  Expected: PV bumps to 10 within ≤8 boundaries.
        let starting_epoch = ctx.current_epoch.0;
        for next_epoch in (starting_epoch + 1)..=(starting_epoch + 8) {
            rules
                .process_epoch_transition(
                    EpochNo(next_epoch),
                    &ctx,
                    &mut utxo,
                    &mut certs,
                    &mut gov,
                    &mut epochs,
                    &mut consensus,
                )
                .expect("epoch transition must not fail");
            if epochs.protocol_params.protocol_version_major == 10 {
                return; // success — HF enacted
            }
        }

        panic!(
            "HardForkInitiation(PV9→PV10) never enacted after 8 epoch boundaries; \
             current PV={}.{}, proposals_remaining={:?}, enacted_hard_fork={:?}",
            epochs.protocol_params.protocol_version_major,
            epochs.protocol_params.protocol_version_minor,
            gov.governance.proposals.keys().collect::<Vec<_>>(),
            gov.governance.enacted_hard_fork,
        );
    }

    /// Regression test for issue #496: three chained UpdateCommittee proposals
    /// (each with a `prev_action_id` pointing at the previous proposal's action ID)
    /// must each enact through the era-rules chunk-replay code path.
    ///
    /// All three proposals are submitted in the SAME epoch (before the first
    /// boundary), matching the real preview-testnet chain where proposals 1, 2, 3
    /// were submitted at epochs 992, 996, 1011, all well before ratification at
    /// boundary 1041→1042.
    ///
    ///   Proposal 1 (prev=None)      → adds p1_member (enacted at boundary E+1)
    ///   Proposal 2 (prev=Proposal1) → adds p2_member (enacted at boundary E+2)
    ///   Proposal 3 (prev=Proposal2) → removes p1_member (enacted at boundary E+3)
    ///
    /// The delaying-action rule means only one UpdateCommittee can enact per
    /// boundary.  The ratification snapshot carries `enacted_committee` forward
    /// across boundaries, allowing each successive proposal to see the correct
    /// `prev_action_id` chain.
    #[test]
    fn test_conway_updatecommittee_chained_prev_action_id_enacts_through_era_rules() {
        use crate::state::DRepRegistration;
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::transaction::{GovAction, GovActionId, Rational, Voter};

        // PV10 (post-bootstrap), lower DRep/SPO thresholds for small electorate.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        params.committee_min_size = 0;
        params.gov_action_lifetime = 30; // Matches preview testnet
        params.committee_max_term_length = 365; // Matches preview testnet
        params.dvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };
        params.pvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };

        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();
        epochs.prev_protocol_params = params.clone();
        epochs.prev_protocol_version_major = 10;
        let mut consensus = make_consensus_sub();

        // ── Seed DRep electorate (10 DReps, 1B stake each delegated to them).
        for i in 0..10u8 {
            let drep_cred = Credential::VerificationKey(Hash28::from_bytes([i; 28]));
            let drep_key = Credential::to_typed_hash32(&drep_cred);
            Arc::make_mut(&mut gov.governance).dreps.insert(
                drep_key,
                DRepRegistration {
                    credential: drep_cred.clone(),
                    deposit: Lovelace(500_000_000),
                    anchor: None,
                    registered_epoch: EpochNo(0),
                    drep_expiry: EpochNo(200),
                    active: true,
                },
            );
            let stake_key = Hash32::from_bytes([200u8 + i; 32]);
            Arc::make_mut(&mut gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::KeyHash(drep_key));
            certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }

        // ── Seed SPO electorate (5 pools, 1B stake each).
        for i in 0..5u8 {
            let pool_id = Hash28::from_bytes([100u8 + i; 28]);
            Arc::make_mut(&mut certs.pool_params).insert(
                pool_id,
                PoolRegistration {
                    pool_id,
                    vrf_keyhash: Hash32::ZERO,
                    pledge: Lovelace(1_000_000),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 100,
                    reward_account: vec![],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
            let stake_key = Hash32::from_bytes([150u8 + i; 32]);
            certs.delegations.insert(stake_key, pool_id);
            certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }

        // Prime the mark/set snapshots with SPO stake so the ratification
        // denominator is populated (ratify_proposals_impl reads snapshots.set
        // for total_spo_stake; absent that, falls back to live state).
        let mark_pool_stake: HashMap<Hash28, Lovelace> = (0..5u8)
            .map(|i| (Hash28::from_bytes([100u8 + i; 28]), Lovelace(1_000_000_000)))
            .collect();
        let pool_params_snap = certs.pool_params.clone();
        let make_snap = || crate::state::StakeSnapshot {
            epoch: ctx.current_epoch,
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_pool_stake.clone(),
            pool_params: pool_params_snap.clone(),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };
        epochs.snapshots.mark = Some(make_snap());
        epochs.snapshots.set = Some(make_snap());
        epochs.snapshots.go = Some(make_snap());

        // Helper: append 10 DRep + 5 SPO yes-votes for `action_id` into `vote_tx`.
        let append_votes = |vote_tx: &mut Transaction, action_id: &GovActionId| {
            for i in 0..10u8 {
                let voter = Voter::DRep(Credential::VerificationKey(Hash28::from_bytes([i; 28])));
                vote_tx
                    .body
                    .voting_procedures
                    .entry(voter)
                    .or_default()
                    .insert(
                        action_id.clone(),
                        VotingProcedure {
                            vote: Vote::Yes,
                            anchor: None,
                        },
                    );
            }
            for i in 0..5u8 {
                let pool_hash32 = Hash28::from_bytes([100u8 + i; 28]).to_hash32_padded();
                let voter = Voter::StakePool(pool_hash32);
                vote_tx
                    .body
                    .voting_procedures
                    .entry(voter)
                    .or_default()
                    .insert(
                        action_id.clone(),
                        VotingProcedure {
                            vote: Vote::Yes,
                            anchor: None,
                        },
                    );
            }
        };

        // ── Proposal 1: prev_action_id = None, adds p1_member.
        let p1_member_cred = Credential::VerificationKey(Hash28::from_bytes([0xA1; 28]));
        let p1_member_key = Credential::to_typed_hash32(&p1_member_cred);
        let mut p1_members = BTreeMap::new();
        p1_members.insert(p1_member_cred.clone(), 200u64); // expiry epoch 200

        let proposal1 = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove: vec![],
                members_to_add: p1_members,
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            anchor: Anchor {
                url: "https://test/p1".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let p1_tx_hash = Hash32::from_bytes([0x01u8; 32]);
        let action_id_1 = GovActionId {
            transaction_id: p1_tx_hash,
            action_index: 0,
        };

        // ── Proposal 2: prev_action_id = Some(action_id_1), adds p2_member.
        let p2_member_cred = Credential::VerificationKey(Hash28::from_bytes([0xA2; 28]));
        let p2_member_key = Credential::to_typed_hash32(&p2_member_cred);
        let mut p2_members = BTreeMap::new();
        p2_members.insert(p2_member_cred.clone(), 200u64);

        let proposal2 = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::UpdateCommittee {
                prev_action_id: Some(action_id_1.clone()),
                members_to_remove: vec![],
                members_to_add: p2_members,
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            anchor: Anchor {
                url: "https://test/p2".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let p2_tx_hash = Hash32::from_bytes([0x02u8; 32]);
        let action_id_2 = GovActionId {
            transaction_id: p2_tx_hash,
            action_index: 0,
        };

        // ── Proposal 3: prev_action_id = Some(action_id_2), removes p1_member.
        let proposal3 = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::UpdateCommittee {
                prev_action_id: Some(action_id_2.clone()),
                members_to_remove: vec![p1_member_cred.clone()],
                members_to_add: BTreeMap::new(),
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            anchor: Anchor {
                url: "https://test/p3".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let p3_tx_hash = Hash32::from_bytes([0x03u8; 32]);
        let action_id_3 = GovActionId {
            transaction_id: p3_tx_hash,
            action_index: 0,
        };

        // ── Submit ALL three proposals in a single tx BEFORE the first boundary.
        //    This matches the real chain: proposals submitted at epochs 992/996/1011,
        //    all well before the ratification boundary at 1041→1042.
        let mut prop_tx = make_tx(0x01, vec![], vec![], 0);
        // Override the hash so action_id_1 matches.
        prop_tx.hash = p1_tx_hash;
        prop_tx.body.proposal_procedures = vec![proposal1];
        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply proposal 1 tx");
        assert!(
            gov.governance.proposals.contains_key(&action_id_1),
            "proposal 1 must be ingested"
        );

        let mut prop2_tx = make_tx(0x02, vec![], vec![], 0);
        prop2_tx.hash = p2_tx_hash;
        prop2_tx.body.proposal_procedures = vec![proposal2];
        rules
            .apply_valid_tx(
                &prop2_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply proposal 2 tx");
        assert!(
            gov.governance.proposals.contains_key(&action_id_2),
            "proposal 2 must be ingested"
        );

        let mut prop3_tx = make_tx(0x03, vec![], vec![], 0);
        prop3_tx.hash = p3_tx_hash;
        prop3_tx.body.proposal_procedures = vec![proposal3];
        rules
            .apply_valid_tx(
                &prop3_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply proposal 3 tx");
        assert!(
            gov.governance.proposals.contains_key(&action_id_3),
            "proposal 3 must be ingested"
        );

        // ── Submit all votes for all three proposals in a single tx.
        let mut vote_tx = make_tx(0x10, vec![], vec![], 0);
        append_votes(&mut vote_tx, &action_id_1);
        append_votes(&mut vote_tx, &action_id_2);
        append_votes(&mut vote_tx, &action_id_3);
        rules
            .apply_valid_tx(
                &vote_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply votes tx");

        let starting_epoch = ctx.current_epoch.0;

        // ── Boundary E+1: proposal 1 ratifies (prev=None matches enacted_committee=None).
        //    Proposals 2 and 3 are delayed (UpdateCommittee is a delaying action).
        rules
            .process_epoch_transition(
                EpochNo(starting_epoch + 1),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition E+1");

        assert!(
            gov.governance
                .committee_expiration
                .contains_key(&p1_member_key),
            "proposal 1 must have enacted after E+1; \
             enacted_committee={:?}, proposals_remaining={:?}",
            gov.governance.enacted_committee,
            gov.governance.proposals.keys().collect::<Vec<_>>(),
        );
        assert_eq!(
            gov.governance.enacted_committee,
            Some(action_id_1.clone()),
            "enacted_committee must be proposal 1's ID after E+1"
        );
        // The ratification snapshot must carry enacted_id_1 so that proposal 2's
        // prev_action_as_expected check passes at E+2 ratification.
        let snap = gov
            .governance
            .ratification_snapshot
            .as_ref()
            .expect("ratification snapshot must be populated after E+1");
        assert_eq!(
            snap.enacted_committee,
            Some(action_id_1.clone()),
            "ratification snapshot must carry proposal 1's ID for proposal 2 chain check"
        );
        assert!(
            snap.proposals.contains_key(&action_id_2),
            "snapshot must contain proposal 2 so it is visible at E+2 ratification; \
             snap.proposals={:?}",
            snap.proposals.keys().collect::<Vec<_>>()
        );

        // ── Boundary E+2: proposal 2 ratifies (prev=action_id_1, snapshot.enacted=action_id_1).
        rules
            .process_epoch_transition(
                EpochNo(starting_epoch + 2),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition E+2");

        assert!(
            gov.governance
                .committee_expiration
                .contains_key(&p2_member_key),
            "proposal 2 must have enacted after E+2; \
             enacted_committee={:?}, proposals_remaining={:?}",
            gov.governance.enacted_committee,
            gov.governance.proposals.keys().collect::<Vec<_>>(),
        );
        assert_eq!(
            gov.governance.enacted_committee,
            Some(action_id_2.clone()),
            "enacted_committee must be proposal 2's ID after E+2"
        );

        // ── Boundary E+3: proposal 3 ratifies (prev=action_id_2, snapshot.enacted=action_id_2).
        rules
            .process_epoch_transition(
                EpochNo(starting_epoch + 3),
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
                &mut consensus,
            )
            .expect("epoch transition E+3");

        // Proposal 3 removes p1_member.
        assert!(
            !gov.governance
                .committee_expiration
                .contains_key(&p1_member_key),
            "proposal 3 must remove p1_member from committee after E+3; \
             enacted_committee={:?}, committee={:?}",
            gov.governance.enacted_committee,
            gov.governance
                .committee_expiration
                .keys()
                .collect::<Vec<_>>(),
        );
        // p2_member was added by proposal 2 and not removed by proposal 3.
        assert!(
            gov.governance
                .committee_expiration
                .contains_key(&p2_member_key),
            "p2_member must remain in committee after E+3"
        );
        assert_eq!(
            gov.governance.enacted_committee,
            Some(action_id_3.clone()),
            "enacted_committee must be proposal 3's ID after E+3"
        );
    }

    // -----------------------------------------------------------------------
    // InvalidPrevGovActionId — GOV rule block-apply path (process_governance_votes_and_proposals)
    //
    // These tests exercise the production code path (ConwayRules::apply_valid_tx →
    // process_governance_votes_and_proposals) rather than the LedgerState::process_proposal
    // path used by the governance.rs tests.  The distinction matters: the block-apply path
    // previously skipped all prev_action_id validation (see commit following b0a6da398),
    // admitting proposals that would later silently fail ratification at every epoch
    // boundary (the devnet-validate Round 2 gov-lifecycle 10e timeout).
    // -----------------------------------------------------------------------

    /// A proposal with `prev_action_id = Some(stale_id)` where `stale_id` does not
    /// exist in the active proposals map AND does not match the enacted root must be
    /// silently dropped by the GOV rule (InvalidPrevGovActionId).
    ///
    /// Repro: devnet Round 2 submits ParameterChange with prev=Round1's enacted ID,
    /// but Round 2 is a fresh chain so that ID is not in the graph nor the enacted root.
    /// Before the fix, `process_governance_votes_and_proposals` inserted it anyway.
    #[test]
    fn test_stale_prev_action_id_rejected_via_apply_path() {
        use dugite_primitives::transaction::{GovAction, GovActionId};

        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();

        // The stale action ID references a tx/index that does NOT exist on this chain.
        let stale_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xDE; 32]),
            action_index: 0,
        };
        // No enacted root, no active proposals — stale_id is neither.

        let proposal = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::ParameterChange {
                prev_action_id: Some(stale_id),
                protocol_param_update: Box::new(
                    dugite_primitives::transaction::ProtocolParamUpdate {
                        n_opt: Some(500),
                        ..Default::default()
                    },
                ),
                policy_hash: None,
            },
            anchor: Anchor {
                url: "https://test".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let mut prop_tx = make_tx(0x50, vec![], vec![], 0);
        prop_tx.body.proposal_procedures = vec![proposal];

        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply_valid_tx must not error — GOV rule silently drops bad proposals");

        let action_id = GovActionId {
            transaction_id: prop_tx.hash,
            action_index: 0,
        };
        assert!(
            !gov.governance.proposals.contains_key(&action_id),
            "proposal with stale prev_action_id must be rejected (InvalidPrevGovActionId) \
             via the GOV rule block-apply path; proposals={:?}",
            gov.governance.proposals.keys().collect::<Vec<_>>(),
        );
    }

    /// #898: an `UpdateCommittee` proposal chaining onto the **enacted committee
    /// root** must be admitted — and is silently dropped when that root is `None`.
    ///
    /// This is the exact shape that wedged preview. Ledger snapshots imported
    /// from Haskell (Mithril bootstrap) used to discard `Proposals.pRoots`, so
    /// every `enacted_*` root came back `None`. Preview then produced
    /// `UpdateCommittee` action `65c41d16…#0` with
    /// `prev_action_id = Some(ac993231…#0)` — the real enacted committee root.
    /// With the root missing, `prev_action_matches_enacted_root` failed and the
    /// proposal was dropped (no error, no block rejection), which meant:
    ///
    /// 1. votes on it were rejected as `GovActionsDoNotExist` (masked by the
    ///    "trusting on-chain consensus" fallback),
    /// 2. it never ratified, so dugite's committee diverged from the chain's,
    /// 3. its 1000-ADA deposit was never refunded to the return account, whose
    ///    snapshot stake stayed 1_000_000_000 lovelace below Haskell's — which
    ///    lowered `totalActiveStake`, hence every pool's `appPerf`, hence every
    ///    reward, until an exact-drain withdrawal failed and chain advance
    ///    halted (see `state::rewards` tests).
    ///
    /// The real proposal added script-hash committee members, so this test uses
    /// script credentials too.
    #[test]
    fn test_update_committee_needs_enacted_root_898() {
        use dugite_primitives::transaction::{GovAction, GovActionId};
        use std::collections::BTreeMap;

        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        // The chain's last enacted committee action (preview: ac993231…#0).
        let enacted_root = GovActionId {
            transaction_id: Hash32::from_bytes([0xAC; 32]),
            action_index: 0,
        };

        // Three script-hash committee members, as in the real proposal.
        let mut members_to_add: BTreeMap<Credential, u64> = BTreeMap::new();
        for fill in [0x6au8, 0x88, 0xbe] {
            members_to_add.insert(Credential::Script(Hash28::from_bytes([fill; 28])), 1720);
        }

        let build_tx = || {
            let proposal = ProposalProcedure {
                deposit: Lovelace(1_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::UpdateCommittee {
                    prev_action_id: Some(enacted_root.clone()),
                    members_to_remove: vec![],
                    members_to_add: members_to_add.clone(),
                    threshold: dugite_primitives::transaction::Rational {
                        numerator: 2,
                        denominator: 3,
                    },
                },
                anchor: Anchor {
                    url: "ipfs://QmbPE322FGQBPsg26pAS97nZzMtueiihTdBorqWwJTmhgW".to_string(),
                    data_hash: Hash32::ZERO,
                },
            };
            let mut tx = make_tx(0x65, vec![], vec![], 0);
            tx.body.proposal_procedures = vec![proposal];
            tx
        };

        // Returns whether the proposal was admitted for a given enacted root.
        let admitted = |root: Option<GovActionId>| -> bool {
            let mut utxo = make_utxo_sub(vec![]);
            let mut certs = make_cert_sub();
            let mut gov = make_gov_sub();
            let mut epochs = make_epoch_sub();
            epochs.protocol_params = params.clone();
            Arc::make_mut(&mut gov.governance).enacted_committee = root;

            let tx = build_tx();
            rules
                .apply_valid_tx(
                    &tx,
                    BlockValidationMode::ApplyOnly,
                    &ctx,
                    &mut utxo,
                    &mut certs,
                    &mut gov,
                    &mut epochs,
                )
                .expect("apply_valid_tx must not error — the GOV rule drops silently");
            gov.governance.proposals.contains_key(&GovActionId {
                transaction_id: tx.hash,
                action_index: 0,
            })
        };

        assert!(
            admitted(Some(enacted_root.clone())),
            "#898: an UpdateCommittee proposal whose prev_action_id IS the enacted \
             committee root must be admitted. Rejecting it strands the proposal's \
             deposit and diverges the committee from the chain."
        );
        assert!(
            !admitted(None),
            "sanity: with enacted_committee = None the same proposal is dropped — \
             this is precisely the state a Haskell-snapshot import used to produce, \
             and why `decode_proposals_with_roots` must populate the roots."
        );
    }

    /// #858: the LIVE block-apply GOV rule must drop a HardForkInitiation whose
    /// target version does not `pvCanFollow` the current version (skip-minor), and
    /// admit one that does (exact +1 minor). Previously the live path ran NO
    /// pvCanFollow check, so a skip-minor target was silently admitted at block apply.
    #[test]
    fn test_hardfork_pvcanfollow_enforced_via_apply_path_858() {
        use dugite_primitives::transaction::{GovAction, GovActionId};

        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let run = |seed: u8, ver: (u64, u64)| -> bool {
            let mut utxo = make_utxo_sub(vec![]);
            let mut certs = make_cert_sub();
            let mut gov = make_gov_sub();
            let mut epochs = make_epoch_sub();
            epochs.protocol_params = params.clone();
            epochs.protocol_params.protocol_version_major = 10;
            epochs.protocol_params.protocol_version_minor = 0;

            let proposal = ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::HardForkInitiation {
                    prev_action_id: None,
                    protocol_version: ver,
                },
                anchor: Anchor {
                    url: "https://test".to_string(),
                    data_hash: Hash32::ZERO,
                },
            };
            let mut tx = make_tx(seed, vec![], vec![], 0);
            tx.body.proposal_procedures = vec![proposal];

            rules
                .apply_valid_tx(
                    &tx,
                    BlockValidationMode::ApplyOnly,
                    &ctx,
                    &mut utxo,
                    &mut certs,
                    &mut gov,
                    &mut epochs,
                )
                .expect("apply_valid_tx must not error — GOV rule silently drops bad proposals");
            let id = GovActionId {
                transaction_id: tx.hash,
                action_index: 0,
            };
            gov.governance.proposals.contains_key(&id)
        };

        assert!(
            !run(0x60, (10, 2)),
            "skip-minor HardForkInitiation (10,0)->(10,2) must be dropped by the live GOV rule (#858)"
        );
        assert!(
            run(0x61, (10, 1)),
            "exact +1 minor HardForkInitiation (10,0)->(10,1) must be admitted by the live GOV rule"
        );
    }

    /// A proposal with `prev_action_id = Some(enacted_id)` where `enacted_id` is the
    /// last enacted root for its purpose must be ADMITTED by the GOV rule.
    ///
    /// This is the Round 2 happy-path: 10a reads the enacted action ID from
    /// `enacted.actionid` and correctly chains from it.
    #[test]
    fn test_enacted_root_chained_action_admitted_via_apply_path() {
        use dugite_primitives::transaction::{GovAction, GovActionId};

        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();

        // Simulate that a prior ParameterChange was enacted: set enacted_pparam_update.
        let enacted_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAB; 32]),
            action_index: 0,
        };
        Arc::make_mut(&mut gov.governance).enacted_pparam_update = Some(enacted_id.clone());

        // New proposal chains from the enacted root.
        let proposal = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::ParameterChange {
                prev_action_id: Some(enacted_id.clone()),
                protocol_param_update: Box::new(
                    dugite_primitives::transaction::ProtocolParamUpdate {
                        n_opt: Some(501),
                        ..Default::default()
                    },
                ),
                policy_hash: None,
            },
            anchor: Anchor {
                url: "https://test".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let mut prop_tx = make_tx(0x51, vec![], vec![], 0);
        prop_tx.body.proposal_procedures = vec![proposal];

        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply_valid_tx must not error");

        let action_id = GovActionId {
            transaction_id: prop_tx.hash,
            action_index: 0,
        };
        assert!(
            gov.governance.proposals.contains_key(&action_id),
            "proposal with prev_action_id = enacted root must be admitted; \
             proposals={:?}",
            gov.governance.proposals.keys().collect::<Vec<_>>(),
        );
    }

    /// A proposal with `prev_action_id = None` on a fresh chain (no enacted root)
    /// must be ADMITTED as a genesis root proposal.
    ///
    /// Regression guard for b0a6da398's positive path: genesis-root proposals
    /// are valid on a brand-new chain where nothing of that purpose has been enacted.
    #[test]
    fn test_genesis_proposal_admitted_when_no_enacted_root_via_apply_path() {
        use dugite_primitives::transaction::{GovAction, GovActionId};

        let params = ProtocolParameters::mainnet_defaults();
        let ctx = make_conway_ctx(&params);
        let rules = ConwayRules::new();

        let mut utxo = make_utxo_sub(vec![]);
        let mut certs = make_cert_sub();
        let mut gov = make_gov_sub();
        let mut epochs = make_epoch_sub();
        epochs.protocol_params = params.clone();
        // enacted_pparam_update is None (fresh chain) — genesis root proposal must be accepted.

        let proposal = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::ParameterChange {
                prev_action_id: None, // genesis root
                protocol_param_update: Box::new(
                    dugite_primitives::transaction::ProtocolParamUpdate {
                        n_opt: Some(500),
                        ..Default::default()
                    },
                ),
                policy_hash: None,
            },
            anchor: Anchor {
                url: "https://test".to_string(),
                data_hash: Hash32::ZERO,
            },
        };

        let mut prop_tx = make_tx(0x52, vec![], vec![], 0);
        prop_tx.body.proposal_procedures = vec![proposal];

        rules
            .apply_valid_tx(
                &prop_tx,
                BlockValidationMode::ApplyOnly,
                &ctx,
                &mut utxo,
                &mut certs,
                &mut gov,
                &mut epochs,
            )
            .expect("apply_valid_tx must not error");

        let action_id = GovActionId {
            transaction_id: prop_tx.hash,
            action_index: 0,
        };
        assert!(
            gov.governance.proposals.contains_key(&action_id),
            "genesis-root proposal (prev_action_id=None) must be admitted on a fresh chain; \
             proposals={:?}",
            gov.governance.proposals.keys().collect::<Vec<_>>(),
        );
    }
}
