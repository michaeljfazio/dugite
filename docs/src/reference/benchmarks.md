# Nightly Benchmark Results — 2026-11-08

Captured on **2026-11-08** from commit [`00c2806`](https://github.com/MichaelJFazio/dugite/commit/00c2806) on `main` (GitHub Actions `ubuntu-latest`).

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

> The collapsed _Raw measurements_ blocks contain the filtered `cargo bench` output (cargo build chatter and ANSI escapes are stripped). Full unfiltered logs are uploaded as the `benchmark-results-248` workflow artifact.

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
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 6.4157 s (30 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [202.95 ms 211.18 ms 220.77 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 5.3006 s (30k iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [174.28 µs 174.96 µs 175.73 µs]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 5.3128 s (500 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [10.450 ms 10.593 ms 10.741 ms]

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 5.0000 s (21B iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [239.01 ps 239.90 ps 240.98 ps]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 5.0370 s (354k iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [14.178 µs 14.282 µs 14.445 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 5.0000 s (30M iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [162.91 ns 164.45 ns 166.27 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 7.3907 s (165 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [7.0489 ms 7.2713 ms 7.4660 ms]

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 6.5199 s (30 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [206.82 ms 210.11 ms 213.43 ms]
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 6.4800 s (30 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [212.77 ms 214.96 ms 216.97 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 6.4207 s (30 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [34.825 ms 35.683 ms 36.699 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 6.7048 s (30 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [35.122 ms 36.089 ms 37.168 ms]

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.4s, or reduce sample count to 40.
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 10.419 s (100 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [101.70 ms 102.88 ms 104.43 ms]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.4s, or reduce sample count to 40.
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 10.380 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [100.01 ms 101.04 ms 102.32 ms]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 11.9s, or reduce sample count to 40.
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 11.861 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [98.171 ms 98.752 ms 99.402 ms]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 120.5s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 120.46 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [983.89 ms 994.42 ms 1.0071 s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 96.9s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 96.904 s (100 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [981.15 ms 984.79 ms 988.66 ms]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 97.4s, or reduce sample count to 10.
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 97.411 s (100 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [968.04 ms 971.00 ms 974.18 ms]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.7013 s (700 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [8.2922 ms 8.3518 ms 8.4147 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.7045 s (700 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [8.0919 ms 8.1437 ms 8.1989 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0193 s (258k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [19.548 µs 19.589 µs 19.634 µs]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0873 s (258k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [19.484 µs 19.554 µs 19.631 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 5.1797 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [12.899 ms 13.074 ms 13.373 ms]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.5696 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [13.691 ms 13.749 ms 13.811 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.7181 s (20k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [293.57 µs 295.98 µs 298.30 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 6.1010 s (20k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [291.93 µs 293.84 µs 295.77 µs]

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 6.7702 s (15k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [452.06 µs 454.06 µs 456.09 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.0987 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.2851 ms 6.3460 ms 6.4178 ms]
Found 41 outliers among 100 measurements (41.00%)
  13 (13.00%) low severe
  4 (4.00%) low mild
  1 (1.00%) high mild
  23 (23.00%) high severe
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.5s, enable flat sampling, or reduce sample count to 50.
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 9.5494 s (5050 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [1.8828 ms 1.8917 ms 1.9056 ms]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.5s, or reduce sample count to 90.
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 5.5147 s (100 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [54.801 ms 55.274 ms 55.830 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.1486 s (1000 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [4.6537 ms 4.7933 ms 4.9581 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 11.6s, or reduce sample count to 40.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 11.587 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [111.86 ms 113.25 ms 114.87 ms]
Found 21 outliers among 100 measurements (21.00%)
  2 (2.00%) low mild
  19 (19.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0081 s (550k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [8.9343 µs 8.9713 µs 9.0154 µs]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0487 s (303k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [16.567 µs 16.651 µs 16.744 µs]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0034 s (535k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [9.3683 µs 9.4282 µs 9.5032 µs]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0505 s (424k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [12.040 µs 12.132 µs 12.267 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0352 s (535k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [9.3569 µs 9.3835 µs 9.4142 µs]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0432 s (439k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [11.499 µs 11.673 µs 11.967 µs]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0119 s (722k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [7.1271 µs 7.1464 µs 7.1695 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.0884 s (222k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [23.061 µs 23.374 µs 23.807 µs]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0222 s (11k iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [422.69 µs 426.30 µs 434.73 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.3415 s (770 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.4639 ms 6.6152 ms 6.8102 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.0501 s (2640 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [1.8996 ms 1.9030 ms 1.9071 ms]
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 6.0554 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [55.143 ms 55.388 ms 55.555 ms]
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.0351 s (1375 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [3.6076 ms 3.6364 ms 3.6745 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.5s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 6.5395 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [110.15 ms 111.74 ms 114.03 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0003 s (565k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [8.6370 µs 8.7138 µs 8.8388 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0006 s (311k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [16.124 µs 16.306 µs 16.512 µs]
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0002 s (548k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [8.8523 µs 8.9362 µs 9.1078 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0006 s (434k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [11.359 µs 11.428 µs 11.526 µs]
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0003 s (551k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [9.0337 µs 9.0844 µs 9.1350 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0004 s (448k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [11.038 µs 11.150 µs 11.250 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.1s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.1013 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [93.192 ms 94.922 ms 97.509 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.2s or enable flat sampling.
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.1932 s (55 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [92.718 ms 95.051 ms 96.736 ms]
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 9.3287 s (20 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [459.45 ms 463.31 ms 467.63 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 9.3753 s (20 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [458.58 ms 461.31 ms 464.71 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.4s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 10.382 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [922.49 ms 934.97 ms 948.99 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.4s.
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 9.3887 s (10 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [928.32 ms 941.34 ms 957.56 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 6.1719 s (30 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [198.96 ms 203.43 ms 207.54 ms]


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

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 21.9s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 21.918 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.1455 s 2.1689 s 2.1920 s]

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 6.5892 s (20k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [317.99 µs 321.58 µs 326.98 µs]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.1356 s (30k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [172.21 µs 174.16 µs 176.30 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 5.2553 s (20k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [258.94 µs 260.03 µs 261.12 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.2012 s (30k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [172.13 µs 174.40 µs 177.66 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 40.2s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 40.239 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.1925 s 2.2133 s 2.2401 s]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  2 (20.00%) high severe

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.1s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 22.146 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [329.86 ms 346.74 ms 366.23 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.2s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 22.209 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [355.07 ms 362.41 ms 370.52 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.1s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 28.111 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [2.6494 s 2.6731 s 2.7019 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.6s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 27.642 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [123.43 ms 133.98 ms 144.53 ms]

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 23.5s, or reduce sample count to 20.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 23.496 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [241.66 ms 243.31 ms 245.09 ms]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 30.4s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 30.447 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [402.78 ms 408.94 ms 415.27 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.5s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 22.482 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.2349 s 2.2666 s 2.2989 s]
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.3s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 22.330 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.2060 s 2.2123 s 2.2183 s]
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.3s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 22.297 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.2351 s 2.2553 s 2.2756 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 23.1s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 23.075 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.3041 s 2.3219 s 2.3408 s]
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 23.0s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 23.012 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.2690 s 2.2865 s 2.3027 s]

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 6.4538 s (20k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [316.55 µs 317.99 µs 319.44 µs]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 6.4887 s (20k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [323.47 µs 324.73 µs 326.02 µs]
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 6.5478 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [322.30 µs 325.15 µs 328.75 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 6.5180 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [328.21 µs 331.76 µs 336.49 µs]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 6.5098 s (20k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [327.23 µs 331.11 µs 335.92 µs]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.6s or enable flat sampling.
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 9.5762 s (55 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [154.83 ms 156.69 ms 158.68 ms]
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.0s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 10.017 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [989.43 ms 1.0000 s 1.0096 s]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 22.6s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 22.598 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.2483 s 2.2662 s 2.2850 s]

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0118 s (22k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [225.73 µs 227.15 µs 230.97 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0062 s (17k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [291.99 µs 292.47 µs 293.08 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0107 s (15k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [332.56 µs 335.64 µs 339.12 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.5s or enable flat sampling.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 9.5216 s (55 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [18.021 ms 19.009 ms 19.799 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.7s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 10.730 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [140.45 ms 150.16 ms 159.85 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 23.5s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 23.498 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [356.96 ms 368.71 ms 381.86 ms]

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.4516 s (330 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [16.748 ms 16.906 ms 17.138 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.7s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 6.7493 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [128.06 ms 129.07 ms 131.04 ms]
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 7.3662 s (30 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [254.58 ms 257.57 ms 261.26 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 144.7s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 144.66 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [14.200 s 14.308 s 14.410 s]
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 313.5s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 313.54 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [29.177 s 29.369 s 29.574 s]

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0160 s (4895 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.0255 ms 1.0363 ms 1.0507 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0333 s (4015 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.2355 ms 1.2513 ms 1.2630 ms]

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.9s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 10.920 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.1139 s 1.1215 s 1.1277 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.0s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 19.986 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [2.1274 s 2.1381 s 2.1485 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking ledger/apply_block/apply_only_shelley_50tx
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Collecting 20 samples in estimated 5.0026 s (98k iterations)
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Analyzing
ledger/apply_block/apply_only_shelley_50tx
                        time:   [50.464 µs 50.803 µs 51.177 µs]
Benchmarking ledger/apply_block/validate_all_shelley_50tx
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Collecting 20 samples in estimated 5.0172 s (56k iterations)
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Analyzing
ledger/apply_block/validate_all_shelley_50tx
                        time:   [89.114 µs 90.109 µs 91.198 µs]


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
Benchmarking network/handshake_encode/n2n_version_data: Collecting 100 samples in estimated 5.0001 s (154M iterations)
Benchmarking network/handshake_encode/n2n_version_data: Analyzing
network/handshake_encode/n2n_version_data
                        time:   [31.330 ns 31.569 ns 31.800 ns]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild
Benchmarking network/handshake_encode/n2c_version_data
Benchmarking network/handshake_encode/n2c_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2c_version_data: Collecting 100 samples in estimated 5.0001 s (223M iterations)
Benchmarking network/handshake_encode/n2c_version_data: Analyzing
network/handshake_encode/n2c_version_data
                        time:   [22.301 ns 22.501 ns 22.726 ns]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe

Benchmarking network/chainsync/roll_forward/encode/256
Benchmarking network/chainsync/roll_forward/encode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/256: Collecting 100 samples in estimated 5.0000 s (42M iterations)
Benchmarking network/chainsync/roll_forward/encode/256: Analyzing
network/chainsync/roll_forward/encode/256
                        time:   [116.00 ns 116.91 ns 117.79 ns]
                        thrpt:  [2.4589 GiB/s 2.4775 GiB/s 2.4969 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking network/chainsync/roll_forward/decode/256
Benchmarking network/chainsync/roll_forward/decode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/256: Collecting 100 samples in estimated 5.0002 s (58M iterations)
Benchmarking network/chainsync/roll_forward/decode/256: Analyzing
network/chainsync/roll_forward/decode/256
                        time:   [84.557 ns 85.300 ns 85.995 ns]
                        thrpt:  [3.3681 GiB/s 3.3956 GiB/s 3.4254 GiB/s]
Benchmarking network/chainsync/roll_forward/encode/1024
Benchmarking network/chainsync/roll_forward/encode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/1024: Collecting 100 samples in estimated 5.0001 s (36M iterations)
Benchmarking network/chainsync/roll_forward/encode/1024: Analyzing
network/chainsync/roll_forward/encode/1024
                        time:   [136.36 ns 137.49 ns 138.63 ns]
                        thrpt:  [7.2486 GiB/s 7.3089 GiB/s 7.3693 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
Benchmarking network/chainsync/roll_forward/decode/1024
Benchmarking network/chainsync/roll_forward/decode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/1024: Collecting 100 samples in estimated 5.0001 s (57M iterations)
Benchmarking network/chainsync/roll_forward/decode/1024: Analyzing
network/chainsync/roll_forward/decode/1024
                        time:   [88.342 ns 89.046 ns 89.727 ns]
                        thrpt:  [11.199 GiB/s 11.285 GiB/s 11.375 GiB/s]
Benchmarking network/chainsync/roll_forward/encode/4096
Benchmarking network/chainsync/roll_forward/encode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/4096: Collecting 100 samples in estimated 5.0001 s (27M iterations)
Benchmarking network/chainsync/roll_forward/encode/4096: Analyzing
network/chainsync/roll_forward/encode/4096
                        time:   [188.45 ns 189.80 ns 191.04 ns]
                        thrpt:  [20.236 GiB/s 20.368 GiB/s 20.515 GiB/s]
Benchmarking network/chainsync/roll_forward/decode/4096
Benchmarking network/chainsync/roll_forward/decode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/4096: Collecting 100 samples in estimated 5.0004 s (34M iterations)
Benchmarking network/chainsync/roll_forward/decode/4096: Analyzing
network/chainsync/roll_forward/decode/4096
                        time:   [146.34 ns 147.22 ns 148.13 ns]
                        thrpt:  [26.099 GiB/s 26.259 GiB/s 26.417 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild

Benchmarking network/chainsync/roll_backward/encode
Benchmarking network/chainsync/roll_backward/encode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/encode: Collecting 100 samples in estimated 5.0005 s (27M iterations)
Benchmarking network/chainsync/roll_backward/encode: Analyzing
network/chainsync/roll_backward/encode
                        time:   [181.22 ns 182.47 ns 183.70 ns]
                        thrpt:  [456.85 MiB/s 459.93 MiB/s 463.11 MiB/s]
Benchmarking network/chainsync/roll_backward/decode
Benchmarking network/chainsync/roll_backward/decode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/decode: Collecting 100 samples in estimated 5.0000 s (84M iterations)
Benchmarking network/chainsync/roll_backward/decode: Analyzing
network/chainsync/roll_backward/decode
                        time:   [59.600 ns 59.978 ns 60.378 ns]
                        thrpt:  [1.3574 GiB/s 1.3664 GiB/s 1.3751 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe

Benchmarking network/blockfetch/msg_block/encode/2048
Benchmarking network/blockfetch/msg_block/encode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/2048: Collecting 100 samples in estimated 5.0001 s (56M iterations)
Benchmarking network/blockfetch/msg_block/encode/2048: Analyzing
network/blockfetch/msg_block/encode/2048
                        time:   [86.531 ns 87.235 ns 87.964 ns]
                        thrpt:  [21.736 GiB/s 21.918 GiB/s 22.096 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/2048
Benchmarking network/blockfetch/msg_block/decode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/2048: Collecting 100 samples in estimated 5.0002 s (56M iterations)
Benchmarking network/blockfetch/msg_block/decode/2048: Analyzing
network/blockfetch/msg_block/decode/2048
                        time:   [88.186 ns 88.816 ns 89.427 ns]
                        thrpt:  [21.381 GiB/s 21.528 GiB/s 21.681 GiB/s]
Benchmarking network/blockfetch/msg_block/encode/20480
Benchmarking network/blockfetch/msg_block/encode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/20480: Collecting 100 samples in estimated 5.0005 s (20M iterations)
Benchmarking network/blockfetch/msg_block/encode/20480: Analyzing
network/blockfetch/msg_block/encode/20480
                        time:   [248.85 ns 250.04 ns 251.14 ns]
                        thrpt:  [75.966 GiB/s 76.299 GiB/s 76.666 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  5 (5.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/20480
Benchmarking network/blockfetch/msg_block/decode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/20480: Collecting 100 samples in estimated 5.0003 s (26M iterations)
Benchmarking network/blockfetch/msg_block/decode/20480: Analyzing
network/blockfetch/msg_block/decode/20480
                        time:   [191.37 ns 193.28 ns 195.57 ns]
                        thrpt:  [97.553 GiB/s 98.707 GiB/s 99.691 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  2 (2.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/90000
Benchmarking network/blockfetch/msg_block/encode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/90000: Collecting 100 samples in estimated 5.0022 s (2.2M iterations)
Benchmarking network/blockfetch/msg_block/encode/90000: Analyzing
network/blockfetch/msg_block/encode/90000
                        time:   [2.2329 µs 2.2481 µs 2.2649 µs]
                        thrpt:  [37.011 GiB/s 37.288 GiB/s 37.541 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/90000
Benchmarking network/blockfetch/msg_block/decode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/90000: Collecting 100 samples in estimated 5.0039 s (2.2M iterations)
Benchmarking network/blockfetch/msg_block/decode/90000: Analyzing
network/blockfetch/msg_block/decode/90000
                        time:   [2.2611 µs 2.2749 µs 2.2888 µs]
                        thrpt:  [36.624 GiB/s 36.848 GiB/s 37.073 GiB/s]

Benchmarking network/blockfetch/request_range/encode
Benchmarking network/blockfetch/request_range/encode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/encode: Collecting 100 samples in estimated 5.0000 s (44M iterations)
Benchmarking network/blockfetch/request_range/encode: Analyzing
network/blockfetch/request_range/encode
                        time:   [111.64 ns 112.61 ns 113.62 ns]
Benchmarking network/blockfetch/request_range/decode
Benchmarking network/blockfetch/request_range/decode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/decode: Collecting 100 samples in estimated 5.0001 s (103M iterations)
Benchmarking network/blockfetch/request_range/decode: Analyzing
network/blockfetch/request_range/decode
                        time:   [48.469 ns 48.921 ns 49.372 ns]

Benchmarking network/n2c_query_encode/pparams/hfc_success
Benchmarking network/n2c_query_encode/pparams/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/hfc_success: Collecting 100 samples in estimated 5.0001 s (77M iterations)
Benchmarking network/n2c_query_encode/pparams/hfc_success: Analyzing
network/n2c_query_encode/pparams/hfc_success
                        time:   [65.970 ns 66.524 ns 67.156 ns]
                        thrpt:  [1.2620 GiB/s 1.2740 GiB/s 1.2847 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking network/n2c_query_encode/pparams/tag24
Benchmarking network/n2c_query_encode/pparams/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/tag24: Collecting 100 samples in estimated 5.0000 s (75M iterations)
Benchmarking network/n2c_query_encode/pparams/tag24: Analyzing
network/n2c_query_encode/pparams/tag24
                        time:   [67.127 ns 67.439 ns 67.759 ns]
                        thrpt:  [1.2508 GiB/s 1.2567 GiB/s 1.2625 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
Benchmarking network/n2c_query_encode/govstate/hfc_success
Benchmarking network/n2c_query_encode/govstate/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/hfc_success: Collecting 100 samples in estimated 5.0000 s (61M iterations)
Benchmarking network/n2c_query_encode/govstate/hfc_success: Analyzing
network/n2c_query_encode/govstate/hfc_success
                        time:   [80.817 ns 81.330 ns 81.821 ns]
                        thrpt:  [8.9011 GiB/s 8.9548 GiB/s 9.0116 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking network/n2c_query_encode/govstate/tag24
Benchmarking network/n2c_query_encode/govstate/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/tag24: Collecting 100 samples in estimated 5.0003 s (58M iterations)
Benchmarking network/n2c_query_encode/govstate/tag24: Analyzing
network/n2c_query_encode/govstate/tag24
                        time:   [84.949 ns 85.553 ns 86.224 ns]
                        thrpt:  [8.4466 GiB/s 8.5128 GiB/s 8.5733 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking network/n2c_query_encode/era_mismatch
Benchmarking network/n2c_query_encode/era_mismatch: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/era_mismatch: Collecting 100 samples in estimated 5.0000 s (220M iterations)
Benchmarking network/n2c_query_encode/era_mismatch: Analyzing
network/n2c_query_encode/era_mismatch
                        time:   [23.007 ns 23.171 ns 23.318 ns]
                        thrpt:  [31.233 GiB/s 31.431 GiB/s 31.655 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild


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
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Collecting 100 samples in estimated 5.0990 s (162k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Analyzing
consensus/vrf_leader_check/single/sigma=0.0000247
                        time:   [31.456 µs 31.473 µs 31.490 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.001
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Collecting 100 samples in estimated 5.1036 s (162k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Analyzing
consensus/vrf_leader_check/single/sigma=0.001
                        time:   [31.616 µs 31.639 µs 31.665 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.01
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Collecting 100 samples in estimated 5.1016 s (162k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Analyzing
consensus/vrf_leader_check/single/sigma=0.01
                        time:   [31.624 µs 31.707 µs 31.799 µs]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking consensus/vrf_leader_check/batch/21600
Benchmarking consensus/vrf_leader_check/batch/21600: Warming up for 3.0000 s

Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 13.7s, or reduce sample count to 10.
Benchmarking consensus/vrf_leader_check/batch/21600: Collecting 20 samples in estimated 13.726 s (20 iterations)
Benchmarking consensus/vrf_leader_check/batch/21600: Analyzing
consensus/vrf_leader_check/batch/21600
                        time:   [683.80 ms 684.03 ms 684.24 ms]
                        thrpt:  [31.568 Kelem/s 31.577 Kelem/s 31.588 Kelem/s]
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) low severe
  2 (10.00%) low mild
  1 (5.00%) high mild

Benchmarking consensus/validate_header/replay_mode
Benchmarking consensus/validate_header/replay_mode: Warming up for 3.0000 s
Benchmarking consensus/validate_header/replay_mode: Collecting 100 samples in estimated 5.0000 s (572M iterations)
Benchmarking consensus/validate_header/replay_mode: Analyzing
consensus/validate_header/replay_mode
                        time:   [8.7393 ns 8.8014 ns 8.9297 ns]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

Benchmarking consensus/chain_selection/longer_fork_100
Benchmarking consensus/chain_selection/longer_fork_100: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/longer_fork_100: Collecting 100 samples in estimated 5.0000 s (3.6B iterations)
Benchmarking consensus/chain_selection/longer_fork_100: Analyzing
consensus/chain_selection/longer_fork_100
                        time:   [1.3634 ns 1.3646 ns 1.3665 ns]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
Benchmarking consensus/chain_selection/equal_length_tiebreak
Benchmarking consensus/chain_selection/equal_length_tiebreak: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/equal_length_tiebreak: Collecting 100 samples in estimated 5.0012 s (19M iterations)
Benchmarking consensus/chain_selection/equal_length_tiebreak: Analyzing
consensus/chain_selection/equal_length_tiebreak
                        time:   [266.59 ns 266.86 ns 267.13 ns]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild
Benchmarking consensus/chain_selection/prefer_simple
Benchmarking consensus/chain_selection/prefer_simple: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/prefer_simple: Collecting 100 samples in estimated 5.0000 s (2.4B iterations)
Benchmarking consensus/chain_selection/prefer_simple: Analyzing
consensus/chain_selection/prefer_simple
                        time:   [2.0752 ns 2.0839 ns 2.0974 ns]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
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
Benchmarking lsm/insert/random_keys/10000: Collecting 10 samples in estimated 5.1610 s (1540 iterations)
Benchmarking lsm/insert/random_keys/10000: Analyzing
lsm/insert/random_keys/10000
                        time:   [3.1986 ms 3.2037 ms 3.2097 ms]
                        thrpt:  [3.1155 Melem/s 3.1214 Melem/s 3.1264 Melem/s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking lsm/point_lookup/hit_random/10000
Benchmarking lsm/point_lookup/hit_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/hit_random/10000: Collecting 100 samples in estimated 6.3209 s (15k iterations)
Benchmarking lsm/point_lookup/hit_random/10000: Analyzing
lsm/point_lookup/hit_random/10000
                        time:   [416.96 µs 417.13 µs 417.32 µs]
                        thrpt:  [2.3962 Melem/s 2.3973 Melem/s 2.3983 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking lsm/point_lookup/miss_random/10000
Benchmarking lsm/point_lookup/miss_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/miss_random/10000: Collecting 100 samples in estimated 5.4978 s (56k iterations)
Benchmarking lsm/point_lookup/miss_random/10000: Analyzing
lsm/point_lookup/miss_random/10000
                        time:   [99.180 µs 99.263 µs 99.386 µs]
                        thrpt:  [10.062 Melem/s 10.074 Melem/s 10.083 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe

Benchmarking lsm/range_scan/window_100_of_10k/10000
Benchmarking lsm/range_scan/window_100_of_10k/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/window_100_of_10k/10000: Collecting 20 samples in estimated 5.0102 s (85k iterations)
Benchmarking lsm/range_scan/window_100_of_10k/10000: Analyzing
lsm/range_scan/window_100_of_10k/10000
                        time:   [57.499 µs 57.538 µs 57.588 µs]
                        thrpt:  [1.7365 Melem/s 1.7380 Melem/s 1.7392 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking lsm/range_scan/full_scan/10000
Benchmarking lsm/range_scan/full_scan/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/full_scan/10000: Collecting 20 samples in estimated 5.2156 s (3360 iterations)
Benchmarking lsm/range_scan/full_scan/10000: Analyzing
lsm/range_scan/full_scan/10000
                        time:   [1.5481 ms 1.5573 ms 1.5694 ms]
                        thrpt:  [6.3719 Melem/s 6.4215 Melem/s 6.4596 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  3 (15.00%) low severe
  1 (5.00%) low mild

Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Warming up for 3.0000 s
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Collecting 10 samples in estimated 5.2495 s (440 iterations)
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Analyzing
lsm/apply_batch/inserts_10k_deletes_2.5k/10000
                        time:   [5.3366 ms 5.3626 ms 5.3953 ms]
                        thrpt:  [1.8535 Melem/s 1.8648 Melem/s 1.8738 Melem/s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking lsm/snapshot/save_10k
Benchmarking lsm/snapshot/save_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/save_10k: Collecting 10 samples in estimated 5.0611 s (770 iterations)
Benchmarking lsm/snapshot/save_10k: Analyzing
lsm/snapshot/save_10k   time:   [484.32 µs 487.38 µs 490.65 µs]
Benchmarking lsm/snapshot/load_10k
Benchmarking lsm/snapshot/load_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/load_10k: Collecting 10 samples in estimated 5.0306 s (605 iterations)
Benchmarking lsm/snapshot/load_10k: Analyzing
lsm/snapshot/load_10k   time:   [1.5726 ms 1.5763 ms 1.5823 ms]
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
Benchmarking mempool/add/txs/1000: Collecting 100 samples in estimated 7.8724 s (10k iterations)
Benchmarking mempool/add/txs/1000: Analyzing
mempool/add/txs/1000    time:   [739.38 µs 740.02 µs 740.67 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking mempool/add/txs/5000
Benchmarking mempool/add/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/5000: Collecting 100 samples in estimated 5.9611 s (500 iterations)
Benchmarking mempool/add/txs/5000: Analyzing
mempool/add/txs/5000    time:   [9.2460 ms 9.5055 ms 9.7496 ms]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low severe
  6 (6.00%) low mild
  1 (1.00%) high mild
Benchmarking mempool/add/txs/10000
Benchmarking mempool/add/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/10000: Collecting 100 samples in estimated 6.2479 s (400 iterations)
Benchmarking mempool/add/txs/10000: Analyzing
mempool/add/txs/10000   time:   [13.327 ms 13.872 ms 14.451 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking mempool/remove/txs/1000
Benchmarking mempool/remove/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.8s, enable flat sampling, or reduce sample count to 60.
Benchmarking mempool/remove/txs/1000: Collecting 100 samples in estimated 5.8396 s (5050 iterations)
Benchmarking mempool/remove/txs/1000: Analyzing
mempool/remove/txs/1000 time:   [474.06 µs 475.07 µs 476.15 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking mempool/remove/txs/5000
Benchmarking mempool/remove/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/5000: Collecting 100 samples in estimated 5.6242 s (900 iterations)
Benchmarking mempool/remove/txs/5000: Analyzing
mempool/remove/txs/5000 time:   [2.6711 ms 2.6800 ms 2.6907 ms]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/remove/txs/10000
Benchmarking mempool/remove/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/10000: Collecting 100 samples in estimated 6.5222 s (400 iterations)
Benchmarking mempool/remove/txs/10000: Analyzing
mempool/remove/txs/10000
                        time:   [6.4521 ms 6.5465 ms 6.6448 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mempool/get_sorted/by_fee_density/1000
Benchmarking mempool/get_sorted/by_fee_density/1000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/1000: Collecting 100 samples in estimated 5.2640 s (71k iterations)
Benchmarking mempool/get_sorted/by_fee_density/1000: Analyzing
mempool/get_sorted/by_fee_density/1000
                        time:   [75.020 µs 75.441 µs 75.872 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking mempool/get_sorted/by_fee_density/5000
Benchmarking mempool/get_sorted/by_fee_density/5000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/5000: Collecting 100 samples in estimated 5.0567 s (66k iterations)
Benchmarking mempool/get_sorted/by_fee_density/5000: Analyzing
mempool/get_sorted/by_fee_density/5000
                        time:   [77.973 µs 78.430 µs 78.915 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking mempool/get_sorted/by_fee_density/10000
Benchmarking mempool/get_sorted/by_fee_density/10000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/10000: Collecting 100 samples in estimated 5.3797 s (71k iterations)
Benchmarking mempool/get_sorted/by_fee_density/10000: Analyzing
mempool/get_sorted/by_fee_density/10000
                        time:   [77.058 µs 77.462 µs 77.861 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking mempool/drain_readd/txs/1000
Benchmarking mempool/drain_readd/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, enable flat sampling, or reduce sample count to 50.
Benchmarking mempool/drain_readd/txs/1000: Collecting 100 samples in estimated 7.1644 s (5050 iterations)
Benchmarking mempool/drain_readd/txs/1000: Analyzing
mempool/drain_readd/txs/1000
                        time:   [753.74 µs 754.78 µs 755.71 µs]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/drain_readd/txs/5000
Benchmarking mempool/drain_readd/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/5000: Collecting 100 samples in estimated 5.6983 s (700 iterations)
Benchmarking mempool/drain_readd/txs/5000: Analyzing
mempool/drain_readd/txs/5000
                        time:   [4.3880 ms 4.4175 ms 4.4481 ms]
Benchmarking mempool/drain_readd/txs/10000
Benchmarking mempool/drain_readd/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/10000: Collecting 100 samples in estimated 7.1970 s (200 iterations)
Benchmarking mempool/drain_readd/txs/10000: Analyzing
mempool/drain_readd/txs/10000
                        time:   [19.202 ms 19.413 ms 19.612 ms]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  8 (8.00%) low mild
  1 (1.00%) high mild

Benchmarking mempool/batch_remove/50_from/5000
Benchmarking mempool/batch_remove/50_from/5000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/5000: Collecting 100 samples in estimated 5.0022 s (3.4M iterations)
Benchmarking mempool/batch_remove/50_from/5000: Analyzing
mempool/batch_remove/50_from/5000
                        time:   [1.4641 µs 1.4681 µs 1.4738 µs]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
Benchmarking mempool/batch_remove/50_from/10000
Benchmarking mempool/batch_remove/50_from/10000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/10000: Collecting 100 samples in estimated 5.0029 s (3.5M iterations)
Benchmarking mempool/batch_remove/50_from/10000: Analyzing
mempool/batch_remove/50_from/10000
                        time:   [1.4386 µs 1.4392 µs 1.4400 µs]
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
Benchmarking ed25519_verify/single: Collecting 100 samples in estimated 5.0596 s (172k iterations)
Benchmarking ed25519_verify/single: Analyzing
ed25519_verify/single   time:   [29.463 µs 29.642 µs 29.826 µs]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking ed25519_batch_verify/sequential/1
Benchmarking ed25519_batch_verify/sequential/1: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/1: Collecting 100 samples in estimated 5.0018 s (167k iterations)
Benchmarking ed25519_batch_verify/sequential/1: Analyzing
ed25519_batch_verify/sequential/1
                        time:   [30.000 µs 30.147 µs 30.301 µs]
Benchmarking ed25519_batch_verify/sequential/10
Benchmarking ed25519_batch_verify/sequential/10: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/10: Collecting 100 samples in estimated 6.1472 s (20k iterations)
Benchmarking ed25519_batch_verify/sequential/10: Analyzing
ed25519_batch_verify/sequential/10
                        time:   [299.23 µs 300.70 µs 302.35 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking ed25519_batch_verify/sequential/50
Benchmarking ed25519_batch_verify/sequential/50: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.5s, enable flat sampling, or reduce sample count to 50.
Benchmarking ed25519_batch_verify/sequential/50: Collecting 100 samples in estimated 7.5457 s (5050 iterations)
Benchmarking ed25519_batch_verify/sequential/50: Analyzing
ed25519_batch_verify/sequential/50
                        time:   [1.4899 ms 1.5003 ms 1.5113 ms]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking ed25519_batch_verify/sequential/100
Benchmarking ed25519_batch_verify/sequential/100: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/100: Collecting 100 samples in estimated 5.2160 s (1700 iterations)
Benchmarking ed25519_batch_verify/sequential/100: Analyzing
ed25519_batch_verify/sequential/100
                        time:   [3.0216 ms 3.0320 ms 3.0425 ms]
Benchmarking ed25519_batch_verify/sequential/200
Benchmarking ed25519_batch_verify/sequential/200: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/200: Collecting 100 samples in estimated 5.0135 s (800 iterations)
Benchmarking ed25519_batch_verify/sequential/200: Analyzing
ed25519_batch_verify/sequential/200
                        time:   [6.0870 ms 6.1166 ms 6.1474 ms]
Benchmarking ed25519_batch_verify/sequential/500
Benchmarking ed25519_batch_verify/sequential/500: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/500: Collecting 100 samples in estimated 6.1810 s (400 iterations)
Benchmarking ed25519_batch_verify/sequential/500: Analyzing
ed25519_batch_verify/sequential/500
                        time:   [15.083 ms 15.131 ms 15.181 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking keyhash_from_vkey/single
Benchmarking keyhash_from_vkey/single: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/single: Collecting 100 samples in estimated 5.0005 s (29M iterations)
Benchmarking keyhash_from_vkey/single: Analyzing
keyhash_from_vkey/single
                        time:   [175.95 ns 177.15 ns 178.71 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking keyhash_from_vkey/batch/10
Benchmarking keyhash_from_vkey/batch/10: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/10: Collecting 100 samples in estimated 5.0048 s (3.0M iterations)
Benchmarking keyhash_from_vkey/batch/10: Analyzing
keyhash_from_vkey/batch/10
                        time:   [1.6715 µs 1.6871 µs 1.7149 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/50
Benchmarking keyhash_from_vkey/batch/50: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/50: Collecting 100 samples in estimated 5.0293 s (591k iterations)
Benchmarking keyhash_from_vkey/batch/50: Analyzing
keyhash_from_vkey/batch/50
                        time:   [8.4798 µs 8.5107 µs 8.5432 µs]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
Benchmarking keyhash_from_vkey/batch/100
Benchmarking keyhash_from_vkey/batch/100: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/100: Collecting 100 samples in estimated 5.0184 s (283k iterations)
Benchmarking keyhash_from_vkey/batch/100: Analyzing
keyhash_from_vkey/batch/100
                        time:   [17.299 µs 17.442 µs 17.615 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking keyhash_from_vkey/batch/200
Benchmarking keyhash_from_vkey/batch/200: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/200: Collecting 100 samples in estimated 5.0245 s (146k iterations)
Benchmarking keyhash_from_vkey/batch/200: Analyzing
keyhash_from_vkey/batch/200
                        time:   [33.705 µs 33.983 µs 34.331 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/500
Benchmarking keyhash_from_vkey/batch/500: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/500: Collecting 100 samples in estimated 5.0197 s (61k iterations)
Benchmarking keyhash_from_vkey/batch/500: Analyzing
keyhash_from_vkey/batch/500
                        time:   [83.967 µs 84.574 µs 85.182 µs]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking vrf_verify/single_proof
Benchmarking vrf_verify/single_proof: Warming up for 3.0000 s
Benchmarking vrf_verify/single_proof: Collecting 100 samples in estimated 5.0805 s (56k iterations)
Benchmarking vrf_verify/single_proof: Analyzing
vrf_verify/single_proof time:   [91.408 µs 91.623 µs 91.894 µs]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe


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
Benchmarking blake2b_256/hash/32B_txhash: Collecting 100 samples in estimated 5.0003 s (39M iterations)
Benchmarking blake2b_256/hash/32B_txhash: Analyzing
blake2b_256/hash/32B_txhash
                        time:   [127.56 ns 127.88 ns 128.27 ns]
Benchmarking blake2b_256/hash/64B_vkey
Benchmarking blake2b_256/hash/64B_vkey: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/64B_vkey: Collecting 100 samples in estimated 5.0005 s (39M iterations)
Benchmarking blake2b_256/hash/64B_vkey: Analyzing
blake2b_256/hash/64B_vkey
                        time:   [127.73 ns 128.07 ns 128.48 ns]
Benchmarking blake2b_256/hash/256B_small_tx
Benchmarking blake2b_256/hash/256B_small_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/256B_small_tx: Collecting 100 samples in estimated 5.0008 s (22M iterations)
Benchmarking blake2b_256/hash/256B_small_tx: Analyzing
blake2b_256/hash/256B_small_tx
                        time:   [231.83 ns 232.06 ns 232.34 ns]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high mild
Benchmarking blake2b_256/hash/500B_avg_tx
Benchmarking blake2b_256/hash/500B_avg_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/500B_avg_tx: Collecting 100 samples in estimated 5.0008 s (11M iterations)
Benchmarking blake2b_256/hash/500B_avg_tx: Analyzing
blake2b_256/hash/500B_avg_tx
                        time:   [447.64 ns 449.44 ns 452.42 ns]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_256/hash/1KB_tx_body
Benchmarking blake2b_256/hash/1KB_tx_body: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/1KB_tx_body: Collecting 100 samples in estimated 5.0011 s (5.8M iterations)
Benchmarking blake2b_256/hash/1KB_tx_body: Analyzing
blake2b_256/hash/1KB_tx_body
                        time:   [869.02 ns 869.36 ns 869.80 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_256/hash/4KB_large_tx
Benchmarking blake2b_256/hash/4KB_large_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/4KB_large_tx: Collecting 100 samples in estimated 5.0028 s (1.5M iterations)
Benchmarking blake2b_256/hash/4KB_large_tx: Analyzing
blake2b_256/hash/4KB_large_tx
                        time:   [3.4221 µs 3.4231 µs 3.4242 µs]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/16KB_block_header
Benchmarking blake2b_256/hash/16KB_block_header: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/16KB_block_header: Collecting 100 samples in estimated 5.0260 s (369k iterations)
Benchmarking blake2b_256/hash/16KB_block_header: Analyzing
blake2b_256/hash/16KB_block_header
                        time:   [13.633 µs 13.635 µs 13.637 µs]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_256/hash/20KB_avg_block
Benchmarking blake2b_256/hash/20KB_avg_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/20KB_avg_block: Collecting 100 samples in estimated 5.0752 s (298k iterations)
Benchmarking blake2b_256/hash/20KB_avg_block: Analyzing
blake2b_256/hash/20KB_avg_block
                        time:   [17.039 µs 17.041 µs 17.043 µs]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking blake2b_256/hash/90KB_max_block
Benchmarking blake2b_256/hash/90KB_max_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/90KB_max_block: Collecting 100 samples in estimated 5.0288 s (66k iterations)
Benchmarking blake2b_256/hash/90KB_max_block: Analyzing
blake2b_256/hash/90KB_max_block
                        time:   [76.621 µs 76.631 µs 76.643 µs]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

Benchmarking blake2b_224/hash/32B_vkey_to_keyhash
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Collecting 100 samples in estimated 5.0000 s (37M iterations)
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Analyzing
blake2b_224/hash/32B_vkey_to_keyhash
                        time:   [134.61 ns 135.27 ns 136.05 ns]
Benchmarking blake2b_224/hash/64B_script_bytes
Benchmarking blake2b_224/hash/64B_script_bytes: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/64B_script_bytes: Collecting 100 samples in estimated 5.0001 s (37M iterations)
Benchmarking blake2b_224/hash/64B_script_bytes: Analyzing
blake2b_224/hash/64B_script_bytes
                        time:   [135.44 ns 136.06 ns 136.83 ns]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
Benchmarking blake2b_224/hash/256B_address_payload
Benchmarking blake2b_224/hash/256B_address_payload: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/256B_address_payload: Collecting 100 samples in estimated 5.0000 s (21M iterations)
Benchmarking blake2b_224/hash/256B_address_payload: Analyzing
blake2b_224/hash/256B_address_payload
                        time:   [238.23 ns 238.84 ns 239.56 ns]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high mild

Benchmarking blake2b_batch/224_keyhashes/10
Benchmarking blake2b_batch/224_keyhashes/10: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/10: Collecting 100 samples in estimated 5.0052 s (3.9M iterations)
Benchmarking blake2b_batch/224_keyhashes/10: Analyzing
blake2b_batch/224_keyhashes/10
                        time:   [1.2717 µs 1.2751 µs 1.2793 µs]
Benchmarking blake2b_batch/224_keyhashes/50
Benchmarking blake2b_batch/224_keyhashes/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/50: Collecting 100 samples in estimated 5.0264 s (783k iterations)
Benchmarking blake2b_batch/224_keyhashes/50: Analyzing
blake2b_batch/224_keyhashes/50
                        time:   [6.3594 µs 6.3776 µs 6.3994 µs]
Benchmarking blake2b_batch/224_keyhashes/100
Benchmarking blake2b_batch/224_keyhashes/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/100: Collecting 100 samples in estimated 5.0539 s (394k iterations)
Benchmarking blake2b_batch/224_keyhashes/100: Analyzing
blake2b_batch/224_keyhashes/100
                        time:   [12.726 µs 12.760 µs 12.801 µs]
Benchmarking blake2b_batch/224_keyhashes/500
Benchmarking blake2b_batch/224_keyhashes/500: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/500: Collecting 100 samples in estimated 5.1835 s (81k iterations)
Benchmarking blake2b_batch/224_keyhashes/500: Analyzing
blake2b_batch/224_keyhashes/500
                        time:   [63.633 µs 63.815 µs 64.031 µs]
Benchmarking blake2b_batch/256_txbodies_500B/50
Benchmarking blake2b_batch/256_txbodies_500B/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/50: Collecting 100 samples in estimated 5.0479 s (227k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/50: Analyzing
blake2b_batch/256_txbodies_500B/50
                        time:   [22.135 µs 22.147 µs 22.159 µs]
Found 17 outliers among 100 measurements (17.00%)
  12 (12.00%) high mild
  5 (5.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/100
Benchmarking blake2b_batch/256_txbodies_500B/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/100: Collecting 100 samples in estimated 5.1593 s (116k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/100: Analyzing
blake2b_batch/256_txbodies_500B/100
                        time:   [44.269 µs 44.289 µs 44.310 µs]
Found 17 outliers among 100 measurements (17.00%)
  9 (9.00%) high mild
  8 (8.00%) high severe
Benchmarking blake2b_batch/256_txbodies_500B/300
Benchmarking blake2b_batch/256_txbodies_500B/300: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/300: Collecting 100 samples in estimated 5.3835 s (40k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/300: Analyzing
blake2b_batch/256_txbodies_500B/300
                        time:   [132.76 µs 132.84 µs 132.92 µs]
Found 17 outliers among 100 measurements (17.00%)
  16 (16.00%) high mild
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
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Collecting 100 samples in estimated 5.0075 s (3.1M iterations)
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Analyzing
serialization/encode_transaction/conway_2in_2out_2wit
                        time:   [1.6537 µs 1.6549 µs 1.6562 µs]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking serialization/encode_transaction/body_only_2in_2out
Benchmarking serialization/encode_transaction/body_only_2in_2out: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/body_only_2in_2out: Collecting 100 samples in estimated 5.0032 s (4.7M iterations)
Benchmarking serialization/encode_transaction/body_only_2in_2out: Analyzing
serialization/encode_transaction/body_only_2in_2out
                        time:   [1.0714 µs 1.0722 µs 1.0731 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe

Benchmarking serialization/encode_block_header/with_vrf_output
Benchmarking serialization/encode_block_header/with_vrf_output: Warming up for 3.0000 s
Benchmarking serialization/encode_block_header/with_vrf_output: Collecting 100 samples in estimated 5.0003 s (5.6M iterations)
Benchmarking serialization/encode_block_header/with_vrf_output: Analyzing
serialization/encode_block_header/with_vrf_output
                        time:   [884.80 ns 885.59 ns 886.62 ns]
Found 14 outliers among 100 measurements (14.00%)
  11 (11.00%) high mild
  3 (3.00%) high severe

Benchmarking serialization/encode_value/ada_only
Benchmarking serialization/encode_value/ada_only: Warming up for 3.0000 s
Benchmarking serialization/encode_value/ada_only: Collecting 100 samples in estimated 5.0000 s (223M iterations)
Benchmarking serialization/encode_value/ada_only: Analyzing
serialization/encode_value/ada_only
                        time:   [22.219 ns 22.252 ns 22.288 ns]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking serialization/encode_value/multi_asset_3policy_5asset
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Warming up for 3.0000 s
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Collecting 100 samples in estimated 5.0014 s (7.7M iterations)
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Analyzing
serialization/encode_value/multi_asset_3policy_5asset
                        time:   [653.29 ns 653.99 ns 654.67 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe


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
[test_mainnet_scale_wal_crash_recovery] writing 100000 entries (WAL only)...
[test_mainnet_scale_delete_amplification] inserting 100000 entries...
[test_mainnet_scale_insert_read] inserting 100000 entries...
[test_mainnet_scale_insert_read] flush complete, sampling 1000 keys...
[test_mainnet_scale_wal_crash_recovery] simulating crash (drop without flush)...
[test_mainnet_scale_insert_read] verified 1000/1000 sampled keys — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
[test_mainnet_scale_delete_amplification] insert flush complete
[test_mainnet_scale_delete_amplification] deleting 80000 entries...
[test_mainnet_scale_wal_crash_recovery] reopened, verifying 100000 entries...
[test_mainnet_scale_wal_crash_recovery] all 100000 entries recovered — PASS
[test_mainnet_scale_delete_amplification] delete flush complete
[test_mainnet_scale_delete_amplification] verifying surviving 20000 entries...
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
[test_mainnet_scale_delete_amplification] verifying 1K deleted keys return None...
[test_mainnet_scale_delete_amplification] range scanning for exact count...
[test_mainnet_scale_delete_amplification] 20000 entries confirmed — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out; finished in 0.46s

   Doc-tests dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s


```

</details>

