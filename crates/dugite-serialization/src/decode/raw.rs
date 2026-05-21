//! Zero-copy raw-byte preservation for CBOR values.
//!
//! [`KeepRaw`] wraps a decoded value together with the exact CBOR bytes that
//! produced it. This is essential for several correctness invariants in Cardano:
//!
//! - **Transaction body hash** (`tx.hash`): computed over the original CBOR encoding.
//! - **Auxiliary data hash**: `blake2b_256(raw_cbor)` of the auxiliary data.
//! - **Script data hash**: covers raw redeemers + raw Plutus datums, preserving
//!   definite-vs-indefinite array length encoding and Plutus byte-string chunking.
//! - **Native script hash**: `blake2b_224(0x00 || raw_cbor)`.
//! - **Inline datum**: passed verbatim to Plutus phase-2 via the script context.
//!
//! The implementation uses [`Reader::position`] / [`Reader::slice_from`] to take
//! a zero-copy slice of the original decode buffer. No intermediate allocation is
//! made during parsing; `to_owned()` allocates once at the point where the caller
//! needs an owned copy.

use crate::decode::reader::Reader;
use crate::error::SerializationError;

/// A decoded value together with the exact raw CBOR bytes it was decoded from.
///
/// The lifetime `'b` is tied to the input buffer of the [`Reader`] that produced
/// this value. When `'b` must be erased (e.g. when storing in a long-lived type),
/// call [`KeepRaw::to_owned`] to copy the raw bytes into a `Vec<u8>`.
///
/// # Invariant
///
/// `raw` is always a non-empty slice of the original buffer. It spans exactly the
/// bytes of the outermost CBOR value that was decoded by the closure passed to
/// [`KeepRaw::parse_with`].
#[derive(Debug, Clone)]
pub struct KeepRaw<'b, T> {
    /// The decoded value.
    pub value: T,
    /// The verbatim CBOR bytes from which `value` was decoded.
    pub raw: &'b [u8],
}

impl<'b, T> KeepRaw<'b, T> {
    /// Decode a value while capturing the underlying raw CBOR bytes.
    ///
    /// Snapshots `r.position()` before calling `f`, then slices
    /// `r.origin[start..r.position()]` after `f` returns. The slice covers
    /// exactly the bytes that `f` consumed.
    ///
    /// A debug assertion verifies that the position is monotonically increasing
    /// (i.e. `f` did not seek backwards), which would indicate a bug in the
    /// closure.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let body = KeepRaw::parse_with(&mut r, |r| decode_tx_body(r))?;
    /// let tx_hash = blake2b_256(body.raw);
    /// ```
    pub fn parse_with<F>(r: &mut Reader<'b>, f: F) -> Result<Self, SerializationError>
    where
        F: FnOnce(&mut Reader<'b>) -> Result<T, SerializationError>,
    {
        let start = r.position();
        let value = f(r)?;
        let end = r.position();
        debug_assert!(
            end >= start,
            "KeepRaw: position went backwards from {start} to {end} — reader bug"
        );
        let raw = r.slice_from(start);
        Ok(KeepRaw { value, raw })
    }

    /// Convert this borrowed `KeepRaw<'b, T>` to an owned `KeepRaw<'static, T>` by
    /// copying the raw bytes into a freshly allocated `Vec<u8>`.
    ///
    /// Use this when you need to store the raw bytes beyond the lifetime of the
    /// original decode buffer (e.g. in a `Block` or `Transaction` that outlives the
    /// network frame it was decoded from).
    pub fn into_owned(self) -> KeepRawOwned<T> {
        KeepRawOwned {
            value: self.value,
            raw: self.raw.to_vec(),
        }
    }
}

/// An owned variant of [`KeepRaw`] whose raw bytes are stored in a `Vec<u8>`.
///
/// Produced by [`KeepRaw::to_owned`]. Carries no lifetime.
#[derive(Debug, Clone)]
pub struct KeepRawOwned<T> {
    /// The decoded value.
    pub value: T,
    /// Owned copy of the verbatim CBOR bytes.
    pub raw: Vec<u8>,
}

impl<T> KeepRawOwned<T> {
    /// Borrow the raw bytes as a slice.
    #[inline]
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::reader::Reader;

    // Helper: encode a definite-length array of uints (test inputs use small values).
    fn cbor_uint_bytes(n: u64) -> Vec<u8> {
        if n < 24 {
            vec![n as u8]
        } else if n < 256 {
            vec![0x18, n as u8]
        } else if n < 65536 {
            let mut v = vec![0x19];
            v.extend_from_slice(&(n as u16).to_be_bytes());
            v
        } else {
            let mut v = vec![0x1b];
            v.extend_from_slice(&n.to_be_bytes());
            v
        }
    }

    fn cbor_array_uints(items: &[u64]) -> Vec<u8> {
        assert!(items.len() <= 23, "test helper assumes small arrays");
        let mut v = vec![0x80u8 | items.len() as u8];
        for &n in items {
            v.extend(cbor_uint_bytes(n));
        }
        v
    }

    fn cbor_uint(n: u8) -> Vec<u8> {
        cbor_uint_bytes(n as u64)
    }

    // -----------------------------------------------------------------------
    // Basic parse_with semantics
    // -----------------------------------------------------------------------

    #[test]
    fn parse_with_captures_bytes() {
        // Buffer: [0x82, 0x01, 0x02] = array(2)[1, 2], followed by byte uint(99)
        let mut data = cbor_array_uints(&[1, 2]);
        data.push(99); // trailing uint
        let mut r = Reader::new(&data);

        let kept = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();

        // The captured raw bytes should be exactly the array encoding.
        assert_eq!(kept.raw, &cbor_array_uints(&[1, 2]));
        assert_eq!(kept.value, vec![1u64, 2]);
        // Reader should have advanced past the array, not the trailing byte.
        assert_eq!(r.position(), cbor_array_uints(&[1, 2]).len());
    }

    #[test]
    fn parse_with_single_uint() {
        let data = cbor_uint(7);
        let mut r = Reader::new(&data);
        let kept = KeepRaw::parse_with(&mut r, |r| r.read_uint()).unwrap();
        assert_eq!(kept.raw, &[7u8]);
        assert_eq!(kept.value, 7u64);
        assert_eq!(r.position(), 1);
    }

    #[test]
    fn parse_with_position_monotonic() {
        // After parse_with, position must equal the end of the decoded value.
        let data = cbor_array_uints(&[10, 20, 30]);
        let len = data.len();
        let mut r = Reader::new(&data);
        let _kept = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();
        assert_eq!(r.position(), len);
    }

    #[test]
    fn parse_with_nested() {
        // Outer array [[1, 2], [3, 4]]
        let inner_a = cbor_array_uints(&[1, 2]);
        let inner_b = cbor_array_uints(&[3, 4]);
        let mut data = vec![0x82u8]; // array(2)
        data.extend_from_slice(&inner_a);
        data.extend_from_slice(&inner_b);

        let mut r = Reader::new(&data);
        // Capture the outer array.
        let kept = KeepRaw::parse_with(&mut r, |r| {
            r.read_array(|r| r.read_array(|r| r.read_uint()))
        })
        .unwrap();

        assert_eq!(kept.raw, &data[..]);
        assert_eq!(kept.value, vec![vec![1u64, 2], vec![3, 4]]);
    }

    // -----------------------------------------------------------------------
    // to_owned
    // -----------------------------------------------------------------------

    #[test]
    fn to_owned_copies_raw() {
        let data = cbor_array_uints(&[5, 6]);
        let mut r = Reader::new(&data);
        let kept = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();
        let owned = kept.into_owned();
        assert_eq!(owned.raw, cbor_array_uints(&[5, 6]));
        assert_eq!(owned.value, vec![5u64, 6]);
    }

    #[test]
    fn owned_raw_bytes_helper() {
        let data = cbor_uint(3);
        let mut r = Reader::new(&data);
        let kept = KeepRaw::parse_with(&mut r, |r| r.read_uint()).unwrap();
        let owned = kept.into_owned();
        assert_eq!(owned.raw_bytes(), &[3u8]);
    }

    // -----------------------------------------------------------------------
    // Empty values
    // -----------------------------------------------------------------------

    #[test]
    fn parse_with_empty_array() {
        let data = cbor_array_uints(&[]);
        let mut r = Reader::new(&data);
        let kept = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();
        assert_eq!(kept.value, Vec::<u64>::new());
        assert_eq!(kept.raw, &[0x80u8]); // empty array header
    }

    // -----------------------------------------------------------------------
    // Multiple parse_with calls on same reader
    // -----------------------------------------------------------------------

    #[test]
    fn two_sequential_keep_raws() {
        let a = cbor_array_uints(&[1]);
        let b = cbor_array_uints(&[2, 3]);
        let mut data = a.clone();
        data.extend_from_slice(&b);

        let mut r = Reader::new(&data);
        let ka = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();
        let kb = KeepRaw::parse_with(&mut r, |r| r.read_array(|r| r.read_uint())).unwrap();

        assert_eq!(ka.raw, &a[..]);
        assert_eq!(kb.raw, &b[..]);
        assert_eq!(r.position(), data.len());
    }
}
