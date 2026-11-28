//! Node-diversity conformance runners and harness tests (#1114, Phase 1: #1116).
//!
//! * `ndc_report_*` are the four scenario runners.  Each reads `DUGITE_NDC_REPORT=<path>`, writes
//!   its report there, then reads the file back and validates it against the scenario's own
//!   `schema.json` from the pinned NDC checkout.  **Reporting never fails on a score**; only a
//!   harness failure fails (corpus not fetched, schema mismatch, unwritable path).  Without
//!   `DUGITE_NDC_REPORT` they do nothing (and say so), so a plain `cargo nextest run` needs no
//!   corpus.  `just ndc-report <scenario>` checks the file exists afterwards.
//! * Phase 1 ships each runner as a **stub**: the report is schema-valid and carries no result.
//!   Phases 2a-2d of #1114 replace the stub bodies.
//! * Everything else tests the harness itself.  Tests that need the fetched corpus run only when
//!   it is there, and fail instead of skipping when `DUGITE_NDC_REQUIRE=1` (the just recipes set it).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use chrono::{TimeZone, Utc};
use dugite_conformance::node_diversity::pins::{corpora_dir, repo_root, Pins};
use dugite_conformance::node_diversity::report::*;
use serde_json::{json, Value};

const REQUIRE_ENV: &str = "DUGITE_NDC_REQUIRE";

// ── helpers ─────────────────────────────────────────────────────────────────────────────────

fn pins() -> Pins {
    Pins::load_default().expect("corpora.toml must parse")
}

fn fixed_run() -> RunInfo {
    RunInfo::new(
        "0123456789abcdef0123456789abcdef01234567",
        Utc.with_ymd_and_hms(2026, 11, 28, 12, 30, 45).unwrap(),
    )
}

/// The fetched corpora, or None (loudly) when they are absent and not required.
fn corpora_or_skip(test: &str) -> Option<PathBuf> {
    let corpora = corpora_dir();
    let have = Scenario::ALL
        .iter()
        .all(|s| s.schema_path(&corpora).is_file());
    if have {
        return Some(corpora);
    }
    if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
        panic!(
            "{test}: {REQUIRE_ENV}=1 but the NDC schemas are missing under {}; \
             run scripts/node-diversity/fetch-corpora.sh",
            corpora.display()
        );
    }
    eprintln!(
        "SKIPPED {test}: corpora not fetched under {} (scripts/node-diversity/fetch-corpora.sh)",
        corpora.display()
    );
    None
}

fn stub_note(scenario: Scenario) -> &'static str {
    match scenario {
        Scenario::LedgerRules => "ledger-rules runner not implemented yet (#1114 Phase 2b); this report carries no result",
        Scenario::CborDecoding => "cbor-decoding runner not implemented yet (#1114 Phase 2c); this report carries no result",
        Scenario::UplcConformance => "uplc-conformance runner not implemented yet (#1114 Phase 2a); this report carries no result",
        Scenario::UplcTurbo => "uplc-turbo runner not implemented yet (#1114 Phase 2d); this report carries no result",
    }
}

/// The body of a runner.  Phases 2a-2d replace the stub arms with real results.
fn generate(
    scenario: Scenario,
    era: CborEra,
    pins: &Pins,
    run: &RunInfo,
) -> Result<(ReportStatus, String, Value), String> {
    if scenario == Scenario::UplcConformance {
        return generate_uplc_conformance(run);
    }
    let note = stub_note(scenario).to_string();
    Ok((
        ReportStatus::Stub,
        note.clone(),
        empty_report(scenario, era, pins, run, &note),
    ))
}

/// `uplc-conformance` (#1114 Phase 2a): every vector through dugite's production gated
/// evaluation path.  The runner lives in `dugite_conformance::node_diversity::uplc_conformance`.
fn generate_uplc_conformance(run: &RunInfo) -> Result<(ReportStatus, String, Value), String> {
    use dugite_conformance::node_diversity::uplc_conformance::run_all;
    let result = run_all(&corpora_dir())?;
    let note = format!(
        "{} vectors evaluated through the production gated path; {} evaluator panics, {} internal errors",
        result.cases.len(),
        result.panics.len(),
        result.internal.len()
    );
    for line in result.panics.iter().chain(result.internal.iter()) {
        eprintln!("uplc-conformance: {line}");
    }
    Ok((
        ReportStatus::Ok,
        note,
        uplc_conformance_report(run, &result.cases),
    ))
}

/// What every `ndc_report_*` test does.
fn run_scenario(test: &str, scenario: Scenario) {
    let Some(path) = std::env::var_os(REPORT_ENV).map(PathBuf::from) else {
        eprintln!("{test}: {REPORT_ENV} not set, nothing to report (just ndc-report {scenario})");
        return;
    };
    let corpora = corpora_dir();
    assert!(
        scenario.schema_path(&corpora).is_file(),
        "{test}: {} is missing; run scripts/node-diversity/fetch-corpora.sh",
        scenario.schema_path(&corpora).display()
    );
    let pins = pins();
    let run = RunInfo::detect().expect("implementation commit");
    let era = CborEra::from_env().expect("era");
    let status = write_generated(&path, scenario, era, &pins, &run, || {
        generate(scenario, era, &pins, &run)
    })
    .expect("the report must be writable");
    // The file on disk is what gets published, so it is the file that is validated.
    validate_report_file(&corpora, scenario, &path).unwrap_or_else(|e| panic!("{e}"));
    eprintln!(
        "{test}: wrote {} ({}), schema-valid",
        path.display(),
        status.as_str()
    );
    // The error report is on disk and valid, so the scenario can still be published; the RUN fails.
    ensure_not_error(status, &path).unwrap_or_else(|e| panic!("{test}: {e}"));
}

// ── the four runners (stubs in Phase 1) ─────────────────────────────────────────────────────

#[test]
fn ndc_report_ledger_rules() {
    run_scenario("ndc_report_ledger_rules", Scenario::LedgerRules);
}

#[test]
fn ndc_report_cbor_decoding() {
    run_scenario("ndc_report_cbor_decoding", Scenario::CborDecoding);
}

#[test]
fn ndc_report_uplc_conformance() {
    run_scenario("ndc_report_uplc_conformance", Scenario::UplcConformance);
}

#[test]
fn ndc_report_uplc_turbo() {
    run_scenario("ndc_report_uplc_turbo", Scenario::UplcTurbo);
}

// ── harness: pins ───────────────────────────────────────────────────────────────────────────

#[test]
fn ndc_pins_file_is_well_formed() {
    let pins = pins();
    // The numbers the fetch script measures against the real fixtures (verified there too).
    assert_eq!(pins.ledger_rules.missing_protocol_parameters.len(), 3);
    assert_eq!(pins.fixtures_using_missing_parameters(), 32);
    assert_eq!(pins.ledger_rules.known_divergences.len(), 10);
    assert_eq!(pins.ledger_rules.ndc_only_files.len(), 3);
    // The two commits a report names are the pinned ones.
    assert_eq!(
        pins.ledger_rules_dataset_commit(),
        "40ba995081b2b066cab497eade9abfbc0e350970"
    );
    assert_eq!(pins.cbor_dataset_commit().len(), 40);
}

#[test]
fn ndc_pins_reject_corruption() {
    let text =
        std::fs::read_to_string(dugite_conformance::node_diversity::pins::default_pins_path())
            .unwrap();
    let ndc = pins().ndc.commit;
    for (what, corrupted) in [
        ("short commit", text.replacen(&ndc, &ndc[..39], 1)),
        (
            "uppercase commit",
            text.replacen(&ndc, &ndc.to_uppercase(), 1),
        ),
        (
            "short digest",
            text.replacen(&pins().uplc_turbo.sha256, "abc123", 1),
        ),
        (
            "non-https url",
            text.replacen("https://pub-", "http://pub-", 1),
        ),
        (
            "bad divergence id",
            text.replacen("\"00016\" =", "\"16\" =", 1),
        ),
    ] {
        assert_ne!(corrupted, text, "{what}: the corruption changed nothing");
        assert!(Pins::parse(&corrupted).is_err(), "{what} was accepted");
    }
}

#[test]
fn fetch_script_rejects_corrupted_pins() {
    // Offline self-test of scripts/node-diversity/fetch-corpora.sh: each corrupted pin must exit
    // non-zero, naming the cause.  Needs git, zstd, tar and python3.
    let out = Command::new("bash")
        .arg(repo_root().join("scripts/node-diversity/test-fetch-corpora.sh"))
        .output()
        .expect("bash must run the self-test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "fetch-corpora self-test failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("cbor-commit-flipped") && stdout.contains("uplc-sha-flipped"),
        "the self-test did not run its corrupted-pin cases:\n{stdout}"
    );
}

// ── harness: envelopes ──────────────────────────────────────────────────────────────────────

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

#[test]
fn ndc_envelopes_use_each_scenarios_own_field_names() {
    let pins = pins();
    let run = fixed_run();

    let ledger = ledger_rules_report(
        &pins,
        &run,
        &[LedgerRulesCase {
            id: "00001".into(),
            result: "pass".into(),
        }],
        None,
    );
    assert_eq!(
        keys(&ledger),
        [
            "dataset_commit",
            "generated_at",
            "implementation",
            "scenario",
            "scenarios",
            "version"
        ]
    );
    assert_eq!(ledger["version"], "1");
    assert_eq!(ledger["scenario"], "ledger-rules");
    assert_eq!(ledger["generated_at"], "2026-11-28T12:30:45Z");
    assert_eq!(ledger["dataset_commit"], pins.ledger_rules_dataset_commit());
    assert_eq!(ledger["implementation"], json!({"name": "dugite"}));
    assert_eq!(
        ledger["scenarios"],
        json!([{"id": "00001", "result": "pass"}])
    );

    let cbor = cbor_decoding_report(
        &pins,
        &run,
        "10.0",
        &BTreeMap::from([
            ("a/valid/1.input.cbor".to_string(), CborOutcome::Pass),
            (
                "a/invalid-zap-1/2.input.cbor".to_string(),
                CborOutcome::Fail("decode: boom".into()),
            ),
        ]),
    );
    // Version 2: no `scenario`, no `implementation` object, integer milliseconds.
    assert_eq!(
        keys(&cbor),
        [
            "dataset_commit",
            "generated_at",
            "implementation_commit",
            "protocol_version",
            "results",
            "version"
        ]
    );
    assert_eq!(cbor["version"], "2");
    assert_eq!(cbor["generated_at"], run.generated_at.timestamp_millis());
    assert_eq!(cbor["dataset_commit"], pins.cbor_dataset_commit());
    assert_eq!(cbor["implementation_commit"], run.implementation_commit);
    assert_eq!(
        cbor["results"],
        json!({"a/valid/1.input.cbor": true, "a/invalid-zap-1/2.input.cbor": "decode: boom"})
    );

    let uplc = uplc_conformance_report(&run, &[]);
    assert_eq!(
        keys(&uplc),
        [
            "generated_at",
            "implementation",
            "scenario",
            "scenarios",
            "version"
        ]
    );
    assert_eq!(uplc["scenario"], "uplc-conformance");

    let turbo = uplc_turbo_report(&run, None, &[]);
    assert_eq!(
        keys(&turbo),
        [
            "benchmarks",
            "generated_at",
            "implementation",
            "scenario",
            "version"
        ]
    );
    assert_eq!(
        turbo["implementation"],
        json!({"name": "dugite", "language": "Rust"})
    );
}

#[test]
fn ndc_validator_rejects_a_corrupted_report_against_an_inline_schema() {
    // No corpus needed: proves `validate_against` goes red on a corrupted field.
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["version", "generated_at"],
        "properties": {
            "version": {"const": "1"},
            "generated_at": {"type": "string", "format": "date-time"}
        }
    });
    let good = json!({"version": "1", "generated_at": "2026-11-28T12:30:45Z"});
    validate_against(&schema, &good).expect("a valid report passes");
    for (what, bad) in [
        (
            "wrong const",
            json!({"version": "2", "generated_at": "2026-11-28T12:30:45Z"}),
        ),
        (
            "bad date-time",
            json!({"version": "1", "generated_at": "yesterday"}),
        ),
        ("missing field", json!({"version": "1"})),
    ] {
        assert!(
            validate_against(&schema, &bad).is_err(),
            "{what} was accepted"
        );
    }
}

#[test]
fn ndc_crashed_generator_writes_an_error_report() {
    let dir = tempfile::tempdir().unwrap();
    let pins = pins();
    let run = fixed_run();
    for scenario in Scenario::ALL {
        let path = dir.path().join(format!("{scenario}.json"));
        let status = write_generated(&path, scenario, CborEra::Conway, &pins, &run, || {
            if scenario == Scenario::UplcTurbo {
                Err("corpus could not be prepared".to_string())
            } else {
                panic!("generator crashed")
            }
        })
        .unwrap();
        assert_eq!(status, ReportStatus::Error, "{scenario}");
        // The report is written, and the run still fails.
        assert!(path.is_file(), "{scenario}: no error report was written");
        let verdict = ensure_not_error(status, &path);
        assert!(
            verdict.is_err(),
            "{scenario}: an error report did not fail the run"
        );
        let side: Value =
            serde_json::from_str(&std::fs::read_to_string(status_path(&path)).unwrap()).unwrap();
        assert_eq!(side["status"], "error");
        assert_eq!(side["scenario"], scenario.id());
        // The sidecar binds the report to its inputs.
        assert_eq!(side["implementation_commit"], run.implementation_commit);
        assert_eq!(
            side["pins_sha256"],
            dugite_conformance::node_diversity::pins::pins_sha256().unwrap()
        );
        let note = side["note"].as_str().unwrap();
        assert!(
            note.contains("generator panicked: generator crashed")
                || note.contains("corpus could not be prepared"),
            "{scenario}: {note}"
        );
        let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if scenario == Scenario::LedgerRules {
            assert_eq!(report["error"], note);
        }
        // The payload is empty: an error report is never mistaken for a result.
        let payload = match scenario {
            Scenario::LedgerRules | Scenario::UplcConformance => &report["scenarios"],
            Scenario::CborDecoding => &report["results"],
            Scenario::UplcTurbo => &report["benchmarks"],
        };
        let empty = match payload {
            Value::Array(items) => items.is_empty(),
            Value::Object(map) => map.is_empty(),
            other => panic!("{scenario}: unexpected payload {other}"),
        };
        assert!(empty, "{scenario}: payload not empty");
    }
}

// ── harness: the schemas of the pinned NDC checkout ─────────────────────────────────────────

#[test]
fn ndc_stub_and_error_reports_match_every_schema() {
    let Some(corpora) = corpora_or_skip("ndc_stub_and_error_reports_match_every_schema") else {
        return;
    };
    let pins = pins();
    let run = fixed_run();
    for scenario in Scenario::ALL {
        for era in CborEra::ALL {
            let report = empty_report(scenario, era, &pins, &run, "no result");
            validate_report(&corpora, scenario, &report).unwrap_or_else(|e| panic!("{e}"));
        }
    }
    // A populated report of each scenario also matches: the per-case structs are the schema's.
    let ledger = ledger_rules_report(
        &pins,
        &run,
        &[
            LedgerRulesCase {
                id: "00001".into(),
                result: "pass".into(),
            },
            LedgerRulesCase {
                id: "00002".into(),
                result: "BadInputsUTxO".into(),
            },
        ],
        None,
    );
    validate_report(&corpora, Scenario::LedgerRules, &ledger).unwrap();
    let cbor = cbor_decoding_report(
        &pins,
        &run,
        "11.0",
        &BTreeMap::from([
            ("a/valid/1.input.cbor".to_string(), CborOutcome::Pass),
            (
                "a/manual-invalid/2.input.cbor".to_string(),
                CborOutcome::Fail("encode: x".into()),
            ),
        ]),
    );
    validate_report(&corpora, Scenario::CborDecoding, &cbor).unwrap();
    let uplc = uplc_conformance_report(
        &run,
        &[UplcCase {
            id: "x_v1".into(),
            title: "x".into(),
            group: "g".into(),
            plutus_version: "V1".into(),
            protocol_version: "10.0".into(),
            evaluation: "(program 1.0.0 (con integer 1))".into(),
            budget: Some(Budget { cpu: 1, mem: 2 }),
            error: None,
        }],
    );
    validate_report(&corpora, Scenario::UplcConformance, &uplc).unwrap();
    let machine = Machine {
        os: "linux".into(),
        architecture: "x86_64".into(),
        logical_cores: 4,
        ..Machine::default()
    };
    let turbo = uplc_turbo_report(&run, Some(&machine), &[]);
    validate_report(&corpora, Scenario::UplcTurbo, &turbo).unwrap();
}

#[test]
fn ndc_schema_validation_goes_red_on_a_corrupted_report() {
    let Some(corpora) = corpora_or_skip("ndc_schema_validation_goes_red_on_a_corrupted_report")
    else {
        return;
    };
    let pins = pins();
    let run = fixed_run();
    let mut cases: Vec<(Scenario, &str, Value)> = Vec::new();
    for scenario in Scenario::ALL {
        let good = empty_report(scenario, CborEra::Conway, &pins, &run, "no result");
        validate_report(&corpora, scenario, &good).expect("the control report is valid");

        let corrupt = |mutate: &dyn Fn(&mut Value)| {
            let mut report = good.clone();
            mutate(&mut report);
            report
        };
        cases.push((scenario, "version", corrupt(&|r| r["version"] = json!("9"))));
        cases.push((
            scenario,
            "generated_at",
            corrupt(&|r| r["generated_at"] = json!("not a timestamp")),
        ));
        match scenario {
            Scenario::LedgerRules => {
                cases.push((
                    scenario,
                    "result name",
                    corrupt(&|r| {
                        r["scenarios"] = json!([{"id": "00001", "result": "NotAPredicate"}]);
                    }),
                ));
                cases.push((
                    scenario,
                    "dataset_commit",
                    corrupt(&|r| r["dataset_commit"] = json!("abc")),
                ));
                cases.push((
                    scenario,
                    "payload removed",
                    corrupt(&|r| {
                        r.as_object_mut().unwrap().remove("scenarios");
                    }),
                ));
            }
            Scenario::CborDecoding => {
                cases.push((
                    scenario,
                    "sample path",
                    corrupt(&|r| {
                        r["results"] = json!({"a/valid/1.cbor": true});
                    }),
                ));
                cases.push((
                    scenario,
                    "error class",
                    corrupt(&|r| {
                        r["results"] = json!({"a/valid/1.input.cbor": "oops: x"});
                    }),
                ));
                cases.push((
                    scenario,
                    "implementation_commit",
                    corrupt(&|r| {
                        r["implementation_commit"] = json!("xyz");
                    }),
                ));
                cases.push((
                    scenario,
                    "unknown field",
                    corrupt(&|r| r["scenario"] = json!("cbor-decoding")),
                ));
            }
            Scenario::UplcConformance => {
                cases.push((
                    scenario,
                    "case missing fields",
                    corrupt(&|r| {
                        r["scenarios"] = json!([{"id": "x"}]);
                    }),
                ));
                cases.push((
                    scenario,
                    "scenario id",
                    corrupt(&|r| r["scenario"] = json!("ledger-rules")),
                ));
            }
            Scenario::UplcTurbo => {
                cases.push((
                    scenario,
                    "unknown bench",
                    corrupt(&|r| {
                        r["benchmarks"] = json!([{
                            "bench": "not-a-corpus-program", "plutus_version": "V1",
                            "protocol_version": "10", "budget_cpu": 1, "budget_mem": 1,
                            "cpu_us": 1.0, "mem_bytes": 1.0
                        }]);
                    }),
                ));
                cases.push((
                    scenario,
                    "unknown field",
                    corrupt(&|r| r["error"] = json!("x")),
                ));
            }
        }
    }
    for (scenario, what, report) in cases {
        assert!(
            validate_report(&corpora, scenario, &report).is_err(),
            "{scenario}: a corrupted report ({what}) was accepted by its schema"
        );
    }
}
