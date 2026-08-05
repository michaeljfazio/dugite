//! Integration tests for the real `WatchService` implementation
//! (`src/services/watch.rs`) — `WatchTx` on both `v1beta` and `v1alpha`.
//!
//! The service decodes each mempool `Added` event's raw CBOR as a
//! Conway transaction, applies the request's `TxPredicate`, and emits
//! `Apply` actions with the parsed proto Tx. These tests drive it with
//! real Conway CBOR produced by `dugite_serialization::encode_transaction`
//! — the fixture builder asserts the bytes decode up front so a broken
//! fixture fails instantly instead of hanging the stream await.
//!
//! Event-driven only: events are fired after the stream is established
//! (the handler subscribes before returning), and "must be skipped"
//! cases are proven by firing a passing sentinel afterwards and
//! asserting the sentinel is the FIRST message received.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dugite_primitives::address::{Address, EnterpriseAddress};
use dugite_primitives::block::Point;
use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32, TransactionHash};
use dugite_primitives::network::NetworkId;
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};
use dugite_primitives::Era;
use dugite_rpc::context::{EraHistoryView, GenesisView, ParamsView};
use dugite_rpc::proto::v1beta;
use dugite_rpc::{
    noop_metrics, LedgerContext, MempoolEvent, MempoolFeed, RawBlock, RawTx, RpcConfig, RpcError,
    RpcServer, SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use tokio::sync::{broadcast, watch};
use tokio_stream::StreamExt;
use tonic::transport::Channel;

// ─── Minimal context (WatchTx never touches the ledger) ──────────────────

struct WatchMock;

#[async_trait]
impl LedgerContext for WatchMock {
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
    async fn submit_tx(&self, _: u16, _: &[u8]) -> SubmitOutcome {
        SubmitOutcome::Rejected { reason: "".into() }
    }
    async fn eval_tx(&self, _: u16, _: &[u8]) -> dugite_rpc::EvalOutcome {
        dugite_rpc::EvalOutcome {
            fee: 0,
            error: Some("".into()),
            redeemers: Vec::new(),
        }
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
        Err(RpcError::Unimplemented(""))
    }
    async fn mempool_contains(&self, _: &TransactionHash) -> bool {
        false
    }
}

// ─── Scaffold ────────────────────────────────────────────────────────────

struct TestServer {
    addr: std::net::SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    mempool_tx: broadcast::Sender<MempoolEvent>,
}

impl TestServer {
    async fn start() -> Self {
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
            Arc::new(WatchMock),
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

    fn added(&self, hash_byte: u8, cbor: Option<Vec<u8>>) {
        self.mempool_tx
            .send(MempoolEvent::Added {
                tx_hash: Hash32::from_bytes([hash_byte; 32]),
                raw_cbor: cbor,
            })
            .expect("subscriber alive");
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

// ─── Conway tx fixtures ──────────────────────────────────────────────────

fn addr_with_payment(byte: u8) -> Address {
    Address::Enterprise(EnterpriseAddress {
        network: NetworkId::Mainnet,
        payment: Credential::VerificationKey(Hash28::from_bytes([byte; 28])),
    })
}

fn empty_witness_set() -> TransactionWitnessSet {
    TransactionWitnessSet {
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
    }
}

fn conway_body(fee: u64, payment_byte: u8) -> TransactionBody {
    TransactionBody {
        inputs: vec![TransactionInput {
            transaction_id: Hash32::ZERO,
            index: 0,
        }],
        outputs: vec![TransactionOutput {
            address: addr_with_payment(payment_byte),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }],
        fee: Lovelace(fee),
        ttl: Some(SlotNo(999_999)),
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
        direct_deposits: BTreeMap::new(),
        guards: Vec::new(),
    }
}

/// Encode a minimal Conway tx paying `fee` to an enterprise address whose
/// payment credential is `[payment_byte; 28]`. Asserts the bytes decode
/// through the same path the service uses, so a fixture regression fails
/// the test immediately instead of silently starving the stream.
fn conway_tx_cbor(fee: u64, payment_byte: u8) -> Vec<u8> {
    conway_tx_cbor_with(fee, payment_byte, |_| {})
}

fn conway_tx_cbor_with(
    fee: u64,
    payment_byte: u8,
    customize: impl FnOnce(&mut TransactionBody),
) -> Vec<u8> {
    let mut body = conway_body(fee, payment_byte);
    customize(&mut body);
    let tx = Transaction {
        hash: Hash32::ZERO,
        era: Era::Conway,
        body,
        witness_set: empty_witness_set(),
        is_valid: true,
        auxiliary_data: None,
        raw_cbor: None,
        raw_body_cbor: None,
        raw_witness_cbor: None,
    };
    let cbor = dugite_serialization::encode::encode_transaction(&tx);
    dugite_serialization::decode_transaction(6, &cbor)
        .expect("fixture must decode as a Conway transaction");
    cbor
}

/// Extract the parsed Cardano Tx from a v1beta WatchTxResponse Apply action.
fn apply_tx(msg: v1beta::watch::WatchTxResponse) -> v1beta::cardano::Tx {
    match msg.action.expect("action set") {
        v1beta::watch::watch_tx_response::Action::Apply(any) => match any.chain.expect("chain") {
            v1beta::watch::any_chain_tx::Chain::Cardano(tx) => tx,
        },
        other => panic!("expected Apply action, got {other:?}"),
    }
}

fn fee_of(tx: &v1beta::cardano::Tx) -> i64 {
    match tx
        .fee
        .as_ref()
        .expect("fee set")
        .big_int
        .as_ref()
        .expect("big_int set")
    {
        v1beta::cardano::big_int::BigInt::Int(v) => *v,
        other => panic!("unexpected fee variant: {other:?}"),
    }
}

/// Build a v1beta TxPredicate with a `has_address(payment_part)` leaf.
fn payment_part_predicate(payment_byte: u8) -> v1beta::watch::TxPredicate {
    v1beta::watch::TxPredicate {
        r#match: Some(v1beta::watch::AnyChainTxPattern {
            chain: Some(v1beta::watch::any_chain_tx_pattern::Chain::Cardano(
                v1beta::cardano::TxPattern {
                    has_address: Some(v1beta::cardano::AddressPattern {
                        exact_address: None,
                        payment_part: Some(vec![payment_byte; 28]),
                        delegation_part: None,
                    }),
                    ..Default::default()
                },
            )),
        }),
        not: vec![],
        all_of: vec![],
        any_of: vec![],
    }
}

/// Build a `TxPredicate` naming `consumes` — a leaf `matches_tx_predicate`
/// cannot evaluate (needs resolved-input UTxO data unavailable on the
/// mempool-only watch path).
fn consumes_predicate() -> v1beta::watch::TxPredicate {
    v1beta::watch::TxPredicate {
        r#match: Some(v1beta::watch::AnyChainTxPattern {
            chain: Some(v1beta::watch::any_chain_tx_pattern::Chain::Cardano(
                v1beta::cardano::TxPattern {
                    consumes: Some(v1beta::cardano::TxOutputPattern::default()),
                    ..Default::default()
                },
            )),
        }),
        not: vec![],
        all_of: vec![],
        any_of: vec![],
    }
}

/// A `WatchTx` request naming an unsupported `TxPattern` leaf
/// (`consumes`) must be REJECTED outright, not silently accepted and
/// under-filtered (a subscriber asking to watch for spends of a
/// specific input has no way to know its filter was quietly ignored).
#[tokio::test]
async fn watch_tx_rejects_predicate_naming_consumes() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let status = client
        .watch_tx(WatchTxRequest {
            predicate: Some(consumes_predicate()),
            field_mask: None,
            intersect: vec![],
        })
        .await
        .expect_err("a `consumes` predicate must be rejected, not silently accepted");
    assert_eq!(status.code(), tonic::Code::Unimplemented);

    server.stop().await;
}

/// Same rejection, nested under `all_of` — proves a client can't smuggle
/// the unsupported leaf past a combinator to bypass the guard.
#[tokio::test]
async fn watch_tx_rejects_consumes_nested_under_all_of() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let predicate = v1beta::watch::TxPredicate {
        all_of: vec![consumes_predicate(), payment_part_predicate(0x11)],
        ..Default::default()
    };
    let status = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            field_mask: None,
            intersect: vec![],
        })
        .await
        .expect_err("nested `consumes` under all_of must still be rejected");
    assert_eq!(status.code(), tonic::Code::Unimplemented);

    server.stop().await;
}

// ─── WatchTx v1beta ──────────────────────────────────────────────────────

#[tokio::test]
async fn watch_tx_streams_apply_event_with_parsed_tx() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    server.added(0x01, Some(conway_tx_cbor(171_111, 0x11)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    let tx = apply_tx(msg);
    assert_eq!(fee_of(&tx), 171_111);
    assert_eq!(tx.inputs.len(), 1);
    assert_eq!(tx.outputs.len(), 1);
    assert!(tx.successful, "is_valid=true must map to successful");
    assert_eq!(tx.hash.len(), 32, "decoded tx hash must be present");
    assert_ne!(tx.hash, vec![0u8; 32], "hash is computed from body bytes");

    drop(stream);
    server.stop().await;
}

/// FieldMask (issue #1004): a mask naming only `action` (the outer
/// oneof field, itself untouchable further without descending into the
/// Cardano `Tx` type — proto3 oneofs are not addressable by field name
/// the way a plain message is) proves the mask reaches every streamed
/// `WatchTxResponse`. Masking down INTO the tx body is exercised by
/// `masking.rs`'s own unit tests + the sync-service integration tests
/// (`fetch_block_field_mask_prunes_repeated_block_elements`); this test
/// is about the wiring being live on this particular stream.
#[tokio::test]
async fn watch_tx_field_mask_prunes_streamed_responses() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: None,
            field_mask: Some(prost_types::FieldMask {
                paths: vec!["bogus_field_name".to_string()],
            }),
            intersect: vec![],
        })
        .await
        .unwrap()
        .into_inner();

    server.added(0x01, Some(conway_tx_cbor(171_111, 0x11)));

    let msg = stream.next().await.expect("msg").unwrap();
    assert!(
        msg.action.is_none(),
        "the only real field (`action`) must be pruned when the mask names \
         something else entirely — proves the mask reached this stream, not \
         just the isolated masking::apply function"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_skips_undecodable_cbor_then_delivers_next_valid() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    // Garbage CBOR must be skipped (logged, stream stays alive)…
    server.added(0x01, Some(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    // …and the next valid tx must still flow.
    server.added(0x02, Some(conway_tx_cbor(222_222, 0x11)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    let tx = apply_tx(msg);
    assert_eq!(
        fee_of(&tx),
        222_222,
        "first delivered tx must be the valid sentinel, not the garbage event"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_skips_events_lacking_raw_cbor_and_removals() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    // Added with no raw bytes → skipped.
    server.added(0x01, None);
    // Removed → skipped.
    server
        .mempool_tx
        .send(MempoolEvent::Removed {
            tx_hash: Hash32::from_bytes([0x01; 32]),
            reason: dugite_rpc::MempoolRemoveReason::Mined,
        })
        .unwrap();
    // Sentinel.
    server.added(0x02, Some(conway_tx_cbor(333_333, 0x11)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(fee_of(&apply_tx(msg)), 333_333);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_predicate_filters_on_payment_part() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(payment_part_predicate(0x11)),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // Pays to credential 0x22 → filtered out.
    server.added(0x01, Some(conway_tx_cbor(111_111, 0x22)));
    // Pays to credential 0x11 → must be the first delivered message.
    server.added(0x02, Some(conway_tx_cbor(444_444, 0x11)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(
        fee_of(&apply_tx(msg)),
        444_444,
        "non-matching tx must be filtered by the predicate"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_predicate_not_combinator_excludes_matching_txs() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    // not: [has_address 0x11] — matches every tx EXCEPT ones paying 0x11.
    let predicate = v1beta::watch::TxPredicate {
        not: vec![payment_part_predicate(0x11)],
        ..Default::default()
    };
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // Pays 0x11 → excluded by `not`.
    server.added(0x01, Some(conway_tx_cbor(111_111, 0x11)));
    // Pays 0x22 → passes.
    server.added(0x02, Some(conway_tx_cbor(555_555, 0x22)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(fee_of(&apply_tx(msg)), 555_555);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_mints_asset_pattern_matches_minting_tx_only() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let predicate = v1beta::watch::TxPredicate {
        r#match: Some(v1beta::watch::AnyChainTxPattern {
            chain: Some(v1beta::watch::any_chain_tx_pattern::Chain::Cardano(
                v1beta::cardano::TxPattern {
                    mints_asset: Some(v1beta::cardano::AssetPattern {
                        policy_id: Some(vec![0xAB; 28]),
                        asset_name: None,
                    }),
                    ..Default::default()
                },
            )),
        }),
        ..Default::default()
    };
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // No mint → filtered.
    server.added(0x01, Some(conway_tx_cbor(111_111, 0x11)));
    // Mints under policy 0xAB → passes.
    let minting = conway_tx_cbor_with(666_666, 0x11, |body| {
        let mut assets = BTreeMap::new();
        assets.insert(AssetName::new(vec![0x01]).unwrap(), 5i64);
        body.mint.insert(Hash28::from_bytes([0xAB; 28]), assets);
    });
    server.added(0x02, Some(minting));

    let msg = stream.next().await.expect("apply msg").unwrap();
    let tx = apply_tx(msg);
    assert_eq!(fee_of(&tx), 666_666);
    assert_eq!(tx.mint.len(), 1, "mint must survive the proto mapping");

    drop(stream);
    server.stop().await;
}

// ─── WatchTx v1alpha ─────────────────────────────────────────────────────

#[tokio::test]
async fn watch_tx_alpha_recodes_predicate_and_streams_parsed_tx() {
    use dugite_rpc::proto::v1alpha;
    use v1alpha::watch::watch_service_client::WatchServiceClient;
    use v1alpha::watch::WatchTxRequest;

    let server = TestServer::start().await;
    let mut client = WatchServiceClient::new(server.channel().await);

    // Same has_address(payment_part 0x11) predicate in v1alpha shape —
    // the service re-encodes it to v1beta internally.
    let predicate = v1alpha::watch::TxPredicate {
        r#match: Some(v1alpha::watch::AnyChainTxPattern {
            chain: Some(v1alpha::watch::any_chain_tx_pattern::Chain::Cardano(
                v1alpha::cardano::TxPattern {
                    // v1alpha AddressPattern uses plain (non-optional)
                    // bytes fields; empty = unset after prost re-encode.
                    has_address: Some(v1alpha::cardano::AddressPattern {
                        exact_address: Vec::new(),
                        payment_part: vec![0x11; 28],
                        delegation_part: Vec::new(),
                    }),
                    ..Default::default()
                },
            )),
        }),
        ..Default::default()
    };
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // Filtered (pays 0x22), then matching (pays 0x11).
    server.added(0x01, Some(conway_tx_cbor(111_111, 0x22)));
    server.added(0x02, Some(conway_tx_cbor(777_777, 0x11)));

    let msg = stream.next().await.expect("apply msg").unwrap();
    let tx = match msg.action.expect("action") {
        v1alpha::watch::watch_tx_response::Action::Apply(any) => match any.chain.expect("chain") {
            v1alpha::watch::any_chain_tx::Chain::Cardano(tx) => tx,
        },
        other => panic!("expected Apply, got {other:?}"),
    };
    let fee = match tx
        .fee
        .as_ref()
        .expect("fee")
        .big_int
        .as_ref()
        .expect("big_int")
    {
        v1alpha::cardano::big_int::BigInt::Int(v) => *v,
        other => panic!("unexpected fee variant: {other:?}"),
    };
    assert_eq!(
        fee, 777_777,
        "alpha predicate recode must filter identically"
    );
    assert_eq!(tx.outputs.len(), 1);

    drop(stream);
    server.stop().await;
}
