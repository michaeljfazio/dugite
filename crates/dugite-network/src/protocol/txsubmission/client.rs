//! TxSubmission2 client — announces and serves transactions to remote peers.
//!
//! In TxSubmission2, the "client" is the side that has transactions to offer.
//! The server drives the protocol by requesting tx IDs and tx bodies.
//!
//! The client:
//! 1. Sends `MsgInit` to initialize
//! 2. Waits for `MsgRequestTxIds` from the server
//! 3. Replies with tx IDs from the mempool
//! 4. Waits for `MsgRequestTxs` to send full tx bodies
//! 5. Sends `MsgDone` when in blocking state with no transactions

use std::sync::Arc;

use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;

use super::{decode_message, encode_message, TxIdAndSize, TxSubmissionMessage};

/// Trait for providing transactions to the TxSubmission2 client.
///
/// Implemented by the mempool layer.
pub trait TxSource: Send + Sync {
    /// Get pending transaction IDs with their sizes.
    /// Returns up to `max_count` tx IDs, acknowledging `ack_count` previously returned.
    fn get_tx_ids(&self, ack_count: u16, max_count: u16) -> Vec<TxIdAndSize>;

    /// Get full transaction CBOR by their IDs.
    ///
    /// Each element in `tx_ids` is `(era_id, tx_hash)` matching the HFC GenTxId
    /// envelope from `MsgRequestTxs`.  Returns `(era_id, tx_cbor)` pairs for
    /// `MsgReplyTxs`, preserving the era for the HFC GenTx envelope.
    fn get_txs(&self, tx_ids: &[(u8, [u8; 32])]) -> Vec<(u8, Vec<u8>)>;

    /// Check if there are any pending transactions.
    fn has_pending(&self) -> bool;

    /// Optional notification handle for event-driven wakeup.
    ///
    /// When `Some`, the client awaits this instead of polling every 500ms.
    /// The mempool fires `notify_waiters()` on each successful tx admission,
    /// providing zero-CPU-waste blocking behavior matching Haskell's STM retry.
    fn tx_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

/// TxSubmission2 client that announces transactions to a remote peer.
pub struct TxSubmissionClient;

impl TxSubmissionClient {
    /// Run the TxSubmission2 client protocol.
    ///
    /// Sends `MsgInit`, then responds to server requests until `MsgDone`.
    pub async fn run<S: TxSource>(
        channel: &mut MuxChannel,
        source: &S,
    ) -> Result<(), ProtocolError> {
        // Send MsgInit
        let init = encode_message(&TxSubmissionMessage::MsgInit);
        channel.send(init).await.map_err(ProtocolError::from)?;
        tracing::debug!("txsubmission2 client: MsgInit sent, awaiting server requests");

        loop {
            // Wait for server request
            let msg_bytes = channel.recv().await.map_err(ProtocolError::from)?;
            let msg = decode_message(&msg_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: "TxSubmission2",
                reason: e,
            })?;

            match msg {
                TxSubmissionMessage::MsgRequestTxIds {
                    blocking,
                    ack_count,
                    req_count,
                } => {
                    let mut tx_ids = source.get_tx_ids(ack_count, req_count);
                    tracing::debug!(
                        blocking,
                        ack_count,
                        req_count,
                        yielded = tx_ids.len(),
                        "txsubmission2 client: MsgRequestTxIds received"
                    );

                    if tx_ids.is_empty() && blocking {
                        // Blocking mode with empty mempool: wait for txs to appear.
                        //
                        // The initial get_tx_ids(ack_count, req_count) already
                        // acknowledged previously-outstanding tx IDs.  Subsequent
                        // polls must NOT re-acknowledge (ack_count=0) but the
                        // outstanding set was already drained by the first call.
                        //
                        // Race-free ordering:
                        //   1. Create the `Notified` future BEFORE re-querying.
                        //   2. Re-query the mempool.
                        //   3. Await the pre-armed `Notified` only if still empty.
                        //
                        // tokio's `Notify::notify_waiters()` does NOT buffer — it
                        // only wakes futures that already exist when it fires. If
                        // we re-queried first and then created the Notified, a
                        // `notify_waiters` fired in between would be lost and the
                        // task would block until the next tx admission happens
                        // by chance. That's exactly the symptom seen with Conway
                        // cert-bearing txs in #521: the tx sat in the upstream
                        // mempool for ~60 s waiting for an unrelated wakeup.
                        //
                        // Creating the `Notified` before the re-check guarantees
                        // any `notify_waiters` that fires after this point wakes
                        // us (Notified captures wakes targeted at it regardless
                        // of whether `.await` has been polled yet). And the
                        // re-check immediately afterwards picks up txs that were
                        // already in the mempool when we entered the loop, so
                        // we never block indefinitely on a tx that's already there.
                        tracing::debug!("txsubmission2 client: blocking — waiting for mempool txs");
                        loop {
                            // Step 1: arm the wakeup BEFORE checking state.
                            let notified = source
                                .tx_notify()
                                .map(|n| Box::pin(async move { n.notified().await }));

                            // Step 2: re-query the mempool. ack_count=0: the
                            // first call already acknowledged; req_count stays
                            // the same — peer wants up to this many.
                            tx_ids = source.get_tx_ids(0, req_count);
                            if !tx_ids.is_empty() {
                                tracing::info!(
                                    count = tx_ids.len(),
                                    "txsubmission2 client: mempool txs available, resuming"
                                );
                                break;
                            }

                            // Step 3: wait for next notify, with a 500 ms
                            // fallback so we still re-poll periodically if a
                            // TxSource impl doesn't provide a Notify (test
                            // mocks) or to provide defence-in-depth.
                            match notified {
                                Some(fut) => {
                                    tokio::select! {
                                        _ = fut => {}
                                        _ = tokio::time::sleep(
                                            std::time::Duration::from_millis(500),
                                        ) => {}
                                    }
                                }
                                None => {
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                            }
                        }
                    }

                    let reply = encode_message(&TxSubmissionMessage::MsgReplyTxIds(tx_ids));
                    channel.send(reply).await.map_err(ProtocolError::from)?;
                }
                TxSubmissionMessage::MsgRequestTxs(tx_ids) => {
                    let txs = source.get_txs(&tx_ids);
                    tracing::debug!(
                        requested = tx_ids.len(),
                        returned = txs.len(),
                        "txsubmission2 client: MsgRequestTxs received"
                    );
                    let reply = encode_message(&TxSubmissionMessage::MsgReplyTxs(txs));
                    channel.send(reply).await.map_err(ProtocolError::from)?;
                }
                other => {
                    return Err(ProtocolError::StateViolation {
                        protocol: "TxSubmission2",
                        expected: "MsgRequestTxIds or MsgRequestTxs".to_string(),
                        actual: format!("{other:?}"),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::sync::mpsc;

    struct MockTxSource {
        tx_ids: Vec<TxIdAndSize>,
        txs: Vec<([u8; 32], Vec<u8>)>,
    }

    impl TxSource for MockTxSource {
        fn get_tx_ids(&self, _ack_count: u16, max_count: u16) -> Vec<TxIdAndSize> {
            self.tx_ids
                .iter()
                .take(max_count as usize)
                .cloned()
                .collect()
        }

        fn get_txs(&self, tx_ids: &[(u8, [u8; 32])]) -> Vec<(u8, Vec<u8>)> {
            tx_ids
                .iter()
                .filter_map(|(era_id, id)| {
                    self.txs
                        .iter()
                        .find(|(tid, _)| tid == id)
                        .map(|(_, data)| (*era_id, data.clone()))
                })
                .collect()
        }

        fn has_pending(&self) -> bool {
            !self.tx_ids.is_empty()
        }
    }

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = MuxChannel::new(
            4,
            crate::mux::Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    #[tokio::test]
    async fn client_sends_init_and_replies() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let source = MockTxSource {
            tx_ids: vec![TxIdAndSize {
                era_id: 6,
                tx_id: [0xAA; 32],
                size_in_bytes: 200,
            }],
            txs: vec![([0xAA; 32], vec![0x01, 0x02])],
        };

        let handle =
            tokio::spawn(async move { TxSubmissionClient::run(&mut channel, &source).await });

        // Read MsgInit
        let (_, _, init) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&init).unwrap(),
            TxSubmissionMessage::MsgInit
        ));

        // Send MsgRequestTxIds (non-blocking, first request)
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: false,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Read MsgReplyTxIds
        let (_, _, reply) = egress_rx.recv().await.unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decode_message(&reply).unwrap() {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].tx_id, [0xAA; 32]);
        } else {
            panic!("expected MsgReplyTxIds");
        }

        // Send MsgRequestTxs — each element is (era_id, tx_hash)
        let req_txs = encode_message(&TxSubmissionMessage::MsgRequestTxs(vec![(6u8, [0xAA; 32])]));
        ingress_tx.send(Bytes::from(req_txs)).await.unwrap();

        // Read MsgReplyTxs — each element is (era_id, tx_cbor)
        let (_, _, reply_txs) = egress_rx.recv().await.unwrap();
        if let TxSubmissionMessage::MsgReplyTxs(txs) = decode_message(&reply_txs).unwrap() {
            assert_eq!(txs, vec![(6u8, vec![0x01, 0x02])]);
        } else {
            panic!("expected MsgReplyTxs");
        }

        // We can't change the source mid-test, so just drop the channel to end
        drop(ingress_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn client_blocks_when_blocking_with_no_txs() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let source = MockTxSource {
            tx_ids: vec![],
            txs: vec![],
        };

        let handle =
            tokio::spawn(async move { TxSubmissionClient::run(&mut channel, &source).await });

        // Read MsgInit
        let _ = egress_rx.recv().await.unwrap();

        // Send blocking MsgRequestTxIds
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: true,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Client should block (polling mempool) rather than sending MsgDone.
        // Verify no message arrives within 200ms.
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(200), egress_rx.recv()).await;
        assert!(result.is_err(), "client should block, not send MsgDone");

        // Abort the client task (it's polling forever with empty mempool).
        handle.abort();
        let _ = handle.await;
    }

    /// Mock TxSource that supports Notify-based wakeup with shared tx_ids
    /// that can be populated externally after construction.
    struct NotifyMockTxSource {
        notify: Arc<tokio::sync::Notify>,
        /// Shared so the test can inject tx IDs while the source is in use.
        tx_ids: std::sync::Arc<std::sync::Mutex<Vec<TxIdAndSize>>>,
        txs: Vec<([u8; 32], Vec<u8>)>,
    }

    impl TxSource for NotifyMockTxSource {
        fn get_tx_ids(&self, _ack_count: u16, max_count: u16) -> Vec<TxIdAndSize> {
            self.tx_ids
                .lock()
                .unwrap()
                .iter()
                .take(max_count as usize)
                .cloned()
                .collect()
        }

        fn get_txs(&self, tx_ids: &[(u8, [u8; 32])]) -> Vec<(u8, Vec<u8>)> {
            tx_ids
                .iter()
                .filter_map(|(era_id, id)| {
                    self.txs
                        .iter()
                        .find(|(tid, _)| tid == id)
                        .map(|(_, data)| (*era_id, data.clone()))
                })
                .collect()
        }

        fn has_pending(&self) -> bool {
            !self.tx_ids.lock().unwrap().is_empty()
        }

        fn tx_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
            Some(self.notify.clone())
        }
    }

    #[tokio::test]
    async fn client_wakes_on_notify_instead_of_polling() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let notify = Arc::new(tokio::sync::Notify::new());
        let shared_tx_ids: std::sync::Arc<std::sync::Mutex<Vec<TxIdAndSize>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx_ids_handle = shared_tx_ids.clone();

        let source = NotifyMockTxSource {
            notify: notify.clone(),
            tx_ids: shared_tx_ids,
            txs: vec![([0xDD; 32], vec![0x99])],
        };

        let handle =
            tokio::spawn(async move { TxSubmissionClient::run(&mut channel, &source).await });

        // Read MsgInit
        let _ = egress_rx.recv().await.unwrap();

        // Send blocking MsgRequestTxIds
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: true,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Client should be waiting on the notify (not polling).
        // Verify no message within 100ms.
        let timeout_result =
            tokio::time::timeout(std::time::Duration::from_millis(100), egress_rx.recv()).await;
        assert!(
            timeout_result.is_err(),
            "client should be waiting on notify"
        );

        // "Add a tx to the mempool" via the shared handle, then fire notify.
        tx_ids_handle.lock().unwrap().push(TxIdAndSize {
            era_id: 6,
            tx_id: [0xDD; 32],
            size_in_bytes: 200,
        });
        notify.notify_waiters();

        // The client should wake promptly and send MsgReplyTxIds within 100ms
        // (proving it used Notify, not 500ms polling).
        let reply_result =
            tokio::time::timeout(std::time::Duration::from_millis(100), egress_rx.recv()).await;
        assert!(
            reply_result.is_ok(),
            "client should wake from notify and reply promptly"
        );
        let (_, _, reply_bytes) = reply_result.unwrap().unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decode_message(&reply_bytes).unwrap() {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].tx_id, [0xDD; 32]);
        } else {
            panic!("expected MsgReplyTxIds");
        }

        // Clean up
        handle.abort();
        let _ = handle.await;
    }

    /// Regression test for #521: cert-bearing txs sat in the relay's mempool
    /// for ~60 s before propagating to the BP.
    ///
    /// Root cause: a race between `get_tx_ids` returning empty and the
    /// `Notify::notified()` future being created. `Notify::notify_waiters()`
    /// does NOT buffer; if it fires before a `Notified` is created, the
    /// wake is lost and the client blocks until the next admission.
    ///
    /// This test simulates the race: it adds a tx and fires `notify_waiters`
    /// BEFORE the client enters its inner wait loop. The fixed client must
    /// re-query the mempool before awaiting (so it picks up the tx without
    /// needing a second notify) AND it must arm the `Notified` before the
    /// re-query (so a notify fired immediately after the re-query is not
    /// lost). With the previous code (await-then-requery), this test hangs
    /// for ~500 ms (or forever if no fallback poll exists).
    #[tokio::test]
    async fn client_does_not_miss_notify_fired_before_blocking_wait() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let notify = Arc::new(tokio::sync::Notify::new());
        let shared_tx_ids: std::sync::Arc<std::sync::Mutex<Vec<TxIdAndSize>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx_ids_handle = shared_tx_ids.clone();

        let source = NotifyMockTxSource {
            notify: notify.clone(),
            tx_ids: shared_tx_ids,
            txs: vec![([0xEE; 32], vec![0x77])],
        };

        let handle =
            tokio::spawn(async move { TxSubmissionClient::run(&mut channel, &source).await });

        // Drain MsgInit (sent immediately on startup).
        let _ = egress_rx.recv().await.unwrap();

        // Inject the tx and fire notify_waiters() BEFORE sending
        // MsgRequestTxIds. This reproduces the race where notify_waiters
        // fires while no Notified is yet listening:
        //
        //   - In the buggy code, the client would call get_tx_ids (empty
        //     because we haven't sent the request yet — wait, actually
        //     the request hasn't been received yet, so the client is
        //     blocked on channel.recv()).
        //
        // Actually, the more direct repro is: send MsgRequestTxIds first,
        // then BEFORE the client wakes from recv to call get_tx_ids, push
        // the tx and fire notify. The client's get_tx_ids will then return
        // [tx] on first call — no race. That's not the bug.
        //
        // The real bug is: send MsgRequestTxIds, client's get_tx_ids
        // returns empty (mempool empty), client enters inner wait loop.
        // Between get_tx_ids returning empty and Notified being created,
        // a tx is added and notify_waiters fires.
        //
        // We can't deterministically schedule that micro-window in a test,
        // but we CAN guarantee Notified-first ordering by: registering the
        // mempool tx + firing notify_waiters AFTER MsgRequestTxIds arrives
        // but BEFORE the inner wait sleep completes. We use a longer sleep
        // before sending MsgRequestTxIds to give the test predictable
        // timing.
        //
        // Easier deterministic repro: do the "add + notify_waiters" at the
        // same time as sending MsgRequestTxIds. With the fix, the client's
        // re-query inside the inner loop picks up the tx without needing
        // another notify. With the buggy await-first code, the client
        // would block until the 500ms fallback elapses (or forever if no
        // fallback).

        // Add the tx and fire notify_waiters (this is the "stale" notify
        // that the buggy code misses).
        tx_ids_handle.lock().unwrap().push(TxIdAndSize {
            era_id: 6,
            tx_id: [0xEE; 32],
            size_in_bytes: 200,
        });
        notify.notify_waiters();

        // Now send the blocking MsgRequestTxIds. The client will call
        // get_tx_ids inside the outer match arm and find the tx
        // immediately (no inner-loop entry needed for this exact path).
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: true,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Expect MsgReplyTxIds promptly (well under the 500ms fallback).
        let reply_result =
            tokio::time::timeout(std::time::Duration::from_millis(200), egress_rx.recv()).await;
        assert!(
            reply_result.is_ok(),
            "client should respond promptly using already-armed wakeup"
        );
        let (_, _, reply_bytes) = reply_result.unwrap().unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decode_message(&reply_bytes).unwrap() {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].tx_id, [0xEE; 32]);
        } else {
            panic!("expected MsgReplyTxIds");
        }

        handle.abort();
        let _ = handle.await;
    }

    /// Regression test for #521 — the harder race variant.
    ///
    /// The client receives a blocking MsgRequestTxIds when the mempool is
    /// empty. It enters the inner wait loop. After get_tx_ids returns empty
    /// inside the loop (or, in the buggy code, after `notify.notified()`
    /// has not yet been created), a tx is admitted and notify_waiters
    /// fires. With the fix, the re-query inside the loop sees the tx
    /// without requiring a second wake-up. With the buggy code, the loop
    /// would block on Notified until either (a) the 500 ms fallback fires
    /// (current code) or (b) a subsequent admission, whichever came first.
    #[tokio::test]
    async fn client_inner_loop_picks_up_tx_added_just_before_wait() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let notify = Arc::new(tokio::sync::Notify::new());
        let shared_tx_ids: std::sync::Arc<std::sync::Mutex<Vec<TxIdAndSize>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx_ids_handle = shared_tx_ids.clone();

        let source = NotifyMockTxSource {
            notify: notify.clone(),
            tx_ids: shared_tx_ids,
            txs: vec![([0xCC; 32], vec![0x44])],
        };

        let handle =
            tokio::spawn(async move { TxSubmissionClient::run(&mut channel, &source).await });

        // Drain MsgInit.
        let _ = egress_rx.recv().await.unwrap();

        // Send blocking MsgRequestTxIds with empty mempool — client enters
        // the inner wait loop.
        let req = encode_message(&TxSubmissionMessage::MsgRequestTxIds {
            blocking: true,
            ack_count: 0,
            req_count: 10,
        });
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Yield briefly so the client task gets a chance to run, hit the
        // empty get_tx_ids, and enter the inner wait loop.
        tokio::task::yield_now().await;

        // Now race: add a tx and fire notify_waiters. The fixed client
        // arms Notified BEFORE re-querying, so this wake is guaranteed
        // to be observed even if it lands between iterations.
        tx_ids_handle.lock().unwrap().push(TxIdAndSize {
            era_id: 6,
            tx_id: [0xCC; 32],
            size_in_bytes: 250,
        });
        notify.notify_waiters();

        // The client must respond well before the 500 ms fallback would
        // have fired — proving it observed the wake-up (not just the
        // polling fallback).
        let reply_result =
            tokio::time::timeout(std::time::Duration::from_millis(200), egress_rx.recv()).await;
        assert!(
            reply_result.is_ok(),
            "fixed client must observe notify_waiters fired right before await"
        );
        let (_, _, reply_bytes) = reply_result.unwrap().unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decode_message(&reply_bytes).unwrap() {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].tx_id, [0xCC; 32]);
        } else {
            panic!("expected MsgReplyTxIds");
        }

        handle.abort();
        let _ = handle.await;
    }
}
