//! ChainSync mini-protocol codec and message types.
//!
//! ChainSync is the core synchronization protocol — the client follows the server's
//! chain by receiving headers (N2N) or full blocks (N2C).
//!
//! ## State machine
//! ```text
//! StIdle ──MsgRequestNext──► StCanAwait
//! StCanAwait ──MsgRollForward(header, tip)──► StIdle
//! StCanAwait ──MsgRollBackward(point, tip)──► StIdle
//! StCanAwait ──MsgAwaitReply──► StMustReply
//! StMustReply ──MsgRollForward(header, tip)──► StIdle
//! StMustReply ──MsgRollBackward(point, tip)──► StIdle
//! StIdle ──MsgFindIntersect([points])──► StIntersect
//! StIntersect ──MsgIntersectFound(point, tip)──► StIdle
//! StIntersect ──MsgIntersectNotFound(tip)──► StIdle
//! StIdle ──MsgDone──► StDone
//! ```
//!
//! ## Wire format (CBOR)
//! - `MsgRequestNext` = `[0]`
//! - `MsgAwaitReply` = `[1]`
//! - `MsgRollForward` = `[2, header, tip]`
//! - `MsgRollBackward` = `[3, point, tip]`
//! - `MsgFindIntersect` = `[4, [*point]]`
//! - `MsgIntersectFound` = `[5, point, tip]`
//! - `MsgIntersectNotFound` = `[6, tip]`
//! - `MsgDone` = `[7]`
//!
//! ## N2N ChainSync header field format
//! For N2N ChainSync, `MsgRollForward` sends a block *header*, not the full block.
//! The header field is HFC-wrapped: `[era_id, #6.24(bstr(header_cbor))]`
//! where `era_id` identifies the era (1=Byron, 2=Shelley, …, 6=Conway) and
//! `header_cbor` is the serialised block header extracted from position 0 of the
//! block body array.
//!
//! The `hfc_header` field in [`ChainSyncMessage::MsgRollForward`] stores the
//! pre-encoded CBOR bytes of this two-element array.  The encoder inlines them
//! verbatim into the message; the decoder captures the corresponding sub-value.

pub mod client;
pub mod jumping;
pub mod server;

use crate::codec::{self, Point};
use minicbor::{data::Type, Decoder, Encoder};
use std::io::Write as _;

/// Maximum number of points in a `MsgFindIntersect` request.
///
/// B3/B19: Haskell's `ouroboros-consensus` limits this to `maxNumOfPoints` (100).
/// Exceeding this would allow:
///  (a) `points.reserve(u64::MAX)` allocation bomb before the bounds check (B19).
///  (b) O(N) `has_block()` DB lookups per request (B3 I/O amplification).
pub const MAX_INTERSECT_POINTS: u64 = 100;

/// ChainSync protocol state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainSyncState {
    /// Client has agency — can send MsgRequestNext, MsgFindIntersect, or MsgDone.
    StIdle,
    /// Server has agency — can reply immediately or send MsgAwaitReply.
    StCanAwait,
    /// Server must reply (at tip, waiting for new block).
    StMustReply,
    /// Server has agency after MsgFindIntersect.
    StIntersect,
    /// Terminal state.
    StDone,
}

/// ChainSync protocol messages.
#[derive(Debug, Clone)]
pub enum ChainSyncMessage {
    /// Client requests the next header/block (tag 0).
    MsgRequestNext,
    /// Server signals it's at the tip, will reply when a new block arrives (tag 1).
    MsgAwaitReply,
    /// Server sends a new header rolling the chain forward (tag 2).
    ///
    /// For N2N ChainSync the `header` field holds the pre-encoded CBOR bytes of
    /// the HFC-wrapped header: `[era_id, #6.24(bstr(header_cbor))]`.
    /// The encoder inlines these bytes verbatim as a CBOR value (not as a bstr).
    ///
    /// The decoder captures the raw bytes of the header sub-value so that the
    /// field always contains ready-to-inline CBOR.
    MsgRollForward {
        /// Pre-encoded CBOR bytes of `[era_id, #6.24(bstr(inner_header))]`.
        header: Vec<u8>,
        tip_slot: u64,
        tip_hash: [u8; 32],
        tip_block_number: u64,
    },
    /// Server rolls the chain backward to a previous point (tag 3).
    MsgRollBackward {
        point: Point,
        tip_slot: u64,
        tip_hash: [u8; 32],
        tip_block_number: u64,
    },
    /// Client requests intersection with a list of known points (tag 4).
    MsgFindIntersect(Vec<Point>),
    /// Server found an intersection point (tag 5).
    MsgIntersectFound {
        point: Point,
        tip_slot: u64,
        tip_hash: [u8; 32],
        tip_block_number: u64,
    },
    /// Server could not find any intersection (tag 6).
    MsgIntersectNotFound {
        tip_slot: u64,
        tip_hash: [u8; 32],
        tip_block_number: u64,
    },
    /// Client terminates the protocol (tag 7).
    MsgDone,
}

// CBOR message tags
const TAG_REQUEST_NEXT: u64 = 0;
const TAG_AWAIT_REPLY: u64 = 1;
const TAG_ROLL_FORWARD: u64 = 2;
const TAG_ROLL_BACKWARD: u64 = 3;
const TAG_FIND_INTERSECT: u64 = 4;
const TAG_INTERSECT_FOUND: u64 = 5;
const TAG_INTERSECT_NOT_FOUND: u64 = 6;
const TAG_DONE: u64 = 7;

/// Encode a ChainSync message as CBOR.
pub fn encode_message(msg: &ChainSyncMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    match msg {
        ChainSyncMessage::MsgRequestNext => {
            enc.array(1).expect("infallible");
            enc.u64(TAG_REQUEST_NEXT).expect("infallible");
        }
        ChainSyncMessage::MsgAwaitReply => {
            enc.array(1).expect("infallible");
            enc.u64(TAG_AWAIT_REPLY).expect("infallible");
        }
        ChainSyncMessage::MsgRollForward {
            header,
            tip_slot,
            tip_hash,
            tip_block_number,
        } => {
            enc.array(3).expect("infallible");
            enc.u64(TAG_ROLL_FORWARD).expect("infallible");
            // The header field holds pre-encoded CBOR bytes for the HFC-wrapped
            // header array [era_id, #6.24(bstr(inner_header))].  We inline them
            // verbatim as a CBOR value (not wrapped in a bstr) so that the wire
            // format matches what Haskell cardano-node sends.
            enc.writer_mut().write_all(header).expect("infallible");
            codec::encode_tip(&mut enc, *tip_slot, tip_hash, *tip_block_number);
        }
        ChainSyncMessage::MsgRollBackward {
            point,
            tip_slot,
            tip_hash,
            tip_block_number,
        } => {
            enc.array(3).expect("infallible");
            enc.u64(TAG_ROLL_BACKWARD).expect("infallible");
            codec::encode_point(&mut enc, point);
            codec::encode_tip(&mut enc, *tip_slot, tip_hash, *tip_block_number);
        }
        ChainSyncMessage::MsgFindIntersect(points) => {
            enc.array(2).expect("infallible");
            enc.u64(TAG_FIND_INTERSECT).expect("infallible");
            enc.array(points.len() as u64).expect("infallible");
            for p in points {
                codec::encode_point(&mut enc, p);
            }
        }
        ChainSyncMessage::MsgIntersectFound {
            point,
            tip_slot,
            tip_hash,
            tip_block_number,
        } => {
            enc.array(3).expect("infallible");
            enc.u64(TAG_INTERSECT_FOUND).expect("infallible");
            codec::encode_point(&mut enc, point);
            codec::encode_tip(&mut enc, *tip_slot, tip_hash, *tip_block_number);
        }
        ChainSyncMessage::MsgIntersectNotFound {
            tip_slot,
            tip_hash,
            tip_block_number,
        } => {
            enc.array(2).expect("infallible");
            enc.u64(TAG_INTERSECT_NOT_FOUND).expect("infallible");
            codec::encode_tip(&mut enc, *tip_slot, tip_hash, *tip_block_number);
        }
        ChainSyncMessage::MsgDone => {
            enc.array(1).expect("infallible");
            enc.u64(TAG_DONE).expect("infallible");
        }
    }
    buf
}

/// Decode a ChainSync message from CBOR bytes.
pub fn decode_message(data: &[u8]) -> Result<ChainSyncMessage, String> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec.array().map_err(|e| e.to_string())?;
    let tag = dec.u64().map_err(|e| e.to_string())?;

    match tag {
        TAG_REQUEST_NEXT => Ok(ChainSyncMessage::MsgRequestNext),
        TAG_AWAIT_REPLY => Ok(ChainSyncMessage::MsgAwaitReply),
        TAG_ROLL_FORWARD => {
            // The header field is an HFC-wrapped header array: [era_id, #6.24(bstr(hdr))]
            // Haskell cardano-node sends: [2, [era_id, tag24(header_cbor)], tip]
            //
            // We capture the raw CBOR bytes of the entire header sub-value so they can
            // be stored and inlined verbatim when re-encoding (no double-wrapping).
            // This is required because the encoder writes `header` bytes inline as a raw
            // CBOR value rather than as a bstr.
            let header_start = dec.position();
            match dec.datatype().map_err(|e| e.to_string())? {
                minicbor::data::Type::Array | minicbor::data::Type::ArrayIndef => {
                    // HFC-wrapped: [era_id, tag24(header_cbor)] — skip the whole value.
                    dec.skip().map_err(|e| e.to_string())?;
                }
                minicbor::data::Type::Bytes | minicbor::data::Type::BytesIndef => {
                    // Legacy / Dugite-to-Dugite: raw bytes fallback.
                    dec.skip().map_err(|e| e.to_string())?;
                }
                minicbor::data::Type::Tag => {
                    // N2C Serialised encoding: tag(24)(bytes(block_cbor)).
                    dec.skip().map_err(|e| e.to_string())?;
                }
                other => {
                    return Err(format!(
                        "ChainSync RollForward: unexpected header type {other:?}"
                    ));
                }
            }
            let header_end = dec.position();
            // Capture the raw CBOR bytes of the header sub-value for verbatim re-encoding.
            let header = data[header_start..header_end].to_vec();
            let (tip_slot, tip_hash, tip_block_number) =
                codec::decode_tip(&mut dec).map_err(|e| e.to_string())?;
            Ok(ChainSyncMessage::MsgRollForward {
                header,
                tip_slot,
                tip_hash,
                tip_block_number,
            })
        }
        TAG_ROLL_BACKWARD => {
            let point = codec::decode_point(&mut dec).map_err(|e| e.to_string())?;
            let (tip_slot, tip_hash, tip_block_number) =
                codec::decode_tip(&mut dec).map_err(|e| e.to_string())?;
            Ok(ChainSyncMessage::MsgRollBackward {
                point,
                tip_slot,
                tip_hash,
                tip_block_number,
            })
        }
        TAG_FIND_INTERSECT => {
            // Accept both definite and indefinite-length arrays.
            // The CDDL spec uses `[* point]` which permits either encoding.
            //
            // B3+B19: Cap the number of intersection points to prevent:
            //   (a) Allocation bombs: `points.reserve(u64::MAX)` before a bounds
            //       check causes the allocator to attempt a huge allocation.
            //   (b) I/O amplification: each Point::Specific triggers a has_block()
            //       DB lookup in the server; 100,000 lookups = ~1s of I/O.
            // Haskell's `ouroboros-consensus` limits this to `maxNumOfPoints` (100).
            let mut points = Vec::new();
            match dec.datatype().map_err(|e| e.to_string())? {
                Type::ArrayIndef => {
                    dec.array().map_err(|e| e.to_string())?;
                    let mut count: u64 = 0;
                    loop {
                        if dec.datatype().map_err(|e| e.to_string())? == Type::Break {
                            dec.skip().map_err(|e| e.to_string())?;
                            break;
                        }
                        if count >= MAX_INTERSECT_POINTS {
                            return Err(format!(
                                "MsgFindIntersect: indefinite array exceeded max \
                                 {MAX_INTERSECT_POINTS} points"
                            ));
                        }
                        count += 1;
                        points.push(codec::decode_point(&mut dec).map_err(|e| e.to_string())?);
                    }
                }
                Type::Array => {
                    let arr_len = dec
                        .array()
                        .map_err(|e| e.to_string())?
                        .ok_or("expected definite array length")?;
                    // B19: Check BEFORE reserve — a u64 arr_len of u64::MAX would
                    // cause Vec::reserve to attempt a huge allocation before failing.
                    if arr_len > MAX_INTERSECT_POINTS {
                        return Err(format!(
                            "MsgFindIntersect: array length {arr_len} exceeds max \
                             {MAX_INTERSECT_POINTS} points"
                        ));
                    }
                    points.reserve(arr_len as usize);
                    for _ in 0..arr_len {
                        points.push(codec::decode_point(&mut dec).map_err(|e| e.to_string())?);
                    }
                }
                other => {
                    return Err(format!(
                        "MsgFindIntersect: expected array of points, got {other:?}"
                    ));
                }
            }
            Ok(ChainSyncMessage::MsgFindIntersect(points))
        }
        TAG_INTERSECT_FOUND => {
            let point = codec::decode_point(&mut dec).map_err(|e| e.to_string())?;
            let (tip_slot, tip_hash, tip_block_number) =
                codec::decode_tip(&mut dec).map_err(|e| e.to_string())?;
            Ok(ChainSyncMessage::MsgIntersectFound {
                point,
                tip_slot,
                tip_hash,
                tip_block_number,
            })
        }
        TAG_INTERSECT_NOT_FOUND => {
            let (tip_slot, tip_hash, tip_block_number) =
                codec::decode_tip(&mut dec).map_err(|e| e.to_string())?;
            Ok(ChainSyncMessage::MsgIntersectNotFound {
                tip_slot,
                tip_hash,
                tip_block_number,
            })
        }
        TAG_DONE => Ok(ChainSyncMessage::MsgDone),
        _ => Err(format!("unknown ChainSync message tag: {tag}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_request_next_roundtrip() {
        let msg = ChainSyncMessage::MsgRequestNext;
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, ChainSyncMessage::MsgRequestNext));
    }

    #[test]
    fn msg_request_next_wire_format() {
        // [0] = 0x81 0x00
        let encoded = encode_message(&ChainSyncMessage::MsgRequestNext);
        assert_eq!(encoded, vec![0x81, 0x00]);
    }

    #[test]
    fn msg_await_reply_roundtrip() {
        let encoded = encode_message(&ChainSyncMessage::MsgAwaitReply);
        let decoded = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, ChainSyncMessage::MsgAwaitReply));
    }

    /// Build the pre-encoded CBOR bytes for an HFC header:
    /// `[era_id, #6.24(bstr(inner_header))]`.
    fn make_hfc_header(era_id: u64, inner_header: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(era_id).unwrap();
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(inner_header).unwrap();
        buf
    }

    #[test]
    fn msg_roll_forward_roundtrip() {
        // The `header` field must be pre-encoded CBOR — the HFC-wrapped header
        // array [era_id, #6.24(bstr(inner_header))].  The encoder inlines it
        // verbatim; the decoder captures the sub-value bytes.
        let hfc_header = make_hfc_header(6, &[0xDE, 0xAD]);
        let msg = ChainSyncMessage::MsgRollForward {
            header: hfc_header.clone(),
            tip_slot: 100,
            tip_hash: [0xAB; 32],
            tip_block_number: 50,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let ChainSyncMessage::MsgRollForward {
            header,
            tip_slot,
            tip_hash,
            tip_block_number,
        } = decoded
        {
            // The decoded header bytes should be the same pre-encoded CBOR that was
            // provided — the sub-value was captured verbatim by the decoder.
            assert_eq!(header, hfc_header);
            assert_eq!(tip_slot, 100);
            assert_eq!(tip_hash, [0xAB; 32]);
            assert_eq!(tip_block_number, 50);
        } else {
            panic!("expected MsgRollForward");
        }
    }

    #[test]
    fn msg_roll_forward_hfc_wire_format() {
        // Verify the exact wire bytes produced for a Conway-era (era_id=6) header.
        //
        // Expected outer structure: [2, [6, #6.24(bstr(inner))], tip]
        // where tip = [[slot, hash], block_number]
        let inner = vec![0xAA, 0xBB];
        let hfc_header = make_hfc_header(6, &inner);
        let msg = ChainSyncMessage::MsgRollForward {
            header: hfc_header,
            tip_slot: 0,
            tip_hash: [0; 32],
            tip_block_number: 0,
        };
        let encoded = encode_message(&msg);
        // The third byte after the outer array+tag should be the start of the
        // HFC-wrapped header array (0x82 = definite array of 2).
        // [0x83, 0x02, 0x82, 0x06, 0xd8, 0x18, 0x42, 0xAA, 0xBB, <tip>]
        assert_eq!(encoded[0], 0x83, "outer array(3)");
        assert_eq!(encoded[1], 0x02, "TAG_ROLL_FORWARD");
        assert_eq!(encoded[2], 0x82, "HFC header array(2)");
        assert_eq!(encoded[3], 0x06, "era_id=6 (Conway)");
        assert_eq!(encoded[4], 0xd8, "CBOR tag major type + 1-byte follows");
        assert_eq!(encoded[5], 0x18, "tag value 24");
        assert_eq!(encoded[6], 0x42, "bstr of length 2");
        assert_eq!(encoded[7], 0xAA, "inner header byte 0");
        assert_eq!(encoded[8], 0xBB, "inner header byte 1");
    }

    #[test]
    fn msg_roll_backward_with_origin() {
        let msg = ChainSyncMessage::MsgRollBackward {
            point: Point::Origin,
            tip_slot: 0,
            tip_hash: [0; 32],
            tip_block_number: 0,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let ChainSyncMessage::MsgRollBackward { point, .. } = decoded {
            assert_eq!(point, Point::Origin);
        } else {
            panic!("expected MsgRollBackward");
        }
    }

    #[test]
    fn msg_find_intersect_roundtrip() {
        let points = vec![
            Point::Specific(1000, [0x11; 32]),
            Point::Specific(500, [0x22; 32]),
            Point::Origin,
        ];
        let msg = ChainSyncMessage::MsgFindIntersect(points.clone());
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let ChainSyncMessage::MsgFindIntersect(decoded_points) = decoded {
            assert_eq!(decoded_points, points);
        } else {
            panic!("expected MsgFindIntersect");
        }
    }

    #[test]
    fn msg_intersect_found_roundtrip() {
        let msg = ChainSyncMessage::MsgIntersectFound {
            point: Point::Specific(42, [0xCC; 32]),
            tip_slot: 100,
            tip_hash: [0xDD; 32],
            tip_block_number: 50,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let ChainSyncMessage::MsgIntersectFound {
            point, tip_slot, ..
        } = decoded
        {
            assert_eq!(point, Point::Specific(42, [0xCC; 32]));
            assert_eq!(tip_slot, 100);
        } else {
            panic!("expected MsgIntersectFound");
        }
    }

    #[test]
    fn msg_intersect_not_found_roundtrip() {
        let msg = ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: 200,
            tip_hash: [0xEE; 32],
            tip_block_number: 100,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let ChainSyncMessage::MsgIntersectNotFound {
            tip_slot,
            tip_block_number,
            ..
        } = decoded
        {
            assert_eq!(tip_slot, 200);
            assert_eq!(tip_block_number, 100);
        } else {
            panic!("expected MsgIntersectNotFound");
        }
    }

    #[test]
    fn msg_done_roundtrip() {
        let encoded = encode_message(&ChainSyncMessage::MsgDone);
        let decoded = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, ChainSyncMessage::MsgDone));
    }

    // ─── B3/B19: MsgFindIntersect cap tests ───────────────────────────────────

    /// B19: Definite-array MsgFindIntersect exceeding MAX_INTERSECT_POINTS must be rejected
    /// BEFORE any allocation (checks cap before reserve).
    #[test]
    fn find_intersect_definite_over_cap_rejected() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(4).unwrap(); // TAG_FIND_INTERSECT
                                 // Claim arr_len = MAX_INTERSECT_POINTS + 1 but only write 0 items
                                 // (the decoder should reject before iterating).
            enc.array(MAX_INTERSECT_POINTS + 1).unwrap();
            // Write one dummy point so the decode doesn't fail on an empty stream first.
            // Origin point = [0]
            enc.array(1).unwrap();
            enc.u64(0).unwrap();
        }
        let result = decode_message(&buf);
        assert!(
            result.is_err(),
            "should reject definite array > MAX_INTERSECT_POINTS"
        );
        assert!(
            result.unwrap_err().contains("exceeds max"),
            "error should mention max"
        );
    }

    /// B3: Indefinite-array MsgFindIntersect exceeding MAX_INTERSECT_POINTS must be rejected.
    #[test]
    fn find_intersect_indefinite_over_cap_rejected() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(4).unwrap(); // TAG_FIND_INTERSECT
            enc.begin_array().unwrap(); // 0x9F
                                        // Write MAX_INTERSECT_POINTS + 1 Origin points.
            for _ in 0..=MAX_INTERSECT_POINTS {
                enc.array(1).unwrap();
                enc.u64(0).unwrap(); // Origin
            }
            enc.end().unwrap(); // 0xFF
        }
        let result = decode_message(&buf);
        assert!(
            result.is_err(),
            "should reject indefinite array > MAX_INTERSECT_POINTS"
        );
    }

    /// B3: Definite-array MsgFindIntersect at exactly MAX_INTERSECT_POINTS must succeed.
    #[test]
    fn find_intersect_at_cap_accepted() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(4).unwrap(); // TAG_FIND_INTERSECT
            enc.array(MAX_INTERSECT_POINTS).unwrap();
            for i in 0..MAX_INTERSECT_POINTS {
                // Specific point
                enc.array(2).unwrap();
                enc.u64(i * 100 + 1).unwrap(); // slot
                enc.bytes(&[i as u8; 32]).unwrap(); // hash
            }
        }
        let result = decode_message(&buf);
        assert!(
            result.is_ok(),
            "exactly MAX_INTERSECT_POINTS must be accepted: {result:?}"
        );
    }
}
