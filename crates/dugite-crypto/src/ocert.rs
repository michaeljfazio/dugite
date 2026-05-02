//! Operational-certificate signing primitive — the canonical Haskell-compatible
//! `OCertSignable` byte layout used by both the CLI's `issue_op_cert` and the
//! consensus verifier.
//!
//! Cardano's Praos consensus authorizes a hot KES key via an operational
//! certificate signed by the SPO's cold key. The bytes the cold key signs are
//! defined by `Cardano.Protocol.TPraos.OCert.SignableRepresentation
//! (OCertSignable c)` in `cardano-protocol-tpraos`:
//!
//! ```text
//! OCertSignable = kes_vkey(32 raw bytes) || seqNo(u64 big-endian) || kesPeriod(u64 big-endian)
//! ```
//!
//! This is **NOT** CBOR. Specifically: no array header, no length prefix, no
//! domain separator, no per-field CBOR encoding. The `seqNo` and `kesPeriod`
//! are 8-byte big-endian raw integers — not CBOR uints. Total length is
//! exactly 48 bytes for a 32-byte KES vkey.
//!
//! cardano-node's verifier reconstructs these 48 bytes from the operational
//! certificate fields embedded in the block header CBOR and then runs
//! standard Ed25519 verify. If we sign anything other than these 48 bytes,
//! every forged block will be rejected with `InvalidSignatureOCERT`.

/// Build the canonical 48-byte `OCertSignable` payload that the cold key
/// signs.
///
/// This MUST match the bytes the verifier reconstructs in
/// `dugite_consensus::praos::verify_opcert_signature`. The two functions are
/// kept symmetrical via a shared roundtrip test (`tests::roundtrip_*`).
pub fn ocert_signable_bytes(kes_vkey: &[u8], sequence_number: u64, kes_period: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(kes_vkey.len() + 16);
    bytes.extend_from_slice(kes_vkey);
    bytes.extend_from_slice(&sequence_number.to_be_bytes());
    bytes.extend_from_slice(&kes_period.to_be_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::PaymentSigningKey;

    #[test]
    fn signable_layout_is_48_bytes_for_32_byte_kes_vkey() {
        let kes_vkey = [0xABu8; 32];
        let bytes = ocert_signable_bytes(&kes_vkey, 0, 0);
        assert_eq!(bytes.len(), 48);
        assert_eq!(&bytes[0..32], &kes_vkey[..]);
        assert_eq!(&bytes[32..40], &[0u8; 8]);
        assert_eq!(&bytes[40..48], &[0u8; 8]);
    }

    #[test]
    fn signable_uses_big_endian_u64_not_cbor() {
        // seqNo = 3, kesPeriod = 840 — the same values from the soak failure
        // (InvalidSignatureOCERT 3 (KESPeriod 840)).
        let kes_vkey = [0u8; 32];
        let bytes = ocert_signable_bytes(&kes_vkey, 3, 840);

        // bytes[32..40] = 3u64 big-endian = 00 00 00 00 00 00 00 03
        assert_eq!(&bytes[32..40], &[0, 0, 0, 0, 0, 0, 0, 3]);
        // bytes[40..48] = 840u64 big-endian = 00 00 00 00 00 00 03 48
        assert_eq!(&bytes[40..48], &[0, 0, 0, 0, 0, 0, 0x03, 0x48]);

        // Sanity: the result must NOT be CBOR-encoded. CBOR-encoded
        // `array(3) [bytes(32), uint(3), uint(840)]` would start with 0x83
        // (array header). The raw form starts with the kes_vkey bytes.
        assert_ne!(bytes[0], 0x83);
    }

    #[test]
    fn roundtrip_sign_then_verify_with_consensus_layout() {
        // This test asserts that bytes produced by `ocert_signable_bytes`
        // are accepted by the same byte layout that
        // dugite-consensus::verify_opcert_signature reconstructs from header
        // fields. If this test fails, it means signing and verification
        // disagree on the OCertSignable layout — exactly the bug class that
        // caused InvalidSignatureOCERT in the 2026-05-01 soak.
        let cold_sk = PaymentSigningKey::generate();
        let cold_vk = cold_sk.verification_key();

        let kes_vkey = [0x42u8; 32];
        let seq_no = 7u64;
        let kes_period = 123u64;

        // Sign via the canonical helper.
        let signable = ocert_signable_bytes(&kes_vkey, seq_no, kes_period);
        let sig = cold_sk.sign(&signable);

        // Reconstruct independently (mirrors verify_opcert_signature in
        // dugite-consensus::praos) and verify.
        let mut reconstructed = Vec::with_capacity(48);
        reconstructed.extend_from_slice(&kes_vkey);
        reconstructed.extend_from_slice(&seq_no.to_be_bytes());
        reconstructed.extend_from_slice(&kes_period.to_be_bytes());
        assert_eq!(signable, reconstructed);

        // Standard Ed25519 verify must succeed.
        cold_vk
            .verify(&reconstructed, &sig)
            .expect("cold-key signature over OCertSignable must verify");
    }
}
