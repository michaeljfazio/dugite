# Nightly Benchmark Results — 2026-10-26

Captured on **2026-10-26** from commit [`a8ba00f`](https://github.com/MichaelJFazio/dugite/commit/a8ba00f) on `main` (GitHub Actions `ubuntu-latest`).

### Measurement environment

| | |
|---|---|
| Runner | GitHub Actions `ubuntu-latest` (shared, virtualised) |
| CPU | AMD EPYC 9V74 80-Core Processor |
| vCPUs | 4 |
| Memory | 15.6 GiB |
| Kernel | Linux 6.17.0-1022-azure |
| Toolchain | rustc 1.97.0 (2d8144b78 2026-07-07) |
| Build profile | `bench` (release + debug assertions off) |

> **Read absolute numbers with care.** These run on shared, virtualised GitHub-hosted runners whose CPU model is not pinned and whose neighbours are not controlled. Treat the figures as an order-of-magnitude regression tripwire, not as hardware benchmarks: a `change: ±x%` between two dates can reflect a different host class rather than a code change. Use the interactive trend lines below, where a real regression shows as a sustained step rather than a single-day spike.

> **Not measured here:** end-to-end sync throughput, mainnet-scale UTxO memory, and anything requiring a live network. Those are covered by the devnet-validate and soak rigs, not by Criterion.

> **Interactive reports**, including per-benchmark detail pages and historical trend lines, are published at <https://michaeljfazio.github.io/dugite/benchmarks/>. Each section below also links directly to its interactive report.

> The collapsed _Raw measurements_ blocks contain the filtered `cargo bench` output (cargo build chatter and ANSI escapes are stripped). Full unfiltered logs are uploaded as the `benchmark-results-235` workflow artifact.

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

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.6s or enable flat sampling.
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 8.6201 s (55 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [151.32 ms 154.74 ms 157.86 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 6.1209 s (15k iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [400.37 µs 401.76 µs 403.25 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 5.1723 s (600 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [8.3399 ms 8.4238 ms 8.5086 ms]

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 5.0000 s (17B iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [292.07 ps 292.84 ps 293.67 ps]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 5.0858 s (273k iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [18.329 µs 18.432 µs 18.542 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 5.0008 s (23M iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [219.86 ns 220.79 ns 221.75 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 6.5963 s (220 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [4.2771 ms 4.3727 ms 4.5282 ms]

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.2s or enable flat sampling.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 8.2343 s (55 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [147.74 ms 148.91 ms 150.28 ms]
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.3s or enable flat sampling.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 8.2905 s (55 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [147.20 ms 148.22 ms 149.71 ms]
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.1s or enable flat sampling.
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 8.1376 s (55 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [22.517 ms 22.715 ms 23.011 ms]
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.4s or enable flat sampling.
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 8.4373 s (55 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [24.053 ms 24.295 ms 24.602 ms]

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.1s, or reduce sample count to 90.
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 5.1272 s (100 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [50.126 ms 51.296 ms 53.419 ms]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 9.8799 s (200 iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [48.690 ms 48.855 ms 49.029 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 9.9264 s (200 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [49.538 ms 52.062 ms 55.384 ms]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 71.2s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 71.156 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [525.07 ms 541.56 ms 561.40 ms]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 51.7s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 51.675 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [515.27 ms 516.18 ms 517.11 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 51.9s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 51.944 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [518.17 ms 519.05 ms 519.95 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.4117 s (800 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [6.7887 ms 6.8054 ms 6.8225 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.3975 s (800 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [6.6658 ms 6.6951 ms 6.7301 ms]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0561 s (182k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [27.067 µs 27.284 µs 27.507 µs]
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0758 s (182k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [27.341 µs 27.503 µs 27.673 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 5.0134 s (600 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [8.3402 ms 8.3798 ms 8.4219 ms]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.5666 s (600 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [9.0297 ms 9.1053 ms 9.1879 ms]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.6853 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [366.43 µs 368.94 µs 371.29 µs]
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 5.7639 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [371.73 µs 373.69 µs 375.34 µs]

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 6.0934 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [605.70 µs 608.44 µs 611.38 µs]
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) low severe
  5 (5.00%) low mild
  3 (3.00%) high mild
  5 (5.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.0047 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.2287 ms 6.2851 ms 6.3445 ms]
Found 41 outliers among 100 measurements (41.00%)
  9 (9.00%) low severe
  10 (10.00%) low mild
  6 (6.00%) high mild
  16 (16.00%) high severe
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.2443 s (1900 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [2.7282 ms 2.7342 ms 2.7398 ms]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low severe
  9 (9.00%) low mild
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.2s, or reduce sample count to 90.
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 5.2244 s (100 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [51.463 ms 52.198 ms 52.973 ms]
Found 23 outliers among 100 measurements (23.00%)
  1 (1.00%) low severe
  22 (22.00%) high severe
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.5396 s (1000 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [5.6090 ms 5.6203 ms 5.6315 ms]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 11.0s, or reduce sample count to 40.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 11.023 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [110.17 ms 110.80 ms 111.57 ms]
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  9 (9.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0087 s (414k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [12.043 µs 12.132 µs 12.235 µs]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0213 s (217k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [22.442 µs 22.589 µs 22.733 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0098 s (394k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [12.037 µs 12.141 µs 12.266 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0048 s (288k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [17.069 µs 17.163 µs 17.259 µs]
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0179 s (394k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [12.601 µs 12.652 µs 12.701 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0562 s (293k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [17.014 µs 17.072 µs 17.127 µs]
Found 21 outliers among 100 measurements (21.00%)
  4 (4.00%) low severe
  2 (2.00%) low mild
  10 (10.00%) high mild
  5 (5.00%) high severe

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0169 s (500k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [9.9857 µs 10.002 µs 10.017 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.0770 s (157k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [40.362 µs 42.182 µs 43.763 µs]

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0258 s (8360 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [602.07 µs 605.09 µs 606.99 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.1896 s (825 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.2612 ms 6.3029 ms 6.3402 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.0758 s (1870 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [2.6785 ms 2.7020 ms 2.7239 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 6.0326 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [51.988 ms 52.400 ms 53.089 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  2 (20.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.1856 s (935 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [5.5146 ms 5.5225 ms 5.5284 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.1s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 6.0549 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [109.97 ms 110.00 ms 110.02 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0006 s (415k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [11.974 µs 12.056 µs 12.162 µs]
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0009 s (215k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [23.367 µs 23.574 µs 23.718 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0002 s (394k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [12.413 µs 12.546 µs 12.660 µs]
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0006 s (283k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [17.809 µs 17.855 µs 17.875 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0001 s (390k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [12.728 µs 12.753 µs 12.803 µs]
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0007 s (292k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [17.216 µs 17.261 µs 17.323 µs]

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.5967 s (110 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [50.427 ms 50.513 ms 50.668 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.4855 s (110 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [50.704 ms 51.165 ms 52.234 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 5.2923 s (20 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [258.56 ms 259.98 ms 260.92 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.2159 s (20 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [254.93 ms 256.93 ms 258.68 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.6s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 6.5611 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [511.36 ms 513.97 ms 516.44 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.1s.
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 5.1446 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [516.30 ms 520.03 ms 524.99 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.5s or enable flat sampling.
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 8.5074 s (55 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [152.75 ms 153.30 ms 153.94 ms]
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

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.1s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 26.133 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.5709 s 2.5883 s 2.6076 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 5.3640 s (15k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [366.19 µs 368.88 µs 371.20 µs]
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.0636 s (30k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [166.98 µs 167.10 µs 167.24 µs]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) low mild
  6 (6.00%) high mild
  3 (3.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 6.2027 s (25k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [244.57 µs 244.75 µs 244.95 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.2642 s (30k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [172.74 µs 173.72 µs 175.38 µs]
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 48.6s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 48.644 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.5926 s 2.6050 s 2.6197 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.9s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 26.871 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [370.98 ms 377.35 ms 382.80 ms]
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.9s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 26.923 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [369.37 ms 374.45 ms 379.55 ms]

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 34.2s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 34.240 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [3.2296 s 3.2397 s 3.2511 s]
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 32.6s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 32.584 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [116.40 ms 133.11 ms 160.24 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 23.1s, or reduce sample count to 20.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 23.057 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [228.44 ms 229.28 ms 230.31 ms]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 34.6s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 34.634 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [440.47 ms 443.64 ms 447.15 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.6s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 25.561 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.5640 s 2.5919 s 2.6188 s]
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.0s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 25.991 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.5709 s 2.5825 s 2.5933 s]
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.8s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 25.773 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.5545 s 2.5701 s 2.5859 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.4s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 25.427 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.5296 s 2.5396 s 2.5501 s]
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.0s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 25.001 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.5406 s 2.5534 s 2.5672 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 5.3822 s (20k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [265.71 µs 266.70 µs 267.78 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 5.3136 s (20k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [267.52 µs 268.45 µs 269.38 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 5.4308 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [263.96 µs 264.57 µs 265.31 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 5.3093 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [261.00 µs 262.56 µs 265.01 µs]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 5.3129 s (20k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [259.85 µs 262.12 µs 265.32 µs]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 6.0228 s (30 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [200.49 ms 202.15 ms 204.36 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 12.0s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 11.962 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [1.1644 s 1.1736 s 1.1829 s]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.5s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 25.531 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.5385 s 2.5543 s 2.5711 s]

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0101 s (22k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [213.87 µs 214.06 µs 214.30 µs]
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0100 s (19k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [249.73 µs 252.36 µs 255.35 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0018 s (18k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [265.66 µs 269.38 µs 272.30 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 6.8889 s (30 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [35.749 ms 38.087 ms 40.219 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.4s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 13.351 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [177.92 ms 189.60 ms 201.20 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.7s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 28.679 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [361.51 ms 388.55 ms 416.15 ms]

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.9870 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [24.326 ms 25.135 ms 25.914 ms]
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.9s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 6.9203 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [112.90 ms 116.36 ms 119.74 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 6.1161 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [262.70 ms 277.74 ms 291.25 ms]

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 178.2s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 178.18 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [15.193 s 15.735 s 16.388 s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 341.0s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 341.00 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [32.595 s 32.729 s 32.862 s]

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0551 s (4840 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.0402 ms 1.0426 ms 1.0449 ms]
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0367 s (3685 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.3447 ms 1.3538 ms 1.3732 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 12.8s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 12.809 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.2844 s 1.2898 s 1.2972 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.0s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 25.030 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [2.5014 s 2.5131 s 2.5261 s]

Benchmarking ledger/apply_block/apply_only_shelley_50tx
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Collecting 20 samples in estimated 5.0032 s (78k iterations)
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Analyzing
ledger/apply_block/apply_only_shelley_50tx
                        time:   [62.678 µs 62.968 µs 63.495 µs]
Found 5 outliers among 20 measurements (25.00%)
  3 (15.00%) low mild
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking ledger/apply_block/validate_all_shelley_50tx
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Collecting 20 samples in estimated 5.0217 s (44k iterations)
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Analyzing
ledger/apply_block/validate_all_shelley_50tx
                        time:   [113.45 µs 113.92 µs 114.55 µs]
Found 5 outliers among 20 measurements (25.00%)
  3 (15.00%) low severe
  2 (10.00%) high mild


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
Benchmarking network/handshake_encode/n2n_version_data: Collecting 100 samples in estimated 5.0001 s (230M iterations)
Benchmarking network/handshake_encode/n2n_version_data: Analyzing
network/handshake_encode/n2n_version_data
                        time:   [21.362 ns 21.450 ns 21.551 ns]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
Benchmarking network/handshake_encode/n2c_version_data
Benchmarking network/handshake_encode/n2c_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2c_version_data: Collecting 100 samples in estimated 5.0000 s (253M iterations)
Benchmarking network/handshake_encode/n2c_version_data: Analyzing
network/handshake_encode/n2c_version_data
                        time:   [19.439 ns 19.566 ns 19.719 ns]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking network/chainsync/roll_forward/encode/256
Benchmarking network/chainsync/roll_forward/encode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/256: Collecting 100 samples in estimated 5.0003 s (62M iterations)
Benchmarking network/chainsync/roll_forward/encode/256: Analyzing
network/chainsync/roll_forward/encode/256
                        time:   [81.925 ns 82.443 ns 82.944 ns]
                        thrpt:  [3.4920 GiB/s 3.5132 GiB/s 3.5354 GiB/s]
Benchmarking network/chainsync/roll_forward/decode/256
Benchmarking network/chainsync/roll_forward/decode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/256: Collecting 100 samples in estimated 5.0001 s (90M iterations)
Benchmarking network/chainsync/roll_forward/decode/256: Analyzing
network/chainsync/roll_forward/decode/256
                        time:   [55.315 ns 55.507 ns 55.738 ns]
                        thrpt:  [5.1965 GiB/s 5.2181 GiB/s 5.2362 GiB/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/1024
Benchmarking network/chainsync/roll_forward/encode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/1024: Collecting 100 samples in estimated 5.0003 s (44M iterations)
Benchmarking network/chainsync/roll_forward/encode/1024: Analyzing
network/chainsync/roll_forward/encode/1024
                        time:   [109.35 ns 109.92 ns 110.53 ns]
                        thrpt:  [9.0913 GiB/s 9.1418 GiB/s 9.1897 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/1024
Benchmarking network/chainsync/roll_forward/decode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/1024: Collecting 100 samples in estimated 5.0001 s (84M iterations)
Benchmarking network/chainsync/roll_forward/decode/1024: Analyzing
network/chainsync/roll_forward/decode/1024
                        time:   [59.544 ns 59.744 ns 59.976 ns]
                        thrpt:  [16.755 GiB/s 16.820 GiB/s 16.877 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/4096
Benchmarking network/chainsync/roll_forward/encode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/4096: Collecting 100 samples in estimated 5.0003 s (32M iterations)
Benchmarking network/chainsync/roll_forward/encode/4096: Analyzing
network/chainsync/roll_forward/encode/4096
                        time:   [153.05 ns 153.49 ns 154.00 ns]
                        thrpt:  [25.103 GiB/s 25.186 GiB/s 25.258 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/4096
Benchmarking network/chainsync/roll_forward/decode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/4096: Collecting 100 samples in estimated 5.0000 s (42M iterations)
Benchmarking network/chainsync/roll_forward/decode/4096: Analyzing
network/chainsync/roll_forward/decode/4096
                        time:   [116.37 ns 117.57 ns 119.36 ns]
                        thrpt:  [32.389 GiB/s 32.881 GiB/s 33.221 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking network/chainsync/roll_backward/encode
Benchmarking network/chainsync/roll_backward/encode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/encode: Collecting 100 samples in estimated 5.0001 s (41M iterations)
Benchmarking network/chainsync/roll_backward/encode: Analyzing
network/chainsync/roll_backward/encode
                        time:   [123.50 ns 124.32 ns 125.47 ns]
                        thrpt:  [668.86 MiB/s 675.04 MiB/s 679.57 MiB/s]
Found 24 outliers among 100 measurements (24.00%)
  15 (15.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
Benchmarking network/chainsync/roll_backward/decode
Benchmarking network/chainsync/roll_backward/decode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/decode: Collecting 100 samples in estimated 5.0001 s (139M iterations)
Benchmarking network/chainsync/roll_backward/decode: Analyzing
network/chainsync/roll_backward/decode
                        time:   [35.618 ns 35.774 ns 35.956 ns]
                        thrpt:  [2.2794 GiB/s 2.2909 GiB/s 2.3010 GiB/s]
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe

Benchmarking network/blockfetch/msg_block/encode/2048
Benchmarking network/blockfetch/msg_block/encode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/2048: Collecting 100 samples in estimated 5.0003 s (79M iterations)
Benchmarking network/blockfetch/msg_block/encode/2048: Analyzing
network/blockfetch/msg_block/encode/2048
                        time:   [61.727 ns 62.012 ns 62.352 ns]
                        thrpt:  [30.665 GiB/s 30.833 GiB/s 30.975 GiB/s]
Found 11 outliers among 100 measurements (11.00%)
  8 (8.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/2048
Benchmarking network/blockfetch/msg_block/decode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/2048: Collecting 100 samples in estimated 5.0003 s (80M iterations)
Benchmarking network/blockfetch/msg_block/decode/2048: Analyzing
network/blockfetch/msg_block/decode/2048
                        time:   [62.394 ns 62.664 ns 62.953 ns]
                        thrpt:  [30.372 GiB/s 30.512 GiB/s 30.644 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking network/blockfetch/msg_block/encode/20480
Benchmarking network/blockfetch/msg_block/encode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/20480: Collecting 100 samples in estimated 5.0001 s (30M iterations)
Benchmarking network/blockfetch/msg_block/encode/20480: Analyzing
network/blockfetch/msg_block/encode/20480
                        time:   [165.97 ns 166.68 ns 167.49 ns]
                        thrpt:  [113.90 GiB/s 114.46 GiB/s 114.95 GiB/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/20480
Benchmarking network/blockfetch/msg_block/decode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/20480: Collecting 100 samples in estimated 5.0009 s (25M iterations)
Benchmarking network/blockfetch/msg_block/decode/20480: Analyzing
network/blockfetch/msg_block/decode/20480
                        time:   [196.38 ns 197.47 ns 198.87 ns]
                        thrpt:  [95.932 GiB/s 96.615 GiB/s 97.149 GiB/s]
Found 15 outliers among 100 measurements (15.00%)
  7 (7.00%) high mild
  8 (8.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/90000
Benchmarking network/blockfetch/msg_block/encode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/90000: Collecting 100 samples in estimated 5.0018 s (7.0M iterations)
Benchmarking network/blockfetch/msg_block/encode/90000: Analyzing
network/blockfetch/msg_block/encode/90000
                        time:   [713.34 ns 716.86 ns 720.50 ns]
                        thrpt:  [116.34 GiB/s 116.93 GiB/s 117.51 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/90000
Benchmarking network/blockfetch/msg_block/decode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/90000: Collecting 100 samples in estimated 5.0019 s (7.2M iterations)
Benchmarking network/blockfetch/msg_block/decode/90000: Analyzing
network/blockfetch/msg_block/decode/90000
                        time:   [687.39 ns 689.89 ns 692.73 ns]
                        thrpt:  [121.01 GiB/s 121.51 GiB/s 121.95 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking network/blockfetch/request_range/encode
Benchmarking network/blockfetch/request_range/encode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/encode: Collecting 100 samples in estimated 5.0004 s (59M iterations)
Benchmarking network/blockfetch/request_range/encode: Analyzing
network/blockfetch/request_range/encode
                        time:   [85.153 ns 85.865 ns 86.750 ns]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking network/blockfetch/request_range/decode
Benchmarking network/blockfetch/request_range/decode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/decode: Collecting 100 samples in estimated 5.0001 s (151M iterations)
Benchmarking network/blockfetch/request_range/decode: Analyzing
network/blockfetch/request_range/decode
                        time:   [32.737 ns 32.879 ns 33.043 ns]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking network/n2c_query_encode/pparams/hfc_success
Benchmarking network/n2c_query_encode/pparams/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/hfc_success: Collecting 100 samples in estimated 5.0001 s (113M iterations)
Benchmarking network/n2c_query_encode/pparams/hfc_success: Analyzing
network/n2c_query_encode/pparams/hfc_success
                        time:   [44.946 ns 45.122 ns 45.294 ns]
                        thrpt:  [1.8711 GiB/s 1.8782 GiB/s 1.8856 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking network/n2c_query_encode/pparams/tag24
Benchmarking network/n2c_query_encode/pparams/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/tag24: Collecting 100 samples in estimated 5.0002 s (109M iterations)
Benchmarking network/n2c_query_encode/pparams/tag24: Analyzing
network/n2c_query_encode/pparams/tag24
                        time:   [46.250 ns 46.463 ns 46.682 ns]
                        thrpt:  [1.8155 GiB/s 1.8240 GiB/s 1.8325 GiB/s]
Found 27 outliers among 100 measurements (27.00%)
  15 (15.00%) low severe
  5 (5.00%) high mild
  7 (7.00%) high severe
Benchmarking network/n2c_query_encode/govstate/hfc_success
Benchmarking network/n2c_query_encode/govstate/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/hfc_success: Collecting 100 samples in estimated 5.0003 s (82M iterations)
Benchmarking network/n2c_query_encode/govstate/hfc_success: Analyzing
network/n2c_query_encode/govstate/hfc_success
                        time:   [68.124 ns 68.609 ns 69.048 ns]
                        thrpt:  [10.548 GiB/s 10.615 GiB/s 10.691 GiB/s]
Benchmarking network/n2c_query_encode/govstate/tag24
Benchmarking network/n2c_query_encode/govstate/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/tag24: Collecting 100 samples in estimated 5.0001 s (81M iterations)
Benchmarking network/n2c_query_encode/govstate/tag24: Analyzing
network/n2c_query_encode/govstate/tag24
                        time:   [61.352 ns 61.518 ns 61.712 ns]
                        thrpt:  [11.802 GiB/s 11.839 GiB/s 11.871 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/era_mismatch
Benchmarking network/n2c_query_encode/era_mismatch: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/era_mismatch: Collecting 100 samples in estimated 5.0000 s (259M iterations)
Benchmarking network/n2c_query_encode/era_mismatch: Analyzing
network/n2c_query_encode/era_mismatch
                        time:   [19.260 ns 19.385 ns 19.550 ns]
                        thrpt:  [37.252 GiB/s 37.569 GiB/s 37.814 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
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
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Collecting 100 samples in estimated 5.1367 s (131k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Analyzing
consensus/vrf_leader_check/single/sigma=0.0000247
                        time:   [39.142 µs 39.187 µs 39.244 µs]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.001
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Collecting 100 samples in estimated 5.0423 s (126k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Analyzing
consensus/vrf_leader_check/single/sigma=0.001
                        time:   [40.037 µs 40.188 µs 40.489 µs]
Found 21 outliers among 100 measurements (21.00%)
  1 (1.00%) high mild
  20 (20.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.01
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Collecting 100 samples in estimated 5.1630 s (131k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Analyzing
consensus/vrf_leader_check/single/sigma=0.01
                        time:   [39.208 µs 39.245 µs 39.305 µs]
Found 21 outliers among 100 measurements (21.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  19 (19.00%) high severe
Benchmarking consensus/vrf_leader_check/batch/21600
Benchmarking consensus/vrf_leader_check/batch/21600: Warming up for 3.0000 s

Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 16.9s, or reduce sample count to 10.
Benchmarking consensus/vrf_leader_check/batch/21600: Collecting 20 samples in estimated 16.865 s (20 iterations)
Benchmarking consensus/vrf_leader_check/batch/21600: Analyzing
consensus/vrf_leader_check/batch/21600
                        time:   [843.02 ms 843.39 ms 843.80 ms]
                        thrpt:  [25.598 Kelem/s 25.611 Kelem/s 25.622 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild

Benchmarking consensus/validate_header/replay_mode
Benchmarking consensus/validate_header/replay_mode: Warming up for 3.0000 s
Benchmarking consensus/validate_header/replay_mode: Collecting 100 samples in estimated 5.0000 s (433M iterations)
Benchmarking consensus/validate_header/replay_mode: Analyzing
consensus/validate_header/replay_mode
                        time:   [11.534 ns 11.589 ns 11.663 ns]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe

Benchmarking consensus/chain_selection/longer_fork_100
Benchmarking consensus/chain_selection/longer_fork_100: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/longer_fork_100: Collecting 100 samples in estimated 5.0000 s (3.2B iterations)
Benchmarking consensus/chain_selection/longer_fork_100: Analyzing
consensus/chain_selection/longer_fork_100
                        time:   [1.5558 ns 1.5565 ns 1.5573 ns]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking consensus/chain_selection/equal_length_tiebreak
Benchmarking consensus/chain_selection/equal_length_tiebreak: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/equal_length_tiebreak: Collecting 100 samples in estimated 5.0007 s (15M iterations)
Benchmarking consensus/chain_selection/equal_length_tiebreak: Analyzing
consensus/chain_selection/equal_length_tiebreak
                        time:   [328.18 ns 328.61 ns 329.10 ns]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
Benchmarking consensus/chain_selection/prefer_simple
Benchmarking consensus/chain_selection/prefer_simple: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/prefer_simple: Collecting 100 samples in estimated 5.0000 s (2.1B iterations)
Benchmarking consensus/chain_selection/prefer_simple: Analyzing
consensus/chain_selection/prefer_simple
                        time:   [2.3989 ns 2.4017 ns 2.4044 ns]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe


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
Benchmarking lsm/insert/random_keys/10000: Collecting 10 samples in estimated 5.0446 s (1925 iterations)
Benchmarking lsm/insert/random_keys/10000: Analyzing
lsm/insert/random_keys/10000
                        time:   [2.5340 ms 2.5357 ms 2.5382 ms]
                        thrpt:  [3.9399 Melem/s 3.9436 Melem/s 3.9463 Melem/s]

Benchmarking lsm/point_lookup/hit_random/10000
Benchmarking lsm/point_lookup/hit_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/hit_random/10000: Collecting 100 samples in estimated 6.3988 s (20k iterations)
Benchmarking lsm/point_lookup/hit_random/10000: Analyzing
lsm/point_lookup/hit_random/10000
                        time:   [316.44 µs 317.52 µs 319.46 µs]
                        thrpt:  [3.1302 Melem/s 3.1494 Melem/s 3.1601 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking lsm/point_lookup/miss_random/10000
Benchmarking lsm/point_lookup/miss_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/miss_random/10000: Collecting 100 samples in estimated 5.1224 s (71k iterations)
Benchmarking lsm/point_lookup/miss_random/10000: Analyzing
lsm/point_lookup/miss_random/10000
                        time:   [72.157 µs 72.211 µs 72.274 µs]
                        thrpt:  [13.836 Melem/s 13.848 Melem/s 13.859 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

Benchmarking lsm/range_scan/window_100_of_10k/10000
Benchmarking lsm/range_scan/window_100_of_10k/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/window_100_of_10k/10000: Collecting 20 samples in estimated 5.0043 s (102k iterations)
Benchmarking lsm/range_scan/window_100_of_10k/10000: Analyzing
lsm/range_scan/window_100_of_10k/10000
                        time:   [48.506 µs 49.143 µs 50.300 µs]
                        thrpt:  [1.9881 Melem/s 2.0349 Melem/s 2.0616 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking lsm/range_scan/full_scan/10000
Benchmarking lsm/range_scan/full_scan/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/full_scan/10000: Collecting 20 samples in estimated 5.2549 s (3990 iterations)
Benchmarking lsm/range_scan/full_scan/10000: Analyzing
lsm/range_scan/full_scan/10000
                        time:   [1.2983 ms 1.3033 ms 1.3071 ms]
                        thrpt:  [7.6507 Melem/s 7.6730 Melem/s 7.7026 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  2 (10.00%) high mild
  2 (10.00%) high severe

Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Warming up for 3.0000 s
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Collecting 10 samples in estimated 5.2325 s (550 iterations)
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Analyzing
lsm/apply_batch/inserts_10k_deletes_2.5k/10000
                        time:   [4.2061 ms 4.2203 ms 4.2537 ms]
                        thrpt:  [2.3509 Melem/s 2.3695 Melem/s 2.3775 Melem/s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking lsm/snapshot/save_10k
Benchmarking lsm/snapshot/save_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/save_10k: Collecting 10 samples in estimated 5.0133 s (935 iterations)
Benchmarking lsm/snapshot/save_10k: Analyzing
lsm/snapshot/save_10k   time:   [421.73 µs 422.54 µs 423.58 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking lsm/snapshot/load_10k
Benchmarking lsm/snapshot/load_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/load_10k: Collecting 10 samples in estimated 5.2813 s (770 iterations)
Benchmarking lsm/snapshot/load_10k: Analyzing
lsm/snapshot/load_10k   time:   [1.3315 ms 1.3410 ms 1.3564 ms]
Found 1 outliers among 10 measurements (10.00%)
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
Benchmarking mempool/add/txs/1000: Collecting 100 samples in estimated 6.4817 s (10k iterations)
Benchmarking mempool/add/txs/1000: Analyzing
mempool/add/txs/1000    time:   [618.29 µs 619.73 µs 621.57 µs]
Found 14 outliers among 100 measurements (14.00%)
  7 (7.00%) high mild
  7 (7.00%) high severe
Benchmarking mempool/add/txs/5000
Benchmarking mempool/add/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/5000: Collecting 100 samples in estimated 5.0570 s (700 iterations)
Benchmarking mempool/add/txs/5000: Analyzing
mempool/add/txs/5000    time:   [6.7187 ms 6.9395 ms 7.1559 ms]
Benchmarking mempool/add/txs/10000
Benchmarking mempool/add/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/10000: Collecting 100 samples in estimated 5.8045 s (400 iterations)
Benchmarking mempool/add/txs/10000: Analyzing
mempool/add/txs/10000   time:   [15.574 ms 16.288 ms 17.005 ms]

Benchmarking mempool/remove/txs/1000
Benchmarking mempool/remove/txs/1000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/1000: Collecting 100 samples in estimated 9.9059 s (10k iterations)
Benchmarking mempool/remove/txs/1000: Analyzing
mempool/remove/txs/1000 time:   [407.96 µs 409.07 µs 410.41 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking mempool/remove/txs/5000
Benchmarking mempool/remove/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/5000: Collecting 100 samples in estimated 5.3222 s (1000 iterations)
Benchmarking mempool/remove/txs/5000: Analyzing
mempool/remove/txs/5000 time:   [2.3087 ms 2.3219 ms 2.3378 ms]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/remove/txs/10000
Benchmarking mempool/remove/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/10000: Collecting 100 samples in estimated 6.0671 s (400 iterations)
Benchmarking mempool/remove/txs/10000: Analyzing
mempool/remove/txs/10000
                        time:   [5.8012 ms 5.8825 ms 5.9667 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mempool/get_sorted/by_fee_density/1000
Benchmarking mempool/get_sorted/by_fee_density/1000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/1000: Collecting 100 samples in estimated 5.2167 s (86k iterations)
Benchmarking mempool/get_sorted/by_fee_density/1000: Analyzing
mempool/get_sorted/by_fee_density/1000
                        time:   [60.526 µs 60.770 µs 61.057 µs]
Benchmarking mempool/get_sorted/by_fee_density/5000
Benchmarking mempool/get_sorted/by_fee_density/5000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/5000: Collecting 100 samples in estimated 5.2631 s (71k iterations)
Benchmarking mempool/get_sorted/by_fee_density/5000: Analyzing
mempool/get_sorted/by_fee_density/5000
                        time:   [73.807 µs 74.130 µs 74.502 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking mempool/get_sorted/by_fee_density/10000
Benchmarking mempool/get_sorted/by_fee_density/10000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/10000: Collecting 100 samples in estimated 5.1086 s (76k iterations)
Benchmarking mempool/get_sorted/by_fee_density/10000: Analyzing
mempool/get_sorted/by_fee_density/10000
                        time:   [67.855 µs 68.341 µs 68.867 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mempool/drain_readd/txs/1000
Benchmarking mempool/drain_readd/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 60.
Benchmarking mempool/drain_readd/txs/1000: Collecting 100 samples in estimated 6.2298 s (5050 iterations)
Benchmarking mempool/drain_readd/txs/1000: Analyzing
mempool/drain_readd/txs/1000
                        time:   [644.89 µs 646.11 µs 647.50 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking mempool/drain_readd/txs/5000
Benchmarking mempool/drain_readd/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/5000: Collecting 100 samples in estimated 5.6415 s (800 iterations)
Benchmarking mempool/drain_readd/txs/5000: Analyzing
mempool/drain_readd/txs/5000
                        time:   [3.7118 ms 3.7369 ms 3.7644 ms]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/drain_readd/txs/10000
Benchmarking mempool/drain_readd/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/10000: Collecting 100 samples in estimated 5.8766 s (200 iterations)
Benchmarking mempool/drain_readd/txs/10000: Analyzing
mempool/drain_readd/txs/10000
                        time:   [12.933 ms 13.072 ms 13.210 ms]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild

Benchmarking mempool/batch_remove/50_from/5000
Benchmarking mempool/batch_remove/50_from/5000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/5000: Collecting 100 samples in estimated 5.0042 s (4.5M iterations)
Benchmarking mempool/batch_remove/50_from/5000: Analyzing
mempool/batch_remove/50_from/5000
                        time:   [1.1065 µs 1.1078 µs 1.1096 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/batch_remove/50_from/10000
Benchmarking mempool/batch_remove/50_from/10000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/10000: Collecting 100 samples in estimated 5.0003 s (4.5M iterations)
Benchmarking mempool/batch_remove/50_from/10000: Analyzing
mempool/batch_remove/50_from/10000
                        time:   [1.1204 µs 1.1229 µs 1.1261 µs]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe


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
Benchmarking ed25519_verify/single: Collecting 100 samples in estimated 5.0840 s (101k iterations)
Benchmarking ed25519_verify/single: Analyzing
ed25519_verify/single   time:   [46.442 µs 46.823 µs 47.152 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking ed25519_batch_verify/sequential/1
Benchmarking ed25519_batch_verify/sequential/1: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/1: Collecting 100 samples in estimated 5.0575 s (116k iterations)
Benchmarking ed25519_batch_verify/sequential/1: Analyzing
ed25519_batch_verify/sequential/1
                        time:   [43.300 µs 43.413 µs 43.522 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking ed25519_batch_verify/sequential/10
Benchmarking ed25519_batch_verify/sequential/10: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/10: Collecting 100 samples in estimated 6.6068 s (15k iterations)
Benchmarking ed25519_batch_verify/sequential/10: Analyzing
ed25519_batch_verify/sequential/10
                        time:   [439.69 µs 441.36 µs 443.17 µs]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
Benchmarking ed25519_batch_verify/sequential/50
Benchmarking ed25519_batch_verify/sequential/50: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/50: Collecting 100 samples in estimated 5.0956 s (2300 iterations)
Benchmarking ed25519_batch_verify/sequential/50: Analyzing
ed25519_batch_verify/sequential/50
                        time:   [2.2001 ms 2.2042 ms 2.2081 ms]
Benchmarking ed25519_batch_verify/sequential/100
Benchmarking ed25519_batch_verify/sequential/100: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/100: Collecting 100 samples in estimated 5.3699 s (1200 iterations)
Benchmarking ed25519_batch_verify/sequential/100: Analyzing
ed25519_batch_verify/sequential/100
                        time:   [4.4209 ms 4.4284 ms 4.4356 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking ed25519_batch_verify/sequential/200
Benchmarking ed25519_batch_verify/sequential/200: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/200: Collecting 100 samples in estimated 5.3128 s (600 iterations)
Benchmarking ed25519_batch_verify/sequential/200: Analyzing
ed25519_batch_verify/sequential/200
                        time:   [8.9161 ms 8.9319 ms 8.9478 ms]
Benchmarking ed25519_batch_verify/sequential/500
Benchmarking ed25519_batch_verify/sequential/500: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/500: Collecting 100 samples in estimated 6.6963 s (300 iterations)
Benchmarking ed25519_batch_verify/sequential/500: Analyzing
ed25519_batch_verify/sequential/500
                        time:   [22.314 ms 22.348 ms 22.382 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking keyhash_from_vkey/single
Benchmarking keyhash_from_vkey/single: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/single: Collecting 100 samples in estimated 5.0005 s (31M iterations)
Benchmarking keyhash_from_vkey/single: Analyzing
keyhash_from_vkey/single
                        time:   [162.79 ns 164.13 ns 165.31 ns]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
Benchmarking keyhash_from_vkey/batch/10
Benchmarking keyhash_from_vkey/batch/10: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/10: Collecting 100 samples in estimated 5.0024 s (3.3M iterations)
Benchmarking keyhash_from_vkey/batch/10: Analyzing
keyhash_from_vkey/batch/10
                        time:   [1.5197 µs 1.5218 µs 1.5241 µs]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/50
Benchmarking keyhash_from_vkey/batch/50: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/50: Collecting 100 samples in estimated 5.0198 s (662k iterations)
Benchmarking keyhash_from_vkey/batch/50: Analyzing
keyhash_from_vkey/batch/50
                        time:   [7.6043 µs 7.6167 µs 7.6341 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/100
Benchmarking keyhash_from_vkey/batch/100: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/100: Collecting 100 samples in estimated 5.0454 s (333k iterations)
Benchmarking keyhash_from_vkey/batch/100: Analyzing
keyhash_from_vkey/batch/100
                        time:   [15.191 µs 15.208 µs 15.227 µs]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/200
Benchmarking keyhash_from_vkey/batch/200: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/200: Collecting 100 samples in estimated 5.0584 s (167k iterations)
Benchmarking keyhash_from_vkey/batch/200: Analyzing
keyhash_from_vkey/batch/200
                        time:   [30.398 µs 30.434 µs 30.472 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/500
Benchmarking keyhash_from_vkey/batch/500: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/500: Collecting 100 samples in estimated 5.3501 s (71k iterations)
Benchmarking keyhash_from_vkey/batch/500: Analyzing
keyhash_from_vkey/batch/500
                        time:   [76.113 µs 76.213 µs 76.316 µs]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild

Benchmarking vrf_verify/single_proof
Benchmarking vrf_verify/single_proof: Warming up for 3.0000 s
Benchmarking vrf_verify/single_proof: Collecting 100 samples in estimated 5.6128 s (45k iterations)
Benchmarking vrf_verify/single_proof: Analyzing
vrf_verify/single_proof time:   [123.51 µs 123.61 µs 123.71 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe


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
Benchmarking blake2b_256/hash/32B_txhash: Collecting 100 samples in estimated 5.0005 s (32M iterations)
Benchmarking blake2b_256/hash/32B_txhash: Analyzing
blake2b_256/hash/32B_txhash
                        time:   [157.29 ns 157.55 ns 157.87 ns]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
Benchmarking blake2b_256/hash/64B_vkey
Benchmarking blake2b_256/hash/64B_vkey: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/64B_vkey: Collecting 100 samples in estimated 5.0003 s (32M iterations)
Benchmarking blake2b_256/hash/64B_vkey: Analyzing
blake2b_256/hash/64B_vkey
                        time:   [157.33 ns 157.64 ns 158.02 ns]
Found 19 outliers among 100 measurements (19.00%)
  2 (2.00%) high mild
  17 (17.00%) high severe
Benchmarking blake2b_256/hash/256B_small_tx
Benchmarking blake2b_256/hash/256B_small_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/256B_small_tx: Collecting 100 samples in estimated 5.0012 s (18M iterations)
Benchmarking blake2b_256/hash/256B_small_tx: Analyzing
blake2b_256/hash/256B_small_tx
                        time:   [277.69 ns 278.47 ns 279.41 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking blake2b_256/hash/500B_avg_tx
Benchmarking blake2b_256/hash/500B_avg_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/500B_avg_tx: Collecting 100 samples in estimated 5.0019 s (9.3M iterations)
Benchmarking blake2b_256/hash/500B_avg_tx: Analyzing
blake2b_256/hash/500B_avg_tx
                        time:   [536.38 ns 536.76 ns 537.24 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/1KB_tx_body
Benchmarking blake2b_256/hash/1KB_tx_body: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/1KB_tx_body: Collecting 100 samples in estimated 5.0029 s (4.8M iterations)
Benchmarking blake2b_256/hash/1KB_tx_body: Analyzing
blake2b_256/hash/1KB_tx_body
                        time:   [1.0493 µs 1.0498 µs 1.0504 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking blake2b_256/hash/4KB_large_tx
Benchmarking blake2b_256/hash/4KB_large_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/4KB_large_tx: Collecting 100 samples in estimated 5.0176 s (1.2M iterations)
Benchmarking blake2b_256/hash/4KB_large_tx: Analyzing
blake2b_256/hash/4KB_large_tx
                        time:   [4.1219 µs 4.1316 µs 4.1450 µs]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
Benchmarking blake2b_256/hash/16KB_block_header
Benchmarking blake2b_256/hash/16KB_block_header: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/16KB_block_header: Collecting 100 samples in estimated 5.0696 s (308k iterations)
Benchmarking blake2b_256/hash/16KB_block_header: Analyzing
blake2b_256/hash/16KB_block_header
                        time:   [16.415 µs 16.418 µs 16.422 µs]
Found 17 outliers among 100 measurements (17.00%)
  2 (2.00%) high mild
  15 (15.00%) high severe
Benchmarking blake2b_256/hash/20KB_avg_block
Benchmarking blake2b_256/hash/20KB_avg_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/20KB_avg_block: Collecting 100 samples in estimated 5.0792 s (247k iterations)
Benchmarking blake2b_256/hash/20KB_avg_block: Analyzing
blake2b_256/hash/20KB_avg_block
                        time:   [20.514 µs 20.517 µs 20.521 µs]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking blake2b_256/hash/90KB_max_block
Benchmarking blake2b_256/hash/90KB_max_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/90KB_max_block: Collecting 100 samples in estimated 5.1389 s (56k iterations)
Benchmarking blake2b_256/hash/90KB_max_block: Analyzing
blake2b_256/hash/90KB_max_block
                        time:   [92.247 µs 92.277 µs 92.325 µs]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe

Benchmarking blake2b_224/hash/32B_vkey_to_keyhash
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Collecting 100 samples in estimated 5.0006 s (30M iterations)
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Analyzing
blake2b_224/hash/32B_vkey_to_keyhash
                        time:   [165.33 ns 165.80 ns 166.35 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking blake2b_224/hash/64B_script_bytes
Benchmarking blake2b_224/hash/64B_script_bytes: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/64B_script_bytes: Collecting 100 samples in estimated 5.0007 s (30M iterations)
Benchmarking blake2b_224/hash/64B_script_bytes: Analyzing
blake2b_224/hash/64B_script_bytes
                        time:   [165.07 ns 165.71 ns 166.65 ns]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_224/hash/256B_address_payload
Benchmarking blake2b_224/hash/256B_address_payload: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/256B_address_payload: Collecting 100 samples in estimated 5.0011 s (17M iterations)
Benchmarking blake2b_224/hash/256B_address_payload: Analyzing
blake2b_224/hash/256B_address_payload
                        time:   [285.13 ns 285.54 ns 285.98 ns]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild

Benchmarking blake2b_batch/224_keyhashes/10
Benchmarking blake2b_batch/224_keyhashes/10: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/10: Collecting 100 samples in estimated 5.0054 s (3.2M iterations)
Benchmarking blake2b_batch/224_keyhashes/10: Analyzing
blake2b_batch/224_keyhashes/10
                        time:   [1.5654 µs 1.5677 µs 1.5705 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/50
Benchmarking blake2b_batch/224_keyhashes/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/50: Collecting 100 samples in estimated 5.0270 s (636k iterations)
Benchmarking blake2b_batch/224_keyhashes/50: Analyzing
blake2b_batch/224_keyhashes/50
                        time:   [7.8255 µs 7.8356 µs 7.8478 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/224_keyhashes/100
Benchmarking blake2b_batch/224_keyhashes/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/100: Collecting 100 samples in estimated 5.0363 s (318k iterations)
Benchmarking blake2b_batch/224_keyhashes/100: Analyzing
blake2b_batch/224_keyhashes/100
                        time:   [15.650 µs 15.665 µs 15.682 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_batch/224_keyhashes/500
Benchmarking blake2b_batch/224_keyhashes/500: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/500: Collecting 100 samples in estimated 5.1999 s (66k iterations)
Benchmarking blake2b_batch/224_keyhashes/500: Analyzing
blake2b_batch/224_keyhashes/500
                        time:   [78.418 µs 78.520 µs 78.656 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/50
Benchmarking blake2b_batch/256_txbodies_500B/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/50: Collecting 100 samples in estimated 5.1332 s (192k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/50: Analyzing
blake2b_batch/256_txbodies_500B/50
                        time:   [26.694 µs 26.713 µs 26.732 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/100
Benchmarking blake2b_batch/256_txbodies_500B/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/100: Collecting 100 samples in estimated 5.1508 s (96k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/100: Analyzing
blake2b_batch/256_txbodies_500B/100
                        time:   [53.362 µs 53.394 µs 53.424 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/300
Benchmarking blake2b_batch/256_txbodies_500B/300: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/300: Collecting 100 samples in estimated 5.6739 s (35k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/300: Analyzing
blake2b_batch/256_txbodies_500B/300
                        time:   [160.13 µs 160.24 µs 160.34 µs]
Found 1 outliers among 100 measurements (1.00%)
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
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Collecting 100 samples in estimated 5.0001 s (2.6M iterations)
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Analyzing
serialization/encode_transaction/conway_2in_2out_2wit
                        time:   [1.9298 µs 1.9432 µs 1.9629 µs]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking serialization/encode_transaction/body_only_2in_2out
Benchmarking serialization/encode_transaction/body_only_2in_2out: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/body_only_2in_2out: Collecting 100 samples in estimated 5.0009 s (3.9M iterations)
Benchmarking serialization/encode_transaction/body_only_2in_2out: Analyzing
serialization/encode_transaction/body_only_2in_2out
                        time:   [1.2466 µs 1.2526 µs 1.2627 µs]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

Benchmarking serialization/encode_block_header/with_vrf_output
Benchmarking serialization/encode_block_header/with_vrf_output: Warming up for 3.0000 s
Benchmarking serialization/encode_block_header/with_vrf_output: Collecting 100 samples in estimated 5.0042 s (4.6M iterations)
Benchmarking serialization/encode_block_header/with_vrf_output: Analyzing
serialization/encode_block_header/with_vrf_output
                        time:   [1.1210 µs 1.1416 µs 1.1669 µs]
Found 27 outliers among 100 measurements (27.00%)
  15 (15.00%) low mild
  12 (12.00%) high severe

Benchmarking serialization/encode_value/ada_only
Benchmarking serialization/encode_value/ada_only: Warming up for 3.0000 s
Benchmarking serialization/encode_value/ada_only: Collecting 100 samples in estimated 5.0001 s (225M iterations)
Benchmarking serialization/encode_value/ada_only: Analyzing
serialization/encode_value/ada_only
                        time:   [22.110 ns 22.125 ns 22.143 ns]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking serialization/encode_value/multi_asset_3policy_5asset
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Warming up for 3.0000 s
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Collecting 100 samples in estimated 5.0024 s (6.7M iterations)
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Analyzing
serialization/encode_value/multi_asset_3policy_5asset
                        time:   [759.35 ns 760.75 ns 762.27 ns]


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
[test_mainnet_scale_insert_read] flush complete, sampling 1000 keys...
[test_mainnet_scale_insert_read] verified 1000/1000 sampled keys — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
[test_mainnet_scale_delete_amplification] insert flush complete
[test_mainnet_scale_delete_amplification] deleting 80000 entries...
[test_mainnet_scale_wal_crash_recovery] reopened, verifying 100000 entries...
[test_mainnet_scale_wal_crash_recovery] all 100000 entries recovered — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
[test_mainnet_scale_delete_amplification] delete flush complete
[test_mainnet_scale_delete_amplification] verifying surviving 20000 entries...
[test_mainnet_scale_delete_amplification] verifying 1K deleted keys return None...
[test_mainnet_scale_delete_amplification] range scanning for exact count...
[test_mainnet_scale_delete_amplification] 20000 entries confirmed — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out; finished in 0.64s

   Doc-tests dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s


```

</details>

