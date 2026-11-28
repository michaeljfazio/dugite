//! The expected-results contract for the replayed ImpSpec rules, tested on its
//! own: no corpus needed.
//!
//! The gate must fail on each of the things the contract names — an unknown id,
//! an entry that now matches, a changed observed result — and pass when nothing
//! has changed. The committed file must parse (every divergence linking an issue),
//! be bound to the manifest's corpus tag, and contain no harness-error entry.

use dugite_conformance::upstream::ledger_rules_replay::expected::{
    entry_for, gate, issue_for, manifest_tag, pinned_counts, Entry, Expected, Gate, ISSUE_TITLES,
};
use dugite_conformance::upstream::ledger_rules_replay::replay::Verdict;

fn exp(entries: &[(&str, &str, &str)]) -> Expected {
    Expected {
        corpus_tag: "t".into(),
        issue_titles: [(1, "t1"), (7, "t7")]
            .into_iter()
            .map(|(n, t)| (n, t.to_string()))
            .collect(),
        entries: entries
            .iter()
            .map(|(id, o, i)| {
                (
                    id.to_string(),
                    Entry {
                        observed: o.to_string(),
                        issue: i.to_string(),
                    },
                )
            })
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
            &exp(&[("LEDGER/test_1", "X", "#1")]),
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
        gate(&exp(&[("LEDGER/test_1", "X", "#1")]), "LEDGER/test_1", &v),
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
        gate(&exp(&[("LEDGER/test_1", "X", "#1")]), "LEDGER/test_1", &v),
        Gate::Ok
    );
}

#[test]
fn an_agreed_rejection_is_pinned_like_a_divergence() {
    let e = exp(&[("LEDGER/test_1", "AGREED_REJECT validator=A", "-")]);
    assert_eq!(
        gate(
            &e,
            "LEDGER/test_1",
            &Verdict::AgreedReject("AGREED_REJECT validator=A".into())
        ),
        Gate::Ok
    );
    assert!(matches!(
        gate(
            &e,
            "LEDGER/test_1",
            &Verdict::AgreedReject("AGREED_REJECT validator=B".into())
        ),
        Gate::Changed { .. }
    ));
    assert!(matches!(
        gate(
            &exp(&[]),
            "LEDGER/test_1",
            &Verdict::AgreedReject("AGREED_REJECT validator=A".into())
        ),
        Gate::Unknown { .. }
    ));
}

#[test]
fn harness_error_is_never_recordable() {
    let v = Verdict::HarnessError("boom".into());
    assert!(matches!(
        gate(
            &exp(&[("LEDGER/test_1", "boom", "#1")]),
            "LEDGER/test_1",
            &v
        ),
        Gate::HarnessError(_)
    ));
}

#[test]
fn render_parse_round_trips_and_groups_by_class() {
    let e = exp(&[
        ("LEDGER/test_10", "A post=x #1", "#7"),
        ("LEDGER/test_2", "A post=x #2", "#7"),
        ("NEWEPOCH/test_5", "AGREED_REJECT y", "-"),
    ]);
    let text = e.render();
    assert!(text.contains("# 2 vector(s), issue #7: A post=x"), "{text}");
    let back = Expected::parse(&text).unwrap();
    assert_eq!(back.entries, e.entries);
    assert_eq!(back.corpus_tag, "t");
    assert!(text.find("test_2\t").unwrap() < text.find("test_10\t").unwrap());
}

#[test]
fn every_divergence_must_link_an_issue() {
    let h = "# issue #12: t12\n# issue #13: t13\n";
    // missing column
    assert!(Expected::parse("A/test_1\tX\n").is_err());
    // not an issue reference
    assert!(Expected::parse("A/test_1\tX\tTBD\n").is_err());
    assert!(Expected::parse("A/test_1\tX\t-\n").is_err());
    assert!(Expected::parse("A/test_1\tX\t#\n").is_err());
    assert!(Expected::parse(&format!("{h}A/test_1\tX\t#12\n")).is_ok());
    assert!(Expected::parse(&format!("{h}A/test_1\tX\t#12,#13\n")).is_ok());
    assert!(Expected::parse(&format!("{h}A/test_1\tX\t#12,\n")).is_err());
    // an issue with no pinned title cannot be checked against GitHub
    assert!(Expected::parse("A/test_1\tX\t#12\n").is_err());
    // an agreed rejection has no issue, and a divergence cannot hide as one
    assert!(Expected::parse("A/test_1\tAGREED_REJECT x\t-\n").is_ok());
    assert!(Expected::parse(&format!("{h}A/test_1\tAGREED_REJECT x\t#12\n")).is_err());
}

#[test]
fn duplicate_ids_are_rejected() {
    assert!(Expected::parse("A/test_1\tx\t#1\nA/test_1\ty\t#1\n").is_err());
}

#[test]
fn issue_assignment_follows_the_class() {
    let i = |id: &str, o: &str| issue_for(id, o).unwrap();
    assert_eq!(i("LEDGER/test_1", "AGREED_REJECT validator=X"), None);
    // every issue issue_for returns has a pinned title (checked against GitHub by
    // scripts/node-diversity/check-expected-issues.sh)
    for n in [
        1132u64, 1133, 1134, 1190, 1191, 1192, 1194, 1195, 1196, 1197, 1198,
    ] {
        assert!(
            ISSUE_TITLES.iter().any(|(m, _)| *m == n),
            "no title for #{n}"
        );
    }
    assert_eq!(
        i("NEWEPOCH/test_1", "STATE_MISMATCH post=gov:proposal/*,vstate:dormant_epochs pre=inputs:drep_pulsing_state #a").as_deref(),
        Some("#1132,#1198")
    );
    assert_eq!(
        i(
            "NEWEPOCH/test_1",
            "STATE_MISMATCH post=gov:future_pparams/tag pre=none #a"
        )
        .as_deref(),
        Some("#1133")
    );
    assert_eq!(
        i(
            "NEWEPOCH/test_1",
            "STATE_MISMATCH post=gov:committee/present pre=none #a"
        )
        .as_deref(),
        Some("#1134")
    );
    assert_eq!(
        i(
            "LEDGER/test_1",
            "STATE_MISMATCH post=instant_stake:* pre=none #a"
        )
        .as_deref(),
        Some("#1194")
    );
    assert_eq!(
        i(
            "LEDGER/test_6121",
            "ACCEPTED_BY_DUGITE_REJECTED_BY_HASKELL validator=ok applied pre=none #a"
        )
        .as_deref(),
        Some("#1191")
    );
    // a result that fits no issue is an error, never a placeholder
    assert!(issue_for("LEDGER/test_1", "STATE_MISMATCH post=pots:fees pre=none #a").is_err());
}

#[test]
fn committed_file_is_bound_to_the_pinned_corpus() {
    let e = Expected::load().expect("the expected-results file must be committed and well formed");
    assert_eq!(
        e.corpus_tag,
        manifest_tag().unwrap(),
        "the expected-results file is for a different corpus than manifest.toml pins"
    );
}

// ── red proofs: a result that fits no issue never gets one ────────────────────

#[test]
fn an_unrecognised_class_fails_instead_of_getting_an_issue() {
    let unknown = "STATE_MISMATCH post=pots:fees pre=none #a";
    assert!(issue_for("LEDGER/test_1", unknown).is_err());
    // ...and so does the entry regeneration would write
    assert!(entry_for("LEDGER/test_1", &Verdict::Diverges(unknown.into())).is_err());
    // a harness error is never an entry
    assert!(entry_for("LEDGER/test_1", &Verdict::HarnessError("x".into())).is_err());
    // a plain match has none; a known class has the right one
    assert_eq!(entry_for("LEDGER/test_1", &Verdict::Match), Ok(None));
    let e = entry_for(
        "LEDGER/test_1",
        &Verdict::Diverges("STATE_MISMATCH post=instant_stake:* pre=none #a".into()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(e.issue, "#1194");
}

#[test]
fn the_committed_titles_are_the_pinned_ones() {
    let e = Expected::load().unwrap();
    for (n, t) in ISSUE_TITLES {
        if let Some(committed) = e.issue_titles.get(n) {
            assert_eq!(committed, t, "#{n}");
        }
    }
    // every issue the entries use is titled (parse enforces it), and nothing else is
    for n in e.issue_titles.keys() {
        assert!(ISSUE_TITLES.iter().any(|(m, _)| m == n), "stray title #{n}");
    }
}

#[test]
fn a_corpus_count_is_pinned_for_the_manifest_tag() {
    let (ledger, newepoch) = pinned_counts(&manifest_tag().unwrap())
        .expect("vector counts must be pinned for the corpus manifest.toml names");
    assert!(ledger > 0 && newepoch > 0);
}
