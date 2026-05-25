# dugite-monitor: Auto-discovery of running nodes

**Status:** Design (pending implementation)
**Date:** 2026-05-25
**Tracking issue:** filed alongside implementation PR
**Crate:** `dugite-monitor`

## Summary

Today `dugite-monitor` defaults to `http://localhost:12798/metrics` and requires an explicit `--metrics-url` flag when the operator's node listens elsewhere (e.g. the running preview node on `:12796`, or when multiple nodes share the host). This design adds automatic discovery of running `dugite-node` processes via process + LISTEN-socket enumeration and presents a selection dialog when more than one node is found.

## Goals

- Zero-config attach when exactly one `dugite-node` is running.
- Interactive selection when multiple `dugite-node` processes are running.
- Silent fall-back to today's default URL when no `dugite-node` is found.
- Explicit `--metrics-url` always bypasses discovery.
- Linux + macOS only. Windows is out of scope (not on dugite's roadmap).

## Non-goals

- Remote-host discovery (Docker, SSH-tunnel). Operators with that setup pass `--metrics-url`.
- Auto-rediscovery on mid-session disconnect. The monitor continues to show "disconnected" and the operator restarts to re-run discovery.
- A separate `dugite-discovery` crate. Discovery lives inside `dugite-monitor` for v1 (YAGNI for sharing).
- Changes to `dugite-node`'s metrics-port configuration or any rendezvous-file mechanism.

## Decisions

| Question | Decision |
|---|---|
| Discovery method | Process scan via `sysinfo` + LISTEN-socket enum via `netstat2`. |
| `--metrics-url` interaction | Supplied → bypass discovery. Omitted (or empty string) → run discovery. |
| Zero nodes found | Silent fall-back to `http://localhost:12798/metrics`, one INFO log line. |
| Dialog columns | Network, role, era + tip slot + sync%, and PID + port + db path. |
| Mid-session disconnect | Show "disconnected" banner; no auto-reattach. |
| Dependencies | Add `netstat2`. `sysinfo` is already a workspace dep (`dugite-config` at 0.39). |

## Architecture

### Module layout

A new private module inside `dugite-monitor`:

```
crates/dugite-monitor/src/
├── discover/
│   ├── mod.rs       — public discover_nodes() entry point + DiscoveredNode type
│   ├── process.rs   — sysinfo wrapper: enumerate dugite-node PIDs + cmdlines
│   ├── sockets.rs   — netstat2 wrapper: PID → Vec<LISTEN port>
│   └── probe.rs     — HTTP GET /metrics + parse discriminator fields
└── main.rs          — wires discovery into startup
```

### Public API

```rust
pub struct DiscoveredNode {
    pub pid: u32,
    pub metrics_url: String,           // always http://127.0.0.1:<port>/metrics
    pub network: Option<Network>,      // from dugite_network_magic
    pub role: Option<Role>,            // from dugite_is_block_producer (1=bp, 0=relay)
    pub era: Option<Era>,              // from dugite_protocol_major_version
    pub tip_slot: Option<u64>,         // from dugite_slot_number
    pub sync_pct: Option<f64>,         // from dugite_sync_progress_percent
    pub db_path: Option<PathBuf>,      // from --database-path / --db-path argv
}

pub async fn discover_nodes() -> Vec<DiscoveredNode>;
```

`discover_nodes()` is the only public function. All other items in the submodules are crate-private.

### Discovery pipeline

```
1. Process scan (sysinfo)
   - Match process name exactly == "dugite-node".
   - Capture: pid, full cmdline (for db_path extraction).

2. Socket enum (netstat2)
   - Single call to get all TCP LISTEN sockets with their PIDs.
   - Filter to PIDs from step 1.
   - Filter local address to 127.0.0.1 / 0.0.0.0 / :: (skip foreign).

3. HTTP probe (reqwest, parallel via futures::join_all)
   - GET http://127.0.0.1:<port>/metrics with 500ms timeout.
   - Discriminator: response body contains the literal "dugite_network_magic".
   - Reject anything else (cardano-node's /metrics, N2N port 3001, etc.).

4. Parse + enrich
   - Reuse the Prometheus parser from metrics.rs.
   - Extract network/role/era/tip_slot/sync_pct (each Option, None if absent).
   - Extract db_path from cmdline.

5. Return Vec<DiscoveredNode> with one entry per (PID, port) that survived.
```

**Hard timeout:** the entire pipeline wraps in `tokio::time::timeout(2s, ...)`. On timeout the partial result is returned (zero nodes → fall-back).

### CLI changes

```rust
// before
#[arg(long, default_value = DEFAULT_METRICS_URL)]
metrics_url: String,

// after
#[arg(long)]
metrics_url: Option<String>,
```

Resolution in `main()`:

```rust
let url = match args.metrics_url.as_deref() {
    Some(u) if !u.is_empty() => u.to_string(),     // explicit flag, bypass discovery
    _ => {
        let nodes = discover_nodes().await;
        match nodes.len() {
            0 => {
                info!("no dugite-node process found, using {}", DEFAULT_METRICS_URL);
                DEFAULT_METRICS_URL.to_string()
            }
            1 => {
                info!("attached to dugite-node pid={} port={}", nodes[0].pid, port_of(&nodes[0]));
                nodes.into_iter().next().unwrap().metrics_url
            }
            _ => run_selection_dialog(nodes).await?,  // returns chosen URL or aborts on q
        }
    }
};
```

`--metrics-url ""` is treated as "not supplied" so shell-script callers passing
`"$DUGITE_METRICS_URL"` unconditionally still get discovery when the env var is unset.

### Selection dialog UI

Pre-launch ratatui modal, drawn before the main metrics loop enters. Uses the same `Terminal` + alternate-screen setup so there is no teardown/rebuild cycle.

Layout (full screen, single bordered block):

```
┌─ Dugite Monitor — Select a node ─────────────────────────────────┐
│                                                                  │
│   Multiple dugite-node processes found. Select one:              │
│                                                                  │
│   ▸ preview   relay  Conway   tip 111,661,041   sync 100.0%      │
│       pid 27995  port 12796   db ./db-preview                    │
│                                                                  │
│     preprod   bp     Conway   tip 89,234,567    sync  97.3%      │
│       pid 30166  port 12798   db ./db-preprod                    │
│                                                                  │
│   ↑/↓ select   Enter attach   q quit                             │
└──────────────────────────────────────────────────────────────────┘
```

Two-line rows so all four column groups fit without horizontal scrolling. Missing fields render as `--`. No live refresh — discovery runs once at launch.

Key bindings: `↑`/`↓` (cursor), `Enter` (attach), `q`/`Esc`/`Ctrl-C` (quit cleanly, restore terminal, exit 0).

## Error handling

| Failure | Behaviour |
|---|---|
| `sysinfo` refresh errors | WARN log, return empty Vec, fall back to default URL. |
| `netstat2::get_sockets_info` errors (e.g. permission denied) | Same. |
| dugite-node PIDs found but no LISTEN sockets matched | Treat as 0 nodes (e.g. `metrics_port: 0` disables metrics). |
| HTTP probe times out (>500ms) | Drop that port; do not retry. |
| Probe succeeds but body lacks `dugite_network_magic` | Drop; not a dugite metrics endpoint. |
| Probe succeeds but body is malformed Prometheus text | Include node with partial enrichment; missing dialog fields render as `--`. |
| Cmdline `--database-path` parse fails | `db_path = None`. |
| Total discovery >2s | `tokio::time::timeout` returns whatever completed; fall back if zero. |
| User Ctrl-C during dialog | Treat as `q` — restore terminal, exit 0. |
| `--metrics-url <unreachable>` | Out of scope; main loop's existing connection-error banner handles it. |
| Other-user dugite-node processes | Silently skipped (DEBUG log). Documented; not handled. |

Discovery must never panic and must never block the UI thread. Worst-case extra startup latency is 2s (the hard timeout). Common path is ~50-100ms.

## Testing

### Unit tests

- `extract_db_path_from_cmdline(&[String]) -> Option<PathBuf>`
  - `--database-path X` → Some(X)
  - `--db-path X` → Some(X)
  - `--database-path=X` → Some(X)
  - No flag → None
  - Flag with no value → None
  - Multiple occurrences → first wins
- `parse_discriminator(&str) -> DiscoveredFields`
  - Complete dugite response → all fields populated
  - Missing fields → partial populated, rest None
  - Malformed lines → skipped, parser doesn't panic
  - Empty body → all None
- `is_dugite_response(&str) -> bool`
  - Body containing `dugite_network_magic` → true
  - cardano-node response (contains `cardano_node_metrics_*`) → false
  - HTML 404 → false
  - Empty → false

### Integration tests

`crates/dugite-monitor/tests/discovery_test.rs`:

- Spin up N `hyper` servers on ephemeral ports serving canned `/metrics` payloads: one dugite-like, one cardano-node-like, one 404, one that delays past the 500ms timeout.
- Use a test-only constructor `probe_candidates(&[(pid, port)])` that skips process + socket enumeration and feeds candidates directly into the probe stage.
- Assert: only the dugite-like server survives; cardano-node and 404 are filtered; timeout is dropped.
- Assert: `Option<String> --metrics-url` parsing — `try_parse_from` with `--metrics-url=""` yields the no-discovery-bypass path (`""` treated as None).

### Manual smoke test (PR checklist, not automated)

- [ ] One node running → silent attach within ~1s; log confirms PID/port.
- [ ] Two nodes (preview + preprod) → selection dialog appears, both rows show correct network/era/sync.
- [ ] Zero nodes → monitor starts, default-URL "connecting…" state.
- [ ] `--metrics-url http://localhost:9999/metrics` with no node listening → discovery bypassed, immediate connection-error banner.

### Out of scope

- Property tests on the Prometheus parser (already covered in `metrics.rs`).
- Cross-OS CI matrix (netstat2 abstracts per-OS).
- Stress tests with many fake nodes (realistic max ~5, well within budget).

## Dependencies

| Crate | Version | Status | Notes |
|---|---|---|---|
| `sysinfo` | 0.39 | Already in workspace (dugite-config) | `default-features = false`, `features = ["system"]`. |
| `netstat2` | latest stable | **New** | Pure-Rust Linux + macOS socket enum. Confirm version + license in implementation plan. |

No changes to `dugite-node`, `dugite-config`, or any other crate.

## Open questions

None. All design decisions are settled in the Decisions table.

## Acceptance criteria

- [ ] `dugite-monitor` with no flags and exactly one running dugite-node attaches silently within 1s on a typical dev host.
- [ ] `dugite-monitor` with two running dugite-nodes shows the selection dialog.
- [ ] `dugite-monitor --metrics-url URL` skips discovery entirely.
- [ ] Discovery never panics; never blocks the UI for >2s.
- [ ] `just check` (fmt + clippy + build + test) passes.
- [ ] One INFO log line on successful auto-attach; one on fall-back to default URL.
- [ ] Unit tests for all parse helpers; integration test for the probe pipeline.
