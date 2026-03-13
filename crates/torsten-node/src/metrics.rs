use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Fixed histogram bucket boundaries (in milliseconds) for latency tracking.
const LATENCY_BUCKETS_MS: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];

/// Prometheus-style histogram with fixed buckets.
#[derive(Debug)]
pub struct Histogram {
    /// Count of observations in each bucket (cumulative upper bound).
    buckets: Vec<AtomicU64>,
    /// Total count of observations.
    count: AtomicU64,
    /// Sum of all observed values (stored as f64 bits for atomicity).
    sum_bits: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Histogram {
            buckets: (0..LATENCY_BUCKETS_MS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(f64::to_bits(0.0)),
        }
    }

    /// Record an observation (value in milliseconds).
    /// Increments the first bucket whose upper bound >= value_ms.
    #[allow(dead_code)]
    pub fn observe(&self, value_ms: f64) {
        for (i, &bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            if value_ms <= bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        // Approximate sum update — relaxed ordering is fine for metrics
        loop {
            let old_bits = self.sum_bits.load(Ordering::Relaxed);
            let old_sum = f64::from_bits(old_bits);
            let new_sum = old_sum + value_ms;
            if self
                .sum_bits
                .compare_exchange_weak(
                    old_bits,
                    f64::to_bits(new_sum),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
    }

    /// Format as Prometheus histogram lines.
    fn to_prometheus(&self, name: &str, help: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
        let mut cumulative = 0u64;
        for (i, &bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!("{name}_bucket{{le=\"{bound}\"}} {cumulative}\n"));
        }
        let total = self.count.load(Ordering::Relaxed);
        out.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {total}\n"));
        let sum = f64::from_bits(self.sum_bits.load(Ordering::Relaxed));
        out.push_str(&format!("{name}_sum {sum}\n"));
        out.push_str(&format!("{name}_count {total}\n"));
        out
    }
}

/// Node metrics for monitoring
pub struct NodeMetrics {
    pub blocks_received: AtomicU64,
    pub blocks_applied: AtomicU64,
    pub transactions_received: AtomicU64,
    pub transactions_validated: AtomicU64,
    pub transactions_rejected: AtomicU64,
    pub peers_connected: AtomicU64,
    pub peers_cold: AtomicU64,
    pub peers_warm: AtomicU64,
    pub peers_hot: AtomicU64,
    pub sync_progress_pct: AtomicU64,
    pub slot_number: AtomicU64,
    pub block_number: AtomicU64,
    pub epoch_number: AtomicU64,
    pub utxo_count: AtomicU64,
    pub mempool_tx_count: AtomicU64,
    pub mempool_bytes: AtomicU64,
    pub rollback_count: AtomicU64,
    pub blocks_forged: AtomicU64,
    pub delegation_count: AtomicU64,
    pub treasury_lovelace: AtomicU64,
    pub drep_count: AtomicU64,
    pub proposal_count: AtomicU64,
    pub pool_count: AtomicU64,
    pub disk_available_bytes: AtomicU64,
    /// Peer handshake RTT histogram (milliseconds)
    pub peer_handshake_rtt_ms: Histogram,
    /// Block fetch latency histogram (milliseconds per block)
    pub peer_block_fetch_ms: Histogram,
    /// Node uptime in seconds
    startup_instant: std::time::Instant,
    /// Per-validation-error-type rejection counts (label → count).
    validation_errors: std::sync::Mutex<HashMap<String, u64>>,
}

impl NodeMetrics {
    pub fn new() -> Self {
        NodeMetrics {
            blocks_received: AtomicU64::new(0),
            blocks_applied: AtomicU64::new(0),
            transactions_received: AtomicU64::new(0),
            transactions_validated: AtomicU64::new(0),
            transactions_rejected: AtomicU64::new(0),
            peers_connected: AtomicU64::new(0),
            peers_cold: AtomicU64::new(0),
            peers_warm: AtomicU64::new(0),
            peers_hot: AtomicU64::new(0),
            sync_progress_pct: AtomicU64::new(0),
            slot_number: AtomicU64::new(0),
            block_number: AtomicU64::new(0),
            epoch_number: AtomicU64::new(0),
            utxo_count: AtomicU64::new(0),
            mempool_tx_count: AtomicU64::new(0),
            mempool_bytes: AtomicU64::new(0),
            rollback_count: AtomicU64::new(0),
            blocks_forged: AtomicU64::new(0),
            delegation_count: AtomicU64::new(0),
            treasury_lovelace: AtomicU64::new(0),
            drep_count: AtomicU64::new(0),
            proposal_count: AtomicU64::new(0),
            pool_count: AtomicU64::new(0),
            disk_available_bytes: AtomicU64::new(0),
            peer_handshake_rtt_ms: Histogram::new(),
            peer_block_fetch_ms: Histogram::new(),
            startup_instant: std::time::Instant::now(),
            validation_errors: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Record a transaction validation error by type.
    pub fn record_validation_error(&self, error_type: &str) {
        if let Ok(mut map) = self.validation_errors.lock() {
            *map.entry(error_type.to_string()).or_insert(0) += 1;
        }
    }

    /// Record a peer handshake latency observation.
    pub fn record_handshake_rtt(&self, rtt_ms: f64) {
        self.peer_handshake_rtt_ms.observe(rtt_ms);
    }

    /// Record a per-block fetch latency observation.
    pub fn record_block_fetch_latency(&self, ms_per_block: f64) {
        self.peer_block_fetch_ms.observe(ms_per_block);
    }

    pub fn add_blocks_received(&self, count: u64) {
        self.blocks_received.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_blocks_applied(&self, count: u64) {
        self.blocks_applied.fetch_add(count, Ordering::Relaxed);
    }

    pub fn set_slot(&self, slot: u64) {
        self.slot_number.store(slot, Ordering::Relaxed);
    }

    pub fn set_block_number(&self, block_no: u64) {
        self.block_number.store(block_no, Ordering::Relaxed);
    }

    pub fn set_epoch(&self, epoch: u64) {
        self.epoch_number.store(epoch, Ordering::Relaxed);
    }

    pub fn set_sync_progress(&self, pct: f64) {
        self.sync_progress_pct
            .store((pct * 100.0) as u64, Ordering::Relaxed);
    }

    pub fn set_utxo_count(&self, count: u64) {
        self.utxo_count.store(count, Ordering::Relaxed);
    }

    pub fn set_mempool_count(&self, count: u64) {
        self.mempool_tx_count.store(count, Ordering::Relaxed);
    }

    pub fn set_disk_available_bytes(&self, bytes: u64) {
        self.disk_available_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Format metrics as Prometheus exposition format
    pub(crate) fn to_prometheus(&self) -> String {
        let mut out = String::with_capacity(2048);

        // Counters (monotonically increasing totals)
        let counters: &[(&str, &str, &AtomicU64)] = &[
            (
                "torsten_blocks_received_total",
                "Total blocks received from peers",
                &self.blocks_received,
            ),
            (
                "torsten_blocks_applied_total",
                "Total blocks applied to ledger",
                &self.blocks_applied,
            ),
            (
                "torsten_transactions_received_total",
                "Total transactions received",
                &self.transactions_received,
            ),
            (
                "torsten_transactions_validated_total",
                "Total transactions validated",
                &self.transactions_validated,
            ),
            (
                "torsten_transactions_rejected_total",
                "Total transactions rejected",
                &self.transactions_rejected,
            ),
            (
                "torsten_rollback_count_total",
                "Total number of chain rollbacks",
                &self.rollback_count,
            ),
            (
                "torsten_blocks_forged_total",
                "Total blocks forged by this node",
                &self.blocks_forged,
            ),
        ];

        // Gauges (can go up and down)
        let gauges: &[(&str, &str, &AtomicU64)] = &[
            (
                "torsten_peers_connected",
                "Number of connected peers",
                &self.peers_connected,
            ),
            (
                "torsten_peers_cold",
                "Number of cold (known but unconnected) peers",
                &self.peers_cold,
            ),
            (
                "torsten_peers_warm",
                "Number of warm (connected, not syncing) peers",
                &self.peers_warm,
            ),
            (
                "torsten_peers_hot",
                "Number of hot (actively syncing) peers",
                &self.peers_hot,
            ),
            (
                "torsten_sync_progress_percent",
                "Chain sync progress (0-10000, divide by 100 for %)",
                &self.sync_progress_pct,
            ),
            (
                "torsten_slot_number",
                "Current slot number",
                &self.slot_number,
            ),
            (
                "torsten_block_number",
                "Current block number",
                &self.block_number,
            ),
            (
                "torsten_epoch_number",
                "Current epoch number",
                &self.epoch_number,
            ),
            (
                "torsten_utxo_count",
                "Number of entries in the UTxO set",
                &self.utxo_count,
            ),
            (
                "torsten_mempool_tx_count",
                "Number of transactions in the mempool",
                &self.mempool_tx_count,
            ),
            (
                "torsten_mempool_bytes",
                "Size of mempool in bytes",
                &self.mempool_bytes,
            ),
            (
                "torsten_delegation_count",
                "Number of active stake delegations",
                &self.delegation_count,
            ),
            (
                "torsten_treasury_lovelace",
                "Total lovelace in the treasury",
                &self.treasury_lovelace,
            ),
            (
                "torsten_drep_count",
                "Number of registered DReps",
                &self.drep_count,
            ),
            (
                "torsten_proposal_count",
                "Number of active governance proposals",
                &self.proposal_count,
            ),
            (
                "torsten_pool_count",
                "Number of registered stake pools",
                &self.pool_count,
            ),
            (
                "torsten_disk_available_bytes",
                "Available disk space in bytes on the database volume",
                &self.disk_available_bytes,
            ),
        ];

        for (name, help, value) in counters {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }

        for (name, help, value) in gauges {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }

        // Uptime gauge
        let uptime_secs = self.startup_instant.elapsed().as_secs();
        out.push_str(&format!(
            "# HELP torsten_uptime_seconds Time since node startup\n# TYPE torsten_uptime_seconds gauge\ntorsten_uptime_seconds {uptime_secs}\n"
        ));

        // Validation error breakdown
        if let Ok(errors) = self.validation_errors.lock() {
            if !errors.is_empty() {
                out.push_str("# HELP torsten_validation_errors_total Transaction validation errors by type\n");
                out.push_str("# TYPE torsten_validation_errors_total counter\n");
                let mut sorted: Vec<_> = errors.iter().collect();
                sorted.sort_by_key(|(k, _)| (*k).clone());
                for (error_type, count) in sorted {
                    out.push_str(&format!(
                        "torsten_validation_errors_total{{error=\"{error_type}\"}} {count}\n"
                    ));
                }
            }
        }

        // Histograms
        out.push_str(&self.peer_handshake_rtt_ms.to_prometheus(
            "torsten_peer_handshake_rtt_ms",
            "Peer handshake round-trip time in milliseconds",
        ));
        out.push_str(&self.peer_block_fetch_ms.to_prometheus(
            "torsten_peer_block_fetch_ms",
            "Per-block fetch latency in milliseconds",
        ));

        out
    }
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Start an HTTP metrics server on the given port.
/// Responds to any request with Prometheus-format metrics.
/// Returns `Err` if the port cannot be bound (e.g. address already in use).
pub async fn start_metrics_server(
    port: u16,
    metrics: Arc<NodeMetrics>,
) -> Result<(), std::io::Error> {
    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            info!(
                url = format_args!("http://{addr}/metrics"),
                "Metrics server started"
            );
            l
        }
        Err(e) => {
            error!("Failed to start metrics server on {addr}: {e}");
            return Err(e);
        }
    };

    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Metrics server accept error: {e}");
                continue;
            }
        };

        // Read the request to determine the path
        let mut buf = [0u8; 1024];
        let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
            .await
            .unwrap_or(0);
        let request = std::str::from_utf8(&buf[..n]).unwrap_or("");

        let response = if request.starts_with("GET /health") {
            let uptime = metrics.startup_instant.elapsed().as_secs();
            let slot = metrics.slot_number.load(Ordering::Relaxed);
            let block = metrics.block_number.load(Ordering::Relaxed);
            let epoch = metrics.epoch_number.load(Ordering::Relaxed);
            let sync_pct = metrics.sync_progress_pct.load(Ordering::Relaxed) as f64 / 100.0;
            let peers = metrics.peers_connected.load(Ordering::Relaxed);
            let body = format!(
                "{{\"status\":\"ok\",\"uptime_secs\":{uptime},\"slot\":{slot},\"block\":{block},\"epoch\":{epoch},\"sync_progress\":{sync_pct:.2},\"peers\":{peers}}}"
            );
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        } else {
            let body = metrics.to_prometheus();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        };

        if let Err(e) = stream.write_all(response.as_bytes()).await {
            error!("Metrics server write error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics() {
        let metrics = NodeMetrics::new();
        assert_eq!(metrics.blocks_applied.load(Ordering::Relaxed), 0);

        metrics.add_blocks_applied(2);
        assert_eq!(metrics.blocks_applied.load(Ordering::Relaxed), 2);

        metrics.set_slot(12345);
        assert_eq!(metrics.slot_number.load(Ordering::Relaxed), 12345);
    }

    #[test]
    fn test_prometheus_output() {
        let metrics = NodeMetrics::new();
        metrics.set_slot(99999);
        metrics.set_epoch(42);
        metrics.add_blocks_applied(100);

        let output = metrics.to_prometheus();
        assert!(output.contains("torsten_slot_number 99999"));
        assert!(output.contains("torsten_epoch_number 42"));
        assert!(output.contains("torsten_blocks_applied_total 100"));
        assert!(output.contains("# HELP"));
        // Verify correct metric types
        assert!(output.contains("# TYPE torsten_blocks_applied_total counter"));
        assert!(output.contains("# TYPE torsten_slot_number gauge"));
        assert!(output.contains("# TYPE torsten_rollback_count_total counter"));
        assert!(output.contains("# TYPE torsten_peers_connected gauge"));
    }

    #[test]
    fn test_histogram_observe() {
        let h = Histogram::new();
        h.observe(5.0); // → bucket le=5
        h.observe(50.0); // → bucket le=50
        h.observe(500.0); // → bucket le=500

        assert_eq!(h.count.load(Ordering::Relaxed), 3);
        let sum = f64::from_bits(h.sum_bits.load(Ordering::Relaxed));
        assert!((sum - 555.0).abs() < 0.01);

        // Each observation lands in exactly one bucket
        assert_eq!(h.buckets[1].load(Ordering::Relaxed), 1); // le=5.0
        assert_eq!(h.buckets[4].load(Ordering::Relaxed), 1); // le=50.0
        assert_eq!(h.buckets[7].load(Ordering::Relaxed), 1); // le=500.0

        // Verify cumulative output via prometheus format
        let output = h.to_prometheus("test", "test");
        assert!(output.contains("test_bucket{le=\"5\"} 1"));
        assert!(output.contains("test_bucket{le=\"50\"} 2")); // cumulative: 5 + 50
        assert!(output.contains("test_bucket{le=\"500\"} 3")); // cumulative: all three
        assert!(output.contains("test_bucket{le=\"+Inf\"} 3"));
    }

    #[test]
    fn test_histogram_prometheus_format() {
        let h = Histogram::new();
        h.observe(10.0);
        h.observe(100.0);

        let output = h.to_prometheus("test_latency", "Test latency");
        assert!(output.contains("# TYPE test_latency histogram"));
        assert!(output.contains("test_latency_bucket{le=\"10\"} 1"));
        assert!(output.contains("test_latency_bucket{le=\"100\"} 2"));
        assert!(output.contains("test_latency_bucket{le=\"+Inf\"} 2"));
        assert!(output.contains("test_latency_sum 110"));
        assert!(output.contains("test_latency_count 2"));
    }

    #[test]
    fn test_prometheus_output_includes_histograms() {
        let metrics = NodeMetrics::new();
        metrics.record_handshake_rtt(50.0);
        metrics.record_block_fetch_latency(25.0);

        let output = metrics.to_prometheus();
        assert!(output.contains("torsten_peer_handshake_rtt_ms_bucket"));
        assert!(output.contains("torsten_peer_block_fetch_ms_bucket"));
        assert!(output.contains("torsten_uptime_seconds"));
    }
}
