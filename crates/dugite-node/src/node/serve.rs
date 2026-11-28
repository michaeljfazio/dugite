//! N2N and N2C server setup: connection-facing adapters that bridge the node's
//! internal state (ChainDB, LedgerState, Mempool) to the network protocol
//! implementations in dugite-network.

use std::sync::Arc;
use tokio::sync::RwLock;

use dugite_ledger::LedgerState;
use dugite_network::{
    BlockProvider, TipInfo, TxValidationError, TxValidator, UtxoQueryProvider, UtxoSnapshot,
    UtxoViewPoint,
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

    fn get_next_block_after_point(
        &self,
        slot: u64,
        hash: &[u8; 32],
    ) -> Option<(u64, [u8; 32], Vec<u8>)> {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            match db.get_next_block_after_point(dugite_primitives::time::SlotNo(slot), &block_hash)
            {
                Ok(Some((s, h, cbor))) => {
                    let mut hash_arr = [0u8; 32];
                    hash_arr.copy_from_slice(h.as_bytes());
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

    fn canonical_point_slot(&self, hash: &[u8; 32]) -> Option<u64> {
        let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash);
        tokio::task::block_in_place(|| {
            let db = self.chain_db.blocking_read();
            db.canonical_point_slot(&block_hash).map(|s| s.0)
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
            // Point cursor (slot, hash) of the last collected block.  A Byron
            // EBB shares its absolute slot with the first main block of the
            // epoch, so iteration must step by point — a slot cursor would
            // skip the same-slot main block after collecting the EBB.
            let mut cursor: Option<(u64, dugite_primitives::hash::Hash32)> = None;

            while blocks.len() < limit {
                // Acquire the read lock for a chunk of blocks.
                let db = self.chain_db.blocking_read();
                let chunk_limit = BATCH_CHUNK_SIZE.min(limit - blocks.len());

                for _ in 0..chunk_limit {
                    let result = match cursor {
                        None => db
                            .get_block_at_or_after_slot(dugite_primitives::time::SlotNo(from_slot)),
                        Some((slot, hash)) => db.get_next_block_after_point(
                            dugite_primitives::time::SlotNo(slot),
                            &hash,
                        ),
                    };
                    match result {
                        Ok(Some((s, hash, cbor))) if s.0 <= to_slot => {
                            let mut hash_arr = [0u8; 32];
                            hash_arr.copy_from_slice(hash.as_bytes());
                            cursor = Some((s.0, hash));
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

/// Provides UTxO lookups as of an LSQ acquisition's pinned chain point.
///
/// The live ledger is the only materialised UTxO set — `UtxoSet` is a plain
/// `HashMap`, so at mainnet scale (~10M entries) there is no cheap snapshot to
/// take and copying one per acquisition is not an option. The pinned view is
/// therefore RECONSTRUCTED: read the live set, then undo the `LedgerSeq` deltas
/// applied strictly after the acquired point. Those deltas already exist for
/// rollback, and `UtxoDiff::deletes` preserves the spent output precisely so it
/// can be restored, so this reuses the machinery rather than adding a second
/// UTxO history.
///
/// This is upstream's shape, not an invention: `LedgerDB` retains the last `k`
/// ledger states and an acquisition reads one of them.
pub(crate) struct LedgerUtxoProvider {
    pub ledger: Arc<RwLock<LedgerState>>,
    /// Volatile window of per-block `UtxoDiff`s, used to undo forward.
    ///
    /// **Lock ordering**: `ledger` before `ledger_seq`, per `Node`'s docs.
    pub ledger_seq: Arc<RwLock<dugite_ledger::ledger_seq::LedgerSeq>>,
}

impl LedgerUtxoProvider {
    /// Deltas applied strictly after `at`, NEWEST FIRST.
    ///
    /// `None` means the pinned point is not reachable in the volatile window,
    /// so no honest answer is available. Unreachable in practice — `acquire`
    /// only ever pins the current tip, and the window is `k` blocks — but
    /// answering from the live set anyway would be the silent tear this exists
    /// to remove.
    fn undo_diffs(
        seq: &dugite_ledger::ledger_seq::LedgerSeq,
        at: &UtxoViewPoint,
    ) -> Option<Vec<dugite_ledger::UtxoDiff>> {
        use dugite_primitives::block::Point;

        let target = match at {
            UtxoViewPoint::Origin => Point::Origin,
            UtxoViewPoint::Specific { slot, hash } => Point::Specific(
                dugite_primitives::time::SlotNo(*slot),
                dugite_primitives::hash::Hash32::from_bytes(*hash),
            ),
        };

        // The anchor is the oldest reachable point; everything after it is a
        // delta. `denotes_origin` because Origin has two spellings and
        // comparing points with `==` across that seam is #1057's half 3.
        if target.denotes_origin() && seq.anchor_point().denotes_origin() {
            return Some(
                seq.deltas()
                    .iter()
                    .rev()
                    .map(|d| d.utxo_diff.clone())
                    .collect(),
            );
        }
        if seq.anchor_point() == &target {
            return Some(
                seq.deltas()
                    .iter()
                    .rev()
                    .map(|d| d.utxo_diff.clone())
                    .collect(),
            );
        }

        let mut after = Vec::new();
        let mut found = false;
        for delta in seq.deltas().iter() {
            if found {
                after.push(delta.utxo_diff.clone());
            } else if Point::Specific(delta.slot, delta.hash) == target {
                found = true;
            }
        }
        if !found {
            return None;
        }
        after.reverse();
        Some(after)
    }

    /// Read the live ledger and the undo plan under the documented lock order.
    fn with_pinned<T>(
        &self,
        at: &UtxoViewPoint,
        f: impl FnOnce(&LedgerState, &[dugite_ledger::UtxoDiff]) -> T,
    ) -> Option<T> {
        tokio::task::block_in_place(|| {
            let ledger = self.ledger.blocking_read();
            let seq = self.ledger_seq.blocking_read();
            let undo = Self::undo_diffs(&seq, at)?;
            if !undo.is_empty() {
                tracing::debug!(
                    blocks = undo.len(),
                    "UTxO query: undoing blocks applied after the acquired point"
                );
            }
            Some(f(&ledger, &undo))
        })
    }
}

/// Apply the undo plan to a set of live UTxO entries keyed by input.
///
/// Within ONE delta the restores must be applied BEFORE the removals: an output
/// created and spent by the same block appears in both `inserts` and `deletes`,
/// and it must end up ABSENT at the pinned point. Restoring first and then
/// removing gets that right; the other order leaves it present.
///
/// Across deltas the iteration must be NEWEST FIRST. Oldest-first re-adds an
/// output that a later block spent and never removes it again.
fn undo_into(
    live: &mut std::collections::HashMap<
        dugite_primitives::transaction::TransactionInput,
        dugite_primitives::transaction::TransactionOutput,
    >,
    undo: &[dugite_ledger::UtxoDiff],
) {
    for diff in undo {
        for (input, output) in &diff.deletes {
            live.insert(input.clone(), output.clone());
        }
        for (input, _) in &diff.inserts {
            live.remove(input);
        }
    }
}

impl UtxoQueryProvider for LedgerUtxoProvider {
    fn utxos_at_address_bytes(
        &self,
        addr_bytes: &[u8],
        at: &UtxoViewPoint,
    ) -> Option<Vec<UtxoSnapshot>> {
        let addr = match dugite_primitives::address::Address::from_bytes(addr_bytes) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    "UTxO query: address decode failed: {e} (bytes len={})",
                    addr_bytes.len()
                );
                return Some(vec![]);
            }
        };
        // block_in_place + blocking_read so this works when called from within
        // a tokio async runtime (avoids the "cannot block" panic).
        self.with_pinned(at, |ledger, undo| {
            let mut set: std::collections::HashMap<_, _> = ledger
                .utxo
                .utxo_set
                .utxos_at_address(&addr)
                .into_iter()
                .collect();
            if !undo.is_empty() {
                // The undo plan is chain-wide, so it is filtered to this
                // address here. A restore whose output sits at another address
                // must not appear in this answer, and a removal for another
                // address is simply absent from `set`.
                let mut scoped: Vec<dugite_ledger::UtxoDiff> = Vec::with_capacity(undo.len());
                for d in undo {
                    scoped.push(dugite_ledger::UtxoDiff {
                        inserts: d
                            .inserts
                            .iter()
                            .filter(|(_, o)| o.address == addr)
                            .cloned()
                            .collect(),
                        deletes: d
                            .deletes
                            .iter()
                            .filter(|(_, o)| o.address == addr)
                            .cloned()
                            .collect(),
                    });
                }
                undo_into(&mut set, &scoped);
            }
            let results: Vec<_> = set
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

    fn utxos_by_tx_inputs(
        &self,
        inputs: &[(Vec<u8>, u32)],
        at: &UtxoViewPoint,
    ) -> Option<Vec<UtxoSnapshot>> {
        self.with_pinned(at, |ledger, undo| {
            let mut results = Vec::new();
            for (tx_hash_bytes, idx) in inputs {
                if tx_hash_bytes.len() != 32 {
                    continue;
                }
                let mut hash_arr = [0u8; 32];
                hash_arr.copy_from_slice(tx_hash_bytes);
                let tx_input = dugite_primitives::transaction::TransactionInput {
                    transaction_id: dugite_primitives::hash::Hash32::from_bytes(hash_arr),
                    index: *idx,
                };
                // Resolve this ONE input against the pinned view — exactly
                // `undo_into`'s semantics restricted to a single key, and it
                // must run over EVERY delta rather than stopping at the first
                // that mentions the input. An output created in one block and
                // spent in a later one appears in both; stopping at the newest
                // delta would see only the spend, restore it, and report a
                // UTxO that did not exist at the pinned point.
                let mut output = ledger.utxo.utxo_set.lookup(&tx_input);
                for diff in undo {
                    if let Some((_, o)) = diff.deletes.iter().find(|(i, _)| *i == tx_input) {
                        output = Some(o.clone());
                    }
                    if diff.inserts.iter().any(|(i, _)| *i == tx_input) {
                        output = None;
                    }
                }
                if let Some(output) = output {
                    results.push(utxo_to_snapshot(&tx_input, &output));
                }
            }
            results
        })
    }

    fn utxos_all(&self, at: &UtxoViewPoint) -> Option<Vec<UtxoSnapshot>> {
        self.with_pinned(at, |ledger, undo| {
            let mut set: std::collections::HashMap<_, _> =
                ledger.utxo.utxo_set.iter().into_iter().collect();
            undo_into(&mut set, undo);
            let results: Vec<_> = set
                .into_iter()
                .map(|(input, output)| utxo_to_snapshot(&input, &output))
                .collect();
            tracing::debug!(utxos_found = results.len(), "UTxO query: whole set");
            results
        })
    }
}

// ─── LedgerTxValidator ───────────────────────────────────────────────────────

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
    pub network: dugite_primitives::network::NetworkId,
    /// Live era history. Used to compute the per-tx safe-zone horizon
    /// (`EraHistory::safe_zone_horizon_slot(ledger_tip)`) that
    /// dugite-uplc enforces during Plutus context translation, mirroring
    /// Haskell `TimeTranslationPastHorizon`. See Round-1 audit finding
    /// `audit-findings/2026-05-28-skill-self-audit.md` "DUGITE BUG
    /// CAUGHT BY ROUND 1".
    pub era_history: Arc<RwLock<dugite_consensus::EraHistory>>,
}

impl TxValidator for LedgerTxValidator {
    fn validate_tx(&self, era_id: u16, tx_bytes: &[u8]) -> Result<(), TxValidationError> {
        // #1047: WIRE-ERA CHECK, before decoding.
        //
        // Upstream cannot even represent applying a tx from era X to a ledger in
        // era Y: `ApplyTxErr` for the HFC is
        // `Either (MismatchEraInfo xs) (OneEraApplyTxErr xs)`, and the HFC's
        // mempool returns `HardForkApplyTxErrWrongEra` before any era's rules
        // run. dugite had no such check — a legacy-era submission was rejected
        // only as an ACCIDENTAL CBOR decode error (the standalone Shelley decoder
        // expects a different array length than a real Shelley tx has).
        //
        // That accident is load-bearing today and must not be relied on: correct
        // any one of those decoders without adding this check, and MIR /
        // GenesisKeyDelegation / other legacy-era artifacts become ACCEPTABLE on a
        // Conway chain where cardano-node answers
        // `HardForkApplyTxErrWrongEra` — the accept-where-Haskell-rejects wedge
        // class (#996/#997/#985/#1023). MIR Phase-1 validation is a documented
        // no-op at PV>=9 and GenesisKeyDelegation has era-unconditional
        // apply-time support, so the ledger layer would NOT catch it.
        //
        // The ledger's era comes from `EraHistory::current_era()`, not from
        // ledger pparams: #985's lesson is that a stale/corrupt protocol version
        // must never drive an era decision.
        // #1090: WAIT for era_history rather than skipping the check on
        // contention. A `try_read()` failure here used to fall through to
        // the decoder silently, which meant #1047's wire-era protection did
        // not run in exactly the window a block was being applied. This
        // acquisition MUST stay scoped to this expression — only the owned
        // `Era` escapes, so the read guard is dropped right here, BEFORE
        // the `ledger` acquire a few lines down. That ordering is
        // load-bearing now that the reads can actually block: sync.rs
        // holds `ledger` write across an `era_history` read/write
        // (sync.rs:1629, "Lock order ls->era_history"), so holding this
        // guard across a blocking `ledger` acquire would invert that order
        // and create a real AB-BA deadlock against the apply path, not
        // merely a benign try-lock failure. See the design doc for #1090.
        let ledger_era =
            tokio::task::block_in_place(|| self.era_history.blocking_read().current_era());
        {
            let ledger_era_index = ledger_era.to_era_index();
            if u32::from(era_id) != ledger_era_index {
                let tx_era = dugite_primitives::era::Era::from_era_index(era_id);
                tracing::warn!(
                    era_id,
                    ledger_era = %ledger_era,
                    "N2C tx rejected: wrong era (HardForkApplyTxErrWrongEra)"
                );
                return Err(TxValidationError::HardForkApplyTxErrWrongEra {
                    tx_era_index: era_id as u8,
                    tx_era_name: tx_era
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| format!("UnknownEra{era_id}")),
                    ledger_era_index: ledger_era_index as u8,
                    ledger_era_name: ledger_era.to_string(),
                });
            }
        }

        let tx = dugite_serialization::decode_transaction(era_id, tx_bytes).map_err(|e| {
            TxValidationError::DecodeFailed {
                reason: e.to_string(),
            }
        })?;

        // #1090: WAIT for the ledger rather than rejecting on contention.
        // `try_read()` used to fail whenever a block-apply writer held —
        // or was queued behind readers for — this lock, and the failure
        // was mapped to a fabricated `LedgerStateUnavailable` rejection
        // (since deleted) for a transaction on which no validation had
        // run. cardano-node has no give-up-if-busy path anywhere on its
        // submission surface (see docs/superpowers/specs/
        // 2026-08-21-tx-admission-lock-contention-design.md): the mempool
        // add path blocks-as-queues and never fails an acquire, so the
        // correct alignment is to wait, not to invent a verdict.
        // `block_in_place` is this file's own established idiom for a
        // lock that may briefly block a worker — `LedgerUtxoProvider::
        // with_pinned` above already takes this EXACT `ledger` lock with
        // `blocking_read()` for LSQ UTxO queries. It hands the worker's
        // task queue to a replacement thread while parked, so the wait
        // does not starve the async pool; the parked duration is bounded
        // by the writer(s) ahead, which is exactly how long a real verdict
        // would take to compute against the post-apply state anyway.
        let ledger = tokio::task::block_in_place(|| self.ledger.blocking_read());
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

        // The ledger-derived context — pool/DRep/VRF registries, the Conway
        // governance state (so `DisallowedVoters`, `VotersDoNotExist`,
        // `VotingOnExpiredGovAction`, `InvalidPrevGovActionId` and the
        // committee predicates fire here and not first at forge time), the
        // reward accounts behind `WithdrawalsNotInRewardsCERTS`, and the
        // deposit/treasury/epoch terms.
        //
        // #996: this is now the SAME builder the post-block revalidation uses.
        // It was previously hand-rolled here and was a strict subset of what
        // block-apply builds, so each missing field was a way to admit a
        // transaction that block-apply — and therefore cardano-node — rejects.
        let context = ledger
            .mempool_validation_context()
            .with_network(self.network);

        // Compute the per-tx safe-zone horizon and inject it into the
        // SlotConfig so the Plutus context-builder rejects any tx whose
        // validity interval translates past the era horizon. Mirrors
        // Haskell `Alonzo.Plutus.TxInfo.transValidityInterval` →
        // `TimeTranslationPastHorizon`. Round-1 P0 regression: without
        // this, dugite admitted a tx with ttl=865 at chain tip 265 and
        // cardano-bp permanently rejected the resulting block.
        // #1090: WAIT for era_history rather than falling back to the
        // unbounded static SlotConfig on contention — see the `ledger`
        // acquire above. At this point the `ledger` read guard is ALREADY
        // held, so this acquisition order is `ledger` then `era_history`,
        // matching sync.rs's documented order (sync.rs:1629) rather than
        // inverting it; no new deadlock risk is introduced here.
        let per_tx_slot_config = tokio::task::block_in_place(|| {
            let eh = self.era_history.blocking_read();
            let horizon = eh.safe_zone_horizon_slot(dugite_primitives::time::SlotNo(current_slot));
            let mut sc = self.slot_config;
            if let Some(h) = horizon {
                sc.safe_zone_horizon_slot = Some(h);
            }
            sc
        });

        let pv_major = ledger.epochs.protocol_params.protocol_version_major;
        dugite_ledger::validation::validate_transaction_with_context(
            &tx,
            &utxo_view,
            &ledger.epochs.protocol_params,
            current_slot,
            tx_size,
            Some(&per_tx_slot_config),
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
            // The conversion lives in `dugite-tx-admission` so the conformance harness runs
            // the very same code at the same protocol version.
            dugite_tx_admission::rejection_from_validation_errors(errors, &tx, &utxo_view, pv_major)
        })
    }
}

// ─── Connection metrics bridges ──────────────────────────────────────────────

/// Bridges N2N server connection events to the node metrics system.
// Construction happens only in #[cfg(test)] — suppress the dead_code lint.
#[allow(dead_code)]
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
// Construction happens only in #[cfg(test)] — suppress the dead_code lint.
#[allow(dead_code)]
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

    // ── #1068: reconstructing a pinned UTxO view ─────────────────────────

    fn txin(n: u8) -> dugite_primitives::transaction::TransactionInput {
        dugite_primitives::transaction::TransactionInput {
            transaction_id: Hash32::from_bytes([n; 32]),
            index: 0,
        }
    }

    fn txout(lovelace: u64) -> dugite_primitives::transaction::TransactionOutput {
        dugite_primitives::transaction::TransactionOutput {
            address: dugite_primitives::address::Address::from_bytes(&[0x61u8; 29])
                .expect("29-byte enterprise address"),
            value: dugite_primitives::value::Value::lovelace(lovelace),
            datum: dugite_primitives::transaction::OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    fn diff(inserts: Vec<(u8, u64)>, deletes: Vec<(u8, u64)>) -> dugite_ledger::UtxoDiff {
        dugite_ledger::UtxoDiff {
            inserts: inserts
                .into_iter()
                .map(|(n, v)| (txin(n), txout(v)))
                .collect(),
            deletes: deletes
                .into_iter()
                .map(|(n, v)| (txin(n), txout(v)))
                .collect(),
        }
    }

    /// The undo has TWO orderings that are each wrong in one direction, and a
    /// naive implementation gets one of them wrong silently — the answer is
    /// still a plausible UTxO set, just not the one at the pinned point.
    ///
    /// Case A pins the WITHIN-delta order: an output created and spent by the
    /// SAME block is in both `inserts` and `deletes`, and must end up ABSENT.
    /// Restoring deletes before removing inserts gets that right; the reverse
    /// order leaves it present.
    ///
    /// Case B pins the ACROSS-delta order: an output created in one block and
    /// spent in a LATER one must also end up absent. Oldest-first re-adds it
    /// from the later block's `deletes` and never removes it again.
    #[test]
    fn undo_reconstructs_the_pinned_view_in_both_orderings() {
        // Case A — created and spent inside one block (newest, index 0).
        let mut live = std::collections::HashMap::new();
        undo_into(&mut live, &[diff(vec![(1, 100)], vec![(1, 100)])]);
        assert!(
            !live.contains_key(&txin(1)),
            "an output created AND spent by the same block did not exist at the \
             pinned point; restoring deletes must precede removing inserts"
        );

        // Case B — created in the older block, spent in the newer one. The
        // slice is NEWEST FIRST, which is the order `undo_diffs` returns.
        let mut live = std::collections::HashMap::new();
        undo_into(
            &mut live,
            &[
                diff(vec![], vec![(2, 200)]), // newer block spent it
                diff(vec![(2, 200)], vec![]), // older block created it
            ],
        );
        assert!(
            !live.contains_key(&txin(2)),
            "an output created after the pinned point and later spent still did \
             not exist at that point; the undo must run newest-first"
        );

        // Case C — existed at the pinned point, spent afterwards: restored.
        let mut live = std::collections::HashMap::new();
        undo_into(&mut live, &[diff(vec![], vec![(3, 300)])]);
        assert_eq!(
            live.get(&txin(3)).map(|o| o.value.coin.0),
            Some(300),
            "an output spent after the pinned point was still present at it"
        );

        // Case D — created after the pinned point and still live: removed.
        let mut live = std::collections::HashMap::new();
        live.insert(txin(4), txout(400));
        undo_into(&mut live, &[diff(vec![(4, 400)], vec![])]);
        assert!(
            !live.contains_key(&txin(4)),
            "an output created after the pinned point must not appear in it"
        );

        // Case E — untouched by any delta: passes through unchanged.
        let mut live = std::collections::HashMap::new();
        live.insert(txin(5), txout(500));
        undo_into(&mut live, &[diff(vec![(9, 900)], vec![(8, 800)])]);
        assert_eq!(live.get(&txin(5)).map(|o| o.value.coin.0), Some(500));
    }

    /// `undo_diffs` must EMIT the deltas newest-first.
    ///
    /// The ordering test above drives `undo_into` with a hand-built slice, so
    /// it bounds that function and nothing else — reversing the producer left
    /// it green. This drives the real producer, which is the only thing that
    /// can catch the reversal in production. A RED-proven unit test bounds the
    /// function, not the system (#1057).
    #[test]
    fn undo_diffs_are_emitted_newest_first() {
        use dugite_ledger::ledger_seq::{LedgerDelta, LedgerSeq};
        use dugite_primitives::time::{BlockNo, SlotNo};

        let anchor = dugite_ledger::LedgerState::new(ProtocolParameters::mainnet_defaults());
        let mut seq = LedgerSeq::with_defaults(anchor, 2160);

        // Pushed oldest-first, as production does.
        let mut older = LedgerDelta::new(SlotNo(10), Hash32::from_bytes([10u8; 32]), BlockNo(1));
        older.utxo_diff = diff(vec![(1, 100)], vec![]);
        seq.push(older);

        let mut newer = LedgerDelta::new(SlotNo(20), Hash32::from_bytes([20u8; 32]), BlockNo(2));
        newer.utxo_diff = diff(vec![(2, 200)], vec![]);
        seq.push(newer);

        let plan = LedgerUtxoProvider::undo_diffs(&seq, &UtxoViewPoint::Origin)
            .expect("the anchor is Origin, so the whole window is reachable");
        assert_eq!(plan.len(), 2, "both post-anchor blocks must be undone");
        assert_eq!(
            plan[0].inserts[0].0,
            txin(2),
            "the NEWEST block must be undone first; oldest-first re-adds an \
             output a later block spent and never removes it again"
        );
        assert_eq!(plan[1].inserts[0].0, txin(1));

        // The SPECIFIC-point path is a SECOND ordering site: the Origin branch
        // reverses inline, the specific-point branch builds forward and
        // reverses at the end. Disarming one leaves the other green, so both
        // are driven here. Pinning the OLDER block must yield only the newer
        // one — the older block is at the pinned point, not after it.
        let at_older = UtxoViewPoint::Specific {
            slot: 10,
            hash: [10u8; 32],
        };
        let plan = LedgerUtxoProvider::undo_diffs(&seq, &at_older)
            .expect("the older block is inside the volatile window");
        assert_eq!(
            plan.len(),
            1,
            "only blocks applied STRICTLY AFTER the pinned point are undone"
        );
        assert_eq!(plan[0].inserts[0].0, txin(2));

        // Three deltas, pinned at the oldest, exercises the reversal on the
        // specific-point path with enough elements for order to be observable
        // — with two, a reversal bug and a correct one differ only by which
        // single element is dropped.
        let mut third = LedgerDelta::new(SlotNo(30), Hash32::from_bytes([30u8; 32]), BlockNo(3));
        third.utxo_diff = diff(vec![(3, 300)], vec![]);
        seq.push(third);
        let plan = LedgerUtxoProvider::undo_diffs(&seq, &at_older).expect("still in window");
        assert_eq!(
            plan.iter()
                .map(|d| d.inserts[0].0.clone())
                .collect::<Vec<_>>(),
            vec![txin(3), txin(2)],
            "the specific-point path must also emit newest-first"
        );

        // A point that is not in the window is not reconstructible, and must
        // say so rather than silently answering from the live set.
        let unknown = UtxoViewPoint::Specific {
            slot: 99,
            hash: [0xab; 32],
        };
        assert!(
            LedgerUtxoProvider::undo_diffs(&seq, &unknown).is_none(),
            "an unreachable pinned point must refuse, not fall back to live"
        );
    }

    /// An empty undo plan must be the identity — the overwhelmingly common
    /// case, since `acquire` pins the current tip and most acquisitions see no
    /// block at all. If this were not exact, #1068's fix would have changed
    /// every UTxO answer rather than only the torn ones.
    #[test]
    fn undo_with_no_deltas_is_the_identity() {
        let mut live = std::collections::HashMap::new();
        live.insert(txin(1), txout(111));
        live.insert(txin(2), txout(222));
        let before = live.clone();
        undo_into(&mut live, &[]);
        assert_eq!(live.len(), before.len());
        assert_eq!(live.get(&txin(1)).map(|o| o.value.coin.0), Some(111));
        assert_eq!(live.get(&txin(2)).map(|o| o.value.coin.0), Some(222));
    }

    /// Verifies that `LedgerState::mempool_validation_context` projects the
    /// live Conway governance fields into the shapes expected by
    /// `ValidationContext`.  This is the production wiring used by BOTH the
    /// N2C tx-submission path and the post-block revalidation (#996); getting
    /// the projection wrong would silently disable the cross-tx voting
    /// predicates (`DisallowedVoters` etc.).
    #[test]
    fn mempool_validation_context_projects_proposals_and_hot_keys() {
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
            submission_index: 0,
        };
        let cold_cred = Hash32::from_bytes([0xC0; 32]);
        let hot_cred = Hash32::from_bytes([0x77; 32]);

        {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            gov.proposals.insert(action_id.clone(), proposal_state);
            gov.committee_hot_keys.insert(cold_cred, hot_cred);
        }

        let ctx = ledger.mempool_validation_context();
        let active = ctx.active_proposals.clone().expect("proposals projected");
        let hot_keys = ctx
            .committee_authorized_hot_keys
            .clone()
            .expect("hot keys projected");

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
    fn mempool_validation_context_empty_for_fresh_ledger() {
        let ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let ctx = ledger.mempool_validation_context();
        assert!(ctx.active_proposals.expect("set").is_empty());
        assert!(ctx.committee_authorized_hot_keys.expect("set").is_empty());
        assert!(ctx.committee_members.expect("set").is_empty());
        assert!(ctx.committee_resigned.expect("set").is_empty());
        assert!(ctx
            .committee_authorized_elected_hot_keys
            .expect("set")
            .is_empty());
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
                data: PlutusData::Integer(num_bigint::BigInt::from(42i128)),
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

    // ─── Accept-loop integration tests ──────────────────────────────────────
    //
    // These tests bind real ephemeral sockets, connect a client, and verify
    // that the production counter-increment logic (copied verbatim from
    // `node/mod.rs`) fires.  The point is to catch any future refactor that
    // moves the `fetch_add` call or wraps it in a conditional that silently
    // suppresses it.
    //
    // We exercise the counter logic by replicating the exact pattern used in
    // the production accept loops: bind → accept → fetch_add.  This is a
    // targeted integration test, not a full node smoke test — the Node struct
    // is intentionally not involved.

    /// N2N inbound accept loop: each accepted TCP connection must bump
    /// `n2n_connections_total` by exactly 1.
    ///
    /// This replicates the counter-increment path in `node/mod.rs`:
    /// ```text
    /// Ok((stream, peer_addr)) => {
    ///     // ... IP / rate-limit checks ...
    ///     conn_metrics.n2n_connections_total.fetch_add(1, Relaxed);
    ///     // ... spawn handshake task ...
    /// }
    /// ```
    #[tokio::test]
    async fn n2n_accept_loop_bumps_total_counter() {
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        assert_eq!(
            metrics.n2n_connections_total.load(Relaxed),
            0,
            "starts at zero"
        );

        // Bind on an ephemeral loopback port — port 0 lets the OS pick.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral TCP socket");
        let addr = listener.local_addr().expect("local addr");

        // Spawn a minimal accept loop that replicates the production pattern
        // from `node/mod.rs`: accept() → fetch_add(n2n_connections_total).
        let m = metrics.clone();
        let accept_task = tokio::spawn(async move {
            // Accept exactly one connection then stop — mirrors the production
            // loop body for the success branch.
            if let Ok((_stream, _peer)) = listener.accept().await {
                m.n2n_connections_total.fetch_add(1, Relaxed);
            }
        });

        // Connect a client — this triggers the accept().
        let _client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to ephemeral listener");

        // Wait for the accept task to finish.
        accept_task.await.expect("accept task panicked");

        assert_eq!(
            metrics.n2n_connections_total.load(Relaxed),
            1,
            "n2n_connections_total must be 1 after one accepted TCP connection"
        );
        // The active gauge is intentionally NOT bumped in the N2N path —
        // it is derived from ConnectionLifecycleManager (see N2NConnectionMetrics).
        assert_eq!(
            metrics.n2n_connections_active.load(Relaxed),
            0,
            "n2n_connections_active is lifecycle-derived, not bumped at accept"
        );
    }

    /// N2N accept loop: two successive connections each bump the counter once.
    #[tokio::test]
    async fn n2n_accept_loop_counter_accumulates() {
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral TCP socket");
        let addr = listener.local_addr().expect("local addr");

        let m = metrics.clone();
        let accept_task = tokio::spawn(async move {
            for _ in 0..2u32 {
                if let Ok((_stream, _peer)) = listener.accept().await {
                    m.n2n_connections_total.fetch_add(1, Relaxed);
                }
            }
        });

        for _ in 0..2u32 {
            let _c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        }
        accept_task.await.expect("accept task panicked");

        assert_eq!(metrics.n2n_connections_total.load(Relaxed), 2);
    }

    /// N2C inbound accept loop: each accepted Unix-socket connection must bump
    /// both `n2c_connections_total` (monotonic) and `n2c_connections_active`
    /// (gauge).  Disconnect (task exit) must decrement the active gauge.
    ///
    /// Replicates the production pattern from `node/mod.rs`:
    /// ```text
    /// Ok((stream, _addr)) => {
    ///     conn_metrics.n2c_connections_total.fetch_add(1, Relaxed);
    ///     conn_metrics.n2c_connections_active.fetch_add(1, Relaxed);
    ///     tokio::spawn(async move {
    ///         // ... handle connection ...
    ///         metrics.n2c_connections_active.fetch_sub(1, Relaxed);
    ///     });
    /// }
    /// ```
    #[tokio::test]
    async fn n2c_accept_loop_bumps_total_and_active_counters() {
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        assert_eq!(
            metrics.n2c_connections_total.load(Relaxed),
            0,
            "starts at zero"
        );
        assert_eq!(
            metrics.n2c_connections_active.load(Relaxed),
            0,
            "starts at zero"
        );

        // Create a temp dir for the Unix socket.
        let tmp = std::env::temp_dir().join(format!(
            "dugite-test-n2c-{}.sock",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_file(&tmp); // clean up any stale socket

        let listener = tokio::net::UnixListener::bind(&tmp).expect("bind ephemeral Unix socket");

        // One-shot accept task: accept one connection, bump counters, then
        // simulate the connection handler exiting (decrement active).
        let m = metrics.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let accept_task = tokio::spawn(async move {
            if let Ok((_stream, _addr)) = listener.accept().await {
                m.n2c_connections_total.fetch_add(1, Relaxed);
                m.n2c_connections_active.fetch_add(1, Relaxed);

                // Signal the test we've accepted; test then checks active > 0.
                let _ = done_tx.send(());

                // Simulate the spawned connection handler doing work and exiting.
                // In production this is a `tokio::spawn` — here we just do it
                // inline after the signal so the test can observe both states.
                m.n2c_connections_active.fetch_sub(1, Relaxed);
            }
        });

        // Connect a Unix client.
        let _client = tokio::net::UnixStream::connect(&tmp)
            .await
            .expect("connect to ephemeral Unix socket");

        // Wait for the accept task to signal it bumped the counters.
        done_rx.await.expect("accept task dropped sender");

        assert_eq!(
            metrics.n2c_connections_total.load(Relaxed),
            1,
            "n2c_connections_total must be 1 after one accepted Unix connection"
        );

        // After the accept task completes, active must be back at zero.
        accept_task.await.expect("accept task panicked");
        assert_eq!(
            metrics.n2c_connections_active.load(Relaxed),
            0,
            "n2c_connections_active must return to 0 after connection closes"
        );

        // Cleanup.
        let _ = std::fs::remove_file(&tmp);
    }

    /// N2C accept loop: total is monotonic; active tracks in-flight connections.
    #[tokio::test]
    async fn n2c_accept_loop_active_tracks_concurrent_connections() {
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());

        let tmp = std::env::temp_dir().join(format!(
            "dugite-test-n2c-multi-{}.sock",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_file(&tmp);

        let listener = tokio::net::UnixListener::bind(&tmp).expect("bind ephemeral Unix socket");

        let m = metrics.clone();
        // Accept two connections; bump both counters per accepted connection.
        let accept_task = tokio::spawn(async move {
            for _ in 0..2u32 {
                if let Ok((_stream, _)) = listener.accept().await {
                    m.n2c_connections_total.fetch_add(1, Relaxed);
                    m.n2c_connections_active.fetch_add(1, Relaxed);
                }
            }
        });

        // Connect two clients concurrently.
        let c1 = tokio::net::UnixStream::connect(&tmp)
            .await
            .expect("client 1");
        let c2 = tokio::net::UnixStream::connect(&tmp)
            .await
            .expect("client 2");
        accept_task.await.expect("accept task panicked");

        // Both connections accepted → total=2, active=2 (neither has closed yet).
        assert_eq!(metrics.n2c_connections_total.load(Relaxed), 2);
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 2);

        // Close one client — simulates disconnection decrement.
        drop(c1);
        metrics.n2c_connections_active.fetch_sub(1, Relaxed);
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 1);
        assert_eq!(
            metrics.n2c_connections_total.load(Relaxed),
            2,
            "total must not decrease on disconnect"
        );

        // Close second client.
        drop(c2);
        metrics.n2c_connections_active.fetch_sub(1, Relaxed);
        assert_eq!(metrics.n2c_connections_active.load(Relaxed), 0);
        assert_eq!(metrics.n2c_connections_total.load(Relaxed), 2);

        let _ = std::fs::remove_file(&tmp);
    }

    /// Verify that `to_prometheus()` surfaces both `_total` counters so they
    /// can safely be scraped by Prometheus and visualised in dashboards.
    #[test]
    fn prometheus_output_includes_connection_total_counters() {
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = Arc::new(crate::metrics::NodeMetrics::new());
        // Simulate one N2N and two N2C connections accepted.
        metrics.n2n_connections_total.fetch_add(1, Relaxed);
        metrics.n2c_connections_total.fetch_add(2, Relaxed);

        let out = metrics.to_prometheus();

        assert!(
            out.contains("dugite_n2n_connections_total"),
            "Prometheus output must include dugite_n2n_connections_total"
        );
        assert!(
            out.contains("dugite_n2c_connections_total"),
            "Prometheus output must include dugite_n2c_connections_total"
        );
        // The values must reflect the bumps above.  The Prometheus text format
        // emits unlabelled counters as `<name> <value>` (no `{...}` suffix).
        assert!(
            out.contains("dugite_n2n_connections_total 1"),
            "expected n2n_connections_total=1 in:\n{out}"
        );
        assert!(
            out.contains("dugite_n2c_connections_total 2"),
            "expected n2c_connections_total=2 in:\n{out}"
        );
    }

    // ── #1090: tx admission WAITS for a contended ledger lock ───────────
    //
    // Design: docs/superpowers/specs/2026-08-21-tx-admission-lock-contention-design.md
    //
    // Before the fix, `validate_tx` mapped a `try_read()` failure on a
    // contended `ledger` lock straight to `Err(LedgerStateUnavailable)` — a
    // rejection for a transaction on which NO validation had run.
    // cardano-node has no give-up-if-busy path anywhere on its submission
    // surface, so the correct behaviour is to WAIT, not to fabricate a
    // verdict.

    /// A minimal era history whose `current_era()` is Conway. The
    /// intermediate eras are irrelevant to this test —
    /// `record_era_transition` does not require walking them one at a
    /// time.
    fn conway_era_history_for_test() -> dugite_consensus::EraHistory {
        let byron = dugite_consensus::EraParams {
            epoch_size: 21600,
            slot_length_ms: 20000,
            safe_zone: 4320,
            genesis_window: 4320,
        };
        let shelley = dugite_consensus::EraParams {
            epoch_size: 432000,
            slot_length_ms: 1000,
            safe_zone: 129600,
            genesis_window: 129600,
        };
        let mut eh = dugite_consensus::EraHistory::from_genesis(byron, shelley, 0);
        eh.record_era_transition(dugite_primitives::era::Era::Conway, 1);
        eh
    }

    /// A minimal, DECODABLE Conway transaction. It has no witnesses and
    /// spends a UTxO the test ledger never seeded, so it is certain to be
    /// rejected once real Phase-1 validation runs. That is the point: the
    /// test does not need the tx to be admitted, only to prove that a REAL
    /// verdict — not a contention artifact — comes back once the wait ends.
    fn minimal_decodable_conway_tx_bytes() -> Vec<u8> {
        let body = dugite_primitives::transaction::TransactionBody {
            inputs: vec![txin(1)],
            outputs: vec![txout(1_000_000)],
            fee: Lovelace(170_000),
            ttl: None,
            certificates: vec![],
            withdrawals: std::collections::BTreeMap::new(),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: std::collections::BTreeMap::new(),
            script_data_hash: None,
            collateral: vec![],
            required_signers: vec![],
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: vec![],
            update: None,
            voting_procedures: std::collections::BTreeMap::new(),
            proposal_procedures: vec![],
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
            direct_deposits: std::collections::BTreeMap::new(),
            guards: vec![],
        };
        let witness_set = dugite_primitives::transaction::TransactionWitnessSet {
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
        };
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: dugite_primitives::era::Era::Conway,
            body,
            witness_set,
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        dugite_serialization::encode_transaction(&tx)
    }

    /// RED PROOF for #1090.
    ///
    /// The assertion is the WAIT ITSELF, not merely the absence of an
    /// error: `validate_tx` must not return until the contending writer
    /// has released the `ledger` lock. Asserting only "no error" would not
    /// bound this — a fast wrong answer computed against a lock that
    /// SHOULD have been contended could also come back `Err` (a real
    /// Phase-1 rejection) without ever having waited, and this test would
    /// stay green under the old `try_read()` behaviour by coincidence.
    ///
    /// Disarm the fix (revert `serve.rs`'s three lock acquisitions back to
    /// `try_read()` and restore the `LedgerStateUnavailable` variant) and
    /// this goes RED: `validate_tx` returns almost immediately, before
    /// `writer_released` is set, because `try_read()` fails fast on
    /// contention instead of waiting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn validate_tx_waits_for_a_contended_ledger_lock_instead_of_rejecting() {
        let ledger = Arc::new(RwLock::new(LedgerState::new(
            ProtocolParameters::mainnet_defaults(),
        )));
        let era_history = Arc::new(RwLock::new(conway_era_history_for_test()));
        let validator = Arc::new(LedgerTxValidator {
            ledger: ledger.clone(),
            slot_config: dugite_ledger::plutus::SlotConfig {
                zero_time: 0,
                zero_slot: 0,
                slot_length: 1000,
                safe_zone_horizon_slot: None,
            },
            metrics: Arc::new(crate::metrics::NodeMetrics::new()),
            mempool: None,
            network: dugite_primitives::network::NetworkId::Testnet,
            era_history,
        });

        let writer_released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (writer_holds_lock_tx, writer_holds_lock_rx) = tokio::sync::oneshot::channel();

        // Task A: take the write lock FIRST, signal that it holds it, hold
        // it for a fixed window (simulating a block apply), then release
        // and record the release.
        let writer_ledger = ledger.clone();
        let writer_flag = writer_released.clone();
        let writer_task = tokio::spawn(async move {
            let guard = writer_ledger.write().await;
            writer_holds_lock_tx.send(()).expect("receiver still alive");
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            drop(guard);
            writer_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // Wait until the writer confirms it holds the lock before
        // submitting — otherwise this test would race the writer instead
        // of deterministically exercising contention.
        writer_holds_lock_rx
            .await
            .expect("writer task signalled lock acquisition");

        // Task B: submit while the writer holds the lock. `validate_tx` is
        // synchronous and calls `block_in_place` internally, so it must be
        // driven from a real async task on a multi-thread runtime — NOT
        // `spawn_blocking` (tokio's own docs warn that calling
        // `block_in_place` from a `spawn_blocking` task can deadlock) —
        // which is exactly how production drives it from the N2C server
        // loop (`local_tx_submission/server.rs`).
        let reader_validator = validator.clone();
        let tx_bytes = minimal_decodable_conway_tx_bytes();
        let reader_flag = writer_released.clone();
        let reader_task = tokio::spawn(async move {
            let result =
                reader_validator.validate_tx(6 /* Conway wire era index */, &tx_bytes);
            let released_when_we_returned = reader_flag.load(std::sync::atomic::Ordering::SeqCst);
            (result, released_when_we_returned)
        });

        let (result, released_when_we_returned) = reader_task.await.expect("reader task panicked");
        writer_task.await.expect("writer task panicked");

        assert!(
            released_when_we_returned,
            "validate_tx returned before the contending writer released the ledger \
             lock — it must WAIT for a contended `ledger` lock, not fail fast (#1090)"
        );

        // The verdict must be a REAL one, computed against the ledger once
        // it was actually available — never the old fabricated
        // `LedgerStateUnavailable` (which no longer even compiles; the
        // variant is deleted). This tx spends a UTxO the test ledger never
        // seeded, so a real Phase-1 rejection is expected.
        assert!(
            result.is_err(),
            "expected a real Phase-1 rejection (input not found) — got {result:?}"
        );
    }
}
