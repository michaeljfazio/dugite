//! LocalChainSync server — serves full blocks to N2C clients.
//!
//! Uses the same ChainSync message wire format (tags 0-7) but wraps block data
//! in `Serialised` encoding: `tag(24)(bytes(block_cbor))` (CBOR-in-CBOR).
//! This matches the Haskell `SerialiseNodeToClient` encoding for `Serialised blk`.
//!
//! Delegates all protocol logic to the shared `ServeCore` (issue #881) —
//! see `crate::protocol::chainsync::serve_core` for the state machine.  This
//! server differs from N2N `ChainSyncServer` in exactly one respect: the
//! `MsgRollForward` payload is the full `Serialised`-wrapped block
//! ([`wrap_serialised`]) rather than an HFC-wrapped header.  Before this
//! extraction the two servers had drifted — this server was missing the
//! duplicate-serve dedup, the `biased` rollback-first `select!` ordering,
//! the `StMustReply` retry loop (#869), `cursor_at_origin` genesis-EBB
//! handling, and lagged-rollback safety (Bug J) that N2N already had (#868).

use std::time::Duration;

use minicbor::Encoder;
use tokio::sync::broadcast;

use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;
use crate::protocol::chainsync::serve_core::ServeCore;
use crate::protocol::chainsync::server::{BlockAnnouncement, RollbackAnnouncement};
use crate::BlockProvider;

#[cfg(test)]
use crate::codec::Point;

/// Wrap raw block CBOR in `Serialised` encoding: `tag(24)(bytes(block_cbor))`.
///
/// N2C LocalChainSync sends blocks as `Serialised (HardForkBlock xs)` which
/// uses CBOR-in-CBOR wrapping. The inner bytes are the full multi-era block
/// CBOR (including era tag) as stored in ChainDB.
fn wrap_serialised(block_cbor: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(block_cbor.len() + 10);
    let mut enc = Encoder::new(&mut buf);
    enc.tag(minicbor::data::Tag::new(24)).expect("infallible");
    enc.bytes(block_cbor).expect("infallible");
    buf
}

/// Encode the `MsgRollForward` payload for N2C LocalChainSync: wrap the full
/// block CBOR in `Serialised` encoding.  Infallible — no parsing required —
/// matching the `PayloadEncoder` signature used by the shared serve core.
fn n2c_payload_encoder(block_cbor: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    Ok(wrap_serialised(block_cbor))
}

/// LocalChainSync server that serves full blocks to N2C clients.
///
/// A thin wrapper around the shared `ServeCore` (issue #881) — all protocol
/// logic (cursor tracking, `MsgFindIntersect`/`MsgRequestNext` handling,
/// rollback propagation, `StMustReply` retry loop) lives in the shared core.
/// This type only supplies the N2C-specific payload encoder
/// ([`n2c_payload_encoder`]: full `Serialised`-wrapped block, not just the
/// header) and the `"LocalChainSync"` protocol label.
pub struct LocalChainSyncServer {
    core: ServeCore,
}

impl LocalChainSyncServer {
    /// Create a new server with no cursor.
    ///
    /// Draws a per-connection `StMustReply` timeout uniformly at random in
    /// `[601 s, 911 s]`, matching N2N ChainSync and Haskell's
    /// `ouroboros-network` ChainSync codec timeout policy.
    pub fn new() -> Self {
        Self {
            core: ServeCore::new(n2c_payload_encoder, "LocalChainSync"),
        }
    }

    /// Test/explicit-timeout constructor.  Production code should call
    /// [`Self::new`] to get the random per-connection draw; tests use this to
    /// pin the timeout to a known small value.
    #[doc(hidden)]
    pub fn new_with_timeout(must_reply_timeout: Duration) -> Self {
        Self {
            core: ServeCore::new_with_timeout(
                must_reply_timeout,
                n2c_payload_encoder,
                "LocalChainSync",
            ),
        }
    }

    /// Configured `StMustReply` timeout (exposed for diagnostics / tests).
    pub fn must_reply_timeout(&self) -> Duration {
        self.core.must_reply_timeout()
    }

    /// Run the LocalChainSync server loop.
    ///
    /// Accepts a `rollback_rx` receiver to propagate chain rollbacks to N2C
    /// clients via `MsgRollBackward`.
    pub async fn run<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        announcement_rx: broadcast::Receiver<BlockAnnouncement>,
        rollback_rx: broadcast::Receiver<RollbackAnnouncement>,
    ) -> Result<(), ProtocolError> {
        self.core
            .run(channel, block_provider, announcement_rx, rollback_rx)
            .await
    }

    /// Drain any pending rollback announcements, returning the most recent one.
    #[doc(hidden)]
    pub fn drain_rollback(
        rollback_rx: &mut broadcast::Receiver<RollbackAnnouncement>,
    ) -> Option<RollbackAnnouncement> {
        ServeCore::drain_rollback(rollback_rx)
    }
}

impl Default for LocalChainSyncServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::chainsync::serve_core::test_support::{
        ForkAwareMockProvider, MockBlockProvider, MutableMockBlockProvider,
    };
    use crate::protocol::chainsync::server::{BlockAnnouncement, RollbackAnnouncement};
    use crate::protocol::chainsync::{decode_message, encode_message, ChainSyncMessage};
    use crate::TipInfo;
    use bytes::Bytes;
    use minicbor::Encoder;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tokio::sync::{broadcast, mpsc};

    // ─── Test infrastructure ─────────────────────────────────────────────────

    /// Create block CBOR as a CBOR bstr so the ChainSync codec decoder
    /// recognises the `header` field (which expects Array or Bytes CBOR type).
    ///
    /// LocalChainSync sends raw block CBOR in MsgRollForward.header. When
    /// decoded by `decode_message`, it must be a valid CBOR type (Array or
    /// Bytes). Using a bstr ensures the roundtrip works.
    fn make_block_cbor(payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.bytes(payload).unwrap();
        buf
    }

    // `MockBlockProvider` (flat, no-fork block store) is shared with the N2N
    // ChainSync test suite via `serve_core::test_support` (issue #881) — both
    // wirings exercise the exact same `BlockProvider` semantics against the
    // shared `ServeCore`.

    /// Create a test MuxChannel with egress receiver and ingress sender.
    fn make_test_channel() -> (
        crate::mux::channel::MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = crate::mux::channel::MuxChannel::new(
            5, // LocalChainSync protocol ID
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            Arc::new(AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    /// Helper: spawn server, returning the join handle.
    fn spawn_server(
        mut channel: MuxChannel,
        provider: MockBlockProvider,
        ann_rx: broadcast::Receiver<BlockAnnouncement>,
        rb_rx: broadcast::Receiver<RollbackAnnouncement>,
    ) -> tokio::task::JoinHandle<Result<(), ProtocolError>> {
        tokio::spawn(async move {
            let mut server = LocalChainSyncServer::new();
            server.run(&mut channel, &provider, ann_rx, rb_rx).await
        })
    }

    /// Helper: decode egress message, stripping the mux header tuple.
    async fn recv_msg(
        egress_rx: &mut mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
    ) -> ChainSyncMessage {
        let (_, _, bytes) = egress_rx.recv().await.expect("egress channel closed");
        decode_message(&bytes).expect("failed to decode ChainSync message")
    }

    /// Helper: send a ChainSync message through the ingress channel.
    async fn send_msg(ingress_tx: &mpsc::Sender<Bytes>, msg: &ChainSyncMessage) {
        let encoded = encode_message(msg);
        ingress_tx
            .send(Bytes::from(encoded))
            .await
            .expect("ingress channel closed");
    }

    // ─── FindIntersect tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn find_intersect_origin() {
        // Finding intersection at Origin should always succeed.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], make_block_cbor(&[0x01, 0x02]))],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgIntersectFound {
                point,
                tip_slot,
                tip_hash,
                tip_block_number,
            } => {
                assert_eq!(point, Point::Origin);
                assert_eq!(tip_slot, 100);
                assert_eq!(tip_hash, [0xAA; 32]);
                assert_eq!(tip_block_number, 1);
            }
            other => panic!("expected MsgIntersectFound, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_specific_block() {
        // Finding intersection at a known specific block.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0xAA])),
                (20, [0x02; 32], make_block_cbor(&[0xBB])),
                (30, [0x03; 32], make_block_cbor(&[0xCC])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Request intersection at the second block.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(20, [0x02; 32])]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgIntersectFound {
                point,
                tip_slot,
                tip_hash,
                ..
            } => {
                assert_eq!(point, Point::Specific(20, [0x02; 32]));
                assert_eq!(tip_slot, 30);
                assert_eq!(tip_hash, [0x03; 32]);
            }
            other => panic!("expected MsgIntersectFound, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_not_found() {
        // When no requested points exist, server responds with MsgIntersectNotFound.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], make_block_cbor(&[0xAA]))],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Request intersection at a nonexistent block.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(999, [0xFF; 32])]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgIntersectNotFound {
                tip_slot, tip_hash, ..
            } => {
                assert_eq!(tip_slot, 10);
                assert_eq!(tip_hash, [0x01; 32]);
            }
            other => panic!("expected MsgIntersectNotFound, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_multiple_points_returns_first_match() {
        // When multiple points are provided, the server returns the first one that
        // exists in the chain (matching Haskell findIntersect behavior — clients
        // send points in reverse order, so the first match is the best intersection).
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0xAA])),
                (20, [0x02; 32], make_block_cbor(&[0xBB])),
                (30, [0x03; 32], make_block_cbor(&[0xCC])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Client sends points in reverse slot order (typical behavior).
        // First point (slot 30) exists, so it should be returned.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![
                Point::Specific(30, [0x03; 32]),
                Point::Specific(10, [0x01; 32]),
                Point::Origin,
            ]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgIntersectFound { point, .. } => {
                assert_eq!(
                    point,
                    Point::Specific(30, [0x03; 32]),
                    "should return the first matching point"
                );
            }
            other => panic!("expected MsgIntersectFound, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_falls_through_to_later_point() {
        // When the first point doesn't exist but a later one does, use the later one.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0xAA])),
                (20, [0x02; 32], make_block_cbor(&[0xBB])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // First point doesn't exist, second does.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![
                Point::Specific(99, [0xFF; 32]),
                Point::Specific(10, [0x01; 32]),
            ]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgIntersectFound { point, .. } => {
                assert_eq!(point, Point::Specific(10, [0x01; 32]));
            }
            other => panic!("expected MsgIntersectFound, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_empty_chain() {
        // With an empty chain, only Origin should match.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider { blocks: vec![] };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Specific point on empty chain → not found.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(10, [0x01; 32])]),
        )
        .await;

        let msg = recv_msg(&mut egress_rx).await;
        assert!(
            matches!(msg, ChainSyncMessage::MsgIntersectNotFound { .. }),
            "specific point on empty chain should not be found"
        );

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn find_intersect_sets_cursor_for_subsequent_request_next() {
        // After FindIntersect, RequestNext should serve the block AFTER the
        // intersection point (not the intersection block itself).
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let block_b_cbor = make_block_cbor(&[0xBB, 0xCC, 0xDD]);
        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], vec![0xAA]),
                (20, [0x02; 32], block_b_cbor.clone()),
                (30, [0x03; 32], make_block_cbor(&[0xEE])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Set intersection at slot 10.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(10, [0x01; 32])]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await; // MsgIntersectFound

        // RequestNext should serve block at slot 20 (next after cursor slot 10).
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward { header, .. } => {
                // LocalChainSync wraps blocks in Serialised encoding: tag(24)(bytes(cbor)).
                assert_eq!(header, wrap_serialised(&block_b_cbor));
            }
            other => panic!("expected MsgRollForward, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    // ─── RequestNext tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn request_next_serves_sequential_blocks() {
        // After intersecting at Origin, RequestNext should serve blocks in order.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0x10])),
                (20, [0x02; 32], make_block_cbor(&[0x20])),
                (30, [0x03; 32], make_block_cbor(&[0x30])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Intersect at Origin.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        // Serve all 3 blocks in order.
        let expected_cbor = [
            make_block_cbor(&[0x10]),
            make_block_cbor(&[0x20]),
            make_block_cbor(&[0x30]),
        ];
        for expected in &expected_cbor {
            send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
            let msg = recv_msg(&mut egress_rx).await;
            match msg {
                ChainSyncMessage::MsgRollForward { header, .. } => {
                    assert_eq!(header, wrap_serialised(expected));
                }
                other => panic!("expected MsgRollForward, got {other:?}"),
            }
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_next_sends_full_block_not_header() {
        // LocalChainSync (N2C) must send full block CBOR, not just the header.
        // This is the key difference from N2N ChainSync.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let full_block = make_block_cbor(&[0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE]);
        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], full_block.clone())],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward { header, .. } => {
                assert_eq!(
                    header,
                    wrap_serialised(&full_block),
                    "LocalChainSync must send Serialised-wrapped full block CBOR"
                );
            }
            other => panic!("expected MsgRollForward, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_next_tip_info_matches_provider() {
        // The tip info in MsgRollForward must match the provider's current tip.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0xAA])),
                (20, [0x02; 32], make_block_cbor(&[0xBB])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward {
                tip_slot,
                tip_hash,
                tip_block_number,
                ..
            } => {
                assert_eq!(tip_slot, 20);
                assert_eq!(tip_hash, [0x02; 32]);
                assert_eq!(tip_block_number, 2);
            }
            other => panic!("expected MsgRollForward, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_next_at_tip_sends_await_reply() {
        // When there are no more blocks to serve, the server sends MsgAwaitReply,
        // then waits for a block announcement or rollback.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], make_block_cbor(&[0xAA]))],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        // Serve the only block.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let _ = recv_msg(&mut egress_rx).await; // MsgRollForward

        // Next request — at tip, should get MsgAwaitReply.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        assert!(
            matches!(msg, ChainSyncMessage::MsgAwaitReply),
            "expected MsgAwaitReply at tip, got {msg:?}"
        );

        // Fire a SPURIOUS announcement — the provider does not actually have
        // this block, so `try_serve_next_block` finds nothing to serve.
        //
        // #869 fix: this must NOT cause the server to return without a
        // reply.  Per the Ouroboros ChainSync state machine, the client is
        // in StMustReply and will never send another MsgRequestNext, so a
        // silent return would wedge the connection.  Before the shared-core
        // extraction, N2C's `select!` was NOT looped and returned
        // immediately here — this test used to rely on that bug to
        // terminate; now it asserts the fixed behaviour (no reply, no
        // early return) and aborts the task to clean up rather than
        // waiting on a `handle.await` that would never resolve.
        ann_tx
            .send(BlockAnnouncement {
                slot: 20,
                hash: [0x02; 32],
                block_number: 2,
            })
            .unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(200), egress_rx.recv())
                .await
                .is_err(),
            "server must not send anything on a spurious wake with nothing new to serve"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn request_next_announcement_serves_new_block() {
        // When waiting at tip and a block announcement arrives for a block
        // that the provider has, the server should send MsgRollForward.

        // Use a mutable provider so we can add a block after the server starts.
        type BlockList = Vec<(u64, [u8; 32], Vec<u8>)>;
        struct ArcBlockProvider {
            blocks: Arc<std::sync::Mutex<BlockList>>,
        }
        impl BlockProvider for ArcBlockProvider {
            fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
                self.blocks
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(_, h, _)| h == hash)
                    .map(|(_, _, c)| c.clone())
            }
            fn has_block(&self, hash: &[u8; 32]) -> bool {
                self.blocks
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(_, h, _)| h == hash)
            }
            fn get_tip(&self) -> TipInfo {
                let b = self.blocks.lock().unwrap();
                b.last()
                    .map(|(s, h, _)| TipInfo {
                        slot: *s,
                        hash: *h,
                        block_number: b.len() as u64,
                    })
                    .unwrap_or(TipInfo {
                        slot: 0,
                        hash: [0; 32],
                        block_number: 0,
                    })
            }
            fn get_next_block_after_slot(
                &self,
                after_slot: u64,
            ) -> Option<(u64, [u8; 32], Vec<u8>)> {
                self.blocks
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(s, _, _)| *s > after_slot)
                    .cloned()
            }
        }

        let blocks = Arc::new(std::sync::Mutex::new(vec![(
            10u64,
            [0x01u8; 32],
            make_block_cbor(&[0xAA]),
        )]));
        let blocks_ref = blocks.clone();
        let provider = ArcBlockProvider { blocks };

        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let (egress_tx, mut egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let mut channel = crate::mux::channel::MuxChannel::new(
            5,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            Arc::new(AtomicUsize::new(0)),
        );

        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let handle = tokio::spawn(async move {
            let mut server = LocalChainSyncServer::new();
            server.run(&mut channel, &provider, ann_rx, rb_rx).await
        });

        // Intersect at Origin.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        // Serve the initial block.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let _ = recv_msg(&mut egress_rx).await;

        // Request next at tip — triggers MsgAwaitReply.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        assert!(matches!(msg, ChainSyncMessage::MsgAwaitReply));

        // Add a new block and announce it.
        let new_block_cbor = make_block_cbor(&[0xBB, 0xCC]);
        blocks_ref
            .lock()
            .unwrap()
            .push((20, [0x02; 32], new_block_cbor.clone()));
        ann_tx
            .send(BlockAnnouncement {
                slot: 20,
                hash: [0x02; 32],
                block_number: 2,
            })
            .unwrap();

        // Server should respond with MsgRollForward for the new block.
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward { header, .. } => {
                assert_eq!(header, wrap_serialised(&new_block_cbor));
            }
            other => panic!("expected MsgRollForward after announcement, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    // ─── Rollback tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn rollback_before_cursor_sends_roll_backward() {
        // When a rollback occurs to a point before the cursor, the server must
        // send MsgRollBackward and rewind its cursor.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0x10])),
                (20, [0x02; 32], make_block_cbor(&[0x20])),
                (30, [0x03; 32], make_block_cbor(&[0x30])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        // Intersect at Origin and serve all 3 blocks.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        for _ in 0..3 {
            send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
            let msg = recv_msg(&mut egress_rx).await;
            assert!(matches!(msg, ChainSyncMessage::MsgRollForward { .. }));
        }

        // Cursor is now at slot 30. Rollback to slot 10.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 10,
                hash: [0x01; 32],
            })
            .unwrap();

        // Next RequestNext should yield MsgRollBackward.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollBackward { point, .. } => {
                assert_eq!(point, Point::Specific(10, [0x01; 32]));
            }
            other => panic!("expected MsgRollBackward, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_to_origin() {
        // Rollback to slot 0 with zero hash should produce MsgRollBackward(Origin).
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], make_block_cbor(&[0xAA]))],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        // Serve the block (cursor at slot 10).
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let _ = recv_msg(&mut egress_rx).await;

        // Rollback to origin.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 0,
                hash: [0u8; 32],
            })
            .unwrap();

        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollBackward { point, .. } => {
                assert_eq!(
                    point,
                    Point::Origin,
                    "rollback to slot 0 + zero hash = Origin"
                );
            }
            other => panic!("expected MsgRollBackward(Origin), got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_behind_cursor_no_rollback_sent() {
        // If the rollback point is ahead of the cursor, no MsgRollBackward should
        // be sent — the client hasn't seen the rolled-back blocks.
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], make_block_cbor(&[0x10])),
                (20, [0x02; 32], make_block_cbor(&[0x20])),
                (30, [0x03; 32], make_block_cbor(&[0x30])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        // Only serve 1 block (cursor at slot 10).
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let _ = recv_msg(&mut egress_rx).await;

        // Rollback to slot 20 (ahead of cursor at slot 10).
        rb_tx
            .send(RollbackAnnouncement {
                slot: 20,
                hash: [0x02; 32],
            })
            .unwrap();

        // Should get MsgRollForward for block at slot 20, not MsgRollBackward.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        assert!(
            matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
            "rollback ahead of cursor should not trigger MsgRollBackward, got {msg:?}"
        );

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    // ─── MsgDone tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn msg_done_terminates_server_cleanly() {
        let (channel, _egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let provider = MockBlockProvider { blocks: vec![] };
        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "MsgDone should terminate server cleanly");
    }

    // ─── drain_rollback tests ────────────────────────────────────────────────

    #[test]
    fn drain_rollback_returns_latest() {
        // drain_rollback should return the most recent rollback announcement
        // when multiple are buffered.
        let (tx, mut rx) = broadcast::channel(16);
        tx.send(RollbackAnnouncement {
            slot: 10,
            hash: [0x01; 32],
        })
        .unwrap();
        tx.send(RollbackAnnouncement {
            slot: 5,
            hash: [0x05; 32],
        })
        .unwrap();

        let result = LocalChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_some());
        let rb = result.unwrap();
        assert_eq!(rb.slot, 5, "should return the last (most recent) rollback");
        assert_eq!(rb.hash, [0x05; 32]);
    }

    #[test]
    fn drain_rollback_empty_returns_none() {
        let (_tx, mut rx) = broadcast::channel::<RollbackAnnouncement>(16);
        let result = LocalChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_none());
    }

    // ─── Default impl test ───────────────────────────────────────────────────

    #[test]
    fn default_creates_zero_cursor() {
        let server = LocalChainSyncServer::default();
        assert_eq!(server.core.cursor_slot, 0);
        assert_eq!(server.core.cursor_hash, [0; 32]);
    }

    // ─── #868: N2C now exercises the shared ServeCore — parity with N2N ──────
    //
    // Before the shared-core extraction (#881), `LocalChainSyncServer` had its
    // own hand-rolled `handle_request_next` that was missing several fixes
    // already present in N2N `ChainSyncServer`: cursor revalidation after a
    // fork switch (Bug J), `cursor_at_origin` genesis-EBB handling, and the
    // `StMustReply` retry loop (#869) / timeout arm.  These tests drive the
    // N2C wiring through the same scenarios as the N2N test suite in
    // `chainsync::server` and assert identical protocol-level behaviour.

    /// #868 parity with N2N's `fork_switch_to_lower_slot_chain_sends_rollback_then_serves_new_blocks`
    /// (Bug J): after a fork switch displaces the follower cursor's block,
    /// the server must send `MsgRollBackward` to the most recent on-chain
    /// ancestor, then deliver the new chain's blocks in slot order —
    /// including blocks at slots at or below the old cursor slot.
    #[tokio::test]
    async fn fork_switch_sends_rollback_then_serves_new_blocks() {
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        let b_hash = [0x0B; 32];
        let c_hash = [0x0C; 32];
        let d_hash = [0x0D; 32];

        // Original chain: A@10 → B@20 → C@30 → D@40.
        provider.push_on_chain(10, a_hash, genesis, 1, make_block_cbor(&[0xA1]));
        provider.push_on_chain(20, b_hash, a_hash, 2, make_block_cbor(&[0xB1]));
        provider.push_on_chain(30, c_hash, b_hash, 3, make_block_cbor(&[0xC1]));
        provider.push_on_chain(40, d_hash, c_hash, 4, make_block_cbor(&[0xD1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = LocalChainSyncServer::new();
        let handle = {
            let provider_handle = ForkAwareMockProvider {
                chain: provider.chain.clone(),
                store: provider.store.clone(),
            };
            tokio::spawn(async move {
                server
                    .run(&mut channel, &provider_handle, ann_rx, rb_rx)
                    .await
            })
        };

        // Intersect at origin and serve A, B, C, D.
        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        for _ in 0..4 {
            let msg = recv_msg_after(&ingress_tx, &mut egress_rx).await;
            assert!(matches!(msg, ChainSyncMessage::MsgRollForward { .. }));
        }
        // Cursor now at D@40.

        // Fork switch: A → X@15 → Y@25 → Z@35 → W@50 — intermediate blocks
        // sit at slots BELOW the cursor (40).
        let x_hash = [0x1A; 32];
        let y_hash = [0x2A; 32];
        let z_hash = [0x3A; 32];
        let w_hash = [0x4A; 32];
        provider.put_fork(15, x_hash, a_hash, 2, make_block_cbor(&[0xAA]));
        provider.put_fork(25, y_hash, x_hash, 3, make_block_cbor(&[0xBB]));
        provider.put_fork(35, z_hash, y_hash, 4, make_block_cbor(&[0xCC]));
        provider.put_fork(50, w_hash, z_hash, 5, make_block_cbor(&[0xDD]));
        provider.replace_chain(vec![a_hash, x_hash, y_hash, z_hash, w_hash]);

        ann_tx
            .send(BlockAnnouncement {
                slot: 50,
                hash: w_hash,
                block_number: 5,
            })
            .unwrap();

        let msg = recv_msg_after(&ingress_tx, &mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollBackward { point, .. } => {
                assert_eq!(
                    point,
                    Point::Specific(10, a_hash),
                    "must rewind to most recent on-chain ancestor"
                );
            }
            other => panic!("expected MsgRollBackward to A@10, got {other:?}"),
        }

        // The entire new chain past A must be delivered, in order, including
        // slots BELOW the old cursor (40).
        for _ in 0..4 {
            let msg = recv_msg_after(&ingress_tx, &mut egress_rx).await;
            match msg {
                ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                    assert_eq!(tip_slot, 50);
                }
                other => panic!("expected MsgRollForward on new chain, got {other:?}"),
            }
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    /// #868 parity: a block sitting exactly at slot 0 (e.g. a Byron genesis
    /// EBB) must be served when intersecting at Origin.  Before the
    /// shared-core extraction, N2C never tracked `cursor_at_origin` and used
    /// a strict point-cursor lookup that silently skipped a slot-0 block.
    #[tokio::test]
    async fn genesis_block_at_slot_zero_served_from_origin() {
        let (channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let ebb_cbor = make_block_cbor(&[0xEB]);
        let provider = MockBlockProvider {
            blocks: vec![
                (0, [0xEB; 32], ebb_cbor.clone()),
                (1, [0x01; 32], make_block_cbor(&[0x01])),
            ],
        };

        let handle = spawn_server(channel, provider, ann_tx.subscribe(), rb_tx.subscribe());

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await; // MsgIntersectFound

        // First MsgRequestNext must serve the slot-0 block, not skip it.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward { header, .. } => {
                assert_eq!(
                    header,
                    wrap_serialised(&ebb_cbor),
                    "genesis block at slot 0 must be served, not skipped"
                );
            }
            other => panic!("expected MsgRollForward for slot-0 block, got {other:?}"),
        }

        // Second request must advance past slot 0 to slot 1 (cursor_at_origin
        // must have been cleared — otherwise the slot-0 block is re-served).
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        match msg {
            ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                assert_eq!(
                    tip_slot, 1,
                    "must advance past the slot-0 block, not re-serve it"
                );
            }
            other => panic!("expected MsgRollForward for slot 1, got {other:?}"),
        }

        send_msg(&ingress_tx, &ChainSyncMessage::MsgDone).await;
        handle.await.unwrap().unwrap();
    }

    /// #868 parity with N2N's `spurious_announcement_wake_does_not_return_agency_without_reply`
    /// (#869): once `MsgAwaitReply` has been sent, a spurious wake with
    /// nothing new to serve must not cause the server to return without a
    /// reply — and the periodic `StMustReply` timeout re-poll (which N2C
    /// never had before the shared-core extraction) must eventually deliver
    /// a block that arrives after the timeout has already fired once.
    #[tokio::test]
    async fn idle_timeout_repolls_and_eventually_serves() {
        let block_a = make_block_cbor(&[0x0A]);
        let provider = MutableMockBlockProvider::new(vec![(10, [0x01; 32], block_a)]);
        let blocks_ref = provider.blocks.clone();

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        // Pin a short StMustReply timeout so the timeout arm fires quickly.
        let mut server = LocalChainSyncServer::new_with_timeout(Duration::from_millis(100));
        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        send_msg(
            &ingress_tx,
            &ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]),
        )
        .await;
        let _ = recv_msg(&mut egress_rx).await;

        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        assert!(matches!(msg, ChainSyncMessage::MsgRollForward { .. }));

        // At tip — enters StMustReply.
        send_msg(&ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        let msg = recv_msg(&mut egress_rx).await;
        assert!(matches!(msg, ChainSyncMessage::MsgAwaitReply));

        // Nothing new yet — let the 100ms timeout fire at least once with
        // nothing to serve.  The server must keep waiting (not wedge, not
        // return early) rather than requiring a second MsgRequestNext.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            egress_rx.try_recv().is_err(),
            "server must not send anything while there is still nothing new to serve"
        );

        // Now push a genuine block — no announcement this time, proving the
        // timeout re-poll (not just the announcement wake) picks it up.
        let block_b = make_block_cbor(&[0x0B]);
        blocks_ref.lock().unwrap().push((20, [0x02; 32], block_b));

        let (_, _, resp) = tokio::time::timeout(Duration::from_secs(5), egress_rx.recv())
            .await
            .expect("server must eventually reply once a real block is available")
            .unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        handle.abort();
    }

    /// Helper: send `MsgRequestNext` and receive the next reply.  Used by the
    /// fork-switch parity test where the same request/response idiom repeats.
    async fn recv_msg_after(
        ingress_tx: &mpsc::Sender<Bytes>,
        egress_rx: &mut mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
    ) -> ChainSyncMessage {
        send_msg(ingress_tx, &ChainSyncMessage::MsgRequestNext).await;
        recv_msg(egress_rx).await
    }
}
