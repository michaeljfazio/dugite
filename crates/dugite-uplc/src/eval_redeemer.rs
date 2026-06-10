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
    ScriptContextV1, ScriptContextV2, ScriptContextV3, ScriptInfo, ScriptPurpose, TxOutRef,
};
use crate::term::{Constant, Term};
use crate::tx_info_populate::plutus_data_to_data;
use crate::UplcError;
use dugite_primitives::transaction::{
    Transaction, TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut,
};
use std::rc::Rc;

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
/// `cost_models` carries the per-Plutus-version flat cost-model arrays the
/// ledger lifted from the block's protocol parameters. When the array for
/// `r.language` is present and a byte-exact applier exists for that version,
/// the CEK machine is charged with the **on-chain** per-step + per-builtin
/// cost model (see [`crate::cost_apply`]); otherwise it falls back to the
/// latest reference model in `machine::cost` / `builtin::cost`. Matching the
/// on-chain model byte-exact is what makes `consumed` agree with cardano-node
/// — both for pass/fail classification and for the memory bound (the CEK
/// machine's only allocation limiter is the `ExBudget` memory dimension).
pub fn eval_resolved_redeemer(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    r: &ResolvedRedeemer,
    slot_config: &SlotConfig,
    initial_budget: ExBudget,
    cost_models: Option<&CostModels>,
    major_pv: u32,
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
    let version = program.version;
    let term = program.term;

    // 2. Build the per-version ScriptContext as Data.
    let ctx_data = build_script_context(tx, resolved, r, slot_config)?;

    if std::env::var("DUGITE_DUMP_CTX").is_ok() {
        eprintln!(
            "=== CTX tag={:?} idx={} lang={:?} ===",
            r.tag, r.index, r.language
        );
        debug_dump_data(&ctx_data, 0);
    }

    // Debug aid: alongside the applied-program flat dump (below), write each
    // script argument as CBOR so it can be byte-diffed against the canonical
    // args produced by cardano-ledger's `collectPlutusScriptsWithContext`.
    if let Ok(dir) = std::env::var("DUGITE_DUMP_APPLIED_DIR") {
        let write_arg = |name: &str, data: &crate::data::Data| {
            let path = format!("{dir}/args-{:?}-{}-{name}.cbor", r.tag, r.index);
            match data.to_cbor() {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&path, bytes) {
                        eprintln!("DUGITE_DUMP_APPLIED_DIR: write {path} failed: {e}");
                    }
                }
                Err(e) => eprintln!("DUGITE_DUMP_APPLIED_DIR: {name} to_cbor failed: {e}"),
            }
        };
        if let Some(datum) = r.datum.as_ref() {
            write_arg("datum", &plutus_data_to_data(datum));
        }
        write_arg("redeemer", &plutus_data_to_data(&r.redeemer_data));
        write_arg("ctx", &ctx_data);
    }

    // 3. Pre-apply the args. V3 takes one (ctx); V1/V2 takes three for
    //    a Spend redeemer (datum, redeemer, ctx) and two for
    //    Mint/Cert/Reward redeemers (redeemer, ctx) — the latter have
    //    no datum. Cf. Haskell `Plutus.V1.Ledger.Api`:
    //      * spending policy:  `\datum redeemer ctx -> ()`
    //      * minting policy / cert / reward script: `\redeemer ctx -> ()`
    let applied_term = match r.language {
        ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
            use dugite_primitives::transaction::RedeemerTag;
            let redeemer_term = data_const_term(plutus_data_to_data(&r.redeemer_data));
            let ctx_term = data_const_term(ctx_data);
            match r.tag {
                RedeemerTag::Spend => {
                    let datum = r.datum.as_ref().ok_or_else(|| {
                        PhaseTwoError::Internal(
                            "eval_resolved_redeemer: V1/V2 spend redeemer missing datum"
                                .to_string(),
                        )
                    })?;
                    let datum_term = data_const_term(plutus_data_to_data(datum));
                    // script(datum)(redeemer)(ctx)
                    Term::App(
                        Rc::new(Term::App(
                            Rc::new(Term::App(Rc::new(term), Rc::new(datum_term))),
                            Rc::new(redeemer_term),
                        )),
                        Rc::new(ctx_term),
                    )
                }
                RedeemerTag::Mint | RedeemerTag::Cert | RedeemerTag::Reward => {
                    // script(redeemer)(ctx)
                    Term::App(
                        Rc::new(Term::App(Rc::new(term), Rc::new(redeemer_term))),
                        Rc::new(ctx_term),
                    )
                }
                RedeemerTag::Vote | RedeemerTag::Propose | RedeemerTag::Guarding => {
                    return Err(PhaseTwoError::Internal(format!(
                        "eval_resolved_redeemer: tag {:?} is not valid for V1/V2",
                        r.tag
                    )));
                }
            }
        }
        ScriptLanguage::PlutusV3 => {
            let ctx_term = data_const_term(ctx_data);
            Term::App(Rc::new(term), Rc::new(ctx_term))
        }
    };

    // Debug aid (like DUGITE_DUMP_CTX above): dump the fully-applied
    // program as flat so it can be replayed through an external reference
    // CEK (Haskell `uplc evaluate`, `aiken uplc eval`) when root-causing a
    // budget/trace divergence offline.
    if let Ok(dir) = std::env::var("DUGITE_DUMP_APPLIED_DIR") {
        let prog = crate::program::Program {
            version,
            term: applied_term.clone(),
        };
        let path = format!("{dir}/applied-{:?}-{}.flat", r.tag, r.index);
        match prog.to_flat() {
            Ok(bytes) => match std::fs::write(&path, bytes) {
                Ok(()) => eprintln!("DUGITE_DUMP_APPLIED_DIR: wrote {path}"),
                Err(e) => eprintln!("DUGITE_DUMP_APPLIED_DIR: write {path} failed: {e}"),
            },
            Err(e) => eprintln!("DUGITE_DUMP_APPLIED_DIR: flat-encode failed: {e}"),
        }
    }

    // 4. Run the CEK machine with budget tracking and trace capture.
    //
    // `trace_log` collects strings emitted by the `Trace` builtin during
    // script execution, in emission order. The Haskell reference surfaces
    // these in `ValidationFailure logs` and in `cardano-cli`'s
    // "Trace logs: ..." error output.  We populate `RedeemerEvalOutcome.logs`
    // so callers (dugite-ledger Phase-2 error path and the CLI EvalTx
    // response) can mirror that behaviour.
    let mut trace_log: Vec<String> = Vec::new();
    let mut tracker = match resolve_applied_costs(cost_models, r.language, major_pv) {
        Some(applied) => BudgetTracker::with_applied(initial_budget, applied),
        None => BudgetTracker::new(initial_budget),
    };
    // Select the Plutus `BuiltinSemanticsVariant` ONCE from the script's
    // language + the block's major protocol version. This governs the small
    // set of builtins whose RESULT changed across protocol versions — today
    // only `consByteString` (lenient `fromIntegral`/mod-256 for PlutusV1/V2 at
    // every PV; strict `Word8` range-check for PlutusV3). See
    // [`SemanticsVariant::for_script`].
    let variant = crate::builtin::semantics::SemanticsVariant::for_script(r.language, major_pv);
    let value = evaluate_with_budget(applied_term, &mut tracker, Some(&mut trace_log), variant)
        .map_err(|error| {
            // If any trace strings were emitted before the error, surface
            // them in the richer `ScriptEvaluationFailedWithLogs` variant
            // so callers can render "Trace logs: [...]" before the error
            // (matching Haskell `cardano-cli transaction submit` output).
            // When no traces were emitted, use the simpler variant to keep
            // the error message clean.
            if trace_log.is_empty() {
                PhaseTwoError::ScriptEvaluationFailed(error)
            } else {
                PhaseTwoError::ScriptEvaluationFailedWithLogs {
                    error,
                    logs: trace_log.clone(),
                }
            }
        })?;

    // 5. Classify the CEK result.
    //
    //    Reaching this point means `evaluate_with_budget` returned `Ok` —
    //    i.e. the CEK machine produced a value without raising
    //    `Term::Error`. The shape of that value matters by language:
    //
    //    * V3 (Conway): result MUST be `Const(Unit)`. Anything else
    //      (Lambda / Delay / non-Unit constant) is a phase-2 failure.
    //      Cf. Haskell `Plutus.V3.Ledger.Api.evaluateScriptCounting`.
    //    * V1 / V2: success = CEK didn't raise `Error`. The result may
    //      be a Lambda or Delay value — e.g. the canonical IOG
    //      `always-true-v1` vendored fixture
    //      (cborHex `4e4d01000033222220051200120011`) reduces to a
    //      `Delay(Lam(Var 1))` after the 3-arg pre-application, and
    //      cardano-node accepts it. Cf. Haskell `Plutus.V1.Ledger.Api`
    //      / `Plutus.V2.Ledger.Api.evaluateScriptCounting`, which
    //      only fail on `CekFailure`.
    //
    //    `result_term` is preserved for V3's shape check; for V1/V2 it
    //    is forced to `Term::Error` only when the CEK actually raised
    //    Error (which would already have surfaced as `Err` above, but
    //    we keep the down-conversion defensively).
    let result_term = match value {
        crate::machine::Value::Const(c) => Term::Const(c),
        _ => Term::Error,
    };

    if matches!(r.language, ScriptLanguage::PlutusV3)
        && !matches!(&result_term, Term::Const(Constant::Unit))
    {
        return Err(PhaseTwoError::Internal(format!(
            "V3 script returned non-Unit value: {result_term:?}"
        )));
    }

    Ok(RedeemerEvalOutcome {
        consumed: tracker.consumed(),
        result_term,
        logs: trace_log,
    })
}

/// Convert a [`crate::data::Data`] into the term that pre-application
/// will pass into the script.
fn data_const_term(d: crate::data::Data) -> Term {
    Term::Const(Constant::Data(d))
}

/// Resolve the on-chain cost model for `language` into a fully-applied
/// [`crate::cost_apply::AppliedCosts`]. Returns `None` (→ fall back to the
/// latest reference model) when no array was supplied for that version, the
/// array is malformed, or no byte-exact applier exists for that version yet
/// (PlutusV2/V3 land in follow-ups). Mirrors how `mkEvaluationContext`
/// consumes the ledger's flat `[Int64]` per language in the Haskell node.
fn resolve_applied_costs(
    cost_models: Option<&CostModels>,
    language: ScriptLanguage,
    major_pv: u32,
) -> Option<crate::cost_apply::AppliedCosts> {
    let cm = cost_models?;
    match language {
        ScriptLanguage::PlutusV1 => {
            let params = cm.plutus_v1.as_deref()?;
            match crate::cost_apply::apply_v1(params, major_pv) {
                Ok(applied) => Some(applied),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "PlutusV1 cost model could not be applied; \
                         falling back to reference model"
                    );
                    None
                }
            }
        }
        ScriptLanguage::PlutusV2 => {
            let params = cm.plutus_v2.as_deref()?;
            match crate::cost_apply::apply_v2(params, major_pv) {
                Ok(applied) => Some(applied),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "PlutusV2 cost model could not be applied; \
                         falling back to reference model"
                    );
                    None
                }
            }
        }
        ScriptLanguage::PlutusV3 => {
            let params = cm.plutus_v3.as_deref()?;
            match crate::cost_apply::apply_v3(params) {
                Ok(applied) => Some(applied),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "PlutusV3 cost model could not be applied; \
                         falling back to reference model"
                    );
                    None
                }
            }
        }
    }
}

// Per-thread memoization of `script-bytes -> decoded Program`.
//
// Flat decoding (`flat::term::decode_term_inner`) is the single dominant
// apply-path cost on Plutus-dense mainnet blocks (profiling: ~1350/16800
// apply samples, allocation-bound), and the same popular validators recur
// across thousands of transactions — each carrying the full script in its
// witness. Decoding is a *pure, deterministic* function of the bytes, so
// memoizing it changes nothing observable (the `apply_bench` regression
// fingerprint must stay identical). Keyed by the exact input bytes, so there
// is no weak-hash collision risk. `Rc<Term>`-based `Program` is not `Send`,
// hence a `thread_local` cache (each apply/rayon thread keeps its own).
thread_local! {
    static SCRIPT_DECODE_CACHE: std::cell::RefCell<std::collections::HashMap<Vec<u8>, Program>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Bound on distinct cached scripts per thread (popular mainnet validators
/// number in the hundreds; the cap is a memory backstop, not a working-set
/// limit). On overflow the cache is cleared and repopulated.
const SCRIPT_DECODE_CACHE_CAP: usize = 4096;

/// Decode the script bytes the wire / reference-input provides.
///
/// On-chain Plutus scripts are CBOR-encoded byte-strings holding the
/// flat-encoded program (`from_cbor` handles this). Raw flat bytes
/// (no CBOR wrapper) are accepted as a fallback for scripts that were
/// extracted from the inner byte-string by an upstream decoder.
///
/// If the outer byte looks like a CBOR byte-string (major type 2) and
/// `from_cbor` fails, we propagate that error directly — the bytes are
/// structurally CBOR and it makes no sense to re-attempt as raw flat.
///
/// The decode is memoized per thread (see [`SCRIPT_DECODE_CACHE`]) — a pure
/// performance optimisation that does not change the decoded program.
fn decode_script_bytes(bytes: &[u8]) -> Result<Program, PhaseTwoError> {
    if let Some(prog) = SCRIPT_DECODE_CACHE.with(|c| c.borrow().get(bytes).cloned()) {
        return Ok(prog);
    }
    let prog = decode_script_bytes_uncached(bytes)?;
    SCRIPT_DECODE_CACHE.with(|c| {
        let mut m = c.borrow_mut();
        if m.len() >= SCRIPT_DECODE_CACHE_CAP {
            m.clear();
        }
        m.insert(bytes.to_vec(), prog.clone());
    });
    Ok(prog)
}

/// The uncached decode (the actual flat/CBOR deserialization).
fn decode_script_bytes_uncached(bytes: &[u8]) -> Result<Program, PhaseTwoError> {
    // CBOR major-type 2 (byte-string) = 0x40-0x5f (short) or 0x58/0x59/0x5a/0x5b (extended).
    let looks_like_cbor_bytes = bytes.first().is_some_and(|&b| b >> 5 == 2);

    if looks_like_cbor_bytes {
        // Bytes are CBOR-wrapped — decode once and propagate any error.
        return Program::from_cbor(bytes).map_err(|e| {
            PhaseTwoError::Internal(format!("eval_resolved_redeemer: script decode: {e}"))
        });
    }

    // Raw flat bytes (already unwrapped by the caller / serialization layer).
    Program::from_flat(bytes)
        .map_err(|e| PhaseTwoError::Internal(format!("eval_resolved_redeemer: script decode: {e}")))
}

fn debug_dump_data(d: &crate::data::Data, depth: usize) {
    use crate::data::Data;
    let pad = "  ".repeat(depth);
    match d {
        Data::Constr(tag, args) => {
            eprintln!("{pad}Constr {tag} [{}]", args.len());
            for a in args {
                debug_dump_data(a, depth + 1);
            }
        }
        Data::Map(kvs) => {
            eprintln!("{pad}Map [{}]", kvs.len());
            for (k, v) in kvs {
                eprintln!("{pad} k:");
                debug_dump_data(k, depth + 2);
                eprintln!("{pad} v:");
                debug_dump_data(v, depth + 2);
            }
        }
        Data::List(xs) => {
            eprintln!("{pad}List [{}]", xs.len());
            for x in xs {
                debug_dump_data(x, depth + 1);
            }
        }
        Data::I(n) => eprintln!("{pad}I {n}"),
        Data::B(b) => {
            let h = hex::encode(b);
            eprintln!(
                "{pad}B(len={}) {}",
                b.len(),
                if h.len() > 24 { &h[..24] } else { &h }
            );
        }
    }
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
            // the resolved inline/witness datum.
            let script_info = purpose_to_script_info_v3(r, tx, resolved)?;
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
///
/// For `Spending`, the inline `Option<Datum>` is resolved per
/// `Cardano.Ledger.Conway.TxInfo.toPlutusV3Args` /
/// `scriptPurposeToScriptInfo`:
///
/// ```haskell
/// PV3.Spending txIn ->
///   PV3.SpendingScript txIn maybeSpendingData
///   where
///     maybeSpendingData = transDatum <$> getBabbageSpendingDatum utxo tx sp
///     transDatum = PV2.Datum . dataToBuiltinData . getPlutusData
/// ```
///
/// `getPlutusData` strips the ledger `MemoBytes`, so the resulting
/// `Data` is the **canonical structural** translation
/// (`plutus_data_to_data`) — never the verbatim wire bytes.
fn purpose_to_script_info_v3(
    r: &ResolvedRedeemer,
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ScriptInfo, PhaseTwoError> {
    Ok(match &r.purpose {
        ScriptPurpose::Minting(h) => ScriptInfo::Minting(*h),
        ScriptPurpose::Spending(out_ref) => {
            // Resolve the spent output and run `getBabbageSpendingDatum`
            // (inline datum first, then datum-hash witness lookup, else
            // `Nothing`). `Nothing` is a VALID state for V3 — only the
            // V1/V2 path treats a missing spending datum as a hard error.
            let datum = resolved
                .iter()
                .find(|(i, _, _)| {
                    i.transaction_id.0 == out_ref.tx_id && i.index as u64 == out_ref.idx
                })
                .and_then(|(_, spent_out, _)| {
                    crate::redeemer_resolve::resolve_spend_datum_v3(tx, spent_out).transpose()
                })
                .transpose()?
                // `transDatum = PV2.Datum . dataToBuiltinData . getPlutusData`
                // — canonical structural Data, MemoBytes stripped.
                .map(|d| plutus_data_to_data(&d));
            ScriptInfo::Spending {
                out_ref: TxOutRef {
                    tx_id: out_ref.tx_id,
                    idx: out_ref.idx,
                },
                datum,
            }
        }
        ScriptPurpose::Rewarding(c) => ScriptInfo::Rewarding(c.clone()),
        ScriptPurpose::Certifying(idx, c) => ScriptInfo::Certifying(*idx, c.clone()),
        ScriptPurpose::Voting(v) => ScriptInfo::Voting(v.clone()),
        ScriptPurpose::Proposing(idx, p) => ScriptInfo::Proposing(*idx, p.clone()),
        // Dijkstra `DijkstraGuarding(ScriptHash)` — Sum 6.
        // Issue #475 Phase 3.5.
        ScriptPurpose::Guarding(h) => ScriptInfo::Guarding(*h),
    })
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
            safe_zone_horizon_slot: None,
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
            term: Term::Lam(Rc::new(Term::Const(Constant::Unit))),
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
            guards: Vec::new(),
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
            9,
        )
        .expect("V3 unit script runs");
        assert!(matches!(outcome.result_term, Term::Const(Constant::Unit)));
        assert!(outcome.consumed.cpu > 0);
        // No trace calls in this script — logs must be empty.
        assert!(outcome.logs.is_empty(), "no trace calls → empty logs");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Trace capture tests
    //
    // The Plutus `trace` builtin has Plutus type `all a. text -> a -> a`
    // and requires 1 force before use (`arity = (1, 2)`). So the UPLC
    // term shape is:
    //
    //   (force builtin(trace)) "msg" continuation
    //
    // We build minimal V3-compatible programs that call `trace` once (or
    // multiple times) before returning `()` or before erroring, to verify:
    //   1. Successful eval surfaces logged strings in `outcome.logs`.
    //   2. Failed eval (error term) surfaces logs in the error variant.
    // ─────────────────────────────────────────────────────────────────────

    /// Build a V3 script that emits `msg` via `trace` then returns `()`.
    /// Term shape:
    ///   lam ctx.
    ///     (force trace) "msg" ()
    fn trace_then_unit_v3_script(msg: &str) -> Vec<u8> {
        use crate::term::BuiltinId;
        // (force builtin(trace)) "msg" (con unit ())
        let trace_call = Term::App(
            Rc::new(Term::App(
                Rc::new(Term::Force(Rc::new(Term::Builtin(BuiltinId::Trace)))),
                Rc::new(Term::Const(Constant::String(msg.to_string()))),
            )),
            Rc::new(Term::Const(Constant::Unit)),
        );
        // Wrap in lam so the V3 single-arg calling convention is satisfied.
        let program = Program {
            version: (1, 0, 0),
            term: Term::Lam(Rc::new(trace_call)),
        };
        program.to_cbor().unwrap()
    }

    /// Build a V3 script that emits multiple trace messages in FIFO order
    /// then returns `()`.
    ///
    /// The CEK machine evaluates arguments in applicative order (innermost
    /// first). To get FIFO emission order `[first, second, third]`, we chain
    /// the traces via sequential lambda application so each message is
    /// evaluated as a function argument left-to-right:
    ///
    ///   lam ctx.
    ///     (lam _.
    ///       (lam _.
    ///         ()
    ///       ) ((force trace) "second" ())
    ///     ) ((force trace) "first" ())
    ///
    /// Wait — that still evaluates "first" (the outer arg) before "second"
    /// (the inner body's arg), which means:
    ///   evaluation: outer arg "first" → outer body → inner arg "second" → ...
    ///
    /// Actually the right structure for `[first, second, third]` in FIFO is:
    ///   (lam _ (lam _ (lam _ ()) trace("third")) trace("second")) trace("first")
    ///
    /// Because: evaluate outer arg trace("first") first, then enter body,
    /// evaluate inner arg trace("second"), then evaluate innermost arg trace("third").
    fn trace_triple_v3_script() -> Vec<u8> {
        use crate::term::BuiltinId;
        fn trace_call(msg: &str) -> Term {
            Term::App(
                Rc::new(Term::App(
                    Rc::new(Term::Force(Rc::new(Term::Builtin(BuiltinId::Trace)))),
                    Rc::new(Term::Const(Constant::String(msg.to_string()))),
                )),
                Rc::new(Term::Const(Constant::Unit)),
            )
        }
        // (lam _ (lam _ (lam _ ()) trace("third")) trace("second")) trace("first")
        //   → evaluates trace("first") → enters body → evaluates trace("second")
        //   → enters body → evaluates trace("third") → returns ()
        let body = Term::App(
            Rc::new(Term::App(
                Rc::new(Term::App(
                    Rc::new(Term::Lam(Rc::new(Term::Lam(Rc::new(Term::Lam(Rc::new(
                        Term::Const(Constant::Unit),
                    ))))))),
                    Rc::new(trace_call("first")),
                )),
                Rc::new(trace_call("second")),
            )),
            Rc::new(trace_call("third")),
        );
        let program = Program {
            version: (1, 0, 0),
            term: Term::Lam(Rc::new(body)),
        };
        program.to_cbor().unwrap()
    }

    /// Build a V3 script that emits a trace message then errors.
    ///
    /// The trace must FIRE before the error. In applicative order the second
    /// arg to trace is evaluated first (to get its value), so passing
    /// `Term::Error` directly as the second arg causes the error before trace
    /// fires. Instead we have trace return `()` and then error in the
    /// continuation:
    ///
    ///   lam ctx.
    ///     (lam _. error)             -- continuation: ignores the Unit, errors
    ///       ((force trace) "msg" ()) -- evaluates trace first, returns ()
    fn trace_then_error_v3_script(msg: &str) -> Vec<u8> {
        use crate::term::BuiltinId;
        // (force trace) "msg" () → fires trace, returns ()
        let trace_call = Term::App(
            Rc::new(Term::App(
                Rc::new(Term::Force(Rc::new(Term::Builtin(BuiltinId::Trace)))),
                Rc::new(Term::Const(Constant::String(msg.to_string()))),
            )),
            Rc::new(Term::Const(Constant::Unit)),
        );
        // (lam _. error) trace_call → evaluates trace_call (fires trace),
        //   binds result to _, then reduces body to error → ScriptError
        let body = Term::App(
            Rc::new(Term::Lam(Rc::new(Term::Error))),
            Rc::new(trace_call),
        );
        let program = Program {
            version: (1, 0, 0),
            term: Term::Lam(Rc::new(body)),
        };
        program.to_cbor().unwrap()
    }

    /// Build a minimal V3 `ResolvedRedeemer` using the supplied script bytes.
    /// Returns `(tx, resolved, resolved_redeemer)` ready for `eval_resolved_redeemer`.
    #[allow(clippy::type_complexity)]
    fn minimal_v3_setup(
        script_cbor: Vec<u8>,
    ) -> (
        dugite_primitives::transaction::Transaction,
        Vec<(
            dugite_primitives::transaction::TransactionInput,
            dugite_primitives::transaction::TransactionOutput,
            Vec<u8>,
        )>,
        crate::redeemer_resolve::ResolvedRedeemer,
    ) {
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

        let inner = {
            let mut d = minicbor::Decoder::new(&script_cbor);
            d.bytes().unwrap().to_vec()
        };
        let mut buf = vec![3u8];
        buf.extend_from_slice(&inner);
        let script_hash = dugite_primitives::hash::blake2b_224(&buf).0;
        let input = TransactionInput {
            transaction_id: Hash::<32>([0xcc; 32]),
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
            direct_deposits: BTreeMap::new(),
            guards: Vec::new(),
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
        (tx, resolved, resolved_redeemer)
    }

    #[test]
    fn trace_single_message_captured_in_logs() {
        let script_cbor = trace_then_unit_v3_script("hello from plutus");
        let (tx, resolved, r) = minimal_v3_setup(script_cbor);
        let budget = ExBudget {
            cpu: 100_000_000,
            mem: 1_000_000,
        };
        let outcome = eval_resolved_redeemer(&tx, &resolved, &r, &slot_cfg(), budget, None, 9)
            .expect("trace+unit script should succeed");
        assert_eq!(
            outcome.logs,
            vec!["hello from plutus"],
            "single trace call must appear in logs"
        );
    }

    #[test]
    fn trace_multiple_messages_captured_in_fifo_order() {
        let script_cbor = trace_triple_v3_script();
        let (tx, resolved, r) = minimal_v3_setup(script_cbor);
        let budget = ExBudget {
            cpu: 100_000_000,
            mem: 1_000_000,
        };
        let outcome = eval_resolved_redeemer(&tx, &resolved, &r, &slot_cfg(), budget, None, 9)
            .expect("triple-trace script should succeed");
        assert_eq!(
            outcome.logs,
            vec!["first", "second", "third"],
            "trace calls must appear in emission (FIFO) order"
        );
    }

    #[test]
    fn trace_before_error_surfaces_in_error_path() {
        let script_cbor = trace_then_error_v3_script("pre-error trace");
        let (tx, resolved, r) = minimal_v3_setup(script_cbor);
        let budget = ExBudget {
            cpu: 100_000_000,
            mem: 1_000_000,
        };
        let err = eval_resolved_redeemer(&tx, &resolved, &r, &slot_cfg(), budget, None, 9)
            .expect_err("trace+error script must fail");
        // The error variant must carry the trace log.
        match err {
            PhaseTwoError::ScriptEvaluationFailedWithLogs { logs, .. } => {
                assert_eq!(
                    logs,
                    vec!["pre-error trace"],
                    "trace emitted before error must appear in error logs"
                );
            }
            other => panic!("expected ScriptEvaluationFailedWithLogs, got: {other:?}"),
        }
    }
}
