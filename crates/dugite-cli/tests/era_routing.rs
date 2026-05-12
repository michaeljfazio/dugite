//! Tests for era-prefixed subcommand routing (#321).
//!
//! cardano-cli supports `cardano-cli conway transaction build`,
//! `cardano-cli latest query tip`, etc. dugite-cli mirrors this surface
//! so existing cardano-cli scripts work unchanged. All era prefixes
//! currently route to the same handlers (dugite is era-agnostic at the
//! CLI surface today), so we mainly verify that argument parsing and
//! dispatch succeed.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_dugite-cli");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to invoke dugite-cli")
}

fn assert_help_ok(args: &[&str]) {
    let out = run(args);
    assert!(
        out.status.success(),
        "{:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "no Usage: in help for {args:?}");
}

#[test]
fn top_level_help_lists_era_aliases() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    for era in [
        "conway", "babbage", "alonzo", "mary", "allegra", "shelley", "latest",
    ] {
        assert!(s.contains(era), "era `{era}` missing from --help");
    }
}

#[test]
fn conway_query_help_succeeds() {
    assert_help_ok(&["conway", "query", "--help"]);
}

#[test]
fn latest_query_tip_help_succeeds() {
    assert_help_ok(&["latest", "query", "tip", "--help"]);
}

#[test]
fn babbage_transaction_build_help_succeeds() {
    assert_help_ok(&["babbage", "transaction", "build", "--help"]);
}

#[test]
fn shelley_address_key_gen_help_succeeds() {
    assert_help_ok(&["shelley", "address", "key-gen", "--help"]);
}

#[test]
fn allegra_key_help_succeeds() {
    assert_help_ok(&["allegra", "key", "--help"]);
}

#[test]
fn mary_stake_pool_help_succeeds() {
    assert_help_ok(&["mary", "stake-pool", "--help"]);
}

#[test]
fn alonzo_genesis_help_succeeds() {
    assert_help_ok(&["alonzo", "genesis", "--help"]);
}

#[test]
fn era_prefix_help_matches_flat_help() {
    // The subcommand list under `latest` must mirror the flat top-level
    // subcommand list — same commands, same descriptions.
    let flat = run(&["transaction", "--help"]);
    let latest = run(&["latest", "transaction", "--help"]);
    assert!(flat.status.success() && latest.status.success());

    // Strip the `Usage:` line (which contains the differing prefix path)
    // and compare the rest of the body.
    let strip_usage = |s: &str| -> String {
        s.lines()
            .filter(|l| !l.trim_start().starts_with("Usage:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let flat_body = strip_usage(&String::from_utf8_lossy(&flat.stdout));
    let latest_body = strip_usage(&String::from_utf8_lossy(&latest.stdout));
    assert_eq!(
        flat_body, latest_body,
        "era-prefixed help diverges from flat help"
    );
}

#[test]
fn unknown_era_is_rejected() {
    let out = run(&["byron", "query", "tip"]);
    assert!(!out.status.success(), "byron should not be a routable era");
}

#[test]
fn flat_top_level_still_works() {
    // Backward compatibility: existing flat commands continue to function.
    assert_help_ok(&["query", "tip", "--help"]);
    assert_help_ok(&["transaction", "build", "--help"]);
}
