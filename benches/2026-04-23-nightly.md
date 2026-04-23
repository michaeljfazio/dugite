# Nightly Benchmark Results — 2026-04-23

Machine: GitHub Actions ubuntu-latest
Branch: main (6e607aa)

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
[1m[92m   Compiling[0m zerocopy v0.8.48
[1m[92m   Compiling[0m syn v2.0.117
[1m[92m   Compiling[0m serde v1.0.228
[1m[92m   Compiling[0m subtle v2.6.1
[1m[92m   Compiling[0m autocfg v1.5.0
[1m[92m   Compiling[0m crypto-common v0.1.6
[1m[92m   Compiling[0m block-buffer v0.10.4
[1m[92m   Compiling[0m num-traits v0.2.19
[1m[92m   Compiling[0m digest v0.10.7
[1m[92m   Compiling[0m cpufeatures v0.2.17
[1m[92m   Compiling[0m semver v1.0.28
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m getrandom v0.3.4
[1m[92m   Compiling[0m strsim v0.11.1
[1m[92m   Compiling[0m ident_case v1.0.1
[1m[92m   Compiling[0m rustc_version v0.4.1
[1m[92m   Compiling[0m thiserror v1.0.69
[1m[92m   Compiling[0m minicbor v0.26.5
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m zmij v1.0.21
[1m[92m   Compiling[0m rand_core v0.9.5
[1m[92m   Compiling[0m serde_json v1.0.149
[1m[92m   Compiling[0m signature v2.2.0
[1m[92m   Compiling[0m hex v0.4.3
[1m[92m   Compiling[0m sha2 v0.10.9
[1m[92m   Compiling[0m getrandom v0.1.16
[1m[92m   Compiling[0m itoa v1.0.18
[1m[92m   Compiling[0m rand_core v0.6.4
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
[1m[92m   Compiling[0m crossbeam-utils v0.8.21
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m either v1.15.0
[1m[92m   Compiling[0m convert_case v0.10.0
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m digest v0.9.0
[1m[92m   Compiling[0m hybrid-array v0.4.10
[1m[92m   Compiling[0m thiserror v2.0.18
[1m[92m   Compiling[0m half v2.7.1
[1m[92m   Compiling[0m ppv-lite86 v0.2.21
[1m[92m   Compiling[0m unicode-xid v0.2.6
[1m[92m   Compiling[0m derive_more-impl v2.1.1
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m rand_chacha v0.9.0
[1m[92m   Compiling[0m rand v0.9.4
[1m[92m   Compiling[0m rand_core v0.5.1
[1m[92m   Compiling[0m pallas-codec v1.0.0-alpha.6
[1m[92m   Compiling[0m itertools v0.13.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m thiserror-impl v2.0.18
[1m[92m   Compiling[0m crc-catalog v2.4.0
[1m[92m   Compiling[0m rustversion v1.0.22
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m iana-time-zone v0.1.65
[1m[92m   Compiling[0m find-msvc-tools v0.1.9
[1m[92m   Compiling[0m arrayref v0.3.9
[1m[92m   Compiling[0m bytes v1.11.1
[1m[92m   Compiling[0m constant_time_eq v0.4.2
[1m[92m   Compiling[0m paste v1.0.15
[1m[92m   Compiling[0m parking_lot_core v0.9.12
[1m[92m   Compiling[0m arrayvec v0.7.6
[1m[92m   Compiling[0m shlex v1.3.0
[1m[92m   Compiling[0m once_cell v1.21.4
[1m[92m   Compiling[0m cc v1.2.60
[1m[92m   Compiling[0m blake2b_simd v1.0.4
[1m[92m   Compiling[0m chrono v0.4.44
[1m[92m   Compiling[0m crc v3.4.0
[1m[92m   Compiling[0m crossbeam-epoch v0.9.18
[1m[92m   Compiling[0m derive_more v2.1.1
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m block-buffer v0.12.0
[1m[92m   Compiling[0m crypto-common v0.2.1
[1m[92m   Compiling[0m blake2 v0.10.6
[1m[92m   Compiling[0m block-buffer v0.9.0
[1m[92m   Compiling[0m bech32 v0.11.1
[1m[92m   Compiling[0m rayon-core v1.13.0
[1m[92m   Compiling[0m base58 v0.2.0
[1m[92m   Compiling[0m pin-project-lite v0.2.17
[1m[92m   Compiling[0m scopeguard v1.2.0
[1m[92m   Compiling[0m bech32 v0.9.1
[1m[92m   Compiling[0m opaque-debug v0.3.1
[1m[92m   Compiling[0m const-oid v0.10.2
[1m[92m   Compiling[0m num-modular v0.6.1
[1m[92m   Compiling[0m smallvec v1.15.1
[1m[92m   Compiling[0m digest v0.11.2
[1m[92m   Compiling[0m sha2 v0.9.9
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m num-order v1.2.0
[1m[92m   Compiling[0m dugite-primitives v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m lock_api v0.4.14
[1m[92m   Compiling[0m alloca v0.4.0
[1m[92m   Compiling[0m crossbeam-deque v0.8.6
[1m[92m   Compiling[0m tracing-core v0.1.36
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m tracing-attributes v0.1.31
[1m[92m   Compiling[0m errno v0.3.14
[1m[92m   Compiling[0m ciborium-io v0.2.2
[1m[92m   Compiling[0m anstyle v1.0.14
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m dashu-base v0.4.1
[1m[92m   Compiling[0m crc32fast v1.5.0
[1m[92m   Compiling[0m rustix v1.1.4
[1m[92m   Compiling[0m clap_lex v1.1.0
[1m[92m   Compiling[0m static_assertions v1.1.0
[1m[92m   Compiling[0m plotters-backend v0.3.7
[1m[92m   Compiling[0m getrandom v0.4.2
[1m[92m   Compiling[0m cpufeatures v0.3.0
[1m[92m   Compiling[0m sha2 v0.11.0
[1m[92m   Compiling[0m plotters-svg v0.3.7
[1m[92m   Compiling[0m dashu-int v0.4.1
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m clap_builder v4.6.0
[1m[92m   Compiling[0m tracing v0.1.44
[1m[92m   Compiling[0m ciborium-ll v0.2.2
[1m[92m   Compiling[0m signal-hook-registry v1.4.8
[1m[92m   Compiling[0m vrf_dalek v0.1.0 (https://github.com/input-output-hk/vrf?rev=03ac038e9b92c754ebbcb71824866d93f25e27f3#03ac038e)
[1m[92m   Compiling[0m parking_lot v0.12.5
[1m[92m   Compiling[0m tokio-macros v2.7.0
[1m[92m   Compiling[0m mio v1.2.0
[1m[92m   Compiling[0m socket2 v0.6.3
[1m[92m   Compiling[0m cast v0.3.0
[1m[92m   Compiling[0m same-file v1.0.6
[1m[92m   Compiling[0m bitflags v2.11.0
[1m[92m   Compiling[0m linux-raw-sys v0.12.1
[1m[92m   Compiling[0m walkdir v2.5.0
[1m[92m   Compiling[0m criterion-plot v0.8.2
[1m[92m   Compiling[0m tokio v1.51.1
[1m[92m   Compiling[0m rayon v1.11.0
[1m[92m   Compiling[0m clap v4.6.0
[1m[92m   Compiling[0m dugite-crypto v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m ciborium v0.2.2
[1m[92m   Compiling[0m dugite-serialization v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m plotters v0.3.7
[1m[92m   Compiling[0m tinytemplate v1.2.1
[1m[92m   Compiling[0m memmap2 v0.9.10
[1m[92m   Compiling[0m page_size v0.6.0
[1m[92m   Compiling[0m anes v0.1.6
[1m[92m   Compiling[0m oorandom v11.1.5
[1m[92m   Compiling[0m fastrand v2.4.1
[1m[92m   Compiling[0m tempfile v3.27.0
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m dugite-storage v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-storage)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 06s
[1m[92m     Running[0m benches/storage_bench.rs (target/release/deps/storage_bench-3e5b3009c6561638)
Gnuplot not found, using plotters backend
Benchmarking chaindb/sequential_insert/10k_20kb
Benchmarking chaindb/sequential_insert/10k_20kb: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 34.2s.
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 34.165 s (10 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [3.0547 s 3.1266 s 3.2185 s]
                        change: [−37.095% −34.192% −31.529%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 300.2s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 300.16 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [33.843 ms 34.415 ms 35.105 ms]
                        change: [+4.6905% +6.8048% +9.1564%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 2952.0s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 2952.0 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [357.02 ms 362.13 ms 367.50 ms]
                        change: [+0.0359% +2.1281% +4.2147%] (p = 0.04 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 276.4s, or reduce sample count to 10.
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 276.38 s (100 iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [30.448 ms 30.637 ms 30.843 ms]
                        change: [−0.5157% +0.3724% +1.3606%] (p = 0.44 > 0.05)
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 278.9s, or reduce sample count to 10.
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 278.94 s (100 iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [30.197 ms 30.325 ms 30.456 ms]
                        change: [−2.8838% −2.1946% −1.4747%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 303.6s, or reduce sample count to 10.
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 303.60 s (100 iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [31.072 ms 31.211 ms 31.350 ms]
                        change: [−6.9853% −6.0461% −5.0788%] (p = 0.00 < 0.05)
                        Performance has improved.

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.9s.
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 5.9028 s (10 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [6.6408 ms 6.7968 ms 6.9633 ms]
                        change: [−0.4657% +2.4148% +5.6750%] (p = 0.16 > 0.05)
                        No change in performance detected.

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.3s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 27.296 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [2.7688 s 2.8280 s 2.8957 s]
                        change: [−31.690% −29.710% −27.639%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.4s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 27.404 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [2.6725 s 2.6943 s 2.7168 s]
                        change: [−41.162% −38.523% −35.924%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 26.7s.
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 26.671 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [30.938 ms 31.286 ms 31.723 ms]
                        change: [+0.5331% +2.8679% +5.2633%] (p = 0.03 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.8s.
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 28.827 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [30.891 ms 31.340 ms 31.799 ms]
                        change: [−3.0381% −0.4979% +2.2850%] (p = 0.73 > 0.05)
                        No change in performance detected.

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 6.2936 s (200 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [31.167 ms 31.205 ms 31.244 ms]
                        change: [−66.411% −66.351% −66.293%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 5.4222 s (25k iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [215.03 µs 217.77 µs 221.15 µs]
                        change: [+6.5108% +7.3535% +8.4899%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 5.0744 s (1700 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [2.9206 ms 3.0633 ms 3.3378 ms]
                        change: [−18.857% −8.3164% +3.4916%] (p = 0.14 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 32.4s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 32.399 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [263.79 ms 264.94 ms 266.37 ms]
                        change: [−69.622% −69.478% −69.320%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 5.1557 s (2600 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [2.0867 ms 2.1088 ms 2.1359 ms]
                        change: [+6.5255% +7.9437% +9.6817%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 5.4827 s (200 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [27.277 ms 27.428 ms 27.582 ms]
                        change: [−0.3254% +0.9058% +2.0529%] (p = 0.14 > 0.05)
                        No change in performance detected.

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.7784 s (500 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [11.812 ms 11.877 ms 11.944 ms]
                        change: [+6.5836% +7.4176% +8.2299%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 6.0996 s (500 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [11.495 ms 11.535 ms 11.581 ms]
                        change: [+2.2465% +2.8682% +3.5070%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0477 s (157k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [31.601 µs 31.712 µs 31.829 µs]
                        change: [−8.8296% −7.9797% −7.2650%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0488 s (157k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [31.604 µs 31.714 µs 31.831 µs]
                        change: [−8.0230% −7.6039% −7.1750%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 6.4364 s (400 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [15.968 ms 16.106 ms 16.277 ms]
                        change: [+4.8889% +5.9003% +7.0842%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.0867 s (300 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [16.800 ms 16.843 ms 16.886 ms]
                        change: [+5.9733% +6.4220% +6.8712%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 5.2792 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [347.29 µs 347.61 µs 347.94 µs]
                        change: [+6.8297% +7.6088% +8.3710%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  10 (10.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 5.2915 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [348.57 µs 349.01 µs 349.43 µs]
                        change: [+8.0116% +8.7308% +9.4646%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 7.8118 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [780.82 µs 781.30 µs 781.72 µs]
                        change: [−4.8859% −4.1290% −3.5983%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.1087 s (800 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [6.3388 ms 6.3497 ms 6.3608 ms]
                        change: [+0.3726% +1.1824% +1.9078%] (p = 0.00 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.0672 s (1500 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [3.3728 ms 3.3767 ms 3.3809 ms]
                        change: [−4.2589% −3.9285% −3.6168%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 9.5676 s (200 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [47.583 ms 47.922 ms 48.364 ms]
                        change: [+3.7948% +4.9938% +6.2689%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.1273 s (700 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [7.5526 ms 7.6658 ms 7.8110 ms]
                        change: [−1.7804% +0.1920% +2.4374%] (p = 0.86 > 0.05)
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.4s, or reduce sample count to 50.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 9.3635 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [95.749 ms 97.050 ms 98.865 ms]
                        change: [+7.0652% +8.6347% +10.933%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0631 s (293k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [17.137 µs 17.165 µs 17.192 µs]
                        change: [−6.0164% −5.5446% −5.1868%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.0756 s (187k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [27.005 µs 27.119 µs 27.259 µs]
                        change: [−22.433% −21.167% −19.827%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0428 s (283k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [17.719 µs 17.744 µs 17.767 µs]
                        change: [−5.7973% −5.4103% −5.1167%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0601 s (242k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [20.909 µs 20.937 µs 20.965 µs]
                        change: [−11.318% −9.9107% −8.4280%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0790 s (283k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [17.862 µs 17.886 µs 17.911 µs]
                        change: [−5.4480% −4.9538% −4.5834%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0897 s (247k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [20.504 µs 20.544 µs 20.591 µs]
                        change: [−10.899% −9.4585% −7.9496%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0160 s (409k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [12.281 µs 12.291 µs 12.304 µs]
                        change: [+6.1153% +6.3711% +6.6082%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.0264 s (126k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [50.947 µs 53.939 µs 56.813 µs]
                        change: [+14.557% +19.405% +24.362%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0309 s (6490 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [775.12 µs 778.26 µs 781.02 µs]
                        change: [−3.3373% −3.0124% −2.6672%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.2914 s (825 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [6.5218 ms 6.5408 ms 6.5593 ms]
                        change: [+3.7089% +4.5804% +5.5265%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.1336 s (1375 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [3.7731 ms 3.8513 ms 3.9275 ms]
                        change: [+5.2286% +8.3376% +11.211%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 5.3976 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [49.022 ms 50.090 ms 51.329 ms]
                        change: [+7.5881% +9.3579% +11.946%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.1783 s (550 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [9.1603 ms 9.1958 ms 9.2454 ms]
                        change: [+17.068% +21.829% +25.703%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.4s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 5.4204 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [96.344 ms 96.653 ms 96.976 ms]
                        change: [+8.4119% +9.3173% +10.252%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/250000
Benchmarking scaling/block_index_insert/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/250000: Collecting 10 samples in estimated 5.5207 s (165 iterations)
Benchmarking scaling/block_index_insert/in_memory/250000: Analyzing
scaling/block_index_insert/in_memory/250000
                        time:   [33.438 ms 33.722 ms 34.002 ms]
                        change: [+8.4296% +11.339% +14.308%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low mild
Benchmarking scaling/block_index_insert/mmap/250000
Benchmarking scaling/block_index_insert/mmap/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/250000: Collecting 10 samples in estimated 6.1341 s (30 iterations)
Benchmarking scaling/block_index_insert/mmap/250000: Analyzing
scaling/block_index_insert/mmap/250000
                        time:   [202.50 ms 203.07 ms 203.74 ms]
                        change: [+8.9928% +9.7392% +10.420%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/block_index_insert/in_memory/500000
Benchmarking scaling/block_index_insert/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/500000: Collecting 10 samples in estimated 8.1246 s (110 iterations)
Benchmarking scaling/block_index_insert/in_memory/500000: Analyzing
scaling/block_index_insert/in_memory/500000
                        time:   [72.256 ms 73.136 ms 74.289 ms]
                        change: [+0.6659% +2.4829% +4.3299%] (p = 0.02 < 0.05)
                        Change within noise threshold.
Benchmarking scaling/block_index_insert/mmap/500000
Benchmarking scaling/block_index_insert/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/500000: Collecting 10 samples in estimated 7.1037 s (20 iterations)
Benchmarking scaling/block_index_insert/mmap/500000: Analyzing
scaling/block_index_insert/mmap/500000
                        time:   [350.42 ms 351.23 ms 352.17 ms]
                        change: [+6.1032% +6.9978% +7.8365%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/1000000
Benchmarking scaling/block_index_insert/in_memory/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.7s or enable flat sampling.
Benchmarking scaling/block_index_insert/in_memory/1000000: Collecting 10 samples in estimated 8.7267 s (55 iterations)
Benchmarking scaling/block_index_insert/in_memory/1000000: Analyzing
scaling/block_index_insert/in_memory/1000000
                        time:   [158.19 ms 160.93 ms 164.88 ms]
                        change: [−3.8442% −0.4517% +2.1981%] (p = 0.83 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_insert/mmap/1000000
Benchmarking scaling/block_index_insert/mmap/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.1s.
Benchmarking scaling/block_index_insert/mmap/1000000: Collecting 10 samples in estimated 6.1058 s (10 iterations)
Benchmarking scaling/block_index_insert/mmap/1000000: Analyzing
scaling/block_index_insert/mmap/1000000
                        time:   [607.63 ms 608.33 ms 609.07 ms]
                        change: [+7.2588% +7.9194% +8.5461%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0002 s (291k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [17.311 µs 17.323 µs 17.343 µs]
                        change: [−4.2970% −3.9991% −3.7411%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0014 s (185k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [27.030 µs 27.046 µs 27.069 µs]
                        change: [−10.231% −9.7288% −9.4217%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0006 s (284k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [17.731 µs 17.763 µs 17.803 µs]
                        change: [−5.1325% −3.6403% −0.8533%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0003 s (238k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [20.850 µs 20.862 µs 20.875 µs]
                        change: [+1.6865% +2.1877% +2.5488%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0006 s (282k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [17.899 µs 17.930 µs 17.991 µs]
                        change: [−4.4559% −3.4498% −1.7536%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0001 s (243k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [20.424 µs 20.433 µs 20.445 µs]
                        change: [+1.8607% +2.4267% +2.8804%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/250000
Benchmarking scaling/block_index_lookup/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/250000: Collecting 10 samples in estimated 5.0008 s (278k iterations)
Benchmarking scaling/block_index_lookup/in_memory/250000: Analyzing
scaling/block_index_lookup/in_memory/250000
                        time:   [18.063 µs 18.079 µs 18.109 µs]
                        change: [−4.4649% −4.1200% −3.8164%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/250000
Benchmarking scaling/block_index_lookup/mmap/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/250000: Collecting 10 samples in estimated 5.0004 s (244k iterations)
Benchmarking scaling/block_index_lookup/mmap/250000: Analyzing
scaling/block_index_lookup/mmap/250000
                        time:   [20.310 µs 20.327 µs 20.350 µs]
                        change: [+2.1625% +2.6708% +3.0348%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/500000
Benchmarking scaling/block_index_lookup/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/500000: Collecting 10 samples in estimated 5.0009 s (280k iterations)
Benchmarking scaling/block_index_lookup/in_memory/500000: Analyzing
scaling/block_index_lookup/in_memory/500000
                        time:   [18.069 µs 18.079 µs 18.092 µs]
                        change: [−10.876% −10.683% −10.558%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/block_index_lookup/mmap/500000
Benchmarking scaling/block_index_lookup/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/500000: Collecting 10 samples in estimated 5.0001 s (247k iterations)
Benchmarking scaling/block_index_lookup/mmap/500000: Analyzing
scaling/block_index_lookup/mmap/500000
                        time:   [20.032 µs 20.042 µs 20.055 µs]
                        change: [+2.2732% +2.8291% +3.2670%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/1000000
Benchmarking scaling/block_index_lookup/in_memory/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/1000000: Collecting 10 samples in estimated 5.0000 s (280k iterations)
Benchmarking scaling/block_index_lookup/in_memory/1000000: Analyzing
scaling/block_index_lookup/in_memory/1000000
                        time:   [18.061 µs 18.113 µs 18.247 µs]
                        change: [−4.3506% −3.9024% −3.2020%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/1000000
Benchmarking scaling/block_index_lookup/mmap/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/1000000: Collecting 10 samples in estimated 5.0011 s (248k iterations)
Benchmarking scaling/block_index_lookup/mmap/1000000: Analyzing
scaling/block_index_lookup/mmap/1000000
                        time:   [19.953 µs 20.008 µs 20.143 µs]
                        change: [+2.3887% +3.0041% +3.6709%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.4143 s (165 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [32.105 ms 32.231 ms 32.376 ms]
                        change: [−65.949% −65.426% −65.017%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.0075 s (23k iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [214.60 µs 214.91 µs 215.10 µs]
                        change: [+2.0977% +2.7501% +3.1862%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.5s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 7.5437 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [136.08 ms 138.16 ms 141.00 ms]
                        change: [−69.134% −68.794% −68.333%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.0347 s (5720 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [886.01 µs 905.65 µs 924.54 µs]
                        change: [+5.5097% +7.3713% +9.3518%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 6.5164 s (20 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [266.91 ms 270.07 ms 274.44 ms]
                        change: [−69.298% −68.938% −68.436%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 5.0882 s (2365 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [2.1385 ms 2.1492 ms 2.1582 ms]
                        change: [+2.8879% +5.1053% +7.5057%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking scaling/immutabledb_open/in_memory/250000
Benchmarking scaling/immutabledb_open/in_memory/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 12.4s.
Benchmarking scaling/immutabledb_open/in_memory/250000: Collecting 10 samples in estimated 12.375 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/250000: Analyzing
scaling/immutabledb_open/in_memory/250000
                        time:   [748.60 ms 756.77 ms 767.47 ms]
                        change: [−65.424% −65.059% −64.576%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/250000
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Collecting 10 samples in estimated 5.2539 s (990 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Analyzing
scaling/immutabledb_open/mmap_cached/250000
                        time:   [5.2625 ms 5.2813 ms 5.3108 ms]
                        change: [−6.8412% −6.0995% −5.3126%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/immutabledb_open/in_memory/500000
Benchmarking scaling/immutabledb_open/in_memory/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 52.3s.
Benchmarking scaling/immutabledb_open/in_memory/500000: Collecting 10 samples in estimated 52.307 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/500000: Analyzing
scaling/immutabledb_open/in_memory/500000
                        time:   [22.893 s 23.013 s 23.180 s]
                        change: [−2.7852% −0.7783% +0.6878%] (p = 0.51 > 0.05)
                        No change in performance detected.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/500000
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Collecting 10 samples in estimated 5.0838 s (550 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Analyzing
scaling/immutabledb_open/mmap_cached/500000
                        time:   [9.1520 ms 9.3483 ms 9.6392 ms]
                        change: [−7.5426% −6.2523% −4.5277%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.8s.
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 29.791 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [2.8630 s 2.9221 s 2.9931 s]
                        change: [−29.858% −27.687% −25.376%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/chaindb_insert/default_20kb/50000
Benchmarking scaling/chaindb_insert/default_20kb/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 145.0s.
Benchmarking scaling/chaindb_insert/default_20kb/50000: Collecting 10 samples in estimated 144.98 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/50000: Analyzing
scaling/chaindb_insert/default_20kb/50000
                        time:   [14.215 s 14.405 s 14.598 s]
                        change: [−30.161% −28.771% −27.483%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/chaindb_insert/default_20kb/100000
Benchmarking scaling/chaindb_insert/default_20kb/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 290.8s.
Benchmarking scaling/chaindb_insert/default_20kb/100000: Collecting 10 samples in estimated 290.84 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/100000: Analyzing
scaling/chaindb_insert/default_20kb/100000
                        time:   [28.671 s 28.981 s 29.316 s]
                        change: [−30.064% −29.071% −28.060%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking scaling/chaindb_insert/default_20kb/250000
Benchmarking scaling/chaindb_insert/default_20kb/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 752.0s.
Benchmarking scaling/chaindb_insert/default_20kb/250000: Collecting 10 samples in estimated 751.97 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/250000: Analyzing
scaling/chaindb_insert/default_20kb/250000
                        time:   [70.893 s 71.408 s 72.041 s]
                        change: [−30.493% −29.769% −28.965%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

```

## UTxO Benchmarks
```
[1m[92m   Compiling[0m zeroize_derive v1.4.3
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m minicbor v0.25.1
[1m[92m   Compiling[0m minicbor-derive v0.15.3
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m secp256k1-sys v0.8.2
[1m[92m   Compiling[0m radium v0.7.0
[1m[92m   Compiling[0m log v0.4.29
[1m[92m   Compiling[0m ed25519-dalek v2.2.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m pallas-codec v0.33.0
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m pallas-crypto v0.33.0
[1m[92m   Compiling[0m blst v0.3.16
[1m[92m   Compiling[0m indexmap v1.9.3
[1m[92m   Compiling[0m num_cpus v1.17.0
[1m[92m   Compiling[0m tap v1.0.1
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m heck v0.5.0
[1m[92m   Compiling[0m peg-runtime v0.8.5
[1m[92m   Compiling[0m peg-macros v0.8.5
[1m[92m   Compiling[0m strum_macros v0.26.4
[1m[92m   Compiling[0m threadpool v1.8.1
[1m[92m   Compiling[0m wyz v0.5.1
[1m[92m   Compiling[0m dugite-primitives v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m pallas-primitives v0.33.0
[1m[92m   Compiling[0m pallas-addresses v0.33.0
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m miette-derive v5.10.0
[1m[92m   Compiling[0m arrayvec v0.5.2
[1m[92m   Compiling[0m unicode-width v0.1.14
[1m[92m   Compiling[0m hashbrown v0.12.3
[1m[92m   Compiling[0m funty v2.0.0
[1m[92m   Compiling[0m typed-arena v2.0.2
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m pretty v0.11.3
[1m[92m   Compiling[0m bitvec v1.0.1
[1m[92m   Compiling[0m peg v0.8.5
[1m[92m   Compiling[0m pallas-traverse v0.33.0
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m miette v5.10.0
[1m[92m   Compiling[0m vrf_dalek v0.1.0 (https://github.com/input-output-hk/vrf?rev=03ac038e9b92c754ebbcb71824866d93f25e27f3#03ac038e)
[1m[92m   Compiling[0m secp256k1 v0.26.0
[1m[92m   Compiling[0m strum v0.26.3
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m itertools v0.10.5
[1m[92m   Compiling[0m wait-timeout v0.2.1
[1m[92m   Compiling[0m fs2 v0.4.3
[1m[92m   Compiling[0m fnv v1.0.7
[1m[92m   Compiling[0m quick-error v1.2.3
[1m[92m   Compiling[0m bit-vec v0.8.0
[1m[92m   Compiling[0m hamming v0.1.3
[1m[92m   Compiling[0m uplc v1.1.21 (https://github.com/aiken-lang/aiken.git?rev=6806d52211fbb469ae13aa0e0290aeb6b9b3e8cf#6806d522)
[1m[92m   Compiling[0m bit-set v0.8.0
[1m[92m   Compiling[0m rusty-fork v0.3.1
[1m[92m   Compiling[0m dugite-lsm v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-lsm)
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m dugite-serialization v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m dugite-crypto v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m rand_xorshift v0.4.0
[1m[92m   Compiling[0m bincode v1.3.3
[1m[92m   Compiling[0m unarray v0.1.4
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m proptest v1.11.0
[1m[92m   Compiling[0m dugite-ledger v1.1.0-alpha (/home/runner/work/dugite/dugite/crates/dugite-ledger)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 09s
[1m[92m     Running[0m benches/utxo_bench.rs (target/release/deps/utxo_bench-1a193bb5e664a02b)
Gnuplot not found, using plotters backend
Benchmarking utxo_store/insert/default/1000000
Benchmarking utxo_store/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 31.3s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 31.324 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [3.0084 s 3.0229 s 3.0375 s]
                        change: [+15.951% +16.713% +17.467%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 6.5056 s (15k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [422.33 µs 424.92 µs 428.36 µs]
                        change: [−29.080% −27.794% −26.812%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.6096 s (25k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [222.22 µs 222.51 µs 222.79 µs]
                        change: [−35.204% −34.928% −34.689%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 6.2807 s (20k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [310.11 µs 312.39 µs 315.58 µs]
                        change: [−33.427% −33.090% −32.688%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 5.5160 s (25k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [217.65 µs 217.83 µs 218.05 µs]
                        change: [−35.352% −35.061% −34.784%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  3 (3.00%) high severe

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 56.4s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 56.387 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [2.9068 s 2.9250 s 2.9445 s]
                        change: [+7.1061% +8.1628% +9.1111%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.3s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 29.259 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [334.61 ms 342.73 ms 350.22 ms]
                        change: [+7.8633% +11.069% +14.420%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.2s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 29.185 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [321.87 ms 332.98 ms 343.92 ms]
                        change: [+13.945% +18.302% +22.750%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 38.4s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 38.380 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [3.6304 s 3.6485 s 3.6647 s]
                        change: [+7.5106% +8.3278% +9.0677%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 36.4s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 36.430 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [129.55 ms 137.88 ms 146.33 ms]
                        change: [+1.9454% +11.752% +23.211%] (p = 0.05 < 0.05)
                        Performance has regressed.

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 26.5s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 26.511 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [269.69 ms 271.26 ms 273.15 ms]
                        change: [−8.8819% −8.1087% −7.3232%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 39.0s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 38.959 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [483.77 ms 491.05 ms 499.86 ms]
                        change: [−7.5537% −5.9108% −4.0763%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.0s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 28.987 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.8735 s 2.8838 s 2.8939 s]
                        change: [+10.228% +11.286% +12.211%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.6s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 28.637 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.8408 s 2.8578 s 2.8729 s]
                        change: [+10.290% +10.958% +11.620%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.6s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 28.614 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.8659 s 2.8815 s 2.8963 s]
                        change: [+12.100% +12.811% +13.489%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.8s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 28.847 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.8833 s 2.8983 s 2.9138 s]
                        change: [+13.677% +14.447% +15.280%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.0s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 28.975 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [2.8765 s 2.8964 s 2.9225 s]
                        change: [+13.717% +14.590% +15.646%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 5.3431 s (15k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [337.22 µs 339.03 µs 341.00 µs]
                        change: [−23.193% −22.681% −22.168%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 5.2707 s (15k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [337.51 µs 339.24 µs 341.11 µs]
                        change: [−22.775% −22.135% −21.535%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 5.2809 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [339.30 µs 340.97 µs 342.68 µs]
                        change: [−22.248% −21.661% −21.027%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) high mild
  3 (3.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 5.2757 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [338.45 µs 340.33 µs 342.41 µs]
                        change: [−21.856% −20.835% −19.574%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 5.2785 s (15k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [334.67 µs 339.21 µs 345.12 µs]
                        change: [−22.860% −21.885% −20.639%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 6.8869 s (30 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [228.48 ms 230.27 ms 232.17 ms]
                        change: [+8.2502% +9.4044% +10.596%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.6s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 13.597 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [1.3663 s 1.3782 s 1.3920 s]
                        change: [+13.790% +15.016% +16.269%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.2s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 29.234 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.8907 s 2.9136 s 2.9391 s]
                        change: [+13.648% +14.677% +15.842%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0002 s (18k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [286.68 µs 287.07 µs 287.68 µs]
                        change: [−22.350% −21.581% −20.777%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0046 s (16k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [325.14 µs 326.08 µs 327.36 µs]
                        change: [−23.266% −22.556% −21.792%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0148 s (15k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [343.91 µs 344.61 µs 345.87 µs]
                        change: [−24.071% −23.387% −22.649%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 7.2139 s (30 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [18.692 ms 20.783 ms 23.317 ms]
                        change: [−3.0443% +8.3868% +21.676%] (p = 0.25 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 13.9s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 13.888 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [164.44 ms 167.78 ms 170.83 ms]
                        change: [+23.912% +28.125% +32.528%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.8s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 29.779 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [343.30 ms 347.74 ms 351.63 ms]
                        change: [+23.207% +26.123% +28.799%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 6.1056 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [27.696 ms 27.769 ms 27.854 ms]
                        change: [−3.3402% −1.9723% −0.9097%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.3s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 7.2758 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [132.37 ms 134.05 ms 135.63 ms]
                        change: [−4.1305% −2.5681% −1.2484%] (p = 0.00 < 0.05)
                        Performance has improved.
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 5.3816 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [268.98 ms 271.69 ms 275.35 ms]
                        change: [−6.3718% −4.7611% −3.1325%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 175.7s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 175.69 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [17.176 s 17.247 s 17.319 s]
                        change: [+10.126% +10.782% +11.378%] (p = 0.00 < 0.05)
                        Performance has regressed.
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 381.5s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 381.54 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [36.618 s 36.745 s 36.878 s]
                        change: [+10.537% +11.109% +11.670%] (p = 0.00 < 0.05)
                        Performance has regressed.

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0431 s (3795 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.3341 ms 1.3400 ms 1.3551 ms]
                        change: [+2.7060% +4.0458% +6.0024%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0898 s (2915 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.7459 ms 1.7471 ms 1.7481 ms]
                        change: [+6.0541% +6.3558% +6.6841%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high mild

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 15.5s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 15.488 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.5470 s 1.5550 s 1.5679 s]
                        change: [−5.2769% −4.0017% −2.7631%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 30.9s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 30.913 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [3.0989 s 3.1223 s 3.1587 s]
                        change: [−3.0430% −1.3158% +0.2142%] (p = 0.15 > 0.05)
                        No change in performance detected.
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

```

## LSM Stress Tests
```
[1m[92m   Compiling[0m serde_core v1.0.228
[1m[92m   Compiling[0m syn v2.0.117
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
[1m[92m    Finished[0m `release` profile [optimized] target(s) in 30.62s
[1m[92m     Running[0m unittests src/lib.rs (target/release/deps/dugite_lsm-70eb9ee18bad5445)

running 3 tests
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 9.27s

[1m[92m   Doc-tests[0m dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

```
