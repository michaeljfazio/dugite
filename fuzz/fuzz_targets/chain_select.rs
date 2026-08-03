//! Fuzz target for `dugite_consensus::chain_selection::chain_preference` and
//! `ChainSelection::prefer_chain`.
//!
//! Constructs two `ChainFragment`s and a `Tip` pair from fuzz bytes and calls
//! the real fork-choice selector.  The selector must never panic regardless of
//! block numbers, slot values, or era flags drawn from the fuzzer.
//!
//! Byte layout:
//!   Byte 0  : era selector (0 = Byron, 1 = Shelley, 2+ = Conway)
//!   Bytes 1-8  : current chain block number (u64 LE)
//!   Bytes 9-16 : candidate chain block number (u64 LE)
//!   Bytes 17-24: current chain slot (u64 LE)
//!   Bytes 25-32: candidate chain slot (u64 LE)
//!   Bytes 33-40: slot_window for chain_preference (u64 LE; 0 means u64::MAX)
//!
//! Run with: cargo +nightly fuzz run fuzz_chain_select

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_consensus::chain_fragment::ChainFragment;
use dugite_consensus::chain_selection::{chain_preference, ChainPreference, ChainSelection};
use dugite_primitives::block::{
    BlockHeader, OperationalCert, Point, ProtocolVersion, Tip, VrfOutput,
};
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};

/// Read a u64 LE from `data[offset..offset+8]`, zero-padding if short.
fn read_u64(data: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    let end = data.len().min(offset + 8);
    if end > offset {
        buf[..end - offset].copy_from_slice(&data[offset..end]);
    }
    u64::from_le_bytes(buf)
}

/// Build a minimal, structurally-valid `BlockHeader` for fuzz use.
/// We zero every field that doesn't affect chain selection logic.
fn make_header(slot: SlotNo, block_no: BlockNo, era: Era) -> BlockHeader {
    let protocol_major = match era {
        Era::Byron => 1,
        Era::Shelley => 2,
        Era::Allegra => 3,
        Era::Mary => 4,
        Era::Alonzo => 5,
        Era::Babbage => 7,
        Era::Conway => 9,
        // TODO(dijkstra): expand fuzz coverage once Dijkstra ledger support lands.
        // The era selector below only emits Byron/Shelley/Conway, so this arm is
        // currently unreachable — it exists to satisfy match exhaustiveness.
        Era::Dijkstra => 12,
    };
    BlockHeader {
        header_hash: Hash32::ZERO,
        prev_hash: Hash32::ZERO,
        issuer_vkey: vec![0u8; 32],
        vrf_vkey: vec![0u8; 32],
        vrf_result: VrfOutput {
            output: vec![0u8; 64],
            proof: vec![0u8; 80],
        },
        block_number: block_no,
        slot,
        epoch_nonce: Hash32::ZERO,
        body_size: 0,
        body_hash: Hash32::ZERO,
        operational_cert: OperationalCert {
            hot_vkey: vec![0u8; 32],
            sequence_number: 0,
            kes_period: 0,
            sigma: vec![0u8; 64],
        },
        protocol_version: ProtocolVersion {
            major: protocol_major,
            minor: 0,
        },
        kes_signature: vec![],
        nonce_vrf_output: vec![],
        nonce_vrf_proof: vec![],
        prev_nonce: None,
        raw_header_body: None,
    }
}

fuzz_target!(|data: &[u8]| {
    // Byte 0: era selector
    let era = match data.first().copied().unwrap_or(1) % 3 {
        0 => Era::Byron,
        1 => Era::Shelley,
        _ => Era::Conway,
    };

    let current_block_no = BlockNo(read_u64(data, 1).min(u32::MAX as u64) as u64);
    let candidate_block_no = BlockNo(read_u64(data, 9).min(u32::MAX as u64) as u64);
    let current_slot = SlotNo(read_u64(data, 17));
    let candidate_slot = SlotNo(read_u64(data, 25));
    let raw_window = read_u64(data, 33);
    let slot_window = if raw_window == 0 {
        u64::MAX
    } else {
        raw_window
    };

    // --- ChainSelection::prefer_chain (uses Tip, no headers) ---
    let current_tip = Tip {
        point: if current_block_no.0 == 0 {
            Point::Origin
        } else {
            Point::Specific(current_slot, Hash32::ZERO)
        },
        block_number: current_block_no,
    };
    let candidate_tip = Tip {
        point: if candidate_block_no.0 == 0 {
            Point::Origin
        } else {
            Point::Specific(candidate_slot, Hash32::ZERO)
        },
        block_number: candidate_block_no,
    };

    let mut sel = ChainSelection::new();
    sel.set_tip(current_tip);
    let _ = sel.prefer_chain(&candidate_tip, era);
    let _ = sel.prefer(&candidate_tip);

    // --- chain_preference (uses ChainFragment) ---
    let current_header = make_header(current_slot, current_block_no, era);
    let candidate_header = make_header(candidate_slot, candidate_block_no, era);

    let current_frag = if current_block_no.0 == 0 {
        ChainFragment::new(Point::Origin)
    } else {
        ChainFragment::from_headers(Point::Origin, [current_header])
    };

    let candidate_frag = if candidate_block_no.0 == 0 {
        ChainFragment::new(Point::Origin)
    } else {
        ChainFragment::from_headers(Point::Origin, [candidate_header])
    };

    // Must never panic.
    let result = chain_preference(&current_frag, &candidate_frag, slot_window);
    let _ = matches!(
        result,
        ChainPreference::PreferCurrent | ChainPreference::PreferCandidate | ChainPreference::Equal
    );
});
