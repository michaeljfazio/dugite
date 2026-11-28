//! Running every fixture and writing the `ledger-rules` report.

use serde_json::Value;

use crate::node_diversity::pins::{corpora_dir, Pins};
use crate::node_diversity::report::{
    ledger_rules_report, LedgerRulesCase, ReportStatus, RunInfo, Scenario,
};

use super::fixture::{corpus_root, load_all, Fixture};
use super::names::Vocabulary;
use super::run::{run_fixture, Run};

/// The scenario's own vocabulary, from the pinned `schema.json`.
pub fn vocabulary() -> Result<Vocabulary, String> {
    Vocabulary::load(&Scenario::LedgerRules.schema_path(&corpora_dir()))
}

/// Load every fixture and run it.  A fixture the harness cannot even prepare (a parameter it
/// cannot map, a state section it cannot read) is an `Err`, never a skipped fixture.
pub fn run_all() -> Result<(Vec<Fixture>, Vec<Run>), String> {
    let fixtures = load_all(&corpus_root())?;
    if fixtures.len() != 368 {
        return Err(format!(
            "the scenario has {} fixtures, the harness was written for 368; review the pin",
            fixtures.len()
        ));
    }
    let vocab = vocabulary()?;
    let mut runs = Vec::with_capacity(fixtures.len());
    for f in &fixtures {
        runs.push(run_fixture(f, &vocab, None).map_err(|e| format!("fixture {}: {e}", f.id))?);
    }
    Ok((fixtures, runs))
}

/// The report: one result per fixture, from dugite's own validation path.
pub fn generate(pins: &Pins, run_info: &RunInfo) -> Result<(ReportStatus, String, Value), String> {
    let (_fixtures, runs) = run_all()?;
    let cases: Vec<LedgerRulesCase> = runs
        .iter()
        .map(|r| LedgerRulesCase {
            id: r.id.clone(),
            result: r.report_result(),
        })
        .collect();
    let matched = runs.iter().filter(|r| r.matched()).count();
    let report = ledger_rules_report(pins, run_info, &cases, None);
    Ok((
        ReportStatus::Ok,
        format!("{matched}/{} fixtures match", runs.len()),
        report,
    ))
}
