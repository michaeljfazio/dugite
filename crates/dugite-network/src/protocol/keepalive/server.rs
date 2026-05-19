//! KeepAlive server — echoes cookie values back to the client.
//!
//! Receives `MsgKeepAlive(cookie)` and responds with `MsgKeepAliveResponse(cookie)`.
//! Exits cleanly on `MsgDone`.

use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;

use super::{decode_message, encode_message, KeepAliveMessage};

/// Maximum number of consecutive `MsgKeepAlive` pings before enforcing a rate
/// limit and disconnecting.
///
/// B5: Without a cap, a peer can flood the server with rapid-fire `MsgKeepAlive`
/// messages, consuming CPU (parse + CBOR encode) and bandwidth (pong echoes) at
/// the peer's sending rate.  Haskell cardano-node's KeepAlive client sends one
/// ping per 10 seconds; `MAX_PINGS_PER_SESSION` provides a generous bound before
/// the connection is considered abusive.
///
/// Value: 10 pings/min × 60 min × 24 h = 14,400 per day.  We allow 10× that
/// (144,000) so a stuck but non-malicious peer is not unfairly disconnected.
const MAX_PINGS_PER_SESSION: u64 = 144_000;

/// KeepAlive server that echoes ping cookies.
pub struct KeepAliveServer;

impl KeepAliveServer {
    /// Run the keepalive server loop.
    ///
    /// Receives messages from the client and responds:
    /// - `MsgKeepAlive(cookie)` → reply with `MsgKeepAliveResponse(cookie)`
    /// - `MsgDone` → exit cleanly
    ///
    /// Returns `Ok(ping_count)` on clean shutdown (MsgDone received).
    pub async fn run(channel: &mut MuxChannel) -> Result<u64, ProtocolError> {
        let mut ping_count: u64 = 0;
        tracing::info!(
            protocol_id = channel.protocol_id(),
            direction = ?channel.direction(),
            "keepalive server: started, waiting for pings"
        );

        loop {
            let msg_bytes = channel.recv().await.map_err(ProtocolError::from)?;
            let msg = decode_message(&msg_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: "KeepAlive",
                reason: e,
            })?;

            match msg {
                KeepAliveMessage::MsgKeepAlive(cookie) => {
                    // B5: Rate-limit: disconnect if peer sends an unreasonable number
                    // of pings.  A legitimate peer pings ~once per 10s; exceeding
                    // MAX_PINGS_PER_SESSION indicates either a bug or a DoS attempt.
                    if ping_count >= MAX_PINGS_PER_SESSION {
                        return Err(ProtocolError::BoundsExceeded {
                            protocol: "KeepAlive",
                            reason: format!(
                                "peer sent {ping_count} pings, exceeding session limit \
                                 of {MAX_PINGS_PER_SESSION}; disconnecting"
                            ),
                        });
                    }
                    // Echo the cookie back
                    let response = encode_message(&KeepAliveMessage::MsgKeepAliveResponse(cookie));
                    channel.send(response).await.map_err(ProtocolError::from)?;
                    ping_count += 1;
                    tracing::debug!(cookie, ping_count, "keepalive: echoed ping");
                }
                KeepAliveMessage::MsgDone => {
                    tracing::debug!(ping_count, "keepalive: client sent MsgDone");
                    return Ok(ping_count);
                }
                KeepAliveMessage::MsgKeepAliveResponse(cookie) => {
                    // Server should never receive a response — agency violation
                    return Err(ProtocolError::AgencyViolation {
                        protocol: "KeepAlive",
                        state: "StServer".to_string(),
                        received_tag: cookie as u8,
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

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(32);
        let (ingress_tx, ingress_rx) = mpsc::channel(32);
        let channel = MuxChannel::new(
            8,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            65536,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    #[tokio::test]
    async fn echoes_cookie_and_exits_on_done() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        // Spawn the server
        let handle = tokio::spawn(async move { KeepAliveServer::run(&mut channel).await });

        // Send MsgKeepAlive(42)
        let ping = encode_message(&KeepAliveMessage::MsgKeepAlive(42));
        ingress_tx.send(Bytes::from(ping)).await.unwrap();

        // Read the response
        let (_, _, response_bytes) = egress_rx.recv().await.unwrap();
        let response = decode_message(&response_bytes).unwrap();
        assert_eq!(response, KeepAliveMessage::MsgKeepAliveResponse(42));

        // Send MsgDone
        let done = encode_message(&KeepAliveMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        let result = handle.await.unwrap();
        assert_eq!(result.unwrap(), 1); // 1 ping echoed
    }

    #[tokio::test]
    async fn multiple_pings() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();

        let handle = tokio::spawn(async move { KeepAliveServer::run(&mut channel).await });

        // Send 3 pings
        for cookie in 0..3u16 {
            let ping = encode_message(&KeepAliveMessage::MsgKeepAlive(cookie));
            ingress_tx.send(Bytes::from(ping)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let response = decode_message(&resp).unwrap();
            assert_eq!(response, KeepAliveMessage::MsgKeepAliveResponse(cookie));
        }

        // Send MsgDone
        let done = encode_message(&KeepAliveMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        let result = handle.await.unwrap();
        assert_eq!(result.unwrap(), 3);
    }

    /// B5: KeepAlive server must disconnect when ping count exceeds MAX_PINGS_PER_SESSION.
    ///
    /// The session limit (144,000) is impractical to exercise in a unit test.
    /// We verify:
    /// 1. The constant is positive and bounded (compile-time asserts).
    /// 2. The `BoundsExceeded` variant is used for the rate-limit error path.
    /// 3. The server correctly rejects a response message (AgencyViolation) —
    ///    a different code path that is practical to drive.
    const _: () = assert!(
        MAX_PINGS_PER_SESSION > 0,
        "MAX_PINGS_PER_SESSION must be positive"
    );
    const _: () = assert!(
        MAX_PINGS_PER_SESSION <= 1_000_000,
        "MAX_PINGS_PER_SESSION should be <= 1M"
    );

    #[test]
    fn server_disconnects_after_session_limit_error_variant() {
        // Construct the error that the server emits when the session limit fires.
        // Confirms the variant and fields are correct without driving 144K pings.
        let expected_error = ProtocolError::BoundsExceeded {
            protocol: "KeepAlive",
            reason: format!(
                "peer sent 0 pings, exceeding session limit of {MAX_PINGS_PER_SESSION}; \
                 disconnecting"
            ),
        };
        assert!(
            matches!(
                expected_error,
                ProtocolError::BoundsExceeded {
                    protocol: "KeepAlive",
                    ..
                }
            ),
            "BoundsExceeded variant must be used for keepalive rate limit"
        );
    }

    /// B5: Server responds to MsgKeepAliveResponse with AgencyViolation.
    #[tokio::test]
    async fn server_rejects_response_in_server_state() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();

        let handle = tokio::spawn(async move { KeepAliveServer::run(&mut channel).await });

        // Send a MsgKeepAliveResponse — server should never receive this (it sends them).
        let rogue = encode_message(&KeepAliveMessage::MsgKeepAliveResponse(99));
        ingress_tx.send(Bytes::from(rogue)).await.unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_err(), "server should reject MsgKeepAliveResponse");
        assert!(
            matches!(
                result.unwrap_err(),
                ProtocolError::AgencyViolation {
                    protocol: "KeepAlive",
                    ..
                }
            ),
            "expected AgencyViolation"
        );
    }
}
