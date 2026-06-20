//! Block application logic: thin orchestrator dispatching to `EraRulesImpl`.
//!
//! This module contains the core block processing pipeline for the Dugite ledger,
//! implemented as a thin orchestrator that delegates era-specific logic to
//! [`EraRulesImpl`](crate::eras::EraRulesImpl) while retaining cross-cutting
//! concerns (validation, epoch transitions) inline.
//!
//! The orchestrator is responsible for:
//!
//! - Verifying block connectivity (prev_hash chain)
//! - Detecting and dispatching HFC era boundary transformations (`on_era_transition`)
//! - Detecting and triggering epoch transitions (via existing `process_epoch_transition`)
//! - Dispatching block body validation (`validate_block_body`)
//! - Phase-1 and Phase-2 (Plutus) transaction validation (ValidateAll mode)
//! - Dispatching per-transaction apply logic to era rules
//! - Pre-Conway protocol parameter update proposal collection
//! - Dispatching nonce evolution and block production tracking (`evolve_nonce`)

use super::{credential_to_hash, BlockValidationMode, LedgerError, LedgerState};
use crate::eras::byron::{apply_byron_block, ByronApplyMode, ByronFeePolicy};
use crate::eras::{EraRules, EraRulesImpl, RuleContext};
use crate::ledger_seq::{BlockFieldsDelta, LedgerDelta};
#[cfg(not(feature = "parallel-verification"))]
use crate::plutus::evaluate_plutus_scripts;
#[cfg(feature = "parallel-verification")]
use crate::plutus::{capture_phase2_work_item, run_phase2_parallel, Phase2WorkItem};
use crate::utxo_diff::UtxoDiff;
use crate::validation::{calculate_ref_script_size, ValidationError};
use dugite_primitives::block::{Block, Point};
use dugite_primitives::era::Era;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::Certificate;
use dugite_primitives::value::Lovelace;
use std::sync::Arc;
use tracing::{debug, trace, warn};

/// Maximum total reference script size allowed in a single transaction.
///
/// Source: Haskell `ppMaxRefScriptSizePerTxG = L.to . const $ 200 * 1024`
/// (Conway PParams). Also hardcoded, not a governance-updateable protocol parameter.
///
/// Enforced in `apply_block` for `ValidateAll` mode: any transaction whose
/// combined spending-input + reference-input script_ref byte count exceeds this
/// limit is rejected with [`LedgerError::BlockTxValidationFailed`].
const MAX_REF_SCRIPT_SIZE_PER_TX: u64 = 200 * 1024; // 200 KiB

/// Whether a phase-2 collection error (Haskell `UtxosFailure CollectErrors`)
/// is BLOCK-FATAL at apply (#733). Default true — the Haskell-faithful
/// behaviour for Babbage+ blocks. `DUGITE_PHASE2_APPLY_FATAL=0` reverts to
/// warn-and-trust as an operational escape hatch (e.g. to keep syncing past
/// a suspected false fatality while it is reported). Read once per process
/// for determinism.
fn phase2_collect_fatal_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("DUGITE_PHASE2_APPLY_FATAL")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true)
    })
}

/// Apply the result of a block's Phase-2 (Plutus) evaluation: convert the
/// per-tx [`Phase2Outcome`]s into the block-fatal rejection decision (Babbage+
/// collection error) or a divergence warning, exactly as Step 8d does inline.
///
/// Extracted so the deferred bulk-sync path can reproduce the identical
/// fatality decision after pooling+evaluating work items across many blocks
/// (see [`LedgerState::apply_block_defer_phase2`]). It is a **pure function of
/// `(block, outcomes)`** — it mutates no ledger state (state is already applied
/// in Step 8b), so deferring *when* it runs cannot change *what* it decides.
///
/// Returns `Err(Phase2CollectErrors)` for a block-fatal collection error and
/// `Ok(())` otherwise (logging is_valid divergences warn-and-trust).
#[cfg(feature = "parallel-verification")]
pub fn apply_phase2_outcomes(
    block: &Block,
    outcomes: Vec<crate::plutus::Phase2Outcome>,
) -> Result<(), LedgerError> {
    for outcome in outcomes {
        // Look up the transaction by index. Duplicate txs were skipped during
        // the loop (not added to work_items), so every outcome.tx_idx is valid.
        let tx = match block.transactions.get(outcome.tx_idx) {
            Some(t) => t,
            None => continue,
        };
        // #733: a phase-2 COLLECTION error is block-fatal in Babbage+ regardless
        // of the is_valid tag — Haskell raises `UtxosFailure (CollectErrors …)`
        // before any script runs, so every honest node rejects this block.
        // Carve-outs: CEK panics (not a Haskell error class — correction 3) and
        // UTxO-gap work items (inputs unresolved during best-effort partial
        // replay — correction 4) stay warn-and-trust. Alonzo blocks never arm
        // the time-translation horizon (correction 2) and keep warn-only
        // semantics for the remaining collect classes (no false fatality).
        if let Err(e) = &outcome.result {
            if e.is_eval_panic() {
                warn!(
                    tx_hash = %tx.hash.to_hex(),
                    slot = block.slot().0,
                    error = %e,
                    "Phase-2 evaluator PANIC on confirmed block — trusting \
                     on-chain consensus (dugite CEK robustness gap; never \
                     block-fatal at apply)"
                );
                continue;
            }
            if e.is_collect_error()
                && block.era >= Era::Babbage
                && outcome.utxo_complete
                && phase2_collect_fatal_enabled()
            {
                return Err(LedgerError::Phase2CollectErrors {
                    slot: block.slot().0,
                    tx_hash: tx.hash.to_hex(),
                    error: e.to_string(),
                });
            }
        }
        if outcome.is_valid {
            // is_valid=true: phase-2 failure is a ScriptFailed on a confirmed
            // block — log a warning and trust on-chain consensus.
            if let Err(e) = outcome.result {
                warn!(
                    tx_hash = %tx.hash.to_hex(),
                    slot = block.slot().0,
                    error = %e,
                    "Plutus evaluation divergence (parallel): uplc says scripts fail \
                     but block is_valid=true on-chain — trusting on-chain consensus"
                );
            }
        } else {
            // is_valid=false: the producer's scripts failed on-chain (collateral
            // consumed). dugite's CEK says they pass — a Phase-2 evaluation
            // divergence. On a block received via ChainSync the is_valid flag is
            // CONSENSUS TRUTH: honest (Haskell) nodes enforce it with a correct
            // CEK, so any block on the selected chain carries a genuine flag and
            // the divergence is a dugite-CEK bug, not collateral theft. The tx
            // was ALREADY applied as invalid (collateral consumed, no outputs) in
            // Step 8b, so the ledger state is byte-exact regardless. Trust the
            // on-chain flag and log the divergence (symmetric with the
            // is_valid=true branch above) instead of hard-halting the whole sync.
            // The DUGITE_PHASE2_DUMP_DIR repro was captured in
            // run_phase2_parallel for offline CEK root-causing.
            if outcome.result.is_ok() {
                warn!(
                    tx_hash = %tx.hash.to_hex(),
                    slot = block.slot().0,
                    "Plutus evaluation divergence (parallel): uplc says scripts PASS \
                     but block is_valid=false on-chain — trusting on-chain consensus \
                     (tx applied as invalid; dugite CEK over-permissive — see \
                     DUGITE_PHASE2_DUMP_DIR)"
                );
            }
        }
    }
    Ok(())
}

impl LedgerState {
    /// Build a read-only rule context for era rule dispatch.
    ///
    /// Assembles all the immutable per-block parameters that era rules need
    /// without requiring a `&mut self` borrow.
    ///
    /// NOTE: Cannot be used inside the per-tx loop of `apply_block` because it
    /// borrows `&self` (including `&self.epochs.protocol_params`) which conflicts
    /// with the `&mut self.epochs` needed for `apply_valid_tx`. Use inline
    /// `RuleContext` construction with `cached_params` instead.
    #[allow(dead_code)]
    fn build_rule_context<'a>(&'a self, block: &Block, tx_index: u64) -> RuleContext<'a> {
        RuleContext {
            params: &self.epochs.protocol_params,
            current_slot: block.slot().0,
            current_epoch: self.epoch,
            era: block.era,
            slot_config: Some(&self.slot_config),
            node_network: self.node_network,
            genesis_delegates: &self.genesis_delegates,
            update_quorum: self.update_quorum,
            epoch_length: self.epoch_length,
            shelley_transition_epoch: self.shelley_transition_epoch,
            byron_epoch_length: self.byron_epoch_length,
            stability_window: self.randomness_stabilisation_window,
            stability_window_3kf: self.stability_window_3kf,
            randomness_stabilisation_window: self.randomness_stabilisation_window,
            tx_index,
            conway_genesis: self.conway_genesis_init.as_ref(),
            max_lovelace_supply: self.max_lovelace_supply,
        }
    }

    /// Apply a block to the ledger state.
    ///
    /// This is the **thin orchestrator** that dispatches era-specific transaction
    /// processing to [`EraRulesImpl`] while retaining cross-cutting concerns
    /// (validation, epoch transitions, nonce evolution) inline.
    ///
    /// When `mode` is `ValidateAll`, each transaction is independently validated
    /// (Phase-1 + Phase-2 Plutus evaluation) and the result is compared against
    /// the block producer's `is_valid` flag. A mismatch rejects the block.
    pub fn apply_block(
        &mut self,
        block: &Block,
        mode: BlockValidationMode,
    ) -> Result<(), LedgerError> {
        // Default path: apply state AND drain Phase-2 inline (byte-identical to
        // the historic behaviour). The returned work-item vec is always empty.
        self.apply_block_impl(block, mode, false)?;
        Ok(())
    }

    /// Apply a block's STATE in-order but **defer** the Phase-2 (Plutus) drain,
    /// returning the captured [`Phase2WorkItem`]s instead of evaluating them.
    ///
    /// Bulk-sync CPU-saturation lever: each [`Phase2WorkItem`] is fully
    /// self-contained (resolved UTxO CBOR + script + per-block `cost_models_cbor`
    /// / `slot_config` / `protocol_major`), so items captured here can be pooled
    /// across many blocks and evaluated together on a rayon pool to fill all
    /// cores — a single block's ~2-3 redeemers can never saturate a 12-core host.
    ///
    /// **Byte-exact contract:** ledger STATE is mutated identically to
    /// [`apply_block`] (Plutus never writes state — it only gates acceptance), so
    /// the caller MUST later run [`apply_phase2_outcomes`] on the drained outcomes
    /// to reproduce the exact block-fatal rejection decision before the block is
    /// exposed. Deferring *when* Plutus runs cannot change *what* it decides
    /// (the fatality verdict is a pure function of the self-contained outcomes).
    pub fn apply_block_defer_phase2(
        &mut self,
        block: &Block,
        mode: BlockValidationMode,
    ) -> Result<Vec<Phase2WorkItem>, LedgerError> {
        self.apply_block_impl(block, mode, true)
    }

    /// Implementation shared by [`apply_block`] (inline drain) and
    /// [`apply_block_defer_phase2`] (deferred drain). When `defer_phase2` is
    /// `true`, Step 8d's `run_phase2_parallel` + fatality loop is skipped and the
    /// captured `phase2_work_items` are returned for later pooled evaluation;
    /// when `false`, the drain runs inline and an empty vec is returned.
    fn apply_block_impl(
        &mut self,
        block: &Block,
        mode: BlockValidationMode,
        defer_phase2: bool,
    ) -> Result<Vec<Phase2WorkItem>, LedgerError> {
        trace!(
            slot = block.slot().0,
            block_no = block.block_number().0,
            era = ?block.era,
            txs = block.transactions.len(),
            hash = %block.header.header_hash.to_hex(),
            "Ledger: applying block"
        );

        // #733: consume the one-shot apply-time phase-2 horizon set by the
        // async caller at the PRE-block ledger tip (corrections 5/6 — a
        // deterministic per-block snapshot, never a `try_read` inside
        // apply). Taken unconditionally so a stale value can never leak
        // into a later block. Alonzo gate (correction 2): Haskell's Alonzo
        // UTXOS translates time via the linearly-extended EpochInfo and
        // filters `BadTranslation` out of CollectErrors
        // (`isNotBadTranslation`, Alonzo/Rules/Utxos.hs) — past-horizon is
        // structurally non-fatal there, so the horizon only arms for
        // Babbage+ blocks.
        let phase2_horizon = self.phase2_apply_horizon.take();
        let phase2_slot_config = {
            let mut sc = self.slot_config;
            sc.safe_zone_horizon_slot = if block.era >= Era::Babbage {
                phase2_horizon
            } else {
                None
            };
            sc
        };
        let _ = &phase2_slot_config; // used by the ValidateAll tx loop below

        // ── Step 1: Verify block connects to current tip ──────────────────
        //
        // A block must have `prev_hash == ledger.tip.hash`; otherwise it
        // belongs to a different chain and applying it would silently corrupt
        // ledger state. The correct handling of a prev_hash mismatch at the
        // live tip is CHAIN SELECTION: rollback the ledger to the common
        // intersection and replay the winning fork. That happens in the sync
        // loop when ChainSelQueue returns `TriggeredFork` — it must NOT be
        // masked here.
        //
        // Historical note: earlier versions of this function accepted any
        // block whose `block_number` was `tip.block_number + 1` even when
        // `prev_hash` did not match, on the rationale that chunk-file replay
        // could produce different CBOR hashes from the network path. That
        // bypass silently papered over fork switches and led to divergence
        // between VolatileDB.selected_chain and the ledger state — including
        // forged blocks being effectively orphaned from our own view (see
        // issue #439). The bypass is retained ONLY for `ApplyOnly` mode
        // (used during startup chunk-file replay, where the block source is
        // our own trusted ImmutableDB).
        //
        // The one legitimate cause of a Byron prev_hash mismatch during replay
        // is a MISSING EPOCH BOUNDARY BLOCK (EBB) in an ImmutableDB built by a
        // pre-fix dugite. On Byron, every pre-OBFT epoch starts with a
        // content-free EBB that shares its `block_no` with the predecessor
        // main block (EBBs do not increment the chain difficulty). The old
        // VolatileDB→ImmutableDB flush walked entries by
        // `block_no >= last_flushed + 1`, so an EBB was silently dropped
        // whenever a flush batch ended exactly at its predecessor (mainnet
        // Byron: epochs 112/135/145 at 1-in-FLUSH_BATCH_SIZE odds, plus the
        // genesis EBB at block_no 0 deterministically). The first block of
        // the next epoch then declares `prev_hash = <EBB hash>`, which cannot
        // match the last block the ledger holds. EBBs carry NO transactions
        // and NO ledger state, so the dropped block does not affect
        // UTxO/reserves/etc., and applying the canonical successor by
        // sequence number is correct. (The decoder's Byron header hash is
        // byte-exact — `decode_byron_main_block` hashes the captured raw
        // header bytes, matching Haskell `headerHashAnnotated`.)
        //
        // The flush no longer drops EBBs (`ChainDB::flush_to_immutable*` +
        // `VolatileDB::selected_chain_entries_bounded` admit same-block_no
        // EBBs), so a fresh sync/re-import stores the full Byron chain and
        // this fallback never fires. It is RETAINED as the safety net for
        // ImmutableDBs built before the fix, which still lack those EBBs
        // until re-imported. `ValidateAll` mode (used for every live block
        // and every forged block) must reject mismatches unconditionally.
        if self.tip.point != Point::Origin {
            if let Some(tip_hash) = self.tip.point.hash() {
                if block.prev_hash() != tip_hash {
                    let is_sequential_successor =
                        block.block_number().0 == self.tip.block_number.0 + 1;
                    match mode {
                        BlockValidationMode::ApplyOnly
                            if is_sequential_successor && block.era == Era::Byron =>
                        {
                            tracing::info!(
                                block_no = block.block_number().0,
                                tip_block = self.tip.block_number.0,
                                slot = block.slot().0,
                                tip_hash = %tip_hash.to_hex(),
                                got_prev = %block.prev_hash().to_hex(),
                                era = ?block.era,
                                "ApplyOnly (Byron): accepting block by sequence number despite \
                                 prev_hash mismatch — this block's prev_hash points at an \
                                 epoch-boundary block (EBB) that a pre-fix flush dropped from \
                                 the ImmutableDB (EBBs share their block_no with the \
                                 predecessor and were skipped at flush batch boundaries). \
                                 EBBs carry no ledger state, so applying the canonical \
                                 successor is correct. Re-import (mithril-import or \
                                 from-genesis re-sync) to restore the missing EBBs."
                            );
                        }
                        _ => {
                            return Err(LedgerError::BlockDoesNotConnect {
                                expected: tip_hash.to_hex(),
                                got: block.prev_hash().to_hex(),
                            });
                        }
                    }
                }
            }
        }

        // ── Step 2: HFC era boundary transformation ─────────────────────
        //
        // When the block era exceeds the current ledger era, dispatch
        // era-specific state transformations via the trait. For Conway this
        // includes discarding pointer-addressed UTxO stake (TranslateEra
        // equivalent) and resetting donations.
        if block.era > self.era {
            let transition_rules = EraRulesImpl::for_era(block.era);
            // Clone protocol_params to break the aliasing conflict between
            // the immutable borrow in RuleContext and &mut self.epochs.
            let transition_params = self.epochs.protocol_params.clone();
            let transition_ctx = RuleContext {
                params: &transition_params,
                current_slot: block.slot().0,
                current_epoch: self.epoch,
                era: block.era,
                slot_config: Some(&self.slot_config),
                node_network: self.node_network,
                genesis_delegates: &self.genesis_delegates,
                update_quorum: self.update_quorum,
                epoch_length: self.epoch_length,
                shelley_transition_epoch: self.shelley_transition_epoch,
                byron_epoch_length: self.byron_epoch_length,
                stability_window: self.randomness_stabilisation_window,
                stability_window_3kf: self.stability_window_3kf,
                randomness_stabilisation_window: self.randomness_stabilisation_window,
                tx_index: 0,
                conway_genesis: self.conway_genesis_init.as_ref(),
                max_lovelace_supply: self.max_lovelace_supply,
            };
            transition_rules.on_era_transition(
                self.era,
                &transition_ctx,
                &mut self.utxo,
                &mut self.certs,
                &mut self.gov,
                &mut self.consensus,
                &mut self.epochs,
            )?;
            self.pending_era_transition = Some((self.era, block.era, self.epoch));
        }

        // ── Step 3: Epoch transitions ─────────────────────────────────────
        //
        // When multiple epochs are skipped (e.g., after offline time or Mithril
        // import), process each intermediate epoch transition individually.
        // Dispatched through EraRulesImpl for the block's era (post-HFC).
        let block_epoch = EpochNo(self.epoch_of_slot(block.slot().0));
        if block_epoch > self.epoch {
            debug!(
                "Ledger: epoch transition {} -> {} at slot {}",
                self.epoch.0,
                block_epoch.0,
                block.slot().0,
            );
            while self.epoch < block_epoch {
                let next_epoch = EpochNo(self.epoch.0.saturating_add(1));
                let epoch_rules = EraRulesImpl::for_era(block.era);
                let epoch_params = self.epochs.protocol_params.clone();
                let epoch_ctx = RuleContext {
                    params: &epoch_params,
                    current_slot: block.slot().0,
                    current_epoch: self.epoch,
                    era: block.era,
                    slot_config: Some(&self.slot_config),
                    node_network: self.node_network,
                    genesis_delegates: &self.genesis_delegates,
                    update_quorum: self.update_quorum,
                    epoch_length: self.epoch_length,
                    shelley_transition_epoch: self.shelley_transition_epoch,
                    byron_epoch_length: self.byron_epoch_length,
                    stability_window: self.randomness_stabilisation_window,
                    stability_window_3kf: self.stability_window_3kf,
                    randomness_stabilisation_window: self.randomness_stabilisation_window,
                    conway_genesis: self.conway_genesis_init.as_ref(),
                    max_lovelace_supply: self.max_lovelace_supply,
                    tx_index: 0,
                };
                // #615f: per-epoch-boundary full-state dump fires BEFORE
                // process_epoch_transition so it captures the END of the
                // just-ending epoch (matching Haskell's `currentEpoch=N`
                // last-block-of-N emission).  The post-boundary dump used
                // to label as `next_epoch` but reflected start-of-N+1
                // state, which is an OFF-BY-ONE relative to Haskell's
                // splitter (which takes last-per-epoch).
                //
                // We compute the upcoming RUPD here (without applying it)
                // so the dumper can expose `rewards.total_distributed`
                // as the rewards ABOUT TO BE applied at this boundary —
                // mirroring Haskell's `rewardUpdate` JSON field which
                // shows the queued (un-applied) RUPD.
                //
                // The boundary handler then runs and computes the same
                // RUPD again — compute_reward_update is pure, so this
                // double-compute is wasteful but byte-exact correct.
                #[cfg(feature = "epoch-state-debug")]
                {
                    let _reward_accounts_std: std::collections::HashMap<_, _> = self
                        .certs
                        .reward_accounts
                        .iter()
                        .map(|(k, v)| (*k, *v))
                        .collect();
                    // Mirror the boundary handler's pre-AVVM-reserves adjustment at
                    // the Shelley→Allegra boundary (see `pending_avvm_return`); a
                    // non-consuming read so `process_epoch_transition` still applies it.
                    let dbg_reward_reserves = Lovelace(
                        self.epochs
                            .reserves
                            .0
                            .saturating_sub(self.epochs.pending_avvm_return),
                    );
                    let upcoming_rupd = crate::state::rewards::compute_reward_update(
                        &self.epochs.prev_protocol_params,
                        &self.epochs.prev_d,
                        self.epochs.prev_protocol_version_major,
                        self.epochs.snapshots.go.as_ref(),
                        &self.epochs.snapshots.bprev_blocks_by_pool,
                        self.epochs.snapshots.ss_fee,
                        dbg_reward_reserves,
                        self.epochs.treasury,
                        &_reward_accounts_std,
                        self.epochs.rupd_addrs_rew.as_deref(),
                        self.epoch_length,
                        self.shelley_transition_epoch,
                        self.max_lovelace_supply,
                    );
                    crate::state::epoch_state_debug::maybe_dump(
                        self,
                        self.epoch.0,
                        block.slot().0,
                        Some(&upcoming_rupd),
                    );
                }

                epoch_rules.process_epoch_transition(
                    next_epoch,
                    &epoch_ctx,
                    &mut self.utxo,
                    &mut self.certs,
                    &mut self.gov,
                    &mut self.epochs,
                    &mut self.consensus,
                )?;
                self.epoch = next_epoch;
                // #11: the startStep-frozen fvAddrsRew set just consumed by this
                // boundary's RUPD is cleared so the new epoch re-captures it.
                // (Subsequent skipped-epoch boundaries see None → fall back to
                // boundary accounts, matching Haskell's forced startStep.)
                self.epochs.rupd_addrs_rew = None;
            }
        }

        // #11 startStep capture: the first time this (Shelley+) epoch's
        // block slot crosses `epoch_first_slot + randomness_stabilisation_window`
        // (4k/f), freeze the registered reward-account credential set — BEFORE
        // applying this block's certificates. Mirrors Haskell's RUPD pulser
        // `FreeVars.fvAddrsRew = Map.keysSet(accounts)`, captured during TICK
        // (which precedes the block body). The next epoch boundary's
        // `compute_reward_update` uses this frozen set for the pv≤6 member +
        // leader reward prefilters instead of boundary-time accounts. pv≥7
        // bypasses the prefilter, so we skip the (potentially large) snapshot.
        //
        // PV GATE: the RUPD this freeze feeds is the one applied at the NEXT
        // boundary, which Haskell computes under prevPParams — i.e. THIS
        // epoch's previous-boundary params (`prev_protocol_version_major`),
        // NOT the current curPParams. Gating on curPParams skipped the freeze
        // during the Vasil epoch itself (ep365: cur=pv7, prev=pv6): Haskell's
        // pulser still ran the pv6 prefilter with its mid-epoch frozen set,
        // while dugite fell back to boundary-time accounts — accounts
        // deregistered between the 4k/f mark and the boundary were never
        // computed (left in reserves) where Haskell computes-then-routes them
        // to treasury. Observed live at mainnet 365→366: treasury short
        // 857600586 lovelace, reserves correspondingly high.
        if block.era != Era::Byron
            && self.epochs.prev_protocol_version_major <= 6
            && self.epochs.rupd_addrs_rew.is_none()
        {
            let startstep_slot = self
                .first_slot_of_epoch(self.epoch.0)
                .saturating_add(self.randomness_stabilisation_window);
            if block.slot().0 > startstep_slot {
                let frozen: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                    self.certs.reward_accounts.keys().copied().collect();
                self.epochs.rupd_addrs_rew = Some(std::sync::Arc::new(frozen));
            }
        }

        // ── BBODY rule: block body size equality check ────────────────────
        //
        // Haskell enforces `actual_body_bytes == header.body_size`.  We extract the
        // actual serialized body size from the raw CBOR wire bytes (indices 1..4 of
        // the inner block array) and compare against the header's claim.  This only
        // runs in ValidateAll mode and only when raw_cbor is available (i.e., blocks
        // received from the network, not constructed in-memory).
        // Closes #377.
        if mode == BlockValidationMode::ValidateAll {
            if let Some(ref raw) = block.raw_cbor {
                if let Some(actual_body_size) =
                    dugite_serialization::compute_block_body_size_from_cbor(raw)
                {
                    let claimed = block.header.body_size;
                    if actual_body_size != claimed {
                        return Err(LedgerError::WrongBlockBodySize {
                            actual: actual_body_size,
                            claimed,
                        });
                    }
                }
            }
        }

        // Allocate a per-block diff to record all UTxO inserts and deletes.
        let mut block_diff = UtxoDiff::new();

        // ── Step 5: Byron early return ────────────────────────────────────
        //
        // Byron has no scripts, certificates, withdrawals, governance, or
        // multi-asset. Process via dedicated Byron path with per-tx sequential
        // application (earlier tx outputs visible to later txs in the same block).
        if block.era == Era::Byron {
            // Byron fee policy is a network-wide genesis constant
            // (a + ceiling(size*b), b an exact rational), not the Shelley integer
            // params carried in `protocol_params`. See `ByronFeePolicy`.
            let fee_policy = ByronFeePolicy::canonical();
            let byron_mode = match mode {
                BlockValidationMode::ValidateAll => ByronApplyMode::ValidateAll,
                BlockValidationMode::ApplyOnly => ByronApplyMode::ApplyOnly,
            };

            // Process each Byron transaction one at a time so that outputs
            // created by an earlier transaction in the block are immediately
            // visible to later transactions (within-block spending chains).
            let mut total_byron_fees = Lovelace(0);
            let mut seen_hashes =
                std::collections::HashSet::with_capacity(block.transactions.len());
            for tx in &block.transactions {
                if !seen_hashes.insert(tx.hash) {
                    warn!(
                        tx_hash = %tx.hash.to_hex(),
                        slot = block.slot().0,
                        "Byron: duplicate tx hash in block, skipping"
                    );
                    continue;
                }
                let effect = apply_byron_block(
                    std::slice::from_ref(tx),
                    fee_policy,
                    block.slot().0,
                    byron_mode,
                    |input| self.utxo.utxo_set.lookup(input),
                )
                .map_err(|e| LedgerError::BlockTxValidationFailed {
                    slot: e.slot,
                    tx_hash: e.tx_hash,
                    errors: e.reason.to_string(),
                })?;

                // Apply each tx's effects immediately so subsequent txs in the
                // same block see the correct UTxO state.
                for input in &effect.spent {
                    if let Some(spent_output) = self.utxo.utxo_set.lookup(input) {
                        block_diff.record_delete(input.clone(), spent_output);
                    }
                    self.utxo.utxo_set.remove(input);
                }
                for (input, output) in effect.created {
                    block_diff.record_insert(input.clone(), output.clone());
                    self.utxo.utxo_set.insert(input, output);
                }
                total_byron_fees.0 = total_byron_fees.0.saturating_add(effect.fees.0);
            }
            self.utxo.epoch_fees += total_byron_fees;

            // Track block production (Byron uses OBFT, not VRF)
            if !block.header.issuer_vkey.is_empty() {
                let pool_id = dugite_primitives::hash::blake2b_224(&block.header.issuer_vkey);
                *Arc::make_mut(&mut self.consensus.epoch_blocks_by_pool)
                    .entry(pool_id)
                    .or_insert(0) += 1;
            }
            self.consensus.epoch_block_count += 1;

            // Byron (PBFT/OBFT) does NOT maintain the TPraos `csLabNonce`: in
            // Haskell the Byron ChainDepState has no nonce fields, and
            // `translateChainDepStateByronToShelley` initialises `csLabNonce` to
            // `NeutralNonce`. Keeping `lab_nonce` at NeutralNonce (ZERO) here is
            // load-bearing: the first Shelley epoch-nonce TICKN (mainnet 207->208)
            // copies `lab_nonce` into `last_epoch_block_nonce`, and if that holds a
            // Byron prev-hash then η0(209) = candidate(208) ⭒ byron_hash instead of
            // candidate(208) ⭒ NeutralNonce, breaking VRF on the first epoch-209
            // block. The FIRST Shelley block is what first sets `lab_nonce` (see
            // common.rs / shelley evolve_nonce).
            self.consensus.lab_nonce = dugite_primitives::hash::Hash32::ZERO;

            self.tip = block.tip();
            if block.era > self.era {
                self.pending_era_transition = Some((self.era, block.era, self.epoch));
            }
            self.era = block.era;

            self.utxo.diff_seq.push_bounded(
                block.slot(),
                *block.hash(),
                block_diff,
                self.security_param as usize,
            );

            trace!(
                slot = block.slot().0,
                block_no = block.block_number().0,
                utxo_count = self.utxo.utxo_set.len(),
                epoch = self.epoch.0,
                "Ledger: Byron block applied successfully"
            );
            // Byron has no Phase-2; nothing to defer.
            return Ok(Vec::new());
        }

        // ══════════════════════════════════════════════════════════════════
        // Shelley+ era block processing via EraRulesImpl
        // ══════════════════════════════════════════════════════════════════

        let rules = EraRulesImpl::for_era(block.era);

        // ── Step 6: Block body validation (ExUnit budgets, ref scripts) ──
        //
        // Dispatched to era rules in ValidateAll mode. Each era checks its own
        // constraints (e.g., Alonzo+ ExUnit budgets, Conway+ ref script size
        // limits). In ApplyOnly mode (historical replay) these checks are
        // skipped — the block was already validated by the producing node.
        if mode == BlockValidationMode::ValidateAll {
            let body_ctx = RuleContext {
                params: &self.epochs.protocol_params,
                current_slot: block.slot().0,
                current_epoch: self.epoch,
                era: block.era,
                slot_config: Some(&self.slot_config),
                node_network: self.node_network,
                genesis_delegates: &self.genesis_delegates,
                update_quorum: self.update_quorum,
                epoch_length: self.epoch_length,
                shelley_transition_epoch: self.shelley_transition_epoch,
                byron_epoch_length: self.byron_epoch_length,
                stability_window: self.randomness_stabilisation_window,
                stability_window_3kf: self.stability_window_3kf,
                randomness_stabilisation_window: self.randomness_stabilisation_window,
                tx_index: 0,
                conway_genesis: self.conway_genesis_init.as_ref(),
                max_lovelace_supply: self.max_lovelace_supply,
            };
            rules.validate_block_body(block, &body_ctx, &self.utxo)?;
        }

        // Pre-compute cost_models CBOR once per block, for the ValidateAll phase-2
        // path only. All consumers (capture_phase2_work_item, the sequential
        // evaluate_plutus_scripts, and the parallel divergence checker) live inside
        // the `if mode == ValidateAll` block / are gated on ValidateAll — in
        // ApplyOnly mode phase-2 is suppressed (SKIP_PHASE2_EVAL), so the value
        // would be unused dead work on the bulk-sync hot path.
        let cost_models_cbor = if mode == BlockValidationMode::ValidateAll {
            self.epochs.protocol_params.cost_models.to_cbor()
        } else {
            None
        };

        // Track processed tx hashes to skip duplicates within a block
        let mut processed_tx_hashes =
            std::collections::HashSet::with_capacity(block.transactions.len());

        // ── Deferred-parallel Phase-2 work items ─────────────────────────
        //
        // When `parallel-verification` is enabled, the per-tx loop resolves
        // Plutus inputs sequentially (ordering-dependent: a tx may spend
        // outputs produced by an earlier tx in the same block) and defers the
        // expensive `eval_phase_two_raw` call to a parallel post-loop pass.
        //
        // Each work item is a self-contained struct (tx CBOR + resolved UTxO
        // pairs + budget params) captured while the UTxO set still reflects
        // the correct apply-point state for that transaction.
        //
        // The `SKIP_PHASE2_EVAL` thread-local is set to `true` before the
        // loop so `validate_transaction_with_pools` suppresses its own phase-2
        // call. Callers outside `apply_block` (mempool, tests) are unaffected.
        // Typically only a small fraction of txs have redeemers, so we start
        // with a zero-allocation Vec and let it grow on demand.
        #[cfg(feature = "parallel-verification")]
        let mut phase2_work_items: Vec<Phase2WorkItem> = Vec::new();
        // Activate the phase-2 skip flag only in ValidateAll mode (the only
        // mode that calls `validate_transaction_with_context`). In ApplyOnly
        // mode, the flag would never be read, but we still set it for safety.
        // The guard holds the previous flag value so we can restore it after
        // the loop — critical for nested calls (e.g. forging path).
        #[cfg(feature = "parallel-verification")]
        let _phase2_skip_guard: bool = if mode == BlockValidationMode::ValidateAll {
            crate::validation::suppress_phase2_eval()
        } else {
            false
        };

        // Cache block-level values for RuleContext construction inside the loop.
        // We cannot call self.build_rule_context() inside the loop because it
        // borrows &self (via &self.epochs.protocol_params) while we also need
        // &mut self.utxo/certs/gov/epochs. Clone protocol_params once per block
        // to break the aliasing conflict.
        let block_slot = block.slot().0;
        let block_era = block.era;
        let cached_params = self.epochs.protocol_params.clone();

        // ── Per-block timing instrumentation (#698) ──────────────────────
        //
        // Enable with `DUGITE_BLOCK_APPLY_TIMING=1`.  Records cumulative
        // time spent in each major step of the per-tx loop and emits one
        // `info!` log line per block summarising the breakdown.  Used to
        // identify which step (validation, era-rule apply, diff merge) is
        // the dominant cost at any given epoch / state size.
        let timing_enabled = std::env::var("DUGITE_BLOCK_APPLY_TIMING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let block_start = if timing_enabled {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let mut t_registry_build = std::time::Duration::ZERO;
        let mut t_validate = std::time::Duration::ZERO;
        let mut t_phase2 = std::time::Duration::ZERO;
        let mut t_apply = std::time::Duration::ZERO;
        let mut t_diff_merge = std::time::Duration::ZERO;
        let mut t_ctx_build = std::time::Duration::ZERO;
        let mut tx_count: usize = 0;
        let mut tx_valid_count: usize = 0;
        let mut tx_input_count: usize = 0;
        let mut tx_output_count: usize = 0;
        let mut tx_witness_count: usize = 0;
        let mut tx_redeemer_count: usize = 0;
        let registry_start = std::time::Instant::now();

        // ── Block-level validation registry snapshot ─────────────────────
        //
        // Phase-1 / Phase-2 validation needs read-only views of several
        // ledger registries (pool params, DRep set, committee state,
        // reward accounts, etc.).  These do NOT change during the tx
        // loop — cert/gov apply is the LAST step per tx (see Step 8b
        // below) and validation reads from the pre-tx snapshot, so we
        // build each registry exactly ONCE per block and share it across
        // every transaction via `Arc::clone` (O(1) refcount bump).
        //
        // Previously these were rebuilt from scratch per-tx (one
        // `.keys().copied().collect()` per registry), and the
        // ValidationContext deep-cloned every entry including the
        // ~90 k-entry `reward_accounts` HashMap — ~400 k hash-table
        // entries copied per block on Babbage/Conway preview.  That
        // memcpy alone dominated block-apply time on a fast machine
        // (100-200 ms / block ≈ ~2 blocks/sec end-to-end).  The
        // hoist + Arc share collapses that to micro-seconds per block.
        #[allow(clippy::type_complexity)]
        let (
            block_registered_pool_ids,
            block_registered_drep_ids,
            block_registered_vrf_keys,
            block_committee_member_keys,
            block_committee_resigned_keys,
            block_vote_delegation_keys,
            // `block_active_proposals` is `mut` so we can update it after each
            // proposal-containing tx.  Haskell's `LEDGER` rule processes txs
            // sequentially with updating state, so a vote in tx[j] on a proposal
            // submitted by tx[i] (i < j) in the same block is valid — after tx[i]
            // applies, the proposal is in `vsProposals` and tx[j] can reference it.
            // Without this update dugite logs spurious `GovActionsDoNotExist`
            // warnings for cross-tx same-block proposal+vote patterns.
            mut block_active_proposals,
            block_committee_authorized_hot_keys,
            block_committee_authorized_elected_hot_keys,
            // Retained for registry-builder signature compatibility; the
            // per-tx ValidationContext now clones the LIVE deposits map
            // (sequential LEDGERS semantics — see the reward-accounts note).
            _block_stake_key_deposits_snap,
            block_constitution_script_hash,
        ): (
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash28>>,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            std::sync::Arc<
                std::collections::HashMap<
                    dugite_primitives::hash::Hash32,
                    dugite_primitives::hash::Hash28,
                >,
            >,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            std::sync::Arc<
                std::collections::HashMap<
                    dugite_primitives::transaction::GovActionId,
                    crate::validation::ActiveProposal,
                >,
            >,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            std::sync::Arc<std::collections::HashSet<dugite_primitives::hash::Hash32>>,
            imbl::HashMap<dugite_primitives::hash::Hash32, u64>,
            Option<dugite_primitives::hash::Hash28>,
        ) = if mode == BlockValidationMode::ValidateAll {
            use std::collections::{HashMap, HashSet};
            use std::sync::Arc;

            // ── Per-source registry cache (bulk-sync apply-ceiling fix) ───────
            //
            // Rebuilding these derived registries from scratch every block was
            // ~51% of apply wall time on preview Conway blocks.  The THREE
            // expensive registries are memoized, each keyed on the structural
            // identity of its OWN source map (see `CachedValidationRegistry`):
            //   • `pools` + `vrf_keys` ← `certs.pool_params`  (Arc::ptr_eq)
            //   • `dreps`              ← `gov.dreps`           (imbl ptr_eq)
            //   • `vote_delegations`   ← `gov.vote_delegations`(imbl ptr_eq)
            // Per-source keying means a vote/proposal block (which bumps the
            // `governance` Arc + `proposals` map but leaves `dreps` /
            // `vote_delegations` structurally identical) still hits both big
            // caches.  The small registries below are cheap and rebuilt fresh.
            //
            // Byte-exact: identical source pointer ⟹ identical contents ⟹
            // identical registry; any mutation copy-on-writes a fresh root.
            let cached = self.cached_validation_registry.take();

            // `pools` + `vrf_keys` ← `certs.pool_params` (Arc ptr-eq).
            let pp_hit = cached
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(&c.pool_params_src, &self.certs.pool_params));
            let (pools, vrf_keys) = if pp_hit {
                let c = cached.as_ref().expect("pp_hit implies Some");
                (Arc::clone(&c.pools), Arc::clone(&c.vrf_keys))
            } else {
                let pools: Arc<HashSet<dugite_primitives::hash::Hash28>> =
                    Arc::new(self.certs.pool_params.keys().copied().collect());
                let vrf_keys: Arc<
                    HashMap<dugite_primitives::hash::Hash32, dugite_primitives::hash::Hash28>,
                > = Arc::new(
                    self.certs
                        .pool_params
                        .values()
                        .map(|reg| (reg.vrf_keyhash, reg.pool_id))
                        .collect(),
                );
                (pools, vrf_keys)
            };

            // `dreps` ← `gov.dreps` (imbl ptr-eq).
            let dreps_hit = cached
                .as_ref()
                .is_some_and(|c| c.dreps_src.ptr_eq(&self.gov.governance.dreps));
            let dreps: Arc<HashSet<dugite_primitives::hash::Hash32>> = if dreps_hit {
                Arc::clone(&cached.as_ref().expect("dreps_hit implies Some").dreps)
            } else {
                Arc::new(self.gov.governance.dreps.keys().copied().collect())
            };

            // `vote_delegations` ← `gov.vote_delegations` (imbl ptr-eq).
            let vd_hit = cached.as_ref().is_some_and(|c| {
                c.vote_delegations_src
                    .ptr_eq(&self.gov.governance.vote_delegations)
            });
            let vote_delegations: Arc<HashSet<dugite_primitives::hash::Hash32>> = if vd_hit {
                Arc::clone(
                    &cached
                        .as_ref()
                        .expect("vd_hit implies Some")
                        .vote_delegations,
                )
            } else {
                Arc::new(
                    self.gov
                        .governance
                        .vote_delegations
                        .keys()
                        .copied()
                        .collect(),
                )
            };

            // ── Small registries — cheap, always rebuilt fresh ────────────────
            // Current members ∪ `members_to_add` of live UpdateCommittee
            // proposals — Haskell GOVCERT accepts a CommitteeHotAuth from a
            // potential FUTURE member too (`isPotentialFutureMember`).
            let committee_members: Arc<HashSet<dugite_primitives::hash::Hash32>> =
                Arc::new(self.gov.governance.committee_auth_eligible_members());
            let committee_resigned: Arc<HashSet<dugite_primitives::hash::Hash32>> = Arc::new(
                self.gov
                    .governance
                    .committee_resigned
                    .keys()
                    .copied()
                    .collect(),
            );
            let active_proposals: Arc<
                HashMap<
                    dugite_primitives::transaction::GovActionId,
                    crate::validation::ActiveProposal,
                >,
            > = Arc::new(
                self.gov
                    .governance
                    .proposals
                    .iter()
                    .map(|(id, state)| {
                        (
                            id.clone(),
                            crate::validation::ActiveProposal {
                                gov_action: state.procedure.gov_action.clone(),
                                return_addr: state.procedure.return_addr.clone(),
                                deposit: state.procedure.deposit,
                                expires_after_epoch: state.expires_epoch,
                                proposed_in_epoch: state.proposed_epoch,
                            },
                        )
                    })
                    .collect(),
            );
            let committee_authorized_hot_keys: Arc<HashSet<dugite_primitives::hash::Hash32>> =
                Arc::new(
                    self.gov
                        .governance
                        .committee_hot_keys
                        .values()
                        .copied()
                        .collect(),
                );
            let committee_authorized_elected_hot_keys: Arc<
                HashSet<dugite_primitives::hash::Hash32>,
            > = Arc::new(
                self.gov
                    .governance
                    .committee_hot_keys
                    .iter()
                    .filter(|(cold, _)| {
                        self.gov.governance.committee_expiration.contains_key(*cold)
                    })
                    .map(|(_, hot)| *hot)
                    .collect(),
            );
            let constitution_script_hash = self
                .gov
                .governance
                .constitution
                .as_ref()
                .and_then(|c| c.script_hash);

            // Refresh the cache with the (possibly rebuilt) large registries and
            // their current source keys.  RHS reads only `&self` immutably and
            // local Arcs; the assignment target is a disjoint field.
            self.cached_validation_registry = Some(crate::state::CachedValidationRegistry {
                pool_params_src: Arc::clone(&self.certs.pool_params),
                pools: Arc::clone(&pools),
                vrf_keys: Arc::clone(&vrf_keys),
                dreps_src: self.gov.governance.dreps.clone(),
                dreps: Arc::clone(&dreps),
                vote_delegations_src: self.gov.governance.vote_delegations.clone(),
                vote_delegations: Arc::clone(&vote_delegations),
            });

            (
                pools,
                dreps,
                vrf_keys,
                committee_members,
                committee_resigned,
                vote_delegations,
                active_proposals,
                committee_authorized_hot_keys,
                committee_authorized_elected_hot_keys,
                // O(1) imbl structural clone — pre-block snapshot for stake_key_deposits validation.
                self.certs.stake_key_deposits.clone(),
                constitution_script_hash,
            )
        } else {
            // ApplyOnly path doesn't enter the per-tx validation arm — we
            // still need to satisfy the type, so populate with cheap empties.
            use std::collections::{HashMap, HashSet};
            use std::sync::Arc;
            (
                Arc::new(HashSet::new()),
                Arc::new(HashSet::new()),
                Arc::new(HashMap::new()),
                Arc::new(HashSet::new()),
                Arc::new(HashSet::new()),
                Arc::new(HashSet::new()),
                Arc::new(HashMap::new()),
                Arc::new(HashSet::new()),
                Arc::new(HashSet::new()),
                imbl::HashMap::new(), // empty imbl::HashMap for stake_key_deposits
                None,
            )
        };
        // NOTE: the per-tx ValidationContext below takes a FRESH O(1) imbl
        // clone of `self.certs.reward_accounts` for every transaction —
        // Haskell's LEDGERS rule applies txs SEQUENTIALLY, so tx N+1 must
        // validate against the post-tx-N state. A per-BLOCK snapshot here
        // false-positived `StakeKeyHasNonZeroBalance` on the common
        // withdraw-then-deregister same-block chain (observed: 39 spurious
        // divergence warnings during the mainnet ep345-351 sync while the
        // pots stayed byte-exact and the accounts were correctly drained).

        if timing_enabled {
            t_registry_build = registry_start.elapsed();
        }

        // ── Step 8: Per-transaction processing loop ───────────────────────
        for (tx_idx, tx) in block.transactions.iter().enumerate() {
            if !processed_tx_hashes.insert(tx.hash) {
                warn!(
                    tx_hash = %tx.hash.to_hex(),
                    slot = block.slot().0,
                    "Duplicate transaction hash in block, skipping"
                );
                continue;
            }
            if timing_enabled {
                tx_count += 1;
                if tx.is_valid {
                    tx_valid_count += 1;
                }
                tx_input_count += tx.body.inputs.len();
                tx_output_count += tx.body.outputs.len();
                tx_witness_count +=
                    tx.witness_set.vkey_witnesses.len() + tx.witness_set.bootstrap_witnesses.len();
                tx_redeemer_count += tx.witness_set.redeemers.len();
            }

            // ── Step 8a: Phase-1 + Phase-2 validation (ValidateAll only) ──
            if mode == BlockValidationMode::ValidateAll {
                // Conway per-tx ref script size limit
                if self.epochs.protocol_params.protocol_version_major >= 9 && tx.is_valid {
                    let tx_ref_script_size = calculate_ref_script_size(
                        &tx.body.inputs,
                        &tx.body.reference_inputs,
                        &self.utxo.utxo_set,
                    );
                    if tx_ref_script_size > MAX_REF_SCRIPT_SIZE_PER_TX {
                        return Err(LedgerError::BlockTxValidationFailed {
                            slot: block.slot().0,
                            tx_hash: tx.hash.to_hex(),
                            errors: format!(
                                "TxRefScriptSizeTooLarge: reference script size {} exceeds \
                                 per-transaction limit {} bytes \
                                 (Conway ppMaxRefScriptSizePerTxG)",
                                tx_ref_script_size, MAX_REF_SCRIPT_SIZE_PER_TX
                            ),
                        });
                    }
                }

                let has_redeemers = !tx.witness_set.redeemers.is_empty();

                if tx.is_valid {
                    // Conway LEDGERS: treasury value check.
                    //
                    // Haskell `Cardano.Ledger.Conway.Rules.Ledger.hs:364,442` checks
                    // `declared == actual` where `actual = ChainAccountState.casTreasury`
                    // (frozen at block entry, constant intra-block — every tx in the
                    // block compares against the SAME pre-block treasury value).
                    //
                    // Issue #678: the previous self-correction
                    // (`self.epochs.treasury = declared_treasury`) silently masked
                    // upstream treasury-calculation drift during live sync. Removing
                    // the snap exposes the underlying divergence — the
                    // MismatchedTreasuryValue WARN now points at a real per-epoch
                    // accounting bug that must be fixed in the treasury update path,
                    // not papered over here. Same masking-removed pattern as
                    // #438 / #481 / #624 / #626.
                    if self.epochs.protocol_params.protocol_version_major >= 9 {
                        if let Some(declared_treasury) = tx.body.treasury_value {
                            if declared_treasury.0 != self.epochs.treasury.0 {
                                let delta =
                                    declared_treasury.0.saturating_sub(self.epochs.treasury.0)
                                        as i128
                                        - self.epochs.treasury.0.saturating_sub(declared_treasury.0)
                                            as i128;
                                warn!(
                                    tx_hash = %tx.hash.to_hex(),
                                    slot = block.slot().0,
                                    declared = declared_treasury.0,
                                    actual = self.epochs.treasury.0,
                                    delta_lovelace = delta,
                                    "TreasuryValueMismatch — dugite's treasury differs \
                                     from on-chain declared value (issue #678). \
                                     The self-correction was removed; treasury \
                                     accumulates over epoch boundaries from this point."
                                );
                            }
                        }
                    }

                    // Conway LEDGERS: unelected committee member check.
                    //
                    // Per Haskell GOVCERT (`checkAndOverwriteCommitteeMemberState`)
                    // the cold credential must be a CURRENT committee member OR a
                    // potential FUTURE member (named in `members_to_add` of a live
                    // UpdateCommittee proposal) — `isCurrentMember ||
                    // isPotentialFutureMember`.
                    //
                    // Aggregate one WARN per tx, not one per cert: a single
                    // CommitteeHotAuth tx can carry 30+ certs; per-cert logging
                    // produced a 186K-line / 1GB+ log over a single Conway sync
                    // (#22 follow-up).
                    if self.epochs.protocol_params.protocol_version_major >= 9 {
                        let mut unelected: Vec<String> = Vec::new();
                        let mut eligible: Option<
                            std::collections::HashSet<dugite_primitives::hash::Hash32>,
                        > = None;
                        for cert in &tx.body.certificates {
                            if let Certificate::CommitteeHotAuth {
                                cold_credential, ..
                            } = cert
                            {
                                let cold_key = credential_to_hash(cold_credential);
                                let eligible = eligible.get_or_insert_with(|| {
                                    self.gov.governance.committee_auth_eligible_members()
                                });
                                if !eligible.contains(&cold_key) {
                                    unelected.push(cold_key.to_hex());
                                }
                            }
                        }
                        if !unelected.is_empty() {
                            warn!(
                                tx_hash = %tx.hash.to_hex(),
                                slot = block.slot().0,
                                count = unelected.len(),
                                first_cold_key = %unelected[0],
                                "CommitteeHotAuth for cold credential(s) neither in the \
                                 current committee nor in any live UpdateCommittee \
                                 proposal — committee state may be stale; trusting \
                                 on-chain consensus"
                            );
                        }
                    }

                    // Full Phase-1 + Phase-2 validation
                    //
                    // ── imbl persistent-map apply path (#698 Task C) ─────────
                    //
                    // `self.certs.reward_accounts` and `self.certs.stake_key_deposits`
                    // are now `imbl::HashMap` (persistent HAMT), so apply-time
                    // mutations (`drain_withdrawal_accounts`, `apply_shelley_cert`)
                    // are O(log N) structural updates — no deep-clone ever fires.
                    // The per-block snapshot Arcs (`block_reward_accounts_arc`,
                    // `block_stake_key_deposits_arc`) are independent std::HashMaps
                    // built once at registry-build time (O(N) before this loop) and
                    // hold the pre-block state for validation.  The live imbl maps
                    // accumulate mutations independently with zero CoW overhead.
                    //
                    // The `block_reward_accounts_arc` snapshot correctly reflects
                    // the PRE-BLOCK state for all txs (Haskell LEDGER: validates
                    // against pre-tx state; the withdrawal-drain predicate uses the
                    // pre-block balance, consistent with Haskell's sequential
                    // drainAccounts application).
                    let tx_size = tx.raw_cbor.as_ref().map_or(0, |c| c.len() as u64);
                    // All ValidationContext registries are pre-built once per
                    // block above (see "Block-level validation registry
                    // snapshot").  Per-tx construction is just `Arc::clone`
                    // refcount bumps — no hash-table allocation or copying.
                    let ctx_start = if timing_enabled {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                    let mut ctx = crate::validation::ValidationContext::new()
                        .with_active_proposals_arc(std::sync::Arc::clone(&block_active_proposals))
                        .with_committee_authorized_hot_keys_arc(std::sync::Arc::clone(
                            &block_committee_authorized_hot_keys,
                        ))
                        .with_committee_authorized_elected_hot_keys_arc(std::sync::Arc::clone(
                            &block_committee_authorized_elected_hot_keys,
                        ))
                        .with_pools_arc(std::sync::Arc::clone(&block_registered_pool_ids))
                        .with_dreps_arc(std::sync::Arc::clone(&block_registered_drep_ids))
                        .with_vrf_keys_arc(std::sync::Arc::clone(&block_registered_vrf_keys))
                        .with_committee_members_arc(std::sync::Arc::clone(
                            &block_committee_member_keys,
                        ))
                        .with_committee_resigned_arc(std::sync::Arc::clone(
                            &block_committee_resigned_keys,
                        ))
                        .with_treasury(self.epochs.treasury.0)
                        // O(1) imbl clone of the LIVE map at THIS tx's apply
                        // point — Haskell LEDGERS applies txs sequentially,
                        // so a same-block withdraw→deregister chain must see
                        // the post-withdrawal balance (a pre-block snapshot
                        // false-positived StakeKeyHasNonZeroBalance here).
                        .with_reward_accounts_imbl(self.certs.reward_accounts.clone())
                        .with_epoch(self.epoch.0)
                        .with_stake_key_deposits_imbl(self.certs.stake_key_deposits.clone())
                        .with_vote_delegations_arc(std::sync::Arc::clone(
                            &block_vote_delegation_keys,
                        ));
                    if let Some(net) = self.node_network {
                        ctx = ctx.with_network(net);
                    }
                    if let Some(h) = block_constitution_script_hash {
                        ctx = ctx.with_constitution_script_hash(h);
                    }
                    if let Some(start) = ctx_start {
                        t_ctx_build += start.elapsed();
                    }
                    let validate_start = if timing_enabled {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                    // ctx is consumed (and all per-tx Arc clones dropped) here:
                    // `phase2_slot_config` carries the #733 apply horizon —
                    // inert in parallel builds (phase-2 deferred), live for
                    // the sequential fallback.
                    let result = crate::validation::validate_transaction_with_context(
                        tx,
                        &self.utxo.utxo_set,
                        &self.epochs.protocol_params,
                        block.slot().0,
                        tx_size,
                        Some(&phase2_slot_config),
                        ctx,
                    );
                    if let Some(start) = validate_start {
                        t_validate += start.elapsed();
                    }
                    if let Err(errors) = result {
                        let has_script_failure = errors
                            .iter()
                            .any(|e| matches!(e, ValidationError::ScriptFailed(_)));
                        if has_script_failure {
                            warn!(
                                tx_hash = %tx.hash.to_hex(),
                                slot = block.slot().0,
                                errors = ?errors.iter().filter(|e| matches!(e, ValidationError::ScriptFailed(_)))
                                    .map(|e| e.to_string()).collect::<Vec<_>>(),
                                "Plutus evaluation divergence: uplc says scripts fail but block is_valid=true on-chain — \
                                 trusting on-chain consensus (likely marginal budget difference)"
                            );
                        } else {
                            let is_utxo_gap_only = errors.iter().all(|e| {
                                matches!(
                                    e,
                                    ValidationError::InputNotFound(_)
                                        | ValidationError::CollateralNotFound(_)
                                        | ValidationError::CollateralMismatch { .. }
                                        | ValidationError::InsufficientCollateral
                                        | ValidationError::ValueNotConserved { .. }
                                        | ValidationError::MultiAssetNotConserved { .. }
                                )
                            });
                            if is_utxo_gap_only {
                                let err_str: Vec<String> =
                                    errors.iter().map(|e| e.to_string()).collect();
                                debug!(
                                    tx_hash = %tx.hash.to_hex(),
                                    slot = block.slot().0,
                                    errors = %err_str.join("; "),
                                    "Phase-1 UTxO-gap errors on confirmed block (inputs not yet \
                                     in store due to partial replay) — outputs will still be \
                                     inserted by best-effort apply"
                                );
                            } else {
                                // #733 (sequential builds): a phase-2
                                // collection error from the inline eval is
                                // block-fatal in Babbage+ unless any input
                                // failed to resolve (UTxO-gap carve-out).
                                // Parallel builds surface this via the
                                // Step 8d outcomes instead (phase-2 was
                                // skipped here).
                                let has_collect = errors
                                    .iter()
                                    .any(|e| matches!(e, ValidationError::Phase2CollectError(_)));
                                let has_utxo_gap = errors.iter().any(|e| {
                                    matches!(
                                        e,
                                        ValidationError::InputNotFound(_)
                                            | ValidationError::CollateralNotFound(_)
                                            | ValidationError::ReferenceInputNotFound(_)
                                    )
                                });
                                if has_collect
                                    && !has_utxo_gap
                                    && block.era >= Era::Babbage
                                    && phase2_collect_fatal_enabled()
                                {
                                    let err_str: Vec<String> =
                                        errors.iter().map(|e| e.to_string()).collect();
                                    return Err(LedgerError::Phase2CollectErrors {
                                        slot: block.slot().0,
                                        tx_hash: tx.hash.to_hex(),
                                        error: err_str.join("; "),
                                    });
                                }
                                let err_str: Vec<String> =
                                    errors.iter().map(|e| e.to_string()).collect();
                                warn!(
                                    tx_hash = %tx.hash.to_hex(),
                                    slot = block.slot().0,
                                    errors = %err_str.join("; "),
                                    "Phase-1 validation divergence on confirmed block — \
                                     trusting on-chain consensus"
                                );
                            }
                        }
                    }
                    // ValidationContext consumed by validate_transaction_with_context.
                    // `block_reward_accounts_snap` / `block_stake_key_deposits_snap`
                    // are O(1) imbl structural clones — independent from the live maps.
                    // Apply-time mutations on `self.certs.reward_accounts` etc. are
                    // O(log N) with no CoW deep-clone, and don't affect the snapshots.
                    //
                    // ── Deferred Phase-2 work item capture ───────────────────
                    //
                    // When `parallel-verification` is enabled, `validate_transaction_with_context`
                    // ran with the SKIP_PHASE2_EVAL flag set, so phase-2 was suppressed
                    // above. Capture the work item now — while the UTxO set still
                    // reflects the apply-point state for this transaction. The actual
                    // `eval_phase_two_raw` call runs in the parallel pass after the loop.
                    #[cfg(feature = "parallel-verification")]
                    if has_redeemers {
                        let max_ex = (
                            self.epochs.protocol_params.max_tx_ex_units.steps,
                            self.epochs.protocol_params.max_tx_ex_units.mem,
                        );
                        phase2_work_items.push(capture_phase2_work_item(
                            tx_idx,
                            tx,
                            &self.utxo.utxo_set,
                            cost_models_cbor.clone(),
                            max_ex,
                            phase2_slot_config,
                            self.epochs.protocol_params.protocol_version_major as u32,
                        ));
                    }
                } else if has_redeemers {
                    // Producer claims tx is invalid with scripts present.
                    // Verify scripts actually fail; if they pass, producer is stealing collateral.
                    //
                    // ── Parallel path: defer eval, capture work item ──────────
                    #[cfg(feature = "parallel-verification")]
                    {
                        let max_ex = (
                            self.epochs.protocol_params.max_tx_ex_units.steps,
                            self.epochs.protocol_params.max_tx_ex_units.mem,
                        );
                        phase2_work_items.push(capture_phase2_work_item(
                            tx_idx,
                            tx,
                            &self.utxo.utxo_set,
                            cost_models_cbor.clone(),
                            max_ex,
                            phase2_slot_config,
                            self.epochs.protocol_params.protocol_version_major as u32,
                        ));
                    }
                    // ── Sequential fallback (feature disabled) ────────────────
                    #[cfg(not(feature = "parallel-verification"))]
                    {
                        let max_ex = (
                            self.epochs.protocol_params.max_tx_ex_units.steps,
                            self.epochs.protocol_params.max_tx_ex_units.mem,
                        );
                        let eval_result = evaluate_plutus_scripts(
                            tx,
                            &self.utxo.utxo_set,
                            cost_models_cbor.as_deref(),
                            max_ex,
                            &phase2_slot_config,
                            self.epochs.protocol_params.protocol_version_major as u32,
                        );
                        match &eval_result {
                            Ok(()) => {
                                // Phase-2 divergence on a confirmed block: the
                                // on-chain is_valid=false flag is consensus truth
                                // (see the parallel branch in Step 8d for the full
                                // rationale). Trust it, log, and fall through to
                                // Step 8b which applies the tx as invalid — keeping
                                // the ledger state byte-exact — instead of halting
                                // the sync.
                                warn!(
                                    tx_hash = %tx.hash.to_hex(),
                                    slot = block.slot().0,
                                    "Plutus evaluation divergence: uplc says scripts PASS but block \
                                     is_valid=false on-chain — trusting on-chain consensus (tx applied \
                                     as invalid; dugite CEK over-permissive)"
                                );
                            }
                            Err(e)
                                if e.is_collect_error()
                                    && block.era >= Era::Babbage
                                    && phase2_collect_fatal_enabled() =>
                            {
                                // #733: CollectErrors reject the block
                                // regardless of the is_valid tag — Haskell
                                // never reaches script evaluation. UTxO-gap
                                // carve-out: only fatal when every input
                                // resolved.
                                let attempted = tx.body.inputs.len()
                                    + tx.body.reference_inputs.len()
                                    + tx.body.collateral.len();
                                let resolved = crate::plutus::resolve_phase2_utxo_pairs(
                                    tx,
                                    &self.utxo.utxo_set,
                                )
                                .len();
                                if resolved == attempted {
                                    return Err(LedgerError::Phase2CollectErrors {
                                        slot: block.slot().0,
                                        tx_hash: tx.hash.to_hex(),
                                        error: e.to_string(),
                                    });
                                }
                            }
                            Err(_) => {
                                // Genuine script failure (legitimate
                                // is_valid=false path) or CEK panic
                                // (warn-and-trust at apply, #733
                                // correction 3) — collateral consumption
                                // proceeds in Step 8b.
                            }
                        }
                    }
                }
            }

            // ── Step 8b: Apply transaction via era rules ──────────────────
            //
            // Build the RuleContext inline to avoid borrowing &self while also
            // needing &mut self.utxo/certs/gov/epochs. The context references
            // only immutable fields or fields we snapshot before mutation.
            let ctx = RuleContext {
                params: &cached_params,
                current_slot: block_slot,
                current_epoch: self.epoch,
                era: block_era,
                slot_config: Some(&self.slot_config),
                node_network: self.node_network,
                genesis_delegates: &self.genesis_delegates,
                update_quorum: self.update_quorum,
                epoch_length: self.epoch_length,
                shelley_transition_epoch: self.shelley_transition_epoch,
                byron_epoch_length: self.byron_epoch_length,
                stability_window: self.randomness_stabilisation_window,
                stability_window_3kf: self.stability_window_3kf,
                randomness_stabilisation_window: self.randomness_stabilisation_window,
                tx_index: tx_idx as u64,
                conway_genesis: self.conway_genesis_init.as_ref(),
                max_lovelace_supply: self.max_lovelace_supply,
            };

            let apply_start = if timing_enabled {
                Some(std::time::Instant::now())
            } else {
                None
            };
            if !tx.is_valid {
                // Invalid transaction: consume collateral via era rules.
                let diff = rules.apply_invalid_tx(
                    tx,
                    mode,
                    &ctx,
                    &mut self.utxo,
                    &mut self.certs,
                    &mut self.epochs,
                )?;
                if let Some(start) = apply_start {
                    t_apply += start.elapsed();
                }
                let merge_start = if timing_enabled {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                block_diff.merge(&diff);
                if let Some(start) = merge_start {
                    t_diff_merge += start.elapsed();
                }
            } else {
                // Valid transaction: full LEDGER rule pipeline via era rules.
                // The era rules handle: drain withdrawals, process certificates,
                // apply UTxO changes (consume inputs, produce outputs, accumulate fee),
                // and Conway-specific governance (votes, proposals, donations).
                let diff = rules.apply_valid_tx(
                    tx,
                    mode,
                    &ctx,
                    &mut self.utxo,
                    &mut self.certs,
                    &mut self.gov,
                    &mut self.epochs,
                )?;
                if let Some(start) = apply_start {
                    t_apply += start.elapsed();
                }
                let merge_start = if timing_enabled {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                block_diff.merge(&diff);
                if let Some(start) = merge_start {
                    t_diff_merge += start.elapsed();
                }

                // ── Step 8b-post: Update block_active_proposals ───────────
                //
                // After a proposal-containing tx is applied, `self.gov.governance.proposals`
                // now includes the new proposal(s).  Update `block_active_proposals` so
                // that a subsequent tx in the same block that votes on this proposal can
                // resolve it — mirroring Haskell's sequential `LEDGER` rule state updates.
                //
                // This is the cross-tx same-block case: tx[i] proposes, tx[j] (j>i) votes.
                // Haskell accepts because by the time tx[j] is processed, `vsProposals`
                // already contains tx[i]'s proposal.  Without this update dugite logs
                // a spurious `GovActionsDoNotExist` warning for tx[j] and trusts on-chain
                // consensus (which is correct, but the warning is misleading).
                //
                // Only rebuild when the tx actually has proposals; avoid the
                // `Arc::unwrap_or_clone` cost for the common case (no proposals).
                if mode == BlockValidationMode::ValidateAll
                    && !tx.body.proposal_procedures.is_empty()
                {
                    let mut updated_proposals =
                        std::sync::Arc::unwrap_or_clone(block_active_proposals);
                    for (idx, _) in tx.body.proposal_procedures.iter().enumerate() {
                        let id = dugite_primitives::transaction::GovActionId {
                            transaction_id: tx.hash,
                            action_index: idx as u32,
                        };
                        if let Some(state) = self.gov.governance.proposals.get(&id) {
                            updated_proposals.insert(
                                id,
                                crate::validation::ActiveProposal {
                                    gov_action: state.procedure.gov_action.clone(),
                                    return_addr: state.procedure.return_addr.clone(),
                                    deposit: state.procedure.deposit,
                                    expires_after_epoch: state.expires_epoch,
                                    proposed_in_epoch: state.proposed_epoch,
                                },
                            );
                        }
                    }
                    block_active_proposals = std::sync::Arc::new(updated_proposals);
                }

                // ── Step 8c: Pre-Conway PP update proposals ───────────────
                //
                // These are NOT part of the era rules because they operate on the
                // epoch sub-state's pending/future maps and are only relevant for
                // Shelley through Babbage (pre-governance PP updates).
                if let Some(ref update) = tx.body.update {
                    let is_future = update.epoch > self.epoch.0;
                    for (genesis_hash, ppu) in &update.proposed_updates {
                        debug!(
                            genesis_hash = %genesis_hash.to_hex(),
                            target_epoch = update.epoch,
                            current_epoch = self.epoch.0,
                            kind = if is_future { "future" } else { "current" },
                            protocol_version = ?ppu.protocol_version_major.zip(ppu.protocol_version_minor),
                            d = ?ppu.d,
                            n_opt = ?ppu.n_opt,
                            "Collected protocol parameter update proposal"
                        );
                        if is_future {
                            self.epochs
                                .future_pp_updates
                                .entry(EpochNo(update.epoch))
                                .or_default()
                                .push((*genesis_hash, ppu.clone()));
                        } else {
                            self.epochs
                                .pending_pp_updates
                                .entry(EpochNo(update.epoch))
                                .or_default()
                                .push((*genesis_hash, ppu.clone()));
                        }
                    }
                }
            }
        }

        // ── Step 8d: Parallel Phase-2 evaluation ─────────────────────────
        //
        // Restore the SKIP_PHASE2_EVAL flag and run all deferred Plutus evals
        // in parallel via rayon. Results are collected in tx_idx order (rayon
        // `par_iter` + `sort_by_key` in `run_phase2_parallel`).
        //
        // Error semantics (per-result):
        //   is_valid=true  → ScriptFailed warning (trusting on-chain consensus)
        //   is_valid=false → ValidationTagMismatch → early-return Err (fatal)
        //
        // This mirrors the sequential path exactly: same checks, same error
        // precedence, same result per transaction. The fingerprint is unaffected
        // because ledger state is determined by the UTxO apply (Step 8b), which
        // already ran sequentially above.
        #[cfg(feature = "parallel-verification")]
        {
            crate::validation::restore_phase2_eval(_phase2_skip_guard);
            // When deferring (bulk-sync pooling), DO NOT drain here — the
            // captured `phase2_work_items` are returned to the caller, which
            // pools them across blocks, evaluates on a rayon pool, and runs
            // `apply_phase2_outcomes` to reproduce the exact fatality decision
            // before exposing the block. The inline path drains immediately so
            // its behaviour (and the apply_bench fingerprint) is unchanged.
            if !defer_phase2
                && mode == BlockValidationMode::ValidateAll
                && !phase2_work_items.is_empty()
            {
                let t_phase2_start = if timing_enabled {
                    Some(std::time::Instant::now())
                } else {
                    None
                };
                let outcomes = run_phase2_parallel(std::mem::take(&mut phase2_work_items));
                if let Some(start) = t_phase2_start {
                    t_phase2 += start.elapsed();
                }
                apply_phase2_outcomes(block, outcomes)?;
            }
        }

        // ── Step 9: Nonce evolution and block production tracking ─────────
        //
        // Dispatched to era rules via `evolve_nonce`. Each era's implementation
        // handles: evolving nonce update (VRF-based), candidate nonce freeze
        // (stability window), lab nonce = prevHashToNonce, and block production
        // counting (incrBlocks with d-parameter gating).
        {
            let nonce_ctx = RuleContext {
                params: &self.epochs.protocol_params,
                current_slot: block.slot().0,
                current_epoch: self.epoch,
                era: block.era,
                slot_config: Some(&self.slot_config),
                node_network: self.node_network,
                genesis_delegates: &self.genesis_delegates,
                update_quorum: self.update_quorum,
                epoch_length: self.epoch_length,
                shelley_transition_epoch: self.shelley_transition_epoch,
                byron_epoch_length: self.byron_epoch_length,
                stability_window: self.randomness_stabilisation_window,
                stability_window_3kf: self.stability_window_3kf,
                randomness_stabilisation_window: self.randomness_stabilisation_window,
                tx_index: 0,
                conway_genesis: self.conway_genesis_init.as_ref(),
                max_lovelace_supply: self.max_lovelace_supply,
            };
            rules.evolve_nonce(&block.header, &nonce_ctx, &mut self.consensus);
        }

        // ── Step 10: Update tip and era ──────────────────────────────────
        self.tip = block.tip();
        self.era = block.era;

        // Record this block's UTxO diff for rollback support.
        //
        // Use `push_bounded` with `security_param` (k) as the cap, exactly as
        // the Byron path does (line ~446 above).  The plain unbounded `push` was
        // the root cause of the ~42 GB from-genesis replay OOM: across 6.8 M
        // Shelley+ blocks the DiffSeq VecDeque grew without bound — ~4 KB per
        // block × 6.8 M ≈ 27 GB in the seq alone, on top of the LSM UTxO store.
        // Immutable blocks cannot be rolled back, so retaining more than k diffs
        // is waste; a k-bounded window is exactly what Haskell's `DiffSeq`
        // semantics require (see ouroboros-consensus `LedgerDB.V2`).
        //
        // Byte-exact safety: `push_bounded` only controls how many diffs are
        // *retained* after the push; the state mutation itself (UTxO inserts /
        // deletes, nonces, certs, etc.) already happened above and is unaffected.
        // Rollback via `DiffSeq` only works within the k-window anyway, and the
        // `LedgerSeq` layer (which also operates over k deltas) is the primary
        // rollback mechanism.  Evicting the oldest diff merely stops us from
        // being able to undo more than k blocks via the fast UTxO-diff path —
        // which matches the Haskell invariant.
        self.utxo.diff_seq.push_bounded(
            block.slot(),
            *block.hash(),
            block_diff,
            self.security_param as usize,
        );

        trace!(
            slot = block.slot().0,
            block_no = block.block_number().0,
            utxo_count = self.utxo.utxo_set.len(),
            epoch = self.epoch.0,
            era = ?self.era,
            "Ledger: block applied successfully"
        );

        if let Some(start) = block_start {
            let total = start.elapsed();
            let accounted =
                t_registry_build + t_ctx_build + t_validate + t_phase2 + t_apply + t_diff_merge;
            let other = total.saturating_sub(accounted);
            tracing::info!(
                target: "dugite_ledger::apply::timing",
                slot = block.slot().0,
                block_no = block.block_number().0,
                era = ?self.era,
                txs = tx_count,
                valid_txs = tx_valid_count,
                inputs = tx_input_count,
                outputs = tx_output_count,
                witnesses = tx_witness_count,
                redeemers = tx_redeemer_count,
                total_us = total.as_micros() as u64,
                registry_us = t_registry_build.as_micros() as u64,
                ctx_build_us = t_ctx_build.as_micros() as u64,
                validate_us = t_validate.as_micros() as u64,
                phase2_us = t_phase2.as_micros() as u64,
                apply_us = t_apply.as_micros() as u64,
                merge_us = t_diff_merge.as_micros() as u64,
                other_us = other.as_micros() as u64,
                utxo_count = self.utxo.utxo_set.len(),
                "block apply timing"
            );
        }

        // In deferred mode return the captured Phase-2 work items (possibly
        // empty for ApplyOnly / Byron / script-free blocks); the inline path
        // already drained them above and returns an empty vec.
        Ok(if defer_phase2 {
            std::mem::take(&mut phase2_work_items)
        } else {
            Vec::new()
        })
    }

    /// Apply a block and produce a [`LedgerDelta`] capturing all state changes.
    ///
    /// Performs the exact same state mutations as [`apply_block`], and additionally
    /// returns a `LedgerDelta` recording every change. The delta is used by
    /// `LedgerSeq` for O(1) rollback and O(checkpoint_interval) state reconstruction.
    ///
    /// # Implementation
    ///
    /// Delegates to `apply_block()` for all state mutations, then extracts the
    /// UTxO diff from the DiffSeq and builds `BlockFieldsDelta` from post-block state.
    /// Epoch transition deltas capture absolute post-transition values.
    pub fn apply_block_with_delta(
        &mut self,
        block: &Block,
        mode: BlockValidationMode,
    ) -> Result<LedgerDelta, LedgerError> {
        let mut delta = LedgerDelta::new(block.slot(), *block.hash(), block.block_number());

        // Snapshot pre-block epoch to detect epoch transitions.
        let pre_epoch = self.epoch;
        // Capture the pre-block governance Arc so we can detect (by pointer)
        // whether this block mutated governance at all.
        let gov_before = std::sync::Arc::clone(&self.gov.governance);
        // Capture the pre-block pool_params Arc pointer so we can detect
        // whether this block mutated pool state (PoolRegistration / PoolRetirement
        // certs). `pool_params` is mutated via `Arc::make_mut` on first-time pool
        // registration — that CoW breaks pointer equality, giving us a cheap
        // "did pool state change?" signal without scanning block contents.
        let pool_params_before = std::sync::Arc::clone(&self.certs.pool_params);
        // Also capture pre-block sizes of the plain-HashMap pool fields so we can
        // detect changes that don't go through pool_params (e.g. retirement-only
        // blocks where a pending retirement is added but pool_params is unchanged).
        let pending_retirements_len_before = self.certs.pending_retirements.len();
        let future_pool_params_len_before = self.certs.future_pool_params.len();

        // Apply the block (all state mutations happen here).
        self.apply_block(block, mode)?;

        // Capture the post-block `imbl` cert maps so state reconstruction
        // (rollback_via_seq / state_at_index / anchor advance) restores them
        // exactly instead of inheriting the stale anchor value. These are
        // mutated in place by `apply_block` and are not represented by the
        // `*_changes` delta vecs. `imbl::HashMap` clone is O(1). This fixes the
        // fork-induced reward-account corruption behind the preprod ep292 halt.
        delta.reward_accounts_snapshot = Some(self.certs.reward_accounts.clone());
        delta.delegations_snapshot = Some(self.certs.delegations.clone());
        delta.stake_key_deposits_snapshot = Some(self.certs.stake_key_deposits.clone());

        // Snapshot pool state when this block mutated it (pool_params Arc pointer
        // changed = first-time registration; or plain-map sizes changed = retirement
        // or re-registration). `pool_params` Arc::clone is O(1); the plain HashMaps
        // hold at most a few entries per block. On reconstruction,
        // `apply_delta_to_state` restores these instead of inheriting the stale
        // anchor value — fixes fork rollback corrupting pool registrations.
        let pool_state_changed =
            !std::sync::Arc::ptr_eq(&pool_params_before, &self.certs.pool_params)
                || self.certs.pending_retirements.len() != pending_retirements_len_before
                || self.certs.future_pool_params.len() != future_pool_params_len_before;
        if pool_state_changed {
            delta.pool_params_snapshot = Some(std::sync::Arc::clone(&self.certs.pool_params));
            delta.future_pool_params_snapshot = Some(self.certs.future_pool_params.clone());
            delta.pending_retirements_snapshot = Some(self.certs.pending_retirements.clone());
            delta.pool_deposits_snapshot = Some(self.certs.pool_deposits.clone());
        }

        // Snapshot gov ONLY when this block actually mutated it. gov is
        // `Arc<GovernanceState>` backed by std HashMaps (not imbl), so an
        // `Arc::make_mut` after a retained snapshot deep-clones the whole
        // GovernanceState; capturing it unconditionally in every one of the
        // k=2160 retained deltas blew memory up at deep Conway (OOM). Most
        // blocks don't touch governance, so `gov_before` and the post-block Arc
        // are pointer-equal → no snapshot, no cost. On reconstruction,
        // `apply_delta_to_state` leaves gov untouched for `None` deltas, so it
        // is carried forward from the most recent delta that DID change it (or
        // the anchor) — which is exactly the gov state at that point. Captures
        // DReps, vote delegations, proposals, votes and enacted roots so a fork
        // rollback restores them instead of the stale anchor governance (which
        // left DRep power at 0 → ParameterChanges never ratified → V3 cost
        // model frozen → script_data_hash divergence + deposits_proposal=0).
        if !std::sync::Arc::ptr_eq(&gov_before, &self.gov.governance) {
            delta.gov_snapshot = Some(self.gov.clone());
        }

        // Extract the UTxO diff from the DiffSeq entry that apply_block just pushed.
        if let Some((_slot, _hash, utxo_diff)) = self.utxo.diff_seq.diffs.back() {
            delta.utxo_diff = utxo_diff.clone();
        }

        // Capture epoch transition delta if an epoch boundary was crossed.
        if self.epoch > pre_epoch {
            delta.epoch_transition = Some(crate::ledger_seq::EpochTransitionDelta {
                new_epoch: self.epoch,
                treasury: self.epochs.treasury,
                reserves: self.epochs.reserves,
                snapshots: self.epochs.snapshots.clone(),
                protocol_params: self.epochs.protocol_params.clone(),
                prev_protocol_params: self.epochs.prev_protocol_params.clone(),
                prev_d: self.epochs.prev_d.clone(),
                prev_protocol_version_major: self.epochs.prev_protocol_version_major,
                pending_pp_updates_cleared: self.epochs.pending_pp_updates.is_empty()
                    && self.epochs.future_pp_updates.is_empty(),
                epoch_nonce: self.consensus.epoch_nonce,
                last_epoch_block_nonce: self.consensus.last_epoch_block_nonce,
                reward_credits: std::collections::HashMap::new(),
                pools_retired: Vec::new(),
                future_params_promoted: Vec::new(),
                drep_activity_updates: self
                    .gov
                    .governance
                    .dreps
                    .iter()
                    .map(|(cred, drep)| (*cred, drep.active))
                    .collect(),
                last_ratified: self.gov.governance.last_ratified.clone(),
                last_expired: self.gov.governance.last_expired.clone(),
                last_ratify_delayed: self.gov.governance.last_ratify_delayed,
                new_constitution: self.gov.governance.constitution.clone(),
                no_confidence: Some(self.gov.governance.no_confidence),
                committee_threshold: Some(self.gov.governance.committee_threshold.clone()),
                proposals_enacted: self
                    .gov
                    .governance
                    .last_ratified
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect(),
                proposals_expired: self.gov.governance.last_expired.clone(),
                enacted_pparam_update: Some(self.gov.governance.enacted_pparam_update.clone()),
                enacted_hard_fork: Some(self.gov.governance.enacted_hard_fork.clone()),
                enacted_committee: Some(self.gov.governance.enacted_committee.clone()),
                enacted_constitution: Some(self.gov.governance.enacted_constitution.clone()),
                stake_distribution: self.certs.stake_distribution.clone(),
                delegation_changes: Vec::new(),
            });
        }

        // Build per-block scalar field delta from post-block state.
        //
        // Mirror Haskell `incrBlocks` (eras/shelley/impl/.../BlockBody/
        // Internal.hs:241): per-block pool attribution gated on
        // `!isOverlaySlot(firstSlotOfCurrentEpoch, d, blockSlot)`.
        // This must match the same gate in
        // `crates/dugite-ledger/src/eras/common.rs::compute_shelley_nonce`
        // so this delta agrees with the canonical attribution count.
        let pool_block_increment = if !block.header.issuer_vkey.is_empty() {
            let (d_num, d_den) = if self.epochs.protocol_params.protocol_version_major >= 7 {
                (0u64, 1u64)
            } else {
                (
                    self.epochs.protocol_params.d.numerator,
                    self.epochs.protocol_params.d.denominator.max(1),
                )
            };
            let first_slot_of_current_epoch = self
                .epoch
                .0
                .saturating_mul(self.epoch_length)
                .saturating_add(
                    self.shelley_transition_epoch
                        .saturating_mul(self.byron_epoch_length),
                );
            if !crate::eras::common::is_overlay_slot(
                first_slot_of_current_epoch,
                block.slot().0,
                d_num,
                d_den,
            ) {
                Some(dugite_primitives::hash::blake2b_224(
                    &block.header.issuer_vkey,
                ))
            } else {
                None
            }
        } else {
            None
        };

        delta.block_fields = BlockFieldsDelta {
            fees_collected: block
                .transactions
                .iter()
                .filter(|tx| tx.is_valid)
                .map(|tx| tx.body.fee)
                .fold(Lovelace(0), |acc, fee| Lovelace(acc.0 + fee.0)),
            pool_block_increment,
            epoch_block_count: self.consensus.epoch_block_count,
            evolving_nonce: self.consensus.evolving_nonce,
            candidate_nonce: self.consensus.candidate_nonce,
            lab_nonce: self.consensus.lab_nonce,
            epoch_fees: self.utxo.epoch_fees,
        };

        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address, ByronAddress, EnterpriseAddress};
    use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::{BlockNo, SlotNo};
    use dugite_primitives::transaction::{
        ExUnits, OutputDatum, Redeemer, RedeemerTag, ScriptRef, Transaction, TransactionInput,
        TransactionOutput,
    };
    use dugite_primitives::value::{Lovelace, Value};

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Build a minimal `Block` for the given era, slot, and block number.
    ///
    /// `prev_hash` is set to zero so the ledger tip check is skipped (the
    /// state starts at `Point::Origin` so the prev-hash validation is
    /// bypassed entirely for the first block applied).
    fn make_test_block(
        era: Era,
        slot: u64,
        block_no: u64,
        protocol_major: u64,
        body_size: u64,
        txs: Vec<Transaction>,
    ) -> Block {
        Block {
            header: BlockHeader {
                header_hash: Hash32::from_bytes({
                    let mut b = [0u8; 32];
                    b[..8].copy_from_slice(&block_no.to_be_bytes());
                    b
                }),
                prev_hash: Hash32::ZERO,
                issuer_vkey: vec![],
                vrf_vkey: vec![],
                vrf_result: VrfOutput {
                    output: vec![],
                    proof: vec![],
                },
                nonce_vrf_output: vec![],
                nonce_vrf_proof: vec![],
                prev_nonce: None,
                raw_header_body: None,
                block_number: BlockNo(block_no),
                slot: SlotNo(slot),
                epoch_nonce: Hash32::ZERO,
                body_size,
                body_hash: Hash32::ZERO,
                operational_cert: OperationalCert {
                    hot_vkey: vec![],
                    sequence_number: 0,
                    kes_period: 0,
                    sigma: vec![],
                },
                protocol_version: ProtocolVersion {
                    major: protocol_major,
                    minor: 0,
                },
                kes_signature: vec![],
            },
            transactions: txs,
            era,
            raw_cbor: None,
        }
    }

    /// Build a minimal Shelley+ `TransactionOutput` with an enterprise address
    /// and ADA-only value.  No datum, no script_ref.
    fn make_output(coin: u64) -> TransactionOutput {
        TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0xABu8; 28])),
            }),
            value: Value {
                coin: Lovelace(coin),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Build a minimal valid-looking transaction in ApplyOnly style.
    ///
    /// `tx_id_byte` is used to uniquely distinguish the transaction hash so
    /// that sequential UTxO spending tests can reference specific outputs by
    /// their parent tx hash.
    fn make_simple_tx(
        tx_id_byte: u8,
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        fee: u64,
    ) -> Transaction {
        let hash = Hash32::from_bytes([tx_id_byte; 32]);
        let mut tx = Transaction::empty_with_hash(hash);
        tx.body.inputs = inputs;
        tx.body.outputs = outputs;
        tx.body.fee = Lovelace(fee);
        tx
    }

    /// Seed a UTxO entry in the ledger state.
    fn seed_utxo(state: &mut LedgerState, input: TransactionInput, output: TransactionOutput) {
        state.utxo.utxo_set.insert(input, output);
    }

    // ── Test 1: Byron-era block with one tx consuming a UTxO ─────────────────

    #[test]
    fn test_apply_byron_block() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Seed the UTxO that the Byron tx will spend.
        let genesis_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x01u8; 32]),
            index: 0,
        };
        let genesis_output = TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![0u8; 32],
            }),
            value: Value {
                coin: Lovelace(10_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        };
        seed_utxo(&mut state, genesis_input.clone(), genesis_output);

        // Build a Byron tx that spends the genesis UTxO.
        // Fee > 0 satisfies the value-conservation check (no minimum fee check
        // when raw_cbor is None → tx_size_bytes = 0 → min_fee = min_fee_b).
        // We set fee to min_fee_b (155381) so conservation holds:
        // input 10_000_000 = output 9_844_619 + fee 155_381.
        let fee: u64 = state.epochs.protocol_params.min_fee_b;
        let output_value = 10_000_000u64 - fee;

        let _out_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x10u8; 32]),
            index: 0,
        };
        let out_output = TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![1u8; 32],
            }),
            value: Value {
                coin: Lovelace(output_value),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        };

        let mut tx = Transaction::empty_with_hash(Hash32::from_bytes([0x10u8; 32]));
        tx.body.inputs = vec![genesis_input.clone()];
        tx.body.outputs = vec![out_output];
        tx.body.fee = Lovelace(fee);

        // Byron slots: 208 epochs × 21600 slots/epoch = 4,492,800.
        // Slot 100 is firmly in the Byron era.
        let block = make_test_block(Era::Byron, 100, 1, 1, 0, vec![tx]);

        // ApplyOnly skips fee-policy validation while still applying UTxO changes.
        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Byron block should apply");

        // The genesis UTxO must have been consumed.
        assert!(
            state.utxo.utxo_set.lookup(&genesis_input).is_none(),
            "Spent Byron input must be removed"
        );
        // The new output at index 0 of the tx hash must exist.
        let new_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x10u8; 32]),
            index: 0,
        };
        assert!(
            state.utxo.utxo_set.lookup(&new_input).is_some(),
            "Byron output must be created"
        );
        // Tip was advanced.
        assert_ne!(state.tip, dugite_primitives::block::Tip::origin());
    }

    /// Issue #670: apply_block of a Conway+ block must update
    /// `consensus.opcert_counters` with the per-pool max
    /// `OperationalCert.sequence_number`. Mirrors
    /// `Haskell PraosState.ocertCounters`. Without this update the
    /// from-genesis ledger diverges from the ancillary import on the
    /// `opcert_counters` field of `ConsensusSubState` (verify-ledger-snapshot
    /// shows `L=0 R=467` on preview epoch-1308 snapshots).
    #[test]
    fn test_apply_block_tracks_opcert_counters() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x77u8; 32]),
            index: 0,
        };
        seed_utxo(&mut state, input.clone(), make_output(5_000_000));

        let output = make_output(4_500_000);
        let tx = make_simple_tx(0x78, vec![input.clone()], vec![output], 500_000);
        let mut block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx]);
        // Issuer vkey + non-zero opcert sequence number
        block.header.issuer_vkey = vec![0xAA; 32];
        block.header.operational_cert.sequence_number = 42;

        let pool_id = dugite_primitives::hash::blake2b_224(&block.header.issuer_vkey);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Conway block must apply");

        assert_eq!(
            state.consensus.opcert_counters.get(&pool_id).copied(),
            Some(42),
            "apply_block must record OperationalCert.sequence_number in \
             consensus.opcert_counters via compute_shelley_nonce"
        );

        // Newer opcert seq → counter advances. Second block must chain
        // off block #1 — its `prev_hash` must equal block #1's
        // `header_hash`.
        let prev_hash = block.header.header_hash;
        let mut block2 = make_test_block(Era::Conway, 1_020, 2, 9, 0, vec![]);
        block2.header.prev_hash = prev_hash;
        block2.header.issuer_vkey = vec![0xAA; 32];
        block2.header.operational_cert.sequence_number = 99;
        state
            .apply_block(&block2, BlockValidationMode::ApplyOnly)
            .expect("Conway block must apply");
        assert_eq!(
            state.consensus.opcert_counters.get(&pool_id).copied(),
            Some(99)
        );
    }

    // ── Test 2: Shelley+ block with one valid tx ──────────────────────────────

    #[test]
    fn test_apply_shelley_block() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x02u8; 32]),
            index: 0,
        };
        seed_utxo(&mut state, input.clone(), make_output(5_000_000));

        let output = make_output(4_500_000);
        let tx = make_simple_tx(0x20, vec![input.clone()], vec![output], 500_000);

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx.clone()]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Shelley+ block should apply");

        // Input was consumed.
        assert!(state.utxo.utxo_set.lookup(&input).is_none());
        // New output at index 0 exists.
        let new_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x20u8; 32]),
            index: 0,
        };
        assert!(state.utxo.utxo_set.lookup(&new_input).is_some());
        // Tip updated.
        assert_eq!(state.tip.block_number, BlockNo(1));
    }

    // ── Test 3: Empty block ───────────────────────────────────────────────────

    #[test]
    fn test_apply_empty_block() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Seed one UTxO — it must survive the empty block.
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x03u8; 32]),
            index: 0,
        };
        seed_utxo(&mut state, input.clone(), make_output(1_000_000));

        let block = make_test_block(Era::Conway, 2_000, 1, 9, 0, vec![]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Empty block should apply");

        // UTxO untouched.
        assert!(state.utxo.utxo_set.lookup(&input).is_some());
        // Tip updated.
        assert_eq!(state.tip.point.slot().unwrap(), SlotNo(2_000));
    }

    // ── Test 4: invalid tx — collateral consumed, regular input untouched ────

    #[test]
    fn test_invalid_tx_collateral_consumed() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Regular input (must NOT be consumed).
        let regular_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x04u8; 32]),
            index: 0,
        };
        seed_utxo(&mut state, regular_input.clone(), make_output(2_000_000));

        // Collateral input (MUST be consumed).
        let collateral_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x04u8; 32]),
            index: 1,
        };
        seed_utxo(&mut state, collateral_input.clone(), make_output(3_000_000));

        let mut tx = Transaction::empty_with_hash(Hash32::from_bytes([0x40u8; 32]));
        tx.body.inputs = vec![regular_input.clone()];
        tx.body.collateral = vec![collateral_input.clone()];
        tx.body.fee = Lovelace(0);
        tx.is_valid = false;

        let block = make_test_block(Era::Conway, 3_000, 1, 9, 0, vec![tx]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Block with invalid tx should apply");

        // Collateral was spent.
        assert!(
            state.utxo.utxo_set.lookup(&collateral_input).is_none(),
            "Collateral input must be consumed"
        );
        // Regular input survived.
        assert!(
            state.utxo.utxo_set.lookup(&regular_input).is_some(),
            "Regular input of invalid tx must not be consumed"
        );
    }

    // ── Test 5: Epoch transition detected ────────────────────────────────────

    #[test]
    fn test_epoch_transition_detected() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        // Zero out Byron transition so epoch_of_slot is just slot/epoch_length.
        state.shelley_transition_epoch = 0;
        state.byron_epoch_length = 0;
        assert_eq!(state.epoch, EpochNo(0));

        // First slot of epoch 1 triggers the transition.
        let slot = state.epoch_length; // 432000
        let block = make_test_block(Era::Conway, slot, 1, 9, 0, vec![]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Block should apply");

        assert_eq!(state.epoch, EpochNo(1));
    }

    // ── Test 6: Multi-epoch gap ───────────────────────────────────────────────

    #[test]
    fn test_multi_epoch_gap() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.shelley_transition_epoch = 0;
        state.byron_epoch_length = 0;
        assert_eq!(state.epoch, EpochNo(0));

        // Jump directly to epoch 3.
        let slot = state.epoch_length * 3; // first slot of epoch 3
        let block = make_test_block(Era::Conway, slot, 1, 9, 0, vec![]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Block should apply after multi-epoch gap");

        // All three epoch transitions (1, 2, 3) must have been processed.
        assert_eq!(state.epoch, EpochNo(3));
    }

    // ── Test 8: Per-tx ref-script size limit (Conway ValidateAll) ─────────────

    #[test]
    fn test_ref_script_size_per_tx_limit() {
        let mut params = ProtocolParameters::mainnet_defaults();
        // Must be Conway (protocol_version_major >= 9).
        params.protocol_version_major = 9;
        let mut state = LedgerState::new(params);

        // A UTxO whose script_ref exceeds the 200 KiB per-tx limit.
        let big_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x08u8; 32]),
            index: 0,
        };
        let big_output = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x08u8; 28])),
            }),
            value: Value {
                coin: Lovelace(5_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            // 205_000 bytes > 200 * 1024 (204_800) per-tx limit.
            script_ref: Some(ScriptRef::PlutusV2(vec![0u8; 205_000])),
            is_legacy: false,
            raw_cbor: None,
        };
        seed_utxo(&mut state, big_input.clone(), big_output);

        // A valid tx that spends the large-script UTxO.
        let tx = make_simple_tx(0x80, vec![big_input], vec![make_output(4_000_000)], 0);

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx]);

        let result = state.apply_block(&block, BlockValidationMode::ValidateAll);
        assert!(
            result.is_err(),
            "Per-tx ref-script size limit must be enforced in ValidateAll mode"
        );
        if let Err(LedgerError::BlockTxValidationFailed { errors, .. }) = result {
            assert!(
                errors.contains("TxRefScriptSizeTooLarge"),
                "Error must mention TxRefScriptSizeTooLarge, got: {errors}"
            );
        }
    }

    // ── Test 9: Per-block ref-script size limit ───────────────────────────────

    #[test]
    fn test_ref_script_size_per_block_limit() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let mut state = LedgerState::new(params);

        // Create multiple UTxOs each with a script_ref that individually fits
        // within the 200 KiB per-tx limit but together exceed the 1 MiB
        // per-block limit.  6 × 200 KiB = 1,200 KiB > 1,048,576 bytes.
        let script_bytes = vec![0u8; 200 * 1024];
        let mut txs = Vec::new();
        for i in 0u8..6 {
            let inp = TransactionInput {
                transaction_id: Hash32::from_bytes([0x09u8 + i; 32]),
                index: 0,
            };
            let out = TransactionOutput {
                address: Address::Enterprise(EnterpriseAddress {
                    network: NetworkId::Mainnet,
                    payment: Credential::VerificationKey(Hash28::from_bytes([0x09u8 + i; 28])),
                }),
                value: Value {
                    coin: Lovelace(2_000_000),
                    multi_asset: Default::default(),
                },
                datum: OutputDatum::None,
                script_ref: Some(ScriptRef::PlutusV2(script_bytes.clone())),
                is_legacy: false,
                raw_cbor: None,
            };
            seed_utxo(&mut state, inp.clone(), out);

            let tx = make_simple_tx(0x90 + i, vec![inp], vec![make_output(1_000_000)], 0);
            txs.push(tx);
        }

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, txs);

        let result = state.apply_block(&block, BlockValidationMode::ValidateAll);
        assert!(
            result.is_err(),
            "Per-block ref-script size limit must be enforced"
        );
        if let Err(LedgerError::BlockTxValidationFailed { errors, .. }) = result {
            assert!(
                errors.contains("BodyRefScriptsSizeTooBig"),
                "Error must mention BodyRefScriptsSizeTooBig, got: {errors}"
            );
        }
    }

    // ── Test 10: Block ExUnits memory budget exceeded ─────────────────────────

    #[test]
    fn test_block_ex_units_memory_exceeded() {
        let mut params = ProtocolParameters::mainnet_defaults();
        // Lower the per-block limit to make it easy to exceed with two txs.
        params.max_block_ex_units.mem = 10;
        let mut state = LedgerState::new(params);

        // Two valid txs each consuming 6 memory units → total 12 > limit 10.
        let mut txs = Vec::new();
        for i in 0u8..2 {
            let inp = TransactionInput {
                transaction_id: Hash32::from_bytes([0x0Au8 + i; 32]),
                index: 0,
            };
            seed_utxo(&mut state, inp.clone(), make_output(2_000_000));

            let mut tx = make_simple_tx(0xA0 + i, vec![inp], vec![make_output(1_000_000)], 0);
            tx.witness_set.redeemers = vec![Redeemer {
                tag: RedeemerTag::Spend,
                index: 0,
                data: dugite_primitives::transaction::PlutusData::Integer(
                    num_bigint::BigInt::from(0i64),
                ),
                ex_units: ExUnits { mem: 6, steps: 1 },
            }];
            txs.push(tx);
        }

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, txs);

        let result = state.apply_block(&block, BlockValidationMode::ValidateAll);
        assert!(result.is_err(), "Exceeded memory budget must be rejected");
        if let Err(LedgerError::BlockTxValidationFailed { errors, .. }) = result {
            assert!(
                errors.contains("BlockExUnitsExceeded") && errors.contains("memory"),
                "Error must mention BlockExUnitsExceeded (memory), got: {errors}"
            );
        }
    }

    // ── Test 11: Block ExUnits steps budget exceeded ───────────────────────────

    #[test]
    fn test_block_ex_units_steps_exceeded() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.max_block_ex_units.steps = 10;
        let mut state = LedgerState::new(params);

        let mut txs = Vec::new();
        for i in 0u8..2 {
            let inp = TransactionInput {
                transaction_id: Hash32::from_bytes([0x0Bu8 + i; 32]),
                index: 0,
            };
            seed_utxo(&mut state, inp.clone(), make_output(2_000_000));

            let mut tx = make_simple_tx(0xB0 + i, vec![inp], vec![make_output(1_000_000)], 0);
            tx.witness_set.redeemers = vec![Redeemer {
                tag: RedeemerTag::Spend,
                index: 0,
                data: dugite_primitives::transaction::PlutusData::Integer(
                    num_bigint::BigInt::from(0i64),
                ),
                ex_units: ExUnits { mem: 1, steps: 6 },
            }];
            txs.push(tx);
        }

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, txs);

        let result = state.apply_block(&block, BlockValidationMode::ValidateAll);
        assert!(result.is_err(), "Exceeded steps budget must be rejected");
        if let Err(LedgerError::BlockTxValidationFailed { errors, .. }) = result {
            assert!(
                errors.contains("BlockExUnitsExceeded") && errors.contains("step"),
                "Error must mention BlockExUnitsExceeded (steps), got: {errors}"
            );
        }
    }

    // ── Test 12: Sequential UTxO — tx1 creates, tx2 spends in same block ─────

    #[test]
    fn test_multiple_txs_sequential_utxo() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Genesis UTxO consumed by tx1.
        let genesis_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x0Cu8; 32]),
            index: 0,
        };
        seed_utxo(&mut state, genesis_input.clone(), make_output(5_000_000));

        // tx1: spends genesis_input, creates a new output.
        let tx1_hash = Hash32::from_bytes([0xC0u8; 32]);
        let mut tx1 = Transaction::empty_with_hash(tx1_hash);
        tx1.body.inputs = vec![genesis_input.clone()];
        tx1.body.outputs = vec![make_output(4_500_000)];
        tx1.body.fee = Lovelace(500_000);

        // tx2: spends tx1's output (index 0).
        let tx1_output_input = TransactionInput {
            transaction_id: tx1_hash,
            index: 0,
        };
        let tx2_hash = Hash32::from_bytes([0xC1u8; 32]);
        let mut tx2 = Transaction::empty_with_hash(tx2_hash);
        tx2.body.inputs = vec![tx1_output_input.clone()];
        tx2.body.outputs = vec![make_output(4_000_000)];
        tx2.body.fee = Lovelace(500_000);

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx1, tx2]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Sequential within-block spending should succeed");

        // Genesis input consumed.
        assert!(state.utxo.utxo_set.lookup(&genesis_input).is_none());
        // tx1's intermediate output was consumed by tx2.
        assert!(state.utxo.utxo_set.lookup(&tx1_output_input).is_none());
        // tx2's output exists.
        let tx2_out = TransactionInput {
            transaction_id: tx2_hash,
            index: 0,
        };
        assert!(state.utxo.utxo_set.lookup(&tx2_out).is_some());
    }

    // ── Test 13: Conway pointer-stake exclusion ───────────────────────────────

    #[test]
    fn test_conway_pointer_stake_exclusion() {
        use dugite_primitives::credentials::Pointer;

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Set era to Babbage so the Conway block triggers an era transition,
        // which is where the pointer-stake exclusion logic now lives
        // (on_era_transition in conway.rs).
        state.era = Era::Babbage;

        // Pre-seed ptr_stake entries that should be cleared on first Conway block.
        state.epochs.ptr_stake.insert(
            Pointer {
                slot: 1,
                tx_index: 0,
                cert_index: 0,
            },
            1_000_000,
        );
        state.epochs.ptr_stake.insert(
            Pointer {
                slot: 2,
                tx_index: 0,
                cert_index: 0,
            },
            2_000_000,
        );
        assert_eq!(state.epochs.ptr_stake.len(), 2);
        assert!(!state.epochs.ptr_stake_excluded);

        // Apply a Conway-era block — the era transition from Babbage to Conway
        // triggers on_era_transition which sets ptr_stake_excluded = true.
        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Conway block should apply");

        // The one-time exclusion flag must be set after the era transition.
        assert!(
            state.epochs.ptr_stake_excluded,
            "ptr_stake_excluded must be true after first Conway block"
        );
    }

    // ── Test 14: ApplyOnly mode skips per-tx ref-script validation ────────────

    #[test]
    fn test_apply_only_mode_skips_validation() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let mut state = LedgerState::new(params);

        // Same setup as test 8 — a UTxO with a script_ref that exceeds the
        // 200 KiB per-tx limit.  In ValidateAll mode this returns Err; in
        // ApplyOnly mode the check is skipped.
        let big_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x0Eu8; 32]),
            index: 0,
        };
        let big_output = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x0Eu8; 28])),
            }),
            value: Value {
                coin: Lovelace(5_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: Some(ScriptRef::PlutusV2(vec![0u8; 205_000])),
            is_legacy: false,
            raw_cbor: None,
        };
        seed_utxo(&mut state, big_input.clone(), big_output);

        let tx = make_simple_tx(0xE0, vec![big_input], vec![make_output(4_000_000)], 0);
        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx]);

        // ApplyOnly must skip the per-tx ref-script size check.
        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("ApplyOnly must succeed even with oversized ref-script");

        // Block was applied — tip advanced.
        assert_eq!(state.tip.block_number, BlockNo(1));
    }

    // ── Test 15: Certificate processing — StakeRegistration per tx ───────────

    #[test]
    fn test_certificate_processing_order() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Two credentials to register via StakeRegistration certs.
        let cred1 = Credential::VerificationKey(Hash28::from_bytes([0x0Fu8; 28]));
        let cred2 = Credential::VerificationKey(Hash28::from_bytes([0x1Fu8; 28]));
        let key1 = cred1.to_typed_hash32();
        let key2 = cred2.to_typed_hash32();

        // tx1 registers cred1; tx2 registers cred2.
        let mut tx1 = Transaction::empty_with_hash(Hash32::from_bytes([0xF0u8; 32]));
        tx1.body.certificates = vec![Certificate::StakeRegistration(cred1)];

        let mut tx2 = Transaction::empty_with_hash(Hash32::from_bytes([0xF1u8; 32]));
        tx2.body.certificates = vec![Certificate::StakeRegistration(cred2)];

        let block = make_test_block(Era::Conway, 1_000, 1, 9, 0, vec![tx1, tx2]);

        state
            .apply_block(&block, BlockValidationMode::ApplyOnly)
            .expect("Block with stake-registration certs should apply");

        // Both credentials must now have a reward-account entry.
        let reward_accounts = &state.certs.reward_accounts;
        assert!(
            reward_accounts.contains_key(&key1),
            "cred1 must be registered in reward_accounts"
        );
        assert!(
            reward_accounts.contains_key(&key2),
            "cred2 must be registered in reward_accounts"
        );
    }

    // ── Test 16: ApplyOnly rejects Shelley+ hash mismatch ────────────────────
    //
    // After Sprint 1 Task 1, `ApplyOnly` only tolerates hash mismatch for Byron
    // blocks. Shelley+ blocks must still be rejected — the legacy decoder's Shelley-era
    // `OriginalHash` uses raw bytes so hash mismatch cannot legitimately occur
    // through the decode→store→decode cycle.
    #[test]
    fn test_apply_only_rejects_shelley_hash_mismatch() {
        let params = ProtocolParameters::mainnet_defaults();
        let mut state = LedgerState::new(params);
        state.era = Era::Conway;
        state.tip = dugite_primitives::block::Tip {
            point: Point::Specific(SlotNo(100), Hash32::from_bytes([0xAAu8; 32])),
            block_number: BlockNo(10),
        };

        // Conway-era block at tip+1 with a prev_hash that does NOT match tip
        // hash — must be rejected in ApplyOnly mode (bypass is Byron-only now).
        let mut shelley_block = make_test_block(Era::Conway, 101, 11, 9, 0, vec![]);
        // prev_hash = 0xBB... does not match tip hash = 0xAA...
        shelley_block.header.prev_hash = Hash32::from_bytes([0xBBu8; 32]);

        let result = state.apply_block(&shelley_block, BlockValidationMode::ApplyOnly);

        assert!(
            matches!(result, Err(LedgerError::BlockDoesNotConnect { .. })),
            "ApplyOnly must reject Shelley+ hash mismatch; bypass is Byron-only now. Got: {result:?}"
        );
        assert_eq!(state.tip.block_number, BlockNo(10));
    }

    // ── Test 17: ApplyOnly accepts Byron hash mismatch (re-encode bug) ──
    //
    // The `ApplyOnly` bypass is retained for Byron blocks: the legacy decoder's
    // `OriginalHash<32> for KeepRaw<'_, byron::BlockHead>` re-encodes the
    // decoded struct and can produce a hash different from the original wire
    // bytes. Chunk-file replay must tolerate this until the in-house upstream
    // fix lands (tracked separately).
    #[test]
    fn test_apply_only_byron_hash_mismatch_accepted() {
        let params = ProtocolParameters::mainnet_defaults();
        let mut state = LedgerState::new(params);
        state.era = Era::Byron;
        state.tip = dugite_primitives::block::Tip {
            point: Point::Specific(SlotNo(100), Hash32::from_bytes([0xAAu8; 32])),
            block_number: BlockNo(10),
        };

        // Byron-era block at tip+1 with a prev_hash that does NOT match tip
        // hash — must be accepted in ApplyOnly mode (legacy-decoder bypass retained).
        let mut byron_block = make_test_block(Era::Byron, 101, 11, 1, 0, vec![]);
        // prev_hash = 0xBB... does not match tip hash = 0xAA...
        byron_block.header.prev_hash = Hash32::from_bytes([0xBBu8; 32]);

        let result = state.apply_block(&byron_block, BlockValidationMode::ApplyOnly);

        assert!(
            result.is_ok(),
            "ApplyOnly + Byron era must retain the bypass until upstream fix. Got: {result:?}"
        );
        assert_eq!(state.tip.block_number, BlockNo(11));
    }

    // ── Regression: bogus body-size approximation must not exist ─────────────
    //
    // The old check compared header.body_size > max_block_body_size and rejected
    // with WrongBlockBodySize. That predicate was wrong (Haskell uses equality)
    // and at the wrong layer (max_block_body_size is a chain-checks cap, not
    // BBODY). This test ensures the approximation does not re-appear.
    #[test]
    fn test_body_size_approximation_removed() {
        let mut params = ProtocolParameters::mainnet_defaults();
        // Set a small cap so the old check would fire.
        params.max_block_body_size = 100;
        let mut state = LedgerState::new(params);

        // body_size (200) > max_block_body_size (100) — the bogus check would
        // have rejected this with WrongBlockBodySize.
        let block = make_test_block(Era::Conway, 1_000, 1, 9, 200, vec![]);

        let result = state.apply_block(&block, BlockValidationMode::ValidateAll);

        // The removed approximation must NOT produce WrongBlockBodySize.
        // Any other error or Ok is acceptable.
        assert!(
            !matches!(result, Err(LedgerError::WrongBlockBodySize { .. })),
            "Bogus body-size approximation must not be present; got: {result:?}"
        );
    }
}
