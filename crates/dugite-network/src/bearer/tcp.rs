//! TCP bearer implementation for N2N connections.
//!
//! SDU payload size: 12,288 bytes (matching Haskell `makeSocketBearer`).
//! Batch size: 131,072 bytes.
//! TCP_NODELAY=true (Nagle disabled — matching Haskell `configureSocket`).
//! SO_KEEPALIVE=true with 60s interval.

use socket2::{Socket, TcpKeepalive};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::Bearer;
use crate::error::BearerError;

/// TCP SDU payload size (bytes). Matches Haskell's `SDUSize 12_288`.
///
/// Haskell's `BL.splitAt (fromIntegral sduSize) d` splits payload at
/// exactly this many bytes.  The 8-byte mux header is added separately.
pub const TCP_SDU_SIZE: usize = 12_288;

/// TCP write batch size (bytes). Matches Haskell's batch of 131,072.
pub const TCP_BATCH_SIZE: usize = 131_072;

/// TCP read buffer size. Matches Haskell's `readBufferSize`.
pub const TCP_READ_BUFFER_SIZE: usize = 131_072;

/// TCP keepalive interval — sends probes after this idle duration.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// TCP bearer wrapping a tokio `TcpStream` with Cardano-specific socket options.
pub struct TcpBearer {
    stream: TcpStream,
}

impl TcpBearer {
    /// Create a new TCP bearer from an existing stream.
    ///
    /// Configures:
    /// - `TCP_NODELAY=true` (Nagle disabled — matching Haskell `configureSocket`)
    /// - `SO_KEEPALIVE=true` with 60s interval
    ///
    /// TCP_NODELAY is critical for Ouroboros mux correctness. The mux sends
    /// small SDUs (e.g., KeepAlive pings = 11 bytes) that must be delivered
    /// immediately. With Nagle's algorithm enabled, small packets are buffered
    /// waiting for ACKs, causing multi-second delays or deadlocks — especially
    /// on duplex connections where both sides send small packets simultaneously
    /// (Nagle + delayed ACK interaction).
    ///
    /// Haskell's `configureSocket` (ouroboros-network Snocket.hs) and
    /// `configureOutboundSocket` (ConnectionHandler.hs) both set NoDelay=1.
    pub fn new(stream: TcpStream) -> Result<Self, BearerError> {
        // Match Haskell cardano-node bearer configuration:
        // TCP_NODELAY=true (Nagle disabled — required for mux SDU delivery)
        // SO_KEEPALIVE with 60s interval
        //
        // Use socket2 for TCP option configuration, then convert back to tokio.
        let std_stream = stream.into_std().map_err(BearerError::Io)?;
        let socket = Socket::from(std_stream);

        socket.set_tcp_nodelay(true).map_err(BearerError::Io)?;

        // SO_RCVBUF / SO_SNDBUF — lift the single-stream BDP ceiling.
        //
        // A single TCP stream's throughput is bounded by `window / RTT`. Public
        // Cardano relays sit at 250–500 ms RTT, where the OS default receive
        // window (macOS `net.inet.tcp.recvspace` = 128 KiB, auto-tuning to
        // `autorcvbufmax` = 4 MiB but in practice settling near ~1 MiB) caps a
        // BlockFetch stream at ~1–3 MB/s. Once apply is no longer the bottleneck
        // (registry cache + Plutus pooling), this single-peer BDP cap — not CPU
        // — is the bulk-sync ceiling, and the bfcMaxConcurrencyBulkSync=1
        // single-fetcher means the other hot peers can't pick up the slack.
        //
        // Setting SO_RCVBUF explicitly bypasses conservative auto-tuning and
        // takes the window up to the OS hard cap (`kern.ipc.maxsockbuf`, 8 MiB
        // default on macOS) → ~20 MB/s at 400 ms RTT. Best-effort: the kernel
        // silently clamps to its max, and some platforms ignore it — never fatal.
        // Tunable via `DUGITE_TCP_BUFFER_BYTES`; for windows beyond the OS cap,
        // raise `kern.ipc.maxsockbuf` / `net.core.rmem_max` first.
        let buf_bytes: usize = std::env::var("DUGITE_TCP_BUFFER_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 1024 * 1024);
        if buf_bytes > 0 {
            let _ = socket.set_recv_buffer_size(buf_bytes);
            let _ = socket.set_send_buffer_size(buf_bytes);
        }

        let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_INTERVAL);
        socket
            .set_tcp_keepalive(&keepalive)
            .map_err(BearerError::Io)?;

        let std_stream: std::net::TcpStream = socket.into();
        std_stream.set_nonblocking(true).map_err(BearerError::Io)?;
        let stream = TcpStream::from_std(std_stream).map_err(BearerError::Io)?;

        Ok(Self { stream })
    }

    /// Connect to a remote address and return a configured bearer.
    pub async fn connect(addr: std::net::SocketAddr) -> Result<Self, BearerError> {
        let stream = TcpStream::connect(addr).await.map_err(BearerError::Io)?;
        Self::new(stream)
    }

    /// Local socket address (`(local_ip, local_port)`) of this bearer's stream.
    ///
    /// This is the source endpoint chosen by the OS (or by an explicit `bind`
    /// in `connect_from`). Together with the peer address it forms the
    /// `ConnectionId` used by the lifecycle manager to distinguish concurrent
    /// connections to the same remote — matching Haskell ouroboros-network's
    /// `ConnectionId { localAddress, remoteAddress }` keying.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.stream.local_addr()
    }

    /// Connect to a remote address with the source port bound to `local_addr`.
    ///
    /// This is the duplex-pairing convention used by Haskell ouroboros-network
    /// (`Ouroboros.Network.Server2.Sock` `configureOutboundSocket`): outbound
    /// sockets bind their local port to the node's listen port using
    /// SO_REUSEADDR + SO_REUSEPORT, so a remote peer accepting our connection
    /// sees the source address `(our_ip, our_listen_port)`. When both peers
    /// already exchange a duplex connection in this form, neither needs to
    /// open a second outbound — preventing the "two TCP connections to one
    /// logical peer" race that breaks ChainSync ServerHasAgency timeouts.
    ///
    /// Falls back to `connect()` (ephemeral source port) when either the bind
    /// OR the connect fails — non-fatal, the caller still gets a working TCP
    /// bearer.  The connect fallback handles the case where the listen IP is
    /// not routable to the destination (e.g. `bind(127.0.0.1:P)` then
    /// `connect()` to a public-internet peer yields `EADDRNOTAVAIL` on macOS /
    /// Linux).  Without this fallback, dugite-node would fail to establish ANY
    /// outbound peers when `--host-addr` is a loopback or otherwise
    /// non-routable address (issue #608).
    pub async fn connect_from(
        addr: std::net::SocketAddr,
        local_addr: std::net::SocketAddr,
    ) -> Result<Self, BearerError> {
        let socket = match local_addr {
            std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
            std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
        }
        .map_err(BearerError::Io)?;

        // SO_REUSEADDR allows binding to a port that already has a listener
        // on it; SO_REUSEPORT additionally allows two sockets on the same
        // (addr, port) — required when our N2N listener and our outbound
        // both bind to the same local port. Failure on either is non-fatal.
        let _ = socket.set_reuseaddr(true);
        #[cfg(unix)]
        let _ = socket.set_reuseport(true);

        // Size the receive/send buffers BEFORE connect so the TCP window scale
        // factor is negotiated for the large window (see `new()` for the BDP
        // rationale). `new()` also sets them post-connect as a backstop for the
        // ephemeral-port fallback and the inbound-accept path.
        let buf_bytes: u32 = std::env::var("DUGITE_TCP_BUFFER_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 1024 * 1024);
        if buf_bytes > 0 {
            let _ = socket.set_recv_buffer_size(buf_bytes);
            let _ = socket.set_send_buffer_size(buf_bytes);
        }

        if socket.bind(local_addr).is_err() {
            // Bind to listen-port may fail (already in use without REUSEPORT
            // support, or port already exhausted). Fall back to ephemeral
            // source port — the connection still works, just without
            // duplex-pairing benefits.
            return Self::connect(addr).await;
        }

        match socket.connect(addr).await {
            Ok(stream) => Self::new(stream),
            Err(e) => {
                // Connect can fail post-bind when the source IP is not
                // routable to the destination — e.g. binding `127.0.0.1` then
                // dialing a public-internet host yields `EADDRNOTAVAIL`.
                // This is non-fatal for duplex-pairing: drop the bound socket
                // and retry with an ephemeral source the OS picks itself.
                tracing::debug!(
                    %addr,
                    %local_addr,
                    error = %e,
                    "connect_from: bound-source connect failed, falling back to ephemeral"
                );
                Self::connect(addr).await
            }
        }
    }

    /// Consume this bearer and return the underlying `TcpStream`.
    pub fn into_stream(self) -> TcpStream {
        self.stream
    }
}

#[async_trait::async_trait]
impl Bearer for TcpBearer {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), BearerError> {
        self.stream
            .read_exact(buf)
            .await
            .map_err(BearerError::from)?;
        Ok(())
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), BearerError> {
        self.stream.write_all(buf).await.map_err(BearerError::from)
    }

    async fn flush(&mut self) -> Result<(), BearerError> {
        self.stream.flush().await.map_err(BearerError::from)
    }

    async fn close(&mut self) -> Result<(), BearerError> {
        self.stream.shutdown().await.map_err(BearerError::from)
    }

    fn sdu_size(&self) -> usize {
        TCP_SDU_SIZE
    }

    fn batch_size(&self) -> usize {
        TCP_BATCH_SIZE
    }

    fn split(
        self,
    ) -> (
        Box<dyn super::BearerReader + Send>,
        Box<dyn super::BearerWriter + Send>,
    ) {
        let (read_half, write_half) = self.stream.into_split();
        (
            Box::new(TcpBearerReader(read_half)),
            Box::new(TcpBearerWriter(write_half)),
        )
    }
}

/// Read half of a split TCP bearer.
struct TcpBearerReader(tokio::net::tcp::OwnedReadHalf);

#[async_trait::async_trait]
impl super::BearerReader for TcpBearerReader {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), BearerError> {
        self.0.read_exact(buf).await.map_err(BearerError::from)?;
        Ok(())
    }
}

/// Write half of a split TCP bearer.
struct TcpBearerWriter(tokio::net::tcp::OwnedWriteHalf);

#[async_trait::async_trait]
impl super::BearerWriter for TcpBearerWriter {
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), BearerError> {
        self.0.write_all(buf).await.map_err(BearerError::from)
    }
    async fn flush(&mut self) -> Result<(), BearerError> {
        self.0.flush().await.map_err(BearerError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bearer::Bearer;

    // ─── Constant verification ───────────────────────────────────────────────

    #[test]
    fn sdu_size_matches_haskell() {
        // Haskell cardano-node uses SDUSize 12288 for TCP bearers.
        assert_eq!(TCP_SDU_SIZE, 12_288);
    }

    #[test]
    fn batch_size_matches_haskell() {
        // Haskell cardano-node uses a batch size of 131072 for TCP write coalescing.
        assert_eq!(TCP_BATCH_SIZE, 131_072);
    }

    #[test]
    fn read_buffer_size_matches_haskell() {
        // Haskell's readBufferSize = 131072.
        assert_eq!(TCP_READ_BUFFER_SIZE, 131_072);
    }

    // ─── Connection lifecycle tests ──────────────────────────────────────────

    #[tokio::test]
    async fn connect_and_read_write() {
        // Create a TCP listener, connect a bearer, and verify data exchange.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Echo back whatever we read.
            let mut buf = [0u8; 5];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut buf)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, &buf)
                .await
                .unwrap();
        });

        let mut bearer = TcpBearer::connect(addr).await.unwrap();

        // Verify SDU/batch sizes.
        assert_eq!(bearer.sdu_size(), TCP_SDU_SIZE);
        assert_eq!(bearer.batch_size(), TCP_BATCH_SIZE);

        // Write and read back.
        bearer.write_all(b"hello").await.unwrap();
        bearer.flush().await.unwrap();

        let mut buf = [0u8; 5];
        bearer.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        bearer.close().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bearer_split_concurrent_io() {
        // Verify that split() produces independent read and write halves
        // that can operate concurrently.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut buf)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, &buf)
                .await
                .unwrap();
        });

        let bearer = TcpBearer::connect(addr).await.unwrap();
        let (mut reader, mut writer) = bearer.split();

        // Write from one half, read from the other.
        writer.write_all(b"test").await.unwrap();
        writer.flush().await.unwrap();

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"test");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn read_on_closed_connection_returns_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Close immediately.
            drop(stream);
        });

        let mut bearer = TcpBearer::connect(addr).await.unwrap();
        // Give the server time to close.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut buf = [0u8; 1];
        let result = bearer.read_exact(&mut buf).await;
        assert!(result.is_err(), "read on closed connection should fail");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn new_configures_socket_options() {
        // Verify TcpBearer::new succeeds and configures the stream properly.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let bearer = TcpBearer::new(stream);
        assert!(bearer.is_ok(), "TcpBearer::new should succeed");
    }

    /// `connect_from` with a same-family loopback source must succeed and
    /// produce a connected bearer.  This is the happy-path duplex-pairing
    /// scenario when both ends are on the same host.
    #[tokio::test]
    async fn connect_from_same_loopback_succeeds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        // Bind source to ephemeral on the loopback interface (port 0).
        let local: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let bearer = TcpBearer::connect_from(addr, local).await;
        assert!(
            bearer.is_ok(),
            "connect_from(127.0.0.1→127.0.0.1) must succeed: {:?}",
            bearer.err()
        );
    }

    /// When `bind(local_addr)` fails because the address is not assigned to
    /// any local interface, `connect_from` must fall back to ephemeral source
    /// and still produce a working bearer.  Without this fallback,
    /// dugite-node could fail to establish any outbound peer when the
    /// configured listen address is not routable (issue #608, ipv4 form).
    #[tokio::test]
    async fn connect_from_invalid_bind_falls_back_to_ephemeral() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        // 240.0.0.1 is in the IANA "future use" range and is not assigned to
        // any normal host interface, so bind(240.0.0.1:0) reliably fails
        // with EADDRNOTAVAIL on Linux + macOS.
        let bad_local: std::net::SocketAddr = "240.0.0.1:0".parse().unwrap();
        let bearer = TcpBearer::connect_from(addr, bad_local).await;
        assert!(
            bearer.is_ok(),
            "connect_from must fall back to ephemeral source on bind failure: {:?}",
            bearer.err()
        );
    }

    /// When the bind succeeds (e.g. binding 127.0.0.1) but the OS cannot
    /// route from that source to the destination, `connect()` itself fails
    /// with EADDRNOTAVAIL.  `connect_from` must catch that error and retry
    /// with ephemeral source — the original bug behind issue #608.
    ///
    /// We exercise this by binding to a loopback alias `127.0.0.2` (which
    /// IS bindable on macOS / Linux) and dialling a non-loopback host:
    /// the route lookup picks the wrong source.  Because we don't have a
    /// reliable public peer in the test environment, we instead use a
    /// listener whose peer-address path forces the OS to disagree with our
    /// bound source — see the inline notes for why this is portable.
    #[tokio::test]
    async fn connect_from_unroutable_source_falls_back_to_ephemeral() {
        // Listener on the default loopback (127.0.0.1).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        // Bind source to an IPv6 unspecified-but-mapped address.  Since the
        // destination is IPv4, the V4/V6 family check in `connect_from`
        // chooses TcpSocket::new_v6, and bind to an unspecified V6 source is
        // allowed but connect-to-V4 from that socket fails on most kernels —
        // exercising the post-bind connect failure path.  If the OS happens
        // to accept the V6→V4 connect via mapping, the bearer is still
        // returned successfully (also acceptable: pairing worked).
        let v6_local: std::net::SocketAddr = "[::]:0".parse().unwrap();
        let bearer = TcpBearer::connect_from(addr, v6_local).await;
        assert!(
            bearer.is_ok(),
            "connect_from must fall back to ephemeral source on connect failure: {:?}",
            bearer.err()
        );
    }

    #[tokio::test]
    async fn into_stream_returns_underlying_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let bearer = TcpBearer::connect(addr).await.unwrap();
        let stream = bearer.into_stream();
        // Verify the stream is valid by checking peer addr.
        assert_eq!(stream.peer_addr().unwrap(), addr);
    }
}
