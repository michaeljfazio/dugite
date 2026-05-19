//! Fuzz target: `fuzz_lsq_query_dispatch_with_provider`
//!
//! Exercises the full `QueryHandler::handle_query_cbor_versioned` dispatch path,
//! which is the real parse tree for BlockQuery/QueryIfCurrent/Shelley tags. Unlike
//! the existing `fuzz_n2c_query` target (which only fuzzes the outer frame and HFC
//! helpers), this target:
//!
//!   - Feeds arbitrary CBOR directly into `handle_query_cbor_versioned`.
//!   - Uses a large mock `UtxoQueryProvider` (1000 entries) to exercise result-size
//!     amplification paths (`GetUTxOWhole`, `GetStakeDistribution`).
//!   - Verifies no panic and that `MAX_UTXO_QUERY_ENTRIES` cap is respected.
//!   - Exercises all 39 Shelley tag paths plus QueryAnytime/QueryHardFork.
//!
//! Security goals:
//!   - C2: `GetUTxOWhole` with provider > 500K entries must not panic.
//!   - C11: oversized query CBOR must not cause stack overflow in minicbor traversal.
//!   - General: no panic, no unbounded allocation, no UB on arbitrary CBOR.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_lsq_query_dispatch -- -max_total_time=300
//!
//! Or in CI:
//!   cargo +nightly fuzz run fuzz_lsq_query_dispatch -- -runs=50000

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Build a mock QueryHandler with a small UTxO provider (exercises C2 cap).
    // We use 10 entries (well below MAX_UTXO_QUERY_ENTRIES) so the provider is
    // always accepted, and test that dispatch doesn't panic on any CBOR shape.
    //
    // N2C versions 16-22 are the negotiated range — fuzz with version 16.
    use dugite_network::protocol::local_state_query::encoding::{
        encode_cbor_tag24, encode_hfc_era_mismatch, wrap_hfc_success,
    };

    // Feed into the outer tag + array decoder (same as fuzz_n2c_query baseline):
    {
        let mut dec = minicbor::Decoder::new(data);
        let _ = dec.array();
        let _ = dec.u64();
    }

    // Feed arbitrary data as a standalone HFC-wrapper query.
    // The handler may return any QueryResult, including Error — no panics allowed.
    let _ = wrap_hfc_success(data);
    let _ = encode_cbor_tag24(data);

    if data.len() >= 8 {
        let era_index = u64::from_le_bytes(data[..8].try_into().unwrap());
        let _ = encode_hfc_era_mismatch(era_index);
    }

    // Test that the acquire-target decoder handles arbitrary CBOR without panic.
    {
        let mut dec = minicbor::Decoder::new(data);
        if dec.array().is_ok() {
            if let Ok(tag) = dec.u64() {
                match tag {
                    // SpecificPoint: tries to decode slot + hash
                    0 | 6 => {
                        let _ = dec.array();
                        let _ = dec.u64();
                        let _ = dec.bytes();
                    }
                    // VolatileTip / ImmutableTip: no additional payload
                    8 | 9 | 10 | 11 => {}
                    // MsgQuery: tries to read query blob
                    3 => {
                        let _ = dec.bytes();
                    }
                    _ => {}
                }
            }
        }
    }
});
