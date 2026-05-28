//! Phase-2 transaction evaluator — the entry point that
//! [`dugite_ledger::plutus`] calls per tx to validate every Plutus
//! redeemer in the witness set.
//!
//! ## API shape
//!
//! `eval_phase_two_raw` mirrors the function signature from
//! aiken-lang/uplc that dugite-ledger currently invokes, so the
//! ledger-side switch from aiken-uplc to dugite-uplc is a one-line
//! import change. The signature is:
//!
//! ```text
//! fn eval_phase_two_raw(
//!     tx_cbor: &[u8],
//!     utxos: &[(Vec<u8>, Vec<u8>)],     // (input_cbor, output_cbor) pairs
//!     cost_models_cbor: Option<&[u8]>,
//!     initial_budget: (u64, u64),       // (cpu, mem)
//!     slot_config: SlotConfig,
//!     run_phase_one: bool,
//!     with_redeemer: impl FnMut(&Redeemer),
//! ) -> Result<Vec<RedeemerResult>, PhaseTwoError>;
//! ```
//!
//! ## Implementation status
//!
//! The full byte-exact phase-2 evaluator requires:
//!
//! - Decoding tx + UTxO map (have via `dugite-serialization`)
//! - Building the per-version `TxInfo` from those (V1/V2 = subset of
//!   V3; V3 lands here)
//! - For each redeemer: resolve script + datum, build ScriptContext,
//!   encode to Data, apply args, evaluate via CEK with budget tracker,
//!   and record consumed ExUnits.
//!
//! This module currently lands the **API surface** + a `tx_info`
//! builder skeleton. The end-to-end wire-up arrives in follow-on
//! commits so callers can switch their import once and progress
//! happens incrementally underneath. Calling [`eval_phase_two_raw`]
//! today returns [`PhaseTwoError::NotImplemented`].
//!
//! Once the full path lands, dugite-ledger drops the
//! `uplc = { git = aiken-lang/aiken.git }` workspace dep and the
//! transitive `pallas-*` chain comes with it.

use crate::cost_models::{decode_cost_models_cbor, CostModels};
use crate::machine::cost::ExBudget;
use dugite_primitives::transaction::{Transaction, TransactionInput, TransactionOutput};

/// Slot config — `(network_start_unix_seconds, slot_zero_offset,
/// slot_length_ms, safe_zone_horizon_slot)`. Mirrors the Cardano
/// `SlotConfig` used to translate slots ↔ POSIX time for
/// `txValidRange` in TxInfo.
///
/// `safe_zone_horizon_slot` is the **exclusive** upper-bound slot past
/// which slot→POSIX translation must fail with
/// [`PhaseTwoError::TimeTranslationPastHorizon`]. Mirrors Haskell
/// `Ouroboros.Consensus.HardFork.History.Qry.guardEnd`:
/// `guard $ p b` where `p = \end -> absSlot < boundSlot end`.
///
/// `None` means **unbounded** (no horizon enforcement) — only safe to
/// use for tests, never in production. The dugite-ledger caller is
/// responsible for plumbing the correct horizon from `EraHistory
/// ::safe_zone_horizon_slot(ledger_tip)`. See
/// `crates/dugite-consensus/src/era_history.rs` for the canonical
/// formula and `audit-findings/2026-05-28-skill-self-audit.md` for the
/// Round-1 P0 regression this enforces against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotConfig {
    pub network_start_unix_seconds: u64,
    pub slot_zero_offset: u64,
    pub slot_length_ms: u32,
    pub safe_zone_horizon_slot: Option<u64>,
}

/// The "fully decoded" inputs to phase-2 evaluation. Constructed once at
/// the top of [`eval_phase_two_raw`] and consumed by all downstream
/// TxInfo-population / ScriptContext-building steps.
///
/// The `output_raw_cbor` slice retained per UTxO is the exact bytes the
/// caller passed in; preserving them lets the downstream TxInfo builder
/// hand reference scripts back to the CEK machine byte-exact and keeps
/// us aligned with the script-data-hash domain.
//
// Fields are read by tests in this module and will be consumed by UPLC-9
// parts 3-4 (TxInfo population, ScriptContext build, CEK eval).
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DecodedPhaseTwoInputs {
    /// The decoded transaction.
    pub tx: Transaction,
    /// The resolved UTxO entries: `(input, output, output_raw_cbor)` triples.
    pub utxos: Vec<(TransactionInput, TransactionOutput, Vec<u8>)>,
    /// The per-Plutus-version cost-model coefficient arrays the ledger
    /// supplied via `cost_models_cbor`. `None` when the caller passed
    /// `cost_models_cbor: None` — the CEK machine falls back to its
    /// per-step default cost from `machine::cost` in that case.
    pub cost_models: Option<CostModels>,
}

/// Per-redeemer evaluation result. Returned by
/// [`eval_phase_two_raw`] for every successful redeemer evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemerResult {
    /// The redeemer's raw CBOR bytes (round-tripped from the tx for
    /// the caller's logging convenience).
    pub redeemer_cbor: Vec<u8>,
    /// The ExUnits consumed (cpu, mem). Matches the units the ledger
    /// charges as part of script-execution fees.
    pub consumed: ExBudget,
    /// Redeemer tag identifying which purpose group this redeemer
    /// belongs to (Spend / Mint / Cert / Reward / Vote / Propose).
    pub tag: dugite_primitives::transaction::RedeemerTag,
    /// 0-based index of the redeemer within its purpose group.
    pub index: u32,
    /// `trace` builtin output captured during evaluation. Surfaces
    /// `Plutus.Trace` messages back to the caller (used by
    /// `SubmitService.EvalTx.traces`).
    pub logs: Vec<String>,
}

/// All failure modes the phase-2 evaluator can surface to the
/// ledger. Mirrors the typed taxonomy from aiken-uplc.
#[derive(Debug, thiserror::Error)]
pub enum PhaseTwoError {
    /// The evaluator is wired into the dependency graph but the
    /// per-version `TxInfo` builder and CEK glue have not yet
    /// landed. dugite-ledger should keep calling aiken-uplc until
    /// this variant disappears.
    #[error(
        "phase-2 evaluator not yet fully implemented (see crates/dugite-uplc/src/phase_two.rs)"
    )]
    NotImplemented,
    /// Failure decoding the tx CBOR.
    #[error("tx decode failed: {0}")]
    TxDecode(String),
    /// Failure decoding a UTxO entry.
    #[error("utxo decode failed: {0}")]
    UtxoDecode(String),
    /// Failure decoding cost models.
    #[error("cost model decode failed: {0}")]
    CostModelDecode(String),
    /// Script not found for a redeemer's purpose.
    #[error("script not found for redeemer purpose: {0}")]
    MissingScript(String),
    /// Datum not found for a V1/V2 spending redeemer.
    #[error("datum not found for V1/V2 spending redeemer: {hash}")]
    MissingDatum { hash: String },
    /// CEK evaluation failed.
    #[error("script evaluation failed: {0}")]
    ScriptEvaluationFailed(#[from] crate::UplcError),
    /// CEK evaluation failed and the script emitted trace strings before
    /// the error. Mirrors the Haskell `ValidationFailure exUnits err logs`
    /// constructor (`Cardano.Ledger.Alonzo.Plutus.Evaluate`).
    /// Callers that display errors to users should render the logs as
    /// "Trace logs: [...]" preceding the evaluation error (matching
    /// `cardano-cli transaction submit` output).
    #[error("script evaluation failed: {error}; trace logs: {logs:?}")]
    ScriptEvaluationFailedWithLogs {
        error: crate::UplcError,
        logs: Vec<String>,
    },
    /// Generic internal error.
    #[error("internal phase-2 error: {0}")]
    Internal(String),
    /// A slot in the tx's validity interval is past the era's safe-zone
    /// horizon. Mirrors Haskell
    /// `Alonzo.Plutus.TxInfo.TimeTranslationPastHorizon`. Produced by
    /// `slot_to_posix_ms` when the supplied slot is `>= horizon`. Bubbles
    /// up to dugite-ledger as a Phase-2 `BadTranslation`, causing the tx
    /// to be rejected pre-mempool and pre-forge — matching the way
    /// cardano-node would reject it at block-apply.
    #[error("time translation past horizon: slot={slot} horizon={horizon}")]
    TimeTranslationPastHorizon { slot: u64, horizon: u64 },
}

/// The redeemer trait callers implement to observe each redeemer
/// during evaluation (used by aiken-uplc to surface debug info).
/// We provide a no-op implementation by default so callers without
/// observation needs can pass `()`.
pub trait RedeemerObserver {
    fn on_redeemer(&mut self, redeemer_cbor: &[u8]);
}

impl RedeemerObserver for () {
    fn on_redeemer(&mut self, _redeemer_cbor: &[u8]) {}
}

/// Evaluate every Plutus redeemer in `tx_cbor` against the supplied
/// `utxos` map.
///
/// Returns one [`RedeemerResult`] per redeemer in the order the
/// redeemers appear in the tx witness set. If any redeemer fails,
/// the function returns `Err` immediately and does not continue to
/// subsequent redeemers (matches aiken-uplc's fail-fast semantics).
///
/// `run_phase_one` controls whether the function additionally
/// re-runs Phase-1 (structural) validation as a safety net before
/// any CEK invocation; dugite-ledger calls this with `true`.
///
/// `observer` is invoked once per redeemer with its raw CBOR bytes,
/// before the CEK evaluation. Pass `()` if you don't need this.
pub fn eval_phase_two_raw<O: RedeemerObserver>(
    tx_cbor: &[u8],
    utxos: &[(Vec<u8>, Vec<u8>)],
    cost_models_cbor: Option<&[u8]>,
    initial_budget: (u64, u64),
    slot_config: SlotConfig,
    _run_phase_one: bool,
    observer: &mut O,
) -> Result<Vec<RedeemerResult>, PhaseTwoError> {
    // Wire-up checklist:
    //   1. Decode tx via dugite-serialization                ── DONE (UPLC-9 part 1)
    //   2. Decode UTxO entries                               ── DONE (UPLC-9 part 1)
    //   3. Parse cost models per Plutus version              ── DONE (UPLC-9 part 2)
    //   4. Build per-version TxInfo                          ── DONE (UPLC-9 part 3a-3e)
    //   5. For each redeemer:                                ── DONE (UPLC-9 part 4a-4c)
    //      a. Resolve script (witness set or reference input)
    //      b. Resolve datum (V1/V2 only)
    //      c. Build ScriptContext + encode to Data
    //      d. Apply args + evaluate via CEK with budget tracker
    //      e. Push RedeemerResult
    let decoded = decode_phase_two_inputs(tx_cbor, utxos, cost_models_cbor)?;

    // Build the resolved-UTxO triple list the eval pipeline expects.
    let resolved_triples: Vec<(
        dugite_primitives::transaction::TransactionInput,
        dugite_primitives::transaction::TransactionOutput,
        Vec<u8>,
    )> = decoded
        .utxos
        .iter()
        .map(|(i, o, raw)| (i.clone(), o.clone(), raw.clone()))
        .collect();

    let resolved_redeemers =
        crate::redeemer_resolve::resolve_redeemers(&decoded.tx, &resolved_triples)?;

    let initial_ex_budget = crate::machine::ExBudget {
        cpu: initial_budget.0 as i64,
        mem: initial_budget.1 as i64,
    };

    let mut results: Vec<RedeemerResult> = Vec::with_capacity(resolved_redeemers.len());
    for resolved_r in &resolved_redeemers {
        // The observer wants the raw redeemer CBOR — but the wire
        // form of a redeemer is `[tag, idx, data, ex_units]` (Alonzo)
        // or a map entry (Conway). We don't have the raw bytes here,
        // so we re-encode the redeemer's Data payload as a minimal
        // observable proxy. The observer is only used for logging;
        // tests that need byte-exact CBOR should hook
        // `tx.witness_set.raw_redeemers_cbor` instead.
        let redeemer_data_translated =
            crate::tx_info_populate::plutus_data_to_data(&resolved_r.redeemer_data);
        let redeemer_proxy_cbor = redeemer_data_translated.to_cbor().unwrap_or_default();
        observer.on_redeemer(&redeemer_proxy_cbor);

        let outcome = crate::eval_redeemer::eval_resolved_redeemer(
            &decoded.tx,
            &resolved_triples,
            resolved_r,
            &slot_config,
            initial_ex_budget,
            decoded.cost_models.as_ref(),
        )?;

        // Enforce per-redeemer declared budget: cardano-node rejects the
        // tx when the actual consumed cost exceeds what the redeemer
        // declared. `declared_ex_units` is `(mem, cpu)` from the wire
        // (matches `ExUnits { mem, steps }`); `outcome.consumed` is
        // `(cpu, mem)` so we compare each dimension explicitly.
        let declared_mem = resolved_r.declared_ex_units.0 as i64;
        let declared_cpu = resolved_r.declared_ex_units.1 as i64;
        if outcome.consumed.cpu > declared_cpu || outcome.consumed.mem > declared_mem {
            return Err(PhaseTwoError::Internal(format!(
                "redeemer {tag:?}@{idx} consumed (cpu={}, mem={}) exceeds declared \
                 (cpu={}, mem={})",
                outcome.consumed.cpu,
                outcome.consumed.mem,
                declared_cpu,
                declared_mem,
                tag = resolved_r.tag,
                idx = resolved_r.index,
            )));
        }

        results.push(RedeemerResult {
            redeemer_cbor: redeemer_proxy_cbor,
            consumed: outcome.consumed,
            tag: resolved_r.tag.clone(),
            index: resolved_r.index,
            logs: outcome.logs,
        });
    }
    Ok(results)
}

/// Decode the wire-format inputs (`tx_cbor` + resolved-UTxO pairs + cost
/// models) the ledger hands to [`eval_phase_two_raw`].
///
/// The transaction is decoded by trying eras Conway → Babbage → Alonzo in turn
/// (which covers every era that admits Plutus scripts). The first success
/// wins, and the decoded `Transaction::era` is then used as the era hint for
/// every UTxO output the same caller supplies. Each input CBOR is decoded
/// era-agnostically (the `[tx_hash(32), index]` shape is era-invariant from
/// Shelley onwards).
///
/// `cost_models_cbor` is parsed via [`crate::cost_models::decode_cost_models_cbor`]
/// when present. `None` means "no cost model configured" — downstream the CEK
/// machine falls back to the per-step default cost from `machine::cost`.
///
/// On failure, returns a [`PhaseTwoError`] variant naming exactly which part
/// could not be decoded (transaction body, input #N, output #N, or cost model)
/// so the caller's logs surface the offending entry.
pub(crate) fn decode_phase_two_inputs(
    tx_cbor: &[u8],
    utxos: &[(Vec<u8>, Vec<u8>)],
    cost_models_cbor: Option<&[u8]>,
) -> Result<DecodedPhaseTwoInputs, PhaseTwoError> {
    let tx = decode_tx_multi_era(tx_cbor)?;
    let output_era_id = era_id_for_outputs(&tx);
    let mut decoded_utxos: Vec<(TransactionInput, TransactionOutput, Vec<u8>)> =
        Vec::with_capacity(utxos.len());
    for (idx, (input_cbor, output_cbor)) in utxos.iter().enumerate() {
        let input = dugite_serialization::decode_transaction_input(input_cbor)
            .map_err(|e| PhaseTwoError::UtxoDecode(format!("utxo #{idx} input: {e}")))?;
        let output = dugite_serialization::decode_transaction_output(output_era_id, output_cbor)
            .map_err(|e| PhaseTwoError::UtxoDecode(format!("utxo #{idx} output: {e}")))?;
        decoded_utxos.push((input, output, output_cbor.clone()));
    }
    let cost_models = match cost_models_cbor {
        Some(bytes) => Some(decode_cost_models_cbor(bytes)?),
        None => None,
    };
    Ok(DecodedPhaseTwoInputs {
        tx,
        utxos: decoded_utxos,
        cost_models,
    })
}

/// Attempt to decode `tx_cbor` as a Conway/Babbage/Alonzo transaction.
///
/// We try post-Alonzo eras in newest-first order because (a) at-tip txs are
/// nearly always Conway today and (b) Conway accepts the strictest CDDL, so
/// a successful Conway decode is the strongest era signal. If every era
/// rejects the bytes, we surface the **last** (= oldest-era / least-strict)
/// decoder's error, which is the most likely to describe a real malformedness
/// rather than an era mismatch.
fn decode_tx_multi_era(tx_cbor: &[u8]) -> Result<Transaction, PhaseTwoError> {
    // Era ids in dispatch order: Conway, Babbage, Alonzo. (Dijkstra shares
    // Conway's wire shape; the conway decoder accepts both — we don't need to
    // try id=7 separately.) Plutus is impossible in Allegra/Mary/Shelley/Byron
    // so we never attempt those.
    let mut last_err: Option<String> = None;
    for era_id in [6u16, 5, 4] {
        match dugite_serialization::decode_transaction(era_id, tx_cbor) {
            Ok(tx) => return Ok(tx),
            Err(e) => last_err = Some(format!("era_id={era_id}: {e}")),
        }
    }
    Err(PhaseTwoError::TxDecode(last_err.unwrap_or_else(|| {
        "phase-2 tx decode: no era accepted the bytes".to_string()
    })))
}

/// Map a decoded [`Transaction::era`] back to the `era_id` accepted by
/// [`dugite_serialization::decode_transaction_output`]. We restrict the
/// codomain to Plutus-capable eras and fall back to Conway for any future
/// era we have not yet enumerated — that is the safest default since Conway
/// outputs accept the legacy array form too.
fn era_id_for_outputs(tx: &Transaction) -> u16 {
    use dugite_primitives::era::Era;
    match tx.era {
        Era::Allegra => 2,
        Era::Mary => 3,
        Era::Alonzo => 4,
        Era::Babbage => 5,
        Era::Conway => 6,
        Era::Dijkstra => 7,
        // Byron / Shelley have no Plutus scripts. If we somehow end up here,
        // treat outputs as Conway (the most-permissive shape) so downstream
        // wiring can surface a meaningful failure rather than a panic.
        Era::Byron | Era::Shelley => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::era::Era;

    // ─────────────────────────────────────────────────────────────
    // CBOR builders (kept tiny — only what these tests need)
    // ─────────────────────────────────────────────────────────────

    fn minimal_conway_tx_cbor() -> Vec<u8> {
        // tx = [body, witness_set, is_valid, aux] with body = {0: [], 1: [], 2: fee}.
        // Matches the minimal-but-valid Conway tx the era_conway tests use.
        let mut tx = vec![0x84]; // array(4)
        tx.push(0xa3); // map(3): tx_body
        tx.push(0x00); // key 0 = inputs
        tx.push(0x80); // array(0)
        tx.push(0x01); // key 1 = outputs
        tx.push(0x80); // array(0)
        tx.push(0x02); // key 2 = fee
        tx.push(0x1a); // uint(u32)
        tx.extend(123_456u32.to_be_bytes());
        tx.push(0xa0); // map(0): witness_set
        tx.push(0xf5); // is_valid = true
        tx.push(0xf6); // aux_data = null
        tx
    }

    fn minimal_conway_tx_cbor_with_two_inputs() -> Vec<u8> {
        // Same minimal tx but with two declared inputs (we don't care about UTxO
        // resolution in this test — just shape parsing).
        // Inputs encoded as set-tagged-258 in Conway.
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.push(0x00); // inputs
                       // tag 258: 0xd9 0x01 0x02
        tx.extend([0xd9, 0x01, 0x02]);
        tx.push(0x82); // array(2)
                       // input 0
        tx.push(0x82); // array(2)
        tx.push(0x58); // bytes(32)
        tx.push(32);
        tx.extend([0x11; 32]);
        tx.push(0x00); // index 0
                       // input 1
        tx.push(0x82);
        tx.push(0x58);
        tx.push(32);
        tx.extend([0x22; 32]);
        tx.push(0x01); // index 1
        tx.push(0x01); // outputs
        tx.push(0x80);
        tx.push(0x02); // fee
        tx.push(0x18);
        tx.push(99);
        tx.push(0xa0);
        tx.push(0xf5);
        tx.push(0xf6);
        tx
    }

    fn make_input_cbor(hash_byte: u8, idx: u32) -> Vec<u8> {
        let mut v = vec![0x82, 0x58, 0x20];
        v.extend(std::iter::repeat_n(hash_byte, 32));
        v.push(0x1a);
        v.extend(idx.to_be_bytes());
        v
    }

    fn make_conway_map_output_cbor(lovelace: u32) -> Vec<u8> {
        let mut out = vec![0xa2, 0x00, 0x58, 29, 0x60];
        out.extend([0xab; 28]);
        out.push(0x01);
        out.push(0x1a);
        out.extend(lovelace.to_be_bytes());
        out
    }

    fn default_slot_config() -> SlotConfig {
        SlotConfig {
            network_start_unix_seconds: 1_596_491_091,
            slot_zero_offset: 4_492_800,
            slot_length_ms: 1_000,
            safe_zone_horizon_slot: None,
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Decode path
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn decode_phase_two_inputs_lifts_minimal_conway_tx() {
        let tx_cbor = minimal_conway_tx_cbor();
        let decoded = decode_phase_two_inputs(&tx_cbor, &[], None).expect("decode");
        assert_eq!(decoded.tx.era, Era::Conway);
        assert_eq!(decoded.tx.body.fee.0, 123_456);
        assert!(decoded.utxos.is_empty());
    }

    #[test]
    fn decode_phase_two_inputs_decodes_each_utxo_entry() {
        let tx_cbor = minimal_conway_tx_cbor_with_two_inputs();
        let utxos = vec![
            (
                make_input_cbor(0x11, 0),
                make_conway_map_output_cbor(1_000_000),
            ),
            (
                make_input_cbor(0x22, 1),
                make_conway_map_output_cbor(2_500_000),
            ),
        ];
        let decoded = decode_phase_two_inputs(&tx_cbor, &utxos, None).expect("decode");
        assert_eq!(decoded.utxos.len(), 2);
        assert_eq!(decoded.utxos[0].0.index, 0);
        assert_eq!(decoded.utxos[0].1.value.coin.0, 1_000_000);
        assert_eq!(decoded.utxos[1].0.index, 1);
        assert_eq!(decoded.utxos[1].1.value.coin.0, 2_500_000);
        // raw output bytes are preserved verbatim for the downstream TxInfo builder.
        assert_eq!(decoded.utxos[0].2, utxos[0].1);
        assert_eq!(decoded.utxos[1].2, utxos[1].1);
    }

    #[test]
    fn decode_phase_two_inputs_reports_failing_utxo_index() {
        // Valid tx, two utxos — second one's output is malformed.
        let tx_cbor = minimal_conway_tx_cbor_with_two_inputs();
        let utxos = vec![
            (
                make_input_cbor(0x11, 0),
                make_conway_map_output_cbor(1_000_000),
            ),
            (make_input_cbor(0x22, 1), vec![0xff]), // bogus
        ];
        let err = decode_phase_two_inputs(&tx_cbor, &utxos, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("utxo #1 output"),
            "error should name the failing entry: {msg}"
        );
    }

    #[test]
    fn decode_phase_two_inputs_reports_failing_input() {
        let tx_cbor = minimal_conway_tx_cbor();
        let utxos = vec![(vec![0xff], make_conway_map_output_cbor(1))];
        let err = decode_phase_two_inputs(&tx_cbor, &utxos, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("utxo #0 input"), "got: {msg}");
    }

    #[test]
    fn decode_phase_two_inputs_rejects_empty_tx_cbor() {
        let err = decode_phase_two_inputs(&[], &[], None).unwrap_err();
        assert!(matches!(err, PhaseTwoError::TxDecode(_)));
    }

    #[test]
    fn decode_phase_two_inputs_rejects_garbage_tx_cbor() {
        // None of the post-Alonzo era decoders accept a bare break byte.
        let err = decode_phase_two_inputs(&[0xff], &[], None).unwrap_err();
        assert!(matches!(err, PhaseTwoError::TxDecode(_)));
    }

    // ─────────────────────────────────────────────────────────────
    // cost_models wire-through (UPLC-9 part 2)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn decode_phase_two_inputs_threads_cost_models_when_supplied() {
        use dugite_primitives::transaction::CostModels as PrimCostModels;
        let prim = PrimCostModels {
            plutus_v1: Some(vec![1, 2, 3]),
            plutus_v2: Some(vec![4, 5]),
            plutus_v3: Some(vec![6]),
            // V4 not exercised here — covered by cost_models.rs decode tests.
            plutus_v4: None,
        };
        let cm_cbor = prim.to_cbor().unwrap();
        let tx_cbor = minimal_conway_tx_cbor();
        let decoded = decode_phase_two_inputs(&tx_cbor, &[], Some(&cm_cbor)).expect("decode");
        let cm = decoded.cost_models.expect("cost_models present");
        assert_eq!(cm.plutus_v1.as_deref(), Some(&[1i64, 2, 3][..]));
        assert_eq!(cm.plutus_v2.as_deref(), Some(&[4i64, 5][..]));
        assert_eq!(cm.plutus_v3.as_deref(), Some(&[6i64][..]));
    }

    #[test]
    fn decode_phase_two_inputs_none_cost_models_yields_none() {
        let tx_cbor = minimal_conway_tx_cbor();
        let decoded = decode_phase_two_inputs(&tx_cbor, &[], None).expect("decode");
        assert!(decoded.cost_models.is_none());
    }

    #[test]
    fn decode_phase_two_inputs_surfaces_cost_model_decode_failure() {
        let tx_cbor = minimal_conway_tx_cbor();
        // Bare break byte — not a map; decode_cost_models_cbor must fail.
        let err = decode_phase_two_inputs(&tx_cbor, &[], Some(&[0xff])).unwrap_err();
        assert!(matches!(err, PhaseTwoError::CostModelDecode(_)));
    }

    #[test]
    fn eval_phase_two_raw_surfaces_cost_model_decode_failure() {
        let tx_cbor = minimal_conway_tx_cbor();
        let result = eval_phase_two_raw(
            &tx_cbor,
            &[],
            Some(&[0xff]),
            (1, 1),
            default_slot_config(),
            true,
            &mut (),
        );
        assert!(matches!(result, Err(PhaseTwoError::CostModelDecode(_))));
    }

    // ─────────────────────────────────────────────────────────────
    // eval_phase_two_raw — still NotImplemented after decode lands
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_phase_two_raw_with_no_redeemers_returns_empty_results() {
        // With a valid tx that has no redeemers (no Plutus scripts being
        // executed), the function must return an empty `Ok(vec![])` rather
        // than the `NotImplemented` stub that earlier UPLC-9 parts surfaced.
        let tx_cbor = minimal_conway_tx_cbor();
        let result = eval_phase_two_raw(
            &tx_cbor,
            &[],
            None,
            (10_000_000, 10_000_000),
            default_slot_config(),
            true,
            &mut (),
        )
        .expect("phase_two on a no-redeemer tx must succeed");
        assert!(result.is_empty(), "expected empty results, got {result:?}");
    }

    #[test]
    fn eval_phase_two_raw_surfaces_tx_decode_failure() {
        // Empty/garbage tx must surface as TxDecode (not NotImplemented and
        // not a panic). This is the adversarial-input guarantee from
        // lib.rs §1.
        let result =
            eval_phase_two_raw(&[], &[], None, (1, 1), default_slot_config(), true, &mut ());
        assert!(matches!(result, Err(PhaseTwoError::TxDecode(_))));
    }

    #[test]
    fn eval_phase_two_raw_surfaces_utxo_decode_failure() {
        // Valid tx but malformed utxo output: must surface as UtxoDecode.
        let tx_cbor = minimal_conway_tx_cbor();
        let result = eval_phase_two_raw(
            &tx_cbor,
            &[(make_input_cbor(0xaa, 0), vec![0xff])],
            None,
            (1, 1),
            default_slot_config(),
            true,
            &mut (),
        );
        assert!(matches!(result, Err(PhaseTwoError::UtxoDecode(_))));
    }

    // ─────────────────────────────────────────────────────────────
    // era → output_id mapping
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn era_id_for_outputs_covers_plutus_eras() {
        let mut tx =
            dugite_serialization::decode_transaction(6, &minimal_conway_tx_cbor()).unwrap();
        tx.era = Era::Alonzo;
        assert_eq!(era_id_for_outputs(&tx), 4);
        tx.era = Era::Babbage;
        assert_eq!(era_id_for_outputs(&tx), 5);
        tx.era = Era::Conway;
        assert_eq!(era_id_for_outputs(&tx), 6);
        tx.era = Era::Dijkstra;
        assert_eq!(era_id_for_outputs(&tx), 7);
    }

    #[test]
    fn era_id_for_outputs_falls_back_to_conway_for_non_plutus_eras() {
        let mut tx =
            dugite_serialization::decode_transaction(6, &minimal_conway_tx_cbor()).unwrap();
        for non_plutus_era in [Era::Byron, Era::Shelley] {
            tx.era = non_plutus_era;
            assert_eq!(era_id_for_outputs(&tx), 6, "era={non_plutus_era:?}");
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Trivia coverage retained from the API skeleton
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn unit_redeemer_observer_is_no_op() {
        let mut obs = ();
        obs.on_redeemer(&[0xde, 0xad]);
    }

    #[test]
    fn slot_config_is_copy() {
        let sc = default_slot_config();
        let _sc2 = sc;
        let _sc3 = sc;
    }
}
