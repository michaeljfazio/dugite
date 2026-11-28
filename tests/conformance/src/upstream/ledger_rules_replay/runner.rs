//! Runner: validate the ImpSpec vectors that are NOT replayed through the ledger.
//!
//! LEDGER and NEWEPOCH are applied and compared by [`replay`](super::replay).
//! This module covers the rest, and only DECODES the signal:
//!
//! - **UTXO** (and any `*UTXO*` rule): decodes the signal as a transaction with
//!   `dugite_serialization::decode_transaction`.
//!
//! - **POOL / CERT / CERTS / DELEG / GOVCERT / GOV / ENACT / RATIFY**
//!   (the ExecSpecRule rules, whose states are not a `NewEpochState`):
//!   structural shape decode of the native STS signal — verifies the CBOR
//!   layout without executing a rule.
//!
//! - **Unknown rules**: `Skipped`.
//!
//! The ExecSpecRule rules stay decode-only until their states get a bridge of
//! their own.

use dugite_serialization::decode_transaction;

use crate::upstream::ledger_rules_replay::{
    bridge::{
        decode_cert_signal, decode_certs_signal_count, decode_deleg_signal,
        decode_enact_signal_shape, decode_gov_signal_shape, decode_govcert_signal,
        decode_pool_signal, decode_ratify_signal_count,
    },
    vector::ImpVector,
};

/// Outcome of validating one ImpSpec vector.
#[derive(Debug)]
pub enum RunOutcome {
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
    rule.to_uppercase().contains("UTXO")
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

    if super::replay::rule_kind(rule).is_some() {
        RunOutcome::Failed {
            detail: format!(
                "rule '{rule}' is applied and compared by `replay::replay_vector`, \
                 not decoded by the runner"
            ),
        }
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

/// Validate a UTXO vector by decoding the signal as a transaction.
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
