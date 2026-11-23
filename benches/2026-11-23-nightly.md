# Nightly Benchmark Results — 2026-11-23

Captured on **2026-11-23** from commit [`68ea400`](https://github.com/MichaelJFazio/dugite/commit/68ea400) on `main` (GitHub Actions `ubuntu-latest`).

### Measurement environment

| | |
|---|---|
| Runner | GitHub Actions `ubuntu-latest` (shared, virtualised) |
| CPU | AMD EPYC 9V45 96-Core Processor |
| vCPUs | 4 |
| Memory | 15.6 GiB |
| Kernel | Linux 7.0.0-1020-azure |
| Toolchain | rustc 1.97.0 (2d8144b78 2026-07-07) |
| Build profile | `bench` (release + debug assertions off) |

> **Read absolute numbers with care.** These run on shared, virtualised GitHub-hosted runners whose CPU model is not pinned and whose neighbours are not controlled. Treat the figures as an order-of-magnitude regression tripwire, not as hardware benchmarks: a `change: ±x%` between two dates can reflect a different host class rather than a code change. Use the interactive trend lines below, where a real regression shows as a sustained step rather than a single-day spike.

> **Not measured here:** end-to-end sync throughput, mainnet-scale UTxO memory, and anything requiring a live network. Those are covered by the devnet-validate and soak rigs, not by Criterion.

> **Interactive reports**, including per-benchmark detail pages and historical trend lines, are published at <https://michaeljfazio.github.io/dugite/benchmarks/>. Each section below also links directly to its interactive report.

> The collapsed _Raw measurements_ blocks contain the filtered `cargo bench` output (cargo build chatter and ANSI escapes are stripped). Full unfiltered logs are uploaded as the `benchmark-results-263` workflow artifact.

---

## Storage

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking chaindb/sequential_insert/10k_20kb
Benchmarking chaindb/sequential_insert/10k_20kb: Warming up for 3.0000 s
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 6.7266 s (30 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [215.69 ms 217.28 ms 219.08 ms]

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 5.4128 s (20k iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [265.56 µs 266.02 µs 266.55 µs]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 5.5128 s (400 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [13.410 ms 13.638 ms 13.874 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 5.0000 s (18B iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [273.28 ps 274.39 ps 275.82 ps]
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 5.0767 s (308k iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [16.287 µs 16.394 µs 16.530 µs]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 5.0009 s (25M iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [202.53 ns 203.08 ns 203.72 ns]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 5.3116 s (110 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [6.9897 ms 8.1730 ms 10.644 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 6.5798 s (30 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [212.04 ms 214.13 ms 217.52 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 6.3796 s (30 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [214.75 ms 215.16 ms 215.69 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 6.3874 s (30 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [29.906 ms 31.200 ms 32.959 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 6.5171 s (30 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [30.366 ms 30.665 ms 30.978 ms]

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.8s, or reduce sample count to 40.
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 10.797 s (100 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [107.70 ms 110.02 ms 112.94 ms]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.8s, or reduce sample count to 40.
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 10.817 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [108.23 ms 111.91 ms 116.53 ms]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 12.5s, or reduce sample count to 40.
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 12.490 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [110.85 ms 116.21 ms 122.65 ms]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) high mild
  12 (12.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 122.3s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 122.31 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [1.0418 s 1.0531 s 1.0668 s]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 103.5s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 103.50 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [1.0449 s 1.0495 s 1.0548 s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 107.8s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 107.83 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [1.0513 s 1.0571 s 1.0640 s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.2294 s (500 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [10.292 ms 10.372 ms 10.482 ms]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.1579 s (500 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [10.206 ms 10.259 ms 10.317 ms]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0722 s (202k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [24.586 µs 24.707 µs 24.841 µs]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0534 s (202k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [24.597 µs 24.700 µs 24.812 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 6.3011 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [15.614 ms 15.706 ms 15.837 ms]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.6005 s (300 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [16.400 ms 16.535 ms 16.719 ms]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.3084 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [349.57 µs 351.18 µs 352.89 µs]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 5.2241 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [349.09 µs 351.39 µs 353.78 µs]
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low severe
  7 (7.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 6.2273 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [622.34 µs 627.97 µs 637.28 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.3080 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.7940 ms 7.3684 ms 8.0717 ms]
Found 19 outliers among 100 measurements (19.00%)
  8 (8.00%) high mild
  11 (11.00%) high severe
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.0269 s (1800 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [2.7483 ms 2.7615 ms 2.7763 ms]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.7s, or reduce sample count to 60.
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 7.7424 s (100 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [60.812 ms 69.351 ms 79.920 ms]
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) high mild
  13 (13.00%) high severe
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.4975 s (800 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [6.9097 ms 7.0042 ms 7.1062 ms]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 11.8s, or reduce sample count to 40.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 11.786 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [120.24 ms 128.82 ms 139.21 ms]
Found 17 outliers among 100 measurements (17.00%)
  3 (3.00%) high mild
  14 (14.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0250 s (348k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [14.242 µs 14.265 µs 14.288 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.1044 s (242k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [20.919 µs 20.941 µs 20.961 µs]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0364 s (333k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [15.023 µs 15.045 µs 15.067 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0428 s (313k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [16.020 µs 16.048 µs 16.087 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0392 s (323k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [15.514 µs 15.701 µs 16.081 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0583 s (318k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [15.779 µs 15.802 µs 15.828 µs]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0052 s (530k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [9.4058 µs 9.4443 µs 9.4951 µs]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.1405 s (167k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [31.253 µs 31.388 µs 31.531 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0153 s (8085 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [609.74 µs 611.85 µs 613.30 µs]
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.3815 s (660 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [7.4961 ms 10.177 ms 11.712 ms]
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.1266 s (1870 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [2.7115 ms 2.7176 ms 2.7239 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.1s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 8.1202 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [60.216 ms 72.075 ms 87.669 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.1968 s (825 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [6.3881 ms 6.6401 ms 6.9139 ms]
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.3s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 7.3450 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [119.09 ms 138.62 ms 159.05 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0008 s (354k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [14.274 µs 14.303 µs 14.333 µs]
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0004 s (240k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [21.037 µs 21.129 µs 21.247 µs]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0008 s (333k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [15.129 µs 15.149 µs 15.191 µs]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0001 s (312k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [16.113 µs 16.164 µs 16.273 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0004 s (325k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [15.526 µs 15.616 µs 15.726 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0002 s (317k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [15.842 µs 15.860 µs 15.890 µs]
Found 4 outliers among 10 measurements (40.00%)
  2 (20.00%) low mild
  2 (20.00%) high severe

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.3s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 6.2726 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [111.30 ms 131.30 ms 141.43 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.4s or enable flat sampling.
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 6.4463 s (55 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [109.28 ms 111.96 ms 118.19 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.2s.
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 5.1882 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [518.65 ms 524.29 ms 529.58 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low mild
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.5s.
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.4953 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [529.25 ms 546.21 ms 571.22 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 11.7s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 11.677 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [1.0270 s 1.0315 s 1.0365 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.3s.
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 10.318 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [1.0344 s 1.0510 s 1.0769 s]
Found 4 outliers among 10 measurements (40.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  2 (20.00%) high severe

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 6.5739 s (30 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [213.44 ms 214.22 ms 215.12 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild


```

</details>

## Ledger (UTxO)

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking utxo_store/insert/default/1000000
Benchmarking utxo_store/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 18.7s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 18.728 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [1.8697 s 1.8887 s 1.9097 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 5.1094 s (15k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [299.07 µs 299.95 µs 301.05 µs]
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.1514 s (30k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [167.87 µs 168.47 µs 169.15 µs]

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 5.1100 s (20k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [242.77 µs 243.80 µs 244.90 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.0192 s (30k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [163.86 µs 164.39 µs 165.05 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 37.0s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 37.048 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [1.9297 s 1.9457 s 1.9650 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.3s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 19.320 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [134.36 ms 135.68 ms 136.95 ms]
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 18.9s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 18.921 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [138.25 ms 140.56 ms 142.73 ms]

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.0s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 25.014 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [2.4488 s 2.4655 s 2.4812 s]
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 24.8s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 24.814 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [103.25 ms 103.92 ms 104.55 ms]

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 25.2s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 25.167 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [242.85 ms 244.21 ms 245.72 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.3s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 26.291 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [383.83 ms 386.08 ms 388.15 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.2s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 19.202 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [1.8884 s 1.8983 s 1.9101 s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.1s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 19.134 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [1.9311 s 1.9363 s 1.9416 s]
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.2s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 19.198 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [1.9316 s 1.9532 s 1.9766 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.3s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 19.341 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [1.9170 s 1.9324 s 1.9522 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.2s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 19.168 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [1.9440 s 1.9646 s 1.9852 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 6.3171 s (20k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [306.53 µs 307.21 µs 307.99 µs]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 6.1858 s (20k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [315.74 µs 322.41 µs 330.64 µs]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 6.4176 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [313.11 µs 314.24 µs 315.53 µs]
Found 13 outliers among 100 measurements (13.00%)
  10 (10.00%) high mild
  3 (3.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 6.3381 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [312.32 µs 314.00 µs 316.29 µs]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 6.4286 s (20k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [312.85 µs 313.68 µs 314.62 µs]

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.1s or enable flat sampling.
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 9.0719 s (55 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [149.90 ms 150.78 ms 152.03 ms]
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.9s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 8.9147 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [893.66 ms 901.34 ms 909.51 ms]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.8s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 19.756 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [1.9806 s 2.0122 s 2.0547 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0091 s (24k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [204.95 µs 205.19 µs 205.68 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0066 s (16k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [293.64 µs 294.50 µs 295.67 µs]
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0096 s (16k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [311.03 µs 318.13 µs 327.81 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.4s or enable flat sampling.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 8.3525 s (55 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [9.6480 ms 9.9557 ms 10.288 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.7s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 8.6894 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [60.892 ms 61.279 ms 61.652 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.1s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 19.086 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [134.62 ms 135.68 ms 136.74 ms]

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.4627 s (330 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [13.645 ms 13.794 ms 14.032 ms]
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.8s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 6.8050 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [117.92 ms 120.36 ms 126.26 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 5.0667 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [242.25 ms 249.58 ms 259.62 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 113.6s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 113.61 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [11.281 s 11.311 s 11.354 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 253.4s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 253.41 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [24.587 s 24.748 s 24.959 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0477 s (5115 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [962.73 µs 965.64 µs 969.01 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0701 s (3905 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.2548 ms 1.2644 ms 1.2717 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.9s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 10.911 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.0423 s 1.0500 s 1.0617 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 21.1s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 21.056 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [1.9825 s 2.0007 s 2.0288 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking ledger/apply_block/apply_only_shelley_50tx
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Collecting 20 samples in estimated 5.0016 s (103k iterations)
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Analyzing
ledger/apply_block/apply_only_shelley_50tx
                        time:   [47.531 µs 47.677 µs 47.791 µs]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
Benchmarking ledger/apply_block/validate_all_shelley_50tx
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Collecting 20 samples in estimated 5.0067 s (60k iterations)
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Analyzing
ledger/apply_block/validate_all_shelley_50tx
                        time:   [82.052 µs 82.382 µs 82.695 µs]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe


```

</details>

## Network

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking network/handshake_encode/n2n_version_data
Benchmarking network/handshake_encode/n2n_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2n_version_data: Collecting 100 samples in estimated 5.0001 s (246M iterations)
Benchmarking network/handshake_encode/n2n_version_data: Analyzing
network/handshake_encode/n2n_version_data
                        time:   [20.551 ns 20.714 ns 20.885 ns]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking network/handshake_encode/n2c_version_data
Benchmarking network/handshake_encode/n2c_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2c_version_data: Collecting 100 samples in estimated 5.0000 s (286M iterations)
Benchmarking network/handshake_encode/n2c_version_data: Analyzing
network/handshake_encode/n2c_version_data
                        time:   [17.226 ns 17.293 ns 17.356 ns]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) low mild

Benchmarking network/chainsync/roll_forward/encode/256
Benchmarking network/chainsync/roll_forward/encode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/256: Collecting 100 samples in estimated 5.0003 s (58M iterations)
Benchmarking network/chainsync/roll_forward/encode/256: Analyzing
network/chainsync/roll_forward/encode/256
                        time:   [84.892 ns 85.398 ns 85.968 ns]
                        thrpt:  [3.3692 GiB/s 3.3917 GiB/s 3.4119 GiB/s]
Found 21 outliers among 100 measurements (21.00%)
  4 (4.00%) low severe
  9 (9.00%) low mild
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/256
Benchmarking network/chainsync/roll_forward/decode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/256: Collecting 100 samples in estimated 5.0002 s (88M iterations)
Benchmarking network/chainsync/roll_forward/decode/256: Analyzing
network/chainsync/roll_forward/decode/256
                        time:   [55.285 ns 55.869 ns 56.724 ns]
                        thrpt:  [5.1061 GiB/s 5.1843 GiB/s 5.2390 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/1024
Benchmarking network/chainsync/roll_forward/encode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/1024: Collecting 100 samples in estimated 5.0000 s (45M iterations)
Benchmarking network/chainsync/roll_forward/encode/1024: Analyzing
network/chainsync/roll_forward/encode/1024
                        time:   [108.60 ns 110.24 ns 112.70 ns]
                        thrpt:  [8.9167 GiB/s 9.1158 GiB/s 9.2530 GiB/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/1024
Benchmarking network/chainsync/roll_forward/decode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/1024: Collecting 100 samples in estimated 5.0001 s (84M iterations)
Benchmarking network/chainsync/roll_forward/decode/1024: Analyzing
network/chainsync/roll_forward/decode/1024
                        time:   [59.121 ns 59.366 ns 59.666 ns]
                        thrpt:  [16.842 GiB/s 16.927 GiB/s 16.997 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/4096
Benchmarking network/chainsync/roll_forward/encode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/4096: Collecting 100 samples in estimated 5.0004 s (39M iterations)
Benchmarking network/chainsync/roll_forward/encode/4096: Analyzing
network/chainsync/roll_forward/encode/4096
                        time:   [125.63 ns 126.18 ns 126.71 ns]
                        thrpt:  [30.511 GiB/s 30.638 GiB/s 30.772 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) low mild
  2 (2.00%) high mild
Benchmarking network/chainsync/roll_forward/decode/4096
Benchmarking network/chainsync/roll_forward/decode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/4096: Collecting 100 samples in estimated 5.0003 s (50M iterations)
Benchmarking network/chainsync/roll_forward/decode/4096: Analyzing
network/chainsync/roll_forward/decode/4096
                        time:   [102.76 ns 103.28 ns 103.83 ns]
                        thrpt:  [37.232 GiB/s 37.430 GiB/s 37.619 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking network/chainsync/roll_backward/encode
Benchmarking network/chainsync/roll_backward/encode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/encode: Collecting 100 samples in estimated 5.0005 s (45M iterations)
Benchmarking network/chainsync/roll_backward/encode: Analyzing
network/chainsync/roll_backward/encode
                        time:   [113.77 ns 114.24 ns 114.72 ns]
                        thrpt:  [731.57 MiB/s 734.59 MiB/s 737.67 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking network/chainsync/roll_backward/decode
Benchmarking network/chainsync/roll_backward/decode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/decode: Collecting 100 samples in estimated 5.0000 s (130M iterations)
Benchmarking network/chainsync/roll_backward/decode: Analyzing
network/chainsync/roll_backward/decode
                        time:   [37.409 ns 37.765 ns 38.234 ns]
                        thrpt:  [2.1435 GiB/s 2.1702 GiB/s 2.1908 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe

Benchmarking network/blockfetch/msg_block/encode/2048
Benchmarking network/blockfetch/msg_block/encode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/2048: Collecting 100 samples in estimated 5.0002 s (77M iterations)
Benchmarking network/blockfetch/msg_block/encode/2048: Analyzing
network/blockfetch/msg_block/encode/2048
                        time:   [63.798 ns 64.101 ns 64.407 ns]
                        thrpt:  [29.686 GiB/s 29.828 GiB/s 29.970 GiB/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/2048
Benchmarking network/blockfetch/msg_block/decode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/2048: Collecting 100 samples in estimated 5.0003 s (85M iterations)
Benchmarking network/blockfetch/msg_block/decode/2048: Analyzing
network/blockfetch/msg_block/decode/2048
                        time:   [57.617 ns 58.217 ns 59.211 ns]
                        thrpt:  [32.291 GiB/s 32.843 GiB/s 33.185 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/20480
Benchmarking network/blockfetch/msg_block/encode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/20480: Collecting 100 samples in estimated 5.0006 s (35M iterations)
Benchmarking network/blockfetch/msg_block/encode/20480: Analyzing
network/blockfetch/msg_block/encode/20480
                        time:   [137.91 ns 138.52 ns 139.26 ns]
                        thrpt:  [137.00 GiB/s 137.73 GiB/s 138.34 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/20480
Benchmarking network/blockfetch/msg_block/decode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/20480: Collecting 100 samples in estimated 5.0003 s (39M iterations)
Benchmarking network/blockfetch/msg_block/decode/20480: Analyzing
network/blockfetch/msg_block/decode/20480
                        time:   [131.22 ns 132.46 ns 134.08 ns]
                        thrpt:  [142.29 GiB/s 144.02 GiB/s 145.39 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/90000
Benchmarking network/blockfetch/msg_block/encode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/90000: Collecting 100 samples in estimated 5.0011 s (5.0M iterations)
Benchmarking network/blockfetch/msg_block/encode/90000: Analyzing
network/blockfetch/msg_block/encode/90000
                        time:   [984.28 ns 987.20 ns 989.73 ns]
                        thrpt:  [84.696 GiB/s 84.912 GiB/s 85.164 GiB/s]
Found 10 outliers among 100 measurements (10.00%)
  7 (7.00%) low mild
  3 (3.00%) high mild
Benchmarking network/blockfetch/msg_block/decode/90000
Benchmarking network/blockfetch/msg_block/decode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/90000: Collecting 100 samples in estimated 5.0021 s (5.2M iterations)
Benchmarking network/blockfetch/msg_block/decode/90000: Analyzing
network/blockfetch/msg_block/decode/90000
                        time:   [965.06 ns 972.39 ns 983.67 ns]
                        thrpt:  [85.217 GiB/s 86.205 GiB/s 86.860 GiB/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe

Benchmarking network/blockfetch/request_range/encode
Benchmarking network/blockfetch/request_range/encode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/encode: Collecting 100 samples in estimated 5.0003 s (60M iterations)
Benchmarking network/blockfetch/request_range/encode: Analyzing
network/blockfetch/request_range/encode
                        time:   [83.162 ns 84.151 ns 85.420 ns]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low mild
  7 (7.00%) high severe
Benchmarking network/blockfetch/request_range/decode
Benchmarking network/blockfetch/request_range/decode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/decode: Collecting 100 samples in estimated 5.0000 s (145M iterations)
Benchmarking network/blockfetch/request_range/decode: Analyzing
network/blockfetch/request_range/decode
                        time:   [35.597 ns 36.290 ns 37.152 ns]

Benchmarking network/n2c_query_encode/pparams/hfc_success
Benchmarking network/n2c_query_encode/pparams/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/hfc_success: Collecting 100 samples in estimated 5.0002 s (94M iterations)
Benchmarking network/n2c_query_encode/pparams/hfc_success: Analyzing
network/n2c_query_encode/pparams/hfc_success
                        time:   [52.699 ns 53.302 ns 54.098 ns]
                        thrpt:  [1.5666 GiB/s 1.5900 GiB/s 1.6082 GiB/s]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low severe
  7 (7.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking network/n2c_query_encode/pparams/tag24
Benchmarking network/n2c_query_encode/pparams/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/tag24: Collecting 100 samples in estimated 5.0002 s (92M iterations)
Benchmarking network/n2c_query_encode/pparams/tag24: Analyzing
network/n2c_query_encode/pparams/tag24
                        time:   [53.821 ns 53.985 ns 54.179 ns]
                        thrpt:  [1.5643 GiB/s 1.5699 GiB/s 1.5747 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking network/n2c_query_encode/govstate/hfc_success
Benchmarking network/n2c_query_encode/govstate/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/hfc_success: Collecting 100 samples in estimated 5.0000 s (85M iterations)
Benchmarking network/n2c_query_encode/govstate/hfc_success: Analyzing
network/n2c_query_encode/govstate/hfc_success
                        time:   [58.961 ns 59.512 ns 60.255 ns]
                        thrpt:  [12.087 GiB/s 12.238 GiB/s 12.352 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
Benchmarking network/n2c_query_encode/govstate/tag24
Benchmarking network/n2c_query_encode/govstate/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/tag24: Collecting 100 samples in estimated 5.0001 s (83M iterations)
Benchmarking network/n2c_query_encode/govstate/tag24: Analyzing
network/n2c_query_encode/govstate/tag24
                        time:   [60.242 ns 61.342 ns 62.686 ns]
                        thrpt:  [11.618 GiB/s 11.873 GiB/s 12.089 GiB/s]
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) low mild
  1 (1.00%) high mild
  10 (10.00%) high severe
Benchmarking network/n2c_query_encode/era_mismatch
Benchmarking network/n2c_query_encode/era_mismatch: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/era_mismatch: Collecting 100 samples in estimated 5.0000 s (286M iterations)
Benchmarking network/n2c_query_encode/era_mismatch: Analyzing
network/n2c_query_encode/era_mismatch
                        time:   [17.470 ns 17.516 ns 17.559 ns]
                        thrpt:  [41.478 GiB/s 41.579 GiB/s 41.687 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild


```

</details>

## Consensus

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Collecting 100 samples in estimated 5.0290 s (177k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Analyzing
consensus/vrf_leader_check/single/sigma=0.0000247
                        time:   [27.710 µs 27.742 µs 27.780 µs]
Found 19 outliers among 100 measurements (19.00%)
  12 (12.00%) high mild
  7 (7.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.001
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Collecting 100 samples in estimated 5.0166 s (177k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Analyzing
consensus/vrf_leader_check/single/sigma=0.001
                        time:   [27.844 µs 27.912 µs 27.994 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.01
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Collecting 100 samples in estimated 5.0710 s (182k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Analyzing
consensus/vrf_leader_check/single/sigma=0.01
                        time:   [27.725 µs 27.746 µs 27.772 µs]
Found 19 outliers among 100 measurements (19.00%)
  19 (19.00%) high severe
Benchmarking consensus/vrf_leader_check/batch/21600
Benchmarking consensus/vrf_leader_check/batch/21600: Warming up for 3.0000 s

Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 12.1s, or reduce sample count to 10.
Benchmarking consensus/vrf_leader_check/batch/21600: Collecting 20 samples in estimated 12.071 s (20 iterations)
Benchmarking consensus/vrf_leader_check/batch/21600: Analyzing
consensus/vrf_leader_check/batch/21600
                        time:   [597.69 ms 598.64 ms 599.88 ms]
                        thrpt:  [36.007 Kelem/s 36.082 Kelem/s 36.139 Kelem/s]
Found 5 outliers among 20 measurements (25.00%)
  2 (10.00%) low mild
  1 (5.00%) high mild
  2 (10.00%) high severe

Benchmarking consensus/validate_header/replay_mode
Benchmarking consensus/validate_header/replay_mode: Warming up for 3.0000 s
Benchmarking consensus/validate_header/replay_mode: Collecting 100 samples in estimated 5.0000 s (525M iterations)
Benchmarking consensus/validate_header/replay_mode: Analyzing
consensus/validate_header/replay_mode
                        time:   [9.4737 ns 9.4857 ns 9.5001 ns]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe

Benchmarking consensus/chain_selection/longer_fork_100
Benchmarking consensus/chain_selection/longer_fork_100: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/longer_fork_100: Collecting 100 samples in estimated 5.0000 s (5.1B iterations)
Benchmarking consensus/chain_selection/longer_fork_100: Analyzing
consensus/chain_selection/longer_fork_100
                        time:   [980.95 ps 990.22 ps 1.0059 ns]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking consensus/chain_selection/equal_length_tiebreak
Benchmarking consensus/chain_selection/equal_length_tiebreak: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/equal_length_tiebreak: Collecting 100 samples in estimated 5.0011 s (19M iterations)
Benchmarking consensus/chain_selection/equal_length_tiebreak: Analyzing
consensus/chain_selection/equal_length_tiebreak
                        time:   [265.69 ns 265.92 ns 266.14 ns]
Found 14 outliers among 100 measurements (14.00%)
  9 (9.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking consensus/chain_selection/prefer_simple
Benchmarking consensus/chain_selection/prefer_simple: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/prefer_simple: Collecting 100 samples in estimated 5.0000 s (7.1B iterations)
Benchmarking consensus/chain_selection/prefer_simple: Analyzing
consensus/chain_selection/prefer_simple
                        time:   [722.17 ps 746.50 ps 773.09 ps]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe


```

</details>

## LSM

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking lsm/insert/random_keys/10000
Benchmarking lsm/insert/random_keys/10000: Warming up for 3.0000 s
Benchmarking lsm/insert/random_keys/10000: Collecting 10 samples in estimated 5.0534 s (2035 iterations)
Benchmarking lsm/insert/random_keys/10000: Analyzing
lsm/insert/random_keys/10000
                        time:   [2.3622 ms 2.3712 ms 2.3794 ms]
                        thrpt:  [4.2027 Melem/s 4.2173 Melem/s 4.2334 Melem/s]

Benchmarking lsm/point_lookup/hit_random/10000
Benchmarking lsm/point_lookup/hit_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/hit_random/10000: Collecting 100 samples in estimated 6.4396 s (20k iterations)
Benchmarking lsm/point_lookup/hit_random/10000: Analyzing
lsm/point_lookup/hit_random/10000
                        time:   [319.16 µs 320.62 µs 323.06 µs]
                        thrpt:  [3.0954 Melem/s 3.1189 Melem/s 3.1332 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking lsm/point_lookup/miss_random/10000
Benchmarking lsm/point_lookup/miss_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/miss_random/10000: Collecting 100 samples in estimated 5.0455 s (71k iterations)
Benchmarking lsm/point_lookup/miss_random/10000: Analyzing
lsm/point_lookup/miss_random/10000
                        time:   [72.011 µs 72.447 µs 72.867 µs]
                        thrpt:  [13.724 Melem/s 13.803 Melem/s 13.887 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe

Benchmarking lsm/range_scan/window_100_of_10k/10000
Benchmarking lsm/range_scan/window_100_of_10k/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/window_100_of_10k/10000: Collecting 20 samples in estimated 5.0047 s (112k iterations)
Benchmarking lsm/range_scan/window_100_of_10k/10000: Analyzing
lsm/range_scan/window_100_of_10k/10000
                        time:   [44.477 µs 45.180 µs 45.784 µs]
                        thrpt:  [2.1842 Melem/s 2.2134 Melem/s 2.2483 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) high mild
  3 (15.00%) high severe
Benchmarking lsm/range_scan/full_scan/10000
Benchmarking lsm/range_scan/full_scan/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/full_scan/10000: Collecting 20 samples in estimated 5.0906 s (3990 iterations)
Benchmarking lsm/range_scan/full_scan/10000: Analyzing
lsm/range_scan/full_scan/10000
                        time:   [1.2557 ms 1.2704 ms 1.2897 ms]
                        thrpt:  [7.7535 Melem/s 7.8715 Melem/s 7.9639 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  3 (15.00%) high mild
  1 (5.00%) high severe

Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Warming up for 3.0000 s
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Collecting 10 samples in estimated 5.4062 s (605 iterations)
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Analyzing
lsm/apply_batch/inserts_10k_deletes_2.5k/10000
                        time:   [4.1243 ms 4.1718 ms 4.2289 ms]
                        thrpt:  [2.3647 Melem/s 2.3970 Melem/s 2.4246 Melem/s]

Benchmarking lsm/snapshot/save_10k
Benchmarking lsm/snapshot/save_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/save_10k: Collecting 10 samples in estimated 5.0253 s (990 iterations)
Benchmarking lsm/snapshot/save_10k: Analyzing
lsm/snapshot/save_10k   time:   [397.27 µs 406.51 µs 416.19 µs]
Benchmarking lsm/snapshot/load_10k
Benchmarking lsm/snapshot/load_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/load_10k: Collecting 10 samples in estimated 5.0810 s (825 iterations)
Benchmarking lsm/snapshot/load_10k: Analyzing
lsm/snapshot/load_10k   time:   [1.2672 ms 1.2835 ms 1.3057 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild


```

</details>

## Mempool

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking mempool/add/txs/1000
Benchmarking mempool/add/txs/1000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/1000: Collecting 100 samples in estimated 6.4780 s (10k iterations)
Benchmarking mempool/add/txs/1000: Analyzing
mempool/add/txs/1000    time:   [623.86 µs 626.22 µs 629.56 µs]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
Benchmarking mempool/add/txs/5000
Benchmarking mempool/add/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/5000: Collecting 100 samples in estimated 5.1450 s (1300 iterations)
Benchmarking mempool/add/txs/5000: Analyzing
mempool/add/txs/5000    time:   [3.8347 ms 3.8890 ms 3.9439 ms]
Benchmarking mempool/add/txs/10000
Benchmarking mempool/add/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/10000: Collecting 100 samples in estimated 7.4231 s (300 iterations)
Benchmarking mempool/add/txs/10000: Analyzing
mempool/add/txs/10000   time:   [19.987 ms 20.255 ms 20.501 ms]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low severe
  4 (4.00%) low mild
  1 (1.00%) high mild

Benchmarking mempool/remove/txs/1000
Benchmarking mempool/remove/txs/1000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/1000: Collecting 100 samples in estimated 9.8587 s (10k iterations)
Benchmarking mempool/remove/txs/1000: Analyzing
mempool/remove/txs/1000 time:   [409.06 µs 409.68 µs 410.37 µs]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/remove/txs/5000
Benchmarking mempool/remove/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/5000: Collecting 100 samples in estimated 5.5147 s (1000 iterations)
Benchmarking mempool/remove/txs/5000: Analyzing
mempool/remove/txs/5000 time:   [2.3237 ms 2.3380 ms 2.3552 ms]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking mempool/remove/txs/10000
Benchmarking mempool/remove/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/10000: Collecting 100 samples in estimated 5.5676 s (400 iterations)
Benchmarking mempool/remove/txs/10000: Analyzing
mempool/remove/txs/10000
                        time:   [5.9462 ms 6.0011 ms 6.0557 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking mempool/get_sorted/by_fee_density/1000
Benchmarking mempool/get_sorted/by_fee_density/1000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/1000: Collecting 100 samples in estimated 5.0033 s (86k iterations)
Benchmarking mempool/get_sorted/by_fee_density/1000: Analyzing
mempool/get_sorted/by_fee_density/1000
                        time:   [58.790 µs 59.043 µs 59.323 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking mempool/get_sorted/by_fee_density/5000
Benchmarking mempool/get_sorted/by_fee_density/5000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/5000: Collecting 100 samples in estimated 5.1223 s (76k iterations)
Benchmarking mempool/get_sorted/by_fee_density/5000: Analyzing
mempool/get_sorted/by_fee_density/5000
                        time:   [68.029 µs 68.365 µs 68.764 µs]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking mempool/get_sorted/by_fee_density/10000
Benchmarking mempool/get_sorted/by_fee_density/10000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/10000: Collecting 100 samples in estimated 5.0565 s (86k iterations)
Benchmarking mempool/get_sorted/by_fee_density/10000: Analyzing
mempool/get_sorted/by_fee_density/10000
                        time:   [59.489 µs 59.987 µs 60.744 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

Benchmarking mempool/drain_readd/txs/1000
Benchmarking mempool/drain_readd/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.1s, enable flat sampling, or reduce sample count to 60.
Benchmarking mempool/drain_readd/txs/1000: Collecting 100 samples in estimated 6.1358 s (5050 iterations)
Benchmarking mempool/drain_readd/txs/1000: Analyzing
mempool/drain_readd/txs/1000
                        time:   [639.05 µs 639.52 µs 640.04 µs]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/drain_readd/txs/5000
Benchmarking mempool/drain_readd/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/5000: Collecting 100 samples in estimated 5.1384 s (700 iterations)
Benchmarking mempool/drain_readd/txs/5000: Analyzing
mempool/drain_readd/txs/5000
                        time:   [3.9801 ms 4.0416 ms 4.1064 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/drain_readd/txs/10000
Benchmarking mempool/drain_readd/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/10000: Collecting 100 samples in estimated 6.4657 s (200 iterations)
Benchmarking mempool/drain_readd/txs/10000: Analyzing
mempool/drain_readd/txs/10000
                        time:   [17.598 ms 17.721 ms 17.831 ms]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild

Benchmarking mempool/batch_remove/50_from/5000
Benchmarking mempool/batch_remove/50_from/5000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/5000: Collecting 100 samples in estimated 5.0052 s (4.4M iterations)
Benchmarking mempool/batch_remove/50_from/5000: Analyzing
mempool/batch_remove/50_from/5000
                        time:   [1.1273 µs 1.1284 µs 1.1297 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/batch_remove/50_from/10000
Benchmarking mempool/batch_remove/50_from/10000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/10000: Collecting 100 samples in estimated 5.0001 s (4.4M iterations)
Benchmarking mempool/batch_remove/50_from/10000: Analyzing
mempool/batch_remove/50_from/10000
                        time:   [1.1203 µs 1.1213 µs 1.1225 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe


```

</details>

## Crypto

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking ed25519_verify/single
Benchmarking ed25519_verify/single: Warming up for 3.0000 s
Benchmarking ed25519_verify/single: Collecting 100 samples in estimated 5.0773 s (116k iterations)
Benchmarking ed25519_verify/single: Analyzing
ed25519_verify/single   time:   [52.394 µs 52.957 µs 53.418 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild

Benchmarking ed25519_batch_verify/sequential/1
Benchmarking ed25519_batch_verify/sequential/1: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/1: Collecting 100 samples in estimated 5.2002 s (116k iterations)
Benchmarking ed25519_batch_verify/sequential/1: Analyzing
ed25519_batch_verify/sequential/1
                        time:   [44.529 µs 44.626 µs 44.736 µs]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
Benchmarking ed25519_batch_verify/sequential/10
Benchmarking ed25519_batch_verify/sequential/10: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/10: Collecting 100 samples in estimated 6.7394 s (15k iterations)
Benchmarking ed25519_batch_verify/sequential/10: Analyzing
ed25519_batch_verify/sequential/10
                        time:   [444.15 µs 446.46 µs 448.84 µs]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe
Benchmarking ed25519_batch_verify/sequential/50
Benchmarking ed25519_batch_verify/sequential/50: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/50: Collecting 100 samples in estimated 5.1119 s (2300 iterations)
Benchmarking ed25519_batch_verify/sequential/50: Analyzing
ed25519_batch_verify/sequential/50
                        time:   [2.2132 ms 2.2146 ms 2.2161 ms]
Benchmarking ed25519_batch_verify/sequential/100
Benchmarking ed25519_batch_verify/sequential/100: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/100: Collecting 100 samples in estimated 5.3205 s (1200 iterations)
Benchmarking ed25519_batch_verify/sequential/100: Analyzing
ed25519_batch_verify/sequential/100
                        time:   [4.4167 ms 4.4235 ms 4.4328 ms]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking ed25519_batch_verify/sequential/200
Benchmarking ed25519_batch_verify/sequential/200: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/200: Collecting 100 samples in estimated 5.4397 s (600 iterations)
Benchmarking ed25519_batch_verify/sequential/200: Analyzing
ed25519_batch_verify/sequential/200
                        time:   [9.0159 ms 9.0276 ms 9.0419 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking ed25519_batch_verify/sequential/500
Benchmarking ed25519_batch_verify/sequential/500: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/500: Collecting 100 samples in estimated 6.8312 s (300 iterations)
Benchmarking ed25519_batch_verify/sequential/500: Analyzing
ed25519_batch_verify/sequential/500
                        time:   [22.691 ms 22.711 ms 22.733 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking keyhash_from_vkey/single
Benchmarking keyhash_from_vkey/single: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/single: Collecting 100 samples in estimated 5.0007 s (28M iterations)
Benchmarking keyhash_from_vkey/single: Analyzing
keyhash_from_vkey/single
                        time:   [174.67 ns 175.01 ns 175.40 ns]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/10
Benchmarking keyhash_from_vkey/batch/10: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/10: Collecting 100 samples in estimated 5.0005 s (3.1M iterations)
Benchmarking keyhash_from_vkey/batch/10: Analyzing
keyhash_from_vkey/batch/10
                        time:   [1.6396 µs 1.6414 µs 1.6432 µs]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/50
Benchmarking keyhash_from_vkey/batch/50: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/50: Collecting 100 samples in estimated 5.0025 s (611k iterations)
Benchmarking keyhash_from_vkey/batch/50: Analyzing
keyhash_from_vkey/batch/50
                        time:   [8.1942 µs 8.2203 µs 8.2599 µs]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
Benchmarking keyhash_from_vkey/batch/100
Benchmarking keyhash_from_vkey/batch/100: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/100: Collecting 100 samples in estimated 5.0520 s (308k iterations)
Benchmarking keyhash_from_vkey/batch/100: Analyzing
keyhash_from_vkey/batch/100
                        time:   [16.430 µs 16.482 µs 16.548 µs]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
Benchmarking keyhash_from_vkey/batch/200
Benchmarking keyhash_from_vkey/batch/200: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/200: Collecting 100 samples in estimated 5.1164 s (157k iterations)
Benchmarking keyhash_from_vkey/batch/200: Analyzing
keyhash_from_vkey/batch/200
                        time:   [32.760 µs 32.788 µs 32.815 µs]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking keyhash_from_vkey/batch/500
Benchmarking keyhash_from_vkey/batch/500: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/500: Collecting 100 samples in estimated 5.3702 s (66k iterations)
Benchmarking keyhash_from_vkey/batch/500: Analyzing
keyhash_from_vkey/batch/500
                        time:   [81.995 µs 82.117 µs 82.284 µs]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high severe

Benchmarking vrf_verify/single_proof
Benchmarking vrf_verify/single_proof: Warming up for 3.0000 s
Benchmarking vrf_verify/single_proof: Collecting 100 samples in estimated 5.1191 s (30k iterations)
Benchmarking vrf_verify/single_proof: Analyzing
vrf_verify/single_proof time:   [168.60 µs 168.69 µs 168.80 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe


```

</details>

## Primitives

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking blake2b_256/hash/32B_txhash
Benchmarking blake2b_256/hash/32B_txhash: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/32B_txhash: Collecting 100 samples in estimated 5.0006 s (35M iterations)
Benchmarking blake2b_256/hash/32B_txhash: Analyzing
blake2b_256/hash/32B_txhash
                        time:   [127.57 ns 127.70 ns 127.83 ns]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking blake2b_256/hash/64B_vkey
Benchmarking blake2b_256/hash/64B_vkey: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/64B_vkey: Collecting 100 samples in estimated 5.0005 s (35M iterations)
Benchmarking blake2b_256/hash/64B_vkey: Analyzing
blake2b_256/hash/64B_vkey
                        time:   [127.33 ns 127.54 ns 127.76 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_256/hash/256B_small_tx
Benchmarking blake2b_256/hash/256B_small_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/256B_small_tx: Collecting 100 samples in estimated 5.0006 s (22M iterations)
Benchmarking blake2b_256/hash/256B_small_tx: Analyzing
blake2b_256/hash/256B_small_tx
                        time:   [230.79 ns 231.50 ns 232.60 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/500B_avg_tx
Benchmarking blake2b_256/hash/500B_avg_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/500B_avg_tx: Collecting 100 samples in estimated 5.0015 s (11M iterations)
Benchmarking blake2b_256/hash/500B_avg_tx: Analyzing
blake2b_256/hash/500B_avg_tx
                        time:   [447.05 ns 447.16 ns 447.27 ns]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/1KB_tx_body
Benchmarking blake2b_256/hash/1KB_tx_body: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/1KB_tx_body: Collecting 100 samples in estimated 5.0022 s (5.8M iterations)
Benchmarking blake2b_256/hash/1KB_tx_body: Analyzing
blake2b_256/hash/1KB_tx_body
                        time:   [868.21 ns 870.07 ns 872.68 ns]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  4 (4.00%) high severe
Benchmarking blake2b_256/hash/4KB_large_tx
Benchmarking blake2b_256/hash/4KB_large_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/4KB_large_tx: Collecting 100 samples in estimated 5.0135 s (1.5M iterations)
Benchmarking blake2b_256/hash/4KB_large_tx: Analyzing
blake2b_256/hash/4KB_large_tx
                        time:   [3.4232 µs 3.4263 µs 3.4307 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking blake2b_256/hash/16KB_block_header
Benchmarking blake2b_256/hash/16KB_block_header: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/16KB_block_header: Collecting 100 samples in estimated 5.0453 s (369k iterations)
Benchmarking blake2b_256/hash/16KB_block_header: Analyzing
blake2b_256/hash/16KB_block_header
                        time:   [13.631 µs 13.670 µs 13.731 µs]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking blake2b_256/hash/20KB_avg_block
Benchmarking blake2b_256/hash/20KB_avg_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/20KB_avg_block: Collecting 100 samples in estimated 5.0157 s (293k iterations)
Benchmarking blake2b_256/hash/20KB_avg_block: Analyzing
blake2b_256/hash/20KB_avg_block
                        time:   [17.036 µs 17.054 µs 17.078 µs]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  7 (7.00%) high severe
Benchmarking blake2b_256/hash/90KB_max_block
Benchmarking blake2b_256/hash/90KB_max_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/90KB_max_block: Collecting 100 samples in estimated 5.0504 s (66k iterations)
Benchmarking blake2b_256/hash/90KB_max_block: Analyzing
blake2b_256/hash/90KB_max_block
                        time:   [76.627 µs 76.742 µs 76.938 µs]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe

Benchmarking blake2b_224/hash/32B_vkey_to_keyhash
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Collecting 100 samples in estimated 5.0006 s (33M iterations)
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Analyzing
blake2b_224/hash/32B_vkey_to_keyhash
                        time:   [133.97 ns 134.03 ns 134.10 ns]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_224/hash/64B_script_bytes
Benchmarking blake2b_224/hash/64B_script_bytes: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/64B_script_bytes: Collecting 100 samples in estimated 5.0001 s (33M iterations)
Benchmarking blake2b_224/hash/64B_script_bytes: Analyzing
blake2b_224/hash/64B_script_bytes
                        time:   [134.59 ns 135.23 ns 136.05 ns]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking blake2b_224/hash/256B_address_payload
Benchmarking blake2b_224/hash/256B_address_payload: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/256B_address_payload: Collecting 100 samples in estimated 5.0010 s (21M iterations)
Benchmarking blake2b_224/hash/256B_address_payload: Analyzing
blake2b_224/hash/256B_address_payload
                        time:   [237.73 ns 237.90 ns 238.06 ns]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking blake2b_batch/224_keyhashes/10
Benchmarking blake2b_batch/224_keyhashes/10: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/10: Collecting 100 samples in estimated 5.0059 s (4.0M iterations)
Benchmarking blake2b_batch/224_keyhashes/10: Analyzing
blake2b_batch/224_keyhashes/10
                        time:   [1.2730 µs 1.2760 µs 1.2798 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/50
Benchmarking blake2b_batch/224_keyhashes/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/50: Collecting 100 samples in estimated 5.0071 s (793k iterations)
Benchmarking blake2b_batch/224_keyhashes/50: Analyzing
blake2b_batch/224_keyhashes/50
                        time:   [6.3664 µs 6.3770 µs 6.3879 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/100
Benchmarking blake2b_batch/224_keyhashes/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/100: Collecting 100 samples in estimated 5.0469 s (399k iterations)
Benchmarking blake2b_batch/224_keyhashes/100: Analyzing
blake2b_batch/224_keyhashes/100
                        time:   [12.723 µs 12.741 µs 12.759 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/500
Benchmarking blake2b_batch/224_keyhashes/500: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/500: Collecting 100 samples in estimated 5.1080 s (81k iterations)
Benchmarking blake2b_batch/224_keyhashes/500: Analyzing
blake2b_batch/224_keyhashes/500
                        time:   [63.608 µs 63.707 µs 63.805 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/50
Benchmarking blake2b_batch/256_txbodies_500B/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/50: Collecting 100 samples in estimated 5.0282 s (227k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/50: Analyzing
blake2b_batch/256_txbodies_500B/50
                        time:   [22.155 µs 22.189 µs 22.234 µs]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/100
Benchmarking blake2b_batch/256_txbodies_500B/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/100: Collecting 100 samples in estimated 5.1444 s (116k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/100: Analyzing
blake2b_batch/256_txbodies_500B/100
                        time:   [44.280 µs 44.309 µs 44.341 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/300
Benchmarking blake2b_batch/256_txbodies_500B/300: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/300: Collecting 100 samples in estimated 5.3686 s (40k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/300: Analyzing
blake2b_batch/256_txbodies_500B/300
                        time:   [132.83 µs 132.95 µs 133.12 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe


```

</details>

## Serialization

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components
Gnuplot not found, using plotters backend
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Collecting 100 samples in estimated 5.0072 s (3.2M iterations)
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Analyzing
serialization/encode_transaction/conway_2in_2out_2wit
                        time:   [1.5862 µs 1.5926 µs 1.6029 µs]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking serialization/encode_transaction/body_only_2in_2out
Benchmarking serialization/encode_transaction/body_only_2in_2out: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/body_only_2in_2out: Collecting 100 samples in estimated 5.0016 s (4.3M iterations)
Benchmarking serialization/encode_transaction/body_only_2in_2out: Analyzing
serialization/encode_transaction/body_only_2in_2out
                        time:   [1.1672 µs 1.1686 µs 1.1703 µs]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe

Benchmarking serialization/encode_block_header/with_vrf_output
Benchmarking serialization/encode_block_header/with_vrf_output: Warming up for 3.0000 s
Benchmarking serialization/encode_block_header/with_vrf_output: Collecting 100 samples in estimated 5.0053 s (4.6M iterations)
Benchmarking serialization/encode_block_header/with_vrf_output: Analyzing
serialization/encode_block_header/with_vrf_output
                        time:   [1.0813 µs 1.0850 µs 1.0892 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking serialization/encode_value/ada_only
Benchmarking serialization/encode_value/ada_only: Warming up for 3.0000 s
Benchmarking serialization/encode_value/ada_only: Collecting 100 samples in estimated 5.0000 s (265M iterations)
Benchmarking serialization/encode_value/ada_only: Analyzing
serialization/encode_value/ada_only
                        time:   [18.545 ns 18.574 ns 18.603 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking serialization/encode_value/multi_asset_3policy_5asset
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Warming up for 3.0000 s
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Collecting 100 samples in estimated 5.0010 s (7.5M iterations)
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Analyzing
serialization/encode_value/multi_asset_3policy_5asset
                        time:   [664.38 ns 665.04 ns 665.80 ns]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe


```

</details>

## LSM stress tests

<details>
<summary>Raw measurements</summary>

```
info: syncing channel updates for 1.97.0-x86_64-unknown-linux-gnu
info: latest update on 2026-07-09 for version 1.97.0 (2d8144b78 2026-07-07)
info: downloading 3 components

running 3 tests
[test_mainnet_scale_delete_amplification] inserting 100000 entries...
[test_mainnet_scale_wal_crash_recovery] writing 100000 entries (WAL only)...
[test_mainnet_scale_insert_read] inserting 100000 entries...
[test_mainnet_scale_wal_crash_recovery] simulating crash (drop without flush)...
[test_mainnet_scale_wal_crash_recovery] reopened, verifying 100000 entries...
[test_mainnet_scale_wal_crash_recovery] all 100000 entries recovered — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
[test_mainnet_scale_delete_amplification] insert flush complete
[test_mainnet_scale_delete_amplification] deleting 80000 entries...
[test_mainnet_scale_insert_read] flush complete, sampling 1000 keys...
[test_mainnet_scale_insert_read] verified 1000/1000 sampled keys — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
[test_mainnet_scale_delete_amplification] delete flush complete
[test_mainnet_scale_delete_amplification] verifying surviving 20000 entries...
[test_mainnet_scale_delete_amplification] verifying 1K deleted keys return None...
[test_mainnet_scale_delete_amplification] range scanning for exact count...
[test_mainnet_scale_delete_amplification] 20000 entries confirmed — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out; finished in 0.59s

   Doc-tests dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s


```

</details>

