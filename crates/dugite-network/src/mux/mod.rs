//! Ouroboros multiplexer.
//!
//! Multiplexes multiple mini-protocols over a single bearer (TCP or Unix socket).
//! Matches the Haskell `network-mux` architecture with SDU framing, direction bits,
//! and per-protocol channels.
//!
//! ## Architecture
//! - [`segment`] — SDU header encoding/decoding (8-byte wire format)
//! - [`channel`] — Per-protocol MuxChannel with CBOR boundary detection
//! - [`egress`] — Outbound message segmentation with round-robin fairness
//! - [`ingress`] — Inbound SDU demuxing with direction bit flipping
//!
//! ## Usage
//! ```ignore
//! let mut mux = Mux::new(bearer, true /* is_initiator */);
//! let chainsync_ch = mux.subscribe(PROTOCOL_N2N_CHAINSYNC, Direction::InitiatorDir, 65536);
//! let blockfetch_ch = mux.subscribe(PROTOCOL_N2N_BLOCKFETCH, Direction::InitiatorDir, 65536);
//! mux.run().await?;
//! ```

pub mod channel;
pub mod egress;
pub mod ingress;
pub mod segment;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::bearer::Bearer;
use crate::error::MuxError;
use crate::mux::ingress::SwappableSender;

pub use channel::MuxChannel;
pub use segment::{Direction, SduHeader, HEADER_SIZE};

/// Re-subscription handle for Hot→Warm demotion without closing TCP.
///
/// Mirrors Haskell's `deactivatePeerConnection` in
/// `PeerStateActions.hs#L978`:  when hot mini-protocol tasks exit, the mux
/// bearer stays alive.  `MuxHandle` provides `resubscribe()` to install
/// fresh ingress receivers atomically via the running `IngressTask`'s
/// `SwappableSender`s — no TCP teardown required.
///
/// Obtained via [`Mux::take_handle()`] BEFORE calling [`Mux::run()`].
pub struct MuxHandle {
    /// Per-(protocol_id, direction) swappable sender + ingress limit +
    /// shared `bytes_in_flight` counter.
    senders: HashMap<(u16, Direction), (SwappableSender, usize, Arc<AtomicUsize>)>,
    /// Egress sender — cloned into fresh `MuxChannel` returned by `resubscribe()`.
    egress_tx: mpsc::Sender<(u16, Direction, Bytes)>,
}

impl MuxHandle {
    /// Create an empty handle that always returns `None` from `resubscribe()`.
    ///
    /// Used in unit tests and fake `PeerConnection` instances where there is
    /// no real mux backing the channels.
    pub fn empty() -> Self {
        let (egress_tx, _) = mpsc::channel(1);
        Self {
            senders: HashMap::new(),
            egress_tx,
        }
    }

    /// Atomically install a fresh ingress receiver for the given
    /// `(protocol_id, direction)` and return a new [`MuxChannel`].
    ///
    /// The running `IngressTask` will start delivering frames to the new
    /// receiver immediately after the swap.  The `bytes_in_flight` counter
    /// is reset to zero so the new receiver begins with a clean budget.
    ///
    /// Returns `None` if the protocol was not registered (e.g., this is an
    /// inbound-only connection that didn't subscribe initiator-side hot
    /// channels).
    pub fn resubscribe(&self, protocol_id: u16, direction: Direction) -> Option<MuxChannel> {
        let (swappable_tx, ingress_limit, bytes_in_flight) =
            self.senders.get(&(protocol_id, direction))?;

        // Create a fresh mpsc pair for the new task cycle.
        let (new_tx, new_rx) = mpsc::channel::<Bytes>(ingress::INGRESS_CHANNEL_CAPACITY);

        // Swap the sender AND reset the byte counter under a SINGLE hold of the
        // sender lock. The IngressTask performs its `fetch_add` + `try_send`
        // critical section under the same lock, so this pair is mutually
        // exclusive with delivery: an in-flight frame is either fully accounted
        // against the OLD receiver (its bytes discarded when the old MuxChannel
        // drops, counter then reset to 0) or fully accounted against the NEW
        // receiver (delivered after the swap, so its consumer `fetch_sub`
        // balances the `fetch_add`). Neither ordering can leave a queued chunk
        // with a zeroed counter, which was the #877 underflow window. The lock
        // is held only for an assignment + an atomic store — a few nanoseconds.
        {
            let mut guard = swappable_tx.lock();
            *guard = new_tx;
            // SeqCst so the IngressTask observes the reset atomically with the
            // sender swap.
            bytes_in_flight.store(0, std::sync::atomic::Ordering::SeqCst);
        }

        Some(MuxChannel::new(
            protocol_id,
            direction,
            self.egress_tx.clone(),
            new_rx,
            *ingress_limit,
            Arc::clone(bytes_in_flight),
        ))
    }
}

/// Ouroboros multiplexer. Owns the bearer and coordinates ingress/egress tasks.
///
/// Subscribe protocol channels before calling `run()`. Once running, the mux
/// spawns ingress and egress tasks and blocks until the bearer is closed or
/// an error occurs.
pub struct Mux<B: Bearer> {
    /// The underlying transport (TCP or Unix socket).
    bearer: Option<B>,
    /// Whether we initiated the TCP connection (determines direction semantics).
    is_initiator: bool,
    /// Shared egress sender — cloned into each MuxChannel.
    egress_tx: mpsc::Sender<(u16, Direction, Bytes)>,
    /// Egress receiver — consumed by the egress task.
    egress_rx: Option<mpsc::Receiver<(u16, Direction, Bytes)>>,
    /// Per-protocol ingress senders — consumed by the ingress task.
    ingress_routes: HashMap<(u16, Direction), ingress::IngressRoute>,
    /// Re-subscription handle entries: populated by `subscribe()`, drained by `take_handle()`.
    ///
    /// Each entry mirrors the `IngressRoute`'s `SwappableSender` so that
    /// `MuxHandle::resubscribe()` can atomically swap the sender after hot
    /// protocol tasks exit.
    handle_senders: HashMap<(u16, Direction), (SwappableSender, usize, Arc<AtomicUsize>)>,
}

impl<B: Bearer> Mux<B> {
    /// Create a new multiplexer over the given bearer.
    ///
    /// `is_initiator` is `true` if we initiated the TCP connection (client side),
    /// `false` if we accepted it (server side). This determines the direction
    /// semantics for the SDU direction bit.
    pub fn new(bearer: B, is_initiator: bool) -> Self {
        let (egress_tx, egress_rx) = mpsc::channel(channel::EGRESS_CHANNEL_CAPACITY * 8);
        Self {
            bearer: Some(bearer),
            is_initiator,
            egress_tx,
            egress_rx: Some(egress_rx),
            ingress_routes: HashMap::new(),
            handle_senders: HashMap::new(),
        }
    }

    /// Take the re-subscription handle for use by the lifecycle manager.
    ///
    /// **Must be called BEFORE `run()`** — `run()` consumes `self` and moves
    /// `ingress_routes` into `IngressTask`, making it inaccessible afterwards.
    ///
    /// The returned [`MuxHandle`] holds `Arc` references to the same
    /// `SwappableSender`s that `IngressTask` will use.  After `take_handle()`
    /// is called, the `handle_senders` map inside `Mux` is empty (drained).
    pub fn take_handle(&mut self) -> MuxHandle {
        MuxHandle {
            senders: std::mem::take(&mut self.handle_senders),
            egress_tx: self.egress_tx.clone(),
        }
    }

    /// Subscribe a protocol channel on this multiplexer.
    ///
    /// Must be called before `run()`. Returns a [`MuxChannel`] that the protocol
    /// implementation uses to send and receive CBOR messages.
    ///
    /// - `protocol_id`: Ouroboros protocol number (e.g., 2 for ChainSync)
    /// - `direction`: InitiatorDir or ResponderDir
    /// - `ingress_limit`: maximum bytes buffered for this channel before overrun
    pub fn subscribe(
        &mut self,
        protocol_id: u16,
        direction: Direction,
        ingress_limit: usize,
    ) -> MuxChannel {
        let (ingress_tx, ingress_rx) = mpsc::channel::<Bytes>(ingress::INGRESS_CHANNEL_CAPACITY);

        // Wrap the sender in a SwappableSender so MuxHandle::resubscribe() can
        // atomically install a fresh receiver after hot protocol tasks exit.
        let swappable_tx: SwappableSender = Arc::new(Mutex::new(ingress_tx));

        // Shared byte-in-flight counter between the IngressRoute (producer side)
        // and the MuxChannel (consumer side). IngressTask increments it when
        // enqueuing a chunk; MuxChannel::recv() decrements it when consuming.
        let bytes_in_flight = Arc::new(AtomicUsize::new(0));

        // Register the ingress route for the ingress task to dispatch to.
        // The direction here is the LOCAL direction — what we'll see after
        // the ingress task flips the remote's direction bit.
        self.ingress_routes.insert(
            (protocol_id, direction),
            ingress::IngressRoute {
                tx: Arc::clone(&swappable_tx),
                limit: ingress_limit,
                bytes_in_flight: Arc::clone(&bytes_in_flight),
            },
        );

        // Store a clone of the swappable sender + metadata in handle_senders
        // so take_handle() can hand them to MuxHandle::resubscribe().
        self.handle_senders.insert(
            (protocol_id, direction),
            (swappable_tx, ingress_limit, Arc::clone(&bytes_in_flight)),
        );

        MuxChannel::new(
            protocol_id,
            direction,
            self.egress_tx.clone(),
            ingress_rx,
            ingress_limit,
            bytes_in_flight,
        )
    }

    /// Run the multiplexer. Spawns ingress and egress tasks and blocks until
    /// the bearer is closed or an error occurs.
    ///
    /// Returns `Ok(())` on clean shutdown, `Err(MuxError)` on failure.
    pub async fn run(mut self) -> Result<(), MuxError> {
        let bearer = self.bearer.take().expect("bearer already consumed");
        let egress_rx = self.egress_rx.take().expect("egress_rx already consumed");
        let routes = std::mem::take(&mut self.ingress_routes);

        let sdu_size = bearer.sdu_size();
        let batch_size = bearer.batch_size();

        // Split the bearer into independent read and write halves using
        // separate tokio tasks with NO shared lock. This is CRITICAL:
        //
        // The Ouroboros mux requires full-duplex I/O — the initiator must send
        // MsgProposeVersions while simultaneously being ready to receive data.
        // A shared mutex deadlocks because the ingress task holds the lock
        // during read_exact() (waiting for peer data), preventing the egress
        // task from writing the handshake that the peer is waiting for.
        //
        // Solution: use channels to decouple the tasks from direct bearer access.
        // A dedicated reader task owns the read half, a dedicated writer task
        // owns the write half. Both run independently without contention.

        // CRITICAL: True split I/O for full-duplex Ouroboros mux.
        //
        // The Ouroboros protocol requires concurrent reads and writes on the
        // same TCP connection. The initiator must send MsgProposeVersions while
        // simultaneously being ready to receive data from the peer.
        //
        // We split the bearer into independent read and write halves using
        // Bearer::split(). Each half is given exclusively to its own task,
        // eliminating all contention. This matches the Haskell network-mux
        // architecture where ingress and egress run as independent threads.

        let (mut reader, mut writer) = bearer.split();

        // Spawn a dedicated WRITER task — owns the write half exclusively
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let writer_handle = tokio::spawn(async move {
            while let Some(data) = write_rx.recv().await {
                tracing::debug!(
                    bytes = data.len(),
                    hex = %hex::encode(&data[..data.len().min(32)]),
                    "mux: writing to bearer"
                );
                if let Err(e) = writer.write_all(&data).await {
                    tracing::debug!("mux: bearer write error: {e}");
                    return;
                }
                if let Err(e) = writer.flush().await {
                    tracing::debug!("mux: bearer flush error: {e}");
                    return;
                }
            }
        });

        // Spawn a dedicated READER task — owns the read half exclusively
        let (read_tx, mut read_rx) = tokio::sync::mpsc::channel::<(
            usize,
            tokio::sync::oneshot::Sender<Result<Vec<u8>, crate::error::BearerError>>,
        )>(64);
        let reader_handle = tokio::spawn(async move {
            while let Some((n, reply)) = read_rx.recv().await {
                let mut buf = vec![0u8; n];
                match reader.read_exact(&mut buf).await {
                    Ok(()) => {
                        tracing::trace!(
                            bytes = n,
                            hex = %hex::encode(&buf[..buf.len().min(16)]),
                            "mux: read from bearer"
                        );
                        let _ = reply.send(Ok(buf));
                    }
                    Err(e) => {
                        tracing::debug!("mux: bearer read error: {e}");
                        let _ = reply.send(Err(e));
                        return;
                    }
                }
            }
            tracing::debug!("mux: reader task exiting (read_rx channel closed)");
        });

        // Egress task: sends SDU frames via the write channel
        let egress_task = egress::EgressTask::new(egress_rx, sdu_size, batch_size);
        let egress_handle = tokio::spawn(async move {
            egress_task
                .run(move |data: &[u8]| {
                    let tx = write_tx.clone();
                    let data = data.to_vec();
                    Box::pin(async move {
                        tx.send(data)
                            .await
                            .map_err(|_| crate::error::BearerError::ConnectionReset)?;
                        Ok(())
                    })
                })
                .await
        });

        // Ingress task: reads SDU frames via the read channel
        let ingress_task = ingress::IngressTask::new(routes);
        let ingress_handle = tokio::spawn(async move {
            ingress_task
                .run(move |n: usize| {
                    let tx = read_tx.clone();
                    Box::pin(async move {
                        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                        tx.send((n, reply_tx))
                            .await
                            .map_err(|_| crate::error::BearerError::ConnectionReset)?;
                        reply_rx
                            .await
                            .map_err(|_| crate::error::BearerError::ConnectionReset)?
                    })
                })
                .await
        });

        // Abort-on-drop guard for ALL four child tasks (#866).
        //
        // A bare `JoinHandle` DETACHES its task on drop — it does not cancel it.
        // The reader/writer tasks own the two socket halves and the reader blocks
        // indefinitely in `read_exact` (Phase-1 header reads have no timeout), so
        // detaching them leaks the FDs and the tasks forever: under peer churn
        // (Hot→Warm→Cold demotions, rotation) every mux teardown leaked a socket
        // → EMFILE. `run()` is itself a spawned task; when the lifecycle manager
        // calls `mux_handle.abort()`, this future is dropped at its current await
        // point and its locals — including this guard — are dropped, which
        // `.abort()`s every child. Dropping the reader/writer tasks drops the
        // bearer halves, closing the TCP/Unix socket. When `run()` returns
        // normally the same cleanup happens.
        struct AbortOnDrop(Vec<tokio::task::AbortHandle>);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                for h in &self.0 {
                    h.abort();
                }
            }
        }
        let mut writer_handle = writer_handle;
        let mut reader_handle = reader_handle;
        let mut egress_handle = egress_handle;
        let mut ingress_handle = ingress_handle;
        let _abort_guard = AbortOnDrop(vec![
            writer_handle.abort_handle(),
            reader_handle.abort_handle(),
            egress_handle.abort_handle(),
            ingress_handle.abort_handle(),
        ]);

        // Wait for any task to complete. Whichever finishes first (clean EOF,
        // error, or bearer close) tears the connection down; the guard aborts
        // the survivors — including the reader blocked on `read_exact` — so no
        // task and no socket FD is leaked.
        let result = tokio::select! {
            result = &mut egress_handle => {
                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(_join_err) => Err(MuxError::ChannelClosed),
                }
            }
            result = &mut ingress_handle => {
                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(_join_err) => Err(MuxError::ChannelClosed),
                }
            }
            // The bearer I/O halves live in these tasks; if either exits (socket
            // closed, write error) the mux is done — return and let the guard
            // abort the rest.
            _ = &mut reader_handle => Ok(()),
            _ = &mut writer_handle => Ok(()),
        };
        result
    }

    /// Whether we are the TCP connection initiator.
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bearer::tcp::TcpBearer;
    use crate::bearer::MockBearer;
    use crate::mux::segment::encode_header;
    use crate::protocol::keepalive::client::KeepAliveClient;
    use crate::protocol::keepalive::server::KeepAliveServer;
    use crate::protocol::PROTOCOL_N2N_KEEPALIVE;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// Build a mock bearer pre-loaded with raw SDU data for ingress testing.
    fn build_mock_bearer_with_sdus(sdus: Vec<(u16, Direction, Vec<u8>)>) -> MockBearer {
        let mut wire_data = Vec::new();
        for (pid, dir, payload) in sdus {
            let header = segment::SduHeader {
                timestamp: 0,
                protocol_id: pid,
                direction: dir,
                payload_length: payload.len() as u16,
            };
            wire_data.extend_from_slice(&encode_header(&header));
            wire_data.extend_from_slice(&payload);
        }
        MockBearer::new(wire_data, 12288, 131072)
    }

    #[tokio::test]
    async fn mux_subscribe_and_send() {
        // Create a mux with a mock bearer (no ingress data — it will EOF immediately)
        let bearer = MockBearer::new(vec![], 12288, 131072);
        let mut mux = Mux::new(bearer, true);

        let ch = mux.subscribe(2, Direction::InitiatorDir, 65536);

        // Send a message through the channel
        ch.send(vec![0x82, 0x01, 0x02]).await.unwrap();

        // Verify the channel properties
        assert_eq!(ch.protocol_id(), 2);
        assert_eq!(ch.direction(), Direction::InitiatorDir);
    }

    #[tokio::test]
    async fn mux_full_lifecycle() {
        // Build a bearer with one SDU for protocol 2 (sent by remote as InitiatorDir)
        let bearer =
            build_mock_bearer_with_sdus(vec![(2, Direction::InitiatorDir, vec![0x82, 0x01, 0x02])]);

        let mut mux = Mux::new(bearer, false); // we are responder

        // Subscribe protocol 2, ResponderDir (after flip from remote's InitiatorDir)
        let mut ch = mux.subscribe(2, Direction::ResponderDir, 65536);

        // Run the mux in the background
        let mux_handle = tokio::spawn(async move { mux.run().await });

        // Receive the message from the channel
        let msg = ch.recv().await.unwrap();
        assert_eq!(msg, vec![0x82, 0x01, 0x02]);

        // The mux should complete after EOF on the bearer
        let result = mux_handle.await.unwrap();
        // It's OK if this returns an error due to the bearer closing
        let _ = result;
    }

    /// Verify is_initiator() reflects the value passed to Mux::new().
    #[test]
    fn mux_is_initiator_flag() {
        let bearer = MockBearer::new(vec![], 12288, 131072);
        let mux = Mux::new(bearer, true);
        assert!(mux.is_initiator());

        let bearer2 = MockBearer::new(vec![], 12288, 131072);
        let mux2 = Mux::new(bearer2, false);
        assert!(!mux2.is_initiator());
    }

    /// Verify that multiple subscriptions with different (protocol_id, direction) pairs
    /// can coexist without overwriting each other (regression lock for HashMap keying).
    #[test]
    fn subscribe_multiple_protocols_distinct_keys() {
        let bearer = MockBearer::new(vec![], 12288, 131072);
        let mut mux = Mux::new(bearer, true);

        let ch2_init = mux.subscribe(2, Direction::InitiatorDir, 65536);
        let ch2_resp = mux.subscribe(2, Direction::ResponderDir, 65536);
        let ch3_init = mux.subscribe(3, Direction::InitiatorDir, 65536);

        // Verify each channel reports the correct binding.
        assert_eq!(ch2_init.protocol_id(), 2);
        assert_eq!(ch2_init.direction(), Direction::InitiatorDir);
        assert_eq!(ch2_resp.protocol_id(), 2);
        assert_eq!(ch2_resp.direction(), Direction::ResponderDir);
        assert_eq!(ch3_init.protocol_id(), 3);
        assert_eq!(ch3_init.direction(), Direction::InitiatorDir);
    }

    /// Verify that two subscriptions with the same (protocol_id, direction) key cause
    /// the ingress_routes HashMap to hold only the LATEST entry (last-writer-wins).
    ///
    /// This documents current behavior — in production, each unique (pid, dir) pair
    /// must be subscribed at most once, matching the connection architecture where
    /// client=InitiatorDir and server=ResponderDir per protocol.
    #[test]
    fn subscribe_duplicate_key_replaces_previous_route() {
        let bearer = MockBearer::new(vec![], 12288, 131072);
        let mut mux = Mux::new(bearer, true);

        // First subscription for (2, InitiatorDir).
        let _ch1 = mux.subscribe(2, Direction::InitiatorDir, 65536);
        // Second subscription replaces it (same key).
        let _ch2 = mux.subscribe(2, Direction::InitiatorDir, 65536);

        // We only verify this doesn't panic — the HashMap has 1 entry for this key.
        assert_eq!(mux.ingress_routes.len(), 1);
    }

    /// Verify that ingress SDU with a direction other than what was subscribed is ignored
    /// (different key after flip, routes to unknown-protocol path).
    ///
    /// Wire: remote sends protocol 2 as ResponderDir → after flip → InitiatorDir.
    /// We subscribed (2, ResponderDir) — so InitiatorDir won't match.
    /// The data should be silently dropped (no panic, no blocking).
    #[tokio::test]
    async fn ingress_sdu_wrong_direction_silently_dropped() {
        // Subscribe (2, ResponderDir) — listens for remote's InitiatorDir (after flip).
        // Send remote's ResponderDir → after flip → InitiatorDir → no match.
        let bearer = build_mock_bearer_with_sdus(vec![
            (2, Direction::ResponderDir, vec![0x81, 0x01]), // will flip to InitiatorDir
        ]);

        let mut mux = Mux::new(bearer, false);
        // Subscribe (2, ResponderDir) — expects flip from InitiatorDir.
        let mut ch = mux.subscribe(2, Direction::ResponderDir, 65536);

        let mux_handle = tokio::spawn(async move { mux.run().await });

        // The mux should terminate (EOF) without delivering any data to ch.
        let _ = mux_handle.await.unwrap();

        // Channel should be closed / empty — try_recv returns None or BearerClosed.
        let result = ch.try_recv();
        // Either None (nothing delivered) or Err(BearerClosed) is acceptable.
        match result {
            Ok(None) => {}                                  // Nothing in buffer — correct
            Err(crate::error::MuxError::BearerClosed) => {} // Channel dropped — correct
            other => panic!("expected Ok(None) or Err(BearerClosed), got {other:?}"),
        }
    }

    /// Verify that two sequential SDUs for the same protocol are both delivered.
    #[tokio::test]
    async fn two_sequential_sdus_both_delivered() {
        let bearer = build_mock_bearer_with_sdus(vec![
            (2, Direction::InitiatorDir, vec![0x81, 0x01]),
            (2, Direction::InitiatorDir, vec![0x81, 0x02]),
        ]);

        let mut mux = Mux::new(bearer, false); // responder
        let mut ch = mux.subscribe(2, Direction::ResponderDir, 65536);

        tokio::spawn(async move { mux.run().await });

        let msg1 = ch.recv().await.unwrap();
        assert_eq!(msg1, vec![0x81, 0x01]);

        let msg2 = ch.recv().await.unwrap();
        assert_eq!(msg2, vec![0x81, 0x02]);
    }

    /// Regression lock: INGRESS_CHANNEL_CAPACITY must exceed the default pipeline depth (300).
    ///
    /// Verified at compile time: if the constant is lowered below 300,
    /// ChainSync pipelining will stall the ingress task.
    const _: () = assert!(
        ingress::INGRESS_CHANNEL_CAPACITY > 300,
        "INGRESS_CHANNEL_CAPACITY must exceed default pipeline depth 300 \
         to prevent ingress stall under pipelining"
    );

    /// #866: aborting the mux `run()` task must CANCEL its reader/writer child
    /// tasks (not detach them), which drops the bearer halves and closes the
    /// socket. We observe the closure from the peer side: after abort, the
    /// peer's `read` returns EOF (`Ok(0)`). A detached reader would keep the
    /// socket open indefinitely (the FD leak this test locks against).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_run_closes_socket_no_fd_leak() {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        let (mux_stream, mut peer_stream) = tokio::join!(
            async { tokio::net::TcpStream::connect(addr).await.expect("connect") },
            async { listener.accept().await.expect("accept").0 }
        );

        let bearer = TcpBearer::new(mux_stream).expect("bearer");
        let mut mux = Mux::new(bearer, true);
        // Subscribe a channel so the mux has routes; it will idle waiting for
        // SDU headers that never come (peer sends nothing).
        let _ch = mux.subscribe(2, Direction::InitiatorDir, 65536);
        let mux_handle = tokio::spawn(async move { mux.run().await });

        // Let the mux tasks spin up and park on read.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Abort the run() task. The AbortOnDrop guard must cancel reader+writer,
        // dropping the socket halves and closing the connection.
        mux_handle.abort();
        let _ = mux_handle.await;

        // Peer read must observe EOF within a short window if the socket closed.
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), peer_stream.read(&mut buf))
            .await
            .expect("peer read timed out — socket was NOT closed (reader task leaked)")
            .expect("peer read errored");
        assert_eq!(n, 0, "expected EOF after mux abort; socket left open (FD leak)");
    }

    /// Duplex KeepAlive integration test.
    ///
    /// Exercises a real TCP loopback connection with a full Mux instance on each
    /// side, each running both a KeepAlive CLIENT and a KeepAlive SERVER
    /// simultaneously. This is the duplex configuration matching `PeerConnection`
    /// where client and server use different direction slots to avoid collisions.
    ///
    /// ## Direction assignment (no collision)
    ///
    /// Each side subscribes BOTH directions for protocol 8, assigning one to the
    /// client and the other to the server:
    ///
    /// ```text
    /// Side A (TCP initiator, outbound):
    ///   (8, InitiatorDir) → A's CLIENT (sends pings, receives pongs)
    ///   (8, ResponderDir)  → A's SERVER (receives B's pings, sends pongs)
    ///
    /// Side B (TCP responder, inbound):
    ///   (8, ResponderDir)  → B's SERVER (receives A's pings, sends pongs)
    ///   (8, InitiatorDir) → B's CLIENT (sends pings, receives pongs)
    /// ```
    ///
    /// ## Message flow (A→B)
    ///
    /// ```text
    /// A client sends on InitiatorDir → wire=InitiatorDir
    /// B ingress flips: InitiatorDir → ResponderDir → B's (8, ResponderDir) = B's server ✓
    /// B server sends pong on ResponderDir → wire=ResponderDir
    /// A ingress flips: ResponderDir → InitiatorDir → A's (8, InitiatorDir) = A's client ✓
    /// ```
    ///
    /// ## Message flow (B→A)
    ///
    /// ```text
    /// B client sends on InitiatorDir → wire=InitiatorDir
    /// A ingress flips: InitiatorDir → ResponderDir → A's (8, ResponderDir) = A's server ✓
    /// A server sends pong on ResponderDir → wire=ResponderDir
    /// B ingress flips: ResponderDir → InitiatorDir → B's (8, InitiatorDir) = B's client ✓
    /// ```
    ///
    /// No direction collisions: each `(protocol_id, direction)` key is subscribed
    /// exactly once per mux instance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplex_keepalive_both_sides_receive_pongs() {
        // ── 1. Real TCP loopback connection ───────────────────────────────────
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind TCP listener");
        let addr = listener.local_addr().expect("no local addr");

        let (stream_a, stream_b) = tokio::join!(
            async {
                tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("TcpStream::connect failed")
            },
            async {
                let (stream, _peer_addr) = listener.accept().await.expect("accept failed");
                stream
            }
        );

        let bearer_a = TcpBearer::new(stream_a).expect("TcpBearer for side A");
        let bearer_b = TcpBearer::new(stream_b).expect("TcpBearer for side B");

        // ── 2. Mux instances ─────────────────────────────────────────────────
        let mut mux_a = Mux::new(bearer_a, true /* TCP initiator */);
        let mut mux_b = Mux::new(bearer_b, false /* TCP responder */);

        // ── 3. Subscribe protocol 8 (KeepAlive) channels ─────────────────────
        //
        // Correct duplex assignment: client and server use DIFFERENT directions
        // on each side. This matches PeerConnection::connect/accept where:
        //   outbound: client=InitiatorDir, server=ResponderDir
        //   inbound:  server=ResponderDir, client=InitiatorDir
        //
        // No HashMap collisions — each (protocol_id, direction) key is unique.

        // Side A: client on InitiatorDir, server on ResponderDir
        let mut a_client_ch =
            mux_a.subscribe(PROTOCOL_N2N_KEEPALIVE, Direction::InitiatorDir, 65536);
        let mut a_server_ch =
            mux_a.subscribe(PROTOCOL_N2N_KEEPALIVE, Direction::ResponderDir, 65536);

        // Side B: server on ResponderDir, client on InitiatorDir
        let mut b_server_ch =
            mux_b.subscribe(PROTOCOL_N2N_KEEPALIVE, Direction::ResponderDir, 65536);
        let mut b_client_ch =
            mux_b.subscribe(PROTOCOL_N2N_KEEPALIVE, Direction::InitiatorDir, 65536);

        // ── 4. Run both Mux instances ────────────────────────────────────────
        tokio::spawn(async move {
            let _ = mux_a.run().await;
        });
        tokio::spawn(async move {
            let _ = mux_b.run().await;
        });

        // ── 5. RTT reporting channels ────────────────────────────────────────
        let (a_rtt_tx, mut a_rtt_rx) = tokio::sync::mpsc::channel::<f64>(16);
        let (b_rtt_tx, mut b_rtt_rx) = tokio::sync::mpsc::channel::<f64>(16);

        // ── 6. Cancellation tokens ───────────────────────────────────────────
        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();

        // ── 7. Spawn KeepAlive SERVERS ───────────────────────────────────────
        //
        // Servers are started before clients so they are ready to echo pings
        // before the first MsgKeepAlive arrives on the channel.
        tokio::spawn(async move {
            let _ = KeepAliveServer::run(&mut a_server_ch).await;
        });
        tokio::spawn(async move {
            let _ = KeepAliveServer::run(&mut b_server_ch).await;
        });

        // ── 8. Spawn KeepAlive CLIENTS ───────────────────────────────────────
        //
        // 5-second ping interval: the test window is 15 seconds, so each client
        // attempts at least two pings.
        let cancel_a_clone = cancel_a.clone();
        let a_client_handle = tokio::spawn(async move {
            let client = KeepAliveClient::new(Duration::from_secs(5), cancel_a_clone)
                .with_rtt_sender(a_rtt_tx);
            client.run(&mut a_client_ch).await
        });

        let cancel_b_clone = cancel_b.clone();
        let b_client_handle = tokio::spawn(async move {
            let client = KeepAliveClient::new(Duration::from_secs(5), cancel_b_clone)
                .with_rtt_sender(b_rtt_tx);
            client.run(&mut b_client_ch).await
        });

        // ── 9. Wait up to 15 seconds for at least one pong on each side ──────
        let a_pong_result = tokio::time::timeout(Duration::from_secs(15), a_rtt_rx.recv()).await;
        let b_pong_result = tokio::time::timeout(Duration::from_secs(15), b_rtt_rx.recv()).await;

        // Cancel both clients for a graceful shutdown.
        cancel_a.cancel();
        cancel_b.cancel();

        // Allow time for MsgDone to be sent and the client tasks to unwind.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // ── 10. Assert both sides received at least one pong ─────────────────
        assert!(
            a_pong_result.is_ok() && a_pong_result.unwrap().is_some(),
            "side A (TCP initiator) KeepAlive CLIENT received no pong within 15 s"
        );
        assert!(
            b_pong_result.is_ok() && b_pong_result.unwrap().is_some(),
            "side B (TCP acceptor) KeepAlive CLIENT received no pong within 15 s"
        );

        // Confirm that both client tasks exited without a protocol error.
        let a_result = a_client_handle.await.expect("side A client task panicked");
        let b_result = b_client_handle.await.expect("side B client task panicked");

        assert!(
            a_result.is_ok(),
            "side A KeepAlive client exited with error: {:?}",
            a_result.unwrap_err()
        );
        assert!(
            b_result.is_ok(),
            "side B KeepAlive client exited with error: {:?}",
            b_result.unwrap_err()
        );
    }
}
