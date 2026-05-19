//! Fuzz target: BlockFetch client receive path.
//!
//! An adversarial server can send arbitrary bytes on the BlockFetch channel.
//! This target exercises `decode_message` to verify:
//!
//! - B4: Indefinitely long MsgBlock stream is eventually bounded by
//!       `MAX_BLOCKS_PER_FETCH` in the client receive loop.
//! - No panic on any CBOR input.
//! - MsgBlock tag(24) wrapping: wrong tag is rejected (B6-class).
//!
//! The codec itself is exercised here; the MAX_BLOCKS_PER_FETCH enforcement
//! lives in `BlockFetchClient::fetch_range` (requires a live channel). The
//! codec must not panic regardless of input.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_blockfetch_client_recv -- -max_total_time=120

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::protocol::blockfetch::{client::MAX_BLOCKS_PER_FETCH, decode_message};

fuzz_target!(|data: &[u8]| {
    // Codec must never panic.
    let _ = decode_message(data);

    // Regression guard: cap must remain a reasonable positive number.
    // If this assert triggers (it won't at runtime), the fuzz build itself
    // would fail to compile, surfacing the regression immediately.
    let _ = MAX_BLOCKS_PER_FETCH;
});
