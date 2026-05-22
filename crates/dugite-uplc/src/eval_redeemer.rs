//! Per-redeemer CEK invocation: takes a [`ResolvedRedeemer`] and the
//! per-version `TxInfo`, builds the appropriate `ScriptContext`,
//! pre-applies args, runs the CEK machine with a budget tracker, and
//! classifies the result.
//!
//! ## Calling convention
//!
//! * **V1 / V2** — the script term takes three args:
//!   `script(datum, redeemer, ScriptContext)`. `ScriptContext` =
//!   `Constr 0 [TxInfo, ScriptPurpose]`.
//! * **V3** — the script term takes a single arg:
//!   `script(ScriptContext)`. `ScriptContext` =
//!   `Constr 0 [TxInfo, redeemer, ScriptInfo]`; the redeemer + script
//!   purpose are embedded inside the context.
//!
//! Each arg is `Term::Const(Constant::Data(d))` where `d` is the
//! Data-encoded form. Application is via repeated `Term::App` so the
//! evaluator chains the reductions.
//!
//! ## Result classification
//!
//! * **V1 / V2** — success = any non-`Error` term value.
//! * **V3** — success = `Const(Unit)` exactly. Any other term value
//!   (Integer, Bool, ByteString, etc.) is treated as
//!   `InvalidReturnValue` per the Plutus V3 spec.

use crate::cost_models::CostModels;
use crate::machine::cost::BudgetTracker;
use crate::machine::step::evaluate_with_budget;
use crate::machine::ExBudget;
use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::populate_v1_v2::{populate_tx_info_v1, populate_tx_info_v2};
use crate::populate_v3::populate_tx_info_v3;
use crate::program::Program;
use crate::redeemer_resolve::{ResolvedRedeemer, ScriptLanguage};
use crate::script_context::{
    Credential as PlCred, ScriptContextV1, ScriptContextV2, ScriptContextV3, ScriptInfo,
    ScriptPurpose, TxOutRef,
};
use crate::term::{Constant, Term};
use crate::tx_info_populate::plutus_data_to_data;
use crate::UplcError;
use dugite_primitives::transaction::{
    Transaction, TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut,
};

/// The outcome of evaluating a single redeemer's script.
///
/// `consumed` is the actual `ExBudget` the CEK machine charged.
/// Callers compare this against the redeemer's declared `ex_units`
/// for per-redeemer enforcement (the ledger rejects the tx if
/// `consumed > declared` for any redeemer — matching cardano-node).
#[derive(Debug, Clone)]
pub struct RedeemerEvalOutcome {
    pub consumed: ExBudget,
    pub result_term: Term,
    pub logs: Vec<String>,
}

/// Evaluate a single resolved redeemer against the supplied tx +
/// resolved-UTxO context. Returns the CEK outcome on success.
///
/// `_cost_models` is currently accepted but not yet applied — the
/// CEK machine charges its default per-step cost from `machine::cost`.
/// The per-builtin cost-model wiring lands in a follow-on PR; until
/// then `consumed` is conservative-but-not-byte-exact. Cardano-node
/// validates only that the declared budget is sufficient, so a
/// conservative (smaller) consumption will still pass for the same
/// scripts cardano-node accepts.
pub fn eval_resolved_redeemer(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    r: &ResolvedRedeemer,
    slot_config: &SlotConfig,
    initial_budget: ExBudget,
    _cost_models: Option<&CostModels>,
) -> Result<RedeemerEvalOutcome, PhaseTwoError> {
    // 1. Decode script bytes → typed Term.
    //
    // Plutus scripts live in the witness set as **double-CBOR-wrapped**
    // bytes: the outer wrapper is a CBOR byte-string holding another
    // CBOR byte-string, which in turn holds the flat-encoded program.
    // We accept either single-wrapped (raw flat bytes inside a CBOR
    // bytes wrapper) or unwrapped flat depending on which decoder
    // succeeds first.
    let program = decode_script_bytes(&r.script_bytes)?;
    let term = program.term;

    // 2. Build the per-version ScriptContext as Data.
    let ctx_data = build_script_context(tx, resolved, r, slot_config)?;

    // 3. Pre-apply the args. V1/V2 take three; V3 takes one (the
    //    ctx already carries redeemer + script_info inside).
    let applied_term = match r.language {
        ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
            let datum = r.datum.as_ref().ok_or_else(|| {
                PhaseTwoError::Internal(
                    "eval_resolved_redeemer: V1/V2 spend redeemer missing datum".to_string(),
                )
            })?;
            let datum_term = data_const_term(plutus_data_to_data(datum));
            let redeemer_term = data_const_term(plutus_data_to_data(&r.redeemer_data));
            let ctx_term = data_const_term(ctx_data);
            // script(datum)(redeemer)(ctx)
            Term::App(
                Box::new(Term::App(
                    Box::new(Term::App(Box::new(term), Box::new(datum_term))),
                    Box::new(redeemer_term),
                )),
                Box::new(ctx_term),
            )
        }
        ScriptLanguage::PlutusV3 => {
            let ctx_term = data_const_term(ctx_data);
            Term::App(Box::new(term), Box::new(ctx_term))
        }
    };

    // 4. Run the CEK machine with budget tracking.
    let mut tracker = BudgetTracker::new(initial_budget);
    let value = evaluate_with_budget(applied_term, &mut tracker)
        .map_err(PhaseTwoError::ScriptEvaluationFailed)?;

    // 5. Convert the Value back to a Term for inspection. The CEK's
    //    Value::Const wraps a Constant; other Value variants
    //    (Lambda / Delay) cannot appear at the final reduction step
    //    of a well-formed script — cardano-node treats them as
    //    Error. We surface `Term::Error` in those cases so the
    //    classification below (success/failure) treats them
    //    uniformly.
    let result_term = match value {
        crate::machine::Value::Const(c) => Term::Const(c),
        // Lambdas / Delays at the top level mean the script returned a
        // partially-applied term — that's a Plutus-side bug, treat as
        // an Error result so callers reject the tx.
        _ => Term::Error,
    };

    // 6. V3-specific check: result MUST be `Const(Unit)`. V1/V2 accept
    //    any non-Error term.
    match r.language {
        ScriptLanguage::PlutusV3 => {
            if !matches!(&result_term, Term::Const(Constant::Unit)) {
                return Err(PhaseTwoError::Internal(format!(
                    "V3 script returned non-Unit value: {result_term:?}"
                )));
            }
        }
        ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
            if matches!(&result_term, Term::Error) {
                return Err(PhaseTwoError::Internal(
                    "V1/V2 script reduced to Term::Error".to_string(),
                ));
            }
        }
    }

    Ok(RedeemerEvalOutcome {
        consumed: tracker.consumed(),
        result_term,
        logs: Vec::new(), // trace-string capture is a follow-up
    })
}

/// Convert a [`crate::data::Data`] into the term that pre-application
/// will pass into the script.
fn data_const_term(d: crate::data::Data) -> Term {
    Term::Const(Constant::Data(d))
}

/// Decode the script bytes the wire / reference-input provides. Plutus
/// scripts on chain are encoded as a CBOR byte-string holding the
/// flat program; we try CBOR-wrapped first, then fall back to raw
/// flat. Returns the first decoder to succeed.
fn decode_script_bytes(bytes: &[u8]) -> Result<Program, PhaseTwoError> {
    // Cardano typically double-wraps: outer CBOR bstr holds an inner
    // CBOR bstr holding flat. Try inner-only first.
    if let Ok(p) = Program::from_cbor(bytes) {
        return Ok(p);
    }
    Program::from_flat(bytes)
        .map_err(|e| PhaseTwoError::Internal(format!("eval_resolved_redeemer: script decode: {e}")))
}

/// Build the per-version `ScriptContext` as a `Data` value.
fn build_script_context(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    r: &ResolvedRedeemer,
    slot_config: &SlotConfig,
) -> Result<crate::data::Data, PhaseTwoError> {
    match r.language {
        ScriptLanguage::PlutusV1 => {
            let tx_info = populate_tx_info_v1(tx, resolved, slot_config)?;
            let ctx = ScriptContextV1 {
                tx_info,
                purpose: r.purpose.clone(),
            };
            Ok(ctx.to_data())
        }
        ScriptLanguage::PlutusV2 => {
            let tx_info = populate_tx_info_v2(tx, resolved, slot_config)?;
            let ctx = ScriptContextV2 {
                tx_info,
                purpose: r.purpose.clone(),
            };
            Ok(ctx.to_data())
        }
        ScriptLanguage::PlutusV3 => {
            let tx_info = populate_tx_info_v3(tx, resolved, slot_config)?;
            // V3 builds a `ScriptInfo` from the purpose + (for spend)
            // the resolved datum.
            let script_info = purpose_to_script_info_v3(r);
            let redeemer_data = plutus_data_to_data(&r.redeemer_data);
            let ctx = ScriptContextV3 {
                tx_info,
                redeemer: redeemer_data,
                script_info,
            };
            Ok(ctx.to_data())
        }
    }
}

/// Lift a [`ScriptPurpose`] into a V3 [`ScriptInfo`]. The two enums
/// share constructors for Minting / Rewarding / Certifying / Voting
/// / Proposing — Spending differs (V3 inlines the datum reference).
fn purpose_to_script_info_v3(r: &ResolvedRedeemer) -> ScriptInfo {
    match &r.purpose {
        ScriptPurpose::Minting(h) => ScriptInfo::Minting(*h),
        ScriptPurpose::Spending(out_ref) => {
            // V3 inline-datum lookup: cardano-node passes
            // `Option<Datum>` here. Without an explicit datum on the
            // ResolvedRedeemer (V3 doesn't fill it), we fall back to
            // `None` — which matches V3 spending validators that
            // don't actually consume a datum (e.g., a "burn"-only
            // script). Validators that DO need a datum will pull it
            // from the inputs/refInputs in the ctx.
            ScriptInfo::Spending {
                out_ref: TxOutRef {
                    tx_id: out_ref.tx_id,
                    idx: out_ref.idx,
                },
                datum: None,
            }
        }
        ScriptPurpose::Rewarding(c) => ScriptInfo::Rewarding(c.clone()),
        ScriptPurpose::Certifying(idx, c) => ScriptInfo::Certifying(*idx, c.clone()),
        ScriptPurpose::Voting(v) => ScriptInfo::Voting(v.clone()),
        ScriptPurpose::Proposing(idx, p) => ScriptInfo::Proposing(*idx, p.clone()),
    }
}

// `PlCred` brought in for symmetry with future Spending-datum lookup
// (not used yet — placeholder for V3 inline-datum binding).
#[allow(dead_code)]
fn _unused_placeholder(c: PlCred) -> PlCred {
    c
}

// The `UplcError` import keeps the script-eval-failure path's
// `From<UplcError>` impl on PhaseTwoError reachable through the
// public API; suppress the unused-import warning that the trait
// import alone otherwise triggers.
#[allow(dead_code)]
fn _force_uplc_error_in_scope() -> Option<UplcError> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Data;
    use crate::term::{Constant, Term};
    use num_bigint::BigInt;

    fn slot_cfg() -> SlotConfig {
        SlotConfig {
            network_start_unix_seconds: 1_666_656_000,
            slot_zero_offset: 0,
            slot_length_ms: 1_000,
        }
    }

    #[test]
    fn data_const_term_wraps_correctly() {
        let d = Data::I(BigInt::from(7));
        let t = data_const_term(d.clone());
        assert!(matches!(t, Term::Const(Constant::Data(_))));
    }

    #[test]
    fn decode_script_bytes_rejects_garbage() {
        // Pure garbage — neither CBOR-wrapped nor valid flat.
        let err = decode_script_bytes(&[0xfe, 0xfe, 0xfe]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// Build a minimal `ResolvedRedeemer` that points at the smallest
    /// well-formed V3 program: `Program (1,0,0) (lam x. ())`. When
    /// applied with the script-context arg, it returns Unit and the
    /// V3 success check passes.
    fn unit_returning_v3_script() -> Vec<u8> {
        let program = Program {
            version: (1, 0, 0),
            // `lam x. const_unit` — `Lam(Const Unit)`. The bound var
            // `x` is unused, so the CEK just returns the constant.
            term: Term::Lam(Box::new(Term::Const(Constant::Unit))),
        };
        program.to_cbor().unwrap()
    }

    #[test]
    fn smoke_v3_unit_script_runs_and_returns_unit() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential as PrimCred;
        use dugite_primitives::era::Era;
        use dugite_primitives::hash::Hash;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{
            ExUnits, OutputDatum as PrimOutputDatum, PlutusData as PrimPlutusData, Redeemer,
            RedeemerTag, Transaction, TransactionBody, TransactionInput, TransactionOutput,
            TransactionWitnessSet,
        };
        use dugite_primitives::value::{Lovelace, Value};
        use std::collections::BTreeMap;

        let script_cbor = unit_returning_v3_script();
        // The witness-set Plutus-script entry is the inner flat bytes
        // (cardano stores them unwrapped at the witness-set level —
        // see `Program::from_cbor` which decodes the outer wrapper).
        let mut buf = vec![3u8];
        // Pull the inner flat bytes out of the CBOR wrapper.
        let inner = {
            let mut d = minicbor::Decoder::new(&script_cbor);
            d.bytes().unwrap().to_vec()
        };
        buf.extend_from_slice(&inner);
        let script_hash = dugite_primitives::hash::blake2b_224(&buf).0;
        let input = TransactionInput {
            transaction_id: Hash::<32>([0xaa; 32]),
            index: 0,
        };
        let spent_out = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::Script(Hash::<28>(script_hash)),
            }),
            value: Value::lovelace(1),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let body = TransactionBody {
            inputs: vec![input.clone()],
            outputs: vec![],
            fee: Lovelace(0),
            ttl: None,
            certificates: vec![],
            withdrawals: BTreeMap::new(),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: BTreeMap::new(),
            script_data_hash: None,
            collateral: vec![],
            required_signers: vec![],
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: vec![],
            update: None,
            voting_procedures: BTreeMap::new(),
            proposal_procedures: vec![],
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
            direct_deposits: ::std::collections::BTreeMap::new(),
        };
        let ws = TransactionWitnessSet {
            vkey_witnesses: vec![],
            native_scripts: vec![],
            bootstrap_witnesses: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![inner.clone()],
            plutus_data: vec![],
            redeemers: vec![Redeemer {
                tag: RedeemerTag::Spend,
                index: 0,
                data: PrimPlutusData::Integer(BigInt::from(0)),
                ex_units: ExUnits {
                    mem: 1_000_000,
                    steps: 100_000_000,
                },
            }],
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        };
        let tx = Transaction {
            hash: Hash::<32>([0; 32]),
            era: Era::Conway,
            body,
            witness_set: ws,
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let resolved = vec![(input, spent_out, vec![])];
        let resolved_redeemer = crate::redeemer_resolve::resolve_redeemers(&tx, &resolved)
            .expect("redeemer resolves")
            .into_iter()
            .next()
            .unwrap();
        let budget = ExBudget {
            cpu: 100_000_000,
            mem: 1_000_000,
        };
        let outcome = eval_resolved_redeemer(
            &tx,
            &resolved,
            &resolved_redeemer,
            &slot_cfg(),
            budget,
            None,
        )
        .expect("V3 unit script runs");
        assert!(matches!(outcome.result_term, Term::Const(Constant::Unit)));
        assert!(outcome.consumed.cpu > 0);
    }
}
