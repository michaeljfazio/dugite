//! ChainSync server — serves headers to downstream peers.
//!
//! Maintains a per-peer cursor tracking the last block served. When the peer
//! sends `MsgRequestNext`, serves the next header from ChainDB. At the tip,
//! waits for a block announcement via a broadcast channel before responding.
//!
//! ## Block Announcement
//! When a new block is received (either from upstream sync or forged locally),
//! a broadcast is sent. The server listens for this broadcast to unblock peers
//! waiting at the tip (in `StMustReply` state).
//!
//! ## Header Extraction
//! N2N ChainSync sends only the block *header*, not the full block.  Headers are
//! 10–100× smaller than full blocks; sending full blocks wastes bandwidth and
//! produces CBOR that Haskell decoders cannot parse as a header.
//!
//! Shelley+ blocks are HFC-wrapped: `[era_tag, block_body]` where
//! `block_body = [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]`.
//! We extract `header` (index 0 of `block_body`) and re-wrap as
//! `[era_tag, #6.24(bstr(header_cbor))]`.
//!
//! Byron blocks (era_tag = 0) use a different internal structure; they are
//! handled by a dedicated path that skips tag-24 wrapping.

use std::io::Write as _;
use std::time::Duration;

use minicbor::{Decoder, Encoder};
use tokio::sync::broadcast;

use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;
use crate::BlockProvider;

use super::serve_core::ServeCore;

// Re-use shared HFC helpers from the protocol module.
use crate::protocol::{storage_era_tag_to_hfc_index, CBOR_TAG_EMBEDDED};

#[cfg(test)]
use super::{decode_message, encode_message, ChainSyncMessage};
#[cfg(test)]
use crate::codec::Point;

/// Extract the block header from raw HFC-wrapped block CBOR and encode it as
/// `[hfc_index, #6.24(bstr(header_cbor))]` ready for inlining into MsgRollForward.
///
/// # Era tag conversion
///
/// The block CBOR uses storage era tags (Byron=0/1, Shelley=2, …,
/// Conway=7).  The N2N ChainSync `MsgRollForward` uses HFC NS indices
/// (Byron=0, Shelley=1, …, Conway=6) — one less than the storage tag for all
/// post-Byron eras.  This function converts between the two schemes so that
/// Haskell peers route the header to the correct era-specific decoder.
///
/// # Block layout
///
/// ```text
/// Shelley+ HFC layout: [storage_era_tag, [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]]
/// Byron HFC layout:    [storage_era_tag, [header, body, extra]]
/// ```
///
/// Returns an error if the CBOR does not match the expected structure.  The
/// caller MUST propagate the error and MUST NOT fall back to sending the full
/// block — doing so would produce incorrect wire output.
pub fn extract_header_for_chainsync(block_cbor: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = Decoder::new(block_cbor);

    // Outer array: [storage_era_tag, block_body]
    dec.array()
        .map_err(|e| format!("block CBOR: expected outer array: {e}"))?;
    let storage_era_tag = dec
        .u64()
        .map_err(|e| format!("block CBOR: expected era_tag u64: {e}"))?;

    // Convert to HFC NS index (the value Haskell's encodeNS/decodeNS expects).
    let hfc_index = storage_era_tag_to_hfc_index(storage_era_tag)?;

    // Inner array: [header, ...]  — we only need the first element.
    dec.array().map_err(|e| {
        format!("block CBOR (storage_era={storage_era_tag}): expected inner block array: {e}")
    })?;

    // Capture the raw CBOR bytes of the header sub-value.
    let header_start = dec.position();
    dec.skip().map_err(|e| {
        format!("block CBOR (storage_era={storage_era_tag}): could not skip header: {e}")
    })?;
    let header_end = dec.position();
    let header_cbor = &block_cbor[header_start..header_end];

    // Encode the HFC-wrapped header: [hfc_index, #6.24(bstr(header_cbor))]
    //
    // This is the format expected by Haskell's `dispatchDecoder` in
    // `Ouroboros.Consensus.HardFork.Combinator.Serialisation.SerialiseNodeToNode`.
    // `encodeNS` produces `array(2)[era_index_u8, tag(24)(header_bytes)]`.
    let mut buf = Vec::with_capacity(8 + header_cbor.len());
    let mut enc = Encoder::new(&mut buf);
    enc.array(2)
        .map_err(|e| format!("encode hfc header: array: {e}"))?;
    enc.u8(hfc_index)
        .map_err(|e| format!("encode hfc header: hfc_index: {e}"))?;
    enc.tag(minicbor::data::Tag::new(CBOR_TAG_EMBEDDED))
        .map_err(|e| format!("encode hfc header: tag24: {e}"))?;
    enc.bytes(header_cbor)
        .map_err(|e| format!("encode hfc header: bytes: {e}"))?;
    // Write any remaining encoder state.
    enc.writer_mut()
        .flush()
        .map_err(|e| format!("encode hfc header: flush: {e}"))?;

    Ok(buf)
}

/// Block announcement sent via broadcast channel when a new block arrives.
#[derive(Debug, Clone)]
pub struct BlockAnnouncement {
    /// Slot of the announced block.
    pub slot: u64,
    /// Hash of the announced block.
    pub hash: [u8; 32],
    /// Block number of the announced block.
    pub block_number: u64,
}

/// Rollback announcement sent via broadcast channel when the chain rolls back.
///
/// When the local chain switches to a better fork, all downstream ChainSync
/// followers must be notified so they send `MsgRollBackward` to their peers
/// instead of continuing to serve blocks from the old (now-abandoned) fork.
#[derive(Debug, Clone)]
pub struct RollbackAnnouncement {
    /// Slot of the point to roll back to.
    pub slot: u64,
    /// Hash of the block at the rollback point.
    pub hash: [u8; 32],
}

/// Lower bound of the ChainSync `StMustReply` (`MsgAwaitReply`) timeout.
///
/// Matches Haskell's `minChainSyncTimeout = 601` seconds from
/// `ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/ChainSync/Codec.hs`.
/// Each ChainSync server draws a uniform-random timeout in
/// `[CHAINSYNC_MUST_REPLY_TIMEOUT_MIN, CHAINSYNC_MUST_REPLY_TIMEOUT_MAX]` at
/// connection time and reuses it for every wait-at-tip cycle.  The randomized
/// per-peer draw prevents synchronized re-poll storms across a population of
/// peers and matches the [99.9-99.9999%] block-arrival window for f=0.05.
/// Issue #701.
pub const CHAINSYNC_MUST_REPLY_TIMEOUT_MIN: Duration = Duration::from_secs(601);

/// Upper bound of the ChainSync `StMustReply` (`MsgAwaitReply`) timeout.
///
/// Matches Haskell's `maxChainSyncTimeout = 911` seconds (see
/// [`CHAINSYNC_MUST_REPLY_TIMEOUT_MIN`]).  Issue #701.
pub const CHAINSYNC_MUST_REPLY_TIMEOUT_MAX: Duration = Duration::from_secs(911);

/// Encode the `MsgRollForward` payload for N2N ChainSync: extract just the
/// block header and HFC-wrap it.  Wraps [`extract_header_for_chainsync`]'s
/// `String` error into a [`ProtocolError::CborDecode`] to match the
/// `PayloadEncoder` signature used by the shared serve core (issue #881).
fn n2n_payload_encoder(block_cbor: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    extract_header_for_chainsync(block_cbor).map_err(|reason| ProtocolError::CborDecode {
        protocol: "ChainSync",
        reason: format!("header extraction failed: {reason}"),
    })
}

/// ChainSync server that serves headers to a single downstream peer.
///
/// A thin wrapper around the shared `ServeCore` (issue #881) — all protocol
/// logic (cursor tracking, `MsgFindIntersect`/`MsgRequestNext` handling,
/// rollback propagation, `StMustReply` retry loop) lives in the shared core.
/// This type only supplies the N2N-specific payload encoder
/// ([`n2n_payload_encoder`]: HFC-wrapped header, not the full block) and the
/// `"ChainSync"` protocol label.
pub struct ChainSyncServer {
    core: ServeCore,
}

impl ChainSyncServer {
    /// Create a new server with no cursor (must find intersection first).
    ///
    /// Draws a per-connection `StMustReply` timeout uniformly at random in
    /// `[601 s, 911 s]` to match Haskell's `ouroboros-network` ChainSync
    /// codec timeout policy.
    pub fn new() -> Self {
        Self {
            core: ServeCore::new(n2n_payload_encoder, "ChainSync"),
        }
    }

    /// Test/explicit-timeout constructor.  Production code should call
    /// [`Self::new`] to get the random per-connection draw; tests use this to
    /// pin the timeout to a known small value.
    #[doc(hidden)]
    pub fn new_with_timeout(must_reply_timeout: Duration) -> Self {
        Self {
            core: ServeCore::new_with_timeout(must_reply_timeout, n2n_payload_encoder, "ChainSync"),
        }
    }

    /// Configured `StMustReply` timeout (exposed for diagnostics / tests).
    pub fn must_reply_timeout(&self) -> Duration {
        self.core.must_reply_timeout()
    }

    /// Run the ChainSync server loop.
    ///
    /// Handles `MsgFindIntersect`, `MsgRequestNext`, and `MsgDone` from the client.
    /// Uses `block_provider` to look up blocks, `announcement_rx` to wait for
    /// new blocks at the tip, and `rollback_rx` to propagate chain rollbacks
    /// to downstream peers via `MsgRollBackward`.
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
    ///
    /// Multiple rollbacks may have queued up between calls — only the latest
    /// matters because each successive rollback supersedes the previous one.
    #[doc(hidden)]
    pub fn drain_rollback(
        rollback_rx: &mut broadcast::Receiver<RollbackAnnouncement>,
    ) -> Option<RollbackAnnouncement> {
        ServeCore::drain_rollback(rollback_rx)
    }
}

impl Default for ChainSyncServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::serve_core::test_support::{
        ForkAwareMockProvider, MockBlockProvider, MutableMockBlockProvider,
    };
    use super::*;
    use bytes::Bytes;
    use minicbor::Encoder;
    use tokio::sync::mpsc;

    // ─── helpers ──────────────────────────────────────────────────────────────

    /// Encode `value` as a CBOR bstr — returns the bytes used to store `value`
    /// inside the block's inner array.  This is what the header element looks
    /// like at wire level when we put raw bytes there for testing.
    fn cbor_encode_bytes(value: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.bytes(value).unwrap();
        buf
    }

    /// Build a minimal valid HFC-wrapped block CBOR for testing.
    ///
    /// Layout: `[era_tag, [header_cbor, [], [], null, []]]`
    ///
    /// The header element is stored as a CBOR bstr containing `header_bytes`.
    /// `extract_header_for_chainsync` will capture the full CBOR encoding of
    /// that bstr (including the length prefix) as the header sub-value.
    fn make_hfc_block(era_tag: u64, header_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        // Outer: [era_tag, inner_array]
        enc.array(2).unwrap();
        enc.u64(era_tag).unwrap();
        // Inner block body array: [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]
        enc.array(5).unwrap();
        enc.bytes(header_bytes).unwrap(); // header stored as a bstr
        enc.array(0).unwrap(); // tx_bodies: []
        enc.array(0).unwrap(); // tx_witnesses: []
        enc.null().unwrap(); // aux_data: null
        enc.array(0).unwrap(); // invalid_txs: []
        buf
    }

    /// Build the expected HFC-wrapped header bytes that `extract_header_for_chainsync`
    /// should produce.
    ///
    /// `storage_era_tag` is the era tag from the block's on-disk/wire CBOR (legacy
    /// convention: Byron=0/1, Shelley=2, ..., Conway=7).  The function converts it
    /// to the HFC NS index and encodes `[hfc_index_u8, #6.24(bstr(header_cbor))]`.
    ///
    /// `header_cbor` must be the **CBOR-encoded** form of the header element as
    /// it appears inside the block — i.e. the bytes captured by `dec.skip()`.
    /// For blocks built with `make_hfc_block`, pass `cbor_encode_bytes(raw_bytes)`.
    fn expected_hfc_header(storage_era_tag: u64, header_cbor: &[u8]) -> Vec<u8> {
        let hfc_index = storage_era_tag_to_hfc_index(storage_era_tag)
            .expect("test fixture used invalid storage era tag");
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u8(hfc_index).unwrap();
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        // header_cbor is the raw CBOR of the header element; embed it as a bstr
        // (this is what tag24 wraps — the CBOR serialisation of the header).
        enc.bytes(header_cbor).unwrap();
        buf
    }

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = MuxChannel::new(
            2,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    // ─── unit tests for extract_header_for_chainsync ──────────────────────────

    #[test]
    fn extract_header_conway_block() {
        // Build a synthetic Conway (storage era_tag=7, HFC index=6) HFC block.
        // Pallas uses era_tag=7 for Conway blocks in ImmutableDB and block-fetch.
        // The ChainSync header wire format uses HFC NS index=6 for Conway.
        // In our test fixture the header element is a bstr; extraction captures
        // the full CBOR encoding of that element (bstr length prefix + bytes).
        let inner_header_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let block_cbor = make_hfc_block(7, &inner_header_bytes); // storage era_tag=7 (Conway)

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed for valid Conway block");

        // The extractor converts storage era_tag=7 → HFC index=6, then produces
        // [6_u8, #6.24(bstr(header_cbor))].  Pass the CBOR form of the element.
        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(7, &header_cbor); // storage tag→hfc_index conversion
        assert_eq!(
            hfc_header, expected,
            "extracted HFC header does not match expected encoding"
        );

        // Verify the HFC index in the output is actually 6 (Conway).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 6, "Conway blocks must use HFC NS index 6");
    }

    #[test]
    fn extract_header_babbage_block() {
        // Babbage (storage era_tag=6, HFC index=5) — distinct from Conway.
        let inner_header_bytes = vec![0xBA, 0xBB, 0xAA, 0xBE];
        let block_cbor = make_hfc_block(6, &inner_header_bytes); // storage era_tag=6 (Babbage)

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed for valid Babbage block");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(6, &header_cbor);
        assert_eq!(hfc_header, expected);

        // Verify the HFC index in the output is actually 5 (Babbage).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 5, "Babbage blocks must use HFC NS index 5");
    }

    #[test]
    fn extract_header_shelley_block() {
        // Shelley (storage era_tag=2, HFC index=1) — same structure, different era identifier.
        let inner_header_bytes = vec![0x01, 0x02, 0x03];
        let block_cbor = make_hfc_block(2, &inner_header_bytes);

        let hfc_header =
            extract_header_for_chainsync(&block_cbor).expect("Shelley extraction should succeed");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(2, &header_cbor);
        assert_eq!(hfc_header, expected);

        // Verify the HFC index in the output is actually 1 (Shelley).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 1, "Shelley blocks must use HFC NS index 1");
    }

    #[test]
    fn extract_header_larger_inner_header() {
        // Verify extraction with a larger (256-byte) inner header payload (Conway).
        let inner_header_bytes: Vec<u8> = (0u8..=255u8).collect();
        let block_cbor = make_hfc_block(7, &inner_header_bytes); // Conway storage era_tag=7

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed with large inner header");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(7, &header_cbor); // Conway storage_era_tag=7
        assert_eq!(hfc_header, expected);
    }

    #[test]
    fn extract_header_invalid_cbor_returns_error() {
        // Truncated / garbage input must return an Err, not panic.
        let result = extract_header_for_chainsync(&[0xFF, 0x00, 0x01]);
        assert!(
            result.is_err(),
            "expected Err for invalid CBOR, got Ok: {result:?}"
        );
    }

    #[test]
    fn extract_header_empty_input_returns_error() {
        let result = extract_header_for_chainsync(&[]);
        assert!(result.is_err(), "expected Err for empty input");
    }

    // ─── integration tests for ChainSync server ───────────────────────────────

    #[tokio::test]
    async fn find_intersect_with_known_block() {
        // Block CBOR does not need to be valid for FindIntersect (the server only
        // checks presence via has_block, not the CBOR content).
        // Use Conway storage era_tag=7 for realistic block CBOR.
        let block_cbor = make_hfc_block(7, &[0x01, 0x02]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block_cbor)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Send MsgFindIntersect
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            100, [0xAA; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        // Read MsgIntersectFound
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(matches!(msg, ChainSyncMessage::MsgIntersectFound { .. }));

        // Send MsgDone to clean up
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_next_serves_header_not_full_block() {
        // The mock provider holds blocks whose CBOR is a valid HFC-wrapped block.
        // The server must extract the header and send [hfc_index, #6.24(bstr(hdr))].
        // Use Conway storage era_tag=7 (→ HFC index=6) for realistic fixtures.
        let inner_header_bytes_a = vec![0xAA, 0xBB];
        let inner_header_bytes_b = vec![0xCC, 0xDD];
        let block_a = make_hfc_block(7, &inner_header_bytes_a); // Conway storage_era_tag=7
        let block_b = make_hfc_block(7, &inner_header_bytes_b); // Conway storage_era_tag=7

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], block_a), (20, [0x02; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Find intersection at origin (sets cursor_slot = 0).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Request next — should get the header for the block at slot 10.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { header, .. } = msg {
            // The header field must be the HFC-wrapped header, not the full block.
            // In our fixture the header element is a bstr, so its CBOR encoding
            // is cbor_encode_bytes(inner_header_bytes_a).
            // Conway storage_era_tag=7 → HFC index=6, so expected = [6, tag24(bytes)].
            let header_cbor = cbor_encode_bytes(&inner_header_bytes_a);
            let expected = expected_hfc_header(7, &header_cbor); // storage_era_tag=7 (Conway)
            assert_eq!(
                header, expected,
                "server sent incorrect HFC-wrapped header; \
                 expected [hfc_index=6, #6.24(bstr(inner))], got {header:?}"
            );
            // Sanity-check: the header is strictly smaller than the full block CBOR.
            // (Full block includes tx_bodies, tx_witnesses, aux_data, invalid_txs.)
            let full_block = make_hfc_block(7, &inner_header_bytes_a);
            assert!(
                header.len() < full_block.len(),
                "header ({} bytes) should be smaller than full block ({} bytes)",
                header.len(),
                full_block.len()
            );
        } else {
            panic!("expected MsgRollForward, got {msg:?}");
        }

        // Send MsgDone
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// A Byron EBB shares its absolute slot with the first main block of the
    /// epoch (mainnet: 171 of 176 Byron boundaries).  The follower cursor is
    /// a point (slot + hash), so the server must serve the EBB and then the
    /// same-slot main block — a slot-only cursor advance skips the main
    /// block and serves the peer a chain with a hole in it.
    #[tokio::test]
    async fn serves_same_slot_ebb_pair_in_chain_order() {
        let hdr_pred = vec![0x10, 0x11];
        let hdr_ebb = vec![0x20, 0x21];
        let hdr_main = vec![0x30, 0x31];
        let hdr_next = vec![0x40, 0x41];

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![
                (99, [0x01; 32], make_hfc_block(7, &hdr_pred)),
                (100, [0x02; 32], make_hfc_block(7, &hdr_ebb)),
                (100, [0x03; 32], make_hfc_block(7, &hdr_main)),
                (101, [0x04; 32], make_hfc_block(7, &hdr_next)),
            ],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let mut server = ChainSyncServer::new();
        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Intersect at the predecessor of the boundary pair.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            99, [0x01; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve order must be: EBB@100, main@100, next@101.
        for (name, hdr) in [("ebb", &hdr_ebb), ("main", &hdr_main), ("next", &hdr_next)] {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&resp).unwrap();
            let ChainSyncMessage::MsgRollForward { header, .. } = msg else {
                panic!("expected MsgRollForward for {name}, got {msg:?}");
            };
            let expected = expected_hfc_header(7, &cbor_encode_bytes(hdr));
            assert_eq!(
                header, expected,
                "wrong block served at step {name}: same-slot EBB/main pair \
                 must be served in chain order"
            );
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── rollback propagation tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn rollback_sends_msg_roll_backward() {
        // Scenario: serve 3 blocks (slots 10, 20, 30), then trigger a 2-block
        // rollback to slot 10.  The server should send MsgRollBackward(slot=10)
        // and then serve new fork blocks on the next MsgRequestNext.
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let block_c = make_hfc_block(7, &[0x0C]);

        let provider = MutableMockBlockProvider::new(vec![
            (10, [0x01; 32], block_a),
            (20, [0x02; 32], block_b),
            (30, [0x03; 32], block_c),
        ]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let blocks_ref = provider.blocks.clone();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve all 3 blocks via MsgRequestNext.
        for _ in 0..3 {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&resp).unwrap();
            assert!(
                matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
                "expected MsgRollForward, got {msg:?}"
            );
        }

        // Simulate rollback: remove blocks at slots 20 and 30, add new fork block.
        let new_fork_block = make_hfc_block(7, &[0xF1]);
        {
            let mut blocks = blocks_ref.lock().unwrap();
            blocks.retain(|(s, _, _)| *s <= 10);
            blocks.push((25, [0xF1; 32], new_fork_block));
        }

        // Broadcast rollback announcement to slot 10.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 10,
                hash: [0x01; 32],
            })
            .unwrap();

        // The next MsgRequestNext should yield MsgRollBackward to slot 10.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollBackward {
            point, tip_slot, ..
        } = msg
        {
            assert_eq!(
                point,
                Point::Specific(10, [0x01; 32]),
                "rollback should target slot 10"
            );
            // Tip should reflect the new chain state (slot 25).
            assert_eq!(tip_slot, 25);
        } else {
            panic!("expected MsgRollBackward, got {msg:?}");
        }

        // Next MsgRequestNext should serve the new fork block at slot 25.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { tip_slot, .. } = msg {
            assert_eq!(tip_slot, 25, "should serve new fork block");
        } else {
            panic!("expected MsgRollForward for new fork, got {msg:?}");
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_during_await_reply() {
        // Scenario: follower is at the tip (in MsgAwaitReply/StMustReply state)
        // when a rollback occurs.  The server must send MsgRollBackward, not a
        // stale MsgRollForward.
        let block_a = make_hfc_block(7, &[0x0A]);

        let provider = MutableMockBlockProvider::new(vec![(10, [0x01; 32], block_a)]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve the only block.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Request next — will go into MsgAwaitReply since we're at the tip.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Read MsgAwaitReply.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgAwaitReply
        ));

        // Now broadcast a rollback to origin (slot 0).
        // The server is in StMustReply — it must respond with MsgRollBackward.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 0,
                hash: [0u8; 32],
            })
            .unwrap();

        // The server should send MsgRollBackward to origin.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollBackward { point, .. } = msg {
            assert_eq!(point, Point::Origin, "should rollback to origin");
        } else {
            panic!("expected MsgRollBackward while in MsgAwaitReply, got {msg:?}");
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_cursor_behind_rollback_point_no_rollback_sent() {
        // Scenario: cursor is at slot 10, rollback occurs to slot 20.
        // Since the cursor is behind the rollback point, the server should
        // NOT send MsgRollBackward — the peer hasn't seen the rolled-back blocks.
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let block_c = make_hfc_block(7, &[0x0C]);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_a),
                (20, [0x02; 32], block_b),
                (30, [0x03; 32], block_c),
            ],
        };

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve only block at slot 10 (cursor now at slot 10).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Broadcast rollback to slot 20 — cursor is at slot 10, which is before
        // the rollback point.  No MsgRollBackward should be sent.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 20,
                hash: [0x02; 32],
            })
            .unwrap();

        // Next request should serve the block at slot 20 (MsgRollForward, not RollBackward).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
            "cursor behind rollback point should not trigger MsgRollBackward, got {msg:?}"
        );

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// **Bug J regression** (issue #500, 2026-05-16):
    ///
    /// Before this fix, the ChainSync server's `try_serve_next_block` used
    /// only `get_next_block_after_slot(cursor_slot)` to pick the next block
    /// to forward — slot-based, with no validation that the cursor's block
    /// was still on the canonical chain.  After a fork switch that replaced
    /// the in-flight chain with a competitor whose blocks are at *lower*
    /// slots than the cursor (typical for two BPs forging on diverged
    /// chains for several blocks before merging), this lookup silently
    /// skipped every new-chain block at or below the cursor's slot, and
    /// the downstream peer never received the bodies needed to reach the
    /// new tip.  Empirically: dugite-bp's volatile DB had no record of
    /// the relay-side competing chain's blocks 10..16 even though the
    /// relay had selected them after a fork switch.
    ///
    /// Fixed behaviour: when `cursor_hash` is not on the canonical chain,
    /// the server rewinds the cursor to the most recent on-chain ancestor
    /// by sending `MsgRollBackward` BEFORE serving any further blocks.
    /// Subsequent `MsgRequestNext`s then deliver the new chain's blocks
    /// in slot order — including those at slots ≤ the old cursor slot.
    #[tokio::test]
    async fn fork_switch_to_lower_slot_chain_sends_rollback_then_serves_new_blocks() {
        // Phase 1: build a 4-block chain A → B → C → D and serve it to the
        // client.  Hash bytes encode the block_no in the first byte.
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        let b_hash = [0x0B; 32];
        let c_hash = [0x0C; 32];
        let d_hash = [0x0D; 32];

        // Original chain: A@slot10 → B@slot20 → C@slot30 → D@slot40
        provider.push_on_chain(10, a_hash, genesis, 1, make_hfc_block(7, &[0xA1]));
        provider.push_on_chain(20, b_hash, a_hash, 2, make_hfc_block(7, &[0xB1]));
        provider.push_on_chain(30, c_hash, b_hash, 3, make_hfc_block(7, &[0xC1]));
        provider.push_on_chain(40, d_hash, c_hash, 4, make_hfc_block(7, &[0xD1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();
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
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap();

        for _ in 0..4 {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            assert!(matches!(
                decode_message(&resp).unwrap(),
                ChainSyncMessage::MsgRollForward { .. }
            ));
        }
        // Cursor now at D@slot40.

        // Phase 2: simulate the fork-switch on the relay side.  Build a new
        // chain A → X (slot15) → Y (slot25) → Z (slot35) → W (slot50).
        // The new chain's intermediate blocks (X, Y, Z) sit at slots BELOW
        // the cursor (40) — these are the blocks that the buggy code lost.
        let x_hash = [0x1A; 32];
        let y_hash = [0x2A; 32];
        let z_hash = [0x3A; 32];
        let w_hash = [0x4A; 32];
        provider.put_fork(15, x_hash, a_hash, 2, make_hfc_block(7, &[0xAA]));
        provider.put_fork(25, y_hash, x_hash, 3, make_hfc_block(7, &[0xBB]));
        provider.put_fork(35, z_hash, y_hash, 4, make_hfc_block(7, &[0xCC]));
        provider.put_fork(50, w_hash, z_hash, 5, make_hfc_block(7, &[0xDD]));
        provider.replace_chain(vec![a_hash, x_hash, y_hash, z_hash, w_hash]);

        // The cursor's block (D@slot40) is now a fork, no longer on chain.
        // Announce the new tip — server wakes, sees cursor off-chain via
        // `is_on_chain`, walks `find_chain_ancestor` back to A, and sends
        // MsgRollBackward(A@slot10).
        ann_tx
            .send(BlockAnnouncement {
                slot: 50,
                hash: w_hash,
                block_number: 5,
            })
            .unwrap();

        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        match msg {
            ChainSyncMessage::MsgRollBackward { point, .. } => {
                assert_eq!(
                    point,
                    Point::Specific(10, a_hash),
                    "must rewind to most recent on-chain ancestor"
                );
            }
            other => panic!(
                "Bug J regression: expected MsgRollBackward to A@10, got {other:?}.  \
                 The cursor was at D@40 which is no longer on the chain; \
                 serving any MsgRollForward here would skip the new chain's \
                 blocks at slots ≤ 40."
            ),
        }

        // Now the client (representing dbp) requests next blocks; the server
        // MUST deliver the entire new chain past A in slot order, including
        // X@15, Y@25, Z@35 — all of which are BELOW the original cursor
        // slot of 40.  Pre-fix, these would have been silently skipped.
        let expected_slots = [15u64, 25, 35, 50];
        for expected_slot in expected_slots {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&resp).unwrap();
            match msg {
                ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                    // The MsgRollForward payload doesn't expose the served
                    // block's slot directly (only the tip), but the cursor
                    // advances through the chain in order.  Verify the tip
                    // slot is 50 throughout (since the chain hasn't grown
                    // further), and that we received all four expected
                    // forwards.
                    assert_eq!(tip_slot, 50);
                    let _ = expected_slot;
                }
                other => panic!("expected MsgRollForward at slot {expected_slot}, got {other:?}"),
            }
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── first-block-from-origin regression tests ────────────────────────────

    /// Regression test for a bug where the ChainSync server re-served the
    /// very first block on every subsequent `MsgRequestNext`, causing the
    /// Haskell peer to reject it with `UnexpectedBlockNo`.
    ///
    /// Scenario:
    /// 1. Provider starts empty.
    /// 2. Client sends `MsgFindIntersect(Origin)` → server sets
    ///    `cursor_at_origin = true`.
    /// 3. Client sends `MsgRequestNext` → no block yet, server replies
    ///    `MsgAwaitReply` and waits on the announcement channel.
    /// 4. Producer forges block 0 (slot 1), the block is added to the
    ///    provider, and a `BlockAnnouncement` is broadcast.
    /// 5. Server wakes, serves block 0 via `MsgRollForward`, **must** clear
    ///    `cursor_at_origin`.
    /// 6. Client sends another `MsgRequestNext`.
    ///    EXPECTED: `MsgAwaitReply` (no more blocks).
    ///    BUG (pre-fix): `MsgRollForward` re-serving block 0, because
    ///    `cursor_at_origin` was never cleared in the announcement-wakeup
    ///    path, so `get_block_at_or_after_slot(0)` returned block 0 again.
    #[tokio::test]
    async fn first_block_not_served_twice_when_forged_mid_await() {
        let provider = MutableMockBlockProvider::new(vec![]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let blocks_ref = provider.blocks.clone();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Step 1 — find intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgIntersectFound { .. }
        ));

        // Step 2 — request next, no block yet → expect MsgAwaitReply.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                ChainSyncMessage::MsgAwaitReply
            ),
            "first MsgRequestNext with empty provider must produce MsgAwaitReply"
        );

        // Step 3 — forger produces block 0 at slot 1; commit to provider,
        // then broadcast the announcement (matches node/mod.rs ordering).
        let block0_cbor = make_hfc_block(7, &[0xB0]);
        {
            let mut blocks = blocks_ref.lock().unwrap();
            blocks.push((1, [0xB0; 32], block0_cbor));
        }
        ann_tx
            .send(BlockAnnouncement {
                slot: 1,
                hash: [0xB0; 32],
                block_number: 0,
            })
            .unwrap();

        // Step 4 — the server should wake and send MsgRollForward for block 0.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
            "expected MsgRollForward after announcement, got {msg:?}"
        );

        // Step 5 — second MsgRequestNext must NOT re-serve block 0.
        // Pre-fix bug: cursor_at_origin stayed true, so the direct-serve
        // path called get_block_at_or_after_slot(0) and served block 0
        // again, producing the Haskell error:
        //   HeaderError ... UnexpectedBlockNo (BlockNo 1) (BlockNo 0)
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgAwaitReply),
            "REGRESSION: second MsgRequestNext re-served the first block \
             instead of MsgAwaitReply; got {msg:?}"
        );

        // Server is now parked in MsgAwaitReply (in a 135-second select).
        // Aborting is the cheapest clean-up for this test; a graceful MsgDone
        // handshake would have to wait out the tip-wait timeout.
        handle.abort();
    }

    /// Verify that MsgIntersectNotFound is sent when no intersection exists.
    #[tokio::test]
    async fn find_intersect_not_found_when_no_blocks_match() {
        // Provider has a block at slot 50, but we ask for slot 999.
        let block_cbor = make_hfc_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(50, [0x01; 32], block_cbor)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Ask for a point we don't have.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            999, [0xFF; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgIntersectNotFound { .. }),
            "expected MsgIntersectNotFound, got {msg:?}"
        );

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgFindIntersect with Origin always finds an intersection.
    #[tokio::test]
    async fn find_intersect_origin_always_found() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] }; // empty chain
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgIntersectFound { point, .. } = msg {
            assert_eq!(
                point,
                Point::Origin,
                "intersection at origin should return Point::Origin"
            );
        } else {
            panic!("expected MsgIntersectFound, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify cursor state is set correctly after MsgFindIntersect with a specific point.
    #[tokio::test]
    async fn find_intersect_sets_cursor_correctly() {
        let block_cbor = make_hfc_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_cbor.clone()),
                (20, [0x02; 32], make_hfc_block(7, &[0x02])),
            ],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Intersect at slot 10 (block_a).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            10, [0x01; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Next block should be at slot 20 (not slot 10 again).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { tip_slot, .. } = msg {
            assert_eq!(
                tip_slot, 20,
                "after intersect at slot 10, next block should be slot 20"
            );
        } else {
            panic!("expected MsgRollForward after intersect, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgDone causes the server to exit cleanly.
    #[tokio::test]
    async fn msg_done_terminates_server() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        // Server should terminate cleanly.
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "server should exit Ok on MsgDone: {result:?}"
        );
    }

    /// Verify tip information is included correctly in MsgRollForward.
    #[tokio::test]
    async fn roll_forward_includes_tip_info() {
        let block_a = make_hfc_block(7, &[0x01]);
        let block_b = make_hfc_block(7, &[0x02]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block_a), (200, [0xBB; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap();

        // Request next — serve block at slot 100.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();

        if let ChainSyncMessage::MsgRollForward {
            tip_slot,
            tip_hash,
            tip_block_number,
            ..
        } = msg
        {
            // Tip should be the chain tip (slot 200), not the served block (slot 100).
            assert_eq!(tip_slot, 200, "tip_slot should reflect chain tip");
            assert_eq!(tip_hash, [0xBB; 32], "tip_hash should reflect chain tip");
            assert_eq!(
                tip_block_number, 2,
                "tip_block_number should reflect chain length"
            );
        } else {
            panic!("expected MsgRollForward, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that drain_rollback() returns None when channel is empty.
    #[test]
    fn drain_rollback_empty_channel_returns_none() {
        let (_tx, mut rx) = broadcast::channel::<RollbackAnnouncement>(4);
        let result = ChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_none(), "empty channel should return None");
    }

    /// Verify that drain_rollback() returns the latest of multiple queued rollbacks.
    #[test]
    fn drain_rollback_returns_latest_of_multiple() {
        let (tx, mut rx) = broadcast::channel::<RollbackAnnouncement>(8);

        // Queue 3 rollbacks — only the last should be returned.
        tx.send(RollbackAnnouncement {
            slot: 10,
            hash: [0x01; 32],
        })
        .unwrap();
        tx.send(RollbackAnnouncement {
            slot: 20,
            hash: [0x02; 32],
        })
        .unwrap();
        tx.send(RollbackAnnouncement {
            slot: 30,
            hash: [0x03; 32],
        })
        .unwrap();

        let result = ChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_some());
        let rb = result.unwrap();
        assert_eq!(rb.slot, 30, "should return the latest (slot 30) rollback");
        assert_eq!(rb.hash, [0x03; 32]);
    }

    /// Verify that the server handles MsgFindIntersect with multiple points,
    /// finding the most recent matching point.
    #[tokio::test]
    async fn find_intersect_picks_most_recent_matching_point() {
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(10, [0x0A; 32], block_a), (20, [0x0B; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Send two points in order: [slot=20 (exists), slot=10 (also exists), Origin].
        // The server should match the first one it finds (slot 20).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![
            Point::Specific(20, [0x0B; 32]),
            Point::Specific(10, [0x0A; 32]),
            Point::Origin,
        ]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgIntersectFound { point, .. } = msg {
            assert_eq!(
                point,
                Point::Specific(20, [0x0B; 32]),
                "should match the first (most recent) known point"
            );
        } else {
            panic!("expected MsgIntersectFound, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that extract_header_for_chainsync returns error for invalid era tag.
    ///
    /// Storage era tag 100 is not a valid Cardano era and should produce an error
    /// from `storage_era_tag_to_hfc_index`.
    #[test]
    fn extract_header_invalid_era_tag_returns_error() {
        // Build block with era_tag=100 (invalid).
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(100).unwrap(); // invalid era tag
        enc.array(5).unwrap();
        enc.bytes(&[0x01]).unwrap();
        enc.array(0).unwrap();
        enc.array(0).unwrap();
        enc.null().unwrap();
        enc.array(0).unwrap();

        let result = extract_header_for_chainsync(&buf);
        assert!(
            result.is_err(),
            "era_tag=100 should produce an error, got: {result:?}"
        );
    }

    /// Pin Haskell parity: per-server StMustReply timeout draw must fall in
    /// `[601 s, 911 s]` (`minChainSyncTimeout`, `maxChainSyncTimeout`).
    /// Issue #701.  This is a single-sample bound check on the value drawn at
    /// `ChainSyncServer::new()`; the `draws_distinct_across_constructions`
    /// test below verifies the randomization itself.
    #[test]
    fn must_reply_timeout_within_haskell_range() {
        let s = ChainSyncServer::new();
        let t = s.must_reply_timeout();
        assert!(
            t >= CHAINSYNC_MUST_REPLY_TIMEOUT_MIN && t <= CHAINSYNC_MUST_REPLY_TIMEOUT_MAX,
            "timeout {t:?} not in [{:?}, {:?}]",
            CHAINSYNC_MUST_REPLY_TIMEOUT_MIN,
            CHAINSYNC_MUST_REPLY_TIMEOUT_MAX,
        );
    }

    /// Haskell-parity constants pinned.
    #[test]
    fn must_reply_timeout_constants_pinned() {
        assert_eq!(
            CHAINSYNC_MUST_REPLY_TIMEOUT_MIN,
            Duration::from_secs(601),
            "Haskell minChainSyncTimeout"
        );
        assert_eq!(
            CHAINSYNC_MUST_REPLY_TIMEOUT_MAX,
            Duration::from_secs(911),
            "Haskell maxChainSyncTimeout"
        );
    }

    /// The per-connection draw must produce at least two distinct timeouts
    /// across 64 constructions.  Statistically the probability of every
    /// draw landing on the same millisecond out of 310 000 ms is
    /// `(1/310_000)^63 ≈ 10^-350` — astronomical.  If this ever fails the
    /// RNG is deterministic, which would synchronize re-poll storms across
    /// the population.
    #[test]
    fn must_reply_timeout_draws_distinct_across_constructions() {
        let mut seen: std::collections::HashSet<u128> = std::collections::HashSet::new();
        for _ in 0..64 {
            seen.insert(ChainSyncServer::new().must_reply_timeout().as_micros());
        }
        assert!(
            seen.len() >= 2,
            "must_reply_timeout draws all identical across 64 constructions: RNG is broken"
        );
    }

    /// Regression lock: MsgAwaitReply is sent when at the tip (no blocks available).
    ///
    /// This verifies the wakeup-race boundary: when the server has no next block,
    /// it MUST send MsgAwaitReply before blocking in the select!{} loop.
    /// Missing this message leaves the peer in StCanAwait with no reply — the
    /// SingMustReply timeout (135s) described in project_chainsync_server_silence.md.
    #[tokio::test]
    async fn await_reply_sent_when_at_tip() {
        let block = make_hfc_block(7, &[0xAA]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block)],
        };
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Intersect at origin, serve block, then reach tip.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap(); // MsgRollForward
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Now at tip — send another MsgRequestNext.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // CRITICAL: server MUST send MsgAwaitReply before blocking.
        // If it doesn't, we'd hang here — verifying the wakeup race doesn't
        // prevent the mandatory MsgAwaitReply from being sent.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgAwaitReply),
            "REGRESSION: server did not send MsgAwaitReply at tip — \
             this causes the SingMustReply timeout (project_chainsync_server_silence.md); \
             got {msg:?}"
        );

        handle.abort();
    }

    /// Regression test for stale-announcement draining at the top of
    /// `handle_request_next`.
    ///
    /// Scenario: the producer has already forged and stored block 0, but the
    /// broadcast is queued in `announcement_rx` because the server was in the
    /// middle of handling a previous message. On the next `MsgRequestNext`:
    /// 1. The server must drain the buffered announcement.
    /// 2. Serve block 0 via the direct path (using the authoritative
    ///    `BlockProvider` lookup).
    /// 3. On the subsequent `MsgRequestNext`, the server must serve block 1
    ///    (NOT re-serve block 0 from a stale queued announcement).
    #[tokio::test]
    async fn pre_queued_announcement_does_not_cause_duplicate_first_block() {
        let block0 = make_hfc_block(7, &[0xB0]);
        let block1 = make_hfc_block(7, &[0xB1]);
        let provider =
            MutableMockBlockProvider::new(vec![(1, [0xB0; 32], block0), (2, [0xB1; 32], block1)]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        // Queue a stale announcement for block 0 before the server starts —
        // this simulates a forge event that happened while the server was
        // still setting up.
        ann_tx
            .send(BlockAnnouncement {
                slot: 1,
                hash: [0xB0; 32],
                block_number: 0,
            })
            .unwrap();

        let mut server = ChainSyncServer::new();
        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // First MsgRequestNext: server drains the stale announcement for
        // block 0, then serves block 0 via the direct path.  The drain
        // prevents the stale announcement from re-waking the server on the
        // next MsgRequestNext.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Second MsgRequestNext: must serve block 1 (slot 2), not re-serve
        // block 0.  Before the fix, the stale queued announcement for block
        // 0 could wake the server in the await branch and cause a duplicate.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        match msg {
            ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                assert_eq!(tip_slot, 2, "second serve must advance past block 0");
            }
            other => panic!("expected MsgRollForward for block 1, got {other:?}"),
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── #869: StMustReply silent no-reply ───────────────────────────────────

    /// Regression test for #869: a spurious announcement wake (nothing new
    /// past the cursor) must NOT cause `handle_request_next` to return
    /// without sending a message.
    ///
    /// Per the Ouroboros ChainSync state machine, once `MsgAwaitReply` has
    /// been sent the client is in `StMustReply` and will not send another
    /// `MsgRequestNext` until it receives a reply.  Pre-fix, a spurious wake
    /// (e.g. an announcement whose content doesn't correspond to anything
    /// new past the cursor) caused the server to silently return `Ok(())`
    /// without sending anything — the outer loop would then block on
    /// `channel.recv()` waiting for a client message that, per protocol,
    /// will never arrive.  This test drives that exact sequence without ever
    /// sending a second `MsgRequestNext`, proving the server keeps waiting
    /// on the SAME `handle_request_next` call until it has something to send.
    #[tokio::test]
    async fn spurious_announcement_wake_does_not_return_agency_without_reply() {
        let block_a = make_hfc_block(7, &[0x0A]);
        let provider = MutableMockBlockProvider::new(vec![(10, [0x01; 32], block_a)]);
        let blocks_ref = provider.blocks.clone();

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();
        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Intersect at origin, serve the only block — cursor now at slot 10.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Request next again — at tip, enters StMustReply.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgAwaitReply
        ));

        // Fire a SPURIOUS announcement: nothing new is added to the block
        // store, so `try_serve_next_block` finds nothing to serve.
        ann_tx
            .send(BlockAnnouncement {
                slot: 10,
                hash: [0x01; 32],
                block_number: 1,
            })
            .unwrap();

        // Give the server a moment to process the spurious wake.  Pre-fix,
        // this would cause `handle_request_next` to return without sending
        // anything, wedging the connection.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), egress_rx.recv())
                .await
                .is_err(),
            "server must not send anything on a spurious wake with nothing new to serve"
        );

        // Now push a genuine new block and announce it — the SAME
        // `handle_request_next` call (no second MsgRequestNext from the
        // client) must eventually deliver it.
        let block_b = make_hfc_block(7, &[0x0B]);
        blocks_ref.lock().unwrap().push((20, [0x02; 32], block_b));
        ann_tx
            .send(BlockAnnouncement {
                slot: 20,
                hash: [0x02; 32],
                block_number: 2,
            })
            .unwrap();

        let (_, _, resp) = tokio::time::timeout(Duration::from_secs(5), egress_rx.recv())
            .await
            .expect("server must eventually reply once a real block is available")
            .unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                ChainSyncMessage::MsgRollForward { .. }
            ),
            "server must serve the genuine block without a second MsgRequestNext"
        );

        handle.abort();
    }

    // ─── #876: MsgFindIntersect fork/slot validation ─────────────────────────

    /// #876: a client claiming the wrong slot for a real on-chain hash must
    /// NOT get `MsgIntersectFound` — accepting it would poison the follower
    /// cursor with a `(claimed_slot, real_hash)` pair that doesn't correspond
    /// to any actual chain point (e.g. `(u64::MAX, real_hash)` would wedge
    /// every subsequent `try_serve_next_block` lookup).
    #[tokio::test]
    async fn find_intersect_wrong_claimed_slot_rejected() {
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        provider.push_on_chain(10, a_hash, genesis, 1, make_hfc_block(7, &[0xA1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // a_hash is real and on-chain, but at slot 10 — not the claimed 999.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            999, a_hash,
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgIntersectNotFound { .. }),
            "wrong claimed slot for a real hash must be rejected, got {msg:?}"
        );

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// #876: a hash that exists in storage but is NOT on the canonical chain
    /// (a fork block) must not be accepted as an intersection point — only
    /// `is_on_chain` hashes may become the follower cursor.
    #[tokio::test]
    async fn find_intersect_fork_only_hash_rejected() {
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        provider.push_on_chain(10, a_hash, genesis, 1, make_hfc_block(7, &[0xA1]));
        let fork_hash = [0xF0; 32];
        provider.put_fork(15, fork_hash, a_hash, 2, make_hfc_block(7, &[0xF1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            15, fork_hash,
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgIntersectNotFound { .. }),
            "fork-only hash must not be accepted as an intersection point, got {msg:?}"
        );

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// #876: a correct canonical point (real on-chain hash + matching slot)
    /// must still be accepted — the fix must not be overly strict.
    #[tokio::test]
    async fn find_intersect_correct_canonical_point_found() {
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        let b_hash = [0x0B; 32];
        provider.push_on_chain(10, a_hash, genesis, 1, make_hfc_block(7, &[0xA1]));
        provider.push_on_chain(20, b_hash, a_hash, 2, make_hfc_block(7, &[0xB1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            10, a_hash,
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        match msg {
            ChainSyncMessage::MsgIntersectFound { point, .. } => {
                assert_eq!(point, Point::Specific(10, a_hash));
            }
            other => {
                panic!("expected MsgIntersectFound for correct canonical point, got {other:?}")
            }
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }
}
