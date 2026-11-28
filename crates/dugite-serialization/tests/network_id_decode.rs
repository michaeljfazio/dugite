//! Transaction body key 15 (`network_id`) on the block path.
//!
//! Haskell's `Network` `DecCBOR` is `decodeWord8` followed by `word8ToNetwork`
//! (`Cardano.Ledger.BaseTypes`), which fails with "Unknown network id" for every value but
//! 0 (Testnet) and 1 (Mainnet). cborg's `decodeWord8` accepts any head width, so `18 01` and
//! `1b 0000000000000001` are valid spellings of 1. The Alonzo, Babbage, Conway and Dijkstra
//! body decoders (the latter for sub-transaction bodies too) all use it for key 15, so a body
//! carrying another value must not decode (dugite used to map it to "no network id").

use dugite_serialization::{decode_block, decode_block_minimal, decode_transaction};

fn load_vector(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/test_vectors/{}.hex",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap()
}

/// `{0: [], 1: [], 2: 0, 15: <network id item>}`
fn body_with_network_id(item: &[u8]) -> Vec<u8> {
    let mut b = vec![0xa4, 0x00, 0x80, 0x01, 0x80, 0x02, 0x00, 0x0f];
    b.extend_from_slice(item);
    b
}

fn standalone(body: &[u8]) -> Vec<u8> {
    let mut t = vec![0x84];
    t.extend_from_slice(body);
    t.extend([0xa0, 0xf5, 0xf6]);
    t
}

fn block_with_body(body: &[u8]) -> Vec<u8> {
    let full = load_vector("conway");
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    let era = d.u64().unwrap();
    d.array().unwrap();
    let hs = d.position();
    d.skip().unwrap();
    let mut blk = vec![0x82, era as u8, 0x85];
    blk.extend_from_slice(&full[hs..d.position()]);
    blk.push(0x81);
    blk.extend_from_slice(body);
    blk.extend([0x81, 0xa0, 0xa0, 0x80]);
    blk
}

/// (name, item bytes, decoded value)
const VALID: &[(&str, &[u8], u8)] = &[
    ("0", &[0x00], 0),
    ("1", &[0x01], 1),
    ("1800 (wide head)", &[0x18, 0x00], 0),
    ("1801 (wide head)", &[0x18, 0x01], 1),
    ("1900 01 (wide head)", &[0x19, 0x00, 0x01], 1),
    (
        "1b 0..01 (widest head)",
        &[0x1b, 0, 0, 0, 0, 0, 0, 0, 0x01],
        1,
    ),
];

const INVALID: &[(&str, &[u8])] = &[
    // dataset `network_id/manual-invalid/0001-other` and `0002-minus-one`
    ("2", &[0x02]),
    ("-1", &[0x20]),
    ("255", &[0x18, 0xff]),
    ("256 (does not fit a Word8)", &[0x19, 0x01, 0x00]),
    ("4294967297", &[0x1b, 0, 0, 0, 1, 0, 0, 0, 0x01]),
    ("text", &[0x61, 0x30]),
    ("null", &[0xf6]),
];

#[test]
fn valid_network_ids_decode_in_every_spelling() {
    for (name, item, expected) in VALID {
        let body = body_with_network_id(item);
        let tx =
            decode_transaction(6, &standalone(&body)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(tx.body.network_id, Some(*expected), "{name}");
        let blk = block_with_body(&body);
        for (path, r) in [
            ("decode_block", decode_block(&blk)),
            ("decode_block_minimal", decode_block_minimal(&blk)),
        ] {
            let b = r.unwrap_or_else(|e| panic!("{name} {path}: {e}"));
            assert_eq!(b.transactions[0].body.network_id, Some(*expected));
        }
        // Babbage and Alonzo bodies share the key.
        for era in [4u16, 5] {
            decode_transaction(era, &standalone(&body))
                .unwrap_or_else(|e| panic!("{name} era {era}: {e}"));
        }
    }
}

#[test]
fn invalid_network_ids_are_rejected_on_every_path() {
    for (name, item) in INVALID {
        let body = body_with_network_id(item);
        for era in [4u16, 5, 6] {
            assert!(
                decode_transaction(era, &standalone(&body)).is_err(),
                "{name}: submission era {era}"
            );
        }
        let blk = block_with_body(&body);
        assert!(decode_block(&blk).is_err(), "{name}: decode_block");
        assert!(decode_block_minimal(&blk).is_err(), "{name}: minimal");
    }
}

#[test]
fn a_body_without_network_id_still_decodes() {
    let body = vec![0xa3, 0x00, 0x80, 0x01, 0x80, 0x02, 0x00];
    let tx = decode_transaction(6, &standalone(&body)).unwrap();
    assert_eq!(tx.body.network_id, None);
}

/// Dijkstra sub-transaction bodies use the same `Network` decoder (`bodyFields 15` is level
/// generic in `Dijkstra/TxBody.hs`).
#[test]
fn dijkstra_sub_transaction_network_id() {
    let dijkstra = |net: &[u8]| {
        let sub = {
            // sub-transaction body: no fee (key 2); `[body, wits, aux]`
            let mut s = vec![0x83, 0xa3, 0x00, 0x80, 0x01, 0x80, 0x0f];
            s.extend_from_slice(net);
            s.extend([0xa0, 0xf6]);
            s
        };
        // top-level body {0:[],1:[],2:0, 23: [sub]}, Dijkstra tx = [body, wits, aux]
        let mut b = vec![0xa4, 0x00, 0x80, 0x01, 0x80, 0x02, 0x00, 0x17, 0x81];
        b.extend(sub);
        let mut t = vec![0x83];
        t.extend(b);
        t.extend([0xa0, 0xf6]);
        t
    };
    let ok = decode_transaction(7, &dijkstra(&[0x01])).expect("sub-tx with network id 1");
    assert_eq!(ok.body.sub_transactions[0].network_id, Some(1));
    assert!(decode_transaction(7, &dijkstra(&[0x02])).is_err());
}
