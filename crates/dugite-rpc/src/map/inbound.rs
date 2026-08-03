//! utxorpc protobuf → dugite mapping: the **inbound**, attacker-controlled
//! direction.
//!
//! Every other module under `map/` goes dugite → protobuf, over values the node
//! already validated. This one goes the other way, over bytes a gRPC client
//! chose, and it is the only mapping surface where a malformed input is
//! expected rather than exceptional.
//!
//! # Why it exists (#983)
//!
//! Until now there was no inbound mapping module: the conversions lived inline
//! in `services/{query,submit,sync}.rs`, each written as
//!
//! ```ignore
//! for r in &refs {
//!     if r.hash.len() == 32 {
//!         let mut arr = [0u8; 32];
//!         arr.copy_from_slice(&r.hash);
//!         inputs.push(TransactionInput { … });
//!     }
//!     // else: silently skipped
//! }
//! ```
//!
//! A reference the node could not parse was **dropped without a word**. The
//! consequences differ per call and none of them is benign:
//!
//! * `ReadUtxos` — a client asking about three UTxOs with one malformed key
//!   gets two results back and no way to tell which request was ignored, since
//!   a missing UTxO legitimately yields fewer items too.
//! * `WaitForTx` — the stream simply never reports on the dropped ref, so the
//!   client waits forever for a transaction the server was never watching.
//! * `FollowTip` intersect — the client asks to intersect at points A, B, C;
//!   the node intersects at A and C and answers with an agreed point the
//!   client never offered.
//!
//! That is the #963 shape in a different protocol: a request partially ignored
//! rather than refused. dugite's posture is to reject loudly, so these now fail
//! with `InvalidArgument` naming the offending index and the length seen.
//!
//! Extracting them here is also what makes the surface fuzzable at all —
//! `fuzz_rpc_inbound` drives these functions directly, which it could not do
//! while they were inline in `async fn` service methods behind tonic.

use dugite_primitives::block::Point;
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::TransactionInput;

/// Why an inbound protobuf value could not be mapped.
///
/// Deliberately carries the field name, the element index and the observed
/// length: "invalid argument" on its own leaves the client guessing which of
/// its fifty keys was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundError {
    pub field: &'static str,
    pub index: usize,
    pub message: String,
}

impl std::fmt::Display for InboundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}[{}]: {}", self.field, self.index, self.message)
    }
}

impl From<InboundError> for tonic::Status {
    fn from(e: InboundError) -> Self {
        tonic::Status::invalid_argument(e.to_string())
    }
}

/// A 32-byte hash from a protobuf `bytes` field.
///
/// Width is enforced rather than tolerated. `copy_from_slice` panics on a
/// length mismatch, so the length check is load-bearing — but the check must
/// produce an *error*, not a skip.
pub fn hash32(bytes: &[u8], field: &'static str, index: usize) -> Result<Hash32, InboundError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| InboundError {
        field,
        index,
        message: format!("expected a 32-byte hash, got {} bytes", bytes.len()),
    })?;
    Ok(Hash32::from_bytes(arr))
}

/// `TxoRef { hash, index }` → `TransactionInput`.
pub fn txo_ref(
    hash: &[u8],
    output_index: u32,
    field: &'static str,
    index: usize,
) -> Result<TransactionInput, InboundError> {
    Ok(TransactionInput {
        transaction_id: hash32(hash, field, index)?,
        index: output_index,
    })
}

/// A whole list of `TxoRef`s, all-or-nothing.
///
/// All-or-nothing is the point: a partially-honoured request is
/// indistinguishable from a fully-honoured one whose extra entries did not
/// exist on chain.
pub fn txo_refs<'a>(
    refs: impl IntoIterator<Item = (&'a [u8], u32)>,
    field: &'static str,
) -> Result<Vec<TransactionInput>, InboundError> {
    refs.into_iter()
        .enumerate()
        .map(|(i, (hash, idx))| txo_ref(hash, idx, field, i))
        .collect()
}

/// A list of transaction hashes (`WaitForTx`'s `ref` field).
pub fn tx_hashes<'a>(
    refs: impl IntoIterator<Item = &'a [u8]>,
    field: &'static str,
) -> Result<Vec<Hash32>, InboundError> {
    refs.into_iter()
        .enumerate()
        .map(|(i, h)| hash32(h, field, i))
        .collect()
}

/// `BlockRef { hash, slot }` → `Point`.
///
/// An empty hash is `Point::Origin`, matching how `point_to_block_ref` writes
/// it on the way out; every other length must be exactly 32.
pub fn block_ref(
    hash: &[u8],
    slot: u64,
    field: &'static str,
    index: usize,
) -> Result<Point, InboundError> {
    if hash.is_empty() {
        return Ok(Point::Origin);
    }
    Ok(Point::Specific(SlotNo(slot), hash32(hash, field, index)?))
}

/// A list of `BlockRef`s, all-or-nothing — see [`txo_refs`].
pub fn block_refs<'a>(
    refs: impl IntoIterator<Item = (&'a [u8], u64)>,
    field: &'static str,
) -> Result<Vec<Point>, InboundError> {
    refs.into_iter()
        .enumerate()
        .map(|(i, (hash, slot))| block_ref(hash, slot, field, i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash32_accepts_exactly_32_bytes() {
        assert!(hash32(&[7u8; 32], "f", 0).is_ok());
    }

    /// The pre-#983 code silently skipped every one of these.
    #[test]
    fn hash32_rejects_every_other_width() {
        for len in [0usize, 1, 28, 31, 33, 64] {
            let e = hash32(&vec![0u8; len], "keys", 3).expect_err("must reject");
            assert_eq!(e.index, 3);
            assert_eq!(e.field, "keys");
            assert!(
                e.message.contains(&len.to_string()),
                "the error must name the length seen, got {e}"
            );
        }
    }

    /// All-or-nothing: one bad element fails the whole request rather than
    /// quietly shortening it.
    #[test]
    fn a_single_bad_ref_fails_the_whole_list() {
        let good: Vec<u8> = vec![1u8; 32];
        let bad: Vec<u8> = vec![2u8; 31];
        let refs: Vec<(&[u8], u32)> = vec![(&good, 0), (&bad, 1), (&good, 2)];
        let err = txo_refs(refs, "keys").expect_err("must reject");
        assert_eq!(err.index, 1, "the error must name WHICH element was bad");
    }

    #[test]
    fn a_well_formed_list_maps_one_for_one() {
        let a: Vec<u8> = vec![1u8; 32];
        let b: Vec<u8> = vec![2u8; 32];
        let refs: Vec<(&[u8], u32)> = vec![(&a, 0), (&b, 7)];
        let got = txo_refs(refs, "keys").expect("valid");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].index, 7);
        assert_eq!(got[1].transaction_id, Hash32::from_bytes([2u8; 32]));
    }

    /// An empty hash is Origin on the way in, mirroring `point_to_block_ref`
    /// on the way out. Round-tripping the two must not shift the meaning.
    #[test]
    fn empty_block_ref_hash_is_origin() {
        assert_eq!(block_ref(&[], 0, "f", 0).unwrap(), Point::Origin);
        assert_eq!(block_ref(&[], 99, "f", 0).unwrap(), Point::Origin);
    }

    #[test]
    fn block_ref_rejects_a_short_hash() {
        assert!(block_ref(&[0u8; 31], 5, "intersect", 2).is_err());
    }

    #[test]
    fn inbound_error_becomes_invalid_argument() {
        let status: tonic::Status = InboundError {
            field: "keys",
            index: 4,
            message: "expected a 32-byte hash, got 3 bytes".into(),
        }
        .into();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("keys[4]"));
    }
}
