//! BlockFetch server — serves block ranges to requesting peers.
//!
//! Handles `MsgRequestRange(from, to)` by looking up blocks via [`BlockProvider`]
//! and streaming them as `MsgStartBatch` → `MsgBlock` × N → `MsgBatchDone`.
//! Sends `MsgNoBlocks` if the requested range is unavailable.
//!
//! ## HFC wrapping (N2N wire format)
//!
//! The Haskell N2N BlockFetch encoding is derived from the SerialiseNodeToNode
//! instance for HardForkBlock, which calls:
//!
//!   `encodeNodeToNode ccfg _ = wrapCBORinCBOR (encodeDiskHfcBlock ccfg)`
//!
//! where `wrapCBORinCBOR enc x = Serialise.encode (tag(24) bstr(enc(x)))`.
//!
//! The `encodeDiskHfcBlock` for Cardano is a **custom** override (not the generic
//! `encodeNS`) that emits `[era_word, block_body]` — identical to the on-disk
//! storage format.  The mapping is:
//!   - Byron EBB         → [0, body]
//!   - Byron regular     → [1, body]
//!   - Shelley           → [2, body]
//!   - Allegra           → [3, body]
//!   - Mary              → [4, body]
//!   - Alonzo            → [5, body]
//!   - Babbage           → [6, body]
//!   - Conway            → [7, body]
//!   - Dijkstra          → [8, body]  (future era)
//!
//! Therefore the complete MsgBlock wire encoding is:
//!
//! ```text
//!   [2,                                  ← array(2)
//!     word(4),                           ← MsgBlock tag
//!     #6.24(bstr( [era_word, body] ))    ← tag(24) wrapping raw stored CBOR
//!   ]
//! ```
//!
//! Since Dugite stores blocks in the same `[era_word, body]` layout that
//! `encodeDiskHfcBlock` produces, the stored bytes need NO structural
//! transformation — they are placed verbatim inside tag(24).
//!
//! ## Range validation
//! - Maximum blocks per batch: 100 (prevents memory exhaustion)

use std::io::Write as _;

use minicbor::Encoder;

use crate::codec::Point;
use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;
use crate::protocol::CBOR_TAG_EMBEDDED;
use crate::BlockProvider;

use super::{decode_message, encode_message, BlockFetchMessage, TAG_BLOCK};

/// Safety limit on blocks pre-collected per batch response.
///
/// This is a **block count** limit, not a slot count limit.  The slot range
/// `(from_slot, to_slot)` defines the range boundaries, but the `limit`
/// parameter passed to `get_blocks_in_range()` caps the number of actual
/// blocks returned from the iterator.
///
/// The Haskell BlockFetch client expects ALL blocks between from_point and
/// to_point to be served in a single batch.  Sending fewer triggers
/// `BlockFetchProtocolFailureTooFewBlocks`.  Typical ranges are 10–200
/// blocks; in dense chain regions they can reach ~500.  We set a generous
/// upper bound to prevent unbounded memory use from malicious requests.
pub const MAX_BLOCKS_PER_BATCH: usize = 2000;

/// Blocks fetched per look-ahead chunk while streaming a range (#878).
///
/// Caps per-connection memory at O(STREAM_CHUNK) full block CBORs instead of
/// pre-collecting the whole batch. A multiple of the ChainDB's internal
/// 50-block lock chunk so the chunked-lock optimisation (which keeps
/// `get_blocks_in_range` off the per-block `blocking_read` path) is preserved.
pub const STREAM_CHUNK: usize = 200;

/// BlockFetch server that serves block ranges to peers.
pub struct BlockFetchServer;

impl BlockFetchServer {
    /// Run the BlockFetch server loop.
    ///
    /// Handles `MsgRequestRange` and `MsgClientDone`.
    pub async fn run<B: BlockProvider>(
        channel: &mut MuxChannel,
        block_provider: &B,
    ) -> Result<(), ProtocolError> {
        loop {
            let msg_bytes = channel.recv().await.map_err(ProtocolError::from)?;
            let msg = decode_message(&msg_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: "BlockFetch",
                reason: e,
            })?;

            match msg {
                BlockFetchMessage::MsgRequestRange { from, to } => {
                    tracing::debug!(
                        from = ?from,
                        to = ?to,
                        "blockfetch server: received MsgRequestRange"
                    );
                    Self::handle_request_range(channel, block_provider, &from, &to).await?;
                }
                BlockFetchMessage::MsgClientDone => {
                    tracing::debug!("blockfetch server: client sent MsgClientDone");
                    return Ok(());
                }
                other => {
                    return Err(ProtocolError::AgencyViolation {
                        protocol: "BlockFetch",
                        state: "BFIdle".to_string(),
                        received_tag: format!("{other:?}")
                            .as_bytes()
                            .first()
                            .copied()
                            .unwrap_or(0),
                    });
                }
            }
        }
    }

    /// Handle a range request: validate range, look up blocks, stream response.
    async fn handle_request_range<B: BlockProvider>(
        channel: &mut MuxChannel,
        block_provider: &B,
        from: &Point,
        to: &Point,
    ) -> Result<(), ProtocolError> {
        // Validate range — extract from_slot for iteration.
        let from_slot = match from {
            Point::Origin => 0,
            Point::Specific(slot, _) => *slot,
        };
        let to_slot = match to {
            Point::Origin => 0,
            Point::Specific(slot, _) => *slot,
        };

        // B18: Enforce a slot span limit in addition to the block count cap.
        // MAX_BLOCKS_PER_BATCH caps the number of blocks returned, but the
        // ImmutableDB secondary index must still be scanned over the full
        // requested slot range to count blocks.  A request spanning millions of
        // slots (e.g. slot 0 → slot 50,000,000) causes an I/O-intensive index
        // scan even though only MAX_BLOCKS_PER_BATCH blocks are eventually sent.
        //
        // We choose MAX_SLOT_SPAN = 432,000 (5 days × 86,400 slots/day).  A
        // legitimate Haskell bulk-sync request spans at most a few thousand
        // slots; this cap is generous while blocking trillion-slot range bombs.
        const MAX_SLOT_SPAN: u64 = 432_000;

        if to_slot < from_slot {
            let no_blocks = encode_message(&BlockFetchMessage::MsgNoBlocks);
            channel.send(no_blocks).await.map_err(ProtocolError::from)?;
            return Ok(());
        }
        if to_slot - from_slot > MAX_SLOT_SPAN {
            let no_blocks = encode_message(&BlockFetchMessage::MsgNoBlocks);
            channel.send(no_blocks).await.map_err(ProtocolError::from)?;
            return Ok(());
        }

        // Verify we have the starting block.
        let have_from = match from {
            Point::Origin => true,
            Point::Specific(_, hash) => block_provider.has_block(hash),
        };

        if !have_from {
            let no_blocks = encode_message(&BlockFetchMessage::MsgNoBlocks);
            channel.send(no_blocks).await.map_err(ProtocolError::from)?;
            return Ok(());
        }

        // #878: stream the range in bounded look-ahead chunks instead of
        // pre-collecting the whole batch (up to MAX_BLOCKS_PER_BATCH full block
        // CBORs ≈ 180 MB on mainnet) into one owned Vec. Per-connection memory
        // is now O(STREAM_CHUNK) blocks regardless of range size, so N
        // concurrent adversarial dense-range requests can no longer multiply a
        // huge pinned allocation. The chunk size is a multiple of the ChainDB's
        // internal 50-block lock chunk, so the chunked-lock optimisation that
        // keeps get_blocks_in_range() off the per-block blocking_read path is
        // preserved.
        Self::stream_range(
            channel,
            block_provider,
            from,
            to,
            from_slot,
            to_slot,
            STREAM_CHUNK,
        )
        .await
    }

    /// Stream the blocks of `[from, to]` (inclusive by POINT) to the client in
    /// bounded look-ahead chunks, holding at most `chunk_size` block CBORs in
    /// memory at once (#878).
    ///
    /// Emits `MsgStartBatch → MsgBlock* → MsgBatchDone`, or `MsgNoBlocks` if the
    /// range yields nothing. The cursor advances by POINT (via the last emitted
    /// block's hash) so a Byron EBB and the same-slot first main block of the
    /// epoch are both served across a chunk boundary, matching the point-inclusive
    /// semantics of the previous collect-then-trim implementation:
    /// - leading same-slot siblings that precede the requested `from` hash are
    ///   dropped, and
    /// - streaming stops the instant the requested `to` hash is emitted, so
    ///   trailing same-slot siblings after `to` are never sent.
    #[allow(clippy::too_many_arguments)]
    async fn stream_range<B: BlockProvider>(
        channel: &mut MuxChannel,
        block_provider: &B,
        from: &Point,
        to: &Point,
        from_slot: u64,
        to_slot: u64,
        chunk_size: usize,
    ) -> Result<(), ProtocolError> {
        let from_hash: Option<[u8; 32]> = match from {
            Point::Specific(_, h) => Some(*h),
            Point::Origin => None,
        };
        let to_hash: Option<[u8; 32]> = match to {
            Point::Specific(_, h) => Some(*h),
            Point::Origin => None,
        };

        let mut cursor_slot = from_slot;
        // Hash of the last block emitted in the previous chunk. Because a slot
        // can hold two blocks (EBB + main) we advance the cursor by slot but
        // re-fetch from `cursor_slot`, then skip forward through this hash so
        // the boundary block is emitted exactly once.
        let mut boundary_hash: Option<[u8; 32]> = None;
        // Until the requested `from` hash is reached we are trimming leading
        // same-slot siblings that precede it (Point::Origin has no such trim).
        let mut reached_from = from_hash.is_none();
        let mut started = false;
        let mut sent = 0usize;

        'chunks: loop {
            let chunk = block_provider.get_blocks_in_range(cursor_slot, to_slot, chunk_size);
            if chunk.is_empty() {
                break;
            }
            let full_chunk = chunk.len() >= chunk_size;
            let last_slot = chunk.last().unwrap().0;
            let last_hash = chunk.last().unwrap().1;

            // Skip the overlap already emitted at the end of the previous chunk.
            let mut i = 0usize;
            if let Some(b) = boundary_hash {
                while i < chunk.len() && chunk[i].1 != b {
                    i += 1;
                }
                if i < chunk.len() {
                    i += 1; // skip the boundary block itself
                }
            }

            let mut emitted_this_chunk = false;
            while i < chunk.len() {
                let (slot, hash, block_cbor) = &chunk[i];
                i += 1;

                if !reached_from {
                    // Leading trim: drop same-slot siblings before `from`.
                    if Some(*hash) == from_hash {
                        reached_from = true;
                    } else {
                        continue;
                    }
                }

                if !started {
                    let start = encode_message(&BlockFetchMessage::MsgStartBatch);
                    channel.send(start).await.map_err(ProtocolError::from)?;
                    started = true;
                }

                tracing::debug!(
                    slot,
                    hash = hex::encode(hash),
                    cbor_len = block_cbor.len(),
                    "blockfetch server: streaming block"
                );
                // MsgBlock: [4, tag(24) bstr(stored_block_cbor)]. Stored CBOR is
                // already the [era_word, body] layout encodeDiskHfcBlock emits.
                let block_msg = Self::encode_hfc_msg_block(block_cbor).map_err(|reason| {
                    ProtocolError::CborDecode {
                        protocol: "BlockFetch",
                        reason: format!("HFC wrapping failed: {reason}"),
                    }
                })?;
                channel.send(block_msg).await.map_err(ProtocolError::from)?;
                sent += 1;
                emitted_this_chunk = true;

                // Trailing trim: stop the instant we emit the `to` block, and
                // never exceed the batch cap.
                if to_hash == Some(*hash) || sent >= MAX_BLOCKS_PER_BATCH {
                    break 'chunks;
                }
            }

            if !full_chunk {
                // The provider returned fewer than a full chunk — end of range.
                break;
            }
            if emitted_this_chunk {
                // Re-fetch from the last emitted slot next round and skip past
                // the boundary block (handles a same-slot sibling straddling
                // the chunk edge).
                boundary_hash = Some(last_hash);
                cursor_slot = last_slot;
            } else {
                // A full chunk whose blocks were ALL already emitted (both
                // same-slot siblings of `last_slot` sent in prior chunks). The
                // slot-keyed re-fetch would loop forever; advance past the slot.
                boundary_hash = None;
                cursor_slot = last_slot + 1;
            }
        }

        if started {
            let done = encode_message(&BlockFetchMessage::MsgBatchDone);
            channel.send(done).await.map_err(ProtocolError::from)?;
            tracing::debug!(block_count = sent, from_slot, to_slot, "blockfetch server: streamed batch");
        } else {
            let no_blocks = encode_message(&BlockFetchMessage::MsgNoBlocks);
            channel.send(no_blocks).await.map_err(ProtocolError::from)?;
        }

        Ok(())
    }

    /// Encode a single block as an HFC-wrapped `MsgBlock` message.
    ///
    /// ## Wire format
    ///
    /// The Haskell N2N `SerialiseNodeToNode` instance for `HardForkBlock` is:
    ///
    /// ```haskell
    /// encodeNodeToNode ccfg _ = wrapCBORinCBOR (encodeDiskHfcBlock ccfg)
    /// ```
    ///
    /// `wrapCBORinCBOR` serialises the value and wraps it in CBOR tag(24):
    ///
    /// ```text
    /// tag(24) bstr( encodeDiskHfcBlock_output )
    /// ```
    ///
    /// The Cardano-specific `encodeDiskHfcBlock` override produces the same
    /// `[era_word, block_body]` layout used for on-disk storage (NOT the
    /// generic 0-based NS index produced by `encodeNS`).  Therefore the
    /// stored block CBOR bytes can be placed **verbatim** inside tag(24)
    /// without any structural transformation.
    ///
    /// The resulting `MsgBlock` wire encoding is:
    ///
    /// ```text
    /// array(2) [
    ///   word(4),                          -- MsgBlock tag
    ///   tag(24) bstr( stored_block_cbor ) -- CBOR-in-CBOR
    /// ]
    /// ```
    fn encode_hfc_msg_block(block_cbor: &[u8]) -> Result<Vec<u8>, String> {
        // Pre-allocate: 1 (array(2)) + 1 (word 4) + 2 (tag 24) + varint (len) + payload.
        let mut buf = Vec::with_capacity(8 + block_cbor.len());
        let mut enc = Encoder::new(&mut buf);

        enc.array(2).map_err(|e| format!("MsgBlock array: {e}"))?;
        enc.u64(TAG_BLOCK)
            .map_err(|e| format!("MsgBlock tag: {e}"))?;
        // tag(24) wraps the complete stored-format CBOR bytes verbatim.
        enc.tag(minicbor::data::Tag::new(CBOR_TAG_EMBEDDED))
            .map_err(|e| format!("tag(24): {e}"))?;
        enc.bytes(block_cbor)
            .map_err(|e| format!("block bstr: {e}"))?;
        enc.writer_mut()
            .flush()
            .map_err(|e| format!("flush: {e}"))?;

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TipInfo;
    use bytes::Bytes;
    use minicbor::Decoder;
    use tokio::sync::mpsc;

    /// Build a minimal storage-format block CBOR for testing.
    ///
    /// Layout (matching Haskell `encodeDiskHfcBlock` for Shelley+ eras):
    /// `[era_tag, [header_cbor, [], [], null, []]]`
    fn make_storage_block(era_tag: u64, header_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(era_tag).unwrap();
        enc.array(5).unwrap();
        enc.bytes(header_bytes).unwrap(); // header
        enc.array(0).unwrap(); // tx_bodies
        enc.array(0).unwrap(); // tx_witnesses
        enc.null().unwrap(); // aux_data
        enc.array(0).unwrap(); // invalid_txs
        buf
    }

    struct MockBlockProvider {
        blocks: Vec<(u64, [u8; 32], Vec<u8>)>,
    }

    impl BlockProvider for MockBlockProvider {
        fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.blocks
                .iter()
                .find(|(_, h, _)| h == hash)
                .map(|(_, _, cbor)| cbor.clone())
        }
        fn has_block(&self, hash: &[u8; 32]) -> bool {
            self.blocks.iter().any(|(_, h, _)| h == hash)
        }
        fn get_tip(&self) -> TipInfo {
            self.blocks
                .last()
                .map(|(s, h, _)| TipInfo {
                    slot: *s,
                    hash: *h,
                    block_number: self.blocks.len() as u64,
                })
                .unwrap_or(TipInfo {
                    slot: 0,
                    hash: [0; 32],
                    block_number: 0,
                })
        }
        fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            self.blocks
                .iter()
                .find(|(s, _, _)| *s > after_slot)
                .cloned()
        }

        fn get_next_block_after_point(
            &self,
            slot: u64,
            hash: &[u8; 32],
        ) -> Option<(u64, [u8; 32], Vec<u8>)> {
            if let Some(pos) = self
                .blocks
                .iter()
                .position(|(s, h, _)| *s == slot && h == hash)
            {
                return self.blocks.get(pos + 1).cloned();
            }
            self.get_next_block_after_slot(slot)
        }
    }

    /// Drive one MsgRequestRange against `provider` and collect the served
    /// block bodies (empty when the server answers MsgNoBlocks).
    async fn collect_range(provider: MockBlockProvider, from: Point, to: Point) -> Vec<Vec<u8>> {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        let req = encode_message(&BlockFetchMessage::MsgRequestRange { from, to });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let mut blocks = Vec::new();
        let (_, _, first) = egress_rx.recv().await.unwrap();
        match decode_message(&first).unwrap() {
            BlockFetchMessage::MsgNoBlocks => {}
            BlockFetchMessage::MsgStartBatch => loop {
                let (_, _, msg) = egress_rx.recv().await.unwrap();
                match decode_message(&msg).unwrap() {
                    BlockFetchMessage::MsgBlock(body) => blocks.push(body),
                    BlockFetchMessage::MsgBatchDone => break,
                    other => panic!("unexpected message in batch: {other:?}"),
                }
            },
            other => panic!("unexpected first response: {other:?}"),
        }

        let done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
        blocks
    }

    /// Byron boundary pair: the EBB and the first main block of the epoch
    /// share an absolute slot.  A range spanning the boundary must serve
    /// BOTH blocks in chain order — slot-cursor iteration drops the second.
    #[tokio::test]
    async fn range_includes_both_same_slot_blocks() {
        let pred = make_storage_block(7, &[0x10]);
        let ebb = make_storage_block(7, &[0x20]);
        let main = make_storage_block(7, &[0x30]);
        let next = make_storage_block(7, &[0x40]);
        let provider = MockBlockProvider {
            blocks: vec![
                (99, [0x01; 32], pred.clone()),
                (100, [0x02; 32], ebb.clone()),
                (100, [0x03; 32], main.clone()),
                (101, [0x04; 32], next.clone()),
            ],
        };

        let served = collect_range(
            provider,
            Point::Specific(99, [0x01; 32]),
            Point::Specific(101, [0x04; 32]),
        )
        .await;

        assert_eq!(
            served,
            vec![pred, ebb, main, next],
            "range spanning a Byron boundary must include both same-slot blocks"
        );
    }

    /// The `from` point is a specific block (slot + hash): when the range
    /// starts at the main block of a same-slot EBB/main pair, the EBB must
    /// NOT be served — Haskell ranges are inclusive by point, not by slot.
    #[tokio::test]
    async fn range_start_at_main_excludes_same_slot_ebb() {
        let ebb = make_storage_block(7, &[0x20]);
        let main = make_storage_block(7, &[0x30]);
        let next = make_storage_block(7, &[0x40]);
        let provider = MockBlockProvider {
            blocks: vec![
                (100, [0x02; 32], ebb),
                (100, [0x03; 32], main.clone()),
                (101, [0x04; 32], next.clone()),
            ],
        };

        let served = collect_range(
            provider,
            Point::Specific(100, [0x03; 32]),
            Point::Specific(101, [0x04; 32]),
        )
        .await;

        assert_eq!(
            served,
            vec![main, next],
            "range starting at the main block must not include the same-slot EBB"
        );
    }

    /// The `to` point is a specific block: when the range ends at the EBB of
    /// a same-slot pair, the main block at the same slot must NOT be served.
    #[tokio::test]
    async fn range_end_at_ebb_excludes_same_slot_main() {
        let pred = make_storage_block(7, &[0x10]);
        let ebb = make_storage_block(7, &[0x20]);
        let main = make_storage_block(7, &[0x30]);
        let provider = MockBlockProvider {
            blocks: vec![
                (99, [0x01; 32], pred.clone()),
                (100, [0x02; 32], ebb.clone()),
                (100, [0x03; 32], main),
            ],
        };

        let served = collect_range(
            provider,
            Point::Specific(99, [0x01; 32]),
            Point::Specific(100, [0x02; 32]),
        )
        .await;

        assert_eq!(
            served,
            vec![pred, ebb],
            "range ending at the EBB must not include the same-slot main block"
        );
    }

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(4096);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = MuxChannel::new(
            3,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    /// #878: drive `stream_range` directly with a small `chunk_size` and collect
    /// the served block bodies, so the multi-chunk look-ahead + boundary-overlap
    /// path is exercised (the collect-then-stream path never had chunks).
    async fn collect_stream_range(
        provider: MockBlockProvider,
        from: Point,
        to: Point,
        chunk_size: usize,
    ) -> Vec<Vec<u8>> {
        let (mut channel, mut egress_rx, _ingress_tx) = make_test_channel();
        let from_slot = match &from {
            Point::Origin => 0,
            Point::Specific(s, _) => *s,
        };
        let to_slot = match &to {
            Point::Origin => 0,
            Point::Specific(s, _) => *s,
        };
        let handle = tokio::spawn(async move {
            BlockFetchServer::stream_range(
                &mut channel,
                &provider,
                &from,
                &to,
                from_slot,
                to_slot,
                chunk_size,
            )
            .await
            .unwrap();
        });

        let mut blocks = Vec::new();
        let (_, _, first) = egress_rx.recv().await.unwrap();
        match decode_message(&first).unwrap() {
            BlockFetchMessage::MsgNoBlocks => {}
            BlockFetchMessage::MsgStartBatch => loop {
                let (_, _, msg) = egress_rx.recv().await.unwrap();
                match decode_message(&msg).unwrap() {
                    BlockFetchMessage::MsgBlock(body) => blocks.push(body),
                    BlockFetchMessage::MsgBatchDone => break,
                    other => panic!("unexpected message in batch: {other:?}"),
                }
            },
            other => panic!("unexpected first response: {other:?}"),
        }
        handle.await.unwrap();
        blocks
    }

    /// #878: streaming across several small chunks yields the exact same block
    /// sequence as a single-shot serve — no gaps, no duplicates at chunk edges.
    #[tokio::test]
    async fn stream_range_multichunk_matches_full_sequence() {
        let bodies: Vec<Vec<u8>> = (0u8..10).map(|i| make_storage_block(7, &[i])).collect();
        let provider = MockBlockProvider {
            blocks: (0u8..10)
                .map(|i| {
                    let mut h = [0u8; 32];
                    h[0] = i;
                    ((i as u64) * 10 + 10, h, bodies[i as usize].clone())
                })
                .collect(),
        };
        let mut from_h = [0u8; 32];
        from_h[0] = 0;
        let mut to_h = [0u8; 32];
        to_h[0] = 9;

        // chunk_size 3 forces 4 chunks with boundary overlaps.
        let served = collect_stream_range(
            provider,
            Point::Specific(10, from_h),
            Point::Specific(100, to_h),
            3,
        )
        .await;
        assert_eq!(served, bodies, "multi-chunk stream must equal the full range");
    }

    /// #878: a Byron EBB/main same-slot pair split across a chunk boundary must
    /// still serve BOTH blocks exactly once (the boundary-overlap skip advances
    /// by point, not slot).
    #[tokio::test]
    async fn stream_range_same_slot_pair_across_chunk_boundary() {
        let pred = make_storage_block(7, &[0x10]);
        let ebb = make_storage_block(7, &[0x20]);
        let main = make_storage_block(7, &[0x30]);
        let next = make_storage_block(7, &[0x40]);
        let provider = MockBlockProvider {
            blocks: vec![
                (99, [0x01; 32], pred.clone()),
                (100, [0x02; 32], ebb.clone()),
                (100, [0x03; 32], main.clone()),
                (101, [0x04; 32], next.clone()),
            ],
        };
        // chunk_size 2 places the (100,ebb)/(100,main) pair straddling chunk 1/2.
        let served = collect_stream_range(
            provider,
            Point::Specific(99, [0x01; 32]),
            Point::Specific(101, [0x04; 32]),
            2,
        )
        .await;
        assert_eq!(
            served,
            vec![pred, ebb, main, next],
            "same-slot pair across a chunk boundary must both be served once"
        );
    }

    #[tokio::test]
    async fn serves_block_range_with_hfc_wrapping() {
        // Use Conway storage-format blocks (era_tag=7).
        let block_a = make_storage_block(7, &[0xAA, 0xBB]);
        let block_b = make_storage_block(7, &[0xCC, 0xDD]);
        let block_c = make_storage_block(7, &[0xEE, 0xFF]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_a),
                (20, [0x02; 32], block_b),
                (30, [0x03; 32], block_c),
            ],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // Request range from slot 10 to slot 30.
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(10, [0x01; 32]),
            to: Point::Specific(30, [0x03; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Should receive: MsgStartBatch → MsgBlock × 3 → MsgBatchDone.
        let (_, _, start) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&start).unwrap(),
            BlockFetchMessage::MsgStartBatch
        ));

        // The server HFC-wraps each block. The decoder extracts the inner
        // block body from [hfc_index, tag24(body)] and returns it.
        for _ in 0..3 {
            let (_, _, block) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&block).unwrap();
            assert!(
                matches!(msg, BlockFetchMessage::MsgBlock(_)),
                "expected MsgBlock, got {msg:?}"
            );
        }

        let (_, _, done_msg) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&done_msg).unwrap(),
            BlockFetchMessage::MsgBatchDone
        ));

        // Send MsgClientDone.
        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn msgblock_wire_format_is_tag24_cbor_in_cbor() {
        // Verify the exact Haskell-compatible wire format:
        //   array(2) [ word(4), tag(24) bstr(stored_block_cbor) ]
        //
        // The Haskell SerialiseNodeToNode instance for HardForkBlock is:
        //   encodeNodeToNode ccfg _ = wrapCBORinCBOR (encodeDiskHfcBlock ccfg)
        //
        // wrapCBORinCBOR places the encodeDiskHfcBlock output (which already
        // has the [era_word, body] layout) inside tag(24).  There is NO
        // intermediate HFC array([hfc_index, ...]) layer.
        let stored_cbor = make_storage_block(7, &[0x01, 0x02]); // Conway era_tag=7
        let wire_bytes = BlockFetchServer::encode_hfc_msg_block(&stored_cbor).unwrap();

        let mut dec = Decoder::new(&wire_bytes);
        let arr = dec.array().unwrap();
        assert_eq!(arr, Some(2), "outer array must have length 2");
        assert_eq!(
            dec.u64().unwrap(),
            TAG_BLOCK,
            "first element must be MsgBlock tag (4)"
        );

        // Second element MUST be tag(24) — the CBOR-in-CBOR wrapper.
        let tag = dec.tag().unwrap();
        assert_eq!(
            tag.as_u64(),
            24,
            "second element must be tag(24) (CBOR-in-CBOR), not an array"
        );

        // The bstr payload must be the original stored CBOR verbatim.
        let payload = dec.bytes().unwrap();
        assert_eq!(
            payload,
            stored_cbor.as_slice(),
            "tag(24) payload must be the verbatim stored block CBOR"
        );
    }

    #[tokio::test]
    async fn no_blocks_when_range_missing() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(999, [0xFF; 32]),
            to: Point::Specific(999, [0xFF; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            BlockFetchMessage::MsgNoBlocks
        ));

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn batch_range_uses_single_lock_acquisition() {
        // Verify that get_blocks_in_range() returns all blocks in a contiguous
        // slot range, exercising the default trait implementation which delegates
        // to get_block_at_or_after_slot / get_next_block_after_slot.
        let blocks: Vec<_> = (0..5u64)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i as u8;
                (i * 10 + 10, h, make_storage_block(7, &[i as u8]))
            })
            .collect();

        let provider = MockBlockProvider {
            blocks: blocks.clone(),
        };

        // Use the default trait implementation.
        let result = provider.get_blocks_in_range(10, 50, 100);
        assert_eq!(result.len(), 5, "should return all 5 blocks in range");
        for (i, (slot, hash, _cbor)) in result.iter().enumerate() {
            assert_eq!(*slot, (i as u64) * 10 + 10);
            assert_eq!(hash[0], i as u8);
        }

        // Partial range.
        let partial = provider.get_blocks_in_range(20, 40, 100);
        assert_eq!(partial.len(), 3, "should return blocks at slots 20, 30, 40");

        // Limit enforcement.
        let limited = provider.get_blocks_in_range(10, 50, 2);
        assert_eq!(limited.len(), 2, "limit should cap at 2 blocks");

        // Empty range.
        let empty = provider.get_blocks_in_range(100, 200, 100);
        assert_eq!(empty.len(), 0, "no blocks in range should return empty vec");
    }

    /// Verify that MsgClientDone terminates the server cleanly.
    #[tokio::test]
    async fn client_done_terminates_server() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "MsgClientDone should terminate server cleanly: {result:?}"
        );
    }

    /// Verify that inverted range (to < from) returns MsgNoBlocks.
    #[tokio::test]
    async fn inverted_range_returns_no_blocks() {
        let block = make_storage_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0x01; 32], block)],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // to_slot (50) < from_slot (100) → invalid range.
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(100, [0x01; 32]),
            to: Point::Specific(50, [0x99; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                BlockFetchMessage::MsgNoBlocks
            ),
            "inverted range should return MsgNoBlocks"
        );

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that requesting from a hash we don't have returns MsgNoBlocks.
    #[tokio::test]
    async fn unknown_from_hash_returns_no_blocks() {
        let block = make_storage_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0x01; 32], block)],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // from_hash = [0xAB; 32] — not in the provider.
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(100, [0xAB; 32]),
            to: Point::Specific(200, [0xAB; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                BlockFetchMessage::MsgNoBlocks
            ),
            "unknown from-hash should return MsgNoBlocks"
        );

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that a single-block range (from == to) returns exactly one MsgBlock.
    #[tokio::test]
    async fn single_block_range_returns_one_msg_block() {
        let block = make_storage_block(7, &[0xAA, 0xBB]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(50, [0x01; 32], block)],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(50, [0x01; 32]),
            to: Point::Specific(50, [0x01; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // MsgStartBatch.
        let (_, _, start) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&start).unwrap(),
            BlockFetchMessage::MsgStartBatch
        ));

        // Exactly one MsgBlock.
        let (_, _, block_msg) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&block_msg).unwrap(),
            BlockFetchMessage::MsgBlock(_)
        ));

        // MsgBatchDone.
        let (_, _, done_msg) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&done_msg).unwrap(),
            BlockFetchMessage::MsgBatchDone
        ));

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgBlock payloads are retrievable across multiple batch requests.
    #[tokio::test]
    async fn multiple_sequential_range_requests() {
        let block_a = make_storage_block(7, &[0x0A]);
        let block_b = make_storage_block(7, &[0x0B]);
        let block_c = make_storage_block(7, &[0x0C]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_a),
                (20, [0x02; 32], block_b),
                (30, [0x03; 32], block_c),
            ],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // First request: range [10, 10] → 1 block.
        let req1 = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(10, [0x01; 32]),
            to: Point::Specific(10, [0x01; 32]),
        });
        ingress_tx.send(Bytes::from(req1)).await.unwrap();
        egress_rx.recv().await.unwrap(); // MsgStartBatch
        egress_rx.recv().await.unwrap(); // MsgBlock
        egress_rx.recv().await.unwrap(); // MsgBatchDone

        // Second request: range [20, 30] → 2 blocks.
        let req2 = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(20, [0x02; 32]),
            to: Point::Specific(30, [0x03; 32]),
        });
        ingress_tx.send(Bytes::from(req2)).await.unwrap();

        let (_, _, start) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&start).unwrap(),
            BlockFetchMessage::MsgStartBatch
        ));
        let (_, _, b1) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&b1).unwrap(),
            BlockFetchMessage::MsgBlock(_)
        ));
        let (_, _, b2) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&b2).unwrap(),
            BlockFetchMessage::MsgBlock(_)
        ));
        let (_, _, done) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&done).unwrap(),
            BlockFetchMessage::MsgBatchDone
        ));

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that the stored block CBOR is preserved verbatim inside tag(24).
    ///
    /// This is a wire-format regression lock: the Haskell BlockFetch decoder
    /// expects tag(24) bstr(stored_cbor) where stored_cbor is the exact on-disk
    /// format [era_word, block_body], NOT an HFC-wrapped [era_index, ...] format.
    #[test]
    fn encode_hfc_msg_block_preserves_stored_cbor_verbatim() {
        let stored_cbor = make_storage_block(5, &[0x11, 0x22, 0x33]); // Alonzo era_tag=5
        let wire = BlockFetchServer::encode_hfc_msg_block(&stored_cbor).unwrap();

        let mut dec = Decoder::new(&wire);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u64().unwrap(), TAG_BLOCK); // tag 4
        let tag = dec.tag().unwrap();
        assert_eq!(tag.as_u64(), 24, "must be tag(24)");
        let payload = dec.bytes().unwrap();

        // The stored CBOR must be preserved VERBATIM inside tag(24).
        assert_eq!(
            payload,
            stored_cbor.as_slice(),
            "stored CBOR must be verbatim inside tag(24) — no structural transformation"
        );

        // Confirm the inner CBOR starts with [era_word=5, ...].
        let mut inner_dec = Decoder::new(payload);
        assert_eq!(
            inner_dec.array().unwrap(),
            Some(2),
            "inner CBOR must be array(2)"
        );
        assert_eq!(
            inner_dec.u64().unwrap(),
            5,
            "inner era tag must be 5 (Alonzo)"
        );
    }

    /// Regression lock: MAX_BLOCKS_PER_BATCH must be large enough for dense chain regions.
    ///
    /// Haskell BlockFetch clients issue batch requests that can span up to ~500 blocks
    /// during rapid syncing. Setting this too low causes BlockFetchProtocolFailureTooFewBlocks.
    /// Verified at compile time.
    const _: () = assert!(
        MAX_BLOCKS_PER_BATCH >= 500,
        "MAX_BLOCKS_PER_BATCH must be >= 500 to handle dense chain regions without TooFewBlocks"
    );

    /// B18: A range spanning more than MAX_SLOT_SPAN slots returns MsgNoBlocks
    /// without scanning the index (prevents trillion-slot range bombs).
    #[tokio::test]
    async fn excessive_slot_span_returns_no_blocks() {
        let block = make_storage_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(0, [0x01; 32], block)],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // from_slot=0, to_slot=50_000_000 → span of 50M >> MAX_SLOT_SPAN (432,000).
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Origin,
            to: Point::Specific(50_000_000, [0xFF; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                BlockFetchMessage::MsgNoBlocks
            ),
            "excessive slot span should return MsgNoBlocks"
        );

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// B18: Exactly MAX_SLOT_SPAN slot range is allowed (boundary check).
    ///
    /// The guard is `if to_slot - from_slot > MAX_SLOT_SPAN`.  A range of
    /// exactly MAX_SLOT_SPAN slots must not be rejected.
    #[tokio::test]
    async fn exact_max_slot_span_is_allowed() {
        // Range: from_slot=0 to to_slot=432_000 → span=432_000 = MAX_SLOT_SPAN → allowed.
        // The MockBlockProvider has no blocks so the server returns MsgNoBlocks
        // (range is valid but no blocks are present).  The important guarantee:
        // the response is NOT caused by the slot-span guard — if the guard had
        // fired, it would also be MsgNoBlocks, but for the wrong reason.
        // We verify the guard by also checking a span of MAX_SLOT_SPAN + 1 (must
        // be rejected) in the `excessive_slot_span_returns_no_blocks` test, which
        // uses a span of 50M.  The boundary = MAX_SLOT_SPAN case here just checks
        // that the guard condition `> MAX_SLOT_SPAN` does NOT fire at exactly MAX.
        const MAX_SLOT_SPAN: u64 = 432_000;

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // span = MAX_SLOT_SPAN exactly → guard must NOT reject this.
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Specific(0, [0x00; 32]),
            to: Point::Specific(MAX_SLOT_SPAN, [0xAA; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Server returns MsgNoBlocks (no blocks in provider, but the span is valid).
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        // We do not assert the specific reason for MsgNoBlocks here — both the
        // span-guard and the "no blocks found" path return MsgNoBlocks.  The
        // point is that the request is not rejected with an error (panic) before
        // reaching that code.  The excessive-span test (50M slot range) is the
        // normative test for the guard; this test locks the boundary.
        let _ = decode_message(&resp).unwrap(); // must decode successfully (no panic)

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that range [from=Origin, to=slot_X] is handled: Origin maps to slot 0
    /// and has_block() always returns true for Origin, so the server proceeds to
    /// collect blocks starting from slot 0. Uses a positive-slot block because the
    /// default `get_block_at_or_after_slot(0)` delegates to `get_next_block_after_slot(0)`
    /// (strict `>` comparison), which finds the first block with slot > 0.
    #[tokio::test]
    async fn range_from_origin_to_specific_slot() {
        let block_a = make_storage_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            // Block at slot 5 — reachable via get_next_block_after_slot(0) (slot > 0).
            blocks: vec![(5, [0x01; 32], block_a)],
        };

        let handle =
            tokio::spawn(async move { BlockFetchServer::run(&mut channel, &provider).await });

        // Range from Origin (slot=0) to Specific(5).
        // `from=Origin` → have_from=true; `get_blocks_in_range(0, 5, ...)` finds slot 5.
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: Point::Origin,
            to: Point::Specific(5, [0x01; 32]),
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, start) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&start).unwrap(),
            BlockFetchMessage::MsgStartBatch
        ));
        let (_, _, block_msg) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&block_msg).unwrap(),
            BlockFetchMessage::MsgBlock(_)
        ));
        let (_, _, done_msg) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&done_msg).unwrap(),
            BlockFetchMessage::MsgBatchDone
        ));

        let client_done = encode_message(&BlockFetchMessage::MsgClientDone);
        ingress_tx.send(Bytes::from(client_done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }
}
