//! The `uplc-conformance` scenario runner (#1114 Phase 2a).
//!
//! Every vector of the corpus is evaluated through dugite's production, protocol-version- and
//! Plutus-language-gated evaluation path ([`dugite_uplc::eval_redeemer::eval_script_bytes_gated`]
//! for vectors that ship flat bytes, [`dugite_uplc::eval_redeemer::eval_program_gated`] for the
//! ones that only exist as text) with the corpus' cost model for the vector's protocol version and
//! Plutus language and the scenario's budget of 10B CPU / 14M MEM.  The runner emits the observed
//! outcome only: the textual CEK result and the consumed budget, or one of the scenario's three
//! error classes and a `null` budget.  Whether an outcome is a success is the scorer's business
//! (`just ndc-score`), never the runner's: nothing here reads an expected result.
//!
//! * The list of vectors, and each vector's `id`, `title`, `group`, Plutus version and protocol
//!   version, is NDC's `expectations.json` (the scenario's changelog names it the authoritative
//!   list); the vector's files are found from those fields.  A vector that cannot be found, or a
//!   corpus vector no expectation names, is a harness failure.
//! * A vector with a `.flat` file is run from the flat bytes (the ledger-facing decoder); the
//!   ones without (constants flat cannot carry, such as BLS elements, and inputs that are
//!   ill-typed by construction) are parsed from the `.uplc` text.
//! * The three error classes: `parse/decode error` = the program never became a runnable program
//!   (text parse, CBOR/flat decode, or the protocol-version availability rules); `out of budget` =
//!   the machine ran out of the budget; `evaluation failure` = everything else that stops a run.
//!   An evaluator panic is reported as `evaluator panic`, which no expectation names, so it can
//!   never score; it is also counted in the summary.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use dugite_uplc::cost_models::CostModels;
use dugite_uplc::eval_redeemer::{eval_program_gated, eval_script_bytes_gated, ProgramEvalError};
use dugite_uplc::machine::ExBudget;
use dugite_uplc::redeemer_resolve::ScriptLanguage;
use dugite_uplc::{Program, UplcError};
use serde_json::Value;

use super::report::{Budget, UplcCase};
use super::uplc_print::{print_program, readback};

/// The scenario's evaluation budget.
pub const MAX_CPU: i64 = 10_000_000_000;
pub const MAX_MEM: i64 = 14_000_000;

pub const EVAL_FAILURE: &str = "evaluation failure";
pub const OUT_OF_BUDGET: &str = "out of budget";
pub const PARSE_DECODE_ERROR: &str = "parse/decode error";
/// The three classes below are NOT the scenario's, on purpose: no expectation names them, so a
/// vector that lands in one can never score, and a run that produces one FAILS (an error report).
/// A panic, a dugite-internal or encoder error, and a decoder arm that says it is not yet wired are
/// dugite defects or gaps, not outcomes of the program; folding them into `evaluation failure` or
/// `parse/decode error` would let them score as a correct rejection.
pub const EVALUATOR_PANIC: &str = "evaluator panic";
pub const EVALUATOR_INTERNAL: &str = "evaluator internal error";
pub const DECODER_NOT_WIRED: &str = "decoder not yet wired";

/// One vector of the corpus, located from an `expectations.json` entry.
#[derive(Debug, Clone)]
pub struct Vector {
    pub id: String,
    pub title: String,
    pub group: String,
    pub plutus_version: String,
    pub protocol_version: String,
    /// `1`..`3` and the protocol major version, from the two version fields.
    pub language: ScriptLanguage,
    pub major_pv: u32,
    /// `<corpus>/protocol-version-N/vM/<group>/<dir>`; the files inside are named `<dir>.*`.
    pub dir: PathBuf,
    pub stem: String,
}

impl Vector {
    fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.stem))
    }
}

fn normalise_name(s: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for ch in s.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(ch);
        } else {
            gap = true;
        }
    }
    out
}

fn str_field<'a>(entry: &'a Value, key: &str) -> Result<&'a str, String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("expectation {entry} has no string field `{key}`"))
}

/// The scenario's vectors: `expectations.json` entries located in the fetched corpus.
pub fn load_vectors(corpora: &Path) -> Result<Vec<Vector>, String> {
    let expectations = corpora.join("ndc/scenarios/uplc-conformance/expectations.json");
    let text = std::fs::read_to_string(&expectations)
        .map_err(|e| format!("cannot read {}: {e}", expectations.display()))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", expectations.display()))?;
    let entries = doc
        .get("scenarios")
        .and_then(Value::as_array)
        .ok_or("expectations.json has no `scenarios` array")?;
    let root = corpora.join("uplc-conformance");

    let mut vectors = Vec::with_capacity(entries.len());
    let mut claimed = BTreeSet::new();
    for entry in entries {
        let id = str_field(entry, "id")?.to_string();
        let title = str_field(entry, "title")?.to_string();
        let group = str_field(entry, "group")?.to_string();
        let plutus_version = str_field(entry, "plutus_version")?.to_string();
        let protocol_version = str_field(entry, "protocol_version")?.to_string();

        let language = match plutus_version.as_str() {
            "V1" => ScriptLanguage::PlutusV1,
            "V2" => ScriptLanguage::PlutusV2,
            "V3" => ScriptLanguage::PlutusV3,
            other => return Err(format!("{id}: unknown plutus_version {other:?}")),
        };
        let major_pv: u32 = protocol_version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(|| format!("{id}: unreadable protocol_version {protocol_version:?}"))?;

        let group_dir = root
            .join(format!("protocol-version-{major_pv}"))
            .join(plutus_version.to_lowercase())
            .join(&group);
        let wanted = normalise_name(&title);
        let mut candidates = Vec::new();
        let listing = std::fs::read_dir(&group_dir)
            .map_err(|e| format!("{id}: cannot list {}: {e}", group_dir.display()))?;
        for item in listing {
            let item = item.map_err(|e| format!("{id}: {e}"))?;
            let name = item.file_name().to_string_lossy().into_owned();
            if item.path().is_dir() && normalise_name(&name) == wanted {
                candidates.push(name);
            }
        }
        let stem = match candidates.as_slice() {
            [one] => one.clone(),
            other => {
                return Err(format!(
                    "{id}: {} directories under {} match title {title:?}: {other:?}",
                    other.len(),
                    group_dir.display()
                ))
            }
        };
        let dir = group_dir.join(&stem);
        if !claimed.insert(dir.clone()) {
            return Err(format!(
                "{id}: {} is claimed by two expectations",
                dir.display()
            ));
        }
        let vector = Vector {
            id,
            title,
            group,
            plutus_version,
            protocol_version,
            language,
            major_pv,
            dir,
            stem,
        };
        if !vector.file("uplc").is_file() {
            return Err(format!(
                "{}: {} does not exist",
                vector.id,
                vector.file("uplc").display()
            ));
        }
        vectors.push(vector);
    }

    // The other direction: a corpus vector no expectation names would be silently unscored.
    let mut in_corpus = BTreeSet::new();
    collect_uplc_dirs(&root, &mut in_corpus)?;
    let orphans: Vec<_> = in_corpus.difference(&claimed).collect();
    if !orphans.is_empty() {
        return Err(format!(
            "{} corpus vectors have no expectation (first: {}): the corpus and expectations.json \
             have drifted",
            orphans.len(),
            orphans[0].display()
        ));
    }
    Ok(vectors)
}

fn collect_uplc_dirs(dir: &Path, out: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    let listing =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for item in listing {
        let item = item.map_err(|e| format!("{e}"))?;
        let path = item.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("cost-models") {
                continue;
            }
            collect_uplc_dirs(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("uplc") {
            if let Some(parent) = path.parent() {
                out.insert(parent.to_path_buf());
            }
        }
    }
    Ok(())
}

/// The corpus' cost models, keyed by `(protocol major, plutus version)`.
pub fn load_cost_models(corpora: &Path) -> Result<BTreeMap<(u32, String), CostModels>, String> {
    let mut out = BTreeMap::new();
    for pv in [10u32, 11] {
        let mut models = CostModels::default();
        for (version, name) in [(1, "v1"), (2, "v2"), (3, "v3")] {
            let path = corpora
                .join("uplc-conformance/cost-models")
                .join(format!("protocol-version-{pv}"))
                .join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let params: Vec<i64> = serde_json::from_str(&text)
                .map_err(|e| format!("{} is not an array of integers: {e}", path.display()))?;
            match version {
                1 => models.plutus_v1 = Some(params),
                2 => models.plutus_v2 = Some(params),
                _ => models.plutus_v3 = Some(params),
            }
        }
        // One `CostModels` per language so a vector is only ever charged by ITS language's model.
        for (version, name) in [(1, "V1"), (2, "V2"), (3, "V3")] {
            let mut only = CostModels::default();
            match version {
                1 => only.plutus_v1 = models.plutus_v1.clone(),
                2 => only.plutus_v2 = models.plutus_v2.clone(),
                _ => only.plutus_v3 = models.plutus_v3.clone(),
            }
            out.insert((pv, name.to_string()), only);
        }
    }
    Ok(out)
}

/// The on-chain script form of flat bytes: one CBOR byte string around them.
pub fn cbor_wrap(flat: &[u8]) -> Vec<u8> {
    let len = flat.len();
    let mut out = Vec::with_capacity(len + 9);
    match len {
        0..=23 => out.push(0x40 | len as u8),
        24..=0xff => out.extend([0x58, len as u8]),
        0x100..=0xffff => {
            out.push(0x59);
            out.extend((len as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0x5a);
            out.extend((len as u32).to_be_bytes());
        }
        _ => {
            out.push(0x5b);
            out.extend((len as u64).to_be_bytes());
        }
    }
    out.extend(flat);
    out
}

/// NDC's own comparison of two evaluation texts (its `build.py` `normalize_evaluation`): whitespace
/// around punctuation is presentation, the contents of a string are not.
pub fn normalise_evaluation(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            tokens.push(chars[start..i].iter().collect());
        } else if "()[],{}".contains(c) {
            tokens.push(c.to_string());
            i += 1;
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && !"()[],{}\"".contains(chars[i]) {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        }
    }
    let mut out = String::new();
    for token in tokens {
        let glue_left = matches!(token.as_str(), ")" | "]" | "}" | ",");
        let after_open = out.ends_with(['(', '[', '{', ',']);
        if !out.is_empty() && !glue_left && !after_open {
            out.push(' ');
        }
        out.push_str(&token);
    }
    out
}

/// One NDC expectation: the observable result only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub evaluation: String,
    pub budget: Option<(i64, i64)>,
}

/// Would NDC's scorer pass this report entry against this expectation?  (Evaluation text equal
/// after normalisation, and the budget equal wherever one is expected.)
pub fn scores(case: &UplcCase, expected: &Expectation) -> bool {
    normalise_evaluation(&case.evaluation) == normalise_evaluation(&expected.evaluation)
        && (expected.budget.is_none() || case.budget.map(|b| (b.cpu, b.mem)) == expected.budget)
}

/// `expectations.json`, keyed by `(id, protocol_version, plutus_version)`.  For CHECKING a run;
/// the runner itself never reads an expected result.
pub fn load_expectations(
    corpora: &Path,
) -> Result<BTreeMap<(String, String, String), Expectation>, String> {
    let path = corpora.join("ndc/scenarios/uplc-conformance/expectations.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{e}"))?;
    let mut out = BTreeMap::new();
    for e in doc["scenarios"].as_array().ok_or("no scenarios array")? {
        let key = (
            str_field(e, "id")?.to_string(),
            str_field(e, "protocol_version")?.to_string(),
            str_field(e, "plutus_version")?.to_string(),
        );
        let budget = e["budget"].as_object().map(|b| {
            (
                b["cpu"].as_i64().unwrap_or(-1),
                b["mem"].as_i64().unwrap_or(-1),
            )
        });
        out.insert(
            key,
            Expectation {
                evaluation: str_field(e, "evaluation")?.to_string(),
                budget,
            },
        );
    }
    Ok(out)
}

/// What running one vector produced, before it becomes a report entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub evaluation: String,
    pub budget: Option<(i64, i64)>,
    /// The evaluator's own message, for the report's `error` field and for triage.
    pub error: Option<String>,
}

impl Observed {
    fn failure(class: &str, error: impl std::fmt::Display) -> Self {
        Observed {
            evaluation: class.to_string(),
            budget: None,
            error: Some(error.to_string()),
        }
    }
}

/// Whether `class` is one of the scenario's three, i.e. can score.
pub fn is_scenario_class(class: &str) -> bool {
    matches!(class, EVAL_FAILURE | OUT_OF_BUDGET | PARSE_DECODE_ERROR)
}

/// The class of an error raised BEFORE the machine started (decode, availability rules).
fn classify_decode(error: &UplcError) -> &'static str {
    match error {
        // The decoder's own wording for an arm it does not implement.
        UplcError::FlatDecode(m) | UplcError::CborDecode(m) if m.contains("not yet wired") => {
            DECODER_NOT_WIRED
        }
        UplcError::FlatDecode(_) | UplcError::CborDecode(_) => PARSE_DECODE_ERROR,
        UplcError::Internal(_) | UplcError::Encode(_) => EVALUATOR_INTERNAL,
        _ => EVAL_FAILURE,
    }
}

/// The class of an error raised by the machine.
fn classify(error: &UplcError) -> &'static str {
    match error {
        UplcError::BudgetExhausted { .. } => OUT_OF_BUDGET,
        UplcError::FlatDecode(_) | UplcError::CborDecode(_) => classify_decode(error),
        // dugite bugs, never an outcome of the program.
        UplcError::Internal(_) | UplcError::Encode(_) => EVALUATOR_INTERNAL,
        // A script error, builtin type error or failure, machine error, free variable...: the
        // program was runnable and did not produce a value.
        _ => EVAL_FAILURE,
    }
}

/// Run one vector.  `Err` is a harness failure (an unreadable file, an unusable cost model, a
/// result that cannot be printed); a failing program is an `Ok` observation.
pub fn run_vector(
    v: &Vector,
    cost_models: &BTreeMap<(u32, String), CostModels>,
) -> Result<Observed, String> {
    let models = cost_models
        .get(&(v.major_pv, v.plutus_version.clone()))
        .ok_or_else(|| {
            format!(
                "{}: no cost model for PV{} {}",
                v.id, v.major_pv, v.plutus_version
            )
        })?;
    let budget = ExBudget {
        cpu: MAX_CPU,
        mem: MAX_MEM,
    };

    let flat_path = v.file("flat");
    // The version the result is printed under is the program's own.
    let (outcome, input_program) = if flat_path.is_file() {
        // The corpus stores each `.flat` as hex text, not as raw bytes.
        let hex_text = std::fs::read_to_string(&flat_path)
            .map_err(|e| format!("{}: cannot read {}: {e}", v.id, flat_path.display()))?;
        let flat = hex::decode(hex_text.trim())
            .map_err(|e| format!("{}: {} is not hex: {e}", v.id, flat_path.display()))?;
        let wrapped = cbor_wrap(&flat);
        // Only used to print the result under the program's declared version.
        let program = Program::from_flat(&flat).ok();
        (
            eval_script_bytes_gated(&wrapped, v.language, v.major_pv, models, budget),
            program,
        )
    } else {
        let path = v.file("uplc");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: cannot read {}: {e}", v.id, path.display()))?;
        match dugite_uplc::syn::parse_program(&text) {
            Ok(program) => (
                eval_program_gated(program.clone(), v.language, v.major_pv, models, budget),
                Some(program),
            ),
            Err(e) => {
                return Ok(Observed::failure(
                    PARSE_DECODE_ERROR,
                    format!("parse error at byte {}: {}", e.offset, e.message),
                ))
            }
        }
    };

    match outcome {
        Err(ProgramEvalError::CostModelUnavailable) => Err(format!(
            "{}: the corpus cost model for PV{} {} could not be applied",
            v.id, v.major_pv, v.plutus_version
        )),
        Err(ProgramEvalError::Decode(e)) => Ok(Observed::failure(classify_decode(&e), e)),
        Err(ProgramEvalError::Scope(e)) => Ok(Observed::failure(classify(&e), e)),
        Err(ProgramEvalError::Eval { error, .. }) => Ok(Observed::failure(classify(&error), error)),
        Ok(done) => {
            let program = input_program.ok_or_else(|| {
                format!(
                    "{}: evaluated, but the flat bytes do not decode for printing",
                    v.id
                )
            })?;
            let term = readback(done.value).map_err(|e| format!("{}: read-back: {e}", v.id))?;
            let evaluation =
                print_program(&program, &term).map_err(|e| format!("{}: print: {e}", v.id))?;
            Ok(Observed {
                evaluation,
                budget: Some((done.consumed.cpu, done.consumed.mem)),
                error: None,
            })
        }
    }
}

/// A run over every vector: the report entries and a tally for the log.
#[derive(Debug, Default)]
pub struct Run {
    pub cases: Vec<UplcCase>,
    /// Vectors that produced one of the non-scoring classes (panic, internal error, decoder not
    /// yet wired), with the evaluator's message.  A non-empty list fails the run.
    pub unscorable: Vec<String>,
    /// Vectors run from `.flat` through the production decoder.
    pub from_flat: usize,
    /// Vectors with no `.flat`, run from `.uplc` through the TEXT parser (`dugite_uplc::syn`),
    /// which is not the production decoder.
    pub from_text: usize,
}

impl Run {
    /// The run's failure, if any: an unscorable outcome is a dugite defect the report must not hide.
    pub fn fatal(&self) -> Option<String> {
        if self.unscorable.is_empty() {
            None
        } else {
            Some(format!(
                "{} vectors hit a non-scoring outcome (panic, internal error or decoder not yet wired): {}",
                self.unscorable.len(),
                self.unscorable.join("; ")
            ))
        }
    }

    /// One line for the status note: how many results of each kind, and by which input path.
    pub fn summary(&self) -> String {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &self.cases {
            let kind = if c.evaluation.starts_with('(') {
                "result"
            } else {
                c.evaluation.as_str()
            };
            *counts.entry(kind).or_default() += 1;
        }
        format!(
            "{} vectors through the production gated path; observed {counts:?}; {} run from .flat \
             (production decoder), {} run from .uplc (text parser, not the production decoder)",
            self.cases.len(),
            self.from_flat,
            self.from_text
        )
    }
}

/// Run the whole scenario on a thread with a large stack: the textual parser and some corpus
/// programs recurse deeply, and a stack overflow would abort the whole run.
pub fn run_all(corpora: &Path) -> Result<Run, String> {
    let vectors = load_vectors(corpora)?;
    let cost_models = load_cost_models(corpora)?;
    let worker = std::thread::Builder::new()
        .name("uplc-conformance".into())
        .stack_size(512 * 1024 * 1024)
        .spawn(move || -> Result<Run, String> {
            let mut run = Run::default();
            for v in &vectors {
                let observed = match catch_unwind(AssertUnwindSafe(|| run_vector(v, &cost_models)))
                {
                    Ok(result) => result?,
                    Err(payload) => {
                        let message = payload
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "non-string panic payload".into());
                        Observed::failure(EVALUATOR_PANIC, message)
                    }
                };
                if v.file("flat").is_file() {
                    run.from_flat += 1;
                } else {
                    run.from_text += 1;
                }
                if !is_scenario_class(&observed.evaluation) && !observed.evaluation.starts_with('(')
                {
                    run.unscorable.push(format!(
                        "{}: {} ({})",
                        v.id,
                        observed.evaluation,
                        observed.error.clone().unwrap_or_default()
                    ));
                }
                run.cases.push(UplcCase {
                    id: v.id.clone(),
                    title: v.title.clone(),
                    group: v.group.clone(),
                    plutus_version: v.plutus_version.clone(),
                    protocol_version: v.protocol_version.clone(),
                    evaluation: observed.evaluation,
                    budget: observed.budget.map(|(cpu, mem)| Budget { cpu, mem }),
                    error: observed.error,
                });
            }
            Ok(run)
        })
        .map_err(|e| format!("cannot start the evaluation thread: {e}"))?;
    worker
        .join()
        .map_err(|_| "the evaluation thread died outside a vector".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbor_wrap_matches_the_production_encoder_at_every_length_class() {
        for len in [0usize, 1, 23, 24, 255, 256, 65_535, 65_536] {
            let flat = vec![0xa5u8; len];
            let mut expected = Vec::new();
            minicbor::Encoder::new(&mut expected).bytes(&flat).unwrap();
            assert_eq!(cbor_wrap(&flat), expected, "length {len}");
        }
    }

    #[test]
    fn names_match_across_case_and_separators() {
        assert_eq!(
            normalise_name("illTypedArray-01"),
            normalise_name("illtypedarray 01")
        );
        assert_eq!(
            normalise_name("1-1-0-before-pv11"),
            normalise_name("1 1 0 before pv11")
        );
        assert_ne!(normalise_name("a-b"), normalise_name("ab"));
    }

    #[test]
    fn error_classes_are_the_scenarios_three() {
        assert_eq!(classify(&UplcError::ScriptError), EVAL_FAILURE);
        assert_eq!(
            classify(&UplcError::BudgetExhausted {
                cpu_remaining: -1,
                mem_remaining: 0
            }),
            OUT_OF_BUDGET
        );
        assert_eq!(
            classify(&UplcError::FlatDecode("x".into())),
            PARSE_DECODE_ERROR
        );
        assert_eq!(classify(&UplcError::FreeVariable(1)), EVAL_FAILURE);
    }

    #[test]
    fn internal_encode_and_not_wired_errors_never_become_a_scenario_class() {
        assert_eq!(
            classify(&UplcError::Internal("x".into())),
            EVALUATOR_INTERNAL
        );
        assert_eq!(classify(&UplcError::Encode("x".into())), EVALUATOR_INTERNAL);
        assert_eq!(
            classify_decode(&UplcError::Internal("x".into())),
            EVALUATOR_INTERNAL
        );
        assert_eq!(
            classify_decode(&UplcError::FlatDecode(
                "Array / Value flat encoding not yet wired".into()
            )),
            DECODER_NOT_WIRED
        );
        // An ordinary rejection of bad bytes is still a decode error.
        assert_eq!(
            classify_decode(&UplcError::FlatDecode("unknown term tag 0b1011".into())),
            PARSE_DECODE_ERROR
        );
        for class in [EVALUATOR_INTERNAL, DECODER_NOT_WIRED, EVALUATOR_PANIC] {
            assert!(!is_scenario_class(class));
        }
    }
}
