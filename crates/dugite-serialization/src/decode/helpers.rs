//! Leaf decode helpers for common Cardano primitives.
//!
//! Each function takes a `&mut Reader<'b>` and returns a specific dugite primitive.
//! These are deliberately simple: one value in, one value out, no era-specific
//! logic. Era-specific semantics (e.g. which fields use `Hash28` vs `Hash32`)
//! live in the era decoder files.
//!
//! ## No implicit hash widening
//!
//! `Hash28` and `Hash32` are **distinct types** in `dugite-primitives`. There is
//! no implicit conversion from one to the other. When a 28-byte hash must be used
//! as a 32-byte map key (e.g. pool IDs in reward distributions), the caller must
//! explicitly call `.to_hash32_padded()` from `dugite_primitives::hash::Hash28`.
//! This constraint is enforced here by providing separate `read_hash28` and
//! `read_hash32` functions with no implicit widening.
//!
//! ## Network ID
//!
//! `read_network_id` returns a raw `u8` (0 = Testnet, 1 = Mainnet). The caller
//! is responsible for interpreting this in the context of the address or header
//! being decoded.

use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::hash::{Hash, Hash28, Hash32};
use dugite_primitives::value::Lovelace;

/// Read a CBOR byte string and interpret it as a `Hash<N>`.
///
/// Returns `SerializationError::InvalidLength` if the byte string is not exactly
/// `N` bytes long.
///
/// # Type parameter
/// `N` is the expected byte count, e.g. 28 or 32. Prefer the named aliases
/// [`read_hash28`] and [`read_hash32`] for clarity.
pub fn read_hash<'b, const N: usize>(r: &mut Reader<'b>) -> Result<Hash<N>, SerializationError> {
    let bytes = r.read_bytes()?;
    if bytes.len() != N {
        return Err(SerializationError::InvalidLength {
            expected: N,
            got: bytes.len(),
        });
    }
    // SAFETY: we just verified bytes.len() == N.
    let arr: [u8; N] = bytes
        .try_into()
        .map_err(|_| SerializationError::InvalidLength {
            expected: N,
            got: bytes.len(),
        })?;
    Ok(Hash::from_bytes(arr))
}

/// Read a CBOR byte string and interpret it as a 28-byte hash (`Hash28`).
///
/// Returns an error if the byte string is not exactly 28 bytes. Used for:
/// - `PolicyId` (minting policy / script hash in 28-byte form)
/// - `PoolKeyHash` (pool operator key hash)
/// - `GenesisHash`, `GenesisDelegateHash`
/// - DRep key hashes, required signer hashes, committee cold/hot hashes
///
/// When a `Hash28` must be used as a `Hash32` key, the caller must explicitly
/// call `.to_hash32_padded()` — no implicit widening occurs here.
pub fn read_hash28(r: &mut Reader<'_>) -> Result<Hash28, SerializationError> {
    read_hash::<28>(r)
}

/// Read a CBOR byte string and interpret it as a 32-byte hash (`Hash32`).
///
/// Returns an error if the byte string is not exactly 32 bytes. Used for:
/// - `TransactionHash` (tx id)
/// - `BlockHeaderHash`
/// - `DatumHash`
/// - `AuxiliaryDataHash`
/// - `VrfKeyHash`
/// - Epoch nonce
pub fn read_hash32(r: &mut Reader<'_>) -> Result<Hash32, SerializationError> {
    read_hash::<32>(r)
}

/// Read a CBOR uint and interpret it as a Lovelace amount.
///
/// Cardano amounts are always non-negative. A zero value is valid (e.g. deposit
/// return outputs). The maximum ADA supply (45 × 10⁹ ADA = 45 × 10¹⁵ lovelace)
/// fits comfortably in a u64.
pub fn read_lovelace(r: &mut Reader<'_>) -> Result<Lovelace, SerializationError> {
    r.read_uint().map(Lovelace)
}

/// Read a CBOR uint and validate it as a network identifier.
///
/// Returns `0` for Testnet and `1` for Mainnet. Returns an error for any other
/// value — Cardano currently defines only two network IDs.
pub fn read_network_id(r: &mut Reader<'_>) -> Result<u8, SerializationError> {
    let id = r.read_uint()?;
    match id {
        0 | 1 => Ok(id as u8),
        other => Err(SerializationError::CborDecode(format!(
            "read_network_id: invalid network id {other} (expected 0 or 1)"
        ))),
    }
}

// =========================================================================
// Tests
// =========================================================================

/// Deduplicate decoded redeemers by `(tag, index)` with Haskell's exact
/// collision semantics: **last occurrence wins** (`Map.fromList` — used by
/// cardano-ledger for BOTH the pre-PV9 list wire form and the PV9+ map wire
/// form of the redeemers field; duplicates are deliberately NOT a decode
/// error, see `Cardano.Ledger.Alonzo.TxWits` `RedeemersRaw . Map.fromList`).
///
/// The kept (last) value is placed at the position where its key FIRST
/// appeared, so the relative order of distinct keys is unchanged — for
/// transactions without duplicates this is the identity.
///
/// Why this matters (#753): every downstream Haskell consumer (`totExUnits`
/// for the BBODY `maxBlockExUnits` check and `minfee`,
/// `collectTwoPhaseScriptInputs`/`evalScripts`) operates on the deduplicated
/// Map. Keeping wire duplicates in a Vec double-counted a duplicated mint
/// redeemer's ExUnits in mainnet block 8,826,011 (slot 93,595,649) — dugite
/// computed 20,173,234,420 block steps vs the true 19,214,574,638 and
/// HALTED on a confirmed block — and double-evaluated such scripts in
/// phase-2 (spurious 'budget exhausted' divergences; tx 55519c6d… and the
/// long-unexplained preprod #730 'fixed-delta' residual class).
///
/// The raw redeemers wire bytes are preserved separately by the witness
/// decoders, so script-integrity hashing is unaffected.
pub(crate) fn dedup_redeemers_last_wins(
    redeemers: Vec<dugite_primitives::transaction::Redeemer>,
) -> Vec<dugite_primitives::transaction::Redeemer> {
    use std::collections::HashMap;
    let mut pos: HashMap<(dugite_primitives::transaction::RedeemerTag, u32), usize> =
        HashMap::with_capacity(redeemers.len());
    let mut out: Vec<dugite_primitives::transaction::Redeemer> =
        Vec::with_capacity(redeemers.len());
    for rd in redeemers {
        let key = (rd.tag.clone(), rd.index);
        match pos.get(&key) {
            Some(&i) => out[i] = rd, // last value wins, first position kept
            None => {
                pos.insert(key, out.len());
                out.push(rd);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::reader::Reader;

    fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        if b.len() <= 23 {
            let mut v = vec![0x40 | b.len() as u8];
            v.extend_from_slice(b);
            v
        } else if b.len() <= 0xff {
            let mut v = vec![0x58, b.len() as u8];
            v.extend_from_slice(b);
            v
        } else {
            let len = b.len() as u16;
            let bytes = len.to_be_bytes();
            let mut v = vec![0x59, bytes[0], bytes[1]];
            v.extend_from_slice(b);
            v
        }
    }

    fn cbor_uint(n: u64) -> Vec<u8> {
        if n <= 23 {
            vec![n as u8]
        } else if n <= 0xff {
            vec![0x18, n as u8]
        } else {
            let b = n.to_be_bytes();
            vec![0x1b, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
        }
    }

    // -----------------------------------------------------------------------
    // read_hash28
    // -----------------------------------------------------------------------

    #[test]
    fn read_hash28_ok() {
        let bytes = [0xabu8; 28];
        let data = cbor_bytes(&bytes);
        let mut r = Reader::new(&data);
        let h = read_hash28(&mut r).unwrap();
        assert_eq!(h, Hash28::from_bytes([0xab; 28]));
    }

    #[test]
    fn read_hash28_wrong_length_rejected() {
        // 32-byte payload where 28 is expected.
        let data = cbor_bytes(&[0xabu8; 32]);
        let mut r = Reader::new(&data);
        let err = read_hash28(&mut r).unwrap_err();
        assert!(
            matches!(
                err,
                SerializationError::InvalidLength {
                    expected: 28,
                    got: 32
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn read_hash28_short_rejected() {
        let data = cbor_bytes(&[0u8; 27]);
        let mut r = Reader::new(&data);
        assert!(matches!(
            read_hash28(&mut r).unwrap_err(),
            SerializationError::InvalidLength {
                expected: 28,
                got: 27
            }
        ));
    }

    #[test]
    fn read_hash28_empty_rejected() {
        let data = cbor_bytes(&[]);
        let mut r = Reader::new(&data);
        assert!(matches!(
            read_hash28(&mut r).unwrap_err(),
            SerializationError::InvalidLength {
                expected: 28,
                got: 0
            }
        ));
    }

    // -----------------------------------------------------------------------
    // read_hash32
    // -----------------------------------------------------------------------

    #[test]
    fn read_hash32_ok() {
        let bytes = [0xffu8; 32];
        let data = cbor_bytes(&bytes);
        let mut r = Reader::new(&data);
        let h = read_hash32(&mut r).unwrap();
        assert_eq!(h, Hash32::from_bytes([0xff; 32]));
    }

    #[test]
    fn read_hash32_wrong_length_rejected() {
        let data = cbor_bytes(&[0u8; 28]);
        let mut r = Reader::new(&data);
        assert!(matches!(
            read_hash32(&mut r).unwrap_err(),
            SerializationError::InvalidLength {
                expected: 32,
                got: 28
            }
        ));
    }

    #[test]
    fn read_hash32_zero_hash() {
        let data = cbor_bytes(&[0u8; 32]);
        let mut r = Reader::new(&data);
        let h = read_hash32(&mut r).unwrap();
        assert_eq!(h, Hash32::ZERO);
    }

    // -----------------------------------------------------------------------
    // No implicit 28→32 widening
    // -----------------------------------------------------------------------

    #[test]
    fn hash28_to_hash32_requires_explicit_call() {
        // A 28-byte hash cannot be used as Hash32 directly — explicit conversion needed.
        let bytes = [0x01u8; 28];
        let data = cbor_bytes(&bytes);
        let mut r = Reader::new(&data);
        let h28 = read_hash28(&mut r).unwrap();
        // Explicit padded conversion:
        let h32 = h28.to_hash32_padded();
        // First 28 bytes match; last 4 are zero.
        assert_eq!(&h32.as_bytes()[..28], &[0x01; 28]);
        assert_eq!(&h32.as_bytes()[28..], &[0x00; 4]);
    }

    // -----------------------------------------------------------------------
    // read_lovelace
    // -----------------------------------------------------------------------

    #[test]
    fn read_lovelace_zero() {
        let data = cbor_uint(0);
        let mut r = Reader::new(&data);
        assert_eq!(read_lovelace(&mut r).unwrap(), Lovelace(0));
    }

    #[test]
    fn read_lovelace_max_ada_supply() {
        // 45 billion ADA × 10^6 lovelace = 45_000_000_000_000_000
        let supply: u64 = 45_000_000_000_000_000;
        let data = cbor_uint(supply);
        let mut r = Reader::new(&data);
        assert_eq!(read_lovelace(&mut r).unwrap(), Lovelace(supply));
    }

    #[test]
    fn read_lovelace_u64_max() {
        let data = cbor_uint(u64::MAX);
        let mut r = Reader::new(&data);
        assert_eq!(read_lovelace(&mut r).unwrap(), Lovelace(u64::MAX));
    }

    // -----------------------------------------------------------------------
    // read_network_id
    // -----------------------------------------------------------------------

    #[test]
    fn read_network_id_testnet() {
        let data = cbor_uint(0);
        let mut r = Reader::new(&data);
        assert_eq!(read_network_id(&mut r).unwrap(), 0);
    }

    #[test]
    fn read_network_id_mainnet() {
        let data = cbor_uint(1);
        let mut r = Reader::new(&data);
        assert_eq!(read_network_id(&mut r).unwrap(), 1);
    }

    #[test]
    fn read_network_id_invalid_rejected() {
        let data = cbor_uint(2);
        let mut r = Reader::new(&data);
        let err = read_network_id(&mut r).unwrap_err();
        assert!(matches!(err, SerializationError::CborDecode(_)));
        let msg = format!("{err}");
        assert!(msg.contains("invalid network id 2"));
    }

    #[test]
    fn read_network_id_large_invalid() {
        let data = cbor_uint(255);
        let mut r = Reader::new(&data);
        assert!(read_network_id(&mut r).is_err());
    }

    // -----------------------------------------------------------------------
    // read_hash (generic — edge cases)
    // -----------------------------------------------------------------------

    #[test]
    fn read_hash_generic_4_bytes() {
        let data = cbor_bytes(&[0x01, 0x02, 0x03, 0x04]);
        let mut r = Reader::new(&data);
        let h: Hash<4> = read_hash(&mut r).unwrap();
        assert_eq!(h, Hash::from_bytes([0x01, 0x02, 0x03, 0x04]));
    }
}
