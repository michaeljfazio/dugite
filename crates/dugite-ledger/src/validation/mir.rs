//! MIR (Move Instantaneous Rewards) validation rules.
//!
//! Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
//!
//! MIR certificates exist only in Shelley–Babbage (`AtMostEra "Babbage"`).
//! Conway has removed `MIRCert` entirely — at PV >= 9 every MIR predicate is
//! a no-op (`Ok(())`).  Several sub-rules are further gated by Alonzo's
//! `hardforkAlonzoAllowMIRTransfer = pvMajor pv > 4` switch:
//!
//! | Predicate                            | Era gate                         |
//! |--------------------------------------|----------------------------------|
//! | `MIRCertificateTooLateInEpoch`       | All MIR eras (Shelley–Babbage)   |
//! | `InsufficientForInstantaneousRewards`| All MIR eras                     |
//! | `MIRTransferNotCurrentlyAllowed`     | Shelley–Mary only (PV ≤ 4)       |
//! | `MIRNegativesNotCurrentlyAllowed`    | Shelley–Mary only (PV ≤ 4)       |
//! | `MIRProducesNegativeUpdate`          | Alonzo–Babbage only (PV >= 5)    |
//! | `InsufficientForTransferDELEG`       | Alonzo–Babbage only (PV >= 5)    |
//! | `MIRNegativeTransfer`                | Alonzo–Babbage only (PV >= 5)    |
//!
//! ## Known partial limitations
//!
//! Mainnet has been Conway (PV ≥ 9) since September 2024 and MIR certs were
//! removed at the era boundary, so the limitations below have **zero impact
//! on live mainnet tx flow** (`validate_mir_cert` short-circuits `Ok(())`
//! for `pv >= 9`).  They matter only for pre-Conway replay correctness and
//! for tier-0 test fidelity that exercises MIR predicates directly.
//!
//! 1. `MIRProducesNegativeUpdate` requires the per-credential accumulated
//!    MIR rewards snapshot (Haskell `dsIRewards`).  When the caller does not
//!    supply `ValidationContext::accumulated_mir_balances`, the predicate
//!    is silently skipped.  Tests and replay tooling can populate the field
//!    via [`ValidationContext::with_accumulated_mir_balances`] (raw map) or
//!    [`ValidationContext::with_accumulated_mir_balances_from_ledger`]
//!    (snapshot from a `LedgerState`).  The latter is a *bounded-fidelity*
//!    approximation: dugite credits MIR distributions immediately to
//!    `reward_accounts` (no separate pending-delta map), so the snapshot is
//!    the post-distribution view — sufficient for catching obvious negative
//!    updates but not byte-for-byte equivalent to Haskell `dsIRewards`.
//!
//! 2. `InsufficientForInstantaneousRewards` in Alonzo+ uses Haskell's
//!    `availableAfterMIR` semantics (existing balance + deltas).  Dugite
//!    falls back to the simpler `sum(deltas) > pot_balance` check; this
//!    matches the spirit of the Haskell predicate but may accept some
//!    edge-case rejections that Haskell would reject when negative-delta
//!    claw-backs interact with `dsIRewards`.

use std::collections::HashMap;

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{blake2b_224, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{Certificate, MIRSource, MIRTarget, Transaction};

use super::{ValidationContext, ValidationError};

/// Validate a single MIR certificate against the supplied context.
///
/// `params` and `current_slot` are passed in directly (matching the rest of
/// the validation surface), while pot balances, epoch geometry, and the
/// accumulated-MIR snapshot come from `ValidationContext`.
///
/// Returns `Ok(())` when:
/// - the certificate is not an MIR certificate (no-op);
/// - the protocol version is Conway+ (`>= 9`) — Conway has no MIR;
/// - all 7 predicates pass.
///
/// Returns `Err(errors)` aggregating every predicate failure for this cert.
pub fn validate_mir_cert(
    cert: &Certificate,
    params: &ProtocolParameters,
    current_slot: u64,
    ctx: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    // Conway and onward: MIR is no longer part of the era. No-op.
    if params.protocol_version_major >= 9 {
        return Ok(());
    }
    let Certificate::MoveInstantaneousRewards { source, target } = cert else {
        return Ok(());
    };

    let mut errors = Vec::new();
    check_slot_not_too_late(params, current_slot, ctx, &mut errors);
    match target {
        MIRTarget::StakeCredentials(deltas) => {
            check_stake_addresses_mir(source, deltas, params, ctx, &mut errors);
        }
        MIRTarget::OtherAccountingPot(coin) => {
            check_send_to_opposite_pot_mir(source, *coin, params, ctx, &mut errors);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// `MIRCertificateTooLateInEpoch` — all MIR eras
// ---------------------------------------------------------------------------

/// Reject MIR certs submitted within `stabilityWindow` slots of the next
/// epoch boundary.
///
/// Reference: Haskell `checkSlotNotTooLate` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`:
/// ```text
/// tooLate = firstSlotOfNextEpoch - stabilityWindow
/// stabilityWindow = ceil(3 * k / f)
/// reject when currentSlot >= tooLate
/// ```
///
/// Silently skipped (lenient) when `epoch_length` or `security_param` are
/// not supplied on the context — the predicate cannot fire without them.
pub(crate) fn check_slot_not_too_late(
    params: &ProtocolParameters,
    current_slot: u64,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let Some(epoch_length) = ctx.epoch_length else {
        return;
    };
    let Some(stability_window) = compute_stability_window(params, ctx.security_param) else {
        return;
    };

    // `current_epoch * epoch_length` gives the first slot of the current
    // epoch, so the next-epoch boundary is `(current_epoch + 1) *
    // epoch_length`.  When `current_epoch` is not supplied we fall back to
    // dividing the slot, which is identical for any chain that has not been
    // hard-forked through a different epoch length.
    let current_epoch = ctx
        .current_epoch
        .unwrap_or_else(|| current_slot / epoch_length);
    let first_slot_next_epoch = current_epoch.saturating_add(1).saturating_mul(epoch_length);
    let too_late = first_slot_next_epoch.saturating_sub(stability_window);
    if current_slot >= too_late {
        errors.push(ValidationError::MIRCertificateTooLateInEpoch {
            current_slot,
            deadline: too_late,
        });
    }
}

// ---------------------------------------------------------------------------
// `checkStakeAddressesMIR` — distribute branch
// ---------------------------------------------------------------------------

/// Validate the `StakeCredentials` distribution branch of an MIR certificate.
///
/// Predicate breakdown:
/// - Pre-Alonzo (`pv <= 4`): all deltas must be non-negative
///   (`MIRNegativesNotCurrentlyAllowed`).
/// - Always: `sum(deltas) <= pot_balance` (`InsufficientForInstantaneousRewards`).
/// - Alonzo+ (`pv >= 5`): for any cred whose `delta + accumulated < 0`,
///   raise `MIRProducesNegativeUpdate`.  Skipped silently when
///   `accumulated_mir_balances` is `None`.  Tests and pre-Conway replay can
///   populate the accumulator via
///   [`ValidationContext::with_accumulated_mir_balances_from_ledger`] (a
///   bounded-fidelity snapshot of `LedgerState.certs.reward_accounts`).
///
/// Reference: Haskell `checkStakeAddressesMIR` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
pub(crate) fn check_stake_addresses_mir(
    source: &MIRSource,
    deltas: &[(Credential, i64)],
    params: &ProtocolParameters,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let pv = params.protocol_version_major;
    let alonzo_or_later = pv > 4;

    // ---------- MIRNegativesNotCurrentlyAllowed (pre-Alonzo) -----------
    if !alonzo_or_later && deltas.iter().any(|(_, v)| *v < 0) {
        errors.push(ValidationError::MIRNegativesNotCurrentlyAllowed);
        // Haskell short-circuits here (predicate ordering) — emit the one
        // pre-Alonzo error and skip the further checks for this cert.
        return;
    }

    // ---------- InsufficientForInstantaneousRewards --------------------
    if let Some(available) = pot_balance(source, ctx) {
        // Haskell's pre-Alonzo branch uses `sum (filter (>0) deltas)` (no
        // negatives are possible since the prior check rejects them); the
        // Alonzo+ branch uses `sum deltas` against `availableAfterMIR =
        // pot - sum existing iRewards`.  Without `dsIRewards` we use the
        // simpler `sum deltas vs pot` form for both branches — documented
        // limitation.
        let required: i128 = deltas.iter().map(|(_, v)| *v as i128).sum();
        if required > available as i128 {
            errors.push(ValidationError::InsufficientForInstantaneousRewards {
                pot: source.clone(),
                required: required.max(0) as u64,
                available,
            });
        }
    }

    // ---------- MIRProducesNegativeUpdate (Alonzo+) --------------------
    if alonzo_or_later {
        if let Some(accumulated) = ctx.accumulated_mir_balances.as_ref() {
            // Aggregate per-credential deltas in case the same credential
            // appears more than once in the cert (Haskell uses a Map and
            // sums implicitly; the wire format permits duplicates).
            let mut combined: HashMap<Hash32, i128> = HashMap::new();
            for (cred, delta) in deltas {
                // `accumulated_mir_balances` mirrors the post-distribution
                // reward-accounts view, which is keyed by
                // `Credential::to_typed_hash32` (kind-tagged).  Use the same
                // form here so key and script stake credentials with
                // colliding 28-byte hashes do not merge.
                let key = cred.to_typed_hash32();
                *combined.entry(key).or_insert(0) += *delta as i128;
            }
            let mut bad: Vec<String> = Vec::new();
            for (key, delta_sum) in combined.iter() {
                let existing: i128 = accumulated.get(key).copied().unwrap_or(0) as i128;
                if existing + *delta_sum < 0 {
                    bad.push(key.to_hex());
                }
            }
            if !bad.is_empty() {
                bad.sort(); // deterministic for diagnostic stability
                errors.push(ValidationError::MIRProducesNegativeUpdate { credentials: bad });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `checkSendToOppositePotMIR` — pot transfer branch
// ---------------------------------------------------------------------------

/// Validate the `OtherAccountingPot(coin)` pot-to-pot transfer branch of an
/// MIR certificate.
///
/// Predicate breakdown:
/// - Pre-Alonzo (`pv <= 4`): pot transfers are not allowed
///   (`MIRTransferNotCurrentlyAllowed`).
/// - Alonzo+ (`pv >= 5`):
///   - `coin >= 0` (`MIRNegativeTransfer` — unreachable via dugite's
///     `u64`-typed `OtherAccountingPot`, retained for type completeness).
///   - `coin <= pot_balance` (`InsufficientForTransferDELEG`).
///
/// Reference: Haskell `checkSendToOppositePotMIR` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
pub(crate) fn check_send_to_opposite_pot_mir(
    source: &MIRSource,
    coin: u64,
    params: &ProtocolParameters,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let pv = params.protocol_version_major;
    let alonzo_or_later = pv > 4;

    if !alonzo_or_later {
        errors.push(ValidationError::MIRTransferNotCurrentlyAllowed);
        return;
    }

    // `OtherAccountingPot(u64)` is structurally non-negative in dugite, so
    // `MIRNegativeTransfer` is unreachable on the public type.  We keep the
    // variant defined for parity with Haskell's `DeltaCoin` payload.

    let Some(available) = pot_balance(source, ctx) else {
        return;
    };
    if coin > available {
        errors.push(ValidationError::InsufficientForTransferDELEG {
            pot: source.clone(),
            requested: coin,
            available,
        });
    }
}

// ---------------------------------------------------------------------------
// `validateMIRInsufficientGenesisSigs` — whole-transaction, UTXOW-level (#804)
// ---------------------------------------------------------------------------

/// Whole-transaction check: if `tx` carries at least one
/// `MoveInstantaneousRewards` certificate, at least
/// [`ValidationContext::update_quorum`] of the CURRENT genesis-delegate
/// (hot/delegate) keys — [`ValidationContext::genesis_delegate_keys`] —
/// must appear among the transaction's VKey witnesses.
///
/// Unlike every other predicate in this module, this is NOT a per-certificate
/// check folded into [`validate_mir_cert`] — it is a property of the whole
/// transaction (a single set of witnesses is checked against ALL MIR certs
/// in the tx collectively), matching Haskell's `validateMIRInsufficientGenesisSigs`
/// living in the UTXOW rule rather than DELEG.
///
/// Returns `Ok(())` (no-op) when:
/// - the protocol version is Conway+ (`>= 9`) — MIR certs are structurally
///   impossible there (`isInstantaneousRewards` is `AtMostEra "Babbage"` in
///   Haskell; `babbageUtxowMirTransition` is not in Conway's `transitionRules`
///   at all);
/// - `tx` has no `MoveInstantaneousRewards` certificate;
/// - [`ValidationContext::genesis_delegate_keys`] or
///   [`ValidationContext::update_quorum`] is `None` (lenient default —
///   callers without genesis-delegate plumbing cannot evaluate the quorum).
///
/// Reference: Haskell `validateMIRInsufficientGenesisSigs` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs`:
/// ```text
/// genDelegates = Set.fromList $ asWitness . genDelegKeyHash <$> Map.elems genMapping
/// genSig = Set.intersection genDelegates witsKeyHashes
/// failureUnless (not (null mirCerts) ==> Set.size genSig >= fromIntegral quorum)
/// ```
pub fn check_mir_genesis_quorum(
    tx: &Transaction,
    params: &ProtocolParameters,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    if params.protocol_version_major >= 9 {
        return;
    }
    let has_mir_cert = tx
        .body
        .certificates
        .iter()
        .any(|c| matches!(c, Certificate::MoveInstantaneousRewards { .. }));
    if !has_mir_cert {
        return;
    }
    let (Some(genesis_delegate_keys), Some(quorum)) =
        (ctx.genesis_delegate_keys.as_ref(), ctx.update_quorum)
    else {
        return;
    };

    // Same 32-byte-vkey guard as the general witness-completeness check
    // (`phase1::run_phase1_rules`, Rule 9b) — a malformed vkey must not be
    // hashed and counted toward the quorum.
    let vkey_witness_hashes: std::collections::HashSet<dugite_primitives::hash::Hash28> = tx
        .witness_set
        .vkey_witnesses
        .iter()
        .filter(|w| w.vkey.len() == 32)
        .map(|w| blake2b_224(&w.vkey))
        .collect();

    let signers: Vec<dugite_primitives::hash::Hash28> = genesis_delegate_keys
        .intersection(&vkey_witness_hashes)
        .copied()
        .collect();

    if (signers.len() as u64) < quorum {
        let mut signer_hexes: Vec<String> = signers.iter().map(|h| h.to_hex()).collect();
        signer_hexes.sort(); // deterministic for diagnostic stability
        errors.push(ValidationError::MIRInsufficientGenesisSigs {
            present: signers.len(),
            required: quorum,
            signers: signer_hexes,
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn pot_balance(source: &MIRSource, ctx: &ValidationContext) -> Option<u64> {
    match source {
        MIRSource::Reserves => ctx.reserves.map(|l| l.0),
        MIRSource::Treasury => ctx.treasury.map(|l| l.0),
    }
}

/// Compute `stabilityWindow = ceil(3 * k / f)` from the protocol parameters
/// and the supplied security parameter `k`.
///
/// Reference: Haskell `stabilityWindow` in
/// `cardano-ledger-shelley`/`cardano-protocol-tpraos`.
///
/// Returns `None` when either `security_param` or the active-slot
/// coefficient is unavailable.
pub(crate) fn compute_stability_window(
    params: &ProtocolParameters,
    security_param: Option<u64>,
) -> Option<u64> {
    let k = security_param?;
    let (f_num, f_den) = params.active_slot_coeff_rational();
    if f_num == 0 {
        return None;
    }
    let numerator = 3u128 * k as u128 * f_den as u128;
    let denominator = f_num as u128;
    Some(numerator.div_ceil(denominator) as u64)
}

#[cfg(test)]
mod tests;
