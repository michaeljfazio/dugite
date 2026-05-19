//! LocalStateQuery server — dispatches ledger queries from N2C clients.
//!
//! Handles the acquire/release state machine and dispatches queries to
//! a [`QueryHandler`] trait that the node integration layer implements.
//!
//! ## Query dispatch
//! The server decodes the outer query structure (HFC wrapping, Shelley BlockQuery
//! tag dispatch) and passes the tag + raw query CBOR to the QueryHandler.
//! The handler returns pre-encoded CBOR response bytes.

use minicbor::{Decoder, Encoder};

use crate::codec;
use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;

use super::{
    AcquireFailure, AcquireTarget, TAG_ACQUIRED, TAG_ACQUIRE_IMMUTABLE, TAG_ACQUIRE_SPECIFIC,
    TAG_ACQUIRE_VOLATILE, TAG_DONE, TAG_FAILURE, TAG_QUERY, TAG_REACQUIRE_SPECIFIC,
    TAG_REACQUIRE_VOLATILE, TAG_RELEASE, TAG_RESULT,
};

/// Trait for handling LocalStateQuery queries.
///
/// Implemented by the node integration layer, which has access to the ledger state.
pub trait QueryHandler: Send + Sync {
    /// Handle a raw query from the MsgQuery payload.
    ///
    /// `query_cbor` is the raw CBOR after the MsgQuery tag (i.e. the consensus-level
    /// query structure). The handler is responsible for parsing the full query
    /// hierarchy: consensus-level (BlockQuery/GetSystemStart/GetChainBlockNo/GetChainPoint),
    /// HFC-level (QueryIfCurrent/QueryAnytime/QueryHardFork), and era-level dispatch.
    ///
    /// Returns the fully-encoded MsgResult payload bytes (WITHOUT the `[4, ...]`
    /// envelope — the server adds that). For BlockQuery QueryIfCurrent results,
    /// wrap in HFC success `[1, result]`. For other query types, return unwrapped.
    fn handle_query(&self, query_cbor: &[u8], n2c_version: u16) -> Result<Vec<u8>, String>;

    /// Handle a Shelley BlockQuery by tag number.
    ///
    /// - `tag`: Shelley BlockQuery tag (0-38)
    /// - `query_cbor`: raw CBOR of the query parameters (after the tag)
    ///
    /// Returns the CBOR-encoded result, which will be wrapped in the HFC
    /// success envelope `[1, result]` by the server.
    fn handle_block_query(&self, tag: u64, query_cbor: &[u8]) -> Result<Vec<u8>, String>;

    /// Handle a QueryAnytime query (e.g., GetEraStart).
    /// Returns unwrapped CBOR result.
    fn handle_query_anytime(&self, query_cbor: &[u8]) -> Result<Vec<u8>, String>;

    /// Handle a QueryHardFork query (e.g., GetInterpreter).
    /// Returns unwrapped CBOR result.
    fn handle_query_hard_fork(&self, query_cbor: &[u8]) -> Result<Vec<u8>, String>;

    /// Validate an acquire target. Returns `Ok(())` if the target can be acquired,
    /// or `Err(AcquireFailure)` if the point is invalid.
    fn validate_acquire(&self, target: &AcquireTarget) -> Result<(), AcquireFailure>;
}

/// LocalStateQuery server.
pub struct LocalStateQueryServer;

impl LocalStateQueryServer {
    /// Run the LocalStateQuery server loop.
    ///
    /// `n2c_version` is the negotiated N2C protocol version (16-23) from the
    /// handshake phase, used for version-gated query dispatch.
    pub async fn run<H: QueryHandler>(
        channel: &mut MuxChannel,
        handler: &H,
        n2c_version: u16,
    ) -> Result<(), ProtocolError> {
        let mut acquired = false;

        loop {
            let msg_bytes = channel.recv().await.map_err(ProtocolError::from)?;
            let mut dec = Decoder::new(&msg_bytes);

            let _arr_len = dec.array().map_err(|e| ProtocolError::CborDecode {
                protocol: "LocalStateQuery",
                reason: e.to_string(),
            })?;
            let tag = dec.u64().map_err(|e| ProtocolError::CborDecode {
                protocol: "LocalStateQuery",
                reason: e.to_string(),
            })?;

            match tag {
                // MsgDone = [7]
                TAG_DONE => {
                    tracing::debug!("local state query: client done");
                    return Ok(());
                }

                // MsgAcquire: [0, point] / [8] / [10]
                TAG_ACQUIRE_SPECIFIC | TAG_ACQUIRE_VOLATILE | TAG_ACQUIRE_IMMUTABLE => {
                    let target = decode_acquire_target(tag, &mut dec)?;
                    match handler.validate_acquire(&target) {
                        Ok(()) => {
                            acquired = true;
                            // MsgAcquired = [1]
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(1).expect("infallible");
                            enc.u64(TAG_ACQUIRED).expect("infallible");
                            channel.send(buf).await.map_err(ProtocolError::from)?;
                        }
                        Err(failure) => {
                            // MsgFailure = [2, [tag]]
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(2).expect("infallible");
                            enc.u64(TAG_FAILURE).expect("infallible");
                            match failure {
                                AcquireFailure::PointTooOld => {
                                    enc.array(1).expect("infallible");
                                    enc.u8(0).expect("infallible");
                                }
                                AcquireFailure::PointNotOnChain => {
                                    enc.array(1).expect("infallible");
                                    enc.u8(1).expect("infallible");
                                }
                            }
                            channel.send(buf).await.map_err(ProtocolError::from)?;
                        }
                    }
                }

                // MsgReAcquire: [6, point] / [9] / [11]
                TAG_REACQUIRE_SPECIFIC
                | TAG_REACQUIRE_VOLATILE
                | super::TAG_REACQUIRE_IMMUTABLE => {
                    // Release old state, acquire new
                    let target = decode_acquire_target(tag, &mut dec)?;
                    match handler.validate_acquire(&target) {
                        Ok(()) => {
                            acquired = true;
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(1).expect("infallible");
                            enc.u64(TAG_ACQUIRED).expect("infallible");
                            channel.send(buf).await.map_err(ProtocolError::from)?;
                        }
                        Err(failure) => {
                            acquired = false; // Old state is also lost
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(2).expect("infallible");
                            enc.u64(TAG_FAILURE).expect("infallible");
                            match failure {
                                AcquireFailure::PointTooOld => {
                                    enc.array(1).expect("infallible");
                                    enc.u8(0).expect("infallible");
                                }
                                AcquireFailure::PointNotOnChain => {
                                    enc.array(1).expect("infallible");
                                    enc.u8(1).expect("infallible");
                                }
                            }
                            channel.send(buf).await.map_err(ProtocolError::from)?;
                        }
                    }
                }

                // MsgRelease = [5]
                TAG_RELEASE => {
                    acquired = false;
                }

                // MsgQuery = [3, query]
                TAG_QUERY => {
                    if !acquired {
                        return Err(ProtocolError::StateViolation {
                            protocol: "LocalStateQuery",
                            expected: "StAcquired".to_string(),
                            actual: "StIdle (not acquired)".to_string(),
                        });
                    }

                    // Pass the full consensus-level query CBOR to the handler.
                    // The handler dispatches BlockQuery/GetSystemStart/GetChainBlockNo/
                    // GetChainPoint and returns the result with proper HFC wrapping.
                    let query_start = dec.position();
                    let query_cbor = &msg_bytes[query_start..];

                    // C11 fix: cap the query blob size before attempting parse.
                    // A 1 MB blob of malformed deeply-nested CBOR (e.g. 0x81 bytes) can
                    // consume O(N) CPU time in minicbor's skip/array traversal before
                    // returning an error. The outer channel ingress limit is 1 MB;
                    // legitimate queries are tiny (< 1 KB). Cap at 4 KB.
                    const MAX_QUERY_CBOR_BYTES: usize = 4096;
                    if query_cbor.len() > MAX_QUERY_CBOR_BYTES {
                        return Err(ProtocolError::InvalidMessage {
                            protocol: "LocalStateQuery",
                            tag: TAG_QUERY as u8,
                            reason: format!(
                                "query blob too large: {} bytes (max {})",
                                query_cbor.len(),
                                MAX_QUERY_CBOR_BYTES
                            ),
                        });
                    }

                    let result_cbor =
                        handler.handle_query(query_cbor, n2c_version).map_err(|e| {
                            ProtocolError::CborDecode {
                                protocol: "LocalStateQuery",
                                reason: format!("query dispatch: {e}"),
                            }
                        })?;

                    // MsgResult = [4, result]
                    let mut buf = Vec::new();
                    {
                        let mut enc = Encoder::new(&mut buf);
                        enc.array(2).expect("infallible");
                        enc.u64(TAG_RESULT).expect("infallible");
                    }
                    buf.extend_from_slice(&result_cbor);

                    channel.send(buf).await.map_err(ProtocolError::from)?;
                }

                _ => {
                    return Err(ProtocolError::InvalidMessage {
                        protocol: "LocalStateQuery",
                        tag: tag as u8,
                        reason: format!("unexpected message tag: {tag}"),
                    });
                }
            }
        }
    }
}

/// Decode an acquire target from the message tag and remaining CBOR.
///
/// The target is determined by the message tag:
/// - Tag 0/6: SpecificPoint — remaining CBOR is the point
/// - Tag 8/9: VolatileTip — no additional data
/// - Tag 10/11: ImmutableTip — no additional data
fn decode_acquire_target(tag: u64, dec: &mut Decoder<'_>) -> Result<AcquireTarget, ProtocolError> {
    match tag {
        // SpecificPoint: tag 0 (Acquire) or tag 6 (ReAcquire) — point follows
        0 | 6 => {
            let point = codec::decode_point(dec).map_err(|e| ProtocolError::CborDecode {
                protocol: "LocalStateQuery",
                reason: format!("acquire point: {e}"),
            })?;
            Ok(AcquireTarget::SpecificPoint(point))
        }
        // VolatileTip: tag 8 (Acquire) or tag 9 (ReAcquire) — no additional data
        8 | 9 => Ok(AcquireTarget::VolatileTip),
        // ImmutableTip: tag 10 (Acquire) or tag 11 (ReAcquire) — no additional data
        10 | 11 => Ok(AcquireTarget::ImmutableTip),
        _ => Err(ProtocolError::InvalidMessage {
            protocol: "LocalStateQuery",
            tag: tag as u8,
            reason: format!("unexpected acquire/reacquire tag: {tag}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::sync::mpsc;

    /// Simple mock query handler for testing.
    struct MockQueryHandler;

    impl QueryHandler for MockQueryHandler {
        fn handle_query(&self, query_cbor: &[u8], _n2c_version: u16) -> Result<Vec<u8>, String> {
            // Simple mock: decode the consensus-level query and return a
            // canned response. For BlockQuery(QueryIfCurrent), return the
            // shelley tag wrapped in HFC success [1, tag].
            let mut dec = Decoder::new(query_cbor);
            let _ = dec.array();
            let outer_tag = dec.u64().unwrap_or(999);
            match outer_tag {
                0 => {
                    // BlockQuery → parse HFC inner
                    let _ = dec.array();
                    let hfc_tag = dec.u64().unwrap_or(999);
                    match hfc_tag {
                        0 => {
                            // QueryIfCurrent → parse shelley tag
                            let _ = dec.array();
                            let shelley_tag = dec.u64().unwrap_or(999);
                            let result = self.handle_block_query(shelley_tag, &[])?;
                            // Wrap in HFC success: [1, result]
                            let mut buf = Vec::new();
                            let mut enc = Encoder::new(&mut buf);
                            enc.array(2).expect("infallible");
                            enc.u64(1).expect("infallible");
                            buf.extend_from_slice(&result);
                            Ok(buf)
                        }
                        _ => {
                            let mut buf = Vec::new();
                            Encoder::new(&mut buf).str("hfc-other").expect("infallible");
                            Ok(buf)
                        }
                    }
                }
                _ => {
                    let mut buf = Vec::new();
                    Encoder::new(&mut buf).str("top-level").expect("infallible");
                    Ok(buf)
                }
            }
        }

        fn handle_block_query(&self, tag: u64, _query_cbor: &[u8]) -> Result<Vec<u8>, String> {
            // Return a simple response: the tag as a CBOR integer
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.u64(tag).expect("infallible");
            Ok(buf)
        }

        fn handle_query_anytime(&self, _query_cbor: &[u8]) -> Result<Vec<u8>, String> {
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.str("anytime").expect("infallible");
            Ok(buf)
        }

        fn handle_query_hard_fork(&self, _query_cbor: &[u8]) -> Result<Vec<u8>, String> {
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.str("hardfork").expect("infallible");
            Ok(buf)
        }

        fn validate_acquire(&self, target: &AcquireTarget) -> Result<(), AcquireFailure> {
            match target {
                AcquireTarget::VolatileTip | AcquireTarget::ImmutableTip => Ok(()),
                AcquireTarget::SpecificPoint(codec::Point::Origin) => Ok(()),
                AcquireTarget::SpecificPoint(codec::Point::Specific(slot, _)) => {
                    if *slot > 1000 {
                        Err(AcquireFailure::PointNotOnChain)
                    } else {
                        Ok(())
                    }
                }
            }
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
            7, // LocalStateQuery protocol ID
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    /// Encode MsgAcquire with VolatileTip target: [8]
    fn encode_acquire_volatile() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(8).expect("infallible"); // MsgAcquire tag
        buf
    }

    /// Encode MsgRelease: [5]
    fn encode_release() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(TAG_RELEASE).expect("infallible");
        buf
    }

    /// Encode MsgDone: [7]
    fn encode_done() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(TAG_DONE).expect("infallible");
        buf
    }

    /// Encode MsgQuery with a BlockQuery > QueryIfCurrent > Shelley tag:
    /// [3, [0, [0, [shelley_tag]]]]
    fn encode_block_query(shelley_tag: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).expect("infallible");
        enc.u64(TAG_QUERY).expect("infallible"); // MsgQuery
                                                 // Consensus-level BlockQuery: [0, hfc_query]
        enc.array(2).expect("infallible");
        enc.u64(0).expect("infallible"); // BlockQuery
                                         // HFC QueryIfCurrent: [0, shelley_query]
        enc.array(2).expect("infallible");
        enc.u64(0).expect("infallible"); // QueryIfCurrent
                                         // Shelley query: [shelley_tag]
        enc.array(1).expect("infallible");
        enc.u64(shelley_tag).expect("infallible");
        buf
    }

    #[tokio::test]
    async fn acquire_volatile_tip_succeeds() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Acquire volatile tip
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();

        // Should get MsgAcquired
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(dec.u64().unwrap(), TAG_ACQUIRED);

        // Release and done
        ingress_tx
            .send(Bytes::from(encode_release()))
            .await
            .unwrap();
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn query_dispatches_to_handler() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Acquire
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgAcquired

        // Query BlockQuery tag 5
        ingress_tx
            .send(Bytes::from(encode_block_query(5)))
            .await
            .unwrap();

        // Should get MsgResult
        let (_, _, result) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&result);
        dec.array().unwrap();
        let result_tag = dec.u64().unwrap();
        assert_eq!(result_tag, TAG_RESULT);
        // Result is wrapped: [1, 5] (success envelope with our mock response)
        dec.array().unwrap(); // HFC success array
        let success_tag = dec.u64().unwrap();
        assert_eq!(success_tag, 1); // success
        let inner_result = dec.u64().unwrap();
        assert_eq!(inner_result, 5); // our mock returns the tag number

        // Release and done
        ingress_tx
            .send(Bytes::from(encode_release()))
            .await
            .unwrap();
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgDone in StIdle (before acquire) causes a clean exit.
    #[tokio::test]
    async fn msg_done_before_acquire_exits_cleanly() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Send MsgDone without acquiring first.
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "MsgDone in StIdle should exit Ok: {result:?}"
        );
    }

    /// Verify that MsgQuery before acquiring returns a StateViolation error.
    #[tokio::test]
    async fn query_without_acquire_returns_state_violation() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Send MsgQuery without acquiring first.
        ingress_tx
            .send(Bytes::from(encode_block_query(0)))
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(
                result,
                Err(crate::error::ProtocolError::StateViolation { .. })
            ),
            "expected StateViolation for query without acquire, got: {result:?}"
        );
    }

    /// Verify that MsgRelease in StAcquired drops the acquired state.
    /// A subsequent MsgQuery (without re-acquiring) must fail with StateViolation.
    #[tokio::test]
    async fn release_clears_acquired_state() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Acquire.
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgAcquired

        // Release.
        ingress_tx
            .send(Bytes::from(encode_release()))
            .await
            .unwrap();

        // Attempt query without re-acquiring.
        ingress_tx
            .send(Bytes::from(encode_block_query(0)))
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(
                result,
                Err(crate::error::ProtocolError::StateViolation { .. })
            ),
            "expected StateViolation after release + query, got: {result:?}"
        );
    }

    /// Verify acquire of ImmutableTip ([10]) succeeds and sets acquired state.
    #[tokio::test]
    async fn acquire_immutable_tip_succeeds() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // MsgAcquire ImmutableTip = [10]
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(TAG_ACQUIRE_IMMUTABLE).expect("infallible");
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Should get MsgAcquired.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(
            dec.u64().unwrap(),
            TAG_ACQUIRED,
            "ImmutableTip acquire should return MsgAcquired"
        );

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify acquire of a specific point that fails PointNotOnChain returns MsgFailure.
    #[tokio::test]
    async fn acquire_specific_point_not_on_chain_returns_failure() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler; // slot > 1000 → PointNotOnChain

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // MsgAcquire specific point at slot 9999 (> 1000 → PointNotOnChain).
        // Point::Specific wire format (codec::encode_point): [slot, hash_bytes]
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).expect("infallible");
            enc.u64(TAG_ACQUIRE_SPECIFIC).expect("infallible");
            // Point::Specific(9999, hash) → [9999, hash_bytes]
            enc.array(2).expect("infallible");
            enc.u64(9999).expect("infallible");
            enc.bytes(&[0xFFu8; 32]).expect("infallible");
        }
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Should get MsgFailure.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        let tag = dec.u64().unwrap();
        assert_eq!(
            tag, TAG_FAILURE,
            "out-of-chain point should return MsgFailure"
        );

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify MsgReAcquire in StAcquired succeeds (replaces previous acquire).
    #[tokio::test]
    async fn reacquire_volatile_tip_succeeds() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // First acquire.
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgAcquired

        // ReAcquire VolatileTip = [9].
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(TAG_REACQUIRE_VOLATILE).expect("infallible");
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Should get MsgAcquired again.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(
            dec.u64().unwrap(),
            TAG_ACQUIRED,
            "ReAcquire should return MsgAcquired"
        );

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify multiple queries in a single acquire session are all answered.
    #[tokio::test]
    async fn multiple_queries_in_one_acquire_session() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgAcquired

        // Issue 3 queries.
        for shelley_tag in [0u64, 5, 14] {
            ingress_tx
                .send(Bytes::from(encode_block_query(shelley_tag)))
                .await
                .unwrap();
            let (_, _, result) = egress_rx.recv().await.unwrap();
            let mut dec = Decoder::new(&result);
            dec.array().unwrap();
            let result_tag = dec.u64().unwrap();
            assert_eq!(
                result_tag, TAG_RESULT,
                "query {shelley_tag} should get MsgResult"
            );
        }

        ingress_tx
            .send(Bytes::from(encode_release()))
            .await
            .unwrap();
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that an unexpected message tag returns an error.
    #[tokio::test]
    async fn unknown_message_tag_returns_error() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // Send a message with tag 99 (not a valid LSQ tag).
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(1).expect("infallible");
        enc.u64(99).expect("infallible");
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(
                result,
                Err(crate::error::ProtocolError::InvalidMessage { .. })
            ),
            "unknown tag should return InvalidMessage, got: {result:?}"
        );
    }

    /// Verify acquire/query/release/acquire cycle works correctly.
    ///
    /// This exercises the full state machine: StIdle → StAcquiring → StAcquired →
    /// StQuerying → StAcquired → StIdle → StAcquiring → StAcquired → StDone.
    #[tokio::test]
    async fn acquire_query_release_reacquire_cycle() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // First acquire.
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgAcquired

        // Query.
        ingress_tx
            .send(Bytes::from(encode_block_query(1)))
            .await
            .unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgResult

        // Release.
        ingress_tx
            .send(Bytes::from(encode_release()))
            .await
            .unwrap();

        // Second acquire.
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(
            dec.u64().unwrap(),
            TAG_ACQUIRED,
            "second acquire should succeed"
        );

        // Done.
        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ── C11 test: oversized MsgQuery blob is rejected ─────────────────────────

    /// C11: a MsgQuery with > 4096 bytes of query CBOR must disconnect the client.
    #[tokio::test]
    async fn c11_oversized_query_blob_rejected() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // First acquire so we can send MsgQuery.
        ingress_tx
            .send(Bytes::from(encode_acquire_volatile()))
            .await
            .unwrap();

        // Build MsgQuery with a 5000-byte query blob (exceeds MAX_QUERY_CBOR_BYTES=4096).
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).expect("infallible");
            enc.u64(TAG_QUERY).expect("infallible");
            // Write 5000 bytes of padding as a CBOR byte-string.
            enc.bytes(&[0x00u8; 5000]).expect("infallible");
        }
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Server should terminate with an InvalidMessage error (not panic).
        let result = handle.await.unwrap();
        assert!(
            matches!(
                result,
                Err(crate::error::ProtocolError::InvalidMessage { .. })
            ),
            "oversized query must return InvalidMessage, got: {result:?}"
        );
    }

    /// C1 server test: SpecificPoint at slot > 1000 returns MsgFailure PointNotOnChain.
    #[tokio::test]
    async fn c1_specific_point_not_on_chain_returns_failure() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler; // rejects slot > 1000

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // MsgAcquire SpecificPoint at slot 2000 — MockQueryHandler returns PointNotOnChain.
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).expect("infallible");
            enc.u64(TAG_ACQUIRE_SPECIFIC).expect("infallible");
            // Point::Specific(2000, [0xAA; 32]) wire format: [2000, bytes(32)]
            enc.array(2).expect("infallible");
            enc.u64(2000).expect("infallible");
            enc.bytes(&[0xAAu8; 32]).expect("infallible");
        }
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        // Expect MsgFailure = [2, [1]]  (tag 2, PointNotOnChain = [1])
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        let tag = dec.u64().unwrap();
        assert_eq!(tag, TAG_FAILURE, "must send MsgFailure");
        // Inner failure: array(1)[1] = PointNotOnChain
        let inner_len = dec.array().unwrap().unwrap();
        assert_eq!(inner_len, 1);
        let code = dec.u8().unwrap();
        assert_eq!(code, 1, "PointNotOnChain wire code must be 1");

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify acquire of SpecificPoint at Origin always succeeds.
    #[tokio::test]
    async fn acquire_specific_point_origin_succeeds() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let handler = MockQueryHandler;

        let handle =
            tokio::spawn(
                async move { LocalStateQueryServer::run(&mut channel, &handler, 16).await },
            );

        // MsgAcquire SpecificPoint at Origin.
        // Point::Origin wire format (codec::encode_point): []  (empty array, length 0)
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.array(2).expect("infallible");
            enc.u64(TAG_ACQUIRE_SPECIFIC).expect("infallible");
            // Point::Origin → [] (empty array)
            enc.array(0).expect("infallible");
        }
        ingress_tx.send(Bytes::from(buf)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let mut dec = Decoder::new(&resp);
        dec.array().unwrap();
        assert_eq!(dec.u64().unwrap(), TAG_ACQUIRED);

        ingress_tx.send(Bytes::from(encode_done())).await.unwrap();
        handle.await.unwrap().unwrap();
    }
}
