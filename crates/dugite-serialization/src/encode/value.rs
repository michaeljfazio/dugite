use crate::cbor::*;
use dugite_primitives::hash::Hash28;
use dugite_primitives::value::{AssetName, Value};
use std::collections::BTreeMap;

// The Haskell `encodeMap` helpers (`encode_map_open`/`encode_map_close`,
// threshold `ENCODE_MAP_DEFINITE_MAX = 23`) were introduced here for issue
// #930 and promoted to `crate::cbor` for issue #932 so every encodeMap-shaped
// site in the encoder tree shares ONE implementation. They arrive via the
// `use crate::cbor::*` glob above.

/// Encode a Value to CBOR.
///
/// Pure ADA: just the coin amount.
/// Multi-asset: [coin, {policy_id: {asset_name: quantity}, ...}]
///
/// The outer `[coin, multiasset]` wrapper is ALWAYS a definite-length
/// array(2) — Haskell encodes it via `Rec MaryValue` which always emits
/// `encodeListLen 2`. The multi-asset maps inside follow Haskell
/// `encodeMap` semantics (definite <= 23 entries, indefinite above; issue
/// #930) — see [`encode_multi_asset`]. This byte-exact alignment is what
/// Rule 5a (`maxValSize`) measures against, and matches cardano-api's wire
/// output for synthetic re-encodes.
pub fn encode_value(value: &Value) -> Vec<u8> {
    if value.is_pure_ada() {
        encode_uint(value.coin.0)
    } else {
        let mut buf = encode_array_header(2);
        buf.extend(encode_uint(value.coin.0));
        buf.extend(encode_multi_asset(&value.multi_asset));
        buf
    }
}

/// Encode multi-asset map: {policy_id: {asset_name: quantity}}
///
/// Both map levels follow Haskell cardano-ledger-binary `encodeMap`
/// (issue #930): definite-length header for <= 23 entries, indefinite
/// (`0xbf` ... `0xff`) for > 23 — the two levels switch independently.
pub(crate) fn encode_multi_asset(
    multi_asset: &BTreeMap<Hash28, BTreeMap<AssetName, u64>>,
) -> Vec<u8> {
    let mut buf = encode_map_open(multi_asset.len());
    for (policy_id, assets) in multi_asset {
        buf.extend(encode_hash28(policy_id));
        buf.extend(encode_map_open(assets.len()));
        for (asset_name, qty) in assets {
            buf.extend(encode_bytes(&asset_name.0));
            buf.extend(encode_uint(*qty));
        }
        encode_map_close(&mut buf, assets.len());
    }
    encode_map_close(&mut buf, multi_asset.len());
    buf
}

/// Encode mint map: {policy_id: {asset_name: i64}}
///
/// Same Haskell `encodeMap` semantics as [`encode_multi_asset`] (issue
/// #930): Haskell's `EncCBOR MultiAsset` instance is shared by the mint
/// field, so the definite/indefinite threshold applies here too.
pub(crate) fn encode_mint(mint: &BTreeMap<Hash28, BTreeMap<AssetName, i64>>) -> Vec<u8> {
    let mut buf = encode_map_open(mint.len());
    for (policy_id, assets) in mint {
        buf.extend(encode_hash28(policy_id));
        buf.extend(encode_map_open(assets.len()));
        for (asset_name, qty) in assets {
            buf.extend(encode_bytes(&asset_name.0));
            buf.extend(encode_int(*qty as i128));
        }
        encode_map_close(&mut buf, assets.len());
    }
    encode_map_close(&mut buf, mint.len());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::value::Lovelace;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Build a 28-byte Hash28 from a repeating seed byte (deterministic, cheap).
    fn policy(seed: u8) -> Hash28 {
        Hash28::from_bytes([seed; 28])
    }

    /// Build an AssetName from a byte slice.
    fn asset(name: &[u8]) -> AssetName {
        AssetName(name.to_vec())
    }

    // ---------------------------------------------------------------------------
    // encode_value — pure ADA
    // ---------------------------------------------------------------------------

    /// ADA-only value must encode as a bare CBOR uint (not wrapped in an array).
    #[test]
    fn test_encode_value_pure_ada() {
        let v = Value::lovelace(1_000_000);
        let enc = encode_value(&v);
        // 1_000_000 = 0x0F4240  → 0x1a 0x00 0x0f 0x42 0x40
        assert_eq!(enc, vec![0x1a, 0x00, 0x0f, 0x42, 0x40]);
        // Must NOT start with an array header (0x82)
        assert_ne!(enc[0], 0x82, "pure-ADA value must not be wrapped in array");
    }

    /// ADA-only zero value encodes as CBOR uint 0.
    #[test]
    fn test_encode_value_zero_ada() {
        let v = Value::lovelace(0);
        let enc = encode_value(&v);
        assert_eq!(enc, vec![0x00]);
    }

    // ---------------------------------------------------------------------------
    // encode_value — multi-asset
    // ---------------------------------------------------------------------------

    /// Multi-asset value encodes as CBOR array(2): [coin, {policy: {asset: qty}}].
    #[test]
    fn test_encode_value_multi_asset_structure() {
        let mut v = Value::lovelace(2_000_000);
        let mut assets = BTreeMap::new();
        assets.insert(asset(b"tokenA"), 100u64);
        v.multi_asset.insert(policy(0x01), assets);

        let enc = encode_value(&v);

        // First byte: array(2) = 0x82
        assert_eq!(enc[0], 0x82, "multi-asset value must start with array(2)");

        // Coin: 2_000_000 = 0x1E8480
        // 0x1a 0x00 0x1e 0x84 0x80
        assert_eq!(&enc[1..6], &[0x1a, 0x00, 0x1e, 0x84, 0x80], "coin mismatch");

        // Multi-asset map header: map(1) = 0xa1
        assert_eq!(enc[6], 0xa1, "multi-asset outer map must have 1 entry");
    }

    /// Multi-asset encoding with two policies, each with one asset.
    #[test]
    fn test_encode_value_two_policies() {
        let mut v = Value::lovelace(0);
        let mut assets_a = BTreeMap::new();
        assets_a.insert(asset(b"A"), 1u64);
        let mut assets_b = BTreeMap::new();
        assets_b.insert(asset(b"B"), 2u64);
        // BTreeMap is ordered by key, so policy(0x01) < policy(0x02)
        v.multi_asset.insert(policy(0x01), assets_a);
        v.multi_asset.insert(policy(0x02), assets_b);

        let enc = encode_value(&v);

        // array(2)
        assert_eq!(enc[0], 0x82);
        // coin = 0
        assert_eq!(enc[1], 0x00);
        // outer map(2) = 0xa2
        assert_eq!(enc[2], 0xa2, "outer map must report 2 policies");
    }

    /// Multi-asset value with an empty inner asset map still encodes correctly.
    #[test]
    fn test_encode_value_empty_multi_asset_map() {
        let mut v = Value::lovelace(500);
        // Insert a policy with zero assets (degenerate but must not panic)
        v.multi_asset.insert(policy(0xAA), BTreeMap::new());

        let enc = encode_value(&v);

        // array(2) header
        assert_eq!(enc[0], 0x82);
        // outer map(1) = 0xa1
        // offset: coin is 500 = 0x19 0x01 0xf4 → 3 bytes
        assert_eq!(enc[4], 0xa1, "outer map must have 1 policy entry");
        // inner map(0) = 0xa0 — immediately after the 30-byte Hash28 header+body
        // Hash28 header: 0x58 0x1c = 2 bytes, then 28 bytes = 30 bytes after outer-map byte
        let inner_map_offset = 4 + 1 + 2 + 28; // outer_map + policy_header(2) + policy_bytes(28)
        assert_eq!(
            enc[inner_map_offset], 0xa0,
            "empty inner asset map must encode as map(0)"
        );
    }

    // ---------------------------------------------------------------------------
    // encode_multi_asset
    // ---------------------------------------------------------------------------

    /// encode_multi_asset with a single policy, two assets encodes both correctly.
    #[test]
    fn test_encode_multi_asset_two_assets_per_policy() {
        let mut multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = BTreeMap::new();
        let mut assets = BTreeMap::new();
        // Use short names so lengths are predictable
        assets.insert(asset(b"x"), 10u64);
        assets.insert(asset(b"y"), 20u64);
        multi.insert(policy(0x05), assets);

        let enc = encode_multi_asset(&multi);

        // outer map(1) = 0xa1
        assert_eq!(enc[0], 0xa1);
        // policy Hash28: 0x58 0x1c ...
        assert_eq!(enc[1], 0x58);
        assert_eq!(enc[2], 28);
        // inner map(2) = 0xa2, located at byte 1 + 30 = 31
        assert_eq!(enc[31], 0xa2, "inner map must have 2 asset entries");
    }

    // ---------------------------------------------------------------------------
    // encode_mint — negative quantities
    // ---------------------------------------------------------------------------

    /// encode_mint with a negative quantity must use CBOR negative integer encoding.
    #[test]
    fn test_encode_mint_negative_quantity() {
        let mut mint: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        let mut assets = BTreeMap::new();
        assets.insert(asset(b"burn"), -500i64);
        mint.insert(policy(0x10), assets);

        let enc = encode_mint(&mint);

        // outer map(1) = 0xa1
        assert_eq!(enc[0], 0xa1);

        // After policy bytes (1+30=31 bytes) we have:
        // inner map(1) = 0xa1
        assert_eq!(enc[31], 0xa1);

        // asset name b"burn" (4 bytes) → encode_bytes: 0x44 + 4 bytes = 5 bytes
        // at offset 32
        assert_eq!(enc[32], 0x44, "asset name should be 4-byte bytestring");

        // quantity -500:  -(500) - 1 = 499 = 0x01F3 → 0x39 0x01 0xf3
        let qty_offset = 32 + 1 + 4; // map_header(1) + bytestr_header(1) + "burn"(4)
        assert_eq!(
            enc[qty_offset], 0x39,
            "negative qty must use 2-byte CBOR negative"
        );
        assert_eq!(enc[qty_offset + 1], 0x01);
        assert_eq!(enc[qty_offset + 2], 0xf3);
    }

    /// encode_mint with a positive quantity uses normal uint encoding.
    #[test]
    fn test_encode_mint_positive_quantity() {
        let mut mint: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        let mut assets = BTreeMap::new();
        assets.insert(asset(b"mint"), 42i64);
        mint.insert(policy(0x20), assets);

        let enc = encode_mint(&mint);

        // quantity 42 → 0x18 0x2a  (one-byte uint)
        // offset: 0xa1 + policy(30) + 0xa1 + name_bytes(5 for "mint") = 37
        let qty_offset = 1 + 30 + 1 + 1 + 4;
        assert_eq!(
            enc[qty_offset], 0x18,
            "positive qty 42 should use 0x18 prefix"
        );
        assert_eq!(enc[qty_offset + 1], 42);
    }

    /// encode_mint with multiple assets per policy encodes all of them.
    #[test]
    fn test_encode_mint_multiple_assets_per_policy() {
        let mut mint: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        let mut assets = BTreeMap::new();
        assets.insert(asset(b"a"), 1i64);
        assets.insert(asset(b"b"), -1i64);
        assets.insert(asset(b"c"), 0i64);
        mint.insert(policy(0x30), assets);

        let enc = encode_mint(&mint);

        // inner map must have 3 entries: 0xa3
        assert_eq!(enc[31], 0xa3, "inner map must have 3 asset entries");
    }

    // ---------------------------------------------------------------------------
    // Roundtrip length sanity checks
    // ---------------------------------------------------------------------------

    // ---------------------------------------------------------------------------
    // #930 — Haskell `encodeMap` variable-length header semantics
    //
    // cardano-ledger-binary `encodeMap` (encoding version >= 2, i.e. every
    // Shelley+ era) emits a DEFINITE-length map header for maps with <= 23
    // entries and an INDEFINITE-length map (0xbf ... 0xff) for > 23 entries —
    // independently at the outer policy-map level and every inner
    // asset-name-map level. A definite-only encoder is byte-identical for
    // 0-23 entries, same total length for 24-255 (2-byte header vs
    // 0xbf+0xff), and 1 byte LONGER for 256-65535 — which made Rule 5a
    // falsely reject preprod tx 96ae78f7 (5001 vs Haskell's 5000).
    //
    // Byte math below: every asset entry uses a 1-byte name (0x41 nn) and
    // qty 1 (0x01) = 3 bytes; every policy entry is 0x58 0x1c + 28 = 30
    // bytes.
    // ---------------------------------------------------------------------------

    /// Build an inner asset map with `n` (<= 256) distinct 1-byte names, qty 1.
    fn assets_n(n: usize) -> BTreeMap<AssetName, u64> {
        assert!(n <= 256);
        (0..n).map(|i| (asset(&[i as u8]), 1u64)).collect()
    }

    /// Same shape for mint maps (i64 quantities).
    fn mint_assets_n(n: usize) -> BTreeMap<AssetName, i64> {
        assert!(n <= 256);
        (0..n).map(|i| (asset(&[i as u8]), 1i64)).collect()
    }

    /// Inner asset map with 23 entries stays a DEFINITE 1-byte header (0xb7).
    #[test]
    fn multi_asset_inner_map_23_entries_definite_header() {
        let mut multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = BTreeMap::new();
        multi.insert(policy(0x01), assets_n(23));
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[0], 0xa1, "outer map(1) must be definite");
        // inner header at offset 1 (outer) + 30 (policy) = 31
        assert_eq!(enc[31], 0xb7, "23-entry inner map must be definite map(23)");
        assert_ne!(*enc.last().unwrap(), 0xff, "no break byte for definite map");
        // 1 (outer) + 30 (policy) + 1 (header) + 23*3 (entries) = 101
        assert_eq!(enc.len(), 101);
    }

    /// Inner asset map with 24 entries switches to INDEFINITE (0xbf ... 0xff).
    /// Total length ties with the definite form (0xb8 0x18 = 2 bytes vs
    /// 0xbf + 0xff = 2 bytes) — the BYTES differ, the size does not.
    #[test]
    fn multi_asset_inner_map_24_entries_indefinite() {
        let mut multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = BTreeMap::new();
        multi.insert(policy(0x01), assets_n(24));
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[0], 0xa1, "outer map(1) must stay definite");
        assert_eq!(enc[31], 0xbf, "24-entry inner map must open indefinite");
        assert_eq!(
            *enc.last().unwrap(),
            0xff,
            "indefinite map must close with break"
        );
        // 1 + 30 + 1 (0xbf) + 24*3 + 1 (0xff) = 105 — same as definite (2-byte header).
        assert_eq!(enc.len(), 105);
    }

    /// 255 entries: indefinite, same total length as the 2-byte definite header.
    #[test]
    fn multi_asset_inner_map_255_entries_indefinite_same_length() {
        let mut multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = BTreeMap::new();
        multi.insert(policy(0x01), assets_n(255));
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[31], 0xbf);
        assert_eq!(*enc.last().unwrap(), 0xff);
        // 1 + 30 + 1 + 255*3 + 1 = 798 — definite (0xb8 0xff) would also be 798.
        assert_eq!(enc.len(), 798);
    }

    /// 256 entries: indefinite saves exactly 1 byte over the 3-byte definite
    /// header (0xb9 0x01 0x00) — THE #930 divergence class.
    #[test]
    fn multi_asset_inner_map_256_entries_indefinite_saves_one_byte() {
        let mut multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = BTreeMap::new();
        multi.insert(policy(0x01), assets_n(256));
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[31], 0xbf, "256-entry inner map must open indefinite");
        assert_eq!(*enc.last().unwrap(), 0xff);
        // 1 + 30 + 1 (0xbf) + 256*3 + 1 (0xff) = 801.
        // Definite would be 1 + 30 + 3 (0xb9 0x01 0x00) + 768 = 802.
        assert_eq!(enc.len(), 801, "must be 1 byte shorter than definite");
    }

    /// Outer policy map with 23 policies stays definite (0xb7).
    #[test]
    fn multi_asset_outer_map_23_policies_definite_header() {
        let multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> =
            (0..23).map(|i| (policy(i as u8), assets_n(1))).collect();
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[0], 0xb7, "23-policy outer map must be definite map(23)");
        assert_ne!(*enc.last().unwrap(), 0xff);
        // 1 (header) + 23 * (30 policy + 1 inner map(1) + 3 entry) = 783
        assert_eq!(enc.len(), 783);
    }

    /// Outer policy map with 24 policies switches to indefinite.
    #[test]
    fn multi_asset_outer_map_24_policies_indefinite() {
        let multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> =
            (0..24).map(|i| (policy(i as u8), assets_n(1))).collect();
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[0], 0xbf, "24-policy outer map must open indefinite");
        assert_eq!(
            *enc.last().unwrap(),
            0xff,
            "outer map must close with break"
        );
        // 1 (0xbf) + 24*34 + 1 (0xff) = 818 — ties with the 2-byte definite header.
        assert_eq!(enc.len(), 818);
    }

    /// Outer policy map with 256 policies: indefinite saves 1 byte at the
    /// outer level too (both map levels follow `encodeMap` independently).
    #[test]
    fn multi_asset_outer_map_256_policies_indefinite_saves_one_byte() {
        let multi: BTreeMap<Hash28, BTreeMap<AssetName, u64>> = (0..256)
            .map(|i| {
                let mut bytes = [0u8; 28];
                bytes[0] = (i / 256) as u8;
                bytes[1] = (i % 256) as u8;
                (Hash28::from_bytes(bytes), assets_n(1))
            })
            .collect();
        assert_eq!(multi.len(), 256);
        let enc = encode_multi_asset(&multi);

        assert_eq!(enc[0], 0xbf);
        assert_eq!(*enc.last().unwrap(), 0xff);
        // 1 + 256*34 + 1 = 8706; definite (0xb9 0x01 0x00) would be 8707.
        assert_eq!(enc.len(), 8706, "must be 1 byte shorter than definite");
    }

    /// encode_mint follows the same `encodeMap` semantics: 23 definite / 24
    /// indefinite at the inner level.
    #[test]
    fn mint_inner_map_23_vs_24_entries_header_switch() {
        let mut mint23: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        mint23.insert(policy(0x01), mint_assets_n(23));
        let enc23 = encode_mint(&mint23);
        assert_eq!(enc23[31], 0xb7, "23-entry mint inner map must be definite");
        assert_eq!(enc23.len(), 101);

        let mut mint24: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        mint24.insert(policy(0x01), mint_assets_n(24));
        let enc24 = encode_mint(&mint24);
        assert_eq!(
            enc24[31], 0xbf,
            "24-entry mint inner map must open indefinite"
        );
        assert_eq!(*enc24.last().unwrap(), 0xff);
        assert_eq!(enc24.len(), 105);
    }

    /// encode_mint at the 255/256 boundary: 256 entries save 1 byte vs
    /// definite; and the outer mint map switches at > 23 policies too.
    #[test]
    fn mint_256_entries_and_outer_map_indefinite() {
        let mut mint: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
        mint.insert(policy(0x01), mint_assets_n(256));
        let enc = encode_mint(&mint);
        assert_eq!(enc[31], 0xbf);
        assert_eq!(*enc.last().unwrap(), 0xff);
        assert_eq!(enc.len(), 801, "must be 1 byte shorter than definite");

        let outer: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = (0..24)
            .map(|i| (policy(i as u8), mint_assets_n(1)))
            .collect();
        let enc_outer = encode_mint(&outer);
        assert_eq!(
            enc_outer[0], 0xbf,
            "24-policy mint outer map must open indefinite"
        );
        assert_eq!(*enc_outer.last().unwrap(), 0xff);
        assert_eq!(enc_outer.len(), 818);
    }

    /// Round-trip: a value whose OUTER and INNER maps are both > 23 entries
    /// (indefinite at both levels) must decode back identically — dugite's
    /// decoder (`read_map`) accepts definite and indefinite maps alike.
    #[test]
    fn value_with_indefinite_maps_roundtrips_through_decoder() {
        use crate::decode::era_alonzo::read_value;
        use crate::decode::reader::Reader;

        let mut v = Value::lovelace(1_234_567);
        for i in 0..25u8 {
            let n_assets = if i == 0 { 30 } else { 1 };
            v.multi_asset.insert(policy(i), assets_n(n_assets));
        }
        let enc = encode_value(&v);
        // Sanity: outer map (after 0x82 + 5-byte coin) opens indefinite.
        assert_eq!(enc[0], 0x82);
        assert_eq!(enc[6], 0xbf, "25-policy outer map must open indefinite");

        let mut r = Reader::new(&enc);
        let decoded = read_value(&mut r).expect("decode indefinite-map value");
        assert_eq!(decoded, v, "indefinite-map value must round-trip");
    }

    /// Verifies the total byte length of a known single-asset encoding.
    #[test]
    fn test_encode_value_known_byte_length() {
        // Value: 1 ADA + 1 policy with 1 asset named "X" (1 byte), qty=1
        let mut v = Value {
            coin: Lovelace(1_000_000),
            multi_asset: BTreeMap::new(),
        };
        let mut assets = BTreeMap::new();
        assets.insert(asset(b"X"), 1u64);
        v.multi_asset.insert(policy(0xBE), assets);

        let enc = encode_value(&v);

        // Layout:
        //   0x82                        — array(2)         1
        //   0x1a 0x00 0x0f 0x42 0x40   — coin=1_000_000   5
        //   0xa1                        — map(1)           1
        //   0x58 0x1c <28 bytes>        — policy_id       30
        //   0xa1                        — map(1)           1
        //   0x41 0x58                   — bytes(1) "X"     2
        //   0x01                        — uint 1           1
        // Total: 1+5+1+30+1+2+1 = 41
        assert_eq!(
            enc.len(),
            41,
            "unexpected encoded length for single-asset value"
        );
    }
}
