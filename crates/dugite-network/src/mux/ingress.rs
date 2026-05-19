//! Ingress task — reads multiplexed SDU segments from the bearer and dispatches them.
//!
//! Reads 8-byte SDU headers followed by payloads from the bearer, flips the direction
//! bit (remote's InitiatorDir → our ResponderDir), and dispatches payloads to the
//! appropriate per-protocol channel.
//!
//! ## Protocol ID 1
//! Protocol ID 1 is reserved in the Ouroboros spec and never used. A frame on this
//! protocol ID is treated as a protocol error and terminates the connection (A-011).
//!
//! ## Unknown protocol IDs
//! Any frame for a protocol ID not registered in the subscription table is treated
//! as a protocol error and terminates the connection (A-005). This matches Haskell's
//! `Ouroboros.Network.Mux.Ingress` which errors on unknown protocol IDs.
//!
//! ## Byte tracking
//! The ingress task tracks how many bytes are currently buffered per
//! `(protocol_id, direction)` channel using an `Arc<AtomicUsize>` counter shared
//! with the corresponding [`MuxChannel`] receiver. The receiver decrements the
//! counter as it consumes data, giving the ingress task an accurate view of queue
//! pressure. If the counter exceeds the configured per-channel limit, the ingress
//! task returns `IngressQueueOverrun` to disconnect the peer.

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::error::{BearerError, MuxError};
use crate::mux::segment::{decode_header, Direction, HEADER_SIZE};

/// Reserved protocol ID that is silently discarded on ingress.
const RESERVED_PROTOCOL_ID: u16 = 1;

/// Per-SDU read deadline matching Haskell's `sduTimeout` (30 seconds).
///
/// This timeout applies ONLY to in-progress SDU reads — NOT to idle waits.
/// Matching Haskell's two-phase bearer read design:
///
/// - **Phase 1 (header):** block indefinitely waiting for the next SDU header.
///   The Haskell mux (`Bearer/Socket.hs`) calls `recvAtMost True msHeaderLength`
///   with NO timeout, allowing mini-protocols to have arbitrarily long idle
///   periods (e.g., ChainSync waiting at tip, KeepAlive intervals).
///
/// - **Phase 2 (payload):** once a header arrives, the payload MUST follow
///   within `sduTimeout` (30 seconds). This enforces a minimum transfer rate
///   of ~17 kbps (max SDU = 65543 bytes × 8 bits / 30s ≈ 17 kbps), matching
///   the Haskell spec comment in `ConnectionHandler.hs`.
///
/// The mux relies on KeepAlive (10-second pings) for TCP liveness detection,
/// NOT on SDU read timeouts. An idle connection is perfectly valid — it just
/// means no mini-protocol has data to send.
const SDU_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-protocol ingress channel registration.
pub(crate) struct IngressRoute {
    /// Sender to deliver byte chunks to the protocol's MuxChannel.
    pub tx: mpsc::Sender<Bytes>,
    /// Maximum bytes allowed in this channel's queue before overrun.
    pub limit: usize,
    /// Shared byte counter between this ingress route and the corresponding
    /// MuxChannel receiver. Incremented here when data is enqueued; decremented
    /// by MuxChannel::recv() as data is consumed. This gives an accurate measure
    /// of queue pressure without requiring channel introspection.
    pub bytes_in_flight: Arc<AtomicUsize>,
}

/// Ingress task state. Created by the [`Mux`] and run as a spawned tokio task.
pub struct IngressTask {
    /// Registered protocol channels, keyed by `(protocol_id, direction)`.
    routes: HashMap<(u16, Direction), IngressRoute>,
}

impl IngressTask {
    /// Create a new ingress task with the given protocol routes.
    pub(crate) fn new(routes: HashMap<(u16, Direction), IngressRoute>) -> Self {
        Self { routes }
    }

    /// Run the ingress loop. Reads SDU headers + payloads from the bearer and
    /// dispatches to registered protocol channels.
    ///
    /// The `read_fn` is called to read exact byte counts from the bearer.
    ///
    /// Returns `Ok(())` when the bearer is cleanly closed (EOF).
    /// Returns `Err(MuxError)` on read errors or queue overruns.
    pub async fn run<R>(mut self, mut read_fn: R) -> Result<(), MuxError>
    where
        R: FnMut(
                usize,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<u8>, BearerError>> + Send>,
            > + Send,
    {
        loop {
            // Phase 1: Wait indefinitely for the 8-byte SDU header.
            //
            // Matching Haskell's `Bearer/Socket.hs`: the first recv has NO
            // timeout, blocking until the remote peer sends the next SDU.
            // This is correct because idle connections are valid — mini-protocols
            // can have arbitrarily long pauses (ChainSync at tip waiting for
            // blocks, KeepAlive 10-second intervals, etc.). The mux never
            // spontaneously closes an idle connection; liveness is handled by
            // the KeepAlive protocol and OS-level TCP keepalive.
            let header_bytes = match read_fn(HEADER_SIZE).await {
                Ok(bytes) => bytes,
                Err(BearerError::ConnectionReset) => {
                    tracing::debug!("mux ingress: bearer connection reset (clean EOF)");
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("mux ingress: bearer read error: {e}");
                    return Err(MuxError::Bearer(e));
                }
            };

            let header = decode_header(header_bytes[..8].try_into().expect("read exact 8 bytes"));

            // A-011 (security audit 2026-05-19): reject reserved protocol ID 1.
            // Per the Ouroboros mux spec, protocol ID 1 is permanently reserved and
            // must never carry data. A peer sending frames on it has violated the
            // protocol — treat as a fatal error rather than silently discarding,
            // matching Haskell's `error "unknown protocol"` in Ouroboros.Network.Mux.
            if header.protocol_id == RESERVED_PROTOCOL_ID {
                return Err(MuxError::InvalidHeader {
                    protocol_id: header.protocol_id,
                    payload_len: header.payload_length,
                });
            }

            // Phase 2: Read the payload with sduTimeout (30s).
            //
            // Once the header has arrived, the payload MUST follow within
            // 30 seconds. This enforces a minimum transfer rate and detects
            // stalled mid-SDU transfers (e.g., half-open TCP connections).
            let payload = if header.payload_length > 0 {
                tokio::time::timeout(SDU_READ_TIMEOUT, read_fn(header.payload_length as usize))
                    .await
                    .map_err(|_| MuxError::SduReadTimeout)?
                    .map_err(MuxError::Bearer)?
            } else {
                Vec::new()
            };

            // Flip direction: what the remote sent as InitiatorDir, we receive as ResponderDir
            let local_direction = header.direction.flip();
            let key = (header.protocol_id, local_direction);

            // Temporarily log ALL SDU routing at debug for duplex diagnostics.
            tracing::debug!(
                protocol_id = header.protocol_id,
                wire_direction = ?header.direction,
                local_direction = ?local_direction,
                payload_len = header.payload_length,
                "ingress: routing SDU"
            );

            match self.routes.get_mut(&key) {
                Some(route) => {
                    let payload_len = payload.len();

                    // A-012 (security audit 2026-05-19): skip counter ops for
                    // zero-length payloads entirely — fetch_add(0)+fetch_sub(0) is
                    // a no-op and the atomic bus lock is wasted work.
                    if payload_len == 0 {
                        // Zero-length SDUs are valid per spec (keep-alive in some
                        // implementations). Accept silently with no counter change.
                        continue;
                    }

                    // Atomically add the incoming payload size and check whether
                    // we have exceeded the per-channel byte budget.
                    //
                    // `fetch_add` returns the value *before* addition, so the new
                    // total is `prev + payload_len`.
                    let prev = route
                        .bytes_in_flight
                        .fetch_add(payload_len, Ordering::Relaxed);
                    let new_total = prev + payload_len;

                    if new_total > route.limit {
                        // Revert the addition before returning the error so that
                        // the counter stays consistent if this route is somehow
                        // reused (though in practice the connection is torn down).
                        route
                            .bytes_in_flight
                            .fetch_sub(payload_len, Ordering::Relaxed);
                        return Err(MuxError::IngressQueueOverrun {
                            protocol_id: header.protocol_id,
                            bytes: new_total,
                            limit: route.limit,
                        });
                    }

                    // Deliver to protocol channel using try_send (non-blocking).
                    //
                    // CRITICAL: We must NOT use the blocking `.send().await` here.
                    // If a protocol's ingress channel is full (e.g. ChainSync server
                    // blocked at tip waiting for announcements while pipelined
                    // MsgRequestNext messages fill the buffer), the blocking send
                    // would stall the ENTIRE ingress task, preventing ALL other
                    // protocols (KeepAlive, BlockFetch) from receiving data.
                    //
                    // This matches Haskell's network-mux demuxer which throws
                    // IngressQueueOverRun (fatal connection error) when a protocol's
                    // queue overflows — the demuxer never blocks.
                    //
                    // MuxChannel::recv() will decrement bytes_in_flight after
                    // consuming the chunk.
                    match route.tx.try_send(Bytes::from(payload)) {
                        Ok(()) => {} // delivered successfully
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            // Channel full — protocol is not consuming fast enough.
                            // Revert counter and return fatal overrun error.
                            route
                                .bytes_in_flight
                                .fetch_sub(payload_len, Ordering::Relaxed);
                            return Err(MuxError::IngressQueueOverrun {
                                protocol_id: header.protocol_id,
                                bytes: new_total,
                                limit: route.limit,
                            });
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            // Receiver dropped (protocol shut down) — undo counter.
                            route
                                .bytes_in_flight
                                .fetch_sub(payload_len, Ordering::Relaxed);
                        }
                    }
                }
                None => {
                    // A-005 (security audit 2026-05-19): terminate on any frame for a
                    // protocol ID not in our subscription table.
                    //
                    // Previous behaviour silently discarded the payload and continued,
                    // allowing an attacker to flood garbage frames on phantom protocol
                    // IDs (e.g. 0xFFFE) with no consequence: each frame consumed NIC
                    // bandwidth, CPU (decode + log), and debug-log I/O at full read speed.
                    //
                    // Haskell `Ouroboros.Network.Mux.Ingress` terminates the bearer on
                    // any unknown protocol ID. We match that behaviour.
                    tracing::warn!(
                        protocol_id = header.protocol_id,
                        direction = ?local_direction,
                        payload_len = header.payload_length,
                        "ingress: received data for unsubscribed protocol — terminating connection"
                    );
                    return Err(MuxError::UnknownProtocol(header.protocol_id));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a raw SDU (header + payload) for testing.
    fn build_sdu(protocol_id: u16, direction: Direction, payload: &[u8]) -> Vec<u8> {
        use crate::mux::segment::encode_header;
        let header = crate::mux::segment::SduHeader {
            timestamp: 0,
            protocol_id,
            direction,
            payload_length: payload.len() as u16,
        };
        let mut buf = encode_header(&header).to_vec();
        buf.extend_from_slice(payload);
        buf
    }

    #[tokio::test]
    async fn dispatches_to_correct_channel() {
        let (tx2, mut rx2) = mpsc::channel(32);
        let (tx3, mut rx3) = mpsc::channel(32);

        let mut routes = HashMap::new();
        // Protocol 2, ResponderDir (after flip from InitiatorDir)
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx: tx2,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );
        // Protocol 3, ResponderDir
        routes.insert(
            (3, Direction::ResponderDir),
            IngressRoute {
                tx: tx3,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // Build raw wire data: protocol 2 message + protocol 3 message + EOF
        let mut wire_data = Vec::new();
        wire_data.extend_from_slice(&build_sdu(2, Direction::InitiatorDir, &[0x82, 0x01, 0x02]));
        wire_data.extend_from_slice(&build_sdu(
            3,
            Direction::InitiatorDir,
            &[0x83, 0x01, 0x02, 0x03],
        ));

        let wire_data = std::sync::Arc::new(std::sync::Mutex::new(wire_data));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let wire_data_clone = wire_data.clone();
        let offset_clone = offset.clone();

        task.run(move |n: usize| {
            let wire_data = wire_data_clone.clone();
            let offset = offset_clone.clone();
            Box::pin(async move {
                let data = wire_data.lock().unwrap();
                let off = offset.load(std::sync::atomic::Ordering::SeqCst);
                if off + n > data.len() {
                    return Err(BearerError::ConnectionReset);
                }
                let result = data[off..off + n].to_vec();
                offset.store(off + n, std::sync::atomic::Ordering::SeqCst);
                Ok(result)
            })
        })
        .await
        .unwrap();

        // Verify protocol 2 received its message
        let chunk2 = rx2.recv().await.unwrap();
        assert_eq!(chunk2.as_ref(), &[0x82, 0x01, 0x02]);

        // Verify protocol 3 received its message
        let chunk3 = rx3.recv().await.unwrap();
        assert_eq!(chunk3.as_ref(), &[0x83, 0x01, 0x02, 0x03]);
    }

    /// A-011 (security audit 2026-05-19): reserved protocol ID 1 must terminate
    /// the connection, not be silently discarded.
    #[tokio::test]
    async fn reserved_protocol_id_terminates_connection() {
        let (tx2, _rx2) = mpsc::channel(32);

        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx: tx2,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // Reserved protocol 1 frame — must cause a fatal error.
        let wire_data = build_sdu(RESERVED_PROTOCOL_ID, Direction::InitiatorDir, &[0xFF, 0xFF]);
        let wire_data = std::sync::Arc::new(std::sync::Mutex::new(wire_data));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let wire_data_clone = wire_data.clone();
        let offset_clone = offset.clone();

        let result = task
            .run(move |n: usize| {
                let wire_data = wire_data_clone.clone();
                let offset = offset_clone.clone();
                Box::pin(async move {
                    let data = wire_data.lock().unwrap();
                    let off = offset.load(std::sync::atomic::Ordering::SeqCst);
                    if off + n > data.len() {
                        return Err(BearerError::ConnectionReset);
                    }
                    let result = data[off..off + n].to_vec();
                    offset.store(off + n, std::sync::atomic::Ordering::SeqCst);
                    Ok(result)
                })
            })
            .await;

        // Reserved protocol ID must produce InvalidHeader error.
        assert!(
            matches!(result, Err(MuxError::InvalidHeader { protocol_id: 1, .. })),
            "reserved protocol ID 1 must terminate with InvalidHeader, got: {result:?}"
        );
    }

    /// Verify that a stalled bearer during header wait blocks indefinitely.
    ///
    /// Matching Haskell's two-phase design: the header read has NO timeout.
    /// A bearer that never sends data blocks forever — liveness is handled
    /// by KeepAlive and OS TCP keepalive, not the mux SDU timeout.
    #[tokio::test(start_paused = true)]
    async fn idle_bearer_blocks_indefinitely_on_header() {
        let routes = HashMap::new();
        let task = IngressTask::new(routes);

        // read_fn that never resolves — simulates an idle connection.
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            task.run(|_n: usize| Box::pin(std::future::pending::<Result<Vec<u8>, BearerError>>())),
        )
        .await;

        // The ingress task should NOT have completed — it blocks forever
        // waiting for the next SDU header (matching Haskell behavior).
        assert!(
            result.is_err(),
            "expected timeout (ingress should block), got: {result:?}"
        );
    }

    /// Verify that ingress gracefully handles EOF on header read (connection closed cleanly).
    #[tokio::test]
    async fn clean_eof_on_header_returns_ok() {
        let routes = HashMap::new();
        let task = IngressTask::new(routes);

        // read_fn immediately signals ConnectionReset (clean EOF)
        let result = task
            .run(|_n: usize| Box::pin(async move { Err(BearerError::ConnectionReset) }))
            .await;

        assert!(
            result.is_ok(),
            "ConnectionReset on header read should be treated as clean EOF: {result:?}"
        );
    }

    /// Verify direction flip: remote InitiatorDir → local ResponderDir routing.
    #[tokio::test]
    async fn direction_flip_routes_to_correct_channel() {
        // Subscribe (5, ResponderDir) — that's what we should see after flipping
        // the remote's (5, InitiatorDir).
        let (tx, mut rx) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (5, Direction::ResponderDir),
            IngressRoute {
                tx,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // Remote sends on protocol 5 with InitiatorDir → wire shows InitiatorDir.
        let wire = std::sync::Arc::new(std::sync::Mutex::new(build_sdu(
            5,
            Direction::InitiatorDir,
            &[0x01],
        )));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        task.run(move |n: usize| {
            let w = wire2.clone();
            let o = off2.clone();
            Box::pin(async move {
                let data = w.lock().unwrap();
                let off = o.load(std::sync::atomic::Ordering::SeqCst);
                if off + n > data.len() {
                    return Err(BearerError::ConnectionReset);
                }
                let r = data[off..off + n].to_vec();
                o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                Ok(r)
            })
        })
        .await
        .unwrap();

        let chunk = rx.recv().await.unwrap();
        assert_eq!(chunk.as_ref(), &[0x01]);
    }

    /// Verify direction flip: remote ResponderDir → local InitiatorDir routing.
    #[tokio::test]
    async fn direction_flip_responder_to_initiator() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (5, Direction::InitiatorDir),
            IngressRoute {
                tx,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        let wire = build_sdu(5, Direction::ResponderDir, &[0x02, 0x03]);
        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        task.run(move |n: usize| {
            let w = wire2.clone();
            let o = off2.clone();
            Box::pin(async move {
                let data = w.lock().unwrap();
                let off = o.load(std::sync::atomic::Ordering::SeqCst);
                if off + n > data.len() {
                    return Err(BearerError::ConnectionReset);
                }
                let r = data[off..off + n].to_vec();
                o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                Ok(r)
            })
        })
        .await
        .unwrap();

        let chunk = rx.recv().await.unwrap();
        assert_eq!(chunk.as_ref(), &[0x02, 0x03]);
    }

    /// Verify that an unknown protocol ID terminates the connection (A-005).
    ///
    /// Haskell `Ouroboros.Network.Mux.Ingress` terminates the bearer on any
    /// unknown protocol ID. Silently discarding would allow attackers to flood
    /// garbage frames at full read speed with no consequence.
    #[tokio::test]
    async fn unknown_protocol_terminates_connection() {
        // No routes registered. Protocol 99 must return UnknownProtocol error.
        let routes = HashMap::new();
        let task = IngressTask::new(routes);

        let wire = build_sdu(99, Direction::InitiatorDir, &[0xDE, 0xAD]);
        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        let result = task
            .run(move |n: usize| {
                let w = wire2.clone();
                let o = off2.clone();
                Box::pin(async move {
                    let data = w.lock().unwrap();
                    let off = o.load(std::sync::atomic::Ordering::SeqCst);
                    if off + n > data.len() {
                        return Err(BearerError::ConnectionReset);
                    }
                    let r = data[off..off + n].to_vec();
                    o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                    Ok(r)
                })
            })
            .await;

        // A-005: unknown protocol must be a fatal error (UnknownProtocol).
        assert!(
            matches!(result, Err(MuxError::UnknownProtocol(99))),
            "unknown protocol must terminate with UnknownProtocol(99), got: {result:?}"
        );
    }

    /// Verify that an overrun (bytes exceed channel limit) returns IngressQueueOverrun.
    #[tokio::test]
    async fn ingress_queue_overrun_returned_when_limit_exceeded() {
        let (tx, _rx) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx,
                limit: 2, // very small limit: 2 bytes
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // Send a 10-byte payload — exceeds the 2-byte limit.
        let wire = build_sdu(2, Direction::InitiatorDir, &[0u8; 10]);
        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        let result = task
            .run(move |n: usize| {
                let w = wire2.clone();
                let o = off2.clone();
                Box::pin(async move {
                    let data = w.lock().unwrap();
                    let off = o.load(std::sync::atomic::Ordering::SeqCst);
                    if off + n > data.len() {
                        return Err(BearerError::ConnectionReset);
                    }
                    let r = data[off..off + n].to_vec();
                    o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                    Ok(r)
                })
            })
            .await;

        assert!(
            matches!(
                result,
                Err(MuxError::IngressQueueOverrun { protocol_id: 2, .. })
            ),
            "expected IngressQueueOverrun for protocol 2, got: {result:?}"
        );
    }

    /// A-011: reserved protocol 1 with zero-length payload also terminates the connection.
    /// The payload length being zero doesn't change the protocol violation.
    #[tokio::test]
    async fn reserved_protocol_id_zero_length_also_terminates() {
        let (tx2, _rx2) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx: tx2,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // Zero-length payload for reserved protocol 1.
        let wire = build_sdu(RESERVED_PROTOCOL_ID, Direction::InitiatorDir, &[]);
        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        let result = task
            .run(move |n: usize| {
                let w = wire2.clone();
                let o = off2.clone();
                Box::pin(async move {
                    let data = w.lock().unwrap();
                    let off = o.load(std::sync::atomic::Ordering::SeqCst);
                    if off + n > data.len() {
                        return Err(BearerError::ConnectionReset);
                    }
                    let r = data[off..off + n].to_vec();
                    o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                    Ok(r)
                })
            })
            .await;

        // Even zero-length reserved-ID frame must produce InvalidHeader.
        assert!(
            matches!(result, Err(MuxError::InvalidHeader { protocol_id: 1, .. })),
            "zero-length reserved-ID frame must terminate, got: {result:?}"
        );
    }

    /// Verify bytes_in_flight counter is correctly incremented for delivered payloads.
    #[tokio::test]
    async fn bytes_in_flight_counter_incremented_on_delivery() {
        let (tx, mut rx) = mpsc::channel(32);
        let bytes_in_flight = Arc::new(AtomicUsize::new(0));
        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx,
                limit: 65536,
                bytes_in_flight: bytes_in_flight.clone(),
            },
        );

        let task = IngressTask::new(routes);

        let payload = vec![0x01, 0x02, 0x03, 0x04, 0x05]; // 5 bytes
        let wire = build_sdu(2, Direction::InitiatorDir, &payload);
        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        task.run(move |n: usize| {
            let w = wire2.clone();
            let o = off2.clone();
            Box::pin(async move {
                let data = w.lock().unwrap();
                let off = o.load(std::sync::atomic::Ordering::SeqCst);
                if off + n > data.len() {
                    return Err(BearerError::ConnectionReset);
                }
                let r = data[off..off + n].to_vec();
                o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                Ok(r)
            })
        })
        .await
        .unwrap();

        // bytes_in_flight should reflect the 5 bytes queued in the channel
        // (not yet consumed by the receiver).
        assert_eq!(
            bytes_in_flight.load(std::sync::atomic::Ordering::Relaxed),
            5,
            "bytes_in_flight should equal the payload size after delivery"
        );

        // Consuming the chunk decrements bytes_in_flight via MuxChannel::recv(),
        // but here we drive it manually via the raw mpsc receiver — so counter
        // stays at 5 until a MuxChannel consumer calls fetch_sub.
        let chunk = rx.recv().await.unwrap();
        assert_eq!(chunk.len(), 5);
    }

    /// Verify that multiple sequential SDUs for the same protocol are all delivered
    /// in order.
    #[tokio::test]
    async fn multiple_sdus_same_protocol_delivered_in_order() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        let mut wire = Vec::new();
        for i in 0u8..5 {
            wire.extend_from_slice(&build_sdu(2, Direction::InitiatorDir, &[i]));
        }

        let wire = std::sync::Arc::new(std::sync::Mutex::new(wire));
        let offset = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire2 = wire.clone();
        let off2 = offset.clone();
        task.run(move |n: usize| {
            let w = wire2.clone();
            let o = off2.clone();
            Box::pin(async move {
                let data = w.lock().unwrap();
                let off = o.load(std::sync::atomic::Ordering::SeqCst);
                if off + n > data.len() {
                    return Err(BearerError::ConnectionReset);
                }
                let r = data[off..off + n].to_vec();
                o.store(off + n, std::sync::atomic::Ordering::SeqCst);
                Ok(r)
            })
        })
        .await
        .unwrap();

        for i in 0u8..5 {
            let chunk = rx.recv().await.unwrap();
            assert_eq!(chunk.as_ref(), &[i], "SDU {i} delivered out of order");
        }
    }

    /// Verify that a stalled payload (header arrived, payload stalled)
    /// triggers SduReadTimeout.
    #[tokio::test(start_paused = true)]
    async fn sdu_read_timeout_on_stalled_payload() {
        use std::sync::atomic::AtomicUsize;

        let (tx2, _rx2) = mpsc::channel(32);
        let mut routes = HashMap::new();
        routes.insert(
            (2, Direction::ResponderDir),
            IngressRoute {
                tx: tx2,
                limit: 65536,
                bytes_in_flight: Arc::new(AtomicUsize::new(0)),
            },
        );

        let task = IngressTask::new(routes);

        // First call returns a valid header (8 bytes), second call (payload) never resolves.
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result = task
            .run(move |n: usize| {
                let count = call_count_clone.clone();
                Box::pin(async move {
                    let c = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if c == 0 {
                        // First call: return a valid SDU header for protocol 2,
                        // InitiatorDir, with payload_length=100
                        assert_eq!(n, 8);
                        let header = crate::mux::segment::SduHeader {
                            timestamp: 0,
                            protocol_id: 2,
                            direction: Direction::InitiatorDir,
                            payload_length: 100,
                        };
                        Ok(crate::mux::segment::encode_header(&header).to_vec())
                    } else {
                        // Second call (payload): never resolves — stalled transfer
                        std::future::pending::<Result<Vec<u8>, BearerError>>().await
                    }
                })
            })
            .await;

        // Should timeout on the payload read after SDU_READ_TIMEOUT (30s).
        assert!(
            matches!(result, Err(MuxError::SduReadTimeout)),
            "expected SduReadTimeout on stalled payload, got: {result:?}"
        );
    }
}
