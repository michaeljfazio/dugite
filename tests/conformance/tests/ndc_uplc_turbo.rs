//! Tests of the `uplc-turbo` runner (#1114 Phase 2d): the report itself is produced by
//! `just ndc-report uplc-turbo` (the `ndc_report_uplc_turbo` runner in `ndc_reports.rs`, which
//! installs the counting allocator for the real run); these tests bound the runner's parts.
//!
//! The ones that need the fetched corpus skip (and say so) without it; with `DUGITE_NDC_REQUIRE=1`
//! (the just recipes set it) a missing corpus fails them.  They run in the debug profile: they
//! check what the runner measures and reports, not how fast it is.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use dugite_conformance::node_diversity::pins::corpora_dir;
use dugite_conformance::node_diversity::report::{
    uplc_turbo_report, validate_report, RunInfo, Scenario,
};
use dugite_conformance::node_diversity::uplc_turbo::{
    allocated_bytes, allocator_is_counting, benchmark_entry, load_programs, measure,
    schema_benchmarks, turbo_cost_models, CountingAllocator, Outcome, Program,
};
use dugite_uplc::redeemer_resolve::ScriptLanguage;

// This binary installs the counter itself so the allocator can be tested on its own.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const REQUIRE_ENV: &str = "DUGITE_NDC_REQUIRE";

fn corpus_or_skip(test: &str) -> Option<PathBuf> {
    let corpora = corpora_dir();
    if corpora.join("uplc-turbo").is_dir()
        && corpora
            .join("ndc/scenarios/uplc-turbo/schema.json")
            .is_file()
    {
        return Some(corpora);
    }
    if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
        panic!(
            "{test}: {REQUIRE_ENV}=1 but the uplc-turbo corpus is missing under {}; \
             run scripts/node-diversity/fetch-corpora.sh",
            corpora.display()
        );
    }
    eprintln!(
        "SKIPPED {test}: corpus not fetched under {}",
        corpora.display()
    );
    None
}

#[test]
fn the_counting_allocator_counts_the_requests_of_its_own_thread() {
    assert!(allocator_is_counting());
    let before = allocated_bytes();
    let v: Vec<u8> = Vec::with_capacity(10_000);
    std::hint::black_box(&v);
    assert!(allocated_bytes() - before >= 10_000);
    drop(v);
    // Freeing subtracts nothing: the counter is cumulative.
    assert!(allocated_bytes() - before >= 10_000);
    // A growing vector is counted at each reallocation.
    let before = allocated_bytes();
    let mut grown: Vec<u8> = Vec::new();
    for i in 0..100_000u32 {
        grown.push(i as u8);
    }
    std::hint::black_box(&grown);
    assert!(allocated_bytes() - before >= 100_000);
    // Another thread's allocations are not this thread's.
    let before = allocated_bytes();
    std::thread::spawn(|| {
        let big: Vec<u8> = vec![1; 5_000_000];
        std::hint::black_box(&big);
    })
    .join()
    .unwrap();
    assert!(allocated_bytes() - before < 100_000);
}

#[test]
fn the_corpus_is_exactly_the_schemas_benchmark_set() {
    let Some(corpora) = corpus_or_skip("the_corpus_is_exactly_the_schemas_benchmark_set") else {
        return;
    };
    let programs = load_programs(&corpora).unwrap();
    let mut have: Vec<&str> = programs.iter().map(|p| p.bench.as_str()).collect();
    have.sort_unstable();
    let mut want = schema_benchmarks(&corpora).unwrap();
    want.sort();
    assert_eq!(have, want.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(programs.len(), 3997);
    // The protocol version follows the epoch directory: this corpus is epochs 290..=521.
    assert!(programs.iter().all(|p| p.major_pv == 8 || p.major_pv == 9));
    assert!(programs
        .iter()
        .all(|p| (p.major_pv == 9) == (p.epoch >= 507)));
}

#[test]
fn every_protocol_version_and_language_has_its_own_cost_model() {
    let Some(corpora) =
        corpus_or_skip("every_protocol_version_and_language_has_its_own_cost_model")
    else {
        return;
    };
    let models = turbo_cost_models(&corpora).unwrap();
    for pv in [9u32, 10, 11] {
        for version in ["V1", "V2", "V3"] {
            assert!(
                models.contains_key(&(pv, version.to_string())),
                "{pv} {version}"
            );
        }
    }
    for version in ["V1", "V2"] {
        assert!(models.contains_key(&(8, version.to_string())));
    }
    // Variant A (protocol version 8) is not variant B (9 and 10).
    assert_ne!(
        models[&(8, "V1".to_string())].plutus_v1,
        models[&(9, "V1".to_string())].plutus_v1
    );
    assert_eq!(
        models[&(9, "V2".to_string())].plutus_v2,
        models[&(10, "V2".to_string())].plutus_v2
    );
}

#[test]
fn a_program_is_measured_and_its_budget_is_the_one_cardano_node_reports() {
    let Some(corpora) =
        corpus_or_skip("a_program_is_measured_and_its_budget_is_the_one_cardano_node_reports")
    else {
        return;
    };
    let models = turbo_cost_models(&corpora).unwrap();
    let programs = load_programs(&corpora).unwrap();
    // cardano-node's and Amaru's preview reports give this program (epoch 290, V1, PV8) the
    // budget 377,880,030 CPU / 1,426,968 memory.
    let first = programs
        .iter()
        .find(|p| p.bench.starts_with("13D1D007579D7041"))
        .expect("the first program of the corpus");
    match measure(first, &models).unwrap() {
        Outcome::Measured {
            cpu_us,
            mem_bytes,
            budget_cpu,
            budget_mem,
        } => {
            assert_eq!((budget_cpu, budget_mem), (377_880_030, 1_426_968));
            assert!(cpu_us > 0.0);
            assert!(mem_bytes > 0.0, "an evaluation allocates");
        }
        other => panic!("expected a measurement, got {other:?}"),
    }
    // A PlutusV3 program at protocol version 9 goes through the other language and variant.
    let v3 = programs
        .iter()
        .find(|p| p.language == ScriptLanguage::PlutusV3)
        .expect("the corpus has V3 programs");
    assert!(matches!(
        measure(v3, &models).unwrap(),
        Outcome::Measured { .. }
    ));
}

#[test]
fn a_program_that_does_not_evaluate_is_reported_with_null_budgets_and_still_validates() {
    let Some(corpora) = corpus_or_skip(
        "a_program_that_does_not_evaluate_is_reported_with_null_budgets_and_still_validates",
    ) else {
        return;
    };
    let models = turbo_cost_models(&corpora).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.flat");
    std::fs::write(&path, [0x43, 0xde, 0xad, 0xbf]).unwrap();
    let real = load_programs(&corpora).unwrap().remove(0);
    let broken = Program {
        path,
        ..real.clone()
    };
    let outcome = measure(&broken, &models).unwrap();
    assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
    let entry = benchmark_entry(&broken, &outcome);
    assert_eq!(entry.budget_cpu, None);
    assert_eq!(entry.budget_mem, None);
    assert_eq!(entry.mem_bytes, None);
    // The entry names a real benchmark, so the scenario's own schema accepts it.
    let run = RunInfo::new(
        "0123456789abcdef0123456789abcdef01234567",
        Utc.with_ymd_and_hms(2026, 11, 28, 12, 0, 0).unwrap(),
    );
    let report = uplc_turbo_report(&run, None, &[entry]);
    validate_report(&corpora, Scenario::UplcTurbo, &report).unwrap();
}
