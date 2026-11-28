//! Tests of the `cbor-decoding` runner against the fetched dataset (#1114 Phase 2c).
//!
//! The pure guards (a decoder that rejects everything, an unregistered rule, the discriminating
//! counts) are unit tests in `src/node_diversity/cbor_decoding/`; these need the pinned dataset
//! and, like the other harness tests, skip loudly when it is not fetched, and FAIL instead of
//! skipping when `DUGITE_NDC_REQUIRE=1` (the just recipes set it).

use std::collections::BTreeSet;
use std::path::PathBuf;

use dugite_conformance::node_diversity::cbor_decoding::corpus::{
    dataset_dir, Corpus, Expectation, Sample,
};
use dugite_conformance::node_diversity::cbor_decoding::normalize::normalize;
use dugite_conformance::node_diversity::cbor_decoding::registry::{registry, Failure};
use dugite_conformance::node_diversity::cbor_decoding::scrub::{
    checked_scrub, Scrub, PRESERVED_BYTES_FIELDS,
};
use dugite_conformance::node_diversity::cbor_decoding::{
    check_dataset_self_consistency, run_corpus, HarnessDefect, BYTE_EXACT_RULES,
};
use dugite_conformance::node_diversity::pins::corpora_dir;
use dugite_conformance::node_diversity::report::{CborEra, CborOutcome};
use dugite_primitives::era::Era;
use dugite_serialization::decode::rules as dec;
use dugite_serialization::encode::rules as enc;

const REQUIRE_ENV: &str = "DUGITE_NDC_REQUIRE";

/// The fetched corpora, or None (loudly) when absent and not required.
fn corpora_or_skip(test: &str) -> Option<PathBuf> {
    let corpora = corpora_dir();
    if dataset_dir(&corpora, CborEra::Conway).is_dir()
        && dataset_dir(&corpora, CborEra::Dijkstra).is_dir()
    {
        return Some(corpora);
    }
    if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
        panic!(
            "{test}: {REQUIRE_ENV}=1 but the cbor dataset is missing under {}; \
             run scripts/node-diversity/fetch-corpora.sh",
            corpora.display()
        );
    }
    eprintln!(
        "SKIPPED {test}: dataset not fetched under {}",
        corpora.display()
    );
    None
}

fn era_of(era: CborEra) -> Era {
    match era {
        CborEra::Conway => Era::Conway,
        CborEra::Dijkstra => Era::Dijkstra,
    }
}

// ── the normaliser ──────────────────────────────────────────────────────────────────────────

/// The dataset's own hand-verified pairs: `normalize(<n>.input.cbor)` is `<n>.expected.cbor`.
#[test]
fn the_normaliser_reproduces_the_datasets_reference_vectors() {
    let Some(corpora) = corpora_or_skip("the_normaliser_reproduces_the_datasets_reference_vectors")
    else {
        return;
    };
    let dir = corpora.join("cbor-dataset/dataset/normalization");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".input.cbor") else {
            continue;
        };
        let input = std::fs::read(&path).unwrap();
        let expected = std::fs::read(dir.join(format!("{stem}.expected.cbor"))).unwrap();
        assert_eq!(
            normalize(&input).unwrap(),
            expected,
            "normalization/{stem}: {} -> expected {}",
            hex::encode(&input),
            hex::encode(&expected)
        );
        checked += 1;
    }
    assert_eq!(checked, 20, "the dataset documents 20 vectors");
}

/// Every valid sample of both eras agrees with the normaliser (or is explained by ordering).
#[test]
fn the_dataset_agrees_with_the_normaliser() {
    let Some(corpora) = corpora_or_skip("the_dataset_agrees_with_the_normaliser") else {
        return;
    };
    for era in CborEra::ALL {
        let corpus = Corpus::load(&corpora, era).unwrap();
        let consistency =
            check_dataset_self_consistency(&corpus).unwrap_or_else(|e| panic!("{}: {e}", era.id()));
        let valid: usize = corpus
            .rules
            .values()
            .flatten()
            .filter(|s| s.expectation == Expectation::Decodes)
            .count();
        let explained = consistency.exact
            + consistency.map_reordered.values().sum::<usize>()
            + consistency.up_to_ordering.values().sum::<usize>()
            + consistency.reencoded_inside_bytes;
        assert_eq!(
            explained,
            valid,
            "{}: every valid sample is accounted for",
            era.id()
        );
        // The check must have teeth: most valid samples match EXACTLY, so a broken normaliser
        // cannot hide in the weaker tiers.
        assert!(
            consistency.exact * 10 >= valid * 8,
            "{}: only {} of {valid} valid samples match normalize(input) exactly",
            era.id(),
            consistency.exact
        );
    }
}

/// Byte-exact rules: for every valid sample the expected file IS the input.
#[test]
fn byte_exact_rules_have_the_input_as_their_expected_bytes() {
    let Some(corpora) = corpora_or_skip("byte_exact_rules_have_the_input_as_their_expected_bytes")
    else {
        return;
    };
    for era in CborEra::ALL {
        let corpus = Corpus::load(&corpora, era).unwrap();
        for rule in BYTE_EXACT_RULES {
            let samples = &corpus.rules[rule];
            assert!(samples
                .iter()
                .any(|s| s.expectation == Expectation::Decodes));
            for s in samples
                .iter()
                .filter(|s| s.expectation == Expectation::Decodes)
            {
                assert_eq!(
                    s.expected.as_deref(),
                    Some(s.input.as_slice()),
                    "{}",
                    s.key()
                );
            }
        }
    }
}

// ── the registry ────────────────────────────────────────────────────────────────────────────

/// A registry entry for a rule the dataset does not have would be dead code that inflates the
/// count of rules with a decoder.
#[test]
fn every_registered_rule_is_a_rule_of_the_dataset() {
    let Some(corpora) = corpora_or_skip("every_registered_rule_is_a_rule_of_the_dataset") else {
        return;
    };
    for era in CborEra::ALL {
        let corpus = Corpus::load(&corpora, era).unwrap();
        let reg = registry(era);
        let unknown: Vec<_> = reg
            .keys()
            .filter(|r| !corpus.rules.contains_key(**r))
            .collect();
        assert!(unknown.is_empty(), "{}: {unknown:?}", era.id());
        let run = run_corpus(&corpus, &reg);
        assert_eq!(
            run.missing_decoders.len() + reg.len(),
            corpus.rules.len(),
            "{}",
            era.id()
        );
        assert_eq!(
            run.results.len(),
            corpus.sample_count(),
            "one result per sample"
        );
    }
}

// ── no input-byte reuse ─────────────────────────────────────────────────────────────────────

/// Decode samples of every rule that decodes to a value with preserved-bytes fields and record
/// which fields the decoders populate.  Each must be cleared by the harness path.
#[test]
fn the_decoders_populate_preserved_bytes_and_the_harness_clears_all_of_them() {
    let Some(corpora) =
        corpora_or_skip("the_decoders_populate_preserved_bytes_and_the_harness_clears_all_of_them")
    else {
        return;
    };
    let mut populated: BTreeSet<&'static str> = BTreeSet::new();
    for era in CborEra::ALL {
        let corpus = Corpus::load(&corpora, era).unwrap();
        let e = era_of(era);
        for (rule, samples) in &corpus.rules {
            for s in samples
                .iter()
                .filter(|s| s.expectation == Expectation::Decodes)
            {
                let b = &s.input;
                macro_rules! probe {
                    ($decoded:expr) => {
                        if let Ok(mut value) = $decoded {
                            populated.extend(value.populated());
                            checked_scrub(&mut value);
                            assert!(value.populated().is_empty(), "{}", s.key());
                        }
                    };
                }
                match rule.as_str() {
                    "block" => probe!(dec::block(b, e)),
                    "header" => probe!(dec::block_header(b)),
                    "transaction" => probe!(dec::transaction(b, e)),
                    "transaction_body" => probe!(dec::transaction_body(b, e)),
                    "sub_transaction_body" => probe!(dec::sub_transaction_body(b)),
                    "transaction_witness_set" => probe!(dec::transaction_witness_set(b, e)),
                    "transaction_output" | "babbage_transaction_output" => {
                        probe!(dec::transaction_output(b))
                    }
                    "datum_option" => probe!(dec::datum_option(b)),
                    "auxiliary_data" => probe!(dec::auxiliary_data(b, e)),
                    _ => {}
                }
            }
        }
    }
    // Every audited field is populated by SOME production decode of the dataset, so the scrub
    // list is exercised end to end rather than assumed.  A field no decode populates is named.
    let all: BTreeSet<String> = PRESERVED_BYTES_FIELDS
        .iter()
        .map(|(o, f)| format!("{}.{f}", o.split("::").last().unwrap()))
        .collect();
    let seen: BTreeSet<String> = populated.iter().map(|s| s.to_string()).collect();
    let never: Vec<_> = all.difference(&seen).collect();
    // `Transaction.raw_cbor` and `TransactionOutput.raw_cbor` are filled only by the standalone
    // transaction-output path and by callers that wrap a whole transaction; the scrub clears them
    // regardless (the unit of the guard is "scrub, then verify").
    eprintln!(
        "preserved-bytes fields populated by a dataset decode: {seen:?}; not populated: {never:?}"
    );
    assert!(
        seen.len() >= 8,
        "the dataset decodes should populate most audited fields, got {seen:?}"
    );

    // The fields no dataset decode populates are set by hand and must be cleared all the same.
    let corpus = Corpus::load(&corpora, CborEra::Conway).unwrap();
    let mut block = corpus.rules["block"]
        .iter()
        .filter(|s| s.expectation == Expectation::Decodes)
        .find_map(|s| dec::block(&s.input, Era::Conway).ok())
        .expect("some Conway block sample decodes");
    block.raw_cbor = Some(vec![1]);
    assert!(block.populated().contains(&"Block.raw_cbor"));
    checked_scrub(&mut block);
    assert!(block.populated().is_empty());

    let mut sub = dugite_primitives::transaction::SubTransaction {
        raw_body_cbor: Some(vec![1]),
        ..Default::default()
    };
    assert_eq!(sub.populated(), vec!["SubTransaction.raw_body_cbor"]);
    checked_scrub(&mut sub);
    assert!(sub.populated().is_empty());

    let mut ws = dugite_primitives::transaction::TransactionWitnessSet {
        original_script_data_hash: Some(dugite_primitives::hash::Hash::from_bytes([7; 32])),
        raw_redeemers_cbor: Some(vec![1]),
        raw_plutus_data_cbor: Some(vec![1]),
        ..Default::default()
    };
    assert_eq!(ws.populated().len(), 3);
    checked_scrub(&mut ws);
    assert!(ws.populated().is_empty());
}

/// THE proof that reusing input bytes would make a wrong encoding look right, and that the
/// harness path does not reuse them.
///
/// An output whose inline datum is an INDEFINITE-length list.  The reference keeps the datum's
/// bytes (the datum is `bytes .cbor`, opaque to the normaliser), so `expected == input`.
///  * the encoder, left alone, reproduces the input byte for byte, from the preserved bytes;
///  * the harness path clears them first, so the encoder must rebuild the datum, and produces the
///    definite form: a different encoding, which the comparison then reports as a mismatch.
///
/// If the harness path ever stops scrubbing, the second assertion fails.
#[test]
fn reuse_of_preserved_bytes_would_make_a_wrong_encoding_look_right() {
    // {0: addr, 1: 1_000_000, 2: [1, #6.24(h'9f0102ff')]}   (inline datum [1, 2], indefinite)
    let mut input = vec![0xa3, 0x00, 0x58, 0x1d, 0x61];
    input.extend([0x11; 28]);
    input.extend([0x01, 0x1a, 0x00, 0x0f, 0x42, 0x40]);
    input.extend([0x02, 0x82, 0x01, 0xd8, 0x18, 0x44, 0x9f, 0x01, 0x02, 0xff]);

    // Left alone, the production encoder echoes the input from the preserved bytes.
    let decoded = dec::transaction_output(&input).unwrap();
    assert!(
        !decoded.populated().is_empty(),
        "the decoder must have preserved the datum bytes for this test to mean anything"
    );
    assert_eq!(
        enc::transaction_output(&decoded),
        input,
        "with the preserved bytes in place the encoder reproduces the input exactly"
    );

    // The harness path scrubs first and therefore rebuilds the datum.
    let codec = &registry(CborEra::Conway)["transaction_output"];
    let rebuilt = (codec.reencode)(&input).expect("the sample decodes and re-encodes");
    assert_ne!(
        rebuilt, input,
        "the harness path re-encoded to the input bytes: preserved bytes are being reused"
    );
    let mut definite = input.clone();
    let n = definite.len();
    definite.splice(n - 5.., [0x43, 0x82, 0x01, 0x02]);
    assert_eq!(
        rebuilt, definite,
        "the datum is rebuilt in the definite form"
    );

    // And against the reference (expected == input here) the harness path FAILS the sample,
    // where reuse would have passed it.
    let mut corpus = Corpus {
        era: CborEra::Conway,
        root: "synthetic".into(),
        rules: Default::default(),
    };
    corpus.rules.insert(
        "transaction_output".into(),
        vec![Sample {
            rule: "transaction_output".into(),
            category: "valid".into(),
            file: "reuse.input.cbor".into(),
            expectation: Expectation::Decodes,
            input: input.clone(),
            expected: Some(input),
        }],
    );
    let run = run_corpus(&corpus, &registry(CborEra::Conway));
    let outcome = &run.results["transaction_output/valid/reuse.input.cbor"];
    assert!(
        matches!(outcome, CborOutcome::Fail(m) if m.starts_with("encode:")),
        "{outcome:?}"
    );
}

/// A scrub that misses a field is a harness defect that unwinds out of the runner, never a
/// sample result.
#[test]
fn a_populated_field_after_scrubbing_is_a_harness_defect() {
    struct Leaky;
    impl Scrub for Leaky {
        fn scrub(&mut self) -> Vec<&'static str> {
            Vec::new()
        }
        fn populated(&self) -> Vec<&'static str> {
            vec!["Leaky.raw_cbor"]
        }
    }
    let result = std::panic::catch_unwind(|| checked_scrub(&mut Leaky));
    let payload = result.expect_err("must panic");
    let defect = payload
        .downcast_ref::<HarnessDefect>()
        .expect("a HarnessDefect payload");
    assert!(defect.0.contains("Leaky.raw_cbor"), "{}", defect.0);
}

/// A decode failure and an encode failure are told apart in the report's `decode:` / `encode:`
/// prefixes, and a clean item re-encodes.
#[test]
fn decode_and_encode_failures_carry_their_stage() {
    let codec = &registry(CborEra::Conway)["coin"];
    match (codec.reencode)(&[0x20]) {
        Err(Failure::Decode(_)) => {}
        other => panic!("a negative integer is refused at decode, got {other:?}"),
    }
    assert_eq!((codec.reencode)(&[0x18, 0x05]).unwrap(), vec![0x05]);
}
