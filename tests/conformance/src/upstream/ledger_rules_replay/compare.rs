//! Compare dugite's post-state with Haskell's `st_out` over the enumerated
//! field list in [`project`](super::project).
//!
//! Two projections go in, one list of field-level differences comes out. A group
//! one side does not carry is never a match: it is reported as absent, so a
//! missing writer cannot turn into a green comparison.

use std::collections::BTreeSet;

use crate::upstream::ledger_rules_replay::project::{Group, Projection, GROUPS};

/// One differing path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDiff {
    pub group: &'static str,
    pub path: String,
    /// `None` = the path is absent on that side.
    pub dugite: Option<String>,
    pub haskell: Option<String>,
}

/// Result of comparing two projections.
#[derive(Debug, Clone, Default)]
pub struct CompareResult {
    pub diffs: Vec<PathDiff>,
    /// Groups compared (present on both sides).
    pub compared: Vec<&'static str>,
    /// Groups carried by one side only — never counted as matched.
    pub not_comparable: Vec<&'static str>,
}

impl CompareResult {
    pub fn matches(&self) -> bool {
        self.diffs.is_empty() && self.not_comparable.is_empty()
    }

    /// Distinct differing groups, in [`GROUPS`] order.
    pub fn groups(&self) -> Vec<&'static str> {
        let set: BTreeSet<&str> = self.diffs.iter().map(|d| d.group).collect();
        GROUPS.iter().copied().filter(|g| set.contains(g)).collect()
    }

    /// Distinct differing field KINDS, `group:path-shape`, sorted. A path's
    /// shape replaces every credential/hash/index segment with `*`, so
    /// `acct/0:<hash>` and `acct/1:<hash>` are one kind (`dstate:acct/*`).
    pub fn kinds(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .diffs
            .iter()
            .map(|d| format!("{}:{}", d.group, path_shape(&d.path)))
            .collect();
        set.into_iter().collect()
    }

    /// One line per differing path (group, path, dugite value, haskell value),
    /// sorted: what a digest is taken over, so any change in WHAT differs, not
    /// only in which group, changes it.
    pub fn digest_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .diffs
            .iter()
            .map(|d| {
                format!(
                    "{}|{}|{}|{}",
                    d.group,
                    d.path,
                    d.dugite.as_deref().unwrap_or("<absent>"),
                    d.haskell.as_deref().unwrap_or("<absent>")
                )
            })
            .collect();
        lines.sort();
        lines
    }

    /// Human-readable diff, at most `limit` paths.
    pub fn render(&self, limit: usize) -> String {
        let mut s = String::new();
        for d in self.diffs.iter().take(limit) {
            s.push_str(&format!(
                "  [{}] {}\n      dugite : {}\n      haskell: {}\n",
                d.group,
                d.path,
                d.dugite.as_deref().unwrap_or("<absent>"),
                d.haskell.as_deref().unwrap_or("<absent>")
            ));
        }
        if self.diffs.len() > limit {
            s.push_str(&format!("  … {} more\n", self.diffs.len() - limit));
        }
        s
    }
}

/// Replace identifier-like segments (credentials, hashes, ids, indices) with `*`.
pub fn path_shape(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let hexish = seg.len() >= 8 && seg.chars().all(|c| c.is_ascii_hexdigit());
            let numeric = seg.len() >= 4 && seg.chars().all(|c| c.is_ascii_digit());
            let tagged = seg.contains(':') || seg.contains('#');
            if hexish || numeric || tagged {
                "*"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn diff_group(name: &'static str, d: &Group, h: &Group, out: &mut Vec<PathDiff>) {
    let keys: BTreeSet<&String> = d.keys().chain(h.keys()).collect();
    for k in keys {
        let (dv, hv) = (d.get(k), h.get(k));
        if dv != hv {
            out.push(PathDiff {
                group: name,
                path: k.clone(),
                dugite: dv.cloned(),
                haskell: hv.cloned(),
            });
        }
    }
}

/// Compare `dugite` (what dugite holds) against `haskell` (what Haskell holds).
///
/// A group present on both sides is compared path by path, including paths that
/// exist on one side only. A group present on exactly one side is
/// `not_comparable`.
pub fn compare_projections(dugite: &Projection, haskell: &Projection) -> CompareResult {
    let mut r = CompareResult::default();
    for g in GROUPS {
        match (dugite.groups.get(g), haskell.groups.get(g)) {
            (Some(d), Some(h)) => {
                r.compared.push(g);
                diff_group(g, d, h, &mut r.diffs);
            }
            (None, None) => {}
            _ => r.not_comparable.push(g),
        }
    }
    r
}
