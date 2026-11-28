//! The `cbor-decoding` runner (#1114 Phase 2c, #1126).
//!
//! For every sample of the pinned cardano-cbor-dataset (`<era>/<rule>/<category>/*.input.cbor`):
//!
//! * a sample that must DECODE (`valid`, `manual-valid`) is decoded by dugite's production decoder
//!   for its rule, re-encoded by dugite's production encoder, and compared with `.expected.cbor`
//!   (after normalisation, except for the byte-exact rules, which compare exactly);
//! * a sample that must be REFUSED (`invalid-zap-<n>`, `manual-invalid`) must fail to decode.
//!
//! The report's per-sample value is just `true` or an error string, so the harness is the only
//! check on how the result was reached.  The guards, all published in the report's `.guards.json`
//! sidecar (the report schema has no room for them):
//!
//! * **No input-byte reuse.**  The encoder prefers preserved original bytes in several places.
//!   [`scrub`] clears every such field between decode and encode, and PANICS (a harness failure,
//!   never a sample failure) if one is still populated.
//! * **Discriminating samples.**  A valid sample whose input equals its `.expected.cbor` cannot
//!   tell a re-encoder from a pass-through.  Only samples whose input DIFFERS are discriminating;
//!   their count is published, with how many of them pass.  Because the comparison normalises
//!   first, a stricter count is published too: the samples that echoing the input cannot pass
//!   (`normalize(input) != expected`), which only a real decode-and-rebuild gets right.
//! * **A rule's invalid samples count only if its valid samples decode.**  A decoder that
//!   rejects everything would otherwise score every invalid sample.
//! * **A rule with no registered decoder FAILS**, valid and invalid samples alike.
//! * **The protocol version is stated** in each report.

pub mod corpus;
pub mod normalize;
pub mod registry;
pub mod scrub;

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use serde_json::{json, Value};

use self::corpus::{Corpus, Expectation, Sample};
use self::registry::{registry, Codec, Failure};
use crate::node_diversity::pins::Pins;
use crate::node_diversity::report::{cbor_decoding_report, CborEra, CborOutcome, RunInfo};

/// The protocol version dugite's decoders are run under, per era.  Dugite's decoders take an
/// `Era`, not a protocol version, so this is the version the dataset's reference run used for the
/// era (`eraProtVerHigh`), which is also the version dugite's release bar decodes mainnet under.
pub fn protocol_version(era: CborEra) -> &'static str {
    match era {
        CborEra::Conway => "11.0",
        CborEra::Dijkstra => "12.0",
    }
}

/// The rules whose re-encoding must be the input bytes themselves (`mkByteExactRule` and
/// `mkByteExactAnnotatorRule` in the dataset's `app/Generation/EraSpec.hs`).  For these the
/// `.expected.cbor` file holds the input bytes and the comparison is exact, not normalised.
/// [`check_dataset_self_consistency`] verifies this list against the dataset.
pub const BYTE_EXACT_RULES: [&str; 7] = [
    "header",
    "transaction_body",
    "transaction_witness_set",
    "native_script",
    "plutus_data",
    "redeemers",
    "auxiliary_data",
];

/// Samples whose `.expected.cbor` differs from `normalize(input)` because the reference decoded a
/// Byron address and re-encoded the CBOR INSIDE its byte string (the payload is itself a CBOR
/// item, `[#6.24(bytes), crc]`, here with a wide head).  The dataset's normalisation does not look
/// inside byte strings, so these are exempt from the self-check by name.
pub const BYRON_ADDRESS_REENCODED: [&str; 1] =
    ["address/manual-valid/byron-payload-array-with-a-wide-head.input.cbor"];

/// A harness defect: it must fail the run, not become a sample failure.
#[derive(Debug)]
pub struct HarnessDefect(pub String);

/// Everything that goes into a rule's guard summary.
#[derive(Debug, Default, Clone)]
pub struct RuleStats {
    pub has_decoder: bool,
    pub valid_total: usize,
    pub valid_decoded: usize,
    pub valid_pass: usize,
    /// Valid samples whose input differs from `.expected.cbor`.
    pub discriminating_total: usize,
    pub discriminating_pass: usize,
    /// Valid samples that echoing the input cannot pass: `normalize(input)` (the input itself for
    /// a byte-exact rule) is not the expected bytes, so only a decoder that decoded AND a
    /// re-encoder that rebuilt the value from it gets them right.  A subset of the above.
    pub echo_proof_total: usize,
    pub echo_proof_pass: usize,
    pub invalid_total: usize,
    /// Invalid samples the decoder refused, whether or not they were counted.
    pub invalid_refused: usize,
    /// Invalid samples that pass in the report (refused AND the rule's valid samples decode).
    pub invalid_pass: usize,
}

impl RuleStats {
    /// Whether every valid sample of the rule decodes (and at least one exists): the gate on the
    /// rule's invalid samples.
    pub fn valid_samples_decode(&self) -> bool {
        self.valid_total > 0 && self.valid_decoded == self.valid_total
    }
}

/// The result of running one era.
#[derive(Debug)]
pub struct EraRun {
    pub results: BTreeMap<String, CborOutcome>,
    pub rules: BTreeMap<String, RuleStats>,
    pub missing_decoders: Vec<String>,
}

fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

/// Run `f`, turning a panic into a message, EXCEPT a harness defect, which keeps unwinding.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => {
            if let Some(defect) = payload.downcast_ref::<HarnessDefect>() {
                std::panic::resume_unwind(Box::new(HarnessDefect(defect.0.clone())));
            }
            Err(format!("panicked: {}", panic_text(&payload)))
        }
    }
}

/// What the decoder did with one sample, before the report-level gating.
enum Raw {
    Pass,
    Fail(String),
}

/// Set `DUGITE_NDC_CBOR_EXPLAIN=<substring of a sample key>` to print the input, the expected
/// bytes and what was produced for every mismatching sample whose key contains it (triage aid).
fn explain(sample: &Sample, actual: &[u8]) {
    let Ok(filter) = std::env::var("DUGITE_NDC_CBOR_EXPLAIN") else {
        return;
    };
    if !sample.key().contains(&filter) {
        return;
    }
    let expected = sample.expected.as_deref().unwrap_or_default();
    let first_diff = actual
        .iter()
        .zip(expected)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| actual.len().min(expected.len()));
    eprintln!(
        "EXPLAIN {}\n  input    {}\n  expected {}\n  actual   {}\n  first difference at byte {first_diff} (actual {} bytes, expected {} bytes)",
        sample.key(),
        hex::encode(&sample.input),
        hex::encode(expected),
        hex::encode(actual),
        actual.len(),
        expected.len()
    );
}

fn run_valid(sample: &Sample, codec: &Codec, byte_exact: bool) -> (bool, Raw) {
    let expected = sample
        .expected
        .as_deref()
        .expect("valid sample has expected");
    match guarded(|| (codec.reencode)(&sample.input)) {
        Err(panic) => (false, Raw::Fail(format!("decode: {panic}"))),
        Ok(Err(Failure::Decode(message))) => (false, Raw::Fail(format!("decode: {message}"))),
        Ok(Err(Failure::Encode(message))) => (true, Raw::Fail(format!("encode: {message}"))),
        Ok(Ok(reencoded)) => {
            let comparable = if byte_exact {
                Ok(reencoded)
            } else {
                normalize::normalize(&reencoded)
            };
            match comparable {
                Err(e) => (
                    true,
                    Raw::Fail(format!("encode: the re-encoding is not valid CBOR: {e}")),
                ),
                Ok(actual) if actual == expected => (true, Raw::Pass),
                Ok(actual) => {
                    explain(sample, &actual);
                    (
                        true,
                        Raw::Fail("encode: re-encoded bytes do not match expected".into()),
                    )
                }
            }
        }
    }
}

/// Whether a decoder that accepted the sample and re-emitted its INPUT would pass it.
fn echo_passes(sample: &Sample, byte_exact: bool) -> bool {
    let Some(expected) = sample.expected.as_deref() else {
        return false;
    };
    if byte_exact {
        sample.input == expected
    } else {
        normalize::normalize(&sample.input).is_ok_and(|n| n == expected)
    }
}

fn run_invalid(sample: &Sample, codec: &Codec) -> Raw {
    match guarded(|| (codec.decode)(&sample.input)) {
        Err(panic) => Raw::Fail(format!("decode: {panic}")),
        Ok(Err(_)) => Raw::Pass,
        Ok(Ok(())) => Raw::Fail("decode: succeeded when expected to fail".into()),
    }
}

/// Run every sample of `corpus` through `registry`.
pub fn run_corpus(corpus: &Corpus, registry: &BTreeMap<&'static str, Codec>) -> EraRun {
    let mut results = BTreeMap::new();
    let mut rules = BTreeMap::new();
    let mut missing = Vec::new();
    for (rule, samples) in &corpus.rules {
        let codec = registry.get(rule.as_str());
        let byte_exact = BYTE_EXACT_RULES.contains(&rule.as_str());
        let mut stats = RuleStats {
            has_decoder: codec.is_some(),
            ..RuleStats::default()
        };
        if codec.is_none() {
            missing.push(rule.clone());
        }
        // Valid samples first: they decide whether the invalid ones are counted.
        let mut invalid: Vec<(&Sample, Raw)> = Vec::new();
        for sample in samples {
            match (sample.expectation, codec) {
                (Expectation::Decodes, None) => {
                    stats.valid_total += 1;
                    results.insert(
                        sample.key(),
                        CborOutcome::Fail(format!("decode: no decoder registered for rule {rule}")),
                    );
                    if sample.expected.as_deref() != Some(sample.input.as_slice()) {
                        stats.discriminating_total += 1;
                    }
                    if !echo_passes(sample, byte_exact) {
                        stats.echo_proof_total += 1;
                    }
                }
                (Expectation::Decodes, Some(codec)) => {
                    stats.valid_total += 1;
                    let discriminating =
                        sample.expected.as_deref() != Some(sample.input.as_slice());
                    if discriminating {
                        stats.discriminating_total += 1;
                    }
                    let echo_proof = !echo_passes(sample, byte_exact);
                    if echo_proof {
                        stats.echo_proof_total += 1;
                    }
                    let (decoded, raw) = run_valid(sample, codec, byte_exact);
                    if decoded {
                        stats.valid_decoded += 1;
                    }
                    results.insert(
                        sample.key(),
                        match raw {
                            Raw::Pass => {
                                stats.valid_pass += 1;
                                if discriminating {
                                    stats.discriminating_pass += 1;
                                }
                                if echo_proof {
                                    stats.echo_proof_pass += 1;
                                }
                                CborOutcome::Pass
                            }
                            Raw::Fail(m) => CborOutcome::Fail(m),
                        },
                    );
                }
                (Expectation::Refused, None) => {
                    stats.invalid_total += 1;
                    results.insert(
                        sample.key(),
                        CborOutcome::Fail(format!("decode: no decoder registered for rule {rule}")),
                    );
                }
                (Expectation::Refused, Some(codec)) => {
                    stats.invalid_total += 1;
                    invalid.push((sample, run_invalid(sample, codec)));
                }
            }
        }
        for (sample, raw) in invalid {
            let outcome = match raw {
                Raw::Fail(m) => CborOutcome::Fail(m),
                Raw::Pass => {
                    stats.invalid_refused += 1;
                    if stats.valid_samples_decode() {
                        stats.invalid_pass += 1;
                        CborOutcome::Pass
                    } else {
                        CborOutcome::Fail(format!(
                            "decode: refused, but not counted: only {} of {} valid samples of rule {rule} decode",
                            stats.valid_decoded, stats.valid_total
                        ))
                    }
                }
            };
            results.insert(sample.key(), outcome);
        }
        rules.insert(rule.clone(), stats);
    }
    EraRun {
        results,
        rules,
        missing_decoders: missing,
    }
}

/// How a valid sample's `.expected.cbor` relates to its input under this harness's normaliser.
#[derive(Debug, Default)]
pub struct Consistency {
    /// `expected == normalize(input)` (or `== input` for a byte-exact rule).
    pub exact: usize,
    /// Equal only after sorting map entries: the reference re-encoded a `Map`.  Per rule.
    pub map_reordered: BTreeMap<String, usize>,
    /// Equal only in the weak [`normalize::normalize_loose`] form: ordering, set tags and
    /// duplicates differ (a Haskell `Set` re-encoding).  Per rule.
    pub up_to_ordering: BTreeMap<String, usize>,
    /// Samples exempted by name: the reference re-encoded CBOR inside a byte string that is not
    /// an embedded-CBOR tag (see [`BYRON_ADDRESS_REENCODED`]).
    pub reencoded_inside_bytes: usize,
    /// The rest.  Any entry here is a harness defect.
    pub unexplained: Vec<String>,
}

/// Check the dataset against this harness: every valid sample's `.expected.cbor` must equal
/// `normalize(input)` (the input itself for a byte-exact rule), or at least agree once map
/// entries are sorted (the reference decoded into a `Map`).  This verifies the normaliser on
/// tens of thousands of real pairs; anything else is a harness defect.
pub fn check_dataset_self_consistency(corpus: &Corpus) -> Result<Consistency, String> {
    let mut out = Consistency::default();
    for (rule, samples) in &corpus.rules {
        let byte_exact = BYTE_EXACT_RULES.contains(&rule.as_str());
        for sample in samples {
            let Some(expected) = sample.expected.as_deref() else {
                continue;
            };
            if byte_exact {
                if sample.input == expected {
                    out.exact += 1;
                } else {
                    out.unexplained.push(format!(
                        "{}: byte-exact rule, expected differs from input",
                        sample.key()
                    ));
                }
                continue;
            }
            if BYRON_ADDRESS_REENCODED.contains(&sample.key().as_str()) {
                out.reencoded_inside_bytes += 1;
                continue;
            }
            match normalize::normalize(&sample.input) {
                Ok(bytes) if bytes == expected => out.exact += 1,
                Ok(_) => match (
                    normalize::normalize_sorted(&sample.input),
                    normalize::normalize_sorted(expected),
                ) {
                    (Ok(a), Ok(b)) if a == b => {
                        *out.map_reordered.entry(rule.clone()).or_default() += 1;
                    }
                    _ => match (
                        normalize::normalize_loose(&sample.input),
                        normalize::normalize_loose(expected),
                    ) {
                        (Ok(a), Ok(b)) if a == b => {
                            *out.up_to_ordering.entry(rule.clone()).or_default() += 1;
                        }
                        _ => out.unexplained.push(format!(
                            "{}: expected differs from normalize(input)",
                            sample.key()
                        )),
                    },
                },
                Err(e) => out
                    .unexplained
                    .push(format!("{}: input does not normalise: {e}", sample.key())),
            }
        }
    }
    if out.unexplained.is_empty() {
        Ok(out)
    } else {
        let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
        for u in &out.unexplained {
            *by_rule
                .entry(u.split('/').next().unwrap_or("?"))
                .or_default() += 1;
        }
        let mut examples: Vec<String> = Vec::new();
        let mut per_rule: BTreeMap<&str, usize> = BTreeMap::new();
        for u in &out.unexplained {
            let rule = u.split('/').next().unwrap_or("?");
            let n = per_rule.entry(rule).or_default();
            if *n < 2 {
                examples.push(u.clone());
            }
            *n += 1;
        }
        let shown = format!("{by_rule:?}\n  {}", examples.join("\n  "));
        Err(format!(
            "{} of the dataset's valid samples disagree with the harness's normaliser (harness defect):\n  {shown}",
            out.unexplained.len()
        ))
    }
}

/// The published numbers of one era run.
pub fn guards_json(era: CborEra, corpus: &Corpus, run: &EraRun) -> Value {
    let mut totals = RuleStats::default();
    let mut per_rule = serde_json::Map::new();
    for (rule, s) in &run.rules {
        totals.valid_total += s.valid_total;
        totals.valid_decoded += s.valid_decoded;
        totals.valid_pass += s.valid_pass;
        totals.discriminating_total += s.discriminating_total;
        totals.discriminating_pass += s.discriminating_pass;
        totals.echo_proof_total += s.echo_proof_total;
        totals.echo_proof_pass += s.echo_proof_pass;
        totals.invalid_total += s.invalid_total;
        totals.invalid_refused += s.invalid_refused;
        totals.invalid_pass += s.invalid_pass;
        per_rule.insert(
            rule.clone(),
            json!({
                "has_decoder": s.has_decoder,
                "valid_total": s.valid_total,
                "valid_decoded": s.valid_decoded,
                "valid_pass": s.valid_pass,
                "discriminating_total": s.discriminating_total,
                "discriminating_pass": s.discriminating_pass,
                "echo_proof_total": s.echo_proof_total,
                "echo_proof_pass": s.echo_proof_pass,
                "invalid_total": s.invalid_total,
                "invalid_refused": s.invalid_refused,
                "invalid_pass": s.invalid_pass,
                "invalid_counted": s.valid_samples_decode(),
            }),
        );
    }
    let passed = run
        .results
        .values()
        .filter(|o| **o == CborOutcome::Pass)
        .count();
    json!({
        "era": era.id(),
        "protocol_version": protocol_version(era),
        "rules_in_dataset": corpus.rules.len(),
        "rules_with_decoder": corpus.rules.len() - run.missing_decoders.len(),
        "rules_without_decoder": run.missing_decoders,
        "samples": corpus.sample_count(),
        "passed": passed,
        "valid_samples": totals.valid_total,
        "valid_samples_decoded": totals.valid_decoded,
        "valid_samples_passed": totals.valid_pass,
        "discriminating_valid_samples": totals.discriminating_total,
        "discriminating_valid_samples_passed": totals.discriminating_pass,
        "echo_proof_valid_samples": totals.echo_proof_total,
        "echo_proof_valid_samples_passed": totals.echo_proof_pass,
        "invalid_samples": totals.invalid_total,
        "invalid_samples_refused_by_decoder": totals.invalid_refused,
        "invalid_samples_counted_in_report": totals.invalid_pass,
        "byte_exact_rules": BYTE_EXACT_RULES,
        "rules": per_rule,
    })
}

/// One line for the status sidecar's note.
pub fn summary(guards: &Value) -> String {
    let g = |k: &str| guards[k].as_u64().unwrap_or(0);
    format!(
        "{era} (protocol {pv}): {passed}/{samples} samples pass; {with}/{rules} rules have a decoder; \
         {disc} of {valid} valid samples have input != expected ({disc_pass} pass), \
         {echo} cannot be passed by echoing the input ({echo_pass} pass); \
         {inv_pass} of {inv_refused} refused invalid samples counted ({inv} invalid in all)",
        era = guards["era"].as_str().unwrap_or("?"),
        pv = guards["protocol_version"].as_str().unwrap_or("?"),
        passed = g("passed"),
        samples = g("samples"),
        with = g("rules_with_decoder"),
        rules = g("rules_in_dataset"),
        disc = g("discriminating_valid_samples"),
        valid = g("valid_samples"),
        disc_pass = g("discriminating_valid_samples_passed"),
        echo = g("echo_proof_valid_samples"),
        echo_pass = g("echo_proof_valid_samples_passed"),
        inv_pass = g("invalid_samples_counted_in_report"),
        inv_refused = g("invalid_samples_refused_by_decoder"),
        inv = g("invalid_samples"),
    )
}

/// What the `ndc_report_cbor_decoding` runner calls: load the era's corpus, check it against the
/// harness, run it, and build the report plus the guard summary.
pub fn generate(
    corpora: &Path,
    era: CborEra,
    pins: &Pins,
    run_info: &RunInfo,
) -> Result<(Value, Value), String> {
    let corpus = Corpus::load(corpora, era)?;
    let consistency = check_dataset_self_consistency(&corpus)?;
    let registry = registry(era);
    let unknown: Vec<&str> = registry
        .keys()
        .copied()
        .filter(|rule| !corpus.rules.contains_key(*rule))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "the registry names rules the {} dataset does not have: {}",
            era.id(),
            unknown.join(", ")
        ));
    }
    let result = run_corpus(&corpus, &registry);
    let report = cbor_decoding_report(pins, run_info, protocol_version(era), &result.results);
    let mut guards = guards_json(era, &corpus, &result);
    guards["dataset_self_check"] = json!({
        "valid_samples_equal_to_normalize_of_input": consistency.exact,
        "valid_samples_equal_after_sorting_map_entries": consistency.map_reordered.values().sum::<usize>(),
        "rules_whose_reference_sorts_maps": consistency.map_reordered,
        "valid_samples_reencoded_inside_bytes_exempt_by_name": consistency.reencoded_inside_bytes,
        "valid_samples_equal_only_up_to_ordering": consistency.up_to_ordering.values().sum::<usize>(),
        "rules_whose_reference_reorders_sets": consistency.up_to_ordering,
    });
    Ok((report, guards))
}

/// The body of the `cbor-decoding` runner: build the era's report, write the guard summary next
/// to the report file (`<report>.guards.json`) when `DUGITE_NDC_REPORT` names one, and return the
/// status, a one-line note for the status sidecar and the report.
pub fn generate_for_runner(
    corpora: &Path,
    era: CborEra,
    pins: &Pins,
    run_info: &RunInfo,
) -> Result<(crate::node_diversity::report::ReportStatus, String, Value), String> {
    let (report, guards) = generate(corpora, era, pins, run_info)?;
    let note = summary(&guards);
    if let Some(path) = std::env::var_os(crate::node_diversity::report::REPORT_ENV) {
        let mut name = std::path::PathBuf::from(path).into_os_string();
        name.push(".guards.json");
        let mut text = serde_json::to_string_pretty(&guards).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&name, text).map_err(|e| {
            format!(
                "cannot write {}: {e}",
                std::path::Path::new(&name).display()
            )
        })?;
    }
    eprintln!("cbor-decoding: {note}");
    Ok((
        crate::node_diversity::report::ReportStatus::Ok,
        note,
        report,
    ))
}

#[cfg(test)]
mod tests {
    //! The harness's guards, proved on synthetic corpora (no dataset needed): each test builds a
    //! tiny corpus and a codec with a known behaviour, and checks what the report says.

    use super::registry::Failure;
    use super::*;

    fn sample(
        rule: &str,
        category: &str,
        name: &str,
        input: &[u8],
        expected: Option<&[u8]>,
    ) -> Sample {
        Sample {
            rule: rule.into(),
            category: category.into(),
            file: format!("{name}.input.cbor"),
            expectation: corpus::expectation_of(category).unwrap(),
            input: input.to_vec(),
            expected: expected.map(<[u8]>::to_vec),
        }
    }

    fn corpus_of(rule: &str, samples: Vec<Sample>) -> Corpus {
        Corpus {
            era: CborEra::Conway,
            root: "synthetic".into(),
            rules: BTreeMap::from([(rule.to_string(), samples)]),
        }
    }

    /// Accepts exactly the inputs for which `accept` is true and re-encodes them as themselves.
    fn codec(accept: fn(&[u8]) -> bool) -> Codec {
        Codec {
            decode: Box::new(move |b| {
                if accept(b) {
                    Ok(())
                } else {
                    Err("refused".into())
                }
            }),
            reencode: Box::new(move |b| {
                if accept(b) {
                    Ok(b.to_vec())
                } else {
                    Err(Failure::Decode("refused".into()))
                }
            }),
        }
    }

    fn registry_of(rule: &'static str, codec: Codec) -> BTreeMap<&'static str, Codec> {
        BTreeMap::from([(rule, codec)])
    }

    fn mixed_corpus() -> Corpus {
        corpus_of(
            "r",
            vec![
                // A valid sample whose expected file equals its input: it cannot discriminate.
                sample("r", "valid", "v1", &[0x01], Some(&[0x01])),
                // One whose input differs from expected (an indefinite array): it can.
                sample("r", "valid", "v2", &[0x9f, 0x01, 0xff], Some(&[0x81, 0x01])),
                sample("r", "manual-invalid", "i1", &[0xf6], None),
                sample("r", "invalid-zap-1", "i2", &[0xf5], None),
            ],
        )
    }

    fn outcome(run: &EraRun, key: &str) -> CborOutcome {
        run.results
            .get(key)
            .unwrap_or_else(|| panic!("no result for {key}"))
            .clone()
    }

    /// Red-proof: a decoder that rejects EVERYTHING must not score the invalid samples.
    #[test]
    fn a_decoder_that_rejects_everything_scores_no_invalid_samples() {
        let corpus = mixed_corpus();
        let run = run_corpus(&corpus, &registry_of("r", codec(|_| false)));
        let stats = &run.rules["r"];
        assert_eq!(stats.valid_decoded, 0);
        assert_eq!(stats.invalid_refused, 2, "the decoder does refuse both");
        assert_eq!(stats.invalid_pass, 0, "but none may be counted");
        for key in [
            "r/manual-invalid/i1.input.cbor",
            "r/invalid-zap-1/i2.input.cbor",
        ] {
            match outcome(&run, key) {
                CborOutcome::Fail(m) => {
                    assert!(m.starts_with("decode: refused, but not counted"), "{m}")
                }
                CborOutcome::Pass => panic!("{key} scored by a decoder that rejects everything"),
            }
        }
        assert!(run.results.values().all(|o| *o != CborOutcome::Pass));
    }

    /// The same decoder, once it accepts the valid samples, does score the invalid ones it refuses
    /// (so the guard above is the gate, not a blanket zero).
    #[test]
    fn a_decoder_that_decodes_the_valid_samples_scores_the_invalid_ones_it_refuses() {
        let corpus = mixed_corpus();
        let run = run_corpus(
            &corpus,
            &registry_of("r", codec(|b| b == [0x01] || b == [0x9f, 0x01, 0xff])),
        );
        assert_eq!(run.rules["r"].invalid_pass, 2);
        assert_eq!(
            outcome(&run, "r/manual-invalid/i1.input.cbor"),
            CborOutcome::Pass
        );
    }

    /// A decoder that accepts everything fails every invalid sample.
    #[test]
    fn a_decoder_that_accepts_everything_fails_every_invalid_sample() {
        let corpus = mixed_corpus();
        let run = run_corpus(&corpus, &registry_of("r", codec(|_| true)));
        assert_eq!(run.rules["r"].invalid_pass, 0);
        assert_eq!(
            outcome(&run, "r/manual-invalid/i1.input.cbor"),
            CborOutcome::Fail("decode: succeeded when expected to fail".into())
        );
    }

    /// Red-proof: a rule with no registered decoder is a FAILURE for every sample, valid or
    /// invalid, and is counted as missing.
    #[test]
    fn an_unregistered_rule_fails_every_sample() {
        let corpus = mixed_corpus();
        let run = run_corpus(&corpus, &BTreeMap::new());
        assert_eq!(run.missing_decoders, vec!["r".to_string()]);
        assert_eq!(run.results.len(), 4);
        for (key, o) in &run.results {
            match o {
                CborOutcome::Fail(m) => {
                    assert!(m.contains("no decoder registered for rule r"), "{key}: {m}")
                }
                CborOutcome::Pass => panic!("{key} passed with no decoder"),
            }
        }
        assert!(!run.rules["r"].has_decoder);
    }

    /// The discriminating-sample count is the number of valid samples whose input differs from
    /// the expected bytes, whatever the decoder does, and the passes among them are counted apart.
    /// The stricter echo-proof count is the samples an echo of the input cannot pass.
    #[test]
    fn discriminating_samples_are_counted() {
        let corpus = corpus_of(
            "r",
            vec![
                // Input == expected: discriminates nothing.
                sample("r", "valid", "v1", &[0x01], Some(&[0x01])),
                // Input != expected, but only in form: normalising the input gives expected, so
                // an echo passes it.
                sample("r", "valid", "v2", &[0x9f, 0x01, 0xff], Some(&[0x81, 0x01])),
                // Input != expected in content (the reference reordered): an echo fails it.
                sample(
                    "r",
                    "valid",
                    "v3",
                    &[0x82, 0x02, 0x01],
                    Some(&[0x82, 0x01, 0x02]),
                ),
            ],
        );
        let echo = run_corpus(&corpus, &registry_of("r", codec(|_| true)));
        let stats = &echo.rules["r"];
        assert_eq!(stats.valid_total, 3);
        assert_eq!(
            stats.discriminating_total, 2,
            "v2 and v3 have input != expected"
        );
        assert_eq!(
            stats.discriminating_pass, 1,
            "the echo passes v2 after normalisation"
        );
        assert_eq!(
            stats.echo_proof_total, 1,
            "only v3 cannot be passed by echoing"
        );
        assert_eq!(stats.echo_proof_pass, 0, "the echo does not pass v3");
        assert_eq!(stats.valid_pass, 2);
        // A decoder that really rebuilds the value passes v3 too.
        let rebuilding = Codec {
            decode: Box::new(|_| Ok(())),
            reencode: Box::new(|b| {
                Ok(if b == [0x82, 0x02, 0x01] {
                    vec![0x82, 0x01, 0x02]
                } else {
                    b.to_vec()
                })
            }),
        };
        let run = run_corpus(&corpus, &registry_of("r", rebuilding));
        assert_eq!(run.rules["r"].echo_proof_pass, 1);
        assert_eq!(run.rules["r"].discriminating_pass, 2);
        let guards = guards_json(CborEra::Conway, &corpus, &run);
        assert_eq!(guards["discriminating_valid_samples"], 2);
        assert_eq!(guards["discriminating_valid_samples_passed"], 2);
        assert_eq!(guards["echo_proof_valid_samples"], 1);
        assert_eq!(guards["echo_proof_valid_samples_passed"], 1);
        assert_eq!(guards["protocol_version"], "11.0");
    }

    /// A byte-exact rule compares exactly, a normalised rule compares after normalisation.
    #[test]
    fn byte_exact_rules_compare_exactly_and_the_others_after_normalising() {
        let indefinite = |rule: &str| {
            corpus_of(
                rule,
                vec![sample(
                    rule,
                    "valid",
                    "v",
                    &[0x9f, 0x01, 0xff],
                    Some(&[0x9f, 0x01, 0xff]),
                )],
            )
        };
        // Re-encoding as the definite form: equal to expected only after normalisation.
        let definite = || Codec {
            decode: Box::new(|_| Ok(())),
            reencode: Box::new(|_| Ok(vec![0x81, 0x01])),
        };
        let plain = {
            let c = corpus_of(
                "coin",
                vec![sample(
                    "coin",
                    "valid",
                    "v",
                    &[0x9f, 0x01, 0xff],
                    Some(&[0x81, 0x01]),
                )],
            );
            run_corpus(&c, &registry_of("coin", definite()))
        };
        assert_eq!(
            outcome(&plain, "coin/valid/v.input.cbor"),
            CborOutcome::Pass
        );
        let exact = run_corpus(
            &indefinite("plutus_data"),
            &registry_of("plutus_data", definite()),
        );
        assert_eq!(
            outcome(&exact, "plutus_data/valid/v.input.cbor"),
            CborOutcome::Fail("encode: re-encoded bytes do not match expected".into()),
            "plutus_data is byte-exact: the definite form is not the original bytes"
        );
    }

    /// A decoder that panics is a sample failure, not a harness failure; a harness defect is not
    /// swallowed.
    #[test]
    fn a_panicking_decoder_fails_its_sample_and_a_harness_defect_unwinds() {
        let corpus = mixed_corpus();
        let panicking = Codec {
            decode: Box::new(|_| panic!("boom")),
            reencode: Box::new(|_| panic!("boom")),
        };
        let run = run_corpus(&corpus, &registry_of("r", panicking));
        match outcome(&run, "r/valid/v1.input.cbor") {
            CborOutcome::Fail(m) => assert!(m.starts_with("decode: panicked: boom"), "{m}"),
            CborOutcome::Pass => panic!("a panic passed"),
        }
        let defective = Codec {
            decode: Box::new(|_| Ok(())),
            reencode: Box::new(|_| std::panic::panic_any(HarnessDefect("scrub".into()))),
        };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_corpus(&mixed_corpus(), &registry_of("r", defective))
        }));
        let payload = caught.expect_err("a harness defect must unwind out of the run");
        assert!(payload.downcast_ref::<HarnessDefect>().is_some());
    }

    /// An unknown category directory is a harness failure (the site stops on it).
    #[test]
    fn unknown_categories_are_rejected() {
        assert_eq!(corpus::expectation_of("valid"), Some(Expectation::Decodes));
        assert_eq!(
            corpus::expectation_of("manual-valid"),
            Some(Expectation::Decodes)
        );
        assert_eq!(
            corpus::expectation_of("manual-invalid"),
            Some(Expectation::Refused)
        );
        assert_eq!(
            corpus::expectation_of("invalid-zap-0"),
            Some(Expectation::Refused)
        );
        assert_eq!(corpus::expectation_of("invalid-zap-"), None);
        assert_eq!(corpus::expectation_of("invalid-zap-x"), None);
        assert_eq!(corpus::expectation_of("manual"), None);
        assert_eq!(corpus::expectation_of("mutated"), None);
    }
}
