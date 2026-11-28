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
    measure_with_runs, schema_benchmarks, turbo_cost_models, CountingAllocator, Outcome, Program,
    PV8_ARRAY_LEN,
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

/// `(protocol major, Plutus version, benchmark id, budget cpu, budget mem)`: the first, middle and
/// last program of each (protocol version, language) group of this corpus, with the budget
/// cardano-node's published report (`preview/cardano-node.json.zst` of the pinned NDC checkout)
/// gives it.  PV9 V3 has only ten programs.
const CARDANO_NODE_BUDGETS: &[(&str, &str, &str, u64, u64)] = &[
    ("8", "V1", "13D1D007579D7041A9BCCFE02930D63F8ACA8B7DFD25793C11D56B3EA3339956-0-E24268ACB75D8613BECF77027B810028D17C776FBDB8EC97B0A69B8E-v1.flat", 377880030, 1426968),
    ("8", "V1", "10CFE92651A02D67A9B1F6EA8217D3E62DF5E6A443587B05C95D3EF5BE8C4D34-0-75FB6C16FF4368305598B06E296698D8E4E0C2D37B0023A95A67E1D6-v1.flat", 1060568295, 4032394),
    ("8", "V1", "EED974342768D2E0E054EC6A10D3C4574E48C05DF758AA072AA635B97A2D196C-0-7E43587541A7F80326FFABB4977327E83F9FCB4097A471024C23FDCA-v1.flat", 1069566422, 4060796),
    ("8", "V2", "004DD3548C5FDB1383BBD81B748F2FD5D9B6BDE5BD96605B9618A1D129F4EFDD-0-9068A7A3F008803EDAC87AF1619860F2CDCDE40C26987325ACE138AD-v2.flat", 808437067, 2532454),
    ("8", "V2", "2EDD04B8E55AEF14F0D1111E3F6CE3DD340E5742C3F1B995F20AEC9E73F7048D-0-6587B5E44BDE31AA4E10DF2FFCCCE0FC57E59D428E224830F5D3EC86-v2.flat", 62460469, 175968),
    ("8", "V2", "F831BABA9FFBCBE4145C0B3C9A9CBEC06171F3A12E12AE6855F6ED2D8F49FE38-0-813BFCA1F722AE013693ED54B5C761BC38D1A96F94FC94EB8EE76725-v2.flat", 36036953, 96140),
    ("9", "V1", "00C3B14BC84080F7EFD822B5A1AF4956556A018412B5F3314651799B9B06E5CA-0-0237CC313756EBB5BCFC2728F7BDC6A8047B471220A305AA373B278A-v1.flat", 24687155, 77639),
    ("9", "V1", "0A44D662E90060E908A0121D69A2385A0C4880F83F4745B03C103C8FADC2E6DE-1-894F7DD2675DF01ABE57258D239DD6BA30BDB4CD3053746E23AF81F4-v1.flat", 112940914, 468430),
    ("9", "V1", "B5093F4449BA6473643D3428A7A06652F9FCDB8D2FB0CEB6436878C9EC2D5C2A-0-79FBE6CD147453C39F2F38372F2F8A09E2DA0B5CFC108939D4AA35CA-v1.flat", 40700787, 170544),
    ("9", "V2", "00771C2703E5A6743CC39D23E335238A503159304F3AAC67DCC59068E8839B5B-0-464EEEE89F05AFF787D40045AF2A40A83FD96C513197D32FBC54FF02-v2.flat", 41211206, 114720),
    ("9", "V2", "1CDC5B22B9C82A16CAE59C563E894F8A61E6150EA8C89953B5F81E6DCD733159-0-26AEA7E03A53E374C1D82C560A91B4147B8954D2EC4ECBD128E0C18C-v2.flat", 166086794, 565476),
    ("9", "V2", "ED03AA3828A5E7F8DC9A8E8C4F0B647E51639A6B48AEF02519ED73D142A7CA3D-2-7E6835AADDC8D9912B5785FC6B80F6030E26EAEB48DE6E84018ADB39-v2.flat", 21931983, 55089),
    ("9", "V3", "43711B35D4235861EAC117BB39EF084A841B05BBABA2EF281309E08B6EC3B8CF-0-5994B7767C73D331C79020D832C9C178497DDE3BCA6AAFD08BB41025-v3.flat", 280221223, 1226000),
    ("9", "V3", "5F3549654D586660616716C696ED691523073DD80A67EF647AA2046B80D4AABD-0-F4E8F9743FA0F750CA7D88D06664A87CBBB16E58E8B134B66DA0791A-v3.flat", 88362735, 260840),
    ("9", "V3", "50DA1DB1981581977B4FF36E005BC9234AB57F7DC6A94A161F4412B5C39913DF-0-172325BD1AC0ED6BAE98456245688921AA02FA82C1A972D9B7E3C43A-v3.flat", 88835052, 262206),
];

#[test]
fn budgets_equal_the_published_cardano_node_budgets_for_every_protocol_version_and_language() {
    let Some(corpora) = corpus_or_skip(
        "budgets_equal_the_published_cardano_node_budgets_for_every_protocol_version_and_language",
    ) else {
        return;
    };
    let models = turbo_cost_models(&corpora).unwrap();
    let programs = load_programs(&corpora).unwrap();
    let mut groups = std::collections::BTreeSet::new();
    for (pv, version, bench, cpu, mem) in CARDANO_NODE_BUDGETS {
        let program = programs
            .iter()
            .find(|p| p.bench == *bench)
            .unwrap_or_else(|| panic!("{bench} is not in the corpus"));
        assert_eq!(program.major_pv.to_string(), *pv, "{bench}");
        assert_eq!(program.plutus_version, *version, "{bench}");
        groups.insert((*pv, *version));
        match measure_with_runs(program, &models, 1).unwrap() {
            Outcome::Measured {
                budget_cpu,
                budget_mem,
                ..
            } => assert_eq!((budget_cpu, budget_mem), (*cpu, *mem), "{bench}"),
            other => panic!("{bench}: {other:?}"),
        }
    }
    // PV8 V1/V2 and PV9 V1/V2/V3 are every group the corpus has.
    assert_eq!(groups.len(), 5, "{groups:?}");
}

#[test]
fn the_pv8_cost_model_file_is_the_pinned_one_with_two_full_arrays() {
    let Some(corpora) =
        corpus_or_skip("the_pv8_cost_model_file_is_the_pinned_one_with_two_full_arrays")
    else {
        return;
    };
    // `turbo_cost_models` refuses a file whose SHA-256 is not the pinned one or whose arrays are
    // not 332 entries long; here the arrays it returned are counted too.
    let models = turbo_cost_models(&corpora).unwrap();
    for version in ["V1", "V2"] {
        let m = &models[&(8, version.to_string())];
        let array = if version == "V1" {
            &m.plutus_v1
        } else {
            &m.plutus_v2
        };
        assert_eq!(array.as_ref().unwrap().len(), PV8_ARRAY_LEN);
    }
    assert_eq!(PV8_ARRAY_LEN, 332);
}

/// The README's claim made a check: every program's budget, from one run, equals the budget in
/// cardano-node's and in Amaru's published report.  Slow in the debug profile, so it is ignored by
/// default; `just ndc-report uplc-turbo` runs it in release (`--run-ignored only`).
#[test]
#[ignore = "runs all 3,997 programs; just ndc-report uplc-turbo runs it in release"]
fn every_program_budget_equals_the_published_reports() {
    let Some(corpora) = corpus_or_skip("every_program_budget_equals_the_published_reports") else {
        return;
    };
    let reference = |node: &str| -> std::collections::BTreeMap<String, (u64, u64)> {
        let path = corpora.join(format!("ndc/scenarios/uplc-turbo/preview/{node}.json.zst"));
        let out = std::process::Command::new("zstd")
            .args(["-dc"])
            .arg(&path)
            .output()
            .expect("zstd is needed to read the published reports");
        assert!(out.status.success(), "zstd failed on {}", path.display());
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        report["benchmarks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["bench"].as_str().unwrap().to_string(),
                    (
                        b["budget_cpu"].as_u64().unwrap(),
                        b["budget_mem"].as_u64().unwrap(),
                    ),
                )
            })
            .collect()
    };
    let references = [
        ("cardano-node", reference("cardano-node")),
        ("amaru", reference("amaru")),
    ];
    let models = turbo_cost_models(&corpora).unwrap();
    let programs = load_programs(&corpora).unwrap();
    assert_eq!(programs.len(), 3997);
    let mut mismatches = Vec::new();
    for program in &programs {
        let mine = match measure_with_runs(program, &models, 1).unwrap() {
            Outcome::Measured {
                budget_cpu,
                budget_mem,
                ..
            } => (budget_cpu, budget_mem),
            other => {
                mismatches.push(format!("{}: {other:?}", program.bench));
                continue;
            }
        };
        for (node, budgets) in &references {
            if budgets.get(&program.bench) != Some(&mine) {
                mismatches.push(format!(
                    "{}: dugite {mine:?}, {node} {:?}",
                    program.bench,
                    budgets.get(&program.bench)
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} budget mismatches, first: {:?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(5)]
    );
}

#[test]
fn a_file_without_a_cbor_wrapper_is_a_harness_error_not_a_result() {
    let Some(corpora) =
        corpus_or_skip("a_file_without_a_cbor_wrapper_is_a_harness_error_not_a_result")
    else {
        return;
    };
    let models = turbo_cost_models(&corpora).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-wrapper.flat");
    // 0xff is not a CBOR byte string.
    std::fs::write(&path, [0xff, 0x00, 0x01]).unwrap();
    let real = load_programs(&corpora).unwrap().remove(0);
    let broken = Program { path, ..real };
    let error = measure(&broken, &models).unwrap_err();
    assert!(error.contains("outer CBOR wrapper"), "{error}");
}
