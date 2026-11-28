//! The LEDGER / NEWEPOCH comparison must be able to FAIL.
//!
//! A comparison that cannot go red is a check that measures nothing, so this
//! file proves each compared field group bites, against the real corpus and the
//! real replay path:
//!
//! 1. For each group, find a real vector whose dugite post-state already matches
//!    Haskell's `st_out` for that group.
//! 2. Perturb dugite's `LedgerState` in exactly the field(s) that group reads.
//! 3. Re-project and re-compare: that group must now differ.
//!
//! plus a negative control: dugite's post-state for one vector compared against
//! a DIFFERENT vector's `st_out` must not match.
//!
//! Needs the corpus (`cargo xtask download-upstream-fixtures`); with
//! `DUGITE_REQUIRE_UPSTREAM=1` a missing corpus is a failure, otherwise a skip.

use std::collections::BTreeMap;
use std::sync::Arc;

use dugite_conformance::upstream::fixtures;
use dugite_conformance::upstream::ledger_rules_replay::compare::compare_projections;
use dugite_conformance::upstream::ledger_rules_replay::imp_state::RuleKind;
use dugite_conformance::upstream::ledger_rules_replay::project::{from_dugite, GROUPS};
use dugite_conformance::upstream::ledger_rules_replay::replay::{replay_vector, Replay};
use dugite_conformance::upstream::ledger_rules_replay::{collect_test_case_dirs, vector};
use dugite_ledger::LedgerState;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::value::Lovelace;

/// How many vectors of each kind to scan for a per-group green baseline.
const SCAN_LIMIT: usize = 150;

type Perturb = fn(&mut LedgerState);

fn bogus32() -> Hash32 {
    Hash32::from_bytes([0xEE; 32])
}
fn bogus28() -> Hash28 {
    Hash28::from_bytes([0xEE; 28])
}

/// One perturbation per group: a change to a field that group's reader uses.
fn perturbations(kind: RuleKind) -> Vec<(&'static str, Perturb)> {
    let mut v: Vec<(&'static str, Perturb)> = vec![
        ("epoch", |s| s.epoch = EpochNo(s.epoch.0 + 1)),
        ("utxo", |s| {
            match s.utxo.utxo_set.iter().into_iter().next() {
                Some((i, _)) => {
                    s.utxo.utxo_set.remove(&i);
                }
                None => s.utxo.utxo_set.insert(
                    dugite_primitives::transaction::TransactionInput {
                        transaction_id: bogus32(),
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
        ("pots", |s| {
            s.epochs.treasury = Lovelace(s.epochs.treasury.0 + 1)
        }),
        ("dstate", |s| {
            s.certs.reward_accounts.insert(bogus32(), Lovelace(1));
        }),
        ("pstate", |s| {
            s.certs.pending_retirements.insert(bogus28(), EpochNo(1));
        }),
        ("vstate", |s| {
            Arc::make_mut(&mut s.gov.governance).num_dormant_epochs += 1;
        }),
        ("gov", |s| {
            let g = Arc::make_mut(&mut s.gov.governance);
            g.no_confidence = !g.no_confidence;
        }),
        ("pparams", |s| s.epochs.protocol_params.max_tx_size += 1),
        ("instant_stake", |s| {
            s.certs
                .stake_distribution
                .stake_map
                .insert(bogus32(), Lovelace(1));
        }),
    ];
    if kind == RuleKind::NewEpoch {
        v.extend::<Vec<(&'static str, Perturb)>>(vec![
            ("snapshots", |s| {
                s.epochs.snapshots.ss_fee = Lovelace(s.epochs.snapshots.ss_fee.0 + 1)
            }),
            ("blocks_made", |s| {
                Arc::make_mut(&mut s.consensus.epoch_blocks_by_pool).insert(bogus28(), 1);
            }),
            ("non_myopic", |s| {
                s.epochs.non_myopic.reward_pot = Lovelace(s.epochs.non_myopic.reward_pot.0 + 1)
            }),
        ]);
    }
    v
}

fn group_green(r: &Replay, g: &str) -> bool {
    r.post
        .as_ref()
        .map(|p| p.compared.contains(&g) && !p.diffs.iter().any(|d| d.group == g))
        .unwrap_or(false)
}

/// Replay up to `SCAN_LIMIT` vectors of `kind`, keeping those that produced a
/// post-state.
fn scan(dir: &std::path::Path, kind: RuleKind) -> Vec<Replay> {
    let mut out = Vec::new();
    for d in collect_test_case_dirs(dir) {
        let Ok(vec) = vector::decode_vector(&d) else {
            continue;
        };
        let Some(k) =
            dugite_conformance::upstream::ledger_rules_replay::replay::rule_kind(&vec.rule)
        else {
            continue;
        };
        if k != kind {
            continue;
        }
        if let Some(r) = replay_vector(&vec) {
            if r.dugite_state.is_some() && r.haskell_post.is_some() {
                out.push(r);
            }
        }
        if out.len() >= SCAN_LIMIT {
            break;
        }
    }
    out
}

fn bite(kind: RuleKind) {
    let Some(dir) = fixtures::check_area("ledger-rules") else {
        return;
    };
    let replays = scan(&dir, kind);
    assert!(
        !replays.is_empty(),
        "{kind:?}: no vector produced a post-state to perturb"
    );

    let mut proven: BTreeMap<&str, usize> = BTreeMap::new();
    for (group, perturb) in perturbations(kind) {
        // Prefer a baseline whose group actually holds Haskell content, so the
        // perturbation changes a populated group rather than filling an empty one.
        let populated = |r: &&Replay| {
            r.haskell_post
                .as_ref()
                .and_then(|p| p.groups.get(group))
                .is_some_and(|g| !g.is_empty())
        };
        let Some(base) = replays
            .iter()
            .find(|r| group_green(r, group) && populated(r))
            .or_else(|| replays.iter().find(|r| group_green(r, group)))
        else {
            panic!(
                "{kind:?}: group `{group}` matched in none of the first {} vectors, so there is \
                 no green baseline to perturb — the group cannot be shown to bite. \
                 (If the group never matches, that is itself a finding; record it.)",
                replays.len()
            );
        };
        let mut state = (**base.dugite_state.as_ref().unwrap()).clone();
        perturb(&mut state);
        let cmp = compare_projections(
            &from_dugite(&state, kind, true),
            base.haskell_post.as_ref().unwrap(),
        );
        assert!(
            cmp.diffs.iter().any(|d| d.group == group),
            "{kind:?}: perturbing `{group}` did NOT make the `{group}` comparison differ \
             (diffs: {:?})",
            cmp.groups()
        );
        proven.insert(group, cmp.diffs.iter().filter(|d| d.group == group).count());
    }
    eprintln!("[bites] {kind:?}: groups proven to bite (group -> differing paths): {proven:?}");
    for g in GROUPS {
        let expected_here = perturbations(kind).iter().any(|(n, _)| n == g);
        let nes_only = matches!(*g, "snapshots" | "blocks_made" | "non_myopic");
        assert!(
            expected_here || (nes_only && kind == RuleKind::Ledger),
            "{kind:?}: compared group `{g}` has no perturbation test"
        );
    }
}

/// Negative control: a vector's dugite post-state against ANOTHER vector's
/// `st_out` must not match.
fn negative_control(kind: RuleKind) {
    let Some(dir) = fixtures::check_area("ledger-rules") else {
        return;
    };
    let replays = scan(&dir, kind);
    assert!(
        replays.len() >= 2,
        "{kind:?}: need two vectors for the negative control"
    );
    let mut checked = 0;
    for (i, a) in replays.iter().enumerate().take(50) {
        // A vector other than `a`; with a different Haskell post-state.
        let Some(b) = replays
            .iter()
            .enumerate()
            .find(|(j, b)| {
                *j != i
                    && b.haskell_post.as_ref().map(|p| &p.groups)
                        != a.haskell_post.as_ref().map(|p| &p.groups)
            })
            .map(|(_, b)| b)
        else {
            continue;
        };
        let cmp = compare_projections(
            &from_dugite(a.dugite_state.as_ref().unwrap(), kind, true),
            b.haskell_post.as_ref().unwrap(),
        );
        assert!(
            !cmp.matches(),
            "{kind:?}: vector {i}'s dugite post-state MATCHED a different vector's st_out"
        );
        checked += 1;
    }
    assert!(checked > 0, "{kind:?}: negative control compared nothing");
    eprintln!("[bites] {kind:?}: negative control held for {checked} vector pair(s)");
}

#[test]
fn ledger_comparison_bites_every_group() {
    bite(RuleKind::Ledger);
}

#[test]
fn newepoch_comparison_bites_every_group() {
    bite(RuleKind::NewEpoch);
}

#[test]
fn ledger_comparison_rejects_another_vectors_st_out() {
    negative_control(RuleKind::Ledger);
}

#[test]
fn newepoch_comparison_rejects_another_vectors_st_out() {
    negative_control(RuleKind::NewEpoch);
}
