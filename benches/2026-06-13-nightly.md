# Nightly Benchmark Results — 2026-06-13

Captured on **2026-06-13** from commit [`fd2d240`](https://github.com/MichaelJFazio/dugite/commit/fd2d240) on `main` (GitHub Actions `ubuntu-latest`).

> **Interactive reports**, including per-benchmark detail pages and historical trend lines, are published at <https://michaeljfazio.github.io/dugite/benchmarks/>. Each section below also links directly to its interactive report.

> The collapsed _Raw measurements_ blocks contain the filtered `cargo bench` output (cargo build chatter and ANSI escapes are stripped). Full unfiltered logs are uploaded as the `benchmark-results-100` workflow artifact.

---

## Storage

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking chaindb/sequential_insert/10k_20kb
Benchmarking chaindb/sequential_insert/10k_20kb: Warming up for 3.0000 s
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 5.1760 s (20 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [251.21 ms 253.75 ms 256.49 ms]

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 6.1148 s (20k iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [303.23 µs 304.35 µs 305.72 µs]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 6.4697 s (20k iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [321.74 µs 322.35 µs 323.30 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 5.0000 s (16B iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [311.32 ps 311.45 ps 311.59 ps]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 5.1205 s (212k iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [26.913 µs 27.928 µs 28.963 µs]

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 5.0012 s (20M iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [248.77 ns 249.01 ns 249.22 ns]
Found 17 outliers among 100 measurements (17.00%)
  2 (2.00%) low severe
  11 (11.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 5.8258 s (110 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [6.7014 ms 6.7940 ms 6.9020 ms]

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 5.2022 s (20 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [256.10 ms 259.45 ms 262.99 ms]
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 5.0718 s (20 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [254.26 ms 256.99 ms 259.59 ms]
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 5.1403 s (20 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [33.461 ms 33.887 ms 34.446 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 5.1978 s (20 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [33.454 ms 33.946 ms 34.419 ms]

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.3s, or reduce sample count to 50.
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 9.2655 s (100 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [92.828 ms 93.132 ms 93.572 ms]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 5.7437 s (30k iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [189.61 µs 189.95 µs 190.43 µs]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 5.1887 s (1600 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [3.2267 ms 3.2480 ms 3.2741 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 100.7s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 100.69 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [872.96 ms 873.92 ms 875.03 ms]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.9s, enable flat sampling, or reduce sample count to 50.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 8.8922 s (5050 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [1.7459 ms 1.7517 ms 1.7578 ms]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 5.4990 s (200 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [26.790 ms 26.920 ms 27.063 ms]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.4641 s (500 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [10.864 ms 10.901 ms 10.941 ms]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.4755 s (500 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [10.922 ms 10.960 ms 11.001 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0021 s (141k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [33.009 µs 33.442 µs 33.901 µs]
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0006 s (141k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [33.035 µs 33.464 µs 33.911 µs]

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 6.0228 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [14.899 ms 14.930 ms 14.964 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 6.2630 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [15.430 ms 15.465 ms 15.502 ms]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 6.5305 s (20k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [322.88 µs 323.34 µs 323.86 µs]
Found 15 outliers among 100 measurements (15.00%)
  7 (7.00%) low severe
  5 (5.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 6.5258 s (20k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [322.48 µs 323.32 µs 324.40 µs]
Found 19 outliers among 100 measurements (19.00%)
  5 (5.00%) low severe
  7 (7.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 7.9798 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [799.57 µs 803.32 µs 807.77 µs]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.4952 s (900 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.0574 ms 6.0857 ms 6.1155 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.1406 s (1500 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [3.4607 ms 3.4688 ms 3.4769 ms]
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 7.7894 s (200 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [38.940 ms 39.009 ms 39.080 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.0173 s (700 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [7.0708 ms 7.0959 ms 7.1224 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.8s, or reduce sample count to 60.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 7.7645 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [77.335 ms 77.544 ms 77.766 ms]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0937 s (273k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [18.183 µs 18.212 µs 18.244 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0699 s (167k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [29.383 µs 29.614 µs 29.834 µs]
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0721 s (268k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [18.715 µs 18.743 µs 18.773 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0339 s (242k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [20.169 µs 20.298 µs 20.416 µs]
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0196 s (263k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [18.829 µs 18.862 µs 18.897 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0150 s (247k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [19.681 µs 19.807 µs 19.923 µs]

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0177 s (404k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [12.436 µs 12.446 µs 12.457 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.1528 s (131k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [41.376 µs 42.012 µs 42.619 µs]

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0293 s (6325 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [798.65 µs 802.75 µs 808.13 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.0266 s (825 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.1103 ms 6.1263 ms 6.1454 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.0143 s (1430 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [3.5100 ms 3.5125 ms 3.5150 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 6.3924 s (165 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [38.892 ms 39.162 ms 39.754 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.1248 s (715 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [7.1406 ms 7.1578 ms 7.1819 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 8.4748 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [77.295 ms 77.480 ms 77.779 ms]

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0009 s (275k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [18.524 µs 18.532 µs 18.537 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0016 s (172k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [30.393 µs 30.490 µs 30.642 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0006 s (269k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [18.970 µs 19.032 µs 19.135 µs]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0004 s (246k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [20.765 µs 20.772 µs 20.778 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0000 s (268k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [18.976 µs 18.985 µs 18.994 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0001 s (252k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [20.279 µs 20.305 µs 20.332 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.1s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.1420 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [93.604 ms 93.812 ms 94.051 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.0090 s (27k iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [186.09 µs 186.27 µs 186.47 µs]
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 8.8044 s (20 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [437.92 ms 438.13 ms 438.36 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.0051 s (6435 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [767.89 µs 769.15 µs 769.98 µs]
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 10.4s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 10.353 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [868.75 ms 870.65 ms 872.96 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 5.0687 s (2915 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [1.7389 ms 1.7664 ms 1.8057 ms]

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 7.4553 s (30 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [246.45 ms 248.17 ms 249.79 ms]


```

</details>

## Ledger (UTxO)

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking utxo_store/insert/default/1000000
Benchmarking utxo_store/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.9s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 25.893 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.5272 s 2.5448 s 2.5615 s]

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 5.8486 s (10k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [574.03 µs 576.05 µs 578.10 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.0801 s (15k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [331.76 µs 332.38 µs 333.05 µs]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 6.8657 s (15k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [455.63 µs 456.16 µs 456.73 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.0278 s (15k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [330.16 µs 330.57 µs 331.01 µs]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 50.6s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 50.576 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.5928 s 2.6127 s 2.6333 s]

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.3s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 26.296 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [306.65 ms 314.64 ms 321.80 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low mild
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.2s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 26.169 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [283.14 ms 288.50 ms 293.57 ms]

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 35.5s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 35.532 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [3.3625 s 3.3739 s 3.3842 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 34.0s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 33.951 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [116.74 ms 122.42 ms 128.70 ms]

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 26.6s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 26.604 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [272.96 ms 274.41 ms 275.88 ms]

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 35.0s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 34.986 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [506.58 ms 514.80 ms 523.12 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.3s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 25.278 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.5209 s 2.5334 s 2.5459 s]
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.2s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 25.230 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.5129 s 2.5235 s 2.5339 s]
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 24.9s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 24.944 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.4894 s 2.4957 s 2.5020 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 24.9s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 24.894 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.4944 s 2.5042 s 2.5141 s]
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.0s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 24.978 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.4762 s 2.4818 s 2.4877 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 6.6478 s (15k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [451.35 µs 452.99 µs 454.50 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 6.6235 s (15k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [447.87 µs 449.42 µs 450.89 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 6.6045 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [446.57 µs 448.25 µs 449.83 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 6.6156 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [450.48 µs 452.11 µs 453.64 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 6.7098 s (15k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [456.99 µs 458.96 µs 460.88 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 6.2806 s (30 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [207.40 ms 208.34 ms 209.29 ms]
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 11.7s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 11.692 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [1.1674 s 1.1702 s 1.1732 s]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.1s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 25.140 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.5069 s 2.5155 s 2.5266 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0047 s (13k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [356.94 µs 357.84 µs 359.37 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0052 s (12k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [404.12 µs 404.59 µs 405.13 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0062 s (11k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [438.79 µs 439.00 µs 439.40 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 6.2550 s (30 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [16.059 ms 16.812 ms 17.590 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 11.8s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 11.834 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [114.16 ms 117.45 ms 120.70 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 25.3s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 25.308 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [261.22 ms 263.34 ms 265.37 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.6384 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [25.990 ms 26.303 ms 26.583 ms]
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.4s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 7.3668 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [134.61 ms 134.70 ms 134.83 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 5.3200 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [265.79 ms 267.49 ms 270.27 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 154.1s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 154.12 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [15.201 s 15.251 s 15.306 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 332.1s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 332.09 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [31.842 s 31.981 s 32.130 s]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  2 (20.00%) high mild

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0005 s (3905 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.2597 ms 1.2669 ms 1.2725 ms]
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0217 s (3190 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.5486 ms 1.5501 ms 1.5513 ms]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high severe

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 15.7s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 15.714 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.6153 s 1.6176 s 1.6200 s]
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 30.7s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 30.658 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [3.1623 s 3.1688 s 3.1740 s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low severe

Benchmarking ledger/apply_block/apply_only_shelley_50tx
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Collecting 20 samples in estimated 5.0100 s (13k iterations)
Benchmarking ledger/apply_block/apply_only_shelley_50tx: Analyzing
ledger/apply_block/apply_only_shelley_50tx
                        time:   [370.73 µs 372.17 µs 373.35 µs]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high mild
Benchmarking ledger/apply_block/validate_all_shelley_50tx
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Warming up for 3.0000 s
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Collecting 20 samples in estimated 5.0628 s (12k iterations)
Benchmarking ledger/apply_block/validate_all_shelley_50tx: Analyzing
ledger/apply_block/validate_all_shelley_50tx
                        time:   [432.14 µs 434.70 µs 437.72 µs]


```

</details>

## Network

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking network/handshake_encode/n2n_version_data
Benchmarking network/handshake_encode/n2n_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2n_version_data: Collecting 100 samples in estimated 5.0001 s (111M iterations)
Benchmarking network/handshake_encode/n2n_version_data: Analyzing
network/handshake_encode/n2n_version_data
                        time:   [45.135 ns 45.222 ns 45.352 ns]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking network/handshake_encode/n2c_version_data
Benchmarking network/handshake_encode/n2c_version_data: Warming up for 3.0000 s
Benchmarking network/handshake_encode/n2c_version_data: Collecting 100 samples in estimated 5.0000 s (175M iterations)
Benchmarking network/handshake_encode/n2c_version_data: Analyzing
network/handshake_encode/n2c_version_data
                        time:   [28.280 ns 28.302 ns 28.331 ns]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe

Benchmarking network/chainsync/roll_forward/encode/256
Benchmarking network/chainsync/roll_forward/encode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/256: Collecting 100 samples in estimated 5.0000 s (34M iterations)
Benchmarking network/chainsync/roll_forward/encode/256: Analyzing
network/chainsync/roll_forward/encode/256
                        time:   [145.50 ns 146.76 ns 148.38 ns]
                        thrpt:  [1.9520 GiB/s 1.9736 GiB/s 1.9907 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) high mild
  11 (11.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/256
Benchmarking network/chainsync/roll_forward/decode/256: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/256: Collecting 100 samples in estimated 5.0002 s (49M iterations)
Benchmarking network/chainsync/roll_forward/decode/256: Analyzing
network/chainsync/roll_forward/decode/256
                        time:   [102.22 ns 102.32 ns 102.45 ns]
                        thrpt:  [2.8272 GiB/s 2.8307 GiB/s 2.8336 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/1024
Benchmarking network/chainsync/roll_forward/encode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/1024: Collecting 100 samples in estimated 5.0005 s (27M iterations)
Benchmarking network/chainsync/roll_forward/encode/1024: Analyzing
network/chainsync/roll_forward/encode/1024
                        time:   [187.19 ns 187.32 ns 187.46 ns]
                        thrpt:  [5.3607 GiB/s 5.3645 GiB/s 5.3684 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/1024
Benchmarking network/chainsync/roll_forward/decode/1024: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/1024: Collecting 100 samples in estimated 5.0001 s (44M iterations)
Benchmarking network/chainsync/roll_forward/decode/1024: Analyzing
network/chainsync/roll_forward/decode/1024
                        time:   [112.18 ns 112.51 ns 113.12 ns]
                        thrpt:  [8.8832 GiB/s 8.9313 GiB/s 8.9582 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
Benchmarking network/chainsync/roll_forward/encode/4096
Benchmarking network/chainsync/roll_forward/encode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/encode/4096: Collecting 100 samples in estimated 5.0009 s (18M iterations)
Benchmarking network/chainsync/roll_forward/encode/4096: Analyzing
network/chainsync/roll_forward/encode/4096
                        time:   [270.42 ns 270.61 ns 270.83 ns]
                        thrpt:  [14.274 GiB/s 14.286 GiB/s 14.296 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking network/chainsync/roll_forward/decode/4096
Benchmarking network/chainsync/roll_forward/decode/4096: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_forward/decode/4096: Collecting 100 samples in estimated 5.0008 s (26M iterations)
Benchmarking network/chainsync/roll_forward/decode/4096: Analyzing
network/chainsync/roll_forward/decode/4096
                        time:   [188.13 ns 188.59 ns 189.24 ns]
                        thrpt:  [20.429 GiB/s 20.499 GiB/s 20.549 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  8 (8.00%) high severe

Benchmarking network/chainsync/roll_backward/encode
Benchmarking network/chainsync/roll_backward/encode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/encode: Collecting 100 samples in estimated 5.0004 s (28M iterations)
Benchmarking network/chainsync/roll_backward/encode: Analyzing
network/chainsync/roll_backward/encode
                        time:   [179.03 ns 179.36 ns 179.94 ns]
                        thrpt:  [466.39 MiB/s 467.91 MiB/s 468.76 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking network/chainsync/roll_backward/decode
Benchmarking network/chainsync/roll_backward/decode: Warming up for 3.0000 s
Benchmarking network/chainsync/roll_backward/decode: Collecting 100 samples in estimated 5.0003 s (72M iterations)
Benchmarking network/chainsync/roll_backward/decode: Analyzing
network/chainsync/roll_backward/decode
                        time:   [70.061 ns 70.417 ns 70.758 ns]
                        thrpt:  [1.1583 GiB/s 1.1639 GiB/s 1.1698 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking network/blockfetch/msg_block/encode/2048
Benchmarking network/blockfetch/msg_block/encode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/2048: Collecting 100 samples in estimated 5.0005 s (41M iterations)
Benchmarking network/blockfetch/msg_block/encode/2048: Analyzing
network/blockfetch/msg_block/encode/2048
                        time:   [123.87 ns 124.02 ns 124.15 ns]
                        thrpt:  [15.401 GiB/s 15.417 GiB/s 15.436 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking network/blockfetch/msg_block/decode/2048
Benchmarking network/blockfetch/msg_block/decode/2048: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/2048: Collecting 100 samples in estimated 5.0000 s (51M iterations)
Benchmarking network/blockfetch/msg_block/decode/2048: Analyzing
network/blockfetch/msg_block/decode/2048
                        time:   [98.451 ns 98.794 ns 99.197 ns]
                        thrpt:  [19.275 GiB/s 19.353 GiB/s 19.421 GiB/s]
Found 17 outliers among 100 measurements (17.00%)
  16 (16.00%) high mild
  1 (1.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/20480
Benchmarking network/blockfetch/msg_block/encode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/20480: Collecting 100 samples in estimated 5.0009 s (8.2M iterations)
Benchmarking network/blockfetch/msg_block/encode/20480: Analyzing
network/blockfetch/msg_block/encode/20480
                        time:   [607.27 ns 607.51 ns 607.80 ns]
                        thrpt:  [31.389 GiB/s 31.404 GiB/s 31.416 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/20480
Benchmarking network/blockfetch/msg_block/decode/20480: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/20480: Collecting 100 samples in estimated 5.0182 s (651k iterations)
Benchmarking network/blockfetch/msg_block/decode/20480: Analyzing
network/blockfetch/msg_block/decode/20480
                        time:   [7.6811 µs 7.6840 µs 7.6874 µs]
                        thrpt:  [2.4818 GiB/s 2.4828 GiB/s 2.4838 GiB/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe
Benchmarking network/blockfetch/msg_block/encode/90000
Benchmarking network/blockfetch/msg_block/encode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/encode/90000: Collecting 100 samples in estimated 5.0012 s (2.2M iterations)
Benchmarking network/blockfetch/msg_block/encode/90000: Analyzing
network/blockfetch/msg_block/encode/90000
                        time:   [2.3247 µs 2.3251 µs 2.3255 µs]
                        thrpt:  [36.046 GiB/s 36.052 GiB/s 36.058 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking network/blockfetch/msg_block/decode/90000
Benchmarking network/blockfetch/msg_block/decode/90000: Warming up for 3.0000 s
Benchmarking network/blockfetch/msg_block/decode/90000: Collecting 100 samples in estimated 5.0028 s (2.2M iterations)
Benchmarking network/blockfetch/msg_block/decode/90000: Analyzing
network/blockfetch/msg_block/decode/90000
                        time:   [2.3129 µs 2.3135 µs 2.3141 µs]
                        thrpt:  [36.223 GiB/s 36.234 GiB/s 36.242 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

Benchmarking network/blockfetch/request_range/encode
Benchmarking network/blockfetch/request_range/encode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/encode: Collecting 100 samples in estimated 5.0004 s (44M iterations)
Benchmarking network/blockfetch/request_range/encode: Analyzing
network/blockfetch/request_range/encode
                        time:   [113.99 ns 114.12 ns 114.27 ns]
Found 16 outliers among 100 measurements (16.00%)
  11 (11.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking network/blockfetch/request_range/decode
Benchmarking network/blockfetch/request_range/decode: Warming up for 3.0000 s
Benchmarking network/blockfetch/request_range/decode: Collecting 100 samples in estimated 5.0002 s (78M iterations)
Benchmarking network/blockfetch/request_range/decode: Analyzing
network/blockfetch/request_range/decode
                        time:   [64.273 ns 64.347 ns 64.427 ns]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe

Benchmarking network/n2c_query_encode/pparams/hfc_success
Benchmarking network/n2c_query_encode/pparams/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/hfc_success: Collecting 100 samples in estimated 5.0003 s (91M iterations)
Benchmarking network/n2c_query_encode/pparams/hfc_success: Analyzing
network/n2c_query_encode/pparams/hfc_success
                        time:   [54.890 ns 54.954 ns 55.017 ns]
                        thrpt:  [1.5404 GiB/s 1.5422 GiB/s 1.5440 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/pparams/tag24
Benchmarking network/n2c_query_encode/pparams/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/pparams/tag24: Collecting 100 samples in estimated 5.0002 s (83M iterations)
Benchmarking network/n2c_query_encode/pparams/tag24: Analyzing
network/n2c_query_encode/pparams/tag24
                        time:   [59.757 ns 59.802 ns 59.846 ns]
                        thrpt:  [1.4162 GiB/s 1.4172 GiB/s 1.4182 GiB/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/govstate/hfc_success
Benchmarking network/n2c_query_encode/govstate/hfc_success: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/hfc_success: Collecting 100 samples in estimated 5.0002 s (58M iterations)
Benchmarking network/n2c_query_encode/govstate/hfc_success: Analyzing
network/n2c_query_encode/govstate/hfc_success
                        time:   [86.721 ns 86.863 ns 87.011 ns]
                        thrpt:  [8.3702 GiB/s 8.3844 GiB/s 8.3981 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/govstate/tag24
Benchmarking network/n2c_query_encode/govstate/tag24: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/govstate/tag24: Collecting 100 samples in estimated 5.0004 s (53M iterations)
Benchmarking network/n2c_query_encode/govstate/tag24: Analyzing
network/n2c_query_encode/govstate/tag24
                        time:   [94.245 ns 94.287 ns 94.336 ns]
                        thrpt:  [7.7202 GiB/s 7.7242 GiB/s 7.7277 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking network/n2c_query_encode/era_mismatch
Benchmarking network/n2c_query_encode/era_mismatch: Warming up for 3.0000 s
Benchmarking network/n2c_query_encode/era_mismatch: Collecting 100 samples in estimated 5.0000 s (176M iterations)
Benchmarking network/n2c_query_encode/era_mismatch: Analyzing
network/n2c_query_encode/era_mismatch
                        time:   [28.321 ns 28.330 ns 28.339 ns]
                        thrpt:  [25.699 GiB/s 25.708 GiB/s 25.716 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe


```

</details>

## Consensus

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Collecting 100 samples in estimated 5.1367 s (126k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.0000247: Analyzing
consensus/vrf_leader_check/single/sigma=0.0000247
                        time:   [40.499 µs 40.521 µs 40.547 µs]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.001
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Collecting 100 samples in estimated 5.1260 s (126k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.001: Analyzing
consensus/vrf_leader_check/single/sigma=0.001
                        time:   [40.508 µs 40.533 µs 40.563 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking consensus/vrf_leader_check/single/sigma=0.01
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Warming up for 3.0000 s
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Collecting 100 samples in estimated 5.1308 s (126k iterations)
Benchmarking consensus/vrf_leader_check/single/sigma=0.01: Analyzing
consensus/vrf_leader_check/single/sigma=0.01
                        time:   [40.532 µs 40.553 µs 40.577 µs]
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe
Benchmarking consensus/vrf_leader_check/batch/21600
Benchmarking consensus/vrf_leader_check/batch/21600: Warming up for 3.0000 s

Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 17.7s, or reduce sample count to 10.
Benchmarking consensus/vrf_leader_check/batch/21600: Collecting 20 samples in estimated 17.714 s (20 iterations)
Benchmarking consensus/vrf_leader_check/batch/21600: Analyzing
consensus/vrf_leader_check/batch/21600
                        time:   [884.12 ms 890.15 ms 897.33 ms]
                        thrpt:  [24.071 Kelem/s 24.265 Kelem/s 24.431 Kelem/s]
Found 5 outliers among 20 measurements (25.00%)
  2 (10.00%) high mild
  3 (15.00%) high severe

Benchmarking consensus/validate_header/replay_mode
Benchmarking consensus/validate_header/replay_mode: Warming up for 3.0000 s
Benchmarking consensus/validate_header/replay_mode: Collecting 100 samples in estimated 5.0001 s (442M iterations)
Benchmarking consensus/validate_header/replay_mode: Analyzing
consensus/validate_header/replay_mode
                        time:   [11.264 ns 11.274 ns 11.286 ns]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

Benchmarking consensus/chain_selection/longer_fork_100
Benchmarking consensus/chain_selection/longer_fork_100: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/longer_fork_100: Collecting 100 samples in estimated 5.0000 s (2.8B iterations)
Benchmarking consensus/chain_selection/longer_fork_100: Analyzing
consensus/chain_selection/longer_fork_100
                        time:   [1.7582 ns 1.7625 ns 1.7692 ns]
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe
Benchmarking consensus/chain_selection/equal_length_tiebreak
Benchmarking consensus/chain_selection/equal_length_tiebreak: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/equal_length_tiebreak: Collecting 100 samples in estimated 5.0016 s (15M iterations)
Benchmarking consensus/chain_selection/equal_length_tiebreak: Analyzing
consensus/chain_selection/equal_length_tiebreak
                        time:   [343.50 ns 344.29 ns 345.41 ns]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking consensus/chain_selection/prefer_simple
Benchmarking consensus/chain_selection/prefer_simple: Warming up for 3.0000 s
Benchmarking consensus/chain_selection/prefer_simple: Collecting 100 samples in estimated 5.0000 s (1.9B iterations)
Benchmarking consensus/chain_selection/prefer_simple: Analyzing
consensus/chain_selection/prefer_simple
                        time:   [2.6564 ns 2.6624 ns 2.6680 ns]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild


```

</details>

## LSM

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking lsm/insert/random_keys/10000
Benchmarking lsm/insert/random_keys/10000: Warming up for 3.0000 s
Benchmarking lsm/insert/random_keys/10000: Collecting 10 samples in estimated 5.0897 s (1485 iterations)
Benchmarking lsm/insert/random_keys/10000: Analyzing
lsm/insert/random_keys/10000
                        time:   [3.2913 ms 3.2947 ms 3.2998 ms]
                        thrpt:  [3.0305 Melem/s 3.0352 Melem/s 3.0383 Melem/s]

Benchmarking lsm/point_lookup/hit_random/10000
Benchmarking lsm/point_lookup/hit_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/hit_random/10000: Collecting 100 samples in estimated 6.2823 s (15k iterations)
Benchmarking lsm/point_lookup/hit_random/10000: Analyzing
lsm/point_lookup/hit_random/10000
                        time:   [414.43 µs 415.03 µs 415.90 µs]
                        thrpt:  [2.4044 Melem/s 2.4094 Melem/s 2.4130 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking lsm/point_lookup/miss_random/10000
Benchmarking lsm/point_lookup/miss_random/10000: Warming up for 3.0000 s
Benchmarking lsm/point_lookup/miss_random/10000: Collecting 100 samples in estimated 5.1258 s (56k iterations)
Benchmarking lsm/point_lookup/miss_random/10000: Analyzing
lsm/point_lookup/miss_random/10000
                        time:   [92.043 µs 92.507 µs 93.229 µs]
                        thrpt:  [10.726 Melem/s 10.810 Melem/s 10.864 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

Benchmarking lsm/range_scan/window_100_of_10k/10000
Benchmarking lsm/range_scan/window_100_of_10k/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/window_100_of_10k/10000: Collecting 20 samples in estimated 5.0121 s (82k iterations)
Benchmarking lsm/range_scan/window_100_of_10k/10000: Analyzing
lsm/range_scan/window_100_of_10k/10000
                        time:   [60.302 µs 60.360 µs 60.432 µs]
                        thrpt:  [1.6547 Melem/s 1.6567 Melem/s 1.6583 Melem/s]
Found 5 outliers among 20 measurements (25.00%)
  1 (5.00%) low severe
  2 (10.00%) low mild
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking lsm/range_scan/full_scan/10000
Benchmarking lsm/range_scan/full_scan/10000: Warming up for 3.0000 s
Benchmarking lsm/range_scan/full_scan/10000: Collecting 20 samples in estimated 5.2637 s (3150 iterations)
Benchmarking lsm/range_scan/full_scan/10000: Analyzing
lsm/range_scan/full_scan/10000
                        time:   [1.6544 ms 1.6583 ms 1.6616 ms]
                        thrpt:  [6.0184 Melem/s 6.0303 Melem/s 6.0447 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Warming up for 3.0000 s
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Collecting 10 samples in estimated 5.4666 s (440 iterations)
Benchmarking lsm/apply_batch/inserts_10k_deletes_2.5k/10000: Analyzing
lsm/apply_batch/inserts_10k_deletes_2.5k/10000
                        time:   [5.4799 ms 5.4980 ms 5.5080 ms]
                        thrpt:  [1.8155 Melem/s 1.8189 Melem/s 1.8248 Melem/s]

Benchmarking lsm/snapshot/save_10k
Benchmarking lsm/snapshot/save_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/save_10k: Collecting 10 samples in estimated 5.3601 s (770 iterations)
Benchmarking lsm/snapshot/save_10k: Analyzing
lsm/snapshot/save_10k   time:   [542.88 µs 545.43 µs 547.88 µs]
Benchmarking lsm/snapshot/load_10k
Benchmarking lsm/snapshot/load_10k: Warming up for 3.0000 s
Benchmarking lsm/snapshot/load_10k: Collecting 10 samples in estimated 5.1213 s (605 iterations)
Benchmarking lsm/snapshot/load_10k: Analyzing
lsm/snapshot/load_10k   time:   [1.7251 ms 1.7310 ms 1.7408 ms]


```

</details>

## Mempool

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking mempool/add/txs/1000
Benchmarking mempool/add/txs/1000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/1000: Collecting 100 samples in estimated 9.9946 s (10k iterations)
Benchmarking mempool/add/txs/1000: Analyzing
mempool/add/txs/1000    time:   [924.08 µs 924.63 µs 925.21 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/add/txs/5000
Benchmarking mempool/add/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/5000: Collecting 100 samples in estimated 5.4436 s (600 iterations)
Benchmarking mempool/add/txs/5000: Analyzing
mempool/add/txs/5000    time:   [7.9985 ms 8.1440 ms 8.2867 ms]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
Benchmarking mempool/add/txs/10000
Benchmarking mempool/add/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/add/txs/10000: Collecting 100 samples in estimated 6.3264 s (400 iterations)
Benchmarking mempool/add/txs/10000: Analyzing
mempool/add/txs/10000   time:   [14.464 ms 14.890 ms 15.341 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mempool/remove/txs/1000
Benchmarking mempool/remove/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.6s, enable flat sampling, or reduce sample count to 60.
Benchmarking mempool/remove/txs/1000: Collecting 100 samples in estimated 6.6339 s (5050 iterations)
Benchmarking mempool/remove/txs/1000: Analyzing
mempool/remove/txs/1000 time:   [515.81 µs 516.66 µs 517.54 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/remove/txs/5000
Benchmarking mempool/remove/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/5000: Collecting 100 samples in estimated 5.1641 s (700 iterations)
Benchmarking mempool/remove/txs/5000: Analyzing
mempool/remove/txs/5000 time:   [2.8955 ms 2.9015 ms 2.9077 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking mempool/remove/txs/10000
Benchmarking mempool/remove/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/remove/txs/10000: Collecting 100 samples in estimated 5.1183 s (300 iterations)
Benchmarking mempool/remove/txs/10000: Analyzing
mempool/remove/txs/10000
                        time:   [6.5236 ms 6.5508 ms 6.5783 ms]

Benchmarking mempool/get_sorted/by_fee_density/1000
Benchmarking mempool/get_sorted/by_fee_density/1000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/1000: Collecting 100 samples in estimated 5.0941 s (91k iterations)
Benchmarking mempool/get_sorted/by_fee_density/1000: Analyzing
mempool/get_sorted/by_fee_density/1000
                        time:   [54.352 µs 54.749 µs 55.205 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking mempool/get_sorted/by_fee_density/5000
Benchmarking mempool/get_sorted/by_fee_density/5000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/5000: Collecting 100 samples in estimated 5.2446 s (56k iterations)
Benchmarking mempool/get_sorted/by_fee_density/5000: Analyzing
mempool/get_sorted/by_fee_density/5000
                        time:   [93.611 µs 93.740 µs 93.884 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mempool/get_sorted/by_fee_density/10000
Benchmarking mempool/get_sorted/by_fee_density/10000: Warming up for 3.0000 s
Benchmarking mempool/get_sorted/by_fee_density/10000: Collecting 100 samples in estimated 5.0328 s (81k iterations)
Benchmarking mempool/get_sorted/by_fee_density/10000: Analyzing
mempool/get_sorted/by_fee_density/10000
                        time:   [61.229 µs 61.485 µs 61.774 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking mempool/drain_readd/txs/1000
Benchmarking mempool/drain_readd/txs/1000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.6s, enable flat sampling, or reduce sample count to 50.
Benchmarking mempool/drain_readd/txs/1000: Collecting 100 samples in estimated 8.6312 s (5050 iterations)
Benchmarking mempool/drain_readd/txs/1000: Analyzing
mempool/drain_readd/txs/1000
                        time:   [902.48 µs 903.86 µs 905.17 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking mempool/drain_readd/txs/5000
Benchmarking mempool/drain_readd/txs/5000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/5000: Collecting 100 samples in estimated 5.7902 s (600 iterations)
Benchmarking mempool/drain_readd/txs/5000: Analyzing
mempool/drain_readd/txs/5000
                        time:   [5.2096 ms 5.2619 ms 5.3173 ms]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
Benchmarking mempool/drain_readd/txs/10000
Benchmarking mempool/drain_readd/txs/10000: Warming up for 3.0000 s
Benchmarking mempool/drain_readd/txs/10000: Collecting 100 samples in estimated 6.1046 s (200 iterations)
Benchmarking mempool/drain_readd/txs/10000: Analyzing
mempool/drain_readd/txs/10000
                        time:   [15.498 ms 15.616 ms 15.735 ms]

Benchmarking mempool/batch_remove/50_from/5000
Benchmarking mempool/batch_remove/50_from/5000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/5000: Collecting 100 samples in estimated 5.0028 s (3.3M iterations)
Benchmarking mempool/batch_remove/50_from/5000: Analyzing
mempool/batch_remove/50_from/5000
                        time:   [1.5055 µs 1.5061 µs 1.5067 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking mempool/batch_remove/50_from/10000
Benchmarking mempool/batch_remove/50_from/10000: Warming up for 3.0000 s
Benchmarking mempool/batch_remove/50_from/10000: Collecting 100 samples in estimated 5.0040 s (3.2M iterations)
Benchmarking mempool/batch_remove/50_from/10000: Analyzing
mempool/batch_remove/50_from/10000
                        time:   [1.5595 µs 1.5603 µs 1.5614 µs]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe


```

</details>

## Crypto

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking ed25519_verify/single
Benchmarking ed25519_verify/single: Warming up for 3.0000 s
Benchmarking ed25519_verify/single: Collecting 100 samples in estimated 5.1511 s (136k iterations)
Benchmarking ed25519_verify/single: Analyzing
ed25519_verify/single   time:   [37.454 µs 37.493 µs 37.537 µs]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

Benchmarking ed25519_batch_verify/sequential/1
Benchmarking ed25519_batch_verify/sequential/1: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/1: Collecting 100 samples in estimated 5.0064 s (106k iterations)
Benchmarking ed25519_batch_verify/sequential/1: Analyzing
ed25519_batch_verify/sequential/1
                        time:   [42.760 µs 43.485 µs 44.309 µs]
Benchmarking ed25519_batch_verify/sequential/10
Benchmarking ed25519_batch_verify/sequential/10: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/10: Collecting 100 samples in estimated 7.1296 s (15k iterations)
Benchmarking ed25519_batch_verify/sequential/10: Analyzing
ed25519_batch_verify/sequential/10
                        time:   [422.45 µs 429.44 µs 437.40 µs]
Benchmarking ed25519_batch_verify/sequential/50
Benchmarking ed25519_batch_verify/sequential/50: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/50: Collecting 100 samples in estimated 5.1818 s (2200 iterations)
Benchmarking ed25519_batch_verify/sequential/50: Analyzing
ed25519_batch_verify/sequential/50
                        time:   [2.1581 ms 2.1807 ms 2.2023 ms]
Benchmarking ed25519_batch_verify/sequential/100
Benchmarking ed25519_batch_verify/sequential/100: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/100: Collecting 100 samples in estimated 5.1800 s (1100 iterations)
Benchmarking ed25519_batch_verify/sequential/100: Analyzing
ed25519_batch_verify/sequential/100
                        time:   [4.3155 ms 4.3602 ms 4.4028 ms]
Benchmarking ed25519_batch_verify/sequential/200
Benchmarking ed25519_batch_verify/sequential/200: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/200: Collecting 100 samples in estimated 5.7341 s (600 iterations)
Benchmarking ed25519_batch_verify/sequential/200: Analyzing
ed25519_batch_verify/sequential/200
                        time:   [8.7809 ms 8.8763 ms 8.9709 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking ed25519_batch_verify/sequential/500
Benchmarking ed25519_batch_verify/sequential/500: Warming up for 3.0000 s
Benchmarking ed25519_batch_verify/sequential/500: Collecting 100 samples in estimated 7.2063 s (300 iterations)
Benchmarking ed25519_batch_verify/sequential/500: Analyzing
ed25519_batch_verify/sequential/500
                        time:   [22.128 ms 22.354 ms 22.570 ms]

Benchmarking keyhash_from_vkey/single
Benchmarking keyhash_from_vkey/single: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/single: Collecting 100 samples in estimated 5.0008 s (29M iterations)
Benchmarking keyhash_from_vkey/single: Analyzing
keyhash_from_vkey/single
                        time:   [172.91 ns 173.18 ns 173.50 ns]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking keyhash_from_vkey/batch/10
Benchmarking keyhash_from_vkey/batch/10: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/10: Collecting 100 samples in estimated 5.0004 s (3.0M iterations)
Benchmarking keyhash_from_vkey/batch/10: Analyzing
keyhash_from_vkey/batch/10
                        time:   [1.6514 µs 1.6532 µs 1.6553 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking keyhash_from_vkey/batch/50
Benchmarking keyhash_from_vkey/batch/50: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/50: Collecting 100 samples in estimated 5.0246 s (611k iterations)
Benchmarking keyhash_from_vkey/batch/50: Analyzing
keyhash_from_vkey/batch/50
                        time:   [8.1898 µs 8.1941 µs 8.1988 µs]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/100
Benchmarking keyhash_from_vkey/batch/100: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/100: Collecting 100 samples in estimated 5.0717 s (308k iterations)
Benchmarking keyhash_from_vkey/batch/100: Analyzing
keyhash_from_vkey/batch/100
                        time:   [16.400 µs 16.411 µs 16.424 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking keyhash_from_vkey/batch/200
Benchmarking keyhash_from_vkey/batch/200: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/200: Collecting 100 samples in estimated 5.1377 s (157k iterations)
Benchmarking keyhash_from_vkey/batch/200: Analyzing
keyhash_from_vkey/batch/200
                        time:   [32.764 µs 32.784 µs 32.812 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking keyhash_from_vkey/batch/500
Benchmarking keyhash_from_vkey/batch/500: Warming up for 3.0000 s
Benchmarking keyhash_from_vkey/batch/500: Collecting 100 samples in estimated 5.3876 s (66k iterations)
Benchmarking keyhash_from_vkey/batch/500: Analyzing
keyhash_from_vkey/batch/500
                        time:   [81.915 µs 82.050 µs 82.269 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking vrf_verify/single_proof
Benchmarking vrf_verify/single_proof: Warming up for 3.0000 s
Benchmarking vrf_verify/single_proof: Collecting 100 samples in estimated 5.1678 s (30k iterations)
Benchmarking vrf_verify/single_proof: Analyzing
vrf_verify/single_proof time:   [170.10 µs 170.15 µs 170.22 µs]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe


```

</details>

## Primitives

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking blake2b_256/hash/32B_txhash
Benchmarking blake2b_256/hash/32B_txhash: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/32B_txhash: Collecting 100 samples in estimated 5.0003 s (32M iterations)
Benchmarking blake2b_256/hash/32B_txhash: Analyzing
blake2b_256/hash/32B_txhash
                        time:   [157.41 ns 157.73 ns 158.11 ns]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
Benchmarking blake2b_256/hash/64B_vkey
Benchmarking blake2b_256/hash/64B_vkey: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/64B_vkey: Collecting 100 samples in estimated 5.0005 s (32M iterations)
Benchmarking blake2b_256/hash/64B_vkey: Analyzing
blake2b_256/hash/64B_vkey
                        time:   [158.24 ns 158.82 ns 159.38 ns]
Benchmarking blake2b_256/hash/256B_small_tx
Benchmarking blake2b_256/hash/256B_small_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/256B_small_tx: Collecting 100 samples in estimated 5.0007 s (18M iterations)
Benchmarking blake2b_256/hash/256B_small_tx: Analyzing
blake2b_256/hash/256B_small_tx
                        time:   [277.13 ns 277.41 ns 277.74 ns]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking blake2b_256/hash/500B_avg_tx
Benchmarking blake2b_256/hash/500B_avg_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/500B_avg_tx: Collecting 100 samples in estimated 5.0007 s (9.3M iterations)
Benchmarking blake2b_256/hash/500B_avg_tx: Analyzing
blake2b_256/hash/500B_avg_tx
                        time:   [534.94 ns 535.36 ns 535.84 ns]
Found 17 outliers among 100 measurements (17.00%)
  15 (15.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/1KB_tx_body
Benchmarking blake2b_256/hash/1KB_tx_body: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/1KB_tx_body: Collecting 100 samples in estimated 5.0007 s (4.8M iterations)
Benchmarking blake2b_256/hash/1KB_tx_body: Analyzing
blake2b_256/hash/1KB_tx_body
                        time:   [1.0469 µs 1.0473 µs 1.0478 µs]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_256/hash/4KB_large_tx
Benchmarking blake2b_256/hash/4KB_large_tx: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/4KB_large_tx: Collecting 100 samples in estimated 5.0047 s (1.2M iterations)
Benchmarking blake2b_256/hash/4KB_large_tx: Analyzing
blake2b_256/hash/4KB_large_tx
                        time:   [4.1230 µs 4.1329 µs 4.1464 µs]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
Benchmarking blake2b_256/hash/16KB_block_header
Benchmarking blake2b_256/hash/16KB_block_header: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/16KB_block_header: Collecting 100 samples in estimated 5.0553 s (308k iterations)
Benchmarking blake2b_256/hash/16KB_block_header: Analyzing
blake2b_256/hash/16KB_block_header
                        time:   [16.412 µs 16.414 µs 16.416 µs]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking blake2b_256/hash/20KB_avg_block
Benchmarking blake2b_256/hash/20KB_avg_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/20KB_avg_block: Collecting 100 samples in estimated 5.0749 s (247k iterations)
Benchmarking blake2b_256/hash/20KB_avg_block: Analyzing
blake2b_256/hash/20KB_avg_block
                        time:   [20.508 µs 20.510 µs 20.513 µs]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking blake2b_256/hash/90KB_max_block
Benchmarking blake2b_256/hash/90KB_max_block: Warming up for 3.0000 s
Benchmarking blake2b_256/hash/90KB_max_block: Collecting 100 samples in estimated 5.1243 s (56k iterations)
Benchmarking blake2b_256/hash/90KB_max_block: Analyzing
blake2b_256/hash/90KB_max_block
                        time:   [92.206 µs 92.218 µs 92.234 µs]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking blake2b_224/hash/32B_vkey_to_keyhash
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Collecting 100 samples in estimated 5.0008 s (30M iterations)
Benchmarking blake2b_224/hash/32B_vkey_to_keyhash: Analyzing
blake2b_224/hash/32B_vkey_to_keyhash
                        time:   [166.78 ns 166.88 ns 167.00 ns]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low severe
  12 (12.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking blake2b_224/hash/64B_script_bytes
Benchmarking blake2b_224/hash/64B_script_bytes: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/64B_script_bytes: Collecting 100 samples in estimated 5.0008 s (30M iterations)
Benchmarking blake2b_224/hash/64B_script_bytes: Analyzing
blake2b_224/hash/64B_script_bytes
                        time:   [166.31 ns 166.45 ns 166.59 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_224/hash/256B_address_payload
Benchmarking blake2b_224/hash/256B_address_payload: Warming up for 3.0000 s
Benchmarking blake2b_224/hash/256B_address_payload: Collecting 100 samples in estimated 5.0006 s (17M iterations)
Benchmarking blake2b_224/hash/256B_address_payload: Analyzing
blake2b_224/hash/256B_address_payload
                        time:   [285.08 ns 285.31 ns 285.56 ns]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking blake2b_batch/224_keyhashes/10
Benchmarking blake2b_batch/224_keyhashes/10: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/10: Collecting 100 samples in estimated 5.0020 s (3.2M iterations)
Benchmarking blake2b_batch/224_keyhashes/10: Analyzing
blake2b_batch/224_keyhashes/10
                        time:   [1.5694 µs 1.5725 µs 1.5764 µs]
Benchmarking blake2b_batch/224_keyhashes/50
Benchmarking blake2b_batch/224_keyhashes/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/50: Collecting 100 samples in estimated 5.0305 s (641k iterations)
Benchmarking blake2b_batch/224_keyhashes/50: Analyzing
blake2b_batch/224_keyhashes/50
                        time:   [7.8417 µs 7.8582 µs 7.8787 µs]
Benchmarking blake2b_batch/224_keyhashes/100
Benchmarking blake2b_batch/224_keyhashes/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/100: Collecting 100 samples in estimated 5.0780 s (323k iterations)
Benchmarking blake2b_batch/224_keyhashes/100: Analyzing
blake2b_batch/224_keyhashes/100
                        time:   [15.691 µs 15.723 µs 15.764 µs]
Benchmarking blake2b_batch/224_keyhashes/500
Benchmarking blake2b_batch/224_keyhashes/500: Warming up for 3.0000 s
Benchmarking blake2b_batch/224_keyhashes/500: Collecting 100 samples in estimated 5.1586 s (66k iterations)
Benchmarking blake2b_batch/224_keyhashes/500: Analyzing
blake2b_batch/224_keyhashes/500
                        time:   [78.792 µs 78.940 µs 79.117 µs]
Benchmarking blake2b_batch/256_txbodies_500B/50
Benchmarking blake2b_batch/256_txbodies_500B/50: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/50: Collecting 100 samples in estimated 5.1319 s (192k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/50: Analyzing
blake2b_batch/256_txbodies_500B/50
                        time:   [26.752 µs 26.775 µs 26.802 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking blake2b_batch/256_txbodies_500B/100
Benchmarking blake2b_batch/256_txbodies_500B/100: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/100: Collecting 100 samples in estimated 5.1209 s (96k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/100: Analyzing
blake2b_batch/256_txbodies_500B/100
                        time:   [53.429 µs 53.476 µs 53.527 µs]
Benchmarking blake2b_batch/256_txbodies_500B/300
Benchmarking blake2b_batch/256_txbodies_500B/300: Warming up for 3.0000 s
Benchmarking blake2b_batch/256_txbodies_500B/300: Collecting 100 samples in estimated 5.6609 s (35k iterations)
Benchmarking blake2b_batch/256_txbodies_500B/300: Analyzing
blake2b_batch/256_txbodies_500B/300
                        time:   [160.32 µs 160.46 µs 160.61 µs]


```

</details>

## Serialization

<details>
<summary>Raw measurements</summary>

```
Gnuplot not found, using plotters backend
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Collecting 100 samples in estimated 5.0021 s (3.1M iterations)
Benchmarking serialization/encode_transaction/conway_2in_2out_2wit: Analyzing
serialization/encode_transaction/conway_2in_2out_2wit
                        time:   [1.6444 µs 1.6464 µs 1.6483 µs]
Found 17 outliers among 100 measurements (17.00%)
  16 (16.00%) high mild
  1 (1.00%) high severe
Benchmarking serialization/encode_transaction/body_only_2in_2out
Benchmarking serialization/encode_transaction/body_only_2in_2out: Warming up for 3.0000 s
Benchmarking serialization/encode_transaction/body_only_2in_2out: Collecting 100 samples in estimated 5.0035 s (4.6M iterations)
Benchmarking serialization/encode_transaction/body_only_2in_2out: Analyzing
serialization/encode_transaction/body_only_2in_2out
                        time:   [1.1535 µs 1.1595 µs 1.1657 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking serialization/encode_block_header/with_vrf_output
Benchmarking serialization/encode_block_header/with_vrf_output: Warming up for 3.0000 s
Benchmarking serialization/encode_block_header/with_vrf_output: Collecting 100 samples in estimated 5.0017 s (5.0M iterations)
Benchmarking serialization/encode_block_header/with_vrf_output: Analyzing
serialization/encode_block_header/with_vrf_output
                        time:   [994.96 ns 997.15 ns 999.48 ns]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

Benchmarking serialization/encode_value/ada_only
Benchmarking serialization/encode_value/ada_only: Warming up for 3.0000 s
Benchmarking serialization/encode_value/ada_only: Collecting 100 samples in estimated 5.0000 s (230M iterations)
Benchmarking serialization/encode_value/ada_only: Analyzing
serialization/encode_value/ada_only
                        time:   [21.521 ns 21.548 ns 21.577 ns]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
Benchmarking serialization/encode_value/multi_asset_3policy_5asset
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Warming up for 3.0000 s
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Collecting 100 samples in estimated 5.0011 s (7.2M iterations)
Benchmarking serialization/encode_value/multi_asset_3policy_5asset: Analyzing
serialization/encode_value/multi_asset_3policy_5asset
                        time:   [691.85 ns 693.69 ns 695.73 ns]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe


```

</details>

## LSM stress tests

<details>
<summary>Raw measurements</summary>

```

running 3 tests
[test_mainnet_scale_delete_amplification] inserting 100000 entries...
[test_mainnet_scale_insert_read] inserting 100000 entries...
[test_mainnet_scale_wal_crash_recovery] writing 100000 entries (WAL only)...
[test_mainnet_scale_wal_crash_recovery] simulating crash (drop without flush)...
[test_mainnet_scale_wal_crash_recovery] reopened, verifying 100000 entries...
[test_mainnet_scale_wal_crash_recovery] all 100000 entries recovered — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
[test_mainnet_scale_insert_read] flush complete, sampling 1000 keys...
[test_mainnet_scale_delete_amplification] insert flush complete
[test_mainnet_scale_delete_amplification] deleting 80000 entries...
[test_mainnet_scale_insert_read] verified 1000/1000 sampled keys — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
[test_mainnet_scale_delete_amplification] delete flush complete
[test_mainnet_scale_delete_amplification] verifying surviving 20000 entries...
[test_mainnet_scale_delete_amplification] verifying 1K deleted keys return None...
[test_mainnet_scale_delete_amplification] range scanning for exact count...
[test_mainnet_scale_delete_amplification] 20000 entries confirmed — PASS
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out; finished in 0.63s

   Doc-tests dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s


```

</details>

