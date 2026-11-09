# Nightly Benchmark Results — 2026-11-09

Captured on **2026-11-09** from commit [`cf0773d`](https://github.com/MichaelJFazio/dugite/commit/cf0773d) on `main` (GitHub Actions `ubuntu-latest`).

### Measurement environment

| | |
|---|---|
| Runner | GitHub Actions `ubuntu-latest` (shared, virtualised) |
| CPU | AMD EPYC 7763 64-Core Processor |
| vCPUs | 4 |
| Memory | 15.6 GiB |
| Kernel | Linux 7.0.0-1017-azure |
| Toolchain | rustc 1.97.0 (2d8144b78 2026-07-07) |
| Build profile | `bench` (release + debug assertions off) |

> **Read absolute numbers with care.** These run on shared, virtualised GitHub-hosted runners whose CPU model is not pinned and whose neighbours are not controlled. Treat the figures as an order-of-magnitude regression tripwire, not as hardware benchmarks: a `change: ±x%` between two dates can reflect a different host class rather than a code change. Use the interactive trend lines below, where a real regression shows as a sustained step rather than a single-day spike.

> **Not measured here:** end-to-end sync throughput, mainnet-scale UTxO memory, and anything requiring a live network. Those are covered by the devnet-validate and soak rigs, not by Criterion.

> **Interactive reports**, including per-benchmark detail pages and historical trend lines, are published at <https://michaeljfazio.github.io/dugite/benchmarks/>. Each section below also links directly to its interactive report.

> The collapsed _Raw measurements_ blocks contain the filtered `cargo bench` output (cargo build chatter and ANSI escapes are stripped). Full unfiltered logs are uploaded as the `benchmark-results-249` workflow artifact.

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
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 5.2524 s (20 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [251.69 ms 254.82 ms 259.09 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 6.5917 s (20k iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [326.94 µs 327.81 µs 328.66 µs]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 5.0322 s (300 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [16.663 ms 17.178 ms 17.742 ms]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 5.0000 s (16B iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [311.77 ps 312.22 ps 312.83 ps]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 5.0259 s (222k iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [23.564 µs 23.897 µs 24.219 µs]

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 5.0007 s (20M iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [247.68 ns 248.44 ns 249.29 ns]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 6.1337 s (110 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [7.6440 ms 7.7243 ms 7.8866 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 5.0720 s (20 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [254.42 ms 255.65 ms 256.82 ms]
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 5.0148 s (20 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [248.19 ms 248.99 ms 249.86 ms]
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 5.0100 s (20 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [33.334 ms 33.635 ms 33.966 ms]
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 7.4318 s (30 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [33.212 ms 33.481 ms 33.800 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 18.5s, or reduce sample count to 20.
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 18.525 s (100 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [183.39 ms 183.93 ms 184.57 ms]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 18.5s, or reduce sample count to 20.
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 18.526 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [185.54 ms 186.77 ms 188.70 ms]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 18.6s, or reduce sample count to 20.
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 18.636 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [185.01 ms 185.50 ms 186.00 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 182.2s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 182.20 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [1.8115 s 1.8177 s 1.8259 s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 181.9s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 181.87 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [1.8118 s 1.8142 s 1.8168 s]
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 181.5s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 181.51 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [1.8128 s 1.8154 s 1.8182 s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.4671 s (500 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [11.021 ms 11.056 ms 11.090 ms]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.5023 s (500 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [10.785 ms 10.830 ms 10.876 ms]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.1409 s (162k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [33.348 µs 33.793 µs 34.227 µs]
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.1433 s (162k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [33.331 µs 33.776 µs 34.201 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 5.1640 s (300 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [17.213 ms 17.289 ms 17.366 ms]
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.4439 s (300 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [18.409 ms 18.463 ms 18.517 ms]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.4657 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [356.58 µs 357.64 µs 358.70 µs]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low mild
  4 (4.00%) high mild
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 5.4829 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [359.16 µs 360.58 µs 362.56 µs]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 8.2111 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [806.29 µs 808.33 µs 810.53 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.1348 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.3717 ms 6.3916 ms 6.4130 ms]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.2101 s (1400 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [3.6358 ms 3.6660 ms 3.7038 ms]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 8.0703 s (200 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [40.240 ms 40.310 ms 40.382 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.4969 s (700 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [7.6627 ms 7.7307 ms 7.8044 ms]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.9s, or reduce sample count to 60.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 7.9250 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [80.161 ms 80.394 ms 80.632 ms]

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0811 s (263k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [19.380 µs 19.404 µs 19.431 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0855 s (192k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [26.433 µs 26.674 µs 26.927 µs]
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0661 s (252k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [20.119 µs 20.138 µs 20.158 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0150 s (247k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [19.990 µs 20.139 µs 20.293 µs]
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0925 s (247k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [20.607 µs 20.632 µs 20.662 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0030 s (252k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [19.485 µs 19.628 µs 19.779 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0259 s (414k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [12.129 µs 12.142 µs 12.158 µs]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.1154 s (116k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [40.759 µs 41.451 µs 42.192 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0136 s (6270 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [800.31 µs 801.13 µs 802.03 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.3196 s (825 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.3881 ms 6.4142 ms 6.4415 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.0802 s (1430 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [3.5315 ms 3.5404 ms 3.5505 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 6.7311 s (165 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [40.149 ms 40.383 ms 40.664 ms]
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.3183 s (715 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [7.7732 ms 7.8879 ms 7.9487 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 8.8418 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [79.390 ms 79.527 ms 79.712 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0001 s (257k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [19.308 µs 19.317 µs 19.338 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0000 s (181k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [26.502 µs 26.526 µs 26.550 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0011 s (247k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [20.095 µs 20.119 µs 20.163 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0006 s (243k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [20.230 µs 20.237 µs 20.249 µs]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0007 s (242k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [20.535 µs 20.551 µs 20.575 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0008 s (249k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [19.787 µs 19.826 µs 19.887 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.6481 s (30 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [187.25 ms 187.53 ms 187.78 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.5980 s (30 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [185.17 ms 186.15 ms 187.20 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.0s.
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 9.0469 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [901.35 ms 904.89 ms 909.10 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.2s.
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 9.1503 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [909.12 ms 912.66 ms 916.71 ms]
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 18.4s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 18.359 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [1.7991 s 1.8067 s 1.8159 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 17.9s.
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 17.920 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [1.8135 s 1.8687 s 1.9658 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 7.4892 s (30 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [250.46 ms 251.22 ms 252.33 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe


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

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.7s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 28.733 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.7123 s 2.7296 s 2.7450 s]

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 5.8257 s (10k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [575.46 µs 575.95 µs 576.60 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.3613 s (15k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [352.85 µs 354.09 µs 355.46 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 7.0708 s (15k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [467.62 µs 469.10 µs 471.32 µs]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.1956 s (15k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [344.02 µs 344.63 µs 345.31 µs]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 53.5s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 53.461 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.8686 s 2.8838 s 2.9011 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.1s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 29.082 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [378.98 ms 382.92 ms 386.88 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.9s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 28.892 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [381.38 ms 385.62 ms 389.97 ms]

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 37.3s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 37.338 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [3.6636 s 3.6809 s 3.6964 s]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 37.6s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 37.580 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [143.13 ms 153.12 ms 162.42 ms]

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 31.6s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 31.572 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [316.87 ms 317.46 ms 318.17 ms]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 39.8s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 39.785 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [551.19 ms 553.62 ms 556.33 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.4s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 28.403 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.8168 s 2.8279 s 2.8397 s]
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.4s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 28.411 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.8498 s 2.8627 s 2.8754 s]
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.8s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 28.763 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.8594 s 2.8717 s 2.8845 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.5s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 28.506 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.8667 s 2.8796 s 2.8947 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.6s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 28.622 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.8343 s 2.8494 s 2.8641 s]

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 5.8108 s (10k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [573.42 µs 573.98 µs 574.59 µs]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 5.7583 s (10k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [570.05 µs 570.63 µs 571.30 µs]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 5.8624 s (10k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [580.40 µs 581.66 µs 583.12 µs]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 5.8918 s (10k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [579.69 µs 580.87 µs 582.09 µs]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 5.8262 s (10k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [573.60 µs 574.70 µs 575.91 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 7.1212 s (30 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [232.98 ms 234.45 ms 236.02 ms]
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.8s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 13.786 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [1.3714 s 1.3769 s 1.3820 s]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.3s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 29.264 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.9274 s 2.9460 s 2.9614 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0091 s (12k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [412.50 µs 412.95 µs 413.58 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0266 s (9405 iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [530.97 µs 532.79 µs 536.03 µs]
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0326 s (8250 iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [607.14 µs 609.34 µs 613.08 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 7.0684 s (30 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [23.903 ms 24.890 ms 25.842 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.9s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 13.871 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [175.42 ms 177.85 ms 180.50 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.4s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 29.419 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [378.05 ms 385.42 ms 392.60 ms]

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 6.4858 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [29.288 ms 29.311 ms 29.349 ms]
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.7s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 8.6538 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [154.33 ms 155.23 ms 156.34 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 6.6758 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [332.29 ms 333.53 ms 334.88 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 190.6s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 190.60 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [18.500 s 18.646 s 18.795 s]
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 412.3s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 412.34 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [37.329 s 37.756 s 38.148 s]

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0483 s (3135 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.6131 ms 1.6170 ms 1.6194 ms]
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0497 s (2365 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.9837 ms 2.0513 ms 2.0954 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 16.7s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 16.695 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.6705 s 1.6888 s 1.7082 s]
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 32.2s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 32.232 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [3.1860 s 3.2055 s 3.2257 s]

Benchmarking ledger/apply_block/apply_only_shelley_50tx
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Collecting 20 samples in estimated 5.0033 s (71k iterations)
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Analyzing
ledger/apply_block/apply_only_shelley_50tx
                        time:   [69.615 µs 69.689 µs 69.795 µs]
Found 4 outliers among 20 measurements (20.00%)
  2 (10.00%) high mild
  2 (10.00%) high severe
Benchmarking ledger/apply_block/validate_all_shelley_50tx
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Collecting 20 samples in estimated 5.0145 s (37k iterations)
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Analyzing
ledger/apply_block/validate_all_shelley_50tx
                        time:   [133.73 µs 134.04 µs 134.36 µs]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild


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
Benchmarking network/handshake_encode/n2n_version_data: Collecting 100 samples in estimated 5.0001 s (115M iterations)
Benchmarking network/handshake_encode/n2n_version_data: Analyzing
network/handshake_encode/n2n_version_data
                        time:   [43.798 ns 43.903 ns 44.003 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking network/handshake_encode/n2c_version_data
Benchmarking network/handshake_encode/n2c_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2c_version_data: Collecting 100 samples in estimated 5.0000 s (204M iterations)
Benchmarking network/handshake_encode/n2c_version_data: Analyzing
network/handshake_encode/n2c_version_data
                        time:   [24.004 ns 24.166 ns 24.381 ns]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

Benchmarking network/chainsync/roll_forward/encode/256
Benchmarking network/chainsync/roll_forward/encode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/256: Collecting 100 samples in estimated 5.0003 s (36M iterations)
Benchmarking network/chainsync/roll_forward/encode/256: Analyzing
network/chainsync/roll_forward/encode/256
                        time:   [134.03 ns 134.22 ns 134.52 ns]
                        thrpt:  [2.1532 GiB/s 2.1580 GiB/s 2.1611 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/256
Benchmarking network/chainsync/roll_forward/decode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/256: Collecting 100 samples in estimated 5.0003 s (50M iterations)
Benchmarking network/chainsync/roll_forward/decode/256: Analyzing
network/chainsync/roll_forward/decode/256
                        time:   [99.533 ns 99.827 ns 100.16 ns]
                        thrpt:  [2.8919 GiB/s 2.9014 GiB/s 2.9100 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/1024
Benchmarking network/chainsync/roll_forward/encode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/1024: Collecting 100 samples in estimated 5.0004 s (30M iterations)
Benchmarking network/chainsync/roll_forward/encode/1024: Analyzing
network/chainsync/roll_forward/encode/1024
                        time:   [168.70 ns 168.92 ns 169.25 ns]
                        thrpt:  [5.9372 GiB/s 5.9490 GiB/s 5.9569 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/1024
Benchmarking network/chainsync/roll_forward/decode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/1024: Collecting 100 samples in estimated 5.0003 s (47M iterations)
Benchmarking network/chainsync/roll_forward/decode/1024: Analyzing
network/chainsync/roll_forward/decode/1024
                        time:   [107.46 ns 107.64 ns 107.82 ns]
                        thrpt:  [9.3198 GiB/s 9.3358 GiB/s 9.3515 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/4096
Benchmarking network/chainsync/roll_forward/encode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/4096: Collecting 100 samples in estimated 5.0012 s (19M iterations)
Benchmarking network/chainsync/roll_forward/encode/4096: Analyzing
network/chainsync/roll_forward/encode/4096
                        time:   [258.68 ns 258.84 ns 259.01 ns]
                        thrpt:  [14.926 GiB/s 14.936 GiB/s 14.945 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/4096
Benchmarking network/chainsync/roll_forward/decode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/4096: Collecting 100 samples in estimated 5.0003 s (26M iterations)
Benchmarking network/chainsync/roll_forward/decode/4096: Analyzing
network/chainsync/roll_forward/decode/4096
                        time:   [192.85 ns 193.21 ns 193.59 ns]
                        thrpt:  [19.970 GiB/s 20.009 GiB/s 20.047 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking network/chainsync/roll_backward/encode
Benchmarking network/chainsync/roll_backward/encode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/encode: Collecting 100 samples in estimated 5.0003 s (30M iterations)
Benchmarking network/chainsync/roll_backward/encode: Analyzing
network/chainsync/roll_backward/encode
                        time:   [171.32 ns 171.70 ns 172.08 ns]
                        thrpt:  [487.69 MiB/s 488.78 MiB/s 489.87 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_backward/decode
Benchmarking network/chainsync/roll_backward/decode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/decode: Collecting 100 samples in estimated 5.0002 s (68M iterations)
Benchmarking network/chainsync/roll_backward/decode: Analyzing
network/chainsync/roll_backward/decode
                        time:   [72.258 ns 72.472 ns 72.683 ns]
                        thrpt:  [1.1276 GiB/s 1.1309 GiB/s 1.1342 GiB/s]

Benchmarking network/blockfetch/msg_block/encode/2048
Benchmarking network/blockfetch/msg_block/encode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/2048: Collecting 100 samples in estimated 5.0002 s (47M iterations)
Benchmarking network/blockfetch/msg_block/encode/2048: Analyzing
network/blockfetch/msg_block/encode/2048
                        time:   [107.45 ns 107.59 ns 107.76 ns]
                        thrpt:  [17.743 GiB/s 17.770 GiB/s 17.794 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/2048
Benchmarking network/blockfetch/msg_block/decode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/2048: Collecting 100 samples in estimated 5.0005 s (46M iterations)
Benchmarking network/blockfetch/msg_block/decode/2048: Analyzing
network/blockfetch/msg_block/decode/2048
                        time:   [106.85 ns 107.35 ns 107.90 ns]
                        thrpt:  [17.720 GiB/s 17.811 GiB/s 17.895 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking network/blockfetch/msg_block/encode/20480
Benchmarking network/blockfetch/msg_block/encode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/20480: Collecting 100 samples in estimated 5.0024 s (9.7M iterations)
Benchmarking network/blockfetch/msg_block/encode/20480: Analyzing
network/blockfetch/msg_block/encode/20480
                        time:   [512.95 ns 513.93 ns 515.69 ns]
                        thrpt:  [36.996 GiB/s 37.122 GiB/s 37.193 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/20480
Benchmarking network/blockfetch/msg_block/decode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/20480: Collecting 100 samples in estimated 5.0132 s (712k iterations)
Benchmarking network/blockfetch/msg_block/decode/20480: Analyzing
network/blockfetch/msg_block/decode/20480
                        time:   [7.0297 µs 7.0348 µs 7.0418 µs]
                        thrpt:  [2.7093 GiB/s 2.7120 GiB/s 2.7140 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/90000
Benchmarking network/blockfetch/msg_block/encode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/90000: Collecting 100 samples in estimated 5.0014 s (2.4M iterations)
Benchmarking network/blockfetch/msg_block/encode/90000: Analyzing
network/blockfetch/msg_block/encode/90000
                        time:   [2.0788 µs 2.0819 µs 2.0868 µs]
                        thrpt:  [40.170 GiB/s 40.263 GiB/s 40.325 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/90000
Benchmarking network/blockfetch/msg_block/decode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/90000: Collecting 100 samples in estimated 5.0038 s (2.4M iterations)
Benchmarking network/blockfetch/msg_block/decode/90000: Analyzing
network/blockfetch/msg_block/decode/90000
                        time:   [2.0454 µs 2.0471 µs 2.0496 µs]
                        thrpt:  [40.899 GiB/s 40.948 GiB/s 40.982 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking network/blockfetch/request_range/encode
Benchmarking network/blockfetch/request_range/encode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/encode: Collecting 100 samples in estimated 5.0002 s (47M iterations)
Benchmarking network/blockfetch/request_range/encode: Analyzing
network/blockfetch/request_range/encode
                        time:   [105.51 ns 105.66 ns 105.86 ns]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking network/blockfetch/request_range/decode
Benchmarking network/blockfetch/request_range/decode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/decode: Collecting 100 samples in estimated 5.0000 s (70M iterations)
Benchmarking network/blockfetch/request_range/decode: Analyzing
network/blockfetch/request_range/decode
                        time:   [68.933 ns 69.351 ns 69.789 ns]

Benchmarking network/n2c_query_encode/pparams/hfc_success
Benchmarking network/n2c_query_encode/pparams/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/hfc_success: Collecting 100 samples in estimated 5.0002 s (92M iterations)
Benchmarking network/n2c_query_encode/pparams/hfc_success: Analyzing
network/n2c_query_encode/pparams/hfc_success
                        time:   [54.257 ns 54.296 ns 54.334 ns]
                        thrpt:  [1.5598 GiB/s 1.5609 GiB/s 1.5620 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking network/n2c_query_encode/pparams/tag24
Benchmarking network/n2c_query_encode/pparams/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/tag24: Collecting 100 samples in estimated 5.0002 s (92M iterations)
Benchmarking network/n2c_query_encode/pparams/tag24: Analyzing
network/n2c_query_encode/pparams/tag24
                        time:   [55.041 ns 55.130 ns 55.216 ns]
                        thrpt:  [1.5349 GiB/s 1.5373 GiB/s 1.5398 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/govstate/hfc_success
Benchmarking network/n2c_query_encode/govstate/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/hfc_success: Collecting 100 samples in estimated 5.0004 s (63M iterations)
Benchmarking network/n2c_query_encode/govstate/hfc_success: Analyzing
network/n2c_query_encode/govstate/hfc_success
                        time:   [80.141 ns 80.430 ns 80.730 ns]
                        thrpt:  [9.0213 GiB/s 9.0550 GiB/s 9.0876 GiB/s]
Found 17 outliers among 100 measurements (17.00%)
  16 (16.00%) high mild
  1 (1.00%) high severe
Benchmarking network/n2c_query_encode/govstate/tag24
Benchmarking network/n2c_query_encode/govstate/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/tag24: Collecting 100 samples in estimated 5.0001 s (54M iterations)
Benchmarking network/n2c_query_encode/govstate/tag24: Analyzing
network/n2c_query_encode/govstate/tag24
                        time:   [91.872 ns 91.978 ns 92.104 ns]
                        thrpt:  [7.9073 GiB/s 7.9181 GiB/s 7.9272 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking network/n2c_query_encode/era_mismatch
Benchmarking network/n2c_query_encode/era_mismatch: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/era_mismatch: Collecting 100 samples in estimated 5.0001 s (211M iterations)
Benchmarking network/n2c_query_encode/era_mismatch: Analyzing
network/n2c_query_encode/era_mismatch
                        time:   [24.232 ns 24.322 ns 24.409 ns]
                        thrpt:  [29.838 GiB/s 29.944 GiB/s 30.055 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe


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
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Collecting 100 samples in estimated 5.1323 s (131k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Analyzing
consensus/vrf_leader_check/single/sigma=0.0000247
                        time:   [39.043 µs 39.063 µs 39.085 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.001
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Collecting 100 samples in estimated 5.1489 s (131k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Analyzing
consensus/vrf_leader_check/single/sigma=0.001
                        time:   [39.139 µs 39.163 µs 39.192 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.01
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Collecting 100 samples in estimated 5.1878 s (131k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Analyzing
consensus/vrf_leader_check/single/sigma=0.01
                        time:   [39.081 µs 39.104 µs 39.134 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking consensus/vrf_leader_check/batch/21600
Benchmarking consensus/vrf_leader_check/batch/21600: Warming up for 3.0000 s

Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 16.9s, or reduce sample count to 10.
Benchmarking consensus/vrf_leader_check/batch/21600: Collecting 20 samples in estimated 16.910 s (20 iterations)
Benchmarking consensus/vrf_leader_check/batch/21600: Analyzing
consensus/vrf_leader_check/batch/21600
                        time:   [843.89 ms 844.35 ms 844.90 ms]
                        thrpt:  [25.565 Kelem/s 25.582 Kelem/s 25.596 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe

Benchmarking consensus/validate_header/replay_mode
Benchmarking consensus/validate_header/replay_mode: Warming up for 3.0000 s
Benchmarking consensus/validate_header/replay_mode: Collecting 100 samples in estimated 5.0000 s (433M iterations)
Benchmarking consensus/validate_header/replay_mode: Analyzing
consensus/validate_header/replay_mode
                        time:   [11.530 ns 11.541 ns 11.558 ns]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe

Benchmarking consensus/chain_selection/longer_fork_100
Benchmarking consensus/chain_selection/longer_fork_100: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/longer_fork_100: Collecting 100 samples in estimated 5.0000 s (3.2B iterations)
Benchmarking consensus/chain_selection/longer_fork_100: Analyzing
consensus/chain_selection/longer_fork_100
                        time:   [1.5577 ns 1.5583 ns 1.5591 ns]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking consensus/chain_selection/equal_length_tiebreak
Benchmarking consensus/chain_selection/equal_length_tiebreak: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/equal_length_tiebreak: Collecting 100 samples in estimated 5.0011 s (15M iterations)
Benchmarking consensus/chain_selection/equal_length_tiebreak: Analyzing
consensus/chain_selection/equal_length_tiebreak
                        time:   [328.81 ns 329.26 ns 329.85 ns]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
Benchmarking consensus/chain_selection/prefer_simple
Benchmarking consensus/chain_selection/prefer_simple: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/prefer_simple: Collecting 100 samples in estimated 5.0000 s (2.1B iterations)
Benchmarking consensus/chain_selection/prefer_simple: Analyzing
consensus/chain_selection/prefer_simple
                        time:   [2.4498 ns 2.4524 ns 2.4555 ns]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild


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
Benchmarking lsm/insert/random_keys/10000: Collecting 10 samples in estimated 5.0239 s (2585 iterations)
Benchmarking lsm/insert/random_keys/10000: Analyzing
lsm/insert/random_keys/10000
                        time:   [1.8116 ms 1.8306 ms 1.8568 ms]
                        thrpt:  [5.3855 Melem/s 5.4626 Melem/s 5.5200 Melem/s]

Benchmarking lsm/point_lookup/hit_random/10000
Benchmarking lsm/point_lookup/hit_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/hit_random/10000: Collecting 100 samples in estimated 6.0948 s (25k iterations)
Benchmarking lsm/point_lookup/hit_random/10000: Analyzing
lsm/point_lookup/hit_random/10000
                        time:   [243.05 µs 244.43 µs 245.93 µs]
                        thrpt:  [4.0662 Melem/s 4.0912 Melem/s 4.1144 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking lsm/point_lookup/miss_random/10000
Benchmarking lsm/point_lookup/miss_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/miss_random/10000: Collecting 100 samples in estimated 5.0643 s (81k iterations)
Benchmarking lsm/point_lookup/miss_random/10000: Analyzing
lsm/point_lookup/miss_random/10000
                        time:   [62.337 µs 62.878 µs 63.534 µs]
                        thrpt:  [15.739 Melem/s 15.904 Melem/s 16.042 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

Benchmarking lsm/range_scan/window_100_of_10k/10000
Benchmarking lsm/range_scan/window_100_of_10k/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/window_100_of_10k/10000: Collecting 20 samples in estimated 5.0077 s (121k iterations)
Benchmarking lsm/range_scan/window_100_of_10k/10000: Analyzing
lsm/range_scan/window_100_of_10k/10000
                        time:   [41.159 µs 41.246 µs 41.339 µs]
                        thrpt:  [2.4190 Melem/s 2.4245 Melem/s 2.4296 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
Benchmarking lsm/range_scan/full_scan/10000
Benchmarking lsm/range_scan/full_scan/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/full_scan/10000: Collecting 20 samples in estimated 5.0725 s (4620 iterations)
Benchmarking lsm/range_scan/full_scan/10000: Analyzing
lsm/range_scan/full_scan/10000
                        time:   [1.0879 ms 1.0971 ms 1.1086 ms]
                        thrpt:  [9.0204 Melem/s 9.1150 Melem/s 9.1920 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Warming up for 3.0000 s
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Collecting 10 samples in estimated 5.2874 s (770 iterations)
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Analyzing
lsm/apply_batch/inserts_10k_deletes_2.5k/10000
                        time:   [3.2208 ms 3.2670 ms 3.3650 ms]
                        thrpt:  [2.9718 Melem/s 3.0609 Melem/s 3.1049 Melem/s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking lsm/snapshot/save_10k
Benchmarking lsm/snapshot/save_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/save_10k: Collecting 10 samples in estimated 5.0441 s (1265 iterations)
Benchmarking lsm/snapshot/save_10k: Analyzing
lsm/snapshot/save_10k   time:   [366.28 µs 373.24 µs 378.63 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking lsm/snapshot/load_10k
Benchmarking lsm/snapshot/load_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/load_10k: Collecting 10 samples in estimated 5.1771 s (1045 iterations)
Benchmarking lsm/snapshot/load_10k: Analyzing
lsm/snapshot/load_10k   time:   [1.2133 ms 1.2612 ms 1.3249 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
  1 (10.00%) high severe


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
Benchmarking mempool/add/txs/1000: Collecting 100 samples in estimated 8.0838 s (10k iterations)
Benchmarking mempool/add/txs/1000: Analyzing
mempool/add/txs/1000    time:   [762.94 µs 766.03 µs 769.55 µs]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking mempool/add/txs/5000
Benchmarking mempool/add/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/5000: Collecting 100 samples in estimated 5.0741 s (400 iterations)
Benchmarking mempool/add/txs/5000: Analyzing
mempool/add/txs/5000    time:   [10.132 ms 10.606 ms 11.066 ms]
Benchmarking mempool/add/txs/10000
Benchmarking mempool/add/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/10000: Collecting 100 samples in estimated 5.3474 s (200 iterations)
Benchmarking mempool/add/txs/10000: Analyzing
mempool/add/txs/10000   time:   [20.895 ms 22.087 ms 23.274 ms]

Benchmarking mempool/remove/txs/1000
Benchmarking mempool/remove/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.1s, enable flat sampling, or reduce sample count to 60.
Benchmarking mempool/remove/txs/1000: Collecting 100 samples in estimated 6.1432 s (5050 iterations)
Benchmarking mempool/remove/txs/1000: Analyzing
mempool/remove/txs/1000 time:   [481.51 µs 482.52 µs 483.56 µs]
Benchmarking mempool/remove/txs/5000
Benchmarking mempool/remove/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/5000: Collecting 100 samples in estimated 5.8319 s (700 iterations)
Benchmarking mempool/remove/txs/5000: Analyzing
mempool/remove/txs/5000 time:   [3.3612 ms 3.3921 ms 3.4230 ms]
Benchmarking mempool/remove/txs/10000
Benchmarking mempool/remove/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/10000: Collecting 100 samples in estimated 6.1276 s (300 iterations)
Benchmarking mempool/remove/txs/10000: Analyzing
mempool/remove/txs/10000
                        time:   [7.7108 ms 7.8235 ms 7.9422 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mempool/get_sorted/by_fee_density/1000
Benchmarking mempool/get_sorted/by_fee_density/1000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/1000: Collecting 100 samples in estimated 5.4113 s (66k iterations)
Benchmarking mempool/get_sorted/by_fee_density/1000: Analyzing
mempool/get_sorted/by_fee_density/1000
                        time:   [80.074 µs 80.330 µs 80.602 µs]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/get_sorted/by_fee_density/5000
Benchmarking mempool/get_sorted/by_fee_density/5000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/5000: Collecting 100 samples in estimated 5.1904 s (56k iterations)
Benchmarking mempool/get_sorted/by_fee_density/5000: Analyzing
mempool/get_sorted/by_fee_density/5000
                        time:   [92.026 µs 92.309 µs 92.619 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/get_sorted/by_fee_density/10000
Benchmarking mempool/get_sorted/by_fee_density/10000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/10000: Collecting 100 samples in estimated 5.0782 s (56k iterations)
Benchmarking mempool/get_sorted/by_fee_density/10000: Analyzing
mempool/get_sorted/by_fee_density/10000
                        time:   [90.271 µs 90.594 µs 90.956 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

Benchmarking mempool/drain_readd/txs/1000
Benchmarking mempool/drain_readd/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.4s, enable flat sampling, or reduce sample count to 50.
Benchmarking mempool/drain_readd/txs/1000: Collecting 100 samples in estimated 7.4247 s (5050 iterations)
Benchmarking mempool/drain_readd/txs/1000: Analyzing
mempool/drain_readd/txs/1000
                        time:   [772.55 µs 774.13 µs 775.65 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/drain_readd/txs/5000
Benchmarking mempool/drain_readd/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/5000: Collecting 100 samples in estimated 5.6473 s (500 iterations)
Benchmarking mempool/drain_readd/txs/5000: Analyzing
mempool/drain_readd/txs/5000
                        time:   [6.4220 ms 6.5037 ms 6.5817 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking mempool/drain_readd/txs/10000
Benchmarking mempool/drain_readd/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/10000: Collecting 100 samples in estimated 8.5701 s (200 iterations)
Benchmarking mempool/drain_readd/txs/10000: Analyzing
mempool/drain_readd/txs/10000
                        time:   [23.300 ms 23.611 ms 23.919 ms]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high severe

Benchmarking mempool/batch_remove/50_from/5000
Benchmarking mempool/batch_remove/50_from/5000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/5000: Collecting 100 samples in estimated 5.0073 s (3.3M iterations)
Benchmarking mempool/batch_remove/50_from/5000: Analyzing
mempool/batch_remove/50_from/5000
                        time:   [1.5274 µs 1.5283 µs 1.5293 µs]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe
Benchmarking mempool/batch_remove/50_from/10000
Benchmarking mempool/batch_remove/50_from/10000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/10000: Collecting 100 samples in estimated 5.0022 s (3.4M iterations)
Benchmarking mempool/batch_remove/50_from/10000: Analyzing
mempool/batch_remove/50_from/10000
                        time:   [1.4860 µs 1.4876 µs 1.4899 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe


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
Benchmarking ed25519_verify/single: Collecting 100 samples in estimated 5.0608 s (172k iterations)
Benchmarking ed25519_verify/single: Analyzing
ed25519_verify/single   time:   [28.961 µs 29.036 µs 29.126 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe

Benchmarking ed25519_batch_verify/sequential/1
Benchmarking ed25519_batch_verify/sequential/1: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/1: Collecting 100 samples in estimated 5.0268 s (172k iterations)
Benchmarking ed25519_batch_verify/sequential/1: Analyzing
ed25519_batch_verify/sequential/1
                        time:   [28.884 µs 29.097 µs 29.412 µs]
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking ed25519_batch_verify/sequential/10
Benchmarking ed25519_batch_verify/sequential/10: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/10: Collecting 100 samples in estimated 5.8451 s (20k iterations)
Benchmarking ed25519_batch_verify/sequential/10: Analyzing
ed25519_batch_verify/sequential/10
                        time:   [286.61 µs 290.36 µs 294.99 µs]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  6 (6.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking ed25519_batch_verify/sequential/50
Benchmarking ed25519_batch_verify/sequential/50: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.3s, enable flat sampling, or reduce sample count to 50.
Benchmarking ed25519_batch_verify/sequential/50: Collecting 100 samples in estimated 7.2546 s (5050 iterations)
Benchmarking ed25519_batch_verify/sequential/50: Analyzing
ed25519_batch_verify/sequential/50
                        time:   [1.4103 ms 1.4182 ms 1.4267 ms]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking ed25519_batch_verify/sequential/100
Benchmarking ed25519_batch_verify/sequential/100: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/100: Collecting 100 samples in estimated 5.1791 s (1800 iterations)
Benchmarking ed25519_batch_verify/sequential/100: Analyzing
ed25519_batch_verify/sequential/100
                        time:   [2.8365 ms 2.8456 ms 2.8543 ms]
Benchmarking ed25519_batch_verify/sequential/200
Benchmarking ed25519_batch_verify/sequential/200: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/200: Collecting 100 samples in estimated 5.1355 s (900 iterations)
Benchmarking ed25519_batch_verify/sequential/200: Analyzing
ed25519_batch_verify/sequential/200
                        time:   [5.6638 ms 5.7084 ms 5.7676 ms]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking ed25519_batch_verify/sequential/500
Benchmarking ed25519_batch_verify/sequential/500: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/500: Collecting 100 samples in estimated 5.9587 s (400 iterations)
Benchmarking ed25519_batch_verify/sequential/500: Analyzing
ed25519_batch_verify/sequential/500
                        time:   [14.605 ms 14.726 ms 14.889 ms]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe

Benchmarking keyhash_from_vkey/single
Benchmarking keyhash_from_vkey/single: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/single: Collecting 100 samples in estimated 5.0001 s (29M iterations)
Benchmarking keyhash_from_vkey/single: Analyzing
keyhash_from_vkey/single
                        time:   [170.89 ns 172.31 ns 174.02 ns]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking keyhash_from_vkey/batch/10
Benchmarking keyhash_from_vkey/batch/10: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/10: Collecting 100 samples in estimated 5.0077 s (3.1M iterations)
Benchmarking keyhash_from_vkey/batch/10: Analyzing
keyhash_from_vkey/batch/10
                        time:   [1.6291 µs 1.6436 µs 1.6677 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/50
Benchmarking keyhash_from_vkey/batch/50: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/50: Collecting 100 samples in estimated 5.0293 s (616k iterations)
Benchmarking keyhash_from_vkey/batch/50: Analyzing
keyhash_from_vkey/batch/50
                        time:   [8.2602 µs 8.3588 µs 8.4904 µs]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking keyhash_from_vkey/batch/100
Benchmarking keyhash_from_vkey/batch/100: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/100: Collecting 100 samples in estimated 5.0706 s (308k iterations)
Benchmarking keyhash_from_vkey/batch/100: Analyzing
keyhash_from_vkey/batch/100
                        time:   [16.454 µs 16.628 µs 16.863 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/200
Benchmarking keyhash_from_vkey/batch/200: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/200: Collecting 100 samples in estimated 5.1652 s (152k iterations)
Benchmarking keyhash_from_vkey/batch/200: Analyzing
keyhash_from_vkey/batch/200
                        time:   [33.949 µs 34.113 µs 34.338 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  3 (3.00%) high severe
Benchmarking keyhash_from_vkey/batch/500
Benchmarking keyhash_from_vkey/batch/500: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/500: Collecting 100 samples in estimated 5.1226 s (61k iterations)
Benchmarking keyhash_from_vkey/batch/500: Analyzing
keyhash_from_vkey/batch/500
                        time:   [82.740 µs 83.324 µs 84.061 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe

Benchmarking vrf_verify/single_proof
Benchmarking vrf_verify/single_proof: Warming up for 3.0000 s
Benchmarking vrf_verify/single_proof: Collecting 100 samples in estimated 5.4461 s (61k iterations)
Benchmarking vrf_verify/single_proof: Analyzing
vrf_verify/single_proof time:   [89.801 µs 90.651 µs 91.754 µs]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
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
Benchmarking blake2b_256/hash/32B_txhash: Collecting 100 samples in estimated 5.0000 s (32M iterations)
Benchmarking blake2b_256/hash/32B_txhash: Analyzing
blake2b_256/hash/32B_txhash
                        time:   [157.72 ns 158.25 ns 158.76 ns]
Benchmarking blake2b_256/hash/64B_vkey
Benchmarking blake2b_256/hash/64B_vkey: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/64B_vkey: Collecting 100 samples in estimated 5.0000 s (32M iterations)
Benchmarking blake2b_256/hash/64B_vkey: Analyzing
blake2b_256/hash/64B_vkey
                        time:   [158.21 ns 158.78 ns 159.33 ns]
Benchmarking blake2b_256/hash/256B_small_tx
Benchmarking blake2b_256/hash/256B_small_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/256B_small_tx: Collecting 100 samples in estimated 5.0010 s (18M iterations)
Benchmarking blake2b_256/hash/256B_small_tx: Analyzing
blake2b_256/hash/256B_small_tx
                        time:   [279.20 ns 279.90 ns 280.60 ns]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_256/hash/500B_avg_tx
Benchmarking blake2b_256/hash/500B_avg_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/500B_avg_tx: Collecting 100 samples in estimated 5.0021 s (9.3M iterations)
Benchmarking blake2b_256/hash/500B_avg_tx: Analyzing
blake2b_256/hash/500B_avg_tx
                        time:   [537.14 ns 537.72 ns 538.27 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_256/hash/1KB_tx_body
Benchmarking blake2b_256/hash/1KB_tx_body: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/1KB_tx_body: Collecting 100 samples in estimated 5.0003 s (4.8M iterations)
Benchmarking blake2b_256/hash/1KB_tx_body: Analyzing
blake2b_256/hash/1KB_tx_body
                        time:   [1.0505 µs 1.0508 µs 1.0511 µs]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low severe
  12 (12.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/4KB_large_tx
Benchmarking blake2b_256/hash/4KB_large_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/4KB_large_tx: Collecting 100 samples in estimated 5.0031 s (1.2M iterations)
Benchmarking blake2b_256/hash/4KB_large_tx: Analyzing
blake2b_256/hash/4KB_large_tx
                        time:   [4.1234 µs 4.1244 µs 4.1254 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/16KB_block_header
Benchmarking blake2b_256/hash/16KB_block_header: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/16KB_block_header: Collecting 100 samples in estimated 5.0590 s (308k iterations)
Benchmarking blake2b_256/hash/16KB_block_header: Analyzing
blake2b_256/hash/16KB_block_header
                        time:   [16.419 µs 16.423 µs 16.428 µs]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking blake2b_256/hash/20KB_avg_block
Benchmarking blake2b_256/hash/20KB_avg_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/20KB_avg_block: Collecting 100 samples in estimated 5.0762 s (247k iterations)
Benchmarking blake2b_256/hash/20KB_avg_block: Analyzing
blake2b_256/hash/20KB_avg_block
                        time:   [20.516 µs 20.519 µs 20.522 µs]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking blake2b_256/hash/90KB_max_block
Benchmarking blake2b_256/hash/90KB_max_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/90KB_max_block: Collecting 100 samples in estimated 5.1261 s (56k iterations)
Benchmarking blake2b_256/hash/90KB_max_block: Analyzing
blake2b_256/hash/90KB_max_block
                        time:   [92.251 µs 92.264 µs 92.279 µs]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe

Benchmarking blake2b_224/hash/32B_vkey_to_keyhash
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Collecting 100 samples in estimated 5.0007 s (30M iterations)
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Analyzing
blake2b_224/hash/32B_vkey_to_keyhash
                        time:   [166.77 ns 167.47 ns 168.11 ns]
Benchmarking blake2b_224/hash/64B_script_bytes
Benchmarking blake2b_224/hash/64B_script_bytes: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/64B_script_bytes: Collecting 100 samples in estimated 5.0003 s (30M iterations)
Benchmarking blake2b_224/hash/64B_script_bytes: Analyzing
blake2b_224/hash/64B_script_bytes
                        time:   [165.94 ns 166.21 ns 166.44 ns]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) low mild
Benchmarking blake2b_224/hash/256B_address_payload
Benchmarking blake2b_224/hash/256B_address_payload: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/256B_address_payload: Collecting 100 samples in estimated 5.0007 s (18M iterations)
Benchmarking blake2b_224/hash/256B_address_payload: Analyzing
blake2b_224/hash/256B_address_payload
                        time:   [285.08 ns 285.34 ns 285.61 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking blake2b_batch/224_keyhashes/10
Benchmarking blake2b_batch/224_keyhashes/10: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/10: Collecting 100 samples in estimated 5.0073 s (3.2M iterations)
Benchmarking blake2b_batch/224_keyhashes/10: Analyzing
blake2b_batch/224_keyhashes/10
                        time:   [1.5659 µs 1.5674 µs 1.5690 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/50
Benchmarking blake2b_batch/224_keyhashes/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/50: Collecting 100 samples in estimated 5.0256 s (636k iterations)
Benchmarking blake2b_batch/224_keyhashes/50: Analyzing
blake2b_batch/224_keyhashes/50
                        time:   [7.8306 µs 7.8385 µs 7.8470 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/100
Benchmarking blake2b_batch/224_keyhashes/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/100: Collecting 100 samples in estimated 5.0283 s (318k iterations)
Benchmarking blake2b_batch/224_keyhashes/100: Analyzing
blake2b_batch/224_keyhashes/100
                        time:   [15.679 µs 15.692 µs 15.706 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/500
Benchmarking blake2b_batch/224_keyhashes/500: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/500: Collecting 100 samples in estimated 5.2094 s (66k iterations)
Benchmarking blake2b_batch/224_keyhashes/500: Analyzing
blake2b_batch/224_keyhashes/500
                        time:   [78.478 µs 78.560 µs 78.657 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/50
Benchmarking blake2b_batch/256_txbodies_500B/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/50: Collecting 100 samples in estimated 5.1166 s (192k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/50: Analyzing
blake2b_batch/256_txbodies_500B/50
                        time:   [26.646 µs 26.660 µs 26.676 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/100
Benchmarking blake2b_batch/256_txbodies_500B/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/100: Collecting 100 samples in estimated 5.1181 s (96k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/100: Analyzing
blake2b_batch/256_txbodies_500B/100
                        time:   [53.338 µs 53.367 µs 53.401 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/300
Benchmarking blake2b_batch/256_txbodies_500B/300: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/300: Collecting 100 samples in estimated 5.6599 s (35k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/300: Analyzing
blake2b_batch/256_txbodies_500B/300
                        time:   [160.31 µs 160.40 µs 160.50 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe


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
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Collecting 100 samples in estimated 5.0050 s (2.8M iterations)
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Analyzing
serialization/encode_transaction/conway_2in_2out_2wit
                        time:   [1.8235 µs 1.8255 µs 1.8288 µs]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking serialization/encode_transaction/body_only_2in_2out
Benchmarking serialization/encode_transaction/body_only_2in_2out: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/body_only_2in_2out: Collecting 100 samples in estimated 5.0010 s (3.7M iterations)
Benchmarking serialization/encode_transaction/body_only_2in_2out: Analyzing
serialization/encode_transaction/body_only_2in_2out
                        time:   [1.3260 µs 1.3266 µs 1.3273 µs]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

Benchmarking serialization/encode_block_header/with_vrf_output
Benchmarking serialization/encode_block_header/with_vrf_output: Warming up for 3.0000 s
Benchmarking serialization/encode_block_header/with_vrf_output: Collecting 100 samples in estimated 5.0036 s (4.0M iterations)
Benchmarking serialization/encode_block_header/with_vrf_output: Analyzing
serialization/encode_block_header/with_vrf_output
                        time:   [1.2789 µs 1.2886 µs 1.3000 µs]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe

Benchmarking serialization/encode_value/ada_only
Benchmarking serialization/encode_value/ada_only: Warming up for 3.0000 s
Benchmarking serialization/encode_value/ada_only: Collecting 100 samples in estimated 5.0001 s (231M iterations)
Benchmarking serialization/encode_value/ada_only: Analyzing
serialization/encode_value/ada_only
                        time:   [21.094 ns 21.151 ns 21.224 ns]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking serialization/encode_value/multi_asset_3policy_5asset
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Warming up for 3.0000 s
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Collecting 100 samples in estimated 5.0004 s (6.5M iterations)
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Analyzing
serialization/encode_value/multi_asset_3policy_5asset
                        time:   [744.82 ns 755.30 ns 767.73 ns]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe


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
[test_mainnet_scale_insert_read] inserting 100000 entries...
[test_mainnet_scale_wal_crash_recovery] writing 100000 entries (WAL only)...
[test_mainnet_scale_wal_crash_recovery] simulating crash (drop without flush)...
[test_mainnet_scale_delete_amplification] insert flush complete
[test_mainnet_scale_delete_amplification] deleting 80000 entries...
[test_mainnet_scale_insert_read] flush complete, sampling 1000 keys...
[test_mainnet_scale_insert_read] verified 1000/1000 sampled keys — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
[test_mainnet_scale_wal_crash_recovery] reopened, verifying 100000 entries...
[test_mainnet_scale_wal_crash_recovery] all 100000 entries recovered — PASS
[test_mainnet_scale_delete_amplification] delete flush complete
[test_mainnet_scale_delete_amplification] verifying surviving 20000 entries...
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
[test_mainnet_scale_delete_amplification] verifying 1K deleted keys return None...
[test_mainnet_scale_delete_amplification] range scanning for exact count...
[test_mainnet_scale_delete_amplification] 20000 entries confirmed — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out; finished in 0.57s

   Doc-tests dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s


```

</details>

