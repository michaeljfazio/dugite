//! Fuzz the `dugite-node` topology parser (issue #975).
//!
//! `topology.rs` reads JSON from disk carrying peer addresses, DNS names and
//! valency counts, and drives peer selection from them. It was unreachable
//! from any test harness outside the binary until #975 exposed it through the
//! lib target.
//!
//! ## Properties
//!
//! - the parser never panics on arbitrary input
//! - none of the accessors panic on a topology that deserialised — including
//!   `effective_hot_valency` / `effective_warm_valency`, which do arithmetic
//!   on attacker-controlled counts, and `ledger_peers_enabled`, which compares
//!   `useLedgerAfterSlot` against the current slot
//!
//! Seeded from every network's real topology JSON.
//!
//! Run with: cargo +nightly fuzz run fuzz_topology_parse -- -max_total_time=300

#![no_main]

// Compiled in directly rather than via the `dugite-node` crate — see the note
// in genesis_parse.rs. `topology.rs` has no `crate::` references.
// These files are `pub` in dugite-node, but inside a fuzz binary they are a
// private module, so every item this target does not call trips dead_code.
// That is an artefact of the inclusion, not a finding.
#[allow(dead_code)]
#[path = "../../crates/dugite-node/src/topology.rs"]
mod topology;

use libfuzzer_sys::fuzz_target;
use topology::Topology;

fuzz_target!(|data: &[u8]| {
    // First 8 bytes select a current slot for the ledger-peers predicate; the
    // rest is the topology document.
    let (slot, json) = if data.len() >= 8 {
        (
            u64::from_le_bytes(data[..8].try_into().unwrap()),
            &data[8..],
        )
    } else {
        (0u64, data)
    };

    let Ok(text) = std::str::from_utf8(json) else {
        return;
    };

    let Ok(topology) = serde_json::from_str::<Topology>(text) else {
        return;
    };

    // Every accessor the node calls on a freshly-loaded topology.
    let _ = topology.all_peers();
    let _ = topology.detailed_peers();
    let _ = topology.ledger_peers_enabled(slot);
    let _ = topology.has_bootstrap_peers();
    let _ = topology.has_trustable_peers();

    for group in &topology.local_roots {
        let _ = group.is_behind_firewall();
        let _ = group.effective_hot_valency();
        let _ = group.effective_warm_valency();
    }
});
