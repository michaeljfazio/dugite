//! `dugite_primitives::PlutusData` → `utxorpc.v1beta.cardano.PlutusData`.
//!
//! Recursive mapping covering every Plutus Data shape:
//!   * `Constr(tag, fields)` — algebraic-data-type constructor.
//!   * `Map(pairs)` — key-value pairs.
//!   * `List(items)` — array.
//!   * `Integer(BigInt)` — arbitrary-precision signed integer.
//!   * `Bytes(Vec<u8>)` — bounded byte string.

use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::PlutusData;
use num_bigint::Sign;

/// Map a dugite `PlutusData` to its protobuf shape.
pub fn plutus_data_to_proto(d: &PlutusData) -> pb::PlutusData {
    use pb::plutus_data::PlutusData as Inner;
    let inner = match d {
        PlutusData::Constr(tag, fields) => Inner::Constr(pb::Constr {
            tag: *tag as u32,
            // `any_constructor` is a future-compat field for >127 tags; we
            // mirror the spec by leaving it 0 when `tag` fits in 7 bits.
            any_constructor: 0,
            fields: fields.iter().map(plutus_data_to_proto).collect(),
        }),
        PlutusData::Map(pairs) => Inner::Map(pb::PlutusDataMap {
            pairs: pairs
                .iter()
                .map(|(k, v)| pb::PlutusDataPair {
                    key: Some(plutus_data_to_proto(k)),
                    value: Some(plutus_data_to_proto(v)),
                })
                .collect(),
        }),
        PlutusData::List(items) => Inner::Array(pb::PlutusDataArray {
            items: items.iter().map(plutus_data_to_proto).collect(),
        }),
        PlutusData::Integer(bi) => Inner::BigInt(bigint_to_proto(bi)),
        PlutusData::Bytes(b) => Inner::BoundedBytes(b.clone()),
    };
    pb::PlutusData {
        plutus_data: Some(inner),
    }
}

fn bigint_to_proto(bi: &num_bigint::BigInt) -> pb::BigInt {
    use pb::big_int::BigInt as Inner;
    // If it fits in an i64, use the cheap variant.
    if let Some(v) = bi.to_signed_bytes_be_within_i64() {
        return pb::BigInt {
            big_int: Some(Inner::Int(v)),
        };
    }
    // Otherwise, encode as big-endian two's-complement bytes.
    let (sign, mag) = bi.to_bytes_be();
    match sign {
        Sign::NoSign | Sign::Plus => pb::BigInt {
            big_int: Some(Inner::BigUInt(mag)),
        },
        Sign::Minus => {
            // utxorpc `big_n_int` encodes `-1 - n` as BE bytes.
            // dugite BigInt magnitude is `|value|`. For `value = -(n+1)`,
            // `n = -value - 1 = |value| - 1`. Subtract 1 from `mag` BE.
            let n_bytes = sub_one_be(&mag);
            pb::BigInt {
                big_int: Some(Inner::BigNInt(n_bytes)),
            }
        }
    }
}

/// Subtract 1 from a big-endian unsigned byte string. Used to encode
/// negative bigints in the utxorpc `big_n_int` convention `-1 - n`.
fn sub_one_be(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut borrow = 1u16;
    for byte in out.iter_mut().rev() {
        let v = *byte as u16;
        if v >= borrow {
            *byte = (v - borrow) as u8;
            borrow = 0;
            break;
        } else {
            *byte = (256 + v - borrow) as u8;
            borrow = 1;
        }
    }
    let _ = borrow; // exhausting borrow at the leading byte is fine
                    // Trim leading zero bytes (canonical BE).
    while out.first() == Some(&0) && out.len() > 1 {
        out.remove(0);
    }
    out
}

/// `num_bigint::BigInt` doesn't expose a direct "fits in i64" check;
/// this helper does it.
trait BigIntI64Ext {
    fn to_signed_bytes_be_within_i64(&self) -> Option<i64>;
}

impl BigIntI64Ext for num_bigint::BigInt {
    fn to_signed_bytes_be_within_i64(&self) -> Option<i64> {
        use std::convert::TryInto;
        let bytes = self.to_signed_bytes_be();
        if bytes.len() <= 8 {
            // Sign-extend up to 8 bytes.
            let pad = if bytes.first().map(|b| b & 0x80 != 0).unwrap_or(false) {
                0xFF
            } else {
                0x00
            };
            let mut buf = [pad; 8];
            let start = 8 - bytes.len();
            buf[start..].copy_from_slice(&bytes);
            Some(i64::from_be_bytes(buf))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_bytes_round_trip() {
        let d = PlutusData::Bytes(vec![0xCA, 0xFE]);
        let pb_d = plutus_data_to_proto(&d);
        match pb_d.plutus_data.unwrap() {
            pb::plutus_data::PlutusData::BoundedBytes(b) => assert_eq!(b, vec![0xCA, 0xFE]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn small_integer_uses_int_variant() {
        let d = PlutusData::Integer(num_bigint::BigInt::from(42_i64));
        let pb_d = plutus_data_to_proto(&d);
        match pb_d.plutus_data.unwrap() {
            pb::plutus_data::PlutusData::BigInt(b) => match b.big_int.unwrap() {
                pb::big_int::BigInt::Int(v) => assert_eq!(v, 42),
                o => panic!("{o:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn negative_small_integer_uses_int_variant() {
        let d = PlutusData::Integer(num_bigint::BigInt::from(-100_i64));
        let pb_d = plutus_data_to_proto(&d);
        match pb_d.plutus_data.unwrap() {
            pb::plutus_data::PlutusData::BigInt(b) => match b.big_int.unwrap() {
                pb::big_int::BigInt::Int(v) => assert_eq!(v, -100),
                o => panic!("{o:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn constr_recursive_round_trip() {
        let d = PlutusData::Constr(
            0,
            vec![
                PlutusData::Integer(num_bigint::BigInt::from(1_i64)),
                PlutusData::Bytes(vec![0xFF]),
            ],
        );
        let pb_d = plutus_data_to_proto(&d);
        match pb_d.plutus_data.unwrap() {
            pb::plutus_data::PlutusData::Constr(c) => {
                assert_eq!(c.tag, 0);
                assert_eq!(c.fields.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn map_pairs_round_trip() {
        let d = PlutusData::Map(vec![(
            PlutusData::Bytes(vec![0xAA]),
            PlutusData::Integer(num_bigint::BigInt::from(7_i64)),
        )]);
        let pb_d = plutus_data_to_proto(&d);
        match pb_d.plutus_data.unwrap() {
            pb::plutus_data::PlutusData::Map(m) => assert_eq!(m.pairs.len(), 1),
            other => panic!("{other:?}"),
        }
    }
}
