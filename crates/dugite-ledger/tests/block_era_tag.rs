//! `LedgerState::apply_block` must not follow a block's own era tag (#1233).
//!
//! Haskell fixes a block's era from the LEDGER (`extendToSlot`) and rejects a
//! disagreeing block (`HardForkLedgerErrorWrongEra`). dugite used to set
//! `self.era = block.era`, so a Babbage-tagged block on a Conway ledger moved
//! the ledger back to Babbage, and a Conway-tagged block on a Babbage ledger
//! ran the Conway era transition early.
//!
//! The rule under test (`LedgerState::check_block_era`):
//! * a downgrade is always an error;
//! * an upgrade is an error unless the block is the first of a new epoch;
//! * a ledger whose tip is still `Origin` is exempt (genesis fixes the era).

use dugite_ledger::state::{BlockValidationMode, LedgerError, LedgerState};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, SlotNo};

/// One default (Byron-length) epoch: `LedgerState::new` leaves
/// `byron_epoch_length = 21_600` and `shelley_transition_epoch = 208`, so every
/// slot below `208 * 21_600` falls in a 21_600-slot epoch.
const EPOCH: u64 = 21_600;

/// An empty block of `era` at `slot` extending `prev_hash`. The hash is a
/// function of `(block_no, era)` so two blocks never collide.
fn block(era: Era, slot: u64, block_no: u64, prev_hash: Hash32) -> Block {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&block_no.to_be_bytes());
    hash[8] = era.to_era_index() as u8;
    Block {
        header: BlockHeader {
            header_hash: Hash32::from_bytes(hash),
            prev_hash,
            issuer_vkey: vec![],
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
            block_number: BlockNo(block_no),
            slot: SlotNo(slot),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion {
                major: 10,
                minor: 0,
            },
            kes_signature: vec![],
        },
        transactions: vec![],
        era,
        raw_cbor: None,
        byron: None,
    }
}

/// A ledger in `era` with a real (non-Origin) tip in epoch 0, built the way a
/// node builds one: the genesis ledger applies its first block, which fixes
/// the era.
fn ledger_in(era: Era) -> LedgerState {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 10;
    let mut ls = LedgerState::new(params);
    let first = block(era, 100, 1, Hash32::ZERO);
    ls.apply_block(&first, BlockValidationMode::ApplyOnly)
        .expect("the first block of a genesis ledger fixes its era");
    assert_eq!(ls.era, era);
    assert_ne!(ls.tip.point, dugite_primitives::block::Point::Origin);
    ls
}

fn tip_hash(ls: &LedgerState) -> Hash32 {
    *ls.tip.point.hash().expect("non-Origin tip")
}

fn assert_wrong_era(r: Result<(), LedgerError>, block_era: Era, ledger_era: Era, why: &str) {
    match r {
        Err(LedgerError::WrongBlockEra {
            block_era: b,
            ledger_era: l,
            reason,
        }) => {
            assert_eq!((b, l), (block_era, ledger_era));
            assert!(
                reason.contains(why),
                "reason {reason:?} must mention {why:?}"
            );
        }
        other => panic!("expected WrongBlockEra({why}), got {other:?}"),
    }
}

/// A Babbage tag on a Conway ledger: rejected in EVERY validation mode, and the
/// ledger is untouched (era, tip, pending transition).
#[test]
fn downgrade_is_rejected_in_every_mode_and_leaves_the_ledger_untouched() {
    for mode in [
        BlockValidationMode::ApplyOnly,
        BlockValidationMode::ValidateAll,
    ] {
        let mut ls = ledger_in(Era::Conway);
        let tip_before = ls.tip.clone();
        let bad = block(Era::Babbage, 200, 2, tip_hash(&ls));

        assert_wrong_era(
            ls.apply_block(&bad, mode),
            Era::Babbage,
            Era::Conway,
            "downgrade",
        );
        assert_eq!(ls.era, Era::Conway, "the ledger era must not move");
        assert_eq!(ls.tip, tip_before, "the tip must not move");
        assert!(ls.pending_era_transition.is_none());

        // The honest block that follows still applies.
        let honest = block(Era::Conway, 200, 2, tip_hash(&ls));
        ls.apply_block(&honest, BlockValidationMode::ApplyOnly)
            .expect("an honest block applies after a rejected one");
        assert_eq!(ls.era, Era::Conway);
        assert_eq!(ls.tip.block_number, BlockNo(2));
    }
}

/// A Byron tag on a post-Byron ledger is a downgrade like any other.
#[test]
fn byron_tag_on_a_conway_ledger_is_rejected() {
    let mut ls = ledger_in(Era::Conway);
    let bad = block(Era::Byron, 200, 2, tip_hash(&ls));
    assert_wrong_era(
        ls.apply_block(&bad, BlockValidationMode::ApplyOnly),
        Era::Byron,
        Era::Conway,
        "downgrade",
    );
    assert_eq!(ls.era, Era::Conway);
}

/// A higher tag in the middle of an epoch would run the era transition early.
#[test]
fn upgrade_inside_an_epoch_is_rejected() {
    for (ledger, tag) in [
        (Era::Babbage, Era::Conway),
        (Era::Conway, Era::Dijkstra),
        (Era::Byron, Era::Shelley),
    ] {
        let mut ls = ledger_in(ledger);
        let tip_before = ls.tip.clone();
        // Slot 200 is still epoch 0, the epoch of the ledger tip.
        let bad = block(tag, 200, 2, tip_hash(&ls));
        assert_wrong_era(
            ls.apply_block(&bad, BlockValidationMode::ApplyOnly),
            tag,
            ledger,
            "first block of a new epoch",
        );
        assert_eq!(ls.era, ledger);
        assert_eq!(ls.tip, tip_before);
        assert!(ls.pending_era_transition.is_none());
    }
}

/// A legitimate hard fork: the first block of the next epoch carries the new
/// era, and the ledger follows it (and records the transition).
#[test]
fn upgrade_on_the_first_block_of_a_new_epoch_is_accepted() {
    let mut ls = ledger_in(Era::Babbage);
    let fork = block(Era::Conway, EPOCH + 100, 2, tip_hash(&ls));
    ls.apply_block(&fork, BlockValidationMode::ApplyOnly)
        .expect("the first Conway block of a new epoch is a legitimate era boundary");
    assert_eq!(ls.era, Era::Conway);
    assert_eq!(ls.epoch.0, 1);
    assert_eq!(
        ls.pending_era_transition,
        Some((
            Era::Babbage,
            Era::Conway,
            dugite_primitives::time::EpochNo(0)
        ))
    );
}

/// Every consecutive pair of eras can fork at an epoch boundary, and an epoch
/// with no blocks in between does not matter (the next block is still in a
/// later epoch than the ledger tip).
#[test]
fn upgrade_across_an_empty_epoch_is_accepted() {
    let mut ls = ledger_in(Era::Alonzo);
    let fork = block(Era::Conway, 3 * EPOCH + 100, 2, tip_hash(&ls));
    ls.apply_block(&fork, BlockValidationMode::ApplyOnly)
        .expect("an upgrade after empty epochs is still on an epoch boundary");
    assert_eq!(ls.era, Era::Conway);
}

/// Blocks of the ledger's own era apply anywhere.
#[test]
fn same_era_blocks_apply_inside_and_across_epochs() {
    let mut ls = ledger_in(Era::Conway);
    let b2 = block(Era::Conway, 200, 2, tip_hash(&ls));
    ls.apply_block(&b2, BlockValidationMode::ApplyOnly).unwrap();
    let b3 = block(Era::Conway, EPOCH + 5, 3, tip_hash(&ls));
    ls.apply_block(&b3, BlockValidationMode::ApplyOnly).unwrap();
    assert_eq!(ls.era, Era::Conway);
}

/// GENESIS EXEMPTION. A genesis ledger has tip `Origin` and only a placeholder
/// era (`LedgerState::new` stamps Conway). Its first block fixes the era:
/// mainnet starts at Byron (a "downgrade" from the placeholder), and the local
/// devnet's first block is Conway inside epoch 0 (an "upgrade" outside an epoch
/// boundary relative to a Byron-era genesis ledger).
#[test]
fn a_genesis_ledger_takes_the_era_of_its_first_block() {
    for first_era in [
        Era::Byron,
        Era::Shelley,
        Era::Babbage,
        Era::Conway,
        Era::Dijkstra,
    ] {
        let mut ls = LedgerState::new(ProtocolParameters::mainnet_defaults());
        assert_eq!(ls.tip.point, dugite_primitives::block::Point::Origin);
        let first = block(first_era, 100, 1, Hash32::ZERO);
        assert!(ls.check_block_era(&first).is_ok(), "{first_era:?}");
        ls.apply_block(&first, BlockValidationMode::ApplyOnly)
            .unwrap_or_else(|e| panic!("first block {first_era:?} must apply: {e}"));
        assert_eq!(ls.era, first_era);
        assert_eq!(ls.epoch.0, 0, "the devnet case: still epoch 0");
    }
}

/// The exemption ends with the first block. A devnet whose genesis era is
/// Byron-by-placeholder and whose first block is Conway is then a Conway ledger
/// like any other: the same slot cannot be re-tagged afterwards.
#[test]
fn the_exemption_ends_after_the_first_block() {
    let ls = ledger_in(Era::Conway);
    for tag in [Era::Byron, Era::Shelley, Era::Babbage] {
        let bad = block(tag, 150, 2, tip_hash(&ls));
        assert!(
            ls.check_block_era(&bad).is_err(),
            "{tag:?} on a Conway ledger past its first block"
        );
    }
}

// ── The boundary rule: the era the epoch tick adopts ─────────────────────────

use dugite_ledger::state::FuturePParams;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::ProtocolParamUpdate;

/// Schedule protocol version `major` for the next boundary the way the ledger
/// learns of it: pre-Conway a quorum of genesis delegates voted (PPUP), Conway
/// onwards the governance `futurePParams`.
fn schedule_pv(ls: &mut LedgerState, major: u64) {
    if ls.era >= Era::Conway {
        let mut pp = ls.epochs.protocol_params.clone();
        pp.protocol_version_major = major;
        std::sync::Arc::make_mut(&mut ls.gov.governance).future_pparams =
            FuturePParams::DefinitePParamsUpdate(Box::new(pp));
    } else {
        ls.set_update_quorum(2);
        let ppu = ProtocolParamUpdate {
            protocol_version_major: Some(major),
            protocol_version_minor: Some(0),
            ..Default::default()
        };
        let epoch = ls.epoch;
        ls.epochs.pending_pp_updates.insert(
            epoch,
            vec![
                (Hash32::from_bytes([1; 32]), ppu.clone()),
                (Hash32::from_bytes([2; 32]), ppu),
            ],
        );
    }
}

fn ledger_at(era: Era, pv: u64) -> LedgerState {
    let mut ls = ledger_in(era);
    ls.epochs.protocol_params.protocol_version_major = pv;
    ls
}

/// Every hard fork mainnet took, from the real protocol-version values: the
/// ledger in the old era at the old PV, a quorum vote (or, in Conway, the
/// governance future pparams) for the new PV, and the first block of the next
/// epoch tagged with the new era must pass the STRICT check; the old tag and
/// every other tag must not.
#[test]
fn every_mainnet_hard_fork_passes_under_the_boundary_rule() {
    // (old era, old PV, new PV, new era)
    let forks = [
        (Era::Shelley, 2, 3, Era::Allegra),
        (Era::Allegra, 3, 4, Era::Mary),
        (Era::Mary, 4, 5, Era::Alonzo),
        (Era::Alonzo, 6, 7, Era::Babbage),
        (Era::Babbage, 8, 9, Era::Conway),
        (Era::Conway, 11, 12, Era::Dijkstra),
    ];
    for (old, old_pv, new_pv, new) in forks {
        let mut ls = ledger_at(old, old_pv);
        schedule_pv(&mut ls, new_pv);
        assert_eq!(
            ls.era_due_at_next_boundary(),
            new,
            "{old:?} PV{old_pv} -> PV{new_pv}"
        );
        let first = block(new, EPOCH + 100, 2, tip_hash(&ls));
        assert!(ls.check_block_era(&first).is_ok(), "{new:?} must pass");
        for wrong in [Era::Byron, old, Era::Dijkstra, Era::Shelley] {
            if wrong == new {
                continue;
            }
            let bad = block(wrong, EPOCH + 100, 2, tip_hash(&ls));
            assert!(
                ls.check_block_era(&bad).is_err(),
                "{wrong:?} on the first block after {old:?}->{new:?} must fail"
            );
        }
    }
}

/// The intra-era protocol-version bumps mainnet took (no new era) must NOT make
/// an upgrade due: Alonzo 5->6, Babbage 7->8, Conway 9->10 and 10->11.
#[test]
fn intra_era_pv_bumps_do_not_make_an_upgrade_due() {
    for (era, old_pv, new_pv) in [
        (Era::Alonzo, 5, 6),
        (Era::Babbage, 7, 8),
        (Era::Conway, 9, 10),
        (Era::Conway, 10, 11),
    ] {
        let mut ls = ledger_at(era, old_pv);
        schedule_pv(&mut ls, new_pv);
        assert_eq!(
            ls.era_due_at_next_boundary(),
            era,
            "{era:?} PV{old_pv}->{new_pv}"
        );
        assert!(ls
            .check_block_era(&block(era, EPOCH + 100, 2, tip_hash(&ls)))
            .is_ok());
        let up = match era {
            Era::Conway => Era::Dijkstra,
            Era::Babbage => Era::Conway,
            _ => Era::Babbage,
        };
        assert!(
            ls.check_block_era(&block(up, EPOCH + 100, 2, tip_hash(&ls)))
                .is_err(),
            "{up:?} on the first block of an epoch with no fork due"
        );
    }
}

/// Byron to Shelley is due at the network's `shelley_transition_epoch`
/// (mainnet 208), not before and not for any other era.
#[test]
fn byron_to_shelley_is_due_at_the_transition_epoch() {
    // A Byron ledger carries the Shelley genesis protocol version (2): the node
    // applies the Shelley genesis to the parameters at start.
    let mut ls = ledger_at(Era::Byron, 2);
    ls.shelley_transition_epoch = 1;
    let slot = EPOCH + 100;
    assert_eq!(ls.era_due_at_next_boundary(), Era::Shelley);
    assert!(ls
        .check_block_era(&block(Era::Shelley, slot, 2, tip_hash(&ls)))
        .is_ok());
    assert!(
        ls.check_block_era(&block(Era::Byron, slot, 2, tip_hash(&ls)))
            .is_err(),
        "a Byron block once Shelley is due"
    );
    assert!(
        ls.check_block_era(&block(Era::Babbage, slot, 2, tip_hash(&ls)))
            .is_err(),
        "the fork goes to Shelley, not past it"
    );

    // Transition epoch 208: the first block of epoch 1 is not a fork.
    let mut ls = ledger_at(Era::Byron, 2);
    ls.shelley_transition_epoch = 208;
    assert_eq!(ls.era_due_at_next_boundary(), Era::Byron);
    assert!(ls
        .check_block_era(&block(Era::Shelley, slot, 2, tip_hash(&ls)))
        .is_err());
    assert!(ls
        .check_block_era(&block(Era::Byron, slot, 2, tip_hash(&ls)))
        .is_ok());
}

/// `TestXHardForkAtEpoch` (cardano-node `TriggerHardForkAtEpoch`): due at the
/// configured epoch whatever the protocol version says; several forks at one
/// boundary resolve to the last.
#[test]
fn trigger_hard_forks_are_due_at_their_epoch() {
    let mut ls = ledger_at(Era::Alonzo, 5);
    ls.set_hard_fork_triggers([(Era::Babbage, 1), (Era::Conway, 1)].into_iter().collect());
    assert_eq!(ls.era_due_at_next_boundary(), Era::Conway);
    assert!(ls
        .check_block_era(&block(Era::Conway, EPOCH + 100, 2, tip_hash(&ls)))
        .is_ok());

    let mut ls = ledger_at(Era::Alonzo, 5);
    ls.set_hard_fork_triggers([(Era::Babbage, 2)].into_iter().collect());
    assert_eq!(ls.era_due_at_next_boundary(), Era::Alonzo, "epoch 2, not 1");
}

/// The wedge that motivated the rule: any upgrade on the first block of an
/// epoch used to pass. With no fork due it must not, and a due fork must be
/// taken.
#[test]
fn an_upgrade_with_no_fork_due_is_rejected_and_a_due_fork_must_be_taken() {
    let ls = ledger_at(Era::Conway, 10);
    let retagged = block(Era::Dijkstra, EPOCH + 100, 2, tip_hash(&ls));
    assert_wrong_era(
        ls.check_block_era(&retagged),
        Era::Dijkstra,
        Era::Conway,
        "epoch tick adopts",
    );

    let mut ls = ledger_at(Era::Babbage, 8);
    schedule_pv(&mut ls, 9);
    let stale = block(Era::Babbage, EPOCH + 100, 2, tip_hash(&ls));
    assert_wrong_era(
        ls.check_block_era(&stale),
        Era::Babbage,
        Era::Babbage,
        "epoch tick adopts",
    );
}

/// `ApplyOnly` replays our own validated blocks: a forecast error must never
/// halt a replay, so it accepts any upgrade on the first block of a new epoch
/// (downgrades are still rejected).
#[test]
fn apply_only_replay_does_not_enforce_the_due_era() {
    let mut ls = ledger_at(Era::Babbage, 8); // no fork scheduled
    let fork = block(Era::Conway, EPOCH + 100, 2, tip_hash(&ls));
    assert!(ls.check_block_era(&fork).is_err());
    ls.apply_block(&fork, BlockValidationMode::ApplyOnly)
        .expect("trusted replay accepts the upgrade on an epoch boundary");
    assert_eq!(ls.era, Era::Conway);

    let mut ls = ledger_at(Era::Conway, 10);
    let down = block(Era::Babbage, EPOCH + 100, 2, tip_hash(&ls));
    assert!(ls
        .apply_block(&down, BlockValidationMode::ApplyOnly)
        .is_err());
}

/// Guards the assumption `era_due_at_next_boundary` keys on: the ledger's epoch
/// is the epoch of its tip block.
#[test]
fn the_ledger_epoch_is_the_epoch_of_its_tip() {
    let ls = ledger_in(Era::Conway);
    assert_eq!(ls.epoch, EpochNo(0));
}

// ── More than one epoch ahead: no free pass ──────────────────────────────────

/// An empty epoch (a stalled low-density devnet, a node restarted after an idle
/// period) puts the next block two or more epochs ahead of the ledger tip. The
/// tick is deterministic from ledger state, so the same rule applies at the
/// block's own epoch: nothing passes just because the gap is large.
#[test]
fn a_block_several_epochs_ahead_is_held_to_the_due_era() {
    let far = 3 * EPOCH + 100; // three boundaries ahead

    // No fork due: only the ledger's own era passes, whatever the gap.
    let ls = ledger_at(Era::Conway, 10);
    assert!(ls
        .check_block_era(&block(Era::Conway, far, 2, tip_hash(&ls)))
        .is_ok());
    for wrong in [Era::Byron, Era::Babbage, Era::Dijkstra] {
        assert!(
            ls.check_block_era(&block(wrong, far, 2, tip_hash(&ls)))
                .is_err(),
            "{wrong:?} three epochs ahead with no fork due"
        );
    }

    // A fork voted for the next boundary is carried through the empty epochs.
    let mut ls = ledger_at(Era::Babbage, 8);
    schedule_pv(&mut ls, 9);
    assert_eq!(ls.era_due_at_epoch(3), Era::Conway);
    assert!(ls
        .check_block_era(&block(Era::Conway, far, 2, tip_hash(&ls)))
        .is_ok());
    for wrong in [Era::Babbage, Era::Dijkstra] {
        assert!(
            ls.check_block_era(&block(wrong, far, 2, tip_hash(&ls)))
                .is_err(),
            "{wrong:?} three epochs ahead with a fork to Conway due"
        );
    }

    // An epoch-keyed trigger that falls inside the gap is due; one beyond the
    // block's epoch is not.
    let mut ls = ledger_at(Era::Alonzo, 5);
    ls.set_hard_fork_triggers([(Era::Babbage, 2)].into_iter().collect());
    assert!(ls
        .check_block_era(&block(Era::Babbage, far, 2, tip_hash(&ls)))
        .is_ok());
    assert!(ls
        .check_block_era(&block(Era::Alonzo, far, 2, tip_hash(&ls)))
        .is_err());
    ls.set_hard_fork_triggers([(Era::Babbage, 9)].into_iter().collect());
    assert!(ls
        .check_block_era(&block(Era::Alonzo, far, 2, tip_hash(&ls)))
        .is_ok());
    assert!(ls
        .check_block_era(&block(Era::Babbage, far, 2, tip_hash(&ls)))
        .is_err());
}

// ── Config survives a ledger replacement ─────────────────────────────────────

/// Rollback via snapshot, fork-rollback replay and recovery all replace the
/// whole ledger with a state loaded from disk. The triggers are node config, not
/// part of the snapshot, so `adopt_snapshot_state` carries them over; a plain
/// `*ls = loaded` loses them and a legitimate `TestXHardForkAtEpoch` fork would
/// then be rejected.
#[test]
fn hard_fork_triggers_survive_a_snapshot_rollback_and_the_trigger_epoch_still_passes() {
    let mut ls = ledger_at(Era::Alonzo, 5);
    ls.set_hard_fork_triggers([(Era::Babbage, 1)].into_iter().collect());
    let fork = block(Era::Babbage, EPOCH + 100, 2, tip_hash(&ls));
    assert!(
        ls.check_block_era(&fork).is_ok(),
        "control: the fork is due"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollback.snapshot");
    ls.save_snapshot(&path).expect("save snapshot");

    // What every reload used to do: `*ls = loaded`.
    let loaded = LedgerState::load_snapshot(&path).expect("load snapshot");
    assert!(
        loaded.hard_fork_triggers.is_empty(),
        "triggers are config, not snapshot content"
    );
    assert!(
        loaded.check_block_era(&fork).is_err(),
        "without the triggers the legitimate fork would be rejected (the halt)"
    );

    // What the node does now.
    ls.adopt_snapshot_state(loaded);
    assert_eq!(ls.hard_fork_triggers.get(&Era::Babbage), Some(&1));
    assert!(
        ls.check_block_era(&fork).is_ok(),
        "after the rollback the trigger epoch still passes"
    );
    assert!(ls
        .check_block_era(&block(Era::Alonzo, EPOCH + 100, 2, tip_hash(&ls)))
        .is_err());
}
