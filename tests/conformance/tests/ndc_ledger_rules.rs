//! The node-diversity `ledger-rules` strict gate and its red-proofs (#1125, Phase 2b of #1114).
//!
//! `ledger_rules_gate` is the gate: it runs all 368 fixtures through dugite's own validation path
//! and compares the result, read off the `ApplyTxErr` bytes, with the committed expected-results
//! contract (`tests/conformance/node-diversity/ledger-rules/expected-results.toml`).  Every other
//! test here proves the gate can fail:
//!
//! * the context a fixture is validated in comes from a real `LedgerState`, and exactly the
//!   documented fields of it are `None` ([`context_none_set_is_the_documented_one`]);
//! * dropping any `initial_state` section turns a fixture red, or is recorded as a section no
//!   rule reads ([`dropping_each_initial_state_section_is_observable`]);
//! * a wrong tag-table entry turns a fixture red ([`a_wrong_tag_table_entry_turns_a_fixture_red`]);
//! * the generic fallback bytes are a failure, never a match ([`generic_fallback_bytes_are_never_a_match`]);
//! * a removed entry, a stale entry, a changed observed result and an unknown id each fail the gate;
//! * the table decodes a real Haskell-produced `ApplyTxErr` ([`the_tag_table_decodes_the_haskell_golden`]);
//! * the oracle's own report scores 368/368 under the same reading of the fixtures.
//!
//! The corpus must be fetched (`just ndc-fetch`).  Without it these tests skip, loudly, unless
//! `DUGITE_NDC_REQUIRE=1` (the just recipes and CI set it), in which case they fail.

use std::collections::{BTreeMap, BTreeSet};

use dugite_conformance::node_diversity::ledger_rules::contract::{self, Entry};
use dugite_conformance::node_diversity::ledger_rules::decode::{
    decode_apply_tx_err, with_rules_override, Wire,
};
use dugite_conformance::node_diversity::ledger_rules::fixture::{corpus_root, load_all, Fixture};
use dugite_conformance::node_diversity::ledger_rules::names::{Named, Vocabulary};
use dugite_conformance::node_diversity::ledger_rules::ndc_names::{
    HASKELL_TO_NDC, NDC_NAMES_WITHOUT_CONSTRUCTOR,
};
use dugite_conformance::node_diversity::ledger_rules::report::{generate, run_all, vocabulary};
use dugite_conformance::node_diversity::ledger_rules::run::{
    run_fixture, Run, Verdict, CONTEXT_NONE_EXPECTED,
};
use dugite_conformance::node_diversity::ledger_rules::state::Section;
use dugite_conformance::node_diversity::ledger_rules::tag_table::{
    Rule, Variant, ARITY, PAYLOAD_ARITY, PAYLOAD_RULES, RULES,
};
use dugite_conformance::node_diversity::pins::{corpora_dir, Pins};
use dugite_conformance::node_diversity::report::{validate_report, RunInfo, Scenario};

const REQUIRE_ENV: &str = "DUGITE_NDC_REQUIRE";
const UPDATE_ENV: &str = "DUGITE_NDC_UPDATE_SNAPSHOT";

/// The corpus is there, or this test says so and returns `false` (and panics when required).
fn have_corpus(test: &str) -> bool {
    let schema = Scenario::LedgerRules.schema_path(&corpora_dir());
    if schema.is_file() && corpus_root().join("scenarios").is_dir() {
        return true;
    }
    if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
        panic!("{test}: {REQUIRE_ENV}=1 but the ledger-rules corpus is not fetched; run scripts/node-diversity/fetch-corpora.sh");
    }
    eprintln!("SKIPPED {test}: ledger-rules corpus not fetched (just ndc-fetch)");
    false
}

fn entries() -> Vec<Entry> {
    contract::load(&contract::expected_results_path()).expect("expected-results.toml parses")
}

// ── the gate ───────────────────────────────────────────────────────────────────────────────

#[test]
fn ledger_rules_gate() {
    if !have_corpus("ledger_rules_gate") {
        return;
    }
    let (fixtures, runs) = run_all().expect("every fixture must be preparable");
    let entries = entries();
    let gate = contract::check(&fixtures, &runs, &entries);
    eprintln!(
        "ledger-rules: {}/{} fixtures match; {} tracked divergences (expected-results.toml: {} entries)",
        gate.matched,
        gate.total,
        gate.tracked,
        entries.len()
    );

    // The raised-set snapshot: over-rejection and changed wire shapes show as a diff.
    let current = contract::snapshot(&runs);
    if std::env::var(UPDATE_ENV).as_deref() == Ok("1") {
        std::fs::write(contract::snapshot_path(), contract::snapshot_text(&runs))
            .expect("write snapshot");
        eprintln!(
            "{UPDATE_ENV}=1: wrote {}",
            contract::snapshot_path().display()
        );
    } else {
        let committed = std::fs::read_to_string(contract::snapshot_path())
            .expect("raised-snapshot.json is committed");
        let diff = contract::snapshot_diff(&committed, &current);
        assert!(
            diff.is_empty(),
            "the raised-failure snapshot changed (review, then rerun with {UPDATE_ENV}=1):\n{}",
            diff.join("\n")
        );
    }

    assert!(
        gate.ok(),
        "ledger-rules gate failed ({} problems):\n{}",
        gate.problems.len(),
        gate.problems.join("\n")
    );
    assert_eq!(gate.total, 368);
    assert_eq!(gate.matched + gate.tracked, gate.total);
}

/// A copy of the live gate inputs the red-proofs below mutate.
fn live() -> (Vec<Fixture>, Vec<Run>, Vec<Entry>) {
    let (f, r) = run_all().expect("run");
    (f, r, entries())
}

#[test]
fn gate_fails_on_a_removed_entry() {
    if !have_corpus("gate_fails_on_a_removed_entry") {
        return;
    }
    let (f, r, mut e) = live();
    assert!(!e.is_empty(), "the contract is empty: nothing to remove");
    let removed = e.remove(0);
    let gate = contract::check(&f, &r, &e);
    assert!(
        gate.problems
            .iter()
            .any(|p| p.starts_with(&removed.id) && p.contains("a new divergence has no entry")),
        "removing {} did not fail the gate: {:?}",
        removed.id,
        gate.problems
    );
}

#[test]
fn gate_fails_on_a_stale_entry() {
    if !have_corpus("gate_fails_on_a_stale_entry") {
        return;
    }
    let (f, r, mut e) = live();
    let matching = r.iter().find(|x| x.matched()).expect("a matching fixture");
    e.push(Entry {
        id: matching.id.clone(),
        expected: matching.expected.gate_name(),
        observed: "something".into(),
        class: "stale".into(),
        issue: 1,
    });
    let gate = contract::check(&f, &r, &e);
    assert!(
        gate.problems
            .iter()
            .any(|p| p.starts_with(&matching.id) && p.contains("now matches")),
        "{:?}",
        gate.problems
    );
}

#[test]
fn gate_fails_on_a_changed_observed_result() {
    if !have_corpus("gate_fails_on_a_changed_observed_result") {
        return;
    }
    let (f, r, mut e) = live();
    let first = e.first_mut().expect("an entry");
    let id = first.id.clone();
    first.observed = format!("{}-but-different", first.observed);
    let gate = contract::check(&f, &r, &e);
    assert!(
        gate.problems
            .iter()
            .any(|p| p.starts_with(&id) && p.contains("dugite now gives")),
        "{:?}",
        gate.problems
    );
}

#[test]
fn gate_fails_on_an_unknown_id_and_a_missing_issue() {
    if !have_corpus("gate_fails_on_an_unknown_id_and_a_missing_issue") {
        return;
    }
    let (f, r, mut e) = live();
    e.push(Entry {
        id: "99999".into(),
        expected: "pass".into(),
        observed: "x".into(),
        class: "c".into(),
        issue: 7,
    });
    e[0].issue = 0;
    let gate = contract::check(&f, &r, &e);
    assert!(gate
        .problems
        .iter()
        .any(|p| p.contains("99999: not a fixture id")));
    assert!(gate.problems.iter().any(|p| p.contains("no issue number")));
}

// ── names come from bytes ──────────────────────────────────────────────────────────────────

/// The upstream golden: a real `ApplyTxErr` for the Conway era, produced by ouroboros-consensus'
/// golden test.  `[[6, [ConwayCertsFailure (CertFailure (DelegFailure (DelegateeStakePoolNotRegisteredDELEG kh)))]]]`.
#[test]
fn the_tag_table_decodes_the_haskell_golden() {
    let bytes = std::fs::read(contract::data_dir().join("goldens/ApplyTxErr_Conway"))
        .expect("golden is committed");
    let Wire::Failures(failures) = decode_apply_tx_err(&bytes) else {
        panic!("the Haskell golden did not decode");
    };
    assert_eq!(failures.len(), 1);
    let f = &failures[0];
    assert_eq!(
        f.path,
        [
            "ConwayCertsFailure",
            "CertFailure",
            "DelegFailure",
            "DelegateeStakePoolNotRegisteredDELEG"
        ]
    );
    assert_eq!(f.tags, [2, 1, 1, 6]);
    assert!(!f.unknown);
}

/// Swap the names of two constructors of the same rule and arity.  A table with a wrong entry
/// must turn a fixture that depends on it red.
#[test]
fn a_wrong_tag_table_entry_turns_a_fixture_red() {
    if !have_corpus("a_wrong_tag_table_entry_turns_a_fixture_red") {
        return;
    }
    let (fixtures, runs, _) = live();
    let vocab = vocabulary().unwrap();
    // A fixture that matches today, expects ValueNotConservedUTxO, and reaches it through the table.
    let target = runs
        .iter()
        .find(|r| r.matched() && r.expected.result_name() == "ValueNotConservedUTxO")
        .expect("a matching ValueNotConservedUTxO fixture");
    let fixture = fixtures.iter().find(|f| f.id == target.id).unwrap();

    let mut rules: Vec<Rule> = RULES.to_vec();
    let utxo = rules
        .iter_mut()
        .find(|r| r.name == "ConwayUtxoPredFailure")
        .unwrap();
    let mut variants: Vec<Variant> = utxo.variants.to_vec();
    let a = variants
        .iter()
        .position(|v| v.name == "ValueNotConservedUTxO")
        .unwrap();
    let b = variants
        .iter()
        .position(|v| v.name == "FeeTooSmallUTxO")
        .unwrap();
    assert_eq!(arity("ValueNotConservedUTxO"), arity("FeeTooSmallUTxO"));
    let (name_a, name_b) = (variants[a].name, variants[b].name);
    variants[a].name = name_b;
    variants[b].name = name_a;
    utxo.variants = Box::leak(variants.into_boxed_slice());
    let wrong: &'static [Rule] = Box::leak(rules.into_boxed_slice());

    let mutated = with_rules_override(wrong, || run_fixture(fixture, &vocab, None).unwrap());
    assert!(
        !mutated.matched(),
        "a table that swaps ValueNotConservedUTxO and FeeTooSmallUTxO still matched {}: {:?}",
        target.id,
        mutated.raised_lines()
    );
    // And the untouched table still matches.
    assert!(run_fixture(fixture, &vocab, None).unwrap().matched());
}

fn arity(constructor: &str) -> Option<usize> {
    ARITY
        .iter()
        .chain(PAYLOAD_ARITY.iter())
        .find(|(n, _)| *n == constructor)
        .map(|(_, a)| *a)
}

/// The generic `ConwayMempoolFailure` bytes name nothing, even when the failure they stood in for
/// is exactly what the fixture expects.
#[test]
fn generic_fallback_bytes_are_never_a_match() {
    if !have_corpus("generic_fallback_bytes_are_never_a_match") {
        return;
    }
    let (fixtures, runs, _) = live();
    // Fixture 00076 expects InvalidWitnessesUTXOW and dugite answers with the generic fallback.
    let r = runs.iter().find(|r| r.id == "00076").unwrap();
    let bytes = hex::decode(r.wire_hex.as_ref().expect("wire bytes recorded")).unwrap();
    let Wire::Failures(failures) = decode_apply_tx_err(&bytes) else {
        panic!("not an ApplyTxErr");
    };
    assert!(failures.iter().all(|f| f.is_generic_fallback()));
    assert!(!r.matched());
    assert!(r.observed().starts_with("unmapped:ConwayMempoolFailure"));

    // Synthetic: the same bytes against an expectation of BadInputsUTxO (the name another
    // implementation maps the fallback text to) must not match either.
    let fixture = fixtures.iter().find(|f| f.id == "00076").unwrap();
    let mut forged = r.clone();
    forged.expected =
        dugite_conformance::node_diversity::ledger_rules::fixture::Expected::Predicate {
            name: "BadInputsUTxO".into(),
            description: None,
        };
    assert!(!forged.matched());
    // The report still carries a schema-valid name that is not the expectation.
    assert_ne!(forged.report_result(), "BadInputsUTxO");
    let _ = fixture;
    // A rejected run with only a generic failure never has a scenario name.
    let named = Named {
        path: "ConwayMempoolFailure".into(),
        tags: vec![7],
        haskell: "ConwayMempoolFailure".into(),
        ndc: vec![],
        generic: true,
        detail: None,
    };
    forged.verdict = Verdict::Rejected(vec![named]);
    assert!(!forged.matched());
}

// ── the context is the ledger's ────────────────────────────────────────────────────────────

/// Every run that reached validation had a context whose `None` fields are exactly the documented
/// ones: the Shelley-Babbage-only MIR fields, and `drep_deposits`, which the node's own
/// `mempool_validation_context()` leaves unset.  No other field is ever `None`.
#[test]
fn context_none_set_is_the_documented_one() {
    if !have_corpus("context_none_set_is_the_documented_one") {
        return;
    }
    let (_, runs, _) = live();
    let expected: BTreeSet<&str> = CONTEXT_NONE_EXPECTED.iter().map(|(n, _)| *n).collect();
    let mut checked = 0;
    for r in &runs {
        // A fixture refused at decode builds its context only in the shadow run.
        let probe = if matches!(r.verdict, Verdict::DecodingFailure(_)) {
            match &r.shadow {
                Some(s) => s,
                None => continue,
            }
        } else {
            r
        };
        let got: BTreeSet<&str> = probe.context_none.iter().copied().collect();
        assert_eq!(
            got, expected,
            "fixture {}: the context's `None` fields are not the documented set",
            r.id
        );
        checked += 1;
    }
    // Not vacuous: nearly every fixture is checked.
    assert!(
        checked >= 360,
        "only {checked} fixtures had a context to check"
    );
}

/// Dropping an `initial_state` section must change the outcome of at least one fixture that
/// contains it, or the section is one nothing reads.  Those are listed, with the reason, and the
/// list is exact: a section that starts to matter must leave it.
const SECTIONS_NO_RULE_READS: &[(&str, &str)] = &[(
    "governance_activity",
    "the dormant-epoch counter only feeds a DRep's expiry when a DRep is registered, updated or votes \
     (the apply path); no phase-one rule on the admission path reads it, so none of the 7 fixtures that \
     carry it can depend on it",
)];

#[test]
fn dropping_each_initial_state_section_is_observable() {
    if !have_corpus("dropping_each_initial_state_section_is_observable") {
        return;
    }
    let (fixtures, runs, _) = live();
    let vocab = vocabulary().unwrap();
    let base: BTreeMap<&str, &Run> = runs.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut unobservable: Vec<&'static str> = Vec::new();
    for section in Section::ALL {
        let mut red = Vec::new();
        let mut changed = Vec::new();
        for f in fixtures
            .iter()
            .filter(|f| f.initial_state.contains_key(section.key()))
        {
            let before = base[f.id.as_str()];
            // A fixture refused at decode never reads state; its shadow does.
            let after = run_fixture(f, &vocab, Some(section)).expect("run");
            let (b, a) = match (&before.shadow, &after.shadow) {
                (Some(bs), Some(as_)) => (bs.as_ref(), as_.as_ref()),
                _ => (before, &after),
            };
            if b.observed() != a.observed() {
                changed.push(f.id.clone());
                if b.matched() && !a.matched() {
                    red.push(f.id.clone());
                }
            }
        }
        eprintln!(
            "section {:>20}: {} fixtures change, {} turn red {:?}",
            section.key(),
            changed.len(),
            red.len(),
            &red[..red.len().min(4)]
        );
        if changed.is_empty() {
            unobservable.push(section.key());
        }
    }
    let allowed: Vec<&str> = SECTIONS_NO_RULE_READS.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        unobservable, allowed,
        "the sections no fixture's outcome depends on changed; update SECTIONS_NO_RULE_READS with the reason"
    );
}

// ── the vocabulary and the tables ──────────────────────────────────────────────────────────

#[test]
fn the_tables_are_internally_consistent() {
    let mut seen_constructors = BTreeSet::new();
    for rule in RULES.iter().chain(PAYLOAD_RULES.iter()) {
        let mut tags = BTreeSet::new();
        for v in rule.variants {
            assert!(
                tags.insert(v.tag),
                "{}: tag {} is listed twice",
                rule.name,
                v.tag
            );
            assert!(
                v.source.contains(".hs:"),
                "{}.{}: no source line",
                rule.name,
                v.name
            );
            assert!(
                arity(v.name).is_some(),
                "{}.{}: no arity",
                rule.name,
                v.name
            );
            if let Some(n) = v.nested {
                assert!(
                    RULES
                        .iter()
                        .chain(PAYLOAD_RULES.iter())
                        .any(|r| r.name == n),
                    "{}.{} wraps `{n}`, which has no rule",
                    rule.name,
                    v.name
                );
                assert!(v.nested_field >= 1);
            } else {
                assert_eq!(v.nested_field, 0, "{}.{}", rule.name, v.name);
            }
            seen_constructors.insert(v.name);
        }
    }
    // An arity row for a constructor that does not exist is a typo.
    for (n, _) in ARITY.iter().chain(PAYLOAD_ARITY.iter()) {
        assert!(
            seen_constructors.contains(n),
            "arity row for unknown constructor {n}"
        );
    }
}

#[test]
fn every_fixture_result_name_is_reachable_and_in_the_vocabulary() {
    if !have_corpus("every_fixture_result_name_is_reachable_and_in_the_vocabulary") {
        return;
    }
    let vocab = vocabulary().unwrap();
    assert_eq!(
        vocab.len(),
        81,
        "the scenario's result vocabulary is 81 names"
    );
    let constructors: BTreeSet<&str> = RULES
        .iter()
        .chain(PAYLOAD_RULES.iter())
        .flat_map(|r| r.variants.iter().map(|v| v.name))
        .collect();
    // Every row of the name table points at a name the scenario has, from a constructor that exists.
    for (haskell, ndc, evidence) in HASKELL_TO_NDC {
        assert!(
            vocab.contains(ndc),
            "{haskell} -> {ndc}: not in the vocabulary"
        );
        assert!(
            constructors.contains(haskell),
            "{haskell} is not a constructor in the tag table"
        );
        assert!(!evidence.is_empty());
    }
    for (name, why) in NDC_NAMES_WITHOUT_CONSTRUCTOR {
        assert!(vocab.contains(name), "{name}: not in the vocabulary");
        assert!(!why.is_empty());
    }
    // Every name some fixture expects is reachable: identical to a constructor, a table row, or
    // one of the documented constructor-less names.
    let fixtures = load_all(&corpus_root()).unwrap();
    let reachable = |name: &str| {
        constructors.contains(name)
            || HASKELL_TO_NDC.iter().any(|(_, n, _)| *n == name)
            || NDC_NAMES_WITHOUT_CONSTRUCTOR
                .iter()
                .any(|(n, _)| *n == name)
    };
    for f in &fixtures {
        let name = f.expected.result_name();
        assert!(
            vocab.contains(name),
            "{}: `{name}` not in the vocabulary",
            f.id
        );
        assert!(
            reachable(name),
            "{}: `{name}` is not reachable from any constructor",
            f.id
        );
    }
    // The placeholders used for names the report cannot carry are in the vocabulary.
    for p in dugite_conformance::node_diversity::ledger_rules::run::UNREPRESENTABLE_PLACEHOLDERS {
        assert!(vocab.contains(p));
    }
}

/// cardano-node's own report scores 368/368 under the reading of the fixtures this harness uses:
/// a check the oracle fails is a wrong check.
#[test]
fn the_oracle_report_scores_every_fixture() {
    if !have_corpus("the_oracle_report_scores_every_fixture") {
        return;
    }
    let path = corpora_dir().join("ndc/scenarios/ledger-rules/preview/cardano-node.json.zst");
    let compressed = std::fs::read(&path).expect("the oracle report is in the NDC checkout");
    let json = zstd::decode_all(compressed.as_slice()).expect("zstd");
    let report: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let pins = Pins::load_default().unwrap();
    assert_eq!(
        report["dataset_commit"].as_str().unwrap(),
        pins.ledger_rules_dataset_commit(),
        "the oracle report is for a different corpus than the pinned one"
    );
    let results: BTreeMap<String, String> = report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["result"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let fixtures = load_all(&corpus_root()).unwrap();
    let mut scored = 0;
    for f in &fixtures {
        // The report carries the name only, so the comparison is on the name.
        assert_eq!(
            results.get(&f.id).map(String::as_str),
            Some(f.expected.result_name()),
            "fixture {}: the oracle and this harness read the expectation differently",
            f.id
        );
        scored += 1;
    }
    assert_eq!(scored, 368);
}

// ── the report ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_report_is_complete_and_schema_valid() {
    if !have_corpus("the_report_is_complete_and_schema_valid") {
        return;
    }
    let pins = Pins::load_default().unwrap();
    let run = RunInfo::detect().expect("commit");
    let (status, note, report) = generate(&pins, &run).expect("generate");
    assert_eq!(status.as_str(), "ok");
    assert!(note.contains("/368"), "{note}");
    validate_report(&corpora_dir(), Scenario::LedgerRules, &report).unwrap();
    let cases = report["scenarios"].as_array().unwrap();
    assert_eq!(cases.len(), 368);
    let vocab: Vocabulary = vocabulary().unwrap();
    for c in cases {
        assert!(vocab.contains(c["result"].as_str().unwrap()));
    }
}
