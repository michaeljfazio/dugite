//! Unit tests for PPUP (pre-Conway protocol-parameter update) predicates.
//!
//! Each predicate has at least one positive (accept) and one negative
//! (reject) test, plus the lenient-default (skipped-when-context-missing)
//! and Conway-no-op cases.
//!
//! Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.

use std::collections::{BTreeMap, HashSet};

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{ProtocolParamUpdate, UpdateProposal};

use super::*;
use crate::validation::{ValidationContext, ValidationError, VotingPeriod};

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Build a `ProtocolParameters` instance with a specific protocol version
/// and the mainnet active-slot coefficient.
fn params_with_pv(major: u64, minor: u64) -> ProtocolParameters {
    let mut p = ProtocolParameters::mainnet_defaults();
    p.protocol_version_major = major;
    p.protocol_version_minor = minor;
    p.active_slots_coeff = 0.05; // f = 1/20
    p
}

/// Empty PPU — does not propose any specific change (used to isolate the
/// non-PV / non-key checks from `PVCannotFollowPPUP`).
fn empty_ppu() -> ProtocolParamUpdate {
    ProtocolParamUpdate {
        min_fee_a: None,
        min_fee_b: None,
        max_block_body_size: None,
        max_tx_size: None,
        max_block_header_size: None,
        key_deposit: None,
        pool_deposit: None,
        e_max: None,
        n_opt: None,
        a0: None,
        rho: None,
        tau: None,
        min_pool_cost: None,
        ada_per_utxo_byte: None,
        cost_models: None,
        execution_costs: None,
        max_tx_ex_units: None,
        max_block_ex_units: None,
        max_val_size: None,
        collateral_percentage: None,
        max_collateral_inputs: None,
        min_fee_ref_script_cost_per_byte: None,
        d: None,
        extra_entropy: None,
        protocol_version_major: None,
        protocol_version_minor: None,
        drep_deposit: None,
        gov_action_deposit: None,
        gov_action_lifetime: None,
        dvt_pp_network_group: None,
        dvt_pp_economic_group: None,
        dvt_pp_technical_group: None,
        dvt_pp_gov_group: None,
        dvt_hard_fork: None,
        dvt_no_confidence: None,
        dvt_committee_normal: None,
        dvt_committee_no_confidence: None,
        dvt_constitution: None,
        dvt_treasury_withdrawal: None,
        pvt_motion_no_confidence: None,
        pvt_committee_normal: None,
        pvt_committee_no_confidence: None,
        pvt_hard_fork: None,
        pvt_pp_security_group: None,
        min_committee_size: None,
        committee_term_limit: None,
        drep_activity: None,
        // Dijkstra-era fields (keys 34-37)
        max_ref_script_size_per_block: None,
        max_ref_script_size_per_tx: None,
        ref_script_cost_stride: None,
        ref_script_cost_multiplier: None,
    }
}

/// Build a 28-byte hash from a low-byte-only literal.
fn h28(low: u8) -> Hash28 {
    let mut bytes = [0u8; 28];
    bytes[27] = low;
    Hash28::from_bytes(bytes)
}

/// Pad a `Hash28` into a `Hash32` (mirrors the wire-decode path used by
/// `dugite-serialization::multi_era::convert_update_proposal`).
fn h28_padded(h: Hash28) -> Hash32 {
    h.to_hash32_padded()
}

/// Build a `ValidationContext` with mainnet-style epoch geometry (no
/// genesis-delegate set by default).
fn ctx_geom() -> ValidationContext {
    ValidationContext::default().with_epoch_geometry(432_000, 432) // preview-style k=432
}

// ---------------------------------------------------------------------------
// NonGenesisUpdatePPUP
// ---------------------------------------------------------------------------

#[test]
fn test_non_genesis_update_ppup_rejected_unknown_key() {
    let params = params_with_pv(7, 0); // Babbage
    let known = h28(0xAA);
    let unknown = h28(0xBB);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([known]));

    // Use the at-this-epoch path so PPUpdateWrongEpoch doesn't also fire:
    // current_slot=0 → current_epoch=0 → ForThisEpoch → expect target=0.
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(unknown), empty_ppu())],
        epoch: 0,
    };
    let errors = validate_ppup(Some(&update), &params, 0, &ctx).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::NonGenesisUpdatePPUP { .. })),
        "expected NonGenesisUpdatePPUP; got: {errors:?}"
    );
}

#[test]
fn test_non_genesis_update_ppup_only_genesis_keys_passes() {
    let params = params_with_pv(7, 0);
    let g1 = h28(0x01);
    let g2 = h28(0x02);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g1, g2]));

    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g1), empty_ppu()), (h28_padded(g2), empty_ppu())],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

#[test]
fn test_non_genesis_update_ppup_skipped_when_no_genesis_delegates() {
    let params = params_with_pv(7, 0);
    let ctx = ctx_geom();
    // No genesis_delegates on the context → predicate is silently skipped.
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(h28(0xFF)), empty_ppu())],
        epoch: 0,
    };
    let result = validate_ppup(Some(&update), &params, 0, &ctx);
    // The NonGenesisUpdatePPUP predicate must NOT contribute an error.
    if let Err(errors) = result {
        assert!(
            errors
                .iter()
                .all(|e| !matches!(e, ValidationError::NonGenesisUpdatePPUP { .. })),
            "expected NonGenesisUpdatePPUP to be skipped; got: {errors:?}"
        );
    }
}

#[test]
fn test_non_genesis_update_ppup_skipped_in_conway() {
    // PV >= 9 → entire PPUP rule is a no-op.
    let params = params_with_pv(9, 0);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([h28(0x01)]));
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(h28(0xFF)), empty_ppu())],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// PPUpdateWrongEpoch
// ---------------------------------------------------------------------------

#[test]
fn test_pp_update_wrong_epoch_for_this_epoch() {
    // params: pv=7 (Babbage), f=1/20; ctx: k=432, epoch_length=432_000.
    //   stability_window = ceil(3*432*20) = 25_920.
    //   tooLate = 432_000 - 2 * 25_920 = 380_160.
    //   current_slot=0 → ForThisEpoch → target must equal current_epoch=0.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 5, // wrong: must be 0 in the for-this-epoch period
    };
    let errors = validate_ppup(Some(&update), &params, 0, &ctx).unwrap_err();
    let pp_err = errors
        .iter()
        .find(|e| matches!(e, ValidationError::PPUpdateWrongEpoch { .. }))
        .expect("expected PPUpdateWrongEpoch");
    if let ValidationError::PPUpdateWrongEpoch { period, target, .. } = pp_err {
        assert_eq!(*period, VotingPeriod::ForThisEpoch);
        assert_eq!(*target, 5);
    }
}

#[test]
fn test_pp_update_wrong_epoch_for_next_epoch() {
    // current_slot=400_000 >= tooLate=380_160 → ForNextEpoch → target=1.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 5, // wrong: must be 1 (current_epoch+1)
    };
    let errors = validate_ppup(Some(&update), &params, 400_000, &ctx).unwrap_err();
    let pp_err = errors
        .iter()
        .find(|e| matches!(e, ValidationError::PPUpdateWrongEpoch { .. }))
        .expect("expected PPUpdateWrongEpoch");
    if let ValidationError::PPUpdateWrongEpoch { period, .. } = pp_err {
        assert_eq!(*period, VotingPeriod::ForNextEpoch);
    }
}

#[test]
fn test_pp_update_correct_epoch_passes() {
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    // For-this-epoch happy path: current_slot=0, target=0.
    let update_this = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update_this), &params, 0, &ctx).is_ok());

    // For-next-epoch happy path: current_slot=400_000, target=1.
    let update_next = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 1,
    };
    assert!(validate_ppup(Some(&update_next), &params, 400_000, &ctx).is_ok());
}

#[test]
fn test_pp_update_wrong_epoch_skipped_when_geometry_missing() {
    // No `with_epoch_geometry` → predicate is silently skipped.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ValidationContext::default().with_genesis_delegates(HashSet::from([g]));

    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 9999, // would otherwise fail PPUpdateWrongEpoch
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// PVCannotFollowPPUP
// ---------------------------------------------------------------------------

#[test]
fn test_pv_cannot_follow_skip_major() {
    // current=(7, 0), proposed=(9, 0) — skips a major version.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let mut ppu = empty_ppu();
    ppu.protocol_version_major = Some(9);
    ppu.protocol_version_minor = Some(0);
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), ppu)],
        epoch: 0,
    };
    let errors = validate_ppup(Some(&update), &params, 0, &ctx).unwrap_err();
    let pv_err = errors
        .iter()
        .find(|e| matches!(e, ValidationError::PVCannotFollowPPUP { .. }))
        .expect("expected PVCannotFollowPPUP");
    if let ValidationError::PVCannotFollowPPUP { bad_pv } = pv_err {
        assert_eq!(*bad_pv, (9, 0));
    }
}

#[test]
fn test_pv_cannot_follow_skip_minor() {
    // current=(7, 0), proposed=(7, 2) — skips minor.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let mut ppu = empty_ppu();
    ppu.protocol_version_major = Some(7);
    ppu.protocol_version_minor = Some(2);
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), ppu)],
        epoch: 0,
    };
    let errors = validate_ppup(Some(&update), &params, 0, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::PVCannotFollowPPUP { bad_pv: (7, 2) })));
}

#[test]
fn test_pv_cannot_follow_minor_bump_passes() {
    // current=(7, 0), proposed=(7, 1) — valid minor bump.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let mut ppu = empty_ppu();
    ppu.protocol_version_major = Some(7);
    ppu.protocol_version_minor = Some(1);
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), ppu)],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

#[test]
fn test_pv_cannot_follow_major_bump_passes() {
    // current=(7, 5), proposed=(8, 0) — valid major bump (resets minor).
    let params = params_with_pv(7, 5);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let mut ppu = empty_ppu();
    ppu.protocol_version_major = Some(8);
    ppu.protocol_version_minor = Some(0);
    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), ppu)],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

#[test]
fn test_pv_cannot_follow_no_pv_proposed_passes() {
    // PPU does not touch protocol version → predicate doesn't fire.
    let params = params_with_pv(7, 0);
    let g = h28(0x01);
    let ctx = ctx_geom().with_genesis_delegates(HashSet::from([g]));

    let update = UpdateProposal {
        proposed_updates: vec![(h28_padded(g), empty_ppu())],
        epoch: 0,
    };
    assert!(validate_ppup(Some(&update), &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// `voted_future_pparams` (quorum / enactment helper — not an error path)
// ---------------------------------------------------------------------------

#[test]
fn test_voted_future_pparams_quorum_met() {
    let mut params = ProtocolParameters::mainnet_defaults();
    // Make sure the structural-sanity check passes for the empty PPU.
    params.max_tx_size = 16_384;
    params.max_block_header_size = 1_100;
    params.max_block_body_size = 90_112;

    // 5 genesis delegates, all voting for the same empty PPU; quorum=5 → met.
    let mut proposed: BTreeMap<Hash28, ProtocolParamUpdate> = BTreeMap::new();
    for i in 1..=5u8 {
        proposed.insert(h28(i), empty_ppu());
    }
    let result = voted_future_pparams(&proposed, 5, &params);
    assert!(result.is_some());
}

#[test]
fn test_voted_future_pparams_no_quorum() {
    let params = ProtocolParameters::mainnet_defaults();
    // 5 delegates split: 3 vote for ppu_a, 2 for ppu_b; quorum=4 → none met.
    let mut ppu_a = empty_ppu();
    ppu_a.min_fee_a = Some(44);
    let mut ppu_b = empty_ppu();
    ppu_b.min_fee_a = Some(45);

    let mut proposed: BTreeMap<Hash28, ProtocolParamUpdate> = BTreeMap::new();
    proposed.insert(h28(1), ppu_a.clone());
    proposed.insert(h28(2), ppu_a.clone());
    proposed.insert(h28(3), ppu_a);
    proposed.insert(h28(4), ppu_b.clone());
    proposed.insert(h28(5), ppu_b);

    assert!(voted_future_pparams(&proposed, 4, &params).is_none());
}

#[test]
fn test_voted_future_pparams_silent_discard_on_invalid_size_constraint() {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.max_tx_size = 100;
    params.max_block_header_size = 50;
    params.max_block_body_size = 200;

    // Quorum-met but the merged PPU breaks
    // `max_tx_size + max_block_header_size < max_block_body_size`:
    // 90 + 130 = 220 >= 200 → silently discarded.
    let mut bad_ppu = empty_ppu();
    bad_ppu.max_tx_size = Some(90);
    bad_ppu.max_block_header_size = Some(130);

    let mut proposed: BTreeMap<Hash28, ProtocolParamUpdate> = BTreeMap::new();
    for i in 1..=5u8 {
        proposed.insert(h28(i), bad_ppu.clone());
    }
    assert!(voted_future_pparams(&proposed, 5, &params).is_none());
}

#[test]
fn test_voted_future_pparams_tie_returns_none() {
    let params = ProtocolParameters::mainnet_defaults();
    let mut ppu_a = empty_ppu();
    ppu_a.min_fee_a = Some(44);
    let mut ppu_b = empty_ppu();
    ppu_b.min_fee_a = Some(45);

    // 2 votes each for two distinct PPUs; quorum=2 → tied → None.
    let mut proposed: BTreeMap<Hash28, ProtocolParamUpdate> = BTreeMap::new();
    proposed.insert(h28(1), ppu_a.clone());
    proposed.insert(h28(2), ppu_a);
    proposed.insert(h28(3), ppu_b.clone());
    proposed.insert(h28(4), ppu_b);

    assert!(voted_future_pparams(&proposed, 2, &params).is_none());
}
