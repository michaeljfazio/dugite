//! Evaluate a flat-encoded, fully-applied UPLC program (as produced by
//! `DUGITE_DUMP_APPLIED_DIR`, see `eval_redeemer.rs`) in Haskell "counting"
//! mode with the DEFAULT cost model, printing the result shape and consumed
//! budget. Used to cross-check dugite's CEK against external reference
//! evaluators (Haskell `uplc evaluate --counting`, `aiken uplc eval`) on the
//! same term when root-causing a budget divergence offline.
//!
//! Usage: `cargo run -p dugite-uplc --example eval_flat -- <file.flat> [A|B|C|D|E]`
//!
//! The optional second argument selects the `BuiltinSemanticsVariant`
//! (default: latest/strict). For a PlutusV2 script at protocol version 8 use
//! `A` (see `SemanticsVariant::for_script`).

use dugite_uplc::builtin::semantics::SemanticsVariant;
use dugite_uplc::machine::cost::BudgetTracker;
use dugite_uplc::machine::step::evaluate_with_budget;
use dugite_uplc::Program;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: eval_flat <file.flat> [A|B|C|D|E]");
    let variant = match std::env::args().nth(2).as_deref() {
        Some("A") => SemanticsVariant::A,
        Some("B") => SemanticsVariant::B,
        Some("C") => SemanticsVariant::C,
        Some("D") => SemanticsVariant::D,
        Some("E") | None => SemanticsVariant::LATEST,
        Some(other) => panic!("unknown semantics variant {other:?} (want A|B|C|D|E)"),
    };

    let bytes = std::fs::read(&path).expect("read flat file");
    let prog = Program::from_flat(&bytes).expect("decode flat program");
    println!(
        "program version {}.{}.{} variant {variant:?}",
        prog.version.0, prog.version.1, prog.version.2
    );

    let mut tracker = BudgetTracker::new_counting();
    let mut logs: Vec<String> = Vec::new();
    match evaluate_with_budget(prog.term, &mut tracker, Some(&mut logs), variant) {
        Ok(_) => println!("result: OK"),
        Err(e) => println!("result: ERROR {e}"),
    }
    let spent = tracker.consumed();
    println!("consumed cpu={} mem={}", spent.cpu, spent.mem);
    for l in &logs {
        println!("trace: {l}");
    }
}
