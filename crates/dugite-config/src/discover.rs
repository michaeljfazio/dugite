//! Discovery of running `dugite-node` instances on the local host.
//!
//! Used by `dugite-config edit` when invoked without an explicit `<config_file>`
//! argument. Enumerates processes via `sysinfo` (cross-platform), filters to
//! `dugite-node run …` invocations, and parses each argv for `--config`,
//! `--port`, and `--socket-path` so the editor can attach without the operator
//! having to remember paths or manage a separate PID file.
//!
//! Cardano-node instances are deliberately not surfaced — see the rationale in
//! the issue body for `#490`. Operators who want to edit a `cardano-node`
//! config file can still do so by passing `--config <path>` explicitly.

use std::path::{Path, PathBuf};

/// Compiled-in default for `dugite-node run --config` when the flag is absent.
/// Mirrors `crates/dugite-node/src/main.rs` and is updated manually if the
/// upstream default changes.
pub const DUGITE_NODE_DEFAULT_CONFIG: &str = "config/mainnet/config.json";

/// Whether a discovered node's config path was specified on the command line
/// or fell back to the binary's compiled-in default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// Came from `--config` (or `--config=...`).
    Explicit,
    /// `--config` was not on argv; we fell back to [`DUGITE_NODE_DEFAULT_CONFIG`].
    Default,
}

/// State of a discovered node's config file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigStatus {
    /// File exists and was canonicalized.
    Ok,
    /// Path was derived but the file does not exist on disk (moved/deleted).
    Missing,
    /// Could not read the process's argv or cwd (other-user process, sandbox).
    PermissionDenied,
}

/// A running `dugite-node` instance discovered on the local host.
#[derive(Debug, Clone)]
pub struct RunningNode {
    pub pid: u32,
    pub config_path: PathBuf,
    pub config_source: ConfigSource,
    pub port: Option<u16>,
    pub socket: Option<PathBuf>,
    pub status: ConfigStatus,
}

impl RunningNode {
    /// True if the node can be selected by the operator (file exists).
    pub fn is_selectable(&self) -> bool {
        matches!(self.status, ConfigStatus::Ok)
    }
}

/// Subset of argv flags relevant to discovery. Returned only when argv
/// represents a `<binary> run …` invocation.
#[derive(Debug, PartialEq, Eq)]
struct ParsedRunArgs {
    config: Option<String>,
    port: Option<u16>,
    socket: Option<String>,
}

/// Parse a process's argv. Returns `Some` only if `run` appears as a token
/// before any `--` separator; otherwise `None` (the process is `mithril-import`,
/// `db-info`, etc., and has no `--config` to extract).
///
/// Behaviour notes:
/// * `--config=<path>` and `--config <path>` are both accepted.
/// * If `--config` appears more than once, the last occurrence wins
///   (matches clap's `ArgAction::Set` default).
/// * `--config` followed by another flag (e.g. `--config --port 3001`) is
///   treated as no value — defensive against malformed argv.
/// * Scanning stops at `--`; tokens past it are positional.
fn parse_run_argv(argv: &[String]) -> Option<ParsedRunArgs> {
    // Locate `run` and collect the remaining flag tokens (excluding `run`
    // itself and anything past `--`). argv[0] is the binary path.
    let mut run_seen = false;
    let mut flags: Vec<&str> = Vec::with_capacity(argv.len());
    for arg in argv.iter().skip(1) {
        if arg == "--" {
            break;
        }
        if arg == "run" {
            run_seen = true;
            continue;
        }
        flags.push(arg.as_str());
    }
    if !run_seen {
        return None;
    }

    let mut config: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut socket: Option<String> = None;

    let mut i = 0;
    while i < flags.len() {
        let tok = flags[i];

        if let Some(rest) = tok.strip_prefix("--config=") {
            config = Some(rest.to_string());
        } else if let Some(rest) = tok.strip_prefix("--port=") {
            if let Ok(p) = rest.parse::<u16>() {
                port = Some(p);
            }
        } else if let Some(rest) = tok.strip_prefix("--socket-path=") {
            socket = Some(rest.to_string());
        } else if tok == "--config" {
            if let Some(next) = flags.get(i + 1) {
                if !next.starts_with('-') {
                    config = Some((*next).to_string());
                    i += 1;
                }
            }
        } else if tok == "--port" {
            if let Some(next) = flags.get(i + 1) {
                if !next.starts_with('-') {
                    if let Ok(p) = next.parse::<u16>() {
                        port = Some(p);
                    }
                    i += 1;
                }
            }
        } else if tok == "--socket-path" {
            if let Some(next) = flags.get(i + 1) {
                if !next.starts_with('-') {
                    socket = Some((*next).to_string());
                    i += 1;
                }
            }
        }

        i += 1;
    }

    Some(ParsedRunArgs {
        config,
        port,
        socket,
    })
}

/// Resolve a config path string against the process's working directory.
///
/// Absolute paths are taken as-is; relative paths are joined onto `cwd`.
/// `canonicalize` is attempted so the editor sees a stable absolute path; if
/// it fails (file moved/deleted/network mount gone) the un-canonicalized path
/// is returned with [`ConfigStatus::Missing`] so the operator sees the row
/// in the selector with a clear marker rather than the row vanishing.
fn resolve_config(raw: &str, cwd: &Path) -> (PathBuf, ConfigStatus) {
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    match candidate.canonicalize() {
        Ok(canon) => (canon, ConfigStatus::Ok),
        Err(_) => (candidate, ConfigStatus::Missing),
    }
}

/// Match a process exe basename against `dugite-node`, case-insensitive,
/// trimming `.exe` so Windows builds match too.
fn is_dugite_node_exe(exe: Option<&Path>) -> bool {
    exe.and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_lowercase())
        .map(|s| s.trim_end_matches(".exe") == "dugite-node")
        .unwrap_or(false)
}

/// Build a [`RunningNode`] from already-extracted process info. Pulled out of
/// [`discover_dugite_nodes`] so it can be unit-tested without spawning real
/// processes.
fn build_running_node(pid: u32, parsed: &ParsedRunArgs, cwd: Option<&Path>) -> RunningNode {
    let (config_path, config_source, status) = match (parsed.config.as_deref(), cwd) {
        (Some(raw), Some(cwd)) => {
            let (path, status) = resolve_config(raw, cwd);
            (path, ConfigSource::Explicit, status)
        }
        (None, Some(cwd)) => {
            let (path, status) = resolve_config(DUGITE_NODE_DEFAULT_CONFIG, cwd);
            (path, ConfigSource::Default, status)
        }
        (raw, None) => {
            // Without cwd we cannot resolve a relative path. Surface the row
            // with a permission-denied marker rather than dropping it.
            let s = raw
                .map(str::to_string)
                .unwrap_or_else(|| DUGITE_NODE_DEFAULT_CONFIG.to_string());
            let source = if parsed.config.is_some() {
                ConfigSource::Explicit
            } else {
                ConfigSource::Default
            };
            (PathBuf::from(s), source, ConfigStatus::PermissionDenied)
        }
    };

    RunningNode {
        pid,
        config_path,
        config_source,
        port: parsed.port,
        socket: parsed.socket.clone().map(PathBuf::from),
        status,
    }
}

/// Enumerate running `dugite-node` instances on the local host.
///
/// Result is sorted by PID for stable display. Empty when nothing matches —
/// this is *not* an error condition (handled by the caller).
pub fn discover_dugite_nodes() -> Vec<RunningNode> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

    let refresh = RefreshKind::nothing().with_processes(
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    let mut sys = System::new_with_specifics(refresh);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh.processes().unwrap());

    let mut nodes = Vec::new();
    for proc in sys.processes().values() {
        if !is_dugite_node_exe(proc.exe()) {
            continue;
        }

        let argv: Vec<String> = proc
            .cmd()
            .iter()
            .map(|o| o.to_string_lossy().into_owned())
            .collect();

        let Some(parsed) = parse_run_argv(&argv) else {
            continue;
        };

        let pid = proc.pid().as_u32();
        nodes.push(build_running_node(pid, &parsed, proc.cwd()));
    }

    nodes.sort_by_key(|n| n.pid);
    nodes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    // ----- parse_run_argv -----

    #[test]
    fn parse_returns_none_for_non_run_subcommand() {
        let v = argv(&["/opt/dugite-node", "mithril-import", "--network-magic", "2"]);
        assert!(parse_run_argv(&v).is_none());
    }

    #[test]
    fn parse_returns_none_when_run_appears_only_after_dash_dash() {
        let v = argv(&["dugite-node", "--", "run"]);
        assert!(parse_run_argv(&v).is_none());
    }

    #[test]
    fn parse_run_with_no_flags_yields_all_none() {
        let v = argv(&["dugite-node", "run"]);
        let p = parse_run_argv(&v).expect("run invocation");
        assert_eq!(p.config, None);
        assert_eq!(p.port, None);
        assert_eq!(p.socket, None);
    }

    #[test]
    fn parse_config_two_token_form() {
        let v = argv(&["dugite-node", "run", "--config", "/etc/dugite/preview.json"]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config.as_deref(), Some("/etc/dugite/preview.json"));
    }

    #[test]
    fn parse_config_equals_form() {
        let v = argv(&["dugite-node", "run", "--config=preview.json"]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config.as_deref(), Some("preview.json"));
    }

    #[test]
    fn parse_config_followed_by_flag_takes_no_value() {
        // Defensive: malformed argv where --config is followed by another flag.
        let v = argv(&["dugite-node", "run", "--config", "--port", "3001"]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config, None);
        assert_eq!(p.port, Some(3001));
    }

    #[test]
    fn parse_duplicate_config_last_wins() {
        let v = argv(&[
            "dugite-node",
            "run",
            "--config",
            "first.json",
            "--config=second.json",
            "--config",
            "third.json",
        ]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config.as_deref(), Some("third.json"));
    }

    #[test]
    fn parse_stops_at_dash_dash_separator() {
        let v = argv(&["dugite-node", "run", "--", "--config", "after-dash.json"]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config, None);
    }

    #[test]
    fn parse_port_and_socket_two_token_form() {
        let v = argv(&[
            "dugite-node",
            "run",
            "--port",
            "3001",
            "--socket-path",
            "/tmp/node.sock",
        ]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.port, Some(3001));
        assert_eq!(p.socket.as_deref(), Some("/tmp/node.sock"));
    }

    #[test]
    fn parse_port_and_socket_equals_form() {
        let v = argv(&[
            "dugite-node",
            "run",
            "--port=3001",
            "--socket-path=/tmp/n.sock",
        ]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.port, Some(3001));
        assert_eq!(p.socket.as_deref(), Some("/tmp/n.sock"));
    }

    #[test]
    fn parse_invalid_port_silently_dropped() {
        let v = argv(&["dugite-node", "run", "--port", "not-a-number"]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.port, None);
    }

    #[test]
    fn parse_full_realistic_argv() {
        let v = argv(&[
            "/Users/op/dugite/target/release/dugite-node",
            "run",
            "--config",
            "config/preview/config.json",
            "--topology",
            "config/preview/topology.json",
            "--database-path",
            "./db-preview",
            "--socket-path",
            "./node.sock",
            "--host-addr",
            "0.0.0.0",
            "--port",
            "3001",
        ]);
        let p = parse_run_argv(&v).unwrap();
        assert_eq!(p.config.as_deref(), Some("config/preview/config.json"));
        assert_eq!(p.port, Some(3001));
        assert_eq!(p.socket.as_deref(), Some("./node.sock"));
    }

    // ----- resolve_config -----

    #[test]
    fn resolve_absolute_existing_canonicalizes_to_ok() {
        let dir = TempDir::new().unwrap();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, b"{}").unwrap();
        let (path, status) = resolve_config(cfg.to_str().unwrap(), Path::new("/"));
        assert_eq!(status, ConfigStatus::Ok);
        assert!(path.is_absolute());
    }

    #[test]
    fn resolve_relative_joins_against_cwd() {
        let dir = TempDir::new().unwrap();
        let cfg = dir.path().join("rel.json");
        std::fs::write(&cfg, b"{}").unwrap();
        let (path, status) = resolve_config("rel.json", dir.path());
        assert_eq!(status, ConfigStatus::Ok);
        assert!(path.ends_with("rel.json"));
    }

    #[test]
    fn resolve_missing_path_returns_missing_status() {
        let dir = TempDir::new().unwrap();
        let (path, status) = resolve_config("nope.json", dir.path());
        assert_eq!(status, ConfigStatus::Missing);
        // Path is preserved (not canonicalized) so the operator can see what
        // we tried.
        assert!(path.ends_with("nope.json"));
    }

    // ----- build_running_node -----

    #[test]
    fn build_node_explicit_config_with_existing_file() {
        let dir = TempDir::new().unwrap();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, b"{}").unwrap();
        let parsed = ParsedRunArgs {
            config: Some(cfg.to_str().unwrap().to_string()),
            port: Some(3001),
            socket: Some("/tmp/s.sock".to_string()),
        };
        let n = build_running_node(42, &parsed, Some(dir.path()));
        assert_eq!(n.pid, 42);
        assert_eq!(n.config_source, ConfigSource::Explicit);
        assert_eq!(n.status, ConfigStatus::Ok);
        assert_eq!(n.port, Some(3001));
        assert_eq!(n.socket, Some(PathBuf::from("/tmp/s.sock")));
        assert!(n.is_selectable());
    }

    #[test]
    fn build_node_falls_back_to_default_when_config_absent() {
        let dir = TempDir::new().unwrap();
        let parsed = ParsedRunArgs {
            config: None,
            port: None,
            socket: None,
        };
        let n = build_running_node(7, &parsed, Some(dir.path()));
        assert_eq!(n.config_source, ConfigSource::Default);
        // Default file doesn't exist in our temp dir, so status is Missing.
        assert_eq!(n.status, ConfigStatus::Missing);
        assert!(n.config_path.ends_with("config/mainnet/config.json"));
        assert!(!n.is_selectable());
    }

    #[test]
    fn build_node_no_cwd_yields_permission_denied_status() {
        let parsed = ParsedRunArgs {
            config: Some("rel.json".to_string()),
            port: None,
            socket: None,
        };
        let n = build_running_node(99, &parsed, None);
        assert_eq!(n.status, ConfigStatus::PermissionDenied);
        assert_eq!(n.config_source, ConfigSource::Explicit);
        assert_eq!(n.config_path, PathBuf::from("rel.json"));
    }

    // ----- is_dugite_node_exe -----

    #[test]
    fn matches_dugite_node_basename() {
        assert!(is_dugite_node_exe(Some(Path::new(
            "/opt/dugite/target/release/dugite-node"
        ))));
        assert!(is_dugite_node_exe(Some(Path::new("dugite-node"))));
    }

    #[test]
    fn matches_dugite_node_exe_on_windows() {
        assert!(is_dugite_node_exe(Some(Path::new(
            "C:/bin/Dugite-Node.exe"
        ))));
    }

    #[test]
    fn rejects_other_binaries() {
        assert!(!is_dugite_node_exe(Some(Path::new("cardano-node"))));
        assert!(!is_dugite_node_exe(Some(Path::new(
            "/opt/dugite-node-helper"
        ))));
        assert!(!is_dugite_node_exe(None));
    }
}
