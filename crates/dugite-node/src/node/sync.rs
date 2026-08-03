//! Block sync loop, forward-block processing, rollback handling, and ledger replay.
//!
//! This module contains the core pipelined ChainSync state machine that drives
//! block ingestion from upstream peers, as well as the ledger replay path used
//! after a Mithril snapshot import.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{watch, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::connection_lifecycle::{CandidateChainState, PendingHeader};
use super::networking::EbbInfo;
use dugite_consensus::praos::BlockIssuerInfo;
use dugite_consensus::ValidationMode;
use dugite_ledger::BlockValidationMode;
use dugite_network::codec::Point as CodecPoint;
use dugite_network::protocol::chainsync::{
    decode_message as cs_decode, encode_message as cs_encode, ChainSyncMessage,
};
use dugite_network::MuxChannel;
use dugite_network::RollbackAnnouncement;
use dugite_primitives::block::Point;

use super::Node;

// ─── Genesis block validation (free function, used by tests) ─────────────────

/// Validate genesis blocks against expected hashes from the configuration.
///
/// When syncing from genesis (Origin), the first blocks received are the genesis
/// blocks for the chain. For Byron-era networks (mainnet, preprod), the first
/// block is a Byron Epoch Boundary Block (EBB) whose hash must match the
/// expected Byron genesis hash. For networks that start directly in the Shelley
/// era (preview), the first block's prev_hash should match the expected Shelley
/// genesis hash.
///
/// This validation is crucial to ensure we are syncing the correct chain and
/// not connecting to a peer serving a different network's blocks.
#[allow(dead_code)] // retained for networking rewrite; also used in tests
pub fn validate_genesis_blocks(
    blocks: &[dugite_primitives::block::Block],
    expected_byron_hash: Option<&dugite_primitives::hash::Hash32>,
    expected_shelley_hash: Option<&dugite_primitives::hash::Hash32>,
) -> Result<()> {
    if blocks.is_empty() {
        return Ok(());
    }

    let first_block = &blocks[0];

    // Only validate if we're starting from genesis (block 0 at slot 0).
    // If ChainDB already has blocks, genesis was validated on a prior run.
    if first_block.block_number().0 != 0 {
        debug!(
            "Skipping genesis validation — not syncing from genesis (block={})",
            first_block.block_number().0,
        );
        return Ok(());
    }

    // For Byron-era chains, the first block is the Byron EBB (block 0, slot 0).
    // Its hash must match the expected Byron genesis hash.
    if first_block.era == dugite_primitives::era::Era::Byron {
        if let Some(expected) = expected_byron_hash {
            let actual = first_block.hash();
            if actual != expected {
                return Err(anyhow::anyhow!(
                    "Byron genesis block hash mismatch: expected {}, got {} — \
                     this chain does not match the configured genesis. \
                     Check that you are connecting to the correct network.",
                    expected.to_hex(),
                    actual.to_hex()
                ));
            }
            debug!("Byron genesis block validated: {}", actual.to_hex());
        } else {
            warn!("No Byron genesis hash configured — skipping Byron genesis block validation");
        }
    }

    // For Shelley-first chains (e.g., preview testnet), the first block may be
    // a Shelley-era block. Its prev_hash points to the Shelley genesis hash.
    if first_block.era.is_shelley_based() && first_block.block_number().0 == 0 {
        if let Some(expected) = expected_shelley_hash {
            let prev_hash = first_block.prev_hash();
            if prev_hash != expected {
                return Err(anyhow::anyhow!(
                    "Shelley genesis hash mismatch: expected {}, but first block's \
                     prev_hash is {} — this chain does not match the configured genesis. \
                     Check that you are connecting to the correct network.",
                    expected.to_hex(),
                    prev_hash.to_hex()
                ));
            }
            debug!("Shelley genesis ref validated: {}", expected.to_hex());
        } else {
            warn!("No Shelley genesis hash configured — skipping Shelley genesis block validation");
        }
    }

    Ok(())
}

// ─── Node impl: sync loop ─────────────────────────────────────────────────────

impl Node {
    /// Validate genesis blocks against expected hashes from the configuration.
    #[allow(dead_code)] // retained for networking rewrite
    pub(crate) fn validate_genesis_blocks(
        &self,
        blocks: &[dugite_primitives::block::Block],
    ) -> Result<()> {
        validate_genesis_blocks(
            blocks,
            self.expected_byron_genesis_hash.as_ref(),
            self.expected_shelley_genesis_hash.as_ref(),
        )
    }

    /// Compute the current absolute slot number from wall-clock time.
    ///
    /// Uses the HFC era history state machine to correctly account for all
    /// era transitions (Byron→Shelley→...→Conway) with their respective
    /// slot durations.
    pub async fn current_wall_clock_slot(&self) -> Option<dugite_primitives::time::SlotNo> {
        let genesis = self.shelley_genesis.as_ref()?;
        let system_start = dugite_primitives::time::SystemStart {
            utc_time: chrono::DateTime::parse_from_rfc3339(&genesis.system_start)
                .ok()?
                .with_timezone(&chrono::Utc),
        };
        let eh = self.era_history.read().await;
        eh.wallclock_to_slot(chrono::Utc::now(), &system_start).ok()
    }

    /// Compute the POSIX wall-clock time (in ms) of an absolute slot.
    ///
    /// Era-aware: uses the HFC era history so Byron's 20-sec slots are
    /// correctly distinguished from Shelley+'s 1-sec slots. Without this,
    /// the naive `zero_time + slot * 1s` formula under-counts wall time
    /// on networks with non-zero `shelley_transition_epoch` (e.g. preprod
    /// has 4 Byron epochs = 86400 Byron slots × 20s = 1,728,000 sec
    /// = 20 days of wall time that the naive formula misses), causing
    /// `dugite_tip_age_seconds` to be off by exactly that gap — most
    /// visibly the "tip 19d 0h 0m" display in dugite-monitor.
    ///
    /// Falls back to the naive `slot_config` formula only when the
    /// Shelley genesis isn't loaded yet or `slot_to_wallclock` returns
    /// `PastHorizonError` (shouldn't happen for already-applied slots).
    pub async fn slot_to_wallclock_ms(
        &self,
        slot: u64,
        ledger_slot_config: &dugite_ledger::plutus::SlotConfig,
    ) -> u64 {
        let fallback = || {
            ledger_slot_config.zero_time
                + slot.saturating_sub(ledger_slot_config.zero_slot)
                    * ledger_slot_config.slot_length as u64
        };
        let Some(genesis) = self.shelley_genesis.as_ref() else {
            return fallback();
        };
        let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&genesis.system_start) else {
            return fallback();
        };
        let system_start = dugite_primitives::time::SystemStart {
            utc_time: parsed.with_timezone(&chrono::Utc),
        };
        let eh = self.era_history.read().await;
        match eh.slot_to_wallclock(dugite_primitives::time::SlotNo(slot), &system_start) {
            Ok(utc) => utc.timestamp_millis().max(0) as u64,
            Err(_) => fallback(),
        }
    }

    /// Update the time-based sync-progress metric.
    ///
    /// Progress = `(tip wall-clock time − genesis) / (now − genesis)`, the
    /// cardano-node definition. The tip time is computed era-aware via
    /// `slot_to_wallclock_ms` (Byron 20 s slots vs Shelley+ 1 s slots), so this
    /// does NOT undercount Byron the way a raw `applied_slot / peer_tip_slot`
    /// ratio did (which read ~1% at ~18% of the chain). `genesis` is the chain
    /// system start (the shelley-genesis `system_start`, which on mainnet is the
    /// Byron genesis time).
    pub async fn update_sync_progress(
        &self,
        tip_slot: u64,
        slot_config: &dugite_ledger::plutus::SlotConfig,
    ) {
        let Some(genesis_ms) = self
            .shelley_genesis
            .as_ref()
            .and_then(|g| chrono::DateTime::parse_from_rfc3339(&g.system_start).ok())
            .map(|t| t.timestamp_millis())
        else {
            // Genesis time unknown — leave the previous value rather than show a
            // bogus percentage.
            return;
        };
        let tip_ms = self.slot_to_wallclock_ms(tip_slot, slot_config).await as i64;
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.metrics
            .set_sync_progress(crate::metrics::compute_sync_progress(
                tip_ms, genesis_ms, now_ms,
            ));
    }

    /// Notify connected N2N/N2C peers of a chain rollback by broadcasting a
    /// `RollbackAnnouncement`.  Both the N2N ChainSync server and the N2C
    /// LocalChainSync server subscribe to this channel and translate the
    /// announcement into `MsgRollBackward` messages for their downstream peers.
    pub async fn notify_rollback(&self, rollback_point: &Point) {
        let rb_slot = rollback_point.slot().map(|s| s.0).unwrap_or(0);
        let rb_hash = rollback_point
            .hash()
            .map(|h| {
                let bytes: &[u8] = h.as_ref();
                let mut arr = [0u8; 32];
                arr.copy_from_slice(bytes);
                arr
            })
            .unwrap_or([0u8; 32]);

        if let Some(ref tx) = self.rollback_announcement_tx {
            let _ = tx.send(RollbackAnnouncement {
                slot: rb_slot,
                hash: rb_hash,
            });
        }
        if let Some(ref tb) = self.tip_broadcaster {
            tb.announce_rollback(crate::node::tip_broadcast::TipRollback {
                slot: rb_slot,
                hash: rb_hash,
            });
        }
    }

    /// Roll back LEDGER state to the rollback point.
    ///
    /// The caller must have already committed the VolatileDB chain switch via
    /// `ChainSelQueue::switch_chain()`; this function only realigns the ledger,
    /// fragment, and mempool to the intersection. It does NOT rewind ChainDB —
    /// doing so would undo the fork switch and leave the volatile tip stuck at
    /// the intersection slot, producing an O(N) per-block cascade.
    ///
    /// # Strategy (Subsystem 4)
    ///
    /// 1. Try the [`LedgerState::rollback_via_seq`] fast path: if the rollback
    ///    target is in the live `LedgerSeq` volatile window, restore the
    ///    entire ledger in O(n) by reverse-applying the trailing UTxO diffs
    ///    and replacing non-UTxO state from the seq.
    /// 2. Otherwise, fall back to snapshot reload + replay (handles deep
    ///    rollbacks past the volatile window and post-restart cases where the
    ///    seq is empty).
    ///
    /// # Return value
    ///
    /// Returns `true` if the rollback succeeded (including benign no-ops like
    /// "already at target" or "immutable-tip guard skipped").  Returns `false`
    /// only when the rollback could not be completed — i.e. the target is
    /// outside both the LedgerSeq volatile window AND no canonical snapshot is
    /// available (or the snapshot was corrupt).  Callers MUST check the return
    /// value and skip fork replay when it is `false`; attempting replay against
    /// a misaligned ledger will always fail and clearing VolatileDB as a
    /// recovery action produces the permanent `StoreButDontChange` cascade
    /// described in the Bug-B design doc (2026-05-16).
    ///
    /// Matches Haskell's `LedgerDB.V2` rollback flow: try the in-memory
    /// `LedgerSeq` first, fall back to snapshot-driven recovery.
    ///
    /// # Architecture note — peer rollbacks
    ///
    /// Peer `MsgRollBackward` does NOT call this function. A single peer's
    /// rollback only means that peer trimmed its candidate fragment; chain
    /// selection (`ChainSelQueue::TriggeredFork`) owns the global ledger
    /// rollback decision (Haskell parity with `ChainSync.Client::rollBackward`).
    /// See the 2026-04-21 fix for the original cascade bug this guards against.
    pub async fn handle_ledger_rollback(&self, rollback_point: &Point) -> bool {
        self.handle_rollback_inner(rollback_point).await
    }

    /// Re-anchor the `LedgerSeq` on the live ledger state, discarding the
    /// volatile delta window.
    ///
    /// # The invariant this exists to maintain (#985)
    ///
    /// `LedgerSeq` is an anchor state plus a window of per-block deltas, and
    /// `tip_state()` reconstructs the volatile tip as *anchor + deltas*. That
    /// reconstruction is only meaningful when the anchor is the state at
    /// `anchor_point` and the deltas chain forward from it — the same
    /// structural guarantee Haskell gets from `AnchoredSeq`.
    ///
    /// dugite advances the anchor implicitly (`push` → `advance_anchor` once
    /// the window reaches `k`), so the invariant holds automatically for the
    /// normal path: apply a block, push its delta. It does **not** hold for
    /// any path that moves `ledger_state` in bulk without pushing deltas —
    /// startup replay, the rollback snapshot slow path, the gap-bridge. Those
    /// leave the anchor at a state the deltas were never computed against.
    ///
    /// The consequence is not a crash but a silent chimera: a reconstructed
    /// state whose delta-tracked fields (tip, slot) are current while every
    /// field no delta touched — protocol params above all — is stale. On the
    /// first v2.5.0 boot over an existing DB, the SNAPSHOT 31→32 quarantine
    /// made that stale state *genesis*, and the first at-tip fork switch
    /// installed preview-genesis pparams (PV6, d=1) into the live ledger. A
    /// canonical Conway block was then judged against the TPraos overlay
    /// schedule, rejected, and cached as invalid — wedging chain selection for
    /// the process lifetime.
    ///
    /// So: **every bulk mutation of `ledger_state` outside
    /// `apply_block_with_delta` + `seq.push` must call this.** Dropping the
    /// window is correct rather than wasteful — those blocks are already
    /// folded into the state being anchored, and a rollback below the new
    /// anchor falls through to the snapshot slow path, which is exactly the
    /// safe behaviour.
    ///
    /// # Relationship to Haskell (oracle-verified against
    /// ouroboros-consensus `release-ouroboros-consensus-3.0.1.0`)
    ///
    /// Upstream re-anchors *per replayed block*: `initReapplyBlock` is
    /// `reapplyThenPush`, which is `extend` followed by `pruneToImmTipOnly`,
    /// collapsing the sequence to the block just applied. Blocks streamed
    /// from the ImmutableDB are immutable by definition, so there is nothing
    /// to keep in a rollback window.
    ///
    /// Calling this once when replay finishes reaches the identical end state
    /// — anchor at the post-replay tip, window empty — because dugite's
    /// replay pushes no deltas at all. The window is empty for the whole
    /// replay, so there is no intermediate reconstruction that could differ.
    /// Per-block collapsing would only add work.
    ///
    /// Note the *live* path deliberately does not collapse: upstream lets the
    /// sequence grow under `extend` and prunes only on the k-bounded
    /// `implGarbageCollect` from `copyToImmutableDB`, which is what dugite's
    /// `push` → `advance_anchor` already mirrors.
    pub(crate) async fn reanchor_ledger_seq(&self, reason: &str) {
        // Lock order: ledger_state before ledger_seq (per Node docs).
        let ls = self.ledger_state.read().await;
        let mut seq = self.ledger_seq.write().await;
        let old_anchor_slot = seq.anchor_point().slot().map(|s| s.0).unwrap_or(0);
        let dropped = seq.deltas().len();
        seq.reset_anchor(ls.clone_without_utxos());
        info!(
            reason,
            old_anchor_slot,
            new_anchor_slot = seq.anchor_point().slot().map(|s| s.0).unwrap_or(0),
            dropped_deltas = dropped,
            "LedgerSeq re-anchored on the live ledger state"
        );
    }

    async fn handle_rollback_inner(&self, rollback_point: &Point) -> bool {
        let rollback_slot = rollback_point.slot().map(|s| s.0).unwrap_or(0);

        // Count every rollback event for observability, even no-ops.
        self.metrics
            .rollback_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Guard: reject rollback targets older than our ImmutableDB tip.
        //
        // A rollback beyond the immutable tip is protocol-impossible on the
        // honest chain (Ouroboros k-block finality). Such an event is produced
        // by a stale peer advertising an old chain tip, or by a divergent
        // peer on a different network. Acting on it would be a no-op at best
        // and state-corrupting at worst — ignoring preserves node state and
        // lets the offending ChainSync session tear down on its next mismatch
        // (connection_lifecycle will drop the peer automatically, no operator
        // restart required).
        {
            let db = self.chain_db.read().await;
            if let Some(imm_point) = db.get_immutable_tip_point() {
                let imm_slot = imm_point.slot().map(|s| s.0).unwrap_or(0);
                if rollback_slot < imm_slot {
                    warn!(
                        rollback_slot,
                        immutable_slot = imm_slot,
                        "Ignoring rollback to point older than immutable tip; \
                         peer is stale or on a divergent chain. Node state preserved."
                    );
                    return true;
                }
            }
        }

        // Classify the rollback event relative to the current ledger tip.
        {
            let ls = self.ledger_state.read().await;
            let ledger_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);

            if rollback_slot == ledger_slot {
                // Already at the rollback point — genuine no-op.
                debug!(
                    rollback_slot,
                    ledger_slot, "Rollback point equals ledger tip, skipping"
                );
                return true;
            }

            if rollback_slot > ledger_slot {
                // The rollback target is AHEAD of the ledger. This happens on
                // restart when the chunk replay couldn't apply VolatileDB-only
                // intermediate blocks: the ledger sits at the last successful
                // ImmutableDB block while ChainSync finds an intersection further
                // along the chain. We need to advance (not roll back) the ledger
                // by replaying blocks from ChainDB up to the rollback point.
                drop(ls);
                let db = self.chain_db.read().await;
                let mut current_slot = ledger_slot;
                let mut replayed = 0u64;
                let mut ls = self.ledger_state.write().await;
                // Point cursor: a Byron EBB shares its slot with the first
                // main block of the epoch, so the walk must step by
                // (slot, hash) — a slot-only step would skip the main block
                // right after applying the EBB.  An Origin/unknown hash
                // falls back to the slot-based lookup inside ChainDB.
                let mut current_hash = ls
                    .tip
                    .point
                    .hash()
                    .copied()
                    .unwrap_or(dugite_primitives::hash::Hash32::ZERO);
                while current_slot < rollback_slot {
                    match db.get_next_block_after_point(
                        dugite_primitives::time::SlotNo(current_slot),
                        &current_hash,
                    ) {
                        Ok(Some((next_slot, next_hash, cbor))) => {
                            if next_slot.0 > rollback_slot {
                                break;
                            }
                            match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                                &cbor,
                                self.byron_epoch_length,
                            ) {
                                Ok(block) => {
                                    if let Err(e) =
                                        ls.apply_block(&block, BlockValidationMode::ApplyOnly)
                                    {
                                        warn!(
                                            slot = next_slot.0,
                                            "Gap-bridge: ledger apply failed: {e} — stopping advance"
                                        );
                                        break;
                                    }
                                    replayed += 1;
                                    current_slot = next_slot.0;
                                    current_hash = next_hash;
                                }
                                Err(e) => {
                                    warn!(
                                        "Gap-bridge: failed to decode block at slot {}: {e}",
                                        next_slot.0
                                    );
                                    break;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            warn!("Gap-bridge: ChainDB read error: {e}");
                            break;
                        }
                    }
                }
                // Publish view after gap-bridge replay (#651 P2 / #652 P0).
                if replayed > 0 {
                    self.publish_ledger_view(&ls);
                }
                info!(
                    ledger_slot,
                    rollback_slot, replayed, "Gap-bridge: advanced ledger to meet rollback target"
                );
                // #985: the gap-bridge replays blocks straight into
                // `ledger_state` without producing LedgerSeq deltas, so the
                // seq's anchor and window now describe a chain the ledger has
                // moved off. Re-anchor before returning.
                drop(ls);
                self.reanchor_ledger_seq("gap-bridge replay").await;
                return true;
            }
        }

        // 1. LedgerSeq fast path (Subsystem 4).
        //
        // If the rollback target is in the live `LedgerSeq` volatile window,
        // restore the entire ledger state in O(n) by reverse-applying the
        // trailing UTxO diffs and replacing non-UTxO state with
        // `seq.tip_state()`.  No snapshot reload, no replay — matches
        // Haskell `LedgerDB.V2.LedgerSeq.rollbackToPoint`.
        //
        // Lock order: ledger_state before ledger_seq (per Node docs).
        {
            let mut ls = self.ledger_state.write().await;
            let mut seq = self.ledger_seq.write().await;
            if let Some(n) = ls.rollback_via_seq(&mut seq, rollback_point) {
                // Publish view after rollback (#651 P2 / #652 P0).
                self.publish_ledger_view(&ls);
                info!(
                    rollback_slot,
                    rolled_back_blocks = n,
                    new_tip_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0),
                    "LedgerSeq rollback: restored ledger via in-memory volatile window"
                );
                // Stale fork-tracking ratchet: if we just rolled past where the
                // mempool/forge tip thought we were, the live tip is now correct
                // and downstream callers (mempool TTL sweep, BlockFetch decision)
                // will pick up the new tip on their next iteration.
                return true;
            }
            // Else: fall through to the snapshot-reload slow path.  This
            // happens when:
            //   * The target is older than the seq's anchor (deep rollback
            //     beyond k blocks — protocol-impossible on the honest chain
            //     but already guarded above).
            //   * The seq was just rebuilt from a snapshot at startup and
            //     hasn't accumulated deltas yet (volatile window empty).
            //   * The target hash isn't in the seq (peer is on a divergent
            //     chain — caller's chain selection should reject it).
        }

        // 2. Full-state rollback via snapshot + replay (slow path).
        //
        // Used when the LedgerSeq fast path can't satisfy the rollback (target
        // outside the volatile window).  Restores ALL state fields — nonce
        // accumulators, delegations, rewards, governance, etc. — by loading
        // the best snapshot at or before the rollback point and replaying
        // forward.  O(snapshot_interval).
        {
            // 3. Slow path: reload from snapshot and replay to rollback point.
            //
            // Find the best ledger snapshot at or before the rollback point.
            // Try epoch-numbered snapshots first (newest that's <= rollback_slot),
            // then fall back to the latest snapshot.
            // Pass the ChainDB to verify that candidate snapshots are on the
            // canonical chain — fork snapshots must not be used as rollback
            // base states (they would corrupt UTxO state permanently).
            let best_snapshot = {
                let db = self.chain_db.read().await;
                self.find_best_snapshot_for_rollback(rollback_slot, Some(&*db))
            };

            if let Some(snapshot_path) = best_snapshot {
                match dugite_ledger::LedgerState::load_snapshot(&snapshot_path) {
                    Ok(snapshot_state) => {
                        let snapshot_slot =
                            snapshot_state.tip.point.slot().map(|s| s.0).unwrap_or(0);

                        // ─────────────────────────────────────────────────────
                        // CRITICAL: UTxO store must be rebuilt from the snapshot,
                        // NOT reused from the pre-rollback state.
                        //
                        // The previous approach (detach + re-attach the live store)
                        // was fundamentally broken:
                        //
                        //   1. The live store contains UTxOs from blocks BEYOND the
                        //      rollback point (the blocks we just rolled back).
                        //   2. Re-attaching it and then replaying snapshot→rollback
                        //      re-inserts outputs but never removes the stale outputs
                        //      from the rolled-back blocks.
                        //   3. The UTxO store permanently diverges from the canonical
                        //      chain: stale UTxOs (from rolled-back blocks) remain
                        //      forever because they are not tracked in any diff.
                        //   4. On subsequent blocks, inputs spending those stale UTxOs
                        //      succeed in our store when they should fail (double-spend
                        //      from our node's perspective) — or conversely, legitimate
                        //      inputs from blocks we haven't applied yet appear missing
                        //      because the live store's diff context is wrong.
                        //
                        // The CORRECT approach:
                        //   - If we have an LSM UTxO snapshot ("ledger") saved at or
                        //     near the ledger snapshot point, restore the UTxO store
                        //     from that snapshot.  It reflects the exact UTxO set at
                        //     the snapshot slot — no stale entries.
                        //   - Then replay ApplyOnly from snapshot_slot → rollback_slot
                        //     to add the blocks we need to re-apply.
                        //
                        // The "ledger" UTxO snapshot is written by save_utxo_snapshot()
                        // at the same time as each ledger snapshot, so they are always
                        // in sync.
                        //
                        // If no UTxO snapshot exists (e.g., in-memory mode or
                        // very first run), the bincode snapshot's in-memory
                        // UTxO set is used as a degraded fallback (see the
                        // else branch below).
                        // ─────────────────────────────────────────────────────
                        let utxo_store_path = self.database_path.join("utxo-store");

                        let mut ls = self.ledger_state.write().await;

                        // Restore the UTxO store using the live, already-open handle.
                        // Calling open_from_snapshot on the same path would fail with a
                        // lock conflict because this process already owns the session lock.
                        // restore_from_snapshot operates in-place without re-acquiring the
                        // lock, replacing the active runs with the named snapshot's files.
                        let has_store = utxo_store_path.exists() && ls.utxo.utxo_set.has_store();
                        if has_store {
                            match ls
                                .utxo
                                .utxo_set
                                .store_mut()
                                .unwrap()
                                .restore_from_snapshot("ledger")
                            {
                                Ok(()) => {
                                    ls.utxo.utxo_set.store_mut().unwrap().count_entries();
                                    ls.utxo
                                        .utxo_set
                                        .store_mut()
                                        .unwrap()
                                        .set_indexing_enabled(true);
                                    ls.utxo
                                        .utxo_set
                                        .store_mut()
                                        .unwrap()
                                        .rebuild_address_index();
                                    let utxos = ls.utxo.utxo_set.store().unwrap().len();
                                    // Replace all non-UTxO ledger state from the snapshot,
                                    // then re-attach the already-restored store.
                                    let store = ls.utxo.utxo_set.detach_store().unwrap();
                                    *ls = snapshot_state;
                                    ls.attach_utxo_store(store);
                                    info!(
                                        snapshot_slot,
                                        utxos,
                                        "UTxO store restored in-place from LSM snapshot for rollback"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "restore_from_snapshot failed, falling back to in-memory: {e}"
                                    );
                                    let _ = ls.utxo.utxo_set.detach_store();
                                    *ls = snapshot_state;
                                }
                            }
                        } else {
                            // No live LSM store — use bincode snapshot's in-memory UTxO set.
                            let _ = ls.utxo.utxo_set.detach_store();
                            *ls = snapshot_state;
                        }

                        let replay_from = snapshot_slot;

                        // Replay blocks from snapshot tip to rollback point.
                        // In-memory UTxOs from the snapshot are correct at snapshot_slot;
                        // LSM store has been restored from its matching snapshot.
                        // ApplyOnly mode correctly inserts all outputs without re-running
                        // validation, ensuring the UTxO set is canonical at rollback_slot.
                        let db = self.chain_db.read().await;
                        let mut current_slot = replay_from;
                        let mut replayed = 0u64;
                        // Point cursor (slot, hash): steps through same-slot
                        // Byron EBB/main pairs that a slot-only walk skips.
                        let mut current_hash = ls
                            .tip
                            .point
                            .hash()
                            .copied()
                            .unwrap_or(dugite_primitives::hash::Hash32::ZERO);
                        while current_slot < rollback_slot {
                            match db.get_next_block_after_point(
                                dugite_primitives::time::SlotNo(current_slot),
                                &current_hash,
                            ) {
                                Ok(Some((next_slot, next_hash, cbor))) => {
                                    if next_slot.0 > rollback_slot {
                                        break;
                                    }
                                    // Minimal decode: rollback replay uses ApplyOnly
                                    // mode, so witness-set data is never read.
                                    match dugite_serialization::decode_block_minimal_with_byron_epoch_length(&cbor, self.byron_epoch_length) {
                                        Ok(block) => {
                                            if let Err(e) = ls.apply_block(&block, BlockValidationMode::ApplyOnly) {
                                                error!(
                                                    slot = next_slot.0,
                                                    "Ledger apply failed during rollback replay: {e} — aborting replay"
                                                );
                                                break;
                                            }
                                            replayed += 1;
                                            current_slot = next_slot.0;
                                            current_hash = next_hash;
                                        }
                                        Err(e) => {
                                            warn!("Failed to decode block during replay: {e}");
                                            break;
                                        }
                                    }
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    warn!("Failed to read block during replay: {e}");
                                    break;
                                }
                            }
                        }
                        // Defense-in-depth (2026-05-29): the replay loop above
                        // breaks early on the first apply/decode error, which
                        // can leave the ledger far below `rollback_slot` (worst
                        // case: an epoch-0 snapshot whose replay broke
                        // immediately leaves the ledger at genesis).  Returning
                        // `true` here would tell the caller the ledger is at the
                        // rollback point when it is not — the TriggeredFork
                        // caller then applies fork blocks onto a wrong-tip ledger
                        // ("does not connect to tip") and clears the VolatileDB,
                        // an unrecoverable stall.  The primary defence is the
                        // `k`-cap in `VolatileDB::switch_chain` (this snapshot
                        // path is unreachable for in-window forks), but if we
                        // ever land here without reaching the target, report
                        // failure: the ledger now trails the chain, which the
                        // `rollback_slot > ledger_slot` gap-bridge above replays
                        // forward on the next event — recoverable, unlike
                        // `clear_volatile`.
                        let final_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                        if final_slot < rollback_slot {
                            // Publish whatever state we reached so downstream
                            // views are consistent with the ledger.
                            self.publish_ledger_view(&ls);
                            warn!(
                                snapshot_slot,
                                rollback_slot,
                                replayed,
                                final_slot,
                                "Snapshot rollback incomplete: ledger did not reach the \
                                 rollback target (replay broke early). Reporting failure so \
                                 the caller skips fork replay; the ledger trails the chain and \
                                 will be replayed forward on the next event."
                            );
                            return false;
                        }
                        // Publish view after snapshot-load + replay (#651 P2 / #652 P0).
                        self.publish_ledger_view(&ls);
                        info!(
                            snapshot_slot,
                            rollback_slot,
                            replayed,
                            snapshot = %snapshot_path.display(),
                            "Ledger state restored from snapshot and replayed to rollback point"
                        );
                    }
                    Err(e) => {
                        // Snapshot file present but unreadable (corruption,
                        // truncation, version skew).  Pre-Subsystem-4 we
                        // fell through to a genesis replay; that path was a
                        // multi-hour silent recovery that masked real disk
                        // integrity issues.  Now we surface the failure
                        // immediately — the operator should investigate the
                        // snapshot file (corrupted file is recoverable; a
                        // failing disk is not).
                        error!(
                            rollback_slot,
                            snapshot = %snapshot_path.display(),
                            "Failed to load ledger snapshot for rollback: {e}. \
                             Aborting rollback; ledger state preserved.  \
                             Operator should inspect the snapshot file or \
                             restart from a known-good Mithril import."
                        );
                        return false;
                    }
                }
            } else {
                // No canonical ledger snapshot found at or before the rollback
                // target.  Reachable only when:
                //   * No snapshots have been written yet (very early in a
                //     fresh sync), AND
                //   * The rollback target is outside the LedgerSeq volatile
                //     window (the seq fast path above didn't match).
                //
                // Both conditions together imply ChainDB integrity violation
                // or a peer advertising an off-chain target — neither is
                // recoverable by genesis replay (which historically masked
                // these conditions for hours).  Surface the error and let
                // the operator restart from a known-good Mithril import.
                let ledger_slot = self
                    .ledger_state
                    .read()
                    .await
                    .tip
                    .point
                    .slot()
                    .map(|s| s.0)
                    .unwrap_or(0);
                error!(
                    rollback_slot,
                    ledger_slot,
                    "Rollback target outside LedgerSeq volatile window AND no \
                     canonical snapshot available.  Aborting rollback; ledger \
                     state preserved.  Operator should restart from a Mithril \
                     import."
                );
                return false;
            }
        }

        // #985: reaching here means the snapshot slow path ran — it replaced
        // `ledger_state` wholesale from a snapshot and replayed forward, with
        // no LedgerSeq deltas produced. The seq's anchor and its entire window
        // now describe a chain the ledger is no longer on, so anything
        // reconstructed from them would be a chimera. Re-anchor on the state
        // we actually landed at.
        //
        // Placed after the slow-path block rather than inside it because every
        // failure mode in there returns early; falling through to this point
        // is exactly the "snapshot restored and replayed successfully" case.
        self.reanchor_ledger_seq("rollback via snapshot slow path")
            .await;

        // ── Phase 3: Update chain fragment on rollback ───────────────────────
        //
        // Roll back the chain fragment to the rollback point so that the
        // fragment stays in sync with the ChainDB.  Downstream ChainSync peers
        // that are following our chain will be sent a MsgRollBackward by the
        // `notify_rollback` call below; the fragment must reflect the new tip
        // before that happens so that subsequent `find_intersect` queries
        // return correct results.
        {
            let mut fragment = self.chain_fragment.write().await;
            fragment.rollback_to(rollback_point);
        }

        // 3. Re-validate mempool transactions against the rolled-back ledger state.
        //
        // `drain_all` clears the mempool completely (virtual UTxO set, dependency
        // graph, claimed inputs) before we re-examine each pending tx against the
        // rolled-back ledger.  After a rollback the UTxO set has changed, so a
        // clean slate avoids stale virtual-UTxO entries from rolled-back txs
        // corrupting the dependency graph.
        //
        // GOVERNANCE INVARIANT: use `validate_transaction_with_context` (not
        // `validate_transaction`) so that vote transactions whose referenced
        // `GovActionId` was rolled back (the proposal block was orphaned) are
        // correctly evicted rather than re-admitted.  The previous bare
        // `validate_transaction` call omitted the governance context, allowing
        // stale votes to re-enter the mempool.  dugite-bp then forged those votes
        // into the next block, which cardano-node correctly rejected with
        // `ConwayGovFailure (GovActionsDoNotExist …)`.
        //
        // Mirrors the governance check in `apply_fetched_block` (below) and
        // `post_block_apply_updates` (mod.rs), which already build `active_proposals`
        // from the live ledger state for the same purpose.
        let pending_txs = self.mempool.drain_all();
        let pending_count = pending_txs.len();
        if pending_count > 0 {
            let ledger = self.ledger_state.read().await;
            let current_slot = ledger.tip.point.slot().map(|s| s.0).unwrap_or(0);
            // Inject the live safe-zone horizon into the SlotConfig so the
            // post-rollback re-admission path also rejects past-horizon
            // Plutus txs (mirrors Haskell `TimeTranslationPastHorizon`).
            // Without this, a tx admitted before a rollback would silently
            // be re-admitted to the mempool even if its validity bound now
            // crosses the new horizon.
            let slot_config = {
                let mut sc = ledger.slot_config;
                let eh = self.era_history.read().await;
                if let Some(h) =
                    eh.safe_zone_horizon_slot(dugite_primitives::time::SlotNo(current_slot))
                {
                    sc.safe_zone_horizon_slot = Some(h);
                }
                sc
            };

            // Build governance context from the rolled-back ledger state so that
            // votes referencing proposals that were rolled back are rejected.
            // Matches `build_governance_validation_state` in serve.rs but
            // inlined here to avoid a cross-module dependency on that private fn.
            let active_proposals: std::collections::HashMap<
                dugite_primitives::transaction::GovActionId,
                dugite_ledger::validation::ActiveProposal,
            > = ledger
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
            let committee_hot_keys: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                ledger
                    .gov
                    .governance
                    .committee_hot_keys
                    .values()
                    .copied()
                    .collect();
            // Current members ∪ `members_to_add` of live UpdateCommittee
            // proposals — Haskell GOVCERT accepts a CommitteeHotAuth from a
            // potential FUTURE member too (`isPotentialFutureMember`).
            let committee_members: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                ledger.gov.governance.committee_auth_eligible_members();
            let committee_resigned: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                ledger
                    .gov
                    .governance
                    .committee_resigned
                    .keys()
                    .copied()
                    .collect();
            let committee_authorized_elected_hot_keys: std::collections::HashSet<
                dugite_primitives::hash::Hash32,
            > = ledger
                .gov
                .governance
                .committee_hot_keys
                .iter()
                .filter(|(cold, _)| {
                    ledger
                        .gov
                        .governance
                        .committee_expiration
                        .contains_key(*cold)
                })
                .map(|(_, hot)| *hot)
                .collect();
            let registered_pool_ids: std::collections::HashSet<dugite_primitives::hash::Hash28> =
                ledger.certs.pool_params.keys().copied().collect();
            let registered_drep_ids: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                ledger.gov.governance.dreps.keys().copied().collect();
            let node_network = ledger.node_network;

            let mut revalidated = 0u64;
            let mut evicted = 0u64;
            for tx in pending_txs {
                let tx_size = tx.raw_cbor.as_ref().map(|b| b.len() as u64).unwrap_or(0);
                let mut ctx = dugite_ledger::validation::ValidationContext::new()
                    .with_active_proposals(active_proposals.clone())
                    // Roots come from the ROLLED-BACK ledger, so a proposal
                    // whose parent was undone by the rollback is re-rejected
                    // rather than silently re-admitted.
                    .with_enacted_gov_roots(ledger.enacted_gov_roots())
                    .with_committee_authorized_hot_keys(committee_hot_keys.clone())
                    .with_committee_authorized_elected_hot_keys(
                        committee_authorized_elected_hot_keys.clone(),
                    )
                    .with_committee_members(committee_members.clone())
                    .with_committee_resigned(committee_resigned.clone())
                    .with_pools(registered_pool_ids.clone())
                    .with_dreps(registered_drep_ids.clone())
                    // Post-rollback the reward-account map may have shrunk
                    // (a rolled-back block's reward credit reverted). Pass
                    // the live `reward_accounts` so the
                    // `WithdrawalsNotInRewardsCERTS` check evicts any
                    // mempool tx whose declared withdrawal no longer
                    // matches the new balance. Same fix-pattern as
                    // serve.rs:LedgerTxValidator.
                    .with_reward_accounts_imbl(ledger.certs.reward_accounts.clone());
                if let Some(net) = node_network {
                    ctx = ctx.with_network(net);
                }
                if dugite_ledger::validation::validate_transaction_with_context(
                    &tx,
                    &ledger.utxo.utxo_set,
                    &ledger.epochs.protocol_params,
                    current_slot,
                    tx_size,
                    Some(&slot_config),
                    ctx,
                )
                .is_ok()
                {
                    let hash = tx.hash;
                    let size = tx.raw_cbor.as_ref().map(|b| b.len()).unwrap_or(0);
                    let fee = tx.body.fee;
                    let _ = self.mempool.add_tx_with_fee(hash, tx, size, fee);
                    revalidated += 1;
                } else {
                    evicted += 1;
                    debug!(
                        tx_hash = %tx.hash.to_hex(),
                        "Rollback re-validation: evicted mempool tx (failed gov/phase1 check)"
                    );
                }
            }
            info!(
                total = pending_count,
                revalidated, evicted, "Re-validated mempool txs after rollback"
            );
        }

        // 4. Notify peers
        self.notify_rollback(rollback_point).await;

        true
    }

    /// Process a batch of forward blocks: store in ChainDB, apply to ledger, validate, log progress.
    ///
    /// Returns the number of blocks successfully applied to the ledger (0 if the first block
    /// failed connectivity, indicating a state divergence that the caller should handle).
    #[allow(clippy::too_many_arguments, dead_code)] // retained for networking rewrite
    pub async fn process_forward_blocks(
        &mut self,
        mut blocks: Vec<dugite_primitives::block::Block>,
        tip: &dugite_primitives::block::Tip,
        ebb_hashes: &[EbbInfo],
        blocks_received: &mut u64,
        blocks_since_last_log: &mut u64,
        last_snapshot_epoch: &mut u64,
        last_log_time: &mut std::time::Instant,
        last_query_update: &mut std::time::Instant,
    ) -> u64 {
        if blocks.is_empty() {
            return 0;
        }

        // Genesis block validation: on the very first batch of blocks received
        // during initial sync, verify that the genesis block hash matches the
        // expected hash from the configuration. This prevents syncing from a
        // chain with a different genesis (wrong network).
        if !self.genesis_validated {
            if let Err(e) = self.validate_genesis_blocks(&blocks) {
                error!("Genesis block validation failed: {e}");
                return 0;
            }
            self.genesis_validated = true;
        }

        // Validate ALL block headers BEFORE storing.
        // Two-phase validation matching Haskell's cardano-node:
        //
        // During initial sync (non-strict), use Replay mode — skip all cryptographic
        // verification (VRF, KES, opcert Ed25519). This matches Haskell's
        // `reupdateChainDepState` behavior for blocks from the immutable chain.
        // Historical blocks are validated by hash-chain connectivity.
        //
        // At tip (strict), use Full mode with parallel crypto verification via rayon.
        // This matches Haskell's `updateChainDepState` for new network blocks.
        let strict = self.consensus.strict_verification();
        let mode = if strict {
            ValidationMode::Full
        } else {
            ValidationMode::Replay
        };

        // Auto-switch VolatileDB WAL durability mode based on at-tip state.
        //
        // During catch-up sync (`strict == false`) the VolatileDB WAL fsyncs
        // are pure throughput tax: blocks below the k-depth window are
        // re-fetchable from peers if the node crashes.  At tip
        // (`strict == true`) every write must be durable before the next is
        // appended so the producer can adopt the chain head safely.
        //
        // We track the last-set state on `self.volatile_wal_sync_at_tip`
        // to avoid taking the ChainDB write lock on the hot path when the
        // mode hasn't changed.
        let desired_sync_at_tip = strict;
        let prev_sync_at_tip = self
            .volatile_wal_sync_at_tip
            .load(std::sync::atomic::Ordering::Relaxed);
        if prev_sync_at_tip != desired_sync_at_tip {
            let mut db = self.chain_db.write().await;
            // Flush any blocks accumulated under the previous mode before
            // flipping — when transitioning from catch-up → at-tip this
            // makes the buffered tail durable; the reverse direction is
            // a no-op fsync, harmless.
            if let Err(e) = db.sync_volatile_wal() {
                warn!(error = %e, "VolatileDB WAL pre-transition fsync failed");
            }
            db.set_volatile_wal_sync_per_write(desired_sync_at_tip);
            self.volatile_wal_sync_at_tip
                .store(desired_sync_at_tip, std::sync::atomic::Ordering::Relaxed);
            info!(
                at_tip = desired_sync_at_tip,
                "VolatileDB WAL mode switched (per-write fsync = at_tip)"
            );
        }

        // Issue #545 E1: compute the wall-clock slot ONCE before the header
        // validation loop.  `validate_header_full` checks `header.slot >
        // current_slot`; the old code passed `block.slot()` as current_slot,
        // making the guard tautologically false (same value on both sides).
        //
        // Haskell's `updateChainDepState` obtains the current slot via
        // `getCurrentSlot` (wall clock), not from the candidate block.
        //
        // Fallback: if the wall-clock derivation fails (e.g. no Shelley genesis
        // loaded on a pure Byron node or very early in startup), fall back to the
        // ledger tip slot so the check is still meaningful even if not wall-clock
        // exact.  A block arriving whose slot is ahead of the tip is suspicious and
        // should be scrutinised; only a block in the far future is truly dangerous.
        let wall_clock_slot = self.current_wall_clock_slot().await;

        {
            // Read ledger state once for the whole batch
            let ls = self.ledger_state.read().await;

            // Per Praos spec, leader eligibility uses the "set" snapshot
            // (stake distribution from the previous epoch boundary).
            // Fall back to current pool_params if snapshots aren't available yet.
            let set_snapshot = ls.epochs.snapshots.set.as_ref();
            let total_active_stake: u64 = if let Some(snap) = set_snapshot {
                snap.pool_stake.values().map(|s| s.0).sum()
            } else {
                // During early sync, no snapshots exist yet — skip leader eligibility
                0
            };

            // Build overlay context for BFT schedule validation, via the same
            // predicate the live path uses (#985 — two hand-written copies of
            // this condition is how one of them came to be missing the era
            // term). This path is not currently wired
            // (`process_forward_blocks` has no callers); sharing the predicate
            // means re-wiring it cannot reintroduce the false-rejection wedge.
            let overlay_ctx = if blocks.first().is_some_and(|b| {
                super::should_build_overlay_context(
                    b.era,
                    ls.epochs.protocol_params.protocol_version_major,
                    ls.epochs.protocol_params.d.numerator,
                    !ls.genesis_delegates.is_empty(),
                )
            }) {
                let epoch = ls.epoch_of_slot(blocks.first().map(|b| b.slot().0).unwrap_or(0));
                let first_slot = ls.first_slot_of_epoch(epoch);
                let genesis_keys: std::collections::BTreeSet<dugite_primitives::hash::Hash28> =
                    ls.genesis_delegates.keys().copied().collect();
                Some(dugite_consensus::overlay::OverlayContext {
                    genesis_delegates: ls.genesis_delegates.clone(),
                    genesis_keys,
                    d: (
                        ls.epochs.protocol_params.d.numerator,
                        ls.epochs.protocol_params.d.denominator,
                    ),
                    first_slot_of_epoch: first_slot,
                })
            } else {
                None
            };

            // Phase 1: Sequential structural validation + state updates.
            // Uses Replay mode during sync (skip crypto) or Full mode at tip.
            // Opcert counter tracking and structural checks always run.
            for block in &blocks {
                if !block.era.is_shelley_based() {
                    continue;
                }

                // Populate epoch_nonce — the wire format does not include the nonce;
                // it must be injected from ledger state before VRF verification.
                //
                // A single `epoch_nonce` snapshot at batch-start is WRONG when the
                // batch spans an epoch boundary: the first block of the new epoch must
                // be validated with the NEW epoch's nonce (computed by the TICKN rule),
                // not the old one.  `epoch_nonce_for_slot` pre-computes the correct
                // nonce for any block that crosses into the immediately-next epoch,
                // mirroring the TICKN logic in `process_epoch_transition` without
                // mutating any state.  This fixes the "stale nonce after restart"
                // VRF failure that permanently blocked epoch transitions:
                //
                //   1. Node restarts, replays immutable blocks → strict verification on
                //   2. First live block is the first block of epoch E+1
                //   3. Old code used epoch E nonce → VRF failure → batch rejected
                //   4. Ledger never advanced → epoch E+1 nonce never computed → stuck
                let epoch_nonce = ls.epoch_nonce_for_slot(block.slot().0);
                let mut header_with_nonce = block.header.clone();
                header_with_nonce.epoch_nonce = epoch_nonce;

                // Look up pool registration for VRF key binding and leader eligibility.
                // Uses "set" snapshot for stake (per Praos spec), falls back to current
                // pool_params for VRF key binding if snapshot is not available.
                let pool_id = dugite_primitives::hash::blake2b_224(&block.header.issuer_vkey);
                let issuer_info = if !block.header.issuer_vkey.is_empty() {
                    // Try set snapshot first (correct per spec)
                    let pool_reg = set_snapshot
                        .and_then(|snap| snap.pool_params.get(&pool_id))
                        .or_else(|| ls.certs.pool_params.get(&pool_id));

                    pool_reg.map(|reg| {
                        if total_active_stake == 0 {
                            // Issue #545 E7: no stake snapshot yet (first ~3 epochs of sync).
                            // VRF key binding still runs (vrf_keyhash comparison below), but the
                            // leader threshold check is skipped — any registered pool passes.
                            // Haskell uses `MissingStake` when the snapshot entry is absent;
                            // we log a warning in strict mode so operators know the window exists.
                            if strict {
                                warn!(
                                    slot = block.slot().0,
                                    "Leader check: no stake snapshot available — skipping threshold \
                                     (stake = 1/1). This window covers the first ~3 epochs of sync."
                                );
                            }
                            return BlockIssuerInfo {
                                vrf_keyhash: reg.vrf_keyhash,
                                pool_stake: 1,
                                total_active_stake: 1,
                            };
                        }
                        let pool_stake = set_snapshot
                            .and_then(|snap| snap.pool_stake.get(&pool_id))
                            .map(|s| s.0)
                            .unwrap_or(0);
                        BlockIssuerInfo {
                            vrf_keyhash: reg.vrf_keyhash,
                            pool_stake,
                            total_active_stake,
                        }
                    })
                } else {
                    None
                };

                // Envelope checks (Haskell's `envelopeChecks`): body size and
                // optional header size against protocol parameter limits.
                // These are always fatal — no strict/non-strict bypass.
                if let Err(e) = self.consensus.validate_envelope(
                    block.slot(),
                    block.header.body_size,
                    None, // header CBOR size not available during ChainSync header processing
                    ls.epochs.protocol_params.max_block_body_size,
                    ls.epochs.protocol_params.max_block_header_size,
                ) {
                    error!(
                        slot = block.slot().0,
                        block_no = block.block_number().0,
                        "Envelope check failed: {e} — rejecting batch"
                    );
                    return 0;
                }

                // Issue #545 E1: use the wall-clock slot as `current_slot`.
                // Fall back to the ledger tip slot if the wall clock is unavailable,
                // and ultimately to the block's own slot (old, incorrect behaviour)
                // only as a last resort.  Using the block's slot makes the future-slot
                // guard tautologically false (`block.slot() > block.slot()` is always
                // false); using the wall clock matches Haskell's `getCurrentSlot`.
                let current_slot_for_check = wall_clock_slot
                    .or_else(|| ls.tip.point.slot())
                    .unwrap_or(block.slot());

                // Issue #655 P2.b — if this header was eagerly
                // validated against the same epoch, AND the operator
                // has enabled the flag, skip the apply-time re-check.
                // The eager pass already covered the same crypto
                // against the same snapshot pointer. Otherwise (default,
                // and any stale-epoch entry), fall through to the
                // authoritative re-check below.
                // Issue #655 P2.b — apply-time skip decision.
                let current_epoch = ls.epoch.0;
                let recorded_epoch = if self.skip_eagerly_validated_header_crypto {
                    // Only acquire the lock when the feature is on —
                    // zero-cost when off.
                    self.eagerly_validated_headers
                        .lock()
                        .get(block.hash())
                        .copied()
                } else {
                    None
                };
                let (skip_for_eager, should_remove) = decide_skip_apply_header_crypto(
                    self.skip_eagerly_validated_header_crypto,
                    current_epoch,
                    recorded_epoch,
                );
                if should_remove {
                    self.eagerly_validated_headers.lock().remove(block.hash());
                }
                if skip_for_eager {
                    tracing::trace!(
                        slot = block.slot().0,
                        epoch = current_epoch,
                        "issue #655: skipping apply-time validate_header_full \
                         (eager pass already validated against same epoch)"
                    );
                    continue;
                }

                if let Err(e) = self.consensus.validate_header_full(
                    &header_with_nonce,
                    current_slot_for_check,
                    issuer_info.as_ref(),
                    overlay_ctx.as_ref(),
                    mode,
                    Some(ls.epochs.protocol_params.protocol_version_major),
                    ls.tip.point.slot(),
                ) {
                    if strict {
                        error!(
                            slot = block.slot().0,
                            block_no = block.block_number().0,
                            "Consensus validation failed (strict): {e} — rejecting batch"
                        );
                        return 0;
                    }
                    warn!(
                        slot = block.slot().0,
                        block_no = block.block_number().0,
                        "Consensus validation: {e}"
                    );
                }
            }
        }

        let batch_count = blocks.len() as u64;

        // Build ChainDB batch data, taking ownership of raw_cbor to avoid cloning
        let db_batch: Vec<_> = blocks
            .iter_mut()
            .map(|block| {
                (
                    *block.hash(),
                    block.slot(),
                    block.block_number(),
                    *block.prev_hash(),
                    block.raw_cbor.take().unwrap_or_default(),
                )
            })
            .collect();

        // Disk-space back-pressure guard (issue #610).
        //
        // The disk monitor sets `ingestion_paused` when free space drops below
        // PAUSE_THRESHOLD_BYTES (1 GB); it is cleared only after RECOVER_THRESHOLD_BYTES
        // (5 GB) is sustained for 60 s.  Use the shared AtomicBool instead of the
        // watch-channel level so this check and `apply_fetched_block` react to the
        // same state machine and there is a single source of truth.
        if self
            .ingestion_paused
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            error!(
                batch_size = blocks.len(),
                "Disk ingestion paused — refusing to store block batch (disk space critically low)"
            );
            return 0;
        }

        // ── Phase 3: Store blocks to ChainDB FIRST, then apply to ledger ───
        //
        // At tip (strict mode), submit each block through the ChainSelQueue.
        // The queue writes to VolatileDB sequentially, matching the Haskell
        // `addBlockRunner` pattern.  For bulk sync (non-strict), keep the
        // existing batch write path for performance — the queue would be too
        // slow for 4M blocks during fast sync.
        //
        // In both cases, the ledger apply continues directly below (chain
        // selection is not yet fully live in the queue runner), and the
        // chain fragment is updated for each successfully stored block.
        if strict {
            // Live blocks at tip: route through ChainSelQueue for
            // Haskell-compatible sequential processing.
            if let Some(ref handle) = self.chain_sel_handle {
                for (hash, slot, block_no, prev_hash, cbor) in db_batch {
                    match handle
                        .submit_block(hash, slot, block_no, prev_hash, cbor)
                        .await
                    {
                        Some(dugite_storage::AddBlockResult::AddedAsTip { .. })
                        | Some(dugite_storage::AddBlockResult::StoredAsFork)
                        | Some(dugite_storage::AddBlockResult::AlreadyKnown) => {
                            // Block stored — proceed to ledger apply below.
                        }
                        Some(dugite_storage::AddBlockResult::TriggeredFork {
                            intersection_hash,
                            intersection_slot,
                            rollback,
                            apply,
                        }) => {
                            // Chain selection switched to a strictly-longer
                            // fork.  The VolatileDB.selected_chain is already
                            // on the new chain — but the ledger state is still
                            // on the OLD chain.  To restore consistency we
                            // must roll the ledger back to the intersection;
                            // the gap-bridging logic further down in this
                            // function will then replay the new fork's blocks
                            // from ChainDB (which now returns the new chain
                            // for `get_next_block_after_slot`).
                            //
                            // `intersection_slot` is pre-resolved by VolatileDB
                            // so we can build a proper `Point::Specific` without
                            // a second lookup (previously this fell back to
                            // `Point::Origin` when the intersection wasn't in
                            // volatile, which triggered the "refuse genesis
                            // reset" safety in `handle_ledger_rollback` and left the
                            // ledger stuck at the orphan tip — see #439).
                            //
                            // Haskell invariant (`Paths.hs::isReachable`): the
                            // intersection is always within the volatile
                            // window; if not, VolatileDB returns None from
                            // `switch_chain` and the block stays inert in
                            // volatile (`StoreButDontChange`).
                            info!(
                                intersection = %intersection_hash.to_hex(),
                                intersection_slot = intersection_slot.0,
                                slot = slot.0,
                                rollback_count = rollback.len(),
                                apply_count = apply.len(),
                                "Chain selection: fork switch — rolling back ledger to intersection"
                            );
                            self.metrics
                                .rollback_count
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                            let rollback_point = dugite_primitives::block::Point::Specific(
                                intersection_slot,
                                intersection_hash,
                            );
                            // VolatileDB's selected_chain has already been
                            // switched to the new fork by ChainSelQueue; use
                            // the ledger-only path so we don't undo the switch.
                            // In the bulk-sync path we do not short-circuit on
                            // rollback failure: the gap-bridging loop below will
                            // fail cleanly if the ledger is misaligned.
                            let _ = self.handle_ledger_rollback(&rollback_point).await;
                            // Block is stored; ledger is rolled back; the
                            // gap-bridging path further down will apply the
                            // new fork blocks in order.
                        }
                        Some(dugite_storage::AddBlockResult::Invalid(reason)) => {
                            error!(
                                slot = slot.0,
                                reason,
                                "FATAL: ChainSelQueue rejected live block — halting to prevent divergence"
                            );
                            return 0;
                        }
                        None => {
                            error!("FATAL: ChainSelQueue runner exited unexpectedly");
                            return 0;
                        }
                    }
                }
            } else {
                // Fallback: no handle, use batch write.
                let mut db = self.chain_db.write().await;
                if let Err(e) = db.add_blocks_batch(db_batch) {
                    error!(
                        "FATAL: Failed to store block batch: {e} — halting to prevent state divergence"
                    );
                    return 0;
                }
            }
        } else {
            // Bulk sync: keep the fast batch path.  ChainSelQueue overhead
            // (one round-trip per block through an async channel) would
            // reduce throughput from ~10K blk/s to ~1K blk/s or worse.
            let mut db = self.chain_db.write().await;
            if let Err(e) = db.add_blocks_batch(db_batch) {
                error!(
                    "FATAL: Failed to store block batch: {e} — halting to prevent state divergence"
                );
                return 0;
            }
        }

        // Compute the Limit on Eagerness (LoE) slot ceiling ONCE here, before
        // acquiring any other locks.  This value is used in two places:
        //
        // 1. The ledger apply loop below — blocks with slot > loe_slot are
        //    skipped so the ledger state cannot advance past the LoE boundary.
        //    They remain in VolatileDB and will be applied when the GSM later
        //    transitions to CaughtUp and the constraint is lifted.
        //
        // 2. The volatile→immutable flush at the end of this function, which
        //    similarly must not promote blocks beyond the LoE slot.
        //
        // In Praos mode (genesis disabled) the GSM starts in CaughtUp and
        // loe_limit() always returns None, so both paths take the fast branch
        // with zero overhead.
        let loe_limit: Option<u64> = self.gsm_snapshot_rx.borrow().loe_slot;

        // Now apply blocks to ledger — storage is confirmed
        let mut applied_count: u64 = 0;
        let mut collected_deltas: Vec<dugite_ledger::ledger_seq::LedgerDelta> = Vec::new();
        {
            let mut ls = self.ledger_state.write().await;
            let ledger_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            if !blocks.is_empty() {
                debug!(
                    batch_size = blocks.len(),
                    ledger_slot,
                    first_slot = blocks[0].slot().0,
                    first_block = blocks[0].block_number().0,
                    first_prev_hash = %blocks[0].prev_hash().to_hex(),
                    ledger_tip_hash = %ls.tip.point.hash().map(|h| h.to_hex()).unwrap_or_default(),
                    "Applying block batch to ledger"
                );
            }

            // Gap bridging: if the first unskipped block doesn't connect to the
            // ledger tip, try to replay intermediate blocks from ChainDB storage.
            // This handles the case where ChainDB is ahead of the ledger (e.g.,
            // after a crash mid-batch, or when blocks were stored but ledger
            // apply failed in a previous iteration).
            if let Some(first_new) = blocks.iter().find(|b| b.slot().0 > ledger_slot) {
                let ledger_tip_hash = ls.tip.point.hash().cloned();
                let first_prev = first_new.prev_hash();
                if ledger_tip_hash.as_ref() != Some(first_prev) {
                    debug!(
                        "Gap detected (ledger slot={}, first block slot={}) — bridging from ChainDB",
                        ledger_slot, first_new.slot().0,
                    );
                    let mut bridge_slot = ledger_slot;
                    // Point cursor: steps through same-slot Byron EBB/main
                    // pairs that a slot-only walk would skip.  Unknown/origin
                    // hashes fall back to the slot lookup inside ChainDB.
                    let mut bridge_hash =
                        ledger_tip_hash.unwrap_or(dugite_primitives::hash::Hash32::ZERO);
                    let target_slot = first_new.slot().0;
                    let mut bridged = 0u64;
                    let mut bridge_failed = false;
                    loop {
                        let block_data = {
                            let db = self.chain_db.read().await;
                            db.get_next_block_after_point(
                                dugite_primitives::time::SlotNo(bridge_slot),
                                &bridge_hash,
                            )
                        };
                        match block_data {
                            Ok(Some((next_slot, next_hash, cbor))) => {
                                if next_slot.0 >= target_slot {
                                    break; // Reached the incoming batch
                                }
                                // Minimal decode: gap-bridge replay uses ApplyOnly
                                // mode, so witness-set data is never read.
                                match dugite_serialization::decode_block_minimal_with_byron_epoch_length(&cbor, self.byron_epoch_length) {
                                    Ok(block) => {
                                        // Verify the block connects to the ledger tip
                                        // before applying.  ImmutableDB may contain
                                        // contaminated blocks from a prior fork that
                                        // was flushed on shutdown — skip those.
                                        let current_tip = ls.tip.point.hash().cloned();
                                        if current_tip.as_ref() != Some(block.prev_hash()) {
                                            debug!(
                                                slot = next_slot.0,
                                                expected = current_tip.map(|h| h.to_hex()).unwrap_or_default(),
                                                got = block.prev_hash().to_hex(),
                                                "Gap bridge: skipping non-connecting block (likely fork contamination)"
                                            );
                                            bridge_slot = next_slot.0;
                                            bridge_hash = next_hash;
                                            continue; // Skip, try next block
                                        }
                                        if let Err(e) = ls.apply_block(&block, BlockValidationMode::ApplyOnly) {
                                            warn!(
                                                slot = next_slot.0,
                                                "Gap bridge apply failed: {e} — \
                                                 ChainDB may have blocks from a different fork"
                                            );
                                            bridge_failed = true;
                                            break;
                                        }
                                        bridged += 1;
                                        bridge_slot = next_slot.0;
                                        bridge_hash = next_hash;
                                    }
                                    Err(e) => {
                                        warn!(slot = next_slot.0, error = %e, "Gap bridge decode failed");
                                        bridge_slot = next_slot.0;
                                        bridge_hash = next_hash;
                                    }
                                }
                            }
                            _ => break,
                        }
                    }
                    if bridged > 0 {
                        debug!("Bridged {bridged} blocks from ChainDB storage");
                    }
                    if bridge_failed {
                        // Gap bridge found a block that decoded but failed
                        // apply (not just a prev_hash mismatch).  Clear
                        // volatile and retry.
                        {
                            let mut db = self.chain_db.write().await;
                            let removed = db.volatile_block_count();
                            db.clear_volatile();
                            warn!(
                                removed,
                                "Gap bridge failed — cleared volatile DB. Re-syncing."
                            );
                        }
                        return 0;
                    }
                }
            }

            let ledger_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            let ledger_tip_hash = ls.tip.point.hash().cloned();
            for block in &blocks {
                // Skip blocks the ledger has already applied (e.g. replaying from origin).
                // After a rollback/fork, a block at the same slot but with a different
                // prev_hash must NOT be skipped — it belongs to the new fork.
                if block.slot().0 <= ledger_slot {
                    let is_fork_block = ledger_tip_hash
                        .as_ref()
                        .is_some_and(|tip_hash| tip_hash == block.prev_hash());
                    if !is_fork_block {
                        continue;
                    }
                }

                // LoE guard: when the Genesis State Machine is in PreSyncing or
                // Syncing state, do not apply blocks whose slot exceeds the LoE
                // ceiling.  Those blocks are already in VolatileDB (stored above)
                // and will be applied once the GSM transitions to CaughtUp.
                //
                // Because blocks are delivered in slot order, the first block that
                // exceeds the ceiling means all subsequent ones will too — break
                // rather than continue so we don't scan the rest of the batch.
                if let Some(loe_slot) = loe_limit {
                    if block.slot().0 > loe_slot {
                        debug!(
                            slot = block.slot().0,
                            loe_slot,
                            "LoE: deferring ledger application of blocks beyond LoE ceiling"
                        );
                        break;
                    }
                }

                // Byron EBB bridge: before applying a block, check if its
                // prev_hash references a Byron Epoch Boundary Block (EBB)
                // rather than the current ledger tip.  EBBs carry no transactions
                // and are never fetched via BlockFetch, so they are not in `blocks`.
                // Their hashes are tracked in `ebb_hashes` and used here to advance
                // the ledger tip before applying the block that follows the EBB.
                //
                // This handles mainnet Byron epochs 0-207: each epoch boundary
                // produces one EBB whose hash becomes the prev_hash of the first
                // real block of the next epoch.
                let current_tip_hash = ls.tip.point.hash().cloned();
                if current_tip_hash.as_ref() != Some(block.prev_hash()) {
                    // Check if this block's prev_hash matches any EBB in the batch.
                    let ebb_match = ebb_hashes
                        .iter()
                        .find(|ebb| ebb.hash == *block.prev_hash().as_bytes());
                    if let Some(ebb) = ebb_match {
                        use dugite_primitives::hash::Hash32;
                        let ebb_hash = Hash32::from_bytes(ebb.hash);
                        debug!(
                            ebb_hash = %ebb_hash.to_hex(),
                            block_slot = block.slot().0,
                            block_no = block.block_number().0,
                            "Advancing ledger tip through Byron EBB before block application"
                        );
                        if let Err(e) = ls.advance_past_ebb(ebb_hash) {
                            warn!(
                                slot = block.slot().0,
                                "EBB advance failed: {e} — skipping block"
                            );
                            break;
                        }
                    }
                }

                // Issue #545 E5 (#550): verify the block body matches the
                // header's `body_hash` claim before applying. Mirrors the
                // wire-in in `apply_fetched_block` for the bulk-sync path.
                // Uses the per-component `bbHash` algorithm from
                // `Cardano.Ledger.Alonzo.BlockBody`.
                if block.era.is_shelley_based() {
                    if let Some(raw_cbor) = block.raw_cbor.as_deref() {
                        if let Err(e) = dugite_consensus::praos::validate_block_body_hash(
                            &block.header,
                            raw_cbor,
                        ) {
                            error!(
                                slot = block.slot().0,
                                block_no = block.block_number().0,
                                hash = %block.hash().to_hex(),
                                error = %e,
                                "Bulk apply: block body hash verification failed — \
                                 stopping batch (substitution / corruption)"
                            );
                            break;
                        }
                    }
                }

                let ledger_mode = if strict || self.validate_all_blocks {
                    BlockValidationMode::ValidateAll
                } else {
                    BlockValidationMode::ApplyOnly
                };
                // Observability: track which mode each block was applied in
                // so operators can verify catch-up is using the fast path
                // (#698 — `dugite_apply_mode_reapply_total` vs
                // `dugite_apply_mode_validate_all_total`).
                match ledger_mode {
                    BlockValidationMode::ApplyOnly => self.metrics.inc_apply_mode_reapply(),
                    BlockValidationMode::ValidateAll => self.metrics.inc_apply_mode_validate_all(),
                }
                // #733: per-block apply-time phase-2 horizon snapshot,
                // anchored at the PRE-block ledger tip (conservative —
                // sound across HF windows). One-shot: consumed by this
                // apply. Lock order ls→era_history matches the era
                // transition propagation below.
                ls.phase2_apply_horizon = if matches!(ledger_mode, BlockValidationMode::ValidateAll)
                    && block.era >= dugite_primitives::era::Era::Babbage
                {
                    let pre_tip = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                    self.era_history
                        .read()
                        .await
                        .phase2_apply_horizon_slot(dugite_primitives::time::SlotNo(pre_tip))
                } else {
                    None
                };
                // Issue #653 — relief-worker scheduling around the
                // CPU-bound per-block apply inside the bulk batch loop.
                let apply_result =
                    tokio::task::block_in_place(|| ls.apply_block_with_delta(block, ledger_mode));
                match apply_result {
                    Ok(delta) => {
                        collected_deltas.push(delta);
                    }
                    Err(e) => {
                        error!(
                            slot = block.slot().0,
                            block_no = block.block_number().0,
                            hash = %block.hash().to_hex(),
                            "Failed to apply block to ledger: {e} — skipping remaining blocks in batch"
                        );
                        break;
                    }
                }
                // Consume pending era transition and propagate to the HFC state machine.
                if let Some((prev_era, new_era, epoch)) = ls.pending_era_transition.take() {
                    let mut eh = self.era_history.write().await;
                    if eh.current_era() < new_era {
                        eh.record_era_transition(new_era, epoch.0);
                        tracing::info!(
                            prev = %prev_era,
                            new = %new_era,
                            epoch = epoch.0,
                            "Era transition recorded in HFC era history",
                        );
                    }
                }
                applied_count += 1;
            }
            // Publish lock-free read view after the batch (#651 P2 / #652 P0)
            // so readers see the new tip without taking the ledger lock.
            if applied_count > 0 {
                self.publish_ledger_view(&ls);
            }
        }

        // Push collected deltas to LedgerSeq (after releasing ledger_state lock).
        if !collected_deltas.is_empty() {
            let mut seq = self.ledger_seq.write().await;
            for delta in collected_deltas {
                seq.push(delta);
            }
        }

        // Periodic VolatileDB WAL fsync while in catch-up mode.
        //
        // When `set_volatile_wal_sync_per_write(false)` is active each
        // `add_block` skips the per-block fsync.  Force a sync every ~1 s
        // so a crash mid-catch-up loses at most ~1 s of progress (which is
        // re-fetched from peers anyway).  No-op when at-tip — there
        // `sync_per_write == true` already makes every block durable.
        if !self
            .volatile_wal_sync_at_tip
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.last_volatile_wal_sync.elapsed() >= std::time::Duration::from_secs(1)
        {
            let mut db = self.chain_db.write().await;
            if let Err(e) = db.sync_volatile_wal() {
                warn!(error = %e, "VolatileDB WAL periodic fsync failed");
            }
            self.last_volatile_wal_sync = std::time::Instant::now();
        }

        // ── Phase 3: Update chain fragment for all applied blocks ────────────
        //
        // After the ledger apply loop, update the chain fragment with the
        // headers of blocks that were successfully applied.  This keeps the
        // fragment in sync with the selected chain so:
        //   1. ChainSync servers can find correct intersection points for
        //      downstream peers.
        //   2. The background copy-to-immutable can compare fragment.length()
        //      against k to decide when to flush to ImmutableDB.
        //
        // We use `applied_count` to only add headers for blocks that were
        // actually applied to the ledger, not the full batch (some may have
        // been skipped due to LoE or failed applies).
        if applied_count > 0 {
            let mut fragment = self.chain_fragment.write().await;
            let skip = blocks.len().saturating_sub(applied_count as usize);
            for block in blocks.iter().skip(skip) {
                fragment.push(block.header.clone());
            }
        }

        // ── Phase 5: Background maintenance operations ────────────────────────
        //
        // After updating the fragment, run the three background operations
        // that keep storage healthy.  These mirror Haskell's Background.hs:
        //
        // 1. copy_to_immutable — if fragment.len() > k, copy the oldest
        //    volatile block to ImmutableDB and advance the ledger anchor.
        // 2. gc_scheduler — remove expired volatile entries (60s delay).
        // 3. bg_snapshot_scheduler — take a ledger snapshot if warranted.
        //
        // We run these for EVERY batch (not just at tip) so the ImmutableDB
        // advances steadily during bulk sync and the GC queue drains promptly.
        // The copy-to-immutable check is O(1) (compare two integers) so the
        // overhead is negligible even during 10K blk/s bulk sync.
        if applied_count > 0 {
            // --- copy-to-immutable & GC ---
            // Get fragment metadata (oldest hash/slot/block_no) BEFORE
            // acquiring the ChainDB write lock, to avoid holding two locks.
            let fragment_info = {
                let frag = self.chain_fragment.read().await;
                if frag.length() > 0 {
                    // Oldest header (front of the deque)
                    frag.oldest_header()
                        .map(|h| (frag.length(), h.header_hash, h.slot, h.block_number))
                } else {
                    None
                }
            };

            if let Some((frag_len, oldest_hash, oldest_slot, oldest_block_no)) = fragment_info {
                let now = std::time::Instant::now();

                // Run copy-to-immutable + GC under a single ChainDB write lock.
                let copied = {
                    let mut db = self.chain_db.write().await;
                    // copy_to_immutable: moves oldest block if frag_len > k.
                    let copied = self
                        .copy_to_immutable
                        .run_once(
                            &mut db,
                            frag_len,
                            oldest_hash,
                            oldest_slot,
                            oldest_block_no,
                            &mut |_slot, _hash, _block_no| {
                                // LedgerSeq anchor advance and DiffSeq flush happen
                                // after this callback returns (below), since the
                                // callback is sync but our locks are async.
                            },
                        )
                        .unwrap_or_else(|e| {
                            warn!(error = %e, "background: copy-to-immutable failed");
                            None
                        });

                    // gc_scheduler: remove blocks past their 60s GC delay.
                    self.gc_scheduler.run_pending(&mut db, now);

                    copied
                };

                // If a block was copied, schedule it for GC after GC_DELAY.
                if let Some((gc_slot, gc_hash)) = copied {
                    self.gc_scheduler.schedule(gc_slot, gc_hash, now);
                    // The fragment's oldest header was promoted — pop it.
                    let mut frag = self.chain_fragment.write().await;
                    frag.pop_oldest();

                    // Advance LedgerSeq anchor — the oldest volatile delta
                    // is now immutable and can be absorbed into the anchor.
                    self.ledger_seq.write().await.advance_anchor();

                    // Flush DiffSeq entries for the now-immutable block.
                    // These diffs can never be rolled back, so keeping them
                    // wastes memory. Combined with push_bounded in apply_block,
                    // this ensures DiffSeq stays at most k entries.
                    let mut ls = self.ledger_state.write().await;
                    ls.utxo.diff_seq.flush_up_to(gc_slot);
                }
            }

            // --- snapshot scheduler ---
            // Check whether a snapshot should be taken.  Use the last applied
            // block's epoch + slot for the firing decision (#701: slot-based
            // trigger, not block-count).
            let last_applied = blocks.iter().rev().take(applied_count as usize).next();
            if let Some(last_block) = last_applied {
                let current_epoch = {
                    let ls = self.ledger_state.read().await;
                    ls.epoch
                };
                let block_slot = last_block.slot();
                let should_snapshot = {
                    self.bg_snapshot_scheduler
                        .maybe_snapshot_check(current_epoch, block_slot)
                };
                if should_snapshot {
                    // Issue #695: fire-and-forget via the background
                    // snapshot worker. Only record on `Enqueued`;
                    // skipping leaves the pending-deadline state in
                    // place so the next block retriggers.
                    if matches!(
                        self.try_snapshot_async().await,
                        super::snapshot_worker::SnapshotEnqueue::Enqueued
                    ) {
                        self.bg_snapshot_scheduler
                            .record_snapshot_taken(current_epoch, block_slot);
                    }
                }
            }
        }

        // Revalidate mempool transactions against the updated ledger state.
        //
        // Per the Haskell spec (pureSyncWithLedger → revalidateTxsFor → reapplyTxs),
        // ALL mempool txs are re-validated sequentially in FIFO order against the
        // new ticked ledger state. This naturally handles:
        //   - Confirmed txs (double-spend → removed)
        //   - Consumed input conflicts
        //   - TTL expiry
        //   - Cascading child removal (parent removed → child's input missing)
        //   - Any other validation rule changes
        if !self.mempool.is_empty() {
            // First remove confirmed txs by hash (fast path).
            let confirmed_hashes: Vec<_> = blocks
                .iter()
                .flat_map(|b| b.transactions.iter().map(|tx| tx.hash))
                .collect();
            if !confirmed_hashes.is_empty() {
                self.mempool.remove_txs(&confirmed_hashes);
            }

            // Full revalidation against the updated ledger state.
            // Build a set of consumed inputs for a quick first-pass check,
            // plus check TTL against the new tip slot.
            let consumed_inputs: std::collections::HashSet<_> = blocks
                .iter()
                .flat_map(|b| b.transactions.iter())
                .flat_map(|tx| tx.body.inputs.iter().cloned())
                .collect();
            let current_slot = blocks.last().map(|b| b.slot());

            // Also check if the tx's inputs exist in the on-chain UTxO set.
            // This catches chained txs whose parents were removed: their inputs
            // no longer exist in the UTxO set and mempool virtual UTxO.
            let ls = self.ledger_state.read().await;
            // Snapshot the currently-active gov action ids so we can drop any
            // mempool tx whose votes reference a `GovActionId` that no longer
            // exists in the proposals registry — typically because the action
            // was ratified-and-removed at the most-recent epoch boundary, or
            // expired. Without this, dugite's forge picks up such votes and
            // the resulting block is rejected by cardano-node with
            // `ConwayGovFailure (GovActionsDoNotExist …)`, stalling the
            // chain on every downstream Haskell observer.
            let active_action_ids: std::collections::HashSet<
                dugite_primitives::transaction::GovActionId,
            > = ls.gov.governance.proposals.keys().cloned().collect();
            self.mempool.revalidate_all(|tx| {
                // Reject if any input was consumed by the new block
                if tx
                    .body
                    .inputs
                    .iter()
                    .any(|input| consumed_inputs.contains(input))
                {
                    return false;
                }
                // Reject if TTL has expired (half-open: slot >= ttl means expired)
                if let (Some(ttl), Some(slot)) = (tx.body.ttl, current_slot) {
                    if slot.0 >= ttl.0 {
                        return false;
                    }
                }
                // Reject if any input is not in on-chain UTxO or mempool virtual UTxO.
                // This catches orphaned chained txs whose parents were removed.
                for input in &tx.body.inputs {
                    if !ls.utxo.utxo_set.contains(input)
                        && self.mempool.lookup_virtual_utxo(input).is_none()
                    {
                        return false;
                    }
                }
                // Reject if any vote references a gov action that no longer
                // exists. Same-tx proposals are admissible by definition (the
                // action enters the registry as part of this tx's apply step).
                if !tx.body.voting_procedures.is_empty() {
                    // Compute same-tx local action ids once per candidate.
                    let local_action_ids: std::collections::HashSet<
                        dugite_primitives::transaction::GovActionId,
                    > = (0..tx.body.proposal_procedures.len())
                        .map(|idx| dugite_primitives::transaction::GovActionId {
                            transaction_id: tx.hash,
                            action_index: idx as u32,
                        })
                        .collect();
                    for votes in tx.body.voting_procedures.values() {
                        for action_id in votes.keys() {
                            if !active_action_ids.contains(action_id)
                                && !local_action_ids.contains(action_id)
                            {
                                return false;
                            }
                        }
                    }
                }
                true
            });
            drop(ls);

            // Update mempool metrics immediately after revalidation so Prometheus
            // reflects tx removals (confirmed txs, TTL expiry, etc.) without
            // waiting for the periodic 5-second metric refresh.
            self.metrics.set_mempool_count(self.mempool.len() as u64);
            self.metrics.mempool_bytes.store(
                self.mempool.total_bytes() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }

        // Refresh governance gauges (proposal_count, drep_count, …) on every
        // block apply so Prometheus reflects the current ledger state immediately,
        // not on the periodic 5-second log-interval gate.  set_governance_snapshot
        // is a series of atomic stores — negligible cost even at bulk-sync rates.
        {
            let ls = self.ledger_state.read().await;
            self.metrics
                .set_governance_snapshot(&super::governance_snapshot_from_ledger(&ls));
        }

        if let Some(last_block) = blocks.last() {
            self.consensus.update_tip(last_block.tip());
        }

        // Flush finalized blocks from VolatileDB to ImmutableDB.
        //
        // Uses the same `loe_limit` computed before the ledger apply section.
        // When LoE is active the immutable tip cannot advance past the LoE
        // ceiling; blocks beyond that slot remain in VolatileDB (and were not
        // applied to the ledger above) until the GSM reaches CaughtUp.
        // In Praos mode (genesis disabled) `loe_limit` is always None.
        //
        // Flush finalized blocks from VolatileDB to ImmutableDB, then GC.
        //
        // This is split into batches of at most FLUSH_BATCH_SIZE blocks per
        // write-lock acquisition. Between batches we yield to the async
        // runtime so that other tasks (e.g. ChainSync server responding to
        // MsgFindIntersect on inbound N2N connections) can acquire read locks.
        // Without batching, the flush can hold the write lock for >10s during
        // bulk sync, causing Haskell peers to time out their ChainSync idle
        // timeout and drop the connection.
        const FLUSH_BATCH_SIZE: u64 = 50;
        loop {
            tokio::task::yield_now().await;
            let mut db = self.chain_db.write().await;
            // Finalisation is always k-based, in every consensus mode — the
            // Ouroboros Genesis LoE constrains chain SELECTION, never the
            // immutable flush (see run_background_maintenance + the
            // cardano-haskell-oracle cross-check). Gating this on loe_slot froze
            // the immutable tip during Byron PreSyncing → unbounded VolatileDB.
            let flush_result = db.flush_to_immutable_batch(FLUSH_BATCH_SIZE);
            match flush_result {
                Ok(0) => break, // No more to flush
                Ok(_flushed) => {
                    // More blocks may remain — release lock and re-acquire.
                    drop(db);
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, "Failed to flush blocks to immutable storage");
                    break;
                }
            }
        }
        {
            let mut db = self.chain_db.write().await;
            // GC orphaned fork blocks whose 60-second delay has expired.
            db.gc_volatile();
        }

        let tx_count: u64 = blocks.iter().map(|b| b.transactions.len() as u64).sum();

        *blocks_received += batch_count;
        *blocks_since_last_log += batch_count;
        self.snapshot_policy.record_blocks(batch_count);
        self.metrics.add_blocks_received(batch_count);
        self.metrics.record_block_received();
        self.metrics.record_roll_forward();
        self.metrics.add_blocks_applied(batch_count);
        self.metrics
            .transactions_received
            .fetch_add(tx_count, std::sync::atomic::Ordering::Relaxed);
        self.metrics
            .transactions_validated
            .fetch_add(tx_count, std::sync::atomic::Ordering::Relaxed);

        let last_block = blocks
            .last()
            // Safety: function returns early if blocks.is_empty()
            .expect("blocks is non-empty (checked at function entry)");
        let slot = last_block.slot().0;
        let block_no = last_block.block_number().0;
        self.metrics.set_slot(slot);
        self.metrics.set_block_number(block_no);

        // Log each new block when following the tip (individual blocks matter at tip)
        // and announce to connected downstream peers so they receive new blocks
        if strict {
            for block in &blocks {
                let hash_hex = block.hash().to_hex();
                info!(
                    era = %block.era,
                    slot = block.slot().0,
                    block = block.block_number().0,
                    txs = block.transactions.len(),
                    hash = %hash_hex,
                    "New block",
                );
            }

            // Announce the latest block to all connected N2N peers
            // This enables relay behavior: downstream peers waiting at tip (MsgAwaitReply)
            // will receive MsgRollForward for blocks we synced from upstream.
            //
            // `receiver_count()` is logged at debug level so we can correlate
            // sync-path announcement fan-out with forge-path announcement
            // fan-out when diagnosing propagation issues (#439).
            if let Some(ref tx) = self.block_announcement_tx {
                let mut hash_bytes = [0u8; 32];
                hash_bytes.copy_from_slice(last_block.hash().as_ref());
                let subscribers = tx.receiver_count();
                let _ = tx.send(dugite_network::BlockAnnouncement {
                    slot,
                    hash: hash_bytes,
                    block_number: block_no,
                });
                tracing::debug!(
                    slot,
                    block = block_no,
                    subscribers,
                    "sync: announced upstream block to peers"
                );
            }
        }

        {
            // Lock-free epoch read via the published view (#651 P2).
            let current_epoch = self.view().epoch.0;
            if current_epoch > *last_snapshot_epoch {
                // Count ALL epoch transitions (batches may span multiple epochs)
                let epochs_crossed = (current_epoch - *last_snapshot_epoch) as u32;
                info!(
                    epoch = current_epoch,
                    crossed = epochs_crossed,
                    "Epoch transition",
                );
                self.live_epoch_transitions =
                    self.live_epoch_transitions.saturating_add(epochs_crossed);

                // Finalize immutable chunk at epoch boundary and persist.
                // Pass the new epoch's parameters for Haskell-compatible
                // chunk naming and primary index generation.
                {
                    let (next_epoch_length, next_epoch_first_slot) = {
                        let eh = self.era_history.read().await;
                        let epoch_no = dugite_primitives::EpochNo(current_epoch);
                        let length = eh.epoch_size(epoch_no).unwrap_or(432_000);
                        let first_slot = eh.epoch_first_slot(epoch_no).map(|s| s.0).unwrap_or(0);
                        (length, first_slot)
                    };
                    let mut db = self.chain_db.write().await;
                    if let Err(e) = db.finalize_immutable_chunk(
                        current_epoch,
                        next_epoch_length,
                        next_epoch_first_slot,
                    ) {
                        warn!(error = %e, "Failed to finalize immutable chunk at epoch transition");
                    }
                    match db.persist() {
                        Ok(()) => info!(
                            epoch = current_epoch,
                            "ChainDB persisted at epoch transition"
                        ),
                        Err(e) => {
                            warn!(error = %e, "Failed to persist ChainDB at epoch transition")
                        }
                    }
                }
                if self.snapshot_policy.should_snapshot_normal() {
                    // Issue #695: non-blocking trigger; only reset the
                    // policy timer on `Enqueued` so a busy worker
                    // doesn't delay the next attempt by `k*2` seconds.
                    if matches!(
                        self.try_snapshot_async().await,
                        super::snapshot_worker::SnapshotEnqueue::Enqueued
                    ) {
                        self.snapshot_policy.snapshot_taken();
                    }
                }
                *last_snapshot_epoch = current_epoch;

                // Single read acquisition to cover both opcert pruning and
                // epoch-boundary mempool revalidation.  Combining these two
                // read-lock acquisitions into one eliminates the unlock/relock
                // round-trip and reduces contention with any concurrent writer
                // (e.g. the ledger-apply path above).
                //
                // The guard is held for the duration of the mempool closure
                // because the closure borrows `utxo_set` from it directly —
                // avoiding a potentially large clone of the UTxO map.
                {
                    let ledger = self.ledger_state.read().await;

                    // Prune opcert counters to only keep active pools (prevents
                    // unbounded growth as pools retire over epochs).
                    let active_pools: std::collections::HashSet<_> =
                        ledger.certs.pool_params.keys().copied().collect();
                    self.consensus.prune_opcert_counters(&active_pools);

                    // Update mempool capacity limits from the new epoch's protocol params.
                    //
                    // Haskell cardano-node sets mempool capacity to 2x the block's
                    // resource limits (`blockCapacityTxMeasure`).  Protocol params can
                    // change via governance actions at epoch boundaries, so we
                    // recalculate capacity here to stay in sync with the current limits.
                    // This must happen BEFORE revalidation so eviction uses the updated
                    // bounds when computing whether a tx still fits.
                    self.mempool.update_capacity_from_params(
                        ledger.epochs.protocol_params.max_block_body_size,
                        ledger.epochs.protocol_params.max_block_ex_units.mem,
                        ledger.epochs.protocol_params.max_block_ex_units.steps,
                    );

                    // Revalidate all mempool transactions against the new epoch's
                    // protocol parameters.  Protocol parameters can change at epoch
                    // boundaries (fee structure, max tx size, execution unit prices,
                    // etc.), so transactions that were valid in the previous epoch may
                    // now violate the new rules.  This mirrors Haskell cardano-node's
                    // epoch-boundary revalidation and is critical for block producers:
                    // forging a block with transactions that violate the new parameters
                    // would produce an invalid block.
                    if !self.mempool.is_empty() {
                        // Snapshot the scalar fields we need for the closure — these are
                        // cheap copies (params and slot_config are both small structs).
                        // We borrow utxo_set directly from the read-guard so we avoid
                        // cloning the potentially large UTxO map.
                        let new_params = ledger.epochs.protocol_params.clone();
                        let current_slot = ledger.tip.point.slot().map(|s| s.0).unwrap_or(0);
                        // Inject the per-tip safe-zone horizon so the
                        // epoch-boundary mempool revalidation also rejects
                        // any past-horizon Plutus tx (mirrors
                        // `TimeTranslationPastHorizon`). Without this, an
                        // epoch transition that *narrows* the horizon could
                        // leave a now-invalid tx in the mempool that
                        // dugite-bp would later forge into a Haskell-rejected
                        // block.
                        let slot_config = {
                            let mut sc = ledger.slot_config;
                            let eh = self.era_history.read().await;
                            if let Some(h) = eh.safe_zone_horizon_slot(
                                dugite_primitives::time::SlotNo(current_slot),
                            ) {
                                sc.safe_zone_horizon_slot = Some(h);
                            }
                            sc
                        };
                        let utxo_ref = &ledger.utxo.utxo_set;
                        let evicted = self.mempool.revalidate_all(|tx| {
                            let tx_size = tx.raw_cbor.as_ref().map(|b| b.len() as u64).unwrap_or(0);
                            dugite_ledger::validation::validate_transaction(
                                tx,
                                utxo_ref,
                                &new_params,
                                current_slot,
                                tx_size,
                                Some(&slot_config),
                            )
                            .is_ok()
                        });
                        if !evicted.is_empty() {
                            info!(
                                epoch = current_epoch,
                                evicted = evicted.len(),
                                remaining = self.mempool.len(),
                                "Epoch boundary: evicted mempool transactions that violate new protocol parameters",
                            );
                        } else {
                            debug!(
                                epoch = current_epoch,
                                "Epoch boundary: all mempool transactions valid under new protocol parameters",
                            );
                        }
                    }
                }
            }
        }

        let elapsed = last_log_time.elapsed();
        if elapsed.as_secs() >= 5 || *blocks_received <= 5 {
            let tip_slot = tip.point.slot().map(|s| s.0).unwrap_or(0);
            let tip_block = tip.block_number.0;
            let progress = if tip_slot > 0 {
                (slot as f64 / tip_slot as f64 * 100.0).min(100.0)
            } else {
                0.0
            };
            let blocks_per_sec = if elapsed.as_secs_f64() > 0.0 {
                *blocks_since_last_log as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            };
            let blocks_remaining = tip_block.saturating_sub(block_no);
            {
                let ls = self.ledger_state.read().await;
                self.metrics.set_epoch(ls.epoch.0);
                self.metrics.set_utxo_count(ls.utxo.utxo_set.len() as u64);
                self.metrics.set_sync_progress(progress);
                self.metrics.set_mempool_count(self.mempool.len() as u64);
                self.metrics.set_mempool_max(self.mempool.capacity() as u64);
                self.metrics.mempool_bytes.store(
                    self.mempool.total_bytes() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                {
                    let pm = self.peer_manager.read().await;
                    // Connected = warm + hot (both have live TCP connections).
                    self.metrics.peers_connected.store(
                        (pm.warm_peer_count() + pm.hot_peer_count()) as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics.peers_outbound.store(
                        pm.outbound_peer_count() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    let inbound_count = pm.inbound_peer_count() as u64;
                    self.metrics
                        .peers_inbound
                        .store(inbound_count, std::sync::atomic::Ordering::Relaxed);
                    // Duplex = peers with explicit duplex flag set (bidirectional
                    // mini-protocol bundles via InitiatorAndResponder diffusion mode).
                    self.metrics.peers_duplex.store(
                        pm.duplex_peer_count() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics.peers_cold.store(
                        pm.cold_peer_count() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics.peers_warm.store(
                        pm.warm_peer_count() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics.peers_hot.store(
                        pm.hot_peer_count() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );

                    // Connection manager counters (Haskell ConnectionManagerCounters compat).
                    // Uses per-connection state machine to compute overlapping counters
                    // matching Haskell's connectionStateToCounters exactly.
                    let cm_counters = pm.connection_manager_counters();
                    self.metrics.conn_full_duplex.store(
                        cm_counters.full_duplex,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics
                        .conn_duplex
                        .store(cm_counters.duplex, std::sync::atomic::Ordering::Relaxed);
                    self.metrics.conn_unidirectional.store(
                        cm_counters.unidirectional,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    self.metrics
                        .conn_inbound
                        .store(cm_counters.inbound, std::sync::atomic::Ordering::Relaxed);
                    self.metrics
                        .conn_outbound
                        .store(cm_counters.outbound, std::sync::atomic::Ordering::Relaxed);
                    self.metrics.conn_terminating.store(
                        cm_counters.terminating,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                self.metrics
                    .set_governance_snapshot(&super::governance_snapshot_from_ledger(&ls));
                // Era-aware tip-age computation (see slot_to_wallclock_ms doc).
                let slot_time_ms = self.slot_to_wallclock_ms(slot, &ls.slot_config).await;
                self.metrics.set_tip_slot_time_ms(slot_time_ms);
                // Update chainsync idle time
                self.metrics.update_chainsync_idle();
                // Only show sync progress when catching up, not when following the tip
                if blocks_remaining > 0 {
                    info!(
                        progress = format_args!("{progress:.2}%"),
                        epoch = ls.epoch.0,
                        block = block_no,
                        tip = tip_block,
                        remaining = blocks_remaining,
                        speed = format_args!("{} blk/s", blocks_per_sec as u64),
                        utxos = ls.utxo.utxo_set.len(),
                        "Syncing",
                    );
                }
            }
            *last_log_time = std::time::Instant::now();
            *blocks_since_last_log = 0;
            if last_query_update.elapsed().as_secs() >= 30 {
                self.update_query_state().await;
                // Recompute peer reputations periodically
                self.peer_manager.write().await.recompute_reputations();
                *last_query_update = std::time::Instant::now();
            }
        }

        applied_count
    }

    // NOTE: chain_sync_loop and its helper methods (Node::validate_genesis_blocks,
    // Node::extract_slot_from_wrapped_header) were deleted as part of the networking
    // layer rewrite. enable_strict_verification() logic now lives in Node::run()
    // (after replay) and the epoch transition path in apply_blocks_batch().
    // The new connection lifecycle manager (connection_lifecycle.rs) handles
    // per-peer ChainSync/BlockFetch tasks.
    // The free function validate_genesis_blocks() is retained for tests.
    // process_forward_blocks() is retained as the block application entry point.

    /// Replay blocks from local storage to catch the ledger up to the chain tip.
    ///
    /// After a Mithril snapshot import, ChainDB contains millions of blocks
    /// but the ledger state starts from genesis. This replays blocks locally
    /// (no network needed).
    ///
    /// Two replay modes:
    /// 1. **Chunk file replay** (fast path): If `immutable/` exists in the
    ///    database directory (left by Mithril import), reads blocks sequentially
    ///    from chunk files. This is ~100x faster than LSM lookups because chunk
    ///    files are laid out sequentially on disk.
    /// 2. **LSM replay** (fallback): Reads blocks by block number from the LSM tree.
    ///    Slower due to random I/O but works when chunk files aren't available.
    pub async fn replay_ledger_from_storage(&mut self, shutdown_rx: watch::Receiver<bool>) {
        // Migrate legacy immutable-replay/ to immutable/ (backwards compat)
        let legacy_dir = self.database_path.join("immutable-replay");
        let immutable_dir = self.database_path.join("immutable");
        if legacy_dir.is_dir() && !immutable_dir.is_dir() {
            debug!("Migrating legacy immutable-replay/ to immutable/");
            if let Err(e) = std::fs::rename(&legacy_dir, &immutable_dir) {
                warn!("Failed to migrate immutable-replay/ to immutable/: {e}");
            }
        }

        // Check for chunk files — ImmutableDB provides permanent historical
        // block storage from Mithril. Chunk files are NOT deleted after replay.
        let chunk_dir = if immutable_dir.is_dir() {
            Some(immutable_dir)
        } else if legacy_dir.is_dir() {
            Some(legacy_dir)
        } else {
            None
        };
        if let Some(ref dir) = chunk_dir {
            let ledger_slot = {
                let ls = self.ledger_state.read().await;
                ls.tip.point.slot().map(|s| s.0).unwrap_or(0)
            };
            // Only replay if the ledger hasn't caught up to the immutable tip
            let imm_tip_slot = self
                .chain_db
                .read()
                .await
                .get_tip()
                .point
                .slot()
                .map(|s| s.0)
                .unwrap_or(0);
            if ledger_slot < imm_tip_slot {
                info!(
                    ledger_slot,
                    immutable_tip_slot = imm_tip_slot,
                    "Replaying ledger from chunk files",
                );
                self.replay_from_chunk_files(dir, shutdown_rx.clone()).await;
                // Don't return — fall through to LSM replay check below.
                // Chunk files from Mithril may not cover blocks that were
                // previously synced by Dugite and flushed to ImmutableDB.
                // The LSM replay path handles those remaining blocks.
            }
        }

        let db_tip = self.chain_db.read().await.get_tip();
        // Lock-free tip-slot read via the published view (#651 P2).
        let ledger_slot = self.view().tip_slot();
        let db_tip_slot = db_tip.point.slot().map(|s| s.0).unwrap_or(0);

        if db_tip_slot <= ledger_slot {
            // #768: ledger tip STRICTLY ahead of the ChainDB tip = the ledger
            // snapshot is ahead of stored blocks (a Mithril import gap, or a
            // pre-#762 stranded DB). Normal (gap fills as peers deliver) — but if
            // it does NOT fill, the apply-stall watchdog (run loop) detects the
            // wedge and exits with an actionable error. Surface the gap at
            // startup so the condition is visible without waiting for the watchdog.
            if ledger_slot > db_tip_slot {
                warn!(
                    ledger_slot,
                    chaindb_tip_slot = db_tip_slot,
                    gap_slots = ledger_slot - db_tip_slot,
                    "Ledger tip is ahead of the ChainDB tip — sync will not advance \
                     until peers backfill the gap. If this persists (no apply progress), \
                     the database is stranded; re-import via `dugite-node mithril-import` (#768)."
                );
            }
            return; // Ledger is already caught up (or ahead — see warning above)
        }

        let blocks_behind = db_tip.block_number.0.saturating_sub({
            let ls = self.ledger_state.read().await;
            ls.tip.block_number.0
        });

        // Check if the user wants to limit replay via environment variable.
        let replay_limit: u64 = std::env::var("DUGITE_REPLAY_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(u64::MAX);

        if blocks_behind > replay_limit {
            warn!(
                blocks_behind,
                replay_limit,
                db_tip_slot,
                ledger_slot,
                "Skipping ledger replay: gap exceeds DUGITE_REPLAY_LIMIT. \
                 Set DUGITE_REPLAY_LIMIT to a higher value or remove it to replay all blocks."
            );
            return;
        }

        if blocks_behind > 100_000 {
            info!(blocks_behind, "Replaying blocks (time-based snapshots)",);
        }

        info!(
            ledger_slot,
            db_tip_slot, blocks_behind, "Replaying ledger from ChainDB (LSM mode)",
        );
        self.replay_from_lsm(db_tip, shutdown_rx).await;
    }

    /// Fast replay: read blocks sequentially from chunk files.
    ///
    /// Runs in a blocking thread since chunk file I/O and ledger application
    /// are CPU-bound synchronous work.
    async fn replay_from_chunk_files(
        &self,
        replay_dir: &std::path::Path,
        shutdown_rx: watch::Receiver<bool>,
    ) {
        let ledger_state = self.ledger_state.clone();
        let snapshot_path = self.database_path.join("ledger-snapshot.bin");
        let replay_dir = replay_dir.to_path_buf();
        let bel = self.byron_epoch_length;
        let metrics = self.metrics.clone();

        let security_param = self
            .shelley_genesis
            .as_ref()
            .map(|g| g.security_param)
            .unwrap_or(2160);
        let imm_tip_slot = self
            .chain_db
            .read()
            .await
            .get_tip()
            .point
            .slot()
            .map(|s| s.0)
            .unwrap_or(0);
        let result = tokio::task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let mut replayed = 0u64;
            let mut skipped = 0u64;
            let mut last_log = std::time::Instant::now();
            // Issue #695: no mid-replay snapshot policy needed —
            // chunk replay produces no intermediate snapshots; a
            // single save fires after the loop exits.
            let _ = security_param;

            // Get ledger tip slot so we can skip blocks already applied.
            let ledger_tip_slot = {
                let ls = ledger_state.blocking_read();
                info!(
                    ledger_tip_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0),
                    utxos = ls.utxo.utxo_set.len(),
                    "Chunk replay starting",
                );
                ls.tip.point.slot().map(|s| s.0).unwrap_or(0)
            };

            // Disable address index and full stake rebuild during replay.
            // Address index is never queried during replay, and the O(n)
            // retain per remove is expensive. Both are rebuilt at the end.
            // Incremental stake tracking is correct during sequential replay.
            {
                let mut ls = ledger_state.blocking_write();
                ls.utxo.utxo_set.set_indexing_enabled(false);
                ls.utxo.utxo_set.set_wal_enabled(false); // WAL disabled during replay for speed
                ls.epochs.needs_stake_rebuild = false;
            }

            // Pass ledger_tip_slot for #502 chunk-skip optimization.
            // When the gap is small (typical after Mithril import), this
            // binary-searches the chunk list rather than iterating every
            // entry from genesis just to find ~20 blocks to apply.
            let result = crate::mithril::replay_from_chunk_files(&replay_dir, ledger_tip_slot, bel, |cbor| {
                // Check shutdown every 1000 blocks
                if replayed.is_multiple_of(1000) && *shutdown_rx.borrow() {
                    info!("Shutdown requested during chunk replay at block {replayed}");
                    return Err(anyhow::anyhow!("shutdown requested"));
                }

                // Minimal decode: chunk replay always uses ApplyOnly mode.
                // Skipping witness-set parsing is the primary replay speedup:
                // the witness set (vkey witnesses, scripts, redeemers, Plutus
                // data) is the largest per-tx allocation and is never read by
                // the ledger during ApplyOnly block application.
                match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                    cbor, bel,
                ) {
                    Ok(block) => {
                        // Skip blocks already applied (at or before the ledger tip).
                        // Use strict < so that genesis block (slot 0) is NOT skipped
                        // when the ledger starts fresh (tip slot = 0).
                        if ledger_tip_slot > 0 && block.slot().0 <= ledger_tip_slot {
                            skipped += 1;
                            return Ok(());
                        }

                        let mut ls_guard = ledger_state.blocking_write();
                        if let Err(e) =
                            ls_guard.apply_block(&block, BlockValidationMode::ApplyOnly)
                        {
                            warn!(slot = block.slot().0, error = %e, "Ledger apply failed during replay");
                        }
                        replayed += 1;

                        if last_log.elapsed().as_secs() >= 5 {
                            let elapsed = start.elapsed().as_secs_f64();
                            let speed = replayed as f64 / elapsed;
                            let slot = ls_guard.tip.point.slot().map(|s| s.0).unwrap_or(0);
                            let utxos = ls_guard.utxo.utxo_set.len();
                            let pct = if imm_tip_slot > 0 {
                                slot as f64 / imm_tip_slot as f64 * 100.0
                            } else {
                                0.0
                            };
                            // Update Prometheus metric so TUI/monitoring can track replay progress
                            metrics.set_sync_progress(pct);
                            metrics.set_slot(slot);
                            metrics.set_block_number(ls_guard.tip.block_number.0);
                            metrics.set_epoch(ls_guard.epoch.0);
                            info!(
                                progress = format_args!("{pct:>6.2}%"),
                                blocks = replayed,
                                slot,
                                speed = format_args!("{speed:.0} blk/s"),
                                utxos,
                                "Replay",
                            );
                            last_log = std::time::Instant::now();
                        }

                        // Issue #695: no snapshots during chunk-file
                        // replay. Mirrors Haskell cardano-node, which
                        // does not launch the snapshot background task
                        // until `replayStartingWith` returns. The
                        // existing post-loop save (below) writes a
                        // single snapshot once replay completes.
                    }
                    Err(e) => {
                        // #680 diagnostic: dump exact slice characteristics
                        // so we can compare against the probe_block tool.
                        let first16: String = cbor
                            .iter()
                            .take(24)
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        warn!(
                            "Failed to decode block during chunk replay: {e} \
                             (slice_len={} first24={first16})",
                            cbor.len()
                        );
                    }
                }
                Ok(())
            });

            match &result {
                Ok(total) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        replayed as f64 / elapsed
                    } else {
                        0.0
                    };
                    info!(
                        "Replay       complete ({} blocks in {}s, {} applied, {} skipped, {} blk/s)",
                        total, elapsed as u64, replayed, skipped, speed as u64,
                    );
                    // Update metrics with final replay state — include all
                    // counters so governance/state metrics are available
                    // immediately without requiring a node restart (#329).
                    let ls = ledger_state.blocking_read();
                    let slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                    metrics.set_slot(slot);
                    metrics.set_block_number(ls.tip.block_number.0);
                    metrics.set_epoch(ls.epoch.0);
                    metrics.set_utxo_count(ls.utxo.utxo_set.len() as u64);
                    metrics
                        .set_governance_snapshot(&super::governance_snapshot_from_ledger(&ls));
                }
                Err(e) => {
                    // "shutdown requested" is not an error — it's a normal
                    // interruption when Ctrl+C is pressed during replay.
                    let msg = e.to_string();
                    if msg.contains("shutdown") {
                        warn!("Chunk-file replay interrupted: {e}");
                    } else {
                        error!("Chunk-file replay failed: {e}");
                    }
                }
            }

            // Re-enable address indexing and rebuild the index
            {
                let mut ls = ledger_state.blocking_write();
                ls.utxo.utxo_set.set_wal_enabled(true); // Re-enable WAL after replay
                ls.utxo.utxo_set.set_indexing_enabled(true);
                info!("Post-replay: rebuilding address index");
                ls.utxo.utxo_set.rebuild_address_index();
                // Rebuild stake distribution from the full UTxO set to correct any
                // residual state from the pre-replay snapshot. After this single
                // rebuild, incremental tracking is accurate and needs_stake_rebuild
                // self-disables at the next epoch boundary.
                info!("Post-replay: rebuilding stake distribution");
                ls.epochs.needs_stake_rebuild = true;
                ls.rebuild_stake_distribution();
                // Recompute pool_stake for all mark/set/go snapshots using the
                // freshly rebuilt stake_distribution and current reward_accounts.
                info!("Post-replay: recomputing snapshot pool stakes");
                ls.recompute_snapshot_pool_stakes();
                debug!("Rebuilt address index, stake distribution, and snapshot pool stakes after chunk replay");
            }

            // Save final snapshot (write lock to flush UTxO store — no WAL)
            {
                let mut ls = ledger_state.blocking_write();
                info!("Post-replay: saving UTxO snapshot");
                if let Err(e) = ls.save_utxo_snapshot() {
                    error!("Failed to save UTxO store after replay: {e}");
                }
                info!(
                    "Post-replay: saving ledger snapshot to {}",
                    snapshot_path.display()
                );
                if let Err(e) = ls.save_snapshot(&snapshot_path) {
                    error!("Failed to save ledger snapshot after replay: {e}");
                }
            }
            info!("Post-replay: initialization complete");

            result
        })
        .await;

        if let Err(e) = result {
            error!("Chunk-file replay task panicked: {e}");
        }

        // Issue #742 defense-in-depth: publish the post-chunk-replay view so
        // that per-peer forecast-park tasks see the updated tip immediately.
        // Primary publish happens at the Node::run replay→live handover; this
        // ensures coverage for future callers that skip that step.
        self.publish_view_now().await;
    }

    /// Fallback replay: read blocks from ChainDB using slot-based iteration.
    ///
    /// Uses `get_next_block_after_slot()` which queries both ImmutableDB and
    /// VolatileDB, making it correct after restart even when blocks have been
    /// flushed from the VolatileDB WAL into ImmutableDB chunk files.
    ///
    /// The previous implementation used `get_block_by_number()` (block-number
    /// index) which only queried the VolatileDB in-memory index.  After a
    /// clean restart the VolatileDB WAL is empty, so any blocks that had been
    /// flushed to ImmutableDB were invisible to the replay — resulting in
    /// "Block not found in ChainDB during replay block_no=NNNN" and 0 blocks
    /// applied, leaving the ledger stuck at the fork snapshot tip.
    async fn replay_from_lsm(
        &mut self,
        db_tip: dugite_primitives::block::Tip,
        shutdown_rx: watch::Receiver<bool>,
    ) {
        let start = std::time::Instant::now();
        let mut replayed = 0u64;
        let mut last_log = std::time::Instant::now();
        let snapshot_path = self.database_path.join("ledger-snapshot.bin");

        // ── Fork-snapshot detection and recovery ──────────────────────────
        //
        // The ledger snapshot loaded at startup may be on a dead fork.  This
        // happens when the BP forged a block (which entered VolatileDB and
        // triggered a snapshot) but the network did NOT adopt the block.  On
        // the next restart:
        //   - The forged block is no longer in VolatileDB (WAL empty).
        //   - The snapshot tip is in the "volatile region" (above ImmutableDB tip),
        //     so the startup canonicality check provisionally accepts it.
        //   - When replay tries to apply the NEXT canonical block, it fails with
        //     "does not connect to tip: expected <fork_hash>, got <canonical_hash>".
        //
        // Haskell's `LedgerDB.Init.initLedgerDB` handles this by rolling back to
        // the youngest snapshot whose tip IS on the current chain fragment.  We
        // replicate that: BEFORE starting the replay loop, check if the ledger's
        // current tip hash is the expected predecessor of the next canonical block
        // in ChainDB.  If not, roll back to the best canonical snapshot and update
        // start_slot accordingly.
        //
        // The check is: look up the canonical block at or after start_slot.  Its
        // prev_hash should equal the ledger's current tip hash.  If it doesn't,
        // the ledger is on a fork and we must roll back.
        let ledger_on_fork = {
            let ls = self.ledger_state.read().await;
            let ledger_tip_hash = ls.tip.point.hash().copied();
            let ledger_tip_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            drop(ls);

            if ledger_tip_slot == 0 {
                // At genesis — always canonical, no check needed.
                false
            } else {
                // Point lookup: when the ledger tip is a Byron EBB, the next
                // canonical block is the same-slot main block — a slot-only
                // probe would skip it and compare against the WRONG successor,
                // mis-diagnosing a fork.
                let db = self.chain_db.read().await;
                let next_block = db.get_next_block_after_point(
                    dugite_primitives::time::SlotNo(ledger_tip_slot),
                    &ledger_tip_hash.unwrap_or(dugite_primitives::hash::Hash32::ZERO),
                );
                drop(db);

                match (next_block, ledger_tip_hash) {
                    (Ok(Some((_slot, _hash, cbor))), Some(expected_prev)) => {
                        // Decode just enough to get prev_hash.
                        match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                            &cbor,
                            self.byron_epoch_length,
                        ) {
                            Ok(block) => {
                                let actual_prev = *block.prev_hash();
                                if actual_prev != expected_prev {
                                    warn!(
                                        ledger_tip_slot,
                                        ledger_tip_hash = %expected_prev.to_hex(),
                                        next_block_prev_hash = %actual_prev.to_hex(),
                                        "LSM replay: ledger is on a dead fork — \
                                         rolling back to last canonical snapshot \
                                         (Haskell: initLedgerDB)"
                                    );
                                    true
                                } else {
                                    false
                                }
                            }
                            Err(_) => false, // Can't decode; proceed and let apply_block handle it
                        }
                    }
                    _ => false, // No next block or no ledger tip hash — can't detect fork
                }
            }
        };

        if ledger_on_fork {
            // Roll back to the last canonical snapshot.  Use the existing
            // `find_best_snapshot_for_rollback` path (same as handle_ledger_rollback)
            // which correctly verifies canonicality before selecting a snapshot.
            let ledger_tip_slot = {
                self.ledger_state
                    .read()
                    .await
                    .tip
                    .point
                    .slot()
                    .map(|s| s.0)
                    .unwrap_or(0)
            };
            // Pass `ledger_tip_slot - 1` as the rollback target so that the
            // fork snapshot itself (at ledger_tip_slot) is excluded by the
            // `snap_slot <= rollback_slot` filter in find_best_snapshot_for_rollback.
            // This guarantees we pick an *earlier* snapshot whose slot is strictly
            // before the fork point.  Any such snapshot is either in the ImmutableDB
            // range (definitely canonical) or it too will fail the canonicality
            // check and be skipped.
            let rollback_target = ledger_tip_slot.saturating_sub(1);
            let best_snapshot = {
                let db = self.chain_db.read().await;
                self.find_best_snapshot_for_rollback(rollback_target, Some(&*db))
            };

            match best_snapshot {
                Some(snapshot_path_local) => {
                    match dugite_ledger::LedgerState::load_snapshot(&snapshot_path_local) {
                        Ok(snapshot_state) => {
                            let snapshot_slot =
                                snapshot_state.tip.point.slot().map(|s| s.0).unwrap_or(0);

                            // Restore UTxO store from the matching LSM snapshot.
                            let utxo_store_path = self.database_path.join("utxo-store");
                            let mut ls = self.ledger_state.write().await;
                            let has_store =
                                utxo_store_path.exists() && ls.utxo.utxo_set.has_store();
                            if has_store {
                                match ls
                                    .utxo
                                    .utxo_set
                                    .store_mut()
                                    .unwrap()
                                    .restore_from_snapshot("ledger")
                                {
                                    Ok(()) => {
                                        ls.utxo.utxo_set.store_mut().unwrap().count_entries();
                                        ls.utxo
                                            .utxo_set
                                            .store_mut()
                                            .unwrap()
                                            .set_indexing_enabled(true);
                                        ls.utxo
                                            .utxo_set
                                            .store_mut()
                                            .unwrap()
                                            .rebuild_address_index();
                                        let utxos = ls.utxo.utxo_set.store().unwrap().len();
                                        let store = ls.utxo.utxo_set.detach_store().unwrap();
                                        *ls = snapshot_state;
                                        ls.attach_utxo_store(store);
                                        info!(
                                            fork_slot = ledger_tip_slot,
                                            recovered_slot = snapshot_slot,
                                            utxos,
                                            "LSM replay: fork-rollback complete, \
                                             UTxO store restored from LSM snapshot"
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            "LSM replay: UTxO restore_from_snapshot failed \
                                             during fork-rollback: {e} — using in-memory state"
                                        );
                                        let _ = ls.utxo.utxo_set.detach_store();
                                        *ls = snapshot_state;
                                        info!(
                                            fork_slot = ledger_tip_slot,
                                            recovered_slot = snapshot_slot,
                                            "LSM replay: fork-rollback complete (in-memory)"
                                        );
                                    }
                                }
                            } else {
                                let _ = ls.utxo.utxo_set.detach_store();
                                *ls = snapshot_state;
                                info!(
                                    fork_slot = ledger_tip_slot,
                                    recovered_slot = snapshot_slot,
                                    "LSM replay: fork-rollback complete (no LSM store)"
                                );
                            }
                        }
                        Err(e) => {
                            error!(
                                "LSM replay: fork-rollback failed to load snapshot: {e}. \
                                 Replay will likely fail. \
                                 Consider deleting the database and re-importing via mithril-import."
                            );
                        }
                    }
                }
                None => {
                    error!(
                        "LSM replay: ledger is on a dead fork but no canonical snapshot found. \
                         Replay will likely fail with hash-mismatch errors. \
                         Consider deleting the database and re-importing via mithril-import."
                    );
                }
            }
        }

        // Determine the slot range to replay: from the current ledger tip to
        // the ChainDB tip.  Use slots rather than block numbers — block numbers
        // are only indexed in the VolatileDB (which is empty after restart),
        // but slot-based lookup (get_next_block_after_slot) queries both
        // ImmutableDB and VolatileDB and so works correctly at all times.
        let (start_slot, end_slot) = {
            let mut ls = self.ledger_state.write().await;
            ls.utxo.utxo_set.set_indexing_enabled(false);
            ls.utxo.utxo_set.set_wal_enabled(false); // WAL disabled during replay for speed
            ls.epochs.needs_stake_rebuild = false;
            let start = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            let end = db_tip.point.slot().map(|s| s.0).unwrap_or(0);
            (start, end)
        };

        if start_slot >= end_slot {
            info!(
                start_slot,
                end_slot, "LSM replay: nothing to replay (ledger tip >= ChainDB tip)"
            );
        } else {
            info!(
                ledger_slot = start_slot,
                db_tip_slot = end_slot,
                blocks_behind = {
                    // Rough estimate — block_number not available until we replay.
                    // Lock-free read via the published view (#651 P2).
                    db_tip
                        .block_number
                        .0
                        .saturating_sub(self.view().tip.block_number.0)
                },
                "Replaying ledger from ChainDB (slot-based)",
            );
        }

        let mut current_slot = start_slot;
        // Point cursor: steps through same-slot Byron EBB/main pairs that a
        // slot-only walk skips.  Origin/unknown hashes fall back to the
        // slot-based lookup inside ChainDB.
        let mut current_hash = {
            let ls = self.ledger_state.read().await;
            ls.tip
                .point
                .hash()
                .copied()
                .unwrap_or(dugite_primitives::hash::Hash32::ZERO)
        };
        loop {
            // Check shutdown every 1000 blocks
            if replayed.is_multiple_of(1000) && replayed > 0 && *shutdown_rx.borrow() {
                info!(
                    replayed,
                    current_slot, "Shutdown requested during LSM replay, saving snapshot"
                );
                let mut ls = self.ledger_state.write().await;
                merge_opcert_counters_from_praos(
                    &mut ls.consensus.opcert_counters,
                    self.consensus.opcert_counters(),
                );
                if let Err(e) = ls.save_snapshot(&snapshot_path) {
                    warn!("Failed to save snapshot on shutdown: {e}");
                }
                break;
            }

            let block_data = {
                let db = self.chain_db.read().await;
                db.get_next_block_after_point(
                    dugite_primitives::time::SlotNo(current_slot),
                    &current_hash,
                )
            };

            match block_data {
                Ok(Some((next_slot, next_hash, cbor))) => {
                    // Stop once we have replayed up to and including the target slot.
                    if next_slot.0 > end_slot {
                        break;
                    }

                    // Minimal decode: LSM replay always uses ApplyOnly mode;
                    // witness-set fields are never accessed.
                    match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                        &cbor,
                        self.byron_epoch_length,
                    ) {
                        Ok(block) => {
                            let mut ls = self.ledger_state.write().await;
                            let block_no = ls.tip.block_number.0 + 1;
                            if let Err(e) = ls.apply_block(&block, BlockValidationMode::ApplyOnly) {
                                warn!(slot = next_slot.0, "Replay ledger apply failed: {e}");
                            }
                            replayed += 1;
                            current_slot = next_slot.0;
                            current_hash = next_hash;

                            if last_log.elapsed().as_secs() >= 5 {
                                let elapsed = start.elapsed().as_secs_f64();
                                let speed = replayed as f64 / elapsed;
                                let pct = if end_slot > start_slot {
                                    (next_slot.0 - start_slot) as f64
                                        / (end_slot - start_slot) as f64
                                        * 100.0
                                } else {
                                    100.0
                                };
                                // Update Prometheus metric so TUI/monitoring can track replay progress
                                self.metrics.set_sync_progress(pct);
                                self.metrics.set_slot(next_slot.0);
                                self.metrics.set_block_number(block_no);
                                self.metrics.set_epoch(ls.epoch.0);
                                info!(
                                    progress = format_args!("{pct:>6.2}%"),
                                    slot = next_slot.0,
                                    end_slot,
                                    speed = format_args!("{speed:.0} blk/s"),
                                    utxos = ls.utxo.utxo_set.len(),
                                    "Replay",
                                );
                                last_log = std::time::Instant::now();
                            }

                            // Issue #695: no snapshots during LSM
                            // replay. Mirrors Haskell behavior — the
                            // background snapshot task is not active
                            // during replay. The existing post-loop
                            // save (below) produces a single snapshot
                            // once replay completes.
                        }
                        Err(e) => {
                            warn!(
                                slot = next_slot.0,
                                "Failed to decode block during replay: {e}"
                            );
                            // Advance past the undecodable block to avoid an
                            // infinite loop.
                            current_slot = next_slot.0;
                            current_hash = next_hash;
                        }
                    }
                }
                Ok(None) => {
                    // No more blocks after current_slot — replay complete.
                    break;
                }
                Err(e) => {
                    warn!(
                        current_slot,
                        "Failed to read from ChainDB during replay: {e}"
                    );
                    break;
                }
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            replayed as f64 / elapsed
        } else {
            0.0
        };
        info!(
            blocks = replayed,
            elapsed_secs = elapsed as u64,
            speed = format_args!("{} blk/s", speed as u64),
            "Replay complete",
        );

        // Update metrics with final replay state so they reflect the true
        // ledger position immediately (the progress ticker only fires every
        // 5 seconds and may miss the final state for short replays).
        {
            let ls = self.ledger_state.read().await;
            let slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            self.metrics.set_slot(slot);
            self.metrics.set_block_number(ls.tip.block_number.0);
            self.metrics.set_epoch(ls.epoch.0);
        }

        // Re-enable WAL and address indexing after replay
        {
            let mut ls = self.ledger_state.write().await;
            ls.utxo.utxo_set.set_wal_enabled(true);
            ls.utxo.utxo_set.set_indexing_enabled(true);
            ls.utxo.utxo_set.rebuild_address_index();
            // Rebuild stake distribution from the full UTxO set to correct any
            // residual state from the pre-replay snapshot. After this single
            // rebuild, incremental tracking is accurate and needs_stake_rebuild
            // self-disables at the next epoch boundary.
            ls.epochs.needs_stake_rebuild = true;
            ls.rebuild_stake_distribution();
            // Recompute pool_stake for all mark/set/go snapshots.
            ls.recompute_snapshot_pool_stakes();
            debug!("Rebuilt address index, stake distribution, and snapshot pool stakes after LSM replay");
        }

        // Save final snapshot after replay (write lock to flush UTxO store — no WAL)
        {
            let mut ls = self.ledger_state.write().await;
            merge_opcert_counters_from_praos(
                &mut ls.consensus.opcert_counters,
                self.consensus.opcert_counters(),
            );
            if let Err(e) = ls.save_utxo_snapshot() {
                error!("Failed to save UTxO store after replay: {e}");
            }
            if let Err(e) = ls.save_snapshot(&snapshot_path) {
                error!("Failed to save ledger snapshot after replay: {e}");
            }

            // Issue #742 defense-in-depth: publish the post-LSM-replay view
            // so any caller of `replay_from_lsm` that doesn't separately
            // call `publish_ledger_view` still gets a fresh view. The primary
            // publish is at the `Node::run` replay→live handover in `mod.rs`,
            // but having it here ensures correctness if this function is ever
            // called from a future code path that doesn't do the handover step.
            self.publish_ledger_view(&ls);
        }
    }
}

// ─── Per-Peer ChainSync Client Task ──────────────────────────────────────────

/// Extract the block header hash (Blake2b-256) from a raw header CBOR.
///
/// The ChainSync MsgRollForward delivers header CBOR that may be either:
/// 1. HFC-wrapped: `[era_tag, tag24(header_bytes)]` — from Haskell peers
/// 2. Full block CBOR — from Dugite peers
///
/// For Shelley+ HFC-wrapped headers, the hash is `blake2b_256(inner_bytes)`
/// (the bytes inside the tag24 wrap), matching how Haskell computes it.
///
/// For Byron HFC-wrapped headers, the wire form is
/// `[0, [[isEbb, size], tag24(bstr(byron_header))]]` and the hash is
/// `blake2b_256([0x82, isEbb, byron_header])` — see `byron_main_header_hash`
/// / `byron_ebb_header_hash` in dugite-serialization.
/// Merge per-pool opcert counters from the `PraosValidator`'s in-memory
/// state into the `LedgerState`'s persisted map, taking the per-pool max.
///
/// Two sources feed this map:
///
/// 1. **`compute_shelley_nonce`** (issue #670) — runs inside
///    `ls.apply_block(..)` and records the issuing pool's
///    `OperationalCert.sequence_number` for every block applied (chunk
///    replay, LSM replay, live apply).  This is the from-genesis
///    canonical source.
/// 2. **`PraosValidator.check_opcert_counter`** — runs during live
///    header validation only.  Updates the validator's own in-memory
///    map for replay-protection tie-breaking.
///
/// On snapshot save we merge the validator's view INTO the ledger's so
/// the persisted snapshot reflects the higher of the two sources per
/// pool.  The previous behaviour (`ls.consensus.opcert_counters =
/// self.consensus.opcert_counters().clone()`) clobbered the
/// per-apply-populated map with the validator's (which is empty during
/// pure replay), silently zeroing the field on every from-genesis save
/// and producing the `len 0 vs 467` divergence the
/// verify-ledger-snapshot harness reports against a Mithril ancillary.
fn merge_opcert_counters_from_praos(
    ledger: &mut std::collections::HashMap<dugite_primitives::hash::Hash28, u64>,
    praos: &std::collections::HashMap<dugite_primitives::hash::Hash28, u64>,
) {
    for (pool_id, seq) in praos {
        ledger
            .entry(*pool_id)
            .and_modify(|cur| {
                if *seq > *cur {
                    *cur = *seq;
                }
            })
            .or_insert(*seq);
    }
}

fn extract_hash_from_header(header_cbor: &[u8]) -> [u8; 32] {
    // Byron HFC wrap: [0, [[isEbb_u8, size_uint], tag24(bytes(byron_header))]]
    if let Some((is_ebb, byron_header_bytes)) = unwrap_byron_n2n_header(header_cbor) {
        // Byron header hash = blake2b_256(cbor([isEbb_u8, byron_header])).
        // That CBOR encodes as `array(2) ++ uint(isEbb) ++ byron_header_bytes`.
        let mut buf = Vec::with_capacity(2 + byron_header_bytes.len());
        buf.push(0x82); // array(2)
        buf.push(is_ebb); // uint 0 (EBB) or 1 (main)
        buf.extend_from_slice(byron_header_bytes);
        let hash = dugite_primitives::hash::blake2b_256(&buf);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(hash.as_ref());
        return arr;
    }

    // Shelley+ HFC wrap: [era_tag(uint), tag24(bytes(inner_header))]
    // The Cardano block hash is blake2b_256 of the INNER header bytes
    // (the bytes inside tag24), NOT the outer wrapper. Hashing the wrapper
    // produces wrong hashes that BlockFetch cannot find.
    let inner = unwrap_hfc_header(header_cbor).unwrap_or(header_cbor);
    let hash = dugite_primitives::hash::blake2b_256(inner);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(hash.as_ref());
    arr
}

/// Extract the DECLARED block body size from a wrapped ChainSync header.
///
/// Shelley+ headers carry `block_body_size`; Byron headers do not (the
/// wrapped-header decoder rejects Byron era tags), and malformed headers
/// return `None` — both fall back to the adaptive average estimate in the
/// BlockFetch range builder. Used for exact in-flight byte accounting
/// (#747), mirroring Haskell's `blockFetchSize` which schedules fetch
/// ranges from header-declared sizes rather than estimates.
/// Extract `(declared body size, prev_hash)` from a wrapped ChainSync header
/// in ONE decode (both feed BlockFetch range planning, #747):
/// - `body_size` → exact per-range byte accounting;
/// - `prev_hash` → chain-adjacency run splitting. `pending_headers` can be
///   SPARSE relative to the peer's chain (headers whose block is already in
///   the ChainDB are never pushed — common right after a CSJ dynamo rotation
///   re-streams an overlapping segment), so consecutive pending entries are
///   not necessarily consecutive blocks; a `MsgRequestRange` spanning such a
///   hidden gap makes the peer deliver the gap blocks too, blowing the byte
///   budget (observed live: ranges delivering 1.7-2x their estimate).
fn extract_header_fetch_info(header_cbor: &[u8]) -> (Option<u64>, Option<[u8; 32]>) {
    match dugite_serialization::decode_wire_wrapped_block_header(header_cbor) {
        Ok(h) => {
            let mut prev = [0u8; 32];
            prev.copy_from_slice(h.prev_hash.as_ref());
            (Some(h.body_size), Some(prev))
        }
        Err(_) => (None, None),
    }
}

/// Unwrap a Shelley+ HFC-wrapped header to get the inner header bytes.
///
/// N2N ChainSync headers for Shelley+ are wrapped as
/// `[era_tag(uint), tag24(inner_bytes)]`.  Returns the inner bytes, or `None`
/// if the CBOR is not in that format (e.g. a Byron HFC wrap, which has a
/// nested array instead of a uint era tag — see [`unwrap_byron_n2n_header`]).
fn unwrap_hfc_header(header_cbor: &[u8]) -> Option<&[u8]> {
    use minicbor::Decoder;
    let mut dec = Decoder::new(header_cbor);
    let arr_len = dec.array().ok()?;
    if arr_len != Some(2) {
        return None;
    }
    // The first element MUST be a uint for the Shelley+ wrap.  Reject any
    // other type (notably array, which indicates the Byron wrap).
    if !matches!(
        dec.datatype().ok()?,
        minicbor::data::Type::U8
            | minicbor::data::Type::U16
            | minicbor::data::Type::U32
            | minicbor::data::Type::U64
    ) {
        return None;
    }
    let _era_tag = dec.u64().ok()?;
    let tag = dec.tag().ok()?;
    if tag != minicbor::data::Tag::new(24) {
        return None;
    }
    dec.bytes().ok()
}

/// Unwrap a Byron N2N header from its HFC + `ABoundaryOrRegular` wrappers.
///
/// The Byron N2N wire form sent by `cardano-node` is:
///
/// ```text
///   array(2)
///     uint(0)                                 // HFC era index for Byron
///     array(2)
///       array(2) [ uint(isEbb), uint(size) ]  // ABoundaryOrRegular tag + ann. size
///       tag(24)(bytes(byron_header_cbor))
/// ```
///
/// Returns `Some((isEbb, byron_header_bytes))` on success.  Both EBB
/// (`isEbb == 0`) and main-block (`isEbb == 1`) headers are accepted.
fn unwrap_byron_n2n_header(header_cbor: &[u8]) -> Option<(u8, &[u8])> {
    use minicbor::Decoder;
    let mut dec = Decoder::new(header_cbor);

    // Outer: array(2) [era_id=0, payload]
    if dec.array().ok()? != Some(2) {
        return None;
    }
    // era_id must be uint(0) (Byron). Reject any other type so we never
    // mistake a Shelley+ wrap for a Byron one.
    if !matches!(
        dec.datatype().ok()?,
        minicbor::data::Type::U8
            | minicbor::data::Type::U16
            | minicbor::data::Type::U32
            | minicbor::data::Type::U64
    ) {
        return None;
    }
    let era_id = dec.u64().ok()?;
    if era_id != 0 {
        return None;
    }

    // Payload: array(2) [ [isEbb, size], tag24(bytes(...)) ]
    if dec.array().ok()? != Some(2) {
        return None;
    }
    // [isEbb, size]
    if dec.array().ok()? != Some(2) {
        return None;
    }
    let is_ebb_u64 = dec.u64().ok()?;
    if is_ebb_u64 > 1 {
        return None;
    }
    let _size = dec.u64().ok()?; // annotation-size hint; not load-bearing
                                 // tag24(bytes(byron_header_cbor))
    let tag = dec.tag().ok()?;
    if tag != minicbor::data::Tag::new(24) {
        return None;
    }
    let inner = dec.bytes().ok()?;
    Some((is_ebb_u64 as u8, inner))
}

/// Extract the slot number from a wrapped header CBOR.
///
/// The N2N ChainSync protocol sends headers wrapped by the Hard-Fork
/// Combinator.  Three layouts are supported here:
///
/// 1. **Shelley+ HFC wrap**: `[era_id(uint), tag24(bytes(inner_header))]`
///    where `inner_header = [header_body, body_signature, ...]` and
///    `header_body = [block_number, slot, ...]`.
/// 2. **Byron HFC wrap** (sent by `cardano-node` for pre-Shelley blocks):
///    `[0, [[isEbb, size], tag24(bytes(byron_header))]]`.  EBB and main-block
///    headers are decoded by [`decode_byron_header_slot_difficulty`].
/// 3. **Raw Shelley+ header** (already unwrapped) — kept for back-compat with
///    the legacy `chain_sync_loop` code path.
///
/// `byron_epoch_length` is used for Byron slot computation; `0` selects the
/// mainnet formula (`epoch * 21_600 + rel_slot`).
///
/// Returns `None` if the header CBOR cannot be parsed.
#[cfg(test)]
fn extract_slot_from_wrapped_header(header_cbor: &[u8], byron_epoch_length: u64) -> Option<u64> {
    extract_slot_block_no_from_wrapped_header(header_cbor, byron_epoch_length).map(|(s, _)| s)
}

/// Extract `(slot, block_no)` from a wrapped header.
///
/// Same decoding paths as `extract_slot_from_wrapped_header`, but also
/// surfaces the block number for the Genesis candidate fragments
/// (Haskell candidates are full headers; the GSM CaughtUp predicate and
/// chain-preference comparison need `blockNo`):
/// - Shelley+: `header_body = [block_number, slot, …]`.
/// - Byron main: `consensus_data = [[epoch, rel_slot], issuer,
///   [difficulty], sig]` — `difficulty` is the Byron chain length.
/// - Byron EBB: `consensus_data = [epoch, [difficulty]]`.
fn extract_slot_block_no_from_wrapped_header(
    header_cbor: &[u8],
    byron_epoch_length: u64,
) -> Option<(u64, u64)> {
    use minicbor::Decoder;

    // 1. Byron HFC wrap — check first because its outer shape can collide
    //    with the raw-header fallback below.
    if let Some((is_ebb, byron_header_bytes)) = unwrap_byron_n2n_header(header_cbor) {
        return decode_byron_header_slot_difficulty(byron_header_bytes, is_ebb, byron_epoch_length);
    }

    // 2. Shelley+ HFC wrap: [era_tag(uint), tag24(bytes(inner_header))]
    if let Some(inner_bytes) = unwrap_hfc_header(header_cbor) {
        let mut inner = Decoder::new(inner_bytes);
        let _ = inner.array().ok()?;
        let _ = inner.array().ok()?;
        let block_number = inner.u64().ok()?;
        let slot = inner.u64().ok()?;
        return Some((slot, block_number));
    }

    // 3. Raw Shelley+ header (legacy path) — already unwrapped.
    // Structure: array(2+) [header_body, body_signature, ...]
    // header_body: array(N) [block_number, slot, ...]
    let mut dec = Decoder::new(header_cbor);
    if let Ok(Some(_outer_len)) = dec.array() {
        if let Ok(Some(_body_len)) = dec.array() {
            let block_number = dec.u64().ok()?;
            let slot = dec.u64().ok()?;
            return Some((slot, block_number));
        }
    }

    None
}

/// Mainnet Byron epoch length in slots used when `byron_epoch_length == 0`.
/// Matches the mainnet formula `(epoch * 432_000) / 20 = epoch * 21_600`.
const MAINNET_BYRON_SLOTS_PER_EPOCH: u64 = 21_600;

// ───────────────────────────────────────────────────────────────────────────
// Issue #654 — eager forecast-horizon check + watch-channel backpressure
// ───────────────────────────────────────────────────────────────────────────

/// Hard upper bound on how long a chainsync receive task will park waiting
/// for the ledger tip to advance enough to forecast a received header
/// (#654 / #652 C4). When this elapses the peer is disconnected with
/// `ForecastSuspensionTimeout`. 60s is a balance between giving the apply
/// path time to catch up under realistic bulk-sync load and not hanging
/// the receive loop indefinitely if our node is wedged. Used **at/near tip**,
/// where a frozen ledger for 60s is genuinely suspicious.
pub(crate) const FORECAST_PARK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Bulk-catch-up upper bound on the forecast park (see
/// [`forecast_park_or_disconnect`]). When the node is far **behind the network
/// tip** (more than a stability window), the ledger legitimately lags the
/// header tip and transient apply stalls of a couple of minutes are expected
/// (fetch is the bottleneck, especially on high-latency links). Disconnecting
/// the chainsync peer in that window is actively harmful: the peer is innocent
/// (our ledger is the laggard, mirroring cardano-node which parks at the
/// horizon and never drops the upstream), and dropping it removes the very
/// blockfetch candidate that would close the gap — empirically collapsing the
/// whole peer set on a transient stall. We therefore use a much larger bound
/// while behind, comfortably above observed self-recovering stalls (~3 min),
/// so only a *genuine* multi-minute wedge trips it. Crucially this bound is
/// still **finite**: the network-tip signal selects the timeout MAGNITUDE, it
/// never suppresses the watchdog — a real wedge is always surfaced, so a stale
/// or peer-inflated network tip can only lengthen detection, never produce an
/// infinite silent park.
pub(crate) const FORECAST_PARK_TIMEOUT_BULK: std::time::Duration =
    std::time::Duration::from_secs(300);

/// Select the forecast-park no-progress timeout.
///
/// `network_tip_slot` is the highest tip any peer has reported
/// (`metrics.get_peer_tip()`, a monotonic max). When it leads our
/// `local_tip_slot` by more than a (per-era) `stability_window`, the local
/// ledger is legitimately behind the network — bulk catch-up — and a header
/// beyond the forecast horizon is normal apply lag, so we wait patiently
/// ([`FORECAST_PARK_TIMEOUT_BULK`]) rather than churning the peer. Otherwise we
/// are at/near tip, where a frozen ledger is suspicious, so the tight
/// [`FORECAST_PARK_TIMEOUT`] applies.
///
/// Crucially the result is ALWAYS finite: this signal selects the timeout
/// MAGNITUDE only — it can never suppress the watchdog. A stale or
/// peer-inflated `network_tip_slot` can therefore only lengthen detection
/// (max 5 min), never produce an infinite silent park while apply is wedged.
pub(crate) fn forecast_park_timeout(
    network_tip_slot: u64,
    local_tip_slot: u64,
    stability_window: u64,
) -> std::time::Duration {
    if network_tip_slot.saturating_sub(local_tip_slot) > stability_window {
        FORECAST_PARK_TIMEOUT_BULK
    } else {
        FORECAST_PARK_TIMEOUT
    }
}

/// Cheap structural predicate: returns `true` if `header_cbor` is the
/// Byron N2N HFC wrap (era tag 0 with the `ABoundaryOrRegular` payload
/// shape). Inverse of "is this a Shelley+ HFC-wrapped header".
fn is_byron_wrapped_header(header_cbor: &[u8]) -> bool {
    unwrap_byron_n2n_header(header_cbor).is_some()
}

/// Extract the HFC era tag from a Shelley+ wrapped header CBOR.
/// `[era_tag(uint), tag(24)(bytes(inner_header))]` → `Some(era_tag)`.
/// Returns `None` for Byron wraps (which have a nested array where the
/// uint would be) and for malformed CBOR.
fn extract_era_tag_from_wrapped_header(header_cbor: &[u8]) -> Option<u64> {
    use minicbor::Decoder;
    let mut dec = Decoder::new(header_cbor);
    let arr_len = dec.array().ok()?;
    if arr_len != Some(2) {
        return None;
    }
    if !matches!(
        dec.datatype().ok()?,
        minicbor::data::Type::U8
            | minicbor::data::Type::U16
            | minicbor::data::Type::U32
            | minicbor::data::Type::U64
    ) {
        return None;
    }
    dec.u64().ok()
}

/// Issue #655 P2.b — decide whether the apply-time
/// `validate_header_full` re-check can be safely skipped because the
/// header passed eager per-peer validation against the same epoch.
///
/// Inputs: feature flag, current epoch at apply time, optional
/// `(hash, recorded_epoch)` entry the eager path inserted. Returns
/// `(skip?, remove_from_map?)`:
///
/// - flag off → never skip, never remove (the map is irrelevant).
/// - flag on, no entry → never skip, never remove (block didn't go
///   through the eager path — Byron, pre-Conway, or eager skip).
/// - flag on, entry at same epoch → SKIP and remove (eager pass
///   already covered the same crypto against the same snapshot).
/// - flag on, entry at stale epoch → DO NOT skip, but DO remove
///   (snapshot pointer may have changed; re-validate, and cull the
///   stale entry to bound memory).
///
/// Pure function so the truth table is independently testable.
pub(crate) fn decide_skip_apply_header_crypto(
    flag_enabled: bool,
    current_epoch: u64,
    recorded_epoch_for_hash: Option<u64>,
) -> (bool, bool) {
    if !flag_enabled {
        return (false, false);
    }
    match recorded_epoch_for_hash {
        Some(epoch) if epoch == current_epoch => (true, true),
        Some(_stale) => (false, true),
        None => (false, false),
    }
}

/// Issue #654 P1.b — eager per-peer header validation hook.
///
/// Called from the MsgRollForward integration site after slot extraction,
/// Byron skip, and forecast back-pressure. Decodes the wrapped header into
/// a [`BlockHeader`], looks up the issuer pool in the lock-free
/// [`super::ledger_view::LedgerView`]'s `set` snapshot, and runs the full
/// Praos validation against `peer_counters` via
/// `validate_header_full_with_counters` (clone-and-swap so the global
/// `OuroborosPraos.opcert_counters` is not touched — #652 C1).
///
/// Returns:
/// - `Ok(true)`  — header passed eager validation; caller proceeds to
///   `pending_headers.push`.
/// - `Ok(false)` — eager validation was deliberately SKIPPED (pre-Babbage
///   era where overlay context is needed but unavailable, or no `set`
///   snapshot yet on a fresh node). Caller proceeds with the existing
///   path; defense in depth keeps body-apply validation authoritative.
/// - `Err(_)`    — header failed validation. Caller propagates to
///   disconnect the peer with a labelled reason.
///
/// Defense in depth: the apply path continues to fully re-validate every
/// header against the live ledger state. This hook is the earlier
/// signal that flags bad headers within ~100 ms of receive, before they
/// reach BlockFetch + apply.
pub(crate) fn eager_validate_header(
    peer_addr: &SocketAddr,
    header_cbor: &[u8],
    header_slot: u64,
    consensus_seed: &dugite_consensus::praos::OuroborosPraos,
    ledger_view: &super::ledger_view::LedgerView,
    peer_counters: &mut HashMap<dugite_primitives::hash::Hash28, u64>,
) -> Result<bool, dugite_consensus::ConsensusError> {
    // Phase 1 simplification: only eager-validate Conway+ headers. Earlier
    // Shelley/Allegra/Mary/Alonzo/Babbage headers require overlay schedule
    // context (BFT delegate slots when d > 0) which Phase 1 does not
    // construct from the LedgerView. Conway+ has d == 0, so no overlay.
    //
    // N2N WIRE headers carry the 0-based HFC index (Byron COMBINED at 0):
    // … 5=Babbage, 6=Conway, 7=Dijkstra. The previous `< 7` gate was written
    // against the STORAGE mapping (7=Conway) — on the wire that matched
    // Dijkstra only, so eager validation silently never ran for Conway
    // chains (caught 2026-06-12 alongside the #747 wire/storage mismatch).
    const WIRE_HFC_CONWAY: u64 = 6;
    let era_tag = match extract_era_tag_from_wrapped_header(header_cbor) {
        Some(t) => t,
        None => return Ok(false), // malformed envelope — let body apply catch it
    };
    if era_tag < WIRE_HFC_CONWAY {
        return Ok(false);
    }

    // Decode the header. Anything that fails here is malformed CBOR — let
    // the caller treat that as a structural error (return Err so we
    // disconnect with a labelled reason).
    let header = dugite_serialization::decode_wire_wrapped_block_header(header_cbor).map_err(|e| {
        tracing::warn!(%peer_addr, slot = header_slot, "ChainSync eager: header decode failed: {e}");
        dugite_consensus::ConsensusError::InvalidBlock(format!("eager header decode: {e}"))
    })?;

    // Look up the issuer pool in the SET snapshot — the active
    // distribution for the current epoch's leader election (Cardano
    // mark/set/go rule, #652 C2).
    //
    // INCOMPLETE-VIEW GUARD: eager validation is an OPTIMIZATION layer; its
    // failure mode for missing data must be SKIP (fall back to the
    // authoritative body apply), never reject. When the view has NO set
    // snapshot — or an EMPTY one — (genesis epochs 0/1 before the first
    // mark/set/go rotation, or a view that has not been populated yet),
    // rejecting here partitions honest peers: the first devnet run after
    // the wire-era-gate fix made eager validation actually execute for
    // Conway headers rejected every block the BP forged from genesis
    // ("block from unregistered pool", slot 4) and disconnected it.
    // A POPULATED set that lacks the pool still rejects via
    // issuer_info=None below — the #654 unknown-pool header hardening is
    // preserved for every mid-chain case.
    let pool_id = dugite_primitives::hash::blake2b_224(&header.issuer_vkey);
    // Select the snapshot that governs the HEADER's epoch, not the view's.
    //
    // During catch-up the ChainSync header stream legitimately runs ahead of
    // block apply, so a header may belong to epoch E+1 while the view is still
    // at E. Haskell forecasts across that boundary because the next epoch's
    // poolDistr is already fixed (it is the pre-rotation `mark`); using the
    // view's `set` instead applies the WRONG epoch's distribution. A pool that
    // first gained stake in `mark` is then absent from `set` and every valid
    // block it issued in E+1 is rejected as `UnregisteredPool` — which, because
    // an eager failure tears down the connection, disconnects every honest peer
    // serving that block in turn. Observed live on preview 2026-07-16/25.
    let snap = match ledger_view.snapshot_for_epoch(ledger_view.epoch_of_slot(header.slot.0)) {
        Some(s) => s,
        // No captured snapshot describes the header's epoch (genesis epochs
        // before the first rotation, an empty snapshot, or a header further
        // ahead than we can forecast) — skip eager, body apply decides.
        _ => return Ok(false),
    };
    let issuer_info = match (
        snap.pool_stake.get(&pool_id),
        snap.pool_params.get(&pool_id),
    ) {
        (Some(pool_stake), Some(reg)) => {
            let total_active_stake: u64 = snap.pool_stake.values().map(|l| l.0).sum();
            Some(dugite_consensus::praos::BlockIssuerInfo {
                vrf_keyhash: reg.vrf_keyhash,
                pool_stake: pool_stake.0,
                total_active_stake,
            })
        }
        // Stake but no registration in the SAME snapshot: the eager layer cannot
        // state this pool's VRF key. The previous code substituted
        // `Hash32::ZERO`, which can never equal `blake2b_256(header.vrf_vkey)`,
        // so a header whose VRF key was in fact correctly registered was
        // rejected with `VrfKeyMismatch` and its peers dropped (the 2026-07-16
        // report). A fabricated hash must never drive a rejection — skip.
        (Some(_), None) => return Ok(false),
        // No stake entry: genuinely unknown pool for this epoch. Preserved as
        // fatal — this is the #654 unknown-pool header hardening.
        (None, _) => None,
    };

    // Forecast horizon was already checked by `forecast_park_or_disconnect`
    // at this site; pass the same `last_applied_slot` so any further
    // forecast checks inside `validate_header_full` are consistent.
    let ledger_tip = ledger_view.last_applied_slot;
    let ledger_pv = Some(ledger_view.protocol_params.protocol_version_major);

    // current_slot for eager validation: the receiving node's wall-clock
    // slot is not trivially available here, but the header's own slot
    // bounds the maximum legitimate `current_slot` (we only care about
    // the FutureBlock check — header.slot > current_slot — which can be
    // ignored eagerly because chainsync messages from a peer ahead of us
    // are normal; let body apply re-check against the wall-clock slot).
    // Pass `header.slot` so the FutureBlock check trivially passes.
    let current_slot = header.slot;

    // Seed the per-peer opcert-counter view from the GLOBAL (snapshot-derived)
    // counters on the first encounter of each pool.
    //
    // `peer_counters` is a per-peer override map (#652 C1): so that headers
    // from one peer's fork never mutate another peer's — or the global —
    // counter state, `validate_header_full_with_counters` swaps this map in
    // wholesale (`std::mem::take`) for the duration of the call. But the map
    // starts EMPTY (`CandidateChainState::default`), so without seeding, the
    // OCERT check inside falls back to counter 0 for any known pool and a
    // pool whose snapshot counter is > 1 trips a false
    // `CounterOverIncrementedOCERT` on its very first eager header —
    // disconnecting every peer of a Mithril-bootstrapped node.
    //
    // A from-genesis node never hit this: it accumulates each pool's counter
    // from 0 as it applies the chain, so the per-peer map is already warm by
    // the time eager validation runs at the tip. Only a node that JUMPED to a
    // mid-chain tip via a Mithril snapshot (where pools already carry
    // arbitrary counters, e.g. mainnet/preprod max 463) is affected — the
    // reported genesis-mode bootstrap failure.
    //
    // Seeding lazily per-pool (not a full upfront clone) keeps this O(1) per
    // first-seen pool. An existing per-peer entry is never overwritten: a
    // peer's own fork may legitimately have advanced the counter past the
    // snapshot value, and that diverged view must win.
    seed_peer_counter_from_global(peer_counters, consensus_seed.opcert_counters(), pool_id);

    let result = consensus_seed.validate_header_full_with_counters(
        peer_counters,
        &header,
        current_slot,
        issuer_info.as_ref(),
        None, // overlay_ctx — Conway+ d=0, no overlay schedule needed
        dugite_consensus::ValidationMode::Replay,
        ledger_pv,
        ledger_tip,
    );
    if let Err(dugite_consensus::ConsensusError::OpcertCounterOverIncremented { got, last_seen }) =
        &result
    {
        tracing::debug!(
            %peer_addr, slot = header_slot, got, last_seen,
            "eager: opcert over-increment deferred to authoritative body apply \
             (per-peer counter is not authoritative for the upper bound)"
        );
    }
    classify_eager_validation_result(result)
}

/// Map a `validate_header_full_with_counters` result to the eager-validation
/// outcome, deferring the one check the eager layer cannot authoritatively
/// enforce.
///
/// The opcert UPPER bound (`CounterOverIncrementedOCERT`, `n > m + 1`) is
/// computed against `m` = the PER-PEER reconstructed counter, which only ever
/// sees the opcert advances carried by headers THIS peer streamed. A pool may
/// legitimately advance its counter by > 1 across blocks the peer never sent
/// us (it re-issued its opcert several times off-window, or — on a
/// Mithril-bootstrapped node — advanced past the snapshot tip while our
/// per-peer baseline stayed frozen at the startup `consensus_seed` clone).
/// Treating that as fatal in the eager layer false-rejects an honest peer
/// (review of #756: a freshly-connected peer's first header for a
/// post-snapshot-rotated pool was penalised even though the block is valid
/// on-chain).
///
/// So an over-increment is mapped to `Ok(false)` (deliberate skip): the
/// body-apply path (`validate_header_full` against the LIVE global counters,
/// which advance on every applied block) IS authoritative and re-checks the
/// upper bound, so deferring it here loses no safety — a genuine
/// over-increment attack is still rejected at apply with the peer attributed
/// there. EVERY OTHER check (VRF, KES, and crucially the opcert LOWER bound
/// `CounterTooSmallOCERT` replay-regression guard, which a stale baseline can
/// only make MORE conservative) stays fatal.
fn classify_eager_validation_result(
    result: Result<(), dugite_consensus::ConsensusError>,
) -> Result<bool, dugite_consensus::ConsensusError> {
    match result {
        Ok(()) => Ok(true),
        Err(dugite_consensus::ConsensusError::OpcertCounterOverIncremented { .. }) => Ok(false),
        // The eager forecast horizon is checked against the lock-free `LedgerView`,
        // whose `last_applied_slot` can LAG the live ledger tip — `publish_ledger_view`
        // is throttled during catch-up (#698) and freezes entirely if the apply loop
        // stalls. `forecast_park_or_disconnect` has ALREADY gated this header against
        // the FRESH tip watch (`tip_rx`) at the call site, so a stale view that cannot
        // yet forecast the slot is a FALSE negative — defer to the authoritative body
        // apply (which forecasts against the live ledger state), never disconnect.
        // Without this, a throttled view freezing ~a stability window behind the
        // applied tip eager-rejects every in-range header and disconnects all peers →
        // permanent wedge (observed live at the mainnet Babbage→Conway boundary,
        // 2026-06-13: view frozen at slot 133109521 rejected the first Conway block at
        // 133660855 while the applied tip was 133660799, churning all 40 peers).
        Err(dugite_consensus::ConsensusError::OutsideForecast(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Seed a single pool's entry in a per-peer opcert-counter view from the
/// global (snapshot-derived) counter map, iff the per-peer map does not
/// already track that pool.
///
/// This is the core of the Mithril genesis-bootstrap fix: the per-peer eager
/// validation map (`CandidateChainState::eager_opcert_counters`) starts empty
/// and is swapped wholesale into the validator, discarding the global
/// snapshot counters. Seeding the pool on first encounter restores the
/// snapshot's counter as the per-peer baseline; an already-present entry
/// (the peer's own diverged fork value) is preserved.
pub(crate) fn seed_peer_counter_from_global(
    peer_counters: &mut HashMap<dugite_primitives::hash::Hash28, u64>,
    global_counters: &HashMap<dugite_primitives::hash::Hash28, u64>,
    pool_id: dugite_primitives::hash::Hash28,
) {
    if let std::collections::hash_map::Entry::Vacant(slot) = peer_counters.entry(pool_id) {
        if let Some(&seeded) = global_counters.get(&pool_id) {
            slot.insert(seeded);
        }
    }
}

/// Run the forecast check for an incoming header's slot against the
/// lock-free `LedgerView`. If the slot is within `[at, max_for)`
/// (`max_for = tip + 1 + stability_window`), return `Ok(())` immediately.
/// Otherwise, park on `tip_rx.changed()` until either:
///
/// - the ledger tip advances enough that the slot is now in range
///   (return `Ok(())`);
/// - the chainsync task is cancelled (return `Ok(())` so the cancel
///   propagates through the normal exit path);
/// - the hard timeout [`FORECAST_PARK_TIMEOUT`] elapses
///   (return `Err` so the caller disconnects with a labelled reason).
///
/// The stability window is picked per era: the Conway
/// `randomness_stabilisation_window` if set on the view, falling back to
/// the pre-Conway `stability_window_3kf`.
pub(crate) async fn forecast_park_or_disconnect(
    peer_addr: &SocketAddr,
    header_slot: u64,
    ledger_view: &Arc<arc_swap::ArcSwap<super::ledger_view::LedgerView>>,
    tip_rx: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    // GSM snapshot — in genesis Syncing/PreSyncing (bulk sync) the node
    // legitimately streams headers far ahead of the LoE-gated ledger; the
    // forecast park must NOT disconnect (that churns peers and pins the LoE
    // at the immutable tip, deadlocking the sync). Park and wait instead.
    gsm_snapshot_rx: Option<&watch::Receiver<crate::gsm::GsmSnapshot>>,
    // Highest tip slot reported by any peer (global monotonic max,
    // `metrics.get_peer_tip()`). In **Praos** mode the GSM is always
    // `CaughtUp`, so `gsm_snapshot_rx` never marks bulk sync — yet a Praos
    // from-genesis resync IS bulk sync and legitimately lags the header tip by
    // far more than a stability window. We compare this against our local tip
    // to detect "behind the network" and pick the longer, patient timeout.
    // Used to select the timeout MAGNITUDE only; it never suppresses the
    // disconnect (see [`FORECAST_PARK_TIMEOUT_BULK`]).
    network_tip_slot: u64,
) -> anyhow::Result<()> {
    use dugite_consensus::forecast::forecast_for;
    use dugite_primitives::time::SlotNo;

    // Measure the timeout from the last LEDGER PROGRESS, not from when parking
    // began. A header far beyond the forecast horizon legitimately takes many
    // blocks of catch-up apply to come into range (the gap can be ~a stability
    // window ≈ thousands of blocks). As long as the ledger keeps advancing the
    // peer is doing its job — disconnecting it would starve the very blockfetch
    // that closes the gap, wedging the whole sync (all peers churn on
    // "beyond forecast horizon", ledger can never advance). Only a genuinely
    // stalled ledger (no apply progress at all for FORECAST_PARK_TIMEOUT) drops
    // the peer. Mirrors cardano-node, which blocks ChainSync at the forecast
    // horizon and waits for the ledger rather than dropping the upstream peer.
    let mut last_progress = std::time::Instant::now();
    let mut wakes: u32 = 0;
    // Issue #742: track total park duration so we can emit a rate-limited
    // WARN when a peer has been parked for >60s. In genesis bulk-sync mode
    // the park is expected (no timeout), but being silent at DEBUG only
    // meant this failure mode was invisible at default log level.
    let park_start = std::time::Instant::now();
    let mut last_warn = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(120))
        .unwrap_or(std::time::Instant::now());
    const PARK_WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    loop {
        let view = ledger_view.load();
        let stability_window = if view.randomness_stabilisation_window > 0 {
            view.randomness_stabilisation_window
        } else {
            view.stability_window_3kf
        };
        // For the forecast-horizon check we prefer the **freshest** tip slot
        // available — the watch-channel value updated on every block apply
        // (issue #654) — rather than `view.last_applied_slot`, which during
        // catch-up sync may lag behind because `publish_ledger_view` is
        // throttled (#698 perf gate).  Without this, the chainsync client
        // would wake on `tip_rx.changed()` but still see the stale
        // LedgerView, re-park, and eventually hit the
        // FORECAST_PARK_TIMEOUT cap — exactly the regression that follows
        // the gate land.
        let latest_tip_slot = *tip_rx.borrow();
        let tip_slot_no = if latest_tip_slot > 0 {
            Some(SlotNo(latest_tip_slot))
        } else {
            view.last_applied_slot
        };
        match forecast_for(tip_slot_no, stability_window, SlotNo(header_slot)) {
            Ok(()) => return Ok(()),
            Err(out) => {
                let genesis_bulk_sync = gsm_snapshot_rx
                    .is_some_and(|rx| rx.borrow().state != crate::gsm::GenesisSyncState::CaughtUp);
                // Praos from-genesis resync is bulk sync too, but GSM is always
                // `CaughtUp` so `genesis_bulk_sync` can't see it. Detect "behind
                // the network" from the tip gap: if the highest peer-reported
                // tip leads our local tip by more than a (per-era) stability
                // window, our ledger is legitimately lagging the header tip and
                // a header beyond the horizon is normal apply lag — be patient
                // (`FORECAST_PARK_TIMEOUT_BULK`) rather than churning the peer.
                // The signal only selects the timeout magnitude; both bounds are
                // finite so a genuine wedge is always surfaced (a stale or
                // peer-inflated `network_tip_slot` can only lengthen detection,
                // never cause an infinite silent park).
                let park_timeout =
                    forecast_park_timeout(network_tip_slot, latest_tip_slot, stability_window);
                let behind_network_tip = park_timeout == FORECAST_PARK_TIMEOUT_BULK;
                let elapsed = last_progress.elapsed();
                if elapsed >= park_timeout && !genesis_bulk_sync {
                    return Err(anyhow::anyhow!(
                        "ChainSync: {peer_addr} header slot {} beyond forecast horizon \
                         (tip {:?}, max_for {}) — no ledger progress for {:?} \
                         (behind_network_tip={}, network_tip={}); disconnecting",
                        header_slot,
                        out.at,
                        out.max_for.0,
                        park_timeout,
                        behind_network_tip,
                        network_tip_slot
                    ));
                }
                // Issue #742: emit a rate-limited WARN when a peer has been
                // parked beyond the WARN interval (default 60s). This makes
                // the stale-view deadlock visible at default log level.
                // In non-genesis mode the hard timeout fires at 60s anyway,
                // so the warn is primarily for the genesis bulk-sync park.
                let total_parked = park_start.elapsed();
                if total_parked >= PARK_WARN_INTERVAL && last_warn.elapsed() >= PARK_WARN_INTERVAL {
                    warn!(
                        %peer_addr,
                        header_slot,
                        tip = ?out.at,
                        max_for = out.max_for.0,
                        parked_secs = total_parked.as_secs(),
                        genesis_bulk_sync,
                        behind_network_tip,
                        network_tip_slot,
                        park_timeout_secs = park_timeout.as_secs(),
                        "ChainSync: peer has been parked on forecast horizon for >{}s; \
                         check that publish_ledger_view is firing after replay \
                         (issue #742 — stale tip watch causes permanent park in \
                         genesis bulk-sync mode)",
                        PARK_WARN_INTERVAL.as_secs(),
                    );
                    last_warn = std::time::Instant::now();
                }
                debug!(
                    %peer_addr,
                    header_slot,
                    tip = ?out.at,
                    max_for = out.max_for.0,
                    wakes,
                    "ChainSync: header beyond forecast horizon; parking on tip advance"
                );
                // In genesis bulk sync, park indefinitely (re-check on a
                // periodic tick as well as tip advance) — never disconnect.
                let remaining = if genesis_bulk_sync {
                    std::time::Duration::from_secs(5)
                } else {
                    park_timeout.saturating_sub(elapsed)
                };
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    res = tip_rx.changed() => {
                        if res.is_err() {
                            // Sender dropped → node shutdown path. Let the
                            // outer cancel handling take over.
                            return Ok(());
                        }
                        wakes = wakes.saturating_add(1);
                        // The ledger advanced — the peer is helping close the
                        // gap. Reset the no-progress timer and re-check.
                        last_progress = std::time::Instant::now();
                    }
                    _ = tokio::time::sleep(remaining) => {
                        if genesis_bulk_sync {
                            // Re-loop: re-read the GSM state and the horizon
                            // (the LoE/ledger may have advanced without a
                            // tip_rx wake under throttling). No disconnect.
                            continue;
                        }
                        return Err(anyhow::anyhow!(
                            "ChainSync: {peer_addr} header slot {} beyond forecast horizon \
                             — no ledger progress for {:?} \
                             (behind_network_tip={}, network_tip={}); disconnecting",
                            header_slot,
                            park_timeout,
                            behind_network_tip,
                            network_tip_slot
                        ));
                    }
                }
            }
        }
    }
}

/// Decode the slot number from a raw Byron header CBOR.
///
/// `byron_header_cbor` is the array(5) header
/// `[protocol_magic, prev_hash, body_proof, consensus_data, extra_data]`
/// extracted from the Byron HFC + `ABoundaryOrRegular` wrappers.
///
/// * EBB (`is_ebb == 0`): `consensus_data = [uint(epoch), array(1)[difficulty]]`
///   (matching Haskell `ABoundaryConsensusData`), slot = `epoch * epoch_length`.
/// * Main block (`is_ebb == 1`):
///   `consensus_data = [array(2)[epoch, rel_slot], issuer, array(1)[difficulty], block_sig]`,
///   slot = `epoch * epoch_length + rel_slot`.
///
/// `byron_epoch_length == 0` selects the mainnet formula
/// (`epoch * 21_600 + rel_slot`, derived from `(epoch * 432_000) / 20`).
/// Byron header → `(absolute_slot, difficulty)`.
///
/// `difficulty` is Byron's chain-length counter (the block-number
/// equivalent used for the Genesis candidate fragments).
fn decode_byron_header_slot_difficulty(
    byron_header_cbor: &[u8],
    is_ebb: u8,
    byron_epoch_length: u64,
) -> Option<(u64, u64)> {
    use minicbor::Decoder;

    let mut dec = Decoder::new(byron_header_cbor);
    // header = array(5) [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
    if dec.array().ok()? != Some(5) {
        return None;
    }
    let _protocol_magic = dec.u64().ok()?;
    dec.skip().ok()?; // prev_hash
    dec.skip().ok()?; // body_proof

    // consensus_data
    let (epoch, rel_slot, difficulty) = match is_ebb {
        0 => {
            // EBB: consensus_data = [uint(epoch), array(1)[difficulty]]
            if dec.array().ok()? != Some(2) {
                return None;
            }
            let epoch = dec.u64().ok()?;
            if dec.array().ok()? != Some(1) {
                return None;
            }
            let difficulty = dec.u64().ok()?;
            (epoch, 0u64, difficulty)
        }
        1 => {
            // Main: consensus_data =
            //   [array(2)[epoch, rel_slot], issuer, array(1)[difficulty], block_sig]
            if dec.array().ok()? != Some(4) {
                return None;
            }
            if dec.array().ok()? != Some(2) {
                return None;
            }
            let epoch = dec.u64().ok()?;
            let rel_slot = dec.u64().ok()?;
            dec.skip().ok()?; // issuer
            if dec.array().ok()? != Some(1) {
                return None;
            }
            let difficulty = dec.u64().ok()?;
            (epoch, rel_slot, difficulty)
        }
        _ => return None,
    };

    let slot = if byron_epoch_length > 0 {
        epoch
            .checked_mul(byron_epoch_length)?
            .checked_add(rel_slot)?
    } else {
        epoch
            .checked_mul(MAINNET_BYRON_SLOTS_PER_EPOCH)?
            .checked_add(rel_slot)?
    };
    Some((slot, difficulty))
}

/// Convert a `dugite_primitives::block::Point` to a network `codec::Point`.
fn to_codec_point(p: &Point) -> CodecPoint {
    match p {
        Point::Origin => CodecPoint::Origin,
        Point::Specific(slot, hash) => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(hash.as_ref());
            CodecPoint::Specific(slot.0, arr)
        }
    }
}

/// Convert a network `codec::Point` to a `dugite_primitives::block::Point`.
fn from_codec_point(p: &CodecPoint) -> Point {
    match p {
        CodecPoint::Origin => Point::Origin,
        CodecPoint::Specific(slot, hash) => Point::Specific(
            dugite_primitives::time::SlotNo(*slot),
            dugite_primitives::hash::Hash32::from_bytes(*hash),
        ),
    }
}

/// Inputs to `build_known_points`.
///
/// This struct exists as a seam between the live `chainsync_client_task` and
/// the pure point-construction logic so the latter can be unit/property-tested
/// without spinning up a ChainDB / LedgerState.
#[derive(Debug, Clone)]
pub(crate) struct KnownPointsInputs {
    /// Current ledger tip (`Point::Origin` if no blocks applied).
    pub(crate) ledger_tip: Point,
    /// Most-recent canonical points from the VolatileDB (oldest at the end of
    /// the slice is fine — relative order is preserved, duplicates dropped).
    /// Typically `ChainDB::get_chain_points(N)`.
    pub(crate) volatile_chain_points: Vec<Point>,
    /// ImmutableDB tip — the finalized anchor that *every* peer on the network
    /// is expected to know. Issue #552: must be unconditionally included so a
    /// peer that is behind our ledger tip but ahead of our immutable tip can
    /// still find an intersection.
    pub(crate) immutable_tip: Option<Point>,
    /// Sparse historical anchor points sampled from older ImmutableDB chunks
    /// (typically one per chunk). Provides ancestor coverage for peers that
    /// have rolled back past our immutable tip or are on a deep fork.
    pub(crate) deep_historical: Vec<Point>,
    /// Set when the VolatileDB blocks above the ledger tip do not connect to
    /// the ledger tip (e.g. orphan fork blocks left over by a flushed fork
    /// snapshot). In that mode we must *not* offer volatile points — only the
    /// canonical ImmutableDB anchors are safe.
    pub(crate) chain_diverged: bool,
}

impl Default for KnownPointsInputs {
    fn default() -> Self {
        Self {
            ledger_tip: Point::Origin,
            volatile_chain_points: Vec::new(),
            immutable_tip: None,
            deep_historical: Vec::new(),
            chain_diverged: false,
        }
    }
}

/// Build the ChainSync `known_points` list offered to a peer in
/// `MsgFindIntersect`.
///
/// # Order (newest → oldest)
///
/// 1. **ImmutableDB tip** — always included when non-Origin. This is the
///    single most important anchor: a finalized point that every peer on the
///    correct network is guaranteed to know. Mirrors Haskell ouroboros-consensus
///    `chainSyncClientPipelined`, which anchors candidate fragments at the
///    LedgerDB immutable tip.
/// 2. **Ledger tip** — current applied tip (if non-Origin and not already
///    covered by the immutable tip).
/// 3. **Volatile recent points** — last `~k/2` blocks of the selected chain
///    (skipped when `chain_diverged` to avoid offering orphan fork blocks).
/// 4. **Deep historical anchors** — one point per older ImmutableDB chunk,
///    giving exponential-ish coverage back to genesis. Lets a peer recover
///    from forks deeper than our volatile window.
/// 5. **Origin** — final fallback.
///
/// # Issue #552
///
/// Before this refactor, the "ledger leads" branch (the common case at a
/// stable tip) offered `ledger_tip + last 10 volatile points` only. When a
/// peer's chain was strictly behind our ledger tip, none of those points
/// matched the peer's chain prefix and the intersection landed at Origin,
/// after which the node never re-intersected — visible as "tip-age growing"
/// while no headers arrived. The fix is to *always* include the immutable
/// tip + sparse deep history in the known_points list, regardless of branch.
///
/// # Determinism & uniqueness
///
/// - Duplicates are dropped (first-occurrence wins).
/// - `Point::Origin` is filtered out everywhere except the explicit final
///   fallback (so it always appears exactly once at the end).
/// - Empty chain (everything Origin / empty) yields `[Origin]`.
///
/// # Output bound
///
/// Capped at `MAX_KNOWN_POINTS` to keep `MsgFindIntersect` under the Haskell
/// peer's payload limit. Inputs beyond the cap are silently truncated; the
/// caller is expected to size `volatile_chain_points` / `deep_historical`
/// sensibly (typical: 10 + 8).
pub(crate) fn build_known_points(inp: &KnownPointsInputs) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(MAX_KNOWN_POINTS);

    let push_unique = |out: &mut Vec<Point>, p: Point| {
        if p == Point::Origin {
            return;
        }
        if out.len() >= MAX_KNOWN_POINTS - 1 {
            // Reserve one slot for the final Origin entry.
            return;
        }
        if !out.contains(&p) {
            out.push(p);
        }
    };

    // Order matters: `MsgFindIntersect` returns the FIRST point in this list
    // that is on the peer's chain, so the list MUST be NEWEST-FIRST for an
    // up-to-date peer to intersect at OUR TIP.  If the immutable tip is listed
    // first (as it was for issue #552), an up-to-date peer intersects at the
    // immutable tip instead and then re-streams the entire volatile window
    // (every block between the immutable tip and our real tip — up to the
    // background-maintenance retention window, ~10 k blocks) which we already
    // have.  That wasted re-stream drives `prune_already_known_pending_headers`
    // (the #1 CPU consumer during bulk sync) and a peer reconnection churn.
    // Issue #552 only requires the immutable tip to be PRESENT so a peer behind
    // our ledger tip but ahead of our immutable tip can still find an
    // intersection — it does NOT require it to be first.  A behind peer simply
    // falls through the newer points it doesn't have and matches the immutable
    // tip / deep-historical anchors lower in the list.
    //
    // Issue #927: the ledger-tip-first order is only newest-first while
    // ledger >= immutable.  When the ledger tip is BEHIND the immutable tip
    // (crash recovery, #926 index hole, replay apply-failure), offering the
    // stale ledger tip first makes every up-to-date peer intersect there, and
    // its protocol-mandated initial rollback to that sub-immutable point used
    // to trip the #699 guard — every peer disconnected ~1 s after handshake,
    // forever.  In that state the immutable tip must precede the stale ledger
    // tip (true newest-first by slot); a peer matching the immutable tip lets
    // the gap-bridge advance the ledger from ChainDB.
    let ledger_slot = inp.ledger_tip.slot().map(|s| s.0).unwrap_or(0);
    let immutable_slot = inp
        .immutable_tip
        .as_ref()
        .and_then(|p| p.slot())
        .map(|s| s.0)
        .unwrap_or(0);
    let ledger_behind_immutable = immutable_slot > ledger_slot;

    if ledger_behind_immutable {
        // #927 anomalous state — newest-first by slot: volatile points are
        // anchored at the immutable tip (above it), then the immutable tip,
        // then the stale ledger tip as a fallback for peers that are behind
        // our immutable tip but still carry our applied prefix.
        if !inp.chain_diverged {
            for p in &inp.volatile_chain_points {
                push_unique(&mut out, p.clone());
            }
        }
        if let Some(imm) = inp.immutable_tip.clone() {
            push_unique(&mut out, imm);
        }
        push_unique(&mut out, inp.ledger_tip.clone());
    } else {
        // 1. Ledger tip — our canonical applied tip; valid even when the
        //    surrounding volatile chain is divergent.  An up-to-date peer matches
        //    here and streams forward with no re-fetch of already-applied blocks.
        push_unique(&mut out, inp.ledger_tip.clone());

        // 2. Volatile recent points (newest → older) — skipped when chain_diverged.
        //    `get_chain_points` returns these newest-first; the tip itself dedupes
        //    against the ledger tip above.
        if !inp.chain_diverged {
            for p in &inp.volatile_chain_points {
                push_unique(&mut out, p.clone());
            }
        }

        // 3. ImmutableDB tip — unconditionally INCLUDED (issue #552), now after the
        //    volatile window so it only serves as the fallback intersection for a
        //    peer that is behind our volatile window.
        if let Some(imm) = inp.immutable_tip.clone() {
            push_unique(&mut out, imm);
        }
    }

    // 4. Deep historical anchors from older ImmutableDB chunks (newest → oldest).
    for p in &inp.deep_historical {
        push_unique(&mut out, p.clone());
    }

    // 5. Final fallback — Origin always closes the list.
    out.push(Point::Origin);
    out
}

/// #927: should the immutable-tip guard exempt this `MsgRollBackward`?
///
/// The server's initial rollback to the exact point it answered in
/// `MsgIntersectFound` is protocol-mandated, not evidence — Haskell's
/// ChainSync client re-anchors the candidate fragment at the intersection
/// without ever running the rollback-validity check on it. dugite's #699
/// immutable-tip guard used to disconnect peers for this mandated rollback
/// whenever the agreed intersection sat below the immutable tip, which with
/// a persistent ledger<immutable state (#926 index hole, replay
/// apply-failure) meant EVERY honest peer was dropped ~1 s after handshake.
///
/// Exempt iff ALL hold:
/// - it is the initial (first post-intersection) rollback,
/// - the rollback point equals the agreed intersection exactly (slot+hash —
///   a lying server that answers one point and rolls back to another stays
///   guarded),
/// - the point is at-or-above our applied ledger tip: streaming forward from
///   there is pure progress. An initial rollback BELOW the ledger tip keeps
///   the #699 disconnect (divergent-peer stall shape). In the healthy
///   ledger>=immutable state the exemption is unreachable, because the guard
///   only fires for rollback < immutable <= ledger.
pub(crate) fn is_exempt_initial_agreed_rollback(
    is_initial: bool,
    rollback_slot: u64,
    rollback_hash: Option<[u8; 32]>,
    agreed_intersection: Option<(u64, [u8; 32])>,
    ledger_tip_slot: u64,
) -> bool {
    is_initial
        && agreed_intersection.is_some_and(|(s, h)| s == rollback_slot && rollback_hash == Some(h))
        && rollback_slot >= ledger_tip_slot
}

/// Upper bound on the number of points sent in a single `MsgFindIntersect`.
/// Haskell ouroboros-network accepts up to ~32 points in the request payload;
/// we cap conservatively below that.
pub(crate) const MAX_KNOWN_POINTS: usize = 24;

/// Default count of recent VolatileDB points requested for the known_points
/// list (passed to `ChainDB::get_chain_points`).
pub(crate) const VOLATILE_POINTS_DEPTH: usize = 10;

/// Default count of deep historical anchors requested from older ImmutableDB
/// chunks (passed to `ChainDB::get_immutable_historical_points`).
pub(crate) const DEEP_HISTORICAL_DEPTH: usize = 8;

/// Classification of a server response received while the ChainSync client is
/// in `StIntersect` — i.e. immediately after sending `MsgFindIntersect`, before
/// any streaming `MsgRequestNext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntersectOutcome {
    /// Legitimate StIntersect reply: an intersection point was found.
    Found,
    /// Legitimate StIntersect reply: no intersection within our offered points.
    NotFound,
    /// A server *next-phase* response (`MsgRollForward` / `MsgRollBackward` /
    /// `MsgAwaitReply`) seen while in StIntersect. On a reused (resubscribed)
    /// mux this is prior-session residue: the previous ChainSync run pipelined
    /// `MsgRequestNext` and the server's reply was still in-flight in the kernel
    /// socket buffer when the peer was demoted+re-promoted without TCP teardown;
    /// the ingress task then routes it onto this fresh channel. It is provably
    /// NOT a reply to our `MsgFindIntersect` — the client never pipelines a
    /// request before intersection (see `try_find_intersect`) — so it is safe to
    /// discard and re-read. (Haskell typed-protocols treats a next-phase message
    /// in StIntersect as a wire-state violation and tears the run down; we
    /// recover in place for this known stale-only case instead.)
    StaleNextPhase,
    /// Any other message — client-agency `MsgRequestNext` / `MsgFindIntersect` /
    /// `MsgDone`, or anything unexpected: a genuine protocol violation.
    Invalid,
}

/// Classify a message received while in `StIntersect`. Pure and *total* over
/// every `ChainSyncMessage` variant (no wildcard arm, so adding a protocol
/// message forces an update here). See [`IntersectOutcome`] / `try_find_intersect`.
pub(crate) fn classify_intersect_response(msg: &ChainSyncMessage) -> IntersectOutcome {
    match msg {
        ChainSyncMessage::MsgIntersectFound { .. } => IntersectOutcome::Found,
        ChainSyncMessage::MsgIntersectNotFound { .. } => IntersectOutcome::NotFound,
        ChainSyncMessage::MsgRollForward { .. }
        | ChainSyncMessage::MsgRollBackward { .. }
        | ChainSyncMessage::MsgAwaitReply => IntersectOutcome::StaleNextPhase,
        ChainSyncMessage::MsgRequestNext
        | ChainSyncMessage::MsgFindIntersect(_)
        | ChainSyncMessage::MsgDone => IntersectOutcome::Invalid,
    }
}

/// Budget for the post-cancel ChainSync pipeline drain (#910).
///
/// Must stay comfortably under `PeerConnection::PROTOCOL_SHUTDOWN_TIMEOUT`
/// (5 s — dugite's `spsDeactivateTimeout`) so a drain that cannot complete
/// still lets the task exit normally and report the failure, rather than being
/// abort-killed mid-drain by the deactivation timeout.
pub(crate) const CHAINSYNC_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Drain budget when the last server message was `MsgAwaitReply` (#910).
///
/// In that state the protocol is at `StMustReply`: the one outstanding request
/// is parked until the peer mints its next block, tens of seconds away. No
/// budget we can afford would complete that drain — `demote_to_warm` holds the
/// global peer-manager write lock across it — so fail fast and let the demotion
/// escalate to a TCP close. The window is still non-zero so a block that is
/// already arriving is picked up.
pub(crate) const CHAINSYNC_DRAIN_TIMEOUT_AT_TIP: std::time::Duration =
    std::time::Duration::from_millis(250);

/// Drain budget for a cancellation observed in state `at_tip`.
pub(crate) fn chainsync_drain_budget(at_tip: bool) -> std::time::Duration {
    if at_tip {
        CHAINSYNC_DRAIN_TIMEOUT_AT_TIP
    } else {
        CHAINSYNC_DRAIN_TIMEOUT
    }
}

/// How deep the ChainSync pipeline may run, given the currently-known gap
/// between our candidate tip and the peer's reported tip (#910).
///
/// # Haskell alignment
///
/// `Ouroboros.Network.Protocol.ChainSync.PipelineDecision`'s
/// `pipelineDecisionLowHighMark` decides `Pipeline` vs `Collect` vs `Request`
/// from BOTH the current depth `n` AND the block-number distance between the
/// client's tip and the server's tip. When the client has caught up
/// (`cliTipBlockNo >= srvTipBlockNo`) it stops pipelining and issues plain
/// non-pipelined requests, so the pipeline sits at depth `Z` (zero) at the tip.
/// That is what makes `drainThePipe`-before-`MsgDone` cheap in cardano-node.
///
/// Dugite used to blast `high_mark` (default 300) `MsgRequestNext` at session
/// start and refill to `high_mark` whenever `!at_tip`, with no gap term. At the
/// tip that parks 200–300 unanswered requests in the server's read queue
/// indefinitely — the server only answers them as new blocks are minted — so a
/// demotion could never drain, and the residue landed on the next session's
/// resubscribed channel (the observed `317 stale next-phase responses in
/// StIntersect`, 45 reconnects in one 19 h preprod log).
///
/// The floor of 1 is Haskell's `Request` case: even with no known gap we keep
/// exactly one request outstanding so the server has something to answer when
/// the next block arrives, and the loop can never wedge on a zero-depth
/// pipeline.
pub(crate) fn pipeline_target_depth(gap_blocks: u64, high_mark: usize) -> usize {
    gap_blocks.min(high_mark as u64).max(1) as usize
}

/// Drain every outstanding pipelined ChainSync response, then send `MsgDone`
/// (#910).
///
/// # Haskell alignment
///
/// `Ouroboros/Network/Protocol/ChainSync/ClientPipelined.hs` makes `SendMsgDone`
/// constructible **only** at pipeline depth `Z`; the consensus client's
/// `drainThePipe` (`ChainSync/Client.hs`, `terminateAfterDrain`) therefore
/// collects and discards every outstanding pipelined response before
/// terminating. `PeerStateActions.deactivatePeerConnection` then waits for the
/// protocol to finish before the peer becomes Warm with the mux still alive,
/// and `network-mux`'s `runMiniProtocol` refuses to start a new instance unless
/// the previous one is `StatusIdle`. Together those three make it structurally
/// impossible for a fresh ChainSync instance to receive a frame belonging to
/// the prior instance.
///
/// Dugite's analogue: this drain runs in the client task's cancellation arm,
/// and `demote_to_warm` only reuses (resubscribes) the mux when the drain
/// reported success — otherwise it falls back to a full TCP close, mirroring
/// Haskell's `Mux.stop` escalation on `spsDeactivateTimeout`.
///
/// `MsgAwaitReply` does NOT consume a pipelined request (the server still owes
/// the eventual `MsgRollForward`/`MsgRollBackward`), so it is discarded without
/// decrementing — the same accounting the main loop uses.
///
/// Returns the number of frames discarded.
pub(crate) async fn drain_pipeline_and_terminate(
    channel: &mut MuxChannel,
    peer_addr: SocketAddr,
    mut outstanding: usize,
    budget: std::time::Duration,
) -> Result<usize, anyhow::Error> {
    let mut discarded = 0usize;
    let deadline = tokio::time::Instant::now() + budget;

    while outstanding > 0 {
        let data = tokio::time::timeout_at(deadline, channel.recv())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "ChainSync drain from {peer_addr} timed out with {outstanding} \
                     responses still outstanding (discarded {discarded})"
                )
            })?
            .map_err(|e| anyhow::anyhow!("ChainSync drain recv failed: {e}"))?;
        let msg =
            cs_decode(&data).map_err(|e| anyhow::anyhow!("ChainSync drain decode failed: {e}"))?;
        discarded += 1;
        match msg {
            ChainSyncMessage::MsgRollForward { .. } | ChainSyncMessage::MsgRollBackward { .. } => {
                outstanding -= 1;
            }
            // Server has no block yet; it still owes us the reply for this
            // request, so the depth is unchanged.
            ChainSyncMessage::MsgAwaitReply => {}
            // Server terminated first — the protocol is already at StDone for
            // both sides, so there is nothing left to drain and nothing to send.
            ChainSyncMessage::MsgDone => {
                debug!(
                    %peer_addr,
                    discarded,
                    "ChainSync drain: server sent MsgDone first"
                );
                return Ok(discarded);
            }
            other => {
                return Err(anyhow::anyhow!(
                    "ChainSync drain from {peer_addr}: unexpected message {other:?} \
                     (outstanding={outstanding})"
                ));
            }
        }
    }

    // Depth is now Z — `MsgDone` is legal (and, in Haskell, only now
    // constructible). Sending it leaves the server's responder idle so the mux
    // can host a fresh ChainSync instance after the next Warm→Hot promotion.
    channel
        .send(cs_encode(&ChainSyncMessage::MsgDone))
        .await
        .map_err(|e| anyhow::anyhow!("ChainSync drain MsgDone send failed: {e}"))?;
    Ok(discarded)
}

/// Is this the "the peer can only serve us from genesis" outcome? (#908)
///
/// Two wire results mean exactly the same thing once the deepest retry set —
/// which always ends in `Origin` — has been offered:
///
/// * `Some(Origin)` — the peer accepted only the genesis anchor;
/// * `None` — `MsgIntersectNotFound` for every point INCLUDING `Origin`, so the
///   peer's read pointer is at genesis regardless.
///
/// Treating `None` as "sync from Origin and see what happens" (the pre-#908
/// behaviour) wastes a round trip and defers the peer-quality verdict to
/// whether the peer happens to deliver a genesis-region header before hanging
/// up — which it usually does not, so the session died via the bearer and no
/// backoff was ever recorded. Haskell classifies this as `ForkTooDeep` at the
/// intersection, and so do we.
pub(crate) fn intersection_is_genesis_only(intersection: Option<&CodecPoint>) -> bool {
    matches!(intersection, None | Some(CodecPoint::Origin))
}

/// Read the reply to a `MsgFindIntersect` we just sent (the server is in
/// `StIntersect`), tolerating a bounded number of STALE next-phase responses.
///
/// Only `MsgIntersectFound` / `MsgIntersectNotFound` are legitimate here. On a
/// mux that was resubscribed after a warm demote+re-promote (no TCP teardown), a
/// next-phase response (`MsgRollForward` / `MsgRollBackward` / `MsgAwaitReply`)
/// from the PRIOR session can still be in-flight in the kernel socket buffer at
/// swap time and get routed onto this fresh channel by the ingress task. It is
/// provably NOT a reply to our `MsgFindIntersect` — the caller never pipelines a
/// request before intersection — so we discard it and re-read, up to
/// `max_stale_discard` (sized to the pipeline window: a demoted prior session
/// can leave that many responses in flight). Beyond that a genuinely broken
/// peer fails fast and falls
/// back to the pre-existing teardown+reconnect. Mirrors Haskell, whose
/// typed-protocols codec rejects a next-phase message in StIntersect as a
/// wire-state violation.
pub(crate) async fn read_intersect_reply(
    channel: &mut MuxChannel,
    peer_addr: SocketAddr,
    max_stale_discard: u32,
    metrics: &crate::metrics::NodeMetrics,
) -> Result<Option<CodecPoint>, anyhow::Error> {
    let mut discarded: u32 = 0;
    loop {
        let response = channel
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("ChainSync intersection response recv failed: {e}"))?;
        let intersect_msg = cs_decode(&response)
            .map_err(|e| anyhow::anyhow!("ChainSync intersection decode failed: {e}"))?;

        match intersect_msg {
            ChainSyncMessage::MsgIntersectFound {
                point,
                tip_slot,
                tip_block_number,
                ..
            } => {
                let prim_point = from_codec_point(&point);
                if discarded > 0 {
                    warn!(
                        %peer_addr,
                        discarded,
                        "ChainSync intersection found after discarding stale next-phase \
                         residue (reused-mux)",
                    );
                }
                // Issue #904: record the peer's tip HERE, not only on
                // RollForward. `should_skip_forge_for_catch_up` falls back to
                // the wall clock while `peer_tip == 0`, which wedges a block
                // producer restarted onto a stalled chain: it intersects fine
                // but never receives a RollForward, because it is itself the
                // node that would produce the next block. The intersection
                // reply already carries the peer's tip, so there is no reason
                // to keep guessing from the wall clock past this point.
                metrics.update_peer_tip(tip_slot);
                info!(
                    %peer_addr,
                    point = %prim_point,
                    tip_slot,
                    tip_block_number,
                    "ChainSync intersection found",
                );
                return Ok(Some(point));
            }
            ChainSyncMessage::MsgIntersectNotFound {
                tip_slot,
                tip_block_number,
                ..
            } => {
                if discarded > 0 {
                    warn!(
                        %peer_addr,
                        discarded,
                        "ChainSync no-intersection after discarding stale next-phase \
                         residue (reused-mux)",
                    );
                }
                info!(
                    %peer_addr,
                    tip_slot,
                    tip_block_number,
                    "ChainSync MsgIntersectNotFound",
                );
                // #904: NotFound carries the peer's tip too. We have no
                // intersection yet, but we do now know where the peer is, and
                // that beats the wall-clock fallback in the forge gate.
                metrics.update_peer_tip(tip_slot);
                return Ok(None);
            }
            other => match classify_intersect_response(&other) {
                IntersectOutcome::StaleNextPhase => {
                    discarded += 1;
                    if discarded > max_stale_discard {
                        return Err(anyhow::anyhow!(
                            "ChainSync: {discarded} stale next-phase responses in StIntersect \
                             from {peer_addr} (bound {max_stale_discard}); giving up (reconnect)"
                        ));
                    }
                    // Silently discard prior-session residue; the count is logged
                    // ONCE on recovery (the Found/NotFound arms) to avoid
                    // per-frame spam — a deeply-pipelined prior session can leave
                    // up to ~DUGITE_PIPELINE_DEPTH (default 300) frames in flight.
                    // loop re-reads
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "ChainSync unexpected response to MsgFindIntersect: {other:?}"
                    ))
                }
            },
        }
    }
}

/// Drop pending headers whose block is already stored in the local ChainDB.
///
/// Use **hash equality** rather than slot comparison: after a peer issues
/// `MsgRollBackward` at the live tip and switches to a competing fork, the new
/// fork's headers may carry slots that are at or below our current ledger tip
/// but with **different** hashes.  A slot-based filter (`h.slot > applied_slot`)
/// drops those fork headers, leaving BlockFetch unable to assemble the parent
/// chain — `walk_chain_back` from the new fork's tip terminates inside an
/// unreachable orphan island, chain selection silently never fires
/// `TriggeredFork`, and the node remains stuck on the abandoned fork until
/// restart (which then sees no canonical snapshot and falls back to genesis
/// replay).  Observed in production on 2026-04-26.
///
/// This matches Haskell `ouroboros-consensus`: `theirFrag` retains every header
/// on the candidate fragment (anchored at the rollback / intersection point);
/// BlockFetch fetches every block on `theirFrag` not on `curChain`, regardless
/// of block number ordering.  Chain selection fires per-block via
/// `chainSelectionForBlock` once `preferCandidate` (block-number ordering)
/// favours the new fork.
// #767: the production callers (MsgRollForward + the refill ticker) now prune
// OFF the candidate_chains write lock via an inline retain-by-hash (snapshot →
// chain_db.read() alone → retain) to break the lock convoy, so this helper is
// now exercised only by the prune-semantics tests. Kept (with its regression
// tests) as the canonical statement of the prune rule.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn prune_already_known_pending_headers(
    headers: &mut Vec<PendingHeader>,
    chain_db: &dugite_storage::ChainDB,
) {
    use dugite_primitives::hash::Hash32;
    headers.retain(|h| !chain_db.has_block(&Hash32::from_bytes(h.hash)));
}

/// Pause refilling the ChainSync pipeline when this many unfetched headers
/// are queued in `pending_headers`.
///
/// During bulk sync from origin, ChainSync receives headers at network speed
/// (pipelined, header-sized) while BlockFetch (single-active, serial,
/// block-sized) fetches the corresponding bodies far slower.  Without a gate,
/// `pending_headers` would grow without bound until either OOM or — as
/// previously coded with a silent `drain(..)` cap — the oldest unfetched
/// headers were silently dropped.  Silent drops created permanent chain
/// gaps that `chain_sel` could never bridge: every reconnecting peer's
/// fragment also got drained, so no peer ever held the full sequence.
/// Observed live on preprod 2026-05-10: sync stalled at block 904108 and
/// every subsequent fork tip reported `fork unreachable — StoreButDontChange`
/// for ≥1.7 hours with no recovery path.
///
/// The fix is wire-level backpressure: when at the pause threshold, we stop
/// sending `MsgRequestNext`, the pipeline drains naturally, the peer pauses,
/// BlockFetch catches up, and refilling resumes once we cross
/// `PENDING_HEADERS_RESUME`.
pub(crate) const PENDING_HEADERS_PAUSE: usize = 10_000;

/// How many headers may arrive between full `pending_headers` prunes on the
/// MsgRollForward path.  Each header is still individually `has_block`-checked
/// on arrival (O(1)); the full O(pending) prune that drains
/// BlockFetch-completed entries runs only this often, turning the former
/// O(N²)-per-batch per-header scan into amortised O(N).  Bounded well below the
/// PAUSE/RESUME hysteresis gap so the transient over-count of not-yet-pruned
/// (but already-fetched) entries cannot meaningfully perturb the backpressure
/// gate.
pub(crate) const PENDING_PRUNE_INTERVAL: u32 = 256;

/// Resume refilling once `pending_headers` drops below this.  The gap
/// between PAUSE and RESUME is hysteresis — without it, the pipeline
/// would thrash on every fetched block.
pub(crate) const PENDING_HEADERS_RESUME: usize = 6_000;

/// Maximum size of a single header CBOR payload received via `MsgRollForward`.
///
/// B12: Without this cap, a malicious peer could send headers up to the MUX
/// frame limit (~65 KB) and fill `pending_headers` with
/// `PENDING_HEADERS_PAUSE × 65 KB ≈ 650 MB` of data.  Cardano Conway headers
/// are typically ≤ 2 KB; we allow 8 KB (4× safety margin) for future eras.
/// At that size: 10,000 × 8 KB = 80 MB peak, which is safe.
pub(crate) const MAX_HEADER_CBOR_BYTES: usize = 8_192;

/// Decide whether the ChainSync pipeline should send more `MsgRequestNext`.
///
/// Returns `true` only when:
///   - we are not at the peer's tip,
///   - the in-flight pipeline has drained to/below `low_mark`,
///   - the candidate fragment has room (hysteresis-gated by `*throttled`).
///
/// `*throttled` is updated as a side effect to track the hysteresis state:
///   - `pending_count >= PENDING_HEADERS_PAUSE`  → `*throttled = true`
///   - `pending_count <  PENDING_HEADERS_RESUME` → `*throttled = false`
///   - between the two thresholds, the previous state is preserved.
pub(crate) fn should_refill_pipeline(
    at_tip: bool,
    outstanding: usize,
    low_mark: usize,
    pending_count: usize,
    throttled: &mut bool,
) -> bool {
    if pending_count >= PENDING_HEADERS_PAUSE {
        *throttled = true;
    } else if pending_count < PENDING_HEADERS_RESUME {
        *throttled = false;
    }
    !at_tip && outstanding <= low_mark && !*throttled
}

/// How a peer exits [`run_csj_jumper_loop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumperExit {
    /// Became a streaming role with its server cursor still at the normal
    /// intersection — fall straight through to the pipeline.
    Stream,
    /// Was PROMOTED (dynamo/objector `JumpToGoodPoint` accepted): the server
    /// cursor now sits at the far-ahead good point, so the caller MUST
    /// re-intersect at the frontier before streaming (#735). Without this,
    /// the promoted peer streams headers from the jump point, BlockFetch
    /// downloads a far-ahead disjoint range, VolatileDB gets an unreachable
    /// gap, and the single fetcher slot wedges (`fork unreachable`).
    ///
    /// Haskell avoids the gap because a jumper's candidate INHERITS the
    /// dynamo's fragment from the immutable tip (`jTheirFragment`) and
    /// BlockFetch serves bodies off that candidate; dugite's BlockFetch
    /// needs streamed header CBOR, so the architectural equivalent is to
    /// re-stream from the frontier — the gap headers died with the old
    /// dynamo anyway.
    StreamReintersect,
    /// Disengaged or cancelled — the task is done.
    Done,
}

/// CSJ jumper protocol loop.
///
/// A jumper consumes `CsjInstruction`s from the registry: it offers
/// `MsgFindIntersect` jumps (and never `MsgRequestNext`), bisects on
/// rejection, and parks on its notify between jumps. Returns a
/// [`JumperExit`] (`Err` = protocol violation; the caller disconnects).
#[allow(clippy::too_many_arguments)]
async fn run_csj_jumper_loop(
    channel: &mut MuxChannel,
    peer_addr: SocketAddr,
    csj: &Arc<crate::csj::CsjRegistry>,
    notify: &tokio::sync::Notify,
    peer_state: &crate::genesis_peer_state::PeerChainState,
    byron_epoch_length: u64,
    cancel: &CancellationToken,
) -> Result<JumperExit> {
    use crate::csj::CsjInstruction;
    // Set when ANY accepted MsgFindIntersect moved the server cursor away
    // from the original intersection — a regular jump OR a JumpToGoodPoint
    // promotion handshake. A peer whose cursor jumped MUST re-intersect at
    // the frontier before streaming (#735): a backfilled dynamo (promoted
    // with `starting: None` after the old dynamo died) reaches RunNormally
    // directly, so keying this on the good-point handshake alone left it
    // streaming from its far-ahead jump point — the contiguity guard then
    // (correctly) declined every range and the sync stalled (observed live
    // on the mainnet CSJ soak, 2026-06-11 11:29).
    let mut cursor_jumped = false;
    loop {
        match csj.next_instruction(&peer_addr) {
            CsjInstruction::RunNormally | CsjInstruction::Restart => {
                // Became dynamo/objector (or a disengaged peer that now runs
                // normal ChainSync). Fall through to the streaming pipeline.
                return Ok(if cursor_jumped {
                    JumperExit::StreamReintersect
                } else {
                    JumperExit::Stream
                });
            }
            CsjInstruction::Wait => {
                // 1s timeout is a safety net: even if a jump notification is
                // missed, the loop re-reads next_instruction and picks up a
                // pending jump (next_jump is set losslessly by setJumps).
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(JumperExit::Done),
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                }
            }
            instr @ (CsjInstruction::Jump(_) | CsjInstruction::JumpToGoodPoint(_)) => {
                let (ji, is_good_point) = match instr {
                    CsjInstruction::Jump(ji) => (ji, false),
                    CsjInstruction::JumpToGoodPoint(ji) => (ji, true),
                    _ => unreachable!(),
                };
                // A jump to Origin always intersects — skip the wire.
                let accepted = match ji.tip_point() {
                    None => true,
                    Some((slot, hash)) => {
                        let probe = vec![CodecPoint::Specific(slot, hash)];
                        let find = cs_encode(&ChainSyncMessage::MsgFindIntersect(probe));
                        channel.send(find).await.map_err(|e| {
                            anyhow::anyhow!("CSJ jump MsgFindIntersect send failed: {e}")
                        })?;
                        // Await exactly one intersect response (cancellable).
                        let data = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Ok(JumperExit::Done),
                            r = channel.recv() => r.map_err(|e| {
                                anyhow::anyhow!("CSJ jump recv failed: {e}")
                            })?,
                        };
                        match cs_decode(&data)
                            .map_err(|e| anyhow::anyhow!("CSJ jump decode failed: {e}"))?
                        {
                            ChainSyncMessage::MsgIntersectFound { point, .. } => {
                                // Haskell: IntersectFound at any point OTHER
                                // than the probed one is InvalidJumpResponse.
                                let found = match from_codec_point(&point) {
                                    Point::Specific(s, h) => s.0 == slot && *h.as_bytes() == hash,
                                    Point::Origin => false,
                                };
                                if !found {
                                    return Err(anyhow::anyhow!(
                                        "CSJ: {peer_addr} InvalidJumpResponse \
                                         (IntersectFound at unexpected point)"
                                    ));
                                }
                                true
                            }
                            ChainSyncMessage::MsgIntersectNotFound { .. } => false,
                            other => {
                                return Err(anyhow::anyhow!(
                                    "CSJ: {peer_addr} unexpected jump response: {other:?}"
                                ));
                            }
                        }
                    }
                };
                let _ = byron_epoch_length; // jumps carry no Byron headers
                if accepted && ji.tip_point().is_some() {
                    // The server cursor moved to the accepted jump point —
                    // whatever role this peer later assumes, it must
                    // re-intersect at the frontier before streaming (#735).
                    cursor_jumped = true;
                }
                let replace = if is_good_point {
                    csj.process_good_point_result(&peer_addr, accepted)
                } else {
                    csj.process_jump_result(&peer_addr, ji.clone(), accepted)
                };
                if replace {
                    // updateChainSyncState: take the jump's fragment so the
                    // GDD sees this jumper's (now dynamo-aligned) candidate.
                    peer_state.replace_fragment(ji.fragment.clone());
                }
            }
        }
    }
}

/// Re-intersect a freshly-promoted CSJ dynamo/objector at the frontier
/// (#735).
///
/// The promotion handshake (`JumpToGoodPoint`) left the peer's server
/// cursor at its last accepted jump point — potentially hundreds of
/// thousands of slots above our applied frontier. Streaming from there
/// would hand BlockFetch a far-ahead disjoint range (the `fork
/// unreachable` fetcher wedge). Re-anchor the session at the frontier so
/// the promoted peer streams the gap headers itself — the dugite
/// equivalent of Haskell's inherited `jTheirFragment` candidate (see
/// [`JumperExit::StreamReintersect`]).
///
/// Known points: selected tip → recent volatile points → immutable tip.
/// A promoted peer served (or agreed to) our chain's headers, so a
/// `MsgIntersectNotFound` here means it rolled back off our chain —
/// disconnect.
async fn reintersect_promoted_peer(
    channel: &mut MuxChannel,
    peer_addr: SocketAddr,
    chain_db: &Arc<RwLock<dugite_storage::ChainDB>>,
    peer_state: &crate::genesis_peer_state::PeerChainState,
    cancel: &CancellationToken,
) -> Result<Option<CodecPoint>> {
    let mut points: Vec<CodecPoint> = Vec::new();
    {
        let db = chain_db.read().await;
        if let Some((slot, hash, _bn)) = db.get_tip_info() {
            points.push(CodecPoint::Specific(slot.0, *hash.as_bytes()));
        }
        for p in db.get_chain_points(VOLATILE_POINTS_DEPTH) {
            if let Point::Specific(slot, hash) = p {
                let cp = CodecPoint::Specific(slot.0, *hash.as_bytes());
                if !points.contains(&cp) {
                    points.push(cp);
                }
            }
        }
        if let Some(Point::Specific(slot, hash)) = db.get_immutable_tip_point() {
            let cp = CodecPoint::Specific(slot.0, *hash.as_bytes());
            if !points.contains(&cp) {
                points.push(cp);
            }
        }
    }
    if points.is_empty() {
        points.push(CodecPoint::Origin);
    }
    points.truncate(MAX_KNOWN_POINTS);

    let find = cs_encode(&ChainSyncMessage::MsgFindIntersect(points));
    channel
        .send(find)
        .await
        .map_err(|e| anyhow::anyhow!("CSJ reintersect MsgFindIntersect send failed: {e}"))?;
    let data = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(None),
        r = channel.recv() => r.map_err(|e| {
            anyhow::anyhow!("CSJ reintersect recv failed: {e}")
        })?,
    };
    match cs_decode(&data).map_err(|e| anyhow::anyhow!("CSJ reintersect decode failed: {e}"))? {
        ChainSyncMessage::MsgIntersectFound { point, .. } => {
            info!(
                %peer_addr,
                point = ?point,
                "CSJ: promoted peer re-intersected at frontier — streaming from there"
            );
            peer_state.set_anchor(match &point {
                CodecPoint::Specific(slot, hash) => {
                    crate::genesis_peer_state::FragAnchor::Point(*slot, *hash)
                }
                CodecPoint::Origin => crate::genesis_peer_state::FragAnchor::Origin,
            });
            Ok(Some(point))
        }
        ChainSyncMessage::MsgIntersectNotFound { .. } => Err(anyhow::anyhow!(
            "CSJ: promoted peer {peer_addr} no longer intersects our chain — disconnecting"
        )),
        other => Err(anyhow::anyhow!(
            "CSJ: {peer_addr} unexpected reintersect response: {other:?}"
        )),
    }
}

/// Per-peer ChainSync client task.
///
/// Runs on a single MuxChannel, receives headers, and updates shared
/// candidate chain state. Does NOT fetch blocks — that's the
/// BlockFetch decision task's responsibility.
///
/// This matches the Haskell architecture where ChainSync and BlockFetch
/// run as independent threads sharing state via STM.
///
/// # Lifecycle
///
/// Called by `ConnectionLifecycleManager::make_chainsync_task()` when a peer
/// is promoted to Hot. Runs until the cancellation token is triggered (peer
/// demotion/disconnect), a protocol error occurs, or the bearer closes.
///
/// On exit (regardless of reason), the peer's candidate chain entry is
/// removed from the shared map.
///
/// # Protocol Flow
///
/// 1. **Build known points** — Walk backwards through volatile chain and
///    ledger state to build intersection candidates.
/// 2. **Find intersection** — Send `MsgFindIntersect` with the known points.
/// 3. **Pipeline headers** — Send a burst of `MsgRequestNext` up to `high_mark`,
///    then refill when outstanding drops to `low_mark`.
/// 4. **Update state** — For each `MsgRollForward`, add a `PendingHeader` to
///    the shared `candidate_chains` map. For `MsgRollBackward`, trim headers
///    after the rollback point.
#[allow(clippy::too_many_arguments)]
pub async fn chainsync_client_task(
    mut channel: MuxChannel,
    peer_addr: SocketAddr,
    candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>>,
    chain_db: Arc<RwLock<dugite_storage::ChainDB>>,
    ledger_state: Arc<RwLock<dugite_ledger::LedgerState>>,
    // Issue #654 — lock-free read view of stable ledger state and a watch
    // channel that fires on every tip advance. The receive loop uses the
    // view to look up the per-call stability window without parking on
    // `ledger_state`, and parks on `tip_rx.changed()` when an incoming
    // header lies beyond the current forecast horizon (matches the
    // wake-on-tip-advance design in #652 C4).
    ledger_view: Arc<arc_swap::ArcSwap<super::ledger_view::LedgerView>>,
    mut ledger_tip_rx: watch::Receiver<u64>,
    // Issue #654 P1.b — seed Praos engine cloned per-call via
    // `validate_header_full_with_counters` (per-peer counter override,
    // see #652 C1). Read-only inside the task; the global Praos engine
    // is owned exclusively by the body-apply path on `Node`.
    consensus_seed: Arc<dugite_consensus::praos::OuroborosPraos>,
    // Issue #655 P2.b — shared map of header hashes that passed eager
    // validation, keyed by epoch at validation time. Apply path may
    // skip the apply-time crypto re-check when the flag is enabled and
    // the entry matches the current epoch. Inserted here on success.
    eagerly_validated_headers: Arc<
        parking_lot::Mutex<HashMap<dugite_primitives::hash::Hash32, u64>>,
    >,
    byron_epoch_length: u64,
    // Ouroboros security parameter k (number of blocks before finality).
    // Mainnet: 2160, Preview: 432.  Rollbacks deeper than k blocks indicate
    // a dishonest peer and result in peer disconnection, matching Haskell's
    // `terminateAfterDrain RolledBackPastIntersection`.
    security_param: u64,
    // Active slots coefficient from Shelley genesis (0.05 on mainnet/preview).
    // Used to scale the rollback depth threshold from blocks to slots:
    // with coeff=0.05, ~20 slots per block, so k blocks ≈ k*20 slots.
    active_slots_coeff: f64,
    metrics: Arc<crate::metrics::NodeMetrics>,
    cancel: CancellationToken,
    // GSM event sender — emits PeerRegistered, BlockReceived, PeerTipUpdated,
    // PeerActive, PeerIdling events to the GSM actor. Uses try_send (non-blocking).
    gsm_event_tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>,
    // Shared flag: set to true on the first non-Origin MsgIntersectFound.
    // Allows the forge loop to gate on successful peer intersection.
    peer_intersection_established: Arc<std::sync::atomic::AtomicBool>,
    // Shared peer-manager handle — used by the rollback-below-immutable
    // guard (#699) to record divergence witnesses.
    peer_manager: Arc<RwLock<super::networking::NodePeerManager>>,
    // Lossless per-peer Genesis chain state (candidate fragment, idling,
    // csLatestSlot) — the Haskell `ChainSyncState` TVar analogue. Written
    // synchronously at every protocol-message site; the GSM/GDD/LoE read it
    // directly (GsmEvents remain wakeup hints only).
    peer_registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,
    // GSM state snapshot — gates the LoP bucket (active only while Syncing)
    // and the historicity check (PreSyncing/Syncing only).
    gsm_snapshot_rx: tokio::sync::watch::Receiver<crate::gsm::GsmSnapshot>,
    // Limit on Patience (capacity, rate) — `None` in praos mode or when
    // `EnableLoP=false` (Haskell ChainSyncLoPBucketDisabled).
    lop_params: Option<(u64, u64)>,
    // Historicity cutoff in seconds — `None` in praos mode (Haskell
    // `gcHistoricityCutoff = Nothing` → `HistoricityCheck.noCheck`).
    historicity_cutoff_secs: Option<u64>,
    // ChainSync Jumping coordinator — `None` in praos mode or when
    // `EnableCSJ=false` (Haskell `noJumping`: every peer streams normally).
    csj: Option<Arc<crate::csj::CsjRegistry>>,
    // #910 — set to `true` exactly when this session ended with the pipelined
    // responses drained to zero and `MsgDone` sent (or the server having sent
    // `MsgDone` first), i.e. when the mux's ChainSync instance is genuinely
    // idle. `demote_to_warm` reuses (resubscribes) the bearer ONLY then;
    // otherwise it escalates to a full TCP close, mirroring Haskell's
    // `Mux.stop` on `spsDeactivateTimeout`.
    drain_ok: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    // Register this peer's shared chain state for the lifetime of the task.
    // The anchor starts at Origin (no intersection yet): csLatestSlot=None
    // keeps the peer out of GDD density comparisons (Gate 0) while
    // idling=false blocks a spurious GSM CaughtUp — exactly Haskell's fresh
    // `ChainSyncState`. Re-anchored after MsgIntersectFound below.
    let peer_state =
        peer_registry.register(peer_addr, crate::genesis_peer_state::FragAnchor::Origin);
    // Deregister on EVERY exit path (incl. `?` errors): Drop guard.
    struct RegistryGuard {
        registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,
        addr: SocketAddr,
    }
    impl Drop for RegistryGuard {
        fn drop(&mut self) {
            self.registry.deregister(&self.addr);
        }
    }
    let _registry_guard = RegistryGuard {
        registry: peer_registry.clone(),
        addr: peer_addr,
    };
    // CSJ registration MUST be removed on EVERY exit path — including
    // `?`-propagated protocol/IO errors (a dead dynamo's connection ends the
    // task via `?`, not the happy `Ok(())` return). A leaked Dynamo
    // registration is never backfilled, so every jumper parks forever and the
    // whole sync wedges. Mirrors Haskell's `bracket`-guaranteed
    // `unregisterClient` (→ `backfillDynamo`/`electNewObjector`). The Arc is
    // installed below once `csj.register` has actually run.
    struct CsjGuard {
        csj: Option<Arc<crate::csj::CsjRegistry>>,
        addr: SocketAddr,
    }
    impl Drop for CsjGuard {
        fn drop(&mut self) {
            if let Some(csj) = &self.csj {
                csj.unregister(&self.addr);
            }
        }
    }
    let mut _csj_guard = CsjGuard {
        csj: None,
        addr: peer_addr,
    };
    // ═══════════════════════════════════════════════════════════════════════
    // Phase 1: Build known points for intersection
    // ═══════════════════════════════════════════════════════════════════════
    //
    // Walk backwards through the volatile chain and ledger state to collect
    // historical points. This gives the peer multiple candidates for finding
    // a common chain prefix, which is critical for recovery after forging
    // (our local tip may be a freshly-forged block the peer hasn't seen).

    // Snapshot the storage layer in one read-lock acquisition.
    let (chain_tip, volatile_chain_points, immutable_tip, deep_historical) = {
        let db = chain_db.read().await;
        let tip = db.get_tip().point;
        let recent = db.get_chain_points(VOLATILE_POINTS_DEPTH);
        let imm = db.get_immutable_tip_point();
        let deep: Vec<Point> = db
            .get_immutable_historical_points(DEEP_HISTORICAL_DEPTH)
            .into_iter()
            .map(|(slot, hash)| Point::Specific(dugite_primitives::time::SlotNo(slot), hash))
            .collect();
        (tip, recent, imm, deep)
    };

    let ledger_tip = ledger_state.read().await.tip.point.clone();
    let ledger_slot = ledger_tip.slot().map(|s| s.0).unwrap_or(0);
    let chain_slot = chain_tip.slot().map(|s| s.0).unwrap_or(0);

    // Detect fork divergence: check if blocks after the ledger tip in
    // ChainDB actually connect. If not, the ImmutableDB (or volatile)
    // contains orphan fork blocks — we must exclude them from the
    // intersection offer.
    let mut chain_diverged = false;
    if chain_slot >= ledger_slot && ledger_tip != Point::Origin {
        let db = chain_db.read().await;
        // Point lookup: when the ledger tip is a Byron EBB, the next block is
        // the same-slot main block — a slot-only probe would skip it and
        // falsely flag divergence.
        if let Ok(Some((_next_slot, _hash, cbor))) = db.get_next_block_after_point(
            dugite_primitives::time::SlotNo(ledger_slot),
            ledger_tip
                .hash()
                .unwrap_or(&dugite_primitives::hash::Hash32::ZERO),
        ) {
            if let Ok(block) = dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                &cbor,
                byron_epoch_length,
            ) {
                let ledger_hash = ledger_tip.hash();
                if ledger_hash.is_some_and(|h| h != block.prev_hash()) {
                    warn!(
                        %peer_addr,
                        "ChainDB fork divergence detected: blocks after ledger tip \
                         do not connect. Skipping volatile points in intersection.",
                    );
                    chain_diverged = true;
                }
            }
        }
    }

    // Build the known_points list via the pure helper (Issue #552):
    // always anchor on the ImmutableDB tip + deep historical samples so that a
    // peer behind our ledger tip but ahead of our immutable tip can still find
    // an intersection without us having to disconnect and retry.
    let known_points = build_known_points(&KnownPointsInputs {
        ledger_tip: ledger_tip.clone(),
        volatile_chain_points,
        immutable_tip,
        deep_historical,
        chain_diverged,
    });

    info!(
        %peer_addr,
        chain_tip = %chain_tip,
        ledger_tip = %ledger_tip,
        known_points_count = known_points.len(),
        chain_diverged,
        "ChainSync intersection candidates",
    );
    for (i, p) in known_points.iter().enumerate() {
        debug!(%peer_addr, idx = i, point = %p, "known_point");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 2: Find intersection with MsgFindIntersect (with retry)
    // ═══════════════════════════════════════════════════════════════════════
    //
    // Try progressively deeper points if the peer rejects our initial set.
    // This handles peers on a different fork or peers that have pruned recent
    // history.  Retry attempts use deeper ImmutableDB historical points
    // before falling back to Origin.

    /// Send MsgFindIntersect with the given points and return the result.
    async fn try_find_intersect(
        channel: &mut MuxChannel,
        peer_addr: SocketAddr,
        points: &[CodecPoint],
        metrics: &crate::metrics::NodeMetrics,
    ) -> Result<Option<CodecPoint>, anyhow::Error> {
        let find_msg = cs_encode(&ChainSyncMessage::MsgFindIntersect(
            points
                .iter()
                .map(|p| match p {
                    CodecPoint::Origin => dugite_network::codec::Point::Origin,
                    CodecPoint::Specific(s, h) => dugite_network::codec::Point::Specific(*s, *h),
                })
                .collect(),
        ));
        channel
            .send(find_msg)
            .await
            .map_err(|e| anyhow::anyhow!("ChainSync MsgFindIntersect send failed: {e}"))?;

        // After MsgFindIntersect the server is in StIntersect; read the reply,
        // tolerating bounded stale next-phase residue from a reused mux (see
        // `read_intersect_reply` / `classify_intersect_response`).
        //
        // INVARIANT: that tolerance is sound ONLY because we do NOT pipeline any
        // request before this read — do not add a pre-intersection
        // MsgRequestNext without revisiting `read_intersect_reply`.
        //
        // #910: since the demotion path now drains the pipeline to zero and
        // sends `MsgDone` before the mux may be reused — and falls back to a
        // full TCP close when it cannot — a reused mux should carry NO residue
        // at all, matching Haskell (whose typed-protocols codec rejects a
        // next-phase message in `StIntersect` outright). The former bound was
        // `DUGITE_PIPELINE_DEPTH + 16` (316), which was both a divergence and,
        // on its own terms, undersized: at the tip one `MsgRequestNext` can
        // yield TWO messages (`MsgAwaitReply` plus the eventual roll), so the
        // worst-case residue was ~2x depth and the bound was observed to blow
        // (`317 stale next-phase responses ... (bound 316)`, 45x in one 19 h
        // preprod log). The bound is now a small belt-and-braces margin: any
        // residue at all is a bug or a misbehaving peer, and both are better
        // surfaced as a reconnect than absorbed silently.
        const MAX_STALE_INTERSECT_RESIDUE: u32 = 8;
        read_intersect_reply(channel, peer_addr, MAX_STALE_INTERSECT_RESIDUE, metrics).await
    }

    // Attempt 1: use the known_points we built above.
    let codec_points: Vec<CodecPoint> = known_points.iter().map(to_codec_point).collect();
    let mut intersection =
        try_find_intersect(&mut channel, peer_addr, &codec_points, &metrics).await?;

    // Retry with progressively deeper ImmutableDB points if not found.
    if intersection.is_none() {
        let retry_depths: &[usize] = &[16, 64, 256];
        for (attempt, &depth) in retry_depths.iter().enumerate() {
            let db = chain_db.read().await;
            let deep_points: Vec<CodecPoint> = db
                .get_immutable_historical_points(depth)
                .iter()
                .map(|(slot, hash)| CodecPoint::Specific(*slot, hash.0))
                .collect();
            drop(db);

            if deep_points.is_empty() {
                // No deeper points available — fall through to Origin.
                break;
            }

            warn!(
                %peer_addr,
                attempt = attempt + 2,
                depth,
                points = deep_points.len(),
                "ChainSync intersection retry with deeper points",
            );

            // Include Origin as final fallback in the retry set.
            let mut retry_points = deep_points;
            retry_points.push(CodecPoint::Origin);

            if let Some(found) =
                try_find_intersect(&mut channel, peer_addr, &retry_points, &metrics).await?
            {
                intersection = Some(found);
                break;
            }
        }

        if intersection.is_none() {
            info!(
                %peer_addr,
                "ChainSync no intersection after retries — peer shares no history \
                 above genesis with any offered point",
            );
        }
    }

    // Bug A fix (refined 2026-05-16): disconnect when intersection lands at
    // Origin with non-Origin local ledger tip AND the local chain has grown
    // beyond the security window (`k = security_param` blocks). An Origin
    // anchor requires VolatileDB::switch_chain to roll back the entire
    // selected chain back to genesis (or to the immutable anchor). If the
    // local chain is within `k` blocks of genesis, the rollback fits the
    // volatile window — accept Origin and let the RollForward stream feed us
    // the peer's blocks; `process_add_block` (with the Bug B LedgerSeq fix
    // and Bug D Praos tiebreaker) will adopt the peer's chain when it
    // exceeds ours. If the local chain is beyond `k`, rollback exceeds the
    // window and we'd hit `StoreButDontChange` for every peer block — so
    // disconnect per the original Bug A semantics; the peer manager will
    // reconnect after backoff and by then the peer has typically advanced.
    //
    // For the local-devnet startup race (relay accepts inbound from a
    // freshly-started dbp whose tip is still Origin): local chain has at
    // most ~k blocks, gap is small, we accept Origin and dbp's chain
    // diffuses correctly. Pre-refinement, the unconditional disconnect
    // killed the relay→dbp chainsync session at second 1 of every soak,
    // and the peer manager had no path to re-promote (inbound connection
    // already up), causing permanent divergence (Bug E).
    //
    // Issue #552 (2026-05-20): the structural fix is in `build_known_points`,
    // which now unconditionally offers the ImmutableDB tip + deep historical
    // anchors. This block is retained as defense-in-depth — it still fires
    // for genuine fork-divergence cases where the peer's chain has *no*
    // overlap with any of our offered points and our local chain is past k.
    //
    // See: docs/superpowers/specs/2026-05-16-bug-a-stale-intersection-fix.md
    //
    // #908(b): `MsgIntersectNotFound` for every offered point — including the
    // deepest retry set, which ends in `Origin` — is the SAME outcome as an
    // Origin intersection: the peer can only serve us from genesis. Dugite used
    // to log "syncing from Origin" and stream anyway, which wastes a round trip
    // and, worse, only surfaces as `PeerUnsuitable` if the peer actually
    // delivers a genesis-region header first. In the observed preprod flap the
    // peer hung up before that, so the ChainSync task died via the bearer
    // instead of the classified path and NO backoff was recorded — the peer was
    // re-promoted 2–6 s later, forever. Haskell classifies
    // intersection-only-at-genesis as `ForkTooDeep` and demotes; so do we,
    // directly from the NotFound outcome.
    if intersection_is_genesis_only(intersection.as_ref()) && ledger_tip != Point::Origin {
        let local_block_no = {
            let db = chain_db.read().await;
            db.get_tip_info().map(|(_, _, bn)| bn.0).unwrap_or(0)
        };
        if local_block_no > security_param {
            // EXPECTED on public networks: the peer shares no history with us
            // above genesis (it is far behind our immutable tip or on a disjoint
            // chain), so ChainSync cannot make progress and we end it. This is
            // the dugite equivalent of Haskell's
            // `ChainSyncClientResult::ForkTooDeep`, which cardano-node traces at
            // `Notice` (not `Warning`) — see Consensus tracer + ExitPolicy
            // (RepromoteDelay 120). We log at INFO and return a `PeerUnsuitable`
            // marker so `classify_chainsync_failure` maps this to
            // `PeerFailureKind::Unsuitable` (quiet) instead of a generic fault.
            //
            // NOTE — deliberate divergence from Haskell: we tear the whole
            // connection down here (Bug-A / Bug-E: chain selection cannot operate
            // across an Origin anchor, and the inbound supervisor cannot re-spawn
            // a gracefully-ended ChainSync). Haskell ends only the ChainSync
            // mini-protocol and keeps the mux alive for the other protocols. For
            // a peer this far behind, block-fetch from it is useless anyway, so
            // the teardown is harmless; the governor re-promotes after backoff.
            info!(
                %peer_addr,
                local_ledger_tip = %ledger_tip,
                local_block_no,
                security_param,
                "ChainSync intersection only at genesis (peer far behind our \
                 immutable tip / disjoint chain) — ending ChainSync, demoting \
                 for backoff (Haskell ForkTooDeep equivalent)"
            );
            let outcome = if intersection.is_none() {
                "no intersection found"
            } else {
                "intersection only at genesis"
            };
            return Err(anyhow::Error::new(
                super::connection_lifecycle::PeerUnsuitable {
                    reason: format!(
                        "peer {peer_addr} ChainSync {outcome} \
                         (block_no={local_block_no} > k={security_param})"
                    ),
                },
            ));
        }
        info!(
            %peer_addr,
            local_ledger_tip = %ledger_tip,
            local_block_no,
            security_param,
            found_intersection = intersection.is_some(),
            "ChainSync intersection at Origin with non-Origin local chain — \
             accepting because local chain is within k blocks of genesis \
             (volatile window can absorb full rollback if peer's chain wins)"
        );
    }

    // Forge gate: signal that at least one peer has established a valid
    // intersection.  We reach this point only after the Bug-A guard has
    // passed (Origin intersections with non-Origin local tip are rejected
    // above), so any intersection that survives is safe for the forge loop.
    //
    // Two valid cases:
    //   1. Specific intersection — the peer shares our chain; normal sync.
    //   2. Origin intersection with Origin local ledger — both fresh from
    //      genesis; forging can proceed immediately once the first peer blocks
    //      arrive.
    //
    // In the Bug-C scenario the BP has a self-forged fork (non-Origin local
    // tip) and the relay starts at a different chain point.  The Bug-A guard
    // catches that case above and returns Err before we reach here.  So
    // reaching this line means the intersection is either Specific or Origin-
    // with-Origin-ledger, both of which are safe — we set the flag regardless.
    peer_intersection_established.store(true, std::sync::atomic::Ordering::Relaxed);

    // Initialize candidate chain state for this peer.
    // (`mut`: a promoted CSJ peer re-intersects at the frontier below and
    // refreshes this for the GSM registration, #735.)
    let mut intersection_slot = intersection
        .as_ref()
        .map(|p| match p {
            CodecPoint::Specific(s, _) => *s,
            CodecPoint::Origin => 0,
        })
        .unwrap_or(0);
    // #927: the exact point the server answered in `MsgIntersectFound`. The
    // server's initial `MsgRollBackward` targets exactly this point; the
    // immutable-tip guard must never treat that mandated rollback as
    // divergence evidence (we offered the point ourselves). Refreshed on CSJ
    // re-intersection below.
    let mut agreed_intersection: Option<(u64, [u8; 32])> =
        intersection.as_ref().and_then(|p| match p {
            CodecPoint::Specific(s, h) => Some((*s, *h)),
            CodecPoint::Origin => None,
        });
    {
        let mut chains = candidate_chains.write().await;
        chains.insert(
            peer_addr,
            CandidateChainState {
                tip_slot: intersection_slot,
                tip_hash: intersection
                    .as_ref()
                    .map(|p| match p {
                        CodecPoint::Specific(_, h) => *h,
                        CodecPoint::Origin => [0u8; 32],
                    })
                    .unwrap_or([0u8; 32]),
                tip_block_number: 0,
                pending_headers: Vec::new(),
                ..Default::default()
            },
        );
    }

    // Emit PeerRegistered to the GSM actor after successful intersection.
    // The tip_slot here is 0 (we haven't received any headers yet); the
    // GSM will update it as PeerTipUpdated events arrive.
    // Anchor the Genesis candidate fragment at the negotiated intersection
    // (Haskell: the candidate fragment starts at the intersection point).
    peer_state.set_anchor(match &intersection {
        Some(CodecPoint::Specific(slot, hash)) => {
            crate::genesis_peer_state::FragAnchor::Point(*slot, *hash)
        }
        _ => crate::genesis_peer_state::FragAnchor::Origin,
    });

    // ── ChainSync Jumping: register + run the jumper protocol ───────────────
    //
    // The dynamo and objector fall straight through to the normal pipeline
    // below (their `next_instruction` is RunNormally); a jumper instead runs
    // `MsgFindIntersect`-only jumps here and never streams headers, which is
    // the whole point of CSJ. When CSJ is disabled every peer is RunNormally
    // and this block is a no-op — the praos path is byte-identical.
    if let Some(ref csj) = csj {
        let anchor_slot = match &intersection {
            Some(CodecPoint::Specific(slot, _)) => crate::genesis_peer_state::WithOrigin::At(*slot),
            _ => crate::genesis_peer_state::WithOrigin::Origin,
        };
        let gsm_caught_up =
            gsm_snapshot_rx.borrow().state == crate::gsm::GenesisSyncState::CaughtUp;
        let csj_notify = csj.register(peer_addr, gsm_caught_up, anchor_slot);
        // Arm the unregister guard now that the peer is in the CSJ registry.
        _csj_guard.csj = Some(csj.clone());

        // Jumper loop. Returns Stream/StreamReintersect when the peer became
        // a streaming role (dynamo/objector → fall through to the pipeline),
        // Done when the peer disengaged or was cancelled, Err to disconnect.
        let jumper_exit = run_csj_jumper_loop(
            &mut channel,
            peer_addr,
            csj,
            &csj_notify,
            &peer_state,
            byron_epoch_length,
            &cancel,
        )
        .await?;
        match jumper_exit {
            JumperExit::Done => {
                // Disengaged or cancelled — the task is done (a disengaged
                // jumper that should run normal ChainSync re-enters via
                // reconnection; dugite keeps the per-peer task simple).
                let mut chains = candidate_chains.write().await;
                chains.remove(&peer_addr);
                // `_csj_guard` unregisters on this return (→ backfill/re-elect).
                return Ok(());
            }
            JumperExit::StreamReintersect => {
                // Promoted dynamo/objector: re-anchor the session at the
                // frontier so it streams the gap headers itself and
                // BlockFetch only ever sees contiguous ranges (#735).
                let found = reintersect_promoted_peer(
                    &mut channel,
                    peer_addr,
                    &chain_db,
                    &peer_state,
                    &cancel,
                )
                .await?;
                let Some(point) = found else {
                    // Cancelled mid-handshake.
                    let mut chains = candidate_chains.write().await;
                    chains.remove(&peer_addr);
                    return Ok(());
                };
                // Refresh the pre-jump intersection bookkeeping so the GSM
                // registration below and the candidate entry reflect the
                // frontier anchor (peer_state.set_anchor was already
                // updated inside reintersect_promoted_peer).
                intersection_slot = match &point {
                    CodecPoint::Specific(s, _) => *s,
                    CodecPoint::Origin => 0,
                };
                agreed_intersection = match &point {
                    CodecPoint::Specific(s, h) => Some((*s, *h)),
                    CodecPoint::Origin => None,
                };
                let mut chains = candidate_chains.write().await;
                if let Some(state) = chains.get_mut(&peer_addr) {
                    state.tip_slot = intersection_slot;
                    state.tip_hash = match &point {
                        CodecPoint::Specific(_, h) => *h,
                        CodecPoint::Origin => [0u8; 32],
                    };
                }
            }
            JumperExit::Stream => {}
        }
    }

    if let Err(e) = gsm_event_tx.try_send(crate::gsm::GsmEvent::PeerRegistered {
        addr: peer_addr,
        intersection_slot,
        tip_slot: intersection_slot,
    }) {
        debug!(%peer_addr, "GSM PeerRegistered event dropped: {e}");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 3: Pipeline headers with MsgRequestNext
    // ═══════════════════════════════════════════════════════════════════════
    //
    // Send a burst of MsgRequestNext, then refill when outstanding drops to
    // low_mark. Depth is bounded by BOTH `high_mark` and the currently-known
    // block gap to the peer's tip — see `pipeline_target_depth` (#910): at the
    // tip the depth collapses to 1, exactly as Haskell's
    // `pipelineDecisionLowHighMark` does, which is what makes the
    // drain-before-MsgDone on demotion complete in milliseconds instead of
    // never.

    // Pipeline depth: configurable via DUGITE_PIPELINE_DEPTH env var (default: 300).
    let high_mark: usize = std::env::var("DUGITE_PIPELINE_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let low_mark: usize = high_mark * 2 / 3; // refill at ~67%
    let mut outstanding: usize = 0;
    let mut at_tip = false;
    let mut headers_received: u64 = 0;
    // The first MsgRollBackward after intersection is expected protocol
    // behavior — the server rolls the client back to the agreed intersection
    // point before sending new headers. Skip the depth check for it.
    let mut initial_rollback = true;
    // Hysteresis flag for `should_refill_pipeline`.  Set to true when
    // `pending_headers` reaches `PENDING_HEADERS_PAUSE`, cleared when it
    // drops below `PENDING_HEADERS_RESUME`.  Provides wire-level backpressure
    // so the candidate fragment cannot grow without bound when BlockFetch
    // is slower than ChainSync.
    let mut throttled = false;
    // Wake up periodically to re-prune the candidate fragment (BlockFetch
    // may have stored blocks while no MsgRollForward arrived) and to refill
    // the pipeline if the throttle has cleared.  Without this ticker, an
    // `outstanding == 0` state combined with `throttled == true` would
    // deadlock — no responses arrive to drive the message-based refill, and
    // pending_headers would never shrink below RESUME from this task's
    // perspective.
    let mut refill_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
    refill_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // #910 gap tracking for `pipeline_target_depth`.
    //
    // `client_block_no` seeds from our own chain tip: the negotiated
    // intersection is at or below it, so this can only UNDER-estimate the gap
    // (a smaller pipeline, never a stalled one — the depth floor is 1), and it
    // is corrected exactly by the first `MsgRollForward`, whose header carries
    // the authoritative block number. `peer_tip_block_no` starts at 0 and is
    // refreshed from the `tip_block_number` field every server message carries.
    let mut client_block_no: u64 = {
        let db = chain_db.read().await;
        db.get_tip_info().map(|(_, _, bn)| bn.0).unwrap_or(0)
    };
    let mut peer_tip_block_no: u64 = 0;
    let known_gap = |client: u64, peer_tip: u64| peer_tip.saturating_sub(client);

    // Send the initial pipeline burst. We do not know the peer's tip block
    // number until its first reply (the intersection reply carries a tip, but
    // the server always follows intersection with `MsgRollBackward`, which
    // carries it too), so open with a single request and let the first response
    // size the pipeline. One extra round trip at session start is immaterial
    // next to a full sync, and it is what stops a re-promoted at-tip peer from
    // being handed 300 requests it can only answer over the following ~100
    // minutes of block production.
    const INITIAL_PIPELINE_DEPTH: usize = 1;
    for _ in 0..INITIAL_PIPELINE_DEPTH {
        let req = cs_encode(&ChainSyncMessage::MsgRequestNext);
        channel
            .send(req)
            .await
            .map_err(|e| anyhow::anyhow!("ChainSync initial pipeline send failed: {e}"))?;
        outstanding += 1;
    }

    debug!(
        %peer_addr,
        high_mark,
        low_mark,
        initial_depth = INITIAL_PIPELINE_DEPTH,
        client_block_no,
        "ChainSync pipeline started",
    );

    // Limit on Patience bucket (Haskell lopBucketConfig): active only while
    // the GSM is Syncing; PreSyncing / CaughtUp / praos run the dummy.
    let mut lop_bucket = crate::leaky_bucket::LopBucket::dummy(std::time::Instant::now());
    let mut lop_was_active = false;

    // Historicity check (Haskell HistoricityCheck.judgeMessageHistoricity):
    // reject MsgRollBackward / MsgAwaitReply about headers older than the
    // cutoff while PreSyncing/Syncing (CaughtUp and praos: noCheck).
    // The slot→wallclock translation is the Shelley-anchored linear map;
    // pre-Shelley slots saturate to the Shelley start, which still vastly
    // exceeds the cutoff — exactly the verdict such ancient points deserve.
    let judge_historicity =
        |judged_slot: u64, view: &super::ledger_view::LedgerView, what: &str| -> Result<()> {
            let Some(cutoff) = historicity_cutoff_secs else {
                return Ok(());
            };
            let sc = &view.slot_config;
            let slot_ms = sc
                .zero_time
                .saturating_add(judged_slot.saturating_sub(sc.zero_slot) * sc.slot_length as u64);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let age_secs = now_ms.saturating_sub(slot_ms) / 1000;
            if age_secs > cutoff {
                return Err(anyhow::anyhow!(
                    "ChainSync: {peer_addr} sent historical {what} \
                 (judged slot {judged_slot}, age {age_secs}s > cutoff {cutoff}s) \
                 — HistoricityError, disconnecting"
                ));
            }
            Ok(())
        };

    // Main loop: receive responses and update candidate_chains.
    loop {
        // LoP reconfiguration on GSM state change (cschOnGsmStateChanged →
        // updateLopBucketConfig — refills to capacity).
        if let Some((capacity, rate)) = lop_params {
            let syncing = gsm_snapshot_rx.borrow().state == crate::gsm::GenesisSyncState::Syncing;
            if syncing != lop_was_active {
                lop_was_active = syncing;
                lop_bucket.reconfigure(std::time::Instant::now(), syncing, capacity, rate);
                debug!(%peer_addr, active = syncing, "LoP bucket reconfigured");
            }
        }
        // Haskell parity (Client.hs `pauseBucket` around `checkTime`,
        // lines 1880-1889): the bucket must not leak while WE are the
        // bottleneck — "we should not leak tokens as our peer is not
        // responsible for this waiting time". dugite's wire-level
        // backpressure (`throttled`, see `should_refill_pipeline`) is
        // exactly that state: we stopped sending MsgRequestNext, so the
        // peer owes us nothing and must not be charged patience. Without
        // this, every hot peer dies of EmptyBucket ~200 s into any bulk
        // sync where BlockFetch (not the peer) is the rate limiter (#740:
        // 237 LoP kills / 34 min → BLP churn → HAA flap → GSM flap).
        // The at-tip pause/resume pair is managed by the MsgAwaitReply /
        // MsgRollForward / MsgRollBackward handlers (Haskell sites
        // onMsgAwaitReply / handleNext).
        lop_bucket.reconcile_backpressure(std::time::Instant::now(), throttled, at_tip);
        // Empty deadline for the silent-peer case (Haskell's leak thread
        // fires exactly when the bucket empties even if no message arrives).
        let lop_deadline = lop_bucket.empty_deadline(std::time::Instant::now());

        // Check for cancellation before each recv.
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                // #910: Haskell's pipelined ChainSync client can only send
                // `MsgDone` at pipeline depth Z, so `drainThePipe` collects and
                // discards every outstanding response first. Do the same:
                // otherwise the residue of this session is still in flight when
                // `demote_to_warm` resubscribes the mux, and the NEXT session
                // reads prior-session frames as the reply to its
                // `MsgFindIntersect`.
                //
                // On failure we leave `drain_ok` false, which makes
                // `demote_to_warm` fall back to a full TCP close instead of
                // reusing the bearer — dugite's analogue of Haskell escalating
                // to `Mux.stop` when `spsDeactivateTimeout` expires.
                let budget = chainsync_drain_budget(at_tip);
                match drain_pipeline_and_terminate(&mut channel, peer_addr, outstanding, budget)
                    .await
                {
                    Ok(discarded) => {
                        drain_ok.store(true, std::sync::atomic::Ordering::Release);
                        debug!(
                            %peer_addr,
                            outstanding,
                            discarded,
                            "ChainSync task cancelled — pipeline drained, MsgDone sent"
                        );
                    }
                    Err(e) => {
                        warn!(
                            %peer_addr,
                            outstanding,
                            error = %e,
                            "ChainSync task cancelled — pipeline drain failed; \
                             the mux must not be reused for a new session"
                        );
                    }
                }
                break;
            }

            // ── Limit on Patience exhausted ─────────────────────────────
            _ = tokio::time::sleep_until(
                tokio::time::Instant::from_std(
                    lop_deadline.unwrap_or_else(|| std::time::Instant::now()
                        + std::time::Duration::from_secs(3600)),
                )
            ), if lop_deadline.is_some() => {
                return Err(anyhow::anyhow!(
                    "ChainSync: {peer_addr} exhausted the Limit on Patience \
                     (EmptyBucket) — disconnecting"
                ));
            }

            // Periodic wakeup ONLY exists to break the deadlock when
            // `throttled=true` paused the pipeline, `outstanding` drained to
            // 0, and BlockFetch has been making progress (storing blocks)
            // while no MsgRollForward arrived to drive prune/refill from the
            // message path.  In all other cases — boot, normal sync, at-tip,
            // pre-throttle — the message-driven refill arms handle pipeline
            // maintenance.  Gating the arm with `if throttled` (vs. an
            // inside-body `continue`) means tokio doesn't poll the timer
            // future at all while throttled is false, so a 50-hot-peer node
            // at-tip saves 500 wakeups/sec.
            _ = refill_ticker.tick(), if throttled => {
                // #767 (residual): prune OFF the candidate_chains write lock — same
                // convoy fix as the MsgRollForward path.  The previous code held
                // `candidate_chains.write()` across `chain_db.read()` (via the prune),
                // and this arm fires exactly when throttled (pending near the pause
                // mark) i.e. under the storm.  Snapshot the hashes under a read lock,
                // compute the already-stored set under `chain_db.read()` alone, then
                // `retain` under a brief write lock.  The two locks are never held
                // simultaneously (no convoy, no lock-order cycle); retain-by-hash is
                // race-safe vs concurrent pushes.
                let hashes: Vec<[u8; 32]> = {
                    let chains = candidate_chains.read().await;
                    match chains.get(&peer_addr) {
                        Some(entry) => entry.pending_headers.iter().map(|h| h.hash).collect(),
                        None => continue,
                    }
                };
                let known: std::collections::HashSet<[u8; 32]> = {
                    let cdb = chain_db.read().await;
                    hashes
                        .into_iter()
                        .filter(|h| {
                            cdb.has_block(&dugite_primitives::hash::Hash32::from_bytes(*h))
                        })
                        .collect()
                };
                let pending_count = {
                    let mut chains = candidate_chains.write().await;
                    match chains.get_mut(&peer_addr) {
                        Some(entry) => {
                            if !known.is_empty() {
                                entry.pending_headers.retain(|h| !known.contains(&h.hash));
                            }
                            entry.pending_headers.len()
                        }
                        None => continue,
                    }
                };

                if should_refill_pipeline(
                    at_tip,
                    outstanding,
                    low_mark,
                    pending_count,
                    &mut throttled,
                ) {
                    let to_send = pipeline_target_depth(
                        known_gap(client_block_no, peer_tip_block_no),
                        high_mark,
                    )
                    .saturating_sub(outstanding);
                    debug!(
                        %peer_addr,
                        outstanding,
                        pending_count,
                        to_send,
                        "ChainSync ticker refill",
                    );
                    for _ in 0..to_send {
                        let req = cs_encode(&ChainSyncMessage::MsgRequestNext);
                        channel.send(req).await.map_err(|e| {
                            anyhow::anyhow!(
                                "ChainSync ticker pipeline refill failed: {e}"
                            )
                        })?;
                        outstanding += 1;
                    }
                }
            }

            result = channel.recv() => {
                let data = result.map_err(|e| {
                    anyhow::anyhow!("ChainSync recv failed: {e}")
                })?;

                let msg = cs_decode(&data).map_err(|e| {
                    anyhow::anyhow!("ChainSync decode failed: {e}")
                })?;

                match msg {
                    ChainSyncMessage::MsgRollForward {
                        header,
                        tip_slot,
                        tip_hash,
                        tip_block_number,
                    } => {
                        outstanding = outstanding.saturating_sub(1);
                        if at_tip {
                            at_tip = false;
                        }
                        headers_received += 1;
                        // #910: refresh the peer's tip for `pipeline_target_depth`.
                        // (`client_block_no` is refreshed below, once the header's
                        // own block number has been safely extracted.)
                        peer_tip_block_no = tip_block_number;

                        // B12: Reject oversized headers before any parsing.
                        // Haskell's multiplexer enforces a maximum frame size of
                        // 65,535 bytes, and `blockHeaderMaxSize` is ~4 KB for Conway.
                        // We allow 8 KB (2× safety margin) to tolerate future eras.
                        // At PENDING_HEADERS_PAUSE=10,000 × 8 KB = 80 MB maximum
                        // pending_headers memory, vs the current uncapped
                        // 10,000 × 65 KB ≈ 650 MB worst case.
                        if header.len() > MAX_HEADER_CBOR_BYTES {
                            return Err(anyhow::anyhow!(
                                "ChainSync: {peer_addr} sent oversized header \
                                 ({} bytes, limit {}); disconnecting",
                                header.len(),
                                MAX_HEADER_CBOR_BYTES
                            ));
                        }

                        // Extract slot and hash from the header CBOR.
                        // The hash is blake2b_256 of the raw header bytes.
                        let hash = extract_hash_from_header(&header);
                        // B13: Never fall back to the peer-supplied `tip_slot` when
                        // the header's slot cannot be extracted.  `tip_slot` is fully
                        // controlled by the peer and substituting it would allow the
                        // peer to inject an arbitrary slot value into pipeline
                        // scheduling and epoch-boundary logic.  An undecodable header
                        // is a protocol violation → disconnect.
                        let (slot, header_block_no) =
                            match extract_slot_block_no_from_wrapped_header(
                                &header,
                                byron_epoch_length,
                            ) {
                            Some(sb) => sb,
                            None => {
                                return Err(anyhow::anyhow!(
                                    "ChainSync: {peer_addr} sent MsgRollForward with \
                                     undecodable header slot (header len={}); \
                                     disconnecting to prevent slot-injection",
                                    header.len()
                                ));
                            }
                        };
                        // #910: the header's own block number is the authoritative
                        // client-side term of the gap. It corrects the conservative
                        // local-tip seed on the very first header of the session.
                        client_block_no = header_block_no;

                        // Issue #654 — eager forecast-horizon check (Phase 1 of #652).
                        // If the header's slot lies beyond the ledger's current
                        // forecast window (`tip + 1 + stability_window`), park on
                        // the tip-advance watch channel and retry. Mirrors the
                        // wake-on-tip-advance design in #652 C4.
                        //
                        // Defense in depth: this is purely additive; body apply
                        // continues to re-validate every header against the live
                        // ledger state. The eager check just gives us earlier
                        // detection of an adversarial peer sending headers from
                        // far enough in the future that we couldn't possibly
                        // validate them, without burning CPU on each one.
                        //
                        // Byron headers are skipped: forecast horizon doesn't
                        // apply to PBFT (Byron) the same way it applies to Praos,
                        // and Byron is at most ~1 day of mainnet — the missed
                        // fan-out is negligible during bulk sync.
                        if !is_byron_wrapped_header(&header) {
                            // Haskell parity (Client.hs `pauseBucket`, Site D):
                            // pause the LoP bucket for the entire forecast-
                            // horizon wait — the peer is not responsible for
                            // our ledger lagging behind its header. The
                            // loop-top reconcile re-pauses afterwards if the
                            // wire-level throttle is engaged.
                            lop_bucket.pause(std::time::Instant::now());
                            let park_result = forecast_park_or_disconnect(
                                &peer_addr,
                                slot,
                                &ledger_view,
                                &mut ledger_tip_rx,
                                &cancel,
                                Some(&gsm_snapshot_rx),
                                // #767/#sync-eval: highest peer-reported tip →
                                // detect Praos bulk sync (behind the network) and
                                // use the patient timeout, so a transient apply
                                // stall does not churn the whole peer set.
                                metrics.get_peer_tip(),
                            )
                            .await;
                            lop_bucket.resume(std::time::Instant::now());
                            park_result?;
                        }

                        // Issue #654 P1.b — eager per-peer header
                        // validation (full VRF/KES/opcert + envelope +
                        // forecast). Defense in depth: this is purely
                        // additive; body apply continues to re-validate.
                        // Phase 1 scope: only Conway+ (era_tag >= 7).
                        // Early eras + Byron are skipped silently.
                        if !is_byron_wrapped_header(&header) {
                            let view_arc = ledger_view.load();
                            // We need access to the per-peer counter
                            // map, which lives inside CandidateChainState.
                            // Take the chains write lock briefly to run
                            // the eager call. (The window here is short:
                            // bounded by Praos crypto, ~ms.)
                            let mut chains = candidate_chains.write().await;
                            let entry = chains.entry(peer_addr).or_default();
                            match eager_validate_header(
                                &peer_addr,
                                &header,
                                slot,
                                &consensus_seed,
                                &view_arc,
                                &mut entry.eager_opcert_counters,
                            ) {
                                Ok(true) => {
                                    // Issue #655 P2.b — record this hash
                                    // as eagerly validated at the view's
                                    // current epoch. Apply path may
                                    // consult the map when its flag is
                                    // enabled. The view's epoch and the
                                    // epoch used inside the eager call
                                    // are the same: both load the same
                                    // ArcSwap pointer.
                                    let view_epoch = view_arc.epoch.0;
                                    eagerly_validated_headers
                                        .lock()
                                        .insert(
                                            dugite_primitives::hash::Hash32::from_bytes(hash),
                                            view_epoch,
                                        );
                                }
                                Ok(false) => {
                                    // Deliberate skip (Byron, pre-Conway era, or
                                    // malformed envelope). No bookkeeping entry —
                                    // apply path will validate normally.
                                }
                                Err(e) => {
                                    return Err(anyhow::anyhow!(
                                        "ChainSync: {peer_addr} eager header validation failed at slot {slot}: {e}"
                                    ));
                                }
                            }
                            drop(chains);
                        }

                        // Update candidate chain state.
                        //
                        // The captured `pending_count` (post-prune) feeds
                        // `should_refill_pipeline` below — we MUST NOT silently
                        // drop unfetched headers here.  See `PENDING_HEADERS_PAUSE`.
                        // #767 (residual): do the chain_db read(s) WITHOUT holding
                        // `candidate_chains.write()`.  The previous code nested
                        // `chain_db.read()` INSIDE the `candidate_chains.write()`
                        // critical section, so during a rollback storm (many peers
                        // re-sending headers) the continuous write contention starved
                        // the BlockFetch decision task's `candidate_chains.read()`
                        // (connection_lifecycle.rs ~2444) — no fetch ranges were built,
                        // the fetched-blocks channel drained, and the apply task
                        // stalled on an empty channel (a self-sustaining stall).
                        // `chain_db.read()` and `candidate_chains.write()` are now
                        // NEVER held simultaneously, so (a) there is no convoy and
                        // (b) this path cannot participate in any lock-order cycle
                        // (it never holds one lock while awaiting another) — unlike a
                        // naive reorder, which would invert the order vs the blockfetch
                        // decision task (candidate_chains.read → chain_db.read).
                        //
                        // Hash-based filter (unchanged semantics): a header whose hash
                        // is already in the ChainDB is skipped.  Slot-based filtering
                        // is unsound after a peer rolls back below our applied tip and
                        // switches to a competing fork (regression observed 2026-04-26)
                        // — `theirFrag` retains every candidate-fragment header and
                        // BlockFetch fetches everything not in `curChain`.
                        let already_have = {
                            let cdb = chain_db.read().await;
                            cdb.has_block(&dugite_primitives::hash::Hash32::from_bytes(hash))
                        };
                        let new_pending = if !already_have {
                            // Header-declared body size (exact range byte-accounting)
                            // + prev_hash (chain-adjacency run splitting), one decode.
                            // (None, None) for Byron / undecodable headers.
                            let (body_size, prev_hash) = extract_header_fetch_info(&header);
                            Some(PendingHeader {
                                slot,
                                hash,
                                header_cbor: header,
                                body_size,
                                prev_hash,
                            })
                        } else {
                            None
                        };

                        // Short write-lock: tip update + push + prune-due check.  O(1)
                        // plus (only every PENDING_PRUNE_INTERVAL headers) a cheap clone
                        // of the pending hashes for an OFF-LOCK prune.  No chain_db
                        // access while the write lock is held.
                        let (mut pending_count, prune_hashes) = {
                            let mut chains = candidate_chains.write().await;
                            let entry = chains.entry(peer_addr).or_insert_with(|| {
                                CandidateChainState {
                                    tip_slot: 0,
                                    tip_hash: [0u8; 32],
                                    tip_block_number: 0,
                                    pending_headers: Vec::new(),
                                    ..Default::default()
                                }
                            });
                            entry.tip_slot = tip_slot;
                            entry.tip_hash = tip_hash;
                            entry.tip_block_number = tip_block_number;
                            metrics.update_peer_tip(tip_slot);
                            if let Some(ph) = new_pending {
                                entry.pending_headers.push(ph);
                            }
                            entry.headers_since_prune = entry.headers_since_prune.saturating_add(1);
                            let prune = if entry.headers_since_prune >= PENDING_PRUNE_INTERVAL {
                                entry.headers_since_prune = 0;
                                Some(
                                    entry
                                        .pending_headers
                                        .iter()
                                        .map(|h| h.hash)
                                        .collect::<Vec<[u8; 32]>>(),
                                )
                            } else {
                                None
                            };
                            (entry.pending_headers.len(), prune)
                        };

                        // Periodic prune OFF the candidate_chains lock: compute the
                        // already-stored hashes under `chain_db.read()` alone, then
                        // `retain` under a brief write lock.  retain-by-hash is
                        // race-safe vs concurrent pushes — only hashes from the
                        // snapshot are removed; any header pushed during the window is
                        // simply kept (and re-evaluated on the next prune).
                        if let Some(hashes) = prune_hashes {
                            let known: std::collections::HashSet<[u8; 32]> = {
                                let cdb = chain_db.read().await;
                                hashes
                                    .into_iter()
                                    .filter(|h| {
                                        cdb.has_block(&dugite_primitives::hash::Hash32::from_bytes(
                                            *h,
                                        ))
                                    })
                                    .collect()
                            };
                            if !known.is_empty() {
                                let mut chains = candidate_chains.write().await;
                                if let Some(entry) = chains.get_mut(&peer_addr) {
                                    entry.pending_headers.retain(|h| !known.contains(&h.hash));
                                    pending_count = entry.pending_headers.len();
                                }
                            }
                        }

                        // Genesis candidate fragment: lossless synchronous write
                        // (csLatestSlot first, idling cleared, header appended) —
                        // the GSM events below are wakeup HINTS only.
                        peer_state.on_roll_forward(crate::genesis_peer_state::FragEntry {
                            slot,
                            hash,
                            block_no: header_block_no,
                        });

                        // CSJ (dynamo): keep this peer's jump-info snapshot
                        // current, then drive jumps when the cadence boundary
                        // is crossed (Haskell updateJumpInfo + onRollForward,
                        // BEFORE validation). No-op for non-dynamo roles and
                        // when CSJ is disabled.
                        if let Some(ref csj) = csj {
                            csj.update_jump_info(&peer_addr, peer_state.fragment_snapshot());
                            csj.on_roll_forward(&peer_addr, (slot, hash));
                        }

                        // LoP: any message resumes the leak; a header that
                        // STRICTLY advances kBestBlockNo earns one token
                        // (Haskell recvMsgRollForward: idlingStop >> lbResume,
                        // then checkLoP).
                        {
                            let now = std::time::Instant::now();
                            lop_bucket.resume(now);
                            lop_bucket.on_header(now, header_block_no);
                            if lop_bucket.is_empty(now) {
                                return Err(anyhow::anyhow!(
                                    "ChainSync: {peer_addr} exhausted the Limit on \
                                     Patience (EmptyBucket) — disconnecting"
                                ));
                            }
                        }

                        // Emit GSM events: BlockReceived, PeerTipUpdated, PeerActive.
                        // All use try_send — if the channel is full, the event is
                        // dropped silently (the periodic SyncStatus ensures convergence).
                        if let Err(e) = gsm_event_tx.try_send(crate::gsm::GsmEvent::BlockReceived {
                            addr: peer_addr,
                            slot,
                        }) {
                            debug!(%peer_addr, "GSM BlockReceived event dropped: {e}");
                        }
                        if let Err(e) = gsm_event_tx.try_send(crate::gsm::GsmEvent::PeerTipUpdated {
                            addr: peer_addr,
                            tip_slot,
                        }) {
                            debug!(%peer_addr, "GSM PeerTipUpdated event dropped: {e}");
                        }
                        if let Err(e) = gsm_event_tx.try_send(crate::gsm::GsmEvent::PeerActive {
                            addr: peer_addr,
                        }) {
                            debug!(%peer_addr, "GSM PeerActive event dropped: {e}");
                        }

                        // Log progress periodically.
                        if headers_received.is_multiple_of(10_000) {
                            debug!(
                                %peer_addr,
                                headers_received,
                                slot,
                                tip_slot,
                                tip_block_number,
                                outstanding,
                                "ChainSync header progress",
                            );
                        }

                        // Refill pipeline when outstanding drops below low_mark
                        // AND the candidate fragment has room (gates wire-level
                        // backpressure to prevent unbounded `pending_headers`
                        // growth during bulk sync — see `should_refill_pipeline`).
                        if should_refill_pipeline(
                            at_tip,
                            outstanding,
                            low_mark,
                            pending_count,
                            &mut throttled,
                        ) {
                            let to_send = pipeline_target_depth(
                                known_gap(client_block_no, peer_tip_block_no),
                                high_mark,
                            )
                            .saturating_sub(outstanding);
                            for _ in 0..to_send {
                                let req = cs_encode(&ChainSyncMessage::MsgRequestNext);
                                channel.send(req).await.map_err(|e| {
                                    anyhow::anyhow!("ChainSync pipeline refill failed: {e}")
                                })?;
                                outstanding += 1;
                            }
                        }
                    }

                    ChainSyncMessage::MsgRollBackward {
                        point,
                        tip_slot,
                        tip_hash,
                        tip_block_number,
                    } => {
                        outstanding = outstanding.saturating_sub(1);
                        // #910: refresh the peer's tip for `pipeline_target_depth`.
                        // The rollback point's block number is not on the wire, so
                        // `client_block_no` keeps its previous value — an
                        // over-estimate of our position, hence an UNDER-estimate of
                        // the gap, which is the safe direction (smaller pipeline).
                        // The next MsgRollForward supplies the exact value.
                        peer_tip_block_no = tip_block_number;

                        // A rollback means we are no longer at the chain tip —
                        // the peer has new blocks to deliver on the fork branch.
                        // Reset at_tip so the pipeline refill below is not
                        // suppressed.  Without this reset, if outstanding == 0
                        // when the rollback arrives (all pipelined requests were
                        // consumed before MsgAwaitReply), the refill condition
                        // `!at_tip && outstanding <= low_mark` evaluates false and
                        // no MsgRequestNext is ever sent again — the peer has
                        // nothing to respond to and the connection stalls forever.
                        at_tip = false;

                        let rollback_slot = match &point {
                            CodecPoint::Origin => 0,
                            CodecPoint::Specific(s, _) => *s,
                        };

                        // LoP: rollbacks also resume the leak (Haskell
                        // recvMsgRollBackward: idlingStop >> lbResume).
                        lop_bucket.resume(std::time::Instant::now());

                        // CSJ (dynamo/objector): rollback guards (Haskell
                        // onRollBackward — disengage a dynamo that rewinds
                        // behind its last jump, or an objector behind its
                        // bad point).
                        if let Some(ref csj) = csj {
                            let rb = if matches!(point, CodecPoint::Origin) {
                                crate::genesis_peer_state::WithOrigin::Origin
                            } else {
                                crate::genesis_peer_state::WithOrigin::At(rollback_slot)
                            };
                            csj.on_roll_backward(&peer_addr, rb);
                        }

                        // Historicity: judge the OLDEST header this rollback
                        // rewinds (depth-0 rollbacks are never historical —
                        // Haskell judges the HeaderStateWithTime of the
                        // oldest rewound header). Applies while
                        // PreSyncing/Syncing only.
                        if historicity_cutoff_secs.is_some()
                            && gsm_snapshot_rx.borrow().state
                                != crate::gsm::GenesisSyncState::CaughtUp
                        {
                            let frag = peer_state.fragment_snapshot();
                            let oldest_rewound = frag
                                .entries
                                .iter()
                                .find(|e| e.slot > rollback_slot)
                                .map(|e| e.slot);
                            if let Some(judged) = oldest_rewound {
                                let view = ledger_view.load();
                                judge_historicity(judged, &view, "MsgRollBackward")?;
                            }
                        }

                        // Genesis candidate fragment: truncate to the rollback
                        // point and clear idling (Haskell runs `idlingStop` in
                        // the rollback arm too — finding lop-historicity-03).
                        // A rollback target absent from our fragment (e.g. the
                        // initial post-intersection rollback after the fragment
                        // was re-anchored, or a rollback below a re-anchored
                        // prefix) conservatively RESETS the fragment to that
                        // anchor: the peer's candidate beyond it is unknown,
                        // which under-credits density (safe direction). The
                        // hard protocol guards (below-immutable, k-limit)
                        // remain the disconnect authority below.
                        {
                            let (rb_slot, rb_hash) = match &point {
                                CodecPoint::Origin => (0u64, [0u8; 32]),
                                CodecPoint::Specific(s, h) => (*s, *h),
                            };
                            if !peer_state.on_roll_backward(rb_slot, &rb_hash) {
                                peer_state.set_anchor(match &point {
                                    CodecPoint::Origin => {
                                        crate::genesis_peer_state::FragAnchor::Origin
                                    }
                                    CodecPoint::Specific(s, h) => {
                                        crate::genesis_peer_state::FragAnchor::Point(*s, *h)
                                    }
                                });
                            }
                        }
                        let prim_point = from_codec_point(&point);

                        // ── k-block rollback limit (Haskell: terminateAfterDrain) ──
                        //
                        // The first MsgRollBackward after intersection is expected
                        // protocol behavior — the server rolls the client back to
                        // the agreed intersection point. Skip the depth check for it.
                        //
                        // For subsequent rollbacks, compute depth from the ChainDB
                        // tip (NOT the ledger tip, which can diverge after Mithril
                        // import or snapshot restore). The threshold accounts for
                        // active_slots_coeff: with coeff=0.05, ~20 slots per block
                        // on average, so k blocks ≈ k*20 slots. We use 2x that as
                        // a safety margin.
                        //
                        // Haskell's `attemptRollback` returns `Nothing` when the
                        // rollback point is before the anchored fragment's anchor
                        // (deeper than k blocks), causing `ChainSyncClient` to call
                        // `terminateAfterDrain RolledBackPastIntersection`.
                        let is_initial = initial_rollback;
                        {
                            // Immutable-tip guard — applies to BOTH initial and
                            // subsequent rollbacks.  A rollback to a slot older
                            // than our ImmutableDB tip is protocol-impossible on
                            // the canonical chain (Ouroboros k-block finality):
                            // we have already committed those blocks to disk.
                            // Such an offer means the peer is on a divergent
                            // chain (or we are — see the divergence-witness
                            // tracker in `NodePeerManager`).
                            //
                            // Issue #699 — previously the initial-rollback path
                            // was exempted from this check, so a peer offering
                            // a rollback to an ancestor far behind our
                            // immutable tip was silently accepted as the
                            // post-intersection rollback.  BlockFetch then
                            // tried to fetch the peer's chain, chain-selection
                            // refused to roll back (correct), and the node
                            // stalled with no Chain extended log lines.
                            let immutable_slot = chain_db
                                .read()
                                .await
                                .get_immutable_tip_point()
                                .and_then(|p| p.slot().map(|s| s.0))
                                .unwrap_or(0);

                            // #927: the initial rollback to the EXACT agreed
                            // intersection point is protocol-mandated — the
                            // server rolls the client back to the negotiated
                            // point before streaming.  Haskell never routes
                            // this through the rollback-validity check at all:
                            // `intersectFound` re-anchors the candidate
                            // fragment at the intersection directly, and only
                            // wire rollbacks inside StNext reach the k-bound
                            // check.  Disconnecting for it is self-inflicted
                            // (we offered the point), and with a persistent
                            // ledger<immutable state (#926 index hole, replay
                            // apply-failure) it wedged the node in an all-peer
                            // flap loop with HAA permanently lost.
                            //
                            // Scope: exempt ONLY when the rollback equals the
                            // agreed intersection AND sits at-or-above our
                            // applied ledger tip — streaming forward from
                            // there is pure progress (the gap-bridge replays
                            // the ChainDB suffix).  An initial rollback BELOW
                            // the ledger tip keeps the #699 disconnect: that
                            // is the divergent-peer stall shape the guard
                            // exists for, and in the healthy ledger>=immutable
                            // state this exemption can never fire (the guard
                            // itself requires rollback < immutable <= ledger).
                            let rollback_hash = match &point {
                                CodecPoint::Specific(_, h) => Some(*h),
                                CodecPoint::Origin => None,
                            };
                            let exempt_agreed_initial = is_exempt_initial_agreed_rollback(
                                is_initial,
                                rollback_slot,
                                rollback_hash,
                                agreed_intersection,
                                ledger_view.load().tip_slot(),
                            );

                            if immutable_slot > 0
                                && rollback_slot < immutable_slot
                                && exempt_agreed_initial
                            {
                                warn!(
                                    %peer_addr,
                                    rollback_slot,
                                    immutable_slot,
                                    ledger_slot = ledger_view.load().tip_slot(),
                                    "Initial MsgRollBackward to the agreed \
                                     intersection below our immutable tip — \
                                     accepting (#927: ledger behind immutable; \
                                     streaming from the negotiated point \
                                     advances the ledger)"
                                );
                            } else if immutable_slot > 0 && rollback_slot < immutable_slot {
                                // A rollback below our immutable tip is only a
                                // genuine divergence witness when it is BOTH a
                                // mid-stream rollback (not the initial
                                // post-intersection one) AND from a peer whose own
                                // tip is at or beyond our immutable tip.  Two
                                // benign cases must NOT be counted (they would
                                // otherwise trip the multi-peer #699 shutdown):
                                //
                                //  1. peer_behind — the peer is still catching up
                                //     (its tip is below our immutable tip); it
                                //     simply cannot serve blocks we have finalised.
                                //
                                //  2. stale-initial — the peer's INITIAL
                                //     intersection picked a point that was our tip
                                //     when it connected, but the background
                                //     copy-to-immutable maintenance advanced our
                                //     immutable tip past that point before we
                                //     processed the reply.  The peer is on the
                                //     canonical chain (its tip is the network tip);
                                //     it just needs to re-intersect, which it does
                                //     on the next connection cycle against our
                                //     current (advanced) known-points.
                                //
                                // Before catch-up advanced the immutable tip, the
                                // tip stayed at genesis during sync so neither case
                                // arose; counting them caused spurious #699
                                // shutdowns on from-genesis sync.  See the "stale
                                // chainsync intersection when peer behind" note.
                                let peer_behind = tip_slot < immutable_slot;
                                let genuine_divergence = !is_initial && !peer_behind;
                                warn!(
                                    %peer_addr,
                                    rollback_slot,
                                    immutable_slot,
                                    peer_tip_slot = tip_slot,
                                    is_initial,
                                    peer_behind,
                                    genuine_divergence,
                                    "MsgRollBackward to point older than our \
                                     ImmutableDB tip — disconnecting peer. \
                                     Counted as a divergence witness only for a \
                                     mid-stream rollback from an up-to-date peer \
                                     (#699)."
                                );
                                // Record divergence-witness in the peer manager
                                // so the run-loop can detect a multi-peer
                                // divergence consensus and surface a clear
                                // operator error — but ONLY for genuine
                                // divergences, so behind peers and stale initial
                                // intersections cannot trigger a false shutdown.
                                if genuine_divergence {
                                    let mut pm = peer_manager.write().await;
                                    pm.record_rollback_below_immutable(
                                        peer_addr,
                                        rollback_slot,
                                        immutable_slot,
                                    );
                                }
                                return Err(anyhow::anyhow!(
                                    "Peer {peer_addr} requested rollback to slot \
                                     {rollback_slot} (older than our ImmutableDB \
                                     tip at slot {immutable_slot}; peer tip \
                                     {tip_slot}, is_initial={is_initial} — {})",
                                    if genuine_divergence {
                                        "divergent chain"
                                    } else if peer_behind {
                                        "peer is behind us"
                                    } else {
                                        "stale initial intersection (we advanced)"
                                    }
                                ));
                            }

                            if initial_rollback {
                                initial_rollback = false;
                                debug!(
                                    %peer_addr,
                                    rollback_slot,
                                    immutable_slot,
                                    "Skipping rollback depth check for initial \
                                     post-intersection rollback (in-volatile-window)",
                                );
                            } else {
                                let chain_tip_slot = chain_db
                                    .read()
                                    .await
                                    .get_tip()
                                    .point
                                    .slot()
                                    .map(|s| s.0)
                                    .unwrap_or(0);

                                if chain_tip_slot > rollback_slot {
                                    let depth_slots = chain_tip_slot - rollback_slot;
                                    // Scale by active_slots_coeff: e.g. 0.05 → 20 slots/block.
                                    // Use 2x safety margin → k * (1/coeff) * 2.
                                    let slots_per_block =
                                        (1.0 / active_slots_coeff).ceil() as u64;
                                    let threshold_slots = security_param
                                        .saturating_mul(slots_per_block)
                                        .saturating_mul(2);
                                    if depth_slots > threshold_slots {
                                        warn!(
                                            %peer_addr,
                                            depth_slots,
                                            threshold_slots,
                                            security_param,
                                            chain_tip_slot,
                                            rollback_slot,
                                            "MsgRollBackward exceeds k-block limit — \
                                             disconnecting peer (matches Haskell \
                                             terminateAfterDrain RolledBackPastIntersection)"
                                        );
                                        // Return an error to drop this connection.
                                        // The PeerManager will record the failure and
                                        // apply a reputation penalty.
                                        return Err(anyhow::anyhow!(
                                            "Peer {peer_addr} requested rollback of \
                                             {depth_slots} slots (> {threshold_slots} \
                                             threshold, k={security_param})"
                                        ));
                                    }
                                }
                            }
                        }

                        info!(
                            %peer_addr,
                            rollback_point = %prim_point,
                            tip_slot,
                            tip_block_number,
                            "ChainSync rollback",
                        );

                        // Count non-initial rollbacks for observability.
                        // The first MsgRollBackward after intersection is
                        // expected protocol behavior — not a real fork.
                        if !is_initial {
                            metrics
                                .rollback_count
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }

                        // Remove headers after the rollback point and read the
                        // post-trim length so refill can be hysteresis-gated.
                        metrics.update_peer_tip(tip_slot);
                        let pending_count = {
                            let mut chains = candidate_chains.write().await;
                            if let Some(entry) = chains.get_mut(&peer_addr) {
                                entry.pending_headers.retain(|h| h.slot <= rollback_slot);
                                entry.tip_slot = tip_slot;
                                entry.tip_hash = tip_hash;
                                entry.tip_block_number = tip_block_number;
                                // Issue #654 P1.b — reset per-peer eager
                                // op-cert counters on rollback. Phase 1
                                // simplification of #652 C5: rather than
                                // maintain a rewindable per-peer
                                // candidate-state history, we drop the
                                // counters entirely. Worst case: the
                                // next batch of headers on the peer's
                                // post-rollback chain re-establishes
                                // counters from the seed map (empty),
                                // which is what the global apply-time
                                // validator also does for unseen pools.
                                // Body apply remains the source of truth.
                                entry.eager_opcert_counters.clear();
                                entry.pending_headers.len()
                            } else {
                                0
                            }
                        };

                        // MsgRollBackward is deliberately NOT forwarded to a
                        // global rollback handler.  A single peer's rollback
                        // only means that peer trimmed its candidate fragment;
                        // it does not imply our preferred chain has changed.
                        // Matches Haskell `ChainSync.Client::rollBackward`:
                        // only `theirFrag` (per-peer candidate) is trimmed;
                        // chain selection decides whether to actually switch.
                        //
                        // Our ledger is rolled back only via the TriggeredFork
                        // verdict from ChainSelQueue (see apply_fetched_block
                        // in node/mod.rs), which fires when a competing fork
                        // fetched from BlockFetch is strictly preferred by
                        // chain density.  A blanket rollback here caused
                        // unwarranted ~1500-block ledger regressions when any
                        // single peer fell behind and reconnected offering
                        // an old intersection point (see 2026-04-21 fix).
                        let _ = &prim_point;

                        // Refill pipeline after rollback (hysteresis-gated).
                        if should_refill_pipeline(
                            at_tip,
                            outstanding,
                            low_mark,
                            pending_count,
                            &mut throttled,
                        ) {
                            let to_send = pipeline_target_depth(
                                known_gap(client_block_no, peer_tip_block_no),
                                high_mark,
                            )
                            .saturating_sub(outstanding);
                            debug!(
                                %peer_addr,
                                outstanding,
                                low_mark,
                                to_send,
                                "ChainSync refilling pipeline after rollback",
                            );
                            for _ in 0..to_send {
                                let req = cs_encode(&ChainSyncMessage::MsgRequestNext);
                                channel.send(req).await.map_err(|e| {
                                    anyhow::anyhow!("ChainSync pipeline refill failed: {e}")
                                })?;
                                outstanding += 1;
                            }
                            debug!(%peer_addr, outstanding, "ChainSync pipeline refilled");
                        } else {
                            debug!(
                                %peer_addr,
                                outstanding,
                                low_mark,
                                pending_count,
                                throttled,
                                "ChainSync post-rollback: not refilling \
                                 (pipeline full or candidate throttled)",
                            );
                        }
                    }

                    ChainSyncMessage::MsgAwaitReply => {
                        // At tip: the server has no new blocks right now.
                        // Do NOT decrement outstanding — MsgAwaitReply doesn't
                        // consume a request. The server will eventually respond
                        // with MsgRollForward or MsgRollBackward.
                        //
                        // Historicity: a peer claiming OUR candidate tip is
                        // its chain tip (MsgAwaitReply) while that tip is
                        // older than the cutoff is stalling us on a stale
                        // chain (Haskell judges the candidate tip's
                        // HeaderStateWithTime on HistoricalMsgAwaitReply).
                        if historicity_cutoff_secs.is_some()
                            && gsm_snapshot_rx.borrow().state
                                != crate::gsm::GenesisSyncState::CaughtUp
                        {
                            let frag = peer_state.fragment_snapshot();
                            let judged = match frag.head() {
                                crate::genesis_peer_state::FragAnchor::Point(slot, _) => {
                                    Some(slot)
                                }
                                crate::genesis_peer_state::FragAnchor::Origin => None,
                            };
                            if let Some(judged) = judged {
                                let view = ledger_view.load();
                                judge_historicity(judged, &view, "MsgAwaitReply")?;
                            }
                        }

                        // Genesis state: idlingStart (lossless; the GSM event
                        // below is a wakeup hint only). The LoP bucket is
                        // PAUSED while awaiting — an at-tip peer consumes no
                        // patience (Haskell onMsgAwaitReply: idlingStart >>
                        // lbPause).
                        peer_state.on_await_reply();
                        lop_bucket.pause(std::time::Instant::now());

                        // CSJ: any peer claiming it has no more headers leaves
                        // CSJ (Haskell onAwaitReply: disengage + backfill /
                        // elect successor).
                        if let Some(ref csj) = csj {
                            csj.on_await_reply(&peer_addr);
                        }

                        // Emit PeerIdling to the GSM actor so the GDD knows
                        // this peer has stopped sending blocks.
                        if let Err(e) = gsm_event_tx.try_send(crate::gsm::GsmEvent::PeerIdling {
                            addr: peer_addr,
                        }) {
                            debug!(%peer_addr, "GSM PeerIdling event dropped: {e}");
                        }
                        if !at_tip {
                            at_tip = true;
                            // Rate-limit "at tip" logging to at most once per
                            // 60 seconds globally across all peers.
                            //
                            // Rationale: when an inbound peer (e.g. a Haskell
                            // node syncing the full chain from Dugite) sends
                            // rapid MsgRollForward+MsgAwaitReply pairs, the
                            // at_tip flag toggles false→true on every single
                            // block — up to 1.2 million times in 10 minutes.
                            // Each log event at INFO floods the log file,
                            // filling 120MB in under 10 minutes and causing
                            // measurable I/O contention that stalls the main
                            // sync loop.
                            //
                            // Use compare_exchange (not load+store) so that
                            // concurrent tasks racing on the same 60-second
                            // window don't all win and each log once. Only the
                            // task that successfully stores wins.
                            static LAST_LOG: std::sync::atomic::AtomicU64 =
                                std::sync::atomic::AtomicU64::new(0);
                            let now_secs = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let prev = LAST_LOG.load(std::sync::atomic::Ordering::Relaxed);
                            if now_secs.saturating_sub(prev) >= 60
                                && LAST_LOG
                                    .compare_exchange(
                                        prev,
                                        now_secs,
                                        std::sync::atomic::Ordering::Relaxed,
                                        std::sync::atomic::Ordering::Relaxed,
                                    )
                                    .is_ok()
                            {
                                // Emit at DEBUG — "at tip waiting for new block"
                                // is normal steady-state and does not warrant INFO.
                                debug!(
                                    %peer_addr,
                                    headers_received,
                                    "ChainSync at tip — awaiting new blocks",
                                );
                            }
                        }
                    }

                    ChainSyncMessage::MsgDone => {
                        info!(%peer_addr, "ChainSync server sent MsgDone");
                        break;
                    }

                    other => {
                        // B1: State machine violation — a pipelined ChainSync client
                        // MUST NOT silently skip unexpected messages.  Doing so
                        // desynchronises the `outstanding` counter from the peer's
                        // actual state: either the pipeline drains to zero (sync
                        // stalls) or we keep sending MsgRequestNext when the peer
                        // no longer expects them (peer disconnects with AgencyViolation).
                        //
                        // The Haskell typed-protocol framework makes this impossible at
                        // compile time; we enforce the same invariant at runtime here by
                        // returning an error that triggers peer disconnect and reconnection.
                        return Err(anyhow::anyhow!(
                            "ChainSync state machine violation from {peer_addr}: \
                             unexpected message {other:?} (outstanding={outstanding}); \
                             disconnecting to trigger reconnection"
                        ));
                    }
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 4: Cleanup — remove this peer's candidate chain on exit
    // ═══════════════════════════════════════════════════════════════════════

    {
        let mut chains = candidate_chains.write().await;
        chains.remove(&peer_addr);
    }
    // CSJ unregister (→ backfill dynamo / re-elect objector) is handled by
    // `_csj_guard` on drop, covering this happy return AND every `?` error.

    info!(
        %peer_addr,
        headers_received,
        "ChainSync task exiting",
    );

    Ok(())
}

#[cfg(test)]
mod seed_peer_counter_tests {
    use super::*;
    use dugite_primitives::hash::Hash28;

    fn pool(b: u8) -> Hash28 {
        Hash28::from_bytes([b; 28])
    }

    /// The Mithril genesis-bootstrap fix: a per-peer counter map that does
    /// NOT yet track a pool is seeded from the global (snapshot-derived)
    /// counter, so the OCERT check sees the real baseline (e.g. 5) instead of
    /// falling back to 0 and falsely rejecting the pool's first eager header.
    #[test]
    fn seeds_absent_pool_from_global() {
        let mut peer: HashMap<Hash28, u64> = HashMap::new();
        let mut global: HashMap<Hash28, u64> = HashMap::new();
        global.insert(pool(0x85), 5); // TPREP-style snapshot counter
        global.insert(pool(0x99), 463); // preprod max

        seed_peer_counter_from_global(&mut peer, &global, pool(0x85));
        assert_eq!(
            peer.get(&pool(0x85)),
            Some(&5),
            "absent pool must seed from global"
        );
        // Only the requested pool is seeded (lazy, O(1) per first-seen pool).
        assert_eq!(
            peer.get(&pool(0x99)),
            None,
            "unrelated pool must not be seeded"
        );
    }

    /// A per-peer entry that already exists (the peer's own fork advanced the
    /// counter past the snapshot value) must NOT be overwritten by the global
    /// seed — the diverged per-peer view wins.
    #[test]
    fn preserves_existing_peer_entry() {
        let mut peer: HashMap<Hash28, u64> = HashMap::new();
        peer.insert(pool(0x85), 7); // peer advanced past snapshot's 5
        let mut global: HashMap<Hash28, u64> = HashMap::new();
        global.insert(pool(0x85), 5);

        seed_peer_counter_from_global(&mut peer, &global, pool(0x85));
        assert_eq!(
            peer.get(&pool(0x85)),
            Some(&7),
            "existing peer entry must be preserved"
        );
    }

    /// A pool absent from BOTH maps stays absent — the validator's
    /// known-issuer fallback (counter 0) then applies, which is correct for a
    /// genuinely counter-0 pool on a from-genesis node.
    #[test]
    fn pool_absent_from_global_stays_absent() {
        let mut peer: HashMap<Hash28, u64> = HashMap::new();
        let global: HashMap<Hash28, u64> = HashMap::new();
        seed_peer_counter_from_global(&mut peer, &global, pool(0x85));
        assert!(peer.is_empty(), "no entry when global lacks the pool");
    }

    /// Eager-validation outcome mapping (review of #756): an opcert
    /// over-increment is DEFERRED (Ok(false)/skip) because the per-peer
    /// counter is not authoritative for the upper bound — the body-apply
    /// path re-checks it. Every other error stays fatal; success → Ok(true).
    #[test]
    fn eager_defers_over_increment_keeps_other_faults_fatal() {
        use dugite_consensus::ConsensusError;

        // Success → validated.
        assert!(classify_eager_validation_result(Ok(())).unwrap());

        // Over-increment → deliberate skip, NOT a peer fault.
        let over = Err(ConsensusError::OpcertCounterOverIncremented {
            got: 5,
            last_seen: 0,
        });
        assert!(
            !classify_eager_validation_result(over).unwrap(),
            "over-increment must be deferred to body apply, never penalise the peer here"
        );

        // The opcert LOWER bound (replay-regression guard) stays FATAL — a
        // stale baseline can only make it more conservative, never permissive,
        // so eager enforcement is safe.
        let regress = Err(ConsensusError::OpcertSequenceRegression {
            got: 2,
            expected: 5,
        });
        assert!(
            classify_eager_validation_result(regress).is_err(),
            "CounterTooSmall/regression must remain a fatal eager fault"
        );

        // An unrelated crypto fault stays fatal.
        let bad = Err(ConsensusError::InvalidBlock("kes".into()));
        assert!(classify_eager_validation_result(bad).is_err());

        // OutsideForecast → deliberate skip, NOT a peer fault. The eager forecast
        // uses the throttled lock-free view (which can freeze far behind the applied
        // tip); the FRESH tip watch already gated the header in forecast_park, and
        // body apply re-forecasts authoritatively. Treating it as fatal here churned
        // all peers at the mainnet Babbage→Conway boundary (2026-06-13).
        let stale = Err(ConsensusError::OutsideForecast(
            dugite_consensus::OutsideForecastRange {
                at: Some(dugite_primitives::time::SlotNo(133_109_521)),
                max_for: dugite_primitives::time::SlotNo(133_239_122),
                requested: dugite_primitives::time::SlotNo(133_660_855),
            },
        ));
        assert!(
            !classify_eager_validation_result(stale).unwrap(),
            "stale-view OutsideForecast must be deferred to body apply, never disconnect"
        );
    }

    /// End-to-end of the regression: a peer map seeded from a snapshot global
    /// then validated against the praos OCERT predicate accepts the pool's
    /// snapshot counter, where the un-seeded (empty) map would reject it.
    #[test]
    fn seeded_counter_accepts_snapshot_value_unseeded_rejects() {
        // Model the OCERT predicate the way validate_header checks it:
        // m = counter-for-pool (or 0 if known issuer but absent); reject when
        // n > m + 1 (CounterOverIncrementedOCERT).
        let n = 5u64; // header's opcert counter (TPREP at snapshot)
        let mut global: HashMap<Hash28, u64> = HashMap::new();
        global.insert(pool(0x85), 5);

        // Un-seeded (the bug): empty peer map → m falls back to 0 → 5 > 1 → reject.
        let unseeded: HashMap<Hash28, u64> = HashMap::new();
        let m_unseeded = unseeded.get(&pool(0x85)).copied().unwrap_or(0);
        assert!(
            n > m_unseeded + 1,
            "un-seeded map reproduces the false rejection"
        );

        // Seeded (the fix): m = 5 → 5 > 6 is false → accept.
        let mut seeded: HashMap<Hash28, u64> = HashMap::new();
        seed_peer_counter_from_global(&mut seeded, &global, pool(0x85));
        let m_seeded = seeded.get(&pool(0x85)).copied().unwrap_or(0);
        assert!(
            n <= m_seeded + 1,
            "seeded map accepts the pool's snapshot counter"
        );
    }
}

#[cfg(test)]
mod forecast_park_tests {
    use super::*;
    use crate::node::ledger_view::LedgerView;
    use dugite_ledger::LedgerState;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::SlotNo;
    use std::time::Duration;

    /// Build a fresh `LedgerView` published into an `ArcSwap`, with the
    /// supplied tip slot + stability window. Used by the forecast tests to
    /// drive the helper without needing a full `Node`.
    fn make_view_swap(tip_slot: u64, stability_window: u64) -> Arc<arc_swap::ArcSwap<LedgerView>> {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.tip.point = if tip_slot == 0 {
            dugite_primitives::block::Point::Origin
        } else {
            dugite_primitives::block::Point::Specific(
                SlotNo(tip_slot),
                dugite_primitives::hash::Hash32::ZERO,
            )
        };
        state.randomness_stabilisation_window = stability_window;
        let view = LedgerView::from_state(&state);
        Arc::new(arc_swap::ArcSwap::from_pointee(view))
    }

    /// Issue #654: a header well within the forecast horizon returns Ok
    /// without parking.
    #[tokio::test]
    async fn forecast_within_horizon_returns_ok_immediately() {
        let view = make_view_swap(1_000, 100); // max_for = 1001 + 100 = 1101
        let (_tx, mut rx) = watch::channel(1_000u64);
        let cancel = CancellationToken::new();
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        let start = std::time::Instant::now();
        forecast_park_or_disconnect(&peer, 1_050, &view, &mut rx, &cancel, None, 0)
            .await
            .expect("slot 1050 is within [1000, 1101)");
        // Should be near-instant, certainly <500ms.
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    /// Issue #654: a header past the horizon parks until the tip advances
    /// enough to bring it into range, then returns Ok.
    #[tokio::test]
    async fn forecast_parks_then_wakes_on_tip_advance() {
        // tip=1000, sw=100 → max_for=1101 → slot 1500 is outside.
        let view = make_view_swap(1_000, 100);
        let (tx, mut rx) = watch::channel(1_000u64);
        let cancel = CancellationToken::new();
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let view_for_writer = Arc::clone(&view);

        // After 100ms: advance the tip to slot 1500 so max_for = 1601 and
        // slot 1500 becomes valid.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
            state.tip.point = dugite_primitives::block::Point::Specific(
                SlotNo(1_500),
                dugite_primitives::hash::Hash32::ZERO,
            );
            state.randomness_stabilisation_window = 100;
            view_for_writer.store(Arc::new(LedgerView::from_state(&state)));
            let _ = tx.send(1_500);
        });

        let start = std::time::Instant::now();
        forecast_park_or_disconnect(&peer, 1_500, &view, &mut rx, &cancel, None, 0)
            .await
            .expect("park should wake and succeed after tip advance");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80) && elapsed < Duration::from_secs(2),
            "expected ~100ms park, got {elapsed:?}"
        );
    }

    /// Issue #654: cancellation while parked returns `Ok(())` so the outer
    /// cancel handling can propagate.
    #[tokio::test]
    async fn forecast_park_returns_ok_on_cancel() {
        let view = make_view_swap(1_000, 100);
        let (_tx, mut rx) = watch::channel(1_000u64);
        let cancel = CancellationToken::new();
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        // Header slot 2000 is way outside; only cancel can break us out
        // (other than the 60s timeout we don't want to wait for).
        forecast_park_or_disconnect(&peer, 2_000, &view, &mut rx, &cancel, None, 0)
            .await
            .expect("cancel must short-circuit to Ok");
    }

    /// #sync-eval Fix 1: the pure timeout-selection gate. Behind the network
    /// tip by > a stability window ⇒ patient bulk bound; otherwise the tight
    /// at-tip bound. The comparison is strict (`>`), and both bounds are finite.
    #[test]
    fn forecast_park_timeout_selects_bound_by_tip_gap() {
        let sw = 100u64;
        // At/near tip: gap 0, gap < sw, gap == sw (strict >) → tight bound.
        assert_eq!(
            forecast_park_timeout(1_000, 1_000, sw),
            FORECAST_PARK_TIMEOUT
        );
        assert_eq!(
            forecast_park_timeout(1_050, 1_000, sw),
            FORECAST_PARK_TIMEOUT
        );
        assert_eq!(
            forecast_park_timeout(1_100, 1_000, sw),
            FORECAST_PARK_TIMEOUT,
            "gap == stability_window is NOT behind (strict >)"
        );
        // Behind: gap sw+1 and far behind → patient bulk bound.
        assert_eq!(
            forecast_park_timeout(1_101, 1_000, sw),
            FORECAST_PARK_TIMEOUT_BULK,
            "gap == stability_window + 1 ⇒ behind"
        );
        assert_eq!(
            forecast_park_timeout(50_000_000, 1_000, sw),
            FORECAST_PARK_TIMEOUT_BULK
        );
        // Unknown / stale network tip (0) or local ahead → tight bound, never panics.
        assert_eq!(forecast_park_timeout(0, 1_000, sw), FORECAST_PARK_TIMEOUT);
        // Both bounds finite (no infinite park is representable).
        assert!(
            FORECAST_PARK_TIMEOUT_BULK.as_secs() > 0 && FORECAST_PARK_TIMEOUT_BULK.as_secs() < 3600
        );
    }

    /// #sync-eval Fix 1: when we are far BEHIND the network tip (Praos
    /// from-genesis bulk sync), a header beyond the forecast horizon must NOT
    /// disconnect the peer at the tight 60s bound — that transient-stall churn
    /// collapsed the whole peer set in the live preprod repro. It must stay
    /// parked through `FORECAST_PARK_TIMEOUT` and only disconnect at the patient
    /// `FORECAST_PARK_TIMEOUT_BULK` (so the watchdog is still BOUNDED — a real
    /// wedge is surfaced, never an infinite silent park).
    ///
    /// On the pre-fix code this test FAILS: behind-tip used the same 60s bound,
    /// so the task returns `Err` shortly after 60s and the "still parked at 61s"
    /// assertion trips.
    #[tokio::test(start_paused = true)]
    async fn forecast_park_behind_network_tip_is_patient_then_bounded() {
        // tip=1000, sw=100 → max_for=1101 → header 5000 is beyond the horizon.
        let view = make_view_swap(1_000, 100);
        // network tip leads our local tip (1000) by far more than sw=100 ⇒
        // behind_network_tip = true ⇒ patient FORECAST_PARK_TIMEOUT_BULK.
        let network_tip = 1_000 + 100 + 50_000;
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        // tip_rx never advances (apply wedged): only the timeout can end the park.
        let (_tx, rx) = watch::channel(1_000u64);
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(async move {
            let mut rx = rx;
            forecast_park_or_disconnect(&peer, 5_000, &view, &mut rx, &cancel, None, network_tip)
                .await
        });
        // Let the spawned task reach its internal sleep before advancing time.
        tokio::task::yield_now().await;

        // Past the at-tip 60s bound but well under the bulk 300s bound:
        // must still be parked (the fix — old code would have disconnected).
        tokio::time::advance(FORECAST_PARK_TIMEOUT + Duration::from_secs(5)).await;
        assert!(
            !handle.is_finished(),
            "behind network tip: must stay parked past the 60s at-tip bound, not disconnect"
        );

        // Past the bulk bound: the watchdog MUST fire (bounded — no infinite
        // silent park even though network_tip stayed far ahead the whole time).
        tokio::time::advance(FORECAST_PARK_TIMEOUT_BULK).await;
        let res = handle.await.expect("task joins");
        assert!(
            res.is_err(),
            "behind network tip: a genuinely wedged ledger must still disconnect at the bulk bound"
        );
    }

    /// #sync-eval Fix 1: when we are AT/near the network tip, a header beyond
    /// the horizon with no local progress is genuinely suspicious and must
    /// still disconnect at the tight 60s bound (behaviour preserved).
    #[tokio::test(start_paused = true)]
    async fn forecast_park_at_tip_disconnects_at_60s() {
        let view = make_view_swap(1_000, 100);
        // network tip within one stability window of local ⇒ behind=false ⇒ 60s.
        let network_tip = 1_000 + 50;
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let (_tx, rx) = watch::channel(1_000u64);
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(async move {
            let mut rx = rx;
            forecast_park_or_disconnect(&peer, 5_000, &view, &mut rx, &cancel, None, network_tip)
                .await
        });
        tokio::task::yield_now().await;

        // Under 60s: still parked.
        tokio::time::advance(FORECAST_PARK_TIMEOUT - Duration::from_secs(5)).await;
        assert!(!handle.is_finished(), "at tip: parked until the 60s bound");
        // Past 60s: disconnects (and crucially does NOT wait the full 300s).
        tokio::time::advance(Duration::from_secs(10)).await;
        let res = handle.await.expect("task joins");
        assert!(res.is_err(), "at tip + silent: disconnect at the 60s bound");
    }

    /// Issue #654 P1.b: `extract_era_tag_from_wrapped_header` reads the
    /// HFC era tag from a valid Shelley+ wrap.
    #[test]
    fn extract_era_tag_reads_shelley_plus_wrap() {
        use minicbor::Encoder;
        for tag in [2u64, 3, 4, 5, 6, 7, 8] {
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(tag).unwrap();
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(b"inner").unwrap();
            assert_eq!(
                extract_era_tag_from_wrapped_header(&buf),
                Some(tag),
                "era_tag {tag} should round-trip"
            );
        }
    }

    /// Byron N2N wrap returns None — its first element is a nested array,
    /// not a uint.
    #[test]
    fn extract_era_tag_rejects_byron_wrap() {
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.array(2).unwrap(); // nested array (byron payload shape) — NOT a uint
        enc.u64(0).unwrap();
        enc.u64(100).unwrap();
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(b"byron-inner").unwrap();
        assert_eq!(extract_era_tag_from_wrapped_header(&buf), None);
    }

    /// Issue #654 P1.b: `eager_validate_header` returns `Ok(false)`
    /// (skip) for pre-Conway era tags so the existing path takes over.
    #[test]
    fn eager_validate_header_skips_pre_conway_eras() {
        use minicbor::Encoder;
        use std::collections::HashMap;

        let consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        let state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        // WIRE HFC indices (Byron combined at 0): 1=Shelley … 5=Babbage.
        // 6 = Conway on the wire and must NOT skip (covered by the
        // undecodable-header test below).
        for tag in [1u64, 2, 3, 4, 5] {
            // Use a header CBOR that would NOT validate (empty inner) — we
            // assert the function returns Ok(false) BEFORE attempting any
            // decode, proving the era gate fires first.
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(tag).unwrap();
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(b"x").unwrap();

            let mut counters = HashMap::new();
            let result = eager_validate_header(&peer, &buf, 0, &consensus, &view, &mut counters);
            assert!(
                matches!(result, Ok(false)),
                "era_tag {tag} should skip eager validation (got {result:?})"
            );
            assert!(counters.is_empty(), "skip must not mutate peer counters");
        }
    }

    /// Issue #654 P1.b: malformed envelope (non-uint where era tag goes)
    /// returns `Ok(false)` — body apply will catch it as a decode failure.
    #[test]
    fn eager_validate_header_skips_malformed_envelope() {
        use std::collections::HashMap;
        let consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        let state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut counters = HashMap::new();

        let result =
            eager_validate_header(&peer, &[0xff, 0x00], 0, &consensus, &view, &mut counters);
        assert!(matches!(result, Ok(false)));
    }

    /// Issue #654 P1.b: Conway header CBOR that fails to decode returns
    /// `Err`, so the caller disconnects the peer with a labelled reason.
    /// (The valid-header round-trip is exercised via the integration
    /// path; here we just confirm the Err path fires.)
    #[test]
    fn eager_validate_header_errors_on_undecodable_conway_header() {
        use minicbor::Encoder;
        use std::collections::HashMap;

        let consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        let state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        // Build wraps for the WIRE Conway (6) and Dijkstra (7) indices whose
        // inner bytes are not a valid header — the decoder must return Err
        // and eager_validate_header must propagate (i.e. the era gate lets
        // both through to the decode).
        for tag in [6u64, 7] {
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(tag).unwrap();
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(b"garbage").unwrap();

            let mut counters = HashMap::new();
            let result = eager_validate_header(&peer, &buf, 0, &consensus, &view, &mut counters);
            assert!(
                result.is_err(),
                "undecodable wire-tag-{tag} header must Err (got {result:?})"
            );
        }
    }

    #[test]
    fn skip_decision_flag_off_never_skips_never_removes() {
        assert_eq!(
            decide_skip_apply_header_crypto(false, 10, Some(10)),
            (false, false)
        );
        assert_eq!(
            decide_skip_apply_header_crypto(false, 10, Some(9)),
            (false, false)
        );
        assert_eq!(
            decide_skip_apply_header_crypto(false, 10, None),
            (false, false)
        );
    }

    #[test]
    fn skip_decision_flag_on_same_epoch_skips_and_removes() {
        assert_eq!(
            decide_skip_apply_header_crypto(true, 10, Some(10)),
            (true, true)
        );
        assert_eq!(
            decide_skip_apply_header_crypto(true, 0, Some(0)),
            (true, true)
        );
    }

    #[test]
    fn skip_decision_flag_on_stale_epoch_revalidates_but_cleans_up() {
        // Header was eagerly validated at epoch 9 but ledger has since
        // transitioned to epoch 10 — snapshot pointer may differ, must
        // re-validate. Still cull the stale map entry to bound memory.
        assert_eq!(
            decide_skip_apply_header_crypto(true, 10, Some(9)),
            (false, true)
        );
    }

    #[test]
    fn skip_decision_flag_on_no_entry_revalidates() {
        // Header never went through eager validation (Byron, pre-Conway,
        // or eager validation returned Ok(false) for any other reason).
        assert_eq!(
            decide_skip_apply_header_crypto(true, 10, None),
            (false, false)
        );
    }

    /// Issue #654: Byron headers bypass the forecast check entirely
    /// (PBFT doesn't use Praos forecast semantics).
    #[test]
    fn is_byron_wrapped_header_distinguishes_byron_from_shelley() {
        use minicbor::Encoder;
        // Shelley+ HFC wrap: [era_tag(uint), tag24(bytes)]
        let mut shelley = Vec::new();
        let mut enc = Encoder::new(&mut shelley);
        enc.array(2).unwrap();
        enc.u64(6).unwrap(); // Babbage era
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(b"inner").unwrap();
        assert!(
            !is_byron_wrapped_header(&shelley),
            "Shelley+ wrap must NOT be classified as Byron"
        );

        // Byron N2N wrap is array(2) with era_id=0 + complex payload.
        // We don't need to construct a fully-valid one for this predicate
        // test — just verify the negative path; `unwrap_byron_n2n_header`
        // has its own positive-path coverage elsewhere.
        let not_byron: &[u8] = &[];
        assert!(!is_byron_wrapped_header(not_byron));
    }
}

#[cfg(test)]
mod chainsync_task_tests {
    use super::*;

    /// `classify_intersect_response` must map every `ChainSyncMessage` variant
    /// correctly: only `MsgIntersectFound`/`NotFound` are legitimate replies in
    /// StIntersect; the three server next-phase responses are stale residue to
    /// discard; everything else is a protocol violation. (Drives the stale-
    /// RollForward-on-reused-mux fix.)
    #[test]
    fn classify_intersect_response_covers_all_variants() {
        let found = ChainSyncMessage::MsgIntersectFound {
            point: CodecPoint::Origin,
            tip_slot: 1,
            tip_hash: [0u8; 32],
            tip_block_number: 1,
        };
        let not_found = ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: 1,
            tip_hash: [0u8; 32],
            tip_block_number: 1,
        };
        let roll_fwd = ChainSyncMessage::MsgRollForward {
            header: vec![],
            tip_slot: 1,
            tip_hash: [0u8; 32],
            tip_block_number: 1,
        };
        let roll_back = ChainSyncMessage::MsgRollBackward {
            point: CodecPoint::Origin,
            tip_slot: 1,
            tip_hash: [0u8; 32],
            tip_block_number: 1,
        };
        assert_eq!(classify_intersect_response(&found), IntersectOutcome::Found);
        assert_eq!(
            classify_intersect_response(&not_found),
            IntersectOutcome::NotFound
        );
        // The three stale next-phase responses (reused-mux residue):
        assert_eq!(
            classify_intersect_response(&roll_fwd),
            IntersectOutcome::StaleNextPhase
        );
        assert_eq!(
            classify_intersect_response(&roll_back),
            IntersectOutcome::StaleNextPhase
        );
        assert_eq!(
            classify_intersect_response(&ChainSyncMessage::MsgAwaitReply),
            IntersectOutcome::StaleNextPhase
        );
        // Client-agency / terminal messages are genuine violations here:
        assert_eq!(
            classify_intersect_response(&ChainSyncMessage::MsgRequestNext),
            IntersectOutcome::Invalid
        );
        assert_eq!(
            classify_intersect_response(&ChainSyncMessage::MsgFindIntersect(vec![])),
            IntersectOutcome::Invalid
        );
        assert_eq!(
            classify_intersect_response(&ChainSyncMessage::MsgDone),
            IntersectOutcome::Invalid
        );
    }

    /// Build a `MuxChannel` whose ingress is preloaded with the given complete
    /// CBOR frames (each a `cs_encode`d `ChainSyncMessage`), then closed. Lets
    /// `read_intersect_reply` be driven without a live mux/egress.
    fn preload_chainsync_channel(frames: Vec<Vec<u8>>) -> dugite_network::MuxChannel {
        use dugite_network::{Direction, MuxChannel};
        type Bytes = tokio_util::bytes::Bytes;
        let (egress_tx, _egress_rx) = tokio::sync::mpsc::channel::<(u16, Direction, Bytes)>(8);
        let (ingress_tx, ingress_rx) = tokio::sync::mpsc::channel::<Bytes>(64);
        for f in frames {
            ingress_tx
                .try_send(Bytes::from(f))
                .expect("preload ingress");
        }
        drop(ingress_tx); // close after preloading; recv yields buffered frames first
        MuxChannel::new(
            2,
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            65_536,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
    }

    fn test_addr() -> std::net::SocketAddr {
        "127.0.0.1:3001".parse().unwrap()
    }

    fn enc(msg: &ChainSyncMessage) -> Vec<u8> {
        cs_encode(msg)
    }

    fn stale_roll_forward() -> Vec<u8> {
        enc(&ChainSyncMessage::MsgRollForward {
            header: vec![0x80], // any valid CBOR value; content irrelevant (discarded)
            tip_slot: 9,
            tip_hash: [1u8; 32],
            tip_block_number: 9,
        })
    }

    // ─── #910 — pipeline depth is bounded by the known gap ─────────────────

    /// The pipeline may never run deeper than the peer's known block backlog.
    ///
    /// This is what makes drain-before-`MsgDone` terminate: at the tip
    /// (`gap == 0`) exactly one request is outstanding, so a demotion drains in
    /// one message instead of waiting for 200-300 blocks to be minted.
    #[test]
    fn pipeline_target_depth_is_gap_bounded_with_a_floor_of_one() {
        // At tip: Haskell's `Request` case — one non-pipelined request.
        assert_eq!(pipeline_target_depth(0, 300), 1);
        // Small backlog: pipeline exactly the backlog, not the high mark.
        assert_eq!(pipeline_target_depth(1, 300), 1);
        assert_eq!(pipeline_target_depth(7, 300), 7);
        assert_eq!(pipeline_target_depth(299, 300), 299);
        // Bulk sync: clamped at the high mark, so throughput is unchanged.
        assert_eq!(pipeline_target_depth(300, 300), 300);
        assert_eq!(pipeline_target_depth(5_000_000, 300), 300);
        // Operator-lowered depth is still honoured.
        assert_eq!(pipeline_target_depth(5_000_000, 32), 32);
    }

    // ─── #910 — drain to zero, then MsgDone ────────────────────────────────

    fn roll_forward_frame() -> Vec<u8> {
        stale_roll_forward()
    }

    /// Like `preload_chainsync_channel` but keeps the egress receiver alive so
    /// the test can assert what the drain actually put on the wire.
    #[allow(clippy::type_complexity)]
    fn preload_chainsync_channel_with_egress(
        frames: Vec<Vec<u8>>,
    ) -> (
        dugite_network::MuxChannel,
        tokio::sync::mpsc::Receiver<(u16, dugite_network::Direction, tokio_util::bytes::Bytes)>,
    ) {
        use dugite_network::{Direction, MuxChannel};
        type Bytes = tokio_util::bytes::Bytes;
        let (egress_tx, egress_rx) = tokio::sync::mpsc::channel::<(u16, Direction, Bytes)>(8);
        let (ingress_tx, ingress_rx) = tokio::sync::mpsc::channel::<Bytes>(64);
        for f in frames {
            ingress_tx
                .try_send(Bytes::from(f))
                .expect("preload ingress");
        }
        drop(ingress_tx);
        let ch = MuxChannel::new(
            2,
            Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            65_536,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (ch, egress_rx)
    }

    /// Assert that the only thing the drain wrote was a single `MsgDone`.
    fn assert_sent_only_msg_done(
        egress_rx: &mut tokio::sync::mpsc::Receiver<(
            u16,
            dugite_network::Direction,
            tokio_util::bytes::Bytes,
        )>,
    ) {
        let (_, _, frame) = egress_rx.try_recv().expect("drain must send MsgDone");
        assert!(
            matches!(cs_decode(&frame), Ok(ChainSyncMessage::MsgDone)),
            "drain must terminate the instance with MsgDone"
        );
        assert!(
            egress_rx.try_recv().is_err(),
            "the drain must not send anything else (no MsgRequestNext refills)"
        );
    }

    /// The drain consumes exactly `outstanding` depth-consuming responses and
    /// then sends `MsgDone` — Haskell's `drainThePipe` followed by the
    /// depth-`Z`-only `SendMsgDone`.
    #[tokio::test]
    async fn drain_consumes_outstanding_then_sends_done() {
        let (mut ch, mut egress) = preload_chainsync_channel_with_egress(vec![
            roll_forward_frame(),
            roll_forward_frame(),
            roll_forward_frame(),
        ]);
        let discarded =
            drain_pipeline_and_terminate(&mut ch, test_addr(), 3, CHAINSYNC_DRAIN_TIMEOUT)
                .await
                .expect("drain must succeed when every response is available");
        assert_eq!(discarded, 3);
        assert_sent_only_msg_done(&mut egress);
    }

    /// `MsgAwaitReply` does NOT consume a pipelined request — the server still
    /// owes the eventual roll — so the drain must keep reading past it. This is
    /// the accounting the main loop uses, and getting it wrong is what made the
    /// old residue bound (`DUGITE_PIPELINE_DEPTH + 16`) undersized: near the tip
    /// one request can yield two frames.
    #[tokio::test]
    async fn drain_does_not_count_await_reply_against_the_pipeline_depth() {
        let (mut ch, mut egress) = preload_chainsync_channel_with_egress(vec![
            enc(&ChainSyncMessage::MsgAwaitReply),
            roll_forward_frame(),
        ]);
        let discarded =
            drain_pipeline_and_terminate(&mut ch, test_addr(), 1, CHAINSYNC_DRAIN_TIMEOUT)
                .await
                .expect("AwaitReply then the roll must satisfy one outstanding request");
        assert_eq!(discarded, 2, "both frames are discarded");
        assert_sent_only_msg_done(&mut egress);
    }

    /// A server that terminates first leaves nothing to drain and nothing to
    /// send — the instance is already at `StDone`.
    #[tokio::test]
    async fn drain_stops_when_the_server_sends_done_first() {
        let mut ch = preload_chainsync_channel(vec![enc(&ChainSyncMessage::MsgDone)]);
        let discarded =
            drain_pipeline_and_terminate(&mut ch, test_addr(), 5, CHAINSYNC_DRAIN_TIMEOUT)
                .await
                .expect("server MsgDone ends the drain cleanly");
        assert_eq!(discarded, 1);
    }

    /// Nothing outstanding → straight to `MsgDone`, no reads at all. (The
    /// channel is empty and closed; a drain that tried to read would error.)
    #[tokio::test]
    async fn drain_with_empty_pipeline_sends_done_immediately() {
        let (mut ch, mut egress) = preload_chainsync_channel_with_egress(vec![]);
        let discarded =
            drain_pipeline_and_terminate(&mut ch, test_addr(), 0, CHAINSYNC_DRAIN_TIMEOUT)
                .await
                .expect("an already-empty pipeline drains trivially");
        assert_eq!(discarded, 0);
        assert_sent_only_msg_done(&mut egress);
    }

    /// At the tip the single outstanding request is parked in `StMustReply`
    /// until the peer mints its next block, so waiting the full budget would
    /// hold the peer-manager write lock for seconds on every at-tip demotion.
    /// The at-tip budget must be short enough not to stall the connection
    /// manager, and still non-zero.
    #[test]
    fn drain_budget_is_short_at_tip_and_generous_during_bulk_sync() {
        assert_eq!(chainsync_drain_budget(false), CHAINSYNC_DRAIN_TIMEOUT);
        assert_eq!(chainsync_drain_budget(true), CHAINSYNC_DRAIN_TIMEOUT_AT_TIP);
        assert!(CHAINSYNC_DRAIN_TIMEOUT_AT_TIP < CHAINSYNC_DRAIN_TIMEOUT);
        assert!(!CHAINSYNC_DRAIN_TIMEOUT_AT_TIP.is_zero());
        // Both must stay under PeerConnection::PROTOCOL_SHUTDOWN_TIMEOUT (5 s)
        // so the task always exits normally rather than being abort-killed.
        assert!(CHAINSYNC_DRAIN_TIMEOUT < std::time::Duration::from_secs(5));
    }

    /// A drain that cannot complete must FAIL rather than silently leaving
    /// residue behind — that failure is what makes `demote_to_warm` escalate to
    /// a TCP close instead of reusing the mux.
    #[tokio::test]
    async fn drain_fails_when_responses_are_missing() {
        // One response available, two outstanding: the channel then closes.
        let mut ch = preload_chainsync_channel(vec![roll_forward_frame()]);
        let err = drain_pipeline_and_terminate(&mut ch, test_addr(), 2, CHAINSYNC_DRAIN_TIMEOUT)
            .await
            .expect_err("an incomplete drain must not report success");
        let msg = err.to_string();
        assert!(
            msg.contains("drain"),
            "error must identify the drain, got: {msg}"
        );
    }

    /// A wire-state violation during the drain is still a violation.
    #[tokio::test]
    async fn drain_rejects_a_client_agency_message() {
        let mut ch = preload_chainsync_channel(vec![enc(&ChainSyncMessage::MsgRequestNext)]);
        let err = drain_pipeline_and_terminate(&mut ch, test_addr(), 1, CHAINSYNC_DRAIN_TIMEOUT)
            .await
            .expect_err("MsgRequestNext has client agency and is never a response");
        assert!(err.to_string().contains("unexpected message"));
    }

    // ─── #908(b) — NotFound is the same verdict as an Origin intersection ───

    /// `MsgIntersectNotFound` for every point (the deepest retry set always ends
    /// in `Origin`) means the peer can only serve us from genesis — exactly what
    /// an `Origin` intersection means. Pre-#908 the `None` case fell through to
    /// "syncing from Origin", so the ForkTooDeep verdict was only reached if the
    /// peer went on to deliver a genesis-region header; in the observed preprod
    /// flap it hung up first, no failure was ever classified, and the governor
    /// re-promoted it 2-6 s later, indefinitely.
    #[test]
    fn intersection_none_is_treated_like_origin() {
        assert!(
            intersection_is_genesis_only(None),
            "IntersectNotFound for every offered point == genesis-only"
        );
        assert!(intersection_is_genesis_only(Some(&CodecPoint::Origin)));
        assert!(
            !intersection_is_genesis_only(Some(&CodecPoint::Specific(42, [3u8; 32]))),
            "a real intersection is usable"
        );
    }

    /// THE BUG REPRO: a stale `MsgRollForward` (prior-session residue on a
    /// reused mux) is discarded, then the real `MsgIntersectFound` is returned.
    /// Pre-fix this hit the `unexpected response` error and tore the peer down.
    #[tokio::test]
    async fn read_intersect_reply_discards_one_stale_rollforward_then_found() {
        let found = enc(&ChainSyncMessage::MsgIntersectFound {
            point: CodecPoint::Origin,
            tip_slot: 10,
            tip_hash: [2u8; 32],
            tip_block_number: 10,
        });
        let mut ch = preload_chainsync_channel(vec![stale_roll_forward(), found]);
        let res = read_intersect_reply(
            &mut ch,
            test_addr(),
            16,
            &crate::metrics::NodeMetrics::new(),
        )
        .await;
        assert!(
            matches!(res, Ok(Some(_))),
            "stale RollForward must be discarded then IntersectFound returned, got {res:?}"
        );
    }

    /// Issue #904 — the intersection reply must seed `max_peer_tip_slot`.
    ///
    /// `should_skip_forge_for_catch_up` treats `peer_tip == 0` as "no peer has
    /// reported yet" and falls back to comparing our tip against the WALL
    /// CLOCK. That fallback is only sound before the first ChainSync round.
    ///
    /// A block producer restarted onto a stalled chain (devnet, or any network
    /// whose forgers are all down) reaches an intersection but then receives no
    /// RollForward — nothing is producing blocks, because this node is what
    /// would produce them. Pre-fix, `peer_tip` stayed 0, the gate compared
    /// wall-clock against a static tip, concluded it was hundreds of slots
    /// behind, and silently skipped every leadership check forever. The chain
    /// never recovered.
    ///
    /// `MsgIntersectFound` carries the peer's tip. Once we have it, it is
    /// strictly better information than the wall clock and must be recorded.
    #[tokio::test]
    async fn read_intersect_reply_seeds_peer_tip_from_intersection() {
        let metrics = crate::metrics::NodeMetrics::new();
        assert_eq!(metrics.get_peer_tip(), 0, "precondition: no peer tip yet");

        let found = enc(&ChainSyncMessage::MsgIntersectFound {
            point: CodecPoint::Origin,
            tip_slot: 65,
            tip_hash: [7u8; 32],
            tip_block_number: 28,
        });
        let mut ch = preload_chainsync_channel(vec![found]);
        let res = read_intersect_reply(&mut ch, test_addr(), 16, &metrics).await;
        assert!(
            matches!(res, Ok(Some(_))),
            "expected IntersectFound, got {res:?}"
        );
        assert_eq!(
            metrics.get_peer_tip(),
            65,
            "IntersectFound.tip_slot must seed max_peer_tip_slot (#904)"
        );

        // IntersectNotFound also carries the peer's tip — record it too, and
        // keep the monotonic max.
        let not_found = enc(&ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: 90,
            tip_hash: [8u8; 32],
            tip_block_number: 40,
        });
        let mut ch = preload_chainsync_channel(vec![not_found]);
        assert!(matches!(
            read_intersect_reply(&mut ch, test_addr(), 16, &metrics).await,
            Ok(None)
        ));
        assert_eq!(
            metrics.get_peer_tip(),
            90,
            "IntersectNotFound must seed it too"
        );

        // Monotonic: a lower tip must not regress the recorded max.
        let older = enc(&ChainSyncMessage::MsgIntersectFound {
            point: CodecPoint::Origin,
            tip_slot: 12,
            tip_hash: [9u8; 32],
            tip_block_number: 6,
        });
        let mut ch = preload_chainsync_channel(vec![older]);
        let _ = read_intersect_reply(&mut ch, test_addr(), 16, &metrics).await;
        assert_eq!(metrics.get_peer_tip(), 90, "peer tip must stay monotonic");
    }

    /// Issue #904 — the wedge itself, at the pure-gate level.
    ///
    /// Restarted BP: our tip is slot 65, the peer's tip is ALSO 65 (the chain
    /// is stalled and we are exactly at it), but the wall clock has run on to
    /// 817 because ~12 minutes of real time passed while we were down.
    #[test]
    fn forge_gate_does_not_wedge_when_at_a_stalled_chain_tip() {
        use crate::node::should_skip_forge_for_catch_up;
        let stability_window = 240; // ceil(3k/f) for devnet k=40 f=0.5
        let tip_slot = 65;
        let wall_clock = 817;

        // Pre-fix: peer_tip never populated -> wall-clock fallback -> wedge.
        assert!(
            should_skip_forge_for_catch_up(tip_slot, 0, wall_clock, stability_window),
            "documents the pre-fix behaviour: with no peer tip the gate skips"
        );

        // Post-fix: the intersection told us the peer is at 65, same as us.
        assert!(
            !should_skip_forge_for_catch_up(tip_slot, 65, wall_clock, stability_window),
            "with the peer tip known and equal to ours we are AT the tip and must forge (#904)"
        );
    }

    /// A clean connection (only the intersect reply) is byte-identical behavior.
    #[tokio::test]
    async fn read_intersect_reply_clean_found_and_not_found() {
        let found = enc(&ChainSyncMessage::MsgIntersectFound {
            point: CodecPoint::Origin,
            tip_slot: 5,
            tip_hash: [0u8; 32],
            tip_block_number: 5,
        });
        let mut ch = preload_chainsync_channel(vec![found]);
        assert!(matches!(
            read_intersect_reply(
                &mut ch,
                test_addr(),
                16,
                &crate::metrics::NodeMetrics::new()
            )
            .await,
            Ok(Some(_))
        ));

        let not_found = enc(&ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: 5,
            tip_hash: [0u8; 32],
            tip_block_number: 5,
        });
        let mut ch = preload_chainsync_channel(vec![not_found]);
        assert!(matches!(
            read_intersect_reply(
                &mut ch,
                test_addr(),
                16,
                &crate::metrics::NodeMetrics::new()
            )
            .await,
            Ok(None)
        ));
    }

    /// Multiple stale next-phase variants (RollBackward + AwaitReply) discarded
    /// before the real NotFound.
    #[tokio::test]
    async fn read_intersect_reply_discards_rollback_and_awaitreply_then_not_found() {
        let rb = enc(&ChainSyncMessage::MsgRollBackward {
            point: CodecPoint::Origin,
            tip_slot: 1,
            tip_hash: [0u8; 32],
            tip_block_number: 1,
        });
        let await_reply = enc(&ChainSyncMessage::MsgAwaitReply);
        let nf = enc(&ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: 2,
            tip_hash: [0u8; 32],
            tip_block_number: 2,
        });
        let mut ch = preload_chainsync_channel(vec![rb, await_reply, nf]);
        assert!(matches!(
            read_intersect_reply(
                &mut ch,
                test_addr(),
                16,
                &crate::metrics::NodeMetrics::new()
            )
            .await,
            Ok(None)
        ));
    }

    /// Beyond `max_stale_discard` (16 here) the reader fails fast (→ reconnect),
    /// not an infinite spin on a genuinely broken peer.
    #[tokio::test]
    async fn read_intersect_reply_exceeds_stale_bound_errors() {
        let frames: Vec<Vec<u8>> = (0..17).map(|_| stale_roll_forward()).collect();
        let mut ch = preload_chainsync_channel(frames);
        let res = read_intersect_reply(
            &mut ch,
            test_addr(),
            16,
            &crate::metrics::NodeMetrics::new(),
        )
        .await;
        let err = res.expect_err("17 stale frames must exceed the bound and error");
        assert!(
            format!("{err:?}").contains("giving up"),
            "expected a give-up error, got: {err:?}"
        );
    }

    /// A genuine protocol violation (client-agency/terminal message in
    /// StIntersect) errors immediately, NOT tolerated.
    #[tokio::test]
    async fn read_intersect_reply_genuine_violation_errors_immediately() {
        let mut ch = preload_chainsync_channel(vec![enc(&ChainSyncMessage::MsgDone)]);
        let res = read_intersect_reply(
            &mut ch,
            test_addr(),
            16,
            &crate::metrics::NodeMetrics::new(),
        )
        .await;
        let err = res.expect_err("MsgDone in StIntersect is a violation");
        assert!(
            format!("{err:?}").contains("unexpected response"),
            "expected the unexpected-response error, got: {err:?}"
        );
    }

    /// Verify extract_hash_from_header produces a 32-byte array.
    #[test]
    fn test_extract_hash_from_header() {
        let header = vec![0x82, 0x01, 0x02]; // arbitrary CBOR
        let hash = extract_hash_from_header(&header);
        // Should be a valid 32-byte blake2b-256 hash.
        assert_eq!(hash.len(), 32);
        // Same input should produce the same hash (deterministic).
        assert_eq!(hash, extract_hash_from_header(&header));
    }

    /// Verify extract_slot_from_wrapped_header returns None for invalid CBOR.
    #[test]
    fn test_extract_slot_invalid_cbor() {
        assert_eq!(extract_slot_from_wrapped_header(&[], 0), None);
        assert_eq!(extract_slot_from_wrapped_header(&[0x00], 0), None);
    }

    /// Verifies `should_refill_pipeline` enforces hysteresis-gated
    /// wire-level backpressure on the ChainSync pipeline.
    ///
    /// Regression test for the preprod 2026-05-10 stall: when the previous
    /// silent `drain(..)` cap on `pending_headers` removed unfetched headers
    /// to keep the buffer at 10_000, the dropped headers created permanent
    /// chain gaps that `chain_sel` could never bridge — every reconnecting
    /// peer's fragment was drained too, so no peer ever held the full
    /// sequence.  The fix replaces silent drops with refusal-to-refill at
    /// `PENDING_HEADERS_PAUSE`, with hysteresis at `PENDING_HEADERS_RESUME`
    /// preventing per-block thrashing once the throttle clears.
    #[test]
    fn test_should_refill_pipeline_hysteresis() {
        let high_mark = 300;
        let low_mark = high_mark * 2 / 3; // 200
        let mut throttled = false;

        // Fresh state, well under PAUSE, in-flight low → refill.
        assert!(should_refill_pipeline(
            false,
            100,
            low_mark,
            100,
            &mut throttled
        ));
        assert!(!throttled);

        // Pipeline still full (outstanding > low_mark) → don't refill.
        assert!(!should_refill_pipeline(
            false,
            250,
            low_mark,
            100,
            &mut throttled
        ));
        assert!(!throttled);

        // At tip → don't refill regardless of room.
        assert!(!should_refill_pipeline(
            true,
            0,
            low_mark,
            0,
            &mut throttled
        ));
        assert!(!throttled);

        // In hysteresis band, not yet throttled → still refill.
        assert!(should_refill_pipeline(
            false,
            100,
            low_mark,
            (PENDING_HEADERS_RESUME + PENDING_HEADERS_PAUSE) / 2,
            &mut throttled,
        ));
        assert!(!throttled);

        // Hits PAUSE → throttle on, no refill.
        assert!(!should_refill_pipeline(
            false,
            100,
            low_mark,
            PENDING_HEADERS_PAUSE,
            &mut throttled,
        ));
        assert!(throttled);

        // Drops back into hysteresis band → throttle stays on (sticky).
        assert!(!should_refill_pipeline(
            false,
            100,
            low_mark,
            (PENDING_HEADERS_RESUME + PENDING_HEADERS_PAUSE) / 2,
            &mut throttled,
        ));
        assert!(throttled);

        // Drops below RESUME → throttle clears, refill resumes.
        assert!(should_refill_pipeline(
            false,
            100,
            low_mark,
            PENDING_HEADERS_RESUME - 1,
            &mut throttled,
        ));
        assert!(!throttled);

        // PAUSE > RESUME (compile-time sanity — hysteresis only works
        // with strictly ordered thresholds).
        const _: () = assert!(PENDING_HEADERS_PAUSE > PENDING_HEADERS_RESUME);
    }

    /// Sanity: the helper never refills above `low_mark` even at zero
    /// pending — the pipeline gate is independent of the buffer gate.
    #[test]
    fn test_should_refill_pipeline_respects_low_mark() {
        let mut throttled = false;
        // outstanding == low_mark → refill.
        assert!(should_refill_pipeline(false, 200, 200, 0, &mut throttled));
        // outstanding > low_mark → no refill.
        assert!(!should_refill_pipeline(false, 201, 200, 0, &mut throttled));
    }

    /// Verify to_codec_point / from_codec_point round-trip.
    #[test]
    fn test_point_roundtrip() {
        let origin = Point::Origin;
        assert_eq!(from_codec_point(&to_codec_point(&origin)), origin);

        let specific = Point::Specific(
            dugite_primitives::time::SlotNo(42),
            dugite_primitives::hash::Hash32::from_bytes([0xAB; 32]),
        );
        assert_eq!(from_codec_point(&to_codec_point(&specific)), specific);
    }

    /// Regression test for the ChainSel fork-switch stall bug (2026-04-26):
    /// `prune_already_known_pending_headers` MUST keep fork headers whose
    /// hash differs from anything in the ChainDB, even when their slot is
    /// at or below the local ledger / chain tip.  The previous slot-based
    /// filter dropped them and stranded the node on the abandoned fork
    /// after a single-peer `MsgRollBackward` at the live tip.
    #[test]
    fn test_prune_keeps_fork_headers_at_or_below_applied_slot() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::{BlockNo, SlotNo};

        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        // Populate the local chain with three blocks ending at slot 200.
        let hash = |seed: u8| {
            let mut bytes = [0u8; 32];
            bytes[31] = seed;
            Hash32::from_bytes(bytes)
        };
        let h_a = hash(1);
        let h_b = hash(2);
        let h_c = hash(3);
        chain_db
            .add_block(h_a, SlotNo(100), BlockNo(50), Hash32::ZERO, b"a".to_vec())
            .unwrap();
        chain_db
            .add_block(h_b, SlotNo(150), BlockNo(51), h_a, b"b".to_vec())
            .unwrap();
        chain_db
            .add_block(h_c, SlotNo(200), BlockNo(52), h_b, b"c".to_vec())
            .unwrap();

        // Pending headers from a peer that rolled back to slot 150 (h_b)
        // and is now extending a competing fork:
        //   - fork-N at slot 175 (NEW hash, slot < applied tip 200)
        //   - fork-N+1 at slot 220 (NEW hash, slot > applied tip)
        //   - h_b (already in chain_db) — should be dropped
        let fork_root = [0xAAu8; 32];
        let fork_child = [0xBBu8; 32];
        let mut h_b_arr = [0u8; 32];
        h_b_arr.copy_from_slice(h_b.as_ref());
        let mut headers: Vec<PendingHeader> = vec![
            PendingHeader {
                slot: 175,
                hash: fork_root,
                header_cbor: vec![0xF6],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 220,
                hash: fork_child,
                header_cbor: vec![0xF6],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 150,
                hash: h_b_arr,
                header_cbor: vec![0xF6],
                body_size: None,
                prev_hash: None,
            },
        ];

        prune_already_known_pending_headers(&mut headers, &chain_db);

        // h_b is in chain_db → dropped.  The two fork headers must remain
        // even though the first one (slot 175) is BELOW our applied tip
        // (slot 200).  This is exactly the case that broke the prior
        // `h.slot > applied_slot` filter.
        let kept_hashes: Vec<[u8; 32]> = headers.iter().map(|h| h.hash).collect();
        assert!(
            kept_hashes.contains(&fork_root),
            "fork header at slot 175 (≤ applied tip) must be retained: kept={kept_hashes:?}"
        );
        assert!(
            kept_hashes.contains(&fork_child),
            "fork header at slot 220 must be retained"
        );
        assert!(
            !kept_hashes.contains(&h_b_arr),
            "header for block already in chain_db must be dropped"
        );
        assert_eq!(headers.len(), 2);
    }

    /// Verify that extract_slot_from_wrapped_header correctly parses a
    /// Shelley+ wrapped header: [era_tag, tag24(header_bytes)].
    #[test]
    fn test_extract_slot_shelley_header() {
        use minicbor::Encoder;

        // Build a fake Shelley wrapped header:
        // Outer: array(2) [era_tag=1, tag24(inner_bytes)]
        // Inner: array(2) [array(N) [block_number=100, slot=12345, ...], signature]
        let mut inner_buf = Vec::new();
        let mut inner_enc = Encoder::new(&mut inner_buf);
        inner_enc.array(2).unwrap(); // outer: [header_body, signature]
        inner_enc.array(3).unwrap(); // header_body: [block_number, slot, prev_hash]
        inner_enc.u64(100).unwrap(); // block_number
        inner_enc.u64(12345).unwrap(); // slot
        inner_enc.bytes(&[0u8; 32]).unwrap(); // prev_hash (placeholder)
        inner_enc.bytes(&[0u8; 64]).unwrap(); // signature (placeholder)

        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap(); // [era_tag, tag24(inner)]
        enc.u64(1).unwrap(); // Shelley era tag
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(&inner_buf).unwrap();

        assert_eq!(extract_slot_from_wrapped_header(&buf, 0), Some(12345));
    }

    // -----------------------------------------------------------------------
    // k-block rollback limit logic tests
    // -----------------------------------------------------------------------
    //
    // The chainsync_client_task checks:
    //   depth_slots = ledger_slot - rollback_slot
    //   if depth_slots > security_param * 2 → disconnect peer
    //
    // These tests exercise the threshold arithmetic directly so we can verify
    // the boundary conditions without spinning up a full peer connection.

    /// Compute the rollback depth in slots and compare against the threshold.
    /// Returns `true` if the rollback EXCEEDS the k-limit (should disconnect).
    fn rollback_exceeds_k_limit(ledger_slot: u64, rollback_slot: u64, security_param: u64) -> bool {
        if ledger_slot > rollback_slot {
            let depth_slots = ledger_slot - rollback_slot;
            let threshold = security_param.saturating_mul(2);
            depth_slots > threshold
        } else {
            false
        }
    }

    /// A shallow rollback (1 slot) must never trigger the limit.
    #[test]
    fn test_k_rollback_shallow_ok() {
        // Mainnet k=2160
        assert!(!rollback_exceeds_k_limit(1000, 999, 2160));
        // Preview k=432
        assert!(!rollback_exceeds_k_limit(1000, 999, 432));
    }

    /// Rollback to exactly the threshold boundary: depth == 2k (not over).
    #[test]
    fn test_k_rollback_at_boundary_ok() {
        let k: u64 = 432; // preview
        let ledger_slot = k * 2; // exactly at threshold
        let rollback_slot = 0;
        // depth = 2k, threshold = 2k → NOT > → ok
        assert!(!rollback_exceeds_k_limit(ledger_slot, rollback_slot, k));
    }

    /// Rollback one slot beyond the threshold must trigger the limit.
    #[test]
    fn test_k_rollback_one_over_limit() {
        let k: u64 = 432;
        let ledger_slot = k * 2 + 1; // one over
        let rollback_slot = 0;
        // depth = 2k+1 > threshold=2k → must disconnect
        assert!(rollback_exceeds_k_limit(ledger_slot, rollback_slot, k));
    }

    /// Rolling back to the same slot as the ledger tip is never an error.
    #[test]
    fn test_k_rollback_same_slot_ok() {
        assert!(!rollback_exceeds_k_limit(1000, 1000, 432));
    }

    /// Rolling back to a LATER slot (peer confusion / no-op) is not an error.
    #[test]
    fn test_k_rollback_ahead_of_ledger_ok() {
        // rollback_slot > ledger_slot should not trigger the limit.
        assert!(!rollback_exceeds_k_limit(1000, 2000, 432));
    }

    /// Mainnet k=2160: a 5000-slot deep rollback exceeds 2*2160=4320.
    #[test]
    fn test_k_rollback_mainnet_deep_exceeds() {
        assert!(rollback_exceeds_k_limit(10_000, 5_000, 2160));
    }

    /// Mainnet k=2160: a 4000-slot rollback is within 2*2160=4320.
    #[test]
    fn test_k_rollback_mainnet_within_limit() {
        assert!(!rollback_exceeds_k_limit(10_000, 6_001, 2160));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Issue #552: known_points construction for ChainSync MsgFindIntersect
    // ─────────────────────────────────────────────────────────────────────────

    use dugite_primitives::hash::Hash32 as TestHash32;
    use dugite_primitives::time::SlotNo as TestSlotNo;

    /// Helper: build a `Point::Specific` from a slot number and a single-byte
    /// hash seed (rest zero).
    fn pt(slot: u64, seed: u8) -> Point {
        let mut bytes = [0u8; 32];
        bytes[31] = seed;
        Point::Specific(TestSlotNo(slot), TestHash32::from_bytes(bytes))
    }

    /// Empty chain (everything Origin / empty) yields exactly `[Origin]`.
    #[test]
    fn known_points_empty_chain() {
        let pts = build_known_points(&KnownPointsInputs::default());
        assert_eq!(pts, vec![Point::Origin]);
    }

    /// Only the ImmutableDB tip is known (no volatile, no ledger): list is
    /// `[imm_tip, Origin]`.  This is the "fresh after Mithril import" case.
    #[test]
    fn known_points_only_immutable_tip() {
        let imm = pt(100, 1);
        let pts = build_known_points(&KnownPointsInputs {
            immutable_tip: Some(imm.clone()),
            ..Default::default()
        });
        assert_eq!(pts, vec![imm, Point::Origin]);
    }

    /// Immutable tip + ledger tip both present and distinct: both appear,
    /// ledger tip first (newest-first ordering), immutable tip after.
    #[test]
    fn known_points_immutable_and_ledger_tip_distinct() {
        let imm = pt(100, 1);
        let ledger = pt(200, 2);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: ledger.clone(),
            immutable_tip: Some(imm.clone()),
            ..Default::default()
        });
        assert_eq!(pts, vec![ledger, imm, Point::Origin]);
    }

    /// When the immutable tip equals the ledger tip (no volatile blocks
    /// above the immutable anchor — common when the volatile DB is empty),
    /// the duplicate is collapsed.
    #[test]
    fn known_points_immutable_equals_ledger_tip() {
        let p = pt(100, 1);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: p.clone(),
            immutable_tip: Some(p.clone()),
            ..Default::default()
        });
        assert_eq!(pts, vec![p, Point::Origin]);
    }

    /// Deep chain: immutable tip + ledger tip + several volatile points +
    /// deep historical anchors all appear in order, no duplicates, Origin
    /// last.  This is the steady-state case that issue #552 is about.
    #[test]
    fn known_points_deep_chain() {
        let imm = pt(1000, 10);
        let v0 = pt(1100, 11);
        let v1 = pt(1050, 12);
        let v2 = pt(1010, 13);
        let ledger = v0.clone();
        let deep0 = pt(900, 20);
        let deep1 = pt(800, 21);

        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: ledger.clone(),
            volatile_chain_points: vec![v0.clone(), v1.clone(), v2.clone()],
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![deep0.clone(), deep1.clone()],
            chain_diverged: false,
        });

        // Newest-first: [ledger(=v0), v1, v2, imm, deep0, deep1, Origin].
        assert_eq!(pts, vec![ledger, v1, v2, imm, deep0, deep1, Point::Origin]);
    }

    /// Acceptance condition for issue #552: when the local ledger is well
    /// past origin and the immutable tip is non-Origin, the immutable tip
    /// MUST appear in the known_points list.  Without this, a peer behind
    /// our ledger tip but ahead of our immutable tip cannot intersect.
    #[test]
    fn known_points_issue_552_always_includes_immutable_tip() {
        // Volatile points clustered near ledger tip (slot 2000).
        let imm = pt(1000, 1);
        let volatile: Vec<Point> = (0..10).map(|i| pt(1990 + i as u64, 100 + i)).collect();
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: pt(2000, 200),
            volatile_chain_points: volatile,
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![],
            chain_diverged: false,
        });

        assert!(
            pts.contains(&imm),
            "issue #552: immutable tip must be in known_points: {pts:?}"
        );
        assert_eq!(*pts.last().unwrap(), Point::Origin);
    }

    /// `chain_diverged = true`: volatile points must be excluded but the
    /// immutable tip and deep historical anchors must still be offered.
    #[test]
    fn known_points_chain_diverged_skips_volatile() {
        let imm = pt(1000, 1);
        let v_orphan = pt(1100, 99);
        let deep = pt(800, 20);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: pt(950, 10),
            volatile_chain_points: vec![v_orphan.clone()],
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![deep.clone()],
            chain_diverged: true,
        });

        assert!(pts.contains(&imm));
        assert!(pts.contains(&deep));
        assert!(
            !pts.contains(&v_orphan),
            "diverged volatile blocks must NOT be offered as intersection candidates: {pts:?}"
        );
        assert_eq!(*pts.last().unwrap(), Point::Origin);
    }

    /// Issue #927: when the ledger tip is BEHIND the immutable tip (crash
    /// recovery, index hole per #926, replay apply-failure), the offer must
    /// stay newest-first BY SLOT — the immutable tip must precede the stale
    /// ledger tip. With the old ledger-tip-first order every up-to-date peer
    /// intersected at the stale ledger tip, and the #699 guard then
    /// disconnected the peer's mandatory initial rollback to that exact
    /// point — an all-peer flap loop with HAA permanently lost.
    #[test]
    fn known_points_stale_ledger_tip_orders_immutable_first() {
        let imm = pt(129_476_005, 1);
        let stale_ledger = pt(129_437_577, 2);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: stale_ledger.clone(),
            volatile_chain_points: vec![],
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![],
            chain_diverged: false,
        });
        assert_eq!(pts, vec![imm, stale_ledger, Point::Origin]);
    }

    /// Issue #927 companion: in the stale-ledger state the volatile points
    /// (anchored at the immutable tip, so above it) still lead the list,
    /// followed by the immutable tip, then the stale ledger tip.
    #[test]
    fn known_points_stale_ledger_tip_keeps_volatile_first() {
        let imm = pt(1000, 1);
        let stale_ledger = pt(900, 2);
        let v0 = pt(1100, 11);
        let v1 = pt(1050, 12);
        let deep = pt(500, 20);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: stale_ledger.clone(),
            volatile_chain_points: vec![v0.clone(), v1.clone()],
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![deep.clone()],
            chain_diverged: false,
        });
        assert_eq!(pts, vec![v0, v1, imm, stale_ledger, deep, Point::Origin]);
    }

    /// #927 guard exemption: the initial rollback to the exact agreed
    /// intersection at-or-above the ledger tip is exempt; everything else
    /// keeps the #699 disconnect.
    #[test]
    fn exempt_initial_agreed_rollback_scoping() {
        let h1 = [1u8; 32];
        let h2 = [2u8; 32];
        let agreed = Some((900, h1));

        // The #927 wedge shape: initial rollback to the agreed intersection
        // (our stale ledger tip at slot 900, ledger_tip_slot == 900).
        assert!(is_exempt_initial_agreed_rollback(
            true,
            900,
            Some(h1),
            agreed,
            900
        ));

        // Mid-stream rollback to the same point: NOT exempt (#699 authority).
        assert!(!is_exempt_initial_agreed_rollback(
            false,
            900,
            Some(h1),
            agreed,
            900
        ));

        // Initial rollback to a DIFFERENT point than agreed (lying server):
        // not exempt — slot mismatch and hash mismatch each guard alone.
        assert!(!is_exempt_initial_agreed_rollback(
            true,
            899,
            Some(h1),
            agreed,
            800
        ));
        assert!(!is_exempt_initial_agreed_rollback(
            true,
            900,
            Some(h2),
            agreed,
            800
        ));

        // Agreed intersection BELOW the ledger tip (#699 divergent-peer
        // stall shape — deep-historical anchor): not exempt.
        assert!(!is_exempt_initial_agreed_rollback(
            true,
            500,
            Some(h1),
            Some((500, h1)),
            900
        ));

        // Origin rollback / no agreed intersection: never exempt.
        assert!(!is_exempt_initial_agreed_rollback(true, 0, None, agreed, 0));
        assert!(!is_exempt_initial_agreed_rollback(
            true,
            900,
            Some(h1),
            None,
            900
        ));
    }

    /// Duplicates across inputs are dropped (first occurrence wins).
    #[test]
    fn known_points_drops_duplicates() {
        let p1 = pt(100, 1);
        let p2 = pt(200, 2);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: p2.clone(),
            volatile_chain_points: vec![p1.clone(), p2.clone(), p1.clone()],
            immutable_tip: Some(p1.clone()),
            deep_historical: vec![p1.clone(), p2.clone()],
            chain_diverged: false,
        });

        // Each non-Origin point appears at most once.
        let p1_count = pts.iter().filter(|p| **p == p1).count();
        let p2_count = pts.iter().filter(|p| **p == p2).count();
        assert_eq!(p1_count, 1, "p1 deduped: {pts:?}");
        assert_eq!(p2_count, 1, "p2 deduped: {pts:?}");
        // Origin appears exactly once at the end.
        assert_eq!(
            pts.iter().filter(|p| **p == Point::Origin).count(),
            1,
            "Origin appears exactly once: {pts:?}"
        );
        assert_eq!(*pts.last().unwrap(), Point::Origin);
    }

    /// `Point::Origin` passed in any of the input fields is filtered out (we
    /// don't want to offer Origin twice).
    #[test]
    fn known_points_filters_explicit_origin_inputs() {
        let p = pt(100, 1);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: Point::Origin,
            volatile_chain_points: vec![Point::Origin, p.clone(), Point::Origin],
            immutable_tip: Some(Point::Origin),
            deep_historical: vec![Point::Origin],
            chain_diverged: false,
        });
        // Only `p` and the final Origin should remain.
        assert_eq!(pts, vec![p, Point::Origin]);
    }

    /// The point list is bounded — pathologically large inputs are
    /// truncated to `MAX_KNOWN_POINTS` (last slot reserved for Origin).
    #[test]
    fn known_points_bounded_by_max_known_points() {
        // Build many distinct points.
        let huge: Vec<Point> = (0..200u64).map(|i| pt(i, (i % 250) as u8 + 1)).collect();
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: pt(999, 250),
            volatile_chain_points: huge,
            immutable_tip: Some(pt(500, 251)),
            deep_historical: vec![],
            chain_diverged: false,
        });
        assert!(
            pts.len() <= MAX_KNOWN_POINTS,
            "expected <= {} points, got {}: {:?}",
            MAX_KNOWN_POINTS,
            pts.len(),
            pts
        );
        assert_eq!(*pts.last().unwrap(), Point::Origin);
    }

    /// Integration test for issue #552 acceptance criterion: build a real
    /// `ChainDB` whose state mirrors "synced to current preview tip" — many
    /// volatile blocks above an immutable anchor — and verify that the
    /// known_points list we would offer in `MsgFindIntersect` includes the
    /// ImmutableDB tip.
    ///
    /// Before the fix, the "ledger leads" branch built `[ledger_tip,
    /// last 10 volatile points, Origin]`, omitting the immutable tip.  A peer
    /// whose chain was behind us by more than ~10 volatile blocks would have
    /// zero overlap with that list and the intersection would collapse to
    /// Origin.  After the fix, the immutable tip is *always* included.
    #[test]
    fn known_points_integration_peer_behind_local_tip() {
        use dugite_primitives::hash::Hash32 as PH;
        use dugite_primitives::time::{BlockNo, SlotNo};

        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        // Helper for generating distinct test block hashes.
        let make_hash = |seed: u8| {
            let mut bytes = [0u8; 32];
            bytes[31] = seed;
            PH::from_bytes(bytes)
        };

        // Stage 1: lay down an immutable anchor at slot 100 (block 1).
        let imm_hash = make_hash(1);
        chain_db
            .put_blocks_batch(&[(SlotNo(100), &imm_hash, BlockNo(1), b"imm".as_ref(), false)])
            .unwrap();

        // Stage 2: extend with 15 volatile blocks (slots 200..3200, blocks
        // 2..16) — peer is behind us, so none of these will match its chain.
        let mut prev = imm_hash;
        for i in 0..15u8 {
            let h = make_hash(10 + i);
            let slot = SlotNo(200 + (i as u64) * 200);
            chain_db
                .add_block(h, slot, BlockNo(2 + i as u64), prev, vec![i])
                .unwrap();
            prev = h;
        }
        let ledger_tip_hash = prev;
        let ledger_tip = Point::Specific(SlotNo(200 + 14 * 200), ledger_tip_hash);

        // Snapshot the same fields chainsync_client_task uses.
        let volatile_chain_points = chain_db.get_chain_points(VOLATILE_POINTS_DEPTH);
        let immutable_tip = chain_db.get_immutable_tip_point();
        assert!(
            immutable_tip.is_some(),
            "test setup: ChainDB should have an immutable tip"
        );

        let deep_historical: Vec<Point> = chain_db
            .get_immutable_historical_points(DEEP_HISTORICAL_DEPTH)
            .into_iter()
            .map(|(slot, h)| Point::Specific(SlotNo(slot), h))
            .collect();

        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: ledger_tip.clone(),
            volatile_chain_points,
            immutable_tip: immutable_tip.clone(),
            deep_historical,
            chain_diverged: false,
        });

        // ─── Issue #552 acceptance: immutable tip MUST be present. ────────
        let imm_pt = immutable_tip.unwrap();
        assert!(
            pts.contains(&imm_pt),
            "issue #552: immutable tip {imm_pt:?} must be offered in known_points: {pts:?}"
        );

        // ─── Defense-in-depth checks for the rest of the list shape. ──────
        // Ledger tip is offered (any peer at our tip can intersect).
        assert!(
            pts.contains(&ledger_tip),
            "ledger tip {ledger_tip:?} must be offered: {pts:?}"
        );
        // Origin is the final fallback (so even a peer with no overlap can
        // sync from genesis).
        assert_eq!(*pts.last().unwrap(), Point::Origin);
        // Bounded length (sanity).
        assert!(pts.len() <= MAX_KNOWN_POINTS);

        // ─── Simulate "peer is k blocks behind us": construct a candidate
        //     point that matches the immutable anchor (which the peer DOES
        //     know about) and verify the peer's MsgIntersectFound would
        //     succeed against our offered list.
        // This is the actual recovery path: peer's MsgFindIntersect response
        // anchors at our immutable tip, RollForward stream resumes from
        // there, tip-age stops growing.
        let peer_known_hashes: std::collections::HashSet<&PH> =
            std::iter::once(&imm_hash).collect();
        let intersection: Option<&Point> = pts
            .iter()
            .find(|p| p.hash().is_some_and(|h| peer_known_hashes.contains(h)));
        assert!(
            intersection.is_some(),
            "peer behind us (only knows immutable anchor) must find intersection \
             in our offered list: {pts:?}"
        );
        assert_eq!(intersection.unwrap(), &imm_pt);
    }

    /// Ordering: the list is NEWEST-FIRST so an up-to-date peer intersects at
    /// our tip (not the immutable tip, which would force a re-stream of the
    /// whole volatile window).  `MsgFindIntersect` returns the first listed
    /// point the peer has, so the newest point (the ledger tip) must come
    /// first; the immutable tip is still present (issue #552) but lower in the
    /// list as the behind-peer fallback.
    #[test]
    fn known_points_newest_first_ledger_tip_leads_immutable_present() {
        let imm = pt(1000, 10);
        let ledger = pt(2000, 20);
        let pts = build_known_points(&KnownPointsInputs {
            ledger_tip: ledger.clone(),
            volatile_chain_points: vec![pt(1990, 30), pt(1980, 31)],
            immutable_tip: Some(imm.clone()),
            deep_historical: vec![pt(900, 40)],
            chain_diverged: false,
        });
        // First non-Origin entry is our newest point (the ledger tip).
        assert_eq!(
            pts[0], ledger,
            "ledger tip must lead (newest-first): {pts:?}"
        );
        // Immutable tip is still offered (issue #552) — just not first.
        assert!(
            pts.contains(&imm),
            "imm tip must still be present (#552): {pts:?}"
        );
        let ledger_idx = pts.iter().position(|p| p == &ledger).unwrap();
        let imm_idx = pts.iter().position(|p| p == &imm).unwrap();
        assert!(
            ledger_idx < imm_idx,
            "ledger tip must precede immutable tip: {pts:?}"
        );
    }
}

// ─── proptest: known_points construction invariants (Issue #552) ─────────────

#[cfg(test)]
mod known_points_proptests {
    use super::*;
    use proptest::prelude::*;

    fn arb_point() -> impl Strategy<Value = Point> {
        prop_oneof![
            Just(Point::Origin),
            (1u64..1_000_000u64, any::<[u8; 32]>()).prop_map(|(s, h)| {
                Point::Specific(
                    dugite_primitives::time::SlotNo(s),
                    dugite_primitives::hash::Hash32::from_bytes(h),
                )
            }),
        ]
    }

    fn arb_opt_point() -> impl Strategy<Value = Option<Point>> {
        prop_oneof![Just(None), arb_point().prop_map(Some),]
    }

    fn arb_inputs() -> impl Strategy<Value = KnownPointsInputs> {
        (
            arb_point(),
            proptest::collection::vec(arb_point(), 0..20),
            arb_opt_point(),
            proptest::collection::vec(arb_point(), 0..20),
            any::<bool>(),
        )
            .prop_map(
                |(ledger_tip, volatile, immutable_tip, deep_historical, chain_diverged)| {
                    KnownPointsInputs {
                        ledger_tip,
                        volatile_chain_points: volatile,
                        immutable_tip,
                        deep_historical,
                        chain_diverged,
                    }
                },
            )
    }

    proptest! {
        /// Issue #552 invariant: if the immutable tip is non-Origin, the
        /// known_points list MUST contain it (regardless of any other input
        /// state — divergence, deep history, large volatile slates, etc.).
        ///
        /// This is the property that previously broke: when a peer was behind
        /// our ledger tip, the constructed list omitted the immutable tip
        /// and the intersection collapsed to Origin.
        #[test]
        fn prop_immutable_tip_always_present_when_non_origin(inp in arb_inputs()) {
            let pts = build_known_points(&inp);
            if let Some(ref imm) = inp.immutable_tip {
                if *imm != Point::Origin {
                    prop_assert!(
                        pts.contains(imm),
                        "immutable tip {imm:?} must be in known_points {pts:?}"
                    );
                }
            }
        }

        /// The list always ends in `Point::Origin` (so the peer has a final
        /// fallback to sync from genesis when nothing else matches).
        #[test]
        fn prop_origin_always_last(inp in arb_inputs()) {
            let pts = build_known_points(&inp);
            prop_assert!(!pts.is_empty());
            prop_assert_eq!(pts.last().cloned(), Some(Point::Origin));
        }

        /// `Point::Origin` appears exactly once in the output (only as the
        /// final fallback, never via any of the input slots).
        #[test]
        fn prop_origin_appears_exactly_once(inp in arb_inputs()) {
            let pts = build_known_points(&inp);
            let n = pts.iter().filter(|p| **p == Point::Origin).count();
            prop_assert_eq!(n, 1);
        }

        /// No duplicates in the output list.
        #[test]
        fn prop_no_duplicates(inp in arb_inputs()) {
            let pts = build_known_points(&inp);
            let mut sorted = pts.clone();
            sorted.sort();
            sorted.dedup();
            prop_assert_eq!(sorted.len(), pts.len(), "duplicates: {:?}", pts);
        }

        /// Length bound: never exceeds `MAX_KNOWN_POINTS`.
        #[test]
        fn prop_bounded_length(inp in arb_inputs()) {
            let pts = build_known_points(&inp);
            prop_assert!(pts.len() <= MAX_KNOWN_POINTS);
        }

        /// When `chain_diverged = true`, no volatile point appears in the
        /// output (volatile blocks may be orphan fork blocks; we MUST NOT
        /// offer them as intersection candidates).
        #[test]
        fn prop_diverged_excludes_volatile(inp in arb_inputs()) {
            if !inp.chain_diverged {
                return Ok(());
            }
            let pts = build_known_points(&inp);
            for v in &inp.volatile_chain_points {
                if *v == Point::Origin {
                    continue;
                }
                // The same point COULD also be the ledger/immutable tip; in
                // that case its presence is fine.  Only flag if it's *only*
                // present as a volatile point.
                let elsewhere = inp.immutable_tip.as_ref() == Some(v)
                    || inp.ledger_tip == *v
                    || inp.deep_historical.contains(v);
                if !elsewhere {
                    prop_assert!(
                        !pts.contains(v),
                        "diverged volatile point {v:?} leaked into output {pts:?}"
                    );
                }
            }
        }
    }
}

// ─── Additional unit tests ────────────────────────────────────────────────────

#[cfg(test)]
mod additional_sync_tests {
    use super::*;
    use crate::node::connection_lifecycle::select_headers_to_fetch;
    use dugite_primitives::block::{
        Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput,
    };
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};

    // ── Helper: build a minimal Block for genesis-validation tests ────────────

    fn make_block(
        era: Era,
        block_number: u64,
        slot: u64,
        header_hash: Hash32,
        prev_hash: Hash32,
    ) -> Block {
        Block {
            header: BlockHeader {
                header_hash,
                prev_hash,
                issuer_vkey: vec![],
                vrf_vkey: vec![],
                vrf_result: VrfOutput {
                    output: vec![],
                    proof: vec![],
                },
                block_number: BlockNo(block_number),
                slot: SlotNo(slot),
                epoch_nonce: Hash32::from_bytes([0u8; 32]),
                body_size: 0,
                body_hash: Hash32::from_bytes([0u8; 32]),
                operational_cert: OperationalCert {
                    hot_vkey: vec![],
                    sequence_number: 0,
                    kes_period: 0,
                    sigma: vec![],
                },
                protocol_version: ProtocolVersion { major: 9, minor: 0 },
                kes_signature: vec![],
                nonce_vrf_output: vec![],
                nonce_vrf_proof: vec![],
                prev_nonce: None,
                raw_header_body: None,
            },
            transactions: vec![],
            era,
            raw_cbor: None,
        }
    }

    // ── validate_genesis_blocks ───────────────────────────────────────────────

    /// Empty slice must always succeed regardless of configured hashes.
    #[test]
    fn validate_genesis_empty_slice_ok() {
        let expected = Hash32::from_bytes([0xAB; 32]);
        assert!(validate_genesis_blocks(&[], Some(&expected), Some(&expected)).is_ok());
        assert!(validate_genesis_blocks(&[], None, None).is_ok());
    }

    /// Non-genesis first block (block_number > 0) must skip validation and succeed.
    #[test]
    fn validate_genesis_non_genesis_block_skipped() {
        let wrong_hash = Hash32::from_bytes([0xFF; 32]);
        let block = make_block(
            Era::Byron,
            1, // block_number=1, not genesis
            0,
            Hash32::from_bytes([0xAA; 32]),
            Hash32::ZERO,
        );
        // Even if expected hash is wrong, non-genesis blocks skip validation.
        assert!(validate_genesis_blocks(&[block], Some(&wrong_hash), None).is_ok());
    }

    /// Byron EBB at block_number=0 with correct hash must pass.
    #[test]
    fn validate_genesis_byron_correct_hash_ok() {
        let hash = Hash32::from_bytes([0x11; 32]);
        let block = make_block(Era::Byron, 0, 0, hash, Hash32::ZERO);
        // The hash() method on Block returns &header.header_hash.
        // We pass the same value as the expected — must succeed.
        let result =
            validate_genesis_blocks(std::slice::from_ref(&block), Some(block.hash()), None);
        assert!(
            result.is_ok(),
            "correct Byron genesis hash must pass: {result:?}"
        );
    }

    /// Byron EBB at block_number=0 with wrong expected hash must fail.
    #[test]
    fn validate_genesis_byron_wrong_hash_fails() {
        let actual_hash = Hash32::from_bytes([0x11; 32]);
        let expected_hash = Hash32::from_bytes([0x22; 32]);
        let block = make_block(Era::Byron, 0, 0, actual_hash, Hash32::ZERO);
        let result = validate_genesis_blocks(&[block], Some(&expected_hash), None);
        assert!(
            result.is_err(),
            "mismatched Byron genesis hash must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Byron genesis block hash mismatch"),
            "error message should mention mismatch; got: {msg}"
        );
    }

    /// Byron EBB with no expected hash configured must succeed (just a warning).
    #[test]
    fn validate_genesis_byron_no_expected_hash_ok() {
        let block = make_block(
            Era::Byron,
            0,
            0,
            Hash32::from_bytes([0x11; 32]),
            Hash32::ZERO,
        );
        assert!(validate_genesis_blocks(&[block], None, None).is_ok());
    }

    /// Shelley-first chain (block_number=0, era=Shelley): correct prev_hash passes.
    #[test]
    fn validate_genesis_shelley_correct_prev_hash_ok() {
        let shelley_genesis_hash = Hash32::from_bytes([0x33; 32]);
        let block = make_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0xAA; 32]),
            shelley_genesis_hash, // prev_hash == shelley genesis hash
        );
        let result = validate_genesis_blocks(&[block], None, Some(&shelley_genesis_hash));
        assert!(
            result.is_ok(),
            "correct Shelley genesis prev_hash must pass: {result:?}"
        );
    }

    /// Shelley-first chain (block_number=0): wrong prev_hash must fail.
    #[test]
    fn validate_genesis_shelley_wrong_prev_hash_fails() {
        let shelley_genesis_hash = Hash32::from_bytes([0x33; 32]);
        let wrong_prev = Hash32::from_bytes([0x44; 32]);
        let block = make_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0xAA; 32]),
            wrong_prev, // wrong prev_hash
        );
        let result = validate_genesis_blocks(&[block], None, Some(&shelley_genesis_hash));
        assert!(
            result.is_err(),
            "mismatched Shelley genesis hash must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Shelley genesis hash mismatch"),
            "error should mention Shelley mismatch; got: {msg}"
        );
    }

    /// Shelley-first chain with no expected Shelley hash: must succeed (just a warning).
    #[test]
    fn validate_genesis_shelley_no_expected_hash_ok() {
        let block = make_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0xAA; 32]),
            Hash32::from_bytes([0x99; 32]),
        );
        assert!(validate_genesis_blocks(&[block], None, None).is_ok());
    }

    // ── unwrap_hfc_header ─────────────────────────────────────────────────────

    /// Build a minimal valid HFC-wrapped header and verify unwrapping works.
    #[test]
    fn unwrap_hfc_header_valid_returns_inner_bytes() {
        use minicbor::Encoder;
        let inner_bytes = b"fake_inner_header";
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(2).unwrap(); // era_tag=2 (Shelley)
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(inner_bytes).unwrap();

        let result = unwrap_hfc_header(&buf);
        assert_eq!(result, Some(inner_bytes.as_ref()));
    }

    /// CBOR that is NOT an array(2) returns None (not HFC format).
    #[test]
    fn unwrap_hfc_header_not_array2_returns_none() {
        use minicbor::Encoder;
        // array(3) is not HFC
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(3).unwrap();
        enc.u64(1).unwrap();
        enc.u64(2).unwrap();
        enc.u64(3).unwrap();
        assert_eq!(unwrap_hfc_header(&buf), None);
    }

    /// Empty CBOR returns None.
    #[test]
    fn unwrap_hfc_header_empty_returns_none() {
        assert_eq!(unwrap_hfc_header(&[]), None);
    }

    /// Array with wrong tag (not tag 24) returns None.
    #[test]
    fn unwrap_hfc_header_wrong_tag_returns_none() {
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(1).unwrap(); // era_tag
        enc.tag(minicbor::data::Tag::new(6)).unwrap(); // wrong tag
        enc.bytes(b"foo").unwrap();
        assert_eq!(unwrap_hfc_header(&buf), None);
    }

    /// Build the shared eager-validation fixture: a REAL Praos header
    /// (Babbage shape == Conway shape) re-wrapped with the WIRE Conway index,
    /// plus the header's own decoded slot / pool id / VRF key hash.
    #[cfg(test)]
    fn eager_fixture_header() -> (Vec<u8>, u64, dugite_primitives::hash::Hash28, Hash32) {
        let wrapped_block = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dugite-serialization/tests/fixtures/babbage_indef_tx_bodies_block33760.cbor"
        ))
        .expect("fixture");
        let mut dec = minicbor::Decoder::new(&wrapped_block);
        dec.array().expect("outer");
        let _ = dec.u64().expect("era");
        dec.array().expect("block");
        let start = dec.position();
        dec.skip().expect("skip header");
        let header_bytes = &wrapped_block[start..dec.position()];
        let mut buf = Vec::with_capacity(header_bytes.len() + 8);
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(6).unwrap(); // WIRE Conway index
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(header_bytes).unwrap();
        }
        let hdr = dugite_serialization::decode_wire_wrapped_block_header(&buf).expect("decode");
        let pool_id = dugite_primitives::hash::blake2b_224(&hdr.issuer_vkey);
        let vrf_keyhash = dugite_primitives::hash::blake2b_256(&hdr.vrf_vkey);
        (buf, hdr.slot.0, pool_id, vrf_keyhash)
    }

    /// Root cause of the 2026-07-16 "node cannot progress" report: eager header
    /// validation applied the view's `set` snapshot to a header belonging to the
    /// NEXT epoch.
    ///
    /// Per `references/era-rules/shelley-core.md` §NEWEPOCH:
    /// `pd' = ssStakeMarkPoolDistr (esSnapshots es)` — the distribution active in
    /// epoch E+1 is the PRE-rotation `mark`, which only becomes `set` once the
    /// boundary is crossed. A pool that first gained stake in the mark snapshot is
    /// therefore absent from `set` while the ledger is still in epoch E, so every
    /// header it issued in E+1 was rejected as `UnregisteredPool` — and because an
    /// eager failure tears down the connection, EVERY honest peer serving that
    /// (valid) block was disconnected in turn. Permanent wedge.
    ///
    /// Reproduced live on preview 2026-07-25: ledger at epoch 1357 rejected the
    /// valid block at slot 117334934 (epoch 1358) from pool 0b553dde…, which is
    /// present in mark(1357) with 1.009T stake but absent from set(1356).
    #[test]
    fn eager_validate_uses_mark_snapshot_for_next_epoch_header() {
        use dugite_ledger::state::{PoolRegistration, StakeSnapshot};
        use dugite_primitives::time::EpochNo;
        use dugite_primitives::value::Lovelace;
        use std::collections::HashMap;
        use std::sync::Arc;

        let (buf, slot, pool_id, vrf_keyhash) = eager_fixture_header();

        let mut state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        // Pure-Shelley epoch arithmetic so header_epoch == slot / epoch_length.
        state.shelley_transition_epoch = 0;
        state.epoch_length = 1000;
        let header_epoch = slot / 1000;
        assert!(header_epoch >= 2, "fixture slot must be past epoch 2");
        // Ledger sits one epoch BEHIND the header — the exact catch-up window
        // in which ChainSync headers run ahead of block apply.
        state.epoch = EpochNo(header_epoch - 1);

        let reg = PoolRegistration {
            pool_id,
            vrf_keyhash,
            pledge: Lovelace(0),
            cost: Lovelace(0),
            margin_numerator: 0,
            margin_denominator: 1,
            reward_account: Vec::new(),
            owners: Vec::new(),
            relays: Vec::new(),
            metadata_url: None,
            metadata_hash: None,
        };
        // mark = snapshot taken at the start of the view's epoch; it is the
        // distribution that governs epoch `header_epoch`. Contains our pool.
        let mut mark_stake = HashMap::new();
        mark_stake.insert(pool_id, Lovelace(1_000_000_000_000));
        let mut mark_params = HashMap::new();
        mark_params.insert(pool_id, reg);
        state.epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: EpochNo(header_epoch - 1),
            pool_stake: mark_stake,
            pool_params: Arc::new(mark_params),
            ..StakeSnapshot::empty(EpochNo(header_epoch - 1))
        });
        // set = the PREVIOUS epoch's distribution. Populated (so the
        // incomplete-view guard does not fire) but WITHOUT our pool — exactly
        // the newly-active-pool case that wedged preview.
        let mut set_stake = HashMap::new();
        set_stake.insert(
            dugite_primitives::hash::Hash28::from_bytes([0xAB; 28]),
            Lovelace(500_000_000_000),
        );
        state.epochs.snapshots.set = Some(StakeSnapshot {
            epoch: EpochNo(header_epoch - 2),
            pool_stake: set_stake,
            ..StakeSnapshot::empty(EpochNo(header_epoch - 2))
        });

        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        let mut consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        consensus.set_strict_verification(true);
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut counters = HashMap::new();

        let result = eager_validate_header(&peer, &buf, slot, &consensus, &view, &mut counters);

        assert!(
            !matches!(
                result,
                Err(dugite_consensus::ConsensusError::UnregisteredPool { .. })
            ),
            "next-epoch header must be resolved against the mark snapshot, not set \
             — got {result:?} (this is the preview wedge)"
        );
        assert!(
            !matches!(
                result,
                Err(dugite_consensus::ConsensusError::VrfKeyMismatch)
            ),
            "pool VRF key must come from the snapshot governing the header's epoch \
             — got {result:?}"
        );
    }

    /// A pool present in the snapshot's `pool_stake` but MISSING from its
    /// `pool_params` must SKIP eager validation, never reject.
    ///
    /// The old code substituted `Hash32::ZERO` for the missing registration,
    /// which can never equal `blake2b_256(header.vrf_vkey)` — so the header was
    /// rejected with `VrfKeyMismatch` and every peer serving it was dropped.
    /// That is the exact error in the 2026-07-16 report (slot 117417642, pool
    /// 175b7c01…, whose on-chain VRF key hashes correctly to its one and only
    /// registration). A fabricated hash must never drive a rejection.
    #[test]
    fn eager_validate_skips_when_snapshot_lacks_pool_params() {
        use dugite_ledger::state::StakeSnapshot;
        use dugite_primitives::time::EpochNo;
        use dugite_primitives::value::Lovelace;
        use std::collections::HashMap;

        let (buf, slot, pool_id, _vrf) = eager_fixture_header();

        let mut state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        state.shelley_transition_epoch = 0;
        state.epoch_length = 1000;
        let header_epoch = slot / 1000;
        state.epoch = EpochNo(header_epoch);

        // Pool HAS stake but NO registration entry in the same snapshot.
        let mut stake = HashMap::new();
        stake.insert(pool_id, Lovelace(1_000_000_000_000));
        state.epochs.snapshots.set = Some(StakeSnapshot {
            epoch: EpochNo(header_epoch - 1),
            pool_stake: stake,
            ..StakeSnapshot::empty(EpochNo(header_epoch - 1))
        });

        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        let mut consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        consensus.set_strict_verification(true);
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut counters = HashMap::new();

        let result = eager_validate_header(&peer, &buf, slot, &consensus, &view, &mut counters);

        assert!(
            matches!(result, Ok(false)),
            "missing pool_params must SKIP (defer to body apply), never reject \
             on a fabricated ZERO VRF hash — got {result:?}"
        );
    }

    /// Eager validation with an INCOMPLETE view (no/empty set snapshot —
    /// genesis epochs before the first mark/set/go rotation) must SKIP
    /// (Ok(false)), never reject: rejecting partitioned the devnet relay
    /// from its BP ("block from unregistered pool" at slot 4, 2026-06-12)
    /// the moment the wire-era-gate fix made eager validation actually run
    /// for Conway headers.
    #[test]
    fn eager_validate_skips_on_missing_set_snapshot() {
        use std::collections::HashMap;
        // Real Babbage header from the block-33760 fixture; the Praos header
        // shape is identical across Babbage/Conway, so wrapping it with the
        // WIRE Conway index (6) exercises the Conway eager path with a
        // structurally valid header.
        let wrapped_block = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dugite-serialization/tests/fixtures/babbage_indef_tx_bodies_block33760.cbor"
        ))
        .expect("fixture");
        let mut dec = minicbor::Decoder::new(&wrapped_block);
        dec.array().expect("outer");
        let _ = dec.u64().expect("era");
        dec.array().expect("block");
        let start = dec.position();
        dec.skip().expect("skip header");
        let header_bytes = &wrapped_block[start..dec.position()];
        let mut buf = Vec::with_capacity(header_bytes.len() + 8);
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(6).unwrap(); // WIRE Conway index
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(header_bytes).unwrap();
        }

        let consensus = dugite_consensus::praos::OuroborosPraos::new(11);
        // Fresh ledger state → LedgerView with NO set snapshot.
        let state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let view = crate::node::ledger_view::LedgerView::from_state(&state);
        assert!(
            view.snapshots
                .set
                .as_ref()
                .is_none_or(|s| s.pool_stake.is_empty()),
            "test precondition: view must have no populated set snapshot"
        );
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let mut counters = HashMap::new();
        let result = eager_validate_header(&peer, &buf, 0, &consensus, &view, &mut counters);
        assert!(
            matches!(result, Ok(false)),
            "incomplete view must SKIP eager validation, got {result:?}"
        );
    }

    // ── extract_header_fetch_info ────────────────────────────────────────────

    /// REAL mainnet Babbage header (lifted from the block-33760 fixture),
    /// wrapped in the N2N HFC envelope `[6, tag24(bytes(header))]` exactly as
    /// ChainSync delivers it: the declared block body size MUST be extracted.
    /// Guards the #747 exact byte-accounting path — if this returns None the
    /// range builder silently degrades to the average-based estimate that
    /// overran the mux ingress live.
    #[test]
    fn extract_body_size_from_real_babbage_wrapped_header() {
        let wrapped_block = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dugite-serialization/tests/fixtures/babbage_indef_tx_bodies_block33760.cbor"
        ))
        .expect("fixture");
        // Fixture layout: [era_tag(6), block([header, ...])]
        let mut dec = minicbor::Decoder::new(&wrapped_block);
        dec.array().expect("outer");
        assert_eq!(dec.u64().expect("era"), 6, "fixture must be Babbage");
        dec.array().expect("block");
        let start = dec.position();
        dec.skip().expect("skip header");
        let header_bytes = &wrapped_block[start..dec.position()];

        // Re-wrap as the N2N ChainSync WIRE envelope: [5, tag24(bytes(h))].
        // The wire HFC index for Babbage is 5 (0-based, Byron combined) —
        // NOT the storage tag 6 the fixture file carries. Using the storage
        // tag here previously masked the wire/storage mis-dispatch that made
        // extraction fail for every live mainnet header.
        let mut buf = Vec::with_capacity(header_bytes.len() + 8);
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(5).unwrap();
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(header_bytes).unwrap();
        }

        let (size, prev) = extract_header_fetch_info(&buf);
        // Ground truth from manual CBOR analysis of the fixture: header_body
        // index 6 (block_body_size) = 7,531 = EXACTLY the fixture's actual
        // body bytes (8,386 total block − 855 header). An extraction that
        // returns any other value silently breaks the #747 byte accounting.
        assert_eq!(
            size,
            Some(7_531),
            "declared body size must match the fixture's actual body bytes"
        );
        assert!(
            prev.is_some(),
            "must extract prev_hash for chain-adjacency run splitting"
        );
    }

    // ── extract_hash_from_header ──────────────────────────────────────────────

    /// Hash is deterministic: same input always produces the same 32 bytes.
    #[test]
    fn extract_hash_from_header_deterministic() {
        let header = b"test_header_bytes";
        let h1 = extract_hash_from_header(header);
        let h2 = extract_hash_from_header(header);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    /// Different inputs produce different hashes.
    #[test]
    fn extract_hash_from_header_distinct_inputs_distinct_outputs() {
        let h1 = extract_hash_from_header(b"input_a");
        let h2 = extract_hash_from_header(b"input_b");
        assert_ne!(h1, h2);
    }

    /// HFC-wrapped header: hash is computed from INNER bytes, not the wrapper.
    #[test]
    fn extract_hash_from_header_uses_inner_bytes_not_wrapper() {
        use minicbor::Encoder;
        let inner_bytes = b"real_inner_header_data";

        // Build HFC wrapper
        let mut wrapped = Vec::new();
        let mut enc = Encoder::new(&mut wrapped);
        enc.array(2).unwrap();
        enc.u64(3).unwrap(); // era_tag
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(inner_bytes).unwrap();

        // Hash of the wrapped form (as if unwrapping failed)
        let hash_of_wrapper = extract_hash_from_header(&wrapped);
        // Hash of the raw inner bytes (what the function should compute)
        let hash_of_inner = dugite_primitives::hash::blake2b_256(inner_bytes.as_ref());
        let mut expected = [0u8; 32];
        expected.copy_from_slice(hash_of_inner.as_ref());

        // The function should use inner bytes.
        assert_eq!(
            hash_of_wrapper, expected,
            "extract_hash_from_header must hash the INNER (unwrapped) bytes"
        );
    }

    // ── to_codec_point / from_codec_point ─────────────────────────────────────

    /// Origin round-trips through both conversion functions.
    #[test]
    fn point_roundtrip_origin() {
        let p = Point::Origin;
        assert_eq!(from_codec_point(&to_codec_point(&p)), p);
    }

    /// Specific point round-trips preserving slot and hash.
    #[test]
    fn point_roundtrip_specific_preserves_slot_and_hash() {
        let hash = Hash32::from_bytes([0xDE; 32]);
        let p = Point::Specific(SlotNo(99999), hash);
        let rt = from_codec_point(&to_codec_point(&p));
        assert_eq!(rt, p);
    }

    /// Slot=0 with non-zero hash round-trips correctly.
    #[test]
    fn point_roundtrip_slot_zero() {
        let hash = Hash32::from_bytes([0x01; 32]);
        let p = Point::Specific(SlotNo(0), hash);
        assert_eq!(from_codec_point(&to_codec_point(&p)), p);
    }

    /// to_codec_point(Origin) → CodecPoint::Origin.
    #[test]
    fn to_codec_point_origin() {
        assert!(matches!(to_codec_point(&Point::Origin), CodecPoint::Origin));
    }

    /// to_codec_point(Specific) → CodecPoint::Specific with correct (slot, hash).
    #[test]
    fn to_codec_point_specific_fields() {
        let hash = Hash32::from_bytes([0xAB; 32]);
        let p = Point::Specific(SlotNo(42), hash);
        match to_codec_point(&p) {
            CodecPoint::Specific(slot, arr) => {
                assert_eq!(slot, 42);
                assert_eq!(arr, [0xABu8; 32]);
            }
            other => panic!("expected Specific, got {other:?}"),
        }
    }

    // ── prune_already_known_pending_headers ───────────────────────────────────

    /// Empty pending list stays empty after pruning.
    #[test]
    fn prune_empty_headers_noop() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();
        let mut headers: Vec<PendingHeader> = vec![];
        prune_already_known_pending_headers(&mut headers, &chain_db);
        assert!(headers.is_empty());
    }

    /// Headers whose hashes are not in the ChainDB are all retained.
    #[test]
    fn prune_unknown_headers_all_retained() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();
        let headers = vec![
            PendingHeader {
                slot: 10,
                hash: [0x01; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 20,
                hash: [0x02; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let mut headers = headers;
        prune_already_known_pending_headers(&mut headers, &chain_db);
        assert_eq!(headers.len(), 2);
    }

    /// Headers whose hashes are in the ChainDB are dropped.
    #[test]
    fn prune_known_headers_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        let h1 = Hash32::from_bytes([0xAA; 32]);
        let h2 = Hash32::from_bytes([0xBB; 32]);
        chain_db
            .add_block(
                h1,
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                b"block1".to_vec(),
            )
            .unwrap();

        let mut pending = vec![
            PendingHeader {
                slot: 100,
                hash: [0xAAu8; 32], // in ChainDB
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 200,
                hash: {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(h2.as_ref());
                    arr
                }, // NOT in ChainDB
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];

        prune_already_known_pending_headers(&mut pending, &chain_db);
        assert_eq!(pending.len(), 1, "only unknown header should remain");
        assert_eq!(pending[0].hash, {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(h2.as_ref());
            arr
        });
    }

    /// All headers known → list becomes empty.
    #[test]
    fn prune_all_known_headers_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        let h1 = Hash32::from_bytes([0xCC; 32]);
        let h2 = Hash32::from_bytes([0xDD; 32]);
        chain_db
            .add_block(h1, SlotNo(1), BlockNo(1), Hash32::ZERO, b"a".to_vec())
            .unwrap();
        chain_db
            .add_block(h2, SlotNo(2), BlockNo(2), h1, b"b".to_vec())
            .unwrap();

        let mut arr1 = [0u8; 32];
        arr1.copy_from_slice(h1.as_ref());
        let mut arr2 = [0u8; 32];
        arr2.copy_from_slice(h2.as_ref());

        let mut pending = vec![
            PendingHeader {
                slot: 1,
                hash: arr1,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 2,
                hash: arr2,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        prune_already_known_pending_headers(&mut pending, &chain_db);
        assert!(pending.is_empty(), "all known headers must be removed");
    }

    // ── #767 residual: inline retain-by-hash prune (production path) ───────────
    //
    // The MsgRollForward + refill_ticker arms no longer call
    // `prune_already_known_pending_headers` directly; they inline a
    // "snapshot hashes → compute already-stored set under `chain_db.read()`
    // alone → `retain(|h| !known.contains(&h.hash))`" idiom so the chain_db
    // read never happens under `candidate_chains.write()` (the lock convoy that
    // wedged #767).  These tests pin that inline idiom to the canonical
    // (now test-only) helper so a future refactor cannot silently diverge them.

    /// Replicates the exact inline production idiom: build the already-stored
    /// `known` set from `chain_db.has_block`, then `retain` by hash.
    fn retain_by_hash_inline(pending: &mut Vec<PendingHeader>, chain_db: &dugite_storage::ChainDB) {
        let hashes: Vec<[u8; 32]> = pending.iter().map(|h| h.hash).collect();
        let known: std::collections::HashSet<[u8; 32]> = hashes
            .into_iter()
            .filter(|h| chain_db.has_block(&Hash32::from_bytes(*h)))
            .collect();
        if !known.is_empty() {
            pending.retain(|h| !known.contains(&h.hash));
        }
    }

    /// The inline retain-by-hash idiom must be byte-for-byte equivalent to the
    /// canonical `prune_already_known_pending_headers`, including the
    /// 2026-04-26 fork regression rule: a competing-fork header at a slot
    /// at/below the applied tip whose block is NOT in the ChainDB must be KEPT.
    #[test]
    fn retain_by_hash_equivalent_to_prune_known_pending_headers() {
        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        // Canonical chain: slots 1..=3 stored in the ChainDB.
        let c1 = Hash32::from_bytes([0x11; 32]);
        let c2 = Hash32::from_bytes([0x22; 32]);
        let c3 = Hash32::from_bytes([0x33; 32]);
        chain_db
            .add_block(c1, SlotNo(1), BlockNo(1), Hash32::ZERO, b"c1".to_vec())
            .unwrap();
        chain_db
            .add_block(c2, SlotNo(2), BlockNo(2), c1, b"c2".to_vec())
            .unwrap();
        chain_db
            .add_block(c3, SlotNo(3), BlockNo(3), c2, b"c3".to_vec())
            .unwrap();

        let to_arr = |h: Hash32| {
            let mut a = [0u8; 32];
            a.copy_from_slice(h.as_ref());
            a
        };

        // 5 pending headers:
        //  - c2, c3: hashes IN the ChainDB              → must be DROPPED
        //  - fork@slot2: slot at/below tip, NOT stored  → must be KEPT (regression)
        //  - u4, u5: unknown hashes not stored          → must be KEPT
        let fork = [0xF0u8; 32]; // competing-fork header at slot 2, not in ChainDB
        let u4 = [0x44u8; 32];
        let u5 = [0x55u8; 32];
        let make = |slot: u64, hash: [u8; 32]| PendingHeader {
            slot,
            hash,
            header_cbor: vec![],
            body_size: None,
            prev_hash: None,
        };
        let build = || {
            vec![
                make(2, to_arr(c2)),
                make(2, fork),
                make(3, to_arr(c3)),
                make(4, u4),
                make(5, u5),
            ]
        };

        let mut via_helper = build();
        prune_already_known_pending_headers(&mut via_helper, &chain_db);

        let mut via_inline = build();
        retain_by_hash_inline(&mut via_inline, &chain_db);

        // PendingHeader has no PartialEq; compare the ordered hash sequence,
        // which is what both prune paths preserve.
        let helper_hashes: Vec<[u8; 32]> = via_helper.iter().map(|h| h.hash).collect();
        let inline_hashes: Vec<[u8; 32]> = via_inline.iter().map(|h| h.hash).collect();
        assert_eq!(
            helper_hashes, inline_hashes,
            "inline retain-by-hash must equal the canonical prune helper"
        );
        // And assert the expected concrete result: fork + u4 + u5 survive.
        let surviving: std::collections::HashSet<[u8; 32]> =
            via_inline.iter().map(|h| h.hash).collect();
        assert_eq!(via_inline.len(), 3, "c2 and c3 must be pruned, 3 kept");
        assert!(
            surviving.contains(&fork),
            "competing-fork header must be kept"
        );
        assert!(surviving.contains(&u4));
        assert!(surviving.contains(&u5));
        assert!(!surviving.contains(&to_arr(c2)));
        assert!(!surviving.contains(&to_arr(c3)));
    }

    /// `pending_count` after the off-lock prune must reflect the post-prune
    /// length (`entry.pending_headers.len()`), not the stale pre-prune count —
    /// otherwise `should_refill_pipeline` would hold `throttled` past RESUME.
    #[test]
    fn pending_count_post_prune_accurate() {
        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        // PENDING_PRUNE_INTERVAL known headers + 1 unknown.
        let n = PENDING_PRUNE_INTERVAL as usize;
        let mut pending: Vec<PendingHeader> = Vec::with_capacity(n + 1);
        let mut prev = Hash32::ZERO;
        for i in 0..n {
            let mut hb = [0u8; 32];
            hb[0] = (i & 0xFF) as u8;
            hb[1] = ((i >> 8) & 0xFF) as u8;
            hb[31] = 0xAB; // disambiguate from the unknown header below
            let h = Hash32::from_bytes(hb);
            chain_db
                .add_block(
                    h,
                    SlotNo(i as u64 + 1),
                    BlockNo(i as u64 + 1),
                    prev,
                    vec![i as u8],
                )
                .unwrap();
            prev = h;
            pending.push(PendingHeader {
                slot: i as u64 + 1,
                hash: hb,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            });
        }
        // One unknown header (not in ChainDB).
        pending.push(PendingHeader {
            slot: n as u64 + 1,
            hash: [0xEE; 32],
            header_cbor: vec![],
            body_size: None,
            prev_hash: None,
        });

        retain_by_hash_inline(&mut pending, &chain_db);
        let pending_count = pending.len();
        assert_eq!(pending_count, 1, "only the unknown header should remain");
        assert_eq!(pending[0].hash, [0xEE; 32]);
    }

    /// Race-safety: a header pushed concurrently (between the hash snapshot and
    /// the retain) must NOT be dropped.  retain-by-hash removes only snapshot
    /// hashes that are now stored, so a newly-pushed header is always kept.
    #[test]
    fn ticker_arm_prune_race_safe_newly_pushed_header_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut chain_db = dugite_storage::ChainDB::open(dir.path()).unwrap();

        let h1 = [0x01u8; 32]; // will become stored (known)
        let h2 = [0x02u8; 32]; // unknown, in the snapshot
        let h3 = [0x03u8; 32]; // pushed AFTER the snapshot (the race)

        // Snapshot taken when pending = [h1, h2].
        let snapshot_hashes: Vec<[u8; 32]> = vec![h1, h2];

        // Concurrently: h1 gets stored, and h3 is pushed.
        chain_db
            .add_block(
                Hash32::from_bytes(h1),
                SlotNo(1),
                BlockNo(1),
                Hash32::ZERO,
                b"x".to_vec(),
            )
            .unwrap();
        let mut pending = vec![
            PendingHeader {
                slot: 1,
                hash: h1,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 2,
                hash: h2,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 3,
                hash: h3,
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];

        // `known` is computed only from the SNAPSHOT hashes (production idiom).
        let known: std::collections::HashSet<[u8; 32]> = snapshot_hashes
            .into_iter()
            .filter(|h| chain_db.has_block(&Hash32::from_bytes(*h)))
            .collect();
        pending.retain(|h| !known.contains(&h.hash));

        let surviving: std::collections::HashSet<[u8; 32]> =
            pending.iter().map(|h| h.hash).collect();
        assert!(
            !surviving.contains(&h1),
            "stored snapshot header h1 must be dropped"
        );
        assert!(
            surviving.contains(&h2),
            "unknown snapshot header h2 must be kept"
        );
        assert!(
            surviving.contains(&h3),
            "concurrently-pushed header h3 must NOT be dropped"
        );
    }

    // ── select_headers_to_fetch ───────────────────────────────────────────────

    /// Empty pending list produces empty output.
    #[test]
    fn select_headers_empty_pending_empty_result() {
        use std::collections::HashSet;
        let pending: Vec<PendingHeader> = vec![];
        let out = select_headers_to_fetch(&pending, |_| false, &HashSet::new());
        assert!(out.is_empty());
    }

    /// All headers unknown in ChainDB and not in-flight → all selected.
    #[test]
    fn select_headers_all_unknown_all_selected() {
        use std::collections::HashSet;
        let pending = vec![
            PendingHeader {
                slot: 1,
                hash: [0x01; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 2,
                hash: [0x02; 32],
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let out = select_headers_to_fetch(&pending, |_| false, &HashSet::new());
        assert_eq!(out.len(), 2);
    }

    /// Header known in ChainDB is filtered out; unknown header is kept.
    #[test]
    fn select_headers_known_in_chain_db_filtered() {
        use std::collections::HashSet;
        let known: [[u8; 32]; 1] = [[0x01; 32]];
        let pending = vec![
            PendingHeader {
                slot: 1,
                hash: [0x01; 32], // known
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 2,
                hash: [0x02; 32], // unknown
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let out = select_headers_to_fetch(&pending, |h| known.contains(h), &HashSet::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hash, [0x02; 32]);
    }

    /// Header in fetched_hashes (in-flight) is skipped even if not in ChainDB.
    #[test]
    fn select_headers_in_flight_skipped() {
        use std::collections::HashSet;
        let fetched: HashSet<[u8; 32]> = [[0xAA; 32]].into_iter().collect();
        let pending = vec![
            PendingHeader {
                slot: 10,
                hash: [0xAA; 32], // in-flight
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
            PendingHeader {
                slot: 11,
                hash: [0xBB; 32], // available
                header_cbor: vec![],
                body_size: None,
                prev_hash: None,
            },
        ];
        let out = select_headers_to_fetch(&pending, |_| false, &fetched);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hash, [0xBB; 32]);
    }

    /// Fork blocks whose slot is below the applied tip must still be selected
    /// (regression: old slot-based filter dropped them, stalling the fork switch).
    #[test]
    fn select_headers_fork_blocks_below_applied_slot_selected() {
        use std::collections::HashSet;
        // Applied tip is at slot 200.  Fork block is at slot 150 with a new hash.
        let pending = vec![PendingHeader {
            slot: 150, // below applied tip
            hash: [0xFE; 32],
            header_cbor: vec![],
            body_size: None,
            prev_hash: None,
        }];
        let out = select_headers_to_fetch(&pending, |_| false, &HashSet::new());
        assert_eq!(
            out.len(),
            1,
            "fork block below applied slot must still be fetched"
        );
    }

    // ── extract_slot_from_wrapped_header ─────────────────────────────────────

    /// Raw (unwrapped) Shelley header: slot extracted correctly.
    #[test]
    fn extract_slot_raw_shelley_header_ok() {
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap(); // [header_body, signature]
        enc.array(3).unwrap(); // header_body: [block_number, slot, prev_hash]
        enc.u64(42).unwrap(); // block_number
        enc.u64(77777).unwrap(); // slot
        enc.bytes(&[0u8; 32]).unwrap(); // prev_hash

        // signature
        enc.bytes(&[0u8; 64]).unwrap();

        assert_eq!(extract_slot_from_wrapped_header(&buf, 0), Some(77777));
    }

    /// HFC-wrapped Shelley header: slot extracted from inner bytes.
    #[test]
    fn extract_slot_hfc_wrapped_shelley_header_ok() {
        use minicbor::Encoder;

        // Build inner bytes first
        let mut inner = Vec::new();
        let mut enc = Encoder::new(&mut inner);
        enc.array(2).unwrap(); // [header_body, signature]
        enc.array(3).unwrap(); // header_body
        enc.u64(1).unwrap(); // block_number
        enc.u64(54321).unwrap(); // slot
        enc.bytes(&[0u8; 32]).unwrap(); // prev_hash
        enc.bytes(&[0u8; 64]).unwrap(); // signature

        // Wrap as [era_tag, tag24(inner)]
        let mut outer = Vec::new();
        let mut enc2 = Encoder::new(&mut outer);
        enc2.array(2).unwrap();
        enc2.u64(1).unwrap(); // era_tag = Shelley
        enc2.tag(minicbor::data::Tag::new(24)).unwrap();
        enc2.bytes(&inner).unwrap();

        assert_eq!(extract_slot_from_wrapped_header(&outer, 0), Some(54321));
    }

    /// Empty input returns None (no panic).
    #[test]
    fn extract_slot_empty_returns_none() {
        assert_eq!(extract_slot_from_wrapped_header(&[], 0), None);
    }

    /// Random garbage returns None (no panic).
    #[test]
    fn extract_slot_garbage_returns_none() {
        assert_eq!(
            extract_slot_from_wrapped_header(&[0xDE, 0xAD, 0xBE, 0xEF], 0),
            None
        );
    }

    // ── Byron HFC-wrapped header support (issue #613) ────────────────────────
    //
    // Real on-wire bytes captured from cardano-node preprod relay
    // (3.139.241.28:3001) on 2026-05-23.  This is the Byron-genesis EBB at
    // slot 0 — len=87 — that the post-#539 strict check rejected.
    const PREPROD_GENESIS_EBB_HFC_WIRE_HEX: &str = "82008282001853d818584c\
        85015820d4b8de7a11d929a323373cbab6c1a9bdc931beffff11db111cf9d57356ee\
        19375820afc0da64183bf2664f3d4eec7238d524ba607faeeab24fc100eb861dba69\
        971b8200810081a0";

    fn hex_decode(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = Vec::with_capacity(s.len() / 2);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16).unwrap();
            let lo = (bytes[i + 1] as char).to_digit(16).unwrap();
            out.push(((hi << 4) | lo) as u8);
            i += 2;
        }
        out
    }

    /// Regression for issue #613: the preprod Byron-genesis EBB header
    /// arrives wrapped as `[0, [[0, size], tag24(bstr(ebb_header))]]`.  The
    /// extractor MUST decode it (was returning None → all 8 peers
    /// disconnected within 30 s of preprod from-genesis sync).
    #[test]
    fn extract_slot_byron_ebb_genesis_real_wire_ok() {
        let wire = hex_decode(PREPROD_GENESIS_EBB_HFC_WIRE_HEX);
        assert_eq!(wire.len(), 87, "captured len differs from observed");
        // Preprod genesis EBB is at epoch 0 → slot 0 regardless of formula.
        assert_eq!(extract_slot_from_wrapped_header(&wire, 0), Some(0));
        // Custom byron_epoch_length (preprod / preview) still gives slot 0.
        assert_eq!(extract_slot_from_wrapped_header(&wire, 21_600), Some(0));
    }

    /// Hash MUST match the Byron EBB hash formula
    /// `blake2b_256([0x82, 0x00, ebb_header])`, not `blake2b_256(wire)`.
    /// Without this, BlockFetch cannot match the header against the block we
    /// download.
    ///
    /// Cross-checked against a live preprod from-genesis sync after this fix
    /// landed: block 0 is logged as
    /// `9ad7ff320c9cf74e0f5ee78d22a85ce42bb0a487d0506bf60cfb5a91ea4497d2`,
    /// the well-known preprod Byron genesis EBB hash.
    #[test]
    fn extract_hash_byron_ebb_real_wire_matches_byron_formula() {
        use dugite_primitives::hash::blake2b_256;
        let wire = hex_decode(PREPROD_GENESIS_EBB_HFC_WIRE_HEX);

        const PREPROD_BYRON_GENESIS_EBB_HASH: &str =
            "9ad7ff320c9cf74e0f5ee78d22a85ce42bb0a487d0506bf60cfb5a91ea4497d2";

        let got = extract_hash_from_header(&wire);
        let got_hex: String = got.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got_hex, PREPROD_BYRON_GENESIS_EBB_HASH);

        // Sanity: this is NOT the hash of the wrapper bytes (regression
        // guard — easy mistake to make).
        let wrapper_hash = blake2b_256(&wire);
        let wrapper_arr: [u8; 32] = wrapper_hash.as_ref().try_into().unwrap();
        assert_ne!(got, wrapper_arr);
    }

    /// Synthetic Byron main-block header: slot = epoch*epoch_len + rel_slot.
    #[test]
    fn extract_slot_byron_main_header_synthetic() {
        use minicbor::Encoder;

        // Build a Byron main-block header:
        //   array(5) [magic, prev_hash, body_proof, consensus_data, extra_data]
        //   consensus_data = [[epoch, rel_slot], issuer_skip, [diff], block_sig_skip]
        let mut hdr = Vec::new();
        {
            let mut e = Encoder::new(&mut hdr);
            e.array(5).unwrap();
            e.u64(764824073).unwrap(); // protocol_magic (mainnet)
            e.bytes(&[0u8; 32]).unwrap(); // prev_hash
            e.bytes(&[0u8; 32]).unwrap(); // body_proof
            e.array(4).unwrap(); // consensus_data
            e.array(2).unwrap();
            e.u64(7).unwrap(); // epoch
            e.u64(123).unwrap(); // rel_slot
            e.bytes(&[0u8; 64]).unwrap(); // issuer
            e.array(1).unwrap();
            e.u64(0).unwrap(); // difficulty
            e.bytes(&[0u8; 64]).unwrap(); // block_sig
            e.map(0).unwrap(); // extra_data
        }

        // Wrap as Byron N2N: [0, [[1, size], tag24(bytes(hdr))]]
        let mut wire = Vec::new();
        {
            let mut e = Encoder::new(&mut wire);
            e.array(2).unwrap();
            e.u64(0).unwrap(); // era_id Byron
            e.array(2).unwrap();
            e.array(2).unwrap();
            e.u64(1).unwrap(); // isEbb = 1 (main)
            e.u64(hdr.len() as u64).unwrap(); // size hint
            e.tag(minicbor::data::Tag::new(24)).unwrap();
            e.bytes(&hdr).unwrap();
        }

        // Mainnet formula: 7 * 21600 + 123 = 151323
        assert_eq!(
            extract_slot_from_wrapped_header(&wire, 0),
            Some(7 * 21_600 + 123)
        );
        // Custom formula: 7 * 200 + 123 = 1523
        assert_eq!(
            extract_slot_from_wrapped_header(&wire, 200),
            Some(7 * 200 + 123)
        );
    }

    /// Reject a malformed `isEbb` discriminator (> 1).
    #[test]
    fn extract_slot_byron_invalid_isebb_returns_none() {
        use minicbor::Encoder;
        let mut wire = Vec::new();
        let mut e = Encoder::new(&mut wire);
        e.array(2).unwrap();
        e.u64(0).unwrap(); // era_id Byron
        e.array(2).unwrap();
        e.array(2).unwrap();
        e.u64(2).unwrap(); // isEbb = 2 (invalid)
        e.u64(10).unwrap();
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&[0xff; 4]).unwrap();
        assert_eq!(extract_slot_from_wrapped_header(&wire, 0), None);
    }

    /// `unwrap_hfc_header` must NOT match a Byron-wrapped header (its first
    /// element is an array, not a uint era tag).  Without this guard the
    /// Shelley+ decode path would mis-fire on Byron wires.
    #[test]
    fn unwrap_hfc_header_rejects_byron_wrap() {
        let wire = hex_decode(PREPROD_GENESIS_EBB_HFC_WIRE_HEX);
        assert!(unwrap_hfc_header(&wire).is_none());
    }
}

// ─── Bug B regression test ────────────────────────────────────────────────────
//
// Reproduces the root cause of the fork-switch stall described in:
//   docs/superpowers/specs/2026-05-16-bug-b-fork-switch-stall-fix.md
//
// Root cause: the live-tip apply path called `apply_block` (no delta) instead
// of `apply_block_with_delta`, leaving `LedgerSeq` with 0 deltas after N
// applied blocks.  When the first fork fired, `rollback_via_seq` returned
// `None` (no matching point in an empty seq), the snapshot slow-path also
// failed (no snapshot on a fresh node), and the rollback was aborted.
// The subsequent fork replay then cleared VolatileDB, causing permanent
// `StoreButDontChange` for every relay block from that point on.
//
// These tests verify the LedgerSeq invariant that Fix A + B enforce:
// every applied block MUST contribute a delta so that rollback_via_seq
// can always find the intersection point within k blocks.

// ─── Fix 1: publish_ledger_view at replay→live handover (#742) ───────────────

#[cfg(test)]
mod fix1_replay_publish_tests {
    use super::*;
    use crate::gsm::{GenesisSyncState, GsmSnapshot};
    use crate::node::ledger_view::LedgerView;
    use dugite_ledger::LedgerState;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::SlotNo;
    use std::time::Duration;

    fn make_view_swap_at(tip_slot: u64, sw: u64) -> Arc<arc_swap::ArcSwap<LedgerView>> {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.tip.point = if tip_slot == 0 {
            dugite_primitives::block::Point::Origin
        } else {
            dugite_primitives::block::Point::Specific(
                SlotNo(tip_slot),
                dugite_primitives::hash::Hash32::ZERO,
            )
        };
        state.randomness_stabilisation_window = sw;
        Arc::new(arc_swap::ArcSwap::from_pointee(LedgerView::from_state(
            &state,
        )))
    }

    /// Regression test for #742: before the fix, after a from-genesis replay the
    /// tip watch was still at 0, causing `forecast_park_or_disconnect` to park
    /// FOREVER in genesis-bulk-sync mode for any header slot > stability_window.
    ///
    /// This test seeds the watch at 0 (simulating post-replay stale state),
    /// parks a header at slot 73_000_000 (typical first CSJ dynamo slot), then
    /// simulates what `publish_ledger_view` does: advance the watch to 73_000_000
    /// (now within horizon) and update the ArcSwap view. The function must wake
    /// and return Ok within 500ms.
    #[tokio::test]
    async fn genesis_bulk_sync_park_wakes_when_tip_watch_advances() {
        // tip=0 (stale post-replay state), sw=129600 (3k/f mainnet)
        // max_for = 0 + 1 + 129600 = 129601 — slot 73M is outside.
        let view = make_view_swap_at(0, 129_600);
        // tip_rx starts at 0, simulating stale view that replay left behind.
        let (tx, mut rx) = watch::channel(0u64);
        let cancel = CancellationToken::new();
        let peer: SocketAddr = "127.0.0.1:3001".parse().unwrap();

        // GSM snapshot saying we're in Genesis Syncing (bulk sync, not CaughtUp).
        let (gsm_tx, gsm_rx) = watch::channel(GsmSnapshot {
            state: GenesisSyncState::Syncing,
            loe_slot: None,
        });
        let _ = gsm_tx; // keep alive

        let view_for_writer = Arc::clone(&view);
        // After 80ms: simulate what publish_ledger_view does — advance the watch
        // to a slot that brings the header into range.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            // Publish a fresh view with tip=73_000_000 so max_for > 73_000_000.
            let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
            state.tip.point = dugite_primitives::block::Point::Specific(
                SlotNo(73_000_000),
                dugite_primitives::hash::Hash32::ZERO,
            );
            state.randomness_stabilisation_window = 129_600;
            view_for_writer.store(Arc::new(LedgerView::from_state(&state)));
            let _ = tx.send(73_000_000u64);
        });

        let start = std::time::Instant::now();
        let result = forecast_park_or_disconnect(
            &peer,
            73_000_000,
            &view,
            &mut rx,
            &cancel,
            Some(&gsm_rx),
            73_000_000,
        )
        .await;

        assert!(
            result.is_ok(),
            "genesis bulk sync: park must wake on tip advance, got {:?}",
            result
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50) && elapsed < Duration::from_secs(2),
            "expected ~80ms park, got {elapsed:?}"
        );
    }

    /// Verify the tip watch channel semantics that Fix 1 relies on:
    /// a watch::Sender::send advances the borrow value immediately.
    #[test]
    fn tip_watch_send_advances_borrow_immediately() {
        let (tx, rx) = watch::channel(0u64);
        assert_eq!(*rx.borrow(), 0);
        tx.send(42).unwrap();
        assert_eq!(*rx.borrow(), 42);
        tx.send(73_000_000).unwrap();
        assert_eq!(*rx.borrow(), 73_000_000);
    }

    /// Confirm that LedgerView::from_state correctly captures the tip slot,
    /// so that the published view accurately reflects the post-replay ledger.
    #[test]
    fn ledger_view_captures_tip_slot_from_state() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.tip.point = dugite_primitives::block::Point::Specific(
            SlotNo(999_999),
            dugite_primitives::hash::Hash32::ZERO,
        );
        state.randomness_stabilisation_window = 100;
        let view = LedgerView::from_state(&state);
        assert_eq!(
            view.last_applied_slot,
            Some(SlotNo(999_999)),
            "view must reflect the post-replay ledger tip"
        );
    }
}

#[cfg(test)]
mod bug_b_ledger_seq_regression {
    use dugite_ledger::ledger_seq::{LedgerDelta, LedgerSeq};
    use dugite_primitives::block::Point;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};

    /// Build a minimal LedgerDelta for use in tests.
    fn make_delta(slot: u64, hash_byte: u8) -> LedgerDelta {
        LedgerDelta::new(
            SlotNo(slot),
            Hash32::from_bytes([hash_byte; 32]),
            BlockNo(slot),
        )
    }

    /// Build a `Point::Specific` for the given slot / hash byte.
    fn make_point(slot: u64, hash_byte: u8) -> Point {
        Point::Specific(SlotNo(slot), Hash32::from_bytes([hash_byte; 32]))
    }

    /// PRE-FIX SCENARIO: verify that an empty LedgerSeq returns None from
    /// `find_rollback_n`.
    ///
    /// This is the exact condition that triggered Bug B: after 11 live-applied
    /// blocks with no delta pushes, the seq was empty, `find_rollback_n`
    /// returned `None`, and the snapshot fallback also failed — causing
    /// `handle_ledger_rollback` to abort and `clear_volatile()` to fire.
    #[test]
    fn empty_ledger_seq_rollback_returns_none() {
        // Build an anchor at origin (slot 0).
        let anchor_state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let seq = LedgerSeq::with_defaults(anchor_state, 2160);

        // Simulated fork intersection at slot 46 (relay chain block 4).
        let target = make_point(46, 0xAA);

        // Without any delta pushes, find_rollback_n must return None.
        // This is the pre-fix failure mode that caused the cascade.
        assert_eq!(
            seq.find_rollback_n(&target),
            None,
            "empty seq must return None — this is the bug condition Fix A prevents"
        );
    }

    /// POST-FIX SCENARIO: after pushing deltas for each applied block,
    /// `find_rollback_n` returns `Some(n)` for the intersection point —
    /// rollback succeeds and no cascade follows.
    #[test]
    fn populated_ledger_seq_rollback_succeeds() {
        let anchor_state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let mut seq = LedgerSeq::with_defaults(anchor_state, 2160);

        // Simulate 5 relay blocks (slots 17..46) each pushing a delta.
        // This is what Fix A ensures happens for every live-applied block.
        let relay_slots: &[(u64, u8)] = &[
            (17, 0x01),
            (24, 0x02),
            (31, 0x03),
            (38, 0x04),
            (46, 0xAA), // intersection slot — the rollback target
        ];
        for &(slot, hash_byte) in relay_slots {
            seq.push(make_delta(slot, hash_byte));
        }

        // 6 self-forged blocks on top (slots 50..79).
        for (i, slot) in (50u64..80).step_by(6).enumerate() {
            seq.push(make_delta(slot, 0xF0 + i as u8));
        }

        // The intersection point (relay slot 46) is now in the volatile window.
        let target = make_point(46, 0xAA);
        let n = seq.find_rollback_n(&target);
        assert!(
            n.is_some(),
            "populated seq must find the intersection — Fix A makes this true"
        );
        // We pushed 5 relay + some self-forged blocks; intersection is at index 4
        // so n = (total_deltas - 4 - 1) = deltas after intersection.
        assert!(
            n.unwrap() > 0,
            "rollback should unwind at least the self-forged blocks"
        );
    }

    /// POST-FIX SCENARIO: after a fork switch + replay, deltas for the fork
    /// blocks are also in the seq (Fix B), so the NEXT fork can also roll back.
    #[test]
    fn fork_replay_deltas_enable_subsequent_rollback() {
        let anchor_state = dugite_ledger::LedgerState::new(
            dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults(),
        );
        let mut seq = LedgerSeq::with_defaults(anchor_state, 2160);

        // Phase 1: push 3 common-prefix blocks.
        seq.push(make_delta(10, 0x10));
        seq.push(make_delta(20, 0x20));
        seq.push(make_delta(30, 0x30)); // intersection for first fork

        // Phase 2: rollback to slot 30 (simulating first fork).
        let fork1_target = make_point(30, 0x30);
        let n1 = seq
            .find_rollback_n(&fork1_target)
            .expect("must find slot 30");
        seq.rollback(n1);
        assert_eq!(seq.len(), 3, "after rollback to slot 30, 3 deltas remain");

        // Phase 3: replay fork1 blocks — Fix B ensures these are pushed.
        seq.push(make_delta(35, 0x31));
        seq.push(make_delta(40, 0x32));
        seq.push(make_delta(45, 0x33)); // fork1 tip

        // Phase 4: a second fork fires with intersection at slot 35.
        // Without Fix B, seq would only have 3 deltas (pre-fork) and slot 35
        // would NOT be in it.  With Fix B, slot 35 IS present.
        let fork2_target = make_point(35, 0x31);
        let n2 = seq.find_rollback_n(&fork2_target);
        assert!(
            n2.is_some(),
            "Fix B: fork replay deltas must be in seq so second fork can roll back"
        );
        assert_eq!(
            n2.unwrap(),
            2,
            "two deltas (slots 40 and 45) should be rolled back"
        );
    }
}
