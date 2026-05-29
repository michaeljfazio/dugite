//! Auto-discovery of running dugite-node processes.
//!
//! Three-stage pipeline:
//!   1. process enumeration (`sysinfo`)        — find dugite-node PIDs
//!   2. socket enumeration  (`netstat2`)       — find LISTEN ports per PID
//!   3. HTTP probe          (`reqwest`)        — confirm `/metrics` is dugite
//!
//! Public surface is `discover_nodes()` returning `Vec<DiscoveredNode>`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use futures::future::join_all;
use tokio::time::timeout;

pub mod probe;
mod process;
mod sockets;

use crate::app::Network;

/// Hard wall-clock limit for the entire discovery pipeline. On timeout
/// the partial result is returned (zero nodes → fall back to default
/// URL).
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// A dugite-node process that has been discovered and probed.
#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    pub pid: u32,
    pub metrics_url: String,
    pub network: Option<Network>,
    pub is_block_producer: Option<bool>,
    pub protocol_major_version: Option<u64>,
    /// HFC era index (`dugite_era`) — authoritative era, see `DiscoveredFields::era`.
    pub era: Option<u64>,
    pub tip_slot: Option<u64>,
    pub sync_progress_percent: Option<f64>,
    pub db_path: Option<PathBuf>,
}

impl DiscoveredNode {
    /// Short human-readable role label for the selection dialog.
    pub fn role_label(&self) -> &'static str {
        match self.is_block_producer {
            Some(true) => "bp",
            Some(false) => "relay",
            None => "--",
        }
    }
}

/// Discover all running dugite-node processes and probe their metrics
/// endpoints in parallel. Empty Vec on any error or timeout.
pub async fn discover_nodes() -> Vec<DiscoveredNode> {
    match timeout(DISCOVERY_TIMEOUT, discover_inner()).await {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                "dugite-node discovery exceeded {:?}, returning empty result",
                DISCOVERY_TIMEOUT
            );
            Vec::new()
        }
    }
}

async fn discover_inner() -> Vec<DiscoveredNode> {
    let processes = process::find_dugite_node_processes();
    if processes.is_empty() {
        return Vec::new();
    }

    let pid_set: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    let ports_by_pid = sockets::listen_ports_for_pids(&pid_set);

    // Build (pid, port, db_path) candidate triples.
    let mut candidates: Vec<(u32, u16, Option<PathBuf>)> = Vec::new();
    for proc_ in &processes {
        let Some(ports) = ports_by_pid.get(&proc_.pid) else {
            continue;
        };
        for port in ports {
            candidates.push((proc_.pid, *port, proc_.db_path.clone()));
        }
    }

    // Probe every candidate in parallel.
    let probes = candidates.iter().map(|(_pid, port, _db)| {
        let url = format!("http://127.0.0.1:{port}/metrics");
        async move { probe::probe_metrics_url(&url).await }
    });
    let outcomes = join_all(probes).await;

    let mut out = Vec::new();
    for ((pid, _port, db_path), outcome) in candidates.into_iter().zip(outcomes) {
        let Some(outcome) = outcome else {
            continue;
        };
        out.push(DiscoveredNode {
            pid,
            metrics_url: outcome.url,
            network: outcome.fields.network,
            is_block_producer: outcome.fields.is_block_producer,
            protocol_major_version: outcome.fields.protocol_major_version,
            era: outcome.fields.era,
            tip_slot: outcome.fields.tip_slot,
            sync_progress_percent: outcome.fields.sync_progress_percent,
            db_path,
        });
    }
    out
}
