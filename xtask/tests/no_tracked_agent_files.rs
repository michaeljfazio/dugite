//! AI agent files are local-only and must never be tracked.
//!
//! Agent memory, agent definitions, skills, plans/specs, QA reports,
//! `CLAUDE.md` and `.mcp.json` were purged from the whole history on
//! 2026-11-28. `.gitignore` keeps them out of a normal `git add`, but a
//! `git add -f` or an edited ignore rule would put them back silently, and the
//! pre-commit hook only runs where someone installed it. This test runs in CI.
//!
//! The same list is in `.githooks/pre-commit`; keep the two in step.

use std::process::Command;

/// Path prefixes (a trailing `/` means a directory) that must not be tracked.
const FORBIDDEN: &[&str] = &[
    ".claude/",
    ".remember/",
    "CLAUDE.md",
    ".mcp.json",
    "docs/superpowers/",
    "reports/",
];

fn is_forbidden(path: &str) -> bool {
    FORBIDDEN.iter().any(|f| {
        if f.ends_with('/') {
            path.starts_with(f)
        } else {
            path == *f
        }
    })
}

#[test]
fn forbidden_prefixes_match_what_they_claim() {
    assert!(is_forbidden(".claude/agents/tech-lead.md"));
    assert!(is_forbidden("CLAUDE.md"));
    assert!(is_forbidden("reports/devnet-validate/v2.8.0.json"));
    assert!(!is_forbidden("crates/dugite-node/CLAUDE.md.rs"));
    assert!(!is_forbidden("docs/src/reference/conformance.md"));
    assert!(!is_forbidden("scripts/reports.sh"));
}

#[test]
fn no_agent_files_are_tracked() {
    // Tests run with CWD = xtask/, so name the repo root explicitly; a bare
    // `git ls-files` there would list only xtask/ and pass vacuously.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    assert!(out.status.success(), "git ls-files failed");

    let listing = String::from_utf8_lossy(&out.stdout);
    let tracked: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
    assert!(
        tracked.contains(&"Cargo.toml"),
        "git ls-files did not list the workspace root; this check cannot run"
    );

    let bad: Vec<&str> = tracked.into_iter().filter(|p| is_forbidden(p)).collect();
    assert!(
        bad.is_empty(),
        "AI agent files are tracked; they are local-only and were purged from \
         history. Remove them with `git rm --cached`:\n  {}",
        bad.join("\n  ")
    );
}
