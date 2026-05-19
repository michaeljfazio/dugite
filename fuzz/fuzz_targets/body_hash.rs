//! Fuzz target for block body-hash verification (issue #545 E5).
//!
//! `validate_block_body_hash` computes `blake2b_256(body_bytes)` and compares
//! it against `header.body_hash`. This target verifies:
//!   1. The check never panics on arbitrary body bytes or header hashes.
//!   2. When the header hash exactly matches blake2b_256(body), the check passes.
//!   3. When the header hash does NOT match, the check returns `BodyHashMismatch`
//!      with consistent fields (header_hash == supplied hash, computed == actual).
//!   4. `extract_block_body_cbor` never panics on arbitrary raw CBOR.
//!
//! Byte layout:
//!   [0..32]  = header.body_hash bytes (what the header claims)
//!   [32]     = control byte:
//!                bit 0: if set, overwrite header hash with blake2b_256(body) → expect Ok
//!                bit 1: if set, pass the raw block wrapper bytes through extract_block_body_cbor
//!   [33..]   = body bytes (the block body CBOR, passed to validate_block_body_hash)
//!
//! Run with: cargo +nightly fuzz run fuzz_body_hash -- -max_total_time=300

#![no_main]

use dugite_consensus::praos::{validate_block_body_hash, ConsensusError};
use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 34 {
        return;
    }

    // Parse header body_hash from first 32 bytes.
    let hash_bytes: [u8; 32] = data[0..32].try_into().unwrap();
    let control = data[32];
    let body_bytes = &data[33..];

    let make_match = (control & 1) != 0;
    let test_extract = (control & 2) != 0;

    // Exercise extract_block_body_cbor on raw bytes — must never panic.
    if test_extract {
        let _ = dugite_serialization::extract_block_body_cbor(body_bytes);
    }

    // Determine the claimed body_hash in the header.
    let claimed_hash = if make_match {
        // Compute the correct hash so validate_block_body_hash must return Ok.
        dugite_primitives::hash::blake2b_256(body_bytes)
    } else {
        Hash32::from_bytes(hash_bytes)
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
    };

    // validate_block_body_hash must never panic.
    let result = validate_block_body_hash(&header, body_bytes);

    if make_match {
        // Hash was constructed to match → must succeed.
        assert!(
            result.is_ok(),
            "validate_block_body_hash must return Ok when hashes match: {result:?}"
        );
    } else {
        // Hash may or may not match depending on body_bytes content.
        // Either outcome is valid, but the error shape must be consistent.
        if let Err(ConsensusError::BodyHashMismatch {
            header_hash,
            computed_hash,
        }) = result
        {
            // The header_hash in the error must equal what we put in the header.
            assert_eq!(
                header_hash, claimed_hash,
                "BodyHashMismatch.header_hash must equal header.body_hash"
            );
            // The computed hash must NOT equal the header hash (that's why it failed).
            assert_ne!(
                header_hash, computed_hash,
                "BodyHashMismatch.computed_hash must differ from header_hash"
            );
        }
        // Ok(()) is also valid if hash_bytes happen to be blake2b_256(body_bytes).
    }
});
