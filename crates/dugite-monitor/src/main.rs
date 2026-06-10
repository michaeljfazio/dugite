//! dugite-monitor — Terminal UI dashboard for the Dugite Cardano node.
//!
//! Polls the Dugite Prometheus endpoint (default http://localhost:12798/metrics)
//! every second and renders a real-time 5-panel dashboard:
//!
//! - **Node**:         Role, network, version, era, uptime
//! - **Chain**:        Epoch progress bar, block/slot/tip metrics, density, forks, tx counts
//! - **Connections**:  P2P state, inbound/outbound, cold/warm/hot, uni/bi/duplex counts
//! - **Resources**:    CPU %, live memory, RSS memory
//! - **Peers**:        RTT bands (0-50ms, 50-100ms, 100-200ms, 200ms+), min/avg/max RTT
//!
//! # Usage
//!
//! ```bash
//! dugite-monitor                                         # auto-discover running dugite-node
//! dugite-monitor --metrics-url http://host:12798/metrics # explicit endpoint (skip discovery)
//! dugite-monitor --network-magic 2                       # preview testnet epoch length
//! ```
//!
//! When `--metrics-url` is omitted, dugite-monitor enumerates running
//! `dugite-node` processes via `sysinfo` + `netstat2` and probes their
//! `/metrics` endpoints. If exactly one node is found it attaches
//! silently; if multiple are found a selection dialog is shown; if
//! none are found it falls back to `http://localhost:12798/metrics`.
//!
//! # Key bindings
//!
//! | Key      | Action                          |
//! |----------|---------------------------------|
//! | q / Esc  | Quit                            |
//! | t        | Cycle theme                     |
//! | r        | Force-refresh metrics           |
//! | s        | Switch to a different node      |
//! | h / ?    | Toggle help overlay             |

mod app;
mod dialog;
mod discover;
#[allow(dead_code)]
mod disk;
mod layout;
mod metrics;
mod theme;
mod ui;
mod widgets;

use std::io;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;

use app::App;
use metrics::fetch_metrics;

/// Default Prometheus metrics endpoint for the Dugite node.
const DEFAULT_METRICS_URL: &str = "http://localhost:12798/metrics";

/// Poll interval for fetching metrics from the Prometheus endpoint.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// CLI arguments for dugite-monitor.
#[derive(Parser, Debug)]
#[command(
    name = "dugite-monitor",
    about = "Terminal dashboard for the Dugite Cardano node"
)]
struct Args {
    /// URL of the Dugite Prometheus metrics endpoint.
    ///
    /// When omitted, dugite-monitor discovers running `dugite-node`
    /// processes and auto-attaches. If multiple are found a selection
    /// dialog appears. If none are found, falls back to
    /// `http://localhost:12798/metrics`.
    #[arg(long)]
    metrics_url: Option<String>,

    /// Network magic for epoch length calculation.
    ///
    /// Preview = 2 (epoch length 86,400 slots = 1 day).
    /// Mainnet = 764824073 (epoch length 432,000 slots = 5 days).
    /// Preprod = 1 (epoch length 432,000 slots = 5 days).
    ///
    /// When omitted the epoch length is auto-detected from the
    /// `dugite_network_magic` Prometheus metric.
    #[arg(long)]
    network_magic: Option<u64>,

    /// Path to the node's database directory.
    ///
    /// When supplied the Resources panel shows disk space usage for the
    /// filesystem that contains this directory (total, used, free, and a
    /// usage percentage bar).  When omitted the disk row is hidden.
    ///
    /// Example: `--db-path ./db-preview`
    #[arg(long, default_value = "")]
    db_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing so discovery INFO/WARN logs surface on stderr
    // (stdout is owned by the terminal). EnvFilter respects RUST_LOG
    // and defaults to "info".
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let args = Args::parse();
    let mut app = App::new();

    // Apply network magic epoch length override if provided on the CLI.
    if let Some(magic) = args.network_magic {
        app.epoch_length_override = app::Network::from_magic(magic).epoch_length();
    }

    // Store the database path so the Resources panel can query disk space.
    app.db_path = args.db_path.clone();

    // Resolve the metrics URL BEFORE entering raw mode so the discovery
    // INFO logs surface cleanly on stderr. The multi-node selection
    // dialog enters its own terminal session and tears it down before
    // returning. The dialog is themed using the app's default theme so
    // it matches the dashboard look once we launch the main UI.
    let resolution = match resolve_metrics_url(args.metrics_url.as_deref(), app.theme()).await? {
        Some(r) => r,
        None => {
            // User quit at the selection dialog.
            return Ok(());
        }
    };

    // Inherit the discovered --database-path if the CLI did not supply one.
    if app.db_path.is_empty() {
        if let Some(p) = resolution.db_path {
            app.db_path = p.display().to_string();
        }
    }

    // Setup terminal in raw alternate-screen mode for the main dashboard.
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Fetch initial metrics before the first render so the UI is not blank.
    let snapshot = fetch_metrics(&resolution.metrics_url).await;
    app.update_metrics(snapshot);

    let mut metrics_url = resolution.metrics_url;
    let result = run_loop(&mut terminal, &mut app, &mut metrics_url).await;

    // Restore terminal on exit.
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

/// Outcome of resolving how to reach a dugite-node.
struct ResolvedNode {
    metrics_url: String,
    /// `--database-path` captured during discovery, if any.
    db_path: Option<std::path::PathBuf>,
}

/// Resolve which dugite-node to attach to. Returns `Ok(Some(...))` to
/// proceed, `Ok(None)` if the user quit at the selection dialog.
async fn resolve_metrics_url(
    flag: Option<&str>,
    theme: &theme::Theme,
) -> Result<Option<ResolvedNode>> {
    // Explicit non-empty flag bypasses discovery.
    if let Some(url) = flag {
        if !url.is_empty() {
            return Ok(Some(ResolvedNode {
                metrics_url: url.to_string(),
                db_path: None,
            }));
        }
    }

    let nodes = discover::discover_nodes().await;
    match nodes.len() {
        0 => {
            tracing::info!(
                "no dugite-node process found, using default {}",
                DEFAULT_METRICS_URL
            );
            Ok(Some(ResolvedNode {
                metrics_url: DEFAULT_METRICS_URL.to_string(),
                db_path: None,
            }))
        }
        1 => {
            let node = nodes.into_iter().next().unwrap();
            tracing::info!(
                pid = node.pid,
                url = %node.metrics_url,
                "auto-attached to single dugite-node"
            );
            Ok(Some(ResolvedNode {
                metrics_url: node.metrics_url,
                db_path: node.db_path,
            }))
        }
        _ => {
            let chosen_url = match dialog::run(&nodes, theme)? {
                Some(u) => u,
                None => return Ok(None),
            };
            let chosen = nodes.into_iter().find(|n| n.metrics_url == chosen_url);
            Ok(Some(match chosen {
                Some(n) => ResolvedNode {
                    metrics_url: n.metrics_url,
                    db_path: n.db_path,
                },
                None => ResolvedNode {
                    metrics_url: chosen_url,
                    db_path: None,
                },
            }))
        }
    }
}

/// Main event loop: renders each frame, handles keyboard input, and periodically
/// refreshes metrics from the Prometheus endpoint.
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    metrics_url: &mut String,
) -> Result<()> {
    let mut last_fetch = tokio::time::Instant::now();

    loop {
        // Render current state.
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Short poll timeout so the metrics timer fires promptly.
        let timeout = POLL_INTERVAL
            .checked_sub(last_fetch.elapsed())
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_millis(100));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                // Only handle key press events (not release/repeat).
                if key.kind == KeyEventKind::Press {
                    // Any key dismisses the help overlay.
                    if app.show_help {
                        app.show_help = false;
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('t') => {
                            // Cycle through themes.
                            app.cycle_theme();
                        }
                        KeyCode::Char('r') => {
                            // Force immediate metrics refresh.
                            let snapshot = fetch_metrics(metrics_url).await;
                            app.update_metrics(snapshot);
                            last_fetch = tokio::time::Instant::now();
                        }
                        KeyCode::Char('s') => {
                            // Discover running dugite-node processes and let the
                            // user pick one to attach to. Reuses the existing
                            // terminal so the dashboard is restored automatically
                            // on the next draw cycle.
                            if let Some(chosen) = switch_node(terminal, app).await? {
                                *metrics_url = chosen;
                                let snapshot = fetch_metrics(metrics_url).await;
                                app.update_metrics(snapshot);
                                last_fetch = tokio::time::Instant::now();
                            }
                        }
                        KeyCode::Char('h') | KeyCode::Char('?') => {
                            app.toggle_help();
                        }
                        _ => {}
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        // Periodic metrics fetch.
        if last_fetch.elapsed() >= POLL_INTERVAL {
            let snapshot = fetch_metrics(metrics_url).await;
            app.update_metrics(snapshot);
            last_fetch = tokio::time::Instant::now();
        }
    }
}

/// Re-run discovery from inside the main loop, then present the themed
/// selection dialog. Returns the chosen `metrics_url`, or `None` if the
/// user dismissed without choosing (or no nodes were found).
///
/// Side-effect: when a node is selected, the dashboard's `db_path` is
/// refreshed from the discovery result so the Resources panel shows disk
/// usage for the newly-attached node.
async fn switch_node(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<Option<String>> {
    let nodes = discover::discover_nodes().await;
    let theme = app.theme();
    let chosen = dialog::select_with_terminal(terminal, &nodes, theme)?;
    if let Some(url) = chosen.as_deref() {
        if let Some(node) = nodes.iter().find(|n| n.metrics_url == url) {
            if let Some(p) = &node.db_path {
                app.db_path = p.display().to_string();
            }
        }
    }
    // The dialog overlay leaves whatever it last painted on screen until
    // the next render. Force a clear so the dashboard repaints cleanly
    // beneath the spot where the modal was sitting.
    terminal.clear()?;
    Ok(chosen)
}
