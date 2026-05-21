//! Bounded CBOR decoders for peer-supplied data.
//!
//! This module exposes hardened decoders that enforce explicit caps on every
//! peer-supplied length header, mirroring the systemic security pattern #5 of
//! the 2026-05-19 security audit (#548) and the targeted #554 follow-up for
//! tx metadata.
//!
//! **Background.** Naively calling `Vec::with_capacity(declared_len)` /
//! `Vec::reserve(declared_len)` on a length read from CBOR allows a malicious
//! peer to declare `array(u64::MAX)` and force a huge allocation attempt
//! BEFORE the decode loop ever runs. Even a graceful allocation failure can
//! abort the process under aggressive allocator settings, and on systems with
//! overcommit the kernel will OOM-kill the node.
//!
//! **The pattern.** Every decoder that allocates from a CBOR-supplied length
//! must:
//!
//! 1. Decode the length header.
//! 2. Compare it to the protocol-spec maximum (e.g. `MAX_METADATA_ENTRIES`,
//!    `MAX_TX_WITNESSES`) BEFORE allocating.
//! 3. Compare it to `remaining_bytes / minimum_element_size` BEFORE allocating
//!    — a peer can claim a million elements but only have 10 bytes of input
//!    left, so the declared length must be physically realisable.
//! 4. Pre-allocate at the capped value, then run the decode loop with the
//!    real check on each element.
//!
//! Pallas does its own decoding for most of the wire surface, but anywhere
//! dugite owns the decoder, this module is the canonical defense.
//!
//! Add new sites to the module-level list as they're defended:
//!
//! - `decode_metadatum_bounded` — recursive bounded decoder for tx metadata
//!   maps/lists (#554)

use crate::error::SerializationError;
use dugite_primitives::transaction::TransactionMetadatum;
use minicbor::{data::Type, Decoder};

/// Maximum number of entries in any tx metadata map/array (top level or
/// nested). Mirrors Haskell's tolerance — real-world metadata maps never
/// exceed a few hundred entries; 16,384 is generous.
pub const MAX_METADATA_ENTRIES: u64 = 16_384;

/// Maximum recursive nesting depth in tx metadata. Haskell's metadata
/// validator rejects anything deeper to prevent stack overflow during decode.
pub const MAX_METADATA_DEPTH: u32 = 64;

/// Maximum text/bytes length within a single metadatum field. CIP-25 caps
/// individual fields at 64 bytes; we allow 64 KiB as a generous ceiling.
pub const MAX_METADATA_FIELD_BYTES: u64 = 65_536;

/// Decode a `TransactionMetadatum` from a CBOR decoder with explicit caps on
/// every length-prefixed structure.
///
/// **Defense against:**
/// - `array(u64::MAX)` / `map(u64::MAX)` declared-length allocation bombs
///   (#554).
/// - Deeply nested `[[[[...]]]]` stack-overflow attacks.
/// - Pathological single-field byte/text strings up to `u64::MAX`.
///
/// **Caps applied:**
/// - `MAX_METADATA_ENTRIES` per Map/List (declared length must satisfy).
/// - `MAX_METADATA_DEPTH` recursive nesting.
/// - `MAX_METADATA_FIELD_BYTES` per Bytes/Text leaf.
/// - Physical-realisability check: declared length × 1 byte ≤
///   `remaining_input_bytes`.
///
/// The remaining-input check uses `decoder.input().len() - decoder.position()`.
pub fn decode_metadatum_bounded(
    dec: &mut Decoder<'_>,
) -> Result<TransactionMetadatum, SerializationError> {
    decode_metadatum_with_depth(dec, 0)
}

fn decode_metadatum_with_depth(
    dec: &mut Decoder<'_>,
    depth: u32,
) -> Result<TransactionMetadatum, SerializationError> {
    if depth > MAX_METADATA_DEPTH {
        return Err(SerializationError::CborDecode(format!(
            "metadatum nesting exceeded MAX_METADATA_DEPTH ({MAX_METADATA_DEPTH})"
        )));
    }

    let dt = dec
        .datatype()
        .map_err(|e| SerializationError::CborDecode(e.to_string()))?;

    match dt {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let v = dec
                .u64()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?;
            Ok(TransactionMetadatum::Int(v as i128))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
            let v = dec
                .i64()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?;
            Ok(TransactionMetadatum::Int(v as i128))
        }
        Type::Int => {
            let v = dec
                .int()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?;
            // minicbor::data::Int -> i128 is infallible.
            let v128: i128 = v.into();
            Ok(TransactionMetadatum::Int(v128))
        }
        Type::Bytes => {
            let pos_before = dec.position();
            let bytes = dec
                .bytes()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?;
            if bytes.len() as u64 > MAX_METADATA_FIELD_BYTES {
                return Err(SerializationError::CborDecode(format!(
                    "metadatum bytes len {} exceeds MAX_METADATA_FIELD_BYTES ({MAX_METADATA_FIELD_BYTES}); pos {}",
                    bytes.len(),
                    pos_before
                )));
            }
            Ok(TransactionMetadatum::Bytes(bytes.to_vec()))
        }
        Type::BytesIndef => {
            // Reject indefinite-length bytes here; we treat them as
            // non-canonical for metadata. (Pallas also rejects.)
            Err(SerializationError::CborDecode(
                "metadatum: indefinite-length bytes not allowed".into(),
            ))
        }
        Type::String => {
            let pos_before = dec.position();
            let s = dec
                .str()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?;
            if s.len() as u64 > MAX_METADATA_FIELD_BYTES {
                return Err(SerializationError::CborDecode(format!(
                    "metadatum text len {} exceeds MAX_METADATA_FIELD_BYTES ({MAX_METADATA_FIELD_BYTES}); pos {}",
                    s.len(),
                    pos_before
                )));
            }
            Ok(TransactionMetadatum::Text(s.to_string()))
        }
        Type::StringIndef => Err(SerializationError::CborDecode(
            "metadatum: indefinite-length text not allowed".into(),
        )),
        Type::Array => {
            let remaining = dec.input().len().saturating_sub(dec.position());
            let len = dec
                .array()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?
                .ok_or_else(|| {
                    SerializationError::CborDecode(
                        "metadatum: expected definite array length".into(),
                    )
                })?;
            check_collection_cap(len, remaining, "metadatum array")?;
            let cap = cap_capacity(len);
            let mut items = Vec::with_capacity(cap);
            for _ in 0..len {
                items.push(decode_metadatum_with_depth(dec, depth + 1)?);
            }
            Ok(TransactionMetadatum::List(items))
        }
        Type::ArrayIndef => {
            // Mirror Haskell: metadata uses definite-length arrays only.
            Err(SerializationError::CborDecode(
                "metadatum: indefinite-length array not allowed".into(),
            ))
        }
        Type::Map => {
            let remaining = dec.input().len().saturating_sub(dec.position());
            let len = dec
                .map()
                .map_err(|e| SerializationError::CborDecode(e.to_string()))?
                .ok_or_else(|| {
                    SerializationError::CborDecode("metadatum: expected definite map length".into())
                })?;
            check_collection_cap(len, remaining, "metadatum map")?;
            let cap = cap_capacity(len);
            let mut entries = Vec::with_capacity(cap);
            for _ in 0..len {
                let k = decode_metadatum_with_depth(dec, depth + 1)?;
                let v = decode_metadatum_with_depth(dec, depth + 1)?;
                entries.push((k, v));
            }
            Ok(TransactionMetadatum::Map(entries))
        }
        Type::MapIndef => Err(SerializationError::CborDecode(
            "metadatum: indefinite-length map not allowed".into(),
        )),
        other => Err(SerializationError::CborDecode(format!(
            "metadatum: unsupported CBOR type {other:?}"
        ))),
    }
}

/// Common cap-check helper: rejects if declared length exceeds the protocol
/// maximum OR if it's physically impossible given the remaining input.
fn check_collection_cap(
    declared: u64,
    remaining_bytes: usize,
    label: &str,
) -> Result<(), SerializationError> {
    if declared > MAX_METADATA_ENTRIES {
        return Err(SerializationError::CborDecode(format!(
            "{label}: declared length {declared} exceeds MAX_METADATA_ENTRIES ({MAX_METADATA_ENTRIES})"
        )));
    }
    // Every CBOR element occupies at least 1 byte (a single-byte primitive
    // like 0x00 or 0xf6). For maps, both key and value need at least one
    // byte each, so the per-entry minimum is 2 bytes for maps and 1 byte for
    // arrays. We use the conservative 1-byte estimate to catch
    // u64::MAX-class attacks without rejecting tight edge cases.
    if declared > remaining_bytes as u64 {
        return Err(SerializationError::CborDecode(format!(
            "{label}: declared length {declared} exceeds remaining input bytes ({remaining_bytes})"
        )));
    }
    Ok(())
}

/// Clamp the pre-allocated `Vec::with_capacity` value to the
/// `MAX_METADATA_ENTRIES` ceiling. The caller still drives the decode loop by
/// the declared length, so we never silently truncate — we just cap the
/// initial allocation. (The cap is enforced separately by
/// `check_collection_cap`.)
fn cap_capacity(declared: u64) -> usize {
    declared.min(MAX_METADATA_ENTRIES) as usize
}

/// Decode a `TransactionMetadatum` from a raw byte slice using
/// `decode_metadatum_bounded`. Convenience wrapper for callers that don't
/// already hold a `Decoder`.
pub fn decode_metadatum_from_bytes(
    data: &[u8],
) -> Result<TransactionMetadatum, SerializationError> {
    let mut dec = Decoder::new(data);
    decode_metadatum_bounded(&mut dec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::encode_metadatum;
    use minicbor::Encoder;

    fn write_u8_to_vec(buf: &mut Vec<u8>, bytes: &[u8]) {
        buf.extend_from_slice(bytes);
    }

    #[test]
    fn happy_path_int() {
        let meta = TransactionMetadatum::Int(42);
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn happy_path_text() {
        let meta = TransactionMetadatum::Text("hello".into());
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn happy_path_bytes() {
        let meta = TransactionMetadatum::Bytes(vec![1, 2, 3, 4]);
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn happy_path_list() {
        let meta = TransactionMetadatum::List(vec![
            TransactionMetadatum::Int(1),
            TransactionMetadatum::Int(2),
            TransactionMetadatum::Int(3),
        ]);
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn happy_path_map() {
        let meta = TransactionMetadatum::Map(vec![
            (
                TransactionMetadatum::Text("k".into()),
                TransactionMetadatum::Int(7),
            ),
            (
                TransactionMetadatum::Int(0),
                TransactionMetadatum::Bytes(vec![0xff]),
            ),
        ]);
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn happy_path_nested() {
        let inner = TransactionMetadatum::List(vec![
            TransactionMetadatum::Int(1),
            TransactionMetadatum::Int(2),
        ]);
        let meta =
            TransactionMetadatum::Map(vec![(TransactionMetadatum::Text("nested".into()), inner)]);
        let cbor = encode_metadatum(&meta);
        let decoded = decode_metadatum_from_bytes(&cbor).unwrap();
        assert_eq!(decoded, meta);
    }

    // ---- attack vectors ----

    #[test]
    fn rejects_array_with_u64_max_length() {
        // CBOR: 0x9b ff ff ff ff ff ff ff ff (array of u64::MAX elements)
        let bytes: Vec<u8> = vec![0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        let msg = format!("{err}");
        // Should reject either via MAX_METADATA_ENTRIES or remaining-bytes
        // check; both messages contain "exceeds".
        assert!(msg.contains("exceeds"), "got: {msg}");
    }

    #[test]
    fn rejects_array_with_u32_max_length() {
        // CBOR: 0x9a ff ff ff ff (array of u32::MAX elements)
        let bytes: Vec<u8> = vec![0x9a, 0xff, 0xff, 0xff, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        assert!(format!("{err}").contains("exceeds"));
    }

    #[test]
    fn rejects_map_with_u64_max_length() {
        // CBOR: 0xbb ff ff ff ff ff ff ff ff (map of u64::MAX entries)
        let bytes: Vec<u8> = vec![0xbb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        assert!(format!("{err}").contains("exceeds"));
    }

    #[test]
    fn rejects_indefinite_array() {
        // CBOR: 0x9f 0x00 0xff (indefinite array containing 0)
        let bytes: Vec<u8> = vec![0x9f, 0x00, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        assert!(format!("{err}").contains("indefinite"));
    }

    #[test]
    fn rejects_indefinite_map() {
        // CBOR: 0xbf 0x00 0x00 0xff
        let bytes: Vec<u8> = vec![0xbf, 0x00, 0x00, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        assert!(format!("{err}").contains("indefinite"));
    }

    #[test]
    fn rejects_excess_nesting_depth() {
        // Build an array nested MAX_METADATA_DEPTH + 5 deep.
        let mut buf = Vec::new();
        let depth = MAX_METADATA_DEPTH as usize + 5;
        for _ in 0..depth {
            // 0x81 = array of length 1
            write_u8_to_vec(&mut buf, &[0x81]);
        }
        // Innermost = 0
        write_u8_to_vec(&mut buf, &[0x00]);
        let err = decode_metadatum_from_bytes(&buf).unwrap_err();
        assert!(
            format!("{err}").contains("MAX_METADATA_DEPTH") || format!("{err}").contains("depth"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_oversized_text_field() {
        // CBOR: 0x7b ff ff ff ff ff ff ff ff (text of u64::MAX bytes).
        // Minicbor's str() should error on insufficient input before our
        // length check, but the failure is still graceful.
        let bytes: Vec<u8> = vec![0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        // Either our cap rejects it OR minicbor's bounds check does — we
        // just need NOT to panic / not allocate u64::MAX bytes.
        let msg = format!("{err}");
        assert!(!msg.is_empty());
    }

    #[test]
    fn rejects_oversized_bytes_field() {
        // CBOR: 0x5b ff ff ff ff ff ff ff ff
        let bytes: Vec<u8> = vec![0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let err = decode_metadatum_from_bytes(&bytes).unwrap_err();
        assert!(!format!("{err}").is_empty());
    }

    #[test]
    fn accepts_at_protocol_max_entries() {
        // Build a list with exactly MAX_METADATA_ENTRIES entries (small
        // payload). This must NOT be rejected, and must allocate only ~16K
        // entries.
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(MAX_METADATA_ENTRIES).unwrap();
        for _ in 0..MAX_METADATA_ENTRIES {
            enc.u8(0).unwrap(); // each element = single byte 0x00
        }
        let decoded = decode_metadatum_from_bytes(&buf).unwrap();
        if let TransactionMetadatum::List(items) = decoded {
            assert_eq!(items.len(), MAX_METADATA_ENTRIES as usize);
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn rejects_one_over_protocol_max_entries() {
        // Declares MAX_METADATA_ENTRIES + 1 entries — should reject.
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(MAX_METADATA_ENTRIES + 1).unwrap();
        // We don't bother writing the contents; the cap check fires on the
        // length header.
        let err = decode_metadatum_from_bytes(&buf).unwrap_err();
        assert!(format!("{err}").contains("MAX_METADATA_ENTRIES"));
    }

    // length-lattice property: for any (declared_len, actual_len_byte_payload)
    // the decoder rejects without allocating more than MAX_METADATA_ENTRIES.
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn prop_length_lattice_reject_or_decode(
                declared in 0u64..=u64::MAX,
                actual_bytes in 0usize..=1024,
            ) {
                // Build CBOR: array(declared) followed by `actual_bytes` zero
                // bytes (each is one CBOR element).
                let mut buf = Vec::new();
                let mut enc = Encoder::new(&mut buf);
                enc.array(declared).unwrap();
                buf.resize(buf.len() + actual_bytes, 0x00);

                let result = decode_metadatum_from_bytes(&buf);
                if declared <= MAX_METADATA_ENTRIES
                    && declared as usize <= actual_bytes
                {
                    // Should succeed.
                    prop_assert!(result.is_ok());
                } else {
                    // Should reject — no allocation > MAX_METADATA_ENTRIES.
                    prop_assert!(result.is_err());
                }
            }
        }
    }
}
