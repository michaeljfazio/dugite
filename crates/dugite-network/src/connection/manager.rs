//! ConnectionManager — core lifecycle manager for all peer connections.
//!
//! Manages:
//! - Inbound TCP connection acceptance with rate limiting
//! - Outbound TCP connection establishment
//! - N2C Unix socket listener
//! - Connection deduplication and simultaneous open detection
//! - Connection limits (max inbound, max outbound, per-IP rate)

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::warn;

use super::handler::ConnectionHandler;
use super::state::{ConnectionState, DataFlow, Provenance};

/// Duration of the per-IP rate-limit sliding window.
///
/// Connections from the same IP address are counted within this window.
/// Connections accepted more than `PER_IP_WINDOW` ago are not counted.
const PER_IP_WINDOW: Duration = Duration::from_secs(60);

/// Per-IP connection window entry.
struct IpWindowEntry {
    /// Timestamps of accepted connections from this IP within the window.
    timestamps: Vec<Instant>,
}

/// Inbound idle timeout — connections in `InboundIdle` for longer than this
/// are transitioned to `TerminatingConn` and closed.
///
/// Matches Haskell `serverProtocolIdleTimeout = 300s` from
/// `Ouroboros.Network.Server2`.
const INBOUND_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Connection manager configuration.
#[derive(Debug, Clone)]
pub struct ConnectionManagerConfig {
    /// Maximum inbound connections.
    pub max_inbound: usize,
    /// Maximum outbound connections.
    pub max_outbound: usize,
    /// Maximum connection attempts per IP per minute.
    pub per_ip_rate_limit: usize,
    /// Network magic for handshake validation.
    pub network_magic: u64,
    /// Whether to enable peer sharing.
    pub peer_sharing: bool,
}

impl Default for ConnectionManagerConfig {
    fn default() -> Self {
        Self {
            max_inbound: 100,
            max_outbound: 20,
            per_ip_rate_limit: 5,
            network_magic: 2, // Preview testnet
            peer_sharing: true,
        }
    }
}

/// Tracks a single connection's state and handler.
struct ConnectionEntry {
    /// Current connection state.
    state: ConnectionState,
    /// Protocol handler for this connection (used by connection orchestration).
    #[allow(dead_code)]
    handler: ConnectionHandler,
    /// When this connection entered `InboundIdle` state.
    ///
    /// Set when `inbound_negotiated()` transitions to `InboundIdle`, cleared
    /// when `inbound_activity()` transitions to `InboundState`. Used by
    /// `check_inbound_idle_timeouts()` to enforce the 5-minute idle limit.
    idle_since: Option<Instant>,
}

/// ConnectionManager — central lifecycle manager for all connections.
pub struct ConnectionManager {
    /// Configuration.
    config: ConnectionManagerConfig,
    /// Active connections, keyed by peer address.
    connections: Arc<Mutex<HashMap<SocketAddr, ConnectionEntry>>>,
    /// Per-IP sliding-window counters for inbound rate limiting (G1, #547).
    ///
    /// Keyed by the remote IP address.  Each entry holds a `Vec<Instant>` of
    /// recent connection timestamps; entries older than `PER_IP_WINDOW` are
    /// pruned on access so the Vec stays bounded to `per_ip_rate_limit` entries.
    ip_window: Arc<Mutex<HashMap<IpAddr, IpWindowEntry>>>,
    /// Per-IP concurrent inbound connection count (A-002, #541).
    ///
    /// Keyed by source IP; value is the count of currently-active inbound
    /// connections from that IP. Decremented via `remove_connection()`. This
    /// gate complements `ip_window` — `ip_window` rate-limits new connection
    /// establishment over time, this rate-limits *concurrent* active connections.
    per_ip_count: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl ConnectionManager {
    /// Create a new connection manager.
    pub fn new(config: ConnectionManagerConfig) -> Self {
        Self {
            config,
            connections: Arc::new(Mutex::new(HashMap::new())),
            ip_window: Arc::new(Mutex::new(HashMap::new())),
            per_ip_count: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check whether a new inbound connection from `ip` is permitted under the
    /// per-IP rate limit.
    ///
    /// Returns `true` if the connection is within the rate limit and the new
    /// connection timestamp has been recorded.  Returns `false` if the IP has
    /// exceeded `per_ip_rate_limit` connections within `PER_IP_WINDOW` — the
    /// caller should drop the stream without spawning any task.
    ///
    /// This is the G1 per-IP gate that was previously defined in config but
    /// never enforced.  The accept loop in `dugite-node` calls this method
    /// immediately after TCP accept, before spawning any task.
    pub async fn check_and_record_inbound_ip(&self, ip: IpAddr) -> bool {
        let limit = self.config.per_ip_rate_limit;
        if limit == 0 {
            // Rate limiting disabled.
            return true;
        }

        let mut table = self.ip_window.lock().await;
        let now = Instant::now();
        let entry = table.entry(ip).or_insert_with(|| IpWindowEntry {
            timestamps: Vec::new(),
        });

        // Prune timestamps older than the window.
        entry
            .timestamps
            .retain(|t| now.duration_since(*t) < PER_IP_WINDOW);

        if entry.timestamps.len() >= limit {
            warn!(
                %ip,
                count = entry.timestamps.len(),
                limit,
                window_secs = PER_IP_WINDOW.as_secs(),
                "G1: per-IP inbound rate limit exceeded, dropping connection"
            );
            return false;
        }

        entry.timestamps.push(now);
        true
    }

    /// Reserve an outbound connection slot.
    ///
    /// Returns `Ok(())` if a slot is available, `Err` if max_outbound reached
    /// or a connection to this peer already exists.
    pub async fn reserve_outbound(
        &self,
        addr: SocketAddr,
    ) -> Result<(), crate::error::ConnectionError> {
        let mut conns = self.connections.lock().await;

        // Check for existing connection
        if conns.contains_key(&addr) {
            return Err(crate::error::ConnectionError::SimultaneousOpenConflict);
        }

        // Check outbound limit
        let outbound_count = conns
            .values()
            .filter(|e| {
                matches!(
                    e.state,
                    ConnectionState::ReservedOutbound
                        | ConnectionState::OutboundIdle(_)
                        | ConnectionState::OutboundUni
                        | ConnectionState::OutboundDup
                        | ConnectionState::UnnegotiatedConn(Provenance::Outbound)
                        | ConnectionState::DuplexConn
                )
            })
            .count();

        if outbound_count >= self.config.max_outbound {
            return Err(crate::error::ConnectionError::MaxConnectionsReached);
        }

        conns.insert(
            addr,
            ConnectionEntry {
                state: ConnectionState::ReservedOutbound,
                handler: ConnectionHandler::new(),
                idle_since: None,
            },
        );

        Ok(())
    }

    /// Record that an outbound connection completed handshake.
    pub async fn outbound_connected(&self, addr: SocketAddr, duplex: bool) {
        let mut conns = self.connections.lock().await;
        if let Some(entry) = conns.get_mut(&addr) {
            entry.state = ConnectionState::OutboundIdle(if duplex {
                DataFlow::Duplex
            } else {
                DataFlow::Unidirectional
            });
        }
    }

    /// Accept an inbound connection.
    ///
    /// Returns `Ok(())` if the connection is accepted, `Err` if limits reached.
    ///
    /// Enforces:
    /// - Global inbound limit (`max_inbound`)
    /// - Per-IP inbound limit (`per_ip_rate_limit`)
    ///
    /// A-001 / A-002 (security audit 2026-05-19): this function was previously
    /// never called from the actual N2N accept loop. It is now the single gate
    /// that must be passed before spawning a connection handler task.
    pub async fn accept_inbound(
        &self,
        addr: SocketAddr,
    ) -> Result<(), crate::error::ConnectionError> {
        let mut conns = self.connections.lock().await;

        // Check for existing connection (simultaneous open)
        if let Some(existing) = conns.get(&addr) {
            if existing.state == ConnectionState::ReservedOutbound {
                // Simultaneous open — we already have an outbound attempt.
                // The Haskell algorithm uses address comparison to resolve.
                return Err(crate::error::ConnectionError::SimultaneousOpenConflict);
            }
            // Already connected
            return Err(crate::error::ConnectionError::ForbiddenConnection);
        }

        // Check global inbound limit.
        let inbound_count = conns
            .values()
            .filter(|e| {
                matches!(
                    e.state,
                    ConnectionState::InboundIdle(_)
                        | ConnectionState::InboundState(_)
                        | ConnectionState::UnnegotiatedConn(Provenance::Inbound)
                        | ConnectionState::DuplexConn
                )
            })
            .count();

        if inbound_count >= self.config.max_inbound {
            return Err(crate::error::ConnectionError::MaxConnectionsReached);
        }

        // Check per-IP limit.
        let src_ip = addr.ip();
        let mut per_ip = self.per_ip_count.lock().await;
        let ip_count = per_ip.get(&src_ip).copied().unwrap_or(0);
        if ip_count >= self.config.per_ip_rate_limit {
            return Err(crate::error::ConnectionError::RateLimited(addr));
        }
        *per_ip.entry(src_ip).or_insert(0) += 1;
        drop(per_ip);

        conns.insert(
            addr,
            ConnectionEntry {
                state: ConnectionState::UnnegotiatedConn(Provenance::Inbound),
                handler: ConnectionHandler::new(),
                idle_since: None,
            },
        );

        Ok(())
    }

    /// Record that an inbound connection completed handshake.
    ///
    /// Transitions to `InboundIdle` and starts the idle timeout clock.
    pub async fn inbound_negotiated(&self, addr: SocketAddr, duplex: bool) {
        let mut conns = self.connections.lock().await;
        if let Some(entry) = conns.get_mut(&addr) {
            entry.state = ConnectionState::InboundIdle(if duplex {
                DataFlow::Duplex
            } else {
                DataFlow::Unidirectional
            });
            entry.idle_since = Some(Instant::now());
        }
    }

    /// Record mini-protocol activity on an inbound connection.
    ///
    /// If the connection is in `InboundIdle`, transitions to `InboundState`
    /// and cancels the idle timeout. This prevents active connections from
    /// being prematurely closed.
    pub async fn inbound_activity(&self, addr: SocketAddr) {
        let mut conns = self.connections.lock().await;
        if let Some(entry) = conns.get_mut(&addr) {
            if let ConnectionState::InboundIdle(df) = entry.state {
                entry.state = ConnectionState::InboundState(df);
                entry.idle_since = None;
            }
        }
    }

    /// Check for inbound idle timeouts and return addresses to terminate.
    ///
    /// Sweeps all connections in `InboundIdle` state. Any that have been idle
    /// longer than [`INBOUND_IDLE_TIMEOUT`] (5 minutes) are transitioned to
    /// `TerminatingConn` and their addresses returned for the caller to close.
    ///
    /// Matches Haskell `serverProtocolIdleTimeout` from `Ouroboros.Network.Server2`.
    pub async fn check_inbound_idle_timeouts(&self) -> Vec<SocketAddr> {
        let mut conns = self.connections.lock().await;
        let now = Instant::now();
        let mut to_terminate = Vec::new();

        for (addr, entry) in conns.iter_mut() {
            if matches!(entry.state, ConnectionState::InboundIdle(_)) {
                if let Some(since) = entry.idle_since {
                    if now.duration_since(since) >= INBOUND_IDLE_TIMEOUT {
                        entry.state = ConnectionState::TerminatingConn;
                        entry.idle_since = None;
                        to_terminate.push(*addr);
                    }
                }
            }
        }

        to_terminate
    }

    /// Remove a connection (disconnected).
    ///
    /// Decrements the per-IP inbound counter if this was an inbound connection.
    pub async fn remove_connection(&self, addr: &SocketAddr) {
        let mut conns = self.connections.lock().await;
        if let Some(entry) = conns.remove(addr) {
            // Only decrement per-IP for inbound connections (they incremented on accept).
            let was_inbound = matches!(
                entry.state,
                ConnectionState::InboundIdle(_)
                    | ConnectionState::InboundState(_)
                    | ConnectionState::UnnegotiatedConn(Provenance::Inbound)
                    | ConnectionState::DuplexConn
            );
            if was_inbound {
                let mut per_ip = self.per_ip_count.lock().await;
                let count = per_ip.entry(addr.ip()).or_insert(0);
                *count = count.saturating_sub(1);
                if *count == 0 {
                    per_ip.remove(&addr.ip());
                }
            }
        }
    }

    /// Get current connection count.
    pub async fn connection_count(&self) -> usize {
        self.connections.lock().await.len()
    }

    /// Get all connected peer addresses.
    pub async fn connected_peers(&self) -> Vec<SocketAddr> {
        self.connections.lock().await.keys().copied().collect()
    }

    /// Get the configuration.
    pub fn config(&self) -> &ConnectionManagerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), port)
    }

    #[tokio::test]
    async fn reserve_and_connect_outbound() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        cm.reserve_outbound(test_addr(3001)).await.unwrap();
        assert_eq!(cm.connection_count().await, 1);

        cm.outbound_connected(test_addr(3001), true).await;
        assert_eq!(cm.connection_count().await, 1);
    }

    #[tokio::test]
    async fn rejects_duplicate_outbound() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        cm.reserve_outbound(test_addr(3001)).await.unwrap();
        let result = cm.reserve_outbound(test_addr(3001)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn respects_outbound_limit() {
        let config = ConnectionManagerConfig {
            max_outbound: 2,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);

        cm.reserve_outbound(test_addr(3001)).await.unwrap();
        cm.reserve_outbound(test_addr(3002)).await.unwrap();

        let result = cm.reserve_outbound(test_addr(3003)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn accept_inbound() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        cm.accept_inbound(test_addr(3001)).await.unwrap();
        cm.inbound_negotiated(test_addr(3001), true).await;
        assert_eq!(cm.connection_count().await, 1);
    }

    // ── A-001 / A-002: per-IP rate limit (security audit 2026-05-19) ─────────

    #[tokio::test]
    async fn per_ip_rate_limit_enforced() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 3,
            max_inbound: 100,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);

        // Three connections from 1.2.3.4 should succeed.
        cm.accept_inbound("1.2.3.4:10001".parse().unwrap())
            .await
            .unwrap();
        cm.accept_inbound("1.2.3.4:10002".parse().unwrap())
            .await
            .unwrap();
        cm.accept_inbound("1.2.3.4:10003".parse().unwrap())
            .await
            .unwrap();

        // Fourth from same IP must be rate-limited.
        let result = cm.accept_inbound("1.2.3.4:10004".parse().unwrap()).await;
        assert!(
            matches!(result, Err(crate::error::ConnectionError::RateLimited(_))),
            "4th connection from same IP must be rate-limited; got: {result:?}"
        );
    }

    #[tokio::test]
    async fn per_ip_rate_limit_different_ips_independent() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 2,
            max_inbound: 100,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);

        // Two different IPs each within per-IP limit of 2.
        cm.accept_inbound("1.2.3.4:10001".parse().unwrap())
            .await
            .unwrap();
        cm.accept_inbound("1.2.3.4:10002".parse().unwrap())
            .await
            .unwrap();
        cm.accept_inbound("5.6.7.8:10001".parse().unwrap())
            .await
            .unwrap();
        cm.accept_inbound("5.6.7.8:10002".parse().unwrap())
            .await
            .unwrap();

        // Third from first IP: rate-limited.
        assert!(cm
            .accept_inbound("1.2.3.4:10003".parse().unwrap())
            .await
            .is_err());
        // Third from second IP: also rate-limited.
        assert!(cm
            .accept_inbound("5.6.7.8:10003".parse().unwrap())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn per_ip_slot_released_on_remove() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 1,
            max_inbound: 100,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);

        let addr: std::net::SocketAddr = "1.2.3.4:10001".parse().unwrap();
        cm.accept_inbound(addr).await.unwrap();

        // At limit: second connection rejected.
        assert!(cm
            .accept_inbound("1.2.3.4:10002".parse().unwrap())
            .await
            .is_err());

        // After removal: slot freed, new connection from same IP accepted.
        cm.remove_connection(&addr).await;
        cm.accept_inbound("1.2.3.4:10003".parse().unwrap())
            .await
            .unwrap();
    }

    /// Property test: N accept calls within limit succeed; N+1 is rejected.
    #[tokio::test]
    async fn per_ip_rate_limit_lattice() {
        for limit in [1usize, 2, 5, 10] {
            let config = ConnectionManagerConfig {
                per_ip_rate_limit: limit,
                max_inbound: 1000,
                ..Default::default()
            };
            let cm = ConnectionManager::new(config);
            // Fill to limit.
            for port in 0..limit {
                let addr: std::net::SocketAddr =
                    format!("9.9.9.9:{}", 10000 + port).parse().unwrap();
                assert!(
                    cm.accept_inbound(addr).await.is_ok(),
                    "connection {port} within limit {limit} should succeed"
                );
            }
            // One over: rejected.
            let over_addr: std::net::SocketAddr =
                format!("9.9.9.9:{}", 10000 + limit).parse().unwrap();
            assert!(
                cm.accept_inbound(over_addr).await.is_err(),
                "connection {limit} over per-IP limit {limit} should fail"
            );
        }
    }

    #[tokio::test]
    async fn remove_connection() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        cm.accept_inbound(test_addr(3001)).await.unwrap();
        assert_eq!(cm.connection_count().await, 1);

        cm.remove_connection(&test_addr(3001)).await;
        assert_eq!(cm.connection_count().await, 0);
    }

    #[tokio::test]
    async fn simultaneous_open_detected() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        // Reserve outbound
        cm.reserve_outbound(test_addr(3001)).await.unwrap();

        // Try to accept inbound from same address
        let result = cm.accept_inbound(test_addr(3001)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn inbound_idle_timeout_terminates() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());
        let addr = test_addr(3001);

        cm.accept_inbound(addr).await.unwrap();
        cm.inbound_negotiated(addr, true).await;

        // Immediately after negotiation — no timeout yet.
        let expired = cm.check_inbound_idle_timeouts().await;
        assert!(expired.is_empty(), "should not timeout immediately");

        // Manually set idle_since to 6 minutes ago to simulate time passing
        // (avoids dependency on tokio::time::pause which requires "test-util").
        {
            let mut conns = cm.connections.lock().await;
            let entry = conns.get_mut(&addr).unwrap();
            entry.idle_since = Some(Instant::now() - Duration::from_secs(360));
        }

        let expired = cm.check_inbound_idle_timeouts().await;
        assert_eq!(expired, vec![addr], "should timeout after 5+ minutes idle");
    }

    #[tokio::test]
    async fn inbound_activity_cancels_timeout() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());
        let addr = test_addr(3001);

        cm.accept_inbound(addr).await.unwrap();
        cm.inbound_negotiated(addr, true).await;

        // Simulate 3 minutes of idle time.
        {
            let mut conns = cm.connections.lock().await;
            let entry = conns.get_mut(&addr).unwrap();
            entry.idle_since = Some(Instant::now() - Duration::from_secs(180));
        }

        // Mini-protocol activity resets the timer.
        cm.inbound_activity(addr).await;

        // Even after more time, should NOT timeout because activity occurred
        // (state is now InboundState, not InboundIdle).
        let expired = cm.check_inbound_idle_timeouts().await;
        assert!(
            expired.is_empty(),
            "activity should cancel the idle timeout"
        );
    }

    #[tokio::test]
    async fn inbound_idle_no_false_positives_on_outbound() {
        let cm = ConnectionManager::new(ConnectionManagerConfig::default());

        // Outbound connection in idle state — should NOT be affected by
        // inbound idle timeout sweep.
        let addr = test_addr(3001);
        cm.reserve_outbound(addr).await.unwrap();
        cm.outbound_connected(addr, true).await;

        // Manually set idle_since (shouldn't happen in practice for outbound,
        // but verifies the sweep only targets InboundIdle).
        {
            let mut conns = cm.connections.lock().await;
            let entry = conns.get_mut(&addr).unwrap();
            entry.idle_since = Some(Instant::now() - Duration::from_secs(600));
        }

        let expired = cm.check_inbound_idle_timeouts().await;
        assert!(
            expired.is_empty(),
            "outbound connections should not be affected by inbound idle timeout"
        );
    }

    // ── G1: per-IP rate limit tests ─────────────────────────────────────────

    fn test_ip(last_octet: u8) -> IpAddr {
        use std::net::Ipv4Addr;
        IpAddr::V4(Ipv4Addr::new(1, 2, 3, last_octet))
    }

    /// The first `per_ip_rate_limit` connections from one IP are accepted;
    /// the next is rejected.
    #[tokio::test]
    async fn per_ip_rate_limit_enforced() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 3,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);
        let ip = test_ip(10);

        // First 3 connections: accepted
        for _ in 0..3 {
            assert!(
                cm.check_and_record_inbound_ip(ip).await,
                "connection within limit should be accepted"
            );
        }

        // 4th connection: rejected
        assert!(
            !cm.check_and_record_inbound_ip(ip).await,
            "connection exceeding limit should be rejected"
        );
    }

    /// A different IP is not affected by another IP's rate limit.
    #[tokio::test]
    async fn per_ip_rate_limit_per_source_independent() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 2,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);
        let ip_a = test_ip(1);
        let ip_b = test_ip(2);

        // Exhaust ip_a's limit
        cm.check_and_record_inbound_ip(ip_a).await;
        cm.check_and_record_inbound_ip(ip_a).await;
        assert!(
            !cm.check_and_record_inbound_ip(ip_a).await,
            "ip_a exhausted"
        );

        // ip_b is still within its own limit
        assert!(
            cm.check_and_record_inbound_ip(ip_b).await,
            "ip_b should not be affected by ip_a's limit"
        );
    }

    /// When per_ip_rate_limit = 0, rate limiting is disabled and every
    /// connection is accepted regardless of source IP.
    #[tokio::test]
    async fn per_ip_rate_limit_zero_disables() {
        let config = ConnectionManagerConfig {
            per_ip_rate_limit: 0,
            ..Default::default()
        };
        let cm = ConnectionManager::new(config);
        let ip = test_ip(42);

        for _ in 0..100 {
            assert!(
                cm.check_and_record_inbound_ip(ip).await,
                "all connections should be accepted when rate limit is disabled"
            );
        }
    }
}
