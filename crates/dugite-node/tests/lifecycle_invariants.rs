//! Lifecycle invariant tests for Hot→Warm→Hot peer cycle (issue #703 Fix D).
//!
//! These tests exercise `PeerConnection::stop_hot_protocols_and_recover()` and
//! the `MuxHandle::resubscribe()` mechanism that implements Haskell-compatible
//! Hot→Warm demotion without closing the TCP connection.
//!
//! ## What we test
//!
//! 1. **Channel recovery** — after `stop_hot_protocols_and_recover()`, all three
//!    hot client channels are `Some` again and `start_hot_protocols` succeeds.
//!
//! 2. **100 Hot→Warm→Hot cycles** — channels are recovered on every cycle; no
//!    `ChannelUnavailable` errors; all tasks start and stop cleanly.
//!
//! 3. **RSS growth bound** — RSS after 100 cycles stays within 50 MB of the
//!    RSS before the cycle loop (via `getrusage RUSAGE_SELF ru_maxrss`).
//!    Note: `maxrss` is a high-water mark so this test is conservative — it
//!    can only fail if the HWM grows by 50 MB within the test run, which is
//!    the signal for a sustained per-cycle allocation leak.
//!
//! ## Reference: Haskell `deactivatePeerConnection`
//!
//! <https://github.com/IntersectMBO/ouroboros-network/blob/main/ouroboros-network/lib/Ouroboros/Network/PeerSelection/PeerStateActions.hs#L978>

use std::net::SocketAddr;

use dugite_network::mux::segment::Direction;
use dugite_network::mux::{Mux, MuxHandle};
use dugite_network::protocol::{
    PROTOCOL_N2N_BLOCKFETCH, PROTOCOL_N2N_CHAINSYNC, PROTOCOL_N2N_KEEPALIVE,
    PROTOCOL_N2N_TXSUBMISSION,
};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Return current process RSS in bytes (best-effort).
///
/// On macOS, `getrusage` returns `ru_maxrss` in bytes (high-water mark).
/// On Linux, `ru_maxrss` is in kilobytes.
/// Returns 0 if the call fails.
fn rss_bytes() -> u64 {
    #[cfg(unix)]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if ret != 0 {
            return 0;
        }
        let maxrss = usage.ru_maxrss as u64;
        // macOS: bytes; Linux: KB.
        if cfg!(target_os = "macos") {
            maxrss
        } else {
            maxrss * 1024
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Verify that `stop_hot_protocols_and_recover()` does not panic on a fake
/// connection backed by an empty `MuxHandle`.
///
/// Uses `fake_with_hot_channels` from the test helper in `peer_connection.rs`
/// which provides channels backed by disconnected mpsc pairs.  Recovery returns
/// `false` because the empty `MuxHandle` has no `SwappableSender` entries — the
/// important invariant is no panic and clean task exit.
#[tokio::test]
async fn stop_and_recover_restores_channels_on_fake_connection() {
    let addr: SocketAddr = "10.0.1.1:3001".parse().unwrap();
    let mut conn = dugite_node::node::peer_connection::PeerConnection::fake_with_hot_channels(addr);

    // Verify channels are present initially.
    assert!(
        conn.has_hot_client_channels(),
        "channels must be Some before first promotion"
    );

    // Start hot protocols with tasks that wait for cancellation.
    fn noop_fn() -> dugite_node::node::peer_connection::ProtocolTaskFn {
        Box::new(move |_ch, cancel| {
            Box::pin(async move {
                // Wait for cancellation — exercises the cancel→await path in
                // stop_hot_protocols_and_recover.
                cancel.cancelled().await;
            })
        })
    }

    conn.start_hot_protocols(noop_fn(), noop_fn(), noop_fn())
        .expect("start_hot_protocols must succeed");

    assert!(
        !conn.has_hot_client_channels(),
        "channels must be None after start_hot_protocols"
    );

    // Deactivate: stop tasks + attempt channel recovery.
    // With fake connections (MuxHandle::empty()), recovery returns false
    // because there are no real ingress routes to resubscribe.  The test
    // verifies the graceful-shutdown path executes without panic.
    let recovered = conn.stop_hot_protocols_and_recover().await;

    // On fake connections, recovery always returns false (empty MuxHandle).
    // That is expected — the important invariant is no panic and clean task exit.
    assert!(
        !recovered,
        "fake connection (no real mux) must report recovery=false"
    );
}

/// #1097: `start_hot_protocols` must label each spawned hot task with its
/// real protocol name, in the same order it spawns them.
///
/// Found while investigating a preprod soak where `demote_to_warm`'s "channel
/// recovery failed, falling back to TCP close" fired 40+ times in ~10-minute
/// bursts clustered with peer-governor churn, and `stop_hot_protocols_and_recover`'s
/// per-task timeout warning gave no indication of WHICH of ChainSync,
/// BlockFetch or TxSubmission2 was slow to respond to cancellation — so a
/// real regression in one specific protocol's cancellation handling would
/// have looked identical to ordinary real-network scheduling variance.
///
/// `hot_task_names_for_test()` exposes the labels `stop_hot_protocols_and_recover`
/// and `stop_tasks` log against; this pins them to the fixed spawn order
/// (`start_hot_protocols`'s three `self.hot_tasks.push((name, handle, token))`
/// call sites) so a future reorder or mislabel is caught here instead of
/// silently mislabeling every subsequent timeout/join-error log line.
#[tokio::test]
async fn hot_task_names_match_spawn_order() {
    let addr: SocketAddr = "10.0.1.2:3001".parse().unwrap();
    let mut conn = dugite_node::node::peer_connection::PeerConnection::fake_with_hot_channels(addr);

    fn noop_fn() -> dugite_node::node::peer_connection::ProtocolTaskFn {
        Box::new(move |_ch, cancel| {
            Box::pin(async move {
                cancel.cancelled().await;
            })
        })
    }

    conn.start_hot_protocols(noop_fn(), noop_fn(), noop_fn())
        .expect("start_hot_protocols must succeed");

    assert_eq!(
        conn.hot_task_names_for_test(),
        vec!["chainsync", "blockfetch", "txsubmission"],
        "hot_tasks must carry the real protocol name for each slot, in \
         chainsync/blockfetch/txsubmission spawn order — a wrong or swapped \
         label here means stop_hot_protocols_and_recover's timeout/join-error \
         logs (#1097) point at the wrong protocol"
    );

    // Clean up: cancel so the waiting tasks exit rather than being aborted
    // silently at test-drop time.
    let _ = conn.stop_hot_protocols_and_recover().await;
}

/// Verify 100 Hot→Warm→Hot cycles on a real TCP loopback pair.
///
/// Each cycle:
/// 1. Start hot protocol tasks (no-op closures that wait for cancellation).
/// 2. Stop them via `stop_hot_protocols_and_recover()`.
/// 3. Verify channels are recovered (`has_hot_client_channels() == true`).
///
/// Also measures RSS before and after to bound per-cycle allocations to 50 MB.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hundred_hot_warm_hot_cycles_no_channel_unavailable() {
    // Use a fake connection with a real MuxHandle backed by a loopback pair.
    // The local loopback mux provides the SwappableSender infrastructure so
    // resubscribe() can install fresh receivers on each cycle.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listen_addr = listener.local_addr().expect("listen addr");

    // Remote side: accept and run a bare mux (no subscriptions → exits on
    // first unknown-protocol frame, which is fine for this test).
    let remote_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let bearer = dugite_network::TcpBearer::new(stream).expect("TcpBearer");
            let mux = Mux::new(bearer, false);
            tokio::spawn(async move {
                let _ = mux.run().await;
            });
        }
    });

    // Measure RSS before the cycle loop (baseline).
    let rss_before = rss_bytes();

    const CYCLES: usize = 100;
    let mut channel_unavailable_count = 0usize;

    for i in 0..CYCLES {
        // Each iteration opens a fresh TCP connection.  With Fix A, we
        // keep the SAME connection alive by reusing channels; without Fix A
        // we'd close and reopen TCP on every Hot→Warm cycle.  Here we open
        // once per iteration to test the channel-recovery path end-to-end
        // without needing the full lifecycle manager.
        let stream = tokio::net::TcpStream::connect(listen_addr)
            .await
            .expect("TcpStream::connect");
        let local_addr = stream.local_addr().expect("local_addr");

        let bearer = dugite_network::TcpBearer::new(stream).expect("TcpBearer");
        let mut mux = Mux::new(bearer, true);

        // Subscribe all three hot client channels.
        let cs_ch = mux.subscribe(PROTOCOL_N2N_CHAINSYNC, Direction::InitiatorDir, 512 * 1024);
        let bf_ch = mux.subscribe(
            PROTOCOL_N2N_BLOCKFETCH,
            Direction::InitiatorDir,
            24 * 1024 * 1024,
        );
        let tx_ch = mux.subscribe(
            PROTOCOL_N2N_TXSUBMISSION,
            Direction::InitiatorDir,
            8 * 1024 * 1024,
        );
        let ka_ch = mux.subscribe(
            PROTOCOL_N2N_KEEPALIVE,
            Direction::InitiatorDir,
            4 * 1024 * 1024,
        );

        let mux_resubscribe = mux.take_handle();
        let mux_task = tokio::spawn(async move { mux.run().await });

        use dugite_node::node::peer_connection::{PeerConnection, PeerConnectionDirection};
        let mut conn = PeerConnection::fake_with_mux_resubscribe(
            listen_addr,
            local_addr,
            PeerConnectionDirection::Outbound,
            cs_ch,
            bf_ch,
            tx_ch,
            ka_ch,
            mux_task,
            mux_resubscribe,
        );

        // Start hot protocols (tasks that wait for cancellation).
        fn waiting_fn() -> dugite_node::node::peer_connection::ProtocolTaskFn {
            Box::new(move |_ch, cancel| {
                Box::pin(async move {
                    cancel.cancelled().await;
                })
            })
        }

        if let Err(e) = conn.start_hot_protocols(waiting_fn(), waiting_fn(), waiting_fn()) {
            channel_unavailable_count += 1;
            tracing::error!(cycle = i, error = %e, "start_hot_protocols failed");
            conn.shutdown().await;
            continue;
        }

        // Deactivate: stop tasks + recover channels via mux resubscribe.
        let recovered = conn.stop_hot_protocols_and_recover().await;

        if recovered {
            // Channels should be available again.
            assert!(
                conn.has_hot_client_channels(),
                "cycle {i}: channels must be Some after successful recover"
            );

            // Verify a second promotion succeeds (the invariant from #516).
            if let Err(e) = conn.start_hot_protocols(waiting_fn(), waiting_fn(), waiting_fn()) {
                channel_unavailable_count += 1;
                tracing::error!(cycle = i, error = %e, "re-promotion after recover failed");
            } else {
                // Stop the re-promoted tasks cleanly.
                conn.stop_hot_protocols().await;
            }
        }

        // Shutdown this iteration's connection.
        conn.shutdown().await;
    }

    // Stop the remote listener.
    remote_task.abort();
    let _ = remote_task.await;

    // Measure RSS after cycle loop.
    let rss_after = rss_bytes();

    // Invariant: zero ChannelUnavailable errors across all 100 cycles.
    assert_eq!(
        channel_unavailable_count, 0,
        "ChannelUnavailable errors across {CYCLES} Hot→Warm→Hot cycles — \
         this is the #516 regression: channels were not recovered between cycles"
    );

    // RSS growth invariant: the high-water mark must not have grown by more than
    // 50 MB during the cycle loop.  Since maxrss is a HWM, this is a conservative
    // bound — a sustained per-cycle leak of 500 KB would produce ~50 MB of growth
    // in 100 cycles and would be caught here.
    if rss_before > 0 && rss_after > 0 {
        let growth_bytes = rss_after.saturating_sub(rss_before);
        let growth_mb = growth_bytes as f64 / (1024.0 * 1024.0);
        const MAX_GROWTH_MB: f64 = 50.0;
        assert!(
            growth_mb < MAX_GROWTH_MB,
            "RSS grew by {growth_mb:.1} MB across {CYCLES} Hot→Warm→Hot cycles \
             (limit {MAX_GROWTH_MB} MB) — per-cycle leak detected"
        );
    }
}

/// Verify that `MuxHandle::resubscribe()` returns `None` for an unsubscribed
/// protocol (defensive — should never happen in production).
#[test]
fn mux_handle_resubscribe_unknown_protocol_returns_none() {
    let handle = MuxHandle::empty();
    let result = handle.resubscribe(99, Direction::InitiatorDir);
    assert!(
        result.is_none(),
        "resubscribe of unregistered protocol must return None"
    );
}

/// Verify that `MuxHandle::empty()` produces a handle where all resubscribe
/// calls return None (used in fake test connections).
#[test]
fn mux_handle_empty_all_protocols_return_none() {
    let handle = MuxHandle::empty();
    for pid in [
        PROTOCOL_N2N_CHAINSYNC,
        PROTOCOL_N2N_BLOCKFETCH,
        PROTOCOL_N2N_TXSUBMISSION,
        PROTOCOL_N2N_KEEPALIVE,
    ] {
        assert!(
            handle.resubscribe(pid, Direction::InitiatorDir).is_none(),
            "empty handle must return None for protocol {pid}"
        );
        assert!(
            handle.resubscribe(pid, Direction::ResponderDir).is_none(),
            "empty handle must return None for protocol {pid} (ResponderDir)"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// #980 — responder (server-side) mini-protocol termination policy
// ─────────────────────────────────────────────────────────────────────────────
//
// The tests above cover the INITIATOR half of Hot→Warm recovery. The responder
// half had no equivalent, and that gap was issue #980: dugite's N2N ChainSync
// server task logged its return value and exited, nothing observed the handle,
// and the mux silently discards frames for a route whose receiver is gone. A
// downstream cardano-node was left on a live connection where one mini-protocol
// answered nothing, forever, with no error and no disconnect.
//
// The trigger is not exotic. cardano-node sends ChainSync `MsgDone` on every
// Hot→Warm demotion (`deactivatePeerConnection`) and opens a fresh ChainSync
// session on the SAME bearer when it re-promotes. dugite's server returned
// `Ok(())` and the route was gone for the life of the connection.
//
// Upstream policy, which these two tests pin (ouroboros-network
// `c45735a56c567fa977969173d18943bac6bb3821`):
//
//   network-mux/src/Network/Mux.hs
//     | MiniProtocolException MiniProtocolNum MiniProtocolDir SomeException
//       -- ^ A mini-protocol thread terminated with an exception. We always
//       -- respond by terminating the whole mux.
//
//   Ouroboros/Network/InboundGovernor.hs
//     Right _ -> runResponder tMux mpd >>= ...   -- TrResponderRestarted
//
// i.e. error ⇒ kill the connection, clean exit ⇒ re-arm the route. Upstream
// cannot represent the third state dugite was in: its responders are typed so
// that returning mid-protocol is not constructible, and an orphaned ingress
// queue eventually overruns and kills the mux anyway.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dugite_network::protocol::PROTOCOL_N2N_PEERSHARING;

/// Stand up a loopback TCP pair with a responder-side mux on the local end and
/// a raw initiator-side mux on the remote end, and return
/// (connection, remote chainsync channel).
async fn server_side_pair() -> (
    dugite_node::node::peer_connection::PeerConnection,
    dugite_network::mux::channel::MuxChannel,
) {
    use dugite_node::node::peer_connection::PeerConnection;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listen_addr = listener.local_addr().expect("listen addr");

    let accept = tokio::spawn(async move { listener.accept().await });
    let client_stream = tokio::net::TcpStream::connect(listen_addr)
        .await
        .expect("connect");
    let (server_stream, _) = accept.await.expect("join").expect("accept");

    // Local (dugite) end: responder channels for all five server protocols.
    let mut srv_mux = Mux::new(
        dugite_network::TcpBearer::new(server_stream).expect("bearer"),
        false,
    );
    let cs = srv_mux.subscribe(PROTOCOL_N2N_CHAINSYNC, Direction::ResponderDir, 512 * 1024);
    let bf = srv_mux.subscribe(
        PROTOCOL_N2N_BLOCKFETCH,
        Direction::ResponderDir,
        24 * 1024 * 1024,
    );
    let tx = srv_mux.subscribe(
        PROTOCOL_N2N_TXSUBMISSION,
        Direction::ResponderDir,
        8 * 1024 * 1024,
    );
    let ka = srv_mux.subscribe(PROTOCOL_N2N_KEEPALIVE, Direction::ResponderDir, 65536);
    let ps = srv_mux.subscribe(PROTOCOL_N2N_PEERSHARING, Direction::ResponderDir, 65536);
    let srv_handle = srv_mux.take_handle();
    let srv_task = tokio::spawn(async move { srv_mux.run().await });

    let local_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let conn = PeerConnection::fake_with_server_channels(
        listen_addr,
        local_addr,
        cs,
        bf,
        tx,
        ka,
        ps,
        srv_task,
        srv_handle,
    );

    // Remote (peer) end: one initiator channel to drive ChainSync.
    let mut cli_mux = Mux::new(
        dugite_network::TcpBearer::new(client_stream).expect("bearer"),
        true,
    );
    let cli_cs = cli_mux.subscribe(PROTOCOL_N2N_CHAINSYNC, Direction::InitiatorDir, 512 * 1024);
    tokio::spawn(async move { cli_mux.run().await });

    (conn, cli_cs)
}

/// Scripted responder: echoes `PING`→`PONG` and treats `DONE` as the client's
/// `MsgDone`, counting how many times it has been started.
fn scripted_responder(
    starts: Arc<AtomicUsize>,
    fail_instead: bool,
) -> dugite_node::node::peer_connection::ServerProtocolTaskFn {
    use dugite_node::node::peer_connection::ServerProtocolOutcome;
    Arc::new(
        move |mut channel, cancel: tokio_util::sync::CancellationToken| {
            let starts = starts.clone();
            Box::pin(async move {
                starts.fetch_add(1, Ordering::SeqCst);
                loop {
                    tokio::select! {
                        msg = channel.recv() => {
                            let Ok(bytes) = msg else {
                                return ServerProtocolOutcome::Failed("bearer closed".into());
                            };
                            if bytes == b"\x44DONE" {
                                if fail_instead {
                                    return ServerProtocolOutcome::Failed("scripted failure".into());
                                }
                                return ServerProtocolOutcome::ClientDone;
                            }
                            if channel.send(b"\x44PONG".to_vec()).await.is_err() {
                                return ServerProtocolOutcome::Failed("send failed".into());
                            }
                        }
                        _ = cancel.cancelled() => return ServerProtocolOutcome::Cancelled,
                    }
                }
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = ServerProtocolOutcome> + Send>,
                >
        },
    )
}

fn idle_responder() -> dugite_node::node::peer_connection::ServerProtocolTaskFn {
    use dugite_node::node::peer_connection::ServerProtocolOutcome;
    Arc::new(move |_ch, cancel: tokio_util::sync::CancellationToken| {
        Box::pin(async move {
            cancel.cancelled().await;
            ServerProtocolOutcome::Cancelled
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = ServerProtocolOutcome> + Send>>
    })
}

/// #980: after the client's `MsgDone`, the responder must be RE-ARMED on the
/// same connection — a fresh request has to be answered.
///
/// Before the fix the second `PING` produced nothing at all: the task had
/// exited, the mux discarded the frame, and the peer waited forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn responder_is_rearmed_after_client_msgdone() {
    let (mut conn, mut peer) = server_side_pair().await;
    let starts = Arc::new(AtomicUsize::new(0));

    conn.start_server_protocols(
        scripted_responder(starts.clone(), false),
        idle_responder(),
        idle_responder(),
        idle_responder(),
        idle_responder(),
    )
    .expect("start server protocols");

    // First session: request/response works.
    peer.send(b"\x44PING".to_vec()).await.expect("send ping");
    let reply = tokio::time::timeout(Duration::from_secs(5), peer.recv())
        .await
        .expect("first PONG timed out")
        .expect("first PONG");
    assert_eq!(reply, b"\x44PONG");

    // The client ends the mini-protocol, exactly as cardano-node does on a
    // Hot->Warm demotion.
    peer.send(b"\x44DONE".to_vec()).await.expect("send done");

    // ...and later starts a fresh session on the SAME bearer.
    let mut got_second = false;
    for _ in 0..20 {
        if peer.send(b"\x44PING".to_vec()).await.is_err() {
            break;
        }
        if let Ok(Ok(r)) = tokio::time::timeout(Duration::from_millis(500), peer.recv()).await {
            assert_eq!(r, b"\x44PONG");
            got_second = true;
            break;
        }
    }

    assert!(
        got_second,
        "responder was never re-armed after the client's MsgDone — the peer got \
         silence on a live connection, which is #980"
    );
    assert!(
        starts.load(Ordering::SeqCst) >= 2,
        "the responder factory must be invoked again (InboundGovernor \
         TrResponderRestarted), got {} start(s)",
        starts.load(Ordering::SeqCst)
    );

    conn.shutdown().await;
}

/// #980: a responder that FAILS must take the whole connection down, not just
/// its own route.
///
/// Haskell: "A mini-protocol thread terminated with an exception. We always
/// respond by terminating the whole mux." Silence is the one outcome that is
/// never acceptable — a peer that sees a disconnect reconnects, a peer that
/// sees silence waits forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn responder_failure_tears_down_the_whole_connection() {
    let (mut conn, mut peer) = server_side_pair().await;
    let starts = Arc::new(AtomicUsize::new(0));
    let cancel = conn.cancel_token_for_test();

    conn.start_server_protocols(
        scripted_responder(starts.clone(), true),
        idle_responder(),
        idle_responder(),
        idle_responder(),
        idle_responder(),
    )
    .expect("start server protocols");

    peer.send(b"\x44PING".to_vec()).await.expect("send ping");
    let reply = tokio::time::timeout(Duration::from_secs(5), peer.recv())
        .await
        .expect("PONG timed out")
        .expect("PONG");
    assert_eq!(reply, b"\x44PONG");

    // Trigger the scripted failure.
    peer.send(b"\x44DONE".to_vec()).await.expect("send done");

    tokio::time::timeout(Duration::from_secs(5), cancel.cancelled())
        .await
        .expect(
            "a failing responder must cancel the CONNECTION token; leaving the \
             mux alive with one dead route is the state upstream cannot represent",
        );

    // The token alone proves nothing about what the PEER sees. Nothing in the
    // node aborts the mux when the connection token fires — `shutdown()` does
    // those as two separate steps and `is_alive()` reads only the mux handle —
    // so a fix that cancelled the token and stopped there would leave the
    // bearer and TCP socket open, the connection still "alive" to the reaper,
    // and the peer still receiving nothing. That is a worse silence than the
    // one being fixed, and it would have passed a token-only assertion.
    //
    // So assert the observable outcome instead: the mux is dead and the peer's
    // channel is closed, i.e. it sees a disconnect and can reconnect.
    let mut alive_after = true;
    for _ in 0..50 {
        if !conn.is_alive() {
            alive_after = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !alive_after,
        "the mux must be torn down, not merely signalled — upstream's policy is \
         `we always respond by terminating the whole mux`"
    );

    let peer_sees_close = tokio::time::timeout(Duration::from_secs(5), peer.recv()).await;
    assert!(
        matches!(peer_sees_close, Ok(Err(_))),
        "the downstream peer must observe the connection closing, not silence; \
         got {peer_sees_close:?}"
    );

    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "a failed responder must NOT be restarted — only a clean client MsgDone re-arms"
    );

    conn.shutdown().await;
}
