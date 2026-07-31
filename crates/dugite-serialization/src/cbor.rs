use crate::error::SerializationError;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{PlutusData, TransactionInput, TransactionMetadatum};

/// Extract `(slot, block_no, header_hash)` from a multi-era block CBOR.
///
/// Used by chunk-file import (Mithril) where callers only need to index a
/// block by its header identity without paying for a full body decode. The
/// returned triple is byte-equal to what a full decode would yield —
/// importers can use it to populate their `(slot, hash) -> chunk_offset`
/// indexes safely.
///
/// Currently delegates to [`crate::decode::decode_block_minimal`]. A future
/// optimisation could replace this with a header-only CBOR walker (Shelley+:
/// `blake2b_256(header_cbor)` over the first inner-array element; Byron:
/// the Cardano-ledger `coerceHash` wrapper). The current implementation
/// does a minimal body decode — correct, but heavier than needed for
/// pure identity extraction.
pub fn extract_block_identity(
    cbor: &[u8],
) -> Result<(SlotNo, BlockNo, Hash32), SerializationError> {
    let block = crate::decode::decode_block_minimal(cbor)?;
    Ok((block.slot(), block.block_number(), *block.hash()))
}

/// Encode a Hash32 to CBOR bytes
pub fn encode_hash32(hash: &Hash32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(0x58); // byte string, 1-byte length
    buf.push(32);
    buf.extend_from_slice(hash.as_bytes());
    buf
}

/// Decode a Hash32 from CBOR bytes
pub fn decode_hash32(data: &[u8]) -> Result<(Hash32, usize), SerializationError> {
    if data.len() < 2 {
        return Err(SerializationError::InvalidLength {
            expected: 34,
            got: data.len(),
        });
    }
    match data[0] {
        0x58 => {
            let len = data[1] as usize;
            if len != 32 || data.len() < 2 + 32 {
                return Err(SerializationError::InvalidLength {
                    expected: 32,
                    got: len,
                });
            }
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&data[2..34]);
            Ok((Hash32::from_bytes(bytes), 34))
        }
        // Short byte string (length embedded in first byte)
        b if (b & 0xe0) == 0x40 => {
            let len = (b & 0x1f) as usize;
            if len != 32 || data.len() < 1 + 32 {
                return Err(SerializationError::InvalidLength {
                    expected: 32,
                    got: len,
                });
            }
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&data[1..33]);
            Ok((Hash32::from_bytes(bytes), 33))
        }
        _ => Err(SerializationError::CborDecode(format!(
            "Expected byte string, got {:#04x}",
            data[0]
        ))),
    }
}

/// Encode a Point to CBOR
pub fn encode_point(point: &Point) -> Vec<u8> {
    match point {
        Point::Origin => {
            // Origin is encoded as CBOR array with tag
            vec![0x82, 0x00, 0x80] // [0, []]
        }
        Point::Specific(slot, hash) => {
            let mut buf = Vec::new();
            buf.push(0x82); // array of 2
                            // Encode slot as unsigned integer
            buf.extend(encode_uint(slot.0));
            // Encode hash as byte string
            buf.extend(encode_hash32(hash));
            buf
        }
    }
}

/// Encode an unsigned integer to CBOR
pub fn encode_uint(value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if value < 24 {
        buf.push(value as u8);
    } else if value < 256 {
        buf.push(0x18);
        buf.push(value as u8);
    } else if value < 65536 {
        buf.push(0x19);
        buf.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value < 4294967296 {
        buf.push(0x1a);
        buf.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        buf.push(0x1b);
        buf.extend_from_slice(&value.to_be_bytes());
    }
    buf
}

/// Encode an arbitrary-precision Plutus integer to CBOR.
///
/// Plutus values are unbounded (Haskell `Integer`). For magnitudes that fit in
/// i128 we emit the small-int encoding (major types 0/1); larger values use
/// CBOR tag 2 (positive bigint) or tag 3 (negative bigint) followed by a
/// byte-string of the big-endian magnitude.
pub fn encode_plutus_int(value: &num_bigint::BigInt) -> Vec<u8> {
    use num_bigint::Sign;
    use num_traits::ToPrimitive;
    if let Some(v) = value.to_i128() {
        return encode_int(v);
    }
    let (tag, mag) = if value.sign() == Sign::Minus {
        let n = -value - num_bigint::BigInt::from(1);
        let (_, bytes) = n.to_bytes_be();
        (3u64, bytes)
    } else {
        let (_, bytes) = value.to_bytes_be();
        (2u64, bytes)
    };
    let mut buf = encode_tag(tag);
    // The bignum magnitude is a `PlutusData` `ByteString` leaf (Haskell
    // `encodeInteger` outside [-1-maxWord64 .. maxWord64] -> tag 2/3 +
    // `encodeBs (integerToBytesBE magnitude)`), so it obeys the same 64-byte
    // chunking bound as the `Bytes` leaf arm. See #28b.
    buf.extend(encode_bounded_plutus_bytes(&mag));
    buf
}

/// Encode a signed integer to CBOR
pub fn encode_int(value: i128) -> Vec<u8> {
    if value >= 0 {
        encode_uint(value as u64)
    } else {
        let abs_val = (-1 - value) as u64;
        let mut buf = Vec::new();
        if abs_val < 24 {
            buf.push(0x20 | abs_val as u8);
        } else if abs_val < 256 {
            buf.push(0x38);
            buf.push(abs_val as u8);
        } else if abs_val < 65536 {
            buf.push(0x39);
            buf.extend_from_slice(&(abs_val as u16).to_be_bytes());
        } else if abs_val < 4294967296 {
            buf.push(0x3a);
            buf.extend_from_slice(&(abs_val as u32).to_be_bytes());
        } else {
            buf.push(0x3b);
            buf.extend_from_slice(&abs_val.to_be_bytes());
        }
        buf
    }
}

/// Encode a byte string to CBOR
pub fn encode_bytes(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let len = data.len();
    if len < 24 {
        buf.push(0x40 | len as u8);
    } else if len < 256 {
        buf.push(0x58);
        buf.push(len as u8);
    } else if len < 65536 {
        buf.push(0x59);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0x5a);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(data);
    buf
}

/// Encode a text string to CBOR
pub fn encode_text(text: &str) -> Vec<u8> {
    let data = text.as_bytes();
    let mut buf = Vec::new();
    let len = data.len();
    if len < 24 {
        buf.push(0x60 | len as u8);
    } else if len < 256 {
        buf.push(0x78);
        buf.push(len as u8);
    } else if len < 65536 {
        buf.push(0x79);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0x7a);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(data);
    buf
}

/// Encode a CBOR array header
pub fn encode_array_header(len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    if len < 24 {
        buf.push(0x80 | len as u8);
    } else if len < 256 {
        buf.push(0x98);
        buf.push(len as u8);
    } else if len < 65536 {
        buf.push(0x99);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0x9a);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf
}

/// Encode a CBOR map header
pub fn encode_map_header(len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    if len < 24 {
        buf.push(0xa0 | len as u8);
    } else if len < 256 {
        buf.push(0xb8);
        buf.push(len as u8);
    } else if len < 65536 {
        buf.push(0xb9);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xba);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf
}

/// Haskell cardano-ledger-binary `encodeMap` threshold (issues #930/#932).
///
/// From encoding version >= 2 (i.e. every Shelley+ era) `encodeMap` uses
/// `variableMapLenEncoding`: a DEFINITE-length map header for maps with at
/// most 23 entries, and an INDEFINITE-length map (`0xbf` open ... `0xff`
/// break) above that. The rule applies independently at every map nesting
/// level of a structure encoded via Haskell's generic `EncCBOR (Map k v)`
/// instance (and `encodeFoldableMapEncoder`'s `wrapCBORMap`, which shares
/// the same threshold).
///
/// Use [`encode_map_open`]/[`encode_map_close`] for any encoder site whose
/// Haskell counterpart goes through `encodeMap`. Do NOT use them for
/// integer-keyed struct-as-map encodings (tx body, witness set, PParams
/// updates — Haskell `Keyed`/`Omit` coders, always definite) or for
/// `PlutusData::Map` (plutus `encodeData` — a different encoder, correctly
/// definite-only).
pub(crate) const ENCODE_MAP_DEFINITE_MAX: usize = 23;

/// Open a map following Haskell `encodeMap` semantics: definite-length
/// header for `len <= 23`, indefinite open byte (`0xbf`) otherwise.
pub(crate) fn encode_map_open(len: usize) -> Vec<u8> {
    if len <= ENCODE_MAP_DEFINITE_MAX {
        encode_map_header(len)
    } else {
        vec![0xbf]
    }
}

/// Close a map opened by [`encode_map_open`]: emit the CBOR break (`0xff`)
/// only for the indefinite (> 23 entries) form.
pub(crate) fn encode_map_close(buf: &mut Vec<u8>, len: usize) {
    if len > ENCODE_MAP_DEFINITE_MAX {
        buf.push(0xff);
    }
}

/// Haskell cardano-ledger-binary `variableListLenEncoding` threshold (#938).
///
/// The array/list/set counterpart of [`ENCODE_MAP_DEFINITE_MAX`]. Verbatim
/// from `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/Encoder.hs`:
///
/// ```haskell
/// lengthThreshold :: Int
/// lengthThreshold = 23
///
/// variableListLenEncoding len contents =
///   if len <= lengthThreshold
///     then exactListLenEncoding len contents
///     else encodeListLenIndef <> contents <> encodeBreak
/// ```
///
/// Every variable-length collection encoder funnels through it:
/// `encodeFoldableEncoder`, `encodeStrictSeq` / `encodeSeq`, `encodeList`, and
/// `encodeSet` (which, from PV9, emits `encodeTag 258` *before* the
/// variable-length array — the tag wraps the whole thing, so the threshold
/// still applies to the array inside).
///
/// Use [`encode_array_open`]/[`encode_array_close`] for any encoder site whose
/// Haskell counterpart is one of those. Do **not** use them for fixed-arity
/// structural records — Haskell writes those with a literal `encodeListLen n`
/// (e.g. `encodeStrictMaybe`'s `encodeListLen 0`/`1`, the `[era_tag, block]`
/// envelope, `[a, b]` pairs), which is always definite regardless of `n`.
pub(crate) const ENCODE_ARRAY_DEFINITE_MAX: usize = 23;

/// Open an array following Haskell `variableListLenEncoding` semantics:
/// definite-length header for `len <= 23`, indefinite open byte (`0x9f`)
/// otherwise. Pair with [`encode_array_close`].
pub(crate) fn encode_array_open(len: usize) -> Vec<u8> {
    if len <= ENCODE_ARRAY_DEFINITE_MAX {
        encode_array_header(len)
    } else {
        vec![0x9f]
    }
}

/// Close an array opened by [`encode_array_open`]: emit the CBOR break
/// (`0xff`) only for the indefinite (> 23 items) form.
pub(crate) fn encode_array_close(buf: &mut Vec<u8>, len: usize) {
    if len > ENCODE_ARRAY_DEFINITE_MAX {
        buf.push(0xff);
    }
}

/// Encode a `PlutusData` `ByteString` *leaf* (the `Bytes` arm and the tag-2 /
/// tag-3 bignum mantissa) with the plutus 64-byte-per-chunk bound.
///
/// Mirrors Haskell `plutus` `PlutusCore.Data.encodeData` `encodeBs` (and the
/// inverse of dugite's own [`Reader::read_bounded_plutus_bytes`], #28):
///
/// - `len <= 64` -> a single **definite**-length byte string
///   (`CBOR.encodeBytes`). The empty leaf encodes as `0x40`.
/// - `len > 64`  -> an **indefinite**-length byte string `0x5f` ... `0xff`
///   whose payload is `to64ByteChunks` of the input: a *greedy* split into
///   64-byte definite chunks (`data.chunks(64)`), each emitted as its own
///   definite byte string. A 128-byte leaf becomes exactly two 64-byte
///   chunks (the final 64 is **not** re-split); a 100-byte leaf becomes a
///   64-byte chunk followed by a 36-byte chunk.
///
/// The bound constant is shared with the decoder
/// ([`Reader::PLUTUS_DATA_BYTES_LEAF_MAX`]) so the encode and decode bounds can
/// never drift. This is **only** applied at `PlutusData` leaf sites — the
/// generic [`encode_bytes`] deliberately stays a single definite byte string
/// for any size, because it serves ~40 non-plutus call sites (addresses,
/// 28/32-byte hashes, native + Plutus script blobs that routinely exceed 64
/// bytes, metadata, pool relay IPs, reward/return addresses) where chunking
/// would corrupt the field and break block-body / tx hashes.
fn encode_bounded_plutus_bytes(data: &[u8]) -> Vec<u8> {
    const PLUTUS_LEAF_MAX: usize = crate::decode::reader::Reader::PLUTUS_DATA_BYTES_LEAF_MAX;
    if data.len() <= PLUTUS_LEAF_MAX {
        return encode_bytes(data);
    }
    let mut buf = vec![0x5f];
    for chunk in data.chunks(PLUTUS_LEAF_MAX) {
        buf.extend(encode_bytes(chunk));
    }
    buf.push(0xff);
    buf
}

/// Encode PlutusData to CBOR
pub fn encode_plutus_data(data: &PlutusData) -> Vec<u8> {
    match data {
        PlutusData::Constr(tag, fields) => {
            let mut buf = Vec::new();
            // Use CBOR tag 121 + constructor index for small constructors
            if *tag < 7 {
                let cbor_tag = 121 + tag;
                buf.push(0xd8); // tag (1-byte)
                buf.push(cbor_tag as u8);
            } else if *tag < 128 {
                let cbor_tag = 1280 + (tag - 7);
                buf.push(0xd9); // tag (2-byte)
                buf.extend_from_slice(&(cbor_tag as u16).to_be_bytes());
            } else {
                // General form: tag 102 wrapping [constructor, fields]
                buf.push(0xd8); // tag (1-byte)
                buf.push(0x66); // tag 102
                                // Encode as [constructor_index, [fields...]]
                let mut inner = encode_array_header(2);
                inner.extend(encode_uint(*tag));
                inner.extend(encode_array_header(fields.len()));
                for field in fields {
                    inner.extend(encode_plutus_data(field));
                }
                buf.extend(inner);
                return buf;
            }
            buf.extend(encode_array_header(fields.len()));
            for field in fields {
                buf.extend(encode_plutus_data(field));
            }
            buf
        }
        PlutusData::Map(entries) => {
            let mut buf = encode_map_header(entries.len());
            for (k, v) in entries {
                buf.extend(encode_plutus_data(k));
                buf.extend(encode_plutus_data(v));
            }
            buf
        }
        PlutusData::List(items) => {
            let mut buf = encode_array_header(items.len());
            for item in items {
                buf.extend(encode_plutus_data(item));
            }
            buf
        }
        PlutusData::Integer(n) => encode_plutus_int(n),
        PlutusData::Bytes(b) => encode_bounded_plutus_bytes(b),
    }
}

/// Encode a TransactionInput to CBOR [tx_hash, index]
pub fn encode_tx_input(input: &TransactionInput) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_hash32(&input.transaction_id));
    buf.extend(encode_uint(input.index as u64));
    buf
}

/// Encode a Hash28 to CBOR bytes
pub fn encode_hash28(hash: &Hash28) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(0x58); // byte string, 1-byte length
    buf.push(28);
    buf.extend_from_slice(hash.as_bytes());
    buf
}

/// Encode a CBOR tag
pub fn encode_tag(tag: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if tag < 24 {
        buf.push(0xc0 | tag as u8);
    } else if tag < 256 {
        buf.push(0xd8);
        buf.push(tag as u8);
    } else if tag < 65536 {
        buf.push(0xd9);
        buf.extend_from_slice(&(tag as u16).to_be_bytes());
    } else {
        buf.push(0xda);
        buf.extend_from_slice(&(tag as u32).to_be_bytes());
    }
    buf
}

/// Encode a CBOR bool
pub fn encode_bool(value: bool) -> Vec<u8> {
    vec![if value { 0xf5 } else { 0xf4 }]
}

/// Encode CBOR null
pub fn encode_null() -> Vec<u8> {
    vec![0xf6]
}

/// Encode transaction metadata to CBOR
pub fn encode_metadatum(metadatum: &TransactionMetadatum) -> Vec<u8> {
    match metadatum {
        TransactionMetadatum::Int(n) => encode_int(*n),
        TransactionMetadatum::Bytes(b) => encode_bytes(b),
        TransactionMetadatum::Text(t) => encode_text(t),
        TransactionMetadatum::List(items) => {
            let mut buf = encode_array_header(items.len());
            for item in items {
                buf.extend(encode_metadatum(item));
            }
            buf
        }
        TransactionMetadatum::Map(entries) => {
            let mut buf = encode_map_header(entries.len());
            for (k, v) in entries {
                buf.extend(encode_metadatum(k));
                buf.extend(encode_metadatum(v));
            }
            buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::time::SlotNo;

    #[test]
    fn test_encode_uint_small() {
        assert_eq!(encode_uint(0), vec![0x00]);
        assert_eq!(encode_uint(1), vec![0x01]);
        assert_eq!(encode_uint(23), vec![0x17]);
    }

    #[test]
    fn test_encode_uint_one_byte() {
        assert_eq!(encode_uint(24), vec![0x18, 0x18]);
        assert_eq!(encode_uint(255), vec![0x18, 0xff]);
    }

    #[test]
    fn test_encode_uint_two_bytes() {
        assert_eq!(encode_uint(256), vec![0x19, 0x01, 0x00]);
        assert_eq!(encode_uint(1000), vec![0x19, 0x03, 0xe8]);
    }

    #[test]
    fn test_encode_uint_four_bytes() {
        assert_eq!(encode_uint(1_000_000), vec![0x1a, 0x00, 0x0f, 0x42, 0x40]);
    }

    #[test]
    fn test_encode_negative_int() {
        assert_eq!(encode_int(-1), vec![0x20]);
        assert_eq!(encode_int(-10), vec![0x29]);
        assert_eq!(encode_int(-100), vec![0x38, 0x63]);
    }

    #[test]
    fn test_encode_bytes() {
        let data = vec![0x01, 0x02, 0x03];
        let encoded = encode_bytes(&data);
        assert_eq!(encoded[0], 0x43); // byte string of length 3
        assert_eq!(&encoded[1..], &data);
    }

    #[test]
    fn test_encode_text() {
        let encoded = encode_text("hello");
        assert_eq!(encoded[0], 0x65); // text string of length 5
        assert_eq!(&encoded[1..], b"hello");
    }

    #[test]
    fn test_encode_array_header() {
        assert_eq!(encode_array_header(0), vec![0x80]);
        assert_eq!(encode_array_header(3), vec![0x83]);
        assert_eq!(encode_array_header(24), vec![0x98, 0x18]);
    }

    #[test]
    fn test_encode_map_header() {
        assert_eq!(encode_map_header(0), vec![0xa0]);
        assert_eq!(encode_map_header(2), vec![0xa2]);
    }

    #[test]
    fn test_encode_hash32() {
        let hash = Hash32::ZERO;
        let encoded = encode_hash32(&hash);
        assert_eq!(encoded.len(), 34); // 2 byte header + 32 bytes
        assert_eq!(encoded[0], 0x58);
        assert_eq!(encoded[1], 32);
    }

    #[test]
    fn test_encode_point_origin() {
        let point = Point::Origin;
        let encoded = encode_point(&point);
        assert_eq!(encoded, vec![0x82, 0x00, 0x80]);
    }

    #[test]
    fn test_encode_point_specific() {
        let point = Point::Specific(SlotNo(100), Hash32::ZERO);
        let encoded = encode_point(&point);
        assert_eq!(encoded[0], 0x82); // array of 2
        assert_eq!(encoded[1], 0x18); // uint 100
        assert_eq!(encoded[2], 100);
    }

    #[test]
    fn test_encode_plutus_data_integer() {
        let data = PlutusData::Integer(num_bigint::BigInt::from(42i64));
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded, vec![0x18, 42]);
    }

    #[test]
    fn test_encode_plutus_data_bytes() {
        let data = PlutusData::Bytes(vec![0xde, 0xad]);
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded, vec![0x42, 0xde, 0xad]);
    }

    #[test]
    fn test_encode_plutus_data_list() {
        let data = PlutusData::List(vec![
            PlutusData::Integer(num_bigint::BigInt::from(1i64)),
            PlutusData::Integer(num_bigint::BigInt::from(2i64)),
        ]);
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded, vec![0x82, 0x01, 0x02]);
    }

    #[test]
    fn test_encode_plutus_data_constr() {
        let data = PlutusData::Constr(0, vec![PlutusData::Integer(num_bigint::BigInt::from(1i64))]);
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded[0], 0xd8); // tag
        assert_eq!(encoded[1], 121); // constructor 0 = tag 121
        assert_eq!(encoded[2], 0x81); // array of 1
        assert_eq!(encoded[3], 0x01); // integer 1
    }

    #[test]
    fn test_encode_tx_input() {
        let input = TransactionInput {
            transaction_id: Hash32::ZERO,
            index: 0,
        };
        let encoded = encode_tx_input(&input);
        assert_eq!(encoded[0], 0x82); // array of 2
    }

    #[test]
    fn test_encode_metadatum_text() {
        let meta = TransactionMetadatum::Text("hello".to_string());
        let encoded = encode_metadatum(&meta);
        assert_eq!(encoded[0], 0x65);
        assert_eq!(&encoded[1..], b"hello");
    }

    #[test]
    fn test_encode_metadatum_int() {
        let meta = TransactionMetadatum::Int(42);
        let encoded = encode_metadatum(&meta);
        assert_eq!(encoded, vec![0x18, 42]);
    }

    #[test]
    fn test_encode_metadatum_map() {
        let meta = TransactionMetadatum::Map(vec![(
            TransactionMetadatum::Text("key".to_string()),
            TransactionMetadatum::Int(1),
        )]);
        let encoded = encode_metadatum(&meta);
        assert_eq!(encoded[0], 0xa1); // map of 1
    }

    #[test]
    fn test_encode_plutus_constr_small() {
        // Constructors 0-6 use CBOR tags 121-127
        for tag in 0..7u64 {
            let data = PlutusData::Constr(tag, vec![]);
            let encoded = encode_plutus_data(&data);
            assert_eq!(encoded[0], 0xd8);
            assert_eq!(encoded[1], (121 + tag) as u8);
            assert_eq!(encoded[2], 0x80); // empty array
        }
    }

    #[test]
    fn test_encode_plutus_constr_medium() {
        // Constructors 7-127 use CBOR tags 1280+
        let data = PlutusData::Constr(7, vec![PlutusData::Integer(num_bigint::BigInt::from(1i64))]);
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded[0], 0xd9); // 2-byte tag
        let tag_val = u16::from_be_bytes([encoded[1], encoded[2]]);
        assert_eq!(tag_val, 1280); // 1280 + (7 - 7) = 1280
    }

    #[test]
    fn test_encode_plutus_constr_large_uses_tag_102() {
        // Constructors >= 128 must use CBOR tag 102 (NOT tag 258)
        let data = PlutusData::Constr(
            128,
            vec![PlutusData::Integer(num_bigint::BigInt::from(99i64))],
        );
        let encoded = encode_plutus_data(&data);

        // Tag 102 = 0xd8 0x66
        assert_eq!(encoded[0], 0xd8, "should use 1-byte CBOR tag prefix");
        assert_eq!(encoded[1], 0x66, "should use tag 102 (0x66)");

        // After tag: array(2) with [constructor_index, fields_array]
        assert_eq!(encoded[2], 0x82); // array of 2

        // Constructor index 128 = 0x18 0x80
        assert_eq!(encoded[3], 0x18);
        assert_eq!(encoded[4], 128);

        // Fields array(1) with integer 99
        assert_eq!(encoded[5], 0x81);
        assert_eq!(encoded[6], 0x18);
        assert_eq!(encoded[7], 99);
    }

    #[test]
    fn test_encode_plutus_constr_256_tag_102() {
        let data = PlutusData::Constr(256, vec![]);
        let encoded = encode_plutus_data(&data);
        assert_eq!(encoded[0], 0xd8);
        assert_eq!(encoded[1], 0x66); // tag 102
        assert_eq!(encoded[2], 0x82); // array of 2
                                      // Constructor 256 = 0x19 0x01 0x00
        assert_eq!(encoded[3], 0x19);
        assert_eq!(encoded[4], 0x01);
        assert_eq!(encoded[5], 0x00);
        assert_eq!(encoded[6], 0x80); // empty fields array
    }

    // ── #28b: PlutusData ENCODER must chunk >64-byte leaf bytestrings ──
    //
    // Mirrors Haskell `plutus` PlutusCore.Data.encodeData `encodeBs`:
    //   * len <= 64 -> single definite bstr (CBOR.encodeBytes)
    //   * len  > 64 -> indefinite bstr 0x5f ... 0xff, payload = to64ByteChunks
    //     (greedy 64-byte definite chunks, final chunk 1..=64, a 128 -> two 64).
    // The same rule covers the tag-2/tag-3 bignum mantissa.
    // The generic encode_bytes must STAY a single definite bstr for any size.

    use crate::decode::era_alonzo::read_plutus_data as read_plutus_data_alonzo;
    use crate::decode::era_conway::read_plutus_data as read_plutus_data_conway;
    use crate::decode::reader::Reader;
    use num_bigint::BigInt;

    const PLUTUS_LEAF_MAX: usize = Reader::PLUTUS_DATA_BYTES_LEAF_MAX;

    /// (a) chunk-shape: a 100-byte leaf -> 0x5f, 0x58 0x40 <64>, 0x58 0x24 <36>, 0xff.
    #[test]
    fn plutus_bytes_100b_leaf_chunks_64_then_36() {
        let payload = vec![0xABu8; 100];
        let encoded = encode_plutus_data(&PlutusData::Bytes(payload));

        // 0x5f indefinite-length byte-string header.
        assert_eq!(encoded[0], 0x5f);
        // First chunk: definite bstr, 1-byte length 0x40 (=64), 64 payload bytes.
        assert_eq!(encoded[1], 0x58);
        assert_eq!(encoded[2], 0x40);
        assert_eq!(&encoded[3..67], &[0xABu8; 64][..]);
        // Second chunk: definite bstr, 1-byte length 0x24 (=36), 36 payload bytes.
        assert_eq!(encoded[67], 0x58);
        assert_eq!(encoded[68], 0x24);
        assert_eq!(&encoded[69..105], &[0xABu8; 36][..]);
        // 0xff break.
        assert_eq!(encoded[105], 0xff);
        assert_eq!(encoded.len(), 106);
    }

    /// (b) length-lattice: <=64 single definite (no 0x5f); >64 indefinite with
    /// every interior chunk == 64 and final chunk == len-64*floor(...) (64 when
    /// len%64==0).
    #[test]
    fn plutus_bytes_length_lattice_chunk_shape() {
        for &len in &[0usize, 1, 63, 64, 65, 100, 128, 200] {
            let payload = vec![0x5Au8; len];
            let encoded = encode_plutus_data(&PlutusData::Bytes(payload.clone()));

            if len <= PLUTUS_LEAF_MAX {
                // Single definite byte string — NO indefinite marker / break.
                assert_ne!(encoded[0], 0x5f, "len {len} must NOT be indefinite");
                assert_eq!(
                    encoded,
                    encode_bytes(&payload),
                    "len {len} must be a single definite bstr"
                );
            } else {
                // Indefinite: 0x5f ... 0xff.
                assert_eq!(encoded[0], 0x5f, "len {len} must be indefinite");
                assert_eq!(
                    *encoded.last().unwrap(),
                    0xff,
                    "len {len} must end in break"
                );

                // Walk the interior chunks: every chunk except the last must be
                // exactly 64 bytes; the final chunk = len % 64 (or 64).
                let n_chunks = len.div_ceil(PLUTUS_LEAF_MAX);
                let expected_last = if len % PLUTUS_LEAF_MAX == 0 {
                    PLUTUS_LEAF_MAX
                } else {
                    len % PLUTUS_LEAF_MAX
                };
                let mut chunk_lens = Vec::new();
                let mut i = 1usize; // skip 0x5f
                while encoded[i] != 0xff {
                    // Each chunk is a definite bstr; lengths here are <=64 so
                    // either 0x40|len (len<24) or 0x58 <len>.
                    let clen = match encoded[i] {
                        b if (0x40..0x58).contains(&b) => {
                            let l = (b & 0x1f) as usize;
                            i += 1 + l;
                            l
                        }
                        0x58 => {
                            let l = encoded[i + 1] as usize;
                            i += 2 + l;
                            l
                        }
                        other => panic!("len {len}: unexpected chunk header {other:#04x}"),
                    };
                    chunk_lens.push(clen);
                }
                assert_eq!(chunk_lens.len(), n_chunks, "len {len} chunk count");
                for (idx, &cl) in chunk_lens.iter().enumerate() {
                    if idx + 1 < n_chunks {
                        assert_eq!(cl, PLUTUS_LEAF_MAX, "len {len} interior chunk {idx}");
                    } else {
                        assert_eq!(cl, expected_last, "len {len} final chunk");
                    }
                }
                // 128 specifically -> exactly two 64-byte chunks.
                if len == 128 {
                    assert_eq!(chunk_lens, vec![64, 64]);
                }
            }
        }
    }

    /// (c) ROUND-TRIP closure — the self-inconsistency fix. For >64 leaves the
    /// OLD single-definite encoding FAILED dugite's own #28 decode bound; the
    /// chunked encoding now decodes via read_bounded_plutus_bytes AND the full
    /// read_plutus_data (Conway + Alonzo).
    #[test]
    fn plutus_bytes_roundtrip_via_bounded_decoder() {
        for &len in &[0usize, 1, 63, 64, 65, 100, 128, 200] {
            // Distinct byte pattern so a corrupted round-trip is caught.
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let encoded = encode_plutus_data(&PlutusData::Bytes(payload.clone()));

            // Low-level bounded leaf reader.
            let mut r = Reader::new(&encoded);
            let decoded = r
                .read_bounded_plutus_bytes()
                .unwrap_or_else(|e| panic!("len {len}: read_bounded_plutus_bytes failed: {e}"));
            assert_eq!(decoded, payload, "len {len}: bounded leaf round-trip");

            // Full PlutusData decoders (Conway + Alonzo).
            let mut rc = Reader::new(&encoded);
            assert_eq!(
                read_plutus_data_conway(&mut rc).unwrap(),
                PlutusData::Bytes(payload.clone()),
                "len {len}: Conway read_plutus_data round-trip"
            );
            let mut ra = Reader::new(&encoded);
            assert_eq!(
                read_plutus_data_alonzo(&mut ra).unwrap(),
                PlutusData::Bytes(payload.clone()),
                "len {len}: Alonzo read_plutus_data round-trip"
            );
        }
    }

    /// (c-bignum) symmetric case: a BigInt whose big-endian magnitude exceeds
    /// 64 bytes (tag-2 path) must round-trip encode_plutus_int ->
    /// read_bounded_plutus_bigint, AND through the full PlutusData decoders.
    #[test]
    fn plutus_bignum_magnitude_over_64_bytes_roundtrips() {
        // 2^520 has a 66-byte big-endian magnitude -> tag 2 + chunked mantissa.
        let big_pos = BigInt::from(2u8).pow(520);
        let enc_pos = encode_plutus_int(&big_pos);
        // tag 2 (positive bignum) header, then chunked indefinite mantissa.
        assert_eq!(enc_pos[0], 0xc2, "positive bignum tag");
        assert_eq!(enc_pos[1], 0x5f, "mantissa must be chunked (indefinite)");

        let mut r = Reader::new(&enc_pos);
        assert_eq!(
            r.read_bounded_plutus_bigint().unwrap(),
            big_pos,
            "positive bignum magnitude>64 round-trip"
        );
        // Full PlutusData decode (Integer leaf).
        let mut rc = Reader::new(&enc_pos);
        assert_eq!(
            read_plutus_data_conway(&mut rc).unwrap(),
            PlutusData::Integer(big_pos.clone())
        );

        // Negative bignum (tag 3): value = -1 - magnitude.
        let big_neg = -BigInt::from(2u8).pow(600);
        let enc_neg = encode_plutus_int(&big_neg);
        assert_eq!(enc_neg[0], 0xc3, "negative bignum tag");
        assert_eq!(enc_neg[1], 0x5f, "mantissa must be chunked (indefinite)");
        let mut rn = Reader::new(&enc_neg);
        assert_eq!(
            rn.read_bounded_plutus_bigint().unwrap(),
            big_neg,
            "negative bignum magnitude>64 round-trip"
        );
    }

    /// (d) generic-encoder guard: encode_bytes (non-plutus) STAYS a single
    /// definite byte string for >64 bytes — NO indefinite marker. Chunking it
    /// would corrupt addresses / hashes / script blobs and break body hashes.
    #[test]
    fn generic_encode_bytes_stays_single_definite() {
        let encoded = encode_bytes(&[0u8; 100]);
        assert_eq!(
            encoded[0], 0x58,
            "100-byte generic bstr: 1-byte length form"
        );
        assert_eq!(encoded[1], 0x64, "length 100 == 0x64");
        assert_eq!(encoded.len(), 102);
        assert!(
            !encoded.contains(&0x5f),
            "generic encode_bytes must never chunk"
        );

        // And a deliberately large (>64) blob — e.g. a Plutus script blob class.
        let big = encode_bytes(&[0xCDu8; 300]);
        assert_eq!(big[0], 0x59, "300-byte generic bstr: 2-byte length form");
        assert_eq!(u16::from_be_bytes([big[1], big[2]]), 300);
    }

    proptest::proptest! {
        /// (b/c property) length-lattice over arbitrary sizes 0..=512: the
        /// encoding always round-trips through the bounded decoder, <=64 stays a
        /// single definite bstr, and >64 is a chunked indefinite bstr whose
        /// interior chunks are all exactly 64.
        #[test]
        fn prop_plutus_bytes_chunk_shape_and_roundtrip(len in 0usize..=512) {
            let payload: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let encoded = encode_plutus_data(&PlutusData::Bytes(payload.clone()));

            // Round-trip via the bounded leaf reader.
            let mut r = Reader::new(&encoded);
            let decoded = r.read_bounded_plutus_bytes().unwrap();
            proptest::prop_assert_eq!(&decoded, &payload);

            if len <= PLUTUS_LEAF_MAX {
                proptest::prop_assert_ne!(encoded[0], 0x5f);
                proptest::prop_assert_eq!(&encoded, &encode_bytes(&payload));
            } else {
                proptest::prop_assert_eq!(encoded[0], 0x5f);
                proptest::prop_assert_eq!(*encoded.last().unwrap(), 0xff);
            }
        }
    }
}
