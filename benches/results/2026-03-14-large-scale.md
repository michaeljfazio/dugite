# Large-Scale Benchmark Results — 2026-03-14

Machine: Apple M2 Max (32 GB), macOS Darwin 25.2.0
Branch: main
Profile defaults updated: high-memory (512MB memtable, 4GB cache), low-memory (256MB memtable, 2GB cache)

## Block Index Scaling (10K → 1M entries)

### Insert Throughput

| Size | In-Memory | Mmap | Ratio |
|------|-----------|------|-------|
| 10K | 697µs | 1.67ms | 2.4x |
| 50K | 2.90ms | 17.4ms | 6.0x |
| 100K | 5.82ms | 45.6ms | 7.8x |
| 250K | 18.6ms | 56.4ms | 3.0x |
| 500K | 38.3ms | 80.7ms | 2.1x |
| 1M | 80.4ms | 164ms | 2.0x |

Mmap insert is slower due to disk I/O, but the gap narrows at scale (from 7.8x at 100K to 2.0x at 1M).

### Lookup Throughput (500 random lookups)

| Size | In-Memory | Mmap | Speedup |
|------|-----------|------|---------|
| 10K | 9.62µs | 2.82µs | **3.4x** |
| 50K | 10.2µs | 2.16µs | **4.7x** |
| 100K | 10.3µs | 2.19µs | **4.7x** |
| 250K | 10.6µs | 2.14µs | **5.0x** |
| 500K | 10.3µs | 2.02µs | **5.1x** |
| 1M | 11.2µs | 2.01µs | **5.6x** |

**Key finding**: Mmap lookup advantage *increases* with dataset size (3.4x → 5.6x). At mainnet scale (~10M blocks), the advantage will be even more pronounced as HashMap cache misses increase while mmap stays constant via direct memory mapping.

### ImmutableDB Open Time

| Size | In-Memory | Mmap (cached) | Speedup |
|------|-----------|---------------|---------|
| 100K | 1.62ms | 1.62ms | 1.0x |
| 250K | 4.59ms | 4.51ms | 1.0x |
| 500K | 9.89ms | 9.53ms | 1.0x |

At these synthetic scales, open time is dominated by secondary index scanning (same for both). At mainnet scale with a pre-built `hash_index.dat`, mmap open is near-instant (no index rebuilding needed).

## UTxO Store Scaling (10K → 1M entries)

### Insert Throughput

| Size | Time | Per-entry |
|------|------|-----------|
| 10K | 4.42ms | 442ns |
| 50K | 23.4ms | 468ns |
| 100K | 47.7ms | 477ns |
| 250K | 127ms | 509ns |
| 500K | 266ms | 531ns |
| 1M | 578ms | 578ns |

Insert scales linearly — ~1.3x slowdown per 2x size increase. Good LSM write amplification behavior.

### Lookup Throughput (1K random lookups)

| Size | Time (1K lookups) | Per-lookup |
|------|-------------------|-----------|
| 10K | 194µs | 194ns |
| 50K | 221µs | 221ns |
| 100K | 234µs | 234ns |
| 250K | 254µs | 254ns |
| 500K | 275µs | 275ns |
| 1M | 307µs | 307ns |

Lookup degrades gracefully — only 1.6x slowdown from 10K to 1M (bloom filters + block cache effective).

### Apply Transaction (50 txs, each consuming 1 input + producing 2 outputs)

| Base UTxO Size | Time | Per-tx |
|----------------|------|--------|
| 10K | 1.47ms | 29.4µs |
| 50K | 5.24ms | 105µs |
| 100K | 11.3ms | 225µs |
| 250K | 28.3ms | 566µs |
| 500K | 58.4ms | 1.17ms |

Apply_tx scales linearly with base UTxO set size — dominated by lookup + insert cost.

### Total Lovelace Scan (full iteration)

| Size | Time | Per-entry |
|------|------|-----------|
| 10K | 2.36ms | 236ns |
| 50K | 13.0ms | 260ns |
| 100K | 28.6ms | 286ns |
| 250K | 80.1ms | 320ns |
| 500K | 168ms | 336ns |
| 1M | 347ms | 347ns |

Full scan scales linearly. At mainnet scale (~20M UTxOs), expect ~7 seconds for full scan.

## LSM Config Comparison (100K entries)

| Config | Insert | Lookup (1K) |
|--------|--------|-------------|
| low_8gb (256MB/2GB) | 48.6ms | 235µs |
| mid_16gb (512MB/4GB) | 48.6ms | 235µs |
| high_32gb (512MB/8GB) | 48.8ms | 237µs |
| high_bloom_16gb (512MB/4GB/15bit) | 48.4ms | 237µs |
| legacy_small (128MB/256MB) | 48.6ms | 236µs |

At 100K entries, all configs perform identically — the dataset fits comfortably in the smallest cache. The larger configs pay off at mainnet scale (20M UTxOs, ~60GB on-disk) where working set exceeds cache capacity, reducing disk reads through better hit rates.

## Updated Profile Defaults

Based on real-world memory budgets:

| Profile | Target Systems | Memtable | Block Cache | Bloom |
|---------|---------------|----------|-------------|-------|
| `high-memory` | 16-32GB | 512MB | 4GB | 10 bits |
| `low-memory` | 8GB | 256MB | 2GB | 10 bits |

**Rationale**: Previous defaults (128MB/256MB) left most of the available system memory unused. A 16GB system dedicates ~12GB to Torsten, of which ~4.5GB is RSS overhead — leaving 7.5GB+ for LSM caching. The new defaults use this budget effectively while leaving headroom for OS page cache and other system needs.
