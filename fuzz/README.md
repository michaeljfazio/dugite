# Dugite Fuzz Targets

Fuzz testing for Dugite's untrusted-input parsers using
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer backend).

## Prerequisites

```bash
# Install cargo-fuzz (requires nightly Rust)
cargo install cargo-fuzz
rustup install nightly
```

## Available Targets

The current target list is the authoritative source — run

```bash
cargo +nightly fuzz list
```

to enumerate all fuzz binaries. Highlights:

| Target | Description |
|--------|-------------|
| `fuzz_decode_block` | Multi-era CBOR block deserialization |
| `fuzz_byron_block_decode` | Byron-era block decoder (issue #613) — calls `decode_byron_main_block` / `decode_byron_ebb_block` directly |
| `fuzz_decode_transaction` | CBOR transaction deserialization (all eras) |
| `fuzz_dugite_uplc_program_decode` | In-house UPLC `Program::{from_cbor, from_flat}` + flat-encoding round-trip |
| `fuzz_dugite_uplc_data_decode` | In-house `PlutusData::from_cbor` + CBOR round-trip identity |
| `fuzz_plutus_data_decode` | Upstream Aiken `uplc::plutus_data` (sanity-check) |
| `fuzz_plutus_script_decode` | Upstream Aiken `Program::from_cbor` / `from_flat` — **CI-excluded** (upstream panics in `pallas_codec` and `uplc::tx`) |
| `fuzz_body_hash` | `validate_block_body_hash` round-trip + invariant checks |
| `fuzz_nonce_update` | Evolving nonce blake2b computation |

> ⚠️ The in-house `fuzz_dugite_uplc_*` targets are the **production** path
> for phase-2 validation. The upstream `fuzz_plutus_*` targets exercise
> the Aiken `uplc` crate only and are useful for parity checking, not
> for catching dugite DoS surface.

## Running a Target

```bash
# From the repository root:
cd fuzz

# Run a specific target for 5 minutes
cargo +nightly fuzz run fuzz_decode_block -- -max_total_time=300

# Run with a corpus directory (seeds are auto-saved)
cargo +nightly fuzz run fuzz_decode_block corpus/decode_block

# Run all targets sequentially (10 min each)
for target in fuzz_decode_block fuzz_decode_transaction fuzz_mux_segment fuzz_nonce_update; do
  echo "=== Fuzzing $target ==="
  cargo +nightly fuzz run $target -- -max_total_time=600
done
```

## Corpus Management

Seed corpora live in `corpus/<target>/`. The fuzzer automatically saves
interesting inputs (new coverage) to this directory. Commit interesting
corpus files to the repository so future runs start from better seeds.

For `fuzz_decode_block`, the corpus is seeded with real block CBOR from
Cardano eras (Alonzo, Babbage, Conway).

## Coverage-Guided Fuzzing Tips

- **Duration**: 10-60 minutes per target is a reasonable starting point.
  Longer runs explore deeper paths.
- **Parallelism**: Use `-fork=N` to run N fuzzer processes in parallel:
  ```bash
  cargo +nightly fuzz run fuzz_decode_block -- -fork=4 -max_total_time=600
  ```
- **Memory limit**: Default is 2 GB. Increase with `-rss_limit_mb=4096`
  if the target legitimately needs more.
- **Artifact analysis**: When a crash is found, the input is saved to
  `fuzz/artifacts/<target>/`. Reproduce with:
  ```bash
  cargo +nightly fuzz run fuzz_decode_block fuzz/artifacts/fuzz_decode_block/<crash_file>
  ```

## Adding New Targets

1. Create `fuzz/fuzz_targets/<name>.rs` with a `fuzz_target!` macro.
2. Add the `[[bin]]` entry to `fuzz/Cargo.toml`.
3. Add any new crate dependencies to `[dependencies]` in `fuzz/Cargo.toml`.
4. Optionally seed `fuzz/corpus/<name>/` with representative inputs.
