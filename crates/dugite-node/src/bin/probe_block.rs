//! Diagnostic: decode N consecutive blocks from a real chunk file.
//! Used during #673 investigation to find why post-Shelley replay fails.
//! NOT shipped to production.
//!
//! Usage: cargo run --bin probe_block --release -- <chunk> <secondary> [start_entry] [count]

use std::fs;

fn main() {
    let chunk_path = std::env::args().nth(1).expect("chunk path");
    let sec_path = std::env::args().nth(2).expect("secondary path");
    let start_entry: usize = std::env::args()
        .nth(3)
        .map(|s| s.parse().unwrap())
        .unwrap_or(0);
    let count: usize = std::env::args()
        .nth(4)
        .map(|s| s.parse().unwrap())
        .unwrap_or(5);

    let chunk = fs::read(&chunk_path).expect("read chunk");
    let sec = fs::read(&sec_path).expect("read secondary");
    let n_entries = sec.len() / 56;

    println!(
        "chunk={} ({} bytes), secondary={} ({} entries)",
        chunk_path,
        chunk.len(),
        sec_path,
        n_entries
    );

    let mut offsets: Vec<usize> = Vec::with_capacity(n_entries);
    let mut slots: Vec<u64> = Vec::with_capacity(n_entries);
    for i in 0..n_entries {
        let off = 56 * i;
        let block_offset = u64::from_be_bytes(sec[off..off + 8].try_into().unwrap()) as usize;
        let slot = u64::from_be_bytes(sec[off + 48..off + 56].try_into().unwrap());
        offsets.push(block_offset);
        slots.push(slot);
    }

    let end = (start_entry + count).min(n_entries);
    for i in start_entry..end {
        let start = offsets[i];
        let stop = if i + 1 < n_entries {
            offsets[i + 1]
        } else {
            chunk.len()
        };
        let block = &chunk[start..stop];
        let first_hex = block
            .iter()
            .take(24)
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");

        let result_min =
            dugite_serialization::decode_block_minimal_with_byron_epoch_length(block, 21600);
        let result_full = dugite_serialization::decode_block_with_byron_epoch_length(block, 21600);
        let label_min = match &result_min {
            Ok(_) => "min=OK".to_string(),
            Err(e) => format!("min=ERR({})", e),
        };
        let label_full = match &result_full {
            Ok(_) => "full=OK".to_string(),
            Err(e) => format!("full=ERR({})", e),
        };
        println!(
            "entry {:3} slot={} offset={} size={} first24={} → {} | {}",
            i,
            slots[i],
            start,
            stop - start,
            first_hex,
            label_min,
            label_full
        );
    }
}
