//! `ping` — Ouroboros N2N/N2C connectivity probe (#1091).
//!
//! Mirrors real cardano-cli's `ping`: negotiate a handshake, then EITHER
//! keep sending KeepAlive pings (`--count`, N2N only), query the peer's
//! full supported-version table (`-Q`/`--query-versions`), or fetch the
//! chain tip (`-t`/`--tip`).
//!
//! # Grounded in two real peers, not documentation
//!
//! The wire behaviour and output SHAPE below were captured against a
//! running `dugite-node` (this repo's own N2C socket) AND a real
//! `cardano-node` 11.0.1 (both N2N port 3002 and N2C socket) — see the
//! per-mode notes on each function. Two findings surfaced along the way
//! that are OUT OF SCOPE for this command (they are `dugite-node` SERVER
//! defects, not `dugite-cli` client gaps) and are recorded here rather than
//! fixed, per this session's scope (#1091 is the CLI surface only):
//!
//! 1. `dugite-node`'s N2N handshake responder closed the bearer immediately
//!    after the TCP `network_rtt` measurement when probed with real
//!    `cardano-cli ping --host 127.0.0.1 --port <n2n-port>` — a real
//!    `cardano-node` peer on the same run negotiated cleanly. Worth its own
//!    issue; not investigated further here.
//! 2. `dugite-node`'s N2C handshake SERVER does not implement query mode at
//!    all (`run_n2c_handshake_server` in `dugite-network` never inspects
//!    the client's `query` flag, unlike its N2N sibling which does) — a
//!    query-mode N2C proposal against it is answered with a plain
//!    `MsgAcceptVersion` for the single best-common version rather than a
//!    `MsgQueryReply` listing the whole table. `query_n2c_versions`/
//!    `query_n2n_versions` (`dugite-network::handshake`) tolerate this by
//!    treating a lone `MsgAcceptVersion` as a one-entry table — confirmed
//!    live: real `cardano-cli ping -Q` against `dugite-node`'s own N2C
//!    socket returned exactly one queried version, and against a real
//!    `cardano-node` returned its full V16-V23 table.
//!
//! # `--tip` over N2C is a documented no-op, not a gap
//!
//! Verified against BOTH peers: `cardano-cli ping --unixsock ... --tip`
//! ALWAYS prints `{ "tip": [] }`, even against a real `cardano-node`. N2C's
//! `ping --tip` genuinely does not fetch anything over Unix sockets in
//! cardano-cli 11.0.0.0 — dugite-cli matches this exactly rather than
//! inventing a real tip fetch cardano-cli itself does not perform.
//!
//! # `--tip` over N2N fetches the real chain tip
//!
//! Sends `MsgFindIntersect` with an EMPTY point list over the ChainSync
//! mini-protocol — a proposal that can never intersect, so the peer's
//! `MsgIntersectNotFound` reply carries only its own current tip
//! (slot/hash/block number), which is all `ping --tip` needs. Verified
//! against a real `cardano-node` N2N port: `{"tip":[{"blockNo":...,
//! "hash":...,"slotNo":...}]}`.
//!
//! # Percentile statistics are NOT verified byte-exact against Haskell
//!
//! `--count`'s per-pong `median`/`p90` fields are computed here via a
//! straightforward nearest-rank quantile over the RTT samples observed so
//! far (cumulative, matching the observed shape: cardano-cli's own cookie-0
//! row has `max == mean == median == min == p90 == sample`, and later rows
//! diverge as more samples accumulate). The exact algorithm Haskell's
//! `ping` tool uses was not independently re-derived from source — only the
//! STRUCTURE (cumulative running stats, one row per cookie) and the
//! underlying RTT MEASUREMENT MECHANISM (real `KeepAlive` round-trips) are
//! grounded in the real captures above.

use anyhow::{bail, Result};
use clap::Args;
use dugite_network::handshake::{
    n2c::N2CVersionData, n2n::N2NVersionData, query_n2c_versions, query_n2n_versions,
    run_n2c_handshake_client, run_n2n_handshake_client,
};
use dugite_network::protocol::chainsync::{
    decode_message as decode_chainsync_message, encode_message as encode_chainsync_message,
    ChainSyncMessage,
};
use dugite_network::protocol::{
    PROTOCOL_HANDSHAKE, PROTOCOL_N2N_CHAINSYNC, PROTOCOL_N2N_KEEPALIVE,
};
use dugite_network::{Direction, KeepAliveClient, Mux, TcpBearer, UnixBearer};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Handshake/keepalive ingress cap — matches `dugite-node`'s own
/// `peer_connection.rs::DEFAULT_INGRESS_LIMIT` (4 MB).
const DEFAULT_INGRESS_LIMIT: usize = 4 * 1024 * 1024;
/// ChainSync ingress cap — matches `dugite-node`'s
/// `peer_connection.rs::CHAINSYNC_INGRESS_LIMIT` (a `MsgIntersectNotFound`
/// reply is tiny, but the channel is shared infrastructure sized the same
/// way production connections size it).
const CHAINSYNC_INGRESS_LIMIT: usize = 512 * 1024;
/// Interval between KeepAlive pings under `--count` — matches the ~1s
/// cadence observed in a real `cardano-cli ping --count` capture's
/// timestamps.
const PING_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Args, Debug)]
pub struct PingCmd {
    /// Stop after sending count requests and receiving count responses. If
    /// this option is not specified, ping will operate until interrupted.
    #[arg(short = 'c', long, value_name = "COUNT")]
    count: Option<u32>,
    /// Hostname/IP, e.g. relay.iohk.example.
    ///
    /// Real cardano-cli spells this `-h,--host`; dugite-cli only accepts
    /// the long form `--host` because clap reserves `-h` for `--help` and
    /// cannot bind it to two different arguments in the same subcommand the
    /// way cardano-cli's optparse-applicative does.
    #[arg(long, value_name = "HOST", conflicts_with = "unixsock")]
    host: Option<String>,
    /// Unix socket, e.g. file.socket.
    #[arg(short = 'u', long, value_name = "SOCKET")]
    unixsock: Option<PathBuf>,
    /// Port number, e.g. 1234.
    #[arg(short = 'p', long, value_name = "PORT")]
    port: Option<u16>,
    /// Network magic.
    #[arg(short = 'm', long, value_name = "MAGIC")]
    magic: Option<u64>,
    /// JSON output flag.
    #[arg(short = 'j', long)]
    json: bool,
    /// Quiet flag, CSV/JSON only output.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Query the supported protocol versions using the handshake protocol
    /// and terminate the connection.
    #[arg(short = 'Q', long = "query-versions")]
    query_versions: bool,
    /// Request tip then exit.
    #[arg(short = 't', long)]
    tip: bool,
}

impl PingCmd {
    pub fn run(self) -> Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run_async())
    }

    async fn run_async(self) -> Result<()> {
        let magic = self.magic.unwrap_or(764824073);

        match (&self.host, &self.unixsock) {
            (Some(_), Some(_)) => unreachable!("clap enforces --host/--unixsock exclusivity"),
            (None, None) => bail!("pass one of --host or --unixsock"),
            (Some(host), None) => {
                let port = self
                    .port
                    .ok_or_else(|| anyhow::anyhow!("--port is required with --host"))?;
                ping_n2n(
                    host,
                    port,
                    magic,
                    self.count,
                    self.query_versions,
                    self.tip,
                    self.json,
                    self.quiet,
                )
                .await
            }
            (None, Some(sock)) => {
                // Verified against a real cardano-cli 11.0.0.0: neither -Q
                // nor -t given over a unix socket is a hard error, not a
                // silent no-op or a plain-handshake default.
                if !self.query_versions && !self.tip {
                    bail!("Unix sockets only support queries for available versions or a tip.");
                }
                ping_n2c(
                    sock,
                    magic,
                    self.query_versions,
                    self.tip,
                    self.json,
                    self.quiet,
                )
                .await
            }
        }
    }
}

/// Resolve a `--host` value (IP literal or hostname) plus `--port` to a
/// `SocketAddr`, taking the first result — same "first result wins"
/// simplicity as most CLI ping tools; ping is not a load balancer.
async fn resolve_addr(host: &str, port: u16) -> Result<SocketAddr> {
    if let Ok(addr) = format!("{host}:{port}").parse::<SocketAddr>() {
        return Ok(addr);
    }
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve '{host}': {e}"))?;
    addrs
        .next()
        .ok_or_else(|| anyhow::anyhow!("'{host}' resolved to no addresses"))
}

/// Print one `<endpoint> {"field":value}` diagnostic line, matching real
/// cardano-cli's per-step output (both `--json` and plain-text forms
/// verified against a live capture — see module doc).
///
/// `--quiet` suppresses this line in BOTH `--json` and plain-text mode —
/// verified live: `cardano-cli ping --quiet --json` prints ONLY the final
/// `pongs`/`tip`/`queried_versions` result, no `network_rtt`/
/// `handshake_rtt`/`negotiated_version` lines before it.
fn emit_step(json: bool, quiet: bool, endpoint: &str, field: &str, value: &serde_json::Value) {
    if quiet {
        return;
    }
    if json {
        println!("{endpoint} {{\"{field}\":{value}}}");
    } else {
        match field {
            "network_rtt" => println!("{endpoint} network rtt: {value:.3}"),
            "handshake_rtt" => println!("{endpoint} handshake rtt: {value}s"),
            _ => println!("{endpoint} {field}: {value}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn ping_n2n(
    host: &str,
    port: u16,
    magic: u64,
    count: Option<u32>,
    query_versions: bool,
    tip: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let endpoint = format!("{host}:{port}");
    let addr = resolve_addr(host, port).await?;

    let net_start = Instant::now();
    let bearer = TcpBearer::connect(addr)
        .await
        .map_err(|e| anyhow::anyhow!("connect to {endpoint} failed: {e}"))?;
    let network_rtt = net_start.elapsed().as_secs_f64();
    emit_step(
        json,
        quiet,
        &endpoint,
        "network_rtt",
        &serde_json::json!(network_rtt),
    );

    let mut mux = Mux::new(bearer, true);
    let mut handshake_ch = mux.subscribe(
        PROTOCOL_HANDSHAKE,
        Direction::InitiatorDir,
        DEFAULT_INGRESS_LIMIT,
    );
    let keepalive_ch = count.map(|_| {
        mux.subscribe(
            PROTOCOL_N2N_KEEPALIVE,
            Direction::InitiatorDir,
            DEFAULT_INGRESS_LIMIT,
        )
    });
    let chainsync_ch = tip.then(|| {
        mux.subscribe(
            PROTOCOL_N2N_CHAINSYNC,
            Direction::InitiatorDir,
            CHAINSYNC_INGRESS_LIMIT,
        )
    });
    tokio::spawn(async move {
        let _ = mux.run().await;
    });

    if query_versions {
        let hs_start = Instant::now();
        let versions = query_n2n_versions(&mut handshake_ch, magic)
            .await
            .map_err(|e| anyhow::anyhow!("query-versions failed: {e}"))?;
        let handshake_rtt = hs_start.elapsed().as_secs_f64();
        emit_step(
            json,
            quiet,
            &endpoint,
            "handshake_rtt",
            &serde_json::json!(handshake_rtt),
        );
        let entries: Vec<serde_json::Value> = versions
            .iter()
            .map(|(v, d)| {
                serde_json::json!({
                    "version": format!("NodeToNodeVersionV{v}"),
                    "magic": d.network_magic,
                    "initiator": if d.initiator_only { "InitiatorOnly" } else { "InitiatorAndResponder" },
                    "peersharing": if d.peer_sharing { "PeerSharingEnabled" } else { "PeerSharingDisabled" },
                })
            })
            .collect();
        if !quiet {
            if json {
                println!(
                    "{endpoint} {{\"queried_versions\":{}}}",
                    serde_json::Value::Array(entries)
                );
            } else {
                let names: Vec<String> = versions
                    .iter()
                    .map(|(v, d)| format!("NodeToNodeVersionV{v} {}", d.network_magic))
                    .collect();
                println!("{endpoint} Queried versions [{}]", names.join(", "));
            }
        }
        return Ok(());
    }

    let hs_start = Instant::now();
    let our_data = N2NVersionData::new(magic, true, false);
    let result = run_n2n_handshake_client(&mut handshake_ch, &our_data)
        .await
        .map_err(|e| anyhow::anyhow!("handshake failed: {e}"))?;
    let handshake_rtt = hs_start.elapsed().as_secs_f64();
    emit_step(
        json,
        quiet,
        &endpoint,
        "handshake_rtt",
        &serde_json::json!(handshake_rtt),
    );
    let version_name = format!("NodeToNodeVersionV{}", result.version);
    if !quiet {
        if json {
            println!(
                "{endpoint} {{\"negotiated_version\":{{\"initiator\":\"InitiatorOnly\",\"magic\":{magic},\"peersharing\":\"PeerSharingDisabled\",\"version\":\"{version_name}\"}}}}"
            );
        } else {
            println!("{endpoint} Negotiated version {version_name} {magic} InitiatorOnly PeerSharingDisabled");
        }
    }

    if tip {
        let mut cs_ch = chainsync_ch.expect("subscribed above when tip is requested");
        let msg = encode_chainsync_message(&ChainSyncMessage::MsgFindIntersect(vec![]));
        let start = Instant::now();
        cs_ch
            .send(msg)
            .await
            .map_err(|e| anyhow::anyhow!("tip request failed: {e}"))?;
        let resp = cs_ch
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("tip request failed: {e}"))?;
        let rtt = start.elapsed().as_secs_f64();
        let decoded = decode_chainsync_message(&resp)
            .map_err(|e| anyhow::anyhow!("tip decode failed: {e}"))?;
        let (tip_slot, tip_hash, tip_block_number) = match decoded {
            ChainSyncMessage::MsgIntersectFound {
                tip_slot,
                tip_hash,
                tip_block_number,
                ..
            }
            | ChainSyncMessage::MsgIntersectNotFound {
                tip_slot,
                tip_hash,
                tip_block_number,
            } => (tip_slot, tip_hash, tip_block_number),
            other => bail!("unexpected ChainSync response to tip request: {other:?}"),
        };
        let entry = serde_json::json!({
            "addr": host,
            "port": port,
            "slotNo": tip_slot,
            "blockNo": tip_block_number,
            "hash": hex::encode(tip_hash),
            "rtt": rtt,
        });
        if json {
            println!("{{ \"tip\": [{entry}] }}");
        } else if !quiet {
            println!(
                "tip: slot={tip_slot} block={tip_block_number} hash={} rtt={rtt:.3}s",
                hex::encode(tip_hash)
            );
        }
        return Ok(());
    }

    if let Some(n) = count {
        let mut ka_ch = keepalive_ch.expect("subscribed above when count is requested");
        let (rtt_tx, mut rtt_rx) = tokio::sync::mpsc::channel::<f64>(n as usize + 1);
        let cancel = CancellationToken::new();
        let client = KeepAliveClient::new(PING_INTERVAL, cancel.clone()).with_rtt_sender(rtt_tx);
        let ka_task = tokio::spawn(async move { client.run(&mut ka_ch).await });

        // Non-JSON mode prints one CSV row PER pong as it arrives (verified
        // live: two rows a real cardano-cli emitted for `--count 2` carry
        // timestamps ~1s apart, matching `PING_INTERVAL`, not a single
        // post-hoc dump). JSON mode prints ONLY the final aggregated
        // `pongs` array — no progressive lines — also verified live.
        if !json && !quiet {
            println!("timestamp, host, cookie, sample, median, p90, mean, min, max, std");
        }
        let mut samples: Vec<f64> = Vec::new();
        for cookie in 0u64..n as u64 {
            let rtt_ms = rtt_rx
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("keepalive connection closed early"))?;
            samples.push(rtt_ms / 1000.0);
            if !json && !quiet {
                println!("{}", pong_stat_csv_row(&endpoint, cookie, &samples));
            }
        }
        cancel.cancel();
        let _ = ka_task.await;

        if json {
            let entries: Vec<serde_json::Value> = samples
                .iter()
                .enumerate()
                .map(|(i, _)| pong_stat_json(&endpoint, i as u64, &samples[..=i]))
                .collect();
            println!(
                "{{ \"pongs\": [{}] }}",
                entries
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",\n")
            );
        }
    }

    Ok(())
}

async fn ping_n2c(
    sock: &std::path::Path,
    magic: u64,
    query_versions: bool,
    tip: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let endpoint = sock.display().to_string();

    let net_start = Instant::now();
    let stream = tokio::net::UnixStream::connect(sock)
        .await
        .map_err(|e| anyhow::anyhow!("connect to '{endpoint}' failed: {e}"))?;
    let bearer = UnixBearer::new(stream);
    let network_rtt = net_start.elapsed().as_secs_f64();
    emit_step(
        json,
        quiet,
        &endpoint,
        "network_rtt",
        &serde_json::json!(network_rtt),
    );

    let mut mux = Mux::new(bearer, true);
    let mut handshake_ch = mux.subscribe(
        PROTOCOL_HANDSHAKE,
        Direction::InitiatorDir,
        DEFAULT_INGRESS_LIMIT,
    );
    tokio::spawn(async move {
        let _ = mux.run().await;
    });

    if query_versions {
        let hs_start = Instant::now();
        let versions = query_n2c_versions(&mut handshake_ch, magic)
            .await
            .map_err(|e| anyhow::anyhow!("query-versions failed: {e}"))?;
        let handshake_rtt = hs_start.elapsed().as_secs_f64();
        emit_step(
            json,
            quiet,
            &endpoint,
            "handshake_rtt",
            &serde_json::json!(handshake_rtt),
        );
        let entries: Vec<serde_json::Value> = versions
            .iter()
            .map(|(v, d)| serde_json::json!({"version": format!("NodeToClientVersionV{v}"), "magic": d.network_magic}))
            .collect();
        if !quiet {
            if json {
                println!(
                    "{endpoint} {{\"queried_versions\":{}}}",
                    serde_json::Value::Array(entries)
                );
            } else {
                let names: Vec<String> = versions
                    .iter()
                    .map(|(v, _)| format!("NodeToClientVersionV{v} {magic}"))
                    .collect();
                println!("{endpoint} Queried versions [{}]", names.join(", "));
            }
        }
        return Ok(());
    }

    // --tip (the only other reachable path — `run_async` bails otherwise).
    let hs_start = Instant::now();
    let our_data = N2CVersionData::new(magic);
    let result = run_n2c_handshake_client(&mut handshake_ch, &our_data)
        .await
        .map_err(|e| anyhow::anyhow!("handshake failed: {e}"))?;
    let handshake_rtt = hs_start.elapsed().as_secs_f64();
    emit_step(
        json,
        quiet,
        &endpoint,
        "handshake_rtt",
        &serde_json::json!(handshake_rtt),
    );
    let version_name = format!("NodeToClientVersionV{}", result.version);
    if !quiet {
        if json {
            println!(
                "{endpoint} {{\"negotiated_version\":{{\"magic\":{magic},\"version\":\"{version_name}\"}}}}"
            );
        } else {
            println!("{endpoint} Negotiated version {version_name} {magic}");
        }
    }

    debug_assert!(tip, "run_async only reaches here when --tip was passed");
    // Verified against BOTH a real cardano-node and dugite-node: N2C --tip
    // always prints an empty result — see module doc.
    if json {
        println!("{{ \"tip\": [] }}");
    } else {
        println!("tip: []");
    }
    Ok(())
}

/// Cumulative running stats over `samples_so_far` — verified live against a
/// real `cardano-cli ping --count`: the JSON per-pong object has NO `std`
/// field, the CSV row DOES (`NaN` at cookie 0, a real sample-stddev value
/// from cookie 1 onward — an n=1 sample stddev is undefined). See the
/// module doc for why the exact quantile algorithm is an approximation,
/// not a verified match to Haskell's.
struct PongStat {
    sample: f64,
    min: f64,
    max: f64,
    mean: f64,
    median: f64,
    p90: f64,
    /// Sample standard deviation (n-1 divisor); `NaN` for a single sample.
    std: f64,
    timestamp: String,
}

fn compute_pong_stat(samples_so_far: &[f64]) -> PongStat {
    let sample = *samples_so_far.last().expect("at least one sample");
    let min = samples_so_far.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = samples_so_far
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let n = samples_so_far.len() as f64;
    let mean = samples_so_far.iter().sum::<f64>() / n;
    let mut sorted: Vec<f64> = samples_so_far.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = quantile(&sorted, 0.5);
    let p90 = quantile(&sorted, 0.9);
    let std = if samples_so_far.len() < 2 {
        f64::NAN
    } else {
        let variance = samples_so_far
            .iter()
            .map(|s| (s - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        variance.sqrt()
    };
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    PongStat {
        sample,
        min,
        max,
        mean,
        median,
        p90,
        std,
        timestamp,
    }
}

fn pong_stat_json(endpoint: &str, cookie: u64, samples_so_far: &[f64]) -> serde_json::Value {
    let s = compute_pong_stat(samples_so_far);
    serde_json::json!({
        "cookie": cookie,
        "host": endpoint,
        "sample": s.sample,
        "median": s.median,
        "p90": s.p90,
        "mean": s.mean,
        "min": s.min,
        "max": s.max,
        "timestamp": s.timestamp,
    })
}

fn pong_stat_csv_row(endpoint: &str, cookie: u64, samples_so_far: &[f64]) -> String {
    let s = compute_pong_stat(samples_so_far);
    format!(
        "{}, {endpoint}, {cookie}, {:.3}, {:.3}, {:.3}, {:.3}, {:.3}, {:.3}, {}",
        s.timestamp,
        s.sample,
        s.median,
        s.p90,
        s.mean,
        s.min,
        s.max,
        if s.std.is_nan() {
            "NaN".to_string()
        } else {
            format!("{:.3}", s.std)
        }
    )
}

/// Nearest-rank quantile over an already-sorted slice.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Minimal wrapper so `PingCmd`'s `#[derive(Args)]` fields (private to
    /// this module) can be parsed from argv in tests without exposing them
    /// outside the crate.
    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        ping: PingCmd,
    }

    fn parse(args: &[&str]) -> Result<PingCmd, clap::Error> {
        let mut full = vec!["ping"];
        full.extend_from_slice(args);
        TestCli::try_parse_from(full).map(|c| c.ping)
    }

    #[test]
    fn host_and_unixsock_are_mutually_exclusive() {
        let err = parse(&[
            "--host",
            "example.com",
            "--unixsock",
            "/tmp/x.sock",
            "--port",
            "3001",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn host_and_unixsock_both_parse_independently() {
        assert!(parse(&["--host", "example.com", "--port", "3001"]).is_ok());
        assert!(parse(&["--unixsock", "/tmp/x.sock", "--tip"]).is_ok());
    }

    #[tokio::test]
    async fn run_async_requires_host_or_unixsock() {
        let cmd = parse(&[]).unwrap();
        let err = cmd.run_async().await.unwrap_err();
        assert!(err.to_string().contains("--host"));
    }

    #[tokio::test]
    async fn run_async_requires_port_with_host() {
        let cmd = parse(&["--host", "example.com"]).unwrap();
        let err = cmd.run_async().await.unwrap_err();
        assert!(err.to_string().contains("--port"));
    }

    /// The one N2C behaviour independently verified against a real
    /// `cardano-cli 11.0.0.0`: neither `-Q` nor `-t` over a unix socket is
    /// a hard error with this EXACT message (see module doc).
    #[tokio::test]
    async fn run_async_n2c_requires_query_versions_or_tip() {
        let cmd = parse(&["--unixsock", "/tmp/definitely-does-not-exist.sock"]).unwrap();
        let err = cmd.run_async().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Unix sockets only support queries for available versions or a tip."
        );
    }

    #[tokio::test]
    async fn run_async_n2c_with_tip_attempts_connection() {
        // -t is present, so the flag-combination check passes and the error
        // becomes a real (expected) connection failure, not the
        // "-Q or -t" validation error.
        let cmd = parse(&["--unixsock", "/tmp/definitely-does-not-exist.sock", "--tip"]).unwrap();
        let err = cmd.run_async().await.unwrap_err();
        assert!(err.to_string().contains("connect"));
    }

    // ── Pure stat/formatting logic ──────────────────────────────────────

    #[test]
    fn single_sample_has_nan_std_and_equal_quantiles() {
        let s = compute_pong_stat(&[0.005]);
        assert!(s.std.is_nan());
        assert_eq!(s.sample, 0.005);
        assert_eq!(s.min, 0.005);
        assert_eq!(s.max, 0.005);
        assert_eq!(s.mean, 0.005);
        assert_eq!(s.median, 0.005);
        assert_eq!(s.p90, 0.005);
    }

    #[test]
    fn two_samples_have_real_std() {
        let s = compute_pong_stat(&[0.001, 0.003]);
        assert!(!s.std.is_nan());
        assert!(s.std > 0.0);
        assert_eq!(s.min, 0.001);
        assert_eq!(s.max, 0.003);
        assert_eq!(s.sample, 0.003, "sample is the LAST (newest) value");
    }

    #[test]
    fn pong_stat_json_has_no_std_field() {
        let v = pong_stat_json("host:1", 0, &[0.001]);
        assert!(
            v.get("std").is_none(),
            "verified live: cardano-cli's JSON pong object has no std field"
        );
        assert!(v.get("cookie").is_some());
        assert!(v.get("sample").is_some());
    }

    #[test]
    fn pong_stat_csv_row_has_nan_std_for_one_sample() {
        let row = pong_stat_csv_row("host:1", 0, &[0.001]);
        assert!(row.ends_with("NaN"));
        // 10 comma-separated fields: timestamp, host, cookie, sample,
        // median, p90, mean, min, max, std.
        assert_eq!(row.split(", ").count(), 10);
    }

    #[test]
    fn quantile_of_empty_is_zero() {
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    #[test]
    fn quantile_median_of_three() {
        let sorted = vec![1.0, 2.0, 3.0];
        assert_eq!(quantile(&sorted, 0.5), 2.0);
    }

    #[tokio::test]
    async fn resolve_addr_accepts_ip_literal() {
        let addr = resolve_addr("127.0.0.1", 3001).await.unwrap();
        assert_eq!(addr.port(), 3001);
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }

    #[tokio::test]
    async fn resolve_addr_resolves_localhost() {
        let addr = resolve_addr("localhost", 3001).await.unwrap();
        assert_eq!(addr.port(), 3001);
    }
}
