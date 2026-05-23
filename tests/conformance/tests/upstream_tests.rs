//! Upstream conformance test suite.
//!
//! Run with:
//!   DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance \
//!     --features upstream-conformance --test upstream_tests
//!
//! In development (without DUGITE_REQUIRE_UPSTREAM) the tests silently skip
//! when fixtures are not present. CI sets DUGITE_REQUIRE_UPSTREAM=1, making
//! missing fixtures a hard failure.

use dugite_conformance::upstream::{fixtures, status};

// ── Status banner (always runs) ───────────────────────────────────────────────

#[test]
fn upstream_fixtures_status() {
    status::check_and_report();
}

// ── ouroboros-consensus ───────────────────────────────────────────────────────

#[test]
fn ouroboros_consensus_golden_decodes() {
    let Some(dir) = fixtures::check_area("ouroboros-consensus") else {
        return;
    };
    dugite_conformance::upstream::ouroboros_consensus::run_all_checks(&dir);
}

// ── cardano-ledger ────────────────────────────────────────────────────────────

#[test]
fn cardano_ledger_golden_decodes() {
    let Some(dir) = fixtures::check_area("cardano-ledger") else {
        return;
    };
    dugite_conformance::upstream::cardano_ledger::run_all_checks(&dir);
}

// ── cardano-node ──────────────────────────────────────────────────────────────

#[test]
fn cardano_node_genesis_decodes() {
    let Some(dir) = fixtures::check_area("cardano-node") else {
        return;
    };
    dugite_conformance::upstream::cardano_node::run_all_checks(&dir);
}

// ── cardano-ledger strict typed PParams ──────────────────────────────────────

#[test]
fn cardano_ledger_pparams_typed_decodes() {
    let Some(dir) = fixtures::check_area("cardano-ledger") else {
        return;
    };
    dugite_conformance::upstream::cardano_ledger_pparams_typed::run_all_checks(&dir);
}

// ── cardano-ledger CDDL validation ───────────────────────────────────────────

#[test]
fn cardano_ledger_cddl_validates() {
    let Some(ledger_dir) = fixtures::check_area("cardano-ledger") else {
        return;
    };
    let Some(ouroboros_dir) = fixtures::check_area("ouroboros-consensus") else {
        return;
    };
    dugite_conformance::upstream::cardano_ledger_cddl::run_all_checks(&ledger_dir, &ouroboros_dir);
}

// ── ledger-rules ImpSpec replay (Phase 4) ────────────────────────────────────

#[test]
fn ledger_rules_imp_spec_replay() {
    let Some(dir) = fixtures::check_area("ledger-rules") else {
        return;
    };
    dugite_conformance::upstream::ledger_rules_replay::run_all_checks(&dir);
}

// ── cardano-base VRF/KES crypto vectors (Phase 5) ────────────────────────────

#[test]
fn cardano_base_crypto_vectors() {
    let Some(dir) = fixtures::check_area("cardano-base") else {
        return;
    };
    dugite_conformance::upstream::cardano_base::run_all_checks(&dir);
}

// ── Mithril certificate fixtures (Phase 6) ───────────────────────────────────

#[test]
fn mithril_certificate_fixtures() {
    let Some(dir) = fixtures::check_area("mithril") else {
        return;
    };
    dugite_conformance::upstream::mithril::run_all_checks(&dir);
}
