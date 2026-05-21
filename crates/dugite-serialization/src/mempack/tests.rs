//! Tests for the MemPack UTxO decoder.

use super::compact::decode_varlen;
use super::txout::decode_mempack_txout;
use super::{decode_mempack_txin, TvarIterator};

#[test]
fn test_decode_varlen_small() {
    assert_eq!(decode_varlen(&[0]).unwrap(), (0, 1));
    assert_eq!(decode_varlen(&[1]).unwrap(), (1, 1));
    assert_eq!(decode_varlen(&[29]).unwrap(), (29, 1));
    assert_eq!(decode_varlen(&[127]).unwrap(), (127, 1));
}

#[test]
fn test_decode_varlen_multi_byte_msb_first() {
    // Haskell MemPack VarLen is MSB-first. See compact.rs unit tests for the
    // algorithm; these mirror them as sanity for the re-exported function.
    //
    //   [0x81, 0x00] → (1<<7)|0  = 128
    //   [0x81, 0x16] → (1<<7)|22 = 150
    assert_eq!(decode_varlen(&[0x81, 0x00]).unwrap(), (128, 2));
    assert_eq!(decode_varlen(&[0x81, 0x16]).unwrap(), (150, 2));
}

#[test]
fn test_decode_varlen_three_byte_msb_first() {
    // [0xee, 0xdd, 0x01] = 1_814_145 (real preview tvar coin VarLen,
    // cross-checked via Koios: 00002435e40d68a58b5130644c845c05fa8e36e3935a905f718e6fa611f0304a#2).
    //   0xee → 0x6e = 110
    //   0xdd → 110 << 7 | 0x5d = 14_173
    //   0x01 → 14_173 << 7 | 1 = 1_814_145
    assert_eq!(decode_varlen(&[0xee, 0xdd, 0x01]).unwrap(), (1_814_145, 3));
}

#[test]
fn test_decode_varlen_empty_input() {
    assert!(decode_varlen(&[]).is_err());
}

#[test]
fn test_decode_mempack_txin() {
    // Real key from preview tvar fixture:
    // TxId = 00000c339a7d28e08060a69e3d9adf16846382f59a4d321f8b9580ffdb597c0b
    // TxIx = 1 (bytes 01 00 in LE)
    let key = hex::decode("00000c339a7d28e08060a69e3d9adf16846382f59a4d321f8b9580ffdb597c0b0100")
        .unwrap();
    let txin = decode_mempack_txin(&key).unwrap();
    assert_eq!(txin.txix, 1);
    assert_eq!(
        txin.txid.to_hex(),
        "00000c339a7d28e08060a69e3d9adf16846382f59a4d321f8b9580ffdb597c0b"
    );
}

#[test]
fn test_decode_mempack_txin_wrong_length() {
    let short = vec![0u8; 33];
    assert!(decode_mempack_txin(&short).is_err());
    let long = vec![0u8; 35];
    assert!(decode_mempack_txin(&long).is_err());
}

#[test]
fn test_decode_mempack_txin_txix_zero() {
    let key = vec![0u8; 34];
    // TxIx = 0x0000 LE = 0
    let txin = decode_mempack_txin(&key).unwrap();
    assert_eq!(txin.txix, 0);
}

#[test]
fn test_decode_mempack_txin_txix_large() {
    let mut key = vec![0xAA; 34];
    // TxIx = 0xFF 0x00 LE = 255
    key[32] = 0xFF;
    key[33] = 0x00;
    let txin = decode_mempack_txin(&key).unwrap();
    assert_eq!(txin.txix, 255);
}

/// Regression test for issue #461: the Haskell `MemPack Word16` instance
/// (derived for `newtype TxIx = TxIx Word16`) uses `writeWord8ArrayAsWord16#`,
/// which is platform-native endianness. On all supported targets (x86_64,
/// aarch64) that is **little-endian**.
///
/// The ouroboros-consensus 1.0.0.0 "flip TxIx serialization" change cited in
/// the issue does NOT exist in the current upstream changelog (latest is
/// 0.28.x as of 2026-05). Haskell's on-disk MemPack TxIx therefore remains
/// host-LE, and dugite's LE decode matches Haskell byte-for-byte.
///
/// This test pins that invariant so a future endianness flip cannot regress
/// silently — TxIx values >= 256 round-trip only when LE is used.
#[test]
fn test_mempack_txix_endianness_pinned_le_v11() {
    // TxIx = 0x0102 = 258. LE bytes = [0x02, 0x01], BE bytes would be [0x01, 0x02].
    let mut key = [0u8; 34];
    key[32] = 0x02;
    key[33] = 0x01;
    let txin = decode_mempack_txin(&key).unwrap();
    assert_eq!(
        txin.txix, 258,
        "TxIx must decode as little-endian Word16 to match Haskell MemPack"
    );

    // Sweep [0, 1000]: every value must round-trip via LE.
    for ix in 0u16..=1000 {
        let mut k = [0u8; 34];
        let le = ix.to_le_bytes();
        k[32] = le[0];
        k[33] = le[1];
        let decoded = decode_mempack_txin(&k).unwrap();
        assert_eq!(decoded.txix, ix, "LE round-trip failed at ix={ix}");
    }

    // Lexicographic byte ordering of MemPack keys for the same TxId does NOT
    // match numeric TxIx ordering when the encoding is LE. Document this
    // explicitly: for a fixed TxId, key(0x0100=256) sorts BEFORE key(0x00FF=255)
    // bytewise because LE puts the low byte first.
    let mut k_255 = [0u8; 34];
    k_255[32..34].copy_from_slice(&255u16.to_le_bytes()); // [0xFF, 0x00]
    let mut k_256 = [0u8; 34];
    k_256[32..34].copy_from_slice(&256u16.to_le_bytes()); // [0x00, 0x01]
    assert!(
        k_256.as_slice() < k_255.as_slice(),
        "LE encoding: bytewise sort intentionally diverges from numeric TxIx \
         order across the 256 boundary — Haskell behaves identically because \
         MemPack Word16 is host-native LE on x86_64/aarch64"
    );
}

#[test]
fn test_decode_mempack_txout_tag0() {
    // Real tag-0 entry from preview tvar, cross-checked against Koios:
    //   tx 00002435e40d68a58b5130644c845c05fa8e36e3935a905f718e6fa611f0304a#2
    //   value = 1_814_145 lovelace
    //   address = addr_test1vzvxehk0cn64t2rqt43p2pdy4qkzt3t57k0apdu79tx67qsewlc5m
    //             (enterprise testnet, hdr=0x60)
    let val = hex::decode("001d60986cdecfc4f555a8605d621505a4a82c25c574f59fd0b79e2acdaf0200eedd01")
        .unwrap();
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 0);
    assert_eq!(txout.address.len(), 29);
    assert_eq!(txout.address[0], 0x60); // Enterprise testnet header
    assert_eq!(txout.coin, 1_814_145);
    assert!(txout.multi_asset.is_none());
    assert!(txout.datum_hash.is_none());
    assert!(txout.datum.is_none());
    assert!(txout.script_ref.is_none());
}

#[test]
fn test_decode_mempack_txout_tag0_larger_coin() {
    // Real tag-0 entry:
    //   tx 0000665327353c62873a7c88307b40fd8bb994c341a1ebc960af0477f7abae9b#0
    //   value = 25_000_000 lovelace (verified via Koios preview)
    let val =
        hex::decode("001d6000d5c82abfa96b4daa29e7ee3ca4a642fa256d3bae3f7a7c1b78ad47008bf5f040")
            .unwrap();
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.coin, 25_000_000);
    assert_eq!(txout.address[0], 0x60);
}

#[test]
fn test_decode_mempack_txout_tag2_real_entry() {
    // Real tag-2 entry from preview tvar, cross-checked against Koios:
    //   tx 00001a2493f77dcdc7a43e4edd491d30f02e78563f5a4c602185869421d0b5ae#1
    //   address = addr_test1qqdeeh2wtfktppgpu3hpq4gm02ze6j5cy5gqnwu366tctajkj8tg4kr4st7gnwdvg07syf705sgga7merwvc0v5s4xaqja6xpa
    //     hdr=0x00 (base, testnet, pay=key, stake=key)
    //     pay28  = 1b9cdd4e5a6cb08501e46e10551b7a859d4a98251009bb91d69785f6
    //     stake28= 5691d68ad87582fc89b9ac43fd0227cfa4108efb791b9987b290a9ba
    //   value = 1_200_000 lovelace
    let val = hex::decode(
        "02015691d68ad87582fc89b9ac43fd0227cfa4108efb791b9987b290a9ba\
         85b06c5a4edd9c1b857a1b55106ee40191bb091025984a9d01000000f68597d6\
         00c99f00",
    )
    .unwrap();
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 2);

    // Full 57-byte Shelley base address: header + pay28 + stake28.
    assert_eq!(txout.address.len(), 57);
    assert_eq!(txout.address[0], 0x00); // base, testnet, pay=key, stake=key
    assert_eq!(
        hex::encode(&txout.address[1..29]),
        "1b9cdd4e5a6cb08501e46e10551b7a859d4a98251009bb91d69785f6"
    );
    assert_eq!(
        hex::encode(&txout.address[29..57]),
        "5691d68ad87582fc89b9ac43fd0227cfa4108efb791b9987b290a9ba"
    );

    assert_eq!(txout.coin, 1_200_000);
    assert!(txout.multi_asset.is_none());
    assert!(txout.datum_hash.is_none());
    assert!(txout.opaque_tail.is_none());
}

#[test]
fn test_decode_mempack_txout_tag2_handcrafted_edges() {
    // Build a tag-2 entry by hand for coverage of edge cases.
    //
    //   stake cred = ScriptHashObj(all zeros)
    //   payment hash = 0x01..0x1c (28 increasing bytes)
    //   metadata: mainnet (bit 1) + payment is script (bit 0 = 0)
    //   coin = 0
    let mut bytes = Vec::new();
    bytes.push(0x02); // outer tag
    bytes.push(0x00); // Credential Staking tag: 0 = ScriptHashObj
    bytes.extend_from_slice(&[0x00u8; 28]); // stake hash = all zeros

    // Payment hash = [0x01, 0x02, ..., 0x1c]
    let pay: [u8; 28] = core::array::from_fn(|i| (i as u8) + 1);

    // Pack PackedBytes28: w0..w2 BE(pay[0..8]..pay[16..24]), w3_top = BE(pay[24..28]).
    let be_w0 = u64::from_be_bytes(pay[0..8].try_into().unwrap());
    let be_w1 = u64::from_be_bytes(pay[8..16].try_into().unwrap());
    let be_w2 = u64::from_be_bytes(pay[16..24].try_into().unwrap());
    let be_w3_top = u32::from_be_bytes(pay[24..28].try_into().unwrap()) as u64;
    // Metadata: mainnet=1, payment_is_key=0 (script) → bit1 set, bit0 clear
    let meta: u64 = 0b10;
    let w3 = (be_w3_top << 32) | meta;

    // Serialize as native-endian (little-endian on build targets).
    bytes.extend_from_slice(&be_w0.to_le_bytes());
    bytes.extend_from_slice(&be_w1.to_le_bytes());
    bytes.extend_from_slice(&be_w2.to_le_bytes());
    bytes.extend_from_slice(&w3.to_le_bytes());

    // CompactForm Coin: inner tag 0 + VarLen(0)
    bytes.push(0x00);
    bytes.push(0x00);

    let (txout, consumed) = decode_mempack_txout(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(txout.tag, 2);
    assert_eq!(txout.coin, 0);
    assert_eq!(txout.address.len(), 57);
    // Header: base address (bits 6-7 = 0), payment is script (bit 4 = 1),
    // stake is script (bit 5 = 1), mainnet (bit 0 = 1) → 0b00110001 = 0x31.
    assert_eq!(txout.address[0], 0x31);
    assert_eq!(&txout.address[1..29], &pay[..]);
    assert_eq!(&txout.address[29..57], &[0u8; 28]);
}

#[test]
fn test_decode_mempack_txout_tag2_max_u64_coin() {
    // Same synthetic shape as above but with coin = u64::MAX, to exercise
    // full-width VarLen.
    let mut bytes = Vec::new();
    bytes.push(0x02);
    bytes.push(0x01); // Credential Staking: KeyHashObj
    bytes.extend_from_slice(&[0xAAu8; 28]);

    let pay = [0xBBu8; 28];
    let be_w0 = u64::from_be_bytes(pay[0..8].try_into().unwrap());
    let be_w1 = u64::from_be_bytes(pay[8..16].try_into().unwrap());
    let be_w2 = u64::from_be_bytes(pay[16..24].try_into().unwrap());
    let be_w3_top = u32::from_be_bytes(pay[24..28].try_into().unwrap()) as u64;
    // Testnet + payment=key → meta = 0b01
    let meta: u64 = 0b01;
    let w3 = (be_w3_top << 32) | meta;
    bytes.extend_from_slice(&be_w0.to_le_bytes());
    bytes.extend_from_slice(&be_w1.to_le_bytes());
    bytes.extend_from_slice(&be_w2.to_le_bytes());
    bytes.extend_from_slice(&w3.to_le_bytes());

    // CompactForm Coin: inner tag 0 + VarLen(u64::MAX) in MSB-first = 10 bytes.
    bytes.push(0x00);
    bytes.extend_from_slice(&[0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);

    let (txout, consumed) = decode_mempack_txout(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(txout.coin, u64::MAX);
    // Base, testnet, payment=key, stake=key → header = 0x00.
    assert_eq!(txout.address[0], 0x00);
    assert_eq!(&txout.address[1..29], &pay[..]);
}

#[test]
fn test_decode_mempack_txout_tag3_with_datum_hash() {
    // Build a tag-3 entry = tag-2 body + 32-byte DataHash32.
    //
    // The DataHash32 on the wire is 4 × Word64 little-endian; reconstructed
    // via BE u64 in slots (w0,w1,w2,w3). Pick a known datum hash and work
    // backwards.
    let datum_hash: [u8; 32] = core::array::from_fn(|i| (i as u8) + 0x10);
    let dw0 = u64::from_be_bytes(datum_hash[0..8].try_into().unwrap());
    let dw1 = u64::from_be_bytes(datum_hash[8..16].try_into().unwrap());
    let dw2 = u64::from_be_bytes(datum_hash[16..24].try_into().unwrap());
    let dw3 = u64::from_be_bytes(datum_hash[24..32].try_into().unwrap());

    let mut bytes = Vec::new();
    bytes.push(0x03); // outer tag 3
    bytes.push(0x01); // stake cred = KeyHashObj
    bytes.extend_from_slice(&[0xCCu8; 28]); // stake hash

    let pay: [u8; 28] = core::array::from_fn(|i| 0xE0u8.wrapping_add(i as u8));
    let be_w0 = u64::from_be_bytes(pay[0..8].try_into().unwrap());
    let be_w1 = u64::from_be_bytes(pay[8..16].try_into().unwrap());
    let be_w2 = u64::from_be_bytes(pay[16..24].try_into().unwrap());
    let be_w3_top = u32::from_be_bytes(pay[24..28].try_into().unwrap()) as u64;
    // payment=key (bit 0), testnet (bit 1 = 0) → meta = 0b01
    let w3 = (be_w3_top << 32) | 0b01;
    bytes.extend_from_slice(&be_w0.to_le_bytes());
    bytes.extend_from_slice(&be_w1.to_le_bytes());
    bytes.extend_from_slice(&be_w2.to_le_bytes());
    bytes.extend_from_slice(&w3.to_le_bytes());

    // CompactCoin: tag 0 + VarLen(2_000_000)
    // 2_000_000 in MSB-first 7-bit groups:
    //   2_000_000 = 0x1E_8480
    //   bits: 00011110_10000100_10000000 (24 bits needed)
    //   groups (7-bit MSB first): 1111010_0001001_0000000
    //     → 0x7A (top bit 0 set as cont) = 0xFA
    //     → 0x09 | 0x80 = 0x89
    //     → 0x00 (terminal)
    // Verify: ((0x7A)<<14) | ((0x09)<<7) | 0 = 2_007_040 — not 2_000_000, so
    // let me just let the test use a simpler value: 150 (0x81, 0x16).
    bytes.push(0x00); // inner tag
    bytes.extend_from_slice(&[0x81, 0x16]); // VarLen = 150

    // DataHash32 (32 bytes = 4 LE u64)
    bytes.extend_from_slice(&dw0.to_le_bytes());
    bytes.extend_from_slice(&dw1.to_le_bytes());
    bytes.extend_from_slice(&dw2.to_le_bytes());
    bytes.extend_from_slice(&dw3.to_le_bytes());

    let (txout, consumed) = decode_mempack_txout(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(txout.tag, 3);
    assert_eq!(txout.coin, 150);
    assert_eq!(txout.address.len(), 57);
    assert_eq!(&txout.address[1..29], &pay[..]);
    assert_eq!(&txout.address[29..57], &[0xCCu8; 28]);
    assert_eq!(txout.datum_hash.as_ref().unwrap(), &datum_hash);
    assert!(txout.opaque_tail.is_none());
}

#[test]
fn test_decode_mempack_txout_tag4_ada_only() {
    // Construct a synthetic tag-4 ADA-only entry:
    // tag(4) + addr_len(29) + addr(29 bytes) + value_tag(0) + coin_varlen + datum
    let mut val = Vec::new();
    val.push(4); // tag
    val.push(29); // addr len VarLen
    val.extend_from_slice(&[0x70; 29]); // 29-byte enterprise script address
    val.push(0); // value tag = 0 (ADA-only)
    val.extend_from_slice(&[0xee, 0xdd, 0x01]); // coin = 1_814_145 (MSB-first)
    val.extend_from_slice(&[0xd8, 0x79, 0x9f, 0xff]); // 4 bytes of CBOR datum

    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 4);
    assert_eq!(txout.address.len(), 29);
    assert_eq!(txout.coin, 1_814_145);
    assert!(txout.multi_asset.is_none());
    let datum = txout.datum.unwrap();
    assert_eq!(datum, &[0xd8, 0x79, 0x9f, 0xff]);
}

#[test]
fn test_decode_mempack_txout_unknown_tag() {
    let data = [0x06]; // tag 6 doesn't exist
    assert!(decode_mempack_txout(&data).is_err());
}

#[test]
fn test_tvar_iterator_fixture() {
    let data = include_bytes!("../../test_fixtures/preview_tvar_head_64k.bin");
    let iter = TvarIterator::new(data).unwrap();
    let mut count = 0;
    let mut tag_counts = [0u32; 6];
    for result in iter {
        match result {
            Ok((txin, txout)) => {
                assert_eq!(txin.txid.as_bytes().len(), 32);
                // For tags 0/1/4/5 the coin should be > 0 OR multi_asset present.
                // For tags 2/3 the coin is 0 (opaque) but opaque_tail is present.
                match txout.tag {
                    0 | 1 => {
                        assert!(
                            txout.coin > 0 || txout.multi_asset.is_some(),
                            "tag {} entry {}: zero coin without multi-asset",
                            txout.tag,
                            count
                        );
                    }
                    2 => {
                        // Full Shelley base address + decoded coin.
                        assert_eq!(
                            txout.address.len(),
                            57,
                            "tag 2 entry {count}: expected 57-byte base address"
                        );
                        assert!(
                            txout.coin > 0,
                            "tag 2 entry {count}: zero coin (should never happen for real UTxOs)"
                        );
                        assert!(txout.opaque_tail.is_none());
                    }
                    3 => {
                        assert_eq!(txout.address.len(), 57);
                        assert!(txout.coin > 0);
                        assert!(txout.datum_hash.is_some());
                        assert!(txout.opaque_tail.is_none());
                    }
                    4 | 5 => {
                        // Coin may be zero for multi-asset entries, but we still
                        // expect some value data.
                        assert!(
                            txout.coin > 0
                                || txout.multi_asset.is_some()
                                || txout.opaque_tail.is_some()
                        );
                    }
                    _ => panic!("unexpected tag {}", txout.tag),
                }
                if txout.tag < 6 {
                    tag_counts[txout.tag as usize] += 1;
                }
                count += 1;
            }
            Err(e) => panic!("iteration error at entry {count}: {e}"),
        }
    }

    // The 64KB fixture holds ~400 entries (last one may be truncated and
    // silently skipped by the iterator).
    assert!(
        count >= 350,
        "expected >= 350 entries in 64KB fixture, got {count}"
    );

    // Verify we see multiple tag variants.
    assert!(tag_counts[0] > 100, "expected many tag-0 entries");
    assert!(tag_counts[2] > 50, "expected many tag-2 entries");
    assert!(tag_counts[4] > 30, "expected many tag-4 entries");

    eprintln!(
        "tvar iterator: {count} entries, tags: [0]={}, [1]={}, [2]={}, [3]={}, [4]={}, [5]={}",
        tag_counts[0], tag_counts[1], tag_counts[2], tag_counts[3], tag_counts[4], tag_counts[5]
    );
}

#[test]
fn test_tvar_iterator_empty() {
    assert!(TvarIterator::new(&[]).is_err());
}

#[test]
fn test_tvar_iterator_truncated_header() {
    // Just array(1) without the map.
    assert!(TvarIterator::new(&[0x81]).is_err());
}

#[test]
fn test_tvar_iterator_immediate_break() {
    // array(1) + map(indef) + break byte.
    let data = [0x81, 0xbf, 0xff];
    let iter = TvarIterator::new(&data).unwrap();
    let entries: Vec<_> = iter.collect();
    assert!(entries.is_empty());
}

// ── Additional TxOut error-path coverage ──────────────────────────────────────

#[test]
fn test_decode_mempack_txout_empty_input_errors() {
    assert!(decode_mempack_txout(&[]).is_err());
}

#[test]
fn test_decode_mempack_txout_tag1_too_short() {
    // tag=1 then nothing — clearly < 34 bytes.
    let data = [0x01u8; 5];
    assert!(decode_mempack_txout(&data).is_err());
}

#[test]
fn test_decode_mempack_txout_tag1_with_datum_hash() {
    // tag(1) + addr_len(29) + 29 addr bytes + value_tag(0) + coin_varlen(0x00) + 32-byte hash
    let mut val = Vec::new();
    val.push(0x01);
    val.push(29);
    val.extend_from_slice(&[0x70; 29]);
    val.push(0); // ADA-only value tag
    val.push(0); // coin VarLen = 0
    val.extend_from_slice(&[0xCDu8; 32]); // datum hash
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 1);
    assert_eq!(txout.coin, 0);
    assert_eq!(txout.datum_hash.as_ref().unwrap(), &[0xCDu8; 32]);
}

#[test]
fn test_decode_mempack_txout_tag2_invalid_stake_cred_tag() {
    // tag(2) outer + invalid Credential Staking tag (must be 0 or 1).
    let mut bytes = vec![0x02, 0x05]; // stake cred tag = 5 (invalid)
    bytes.extend_from_slice(&[0x00u8; 28]); // stake hash
    bytes.extend_from_slice(&[0x00u8; 32]); // Addr28Extra (all zero, doesn't matter — will fail before)
    bytes.push(0x00); // CompactCoin inner tag
    bytes.push(0x00); // VarLen = 0
    let err = decode_mempack_txout(&bytes).unwrap_err();
    let SerializationError::CborDecode(msg) = err else {
        panic!("expected CborDecode");
    };
    assert!(
        msg.contains("invalid Credential Staking tag"),
        "unexpected error: {msg}"
    );
}

#[test]
fn test_decode_mempack_txout_tag2_truncated_stake_cred() {
    // tag(2) outer + only 10 bytes — fails the 29-byte stake cred check.
    let bytes = vec![0x02u8; 10];
    let err = decode_mempack_txout(&bytes).unwrap_err();
    assert!(matches!(err, SerializationError::CborDecode(_)));
}

#[test]
fn test_decode_mempack_txout_tag2_truncated_addr28extra() {
    // tag(2) + 29 byte cred + 16 bytes (not 32) of addr28extra.
    let mut bytes = vec![0x02, 0x01]; // tag + KeyHash
    bytes.extend_from_slice(&[0u8; 28]);
    bytes.extend_from_slice(&[0u8; 16]); // truncated Addr28Extra
    let err = decode_mempack_txout(&bytes).unwrap_err();
    assert!(matches!(err, SerializationError::CborDecode(_)));
}

#[test]
fn test_decode_mempack_txout_tag2_unexpected_compactcoin_inner_tag() {
    // tag(2) + cred + addr28extra valid + inner tag != 0.
    let mut bytes = vec![0x02, 0x01];
    bytes.extend_from_slice(&[0u8; 28]);
    bytes.extend_from_slice(&[0u8; 32]);
    bytes.push(0x07); // wrong inner tag (must be 0)
    bytes.push(0x00);
    let err = decode_mempack_txout(&bytes).unwrap_err();
    let SerializationError::CborDecode(msg) = err else {
        panic!("expected CborDecode");
    };
    assert!(
        msg.contains("unexpected CompactCoin inner tag"),
        "got: {msg}"
    );
}

#[test]
fn test_decode_mempack_txout_tag3_truncated_datum_hash() {
    // Build a valid tag-3 prefix but only include 10 bytes of DataHash32.
    let mut bytes = vec![0x03, 0x01];
    bytes.extend_from_slice(&[0u8; 28]);
    bytes.extend_from_slice(&[0u8; 32]);
    bytes.push(0x00); // CompactCoin inner tag
    bytes.push(0x00); // VarLen = 0
    bytes.extend_from_slice(&[0u8; 10]); // truncated DataHash32 (need 32)
    let err = decode_mempack_txout(&bytes).unwrap_err();
    assert!(matches!(err, SerializationError::CborDecode(_)));
}

#[test]
fn test_decode_mempack_txout_tag4_no_value_data() {
    // tag(4) + addr only, no value byte.
    let val = vec![
        4u8, 29, /* 29 addr bytes */ 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    // addr is 29 bytes, but we need 30 total (tag + len byte + 29 addr) + value...
    // The slice above is 31 bytes, which is exactly tag + len + 29 addr. No value byte.
    let err = decode_mempack_txout(&val).unwrap_err();
    let SerializationError::CborDecode(msg) = err else {
        panic!("expected CborDecode");
    };
    assert!(msg.contains("tag 4: no value data"), "got: {msg}");
}

#[test]
fn test_decode_mempack_txout_tag4_multi_asset() {
    // tag(4) + addr(29 bytes) + value_tag=1 (multi-asset) + coin + opaque tail
    let mut val = vec![4, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(1); // value tag = 1 (multi-asset)
    val.push(0x00); // VarLen coin = 0
    val.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // some opaque trailing bytes
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 4);
    assert!(txout.multi_asset.is_some());
    assert!(txout.datum.is_none());
}

#[test]
fn test_decode_mempack_txout_tag5_no_value_data() {
    let val = vec![
        5u8, 29, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ];
    let err = decode_mempack_txout(&val).unwrap_err();
    let SerializationError::CborDecode(msg) = err else {
        panic!("expected CborDecode");
    };
    assert!(msg.contains("tag 5: no value data"), "got: {msg}");
}

#[test]
fn test_decode_mempack_txout_tag5_ada_only_with_opaque_tail() {
    let mut val = vec![5, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(0); // ADA-only value tag
    val.push(0x00); // VarLen coin = 0
    val.extend_from_slice(&[0xDE, 0xAD]); // opaque datum + script bytes
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 5);
    assert_eq!(txout.opaque_tail.as_deref(), Some(&[0xDE, 0xAD][..]));
}

#[test]
fn test_decode_mempack_txout_tag5_multi_asset() {
    let mut val = vec![5, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(1); // multi-asset value tag
    val.push(0x00); // VarLen coin
    val.extend_from_slice(&[0xCA, 0xFE]);
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 5);
    assert!(txout.multi_asset.is_some());
}

// ── TvarIterator: definite-length map handling ────────────────────────────────

#[test]
fn test_tvar_iterator_definite_length_map_zero_entries() {
    // array(1) + map(0) — definite-length empty map, no break byte.
    let data = [0x81, 0xa0];
    let iter = TvarIterator::new(&data).unwrap();
    let entries: Vec<_> = iter.collect();
    assert!(entries.is_empty());
}

#[test]
fn test_tvar_iterator_wrong_outer_array_length() {
    // array(2) instead of array(1).
    let data = [0x82, 0xbf, 0xff];
    assert!(TvarIterator::new(&data).is_err());
}

#[test]
fn test_tvar_iterator_inner_not_a_map() {
    // array(1) + uint(0) — not a map.
    let data = [0x81, 0x00];
    assert!(TvarIterator::new(&data).is_err());
}

#[test]
fn test_tvar_iterator_truncated_before_map_header() {
    // array(1) and nothing else.
    let data = [0x81];
    assert!(TvarIterator::new(&data).is_err());
}

// ── MemPackTxIn error paths ───────────────────────────────────────────────────

#[test]
fn test_decode_mempack_txin_short_length_returns_invalid_length_error() {
    let err = decode_mempack_txin(&[0u8; 10]).unwrap_err();
    match err {
        SerializationError::InvalidLength { expected, got } => {
            assert_eq!(expected, 34);
            assert_eq!(got, 10);
        }
        other => panic!("expected InvalidLength, got {other:?}"),
    }
}

// Re-import for the SerializationError type used above.
use crate::error::SerializationError;
