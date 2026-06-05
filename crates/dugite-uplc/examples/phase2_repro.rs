//! Reproduce a Phase-2 evaluation divergence from a dump produced by
//! `dugite_ledger::plutus::maybe_dump_phase2_divergence` (set
//! `DUGITE_PHASE2_DUMP_DIR` on the node).
//!
//! Usage: `cargo run -p dugite-uplc --example phase2_repro -- <dump.json>`
//!
//! Prints dugite's per-redeemer consumed ExUnits / trace logs (on success) or
//! the typed failure (on error), so a divergence against the on-chain
//! `is_valid` flag can be root-caused offline without rebuilding the node.

use dugite_uplc::phase_two::{eval_phase_two_raw, SlotConfig};

fn hexd(s: &str) -> Vec<u8> {
    hex::decode(s).expect("valid hex in dump")
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: phase2_repro <dump.json>");
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read dump")).expect("parse json");

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

    println!(
        "tx_idx={} is_valid(on-chain)={}",
        doc["tx_idx"], doc["is_valid"]
    );
    println!(
        "utxos={} cost_models={} max_ex=(cpu={}, mem={})",
        utxos.len(),
        cost_models_cbor.as_ref().map(|c| c.len()).unwrap_or(0),
        max_ex.0,
        max_ex.1
    );

    // `protocol_major` selects the V1/V2 BuiltinSemanticsVariant. The dump may
    // carry it; default 8 (pre-Conway VariantA) for older divergence dumps.
    let major_pv = doc["protocol_major"].as_u64().unwrap_or(8) as u32;
    match eval_phase_two_raw(
        &tx_cbor,
        &utxos,
        cost_models_cbor.as_deref(),
        max_ex,
        slot_config,
        major_pv,
        false,
        &mut (),
    ) {
        Ok(results) => {
            println!(
                "\n=== dugite EVAL RESULT: Ok ({} redeemer(s)) ===",
                results.len()
            );
            for r in &results {
                println!(
                    "  redeemer tag={:?} index={} consumed cpu={} mem={}",
                    r.tag, r.index, r.consumed.cpu, r.consumed.mem
                );
                for l in &r.logs {
                    println!("    trace: {l}");
                }
            }
            println!(
                "\n>>> DIVERGENCE: on-chain is_valid=false (scripts MUST fail) but dugite says PASS"
            );
        }
        Err(e) => {
            println!("\n=== dugite EVAL RESULT: Err ===\n  {e}");
        }
    }
}
