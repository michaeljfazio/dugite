# Third-Party Licenses

Dugite depends on a number of open-source Rust crates. This page documents
all third-party dependencies and their license terms.

**Total dependencies:** 560

_Generated from `Cargo.lock` on 2026-07-31 at commit `3cbe3986c8`. Regenerate with `just licenses` after any dependency change — nothing in CI does it for you._

Dugite itself is licensed under **Apache-2.0**. Counts below are per unique crate name (the highest version, where a crate appears at several versions) across all target platforms, so they include target-gated dependencies such as the `windows-*` family that are not built on Linux or macOS.

## License Summary

| License | Count |
|---------|-------|
| MIT OR Apache-2.0 | 285 |
| MIT | 119 |
| Apache-2.0 OR MIT | 44 |
| Apache-2.0 | 22 |
| Unicode-3.0 | 18 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 14 |
| Unlicense OR MIT | 6 |
| Zlib OR Apache-2.0 OR MIT | 6 |
| BSD-3-Clause | 4 |
| MPL-2.0+ | 3 |
| Apache-2.0 OR ISC OR MIT | 3 |
| ISC | 3 |
| CC0-1.0 OR MIT-0 OR Apache-2.0 | 2 |
| MIT OR Apache-2.0 OR Zlib | 2 |
| BlueOak-1.0.0 | 2 |
| CDLA-Permissive-2.0 | 2 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| Apache-2.0 WITH LLVM-exception | 1 |
| BSD-2-Clause | 1 |
| ISC AND (Apache-2.0 OR ISC) | 1 |
| ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) | 1 |
| (Apache-2.0 OR MIT) AND BSD-3-Clause | 1 |
| MIT OR Apache-2.0 OR CC0-1.0 | 1 |
| MIT OR Apache-2.0 OR BSD-1-Clause | 1 |
| (MIT OR Apache-2.0) AND Unicode-DFS-2016 | 1 |
| Apache-2.0  OR  MIT | 1 |
| Zlib | 1 |
| MIT AND BSD-3-Clause | 1 |
| MIT OR Zlib OR Apache-2.0 | 1 |
| (MIT OR Apache-2.0) AND Apache-2.0 | 1 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| MPL-2.0 OR MIT OR Apache-2.0 | 1 |
| MIT OR MPL-2.0 | 1 |
| WTFPL | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| Unknown | 1 |
| MIT AND Unicode-DFS-2016 | 1 |

## Licenses Needing Review

Everything not covered by a plainly permissive license (MIT, Apache-2.0, BSD, ISC, Zlib, CC0, Unicode-3.0, BlueOak, CDLA-Permissive, or MPL-2.0 file-level copyleft). Review these before shipping a binary distribution:

| Crate | Version | License |
|-------|---------|---------|
| bitmaps | 3.2.1 | MPL-2.0+ |
| imbl | 7.0.0 | MPL-2.0+ |
| imbl-sized-chunks | 0.1.3 | MPL-2.0+ |
| terminfo | 0.9.0 | WTFPL |
| vrf_dalek | 0.1.0 | Unknown |
| wezterm-bidi | 0.2.3 | MIT AND Unicode-DFS-2016 |

## Key Dependencies

These are the primary libraries that Dugite directly depends on:

| Crate | Version | License | Description |
|-------|---------|---------|-------------|
| [tokio](https://github.com/tokio-rs/tokio) | 1.52.3 | MIT | An event-driven, non-blocking I/O platform for writing asynchronous I/O
backe... |
| [tokio-util](https://github.com/tokio-rs/tokio) | 0.7.18 | MIT | Additional utilities for working with Tokio. |
| [hyper](https://github.com/hyperium/hyper) | 1.10.1 | MIT | A protective and efficient HTTP library for all. |
| [reqwest](https://github.com/seanmonstar/reqwest) | 0.13.3 | MIT OR Apache-2.0 | higher level HTTP client library |
| [socket2](https://github.com/rust-lang/socket2) | 0.6.3 | MIT OR Apache-2.0 | Utilities for handling networking sockets with a maximal amount of configurat... |
| [hickory-resolver](https://github.com/hickory-dns/hickory-dns) | 0.26.1 | MIT OR Apache-2.0 | hickory-resolver is a safe and secure DNS stub resolver library intended to b... |
| [tonic](https://github.com/hyperium/tonic) | 0.14.6 | MIT | A gRPC over HTTP/2 implementation focused on high performance, interoperabili... |
| [prost](https://github.com/tokio-rs/prost) | 0.14.4 | Apache-2.0 | A Protocol Buffers implementation for the Rust Language. |
| [serde](https://github.com/serde-rs/serde) | 1.0.228 | MIT OR Apache-2.0 | A generic serialization/deserialization framework |
| [serde_json](https://github.com/serde-rs/json) | 1.0.150 | MIT OR Apache-2.0 | A JSON serialization file format |
| [minicbor](https://github.com/twittner/minicbor) | 0.26.5 | BlueOak-1.0.0 | A small CBOR codec suitable for no_std environments. |
| [bincode](https://github.com/bincode-org/bincode) | 2.0.1 | MIT | A binary serialization / deserialization strategy for transforming structs in... |
| [toml](https://github.com/toml-rs/toml) | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | A native Rust encoder and decoder of TOML-formatted files and streams. Provid... |
| [blake2](https://github.com/RustCrypto/hashes) | 0.9.2 | MIT OR Apache-2.0 | BLAKE2 hash functions |
| [blake2b_simd](https://github.com/oconnor663/blake2_simd) | 1.0.4 | MIT | a pure Rust BLAKE2b implementation with dynamic SIMD |
| [sha2](https://github.com/RustCrypto/hashes) | 0.9.9 | MIT OR Apache-2.0 | Pure Rust implementation of the SHA-2 hash function family
including SHA-224,... |
| [sha3](https://github.com/RustCrypto/hashes) | 0.12.0 | MIT OR Apache-2.0 | Implementation of the SHA-3 family of cryptographic hash algorithms |
| [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek) | 2.2.0 | BSD-3-Clause | Fast and efficient ed25519 EdDSA key generations, signing, and verification i... |
| [curve25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/curve25519-dalek) | 4.1.3 | BSD-3-Clause | A pure-Rust implementation of group operations on ristretto255 and Curve25519 |
| [blst](https://github.com/supranational/blst) | 0.3.16 | Apache-2.0 | Bindings for blst BLS12-381 library |
| [k256](https://github.com/RustCrypto/elliptic-curves/tree/master/k256) | 0.13.4 | Apache-2.0 OR MIT | secp256k1 elliptic curve library written in pure Rust with support for ECDSA
... |
| [kes-summed-ed25519](https://github.com/input-output-hk/kes) | 0.2.1 | Apache-2.0 | Key Evolving Signature |
| vrf_dalek | 0.1.0 | Unknown |  |
| [num-bigint](https://github.com/rust-num/num-bigint) | 0.4.6 | MIT OR Apache-2.0 | Big integer implementation for Rust |
| [num-rational](https://github.com/rust-num/num-rational) | 0.4.2 | MIT OR Apache-2.0 | Rational numbers implementation for Rust |
| [dashu-int](https://github.com/cmpute/dashu) | 0.4.2 | MIT OR Apache-2.0 | A big integer library with good performance |
| [memmap2](https://github.com/RazrFalcon/memmap2-rs) | 0.9.11 | MIT OR Apache-2.0 | Cross-platform Rust API for memory-mapped file IO |
| [fs2](https://github.com/danburkert/fs2-rs) | 0.4.3 | MIT/Apache-2.0 | Cross-platform file locks and file duplication. |
| [imbl](https://github.com/jneem/imbl) | 7.0.0 | MPL-2.0+ | Immutable collection datatypes |
| [crc32fast](https://github.com/srijs/rust-crc32fast) | 1.5.0 | MIT OR Apache-2.0 | Fast, SIMD-accelerated CRC32 (IEEE) checksum computation |
| [zstd](https://github.com/gyscos/zstd-rs) | 0.13.3 | MIT | Binding for the zstd compression library. |
| [tar](https://github.com/composefs/tar-rs) | 0.4.46 | MIT OR Apache-2.0 | A Rust implementation of a TAR file reader and writer. This library does not
... |
| [hex](https://github.com/KokaKiwi/rust-hex) | 0.4.3 | MIT OR Apache-2.0 | Encoding and decoding data into/from hexadecimal representation. |
| [bs58](https://github.com/Nullus157/bs58-rs) | 0.5.1 | MIT/Apache-2.0 | Another Base58 codec implementation. |
| [bech32](https://github.com/rust-bitcoin/rust-bech32) | 0.11.1 | MIT | Encodes and decodes the Bech32 format and implements the bech32 and bech32m c... |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.22.1 | MIT OR Apache-2.0 | encodes and decodes base64 as bytes or utf8 |
| [mithril-client](https://github.com/input-output-hk/mithril/) | 0.14.5 | Apache-2.0 | Mithril client library |
| [clap](https://github.com/clap-rs/clap) | 4.6.1 | MIT OR Apache-2.0 | A simple to use, efficient, and full-featured Command Line Argument Parser |
| [ratatui](https://github.com/ratatui/ratatui) | 0.30.2 | MIT | A library that's all about cooking up terminal user interfaces |
| [crossterm](https://github.com/crossterm-rs/crossterm) | 0.29.0 | MIT | A crossplatform terminal library for manipulating terminals. |
| [indicatif](https://github.com/console-rs/indicatif) | 0.18.6 | MIT | A progress bar and cli reporting library for Rust |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1.44 | MIT | Application-level tracing for Rust. |
| [tracing-subscriber](https://github.com/tokio-rs/tracing) | 0.3.23 | MIT | Utilities for implementing and composing `tracing` subscribers. |
| [dashmap](https://github.com/xacrimon/dashmap) | 6.2.1 | MIT | Blazing fast concurrent HashMap for Rust. |
| [parking_lot](https://github.com/Amanieu/parking_lot) | 0.12.5 | MIT OR Apache-2.0 | More compact and efficient implementations of the standard synchronization pr... |
| [arc-swap](https://github.com/vorner/arc-swap) | 1.9.2 | MIT OR Apache-2.0 | Atomically swappable Arc |
| [rayon](https://github.com/rayon-rs/rayon) | 1.12.0 | MIT OR Apache-2.0 | Simple work-stealing parallelism for Rust |
| [rand](https://github.com/rust-random/rand) | 0.9.4 | MIT OR Apache-2.0 | Random number generators and other randomness functionality. |
| [chrono](https://github.com/chronotope/chrono) | 0.4.44 | MIT OR Apache-2.0 | Date and time library for Rust |

## All Dependencies

Complete list of all third-party crates used by Dugite, sorted alphabetically.

| Crate | Version | License |
|-------|---------|---------|
| [adler2](https://github.com/oyvindln/adler2) | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| [aho-corasick](https://github.com/BurntSushi/aho-corasick) | 1.1.4 | Unlicense OR MIT |
| [alloca](https://github.com/playXE/alloca-rs) | 0.4.0 | MIT |
| [allocator-api2](https://github.com/zakarumych/allocator-api2) | 0.2.21 | MIT OR Apache-2.0 |
| [android_system_properties](https://github.com/nical/android_system_properties) | 0.1.5 | MIT/Apache-2.0 |
| [anes](https://github.com/zrzka/anes-rs) | 0.1.6 | MIT OR Apache-2.0 |
| [anstream](https://github.com/rust-cli/anstyle.git) | 1.0.0 | MIT OR Apache-2.0 |
| [anstyle](https://github.com/rust-cli/anstyle.git) | 1.0.14 | MIT OR Apache-2.0 |
| [anstyle-parse](https://github.com/rust-cli/anstyle.git) | 1.0.0 | MIT OR Apache-2.0 |
| [anstyle-query](https://github.com/rust-cli/anstyle.git) | 1.1.5 | MIT OR Apache-2.0 |
| [anstyle-wincon](https://github.com/rust-cli/anstyle.git) | 3.0.11 | MIT OR Apache-2.0 |
| [anyhow](https://github.com/dtolnay/anyhow) | 1.0.102 | MIT OR Apache-2.0 |
| [approx](https://github.com/brendanzab/approx) | 0.5.1 | Apache-2.0 |
| [ar_archive_writer](https://github.com/rust-lang/ar_archive_writer) | 0.5.1 | Apache-2.0 WITH LLVM-exception |
| [arc-swap](https://github.com/vorner/arc-swap) | 1.9.2 | MIT OR Apache-2.0 |
| [archery](https://github.com/orium/archery) | 1.2.2 | MIT |
| [arraydeque](https://github.com/andylokandy/arraydeque) | 0.5.1 | MIT/Apache-2.0 |
| [arrayref](https://github.com/droundy/arrayref) | 0.3.9 | BSD-2-Clause |
| [arrayvec](https://github.com/bluss/arrayvec) | 0.7.6 | MIT OR Apache-2.0 |
| [async-trait](https://github.com/dtolnay/async-trait) | 0.1.89 | MIT OR Apache-2.0 |
| [atomic](https://github.com/Amanieu/atomic-rs) | 0.6.1 | Apache-2.0/MIT |
| [atomic-waker](https://github.com/smol-rs/atomic-waker) | 1.1.2 | Apache-2.0 OR MIT |
| [autocfg](https://github.com/cuviper/autocfg) | 1.5.0 | Apache-2.0 OR MIT |
| [aws-lc-rs](https://github.com/aws/aws-lc-rs) | 1.17.0 | ISC AND (Apache-2.0 OR ISC) |
| [aws-lc-sys](https://github.com/aws/aws-lc-rs) | 0.41.0 | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) |
| [axum](https://github.com/tokio-rs/axum) | 0.8.9 | MIT |
| [axum-core](https://github.com/tokio-rs/axum) | 0.5.6 | MIT |
| [az](https://gitlab.com/tspiteri/az) | 1.3.0 | MIT/Apache-2.0 |
| [base16ct](https://github.com/RustCrypto/formats/tree/master/base16ct) | 0.2.0 | Apache-2.0 OR MIT |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.22.1 | MIT OR Apache-2.0 |
| [base64ct](https://github.com/RustCrypto/formats) | 1.8.3 | Apache-2.0 OR MIT |
| [bech32](https://github.com/rust-bitcoin/rust-bech32) | 0.11.1 | MIT |
| [bincode](https://github.com/bincode-org/bincode) | 2.0.1 | MIT |
| [bincode_derive](https://github.com/bincode-org/bincode) | 2.0.1 | MIT |
| [bindgen](https://github.com/rust-lang/rust-bindgen) | 0.72.1 | BSD-3-Clause |
| [bit-set](https://github.com/contain-rs/bit-set) | 0.8.0 | Apache-2.0 OR MIT |
| [bit-vec](https://github.com/contain-rs/bit-vec) | 0.8.0 | Apache-2.0 OR MIT |
| [bitflags](https://github.com/bitflags/bitflags) | 2.13.0 | MIT OR Apache-2.0 |
| [bitmaps](https://github.com/bodil/bitmaps) | 3.2.1 | MPL-2.0+ |
| [blake2](https://github.com/RustCrypto/hashes) | 0.9.2 | MIT OR Apache-2.0 |
| [blake2b_simd](https://github.com/oconnor663/blake2_simd) | 1.0.4 | MIT |
| [block-buffer](https://github.com/RustCrypto/utils) | 0.9.0 | MIT OR Apache-2.0 |
| [blst](https://github.com/supranational/blst) | 0.3.16 | Apache-2.0 |
| [bs58](https://github.com/Nullus157/bs58-rs) | 0.5.1 | MIT/Apache-2.0 |
| [bumpalo](https://github.com/fitzgen/bumpalo) | 3.20.2 | MIT OR Apache-2.0 |
| [by_address](https://github.com/mbrubeck/by_address) | 1.2.1 | MIT OR Apache-2.0 |
| [bytemuck](https://github.com/Lokathor/bytemuck) | 1.25.0 | Zlib OR Apache-2.0 OR MIT |
| [byteorder](https://github.com/BurntSushi/byteorder) | 1.5.0 | Unlicense OR MIT |
| [bytes](https://github.com/tokio-rs/bytes) | 1.12.0 | MIT |
| [cast](https://github.com/japaric/cast.rs) | 0.3.0 | MIT OR Apache-2.0 |
| [castaway](https://github.com/sagebind/castaway) | 0.2.4 | MIT |
| [cc](https://github.com/rust-lang/cc-rs) | 1.2.60 | MIT OR Apache-2.0 |
| [cexpr](https://github.com/jethrogb/rust-cexpr) | 0.6.0 | Apache-2.0/MIT |
| [cfg-if](https://github.com/rust-lang/cfg-if) | 1.0.4 | MIT OR Apache-2.0 |
| [cfg_aliases](https://github.com/katharostech/cfg_aliases) | 0.2.1 | MIT |
| [chacha20](https://github.com/RustCrypto/stream-ciphers) | 0.10.0 | MIT OR Apache-2.0 |
| [chrono](https://github.com/chronotope/chrono) | 0.4.44 | MIT OR Apache-2.0 |
| [ciborium](https://github.com/enarx/ciborium) | 0.2.2 | Apache-2.0 |
| [ciborium-io](https://github.com/enarx/ciborium) | 0.2.2 | Apache-2.0 |
| [ciborium-ll](https://github.com/enarx/ciborium) | 0.2.2 | Apache-2.0 |
| [ckb-merkle-mountain-range](https://github.com/nervosnetwork/merkle-mountain-range) | 0.6.1 | MIT |
| [clang-sys](https://github.com/KyleMayes/clang-sys) | 1.8.1 | Apache-2.0 |
| [clap](https://github.com/clap-rs/clap) | 4.6.1 | MIT OR Apache-2.0 |
| [clap_builder](https://github.com/clap-rs/clap) | 4.6.0 | MIT OR Apache-2.0 |
| [clap_derive](https://github.com/clap-rs/clap) | 4.6.1 | MIT OR Apache-2.0 |
| [clap_lex](https://github.com/clap-rs/clap) | 1.1.0 | MIT OR Apache-2.0 |
| [cmake](https://github.com/rust-lang/cmake-rs) | 0.1.58 | MIT OR Apache-2.0 |
| [colorchoice](https://github.com/rust-cli/anstyle.git) | 1.0.5 | MIT OR Apache-2.0 |
| [combine](https://github.com/Marwes/combine) | 4.6.7 | MIT |
| [compact_str](https://github.com/ParkMyCar/compact_str) | 0.9.0 | MIT |
| [console](https://github.com/console-rs/console) | 0.16.4 | MIT |
| [const-oid](https://github.com/RustCrypto/formats/tree/master/const-oid) | 0.9.6 | Apache-2.0 OR MIT |
| [constant_time_eq](https://github.com/cesarb/constant_time_eq) | 0.4.2 | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| [convert_case](https://github.com/rutrum/convert-case) | 0.10.0 | MIT |
| [core-foundation](https://github.com/servo/core-foundation-rs) | 0.9.4 | MIT OR Apache-2.0 |
| [core-foundation-sys](https://github.com/servo/core-foundation-rs) | 0.8.7 | MIT OR Apache-2.0 |
| [cpufeatures](https://github.com/RustCrypto/utils) | 0.3.0 | MIT OR Apache-2.0 |
| [crc32fast](https://github.com/srijs/rust-crc32fast) | 1.5.0 | MIT OR Apache-2.0 |
| [criterion](https://github.com/criterion-rs/criterion.rs) | 0.8.2 | Apache-2.0 OR MIT |
| [criterion-plot](https://github.com/criterion-rs/criterion.rs) | 0.8.2 | Apache-2.0 OR MIT |
| [critical-section](https://github.com/rust-embedded/critical-section) | 1.2.0 | MIT OR Apache-2.0 |
| [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam) | 0.5.15 | MIT OR Apache-2.0 |
| [crossbeam-deque](https://github.com/crossbeam-rs/crossbeam) | 0.8.6 | MIT OR Apache-2.0 |
| [crossbeam-epoch](https://github.com/crossbeam-rs/crossbeam) | 0.9.18 | MIT OR Apache-2.0 |
| [crossbeam-utils](https://github.com/crossbeam-rs/crossbeam) | 0.8.21 | MIT OR Apache-2.0 |
| [crossterm](https://github.com/crossterm-rs/crossterm) | 0.29.0 | MIT |
| [crossterm_winapi](https://github.com/crossterm-rs/crossterm-winapi) | 0.9.1 | MIT |
| [crunchy](https://github.com/eira-fransham/crunchy) | 0.2.4 | MIT |
| [crypto-bigint](https://github.com/RustCrypto/crypto-bigint) | 0.5.5 | Apache-2.0 OR MIT |
| [crypto-common](https://github.com/RustCrypto/traits) | 0.2.1 | MIT OR Apache-2.0 |
| [crypto-mac](https://github.com/RustCrypto/traits) | 0.8.0 | MIT OR Apache-2.0 |
| [csscolorparser](https://github.com/mazznoer/csscolorparser-rs) | 0.6.2 | MIT OR Apache-2.0 |
| [curve25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/curve25519-dalek) | 4.1.3 | BSD-3-Clause |
| [curve25519-dalek-derive](https://github.com/dalek-cryptography/curve25519-dalek) | 0.1.1 | MIT/Apache-2.0 |
| [darling](https://github.com/TedDriggs/darling) | 0.23.0 | MIT |
| [darling_core](https://github.com/TedDriggs/darling) | 0.23.0 | MIT |
| [darling_macro](https://github.com/TedDriggs/darling) | 0.23.0 | MIT |
| [dashmap](https://github.com/xacrimon/dashmap) | 6.2.1 | MIT |
| [dashu-base](https://github.com/cmpute/dashu) | 0.4.3 | MIT OR Apache-2.0 |
| [dashu-int](https://github.com/cmpute/dashu) | 0.4.2 | MIT OR Apache-2.0 |
| [data-encoding](https://github.com/ia0/data-encoding) | 2.11.0 | MIT |
| [deltae](https://gitlab.com/ryanobeirne/deltae.git) | 0.3.2 | MIT |
| [der](https://github.com/RustCrypto/formats/tree/master/der) | 0.7.10 | Apache-2.0 OR MIT |
| [deranged](https://github.com/jhpratt/deranged) | 0.5.8 | MIT OR Apache-2.0 |
| [derive_more](https://github.com/JelteF/derive_more) | 2.1.1 | MIT |
| [derive_more-impl](https://github.com/JelteF/derive_more) | 2.1.1 | MIT |
| [digest](https://github.com/RustCrypto/traits) | 0.9.0 | MIT OR Apache-2.0 |
| [displaydoc](https://github.com/yaahc/displaydoc) | 0.2.5 | MIT OR Apache-2.0 |
| [document-features](https://github.com/slint-ui/document-features) | 0.2.12 | MIT OR Apache-2.0 |
| [dunce](https://gitlab.com/kornelski/dunce) | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| [dyn-clone](https://github.com/dtolnay/dyn-clone) | 1.0.20 | MIT OR Apache-2.0 |
| [ecdsa](https://github.com/RustCrypto/signatures/tree/master/ecdsa) | 0.16.9 | Apache-2.0 OR MIT |
| [ed25519](https://github.com/RustCrypto/signatures/tree/master/ed25519) | 2.2.3 | Apache-2.0 OR MIT |
| [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek) | 2.2.0 | BSD-3-Clause |
| [either](https://github.com/rayon-rs/either) | 1.15.0 | MIT OR Apache-2.0 |
| [elliptic-curve](https://github.com/RustCrypto/traits/tree/master/elliptic-curve) | 0.13.8 | Apache-2.0 OR MIT |
| [encode_unicode](https://github.com/tormol/encode_unicode) | 1.0.0 | Apache-2.0 OR MIT |
| [encoding_rs](https://github.com/hsivonen/encoding_rs) | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| [equivalent](https://github.com/indexmap-rs/equivalent) | 1.0.2 | Apache-2.0 OR MIT |
| [erased-serde](https://github.com/dtolnay/erased-serde) | 0.4.10 | MIT OR Apache-2.0 |
| [errno](https://github.com/lambda-fairy/rust-errno) | 0.3.14 | MIT OR Apache-2.0 |
| [euclid](https://github.com/servo/euclid) | 0.22.14 | MIT OR Apache-2.0 |
| [fancy-regex](https://github.com/fancy-regex/fancy-regex) | 0.11.0 | MIT |
| [fast-srgb8](https://github.com/thomcc/fast-srgb8) | 1.0.0 | MIT OR Apache-2.0 OR CC0-1.0 |
| [fastrand](https://github.com/smol-rs/fastrand) | 2.4.1 | Apache-2.0 OR MIT |
| [ff](https://github.com/zkcrypto/ff) | 0.13.1 | MIT/Apache-2.0 |
| [fiat-crypto](https://github.com/mit-plv/fiat-crypto) | 0.2.9 | MIT OR Apache-2.0 OR BSD-1-Clause |
| [filedescriptor](https://github.com/wezterm/wezterm) | 0.8.3 | MIT |
| [filetime](https://github.com/alexcrichton/filetime) | 0.2.27 | MIT/Apache-2.0 |
| [find-msvc-tools](https://github.com/rust-lang/cc-rs) | 0.1.9 | MIT OR Apache-2.0 |
| [finl_unicode](https://github.com/dahosek/finl_unicode) | 1.4.0 | (MIT OR Apache-2.0) AND Unicode-DFS-2016 |
| [fixed](https://gitlab.com/tspiteri/fixed) | 1.31.0 | MIT/Apache-2.0 |
| [fixedbitset](https://github.com/petgraph/fixedbitset) | 0.5.7 | MIT OR Apache-2.0 |
| [flate2](https://github.com/rust-lang/flate2-rs) | 1.1.9 | MIT OR Apache-2.0 |
| [flume](https://github.com/zesterer/flume) | 0.12.0 | Apache-2.0/MIT |
| [fnv](https://github.com/servo/rust-fnv) | 1.0.7 | Apache-2.0 / MIT |
| [foldhash](https://github.com/orlp/foldhash) | 0.2.0 | Zlib |
| [form_urlencoded](https://github.com/servo/rust-url) | 1.2.2 | MIT OR Apache-2.0 |
| [fs2](https://github.com/danburkert/fs2-rs) | 0.4.3 | MIT/Apache-2.0 |
| [fs_extra](https://github.com/webdesus/fs_extra) | 1.3.0 | MIT |
| [futures](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-channel](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-core](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-executor](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-io](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-macro](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-sink](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-task](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [futures-util](https://github.com/rust-lang/futures-rs) | 0.3.32 | MIT OR Apache-2.0 |
| [generic-array](https://github.com/fizyk20/generic-array.git) | 0.14.9 | MIT |
| [getrandom](https://github.com/rust-random/getrandom) | 0.4.2 | MIT OR Apache-2.0 |
| [glob](https://github.com/rust-lang/glob) | 0.3.3 | MIT OR Apache-2.0 |
| [group](https://github.com/zkcrypto/group) | 0.13.0 | MIT/Apache-2.0 |
| [h2](https://github.com/hyperium/h2) | 0.4.15 | MIT |
| [half](https://github.com/VoidStarKat/half-rs) | 2.7.1 | MIT OR Apache-2.0 |
| [hashbrown](https://github.com/rust-lang/hashbrown) | 0.17.0 | MIT OR Apache-2.0 |
| [hashlink](https://github.com/kyren/hashlink) | 0.10.0 | MIT OR Apache-2.0 |
| [heck](https://github.com/withoutboats/heck) | 0.5.0 | MIT OR Apache-2.0 |
| [hermit-abi](https://github.com/hermit-os/hermit-rs) | 0.5.2 | MIT OR Apache-2.0 |
| [hex](https://github.com/KokaKiwi/rust-hex) | 0.4.3 | MIT OR Apache-2.0 |
| [hickory-net](https://github.com/hickory-dns/hickory-dns) | 0.26.1 | MIT OR Apache-2.0 |
| [hickory-proto](https://github.com/hickory-dns/hickory-dns) | 0.26.1 | MIT OR Apache-2.0 |
| [hickory-resolver](https://github.com/hickory-dns/hickory-dns) | 0.26.1 | MIT OR Apache-2.0 |
| [hmac](https://github.com/RustCrypto/MACs) | 0.12.1 | MIT OR Apache-2.0 |
| [http](https://github.com/hyperium/http) | 1.4.0 | MIT OR Apache-2.0 |
| [http-body](https://github.com/hyperium/http-body) | 1.0.1 | MIT |
| [http-body-util](https://github.com/hyperium/http-body) | 0.1.3 | MIT |
| [httparse](https://github.com/seanmonstar/httparse) | 1.10.1 | MIT OR Apache-2.0 |
| [httpdate](https://github.com/pyfisch/httpdate) | 1.0.3 | MIT OR Apache-2.0 |
| [hybrid-array](https://github.com/RustCrypto/hybrid-array) | 0.4.10 | MIT OR Apache-2.0 |
| [hyper](https://github.com/hyperium/hyper) | 1.10.1 | MIT |
| [hyper-rustls](https://github.com/rustls/hyper-rustls) | 0.27.8 | Apache-2.0 OR ISC OR MIT |
| [hyper-timeout](https://github.com/hjr3/hyper-timeout) | 0.5.2 | MIT OR Apache-2.0 |
| [hyper-util](https://github.com/hyperium/hyper-util) | 0.1.20 | MIT |
| [iana-time-zone](https://github.com/strawlab/iana-time-zone) | 0.1.65 | MIT OR Apache-2.0 |
| [iana-time-zone-haiku](https://github.com/strawlab/iana-time-zone) | 0.1.2 | MIT OR Apache-2.0 |
| [icu_collections](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_locale_core](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_normalizer](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_normalizer_data](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_properties](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_properties_data](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [icu_provider](https://github.com/unicode-org/icu4x) | 2.2.0 | Unicode-3.0 |
| [id-arena](https://github.com/fitzgen/id-arena) | 2.3.0 | MIT/Apache-2.0 |
| [ident_case](https://github.com/TedDriggs/ident_case) | 1.0.1 | MIT/Apache-2.0 |
| [idna](https://github.com/servo/rust-url/) | 1.1.0 | MIT OR Apache-2.0 |
| [idna_adapter](https://github.com/hsivonen/idna_adapter) | 1.2.1 | Apache-2.0 OR MIT |
| [imbl](https://github.com/jneem/imbl) | 7.0.0 | MPL-2.0+ |
| [imbl-sized-chunks](https://github.com/jneem/imbl-sized-chunks) | 0.1.3 | MPL-2.0+ |
| [indexmap](https://github.com/indexmap-rs/indexmap) | 2.14.0 | Apache-2.0 OR MIT |
| [indicatif](https://github.com/console-rs/indicatif) | 0.18.6 | MIT |
| [indoc](https://github.com/dtolnay/indoc) | 2.0.7 | MIT OR Apache-2.0 |
| [instability](https://github.com/ratatui/instability) | 0.3.12 | MIT |
| [inventory](https://github.com/dtolnay/inventory) | 0.3.24 | MIT OR Apache-2.0 |
| [ipconfig](https://github.com/liranringel/ipconfig) | 0.3.4 | MIT/Apache-2.0 |
| [ipnet](https://github.com/krisprice/ipnet) | 2.12.0 | MIT OR Apache-2.0 |
| [iri-string](https://github.com/lo48576/iri-string) | 0.7.12 | MIT OR Apache-2.0 |
| [is_terminal_polyfill](https://github.com/polyfill-rs/is_terminal_polyfill) | 1.70.2 | MIT OR Apache-2.0 |
| [itertools](https://github.com/rust-itertools/itertools) | 0.14.0 | MIT OR Apache-2.0 |
| [itoa](https://github.com/dtolnay/itoa) | 1.0.18 | MIT OR Apache-2.0 |
| [jni](https://github.com/jni-rs/jni-rs) | 0.22.4 | MIT OR Apache-2.0 |
| [jni-macros](https://github.com/jni-rs/jni-rs) | 0.22.4 | MIT OR Apache-2.0 |
| [jni-sys](https://github.com/jni-rs/jni-sys) | 0.4.1 | MIT OR Apache-2.0 |
| [jni-sys-macros](https://github.com/jni-rs/jni-sys) | 0.4.1 | MIT OR Apache-2.0 |
| [jobserver](https://github.com/rust-lang/jobserver-rs) | 0.1.34 | MIT OR Apache-2.0 |
| [js-sys](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys) | 0.3.95 | MIT OR Apache-2.0 |
| [k256](https://github.com/RustCrypto/elliptic-curves/tree/master/k256) | 0.13.4 | Apache-2.0 OR MIT |
| [kasuari](https://github.com/ratatui/kasuari) | 0.4.12 | MIT OR Apache-2.0 |
| [keccak](https://github.com/RustCrypto/sponges) | 0.2.0 | Apache-2.0 OR MIT |
| [kes-summed-ed25519](https://github.com/input-output-hk/kes) | 0.2.1 | Apache-2.0 |
| [lab](https://github.com/TooManyBees/lab) | 0.11.0 | MIT |
| [lazy_static](https://github.com/rust-lang-nursery/lazy-static.rs) | 1.5.0 | MIT OR Apache-2.0 |
| [leb128fmt](https://github.com/bluk/leb128fmt) | 0.1.0 | MIT OR Apache-2.0 |
| [libc](https://github.com/rust-lang/libc) | 0.2.186 | MIT OR Apache-2.0 |
| [libloading](https://github.com/nagisa/rust_libloading/) | 0.8.9 | ISC |
| [libm](https://github.com/rust-lang/compiler-builtins) | 0.2.16 | MIT |
| [libredox](https://gitlab.redox-os.org/redox-os/libredox.git) | 0.1.16 | MIT |
| [line-clipping](https://github.com/ratatui/line-clipping) | 0.3.7 | MIT OR Apache-2.0 |
| [linux-raw-sys](https://github.com/sunfishcode/linux-raw-sys) | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [litemap](https://github.com/unicode-org/icu4x) | 0.8.2 | Unicode-3.0 |
| [litrs](https://github.com/LukasKalbertodt/litrs) | 1.0.0 | MIT OR Apache-2.0 |
| [lock_api](https://github.com/Amanieu/parking_lot) | 0.4.14 | MIT OR Apache-2.0 |
| [log](https://github.com/rust-lang/log) | 0.4.29 | MIT OR Apache-2.0 |
| [lru](https://github.com/jeromefroe/lru-rs.git) | 0.18.0 | MIT |
| [lru-slab](https://github.com/Ralith/lru-slab) | 0.1.2 | MIT OR Apache-2.0 OR Zlib |
| [mac_address](https://github.com/rep-nop/mac_address) | 1.1.8 | MIT OR Apache-2.0 |
| [matchers](https://github.com/hawkw/matchers) | 0.2.0 | MIT |
| [matchit](https://github.com/ibraheemdev/matchit) | 0.8.4 | MIT AND BSD-3-Clause |
| [memchr](https://github.com/BurntSushi/memchr) | 2.8.0 | Unlicense OR MIT |
| [memmap2](https://github.com/RazrFalcon/memmap2-rs) | 0.9.11 | MIT OR Apache-2.0 |
| [memmem](http://github.com/jneem/memmem) | 0.1.1 | MIT/Apache-2.0 |
| [memoffset](https://github.com/Gilnaa/memoffset) | 0.9.1 | MIT |
| [mime](https://github.com/hyperium/mime) | 0.3.17 | MIT OR Apache-2.0 |
| [minicbor](https://github.com/twittner/minicbor) | 0.26.5 | BlueOak-1.0.0 |
| [minicbor-derive](https://github.com/twittner/minicbor) | 0.16.2 | BlueOak-1.0.0 |
| [minimal-lexical](https://github.com/Alexhuszagh/minimal-lexical) | 0.2.1 | MIT/Apache-2.0 |
| [miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| [mio](https://github.com/tokio-rs/mio) | 1.2.0 | MIT |
| [mithril-aggregator-client](https://github.com/input-output-hk/mithril/) | 0.1.10 | Apache-2.0 |
| [mithril-aggregator-discovery](https://github.com/input-output-hk/mithril/) | 0.1.4 | Apache-2.0 |
| [mithril-build-script](https://github.com/input-output-hk/mithril/) | 0.2.28 | Apache-2.0 |
| [mithril-cardano-node-internal-database](https://github.com/input-output-hk/mithril/) | 0.1.11 | Apache-2.0 |
| [mithril-client](https://github.com/input-output-hk/mithril/) | 0.14.5 | Apache-2.0 |
| [mithril-common](https://github.com/input-output-hk/mithril/) | 0.6.67 | Apache-2.0 |
| [mithril-stm](https://github.com/input-output-hk/mithril/) | 0.10.5 | Apache-2.0 |
| [moka](https://github.com/moka-rs/moka) | 0.12.15 | (MIT OR Apache-2.0) AND Apache-2.0 |
| [multimap](https://github.com/havarnov/multimap) | 0.10.1 | MIT OR Apache-2.0 |
| [ndk-context](https://github.com/rust-windowing/android-ndk-rs) | 0.1.1 | MIT OR Apache-2.0 |
| [netlink-packet-core](https://github.com/rust-netlink/netlink-packet-core) | 0.7.0 | MIT |
| [netlink-packet-sock-diag](https://github.com/rust-netlink/netlink-packet-sock-diag) | 0.4.2 | MIT |
| [netlink-packet-utils](https://github.com/rust-netlink/netlink-packet-utils) | 0.5.2 | MIT |
| [netlink-sys](https://github.com/rust-netlink/netlink-sys) | 0.8.8 | MIT |
| [netstat2](https://github.com/ohadravid/netstat2-rs) | 0.11.2 | MIT OR Apache-2.0 |
| [nix](https://github.com/nix-rust/nix) | 0.31.3 | MIT |
| [nom](https://github.com/rust-bakery/nom) | 8.0.0 | MIT |
| [ntapi](https://github.com/MSxDOS/ntapi) | 0.4.3 | Apache-2.0 OR MIT |
| [nu-ansi-term](https://github.com/nushell/nu-ansi-term) | 0.50.3 | MIT |
| [num-bigint](https://github.com/rust-num/num-bigint) | 0.4.6 | MIT OR Apache-2.0 |
| [num-conv](https://github.com/jhpratt/num-conv) | 0.2.1 | MIT OR Apache-2.0 |
| [num-derive](https://github.com/rust-num/num-derive) | 0.4.2 | MIT OR Apache-2.0 |
| [num-integer](https://github.com/rust-num/num-integer) | 0.1.46 | MIT OR Apache-2.0 |
| [num-modular](https://github.com/cmpute/num-modular) | 0.6.1 | Apache-2.0 |
| [num-order](https://github.com/cmpute/num-order) | 1.2.0 | Apache-2.0 |
| [num-rational](https://github.com/rust-num/num-rational) | 0.4.2 | MIT OR Apache-2.0 |
| [num-traits](https://github.com/rust-num/num-traits) | 0.2.19 | MIT OR Apache-2.0 |
| [num_cpus](https://github.com/seanmonstar/num_cpus) | 1.17.0 | MIT OR Apache-2.0 |
| [num_threads](https://github.com/jhpratt/num_threads) | 0.1.7 | MIT OR Apache-2.0 |
| [objc2-core-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-io-kit](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [object](https://github.com/gimli-rs/object) | 0.37.3 | Apache-2.0 OR MIT |
| [once_cell](https://github.com/matklad/once_cell) | 1.21.4 | MIT OR Apache-2.0 |
| [once_cell_polyfill](https://github.com/polyfill-rs/once_cell_polyfill) | 1.70.2 | MIT OR Apache-2.0 |
| [oorandom](https://hg.sr.ht/~icefox/oorandom) | 11.1.5 | MIT |
| [opaque-debug](https://github.com/RustCrypto/utils) | 0.3.1 | MIT OR Apache-2.0 |
| [openssl-probe](https://github.com/rustls/openssl-probe) | 0.2.1 | MIT OR Apache-2.0 |
| [ordered-float](https://github.com/reem/rust-ordered-float) | 5.3.0 | MIT |
| [page_size](https://github.com/Elzair/page_size_rs) | 0.6.0 | MIT/Apache-2.0 |
| [palette](https://github.com/Ogeon/palette) | 0.7.6 | MIT OR Apache-2.0 |
| [palette_derive](https://github.com/Ogeon/palette) | 0.7.6 | MIT OR Apache-2.0 |
| [parking_lot](https://github.com/Amanieu/parking_lot) | 0.12.5 | MIT OR Apache-2.0 |
| [parking_lot_core](https://github.com/Amanieu/parking_lot) | 0.9.12 | MIT OR Apache-2.0 |
| [paste](https://github.com/dtolnay/paste) | 1.0.15 | MIT OR Apache-2.0 |
| [percent-encoding](https://github.com/servo/rust-url/) | 2.3.2 | MIT OR Apache-2.0 |
| [pest](https://github.com/pest-parser/pest) | 2.8.6 | MIT OR Apache-2.0 |
| [pest_derive](https://github.com/pest-parser/pest) | 2.8.6 | MIT OR Apache-2.0 |
| [pest_generator](https://github.com/pest-parser/pest) | 2.8.6 | MIT OR Apache-2.0 |
| [pest_meta](https://github.com/pest-parser/pest) | 2.8.6 | MIT OR Apache-2.0 |
| [petgraph](https://github.com/petgraph/petgraph) | 0.8.3 | MIT OR Apache-2.0 |
| [phf](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT |
| [phf_codegen](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT |
| [phf_generator](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT |
| [phf_macros](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT |
| [phf_shared](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT |
| [pin-project](https://github.com/taiki-e/pin-project) | 1.1.13 | Apache-2.0 OR MIT |
| [pin-project-internal](https://github.com/taiki-e/pin-project) | 1.1.13 | Apache-2.0 OR MIT |
| [pin-project-lite](https://github.com/taiki-e/pin-project-lite) | 0.2.17 | Apache-2.0 OR MIT |
| [pkcs8](https://github.com/RustCrypto/formats/tree/master/pkcs8) | 0.10.2 | Apache-2.0 OR MIT |
| [pkg-config](https://github.com/rust-lang/pkg-config-rs) | 0.3.33 | MIT OR Apache-2.0 |
| [plain](https://github.com/randomites/plain) | 0.2.3 | MIT/Apache-2.0 |
| [plotters](https://github.com/plotters-rs/plotters) | 0.3.7 | MIT |
| [plotters-backend](https://github.com/plotters-rs/plotters) | 0.3.7 | MIT |
| [plotters-svg](https://github.com/plotters-rs/plotters.git) | 0.3.7 | MIT |
| [portable-atomic](https://github.com/taiki-e/portable-atomic) | 1.13.1 | Apache-2.0 OR MIT |
| [potential_utf](https://github.com/unicode-org/icu4x) | 0.1.5 | Unicode-3.0 |
| [powerfmt](https://github.com/jhpratt/powerfmt) | 0.2.0 | MIT OR Apache-2.0 |
| [ppv-lite86](https://github.com/cryptocorrosion/cryptocorrosion) | 0.2.21 | MIT OR Apache-2.0 |
| [prefix-trie](https://github.com/tiborschneider/prefix-trie) | 0.8.4 | MIT OR Apache-2.0 |
| [prettyplease](https://github.com/dtolnay/prettyplease) | 0.2.37 | MIT OR Apache-2.0 |
| [proc-macro2](https://github.com/dtolnay/proc-macro2) | 1.0.106 | MIT OR Apache-2.0 |
| [proptest](https://github.com/proptest-rs/proptest) | 1.11.0 | MIT OR Apache-2.0 |
| [prost](https://github.com/tokio-rs/prost) | 0.14.4 | Apache-2.0 |
| [prost-build](https://github.com/tokio-rs/prost) | 0.14.4 | Apache-2.0 |
| [prost-derive](https://github.com/tokio-rs/prost) | 0.14.4 | Apache-2.0 |
| [prost-types](https://github.com/tokio-rs/prost) | 0.14.4 | Apache-2.0 |
| [psm](https://github.com/rust-lang/stacker/) | 0.1.31 | MIT OR Apache-2.0 |
| [pulldown-cmark](https://github.com/raphlinus/pulldown-cmark) | 0.13.4 | MIT |
| [pulldown-cmark-to-cmark](https://github.com/Byron/pulldown-cmark-to-cmark) | 22.0.0 | Apache-2.0 |
| [quick-error](http://github.com/tailhook/quick-error) | 1.2.3 | MIT/Apache-2.0 |
| [quinn](https://github.com/quinn-rs/quinn) | 0.11.9 | MIT OR Apache-2.0 |
| [quinn-proto](https://github.com/quinn-rs/quinn) | 0.11.16 | MIT OR Apache-2.0 |
| [quinn-udp](https://github.com/quinn-rs/quinn) | 0.5.14 | MIT OR Apache-2.0 |
| [quote](https://github.com/dtolnay/quote) | 1.0.45 | MIT OR Apache-2.0 |
| [r-efi](https://github.com/r-efi/r-efi) | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| [rand](https://github.com/rust-random/rand) | 0.9.4 | MIT OR Apache-2.0 |
| [rand_chacha](https://github.com/rust-random/rand) | 0.9.0 | MIT OR Apache-2.0 |
| [rand_core](https://github.com/rust-random/rand) | 0.9.5 | MIT OR Apache-2.0 |
| [rand_pcg](https://github.com/rust-random/rngs) | 0.10.2 | MIT OR Apache-2.0 |
| [rand_xorshift](https://github.com/rust-random/rngs) | 0.4.0 | MIT OR Apache-2.0 |
| [rand_xoshiro](https://github.com/rust-random/rngs) | 0.7.0 | MIT OR Apache-2.0 |
| [ratatui](https://github.com/ratatui/ratatui) | 0.30.2 | MIT |
| [ratatui-core](https://github.com/ratatui/ratatui) | 0.1.2 | MIT |
| [ratatui-crossterm](https://github.com/ratatui/ratatui) | 0.1.2 | MIT |
| [ratatui-macros](https://github.com/ratatui/ratatui) | 0.7.2 | MIT |
| [ratatui-termina](https://github.com/ratatui/ratatui) | 0.1.0 | MIT |
| [ratatui-termwiz](https://github.com/ratatui/ratatui) | 0.1.2 | MIT |
| [ratatui-widgets](https://github.com/ratatui/ratatui) | 0.3.2 | MIT |
| [rayon](https://github.com/rayon-rs/rayon) | 1.12.0 | MIT OR Apache-2.0 |
| [rayon-core](https://github.com/rayon-rs/rayon) | 1.13.0 | MIT OR Apache-2.0 |
| [redox_syscall](https://gitlab.redox-os.org/redox-os/syscall) | 0.7.4 | MIT |
| [ref-cast](https://github.com/dtolnay/ref-cast) | 1.0.25 | MIT OR Apache-2.0 |
| [ref-cast-impl](https://github.com/dtolnay/ref-cast) | 1.0.25 | MIT OR Apache-2.0 |
| [regex](https://github.com/rust-lang/regex) | 1.12.3 | MIT OR Apache-2.0 |
| [regex-automata](https://github.com/rust-lang/regex) | 0.4.14 | MIT OR Apache-2.0 |
| [regex-syntax](https://github.com/rust-lang/regex) | 0.8.10 | MIT OR Apache-2.0 |
| [reqwest](https://github.com/seanmonstar/reqwest) | 0.13.3 | MIT OR Apache-2.0 |
| [resolv-conf](https://github.com/hickory-dns/resolv-conf) | 0.7.6 | MIT OR Apache-2.0 |
| [rfc6979](https://github.com/RustCrypto/signatures/tree/master/rfc6979) | 0.4.0 | Apache-2.0 OR MIT |
| [ring](https://github.com/briansmith/ring) | 0.17.14 | Apache-2.0 AND ISC |
| [ripemd](https://github.com/RustCrypto/hashes) | 0.2.0 | MIT OR Apache-2.0 |
| [rustc-hash](https://github.com/rust-lang/rustc-hash) | 2.1.2 | Apache-2.0 OR MIT |
| [rustc_version](https://github.com/djc/rustc-version-rs) | 0.4.1 | MIT OR Apache-2.0 |
| [rustix](https://github.com/bytecodealliance/rustix) | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [rustls](https://github.com/rustls/rustls) | 0.23.38 | Apache-2.0 OR ISC OR MIT |
| [rustls-native-certs](https://github.com/rustls/rustls-native-certs) | 0.8.3 | Apache-2.0 OR ISC OR MIT |
| [rustls-pki-types](https://github.com/rustls/pki-types) | 1.14.0 | MIT OR Apache-2.0 |
| [rustls-platform-verifier](https://github.com/rustls/rustls-platform-verifier) | 0.7.0 | MIT OR Apache-2.0 |
| [rustls-platform-verifier-android](https://github.com/rustls/rustls-platform-verifier) | 0.1.1 | MIT OR Apache-2.0 |
| [rustls-webpki](https://github.com/rustls/webpki) | 0.103.13 | ISC |
| [rustversion](https://github.com/dtolnay/rustversion) | 1.0.22 | MIT OR Apache-2.0 |
| [rusty-fork](https://github.com/altsysrq/rusty-fork) | 0.3.1 | MIT/Apache-2.0 |
| [ryu](https://github.com/dtolnay/ryu) | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| [safe_arch](https://github.com/Lokathor/safe_arch) | 0.7.4 | Zlib OR Apache-2.0 OR MIT |
| [same-file](https://github.com/BurntSushi/same-file) | 1.0.6 | Unlicense/MIT |
| [saphyr](https://github.com/saphyr-rs/saphyr) | 0.0.6 | MIT OR Apache-2.0 |
| [saphyr-parser](https://github.com/saphyr-rs/saphyr) | 0.0.6 | MIT OR Apache-2.0 |
| [schannel](https://github.com/steffengy/schannel-rs) | 0.1.29 | MIT |
| [schemars](https://github.com/GREsau/schemars) | 1.2.1 | MIT |
| [scopeguard](https://github.com/bluss/scopeguard) | 1.2.0 | MIT OR Apache-2.0 |
| [sec1](https://github.com/RustCrypto/formats/tree/master/sec1) | 0.7.3 | Apache-2.0 OR MIT |
| [security-framework](https://github.com/kornelski/rust-security-framework) | 3.7.0 | MIT OR Apache-2.0 |
| [security-framework-sys](https://github.com/kornelski/rust-security-framework) | 2.17.0 | MIT OR Apache-2.0 |
| [semver](https://github.com/dtolnay/semver) | 1.0.28 | MIT OR Apache-2.0 |
| [serde](https://github.com/serde-rs/serde) | 1.0.228 | MIT OR Apache-2.0 |
| [serde_bytes](https://github.com/serde-rs/bytes) | 0.11.19 | MIT OR Apache-2.0 |
| [serde_core](https://github.com/serde-rs/serde) | 1.0.228 | MIT OR Apache-2.0 |
| [serde_derive](https://github.com/serde-rs/serde) | 1.0.228 | MIT OR Apache-2.0 |
| [serde_json](https://github.com/serde-rs/json) | 1.0.150 | MIT OR Apache-2.0 |
| [serde_spanned](https://github.com/toml-rs/toml) | 1.1.1 | MIT OR Apache-2.0 |
| [serde_urlencoded](https://github.com/nox/serde_urlencoded) | 0.7.1 | MIT/Apache-2.0 |
| [serde_with](https://github.com/jonasbb/serde_with/) | 3.21.0 | MIT OR Apache-2.0 |
| [serde_with_macros](https://github.com/jonasbb/serde_with/) | 3.21.0 | MIT OR Apache-2.0 |
| [sha2](https://github.com/RustCrypto/hashes) | 0.9.9 | MIT OR Apache-2.0 |
| [sha3](https://github.com/RustCrypto/hashes) | 0.12.0 | MIT OR Apache-2.0 |
| [sharded-slab](https://github.com/hawkw/sharded-slab) | 0.1.7 | MIT |
| [shlex](https://github.com/comex/rust-shlex) | 1.3.0 | MIT OR Apache-2.0 |
| [signal-hook](https://github.com/vorner/signal-hook) | 0.3.18 | Apache-2.0/MIT |
| [signal-hook-mio](https://github.com/vorner/signal-hook) | 0.2.5 | MIT OR Apache-2.0 |
| [signal-hook-registry](https://github.com/vorner/signal-hook) | 1.4.8 | MIT OR Apache-2.0 |
| [signature](https://github.com/RustCrypto/traits/tree/master/signature) | 2.2.0 | Apache-2.0 OR MIT |
| [simd-adler32](https://github.com/mcountryman/simd-adler32) | 0.3.9 | MIT |
| [simd_cesu8](https://github.com/seancroach/simd_cesu8) | 1.1.1 | Apache-2.0 OR MIT |
| [simdutf8](https://github.com/rusticstuff/simdutf8) | 0.1.5 | MIT OR Apache-2.0 |
| [siphasher](https://github.com/jedisct1/rust-siphash) | 1.0.2 | MIT/Apache-2.0 |
| [slab](https://github.com/tokio-rs/slab) | 0.4.12 | MIT |
| [slog](https://github.com/slog-rs/slog) | 2.8.2 | MPL-2.0 OR MIT OR Apache-2.0 |
| [smallvec](https://github.com/servo/rust-smallvec) | 1.15.1 | MIT OR Apache-2.0 |
| [socket2](https://github.com/rust-lang/socket2) | 0.6.3 | MIT OR Apache-2.0 |
| [spin](https://github.com/mvdnes/spin-rs.git) | 0.9.8 | MIT |
| [spki](https://github.com/RustCrypto/formats/tree/master/spki) | 0.7.3 | Apache-2.0 OR MIT |
| [sponge-cursor](https://github.com/RustCrypto/utils) | 0.1.0 | MIT OR Apache-2.0 |
| [stable_deref_trait](https://github.com/storyyeller/stable_deref_trait) | 1.2.1 | MIT OR Apache-2.0 |
| [stacker](https://github.com/rust-lang/stacker) | 0.1.24 | MIT OR Apache-2.0 |
| [static_assertions](https://github.com/nvzqz/static-assertions-rs) | 1.1.0 | MIT OR Apache-2.0 |
| [strsim](https://github.com/rapidfuzz/strsim-rs) | 0.11.1 | MIT |
| [strum](https://github.com/Peternator7/strum) | 0.28.0 | MIT |
| [strum_macros](https://github.com/Peternator7/strum) | 0.28.0 | MIT |
| [subtle](https://github.com/dalek-cryptography/subtle) | 2.6.1 | BSD-3-Clause |
| [symlink](https://gitlab.com/chris-morgan/symlink) | 0.1.0 | MIT/Apache-2.0 |
| [syn](https://github.com/dtolnay/syn) | 2.0.117 | MIT OR Apache-2.0 |
| [sync_wrapper](https://github.com/Actyx/sync_wrapper) | 1.0.2 | Apache-2.0 |
| [synstructure](https://github.com/mystor/synstructure) | 0.13.2 | MIT |
| [sysinfo](https://github.com/GuillaumeGomez/sysinfo) | 0.39.3 | MIT |
| [system-configuration](https://github.com/mullvad/system-configuration-rs) | 0.7.0 | MIT OR Apache-2.0 |
| [system-configuration-sys](https://github.com/mullvad/system-configuration-rs) | 0.6.0 | MIT OR Apache-2.0 |
| [tagptr](https://github.com/oliver-giersch/tagptr.git) | 0.2.0 | MIT/Apache-2.0 |
| [tar](https://github.com/composefs/tar-rs) | 0.4.46 | MIT OR Apache-2.0 |
| [tempfile](https://github.com/Stebalien/tempfile) | 3.27.0 | MIT OR Apache-2.0 |
| [termina](https://github.com/helix-editor/termina) | 0.3.3 | MIT OR MPL-2.0 |
| [terminfo](https://github.com/meh/rust-terminfo) | 0.9.0 | WTFPL |
| [termios](https://github.com/dcuddeback/termios-rs) | 0.3.3 | MIT |
| [termwiz](https://github.com/wezterm/wezterm) | 0.23.3 | MIT |
| [thiserror](https://github.com/dtolnay/thiserror) | 2.0.18 | MIT OR Apache-2.0 |
| [thiserror-impl](https://github.com/dtolnay/thiserror) | 2.0.18 | MIT OR Apache-2.0 |
| [thread_local](https://github.com/Amanieu/thread_local-rs) | 1.1.9 | MIT OR Apache-2.0 |
| [threadpool](https://github.com/rust-threadpool/rust-threadpool) | 1.8.1 | MIT/Apache-2.0 |
| [time](https://github.com/time-rs/time) | 0.3.47 | MIT OR Apache-2.0 |
| [time-core](https://github.com/time-rs/time) | 0.1.8 | MIT OR Apache-2.0 |
| [time-macros](https://github.com/time-rs/time) | 0.2.27 | MIT OR Apache-2.0 |
| [tinystr](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 |
| [tinytemplate](https://github.com/bheisler/TinyTemplate) | 1.2.1 | Apache-2.0 OR MIT |
| [tinyvec](https://github.com/Lokathor/tinyvec) | 1.11.0 | Zlib OR Apache-2.0 OR MIT |
| [tinyvec_macros](https://github.com/Soveu/tinyvec_macros) | 0.1.1 | MIT OR Apache-2.0 OR Zlib |
| [tokio](https://github.com/tokio-rs/tokio) | 1.52.3 | MIT |
| [tokio-macros](https://github.com/tokio-rs/tokio) | 2.7.0 | MIT |
| [tokio-rustls](https://github.com/rustls/tokio-rustls) | 0.26.4 | MIT OR Apache-2.0 |
| [tokio-stream](https://github.com/tokio-rs/tokio) | 0.1.18 | MIT |
| [tokio-util](https://github.com/tokio-rs/tokio) | 0.7.18 | MIT |
| [toml](https://github.com/toml-rs/toml) | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |
| [toml_datetime](https://github.com/toml-rs/toml) | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| [toml_parser](https://github.com/toml-rs/toml) | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |
| [toml_writer](https://github.com/toml-rs/toml) | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| [tonic](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tonic-build](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tonic-prost](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tonic-prost-build](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tonic-reflection](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tonic-web](https://github.com/hyperium/tonic) | 0.14.6 | MIT |
| [tower](https://github.com/tower-rs/tower) | 0.5.3 | MIT |
| [tower-http](https://github.com/tower-rs/tower-http) | 0.6.8 | MIT |
| [tower-layer](https://github.com/tower-rs/tower) | 0.3.3 | MIT |
| [tower-service](https://github.com/tower-rs/tower) | 0.3.3 | MIT |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1.44 | MIT |
| [tracing-appender](https://github.com/tokio-rs/tracing) | 0.2.5 | MIT |
| [tracing-attributes](https://github.com/tokio-rs/tracing) | 0.1.31 | MIT |
| [tracing-core](https://github.com/tokio-rs/tracing) | 0.1.36 | MIT |
| [tracing-log](https://github.com/tokio-rs/tracing) | 0.2.0 | MIT |
| [tracing-serde](https://github.com/tokio-rs/tracing) | 0.2.0 | MIT |
| [tracing-subscriber](https://github.com/tokio-rs/tracing) | 0.3.23 | MIT |
| [try-lock](https://github.com/seanmonstar/try-lock) | 0.2.5 | MIT |
| [typeid](https://github.com/dtolnay/typeid) | 1.0.3 | MIT OR Apache-2.0 |
| [typenum](https://github.com/paholg/typenum) | 1.19.0 | MIT OR Apache-2.0 |
| [typetag](https://github.com/dtolnay/typetag) | 0.2.21 | MIT OR Apache-2.0 |
| [typetag-impl](https://github.com/dtolnay/typetag) | 0.2.21 | MIT OR Apache-2.0 |
| [ucd-trie](https://github.com/BurntSushi/ucd-generate) | 0.1.7 | MIT OR Apache-2.0 |
| [unarray](https://github.com/cameron1024/unarray) | 0.1.4 | MIT OR Apache-2.0 |
| [unicase](https://github.com/seanmonstar/unicase) | 2.9.0 | MIT OR Apache-2.0 |
| [unicode-ident](https://github.com/dtolnay/unicode-ident) | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| [unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | 1.13.2 | MIT OR Apache-2.0 |
| [unicode-truncate](https://github.com/Aetf/unicode-truncate) | 2.0.1 | MIT OR Apache-2.0 |
| [unicode-width](https://github.com/unicode-rs/unicode-width) | 0.2.2 | MIT OR Apache-2.0 |
| [unicode-xid](https://github.com/unicode-rs/unicode-xid) | 0.2.6 | MIT OR Apache-2.0 |
| [unit-prefix](https://codeberg.org/commons-rs/unit-prefix) | 0.5.2 | MIT |
| [untrusted](https://github.com/briansmith/untrusted) | 0.9.0 | ISC |
| [unty](https://github.com/bincode-org/unty) | 0.0.4 | MIT OR Apache-2.0 |
| [url](https://github.com/servo/rust-url) | 2.5.8 | MIT OR Apache-2.0 |
| [utf8_iter](https://github.com/hsivonen/utf8_iter) | 1.0.4 | Apache-2.0 OR MIT |
| [utf8parse](https://github.com/alacritty/vte) | 0.2.2 | Apache-2.0 OR MIT |
| [uuid](https://github.com/uuid-rs/uuid) | 1.23.1 | Apache-2.0 OR MIT |
| [valuable](https://github.com/tokio-rs/valuable) | 0.1.1 | MIT |
| [version_check](https://github.com/SergioBenitez/version_check) | 0.9.5 | MIT/Apache-2.0 |
| [virtue](https://github.com/bincode-org/virtue) | 0.0.18 | MIT |
| vrf_dalek | 0.1.0 | Unknown |
| [vtparse](https://github.com/wez/wezterm) | 0.6.2 | MIT |
| [wait-timeout](https://github.com/alexcrichton/wait-timeout) | 0.2.1 | MIT/Apache-2.0 |
| [walkdir](https://github.com/BurntSushi/walkdir) | 2.5.0 | Unlicense/MIT |
| [want](https://github.com/seanmonstar/want) | 0.3.1 | MIT |
| [wasi](https://github.com/bytecodealliance/wasi) | 0.9.0+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wasip2](https://github.com/bytecodealliance/wasi-rs) | 1.0.2+wasi-0.2.9 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wasip3](https://github.com/bytecodealliance/wasi-rs) | 0.4.0+wasi-0.3.0-rc-2026-01-06 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wasm-bindgen](https://github.com/wasm-bindgen/wasm-bindgen) | 0.2.118 | MIT OR Apache-2.0 |
| [wasm-bindgen-futures](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures) | 0.4.68 | MIT OR Apache-2.0 |
| [wasm-bindgen-macro](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro) | 0.2.118 | MIT OR Apache-2.0 |
| [wasm-bindgen-macro-support](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro-support) | 0.2.118 | MIT OR Apache-2.0 |
| [wasm-bindgen-shared](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared) | 0.2.118 | MIT OR Apache-2.0 |
| [wasm-encoder](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder) | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wasm-metadata](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-metadata) | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wasm-streams](https://github.com/MattiasBuelens/wasm-streams/) | 0.5.0 | MIT OR Apache-2.0 |
| [wasmparser](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmparser) | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [web-sys](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys) | 0.3.95 | MIT OR Apache-2.0 |
| [web-time](https://github.com/daxpedda/web-time) | 1.1.0 | MIT OR Apache-2.0 |
| [webpki-root-certs](https://github.com/rustls/webpki-roots) | 1.0.7 | CDLA-Permissive-2.0 |
| [webpki-roots](https://github.com/rustls/webpki-roots) | 1.0.6 | CDLA-Permissive-2.0 |
| [wezterm-bidi](https://github.com/wez/wezterm) | 0.2.3 | MIT AND Unicode-DFS-2016 |
| [wezterm-blob-leases](https://github.com/wezterm/wezterm) | 0.1.1 | MIT |
| [wezterm-color-types](https://github.com/wez/wezterm) | 0.3.0 | MIT |
| [wezterm-dynamic](https://github.com/wezterm/wezterm) | 0.2.1 | MIT |
| [wezterm-dynamic-derive](https://github.com/wezterm/wezterm) | 0.1.1 | MIT |
| [wezterm-input-types](https://github.com/wez/wezterm) | 0.1.0 | MIT |
| [wide](https://github.com/Lokathor/wide) | 0.7.33 | Zlib OR Apache-2.0 OR MIT |
| [widestring](https://github.com/VoidStarKat/widestring-rs) | 1.2.1 | MIT OR Apache-2.0 |
| [winapi](https://github.com/retep998/winapi-rs) | 0.3.9 | MIT/Apache-2.0 |
| [winapi-i686-pc-windows-gnu](https://github.com/retep998/winapi-rs) | 0.4.0 | MIT/Apache-2.0 |
| [winapi-util](https://github.com/BurntSushi/winapi-util) | 0.1.11 | Unlicense OR MIT |
| [winapi-x86_64-pc-windows-gnu](https://github.com/retep998/winapi-rs) | 0.4.0 | MIT/Apache-2.0 |
| [windows](https://github.com/microsoft/windows-rs) | 0.62.2 | MIT OR Apache-2.0 |
| [windows-collections](https://github.com/microsoft/windows-rs) | 0.3.2 | MIT OR Apache-2.0 |
| [windows-core](https://github.com/microsoft/windows-rs) | 0.62.2 | MIT OR Apache-2.0 |
| [windows-future](https://github.com/microsoft/windows-rs) | 0.3.2 | MIT OR Apache-2.0 |
| [windows-implement](https://github.com/microsoft/windows-rs) | 0.60.2 | MIT OR Apache-2.0 |
| [windows-interface](https://github.com/microsoft/windows-rs) | 0.59.3 | MIT OR Apache-2.0 |
| [windows-link](https://github.com/microsoft/windows-rs) | 0.2.1 | MIT OR Apache-2.0 |
| [windows-numerics](https://github.com/microsoft/windows-rs) | 0.3.1 | MIT OR Apache-2.0 |
| [windows-registry](https://github.com/microsoft/windows-rs) | 0.6.1 | MIT OR Apache-2.0 |
| [windows-result](https://github.com/microsoft/windows-rs) | 0.4.1 | MIT OR Apache-2.0 |
| [windows-strings](https://github.com/microsoft/windows-rs) | 0.5.1 | MIT OR Apache-2.0 |
| [windows-sys](https://github.com/microsoft/windows-rs) | 0.61.2 | MIT OR Apache-2.0 |
| [windows-targets](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows-threading](https://github.com/microsoft/windows-rs) | 0.2.1 | MIT OR Apache-2.0 |
| [windows_aarch64_gnullvm](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_aarch64_msvc](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_i686_gnu](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_i686_gnullvm](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_i686_msvc](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_x86_64_gnu](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_x86_64_gnullvm](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [windows_x86_64_msvc](https://github.com/microsoft/windows-rs) | 0.52.6 | MIT OR Apache-2.0 |
| [winnow](https://github.com/winnow-rs/winnow) | 1.0.1 | MIT |
| [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wit-bindgen-core](https://github.com/bytecodealliance/wit-bindgen) | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wit-bindgen-rust](https://github.com/bytecodealliance/wit-bindgen) | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wit-bindgen-rust-macro](https://github.com/bytecodealliance/wit-bindgen) | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wit-component](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-component) | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [wit-parser](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-parser) | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [writeable](https://github.com/unicode-org/icu4x) | 0.6.3 | Unicode-3.0 |
| [xattr](https://github.com/Stebalien/xattr) | 1.6.1 | MIT OR Apache-2.0 |
| [yoke](https://github.com/unicode-org/icu4x) | 0.8.2 | Unicode-3.0 |
| [yoke-derive](https://github.com/unicode-org/icu4x) | 0.8.2 | Unicode-3.0 |
| [zerocopy](https://github.com/google/zerocopy) | 0.8.48 | BSD-2-Clause OR Apache-2.0 OR MIT |
| [zerocopy-derive](https://github.com/google/zerocopy) | 0.8.48 | BSD-2-Clause OR Apache-2.0 OR MIT |
| [zerofrom](https://github.com/unicode-org/icu4x) | 0.1.7 | Unicode-3.0 |
| [zerofrom-derive](https://github.com/unicode-org/icu4x) | 0.1.7 | Unicode-3.0 |
| [zeroize](https://github.com/RustCrypto/utils) | 1.9.0 | Apache-2.0 OR MIT |
| [zeroize_derive](https://github.com/RustCrypto/utils) | 1.5.0 | Apache-2.0 OR MIT |
| [zerotrie](https://github.com/unicode-org/icu4x) | 0.2.4 | Unicode-3.0 |
| [zerovec](https://github.com/unicode-org/icu4x) | 0.11.6 | Unicode-3.0 |
| [zerovec-derive](https://github.com/unicode-org/icu4x) | 0.11.3 | Unicode-3.0 |
| [zmij](https://github.com/dtolnay/zmij) | 1.0.21 | MIT |
| [zstd](https://github.com/gyscos/zstd-rs) | 0.13.3 | MIT |
| [zstd-safe](https://github.com/gyscos/zstd-rs) | 7.2.4 | MIT OR Apache-2.0 |
| [zstd-sys](https://github.com/gyscos/zstd-rs) | 2.0.16+zstd.1.5.7 | MIT/Apache-2.0 |

## Regenerating This Page

This page is generated from `Cargo.lock` metadata. To regenerate after dependency changes:

```bash
just licenses
# equivalently:
python3 scripts/dev/generate-licenses.py > docs/src/reference/third-party-licenses.md
```

Native (non-Rust) code reaches the build through a handful of crates worth
naming explicitly:

| Component | Arrives via | Notes |
|-----------|-------------|-------|
| BLS12-381 (`blst`, C) | direct dependency of `dugite-uplc` | Plutus BLS builtins; also pulled transitively by `mithril-stm` |
| zstd (C) | `zstd` / `zstd-sys` | Mithril snapshot decompression |
| ring / aws-lc (C, asm) | rustls stack under `reqwest`, `tonic`, `mithril-client` | TLS |
| libsecp256k1 (C) | **not used** | `dugite-uplc` deliberately uses pure-Rust `k256` instead of `secp256k1-sys` |
| libsodium | **not used** | VRF is pure Rust via a forked `curve25519-dalek` |

Four dependencies are git-pinned rather than published to crates.io:
`vrf_dalek` and `curve25519-dalek-fork` (the IETF-03 VRF-compatible fork,
required for Praos leader election), plus `amaru-uplc` and `cddl`, which are
both development/conformance-only.
