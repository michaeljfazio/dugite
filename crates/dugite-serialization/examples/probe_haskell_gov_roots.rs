//! One-shot diagnostic: decode a cardano-node / Mithril ledger `state` file and
//! print the Conway governance enacted roots (`pRoots` / `toPrevGovActionIds`)
//! plus the active proposal count.
//!
//! Usage:
//!   cargo run --release -p dugite-serialization --example probe_haskell_gov_roots -- <state-file>

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: probe_haskell_gov_roots <ledger/<slot>/state>")?;
    let data = std::fs::read(&path)?;
    println!("state file: {} ({} bytes)", path, data.len());

    let hs = dugite_serialization::haskell_snapshot::decode_state_file(&data)?;
    let raw = &hs.new_epoch_state.gov_state.proposals_raw;
    println!("proposals_raw: {} bytes", raw.len());

    let p = dugite_serialization::haskell_snapshot::decode_proposals_with_roots(raw)?;
    println!("active proposals: {}", p.actions.len());
    let show =
        |n: &str, r: &Option<dugite_serialization::haskell_snapshot::HaskellGovActionId>| match r {
            None => println!("  {n:14} = SNothing"),
            Some(id) => println!("  {n:14} = {}#{}", id.tx_hash.to_hex(), id.index),
        };
    println!("enacted roots (GovRelation StrictMaybe):");
    show("pparam_update", &p.roots.pparam_update);
    show("hard_fork", &p.roots.hard_fork);
    show("committee", &p.roots.committee);
    show("constitution", &p.roots.constitution);
    Ok(())
}
