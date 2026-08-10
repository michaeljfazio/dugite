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
    /// HFC era index from `dugite_era` (authoritative era, independent of the
    /// Shelley-shaped protocol-version major). 0=Byron .. 7=Dijkstra.
    pub era: Option<u64>,
    pub tip_slot: Option<u64>,
    pub sync_progress_percent: Option<f64>,
}

/// Hard timeout for a single probe. Short enough that an unrelated
/// service holding a TCP connection open will not stall discovery.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Timeout for the single, deliberate probe of an explicit `--metrics-url`.
///
/// Longer than `PROBE_TIMEOUT` because that budget exists to keep a discovery
/// fan-out over every listening port from stalling on one unrelated service.
/// An explicit URL is one probe the operator asked for, and it may name a
/// remote host, so the discovery budget would report a slow-but-healthy node
/// as unreachable.
const EXPLICIT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// Outcome of probing an explicit `--metrics-url`.
///
/// Discovery collapses every failure into `None`, which is right when the
/// question is "is there a dugite node on this port" across many ports. For an
/// endpoint the operator NAMED, the two failures need different answers, and
/// conflating them is the defect this type exists to prevent: a URL that
/// answers with someone else's metrics is a mistake to report, while one that
/// does not answer yet is the ordinary case of starting the dashboard before
/// the node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExplicitProbe {
    /// The endpoint answered and publishes dugite metrics.
    Dugite,
    /// The endpoint answered, but it is not dugite. `hint` names what it
    /// looks like, so the error can say more than "wrong".
    NotDugite { hint: &'static str },
    /// The endpoint could not be reached, or did not answer in time.
    Unreachable,
}

/// Identify a non-dugite Prometheus body well enough to name it.
///
/// `cardano_node_metrics_` is cardano-node's own prefix, and pointing at a
/// co-located cardano-node is by far the most likely way to land here — the
/// two default ports differ by two digits.
fn describe_foreign_body(body: &str) -> &'static str {
    if body.contains("cardano_node_metrics_") {
        "a cardano-node metrics endpoint"
    } else if body.contains("# TYPE") || body.contains("# HELP") {
        "a Prometheus endpoint, but not dugite's"
    } else {
        "not a Prometheus metrics endpoint"
    }
}

/// Probe an endpoint the operator named explicitly, distinguishing "answered
/// with the wrong thing" from "did not answer".
pub(crate) async fn probe_explicit_url(url: &str) -> ExplicitProbe {
    let Ok(client) = reqwest::Client::builder()
        .timeout(EXPLICIT_PROBE_TIMEOUT)
        .build()
    else {
        return ExplicitProbe::Unreachable;
    };
    let Ok(resp) = client.get(url).send().await else {
        return ExplicitProbe::Unreachable;
    };
    // A 404 means something IS listening and served us a page that is not the
    // metrics endpoint — a wrong URL, not an absent node. Report it as such.
    if !resp.status().is_success() {
        return ExplicitProbe::NotDugite {
            hint: "an HTTP endpoint that did not return the metrics page",
        };
    }
    let Ok(body) = resp.text().await else {
        return ExplicitProbe::Unreachable;
    };
    if is_dugite_response(&body) {
        ExplicitProbe::Dugite
    } else {
        ExplicitProbe::NotDugite {
            hint: describe_foreign_body(&body),
        }
    }
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
            "dugite_era" => {
                out.era = Some(value as u64);
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

    // ─── Explicit --metrics-url: the three outcomes must stay distinct ──
    //
    // Discovery answers one question ("is there a dugite node here?") and
    // collapses every failure into None. An explicit URL asks two, and the
    // answers differ: a foreign endpoint is an operator mistake to report,
    // an absent one is the ordinary case of starting the dashboard first.
    // Each test below drives a case that USED to be indistinguishable.

    #[tokio::test]
    async fn explicit_probe_accepts_dugite() {
        let addr = serve_body(DUGITE_BODY, StatusCode::OK).await;
        let url = format!("http://{}/metrics", addr);
        assert_eq!(probe_explicit_url(&url).await, ExplicitProbe::Dugite);
    }

    /// The case the fix exists for: cardano-node's endpoint is two digits from
    /// dugite's default port, and every `dugite_*` gauge is absent from it —
    /// which the dashboard renders identically to a node stalled at slot zero.
    #[tokio::test]
    async fn explicit_probe_names_cardano_node_rather_than_drawing_it() {
        let addr = serve_body(CARDANO_NODE_BODY, StatusCode::OK).await;
        let url = format!("http://{}/metrics", addr);
        assert_eq!(
            probe_explicit_url(&url).await,
            ExplicitProbe::NotDugite {
                hint: "a cardano-node metrics endpoint"
            }
        );
    }

    /// A Prometheus endpoint that is neither dugite nor cardano-node is still
    /// wrong, and must not be silently attached to.
    #[tokio::test]
    async fn explicit_probe_rejects_unrelated_prometheus() {
        const OTHER: &str = "# HELP go_goroutines Number of goroutines\ngo_goroutines 7\n";
        let addr = serve_body(OTHER, StatusCode::OK).await;
        let url = format!("http://{}/metrics", addr);
        assert_eq!(
            probe_explicit_url(&url).await,
            ExplicitProbe::NotDugite {
                hint: "a Prometheus endpoint, but not dugite's"
            }
        );
    }

    /// Something IS listening and served a page that is not the metrics
    /// endpoint — a wrong URL, not an absent node. Distinct from Unreachable.
    #[tokio::test]
    async fn explicit_probe_reports_404_as_wrong_url_not_absent_node() {
        let addr = serve_body("not found", StatusCode::NOT_FOUND).await;
        let url = format!("http://{}/metrics", addr);
        assert_eq!(
            probe_explicit_url(&url).await,
            ExplicitProbe::NotDugite {
                hint: "an HTTP endpoint that did not return the metrics page"
            }
        );
    }

    /// Nothing listening: ordinary, and must NOT be an error — otherwise
    /// starting the dashboard before the node stops working.
    #[tokio::test]
    async fn explicit_probe_reports_closed_port_as_unreachable() {
        // Bind to get a free port, then drop the listener so nothing answers.
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let url = format!("http://{}/metrics", addr);
        assert_eq!(probe_explicit_url(&url).await, ExplicitProbe::Unreachable);
    }

    /// The explicit budget must be looser than the discovery budget, or an
    /// operator naming a remote host gets "unreachable" for a healthy node.
    /// Asserted on the constants, not by timing — see the note in
    /// `probe_times_out_on_slow_server`.
    #[test]
    fn explicit_probe_budget_is_looser_than_discovery() {
        assert!(
            EXPLICIT_PROBE_TIMEOUT > PROBE_TIMEOUT,
            "EXPLICIT_PROBE_TIMEOUT ({EXPLICIT_PROBE_TIMEOUT:?}) must exceed the \
             discovery fan-out budget ({PROBE_TIMEOUT:?})"
        );
    }

    #[tokio::test]
    async fn probe_times_out_on_slow_server() {
        let addr = serve_slow().await;
        let url = format!("http://{}/metrics", addr);
        let outcome = probe_metrics_url(&url).await;
        // The contract is "we did not wait for the slow server to respond".
        //
        // `serve_slow` answers with a VALID dugite body and 200 OK once
        // `SLOW_SERVER_DELAY` elapses, so a probe that failed to time out
        // would return `Some`. `is_none()` therefore proves the contract on
        // its own, and does so without measuring wall-clock time at all.
        //
        // There is deliberately NO wall-clock assertion here. Two previous
        // rounds each kept a "generous" elapsed-time backstop and each one
        // flaked anyway: `elapsed` includes tokio scheduling latency under
        // the full 7 000-test parallel run, which has exceeded 30 s on a
        // loaded machine while the probe itself returned in 500 ms. A bound
        // loose enough to never flake under load bounds nothing — it
        // measures the machine, not the probe.
        assert!(
            outcome.is_none(),
            "slow server must time out; a probe that waited would have parsed \
             the (valid) dugite body and returned Some"
        );

        // Guard the configured budget directly rather than by timing: the
        // probe must give up well before the server could possibly answer.
        // This is what actually protects the property above, and unlike a
        // measured duration it cannot flake.
        assert!(
            PROBE_TIMEOUT < SLOW_SERVER_DELAY / 2,
            "PROBE_TIMEOUT ({PROBE_TIMEOUT:?}) must stay well under \
             SLOW_SERVER_DELAY ({SLOW_SERVER_DELAY:?}) or this test stops \
             proving anything"
        );
    }
}
