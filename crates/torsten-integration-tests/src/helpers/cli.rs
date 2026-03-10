use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Result of running a CLI command.
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Resolve the path to the torsten-cli binary.
pub fn cli_path() -> PathBuf {
    if let Ok(path) = env::var("TORSTEN_CLI_PATH") {
        return PathBuf::from(path);
    }
    // Walk up from the crate directory to find the workspace root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let release = workspace_root.join("target/release/torsten-cli");
    if release.exists() {
        return release;
    }
    let debug = workspace_root.join("target/debug/torsten-cli");
    if debug.exists() {
        return debug;
    }
    // Fallback: hope it's on PATH
    PathBuf::from("torsten-cli")
}

/// Resolve the path to the torsten-node binary.
pub fn node_path() -> PathBuf {
    if let Ok(path) = env::var("TORSTEN_NODE_PATH") {
        return PathBuf::from(path);
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let release = workspace_root.join("target/release/torsten-node");
    if release.exists() {
        return release;
    }
    let debug = workspace_root.join("target/debug/torsten-node");
    if debug.exists() {
        return debug;
    }
    PathBuf::from("torsten-node")
}

/// Run torsten-cli with the given arguments.
pub fn run_cli(args: &[&str]) -> CmdOutput {
    let path = cli_path();
    let output: Output = Command::new(&path)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run torsten-cli at {:?}: {}", path, e));

    CmdOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

/// Run torsten-cli, assert success, return stdout.
pub fn run_cli_ok(args: &[&str]) -> String {
    let result = run_cli(args);
    assert!(
        result.success(),
        "torsten-cli {:?} failed (exit {})\nstdout: {}\nstderr: {}",
        args,
        result.exit_code,
        result.stdout,
        result.stderr,
    );
    result.stdout
}

/// Run torsten-cli, assert failure, return stderr.
pub fn run_cli_fail(args: &[&str]) -> String {
    let result = run_cli(args);
    assert!(
        !result.success(),
        "torsten-cli {:?} unexpectedly succeeded\nstdout: {}",
        args,
        result.stdout,
    );
    result.stderr
}

/// Get the socket path for integration tests. Returns None if not set.
pub fn integration_socket() -> Option<String> {
    env::var("TORSTEN_INTEGRATION_SOCKET").ok()
}

/// Get the test keys directory. Returns None if not set.
pub fn test_keys_dir() -> Option<String> {
    env::var("TORSTEN_TEST_KEYS").ok()
}
