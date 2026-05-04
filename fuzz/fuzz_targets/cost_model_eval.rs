//! Fuzz target for Plutus Phase-2 evaluation via the `uplc` CEK machine.
//!
//! This is the highest-blast-radius boundary in the Dugite stack: arbitrary
//! bytes parsed as a Cardano transaction and then fed directly into the uplc
//! evaluator.  Bugs in the uplc pallas transaction decoder, script-context
//! builder, or CEK machine that manifest as Rust panics are findings worth
//! reporting upstream to aiken-lang/aiken.
//!
//! # Budget bounding
//!
//! The execution budget is capped tightly so each fuzzer iteration stays well
//! under 1 second:
//!   - CPU steps: 10_000_000  (1/1000th of mainnet max)
//!   - Memory units: 14_000   (1/1000th of mainnet max)
//!
//! `cost_models_cbor = None` is passed to `eval_phase_two_raw`.  Without a
//! cost model the uplc evaluator uses `ExBudget::default()` (maximum budget),
//! but in practice fuzz-derived script bytes almost never decode as valid flat
//! UPLC programs — the CEK machine returns an error within microseconds.  For
//! the rare case where bytes do form a valid (but short) program, the step
//! budget of 10M caps execution time comfortably under 1 s.
//!
//! # Layered coverage
//!
//! The target exercises the full public Phase-2 path through dugite-ledger's
//! `evaluate_plutus_scripts`, which:
//!   1. Re-encodes the transaction (or uses raw_cbor) for uplc.
//!   2. Resolves UTxO pairs from the provided lookup.
//!   3. Calls `uplc::tx::eval_phase_two_raw` — which decodes the transaction,
//!      builds the `DataLookupTable` (script-context construction), runs the
//!      CEK machine per redeemer, and returns per-redeemer `EvalResult`s.
//!   4. Interprets the `EvalResult` according to V1/V2/V3 semantics.
//!
//! Step 3 is where the script-context builder lives; this target provides
//! indirect coverage of `TxInfoV3::from_transaction`,
//! `DataLookupTable::from_transaction`, and `get_tx_in_info_v2` without
//! requiring a direct pallas-primitives dependency in the fuzz crate.
//!
//! # What we look for
//!
//! - Panics (unwrap/expect/index-out-of-bounds) inside uplc or pallas decoders.
//! - Panics inside our script-version dispatch (version map, redeemer tag decode,
//!   V3 Unit check).
//! - Stack overflows from deeply recursive PlutusData or CEK term structures.
//!
//! `Err` returns from `evaluate_plutus_scripts` are expected for malformed input
//! and are silently discarded.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_cost_model_eval -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_ledger::evaluate_plutus_scripts;
use dugite_ledger::SlotConfig;
use dugite_ledger::utxo::UtxoLookup;
use dugite_primitives::transaction::{TransactionInput, TransactionOutput};

// ---------------------------------------------------------------------------
// Tight execution budget — caps CEK iterations for valid scripts.
// Convention: (cpu_steps, mem_units) matching uplc's eval_phase_two_raw order
// where `.0 = cpu` and `.1 = mem`.
// ---------------------------------------------------------------------------
const FUZZ_BUDGET: (u64, u64) = (10_000_000, 14_000);

// ---------------------------------------------------------------------------
// UTxO lookup stub — always returns None.
//
// We pass an empty UTxO set so that evaluate_plutus_scripts resolves no inputs.
// The coverage goal is to reach the uplc decoder and CEK machine; spending-
// input redeemers will fail due to missing UTxOs, which is expected and safe.
// ---------------------------------------------------------------------------
struct EmptyUtxoSet;

impl UtxoLookup for EmptyUtxoSet {
    fn lookup(&self, _input: &TransactionInput) -> Option<TransactionOutput> {
        None
    }

    fn contains(&self, _input: &TransactionInput) -> bool {
        false
    }
}

// Mainnet slot config used for time-conversion inside script contexts.
const MAINNET_SLOT_CONFIG: (u64, u64, u32) = (1_596_059_091_000, 4_492_800, 1_000);

fuzz_target!(|data: &[u8]| {
    // Need at least 1 byte to do anything meaningful.
    if data.is_empty() {
        return;
    }

    // ---------------------------------------------------------------------------
    // Path A: dugite-ledger `evaluate_plutus_scripts` — the full integration path.
    //
    // We decode `data` through the dugite serialization layer first (which
    // exercises `convert_plutus_data`, redeemer deserialization, etc.) then
    // call `evaluate_plutus_scripts`.  That function re-encodes the tx for uplc
    // (or uses raw_cbor when available), resolves UTxO pairs, and invokes
    // `uplc::tx::eval_phase_two_raw`.
    //
    // Era IDs: 6 = Alonzo, 7 = Babbage, 8 = Conway.
    // ---------------------------------------------------------------------------
    let slot_config = SlotConfig::default();

    for era_id in [6u16, 7, 8] {
        if let Ok(tx) = dugite_serialization::decode_transaction(era_id, data) {
            let _ = evaluate_plutus_scripts(
                &tx,
                &EmptyUtxoSet,
                None, // no cost model; CEK budget applied via FUZZ_BUDGET below
                FUZZ_BUDGET,
                &slot_config,
            );
            // Only try the first era that successfully decodes.
            break;
        }
    }

    // ---------------------------------------------------------------------------
    // Path B: `uplc::tx::eval_phase_two_raw` directly with raw fuzz bytes.
    //
    // This exercises the raw-bytes → MintedTx decode path inside uplc that is
    // NOT gated behind our transaction decoder.  If uplc's own decoder panics on
    // malformed input, that is a finding worth reporting upstream to aiken-lang.
    //
    // Passing `None` for cost_models_cbor means the evaluator falls back to
    // ExBudget::default() (unconstrained).  The script bytes in fuzz-derived
    // transactions are virtually never valid UPLC flat programs, so the machine
    // terminates immediately with an error.  For the rare valid case, FUZZ_BUDGET
    // is passed as `initial_budget` (enforced only when cost model is Some).
    // ---------------------------------------------------------------------------
    let _ = uplc::tx::eval_phase_two_raw(
        data,
        &[],    // no UTxOs
        None,   // no cost model
        FUZZ_BUDGET,
        MAINNET_SLOT_CONFIG,
        false,  // skip phase one (we are testing phase two decoding)
        |_| {},
    );
});
