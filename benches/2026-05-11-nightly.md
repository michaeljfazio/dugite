# Nightly Benchmark Results — 2026-05-11

Machine: GitHub Actions ubuntu-latest
Branch: main (e0da2d5)

## Storage Benchmarks
```
[1m[92m    Updating[0m crates.io index
[1m[92m    Updating[0m git repository `https://github.com/iquerejeta/curve25519-dalek`
[1m[92m    Updating[0m git repository `https://github.com/input-output-hk/vrf`
[1m[92m    Updating[0m git repository `https://github.com/aiken-lang/aiken.git`
[1m[92m Downloading[0m crates ...
[1m[92m  Downloaded[0m anes v0.1.6
[1m[92m  Downloaded[0m anstyle v1.0.14
[1m[92m  Downloaded[0m arrayref v0.3.9
[1m[92m  Downloaded[0m alloca v0.4.0
[1m[92m  Downloaded[0m crc v3.4.0
[1m[92m  Downloaded[0m cast v0.3.0
[1m[92m  Downloaded[0m block-buffer v0.9.0
[1m[92m  Downloaded[0m base58 v0.2.0
[1m[92m  Downloaded[0m digest v0.9.0
[1m[92m  Downloaded[0m num-conv v0.2.1
[1m[92m  Downloaded[0m base64ct v1.8.3
[1m[92m  Downloaded[0m getrandom v0.1.16
[1m[92m  Downloaded[0m clap_lex v1.1.0
[1m[92m  Downloaded[0m bech32 v0.9.1
[1m[92m  Downloaded[0m crossbeam-epoch v0.9.18
[1m[92m  Downloaded[0m ciborium-io v0.2.2
[1m[92m  Downloaded[0m autocfg v1.5.0
[1m[92m  Downloaded[0m criterion-plot v0.8.2
[1m[92m  Downloaded[0m crossbeam-deque v0.8.6
[1m[92m  Downloaded[0m base64 v0.22.1
[1m[92m  Downloaded[0m crypto-common v0.2.1
[1m[92m  Downloaded[0m cpufeatures v0.2.17
[1m[92m  Downloaded[0m ciborium-ll v0.2.2
[1m[92m  Downloaded[0m crossbeam-utils v0.8.21
[1m[92m  Downloaded[0m cpufeatures v0.3.0
[1m[92m  Downloaded[0m bitflags v2.11.0
[1m[92m  Downloaded[0m strsim v0.11.1
[1m[92m  Downloaded[0m parking_lot_core v0.9.12
[1m[92m  Downloaded[0m ident_case v1.0.1
[1m[92m  Downloaded[0m pallas-primitives v1.0.0-alpha.6
[1m[92m  Downloaded[0m powerfmt v0.2.0
[1m[92m  Downloaded[0m parking_lot v0.12.5
[1m[92m  Downloaded[0m hex v0.4.3
[1m[92m  Downloaded[0m num-traits v0.2.19
[1m[92m  Downloaded[0m curve25519-dalek-derive v0.1.1
[1m[92m  Downloaded[0m convert_case v0.10.0
[1m[92m  Downloaded[0m time-macros v0.2.27
[1m[92m  Downloaded[0m ref-cast-impl v1.0.25
[1m[92m  Downloaded[0m cfg-if v1.0.4
[1m[92m  Downloaded[0m minicbor-derive v0.16.2
[1m[92m  Downloaded[0m lock_api v0.4.14
[1m[92m  Downloaded[0m fastrand v2.4.1
[1m[92m  Downloaded[0m crc-catalog v2.4.0
[1m[92m  Downloaded[0m block-buffer v0.12.0
[1m[92m  Downloaded[0m version_check v0.9.5
[1m[92m  Downloaded[0m rustversion v1.0.22
[1m[92m  Downloaded[0m num-rational v0.4.2
[1m[92m  Downloaded[0m getrandom v0.4.2
[1m[92m  Downloaded[0m tokio-macros v2.7.0
[1m[92m  Downloaded[0m iana-time-zone v0.1.65
[1m[92m  Downloaded[0m unicode-xid v0.2.6
[1m[92m  Downloaded[0m darling_core v0.23.0
[1m[92m  Downloaded[0m thiserror v2.0.18
[1m[92m  Downloaded[0m tracing-attributes v0.1.31
[1m[92m  Downloaded[0m hashbrown v0.12.3
[1m[92m  Downloaded[0m derive_more-impl v2.1.1
[1m[92m  Downloaded[0m time-core v0.1.8
[1m[92m  Downloaded[0m itertools v0.13.0
[1m[92m  Downloaded[0m plotters-svg v0.3.7
[1m[92m  Downloaded[0m tinytemplate v1.2.1
[1m[92m  Downloaded[0m subtle v2.6.1
[1m[92m  Downloaded[0m hashbrown v0.17.0
[1m[92m  Downloaded[0m zmij v1.0.21
[1m[92m  Downloaded[0m signature v2.2.0
[1m[92m  Downloaded[0m pin-project-lite v0.2.17
[1m[92m  Downloaded[0m sha2 v0.11.0
[1m[92m  Downloaded[0m rand_chacha v0.9.0
[1m[92m  Downloaded[0m ppv-lite86 v0.2.21
[1m[92m  Downloaded[0m walkdir v2.5.0
[1m[92m  Downloaded[0m rand_core v0.6.4
[1m[92m  Downloaded[0m rand_core v0.5.1
[1m[92m  Downloaded[0m sha2 v0.9.9
[1m[92m  Downloaded[0m rand_core v0.9.5
[1m[92m  Downloaded[0m zeroize v1.8.2
[1m[92m  Downloaded[0m thiserror-impl v1.0.69
[1m[92m  Downloaded[0m tempfile v3.27.0
[1m[92m  Downloaded[0m pkcs8 v0.10.2
[1m[92m  Downloaded[0m curve25519-dalek v3.2.0
[1m[92m  Downloaded[0m thiserror-impl v2.0.18
[1m[92m  Downloaded[0m socket2 v0.6.3
[1m[92m  Downloaded[0m proc-macro2 v1.0.106
[1m[92m  Downloaded[0m serde_with_macros v3.18.0
[1m[92m  Downloaded[0m smallvec v1.15.1
[1m[92m  Downloaded[0m unicode-ident v1.0.24
[1m[92m  Downloaded[0m serde_derive v1.0.228
[1m[92m  Downloaded[0m schemars v0.9.0
[1m[92m  Downloaded[0m zerocopy-derive v0.8.48
[1m[92m  Downloaded[0m serde v1.0.228
[1m[92m  Downloaded[0m unicode-segmentation v1.13.2
[1m[92m  Downloaded[0m schemars v1.2.1
[1m[92m  Downloaded[0m rayon v1.12.0
[1m[92m  Downloaded[0m serde_json v1.0.149
[1m[92m  Downloaded[0m serde_with v3.18.0
[1m[92m  Downloaded[0m regex v1.12.3
[1m[92m  Downloaded[0m zerocopy v0.8.48
[1m[92m  Downloaded[0m time v0.3.47
[1m[92m  Downloaded[0m libc v0.2.185
[1m[92m  Downloaded[0m plotters v0.3.7
[1m[92m  Downloaded[0m syn v2.0.117
[1m[92m  Downloaded[0m rand v0.9.4
[1m[92m  Downloaded[0m typenum v1.19.0
[1m[92m  Downloaded[0m tracing-core v0.1.36
[1m[92m  Downloaded[0m regex-syntax v0.8.10
[1m[92m  Downloaded[0m static_assertions v1.1.0
[1m[92m  Downloaded[0m sha2 v0.10.9
[1m[92m  Downloaded[0m rustix v1.1.4
[1m[92m  Downloaded[0m serde_core v1.0.228
[1m[92m  Downloaded[0m semver v1.0.28
[1m[92m  Downloaded[0m tracing v0.1.44
[1m[92m  Downloaded[0m rayon-core v1.13.0
[1m[92m  Downloaded[0m thiserror v1.0.69
[1m[92m  Downloaded[0m spki v0.7.3
[1m[92m  Downloaded[0m quote v1.0.45
[1m[92m  Downloaded[0m curve25519-dalek v4.1.3
[1m[92m  Downloaded[0m chrono v0.4.44
[1m[92m  Downloaded[0m plotters-backend v0.3.7
[1m[92m  Downloaded[0m regex-automata v0.4.14
[1m[92m  Downloaded[0m cryptoxide v0.4.4
[1m[92m  Downloaded[0m dashu-int v0.4.1
[1m[92m  Downloaded[0m criterion v0.8.2
[1m[92m  Downloaded[0m cc v1.2.60
[1m[92m  Downloaded[0m bytes v1.11.1
[1m[92m  Downloaded[0m same-file v1.0.6
[1m[92m  Downloaded[0m ref-cast v1.0.25
[1m[92m  Downloaded[0m paste v1.0.15
[1m[92m  Downloaded[0m num-bigint v0.4.6
[1m[92m  Downloaded[0m mio v1.2.0
[1m[92m  Downloaded[0m memchr v2.8.0
[1m[92m  Downloaded[0m indexmap v2.14.0
[1m[92m  Downloaded[0m ed25519-dalek v2.2.0
[1m[92m  Downloaded[0m tokio v1.51.1
[1m[92m  Downloaded[0m derive_more v2.1.1
[1m[92m  Downloaded[0m bech32 v0.11.1
[1m[92m  Downloaded[0m signal-hook-registry v1.4.8
[1m[92m  Downloaded[0m shlex v1.3.0
[1m[92m  Downloaded[0m clap_builder v4.6.0
[1m[92m  Downloaded[0m blake2b_simd v1.0.4
[1m[92m  Downloaded[0m aho-corasick v1.1.4
[1m[92m  Downloaded[0m indexmap v1.9.3
[1m[92m  Downloaded[0m digest v0.11.2
[1m[92m  Downloaded[0m clap v4.6.1
[1m[92m  Downloaded[0m half v2.7.1
[1m[92m  Downloaded[0m scopeguard v1.2.0
[1m[92m  Downloaded[0m once_cell v1.21.4
[1m[92m  Downloaded[0m getrandom v0.3.4
[1m[92m  Downloaded[0m getrandom v0.2.17
[1m[92m  Downloaded[0m num-integer v0.1.46
[1m[92m  Downloaded[0m memmap2 v0.9.10
[1m[92m  Downloaded[0m hybrid-array v0.4.10
[1m[92m  Downloaded[0m either v1.15.0
[1m[92m  Downloaded[0m ed25519 v2.2.3
[1m[92m  Downloaded[0m darling v0.23.0
[1m[92m  Downloaded[0m blake2 v0.10.6
[1m[92m  Downloaded[0m oorandom v11.1.5
[1m[92m  Downloaded[0m num-order v1.2.0
[1m[92m  Downloaded[0m minicbor v0.26.5
[1m[92m  Downloaded[0m generic-array v0.14.9
[1m[92m  Downloaded[0m find-msvc-tools v0.1.9
[1m[92m  Downloaded[0m digest v0.10.7
[1m[92m  Downloaded[0m der v0.7.10
[1m[92m  Downloaded[0m const-oid v0.9.6
[1m[92m  Downloaded[0m rustc_version v0.4.1
[1m[92m  Downloaded[0m num-modular v0.6.1
[1m[92m  Downloaded[0m dyn-clone v1.0.20
[1m[92m  Downloaded[0m errno v0.3.14
[1m[92m  Downloaded[0m crc32fast v1.5.0
[1m[92m  Downloaded[0m ciborium v0.2.2
[1m[92m  Downloaded[0m itoa v1.0.18
[1m[92m  Downloaded[0m deranged v0.5.8
[1m[92m  Downloaded[0m dashu-base v0.4.1
[1m[92m  Downloaded[0m constant_time_eq v0.4.2
[1m[92m  Downloaded[0m byteorder v1.5.0
[1m[92m  Downloaded[0m block-buffer v0.10.4
[1m[92m  Downloaded[0m equivalent v1.0.2
[1m[92m  Downloaded[0m darling_macro v0.23.0
[1m[92m  Downloaded[0m pallas-traverse v1.0.0-alpha.6
[1m[92m  Downloaded[0m pallas-codec v1.0.0-alpha.6
[1m[92m  Downloaded[0m pallas-crypto v1.0.0-alpha.6
[1m[92m  Downloaded[0m pallas-addresses v1.0.0-alpha.6
[1m[92m  Downloaded[0m page_size v0.6.0
[1m[92m  Downloaded[0m opaque-debug v0.3.1
[1m[92m  Downloaded[0m crypto-common v0.1.6
[1m[92m  Downloaded[0m const-oid v0.10.2
[1m[92m  Downloaded[0m arrayvec v0.7.6
[1m[92m  Downloaded[0m linux-raw-sys v0.12.1
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
[1m[92m   Compiling[0m digest v0.10.7
[1m[92m   Compiling[0m num-traits v0.2.19
[1m[92m   Compiling[0m strsim v0.11.1
[1m[92m   Compiling[0m cpufeatures v0.2.17
[1m[92m   Compiling[0m getrandom v0.3.4
[1m[92m   Compiling[0m semver v1.0.28
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m ident_case v1.0.1
[1m[92m   Compiling[0m rustc_version v0.4.1
[1m[92m   Compiling[0m thiserror v1.0.69
[1m[92m   Compiling[0m minicbor v0.26.5
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m zmij v1.0.21
[1m[92m   Compiling[0m rand_core v0.9.5
[1m[92m   Compiling[0m signature v2.2.0
[1m[92m   Compiling[0m serde_json v1.0.149
[1m[92m   Compiling[0m hex v0.4.3
[1m[92m   Compiling[0m sha2 v0.10.9
[1m[92m   Compiling[0m itoa v1.0.18
[1m[92m   Compiling[0m getrandom v0.1.16
[1m[92m   Compiling[0m rand_core v0.6.4
[1m[92m   Compiling[0m memchr v2.8.0
[1m[92m   Compiling[0m cryptoxide v0.4.4
[1m[92m   Compiling[0m darling_core v0.23.0
[1m[92m   Compiling[0m zerocopy-derive v0.8.48
[1m[92m   Compiling[0m serde_derive v1.0.228
[1m[92m   Compiling[0m thiserror-impl v1.0.69
[1m[92m   Compiling[0m darling_macro v0.23.0
[1m[92m   Compiling[0m minicbor-derive v0.16.2
[1m[92m   Compiling[0m darling v0.23.0
[1m[92m   Compiling[0m curve25519-dalek-derive v0.1.1
[1m[92m   Compiling[0m serde_with_macros v3.18.0
[1m[92m   Compiling[0m serde_with v3.18.0
[1m[92m   Compiling[0m ed25519 v2.2.3
[1m[92m   Compiling[0m ed25519-dalek v2.2.0
[1m[92m   Compiling[0m either v1.15.0
[1m[92m   Compiling[0m crossbeam-utils v0.8.21
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m convert_case v0.10.0
[1m[92m   Compiling[0m half v2.7.1
[1m[92m   Compiling[0m ppv-lite86 v0.2.21
[1m[92m   Compiling[0m digest v0.9.0
[1m[92m   Compiling[0m hybrid-array v0.4.10
[1m[92m   Compiling[0m rand_chacha v0.9.0
[1m[92m   Compiling[0m rand v0.9.4
[1m[92m   Compiling[0m unicode-xid v0.2.6
[1m[92m   Compiling[0m thiserror v2.0.18
[1m[92m   Compiling[0m derive_more-impl v2.1.1
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m rand_core v0.5.1
[1m[92m   Compiling[0m pallas-codec v1.0.0-alpha.6
[1m[92m   Compiling[0m itertools v0.13.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m thiserror-impl v2.0.18
[1m[92m   Compiling[0m once_cell v1.21.4
[1m[92m   Compiling[0m arrayvec v0.7.6
[1m[92m   Compiling[0m paste v1.0.15
[1m[92m   Compiling[0m iana-time-zone v0.1.65
[1m[92m   Compiling[0m crc-catalog v2.4.0
[1m[92m   Compiling[0m arrayref v0.3.9
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m bytes v1.11.1
[1m[92m   Compiling[0m shlex v1.3.0
[1m[92m   Compiling[0m constant_time_eq v0.4.2
[1m[92m   Compiling[0m rustversion v1.0.22
[1m[92m   Compiling[0m find-msvc-tools v0.1.9
[1m[92m   Compiling[0m parking_lot_core v0.9.12
[1m[92m   Compiling[0m cc v1.2.60
[1m[92m   Compiling[0m blake2b_simd v1.0.4
[1m[92m   Compiling[0m chrono v0.4.44
[1m[92m   Compiling[0m crc v3.4.0
[1m[92m   Compiling[0m derive_more v2.1.1
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m crossbeam-epoch v0.9.18
[1m[92m   Compiling[0m block-buffer v0.12.0
[1m[92m   Compiling[0m crypto-common v0.2.1
[1m[92m   Compiling[0m blake2 v0.10.6
[1m[92m   Compiling[0m block-buffer v0.9.0
[1m[92m   Compiling[0m bech32 v0.11.1
[1m[92m   Compiling[0m bech32 v0.9.1
[1m[92m   Compiling[0m pin-project-lite v0.2.17
[1m[92m   Compiling[0m num-modular v0.6.1
[1m[92m   Compiling[0m opaque-debug v0.3.1
[1m[92m   Compiling[0m const-oid v0.10.2
[1m[92m   Compiling[0m base58 v0.2.0
[1m[92m   Compiling[0m scopeguard v1.2.0
[1m[92m   Compiling[0m smallvec v1.15.1
[1m[92m   Compiling[0m rayon-core v1.13.0
[1m[92m   Compiling[0m lock_api v0.4.14
[1m[92m   Compiling[0m num-order v1.2.0
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m digest v0.11.2
[1m[92m   Compiling[0m sha2 v0.9.9
[1m[92m   Compiling[0m dugite-primitives v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m crossbeam-deque v0.8.6
[1m[92m   Compiling[0m alloca v0.4.0
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m tracing-core v0.1.36
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m tracing-attributes v0.1.31
[1m[92m   Compiling[0m errno v0.3.14
[1m[92m   Compiling[0m clap_lex v1.1.0
[1m[92m   Compiling[0m anstyle v1.0.14
[1m[92m   Compiling[0m plotters-backend v0.3.7
[1m[92m   Compiling[0m crc32fast v1.5.0
[1m[92m   Compiling[0m getrandom v0.4.2
[1m[92m   Compiling[0m dashu-base v0.4.1
[1m[92m   Compiling[0m cpufeatures v0.3.0
[1m[92m   Compiling[0m ciborium-io v0.2.2
[1m[92m   Compiling[0m static_assertions v1.1.0
[1m[92m   Compiling[0m rustix v1.1.4
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m dashu-int v0.4.1
[1m[92m   Compiling[0m tracing v0.1.44
[1m[92m   Compiling[0m ciborium-ll v0.2.2
[1m[92m   Compiling[0m sha2 v0.11.0
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m plotters-svg v0.3.7
[1m[92m   Compiling[0m clap_builder v4.6.0
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
[1m[92m   Compiling[0m rayon v1.12.0
[1m[92m   Compiling[0m clap v4.6.1
[1m[92m   Compiling[0m dugite-serialization v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m dugite-crypto v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m plotters v0.3.7
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m ciborium v0.2.2
[1m[92m   Compiling[0m tinytemplate v1.2.1
[1m[92m   Compiling[0m memmap2 v0.9.10
[1m[92m   Compiling[0m page_size v0.6.0
[1m[92m   Compiling[0m oorandom v11.1.5
[1m[92m   Compiling[0m anes v0.1.6
[1m[92m   Compiling[0m fastrand v2.4.1
[1m[92m   Compiling[0m tempfile v3.27.0
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m dugite-storage v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-storage)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 11s
[1m[92m     Running[0m benches/storage_bench.rs (target/release/deps/storage_bench-9f185658b419e032)
Gnuplot not found, using plotters backend
Benchmarking chaindb/sequential_insert/10k_20kb
Benchmarking chaindb/sequential_insert/10k_20kb: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 33.8s.
Benchmarking chaindb/sequential_insert/10k_20kb: Collecting 10 samples in estimated 33.761 s (10 iterations)
Benchmarking chaindb/sequential_insert/10k_20kb: Analyzing
chaindb/sequential_insert/10k_20kb
                        time:   [2.9569 s 3.0850 s 3.2936 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking chaindb/random_read/by_hash/10000blks
Benchmarking chaindb/random_read/by_hash/10000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 303.2s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/10000blks: Collecting 100 samples in estimated 303.22 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/10000blks: Analyzing
chaindb/random_read/by_hash/10000blks
                        time:   [23.358 ms 23.543 ms 23.731 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking chaindb/random_read/by_hash/100000blks
Benchmarking chaindb/random_read/by_hash/100000blks: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 3212.9s, or reduce sample count to 10.
Benchmarking chaindb/random_read/by_hash/100000blks: Collecting 100 samples in estimated 3212.9 s (100 iterations)
Benchmarking chaindb/random_read/by_hash/100000blks: Analyzing
chaindb/random_read/by_hash/100000blks
                        time:   [228.11 ms 229.81 ms 231.71 ms]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

Benchmarking chaindb/tip_query
Benchmarking chaindb/tip_query: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 267.6s, or reduce sample count to 10.
Benchmarking chaindb/tip_query: Collecting 100 samples in estimated 267.59 s (100 iterations)
Benchmarking chaindb/tip_query: Analyzing
chaindb/tip_query       time:   [21.573 ms 21.732 ms 21.910 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking chaindb/has_block
Benchmarking chaindb/has_block: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 277.9s, or reduce sample count to 10.
Benchmarking chaindb/has_block: Collecting 100 samples in estimated 277.95 s (100 iterations)
Benchmarking chaindb/has_block: Analyzing
chaindb/has_block       time:   [21.465 ms 21.579 ms 21.691 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild

Benchmarking chaindb/slot_range_100
Benchmarking chaindb/slot_range_100: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 265.5s, or reduce sample count to 10.
Benchmarking chaindb/slot_range_100: Collecting 100 samples in estimated 265.54 s (100 iterations)
Benchmarking chaindb/slot_range_100: Analyzing
chaindb/slot_range_100  time:   [23.780 ms 23.920 ms 24.090 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.3s.
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Collecting 10 samples in estimated 6.2627 s (10 iterations)
Benchmarking chaindb/flush_to_immutable/k_2160_blocks_20kb/2160: Analyzing
chaindb/flush_to_immutable/k_2160_blocks_20kb/2160
                        time:   [4.4691 ms 4.6091 ms 4.7566 ms]

Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 28.3s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Collecting 10 samples in estimated 28.345 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/in_memory: Analyzing
chaindb/profile_comparison/insert_10k_20kb/in_memory
                        time:   [2.7572 s 2.7913 s 2.8214 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.5s.
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Collecting 10 samples in estimated 27.474 s (10 iterations)
Benchmarking chaindb/profile_comparison/insert_10k_20kb/mmap: Analyzing
chaindb/profile_comparison/insert_10k_20kb/mmap
                        time:   [2.7191 s 2.7504 s 2.7842 s]
Benchmarking chaindb/profile_comparison/read_500/in_memory
Benchmarking chaindb/profile_comparison/read_500/in_memory: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.4s.
Benchmarking chaindb/profile_comparison/read_500/in_memory: Collecting 10 samples in estimated 27.445 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/in_memory: Analyzing
chaindb/profile_comparison/read_500/in_memory
                        time:   [21.555 ms 22.095 ms 22.695 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking chaindb/profile_comparison/read_500/mmap
Benchmarking chaindb/profile_comparison/read_500/mmap: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.3s.
Benchmarking chaindb/profile_comparison/read_500/mmap: Collecting 10 samples in estimated 27.332 s (10 iterations)
Benchmarking chaindb/profile_comparison/read_500/mmap: Analyzing
chaindb/profile_comparison/read_500/mmap
                        time:   [21.798 ms 22.295 ms 22.699 ms]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) low mild

Benchmarking immutabledb/open/in_memory/10000
Benchmarking immutabledb/open/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/in_memory/10000: Collecting 100 samples in estimated 7.1629 s (200 iterations)
Benchmarking immutabledb/open/in_memory/10000: Analyzing
immutabledb/open/in_memory/10000
                        time:   [35.587 ms 35.659 ms 35.732 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking immutabledb/open/mmap_cached/10000
Benchmarking immutabledb/open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/10000: Collecting 100 samples in estimated 5.9672 s (25k iterations)
Benchmarking immutabledb/open/mmap_cached/10000: Analyzing
immutabledb/open/mmap_cached/10000
                        time:   [234.39 µs 235.10 µs 236.07 µs]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking immutabledb/open/mmap_cold_rebuild/10000
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Collecting 100 samples in estimated 5.0241 s (2200 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/10000: Analyzing
immutabledb/open/mmap_cold_rebuild/10000
                        time:   [2.2499 ms 2.3703 ms 2.6004 ms]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking immutabledb/open/in_memory/100000
Benchmarking immutabledb/open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 65.2s, or reduce sample count to 10.
Benchmarking immutabledb/open/in_memory/100000: Collecting 100 samples in estimated 65.164 s (100 iterations)
Benchmarking immutabledb/open/in_memory/100000: Analyzing
immutabledb/open/in_memory/100000
                        time:   [527.13 ms 528.57 ms 530.01 ms]
Benchmarking immutabledb/open/mmap_cached/100000
Benchmarking immutabledb/open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cached/100000: Collecting 100 samples in estimated 5.2059 s (2100 iterations)
Benchmarking immutabledb/open/mmap_cached/100000: Analyzing
immutabledb/open/mmap_cached/100000
                        time:   [2.5750 ms 2.6135 ms 2.6525 ms]
Benchmarking immutabledb/open/mmap_cold_rebuild/100000
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Warming up for 3.0000 s
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Collecting 100 samples in estimated 6.8734 s (300 iterations)
Benchmarking immutabledb/open/mmap_cold_rebuild/100000: Analyzing
immutabledb/open/mmap_cold_rebuild/100000
                        time:   [22.192 ms 22.366 ms 22.542 ms]

Benchmarking immutabledb/lookup/in_memory/10000
Benchmarking immutabledb/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/in_memory/10000: Collecting 100 samples in estimated 5.4591 s (700 iterations)
Benchmarking immutabledb/lookup/in_memory/10000: Analyzing
immutabledb/lookup/in_memory/10000
                        time:   [7.8676 ms 7.9017 ms 7.9365 ms]
Benchmarking immutabledb/lookup/mmap/10000
Benchmarking immutabledb/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking immutabledb/lookup/mmap/10000: Collecting 100 samples in estimated 5.4505 s (700 iterations)
Benchmarking immutabledb/lookup/mmap/10000: Analyzing
immutabledb/lookup/mmap/10000
                        time:   [7.8558 ms 7.9056 ms 7.9567 ms]

Benchmarking immutabledb/has_block/in_memory
Benchmarking immutabledb/has_block/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/in_memory: Collecting 100 samples in estimated 5.0534 s (177k iterations)
Benchmarking immutabledb/has_block/in_memory: Analyzing
immutabledb/has_block/in_memory
                        time:   [28.344 µs 28.446 µs 28.544 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/has_block/mmap
Benchmarking immutabledb/has_block/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/has_block/mmap: Collecting 100 samples in estimated 5.0586 s (177k iterations)
Benchmarking immutabledb/has_block/mmap: Analyzing
immutabledb/has_block/mmap
                        time:   [28.342 µs 28.442 µs 28.538 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking immutabledb/append/1k_blocks_20kb/in_memory
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Collecting 100 samples in estimated 5.0069 s (500 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/in_memory: Analyzing
immutabledb/append/1k_blocks_20kb/in_memory
                        time:   [9.7031 ms 9.7432 ms 9.7835 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking immutabledb/append/1k_blocks_20kb/mmap
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Collecting 100 samples in estimated 5.0834 s (500 iterations)
Benchmarking immutabledb/append/1k_blocks_20kb/mmap: Analyzing
immutabledb/append/1k_blocks_20kb/mmap
                        time:   [10.193 ms 10.248 ms 10.308 ms]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild

Benchmarking immutabledb/slot_range/range_100/in_memory
Benchmarking immutabledb/slot_range/range_100/in_memory: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/in_memory: Collecting 100 samples in estimated 6.7236 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/in_memory: Analyzing
immutabledb/slot_range/range_100/in_memory
                        time:   [444.84 µs 445.66 µs 446.45 µs]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
Benchmarking immutabledb/slot_range/range_100/mmap
Benchmarking immutabledb/slot_range/range_100/mmap: Warming up for 3.0000 s
Benchmarking immutabledb/slot_range/range_100/mmap: Collecting 100 samples in estimated 6.4472 s (15k iterations)
Benchmarking immutabledb/slot_range/range_100/mmap: Analyzing
immutabledb/slot_range/range_100/mmap
                        time:   [426.15 µs 427.00 µs 427.85 µs]
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking block_index/insert/in_memory/10000
Benchmarking block_index/insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/10000: Collecting 100 samples in estimated 7.4834 s (10k iterations)
Benchmarking block_index/insert/in_memory/10000: Analyzing
block_index/insert/in_memory/10000
                        time:   [744.12 µs 744.62 µs 745.11 µs]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking block_index/insert/mmap/10000
Benchmarking block_index/insert/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/10000: Collecting 100 samples in estimated 5.1225 s (1200 iterations)
Benchmarking block_index/insert/mmap/10000: Analyzing
block_index/insert/mmap/10000
                        time:   [4.2271 ms 4.2658 ms 4.3158 ms]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking block_index/insert/in_memory/50000
Benchmarking block_index/insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/50000: Collecting 100 samples in estimated 5.1472 s (1500 iterations)
Benchmarking block_index/insert/in_memory/50000: Analyzing
block_index/insert/in_memory/50000
                        time:   [3.4536 ms 3.4793 ms 3.5068 ms]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking block_index/insert/mmap/50000
Benchmarking block_index/insert/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/insert/mmap/50000: Collecting 100 samples in estimated 6.6594 s (200 iterations)
Benchmarking block_index/insert/mmap/50000: Analyzing
block_index/insert/mmap/50000
                        time:   [34.545 ms 35.348 ms 36.195 ms]
Benchmarking block_index/insert/in_memory/100000
Benchmarking block_index/insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/insert/in_memory/100000: Collecting 100 samples in estimated 5.7180 s (700 iterations)
Benchmarking block_index/insert/in_memory/100000: Analyzing
block_index/insert/in_memory/100000
                        time:   [7.7675 ms 7.8826 ms 8.0012 ms]
Benchmarking block_index/insert/mmap/100000
Benchmarking block_index/insert/mmap/100000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.7s, or reduce sample count to 70.
Benchmarking block_index/insert/mmap/100000: Collecting 100 samples in estimated 6.7110 s (100 iterations)
Benchmarking block_index/insert/mmap/100000: Analyzing
block_index/insert/mmap/100000
                        time:   [69.082 ms 70.682 ms 72.363 ms]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking block_index/lookup/in_memory/10000
Benchmarking block_index/lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/10000: Collecting 100 samples in estimated 5.0097 s (359k iterations)
Benchmarking block_index/lookup/in_memory/10000: Analyzing
block_index/lookup/in_memory/10000
                        time:   [13.984 µs 14.061 µs 14.213 µs]
Found 18 outliers among 100 measurements (18.00%)
  18 (18.00%) high severe
Benchmarking block_index/lookup/mmap/10000
Benchmarking block_index/lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/10000: Collecting 100 samples in estimated 5.1060 s (222k iterations)
Benchmarking block_index/lookup/mmap/10000: Analyzing
block_index/lookup/mmap/10000
                        time:   [22.914 µs 22.989 µs 23.068 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/50000
Benchmarking block_index/lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/50000: Collecting 100 samples in estimated 5.0093 s (354k iterations)
Benchmarking block_index/lookup/in_memory/50000: Analyzing
block_index/lookup/in_memory/50000
                        time:   [14.149 µs 14.167 µs 14.187 µs]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
Benchmarking block_index/lookup/mmap/50000
Benchmarking block_index/lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/50000: Collecting 100 samples in estimated 5.0036 s (293k iterations)
Benchmarking block_index/lookup/mmap/50000: Analyzing
block_index/lookup/mmap/50000
                        time:   [17.100 µs 17.148 µs 17.200 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking block_index/lookup/in_memory/100000
Benchmarking block_index/lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/in_memory/100000: Collecting 100 samples in estimated 5.0111 s (354k iterations)
Benchmarking block_index/lookup/in_memory/100000: Analyzing
block_index/lookup/in_memory/100000
                        time:   [14.200 µs 14.216 µs 14.234 µs]
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
Benchmarking block_index/lookup/mmap/100000
Benchmarking block_index/lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking block_index/lookup/mmap/100000: Collecting 100 samples in estimated 5.0519 s (303k iterations)
Benchmarking block_index/lookup/mmap/100000: Analyzing
block_index/lookup/mmap/100000
                        time:   [16.672 µs 16.719 µs 16.768 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

Benchmarking block_index/contains_miss/in_memory
Benchmarking block_index/contains_miss/in_memory: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/in_memory: Collecting 100 samples in estimated 5.0318 s (394k iterations)
Benchmarking block_index/contains_miss/in_memory: Analyzing
block_index/contains_miss/in_memory
                        time:   [12.758 µs 12.768 µs 12.781 µs]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking block_index/contains_miss/mmap
Benchmarking block_index/contains_miss/mmap: Warming up for 3.0000 s
Benchmarking block_index/contains_miss/mmap: Collecting 100 samples in estimated 5.0812 s (106k iterations)
Benchmarking block_index/contains_miss/mmap: Analyzing
block_index/contains_miss/mmap
                        time:   [34.380 µs 34.494 µs 34.601 µs]
Found 23 outliers among 100 measurements (23.00%)
  16 (16.00%) low severe
  6 (6.00%) high mild
  1 (1.00%) high severe

Benchmarking scaling/block_index_insert/in_memory/10000
Benchmarking scaling/block_index_insert/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/10000: Collecting 10 samples in estimated 5.0334 s (6820 iterations)
Benchmarking scaling/block_index_insert/in_memory/10000: Analyzing
scaling/block_index_insert/in_memory/10000
                        time:   [737.28 µs 740.76 µs 743.60 µs]
Benchmarking scaling/block_index_insert/mmap/10000
Benchmarking scaling/block_index_insert/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/10000: Collecting 10 samples in estimated 5.0897 s (1210 iterations)
Benchmarking scaling/block_index_insert/mmap/10000: Analyzing
scaling/block_index_insert/mmap/10000
                        time:   [4.1199 ms 4.1428 ms 4.1841 ms]
Benchmarking scaling/block_index_insert/in_memory/50000
Benchmarking scaling/block_index_insert/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/50000: Collecting 10 samples in estimated 5.1502 s (1485 iterations)
Benchmarking scaling/block_index_insert/in_memory/50000: Analyzing
scaling/block_index_insert/in_memory/50000
                        time:   [3.3482 ms 3.3766 ms 3.4268 ms]
Benchmarking scaling/block_index_insert/mmap/50000
Benchmarking scaling/block_index_insert/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/50000: Collecting 10 samples in estimated 5.4923 s (165 iterations)
Benchmarking scaling/block_index_insert/mmap/50000: Analyzing
scaling/block_index_insert/mmap/50000
                        time:   [35.063 ms 35.312 ms 35.577 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/100000
Benchmarking scaling/block_index_insert/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/100000: Collecting 10 samples in estimated 5.3724 s (715 iterations)
Benchmarking scaling/block_index_insert/in_memory/100000: Analyzing
scaling/block_index_insert/in_memory/100000
                        time:   [7.3794 ms 7.7144 ms 7.9998 ms]
Benchmarking scaling/block_index_insert/mmap/100000
Benchmarking scaling/block_index_insert/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/100000: Collecting 10 samples in estimated 7.3553 s (110 iterations)
Benchmarking scaling/block_index_insert/mmap/100000: Analyzing
scaling/block_index_insert/mmap/100000
                        time:   [70.333 ms 70.873 ms 71.420 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_insert/in_memory/250000
Benchmarking scaling/block_index_insert/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/250000: Collecting 10 samples in estimated 5.3828 s (165 iterations)
Benchmarking scaling/block_index_insert/in_memory/250000: Analyzing
scaling/block_index_insert/in_memory/250000
                        time:   [30.557 ms 30.888 ms 31.276 ms]
Benchmarking scaling/block_index_insert/mmap/250000
Benchmarking scaling/block_index_insert/mmap/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.8s or enable flat sampling.
Benchmarking scaling/block_index_insert/mmap/250000: Collecting 10 samples in estimated 7.7546 s (55 iterations)
Benchmarking scaling/block_index_insert/mmap/250000: Analyzing
scaling/block_index_insert/mmap/250000
                        time:   [140.68 ms 142.01 ms 142.90 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild
Benchmarking scaling/block_index_insert/in_memory/500000
Benchmarking scaling/block_index_insert/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/in_memory/500000: Collecting 10 samples in estimated 8.0046 s (110 iterations)
Benchmarking scaling/block_index_insert/in_memory/500000: Analyzing
scaling/block_index_insert/in_memory/500000
                        time:   [71.298 ms 72.365 ms 72.885 ms]
Benchmarking scaling/block_index_insert/mmap/500000
Benchmarking scaling/block_index_insert/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_insert/mmap/500000: Collecting 10 samples in estimated 5.4240 s (20 iterations)
Benchmarking scaling/block_index_insert/mmap/500000: Analyzing
scaling/block_index_insert/mmap/500000
                        time:   [280.78 ms 285.49 ms 290.25 ms]
Benchmarking scaling/block_index_insert/in_memory/1000000
Benchmarking scaling/block_index_insert/in_memory/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.8s or enable flat sampling.
Benchmarking scaling/block_index_insert/in_memory/1000000: Collecting 10 samples in estimated 8.7565 s (55 iterations)
Benchmarking scaling/block_index_insert/in_memory/1000000: Analyzing
scaling/block_index_insert/in_memory/1000000
                        time:   [150.96 ms 152.30 ms 155.07 ms]
Benchmarking scaling/block_index_insert/mmap/1000000
Benchmarking scaling/block_index_insert/mmap/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 5.4s.
Benchmarking scaling/block_index_insert/mmap/1000000: Collecting 10 samples in estimated 5.3891 s (10 iterations)
Benchmarking scaling/block_index_insert/mmap/1000000: Analyzing
scaling/block_index_insert/mmap/1000000
                        time:   [555.88 ms 569.64 ms 584.11 ms]

Benchmarking scaling/block_index_lookup/in_memory/10000
Benchmarking scaling/block_index_lookup/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/10000: Collecting 10 samples in estimated 5.0004 s (358k iterations)
Benchmarking scaling/block_index_lookup/in_memory/10000: Analyzing
scaling/block_index_lookup/in_memory/10000
                        time:   [13.959 µs 13.976 µs 13.991 µs]
Benchmarking scaling/block_index_lookup/mmap/10000
Benchmarking scaling/block_index_lookup/mmap/10000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/10000: Collecting 10 samples in estimated 5.0012 s (216k iterations)
Benchmarking scaling/block_index_lookup/mmap/10000: Analyzing
scaling/block_index_lookup/mmap/10000
                        time:   [22.924 µs 22.940 µs 22.959 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/50000
Benchmarking scaling/block_index_lookup/in_memory/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/50000: Collecting 10 samples in estimated 5.0002 s (354k iterations)
Benchmarking scaling/block_index_lookup/in_memory/50000: Analyzing
scaling/block_index_lookup/in_memory/50000
                        time:   [14.106 µs 14.122 µs 14.140 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/50000
Benchmarking scaling/block_index_lookup/mmap/50000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/50000: Collecting 10 samples in estimated 5.0009 s (289k iterations)
Benchmarking scaling/block_index_lookup/mmap/50000: Analyzing
scaling/block_index_lookup/mmap/50000
                        time:   [17.068 µs 17.089 µs 17.110 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/100000
Benchmarking scaling/block_index_lookup/in_memory/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/100000: Collecting 10 samples in estimated 5.0007 s (352k iterations)
Benchmarking scaling/block_index_lookup/in_memory/100000: Analyzing
scaling/block_index_lookup/in_memory/100000
                        time:   [14.192 µs 14.197 µs 14.202 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high mild
Benchmarking scaling/block_index_lookup/mmap/100000
Benchmarking scaling/block_index_lookup/mmap/100000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/100000: Collecting 10 samples in estimated 5.0008 s (297k iterations)
Benchmarking scaling/block_index_lookup/mmap/100000: Analyzing
scaling/block_index_lookup/mmap/100000
                        time:   [16.646 µs 16.660 µs 16.669 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/250000
Benchmarking scaling/block_index_lookup/in_memory/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/250000: Collecting 10 samples in estimated 5.0008 s (352k iterations)
Benchmarking scaling/block_index_lookup/in_memory/250000: Analyzing
scaling/block_index_lookup/in_memory/250000
                        time:   [14.193 µs 14.202 µs 14.220 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/250000
Benchmarking scaling/block_index_lookup/mmap/250000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/250000: Collecting 10 samples in estimated 5.0002 s (298k iterations)
Benchmarking scaling/block_index_lookup/mmap/250000: Analyzing
scaling/block_index_lookup/mmap/250000
                        time:   [16.548 µs 16.595 µs 16.676 µs]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/500000
Benchmarking scaling/block_index_lookup/in_memory/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/500000: Collecting 10 samples in estimated 5.0003 s (350k iterations)
Benchmarking scaling/block_index_lookup/in_memory/500000: Analyzing
scaling/block_index_lookup/in_memory/500000
                        time:   [14.251 µs 14.258 µs 14.274 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/mmap/500000
Benchmarking scaling/block_index_lookup/mmap/500000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/500000: Collecting 10 samples in estimated 5.0003 s (304k iterations)
Benchmarking scaling/block_index_lookup/mmap/500000: Analyzing
scaling/block_index_lookup/mmap/500000
                        time:   [16.287 µs 16.293 µs 16.300 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking scaling/block_index_lookup/in_memory/1000000
Benchmarking scaling/block_index_lookup/in_memory/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/in_memory/1000000: Collecting 10 samples in estimated 5.0005 s (345k iterations)
Benchmarking scaling/block_index_lookup/in_memory/1000000: Analyzing
scaling/block_index_lookup/in_memory/1000000
                        time:   [14.508 µs 14.513 µs 14.521 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/block_index_lookup/mmap/1000000
Benchmarking scaling/block_index_lookup/mmap/1000000: Warming up for 3.0000 s
Benchmarking scaling/block_index_lookup/mmap/1000000: Collecting 10 samples in estimated 5.0005 s (305k iterations)
Benchmarking scaling/block_index_lookup/mmap/1000000: Analyzing
scaling/block_index_lookup/mmap/1000000
                        time:   [16.220 µs 16.227 µs 16.239 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe

Benchmarking scaling/immutabledb_open/in_memory/10000
Benchmarking scaling/immutabledb_open/in_memory/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/in_memory/10000: Collecting 10 samples in estimated 5.8166 s (165 iterations)
Benchmarking scaling/immutabledb_open/in_memory/10000: Analyzing
scaling/immutabledb_open/in_memory/10000
                        time:   [34.517 ms 34.766 ms 35.044 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/10000
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Collecting 10 samples in estimated 5.0005 s (21k iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/10000: Analyzing
scaling/immutabledb_open/mmap_cached/10000
                        time:   [234.94 µs 235.34 µs 236.11 µs]
Benchmarking scaling/immutabledb_open/in_memory/50000
Benchmarking scaling/immutabledb_open/in_memory/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.6s or enable flat sampling.
Benchmarking scaling/immutabledb_open/in_memory/50000: Collecting 10 samples in estimated 9.6123 s (55 iterations)
Benchmarking scaling/immutabledb_open/in_memory/50000: Analyzing
scaling/immutabledb_open/in_memory/50000
                        time:   [171.29 ms 171.69 ms 172.13 ms]
Benchmarking scaling/immutabledb_open/mmap_cached/50000
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Collecting 10 samples in estimated 5.0590 s (4235 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/50000: Analyzing
scaling/immutabledb_open/mmap_cached/50000
                        time:   [1.1873 ms 1.1922 ms 1.1984 ms]
Benchmarking scaling/immutabledb_open/in_memory/100000
Benchmarking scaling/immutabledb_open/in_memory/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 6.6s.
Benchmarking scaling/immutabledb_open/in_memory/100000: Collecting 10 samples in estimated 6.6051 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/100000: Analyzing
scaling/immutabledb_open/in_memory/100000
                        time:   [512.98 ms 514.82 ms 517.82 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/100000
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Collecting 10 samples in estimated 5.1178 s (1705 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/100000: Analyzing
scaling/immutabledb_open/mmap_cached/100000
                        time:   [2.7190 ms 2.7718 ms 2.8405 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking scaling/immutabledb_open/in_memory/250000
Benchmarking scaling/immutabledb_open/in_memory/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 33.1s.
Benchmarking scaling/immutabledb_open/in_memory/250000: Collecting 10 samples in estimated 33.147 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/250000: Analyzing
scaling/immutabledb_open/in_memory/250000
                        time:   [1.4006 s 1.5535 s 1.7875 s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/250000
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Collecting 10 samples in estimated 5.4216 s (605 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/250000: Analyzing
scaling/immutabledb_open/mmap_cached/250000
                        time:   [8.9148 ms 8.9367 ms 8.9646 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/in_memory/500000
Benchmarking scaling/immutabledb_open/in_memory/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 298.6s.
Benchmarking scaling/immutabledb_open/in_memory/500000: Collecting 10 samples in estimated 298.64 s (10 iterations)
Benchmarking scaling/immutabledb_open/in_memory/500000: Analyzing
scaling/immutabledb_open/in_memory/500000
                        time:   [22.536 s 22.780 s 23.136 s]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
Benchmarking scaling/immutabledb_open/mmap_cached/500000
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Warming up for 3.0000 s
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Collecting 10 samples in estimated 5.8939 s (330 iterations)
Benchmarking scaling/immutabledb_open/mmap_cached/500000: Analyzing
scaling/immutabledb_open/mmap_cached/500000
                        time:   [17.764 ms 17.891 ms 18.022 ms]

Benchmarking scaling/chaindb_insert/default_20kb/10000
Benchmarking scaling/chaindb_insert/default_20kb/10000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 32.4s.
Benchmarking scaling/chaindb_insert/default_20kb/10000: Collecting 10 samples in estimated 32.354 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/10000: Analyzing
scaling/chaindb_insert/default_20kb/10000
                        time:   [2.6359 s 2.6655 s 2.7002 s]
Benchmarking scaling/chaindb_insert/default_20kb/50000
Benchmarking scaling/chaindb_insert/default_20kb/50000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 155.4s.
Benchmarking scaling/chaindb_insert/default_20kb/50000: Collecting 10 samples in estimated 155.42 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/50000: Analyzing
scaling/chaindb_insert/default_20kb/50000
                        time:   [13.617 s 13.716 s 13.812 s]
Benchmarking scaling/chaindb_insert/default_20kb/100000
Benchmarking scaling/chaindb_insert/default_20kb/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 282.2s.
Benchmarking scaling/chaindb_insert/default_20kb/100000: Collecting 10 samples in estimated 282.19 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/100000: Analyzing
scaling/chaindb_insert/default_20kb/100000
                        time:   [27.186 s 27.400 s 27.651 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking scaling/chaindb_insert/default_20kb/250000
Benchmarking scaling/chaindb_insert/default_20kb/250000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 691.1s.
Benchmarking scaling/chaindb_insert/default_20kb/250000: Collecting 10 samples in estimated 691.06 s (10 iterations)
Benchmarking scaling/chaindb_insert/default_20kb/250000: Analyzing
scaling/chaindb_insert/default_20kb/250000
                        time:   [67.561 s 68.037 s 68.517 s]

```

## UTxO Benchmarks
```
[1m[92m   Compiling[0m zeroize_derive v1.4.3
[1m[92m   Compiling[0m num-integer v0.1.46
[1m[92m   Compiling[0m minicbor v0.25.1
[1m[92m   Compiling[0m minicbor-derive v0.15.3
[1m[92m   Compiling[0m byteorder v1.5.0
[1m[92m   Compiling[0m secp256k1-sys v0.8.2
[1m[92m   Compiling[0m num-bigint v0.4.6
[1m[92m   Compiling[0m radium v0.7.0
[1m[92m   Compiling[0m zeroize v1.8.2
[1m[92m   Compiling[0m log v0.4.29
[1m[92m   Compiling[0m curve25519-dalek v4.1.3
[1m[92m   Compiling[0m ed25519-dalek v2.2.0
[1m[92m   Compiling[0m pallas-codec v0.33.0
[1m[92m   Compiling[0m pallas-crypto v1.0.0-alpha.6
[1m[92m   Compiling[0m num-rational v0.4.2
[1m[92m   Compiling[0m pallas-crypto v0.33.0
[1m[92m   Compiling[0m blst v0.3.16
[1m[92m   Compiling[0m indexmap v1.9.3
[1m[92m   Compiling[0m num_cpus v1.17.0
[1m[92m   Compiling[0m tap v1.0.1
[1m[92m   Compiling[0m heck v0.5.0
[1m[92m   Compiling[0m regex-syntax v0.8.10
[1m[92m   Compiling[0m peg-runtime v0.8.5
[1m[92m   Compiling[0m dugite-primitives v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-primitives)
[1m[92m   Compiling[0m peg-macros v0.8.5
[1m[92m   Compiling[0m strum_macros v0.26.4
[1m[92m   Compiling[0m wyz v0.5.1
[1m[92m   Compiling[0m threadpool v1.8.1
[1m[92m   Compiling[0m pallas-primitives v1.0.0-alpha.6
[1m[92m   Compiling[0m pallas-addresses v1.0.0-alpha.6
[1m[92m   Compiling[0m pallas-addresses v0.33.0
[1m[92m   Compiling[0m pallas-primitives v0.33.0
[1m[92m   Compiling[0m curve25519-dalek v3.2.0 (https://github.com/iquerejeta/curve25519-dalek?branch=ietf03_vrf_compat_ell2#70a36f41)
[1m[92m   Compiling[0m curve25519-dalek v3.2.0
[1m[92m   Compiling[0m miette-derive v5.10.0
[1m[92m   Compiling[0m funty v2.0.0
[1m[92m   Compiling[0m arrayvec v0.5.2
[1m[92m   Compiling[0m hashbrown v0.12.3
[1m[92m   Compiling[0m unicode-segmentation v1.13.2
[1m[92m   Compiling[0m unicode-width v0.1.14
[1m[92m   Compiling[0m typed-arena v2.0.2
[1m[92m   Compiling[0m pretty v0.11.3
[1m[92m   Compiling[0m miette v5.10.0
[1m[92m   Compiling[0m pallas-traverse v0.33.0
[1m[92m   Compiling[0m pallas-traverse v1.0.0-alpha.6
[1m[92m   Compiling[0m peg v0.8.5
[1m[92m   Compiling[0m bitvec v1.0.1
[1m[92m   Compiling[0m vrf_dalek v0.1.0 (https://github.com/input-output-hk/vrf?rev=03ac038e9b92c754ebbcb71824866d93f25e27f3#03ac038e)
[1m[92m   Compiling[0m secp256k1 v0.26.0
[1m[92m   Compiling[0m strum v0.26.3
[1m[92m   Compiling[0m regex-automata v0.4.14
[1m[92m   Compiling[0m itertools v0.10.5
[1m[92m   Compiling[0m fs2 v0.4.3
[1m[92m   Compiling[0m wait-timeout v0.2.1
[1m[92m   Compiling[0m hamming v0.1.3
[1m[92m   Compiling[0m fnv v1.0.7
[1m[92m   Compiling[0m quick-error v1.2.3
[1m[92m   Compiling[0m bit-vec v0.8.0
[1m[92m   Compiling[0m bit-set v0.8.0
[1m[92m   Compiling[0m rusty-fork v0.3.1
[1m[92m   Compiling[0m uplc v1.1.21 (https://github.com/aiken-lang/aiken.git?rev=6806d52211fbb469ae13aa0e0290aeb6b9b3e8cf#6806d522)
[1m[92m   Compiling[0m dugite-lsm v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-lsm)
[1m[92m   Compiling[0m regex v1.12.3
[1m[92m   Compiling[0m dugite-crypto v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-crypto)
[1m[92m   Compiling[0m dugite-serialization v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-serialization)
[1m[92m   Compiling[0m rand_xorshift v0.4.0
[1m[92m   Compiling[0m bincode v1.3.3
[1m[92m   Compiling[0m unarray v0.1.4
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m   Compiling[0m proptest v1.11.0
[1m[92m   Compiling[0m dugite-ledger v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-ledger)
[1m[92m    Finished[0m `bench` profile [optimized] target(s) in 1m 09s
[1m[92m     Running[0m benches/utxo_bench.rs (target/release/deps/utxo_bench-e4761f70421c8e01)
Gnuplot not found, using plotters backend
Benchmarking utxo_store/insert/default/1000000
Benchmarking utxo_store/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.6s.
Benchmarking utxo_store/insert/default/1000000: Collecting 10 samples in estimated 20.646 s (10 iterations)
Benchmarking utxo_store/insert/default/1000000: Analyzing
utxo_store/insert/default/1000000
                        time:   [2.0548 s 2.0692 s 2.0830 s]

Benchmarking utxo_store/lookup/hit/1000000
Benchmarking utxo_store/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/hit/1000000: Collecting 100 samples in estimated 5.9298 s (10k iterations)
Benchmarking utxo_store/lookup/hit/1000000: Analyzing
utxo_store/lookup/hit/1000000
                        time:   [581.35 µs 582.45 µs 583.67 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup/miss/1000000
Benchmarking utxo_store/lookup/miss/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup/miss/1000000: Collecting 100 samples in estimated 5.7305 s (20k iterations)
Benchmarking utxo_store/lookup/miss/1000000: Analyzing
utxo_store/lookup/miss/1000000
                        time:   [282.30 µs 282.88 µs 283.66 µs]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe

Benchmarking utxo_store/contains/hit
Benchmarking utxo_store/contains/hit: Warming up for 3.0000 s
Benchmarking utxo_store/contains/hit: Collecting 100 samples in estimated 5.0194 s (10k iterations)
Benchmarking utxo_store/contains/hit: Analyzing
utxo_store/contains/hit time:   [481.04 µs 482.38 µs 484.14 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking utxo_store/contains/miss
Benchmarking utxo_store/contains/miss: Warming up for 3.0000 s
Benchmarking utxo_store/contains/miss: Collecting 100 samples in estimated 6.0042 s (20k iterations)
Benchmarking utxo_store/contains/miss: Analyzing
utxo_store/contains/miss
                        time:   [283.96 µs 284.86 µs 286.04 µs]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking utxo_store/remove/sequential/1000000
Benchmarking utxo_store/remove/sequential/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 35.9s.
Benchmarking utxo_store/remove/sequential/1000000: Collecting 10 samples in estimated 35.858 s (10 iterations)
Benchmarking utxo_store/remove/sequential/1000000: Analyzing
utxo_store/remove/sequential/1000000
                        time:   [1.8405 s 1.8482 s 1.8558 s]

Benchmarking utxo_store/apply_tx/block_50tx_3in_2out
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Collecting 10 samples in estimated 20.096 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_50tx_3in_2out: Analyzing
utxo_store/apply_tx/block_50tx_3in_2out
                        time:   [302.02 ms 308.22 ms 316.99 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Collecting 10 samples in estimated 20.116 s (10 iterations)
Benchmarking utxo_store/apply_tx/block_300tx_2in_2out: Analyzing
utxo_store/apply_tx/block_300tx_2in_2out
                        time:   [306.56 ms 309.46 ms 311.80 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.4s.
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Collecting 10 samples in estimated 27.413 s (10 iterations)
Benchmarking utxo_store/multi_asset/insert_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/insert_mixed_30pct/1000000
                        time:   [2.6758 s 2.6869 s 2.6991 s]
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 27.0s.
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Collecting 10 samples in estimated 26.992 s (10 iterations)
Benchmarking utxo_store/multi_asset/lookup_mixed_30pct/1000000: Analyzing
utxo_store/multi_asset/lookup_mixed_30pct/1000000
                        time:   [125.74 ms 132.25 ms 139.04 ms]

Benchmarking utxo_store/total_lovelace/scan/1000000
Benchmarking utxo_store/total_lovelace/scan/1000000: Warming up for 3.0000 s

Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 28.2s, or reduce sample count to 10.
Benchmarking utxo_store/total_lovelace/scan/1000000: Collecting 100 samples in estimated 28.170 s (100 iterations)
Benchmarking utxo_store/total_lovelace/scan/1000000: Analyzing
utxo_store/total_lovelace/scan/1000000
                        time:   [285.44 ms 287.08 ms 289.11 ms]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

Benchmarking utxo_store/rebuild_address_index/rebuild/1000000
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 30.1s.
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Collecting 10 samples in estimated 30.141 s (10 iterations)
Benchmarking utxo_store/rebuild_address_index/rebuild/1000000: Analyzing
utxo_store/rebuild_address_index/rebuild/1000000
                        time:   [528.43 ms 531.37 ms 534.96 ms]

Benchmarking utxo_store/insert_configs/low_8gb/1000000
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Collecting 10 samples in estimated 20.144 s (10 iterations)
Benchmarking utxo_store/insert_configs/low_8gb/1000000: Analyzing
utxo_store/insert_configs/low_8gb/1000000
                        time:   [2.0025 s 2.0076 s 2.0130 s]
Benchmarking utxo_store/insert_configs/mid_16gb/1000000
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Collecting 10 samples in estimated 20.057 s (10 iterations)
Benchmarking utxo_store/insert_configs/mid_16gb/1000000: Analyzing
utxo_store/insert_configs/mid_16gb/1000000
                        time:   [2.0242 s 2.0291 s 2.0358 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high severe
Benchmarking utxo_store/insert_configs/high_32gb/1000000
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Collecting 10 samples in estimated 20.108 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_32gb/1000000: Analyzing
utxo_store/insert_configs/high_32gb/1000000
                        time:   [2.0007 s 2.0057 s 2.0107 s]
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.0s.
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Collecting 10 samples in estimated 20.019 s (10 iterations)
Benchmarking utxo_store/insert_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/insert_configs/high_bloom_16gb/1000000
                        time:   [2.0071 s 2.0193 s 2.0330 s]
Benchmarking utxo_store/insert_configs/legacy_small/1000000
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 19.9s.
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Collecting 10 samples in estimated 19.890 s (10 iterations)
Benchmarking utxo_store/insert_configs/legacy_small/1000000: Analyzing
utxo_store/insert_configs/legacy_small/1000000
                        time:   [1.9943 s 2.0031 s 2.0108 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low mild

Benchmarking utxo_store/lookup_configs/low_8gb/1000000
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Collecting 100 samples in estimated 7.3950 s (15k iterations)
Benchmarking utxo_store/lookup_configs/low_8gb/1000000: Analyzing
utxo_store/lookup_configs/low_8gb/1000000
                        time:   [486.61 µs 487.64 µs 488.76 µs]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Collecting 100 samples in estimated 7.3847 s (15k iterations)
Benchmarking utxo_store/lookup_configs/mid_16gb/1000000: Analyzing
utxo_store/lookup_configs/mid_16gb/1000000
                        time:   [486.88 µs 488.22 µs 489.68 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_32gb/1000000
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Collecting 100 samples in estimated 7.4705 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_32gb/1000000: Analyzing
utxo_store/lookup_configs/high_32gb/1000000
                        time:   [494.22 µs 496.18 µs 498.84 µs]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Collecting 100 samples in estimated 7.4540 s (15k iterations)
Benchmarking utxo_store/lookup_configs/high_bloom_16gb/1000000: Analyzing
utxo_store/lookup_configs/high_bloom_16gb/1000000
                        time:   [493.19 µs 494.33 µs 495.57 µs]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking utxo_store/lookup_configs/legacy_small/1000000
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Warming up for 3.0000 s
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Collecting 100 samples in estimated 7.4844 s (15k iterations)
Benchmarking utxo_store/lookup_configs/legacy_small/1000000: Analyzing
utxo_store/lookup_configs/legacy_small/1000000
                        time:   [495.40 µs 496.36 µs 497.38 µs]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe

Benchmarking utxo_scaling/insert/default/100000
Benchmarking utxo_scaling/insert/default/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.1s or enable flat sampling.
Benchmarking utxo_scaling/insert/default/100000: Collecting 10 samples in estimated 8.1218 s (55 iterations)
Benchmarking utxo_scaling/insert/default/100000: Analyzing
utxo_scaling/insert/default/100000
                        time:   [151.19 ms 152.94 ms 154.73 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_scaling/insert/default/500000
Benchmarking utxo_scaling/insert/default/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.2s.
Benchmarking utxo_scaling/insert/default/500000: Collecting 10 samples in estimated 9.1768 s (10 iterations)
Benchmarking utxo_scaling/insert/default/500000: Analyzing
utxo_scaling/insert/default/500000
                        time:   [916.91 ms 919.44 ms 921.57 ms]
Benchmarking utxo_scaling/insert/default/1000000
Benchmarking utxo_scaling/insert/default/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.5s.
Benchmarking utxo_scaling/insert/default/1000000: Collecting 10 samples in estimated 20.502 s (10 iterations)
Benchmarking utxo_scaling/insert/default/1000000: Analyzing
utxo_scaling/insert/default/1000000
                        time:   [2.0053 s 2.0147 s 2.0244 s]

Benchmarking utxo_scaling/lookup/hit/100000
Benchmarking utxo_scaling/lookup/hit/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/100000: Collecting 10 samples in estimated 5.0084 s (14k iterations)
Benchmarking utxo_scaling/lookup/hit/100000: Analyzing
utxo_scaling/lookup/hit/100000
                        time:   [370.10 µs 387.34 µs 425.79 µs]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  2 (20.00%) high severe
Benchmarking utxo_scaling/lookup/hit/500000
Benchmarking utxo_scaling/lookup/hit/500000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/500000: Collecting 10 samples in estimated 5.0053 s (11k iterations)
Benchmarking utxo_scaling/lookup/hit/500000: Analyzing
utxo_scaling/lookup/hit/500000
                        time:   [452.52 µs 456.53 µs 462.17 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low severe
  1 (10.00%) high severe
Benchmarking utxo_scaling/lookup/hit/1000000
Benchmarking utxo_scaling/lookup/hit/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/lookup/hit/1000000: Collecting 10 samples in estimated 5.0055 s (10k iterations)
Benchmarking utxo_scaling/lookup/hit/1000000: Analyzing
utxo_scaling/lookup/hit/1000000
                        time:   [490.94 µs 491.27 µs 491.51 µs]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) low severe

Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 8.1s or enable flat sampling.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Collecting 10 samples in estimated 8.1289 s (55 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/100000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/100000
                        time:   [21.923 ms 22.708 ms 23.253 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.1s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Collecting 10 samples in estimated 9.0596 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/500000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/500000
                        time:   [148.00 ms 148.72 ms 149.43 ms]
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 20.1s.
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Collecting 10 samples in estimated 20.090 s (10 iterations)
Benchmarking utxo_scaling/apply_tx/block_50tx_3in_2out/1000000: Analyzing
utxo_scaling/apply_tx/block_50tx_3in_2out/1000000
                        time:   [295.48 ms 297.98 ms 300.73 ms]

Benchmarking utxo_scaling/total_lovelace/scan/100000
Benchmarking utxo_scaling/total_lovelace/scan/100000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/100000: Collecting 10 samples in estimated 5.6566 s (220 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/100000: Analyzing
utxo_scaling/total_lovelace/scan/100000
                        time:   [26.158 ms 26.185 ms 26.209 ms]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) low mild
  1 (10.00%) high mild
Benchmarking utxo_scaling/total_lovelace/scan/500000
Benchmarking utxo_scaling/total_lovelace/scan/500000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 7.4s or enable flat sampling.
Benchmarking utxo_scaling/total_lovelace/scan/500000: Collecting 10 samples in estimated 7.4093 s (55 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/500000: Analyzing
utxo_scaling/total_lovelace/scan/500000
                        time:   [135.75 ms 137.12 ms 138.05 ms]
Benchmarking utxo_scaling/total_lovelace/scan/1000000
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Warming up for 3.0000 s
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Collecting 10 samples in estimated 5.6799 s (20 iterations)
Benchmarking utxo_scaling/total_lovelace/scan/1000000: Analyzing
utxo_scaling/total_lovelace/scan/1000000
                        time:   [286.17 ms 287.46 ms 288.77 ms]

Benchmarking utxo_large_scale/insert/default/5000000
Benchmarking utxo_large_scale/insert/default/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 127.1s.
Benchmarking utxo_large_scale/insert/default/5000000: Collecting 10 samples in estimated 127.14 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/5000000: Analyzing
utxo_large_scale/insert/default/5000000
                        time:   [12.508 s 12.544 s 12.577 s]
Benchmarking utxo_large_scale/insert/default/10000000
Benchmarking utxo_large_scale/insert/default/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 286.7s.
Benchmarking utxo_large_scale/insert/default/10000000: Collecting 10 samples in estimated 286.68 s (10 iterations)
Benchmarking utxo_large_scale/insert/default/10000000: Analyzing
utxo_large_scale/insert/default/10000000
                        time:   [27.170 s 27.310 s 27.495 s]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_large_scale/lookup/hit/5000000
Benchmarking utxo_large_scale/lookup/hit/5000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/5000000: Collecting 10 samples in estimated 5.0578 s (3410 iterations)
Benchmarking utxo_large_scale/lookup/hit/5000000: Analyzing
utxo_large_scale/lookup/hit/5000000
                        time:   [1.4639 ms 1.4675 ms 1.4718 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
Benchmarking utxo_large_scale/lookup/hit/10000000
Benchmarking utxo_large_scale/lookup/hit/10000000: Warming up for 3.0000 s
Benchmarking utxo_large_scale/lookup/hit/10000000: Collecting 10 samples in estimated 5.0058 s (2750 iterations)
Benchmarking utxo_large_scale/lookup/hit/10000000: Analyzing
utxo_large_scale/lookup/hit/10000000
                        time:   [1.8086 ms 1.8204 ms 1.8378 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild

Benchmarking utxo_large_scale/total_lovelace/scan/5000000
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 15.0s.
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Collecting 10 samples in estimated 14.965 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/5000000: Analyzing
utxo_large_scale/total_lovelace/scan/5000000
                        time:   [1.4874 s 1.4891 s 1.4907 s]
Benchmarking utxo_large_scale/total_lovelace/scan/10000000
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Warming up for 3.0000 s

Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 29.4s.
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Collecting 10 samples in estimated 29.433 s (10 iterations)
Benchmarking utxo_large_scale/total_lovelace/scan/10000000: Analyzing
utxo_large_scale/total_lovelace/scan/10000000
                        time:   [2.9143 s 2.9273 s 2.9414 s]

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
[1m[92m   Compiling[0m dugite-lsm v1.4.0 (/home/runner/work/dugite/dugite/crates/dugite-lsm)
[1m[92m   Compiling[0m zerocopy v0.8.48
[1m[92m   Compiling[0m tinytemplate v1.2.1
[1m[92m   Compiling[0m half v2.7.1
[1m[92m   Compiling[0m ppv-lite86 v0.2.21
[1m[92m   Compiling[0m ciborium-ll v0.2.2
[1m[92m   Compiling[0m rand_chacha v0.9.0
[1m[92m   Compiling[0m ciborium v0.2.2
[1m[92m   Compiling[0m proptest v1.11.0
[1m[92m   Compiling[0m criterion v0.8.2
[1m[92m    Finished[0m `release` profile [optimized] target(s) in 29.20s
[1m[92m     Running[0m unittests src/lib.rs (target/release/deps/dugite_lsm-92e0a9a6efe83f91)

running 3 tests
test tree::mainnet_scale_tests::test_mainnet_scale_wal_crash_recovery ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_insert_read ... ok
test tree::mainnet_scale_tests::test_mainnet_scale_delete_amplification ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 7.36s

[1m[92m   Doc-tests[0m dugite_lsm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

```
