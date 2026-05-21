//! Low-level CBOR decoding utilities for Haskell ledger state snapshots.
//!
//! These functions consume CBOR bytes and return `(decoded_value, bytes_consumed)`.
//! They follow the existing pattern in `crate::cbor` but add array/map/rational
//! decoders needed for the Haskell ExtLedgerState format.

use crate::error::SerializationError;
use dugite_primitives::hash::{Hash28, Hash32};

/// Decode a CBOR unsigned integer (major type 0).
pub fn decode_uint(data: &[u8]) -> Result<(u64, usize), SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode("empty input".into()));
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 0 {
        return Err(SerializationError::CborDecode(format!(
            "expected uint (major 0), got major {major} at byte {:#04x}",
            data[0]
        )));
    }
    decode_uint_info(data, info)
}

/// Decode a CBOR integer that could be unsigned (major 0) or negative (major 1).
pub fn decode_int(data: &[u8]) -> Result<(i64, usize), SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode("empty input".into()));
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    match major {
        0 => {
            let (v, n) = decode_uint_info(data, info)?;
            Ok((v as i64, n))
        }
        1 => {
            let (v, n) = decode_uint_info(data, info)?;
            Ok((-1 - v as i64, n))
        }
        _ => Err(SerializationError::CborDecode(format!(
            "expected int, got major {major}"
        ))),
    }
}

/// Internal: decode the integer value given the already-checked major type byte and
/// `info` bits. `data` must still point at the initial header byte.
fn decode_uint_info(data: &[u8], info: u8) -> Result<(u64, usize), SerializationError> {
    match info {
        0..=23 => Ok((info as u64, 1)),
        24 => {
            if data.len() < 2 {
                return Err(eof());
            }
            Ok((data[1] as u64, 2))
        }
        25 => {
            if data.len() < 3 {
                return Err(eof());
            }
            Ok((u16::from_be_bytes([data[1], data[2]]) as u64, 3))
        }
        26 => {
            if data.len() < 5 {
                return Err(eof());
            }
            Ok((
                u32::from_be_bytes([data[1], data[2], data[3], data[4]]) as u64,
                5,
            ))
        }
        27 => {
            if data.len() < 9 {
                return Err(eof());
            }
            Ok((
                u64::from_be_bytes([
                    data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
                ]),
                9,
            ))
        }
        _ => Err(SerializationError::CborDecode(format!(
            "invalid additional info {info}"
        ))),
    }
}

/// Decode a CBOR bigint (tag 2 = positive bignum wrapping a bytestring).
/// Falls back to regular uint if no tag is present.
///
/// # D6 / audit #544 — bignum saturation replaced with hard rejection
///
/// The previous implementation silently saturated to `u64::MAX` when the bignum
/// byte string exceeded 8 bytes.  A malformed or adversarial Haskell ledger
/// snapshot could thus inject `u64::MAX` into protocol parameter fields (e.g.
/// `max_tx_ex_steps`) without any diagnostic.  We now reject such values
/// outright — a bignum of 9+ bytes cannot be represented in `u64`.
pub fn decode_bigint_or_uint(data: &[u8]) -> Result<(u64, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    if major == 0 {
        return decode_uint(data);
    }
    // Tag 2 (positive bignum): 0xc2 + bytestring
    if data[0] == 0xc2 {
        let (bytes, n) = decode_bytes(&data[1..])?;
        // u64 holds at most 8 bytes.  Reject bignums that exceed u64 range
        // rather than silently saturating to u64::MAX (D6, audit #544).
        if bytes.len() > 8 {
            return Err(SerializationError::CborDecode(format!(
                "bignum too large for u64: {} bytes (max 8)",
                bytes.len()
            )));
        }
        let mut val = 0u64;
        for &b in bytes {
            val = (val << 8) | b as u64;
        }
        return Ok((val, 1 + n));
    }
    Err(SerializationError::CborDecode(format!(
        "expected uint or bigint, got {:#04x}",
        data[0]
    )))
}

/// Decode a CBOR definite-length array header, returning `(length, bytes_consumed)`.
///
/// Returns an error on indefinite-length arrays (`0x9f`).  Use
/// [`skip_cbor_value`] to skip indefinite arrays without knowing their length.
pub fn decode_array_len(data: &[u8]) -> Result<(usize, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 4 {
        return Err(SerializationError::CborDecode(format!(
            "expected array (major 4), got major {major} at byte {:#04x}",
            data[0]
        )));
    }
    if info == 31 {
        return Err(SerializationError::CborDecode(
            "expected definite-length array, got indefinite (0x9f)".into(),
        ));
    }
    let (len, consumed) = decode_uint_info(data, info)?;
    Ok((len as usize, consumed))
}

/// Decode a CBOR map header, returning `(Some(length), bytes_consumed)` for a
/// definite-length map or `(None, 1)` for an indefinite-length map.
pub fn decode_map_len(data: &[u8]) -> Result<(Option<usize>, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 5 {
        return Err(SerializationError::CborDecode(format!(
            "expected map (major 5), got major {major} at byte {:#04x}",
            data[0]
        )));
    }
    // Indefinite-length map
    if info == 31 {
        return Ok((None, 1));
    }
    let (len, consumed) = decode_uint_info(data, info)?;
    Ok((Some(len as usize), consumed))
}

/// Decode a CBOR byte string, returning `(&[u8], bytes_consumed)`.
pub fn decode_bytes(data: &[u8]) -> Result<(&[u8], usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 2 {
        return Err(SerializationError::CborDecode(format!(
            "expected bytes (major 2), got major {major} at byte {:#04x}",
            data[0]
        )));
    }
    let (len, hdr) = decode_uint_info(data, info)?;
    let len = len as usize;
    if data.len() < hdr + len {
        return Err(eof());
    }
    Ok((&data[hdr..hdr + len], hdr + len))
}

/// Decode a CBOR text string, returning `(&str, bytes_consumed)`.
pub fn decode_text(data: &[u8]) -> Result<(&str, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 3 {
        return Err(SerializationError::CborDecode(format!(
            "expected text (major 3), got major {major}",
        )));
    }
    let (len, hdr) = decode_uint_info(data, info)?;
    let len = len as usize;
    if data.len() < hdr + len {
        return Err(eof());
    }
    let s = std::str::from_utf8(&data[hdr..hdr + len])
        .map_err(|e| SerializationError::CborDecode(format!("invalid utf8: {e}")))?;
    Ok((s, hdr + len))
}

/// Decode a 28-byte CBOR bytestring into a `Hash28`.
pub fn decode_hash28(data: &[u8]) -> Result<(Hash28, usize), SerializationError> {
    let (bytes, n) = decode_bytes(data)?;
    if bytes.len() != 28 {
        return Err(SerializationError::InvalidLength {
            expected: 28,
            got: bytes.len(),
        });
    }
    Ok((Hash28::from_bytes(bytes.try_into().unwrap()), n))
}

/// Decode a 32-byte CBOR bytestring into a `Hash32`.
pub fn decode_hash32(data: &[u8]) -> Result<(Hash32, usize), SerializationError> {
    let (bytes, n) = decode_bytes(data)?;
    if bytes.len() != 32 {
        // Diagnostic: log the surrounding CBOR header so we can see what was
        // actually presented when the decoder hits a 28-byte field where it
        // expected a 32-byte one (issue #504). The header bytes pin the exact
        // CBOR major-type / length encoding the caller passed in.
        let preview_len = data.len().min(16);
        let preview = hex::encode(&data[..preview_len]);
        tracing::warn!(
            got = bytes.len(),
            data_len = data.len(),
            header_hex = %preview,
            "decode_hash32 received non-32 bytestring"
        );
        return Err(SerializationError::InvalidLength {
            expected: 32,
            got: bytes.len(),
        });
    }
    Ok((Hash32::from_bytes(bytes.try_into().unwrap()), n))
}

/// Decode a Haskell `Nonce`:
/// - `[0]` → `NeutralNonce` mapped to the zero `Hash32`
/// - `[1, bytes(32)]` → `Nonce` carrying the 32-byte hash
pub fn decode_nonce(data: &[u8]) -> Result<(Hash32, usize), SerializationError> {
    let (arr_len, mut off) = decode_array_len(data)?;
    let (tag, n) = decode_uint(&data[off..])?;
    off += n;
    match (arr_len, tag) {
        (1, 0) => Ok((Hash32::ZERO, off)),
        (2, 1) => {
            let (hash, n) = decode_hash32(&data[off..])?;
            off += n;
            Ok((hash, off))
        }
        _ => Err(SerializationError::CborDecode(format!(
            "invalid nonce: array({arr_len}), tag {tag}"
        ))),
    }
}

/// Decode a Haskell `Credential`:
/// - `[0, bytes(28)]` → KeyHash
/// - `[1, bytes(28)]` → ScriptHash
///
/// Returns `((tag, hash28), bytes_consumed)`.
pub fn decode_credential(data: &[u8]) -> Result<((u8, Hash28), usize), SerializationError> {
    let (arr_len, mut off) = decode_array_len(data)?;
    if arr_len != 2 {
        return Err(SerializationError::InvalidLength {
            expected: 2,
            got: arr_len,
        });
    }
    let (tag, n) = decode_uint(&data[off..])?;
    off += n;
    let (hash, n) = decode_hash28(&data[off..])?;
    off += n;
    Ok(((tag as u8, hash), off))
}

/// Decode a Haskell `WithOrigin<T>` array header:
/// - `[]` (array of 0) → `None` (Origin)
/// - `[v]` (array of 1) → `Some(1)` — caller must then decode the inner value
///
/// Returns `(inner_element_count, bytes_of_array_header_consumed)`.
pub fn decode_with_origin_len(data: &[u8]) -> Result<(Option<usize>, usize), SerializationError> {
    let (arr_len, off) = decode_array_len(data)?;
    match arr_len {
        0 => Ok((None, off)),
        1 => Ok((Some(1), off)),
        n => Err(SerializationError::CborDecode(format!(
            "WithOrigin: expected array(0) or array(1), got array({n})"
        ))),
    }
}

/// Decode a Haskell `Rational` encoded as either:
/// - CBOR tag 30 followed by `[numerator, denominator]`, or
/// - plain `[numerator, denominator]` (no tag).
///
/// Each integer is decoded with `decode_bigint_or_uint` to handle bignum encoding.
pub fn decode_rational(data: &[u8]) -> Result<((u64, u64), usize), SerializationError> {
    let mut off = 0;
    // Skip tag 30 (0xd8 0x1e) if present
    if data.len() >= 2 && data[0] == 0xd8 && data[1] == 0x1e {
        off += 2;
    }
    let (arr_len, n) = decode_array_len(&data[off..])?;
    off += n;
    if arr_len != 2 {
        return Err(SerializationError::InvalidLength {
            expected: 2,
            got: arr_len,
        });
    }
    let (num, n) = decode_bigint_or_uint(&data[off..])?;
    off += n;
    let (den, n) = decode_bigint_or_uint(&data[off..])?;
    off += n;
    Ok(((num, den), off))
}

/// Check whether the next byte is CBOR null (`0xf6`).
///
/// Returns `(true, 1)` if the byte is null (consuming it), or `(false, 0)` if not
/// (leaving the cursor unchanged so the caller can decode the actual value).
pub fn decode_null(data: &[u8]) -> Result<(bool, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    if data[0] == 0xf6 {
        Ok((true, 1))
    } else {
        Ok((false, 0))
    }
}

/// Maximum nesting depth for `skip_cbor_value`.
///
/// D10 / audit #544: a deeply-nested indefinite CBOR (e.g. 1000 nested arrays) would
/// exhaust the stack via unbounded recursion.  We cap at 64 levels — sufficient for
/// any legitimate Cardano ledger state structure — and return an error beyond that.
const CBOR_SKIP_MAX_DEPTH: usize = 64;

/// Skip over any single CBOR value, returning the number of bytes consumed.
///
/// Used for fields we don't need to fully decode (e.g., NonMyopic, pulsingRewUpdate).
///
/// # D10 / audit #544 — recursion depth limit
///
/// This is a thin public wrapper that enforces a maximum nesting depth via
/// `skip_cbor_value_depth`.  Adversarial CBOR with thousands of nested arrays
/// would otherwise cause a stack overflow.
pub fn skip_cbor_value(data: &[u8]) -> Result<usize, SerializationError> {
    skip_cbor_value_depth(data, 0)
}

fn skip_cbor_value_depth(data: &[u8], depth: usize) -> Result<usize, SerializationError> {
    if depth > CBOR_SKIP_MAX_DEPTH {
        return Err(SerializationError::CborDecode(format!(
            "CBOR nesting depth exceeds limit ({CBOR_SKIP_MAX_DEPTH})"
        )));
    }
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    match major {
        // Unsigned or negative integer
        0 | 1 => {
            let (_, n) = decode_uint_info(data, info)?;
            Ok(n)
        }
        // Byte string or text string
        2 | 3 => {
            if info == 31 {
                // D15 / audit #544: indefinite-length chunked byte/text string (CBOR §2.2).
                // Iterate through definite-length chunks until the break byte (0xff).
                let mut off = 1; // skip the 0x5f/0x7f header
                while off < data.len() && data[off] != 0xff {
                    // Each chunk is a definite-length byte/text string with the same major type.
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?;
                }
                if off >= data.len() {
                    return Err(eof());
                }
                Ok(off + 1) // +1 for the break byte 0xff
            } else {
                let hdr_len = match info {
                    0..=23 => 1usize,
                    24 => 2,
                    25 => 3,
                    26 => 5,
                    27 => 9,
                    _ => {
                        return Err(SerializationError::CborDecode(
                            "invalid string length encoding".into(),
                        ))
                    }
                };
                let payload_len = match info {
                    0..=23 => info as usize,
                    24 => {
                        if data.len() < 2 {
                            return Err(eof());
                        }
                        data[1] as usize
                    }
                    25 => {
                        if data.len() < 3 {
                            return Err(eof());
                        }
                        u16::from_be_bytes([data[1], data[2]]) as usize
                    }
                    26 => {
                        if data.len() < 5 {
                            return Err(eof());
                        }
                        u32::from_be_bytes([data[1], data[2], data[3], data[4]]) as usize
                    }
                    27 => {
                        if data.len() < 9 {
                            return Err(eof());
                        }
                        u64::from_be_bytes([
                            data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
                        ]) as usize
                    }
                    _ => {
                        return Err(SerializationError::CborDecode(
                            "invalid string length encoding".into(),
                        ))
                    }
                };
                Ok(hdr_len + payload_len)
            }
        }
        // Array
        4 => {
            if info == 31 {
                // Indefinite-length array
                let mut off = 1;
                while off < data.len() && data[off] != 0xff {
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?;
                }
                Ok(off + 1) // +1 for the break byte 0xff
            } else {
                let (count, mut off) = decode_uint_info(data, info)?;
                for _ in 0..count {
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?;
                }
                Ok(off)
            }
        }
        // Map
        5 => {
            if info == 31 {
                let mut off = 1;
                while off < data.len() && data[off] != 0xff {
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?; // key
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?; // value
                }
                Ok(off + 1) // +1 for the break byte 0xff
            } else {
                let (count, mut off) = decode_uint_info(data, info)?;
                for _ in 0..count {
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?; // key
                    if off > data.len() {
                        return Err(eof());
                    }
                    off += skip_cbor_value_depth(&data[off..], depth + 1)?; // value
                }
                Ok(off)
            }
        }
        // Tag: skip the tag header then skip the tagged value
        6 => {
            let (_, n) = decode_uint_info(data, info)?;
            if n > data.len() {
                return Err(eof());
            }
            let inner = skip_cbor_value_depth(&data[n..], depth + 1)?;
            Ok(n + inner)
        }
        // Simple values and floats
        7 => match info {
            0..=23 => Ok(1), // simple value (null=22, true=21, false=20, etc.)
            24 => Ok(2),
            25 => Ok(3), // float16
            26 => Ok(5), // float32
            27 => Ok(9), // float64
            31 => Ok(1), // break code (should not appear at top level, but handle gracefully)
            _ => Err(SerializationError::CborDecode(
                "invalid simple/float encoding".into(),
            )),
        },
        _ => unreachable!("CBOR major type is 3 bits, range 0-7"),
    }
}

/// Decode a CBOR array header, returning `(Some(length), bytes_consumed)` for a
/// definite-length array, or `(None, 1)` for an indefinite-length array (`0x9f`).
///
/// Use this instead of [`decode_array_len`] when the caller can handle both
/// definite and indefinite arrays.
pub fn decode_array_len_or_indef(
    data: &[u8],
) -> Result<(Option<usize>, usize), SerializationError> {
    if data.is_empty() {
        return Err(eof());
    }
    let major = data[0] >> 5;
    let info = data[0] & 0x1f;
    if major != 4 {
        return Err(SerializationError::CborDecode(format!(
            "expected array (major 4), got major {major} at byte {:#04x}",
            data[0]
        )));
    }
    if info == 31 {
        return Ok((None, 1));
    }
    let (len, consumed) = decode_uint_info(data, info)?;
    Ok((Some(len as usize), consumed))
}

/// State for iterating over a CBOR map, supporting both definite-length and
/// indefinite-length encodings.
///
/// Usage:
/// ```ignore
/// let (mut reader, n) = MapReader::new(data)?;
/// off += n;
/// while reader.has_next(&data[off..])? {
///     // decode key-value pair ...
///     off += consumed;
/// }
/// off += reader.finish(&data[off..])?;
/// ```
pub struct MapReader {
    remaining: Option<usize>, // None = indefinite
}

impl MapReader {
    /// Decode a CBOR map header (definite or indefinite) and return a `MapReader`.
    ///
    /// Returns `(reader, bytes_consumed_by_header)`.
    pub fn new(data: &[u8]) -> Result<(Self, usize), SerializationError> {
        let (opt_len, n) = decode_map_len(data)?;
        Ok((MapReader { remaining: opt_len }, n))
    }

    /// Check whether there is another key-value pair to read.
    ///
    /// For definite-length maps this decrements the remaining count.
    /// For indefinite maps this peeks for the break byte (0xff).
    pub fn has_next(&mut self, data: &[u8]) -> Result<bool, SerializationError> {
        match &mut self.remaining {
            Some(count) => {
                if *count == 0 {
                    Ok(false)
                } else {
                    *count -= 1;
                    Ok(true)
                }
            }
            None => {
                if data.is_empty() {
                    Err(eof())
                } else {
                    Ok(data[0] != 0xff)
                }
            }
        }
    }

    /// Consume the trailing break byte for indefinite-length maps.
    /// Returns 0 for definite-length maps, 1 for indefinite (the 0xff byte).
    pub fn finish(&self, data: &[u8]) -> Result<usize, SerializationError> {
        match self.remaining {
            Some(_) => Ok(0),
            None => {
                if data.is_empty() || data[0] != 0xff {
                    Err(SerializationError::CborDecode(
                        "expected break byte (0xff) at end of indefinite map".into(),
                    ))
                } else {
                    Ok(1)
                }
            }
        }
    }

    /// Hint for pre-allocating collections.  Returns 0 for indefinite maps.
    ///
    /// **Cap (#554):** the value is clamped to `MAP_SIZE_HINT_CAP` to
    /// prevent untrusted CBOR (corrupted snapshot, peer-controlled gov
    /// state) from forcing a huge `HashMap::with_capacity` allocation. The
    /// actual decode loop drives `has_next`, so clamping the *hint* never
    /// truncates real data — it only bounds the initial allocation.
    pub fn size_hint(&self) -> usize {
        self.remaining.unwrap_or(0).min(MAP_SIZE_HINT_CAP)
    }
}

/// Cap on the initial `HashMap::with_capacity` allocation when a CBOR map's
/// declared length is used as a size hint. The Cardano ledger snapshot
/// contains some genuinely large maps (millions of UTxOs, hundreds of
/// thousands of stake credentials), so the cap must be generous enough to
/// allow them — but it MUST exist, because the declared length comes from
/// CBOR header bytes that an attacker (e.g. via a tampered Mithril snapshot)
/// can manipulate.
///
/// 8M entries × HashMap's per-entry overhead (~48 bytes on x86_64) ≈ 384 MiB.
/// That's a realistic memory budget for the largest known maps. Anything
/// beyond that is rejected and the decoder falls back to organic growth.
pub const MAP_SIZE_HINT_CAP: usize = 8 * 1024 * 1024;

/// Compute a safe `Vec::with_capacity` value for an array whose declared
/// length came from peer-controlled / untrusted CBOR.
///
/// Caps the value at the smaller of:
///   - the declared length (the natural value),
///   - the protocol-spec `max_allowed` (or `usize::MAX` if unbounded),
///   - `remaining_bytes / MIN_BYTES_PER_ELEMENT`, since every CBOR value is
///     at least 1 byte and a declared length larger than the input cannot
///     possibly be honest.
///
/// Returns `Err` if the declared length exceeds `max_allowed`.
///
/// This is the canonical pre-flight for the systemic pattern #5 from audit
/// #548 / #554. Use it at every site where `Vec::with_capacity(arr_len)` or
/// `Vec::reserve(arr_len)` is called with `arr_len` derived from CBOR.
pub fn bounded_alloc_capacity(
    declared_len: usize,
    max_allowed: usize,
    remaining_bytes: usize,
) -> Result<usize, SerializationError> {
    if declared_len > max_allowed {
        return Err(SerializationError::CborDecode(format!(
            "declared length {declared_len} exceeds protocol-spec max {max_allowed}"
        )));
    }
    if declared_len > remaining_bytes {
        return Err(SerializationError::CborDecode(format!(
            "declared length {declared_len} exceeds remaining input bytes ({remaining_bytes})"
        )));
    }
    Ok(declared_len)
}

/// Construct an "unexpected end of input" error.
fn eof() -> SerializationError {
    SerializationError::CborDecode("unexpected end of input".into())
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn bounded_alloc_happy() {
        assert_eq!(bounded_alloc_capacity(100, 1000, 200).unwrap(), 100);
    }

    #[test]
    fn bounded_alloc_rejects_over_max() {
        let err = bounded_alloc_capacity(1001, 1000, 10_000).unwrap_err();
        assert!(format!("{err:?}").contains("exceeds protocol-spec max"));
    }

    #[test]
    fn bounded_alloc_rejects_over_remaining() {
        let err = bounded_alloc_capacity(100, 1000, 50).unwrap_err();
        assert!(format!("{err:?}").contains("exceeds remaining input bytes"));
    }

    #[test]
    fn bounded_alloc_u64_max_class_attack_rejected() {
        // The actual attack pattern: arr_len from CBOR is usize::MAX / 8
        // (typical 64-bit Vec capacity bombs).
        let err = bounded_alloc_capacity(usize::MAX, 1000, 10).unwrap_err();
        assert!(format!("{err:?}").contains("exceeds"));
    }

    #[test]
    fn map_size_hint_capped() {
        // MapReader::size_hint shouldn't be larger than MAP_SIZE_HINT_CAP.
        let reader = MapReader {
            remaining: Some(usize::MAX),
        };
        assert_eq!(reader.size_hint(), MAP_SIZE_HINT_CAP);
    }

    #[test]
    fn map_size_hint_indefinite_returns_zero() {
        let reader = MapReader { remaining: None };
        assert_eq!(reader.size_hint(), 0);
    }

    #[test]
    fn map_size_hint_small_passthrough() {
        let reader = MapReader {
            remaining: Some(42),
        };
        assert_eq!(reader.size_hint(), 42);
    }
}
