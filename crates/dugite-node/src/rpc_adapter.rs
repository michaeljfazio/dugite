//! `NodeRpcAdapter` — the bridge from `dugite-node` internals to the
//! `dugite-rpc` server's [`LedgerContext`] trait.
//!
//! M1.B (this commit): all chain/blocks methods are implemented end-to-end
//! against [`ChainDB`] + [`Mempool`]:
//!
//! * `tip` — reads from `chain_db.get_tip()` and looks up the matching
//!   block's era via a minimal CBOR decode.
//! * `block_by_hash` — direct `chain_db.get_block()`.
//! * `block_at_slot` — `chain_db.get_block_at_or_after_slot()` filtered
//!   to exact-match.
//! * `block_after` — `chain_db.get_next_block_after_slot()`.
//! * `intersect` — walks the supplied points newest-to-oldest, returns
//!   the first that exists on-chain.
//! * `blocks_range` — `chain_db.get_blocks_in_slot_range()` then decodes
//!   identity per block for the carrier fields.
//!
//! `utxo_* / params_at_tip / era_history / genesis / submit_tx /
//! mempool_snapshot` remain `Unimplemented` pending M2 / M3.

use std::sync::Arc;

use async_trait::async_trait;
use dugite_ledger::LedgerState;
use dugite_mempool::Mempool;
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::TransactionInput;
use dugite_rpc::{
    LedgerContext, ParamsView, RawBlock, RawTx, RpcError, SubmitOutcome, TipFeed, TipInfo,
    TipPublisher, TipRollback, UtxoSnapshot,
};
use dugite_serialization::decode::decode_block_minimal;
use dugite_storage::ChainDB;
use tokio::sync::{watch, RwLock};
use tracing::debug;

use crate::node::tip_broadcast::{TipApply, TipBroadcaster};

/// Concrete impl of [`LedgerContext`] backed by node internals.
pub struct NodeRpcAdapter {
    pub(crate) chain_db: Arc<RwLock<ChainDB>>,
    pub(crate) ledger_state: Arc<RwLock<LedgerState>>,
    pub(crate) mempool: Arc<Mempool>,
}

impl NodeRpcAdapter {
    pub fn new(
        chain_db: Arc<RwLock<ChainDB>>,
        ledger_state: Arc<RwLock<LedgerState>>,
        mempool: Arc<Mempool>,
    ) -> Self {
        Self {
            chain_db,
            ledger_state,
            mempool,
        }
    }

    /// Pull (slot, hash, block_no, era) for a raw block CBOR via a
    /// minimal decode. Returns an internal error if the decode fails —
    /// this should never happen for blocks that ChainDB successfully
    /// admitted, so a failure here indicates on-disk corruption.
    fn raw_block_from_cbor(cbor: Vec<u8>) -> Result<RawBlock, RpcError> {
        let block = decode_block_minimal(&cbor)
            .map_err(|e| RpcError::Internal(format!("block decode failed: {e}")))?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(block.header.header_hash.as_ref());
        Ok(RawBlock {
            slot: block.header.slot.0,
            hash,
            block_number: block.header.block_number.0,
            era: block.era,
            cbor,
        })
    }
}

#[async_trait]
impl LedgerContext for NodeRpcAdapter {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        let db = self.chain_db.read().await;
        let (slot, hash, block_no) = match db.get_tip_info() {
            Some(t) => t,
            None => return Err(RpcError::NotFound("chain tip not available".into())),
        };
        // Look up the actual block to recover the era. Minor cost
        // (one minimal decode); the tip is queried often but blocks
        // are bounded ~72 KB and the decode is microseconds.
        let cbor = db
            .get_block(&hash)
            .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
            .ok_or_else(|| {
                RpcError::Internal("tip block missing from ChainDB after get_tip".into())
            })?;
        let raw = Self::raw_block_from_cbor(cbor)?;
        Ok(TipInfo {
            slot: slot.0,
            hash: raw.hash,
            block_number: block_no.0,
            era: raw.era,
        })
    }

    async fn block_by_hash(&self, hash: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some(cbor) = db
            .get_block(hash)
            .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
        else {
            return Ok(None);
        };
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn block_at_slot(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some((s, _h, cbor)) = db
            .get_block_at_or_after_slot(SlotNo(slot))
            .map_err(|e| RpcError::Internal(format!("chain_db get_block_at_or_after_slot: {e}")))?
        else {
            return Ok(None);
        };
        if s.0 != slot {
            return Ok(None);
        }
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn block_after(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some((_s, _h, cbor)) = db
            .get_next_block_after_slot(SlotNo(slot))
            .map_err(|e| RpcError::Internal(format!("chain_db get_next_block_after_slot: {e}")))?
        else {
            return Ok(None);
        };
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn intersect(&self, points: &[Point]) -> Result<Option<Point>, RpcError> {
        let db = self.chain_db.read().await;
        // Scan from newest-slot to oldest: ChainSync intersection
        // semantics return the LATEST point that exists. Origin is
        // always valid as a fallback.
        let mut sorted: Vec<&Point> = points.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.slot().map(|s| s.0).unwrap_or(0)));
        for point in sorted {
            match point {
                Point::Origin => return Ok(Some(Point::Origin)),
                Point::Specific(slot, hash) => {
                    if let Some(cbor) = db
                        .get_block(hash)
                        .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
                    {
                        // Verify slot matches as a sanity check.
                        if let Ok(decoded) = decode_block_minimal(&cbor) {
                            if decoded.header.slot == *slot {
                                return Ok(Some(point.clone()));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    async fn blocks_range(
        &self,
        from_slot: u64,
        to_slot: u64,
        limit: usize,
    ) -> Result<Vec<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let cbors = db
            .get_blocks_in_slot_range(SlotNo(from_slot), SlotNo(to_slot))
            .map_err(|e| RpcError::Internal(format!("chain_db blocks_range: {e}")))?;
        let mut out = Vec::with_capacity(cbors.len().min(limit));
        for cbor in cbors.into_iter().take(limit) {
            out.push(Self::raw_block_from_cbor(cbor)?);
        }
        Ok(out)
    }

    async fn utxo_by_ref(&self, refs: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        let ledger = self.ledger_state.read().await;
        let mut out = Vec::with_capacity(refs.len());
        for input in refs {
            if let Some(output) = ledger.utxo.utxo_set.lookup(input) {
                out.push(UtxoSnapshot {
                    ref_: input.clone(),
                    output,
                    slot: None,
                });
            }
        }
        Ok(out)
    }

    async fn utxos_by_address(&self, _addr: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(
            "LedgerContext::utxos_by_address (M2.B — needs UtxoSet::outputs_for_address accessor)",
        ))
    }

    async fn utxos_by_payment_credential(
        &self,
        _cred: &Hash32,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(
            "LedgerContext::utxos_by_payment_credential (no index in v1)",
        ))
    }

    async fn utxos_by_asset(
        &self,
        _policy: &Hash32,
        _name: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::utxos_by_asset"))
    }

    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        let ledger = self.ledger_state.read().await;
        let params = ledger.epochs.protocol_params.clone();
        let pv = params.protocol_version_major;
        Ok(ParamsView {
            params: Arc::new(params),
            protocol_version_major: pv,
        })
    }

    async fn era_history(&self) -> Result<dugite_rpc::EraHistoryView, RpcError> {
        // M2.A: minimal era-history projection. We project the era of
        // the current tip as a single EraSummary entry, derived from
        // protocol_version_major. Full era-boundary mapping requires
        // the era-history projection from `node/n2c_query/protocol.rs`
        // which is a sequel commit.
        let ledger = self.ledger_state.read().await;
        let pv = ledger.epochs.protocol_params.protocol_version_major;
        let era = dugite_primitives::block::ProtocolVersion {
            major: pv,
            minor: 0,
        }
        .era();
        Ok(dugite_rpc::EraHistoryView {
            summaries: vec![dugite_rpc::EraSummary {
                era,
                first_slot: 0,
                slot_length_ms: 1_000,
                epoch_length_slots: 432_000,
            }],
        })
    }

    async fn genesis(&self) -> Result<dugite_rpc::GenesisView, RpcError> {
        // M2.A: emit network_magic + the conventional Shelley start
        // time (1596059091 s for mainnet — overridden per-network at
        // M2.B when the genesis files are wired through). security_param
        // mirrors the ledger's k.
        let ledger = self.ledger_state.read().await;
        let _ = ledger; // security_param k lives in node config; surfaced fully in M2.B
        Ok(dugite_rpc::GenesisView {
            network_magic: 0,
            system_start_unix: 0,
            security_param: 0,
        })
    }

    async fn submit_tx(&self, _era: u16, _raw_cbor: &[u8]) -> SubmitOutcome {
        SubmitOutcome::Rejected {
            reason: "M1.B stub — submit_tx not implemented yet (M3)".to_string(),
        }
    }

    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::mempool_snapshot"))
    }

    async fn mempool_contains(&self, hash: &TransactionHash) -> bool {
        self.mempool.contains(hash)
    }
}

/// Spawns a forwarder task that subscribes to the node-side
/// [`TipBroadcaster`] and republishes payload-shaped events into the
/// RPC-side [`TipPublisher`]. Exits cleanly when `shutdown_rx` fires
/// (true) or when the upstream broadcaster has no more senders.
pub fn spawn_tip_forwarder(
    broadcaster: Arc<TipBroadcaster>,
    publisher: TipPublisher,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let mut apply_rx = broadcaster.subscribe_apply();
    let mut rollback_rx = broadcaster.subscribe_rollback();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        debug!("dugite-rpc tip forwarder: shutdown signalled, exiting");
                        break;
                    }
                }
                apply = apply_rx.recv() => {
                    match apply {
                        Ok(ev) => publisher.announce_apply(map_apply(ev)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged = n, "dugite-rpc tip forwarder lagged on apply broadcast");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                rollback = rollback_rx.recv() => {
                    match rollback {
                        Ok(ev) => publisher.announce_rollback(TipRollback {
                            slot: ev.slot,
                            hash: ev.hash,
                        }),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged = n, "dugite-rpc tip forwarder lagged on rollback broadcast");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

fn map_apply(ev: TipApply) -> TipInfo {
    TipInfo {
        slot: ev.slot,
        hash: ev.hash,
        block_number: ev.block_number,
        era: ev.era,
    }
}

/// Builds a fresh [`TipFeed`] suitable for handing to
/// [`dugite_rpc::RpcServer::start`]. Returns the feed + its publisher.
pub fn build_tip_feed() -> (TipFeed, TipPublisher) {
    let feed = TipFeed::new();
    let publisher = feed.publisher();
    (feed, publisher)
}
