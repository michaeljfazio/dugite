//! TxSubmission2 mini-protocol codec and message types.
//!
//! TxSubmission2 is a pull-based protocol with **inverted agency**: the server
//! (not the client) has agency in the Idle state and requests transaction IDs
//! and bodies from the client.
//!
//! ## State machine
//! ```text
//! StInit ──MsgInit──► StIdle
//! StIdle ──MsgRequestTxIds(blocking, ack, req)──► StTxIds
//! StTxIds ──MsgReplyTxIds([tx_id])──► StIdle
//! StIdle ──MsgRequestTxs([tx_id])──► StTxs
//! StTxs ──MsgReplyTxs([tx])──► StIdle
//! StTxIds(blocking) ──MsgDone──► StDone  (only when blocking and no txs)
//! ```
//!
//! ## Wire format (CBOR)
//! - `MsgRequestTxIds` = `[0, blocking, ack_count, req_count]`
//! - `MsgReplyTxIds` = `[1, [[tx_id, size_in_bytes]]]`
//! - `MsgRequestTxs` = `[2, [tx_id]]`
//! - `MsgReplyTxs` = `[3, [tx_bytes]]`
//! - `MsgDone` = `[4]`
//! - `MsgInit` = `[6]`

pub mod client;
pub mod server;

use minicbor::{data::Type, Decoder, Encoder};

/// TxSubmission2 protocol state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxSubmissionState {
    /// Initial state — client sends MsgInit.
    StInit,
    /// Server has agency — requests tx IDs or txs.
    StIdle,
    /// Client must reply with tx IDs (blocking or non-blocking).
    StTxIds { blocking: bool },
    /// Client must reply with tx bodies.
    StTxs,
    /// Terminal state.
    StDone,
}

/// A transaction ID with its size (for flow control).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxIdAndSize {
    /// HFC era index (Conway=6, Babbage=5, etc.). Used for N2N era wrapping.
    pub era_id: u8,
    /// Transaction hash (32 bytes).
    pub tx_id: [u8; 32],
    /// Transaction size in bytes (used by the server for flow control).
    /// Must include HFC envelope overhead (~4 bytes) for N2N compliance.
    pub size_in_bytes: u32,
}

/// Outcome of handing a received transaction body to the admission callback.
///
/// The N2N inbound tx path must reconcile the txid a peer *attests* (in
/// `MsgReplyTxIds`) against the canonical id `blake2b_256(raw_body_cbor)` of the
/// body it later delivers (in `MsgReplyTxs`). A mismatch is a protocol
/// integrity violation, not a mere validation failure, so it is distinguished
/// from an ordinary rejection: the server converts [`Self::Convict`] into a
/// fatal [`crate::error::ProtocolError::IntegrityViolation`] that drops the
/// connection (#864).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxAdmission {
    /// Tx passed validation and was admitted to the mempool (or already present).
    Accepted,
    /// Tx failed validation / mempool admission — NOT a protocol violation; the
    /// connection stays up.
    Rejected,
    /// The peer committed a protocol integrity violation (e.g. the delivered
    /// body's canonical id does not match the txid it attested). Convict the
    /// peer: the server returns a fatal error and the connection is dropped.
    Convict,
}

/// TxSubmission2 protocol messages.
#[derive(Debug, Clone)]
pub enum TxSubmissionMessage {
    /// Protocol initialization (client → server, tag 6).
    MsgInit,
    /// Server requests transaction IDs from the client (tag 0).
    MsgRequestTxIds {
        /// If true, the client must block until it has txs. If false, may return empty.
        blocking: bool,
        /// Number of previously received tx IDs to acknowledge (FIFO).
        ack_count: u16,
        /// Maximum number of tx IDs to return.
        req_count: u16,
    },
    /// Client replies with transaction IDs and sizes (tag 1).
    MsgReplyTxIds(Vec<TxIdAndSize>),
    /// Server requests full transaction bodies by ID (tag 2).
    /// Each entry is (era_id, tx_hash).
    MsgRequestTxs(Vec<(u8, [u8; 32])>),
    /// Client replies with full transaction CBOR bodies (tag 3).
    /// Each entry is (era_id, tx_cbor). Encoded as [era_id, tag(24)(tx_cbor)].
    MsgReplyTxs(Vec<(u8, Vec<u8>)>),
    /// Terminate the protocol (tag 4). Only valid in blocking StTxIds with no txs.
    MsgDone,
}

// CBOR message tags
const TAG_REQUEST_TX_IDS: u64 = 0;
const TAG_REPLY_TX_IDS: u64 = 1;
const TAG_REQUEST_TXS: u64 = 2;
const TAG_REPLY_TXS: u64 = 3;
const TAG_DONE: u64 = 4;
const TAG_INIT: u64 = 6;

/// Upper bound on inflight tx ids/txs per message. The protocol's req_count and
/// ack_count are u16, so a peer claiming more than `u16::MAX` entries is malformed
/// and would otherwise let an adversary trigger an unbounded `Vec::reserve`.
const MAX_INFLIGHT: u64 = u16::MAX as u64;

/// Encode a TxSubmission2 message as CBOR.
pub fn encode_message(msg: &TxSubmissionMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    match msg {
        TxSubmissionMessage::MsgInit => {
            enc.array(1).expect("infallible");
            enc.u64(TAG_INIT).expect("infallible");
        }
        TxSubmissionMessage::MsgRequestTxIds {
            blocking,
            ack_count,
            req_count,
        } => {
            enc.array(4).expect("infallible");
            enc.u64(TAG_REQUEST_TX_IDS).expect("infallible");
            enc.bool(*blocking).expect("infallible");
            enc.u16(*ack_count).expect("infallible");
            enc.u16(*req_count).expect("infallible");
        }
        TxSubmissionMessage::MsgReplyTxIds(ids) => {
            // Outer message array is definite-length (always 2 elements).
            // Inner txIdsAndSizes list uses indefinite-length per the CDDL spec:
            //   MsgReplyTxIds = [1, [* [GenTxId, word32]]]
            // GenTxId is HFC-wrapped: [era_id, txid_bytes]
            enc.array(2).expect("infallible");
            enc.u64(TAG_REPLY_TX_IDS).expect("infallible");
            enc.begin_array().expect("infallible");
            for id in ids {
                // Each [GenTxId, size] entry is a definite 2-element array.
                enc.array(2).expect("infallible");
                // GenTxId = [era_id, txid_bytes] (HFC NS envelope)
                enc.array(2).expect("infallible");
                enc.u8(id.era_id).expect("infallible");
                enc.bytes(&id.tx_id).expect("infallible");
                enc.u32(id.size_in_bytes).expect("infallible");
            }
            enc.end().expect("infallible");
        }
        TxSubmissionMessage::MsgRequestTxs(ids) => {
            // Inner txIdList uses indefinite-length per the CDDL spec:
            //   MsgRequestTxs = [2, [* GenTxId]]
            // GenTxId = [era_id, txid_bytes]
            enc.array(2).expect("infallible");
            enc.u64(TAG_REQUEST_TXS).expect("infallible");
            enc.begin_array().expect("infallible");
            for (era_id, id) in ids {
                enc.array(2).expect("infallible");
                enc.u8(*era_id).expect("infallible");
                enc.bytes(id).expect("infallible");
            }
            enc.end().expect("infallible");
        }
        TxSubmissionMessage::MsgReplyTxs(txs) => {
            // Inner txList uses indefinite-length per the CDDL spec:
            //   MsgReplyTxs = [3, [* GenTx]]
            // GenTx = [era_id, tag(24)(tx_cbor)]  (HFC NS + CBOR-in-CBOR)
            enc.array(2).expect("infallible");
            enc.u64(TAG_REPLY_TXS).expect("infallible");
            enc.begin_array().expect("infallible");
            for (era_id, tx_cbor) in txs {
                // Each GenTx is a 2-element array: [era_id, tag(24)(bytes)]
                enc.array(2).expect("infallible");
                enc.u8(*era_id).expect("infallible");
                // CBOR tag 24 = wrapCBORinCBOR per Haskell reference
                enc.tag(minicbor::data::Tag::new(24)).expect("infallible");
                enc.bytes(tx_cbor).expect("infallible");
            }
            enc.end().expect("infallible");
        }
        TxSubmissionMessage::MsgDone => {
            enc.array(1).expect("infallible");
            enc.u64(TAG_DONE).expect("infallible");
        }
    }
    buf
}

/// Decode a TxSubmission2 message from CBOR bytes.
pub fn decode_message(data: &[u8]) -> Result<TxSubmissionMessage, String> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec.array().map_err(|e| e.to_string())?;
    let tag = dec.u64().map_err(|e| e.to_string())?;

    match tag {
        TAG_INIT => Ok(TxSubmissionMessage::MsgInit),
        TAG_REQUEST_TX_IDS => {
            let blocking = dec.bool().map_err(|e| e.to_string())?;
            let ack_count = dec.u16().map_err(|e| e.to_string())?;
            let req_count = dec.u16().map_err(|e| e.to_string())?;
            Ok(TxSubmissionMessage::MsgRequestTxIds {
                blocking,
                ack_count,
                req_count,
            })
        }
        TAG_REPLY_TX_IDS => {
            // Accept both definite and indefinite-length arrays per the CDDL spec.
            // Haskell nodes send indefinite-length; definite is also valid CBOR.
            // Each entry is [GenTxId, word32] where GenTxId = [era_id, txid_bytes].
            let mut ids = Vec::new();
            match dec.datatype().map_err(|e| e.to_string())? {
                Type::ArrayIndef => {
                    // B2: Apply MAX_INFLIGHT cap inside the indefinite-array loop.
                    // Without this guard a peer can stream 0x9F…items…0xFF without
                    // bound, exhausting heap until OOM.
                    dec.array().map_err(|e| e.to_string())?;
                    let mut count: u64 = 0;
                    loop {
                        if dec.datatype().map_err(|e| e.to_string())? == Type::Break {
                            dec.skip().map_err(|e| e.to_string())?;
                            break;
                        }
                        if count >= MAX_INFLIGHT {
                            return Err(format!(
                                "MsgReplyTxIds: indefinite array exceeded max {MAX_INFLIGHT} items"
                            ));
                        }
                        count += 1;
                        // Outer entry: [GenTxId, size]
                        dec.array().map_err(|e| e.to_string())?;
                        // GenTxId = [era_id, txid_bytes]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        let tx_id_bytes = dec.bytes().map_err(|e| e.to_string())?;
                        if tx_id_bytes.len() != 32 {
                            return Err(format!(
                                "tx_id must be 32 bytes, got {}",
                                tx_id_bytes.len()
                            ));
                        }
                        let mut tx_id = [0u8; 32];
                        tx_id.copy_from_slice(tx_id_bytes);
                        let size = dec.u32().map_err(|e| e.to_string())?;
                        ids.push(TxIdAndSize {
                            era_id,
                            tx_id,
                            size_in_bytes: size,
                        });
                    }
                }
                Type::Array => {
                    // Definite-length: read the count, then iterate.
                    let len = dec
                        .array()
                        .map_err(|e| e.to_string())?
                        .ok_or("expected definite array length")?;
                    if len > MAX_INFLIGHT {
                        return Err(format!(
                            "MsgReplyTxIds: array length {len} exceeds max {MAX_INFLIGHT}"
                        ));
                    }
                    ids.reserve(len as usize);
                    for _ in 0..len {
                        // Outer entry: [GenTxId, size]
                        dec.array().map_err(|e| e.to_string())?;
                        // GenTxId = [era_id, txid_bytes]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        let tx_id_bytes = dec.bytes().map_err(|e| e.to_string())?;
                        if tx_id_bytes.len() != 32 {
                            return Err(format!(
                                "tx_id must be 32 bytes, got {}",
                                tx_id_bytes.len()
                            ));
                        }
                        let mut tx_id = [0u8; 32];
                        tx_id.copy_from_slice(tx_id_bytes);
                        let size = dec.u32().map_err(|e| e.to_string())?;
                        ids.push(TxIdAndSize {
                            era_id,
                            tx_id,
                            size_in_bytes: size,
                        });
                    }
                }
                other => {
                    return Err(format!("MsgReplyTxIds: expected array, got {other:?}"));
                }
            }
            Ok(TxSubmissionMessage::MsgReplyTxIds(ids))
        }
        TAG_REQUEST_TXS => {
            // Accept both definite and indefinite-length arrays.
            // Each element is a GenTxId = [era_id, txid_bytes] (HFC NS envelope).
            let mut ids: Vec<(u8, [u8; 32])> = Vec::new();
            match dec.datatype().map_err(|e| e.to_string())? {
                Type::ArrayIndef => {
                    // B2: Cap indefinite-array items at MAX_INFLIGHT.
                    dec.array().map_err(|e| e.to_string())?;
                    let mut count: u64 = 0;
                    loop {
                        if dec.datatype().map_err(|e| e.to_string())? == Type::Break {
                            dec.skip().map_err(|e| e.to_string())?;
                            break;
                        }
                        if count >= MAX_INFLIGHT {
                            return Err(format!(
                                "MsgRequestTxs: indefinite array exceeded max {MAX_INFLIGHT} items"
                            ));
                        }
                        count += 1;
                        // GenTxId = [era_id, txid_bytes]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        let id_bytes = dec.bytes().map_err(|e| e.to_string())?;
                        if id_bytes.len() != 32 {
                            return Err(format!("tx_id must be 32 bytes, got {}", id_bytes.len()));
                        }
                        let mut id = [0u8; 32];
                        id.copy_from_slice(id_bytes);
                        ids.push((era_id, id));
                    }
                }
                Type::Array => {
                    let len = dec
                        .array()
                        .map_err(|e| e.to_string())?
                        .ok_or("expected definite array length")?;
                    if len > MAX_INFLIGHT {
                        return Err(format!(
                            "MsgRequestTxs: array length {len} exceeds max {MAX_INFLIGHT}"
                        ));
                    }
                    ids.reserve(len as usize);
                    for _ in 0..len {
                        // GenTxId = [era_id, txid_bytes]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        let id_bytes = dec.bytes().map_err(|e| e.to_string())?;
                        if id_bytes.len() != 32 {
                            return Err(format!("tx_id must be 32 bytes, got {}", id_bytes.len()));
                        }
                        let mut id = [0u8; 32];
                        id.copy_from_slice(id_bytes);
                        ids.push((era_id, id));
                    }
                }
                other => {
                    return Err(format!("MsgRequestTxs: expected array, got {other:?}"));
                }
            }
            Ok(TxSubmissionMessage::MsgRequestTxs(ids))
        }
        TAG_REPLY_TXS => {
            // Accept both definite and indefinite-length arrays.
            // Each element is a GenTx = [era_id, tag(24)(tx_cbor)] (HFC NS + CBOR-in-CBOR).
            let mut txs: Vec<(u8, Vec<u8>)> = Vec::new();
            match dec.datatype().map_err(|e| e.to_string())? {
                Type::ArrayIndef => {
                    // B2: Cap indefinite-array items at MAX_INFLIGHT.
                    dec.array().map_err(|e| e.to_string())?;
                    let mut count: u64 = 0;
                    loop {
                        if dec.datatype().map_err(|e| e.to_string())? == Type::Break {
                            dec.skip().map_err(|e| e.to_string())?;
                            break;
                        }
                        if count >= MAX_INFLIGHT {
                            return Err(format!(
                                "MsgReplyTxs: indefinite array exceeded max {MAX_INFLIGHT} items"
                            ));
                        }
                        count += 1;
                        // GenTx = [era_id, tag(24)(tx_cbor)]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        // B6: Validate tag is exactly 24 (wrapCBORinCBOR convention).
                        // The Haskell codec explicitly requires tag 24; accepting any
                        // tag would allow CBOR semantic confusion in era decode loop.
                        let cbor_tag = dec.tag().map_err(|e| e.to_string())?;
                        if cbor_tag != minicbor::data::Tag::new(24) {
                            return Err(format!(
                                "MsgReplyTxs: expected tag(24) for tx body, got tag({})",
                                u64::from(cbor_tag)
                            ));
                        }
                        let tx_cbor = dec.bytes().map_err(|e| e.to_string())?.to_vec();
                        txs.push((era_id, tx_cbor));
                    }
                }
                Type::Array => {
                    let len = dec
                        .array()
                        .map_err(|e| e.to_string())?
                        .ok_or("expected definite array length")?;
                    if len > MAX_INFLIGHT {
                        return Err(format!(
                            "MsgReplyTxs: array length {len} exceeds max {MAX_INFLIGHT}"
                        ));
                    }
                    txs.reserve(len as usize);
                    for _ in 0..len {
                        // GenTx = [era_id, tag(24)(tx_cbor)]
                        dec.array().map_err(|e| e.to_string())?;
                        let era_id = dec.u8().map_err(|e| e.to_string())?;
                        // B6: Validate tag is exactly 24.
                        let cbor_tag = dec.tag().map_err(|e| e.to_string())?;
                        if cbor_tag != minicbor::data::Tag::new(24) {
                            return Err(format!(
                                "MsgReplyTxs: expected tag(24) for tx body, got tag({})",
                                u64::from(cbor_tag)
                            ));
                        }
                        let tx_cbor = dec.bytes().map_err(|e| e.to_string())?.to_vec();
                        txs.push((era_id, tx_cbor));
                    }
                }
                other => {
                    return Err(format!("MsgReplyTxs: expected array, got {other:?}"));
                }
            }
            Ok(TxSubmissionMessage::MsgReplyTxs(txs))
        }
        TAG_DONE => Ok(TxSubmissionMessage::MsgDone),
        _ => Err(format!("unknown TxSubmission2 message tag: {tag}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_init_roundtrip() {
        let encoded = encode_message(&TxSubmissionMessage::MsgInit);
        let decoded = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, TxSubmissionMessage::MsgInit));
        // Wire format: [6] = 0x81 0x06
        assert_eq!(encoded, vec![0x81, 0x06]);
    }

    #[test]
    fn msg_request_tx_ids_roundtrip() {
        let msg = TxSubmissionMessage::MsgRequestTxIds {
            blocking: false,
            ack_count: 0,
            req_count: 10,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
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
    fn msg_reply_tx_ids_roundtrip() {
        let msg = TxSubmissionMessage::MsgReplyTxIds(vec![
            TxIdAndSize {
                era_id: 6,
                tx_id: [0xAA; 32],
                size_in_bytes: 256,
            },
            TxIdAndSize {
                era_id: 6,
                tx_id: [0xBB; 32],
                size_in_bytes: 512,
            },
        ]);
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decoded {
            assert_eq!(ids.len(), 2);
            assert_eq!(ids[0].era_id, 6);
            assert_eq!(ids[0].tx_id, [0xAA; 32]);
            assert_eq!(ids[0].size_in_bytes, 256);
            assert_eq!(ids[1].era_id, 6);
            assert_eq!(ids[1].tx_id, [0xBB; 32]);
            assert_eq!(ids[1].size_in_bytes, 512);
        } else {
            panic!("expected MsgReplyTxIds");
        }
    }

    #[test]
    fn msg_request_txs_roundtrip() {
        // Each element is (era_id, tx_hash) — Conway era_id = 6.
        let msg = TxSubmissionMessage::MsgRequestTxs(vec![(6, [0xCC; 32]), (6, [0xDD; 32])]);
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgRequestTxs(ids) = decoded {
            assert_eq!(ids, vec![(6u8, [0xCC; 32]), (6u8, [0xDD; 32])]);
        } else {
            panic!("expected MsgRequestTxs");
        }
    }

    #[test]
    fn msg_reply_txs_roundtrip() {
        // Each element is (era_id, tx_cbor) — Conway era_id = 6.
        let msg = TxSubmissionMessage::MsgReplyTxs(vec![
            (6, vec![0x01, 0x02, 0x03]),
            (6, vec![0x04, 0x05]),
        ]);
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgReplyTxs(txs) = decoded {
            assert_eq!(
                txs,
                vec![(6u8, vec![0x01, 0x02, 0x03]), (6u8, vec![0x04, 0x05])]
            );
        } else {
            panic!("expected MsgReplyTxs");
        }
    }

    #[test]
    fn msg_done_roundtrip() {
        let encoded = encode_message(&TxSubmissionMessage::MsgDone);
        let decoded = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, TxSubmissionMessage::MsgDone));
        // Wire format: [4] = 0x81 0x04
        assert_eq!(encoded, vec![0x81, 0x04]);
    }

    #[test]
    fn first_request_must_be_non_blocking() {
        // The spec requires the first MsgRequestTxIds to be non-blocking with ack_count=0.
        // This is an encoding test — the server enforces this semantically.
        let msg = TxSubmissionMessage::MsgRequestTxIds {
            blocking: false,
            ack_count: 0,
            req_count: 3,
        };
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgRequestTxIds {
            blocking,
            ack_count,
            ..
        } = decoded
        {
            assert!(!blocking, "first request must be non-blocking");
            assert_eq!(ack_count, 0, "first request must have ack_count=0");
        }
    }

    #[test]
    fn empty_reply_tx_ids() {
        let msg = TxSubmissionMessage::MsgReplyTxIds(vec![]);
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decoded {
            assert!(ids.is_empty());
        } else {
            panic!("expected MsgReplyTxIds");
        }
    }

    // ─── Indefinite-length CBOR encoding tests ───────────────────────────────
    //
    // The CDDL spec requires indefinite-length arrays for inner lists in
    // TxSubmission2 messages.  0x9F is the CBOR "begin indefinite array" byte
    // and 0xFF is the "break" (end) code.

    #[test]
    fn reply_tx_ids_uses_indefinite_inner_array() {
        let msg = TxSubmissionMessage::MsgReplyTxIds(vec![TxIdAndSize {
            era_id: 6,
            tx_id: [0x11; 32],
            size_in_bytes: 100,
        }]);
        let encoded = encode_message(&msg);
        // The outer message array is definite (0x82 = array(2)).
        assert_eq!(encoded[0], 0x82, "outer array must be definite array(2)");
        // After the tag byte (0x01 for TAG_REPLY_TX_IDS), the inner list must
        // start with 0x9F (indefinite-length array marker).
        assert_eq!(encoded[1], 0x01, "tag must be TAG_REPLY_TX_IDS = 1");
        assert_eq!(
            encoded[2], 0x9F,
            "inner txIdsAndSizes list must use indefinite-length array (0x9F)"
        );
        // The encoding must end with 0xFF (break code).
        assert_eq!(
            *encoded.last().unwrap(),
            0xFF,
            "indefinite array must terminate with break code 0xFF"
        );
    }

    #[test]
    fn request_txs_uses_indefinite_inner_array() {
        let msg = TxSubmissionMessage::MsgRequestTxs(vec![(6, [0x22; 32])]);
        let encoded = encode_message(&msg);
        assert_eq!(encoded[0], 0x82, "outer array must be definite array(2)");
        assert_eq!(encoded[1], 0x02, "tag must be TAG_REQUEST_TXS = 2");
        assert_eq!(
            encoded[2], 0x9F,
            "inner txIdList must use indefinite-length array (0x9F)"
        );
        assert_eq!(
            *encoded.last().unwrap(),
            0xFF,
            "indefinite array must terminate with break code 0xFF"
        );
    }

    #[test]
    fn reply_txs_uses_indefinite_inner_array() {
        let msg = TxSubmissionMessage::MsgReplyTxs(vec![(6, vec![0xDE, 0xAD, 0xBE, 0xEF])]);
        let encoded = encode_message(&msg);
        assert_eq!(encoded[0], 0x82, "outer array must be definite array(2)");
        assert_eq!(encoded[1], 0x03, "tag must be TAG_REPLY_TXS = 3");
        assert_eq!(
            encoded[2], 0x9F,
            "inner txList must use indefinite-length array (0x9F)"
        );
        assert_eq!(
            *encoded.last().unwrap(),
            0xFF,
            "indefinite array must terminate with break code 0xFF"
        );
    }

    /// Decode a MsgReplyTxIds message that was encoded with a definite-length
    /// inner array.  Each entry is [[era_id, txid_bytes], size] (HFC GenTxId envelope).
    #[test]
    fn reply_tx_ids_decodes_definite_inner_array() {
        // Hand-craft: [1, [[[6, h"AA…AA"], 256], [[6, h"BB…BB"], 512]]]
        // outer array(2) = 0x82, tag 1 = 0x01
        // inner array(2) = 0x82, entry array(2) = 0x82
        // GenTxId array(2) = 0x82, era_id uint(6) = 0x06, bytes(32) 0xAA...
        // size uint 256 = 0x19 0x01 0x00
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(1).unwrap(); // TAG_REPLY_TX_IDS
        enc.array(2).unwrap(); // definite inner array of 2 entries
        for byte_val in [0xAAu8, 0xBBu8] {
            // Entry = [GenTxId, size]
            enc.array(2).unwrap();
            // GenTxId = [era_id, txid_bytes]
            enc.array(2).unwrap();
            enc.u8(6).unwrap(); // Conway era_id
            enc.bytes(&[byte_val; 32]).unwrap();
            enc.u32(if byte_val == 0xAA { 256 } else { 512 }).unwrap();
        }
        let decoded = decode_message(&buf).unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decoded {
            assert_eq!(ids.len(), 2);
            assert_eq!(ids[0].era_id, 6);
            assert_eq!(ids[0].tx_id, [0xAA; 32]);
            assert_eq!(ids[0].size_in_bytes, 256);
            assert_eq!(ids[1].era_id, 6);
            assert_eq!(ids[1].tx_id, [0xBB; 32]);
            assert_eq!(ids[1].size_in_bytes, 512);
        } else {
            panic!("expected MsgReplyTxIds");
        }
    }

    /// Decode a MsgReplyTxIds message that was encoded with an indefinite-length
    /// inner array, as Haskell nodes send.  Each entry is [[era_id, txid_bytes], size].
    #[test]
    fn reply_tx_ids_decodes_indefinite_inner_array() {
        // Hand-craft: [1, [_ [[6, h"CC…CC"], 99]]]
        // 0x82 = array(2), 0x01 = tag, 0x9F = begin_indef_array
        // outer entry array(2) = 0x82
        //   GenTxId array(2) = 0x82, era_id uint(6), bytes(32) 0xCC*32
        //   size uint(99)
        // 0xFF = break
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(1).unwrap();
            enc.begin_array().unwrap();
            // Entry = [GenTxId, size]
            enc.array(2).unwrap();
            // GenTxId = [era_id, txid_bytes]
            enc.array(2).unwrap();
            enc.u8(6).unwrap(); // Conway era_id
            enc.bytes(&[0xCCu8; 32]).unwrap();
            enc.u32(99).unwrap();
            enc.end().unwrap();
        }
        // Verify the inner array marker is indeed indefinite (0x9F).
        assert_eq!(buf[2], 0x9F);
        let decoded = decode_message(&buf).unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decoded {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].era_id, 6);
            assert_eq!(ids[0].tx_id, [0xCC; 32]);
            assert_eq!(ids[0].size_in_bytes, 99);
        } else {
            panic!("expected MsgReplyTxIds");
        }
    }

    /// Decode a MsgRequestTxs with an indefinite-length tx ID list.
    /// Each element is a GenTxId = [era_id, txid_bytes].
    #[test]
    fn request_txs_decodes_indefinite_inner_array() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(2).unwrap(); // TAG_REQUEST_TXS
            enc.begin_array().unwrap();
            // GenTxId = [era_id, txid_bytes]
            enc.array(2).unwrap();
            enc.u8(6).unwrap(); // Conway era_id
            enc.bytes(&[0xDDu8; 32]).unwrap();
            enc.end().unwrap();
        }
        assert_eq!(buf[2], 0x9F, "inner array must be indefinite");
        let decoded = decode_message(&buf).unwrap();
        if let TxSubmissionMessage::MsgRequestTxs(ids) = decoded {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0], (6u8, [0xDDu8; 32]));
        } else {
            panic!("expected MsgRequestTxs");
        }
    }

    /// Decode a MsgReplyTxs with an indefinite-length tx body list.
    /// Each element is a GenTx = [era_id, tag(24)(tx_cbor)].
    #[test]
    fn reply_txs_decodes_indefinite_inner_array() {
        let tx_body = vec![0x01u8, 0x02, 0x03, 0x04];
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(3).unwrap(); // TAG_REPLY_TXS
            enc.begin_array().unwrap();
            // GenTx = [era_id, tag(24)(tx_cbor)]
            enc.array(2).unwrap();
            enc.u8(6).unwrap(); // Conway era_id
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(&tx_body).unwrap();
            enc.end().unwrap();
        }
        assert_eq!(buf[2], 0x9F, "inner array must be indefinite");
        let decoded = decode_message(&buf).unwrap();
        if let TxSubmissionMessage::MsgReplyTxs(txs) = decoded {
            assert_eq!(txs.len(), 1);
            assert_eq!(txs[0], (6u8, tx_body));
        } else {
            panic!("expected MsgReplyTxs");
        }
    }

    /// The empty MsgReplyTxIds with indefinite encoding: [1, [_ ]] = [1, 0x9F 0xFF]
    #[test]
    fn empty_reply_tx_ids_indefinite() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(1).unwrap();
            enc.begin_array().unwrap();
            enc.end().unwrap();
        }
        let decoded = decode_message(&buf).unwrap();
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = decoded {
            assert!(ids.is_empty());
        } else {
            panic!("expected MsgReplyTxIds");
        }
    }

    // ─── B2: Indefinite-array cap tests ──────────────────────────────────────

    /// B2: MsgReplyTxIds indefinite-array must reject when count exceeds MAX_INFLIGHT.
    #[test]
    fn reply_tx_ids_indefinite_rejects_over_cap() {
        let cap = MAX_INFLIGHT as usize;
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TX_IDS).unwrap();
            enc.begin_array().unwrap();
            // Write cap+1 entries (one more than allowed).
            for i in 0..=(cap as u64) {
                enc.array(2).unwrap(); // [GenTxId, size]
                enc.array(2).unwrap(); // GenTxId
                enc.u8(6).unwrap();
                let mut id = [0u8; 32];
                id[0] = (i & 0xFF) as u8;
                id[1] = ((i >> 8) & 0xFF) as u8;
                enc.bytes(&id).unwrap();
                enc.u32(100).unwrap();
            }
            enc.end().unwrap();
        }
        let result = decode_message(&buf);
        assert!(
            result.is_err(),
            "should reject indefinite array > MAX_INFLIGHT"
        );
        assert!(
            result.unwrap_err().contains("exceeded max"),
            "error message should mention cap"
        );
    }

    /// B2: MsgReplyTxIds indefinite-array at exactly MAX_INFLIGHT must succeed.
    #[test]
    fn reply_tx_ids_indefinite_accepts_at_cap() {
        let cap = MAX_INFLIGHT as usize;
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TX_IDS).unwrap();
            enc.begin_array().unwrap();
            for i in 0..cap {
                enc.array(2).unwrap();
                enc.array(2).unwrap();
                enc.u8(6).unwrap();
                let mut id = [0u8; 32];
                id[0] = (i & 0xFF) as u8;
                id[1] = ((i >> 8) & 0xFF) as u8;
                enc.bytes(&id).unwrap();
                enc.u32(100).unwrap();
            }
            enc.end().unwrap();
        }
        let result = decode_message(&buf);
        assert!(
            result.is_ok(),
            "exactly MAX_INFLIGHT items must be accepted"
        );
        if let TxSubmissionMessage::MsgReplyTxIds(ids) = result.unwrap() {
            assert_eq!(ids.len(), cap);
        }
    }

    /// B2: MsgRequestTxs indefinite-array must reject when count exceeds MAX_INFLIGHT.
    #[test]
    fn request_txs_indefinite_rejects_over_cap() {
        let cap = MAX_INFLIGHT as usize;
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REQUEST_TXS).unwrap();
            enc.begin_array().unwrap();
            for i in 0..=(cap as u64) {
                enc.array(2).unwrap(); // GenTxId
                enc.u8(6).unwrap();
                let mut id = [0u8; 32];
                id[0] = (i & 0xFF) as u8;
                id[1] = ((i >> 8) & 0xFF) as u8;
                enc.bytes(&id).unwrap();
            }
            enc.end().unwrap();
        }
        let result = decode_message(&buf);
        assert!(
            result.is_err(),
            "should reject indefinite array > MAX_INFLIGHT"
        );
    }

    /// B2: MsgReplyTxs indefinite-array must reject when count exceeds MAX_INFLIGHT.
    #[test]
    fn reply_txs_indefinite_rejects_over_cap() {
        let cap = MAX_INFLIGHT as usize;
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TXS).unwrap();
            enc.begin_array().unwrap();
            for _ in 0..=(cap as u64) {
                enc.array(2).unwrap(); // GenTx
                enc.u8(6).unwrap();
                enc.tag(minicbor::data::Tag::new(24)).unwrap();
                enc.bytes(&[0x01]).unwrap();
            }
            enc.end().unwrap();
        }
        let result = decode_message(&buf);
        assert!(
            result.is_err(),
            "should reject indefinite array > MAX_INFLIGHT"
        );
    }

    // ─── B6: tag(24) validation tests ────────────────────────────────────────

    /// B6: MsgReplyTxs with wrong tag value must be rejected.
    #[test]
    fn reply_txs_wrong_tag_rejected() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TXS).unwrap();
            enc.begin_array().unwrap();
            enc.array(2).unwrap(); // GenTx
            enc.u8(6).unwrap();
            // Use tag(6) instead of tag(24)
            enc.tag(minicbor::data::Tag::new(6)).unwrap();
            enc.bytes(&[0xDE, 0xAD]).unwrap();
            enc.end().unwrap();
        }
        let result = decode_message(&buf);
        assert!(result.is_err(), "tag(6) in MsgReplyTxs must be rejected");
        let err = result.unwrap_err();
        assert!(
            err.contains("tag(24)"),
            "error should mention expected tag(24)"
        );
    }

    /// B6: MsgReplyTxs with tag(24) must be accepted (the normal case).
    #[test]
    fn reply_txs_correct_tag_accepted() {
        let tx_body = vec![0x82u8, 0x00, 0x01];
        let msg = TxSubmissionMessage::MsgReplyTxs(vec![(6, tx_body.clone())]);
        let encoded = encode_message(&msg);
        let decoded = decode_message(&encoded).unwrap();
        if let TxSubmissionMessage::MsgReplyTxs(txs) = decoded {
            assert_eq!(txs.len(), 1);
            assert_eq!(txs[0].1, tx_body);
        } else {
            panic!("expected MsgReplyTxs");
        }
    }

    /// B6: MsgReplyTxs with tag(24) in definite-array path must be accepted.
    #[test]
    fn reply_txs_definite_correct_tag_accepted() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TXS).unwrap();
            enc.array(1).unwrap(); // definite array of 1
            enc.array(2).unwrap(); // GenTx
            enc.u8(6).unwrap();
            enc.tag(minicbor::data::Tag::new(24)).unwrap();
            enc.bytes(&[0xAB, 0xCD]).unwrap();
        }
        let result = decode_message(&buf);
        assert!(result.is_ok());
    }

    /// B6: MsgReplyTxs with wrong tag in definite-array path must be rejected.
    #[test]
    fn reply_txs_definite_wrong_tag_rejected() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u64(TAG_REPLY_TXS).unwrap();
            enc.array(1).unwrap(); // definite array of 1
            enc.array(2).unwrap(); // GenTx
            enc.u8(6).unwrap();
            enc.tag(minicbor::data::Tag::new(999)).unwrap();
            enc.bytes(&[0xAB, 0xCD]).unwrap();
        }
        let result = decode_message(&buf);
        assert!(result.is_err(), "tag(999) must be rejected");
    }
}
