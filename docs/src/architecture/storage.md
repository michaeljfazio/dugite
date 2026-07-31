# Storage

Dugite's storage layer is implemented in the `dugite-storage` and `dugite-ledger` crates. It closely mirrors the cardano-node architecture with three distinct storage subsystems coordinated by ChainDB.

## Storage Architecture

```mermaid
flowchart TD
    CDB[ChainDB] --> VOL[VolatileDB<br/>In-Memory HashMap<br/>Last k=2160 blocks]
    CDB --> IMM[ImmutableDB<br/>Append-Only Chunk Files<br/>Finalized blocks]

    NEW[New Block] -->|add_block| VOL
    VOL -->|flush when > k blocks| IMM

    READ[Block Query] -->|1. check volatile| VOL
    READ -->|2. fallback to immutable| IMM

    ROLL[Rollback] -->|remove from volatile| VOL

    LS[LedgerState] --> UTXO[UtxoStore<br/>dugite-lsm LSM tree<br/>On-disk UTxO set]
    LS --> DIFF[DiffSeq<br/>Last k UTxO diffs<br/>For rollback]
```

## Block Storage

### ImmutableDB (Append-Only Chunk Files)

The ImmutableDB stores finalized blocks in append-only chunk files on disk. This matches cardano-node's ImmutableDB design — blocks are simply appended to files and are inherently durable without any snapshot mechanism.

Properties:
- **Always durable** — append-only writes survive process crashes without special persistence logic
- **No LSM tree** — plain chunk files, no compaction or memtable overhead
- **Sequential access** — optimized for the append-heavy, read-sequential block storage workload
- **Secondary indexes** — slot-to-offset and hash-to-slot mappings for efficient lookups
- **Memory-mapped block index** — on-disk open-addressing hash table (`hash_index.dat`) provides 3-5x faster lookups than in-memory HashMap while using near-zero RSS

### Crash Recovery & Integrity

A 2026-07-28 preprod incident (a hard stop that lost the active chunk's secondary index and
wedged sync for every peer) drove a durability hardening pass across the ImmutableDB, shipped in
v2.4.0 (#926-#929):

- **Per-append secondary-index writes** — each block's secondary-index entry is written to disk
  as it is appended (`active.secondary_file.write_all(...)` per block), not buffered in memory
  until a clean shutdown. Previously a hard stop lost every index entry written since the node
  started, even though the block data itself was already durable.
- **Open-time reconciliation in both open paths** — `ImmutableDB::open()` (read-only) and
  `open_for_writing()` both run `reconcile_chunks_on_disk()` before any other file is touched.
  Older versions only validated on the read-only path, so the node's own write-mode startup never
  checked for damage.
- **Tail-chunk CRC verification and truncation** — every block in the highest-numbered
  ("tail") chunk is CRC32-verified; a chunk's true recoverable end is recovered by scanning for
  the last CRC-matching `0x82`-envelope boundary, and the file is truncated to that verified
  prefix. Damage strictly below the tail is refused with a hard `InconsistentChunk` error rather
  than silently repaired.
- **`.chunk.orphaned` quarantine** — a non-empty tail chunk with no matching secondary index is
  renamed to `<num>.chunk.orphaned` and excluded from the chain, preserved on disk for manual
  inspection instead of being silently skipped or overwritten.
- **Cross-chunk boundary linkage** — adjacent chunks are checked so the first block of a chunk
  correctly chains onto the previous chunk's tip (`prev_hash`), the dugite equivalent of
  Haskell's `ChunkFileDoesntFit`. Per-chunk CRC checks alone are not sufficient to catch this — an
  internally-valid orphan island can still pass every per-chunk check while being disconnected
  from the canonical chain.
- **`tip.meta` clamping** — the cached tip metadata is only trusted when it matches the last
  indexed entry's `(slot, hash)`; otherwise it is clamped to the true indexed tip (recovering the
  correct block number by decoding the tip block) and rewritten.
- **`immutable/clean` marker** — a zero-byte `clean` marker file is written by the shutdown flush
  and removed the moment the DB re-enters write mode. Its presence gates whether the
  memory-mapped `hash_index.dat` can be reused as-is; its absence (an unclean stop) forces a
  rebuild, since mmap pages may have reached disk in an order that leaves stale offsets behind.
- **Exclusive directory lock** — `ChainDB::open` takes an advisory `flock(2)` on `<database-path>/lock`
  before touching any other file (the dugite equivalent of Haskell's `withLockDB`). A second
  process opening the same `--database-path` fails fast, naming the holder's pid, instead of both
  processes silently interleaving writes into the same chunk files.

**Operational consequence:** always stop dugite-node with `SIGTERM`, never `SIGKILL`. A graceful
stop runs the shutdown flush (writes the `clean` marker, fsyncs the tail chunk's index); a killed
process leaves `clean` absent and relies entirely on the open-time reconciliation above to recover
— safe, but strictly more expensive and unnecessary to trigger routinely.

### VolatileDB (In-Memory HashMap)

The VolatileDB stores recent blocks (the last k=2160 blocks) in an in-memory `HashMap`. This enables:

- **Fast reads** — no disk I/O for recent blocks
- **Efficient rollback** — blocks can be removed without touching disk
- **Simple eviction** — when a block becomes k-deep, it is flushed to the ImmutableDB

The VolatileDB has no on-disk representation — it exists only in memory and is rebuilt from the ImmutableDB tip on restart.

### ChainDB

ChainDB is the unified interface for block storage. It coordinates the ImmutableDB and VolatileDB:

1. New blocks arrive from peers and are added to the **VolatileDB**
2. Once a block is more than **k** slots deep (k=2160 for mainnet), it is flushed from the VolatileDB to the **ImmutableDB**
3. Flushed blocks are removed from the VolatileDB

The ChainDB write for a new block always happens **before** that block is applied to the ledger
state — never the reverse. If the node crashes between the two steps, the block is durably stored
but not yet reflected in the ledger, so recovery simply re-applies it; the opposite ordering could
leave the ledger ahead of durable storage with no way to recover the block that produced that
state.

When querying for a block:
1. The VolatileDB is checked first (fast, in-memory)
2. If not found, the ImmutableDB is consulted (disk-based)

### Block Range Queries

ChainDB supports querying blocks by slot range:
- VolatileDB scans its HashMap for matching slots
- ImmutableDB uses secondary indexes for slot range scanning
- Results from both databases are merged

## UTxO Storage (UTxO-HD)

The UTxO set is stored on disk using `dugite-lsm`, a pure Rust LSM tree. This matches Haskell cardano-node's UTxO-HD architecture, where the UTxO set lives in an LSM-backed on-disk store rather than entirely in memory.

### UtxoStore

The `UtxoStore` (in `dugite-ledger`) wraps a dugite-lsm `LsmTree` and provides:

- **Disk-backed UTxO set** — the full UTxO set lives on disk, not in memory
- **Efficient point lookups** — bloom filters for fast negative lookups
- **Batch writes** — UTxO inserts and deletes are batched per block
- **Snapshots** — periodic snapshots for crash recovery

dugite-lsm is configured via storage profiles that maximize available system memory:

| Profile | Target System | Memtable | Block Cache | Expected RSS |
|---------|--------------|----------|-------------|-------------|
| `ultra-memory` | 32GB | 2GB | 24GB | ~27GB |
| `high-memory` (default) | 16GB | 1GB | 12GB | ~14GB |
| `low-memory` | 8GB | 512MB | 5GB | ~6.5GB |
| `minimal` | 4GB | 256MB | 2GB | ~3GB |

All profiles use 10 bits per key bloom filters and hybrid compaction (tiered L0, leveled L1+).

### DiffSeq (Rollback Support)

The `DiffSeq` (in `dugite-ledger`) maintains the last k blocks of UTxO diffs, enabling rollback without replaying blocks:

- Each block produces a `UtxoDiff` recording which UTxOs were added and removed
- The `DiffSeq` holds the last k=2160 diffs
- On rollback, diffs are applied in reverse to restore the UTxO set

### io_uring Support (Linux)

On Linux with kernel 5.1+, enable io_uring for async I/O in the UTxO LSM tree:

```bash
cargo build --release --features io-uring
```

On other platforms (macOS, Windows), the feature flag is accepted but falls back to synchronous I/O automatically.

## Snapshot Policy

Dugite uses a time-based snapshot policy matching Haskell's cardano-node:

- **Normal sync**: snapshots every 72 minutes (k * 2 seconds, where k=2160)
- **Bulk sync**: snapshots every 50,000 blocks plus 6 minutes of wall-clock time
- **Maximum retained**: 2 snapshots on disk at any time

Ledger snapshots include the full ledger state (stake distribution, protocol parameters, governance state, etc.). The UTxO set is persisted separately via the UtxoStore's LSM snapshots.

## Tip Recovery

When the node restarts:
1. The ImmutableDB tip is read from the chunk files (always durable)
2. The VolatileDB starts empty (in-memory state is rebuilt)
3. The ledger state is restored from the most recent snapshot
4. The UTxO set is restored from the UtxoStore's LSM snapshot
5. The node resumes syncing from the recovered tip

## Disk Layout

```
database-path/
  lock                # Advisory flock held for the ChainDB's lifetime (#929)
  immutable/          # ImmutableDB — chunk and index files live flat, not nested
    00000.chunk       # Block data, one file per chunk
    00000.secondary   # Per-chunk secondary index (slot/hash -> offset), written per block append
    00001.chunk
    00001.secondary
    ...
    tip.meta          # Cached (slot, hash, block_no) tip — clamped to the indexed chain if stale
    clean             # Zero-byte marker written on graceful shutdown; absent after a hard stop
    hash_index.dat    # Mmap block index (open-addressing hash table)
  utxo-store/         # dugite-lsm database (UTxO set)
    active/           # Current SSTables
    snapshots/        # Durable snapshots
  ledger/             # Ledger state snapshots
```

## Performance Considerations

- **Block writes** — append-only chunk files provide consistent write performance without compaction pauses
- **UTxO lookups** — LSM tree with bloom filters provides efficient point lookups for transaction validation
- **Memory usage** — the VolatileDB holds approximately k blocks in memory (typically a few hundred MB). The UTxO set lives on disk, significantly reducing memory pressure compared to an all-in-memory approach
- **Batch size** — the flush batch size balances memory usage against write efficiency

## Storage Profiles

Dugite provides four storage profiles sized to maximize available system memory:

```bash
# Select a profile via CLI
./dugite-node run --storage-profile high-memory ...

# Override individual parameters
./dugite-node run --storage-profile low-memory --utxo-block-cache-size-mb 4096 ...
```

Profiles can also be set in the node configuration file:

```json
{
  "storage": {
    "profile": "high-memory",
    "utxoBlockCacheSizeMb": 8192
  }
}
```

Resolution order: profile defaults < config file overrides < CLI overrides.

## Fork Recovery & ImmutableDB Contamination

### Problem

When a forged block loses a slot battle, `flush_all_to_immutable` on graceful shutdown can persist orphaned blocks permanently in the ImmutableDB. Since the ImmutableDB is append-only and designed for finalized blocks, these orphaned blocks contaminate the canonical chain history and can cause intersection failures on reconnect.

```mermaid
sequenceDiagram
    participant Node as Dugite Node
    participant Vol as VolatileDB
    participant Imm as ImmutableDB
    participant Peer as Upstream Peer

    Node->>Vol: Forge block at slot S
    Peer->>Node: Competing block at slot S wins
    Note over Vol: Orphaned forged block still in VolatileDB
    Node->>Imm: flush_all_to_immutable (graceful shutdown)
    Note over Imm: Orphaned block now persisted permanently
    Node->>Peer: Restart — intersection negotiation fails
```

### Detection

- **`ChainDB.get_chain_points()`** walks backwards through volatile blocks via `prev_hash` links, providing the peer with enough ancestry for intersection even when the tip is orphaned.
- **`ImmutableDB.get_historical_points()`** samples older chunk secondary indexes in reverse order, providing canonical intersection points even when the immutable tip is contaminated.
- When fork divergence is detected, contaminated ChainDB chain points are excluded from intersection negotiation, preventing the node from advertising orphaned blocks to peers.

### Recovery

- **Case A (Origin intersection):** The volatile DB is cleared, the ledger state is reset, and the node reconnects from genesis. This is the fallback when no valid intersection can be found.
- **Case B (Intersection behind ledger):** A targeted ImmutableDB replay is performed up to the intersection slot using a detached LSM store, achieving approximately 50K blocks/second replay speed. This avoids a full resync while restoring the ledger to a consistent state.

## Benchmarks

Run storage benchmarks with:

```bash
# Storage benchmarks (block index, ImmutableDB, ChainDB, scaling to 1M entries)
cargo bench -p dugite-storage --bench storage_bench

# UTxO store benchmarks (insert, lookup, apply_tx, LSM configs, scaling to 1M entries)
cargo bench -p dugite-ledger --bench ledger_bench

# Crypto benchmarks (Ed25519, blake2b keyhash)
cargo bench -p dugite-crypto --bench crypto_bench

# Hash benchmarks (blake2b_256, blake2b_224, batch hashing)
cargo bench -p dugite-primitives --bench primitives_bench
```

Results are saved to `target/criterion/` with HTML reports. Baseline results are tracked in `benches/`.

### Latest Results (Apple M2 Max, 32GB, 2026-03-14)

#### Block Index Lookup (500 random lookups, mmap vs in-memory HashMap)

| Size | In-Memory | Mmap | Speedup |
|------|-----------|------|---------|
| 10K | 10.0µs | 2.83µs | **3.5x** |
| 100K | 10.1µs | 2.17µs | **4.7x** |
| 1M | 10.6µs | 2.01µs | **5.3x** |

Mmap lookup advantage grows with scale — at mainnet block counts (~10M), the gap widens further.

#### UTxO Store Scaling (dugite-lsm LSM tree)

| Size | Insert (per-entry) | Lookup (per-entry) | Total Lovelace Scan |
|------|-------------------|-------------------|-------------------|
| 10K | 455ns | 191ns | 2.38ms |
| 100K | 479ns | 236ns | 29.1ms |
| 1M | 569ns | 308ns | 330ms |

Insert and lookup scale near-linearly. At mainnet scale (~20M UTxOs), estimated full scan ~6.6s.

#### Crypto & Hashing

| Operation | Time |
|-----------|------|
| Ed25519 verify (single) | 28.6µs |
| Blake2b-224 keyhash (32B) | 128ns |
| Blake2b-256 tx hash (1KB) | 949ns |

A typical block with 50 witnesses: ~1.4ms for signature verification, ~6.4µs for keyhash computation.

#### LSM Config Comparison (100K entries)

All storage profiles perform identically at benchmark scale — config differences emerge at mainnet scale (20M+ UTxOs) where working set exceeds cache capacity.

See `benches/2026-03-14-all-profiles.md` for full results.
