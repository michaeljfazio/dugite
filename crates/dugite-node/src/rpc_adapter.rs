//! `NodeRpcAdapter` — the bridge from `dugite-node` internals to the
//! `dugite-rpc` server's [`LedgerContext`] trait.
//!
//! In M1.A every method returns `Unimplemented`; the adapter only needs
//! to compile + exist so the node can boot the RPC server with all
//! stubbed services. Subsequent milestones fill in real implementations
//! as their corresponding service methods land in `dugite-rpc`.
//!
//! Forward-shim tasks:
//!
//! * [`spawn_tip_forwarder`] subscribes to
//!   `node::tip_broadcast::TipBroadcaster` and republishes into a
//!   `dugite_rpc::TipPublisher`, mapping payloads. Lets the RPC layer
//!   stay dep-free of `dugite-node`.

use std::sync::Arc;

use async_trait::async_trait;
use dugite_mempool::Mempool;
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::transaction::TransactionInput;
use dugite_rpc::{
    LedgerContext, ParamsView, RawBlock, RawTx, RpcError, SubmitOutcome, TipFeed, TipInfo,
    TipPublisher, TipRollback, UtxoSnapshot,
};
use tokio::sync::watch;
use tracing::debug;

use crate::node::tip_broadcast::{TipApply, TipBroadcaster};

/// Concrete impl of [`LedgerContext`] backed by node internals.
///
/// M1.A scope: every method returns `Unimplemented`. Holds Arc clones of
/// the relevant node state so subsequent milestones can fill methods in
/// without re-plumbing the adapter.
pub struct NodeRpcAdapter {
    // Held so that future milestones can fill in real implementations
    // without re-plumbing the adapter. M1.A doesn't read these.
    #[allow(dead_code)]
    pub(crate) mempool: Arc<Mempool>,
}

impl NodeRpcAdapter {
    pub fn new(mempool: Arc<Mempool>) -> Self {
        Self { mempool }
    }
}

#[async_trait]
impl LedgerContext for NodeRpcAdapter {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::tip"))
    }

    async fn block_by_hash(&self, _hash: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::block_by_hash"))
    }

    async fn block_at_slot(&self, _slot: u64) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::block_at_slot"))
    }

    async fn block_after(&self, _slot: u64) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::block_after"))
    }

    async fn intersect(&self, _points: &[Point]) -> Result<Option<Point>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::intersect"))
    }

    async fn blocks_range(
        &self,
        _from_slot: u64,
        _to_slot: u64,
        _limit: usize,
    ) -> Result<Vec<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::blocks_range"))
    }

    async fn utxo_by_ref(&self, _refs: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::utxo_by_ref"))
    }

    async fn utxos_by_address(&self, _addr: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::utxos_by_address"))
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
        Err(RpcError::Unimplemented("LedgerContext::params_at_tip"))
    }

    async fn era_history(&self) -> Result<dugite_rpc::EraHistoryView, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::era_history"))
    }

    async fn genesis(&self) -> Result<dugite_rpc::GenesisView, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::genesis"))
    }

    async fn submit_tx(&self, _era: u16, _raw_cbor: &[u8]) -> SubmitOutcome {
        SubmitOutcome::Rejected {
            reason: "M1.A stub — submit_tx not implemented yet".to_string(),
        }
    }

    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        Err(RpcError::Unimplemented("LedgerContext::mempool_snapshot"))
    }

    async fn mempool_contains(&self, hash: &TransactionHash) -> bool {
        // Cheap call — mempool exposes Mempool::contains directly.
        self.mempool.contains(hash)
    }
}

/// Spawns a forwarder task that subscribes to the node-side
/// [`TipBroadcaster`] and republishes payload-shaped events into the
/// RPC-side [`TipPublisher`]. Exits cleanly when `shutdown_rx` fires
/// (true) or when the upstream broadcaster has no more senders.
///
/// Returns the spawned `JoinHandle` so the host can `.abort()` /
/// `await` it during graceful shutdown.
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
                            // Lossy forwarder — the RPC layer's own slow-
                            // consumer handling kicks in downstream. A
                            // Lagged here means the FORWARDER itself fell
                            // behind, which is structurally unlikely (no
                            // per-event work besides the publish), but
                            // log so it's visible if it ever happens.
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
/// [`dugite_rpc::RpcServer::start`]. Returns the feed + its publisher
/// (the latter is owned by the host so it can spawn the forwarder).
pub fn build_tip_feed() -> (TipFeed, TipPublisher) {
    let feed = TipFeed::new();
    let publisher = feed.publisher();
    (feed, publisher)
}
