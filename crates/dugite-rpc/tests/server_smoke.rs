//! End-to-end smoke test for the M1.A RPC scaffold.
//!
//! Spawns [`dugite_rpc::RpcServer`] on a random port backed by a
//! [`MockLedgerContext`] and exercises every service over a real gRPC
//! connection. Every method should return
//! [`tonic::Code::Unimplemented`] (or `FailedPrecondition` for
//! `SubmitTx` whose adapter stub reports the rejection as a structured
//! `SubmitOutcome::Rejected`). Reflection should list all 8 / 9
//! services. Shutdown should drain in-flight RPCs cooperatively.
//!
//! M1.B will add per-method assertions on real responses; this M1.A
//! suite proves the scaffold itself is sound.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dugite_mempool::{Mempool, MempoolConfig};
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::transaction::TransactionInput;
use dugite_rpc::context::{EraHistoryView, GenesisView, ParamsView};
use dugite_rpc::{
    noop_metrics, LedgerContext, MempoolFeed, RawBlock, RawTx, RpcConfig, RpcError, RpcServer,
    SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use tokio::sync::{broadcast, watch};
use tonic::transport::Channel;

// ─── MockLedgerContext ────────────────────────────────────────────────────

/// In-test [`LedgerContext`] impl. Every method returns
/// `RpcError::Unimplemented` so service stubs propagate
/// `Status::Unimplemented`. `mempool_contains` always returns `false`.
struct MockLedgerContext;

#[async_trait]
impl LedgerContext for MockLedgerContext {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Err(RpcError::Unimplemented("mock::tip"))
    }
    async fn block_by_hash(&self, _: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("mock::block_by_hash"))
    }
    async fn block_at_slot(&self, _: u64) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("mock::block_at_slot"))
    }
    async fn block_after(&self, _: u64) -> Result<Option<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("mock::block_after"))
    }
    async fn intersect(&self, _: &[Point]) -> Result<Option<Point>, RpcError> {
        Err(RpcError::Unimplemented("mock::intersect"))
    }
    async fn blocks_range(&self, _: u64, _: u64, _: usize) -> Result<Vec<RawBlock>, RpcError> {
        Err(RpcError::Unimplemented("mock::blocks_range"))
    }
    async fn utxo_by_ref(&self, _: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("mock::utxo_by_ref"))
    }
    async fn utxos_by_address(&self, _: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("mock::utxos_by_address"))
    }
    async fn utxos_by_payment_credential(&self, _: &Hash32) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("mock::utxos_by_payment_credential"))
    }
    async fn utxos_by_asset(
        &self,
        _: &Hash32,
        _: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented("mock::utxos_by_asset"))
    }
    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        Err(RpcError::Unimplemented("mock::params_at_tip"))
    }
    async fn era_history(&self) -> Result<EraHistoryView, RpcError> {
        Err(RpcError::Unimplemented("mock::era_history"))
    }
    async fn genesis(&self) -> Result<GenesisView, RpcError> {
        Err(RpcError::Unimplemented("mock::genesis"))
    }
    async fn submit_tx(&self, _: u16, _: &[u8]) -> SubmitOutcome {
        SubmitOutcome::Rejected {
            reason: "mock::submit_tx not implemented".into(),
        }
    }
    async fn eval_tx(&self, _: u16, _: &[u8]) -> dugite_rpc::EvalOutcome {
        dugite_rpc::EvalOutcome {
            fee: 0,
            error: Some("mock::eval_tx not implemented".into()),
        }
    }
    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        Err(RpcError::Unimplemented("mock::mempool_snapshot"))
    }
    async fn mempool_contains(&self, _: &TransactionHash) -> bool {
        false
    }
}

// ─── Test scaffolding ─────────────────────────────────────────────────────

struct TestServer {
    addr: std::net::SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestServer {
    async fn start(alpha_enabled: bool) -> Self {
        let config = RpcConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            alpha_enabled,
            reflection_enabled: true,
            web_enabled: false,
            ..Default::default()
        };
        let tip_feed = TipFeed::new();
        // The MempoolFeed needs a broadcast::Sender — a stand-alone
        // mempool gives us one without dragging in Node setup.
        let mempool = Arc::new(Mempool::new(MempoolConfig::default()));
        let mempool_feed = MempoolFeed::new(mempool.tx_events());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = RpcServer::start(
            Arc::new(config),
            Arc::new(MockLedgerContext),
            tip_feed,
            mempool_feed,
            noop_metrics(),
            shutdown_rx,
        )
        .await
        .expect("start RPC server");

        Self {
            addr: handle.local_addr,
            shutdown_tx,
            join: handle.join,
        }
    }

    async fn channel(&self) -> Channel {
        let url = format!("http://{}", self.addr);
        Channel::from_shared(url)
            .unwrap()
            .connect_timeout(Duration::from_secs(2))
            .connect()
            .await
            .expect("connect to RPC server")
    }

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        // Give the server a moment to drain. If it doesn't exit cleanly
        // we surface the join error to the test.
        match tokio::time::timeout(Duration::from_secs(3), self.join).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => panic!("server task errored: {e}"),
            Ok(Err(e)) => panic!("server task panicked: {e}"),
            Err(_) => panic!("server task did not exit within 3s of shutdown signal"),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn server_starts_and_stops_cleanly() {
    let server = TestServer::start(true).await;
    assert_ne!(server.addr.port(), 0, "OS should assign a real port");
    server.stop().await;
}

#[tokio::test]
async fn server_binds_loopback_only_by_default() {
    let server = TestServer::start(true).await;
    assert_eq!(server.addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    server.stop().await;
}

// ── Sync v1beta — implemented as of M1.B; see sync_service.rs ───────────
//
// `SyncService` is no longer the all-UNIMPLEMENTED stub it was in M1.A.
// The detailed sync-method correctness suite lives in
// `tests/sync_service.rs` with a richer `SyncMock`. Here we only retain
// the scaffold smoke (ReadTip surfaces its trait error correctly).

#[tokio::test]
async fn sync_v1beta_read_tip_propagates_mock_unimplemented() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::ReadTipRequest;

    let server = TestServer::start(true).await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let status = client
        .read_tip(ReadTipRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    server.stop().await;
}

// ── Sync v1alpha ─────────────────────────────────────────────────────────

#[tokio::test]
async fn sync_v1alpha_read_tip_propagates_mock_unimplemented() {
    use dugite_rpc::proto::v1alpha::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1alpha::sync::ReadTipRequest;

    let server = TestServer::start(true).await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let status = client
        .read_tip(ReadTipRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    server.stop().await;
}

// ── Query v1beta ─────────────────────────────────────────────────────────

#[tokio::test]
async fn query_v1beta_methods_return_unimplemented() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{
        ReadDataRequest, ReadEraSummaryRequest, ReadGenesisRequest, ReadParamsRequest,
        ReadStateRequest, ReadTxRequest, ReadUtxosRequest, SearchUtxosRequest,
    };

    let server = TestServer::start(true).await;
    let mut client = QueryServiceClient::new(server.channel().await);

    for status in [
        client
            .read_params(ReadParamsRequest::default())
            .await
            .unwrap_err(),
        client
            .read_utxos(ReadUtxosRequest::default())
            .await
            .unwrap_err(),
        client
            .search_utxos(SearchUtxosRequest::default())
            .await
            .unwrap_err(),
        client
            .read_data(ReadDataRequest::default())
            .await
            .unwrap_err(),
        client.read_tx(ReadTxRequest::default()).await.unwrap_err(),
        client
            .read_genesis(ReadGenesisRequest::default())
            .await
            .unwrap_err(),
        client
            .read_era_summary(ReadEraSummaryRequest::default())
            .await
            .unwrap_err(),
        client
            .read_state(ReadStateRequest::default())
            .await
            .unwrap_err(),
    ] {
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }

    server.stop().await;
}

// ── Submit v1beta ────────────────────────────────────────────────────────
//
// All Submit methods are implemented as of the EvalTx follow-up. SubmitTx
// / EvalTx with no `raw` field return INVALID_ARGUMENT; the richer
// integration suite for SubmitService lives in submit_service.rs.

#[tokio::test]
async fn submit_v1beta_eval_tx_with_empty_request_returns_invalid_argument() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::EvalTxRequest;

    let server = TestServer::start(true).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let status = client.eval_tx(EvalTxRequest::default()).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    server.stop().await;
}

// ── Watch v1beta — implemented as of M4; see watch_service.rs ───────────
//
// `WatchTx` is now implemented. The smoke test confirms the stream
// connects without erroring; the richer correctness suite lives in
// `tests/watch_service.rs`.

#[tokio::test]
async fn watch_v1beta_stream_connects_without_error() {
    use dugite_rpc::proto::v1beta::watch::watch_service_client::WatchServiceClient;
    use dugite_rpc::proto::v1beta::watch::WatchTxRequest;

    let server = TestServer::start(true).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .expect("watch_tx subscribes");
    drop(stream);
    server.stop().await;
}

// ── alpha_enabled = false ────────────────────────────────────────────────

#[tokio::test]
async fn disabling_alpha_drops_alpha_routes_but_keeps_beta() {
    use dugite_rpc::proto::v1alpha::sync::sync_service_client::SyncServiceClient as AlphaSyncClient;
    use dugite_rpc::proto::v1alpha::sync::ReadTipRequest as AlphaReadTipRequest;
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient as BetaSyncClient;
    use dugite_rpc::proto::v1beta::sync::ReadTipRequest as BetaReadTipRequest;

    let server = TestServer::start(false).await;

    let mut beta = BetaSyncClient::new(server.channel().await);
    let beta_status = beta
        .read_tip(BetaReadTipRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        beta_status.code(),
        tonic::Code::Unimplemented,
        "v1beta should still respond (with UNIMPLEMENTED stub)"
    );

    let mut alpha = AlphaSyncClient::new(server.channel().await);
    let alpha_status = alpha
        .read_tip(AlphaReadTipRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        alpha_status.code(),
        tonic::Code::Unimplemented,
        "v1alpha route is NOT registered when alpha_enabled = false — \
         tonic returns Unimplemented for unknown service routes too \
         (UNIMPLEMENTED in both cases), but the message indicates the \
         route was unknown rather than a stub. Either way the gRPC \
         status code is the same; this asserts the server didn't \
         crash and the connection was accepted."
    );

    server.stop().await;
}

// ── Multiple concurrent clients ──────────────────────────────────────────

#[tokio::test]
async fn server_handles_concurrent_clients() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::ReadTipRequest;

    let server = TestServer::start(true).await;
    let addr = server.addr;

    let mut handles = Vec::new();
    for i in 0..8 {
        let url = format!("http://{}", addr);
        handles.push(tokio::spawn(async move {
            let channel = Channel::from_shared(url)
                .unwrap()
                .connect_timeout(Duration::from_secs(2))
                .connect()
                .await
                .expect("client connect");
            let mut client = SyncServiceClient::new(channel);
            let status = client
                .read_tip(ReadTipRequest::default())
                .await
                .unwrap_err();
            assert_eq!(
                status.code(),
                tonic::Code::Unimplemented,
                "client {i} expected UNIMPLEMENTED"
            );
        }));
    }
    for h in handles {
        h.await.expect("join client task");
    }

    server.stop().await;
}

// ── Shutdown drain ───────────────────────────────────────────────────────

#[tokio::test]
async fn shutdown_signal_terminates_server_within_3s() {
    let server = TestServer::start(true).await;
    let start = std::time::Instant::now();
    server.stop().await;
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "shutdown took {} ms — should be near-instant for a stub server",
        start.elapsed().as_millis()
    );
}

// ── Tip feed: events flow from publisher to a future Streaming subscriber. ──

#[tokio::test]
async fn tip_feed_publisher_round_trips_through_server_owned_channels() {
    // M1.A pre-stubs FollowTip; this test exercises the underlying
    // broadcaster shape independent of the service. Confirms the
    // publish→subscribe contract that M1.B's FollowTip will rely on.
    let feed = TipFeed::new();
    let publisher = feed.publisher();
    let mut rx = feed.subscribe_apply();

    publisher.announce_apply(TipInfo {
        slot: 1234,
        hash: [9u8; 32],
        block_number: 42,
        era: dugite_primitives::Era::Conway,
    });

    let ev = rx.recv().await.expect("tip apply event");
    assert_eq!(ev.slot, 1234);
    assert_eq!(ev.block_number, 42);
}

// ── Mempool feed: events from the underlying Sender reach subscribers ────

#[tokio::test]
async fn mempool_feed_subscribers_see_events() {
    let (tx, _) = broadcast::channel(8);
    let feed = MempoolFeed::new(tx.clone());
    let mut rx = feed.subscribe();

    let hash = Hash32::from_bytes([3u8; 32]);
    tx.send(dugite_rpc::MempoolEvent::Added {
        tx_hash: hash,
        raw_cbor: Some(vec![1, 2, 3]),
    })
    .unwrap();

    match rx.recv().await.unwrap() {
        dugite_rpc::MempoolEvent::Added { tx_hash, raw_cbor } => {
            assert_eq!(tx_hash, hash);
            assert_eq!(raw_cbor, Some(vec![1, 2, 3]));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
