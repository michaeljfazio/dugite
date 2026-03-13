//! TCP socket configuration and timeout management.
//!
//! Provides configurable timeouts for network operations and TCP keepalive
//! configuration for long-lived Cardano peer connections.

use std::time::Duration;

/// Configurable timeouts for network operations.
///
/// All values have sensible defaults matching the existing hardcoded values
/// used throughout the codebase.
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Interval between keepalive ping messages (default: 16 seconds).
    pub keepalive_interval_secs: u64,
    /// Timeout waiting for a reply after AwaitReply (default: 90 seconds).
    pub await_reply_timeout_secs: u64,
    /// Timeout for peer sharing responses (default: 60 seconds).
    pub peersharing_timeout_secs: u64,
    /// Timeout for establishing a new TCP connection (default: 10 seconds).
    pub connection_timeout_secs: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        TimeoutConfig {
            keepalive_interval_secs: 16,
            await_reply_timeout_secs: 90,
            peersharing_timeout_secs: 60,
            connection_timeout_secs: 10,
        }
    }
}

impl TimeoutConfig {
    /// Duration for keepalive interval.
    pub fn keepalive_interval(&self) -> Duration {
        Duration::from_secs(self.keepalive_interval_secs)
    }

    /// Duration for await-reply timeout.
    pub fn await_reply_timeout(&self) -> Duration {
        Duration::from_secs(self.await_reply_timeout_secs)
    }

    /// Duration for peer sharing timeout.
    pub fn peersharing_timeout(&self) -> Duration {
        Duration::from_secs(self.peersharing_timeout_secs)
    }

    /// Duration for connection timeout.
    pub fn connection_timeout(&self) -> Duration {
        Duration::from_secs(self.connection_timeout_secs)
    }
}

/// Configure TCP keepalive on a connected socket.
///
/// Enables OS-level TCP keepalive probes to detect dead connections that
/// might otherwise go unnoticed (e.g., after network partitions or
/// machine sleep/hibernate). This is complementary to the application-level
/// Ouroboros KeepAlive mini-protocol.
///
/// Settings:
/// - Keepalive time: 60 seconds (idle time before first probe)
/// - Keepalive interval: 15 seconds (time between probes)
pub fn configure_tcp_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    use socket2::SockRef;
    let sock_ref = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    sock_ref.set_tcp_keepalive(&keepalive)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_config_defaults() {
        let config = TimeoutConfig::default();
        assert_eq!(config.keepalive_interval_secs, 16);
        assert_eq!(config.await_reply_timeout_secs, 90);
        assert_eq!(config.peersharing_timeout_secs, 60);
        assert_eq!(config.connection_timeout_secs, 10);
    }

    #[test]
    fn test_timeout_config_durations() {
        let config = TimeoutConfig {
            keepalive_interval_secs: 20,
            await_reply_timeout_secs: 120,
            peersharing_timeout_secs: 45,
            connection_timeout_secs: 5,
        };
        assert_eq!(config.keepalive_interval(), Duration::from_secs(20));
        assert_eq!(config.await_reply_timeout(), Duration::from_secs(120));
        assert_eq!(config.peersharing_timeout(), Duration::from_secs(45));
        assert_eq!(config.connection_timeout(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_configure_tcp_keepalive_on_real_socket() {
        // Create a real TCP listener and connect to it to get a valid TcpStream
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // configure_tcp_keepalive should not error on a valid stream
        let result = configure_tcp_keepalive(&stream);
        assert!(result.is_ok(), "configure_tcp_keepalive failed: {result:?}");
    }
}
