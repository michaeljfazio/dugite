//! M2 integration tests for the QueryService implementation.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dugite_mempool::{Mempool, MempoolConfig};
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{OutputDatum, TransactionInput, TransactionOutput};
use dugite_primitives::value::{Lovelace, Value};
use dugite_primitives::Era;
use dugite_rpc::context::{EraHistoryView, GenesisView, ParamsView};
use dugite_rpc::{
    noop_metrics, LedgerContext, MempoolFeed, RawBlock, RawTx, RpcConfig, RpcError, RpcServer,
    SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use tokio::sync::watch;
use tonic::transport::Channel;

// ─── QueryMock ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct QueryMock {
    params: Arc<ProtocolParameters>,
    utxos: std::collections::HashMap<(TransactionHash, u32), TransactionOutput>,
    tip_slot: u64,
    tip_hash: [u8; 32],
    tip_block_no: u64,
}

#[async_trait]
impl LedgerContext for QueryMock {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        Ok(TipInfo {
            slot: self.tip_slot,
            hash: self.tip_hash,
            block_number: self.tip_block_no,
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
    async fn utxo_by_ref(&self, refs: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            if let Some(output) = self.utxos.get(&(r.transaction_id, r.index)) {
                out.push(UtxoSnapshot {
                    ref_: r.clone(),
                    output: output.clone(),
                    slot: None,
                });
            }
        }
        Ok(out)
    }
    async fn utxos_by_address(&self, _: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Err(RpcError::Unimplemented(""))
    }
    async fn utxos_by_payment_credential(&self, _: &Hash32) -> Result<Vec<UtxoSnapshot>, RpcError> {
        // QueryMock holds no payment-credential index; empty is the
        // correct "no matches" response for the supported-pattern path.
        Ok(Vec::new())
    }
    async fn utxos_by_asset(
        &self,
        _: &Hash32,
        _: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        Ok(Vec::new())
    }
    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        Ok(ParamsView {
            params: self.params.clone(),
            protocol_version_major: self.params.protocol_version_major,
        })
    }
    async fn era_history(&self) -> Result<EraHistoryView, RpcError> {
        Ok(EraHistoryView {
            summaries: vec![dugite_rpc::EraSummary {
                era: Era::Conway,
                first_slot: 0,
                slot_length_ms: 1_000,
                epoch_length_slots: 432_000,
            }],
        })
    }
    async fn genesis(&self) -> Result<GenesisView, RpcError> {
        Ok(GenesisView {
            network_magic: 764_824_073,
            system_start_unix: 1_596_059_091,
            security_param: 2_160,
        })
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
        keep: &(dyn for<'a> Fn(&'a UtxoSnapshot) -> bool + Send + Sync),
        cap: usize,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        let mut out = Vec::new();
        for ((tx_hash, idx), output) in &self.utxos {
            if out.len() >= cap {
                break;
            }
            let snap = UtxoSnapshot {
                ref_: TransactionInput {
                    transaction_id: *tx_hash,
                    index: *idx,
                },
                output: output.clone(),
                slot: None,
            };
            if keep(&snap) {
                out.push(snap);
            }
        }
        Ok(out)
    }
    async fn datum_by_hash(&self, _: &Hash32) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(None)
    }
    async fn tx_by_hash(&self, _: &TransactionHash) -> Result<Option<RawTx>, RpcError> {
        Ok(None)
    }
    async fn ledger_state(&self) -> Result<dugite_rpc::LedgerStateView, RpcError> {
        Ok(dugite_rpc::LedgerStateView {
            tip: TipInfo {
                slot: self.tip_slot,
                hash: self.tip_hash,
                block_number: self.tip_block_no,
                era: Era::Conway,
            },
            epoch: 0,
            slot_in_epoch: 0,
        })
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
}

impl TestServer {
    async fn start(mock: QueryMock) -> Self {
        let config = RpcConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            alpha_enabled: true,
            ..Default::default()
        };
        let tip_feed = TipFeed::new();
        let mempool = Arc::new(Mempool::new(MempoolConfig::default()));
        let mempool_feed = MempoolFeed::new(mempool.tx_events());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = RpcServer::start(
            Arc::new(config),
            Arc::new(mock),
            tip_feed,
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
        let _ = tokio::time::timeout(Duration::from_secs(3), self.join).await;
    }
}

fn make_params() -> ProtocolParameters {
    let mut p = ProtocolParameters::mainnet_defaults();
    p.min_fee_a = 44;
    p.min_fee_b = 155_381;
    p.max_tx_size = 16_384;
    p.max_block_body_size = 90_112;
    p.max_block_header_size = 1_100;
    p.key_deposit = Lovelace(2_000_000);
    p.pool_deposit = Lovelace(500_000_000);
    p.protocol_version_major = 10;
    p.protocol_version_minor = 0;
    p.drep_deposit = Lovelace(500_000_000);
    p.drep_activity = 20;
    p.gov_action_deposit = Lovelace(100_000_000_000);
    p
}

fn make_mock() -> QueryMock {
    QueryMock {
        params: Arc::new(make_params()),
        utxos: std::collections::HashMap::new(),
        tip_slot: 12_345,
        tip_hash: [0xAB; 32],
        tip_block_no: 99,
    }
}

// ─── ReadParams ──────────────────────────────────────────────────────────

#[tokio::test]
async fn read_params_v1beta_returns_mapped_protocol_params() {
    use dugite_rpc::proto::v1beta::cardano::big_int;
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::ReadParamsRequest;

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_params(ReadParamsRequest::default())
        .await
        .unwrap()
        .into_inner();

    let params_pb = resp.values.unwrap().params.unwrap();
    use dugite_rpc::proto::v1beta::query::any_chain_params::Params;
    let Params::Cardano(cardano) = params_pb;
    assert_eq!(cardano.max_tx_size, 16_384);
    assert_eq!(cardano.max_block_body_size, 90_112);
    let pv = cardano.protocol_version.expect("pv set");
    assert_eq!(pv.major, 10);
    let pool_dep = cardano.pool_deposit.expect("pool_deposit set");
    match pool_dep.big_int.unwrap() {
        big_int::BigInt::Int(v) => assert_eq!(v, 500_000_000),
        other => panic!("unexpected: {other:?}"),
    }
    let tip = resp.ledger_tip.unwrap();
    assert_eq!(tip.slot, 12_345);
    assert_eq!(tip.height, 99);
    server.stop().await;
}

#[tokio::test]
async fn read_params_v1alpha_subset_matches() {
    use dugite_rpc::proto::v1alpha::query::any_chain_params::Params;
    use dugite_rpc::proto::v1alpha::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1alpha::query::ReadParamsRequest;

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_params(ReadParamsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let Params::Cardano(cardano) = resp.values.unwrap().params.unwrap();
    assert_eq!(cardano.max_tx_size, 16_384);
    server.stop().await;
}

// ─── ReadUtxos ───────────────────────────────────────────────────────────

#[tokio::test]
async fn read_utxos_v1beta_returns_requested_outputs() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{ReadUtxosRequest, TxoRef};

    let mut mock = make_mock();
    let tx_hash = Hash32::from_bytes([7u8; 32]);
    mock.utxos.insert(
        (tx_hash, 0),
        TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![1, 2, 3],
            }),
            value: Value::lovelace(2_500_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        },
    );
    let server = TestServer::start(mock).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_utxos(ReadUtxosRequest {
            keys: vec![TxoRef {
                hash: tx_hash.as_ref().to_vec(),
                index: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.items.len(), 1);
    let item = &resp.items[0];
    let txo_ref = item.txo_ref.as_ref().unwrap();
    assert_eq!(txo_ref.hash, vec![7u8; 32]);
    assert_eq!(txo_ref.index, 0);
    server.stop().await;
}

#[tokio::test]
async fn read_utxos_unknown_ref_returns_empty_list() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{ReadUtxosRequest, TxoRef};

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_utxos(ReadUtxosRequest {
            keys: vec![TxoRef {
                hash: vec![0xFF; 32],
                index: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.items.is_empty());
    server.stop().await;
}

// ─── ReadGenesis ─────────────────────────────────────────────────────────

#[tokio::test]
async fn read_genesis_returns_cardano_envelope() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::read_genesis_response::Config;
    use dugite_rpc::proto::v1beta::query::ReadGenesisRequest;

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_genesis(ReadGenesisRequest::default())
        .await
        .unwrap()
        .into_inner();
    let Config::Cardano(cardano) = resp.config.unwrap();
    assert_eq!(cardano.network_magic, 764_824_073);
    assert_eq!(cardano.security_param, 2_160);
    server.stop().await;
}

// ─── ReadEraSummary ──────────────────────────────────────────────────────

#[tokio::test]
async fn read_era_summary_returns_summaries() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::read_era_summary_response::Summary;
    use dugite_rpc::proto::v1beta::query::ReadEraSummaryRequest;

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .read_era_summary(ReadEraSummaryRequest::default())
        .await
        .unwrap()
        .into_inner();
    let Summary::Cardano(summaries) = resp.summary.unwrap();
    assert!(!summaries.summaries.is_empty());
    server.stop().await;
}

// ─── SearchUtxos by exact address ─────────────────────────────────────────

#[tokio::test]
async fn search_utxos_empty_predicate_returns_unimplemented() {
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::SearchUtxosRequest;

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let status = client
        .search_utxos(SearchUtxosRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    server.stop().await;
}

#[tokio::test]
async fn search_utxos_payment_part_pattern_succeeds_with_empty_result() {
    use dugite_rpc::proto::v1beta::cardano::{AddressPattern, TxOutputPattern};
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{
        any_utxo_pattern, AnyUtxoPattern, SearchUtxosRequest, UtxoPredicate,
    };

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .search_utxos(SearchUtxosRequest {
            predicate: Some(UtxoPredicate {
                r#match: Some(AnyUtxoPattern {
                    utxo_pattern: Some(any_utxo_pattern::UtxoPattern::Cardano(TxOutputPattern {
                        address: Some(AddressPattern {
                            exact_address: None,
                            payment_part: Some(vec![0xAA; 28]),
                            delegation_part: None,
                        }),
                        asset: None,
                    })),
                }),
                not: vec![],
                all_of: vec![],
                any_of: vec![],
            }),
            field_mask: None,
            max_items: None,
            start_token: None,
        })
        .await
        .unwrap()
        .into_inner();
    // QueryMock has no UTxOs for this credential so the result is
    // empty, but the service signals it's a supported pattern by
    // returning OK rather than UNIMPLEMENTED.
    assert!(resp.items.is_empty());
    server.stop().await;
}

#[tokio::test]
async fn search_utxos_delegation_part_returns_empty_via_filter() {
    use dugite_rpc::proto::v1beta::cardano::{AddressPattern, TxOutputPattern};
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{
        any_utxo_pattern, AnyUtxoPattern, SearchUtxosRequest, UtxoPredicate,
    };

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    // QueryMock returns its in-memory UTxOs via `utxos_filter`; none
    // carry the requested stake credential, so the response is empty
    // (no longer UNIMPLEMENTED).
    let resp = client
        .search_utxos(SearchUtxosRequest {
            predicate: Some(UtxoPredicate {
                r#match: Some(AnyUtxoPattern {
                    utxo_pattern: Some(any_utxo_pattern::UtxoPattern::Cardano(TxOutputPattern {
                        address: Some(AddressPattern {
                            exact_address: None,
                            payment_part: None,
                            delegation_part: Some(vec![0xBB; 28]),
                        }),
                        asset: None,
                    })),
                }),
                not: vec![],
                all_of: vec![],
                any_of: vec![],
            }),
            field_mask: None,
            max_items: None,
            start_token: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.items.is_empty());
    server.stop().await;
}

#[tokio::test]
async fn search_utxos_asset_policy_returns_empty_when_no_matching_utxo() {
    use dugite_rpc::proto::v1beta::cardano::{AssetPattern, TxOutputPattern};
    use dugite_rpc::proto::v1beta::query::query_service_client::QueryServiceClient;
    use dugite_rpc::proto::v1beta::query::{
        any_utxo_pattern, AnyUtxoPattern, SearchUtxosRequest, UtxoPredicate,
    };

    let server = TestServer::start(make_mock()).await;
    let mut client = QueryServiceClient::new(server.channel().await);
    let resp = client
        .search_utxos(SearchUtxosRequest {
            predicate: Some(UtxoPredicate {
                r#match: Some(AnyUtxoPattern {
                    utxo_pattern: Some(any_utxo_pattern::UtxoPattern::Cardano(TxOutputPattern {
                        address: None,
                        asset: Some(AssetPattern {
                            policy_id: Some(vec![0xCC; 28]),
                            asset_name: None,
                        }),
                    })),
                }),
                not: vec![],
                all_of: vec![],
                any_of: vec![],
            }),
            field_mask: None,
            max_items: None,
            start_token: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.items.is_empty());
    server.stop().await;
}
