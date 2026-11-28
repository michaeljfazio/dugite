//! The expected-results contract for the replayed rules (LEDGER, NEWEPOCH).
//!
//! `tests/conformance/expected/ledger_rules_replay.tsv` holds one
//! `(vector id, observed result, issue)` triple per vector whose result is not a
//! plain match with Haskell:
//!
//! * a **divergence** from Haskell, which names the OPEN GitHub issue that tracks
//!   it (`#NNNN`); and
//! * an **agreed rejection** (`AGREED_REJECT ...`): Haskell rejected the signal
//!   and dugite's validator rejects it too. The dump does not carry Haskell's
//!   predicate, so the entry pins dugite's reason; its issue column is `-`.
//!
//! The gate fails on:
//!
//! * an **unknown id** — a divergence with no entry (a new regression, or a new
//!   vector);
//! * an entry that now **matches** — it must be removed, so the file only ever
//!   shrinks as divergences are fixed;
//! * an observed result that **differs** from the recorded one — the divergence
//!   changed, which a reviewer must look at (every observed result ends in a
//!   digest of the evidence, so a changed validator verdict or a changed value
//!   changes it);
//! * a **stale id** — an entry whose vector is not in the corpus;
//! * an entry **without an issue** (or an `AGREED_REJECT` with one).
//!
//! This is NOT the `conformance_skip.txt` contract, which only notices a path
//! that stopped existing. Nothing here skips a vector: every entry is a vector
//! that is replayed and whose result is pinned.
//!
//! The file is bound to the corpus: its `# corpus:` header must equal
//! `manifest.toml`'s release tag, so bumping the pin forces the file to be
//! regenerated and its diff reviewed.
//!
//! ## Regenerating
//!
//! Only the ignored `ledger_rules_replay_report` test rewrites the file, and only
//! with `DUGITE_UPDATE_EXPECTED_RESULTS=1`; it refuses to run when `CI` or
//! `DUGITE_REQUIRE_UPSTREAM=1` is set and prints an added / changed / removed
//! summary. The issue column is assigned by [`issue_for`]; a new class without a
//! rule there fails the regeneration instead of getting a placeholder.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::upstream::fixtures::workspace_root;
use crate::upstream::ledger_rules_replay::replay::Verdict;

/// Environment variable that makes the report test rewrite the file from what it
/// observed instead of failing. Never set in CI.
pub const UPDATE_ENV: &str = "DUGITE_UPDATE_EXPECTED_RESULTS";

/// Path of the committed file.
pub fn expected_path() -> PathBuf {
    workspace_root().join("tests/conformance/expected/ledger_rules_replay.tsv")
}

/// The corpus release tag `manifest.toml` pins.
pub fn manifest_tag() -> Result<String, String> {
    let p = workspace_root().join("tests/conformance/upstream/manifest.toml");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "tag").then(|| v.trim().trim_matches('"').to_string())
        })
        .ok_or_else(|| format!("no `tag` in {}", p.display()))
}

/// One recorded entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub observed: String,
    /// `#NNNN` or `-` (agreed rejection).
    pub issue: String,
}

/// The parsed contract.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub corpus_tag: String,
    pub entries: BTreeMap<String, Entry>,
}

/// `#NNNN`, or several joined by commas.
fn is_issue_ref(s: &str) -> bool {
    s.split(',').all(|one| {
        one.strip_prefix('#')
            .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
    })
}

impl Expected {
    /// Parse the file text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut e = Expected::default();
        for (n, line) in text.lines().enumerate() {
            if let Some(rest) = line.strip_prefix("# corpus:") {
                e.corpus_tag = rest.trim().to_string();
                continue;
            }
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(id), Some(obs), Some(issue), None) =
                (cols.next(), cols.next(), cols.next(), cols.next())
            else {
                return Err(format!(
                    "line {}: expected `<id>\\t<observed result>\\t<issue>`",
                    n + 1
                ));
            };
            let agreed = obs.starts_with("AGREED_REJECT");
            if agreed && issue != "-" {
                return Err(format!(
                    "line {}: {id}: an agreed rejection has no issue",
                    n + 1
                ));
            }
            if !agreed && !is_issue_ref(issue) {
                return Err(format!(
                    "line {}: {id}: a divergence must link an open issue as `#NNNN`, got `{issue}`",
                    n + 1
                ));
            }
            let entry = Entry {
                observed: obs.to_string(),
                issue: issue.to_string(),
            };
            if e.entries.insert(id.to_string(), entry).is_some() {
                return Err(format!("line {}: duplicate id {id}", n + 1));
            }
        }
        Ok(e)
    }

    /// Load the committed file. A missing file is an error: an empty contract
    /// must be written down, not implied.
    pub fn load() -> Result<Self, String> {
        let p = expected_path();
        let text = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
        Self::parse(&text)
    }

    /// Render in the committed format: grouped by class, largest first.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "# Vectors on which dugite does not simply match Haskell, among the replayed ImpSpec rules.\n\
             # One `<vector id><TAB><observed result><TAB><issue>` per line. The gate fails on an unknown id,\n\
             # on an entry that now matches (delete it), on a changed observed result, on a stale id and on\n\
             # an entry without an issue. Regenerate only to RECORD a reviewed change (see expected.rs).\n",
        );
        s.push_str(&format!("# corpus: {}\n", self.corpus_tag));
        // Class = the observed result without its digest.
        let class_of = |obs: &str| obs.split(" #").next().unwrap_or(obs).to_string();
        let mut by_class: BTreeMap<String, Vec<(&String, &Entry)>> = BTreeMap::new();
        for (id, e) in &self.entries {
            by_class
                .entry(class_of(&e.observed))
                .or_default()
                .push((id, e));
        }
        let mut classes: Vec<_> = by_class.into_iter().collect();
        classes.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        for (class, mut members) in classes {
            members.sort_by_key(|(id, _)| natural_key(id));
            s.push_str(&format!(
                "\n# {} vector(s), issue {}: {class}\n",
                members.len(),
                members[0].1.issue
            ));
            for (id, e) in members {
                s.push_str(&format!("{id}\t{}\t{}\n", e.observed, e.issue));
            }
        }
        s
    }

    /// What regenerating from `observed` would add, change and remove.
    pub fn diff_summary(&self, observed: &BTreeMap<String, Entry>) -> String {
        let added = observed
            .keys()
            .filter(|k| !self.entries.contains_key(*k))
            .count();
        let removed = self
            .entries
            .keys()
            .filter(|k| !observed.contains_key(*k))
            .count();
        let changed = observed
            .iter()
            .filter(|(k, v)| self.entries.get(*k).is_some_and(|o| o != *v))
            .count();
        format!("added {added}, changed {changed}, removed {removed}")
    }
}

/// The open issues that track an observed result, comma-separated.
///
/// Mapped from what differs, so an entry that shows several classes lists each:
///
/// * `#1132`: NEWEPOCH proposals not ratified / enacted / expired (the import
///   never maps `cgsDRepPulsingState`); `#1133`: `cgsFuturePParams` dropped;
///   `#1134`: `cgsCommittee = SNothing` ignored.
/// * `#1194`: `instant_stake` keeps zero entries; `#1195`: a DRep deregistration
///   keeps its votes (LEDGER); `#1196`: non-myopic likelihoods; `#1197`:
///   mark-snapshot delegations of zero-stake credentials; `#1198`:
///   `dormant_epochs`.
/// * `#1190` / `#1191` / `#1192`: the validator accepts what Haskell rejected
///   (resign-committee-cold cert / DRep refund / proposal deposit); keyed by
///   vector id, because the observed result does not carry the transaction.
///
/// `Ok(None)` for an `AGREED_REJECT` (no issue). `Err` for a result that fits
/// none of these: regeneration then fails instead of inventing an issue.
pub fn issue_for(id: &str, observed: &str) -> Result<Option<String>, String> {
    if observed.starts_with("AGREED_REJECT") {
        return Ok(None);
    }
    let has = |needle: &str| observed.contains(needle);
    let mut issues: Vec<&str> = Vec::new();
    if observed.starts_with("ACCEPTED_BY_DUGITE_REJECTED_BY_HASKELL") {
        match id {
            "LEDGER/test_6097" | "LEDGER/test_6133" => issues.push("#1190"),
            "LEDGER/test_6121" => issues.push("#1191"),
            "LEDGER/test_5672" | "LEDGER/test_7808" | "LEDGER/test_7811" => issues.push("#1192"),
            _ => {}
        }
    }
    if has("gov:future_pparams/") {
        issues.push("#1133");
    }
    if has("gov:committee/present") {
        issues.push("#1134");
    }
    if has("gov:proposal/") && has("inputs:drep_pulsing_state") {
        issues.push("#1132");
    }
    if has("instant_stake:") {
        issues.push("#1194");
    }
    if has("gov:vote/") && id.starts_with("LEDGER/") {
        issues.push("#1195");
    }
    if has("non_myopic:likelihood/") {
        issues.push("#1196");
    }
    if has("snapshots:mark/deleg/") {
        issues.push("#1197");
    }
    if has("vstate:dormant_epochs") {
        issues.push("#1198");
    }
    if issues.is_empty() {
        return Err(format!("{id}: no issue covers `{observed}`"));
    }
    issues.sort();
    Ok(Some(issues.join(",")))
}

/// `Rule/test_N` sorted by rule, then N numerically.
fn natural_key(id: &str) -> (String, u64) {
    let (rule, test) = id.split_once('/').unwrap_or((id, ""));
    let n = test
        .rsplit('_')
        .next()
        .and_then(|d| d.parse().ok())
        .unwrap_or(u64::MAX);
    (rule.to_string(), n)
}

/// What the gate says about one vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Ok,
    /// Diverges (or agreed-rejects), no entry.
    Unknown {
        observed: String,
    },
    /// Entry exists but the vector now matches.
    NowMatches {
        recorded: String,
    },
    /// Entry exists and differs from what was observed.
    Changed {
        recorded: String,
        observed: String,
    },
    /// The harness failed to run the vector.
    HarnessError(String),
}

impl Gate {
    pub fn describe(&self, id: &str) -> Option<String> {
        match self {
            Gate::Ok => None,
            Gate::Unknown { observed } => Some(format!(
                "{id}: not in the expected-results file: {observed}"
            )),
            Gate::NowMatches { recorded } => Some(format!(
                "{id}: now MATCHES Haskell but is still recorded as `{recorded}` — remove the entry"
            )),
            Gate::Changed { recorded, observed } => Some(format!(
                "{id}: observed result changed\n    recorded: {recorded}\n    observed: {observed}"
            )),
            Gate::HarnessError(e) => Some(format!("{id}: harness error: {e}")),
        }
    }
}

/// Judge one verdict against the contract.
pub fn gate(expected: &Expected, id: &str, verdict: &Verdict) -> Gate {
    let recorded = expected.entries.get(id).map(|e| &e.observed);
    match (verdict, recorded) {
        (Verdict::HarnessError(e), _) => Gate::HarnessError(e.clone()),
        (Verdict::Match, None) => Gate::Ok,
        (Verdict::Match, Some(r)) => Gate::NowMatches {
            recorded: r.clone(),
        },
        (Verdict::Diverges(o) | Verdict::AgreedReject(o), None) => Gate::Unknown {
            observed: o.clone(),
        },
        (Verdict::Diverges(o) | Verdict::AgreedReject(o), Some(r)) if o == r => Gate::Ok,
        (Verdict::Diverges(o) | Verdict::AgreedReject(o), Some(r)) => Gate::Changed {
            recorded: r.clone(),
            observed: o.clone(),
        },
    }
}
