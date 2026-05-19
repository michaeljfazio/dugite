//! Fuzz target for the consensus header-field-size pre-flight check (issue #539).
//!
//! Background: Haskell cardano-base's fixed-size DSIGN/VRF/KES newtypes reject
//! wrong-length fields at CBOR decode time via `failSizeCheck`. Pallas decodes
//! header fields as variable-length `Bytes`, so dugite enforces the invariant
//! via `OuroborosPraos::check_header_field_sizes`. This fuzz target stresses
//! the structural check with arbitrarily-sized fields to ensure:
//!   1. The check never panics on attacker-chosen field lengths.
//!   2. In strict mode, every non-zero non-canonical length is rejected
//!      with the exact `MalformedHeaderField` predicate.
//!   3. In non-strict mode, the check always returns `Ok(())` (preserving
//!      pre-#539 silent-skip semantics for sync replay).
//!
//! Byte layout (LE size prefixes — wrap to 0..=512 to stay tractable):
//!   [0..2]   = issuer_vkey length (mod 513)
//!   [2..4]   = vrf_vkey length (mod 513)
//!   [4..6]   = vrf_result.output length (mod 513)
//!   [6..8]   = opcert.sigma length (mod 513)
//!   [8]      = strict flag (lsb)
//!   [9..]    = ignored
//!
//! Run with: cargo +nightly fuzz run fuzz_header_field_sizes -- -max_total_time=300

#![no_main]

use dugite_consensus::praos::{ConsensusError, OuroborosPraos};
use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};
use libfuzzer_sys::fuzz_target;

const EXPECTED_ISSUER_VKEY: usize = 32;
const EXPECTED_VRF_VKEY: usize = 32;
const EXPECTED_VRF_OUTPUT: usize = 64;
const EXPECTED_SIGMA: usize = 64;

fn read_u16_le(slice: &[u8]) -> u16 {
    if slice.len() < 2 {
        return 0;
    }
    u16::from_le_bytes([slice[0], slice[1]])
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 9 {
        return;
    }

    // Cap lengths at 512 so we cover the realistic adversarial range
    // (empty, just-under, exact, just-over, well-over) without OOM.
    let issuer_vkey_len = (read_u16_le(&data[0..2]) % 513) as usize;
    let vrf_vkey_len = (read_u16_le(&data[2..4]) % 513) as usize;
    let vrf_output_len = (read_u16_le(&data[4..6]) % 513) as usize;
    let sigma_len = (read_u16_le(&data[6..8]) % 513) as usize;
    let strict = (data[8] & 1) != 0;

    let header = BlockHeader {
        header_hash: Hash32::ZERO,
        prev_hash: Hash32::ZERO,
        issuer_vkey: vec![0u8; issuer_vkey_len],
        vrf_vkey: vec![0u8; vrf_vkey_len],
        vrf_result: VrfOutput {
            output: vec![0u8; vrf_output_len],
            proof: vec![0u8; 80],
        },
        nonce_vrf_output: vec![],
        nonce_vrf_proof: vec![],
        block_number: BlockNo(1),
        slot: SlotNo(100),
        epoch_nonce: Hash32::ZERO,
        body_size: 0,
        body_hash: Hash32::ZERO,
        operational_cert: OperationalCert {
            hot_vkey: vec![0u8; 32],
            sequence_number: 0,
            kes_period: 0,
            sigma: vec![0u8; sigma_len],
        },
        protocol_version: ProtocolVersion { major: 9, minor: 0 },
        kes_signature: vec![],
    };

    // The structural pre-flight must never panic.
    let result = OuroborosPraos::check_header_field_sizes(&header, strict);

    if !strict {
        // Non-strict mode: ALWAYS Ok (preserves pre-#539 silent-skip).
        assert!(
            result.is_ok(),
            "non-strict mode must never reject: result={result:?}"
        );
        return;
    }

    // Strict mode invariants. We expect rejection exactly when at least one
    // field has a non-zero, non-canonical length. Empty (len=0) is delegated
    // to upstream `EmptyIssuerVkey`/`EmptyVrfKey`/`InvalidOperationalCert`
    // predicates so the pre-flight intentionally returns Ok for that case.
    let any_malformed = (issuer_vkey_len != 0 && issuer_vkey_len != EXPECTED_ISSUER_VKEY)
        || (vrf_vkey_len != 0 && vrf_vkey_len != EXPECTED_VRF_VKEY)
        || (vrf_output_len != 0 && vrf_output_len != EXPECTED_VRF_OUTPUT)
        || (sigma_len != 0 && sigma_len != EXPECTED_SIGMA);

    if any_malformed {
        match result {
            Err(ConsensusError::MalformedHeaderField {
                field,
                expected_len,
                actual_len,
            }) => {
                // The reported (field, expected, actual) triple must be
                // self-consistent: the named field's actual length must
                // disagree with the canonical expected length.
                match field {
                    "issuer_vkey" => {
                        assert_eq!(expected_len, EXPECTED_ISSUER_VKEY);
                        assert_eq!(actual_len, issuer_vkey_len);
                        assert_ne!(actual_len, 0);
                        assert_ne!(actual_len, expected_len);
                    }
                    "vrf_vkey" => {
                        assert_eq!(expected_len, EXPECTED_VRF_VKEY);
                        assert_eq!(actual_len, vrf_vkey_len);
                        assert_ne!(actual_len, 0);
                        assert_ne!(actual_len, expected_len);
                    }
                    "vrf_result.output" => {
                        assert_eq!(expected_len, EXPECTED_VRF_OUTPUT);
                        assert_eq!(actual_len, vrf_output_len);
                        assert_ne!(actual_len, 0);
                        assert_ne!(actual_len, expected_len);
                    }
                    "opcert.sigma" => {
                        assert_eq!(expected_len, EXPECTED_SIGMA);
                        assert_eq!(actual_len, sigma_len);
                        assert_ne!(actual_len, 0);
                        assert_ne!(actual_len, expected_len);
                    }
                    other => panic!("unexpected field name: {other}"),
                }
            }
            Err(other) => panic!("expected MalformedHeaderField in strict mode, got {other:?}"),
            Ok(()) => panic!(
                "strict mode must reject malformed lengths: issuer_vkey={issuer_vkey_len}, \
                 vrf_vkey={vrf_vkey_len}, vrf_output={vrf_output_len}, sigma={sigma_len}"
            ),
        }
    } else {
        // All non-zero lengths are canonical (or empty, delegated upstream).
        assert!(
            result.is_ok(),
            "strict mode with canonical/empty sizes must accept: result={result:?}"
        );
    }
});
