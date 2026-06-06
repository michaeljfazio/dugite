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
//! Currently these FAIL: dugite's CEK over-charges memory by <1% on real
//! Babbage scripts (issue #730 — not a cost-parameter bug; parameters are
//! byte-exact on-chain, verified; the divergence is in CEK step accounting).
//! Marked `#[ignore]` until the #730 fix lands, at which point un-ignore so
//! this guards the on-chain budget path in CI permanently.

use dugite_uplc::phase_two::{eval_phase_two_raw, SlotConfig};
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/phase2_onchain")
        .join(name);
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read fixture"))
            .expect("parse fixture");

    assert_eq!(
        doc["is_valid"].as_bool(),
        Some(true),
        "{name}: fixture must be an on-chain is_valid=true tx"
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
#[ignore = "#730: dugite CEK over-charges memory <1% on real Babbage scripts; un-ignore when fixed"]
fn onchain_babbage_scripts_validate_within_declared_budget() {
    let fixtures = ["tx0.json", "tx1.json", "tx6.json"];
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
