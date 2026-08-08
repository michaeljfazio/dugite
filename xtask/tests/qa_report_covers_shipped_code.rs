//! A QA report must have been generated from the code being shipped.
//!
//! `reports/devnet-validate/vX.Y.Z.json` records the `git_rev` it ran against,
//! and nothing checked it. That is how a stale report survives: the generator
//! writes `./report.json`, NOT the versioned path, so unless someone copies it
//! across, the tracked file silently keeps an OLDER run's numbers.
//!
//! It happened in this repo on 2026-08-08. `v2.8.0.json` sat at a 09:34 run
//! that predated the entire pulser programme — #1072's consensus fix, the
//! credential-major reward fold, per-block pulsing, the frozen `fvTotalStake`,
//! the `costModels` framing fix. Its summary (565 blocks / 211 zoo pass) reads
//! perfectly plausibly next to the real one (669 / 190); there is nothing in
//! the numbers to tell them apart. It was caught only by comparing timestamps
//! by hand.
//!
//! This is the same failure family as #945, where `cli_parity` was recorded as
//! all-zero in EVERY published report because the generator indexed a broken
//! header — output that looks like a result and is not one.
//!
//! The check: no commit touching `crates/` may be newer than the report's
//! `git_rev`. If one is, the gate did not exercise the shipped code, whatever
//! its verdict says.

use std::process::Command;

/// Always `-C <repo root>`. Tests run with CWD = the PACKAGE root (`xtask/`),
/// so a bare `git log -- crates` finds nothing there and returns empty — which
/// sent this very test down its `is_empty()` early-return and made it pass
/// vacuously against a deliberately backdated report. The guard against
/// checks-that-measure-nothing was itself one.
fn sh(args: &[&str]) -> String {
    let root = repo_root();
    let mut full: Vec<&str> = vec!["-C", root.to_str().expect("utf-8 root")];
    full.extend_from_slice(args);
    let out = Command::new("git")
        .args(&full)
        .output()
        .unwrap_or_else(|e| panic!("git {full:?}: {e}"));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn the_qa_report_was_generated_from_the_shipped_code() {
    let root = repo_root();
    let version = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("workspace Cargo.toml")
        .lines()
        .find_map(|l| {
            l.strip_prefix("version = \"")
                .and_then(|r| r.strip_suffix('"'))
                .map(str::to_owned)
        })
        .expect("workspace version");

    let report = root.join(format!("reports/devnet-validate/v{version}.json"));
    if !report.exists() {
        // A release with no report yet is a normal mid-development state; this
        // test guards against a report that LIES, not against its absence.
        eprintln!("no report at {} yet — nothing to check", report.display());
        return;
    }

    let body = std::fs::read_to_string(&report).expect("read report");
    let rev = body
        .split("\"git_rev\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .expect("report records git_rev")
        .to_string();

    // Is the report's revision reachable at all? A rev from a rebased or
    // dropped branch cannot have tested anything that is here now.
    if sh(&["cat-file", "-t", &rev]) != "commit" {
        panic!(
            "QA report v{version} names git_rev {rev}, which is not a commit in \
             this repository — it cannot describe the code being shipped"
        );
    }

    // The newest commit that touched shipped code.
    let newest_code = sh(&["log", "-1", "--format=%H", "--", "crates"]);
    assert!(
        !newest_code.is_empty(),
        "found no commit touching crates/ — this check cannot run, and an \
         unrunnable check must fail rather than pass silently"
    );

    // `merge-base --is-ancestor A B` succeeds when A is an ancestor of B.
    // The code commit must be an ancestor of (or equal to) the report's rev.
    let root = repo_root();
    let ok = Command::new("git")
        .args([
            "-C",
            root.to_str().expect("utf-8 root"),
            "merge-base",
            "--is-ancestor",
            &newest_code,
            &rev,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    assert!(
        ok,
        "STALE QA REPORT.\n\
         \n\
         reports/devnet-validate/v{version}.json was generated at {rev},\n\
         but crates/ has since changed at {newest_code}.\n\
         \n\
         The gate did not exercise the code being shipped, whatever verdict the\n\
         report carries. Its numbers will look entirely plausible — that is the\n\
         problem, and it is why this is a test and not a code review item.\n\
         \n\
         Re-run the gate and copy the generator's ./report.json to the versioned\n\
         path; the generator does NOT write there itself."
    );
}
