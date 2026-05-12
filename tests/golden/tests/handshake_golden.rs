//! CBOR golden tests for the N2N / N2C handshake protocol.
//!
//! The full handshake message is `[tag, payload]`, where:
//!   - tag 0 = MsgProposeVersions  — payload = `{ * versionNumber => versionData }`
//!   - tag 1 = MsgAcceptVersion    — payload = `[versionNumber, versionData]`
//!     (note: encoded as a 3-tuple `[1, versionNumber, versionData]` on the wire)
//!   - tag 2 = MsgRefuse           — payload = refuseReason
//!
//! N2N versionData (V14/V15): `[networkMagic, initiatorOnly, peerSharing(0|1), query]`
//! N2C versionData (V16+):    `[networkMagic, query]`
//!
//! For N2C the wire-level version number is `v | 0x8000` (bit-15 set).
//!
//! Sources:
//!   - Blueprint test vectors at `cardano-blueprint/src/network/node-to-node/
//!     handshake/test-data/` (already covered in `cbor_golden.rs`).
//!   - dugite production encoders in `dugite-network::handshake::{n2n, n2c}`.

use dugite_network::handshake::n2c::{
    encode_n2c_version, is_n2c_version, N2CVersionData, N2C_V16, N2C_V17, N2C_V18, N2C_V19,
    N2C_V20, N2C_V21, N2C_V22, N2C_V23,
};
use dugite_network::handshake::n2n::{N2NVersionData, N2N_V14, N2N_V15};
use minicbor::Encoder;

fn h(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

// ---------------------------------------------------------------------------
// N2N V14 / V15 version data
// ---------------------------------------------------------------------------

#[test]
fn golden_n2n_v14_version_data_mainnet() {
    // mainnet magic = 764824073 = 0x2D964A09
    // [764824073, false, 0, false]
    let data = N2NVersionData::new(764824073, false, false);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    data.encode(&mut enc);
    // 0x84 = array(4)
    // 0x1A 2D 96 4A 09 = uint(764824073)
    // 0xF4 = false
    // 0x00 = uint(0) — peer_sharing disabled
    // 0xF4 = false
    assert_eq!(buf, h("841a2d964a09f400f4"));
}

#[test]
fn golden_n2n_v15_version_data_preview_with_peersharing() {
    // preview magic = 2
    // [2, false, 1, false]
    let data = N2NVersionData::new(2, false, true);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    data.encode(&mut enc);
    assert_eq!(buf, h("8402f401f4"));
}

#[test]
fn golden_n2n_v15_version_data_initiator_only() {
    // [magic=1, true, 0, false] — initiatorOnly mode
    let data = N2NVersionData::new(1, true, false);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    data.encode(&mut enc);
    assert_eq!(buf, h("8401f500f4"));
}

#[test]
fn golden_n2n_propose_versions_v14_v15() {
    // Full MsgProposeVersions for both V14 and V15 on preview.
    // [0, { 14: [2, false, 1, false], 15: [2, false, 1, false] }]
    let data = N2NVersionData::new(2, false, true);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).unwrap();
    enc.u64(0).unwrap(); // MsgProposeVersions tag
    enc.map(2).unwrap();
    enc.u16(N2N_V14).unwrap();
    data.encode(&mut enc);
    enc.u16(N2N_V15).unwrap();
    data.encode(&mut enc);
    // 0x82 0x00 0xA2 0x0E [vd14] 0x0F [vd15]
    let expected = h("8200a20e8402f401f40f8402f401f4");
    assert_eq!(buf, expected);
}

#[test]
fn golden_n2n_accept_version_v15() {
    // [1, 15, [2, false, 1, false]]
    let data = N2NVersionData::new(2, false, true);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(3).unwrap();
    enc.u64(1).unwrap();
    enc.u16(N2N_V15).unwrap();
    data.encode(&mut enc);
    assert_eq!(buf, h("83010f8402f401f4"));
}

#[test]
fn golden_n2n_refuse_version_mismatch() {
    // [2, [0, [14, 15]]] — RefuseReasonVersionMismatch with our supported versions.
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).unwrap();
    enc.u64(2).unwrap();
    enc.array(2).unwrap();
    enc.u64(0).unwrap();
    enc.array(2).unwrap();
    enc.u16(14).unwrap();
    enc.u16(15).unwrap();
    assert_eq!(buf, h("82028200820e0f"));
}

// ---------------------------------------------------------------------------
// N2C V16-V23 version data and bit-15 encoding
// ---------------------------------------------------------------------------

#[test]
fn golden_n2c_bit15_encoding_all_versions() {
    // The N2C wire version is bit-15 OR'd against the logical version.
    // V16 -> 0x8010 = 32784
    // V23 -> 0x8017 = 32791
    assert_eq!(encode_n2c_version(N2C_V16), 0x8010);
    assert_eq!(encode_n2c_version(N2C_V17), 0x8011);
    assert_eq!(encode_n2c_version(N2C_V18), 0x8012);
    assert_eq!(encode_n2c_version(N2C_V19), 0x8013);
    assert_eq!(encode_n2c_version(N2C_V20), 0x8014);
    assert_eq!(encode_n2c_version(N2C_V21), 0x8015);
    assert_eq!(encode_n2c_version(N2C_V22), 0x8016);
    assert_eq!(encode_n2c_version(N2C_V23), 0x8017);
    for v in [
        N2C_V16, N2C_V17, N2C_V18, N2C_V19, N2C_V20, N2C_V21, N2C_V22, N2C_V23,
    ] {
        assert!(is_n2c_version(encode_n2c_version(v)));
    }
}

#[test]
fn golden_n2c_version_data_preview() {
    let data = N2CVersionData::new(2);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    data.encode(&mut enc);
    // [2, false] = 0x82 0x02 0xF4
    assert_eq!(buf, h("8202f4"));
}

#[test]
fn golden_n2c_version_data_mainnet_query_mode() {
    let data = N2CVersionData {
        network_magic: 764824073,
        query: true,
    };
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    data.encode(&mut enc);
    // [764824073, true] = 0x82 0x1A 2D964A09 0xF5
    assert_eq!(buf, h("821a2d964a09f5"));
}

#[test]
fn golden_n2c_propose_versions_v16_through_v22() {
    // Full MsgProposeVersions for V16-V22 on preview.
    // Wire version numbers are bit-15 encoded; in CBOR they are encoded as
    // u16/u32 depending on numeric magnitude.
    let data = N2CVersionData::new(2);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).unwrap();
    enc.u64(0).unwrap();
    let versions = [
        N2C_V16, N2C_V17, N2C_V18, N2C_V19, N2C_V20, N2C_V21, N2C_V22,
    ];
    enc.map(versions.len() as u64).unwrap();
    for v in versions {
        enc.u32(encode_n2c_version(v) as u32).unwrap();
        data.encode(&mut enc);
    }

    // First two bytes: 82 00 (outer [0, map]).
    assert_eq!(buf[0], 0x82, "outer array(2)");
    assert_eq!(buf[1], 0x00, "MsgProposeVersions tag = 0");
    // Third byte: map(7) — 0xA7
    assert_eq!(buf[2], 0xA7, "map of 7 versions");
    // Each version key takes 3 bytes (0x19 hh ll = u16 wire form) since
    // bit-15 encoded values are >= 0x8010 (>0xFF and <0x10000).
    // Each version data is 3 bytes [2, false] = 0x82 0x02 0xF4 = 3 bytes
    //   so 3 (key) + 3 (val) = 6 bytes per entry, × 7 = 42 + 3 header bytes = 45.
    assert_eq!(buf.len(), 45);
}

#[test]
fn golden_n2c_accept_version_v22() {
    // [1, encode(V22), [2, false]]
    let data = N2CVersionData::new(2);
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(3).unwrap();
    enc.u64(1).unwrap();
    enc.u32(encode_n2c_version(N2C_V22) as u32).unwrap();
    data.encode(&mut enc);
    // 0x83 01 19 8016 82 02 F4
    assert_eq!(buf, h("8301198016 8202f4".replace(' ', "").as_str()));
}

// ---------------------------------------------------------------------------
// Roundtrip safety
// ---------------------------------------------------------------------------

#[test]
fn n2c_version_data_roundtrip_all_eras() {
    // Each combination must roundtrip without loss.
    for magic in [0u64, 1, 2, 764824073] {
        for query in [false, true] {
            let data = N2CVersionData {
                network_magic: magic,
                query,
            };
            let mut buf = Vec::new();
            let mut enc = Encoder::new(&mut buf);
            data.encode(&mut enc);
            let mut dec = minicbor::Decoder::new(&buf);
            let decoded = N2CVersionData::decode(&mut dec).unwrap();
            assert_eq!(decoded, data);
        }
    }
}
