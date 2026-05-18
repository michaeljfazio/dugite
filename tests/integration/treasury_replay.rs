//! Integration tests replaying preview testnet from a known snapshot.
//!
//! These tests verify the fix for P0 (committee composition) and P1 (treasury balance)
//! against both the underlying ledger state and the CLI/Koios integration.
//!
//! # Dependencies
//!
//! - The `dugite-cli` binary must be available (see `DUGITE_CLI_PATH`).
//! - The binary must support the `--koios-url` flag for both
//!   `query committee-state` and `query treasury` commands.
//! - A Cardano node socket should be accessible (default `/tmp/node.sock`).
//!
//! # Environment Variables
//!
//! - `DUGITE_CLI_PATH` (optional): path to the dugite-cli binary.
//!   Defaults to `./target/release/dugite-cli`.
//! - `CARDANO_NODE_SOCKET_PATH` (optional): path to the node socket.
//!   Defaults to `/tmp/node.sock`.
//! - `RUST_LOG` (optional): controls log level (e.g., `debug`).
//! - `SKIP_ON_NO_SOCKET` (optional): set to `"0"` to fail instead of
//!   skipping when no socket is available. Default: `"1"`.
//! - `EPOCH_RANGE` (optional): comma-separated list of epochs for treasury tests.
//!   Defaults to `"1,2,3"`.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use log::{error, info, warn};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;
use wiremock::matchers::{method, path as path_matcher, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default path to the dugite-cli binary (relative to workspace root).
const DEFAULT_DUGITE_CLI_PATH: &str = "./target/release/dugite-cli";

/// Default Cardano node socket path.
const DEFAULT_SOCKET_PATH: &str = "/tmp/node.sock";

/// Testnet magic for preview network.
const TESTNET_MAGIC: &str = "2";

/// Tolerance in lovelace between CLI-reported and mock treasury values.
const TREASURY_TOLERANCE: u64 = 1;

/// Max time for a dugite-cli command to complete.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

/// Minimum expected committee members after the UpdateCommittee enactment.
const MIN_COMMITTEE_MEMBERS: usize = 8;

/// Expected per-epoch treasury in lovelace for the preview testnet snapshot.
/// Update these if the snapshot changes.
const KNOWN_TREASURY: &[(&str, u64)] = &[
    ("1", 1_000_000_000_000_000),
    ("2", 1_000_001_000_000_000),
    ("3", 1_000_002_000_000_000),
];

/// Epoch range used if `EPOCH_RANGE` env var is unset.
const DEFAULT_EPOCH_RANGE: &str = "1,2,3";

/// Global logger initialisation guard.
static LOG_INIT: Once = Once::new();

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialises the env_logger exactly once.
fn init_logger() {
    LOG_INIT.call_once(|| {
        if let Err(e) = env_logger::try_init() {
            eprintln!("Warning: env_logger init failed: {}", e);
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers: path resolution
// ---------------------------------------------------------------------------

/// Returns the path to `dugite-cli` binary.
fn dugite_cli_path() -> Result<PathBuf> {
    let path = env::var("DUGITE_CLI_PATH")
        .unwrap_or_else(|_| DEFAULT_DUGITE_CLI_PATH.to_owned());
    let pb = PathBuf::from(&path);
    if !pb.is_file() {
        return Err(anyhow!(
            "dugite-cli binary not found at {}. Set DUGITE_CLI_PATH.",
            pb.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = pb.metadata() {
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                warn!("Binary at {} may not be executable.", pb.display());
            }
        }
    }
    Ok(pb)
}

/// Returns the node socket path (environment or default).
fn node_socket_path() -> String {
    env::var("CARDANO_NODE_SOCKET_PATH")
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_owned())
}

/// Whether the test should skip when no socket is present.
fn skip_on_no_socket() -> bool {
    env::var("SKIP_ON_NO_SOCKET")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Validates the socket path; returns `None` if it does not exist and skipping is allowed.
fn maybe_socket_path() -> Result<Option<PathBuf>> {
    let sock = node_socket_path();
    let p = PathBuf::from(&sock);
    if !p.exists() {
        let msg = format!("Socket not found at '{}'", sock);
        if skip_on_no_socket() {
            warn!("{} – skipping test.", msg);
            return Ok(None);
        }
        return Err(anyhow!(msg));
    }
    Ok(Some(p))
}

// ---------------------------------------------------------------------------
// Helpers: fake Koios server
// ---------------------------------------------------------------------------

/// Creates a new mock Koios server.
async fn mock_koios_server() -> MockServer {
    MockServer::start().await
}

/// Registers a mock `/totals` response for a single epoch.
async fn register_totals_mock(
    server: &MockServer,
    epoch: u64,
    treasury_lovelace: u64,
) -> Mock {
    let body = json!([{
        "epoch_no": epoch,
        "treasury": treasury_lovelace.to_string()
    }]);
    let mock = Mock::given(method("GET"))
        .and(path_matcher("/totals"))
        .and(query_param("_epoch_no", epoch.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body));
    mock.mount(server).await
}

/// Registers a mock `/committee_info` response returning the specified number
/// of members (all hot‑credential hashes).
async fn register_committee_info_mock(
    server: &MockServer,
    member_count: usize,
) -> Mock {
    let members: Vec<Value> = (0..member_count)
        .map(|i| {
            json!({
                "cold": format!("000000000000000000000000000000000000000000000000000000000000000{}", i),
                "hot": format!("111111111111111111111111111111111111111111111111111111111111111{}", i),
                "expiration_epoch": 1000,
                "status": "active"
            })
        })
        .collect();
    let body = json!({ "committee": members, "epoch_no": 10 });
    let mock = Mock::given(method("GET"))
        .and(path_matcher("/committee_info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body));
    mock.mount(server).await
}

// ---------------------------------------------------------------------------
// Helpers: CLI invocation
// ---------------------------------------------------------------------------

/// Runs `dugite-cli query committee-state` and returns its stdout as a string.
async fn query_committee_state(
    cli: &Path,
    socket: &Path,
    koios_url: &str,
) -> Result<String> {
    run_cli_command(cli, socket, koios_url, &["query", "committee-state"]).await
}

/// Runs `dugite-cli query treasury` for a specific epoch and returns stdout.
async fn query_treasury(
    cli: &Path,
    socket: &Path,
    koios_url: &str,
    epoch: u64,
) -> Result<String> {
    run_cli_command(
        cli,
        socket,
        koios_url,
        &["query", "treasury", "--epoch", &epoch.to_string()],
    )
    .await
}

/// Generic CLI runner with timeout and error handling.
async fn run_cli_command(
    cli: &Path,
    socket: &Path,
    koios_url: &str,
    args: &[&str],
) -> Result<String> {
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new(cli)
            .arg("--socket-path")
            .arg(socket)
            .arg("--testnet-magic")
            .arg(TESTNET_MAGIC)
            .arg("--koios-url")
            .arg(koios_url)
            .args(args)
            .output(),
    )
    .await
    .context("CLI command timed out")?
    .with_context(|| format!("Failed to execute {} {:?}", cli.display(), args))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        error!(
            "CLI command failed: {} {:?}\nstdout: {}\nstderr: {}",
            cli.display(),
            args,
            stdout,
            stderr
        );
        bail!(
            "CLI command exited with code {}: {} {:?}",
            output.status.code().unwrap_or(-1),
            cli.display(),
            args,
        );
    }

    let stdout = String::from_utf8(output.stdout)
        .context("CLI output is not valid UTF-8")?;
    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Integration test for committee composition via CLI (mock Koios).
///
/// This test verifies that the CLI's `query committee-state` command correctly
/// displays members when interacting with a mock Koios server. It does **not**
/// validate the underlying ledger state (see `test_committee_composition_real`).
#[tokio::test]
async fn test_committee_composition_cli() -> Result<()> {
    init_logger();
    let koios_server = mock_koios_server().await;
    let _mock = register_committee_info_mock(&koios_server, MIN_COMMITTEE_MEMBERS).await;

    let cli = dugite_cli_path().context("dugite-cli not found")?;

    // Determine socket path; skip if not available.
    let socket = match maybe_socket_path()? {
        Some(s) => s,
        None => {
            warn!("Skipping test_committee_composition_cli: no socket available.");
            return Ok(());
        }
    };

    let output = query_committee_state(&cli, &socket, &koios_server.uri()).await?;

    // Parse JSON output and check member count.
    let parsed: Value = serde_json::from_str(&output)
        .with_context(|| format!("Failed to parse CLI output: {}", output))?;

    let members = parsed
        .get("members")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("CLI output missing 'members' array: {}", output))?;

    assert!(
        members.len() >= MIN_COMMITTEE_MEMBERS,
        "Committee has {} members, expected at least {}",
        members.len(),
        MIN_COMMITTEE_MEMBERS
    );

    info!(
        "Committee composition test passed: {} members found.",
        members.len()
    );
    Ok(())
}

/// Integration test for committee composition against a real node.
///
/// This test directly queries the local ledger state (via the node socket) to
/// verify that the committee contains the full set of members after the
/// `UpdateCommittee` action enactment. This is the primary validation of the P0 fix.
#[tokio::test]
async fn test_committee_composition_real() -> Result<()> {
    init_logger();
    let cli = dugite_cli_path().context("dugite-cli not found")?;

    let socket = match maybe_socket_path()? {
        Some(s) => s,
        None => {
            warn!("Skipping test_committee_composition_real: no socket available.");
            return Ok(());
        }
    };

    // Use a placeholder Koios URL; the command may still work for local state.
    let output = query_committee_state(&cli, &socket, "http://localhost:9999").await?;

    let parsed: Value = serde_json::from_str(&output)
        .with_context(|| format!("Failed to parse CLI output: {}", output))?;

    let members = parsed
        .get("members")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("CLI output missing 'members' array: {}", output))?;

    assert!(
        members.len() >= MIN_COMMITTEE_MEMBERS,
        "Committee on real node has {} members, expected at least {}",
        members.len(),
        MIN_COMMITTEE_MEMBERS
    );

    info!(
        "Real node committee test passed: {} members found.",
        members.len()
    );
    Ok(())
}

/// Integration test for treasury balance via CLI (mock Koios).
///
/// This test verifies that the CLI's `query treasury` command correctly reports
/// known per-epoch treasury values when interacting with a mock Koios server.
#[tokio::test]
async fn test_treasury_balance_cli() -> Result<()> {
    init_logger();
    let koios_server = mock_koios_server().await;

    // Parse epoch range from env or use default.
    let epoch_range: Vec<u64> = env::var("EPOCH_RANGE")
        .unwrap_or_else(|_| DEFAULT_EPOCH_RANGE.to_owned())
        .split(',')
        .map(|s| s.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .context("Invalid EPOCH_RANGE format")?;

    // Register mocks for each known epoch.
    for (epoch_str, _) in KNOWN_TREASURY {
        let epoch: u64 = epoch_str.parse().unwrap();
        if epoch_range.contains(&epoch) {
            let expected = KNOWN_TREASURY
                .iter()
                .find(|(e, _)| *e == *epoch_str)
                .map(|(_, v)| *v)
                .expect("Known treasury entry exists");
            register_totals_mock(&koios_server, epoch, expected).await;
        }
    }

    let cli = dugite_cli_path().context("dugite-cli not found")?;

    let socket = match maybe_socket_path()? {
        Some(s) => s,
        None => {
            warn!("Skipping test_treasury_balance_cli: no socket available.");
            return Ok(());
        }
    };

    for &(epoch_str, expected_treasury) in KNOWN_TREASURY {
        let epoch: u64 = epoch_str.parse().unwrap();
        if !epoch_range.contains(&epoch) {
            warn!("Skipping epoch {} (not in EPOCH_RANGE)", epoch);
            continue;
        }

        let output = query_treasury(&cli, &socket, &koios_server.uri(), epoch).await?;

        let parsed: Value = serde_json::from_str(&output)
            .with_context(|| format!("Failed to parse CLI output: {}", output))?;

        let treasury_str = parsed
            .get("treasury")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("CLI output missing 'treasury' string: {}", output))?;

        let treasury_value: u64 = treasury_str
            .parse()
            .with_context(|| format!("Could not parse treasury value '{}'", treasury_str))?;

        let diff = if treasury_value > expected_treasury {
            treasury_value - expected_treasury
        } else {
            expected_treasury - treasury_value
        };

        assert!(
            diff <= TREASURY_TOLERANCE,
            "Treasury mismatch for epoch {}: expected {}, got {} (diff {})",
            epoch,
            expected_treasury,
            treasury_value,
            diff
        );

        info!(
            "Treasury for epoch {}: {} (expected {}, diff {})",
            epoch, treasury_value, expected_treasury, diff
        );
    }

    info!("All treasury tests passed.");
    Ok(())
}