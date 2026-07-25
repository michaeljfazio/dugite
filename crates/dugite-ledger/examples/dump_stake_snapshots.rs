//! Dump a ledger snapshot's `mark`/`set`/`go` stake snapshots as JSON so they
//! can be diffed against `cardano-cli query stake-snapshot --all-stake-pools`.
//!
//! `cardano-cli` reports, per pool, `stakeMark`/`stakeSet`/`stakeGo` plus the
//! chain-wide `activeStakeMark`/`activeStakeSet`/`activeStakeGo` totals — the
//! exact analogues of dugite's three `StakeSnapshot`s. Diffing them localises a
//! stake-distribution divergence to a single pool (see issue #898).
//!
//! Usage:
//!   cargo run --release -p dugite-ledger --example dump_stake_snapshots -- \
//!       <snapshot.bin> > dugite-snapshots.json

use dugite_ledger::state::StakeSnapshot;
use dugite_ledger::LedgerState;
use std::path::Path;

fn emit(name: &str, snap: Option<&StakeSnapshot>, first: bool) {
    if !first {
        println!(",");
    }
    match snap {
        None => print!("  \"{name}\": null"),
        Some(s) => {
            // `total` mirrors Haskell `sumAllActiveStake ssActiveStake`: every
            // entry, including stake delegated to pools no longer registered.
            let total: u64 = s.pool_stake.values().map(|l| l.0).sum();
            let registered: u64 = s
                .pool_stake
                .iter()
                .filter(|(p, _)| s.pool_params.contains_key(p))
                .map(|(_, l)| l.0)
                .sum();
            let mut pools: Vec<(String, u64, bool)> = s
                .pool_stake
                .iter()
                .map(|(p, l)| (p.to_hex(), l.0, s.pool_params.contains_key(p)))
                .collect();
            pools.sort();
            println!("  \"{name}\": {{");
            println!("    \"epoch_label\": {},", s.epoch.0);
            println!("    \"total_active_stake\": {total},");
            println!("    \"registered_pool_stake\": {registered},");
            println!("    \"pool_count\": {},", pools.len());
            println!("    \"credential_count\": {},", s.stake_distribution.len());
            println!(
                "    \"credential_stake_sum\": {},",
                s.stake_distribution.values().map(|l| l.0).sum::<u64>()
            );
            println!("    \"pools\": {{");
            for (i, (hex, stake, reg)) in pools.iter().enumerate() {
                let comma = if i + 1 == pools.len() { "" } else { "," };
                println!("      \"{hex}\": {{\"stake\": {stake}, \"registered\": {reg}}}{comma}");
            }
            println!("    }}");
            print!("  }}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: dump_stake_snapshots <snapshot.bin>")?;
    let st = LedgerState::load_snapshot(Path::new(&path))?;
    println!("{{");
    println!("  \"snapshot_epoch\": {},", st.epoch.0);
    println!(
        "  \"tip_slot\": {},",
        st.tip.point.slot().map(|s| s.0).unwrap_or(0)
    );
    println!("  \"reserves\": {},", st.epochs.reserves.0);
    println!("  \"treasury\": {},", st.epochs.treasury.0);
    emit("mark", st.epochs.snapshots.mark.as_ref(), true);
    emit("set", st.epochs.snapshots.set.as_ref(), false);
    emit("go", st.epochs.snapshots.go.as_ref(), false);
    println!();
    println!("}}");
    Ok(())
}
