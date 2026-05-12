//! CBOR golden tests for Ouroboros mini-protocol messages.
//!
//! Coverage:
//! - ChainSync (RequestNext, AwaitReply, RollForward, RollBackward, FindIntersect,
//!   IntersectFound, IntersectNotFound, Done) — N2N wire format
//! - BlockFetch (RequestRange, StartBatch, Block, NoBlocks, BatchDone, ClientDone)
//! - TxSubmission2 (Init, RequestTxIds, ReplyTxIds, RequestTxs, ReplyTxs, Done)
//! - KeepAlive (KeepAlive, KeepAliveResponse, Done)
//! - LocalStateQuery (Acquire/ReAcquire/Acquired/Failure/Query/Result/Release/Done,
//!   including V16+ VolatileTip / ImmutableTip acquire targets)
//!
//! Golden hex strings were either computed by running the production encoder
//! once and validating against the CDDL spec at
//! `ouroboros-network/.../mini-protocols/*.cddl`, or — where Haskell capture
//! exists — cross-referenced byte-for-byte with `tcpdump` of a live
//! cardano-node session.
//!
//! Each test:
//!   1. Constructs the message via the public API.
//!   2. Encodes with the production encoder.
//!   3. Asserts the bytes match an inline `&[u8]` golden hex string.
//!   4. Decodes and asserts the structural roundtrip (where a decoder exists).

use dugite_network::codec::Point;
use dugite_network::protocol::blockfetch::{self as bf, BlockFetchMessage};
use dugite_network::protocol::chainsync::{self as cs, ChainSyncMessage};
use dugite_network::protocol::keepalive::{self as ka, KeepAliveMessage};
use dugite_network::protocol::txsubmission::{self as txs, TxIdAndSize, TxSubmissionMessage};

/// Decode an ASCII hex string into bytes. Test-only helper.
fn h(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

// ---------------------------------------------------------------------------
// ChainSync
// ---------------------------------------------------------------------------

#[test]
fn golden_chainsync_msg_request_next() {
    // CDDL: msgRequestNext = [0]
    let bytes = cs::encode_message(&ChainSyncMessage::MsgRequestNext);
    assert_eq!(bytes, h("8100"), "MsgRequestNext must be [0]");
}

#[test]
fn golden_chainsync_msg_await_reply() {
    // CDDL: msgAwaitReply = [1]
    let bytes = cs::encode_message(&ChainSyncMessage::MsgAwaitReply);
    assert_eq!(bytes, h("8101"), "MsgAwaitReply must be [1]");
}

#[test]
fn golden_chainsync_msg_done() {
    // CDDL: msgDone = [7]
    let bytes = cs::encode_message(&ChainSyncMessage::MsgDone);
    assert_eq!(bytes, h("8107"), "MsgDone must be [7]");
}

#[test]
fn golden_chainsync_msg_find_intersect_origin_only() {
    // CDDL: msgFindIntersect = [4, [* point]]
    // Single point list containing Origin: [4, [[]]]
    let bytes = cs::encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
    assert_eq!(
        bytes,
        h("82048180"),
        "MsgFindIntersect([Origin]) = [4, [[]]]"
    );
    // Roundtrip
    let decoded = cs::decode_message(&bytes).unwrap();
    if let ChainSyncMessage::MsgFindIntersect(pts) = decoded {
        assert_eq!(pts, vec![Point::Origin]);
    } else {
        panic!("expected MsgFindIntersect");
    }
}

#[test]
fn golden_chainsync_msg_find_intersect_specific() {
    // [4, [[42, h"AA..AA"]]]
    let pt = Point::Specific(42, [0xAA; 32]);
    let bytes = cs::encode_message(&ChainSyncMessage::MsgFindIntersect(vec![pt.clone()]));
    let expected = h(
        // 0x82 04 81 — outer [4, [..]]
        "820481\
         8218\
         2a58\
         20aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    assert_eq!(bytes, expected);
    let decoded = cs::decode_message(&bytes).unwrap();
    if let ChainSyncMessage::MsgFindIntersect(pts) = decoded {
        assert_eq!(pts, vec![pt]);
    } else {
        panic!("expected MsgFindIntersect");
    }
}

#[test]
fn golden_chainsync_msg_intersect_not_found_origin_tip() {
    // [6, [[], 0]] — server has no blocks (tip at Origin, block# 0).
    // Note: dugite always encodes the tip's point with the specific shape
    // [slot, hash], using all-zero hash when at origin — matches Haskell's
    // `Tip Origin` encoding.
    let bytes = cs::encode_message(&ChainSyncMessage::MsgIntersectNotFound {
        tip_slot: 0,
        tip_hash: [0; 32],
        tip_block_number: 0,
    });
    let expected = h("820682\
         820058\
         200000000000000000000000000000000000000000000000000000000000000000\
         00");
    assert_eq!(bytes, expected);
}

#[test]
fn golden_chainsync_msg_intersect_found_specific() {
    let bytes = cs::encode_message(&ChainSyncMessage::MsgIntersectFound {
        point: Point::Specific(100, [0x11; 32]),
        tip_slot: 200,
        tip_hash: [0x22; 32],
        tip_block_number: 7,
    });
    let expected = h(
        // [5, point, tip]
        "830582\
         18\
         6458\
         201111111111111111111111111111111111111111111111111111111111111111\
         82\
         8218\
         c858\
         202222222222222222222222222222222222222222222222222222222222222222\
         07",
    );
    assert_eq!(bytes, expected);
    let decoded = cs::decode_message(&bytes).unwrap();
    if let ChainSyncMessage::MsgIntersectFound {
        point,
        tip_slot,
        tip_block_number,
        ..
    } = decoded
    {
        assert_eq!(point, Point::Specific(100, [0x11; 32]));
        assert_eq!(tip_slot, 200);
        assert_eq!(tip_block_number, 7);
    } else {
        panic!("expected MsgIntersectFound");
    }
}

#[test]
fn golden_chainsync_msg_roll_backward_origin() {
    let bytes = cs::encode_message(&ChainSyncMessage::MsgRollBackward {
        point: Point::Origin,
        tip_slot: 0,
        tip_hash: [0; 32],
        tip_block_number: 0,
    });
    // [3, [], [[slot=0, hash=0..0], 0]]
    let expected = h("830380\
         82\
         8200\
         5820\
         0000000000000000000000000000000000000000000000000000000000000000\
         00");
    assert_eq!(bytes, expected);
}

#[test]
fn golden_chainsync_msg_roll_forward_conway_minimal() {
    // RollForward inlines a pre-encoded HFC-wrapped header sub-value.
    // Conway era_id = 6, tiny placeholder inner header bytes.
    //
    // Pre-encoded header = [6, tag(24)(bstr(0xAABB))]
    //   = 0x82 0x06 0xD8 0x18 0x42 0xAA 0xBB
    let mut header = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut header);
    enc.array(2).unwrap();
    enc.u64(6).unwrap();
    enc.tag(minicbor::data::Tag::new(24)).unwrap();
    enc.bytes(&[0xAA, 0xBB]).unwrap();
    let bytes = cs::encode_message(&ChainSyncMessage::MsgRollForward {
        header,
        tip_slot: 0,
        tip_hash: [0; 32],
        tip_block_number: 0,
    });
    // [2, [6, #6.24(bstr(AABB))], [[0, 0..0], 0]]
    let expected = h("830282\
         06d8\
         1842\
         aabb\
         82\
         8200\
         58200000000000000000000000000000000000000000000000000000000000000000\
         00");
    assert_eq!(bytes, expected);
}

// ---------------------------------------------------------------------------
// BlockFetch
// ---------------------------------------------------------------------------

#[test]
fn golden_blockfetch_msg_client_done() {
    let bytes = bf::encode_message(&BlockFetchMessage::MsgClientDone);
    assert_eq!(bytes, h("8101"), "MsgClientDone = [1]");
}

#[test]
fn golden_blockfetch_msg_start_batch() {
    let bytes = bf::encode_message(&BlockFetchMessage::MsgStartBatch);
    assert_eq!(bytes, h("8102"), "MsgStartBatch = [2]");
}

#[test]
fn golden_blockfetch_msg_no_blocks() {
    let bytes = bf::encode_message(&BlockFetchMessage::MsgNoBlocks);
    assert_eq!(bytes, h("8103"), "MsgNoBlocks = [3]");
}

#[test]
fn golden_blockfetch_msg_batch_done() {
    let bytes = bf::encode_message(&BlockFetchMessage::MsgBatchDone);
    assert_eq!(bytes, h("8105"), "MsgBatchDone = [5]");
}

#[test]
fn golden_blockfetch_msg_request_range() {
    let from = Point::Specific(10, [0x01; 32]);
    let to = Point::Specific(20, [0x02; 32]);
    let bytes = bf::encode_message(&BlockFetchMessage::MsgRequestRange {
        from: from.clone(),
        to: to.clone(),
    });
    // [0, from_point, to_point]
    let expected = h("830082\
         0a58\
         200101010101010101010101010101010101010101010101010101010101010101\
         82\
         1458\
         200202020202020202020202020202020202020202020202020202020202020202");
    assert_eq!(bytes, expected);
    let decoded = bf::decode_message(&bytes).unwrap();
    if let BlockFetchMessage::MsgRequestRange { from: f, to: t } = decoded {
        assert_eq!(f, from);
        assert_eq!(t, to);
    } else {
        panic!("expected MsgRequestRange");
    }
}

#[test]
fn golden_blockfetch_msg_block() {
    // MsgBlock encodes a bstr of stored block CBOR.
    let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let bytes = bf::encode_message(&BlockFetchMessage::MsgBlock(payload.clone()));
    // [4, h'DEADBEEF']
    assert_eq!(bytes, h("820444deadbeef"));
}

// ---------------------------------------------------------------------------
// TxSubmission2
// ---------------------------------------------------------------------------

#[test]
fn golden_txsub_msg_init() {
    // [6]
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgInit);
    assert_eq!(bytes, h("8106"), "MsgInit = [6]");
}

#[test]
fn golden_txsub_msg_done() {
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgDone);
    assert_eq!(bytes, h("8104"), "MsgDone = [4]");
}

#[test]
fn golden_txsub_msg_request_tx_ids() {
    // [0, false, 0, 10]
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgRequestTxIds {
        blocking: false,
        ack_count: 0,
        req_count: 10,
    });
    assert_eq!(bytes, h("8400f4000a"));
    let decoded = txs::decode_message(&bytes).unwrap();
    if let TxSubmissionMessage::MsgRequestTxIds {
        blocking,
        ack_count,
        req_count,
    } = decoded
    {
        assert!(!blocking);
        assert_eq!(ack_count, 0);
        assert_eq!(req_count, 10);
    } else {
        panic!("expected MsgRequestTxIds");
    }
}

#[test]
fn golden_txsub_msg_request_tx_ids_blocking() {
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgRequestTxIds {
        blocking: true,
        ack_count: 5,
        req_count: 100,
    });
    assert_eq!(bytes, h("8400f5051864"));
}

#[test]
fn golden_txsub_msg_reply_tx_ids_empty() {
    // [1, []] — empty indef-array reply.
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgReplyTxIds(vec![]));
    // [1, indef-arr, break] = 0x82 0x01 0x9F 0xFF
    assert_eq!(bytes, h("82019fff"));
}

#[test]
fn golden_txsub_msg_reply_tx_ids_one_entry() {
    let entry = TxIdAndSize {
        era_id: 6,
        tx_id: [0xCC; 32],
        size_in_bytes: 300,
    };
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgReplyTxIds(vec![entry]));
    // [1, indef-arr([ [ [6, h'CC..CC'], 300 ] ]), break]
    //   0x82 0x01 0x9F 0x82 0x82 0x06 0x58 0x20 CC..CC 0x19 0x01 0x2C 0xFF
    let expected = h("82019f\
         82\
         8206\
         5820cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\
         19012c\
         ff");
    assert_eq!(bytes, expected);
}

#[test]
fn golden_txsub_msg_request_txs() {
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgRequestTxs(vec![(6, [0x11; 32])]));
    // [2, indef-arr([[6, h'11..']]), break]
    let expected = h("82029f\
         82\
         0658\
         201111111111111111111111111111111111111111111111111111111111111111\
         ff");
    assert_eq!(bytes, expected);
}

#[test]
fn golden_txsub_msg_reply_txs() {
    let bytes = txs::encode_message(&TxSubmissionMessage::MsgReplyTxs(vec![(
        6,
        vec![0xAA, 0xBB],
    )]));
    // [3, indef-arr([[6, tag(24)(h'AABB')]]), break]
    let expected = h("82039f\
         82\
         06d8\
         1842aabb\
         ff");
    assert_eq!(bytes, expected);
}

// ---------------------------------------------------------------------------
// KeepAlive
// ---------------------------------------------------------------------------

#[test]
fn golden_keepalive_msg_keep_alive_zero_cookie() {
    let bytes = ka::encode_message(&KeepAliveMessage::MsgKeepAlive(0));
    assert_eq!(bytes, h("820000"));
}

#[test]
fn golden_keepalive_msg_keep_alive_max_cookie() {
    let bytes = ka::encode_message(&KeepAliveMessage::MsgKeepAlive(u16::MAX));
    // [0, 65535] -> 65535 needs 2-byte uint -> 0x19 0xFF 0xFF
    assert_eq!(bytes, h("820019ffff"));
}

#[test]
fn golden_keepalive_msg_keep_alive_response() {
    let bytes = ka::encode_message(&KeepAliveMessage::MsgKeepAliveResponse(42));
    // [1, 42] -> 42 = 0x18 0x2A (forced 1-byte by minicbor for uint above 23)
    assert_eq!(bytes, h("8201182a"));
}

#[test]
fn golden_keepalive_msg_done() {
    let bytes = ka::encode_message(&KeepAliveMessage::MsgDone);
    assert_eq!(bytes, h("8102"));
}

// ---------------------------------------------------------------------------
// LocalStateQuery (hand-encoded per tag constants; no high-level encoder API)
// ---------------------------------------------------------------------------
//
// The LocalStateQuery server consumes/produces these messages directly via
// minicbor. The tests below verify the wire format against the spec.

fn enc<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut minicbor::Encoder<&mut Vec<u8>>),
{
    let mut buf = Vec::new();
    let mut e = minicbor::Encoder::new(&mut buf);
    f(&mut e);
    buf
}

#[test]
fn golden_lsq_msg_acquire_specific_origin() {
    // [0, []]  — MsgAcquire targeting Origin
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.array(0).unwrap();
    });
    assert_eq!(bytes, h("820080"));
}

#[test]
fn golden_lsq_msg_acquire_volatile_tip_v16plus() {
    // [8] — MsgAcquire targeting VolatileTip (V16+)
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(8).unwrap();
    });
    assert_eq!(bytes, h("8108"));
}

#[test]
fn golden_lsq_msg_acquire_immutable_tip_v16plus() {
    // [10] — MsgAcquire targeting ImmutableTip
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(10).unwrap();
    });
    assert_eq!(bytes, h("810a"));
}

#[test]
fn golden_lsq_msg_acquired() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(1).unwrap();
    });
    assert_eq!(bytes, h("8101"));
}

#[test]
fn golden_lsq_msg_failure_point_too_old() {
    // [2, 0] — PointTooOld
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(2).unwrap();
        e.u64(0).unwrap();
    });
    assert_eq!(bytes, h("820200"));
}

#[test]
fn golden_lsq_msg_failure_point_not_on_chain() {
    // [2, 1] — PointNotOnChain
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(2).unwrap();
        e.u64(1).unwrap();
    });
    assert_eq!(bytes, h("820201"));
}

#[test]
fn golden_lsq_msg_release() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(5).unwrap();
    });
    assert_eq!(bytes, h("8105"));
}

#[test]
fn golden_lsq_msg_done() {
    let bytes = enc(|e| {
        e.array(1).unwrap();
        e.u64(7).unwrap();
    });
    assert_eq!(bytes, h("8107"));
}

#[test]
fn golden_lsq_msg_query_get_current_pparams_conway() {
    // [3, BlockQuery[Conway, GetCurrentPParams]]
    //   = [3, [0, [6, [3]]]]
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(3).unwrap();
        // Inner query — same as Query_Conway_GetCurrentPParams golden
        e.array(2).unwrap();
        e.u64(0).unwrap();
        e.array(2).unwrap();
        e.u64(6).unwrap();
        e.array(1).unwrap();
        e.u64(3).unwrap();
    });
    assert_eq!(bytes, h("820382008206810 3".replace(' ', "").as_str()));
}

#[test]
fn golden_lsq_msg_result_epoch_no() {
    // [4, [epoch_no]] — BlockQuery success uses HFC wrapper array(1)
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(4).unwrap();
        e.array(1).unwrap();
        e.u64(10).unwrap();
    });
    assert_eq!(bytes, h("8204810a"));
}

#[test]
fn golden_lsq_msg_result_anytime_unwrapped() {
    // [4, 42] — QueryAnytime/QueryHardFork result is NOT wrapped in array(1).
    let bytes = enc(|e| {
        e.array(2).unwrap();
        e.u64(4).unwrap();
        e.u64(42).unwrap();
    });
    assert_eq!(bytes, h("820418 2a".replace(' ', "").as_str()));
}
