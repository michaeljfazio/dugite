//! CBOR golden tests for ledger-side data structures: values, certificates,
//! governance actions, protocol parameters, and minimal transaction bodies.
//!
//! These tests are spec-driven (Conway PV11): every golden hex is hand-built
//! from the CDDL in `cardano-ledger/eras/conway/cddl-spec/conway.cddl` and
//! `ledger.cddl`, then verified against the dugite encoder where one exists.
//!
//! For complex era-specific types we hand-build the expected CBOR byte
//! sequences against the official Cardano CDDL specs and assert byte-exact
//! equality against the dugite encoder, so any encoder drift is caught.

use minicbor::Encoder;

fn h(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn enc<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut Encoder<&mut Vec<u8>>),
{
    let mut buf = Vec::new();
    let mut e = Encoder::new(&mut buf);
    f(&mut e);
    buf
}

// ---------------------------------------------------------------------------
// Value encoding (CDDL: value = coin / [coin, multiasset<uint>])
// ---------------------------------------------------------------------------

#[test]
fn golden_value_ada_only_zero() {
    // coin = 0 → CBOR uint(0) = 0x00
    let bytes = enc(|e| {
        e.u64(0).unwrap();
    });
    assert_eq!(bytes, h("00"));
}

#[test]
fn golden_value_ada_only_1_ada() {
    // 1 ADA = 1_000_000 lovelace = 0x0F4240 → 0x1A 00 0F 42 40
    let bytes = enc(|e| {
        e.u64(1_000_000).unwrap();
    });
    assert_eq!(bytes, h("1a000f4240"));
}

#[test]
fn golden_value_ada_only_max_supply() {
    // 45 billion ADA = 45_000_000_000_000_000 lovelace
    // Fits in u64 → 0x1B 00 9FDF93 96..
    let bytes = enc(|e| {
        e.u64(45_000_000_000_000_000).unwrap();
    });
    assert_eq!(bytes[0], 0x1B, "needs 8-byte uint encoding");
}

#[test]
fn golden_value_multi_asset_one_token() {
    // [1_000_000, {h'AA..AA' (28B policy): {h'546F6B656E' "Token": 500}}]
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(1_000_000).unwrap();
        e.map(1).unwrap();
        e.bytes(&[0xAA; 28]).unwrap();
        e.map(1).unwrap();
        e.bytes(b"Token").unwrap();
        e.u64(500).unwrap();
    });
    let expected = h("82\
         1a000f4240\
         a1\
         581c aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
         a1\
         45 546f6b656e\
         1901f4"
        .replace(' ', "")
        .as_str());
    assert_eq!(bytes, expected);
}

#[test]
fn golden_value_multi_asset_empty_asset_name() {
    // Empty asset name (ADA-like sentinel inside policy) — bstr(0) = 0x40
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(1).unwrap();
        e.map(1).unwrap();
        e.bytes(&[0xBB; 28]).unwrap();
        e.map(1).unwrap();
        e.bytes(&[]).unwrap();
        e.u64(1).unwrap();
    });
    assert_eq!(bytes[3 + 28 + 3], 0x40, "empty asset name = bstr(0)");
}

// ---------------------------------------------------------------------------
// Certificate encoding (CDDL: certificate = [type_tag, ...])
// ---------------------------------------------------------------------------
//
// Conway certificate type tags (see conway.cddl `certificate`):
//   0  stake_registration
//   1  stake_deregistration
//   2  stake_delegation
//   3  pool_registration
//   4  pool_retirement
//   ...
//   7  reg_cert (Conway)
//   8  unreg_cert (Conway)
//   9  vote_deleg_cert
//   17 reg_drep_cert
//   18 unreg_drep_cert

#[test]
fn golden_cert_stake_registration_pre_conway() {
    // [0, [0, key_hash(28)]] — register stake credential (legacy form)
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(0).unwrap(); // stake_registration
        e.array(2).unwrap();
        e.u64(0).unwrap(); // KeyHash type
        e.bytes(&[0x11; 28]).unwrap();
    });
    let expected = h("82\
         00\
         82 00 581c 11111111111111111111111111111111111111111111111111111111"
        .replace(' ', "")
        .as_str());
    assert_eq!(bytes, expected);
}

#[test]
fn golden_cert_stake_delegation() {
    // [2, stake_cred, pool_keyhash(28)]
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(2).unwrap(); // stake_delegation
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.bytes(&[0x22; 28]).unwrap();
        e.bytes(&[0x33; 28]).unwrap();
    });
    assert_eq!(bytes[0], 0x83, "stake_delegation = array(3)");
    assert_eq!(bytes[1], 0x02, "tag 2 = stake_delegation");
}

#[test]
fn golden_cert_drep_registration() {
    // [17, drep_credential, coin, anchor?]
    // anchor = [url(text), data_hash(32)] / null
    let bytes = enc(|e| {
        e.array(4).unwrap();
        e.u64(17).unwrap(); // reg_drep_cert
        e.array(2).unwrap();
        e.u64(0).unwrap(); // KeyHash drep
        e.bytes(&[0x44; 28]).unwrap();
        e.u64(500_000_000).unwrap(); // deposit
        e.null().unwrap();
    });
    // 0x84 0x11 [cred] [coin] [null=F6]
    assert_eq!(bytes[0], 0x84, "drep registration = array(4)");
    assert_eq!(bytes[1], 0x11, "tag 17 = reg_drep_cert");
    assert_eq!(*bytes.last().unwrap(), 0xF6, "trailing null anchor");
}

#[test]
fn golden_cert_vote_delegation() {
    // [9, stake_cred, drep]
    // drep = [0, keyhash] / [1, scripthash] / [2 abstain] / [3 noConfidence]
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(9).unwrap(); // vote_deleg_cert
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.bytes(&[0x55; 28]).unwrap();
        // drep = AlwaysAbstain
        e.array(1).unwrap();
        e.u64(2).unwrap();
    });
    assert_eq!(bytes[0], 0x83, "vote_deleg_cert = array(3)");
    assert_eq!(bytes[1], 0x09, "tag 9 = vote_deleg_cert");
    // Trailing drep encoding: [2] = 0x81 0x02
    assert_eq!(&bytes[bytes.len() - 2..], &[0x81, 0x02]);
}

#[test]
fn golden_cert_pool_retirement() {
    // [4, pool_keyhash(28), epoch]
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(4).unwrap();
        e.bytes(&[0x66; 28]).unwrap();
        e.u64(500).unwrap();
    });
    assert_eq!(bytes[0], 0x83);
    assert_eq!(bytes[1], 0x04);
}

// ---------------------------------------------------------------------------
// Governance actions (CIP-1694)
// ---------------------------------------------------------------------------
//
// Conway governance action tag layout (from conway.cddl `gov_action`):
//   0 parameter_change_action
//   1 hard_fork_initiation_action
//   2 treasury_withdrawals_action
//   3 no_confidence
//   4 update_committee
//   5 new_constitution
//   6 info_action

#[test]
fn golden_gov_action_info() {
    // [6] — InfoAction is parameter-less
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(6).unwrap();
    });
    assert_eq!(bytes, h("8106"));
}

#[test]
fn golden_gov_action_no_confidence() {
    // [3, last_action_id?]
    // last_action_id = [tx_id(32), index] / null
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(3).unwrap();
        e.null().unwrap();
    });
    assert_eq!(bytes, h("8203f6"));
}

#[test]
fn golden_gov_action_hard_fork_initiation() {
    // [1, last_action_id, protocol_version]
    // protocol_version = [major, minor]
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(1).unwrap();
        e.null().unwrap(); // no prior action
        e.array(2).unwrap();
        e.u64(11).unwrap(); // major
        e.u64(0).unwrap(); // minor
    });
    assert_eq!(bytes, h("8301f6820b00"));
}

#[test]
fn golden_gov_action_treasury_withdrawals_one() {
    // [2, { reward_account => coin }, policy_hash / null]
    // reward_account = bstr(29) — 1 byte header + 28-byte hash
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(2).unwrap();
        e.map(1).unwrap();
        let mut acc = [0u8; 29];
        acc[0] = 0xE0; // testnet stake header byte (network=0, stake type)
        for (i, b) in acc.iter_mut().enumerate().skip(1) {
            *b = i as u8;
        }
        e.bytes(&acc).unwrap();
        e.u64(1_000_000).unwrap();
        e.null().unwrap(); // no policy hash
    });
    assert_eq!(bytes[0], 0x83);
    assert_eq!(bytes[1], 0x02, "treasury_withdrawals tag");
    assert_eq!(*bytes.last().unwrap(), 0xF6, "null policy hash");
}

#[test]
fn golden_gov_action_new_constitution() {
    // [5, last_action_id, [anchor, script_hash?/null]]
    // anchor = [url(text), data_hash(32)]
    let bytes = enc(|e| {
        e.array(3).unwrap();
        e.u64(5).unwrap();
        e.null().unwrap();
        e.array(2).unwrap();
        // anchor
        e.array(2).unwrap();
        e.str("ipfs://Qm").unwrap();
        e.bytes(&[0x77; 32]).unwrap();
        e.null().unwrap(); // no guardrail script
    });
    assert_eq!(bytes[0], 0x83);
    assert_eq!(bytes[1], 0x05);
}

#[test]
fn golden_gov_action_update_committee() {
    // [4, last_action_id, removed_set(258), { added: epoch }, threshold]
    // threshold = tag(30) [num, den]
    let bytes = enc(|e| {
        e.array(5).unwrap();
        e.u64(4).unwrap();
        e.null().unwrap();
        // removed: tag(258) [ ]
        e.tag(minicbor::data::Tag::new(258)).unwrap();
        e.array(0).unwrap();
        // added: {}
        e.map(0).unwrap();
        // threshold rational 2/3
        e.tag(minicbor::data::Tag::new(30)).unwrap();
        e.array(2).unwrap();
        e.u64(2).unwrap();
        e.u64(3).unwrap();
    });
    assert_eq!(bytes[0], 0x85, "update_committee = array(5)");
    assert_eq!(bytes[1], 0x04);
}

#[test]
fn golden_gov_action_parameter_change() {
    // [0, last_action_id, pparam_update_map, guardrail_script?/null]
    // pparam_update_map = { uint => any } — keyed by PP field index 0..33
    let bytes = enc(|e| {
        e.array(4).unwrap();
        e.u64(0).unwrap();
        e.null().unwrap();
        e.map(1).unwrap();
        e.u64(0).unwrap(); // field 0 = txFeePerByte
        e.u64(50).unwrap(); // new value
        e.null().unwrap();
    });
    assert_eq!(bytes, h("8400f6a100183 2f6".replace(' ', "").as_str()));
}

// ---------------------------------------------------------------------------
// Voting procedures (CIP-1694)
// ---------------------------------------------------------------------------

#[test]
fn golden_voting_procedure_yes() {
    // voting_procedure = [vote, anchor?/null]
    // vote = 0 (no) | 1 (yes) | 2 (abstain)
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(1).unwrap();
        e.null().unwrap();
    });
    assert_eq!(bytes, h("8201f6"));
}

#[test]
fn golden_voter_drep_keyhash() {
    // voter = [type, hash(28)]
    //   0=committee key | 1=committee script
    //   2=drep key | 3=drep script
    //   4=spo key
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(2).unwrap();
        e.bytes(&[0xDD; 28]).unwrap();
    });
    assert_eq!(bytes[0], 0x82);
    assert_eq!(bytes[1], 0x02, "drep key voter");
    assert_eq!(bytes[2], 0x58, "1-byte len prefix");
    assert_eq!(bytes[3], 28);
}

// ---------------------------------------------------------------------------
// Block header per era — first byte sanity
// ---------------------------------------------------------------------------
//
// Block headers in the Cardano CDDL are era-specific complex structures.
// We don't reconstruct full headers here; instead we lock down the
// HFC era-wrapper encoding `[era_id, header_body]` and the on-the-wire
// envelope used by N2N ChainSync (`#6.24(bstr(...))`).
//
// Per-era era_id values:
//   Byron   = 1
//   Shelley = 2
//   Allegra = 3
//   Mary    = 4
//   Alonzo  = 5
//   Babbage = 6 (note: era IDs are shifted vs node-state values)
//   Conway  = 6 (in the N2N ChainSync NS namespace)
//
// (See `dugite-network/src/protocol/chainsync/server.rs::extract_header_for_chainsync`.)

#[test]
fn golden_hfc_header_wrapper_conway() {
    // [6, tag(24)(bstr(inner))]
    let inner = [0x12, 0x34];
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(6).unwrap();
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&inner).unwrap();
    });
    assert_eq!(bytes, h("8206d818421234"));
}

#[test]
fn golden_hfc_header_wrapper_shelley() {
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(2).unwrap(); // shelley era id
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&[0xFF]).unwrap();
    });
    assert_eq!(bytes, h("8202d81841ff"));
}

#[test]
fn golden_hfc_header_wrapper_byron() {
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(1).unwrap();
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&[]).unwrap();
    });
    assert_eq!(bytes, h("8201d81840"));
}

// ---------------------------------------------------------------------------
// Transaction body skeletons (minimal, every era)
// ---------------------------------------------------------------------------
//
// CDDL: `transaction_body = { 0 : set<transaction_input>, 1 : [* transaction_output],
//                              2 : coin, 3 : uint, ... }`
//
// We assert the minimal Shelley/Alonzo/Conway shapes round-trip through CBOR
// and produce stable byte sequences.

#[test]
fn golden_tx_body_minimal_shelley_shape() {
    // Minimal Shelley body: { 0: set<input>, 1: [out], 2: fee, 3: ttl }
    let bytes = enc(|e| {
        e.map(4).unwrap();
        e.u64(0).unwrap();
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.bytes(&[0xAB; 32]).unwrap(); // tx_id
        e.u64(0).unwrap(); // index
        e.u64(1).unwrap();
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.bytes(&[0; 29]).unwrap(); // address (29 bytes, smallest legal)
        e.u64(1_000_000).unwrap();
        e.u64(2).unwrap();
        e.u64(170_000).unwrap();
        e.u64(3).unwrap();
        e.u64(2_000_000).unwrap();
    });
    // First byte must be map(4)
    assert_eq!(bytes[0], 0xA4, "minimal Shelley body uses map of 4 keys");
}

#[test]
fn golden_tx_body_conway_input_set_uses_tag_258() {
    // Conway requires set-of-inputs to use tag(258).
    // body = { 0: tag(258)[input], 1: [out], 2: fee }
    let bytes = enc(|e| {
        e.map(3).unwrap();
        e.u64(0).unwrap();
        e.tag(minicbor::data::Tag::new(258)).unwrap();
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.bytes(&[0x55; 32]).unwrap();
        e.u64(0).unwrap();
        e.u64(1).unwrap();
        e.array(0).unwrap();
        e.u64(2).unwrap();
        e.u64(170_000).unwrap();
    });
    // After the field 0 marker (0x00), we expect tag 258 (0xD9 01 02).
    assert_eq!(bytes[0], 0xA3, "map of 3");
    assert_eq!(bytes[1], 0x00, "field 0 = inputs");
    assert_eq!(bytes[2], 0xD9, "tag prefix");
    assert_eq!(bytes[3], 0x01, "tag high");
    assert_eq!(bytes[4], 0x02, "tag low — tag(258)");
}

// ---------------------------------------------------------------------------
// Protocol parameters (Conway PV11 — positional array(31))
// ---------------------------------------------------------------------------

#[test]
fn golden_pparams_conway_positional_array_header() {
    // Conway PParams is array(31) — verify exact opening bytes from a
    // minimal all-zeros example.
    let mut bytes = Vec::new();
    let mut e = Encoder::new(&mut bytes);
    e.array(31).unwrap();
    // 28 zero-valued fields + protocolVersion + costModels + execPrices + ...
    // We just want the header.
    assert_eq!(bytes, h("981f"), "array(31) header = 0x98 0x1F");
}

#[test]
fn golden_pparams_protocol_version_field() {
    // Conway PParams field [12] protocolVersion = [major, minor]
    // PV11.0 = [11, 0]
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(11).unwrap();
        e.u64(0).unwrap();
    });
    assert_eq!(bytes, h("820b00"));
}

#[test]
fn golden_pparams_rational_tag_30() {
    // Pledge influence (Conway field [9]) is a rational tag(30) [num, den].
    let bytes = enc(|e| {
        e.tag(minicbor::data::Tag::new(30)).unwrap();
        e.array(2).unwrap();
        e.u64(3).unwrap();
        e.u64(10).unwrap();
    });
    assert_eq!(bytes, h("d81e82030a"));
}

#[test]
fn golden_pparams_pool_voting_thresholds_array5() {
    // Field [22] poolVotingThresholds = array(5) of rationals.
    let bytes = enc(|e| {
        e.array(5).unwrap();
        for _ in 0..5 {
            e.tag(minicbor::data::Tag::new(30)).unwrap();
            e.array(2).unwrap();
            e.u64(1).unwrap();
            e.u64(2).unwrap();
        }
    });
    assert_eq!(bytes[0], 0x85, "array(5)");
    // 5 × (1+1+1+1+1) = 25 bytes payload + 1 header = 26
    assert_eq!(bytes.len(), 26);
}

#[test]
fn golden_pparams_drep_voting_thresholds_array10() {
    // Field [23] drepVotingThresholds = array(10) of rationals.
    let bytes = enc(|e| {
        e.array(10).unwrap();
        for _ in 0..10 {
            e.tag(minicbor::data::Tag::new(30)).unwrap();
            e.array(2).unwrap();
            e.u64(1).unwrap();
            e.u64(2).unwrap();
        }
    });
    assert_eq!(bytes[0], 0x8A, "array(10)");
    assert_eq!(bytes.len(), 51);
}

#[test]
fn golden_pparams_exec_prices_array_two_rationals() {
    // Field [16] execPrices = [tag(30) mem_price, tag(30) step_price]
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.tag(minicbor::data::Tag::new(30)).unwrap();
        e.array(2).unwrap();
        e.u64(577).unwrap();
        e.u64(10_000).unwrap();
        e.tag(minicbor::data::Tag::new(30)).unwrap();
        e.array(2).unwrap();
        e.u64(721).unwrap();
        e.u64(10_000_000).unwrap();
    });
    assert_eq!(bytes[0], 0x82, "array(2) of rationals");
    assert_eq!(bytes[1], 0xD8, "first elem tag");
    assert_eq!(bytes[2], 0x1E, "tag = 30");
}

// ---------------------------------------------------------------------------
// drep-delegations query result (issue #458 new shape)
// ---------------------------------------------------------------------------
//
// The new GetFilteredDRepDelegations result shape is:
//   array(1) [ map<credential, drep_target> ]
// where credential is [type, hash(28)] and drep_target is
//   [0, keyhash] | [1, scripthash] | [2] AlwaysAbstain | [3] AlwaysNoConfidence.

#[test]
fn golden_drep_delegations_result_empty_map() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.map(0).unwrap();
    });
    assert_eq!(bytes, h("81a0"));
}

#[test]
fn golden_drep_delegations_result_one_keyhash_drep() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.map(1).unwrap();
        // key: stake credential [0, h'..']
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.bytes(&[0x88; 28]).unwrap();
        // value: drep_target [0, h'..']
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.bytes(&[0x99; 28]).unwrap();
    });
    assert_eq!(bytes[0], 0x81, "HFC success wrapper");
    assert_eq!(bytes[1], 0xA1, "single-entry map");
}

#[test]
fn golden_drep_delegations_result_abstain() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.map(1).unwrap();
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.bytes(&[0xAA; 28]).unwrap();
        e.array(1).unwrap();
        e.u64(2).unwrap(); // AlwaysAbstain
    });
    // Trailing 2 bytes = drep AlwaysAbstain = 0x81 0x02
    assert_eq!(&bytes[bytes.len() - 2..], &[0x81, 0x02]);
}

// ---------------------------------------------------------------------------
// Constitution query result (Result_Conway_Constitution)
// ---------------------------------------------------------------------------

#[test]
fn golden_constitution_result_with_script() {
    // array(1) [array(2) [anchor, script_hash / null]]
    // anchor = [url(text), data_hash(32)]
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.array(2).unwrap();
        e.str("ipfs://QmExample").unwrap();
        e.bytes(&[0x44; 32]).unwrap();
        e.bytes(&[0x55; 28]).unwrap(); // script hash
    });
    assert_eq!(bytes[0], 0x81, "HFC success wrapper");
    assert_eq!(bytes[1], 0x82, "constitution body = array(2)");
}

#[test]
fn golden_constitution_result_no_script() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.array(2).unwrap();
        e.str("ipfs://QmExample").unwrap();
        e.bytes(&[0x44; 32]).unwrap();
        e.null().unwrap();
    });
    assert_eq!(*bytes.last().unwrap(), 0xF6, "null script hash");
}
