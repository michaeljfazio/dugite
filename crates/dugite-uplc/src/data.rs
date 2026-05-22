//! The `Data` type — PlutusData.
//!
//! `Data` is the recursive sum type that the Cardano on-chain script
//! context (TxInfo) and per-redeemer datums/redeemers are encoded into.
//!
//! Encoding rules (mirroring the Haskell reference at
//! `IntersectMBO/plutus:plutus-core/.../PlutusCore/Data.hs`):
//!
//!  - `Constr i ds` where `0 ≤ i ≤ 6`  → CBOR tag `121 + i`, payload =
//!    CBOR array of the encoded `ds`.
//!  - `Constr i ds` where `7 ≤ i ≤ 127` → CBOR tag `1280 + (i - 7)`,
//!    payload = CBOR array of the encoded `ds`.
//!  - `Constr i ds` otherwise → CBOR tag `102`, payload = 2-element
//!    array `[i_as_u64, ds_as_array]`.
//!  - `Map es` → definite-length CBOR map (Haskell encoder always uses
//!    definite-length maps for Data).
//!  - `List ds` → definite-length CBOR array if `len ≤ 64`, otherwise
//!    indefinite-length. The decoder accepts either form.
//!  - `I i` → CBOR major-0/major-1 for `|i| < 2^64`; otherwise CBOR
//!    tag 2 (positive bignum) or tag 3 (negative bignum) wrapping a
//!    byte string ≤ 64 bytes.
//!  - `B bs` → definite-length CBOR byte string if `len ≤ 64`,
//!    otherwise indefinite-length with 64-byte chunks. The decoder
//!    accepts either form.
//!
//! **The encoder is not canonical.** Map entry order is preserved
//! as-is; duplicate keys are permitted. RFC 8949 §4.2 canonical form
//! is NOT enforced by the decoder.
//!
//! ## Defensive bounds
//!
//! The decoder threads an explicit depth counter (capped at
//! [`DATA_MAX_DEPTH`]) and rejects:
//!
//! - Definite-length `B` chunks longer than 64 bytes (Haskell
//!   `decodeBoundedBytes` invariant).
//! - Definite-length bignum payloads longer than 64 bytes.
//! - Pre-allocations on peer-controlled length headers exceeding the
//!   remaining buffer size (the same `safe_alloc_capacity` pattern as
//!   `dugite-serialization::decode::reader`).
//! - Recursion past [`DATA_MAX_DEPTH`].

use crate::UplcError;
use minicbor::data::{Tag, Type};
use minicbor::{Decoder, Encoder};
use num_bigint::{BigInt, Sign};

/// Maximum nesting depth accepted by the [`Data`] CBOR decoder. The
/// real on-chain limit is much lower than this; the cap is purely a
/// DoS guard against pathological adversarial input.
pub const DATA_MAX_DEPTH: usize = 256;

/// Maximum payload length of a single definite-length byte / bignum
/// chunk, per the Haskell `decodeBoundedBytes` invariant.
const DATA_CHUNK_LIMIT: usize = 64;

/// Recursive PlutusData value. Maps 1:1 onto the Haskell
/// `Plutus.V1.Data` definition.
///
/// `Constr` and `Map` payloads are `Vec` rather than `BTreeMap` because
/// the on-chain encoding preserves insertion order — sorting on encode
/// would change the body hash for txs that gossip in non-sorted form,
/// breaking byte-exact round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Data {
    /// `Constr` — tagged sum: `Constr tag args`.
    Constr(u64, Vec<Data>),
    /// `Map` — list of key-value pairs. Insertion order preserved;
    /// duplicates permitted.
    Map(Vec<(Data, Data)>),
    /// `List` — list of values.
    List(Vec<Data>),
    /// `I` — arbitrary-precision integer.
    I(BigInt),
    /// `B` — byte string.
    B(Vec<u8>),
}

impl Data {
    /// Encode the `Data` value to its canonical-on-chain CBOR bytes.
    pub fn to_cbor(&self) -> Result<Vec<u8>, UplcError> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out);
        encode_data(&mut e, self).map_err(|err| UplcError::Encode(err.to_string()))?;
        Ok(out)
    }

    /// Decode a `Data` value from CBOR bytes.
    ///
    /// Defensive invariants:
    ///
    ///  - Recursion depth is capped at [`DATA_MAX_DEPTH`].
    ///  - Definite-length byte / bignum chunks ≤ 64 bytes.
    ///  - `Vec::with_capacity` clamped to `min(declared, remaining_bytes)`.
    ///  - Trailing bytes after the outer value cause a `CborDecode` error.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, UplcError> {
        let mut d = Decoder::new(bytes);
        let data = decode_data(&mut d, 0)?;
        if d.position() != bytes.len() {
            return Err(UplcError::CborDecode(format!(
                "trailing {} bytes after Data value",
                bytes.len() - d.position()
            )));
        }
        Ok(data)
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

fn encode_data<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    data: &Data,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    match data {
        Data::Constr(tag, args) => encode_constr(e, *tag, args),
        Data::Map(entries) => {
            // Haskell's encoder always uses definite-length CBOR maps
            // for Data. The decoder accepts indefinite, but byte-exact
            // round-trip requires definite on the encode path.
            e.map(entries.len() as u64)?;
            for (k, v) in entries {
                encode_data(e, k)?;
                encode_data(e, v)?;
            }
            Ok(())
        }
        Data::List(items) => encode_list(e, items),
        Data::I(n) => encode_integer(e, n),
        Data::B(bs) => encode_bytes(e, bs),
    }
}

fn encode_constr<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    tag: u64,
    args: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    if tag <= 6 {
        e.tag(Tag::new(121 + tag))?;
        encode_args(e, args)?;
    } else if tag <= 127 {
        e.tag(Tag::new(1280 + (tag - 7)))?;
        encode_args(e, args)?;
    } else {
        e.tag(Tag::new(102))?;
        e.array(2)?;
        e.u64(tag)?;
        encode_args(e, args)?;
    }
    Ok(())
}

fn encode_args<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    args: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // `Constr i ds` is encoded in Haskell as
    //   `encodeTag t <> encode ds`
    // where `encode ds` reuses the `Serialise [Data]` instance — see
    // `encode_list` for the exact empty / non-empty split.
    encode_list(e, args)
}

/// Encode a list of `Data` using the `Serialise [a]` rule from `cborg`:
///
/// ```text
/// encodeList [] = encodeListLen 0
/// encodeList xs = encodeListLenIndef <> foldr (\x r -> encode x <> r) encodeBreak xs
/// ```
///
/// i.e. an empty list is `0x80` (definite length zero); a non-empty
/// list is `0x9f <items> 0xff` (indefinite length). This empty / non-
/// empty asymmetry is what cborg's `Codec.Serialise.Class.encodeList`
/// produces, and reproducing it byte-for-byte is required for wire
/// compatibility with cardano-node's PlutusData hashes.
fn encode_list<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    items: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    if items.is_empty() {
        e.array(0)?;
    } else {
        e.begin_array()?;
        for item in items {
            encode_data(e, item)?;
        }
        e.end()?;
    }
    Ok(())
}

fn encode_integer<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    n: &BigInt,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // CBOR major-0/major-1 hold values in `[0, 2^64)` (major 0) and
    // `[-2^64, -1]` (major 1, encoding `-1 - k`). Anything outside
    // that range uses the bignum tag form.
    //
    // Strategy:
    //   - non-negative `n ≤ u64::MAX` → `e.u64(n)`.
    //   - negative `n ≥ -2^63` (i.e. fits in `i64`) → `e.i64(n)`.
    //   - negative `n` in `[-2^64, -2^63 - 1)` → hand-encode major-1
    //     with the internal `k = -1 - n` value (which fits in `u64`).
    //   - everything else → bignum tag (`encode_bignum`).
    match n.sign() {
        Sign::NoSign => {
            e.u64(0)?;
        }
        Sign::Plus => {
            if n.iter_u64_digits().count() == 1 {
                if let Some(v) = n.iter_u64_digits().next() {
                    e.u64(v)?;
                    return Ok(());
                }
            }
            return encode_bignum(e, n);
        }
        Sign::Minus => {
            // `mag` is `-n`, always `> 0`.
            let mag: BigInt = -n;
            // Internal CBOR negint value `k = mag - 1`. Fits in `u64`
            // iff `mag <= 2^64`.
            let k: BigInt = &mag - 1;
            if k.iter_u64_digits().count() <= 1 {
                let k_u64 = k.iter_u64_digits().next().unwrap_or(0);
                if k_u64 <= i64::MAX as u64 {
                    // Safe to go through `i64` — the represented value
                    // `-1 - k_u64` lies in `[i64::MIN + 1, -1]`, which
                    // is representable.
                    e.i64(-1 - (k_u64 as i64))?;
                } else {
                    // `k_u64` is in `(i64::MAX, u64::MAX]`. We hand-write
                    // the major-1 CBOR encoding so we can carry the
                    // full u64 range. Layout: 0x3b (major 1, ai = 27)
                    // followed by the 8-byte big-endian internal value.
                    encode_raw_negint_u64(e, k_u64)?;
                }
                return Ok(());
            }
            return encode_bignum(e, n);
        }
    }
    Ok(())
}

fn encode_raw_negint_u64<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    k: u64,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // 0x20 = major 1, 0x1b = additional info 27 (8-byte length).
    // The `Encoder::writer_mut()` accessor exposes the underlying
    // sink so we can write the negint header + payload directly,
    // sidestepping the lack of a `u64`-range negint method on the
    // public Encoder API.
    let mut header = [0u8; 9];
    header[0] = 0x3b;
    header[1..].copy_from_slice(&k.to_be_bytes());
    e.writer_mut()
        .write_all(&header)
        .map_err(minicbor::encode::Error::write)?;
    Ok(())
}

fn encode_bignum<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    n: &BigInt,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    let (sign, magnitude): (Tag, BigInt) = if n.sign() == Sign::Minus {
        // CBOR tag 3 encodes `-1 - n`, so the payload bytes are the
        // big-endian bytes of `-1 - n`.
        let adjusted: BigInt = -BigInt::from(1) - n;
        (Tag::new(3), adjusted)
    } else {
        (Tag::new(2), n.clone())
    };
    let mut bytes = magnitude.to_bytes_be().1;
    if bytes.is_empty() {
        bytes.push(0);
    }
    e.tag(sign)?;
    encode_bytes_raw(e, &bytes)?;
    Ok(())
}

fn encode_bytes<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    bs: &[u8],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    encode_bytes_raw(e, bs)
}

fn encode_bytes_raw<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    bs: &[u8],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    if bs.len() <= DATA_CHUNK_LIMIT {
        e.bytes(bs)?;
    } else {
        e.begin_bytes()?;
        for chunk in bs.chunks(DATA_CHUNK_LIMIT) {
            e.bytes(chunk)?;
        }
        e.end()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

fn decode_data(d: &mut Decoder<'_>, depth: usize) -> Result<Data, UplcError> {
    if depth > DATA_MAX_DEPTH {
        return Err(UplcError::CborDecode(format!(
            "Data nesting depth exceeds limit ({DATA_MAX_DEPTH})"
        )));
    }
    let ty = d
        .datatype()
        .map_err(|e| UplcError::CborDecode(format!("datatype: {e}")))?;
    match ty {
        Type::Tag => decode_tagged(d, depth),
        Type::Array | Type::ArrayIndef => Ok(Data::List(decode_array(d, depth)?)),
        Type::Map | Type::MapIndef => Ok(Data::Map(decode_map(d, depth)?)),
        Type::Bytes | Type::BytesIndef => Ok(Data::B(decode_bytes(d)?)),
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let v = d
                .u64()
                .map_err(|e| UplcError::CborDecode(format!("uint: {e}")))?;
            Ok(Data::I(BigInt::from(v)))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
            let v = d
                .i64()
                .map_err(|e| UplcError::CborDecode(format!("int: {e}")))?;
            Ok(Data::I(BigInt::from(v)))
        }
        Type::Int => {
            // Wide negative outside i64 range — read as `Int` and
            // promote to BigInt.
            let v = d
                .int()
                .map_err(|e| UplcError::CborDecode(format!("int: {e}")))?;
            // minicbor's `Int` is a signed-CBOR-int wrapper; convert
            // via its display, then parse — narrow and slow but works
            // for the rare case.
            let s = v.to_string();
            let parsed = s
                .parse::<BigInt>()
                .map_err(|e| UplcError::CborDecode(format!("bigint parse: {e}")))?;
            Ok(Data::I(parsed))
        }
        other => Err(UplcError::CborDecode(format!(
            "unexpected CBOR major for Data: {other:?}"
        ))),
    }
}

fn decode_tagged(d: &mut Decoder<'_>, depth: usize) -> Result<Data, UplcError> {
    let tag = d
        .tag()
        .map_err(|e| UplcError::CborDecode(format!("tag: {e}")))?
        .as_u64();
    match tag {
        2 => {
            // Positive bignum.
            let bytes = decode_bytes(d)?;
            Ok(Data::I(BigInt::from_bytes_be(Sign::Plus, &bytes)))
        }
        3 => {
            // Negative bignum: stored as `-1 - n`, so the result is
            // `-1 - magnitude`.
            let bytes = decode_bytes(d)?;
            let magnitude = BigInt::from_bytes_be(Sign::Plus, &bytes);
            Ok(Data::I(-BigInt::from(1) - magnitude))
        }
        tag @ 121..=127 => {
            let constr_tag = tag - 121;
            let args = decode_array(d, depth)?;
            Ok(Data::Constr(constr_tag, args))
        }
        tag @ 1280..=1400 => {
            let constr_tag = tag - 1280 + 7;
            let args = decode_array(d, depth)?;
            Ok(Data::Constr(constr_tag, args))
        }
        102 => {
            let len = d
                .array()
                .map_err(|e| UplcError::CborDecode(format!("constr-102 outer: {e}")))?;
            if len != Some(2) {
                return Err(UplcError::CborDecode(format!(
                    "tag 102: expected definite array(2), got {len:?}"
                )));
            }
            let constr_tag = d
                .u64()
                .map_err(|e| UplcError::CborDecode(format!("constr-102 tag: {e}")))?;
            let args = decode_array(d, depth)?;
            Ok(Data::Constr(constr_tag, args))
        }
        other => Err(UplcError::CborDecode(format!(
            "unsupported CBOR tag for Data: {other}"
        ))),
    }
}

fn decode_array(d: &mut Decoder<'_>, depth: usize) -> Result<Vec<Data>, UplcError> {
    let header = d
        .array()
        .map_err(|e| UplcError::CborDecode(format!("array: {e}")))?;
    match header {
        Some(n) => {
            let cap = safe_alloc_capacity(n, d.input(), d.position());
            let mut out = Vec::with_capacity(cap);
            for _ in 0..n {
                out.push(decode_data(d, depth + 1)?);
            }
            Ok(out)
        }
        None => {
            let mut out = Vec::new();
            loop {
                match d
                    .datatype()
                    .map_err(|e| UplcError::CborDecode(format!("array-indef peek: {e}")))?
                {
                    Type::Break => {
                        d.skip().map_err(|e| {
                            UplcError::CborDecode(format!("array-indef break: {e}"))
                        })?;
                        break;
                    }
                    _ => out.push(decode_data(d, depth + 1)?),
                }
            }
            Ok(out)
        }
    }
}

fn decode_map(d: &mut Decoder<'_>, depth: usize) -> Result<Vec<(Data, Data)>, UplcError> {
    let header = d
        .map()
        .map_err(|e| UplcError::CborDecode(format!("map: {e}")))?;
    match header {
        Some(n) => {
            let cap = safe_alloc_capacity(n, d.input(), d.position());
            let mut out = Vec::with_capacity(cap);
            for _ in 0..n {
                let k = decode_data(d, depth + 1)?;
                let v = decode_data(d, depth + 1)?;
                out.push((k, v));
            }
            Ok(out)
        }
        None => {
            let mut out = Vec::new();
            loop {
                match d
                    .datatype()
                    .map_err(|e| UplcError::CborDecode(format!("map-indef peek: {e}")))?
                {
                    Type::Break => {
                        d.skip()
                            .map_err(|e| UplcError::CborDecode(format!("map-indef break: {e}")))?;
                        break;
                    }
                    _ => {
                        let k = decode_data(d, depth + 1)?;
                        let v = decode_data(d, depth + 1)?;
                        out.push((k, v));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn decode_bytes(d: &mut Decoder<'_>) -> Result<Vec<u8>, UplcError> {
    let ty = d
        .datatype()
        .map_err(|e| UplcError::CborDecode(format!("bytes peek: {e}")))?;
    match ty {
        Type::Bytes => {
            let slice = d
                .bytes()
                .map_err(|e| UplcError::CborDecode(format!("bytes: {e}")))?;
            if slice.len() > DATA_CHUNK_LIMIT {
                return Err(UplcError::CborDecode(format!(
                    "definite-length byte string of {} bytes exceeds chunk limit ({DATA_CHUNK_LIMIT})",
                    slice.len()
                )));
            }
            Ok(slice.to_vec())
        }
        Type::BytesIndef => {
            // Consume the 0x5f header.
            let iter = d
                .bytes_iter()
                .map_err(|e| UplcError::CborDecode(format!("bytes-indef: {e}")))?;
            let mut out = Vec::new();
            for chunk in iter {
                let chunk =
                    chunk.map_err(|e| UplcError::CborDecode(format!("bytes-indef chunk: {e}")))?;
                if chunk.len() > DATA_CHUNK_LIMIT {
                    return Err(UplcError::CborDecode(format!(
                        "indefinite-length chunk of {} bytes exceeds chunk limit ({DATA_CHUNK_LIMIT})",
                        chunk.len()
                    )));
                }
                out.extend_from_slice(chunk);
            }
            Ok(out)
        }
        other => Err(UplcError::CborDecode(format!(
            "expected byte string, got {other:?}"
        ))),
    }
}

/// Cap an initial `Vec::with_capacity` value derived from a CBOR
/// length header so a forged length can't drive a multi-exabyte
/// allocation. Mirrors `dugite-serialization`'s `safe_alloc_capacity`.
fn safe_alloc_capacity(declared: u64, input: &[u8], pos: usize) -> usize {
    let remaining = input.len().saturating_sub(pos);
    usize::try_from(declared)
        .unwrap_or(usize::MAX)
        .min(remaining)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(d: &Data) -> Data {
        let bytes = d.to_cbor().expect("encode");
        Data::from_cbor(&bytes).expect("decode")
    }

    #[test]
    fn roundtrip_small_int() {
        for n in [-1000i64, -1, 0, 1, 100, 1_000_000] {
            let d = Data::I(BigInt::from(n));
            assert_eq!(rt(&d), d, "n={n}");
        }
    }

    #[test]
    fn roundtrip_bignum_positive() {
        // 2^65 — needs the positive bignum tag.
        let n = BigInt::from(1u64) << 65;
        let d = Data::I(n);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_bignum_negative() {
        // -(2^65)
        let pos: BigInt = BigInt::from(1u64) << 65;
        let n = -pos;
        let d = Data::I(n);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_empty_bytes() {
        let d = Data::B(vec![]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_small_bytes() {
        let d = Data::B((0..64u8).collect());
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_large_bytes_indefinite() {
        // 200-byte string — encoded indefinite with chunks of 64.
        let d = Data::B((0..200u8).collect());
        let bytes = d.to_cbor().unwrap();
        // First byte must be 0x5f (indefinite-length byte string).
        assert_eq!(bytes[0], 0x5f, "expected indefinite-length byte string");
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn rejects_definite_bytes_over_64() {
        // Construct a definite-length CBOR byte string of 65 bytes
        // by hand: 0x58 = bytes(uint8), 0x41 = 65, then 65 0x00 bytes.
        let mut bytes = vec![0x58, 0x41];
        bytes.extend(std::iter::repeat_n(0u8, 65));
        let err = Data::from_cbor(&bytes).expect_err("must reject");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn roundtrip_constr_small_tag() {
        for tag in [0u64, 3, 6] {
            let d = Data::Constr(tag, vec![Data::I(BigInt::from(42))]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    #[test]
    fn roundtrip_constr_medium_tag() {
        for tag in [7u64, 50, 127] {
            let d = Data::Constr(tag, vec![Data::I(BigInt::from(42))]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    #[test]
    fn roundtrip_constr_large_tag() {
        // 128 and above use tag 102 wrapping [tag, args].
        for tag in [128u64, 1000, u64::MAX] {
            let d = Data::Constr(tag, vec![]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    #[test]
    fn roundtrip_list() {
        let d = Data::List(vec![
            Data::I(BigInt::from(1)),
            Data::I(BigInt::from(2)),
            Data::B(vec![0xde, 0xad]),
        ]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_large_list_indefinite() {
        let items: Vec<Data> = (0..100).map(|i| Data::I(BigInt::from(i))).collect();
        let d = Data::List(items);
        let bytes = d.to_cbor().unwrap();
        // First byte must be 0x9f (indefinite-length array).
        assert_eq!(bytes[0], 0x9f, "expected indefinite-length array");
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_map() {
        let d = Data::Map(vec![
            (Data::B(vec![0x01]), Data::I(BigInt::from(10))),
            (Data::B(vec![0x02]), Data::I(BigInt::from(20))),
        ]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_map_preserves_duplicates() {
        // Critical: the encoder must not deduplicate.
        let d = Data::Map(vec![
            (Data::B(vec![0x01]), Data::I(BigInt::from(10))),
            (Data::B(vec![0x01]), Data::I(BigInt::from(20))),
        ]);
        let decoded = rt(&d);
        if let Data::Map(es) = decoded {
            assert_eq!(es.len(), 2);
        } else {
            panic!("expected Map");
        }
    }

    #[test]
    fn roundtrip_nested() {
        let d = Data::Constr(
            0,
            vec![
                Data::Map(vec![(
                    Data::B(b"key".to_vec()),
                    Data::List(vec![Data::I(BigInt::from(7))]),
                )]),
                Data::Constr(127, vec![Data::I(BigInt::from(-1))]),
            ],
        );
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn rejects_overlydeep_data() {
        // Hand-craft 300 nested CBOR-tag-121 (Constr 0) wrappers each
        // wrapping an empty array, terminating in an empty array.
        // Goes well past DATA_MAX_DEPTH.
        let mut bytes = Vec::new();
        for _ in 0..300 {
            bytes.push(0xd8); // tag, 1-byte argument
            bytes.push(121); // tag 121
            bytes.push(0x81); // array(1)
        }
        bytes.push(0x80); // innermost empty array
        let err = Data::from_cbor(&bytes).expect_err("must reject");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn rejects_trailing_bytes() {
        let d = Data::I(BigInt::from(1));
        let mut bytes = d.to_cbor().unwrap();
        bytes.push(0x00);
        let err = Data::from_cbor(&bytes).expect_err("must reject trailing");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn empty_input_is_error() {
        let err = Data::from_cbor(&[]).expect_err("must reject empty");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }
}
