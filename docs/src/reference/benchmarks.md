# Nightly Benchmark Results — 2026-04-26

Machine: GitHub Actions ubuntu-latest
Branch: main (f7edcc5)

## Storage Benchmarks
```
[1m[92m   Compiling[0m proc-macro2 v1.0.106
[1m[92m   Compiling[0m unicode-ident v1.0.24
[1m[92m   Compiling[0m quote v1.0.45
[1m[92m   Compiling[0m cfg-if v1.0.4
[1m[92m   Compiling[0m libc v0.2.185
[1m[92m   Compiling[0m typenum v1.19.0
[1m[92m   Compiling[0m version_check v0.9.5
[1m[92m   Compiling[0m generic-array v0.14.9
[1m[92m   Compiling[0m serde_core v1.0.228
[1m[92m   Compiling[0m syn v2.0.117
[1m[92m   Compiling[0m zerocopy v0.8.48
[1m[92m   Compiling[0m serde v1.0.228
[1m[92m   Compiling[0m subtle v2.6.1
[1m[92m   Compiling[0m autocfg v1.5.0
[1m[92m   Compiling[0m crypto-common v0.1.6
[1m[92m   Compiling[0m block-buffer v0.10.4
[1m[92m   Compiling[0m num-traits v0.2.19
[1m[92m   Compiling[0m digest v0.10.7
[1m[92m   Compiling[0m semver v1.0.28
[1m[92m   Compiling[0m getrandom v0.3.4
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m cpufeatures v0.2.17
[1m[92m   Compiling[0m strsim v0.11.1
[1m[92m   Compiling[0m ident_case v1.0.1
[1m[92m   Compiling[0m rustc_version v0.4.1
[1m[92m   Compiling[0m minicbor v0.26.5
[1m[92m   Compiling[0m thiserror v1.0.69
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m zmij v1.0.21
[1m[92m   Compiling[0m rand_core v0.9.5
[1m[92m   Compiling[0m serde_json v1.0.149
[1m[92m   Compiling[0m signature v2.2.0
[1m[92m   Compiling[0m hex v0.4.3
[1m[92m   Compiling[0m sha2 v0.10.9
[1m[92m   Compiling[0m itoa v1.0.18
[1m[92m   Compiling[0m rand_core v0.6.4
[1m[92m   Compiling[0m getrandom v0.1.16
[1m[92m   Compiling[0m memchr v2.8.0
[1m[92m   Compiling[0m cryptoxide v0.4.4
[1m[92m   Compiling[0m darling_core v0.23.0
[1m[92m   Compiling[0m zerocopy-derive v0.8.48
[1m[92m   Compiling[0m serde_derive v1.0.228
[1m[92m   Compiling[0m minicbor-derive v0.16.2
[1m[92m   Compiling[0m darling_macro v0.23.0
[1m[92m   Compiling[0m thiserror-impl v1.0.69
[1m[92m   Compiling[0m darling v0.23.0
[1m[92m   Compiling[0m curve25519-dalek-derive v0.1.1
[1m[92m   Compiling[0m serde_with_macros v3.18.0
[1m[92m   Compiling[0m serde_with v3.18.0
[1m[92m   Compiling[0m ed25519 v2.2.3
[1m[92m   Compiling[0m ed25519-dalek v2.2.0
[1m[92m   Compiling[0m either v1.15.0
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m crossbeam-utils v0.8.21
[1m[92m   Compiling[0m half v2.7.1
[1m[92m   Compiling[0m ppv-lite86 v0.2.21
[1m[92m   Compiling[0m convert_case v0.10.0
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m rand_chacha v0.9.0
[1m[92m   Compiling[0m rand v0.9.4
[1m[92m   Compiling[0m digest v0.9.0
[1m[92m   Compiling[0m hybrid-array v0.4.10
[1m[92m   Compiling[0m thiserror v2.0.18
[1m[92m   Compiling[0m unicode-xid v0.2.6
[1m[92m   Compiling[0m derive_more-impl v2.1.1
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m rand_core v0.5.1
[1m[92m   Compiling[0m pallas-codec v1.0.0-alpha.6
[1m[92m   Compiling[0m itertools v0.13.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m thiserror-impl v2.0.18
[1m[92m   Compiling[0m parking_lot_core v0.9.12
[1m[92m   Compiling[0m arrayref v0.3.9
[1m[92m   Compiling[0m find-msvc-tools v0.1.9
[1m[92m   Compiling[0m bytes v1.11.1
[1m[92m   Compiling[0m paste v1.0.15
[1m[92m   Compiling[0m constant_time_eq v0.4.2
[1m[92m   Compiling[0m rustversion v1.0.22
[1m[92m   Compiling[0m shlex v1.3.0
[1m[92m   Compiling[0m once_cell v1.21.4
[1m[92m   Compiling[0m iana-time-zone v0.1.65
[1m[92m   Compiling[0m crc-catalog v2.4.0
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m arrayvec v0.7.6
[1m[92m   Compiling[0m chrono v0.4.44
[1m[92m   Compiling[0m blake2b_simd v1.0.4
[1m[92m   Compiling[0m crc v3.4.0
[1m[92m   Compiling[0m cc v1.2.60
[1m[92m   Compiling[0m derive_more v2.1.1
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m crossbeam-epoch v0.9.18
[1m[92m   Compiling[0m block-buffer v0.12.0
[1m[92m   Compiling[0m crypto-common v0.2.1
[1m[92m   Compiling[0m blake2 v0.10.6
[1m[92m   Compiling[0m block-buffer v0.9.0
[1m[92m   Compiling[0m scopeguard v1.2.0
[1m[92m   Compiling[0m base58 v0.2.0
[1m[92m   Compiling[0m pin-project-lite v0.2.17
[1m[92m   Compiling[0m bech32 v0.9.1
[1m[92m   Compiling[0m smallvec v1.15.1
[1m[92m   Compiling[0m rayon-core v1.13.0
[1m[92m   Compiling[0m num-modular v0.6.1
[1m[92m   Compiling[0m bech32 v0.11.1
[1m[92m   Compiling[0m const-oid v0.10.2
[1m[92m   Compiling[0m opaque-debug v0.3.1
[1m[92m   Compiling[0m sha2 v0.9.9
[1m[92m   Compiling[0m digest v0.11.2
[1m[92m   Compiling[0m num-order v1.2.0
[1m[92m   Compiling[0m dugite-primitives v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m lock_api v0.4.14
[1m[92m   Compiling[0m crossbeam-deque v0.8.6
[1m[92m   Compiling[0m alloca v0.4.0
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m tracing-core v0.1.36
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m tracing-attributes v0.1.31
[1m[92m   Compiling[0m errno v0.3.14
[1m[92m   Compiling[0m clap_lex v1.1.0
[1m[92m   Compiling[0m rustix v1.1.4
[1m[92m   Compiling[0m dashu-base v0.4.1
[1m[92m   Compiling[0m crc32fast v1.5.0
[1m[92m   Compiling[0m static_assertions v1.1.0
[1m[92m   Compiling[0m plotters-backend v0.3.7
[1m[92m   Compiling[0m ciborium-io v0.2.2
[1m[92m   Compiling[0m cpufeatures v0.3.0
[1m[92m   Compiling[0m anstyle v1.0.14
[1m[92m   Compiling[0m getrandom v0.4.2
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m clap_builder v4.6.0
[1m[92m   Compiling[0m tracing v0.1.44
[1m[92m   Compiling[0m plotters-svg v0.3.7
[1m[92m   Compiling[0m sha2 v0.11.0
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m ciborium-ll v0.2.2
[1m[92m   Compiling[0m dashu-int v0.4.1
[1m[92m   Compiling[0m signal-hook-registry v1.4.8
[1m[92m   Compiling[0m vrf_dalek v0.1.0 (https://github.com/input-output-hk/vrf?rev=03ac038e9b92c754ebbcb71824866d93f25e27f3#03ac038e)
[1m[92m   Compiling[0m parking_lot v0.12.5
[1m[92m   Compiling[0m tokio-macros v2.7.0
[1m[92m   Compiling[0m mio v1.2.0
[1m[92m   Compiling[0m socket2 v0.6.3
[1m[92m   Compiling[0m bitflags v2.11.0
[1m[92m   Compiling[0m linux-raw-sys v0.12.1
[1m[92m   Compiling[0m same-file v1.0.6
[1m[92m   Compiling[0m cast v0.3.0
[1m[92m   Compiling[0m criterion-plot v0.8.2
[1m[92m   Compiling[0m walkdir v2.5.0
[1m[92m   Compiling[0m tokio v1.51.1
[1m[92m   Compiling[0m rayon v1.11.0
[1m[92m   Compiling[0m dugite-crypto v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m dugite-serialization v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m ciborium v0.2.2
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m clap v4.6.0
[1m[92m   Compiling[0m plotters v0.3.7
[1m[92m   Compiling[0m tinytemplate v1.2.1
[1m[92m   Compiling[0m memmap2 v0.9.10
[1m[92m   Compiling[0m page_size v0.6.0
[1m[92m   Compiling[0m anes v0.1.6
[1m[92m   Compiling[0m fastrand v2.4.1
[1m[92m   Compiling[0m oorandom v11.1.5
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m tempfile v3.27.0
[1m[92m   Compiling[0m dugite-storage v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-storage)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 03s
[1m[92m     Running[0m benches/storage_bench.rs (target/release/deps/storage_bench-3e5b3009c6561638)
Gnuplot not found, using plotters backend
Benchmarking chaindb/sequential_insert/10k_20kb
Benchmarking chaindb/sequential_insert/10k_20kb: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 31.5s.
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 31.498 s (10 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [2.8501 s 2.8746 s 2.9013 s]
                        change: [−41.773% −39.498% −37.856%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 277.4s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 277.40 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [32.132 ms 32.406 ms 32.698 ms]
                        change: [−0.5241% +0.5713% +1.8671%] (p = 0.35 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 2840.8s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 2840.8 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [352.60 ms 358.19 ms 364.08 ms]
                        change: [−1.2335% +1.0162% +3.2789%] (p = 0.36 > 0.05)
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 270.6s, or reduce sample count to 10.
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 270.63 s (100 iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [29.534 ms 29.661 ms 29.791 ms]
                        change: [−3.6085% −2.8251% −2.0189%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 261.7s, or reduce sample count to 10.
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 261.69 s (100 iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [29.500 ms 29.609 ms 29.719 ms]
                        change: [−5.1509% −4.5026% −3.8650%] (p = 0.00 < 0.05)
                        Performance has improved.

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 265.7s, or reduce sample count to 10.
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 265.67 s (100 iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [30.742 ms 30.900 ms 31.061 ms]
                        change: [−7.9393% −6.9819% −5.9920%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.6s.
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 5.6294 s (10 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [6.5195 ms 6.6350 ms 6.7565 ms]
                        change: [−2.4725% −0.0233% +2.7777%] (p = 0.99 > 0.05)
                        No change in performance detected.

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.3s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 26.271 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [2.6005 s 2.6232 s 2.6523 s]
                        change: [−36.048% −34.798% −33.413%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.0s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 25.967 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [2.6107 s 2.6437 s 2.6798 s]
                        change: [−42.308% −39.677% −37.040%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.2s.
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 26.217 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [29.829 ms 30.329 ms 30.876 ms]
                        change: [−2.8226% −0.2783% +2.4327%] (p = 0.84 > 0.05)
                        No change in performance detected.
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.1s.
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 26.144 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [29.721 ms 30.136 ms 30.522 ms]
                        change: [−6.6993% −4.3195% −1.7855%] (p = 0.01 < 0.05)
                        Performance has improved.

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 6.3137 s (200 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [31.843 ms 31.875 ms 31.906 ms]
                        change: [−65.682% −65.628% −65.573%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 5.3974 s (25k iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [213.56 µs 213.71 µs 213.92 µs]
                        change: [+5.5899% +6.0000% +6.3256%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 5.0618 s (1700 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [2.8466 ms 2.8543 ms 2.8623 ms]
                        change: [−21.525% −14.572% −10.301%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 32.2s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 32.158 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [261.89 ms 262.37 ms 262.87 ms]
                        change: [−69.858% −69.774% −69.693%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.5s, enable flat sampling, or reduce sample count to 50.
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 8.4893 s (5050 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [1.6822 ms 1.6837 ms 1.6855 ms]
                        change: [−14.039% −13.266% −12.470%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 5.2250 s (200 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [26.279 ms 26.312 ms 26.345 ms]
                        change: [−4.2507% −3.2002% −2.2097%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.8853 s (500 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [11.817 ms 11.838 ms 11.861 ms]
                        change: [+6.4297% +7.0633% +7.6992%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.9271 s (500 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [11.854 ms 11.874 ms 11.895 ms]
                        change: [+5.3361% +5.8908% +6.4464%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0735 s (162k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [31.729 µs 31.846 µs 31.959 µs]
                        change: [−8.9359% −8.0955% −7.3647%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0740 s (162k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [31.737 µs 31.852 µs 31.963 µs]
                        change: [−8.1023% −7.6373% −7.1666%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 6.1194 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [15.294 ms 15.307 ms 15.321 ms]
                        change: [+0.2419% +0.6455% +1.0427%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 6.4350 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [15.940 ms 15.949 ms 15.959 ms]
                        change: [+0.4233% +0.7737% +1.1267%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.1474 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [340.53 µs 341.39 µs 342.11 µs]
                        change: [+4.2730% +5.0500% +5.8330%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 5.1934 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [341.15 µs 341.56 µs 341.91 µs]
                        change: [+5.2658% +6.0035% +6.7608%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high severe

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 7.8392 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [777.00 µs 777.60 µs 778.24 µs]
                        change: [−5.1059% −4.3470% −3.8232%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.1111 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.3233 ms 6.3301 ms 6.3374 ms]
                        change: [+0.0725% +0.8707% +1.5806%] (p = 0.02 < 0.05)
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.0644 s (1500 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [3.3613 ms 3.3634 ms 3.3654 ms]
                        change: [−4.6232% −4.3094% −4.0069%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 9.3802 s (200 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [46.458 ms 46.496 ms 46.536 ms]
                        change: [+0.9833% +1.8683% +2.6551%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.4244 s (800 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [6.7559 ms 6.7621 ms 6.7687 ms]
                        change: [−12.705% −11.619% −10.543%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.1s, or reduce sample count to 50.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 9.1200 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [91.111 ms 91.186 ms 91.269 ms]
                        change: [+1.4394% +2.0704% +2.6060%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0668 s (298k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [17.052 µs 17.073 µs 17.095 µs]
                        change: [−6.4254% −5.9732% −5.5698%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0202 s (187k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [26.988 µs 27.015 µs 27.043 µs]
                        change: [−22.383% −21.120% −19.772%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0645 s (288k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [17.629 µs 17.652 µs 17.677 µs]
                        change: [−6.1531% −5.7750% −5.4788%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0468 s (242k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [20.915 µs 20.939 µs 20.962 µs]
                        change: [−11.244% −9.8248% −8.3594%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0143 s (283k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [17.788 µs 17.811 µs 17.835 µs]
                        change: [−5.5705% −5.0377% −4.5566%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0498 s (247k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [20.517 µs 20.539 µs 20.559 µs]
                        change: [−10.882% −9.4559% −7.9567%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0088 s (414k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [12.086 µs 12.103 µs 12.136 µs]
                        change: [+4.4798% +4.7558% +5.0339%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.1547 s (126k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [40.095 µs 40.219 µs 40.349 µs]
                        change: [−6.3368% −5.2897% −4.4502%] (p = 0.00 < 0.05)
                        Performance has improved.

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0117 s (6490 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [770.88 µs 772.10 µs 774.44 µs]
                        change: [−3.8357% −3.5455% −3.2891%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.2637 s (825 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.3460 ms 6.3582 ms 6.3750 ms]
                        change: [+1.2001% +1.9981% +2.8811%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.1749 s (1540 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [3.3592 ms 3.3603 ms 3.3613 ms]
                        change: [−6.6825% −4.5385% −3.1994%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 5.2130 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [46.668 ms 46.858 ms 47.182 ms]
                        change: [+2.2054% +2.6649% +3.2222%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.2080 s (770 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [6.7556 ms 6.7608 ms 6.7641 ms]
                        change: [−14.357% −10.918% −8.1314%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 9.9862 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [91.239 ms 91.421 ms 91.783 ms]
                        change: [+2.1877% +2.9548% +3.7497%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/in_memory/250000
Benchmarking scaling/block_index_insert/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/250000: Collecting 10 samples in estimated 5.3717 s (220 iterations)
Benchmarking scaling/block_index_insert/in_memory/250000: Analyzing
scaling/block_index_insert/in_memory/250000
                        time:   [24.417 ms 24.441 ms 24.476 ms]
                        change: [−21.331% −19.263% −17.169%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/block_index_insert/mmap/250000
Benchmarking scaling/block_index_insert/mmap/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/250000: Collecting 10 samples in estimated 5.6157 s (30 iterations)
Benchmarking scaling/block_index_insert/mmap/250000: Analyzing
scaling/block_index_insert/mmap/250000
                        time:   [187.50 ms 187.94 ms 188.61 ms]
                        change: [+0.8607% +1.5599% +2.1978%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/in_memory/500000
Benchmarking scaling/block_index_insert/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/500000: Collecting 10 samples in estimated 6.9932 s (110 iterations)
Benchmarking scaling/block_index_insert/in_memory/500000: Analyzing
scaling/block_index_insert/in_memory/500000
                        time:   [63.809 ms 63.852 ms 63.901 ms]
                        change: [−11.869% −10.464% −9.1066%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/500000
Benchmarking scaling/block_index_insert/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/500000: Collecting 10 samples in estimated 6.5029 s (20 iterations)
Benchmarking scaling/block_index_insert/mmap/500000: Analyzing
scaling/block_index_insert/mmap/500000
                        time:   [324.91 ms 325.39 ms 325.88 ms]
                        change: [−1.6701% −0.8747% −0.1160%] (p = 0.05 < 0.05)
                        Change within noise threshold.
Benchmarking scaling/block_index_insert/in_memory/1000000
Benchmarking scaling/block_index_insert/in_memory/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.0s or enable flat sampling.
Benchmarking scaling/block_index_insert/in_memory/1000000: Collecting 10 samples in estimated 8.0192 s (55 iterations)
Benchmarking scaling/block_index_insert/in_memory/1000000: Analyzing
scaling/block_index_insert/in_memory/1000000
                        time:   [144.99 ms 145.05 ms 145.14 ms]
                        change: [−12.570% −9.7198% −7.6918%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/1000000
Benchmarking scaling/block_index_insert/mmap/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.7s.
Benchmarking scaling/block_index_insert/mmap/1000000: Collecting 10 samples in estimated 5.7254 s (10 iterations)
Benchmarking scaling/block_index_insert/mmap/1000000: Analyzing
scaling/block_index_insert/mmap/1000000
                        time:   [572.39 ms 573.14 ms 573.81 ms]
                        change: [+1.0557% +1.6771% +2.2660%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0007 s (290k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [17.039 µs 17.055 µs 17.089 µs]
                        change: [−5.5330% −5.2194% −4.9159%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0006 s (185k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [26.791 µs 26.801 µs 26.821 µs]
                        change: [−10.959% −10.436% −10.081%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0010 s (280k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [17.656 µs 17.661 µs 17.667 µs]
                        change: [−5.5789% −5.3477% −5.0479%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0001 s (240k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [20.800 µs 20.821 µs 20.841 µs]
                        change: [+1.4878% +1.9642% +2.2878%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0006 s (278k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [17.769 µs 17.776 µs 17.795 µs]
                        change: [−4.9999% −4.7292% −4.4245%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0010 s (244k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [20.426 µs 20.456 µs 20.490 µs]
                        change: [+1.8758% +2.4388% +2.8717%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 10 measurements (30.00%)
  2 (20.00%) low mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/250000
Benchmarking scaling/block_index_lookup/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/250000: Collecting 10 samples in estimated 5.0007 s (277k iterations)
Benchmarking scaling/block_index_lookup/in_memory/250000: Analyzing
scaling/block_index_lookup/in_memory/250000
                        time:   [17.893 µs 17.899 µs 17.903 µs]
                        change: [−5.2382% −4.9103% −4.6022%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/250000
Benchmarking scaling/block_index_lookup/mmap/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/250000: Collecting 10 samples in estimated 5.0006 s (246k iterations)
Benchmarking scaling/block_index_lookup/mmap/250000: Analyzing
scaling/block_index_lookup/mmap/250000
                        time:   [20.264 µs 20.285 µs 20.302 µs]
                        change: [+1.9024% +2.3725% +2.6517%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_lookup/in_memory/500000
Benchmarking scaling/block_index_lookup/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/500000: Collecting 10 samples in estimated 5.0004 s (277k iterations)
Benchmarking scaling/block_index_lookup/in_memory/500000: Analyzing
scaling/block_index_lookup/in_memory/500000
                        time:   [17.840 µs 17.852 µs 17.865 µs]
                        change: [−11.964% −11.703% −11.447%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/500000
Benchmarking scaling/block_index_lookup/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/500000: Collecting 10 samples in estimated 5.0001 s (249k iterations)
Benchmarking scaling/block_index_lookup/mmap/500000: Analyzing
scaling/block_index_lookup/mmap/500000
                        time:   [20.023 µs 20.027 µs 20.031 µs]
                        change: [+2.1664% +2.6438% +2.9162%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/1000000
Benchmarking scaling/block_index_lookup/in_memory/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/1000000: Collecting 10 samples in estimated 5.0002 s (259k iterations)
Benchmarking scaling/block_index_lookup/in_memory/1000000: Analyzing
scaling/block_index_lookup/in_memory/1000000
                        time:   [19.086 µs 19.101 µs 19.131 µs]
                        change: [+1.3192% +1.5478% +1.7964%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/1000000
Benchmarking scaling/block_index_lookup/mmap/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/1000000: Collecting 10 samples in estimated 5.0008 s (251k iterations)
Benchmarking scaling/block_index_lookup/mmap/1000000: Analyzing
scaling/block_index_lookup/mmap/1000000
                        time:   [19.945 µs 19.957 µs 19.968 µs]
                        change: [+2.1148% +2.5867% +2.8768%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.2314 s (165 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [31.400 ms 31.483 ms 31.645 ms]
                        change: [−66.794% −66.323% −66.008%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.0046 s (24k iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [211.76 µs 211.87 µs 212.03 µs]
                        change: [+0.8198% +1.4557% +1.8916%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.4s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 7.4226 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [134.09 ms 134.21 ms 134.29 ms]
                        change: [−69.698% −69.595% −69.503%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.0270 s (6050 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [830.77 µs 830.96 µs 831.40 µs]
                        change: [−1.8573% −0.8523% +0.1685%] (p = 0.14 > 0.05)
                        No change in performance detected.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 6.4058 s (20 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [265.66 ms 265.96 ms 266.26 ms]
                        change: [−69.487% −69.411% −69.328%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 5.0893 s (3025 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [1.6790 ms 1.6803 ms 1.6824 ms]
                        change: [−19.883% −18.284% −16.585%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/in_memory/250000
Benchmarking scaling/immutabledb_open/in_memory/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.0s.
Benchmarking scaling/immutabledb_open/in_memory/250000: Collecting 10 samples in estimated 12.976 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/250000: Analyzing
scaling/immutabledb_open/in_memory/250000
                        time:   [779.47 ms 783.74 ms 788.87 ms]
                        change: [−64.012% −63.814% −63.569%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/250000
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Collecting 10 samples in estimated 5.1779 s (1045 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Analyzing
scaling/immutabledb_open/mmap_cached/250000
                        time:   [4.9834 ms 5.0030 ms 5.0155 ms]
                        change: [−12.377% −11.770% −11.156%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/in_memory/500000
Benchmarking scaling/immutabledb_open/in_memory/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 49.1s.
Benchmarking scaling/immutabledb_open/in_memory/500000: Collecting 10 samples in estimated 49.126 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/500000: Analyzing
scaling/immutabledb_open/in_memory/500000
                        time:   [22.787 s 23.100 s 23.534 s]
                        change: [−2.7982% −0.4044% +2.0932%] (p = 0.78 > 0.05)
                        No change in performance detected.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/500000
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Collecting 10 samples in estimated 5.2375 s (605 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Analyzing
scaling/immutabledb_open/mmap_cached/500000
                        time:   [8.6501 ms 8.6605 ms 8.6722 ms]
                        change: [−13.798% −13.195% −12.609%] (p = 0.00 < 0.05)
                        Performance has improved.

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.6s.
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 26.610 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [2.5923 s 2.6081 s 2.6297 s]
                        change: [−36.872% −35.460% −33.932%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/chaindb_insert/default_20kb/50000
Benchmarking scaling/chaindb_insert/default_20kb/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 138.2s.
Benchmarking scaling/chaindb_insert/default_20kb/50000: Collecting 10 samples in estimated 138.20 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/50000: Analyzing
scaling/chaindb_insert/default_20kb/50000
                        time:   [13.567 s 13.638 s 13.713 s]
                        change: [−33.492% −32.566% −31.664%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/chaindb_insert/default_20kb/100000
Benchmarking scaling/chaindb_insert/default_20kb/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 271.8s.
Benchmarking scaling/chaindb_insert/default_20kb/100000: Collecting 10 samples in estimated 271.83 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/100000: Analyzing
scaling/chaindb_insert/default_20kb/100000
                        time:   [26.716 s 26.819 s 26.949 s]
                        change: [−35.029% −34.361% −33.751%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/chaindb_insert/default_20kb/250000
Benchmarking scaling/chaindb_insert/default_20kb/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 680.8s.
Benchmarking scaling/chaindb_insert/default_20kb/250000: Collecting 10 samples in estimated 680.78 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/250000: Analyzing
scaling/chaindb_insert/default_20kb/250000
                        time:   [67.105 s 67.395 s 67.689 s]
                        change: [−34.302% −33.715% −33.171%] (p = 0.00 < 0.05)
                        Performance has improved.

```

## UTxO Benchmarks
```
[1m[92m   Compiling[0m zeroize_derive v1.4.3
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m minicbor v0.25.1
[1m[92m   Compiling[0m minicbor-derive v0.15.3
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m secp256k1-sys v0.8.2
[1m[92m   Compiling[0m log v0.4.29
[1m[92m   Compiling[0m radium v0.7.0
[1m[92m   Compiling[0m ed25519-dalek v2.2.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m pallas-codec v0.33.0
[1m[92m   Compiling[0m pallas-crypto v0.33.0
[1m[92m   Compiling[0m blst v0.3.16
[1m[92m   Compiling[0m indexmap v1.9.3
[1m[92m   Compiling[0m num_cpus v1.17.0
[1m[92m   Compiling[0m peg-runtime v0.8.5
[1m[92m   Compiling[0m heck v0.5.0
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m tap v1.0.1
[1m[92m   Compiling[0m dugite-primitives v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m wyz v0.5.1
[1m[92m   Compiling[0m strum_macros v0.26.4
[1m[92m   Compiling[0m peg-macros v0.8.5
[1m[92m   Compiling[0m threadpool v1.8.1
[1m[92m   Compiling[0m pallas-primitives v0.33.0
[1m[92m   Compiling[0m pallas-addresses v0.33.0
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m miette-derive v5.10.0
[1m[92m   Compiling[0m typed-arena v2.0.2
[1m[92m   Compiling[0m unicode-width v0.1.14
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m hashbrown v0.12.3
[1m[92m   Compiling[0m arrayvec v0.5.2
[1m[92m   Compiling[0m funty v2.0.0
[1m[92m   Compiling[0m bitvec v1.0.1
[1m[92m   Compiling[0m pretty v0.11.3
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m peg v0.8.5
[1m[92m   Compiling[0m pallas-traverse v0.33.0
[1m[92m   Compiling[0m miette v5.10.0
[1m[92m   Compiling[0m vrf_dalek v0.1.0 (https://github.com/input-output-hk/vrf?rev=03ac038e9b92c754ebbcb71824866d93f25e27f3#03ac038e)
[1m[92m   Compiling[0m secp256k1 v0.26.0
[1m[92m   Compiling[0m strum v0.26.3
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m itertools v0.10.5
[1m[92m   Compiling[0m wait-timeout v0.2.1
[1m[92m   Compiling[0m fs2 v0.4.3
[1m[92m   Compiling[0m hamming v0.1.3
[1m[92m   Compiling[0m quick-error v1.2.3
[1m[92m   Compiling[0m bit-vec v0.8.0
[1m[92m   Compiling[0m fnv v1.0.7
[1m[92m   Compiling[0m rusty-fork v0.3.1
[1m[92m   Compiling[0m bit-set v0.8.0
[1m[92m   Compiling[0m uplc v1.1.21 (https://github.com/aiken-lang/aiken.git?rev=6806d52211fbb469ae13aa0e0290aeb6b9b3e8cf#6806d522)
[1m[92m   Compiling[0m dugite-lsm v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-lsm)
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m dugite-crypto v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m dugite-serialization v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m rand_xorshift v0.4.0
[1m[92m   Compiling[0m bincode v1.3.3
[1m[92m   Compiling[0m unarray v0.1.4
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m proptest v1.11.0
[1m[92m   Compiling[0m dugite-ledger v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-ledger)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 07s
[1m[92m     Running[0m benches/utxo_bench.rs (target/release/deps/utxo_bench-1a193bb5e664a02b)
Gnuplot not found, using plotters backend
Benchmarking utxo_store/insert/default/1000000
Benchmarking utxo_store/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 31.4s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 31.441 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.9814 s 3.0015 s 3.0192 s]
                        change: [+14.928% +15.888% +16.672%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 6.1345 s (15k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [390.11 µs 392.26 µs 394.34 µs]
                        change: [−34.606% −33.413% −32.501%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.2182 s (25k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [206.42 µs 206.62 µs 206.96 µs]
                        change: [−39.701% −39.394% −39.049%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 5.5543 s (20k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [273.05 µs 273.23 µs 273.51 µs]
                        change: [−41.399% −41.177% −40.951%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.1359 s (25k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [202.38 µs 203.58 µs 205.17 µs]
                        change: [−39.739% −39.382% −38.976%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  3 (3.00%) high mild
  5 (5.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 53.9s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 53.933 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.8047 s 2.8223 s 2.8497 s]
                        change: [+3.3429% +4.3641% +5.6407%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.8s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 28.774 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [296.33 ms 313.47 ms 328.66 ms]
                        change: [−3.7400% +1.5848% +6.7166%] (p = 0.61 > 0.05)
                        No change in performance detected.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.5s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 28.518 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [321.52 ms 330.50 ms 339.13 ms]
                        change: [+13.439% +17.423% +21.106%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 37.2s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 37.248 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [3.5485 s 3.5620 s 3.5737 s]
                        change: [+5.0716% +5.7606% +6.4387%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 36.1s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 36.084 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [124.90 ms 135.28 ms 145.63 ms]
                        change: [−2.5898% +9.6482% +20.950%] (p = 0.13 > 0.05)
                        No change in performance detected.

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 27.7s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 27.736 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [271.82 ms 272.81 ms 273.79 ms]
                        change: [−8.2002% −7.5866% −6.9617%] (p = 0.00 < 0.05)
                        Performance has improved.

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 38.5s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 38.506 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [485.80 ms 487.87 ms 489.63 ms]
                        change: [−7.4907% −6.5195% −5.6202%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.6s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 28.561 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.8323 s 2.8433 s 2.8537 s]
                        change: [+8.6624% +9.7215% +10.643%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.7s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 28.719 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.8462 s 2.8631 s 2.8822 s]
                        change: [+10.412% +11.164% +11.979%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.3s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 28.323 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.8349 s 2.8431 s 2.8510 s]
                        change: [+10.825% +11.308% +11.764%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.3s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 28.311 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.8348 s 2.8470 s 2.8585 s]
                        change: [+11.695% +12.420% +13.129%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.3s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 28.278 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.8205 s 2.8304 s 2.8401 s]
                        change: [+11.480% +11.975% +12.471%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 5.0116 s (15k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [336.17 µs 337.95 µs 339.71 µs]
                        change: [−23.407% −22.853% −22.282%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 6.6518 s (20k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [334.44 µs 335.99 µs 337.52 µs]
                        change: [−24.380% −23.767% −23.160%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 6.5278 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [329.39 µs 331.04 µs 332.68 µs]
                        change: [−25.289% −24.768% −24.197%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 6.6176 s (20k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [333.84 µs 335.81 µs 337.84 µs]
                        change: [−23.860% −23.220% −22.513%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 6.6406 s (20k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [333.37 µs 335.23 µs 337.01 µs]
                        change: [−24.397% −23.917% −23.387%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 6.7239 s (30 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [221.84 ms 222.61 ms 223.38 ms]
                        change: [+4.9226% +5.7630% +6.5284%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.4s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 13.410 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [1.3234 s 1.3337 s 1.3423 s]
                        change: [+10.265% +11.296% +12.260%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low mild
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.7s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 28.699 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.8361 s 2.8505 s 2.8673 s]
                        change: [+11.415% +12.194% +12.972%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0059 s (18k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [262.36 µs 263.57 µs 265.31 µs]
                        change: [−28.033% −27.282% −26.332%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  2 (20.00%) low severe
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0016 s (16k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [309.04 µs 310.32 µs 311.24 µs]
                        change: [−26.673% −26.006% −25.198%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0093 s (15k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [329.16 µs 330.04 µs 331.82 µs]
                        change: [−27.016% −26.128% −25.091%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 6.6891 s (30 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [13.841 ms 13.931 ms 14.033 ms]
                        change: [−29.735% −27.351% −24.392%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.4s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 13.357 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [145.55 ms 153.45 ms 159.41 ms]
                        change: [+11.104% +17.185% +23.191%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.9s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 28.947 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [323.05 ms 331.46 ms 339.25 ms]
                        change: [+16.448% +20.218% +23.780%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.4192 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [24.057 ms 24.125 ms 24.194 ms]
                        change: [−17.531% −15.849% −14.429%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.6s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 7.6267 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [138.32 ms 138.39 ms 138.44 ms]
                        change: [−1.1819% +0.3492% +1.3667%] (p = 0.66 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 5.4143 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [267.01 ms 269.09 ms 270.24 ms]
                        change: [−7.0882% −5.6722% −4.6242%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 170.9s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 170.90 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [17.067 s 17.107 s 17.145 s]
                        change: [+9.3335% +9.8771% +10.356%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 377.0s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 377.00 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [36.564 s 36.620 s 36.674 s]
                        change: [+10.308% +10.733% +11.183%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 10 measurements (30.00%)
  2 (20.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0507 s (3850 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.2916 ms 1.2932 ms 1.2954 ms]
                        change: [−0.4471% +0.3813% +1.3753%] (p = 0.50 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0893 s (3025 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.6446 ms 1.6485 ms 1.6535 ms]
                        change: [+0.3289% +0.7549% +1.2733%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
  1 (10.00%) high severe

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 16.1s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 16.085 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.5948 s 1.6053 s 1.6109 s]
                        change: [−2.1951% −0.8941% +0.0653%] (p = 0.18 > 0.05)
                        No change in performance detected.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 32.0s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 31.960 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [3.1729 s 3.1964 s 3.2090 s]
                        change: [−0.6304% +1.0258% +2.1986%] (p = 0.21 > 0.05)
                        No change in performance detected.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe

```

## LSM Stress Tests
```
[1m[92m   Compiling[0m syn v2.0.117
[1m[92m   Compiling[0m serde_core v1.0.228
[1m[92m   Compiling[0m num-traits v0.2.19
[1m[92m   Compiling[0m serde v1.0.228
[1m[92m   Compiling[0m once_cell v1.21.4
[1m[92m   Compiling[0m tempfile v3.27.0
[1m[92m   Compiling[0m rusty-fork v0.3.1
[1m[92m   Compiling[0m plotters v0.3.7
[1m[92m   Compiling[0m rand v0.9.4
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m serde_json v1.0.149
[1m[92m   Compiling[0m zerocopy-derive v0.8.48
[1m[92m   Compiling[0m serde_derive v1.0.228
[1m[92m   Compiling[0m thiserror-impl v2.0.18
[1m[92m   Compiling[0m thiserror v2.0.18
[1m[92m   Compiling[0m dugite-lsm v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-lsm)
[1m[92m   Compiling[0m zerocopy v0.8.48
[1m[92m   Compiling[0m tinytemplate v1.2.1
[1m[92m   Compiling[0m half v2.7.1
[1m[92m   Compiling[0m ppv-lite86 v0.2.21
[1m[92m   Compiling[0m ciborium-ll v0.2.2
[1m[92m   Compiling[0m rand_chacha v0.9.0
[1m[92m   Compiling[0m ciborium v0.2.2
[1m[92m   Compiling[0m proptest v1.11.0
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m    Finished[0m `release` profile [optimized] target(s) in 29.08s
[1m[92m     Running[0m unittests src/lib.rs (target/release/deps/dugite_lsm-70eb9ee18bad5445)

running 3 tests
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 8.67s

[1m[92m   Doc-tests[0m dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

```
