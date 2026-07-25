//! One-shot diagnostic: summarise a snapshot's Conway governance state —
//! active proposals (with deposit + return address), committee membership, and
//! whether a specific `GovActionId` is still pending.
//!
//! Usage:
//!   cargo run --release -p dugite-ledger --example probe_gov -- \
//!       <snapshot.bin> [<tx-hash-hex>#<index>]

use dugite_ledger::LedgerState;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: probe_gov <snapshot.bin> [txhash#idx]")?;
    let target = args.next();

    let st = LedgerState::load_snapshot(Path::new(&path))?;
    let g = &st.gov.governance;
    println!(
        "snapshot epoch={} tip_slot={:?}",
        st.epoch.0,
        st.tip.point.slot().map(|s| s.0)
    );
    println!(
        "treasury={} reserves={}",
        st.epochs.treasury.0, st.epochs.reserves.0
    );
    println!(
        "proposals={} dreps={} committee_expiration={} committee_hot_keys={} resigned={}",
        g.proposals.len(),
        g.dreps.len(),
        g.committee_expiration.len(),
        g.committee_hot_keys.len(),
        g.committee_resigned.len(),
    );
    let total_deposits: u64 = g.proposals.values().map(|p| p.procedure.deposit.0).sum();
    println!("sum_of_pending_proposal_deposits={total_deposits}");
    let show = |n: &str, r: &Option<dugite_primitives::transaction::GovActionId>| {
        match r {
            None => println!("enacted_root[{n}] = None"),
            Some(id) => println!(
                "enacted_root[{n}] = {}#{}",
                id.transaction_id.to_hex(),
                id.action_index
            ),
        };
    };
    show("pparam_update", &g.enacted_pparam_update);
    show("hard_fork", &g.enacted_hard_fork);
    show("committee", &g.enacted_committee);
    show("constitution", &g.enacted_constitution);

    let mut rows: Vec<(String, u64, u64, String)> = g
        .proposals
        .iter()
        .map(|(id, p)| {
            (
                format!("{}#{}", id.transaction_id.to_hex(), id.action_index),
                p.procedure.deposit.0,
                p.expires_epoch.0,
                LedgerState::reward_account_to_hash(&p.procedure.return_addr).to_hex(),
            )
        })
        .collect();
    rows.sort();
    println!("--- pending proposals (id, deposit, expires_epoch, return_cred) ---");
    for (id, dep, exp, ret) in &rows {
        println!("  {id} deposit={dep} expires={exp} return_cred={ret}");
    }

    if let Some(t) = target {
        let (h, i) = t.split_once('#').ok_or("target must be <txhash>#<index>")?;
        let idx: u64 = i.parse()?;
        let hit = rows.iter().any(|(id, _, _, _)| *id == format!("{h}#{idx}"));
        println!(
            "--- target {h}#{idx}: {} ---",
            if hit {
                "STILL PENDING in dugite"
            } else {
                "not present (removed/refunded)"
            }
        );
    }
    Ok(())
}
