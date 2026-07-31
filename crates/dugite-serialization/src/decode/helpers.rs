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
use dugite_primitives::transaction::TransactionMetadatum;
use dugite_primitives::value::Lovelace;
use minicbor::data::Type;
use std::collections::BTreeMap;

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

// ---------------------------------------------------------------------------
// Transaction metadata (auxiliary data)
// ---------------------------------------------------------------------------

/// Decode a `Metadatum`, mirroring Haskell `decodeMetadatum`
/// (`libs/cardano-ledger-core/src/Cardano/Ledger/Metadata.hs`) token-for-token.
///
/// The reference decoder dispatches on `peekTokenType` and accepts **both** the
/// definite and the indefinite form of every compound token:
///
/// | Haskell `TokenType`                          | dugite `Type`               |
/// |----------------------------------------------|-----------------------------|
/// | `TypeUInt`, `TypeUInt64`                      | `U8`/`U16`/`U32`/`U64`      |
/// | `TypeNInt`, `TypeNInt64`                      | `I8`/`I16`/`I32`/`I64`/`Int`|
/// | `TypeBytes`, `TypeBytesIndef`                 | `Bytes`, `BytesIndef`       |
/// | `TypeString`, `TypeStringIndef`               | `String`, `StringIndef`     |
/// | `TypeListLen`, `TypeListLen64`, `…LenIndef`   | `Array`, `ArrayIndef`       |
/// | `TypeMapLen`, `TypeMapLen64`, `…LenIndef`     | `Map`, `MapIndef`           |
/// | anything else (incl. `TypeTag`)               | error                       |
///
/// Rejecting `TypeTag` is deliberate and matches Haskell: a bignum-tagged
/// integer (tag 2/3) is *not* a valid metadatum even though it denotes an
/// integer.
///
/// This is decode-acceptance only. The **encoder** stays always-definite
/// (`encodeMetadatum` uses `encodeListLen`/`encodeMapLen`, never the indefinite
/// forms) — see the `encode_map_open` pinning note from #932. Round-tripping an
/// indefinite-form metadatum therefore re-encodes it as definite, exactly as
/// Haskell does; auxiliary-data hashing is unaffected because it runs over the
/// original wire bytes captured in `AuxiliaryData::raw_cbor`.
///
/// The 64-byte `bytes`/`text` leaf bound that Haskell applies here (gated on
/// `getDecoderVersion > natVersion @2`) is enforced in dugite by Phase-1 rule
/// 1c.iii (`InvalidMetadata`, Allegra+) rather than at decode time. Both
/// implementations reject the same transactions; only the reported error
/// differs. See `dugite-ledger` `metadatum_has_oversize_leaf`.
///
/// Shared by every era decoder (Shelley, Alonzo-family, Conway/Dijkstra) so the
/// three copies cannot drift apart again (#937).
pub fn read_metadatum(r: &mut Reader<'_>) -> Result<TransactionMetadatum, SerializationError> {
    match r.peek_major()? {
        Type::Map | Type::MapIndef => {
            let entries = r.read_map(read_metadatum, read_metadatum)?;
            Ok(TransactionMetadatum::Map(entries))
        }
        Type::Array | Type::ArrayIndef => {
            let items = r.read_array(read_metadatum)?;
            Ok(TransactionMetadatum::List(items))
        }
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            Ok(TransactionMetadatum::Int(r.read_uint()? as i128))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
            Ok(TransactionMetadatum::Int(r.read_int()?))
        }
        Type::Bytes | Type::BytesIndef => Ok(TransactionMetadatum::Bytes(r.read_bytes_owned()?)),
        Type::String | Type::StringIndef => Ok(TransactionMetadatum::Text(r.read_str_owned()?)),
        other => Err(SerializationError::CborDecode(format!(
            "metadatum: unexpected type {other}"
        ))),
    }
}

/// Decode the top-level `metadata` map (`{ word64 => metadatum }`).
///
/// Accepts both the definite and the indefinite map form: cardano-node 11.0.1
/// emits indefinite-length metadata maps on preview/preprod for some CIP-20
/// message transactions (#673), and Haskell's `encodeMap` switches to the
/// indefinite form above 23 entries (#932).
pub fn read_metadata_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u64, TransactionMetadatum>, SerializationError> {
    let mut result = BTreeMap::new();
    r.for_each_map_entry(|r| {
        let label = r.read_uint()?;
        let value = read_metadatum(r)?;
        result.insert(label, value);
        Ok(())
    })?;
    Ok(result)
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

    // -----------------------------------------------------------------------
    // read_metadatum — Haskell `decodeMetadatum` parity (#937)
    //
    // Every compound token has a definite and an indefinite wire form, and the
    // reference decoder accepts BOTH. Before #937 dugite accepted only the
    // definite form for nested maps/lists/text (and, in the Shelley and Conway
    // decoders, for nested byte strings), so an on-chain transaction using the
    // indefinite form failed to decode where cardano-node accepts it.
    // -----------------------------------------------------------------------

    use dugite_primitives::transaction::TransactionMetadatum as M;

    fn md(bytes: &[u8]) -> Result<M, SerializationError> {
        read_metadatum(&mut Reader::new(bytes))
    }

    /// Every indefinite form decodes to exactly the value its definite
    /// counterpart does — the whole point of the fix.
    #[test]
    fn indefinite_forms_agree_with_definite() {
        // {"a": 1}  definite 0xa1 vs indefinite 0xbf..0xff
        let def = [0xa1, 0x61, b'a', 0x01];
        let indef = [0xbf, 0x61, b'a', 0x01, 0xff];
        assert_eq!(md(&def).unwrap(), md(&indef).unwrap());
        assert_eq!(
            md(&indef).unwrap(),
            M::Map(vec![(M::Text("a".into()), M::Int(1))])
        );

        // [1, 2]  definite 0x82 vs indefinite 0x9f..0xff
        let def = [0x82, 0x01, 0x02];
        let indef = [0x9f, 0x01, 0x02, 0xff];
        assert_eq!(md(&def).unwrap(), md(&indef).unwrap());
        assert_eq!(md(&indef).unwrap(), M::List(vec![M::Int(1), M::Int(2)]));

        // h'AABBCC'  definite 0x43 vs chunked indefinite 0x5f..0xff
        let def = [0x43, 0xaa, 0xbb, 0xcc];
        let indef = [0x5f, 0x42, 0xaa, 0xbb, 0x41, 0xcc, 0xff];
        assert_eq!(md(&def).unwrap(), md(&indef).unwrap());
        assert_eq!(md(&indef).unwrap(), M::Bytes(vec![0xaa, 0xbb, 0xcc]));

        // "hithx"  definite 0x65 vs chunked indefinite 0x7f..0xff
        let def = [0x65, b'h', b'i', b't', b'h', b'x'];
        let indef = [0x7f, 0x62, b'h', b'i', 0x63, b't', b'h', b'x', 0xff];
        assert_eq!(md(&def).unwrap(), md(&indef).unwrap());
        assert_eq!(md(&indef).unwrap(), M::Text("hithx".into()));
    }

    /// Indefinite chunks concatenate in **wire order**, per
    /// `decodeBytesIndefLen` / `decodeStringIndefLen`.
    #[test]
    fn indefinite_chunks_concatenate_in_wire_order() {
        let indef = [0x5f, 0x41, 0x01, 0x41, 0x02, 0x41, 0x03, 0xff];
        assert_eq!(md(&indef).unwrap(), M::Bytes(vec![0x01, 0x02, 0x03]));

        let indef = [0x7f, 0x61, b'a', 0x61, b'b', 0x61, b'c', 0xff];
        assert_eq!(md(&indef).unwrap(), M::Text("abc".into()));
    }

    /// Empty indefinite containers are legal (immediate break).
    #[test]
    fn empty_indefinite_containers() {
        assert_eq!(md(&[0xbf, 0xff]).unwrap(), M::Map(vec![]));
        assert_eq!(md(&[0x9f, 0xff]).unwrap(), M::List(vec![]));
        assert_eq!(md(&[0x5f, 0xff]).unwrap(), M::Bytes(vec![]));
        assert_eq!(md(&[0x7f, 0xff]).unwrap(), M::Text(String::new()));
    }

    /// The indefinite forms nest arbitrarily deep and mix freely with the
    /// definite ones — the recursion goes back through `read_metadatum`.
    #[test]
    fn deeply_nested_mixed_indefinite() {
        // {"k": [ h'AA', {"z": "yy"} ]} with the outer map, the list and the
        // inner byte/text strings all indefinite, inner map definite.
        let data = [
            0xbf, 0x61, b'k', // {"k":
            0x9f, // [
            0x5f, 0x41, 0xaa, 0xff, // h'AA' (indef)
            0xa1, 0x61, b'z', 0x7f, 0x62, b'y', b'y', 0xff, // {"z": "yy" (indef)}
            0xff, // ]
            0xff, // }
        ];
        assert_eq!(
            md(&data).unwrap(),
            M::Map(vec![(
                M::Text("k".into()),
                M::List(vec![
                    M::Bytes(vec![0xaa]),
                    M::Map(vec![(M::Text("z".into()), M::Text("yy".into()))]),
                ])
            )])
        );
    }

    /// Haskell's `decodeMetadatum` falls through to `decodeError` for
    /// `TypeTag`, so a bignum-tagged integer is NOT a valid metadatum even
    /// though it denotes an integer. dugite must reject it too.
    #[test]
    fn tagged_values_rejected_like_haskell() {
        // tag(2) h'0100000000000000000000' — a positive bignum.
        let bignum = [0xc2, 0x4b, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(md(&bignum).is_err());
        // tag(24) h'01' — CBOR-in-CBOR.
        assert!(md(&[0xd8, 0x18, 0x41, 0x01]).is_err());
    }

    /// Other non-metadatum tokens stay rejected (bool / null / float).
    #[test]
    fn non_metadatum_tokens_rejected() {
        assert!(md(&[0xf5]).is_err()); // true
        assert!(md(&[0xf6]).is_err()); // null
        assert!(md(&[0xfb, 0, 0, 0, 0, 0, 0, 0, 0]).is_err()); // float64
    }

    /// A nested indefinite string whose chunks are themselves indefinite is
    /// rejected, matching `decodeStringIndefLen`'s use of plain `decodeString`.
    #[test]
    fn indefinite_string_chunks_must_be_definite() {
        let data = [0x7f, 0x7f, 0x61, b'a', 0xff, 0xff];
        assert!(md(&data).is_err());
    }

    /// Integers keep working across both signs and all width encodings.
    #[test]
    fn integers_both_signs() {
        assert_eq!(md(&[0x01]).unwrap(), M::Int(1));
        assert_eq!(md(&[0x20]).unwrap(), M::Int(-1));
        assert_eq!(md(&[0x1a, 0x00, 0x01, 0x00, 0x00]).unwrap(), M::Int(65536));
        assert_eq!(
            md(&[0x3b, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
            M::Int(-1_i128)
        );
    }

    /// The top-level `{ word64 => metadatum }` map accepts both header forms,
    /// and its values go through the same recursive decoder.
    #[test]
    fn metadata_map_accepts_both_header_forms() {
        // definite {674: {"msg": "hi"}}
        let def = [
            0xa1, 0x19, 0x02, 0xa2, 0xa1, 0x63, b'm', b's', b'g', 0x62, b'h', b'i',
        ];
        // indefinite outer AND indefinite inner map
        let indef = [
            0xbf, 0x19, 0x02, 0xa2, 0xbf, 0x63, b'm', b's', b'g', 0x62, b'h', b'i', 0xff, 0xff,
        ];
        let a = read_metadata_map(&mut Reader::new(&def)).unwrap();
        let b = read_metadata_map(&mut Reader::new(&indef)).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            b.get(&674),
            Some(&M::Map(vec![(M::Text("msg".into()), M::Text("hi".into()))]))
        );
    }
}
