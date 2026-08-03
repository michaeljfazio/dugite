//! Fuzz target for ImmutableDB secondary index parsing.
//!
//! Writes arbitrary bytes into a tempdir as a `.secondary` index file (the
//! format that drives block lookups), then calls `ImmutableDB::open` which
//! reads and parses every secondary index on startup. Also writes a minimal
//! companion `.chunk` file so the open path does not short-circuit on a
//! missing chunk. Must not panic regardless of the byte content.
//!
//! The secondary index format is 56 bytes per entry:
//!   block_offset(8) + header_offset(2) + header_size(2) + crc32(4)
//!   + header_hash(32) + slot(8) = 56 bytes total
//!
//! Run with: cargo +nightly fuzz run fuzz_immutable_chunk_parse -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_storage::ImmutableDB;

fuzz_target!(|data: &[u8]| {
    // Require at least 1 byte so we can drive the chunk number from it.
    if data.is_empty() {
        return;
    }

    let tempdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };

    // Use the first byte to pick a chunk number in [0, 3] so the secondary
    // file name is deterministic and short.
    let chunk_num = (data[0] as u64) % 4;
    let secondary_bytes = &data[1..];

    // Write the fuzz bytes as the secondary index file.
    let secondary_path = tempdir.path().join(format!("{chunk_num:05}.secondary"));
    if std::fs::write(&secondary_path, secondary_bytes).is_err() {
        return;
    }

    // Write a minimal (possibly empty) companion chunk file. ImmutableDB
    // requires both files to exist before it processes a chunk.
    let chunk_path = tempdir.path().join(format!("{chunk_num:05}.chunk"));
    if std::fs::write(&chunk_path, b"").is_err() {
        return;
    }

    // `open` reads all secondary indexes and builds the in-memory hash index.
    // It must not panic on arbitrary bytes — it should return an error or an
    // empty/partial DB.
    let _ = ImmutableDB::open(tempdir.path());
});
