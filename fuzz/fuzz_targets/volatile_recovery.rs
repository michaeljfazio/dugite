//! Fuzz target for VolatileDB WAL recovery.
//!
//! Writes arbitrary bytes into a tempdir as `volatile-wal.bin` and calls
//! `VolatileDB::open`, which replays the WAL on startup. This exercises the
//! multi-version WAL parser (legacy 56-byte, v2 88-byte, v3 88-byte + CRC32)
//! and the format-detection heuristics. Must not panic regardless of the byte
//! content — the implementation is expected to stop replay at the first
//! corrupted entry and return a partial (or empty) VolatileDB.
//!
//! WAL v3 entry layout (88-byte header + CBOR + 4-byte CRC32 trailer):
//!   magic(4) + slot(8) + block_no(8) + hash(32) + prev_hash(32)
//!   + cbor_len(4) + <cbor> + crc32(4)
//!
//! Run with: cargo +nightly fuzz run fuzz_volatile_recovery -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_storage::volatile_db::VolatileDB;

fuzz_target!(|data: &[u8]| {
    let tempdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };

    // Write fuzz bytes as the WAL file.
    let wal_path = tempdir.path().join("volatile-wal.bin");
    if std::fs::write(&wal_path, data).is_err() {
        return;
    }

    // Open the VolatileDB — this replays the WAL we just wrote.
    // Must not panic. Errors (corrupted WAL, truncated entries, bad magic)
    // are acceptable and expected outcomes.
    let _ = VolatileDB::open(tempdir.path());
});
