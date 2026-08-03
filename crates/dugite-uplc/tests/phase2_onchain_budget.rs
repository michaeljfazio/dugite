//! On-chain cost-model budget conformance — closes a coverage gap.
//!
//! The UPLC conformance suite (`tests/conformance.rs`) evaluates with
//! `BudgetTracker::new_counting()`, which uses `MachineCosts::DEFAULT` /
//! `BuiltinCosts::DEFAULT` — NOT the on-chain cost model applied via
//! `cost_apply::apply_v{1,2,3}` + `BudgetTracker::with_applied`. So the
//! production budget path (the one that runs during sync) had no test.
//!
//! These fixtures are REAL preprod Babbage transactions captured during a
//! from-genesis network sync (`DUGITE_PHASE2_DUMP_DIR`), each with on-chain
//! `is_valid = true` — i.e. the network's (Haskell) nodes evaluated their
//! scripts and they PASSED within the redeemers' declared exUnits. A correct
//! dugite CEK, fed the same on-chain cost model, must therefore also validate
//! them within their declared budget. `eval_phase_two_raw` caps each redeemer
//! at its declared exUnits and returns `Err(BudgetExhausted)` if the CEK
//! exceeds it, so a passing eval == "validated within declared budget".
//!
//! These were FAILING (#730): dugite over-charged real Babbage scripts by a
//! fixed +542,489 cpu / +1,102 mem. Root cause was NOT a cost-parameter or
//! CEK-accounting bug (both byte-exact) — it was the PlutusV1/V2
//! `txInfoValidRange` upper-bound closure. cardano-ledger's Alonzo/Babbage
//! `transValidityInterval` builds a ttl-only range with `PV1.to t =
//! UpperBound (Finite t) True` (CLOSED), but dugite was emitting `False`
//! (open), flipping one boundary comparison in every validity-range check and
//! costing one extra `equalsInteger` + `ifThenElse` + ~11 CEK steps. Fixed in
//! `script_context.rs::PosixTimeRange::to_data` (era-aware closure). tx0 now
//! consumes EXACTLY its declared exUnits (cpu=512453022, mem=1734298).

use dugite_uplc::builtin::semantics::SemanticsVariant;
use dugite_uplc::machine::cost::BudgetTracker;
use dugite_uplc::machine::step::evaluate_with_budget;
use dugite_uplc::phase_two::{eval_phase_two_raw, SlotConfig};
use dugite_uplc::Program;
use std::path::PathBuf;

fn hexd(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Evaluate a captured on-chain phase-2 fixture through the REAL on-chain
/// cost-model path. Returns `Ok(redeemer_count)` when every script validates
/// within its declared exUnits, `Err(message)` otherwise.
fn eval_onchain_fixture(name: &str) -> Result<usize, String> {
    eval_onchain_fixture_expecting(name, true)
}

fn eval_onchain_fixture_expecting(name: &str, expect_valid: bool) -> Result<usize, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/phase2_onchain")
        .join(name);
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read fixture"))
            .expect("parse fixture");

    assert_eq!(
        doc["is_valid"].as_bool(),
        Some(expect_valid),
        "{name}: fixture must be an on-chain is_valid={expect_valid} tx"
    );

    let tx_cbor = hexd(doc["tx_cbor"].as_str().unwrap());
    let utxos: Vec<(Vec<u8>, Vec<u8>)> = doc["utxo_pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                hexd(p["input"].as_str().unwrap()),
                hexd(p["output"].as_str().unwrap()),
            )
        })
        .collect();
    let cost_models_cbor: Option<Vec<u8>> = doc["cost_models_cbor"].as_str().map(hexd);
    let max_ex = (
        doc["max_ex_cpu"].as_u64().unwrap(),
        doc["max_ex_mem"].as_u64().unwrap(),
    );
    let slot_config = SlotConfig {
        network_start_unix_seconds: doc["sc_network_start_unix_seconds"].as_u64().unwrap(),
        slot_zero_offset: doc["sc_slot_zero_offset"].as_u64().unwrap(),
        slot_length_ms: doc["sc_slot_length_ms"].as_u64().unwrap() as u32,
        safe_zone_horizon_slot: doc["sc_safe_zone_horizon_slot"].as_u64(),
    };
    // protocol_major selects the V1/V2 BuiltinSemanticsVariant; Babbage = 8.
    let major_pv = doc["protocol_major"].as_u64().unwrap_or(8) as u32;

    eval_phase_two_raw(
        &tx_cbor,
        &utxos,
        cost_models_cbor.as_deref(),
        max_ex,
        slot_config,
        major_pv,
        false,
        &mut (),
    )
    .map(|r| r.len())
    .map_err(|e| e.to_string())
}

/// Every captured on-chain-valid Babbage tx must validate within its declared
/// budget under dugite's on-chain cost-model CEK path.
#[test]
fn onchain_babbage_scripts_validate_within_declared_budget() {
    let fixtures = [
        "tx0.json",
        "tx1.json",
        "tx6.json",
        // Zero-quantity multiasset entries in TxOut values: Haskell's
        // decoder prunes them pre-Conway (`pruneZeroMultiAsset`), so the
        // Plutus ScriptContext never contains them. Keeping the zero entry
        // made the spend validator walk one extra Value node — a fixed
        // +15,423,657 cpu / +26,364 mem over-charge that pushed these
        // on-chain-valid txs over their declared exUnits (#730).
        "zero_asset_tx0.json",
        "zero_asset_tx1.json",
    ];
    let mut failures = Vec::new();
    for f in fixtures {
        match eval_onchain_fixture(f) {
            Ok(n) => assert!(n > 0, "{f}: expected at least one redeemer"),
            Err(e) => failures.push(format!("{f}: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "on-chain-valid scripts must pass phase-2 within their declared budget (#730):\n{}",
        failures.join("\n")
    );
}

/// The reverse polarity: a captured on-chain `is_valid = FALSE` Babbage tx
/// (the network's Haskell nodes evaluated its scripts and they FAILED) must
/// also fail dugite's phase-2. Before the zero-quantity pruning fix dugite
/// PASSED all three of this tx's redeemers (a wrong-accept): its un-pruned
/// ScriptContext value maps steered the validators down a cheaper, succeeding
/// path (#730).
#[test]
fn onchain_invalid_babbage_tx_fails_phase_two() {
    let result = eval_onchain_fixture_expecting("zero_asset_invalid_tx4.json", false);
    assert!(
        result.is_err(),
        "on-chain is_valid=false tx must fail dugite phase-2, got {result:?}"
    );
}

/// Regression test for issue #761 Bug 2: PlutusV3 SPEND validator on mainnet
/// tx 71579b77 fails with `appendByteString: type error: expected ByteString,
/// got Discriminant(3)` (= Value::Builtin). The tx is `is_valid=true` on-chain
/// (Conway PV9, epoch 523, slot 140988497).
///
/// This test pins that the error no longer occurs after the fix.
#[test]
fn onchain_v3_spend_71579b77_validates() {
    match eval_onchain_fixture("tx_v3_spend_71579b77.json") {
        Ok(n) => assert!(n > 0, "expected at least one redeemer to be evaluated"),
        Err(e) => panic!(
            "issue #761 Bug 2 regression: V3 SPEND tx 71579b77 must pass phase-2\nerror: {e}"
        ),
    }
}

/// Narrow isolation test for issue #761 Bug 2: directly evaluates the
/// flat-encoded applied program (script + ctx already pre-applied) through
/// dugite's CEK machine with LATEST (V3 strict) semantics.
///
/// The program and its context are already applied, so any failure here is a
/// pure CEK bug independent of ScriptContext construction — which is the whole
/// point of isolating it this way.
///
/// The expected result is `(con unit ())`: this is a real on-chain V3 spend
/// (tx 71579b77) that the CHAIN accepted, so the transaction's own inclusion is
/// the oracle. An earlier version of this comment cited `aiken uplc eval` as
/// the authority; that was circular and is not why the expectation holds.
///
/// The flat file was captured at:
/// `DUGITE_DUMP_APPLIED_DIR=/tmp/v3_dump cargo nextest run … onchain_v3_spend_71579b77_validates`
/// and is embedded inline to avoid a runtime path dependency.
#[test]
fn cek_v3_spend_71579b77_flat_evaluates() {
    // Flat bytes of the applied program (script applied to ctx), captured from
    // DUGITE_DUMP_APPLIED_DIR and committed as a hermetic fixture so this test
    // runs in CI (no /tmp runtime dependency).
    // Expected: unit, with no error — the tx this was captured from is on
    // chain, so the reference implementations accepted it by construction.
    // To regenerate: DUGITE_DUMP_APPLIED_DIR=/tmp/v3_dump cargo nextest run -p
    //   dugite-uplc -E 'test(onchain_v3_spend_71579b77_validates)' then copy
    //   /tmp/v3_dump/applied-Spend-0.flat over the fixture below.
    let flat_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/phase2_onchain")
        .join("applied-Spend-0.flat");
    let flat_bytes = std::fs::read(&flat_path)
        .unwrap_or_else(|e| panic!("read flat fixture {}: {e}", flat_path.display()));
    let prog = Program::from_flat(&flat_bytes).expect("applied flat should parse");
    assert_eq!(
        prog.version,
        (
            num_bigint::BigUint::from(1u8),
            num_bigint::BigUint::from(1u8),
            num_bigint::BigUint::ZERO
        ),
        "script should be UPLC 1.1.0 (Conway)"
    );
    let mut tracker = BudgetTracker::new_counting();
    let result = evaluate_with_budget(prog.term, &mut tracker, None, SemanticsVariant::LATEST);
    match result {
        Ok(_) => {} // Pass
        Err(e) => panic!(
            "issue #761 Bug 2 CEK regression: flat program should evaluate to unit\nerror: {e}"
        ),
    }
}
