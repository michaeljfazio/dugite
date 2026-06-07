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
    let txin = decode_mempack_txin(&key, TxIxEndianness::Little).unwrap();
    assert_eq!(txin.txix, 1);
    assert_eq!(
        txin.txid.to_hex(),
        "00000c339a7d28e08060a69e3d9adf16846382f59a4d321f8b9580ffdb597c0b"
    );
}

#[test]
fn test_decode_mempack_txin_wrong_length() {
    let short = vec![0u8; 33];
    assert!(decode_mempack_txin(&short, TxIxEndianness::Little).is_err());
    let long = vec![0u8; 35];
    assert!(decode_mempack_txin(&long, TxIxEndianness::Little).is_err());
}

#[test]
fn test_decode_mempack_txin_txix_zero() {
    let key = vec![0u8; 34];
    // TxIx = 0x0000 LE = 0
    let txin = decode_mempack_txin(&key, TxIxEndianness::Little).unwrap();
    assert_eq!(txin.txix, 0);
}

#[test]
fn test_decode_mempack_txin_txix_large() {
    let mut key = vec![0xAA; 34];
    // TxIx = 0xFF 0x00 LE = 255
    key[32] = 0xFF;
    key[33] = 0x00;
    let txin = decode_mempack_txin(&key, TxIxEndianness::Little).unwrap();
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
    let txin = decode_mempack_txin(&key, TxIxEndianness::Little).unwrap();
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
        let decoded = decode_mempack_txin(&k, TxIxEndianness::Little).unwrap();
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
                                                // Tag-4 datum is a `BinaryData` = `VarLen(len) ‖ raw_cbor`. The 4 CBOR datum
                                                // bytes must be length-prefixed (VarLen(4) = 0x04).
    val.push(0x04);
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
            Err(e) => {
                // The 64KB fixture is a CLIPPED prefix of a real tvar blob, so its
                // FINAL entry is cut short mid-value. The (now strict, #10
                // full-consumption) iterator HARD-ERRORS on that truncated tail
                // rather than silently skipping it — exactly as Haskell
                // `loadSnapshot` aborts on a partial-EOF/CBOR-Fail. For a real
                // (complete) snapshot this would be a genuine failure; for this
                // deliberately-clipped fixture it is the expected end boundary.
                // Accept it ONLY when it is the truncation error AND we have
                // already decoded the bulk of the fixture.
                let msg = format!("{e}");
                assert!(
                    msg.contains("truncated") || msg.contains("partial"),
                    "unexpected non-truncation error at entry {count}: {e}"
                );
                assert!(
                    count >= 350,
                    "truncation error must only occur at the clipped tail (entry \
                     {count}), not mid-fixture: {e}"
                );
                break;
            }
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
    // Tag-4 (`TxOutCompactDH`): full-consumption layout
    //   [4][addr][value(tag=1 multi-asset)][BinaryData datum].
    // value(tag1) = tag, VarLen(coin), VarLen(numMA), VarLen(rep_len), rep_bytes.
    // BinaryData  = VarLen(len) ‖ raw_cbor.
    let mut val = vec![4, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(1); // value tag = 1 (multi-asset)
    val.push(0x00); // VarLen coin = 0
    val.push(0x00); // VarLen numMA = 0
    val.push(0x00); // VarLen rep_len = 0 (empty rep)
                    // BinaryData datum: VarLen(2) ‖ [0xAA, 0xBB].
    val.push(0x02);
    val.extend_from_slice(&[0xAA, 0xBB]);
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 4);
    assert!(txout.multi_asset.is_some());
    assert_eq!(txout.datum.as_deref(), Some(&[0xAA, 0xBB][..]));
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
fn test_decode_mempack_txout_tag5_ada_only_datum_and_script() {
    // Tag-5 (`TxOutCompactRefScript`): full-consumption layout
    //   [5][addr][value(tag=0 ADA-only)][Datum option][Script].
    // Datum option 0 = NoDatum. Script: AlonzoScript tag 0 = NativeScript whose
    // MemoBytes body is `VarLen(len) ‖ raw_cbor` — here VarLen(0) = empty.
    let mut val = vec![5, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(0); // ADA-only value tag
    val.push(0x00); // VarLen coin = 0
    val.push(0x00); // Datum option = 0 (NoDatum)
    val.push(0x00); // AlonzoScript tag 0 (NativeScript)
    val.push(0x00); // MemoBytes VarLen(len) = 0 (empty native script body)
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 5);
    assert!(txout.datum.is_none());
    assert!(txout.datum_hash.is_none());
    // Reference-script blob = the AlonzoScript bytes [tag=0, VarLen=0].
    assert_eq!(txout.script_ref.as_deref(), Some(&[0x00, 0x00][..]));
    assert!(txout.opaque_tail.is_none());
}

#[test]
fn test_decode_mempack_txout_tag5_multi_asset() {
    // Tag-5 multi-asset full-consumption layout:
    //   [5][addr][value(tag=1)][Datum option=0][Script tag0 VarLen0].
    let mut val = vec![5, 29];
    val.extend_from_slice(&[0x70u8; 29]);
    val.push(1); // multi-asset value tag
    val.push(0x00); // VarLen coin = 0
    val.push(0x00); // VarLen numMA = 0
    val.push(0x00); // VarLen rep_len = 0 (empty rep)
    val.push(0x00); // Datum option = 0 (NoDatum)
    val.push(0x00); // AlonzoScript tag 0 (NativeScript)
    val.push(0x00); // MemoBytes VarLen(len) = 0 (empty)
    let (txout, consumed) = decode_mempack_txout(&val).unwrap();
    assert_eq!(consumed, val.len());
    assert_eq!(txout.tag, 5);
    assert!(txout.multi_asset.is_some());
    assert_eq!(txout.script_ref.as_deref(), Some(&[0x00, 0x00][..]));
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
    let err = decode_mempack_txin(&[0u8; 10], TxIxEndianness::Little).unwrap_err();
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

// ── F1: aeson FIRST-wins duplicate-key resolution for tablesCodecVersion ──────
//
// `loadSnapshotMetadata` reads the meta with `Aeson.eitherDecode` (the default
// `json` parser), whose haddock states it "keeps only the first occurrence of
// each key, using Data.Aeson.KeyMap.fromList". serde_json keeps the LAST
// occurrence, so a duplicate-key meta would diverge from Haskell. These tests
// pin the byte-exact FIRST-wins classification AND literal value.

use super::{parse_tables_codec_version, TxIxEndianness};

#[test]
fn test_tables_codec_version_duplicate_first_number_last_string_imports() {
    // first occurrence is Number 1, a later duplicate is String "x".
    // aeson default `json` => FIRST wins => Number 1 => Ok(1) => Big (imports).
    let meta =
        br#"{"backend":"utxohd-mem","checksum":0,"tablesCodecVersion":1,"tablesCodecVersion":"x"}"#;
    let v = parse_tables_codec_version(meta).expect("first-wins Number 1 must parse as Ok(1)");
    assert_eq!(v, 1);
    assert_eq!(
        TxIxEndianness::from_tables_codec_version(Some(v)).unwrap(),
        TxIxEndianness::Big
    );
}

#[test]
fn test_tables_codec_version_duplicate_first_string_last_number_rejects() {
    // first occurrence is String "x", a later duplicate is Number 1.
    // aeson default `json` => FIRST wins => String => typeMismatch "Number" => Err.
    let meta =
        br#"{"backend":"utxohd-mem","checksum":0,"tablesCodecVersion":"x","tablesCodecVersion":1}"#;
    assert!(
        parse_tables_codec_version(meta).is_err(),
        "first-wins String must be rejected even when a later duplicate is Number 1"
    );
}

#[test]
fn test_tables_codec_version_no_duplicate_number_one_unchanged() {
    // Common (non-duplicate) case stays byte-identical: Number 1 => Ok(1).
    let meta = br#"{"backend":"utxohd-mem","checksum":2409556997,"tablesCodecVersion":1}"#;
    assert_eq!(parse_tables_codec_version(meta).unwrap(), 1);
}

#[test]
fn test_tables_codec_version_no_duplicate_string_rejected() {
    // Common (non-duplicate) String value stays a hard error.
    let meta = br#"{"backend":"utxohd-mem","checksum":0,"tablesCodecVersion":"1"}"#;
    assert!(parse_tables_codec_version(meta).is_err());
}

#[test]
fn test_tables_codec_version_duplicate_first_null_last_number_rejects() {
    // first occurrence is null => MetadataInvalid (mandatory `.:` fails) => Err,
    // even though a later duplicate is a valid Number.
    let meta = br#"{"tablesCodecVersion":null,"tablesCodecVersion":1}"#;
    assert!(parse_tables_codec_version(meta).is_err());
}

#[test]
fn test_tables_codec_version_duplicate_first_number_two_last_one_uses_first() {
    // first=2 (unknown version), last=1 (accepted). FIRST wins => 2 => parses Ok(2)
    // here (range/integral check passes) but `from_tables_codec_version` rejects 2.
    let meta = br#"{"tablesCodecVersion":2,"tablesCodecVersion":1}"#;
    let v = parse_tables_codec_version(meta).expect("Number 2 is a valid Word8");
    assert_eq!(v, 2);
    assert!(TxIxEndianness::from_tables_codec_version(Some(v)).is_err());
}

// ── R1 (#10 round-5): codec-version VALUE is structure-scoped to the TOP-LEVEL ──
//
// Aeson `o .: "tablesCodecVersion"` does a TOP-LEVEL `KeyMap.lookup` ONLY; it never
// matches an identically-named key nested inside a sibling's object/array value. The
// previous `extract_raw_number_literal` was a FLAT byte scan that matched the key
// ANYWHERE, so the gate (top-level, first-wins) and the value (flat scan) could
// DISAGREE — making dugite stricter than aeson and rejecting a snapshot aeson loads.
// These tests pin that the value now comes from the SAME top-level resolution as the
// gate: a nested `tablesCodecVersion` is invisible.

#[test]
fn test_tables_codec_version_nested_in_object_value_ignored_top_level_wins() {
    // A SIBLING field's object value contains `"tablesCodecVersion":99`; the real
    // top-level field is 1. aeson reads the TOP-LEVEL 1 and imports (BigEndian). The
    // old flat scan would have returned 99 => from_tables_codec_version(99) => hard
    // error. Top-level MUST win => Ok(1) => Big.
    let meta =
        br#"{"backend":"utxohd-mem","extra":{"tablesCodecVersion":99},"checksum":0,"tablesCodecVersion":1}"#;
    let v = parse_tables_codec_version(meta)
        .expect("nested tablesCodecVersion:99 must be ignored; top-level 1 wins => Ok(1)");
    assert_eq!(v, 1);
    assert_eq!(
        TxIxEndianness::from_tables_codec_version(Some(v)).unwrap(),
        TxIxEndianness::Big
    );
}

#[test]
fn test_tables_codec_version_nested_in_array_value_ignored_top_level_wins() {
    // A sibling ARRAY value contains an object with `"tablesCodecVersion":7`; the real
    // top-level field is 1. Top-level MUST win => Ok(1).
    let meta =
        br#"{"history":[{"tablesCodecVersion":7}],"tablesCodecVersion":1,"backend":"utxohd-mem"}"#;
    let v = parse_tables_codec_version(meta)
        .expect("tablesCodecVersion nested in an array value must be ignored; top-level 1 wins");
    assert_eq!(v, 1);
}

#[test]
fn test_tables_codec_version_nested_before_top_level_does_not_shadow() {
    // The nested occurrence appears BEFORE the top-level one in source order; the flat
    // scan would have hit the nested `:99` first. The structure-scoped walk skips the
    // whole nested object and reads only the top-level 1.
    let meta =
        br#"{"extra":{"a":{"tablesCodecVersion":99},"b":"tablesCodecVersion"},"tablesCodecVersion":1}"#;
    let v =
        parse_tables_codec_version(meta).expect("structure-scoped: top-level 1 wins over nested");
    assert_eq!(v, 1);
}

#[test]
fn test_tables_codec_version_top_level_float_syntax_still_accepted() {
    // The structure-scoped literal extraction must preserve the EXACT top-level token,
    // so the float-syntax integral form `1.0` still normalises to 1 and imports.
    let meta = br#"{"backend":"utxohd-mem","tablesCodecVersion":1.0}"#;
    assert_eq!(parse_tables_codec_version(meta).unwrap(), 1);
}

#[test]
fn test_tables_codec_version_top_level_nonintegral_still_rejected() {
    // And a non-integral top-level literal is still rejected on the exact token, even
    // if a nested integral 1 exists.
    let meta = br#"{"extra":{"tablesCodecVersion":1},"tablesCodecVersion":1.0000000000000001}"#;
    assert!(
        parse_tables_codec_version(meta).is_err(),
        "top-level sub-ULP non-integral must be rejected on its exact literal"
    );
}

// ── R3 (#10 round-5): indefinite map truncated at an entry boundary HARD-ERRORS ──
//
// The tables blob is `array(1) [ map(indefinite) { … } ]` (0xbf … 0xff). An
// indefinite-length CBOR map MUST be terminated by a 0xff break byte (RFC 8949
// §3.2.1). A blob TRUNCATED exactly at an entry boundary — a complete (key,value)
// pair but NO trailing 0xff — used to hit `remaining.is_empty()` and silently return
// None, importing the truncated PREFIX as a complete UTxO set. Haskell `loadSnapshot`
// (`readIncremental … valuesMKDecoder`) surfaces partial-EOF as
// `InitFailureRead.ReadSnapshotFailed` and ABORTS. These tests pin the abort.

/// A real, fully-consuming MemPack tag-0 TxOut value (34 bytes), cross-checked against
/// Koios: tx 00002435…#2, 1_814_145 lovelace. Reused as the value of every fixture
/// entry below so each (key,value) pair is a COMPLETE, decodable map entry.
const SAMPLE_TXOUT_VALUE_HEX: &str =
    "001d60986cdecfc4f555a8605d621505a4a82c25c574f59fd0b79e2acdaf0200eedd01";

/// Build a CBOR `bytes(len)` major-2 item from `payload`. `len` is small here
/// (34 for both key and value), so the header is `0x58 <len-byte>`.
fn cbor_bytes(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + payload.len());
    assert!(payload.len() <= u8::MAX as usize);
    out.push(0x58); // major 2 (byte string), additional-info 24 (one length byte)
    out.push(payload.len() as u8);
    out.extend_from_slice(payload);
    out
}

/// Build one complete tvar map entry: a 34-byte TxIn key (32-byte TxId ‖ 2-byte TxIx)
/// and the sample TxOut value, each as a CBOR byte string.
fn tvar_entry(txix: u16) -> Vec<u8> {
    let mut key = vec![0xaau8; 32]; // arbitrary TxId
    key.extend_from_slice(&txix.to_be_bytes()); // BigEndian TxIx (codec v1)
    let val = hex::decode(SAMPLE_TXOUT_VALUE_HEX).unwrap();
    let mut entry = cbor_bytes(&key);
    entry.extend_from_slice(&cbor_bytes(&val));
    entry
}

/// `array(1) [ map(indefinite) { entries… } ` — WITHOUT the trailing 0xff break.
fn tvar_indefinite_no_break(n_entries: u16) -> Vec<u8> {
    let mut blob = vec![0x81u8, 0xbf]; // array(1), map(indefinite, 0xbf)
    for ix in 0..n_entries {
        blob.extend_from_slice(&tvar_entry(ix));
    }
    blob // NOTE: no 0xff — truncated at an entry boundary
}

#[test]
fn test_tvar_indefinite_map_truncated_at_entry_boundary_hard_errors() {
    // 0xbf + N complete entries + EOF (no 0xff). The (N+1)th `next()` must yield
    // Some(Err) (partial-EOF), NOT None — never a silent partial import.
    let blob = tvar_indefinite_no_break(3);
    let mut iter = TvarIterator::new_with_endianness(&blob, TxIxEndianness::Big).unwrap();
    // 3 entries decode cleanly.
    for i in 0..3 {
        let item = iter
            .next()
            .unwrap_or_else(|| panic!("entry {i} should be Some"));
        assert!(item.is_ok(), "entry {i} should decode: {item:?}");
    }
    // The 4th `next()` hits EOF at an entry boundary on an UNTERMINATED indefinite map.
    let end = iter.next();
    match end {
        Some(Err(e)) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("TRUNCATED") || msg.contains("break") || msg.contains("partial-EOF"),
                "expected an indefinite-map-missing-break truncation error, got: {e}"
            );
        }
        other => {
            panic!("indefinite map truncated at entry boundary must yield Some(Err), got {other:?}")
        }
    }
}

#[test]
fn test_tvar_indefinite_map_with_break_completes_clean() {
    // The SAME entries, properly 0xff-terminated, complete clean (all Ok, then None).
    let mut blob = tvar_indefinite_no_break(3);
    blob.push(0xff); // proper indefinite-map break
    let mut iter = TvarIterator::new_with_endianness(&blob, TxIxEndianness::Big).unwrap();
    for i in 0..3 {
        let item = iter
            .next()
            .unwrap_or_else(|| panic!("entry {i} should be Some"));
        assert!(item.is_ok(), "entry {i} should decode: {item:?}");
    }
    assert!(
        iter.next().is_none(),
        "a 0xff-terminated indefinite map must end cleanly with None"
    );
}

#[test]
fn test_tvar_definite_map_completes_clean_at_count() {
    // A DEFINITE-length map (0xa1 = map of 1 entry) carries NO break byte and
    // legitimately ends at exhaustion of its declared entries: clean None, never an
    // R3 truncation error.
    let mut blob = vec![0x81u8, 0xa1]; // array(1), map(1 entry, definite)
    blob.extend_from_slice(&tvar_entry(0));
    let mut iter = TvarIterator::new_with_endianness(&blob, TxIxEndianness::Big).unwrap();
    let first = iter.next().expect("first entry present");
    assert!(first.is_ok(), "definite-map entry should decode: {first:?}");
    assert!(
        iter.next().is_none(),
        "a definite-length map must end cleanly with None at its declared count"
    );
}

#[test]
fn test_tvar_empty_indefinite_map_with_break_is_clean() {
    // 0xbf 0xff — an empty but PROPERLY TERMINATED indefinite map is clean (None),
    // distinct from a truncated one.
    let blob = [0x81u8, 0xbf, 0xff];
    let mut iter = TvarIterator::new_with_endianness(&blob, TxIxEndianness::Big).unwrap();
    assert!(iter.next().is_none());
}

#[test]
fn test_tvar_empty_indefinite_map_without_break_hard_errors() {
    // 0xbf then EOF (no entries, no break) — still a truncated indefinite map => Err.
    let blob = [0x81u8, 0xbf];
    let mut iter = TvarIterator::new_with_endianness(&blob, TxIxEndianness::Big).unwrap();
    match iter.next() {
        Some(Err(_)) => {}
        other => panic!("empty unterminated indefinite map must yield Some(Err), got {other:?}"),
    }
}

// ── #17: snapshot-level CRC (crcOfConcat) verification ──────────────────────

use super::{parse_snapshot_checksum, snapshot_crc_of_concat};

#[test]
fn snapshot_crc_of_concat_matches_real_preprod_fixtures() {
    // Byte-exact vs TWO REAL preprod cardano-node snapshots (db-preprod-sync/haskell-
    // ledger). The stored `checksum` is crc32(ascii_decimal(crc32(state)) ++
    // ascii_decimal(crc32(tables))), NOT crc32(state ++ tables). The per-file CRC
    // inputs were measured on disk (#17 analyze w2ez2r1lk) and the meta `checksum`
    // values are taken verbatim from each snapshot's `meta` file.
    assert_eq!(
        snapshot_crc_of_concat(2003040462, Some(4175236221)),
        2409556997,
        "fixture 124995007"
    );
    assert_eq!(
        snapshot_crc_of_concat(226322584, Some(1678180760)),
        4213652121,
        "fixture 124999169"
    );
}

#[test]
fn snapshot_crc_of_concat_is_decimal_ascii_fold_not_raw_concat() {
    let (a, b) = (2003040462u32, 4175236221u32);
    // The fold is over the DECIMAL-ASCII rendering of each CRC, concatenated.
    assert_eq!(
        snapshot_crc_of_concat(a, Some(b)),
        crc32fast::hash(b"20030404624175236221"),
    );
    // It is NOT a CRC over the raw little-endian CRC bytes (the naive interpretation).
    let mut raw = Vec::new();
    raw.extend_from_slice(&a.to_le_bytes());
    raw.extend_from_slice(&b.to_le_bytes());
    assert_ne!(snapshot_crc_of_concat(a, Some(b)), crc32fast::hash(&raw));
}

#[test]
fn snapshot_crc_of_concat_tables_absent_folds_to_state() {
    // Haskell `maybe crc1 (crcOfConcat crc1) crc2`: no tables file ⇒ state-only crc1.
    assert_eq!(snapshot_crc_of_concat(123_456_789, None), 123_456_789);
}

#[test]
fn snapshot_crc_detects_single_byte_corruption() {
    // Negative-security property: flipping ONE byte of the state blob changes
    // crc32(state), hence the computed crcOfConcat, hence the import-time
    // `computed != expected` check fires and the corrupt snapshot is rejected.
    let state = b"the quick brown fox state blob";
    let tables = b"tables blob bytes";
    let good = snapshot_crc_of_concat(crc32fast::hash(state), Some(crc32fast::hash(tables)));
    let mut corrupt = state.to_vec();
    corrupt[0] ^= 0x01;
    let bad = snapshot_crc_of_concat(crc32fast::hash(&corrupt), Some(crc32fast::hash(tables)));
    assert_ne!(
        good, bad,
        "a corrupted state blob must produce a different checksum"
    );
    // And corrupting the tables blob is likewise caught.
    let mut corrupt_t = tables.to_vec();
    corrupt_t[0] ^= 0x01;
    let bad_t = snapshot_crc_of_concat(crc32fast::hash(state), Some(crc32fast::hash(&corrupt_t)));
    assert_ne!(
        good, bad_t,
        "a corrupted tables blob must produce a different checksum"
    );
}

#[test]
fn parse_snapshot_checksum_valid() {
    let meta = br#"{"backend":"utxohd-mem","checksum":2409556997,"tablesCodecVersion":1}"#;
    assert_eq!(parse_snapshot_checksum(meta).unwrap(), 2_409_556_997);
    // Full Word32 range boundary.
    assert_eq!(
        parse_snapshot_checksum(br#"{"checksum":4294967295}"#).unwrap(),
        u32::MAX
    );
    // aeson first-wins on a duplicate top-level key.
    assert_eq!(
        parse_snapshot_checksum(br#"{"checksum":7,"checksum":9}"#).unwrap(),
        7
    );
    // aeson accepts float-syntax integral forms (Scientific.toBoundedInteger): 100e-2 == 1.
    assert_eq!(
        parse_snapshot_checksum(br#"{"checksum":100e-2}"#).unwrap(),
        1
    );
}

#[test]
fn parse_snapshot_checksum_rejects_invalid() {
    // absent (mandatory `fmap CRC (o .: "checksum")` fails)
    assert!(parse_snapshot_checksum(br#"{"backend":"utxohd-mem"}"#).is_err());
    // null
    assert!(parse_snapshot_checksum(br#"{"checksum":null}"#).is_err());
    // JSON string, not a Number
    assert!(parse_snapshot_checksum(br#"{"checksum":"123"}"#).is_err());
    // out of Word32 range (u32::MAX + 1)
    assert!(parse_snapshot_checksum(br#"{"checksum":4294967296}"#).is_err());
    // negative
    assert!(parse_snapshot_checksum(br#"{"checksum":-1}"#).is_err());
    // non-integral
    assert!(parse_snapshot_checksum(br#"{"checksum":1.5}"#).is_err());
    // not a JSON object
    assert!(parse_snapshot_checksum(br#"[1,2,3]"#).is_err());
}

// ── #20 hardening (c): backend dup-key must be aeson FIRST-wins ──────────────

use super::enforce_snapshot_backend_is_utxohd_mem as enforce_backend;

#[test]
fn backend_enforce_is_aeson_first_wins_on_duplicate_key() {
    // Single valid backend → Ok (unchanged behavior).
    assert!(enforce_backend(br#"{"backend":"utxohd-mem","tablesCodecVersion":1}"#).is_ok());
    // Duplicate `backend`: aeson keeps the FIRST occurrence.
    //  first valid  → Ok.
    assert!(enforce_backend(br#"{"backend":"utxohd-mem","backend":"lsm"}"#).is_ok());
    //  first invalid → Err. THIS is the fix: serde_json `Value::get` (last-wins) would
    //  have WRONGLY accepted the second "utxohd-mem"; aeson first-wins keeps "lsm" → reject.
    assert!(enforce_backend(br#"{"backend":"lsm","backend":"utxohd-mem"}"#).is_err());
    // absent / null / non-string / wrong-string → Err (unchanged).
    assert!(enforce_backend(br#"{"tablesCodecVersion":1}"#).is_err());
    assert!(enforce_backend(br#"{"backend":null}"#).is_err());
    assert!(enforce_backend(br#"{"backend":123}"#).is_err());
    assert!(enforce_backend(br#"{"backend":"lsm"}"#).is_err());
    // not a JSON object → Err (aeson withObject).
    assert!(enforce_backend(br#"["utxohd-mem"]"#).is_err());
}

// ── #20 hardening (a): decode_varlen Word64 overflow guard (mempack-exact) ───

#[test]
fn varlen_max_u64_still_ok_after_overflow_guard() {
    // u64::MAX must STILL decode Ok. MS byte 0x81 = 1000_0001: continuation +
    // only bit 0 (the 2^63 bit). 0x81 & 0xFE == 0x80 → passes the guard.
    let bytes = [0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
    assert_eq!(decode_varlen(&bytes).unwrap(), (u64::MAX, 10));
}

#[test]
fn varlen_overflow_10byte_msbyte_rejected() {
    // 10-byte form whose MS byte has an overflow payload bit set → value > u64::MAX.
    // MS byte 0x83 = 1000_0011: bit 1 set. 0x83 & 0xFE == 0x82 != 0x80 → Err.
    // Haskell unpack7BitVarLenLast(0b1111_1110) fails here; dugite previously
    // returned a TRUNCATED Ok (high bits silently dropped by `acc << 7`).
    let bytes = [0x83, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
    assert!(
        decode_varlen(&bytes).is_err(),
        "10-byte VarLen with overflow MS byte must Err (matches mempack)"
    );
    // All-high MS byte 0xff (0xff & 0xfe == 0xfe != 0x80) → also rejected.
    let all_high = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
    assert!(decode_varlen(&all_high).is_err());
}

#[test]
fn varlen_non_minimal_submaximal_still_accepted() {
    // mempack does NOT reject non-minimal sub-maximal encodings; we must match
    // (a stricter check could refuse a valid snapshot). 0x80 0x00 = 0 in 2 bytes.
    assert_eq!(decode_varlen(&[0x80, 0x00]).unwrap(), (0, 2));
}
