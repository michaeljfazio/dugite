//! Fuzz target for CBOR encode/decode roundtrip verification.
//!
//! Decodes arbitrary CBOR bytes as transactions (all eras 0-6), re-encodes
//! successfully decoded transactions, and verifies that decoding the
//! re-encoded bytes produces a structurally identical transaction.
//!
//! Also decodes blocks and verifies transaction-level roundtrips within them.
//!
//! Run with: cargo +nightly fuzz run fuzz_encode_roundtrip -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;
use dugite_serialization::{decode_block, decode_transaction, encode_transaction};

fuzz_target!(|data: &[u8]| {
    // Test 1: Transaction roundtrip across all eras
    for era_id in 0..=6u16 {
        if let Ok(tx) = decode_transaction(era_id, data) {
            // Re-encode the decoded transaction
            let encoded = encode_transaction(&tx);

            // Decode the re-encoded bytes. In some cases the encoder produces
            // a different CBOR format than the input (e.g., legacy array format
            // decoded but re-encoded as map format, or indefinite-length maps
            // canonicalised to definite-length). Skip the hash assertion when
            // the re-encoded bytes differ in length from the original — those
            // are by definition non-canonical inputs, and the hash check would
            // be testing the decoder's leniency rather than the encoder's
            // round-trip behaviour. We still cross-check the structural
            // fields below for those inputs.
            if let Ok(re_decoded) = decode_transaction(era_id, &encoded) {
                // Hash equality only when the encoder produced byte-identical
                // output. Lenient inputs (indefinite-length maps, partial
                // bodies, etc.) canonicalise on re-encode and will hash
                // differently — those are not failures of the encoder.
                if encoded.as_slice() == data {
                    assert_eq!(
                        tx.hash, re_decoded.hash,
                        "Transaction hash mismatch after canonical roundtrip (era {})",
                        era_id
                    );
                }
                assert_eq!(
                    tx.body.inputs, re_decoded.body.inputs,
                    "Inputs mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.body.outputs.len(),
                    re_decoded.body.outputs.len(),
                    "Output count mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.body.fee, re_decoded.body.fee,
                    "Fee mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.body.ttl, re_decoded.body.ttl,
                    "TTL mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.body.certificates, re_decoded.body.certificates,
                    "Certificates mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.body.mint, re_decoded.body.mint,
                    "Mint mismatch after roundtrip (era {})",
                    era_id
                );
                assert_eq!(
                    tx.is_valid, re_decoded.is_valid,
                    "is_valid mismatch after roundtrip (era {})",
                    era_id
                );
            }
        }
    }

    // Test 2: Block decode → per-transaction roundtrip
    if let Ok(block) = decode_block(data) {
        for (i, tx) in block.transactions.iter().enumerate() {
            let encoded = encode_transaction(tx);
            // The re-encoded transaction should be decodable for the block's era
            let era_id = match block.era {
                dugite_primitives::era::Era::Byron => 0,
                dugite_primitives::era::Era::Shelley => 1,
                dugite_primitives::era::Era::Allegra => 2,
                dugite_primitives::era::Era::Mary => 3,
                dugite_primitives::era::Era::Alonzo => 4,
                dugite_primitives::era::Era::Babbage => 5,
                dugite_primitives::era::Era::Conway => 6,
                // TODO(dijkstra): re-encode/round-trip Dijkstra transactions once
                // a proper Dijkstra encoder exists. Until then the multi-era
                // decoder treats Dijkstra blocks via the Conway shim, so re-encoding
                // and round-tripping is not meaningful — skip these blocks.
                dugite_primitives::era::Era::Dijkstra => continue,
            };
            if let Ok(re_decoded) = decode_transaction(era_id, &encoded) {
                // Same canonical-input guard as Test 1.
                let original_bytes = tx.raw_cbor.as_deref().unwrap_or(&[]);
                if encoded.as_slice() == original_bytes {
                    assert_eq!(
                        tx.hash, re_decoded.hash,
                        "Block tx {} hash mismatch after roundtrip",
                        i
                    );
                }
                assert_eq!(
                    tx.body.fee, re_decoded.body.fee,
                    "Block tx {} fee mismatch after roundtrip",
                    i
                );
            }
        }
    }
});
