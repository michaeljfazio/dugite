//! M1.B integration tests for the real `SyncService` implementation.
//!
//! Backs the server with a `SyncMock` that holds a small in-memory chain
//! of synthetic blocks (CBOR-shaped enough that `decode_block_minimal`
//! succeeds wouldn't be necessary — the service relies on
//! `LedgerContext::block_by_hash` etc., not on re-parsing inside the
//! service). Each test exercises one method end-to-end on a real gRPC
//! wire.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dugite_mempool::{Mempool, MempoolConfig};
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::transaction::TransactionInput;
use dugite_primitives::Era;
use dugite_rpc::context::{EraHistoryView, GenesisView, ParamsView};
use dugite_rpc::{
    noop_metrics, LedgerContext, MempoolFeed, RawBlock, RawTx, RpcConfig, RpcError, RpcServer,
    SubmitOutcome, TipFeed, TipInfo, UtxoSnapshot,
};
use tokio::sync::watch;
use tonic::transport::Channel;

// ─── SyncMock — minimal in-memory chain ──────────────────────────────────

#[derive(Clone)]
struct ChainEntry {
    slot: u64,
    hash: [u8; 32],
    block_no: u64,
    cbor: Vec<u8>, // can be empty for the M1.B tests; clients only test metadata
}

struct SyncMock {
    blocks: BTreeMap<u64, ChainEntry>, // slot → entry
    by_hash: BTreeMap<[u8; 32], ChainEntry>,
}

impl SyncMock {
    fn new(entries: Vec<ChainEntry>) -> Self {
        let mut blocks = BTreeMap::new();
        let mut by_hash = BTreeMap::new();
        for e in entries {
            blocks.insert(e.slot, e.clone());
            by_hash.insert(e.hash, e);
        }
        Self { blocks, by_hash }
    }

    fn tip_entry(&self) -> Option<&ChainEntry> {
        self.blocks.values().next_back()
    }
}

#[async_trait]
impl LedgerContext for SyncMock {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        match self.tip_entry() {
            Some(e) => Ok(TipInfo {
                slot: e.slot,
                hash: e.hash,
                block_number: e.block_no,
                era: Era::Conway,
            }),
            None => Err(RpcError::NotFound("empty chain".into())),
        }
    }

    async fn block_by_hash(&self, hash: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(hash.as_ref());
        Ok(self.by_hash.get(&arr).map(|e| RawBlock {
            slot: e.slot,
            hash: e.hash,
            block_number: e.block_no,
            era: Era::Conway,
            cbor: e.cbor.clone(),
        }))
    }

    async fn block_at_slot(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        Ok(self.blocks.get(&slot).map(|e| RawBlock {
            slot: e.slot,
            hash: e.hash,
            block_number: e.block_no,
            era: Era::Conway,
            cbor: e.cbor.clone(),
        }))
    }

    async fn block_after(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        Ok(self
            .blocks
            .range((std::ops::Bound::Excluded(slot), std::ops::Bound::Unbounded))
            .next()
            .map(|(_, e)| RawBlock {
                slot: e.slot,
                hash: e.hash,
                block_number: e.block_no,
                era: Era::Conway,
                cbor: e.cbor.clone(),
            }))
    }

    async fn intersect(&self, points: &[Point]) -> Result<Option<Point>, RpcError> {
        // Pick the latest-slot supplied point that exists in our chain.
        let mut sorted: Vec<&Point> = points.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.slot().map(|s| s.0).unwrap_or(0)));
        for p in sorted {
            match p {
                Point::Origin => return Ok(Some(Point::Origin)),
                Point::Specific(_, hash) => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(hash.as_ref());
                    if self.by_hash.contains_key(&arr) {
                        return Ok(Some(p.clone()));
                    }
                }
            }
        }
        Ok(None)
    }

    async fn blocks_range(
        &self,
        from: u64,
        to: u64,
        limit: usize,
    ) -> Result<Vec<RawBlock>, RpcError> {
        Ok(self
            .blocks
            .range(from..=to)
            .take(limit)
            .map(|(_, e)| RawBlock {
                slot: e.slot,
                hash: e.hash,
                block_number: e.block_no,
                era: Era::Conway,
                cbor: e.cbor.clone(),
            })
            .collect())
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

// ─── Test scaffold ───────────────────────────────────────────────────────

struct TestServer {
    addr: std::net::SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    tip_feed: TipFeed,
}

impl TestServer {
    async fn start(mock: SyncMock) -> Self {
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

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        match tokio::time::timeout(Duration::from_secs(3), self.join).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("server task panicked: {e}"),
            Err(_) => panic!("server didn't shut down within 3s"),
        }
    }
}

fn entry(slot: u64, hash_byte: u8, block_no: u64) -> ChainEntry {
    let mut hash = [0u8; 32];
    hash[0] = hash_byte;
    ChainEntry {
        slot,
        hash,
        block_no,
        cbor: vec![0xDE, 0xAD, hash_byte], // synthetic — parse returns None, native_bytes still works
    }
}

// ─── ReadTip ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_tip_returns_highest_slot_block() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::ReadTipRequest;

    let server = TestServer::start(SyncMock::new(vec![
        entry(100, 0xAA, 1),
        entry(200, 0xBB, 2),
        entry(300, 0xCC, 3),
    ]))
    .await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let resp = client.read_tip(ReadTipRequest::default()).await.unwrap();
    let tip = resp.into_inner().tip.expect("tip set");
    assert_eq!(tip.slot, 300);
    assert_eq!(tip.height, 3);
    let mut expected = [0u8; 32];
    expected[0] = 0xCC;
    assert_eq!(tip.hash, expected.to_vec());
    server.stop().await;
}

#[tokio::test]
async fn read_tip_alpha_matches_beta() {
    use dugite_rpc::proto::v1alpha::sync::sync_service_client::SyncServiceClient as Alpha;
    use dugite_rpc::proto::v1alpha::sync::ReadTipRequest;

    let server = TestServer::start(SyncMock::new(vec![entry(50, 0x42, 7)])).await;
    let mut client = Alpha::new(server.channel().await);
    let tip = client
        .read_tip(ReadTipRequest::default())
        .await
        .unwrap()
        .into_inner()
        .tip
        .unwrap();
    assert_eq!(tip.slot, 50);
    assert_eq!(tip.height, 7);
    server.stop().await;
}

// ─── FetchBlock ──────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_block_by_hash_returns_native_bytes() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, FetchBlockRequest};

    let server =
        TestServer::start(SyncMock::new(vec![entry(10, 0x01, 1), entry(20, 0x02, 2)])).await;
    let mut client = SyncServiceClient::new(server.channel().await);

    let mut hash = [0u8; 32];
    hash[0] = 0x02;
    let resp = client
        .fetch_block(FetchBlockRequest {
            r#ref: vec![BlockRef {
                slot: 0,
                hash: hash.to_vec(),
                height: 0,
                timestamp: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.block.len(), 1);
    assert_eq!(resp.block[0].native_bytes, vec![0xDE, 0xAD, 0x02]);
    server.stop().await;
}

#[tokio::test]
async fn fetch_block_unknown_hash_returns_empty_list() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, FetchBlockRequest};

    let server = TestServer::start(SyncMock::new(vec![entry(10, 0x01, 1)])).await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let hash = [0xFF; 32];
    let resp = client
        .fetch_block(FetchBlockRequest {
            r#ref: vec![BlockRef {
                slot: 0,
                hash: hash.to_vec(),
                height: 0,
                timestamp: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.block.is_empty());
    server.stop().await;
}

#[tokio::test]
async fn fetch_block_multiple_refs_in_order() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, FetchBlockRequest};

    let server = TestServer::start(SyncMock::new(vec![
        entry(10, 0x01, 1),
        entry(20, 0x02, 2),
        entry(30, 0x03, 3),
    ]))
    .await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let mut h1 = [0u8; 32];
    h1[0] = 0x03;
    let mut h2 = [0u8; 32];
    h2[0] = 0x01;
    let resp = client
        .fetch_block(FetchBlockRequest {
            r#ref: vec![
                BlockRef {
                    slot: 0,
                    hash: h1.to_vec(),
                    height: 0,
                    timestamp: 0,
                },
                BlockRef {
                    slot: 0,
                    hash: h2.to_vec(),
                    height: 0,
                    timestamp: 0,
                },
            ],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.block.len(), 2);
    assert_eq!(resp.block[0].native_bytes, vec![0xDE, 0xAD, 0x03]);
    assert_eq!(resp.block[1].native_bytes, vec![0xDE, 0xAD, 0x01]);
    server.stop().await;
}

// ─── FieldMask (issue #1004) ───────────────────────────────────────────────
//
// `FetchBlockResponse.block` is exactly the "block bodies" bandwidth
// example from the issue: a repeated field a mask must be able to prune
// INTO (elementwise), not just clear wholesale. masking.rs's unit tests
// already cover the algorithm; these prove it end-to-end over the real
// SyncService wire.

#[tokio::test]
async fn fetch_block_field_mask_unnamed_top_level_field_clears_whole_list() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, FetchBlockRequest};
    use prost_types::FieldMask;

    let server =
        TestServer::start(SyncMock::new(vec![entry(10, 0x01, 1), entry(20, 0x02, 2)])).await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let mut hash = [0u8; 32];
    hash[0] = 0x02;

    // A mask naming a field that doesn't exist on FetchBlockResponse at
    // all ("bogus") selects nothing — the real `block` field (the only
    // payload) must be cleared entirely, proving the mask reached the
    // live response rather than being silently dropped on the floor.
    let resp = client
        .fetch_block(FetchBlockRequest {
            r#ref: vec![BlockRef {
                slot: 0,
                hash: hash.to_vec(),
                height: 0,
                timestamp: 0,
            }],
            field_mask: Some(FieldMask {
                paths: vec!["bogus".to_string()],
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        resp.block.is_empty(),
        "the mock DID find the block (see fetch_block_by_hash_returns_native_bytes); \
         an empty result here can only come from the mask pruning the unnamed `block` field"
    );
    server.stop().await;
}

#[tokio::test]
async fn fetch_block_field_mask_prunes_repeated_block_elements() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, FetchBlockRequest};
    use prost_types::FieldMask;

    let server =
        TestServer::start(SyncMock::new(vec![entry(10, 0x01, 1), entry(20, 0x02, 2)])).await;
    let mut client = SyncServiceClient::new(server.channel().await);
    let mut h1 = [0u8; 32];
    h1[0] = 0x01;
    let mut h2 = [0u8; 32];
    h2[0] = 0x02;

    // "block.native_bytes" — the repeated `block` field is kept (it's
    // named), and the mask continues past it: every element must be
    // pruned down to just native_bytes (elementwise, per masking.rs's
    // documented repeated-field extension).
    let resp = client
        .fetch_block(FetchBlockRequest {
            r#ref: vec![
                BlockRef {
                    slot: 0,
                    hash: h1.to_vec(),
                    height: 0,
                    timestamp: 0,
                },
                BlockRef {
                    slot: 0,
                    hash: h2.to_vec(),
                    height: 0,
                    timestamp: 0,
                },
            ],
            field_mask: Some(FieldMask {
                paths: vec!["block.native_bytes".to_string()],
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.block.len(), 2, "the named repeated field survives");
    assert_eq!(resp.block[0].native_bytes, vec![0xDE, 0xAD, 0x01]);
    assert_eq!(resp.block[1].native_bytes, vec![0xDE, 0xAD, 0x02]);
    server.stop().await;
}

#[tokio::test]
async fn follow_tip_field_mask_applies_to_every_streamed_response() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::FollowTipRequest;
    use prost_types::FieldMask;
    use tokio_stream::StreamExt;

    let server = TestServer::start(SyncMock::new(vec![entry(700, 0x77, 7)])).await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);

    // Mask selects only `tip` — the `action` oneof (the actual block
    // payload) must be cleared on every emitted message, streamed or
    // not.
    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![],
            field_mask: Some(FieldMask {
                paths: vec!["tip".to_string()],
            }),
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    let mut h = [0u8; 32];
    h[0] = 0x77;
    publisher.announce_apply(TipInfo {
        slot: 700,
        hash: h,
        block_number: 7,
        era: Era::Conway,
    });

    let msg = stream.next().await.expect("apply msg").unwrap();
    assert!(
        msg.action.is_none(),
        "action must be pruned — the mask names only `tip`"
    );
    assert_eq!(msg.tip.expect("tip kept").slot, 700);

    drop(stream);
    server.stop().await;
}

// ─── DumpHistory ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_history_pages_forward_from_start_token() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, DumpHistoryRequest};

    let server = TestServer::start(SyncMock::new(vec![
        entry(100, 0x01, 1),
        entry(200, 0x02, 2),
        entry(300, 0x03, 3),
        entry(400, 0x04, 4),
    ]))
    .await;
    let mut client = SyncServiceClient::new(server.channel().await);

    // First page: start at origin, max 2.
    let page1 = client
        .dump_history(DumpHistoryRequest {
            start_token: Some(BlockRef {
                slot: 0,
                hash: vec![],
                height: 0,
                timestamp: 0,
            }),
            max_items: 2,
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page1.block.len(), 2);
    let next_token = page1.next_token.expect("next_token set");
    assert_eq!(next_token.slot, 200);

    // Second page: continue from next_token.
    let page2 = client
        .dump_history(DumpHistoryRequest {
            start_token: Some(next_token),
            max_items: 10,
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page2.block.len(), 2); // 300 and 400
    server.stop().await;
}

#[tokio::test]
async fn dump_history_empty_when_no_blocks_after_token() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{BlockRef, DumpHistoryRequest};

    let server = TestServer::start(SyncMock::new(vec![entry(100, 0x01, 1)])).await;
    let mut client = SyncServiceClient::new(server.channel().await);

    let resp = client
        .dump_history(DumpHistoryRequest {
            start_token: Some(BlockRef {
                slot: 500,
                hash: vec![],
                height: 0,
                timestamp: 0,
            }),
            max_items: 10,
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.block.is_empty());
    assert!(resp.next_token.is_none());
    server.stop().await;
}

// ─── FollowTip ───────────────────────────────────────────────────────────

#[tokio::test]
async fn follow_tip_emits_reset_for_known_intersection_then_apply_events() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{follow_tip_response, BlockRef, FollowTipRequest};
    use tokio_stream::StreamExt;

    let mut intersection_hash = [0u8; 32];
    intersection_hash[0] = 0x02;
    let server = TestServer::start(SyncMock::new(vec![
        entry(100, 0x01, 1),
        entry(200, 0x02, 2),
    ]))
    .await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);

    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![BlockRef {
                slot: 200,
                hash: intersection_hash.to_vec(),
                height: 2,
                timestamp: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    // First message: reset to intersection.
    let first = stream.next().await.expect("first msg").unwrap();
    match first.action.expect("action") {
        follow_tip_response::Action::Reset(r) => {
            assert_eq!(r.slot, 200);
            assert_eq!(r.hash, intersection_hash.to_vec());
        }
        other => panic!("expected reset, got {other:?}"),
    }

    // Fire two apply events.
    let mut h3 = [0u8; 32];
    h3[0] = 0x03;
    publisher.announce_apply(TipInfo {
        slot: 300,
        hash: h3,
        block_number: 3,
        era: Era::Conway,
    });
    let mut h4 = [0u8; 32];
    h4[0] = 0x04;
    publisher.announce_apply(TipInfo {
        slot: 400,
        hash: h4,
        block_number: 4,
        era: Era::Conway,
    });

    let second = stream.next().await.expect("second msg").unwrap();
    let tip = second.tip.expect("tip set");
    assert_eq!(tip.slot, 300);
    match second.action.expect("action") {
        follow_tip_response::Action::Apply(_) => {}
        other => panic!("expected apply, got {other:?}"),
    }
    let third = stream.next().await.expect("third msg").unwrap();
    assert_eq!(third.tip.unwrap().slot, 400);

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn follow_tip_emits_reset_on_rollback() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{follow_tip_response, FollowTipRequest};
    use dugite_rpc::TipRollback;
    use tokio_stream::StreamExt;

    let server = TestServer::start(SyncMock::new(vec![entry(100, 0x01, 1)])).await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);
    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    let mut h = [0u8; 32];
    h[0] = 0x99;
    publisher.announce_rollback(TipRollback { slot: 50, hash: h });

    let msg = stream.next().await.expect("rollback msg").unwrap();
    match msg.action.expect("action") {
        follow_tip_response::Action::Reset(r) => {
            assert_eq!(r.slot, 50);
            assert_eq!(r.hash, h.to_vec());
        }
        other => panic!("expected reset on rollback, got {other:?}"),
    }

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn follow_tip_intersect_with_no_match_skips_reset() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{follow_tip_response, BlockRef, FollowTipRequest};
    use tokio_stream::StreamExt;

    let server = TestServer::start(SyncMock::new(vec![entry(100, 0x01, 1)])).await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);

    let unknown = [0xFF; 32];
    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![BlockRef {
                slot: 999,
                hash: unknown.to_vec(),
                height: 0,
                timestamp: 0,
            }],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    // No reset since intersect found nothing — first message should be
    // the first apply event we fire.
    let mut h = [0u8; 32];
    h[0] = 0x09;
    publisher.announce_apply(TipInfo {
        slot: 110,
        hash: h,
        block_number: 2,
        era: Era::Conway,
    });

    let msg = stream.next().await.expect("apply msg").unwrap();
    match msg.action.expect("action") {
        follow_tip_response::Action::Apply(_) => {
            assert_eq!(msg.tip.unwrap().slot, 110);
        }
        other => panic!("expected apply, got {other:?}"),
    }
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn follow_tip_apply_event_carries_native_bytes_when_block_resolves() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{follow_tip_response, FollowTipRequest};
    use tokio_stream::StreamExt;

    // Mock chain contains a block at hash byte 0x77 → block_by_hash
    // succeeds; FollowTip should populate native_bytes from the CBOR.
    let server = TestServer::start(SyncMock::new(vec![entry(700, 0x77, 7)])).await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);
    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    let mut h = [0u8; 32];
    h[0] = 0x77;
    publisher.announce_apply(TipInfo {
        slot: 700,
        hash: h,
        block_number: 7,
        era: Era::Conway,
    });

    let msg = stream.next().await.expect("apply msg").unwrap();
    match msg.action.expect("action") {
        follow_tip_response::Action::Apply(envelope) => {
            assert_eq!(
                envelope.native_bytes,
                vec![0xDE, 0xAD, 0x77],
                "FollowTip apply should carry the block's native_bytes"
            );
        }
        other => panic!("expected apply, got {other:?}"),
    }
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn follow_tip_apply_event_native_bytes_empty_when_block_missing() {
    use dugite_rpc::proto::v1beta::sync::sync_service_client::SyncServiceClient;
    use dugite_rpc::proto::v1beta::sync::{follow_tip_response, FollowTipRequest};
    use tokio_stream::StreamExt;

    // Mock chain has only block 0x77; we fire an apply for 0x99 which
    // isn't in the mock → resolver logs WARN and emits empty envelope.
    let server = TestServer::start(SyncMock::new(vec![entry(700, 0x77, 7)])).await;
    let publisher = server.tip_feed.publisher();
    let mut client = SyncServiceClient::new(server.channel().await);
    let stream = client
        .follow_tip(FollowTipRequest {
            intersect: vec![],
            field_mask: None,
        })
        .await
        .unwrap()
        .into_inner();
    let mut stream = stream;

    let mut h = [0u8; 32];
    h[0] = 0x99;
    publisher.announce_apply(TipInfo {
        slot: 999,
        hash: h,
        block_number: 9,
        era: Era::Conway,
    });

    let msg = stream.next().await.expect("apply msg").unwrap();
    match msg.action.expect("action") {
        follow_tip_response::Action::Apply(envelope) => {
            assert!(
                envelope.native_bytes.is_empty(),
                "missing block → empty native_bytes (degraded but stream stays alive)"
            );
            // tip BlockRef still carries metadata so the client can
            // FetchBlock with the hash + slot.
            let tip = msg.tip.expect("tip metadata");
            assert_eq!(tip.slot, 999);
        }
        other => panic!("expected apply, got {other:?}"),
    }
    drop(stream);
    server.stop().await;
}
