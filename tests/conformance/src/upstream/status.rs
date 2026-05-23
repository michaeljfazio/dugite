//! Always-running banner test for upstream fixture status.
//!
//! This test always executes (regardless of fixture presence) and reports
//! whether fixtures are available and fresh. In `DUGITE_REQUIRE_UPSTREAM=1`
//! mode it fails hard when fixtures are missing or stale.
//!
//! Every other upstream test calls `check_area()` which silently skips
//! in dev mode and hard-panics in REQUIRE mode.

use super::fixtures;

pub fn check_and_report() {
    let root = fixtures::fixture_root();
    let present = root.exists();
    let sentinel_ok = fixtures::sentinel_matches();
    let require = fixtures::require_mode();

    if present && sentinel_ok {
        eprintln!(
            "[upstream-conformance] Fixtures present and sentinel matches ({})",
            root.display()
        );
    } else if present && !sentinel_ok {
        let msg = format!(
            "[upstream-conformance] Fixtures present but sentinel mismatch — \
             manifest.toml changed since last download. \
             Run: cargo xtask download-upstream-fixtures"
        );
        if require {
            panic!("{msg}");
        } else {
            eprintln!("{msg}");
        }
    } else {
        let msg = format!(
            "[upstream-conformance] Fixtures missing at {}. \
             Run: cargo xtask download-upstream-fixtures",
            root.display()
        );
        if require {
            panic!("{msg}");
        } else {
            eprintln!("{msg}");
        }
    }
}
