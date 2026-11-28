//! Replay one LEDGER / NEWEPOCH vector end to end: decode, import, apply
//! through dugite's production path, project both sides, compare.
//!
//! The result is a [`Verdict`]: either `Match`, or an *observed result* string
//! that the expected-results contract records and later runs must reproduce
//! exactly (see [`expected`](super::expected)).
//!
//! ## Like with like
//!
//! Three things are compared, and each is a pair of the SAME field set taken at
//! the SAME point of the transition:
//!
//! * **post-state** — dugite after the apply vs Haskell's `st_out`;
//! * **precondition** — dugite right after the import vs Haskell's `st`. A
//!   difference here is a field the production importer does not carry across
//!   faithfully; it is reported (`pre=`) so a post-state difference in the same
//!   field is not mistaken for a rule divergence;
//! * **acceptance** — `st_out` absent means Haskell's STS rejected the signal,
//!   and dugite must reject too.

use crate::upstream::ledger_rules_replay::apply::{
    apply_ledger, apply_newepoch, import_state, Applied,
};
use crate::upstream::ledger_rules_replay::bridge::decode_epoch_no;
use crate::upstream::ledger_rules_replay::compare::{compare_projections, CompareResult};
use crate::upstream::ledger_rules_replay::imp_state::{decode_imp_state, RuleKind};
use crate::upstream::ledger_rules_replay::project::{from_dugite, from_haskell, Projection};
use crate::upstream::ledger_rules_replay::vector::ImpVector;
use dugite_ledger::LedgerState;

/// What the replay concluded for one vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// dugite agrees with Haskell on every compared field (or both rejected).
    Match,
    /// A divergence, as the observed-result string the contract records.
    Diverges(String),
    /// The harness could not run the vector (decode failure and the like).
    /// Never recordable: the gate fails on it.
    HarnessError(String),
}

/// A replayed vector with the evidence behind its verdict.
#[derive(Debug)]
pub struct Replay {
    pub kind: RuleKind,
    pub verdict: Verdict,
    /// The post-state comparison, when dugite accepted and Haskell produced a
    /// state.
    pub post: Option<CompareResult>,
    /// The import-fidelity comparison (dugite after import vs Haskell `st`).
    pub pre: CompareResult,
    /// dugite's post-state, when it accepted. Kept so a test can perturb it.
    pub dugite_state: Option<Box<LedgerState>>,
    /// Haskell's post-state projection (`st_out`), when present.
    pub haskell_post: Option<Projection>,
}

/// `Some(kind)` for the two rules this module replays.
pub fn rule_kind(rule: &str) -> Option<RuleKind> {
    let upper = rule.to_uppercase();
    if upper.contains("NEWEPOCH") {
        Some(RuleKind::NewEpoch)
    } else if upper.ends_with("LEDGER") {
        Some(RuleKind::Ledger)
    } else {
        None
    }
}

/// Replay `vec`. Returns `None` when its rule is not replayed here.
pub fn replay_vector(vec: &ImpVector) -> Option<Replay> {
    let kind = rule_kind(&vec.rule)?;
    Some(replay_inner(vec, kind))
}

fn harness_error(kind: RuleKind, msg: String) -> Replay {
    Replay {
        kind,
        verdict: Verdict::HarnessError(msg),
        post: None,
        pre: CompareResult::default(),
        dugite_state: None,
        haskell_post: None,
    }
}

fn replay_inner(vec: &ImpVector, kind: RuleKind) -> Replay {
    // Decode `st` twice: once to project (borrowed), once consumed by the import.
    let st_for_projection = match decode_imp_state(kind, &vec.st_cbor, vec) {
        Ok(s) => s,
        Err(e) => return harness_error(kind, format!("st decode: {e}")),
    };
    let st_for_import = match decode_imp_state(kind, &vec.st_cbor, vec) {
        Ok(s) => s,
        Err(e) => return harness_error(kind, format!("st decode: {e}")),
    };
    // The import's UTxO is the harness's own MemPack conversion, shared by both
    // sides, so the precondition compares the other groups (see `pre=`).
    let haskell_pre = from_haskell(&st_for_projection, false);

    let (imported, env_slot) = import_state(st_for_import);
    let dugite_pre = from_dugite(&imported, kind, false);
    let pre = compare_projections(&dugite_pre, &haskell_pre);

    let applied = match kind {
        RuleKind::Ledger => apply_ledger(imported, vec, env_slot),
        RuleKind::NewEpoch => match decode_epoch_no(&vec.sig_cbor) {
            Ok(e) => apply_newepoch(imported, e),
            Err(e) => return harness_error(kind, format!("sig decode: {e}")),
        },
    };

    let pre_groups = |pre: &CompareResult| -> String {
        let g = pre.kinds();
        if g.is_empty() {
            "none".to_string()
        } else {
            g.join(",")
        }
    };

    match (&vec.st_out_cbor, applied) {
        // Haskell rejected, dugite rejected: agreement.
        (None, Applied::Rejected(_)) => Replay {
            kind,
            verdict: Verdict::Match,
            post: None,
            pre,
            dugite_state: None,
            haskell_post: None,
        },
        (None, Applied::Accepted(_)) => Replay {
            kind,
            verdict: Verdict::Diverges("ACCEPTED_BY_DUGITE_REJECTED_BY_HASKELL".into()),
            post: None,
            pre,
            dugite_state: None,
            haskell_post: None,
        },
        (Some(_), Applied::Rejected(class)) => Replay {
            kind,
            verdict: Verdict::Diverges(format!("REJECTED_BY_DUGITE {class}")),
            post: None,
            pre,
            dugite_state: None,
            haskell_post: None,
        },
        (Some(out), Applied::Accepted(state)) => {
            let st_out = match decode_imp_state(kind, out, vec) {
                Ok(s) => s,
                Err(e) => return harness_error(kind, format!("st_out decode: {e}")),
            };
            let haskell_post = from_haskell(&st_out, true);
            let dugite_post = from_dugite(&state, kind, true);
            let post = compare_projections(&dugite_post, &haskell_post);
            let verdict = if post.matches() {
                Verdict::Match
            } else {
                Verdict::Diverges(format!(
                    "STATE_MISMATCH post={} pre={} #{}",
                    if post.kinds().is_empty() {
                        "not-comparable".to_string()
                    } else {
                        post.kinds().join(",")
                    },
                    pre_groups(&pre),
                    post.digest()
                ))
            };
            Replay {
                kind,
                verdict,
                post: Some(post),
                pre,
                dugite_state: Some(state),
                haskell_post: Some(haskell_post),
            }
        }
    }
}
