//! One-shot diagnostic (#764): load a ledger snapshot and print the
//! Plutus cost-model entry counts + protocol version. Decides whether the
//! V3 cost model is present (Some(N)) or wiped (None) at a given epoch.
//!
//! Usage:
//!   cargo run --release -p dugite-ledger --example inspect_costmodels -- <snapshot.bin>

use std::path::PathBuf;

use dugite_ledger::LedgerState;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: {} <snapshot.bin>", args[0]);
        std::process::exit(1);
    }
    let snapshot_path = PathBuf::from(&args[1]);
    let state = LedgerState::load_snapshot(&snapshot_path)?;
    println!(
        "snapshot={} epoch={} tip_slot={:?} era={:?}",
        snapshot_path.display(),
        state.epoch.0,
        state.tip.point.slot(),
        state.era
    );
    println!(
        "pv_major={} pv_minor={}",
        state.epochs.protocol_params.protocol_version_major,
        state.epochs.protocol_params.protocol_version_minor
    );
    let cm = &state.epochs.protocol_params.cost_models;
    println!(
        "cost_models.plutus_v1: {:?}",
        cm.plutus_v1.as_ref().map(|v| v.len())
    );
    println!(
        "cost_models.plutus_v2: {:?}",
        cm.plutus_v2.as_ref().map(|v| v.len())
    );
    println!(
        "cost_models.plutus_v3: {:?}",
        cm.plutus_v3.as_ref().map(|v| v.len())
    );
    println!(
        "cost_models.plutus_v4: {:?}",
        cm.plutus_v4.as_ref().map(|v| v.len())
    );
    // prev_protocol_params cost models (used by RUPD + sometimes phase-2 forecast)
    let pcm = &state.epochs.prev_protocol_params.cost_models;
    println!(
        "prev_protocol_params.plutus_v3: {:?}",
        pcm.plutus_v3.as_ref().map(|v| v.len())
    );
    Ok(())
}
