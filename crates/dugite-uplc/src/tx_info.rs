//! Helpers for constructing `TxInfoV1` / `TxInfoV2` / `TxInfoV3`
//! values from piecewise tx fields.
//!
//! The builders here take individual fields (fee as `u64`,
//! signatories as `Vec<[u8; 28]>`, etc.) rather than a concrete
//! `dugite_primitives::Transaction` so that dugite-uplc stays free
//! of the dugite-primitives dependency. Callers (dugite-ledger) do
//! the per-field extraction and pass the result here.
//!
//! All builders default the complex fields (votes, proposal
//! procedures, treasury) to empty / `None`. Callers that need
//! Conway-era values populate them after construction via the
//! standard struct-update syntax:
//!
//! ```ignore
//! let tx_info = TxInfoV3 {
//!     votes: votes_extracted_from_tx,
//!     proposal_procedures: ...,
//!     ..build_v3_skeleton(BuildV3Inputs { /* simple fields */ })
//! };
//! ```

use crate::script_context::{
    GovActionId, PlutusValue, PosixTimeRange, ProposalProcedure, PubKeyHash, ScriptPurpose, TxId,
    TxInInfo, TxInfoV1, TxInfoV2, TxInfoV3, TxOut, Vote, Voter,
};
use num_bigint::BigInt;

/// Inputs to [`build_v3_skeleton`].
///
/// The fields here are the "easy" ones — fee, signatories, txid,
/// valid_range — that map cleanly from any wire-format
/// representation. The "hard" ones (inputs / outputs / mint / etc.)
/// require Address + Value translation and are wired by the caller
/// using the structured `script_context::*` types directly.
#[derive(Debug, Clone)]
pub struct BuildV3Inputs {
    pub txid: TxId,
    pub fee_lovelace: u64,
    pub signatories: Vec<PubKeyHash>,
    pub valid_range: PosixTimeRange,
    /// Treasury value at the start of the slot containing the tx
    /// (None if the tx is pre-Conway).
    pub current_treasury: Option<i64>,
    /// Treasury donation declared by this tx (None for non-donation
    /// txs).
    pub treasury_donation: Option<i64>,
}

/// Construct a `TxInfoV3` with the easy fields populated and
/// everything else defaulted to empty / `None`. Callers add their
/// transaction-specific data (inputs, outputs, mint, redeemers,
/// datums, votes, proposals) via struct-update syntax after.
pub fn build_v3_skeleton(inputs: BuildV3Inputs) -> TxInfoV3 {
    TxInfoV3 {
        inputs: Vec::new(),
        reference_inputs: Vec::new(),
        outputs: Vec::new(),
        fee: BigInt::from(inputs.fee_lovelace),
        mint: PlutusValue::default(),
        certs: Vec::new(),
        wdrl: Vec::new(),
        valid_range: inputs.valid_range,
        signatories: inputs.signatories,
        redeemers: Vec::new(),
        datums: Vec::new(),
        txid: inputs.txid,
        votes: Vec::<(Voter, Vec<(GovActionId, Vote)>)>::new(),
        proposal_procedures: Vec::<ProposalProcedure>::new(),
        current_treasury: inputs.current_treasury.map(BigInt::from),
        treasury_donation: inputs.treasury_donation.map(BigInt::from),
    }
}

/// Inputs to [`build_v2_skeleton`].
#[derive(Debug, Clone)]
pub struct BuildV2Inputs {
    pub txid: TxId,
    pub fee_lovelace: u64,
    pub signatories: Vec<PubKeyHash>,
    pub valid_range: PosixTimeRange,
}

/// Skeleton V2 TxInfo. Conway-era fields don't apply (V2 predates
/// them); reference_inputs / redeemers map / data map default empty.
pub fn build_v2_skeleton(inputs: BuildV2Inputs) -> TxInfoV2 {
    TxInfoV2 {
        inputs: Vec::new(),
        reference_inputs: Vec::new(),
        outputs: Vec::new(),
        fee: BigInt::from(inputs.fee_lovelace),
        mint: PlutusValue::default(),
        dcert: Vec::new(),
        wdrl: Vec::new(),
        valid_range: inputs.valid_range,
        signatories: inputs.signatories,
        redeemers: Vec::new(),
        data: Vec::new(),
        txid: inputs.txid,
    }
}

/// Inputs to [`build_v1_skeleton`].
#[derive(Debug, Clone)]
pub struct BuildV1Inputs {
    pub txid: TxId,
    pub fee_lovelace: u64,
    pub signatories: Vec<PubKeyHash>,
    pub valid_range: PosixTimeRange,
}

/// Skeleton V1 TxInfo (Alonzo era — no reference inputs, no
/// redeemers-map, no inline datums).
pub fn build_v1_skeleton(inputs: BuildV1Inputs) -> TxInfoV1 {
    TxInfoV1 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        fee: BigInt::from(inputs.fee_lovelace),
        mint: PlutusValue::default(),
        dcert: Vec::new(),
        wdrl: Vec::new(),
        valid_range: inputs.valid_range,
        signatories: inputs.signatories,
        data: Vec::new(),
        txid: inputs.txid,
    }
}

/// Construct a synthetic ScriptPurpose for the most common shapes
/// dugite-ledger needs to validate. Surfaced as helpers so callers
/// don't have to import every individual variant.
pub fn purpose_minting(policy: &[u8; 28]) -> ScriptPurpose {
    ScriptPurpose::Minting(*policy)
}

pub fn purpose_spending(tx_id: TxId, idx: u64) -> ScriptPurpose {
    ScriptPurpose::Spending(crate::script_context::TxOutRef { tx_id, idx })
}

/// Aggregate the `Vec<TxInInfo>` callers build from their UTxO map
/// + the tx's inputs list. Useful for wiring the inputs / reference
///   inputs of a per-version TxInfo without manual `Vec::push` loops.
pub fn collect_inputs(items: Vec<TxInInfo>) -> Vec<TxInInfo> {
    items
}

/// Aggregate the `Vec<TxOut>` callers build from their tx's outputs.
pub fn collect_outputs(items: Vec<TxOut>) -> Vec<TxOut> {
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_context::{Credential, ScriptContextV3, ScriptInfo};
    use crate::Data;

    #[test]
    fn build_v3_skeleton_has_correct_simple_fields() {
        let inputs = BuildV3Inputs {
            txid: [0x42u8; 32],
            fee_lovelace: 170_000,
            signatories: vec![[1u8; 28], [2u8; 28]],
            valid_range: PosixTimeRange {
                lower: Some(1000),
                upper: Some(2000),
            },
            current_treasury: Some(123_456),
            treasury_donation: None,
        };
        let info = build_v3_skeleton(inputs);
        assert_eq!(info.fee, BigInt::from(170_000));
        assert_eq!(info.signatories.len(), 2);
        assert_eq!(info.txid, [0x42u8; 32]);
        assert_eq!(info.current_treasury, Some(BigInt::from(123_456)));
        assert_eq!(info.treasury_donation, None);
        // Complex fields default empty.
        assert!(info.inputs.is_empty());
        assert!(info.votes.is_empty());
        assert!(info.proposal_procedures.is_empty());
    }

    #[test]
    fn build_v2_skeleton_excludes_conway_fields() {
        let inputs = BuildV2Inputs {
            txid: [1u8; 32],
            fee_lovelace: 100,
            signatories: vec![],
            valid_range: PosixTimeRange {
                lower: None,
                upper: None,
            },
        };
        let info = build_v2_skeleton(inputs);
        assert_eq!(info.fee, BigInt::from(100));
        assert!(info.dcert.is_empty());
        assert!(info.wdrl.is_empty());
    }

    #[test]
    fn build_v1_skeleton_excludes_v2_v3_fields() {
        let inputs = BuildV1Inputs {
            txid: [2u8; 32],
            fee_lovelace: 50,
            signatories: vec![[3u8; 28]],
            valid_range: PosixTimeRange {
                lower: None,
                upper: None,
            },
        };
        let info = build_v1_skeleton(inputs);
        assert_eq!(info.signatories.len(), 1);
        // V1 has no reference_inputs field, so test that the
        // resulting struct's data map is empty (the V1 equivalent of
        // "no datums map").
        assert!(info.data.is_empty());
    }

    #[test]
    fn purpose_helpers() {
        let p_mint = purpose_minting(&[7u8; 28]);
        assert!(matches!(p_mint, ScriptPurpose::Minting(h) if h == [7u8; 28]));
        let p_spend = purpose_spending([8u8; 32], 4);
        assert!(matches!(p_spend, ScriptPurpose::Spending(_)));
    }

    #[test]
    fn v3_skeleton_round_trips_through_cbor() {
        // Build → to_data() → CBOR encode → decode → equal.
        let info = build_v3_skeleton(BuildV3Inputs {
            txid: [0xab; 32],
            fee_lovelace: 200_000,
            signatories: vec![[1u8; 28]],
            valid_range: PosixTimeRange {
                lower: Some(0),
                upper: Some(1_000_000_000),
            },
            current_treasury: None,
            treasury_donation: None,
        });
        let ctx = ScriptContextV3 {
            tx_info: std::rc::Rc::new(info),
            redeemer: Data::I(BigInt::from(0)),
            script_info: ScriptInfo::Rewarding(Credential::PubKey([1u8; 28])),
        };
        let d = ctx.to_data();
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }
}
