//! Integration tests for the real `SubmitService` implementation
//! (`src/services/submit.rs`) — both `v1beta` and `v1alpha` surfaces.
//!
//! Backed by a `SubmitMock` whose submit / eval / mempool behaviour is
//! configured per test and which records every `(era, bytes)` pair the
//! service forwards, so the tests measure what actually crossed the
//! trait boundary rather than just the wire status.
//!
//! Streaming tests are event-driven: the gRPC handler subscribes to the
//! `MempoolFeed` broadcast *before* returning the response stream, so an
//! event fired after the stream is established is guaranteed to be
//! observed — no sleeps, no wall-clock assertions. Negative cases
//! ("this event must NOT be emitted") are proven by firing the
//! non-emitting event first and a sentinel emitting event second, then
//! asserting the first received message is the sentinel: the forwarding
//! task processes broadcast events strictly in order.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::transaction::TransactionInput;
use dugite_rpc::context::{
    EraHistoryView, GenesisView, ParamsView, RedeemerPurpose, RedeemerReport,
};
use dugite_rpc::{
    noop_metrics, EvalOutcome, LedgerContext, MempoolEvent, MempoolFeed, MempoolRemoveReason,
    RawBlock, RawTx, RpcConfig, RpcError, RpcServer, SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use tokio::sync::{broadcast, watch};
use tokio_stream::StreamExt;
use tonic::transport::Channel;

// ─── SubmitMock ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Recorded {
    submits: Vec<(u16, Vec<u8>)>,
    evals: Vec<(u16, Vec<u8>)>,
}

struct SubmitMock {
    /// `Some(hash)` → every submit is `Accepted { hash }`; `None` →
    /// `Rejected { reason: reject_reason }`.
    accept_hash: Option<[u8; 32]>,
    reject_reason: String,
    eval: EvalOutcome,
    /// Hashes `mempool_contains` answers `true` for.
    resident: HashSet<[u8; 32]>,
    /// `Some(items)` → `mempool_snapshot` succeeds; `None` → `Internal`.
    snapshot: Option<Vec<RawTx>>,
    recorded: Mutex<Recorded>,
}

impl SubmitMock {
    fn rejecting(reason: &str) -> Self {
        Self {
            accept_hash: None,
            reject_reason: reason.to_string(),
            eval: EvalOutcome {
                fee: 0,
                error: None,
                redeemers: Vec::new(),
            },
            resident: HashSet::new(),
            snapshot: Some(Vec::new()),
            recorded: Mutex::new(Recorded::default()),
        }
    }

    fn accepting(hash: [u8; 32]) -> Self {
        let mut m = Self::rejecting("unused");
        m.accept_hash = Some(hash);
        m
    }

    fn submits(&self) -> Vec<(u16, Vec<u8>)> {
        self.recorded.lock().unwrap().submits.clone()
    }

    fn evals(&self) -> Vec<(u16, Vec<u8>)> {
        self.recorded.lock().unwrap().evals.clone()
    }
}

#[async_trait]
impl LedgerContext for SubmitMock {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Err(RpcError::Unimplemented("mock::tip"))
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
        Err(RpcError::Unimplemented(""))
    }
    async fn utxos_by_address(&self, _: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn utxos_by_payment_credential(&self, _: &Hash32) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn utxos_by_asset(
        &self,
        _: &Hash32,
        _: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn era_history(&self) -> Result<EraHistoryView, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn genesis(&self) -> Result<GenesisView, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn submit_tx(&self, era: u16, raw_cbor: &[u8]) -> SubmitOutcome {
        self.recorded
            .lock()
            .unwrap()
            .submits
            .push((era, raw_cbor.to_vec()));
        match self.accept_hash {
            Some(h) => SubmitOutcome::Accepted {
                hash: Hash32::from_bytes(h),
            },
            None => SubmitOutcome::Rejected {
                reason: self.reject_reason.clone(),
            },
        }
    }
    async fn eval_tx(&self, era: u16, raw_cbor: &[u8]) -> EvalOutcome {
        self.recorded
            .lock()
            .unwrap()
            .evals
            .push((era, raw_cbor.to_vec()));
        self.eval.clone()
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
    async fn ledger_state(&self) -> Result<dugite_rpc::LedgerStateView, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        match &self.snapshot {
            Some(items) => Ok(items.clone()),
            None => Err(RpcError::Internal("mempool snapshot unavailable".into())),
        }
    }
    async fn mempool_contains(&self, hash: &TransactionHash) -> bool {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(hash.as_ref());
        self.resident.contains(&arr)
    }
}

// ─── Scaffold ────────────────────────────────────────────────────────────

struct TestServer {
    addr: std::net::SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    /// The broadcast sender behind the server's `MempoolFeed` — tests
    /// fire `MempoolEvent`s through it after establishing a stream.
    mempool_tx: broadcast::Sender<MempoolEvent>,
}

impl TestServer {
    async fn start(mock: Arc<SubmitMock>) -> Self {
        let config = RpcConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            alpha_enabled: true,
            ..Default::default()
        };
        let (mempool_tx, _keepalive) = broadcast::channel(64);
        let mempool_feed = MempoolFeed::new(mempool_tx.clone());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = RpcServer::start(
            Arc::new(config),
            mock,
            TipFeed::new(),
            mempool_feed,
            noop_metrics(),
            shutdown_rx,
        )
        .await
        .expect("start RPC");
        Self {
            addr: handle.local_addr,
            shutdown_tx,
            join: handle.join,
            mempool_tx,
        }
    }

    async fn channel(&self) -> Channel {
        let url = format!("http://{}", self.addr);
        Channel::from_shared(url)
            .unwrap()
            .connect_timeout(Duration::from_secs(2))
            .connect()
            .await
            .expect("connect")
    }

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        match tokio::time::timeout(Duration::from_secs(3), self.join).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("server task panicked: {e}"),
            Err(_) => panic!("server didn't shut down within 3s"),
        }
    }
}

fn beta_raw_tx(bytes: Vec<u8>) -> dugite_rpc::proto::v1beta::submit::AnyChainTx {
    use dugite_rpc::proto::v1beta::submit::{any_chain_tx, AnyChainTx};
    AnyChainTx {
        r#type: Some(any_chain_tx::Type::Raw(bytes)),
    }
}

fn alpha_raw_tx(bytes: Vec<u8>) -> dugite_rpc::proto::v1alpha::submit::AnyChainTx {
    use dugite_rpc::proto::v1alpha::submit::{any_chain_tx, AnyChainTx};
    AnyChainTx {
        r#type: Some(any_chain_tx::Type::Raw(bytes)),
    }
}

fn hash32(byte: u8) -> Hash32 {
    Hash32::from_bytes([byte; 32])
}

// ─── SubmitTx ────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_tx_accepted_returns_hash_and_forwards_conway_era_and_bytes() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::SubmitTxRequest;

    let mock = Arc::new(SubmitMock::accepting([0x5A; 32]));
    let server = TestServer::start(mock.clone()).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    let tx_bytes = vec![0x84, 0xA3, 0x00, 0x01, 0x02];
    let resp = client
        .submit_tx(SubmitTxRequest {
            tx: Some(beta_raw_tx(tx_bytes.clone())),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.r#ref, vec![0x5A; 32], "ref must be the accepted hash");

    // The service must forward the raw bytes verbatim under the Conway
    // era id (6) — the documented default until explicit era hints land.
    let submits = mock.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].0, 6, "SubmitTx must claim Conway era id 6");
    assert_eq!(submits[0].1, tx_bytes, "raw tx bytes must pass unmodified");
    server.stop().await;
}

#[tokio::test]
async fn submit_tx_rejected_maps_to_failed_precondition_with_verbatim_reason() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::SubmitTxRequest;

    let reason = "ConwayMempoolFailure(7, \"transaction decode failed: duplicate input\")";
    let mock = Arc::new(SubmitMock::rejecting(reason));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    let status = client
        .submit_tx(SubmitTxRequest {
            tx: Some(beta_raw_tx(vec![0xDE, 0xAD])),
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        status.message(),
        reason,
        "structured rejection reason must survive the wire verbatim"
    );
    server.stop().await;
}

#[tokio::test]
async fn submit_tx_missing_raw_is_invalid_argument_and_never_reaches_context() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{AnyChainTx, SubmitTxRequest};

    let mock = Arc::new(SubmitMock::accepting([0x11; 32]));
    let server = TestServer::start(mock.clone()).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    // Entirely absent tx envelope.
    let status = client
        .submit_tx(SubmitTxRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // Envelope present but the oneof unset.
    let status = client
        .submit_tx(SubmitTxRequest {
            tx: Some(AnyChainTx { r#type: None }),
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    assert!(
        mock.submits().is_empty(),
        "malformed requests must be rejected before reaching the ledger"
    );
    server.stop().await;
}

#[tokio::test]
async fn submit_tx_alpha_paths_match_beta() {
    use dugite_rpc::proto::v1alpha::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1alpha::submit::SubmitTxRequest;

    // Accepted path.
    let mock = Arc::new(SubmitMock::accepting([0x77; 32]));
    let server = TestServer::start(mock.clone()).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let resp = client
        .submit_tx(SubmitTxRequest {
            tx: Some(alpha_raw_tx(vec![0x01, 0x02])),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.r#ref, vec![0x77; 32]);
    assert_eq!(mock.submits()[0].0, 6);
    server.stop().await;

    // Rejected path.
    let mock = Arc::new(SubmitMock::rejecting("alpha reject"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let status = client
        .submit_tx(SubmitTxRequest {
            tx: Some(alpha_raw_tx(vec![0x01])),
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert_eq!(status.message(), "alpha reject");

    // Missing raw path.
    let status = client
        .submit_tx(SubmitTxRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    server.stop().await;
}

// ─── EvalTx ──────────────────────────────────────────────────────────────

fn rich_eval_outcome() -> EvalOutcome {
    EvalOutcome {
        fee: 172_805,
        error: Some("phase-2 failed: script error".into()),
        redeemers: vec![
            RedeemerReport {
                index: 0,
                purpose: RedeemerPurpose::Spend,
                ex_units: (1_000, 2_000),
                logs: vec!["trace-a".into(), "trace-b".into()],
                error: None,
            },
            RedeemerReport {
                index: 1,
                purpose: RedeemerPurpose::Mint,
                ex_units: (30, 40),
                logs: vec![],
                error: Some("mint redeemer blew up".into()),
            },
        ],
    }
}

#[tokio::test]
async fn eval_tx_maps_fee_exunits_traces_errors_and_redeemers() {
    use dugite_rpc::proto::v1beta::cardano::{big_int, RedeemerPurpose as PbPurpose};
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{any_chain_eval, EvalTxRequest};

    let mut mock = SubmitMock::rejecting("unused");
    mock.eval = rich_eval_outcome();
    let mock = Arc::new(mock);
    let server = TestServer::start(mock.clone()).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    let tx_bytes = vec![0x84, 0x00];
    let resp = client
        .eval_tx(EvalTxRequest {
            tx: Some(beta_raw_tx(tx_bytes.clone())),
        })
        .await
        .unwrap()
        .into_inner();

    // EvalTx must forward era 6 + the raw bytes, and must NOT submit.
    assert_eq!(mock.evals(), vec![(6u16, tx_bytes)]);
    assert!(mock.submits().is_empty(), "EvalTx is non-committing");

    let any_chain_eval::Chain::Cardano(eval) = resp.report.unwrap().chain.unwrap();

    // Fee.
    match eval.fee.unwrap().big_int.unwrap() {
        big_int::BigInt::Int(v) => assert_eq!(v, 172_805),
        other => panic!("unexpected fee variant: {other:?}"),
    }

    // Total ex-units = sum of per-redeemer (steps, memory).
    let total = eval.ex_units.unwrap();
    assert_eq!(total.steps, 1_030);
    assert_eq!(total.memory, 2_040);

    // Traces: one report per log line, tagged with purpose + index.
    assert_eq!(eval.traces.len(), 2);
    assert_eq!(eval.traces[0].msg, "trace-a");
    assert_eq!(eval.traces[1].msg, "trace-b");
    for t in &eval.traces {
        assert_eq!(t.purpose, PbPurpose::Spend as i32);
        assert_eq!(t.index, 0);
    }

    // Errors: tx-level first (Unspecified/0), then per-redeemer.
    assert_eq!(eval.errors.len(), 2);
    assert_eq!(eval.errors[0].msg, "phase-2 failed: script error");
    assert_eq!(eval.errors[0].purpose, PbPurpose::Unspecified as i32);
    assert_eq!(eval.errors[1].msg, "mint redeemer blew up");
    assert_eq!(eval.errors[1].purpose, PbPurpose::Mint as i32);
    assert_eq!(eval.errors[1].index, 1);

    // Per-redeemer envelopes.
    assert_eq!(eval.redeemers.len(), 2);
    assert_eq!(eval.redeemers[0].purpose, PbPurpose::Spend as i32);
    let r0_units = eval.redeemers[0].ex_units.unwrap();
    assert_eq!((r0_units.steps, r0_units.memory), (1_000, 2_000));
    assert_eq!(eval.redeemers[1].purpose, PbPurpose::Mint as i32);
    assert_eq!(eval.redeemers[1].index, 1);
    let r1_units = eval.redeemers[1].ex_units.unwrap();
    assert_eq!((r1_units.steps, r1_units.memory), (30, 40));

    server.stop().await;
}

#[tokio::test]
async fn eval_tx_exunits_sum_saturates_instead_of_wrapping() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{any_chain_eval, EvalTxRequest};

    let mut mock = SubmitMock::rejecting("unused");
    mock.eval = EvalOutcome {
        fee: 1,
        error: None,
        redeemers: vec![
            RedeemerReport {
                index: 0,
                purpose: RedeemerPurpose::Spend,
                ex_units: (u64::MAX, u64::MAX),
                logs: vec![],
                error: None,
            },
            RedeemerReport {
                index: 1,
                purpose: RedeemerPurpose::Spend,
                ex_units: (5, 7),
                logs: vec![],
                error: None,
            },
        ],
    };
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let resp = client
        .eval_tx(EvalTxRequest {
            tx: Some(beta_raw_tx(vec![0x00])),
        })
        .await
        .unwrap()
        .into_inner();
    let any_chain_eval::Chain::Cardano(eval) = resp.report.unwrap().chain.unwrap();
    let total = eval.ex_units.unwrap();
    assert_eq!(total.steps, u64::MAX, "steps must saturate, not wrap");
    assert_eq!(total.memory, u64::MAX, "memory must saturate, not wrap");
    server.stop().await;
}

#[tokio::test]
async fn eval_tx_missing_raw_is_invalid_argument() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::EvalTxRequest;

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock.clone()).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let status = client.eval_tx(EvalTxRequest::default()).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(mock.evals().is_empty());
    server.stop().await;
}

#[tokio::test]
async fn eval_tx_alpha_subset_reencode_carries_fee_and_exunits() {
    use dugite_rpc::proto::v1alpha::cardano::big_int;
    use dugite_rpc::proto::v1alpha::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1alpha::submit::{any_chain_eval, EvalTxRequest};

    let mut mock = SubmitMock::rejecting("unused");
    mock.eval = rich_eval_outcome();
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let resp = client
        .eval_tx(EvalTxRequest {
            tx: Some(alpha_raw_tx(vec![0x00])),
        })
        .await
        .unwrap()
        .into_inner();
    let any_chain_eval::Chain::Cardano(eval) = resp.report.unwrap().chain.unwrap();
    match eval.fee.unwrap().big_int.unwrap() {
        big_int::BigInt::Int(v) => assert_eq!(v, 172_805),
        other => panic!("unexpected fee variant: {other:?}"),
    }
    let total = eval.ex_units.unwrap();
    assert_eq!((total.steps, total.memory), (1_030, 2_040));
    assert_eq!(eval.redeemers.len(), 2);
    server.stop().await;
}

// ─── ReadMempool ─────────────────────────────────────────────────────────

#[tokio::test]
async fn read_mempool_returns_snapshot_with_mempool_stage() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{ReadMempoolRequest, Stage};

    let mut mock = SubmitMock::rejecting("unused");
    mock.snapshot = Some(vec![
        RawTx {
            hash: hash32(0x01),
            cbor: vec![0xAA, 0xBB],
        },
        RawTx {
            hash: hash32(0x02),
            cbor: vec![0xCC],
        },
    ]);
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let resp = client
        .read_mempool(ReadMempoolRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].r#ref, vec![0x01; 32]);
    assert_eq!(resp.items[0].native_bytes, vec![0xAA, 0xBB]);
    assert_eq!(resp.items[0].stage, Stage::Mempool as i32);
    assert_eq!(resp.items[1].r#ref, vec![0x02; 32]);
    assert_eq!(resp.items[1].native_bytes, vec![0xCC]);
    assert_eq!(resp.items[1].stage, Stage::Mempool as i32);
    server.stop().await;
}

#[tokio::test]
async fn read_mempool_propagates_context_error_status() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::ReadMempoolRequest;

    let mut mock = SubmitMock::rejecting("unused");
    mock.snapshot = None; // context reports Internal
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let status = client
        .read_mempool(ReadMempoolRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Internal);
    server.stop().await;
}

// ─── WaitForTx ───────────────────────────────────────────────────────────

#[tokio::test]
async fn wait_for_tx_reports_mempool_stage_immediately_when_tx_resident() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{Stage, WaitForTxRequest};

    let mut mock = SubmitMock::rejecting("unused");
    mock.resident.insert([0x42; 32]);
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    let mut stream = client
        .wait_for_tx(WaitForTxRequest {
            r#ref: vec![vec![0x42; 32]],
        })
        .await
        .unwrap()
        .into_inner();

    let msg = stream.next().await.expect("immediate stage msg").unwrap();
    assert_eq!(msg.r#ref, vec![0x42; 32]);
    assert_eq!(msg.stage, Stage::Mempool as i32);
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn wait_for_tx_streams_mempool_then_confirmed_stages() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{Stage, WaitForTxRequest};

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let watched = hash32(0x7E);

    let mut stream = client
        .wait_for_tx(WaitForTxRequest {
            r#ref: vec![watched.as_ref().to_vec()],
        })
        .await
        .unwrap()
        .into_inner();

    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: watched,
            raw_cbor: Some(vec![0x01]),
        })
        .unwrap();
    let msg = stream.next().await.expect("mempool stage").unwrap();
    assert_eq!(msg.r#ref, watched.as_ref().to_vec());
    assert_eq!(msg.stage, Stage::Mempool as i32);

    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: watched,
            reason: MempoolRemoveReason::Mined,
        })
        .unwrap();
    let msg = stream.next().await.expect("confirmed stage").unwrap();
    assert_eq!(msg.r#ref, watched.as_ref().to_vec());
    assert_eq!(msg.stage, Stage::Confirmed as i32);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn wait_for_tx_ignores_unwatched_hashes_and_non_mined_removals() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{Stage, WaitForTxRequest};

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let watched = hash32(0x10);
    let other = hash32(0x20);

    let mut stream = client
        .wait_for_tx(WaitForTxRequest {
            r#ref: vec![watched.as_ref().to_vec()],
        })
        .await
        .unwrap()
        .into_inner();

    // 1. Added for an unwatched hash → must be skipped.
    // 2. Evicted removal of the WATCHED hash → must NOT emit Confirmed.
    // 3. Added for the watched hash → sentinel; must be the FIRST msg.
    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: other,
            raw_cbor: Some(vec![0x02]),
        })
        .unwrap();
    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: watched,
            reason: MempoolRemoveReason::Evicted,
        })
        .unwrap();
    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: watched,
            raw_cbor: Some(vec![0x03]),
        })
        .unwrap();

    let msg = stream.next().await.expect("sentinel msg").unwrap();
    assert_eq!(
        msg.r#ref,
        watched.as_ref().to_vec(),
        "unwatched Added and Evicted removal must both be skipped"
    );
    assert_eq!(msg.stage, Stage::Mempool as i32);

    // A Manual removal must also be skipped; the Mined removal after it
    // must be the next (and only) message.
    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: watched,
            reason: MempoolRemoveReason::Manual,
        })
        .unwrap();
    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: watched,
            reason: MempoolRemoveReason::Mined,
        })
        .unwrap();
    let msg = stream.next().await.expect("confirmed msg").unwrap();
    assert_eq!(msg.stage, Stage::Confirmed as i32);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn wait_for_tx_non_32_byte_ref_is_ignored_without_panic() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{Stage, WaitForTxRequest};

    let mut mock = SubmitMock::rejecting("unused");
    mock.resident.insert([0x66; 32]);
    let server = TestServer::start(Arc::new(mock)).await;
    let mut client = SubmitServiceClient::new(server.channel().await);

    // One malformed (16-byte) ref alongside one valid resident ref: the
    // handler must skip the malformed ref (no panic in the [u8; 32]
    // copy) and still emit the immediate stage for the valid one.
    let mut stream = client
        .wait_for_tx(WaitForTxRequest {
            r#ref: vec![vec![0xAA; 16], vec![0x66; 32]],
        })
        .await
        .unwrap()
        .into_inner();

    let msg = stream.next().await.expect("stage msg").unwrap();
    assert_eq!(msg.r#ref, vec![0x66; 32]);
    assert_eq!(msg.stage, Stage::Mempool as i32);
    drop(stream);
    server.stop().await;
}

// ─── WatchMempool ────────────────────────────────────────────────────────

#[tokio::test]
async fn watch_mempool_streams_added_events_and_skips_removals() {
    use dugite_rpc::proto::v1beta::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1beta::submit::{Stage, WatchMempoolRequest};

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_mempool(WatchMempoolRequest::default())
        .await
        .unwrap()
        .into_inner();

    // Removed first (must be skipped), then Added with bytes.
    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: hash32(0x01),
            reason: MempoolRemoveReason::Mined,
        })
        .unwrap();
    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: hash32(0x02),
            raw_cbor: Some(vec![0xFE, 0xED]),
        })
        .unwrap();

    let msg = stream.next().await.expect("added msg").unwrap();
    let item = msg.tx.expect("tx set");
    assert_eq!(item.r#ref, vec![0x02; 32], "Removed event must be skipped");
    assert_eq!(item.native_bytes, vec![0xFE, 0xED]);
    assert_eq!(item.stage, Stage::Mempool as i32);

    // Added without raw bytes → empty native_bytes, event still emitted.
    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: hash32(0x03),
            raw_cbor: None,
        })
        .unwrap();
    let msg = stream.next().await.expect("bytes-less added msg").unwrap();
    let item = msg.tx.expect("tx set");
    assert_eq!(item.r#ref, vec![0x03; 32]);
    assert!(item.native_bytes.is_empty());

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_mempool_alpha_streams_added_events() {
    use dugite_rpc::proto::v1alpha::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1alpha::submit::{Stage, WatchMempoolRequest};

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_mempool(WatchMempoolRequest::default())
        .await
        .unwrap()
        .into_inner();

    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: hash32(0x09),
            raw_cbor: Some(vec![0x0A]),
        })
        .unwrap();
    let msg = stream.next().await.expect("added msg").unwrap();
    let item = msg.tx.expect("tx set");
    assert_eq!(item.r#ref, vec![0x09; 32]);
    assert_eq!(item.native_bytes, vec![0x0A]);
    assert_eq!(item.stage, Stage::Mempool as i32);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn wait_for_tx_alpha_streams_mempool_stage() {
    use dugite_rpc::proto::v1alpha::submit::submit_service_client::SubmitServiceClient;
    use dugite_rpc::proto::v1alpha::submit::{Stage, WaitForTxRequest};

    let mock = Arc::new(SubmitMock::rejecting("unused"));
    let server = TestServer::start(mock).await;
    let mut client = SubmitServiceClient::new(server.channel().await);
    let watched = hash32(0x4B);
    let mut stream = client
        .wait_for_tx(WaitForTxRequest {
            r#ref: vec![watched.as_ref().to_vec()],
        })
        .await
        .unwrap()
        .into_inner();

    server
        .mempool_tx
        .send(MempoolEvent::Added {
            tx_hash: watched,
            raw_cbor: Some(vec![0x01]),
        })
        .unwrap();
    let msg = stream.next().await.expect("mempool stage").unwrap();
    assert_eq!(msg.r#ref, watched.as_ref().to_vec());
    assert_eq!(msg.stage, Stage::Mempool as i32);
    drop(stream);
    server.stop().await;
}
