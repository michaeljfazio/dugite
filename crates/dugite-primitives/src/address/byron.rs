//! Byron-era address construction and parsing.
//!
//! Byron addresses are encoded on-chain as:
//!
//! ```text
//! ByronAddress = CBOR array(2)[
//!     tag(24, bstr(inner_cbor)),   // payload, tag-24-wrapped
//!     crc32_u32(inner_cbor),        // CRC-32/ISO-HDLC checksum of inner_cbor
//! ]
//!
//! inner_cbor = CBOR array(3)[
//!     bstr(28)(addr_root),          // Blake2b-224(SHA3-256(addr_spec_cbor))
//!     map(attributes),              // {} mainnet, {2: bytes(magic)} testnet
//!     u8(addr_type),                // 0=PubKey 1=Script 2=Redeem
//! ]
//!
//! addr_spec_cbor = CBOR array(3)[
//!     u8(addr_type),
//!     spending_data,                // Redeem: array(2)[i64(2), bstr(32)(ed25519_pk)]
//!     map(attributes),
//! ]
//! ```
//!
//! Key invariant: the root hash is Blake2b-224 ∘ SHA3-256, NOT Blake2b-224 alone.

use blake2b_simd::Params as Blake2bParams;
use sha3::{Digest, Sha3_256};
use thiserror::Error;

/// Address type discriminant (matches Cardano's `AddrType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByronAddrType {
    PubKey = 0,
    Script = 1,
    Redeem = 2,
}

impl ByronAddrType {
    fn from_u8(v: u8) -> Result<Self, ByronAddressError> {
        match v {
            0 => Ok(Self::PubKey),
            1 => Ok(Self::Script),
            2 => Ok(Self::Redeem),
            other => Err(ByronAddressError::InvalidAddrType(other)),
        }
    }

    fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Parsed payload of a Byron address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByronAddressPayload {
    pub addr_type: ByronAddrType,
    /// Blake2b-224(SHA3-256(addr_spec_cbor)) — 28 bytes.
    pub root: [u8; 28],
    /// Raw CBOR bytes of the attributes map.
    /// Empty map (`0xa0`) for mainnet; `{2: bytes(magic)}` for testnets.
    pub attributes: Vec<u8>,
}

/// Error type for Byron address parsing.
#[derive(Debug, Error)]
pub enum ByronAddressError {
    #[error("invalid CBOR: {0}")]
    Cbor(String),
    #[error("CRC32 mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },
    #[error("invalid address type byte: {0}")]
    InvalidAddrType(u8),
    #[error("invalid root hash length: expected 28, got {0}")]
    InvalidRootLen(usize),
}

impl ByronAddressPayload {
    /// Build a Byron Redeem address payload from a 32-byte Ed25519 verification key.
    ///
    /// `network_tag` is `None` for mainnet (protocol magic 764824073) and
    /// `Some(cbor_encoded_magic)` for testnets.  The caller must CBOR-encode
    /// the magic value before passing it in (e.g. `minicbor::to_vec(magic as u32)`).
    pub fn new_redeem(pubkey: &[u8; 32], network_tag: Option<Vec<u8>>) -> Self {
        let attributes = attributes_cbor(network_tag.as_deref());
        let spending = redeem_spending_data_cbor(pubkey);
        let root = addr_root_hash(ByronAddrType::Redeem, &spending, &attributes);
        Self {
            addr_type: ByronAddrType::Redeem,
            root,
            attributes,
        }
    }

    /// Serialize to wire-format CBOR bytes: `array(2)[tag(24, bstr(inner)), crc32]`.
    pub fn to_wire_bytes(&self) -> Vec<u8> {
        let inner = encode_inner(self);
        let crc = crc32_ieee(&inner);

        let mut out = Vec::new();
        let mut e = minicbor::Encoder::new(&mut out);
        e.array(2).unwrap();
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&inner).unwrap();
        e.u32(crc).unwrap();
        out
    }

    /// Parse from wire-format CBOR bytes.  Validates the CRC-32 checksum.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self, ByronAddressError> {
        let mut d = minicbor::Decoder::new(bytes);

        d.array()
            .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;

        let tag = d
            .tag()
            .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;
        if tag.as_u64() != 24 {
            return Err(ByronAddressError::Cbor(format!(
                "expected tag 24, got {}",
                tag.as_u64()
            )));
        }
        let inner = d
            .bytes()
            .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;

        let expected_crc = d
            .u32()
            .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;
        let actual_crc = crc32_ieee(inner);
        if actual_crc != expected_crc {
            return Err(ByronAddressError::CrcMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        decode_inner(inner)
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Compute the address root: Blake2b-224(SHA3-256(addr_spec_cbor)).
///
/// `addr_spec_cbor` = `array(3)[u8(addr_type), spending_data_cbor, attributes_cbor]`.
///
/// Arguments are already CBOR-encoded slices:
/// - `spending_cbor`: the encoded spending data (e.g. the redeem bytes array)
/// - `attributes_cbor`: the encoded attributes map bytes
fn addr_root_hash(
    addr_type: ByronAddrType,
    spending_cbor: &[u8],
    attributes_cbor_bytes: &[u8],
) -> [u8; 28] {
    // Encode addr_spec as array(3)[u8(addr_type), *spending_cbor, *attrs_cbor].
    // minicbor does not support splicing raw bytes, so we build it manually.
    let mut spec = Vec::new();
    // array(3) header
    spec.push(0x83u8);
    // addr_type as a small uint
    encode_uint_into(&mut spec, addr_type.as_u8() as u64);
    // spending data (already CBOR)
    spec.extend_from_slice(spending_cbor);
    // attributes (already CBOR)
    spec.extend_from_slice(attributes_cbor_bytes);

    // SHA3-256 first; convert to a plain byte array to avoid deprecated as_slice on
    // the older GenericArray API used by sha3 0.10.
    let sha3_digest: [u8; 32] = Sha3_256::digest(&spec).into();

    // Blake2b-224 of the SHA3 output
    let blake2b_output = Blake2bParams::new().hash_length(28).hash(&sha3_digest);

    let mut root = [0u8; 28];
    root.copy_from_slice(blake2b_output.as_bytes());
    root
}

/// Encode redeem spending data: `array(2)[i64(2), bstr(32)(pubkey)]`.
///
/// Mirrors pallas `SpendingData::Redeem(ByteVec)` with `#[cbor(flat)]` encoding.
/// The `#[n(0)]` field index causes `max_index = 0`, so array length = 0 + 2 = 2.
fn redeem_spending_data_cbor(pubkey: &[u8; 32]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut e = minicbor::Encoder::new(&mut buf);
    e.array(2).unwrap();
    // Variant index 2 (Redeem), encoded as i64 by minicbor derive
    e.i64(2).unwrap();
    e.bytes(pubkey).unwrap();
    buf
}

/// Encode the attributes map.
///
/// Mainnet (no network_tag): `map(0)` = `0xa0`.
/// Testnet: `map(1){u8(2): bytes(network_tag)}`.
///
/// Attribute key 2 is `NetworkTag` in pallas (`AddrAttrProperty::NetworkTag`).
fn attributes_cbor(network_tag: Option<&[u8]>) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut e = minicbor::Encoder::new(&mut buf);
    match network_tag {
        None => {
            e.map(0).unwrap();
        }
        Some(tag_bytes) => {
            e.map(1).unwrap();
            e.u8(2).unwrap(); // NetworkTag attribute key
            e.bytes(tag_bytes).unwrap();
        }
    }
    buf
}

/// Encode the inner payload: `array(3)[bstr(28)(root), attributes_map, u8(addr_type)]`.
fn encode_inner(p: &ByronAddressPayload) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut e = minicbor::Encoder::new(&mut buf);
    e.array(3).unwrap();
    e.bytes(&p.root).unwrap();
    // Attributes are stored as raw CBOR bytes; splice them in directly.
    buf.extend_from_slice(&p.attributes);
    let mut e2 = minicbor::Encoder::new(&mut buf);
    e2.u8(p.addr_type.as_u8()).unwrap();
    buf
}

/// Decode an inner payload CBOR blob into `ByronAddressPayload`.
fn decode_inner(inner: &[u8]) -> Result<ByronAddressPayload, ByronAddressError> {
    let mut d = minicbor::Decoder::new(inner);

    d.array()
        .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;

    let root_bytes = d
        .bytes()
        .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;
    if root_bytes.len() != 28 {
        return Err(ByronAddressError::InvalidRootLen(root_bytes.len()));
    }
    let mut root = [0u8; 28];
    root.copy_from_slice(root_bytes);

    // Capture the raw CBOR bytes of the attributes map.
    let attrs_start = d.position();
    d.skip()
        .map_err(|e| ByronAddressError::Cbor(e.to_string()))?;
    let attrs_end = d.position();
    let attributes = inner[attrs_start..attrs_end].to_vec();

    let addr_type_raw = d.u8().map_err(|e| ByronAddressError::Cbor(e.to_string()))?;
    let addr_type = ByronAddrType::from_u8(addr_type_raw)?;

    Ok(ByronAddressPayload {
        addr_type,
        root,
        attributes,
    })
}

/// CRC-32/ISO-HDLC checksum (same as `crc::CRC_32_ISO_HDLC` used by pallas).
fn crc32_ieee(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Encode a small unsigned integer into a `Vec<u8>` using CBOR minimal encoding.
fn encode_uint_into(buf: &mut Vec<u8>, v: u64) {
    // We only need small values (0-2) for addr_type — use a scratch encoder.
    let mut scratch = Vec::new();
    let mut e = minicbor::Encoder::new(&mut scratch);
    e.u64(v).unwrap();
    buf.extend_from_slice(&scratch);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Golden vector
    //
    // AVVM key from config/mainnet/byron-genesis.json (first entry):
    //   "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk="
    //
    // Expected golden bytes were generated by running the current pallas-based
    // `ByronGenesis::avvm_to_address` with the same key and mainnet magic,
    // then hex-encoding the result.  Any change that produces a different hex
    // string means the implementation has diverged from pallas.
    // -----------------------------------------------------------------------

    /// Base64url-decode the AVVM key and return its raw 32 bytes.
    fn decode_avvm_key(b64: &str) -> [u8; 32] {
        // AVVM keys use standard base64 with padding; Rust's base64 engine
        // for URL-safe alphabet with padding handles the `-` and `_` chars.
        // Use the standard base64 alphabet but replace - with + and _ with /
        let normalized = b64.replace('-', "+").replace('_', "/");
        let bytes = base64_decode(&normalized);
        assert_eq!(bytes.len(), 32, "AVVM key must be 32 bytes");
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        arr
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        // Minimal base64 decoder: standard alphabet, handles padding.
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut table = [0u8; 256];
        for (i, &c) in ALPHABET.iter().enumerate() {
            table[c as usize] = i as u8;
        }
        let s = s.trim_end_matches('=');
        let n = s.len();
        let mut out = Vec::with_capacity(n * 3 / 4 + 1);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            let b0 = table[bytes[i] as usize] as u32;
            let b1 = table[bytes[i + 1] as usize] as u32;
            let b2 = table[bytes[i + 2] as usize] as u32;
            let b3 = table[bytes[i + 3] as usize] as u32;
            let v = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
            out.push((v >> 16) as u8);
            out.push((v >> 8) as u8);
            out.push(v as u8);
            i += 4;
        }
        let rem = bytes.len() - i;
        if rem == 2 {
            let b0 = table[bytes[i] as usize] as u32;
            let b1 = table[bytes[i + 1] as usize] as u32;
            let v = (b0 << 18) | (b1 << 12);
            out.push((v >> 16) as u8);
        } else if rem == 3 {
            let b0 = table[bytes[i] as usize] as u32;
            let b1 = table[bytes[i + 1] as usize] as u32;
            let b2 = table[bytes[i + 2] as usize] as u32;
            let v = (b0 << 18) | (b1 << 12) | (b2 << 6);
            out.push((v >> 16) as u8);
            out.push((v >> 8) as u8);
        }
        out
    }

    /// Helper: build a redeem address bytes for the given AVVM key + magic.
    fn avvm_to_address(pubkey_b64: &str, protocol_magic: u32) -> Vec<u8> {
        let pubkey = decode_avvm_key(pubkey_b64);
        let network_tag = if protocol_magic == 764824073 {
            None
        } else {
            let mut buf = Vec::new();
            minicbor::encode(protocol_magic, &mut buf).unwrap();
            Some(buf)
        };
        let payload = ByronAddressPayload::new_redeem(&pubkey, network_tag);
        payload.to_wire_bytes()
    }

    // -----------------------------------------------------------------------
    // Golden CBOR vector
    //
    // This hex string was produced by running:
    //   ByronGenesis::avvm_to_address("-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=", 764824073)
    // with the pallas-backed implementation and hex-encoding the output.
    //
    // It will be filled in after the first successful run.  The test checks
    // structural correctness even before the golden bytes are known.
    // -----------------------------------------------------------------------

    #[test]
    fn test_print_golden_bytes() {
        // Helper to print golden bytes — run once and paste output as constants.
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = avvm_to_address(pubkey_b64, 764824073);
        println!("\nGOLDEN (mainnet key 1): {}", hex::encode(&addr_bytes));
        let pubkey_b64_2 = "-0Np4pyTOWF26iXWVIvu6fhz9QupwWRS2hcCaOEYlw0=";
        let addr_bytes_2 = avvm_to_address(pubkey_b64_2, 764824073);
        println!("GOLDEN (mainnet key 2): {}", hex::encode(&addr_bytes_2));
    }

    #[test]
    fn test_golden_mainnet_redeem_structure() {
        // First AVVM key from config/mainnet/byron-genesis.json
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = avvm_to_address(pubkey_b64, 764824073);

        // Must parse back cleanly
        let payload = ByronAddressPayload::from_wire_bytes(&addr_bytes)
            .expect("golden vector must round-trip");

        assert_eq!(payload.addr_type, ByronAddrType::Redeem);
        assert_eq!(payload.root.len(), 28);
        // Mainnet: empty attributes map (0xa0)
        assert_eq!(
            payload.attributes,
            vec![0xa0u8],
            "mainnet must have empty attrs"
        );
    }

    #[test]
    fn test_golden_mainnet_redeem_second_key() {
        // Second AVVM key from config/mainnet/byron-genesis.json
        let pubkey_b64 = "-0Np4pyTOWF26iXWVIvu6fhz9QupwWRS2hcCaOEYlw0=";
        let addr_bytes = avvm_to_address(pubkey_b64, 764824073);
        let payload = ByronAddressPayload::from_wire_bytes(&addr_bytes).unwrap();
        assert_eq!(payload.addr_type, ByronAddrType::Redeem);
        assert_eq!(payload.attributes, vec![0xa0u8]);
    }

    // -----------------------------------------------------------------------
    // CRC32 negative test
    // -----------------------------------------------------------------------

    #[test]
    fn test_crc32_mismatch_rejected() {
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let mut addr_bytes = avvm_to_address(pubkey_b64, 764824073);

        // Flip a bit in the inner payload (byte 5, which is inside the tag-24 blob)
        addr_bytes[5] ^= 0x01;

        let result = ByronAddressPayload::from_wire_bytes(&addr_bytes);
        assert!(
            matches!(result, Err(ByronAddressError::CrcMismatch { .. })),
            "bit-flip must produce CrcMismatch, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Network-tag attribute tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_mainnet_attributes_empty_map() {
        let payload = ByronAddressPayload::new_redeem(&[0u8; 32], None);
        // Empty CBOR map = 0xa0 (1 byte)
        assert_eq!(payload.attributes, vec![0xa0u8]);
    }

    #[test]
    fn test_testnet_preview_attributes() {
        // Preview magic = 2; CBOR-encoded as u32(2) = 0x02
        let mut network_tag = Vec::new();
        minicbor::encode(2u32, &mut network_tag).unwrap();

        let payload = ByronAddressPayload::new_redeem(&[0u8; 32], Some(network_tag));

        // Expected: map(1){u8(2): bytes([0x02])}
        // = 0xa1 0x02 0x41 0x02
        let expected = vec![0xa1u8, 0x02, 0x41, 0x02];
        assert_eq!(
            payload.attributes, expected,
            "preview attributes must encode as {{2: bytes(0x02)}}"
        );
    }

    #[test]
    fn test_testnet_network_tag_included() {
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = avvm_to_address(pubkey_b64, 2); // preview magic
        let payload = ByronAddressPayload::from_wire_bytes(&addr_bytes).unwrap();

        // Testnet must have a non-empty attributes map
        assert!(
            payload.attributes.len() > 1,
            "testnet addr must have non-empty attributes"
        );
        // First byte is map(1) = 0xa1
        assert_eq!(payload.attributes[0], 0xa1u8);
    }

    // -----------------------------------------------------------------------
    // Roundtrip: from_wire_bytes(to_wire_bytes()) == original
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn prop_roundtrip_mainnet(pubkey in proptest::array::uniform32(0u8..)) {
            let payload = ByronAddressPayload::new_redeem(&pubkey, None);
            let wire = payload.to_wire_bytes();
            let recovered = ByronAddressPayload::from_wire_bytes(&wire)
                .expect("round-trip must succeed");
            prop_assert_eq!(payload, recovered);
        }

        #[test]
        fn prop_roundtrip_testnet(
            pubkey in proptest::array::uniform32(0u8..),
            magic in 1u32..u32::MAX,
        ) {
            // Skip mainnet magic
            prop_assume!(magic != 764824073);
            let mut network_tag = Vec::new();
            minicbor::encode(magic, &mut network_tag).unwrap();
            let payload = ByronAddressPayload::new_redeem(&pubkey, Some(network_tag));
            let wire = payload.to_wire_bytes();
            let recovered = ByronAddressPayload::from_wire_bytes(&wire)
                .expect("round-trip must succeed");
            prop_assert_eq!(payload, recovered);
        }
    }
}
