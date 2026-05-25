//! Fuzz target for block body-hash verification (issue #545 E5).
//!
//! `validate_block_body_hash(header, raw_block_cbor)` extracts the inner body
//! components from a wrapped block CBOR (`[era_tag, [header, c_0, ..., c_N]]`)
//! and computes
//!
//!   `bbHash = blake2b_256( blake2b_256(c_0) || ... || blake2b_256(c_N) )`
//!
//! before comparing against `header.body_hash`. See
//! `dugite_consensus::praos::compute_block_body_hash`.
//!
//! Invariants this target exercises:
//!   1. Neither `validate_block_body_hash` nor `extract_block_body_cbor` /
//!      `extract_block_body_components` may panic on arbitrary input.
//!   2. When the header is populated with the *correctly computed* hash
//!      (`compute_block_body_hash(components)`), `validate_block_body_hash`
//!      must return `Ok` — round-trip soundness.
//!   3. When the header hash is supplied by the fuzzer (and statistically
//!      will not match), the function must return `Ok`, `BodyExtractionFailed`,
//!      or `BodyHashMismatch` with self-consistent fields. No other error
//!      variant is permitted from this path.
//!
//! Byte layout:
//!   [0..32]  = header.body_hash bytes (used iff the "matching hash" mode is
//!              not selected, see control byte).
//!   [32]     = control byte:
//!                bit 0 set : populate header.body_hash with the correct hash
//!                            computed from the extracted components (round-
//!                            trip mode). Falls back to the fuzzer-supplied
//!                            bytes when extraction fails.
//!                bit 1 set : additionally run `extract_block_body_cbor` for
//!                            panic coverage.
//!   [33..]   = candidate raw block CBOR.
//!
//! Run with: cargo +nightly fuzz run fuzz_body_hash -- -max_total_time=300

#![no_main]

use dugite_consensus::praos::{compute_block_body_hash, validate_block_body_hash, ConsensusError};
use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_serialization::{extract_block_body_cbor, extract_block_body_components};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 34 {
        return;
    }

    let supplied_hash_bytes: [u8; 32] = data[0..32].try_into().unwrap();
    let control = data[32];
    let body_bytes = &data[33..];

    let round_trip = (control & 1) != 0;
    let test_extract = (control & 2) != 0;

    if test_extract {
        let _ = extract_block_body_cbor(body_bytes);
    }

    // Try the actual extraction once so we know whether `round_trip` mode
    // can construct a hash that the validator will accept.
    let extracted = extract_block_body_components(body_bytes);
    let supplied_hash = Hash32::from_bytes(supplied_hash_bytes);
    let claimed_hash = match (round_trip, &extracted) {
        (true, Some(components)) => compute_block_body_hash(components),
        // Round-trip requested but no extractable components: fall through
        // to the fuzzer-supplied hash; the validator will return
        // BodyExtractionFailed, which is a legal outcome.
        _ => supplied_hash,
    };

    let header = BlockHeader {
        header_hash: Hash32::ZERO,
        prev_hash: Hash32::ZERO,
        issuer_vkey: vec![0u8; 32],
        vrf_vkey: vec![0u8; 32],
        vrf_result: VrfOutput {
            output: vec![0u8; 64],
            proof: vec![0u8; 80],
        },
        nonce_vrf_output: vec![],
        nonce_vrf_proof: vec![],
        block_number: BlockNo(1),
        slot: SlotNo(100),
        epoch_nonce: Hash32::ZERO,
        body_size: body_bytes.len() as u64,
        body_hash: claimed_hash,
        operational_cert: OperationalCert {
            hot_vkey: vec![0u8; 32],
            sequence_number: 0,
            kes_period: 0,
            sigma: vec![0u8; 64],
        },
        protocol_version: ProtocolVersion { major: 9, minor: 0 },
        kes_signature: vec![0u8; 448],
        prev_nonce: None,
    };

    let result = validate_block_body_hash(&header, body_bytes);

    let extracted_ok = extracted.is_some();
    match (&result, round_trip, extracted_ok) {
        // Sound outcomes.
        (Ok(()), _, _) => {
            // If we entered round-trip mode with a successful extraction, Ok
            // is mandatory; if we're here that holds. If round_trip=false and
            // Ok fires, the fuzzer guessed the right hash — fine.
        }
        (Err(ConsensusError::BodyExtractionFailed), _, false) => {
            // Extraction reported failure; the validator must report the same.
        }
        (Err(ConsensusError::BodyExtractionFailed), _, true) => panic!(
            "validate_block_body_hash returned BodyExtractionFailed but \
             extract_block_body_components succeeded on the same input — \
             extractors must agree"
        ),
        (
            Err(ConsensusError::BodyHashMismatch {
                header_hash,
                computed_hash,
            }),
            false,
            true,
        ) => {
            assert_eq!(
                *header_hash, claimed_hash,
                "BodyHashMismatch.header_hash must equal header.body_hash"
            );
            assert_ne!(
                *header_hash, *computed_hash,
                "BodyHashMismatch.computed_hash must differ from header_hash"
            );
        }
        (Err(ConsensusError::BodyHashMismatch { .. }), true, true) => panic!(
            "round_trip mode + successful extraction must yield Ok, got \
             BodyHashMismatch: {result:?}"
        ),
        (Err(ConsensusError::BodyHashMismatch { .. }), _, false) => panic!(
            "BodyHashMismatch fired but extraction returned None — \
             validator should have returned BodyExtractionFailed instead"
        ),
        (Err(other), _, _) => panic!(
            "validate_block_body_hash may only return Ok, BodyExtractionFailed, \
             or BodyHashMismatch; got: {other:?}"
        ),
    }
});
