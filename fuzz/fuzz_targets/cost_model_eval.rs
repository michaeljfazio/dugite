//! Fuzz target for Plutus Phase-2 evaluation through `dugite-ledger`.
//!
//! This is the highest-blast-radius boundary in the Dugite stack: arbitrary
//! bytes parsed as a Cardano transaction and then fed into the phase-2
//! evaluator. Any panic here is a DoS finding in dugite — a peer-supplied
//! transaction that crashes the node.
//!
//! # A second path used to live here (#970)
//!
//! There was a "Path B" that called `uplc::tx::eval_phase_two_raw` from
//! aiken-lang/uplc directly, plus a panic hook that SWALLOWED any panic whose
//! location pointed into `~/.cargo` so upstream's `unwrap()`s would not fail
//! CI. That fuzzed a third-party library dugite does not ship, and the hook
//! suppressed panics from every third-party crate in the graph, not just
//! Aiken's — so a genuine panic reached through a dependency would have been
//! silently discarded. Both are gone.
//!
//! # Budget bounding
//!
//! The execution budget is capped tightly so each fuzzer iteration stays well
//! under 1 second:
//!   - CPU steps: 10_000_000  (1/1000th of mainnet max)
//!   - Memory units: 14_000   (1/1000th of mainnet max)
//!
//! `cost_models_cbor = None` is passed to the evaluator.  Without a
//! cost model it uses `ExBudget::default()` (maximum budget),
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
//!   3. Runs dugite-uplc's phase-2 evaluator — which decodes the transaction,
//!      builds the `DataLookupTable` (script-context construction), runs the
//!      CEK machine per redeemer, and returns per-redeemer `EvalResult`s.
//!   4. Interprets the `EvalResult` according to V1/V2/V3 semantics.
//!
//! Step 3 is where the script-context builder lives; this target provides
//! indirect coverage of `TxInfoV3::from_transaction`,
//! `DataLookupTable::from_transaction`, and `get_tx_in_info_v2` without
//! requiring a direct the legacy primitives crate dependency in the fuzz crate.
//!
//! # What we look for
//!
//! - Panics (unwrap/expect/index-out-of-bounds) inside uplc or the in-house decoders.
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
use dugite_ledger::utxo::UtxoLookup;
use dugite_ledger::SlotConfig;
use dugite_primitives::transaction::{TransactionInput, TransactionOutput};

// ---------------------------------------------------------------------------
// Tight execution budget — caps CEK iterations for valid scripts.
// Convention: (cpu_steps, mem_units).
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

// Conway protocol major version — selects V3 script semantics in the evaluator.
const CONWAY_PROTOCOL_MAJOR: u32 = 10;

// Panic guards
//
// Panics in dugite are FINDINGS: the default libfuzzer hook aborts, which is
// what we want. `catch_unwind` below is retained only so a panic is attributed
// to the specific era that produced it rather than aborting mid-loop.

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
    // call `evaluate_plutus_scripts`, which resolves UTxO pairs and runs
    // dugite-uplc's CEK machine.
    //
    // Era IDs: 6 = Alonzo, 7 = Babbage, 8 = Conway.
    // ---------------------------------------------------------------------------
    let slot_config = SlotConfig::default();

    for era_id in [6u16, 7, 8] {
        if let Ok(tx) = dugite_serialization::decode_transaction(era_id, data) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                evaluate_plutus_scripts(
                    &tx,
                    &EmptyUtxoSet,
                    None, // no cost model; CEK budget applied via FUZZ_BUDGET below
                    FUZZ_BUDGET,
                    &slot_config,
                    CONWAY_PROTOCOL_MAJOR,
                )
            }));
            // Only try the first era that successfully decodes.
            break;
        }
    }
});
