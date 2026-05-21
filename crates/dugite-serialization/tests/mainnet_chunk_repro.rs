//! M5 regression: load real mainnet chunk-file blocks and confirm they decode.
//!
//! This test is `#[ignore]`'d by default because it depends on a populated
//! `db-mainnet/immutable/` directory which is not present in CI. Run locally
//! with:
//!
//!     cargo nextest run -p dugite-serialization mainnet_chunk -- --ignored
//!
//! Soak on 2026-05-21 against `db-mainnet/immutable/08686.chunk` (Conway era,
//! snapshot epoch 632, slot 187766050) produced 683K decode failures with
//! the pattern "expected array, got map at position ~1010" plus 339 live-
//! block failures with "expected bytes, got string at variable position".
//! See `soak-logs/mainnet-20260521-203250.log`.

use dugite_serialization::decode_block;
use std::fs;
use std::path::Path;

fn db_mainnet_immutable() -> &'static Path {
    Path::new("/Users/michaelfazio/Source/dugite/db-mainnet/immutable")
}

/// Read a 56-byte secondary-index entry per dugite-node's mithril.rs spec.
fn read_secondary_block_offset(secondary: &[u8], i: usize) -> Option<u64> {
    let start = i * 56;
    if start + 56 > secondary.len() {
        return None;
    }
    Some(u64::from_be_bytes(
        secondary[start..start + 8].try_into().ok()?,
    ))
}

#[test]
#[ignore]
fn mainnet_chunk_08686_first_block_decodes() {
    let root = db_mainnet_immutable();
    if !root.exists() {
        eprintln!("SKIPPING: db-mainnet/immutable not present");
        return;
    }
    let chunk = fs::read(root.join("08685.chunk")).expect("read chunk");
    let secondary = fs::read(root.join("08685.secondary")).expect("read secondary");

    let offset0 = read_secondary_block_offset(&secondary, 0).expect("offset 0") as usize;
    let offset1 = read_secondary_block_offset(&secondary, 1)
        .map(|n| n as usize)
        .unwrap_or(chunk.len());

    let block_cbor = &chunk[offset0..offset1];
    eprintln!(
        "block 0: offset={offset0} size={} first 64 bytes: {}",
        block_cbor.len(),
        hex::encode(&block_cbor[..block_cbor.len().min(64)])
    );

    match decode_block(block_cbor) {
        Ok(block) => {
            eprintln!(
                "DECODE OK: era={:?} slot={} block={} hash={}",
                block.era,
                block.header.slot.0,
                block.header.block_number.0,
                block.header.header_hash
            );
        }
        Err(e) => {
            eprintln!("DECODE FAIL: {e}");
            // Dump bytes around position 1010 for inspection.
            let pos = 1010usize;
            let start = pos.saturating_sub(8);
            let end = (pos + 16).min(block_cbor.len());
            eprintln!(
                "  bytes [{start}..{end}]: {}",
                hex::encode(&block_cbor[start..end])
            );
            panic!("decode failed at run: {e}");
        }
    }
}

#[test]
#[ignore]
fn mainnet_chunk_08686_first_10_blocks_decode() {
    let root = db_mainnet_immutable();
    if !root.exists() {
        eprintln!("SKIPPING: db-mainnet/immutable not present");
        return;
    }
    let chunk = fs::read(root.join("08685.chunk")).expect("read chunk");
    let secondary = fs::read(root.join("08685.secondary")).expect("read secondary");

    let n = secondary.len() / 56;
    eprintln!("chunk has {n} blocks");
    let mut failures = 0;
    let mut samples_of_failures: Vec<(usize, String, Vec<u8>)> = Vec::new();
    for i in 0..n.min(50) {
        let off = read_secondary_block_offset(&secondary, i).unwrap() as usize;
        let next = read_secondary_block_offset(&secondary, i + 1)
            .map(|n| n as usize)
            .unwrap_or(chunk.len());
        let block_cbor = &chunk[off..next];
        match decode_block(block_cbor) {
            Ok(_) => {}
            Err(e) => {
                failures += 1;
                if samples_of_failures.len() < 3 {
                    samples_of_failures.push((i, e.to_string(), block_cbor.to_vec()));
                }
            }
        }
    }
    eprintln!("failures: {failures} / 50");
    for (i, e, bytes) in &samples_of_failures {
        eprintln!("\nfail[{i}]: {e}");
        eprintln!("  size: {}", bytes.len());
        eprintln!("  first 64: {}", hex::encode(&bytes[..64.min(bytes.len())]));
        // Extract the position number from the error message.
        if let Some(pos_str) = e
            .split("position ")
            .nth(1)
            .and_then(|s| s.split(':').next())
        {
            if let Ok(pos) = pos_str.parse::<usize>() {
                let start = pos.saturating_sub(8);
                let end = (pos + 16).min(bytes.len());
                eprintln!(
                    "  bytes [{start}..{end}] (error position={pos}): {}",
                    hex::encode(&bytes[start..end])
                );
            }
        }
    }
    assert_eq!(failures, 0, "expected zero decode failures");
}

/// Decode every block in chunk 08685 (Conway era, 4298 blocks).
///
/// This is the exhaustive variant of `mainnet_chunk_08686_first_10_blocks_decode`.
/// It covers the full diversity of Conway-era encodings present in the chunk
/// (tag-258 sets, indefinite-length arrays, text-URL anchors, etc.) and is the
/// canonical regression gate for M5 mainnet decoder correctness.
///
/// Run with:
///
///     cargo nextest run -p dugite-serialization mainnet_chunk_08685_all -- --ignored
#[test]
#[ignore]
fn mainnet_chunk_08685_all_blocks_decode() {
    let root = db_mainnet_immutable();
    if !root.exists() {
        eprintln!("SKIPPING: db-mainnet/immutable not present");
        return;
    }
    let chunk = fs::read(root.join("08685.chunk")).expect("read chunk");
    let secondary = fs::read(root.join("08685.secondary")).expect("read secondary");

    let n = secondary.len() / 56;
    eprintln!("chunk has {n} blocks");
    let mut failures = 0;
    let mut samples_of_failures: Vec<(usize, String, Vec<u8>)> = Vec::new();
    for i in 0..n {
        let off = read_secondary_block_offset(&secondary, i).unwrap() as usize;
        let next = read_secondary_block_offset(&secondary, i + 1)
            .map(|n| n as usize)
            .unwrap_or(chunk.len());
        let block_cbor = &chunk[off..next];
        match decode_block(block_cbor) {
            Ok(_) => {}
            Err(e) => {
                failures += 1;
                if samples_of_failures.len() < 3 {
                    samples_of_failures.push((i, e.to_string(), block_cbor.to_vec()));
                }
            }
        }
    }
    eprintln!("failures: {failures} / {n}");
    for (i, e, bytes) in &samples_of_failures {
        eprintln!("\nfail[{i}]: {e}");
        eprintln!("  size: {}", bytes.len());
        eprintln!("  first 64: {}", hex::encode(&bytes[..64.min(bytes.len())]));
        if let Some(pos_str) = e
            .split("position ")
            .nth(1)
            .and_then(|s| s.split(':').next())
        {
            if let Ok(pos) = pos_str.parse::<usize>() {
                let start = pos.saturating_sub(8);
                let end = (pos + 16).min(bytes.len());
                eprintln!(
                    "  bytes [{start}..{end}] (error position={pos}): {}",
                    hex::encode(&bytes[start..end])
                );
            }
        }
    }
    assert_eq!(
        failures, 0,
        "expected zero decode failures for all {n} blocks"
    );
}
