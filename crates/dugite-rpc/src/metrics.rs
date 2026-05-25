//! Metrics sink trait — kept dep-free of any concrete metrics library so
//! `dugite-node` can plug Prometheus, OpenTelemetry, or a no-op
//! implementation transparently.
//!
//! Granularity is per-method (`service` × `method` label cardinality is
//! bounded by the proto definitions — 4 services × ~5 methods = ~20
//! pairs, safe for Prometheus high-cardinality budgets). Status labels
//! likewise are bounded by the gRPC code enum.

use std::sync::Arc;
use std::time::Duration;

/// Implementations of this trait receive per-call observability events
/// from every service implementation.
///
/// All methods take `&self` (no `&mut self`) so implementations can use
/// atomic counters or sharded histograms without external locking. The
/// expected hot-path implementation atomically increments counters /
/// observes histograms keyed by `(service, method[, status])`.
pub trait RpcMetricsSink: Send + Sync + 'static {
    /// Fires once when a unary RPC starts. Implementations typically
    /// stash a deadline / start instant and rely on
    /// [`request_completed`](Self::request_completed) for the duration
    /// observation.
    fn request_started(&self, service: &str, method: &str) {
        let _ = (service, method);
    }

    /// Fires once when a unary RPC completes (success or error).
    /// `status` is the gRPC status code label (`OK`, `NOT_FOUND`,
    /// `UNIMPLEMENTED`, …). `duration` is the wall-clock time.
    fn request_completed(&self, service: &str, method: &str, status: &str, duration: Duration) {
        let _ = (service, method, status, duration);
    }

    /// Fires once when a streaming RPC opens a stream to a client.
    /// Implementations typically increment a `dugite_rpc_active_streams`
    /// gauge.
    fn stream_started(&self, service: &str, method: &str) {
        let _ = (service, method);
    }

    /// Fires once when a streaming RPC's stream ends (client disconnect,
    /// server shutdown, error). Implementations decrement the
    /// corresponding gauge.
    fn stream_ended(&self, service: &str, method: &str, status: &str) {
        let _ = (service, method, status);
    }
}

/// No-op sink — useful for tests and as the default when the host does
/// not wire a real metrics adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl RpcMetricsSink for NoopMetrics {}

/// Shorthand for the boxed sink shape every service holds.
pub type SharedMetrics = Arc<dyn RpcMetricsSink>;

/// Construct a shared no-op metrics sink — convenience for tests +
/// `Default` impls that need an `Arc<dyn RpcMetricsSink>`.
pub fn noop_metrics() -> SharedMetrics {
    Arc::new(NoopMetrics)
}
