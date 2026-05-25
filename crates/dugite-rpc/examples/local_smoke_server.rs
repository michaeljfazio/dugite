//! Stand-alone smoke server for ad-hoc local testing of the UTxO RPC.
//!
//! Boots a complete `RpcServer` backed by a minimal in-memory mock —
//! enough to exercise every QueryService / SubmitService / WatchService
//! / SyncService method via grpcurl. The mock answers most requests
//! with "empty but valid" data so wire-format / reflection / streaming
//! round-trips can be inspected without a running node.
//!
//! Run with:
//!   cargo run --release -p dugite-rpc --example local_smoke_server -- 50051
//!
//! Then exercise from another shell:
//!   grpcurl -plaintext localhost:50051 list
//!   grpcurl -plaintext localhost:50051 utxorpc.v1beta.query.QueryService/ReadState
//!   grpcurl -plaintext -d '{}' localhost:50051 utxorpc.v1beta.query.QueryService/ReadParams

use async_trait::async_trait;
use dugite_mempool::{Mempool, MempoolConfig};
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::TransactionInput;
use dugite_primitives::Era;
use dugite_rpc::{
    context::{EraHistoryView, GenesisView, LedgerStateView, ParamsView},
    noop_metrics, EraSummary, EvalOutcome, LedgerContext, MempoolFeed, RawBlock, RawTx, RpcConfig,
    RpcError, RpcServer, SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::watch;

struct DemoContext {
    params: Arc<ProtocolParameters>,
}

#[async_trait]
impl LedgerContext for DemoContext {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Ok(TipInfo {
            slot: 100,
            hash: [0x11; 32],
            block_number: 50,
            era: Era::Conway,
        })
    }
    async fn block_by_hash(&self, _: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        Ok(None)
    }
    async fn block_at_slot(&self, _: u64) -> Result<Option<RawBlock>, RpcError> {
        Ok(None)
    }
    async fn block_after(&self, _: u64) -> Result<Option<RawBlock>, RpcError> {
        Ok(None)
    }
    async fn intersect(&self, _: &[Point]) -> Result<Option<Point>, RpcError> {
        Ok(None)
    }
    async fn blocks_range(&self, _: u64, _: u64, _: usize) -> Result<Vec<RawBlock>, RpcError> {
        Ok(Vec::new())
    }
    async fn utxo_by_ref(&self, _: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn utxos_by_address(&self, _: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn utxos_by_payment_credential(
        &self,
        _: &Hash32,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn utxos_by_asset(
        &self,
        _: &Hash32,
        _: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn utxos_filter(
        &self,
        _: &(dyn for<'a> Fn(&'a UtxoSnapshot) -> bool + Send + Sync),
        _: usize,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn datum_by_hash(&self, _: &Hash32) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(None)
    }
    async fn tx_by_hash(&self, _: &TransactionHash) -> Result<Option<RawTx>, RpcError> {
        Ok(None)
    }
    async fn ledger_state(&self) -> Result<LedgerStateView, RpcError> {
        Ok(LedgerStateView {
            tip: TipInfo {
                slot: 100,
                hash: [0x11; 32],
                block_number: 50,
                era: Era::Conway,
            },
            epoch: 7,
            slot_in_epoch: 12,
        })
    }
    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        Ok(ParamsView {
            params: self.params.clone(),
            protocol_version_major: self.params.protocol_version_major,
        })
    }
    async fn era_history(&self) -> Result<EraHistoryView, RpcError> {
        Ok(EraHistoryView {
            summaries: vec![EraSummary {
                era: Era::Conway,
                first_slot: 0,
                slot_length_ms: 1000,
                epoch_length_slots: 432_000,
            }],
        })
    }
    async fn genesis(&self) -> Result<GenesisView, RpcError> {
        Ok(GenesisView {
            network_magic: 2,
            system_start_unix: 1_666_656_000,
            security_param: 432,
        })
    }
    async fn submit_tx(&self, _: u16, _: &[u8]) -> SubmitOutcome {
        SubmitOutcome::Rejected {
            reason: "demo server does not accept submissions".into(),
        }
    }
    async fn eval_tx(&self, _: u16, _: &[u8]) -> EvalOutcome {
        EvalOutcome {
            fee: 200_000,
            error: None,
            redeemers: vec![dugite_rpc::RedeemerReport {
                index: 0,
                purpose: dugite_rpc::RedeemerPurpose::Spend,
                ex_units: (1_234_567, 8_910),
                logs: vec!["demo trace line".into()],
                error: None,
            }],
        }
    }
    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        Ok(Vec::new())
    }
    async fn mempool_contains(&self, _: &TransactionHash) -> bool {
        false
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "50051".into())
        .parse()?;
    let config = Arc::new(RpcConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        alpha_enabled: true,
        ..Default::default()
    });
    let context = Arc::new(DemoContext {
        params: Arc::new(ProtocolParameters::mainnet_defaults()),
    });
    let tip_feed = TipFeed::new();
    let mempool = Arc::new(Mempool::new(MempoolConfig::default()));
    let mempool_feed = MempoolFeed::new(mempool.tx_events());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = RpcServer::start(
        config,
        context,
        tip_feed,
        mempool_feed,
        noop_metrics(),
        shutdown_rx,
    )
    .await?;
    println!("UTxO RPC demo server listening on {}", handle.local_addr);
    let _ = handle.join.await?;
    Ok(())
}
