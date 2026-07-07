//! Egress task — writes multiplexed SDU segments to the bearer.
//!
//! Receives complete protocol messages from all [`MuxChannel`]s, segments them
//! into SDU-sized chunks with proper headers, and writes them to the bearer in
//! batches for efficiency.
//!
//! ## Per-protocol serialisation
//!
//! The Ouroboros mux delivers SDU payloads per-protocol in arrival order.
//! A receiver accumulates bytes from successive SDUs until a complete CBOR
//! message is formed.  If segments from two *different messages* on the
//! *same protocol* are interleaved, the receiver concatenates them and the
//! CBOR decoder sees corrupted input.
//!
//! Therefore: for each `(protocol_id, direction)` pair, a message's
//! continuation segments **must** be fully sent before any segment of the
//! *next* message on that same pair can start.  Messages from *different*
//! protocols may still be interleaved freely (fairness).
//!
//! ## Fairness
//!
//! Between continuation chunks of a large message, the egress serves one
//! chunk from every other protocol that has pending data (round-robin).
//! This prevents a large BlockFetch response from starving KeepAlive.
//!
//! ## Batching
//!
//! Multiple SDUs are accumulated up to `batch_size` bytes before a single
//! `write_all()` + `flush()` call to the bearer, reducing syscall overhead.

use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;

use crate::error::{BearerError, MuxError};
use crate::mux::segment::{current_timestamp, encode_header, Direction, SduHeader};

/// Maximum number of SDUs to accumulate in a single write batch.
const MAX_SDUS_PER_BATCH: usize = 100;

/// Maximum total bytes buffered per egress channel (protocol_id, direction) pair.
///
/// When a peer reads TCP data very slowly, the protocol layers keep generating
/// outgoing blocks/messages which accumulate in the per-channel `VecDeque<Bytes>`.
/// Without a cap, a single slow-reader peer can cause unbounded heap growth.
///
/// Haskell's `Ouroboros.Network.Mux.Egress` uses `STM TBQueue` with finite
/// capacity to provide natural back-pressure: the *producer blocks* when the
/// queue is full — a queued message is never dropped. We mirror that by holding
/// the over-cap message aside and ceasing to drain the source channel until
/// writes bring the channel below the cap (#872). Dropping the message instead
/// would desync the peer's mini-protocol state machine (e.g. a BlockFetch client
/// receiving fewer `MsgBlock`s than the requested range, with no error).
///
/// 8 MB matches 2× the largest Cardano block (~4 MB post-Conway) so that a
/// single in-flight large block + one pending block can coexist without
/// prematurely triggering back-pressure.
///
/// A-010 (security audit 2026-05-19); back-pressure semantics per #872.
pub const MAX_EGRESS_BYTES_PER_CHANNEL: usize = 8 * 1024 * 1024; // 8 MB

/// Key identifying a protocol channel: (protocol_id, direction).
type ChannelKey = (u16, Direction);

/// Try to enqueue a message for egress, enforcing the per-channel byte cap
/// (A-010) with true back-pressure (#872).
///
/// Returns `None` when the message was enqueued. Returns `Some((key, data))` —
/// the message unchanged — when the channel already holds queued bytes and
/// admitting this message would exceed [`MAX_EGRESS_BYTES_PER_CHANNEL`]; the
/// caller must hold it as `pending` and stop draining the source channel until
/// writes free space, rather than dropping it.
///
/// A message is always admitted when the channel is currently empty, even if it
/// alone exceeds the cap: a single mini-protocol message (e.g. one ~4 MB
/// `MsgBlock`) must be sent whole, and refusing it would deadlock the channel.
#[must_use = "an over-cap message must be held as pending, not dropped"]
fn enqueue_message(
    queues: &mut HashMap<ChannelKey, VecDeque<Bytes>>,
    channel_bytes: &mut HashMap<ChannelKey, usize>,
    key: ChannelKey,
    data: Bytes,
) -> Option<(ChannelKey, Bytes)> {
    let current = channel_bytes.get(&key).copied().unwrap_or(0);
    if current > 0 && current + data.len() > MAX_EGRESS_BYTES_PER_CHANNEL {
        tracing::debug!(
            protocol_id = key.0,
            direction = ?key.1,
            queued_bytes = current,
            message_bytes = data.len(),
            cap = MAX_EGRESS_BYTES_PER_CHANNEL,
            "egress channel at cap — applying back-pressure (holding message, pausing source drain)"
        );
        return Some((key, data));
    }
    *channel_bytes.entry(key).or_insert(0) += data.len();
    queues.entry(key).or_default().push_back(data);
    None
}

/// Egress task state. Created by the [`Mux`] and run as a spawned tokio task.
pub struct EgressTask {
    /// Receiver for outbound messages from all protocol channels.
    /// Each message is `(protocol_id, direction, complete_message_bytes)`.
    rx: mpsc::Receiver<(u16, Direction, Bytes)>,
    /// Maximum SDU payload size for this bearer (e.g., 12288 for TCP).
    sdu_size: usize,
    /// Maximum bytes per write batch (e.g., 131072 for TCP).
    batch_size: usize,
}

impl EgressTask {
    /// Create a new egress task.
    pub fn new(
        rx: mpsc::Receiver<(u16, Direction, Bytes)>,
        sdu_size: usize,
        batch_size: usize,
    ) -> Self {
        Self {
            rx,
            sdu_size,
            batch_size,
        }
    }

    /// Run the egress loop. Reads messages, segments into SDUs, batches writes.
    ///
    /// The `write_fn` is called with each batch of bytes to write to the bearer.
    /// Using a closure instead of a Bearer trait object allows the mux to split
    /// the bearer into separate read/write halves.
    ///
    /// Returns `Ok(())` when the channel is closed (clean shutdown).
    /// Returns `Err(MuxError)` on bearer write failure.
    pub async fn run<W>(mut self, mut write_fn: W) -> Result<(), MuxError>
    where
        W: FnMut(
                &[u8],
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), BearerError>> + Send>,
            > + Send,
    {
        // Per-channel message queue.  Each entry is a complete message or a
        // continuation remainder.  For a given channel, only the FRONT item
        // may have segments on the wire — new messages are queued BEHIND the
        // current one so their bytes never interleave.
        let mut queues: HashMap<ChannelKey, VecDeque<Bytes>> = HashMap::new();

        // Per-channel byte counters for A-010 back-pressure.
        let mut channel_bytes: HashMap<ChannelKey, usize> = HashMap::new();

        // A single message that could not be enqueued because its channel was at
        // the byte cap (#872). While this is `Some`, we stop draining `self.rx`,
        // so producers back-pressure through the bounded egress channel instead
        // of losing the message. It is retried after every write round; once its
        // channel drains below the cap it is enqueued and draining resumes.
        let mut pending: Option<(ChannelKey, Bytes)> = None;

        // Batch buffer for accumulating multiple SDUs before a single write.
        let mut batch_buf: Vec<u8> = Vec::with_capacity(self.batch_size);

        loop {
            // ── Phase 1: write one SDU per channel that has data ─────────
            let mut made_progress = false;
            let keys: Vec<ChannelKey> = queues.keys().copied().collect();

            for key in &keys {
                let queue = match queues.get_mut(key) {
                    Some(q) if !q.is_empty() => q,
                    _ => continue,
                };

                // Take the front item (the in-flight message for this channel).
                let data = queue.pop_front().unwrap();
                let chunk_len = data.len().min(self.sdu_size);

                // A-010: account for bytes leaving the queue.
                // `chunk_len` bytes are about to be sent; the rest (if any) go
                // back into the queue as a remainder.  We decrement the full
                // `data.len()` here, then re-increment the remainder below.
                *channel_bytes.entry(*key).or_insert(0) = channel_bytes
                    .get(key)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(data.len());

                let header = SduHeader {
                    timestamp: current_timestamp(),
                    protocol_id: key.0,
                    direction: key.1,
                    payload_length: chunk_len as u16,
                };
                batch_buf.extend_from_slice(&encode_header(&header));
                batch_buf.extend_from_slice(&data[..chunk_len]);
                made_progress = true;

                // If there's a remainder, push it BACK TO THE FRONT so it
                // is sent before any queued successor message.
                if chunk_len < data.len() {
                    let remainder = data.slice(chunk_len..);
                    // Re-account the unsent bytes.
                    *channel_bytes.entry(*key).or_insert(0) += remainder.len();
                    queue.push_front(remainder);
                }

                if batch_buf.len() >= self.batch_size {
                    write_fn(&batch_buf).await.map_err(MuxError::Bearer)?;
                    batch_buf.clear();
                }
            }

            // Remove empty queues and their byte counters.
            queues.retain(|k, q| {
                if q.is_empty() {
                    channel_bytes.remove(k);
                    false
                } else {
                    true
                }
            });

            // Flush whatever accumulated in this round.
            if !batch_buf.is_empty() {
                write_fn(&batch_buf).await.map_err(MuxError::Bearer)?;
                batch_buf.clear();
            }

            // Retry the back-pressured message (#872): the write round above may
            // have drained its channel below the cap. `enqueue_message` returns
            // it again if the channel is still full.
            if let Some((key, data)) = pending.take() {
                pending = enqueue_message(&mut queues, &mut channel_bytes, key, data);
            }

            // If we made progress, loop again to send more continuation
            // chunks (or start the next queued message).
            if made_progress {
                // Non-blocking drain of any new messages that arrived while we
                // were writing — but ONLY while no message is back-pressured.
                // Holding `pending` means a channel is at its byte cap; we stop
                // pulling from `self.rx` so producers block on the bounded egress
                // channel (true back-pressure) instead of us buffering unbounded.
                if pending.is_none() {
                    let mut sdu_count = 0;
                    while pending.is_none() {
                        match self.rx.try_recv() {
                            Ok((pid, dir, data)) => {
                                pending = enqueue_message(
                                    &mut queues,
                                    &mut channel_bytes,
                                    (pid, dir),
                                    data,
                                );
                                sdu_count += 1;
                                if sdu_count >= MAX_SDUS_PER_BATCH {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                continue;
            }

            // Whenever a message is back-pressured its channel is non-empty, so
            // Phase 1 makes progress and this point is unreachable with
            // `pending.is_some()`; guard anyway to avoid a busy-loop if that
            // invariant ever changes.
            if pending.is_some() {
                continue;
            }

            // ── Phase 2: no pending data — block for the next message ────
            match self.rx.recv().await {
                None => return Ok(()),
                Some((pid, dir, data)) => {
                    pending = enqueue_message(&mut queues, &mut channel_bytes, (pid, dir), data);
                }
            }

            // Non-blocking drain of any additional messages (paused on pending).
            while pending.is_none() {
                match self.rx.try_recv() {
                    Ok((pid, dir, data)) => {
                        pending =
                            enqueue_message(&mut queues, &mut channel_bytes, (pid, dir), data);
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::segment::{decode_header, HEADER_SIZE};
    use std::sync::{Arc, Mutex};

    /// Helper to run egress with a capturing write function.
    async fn run_egress_capturing(
        sdu_size: usize,
        messages: Vec<(u16, Direction, Vec<u8>)>,
    ) -> Vec<u8> {
        let (tx, rx) = mpsc::channel(64);
        let task = EgressTask::new(rx, sdu_size, 131072);

        // Send all messages
        for (pid, dir, data) in messages {
            tx.send((pid, dir, Bytes::from(data))).await.unwrap();
        }
        // Close the channel to signal shutdown
        drop(tx);

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        task.run(move |data: &[u8]| {
            let captured = captured_clone.clone();
            let data = data.to_vec();
            Box::pin(async move {
                captured.lock().unwrap().extend_from_slice(&data);
                Ok(())
            })
        })
        .await
        .unwrap();

        Arc::try_unwrap(captured).unwrap().into_inner().unwrap()
    }

    #[tokio::test]
    async fn single_message_fits_in_one_sdu() {
        let msg = vec![0x82, 0x01, 0x02]; // 3 bytes, well under SDU limit
        let captured =
            run_egress_capturing(12288, vec![(2, Direction::InitiatorDir, msg.clone())]).await;

        // Should be exactly one SDU: 8-byte header + 3-byte payload
        assert_eq!(captured.len(), HEADER_SIZE + 3);

        let header = decode_header(captured[..8].try_into().unwrap());
        assert_eq!(header.protocol_id, 2);
        assert_eq!(header.direction, Direction::InitiatorDir);
        assert_eq!(header.payload_length, 3);
        assert_eq!(&captured[8..], &msg);
    }

    #[tokio::test]
    async fn large_message_segmented_across_sdus() {
        // SDU size = 4 bytes, message = 10 bytes → should be split into 3 SDUs (4+4+2)
        let msg = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
        let captured =
            run_egress_capturing(4, vec![(3, Direction::ResponderDir, msg.clone())]).await;

        // Parse SDUs from the captured bytes
        let mut offset = 0;
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            offset += HEADER_SIZE;
            let payload = &captured[offset..offset + header.payload_length as usize];
            chunks.push(payload.to_vec());
            offset += header.payload_length as usize;

            assert_eq!(header.protocol_id, 3);
            assert_eq!(header.direction, Direction::ResponderDir);
        }

        // Reassembled should match original
        let reassembled: Vec<u8> = chunks.into_iter().flatten().collect();
        assert_eq!(reassembled, msg);
    }

    #[tokio::test]
    async fn multiple_protocols_interleaved() {
        let msg_a = vec![0x01; 6]; // protocol 2, 6 bytes
        let msg_b = vec![0x02; 3]; // protocol 3, 3 bytes
        let captured = run_egress_capturing(
            4,
            vec![
                (2, Direction::InitiatorDir, msg_a),
                (3, Direction::InitiatorDir, msg_b),
            ],
        )
        .await;

        // With SDU=4: msg_a needs 2 SDUs (4+2), msg_b needs 1 SDU (3)
        // Due to round-robin, we should see interleaved protocol IDs
        let mut offset = 0;
        let mut protocol_ids = Vec::new();
        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            protocol_ids.push(header.protocol_id);
            offset += HEADER_SIZE + header.payload_length as usize;
        }

        // msg_a first chunk (pid=2), msg_b (pid=3), msg_a remainder (pid=2)
        // OR msg_a chunk (pid=2), msg_b chunk (pid=3), msg_a remainder (pid=2)
        // The exact interleaving depends on batching, but both protocols should appear
        assert!(protocol_ids.contains(&2));
        assert!(protocol_ids.contains(&3));
    }

    /// Verify that egress exits cleanly (Ok(())) when the channel is closed with no messages.
    #[tokio::test]
    async fn empty_channel_returns_ok() {
        let (tx, rx) = mpsc::channel(64);
        let task = EgressTask::new(rx, 12288, 131072);

        // Close the channel immediately — no messages ever queued.
        drop(tx);

        let result = task
            .run(move |_data: &[u8]| Box::pin(async move { Ok(()) }))
            .await;

        assert!(result.is_ok(), "empty channel should return Ok: {result:?}");
    }

    /// Verify that exact SDU-boundary messages produce exactly one SDU frame.
    #[tokio::test]
    async fn message_exactly_sdu_size_produces_one_frame() {
        let sdu_size = 10;
        let msg = vec![0xAB; sdu_size]; // exactly 10 bytes
        let captured =
            run_egress_capturing(sdu_size, vec![(2, Direction::InitiatorDir, msg.clone())]).await;

        // Should be exactly one SDU: header + 10 bytes
        assert_eq!(captured.len(), HEADER_SIZE + sdu_size);
        let header = decode_header(captured[..8].try_into().unwrap());
        assert_eq!(header.payload_length as usize, sdu_size);
        assert_eq!(&captured[8..], msg.as_slice());
    }

    /// Verify that an empty message body produces a zero-length SDU.
    #[tokio::test]
    async fn zero_length_message_produces_zero_payload_sdu() {
        let captured =
            run_egress_capturing(12288, vec![(2, Direction::InitiatorDir, vec![])]).await;

        // Should be one SDU: header + 0 bytes
        assert_eq!(captured.len(), HEADER_SIZE);
        let header = decode_header(captured[..8].try_into().unwrap());
        assert_eq!(header.payload_length, 0);
    }

    /// Verify that every SDU in a segmented message carries the same protocol_id and direction.
    #[tokio::test]
    async fn all_segments_carry_correct_protocol_and_direction() {
        // 20-byte message, SDU=5 → 4 segments.
        let msg = (0u8..20u8).collect::<Vec<_>>();
        let captured =
            run_egress_capturing(5, vec![(7, Direction::ResponderDir, msg.clone())]).await;

        let mut offset = 0;
        let mut segment_count = 0;
        let mut reassembled = Vec::new();
        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            assert_eq!(
                header.protocol_id, 7,
                "segment {segment_count}: wrong protocol_id"
            );
            assert_eq!(
                header.direction,
                Direction::ResponderDir,
                "segment {segment_count}: wrong direction"
            );
            let end = offset + HEADER_SIZE + header.payload_length as usize;
            reassembled.extend_from_slice(&captured[offset + HEADER_SIZE..end]);
            offset = end;
            segment_count += 1;
        }

        assert_eq!(
            segment_count, 4,
            "20-byte message / SDU=5 should produce 4 segments"
        );
        assert_eq!(
            reassembled, msg,
            "reassembled payload should match original"
        );
    }

    /// Verify that the egress write_fn error propagates as MuxError::Bearer.
    #[tokio::test]
    async fn write_error_propagates() {
        use crate::error::BearerError;

        let (tx, rx) = mpsc::channel(64);
        let task = EgressTask::new(rx, 12288, 131072);

        tx.send((
            2,
            Direction::InitiatorDir,
            bytes::Bytes::from(vec![0x81, 0x01]),
        ))
        .await
        .unwrap();
        drop(tx);

        let result = task
            .run(move |_data: &[u8]| Box::pin(async move { Err(BearerError::ConnectionReset) }))
            .await;

        assert!(
            matches!(result, Err(MuxError::Bearer(BearerError::ConnectionReset))),
            "expected Bearer(ConnectionReset) error, got: {result:?}"
        );
    }

    /// Verify that messages on different (protocol_id, direction) pairs are kept separate.
    ///
    /// This is the regression lock for the ConnectionId tuple keying fix — two connections
    /// using the same protocol_id but different directions must not overwrite each other's
    /// queues in the egress HashMap.
    #[tokio::test]
    async fn different_direction_same_protocol_kept_separate() {
        // Protocol 8 (KeepAlive) on both InitiatorDir and ResponderDir.
        let msg_init = vec![0x11; 3];
        let msg_resp = vec![0x22; 3];
        let captured = run_egress_capturing(
            12288,
            vec![
                (8, Direction::InitiatorDir, msg_init.clone()),
                (8, Direction::ResponderDir, msg_resp.clone()),
            ],
        )
        .await;

        let mut offset = 0;
        let mut init_payloads: Vec<Vec<u8>> = Vec::new();
        let mut resp_payloads: Vec<Vec<u8>> = Vec::new();

        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            let payload_start = offset + HEADER_SIZE;
            let payload_end = payload_start + header.payload_length as usize;
            let payload = captured[payload_start..payload_end].to_vec();
            match header.direction {
                Direction::InitiatorDir => init_payloads.push(payload),
                Direction::ResponderDir => resp_payloads.push(payload),
            }
            offset = payload_end;
        }

        // Both messages should have been emitted independently.
        let all_init: Vec<u8> = init_payloads.into_iter().flatten().collect();
        let all_resp: Vec<u8> = resp_payloads.into_iter().flatten().collect();
        assert_eq!(all_init, msg_init, "InitiatorDir payload corrupted");
        assert_eq!(all_resp, msg_resp, "ResponderDir payload corrupted");
    }

    /// Verify batching: two small messages sent back-to-back may be coalesced
    /// into a single write call (batch_size larger than their combined size).
    #[tokio::test]
    async fn small_messages_may_be_batched() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (tx, rx) = mpsc::channel(64);
        let task = EgressTask::new(rx, 12288, 131072);

        let write_count = Arc::new(AtomicUsize::new(0));
        let write_count2 = write_count.clone();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap2 = captured.clone();

        // Send two tiny messages before closing.
        tx.send((
            2,
            Direction::InitiatorDir,
            bytes::Bytes::from(vec![0x81, 0x01]),
        ))
        .await
        .unwrap();
        tx.send((
            3,
            Direction::InitiatorDir,
            bytes::Bytes::from(vec![0x81, 0x02]),
        ))
        .await
        .unwrap();
        drop(tx);

        task.run(move |data: &[u8]| {
            write_count2.fetch_add(1, Ordering::SeqCst);
            cap2.lock().unwrap().extend_from_slice(data);
            Box::pin(async move { Ok(()) })
        })
        .await
        .unwrap();

        let total = captured.lock().unwrap().len();
        // Each message is HEADER_SIZE + 2 bytes = 10 bytes; two messages = 20 bytes.
        assert_eq!(total, 2 * (HEADER_SIZE + 2));
        // The two messages may be coalesced into 1 or 2 writes — either is fine.
        // We only assert that at least one write happened.
        assert!(write_count.load(Ordering::SeqCst) >= 1);
    }

    /// Verify three-SDU segmentation: (sdu_size, remainder1, remainder2) for 3-chunk messages.
    #[tokio::test]
    async fn three_chunk_segmentation() {
        // sdu_size=4, message=9 bytes → chunks: [4, 4, 1]
        let msg: Vec<u8> = (1u8..=9u8).collect();
        let captured =
            run_egress_capturing(4, vec![(2, Direction::InitiatorDir, msg.clone())]).await;

        let mut offset = 0;
        let mut chunk_sizes = Vec::new();
        let mut reassembled = Vec::new();
        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            let len = header.payload_length as usize;
            chunk_sizes.push(len);
            reassembled
                .extend_from_slice(&captured[offset + HEADER_SIZE..offset + HEADER_SIZE + len]);
            offset += HEADER_SIZE + len;
        }

        assert_eq!(
            chunk_sizes,
            vec![4, 4, 1],
            "9-byte message / SDU=4 should produce [4,4,1] chunks"
        );
        assert_eq!(reassembled, msg);
    }

    /// #872: an over-cap message is held (back-pressure), never dropped.
    ///
    /// A channel that is empty accepts any message (even one larger than the
    /// cap). A channel that already holds bytes refuses a message that would
    /// exceed the cap, returning it unchanged for the caller to hold as pending.
    #[test]
    fn enqueue_message_backpressures_instead_of_dropping() {
        let mut queues: HashMap<ChannelKey, VecDeque<Bytes>> = HashMap::new();
        let mut channel_bytes: HashMap<ChannelKey, usize> = HashMap::new();
        let key = (3u16, Direction::ResponderDir);

        // Empty channel: an oversized single message is still admitted whole.
        let big = Bytes::from(vec![0u8; MAX_EGRESS_BYTES_PER_CHANNEL + 10]);
        let big_len = big.len();
        assert!(
            enqueue_message(&mut queues, &mut channel_bytes, key, big).is_none(),
            "empty channel must admit even an oversized message (no deadlock)"
        );
        assert_eq!(channel_bytes[&key], big_len);

        // Channel now over cap: a second message is refused (returned), not dropped.
        let more = Bytes::from(vec![1u8; 100]);
        let back = enqueue_message(&mut queues, &mut channel_bytes, key, more.clone());
        assert_eq!(
            back,
            Some((key, more)),
            "over-cap message must be returned for back-pressure, not dropped"
        );
        // Counter unchanged — the refused message was not accounted.
        assert_eq!(channel_bytes[&key], big_len);

        // Once the channel drains below the cap, the same message is accepted.
        channel_bytes.insert(key, 0);
        queues.get_mut(&key).unwrap().clear();
        let retry = Bytes::from(vec![1u8; 100]);
        assert!(
            enqueue_message(&mut queues, &mut channel_bytes, key, retry).is_none(),
            "drained channel must accept the previously back-pressured message"
        );
        assert_eq!(channel_bytes[&key], 100);
    }

    /// #872 end-to-end: feeding more than the per-channel cap through `run()`
    /// delivers every byte — no message is silently dropped.
    #[tokio::test]
    async fn run_delivers_all_bytes_beyond_channel_cap() {
        // 120 messages × 100 KB = 12 MB on one channel, exceeding the 8 MB cap.
        let msg_size = 100 * 1024;
        let count = 120;
        let (tx, rx) = mpsc::channel(count + 1);
        let task = EgressTask::new(rx, 12288, 131072);

        for i in 0..count {
            // Distinct first byte per message so we can count deliveries.
            let mut m = vec![(i % 251) as u8; msg_size];
            m[0] = (i % 251) as u8;
            tx.send((2, Direction::InitiatorDir, Bytes::from(m)))
                .await
                .unwrap();
        }
        drop(tx);

        let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
        let cap2 = captured.clone();
        task.run(move |data: &[u8]| {
            let cap = cap2.clone();
            let data = data.to_vec();
            Box::pin(async move {
                cap.lock().unwrap().extend_from_slice(&data);
                Ok(())
            })
        })
        .await
        .unwrap();

        // Reassemble SDU payloads and confirm total delivered bytes == total sent.
        let bytes = captured.lock().unwrap();
        let mut offset = 0usize;
        let mut payload_total = 0usize;
        while offset < bytes.len() {
            let header = decode_header(bytes[offset..offset + 8].try_into().unwrap());
            offset += HEADER_SIZE + header.payload_length as usize;
            payload_total += header.payload_length as usize;
        }
        assert_eq!(
            payload_total,
            msg_size * count,
            "all {count} messages ({} bytes) must be delivered — none dropped",
            msg_size * count
        );
    }

    /// Verify egress handles a single very large message spanning many SDUs correctly.
    #[tokio::test]
    async fn large_message_full_reconstruction() {
        // 1000-byte message with sdu_size=100 → 10 segments of 100 bytes each.
        let msg: Vec<u8> = (0u8..=255u8).cycle().take(1000).collect();
        let captured =
            run_egress_capturing(100, vec![(2, Direction::InitiatorDir, msg.clone())]).await;

        let mut offset = 0;
        let mut count = 0;
        let mut reassembled = Vec::new();
        while offset < captured.len() {
            let header = decode_header(captured[offset..offset + 8].try_into().unwrap());
            let len = header.payload_length as usize;
            reassembled
                .extend_from_slice(&captured[offset + HEADER_SIZE..offset + HEADER_SIZE + len]);
            offset += HEADER_SIZE + len;
            count += 1;
        }

        assert_eq!(
            count, 10,
            "1000-byte message / SDU=100 should produce 10 segments"
        );
        assert_eq!(
            reassembled, msg,
            "full reconstruction should match original"
        );
    }
}
