//! One-shot diagnostic: run the FULL Mithril/Haskell-snapshot import path on a
//! cardano-node `ledger/<slot>/state` file and report the Conway governance
//! state dugite ends up with.
//!
//! This is the same code path `dugite-node mithril-import` uses
//! (`decode_state_file` → `LedgerState::from_haskell_snapshot`), so it verifies
//! the import end-to-end without downloading a multi-GB Mithril snapshot.
//!
//! Issue #898: the enacted governance roots (`Proposals.pRoots`) used to be
//! discarded here, leaving every `enacted_*` root `None`. That made the GOV
//! rule silently drop any later proposal chaining onto a real root, stranding
//! its deposit and ultimately halting chain advance. All four roots printed
//! below must match `cardano-cli`'s view of the same ledger state.
//!
//! Usage:
//!   cargo run --release -p dugite-ledger --example probe_haskell_import -- \
//!       <cardano-node-db>/ledger/<slot>/state

use dugite_ledger::LedgerState;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: probe_haskell_import <ledger/<slot>/state>")?;
    let data = std::fs::read(&path)?;
    println!("state file: {path} ({} bytes)", data.len());

    let hs = dugite_serialization::haskell_snapshot::decode_state_file(&data)?;
    println!(
        "decoded Haskell ExtLedgerState: epoch={} tip_slot={}",
        hs.epoch.0, hs.tip_slot.0
    );

    let state = LedgerState::from_haskell_snapshot(&hs);
    let g = &state.gov.governance;
    println!("imported LedgerState:");
    println!("  active proposals        = {}", g.proposals.len());
    println!(
        "  pending proposal deposits = {}",
        g.proposals
            .values()
            .map(|p| p.procedure.deposit.0)
            .sum::<u64>()
    );
    println!(
        "  committee members       = {}",
        g.committee_expiration.len()
    );
    println!("  dreps                   = {}", g.dreps.len());

    let show = |name: &str, r: &Option<dugite_primitives::transaction::GovActionId>| match r {
        None => println!("  enacted_{name:<14} = None"),
        Some(id) => println!(
            "  enacted_{name:<14} = {}#{}",
            id.transaction_id.to_hex(),
            id.action_index
        ),
    };
    show("pparam_update", &g.enacted_pparam_update);
    show("hard_fork", &g.enacted_hard_fork);
    show("committee", &g.enacted_committee);
    show("constitution", &g.enacted_constitution);

    let populated = [
        g.enacted_pparam_update.is_some(),
        g.enacted_hard_fork.is_some(),
        g.enacted_committee.is_some(),
        g.enacted_constitution.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    println!("=> {populated}/4 enacted roots populated (#898: was 0/4)");
    Ok(())
}
