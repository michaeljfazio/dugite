//! `dugite_primitives::Block` → `utxorpc.v1beta.cardano.Block` mapping,
//! plus the `sync` wrappers (`AnyChainBlock`, `BlockRef`).

use crate::context::{RawBlock, TipInfo};
use crate::map::common::hash_bytes;
use crate::map::tx::tx_to_proto;
use crate::proto::v1beta::cardano as pb_cardano;
use crate::proto::v1beta::sync as pb_sync;
use dugite_primitives::block::Block;

/// Map a `dugite_primitives::Block` to the parsed Cardano protobuf shape.
///
/// `timestamp` is left at zero — populating it requires the era-history
/// projection (`EraHistoryView`) which lives behind `LedgerContext`.
/// M2's QueryService work fills it; M1.B sync clients that need the
/// wall-clock can either re-derive from `header.slot` + ReadEra or
/// parse `native_bytes` themselves.
pub fn block_to_proto(block: &Block) -> pb_cardano::Block {
    let header = pb_cardano::BlockHeader {
        slot: block.header.slot.0,
        hash: hash_bytes(&block.header.header_hash),
        height: block.header.block_number.0,
    };
    let body = pb_cardano::BlockBody {
        tx: block.transactions.iter().map(tx_to_proto).collect(),
    };
    pb_cardano::Block {
        header: Some(header),
        body: Some(body),
        timestamp: 0,
    }
}

/// Wrap a parsed [`Block`] together with its original CBOR bytes in
/// [`AnyChainBlock`] — the envelope returned by `FetchBlock` /
/// `DumpHistory` / `FollowTip`.
///
/// `native_bytes` is the verbatim block CBOR straight from
/// [`RawBlock::cbor`] / [`Block::raw_cbor`]; clients that prefer the
/// raw bytes (e.g. for offline parsing or for byte-exact retransmission)
/// can ignore the parsed envelope.
pub fn any_chain_block(raw: &RawBlock, parsed: Option<&Block>) -> pb_sync::AnyChainBlock {
    pb_sync::AnyChainBlock {
        native_bytes: raw.cbor.clone(),
        chain: parsed.map(|b| pb_sync::any_chain_block::Chain::Cardano(block_to_proto(b))),
    }
}

/// Project a [`RawBlock`] into the [`BlockRef`] handle that downstream
/// services (FollowTip pagination, DumpHistory next_token) hand back to
/// the client.
pub fn block_ref_from_raw(raw: &RawBlock) -> pb_sync::BlockRef {
    pb_sync::BlockRef {
        slot: raw.slot,
        hash: raw.hash.to_vec(),
        height: raw.block_number,
        timestamp: 0,
    }
}

/// Project a [`TipInfo`] into a [`BlockRef`] — used by `ReadTip` and as
/// the `tip` field on every `FollowTipResponse`.
pub fn block_ref_from_tip(tip: &TipInfo) -> pb_sync::BlockRef {
    pb_sync::BlockRef {
        slot: tip.slot,
        hash: tip.hash.to_vec(),
        height: tip.block_number,
        timestamp: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::Era;

    fn raw_block(slot: u64, hash: [u8; 32], block_no: u64, cbor: Vec<u8>) -> RawBlock {
        RawBlock {
            slot,
            hash,
            block_number: block_no,
            era: Era::Conway,
            cbor,
        }
    }

    #[test]
    fn any_chain_block_carries_native_bytes_without_parse() {
        let raw = raw_block(123, [9u8; 32], 7, vec![0xA, 0xB, 0xC]);
        let envelope = any_chain_block(&raw, None);
        assert_eq!(envelope.native_bytes, vec![0xA, 0xB, 0xC]);
        assert!(envelope.chain.is_none());
    }

    #[test]
    fn block_ref_from_raw_round_trips_metadata() {
        let raw = raw_block(123, [9u8; 32], 7, vec![]);
        let r = block_ref_from_raw(&raw);
        assert_eq!(r.slot, 123);
        assert_eq!(r.height, 7);
        assert_eq!(r.hash, vec![9u8; 32]);
    }

    #[test]
    fn block_ref_from_tip_round_trips_metadata() {
        let tip = TipInfo {
            slot: 9_999,
            hash: [1u8; 32],
            block_number: 42,
            era: Era::Conway,
        };
        let r = block_ref_from_tip(&tip);
        assert_eq!(r.slot, 9_999);
        assert_eq!(r.height, 42);
        assert_eq!(r.hash, vec![1u8; 32]);
    }
}
