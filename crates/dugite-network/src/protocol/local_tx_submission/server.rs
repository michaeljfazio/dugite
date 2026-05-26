//! LocalTxSubmission server — validates and accepts transactions from N2C clients.
//!
//! Receives `MsgSubmitTx` with era ID and raw tx CBOR, validates via `TxValidator`,
//! and responds with `MsgAcceptTx` or `MsgRejectTx`.

use minicbor::{Decoder, Encoder};

use crate::error::{MuxError, ProtocolError};
use crate::mux::channel::MuxChannel;
use crate::TxValidator;

// CBOR message tags for LocalTxSubmission
const TAG_SUBMIT_TX: u64 = 0;
const TAG_ACCEPT_TX: u64 = 1;
const TAG_REJECT_TX: u64 = 2;
const TAG_DONE: u64 = 3;

/// LocalTxSubmission server statistics.
#[derive(Debug, Clone, Default)]
pub struct LocalTxSubmissionStats {
    /// Number of transactions submitted.
    pub submitted: u64,
    /// Number of transactions accepted.
    pub accepted: u64,
    /// Number of transactions rejected.
    pub rejected: u64,
}

/// LocalTxSubmission server that validates transactions from N2C clients.
pub struct LocalTxSubmissionServer;

impl LocalTxSubmissionServer {
    /// Run the LocalTxSubmission server loop.
    ///
    /// `on_accepted` is called with `(era_id, tx_bytes)` for each validated transaction.
    /// It must return `Ok(())` if the tx was successfully added to the mempool, or
    /// `Err(reason)` if the mempool rejected it (e.g. capacity exceeded). On error,
    /// the server sends `MsgRejectTx` rather than `MsgAcceptTx`, preventing the
    /// protocol violation of claiming acceptance for an un-propagated transaction (C12).
    pub async fn run<V, F>(
        channel: &mut MuxChannel,
        validator: &V,
        mut on_accepted: F,
    ) -> Result<LocalTxSubmissionStats, ProtocolError>
    where
        V: TxValidator,
        F: FnMut(u16, Vec<u8>) -> Result<(), String> + Send,
    {
        let mut stats = LocalTxSubmissionStats::default();

        loop {
            let msg_bytes = match channel.recv().await {
                Ok(b) => b,
                // Client closed the connection without sending MsgDone — treat as
                // a graceful disconnect and return the accumulated stats so that
                // the node can update its metrics correctly.
                Err(MuxError::BearerClosed | MuxError::ChannelClosed) => {
                    tracing::debug!(?stats, "local tx submission: client disconnected");
                    return Ok(stats);
                }
                Err(e) => return Err(ProtocolError::from(e)),
            };
            let mut dec = Decoder::new(&msg_bytes);

            let _arr_len = dec.array().map_err(|e| ProtocolError::CborDecode {
                protocol: "LocalTxSubmission",
                reason: e.to_string(),
            })?;
            let tag = dec.u64().map_err(|e| ProtocolError::CborDecode {
                protocol: "LocalTxSubmission",
                reason: e.to_string(),
            })?;

            match tag {
                TAG_SUBMIT_TX => {
                    stats.submitted += 1;

                    // Decode [era_id, tx_bytes]
                    let _inner_arr = dec.array().map_err(|e| ProtocolError::CborDecode {
                        protocol: "LocalTxSubmission",
                        reason: e.to_string(),
                    })?;
                    let era_id = dec.u16().map_err(|e| ProtocolError::CborDecode {
                        protocol: "LocalTxSubmission",
                        reason: e.to_string(),
                    })?;
                    // The tx may be wrapped in CBOR tag 24 (wrapCBORinCBOR).
                    // C9 fix: use `dec.datatype()` to detect the CBOR type before
                    // consuming — this avoids the rewind bug where a non-24 tag would
                    // leave the decoder positioned before the tag byte, causing
                    // `dec.bytes()` to fail with a type mismatch instead of sending
                    // a structured MsgRejectTx. Also reject any tag that is NOT 24
                    // (including double-wrapped tag(24)(tag(24)(...))): the CDDL for
                    // LocalTxSubmission requires exactly zero or one tag-24 wrapper.
                    let tx_bytes = {
                        use minicbor::data::Type;
                        match dec.datatype() {
                            Ok(Type::Tag) => {
                                let tag = dec.tag().map_err(|e| ProtocolError::CborDecode {
                                    protocol: "LocalTxSubmission",
                                    reason: e.to_string(),
                                })?;
                                if tag.as_u64() != 24 {
                                    // Non-24 tag — reject with structured error instead of
                                    // dropping the connection.
                                    tracing::warn!(
                                        tag = tag.as_u64(),
                                        "N2C tx rejected: unexpected CBOR tag in tx payload (expected tag(24) or raw bytes)"
                                    );
                                    // Build a MsgRejectTx with a decode-failure reason
                                    let apply_tx_err = super::encode::encode_apply_tx_err(
                                        &crate::TxValidationError::Other(
                                            "malformed transaction encoding".to_string(),
                                        ),
                                        era_id,
                                    );
                                    let mut buf = Vec::new();
                                    let mut enc = Encoder::new(&mut buf);
                                    enc.array(2).expect("infallible");
                                    enc.u64(TAG_REJECT_TX).expect("infallible");
                                    let writer = enc.writer_mut();
                                    writer.extend_from_slice(&apply_tx_err);
                                    channel.send(buf).await.map_err(ProtocolError::from)?;
                                    stats.rejected += 1;
                                    continue;
                                }
                                // tag 24 consumed; the inner bytes follow
                                dec.bytes()
                                    .map_err(|e| ProtocolError::CborDecode {
                                        protocol: "LocalTxSubmission",
                                        reason: e.to_string(),
                                    })?
                                    .to_vec()
                            }
                            Ok(Type::Bytes) | Ok(_) => {
                                // No tag wrapper — read raw bytes directly
                                dec.bytes()
                                    .map_err(|e| ProtocolError::CborDecode {
                                        protocol: "LocalTxSubmission",
                                        reason: e.to_string(),
                                    })?
                                    .to_vec()
                            }
                            Err(e) => {
                                return Err(ProtocolError::CborDecode {
                                    protocol: "LocalTxSubmission",
                                    reason: e.to_string(),
                                });
                            }
                        }
                    };

                    // Validate via TxValidator
                    match validator.validate_tx(era_id, &tx_bytes) {
                        Ok(()) => {
                            // C12 fix: the `on_accepted` closure must return Ok(()) if the
                            // transaction was successfully added to the mempool, or Err(reason)
                            // if the mempool rejected it (e.g. capacity exceeded). We MUST NOT
                            // send MsgAcceptTx if the tx was not actually admitted, as that is a
                            // protocol violation — the client believes the tx will be propagated.
                            match on_accepted(era_id, tx_bytes) {
                                Ok(()) => {
                                    stats.accepted += 1;

                                    // Send MsgAcceptTx
                                    let mut buf = Vec::new();
                                    let mut enc = Encoder::new(&mut buf);
                                    enc.array(1).expect("infallible");
                                    enc.u64(TAG_ACCEPT_TX).expect("infallible");
                                    channel.send(buf).await.map_err(ProtocolError::from)?;
                                }
                                Err(reason) => {
                                    // Mempool admitted the tx failed after validator Ok.
                                    // Send MsgRejectTx with a generic mempool-full reason.
                                    stats.rejected += 1;
                                    tracing::warn!(era_id, %reason, "N2C tx rejected: mempool add failed after validator Ok (duplicate or full)");
                                    let apply_tx_err = super::encode::encode_apply_tx_err(
                                        &crate::TxValidationError::Other(
                                            "mempool full or duplicate".to_string(),
                                        ),
                                        era_id,
                                    );
                                    let mut buf = Vec::new();
                                    let mut enc = Encoder::new(&mut buf);
                                    enc.array(2).expect("infallible");
                                    enc.u64(TAG_REJECT_TX).expect("infallible");
                                    let writer = enc.writer_mut();
                                    writer.extend_from_slice(&apply_tx_err);
                                    channel.send(buf).await.map_err(ProtocolError::from)?;
                                }
                            }
                        }
                        Err(e) => {
                            stats.rejected += 1;
                            tracing::warn!(era_id, reason = %format!("{e:?}"), "N2C tx rejected: validation failed");

                            // Send MsgRejectTx = [2, ApplyTxErr]
                            // where ApplyTxErr = [[era_id, [failure_0, ...]]]
                            // encoded as structured CBOR matching Haskell cardano-node.
                            let apply_tx_err = super::encode::encode_apply_tx_err(&e, era_id);
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(2).expect("infallible");
                            enc.u64(TAG_REJECT_TX).expect("infallible");
                            let writer = enc.writer_mut();
                            writer.extend_from_slice(&apply_tx_err);
                            channel.send(buf).await.map_err(ProtocolError::from)?;
                        }
                    }
                }
                TAG_DONE => {
                    tracing::debug!(?stats, "local tx submission: client done");
                    return Ok(stats);
                }
                _ => {
                    return Err(ProtocolError::InvalidMessage {
                        protocol: "LocalTxSubmission",
                        tag: tag as u8,
                        reason: format!("unexpected message tag: {tag}"),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TxValidationError;
    use bytes::Bytes;
    use tokio::sync::mpsc;

    struct AcceptAllValidator;
    impl TxValidator for AcceptAllValidator {
        fn validate_tx(&self, _era_id: u16, _tx_bytes: &[u8]) -> Result<(), TxValidationError> {
            Ok(())
        }
    }

    struct RejectAllValidator;
    impl TxValidator for RejectAllValidator {
        fn validate_tx(&self, _era_id: u16, _tx_bytes: &[u8]) -> Result<(), TxValidationError> {
            Err(TxValidationError::Other("test rejection".to_string()))
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
            6,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    /// Encode a MsgSubmitTx for testing.
    fn encode_submit_tx(era_id: u16, tx_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).expect("infallible");
        enc.u64(TAG_SUBMIT_TX).expect("infallible");
        enc.array(2).expect("infallible");
        enc.u16(era_id).expect("infallible");
        enc.bytes(tx_bytes).expect("infallible");
        buf
    }

    /// Encode a MsgDone for testing.
    fn encode_done() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(TAG_DONE).expect("infallible");
        buf
    }

    #[tokio::test]
    async fn accepts_valid_tx() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = AcceptAllValidator;
        let accepted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let accepted_clone = accepted.clone();

        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, move |era, tx| {
                accepted_clone.lock().unwrap().push((era, tx));
                Ok(())
            })
            .await
        });

        // Submit a tx
        let submit = encode_submit_tx(6, &[0xDE, 0xAD]);
        ingress_tx.send(Bytes::from(submit)).await.unwrap();

        // Should get MsgAcceptTx
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(dec.u64().unwrap(), TAG_ACCEPT_TX);

        // Send MsgDone
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();

        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.submitted, 1);
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.rejected, 0);

        let accepted = accepted.lock().unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].0, 6); // era
        assert_eq!(accepted[0].1, vec![0xDE, 0xAD]); // tx bytes
    }

    #[tokio::test]
    async fn rejects_invalid_tx() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = RejectAllValidator;

        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, |_, _| Ok(())).await
        });

        let submit = encode_submit_tx(6, &[0xBA, 0xAD]);
        ingress_tx.send(Bytes::from(submit)).await.unwrap();

        // Should get MsgRejectTx with structured CBOR
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(dec.u64().unwrap(), TAG_REJECT_TX);

        // Verify ApplyTxErr structure: [[era_id, [failure_0, ...]]]
        let outer_len = dec.array().unwrap().unwrap();
        assert_eq!(outer_len, 1, "outer HFC wrapper must be array(1)");
        let inner_len = dec.array().unwrap().unwrap();
        assert_eq!(inner_len, 2, "inner must be [era_id, failures]");
        let era_id = dec.u16().unwrap();
        assert_eq!(era_id, 6);
        let n_failures = dec.array().unwrap().unwrap();
        assert_eq!(n_failures, 1, "one rejection failure");

        // The failure should be ConwayMempoolFailure (tag 7) since RejectAllValidator
        // returns Other("test rejection") which falls through to mempool fallback.
        // C8 fix: the text must NOT contain internal Rust debug formatting.
        let failure_len = dec.array().unwrap().unwrap();
        assert_eq!(failure_len, 2);
        let ledger_tag = dec.u8().unwrap();
        assert_eq!(ledger_tag, 7, "ConwayMempoolFailure");
        let text = dec.str().unwrap();
        // The sanitized message must not contain Rust struct/field names.
        assert!(
            !text.contains('{') && !text.contains('}') && !text.contains("Other"),
            "rejection text must be sanitized: got {text:?}"
        );

        // Send MsgDone
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();

        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.submitted, 1);
        assert_eq!(stats.accepted, 0);
        assert_eq!(stats.rejected, 1);
    }

    #[tokio::test]
    async fn rejects_with_structured_fee_error() {
        // Test that a specific validation error produces correct structured CBOR
        struct FeeTooSmallValidator;
        impl TxValidator for FeeTooSmallValidator {
            fn validate_tx(&self, _era_id: u16, _tx_bytes: &[u8]) -> Result<(), TxValidationError> {
                Err(TxValidationError::FeeTooSmall {
                    minimum: 200_000,
                    actual: 170_000,
                })
            }
        }

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = FeeTooSmallValidator;

        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, |_, _| Ok(())).await
        });

        let submit = encode_submit_tx(6, &[0xDE, 0xAD]);
        ingress_tx.send(Bytes::from(submit)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap(); // MsgRejectTx
        assert_eq!(dec.u64().unwrap(), TAG_REJECT_TX);

        // ApplyTxErr outer wrapper
        dec.array().unwrap(); // [[...]]
        dec.array().unwrap(); // [era_id, [...]]
        assert_eq!(dec.u16().unwrap(), 6);
        dec.array().unwrap(); // failures

        // ConwayLedgerPredFailure(1) → ConwayUtxowPredFailure(0) → ConwayUtxoPredFailure(5)
        dec.array().unwrap();
        assert_eq!(dec.u8().unwrap(), 1, "ConwayUtxowFailure");
        dec.array().unwrap();
        assert_eq!(dec.u8().unwrap(), 0, "UtxoFailure");
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        assert_eq!(dec.u8().unwrap(), 5, "FeeTooSmallUTxO");
        assert_eq!(dec.u64().unwrap(), 200_000, "min fee first");
        assert_eq!(dec.u64().unwrap(), 170_000, "actual fee second");

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.rejected, 1);
    }

    // ── C12 tests: mempool add failure after validator Ok ─────────────────────

    /// C12: when on_accepted returns Err, server must send MsgRejectTx (not MsgAcceptTx).
    #[tokio::test]
    async fn c12_mempool_full_after_validation_sends_reject() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = AcceptAllValidator;

        // on_accepted returns Err to simulate a full mempool.
        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, |_, _| {
                Err("mempool full".to_string())
            })
            .await
        });

        let submit = encode_submit_tx(6, &[0xDE, 0xAD]);
        ingress_tx.send(Bytes::from(submit)).await.unwrap();

        // Must receive MsgRejectTx, NOT MsgAcceptTx.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        let tag = dec.u64().unwrap();
        assert_eq!(
            tag, TAG_REJECT_TX,
            "mempool full after validation must send MsgRejectTx (got tag {tag})"
        );

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.submitted, 1);
        assert_eq!(stats.accepted, 0, "accepted must be 0 when mempool rejects");
        assert_eq!(stats.rejected, 1, "rejected must be 1");
    }

    /// C12: when on_accepted returns Ok, server sends MsgAcceptTx as before.
    #[tokio::test]
    async fn c12_mempool_ok_sends_accept() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = AcceptAllValidator;

        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, |_, _| Ok(())).await
        });

        let submit = encode_submit_tx(6, &[0xDE, 0xAD]);
        ingress_tx.send(Bytes::from(submit)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        let tag = dec.u64().unwrap();
        assert_eq!(
            tag, TAG_ACCEPT_TX,
            "successful mempool add must send MsgAcceptTx"
        );

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.rejected, 0);
    }

    // ── C9 test: non-24 CBOR tag in tx payload ────────────────────────────────

    /// C9: tag(99)(bytes) must receive MsgRejectTx instead of dropping the connection.
    #[tokio::test]
    async fn c9_non_24_tag_sends_reject_not_disconnect() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let validator = AcceptAllValidator;

        let handle = tokio::spawn(async move {
            LocalTxSubmissionServer::run(&mut channel, &validator, |_, _| Ok(())).await
        });

        // Build [0, [6, tag(99)(bytes([0xDE, 0xAD]))]]
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).expect("infallible");
        enc.u64(TAG_SUBMIT_TX).expect("infallible");
        enc.array(2).expect("infallible");
        enc.u16(6).expect("infallible"); // era_id
        enc.tag(minicbor::data::Tag::new(99)).expect("infallible");
        enc.bytes(&[0xDE, 0xAD]).expect("infallible");

        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Must receive MsgRejectTx (tag 2), not a connection drop.
        let response = egress_rx.recv().await;
        assert!(response.is_some(), "server must respond (not disconnect)");
        let (_, _, resp) = response.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        let tag = dec.u64().unwrap();
        assert_eq!(
            tag, TAG_REJECT_TX,
            "non-24 tag must produce MsgRejectTx (got tag {tag})"
        );

        // Server should still be running — send MsgDone to clean up.
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        let _ = handle.await;
    }
}
