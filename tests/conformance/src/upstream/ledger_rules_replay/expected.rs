//! The expected-results contract for the replayed rules (LEDGER, NEWEPOCH).
//!
//! `tests/conformance/expected/ledger_rules_replay.tsv` holds one
//! `(vector id, observed result)` pair per vector on which dugite is known to
//! diverge from Haskell. The gate fails on:
//!
//! * an **unknown id** — a divergence with no entry (a new regression, or a new
//!   vector);
//! * an entry that now **matches** — it must be removed, so the file only ever
//!   shrinks as divergences are fixed;
//! * an observed result that **differs** from the recorded one — the divergence
//!   changed, which a reviewer must look at;
//! * a **stale id** — an entry whose vector is not in the corpus.
//!
//! This is NOT the `conformance_skip.txt` contract, which only notices a path
//! that stopped existing. Nothing here skips a vector: every entry is a vector
//! that is replayed and whose divergence is pinned.
//!
//! The file is bound to the corpus: its `# corpus:` header must equal
//! `manifest.toml`'s release tag, so bumping the pin forces the file to be
//! regenerated and its diff reviewed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::upstream::fixtures::workspace_root;
use crate::upstream::ledger_rules_replay::replay::Verdict;

/// Environment variable that makes the umbrella test rewrite the file from what
/// it observed instead of failing. Never set in CI.
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

/// The parsed contract.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub corpus_tag: String,
    pub entries: BTreeMap<String, String>,
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
            let (id, obs) = line
                .split_once('\t')
                .ok_or_else(|| format!("line {}: expected `<id>\\t<observed result>`", n + 1))?;
            if e.entries.insert(id.to_string(), obs.to_string()).is_some() {
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

    /// Render in the committed format (sorted by id).
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "# Known divergences of dugite from Haskell on the replayed ImpSpec vectors.\n\
             # One `<vector id><TAB><observed result>` per line. The gate fails on an unknown id,\n\
             # on an entry that now matches (delete it), and on a changed observed result.\n\
             # Regenerate only to RECORD a reviewed change: DUGITE_UPDATE_EXPECTED_RESULTS=1.\n",
        );
        s.push_str(&format!("# corpus: {}\n", self.corpus_tag));
        // Grouped by divergence class (the observed result without its digest),
        // largest class first, so a reviewer reads classes, not 3,000 lines.
        let class_of = |obs: &str| obs.split(" #").next().unwrap_or(obs).to_string();
        let mut by_class: BTreeMap<String, Vec<(&String, &String)>> = BTreeMap::new();
        for (id, obs) in &self.entries {
            by_class.entry(class_of(obs)).or_default().push((id, obs));
        }
        let mut classes: Vec<_> = by_class.into_iter().collect();
        classes.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        for (class, mut members) in classes {
            members.sort_by_key(|(id, _)| natural_key(id));
            s.push_str(&format!("\n# {} vector(s): {class}\n", members.len()));
            for (id, obs) in members {
                s.push_str(&format!("{id}\t{obs}\n"));
            }
        }
        s
    }
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
    /// Diverges, no entry.
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
                "{id}: unknown divergence (no entry in the expected-results file): {observed}"
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
    match (verdict, expected.entries.get(id)) {
        (Verdict::HarnessError(e), _) => Gate::HarnessError(e.clone()),
        (Verdict::Match, None) => Gate::Ok,
        (Verdict::Match, Some(r)) => Gate::NowMatches {
            recorded: r.clone(),
        },
        (Verdict::Diverges(o), None) => Gate::Unknown {
            observed: o.clone(),
        },
        (Verdict::Diverges(o), Some(r)) if o == r => Gate::Ok,
        (Verdict::Diverges(o), Some(r)) => Gate::Changed {
            recorded: r.clone(),
            observed: o.clone(),
        },
    }
}
