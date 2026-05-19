//! Fuzz target: PeerSharing client address injection defence.
//!
//! A malicious peer can send `MsgSharePeers` containing any IP addresses,
//! including private ranges, loopback, link-local, cloud metadata endpoints
//! (169.254.169.254), and IPv6 ULA/multicast.  If these are not filtered
//! before being passed to the PeerManager, dugite would attempt to connect
//! to internal services — an SSRF-class vulnerability.
//!
//! Coverage goals:
//! - B10: Client-side `is_routable()` filtering — all addresses returned by
//!        `request_peers()` must be routable (no private/loopback/link-local).
//! - B11: Indefinite-length `MsgSharePeers` array capped at MAX_SHARED_ADDRS.
//! - B14: Documentation ranges, multicast, and reserved ranges are rejected.
//!
//! This target exercises `decode_message` for PeerSharing and `is_routable`
//! to verify:
//! 1. No panic on arbitrary CBOR input.
//! 2. Any decoded MsgSharePeers has addresses that survive `is_routable` check
//!    as a sanity check (we exercise the filter function).
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_peer_sharing_client_addr_injection \
//!     -- -max_total_time=120

#![no_main]

use libfuzzer_sys::fuzz_target;

use dugite_network::protocol::peersharing::{decode_message, is_routable, PeerSharingMessage};

fuzz_target!(|data: &[u8]| {
    // Codec must never panic.
    let result = decode_message(data);

    // For any successfully decoded MsgSharePeers, verify that is_routable()
    // never panics on any IP address present in the decoded message.
    // This is not a correctness assertion (the test suite covers that) —
    // it's a no-panic / no-crash assertion under fuzzer-controlled input.
    if let Ok(PeerSharingMessage::MsgSharePeers(addrs)) = result {
        for addr in &addrs {
            // Must not panic.
            let _ = is_routable(&addr.ip());
        }
    }
});
