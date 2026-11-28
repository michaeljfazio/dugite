//! Report envelopes, the writer, the error-report path and schema validation.
//!
//! The four scenarios do **not** share one envelope.  Their schemas (read from
//! `node-diversity-conformance` at the pinned commit) differ, and this module follows each:
//!
//! | scenario           | `version` | `scenario` | `generated_at`   | commits                                | `implementation`      | payload      |
//! |--------------------|-----------|------------|------------------|----------------------------------------|-----------------------|--------------|
//! | `ledger-rules`     | `"1"`     | yes        | RFC 3339 string  | `dataset_commit` (Amaru)               | `{name}`              | `scenarios`  |
//! | `cbor-decoding`    | `"2"`     | **no**     | integer ms       | `dataset_commit`, `implementation_commit` | **no** (flat commit) | `results`    |
//! | `uplc-conformance` | `"1"`     | yes        | RFC 3339 string  | none                                   | `{name}`              | `scenarios`  |
//! | `uplc-turbo`       | `"1"`     | yes        | RFC 3339 string  | none                                   | `{name, language}`    | `benchmarks` |
//!
//! `cbor-decoding` also names the protocol version (which gives the era) and is published once
//! per era.  Only `ledger-rules` allows an extra top-level `error` field, so only it can say
//! *in the report* that it carries no result; for the other three the empty payload is the
//! schema-valid form and the [`ReportStatus`] sidecar file records the truth.
//!
//! A report is **never** failed on its score.  Everything here that returns `Err` is a harness
//! failure: an unreadable schema, a report that does not match its schema, an unwritable path.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::ser::{Serialize, Serializer};
use serde_json::{json, Map, Value};

use super::pins::{pins_sha256, repo_root, Pins};

/// The environment variable that names the report file a runner writes.
pub const REPORT_ENV: &str = "DUGITE_NDC_REPORT";
/// Which era a `cbor-decoding` run reports (`conway`, the default, or `dijkstra`).
pub const CBOR_ERA_ENV: &str = "DUGITE_NDC_CBOR_ERA";
/// Overrides the implementation commit (the default is `git rev-parse HEAD` of this checkout).
pub const IMPLEMENTATION_COMMIT_ENV: &str = "DUGITE_NDC_IMPLEMENTATION_COMMIT";
/// `implementation.name` in every report.
pub const IMPLEMENTATION_NAME: &str = "dugite";

/// The four shared scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scenario {
    LedgerRules,
    CborDecoding,
    UplcConformance,
    UplcTurbo,
}

impl Scenario {
    pub const ALL: [Scenario; 4] = [
        Scenario::LedgerRules,
        Scenario::CborDecoding,
        Scenario::UplcConformance,
        Scenario::UplcTurbo,
    ];

    /// The scenario id, which is also its directory under NDC's `scenarios/`.
    pub fn id(self) -> &'static str {
        match self {
            Scenario::LedgerRules => "ledger-rules",
            Scenario::CborDecoding => "cbor-decoding",
            Scenario::UplcConformance => "uplc-conformance",
            Scenario::UplcTurbo => "uplc-turbo",
        }
    }

    pub fn from_id(id: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.id() == id)
    }

    /// `scenarios/<id>/schema.json` in the pinned NDC checkout under `corpora`.
    pub fn schema_path(self, corpora: &Path) -> PathBuf {
        corpora
            .join("ndc/scenarios")
            .join(self.id())
            .join("schema.json")
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// The era of a `cbor-decoding` report; the site deduces it from the protocol major version
/// (10 and 11 are Conway, 12 and up are Dijkstra).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CborEra {
    Conway,
    Dijkstra,
}

impl CborEra {
    pub const ALL: [CborEra; 2] = [CborEra::Conway, CborEra::Dijkstra];

    pub fn id(self) -> &'static str {
        match self {
            CborEra::Conway => "conway",
            CborEra::Dijkstra => "dijkstra",
        }
    }

    pub fn from_id(id: &str) -> Option<CborEra> {
        CborEra::ALL.into_iter().find(|e| e.id() == id)
    }

    /// The era from `DUGITE_NDC_CBOR_ERA`; Conway when unset.
    pub fn from_env() -> Result<CborEra, String> {
        match std::env::var(CBOR_ERA_ENV) {
            Ok(value) => CborEra::from_id(&value)
                .ok_or_else(|| format!("{CBOR_ERA_ENV}={value:?}: expected conway or dijkstra")),
            Err(_) => Ok(CborEra::Conway),
        }
    }

    /// The first protocol version of the era.  A real runner states the version it decoded with.
    pub fn default_protocol_version(self) -> &'static str {
        match self {
            CborEra::Conway => "10.0",
            CborEra::Dijkstra => "12.0",
        }
    }
}

/// What produced a report file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// A real result.
    Ok,
    /// A placeholder written by a runner that is not implemented yet.  Carries no result.
    Stub,
    /// A generator crashed or its inputs could not be prepared.  Carries no result.
    Error,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Ok => "ok",
            ReportStatus::Stub => "stub",
            ReportStatus::Error => "error",
        }
    }
}

/// Who ran, and when.  Shared by every report of one run.
#[derive(Debug, Clone)]
pub struct RunInfo {
    /// The 40-hex commit of the dugite checkout under test.
    pub implementation_commit: String,
    pub generated_at: DateTime<Utc>,
}

impl RunInfo {
    pub fn new(implementation_commit: impl Into<String>, generated_at: DateTime<Utc>) -> Self {
        RunInfo {
            implementation_commit: implementation_commit.into(),
            generated_at,
        }
    }

    /// The commit from `DUGITE_NDC_IMPLEMENTATION_COMMIT`, else `git rev-parse HEAD`; now as the time.
    pub fn detect() -> Result<Self, String> {
        let commit = match std::env::var(IMPLEMENTATION_COMMIT_ENV) {
            Ok(value) => value,
            Err(_) => {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(repo_root())
                    .output()
                    .map_err(|e| {
                        format!("cannot run git to find the implementation commit: {e}")
                    })?;
                if !out.status.success() {
                    return Err(format!(
                        "git rev-parse HEAD failed; set {IMPLEMENTATION_COMMIT_ENV}: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
        };
        if commit.len() != 40
            || !commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(format!(
                "implementation commit {commit:?} is not a full lowercase 40-hex commit"
            ));
        }
        Ok(RunInfo::new(commit, Utc::now()))
    }

    fn rfc3339(&self) -> String {
        self.generated_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

// ── per-case results: what a runner emits ───────────────────────────────────────────────────

/// One `ledger-rules` fixture result: `"pass"` or the name of the expected predicate failure.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LedgerRulesCase {
    /// The first five digits of the fixture file name.
    pub id: String,
    pub result: String,
}

/// One `cbor-decoding` sample outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborOutcome {
    /// The sample met its expectation (serialised as `true`).
    Pass,
    /// `"decode: <message>"` or `"encode: <message>"`.
    Fail(String),
}

impl Serialize for CborOutcome {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CborOutcome::Pass => s.serialize_bool(true),
            CborOutcome::Fail(message) => s.serialize_str(message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Budget {
    pub cpu: i64,
    pub mem: i64,
}

/// One `uplc-conformance` vector result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UplcCase {
    pub id: String,
    pub title: String,
    pub group: String,
    pub plutus_version: String,
    pub protocol_version: String,
    /// The textual UPLC result, or one of `evaluation failure`, `out of budget`, `parse/decode error`.
    pub evaluation: String,
    pub budget: Option<Budget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Machine details of an `uplc-turbo` run.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Machine {
    pub os: String,
    pub os_version: Option<String>,
    pub architecture: String,
    pub cpu: Option<String>,
    pub logical_cores: u32,
    pub memory_bytes: Option<u64>,
}

/// One `uplc-turbo` benchmark result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TurboBenchmark {
    pub bench: String,
    pub plutus_version: String,
    pub protocol_version: String,
    pub budget_cpu: Option<u64>,
    pub budget_mem: Option<u64>,
    pub cpu_us: f64,
    pub mem_bytes: Option<f64>,
}

// ── envelopes ───────────────────────────────────────────────────────────────────────────────

/// The `ledger-rules` report.  `error` is the one place a report can say it carries no result.
pub fn ledger_rules_report(
    pins: &Pins,
    run: &RunInfo,
    cases: &[LedgerRulesCase],
    error: Option<&str>,
) -> Value {
    let mut report = json!({
        "version": "1",
        "scenario": Scenario::LedgerRules.id(),
        "generated_at": run.rfc3339(),
        "dataset_commit": pins.ledger_rules_dataset_commit(),
        "implementation": { "name": IMPLEMENTATION_NAME },
        "scenarios": cases,
    });
    if let Some(error) = error {
        report["error"] = Value::String(error.to_string());
    }
    report
}

/// The `cbor-decoding` report (format version 2) for one era.  `results` is keyed by the sample
/// path, `<rule>/<category>/<file>.input.cbor`.
pub fn cbor_decoding_report(
    pins: &Pins,
    run: &RunInfo,
    protocol_version: &str,
    results: &BTreeMap<String, CborOutcome>,
) -> Value {
    json!({
        "version": "2",
        "generated_at": run.generated_at.timestamp_millis(),
        "protocol_version": protocol_version,
        "dataset_commit": pins.cbor_dataset_commit(),
        "implementation_commit": run.implementation_commit,
        "results": results,
    })
}

/// The `uplc-conformance` report.
pub fn uplc_conformance_report(run: &RunInfo, cases: &[UplcCase]) -> Value {
    json!({
        "version": "1",
        "scenario": Scenario::UplcConformance.id(),
        "generated_at": run.rfc3339(),
        "implementation": { "name": IMPLEMENTATION_NAME },
        "scenarios": cases,
    })
}

/// The `uplc-turbo` report.
pub fn uplc_turbo_report(
    run: &RunInfo,
    machine: Option<&Machine>,
    benchmarks: &[TurboBenchmark],
) -> Value {
    let mut report = json!({
        "version": "1",
        "scenario": Scenario::UplcTurbo.id(),
        "generated_at": run.rfc3339(),
        "implementation": { "name": IMPLEMENTATION_NAME, "language": "Rust" },
        "benchmarks": benchmarks,
    });
    if let Some(machine) = machine {
        report["machine"] = json!(machine);
    }
    report
}

/// The report of a generator that produced no result, in the form its schema accepts.
///
/// Dingo's pattern: a crashed generator writes an error report in place of its report, so the
/// scenario is still published.  The envelope is the scenario's own, with an empty payload (and,
/// for `ledger-rules`, the `error` field).  `era` is only used by `cbor-decoding`.
pub fn empty_report(
    scenario: Scenario,
    era: CborEra,
    pins: &Pins,
    run: &RunInfo,
    message: &str,
) -> Value {
    match scenario {
        Scenario::LedgerRules => ledger_rules_report(pins, run, &[], Some(message)),
        Scenario::CborDecoding => {
            cbor_decoding_report(pins, run, era.default_protocol_version(), &BTreeMap::new())
        }
        Scenario::UplcConformance => uplc_conformance_report(run, &[]),
        Scenario::UplcTurbo => uplc_turbo_report(run, None, &[]),
    }
}

// ── writing ─────────────────────────────────────────────────────────────────────────────────

/// The sidecar that says what produced a report: `<report>.status.json`.
pub fn status_path(report: &Path) -> PathBuf {
    let mut name = report.as_os_str().to_owned();
    name.push(".status.json");
    PathBuf::from(name)
}

/// Write a report and its status sidecar.  The sidecar is how a later publish step tells a real
/// result from a stub or an error report, which no schema can express for three of the four
/// scenarios.  It also binds the report to its inputs: the SHA-256 of the pin file and the dugite
/// commit it was made with, so a report left over from other pins or another commit cannot pass
/// for a current one (`score-reports.sh` checks both).
pub fn write_report(
    path: &Path,
    scenario: Scenario,
    status: ReportStatus,
    note: &str,
    run: &RunInfo,
    report: &Value,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut sidecar = Map::new();
    sidecar.insert("scenario".into(), scenario.id().into());
    sidecar.insert("status".into(), status.as_str().into());
    sidecar.insert("note".into(), note.into());
    sidecar.insert("pins_sha256".into(), pins_sha256()?.into());
    sidecar.insert(
        "implementation_commit".into(),
        run.implementation_commit.clone().into(),
    );
    let mut text =
        serde_json::to_string_pretty(&Value::Object(sidecar)).map_err(|e| e.to_string())?;
    text.push('\n');
    let side = status_path(path);
    std::fs::write(&side, text).map_err(|e| format!("cannot write {}: {e}", side.display()))
}

/// Run a generator and write its report; if it crashes (a panic, or an `Err` for inputs that
/// could not be prepared) write an error report in its place, so the scenario still has a
/// report.  The generator returns the status, a note and the report it built.
///
/// This never fails on a *score*.  It returns `Err` only when the report cannot be written.
pub fn write_generated(
    path: &Path,
    scenario: Scenario,
    era: CborEra,
    pins: &Pins,
    run: &RunInfo,
    generate: impl FnOnce() -> Result<(ReportStatus, String, Value), String> + std::panic::UnwindSafe,
) -> Result<ReportStatus, String> {
    let outcome = match std::panic::catch_unwind(generate) {
        Ok(result) => result,
        Err(payload) => {
            let reason = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into());
            Err(format!("generator panicked: {reason}"))
        }
    };
    match outcome {
        Ok((status, note, report)) => {
            write_report(path, scenario, status, &note, run, &report)?;
            Ok(status)
        }
        Err(message) => {
            let report = empty_report(scenario, era, pins, run, &message);
            write_report(path, scenario, ReportStatus::Error, &message, run, &report)?;
            Ok(ReportStatus::Error)
        }
    }
}

/// The harness verdict on a written report: an `error` report is on disk (so the scenario can
/// still be published as a failure) but the run must FAIL.  A stub is not an error; a score never is.
pub fn ensure_not_error(status: ReportStatus, path: &Path) -> Result<(), String> {
    if status == ReportStatus::Error {
        Err(format!(
            "the generator failed; an error report was written to {} and the run fails (see its .status.json)",
            path.display()
        ))
    } else {
        Ok(())
    }
}

// ── validation ──────────────────────────────────────────────────────────────────────────────

/// Validate `instance` against a JSON Schema (draft 2020-12, `format` assertions on, so a
/// malformed `date-time` is an error).  Returns every violation, with its instance path.
pub fn validate_against(schema: &Value, instance: &Value) -> Result<(), Vec<String>> {
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(true)
        .build(schema)
        .map_err(|e| vec![format!("schema does not compile: {e}")])?;
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate a report against the scenario's own `schema.json` from the pinned NDC checkout.
pub fn validate_report(corpora: &Path, scenario: Scenario, report: &Value) -> Result<(), String> {
    let schema_path = scenario.schema_path(corpora);
    let text = std::fs::read_to_string(&schema_path).map_err(|e| {
        format!(
            "cannot read {} ({e}); run scripts/node-diversity/fetch-corpora.sh",
            schema_path.display()
        )
    })?;
    let schema: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", schema_path.display()))?;
    validate_against(&schema, report).map_err(|errors| {
        format!(
            "{scenario} report does not match {}:\n  {}",
            schema_path.display(),
            errors.join("\n  ")
        )
    })
}

/// Read a report back from disk and validate it: what the file says is what is checked.
pub fn validate_report_file(
    corpora: &Path,
    scenario: Scenario,
    path: &Path,
) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read report {}: {e}", path.display()))?;
    let report: Value = serde_json::from_str(&text)
        .map_err(|e| format!("report {} is not JSON: {e}", path.display()))?;
    validate_report(corpora, scenario, &report)?;
    Ok(report)
}
