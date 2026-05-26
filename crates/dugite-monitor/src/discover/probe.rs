//! HTTP probe of a candidate metrics endpoint.
//!
//! Confirms the endpoint serves dugite Prometheus metrics (not
//! cardano-node, not some unrelated service) and extracts the
//! discriminator fields used to populate `DiscoveredNode`.

use std::time::Duration;

use crate::app::Network;

/// Fields extracted from a successful probe response.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DiscoveredFields {
    pub network: Option<Network>,
    pub is_block_producer: Option<bool>,
    pub protocol_major_version: Option<u64>,
    pub tip_slot: Option<u64>,
    pub sync_progress_percent: Option<f64>,
}

/// Hard timeout for a single probe. Short enough that an unrelated
/// service holding a TCP connection open will not stall discovery.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Result of probing a single `(pid, port)` candidate.
#[derive(Debug, Clone)]
pub(crate) struct ProbeOutcome {
    pub url: String,
    pub fields: DiscoveredFields,
}

/// Probe an HTTP `/metrics` endpoint. Returns `Some` if the response
/// looks like dugite (contains `dugite_network_magic`). Times out after
/// `PROBE_TIMEOUT`.
pub(crate) async fn probe_metrics_url(url: &str) -> Option<ProbeOutcome> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    if !is_dugite_response(&body) {
        return None;
    }
    Some(ProbeOutcome {
        url: url.to_string(),
        fields: parse_discovered_fields(&body),
    })
}

/// Returns true iff the body looks like a dugite Prometheus payload.
/// The discriminator is the literal text `dugite_network_magic` — no
/// other Cardano implementation publishes a metric with that name.
pub(crate) fn is_dugite_response(body: &str) -> bool {
    body.contains("dugite_network_magic")
}

/// Parse the discriminator fields out of a Prometheus text body. Any
/// missing metric yields `None` for that field.
pub(crate) fn parse_discovered_fields(body: &str) -> DiscoveredFields {
    let mut out = DiscoveredFields::default();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Skip labeled metrics (we only need bare-name gauges).
        if line.contains('{') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name), Some(value_str)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(value) = value_str.parse::<f64>() else {
            continue;
        };
        match name {
            "dugite_network_magic" => {
                out.network = Some(Network::from_magic(value as u64));
            }
            "dugite_is_block_producer" => {
                out.is_block_producer = Some(value >= 1.0);
            }
            "dugite_protocol_major_version" => {
                out.protocol_major_version = Some(value as u64);
            }
            "dugite_slot_number" => {
                out.tip_slot = Some(value as u64);
            }
            "dugite_sync_progress_percent" => {
                // The node emits this as a fixed-point integer in [0, 10000]
                // where 10000 represents 100.00 %. Normalise to a real
                // percentage for downstream consumers (the selection dialog).
                out.sync_progress_percent = Some(value / 100.0);
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Pure parser unit tests ────────────────────────────────────────

    #[test]
    fn is_dugite_response_true_for_dugite_body() {
        let body = "# HELP dugite_network_magic Network magic\ndugite_network_magic 2\n";
        assert!(is_dugite_response(body));
    }

    #[test]
    fn is_dugite_response_false_for_cardano_node_body() {
        let body =
            "# HELP cardano_node_metrics_blockNum_int\ncardano_node_metrics_blockNum_int 12345\n";
        assert!(!is_dugite_response(body));
    }

    #[test]
    fn is_dugite_response_false_for_empty_body() {
        assert!(!is_dugite_response(""));
    }

    #[test]
    fn parse_discovered_fields_complete_body() {
        // The node emits `dugite_sync_progress_percent` as a fixed-point
        // integer in [0, 10000] (10000 == 100.00 %). The parser must
        // divide by 100 so downstream callers get a real percentage.
        let body = "\
# HELP dugite_network_magic Network magic
dugite_network_magic 2
dugite_is_block_producer 1
dugite_protocol_major_version 11
dugite_slot_number 111661041
dugite_sync_progress_percent 10000
";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.network, Some(Network::Preview));
        assert_eq!(fields.is_block_producer, Some(true));
        assert_eq!(fields.protocol_major_version, Some(11));
        assert_eq!(fields.tip_slot, Some(111_661_041));
        assert_eq!(fields.sync_progress_percent, Some(100.0));
    }

    #[test]
    fn parse_discovered_fields_partial_sync_progress() {
        // 9982 fixed-point should normalise to 99.82 %.
        let body = "dugite_network_magic 2\ndugite_sync_progress_percent 9982\n";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.sync_progress_percent, Some(99.82));
    }

    #[test]
    fn parse_discovered_fields_partial_body() {
        let body = "dugite_network_magic 1\n";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.network, Some(Network::Preprod));
        assert_eq!(fields.is_block_producer, None);
        assert_eq!(fields.protocol_major_version, None);
        assert_eq!(fields.tip_slot, None);
        assert_eq!(fields.sync_progress_percent, None);
    }

    #[test]
    fn parse_discovered_fields_ignores_labeled_and_garbage() {
        let body = "\
dugite_network_magic 2
dugite_pool_id_info{pool_id=\"abc\"} 1
dugite_is_block_producer NaN-not-a-number
garbage line
dugite_slot_number 42
";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.network, Some(Network::Preview));
        assert_eq!(fields.is_block_producer, None);
        assert_eq!(fields.tip_slot, Some(42));
    }

    #[test]
    fn parse_discovered_fields_block_producer_zero_is_relay() {
        let body = "dugite_network_magic 2\ndugite_is_block_producer 0\n";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.is_block_producer, Some(false));
    }

    // ─── Integration tests: hyper-based fake servers ───────────────────

    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    const DUGITE_BODY: &str = "\
# HELP dugite_network_magic Network magic
dugite_network_magic 2
dugite_is_block_producer 0
dugite_protocol_major_version 11
dugite_slot_number 111661041
dugite_sync_progress_percent 10000
";

    const CARDANO_NODE_BODY: &str = "\
# HELP cardano_node_metrics_blockNum_int Block number
cardano_node_metrics_blockNum_int 12345678
";

    async fn serve_body(body: &'static str, status: StatusCode) -> SocketAddr {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: Request<hyper::body::Incoming>| async move {
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from(body)))
                                .unwrap(),
                        )
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        addr
    }

    /// Server that holds the response for `SLOW_SERVER_DELAY`. The probe
    /// must time out well before this elapses. 30 s gives plenty of room
    /// even when the tokio scheduler is heavily loaded (6 000+ parallel tests).
    const SLOW_SERVER_DELAY: Duration = Duration::from_secs(30);

    async fn serve_slow() -> SocketAddr {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                        tokio::time::sleep(SLOW_SERVER_DELAY).await;
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .body(Full::new(Bytes::from(DUGITE_BODY)))
                                .unwrap(),
                        )
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn probe_accepts_dugite_endpoint() {
        let addr = serve_body(DUGITE_BODY, StatusCode::OK).await;
        let url = format!("http://{}/metrics", addr);
        let outcome = probe_metrics_url(&url).await;
        assert!(outcome.is_some(), "dugite endpoint must be accepted");
        let outcome = outcome.unwrap();
        assert_eq!(outcome.url, url);
        assert_eq!(outcome.fields.tip_slot, Some(111_661_041));
        assert_eq!(outcome.fields.network, Some(Network::Preview));
        assert_eq!(outcome.fields.is_block_producer, Some(false));
    }

    #[tokio::test]
    async fn probe_rejects_cardano_node() {
        let addr = serve_body(CARDANO_NODE_BODY, StatusCode::OK).await;
        let url = format!("http://{}/metrics", addr);
        let outcome = probe_metrics_url(&url).await;
        assert!(outcome.is_none(), "cardano-node endpoint must be rejected");
    }

    #[tokio::test]
    async fn probe_rejects_404() {
        let addr = serve_body("not found", StatusCode::NOT_FOUND).await;
        let url = format!("http://{}/metrics", addr);
        let outcome = probe_metrics_url(&url).await;
        assert!(outcome.is_none(), "404 response must be rejected");
    }

    #[tokio::test]
    async fn probe_times_out_on_slow_server() {
        let addr = serve_slow().await;
        let url = format!("http://{}/metrics", addr);
        let start = std::time::Instant::now();
        let outcome = probe_metrics_url(&url).await;
        let elapsed = start.elapsed();
        assert!(outcome.is_none(), "slow server must time out");
        // The contract is "we did not wait for the slow server to respond".
        // Assert elapsed well below SLOW_SERVER_DELAY (30 s) so the test is
        // not flaky under heavy parallel test-suite load (6 000+ tests).  We
        // allow up to 10 s to absorb tokio scheduling jitter; the actual
        // probe timeout is 500 ms, so any realistic schedule lag still
        // satisfies this bound while the slow server is still sleeping.
        assert!(
            elapsed < Duration::from_secs(10),
            "probe should time out well before {:?}, but took {:?}",
            SLOW_SERVER_DELAY,
            elapsed
        );
    }
}
