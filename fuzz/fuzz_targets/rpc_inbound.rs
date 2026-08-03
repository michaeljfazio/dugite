//! Fuzz target: `fuzz_rpc_inbound`
//!
//! `dugite-rpc`'s inbound mapping layer (#983) — the code that turns
//! attacker-controlled protobuf field values into `TransactionInput`, `Hash32`
//! and `Point`.
//!
//! gRPC framing itself is tonic/prost's problem and is not fuzzed here. The
//! interesting surface is the handful of dugite functions that take an
//! already-decoded protobuf value and produce a ledger type, because that is
//! where a length assumption becomes either a panic or a silently-dropped
//! request.
//!
//! # Properties
//!
//! 1. **No panic.** `copy_from_slice` panics on a length mismatch, and the
//!    pre-#983 call sites guarded it with `if len == 32`, which is why they
//!    dropped instead of erroring. The guard is now inside the mapper, so this
//!    target holds the whole surface at once.
//!
//! 2. **Fail closed, and all-or-nothing.** A list either maps completely or
//!    fails. A partially-honoured request is the real hazard: `ReadUtxos`
//!    legitimately returns fewer items when a UTxO is absent, so a dropped key
//!    is indistinguishable from a key that is not on chain; `WaitForTx` simply
//!    never reports on the dropped ref and the client waits forever. Asserted
//!    directly — the output length must equal the input length whenever the
//!    call succeeds.
//!
//! 3. **Exact width.** 32 bytes and only 32 bytes. Accepting a truncated or
//!    padded hash would silently address a *different* UTxO.
//!
//! 4. **Origin is the one exception, and only for `BlockRef`.** An empty hash
//!    means `Point::Origin`, mirroring how `point_to_block_ref` writes it on
//!    the way out. That asymmetry is deliberate and pinned here so it cannot
//!    leak into the `TxoRef` path, where an empty hash is simply malformed.
//!
//! `dugite-rpc` IS a fuzz dependency — unlike `dugite-node` it pulls in no
//! mithril-client, so no `#[path]` inclusion is needed.
//!
//! Run with:
//!   cargo +nightly fuzz run fuzz_rpc_inbound -- -max_total_time=300

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use dugite_primitives::block::Point;
use dugite_rpc::map::inbound;

/// A protobuf `bytes` field as a client could send it: any length, any content.
fn arb_bytes(u: &mut Unstructured<'_>, max: usize) -> Vec<u8> {
    let len = usize::from(u8::arbitrary(u).unwrap_or(0)) % (max + 1);
    let mut v = vec![0u8; len];
    for b in v.iter_mut() {
        *b = u8::arbitrary(u).unwrap_or(0);
    }
    v
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // ── single hashes, every width ──────────────────────────────────────
    let raw = arb_bytes(&mut u, 80);
    match inbound::hash32(&raw, "f", 0) {
        Ok(h) => {
            assert_eq!(raw.len(), 32, "a non-32-byte hash was accepted");
            assert_eq!(h.as_ref(), &raw[..], "the mapped hash is not the input");
        }
        Err(e) => {
            assert_ne!(raw.len(), 32, "a valid 32-byte hash was rejected");
            assert!(
                e.message.contains(&raw.len().to_string()),
                "the error must name the observed length"
            );
        }
    }

    // ── TxoRef lists: all-or-nothing, never shortened ───────────────────
    let count = usize::from(u8::arbitrary(&mut u).unwrap_or(0)) % 12;
    let mut owned: Vec<(Vec<u8>, u32)> = Vec::with_capacity(count);
    for _ in 0..count {
        owned.push((arb_bytes(&mut u, 40), u32::arbitrary(&mut u).unwrap_or(0)));
    }
    let refs: Vec<(&[u8], u32)> = owned.iter().map(|(h, i)| (h.as_slice(), *i)).collect();
    let all_valid = owned.iter().all(|(h, _)| h.len() == 32);
    match inbound::txo_refs(refs, "keys") {
        Ok(inputs) => {
            assert!(all_valid, "a list containing a malformed ref was accepted");
            assert_eq!(
                inputs.len(),
                owned.len(),
                "the request was silently SHORTENED — this is the #983 defect"
            );
            for (got, (hash, idx)) in inputs.iter().zip(owned.iter()) {
                assert_eq!(got.transaction_id.as_ref(), &hash[..]);
                assert_eq!(got.index, *idx);
            }
        }
        Err(e) => {
            assert!(!all_valid, "a fully valid list was rejected");
            // The error must name the FIRST offending element, so the client
            // can act on it rather than re-sending blind.
            let first_bad = owned.iter().position(|(h, _)| h.len() != 32).unwrap();
            assert_eq!(e.index, first_bad, "the error named the wrong element");
        }
    }

    // ── tx-hash lists (WaitForTx) ───────────────────────────────────────
    let count = usize::from(u8::arbitrary(&mut u).unwrap_or(0)) % 12;
    let mut owned: Vec<Vec<u8>> = Vec::with_capacity(count);
    for _ in 0..count {
        owned.push(arb_bytes(&mut u, 40));
    }
    let all_valid = owned.iter().all(|h| h.len() == 32);
    match inbound::tx_hashes(owned.iter().map(|h| h.as_slice()), "ref") {
        Ok(hs) => {
            assert!(all_valid);
            assert_eq!(hs.len(), owned.len(), "the request was silently shortened");
        }
        Err(_) => assert!(!all_valid),
    }

    // ── BlockRef lists: empty hash is Origin, everything else is 32 ─────
    let count = usize::from(u8::arbitrary(&mut u).unwrap_or(0)) % 12;
    let mut owned: Vec<(Vec<u8>, u64)> = Vec::with_capacity(count);
    for _ in 0..count {
        owned.push((arb_bytes(&mut u, 40), u64::arbitrary(&mut u).unwrap_or(0)));
    }
    let refs: Vec<(&[u8], u64)> = owned.iter().map(|(h, s)| (h.as_slice(), *s)).collect();
    let all_valid = owned.iter().all(|(h, _)| h.is_empty() || h.len() == 32);
    match inbound::block_refs(refs, "intersect") {
        Ok(points) => {
            assert!(all_valid, "a malformed BlockRef was accepted");
            assert_eq!(
                points.len(),
                owned.len(),
                "the intersect point list was silently shortened — the node \
                 would agree on a point the client never offered"
            );
            for (got, (hash, slot)) in points.iter().zip(owned.iter()) {
                match got {
                    Point::Origin => assert!(hash.is_empty(), "Origin from a non-empty hash"),
                    Point::Specific(s, h) => {
                        assert_eq!(s.0, *slot);
                        assert_eq!(h.as_ref(), &hash[..]);
                    }
                }
            }
        }
        Err(_) => assert!(!all_valid, "a fully valid BlockRef list was rejected"),
    }

    // ── an empty hash is Origin ONLY for BlockRef ───────────────────────
    assert!(
        inbound::block_ref(&[], u64::arbitrary(&mut u).unwrap_or(0), "f", 0).is_ok(),
        "an empty BlockRef hash must map to Origin"
    );
    assert!(
        inbound::txo_ref(&[], 0, "f", 0).is_err(),
        "an empty hash is not a valid TxoRef — Origin has no meaning there"
    );
});
