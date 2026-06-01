//! Unit tests for MIR validation predicates.
//!
//! Each predicate has at least one positive (accept) and one negative
//! (reject) test.  Integration tests via `validate_transaction_with_context`
//! live in `validation/tests.rs`.

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::Hash28;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{Certificate, MIRSource, MIRTarget};
use dugite_primitives::value::Lovelace;

use super::*;
use crate::validation::ValidationContext;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Build a `ProtocolParameters` instance with a specific protocol version.
fn params_with_pv(pv: u64) -> ProtocolParameters {
    let mut p = ProtocolParameters::mainnet_defaults();
    p.protocol_version_major = pv;
    p.active_slots_coeff = 0.05; // f = 1/20 (mainnet)
    p
}

/// Build a `Credential::VerificationKey` from a 28-byte literal — convenient
/// shorthand for tests.
fn cred(bytes: [u8; 28]) -> Credential {
    Credential::VerificationKey(Hash28::from_bytes(bytes))
}

/// Build a distribute-to-stake-credentials MIR certificate.
fn mir_cert_distribute(source: MIRSource, deltas: Vec<(Credential, i64)>) -> Certificate {
    Certificate::MoveInstantaneousRewards {
        source,
        target: MIRTarget::StakeCredentials(deltas),
    }
}

/// Build a pot-to-pot transfer MIR certificate.
fn mir_cert_transfer(source: MIRSource, coin: u64) -> Certificate {
    Certificate::MoveInstantaneousRewards {
        source,
        target: MIRTarget::OtherAccountingPot(coin),
    }
}

/// Default lenient test context — no MIR-specific state plumbed.
fn ctx_lenient() -> ValidationContext {
    ValidationContext::default()
}

/// MIR-aware test context with mainnet epoch geometry and a generous
/// pot balance.
fn ctx_full(treasury: u64, reserves: u64) -> ValidationContext {
    ValidationContext::default()
        .with_pots(Lovelace(treasury), Lovelace(reserves))
        .with_epoch_geometry(432_000, 432) // preview-style k=432
}

// ---------------------------------------------------------------------------
// MIRCertificateTooLateInEpoch
// ---------------------------------------------------------------------------

#[test]
fn test_mir_too_late_in_epoch() {
    // params: pv=5 (Alonzo), f=1/20; ctx: k=432, epoch_length=432_000.
    // stability_window = ceil(3*432*20 / 1) = 25_920.
    // current_epoch=0, first_slot_next_epoch=432_000, deadline=432_000-25_920=406_080.
    // current_slot=406_080 (== deadline) → reject (boundary inclusive).
    let params = params_with_pv(5);
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![]);
    let errors = validate_mir_cert(&cert, &params, 406_080, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::MIRCertificateTooLateInEpoch { .. })));
}

#[test]
fn test_mir_in_time() {
    let params = params_with_pv(5);
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![]);
    // current_slot well before the deadline → accept.
    assert!(validate_mir_cert(&cert, &params, 100_000, &ctx).is_ok());
}

#[test]
fn test_mir_too_late_skipped_when_geometry_missing() {
    // No epoch_length / security_param on the context → predicate skipped.
    let params = params_with_pv(5);
    let ctx = ValidationContext::default().with_pots(Lovelace(1_000_000), Lovelace(1_000_000));
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![]);
    assert!(validate_mir_cert(&cert, &params, 999_999_999, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// InsufficientForInstantaneousRewards
// ---------------------------------------------------------------------------

#[test]
fn test_mir_insufficient_for_instantaneous_rewards() {
    let params = params_with_pv(5);
    // pot has 100, request 1000.
    let ctx = ctx_full(1_000_000, 100);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), 1000)]);
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InsufficientForInstantaneousRewards { .. }
    )));
}

#[test]
fn test_mir_sufficient_for_instantaneous_rewards() {
    let params = params_with_pv(5);
    // pot has 100, request 50 → ok.
    let ctx = ctx_full(1_000_000, 100);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), 50)]);
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

#[test]
fn test_mir_insufficient_skipped_without_pot_balance() {
    // No pots on the context → InsufficientForInstantaneousRewards must
    // not fire even when sum(deltas) is huge.
    let params = params_with_pv(5);
    let ctx = ctx_lenient();
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), i64::MAX / 2)]);
    let result = validate_mir_cert(&cert, &params, 0, &ctx);
    if let Err(ref errs) = result {
        assert!(
            !errs.iter().any(|e| matches!(
                e,
                ValidationError::InsufficientForInstantaneousRewards { .. }
            )),
            "predicate should be skipped without pot balance"
        );
    }
}

// ---------------------------------------------------------------------------
// MIRTransferNotCurrentlyAllowed (pre-Alonzo)
// ---------------------------------------------------------------------------

#[test]
fn test_mir_transfer_pre_alonzo_disallowed() {
    let params = params_with_pv(4); // Mary
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_transfer(MIRSource::Reserves, 100);
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::MIRTransferNotCurrentlyAllowed)));
}

#[test]
fn test_mir_transfer_alonzo_allowed() {
    let params = params_with_pv(5); // Alonzo
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_transfer(MIRSource::Reserves, 100);
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// MIRNegativesNotCurrentlyAllowed (pre-Alonzo)
// ---------------------------------------------------------------------------

#[test]
fn test_mir_negatives_pre_alonzo_rejected() {
    let params = params_with_pv(4); // Mary
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), -100)]);
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::MIRNegativesNotCurrentlyAllowed)));
}

#[test]
fn test_mir_negatives_alonzo_allowed() {
    let params = params_with_pv(5); // Alonzo
                                    // No accumulated_mir_balances → MIRProducesNegativeUpdate skipped.
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), -100)]);
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// MIRProducesNegativeUpdate (Alonzo+)
// ---------------------------------------------------------------------------

#[test]
fn test_mir_produces_negative_update_alonzo() {
    let params = params_with_pv(5); // Alonzo
    let credential = cred([7u8; 28]);
    let key = credential.to_hash().to_hash32_padded();
    let mut accumulated = std::collections::HashMap::new();
    accumulated.insert(key, 50i64); // recipient already has 50 accumulated

    let ctx = ctx_full(1_000_000, 1_000_000).with_accumulated_mir_balances(accumulated);

    // delta = -100 → 50 + (-100) = -50 → reject.
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(credential, -100)]);
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::MIRProducesNegativeUpdate { .. })));
}

#[test]
fn test_mir_negative_delta_does_not_produce_negative_update() {
    let params = params_with_pv(5); // Alonzo
    let credential = cred([7u8; 28]);
    let key = credential.to_hash().to_hash32_padded();
    let mut accumulated = std::collections::HashMap::new();
    accumulated.insert(key, 200i64); // recipient has 200 accumulated

    let ctx = ctx_full(1_000_000, 1_000_000).with_accumulated_mir_balances(accumulated);

    // delta = -100 → 200 + (-100) = 100 → ok.
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(credential, -100)]);
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// InsufficientForTransferDELEG (Alonzo+)
// ---------------------------------------------------------------------------

#[test]
fn test_mir_insufficient_for_transfer_deleg_alonzo() {
    let params = params_with_pv(5); // Alonzo
    let ctx = ctx_full(1_000_000, 100); // reserves = 100
    let cert = mir_cert_transfer(MIRSource::Reserves, 1000); // request 1000
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::InsufficientForTransferDELEG { .. })));
}

#[test]
fn test_mir_sufficient_for_transfer_deleg_alonzo() {
    let params = params_with_pv(5);
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = mir_cert_transfer(MIRSource::Reserves, 1000);
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// MIRNegativeTransfer (Alonzo+) — unreachable via the typed surface
// ---------------------------------------------------------------------------

#[test]
fn test_mir_negative_transfer_unreachable_via_u64() {
    // `OtherAccountingPot(u64)` cannot be negative, so this predicate is
    // unreachable through the public type system.  The variant exists for
    // parity with Haskell's `DeltaCoin`-typed payload.  We exercise the
    // structural error variant directly to ensure it remains constructible.
    let err = ValidationError::MIRNegativeTransfer {
        pot: MIRSource::Reserves,
        amount: -1,
    };
    assert!(format!("{err}").contains("MIRNegativeTransfer"));
}

// ---------------------------------------------------------------------------
// Conway short-circuit — no MIR errors at PV >= 9
// ---------------------------------------------------------------------------

#[test]
fn test_mir_conway_no_op() {
    let params = params_with_pv(9); // Conway
                                    // Build a clearly-invalid MIR cert (insufficient pot, too late, etc.)
    let ctx = ctx_full(0, 0);
    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(cred([1u8; 28]), 1_000_000)]);
    // PV 9: Conway has no MIR — the predicate must short-circuit Ok.
    assert!(validate_mir_cert(&cert, &params, 9_999_999, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// stability_window helper
// ---------------------------------------------------------------------------

#[test]
fn test_stability_window_mainnet() {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.active_slots_coeff = 0.05; // f = 1/20
                                      // k=2160 → ceil(3*2160*20/1) = 129_600.
    let sw = compute_stability_window(&params, Some(2160)).expect("must compute");
    assert_eq!(sw, 129_600);
}

#[test]
fn test_stability_window_preview_k_432() {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.active_slots_coeff = 0.05;
    let sw = compute_stability_window(&params, Some(432)).expect("must compute");
    assert_eq!(sw, 25_920);
}

#[test]
fn test_stability_window_none_when_k_missing() {
    let params = ProtocolParameters::mainnet_defaults();
    assert!(compute_stability_window(&params, None).is_none());
}

// ---------------------------------------------------------------------------
// Non-MIR cert short-circuit
// ---------------------------------------------------------------------------

#[test]
fn test_validate_mir_cert_non_mir_is_ok() {
    let params = params_with_pv(5);
    let ctx = ctx_full(1_000_000, 1_000_000);
    let cert = Certificate::StakeRegistration(cred([1u8; 28]));
    assert!(validate_mir_cert(&cert, &params, 0, &ctx).is_ok());
}

// ---------------------------------------------------------------------------
// with_accumulated_mir_balances_from_ledger — populates from LedgerState
// ---------------------------------------------------------------------------

/// Build a minimal `LedgerState` for accumulator-helper tests.  The PV and
/// the reward_accounts map are the only fields that matter for the helper.
fn ledger_for_accumulator_test(
    pv: u64,
    reward_accounts: Vec<(dugite_primitives::hash::Hash32, u64)>,
) -> crate::state::LedgerState {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = pv;
    let mut state = crate::state::LedgerState::new(params);
    let map: imbl::HashMap<dugite_primitives::hash::Hash32, Lovelace> = reward_accounts
        .into_iter()
        .map(|(k, v)| (k, Lovelace(v)))
        .collect();
    state.certs.reward_accounts = map;
    state
}

#[test]
fn test_with_accumulated_mir_balances_from_ledger_populates_field() {
    let credential = cred([1u8; 28]);
    let key = credential.to_hash().to_hash32_padded();
    let ledger = ledger_for_accumulator_test(5, vec![(key, 50_000)]);

    let ctx = ValidationContext::new().with_accumulated_mir_balances_from_ledger(&ledger);
    let map = ctx
        .accumulated_mir_balances
        .as_ref()
        .expect("accumulator must be populated");
    assert_eq!(map.len(), 1);
    assert_eq!(*map.get(&key).expect("cred must be in map"), 50_000i64);
}

#[test]
fn test_with_accumulated_mir_balances_in_conway_returns_empty() {
    // Even with a non-empty reward_accounts map, Conway+ short-circuits to
    // an empty accumulator (MIR was removed at the Conway era boundary).
    let credential = cred([1u8; 28]);
    let key = credential.to_hash().to_hash32_padded();
    let ledger = ledger_for_accumulator_test(9, vec![(key, 50_000)]);

    let ctx = ValidationContext::new().with_accumulated_mir_balances_from_ledger(&ledger);
    let map = ctx
        .accumulated_mir_balances
        .as_ref()
        .expect("accumulator must be set (Some)");
    assert!(map.is_empty(), "Conway accumulator must be empty");
}

#[test]
fn test_mir_produces_negative_update_fires_with_populated_accumulator() {
    // Set up: cred has 50_000 in reward_accounts, MIR cert applies a
    // -100_000 delta.  After populating the accumulator from the ledger,
    // MIRProducesNegativeUpdate must fire (50_000 + (-100_000) = -50_000).
    let params = params_with_pv(5); // Alonzo
    let credential = cred([1u8; 28]);
    let key = credential.to_hash().to_hash32_padded();
    let ledger = ledger_for_accumulator_test(5, vec![(key, 50_000)]);

    let ctx = ValidationContext::default()
        .with_pots(Lovelace(1_000_000), Lovelace(1_000_000))
        .with_epoch_geometry(432_000, 432)
        .with_accumulated_mir_balances_from_ledger(&ledger);

    let cert = mir_cert_distribute(MIRSource::Reserves, vec![(credential, -100_000)]);
    let errors = validate_mir_cert(&cert, &params, 0, &ctx).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::MIRProducesNegativeUpdate { .. })),
        "expected MIRProducesNegativeUpdate, got {errors:?}"
    );
}
