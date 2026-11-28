//! Tests of the `uplc-conformance` runner (#1114 Phase 2a): the report itself is produced by
//! `just ndc-report uplc-conformance` (the `ndc_report_uplc_conformance` runner in
//! `ndc_reports.rs`); these tests bound the runner's parts against the fetched corpus.
//!
//! They need the corpus.  Without it they skip (and say so); with `DUGITE_NDC_REQUIRE=1` (the just
//! recipes set it) a missing corpus fails them.  Nothing here changes what the runner reports: the
//! expected results are read only to CHECK the runner, never to produce its output.

use std::path::PathBuf;

use dugite_conformance::node_diversity::pins::corpora_dir;
use dugite_conformance::node_diversity::uplc_conformance::{
    load_cost_models, load_expectations, load_vectors, normalise_evaluation, run_all, run_vector,
    scores, Expectation, Observed, Vector,
};
use dugite_uplc::cost_models::CostModels;
use serde_json::Value;

const REQUIRE_ENV: &str = "DUGITE_NDC_REQUIRE";

fn corpus_or_skip(test: &str) -> Option<PathBuf> {
    let corpora = corpora_dir();
    if corpora
        .join("ndc/scenarios/uplc-conformance/expectations.json")
        .is_file()
        && corpora.join("uplc-conformance").is_dir()
    {
        return Some(corpora);
    }
    if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
        panic!(
            "{test}: {REQUIRE_ENV}=1 but the uplc-conformance corpus is missing under {}; \
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

fn vector<'a>(vectors: &'a [Vector], id: &str, pv: &str) -> &'a Vector {
    vectors
        .iter()
        .find(|v| v.id == id && v.protocol_version == pv)
        .unwrap_or_else(|| panic!("no vector {id} at PV{pv}"))
}

/// Would NDC's scorer pass this observation against this expectation?
fn passes(observed: &Observed, expected: &Expectation) -> bool {
    normalise_evaluation(&observed.evaluation) == normalise_evaluation(&expected.evaluation)
        && (expected.budget.is_none() || observed.budget == expected.budget)
}

#[test]
fn every_expectation_is_found_in_the_corpus_and_every_vector_has_an_expectation() {
    let Some(corpora) = corpus_or_skip("every_expectation_is_found_in_the_corpus") else {
        return;
    };
    let vectors = load_vectors(&corpora).unwrap();
    assert_eq!(vectors.len(), 1064, "the pinned corpus has 1,064 vectors");
    let mut dirs: Vec<_> = vectors.iter().map(|v| v.dir.clone()).collect();
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), vectors.len(), "two vectors share a directory");
}

/// Vectors spanning every outcome the runner can produce, checked against the corpus' own
/// expected results: this bounds the readback, the printer, the budget and the three error
/// classes (the whole-corpus number comes from `just ndc-score`).
#[test]
fn sample_vectors_reproduce_the_corpus_expectations() {
    let Some(corpora) = corpus_or_skip("sample_vectors_reproduce_the_corpus_expectations") else {
        return;
    };
    let vectors = load_vectors(&corpora).unwrap();
    let models = load_cost_models(&corpora).unwrap();
    let expected = load_expectations(&corpora).unwrap();
    let samples = [
        // a constant result, run from flat
        ("example_factorial_v3", "11.0"),
        // closures read back and printed under de Bruijn level names
        ("example_churchsucc_v3", "11.0"),
        // partial builtin applications
        ("builtin_interleaving_iteforced_v3", "11.0"),
        // string escapes
        ("builtin_constant_string_string_07_v3", "11.0"),
        // data, pair, list and value constants
        ("builtin_constant_data_datamap_v3", "11.0"),
        ("builtin_constant_pair_nestedpair_v3", "11.0"),
        ("builtin_parser_value_value_ok_1_v3", "11.0"),
        // constr and case, run from text (PV1.1.0)
        ("term_constr_constr_03_v3", "11.0"),
        // BLS elements exist only as text
        ("builtin_constant_bls12_381_g2_zero_v3", "11.0"),
        // the three error classes
        (
            "builtin_semantics_indexbytestring_index_2pow64_plus_one_v3",
            "10.0",
        ),
        ("builtin_constant_array_illtypedarray_01_v3", "11.0"),
        ("builtin_semantics_droplist_count_beyond_u64_v3", "11.0"),
        // a PV10 vector of each language
        ("builtin_semantics_consbytestring_negative_wraps_v2", "10.0"),
    ];
    for (id, pv) in samples {
        let v = vector(&vectors, id, pv);
        let key = (
            v.id.clone(),
            v.protocol_version.clone(),
            v.plutus_version.clone(),
        );
        let observed = run_vector(v, &models).unwrap();
        let want = &expected[&key];
        assert!(
            passes(&observed, want),
            "{id}: observed {observed:?}, corpus expects {:?} / {:?}",
            want.evaluation,
            want.budget
        );
    }
}

#[test]
fn the_three_error_classes_come_from_the_evaluator_not_from_the_vector_name() {
    let Some(corpora) = corpus_or_skip("the_three_error_classes") else {
        return;
    };
    let vectors = load_vectors(&corpora).unwrap();
    let models = load_cost_models(&corpora).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for v in &vectors {
        let observed = run_vector(v, &models).unwrap();
        if !observed.evaluation.starts_with('(') {
            seen.insert(observed.evaluation);
        }
    }
    for class in ["evaluation failure", "out of budget", "parse/decode error"] {
        assert!(
            seen.contains(class),
            "no vector produced {class:?}: {seen:?}"
        );
    }
    assert!(
        seen.iter().all(|s| matches!(
            s.as_str(),
            "evaluation failure" | "out of budget" | "parse/decode error"
        )),
        "a failure outside the scenario's three classes: {seen:?}"
    );
}

/// Red-proof for the cost model: the budget the runner reports is the corpus cost model's, so a
/// perturbed model must move it and the scorer's comparison must then fail.
#[test]
fn a_perturbed_cost_model_changes_the_budget_and_fails_the_comparison() {
    let Some(corpora) = corpus_or_skip("a_perturbed_cost_model") else {
        return;
    };
    let vectors = load_vectors(&corpora).unwrap();
    let models = load_cost_models(&corpora).unwrap();
    let expected = load_expectations(&corpora).unwrap();
    let v = vector(&vectors, "example_factorial_v3", "11.0");
    let key = (
        v.id.clone(),
        v.protocol_version.clone(),
        v.plutus_version.clone(),
    );

    let honest = run_vector(v, &models).unwrap();
    assert!(
        passes(&honest, &expected[&key]),
        "baseline must pass: {honest:?}"
    );

    let mut bent = models.clone();
    let entry: &mut CostModels = bent.get_mut(&(11, "V3".to_string())).unwrap();
    // The CEK start-up / step costs lead the V3 array; raising one moves every program's budget.
    let params = entry.plutus_v3.as_mut().unwrap();
    params[0] += 1;
    let perturbed = run_vector(v, &bent).unwrap();
    assert_ne!(
        perturbed.budget, honest.budget,
        "perturbing the cost model must move the budget"
    );
    assert!(
        !passes(&perturbed, &expected[&key]),
        "the comparison must go red on a perturbed budget"
    );
}

/// A vector must never be charged by a model it did not ask for: with no model for its language
/// the runner refuses (a harness failure) rather than falling back to the reference model.
#[test]
fn a_missing_cost_model_is_a_harness_failure_not_a_fallback() {
    let Some(corpora) = corpus_or_skip("a_missing_cost_model") else {
        return;
    };
    let vectors = load_vectors(&corpora).unwrap();
    let mut models = load_cost_models(&corpora).unwrap();
    let v = vector(&vectors, "example_factorial_v3", "11.0");
    models.insert((11, "V3".to_string()), CostModels::default());
    let err = run_vector(v, &models).unwrap_err();
    assert!(err.contains("cost model"), "{err}");
}

/// The pinned failures: every vector dugite currently fails, with the observed outcome and the
/// issue tracking it (`node-diversity/uplc-conformance-known-failures.json`).  Reporting never
/// fails on a score; THIS test does, on any change: a new failure, a listed vector that now
/// passes, or a changed outcome.  Fix a vector (or move the pin) and edit the file in the same
/// change.
#[test]
fn the_failing_vectors_are_exactly_the_pinned_ones() {
    let Some(corpora) = corpus_or_skip("the_failing_vectors_are_exactly_the_pinned_ones") else {
        return;
    };
    let run = run_all(&corpora).unwrap();
    assert!(run.fatal().is_none(), "{:?}", run.fatal());
    let expected = load_expectations(&corpora).unwrap();

    let mut observed_failures: std::collections::BTreeMap<(String, String, String), String> =
        Default::default();
    for case in &run.cases {
        let key = (
            case.id.clone(),
            case.protocol_version.clone(),
            case.plutus_version.clone(),
        );
        if !scores(case, &expected[&key]) {
            let budget = case.budget.map(|b| format!("{}/{}", b.cpu, b.mem));
            observed_failures.insert(key, format!("{} | {budget:?}", case.evaluation));
        }
    }

    let pin_path = dugite_conformance::node_diversity::pins::repo_root()
        .join("tests/conformance/node-diversity/uplc-conformance-known-failures.json");
    let pin: Value = serde_json::from_str(&std::fs::read_to_string(&pin_path).unwrap()).unwrap();
    let mut pinned: std::collections::BTreeMap<(String, String, String), String> =
        Default::default();
    for f in pin["failures"].as_array().unwrap() {
        let s = |k: &str| f[k].as_str().unwrap().to_string();
        assert!(
            f["issue"].as_u64().is_some(),
            "every pinned failure names its issue: {f}"
        );
        let budget = f["observed_budget"]
            .as_object()
            .map(|b| format!("{}/{}", b["cpu"], b["mem"]));
        let key = (s("id"), s("protocol_version"), s("plutus_version"));
        let value = format!("{} | {budget:?}", s("observed_evaluation"));
        assert!(
            pinned.insert(key, value).is_none(),
            "duplicate pinned vector: {f}"
        );
    }

    let mut problems = Vec::new();
    for (key, outcome) in &observed_failures {
        match pinned.get(key) {
            None => problems.push(format!("NEW FAILURE {key:?}: {outcome}")),
            Some(p) if p != outcome => problems.push(format!(
                "CHANGED OUTCOME {key:?}: pinned {p}, now {outcome}"
            )),
            Some(_) => {}
        }
    }
    for key in pinned.keys() {
        if !observed_failures.contains_key(key) {
            problems.push(format!("NOW PASSES {key:?}: remove it from the pin"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A run that produces a non-scoring outcome (panic, internal error, decoder not yet wired) must
/// fail, and those outcomes must never be one of the scenario's three classes.
#[test]
fn non_scoring_outcomes_fail_the_run_and_never_score() {
    use dugite_conformance::node_diversity::report::UplcCase;
    use dugite_conformance::node_diversity::uplc_conformance::{
        is_scenario_class, Run, DECODER_NOT_WIRED, EVALUATOR_INTERNAL, EVALUATOR_PANIC,
    };
    for class in [EVALUATOR_PANIC, EVALUATOR_INTERNAL, DECODER_NOT_WIRED] {
        assert!(!is_scenario_class(class), "{class}");
    }
    let mut run = Run::default();
    run.cases.push(UplcCase {
        id: "x".into(),
        title: "x".into(),
        group: "g".into(),
        plutus_version: "V3".into(),
        protocol_version: "11.0".into(),
        evaluation: DECODER_NOT_WIRED.into(),
        budget: None,
        error: None,
    });
    assert!(run.fatal().is_none(), "no unscorable vector was recorded");
    run.unscorable.push("x: decoder not yet wired".into());
    assert!(run.fatal().is_some());
}
