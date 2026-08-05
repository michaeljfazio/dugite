//! Integration tests for the real `WatchService` implementation
//! (`src/services/watch.rs`) — `WatchTx` on both `v1beta` and `v1alpha`.
//!
//! Issue #1007: `WatchTx` is chain-sourced (via `TipFeed`), not
//! mempool-sourced. The mock (`WatchMock`) resolves blocks by hash from
//! an in-memory map, and tests drive the stream by firing `TipInfo`
//! apply / `TipRollback` events through `TipFeed`'s publisher — the same
//! pattern `tests/sync_service.rs` uses for `FollowTip`, since `WatchTx`
//! now shares that source.
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
use dugite_primitives::block::{
    Block, BlockHeader, OperationalCert, Point, ProtocolVersion, VrfOutput,
};
use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32, TransactionHash};
use dugite_primitives::network::NetworkId;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};
use dugite_primitives::Era;
use dugite_rpc::context::{EraHistoryView, GenesisView, ParamsView};
use dugite_rpc::proto::v1beta;
use dugite_rpc::{
    noop_metrics, LedgerContext, MempoolFeed, RawBlock, RawTx, RpcConfig, RpcError, RpcServer,
    SubmitOutcome, TipFeed, TipInfo, TipRollback, UtxoSnapshot,
};
use tokio::sync::{broadcast, watch};
use tokio_stream::StreamExt;
use tonic::transport::Channel;

// ─── Mock — resolves blocks registered up front, everything else stays
// out of scope for WatchTx ──────────────────────────────────────────────

struct WatchMock {
    by_hash: BTreeMap<[u8; 32], RawBlock>,
}

impl WatchMock {
    fn new(blocks: Vec<RawBlock>) -> Self {
        Self {
            by_hash: blocks.into_iter().map(|b| (b.hash, b)).collect(),
        }
    }
}

#[async_trait]
impl LedgerContext for WatchMock {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Err(RpcError::Unimplemented("mock::tip"))
    }
    async fn block_by_hash(&self, hash: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(hash.as_ref());
        Ok(self.by_hash.get(&arr).cloned())
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
    tip_feed: TipFeed,
}

impl TestServer {
    async fn start(mock: WatchMock) -> Self {
        let config = RpcConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            alpha_enabled: true,
            ..Default::default()
        };
        let (mempool_tx, _keepalive) = broadcast::channel(64);
        let mempool_feed = MempoolFeed::new(mempool_tx);
        let tip_feed = TipFeed::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = RpcServer::start(
            Arc::new(config),
            Arc::new(mock),
            tip_feed.clone(),
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
            tip_feed,
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

    /// Fire a `TipInfo` apply event for a block previously registered
    /// with the mock.
    fn apply(&self, raw: &RawBlock) {
        self.tip_feed.publisher().announce_apply(TipInfo {
            slot: raw.slot,
            hash: raw.hash,
            block_number: raw.block_number,
            era: raw.era,
        });
    }

    fn rollback(&self, slot: u64, hash: [u8; 32]) {
        self.tip_feed
            .publisher()
            .announce_rollback(TipRollback { slot, hash });
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

// ─── Conway block/tx fixtures ──────────────────────────────────────────────

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

fn conway_tx(fee: u64, payment_byte: u8) -> Transaction {
    conway_tx_with(fee, payment_byte, |_| {})
}

fn conway_tx_with(
    fee: u64,
    payment_byte: u8,
    customize: impl FnOnce(&mut TransactionBody),
) -> Transaction {
    let mut body = conway_body(fee, payment_byte);
    customize(&mut body);
    Transaction {
        hash: Hash32::ZERO,
        era: Era::Conway,
        body,
        witness_set: empty_witness_set(),
        is_valid: true,
        auxiliary_data: None,
        raw_cbor: None,
        raw_body_cbor: None,
        raw_witness_cbor: None,
    }
}

/// Minimal-but-decodable header (all-zero VRF/KES/opcert, matching the
/// pattern `dugite-serialization`'s own encode tests use) with the
/// slot/block_number a test needs.
fn minimal_header(slot: u64, block_no: u64) -> BlockHeader {
    BlockHeader {
        header_hash: Hash32::ZERO,
        prev_hash: Hash32::from_bytes([1u8; 32]),
        issuer_vkey: vec![0u8; 32],
        vrf_vkey: vec![0u8; 32],
        vrf_result: VrfOutput {
            output: vec![0u8; 64],
            proof: vec![0u8; 80],
        },
        nonce_vrf_output: vec![],
        nonce_vrf_proof: vec![],
        prev_nonce: None,
        raw_header_body: None,
        block_number: BlockNo(block_no),
        slot: SlotNo(slot),
        epoch_nonce: Hash32::ZERO,
        body_size: 256,
        body_hash: Hash32::ZERO,
        operational_cert: OperationalCert {
            hot_vkey: vec![0u8; 32],
            sequence_number: 0,
            kes_period: 100,
            sigma: vec![0u8; 64],
        },
        protocol_version: ProtocolVersion { major: 9, minor: 0 },
        kes_signature: vec![],
    }
}

/// Build + register a decodable Conway block carrying `txs`, at
/// `(slot, block_no, hash_byte)`. Asserts the bytes decode with exactly
/// `txs.len()` transactions up front, so a fixture regression fails the
/// test immediately instead of silently starving the stream.
fn conway_block(slot: u64, block_no: u64, hash_byte: u8, txs: Vec<Transaction>) -> RawBlock {
    let expected_tx_count = txs.len();
    let block = Block {
        header: minimal_header(slot, block_no),
        transactions: txs,
        era: Era::Conway,
        raw_cbor: None,
    };
    let cbor = dugite_serialization::encode::encode_block(&block, &[]);
    let decoded =
        dugite_serialization::decode::decode_block(&cbor).expect("fixture block must decode");
    assert_eq!(decoded.transactions.len(), expected_tx_count);
    RawBlock {
        slot,
        hash: [hash_byte; 32],
        block_number: block_no,
        era: Era::Conway,
        cbor,
    }
}

/// A block that will fail to decode (`decode_block` rejects it) —
/// exercises the "resolvable but undecodable" skip path.
fn undecodable_block(slot: u64, block_no: u64, hash_byte: u8) -> RawBlock {
    RawBlock {
        slot,
        hash: [hash_byte; 32],
        block_number: block_no,
        era: Era::Conway,
        cbor: vec![0xDE, 0xAD, 0xBE, 0xEF],
    }
}

/// Extract the parsed Cardano Tx + block presence from a v1beta
/// WatchTxResponse Apply action.
fn apply_tx(msg: v1beta::watch::WatchTxResponse) -> v1beta::watch::AnyChainTx {
    match msg.action.expect("action set") {
        v1beta::watch::watch_tx_response::Action::Apply(any) => any,
        other => panic!("expected Apply action, got {other:?}"),
    }
}

fn undo_tx(msg: v1beta::watch::WatchTxResponse) -> v1beta::watch::AnyChainTx {
    match msg.action.expect("action set") {
        v1beta::watch::watch_tx_response::Action::Undo(any) => any,
        other => panic!("expected Undo action, got {other:?}"),
    }
}

fn cardano_tx(item: &v1beta::watch::AnyChainTx) -> &v1beta::cardano::Tx {
    match item.chain.as_ref().expect("chain") {
        v1beta::watch::any_chain_tx::Chain::Cardano(tx) => tx,
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
/// chain-sourced watch path).
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

// ─── Reject guard (issue #1004) ─────────────────────────────────────────

/// A `WatchTx` request naming an unsupported `TxPattern` leaf
/// (`consumes`) must be REJECTED outright, not silently accepted and
/// under-filtered (a subscriber asking to watch for spends of a
/// specific input has no way to know its filter was quietly ignored).
#[tokio::test]
async fn watch_tx_rejects_predicate_naming_consumes() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let server = TestServer::start(WatchMock::new(vec![])).await;
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

    let server = TestServer::start(WatchMock::new(vec![])).await;
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
async fn watch_tx_streams_apply_event_with_parsed_tx_and_block() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let block = conway_block(100, 1, 0x01, vec![conway_tx(171_111, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![block.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    server.apply(&block);

    let msg = stream.next().await.expect("apply msg").unwrap();
    let item = apply_tx(msg);
    let tx = cardano_tx(&item);
    assert_eq!(fee_of(tx), 171_111);
    assert_eq!(tx.inputs.len(), 1);
    assert_eq!(tx.outputs.len(), 1);
    assert!(tx.successful, "is_valid=true must map to successful");
    assert_eq!(tx.hash.len(), 32, "decoded tx hash must be present");
    assert_ne!(tx.hash, vec![0u8; 32], "hash is computed from body bytes");

    // Issue #1007: block must be populated — the whole point of sourcing
    // from confirmed blocks instead of the mempool.
    let block_env = item.block.expect("AnyChainTx.block must be populated");
    match block_env.chain.expect("block.chain set") {
        v1beta::watch::any_chain_block::Chain::Cardano(b) => {
            assert_eq!(b.header.expect("header").slot, 100);
        }
    }

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

    let block = conway_block(100, 1, 0x01, vec![conway_tx(171_111, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![block.clone()])).await;
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

    server.apply(&block);

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
async fn watch_tx_skips_undecodable_block_then_delivers_next_valid() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let bad = undecodable_block(100, 1, 0x01);
    let good = conway_block(200, 2, 0x02, vec![conway_tx(222_222, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![bad.clone(), good.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    // Undecodable block must be skipped (logged, stream stays alive)…
    server.apply(&bad);
    // …and the next valid block must still flow.
    server.apply(&good);

    let msg = stream.next().await.expect("apply msg").unwrap();
    let item = apply_tx(msg);
    let tx = cardano_tx(&item);
    assert_eq!(
        fee_of(tx),
        222_222,
        "first delivered tx must be from the valid block, not the undecodable one"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_skips_unresolvable_block_hash() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    // Not registered with the mock at all — block_by_hash returns None.
    let unresolvable_hash = [0xFFu8; 32];
    let good = conway_block(200, 2, 0x02, vec![conway_tx(333_333, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![good.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest::default())
        .await
        .unwrap()
        .into_inner();

    server.tip_feed.publisher().announce_apply(TipInfo {
        slot: 100,
        hash: unresolvable_hash,
        block_number: 1,
        era: Era::Conway,
    });
    server.apply(&good);

    let msg = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(fee_of(cardano_tx(&apply_tx(msg))), 333_333);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_emits_idle_for_block_with_no_matching_tx() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let block = conway_block(100, 1, 0x01, vec![conway_tx(111_111, 0x22)]);
    let server = TestServer::start(WatchMock::new(vec![block.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(payment_part_predicate(0x11)), // block pays 0x22 -> no match
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    server.apply(&block);

    let msg = stream.next().await.expect("idle msg").unwrap();
    match msg.action.expect("action") {
        v1beta::watch::watch_tx_response::Action::Idle(block_ref) => {
            assert_eq!(block_ref.slot, 100);
            assert_eq!(block_ref.height, 1);
        }
        other => panic!("expected Idle, got {other:?}"),
    }

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_emits_undo_on_rollback_reusing_the_apply_envelope() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let block = conway_block(100, 1, 0x01, vec![conway_tx(444_444, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![block.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(payment_part_predicate(0x11)),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    server.apply(&block);
    let applied = stream.next().await.expect("apply msg").unwrap();
    let applied_item = apply_tx(applied);
    assert_eq!(fee_of(cardano_tx(&applied_item)), 444_444);
    assert!(applied_item.block.is_some());

    // Roll back to before this block's slot -> must undo it.
    server.rollback(50, [0x00; 32]);

    let undone = stream.next().await.expect("undo msg").unwrap();
    let undone_item = undo_tx(undone);
    assert_eq!(
        fee_of(cardano_tx(&undone_item)),
        444_444,
        "undo must re-emit the SAME tx that was applied"
    );
    assert!(
        undone_item.block.is_some(),
        "undo carries the block the tx came from, same as apply"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_rollback_at_or_above_applied_slot_does_not_undo() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let block = conway_block(100, 1, 0x01, vec![conway_tx(555_555, 0x11)]);
    let sentinel = conway_block(200, 2, 0x02, vec![conway_tx(999_999, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![block.clone(), sentinel.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(payment_part_predicate(0x11)),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    server.apply(&block);
    let _ = stream.next().await.expect("apply msg").unwrap();

    // Rollback exactly AT the applied slot: the entry's slot (100) is not
    // STRICTLY greater than the rollback point (100), so it must survive.
    server.rollback(100, [0x00; 32]);
    // Prove nothing was undone by having a sentinel apply arrive next and
    // be the very next message — no undo interleaved.
    server.apply(&sentinel);

    let msg = stream.next().await.expect("sentinel msg").unwrap();
    match msg.action.expect("action") {
        v1beta::watch::watch_tx_response::Action::Apply(item) => {
            assert_eq!(fee_of(cardano_tx(&item)), 999_999);
        }
        other => panic!("expected Apply (no undo should have fired), got {other:?}"),
    }

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_predicate_filters_on_payment_part() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

    let filtered = conway_block(100, 1, 0x01, vec![conway_tx(111_111, 0x22)]);
    let matching = conway_block(200, 2, 0x02, vec![conway_tx(444_444, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![filtered.clone(), matching.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(payment_part_predicate(0x11)),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // Pays to credential 0x22 -> idle (no apply).
    server.apply(&filtered);
    // Pays to credential 0x11 -> must be the first Apply delivered.
    server.apply(&matching);

    // First message is the idle signal for the filtered block…
    let first = stream.next().await.expect("idle msg").unwrap();
    assert!(matches!(
        first.action,
        Some(v1beta::watch::watch_tx_response::Action::Idle(_))
    ));
    // …then the matching apply.
    let second = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(
        fee_of(cardano_tx(&apply_tx(second))),
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

    // not: [has_address 0x11] — matches every tx EXCEPT ones paying 0x11.
    let predicate = v1beta::watch::TxPredicate {
        not: vec![payment_part_predicate(0x11)],
        ..Default::default()
    };
    let excluded = conway_block(100, 1, 0x01, vec![conway_tx(111_111, 0x11)]);
    let passes = conway_block(200, 2, 0x02, vec![conway_tx(555_555, 0x22)]);
    let server = TestServer::start(WatchMock::new(vec![excluded.clone(), passes.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    // Pays 0x11 -> excluded by `not` -> idle.
    server.apply(&excluded);
    // Pays 0x22 -> passes.
    server.apply(&passes);

    let first = stream.next().await.expect("idle msg").unwrap();
    assert!(matches!(
        first.action,
        Some(v1beta::watch::watch_tx_response::Action::Idle(_))
    ));
    let second = stream.next().await.expect("apply msg").unwrap();
    assert_eq!(fee_of(cardano_tx(&apply_tx(second))), 555_555);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn watch_tx_mints_asset_pattern_matches_minting_tx_only() {
    use v1beta::watch::watch_service_client::WatchServiceClient;
    use v1beta::watch::WatchTxRequest;

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

    let no_mint = conway_block(100, 1, 0x01, vec![conway_tx(111_111, 0x11)]);
    let minting_tx = conway_tx_with(666_666, 0x11, |body| {
        let mut assets = BTreeMap::new();
        assets.insert(AssetName::new(vec![0x01]).unwrap(), 5i64);
        body.mint.insert(Hash28::from_bytes([0xAB; 28]), assets);
    });
    let minting = conway_block(200, 2, 0x02, vec![minting_tx]);
    let server = TestServer::start(WatchMock::new(vec![no_mint.clone(), minting.clone()])).await;
    let mut client = WatchServiceClient::new(server.channel().await);
    let mut stream = client
        .watch_tx(WatchTxRequest {
            predicate: Some(predicate),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    server.apply(&no_mint);
    server.apply(&minting);

    let first = stream.next().await.expect("idle msg").unwrap();
    assert!(matches!(
        first.action,
        Some(v1beta::watch::watch_tx_response::Action::Idle(_))
    ));
    let second = stream.next().await.expect("apply msg").unwrap();
    let item = apply_tx(second);
    let tx = cardano_tx(&item);
    assert_eq!(fee_of(tx), 666_666);
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

    let filtered = conway_block(100, 1, 0x01, vec![conway_tx(111_111, 0x22)]);
    let matching = conway_block(200, 2, 0x02, vec![conway_tx(777_777, 0x11)]);
    let server = TestServer::start(WatchMock::new(vec![filtered.clone(), matching.clone()])).await;
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

    // Filtered (pays 0x22) -> idle, then matching (pays 0x11) -> apply.
    server.apply(&filtered);
    server.apply(&matching);

    let first = stream.next().await.expect("idle msg").unwrap();
    assert!(matches!(
        first.action,
        Some(v1alpha::watch::watch_tx_response::Action::Idle(_))
    ));

    let second = stream.next().await.expect("apply msg").unwrap();
    let item = match second.action.expect("action") {
        v1alpha::watch::watch_tx_response::Action::Apply(any) => any,
        other => panic!("expected Apply, got {other:?}"),
    };
    let v1alpha::watch::any_chain_tx::Chain::Cardano(tx) = item.chain.expect("chain");
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
    assert!(
        item.block.is_some(),
        "block must round-trip through the alpha recode too"
    );

    drop(stream);
    server.stop().await;
}
