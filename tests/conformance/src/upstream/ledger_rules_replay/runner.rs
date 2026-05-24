//! Phase 4 — Runner: validate ImpSpec vectors against Dugite's engine.
//!
//! ## Current validation (Phase 4)
//!
//! For each vector the runner dispatches on the `rule` field derived from the
//! directory name:
//!
//! - **ConwayNEWEPOCH** (and any `*NEWEPOCH*` rule): reads the initial epoch
//!   number from `st_cbor[0]` and the signal epoch number from `sig_cbor`,
//!   then validates the hard invariant `signal_epoch > initial_epoch`.
//!
//! - **ConwayUTXO** (and any `*UTXO*` rule): calls
//!   `dugite_serialization::decode_transaction(era_id, sig_cbor)` on the
//!   signal file and returns `Failed` if it errors.
//!
//! - **POOL / CERT / CERTS / DELEG / GOVCERT / GOV / ENACT / RATIFY**:
//!   structural shape decode of the native STS signal — verifies the CBOR
//!   layout without full semantic execution.
//!
//! - **Unknown rules**: returns `Skipped` (no validation available yet).
//!
//! ## Phase 4 follow-on: full ledger replay
//!
//! Full ledger execution (apply_tx / apply_tick / apply_epoch) requires:
//! 1. A `LedgerState::from_cbor(imp_spec_format)` bridge for the `arr[7]`
//!    NewEpochState encoding (tracked as a separate ledger bridge task).
//! 2. Real ImpSpec fixture files generated from the Haskell toolchain.
//!
//! Until both are available the runner provides the deepest validation
//! achievable without that bridge.

use dugite_serialization::decode_transaction;

use crate::upstream::ledger_rules_replay::{
    bridge::{
        decode_cert_signal, decode_certs_signal_count, decode_deleg_signal,
        decode_enact_signal_shape, decode_epoch_no, decode_gov_signal_shape, decode_govcert_signal,
        decode_initial_epoch_no, decode_new_epoch_state, decode_pool_signal,
        decode_ratify_signal_count,
    },
    vector::ImpVector,
};

/// Outcome of validating one ImpSpec vector.
#[derive(Debug)]
pub enum RunOutcome {
    /// NEWEPOCH invariant validated: signal_epoch > initial_epoch.
    NewEpochValidated {
        initial_epoch: u64,
        signal_epoch: u64,
        /// Initial treasury (lovelace) from AccountState, or `None` if
        /// structural decode of the full NewEpochState failed.
        treasury: Option<u64>,
        /// Initial reserves (lovelace) from AccountState.
        reserves: Option<u64>,
        /// UTxO entry count from the initial LedgerState.
        utxo_count: Option<u64>,
        /// PoolDistr entry count from field[5] of the initial NewEpochState.
        pool_count: Option<u64>,
        /// Whether the Haskell expected final state (`st_out_cbor`) was present
        /// and its epoch number was successfully verified against the signal.
        ///
        /// `true`  — `st_out_cbor` was present and the final epoch matched the signal.
        /// `false` — `st_out_cbor` was absent (synthetic fixture or rejected transition).
        final_state_validated: bool,
    },
    /// UTXO signal decoded successfully as a transaction.
    UtxoDecoded { era_id: u16, tx_bytes: usize },
    /// Native STS signal decoded (non-Tx rule: POOL, CERT, CERTS, DELEG, GOVCERT, GOV, ENACT, RATIFY).
    NativeSigDecoded { rule_tag: String, sig_bytes: usize },
    /// Vector was skipped because no rule handler is implemented yet.
    Skipped { reason: String },
    /// Validation failed.
    Failed { detail: String },
}

/// Derive a Cardano HFC `era_id` from a rule name prefix.
///
/// Rule names follow the pattern `<Era><RULENAME>`, e.g. "ConwayNEWEPOCH",
/// "BabbageUTXO", "ShelleyNEWEPOCH". The era prefix determines the CBOR
/// decoder used for Transaction signals.
fn era_id_from_rule(rule: &str) -> u16 {
    if rule.starts_with("Conway") || rule.starts_with("CONWAY") {
        6
    } else if rule.starts_with("Babbage") || rule.starts_with("BABBAGE") {
        5
    } else if rule.starts_with("Alonzo") || rule.starts_with("ALONZO") {
        4
    } else if rule.starts_with("Mary") || rule.starts_with("MARY") {
        3
    } else if rule.starts_with("Allegra") || rule.starts_with("ALLEGRA") {
        2
    } else if rule.starts_with("Shelley") || rule.starts_with("SHELLEY") {
        1
    } else {
        6 // default Conway
    }
}

/// Whether a rule's signal is a full transaction CBOR blob (`Tx era`).
///
/// Only LEDGER and UTXO use `Tx era` as their ImpSpec signal. All other
/// ExecSpecRule rules (ENACT, DELEG, GOVCERT, POOL, CERT, CERTS, GOV,
/// RATIFY) use their own native STS signal types which are NOT full Tx:
///
/// Signal types by rule (Haskell cardano-ledger, 2026-05-23):
/// - NEWEPOCH → EpochNo (u64) — handled separately
/// - LEDGER → Tx era         — ImpSpec Imp/Core hook (tx submissions)
/// - UTXO   → Tx era         — ExecSpecRule direct
/// - POOL    → PoolCert       — array with uint tag, NOT a full Tx
/// - CERT    → TxCert era     — certificate, NOT a full Tx
/// - CERTS   → Seq TxCert     — certificate seq, NOT a full Tx
/// - DELEG   → ConwayDelegCert — delegation cert, NOT a full Tx
/// - GOVCERT → ConwayGovCert  — governance cert, NOT a full Tx
/// - GOV     → GovSignal era  — governance signal, NOT a full Tx
/// - ENACT   → EnactSignal    — enactment signal, NOT a full Tx
/// - RATIFY  → RatifySignal   — ratification signal, NOT a full Tx
fn is_tx_signal_rule(rule: &str) -> bool {
    let upper = rule.to_uppercase();
    upper.contains("LEDGER") || upper.contains("UTXO")
}

/// Identify native STS signal rules (non-Tx, non-NEWEPOCH rules).
///
/// Returns a canonical rule type tag string, or `None` if the rule is not
/// a native-signal rule handled by this function.
///
/// Order matters: GOVCERT must be checked before CERT (substring containment),
/// and CERTS must be checked before CERT.
fn is_native_sig_rule(rule: &str) -> Option<&'static str> {
    let upper = rule.to_uppercase();
    // CERTS before CERT (substring containment).
    if upper.contains("CERTS") {
        return Some("CERTS");
    }
    // GOVCERT before CERT.
    if upper.contains("GOVCERT") {
        return Some("GOVCERT");
    }
    if upper.contains("CERT") {
        return Some("CERT");
    }
    if upper.contains("DELEG") {
        return Some("DELEG");
    }
    if upper.contains("POOL") {
        return Some("POOL");
    }
    if upper.contains("ENACT") {
        return Some("ENACT");
    }
    if upper.contains("RATIFY") {
        return Some("RATIFY");
    }
    // GOV after GOVCERT (already handled above).
    if upper.contains("GOV") {
        return Some("GOV");
    }
    None
}

/// Validate `vec` according to its rule and return the outcome.
///
/// The `era_id` used for tx decoding is derived from the rule name prefix.
pub fn run_vector(vec: &ImpVector) -> RunOutcome {
    let rule = vec.rule.as_str();
    let upper = rule.to_uppercase();

    if upper.contains("NEWEPOCH") {
        run_newepoch(vec)
    } else if is_tx_signal_rule(rule) {
        run_tx_signal(vec)
    } else if let Some(rule_type) = is_native_sig_rule(rule) {
        match rule_type {
            "POOL" => run_pool_signal(vec),
            "CERT" => run_cert_signal(vec),
            "CERTS" => run_certs_signal(vec),
            "DELEG" => run_deleg_signal(vec),
            "GOVCERT" => run_govcert_signal(vec),
            "GOV" => run_gov_signal(vec),
            "ENACT" => run_enact_signal(vec),
            "RATIFY" => run_ratify_signal(vec),
            _ => unreachable!(),
        }
    } else {
        // No handler implemented for this rule.
        RunOutcome::Skipped {
            reason: format!(
                "rule '{rule}' has no registered handler — \
                 add to is_native_sig_rule() or is_tx_signal_rule() to enable"
            ),
        }
    }
}

/// Validate a NEWEPOCH vector.
///
/// Gating invariant: `signal_epoch > initial_epoch`.
///
/// The signal for NEWEPOCH is a bare CBOR u64 (target epoch number).
/// The initial epoch number is field [0] of the NewEpochState `array(7)`.
///
/// When `st_out_cbor` is present (real Haskell-generated vectors), the
/// final state epoch (field [0] of the post-transition NewEpochState) is
/// decoded and verified to equal `signal_epoch`.  A mismatch fails the test.
///
/// Additionally, `decode_new_epoch_state` is called on the initial state blob
/// to extract treasury + reserves from `AccountState`.  A failure here is
/// non-fatal — only the epoch-invariant is gating.
fn run_newepoch(vec: &ImpVector) -> RunOutcome {
    let initial_epoch = match decode_initial_epoch_no(&vec.st_cbor) {
        Ok(n) => n,
        Err(e) => {
            return RunOutcome::Failed {
                detail: format!("st_cbor decode (initial epoch_no): {e}"),
            }
        }
    };

    let signal_epoch = match decode_epoch_no(&vec.sig_cbor) {
        Ok(n) => n,
        Err(e) => {
            return RunOutcome::Failed {
                detail: format!("sig_cbor decode (signal epoch_no): {e}"),
            }
        }
    };

    if signal_epoch <= initial_epoch {
        // Conway NEWEPOCH: when signal <= nesEL, Haskell returns the same state
        // (no-op transition). This is a valid STS outcome. Skip rather than fail:
        // Dugite's runner only validates advancing transitions for now.
        return RunOutcome::Skipped {
            reason: format!(
                "NEWEPOCH no-op: signal_epoch ({signal_epoch}) \
                 <= initial_epoch ({initial_epoch}) — valid Haskell no-op, not yet validated"
            ),
        };
    }

    // Best-effort: decode the full initial NewEpochState for diagnostic fields.
    // Failure here is non-fatal — the epoch-invariant is the gating check.
    let (treasury, reserves, utxo_count, pool_count) = match decode_new_epoch_state(&vec.st_cbor) {
        Ok(nes) => (
            Some(nes.treasury),
            Some(nes.reserves),
            nes.ledger_state.utxo_count,
            Some(nes.pool_distr_count),
        ),
        Err(e) => {
            eprintln!("[ledger-rules] WARN decode_new_epoch_state (non-fatal): {e}");
            (None, None, None, None)
        }
    };

    // Gating check: if Haskell's expected final state is present, verify that
    // its epoch number (field[0] of the post-transition NewEpochState) equals
    // the signal epoch.  This validates that the STS rule advanced the epoch
    // counter correctly.
    let mut final_state_validated = false;
    if let Some(st_out) = &vec.st_out_cbor {
        match decode_initial_epoch_no(st_out) {
            Ok(final_epoch) => {
                if final_epoch != signal_epoch {
                    return RunOutcome::Failed {
                        detail: format!(
                            "Final state epoch ({final_epoch}) != signal epoch ({signal_epoch}): \
                             Haskell's post-transition NewEpochState[0] must equal the signal"
                        ),
                    };
                }
                final_state_validated = true;
            }
            Err(e) => {
                return RunOutcome::Failed {
                    detail: format!("Could not decode final state epoch from st_out_cbor: {e}"),
                };
            }
        }
    }

    RunOutcome::NewEpochValidated {
        initial_epoch,
        signal_epoch,
        treasury,
        reserves,
        utxo_count,
        pool_count,
        final_state_validated,
    }
}

/// Validate a UTXO/LEDGER vector by decoding the signal as a transaction.
fn run_tx_signal(vec: &ImpVector) -> RunOutcome {
    let era_id = era_id_from_rule(&vec.rule);
    match decode_transaction(era_id, &vec.sig_cbor) {
        Ok(_tx) => RunOutcome::UtxoDecoded {
            era_id,
            tx_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!(
                "tx-signal decode failed (rule={}, era_id={era_id}, {} bytes): {e}",
                vec.rule,
                vec.sig_cbor.len()
            ),
        },
    }
}

/// Validate a POOL signal: `[0, pool_params_list]` or `[1, pool_id, epoch]`.
fn run_pool_signal(vec: &ImpVector) -> RunOutcome {
    match decode_pool_signal(&vec.sig_cbor) {
        Ok(tag) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("POOL tag={tag}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("POOL signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a CERT signal: any TxCert variant.
fn run_cert_signal(vec: &ImpVector) -> RunOutcome {
    match decode_cert_signal(&vec.sig_cbor) {
        Ok(tag) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("CERT tag={tag}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("CERT signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a CERTS signal: array of TxCert elements.
fn run_certs_signal(vec: &ImpVector) -> RunOutcome {
    match decode_certs_signal_count(&vec.sig_cbor) {
        Ok(count) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("CERTS count={count}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("CERTS signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a DELEG signal: StakeDelegation (tag 2) or ConwayDelegCert (tags 7–13).
fn run_deleg_signal(vec: &ImpVector) -> RunOutcome {
    match decode_deleg_signal(&vec.sig_cbor) {
        Ok(tag) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("DELEG tag={tag}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("DELEG signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a GOVCERT signal: ConwayGovCert (tags 14–18).
fn run_govcert_signal(vec: &ImpVector) -> RunOutcome {
    match decode_govcert_signal(&vec.sig_cbor) {
        Ok(tag) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("GOVCERT tag={tag}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("GOVCERT signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a GOV signal: `[map, set_or_array, uint]`.
fn run_gov_signal(vec: &ImpVector) -> RunOutcome {
    match decode_gov_signal_shape(&vec.sig_cbor) {
        Ok(()) => RunOutcome::NativeSigDecoded {
            rule_tag: "GOV array(3)".to_string(),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("GOV signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate an ENACT signal: `[GovActionId, GovAction]`.
fn run_enact_signal(vec: &ImpVector) -> RunOutcome {
    match decode_enact_signal_shape(&vec.sig_cbor) {
        Ok(()) => RunOutcome::NativeSigDecoded {
            rule_tag: "ENACT array(2)".to_string(),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("ENACT signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}

/// Validate a RATIFY signal: array of GovActionState (each array(7)).
fn run_ratify_signal(vec: &ImpVector) -> RunOutcome {
    match decode_ratify_signal_count(&vec.sig_cbor) {
        Ok(count) => RunOutcome::NativeSigDecoded {
            rule_tag: format!("RATIFY count={count}"),
            sig_bytes: vec.sig_cbor.len(),
        },
        Err(e) => RunOutcome::Failed {
            detail: format!("RATIFY signal decode failed (rule={}): {e}", vec.rule),
        },
    }
}
