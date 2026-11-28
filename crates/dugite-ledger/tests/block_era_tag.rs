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
