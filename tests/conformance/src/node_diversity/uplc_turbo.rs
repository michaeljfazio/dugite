//! The `uplc-turbo` scenario runner (#1114 Phase 2d).
//!
//! A non-functional benchmark: 3,997 mainnet programs (flat, stored as hex text) are each
//! evaluated 50 times through dugite's production, protocol-version- and Plutus-language-gated
//! evaluation path; the best 30 runs give `cpu_us` and `mem_bytes`; the budget the run consumed
//! gives `budget_cpu` / `budget_mem`.  The scenario's README is the specification; where it
//! leaves room this module says what it chose.
//!
//! * **One evaluator.**  Each program goes through [`prepare_script_bytes_gated`] (cost model,
//!   language gate, CBOR/flat decode, availability rules, scope check) once, untimed, and then
//!   [`PreparedScript::run`] 50 times, timed.  `eval_script_bytes_gated`, the entry point the
//!   `uplc-conformance` runner and the ledger-facing gate use, is exactly those two calls, so
//!   nothing here is a second evaluator.  The scenario excludes decoding and preparation from the
//!   timed region; the timed region is the CEK run under the on-chain cost model.
//! * **Protocol version** comes from the corpus epoch directory (`>= 644` 11, `>= 537` 10,
//!   `>= 507` 9, else 8); the cost model is the default of the builtin semantics variant that
//!   (language, protocol version) selects.  Protocol versions 10 and 11 use the models the
//!   `uplc-conformance` corpus ships; version 9 uses the version 10 arrays (the same variant);
//!   version 8 (variant A) uses `node-diversity/uplc-turbo-cost-models-pv8.json`.
//! * **Limits** are the mainnet limits of the program's epoch: 10B CPU, 14M memory up to
//!   protocol version 10 and 16.5M from 11.
//! * **Selection.**  The 30 fastest of the 50 runs are kept (ties by run order) and BOTH means are
//!   taken over those same runs.  Runs are sequential on one thread: no parallel noise.
//! * **`mem_bytes`** is the cumulative number of bytes the evaluation requested from the global
//!   allocator on the evaluating thread ([`CountingAllocator`]), a reallocation counting as a
//!   fresh allocation of its new size and frees not subtracted.  That is Go's `TotalAlloc` as
//!   used by Dingo, and the growth of Amaru's monotone arena: bytes the evaluation caused to be
//!   allocated, not its resident peak.  The allocator is installed by the test binary
//!   (`#[global_allocator]` in `tests/ndc_reports.rs`), never by a production crate; the runner
//!   proves it is installed before it measures anything.
//! * **A program that does not evaluate** (decode, availability or scope error, script failure,
//!   budget exhaustion, or a V3 script that does not return unit, which the ledger also rejects)
//!   is reported with `null` budgets and `null` `mem_bytes`, so it disagrees with every node that
//!   did evaluate it and scores as a failure; the schema does not allow a `null` `cpu_us`, so it
//!   is `0` there.  A panic or an internal error is a dugite defect, not an outcome of the
//!   program: it fails the run (an error report).
//! * **Only the publishing runner publishes.**  A run is `ok` only when it is complete and
//!   [`PUBLISHING_RUNNER_ENV`] is set.  Every other run (this laptop, a sample) is written as a
//!   `stub`, the status a publish step refuses, and its `.status.json` note says it is
//!   trend-only.  The score of a stub is still printed.  On the publishing runner a program that
//!   does not evaluate fails the run, as every corpus program ran on mainnet.  A corpus file whose
//!   outer CBOR wrapper does not decode always fails the run.
//!
//! [`prepare_script_bytes_gated`]: dugite_uplc::eval_redeemer::prepare_script_bytes_gated
//! [`PreparedScript::run`]: dugite_uplc::eval_redeemer::PreparedScript::run

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use dugite_uplc::cost_models::CostModels;
use dugite_uplc::eval_redeemer::{prepare_script_bytes_gated, ProgramEvalError};
use dugite_uplc::machine::{ExBudget, Value};
use dugite_uplc::redeemer_resolve::ScriptLanguage;
use dugite_uplc::term::Constant;
use dugite_uplc::UplcError;
use serde_json::Value as Json;

use super::pins::repo_root;
use super::report::{Machine, TurboBenchmark};
use super::uplc_conformance::load_cost_models;

/// Evaluations per program, and how many of the fastest are kept.
pub const RUNS: usize = 50;
pub const KEEP: usize = 30;

/// Mainnet limits of the program's epoch.
pub const MAX_CPU: i64 = 10_000_000_000;
pub const MAX_MEM_BEFORE_PV11: i64 = 14_000_000;
pub const MAX_MEM_FROM_PV11: i64 = 16_500_000;

/// SHA-256 of `node-diversity/uplc-turbo-cost-models-pv8.json`.  The runner refuses any other file,
/// so a changed number cannot be measured by accident.
pub const PV8_COST_MODELS_SHA256: &str =
    "8265cc94be3313026251fef2844b52dfca855d521a439cddf9189bd75f243a5a";
/// Entries of each protocol version 8 array.
pub const PV8_ARRAY_LEN: usize = 332;

/// `DUGITE_NDC_TURBO_SAMPLE=N`: run every Nth program only.  The result is a partial report that is
/// never publishable.
pub const SAMPLE_ENV: &str = "DUGITE_NDC_TURBO_SAMPLE";
/// `DUGITE_NDC_PUBLISHING_RUNNER=<runner type>`: set by the one fixed runner type whose reports are
/// published.  Unset, a run is trend-only.
pub const PUBLISHING_RUNNER_ENV: &str = "DUGITE_NDC_PUBLISHING_RUNNER";

// ── the counting allocator ──────────────────────────────────────────────────────────────────

thread_local! {
    // `const` initialisation and no destructor: reading it from inside the allocator can neither
    // allocate nor panic during thread teardown.
    static ALLOCATED: Cell<u64> = const { Cell::new(0) };
}

fn record_allocation(bytes: usize) {
    let _ = ALLOCATED.try_with(|c| c.set(c.get().wrapping_add(bytes as u64)));
}

/// Bytes the current thread has requested from the allocator so far (0 when the allocator is not
/// installed).
pub fn allocated_bytes() -> u64 {
    ALLOCATED.try_with(Cell::get).unwrap_or(0)
}

/// The system allocator that counts the bytes it is asked for, per thread.  Test binaries install
/// it with `#[global_allocator]`; no production crate does.
pub struct CountingAllocator;

// SAFETY: every method forwards to `System` unchanged and only adds to a thread-local counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation(new_size);
        System.realloc(ptr, layout, new_size)
    }
}

/// Whether [`CountingAllocator`] is the global allocator of this binary: an allocation of a known
/// size must move the counter by at least that much.
pub fn allocator_is_counting() -> bool {
    let before = allocated_bytes();
    let probe: Vec<u8> = Vec::with_capacity(4096);
    std::hint::black_box(&probe);
    let after = allocated_bytes();
    drop(probe);
    after.saturating_sub(before) >= 4096
}

// ── the corpus ──────────────────────────────────────────────────────────────────────────────

/// One program of the corpus.
#[derive(Debug, Clone)]
pub struct Program {
    /// The file name: the benchmark id the schema enumerates.
    pub bench: String,
    pub epoch: u32,
    pub path: PathBuf,
    pub language: ScriptLanguage,
    pub plutus_version: &'static str,
    pub major_pv: u32,
}

/// The protocol version in force at a mainnet epoch (the scenario's table).
pub fn protocol_major_for_epoch(epoch: u32) -> u32 {
    match epoch {
        644.. => 11,
        537..=643 => 10,
        507..=536 => 9,
        _ => 8,
    }
}

/// The Plutus language a benchmark id names: its `-v1.flat` / `-v2.flat` / `-v3.flat` suffix.
pub fn language_of(bench: &str) -> Result<(ScriptLanguage, &'static str), String> {
    for (suffix, language, name) in [
        ("-v1.flat", ScriptLanguage::PlutusV1, "V1"),
        ("-v2.flat", ScriptLanguage::PlutusV2, "V2"),
        ("-v3.flat", ScriptLanguage::PlutusV3, "V3"),
    ] {
        if bench.ends_with(suffix) {
            return Ok((language, name));
        }
    }
    Err(format!("{bench}: no -v1/-v2/-v3 Plutus version suffix"))
}

/// Every program under `<corpora>/uplc-turbo/<epoch>/*.flat`, ordered by epoch then name.
pub fn load_programs(corpora: &Path) -> Result<Vec<Program>, String> {
    let root = corpora.join("uplc-turbo");
    let mut out = Vec::new();
    let epochs = std::fs::read_dir(&root)
        .map_err(|e| format!("cannot read {}: {e} (run just ndc-fetch)", root.display()))?;
    for entry in epochs {
        let dir = entry.map_err(|e| e.to_string())?.path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let epoch: u32 = name
            .parse()
            .map_err(|_| format!("{} is not an epoch directory", dir.display()))?;
        let files = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for file in files {
            let path = file.map_err(|e| e.to_string())?.path();
            let bench = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{} has no file name", path.display()))?
                .to_string();
            let (language, plutus_version) = language_of(&bench)?;
            out.push(Program {
                bench,
                epoch,
                path,
                language,
                plutus_version,
                major_pv: protocol_major_for_epoch(epoch),
            });
        }
    }
    out.sort_by(|a, b| (a.epoch, &a.bench).cmp(&(b.epoch, &b.bench)));
    Ok(out)
}

/// The benchmark ids the scenario's own schema enumerates.
pub fn schema_benchmarks(corpora: &Path) -> Result<Vec<String>, String> {
    let path = corpora.join("ndc/scenarios/uplc-turbo/schema.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e} (run just ndc-fetch)", path.display()))?;
    let schema: Json =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let ids = schema
        .pointer("/$defs/benchmark/properties/bench/enum")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("{} has no bench enum", path.display()))?;
    ids.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| "a bench id is not a string".to_string())
        })
        .collect()
}

/// The cost models by `(protocol major, Plutus version)` for every protocol version the corpus uses.
pub fn turbo_cost_models(corpora: &Path) -> Result<BTreeMap<(u32, String), CostModels>, String> {
    let mut models = load_cost_models(corpora)?; // protocol versions 10 and 11
                                                 // Protocol version 9 selects the same builtin semantics variant as 10 (B for V1/V2, C for V3).
    for version in ["V1", "V2", "V3"] {
        let ten = models
            .get(&(10, version.to_string()))
            .cloned()
            .ok_or_else(|| format!("no protocol version 10 cost model for {version}"))?;
        models.insert((9, version.to_string()), ten);
    }
    // Protocol version 8 (variant A) has no model in the corpus: it is the data file next to the pins.
    let path = repo_root().join("tests/conformance/node-diversity/uplc-turbo-cost-models-pv8.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let actual = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(text.as_bytes()))
    };
    if actual != PV8_COST_MODELS_SHA256 {
        return Err(format!(
            "{} has SHA-256 {actual}, the runner pins {PV8_COST_MODELS_SHA256}",
            path.display()
        ));
    }
    let file: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for (key, version) in [("plutus_v1", "V1"), ("plutus_v2", "V2")] {
        let params: Vec<i64> = serde_json::from_value(
            file.get(key)
                .cloned()
                .ok_or_else(|| format!("{} has no {key}", path.display()))?,
        )
        .map_err(|e| format!("{}: {key}: {e}", path.display()))?;
        if params.len() != PV8_ARRAY_LEN {
            return Err(format!(
                "{}: {key} has {} entries, expected {PV8_ARRAY_LEN}",
                path.display(),
                params.len()
            ));
        }
        let mut only = CostModels::default();
        if version == "V1" {
            only.plutus_v1 = Some(params);
        } else {
            only.plutus_v2 = Some(params);
        }
        models.insert((8, version.to_string()), only);
    }
    Ok(models)
}

// ── measuring ───────────────────────────────────────────────────────────────────────────────

/// What became of a program.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Evaluated: the mean of the best runs, and the budget one run consumed.
    Measured {
        cpu_us: f64,
        mem_bytes: f64,
        budget_cpu: u64,
        budget_mem: u64,
    },
    /// The program is not a successful evaluation; the class is for the summary.
    Failed { class: &'static str, detail: String },
}

/// One timed run: nanoseconds and bytes allocated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub nanos: u64,
    pub bytes: u64,
}

/// Mean time (µs) and mean bytes of the `keep` fastest samples; ties keep run order.
pub fn best_mean(samples: &[Sample], keep: usize) -> (f64, f64) {
    let mut ordered: Vec<(usize, Sample)> = samples.iter().copied().enumerate().collect();
    ordered.sort_by_key(|(i, s)| (s.nanos, *i));
    let kept = &ordered[..keep.min(ordered.len())];
    let n = kept.len().max(1) as f64;
    let nanos: f64 = kept.iter().map(|(_, s)| s.nanos as f64).sum();
    let bytes: f64 = kept.iter().map(|(_, s)| s.bytes as f64).sum();
    (nanos / n / 1000.0, bytes / n)
}

/// How a failed evaluation is classed in the summary.
fn failure_class(error: &UplcError) -> Option<&'static str> {
    match error {
        UplcError::BudgetExhausted { .. } => Some("out of budget"),
        // dugite defects, never an outcome of the program.
        UplcError::Internal(_) | UplcError::Encode(_) => None,
        UplcError::FlatDecode(m) | UplcError::CborDecode(m) if m.contains("not yet wired") => None,
        UplcError::FlatDecode(_) | UplcError::CborDecode(_) => Some("decode error"),
        _ => Some("evaluation failure"),
    }
}

/// Whether `bytes` is exactly one definite-length CBOR byte string, the on-chain script form.  A
/// corpus file that is not one is a broken corpus, never a program that fails to evaluate.
pub fn check_cbor_wrapper(bytes: &[u8]) -> Result<(), String> {
    let first = *bytes.first().ok_or("empty file")?;
    if first >> 5 != 2 {
        return Err(format!("first byte {first:#04x} is not a byte string"));
    }
    let (header, len) = match first & 0x1f {
        n @ 0..=23 => (1usize, u64::from(n)),
        24 => (2, u64::from(*bytes.get(1).ok_or("truncated length")?)),
        25 => (
            3,
            u64::from(u16::from_be_bytes(
                bytes
                    .get(1..3)
                    .ok_or("truncated length")?
                    .try_into()
                    .unwrap(),
            )),
        ),
        26 => (
            5,
            u64::from(u32::from_be_bytes(
                bytes
                    .get(1..5)
                    .ok_or("truncated length")?
                    .try_into()
                    .unwrap(),
            )),
        ),
        27 => (
            9,
            u64::from_be_bytes(
                bytes
                    .get(1..9)
                    .ok_or("truncated length")?
                    .try_into()
                    .unwrap(),
            ),
        ),
        _ => return Err("indefinite or reserved length".into()),
    };
    if (bytes.len() - header) as u64 != len {
        return Err(format!(
            "the length says {len} bytes, the file has {}",
            bytes.len().saturating_sub(header)
        ));
    }
    Ok(())
}

/// Measure one program.  `Err` is a harness failure or a dugite defect (an unreadable file, an
/// unusable cost model, a panic, an internal error); a program that does not evaluate is an
/// `Ok(Outcome::Failed)`.
pub fn measure(
    program: &Program,
    models: &BTreeMap<(u32, String), CostModels>,
) -> Result<Outcome, String> {
    measure_with_runs(program, models, RUNS)
}

/// [`measure`] with another number of runs.  The budget check of every program needs one run.
pub fn measure_with_runs(
    program: &Program,
    models: &BTreeMap<(u32, String), CostModels>,
    runs: usize,
) -> Result<Outcome, String> {
    let model = models
        .get(&(program.major_pv, program.plutus_version.to_string()))
        .ok_or_else(|| {
            format!(
                "{}: no cost model for PV{} {}",
                program.bench, program.major_pv, program.plutus_version
            )
        })?;
    // Unlike the uplc-conformance corpus, each file is already the on-chain script form: one CBOR
    // byte string around the flat bytes, as raw bytes (not hex text).
    let wrapped = std::fs::read(&program.path).map_err(|e| {
        format!(
            "{}: cannot read {}: {e}",
            program.bench,
            program.path.display()
        )
    })?;
    let budget = ExBudget {
        cpu: MAX_CPU,
        mem: if program.major_pv >= 11 {
            MAX_MEM_FROM_PV11
        } else {
            MAX_MEM_BEFORE_PV11
        },
    };

    check_cbor_wrapper(&wrapped).map_err(|e| {
        format!(
            "{}: the outer CBOR wrapper does not decode: {e}",
            program.bench
        )
    })?;

    // Preparation: untimed, once.
    let prepared = match catch_unwind(AssertUnwindSafe(|| {
        prepare_script_bytes_gated(&wrapped, program.language, program.major_pv, model)
    })) {
        Err(_) => {
            return Err(format!(
                "{}: the evaluator panicked while preparing",
                program.bench
            ))
        }
        Ok(Ok(prepared)) => prepared,
        Ok(Err(ProgramEvalError::CostModelUnavailable)) => {
            return Err(format!(
                "{}: the cost model for PV{} {} could not be applied",
                program.bench, program.major_pv, program.plutus_version
            ))
        }
        // A mainnet script always has its CBOR wrapper: a file without one is a broken corpus or a
        // broken decoder, never a result.
        Ok(Err(ProgramEvalError::Decode(UplcError::CborDecode(e)))) => {
            return Err(format!(
                "{}: the outer CBOR wrapper does not decode: {e}",
                program.bench
            ))
        }
        Ok(Err(ProgramEvalError::Decode(e) | ProgramEvalError::Scope(e))) => {
            return match failure_class(&e) {
                Some(class) => Ok(Outcome::Failed {
                    class,
                    detail: e.to_string(),
                }),
                None => Err(format!(
                    "{}: dugite defect while preparing: {e}",
                    program.bench
                )),
            }
        }
        Ok(Err(ProgramEvalError::Eval { error, .. })) => {
            return Err(format!(
                "{}: preparation ran the machine: {error}",
                program.bench
            ))
        }
    };

    let mut samples = Vec::with_capacity(runs);
    let mut consumed = None;
    for _ in 0..runs {
        // A fresh copy per run, taken outside the timer and the counter: `run` consumes it.
        let fresh = prepared.clone();
        let bytes_before = allocated_bytes();
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| fresh.run(budget)));
        let elapsed = started.elapsed();
        let bytes_after = allocated_bytes();
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(_) => return Err(format!("{}: the evaluator panicked", program.bench)),
        };
        match outcome {
            Ok(done) => {
                let returned_unit = matches!(done.value, Value::Const(Constant::Unit));
                if program.language == ScriptLanguage::PlutusV3 && !returned_unit {
                    return Ok(Outcome::Failed {
                        class: "evaluation failure",
                        detail: "a PlutusV3 script must return unit".into(),
                    });
                }
                match consumed {
                    None => consumed = Some((done.consumed.cpu, done.consumed.mem)),
                    Some(first) if first != (done.consumed.cpu, done.consumed.mem) => {
                        return Err(format!(
                            "{}: the consumed budget differs between runs ({first:?} then {:?})",
                            program.bench,
                            (done.consumed.cpu, done.consumed.mem)
                        ))
                    }
                    Some(_) => {}
                }
                samples.push(Sample {
                    nanos: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
                    bytes: bytes_after.saturating_sub(bytes_before),
                });
            }
            Err(ProgramEvalError::Eval { error, .. }) => {
                return match failure_class(&error) {
                    Some(class) => Ok(Outcome::Failed {
                        class,
                        detail: error.to_string(),
                    }),
                    None => Err(format!("{}: dugite defect: {error}", program.bench)),
                }
            }
            Err(other) => {
                return Err(format!(
                    "{}: unexpected {other:?} from a run",
                    program.bench
                ))
            }
        }
    }
    let (cpu_us, mem_bytes) = best_mean(&samples, KEEP);
    let (cpu, mem) = consumed.ok_or_else(|| format!("{}: no run completed", program.bench))?;
    Ok(Outcome::Measured {
        cpu_us,
        mem_bytes,
        budget_cpu: u64::try_from(cpu)
            .map_err(|_| format!("{}: negative CPU budget", program.bench))?,
        budget_mem: u64::try_from(mem)
            .map_err(|_| format!("{}: negative memory budget", program.bench))?,
    })
}

/// The report entry of a program.
pub fn benchmark_entry(program: &Program, outcome: &Outcome) -> TurboBenchmark {
    let (budget_cpu, budget_mem, cpu_us, mem_bytes) = match outcome {
        Outcome::Measured {
            cpu_us,
            mem_bytes,
            budget_cpu,
            budget_mem,
        } => (
            Some(*budget_cpu),
            Some(*budget_mem),
            *cpu_us,
            Some(*mem_bytes),
        ),
        Outcome::Failed { .. } => (None, None, 0.0, None),
    };
    TurboBenchmark {
        bench: program.bench.clone(),
        plutus_version: program.plutus_version.to_string(),
        protocol_version: format!("{}.0", program.major_pv),
        budget_cpu,
        budget_mem,
        cpu_us,
        mem_bytes,
    }
}

// ── the machine ─────────────────────────────────────────────────────────────────────────────

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The machine the run was made on (the schema's `machine`; nothing here is scored).
pub fn detect_machine() -> Machine {
    let os = command_output("uname", &["-s"]).unwrap_or_else(|| std::env::consts::OS.to_string());
    let (os_version, cpu, memory_bytes) = if os == "Darwin" {
        (
            command_output("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}")),
            command_output("sysctl", &["-n", "machdep.cpu.brand_string"]),
            command_output("sysctl", &["-n", "hw.memsize"]).and_then(|v| v.parse().ok()),
        )
    } else {
        let release = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            });
        let cpu = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        });
        let memory = std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                .map(|kib| kib * 1024)
        });
        (release, cpu, memory)
    };
    Machine {
        os,
        os_version,
        architecture: std::env::consts::ARCH.to_string(),
        cpu,
        logical_cores: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        memory_bytes,
    }
}

/// The one-minute load average, to show how busy the machine was.
fn load_average() -> Option<String> {
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        return text.split_whitespace().next().map(str::to_string);
    }
    // macOS prints `{ 1.23 2.34 3.45 }`.
    command_output("sysctl", &["-n", "vm.loadavg"]).and_then(|t| {
        t.trim_matches(|c| c == '{' || c == '}' || c == ' ')
            .split_whitespace()
            .next()
            .map(str::to_string)
    })
}

// ── a run ───────────────────────────────────────────────────────────────────────────────────

/// The result of running the corpus.
#[derive(Debug)]
pub struct TurboRun {
    pub machine: Machine,
    pub benchmarks: Vec<TurboBenchmark>,
    /// Failed programs by class.
    pub failures: BTreeMap<&'static str, usize>,
    pub programs_total: usize,
    pub sampled: bool,
    /// Whether the run was made on the publishing runner ([`PUBLISHING_RUNNER_ENV`] set).
    pub publishing: bool,
    pub wall: Duration,
    /// Everything a reader of the sidecar needs to interpret the timings.
    pub provenance: String,
}

impl TurboRun {
    /// Only a full run on the publishing runner may be published; the shared writer records any
    /// other run as a `stub`.
    pub fn publishable(&self) -> bool {
        self.publishing && !self.sampled
    }

    /// The sidecar note.
    pub fn summary(&self) -> String {
        let failed: usize = self.failures.values().sum();
        let classes = if self.failures.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = self
                .failures
                .iter()
                .map(|(c, n)| format!("{c}: {n}"))
                .collect();
            format!(" ({})", list.join(", "))
        };
        format!(
            "{} programs evaluated of {} in corpus, {} did not evaluate{}; {RUNS} runs each, best {KEEP}; wall {:.0}s. {}",
            self.benchmarks.len(),
            self.programs_total,
            failed,
            classes,
            self.wall.as_secs_f64(),
            self.provenance
        )
    }
}

fn sample_stride() -> Result<Option<usize>, String> {
    match std::env::var(SAMPLE_ENV) {
        Err(_) => Ok(None),
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n >= 2 => Ok(Some(n)),
            _ => Err(format!(
                "{SAMPLE_ENV}={v:?}: expected an integer of at least 2"
            )),
        },
    }
}

/// Run the corpus.  Fails (an error report) on a harness failure: a corpus that is not the
/// schema's set, a binary without optimisations or without the counting allocator, a dugite defect.
pub fn run_all(corpora: &Path) -> Result<TurboRun, String> {
    if cfg!(debug_assertions) {
        return Err(
            "uplc-turbo timings need an optimised build: run `just ndc-report uplc-turbo` (release profile)"
                .into(),
        );
    }
    if !allocator_is_counting() {
        return Err(
            "the counting allocator is not installed in this test binary, so mem_bytes would be 0"
                .into(),
        );
    }
    let stride = sample_stride()?;
    let all = load_programs(corpora)?;
    let mut expected = schema_benchmarks(corpora)?;
    expected.sort();
    let mut have: Vec<String> = all.iter().map(|p| p.bench.clone()).collect();
    have.sort();
    if have != expected {
        return Err(format!(
            "the fetched corpus ({} programs) is not the schema's benchmark set ({})",
            have.len(),
            expected.len()
        ));
    }
    let programs: Vec<&Program> = match stride {
        Some(n) => all.iter().step_by(n).collect(),
        None => all.iter().collect(),
    };
    let models = turbo_cost_models(corpora)?;
    let machine = detect_machine();
    let load_before = load_average();
    let started = Instant::now();
    let mut benchmarks = Vec::with_capacity(programs.len());
    let mut failures: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut per_pv: BTreeMap<u32, usize> = BTreeMap::new();
    for (i, program) in programs.iter().enumerate() {
        if i % 250 == 0 {
            eprintln!(
                "uplc-turbo: {i}/{} ({:.0}s)",
                programs.len(),
                started.elapsed().as_secs_f64()
            );
        }
        let outcome = measure(program, &models)?;
        if let Outcome::Failed { class, detail } = &outcome {
            eprintln!(
                "uplc-turbo: {} did not evaluate ({class}): {detail}",
                program.bench
            );
            *failures.entry(class).or_default() += 1;
        }
        *per_pv.entry(program.major_pv).or_default() += 1;
        benchmarks.push(benchmark_entry(program, &outcome));
    }
    let wall = started.elapsed();
    // Every corpus program ran on mainnet.  On the publishing runner a program that does not
    // evaluate is a dugite defect, so the run fails; elsewhere it is reported and counted.
    let publishing = std::env::var(PUBLISHING_RUNNER_ENV)
        .ok()
        .filter(|v| !v.is_empty());
    let failed: usize = failures.values().sum();
    if publishing.is_some() && stride.is_none() && failed > 0 {
        return Err(format!(
            "{failed} mainnet programs did not evaluate on the publishing runner: {failures:?}"
        ));
    }
    let load_after = load_average();
    let rustc = command_output("rustc", &["--version"]).unwrap_or_else(|| "rustc unknown".into());
    let status = match (&publishing, stride) {
        (_, Some(n)) => format!("SAMPLE (every {n}th program), NOT PUBLISHABLE"),
        (Some(runner), None) => format!("publishing runner: {runner}"),
        (None, None) => format!(
            "TREND-ONLY, written as a stub so it cannot be published: not produced on the fixed publishing runner ({PUBLISHING_RUNNER_ENV} unset); \
             timings depend on this machine and its load"
        ),
    };
    let pvs: Vec<String> = per_pv
        .iter()
        .map(|(pv, n)| format!("PV{pv}: {n}"))
        .collect();
    let provenance = format!(
        "{status}. Build: release profile, {rustc}. Machine: {} {} {}, {} logical cores. \
         Load average (1 min) before/after: {}/{}. Programs by protocol version: {}.",
        machine.os,
        machine.architecture,
        machine.cpu.as_deref().unwrap_or("unknown cpu"),
        machine.logical_cores,
        load_before.as_deref().unwrap_or("?"),
        load_after.as_deref().unwrap_or("?"),
        pvs.join(", "),
    );
    Ok(TurboRun {
        machine,
        benchmarks,
        failures,
        programs_total: all.len(),
        sampled: stride.is_some(),
        publishing: publishing.is_some(),
        wall,
        provenance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(nanos: u64, bytes: u64) -> Sample {
        Sample { nanos, bytes }
    }

    #[test]
    fn epochs_map_to_the_scenarios_protocol_versions() {
        assert_eq!(protocol_major_for_epoch(290), 8);
        assert_eq!(protocol_major_for_epoch(506), 8);
        assert_eq!(protocol_major_for_epoch(507), 9);
        assert_eq!(protocol_major_for_epoch(536), 9);
        assert_eq!(protocol_major_for_epoch(537), 10);
        assert_eq!(protocol_major_for_epoch(643), 10);
        assert_eq!(protocol_major_for_epoch(644), 11);
    }

    #[test]
    fn best_mean_keeps_the_fastest_runs_and_their_bytes() {
        // 50 runs: 30 fast ones of 100ns / 10 bytes, 20 slow ones of 1000ns / 1000 bytes.
        let mut samples: Vec<Sample> = (0..20).map(|_| s(1000, 1000)).collect();
        samples.extend((0..30).map(|_| s(100, 10)));
        let (us, bytes) = best_mean(&samples, KEEP);
        assert!((us - 0.1).abs() < 1e-9, "{us}");
        assert!((bytes - 10.0).abs() < 1e-9, "{bytes}");
    }

    #[test]
    fn best_mean_takes_the_bytes_of_the_selected_runs() {
        // The fastest two runs allocated 5 and 7; the slow one allocated 1000.
        let (us, bytes) = best_mean(&[s(300, 1000), s(100, 5), s(200, 7)], 2);
        assert!((us - 0.15).abs() < 1e-9);
        assert!((bytes - 6.0).abs() < 1e-9);
    }

    #[test]
    fn language_comes_from_the_suffix() {
        assert_eq!(language_of("AB-0-CD-v3.flat").unwrap().1, "V3");
        assert!(language_of("AB-0-CD.flat").is_err());
    }
}
