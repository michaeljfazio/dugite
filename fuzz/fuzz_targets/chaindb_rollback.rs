//! Fuzz target for ChainDB add_block / rollback_to_point sequences.
//!
//! Interprets fuzz bytes as a sequence of operations against a real ChainDB
//! in a tempdir. Operations are:
//!
//!   0 — add_block: insert a block with fuzz-derived slot, block_no, hashes,
//!       and a short CBOR payload (the exact bytes are not validated by
//!       ChainDB, so any byte sequence is acceptable).
//!   1 — rollback_to_point: roll back to a randomly selected previously-seen
//!       block hash, or to Origin if none are available.
//!   2 — flush_to_immutable: flush the k-deep window to ImmutableDB.
//!   3 — get_tip: read-only query (must not panic).
//!
//! The fuzz driver uses a small key space (16 distinct hashes) to ensure
//! that add_block/rollback interactions collide frequently and exercise the
//! fork/switch paths of VolatileDB.
//!
//! Panics from assertion failures or unwrap are bugs; Err returns are fine.
//!
//! Run with: cargo +nightly fuzz run fuzz_chaindb_rollback -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_primitives::block::Point;
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_storage::ChainDB;

/// Number of distinct synthetic block hashes in the key space.
/// Small enough for frequent hash collisions / re-insertion attempts.
const HASH_SPACE: usize = 16;

/// Build the i-th synthetic block hash (deterministic, non-zero, no panics).
fn synthetic_hash(i: usize) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes[0] = (i & 0xFF) as u8;
    bytes[1] = ((i >> 8) & 0xFF) as u8;
    bytes[31] = 0xBE; // sentinel to distinguish from Hash32::ZERO
    Hash32::from_bytes(bytes)
}

/// Operation types decoded from fuzz bytes.
enum Op {
    AddBlock {
        hash_idx: usize,
        prev_idx: usize,
        slot: u64,
        block_no: u64,
        cbor: Vec<u8>,
    },
    RollbackTo {
        hash_idx: usize,
        slot: u64,
    },
    FlushToImmutable,
    GetTip,
}

/// Parse fuzz bytes into a bounded sequence of ChainDB operations.
fn parse_ops(data: &[u8], max_ops: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut pos = 0;

    while pos < data.len() && ops.len() < max_ops {
        let control = data[pos];
        pos += 1;

        let op_type = control >> 6; // top 2 bits
        let hash_idx = (control & 0x0F) as usize % HASH_SPACE; // bottom 4 bits

        match op_type {
            0 => {
                // AddBlock: consume 3 more bytes for prev_idx, slot, block_no
                let prev_idx = data
                    .get(pos)
                    .copied()
                    .map(|b| b as usize % HASH_SPACE)
                    .unwrap_or(0);
                let slot = data
                    .get(pos + 1)
                    .copied()
                    .map(|b| b as u64 * 1000 + hash_idx as u64)
                    .unwrap_or(hash_idx as u64);
                let block_no = data
                    .get(pos + 2)
                    .copied()
                    .map(|b| b as u64)
                    .unwrap_or(0);
                pos += 3;

                // Use a tiny fixed CBOR payload — ChainDB does not decode it.
                let cbor = vec![0x80u8]; // CBOR empty array

                ops.push(Op::AddBlock {
                    hash_idx,
                    prev_idx,
                    slot,
                    block_no,
                    cbor,
                });
            }
            1 => {
                // RollbackTo: consume 1 byte for a synthetic slot value
                let slot = data
                    .get(pos)
                    .copied()
                    .map(|b| b as u64 * 1000)
                    .unwrap_or(0);
                pos += 1;
                ops.push(Op::RollbackTo { hash_idx, slot });
            }
            2 => {
                ops.push(Op::FlushToImmutable);
            }
            _ => {
                ops.push(Op::GetTip);
            }
        }
    }

    ops
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let tempdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };

    // Use a small k so flush_to_immutable promotes blocks quickly.
    let mut db = match ChainDB::open_with_config(
        tempdir.path(),
        &dugite_storage::ImmutableConfig::default(),
        4, // k=4 so we flush frequently
    ) {
        Ok(db) => db,
        Err(_) => return,
    };

    let ops = parse_ops(data, 128);

    for op in ops {
        match op {
            Op::AddBlock {
                hash_idx,
                prev_idx,
                slot,
                block_no,
                cbor,
            } => {
                let hash = synthetic_hash(hash_idx);
                let prev_hash = synthetic_hash(prev_idx);
                let _ = db.add_block(hash, SlotNo(slot), BlockNo(block_no), prev_hash, cbor);
            }
            Op::RollbackTo { hash_idx, slot } => {
                let point = if slot == 0 {
                    Point::Origin
                } else {
                    let hash = synthetic_hash(hash_idx);
                    Point::Specific(SlotNo(slot), hash)
                };
                let _ = db.rollback_to_point(&point);
            }
            Op::FlushToImmutable => {
                let _ = db.flush_to_immutable();
            }
            Op::GetTip => {
                let _ = db.get_tip();
            }
        }
    }
});
