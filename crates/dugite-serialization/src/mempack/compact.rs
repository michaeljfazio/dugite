//! Low-level decoders for MemPack compact representations.
//!
//! ## VarLen encoding
//!
//! MemPack uses a 7-bit variable-length unsigned integer encoding that is
//! **MSB-first** (big-endian base-128), matching the Haskell `mempack` library
//! in `Data.MemPack` (see `packIntoCont7` / `unpack7BitVarLen` in
//! <https://github.com/lehins/mempack/blob/master/src/Data/MemPack.hs>).
//!
//! On the wire, the value is split into 7-bit groups starting with the most
//! significant group first. Every byte except the last has its top bit set
//! (continuation marker); the final byte has its top bit clear. Decoding shifts
//! the accumulator left by 7 and ORs in the next 7 payload bits each step:
//!
//! ```text
//! acc = 0
//! loop:
//!   b = next byte
//!   acc = (acc << 7) | (b & 0x7f)
//!   if b & 0x80 == 0 break
//! ```
//!
//! This is **NOT** protobuf-style LSB-first varint. The distinction matters:
//! for example `[0xee, 0xdd, 0x01]` decodes to `1_814_145` (MSB-first), not
//! `28_398` (LSB-first).
//!
//! ## CompactAddr
//!
//! ```text
//! VarLen(address_byte_length) + raw_address_bytes
//! ```
//!
//! ## CompactValue
//!
//! ```text
//! tag(0) + VarLen(lovelace)                 — ADA-only
//! tag(1) + VarLen(lovelace) + multi-asset   — multi-asset (opaque bytes for now)
//! ```

use crate::error::SerializationError;

/// Maximum number of bytes we allow for a single VarLen integer. 10 bytes
/// covers the full u64 range (`ceil(64/7) = 10`).
const MAX_VARLEN_BYTES: usize = 10;

/// Decode a MemPack VarLen-encoded unsigned integer (MSB-first base-128).
///
/// Returns `(value, bytes_consumed)`. Errors if the encoding is truncated or
/// exceeds 10 bytes without a terminating byte.
pub fn decode_varlen(data: &[u8]) -> Result<(u64, usize), SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode("varlen: empty input".into()));
    }

    let mut acc: u64 = 0;
    for (i, &byte) in data.iter().take(MAX_VARLEN_BYTES).enumerate() {
        // Shift existing bits up by 7 and OR in the lower 7 bits of this byte.
        acc = (acc << 7) | ((byte & 0x7f) as u64);
        if byte & 0x80 == 0 {
            return Ok((acc, i + 1));
        }
    }

    Err(SerializationError::CborDecode(
        "varlen: exceeded maximum length without termination".into(),
    ))
}

/// Decode a CompactAddr: `VarLen(length) + raw_address_bytes`.
///
/// Returns `(address_bytes, total_bytes_consumed)`.
pub fn decode_compact_addr(data: &[u8]) -> Result<(Vec<u8>, usize), SerializationError> {
    let (addr_len, len_bytes) = decode_varlen(data)?;
    let addr_len = addr_len as usize;
    let total = len_bytes + addr_len;
    if data.len() < total {
        return Err(SerializationError::CborDecode(format!(
            "compact_addr: need {total} bytes, have {}",
            data.len()
        )));
    }
    let addr = data[len_bytes..total].to_vec();
    Ok((addr, total))
}

/// Result of decoding a CompactValue.
#[derive(Debug, Clone)]
pub struct CompactValueDecoded {
    /// Lovelace amount.
    pub coin: u64,
    /// For multi-asset values (tag 1), the raw multi-asset bytes that follow the
    /// coin VarLen.  For ADA-only values (tag 0) this is `None`.
    pub multi_asset_raw: Option<Vec<u8>>,
    /// For multi-asset values (tag 1), the `numMA` count (number of distinct
    /// `(policy, asset)` pairs) parsed from the `CompactValue` header. Needed to
    /// split the `rep` ShortByteString into triples. `0` for ADA-only values and
    /// for the opaque [`decode_compact_value`] path (which does not parse it).
    pub num_assets: u64,
    /// Total bytes consumed from the input slice.
    pub consumed: usize,
}

/// Decode a CompactValue.
///
/// `remaining_len` is the number of bytes available for the *entire* CompactValue
/// plus any trailing data in the same TxOut blob.  When the value is ADA-only
/// (tag 0), the coin VarLen is the only field and we consume exactly those bytes.
/// When the value is multi-asset (tag 1), we consume `VarLen(coin)` and then
/// **all remaining bytes up to `total_remaining`** are stored as opaque
/// multi-asset data (the caller is responsible for further subdivision if needed).
///
/// If `total_remaining` is `None`, we consume only the coin VarLen (useful when
/// the caller knows the exact extent of the CompactValue independently).
pub fn decode_compact_value(
    data: &[u8],
    total_remaining: Option<usize>,
) -> Result<CompactValueDecoded, SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode(
            "compact_value: empty input".into(),
        ));
    }

    let tag = data[0];
    let mut off = 1usize;

    // Decode VarLen(coin).
    let (coin, n) = decode_varlen(&data[off..])?;
    off += n;

    match tag {
        0 => {
            // ADA-only.
            Ok(CompactValueDecoded {
                coin,
                multi_asset_raw: None,
                num_assets: 0,
                consumed: off,
            })
        }
        1 => {
            // Multi-asset. The bytes after the coin VarLen up to the end of the
            // allocated slice represent the multi-asset payload.
            let end = total_remaining.unwrap_or(off);
            let ma = if end > off {
                Some(data[off..end].to_vec())
            } else {
                None
            };
            Ok(CompactValueDecoded {
                coin,
                multi_asset_raw: ma,
                // This opaque path does not parse the numMA/rep split; callers
                // that need triples must use `decode_compact_value_exact`.
                num_assets: 0,
                consumed: end,
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "compact_value: unknown tag {other}"
        ))),
    }
}

/// Decode a CompactValue, fully parsing the multi-asset payload so the exact
/// number of bytes consumed is known.
///
/// Unlike [`decode_compact_value`] (which treats multi-asset bytes as opaque
/// "everything to the end of the blob"), this matches the byte-exact MemPack
/// `instance MemPack CompactValue` from cardano-ledger
/// (`eras/mary/impl/src/Cardano/Ledger/Mary/Value.hs`):
///
/// ```text
/// CompactValueAdaOnly     c       → packTagM 0 >> packM (VarLen c)
/// CompactValueMultiAsset  c n rep → packTagM 1 >> packM (VarLen c)
///                                        >> packM (VarLen n)
///                                        >> packM rep      -- ShortByteString
/// ```
///
/// `rep` is a `ShortByteString`, so it is itself serialized as
/// `VarLen(rep_len) ‖ rep_bytes`. This lets us recover the exact extent of the
/// value field even when it is followed by a Datum + Script tail (tag-5
/// `TxOutCompactRefScript`).
pub fn decode_compact_value_exact(data: &[u8]) -> Result<CompactValueDecoded, SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode(
            "compact_value_exact: empty input".into(),
        ));
    }

    let tag = data[0];
    let mut off = 1usize;

    // VarLen(coin).
    let (coin, n) = decode_varlen(&data[off..])?;
    off += n;

    match tag {
        0 => Ok(CompactValueDecoded {
            coin,
            multi_asset_raw: None,
            num_assets: 0,
            consumed: off,
        }),
        1 => {
            // VarLen(numMA) — Word32 count of distinct (policy, asset) pairs.
            let (num_ma, n_num) = decode_varlen(&data[off..])?;
            off += n_num;
            // rep : ShortByteString = VarLen(rep_len) ‖ rep_bytes.
            let (rep_len, n_rep_len) = decode_varlen(&data[off..])?;
            off += n_rep_len;
            let rep_len = rep_len as usize;
            let rep_start = off;
            let rep_end = off.checked_add(rep_len).ok_or_else(|| {
                SerializationError::CborDecode("compact_value_exact: rep length overflow".into())
            })?;
            if rep_end > data.len() {
                return Err(SerializationError::CborDecode(format!(
                    "compact_value_exact: multi-asset rep needs {rep_end} bytes, have {}",
                    data.len()
                )));
            }
            Ok(CompactValueDecoded {
                coin,
                multi_asset_raw: Some(data[rep_start..rep_end].to_vec()),
                num_assets: num_ma,
                consumed: rep_end,
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "compact_value_exact: unknown tag {other}"
        ))),
    }
}

/// A single decoded multi-asset entry: `(policy_id_28, asset_name, quantity)`.
pub type MultiAssetEntry = ([u8; 28], Vec<u8>, u64);

/// Parse a `CompactValue` multi-asset `rep` `ShortByteString` into its
/// `(PolicyID, AssetName, Quantity)` triples.
///
/// Byte-exact port of the cardano-ledger `CompactValue` representation
/// (`eras/mary/impl/src/Cardano/Ledger/Mary/Value.hs`). The `rep` is five
/// concatenated regions:
///
/// ```text
/// A: numMA × Word64   asset quantities
/// B: numMA × Word16   policyId offsets   (byte offsets within the whole rep, into D)
/// C: numMA × Word16   asset-name offsets (byte offsets within the whole rep, into E)
/// D: concatenated, de-duplicated 28-byte policyIds
/// E: concatenated, sorted, de-duplicated asset names
/// ```
///
/// Crucially the asset-name **length is not stored**: names are sorted by their
/// offset, and a name's length is the gap to the next *distinct* offset (or the
/// end of the rep for the last one). See the `from`/`assetLens` code in the
/// upstream module.
///
/// Endianness: the Word64/Word16 cells are read with `BA.indexByteArray` in
/// Haskell (host-native), and the `rep` `ShortByteString` is MemPack-packed
/// verbatim, so on x86_64/aarch64 the cells are **little-endian** on disk.
/// (This was verified empirically against preprod `00000c0c…#1`, whose rep
/// decodes to the 10 NFT_480..NFT_489 assets reported by Koios.)
///
/// Order is NOT canonicalised here; the caller folds the triples into a
/// `BTreeMap`-backed `Value`, which sorts deterministically.
pub fn parse_multi_asset_rep(
    rep: &[u8],
    num_ma: usize,
) -> Result<Vec<MultiAssetEntry>, SerializationError> {
    if num_ma == 0 {
        return Ok(Vec::new());
    }
    // A(8·n) + B(2·n) + C(2·n) = 12·n bytes of header before regions D and E.
    let abc = num_ma
        .checked_mul(12)
        .ok_or_else(|| SerializationError::CborDecode("multi-asset rep: numMA overflow".into()))?;
    if rep.len() < abc {
        return Err(SerializationError::CborDecode(format!(
            "multi-asset rep: need >= {abc} bytes for A/B/C regions, have {}",
            rep.len()
        )));
    }

    let q_at = |i: usize| u64::from_le_bytes(rep[8 * i..8 * i + 8].try_into().unwrap());
    let pidoff_at = |i: usize| {
        u16::from_le_bytes(
            rep[8 * num_ma + 2 * i..8 * num_ma + 2 * i + 2]
                .try_into()
                .unwrap(),
        )
    };
    let anoff_at = |i: usize| {
        u16::from_le_bytes(
            rep[8 * num_ma + 2 * num_ma + 2 * i..8 * num_ma + 2 * num_ma + 2 * i + 2]
                .try_into()
                .unwrap(),
        )
    };

    // Asset-name length = distance to the next distinct offset (or end of rep).
    let mut distinct: Vec<usize> = (0..num_ma).map(|i| anoff_at(i) as usize).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let name_len = |off: usize| -> usize {
        match distinct.binary_search(&off) {
            Ok(idx) => {
                let end = distinct.get(idx + 1).copied().unwrap_or(rep.len());
                end.saturating_sub(off)
            }
            // An asset-name offset must be one of the distinct offsets; treat a
            // miss defensively as a zero-length name.
            Err(_) => 0,
        }
    };

    let mut out = Vec::with_capacity(num_ma);
    for i in 0..num_ma {
        let pid_off = pidoff_at(i) as usize;
        let an_off = anoff_at(i) as usize;
        let pid_end = pid_off.checked_add(28).ok_or_else(|| {
            SerializationError::CborDecode("multi-asset rep: pid offset overflow".into())
        })?;
        if pid_end > rep.len() {
            return Err(SerializationError::CborDecode(format!(
                "multi-asset rep: policyId at {pid_off} runs past end {}",
                rep.len()
            )));
        }
        let alen = name_len(an_off);
        let an_end = an_off.checked_add(alen).ok_or_else(|| {
            SerializationError::CborDecode("multi-asset rep: name offset overflow".into())
        })?;
        if an_end > rep.len() {
            return Err(SerializationError::CborDecode(format!(
                "multi-asset rep: asset name at {an_off} (+{alen}) runs past end {}",
                rep.len()
            )));
        }
        let mut pid = [0u8; 28];
        pid.copy_from_slice(&rep[pid_off..pid_end]);
        let name = rep[an_off..an_end].to_vec();
        out.push((pid, name, q_at(i)));
    }
    Ok(out)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_decode_varlen_small() {
        // Single-byte encodings (no continuation bit): value = byte.
        assert_eq!(decode_varlen(&[0]).unwrap(), (0, 1));
        assert_eq!(decode_varlen(&[1]).unwrap(), (1, 1));
        assert_eq!(decode_varlen(&[29]).unwrap(), (29, 1));
        assert_eq!(decode_varlen(&[127]).unwrap(), (127, 1));
    }

    #[test]
    fn test_decode_varlen_multi_byte_msb_first() {
        // MSB-first: first byte is the most significant 7 bits.
        //
        // 128 = (1 << 7) | 0  →  [0x81, 0x00]
        assert_eq!(decode_varlen(&[0x81, 0x00]).unwrap(), (128, 2));
        // 150 = (1 << 7) | 22 →  [0x81, 0x16]
        assert_eq!(decode_varlen(&[0x81, 0x16]).unwrap(), (150, 2));
        // 300 = (2 << 7) | 44 →  [0x82, 0x2c]
        assert_eq!(decode_varlen(&[0x82, 0x2c]).unwrap(), (300, 2));
    }

    #[test]
    fn test_decode_varlen_three_bytes_fixture() {
        // 1_814_145 = 0xee 0xdd 0x01 (real coin value from preview tvar,
        // cross-checked against Koios: tx
        // 00002435e40d68a58b5130644c845c05fa8e36e3935a905f718e6fa611f0304a#2
        // → value 1_814_145).
        //
        //   0xee → acc = 0 << 7 | 0x6e = 110
        //   0xdd → acc = 110 << 7 | 0x5d = 14_173
        //   0x01 → acc = 14_173 << 7 | 0x01 = 1_814_145
        assert_eq!(decode_varlen(&[0xee, 0xdd, 0x01]).unwrap(), (1_814_145, 3));
    }

    #[test]
    fn test_decode_varlen_max_u64() {
        // u64::MAX = 2^64 - 1. In 7-bit MSB-first, that is 10 bytes:
        //   [0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]
        // The first byte encodes the single leading bit (2^63), subsequent
        // bytes contribute 7 bits each.
        let bytes = [0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(decode_varlen(&bytes).unwrap(), (u64::MAX, 10));
    }

    #[test]
    fn test_decode_varlen_empty() {
        assert!(decode_varlen(&[]).is_err());
    }

    #[test]
    fn test_decode_varlen_truncated() {
        // Continuation bit set but no more bytes.
        assert!(decode_varlen(&[0x80]).is_err());
    }

    #[test]
    fn test_decode_compact_addr() {
        // addr_len = 29, then 29 bytes of address data.
        let mut data = vec![29u8]; // VarLen(29), single byte
        data.extend_from_slice(&[0x60; 29]); // 29 dummy address bytes
        let (addr, consumed) = decode_compact_addr(&data).unwrap();
        assert_eq!(addr.len(), 29);
        assert_eq!(consumed, 30);
    }

    #[test]
    fn test_decode_compact_value_ada_only() {
        // tag=0, coin VarLen = 1_814_145 (MSB-first [0xee, 0xdd, 0x01]).
        let data = [0x00, 0xee, 0xdd, 0x01];
        let result = decode_compact_value(&data, None).unwrap();
        assert_eq!(result.coin, 1_814_145);
        assert!(result.multi_asset_raw.is_none());
        assert_eq!(result.consumed, 4);
    }

    #[test]
    fn test_decode_compact_value_multi_asset() {
        // tag=1, coin VarLen (3 bytes, MSB-first), then 5 bytes of multi-asset.
        //
        //   0xd8 0xb1 0x60 → ((0x58 << 14) | (0x31 << 7) | 0x60)
        //                  =  1_450_144 + 6_272 + 96
        //                  wait — MSB-first:
        //     0xd8 (cont) → acc = 0x58 = 88
        //     0xb1 (cont) → acc = 88<<7 | 0x31 = 11_264 + 49 = 11_313
        //     0x60 (stop) → acc = 11_313<<7 | 0x60 = 1_448_064 + 96 = 1_448_160
        let data = [0x01, 0xd8, 0xb1, 0x60, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let result = decode_compact_value(&data, Some(data.len())).unwrap();
        assert_eq!(result.coin, 1_448_160);
        let ma = result.multi_asset_raw.unwrap();
        assert_eq!(ma, &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    }

    #[test]
    fn test_parse_multi_asset_rep_empty() {
        assert!(parse_multi_asset_rep(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn test_parse_multi_asset_rep_real_preprod_nft_bundle() {
        // The exact multi-asset `rep` ShortByteString from preprod
        // 00000c0cf6fe6389492dd7fe7c8ff3040d70d11b3356093cf651ac876c6f66d9#1
        // (numMA = 10). Cross-checked against preprod Koios asset_list: 10 NFTs
        // NFT_480..NFT_489, policy f1efa1875fc86249b86bdd726dc72f63ec94e15ba1b1285559bb1d25,
        // quantity 1 each. This is the byte-exact oracle for the rep parser.
        let rep = hex::decode(
            "0100000000000000010000000000000001000000000000000100000000000000\
             0100000000000000010000000000000001000000000000000100000000000000\
             0100000000000000010000000000000078007800780078007800780078007800\
             7800780094009b00a200a900b000b700be00c500cc00d300f1efa1875fc86249\
             b86bdd726dc72f63ec94e15ba1b1285559bb1d254e46545f3438394e46545f34\
             38384e46545f3438374e46545f3438364e46545f3438354e46545f3438344e46\
             545f3438334e46545f3438324e46545f3438314e46545f343830",
        )
        .unwrap();
        let triples = parse_multi_asset_rep(&rep, 10).unwrap();
        assert_eq!(triples.len(), 10);
        let policy =
            hex::decode("f1efa1875fc86249b86bdd726dc72f63ec94e15ba1b1285559bb1d25").unwrap();
        // Fold into a sorted (policy, name)->qty map for deterministic assertions.
        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        for (pid, name, qty) in &triples {
            assert_eq!(&pid[..], &policy[..], "all assets share one policy");
            *by_name.entry(name.clone()).or_default() += qty;
        }
        assert_eq!(by_name.len(), 10);
        for n in 480u32..=489 {
            let name = format!("NFT_{n}").into_bytes();
            assert_eq!(
                by_name.get(&name).copied(),
                Some(1),
                "missing/wrong qty for NFT_{n}"
            );
        }
    }

    #[test]
    fn test_parse_multi_asset_rep_truncated_header_errors() {
        // numMA says 2 (needs >= 24 header bytes) but rep is short.
        assert!(parse_multi_asset_rep(&[0u8; 10], 2).is_err());
    }
}
