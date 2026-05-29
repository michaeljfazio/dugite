//! BlockFetch client — downloads block ranges from peers.
//!
//! Sends `MsgRequestRange(from, to)` and receives the batch response:
//! `MsgStartBatch` → `MsgBlock(data)` × N → `MsgBatchDone`, or `MsgNoBlocks`.
//!
//! Supports batch-level pipelining (multiple outstanding range requests).

use crate::codec::Point;
use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;

use super::{decode_message, encode_message, BlockFetchMessage};

/// Hard cap on `MsgBlock` messages received per batch.
///
/// B4: A malicious peer could stream `MsgBlock` indefinitely without sending
/// `MsgBatchDone`, blocking the receive task and consuming unbounded memory
/// through the `on_block` callback (which writes to storage and applies to
/// ledger).  The Haskell typed protocol knows the exact expected block count;
/// we approximate with a generous cap matching the server's `MAX_BLOCKS_PER_BATCH`.
pub const MAX_BLOCKS_PER_FETCH: usize = 2_000;

/// BlockFetch client for downloading block ranges.
pub struct BlockFetchClient;

impl BlockFetchClient {
    /// Fetch a range of blocks from the remote peer.
    ///
    /// Sends `MsgRequestRange(from, to)` and streams blocks via callback.
    /// The callback receives raw block CBOR for each block in the range.
    ///
    /// Returns `Ok(block_count)` on success, or `Ok(0)` if the range is unavailable.
    pub async fn fetch_range<F>(
        channel: &mut MuxChannel,
        from: Point,
        to: Point,
        mut on_block: F,
    ) -> Result<usize, ProtocolError>
    where
        F: FnMut(Vec<u8>) -> Result<(), ProtocolError>,
    {
        // Send MsgRequestRange
        let req = encode_message(&BlockFetchMessage::MsgRequestRange {
            from: from.clone(),
            to: to.clone(),
        });
        tracing::debug!("blockfetch: sending MsgRequestRange");
        channel.send(req).await.map_err(ProtocolError::from)?;
        tracing::debug!("blockfetch: MsgRequestRange sent, waiting for response");

        // Receive MsgStartBatch or MsgNoBlocks
        let response_bytes = channel.recv().await.map_err(ProtocolError::from)?;
        tracing::debug!(
            bytes = response_bytes.len(),
            "blockfetch: received response"
        );
        let response = decode_message(&response_bytes).map_err(|e| ProtocolError::CborDecode {
            protocol: "BlockFetch",
            reason: e,
        })?;

        match response {
            BlockFetchMessage::MsgNoBlocks => {
                tracing::debug!("blockfetch: range not available (MsgNoBlocks)");
                return Ok(0);
            }
            BlockFetchMessage::MsgStartBatch => {
                tracing::debug!("blockfetch: MsgStartBatch received, streaming blocks");
            }
            other => {
                tracing::error!("blockfetch: unexpected response: {other:?}");
                return Err(ProtocolError::StateViolation {
                    protocol: "BlockFetch",
                    expected: "MsgStartBatch or MsgNoBlocks".to_string(),
                    actual: format!("{other:?}"),
                });
            }
        }

        // Receive blocks until MsgBatchDone
        // B4: Track block_count and enforce MAX_BLOCKS_PER_FETCH.
        // Without this cap a peer can stream MsgBlock indefinitely without ever
        // sending MsgBatchDone, filling storage/ledger with attacker-controlled
        // data and blocking the BlockFetch task forever.
        //
        // The cap PERMITS exactly `MAX_BLOCKS_PER_FETCH` blocks per batch and
        // rejects the (MAX+1)th MsgBlock — before processing it — so an honest
        // peer fulfilling a full `MAX_BLOCKS_PER_FETCH`-block range succeeds.
        // (The previous `block_count >= MAX` check at the loop top rejected the
        // batch upon *reaching* MAX, i.e. permitted only MAX-1, which wrongly
        // failed honest peers once the adaptive request range grew to the cap —
        // a flood of spurious BoundsExceeded disconnects on deep Byron sync.)
        let mut block_count = 0;
        loop {
            let block_bytes = channel.recv().await.map_err(ProtocolError::from)?;
            let msg = decode_message(&block_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: "BlockFetch",
                reason: e,
            })?;

            match msg {
                BlockFetchMessage::MsgBlock(data) => {
                    if block_count >= MAX_BLOCKS_PER_FETCH {
                        return Err(ProtocolError::BoundsExceeded {
                            protocol: "BlockFetch",
                            reason: format!(
                                "peer sent more than {MAX_BLOCKS_PER_FETCH} MsgBlock messages \
                                 without MsgBatchDone"
                            ),
                        });
                    }
                    block_count += 1;
                    tracing::debug!(
                        block_count,
                        data_len = data.len(),
                        "blockfetch: MsgBlock received"
                    );
                    on_block(data)?;
                }
                BlockFetchMessage::MsgBatchDone => {
                    tracing::debug!(block_count, "blockfetch: batch complete");
                    return Ok(block_count);
                }
                other => {
                    return Err(ProtocolError::StateViolation {
                        protocol: "BlockFetch",
                        expected: "MsgBlock or MsgBatchDone".to_string(),
                        actual: format!("{other:?}"),
                    });
                }
            }
        }
    }

    /// Send MsgClientDone to terminate the BlockFetch protocol.
    pub async fn done(channel: &mut MuxChannel) -> Result<(), ProtocolError> {
        let msg = encode_message(&BlockFetchMessage::MsgClientDone);
        channel.send(msg).await.map_err(ProtocolError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::sync::mpsc;

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = MuxChannel::new(
            3, // BlockFetch protocol ID
            crate::mux::Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    #[tokio::test]
    async fn fetch_range_receives_blocks() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        let handle = tokio::spawn(async move {
            let mut blocks = Vec::new();
            let count = BlockFetchClient::fetch_range(
                &mut channel,
                Point::Specific(10, [0x01; 32]),
                Point::Specific(20, [0x02; 32]),
                |block| {
                    blocks.push(block);
                    Ok(())
                },
            )
            .await
            .unwrap();
            (count, blocks)
        });

        // Read MsgRequestRange
        let (_, _, req_bytes) = egress_rx.recv().await.unwrap();
        let req = decode_message(&req_bytes).unwrap();
        assert!(matches!(req, BlockFetchMessage::MsgRequestRange { .. }));

        // Send MsgStartBatch → MsgBlock × 2 → MsgBatchDone
        ingress_tx
            .send(Bytes::from(encode_message(
                &BlockFetchMessage::MsgStartBatch,
            )))
            .await
            .unwrap();
        ingress_tx
            .send(Bytes::from(encode_message(&BlockFetchMessage::MsgBlock(
                vec![0xAA],
            ))))
            .await
            .unwrap();
        ingress_tx
            .send(Bytes::from(encode_message(&BlockFetchMessage::MsgBlock(
                vec![0xBB],
            ))))
            .await
            .unwrap();
        ingress_tx
            .send(Bytes::from(encode_message(
                &BlockFetchMessage::MsgBatchDone,
            )))
            .await
            .unwrap();

        let (count, blocks) = handle.await.unwrap();
        assert_eq!(count, 2);
        assert_eq!(blocks, vec![vec![0xAA], vec![0xBB]]);
    }

    #[tokio::test]
    async fn fetch_range_no_blocks() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        let handle = tokio::spawn(async move {
            BlockFetchClient::fetch_range(&mut channel, Point::Origin, Point::Origin, |_| Ok(()))
                .await
        });

        // Read MsgRequestRange
        let _ = egress_rx.recv().await.unwrap();

        // Send MsgNoBlocks
        ingress_tx
            .send(Bytes::from(encode_message(&BlockFetchMessage::MsgNoBlocks)))
            .await
            .unwrap();

        let count = handle.await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    /// B4: A peer streaming MsgBlock beyond MAX_BLOCKS_PER_FETCH must be disconnected.
    #[tokio::test]
    async fn fetch_range_rejects_infinite_msg_block_stream() {
        // Use a 1-block cap for test speed.
        use crate::protocol::blockfetch::encode_message as bf_enc;
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        // Spawn a task that checks the error returned by fetch_range.
        let handle = tokio::spawn(async move {
            let mut count = 0usize;
            let result = BlockFetchClient::fetch_range(
                &mut channel,
                Point::Origin,
                Point::Specific(999, [0xFF; 32]),
                |_| {
                    count += 1;
                    Ok(())
                },
            )
            .await;
            (result, count)
        });

        // Server side: MsgRequestRange consumed.
        let _ = egress_rx.recv().await.unwrap();

        // Send MsgStartBatch.
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgStartBatch)))
            .await
            .unwrap();

        // Stream MAX_BLOCKS_PER_FETCH + 1 MsgBlock messages without MsgBatchDone.
        for _ in 0..=MAX_BLOCKS_PER_FETCH {
            ingress_tx
                .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgBlock(vec![
                    0x42,
                ]))))
                .await
                .unwrap();
        }

        let (result, _count) = handle.await.unwrap();
        assert!(
            result.is_err(),
            "fetch_range should error when peer exceeds MAX_BLOCKS_PER_FETCH"
        );
        assert!(
            matches!(
                result.unwrap_err(),
                ProtocolError::BoundsExceeded {
                    protocol: "BlockFetch",
                    ..
                }
            ),
            "expected BoundsExceeded from BlockFetch"
        );
    }

    /// B4: MAX_BLOCKS_PER_FETCH - 1 blocks followed by MsgBatchDone must succeed.
    /// (A batch one below the cap — always valid.)
    #[tokio::test]
    async fn fetch_range_accepts_below_cap() {
        use crate::protocol::blockfetch::encode_message as bf_enc;
        let (egress_tx, egress_rx) = tokio::sync::mpsc::channel(4096);
        let (ingress_tx, ingress_rx) = tokio::sync::mpsc::channel(4096);
        let mut channel = MuxChannel::new(
            3,
            crate::mux::Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            // Large enough window for MAX_BLOCKS_PER_FETCH - 1 messages.
            1_000_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        let mut egress_rx = egress_rx;

        let handle = tokio::spawn(async move {
            let mut count = 0usize;
            let result = BlockFetchClient::fetch_range(
                &mut channel,
                Point::Origin,
                Point::Specific(1, [0x01; 32]),
                |_| {
                    count += 1;
                    Ok(())
                },
            )
            .await;
            (result, count)
        });

        let _ = egress_rx.recv().await.unwrap(); // consume MsgRequestRange

        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgStartBatch)))
            .await
            .unwrap();

        // Send MAX_BLOCKS_PER_FETCH - 1 blocks then MsgBatchDone.
        // (MAX_BLOCKS_PER_FETCH blocks would trigger the guard on the next iteration.)
        let valid_count = MAX_BLOCKS_PER_FETCH - 1;
        for _ in 0..valid_count {
            ingress_tx
                .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgBlock(vec![
                    0x00,
                ]))))
                .await
                .unwrap();
        }
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgBatchDone)))
            .await
            .unwrap();

        let (result, count) = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "MAX_BLOCKS_PER_FETCH - 1 blocks should succeed: {result:?}"
        );
        assert_eq!(count, valid_count);
    }

    /// B4 boundary: EXACTLY MAX_BLOCKS_PER_FETCH blocks followed by MsgBatchDone
    /// must succeed.  The guard permits the full cap and rejects only the
    /// (MAX+1)th MsgBlock; an honest peer fulfilling a full-cap request range
    /// (the adaptive byte budget grows the range to the cap for tiny Byron
    /// blocks) must not be disconnected.
    #[tokio::test]
    async fn fetch_range_accepts_exactly_cap() {
        use crate::protocol::blockfetch::encode_message as bf_enc;
        let (egress_tx, egress_rx) = tokio::sync::mpsc::channel(4096);
        let (ingress_tx, ingress_rx) = tokio::sync::mpsc::channel(4096);
        let mut channel = MuxChannel::new(
            3,
            crate::mux::Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            1_000_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        let mut egress_rx = egress_rx;

        let handle = tokio::spawn(async move {
            let mut count = 0usize;
            let result = BlockFetchClient::fetch_range(
                &mut channel,
                Point::Origin,
                Point::Specific(1, [0x01; 32]),
                |_| {
                    count += 1;
                    Ok(())
                },
            )
            .await;
            (result, count)
        });

        let _ = egress_rx.recv().await.unwrap(); // consume MsgRequestRange
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgStartBatch)))
            .await
            .unwrap();
        for _ in 0..MAX_BLOCKS_PER_FETCH {
            ingress_tx
                .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgBlock(vec![
                    0x00,
                ]))))
                .await
                .unwrap();
        }
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgBatchDone)))
            .await
            .unwrap();

        let (result, count) = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "exactly MAX_BLOCKS_PER_FETCH blocks should succeed: {result:?}"
        );
        assert_eq!(count, MAX_BLOCKS_PER_FETCH);
    }

    /// B4: Unexpected message instead of MsgBlock/MsgBatchDone triggers StateViolation.
    #[tokio::test]
    async fn fetch_range_state_violation_mid_batch() {
        use crate::protocol::blockfetch::encode_message as bf_enc;
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        let handle = tokio::spawn(async move {
            BlockFetchClient::fetch_range(
                &mut channel,
                Point::Origin,
                Point::Specific(1, [0x01; 32]),
                |_| Ok(()),
            )
            .await
        });

        let _ = egress_rx.recv().await.unwrap(); // MsgRequestRange

        // Start batch then send an invalid message (MsgNoBlocks mid-batch).
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgStartBatch)))
            .await
            .unwrap();
        ingress_tx
            .send(Bytes::from(bf_enc(&BlockFetchMessage::MsgNoBlocks)))
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_err(), "invalid mid-batch message should error");
        assert!(
            matches!(
                result.unwrap_err(),
                ProtocolError::StateViolation {
                    protocol: "BlockFetch",
                    ..
                }
            ),
            "expected StateViolation"
        );
    }
}
