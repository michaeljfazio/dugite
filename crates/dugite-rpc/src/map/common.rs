//! Shared mapping helpers used by every `map/*` module.
//!
//! Tiny, pure functions only — no state, no I/O. Anything bigger lives in
//! the per-concept files.

use crate::proto::v1beta::cardano as pb;
use dugite_primitives::hash::Hash;

/// Map any fixed-size dugite hash to a `Vec<u8>` for protobuf `bytes` fields.
#[inline]
pub fn hash_bytes<const N: usize>(h: &Hash<N>) -> Vec<u8> {
    h.as_ref().to_vec()
}

/// Encode a `u64` lovelace / quantity value as a `BigInt` protobuf message.
///
/// All Cardano values comfortably fit in `i64` (mainnet supply cap is
/// 45 × 10¹⁵ lovelace ≪ 9.2 × 10¹⁸), so we always pick the `int` variant
/// rather than walking through big-endian byte encoding.
#[inline]
pub fn coin_bigint(value: u64) -> pb::BigInt {
    pb::BigInt {
        big_int: Some(pb::big_int::BigInt::Int(value as i64)),
    }
}

/// Encode a signed `i64` (e.g. mint deltas which can be negative for
/// burns) as a `BigInt` protobuf message.
#[inline]
pub fn signed_bigint(value: i64) -> pb::BigInt {
    pb::BigInt {
        big_int: Some(pb::big_int::BigInt::Int(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1beta::cardano::big_int as bi;

    #[test]
    fn coin_uses_int_variant_within_i64_range() {
        let b = coin_bigint(45_000_000_000_000_000); // 45 PB lovelace
        match b.big_int {
            Some(bi::BigInt::Int(v)) => assert_eq!(v, 45_000_000_000_000_000),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn signed_int_handles_negative_mint() {
        let b = signed_bigint(-1_000_000);
        match b.big_int {
            Some(bi::BigInt::Int(v)) => assert_eq!(v, -1_000_000),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn hash_bytes_round_trip() {
        let h = Hash::<32>::from_bytes([5u8; 32]);
        let v = hash_bytes(&h);
        assert_eq!(v.len(), 32);
        assert_eq!(&v[..], &[5u8; 32]);
    }
}
