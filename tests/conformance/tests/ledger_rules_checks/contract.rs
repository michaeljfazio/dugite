//! The expected-results contract for the replayed ImpSpec rules, tested on its
//! own: no corpus needed.
//!
//! The gate must fail on each of the three things the issue names — an unknown
//! id, an entry that now matches, a changed observed result — and pass when
//! nothing has changed. The committed file must also parse, be bound to the
//! manifest's corpus tag, and contain no harness-error entry.

use dugite_conformance::upstream::ledger_rules_replay::expected::{
    gate, manifest_tag, Expected, Gate,
};
use dugite_conformance::upstream::ledger_rules_replay::replay::Verdict;

fn exp(entries: &[(&str, &str)]) -> Expected {
    Expected {
        corpus_tag: "t".into(),
        entries: entries
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    }
}

#[test]
fn matching_vector_without_entry_is_ok() {
    assert_eq!(gate(&exp(&[]), "LEDGER/test_1", &Verdict::Match), Gate::Ok);
}

#[test]
fn divergence_without_entry_is_unknown() {
    let v = Verdict::Diverges("X".into());
    assert_eq!(
        gate(&exp(&[]), "LEDGER/test_1", &v),
        Gate::Unknown {
            observed: "X".into()
        }
    );
}

#[test]
fn entry_that_now_matches_must_be_removed() {
    assert_eq!(
        gate(
            &exp(&[("LEDGER/test_1", "X")]),
            "LEDGER/test_1",
            &Verdict::Match
        ),
        Gate::NowMatches {
            recorded: "X".into()
        }
    );
}

#[test]
fn changed_observed_result_is_a_failure() {
    let v = Verdict::Diverges("Y".into());
    assert_eq!(
        gate(&exp(&[("LEDGER/test_1", "X")]), "LEDGER/test_1", &v),
        Gate::Changed {
            recorded: "X".into(),
            observed: "Y".into()
        }
    );
}

#[test]
fn identical_observed_result_is_ok() {
    let v = Verdict::Diverges("X".into());
    assert_eq!(
        gate(&exp(&[("LEDGER/test_1", "X")]), "LEDGER/test_1", &v),
        Gate::Ok
    );
}

#[test]
fn harness_error_is_never_recordable() {
    let v = Verdict::HarnessError("boom".into());
    assert!(matches!(
        gate(&exp(&[("LEDGER/test_1", "boom")]), "LEDGER/test_1", &v),
        Gate::HarnessError(_)
    ));
}

#[test]
fn render_parse_round_trips_and_groups_by_class() {
    let e = exp(&[
        ("LEDGER/test_10", "A post=x #1"),
        ("LEDGER/test_2", "A post=x #2"),
        ("NEWEPOCH/test_5", "B"),
    ]);
    let text = e.render();
    assert!(text.contains("# 2 vector(s): A post=x"), "{text}");
    let back = Expected::parse(&text).unwrap();
    assert_eq!(back.entries, e.entries);
    assert_eq!(back.corpus_tag, "t");
    // natural order inside a class
    assert!(text.find("test_2\t").unwrap() < text.find("test_10\t").unwrap());
}

#[test]
fn duplicate_ids_are_rejected() {
    assert!(Expected::parse("A/test_1\tx\nA/test_1\ty\n").is_err());
}

#[test]
fn committed_file_is_bound_to_the_pinned_corpus() {
    let e = Expected::load().expect("the expected-results file must be committed");
    assert_eq!(
        e.corpus_tag,
        manifest_tag().unwrap(),
        "the expected-results file is for a different corpus than manifest.toml pins"
    );
}
