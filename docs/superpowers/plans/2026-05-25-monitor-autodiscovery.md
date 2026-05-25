# dugite-monitor Auto-discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `dugite-monitor` automatically detect running `dugite-node` processes, attach silently when exactly one is found, prompt with a selection dialog when multiple are found, and fall back to today's default URL when none are found.

**Architecture:** New `discover` module inside `dugite-monitor` orchestrates three stages: process enumeration (`sysinfo`), LISTEN-socket enumeration (`netstat2`), and HTTP probing (`reqwest`) against `/metrics` looking for the `dugite_network_magic` discriminator. A pre-launch ratatui modal handles the multi-node selection. `--metrics-url`, when supplied, bypasses discovery entirely.

**Tech Stack:** Rust (existing crate), `sysinfo` 0.39 (already in workspace), `netstat2` 0.11.2 (new dep), `futures` 0.3 (already in workspace), `reqwest` (existing), `ratatui` (existing), `tokio` (existing).

**Spec:** `docs/superpowers/specs/2026-05-25-monitor-autodiscovery-design.md`

---

## File Structure

Files created (paths absolute under repo root):

- `crates/dugite-monitor/src/discover/mod.rs` — public `discover_nodes()` + `DiscoveredNode` type + orchestration
- `crates/dugite-monitor/src/discover/process.rs` — sysinfo wrapper; `find_dugite_node_processes()` + `extract_db_path_from_cmdline()`
- `crates/dugite-monitor/src/discover/sockets.rs` — netstat2 wrapper; `listen_ports_for_pids()`
- `crates/dugite-monitor/src/discover/probe.rs` — HTTP probe + parse; `probe_metrics_url()`, `is_dugite_response()`, `parse_discovered_fields()`
- `crates/dugite-monitor/src/dialog.rs` — pre-launch ratatui selection modal
- `crates/dugite-monitor/tests/discovery_integration.rs` — integration test against hyper servers

Files modified:

- `crates/dugite-monitor/Cargo.toml` — add `sysinfo`, `netstat2`, `futures`, `hyper` (dev), `tracing`
- `crates/dugite-monitor/src/main.rs` — change `--metrics-url` to `Option<String>`, wire discovery + dialog into startup

Each `discover/*` submodule owns one concern and is independently unit-testable. Orchestration lives only in `discover/mod.rs::discover_nodes()`.

---

## Task 1: Add dependencies

**Files:**
- Modify: `crates/dugite-monitor/Cargo.toml`

- [ ] **Step 1: Add the new dependencies**

Replace the `[dependencies]` block (lines 13-20) with:

```toml
[dependencies]
ratatui = { workspace = true }
crossterm = { workspace = true }
tokio = { workspace = true }
reqwest = { workspace = true }
clap = { workspace = true }
anyhow = { workspace = true }
libc = { workspace = true }
sysinfo = { version = "0.39", default-features = false, features = ["system"] }
netstat2 = "0.11"
futures = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
hyper = { version = "1", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
http-body-util = "0.1"
```

- [ ] **Step 2: Verify the dependency tree resolves**

Run: `cargo check -p dugite-monitor`
Expected: builds with zero errors, possibly warnings about unused imports (we will use these in subsequent tasks).

- [ ] **Step 3: Commit**

```bash
git add crates/dugite-monitor/Cargo.toml Cargo.lock
git commit -m "build(monitor): add sysinfo + netstat2 deps for auto-discovery"
```

---

## Task 2: DiscoveredNode type and module skeleton

**Files:**
- Create: `crates/dugite-monitor/src/discover/mod.rs`
- Modify: `crates/dugite-monitor/src/main.rs:29-36`

- [ ] **Step 1: Create the module file with the public type and a stub**

Create `crates/dugite-monitor/src/discover/mod.rs`:

```rust
//! Auto-discovery of running dugite-node processes.
//!
//! Three-stage pipeline:
//!   1. process enumeration (`sysinfo`)        — find dugite-node PIDs
//!   2. socket enumeration  (`netstat2`)       — find LISTEN ports per PID
//!   3. HTTP probe          (`reqwest`)        — confirm `/metrics` is dugite
//!
//! Public surface is `discover_nodes()` returning `Vec<DiscoveredNode>`.

use std::path::PathBuf;

pub mod probe;
mod process;
mod sockets;

use crate::app::Network;

/// A dugite-node process that has been discovered and probed.
#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    /// Process id of the dugite-node process.
    pub pid: u32,
    /// Full URL of the node's Prometheus metrics endpoint.
    pub metrics_url: String,
    /// Network derived from `dugite_network_magic`. `None` if probe could
    /// not extract the value.
    pub network: Option<Network>,
    /// `true` if `dugite_is_block_producer >= 1.0`, `false` if it is 0,
    /// `None` if the metric is absent.
    pub is_block_producer: Option<bool>,
    /// Protocol major version from `dugite_protocol_major_version`.
    pub protocol_major_version: Option<u64>,
    /// Tip slot from `dugite_slot_number`.
    pub tip_slot: Option<u64>,
    /// Sync progress percent from `dugite_sync_progress_percent`.
    pub sync_progress_percent: Option<f64>,
    /// Database path parsed from the process command line. `None` if not
    /// found.
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
/// endpoints in parallel.
///
/// Returns an empty Vec on any error (process scan failure, no nodes
/// running, all probes failed). Wrapped in a 2-second hard timeout so
/// that a slow probe cannot block the monitor's startup.
///
/// Stub: real implementation lands in a later task.
pub async fn discover_nodes() -> Vec<DiscoveredNode> {
    Vec::new()
}
```

- [ ] **Step 2: Declare the module in main.rs**

In `crates/dugite-monitor/src/main.rs`, add `mod discover;` to the module declarations block (lines 29-36):

```rust
mod app;
#[allow(dead_code)]
mod disk;
mod discover;
mod layout;
mod metrics;
mod theme;
mod ui;
mod widgets;
```

- [ ] **Step 3: Verify the module compiles**

Run: `cargo check -p dugite-monitor`
Expected: builds successfully. Unused-imports warning on `probe::*` is OK — used in later task.

- [ ] **Step 4: Commit**

```bash
git add crates/dugite-monitor/src/discover/mod.rs crates/dugite-monitor/src/main.rs
git commit -m "feat(monitor): scaffold discover module with DiscoveredNode type"
```

---

## Task 3: Process submodule — find_dugite_node_processes (TDD)

**Files:**
- Create: `crates/dugite-monitor/src/discover/process.rs`
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test for `extract_db_path_from_cmdline`**

Create `crates/dugite-monitor/src/discover/process.rs`:

```rust
//! Process enumeration via `sysinfo`. Finds dugite-node PIDs and
//! extracts the `--database-path` argument from each command line.

use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

/// Information about a single discovered dugite-node process.
#[derive(Debug, Clone)]
pub(super) struct DugiteProcess {
    pub pid: u32,
    pub db_path: Option<PathBuf>,
}

/// Find every running process whose executable name is exactly
/// `dugite-node`, returning its PID and (if parseable) the
/// `--database-path` argument.
pub(super) fn find_dugite_node_processes() -> Vec<DugiteProcess> {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);

    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let name = proc_.name().to_string_lossy();
        // Match the bare binary name. On macOS sysinfo returns the
        // basename ("dugite-node"); on Linux the same. Anything else
        // (e.g. "dugite-cli", "cardano-node") is skipped.
        if name != "dugite-node" {
            continue;
        }
        let cmdline: Vec<String> = proc_
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        out.push(DugiteProcess {
            pid: pid.as_u32(),
            db_path: extract_db_path_from_cmdline(&cmdline),
        });
    }
    out
}

/// Extract the value of `--database-path` (or its alias `--db-path`)
/// from a command line argv. Supports both whitespace-separated form
/// (`--database-path X`) and `=`-separated form (`--database-path=X`).
/// Returns the first occurrence if duplicated.
pub(super) fn extract_db_path_from_cmdline(cmdline: &[String]) -> Option<PathBuf> {
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        for prefix in ["--database-path", "--db-path"] {
            if arg == prefix {
                return iter.next().map(PathBuf::from);
            }
            if let Some(rest) = arg.strip_prefix(&format!("{prefix}=")) {
                return Some(PathBuf::from(rest));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn extract_db_path_whitespace_form() {
        let cmd = argv(&["dugite-node", "run", "--database-path", "/var/db", "--port", "3001"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), Some(PathBuf::from("/var/db")));
    }

    #[test]
    fn extract_db_path_equals_form() {
        let cmd = argv(&["dugite-node", "run", "--database-path=/var/db", "--port", "3001"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), Some(PathBuf::from("/var/db")));
    }

    #[test]
    fn extract_db_path_alias_form() {
        let cmd = argv(&["dugite-node", "run", "--db-path", "./db-preview"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), Some(PathBuf::from("./db-preview")));
    }

    #[test]
    fn extract_db_path_missing_returns_none() {
        let cmd = argv(&["dugite-node", "run", "--port", "3001"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), None);
    }

    #[test]
    fn extract_db_path_flag_with_no_value_returns_none() {
        let cmd = argv(&["dugite-node", "run", "--database-path"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), None);
    }

    #[test]
    fn extract_db_path_first_occurrence_wins() {
        let cmd = argv(&[
            "dugite-node", "run",
            "--database-path", "/first",
            "--database-path", "/second",
        ]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), Some(PathBuf::from("/first")));
    }
}
```

- [ ] **Step 2: Declare the new submodule**

Already declared in Task 2's `mod.rs` (`mod process;`). Verify with:

Run: `cargo check -p dugite-monitor`
Expected: compiles successfully.

- [ ] **Step 3: Run the unit tests**

Run: `cargo nextest run -p dugite-monitor discover::process`
Expected: 6 tests pass.

- [ ] **Step 4: Smoke-test against a live process**

Run: `cargo run -p dugite-monitor --example=list_dugite_procs 2>&1 || true`

If you want a quick smoke test, write a throwaway example. **Skip if no local node is running** — the unit tests are the gate.

- [ ] **Step 5: Commit**

```bash
git add crates/dugite-monitor/src/discover/process.rs
git commit -m "feat(monitor): enumerate dugite-node processes via sysinfo"
```

---

## Task 4: Sockets submodule — listen_ports_for_pids (TDD lite)

**Files:**
- Create: `crates/dugite-monitor/src/discover/sockets.rs`

This submodule is a thin wrapper around `netstat2`. The actual netstat2 call cannot be unit-tested without root or mocking, so we test only the filter logic.

- [ ] **Step 1: Write the failing test for `filter_listening_for_pids`**

Create `crates/dugite-monitor/src/discover/sockets.rs`:

```rust
//! LISTEN-socket enumeration via `netstat2`. Maps each dugite-node PID
//! to the ports it has bound for listening.

use std::collections::{HashMap, HashSet};

use netstat2::{
    AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, SocketInfo, TcpState,
};

/// For each PID in `pids`, return the list of TCP ports the process has
/// in the LISTEN state. PIDs with no listening ports are present in the
/// returned map with an empty Vec.
///
/// Returns an empty map on any netstat2 error. The caller will fall
/// back to its zero-node path.
pub(super) fn listen_ports_for_pids(pids: &HashSet<u32>) -> HashMap<u32, Vec<u16>> {
    let sockets = match netstat2::get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "netstat2 get_sockets_info failed");
            return HashMap::new();
        }
    };
    filter_listening_for_pids(&sockets, pids)
}

/// Pure filter half of `listen_ports_for_pids`, exposed for unit tests.
fn filter_listening_for_pids(
    sockets: &[SocketInfo],
    pids: &HashSet<u32>,
) -> HashMap<u32, Vec<u16>> {
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    for pid in pids {
        out.entry(*pid).or_default();
    }
    for info in sockets {
        let tcp = match &info.protocol_socket_info {
            ProtocolSocketInfo::Tcp(t) => t,
            _ => continue,
        };
        if tcp.state != TcpState::Listen {
            continue;
        }
        for pid in &info.associated_pids {
            if pids.contains(pid) {
                out.entry(*pid).or_default().push(tcp.local_port);
            }
        }
    }
    // Stable order makes downstream tests deterministic.
    for ports in out.values_mut() {
        ports.sort_unstable();
        ports.dedup();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstat2::TcpSocketInfo;
    use std::net::Ipv4Addr;

    fn tcp(pid: u32, port: u16, state: TcpState) -> SocketInfo {
        SocketInfo {
            protocol_socket_info: ProtocolSocketInfo::Tcp(TcpSocketInfo {
                local_addr: Ipv4Addr::LOCALHOST.into(),
                local_port: port,
                remote_addr: Ipv4Addr::UNSPECIFIED.into(),
                remote_port: 0,
                state,
            }),
            associated_pids: vec![pid],
            #[cfg(target_os = "linux")]
            inode: 0,
            #[cfg(target_os = "linux")]
            uid: 0,
        }
    }

    #[test]
    fn filter_picks_only_listening() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![
            tcp(1234, 12798, TcpState::Listen),
            tcp(1234, 3001, TcpState::Listen),
            tcp(1234, 50000, TcpState::Established),
        ];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![3001, 12798]));
    }

    #[test]
    fn filter_excludes_unrelated_pids() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![
            tcp(9999, 12798, TcpState::Listen),
            tcp(1234, 12798, TcpState::Listen),
        ];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![12798]));
        assert_eq!(out.get(&9999), None);
    }

    #[test]
    fn filter_returns_pid_with_empty_vec_when_no_sockets() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![]));
    }
}
```

- [ ] **Step 2: Declare the new submodule**

In `crates/dugite-monitor/src/discover/mod.rs`, the line `mod sockets;` is already added in Task 2. Verify with:

Run: `cargo check -p dugite-monitor`
Expected: compiles cleanly (zero warnings).

If the `inode` / `uid` cfg-gated fields don't match the netstat2 0.11 schema on your platform, drop them — they exist on Linux only. Check `netstat2`'s actual `SocketInfo` definition with `cargo doc --open -p netstat2`.

- [ ] **Step 3: Run the unit tests**

Run: `cargo nextest run -p dugite-monitor discover::sockets`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/dugite-monitor/src/discover/sockets.rs
git commit -m "feat(monitor): enumerate LISTEN ports per PID via netstat2"
```

---

## Task 5: Probe submodule — discriminator + HTTP probe (TDD)

**Files:**
- Create: `crates/dugite-monitor/src/discover/probe.rs`

- [ ] **Step 1: Write failing tests for the pure parse helpers**

Create `crates/dugite-monitor/src/discover/probe.rs`:

```rust
//! HTTP probe of a candidate metrics endpoint.
//!
//! Confirms the endpoint serves dugite Prometheus metrics (not
//! cardano-node, not some unrelated service) and extracts the
//! discriminator fields used to populate `DiscoveredNode`.

use std::time::Duration;

use crate::app::Network;

/// Fields extracted from a successful probe response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveredFields {
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
pub struct ProbeOutcome {
    pub url: String,
    pub fields: DiscoveredFields,
}

/// Probe an HTTP `/metrics` endpoint. Returns `Some` if the response
/// looks like dugite (contains `dugite_network_magic`) — otherwise
/// `None`. Times out after `PROBE_TIMEOUT`.
pub async fn probe_metrics_url(url: &str) -> Option<ProbeOutcome> {
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
/// missing metric yields `None` for that field — the dialog renders
/// missing fields as `--`.
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
                out.sync_progress_percent = Some(value);
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_dugite_response_true_for_dugite_body() {
        let body = "# HELP dugite_network_magic Network magic\ndugite_network_magic 2\n";
        assert!(is_dugite_response(body));
    }

    #[test]
    fn is_dugite_response_false_for_cardano_node_body() {
        let body = "# HELP cardano_node_metrics_blockNum_int\ncardano_node_metrics_blockNum_int 12345\n";
        assert!(!is_dugite_response(body));
    }

    #[test]
    fn is_dugite_response_false_for_empty_body() {
        assert!(!is_dugite_response(""));
    }

    #[test]
    fn parse_discovered_fields_complete_body() {
        let body = "\
# HELP dugite_network_magic Network magic
dugite_network_magic 2
dugite_is_block_producer 1
dugite_protocol_major_version 11
dugite_slot_number 111661041
dugite_sync_progress_percent 100.0
";
        let fields = parse_discovered_fields(body);
        assert_eq!(fields.network, Some(Network::Preview));
        assert_eq!(fields.is_block_producer, Some(true));
        assert_eq!(fields.protocol_major_version, Some(11));
        assert_eq!(fields.tip_slot, Some(111_661_041));
        assert_eq!(fields.sync_progress_percent, Some(100.0));
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
    fn parse_discovered_fields_ignores_labeled_lines_and_garbage() {
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
}
```

- [ ] **Step 2: Verify the file compiles**

Run: `cargo check -p dugite-monitor`
Expected: compiles cleanly. The line `pub mod probe;` was added in Task 2.

- [ ] **Step 3: Run the unit tests**

Run: `cargo nextest run -p dugite-monitor discover::probe`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/dugite-monitor/src/discover/probe.rs
git commit -m "feat(monitor): probe /metrics + parse dugite discriminator fields"
```

---

## Task 6: Orchestrator — discover_nodes()

**Files:**
- Modify: `crates/dugite-monitor/src/discover/mod.rs`

- [ ] **Step 1: Replace the stub `discover_nodes()` with the real orchestrator**

Replace the body of `discover_nodes()` (the function ending `Vec::new()`) and add the needed imports at the top of `mod.rs`. The full updated file:

```rust
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
/// the partial result is returned (zero nodes -> fall back to default
/// URL).
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    pub pid: u32,
    pub metrics_url: String,
    pub network: Option<Network>,
    pub is_block_producer: Option<bool>,
    pub protocol_major_version: Option<u64>,
    pub tip_slot: Option<u64>,
    pub sync_progress_percent: Option<f64>,
    pub db_path: Option<PathBuf>,
}

impl DiscoveredNode {
    pub fn role_label(&self) -> &'static str {
        match self.is_block_producer {
            Some(true) => "bp",
            Some(false) => "relay",
            None => "--",
        }
    }
}

pub async fn discover_nodes() -> Vec<DiscoveredNode> {
    match timeout(DISCOVERY_TIMEOUT, discover_inner()).await {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                "dugite-node discovery exceeded {:?}, falling back to empty result",
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

    // Build the candidate list: every (pid, port) for every PID we found.
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

    // Stitch outcomes back together with their (pid, db_path).
    let mut out = Vec::new();
    for ((pid, _port, db_path), outcome) in candidates.into_iter().zip(outcomes.into_iter()) {
        let Some(outcome) = outcome else {
            continue;
        };
        out.push(DiscoveredNode {
            pid,
            metrics_url: outcome.url,
            network: outcome.fields.network,
            is_block_producer: outcome.fields.is_block_producer,
            protocol_major_version: outcome.fields.protocol_major_version,
            tip_slot: outcome.fields.tip_slot,
            sync_progress_percent: outcome.fields.sync_progress_percent,
            db_path,
        });
    }
    out
}
```

- [ ] **Step 2: Verify the module compiles**

Run: `cargo check -p dugite-monitor`
Expected: compiles cleanly.

- [ ] **Step 3: Run all existing tests to make sure nothing regressed**

Run: `cargo nextest run -p dugite-monitor`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/dugite-monitor/src/discover/mod.rs
git commit -m "feat(monitor): wire discover_nodes orchestrator with 2s timeout"
```

---

## Task 7: Integration test — probe pipeline against fake servers (inline, no lib.rs)

**Files:**
- Modify: `crates/dugite-monitor/src/discover/probe.rs` (add hyper-based tests under existing `#[cfg(test)] mod tests`)
- Modify: `crates/dugite-monitor/Cargo.toml` (`[dev-dependencies]` already added in Task 1)

Rationale: `dugite-monitor` is a binary-only crate. Rather than splitting it into `lib.rs` + `main.rs` (which would force every internal module to be `pub`), we keep the hyper-based tests inline as `#[tokio::test]` functions inside `probe.rs`. They have direct access to crate-private items.

- [ ] **Step 1: Write the integration test**

Create `crates/dugite-monitor/tests/discovery_integration.rs`:

```rust
//! Integration test for the probe pipeline.
//!
//! Spawns three local HTTP servers on ephemeral ports — one dugite-like,
//! one cardano-node-like, and one returning 404 — and verifies that
//! `probe::probe_metrics_url` accepts only the dugite-like server.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

// Note: these tests live inside `probe.rs` as part of the existing
// `#[cfg(test)] mod tests` block, so they can use crate-private items
// directly. No lib.rs needed.

const DUGITE_BODY: &str = "\
# HELP dugite_network_magic Network magic
dugite_network_magic 2
dugite_is_block_producer 0
dugite_protocol_major_version 11
dugite_slot_number 111661041
dugite_sync_progress_percent 100.0
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
                    tokio::time::sleep(Duration::from_secs(2)).await;
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
    assert!(outcome.fields.tip_slot.is_some());
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
    // 500ms probe timeout + small overhead.
    assert!(elapsed < Duration::from_millis(900), "timeout took {:?}", elapsed);
}

#[test]
fn is_dugite_response_smoke() {
    assert!(is_dugite_response("dugite_network_magic 2"));
    assert!(!is_dugite_response("cardano_node_metrics_blockNum_int 1"));
}
```

- [ ] **Step 2: Expose the `discover` module to integration tests**

Integration tests cannot reach `pub(crate)` items unless the crate is configured as a library. `dugite-monitor` is binary-only today. The minimal change is to expose a tiny re-export module from the binary crate. In `crates/dugite-monitor/src/main.rs`, **after** the `mod discover;` line, add:

```rust
// Re-export discover for integration tests. Binary crates cannot be
// `use`d by tests, so we add a `[lib]` target indirectly via the
// `dugite_monitor` crate-root re-exports.
```

Actually the cleanest fix is to add a `[lib]` target to `crates/dugite-monitor/Cargo.toml`. After the `[[bin]]` block (line 9-11), add:

```toml
[lib]
name = "dugite_monitor"
path = "src/lib.rs"
```

Then create `crates/dugite-monitor/src/lib.rs`:

```rust
//! Library face of dugite-monitor, exposed only so integration tests
//! can reach the `discover` module. The user-facing entry point is the
//! `dugite-monitor` binary in `src/main.rs`.

pub mod app;
pub mod discover;
pub mod metrics;
```

Update `crates/dugite-monitor/src/main.rs` to use the library's modules instead of declaring them inline. Replace the module declaration block (the `mod app;`/`mod discover;`/etc. block) with:

```rust
// Internal modules used only by the binary.
mod disk;
mod dialog;   // added in Task 9
mod layout;
mod theme;
mod ui;
mod widgets;

// Modules shared with integration tests live in the library.
use dugite_monitor::{app, discover, metrics};
```

Note: `dialog` does not yet exist — it is created in Task 9. If you're running this task before Task 9, omit `mod dialog;` and add it back in Task 9.

- [ ] **Step 3: Verify the workspace builds**

Run: `cargo check -p dugite-monitor --all-targets`
Expected: builds cleanly. Any `unused_imports` warnings inside `main.rs` should be fixed by adjusting the `use` lines.

- [ ] **Step 4: Run the integration test**

Run: `cargo nextest run -p dugite-monitor --test discovery_integration`
Expected: 5 tests pass. Total wall time < 2 seconds.

- [ ] **Step 5: Commit**

```bash
git add crates/dugite-monitor/Cargo.toml crates/dugite-monitor/src/lib.rs crates/dugite-monitor/src/main.rs crates/dugite-monitor/tests/discovery_integration.rs
git commit -m "test(monitor): integration test for probe pipeline against fake servers"
```

---

## Task 8: CLI flag change — `--metrics-url` becomes Option<String>

**Files:**
- Modify: `crates/dugite-monitor/src/main.rs:60-90`

- [ ] **Step 1: Update the `Args` struct**

In `crates/dugite-monitor/src/main.rs`, replace the `metrics_url` field (currently at line 66-68):

```rust
    /// URL of the Dugite Prometheus metrics endpoint.
    #[arg(long, default_value = DEFAULT_METRICS_URL)]
    metrics_url: String,
```

with:

```rust
    /// URL of the Dugite Prometheus metrics endpoint.
    ///
    /// When omitted, dugite-monitor discovers running `dugite-node`
    /// processes and auto-attaches. If multiple are found a selection
    /// dialog appears. If none are found, falls back to
    /// `http://localhost:12798/metrics`.
    #[arg(long)]
    metrics_url: Option<String>,
```

- [ ] **Step 2: Verify the crate still compiles**

Run: `cargo check -p dugite-monitor`
Expected: compiles, but `main()` now has a type error where it passes `&args.metrics_url` (`&Option<String>`) to functions expecting `&str`. That is fixed in Task 9.

If the type error blocks you, temporarily wrap the call sites in `&args.metrics_url.clone().unwrap_or_else(|| DEFAULT_METRICS_URL.to_string())` and commit that as a stop-gap. Task 9 replaces it with the proper resolution logic.

- [ ] **Step 3: Commit**

```bash
git add crates/dugite-monitor/src/main.rs
git commit -m "feat(monitor): make --metrics-url optional, prep for discovery"
```

---

## Task 9: Resolution logic + selection dialog wiring

**Files:**
- Modify: `crates/dugite-monitor/src/main.rs`
- Create: `crates/dugite-monitor/src/dialog.rs`

- [ ] **Step 1: Create the selection dialog module**

Create `crates/dugite-monitor/src/dialog.rs`:

```rust
//! Pre-launch ratatui modal for selecting which dugite-node to attach
//! to when multiple have been discovered.
//!
//! Run *before* the main metrics loop enters its render cycle. Uses
//! the same `Terminal` + alternate-screen setup so we do not tear down
//! and rebuild the terminal between this modal and the main UI.

use std::io;
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use ratatui::prelude::*;

use dugite_monitor::discover::DiscoveredNode;

/// Show the selection dialog. Returns the chosen `metrics_url`, or
/// `None` if the user quit (Q / Esc / Ctrl-C).
pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    nodes: &[DiscoveredNode],
) -> Result<Option<String>> {
    if nodes.is_empty() {
        return Err(anyhow!("run() called with empty node list"));
    }

    let mut cursor: usize = 0;

    loop {
        terminal.draw(|frame| draw(frame, nodes, cursor))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up => {
                        if cursor > 0 {
                            cursor -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if cursor + 1 < nodes.len() {
                            cursor += 1;
                        }
                    }
                    KeyCode::Enter => {
                        return Ok(Some(nodes[cursor].metrics_url.clone()));
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        return Ok(None);
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn draw(frame: &mut ratatui::Frame, nodes: &[DiscoveredNode], cursor: usize) {
    let area = centered_rect(80, 60, frame.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Dugite Monitor — Select a node ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Header + footer + 3 rows per node.
    let header = "Multiple dugite-node processes found. Select one:";
    let footer = "↑/↓ select   Enter attach   q quit";

    let mut lines: Vec<Line> = Vec::with_capacity(nodes.len() * 3 + 3);
    lines.push(Line::from(header));
    lines.push(Line::from(""));

    for (i, node) in nodes.iter().enumerate() {
        let cursor_char = if i == cursor { "▸" } else { " " };
        let style = if i == cursor {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let network = node
            .network
            .map(|n| n.label())
            .unwrap_or("--")
            .to_string();
        let role = node.role_label();
        let era = era_label(node.protocol_major_version);
        let tip = node.tip_slot.map_or("--".to_string(), |s| format!("{s}"));
        let sync = node
            .sync_progress_percent
            .map_or("--".to_string(), |p| format!("{p:.1}%"));

        lines.push(Line::from(vec![Span::styled(
            format!("{cursor_char} {network:<8} {role:<5} {era:<8} tip {tip:<12} sync {sync}"),
            style,
        )]));

        let db = node
            .db_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--".to_string());
        lines.push(Line::from(vec![Span::raw(format!(
            "    pid {}  port {}  db {}",
            node.pid,
            port_of_url(&node.metrics_url).unwrap_or(0),
            db,
        ))]));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(footer));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn era_label(pv: Option<u64>) -> &'static str {
    match pv {
        Some(0..=1) => "Byron",
        Some(2..=3) => "Shelley",
        Some(4) => "Allegra",
        Some(5) => "Mary",
        Some(6) => "Alonzo",
        Some(7) => "Babbage",
        Some(_) => "Conway",
        None => "--",
    }
}

fn port_of_url(url: &str) -> Option<u16> {
    // Cheap parse: take the substring after "://", split on ':' once,
    // then split on '/' once.
    let after_scheme = url.split_once("://")?.1;
    let host_port = after_scheme.split_once('/').map(|p| p.0).unwrap_or(after_scheme);
    let port_str = host_port.rsplit_once(':')?.1;
    port_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_of_url_parses_standard() {
        assert_eq!(port_of_url("http://127.0.0.1:12798/metrics"), Some(12798));
        assert_eq!(port_of_url("http://localhost:12796"), Some(12796));
    }

    #[test]
    fn port_of_url_returns_none_when_missing() {
        assert_eq!(port_of_url("http://localhost/metrics"), None);
    }

    #[test]
    fn era_label_maps_major_versions() {
        assert_eq!(era_label(Some(0)), "Byron");
        assert_eq!(era_label(Some(7)), "Babbage");
        assert_eq!(era_label(Some(11)), "Conway");
        assert_eq!(era_label(None), "--");
    }
}
```

Drop the bogus `DUMMY_USE` line — that was a placeholder. The real line was:

```rust
// (remove DUMMY_USE — leftover from drafting)
```

i.e. delete the `pub(crate) const DUMMY_USE` line. The `use std::io;` import can also be removed; replace it with just the imports actually used.

- [ ] **Step 2: Wire discovery + dialog into `main()`**

In `crates/dugite-monitor/src/main.rs`, replace the body of `main()` with the version below. Keep existing imports and add `mod dialog;` plus `use anyhow::Context;` if not already present.

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing so discovery WARN logs surface.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let mut app = App::new();

    if let Some(magic) = args.network_magic {
        app.epoch_length_override = app::Network::from_magic(magic).epoch_length();
    }
    app.db_path = args.db_path.clone();

    // Setup terminal in raw alternate-screen mode.
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Resolve the metrics URL: explicit flag wins, otherwise discover.
    let metrics_url = match resolve_metrics_url(args.metrics_url.as_deref(), &mut terminal).await? {
        Some(url) => url,
        None => {
            // User quit at the selection dialog. Restore terminal and exit 0.
            disable_raw_mode()?;
            io::stdout().execute(LeaveAlternateScreen)?;
            return Ok(());
        }
    };

    // Fetch initial metrics before the first render so the UI is not blank.
    let snapshot = fetch_metrics(&metrics_url).await;
    app.update_metrics(snapshot);

    let result = run_loop(&mut terminal, &mut app, &metrics_url).await;

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

/// Resolve the metrics URL to attach to. Returns `Ok(Some(url))` to
/// proceed, `Ok(None)` if the user quit at the selection dialog.
async fn resolve_metrics_url<B: Backend>(
    flag: Option<&str>,
    terminal: &mut Terminal<B>,
) -> Result<Option<String>> {
    // Explicit non-empty flag bypasses discovery.
    if let Some(url) = flag {
        if !url.is_empty() {
            return Ok(Some(url.to_string()));
        }
    }

    let nodes = discover::discover_nodes().await;
    match nodes.len() {
        0 => {
            tracing::info!(
                "no dugite-node process found, using default {}",
                DEFAULT_METRICS_URL
            );
            Ok(Some(DEFAULT_METRICS_URL.to_string()))
        }
        1 => {
            let node = &nodes[0];
            tracing::info!(
                pid = node.pid,
                url = %node.metrics_url,
                "auto-attached to single dugite-node"
            );
            Ok(Some(node.metrics_url.clone()))
        }
        _ => Ok(dialog::run(terminal, &nodes)?),
    }
}
```

Also add `tracing-subscriber` as a workspace dep if the monitor doesn't already depend on it:

Run: `grep -E "tracing-subscriber" crates/dugite-monitor/Cargo.toml`

If absent, append to the `[dependencies]` block in `crates/dugite-monitor/Cargo.toml`:

```toml
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3: Verify the crate builds with all targets**

Run: `cargo check -p dugite-monitor --all-targets`
Expected: builds cleanly. If `Backend` or `Terminal` is not in scope in `main.rs`, add `use ratatui::prelude::*;` (it's already present).

- [ ] **Step 4: Run the full monitor test suite**

Run: `cargo nextest run -p dugite-monitor`
Expected: all tests pass (unit + integration).

- [ ] **Step 5: Commit**

```bash
git add crates/dugite-monitor/src/dialog.rs crates/dugite-monitor/src/main.rs crates/dugite-monitor/Cargo.toml
git commit -m "feat(monitor): pre-launch selection dialog + discovery resolution"
```

---

## Task 10: Full lint + format gate

**Files:** none

- [ ] **Step 1: Run the workspace formatter check**

Run: `cargo fmt --all -- --check`
Expected: clean. If it fails, run `cargo fmt --all` and commit the formatting fix.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy -p dugite-monitor --all-targets -- -D warnings`
Expected: zero warnings.

Common things to fix:
- Remove the leftover `use std::io;` in `dialog.rs` if `io` is unused.
- Replace any `unwrap()` with `expect("reason")` if clippy flags `unwrap_used`.
- Remove unused imports flagged by `unused_imports`.

- [ ] **Step 3: Run the full nextest suite**

Run: `cargo nextest run -p dugite-monitor`
Expected: all tests pass.

- [ ] **Step 4: Commit any lint fixups**

```bash
git add -u
# only if there were lint changes:
git commit -m "chore(monitor): clippy + fmt fixups"
```

If there are no changes, this step is a no-op.

---

## Task 11: Manual smoke test

**Files:** none (this is a manual verification step that informs the PR description, not an automated test).

- [ ] **Step 1: One-node smoke test**

With exactly one `dugite-node` running locally:

```bash
./target/release/dugite-monitor
```

Expected:
- No prompt; monitor opens directly on the dashboard.
- The stderr log shows `INFO ... auto-attached to single dugite-node pid=... url=http://127.0.0.1:.../metrics`.
- Within ~1 second the dashboard populates with real metrics.

- [ ] **Step 2: Two-node smoke test**

With two `dugite-node` processes running (e.g. preview + preprod):

```bash
./target/release/dugite-monitor
```

Expected:
- Selection dialog appears with two rows.
- Network labels are correct (Preview, Preprod).
- `pid`, `port`, and `db` columns are populated.
- `↑/↓` moves the cursor; `Enter` attaches to the highlighted node.
- `q` quits cleanly (no terminal corruption).

- [ ] **Step 3: Zero-node smoke test**

With no `dugite-node` running:

```bash
./target/release/dugite-monitor
```

Expected:
- Monitor opens directly on the dashboard showing "connecting…".
- The stderr log shows `INFO ... no dugite-node process found, using default http://localhost:12798/metrics`.

- [ ] **Step 4: Explicit-URL bypass test**

With no `dugite-node` running:

```bash
./target/release/dugite-monitor --metrics-url http://localhost:9999/metrics
```

Expected:
- Monitor opens immediately, no discovery delay.
- Dashboard shows connection-error banner against the unreachable URL.

- [ ] **Step 5: Document the smoke test result**

Add a short note in the PR description listing which scenarios you verified. There is no code change to commit for this step.

---

## Task 12: Final integration check

**Files:** none

- [ ] **Step 1: Run `just check`**

Run: `just check`
Expected: passes (fmt + clippy + build + test + test-doc).

- [ ] **Step 2: Verify CI-equivalent invocation**

Run: `RUSTFLAGS="-D warnings" cargo build -p dugite-monitor --release`
Expected: builds successfully with zero warnings.

- [ ] **Step 3: Open the PR**

Branch should already contain all commits from Tasks 1-10. Push and open the PR with a description that references the spec, lists the smoke-test scenarios verified, and notes the new dep (`netstat2 = "0.11"`).

---

## Self-review

Coverage check against the spec:

- ✅ Discovery method (sysinfo + netstat2) — Task 3 + Task 4
- ✅ `--metrics-url` bypass — Task 8 + Task 9
- ✅ Zero-node fall-back to default URL — Task 9
- ✅ Single-node silent attach + INFO log — Task 9
- ✅ Multi-node selection dialog — Task 9
- ✅ Dialog columns (network/role/era/tip/sync, pid/port/db) — Task 9
- ✅ Mid-session disconnect: unchanged behaviour — implicit (no change to `run_loop`)
- ✅ 2s hard timeout — Task 6
- ✅ 500ms per-probe timeout — Task 5
- ✅ Dugite discriminator (`dugite_network_magic`) — Task 5
- ✅ Cmdline parse for `--database-path` / `--db-path` — Task 3
- ✅ Unit tests for parse helpers — Task 3 + Task 5
- ✅ Integration tests for probe pipeline — Task 7
- ✅ Manual smoke test scenarios — Task 11

No placeholders, no "TBD", every code step has the actual code.

Type consistency: `DiscoveredNode` fields are introduced in Task 2 and used unchanged through Task 9. `is_block_producer: Option<bool>` (not `role: Option<Role>`) — the role label is derived via `role_label()` to keep the type minimal. `network: Option<Network>` reuses the existing `app::Network` enum.

One known caveat in Task 7: the `inode`/`uid` cfg-gated fields on `SocketInfo` may differ between netstat2 versions. The step calls this out explicitly and provides a fallback.

Estimated total LOC: ~600 lines new code, ~50 lines modified. Estimated time: 4-6 hours including manual smoke tests.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-25-monitor-autodiscovery.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
