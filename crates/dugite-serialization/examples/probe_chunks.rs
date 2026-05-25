// Probe of mithril chunk first-slot extraction. Mirrors the fast-path logic
// in dugite_node::mithril::read_chunk_first_block_slot so we can verify the
// secondary-index slot extraction works for all eras without depending on the
// node crate.
use std::fs;
use std::path::Path;

fn read_u64_be(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

fn main() {
    let dir = Path::new("/Users/michaelfazio/Source/dugite/db-mainnet/immutable");
    let mut chunk_nums: Vec<u64> = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        let s = name.to_string_lossy();
        if let Some(num_str) = s.strip_suffix(".chunk") {
            if let Ok(n) = num_str.parse() {
                chunk_nums.push(n);
            }
        }
    }
    chunk_nums.sort();
    let mut ok = 0usize;
    let mut none = 0usize;
    let mut first_fail: Option<(u64, String)> = None;
    let mut first_ok_after_fail: Option<u64> = None;
    let mut last_ok_slot = 0u64;
    let mut failures: Vec<u64> = Vec::new();
    let mut secondary_slot_at_start: Option<u64> = None;
    let mut secondary_slot_at_end: Option<u64> = None;
    let mut secondary_monotonic_violations = 0usize;
    let mut prev_sec_slot: Option<u64> = None;
    for &n in &chunk_nums {
        let sec_path = dir.join(format!("{n:05}.secondary"));
        if let Ok(sec) = fs::read(&sec_path) {
            if sec.len() >= 56 {
                let v = u64::from_be_bytes(sec[48..56].try_into().unwrap());
                if secondary_slot_at_start.is_none() {
                    secondary_slot_at_start = Some(v);
                }
                secondary_slot_at_end = Some(v);
                if let Some(prev) = prev_sec_slot {
                    if v < prev {
                        secondary_monotonic_violations += 1;
                    }
                }
                prev_sec_slot = Some(v);
            }
        }
        let chunk_path = dir.join(format!("{n:05}.chunk"));
        let chunk = match fs::read(&chunk_path) {
            Ok(b) => b,
            Err(_) => {
                none += 1;
                continue;
            }
        };
        let sec = match fs::read(&sec_path) {
            Ok(b) => b,
            Err(_) => {
                none += 1;
                continue;
            }
        };
        if sec.len() < 56 || chunk.is_empty() {
            none += 1;
            continue;
        }
        let off0 = read_u64_be(&sec[..8]) as usize;
        let off1 = if sec.len() >= 112 {
            read_u64_be(&sec[56..64]) as usize
        } else {
            chunk.len()
        };
        if off0 >= chunk.len() || off1 > chunk.len() || off0 >= off1 {
            none += 1;
            continue;
        }
        match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
            &chunk[off0..off1],
            21600,
        ) {
            Ok(b) => {
                ok += 1;
                last_ok_slot = b.slot().0;
                if !failures.is_empty() && first_ok_after_fail.is_none() {
                    first_ok_after_fail = Some(n);
                }
            }
            Err(e) => {
                none += 1;
                failures.push(n);
                if first_fail.is_none() {
                    first_fail = Some((n, e.to_string()));
                }
            }
        }
    }
    println!(
        "secondary block_or_ebb: start={:?}, end={:?}, monotonic_violations={}",
        secondary_slot_at_start, secondary_slot_at_end, secondary_monotonic_violations,
    );
    println!("total chunks: {}", chunk_nums.len());
    println!("decoded OK: {}, None/err: {}", ok, none);
    println!("last OK first-slot: {}", last_ok_slot);
    if let Some((n, e)) = first_fail {
        println!("first failure: chunk {:05} err: {}", n, e);
    }
    if !failures.is_empty() {
        println!(
            "failures: {} chunks. first 10: {:?}, last 10: {:?}",
            failures.len(),
            &failures[..failures.len().min(10)],
            &failures[failures.len().saturating_sub(10)..]
        );
    }
    if let Some(n) = first_ok_after_fail {
        println!("first OK after fail: chunk {:05}", n);
    }
}
