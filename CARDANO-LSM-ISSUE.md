# cardano-lsm O(n) SSTable Lookup Bug

## Summary

The `cardano-lsm` crate (v1.0.1) has a critical performance bug where every
point lookup (`get()`) reads the **entire** keyops SSTable file from disk instead
of using the index to seek to the correct offset. This makes the LSM-backed UTxO
store unusable at mainnet scale (~15-20M UTxOs).

## Impact

At 2.4M UTxOs on preview testnet, the keyops file reaches ~674MB. Each UTxO
lookup reads this entire file. A typical block with ~50 transactions and ~4 UTxO
operations per transaction triggers ~200 full file reads per block — approximately
**135 GB of I/O per block**. This reduces throughput from 35,000 blk/s (in-memory
backend) to ~0.05 blk/s (1 block every 10-45 seconds).

On mainnet with ~15M UTxOs, the keyops file would be ~4 GB, making each block
require ~800 GB of I/O — completely impractical.

## Root Cause

File: `~/.cargo/registry/src/.../cardano-lsm-1.0.1/src/sstable_new.rs`

```rust
// Line 370-372
// For now, do a full scan - in a real implementation we'd use the index
// to jump to the right offset in the keyops file
let range_results = self.range_with_tombstones_backend(key, key, backend)?;
```

The `get_backend()` method delegates to `range_with_tombstones_backend()` which
calls `io_backend::read_file()` to read the entire keyops file into memory for
every single lookup. The library's own comment acknowledges this as placeholder
code.

The SSTable already has an index (`sstable_new.rs` stores sorted key offsets),
but the `get()` path bypasses it entirely.

## Workaround

Use the in-memory UTxO backend instead of LSM:

```bash
torsten-node run \
  --storage-profile minimal \
  --utxo-backend in-memory \
  ...
```

This stores all UTxOs in a `HashMap` in RAM. It works well but requires
sufficient memory (~8 GB RSS at 3M UTxOs on preview, estimated ~40-60 GB
for mainnet's 15M UTxOs).

## Fix Options

### Option A: Fix upstream (preferred)

Implement binary search on the SSTable index in `get_backend()`:

1. Read the index to find the approximate offset for the target key
2. Seek to that offset in the keyops file
3. Scan forward from the offset until the key is found or passed

This would reduce each lookup from O(file_size) to O(log(n) + block_size),
making it practical for mainnet.

### Option B: Fork and fix

Fork `cardano-lsm` into the torsten workspace and implement the index-based
lookup ourselves. The index structure is already built during SSTable
creation — only the read path needs fixing.

### Option C: Replace with established LSM

Replace `cardano-lsm` with a mature LSM implementation like `rocksdb` (via
the `rust-rocksdb` crate) or `sled`. These have production-grade point lookup
performance. Downside: adds a C++ dependency (RocksDB) or changes the storage
format (sled).

## Discovered

Found during soak test on preview testnet (2026-03-14). The node replayed
4.1M blocks at 35K blk/s with the in-memory backend, but stalled at ~0.05
blk/s with the LSM backend once the UTxO set exceeded ~2M entries.

## References

- Crate: https://crates.io/crates/cardano-lsm
- Version used: 1.0.1
- Affected file: `src/sstable_new.rs`, function `get_backend()`
