//! The expected-results contract and the raised-set snapshot.
//!
//! `expected-results.toml` is a committed list of `(fixture id, observed result)` pairs: the
//! fixtures dugite is KNOWN not to match, each with the result it produces today and the number of
//! an open GitHub issue holding the evidence.  It is NOT a skip list.  The gate fails when
//!
//! * an entry names an id that is not a fixture;
//! * a fixture does not match and has no entry (a new divergence);
//! * an entry's fixture now matches (the entry must be removed, so a fix cannot go unnoticed);
//! * an entry's observed result is not what dugite now produces (the divergence changed).
//!
//! `raised-snapshot.json` is the full set of failures dugite raises per fixture (tag path, tags,
//! scenario names).  A fixture that matches only because the expected failure is among several
//! shows up as a diff when the set grows, which is how over-rejection is seen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::fixture::Fixture;
use super::run::Run;

/// One known divergence.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The fixture id (five digits).
    pub id: String,
    /// What the fixture expects (its `gate_name`), repeated so a reviewer can read the pair.
    pub expected: String,
    /// What dugite produces today.
    pub observed: String,
    /// The divergence class, equal to the title of the issue.
    pub class: String,
    /// The open GitHub issue holding the evidence.
    pub issue: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    entry: Vec<Entry>,
}

/// `tests/conformance/node-diversity/ledger-rules/`.
pub fn data_dir() -> PathBuf {
    crate::node_diversity::pins::repo_root().join("tests/conformance/node-diversity/ledger-rules")
}

pub fn expected_results_path() -> PathBuf {
    data_dir().join("expected-results.toml")
}

pub fn snapshot_path() -> PathBuf {
    data_dir().join("raised-snapshot.json")
}

pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let file: File = toml::from_str(text).map_err(|e| format!("expected-results.toml: {e}"))?;
    Ok(file.entry)
}

pub fn load(path: &Path) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text)
}

/// The verdict of the gate.
#[derive(Debug, Default)]
pub struct Gate {
    pub problems: Vec<String>,
    /// Fixtures that match.
    pub matched: usize,
    /// Fixtures that do not match and are tracked by an entry.
    pub tracked: usize,
    pub total: usize,
}

impl Gate {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Check `runs` (one per fixture) against `entries`.
pub fn check(fixtures: &[Fixture], runs: &[Run], entries: &[Entry]) -> Gate {
    let mut gate = Gate {
        total: runs.len(),
        ..Gate::default()
    };
    let ids: BTreeSet<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();
    let mut by_id: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in entries {
        if !ids.contains(e.id.as_str()) {
            gate.problems
                .push(format!("entry {}: not a fixture id", e.id));
        }
        if by_id.insert(e.id.as_str(), e).is_some() {
            gate.problems.push(format!("entry {}: listed twice", e.id));
        }
        if e.issue == 0 {
            gate.problems
                .push(format!("entry {}: no issue number", e.id));
        }
        if e.class.trim().is_empty() {
            gate.problems.push(format!("entry {}: no class", e.id));
        }
    }
    for run in runs {
        let entry = by_id.get(run.id.as_str());
        let observed = run.observed();
        let want = run.expected.gate_name();
        if run.matched() {
            gate.matched += 1;
            if entry.is_some() {
                gate.problems.push(format!(
                    "{}: now matches its expectation ({want}); remove its entry from expected-results.toml",
                    run.id
                ));
            }
            continue;
        }
        match entry {
            None => gate.problems.push(format!(
                "{}: expected {want}, dugite gives {observed}; a new divergence has no entry \
                 (file an issue with evidence, then add one)",
                run.id
            )),
            Some(e) => {
                gate.tracked += 1;
                if e.expected != want {
                    gate.problems.push(format!(
                        "{}: the entry says the fixture expects {}, but it expects {want}",
                        run.id, e.expected
                    ));
                }
                if e.observed != observed {
                    gate.problems.push(format!(
                        "{}: the entry records {}, dugite now gives {observed} (expected {want})",
                        run.id, e.observed
                    ));
                }
            }
        }
    }
    gate
}

/// The snapshot: id -> every raised failure, one line each.
pub fn snapshot(runs: &[Run]) -> BTreeMap<String, Vec<String>> {
    runs.iter()
        .map(|r| (r.id.clone(), r.raised_lines()))
        .collect()
}

pub fn snapshot_text(runs: &[Run]) -> String {
    let mut text = serde_json::to_string_pretty(&snapshot(runs)).expect("snapshot serialises");
    text.push('\n');
    text
}

/// The differences between a committed snapshot and the current one, one line per fixture.
pub fn snapshot_diff(committed: &str, current: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let old: BTreeMap<String, Vec<String>> = match serde_json::from_str(committed) {
        Ok(v) => v,
        Err(e) => return vec![format!("raised-snapshot.json does not parse: {e}")],
    };
    let mut out = Vec::new();
    for (id, now) in current {
        match old.get(id) {
            None => out.push(format!("{id}: not in the snapshot")),
            Some(then) if then != now => {
                out.push(format!("{id}: was {then:?}, now {now:?}"));
            }
            Some(_) => {}
        }
    }
    for id in old.keys() {
        if !current.contains_key(id) {
            out.push(format!("{id}: in the snapshot, not a fixture"));
        }
    }
    out
}
