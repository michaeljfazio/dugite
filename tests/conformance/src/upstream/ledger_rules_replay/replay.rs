//! Replay one LEDGER / NEWEPOCH vector end to end: decode, import, apply
//! through dugite's production path, project both sides, compare.
//!
//! The result is a [`Verdict`]: `Match`, `AgreedReject`, or an *observed result*
//! string that the expected-results contract records and later runs must
//! reproduce exactly (see [`expected`](super::expected)).
//!
//! ## Like with like
//!
//! * **post-state** — dugite after the apply vs Haskell's `st_out`;
//! * **precondition** (`pre=`) — dugite right after the import vs Haskell's `st`,
//!   including the transition INPUTS the import drops (`inputs:*`). A difference
//!   here is something the production importer does not carry across, so a
//!   post-state difference is not mistaken for a rule divergence;
//! * **acceptance** — see below.
//!
//! ## Accept / reject
//!
//! * LEDGER: dugite's verdict is its VALIDATOR's (`validate_transaction_with_context`
//!   on the imported pre-state), never `apply_block`'s: for a confirmed block
//!   `apply_block` logs a Phase-1 failure and applies the transaction anyway
//!   (#1131). The validator's error list goes into the observed result.
//! * `st_out` present (Haskell accepted) and the validator rejects: a divergence,
//!   whatever the post-state looks like.
//! * `st_out` absent (Haskell rejected): the dump does not say WHICH predicate
//!   failed, so a validator rejection is recorded as `AgreedReject` with dugite's
//!   reason, not as a proof that the two reasons agree. A validator that accepts is
//!   a divergence.
//! * A panic or a decode failure is a harness error, never a verdict.
//! * NEWEPOCH has no validator; `apply_block` returning an error is the verdict.

use sha2::{Digest, Sha256};

use crate::upstream::ledger_rules_replay::apply::{
    apply_ledger, apply_newepoch, import_state, Applied, ValidatorVerdict,
};
use crate::upstream::ledger_rules_replay::bridge::decode_epoch_no;
use crate::upstream::ledger_rules_replay::compare::{compare_projections, CompareResult};
use crate::upstream::ledger_rules_replay::imp_state::{decode_imp_state, RuleKind};
use crate::upstream::ledger_rules_replay::project::{from_dugite, from_haskell, Projection, Scope};
use crate::upstream::ledger_rules_replay::vector::ImpVector;
use dugite_ledger::LedgerState;

/// What the replay concluded for one vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// dugite agrees with Haskell on every compared field.
    Match,
    /// Haskell rejected and dugite's validator rejects too; carries dugite's
    /// reason (Haskell's predicate is not in the dump).
    AgreedReject(String),
    /// A divergence, as the observed-result string the contract records.
    Diverges(String),
    /// The harness could not run the vector (decode failure, panic and the
    /// like). Never recordable: the gate fails on it.
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
    /// dugite's validator verdict (LEDGER).
    pub validator: Option<ValidatorVerdict>,
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

/// A hook that mutates dugite's post-state before it is projected. Used only by
/// tests that prove the comparison goes red on a perturbed state.
pub type PostHook<'a> = &'a (dyn Fn(&mut LedgerState) + Sync);

/// Replay `vec`. Returns `None` when its rule is not replayed here.
pub fn replay_vector(vec: &ImpVector) -> Option<Replay> {
    replay_vector_with(vec, None)
}

/// [`replay_vector`] with an optional post-state hook.
///
/// Every replay runs on a FRESH thread (so no per-thread cache is shared with an
/// earlier replay on the caller's thread). That is NOT enough to make the verdict
/// independent of what the PROCESS replayed before: in one process the same
/// vectors gave different Phase-2 verdicts depending on which others had been
/// evaluated (observed on `LEDGER/test_7892`, `7945`, `7975` among others, with
/// one thread and with twelve). The reference is therefore a cold process per
/// vector: the generated per-vector tests are exactly that, and the report that
/// regenerates the contract (`mod.rs`, `replay_in_child`) runs each vector in its
/// own child process.
pub fn replay_vector_with(vec: &ImpVector, hook: Option<PostHook<'_>>) -> Option<Replay> {
    let kind = rule_kind(&vec.rule)?;
    let replay = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("imp-replay".into())
            .stack_size(64 * 1024 * 1024)
            .spawn_scoped(scope, || replay_inner(vec, kind, hook))
            .expect("spawn replay thread")
            .join()
    });
    Some(replay.unwrap_or_else(|_| harness_error(kind, "replay thread panicked".into())))
}

fn harness_error(kind: RuleKind, msg: String) -> Replay {
    Replay {
        kind,
        verdict: Verdict::HarnessError(msg),
        post: None,
        pre: CompareResult::default(),
        validator: None,
        dugite_state: None,
        haskell_post: None,
    }
}

fn digest_of(parts: &[String]) -> String {
    let h = Sha256::digest(parts.join("\n").as_bytes());
    h.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn kinds_or(c: &CompareResult, empty: &str) -> String {
    let k = c.kinds();
    if k.is_empty() {
        empty.to_string()
    } else {
        k.join(",")
    }
}

/// A short digest of an entire projection.
fn projection_digest(p: &Projection) -> String {
    let mut lines = Vec::new();
    for (g, m) in &p.groups {
        for (k, v) in m {
            lines.push(format!("{g}|{k}|{v}"));
        }
    }
    digest_of(&lines)
}

fn replay_inner(vec: &ImpVector, kind: RuleKind, hook: Option<PostHook<'_>>) -> Replay {
    // Decode `st` twice: once to project (borrowed), once consumed by the import.
    let st_for_projection = match decode_imp_state(kind, &vec.st_cbor, vec) {
        Ok(s) => s,
        Err(e) => return harness_error(kind, format!("st decode: {e}")),
    };
    let st_for_import = match decode_imp_state(kind, &vec.st_cbor, vec) {
        Ok(s) => s,
        Err(e) => return harness_error(kind, format!("st decode: {e}")),
    };
    let haskell_pre = from_haskell(&st_for_projection, Scope::Pre);

    let (imported, env_slot) = import_state(st_for_import);
    let dugite_pre = from_dugite(&imported, kind, Scope::Pre);
    let pre = compare_projections(&dugite_pre, &haskell_pre);

    // (validator verdict, apply result)
    let (validator, applied) = match kind {
        RuleKind::Ledger => {
            let run = apply_ledger(imported, vec, env_slot);
            (run.validator, run.applied)
        }
        RuleKind::NewEpoch => match decode_epoch_no(&vec.sig_cbor) {
            Ok(e) => (None, apply_newepoch(imported, e)),
            Err(e) => return harness_error(kind, format!("sig decode: {e}")),
        },
    };
    if let Applied::Harness(msg) = &applied {
        return harness_error(kind, msg.clone());
    }

    let mk = |verdict, post, state, haskell_post| Replay {
        kind,
        verdict,
        post,
        pre: pre.clone(),
        validator: validator.clone(),
        dugite_state: state,
        haskell_post,
    };

    let v_rejects = validator.as_ref().is_some_and(|v| !v.accepts());
    let v_class = validator
        .as_ref()
        .map(|v| v.class_string())
        .unwrap_or_else(|| "n/a".into());
    let v_details = validator
        .as_ref()
        .map(|v| v.details.clone())
        .unwrap_or_default();

    match (&vec.st_out_cbor, applied) {
        (_, Applied::Harness(_)) => unreachable!("handled above"),

        // ── Haskell rejected ────────────────────────────────────────────────
        (None, applied) => {
            if v_rejects {
                // Haskell's predicate is not in the dump: record dugite's reason.
                mk(
                    Verdict::AgreedReject(format!("AGREED_REJECT validator={v_class}")),
                    None,
                    None,
                    None,
                )
            } else {
                let (apply_note, state_digest) = match &applied {
                    Applied::Accepted(s) => (
                        "applied".to_string(),
                        projection_digest(&from_dugite(s, kind, Scope::Post)),
                    ),
                    Applied::Rejected(c) => (format!("apply_rejected={c}"), String::new()),
                    Applied::Harness(_) => unreachable!(),
                };
                let mut parts = vec![v_class.clone(), apply_note.clone(), state_digest];
                parts.extend(pre.digest_lines());
                mk(
                    Verdict::Diverges(format!(
                        "ACCEPTED_BY_DUGITE_REJECTED_BY_HASKELL validator={v_class} {apply_note} pre={} #{}",
                        kinds_or(&pre, "none"),
                        digest_of(&parts)
                    )),
                    None,
                    None,
                    None,
                )
            }
        }

        // ── Haskell accepted, dugite's apply failed ─────────────────────────
        (Some(_), Applied::Rejected(class)) => {
            let mut parts = vec![v_class.clone(), class.clone()];
            parts.extend(v_details.clone());
            parts.extend(pre.digest_lines());
            mk(
                Verdict::Diverges(format!(
                    "REJECTED_BY_DUGITE validator={v_class} apply={class} pre={} #{}",
                    kinds_or(&pre, "none"),
                    digest_of(&parts)
                )),
                None,
                None,
                None,
            )
        }

        // ── Haskell accepted, dugite applied ────────────────────────────────
        (Some(out), Applied::Accepted(mut state)) => {
            let st_out = match decode_imp_state(kind, out, vec) {
                Ok(s) => s,
                Err(e) => return harness_error(kind, format!("st_out decode: {e}")),
            };
            if let Some(h) = hook {
                h(&mut state);
            }
            let haskell_post = from_haskell(&st_out, Scope::Post);
            let dugite_post = from_dugite(&state, kind, Scope::Post);
            let post = compare_projections(&dugite_post, &haskell_post);

            let verdict = if v_rejects || !post.matches() {
                let mut parts = vec![v_class.clone()];
                parts.extend(v_details.clone());
                parts.extend(post.digest_lines());
                parts.push("--pre--".into());
                parts.extend(pre.digest_lines());
                Verdict::Diverges(format!(
                    "{} post={} pre={} #{}",
                    if v_rejects {
                        format!("VALIDATOR_REJECTS validator={v_class}")
                    } else {
                        "STATE_MISMATCH".to_string()
                    },
                    kinds_or(
                        &post,
                        if post.matches() {
                            "none"
                        } else {
                            "not-comparable"
                        }
                    ),
                    kinds_or(&pre, "none"),
                    digest_of(&parts)
                ))
            } else {
                Verdict::Match
            };
            mk(verdict, Some(post), Some(state), Some(haskell_post))
        }
    }
}
