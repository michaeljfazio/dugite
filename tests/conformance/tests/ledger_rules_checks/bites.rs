//! The LEDGER / NEWEPOCH comparison must be able to FAIL.
//!
//! A comparison that cannot go red measures nothing, so this file proves it on
//! the real corpus and the real per-vector path:
//!
//! 1. **System level.** For each rule, take a vector that MATCHES, perturb dugite's
//!    post-state with a hook and run it through
//!    [`check_one_test_dir_with`] — the exact function the generated
//!    `upstream_ledger_rules_apply_compare_*` tests call (replay, projection,
//!    comparison, observed result, contract gate). It must FAIL, and the failure
//!    must name the field kind that was perturbed. The perturbations cover every
//!    compared field kind (a proposal, a vote, a committee member, the threshold,
//!    the constitution, an enacted root, future pparams, pool params, DReps,
//!    delegations/accounts, deposits, snapshot stake / delegations / pools,
//!    likelihoods, fees, donation, reserves, ...), not one field per group.
//! 2. **Accept / reject axis.** A vector whose transaction dugite's validator
//!    rejects while Haskell accepted, one that dugite accepts while Haskell
//!    rejected, and an undecodable signal, must each go red (or be a harness
//!    error), never a match.
//! 3. **Negative control.** One vector's dugite post-state against ANOTHER vector's
//!    `st_out` must not match.
//!
//! Needs the corpus (`cargo xtask download-upstream-fixtures`); with
//! `DUGITE_REQUIRE_UPSTREAM=1` a missing corpus is a failure, otherwise a skip.

use std::path::PathBuf;
use std::sync::Arc;

use dugite_conformance::upstream::fixtures;
use dugite_conformance::upstream::ledger_rules_replay::compare::compare_projections;
use dugite_conformance::upstream::ledger_rules_replay::expected::{gate, Expected, Gate};
use dugite_conformance::upstream::ledger_rules_replay::imp_state::RuleKind;
use dugite_conformance::upstream::ledger_rules_replay::project::{from_dugite, Scope};
use dugite_conformance::upstream::ledger_rules_replay::replay::{
    replay_vector, rule_kind, Replay, Verdict,
};
use dugite_conformance::upstream::ledger_rules_replay::vector::{decode_vector, ImpVector};
use dugite_conformance::upstream::ledger_rules_replay::{
    check_one_test_dir, check_one_test_dir_with, collect_test_case_dirs, VectorOutcome,
};
use dugite_ledger::state::{FuturePParams, Likelihood, PoolRegistration, StakeSnapshot};
use dugite_ledger::LedgerState;
use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Anchor, Constitution, GovAction, GovActionId, ProposalProcedure, Rational, Vote, Voter,
    VotingProcedure,
};
use dugite_primitives::value::Lovelace;

type Perturb = fn(&mut LedgerState);

fn h32() -> Hash32 {
    Hash32::from_bytes([0xEE; 32])
}
fn h28() -> Hash28 {
    Hash28::from_bytes([0xEE; 28])
}
fn gid() -> GovActionId {
    GovActionId {
        transaction_id: h32(),
        action_index: 0,
    }
}
fn anchor() -> Anchor {
    Anchor {
        url: "https://perturbed".into(),
        data_hash: h32(),
    }
}
fn pool_reg() -> PoolRegistration {
    PoolRegistration {
        pool_id: h28(),
        vrf_keyhash: h32(),
        pledge: Lovelace(1),
        cost: Lovelace(1),
        margin_numerator: 0,
        margin_denominator: 1,
        reward_account: vec![0xe0; 29],
        owners: vec![],
        relays: vec![],
        metadata_url: None,
        metadata_hash: None,
    }
}

/// (what is perturbed, the field kind that must show up, applies to).
/// `None` = both rules.
fn perturbations() -> Vec<(&'static str, &'static str, Option<RuleKind>, Perturb)> {
    use RuleKind::NewEpoch;
    let table: Vec<(&'static str, &'static str, Option<RuleKind>, Perturb)> = vec![
        ("utxo entry removed/added", "utxo:*", None, |s| {
            match s.utxo.utxo_set.iter().into_iter().next() {
                Some((i, _)) => {
                    s.utxo.utxo_set.remove(&i);
                }
                None => s.utxo.utxo_set.insert(
                    dugite_primitives::transaction::TransactionInput {
                        transaction_id: h32(),
                        index: 0,
                    },
                    dugite_primitives::transaction::TransactionOutput {
                        address: dugite_primitives::address::Address::Byron(
                            dugite_primitives::address::ByronAddress {
                                payload: vec![1; 32],
                            },
                        ),
                        value: dugite_primitives::value::Value::lovelace(1),
                        datum: dugite_primitives::transaction::OutputDatum::None,
                        script_ref: None,
                        is_legacy: false,
                        raw_cbor: None,
                    },
                ),
            };
        }),
        ("fees", "pots:fees", None, |s| {
            s.utxo.epoch_fees = Lovelace(s.utxo.epoch_fees.0 + 1)
        }),
        ("donation", "pots:donation", None, |s| {
            s.utxo.pending_donations = Lovelace(s.utxo.pending_donations.0 + 1)
        }),
        ("treasury", "pots:treasury", Some(NewEpoch), |s| {
            s.epochs.treasury = Lovelace(s.epochs.treasury.0 + 1)
        }),
        ("reserves", "pots:reserves", Some(NewEpoch), |s| {
            s.epochs.reserves = Lovelace(s.epochs.reserves.0 + 1)
        }),
        ("epoch", "epoch:epoch", Some(NewEpoch), |s| {
            s.epoch = EpochNo(s.epoch.0 + 1)
        }),
        ("reward account", "dstate:acct/*", None, |s| {
            s.certs.reward_accounts.insert(h32(), Lovelace(1));
        }),
        (
            "stake-key deposit total",
            "dstate:total_key_deposits",
            None,
            |s| s.certs.total_stake_key_deposits += 1,
        ),
        ("pool registration", "pstate:pool/*", None, |s| {
            Arc::make_mut(&mut s.certs.pool_params).insert(h28(), pool_reg());
        }),
        ("pool retirement", "pstate:retire/*", None, |s| {
            s.certs.pending_retirements.insert(h28(), EpochNo(1));
        }),
        ("VRF key hash", "pstate:vrf/*", None, |s| {
            s.certs.vrf_key_hashes.insert(h32(), 1);
        }),
        ("DRep registration", "vstate:drep/*", None, |s| {
            let g = Arc::make_mut(&mut s.gov.governance);
            g.dreps.insert(
                h32(),
                dugite_ledger::state::DRepRegistration {
                    credential: Credential::VerificationKey(h28()),
                    deposit: Lovelace(1),
                    anchor: None,
                    registered_epoch: EpochNo(0),
                    drep_expiry: EpochNo(1),
                    active: true,
                    delegs: Default::default(),
                },
            );
        }),
        ("dormant epochs", "vstate:dormant_epochs", None, |s| {
            Arc::make_mut(&mut s.gov.governance).num_dormant_epochs += 1
        }),
        ("committee resignation", "vstate:cc/*", None, |s| {
            Arc::make_mut(&mut s.gov.governance)
                .committee_resigned
                .insert(h32(), None);
        }),
        ("proposal", "gov:proposal/*", None, |s| {
            Arc::make_mut(&mut s.gov.governance).proposals.insert(
                gid(),
                dugite_ledger::state::ProposalState {
                    procedure: ProposalProcedure {
                        deposit: Lovelace(1),
                        return_addr: vec![0xe0; 29],
                        gov_action: GovAction::InfoAction,
                        anchor: anchor(),
                    },
                    proposed_epoch: EpochNo(1),
                    expires_epoch: EpochNo(2),
                    yes_votes: 0,
                    no_votes: 0,
                    abstain_votes: 0,
                    submission_index: 0,
                },
            );
        }),
        ("DRep vote", "gov:vote/*/drep/*", None, |s| {
            let g = Arc::make_mut(&mut s.gov.governance);
            g.votes_by_action.insert(gid(), Default::default());
            g.votes_by_action
                .get_mut(&gid())
                .expect("just inserted")
                .insert(
                    Voter::DRep(Credential::VerificationKey(h28())),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                );
        }),
        ("committee member", "gov:committee/member/*", None, |s| {
            Arc::make_mut(&mut s.gov.governance)
                .committee_expiration
                .insert(h32(), EpochNo(1));
        }),
        (
            "committee threshold",
            "gov:committee/threshold",
            None,
            |s| {
                Arc::make_mut(&mut s.gov.governance).committee_threshold = Some(Rational {
                    numerator: 7,
                    denominator: 9,
                })
            },
        ),
        (
            "committee present / no-confidence",
            "gov:committee/present",
            None,
            |s| {
                let g = Arc::make_mut(&mut s.gov.governance);
                g.no_confidence = !g.no_confidence;
            },
        ),
        ("constitution", "gov:constitution", None, |s| {
            Arc::make_mut(&mut s.gov.governance).constitution = Some(Constitution {
                anchor: anchor(),
                script_hash: None,
            })
        }),
        ("enacted root", "gov:root/pparam_update", None, |s| {
            Arc::make_mut(&mut s.gov.governance).enacted_pparam_update = Some(gid())
        }),
        (
            "future pparams",
            "gov:future_pparams/tag",
            Some(NewEpoch),
            |s| {
                let pp = s.epochs.protocol_params.clone();
                Arc::make_mut(&mut s.gov.governance).future_pparams =
                    FuturePParams::DefinitePParamsUpdate(Box::new(pp));
            },
        ),
        ("current pparams", "pparams:cur/max_tx_size", None, |s| {
            s.epochs.protocol_params.max_tx_size += 1
        }),
        ("previous pparams", "pparams:prev/max_tx_size", None, |s| {
            s.epochs.prev_protocol_params.max_tx_size += 1
        }),
        ("instant stake", "instant_stake:*", None, |s| {
            s.certs
                .stake_distribution
                .stake_map
                .insert(h32(), Lovelace(1));
        }),
        (
            "snapshot stake",
            "snapshots:mark/stake/*",
            Some(NewEpoch),
            |s| {
                let m = s
                    .epochs
                    .snapshots
                    .mark
                    .get_or_insert_with(|| StakeSnapshot::empty(EpochNo(0)));
                Arc::make_mut(&mut m.stake_distribution).insert(h32(), Lovelace(1));
            },
        ),
        (
            "snapshot delegation",
            "snapshots:mark/deleg/*",
            Some(NewEpoch),
            |s| {
                let m = s
                    .epochs
                    .snapshots
                    .mark
                    .get_or_insert_with(|| StakeSnapshot::empty(EpochNo(0)));
                Arc::make_mut(&mut m.delegations).insert(h32(), h28());
            },
        ),
        (
            "snapshot pool",
            "snapshots:mark/pool/*",
            Some(NewEpoch),
            |s| {
                let m = s
                    .epochs
                    .snapshots
                    .mark
                    .get_or_insert_with(|| StakeSnapshot::empty(EpochNo(0)));
                Arc::make_mut(&mut m.pool_params).insert(h28(), pool_reg());
            },
        ),
        ("snapshot fee", "snapshots:fee", Some(NewEpoch), |s| {
            s.epochs.snapshots.ss_fee = Lovelace(s.epochs.snapshots.ss_fee.0 + 1)
        }),
        ("blocks made", "blocks_made:cur/*", Some(NewEpoch), |s| {
            Arc::make_mut(&mut s.consensus.epoch_blocks_by_pool).insert(h28(), 1);
        }),
        (
            "likelihood",
            "non_myopic:likelihood/*",
            Some(NewEpoch),
            |s| {
                s.epochs
                    .non_myopic
                    .likelihoods
                    .insert(h28(), Likelihood(vec![0.0; 100]));
            },
        ),
        ("reward pot", "non_myopic:reward_pot", Some(NewEpoch), |s| {
            s.epochs.non_myopic.reward_pot = Lovelace(s.epochs.non_myopic.reward_pot.0 + 1)
        }),
    ];
    table
}

fn fixtures_dir() -> Option<PathBuf> {
    fixtures::check_area("ledger-rules")
}

/// The first directory of `kind` whose replay is a plain `Match`.
fn first_match_dir(
    dir: &std::path::Path,
    kind: RuleKind,
    want: impl Fn(&Replay) -> bool,
) -> PathBuf {
    for d in collect_test_case_dirs(dir) {
        let Ok(v) = decode_vector(&d) else { continue };
        if rule_kind(&v.rule) != Some(kind) {
            continue;
        }
        if let Some(r) = replay_vector(&v) {
            if r.verdict == Verdict::Match && r.dugite_state.is_some() && want(&r) {
                return d;
            }
        }
    }
    panic!("{kind:?}: no matching vector found in the corpus");
}

fn system_level(kind: RuleKind) {
    let Some(dir) = fixtures_dir() else { return };
    let d = first_match_dir(&dir, kind, |_| true);
    // Baseline: the unperturbed path passes.
    assert_eq!(
        check_one_test_dir(&d),
        VectorOutcome::Pass,
        "{kind:?}: baseline {d:?}"
    );
    let mut proven = Vec::new();
    for (what, want_kind, only, perturb) in perturbations() {
        if only.is_some_and(|k| k != kind) {
            continue;
        }
        let hook = |s: &mut LedgerState| perturb(s);
        match check_one_test_dir_with(&d, Some(&hook)) {
            VectorOutcome::Fail(msg) => {
                assert!(
                    msg.contains(want_kind),
                    "{kind:?}: perturbing `{what}` failed, but not on `{want_kind}`: {msg}"
                );
                proven.push(want_kind);
            }
            other => panic!(
                "{kind:?}: perturbing `{what}` did NOT make the per-vector check fail: {other:?}"
            ),
        }
    }
    eprintln!(
        "[bites] {kind:?}: {} field kinds proven to turn the per-vector check red: {proven:?}",
        proven.len()
    );
}

#[test]
fn ledger_every_compared_field_kind_turns_the_per_vector_check_red() {
    system_level(RuleKind::Ledger);
}

#[test]
fn newepoch_every_compared_field_kind_turns_the_per_vector_check_red() {
    system_level(RuleKind::NewEpoch);
}

/// Copy of a vector (the decoded struct is not `Clone`).
fn copy(v: &ImpVector) -> ImpVector {
    ImpVector {
        rule: v.rule.clone(),
        ctx_cbor: v.ctx_cbor.clone(),
        env_cbor: v.env_cbor.clone(),
        st_cbor: v.st_cbor.clone(),
        sig_cbor: v.sig_cbor.clone(),
        st_out_cbor: v.st_out_cbor.clone(),
    }
}

/// A LEDGER vector Haskell accepted and dugite matches: the first directory that
/// is not a recorded entry and whose replay is a plain `Match`.
fn matching_ledger_vector(dir: &std::path::Path) -> ImpVector {
    let exp = Expected::load().expect("expected-results file");
    for d in collect_test_case_dirs(dir) {
        let Ok(v) = decode_vector(&d) else { continue };
        if rule_kind(&v.rule) != Some(RuleKind::Ledger)
            || exp.entries.contains_key(&format!(
                "{}/{}",
                v.rule,
                d.file_name().unwrap().to_string_lossy()
            ))
        {
            continue;
        }
        if let Some(r) = replay_vector(&v) {
            if r.verdict == Verdict::Match && r.dugite_state.is_some() {
                return v;
            }
        }
    }
    panic!("no matching LEDGER vector");
}

/// A LEDGER vector Haskell rejected and dugite's validator rejects too, taken
/// from the recorded `AGREED_REJECT` entries.
fn agreed_reject_vector(dir: &std::path::Path) -> ImpVector {
    let exp = Expected::load().expect("expected-results file");
    let id = exp
        .entries
        .iter()
        .find(|(id, e)| id.starts_with("LEDGER/") && e.observed.starts_with("AGREED_REJECT"))
        .map(|(id, _)| id.clone())
        .expect("an AGREED_REJECT entry");
    let v = decode_vector(&dir.join(&id)).expect("decode agreed-reject vector");
    assert!(matches!(
        replay_vector(&v).expect("replayed").verdict,
        Verdict::AgreedReject(_)
    ));
    v
}

/// A divergence where dugite's validator rejected a transaction Haskell accepted
/// (`VALIDATOR_REJECTS`, or `REJECTED_BY_DUGITE` when `apply_block` refused too).
fn rejects_what_haskell_accepted(observed: &str) -> bool {
    (observed.starts_with("VALIDATOR_REJECTS") || observed.starts_with("REJECTED_BY_DUGITE"))
        && !observed.contains("validator=ok")
}

#[test]
fn accept_reject_axis_goes_red() {
    let Some(dir) = fixtures_dir() else { return };

    // A vector Haskell accepted and dugite matches.
    let accepted = matching_ledger_vector(&dir);
    // A vector Haskell rejected and dugite's validator rejects too.
    let agreed = agreed_reject_vector(&dir);
    assert!(accepted.st_out_cbor.is_some() && agreed.st_out_cbor.is_none());

    // 1. dugite's validator rejects what Haskell accepted: the SAME vector, but
    //    its transaction is the one Haskell rejected (its inputs do not exist in
    //    this state).
    let mut v = copy(&accepted);
    v.sig_cbor = agreed.sig_cbor.clone();
    let r = replay_vector(&v).expect("replayed");
    match &r.verdict {
        Verdict::Diverges(o) => assert!(rejects_what_haskell_accepted(o), "{o}"),
        other => panic!("validator-rejects-what-Haskell-accepted was not a divergence: {other:?}"),
    }

    // 2. dugite accepts what Haskell rejected: drop `st_out` from an accepted vector.
    let mut v = copy(&accepted);
    v.st_out_cbor = None;
    match replay_vector(&v).expect("replayed").verdict {
        Verdict::Diverges(o) => assert!(
            o.starts_with("ACCEPTED_BY_DUGITE_REJECTED_BY_HASKELL"),
            "{o}"
        ),
        other => panic!("accepted-what-Haskell-rejected was not a divergence: {other:?}"),
    }

    // 3. Haskell's rejection with a different validator outcome changes the
    //    recorded result: an agreed rejection gains a state and becomes a divergence.
    let mut v = copy(&agreed);
    v.st_out_cbor = accepted.st_out_cbor.clone();
    match replay_vector(&v).expect("replayed").verdict {
        Verdict::Diverges(o) => assert!(rejects_what_haskell_accepted(&o), "{o}"),
        other => panic!("agreed rejection given a st_out was not a divergence: {other:?}"),
    }

    // 4. An undecodable signal is a harness error, never a "both reject" match.
    let mut v = copy(&agreed);
    v.sig_cbor = vec![0xff, 0x00];
    match replay_vector(&v).expect("replayed").verdict {
        Verdict::HarnessError(_) => {}
        other => panic!("undecodable signal was not a harness error: {other:?}"),
    }

    // 5. The gate rejects a recorded agreed-rejection that now matches, and a
    //    verdict whose reason changed.
    let exp = dugite_conformance::upstream::ledger_rules_replay::expected::Expected::parse(
        "# corpus: t\nLEDGER/test_1\tAGREED_REJECT validator=X\t-\n",
    )
    .unwrap();
    assert_eq!(
        gate(
            &exp,
            "LEDGER/test_1",
            &Verdict::AgreedReject("AGREED_REJECT validator=X".into())
        ),
        Gate::Ok
    );
    assert!(matches!(
        gate(
            &exp,
            "LEDGER/test_1",
            &Verdict::AgreedReject("AGREED_REJECT validator=Y".into())
        ),
        Gate::Changed { .. }
    ));
    assert!(matches!(
        gate(&exp, "LEDGER/test_1", &Verdict::Match),
        Gate::NowMatches { .. }
    ));
}

/// A vector's dugite post-state against ANOTHER vector's `st_out` must not match.
fn negative_control(kind: RuleKind) {
    let Some(dir) = fixtures_dir() else { return };
    let mut replays: Vec<Replay> = Vec::new();
    for d in collect_test_case_dirs(&dir) {
        let Ok(v) = decode_vector(&d) else { continue };
        if rule_kind(&v.rule) != Some(kind) {
            continue;
        }
        if let Some(r) = replay_vector(&v) {
            if r.dugite_state.is_some() && r.haskell_post.is_some() {
                replays.push(r);
            }
        }
        if replays.len() >= 30 {
            break;
        }
    }
    assert!(replays.len() >= 2, "{kind:?}: need two vectors");
    let mut checked = 0;
    for (i, a) in replays.iter().enumerate() {
        let Some(b) = replays.iter().enumerate().find(|(j, b)| {
            *j != i
                && b.haskell_post.as_ref().map(|p| &p.groups)
                    != a.haskell_post.as_ref().map(|p| &p.groups)
        }) else {
            continue;
        };
        let cmp = compare_projections(
            &from_dugite(a.dugite_state.as_ref().unwrap(), kind, Scope::Post),
            b.1.haskell_post.as_ref().unwrap(),
        );
        assert!(
            !cmp.matches(),
            "{kind:?}: vector {i} matched another vector's st_out"
        );
        checked += 1;
    }
    assert!(checked > 0, "{kind:?}: negative control compared nothing");
    eprintln!("[bites] {kind:?}: negative control held for {checked} pair(s)");
}

#[test]
fn ledger_comparison_rejects_another_vectors_st_out() {
    negative_control(RuleKind::Ledger);
}

#[test]
fn newepoch_comparison_rejects_another_vectors_st_out() {
    negative_control(RuleKind::NewEpoch);
}
