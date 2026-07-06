use crate::utxo::UtxoLookup;
#[cfg(test)]
use crate::utxo::UtxoSet;
use dugite_primitives::transaction::Transaction;
use tracing::{debug, trace};

#[derive(Debug, thiserror::Error)]
pub enum PlutusError {
    /// Retained for compatibility with existing call-sites.  The evaluator no
    /// longer fails on missing `tx.raw_cbor` — it re-encodes the in-memory
    /// `Transaction` deterministically (sorted-set inputs, Conway map-format
    /// redeemers) — but the variant is left in place so callers that
    /// explicitly match on it continue to compile.
    #[error("Missing raw CBOR for transaction")]
    MissingTxCbor,
    #[error("Missing raw CBOR for UTxO output: {0}")]
    MissingOutputCbor(String),
    #[error("Plutus evaluation failed: {0}")]
    EvalFailed(String),
    /// Phase-2 **collection/context** error — the script context could not
    /// be built or its inputs collected (UTxO/cost-model decode, missing
    /// script or datum, validity-interval time translation past the
    /// safe-zone horizon, internal translation errors).
    ///
    /// Mirrors Haskell `UtxosFailure (CollectErrors …)` (Babbage/Conway):
    /// raised by `collectTwoPhaseScriptInputs` BEFORE script evaluation and
    /// rejects the transaction — and any block containing it —
    /// **regardless** of the `is_valid` tag.  Unlike [`Self::EvalFailed`],
    /// this never legitimises `is_valid = false` (#733/#734).
    #[error("Phase-2 collection error (UtxosFailure CollectErrors): {0}")]
    CollectError(String),
    /// The dugite CEK **panicked** on this script (caught by
    /// `catch_unwind`). NOT a Haskell error class — a Haskell-validated
    /// chain CAN contain scripts that panic dugite's evaluator, so this
    /// must never be block-fatal at apply (warn-and-trust); at ADMISSION it
    /// rejects both `is_valid` polarities (reject-by-default on adversarial
    /// input, #733 correction 3 / #734).
    #[error("Phase-2 evaluator panic: {0}")]
    EvalPanic(String),
}

impl PlutusError {
    /// Whether this error is a phase-2 **collection/context** error
    /// (Haskell `UtxosFailure (CollectErrors …)`) rather than a genuine
    /// script-evaluation failure.
    ///
    /// Collection errors reject the transaction — and any block containing
    /// it — regardless of the `is_valid` tag; only [`Self::EvalFailed`]
    /// legitimises `is_valid = false` (#733/#734).  Missing-CBOR
    /// infrastructure errors are collect-class: they must never be mistaken
    /// for "scripts genuinely fail".
    pub fn is_collect_error(&self) -> bool {
        match self {
            PlutusError::CollectError(_)
            | PlutusError::MissingTxCbor
            | PlutusError::MissingOutputCbor(_) => true,
            // A CEK panic is NOT a Haskell CollectError — it must be
            // rejected at admission but stay warn-only at apply (#733
            // correction 3).
            PlutusError::EvalFailed(_) | PlutusError::EvalPanic(_) => false,
        }
    }

    /// Whether this error is a dugite CEK panic (see [`Self::EvalPanic`]).
    pub fn is_eval_panic(&self) -> bool {
        matches!(self, PlutusError::EvalPanic(_))
    }
}

/// Recover a printable message from a `catch_unwind` panic payload. The payload
/// is a `Box<dyn Any + Send>` whose concrete type is `&'static str` for `panic!`
/// with a string literal, `String` for `panic!` with a formatted message, or
/// otherwise opaque.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Slot configuration for Plutus time conversion
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SlotConfig {
    /// POSIX time of slot 0 in milliseconds
    pub zero_time: u64,
    /// Slot number at zero_time
    pub zero_slot: u64,
    /// Slot length in milliseconds
    pub slot_length: u32,
    /// Exclusive upper-bound slot past which slot→POSIX translation must
    /// fail with `TimeTranslationPastHorizon`. `None` means unbounded
    /// (only safe for tests; production callers MUST plumb the value
    /// computed by `EraHistory::safe_zone_horizon_slot(ledger_tip)`).
    ///
    /// Mirrors Haskell `Ouroboros.Consensus.HardFork.History.Qry.guardEnd`:
    /// `guard $ p b` where `p = \end -> absSlot < boundSlot end`.
    ///
    /// `#[serde(default)]` keeps wire/on-disk compatibility for any
    /// historic `SlotConfig` blobs that predate this field — those
    /// deserialize to `None` and the validation path then falls back to
    /// the unbounded (pre-fix) semantics, which is exactly what they
    /// had. Production code paths construct `SlotConfig` programmatically
    /// and set the field explicitly.
    #[serde(default)]
    pub safe_zone_horizon_slot: Option<u64>,
}

impl Default for SlotConfig {
    fn default() -> Self {
        // Cardano mainnet defaults
        SlotConfig {
            zero_time: 1_596_059_091_000, // Shelley start (mainnet)
            zero_slot: 4_492_800,         // First Shelley slot (mainnet)
            slot_length: 1_000,           // 1 second
            safe_zone_horizon_slot: None,
        }
    }
}

impl SlotConfig {
    /// Preview testnet slot config
    pub fn preview() -> Self {
        SlotConfig {
            zero_time: 1_666_656_000_000, // Preview genesis time
            zero_slot: 0,
            slot_length: 1_000,
            safe_zone_horizon_slot: None,
        }
    }

    /// Preprod testnet slot config
    pub fn preprod() -> Self {
        SlotConfig {
            zero_time: 1_654_041_600_000, // Preprod genesis time
            zero_slot: 0,
            slot_length: 1_000,
            safe_zone_horizon_slot: None,
        }
    }

    /// Return a new `SlotConfig` with the supplied safe-zone horizon
    /// installed. Use this just before calling `evaluate_plutus_scripts`
    /// to inject the value computed from the live ledger tip. Static
    /// network configuration (zero_time, zero_slot, slot_length) is
    /// untouched.
    #[must_use]
    pub fn with_safe_zone_horizon(mut self, horizon: u64) -> Self {
        self.safe_zone_horizon_slot = Some(horizon);
        self
    }
}

/// Encode a TransactionInput as CBOR bytes (wire format)
///
/// TransactionInput is encoded as a 2-element CBOR array: [hash(32 bytes), index(uint)]
fn encode_input_cbor(input: &dugite_primitives::transaction::TransactionInput) -> Vec<u8> {
    use minicbor::Encoder;
    let mut buf = Vec::with_capacity(40);
    let mut enc = Encoder::new(&mut buf);
    // minicbor encoding to Vec<u8> is infallible
    // Safety: minicbor encoding to Vec<u8> is infallible (cannot fail on memory writes)
    enc.array(2).expect("infallible: Vec<u8> write");
    enc.bytes(input.transaction_id.as_bytes())
        .expect("infallible: Vec<u8> write");
    enc.u32(input.index).expect("infallible: Vec<u8> write");
    buf
}

/// Build the standalone tx CBOR `[body, witness_set, is_valid, aux]` that the
/// phase-2 evaluator decodes, for a tx whose full `raw_cbor` was not preserved.
///
/// **Prefers the ORIGINAL `raw_body_cbor` + `raw_witness_cbor` wire bytes**
/// (captured per-tx by the block decoder) so that non-canonically-encoded
/// witness datums survive verbatim. Many on-chain datums use the general
/// `Constr` form (CBOR tag 102) for small constructor indices, which does NOT
/// round-trip through `encode_transaction` (it canonicalises to tag 121). A
/// datum hash is `blake2b_256` over the datum's *original* bytes, so a
/// re-encode changes the hash and phase-2 datum resolution fails with
/// "datum not found for V1/V2 spending redeemer" — the dominant phase-2
/// divergence during Alonzo sync (the Alonzo block decoder sets `raw_cbor =
/// None` but does capture `raw_body_cbor`/`raw_witness_cbor`).
///
/// Auxiliary data is emitted as CBOR `null` — phase-2 does not consume it (the
/// body's `auxiliary_data_hash` is already inside `raw_body_cbor`). Falls back
/// to a full `encode_transaction` re-encode only when the raw spans are
/// unavailable (e.g. a tx round-tripped through the LSM store, which drops the
/// `#[serde(skip)]` raw fields).
fn reassemble_phase_two_tx_cbor(tx: &Transaction) -> Vec<u8> {
    match (tx.raw_body_cbor.as_ref(), tx.raw_witness_cbor.as_ref()) {
        (Some(body), Some(wits)) => {
            let mut buf = Vec::with_capacity(body.len() + wits.len() + 4);
            buf.push(0x84); // array(4): [body, witness_set, is_valid, aux]
            buf.extend_from_slice(body);
            buf.extend_from_slice(wits);
            buf.push(if tx.is_valid { 0xf5 } else { 0xf4 }); // is_valid bool
            buf.push(0xf6); // auxiliary_data = null (unused by phase-2)
            buf
        }
        _ => dugite_serialization::encode_transaction(tx),
    }
}

/// A resolved, self-contained work item for parallel Phase-2 evaluation.
///
/// Created during the sequential per-tx pass (where UTxO input resolution must
/// happen in order because a tx may spend outputs produced by an earlier tx in
/// the same block). Once captured, the item carries everything `eval_phase_two_raw`
/// needs and is completely independent of the UTxO set — safe to `Send` across
/// rayon threads.
///
/// `tx_idx` preserves the original transaction order so that error reporting
/// after parallel evaluation stays deterministic (errors are re-sorted by
/// `tx_idx` before application).
#[derive(Debug)]
pub struct Phase2WorkItem {
    /// Original position of this transaction in the block (for deterministic
    /// error ordering after parallel evaluation).
    pub tx_idx: usize,
    /// The `is_valid` flag from the transaction (determines the post-eval check).
    pub is_valid: bool,
    /// Pre-assembled transaction CBOR (wire bytes or deterministic re-encoding).
    pub tx_cbor: Vec<u8>,
    /// Resolved `(input_cbor, output_cbor)` pairs — captured from the UTxO set
    /// at the sequential resolution point.
    pub utxo_pairs: Vec<(Vec<u8>, Vec<u8>)>,
    /// Serialised cost-model CBOR (from `params.cost_models.to_cbor()`).
    pub cost_models_cbor: Option<Vec<u8>>,
    /// `(cpu_steps, mem_units)` budget ceiling.
    pub max_ex: (u64, u64),
    /// Slot configuration for Plutus time translation.
    pub slot_config: SlotConfig,
    /// Major protocol version at evaluation time. Selects the PlutusV1/V2
    /// `BuiltinSemanticsVariant` (VariantA pre-Conway, VariantB at PV9+) for the
    /// per-builtin cost model — see `dugite_uplc::cost_apply`.
    pub protocol_major: u32,
    /// Whether ALL of the transaction's inputs (regular + reference +
    /// collateral) resolved against the UTxO set at capture time. `false`
    /// means a best-effort partial-replay state (UTxO gap) — phase-2
    /// errors from such items must NOT be block-fatal (#733 correction 4).
    pub utxo_complete: bool,
}

/// Resolve a transaction's Plutus inputs into `(input_cbor, output_cbor)` pairs
/// from the provided UTxO set.
///
/// This is the ordering-dependent step that must run sequentially during the
/// block-apply loop (a tx may spend outputs created by an earlier tx in the
/// same block). The returned pairs are completely self-contained and can be
/// passed to `eval_phase_two_raw` on any thread.
pub fn resolve_phase2_utxo_pairs(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut utxo_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    let all_inputs = tx.body.inputs.iter().chain(tx.body.reference_inputs.iter());
    for input in all_inputs {
        if let Some(output) = utxo_set.lookup(input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => dugite_serialization::encode_transaction_output(&output),
            };
            utxo_pairs.push((encode_input_cbor(input), output_cbor));
        }
    }
    for col_input in &tx.body.collateral {
        if let Some(output) = utxo_set.lookup(col_input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => dugite_serialization::encode_transaction_output(&output),
            };
            utxo_pairs.push((encode_input_cbor(col_input), output_cbor));
        }
    }
    utxo_pairs
}

/// Capture a [`Phase2WorkItem`] for a transaction during the sequential per-tx
/// pass without running the evaluator.
///
/// The caller is responsible for the ordering invariant: inputs must be
/// resolved while `utxo_set` still reflects the state *at this transaction's
/// apply point* (i.e., after all preceding txs in the block have been applied).
pub fn capture_phase2_work_item(
    tx_idx: usize,
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    cost_models_cbor: Option<Vec<u8>>,
    max_ex: (u64, u64),
    slot_config: SlotConfig,
    protocol_major: u32,
) -> Phase2WorkItem {
    let tx_cbor = match tx.raw_cbor.as_ref() {
        Some(bytes) => bytes.clone(),
        None => reassemble_phase_two_tx_cbor(tx),
    };
    let utxo_pairs = resolve_phase2_utxo_pairs(tx, utxo_set);
    // #733 correction 4: record whether every input resolved. A shortfall
    // means a UTxO gap (best-effort partial replay) — collection errors
    // from such items must stay warn-only at apply.
    let attempted =
        tx.body.inputs.len() + tx.body.reference_inputs.len() + tx.body.collateral.len();
    let utxo_complete = utxo_pairs.len() == attempted;
    Phase2WorkItem {
        tx_idx,
        is_valid: tx.is_valid,
        tx_cbor,
        utxo_pairs,
        cost_models_cbor,
        max_ex,
        slot_config,
        protocol_major,
        utxo_complete,
    }
}

/// When `DUGITE_PHASE2_DUMP_DIR` is set, write the exact CEK inputs of a tx to
/// `<dir>/phase2-divergence-tx<idx>-<txid>.json` so a Phase-2 evaluation
/// divergence (on-chain `is_valid=false` but dugite says the scripts pass — a
/// `ValidationTagMismatch`) can be reproduced byte-for-byte offline. No-op when
/// the env var is unset, so it is safe to leave wired in permanently.
pub fn maybe_dump_phase2_divergence(item: &Phase2WorkItem) {
    let dir = match std::env::var("DUGITE_PHASE2_DUMP_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
    let pairs: Vec<serde_json::Value> = item
        .utxo_pairs
        .iter()
        .map(|(i, o)| serde_json::json!({ "input": hex(i), "output": hex(o) }))
        .collect();
    let doc = serde_json::json!({
        "tx_idx": item.tx_idx,
        "is_valid": item.is_valid,
        "tx_cbor": hex(&item.tx_cbor),
        "utxo_pairs": pairs,
        "cost_models_cbor": item.cost_models_cbor.as_deref().map(&hex),
        "max_ex_cpu": item.max_ex.0,
        "max_ex_mem": item.max_ex.1,
        // Major protocol version at eval time — selects the PlutusV1/V2
        // BuiltinSemanticsVariant + cost-model variant. WITHOUT this an offline
        // replay must guess the version and can misclassify the divergence
        // (wrong variant -> spurious budget / builtin failures).
        "protocol_major": item.protocol_major,
        // dugite SlotConfig -> dugite_uplc SlotConfig mapping (see apply.rs).
        "sc_network_start_unix_seconds": item.slot_config.zero_time / 1_000,
        "sc_slot_zero_offset": item.slot_config.zero_slot,
        "sc_slot_length_ms": item.slot_config.slot_length,
        "sc_safe_zone_horizon_slot": item.slot_config.safe_zone_horizon_slot,
    });
    // Key the filename on a content hash of the tx so distinct divergences
    // across different blocks accumulate instead of overwriting each other
    // (the per-block `tx_idx` repeats every block). Same tx -> same file.
    let tx_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        item.tx_cbor.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let path = format!("{dir}/phase2-divergence-tx{}-{tx_key}.json", item.tx_idx);
    match serde_json::to_vec_pretty(&doc) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!(error = %e, path, "failed to write phase2 divergence repro");
            } else {
                tracing::warn!(path, tx_idx = item.tx_idx, "wrote phase2 divergence repro");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialise phase2 divergence repro"),
    }
}

/// Result of one parallel Phase-2 evaluation.
#[derive(Debug)]
pub struct Phase2Outcome {
    /// Original transaction index (matches [`Phase2WorkItem::tx_idx`]).
    pub tx_idx: usize,
    /// `is_valid` flag from the transaction (copied from work item for convenience).
    pub is_valid: bool,
    /// The evaluation result: `Ok(())` = all scripts pass, `Err(msg)` = failure.
    pub result: Result<(), PlutusError>,
    /// Whether ALL inputs resolved at capture time (see
    /// [`Phase2WorkItem::utxo_complete`]) — gates apply-time fatality
    /// (#733 correction 4).
    pub utxo_complete: bool,
}

/// Execute a batch of pre-resolved Phase-2 work items IN PARALLEL via rayon,
/// preserving result order by `tx_idx`.
///
/// The script-decode cache inside `dugite_uplc` is `thread_local` — each rayon
/// worker thread keeps its own cache, which is still correct (byte-exact identical
/// results, slightly less cache reuse across txs). This is the same semantics as
/// the existing sequential path.
///
/// The returned `Vec<Phase2Outcome>` is sorted by `tx_idx` so that error
/// application in the caller is deterministic regardless of rayon scheduling.
#[cfg(feature = "parallel-verification")]
pub fn run_phase2_parallel(items: Vec<Phase2WorkItem>) -> Vec<Phase2Outcome> {
    use rayon::prelude::*;
    let mut outcomes: Vec<Phase2Outcome> = items
        .into_par_iter()
        .map(|item| {
            let dugite_slot_config = dugite_uplc::phase_two::SlotConfig {
                network_start_unix_seconds: item.slot_config.zero_time / 1_000,
                slot_zero_offset: item.slot_config.zero_slot,
                slot_length_ms: item.slot_config.slot_length,
                safe_zone_horizon_slot: item.slot_config.safe_zone_horizon_slot,
            };
            let result = run_single_phase2_eval(
                &item.tx_cbor,
                &item.utxo_pairs,
                item.cost_models_cbor.as_deref(),
                item.max_ex,
                dugite_slot_config,
                item.protocol_major,
            );
            // Capture EITHER divergence direction for offline reproduction when
            // dumping is enabled: (a) dugite PASSES but on-chain is_valid=false
            // (ValidationTagMismatch, over-permissive CEK), or (b) dugite FAILS
            // but on-chain is_valid=true (over-strict CEK — e.g. the #22 unIData /
            // budget classes). Both are dugite-CEK bugs worth reproducing.
            if (result.is_ok() && !item.is_valid) || (result.is_err() && item.is_valid) {
                maybe_dump_phase2_divergence(&item);
            }
            Phase2Outcome {
                tx_idx: item.tx_idx,
                is_valid: item.is_valid,
                result,
                utxo_complete: item.utxo_complete,
            }
        })
        .collect();
    // Sort by tx_idx so error application order is deterministic.
    outcomes.sort_by_key(|o| o.tx_idx);
    outcomes
}

/// Evaluate one pooled Phase-2 work item into its `(block_idx, outcome)`.
///
/// Pure and self-contained (reads only the borrowed item), so it is safe to run
/// on any rayon worker or sequentially. Shared by the parallel and sequential
/// pooled paths so the per-item logic — including the divergence dump — lives in
/// exactly one place.
fn eval_phase2_item(block_idx: usize, item: &Phase2WorkItem) -> (usize, Phase2Outcome) {
    let dugite_slot_config = dugite_uplc::phase_two::SlotConfig {
        network_start_unix_seconds: item.slot_config.zero_time / 1_000,
        slot_zero_offset: item.slot_config.zero_slot,
        slot_length_ms: item.slot_config.slot_length,
        safe_zone_horizon_slot: item.slot_config.safe_zone_horizon_slot,
    };
    let result = run_single_phase2_eval(
        &item.tx_cbor,
        &item.utxo_pairs,
        item.cost_models_cbor.as_deref(),
        item.max_ex,
        dugite_slot_config,
        item.protocol_major,
    );
    if (result.is_ok() && !item.is_valid) || (result.is_err() && item.is_valid) {
        maybe_dump_phase2_divergence(item);
    }
    (
        block_idx,
        Phase2Outcome {
            tx_idx: item.tx_idx,
            is_valid: item.is_valid,
            result,
            utxo_complete: item.utxo_complete,
        },
    )
}

/// Default cap on CONCURRENT Plutus evaluations in the cross-block pooled flush.
///
/// Each in-flight CEK evaluation can hold up to roughly its declared
/// `maxTxExUnits` mem budget worth of live term/environment data (hundreds of MB
/// for a max-budget script), so peak RSS during a pooled flush is
/// ≈ concurrency × per-eval peak. The original pooled path ran ONE rayon batch
/// across EVERY core (`into_par_iter` on the global pool); in a dense-Plutus
/// region that produced an ~11-core × ~1.2 GB ≈ 13.5 GB runaway that froze block
/// apply (the deferral-soak wedge). Capping concurrency bounds peak RSS while
/// still filling several cores. Tunable via `DUGITE_PHASE2_POOL_THREADS`;
/// default = min(cores − 2, this).
const PHASE2_POOL_DEFAULT_MAX_THREADS: usize = 6;

/// Per-chunk work-item multiple (× pool width). Chunking gives the pooled flush
/// a cancellation checkpoint between chunks and bounds the live outcome buffer,
/// without starving the pool's work-stealing depth.
const PHASE2_POOL_CHUNK_FACTOR: usize = 4;

/// Resolve the pooled-flush concurrency cap (see [`PHASE2_POOL_DEFAULT_MAX_THREADS`]).
fn phase2_pool_max_threads() -> usize {
    if let Ok(v) = std::env::var("DUGITE_PHASE2_POOL_THREADS") {
        if let Ok(n) = v.parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    cores
        .saturating_sub(2)
        .clamp(1, PHASE2_POOL_DEFAULT_MAX_THREADS)
}

/// Process-wide bounded rayon pool for the cross-block pooled flush. Built once
/// (its width is fixed by [`phase2_pool_max_threads`] at first use); `None` only
/// if the pool cannot be created (resource exhaustion), in which case the caller
/// runs the chunks sequentially.
#[cfg(feature = "parallel-verification")]
fn phase2_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(phase2_pool_max_threads())
            .thread_name(|i| format!("phase2-pool-{i}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// Cross-block pooled Phase-2 evaluation — the CPU-saturation primitive for
/// bulk sync.
///
/// A single block carries only ~2-3 redeemers on the median preview Conway
/// block, so its `into_par_iter` cannot fill a 12-core host. This concatenates
/// the work items of MANY blocks (all redeemers fan across the pool at once),
/// then regroups the outcomes back per input block, preserving each block's
/// internal `tx_idx` ordering.
///
/// **Memory-bounded.** Rather than fanning every redeemer across the global
/// rayon pool at once (which produced the deferral-soak RSS runaway), this runs
/// on a dedicated pool capped at [`phase2_pool_max_threads`] and processes the
/// flattened items in chunks, so peak concurrent CEK memory stays bounded.
///
/// `batches[i]` are block `i`'s [`Phase2WorkItem`]s (from
/// [`crate::state::LedgerState::apply_block_defer_phase2`]); the returned
/// `Vec<Vec<Phase2Outcome>>` is aligned 1:1 with `batches` and each inner vec is
/// sorted by `tx_idx`, so feeding `result[i]` to
/// [`crate::state::apply_phase2_outcomes`] with block `i` reproduces the exact
/// per-block fatality decision the serial path would make.
#[cfg(feature = "parallel-verification")]
pub fn run_phase2_parallel_pooled(batches: Vec<Vec<Phase2WorkItem>>) -> Vec<Vec<Phase2Outcome>> {
    // Never-cancel: the offline / bench path always runs to completion.
    run_phase2_pooled_inner(batches, &|| false)
        .expect("pooled phase-2 eval cannot be cancelled with a never-cancel token")
}

/// Cancellable + memory-bounded variant for the live deferral flush.
///
/// Returns `None` if `cancel()` observed `true` between chunks (shutdown in
/// progress) before all items were evaluated — the caller MUST treat the window
/// as NOT confirmed (roll back / retry) and never apply a partial result. On
/// success returns per-block outcomes aligned 1:1 with `batches`.
#[cfg(feature = "parallel-verification")]
pub fn run_phase2_parallel_pooled_cancellable(
    batches: Vec<Vec<Phase2WorkItem>>,
    cancel: &dyn Fn() -> bool,
) -> Option<Vec<Vec<Phase2Outcome>>> {
    run_phase2_pooled_inner(batches, cancel)
}

#[cfg(feature = "parallel-verification")]
fn run_phase2_pooled_inner(
    batches: Vec<Vec<Phase2WorkItem>>,
    cancel: &dyn Fn() -> bool,
) -> Option<Vec<Vec<Phase2Outcome>>> {
    use rayon::prelude::*;

    let n_blocks = batches.len();

    // Flatten to (block_idx, item) so every redeemer is a single work unit.
    // Consume `batches` by value (move, no clone) since items are self-contained.
    let mut flat: Vec<(usize, Phase2WorkItem)> = Vec::new();
    for (block_idx, items) in batches.into_iter().enumerate() {
        for item in items {
            flat.push((block_idx, item));
        }
    }

    let pool = phase2_pool();
    let chunk_items = phase2_pool_max_threads()
        .saturating_mul(PHASE2_POOL_CHUNK_FACTOR)
        .max(1);

    // Process in bounded chunks: a cancellation checkpoint between chunks (one
    // chunk = bounded latency) and a bounded live outcome buffer. Concurrency
    // within a chunk is capped by the dedicated pool, which is what bounds RSS.
    let mut tagged: Vec<(usize, Phase2Outcome)> = Vec::with_capacity(flat.len());
    let mut start = 0usize;
    while start < flat.len() {
        if cancel() {
            return None;
        }
        let end = (start + chunk_items).min(flat.len());
        let chunk = &flat[start..end];
        let mut chunk_out: Vec<(usize, Phase2Outcome)> = match pool {
            Some(p) => p.install(|| {
                chunk
                    .par_iter()
                    .map(|(block_idx, item)| eval_phase2_item(*block_idx, item))
                    .collect()
            }),
            // Pool unavailable (build failed): sequential, still cancellable
            // per chunk.
            None => chunk
                .iter()
                .map(|(block_idx, item)| eval_phase2_item(*block_idx, item))
                .collect(),
        };
        tagged.append(&mut chunk_out);
        start = end;
    }

    // Regroup by block index, one inner vec per input block, sorted by
    // (block_idx, tx_idx) so each block's outcomes land in tx order — identical
    // to run_phase2_parallel's per-block sort_by_key(tx_idx).
    let mut grouped: Vec<Vec<Phase2Outcome>> = (0..n_blocks).map(|_| Vec::new()).collect();
    tagged.sort_by_key(|(block_idx, o)| (*block_idx, o.tx_idx));
    for (block_idx, outcome) in tagged {
        grouped[block_idx].push(outcome);
    }
    Some(grouped)
}

/// Sequential fallback (feature gate off).
#[cfg(not(feature = "parallel-verification"))]
pub fn run_phase2_parallel_pooled(batches: Vec<Vec<Phase2WorkItem>>) -> Vec<Vec<Phase2Outcome>> {
    batches.into_iter().map(run_phase2_parallel).collect()
}

/// Cancellable sequential fallback (feature gate off).
#[cfg(not(feature = "parallel-verification"))]
pub fn run_phase2_parallel_pooled_cancellable(
    batches: Vec<Vec<Phase2WorkItem>>,
    cancel: &dyn Fn() -> bool,
) -> Option<Vec<Vec<Phase2Outcome>>> {
    let mut grouped: Vec<Vec<Phase2Outcome>> = Vec::with_capacity(batches.len());
    for items in batches {
        if cancel() {
            return None;
        }
        grouped.push(run_phase2_parallel(items));
    }
    Some(grouped)
}

/// Sequential fallback (feature gate off).
#[cfg(not(feature = "parallel-verification"))]
pub fn run_phase2_parallel(items: Vec<Phase2WorkItem>) -> Vec<Phase2Outcome> {
    let mut outcomes: Vec<Phase2Outcome> = items
        .into_iter()
        .map(|item| {
            let dugite_slot_config = dugite_uplc::phase_two::SlotConfig {
                network_start_unix_seconds: item.slot_config.zero_time / 1_000,
                slot_zero_offset: item.slot_config.zero_slot,
                slot_length_ms: item.slot_config.slot_length,
                safe_zone_horizon_slot: item.slot_config.safe_zone_horizon_slot,
            };
            let result = run_single_phase2_eval(
                &item.tx_cbor,
                &item.utxo_pairs,
                item.cost_models_cbor.as_deref(),
                item.max_ex,
                dugite_slot_config,
                item.protocol_major,
            );
            if (result.is_ok() && !item.is_valid) || (result.is_err() && item.is_valid) {
                maybe_dump_phase2_divergence(&item);
            }
            Phase2Outcome {
                tx_idx: item.tx_idx,
                is_valid: item.is_valid,
                result,
                utxo_complete: item.utxo_complete,
            }
        })
        .collect();
    outcomes.sort_by_key(|o| o.tx_idx);
    outcomes
}

/// Core single-tx Phase-2 evaluation — operates on pre-resolved CBOR bytes.
///
/// Shared by both `evaluate_plutus_scripts` (sequential path) and
/// `run_phase2_parallel` (deferred-parallel path).  Wrapped in `catch_unwind`
/// as defense-in-depth against adversarial Plutus scripts.
fn run_single_phase2_eval(
    tx_cbor: &[u8],
    utxo_pairs: &[(Vec<u8>, Vec<u8>)],
    cost_models_cbor: Option<&[u8]>,
    max_tx_ex_units: (u64, u64),
    slot_config: dugite_uplc::phase_two::SlotConfig,
    protocol_major: u32,
) -> Result<(), PlutusError> {
    let eval_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dugite_uplc::phase_two::eval_phase_two_raw(
            tx_cbor,
            utxo_pairs,
            cost_models_cbor,
            max_tx_ex_units,
            slot_config,
            protocol_major,
            false, // run_phase_one = false
            &mut (),
        )
    }));
    let eval_result = match eval_outcome {
        Ok(r) => r,
        Err(payload) => {
            let msg = panic_payload_to_string(&payload);
            // Reject-by-default at ADMISSION: a panic on adversarial input
            // is not a legitimate script failure, so it must never
            // legitimise is_valid=false (#734). Distinct from CollectError
            // so the APPLY path stays warn-and-trust — a Haskell-validated
            // chain can contain scripts that panic dugite's CEK (#733
            // correction 3).
            return Err(PlutusError::EvalPanic(format!(
                "dugite-uplc panic on malformed script: {msg}"
            )));
        }
    };
    match eval_result {
        Ok(results) => {
            for r in &results {
                trace!(
                    cpu = r.consumed.cpu,
                    mem = r.consumed.mem,
                    "Plutus script passed (parallel)"
                );
            }
            Ok(())
        }
        Err(e) => {
            let error_msg = match &e {
                dugite_uplc::phase_two::PhaseTwoError::ScriptEvaluationFailedWithLogs {
                    error,
                    logs,
                } => {
                    let joined = logs
                        .iter()
                        .map(|s| format!("[{s}]"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("eval_phase_two_raw error: {error}; Trace logs: {joined}")
                }
                _ => format!("eval_phase_two_raw error: {e}"),
            };
            // Partition per the Haskell phase-2 semantics: genuine script
            // evaluation failures (`evalScripts` → Fails) keep EvalFailed;
            // collection/context errors (`CollectErrors`: decode, missing
            // script/datum, past-horizon time translation) become
            // CollectError and reject regardless of is_valid (#733/#734).
            if e.is_script_evaluation_failure() {
                Err(PlutusError::EvalFailed(error_msg))
            } else {
                Err(PlutusError::CollectError(error_msg))
            }
        }
    }
}

/// Evaluate Plutus scripts in a transaction using the uplc CEK machine
///
/// `max_tx_ex_units` is `(cpu_steps, mem_units)` — this matches the uplc
/// `eval_phase_two_raw` convention where `.0 = cpu` and `.1 = mem`.
/// Callers must ensure they pass `(ExUnits.steps, ExUnits.mem)` in that order;
/// swapping the two produces a 700x too-small CPU ceiling and causes false failures.
///
/// Returns Ok(()) if all scripts pass, or Err with details of failure.
pub fn evaluate_plutus_scripts(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    cost_models_cbor: Option<&[u8]>,
    max_tx_ex_units: (u64, u64),
    slot_config: &SlotConfig,
    protocol_major: u32,
) -> Result<(), PlutusError> {
    // Use the original wire bytes when available — that path preserves the
    // exact TxId of network-submitted transactions (which is the hash of the
    // bytes we received, not of any re-encoding).
    //
    // For locally-built transactions (mempool, tests, anything that hasn't
    // round-tripped through CBOR yet) we re-encode deterministically here.
    // This is safe because:
    //   - `eval_phase_two_raw` only uses the raw bytes to *decode* into a
    //     `MintedTx`; it does not hash them itself.
    //   - The `TxInfo.id` field that Plutus scripts observe is
    //     `KeepRaw::original_hash()` of the body bytes, which becomes the
    //     hash of our re-encoding — matching whatever the caller will treat
    //     as this tx's hash, since they have not yet committed to one.
    //   - `encode_transaction_body_for_era` sorts set fields lexicographically
    //     (Conway tag 258), so redeemer pointer indices into `inputs`,
    //     `mint`, etc. resolve to the same positions the evaluator computes.
    //   - `encode_witness_set_for_era` emits Conway map-format redeemers,
    //     matching `compute_script_data_hash` so script integrity stays
    //     consistent.
    //
    // Reference: see the in-house `KeepRaw<TransactionBody>` and Haskell
    // `MemoBytes`/`SafeHash` — both capture original CBOR on decode and hash
    // those exact bytes for the TxId.
    let owned_cbor: Vec<u8>;
    let tx_cbor: &[u8] = match tx.raw_cbor.as_ref() {
        Some(bytes) => bytes.as_slice(),
        None => {
            owned_cbor = reassemble_phase_two_tx_cbor(tx);
            &owned_cbor
        }
    };

    // Build resolved UTxO pairs (input CBOR, output CBOR)
    let mut utxo_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    // Collect all inputs that need resolution: regular inputs + reference inputs
    let all_inputs = tx.body.inputs.iter().chain(tx.body.reference_inputs.iter());

    for input in all_inputs {
        if let Some(output) = utxo_set.lookup(input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => {
                    // raw_cbor is None when the UTxO was round-tripped through
                    // the LSM store (serde(skip) on raw_cbor). Re-encode the
                    // output from its parsed fields.
                    dugite_serialization::encode_transaction_output(&output)
                }
            };
            let input_cbor = encode_input_cbor(input);
            utxo_pairs.push((input_cbor, output_cbor));
        }
    }

    // Also resolve collateral inputs
    for col_input in &tx.body.collateral {
        if let Some(output) = utxo_set.lookup(col_input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => dugite_serialization::encode_transaction_output(&output),
            };
            let input_cbor = encode_input_cbor(col_input);
            utxo_pairs.push((input_cbor, output_cbor));
        }
    }

    debug!(
        tx_hash = %tx.hash.to_hex(),
        utxo_count = utxo_pairs.len(),
        redeemer_count = tx.witness_set.redeemers.len(),
        "Evaluating Plutus scripts"
    );

    let dugite_slot_config = dugite_uplc::phase_two::SlotConfig {
        network_start_unix_seconds: slot_config.zero_time / 1_000,
        slot_zero_offset: slot_config.zero_slot,
        slot_length_ms: slot_config.slot_length,
        safe_zone_horizon_slot: slot_config.safe_zone_horizon_slot,
    };
    let result = run_single_phase2_eval(
        tx_cbor,
        &utxo_pairs,
        cost_models_cbor,
        max_tx_ex_units,
        dugite_slot_config,
        protocol_major,
    );
    if let Err(ref e) = result {
        debug!(
            tx_hash = %tx.hash.to_hex(),
            error = %e,
            "Plutus evaluation error"
        );
    }
    result
}

/// Per-redeemer report extracted from a Phase-2 evaluation. Mirrors
/// `dugite_uplc::phase_two::RedeemerResult` but lives in `dugite-ledger`
/// so callers don't need to depend on the UPLC crate directly.
#[derive(Debug, Clone)]
pub struct RedeemerReport {
    pub tag: dugite_primitives::transaction::RedeemerTag,
    pub index: u32,
    pub ex_units_cpu: u64,
    pub ex_units_mem: u64,
    pub logs: Vec<String>,
}

/// Run Phase-2 evaluation and surface per-redeemer reports.
///
/// Same semantics as [`evaluate_plutus_scripts`] (declared-budget
/// enforcement, slot-config translation, panic-safe). The only
/// difference is that this variant exposes the typed
/// `dugite_uplc::phase_two::RedeemerResult` list back to the caller so
/// `SubmitService.EvalTx` can populate per-redeemer
/// `ex_units` / `traces` / `errors` fields on the wire.
pub fn evaluate_plutus_scripts_with_reports(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    cost_models_cbor: Option<&[u8]>,
    max_tx_ex_units: (u64, u64),
    slot_config: &SlotConfig,
    protocol_major: u32,
) -> Result<Vec<RedeemerReport>, PlutusError> {
    let owned_cbor: Vec<u8>;
    let tx_cbor: &[u8] = match tx.raw_cbor.as_ref() {
        Some(bytes) => bytes.as_slice(),
        None => {
            owned_cbor = reassemble_phase_two_tx_cbor(tx);
            &owned_cbor
        }
    };

    let mut utxo_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let all_inputs = tx.body.inputs.iter().chain(tx.body.reference_inputs.iter());
    for input in all_inputs {
        if let Some(output) = utxo_set.lookup(input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => dugite_serialization::encode_transaction_output(&output),
            };
            utxo_pairs.push((encode_input_cbor(input), output_cbor));
        }
    }
    for col_input in &tx.body.collateral {
        if let Some(output) = utxo_set.lookup(col_input) {
            let output_cbor = match &output.raw_cbor {
                Some(cbor) => cbor.clone(),
                None => dugite_serialization::encode_transaction_output(&output),
            };
            utxo_pairs.push((encode_input_cbor(col_input), output_cbor));
        }
    }

    let dugite_slot_config = dugite_uplc::phase_two::SlotConfig {
        network_start_unix_seconds: slot_config.zero_time / 1_000,
        slot_zero_offset: slot_config.zero_slot,
        slot_length_ms: slot_config.slot_length,
        // Plumb the safe-zone horizon into the inner evaluator so its
        // `slot_to_posix_ms` rejects past-horizon validity bounds with
        // `TimeTranslationPastHorizon`. When the caller leaves the field
        // unset (tests, legacy on-disk SlotConfig), the evaluator falls
        // back to its pre-fix unbounded semantics.
        safe_zone_horizon_slot: slot_config.safe_zone_horizon_slot,
    };
    let eval_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dugite_uplc::phase_two::eval_phase_two_raw(
            tx_cbor,
            &utxo_pairs,
            cost_models_cbor,
            max_tx_ex_units,
            dugite_slot_config,
            protocol_major,
            false,
            &mut (),
        )
    }));
    let results = match eval_outcome {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            // Same partition as `run_single_phase2_eval` (#733/#734).
            let msg = format!("eval_phase_two_raw error: {e}");
            return Err(if e.is_script_evaluation_failure() {
                PlutusError::EvalFailed(msg)
            } else {
                PlutusError::CollectError(msg)
            });
        }
        Err(payload) => {
            return Err(PlutusError::CollectError(format!(
                "dugite-uplc panic on malformed script: {}",
                panic_payload_to_string(&payload)
            )));
        }
    };
    Ok(results
        .into_iter()
        .map(|r| RedeemerReport {
            tag: r.tag,
            index: r.index,
            ex_units_cpu: r.consumed.cpu.max(0) as u64,
            ex_units_mem: r.consumed.mem.max(0) as u64,
            logs: r.logs,
        })
        .collect())
}

/// Check if a transaction contains any Plutus scripts (in witnesses or reference inputs)
pub fn has_plutus_scripts(tx: &Transaction) -> bool {
    !tx.witness_set.plutus_v1_scripts.is_empty()
        || !tx.witness_set.plutus_v2_scripts.is_empty()
        || !tx.witness_set.plutus_v3_scripts.is_empty()
        || !tx.witness_set.redeemers.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;

    fn hexd(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Phase-2 must be fed the ORIGINAL tx wire bytes, not a re-encode. Real
    /// mainnet Alonzo tx 3cb59645… carries a witness datum encoded in the
    /// general `Constr` form (CBOR tag 102) whose hash is `a44f0b1a…`. When the
    /// tx is round-tripped through `encode_transaction` (the old fallback) that
    /// datum is canonicalised to tag 121, its hash changes, and phase-2 datum
    /// resolution fails ("datum not found"). `reassemble_phase_two_tx_cbor` must
    /// preserve the original bytes via `raw_body_cbor`/`raw_witness_cbor` so the
    /// datum hash survives.
    #[test]
    fn reassemble_phase_two_preserves_noncanonical_datum() {
        let needed = "a44f0b1a7efd4d212e5dad5bcff2050be0e309840bfafc3b716aac7bc48e9ab7";
        let tx_cbor = hexd(include_str!("../test_data/phase2_datum_tag102_tx.hex"));
        // Decode standalone; the decoder captures raw_body_cbor + raw_witness_cbor.
        let mut tx = dugite_serialization::decode_transaction(6, &tx_cbor).unwrap();
        assert!(tx.raw_body_cbor.is_some() && tx.raw_witness_cbor.is_some());
        // Simulate the live block-apply path where the full raw_cbor is absent.
        tx.raw_cbor = None;

        let datum_present = |cbor: &[u8]| -> bool {
            let tx2 = dugite_serialization::decode_transaction(6, cbor).unwrap();
            let raw = tx2
                .witness_set
                .raw_plutus_data_cbor
                .expect("plutus_data present");
            dugite_serialization::plutus_data_element_spans(&raw)
                .unwrap()
                .iter()
                .any(|s| dugite_primitives::hash::blake2b_256(s).to_hex() == needed)
        };

        // The fix: reassembly from original body+witness preserves the datum.
        assert!(
            datum_present(&reassemble_phase_two_tx_cbor(&tx)),
            "reassembled tx must preserve the tag-102 datum hash"
        );
        // The bug it replaces: a full re-encode canonicalises and loses it.
        assert!(
            !datum_present(&dugite_serialization::encode_transaction(&tx)),
            "encode_transaction must mangle the non-canonical datum (proves the bug)"
        );
    }

    /// The memory-bounded + chunked pooled flush must be byte-for-byte
    /// equivalent to the established per-block sequential-parallel path: each
    /// item's evaluation is independent, and the regroup re-sorts by
    /// `(block_idx, tx_idx)`, so the dedicated capped pool + chunking changes
    /// only WHEN/WHERE an eval runs, never its outcome. This pins that invariant
    /// (the deferral byte-exactness guarantee) and the cancellation contract.
    #[test]
    fn pooled_phase2_equals_sequential_reference_and_honours_cancel() {
        let tx_cbor = hexd(include_str!("../test_data/phase2_datum_tag102_tx.hex"));
        let mk_item = |tx_idx: usize, is_valid: bool| Phase2WorkItem {
            tx_idx,
            is_valid,
            tx_cbor: tx_cbor.clone(),
            // No resolved inputs: every eval fails fast + deterministically. We
            // are testing scheduling-invariance, which holds for ANY deterministic
            // per-item function regardless of the Ok/Err verdict.
            utxo_pairs: Vec::new(),
            cost_models_cbor: None,
            max_ex: (10_000_000_000, 14_000_000),
            slot_config: SlotConfig {
                zero_time: 1_596_059_091_000,
                zero_slot: 4_492_800,
                slot_length: 1_000,
                safe_zone_horizon_slot: None,
            },
            protocol_major: 9,
            utxo_complete: false,
        };
        // Fresh blocks each call (Phase2WorkItem is not Clone). Includes an empty
        // block and a block larger than a chunk so flatten/regroup + the chunk
        // boundary are exercised.
        let mk_blocks = || -> Vec<Vec<Phase2WorkItem>> {
            vec![
                vec![mk_item(0, true), mk_item(1, false)],
                Vec::new(),
                vec![mk_item(0, true)],
                (0..40).map(|i| mk_item(i, i % 2 == 0)).collect(),
            ]
        };
        let proj = |groups: Vec<Vec<Phase2Outcome>>| -> Vec<Vec<(usize, bool, bool, bool)>> {
            groups
                .into_iter()
                .map(|g| {
                    g.into_iter()
                        .map(|o| (o.tx_idx, o.is_valid, o.result.is_ok(), o.utxo_complete))
                        .collect()
                })
                .collect()
        };

        // Reference: per-block sequential-parallel (the path the pooled flush
        // must reproduce).
        let reference: Vec<Vec<(usize, bool, bool, bool)>> = mk_blocks()
            .into_iter()
            .map(|b| {
                run_phase2_parallel(b)
                    .into_iter()
                    .map(|o| (o.tx_idx, o.is_valid, o.result.is_ok(), o.utxo_complete))
                    .collect()
            })
            .collect();

        // Pooled (memory-bounded, chunked) must align 1:1 and match exactly.
        let pooled = run_phase2_parallel_pooled(mk_blocks());
        assert_eq!(
            pooled.len(),
            4,
            "outcome groups align 1:1 with input blocks"
        );
        assert_eq!(
            proj(pooled),
            reference,
            "pooled outcomes must equal the sequential reference exactly"
        );

        // Cancellable: never-cancel == reference; always-cancel == None (the
        // window is NOT confirmed, never partially applied).
        let never = run_phase2_parallel_pooled_cancellable(mk_blocks(), &|| false)
            .expect("never-cancel returns Some");
        assert_eq!(proj(never), reference, "never-cancel matches the reference");
        assert!(
            run_phase2_parallel_pooled_cancellable(mk_blocks(), &|| true).is_none(),
            "always-cancel returns None so the caller cannot apply a partial window"
        );
    }

    #[test]
    fn test_encode_input_cbor() {
        use dugite_primitives::transaction::TransactionInput;

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xab; 32]),
            index: 1,
        };
        let cbor = encode_input_cbor(&input);
        // Should be a valid CBOR array with 2 elements
        assert!(!cbor.is_empty());
        // First byte should be 0x82 (array of 2)
        assert_eq!(cbor[0], 0x82);
    }

    #[test]
    fn test_slot_config_defaults() {
        let config = SlotConfig::default();
        assert_eq!(config.slot_length, 1_000);
        assert_eq!(config.zero_slot, 4_492_800);

        let preview = SlotConfig::preview();
        assert_eq!(preview.zero_slot, 0);
    }

    #[test]
    fn test_has_plutus_scripts_empty() {
        let tx = Transaction::empty_with_hash(Hash32::ZERO);
        assert!(!has_plutus_scripts(&tx));
    }

    #[test]
    fn test_has_plutus_scripts_with_redeemers() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::transaction::{ExUnits, PlutusData, Redeemer, RedeemerTag};

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.witness_set.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(num_bigint::BigInt::from(0i64)),
            ex_units: ExUnits {
                mem: 100,
                steps: 100,
            },
        });
        assert!(has_plutus_scripts(&tx));
    }

    #[test]
    fn test_evaluate_missing_cbor_falls_back_to_re_encoding() {
        // Locally-built tx (raw_cbor = None) with no redeemers must succeed:
        // the evaluator re-encodes the in-memory `Transaction` deterministically
        // and the empty witness set produces zero redeemers to evaluate.
        let tx = Transaction::empty_with_hash(Hash32::ZERO);
        assert!(tx.raw_cbor.is_none(), "precondition: locally-built tx");
        let utxo_set = UtxoSet::new();
        let slot_config = SlotConfig::default();

        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (10_000_000, 10_000_000),
            &slot_config,
            9,
        );
        assert!(
            result.is_ok(),
            "Evaluator must accept a locally-built tx via the re-encode fallback: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Plutus V1/V2/V3 script execution test vectors
    //
    // These tests verify the behaviour of `evaluate_plutus_scripts` using
    // minimal hand-crafted UPLC programs:
    //
    // - always-succeeds: a Plutus V2 spending validator that immediately
    //   returns Unit regardless of datum, redeemer, or script context.
    // - always-fails:    a program whose body is the UPLC error term; any
    //   execution attempt terminates with a machine error.
    // - budget exhaustion: declared ExUnits well below the actual cost,
    //   verifying the CPU budget enforcement logic.
    //
    // Script bytecode is derived directly from the UPLC AST (via the uplc
    // parser → DeBruijn conversion → flat-encoded CBOR), so there is no
    // dependency on external compiled artefacts or pre-baked hex vectors.
    //
    // The full Conway-era CBOR transaction used as `raw_cbor` is assembled
    // manually using `minicbor::Encoder`.  Since `evaluate_plutus_scripts`
    // calls `eval_phase_two_raw` with `run_phase_one = false`, the
    // script_data_hash field in the body is intentionally omitted; the legacy decoder
    // will parse the transaction, but uplc will not re-validate structural
    // rules that our own Phase-1 pass already enforces.
    // -----------------------------------------------------------------------

    /// Build the CBOR bytes for a Plutus script (the format stored in the
    /// transaction witness set).
    ///
    /// A Plutus script in the witness set is `CBOR_bytes(flat_encoded_program)`:
    /// `dugite_uplc::Program::to_cbor()` produces exactly this format.
    ///
    /// We map the small set of UPLC source patterns the test suite uses
    /// to direct `dugite_uplc::Program` constructions. The original
    /// implementation parsed a textual `(program …)` via aiken's
    /// `uplc::parser`; dugite-uplc does not ship a textual parser
    /// (production never needs one — scripts arrive as CBOR-flat on
    /// the wire), so tests express the equivalent `Term` AST directly
    /// here.
    fn build_script_cbor(uplc_src: &str) -> Vec<u8> {
        use dugite_uplc::term::{Constant, Term};
        use dugite_uplc::Program;
        use std::rc::Rc;
        let (version, term) = match uplc_src {
            // V1/V2 always-succeeds: 3 lambdas around a Unit constant.
            "(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))" => (
                dugite_uplc::program::Program::version_triple(1, 0, 0),
                Term::Lam(Rc::new(Term::Lam(Rc::new(Term::Lam(Rc::new(
                    Term::Const(Constant::Unit),
                )))))),
            ),
            // V3 always-succeeds: 1 lambda around a Unit constant.
            "(program 1.1.0 (lam _ (con unit ())))" => (
                dugite_uplc::program::Program::version_triple(1, 1, 0),
                Term::Lam(Rc::new(Term::Const(Constant::Unit))),
            ),
            // V2 single-arg "always-succeeds" (used by tests that confirm
            // the V3 Unit-check doesn't bleed into V2 evaluation).
            "(program 1.0.0 (lam _ (con unit ())))" => (
                dugite_uplc::program::Program::version_triple(1, 0, 0),
                Term::Lam(Rc::new(Term::Const(Constant::Unit))),
            ),
            // V3 returns integer 42 — used by tests that verify the V3
            // non-Unit-return rejection path.
            "(program 1.1.0 (lam _ (con integer 42)))" => (
                dugite_uplc::program::Program::version_triple(1, 1, 0),
                Term::Lam(Rc::new(Term::Const(Constant::Integer(42.into())))),
            ),
            // V1/V2 returns integer 42 — used by tests that verify
            // non-Error returns are accepted for V1/V2.
            "(program 1.0.0 (lam _ (lam _ (lam _ (con integer 42)))))" => (
                dugite_uplc::program::Program::version_triple(1, 0, 0),
                Term::Lam(Rc::new(Term::Lam(Rc::new(Term::Lam(Rc::new(
                    Term::Const(Constant::Integer(42.into())),
                )))))),
            ),
            // V1/V2 always-fails.
            "(program 1.0.0 (error))" => (
                dugite_uplc::program::Program::version_triple(1, 0, 0),
                Term::Error,
            ),
            other => panic!("build_script_cbor: unsupported test script: {other:?}"),
        };
        let program = Program { version, term };
        program.to_cbor().expect("CBOR encode failed")
    }

    /// Compute the PlutusV2 script hash for a script in witness-set encoding.
    ///
    /// The hash is `blake2b_224(0x02 || script_cbor_bytes)`, matching the rule
    /// used by `collect_available_script_hashes` and `compute_script_ref_hash`.
    fn script_hash_v2(script_cbor: &[u8]) -> [u8; 28] {
        let mut tagged = Vec::with_capacity(1 + script_cbor.len());
        tagged.push(0x02u8);
        tagged.extend_from_slice(script_cbor);
        *dugite_primitives::hash::blake2b_224(&tagged).as_bytes()
    }

    /// Return CBOR-encoded PlutusV2 cost model bytes for use with
    /// `evaluate_plutus_scripts`.
    ///
    /// These are the standard Vasil (Babbage) era PlutusV2 cost model entries
    /// (178 coefficients), taken verbatim from the uplc integration test suite.
    /// Having a non-None cost model causes `eval_phase_two_raw` to enforce the
    /// declared ExUnits budget via `Program::eval_as(..., Some(initial_budget))`,
    /// instead of silently using an unconstrained `ExBudget::default()`.
    fn vasil_v2_cost_models_cbor() -> Vec<u8> {
        // Standard Vasil (Babbage) era PlutusV2 cost model: 178 entries.
        // Source: uplc-1.1.21 integration test suite (`tx/tests.rs`).
        // Encoded as a CBOR map {1: [i64; 178]}.
        let v2_costs: &[i64] = &[
            205665,
            812,
            1,
            1,
            1000,
            571,
            0,
            1,
            1000,
            24177,
            4,
            1,
            1000,
            32,
            117366,
            10475,
            4,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            100,
            100,
            23000,
            100,
            19537,
            32,
            175354,
            32,
            46417,
            4,
            221973,
            511,
            0,
            1,
            89141,
            32,
            497525,
            14068,
            4,
            2,
            196500,
            453240,
            220,
            0,
            1,
            1,
            1000,
            28662,
            4,
            2,
            245000,
            216773,
            62,
            1,
            1060367,
            12586,
            1,
            208512,
            421,
            1,
            187000,
            1000,
            52998,
            1,
            80436,
            32,
            43249,
            32,
            1000,
            32,
            80556,
            1,
            57667,
            4,
            1000,
            10,
            197145,
            156,
            1,
            197145,
            156,
            1,
            204924,
            473,
            1,
            208896,
            511,
            1,
            52467,
            32,
            64832,
            32,
            65493,
            32,
            22558,
            32,
            16563,
            32,
            76511,
            32,
            196500,
            453240,
            220,
            0,
            1,
            1,
            69522,
            11687,
            0,
            1,
            60091,
            32,
            196500,
            453240,
            220,
            0,
            1,
            1,
            196500,
            453240,
            220,
            0,
            1,
            1,
            1159724,
            392670,
            0,
            2,
            806990,
            30482,
            4,
            1927926,
            82523,
            4,
            265318,
            0,
            4,
            0,
            85931,
            32,
            205665,
            812,
            1,
            1,
            41182,
            32,
            212342,
            32,
            31220,
            32,
            32696,
            32,
            43357,
            32,
            32247,
            32,
            38314,
            32,
            20000000000,
            20000000000,
            9462713,
            1021,
            10,
            20000000000,
            0,
            20000000000,
        ];
        use minicbor::Encoder;
        let mut buf = Vec::with_capacity(2048);
        let mut enc = Encoder::new(&mut buf);
        // map{1: [cost_entries]}  — key 1 = PlutusV2
        enc.map(1).expect("infallible");
        enc.u8(1).expect("infallible");
        enc.array(v2_costs.len() as u64).expect("infallible");
        for &c in v2_costs {
            enc.i64(c).expect("infallible");
        }
        buf
    }

    /// Build a minimal Conway-era CBOR transaction that `eval_phase_two_raw`
    /// can parse.
    ///
    /// The transaction spends one Plutus V2-locked UTxO and carries exactly
    /// one Spend redeemer.  The witness set contains the compiled V2 script.
    ///
    /// # Transaction layout
    ///
    /// ```text
    /// array(4)
    ///   body = map { 0: [input], 1: [output], 2: fee }
    ///   wits = map { 6: [script_cbor], 5: [[tag=0, idx=0, data=Unit, exunits]] }
    ///   is_valid = true
    ///   aux_data = null
    /// ```
    ///
    /// The body omits `script_data_hash` (key 11) because `eval_phase_two_raw`
    /// is called with `run_phase_one = false` and therefore never validates the
    /// integrity hash — our own Phase-1 pass enforces that rule.
    fn build_conway_tx_cbor(
        tx_input_hash: &[u8; 32],
        script_cbor: &[u8],
        ex_units_steps: u64,
        ex_units_mem: u64,
    ) -> Vec<u8> {
        build_conway_tx_cbor_inner(
            tx_input_hash,
            script_cbor,
            ex_units_steps,
            ex_units_mem,
            None,
        )
    }

    /// Like `build_conway_tx_cbor` but with a TTL (body key 3) so the
    /// script context requires validity-interval time translation —
    /// used by the horizon/CollectError partition tests (#733/#734).
    fn build_conway_tx_cbor_with_ttl(
        tx_input_hash: &[u8; 32],
        script_cbor: &[u8],
        ex_units_steps: u64,
        ex_units_mem: u64,
        ttl: u64,
    ) -> Vec<u8> {
        build_conway_tx_cbor_inner(
            tx_input_hash,
            script_cbor,
            ex_units_steps,
            ex_units_mem,
            Some(ttl),
        )
    }

    fn build_conway_tx_cbor_inner(
        tx_input_hash: &[u8; 32],
        script_cbor: &[u8],
        ex_units_steps: u64,
        ex_units_mem: u64,
        ttl: Option<u64>,
    ) -> Vec<u8> {
        // ----------------------------------------------------------------
        // Re-use the same minicbor encoder as the rest of the Plutus module.
        // All writes to Vec<u8> are infallible.
        // ----------------------------------------------------------------
        use minicbor::Encoder;

        let mut buf = Vec::with_capacity(256);
        let mut enc = Encoder::new(&mut buf);

        // Outer array(4): [body, wits, is_valid, null]
        enc.array(4).expect("infallible");

        // ----------------------------------------------------------------
        // [0] Transaction body — map(3): inputs, outputs, fee (+ ttl key 3)
        // ----------------------------------------------------------------
        enc.map(if ttl.is_some() { 4 } else { 3 })
            .expect("infallible");

        // key 0: inputs — a definite array containing one TransactionInput
        // TransactionInput CBOR: array(2) [bytes(32), uint(0)]
        enc.u8(0).expect("infallible");
        enc.array(1).expect("infallible");
        enc.array(2).expect("infallible");
        enc.bytes(tx_input_hash).expect("infallible");
        enc.u8(0).expect("infallible"); // output index

        // key 1: outputs — a definite array containing one PostAlonzo output
        //   PostAlonzo output is a CBOR map: { 0: address_bytes, 1: coin }
        //   Address: enterprise script address (mainnet), header=0x71 || script_hash
        //   We use a dummy output address (script-locked UTxOs live in utxo_set,
        //   but the output recipient can be any valid address).
        enc.u8(1).expect("infallible");
        let recipient_addr: Vec<u8> = {
            // Mainnet enterprise key-locked address (0x61 || 28-byte payment keyhash)
            let mut a = Vec::with_capacity(29);
            a.push(0x61u8); // mainnet enterprise key
            a.extend_from_slice(&[0xBBu8; 28]); // dummy payment key hash
            a
        };
        enc.array(1).expect("infallible");
        enc.map(2).expect("infallible");
        enc.u8(0).expect("infallible");
        enc.bytes(&recipient_addr).expect("infallible");
        enc.u8(1).expect("infallible");
        // Output value: 9 ADA
        enc.u32(9_000_000).expect("infallible");

        // key 2: fee — 1 ADA (not validated in phase-2 mode)
        enc.u8(2).expect("infallible");
        enc.u32(1_000_000).expect("infallible");

        // key 3: ttl (validity-interval upper bound), when requested
        if let Some(t) = ttl {
            enc.u8(3).expect("infallible");
            enc.u64(t).expect("infallible");
        }

        // ----------------------------------------------------------------
        // [1] Witness set — map(2): plutus_v2_scripts (key 6), redeemers (key 5)
        // ----------------------------------------------------------------
        enc.map(2).expect("infallible");

        // key 6: PlutusV2 scripts — plain array(1) [script_cbor_bytes]
        // the in-house decoder accepts a plain array as well as tag(258)+array for NonEmptySet
        enc.u8(6).expect("infallible");
        enc.array(1).expect("infallible");
        enc.bytes(script_cbor).expect("infallible");

        // key 5: redeemers — array(1) [[Spend, 0, Unit, [mem, steps]]]
        // Each redeemer: array(4) [tag, index, data, ex_units]
        // - tag 0 = Spend
        // - index 0 (first input)
        // - data = Unit = d87980 (Constr tag 0 with empty list, CBOR alternate format)
        // - ex_units = array(2) [mem, steps]  (CDDL order: mem first)
        enc.u8(5).expect("infallible");
        enc.array(1).expect("infallible");
        enc.array(4).expect("infallible");
        enc.u8(0).expect("infallible"); // Spend tag
        enc.u8(0).expect("infallible"); // index 0
                                        // PlutusData Unit: tag 121 (0x79 + 0xd8 two-byte tag encoding) with empty array
                                        // CBOR: d8 79 80 — tag(121), array(0)
                                        // minicbor tag API: tag(121).array(0)
        enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
        enc.array(0).expect("infallible");
        enc.array(2).expect("infallible");
        enc.u64(ex_units_mem).expect("infallible");
        enc.u64(ex_units_steps).expect("infallible");

        // ----------------------------------------------------------------
        // [2] is_valid = true
        // ----------------------------------------------------------------
        enc.bool(true).expect("infallible");

        // ----------------------------------------------------------------
        // [3] aux_data = null
        // ----------------------------------------------------------------
        enc.null().expect("infallible");

        buf
    }

    /// Build the UTxO set used in the Plutus evaluation tests.
    ///
    /// Inserts one script-locked UTxO with an inline Unit datum.  The address
    /// is a mainnet enterprise script address constructed from the provided
    /// script hash.
    fn build_script_utxo_set(
        tx_input_hash: &[u8; 32],
        script_hash: &[u8; 28],
    ) -> (UtxoSet, dugite_primitives::transaction::TransactionInput) {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{
            OutputDatum, PlutusData, TransactionInput, TransactionOutput,
        };
        use dugite_primitives::value::Value;

        let mut utxo_set = UtxoSet::new();
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes(*tx_input_hash),
            index: 0,
        };

        // Mainnet enterprise script address
        let script_cred = Credential::Script(Hash28::from_bytes(*script_hash));
        let address = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Mainnet,
            payment: script_cred,
        });

        // Inline Unit datum (Constr 0 [])
        let output = TransactionOutput {
            address,
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::InlineDatum {
                data: PlutusData::Constr(0, vec![]),
                raw_cbor: None,
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(input.clone(), output);
        (utxo_set, input)
    }

    // -----------------------------------------------------------------------
    // Test 1: Always-succeeds Plutus V2 spending validator
    //
    // The program `(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))` is
    // a valid Plutus V2 spending validator that ignores all three arguments
    // (datum, redeemer, script context) and returns Unit.
    //
    // Per Haskell's processLogsAndErrors for PlutusV1/V2: any non-error result
    // is a success — including a Unit constant.
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_always_succeeds_v2() {
        // Build always-succeeds validator: lam _ (lam _ (lam _ (con unit ())))
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);

        // Fixed UTxO input hash for this test
        let tx_input_hash = [0x01u8; 32];

        // Build transaction and UTxO set
        let tx_cbor = build_conway_tx_cbor(
            &tx_input_hash,
            &script_cbor,
            // Budget: generous CPU/mem — script should terminate well within budget
            14_000_000, // steps
            2_000_000,  // mem
        );
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        // Populate the Transaction struct's raw_cbor from the CBOR we built
        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        // Budget: (steps, mem) matching the convention in evaluate_plutus_scripts
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None, // no cost models — script is so simple it needs no builtins
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            result.is_ok(),
            "Always-succeeds script should pass Phase-2: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: Always-fails Plutus V2 script
    //
    // `(program 1.0.0 (error))` — the UPLC error term causes the CEK machine
    // to terminate with an evaluation error.  The caller should receive
    // `PlutusError::EvalFailed`.
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_always_fails_v2() {
        let script_cbor = build_script_cbor("(program 1.0.0 (error))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x02u8; 32];

        let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        // The error term must produce a script failure, not a parse or
        // infrastructure error.
        assert!(
            matches!(result, Err(PlutusError::EvalFailed(_))),
            "Always-fails script should produce EvalFailed: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // Test 2b: CollectError partition (#733/#734)
    //
    // An ALWAYS-SUCCEEDS script whose tx carries a TTL past the safe-zone
    // horizon must fail with `PlutusError::CollectError` (the Haskell
    // `UtxosFailure (CollectErrors (BadTranslation (TimeTranslationPastHorizon)))`
    // class) — NOT `EvalFailed`.  Conflating the two is what let an
    // `is_valid=false` tx with passing scripts into the mempool (#734) and
    // a Haskell-invalid block onto the dugite chain (#733).
    // -----------------------------------------------------------------------
    #[test]
    fn test_past_horizon_ttl_is_collect_error_not_eval_failed() {
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x07u8; 32];

        // TTL far past any horizon we set below.
        let tx_cbor =
            build_conway_tx_cbor_with_ttl(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000, 728);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor.clone()];

        // Devnet-shaped horizon: tip=128, safe zone 240, epoch 400 →
        // horizon slot 400 (exclusive).  TTL 728 ≥ 400 → past horizon.
        let mut slot_config = SlotConfig::preview();
        slot_config.safe_zone_horizon_slot = Some(400);
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );
        assert!(
            matches!(result, Err(PlutusError::CollectError(_))),
            "past-horizon TTL must be CollectError, got: {result:?}"
        );

        // Same tx with the horizon left unbounded (legacy behavior) must
        // evaluate fine — proving the TTL itself is well-formed and the
        // CollectError above came from the horizon guard.
        slot_config.safe_zone_horizon_slot = None;
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );
        assert!(
            result.is_ok(),
            "same tx within horizon must pass: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: Budget exhaustion
    //
    // Supply a budget of (1 step, 1 mem) — far below what even the simplest
    // always-succeeds script requires.  The evaluation must fail because the
    // machine exhausts its CPU budget before completing.
    //
    // This verifies that:
    //   (a) the budget is actually enforced by the uplc evaluator, and
    //   (b) `evaluate_plutus_scripts` surfaces budget errors as EvalFailed
    //       rather than silently succeeding or panicking.
    //
    // IMPORTANT: budget enforcement only works when cost models are supplied.
    // With `cost_models_cbor = None` the uplc evaluator ignores `initial_budget`
    // and uses an unconstrained `ExBudget::default()`.  Real cost models are
    // required to activate the per-redeemer budget check in
    // `eval_redeemer` / `Program::eval_as(…, Some(budget))`.
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_budget_exhaustion() {
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x03u8; 32];

        // Declare 1 step / 1 mem in the redeemer (so the tx encodes minimal
        // declared budget).  The actual budget limit passed to the evaluator
        // is controlled by the `max_tx_ex_units` argument, NOT the redeemer's
        // ex_units field — but we keep them consistent here for realism.
        let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, 1, 1);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();

        // Supply real cost models so the evaluator enforces the budget cap.
        let cost_models = vasil_v2_cost_models_cbor();

        // Pass an impossibly small budget to the evaluator (1 step, 1 mem).
        let result =
            evaluate_plutus_scripts(&tx, &utxo_set, Some(&cost_models), (1, 1), &slot_config, 9);

        assert!(
            result.is_err(),
            "Evaluation with budget (1, 1) must fail; got Ok"
        );
        // The failure should be reported as EvalFailed (machine-level budget
        // exhaustion), not as a missing-CBOR or infrastructure error.
        assert!(
            matches!(result, Err(PlutusError::EvalFailed(_))),
            "Budget exhaustion should produce EvalFailed: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // Regression test for #450: adversarial malformed Plutus witness scripts.
    //
    // Both the legacy CBOR codec's flat decoder and aiken-lang/uplc have known panics
    // on malformed input (the legacy CBOR codec/src/flat/decode/decoder.rs unwrap,
    // uplc/src/tx.rs:194 unwrap on Err(EndOfInput)). Without the catch_unwind
    // guard added in `evaluate_plutus_scripts`, a peer-supplied script bundled
    // in a gossiped transaction could panic the node — a remote DoS over
    // TxSubmission2 / N2C.
    //
    // This test feeds each of the three saved fuzz crash artifacts
    // (`fuzz/artifacts/fuzz_plutus_script_decode/crash-*`) as the witness-set
    // Plutus V2 script bytes and asserts that the call returns an `Err` —
    // crucially WITHOUT panicking. Any future regression in the catch_unwind
    // guard (e.g. an accidental switch back to `panic = "abort"` or removal
    // of `std::panic::catch_unwind` in plutus.rs) will surface here as a
    // process abort during `cargo test`.
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_rejects_malformed_witness_script_without_panic() {
        // Inline copies of the fuzz crash artifacts. Keeping them inline (vs.
        // reading the files at test time) makes the regression self-contained
        // and survives pruning of the fuzz/ artifacts directory.
        let adversarial_scripts: &[&[u8]] = &[
            // crash-289a373b…  (2 bytes)
            &[0xd6, 0xec],
            // crash-9daa5ea5…  (2 bytes)
            &[0x0a, 0x79],
            // crash-82fab4ff…  (14 bytes)
            &[
                0x21, 0x06, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ],
            // crash-a4a22152…  (12 bytes)
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ],
        ];

        let slot_config = SlotConfig::preview();

        for (i, malformed) in adversarial_scripts.iter().enumerate() {
            let script_cbor = malformed.to_vec();
            let script_hash = script_hash_v2(&script_cbor);
            let tx_input_hash = [(0xa0 + i as u8); 32];

            // The tx body references this script via the spending UTxO. The
            // tx_cbor produced by build_conway_tx_cbor embeds the malformed
            // bytes verbatim in the witness set (key 3 = plutus_v2_script),
            // which is exactly the path through `eval_phase_two_raw` →
            // `Program::<DeBruijn>::from_cbor` → the legacy CBOR codec flat decoder
            // that historically panicked.
            let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
            let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

            let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
            tx.raw_cbor = Some(tx_cbor);
            tx.body.inputs = vec![input];
            tx.witness_set.plutus_v2_scripts = vec![script_cbor];

            // The expectation is "does not panic AND returns Err". We do not
            // pin the exact PlutusError variant because malformed input may
            // surface as either:
            //   - EvalFailed       (panic intercepted by catch_unwind)
            //   - MissingScriptData / ScriptDecode / CborDecode (caught earlier
            //     by our own phase-1 plumbing before reaching the evaluator)
            let result = evaluate_plutus_scripts(
                &tx,
                &utxo_set,
                None,
                (14_000_000, 2_000_000),
                &slot_config,
                9,
            );

            assert!(
                result.is_err(),
                "Adversarial script #{i} ({} bytes) must be rejected, got Ok: {:?}",
                malformed.len(),
                result
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: Always-succeeds Plutus V1 spending validator
    //
    // PlutusV1 scripts follow the same success rule as V2: any non-error
    // result is accepted.  The only difference in our evaluation path is the
    // script version tag (0x01 vs 0x02) which determines the TxInfo version
    // passed to the script.
    //
    // For a PlutusV1 validator the witness set key is 3 (plutus_v1_script).
    // We verify the corresponding V1 code path in evaluate_plutus_scripts.
    //
    // NOTE: PlutusV1 does NOT support inline datums (the inline datum feature
    // was introduced in Babbage/PlutusV2).  The spending UTxO must carry a
    // datum hash, with the corresponding datum placed in the witness set
    // (key 4 = plutus_data).
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_always_succeeds_v1() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{
            OutputDatum, PlutusData, TransactionInput, TransactionOutput,
        };
        use dugite_primitives::value::Value;

        // Build always-succeeds V1 validator
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");

        // V1 script hash: blake2b_224(0x01 || script_bytes)
        let v1_script_hash: [u8; 28] = {
            let mut tagged = Vec::with_capacity(1 + script_cbor.len());
            tagged.push(0x01u8);
            tagged.extend_from_slice(&script_cbor);
            *dugite_primitives::hash::blake2b_224(&tagged).as_bytes()
        };

        let tx_input_hash = [0x04u8; 32];

        // PlutusV1 requires a datum hash in the UTxO output (not inline datum).
        // The datum itself is placed in the witness set.
        // Datum: Unit = Constr 0 []
        // Datum CBOR: d87980 (tag 121, empty array)
        let datum_cbor: Vec<u8> = {
            use minicbor::Encoder;
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
            enc.array(0).expect("infallible");
            buf
        };
        let datum_hash: [u8; 32] = *dugite_primitives::hash::blake2b_256(&datum_cbor).as_bytes();

        // Build UTxO with datum hash (not inline)
        let mut utxo_set = UtxoSet::new();
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes(tx_input_hash),
            index: 0,
        };
        let script_cred = Credential::Script(Hash28::from_bytes(v1_script_hash));
        let address = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Mainnet,
            payment: script_cred,
        });
        let output = TransactionOutput {
            address,
            value: Value::lovelace(10_000_000),
            // Datum hash, not inline — required for PlutusV1 compatibility
            datum: OutputDatum::DatumHash(Hash32::from_bytes(datum_hash)),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(input.clone(), output);

        // Build a Conway CBOR tx with:
        //   - key 3 (V1 scripts) in witness set
        //   - key 4 (plutus_data) with the Unit datum
        //   - key 5 (redeemers) with one Spend redeemer
        let tx_cbor: Vec<u8> = {
            use minicbor::Encoder;
            let mut buf = Vec::with_capacity(512);
            let mut enc = Encoder::new(&mut buf);
            enc.array(4).expect("infallible");

            // Body: inputs, outputs, fee
            enc.map(3).expect("infallible");
            enc.u8(0).expect("infallible"); // inputs key
            enc.array(1).expect("infallible");
            enc.array(2).expect("infallible");
            enc.bytes(&tx_input_hash).expect("infallible");
            enc.u8(0).expect("infallible");
            enc.u8(1).expect("infallible"); // outputs key
            enc.array(1).expect("infallible");
            enc.map(2).expect("infallible");
            enc.u8(0).expect("infallible");
            enc.bytes(&{
                let mut a = vec![0x61u8]; // mainnet enterprise key
                a.extend_from_slice(&[0xBBu8; 28]);
                a
            })
            .expect("infallible");
            enc.u8(1).expect("infallible");
            enc.u32(9_000_000).expect("infallible");
            enc.u8(2).expect("infallible"); // fee key
            enc.u32(1_000_000).expect("infallible");

            // Witness set: V1 scripts (key 3), datums (key 4), redeemers (key 5)
            enc.map(3).expect("infallible");
            enc.u8(3).expect("infallible"); // PlutusV1 scripts
            enc.array(1).expect("infallible");
            enc.bytes(&script_cbor).expect("infallible");
            enc.u8(4).expect("infallible"); // plutus_data (datums)
            enc.array(1).expect("infallible");
            // Encode the datum: Unit = constr 0 []
            enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
            enc.array(0).expect("infallible");
            enc.u8(5).expect("infallible"); // redeemers
            enc.array(1).expect("infallible");
            enc.array(4).expect("infallible");
            enc.u8(0).expect("infallible"); // Spend
            enc.u8(0).expect("infallible"); // index 0
                                            // Redeemer data: Unit
            enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
            enc.array(0).expect("infallible");
            enc.array(2).expect("infallible");
            enc.u64(14_000_000).expect("infallible");
            enc.u64(2_000_000).expect("infallible");

            enc.bool(true).expect("infallible");
            enc.null().expect("infallible");
            buf
        };

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v1_scripts = vec![script_cbor];
        // Provide the datum in the witness set
        tx.witness_set.plutus_data = vec![PlutusData::Constr(0, vec![])];

        let slot_config = SlotConfig::preview();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );
        assert!(
            result.is_ok(),
            "Always-succeeds V1 script should pass Phase-2: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: Script context construction — verify that inputs in the UTxO
    //         set that are NOT referenced by the transaction are NOT resolved,
    //         AND verify that evaluate_plutus_scripts succeeds for a locally-
    //         built (no raw_cbor) transaction with no redeemers — exercising
    //         the deterministic re-encoding fallback.
    //
    // This tests that `evaluate_plutus_scripts` only passes input/output CBOR
    // pairs for inputs that appear in the transaction body (inputs +
    // reference_inputs + collateral), not arbitrary UTxO entries.
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_only_resolves_tx_inputs() {
        use dugite_primitives::address::{Address, ByronAddress};
        use dugite_primitives::transaction::{OutputDatum, TransactionInput, TransactionOutput};
        use dugite_primitives::value::Value;

        // Inject extra UTxOs that must NOT be resolved
        let mut utxo_set = UtxoSet::new();
        for i in 1u8..=5 {
            let extra_input = TransactionInput {
                transaction_id: Hash32::from_bytes([i; 32]),
                index: 0,
            };
            let extra_output = TransactionOutput {
                address: Address::Byron(ByronAddress {
                    payload: vec![0u8; 32],
                }),
                value: Value::lovelace(1_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            };
            utxo_set.insert(extra_input, extra_output);
        }

        // Locally-built transaction with no raw_cbor and no redeemers:
        // evaluate_plutus_scripts must (a) re-encode the tx via the
        // deterministic fallback rather than failing, and (b) succeed
        // without resolving any of the unrelated UTxOs above (the tx body
        // has no inputs, so the resolution loop iterates zero times).
        let tx = Transaction::empty_with_hash(Hash32::ZERO);
        assert!(tx.raw_cbor.is_none(), "precondition: locally-built tx");
        let slot_config = SlotConfig::default();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (10_000_000, 10_000_000),
            &slot_config,
            9,
        );
        assert!(
            result.is_ok(),
            "Locally-built tx with no redeemers must succeed via the re-encode fallback: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: ExUnits comparison — verify the budget tuple convention.
    //
    // `evaluate_plutus_scripts` takes `max_tx_ex_units` as `(steps, mem)`,
    // matching the `uplc::tx::eval_phase_two_raw` convention where `.0 = cpu`
    // and `.1 = mem`.  Passing `(mem, steps)` would produce a 700× too-small
    // CPU ceiling and cause false failures for scripts that use many steps.
    //
    // This test confirms that the correct ordering passes evaluation and the
    // swapped ordering (mem as CPU ceiling) fails budget exhaustion.
    //
    // Budget enforcement requires real cost models (see test 3 notes).
    // -----------------------------------------------------------------------
    #[test]
    fn test_evaluate_exunits_ordering() {
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x06u8; 32];

        // The always-succeeds validator (with V2 cost models applied) uses
        // roughly ~7_600_000 CPU steps and ~2_000 mem units on the CEK machine.
        // We choose budget values that are:
        //   - Clearly sufficient when passed in the correct (steps, mem) order.
        //   - Too small for steps when only mem-scale values are used.
        //
        // budget_steps = 14_000_000  (well above ~7.6M actual cost)
        // budget_mem   = 50_000      (well above ~2 000 actual mem)
        let budget_steps: u64 = 14_000_000;
        let budget_mem: u64 = 50_000;

        let cost_models = vasil_v2_cost_models_cbor();

        let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, budget_steps, budget_mem);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor.clone());
        tx.body.inputs = vec![input.clone()];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor.clone()];

        let slot_config = SlotConfig::preview();

        // Correct ordering (steps, mem) must succeed
        let result_correct = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            Some(&cost_models),
            (budget_steps, budget_mem),
            &slot_config,
            9,
        );
        assert!(
            result_correct.is_ok(),
            "Correct (steps, mem) ordering must succeed: {:?}",
            result_correct.err()
        );

        // Now rebuild UTxO set (it was moved) and test with a tiny CPU budget.
        // Budget (1, budget_mem): 1 step is far too small — must fail.
        let (utxo_set2, input2) = build_script_utxo_set(&tx_input_hash, &script_hash);
        let mut tx2 = Transaction::empty_with_hash(Hash32::ZERO);
        tx2.raw_cbor = Some(tx_cbor);
        tx2.body.inputs = vec![input2];
        tx2.witness_set.plutus_v2_scripts = vec![script_cbor];

        let result_exhausted = evaluate_plutus_scripts(
            &tx2,
            &utxo_set2,
            Some(&cost_models),
            (1, budget_mem),
            &slot_config,
            9,
        );
        assert!(
            result_exhausted.is_err(),
            "Tiny steps budget (1) must cause EvalFailed"
        );
    }

    // -----------------------------------------------------------------------
    // (Removed) `decode_redeemer_tag_index` round-trip test
    //
    // The helper was only used to look up the per-redeemer language
    // version after aiken-uplc's `eval_phase_two_raw` returned. The new
    // `dugite_uplc::phase_two::eval_phase_two_raw` resolves the version
    // internally per redeemer, so the helper + this test are obsolete.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: Cross-validation — always-succeeds V2 with real cost model
    //
    // Verifies that evaluate_plutus_scripts succeeds when supplied with the
    // real Vasil-era PlutusV2 cost model (178 entries).  This validates that
    // our UPLC integration works with production cost model coefficients and
    // that the CostModels CBOR encoding is accepted by the uplc evaluator.
    // -----------------------------------------------------------------------
    #[test]
    fn test_cross_validate_v2_with_real_cost_model() {
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x10u8; 32];

        let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        let cost_models = vasil_v2_cost_models_cbor();

        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            Some(&cost_models),
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            result.is_ok(),
            "Always-succeeds V2 with real cost model should pass Phase-2: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test: Cross-validation — V3 Unit-check with always-succeeds V3 script
    //
    // PlutusV3 requires the script to return exactly Unit (per CIP-1694 /
    // Haskell evaluateScriptRestricting).  This test verifies that:
    //   (a) a V3 script returning Unit passes evaluation
    //   (b) a V3 script returning non-Unit (integer 42) is rejected
    //
    // The V3 script lives in witness set key 7 and uses script hash prefix
    // 0x03.  The per-redeemer V3 check should correctly apply the Unit
    // return-value rule.
    // -----------------------------------------------------------------------

    /// Compute the PlutusV3 script hash (blake2b_224(0x03 || script_cbor_bytes))
    fn script_hash_v3(script_cbor: &[u8]) -> [u8; 28] {
        let mut tagged = Vec::with_capacity(1 + script_cbor.len());
        tagged.push(0x03u8);
        tagged.extend_from_slice(script_cbor);
        *dugite_primitives::hash::blake2b_224(&tagged).as_bytes()
    }

    /// Build a Conway-era CBOR transaction with a PlutusV3 script (key 7).
    fn build_conway_tx_cbor_v3(
        tx_input_hash: &[u8; 32],
        script_cbor: &[u8],
        ex_units_steps: u64,
        ex_units_mem: u64,
    ) -> Vec<u8> {
        use minicbor::Encoder;

        let mut buf = Vec::with_capacity(256);
        let mut enc = Encoder::new(&mut buf);

        enc.array(4).expect("infallible");

        // Body: map(3) {0: [input], 1: [output], 2: fee}
        enc.map(3).expect("infallible");
        enc.u8(0).expect("infallible");
        enc.array(1).expect("infallible");
        enc.array(2).expect("infallible");
        enc.bytes(tx_input_hash).expect("infallible");
        enc.u8(0).expect("infallible");
        enc.u8(1).expect("infallible");
        enc.array(1).expect("infallible");
        enc.map(2).expect("infallible");
        enc.u8(0).expect("infallible");
        enc.bytes(&{
            let mut a = vec![0x61u8];
            a.extend_from_slice(&[0xBBu8; 28]);
            a
        })
        .expect("infallible");
        enc.u8(1).expect("infallible");
        enc.u32(9_000_000).expect("infallible");
        enc.u8(2).expect("infallible");
        enc.u32(1_000_000).expect("infallible");

        // Witness set: map(2) { 5: redeemers, 7: v3_scripts }
        enc.map(2).expect("infallible");

        // key 7: PlutusV3 scripts
        enc.u8(7).expect("infallible");
        enc.array(1).expect("infallible");
        enc.bytes(script_cbor).expect("infallible");

        // key 5: redeemers
        enc.u8(5).expect("infallible");
        enc.array(1).expect("infallible");
        enc.array(4).expect("infallible");
        enc.u8(0).expect("infallible"); // Spend
        enc.u8(0).expect("infallible"); // index 0
        enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
        enc.array(0).expect("infallible"); // Unit redeemer data
        enc.array(2).expect("infallible");
        enc.u64(ex_units_mem).expect("infallible");
        enc.u64(ex_units_steps).expect("infallible");

        enc.bool(true).expect("infallible");
        enc.null().expect("infallible");

        buf
    }

    #[test]
    fn test_cross_validate_v3_unit_return_succeeds() {
        // V3 always-succeeds: returns Unit (the only valid V3 return value)
        // PlutusV3 uses program version 1.1.0 and receives a single merged
        // argument per CIP-0069 (datum + redeemer + context merged into one).
        let script_cbor = build_script_cbor("(program 1.1.0 (lam _ (con unit ())))");
        let script_hash = script_hash_v3(&script_cbor);
        let tx_input_hash = [0x11u8; 32];

        let tx_cbor = build_conway_tx_cbor_v3(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v3_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            result.is_ok(),
            "V3 script returning Unit should pass Phase-2: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_validate_v3_non_unit_return_fails() {
        // V3 script that returns integer 42 (not Unit) — must be rejected.
        // Single-lambda per CIP-0069 (V3 scripts receive one merged argument).
        let script_cbor = build_script_cbor("(program 1.1.0 (lam _ (con integer 42)))");
        let script_hash = script_hash_v3(&script_cbor);
        let tx_input_hash = [0x12u8; 32];

        let tx_cbor = build_conway_tx_cbor_v3(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v3_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None,
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            matches!(result, Err(PlutusError::EvalFailed(_))),
            "V3 script returning non-Unit must be rejected: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // Test: CostModels CBOR encoding roundtrip
    //
    // Verifies that CostModels::to_cbor() produces valid CBOR that can be
    // fed to evaluate_plutus_scripts without error.  Tests V1, V2, and V3
    // cost models individually and together.
    // -----------------------------------------------------------------------
    #[test]
    fn test_cost_models_cbor_roundtrip_v2_evaluator() {
        use dugite_primitives::transaction::CostModels;

        // Build a CostModels with the real V2 cost model and verify
        // its to_cbor() output works with the evaluator.
        let v2_costs: Vec<i64> = vec![
            205665,
            812,
            1,
            1,
            1000,
            571,
            0,
            1,
            1000,
            24177,
            4,
            1,
            1000,
            32,
            117366,
            10475,
            4,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            23000,
            100,
            100,
            100,
            23000,
            100,
            19537,
            32,
            175354,
            32,
            46417,
            4,
            221973,
            511,
            0,
            1,
            89141,
            32,
            497525,
            14068,
            4,
            2,
            196500,
            453240,
            220,
            0,
            1,
            1,
            1000,
            28662,
            4,
            2,
            245000,
            216773,
            62,
            1,
            1060367,
            12586,
            1,
            208512,
            421,
            1,
            187000,
            1000,
            52998,
            1,
            80436,
            32,
            43249,
            32,
            1000,
            32,
            80556,
            1,
            57667,
            4,
            1000,
            10,
            197145,
            156,
            1,
            197145,
            156,
            1,
            204924,
            473,
            1,
            208896,
            511,
            1,
            52467,
            32,
            64832,
            32,
            65493,
            32,
            22558,
            32,
            16563,
            32,
            76511,
            32,
            196500,
            453240,
            220,
            0,
            1,
            1,
            69522,
            11687,
            0,
            1,
            60091,
            32,
            196500,
            453240,
            220,
            0,
            1,
            1,
            196500,
            453240,
            220,
            0,
            1,
            1,
            1159724,
            392670,
            0,
            2,
            806990,
            30482,
            4,
            1927926,
            82523,
            4,
            265318,
            0,
            4,
            0,
            85931,
            32,
            205665,
            812,
            1,
            1,
            41182,
            32,
            212342,
            32,
            31220,
            32,
            32696,
            32,
            43357,
            32,
            32247,
            32,
            38314,
            32,
            20000000000,
            20000000000,
            9462713,
            1021,
            10,
            20000000000,
            0,
            20000000000,
        ];

        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: Some(v2_costs),
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };

        let cbor = cm
            .to_cbor()
            .expect("CostModels::to_cbor() should produce CBOR");

        // Verify the CBOR is valid by decoding the map structure
        let mut dec = minicbor::Decoder::new(&cbor);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1, "Should have exactly 1 entry (V2)");

        // Now feed it to the evaluator with a real script
        let script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))");
        let script_hash = script_hash_v2(&script_cbor);
        let tx_input_hash = [0x13u8; 32];

        let tx_cbor = build_conway_tx_cbor(&tx_input_hash, &script_cbor, 14_000_000, 2_000_000);
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        tx.witness_set.plutus_v2_scripts = vec![script_cbor];

        let slot_config = SlotConfig::preview();
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            Some(&cbor),
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            result.is_ok(),
            "CostModels::to_cbor() output should be accepted by evaluator: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cost_models_cbor_roundtrip_all_versions() {
        use dugite_primitives::transaction::CostModels;

        // Verify that a CostModels with all three versions produces valid CBOR.
        // We use minimal cost arrays since the evaluator only consults the
        // version relevant to the script being evaluated.
        let cm = CostModels {
            plutus_v1: Some(vec![100; 166]), // V1 has 166 cost model entries
            plutus_v2: Some(vec![100; 178]), // V2 has 178 cost model entries
            plutus_v3: Some(vec![100; 251]), // V3 has 251 cost model entries (Conway)
            // PlutusV4 (Dijkstra) cost-model slot is part of issue #475 Phase 5.
            plutus_v4: None,
            ..Default::default()
        };

        let cbor = cm
            .to_cbor()
            .expect("CostModels::to_cbor() should produce CBOR");

        // Verify structure: map with 3 entries (keys 0, 1, 2)
        let mut dec = minicbor::Decoder::new(&cbor);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 3, "Should have 3 entries (V1, V2, V3)");

        // Verify key 0 (V1) has 166 entries
        assert_eq!(dec.u32().unwrap(), 0);
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 166);
        for _ in 0..166 {
            dec.i64().unwrap();
        }

        // Verify key 1 (V2) has 178 entries
        assert_eq!(dec.u32().unwrap(), 1);
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 178);
        for _ in 0..178 {
            dec.i64().unwrap();
        }

        // Verify key 2 (V3) has 251 entries
        assert_eq!(dec.u32().unwrap(), 2);
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 251);
        for _ in 0..251 {
            dec.i64().unwrap();
        }
    }

    // -----------------------------------------------------------------------
    // Test 8: Per-redeemer V3 Unit-check (regression for GH#185)
    //
    // A transaction that contains BOTH a PlutusV2 script (Spend redeemer)
    // AND a PlutusV3 script in the witness set (but no redeemer for the V3
    // script) must NOT apply the Unit-return check to the V2 redeemer.
    //
    // The V2 script `(program 1.0.0 (lam _ (lam _ (lam _ (con integer 42)))))`
    // returns the integer 42 (not Unit).  Under the old transaction-wide
    // `has_any_v3` flag this would have been rejected.  Under the correct
    // per-redeemer check, the V2 Spend redeemer maps to version 2 and the
    // Unit check is NOT applied — the script must succeed.
    //
    // The witness set contains a V3 script (key 7) that has no redeemer, so
    // `eval_phase_two_raw` never executes it — this ensures only the V2
    // script runs while the V3 script is visible in `plutus_script_version_map`.
    // -----------------------------------------------------------------------
    #[test]
    fn test_v2_non_unit_return_not_blocked_by_v3_presence() {
        use minicbor::Encoder;

        // V2 script that returns integer 42 (not Unit)
        let v2_script_cbor =
            build_script_cbor("(program 1.0.0 (lam _ (lam _ (lam _ (con integer 42)))))");
        let v2_script_hash = script_hash_v2(&v2_script_cbor);

        // A trivial V3 script (never executed — no redeemer points to it).
        // We pick a short byte sequence so the script_hash differs from the V2 hash.
        let v3_script_cbor = build_script_cbor("(program 1.0.0 (lam _ (con unit ())))");

        let tx_input_hash = [0x08u8; 32];

        // Build transaction CBOR with both V2 (key 6) and V3 (key 7) scripts
        // and ONE Spend redeemer targeting the V2 script.
        let tx_cbor: Vec<u8> = {
            let mut buf = Vec::with_capacity(512);
            let mut enc = Encoder::new(&mut buf);

            // Outer: array(4) [body, wits, is_valid, null]
            enc.array(4).expect("infallible");

            // Body: map(3) {0: [input], 1: [output], 2: fee}
            enc.map(3).expect("infallible");
            enc.u8(0).expect("infallible"); // inputs
            enc.array(1).expect("infallible");
            enc.array(2).expect("infallible");
            enc.bytes(&tx_input_hash).expect("infallible");
            enc.u8(0).expect("infallible");
            enc.u8(1).expect("infallible"); // outputs
            enc.array(1).expect("infallible");
            enc.map(2).expect("infallible");
            enc.u8(0).expect("infallible");
            enc.bytes(&{
                let mut a = vec![0x61u8];
                a.extend_from_slice(&[0xBBu8; 28]);
                a
            })
            .expect("infallible");
            enc.u8(1).expect("infallible");
            enc.u32(9_000_000).expect("infallible");
            enc.u8(2).expect("infallible"); // fee
            enc.u32(1_000_000).expect("infallible");

            // Witness set: map(3) { 5: redeemers, 6: v2_scripts, 7: v3_scripts }
            enc.map(3).expect("infallible");

            // key 5: redeemers — one Spend redeemer at index 0 (for the V2 script)
            enc.u8(5).expect("infallible");
            enc.array(1).expect("infallible");
            enc.array(4).expect("infallible");
            enc.u8(0).expect("infallible"); // Spend
            enc.u8(0).expect("infallible"); // index 0
            enc.tag(minicbor::data::Tag::new(121)).expect("infallible");
            enc.array(0).expect("infallible"); // Unit redeemer data
            enc.array(2).expect("infallible");
            enc.u64(14_000_000).expect("infallible"); // steps
            enc.u64(2_000_000).expect("infallible"); // mem

            // key 6: PlutusV2 scripts
            enc.u8(6).expect("infallible");
            enc.array(1).expect("infallible");
            enc.bytes(&v2_script_cbor).expect("infallible");

            // key 7: PlutusV3 scripts (present but no redeemer — not executed)
            enc.u8(7).expect("infallible");
            enc.array(1).expect("infallible");
            enc.bytes(&v3_script_cbor).expect("infallible");

            enc.bool(true).expect("infallible"); // is_valid
            enc.null().expect("infallible"); // aux_data

            buf
        };

        // UTxO: the input is locked by the V2 script
        let (utxo_set, input) = build_script_utxo_set(&tx_input_hash, &v2_script_hash);

        let mut tx = Transaction::empty_with_hash(Hash32::ZERO);
        tx.raw_cbor = Some(tx_cbor);
        tx.body.inputs = vec![input];
        // Populate witness_set so plutus_script_version_map can see both scripts
        tx.witness_set.plutus_v2_scripts = vec![v2_script_cbor];
        tx.witness_set.plutus_v3_scripts = vec![v3_script_cbor];

        let slot_config = SlotConfig::preview();

        // The V2 script returns integer 42 (not Unit).  With the old
        // transaction-wide `has_any_v3` flag this would incorrectly fail
        // (because a V3 script is present).  With the correct per-redeemer
        // check the Spend redeemer at (0, 0) maps to V2 → no Unit check →
        // the script must succeed.
        let result = evaluate_plutus_scripts(
            &tx,
            &utxo_set,
            None, // no cost models needed for this simple script
            (14_000_000, 2_000_000),
            &slot_config,
            9,
        );

        assert!(
            result.is_ok(),
            "V2 script returning non-Unit must NOT be blocked by presence of a V3 script: {:?}",
            result.err()
        );
    }
}
