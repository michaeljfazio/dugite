//! V1 and V2 `TxInfo` builders.
//!
//! V1 is the Alonzo-era TxInfo shape: no reference_inputs, no inline
//! datums, no redeemers map, no governance. V2 (Babbage+) adds
//! reference_inputs, inline datums, and the redeemers map but
//! still has no governance fields.
//!
//! Both are essentially **projections** of [`TxInfoV3`] — the same
//! input transaction yields per-version TxInfos that share the
//! inputs / outputs / fee / mint / valid_range / signatories / datums
//! / txid fields and differ only in which Conway-era extensions
//! are visible. We build them directly from the primitive
//! Transaction here rather than projecting from V3 so each version
//! reads cleanly in isolation.

use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::populate_gov::certificates_to_plutus;
use crate::script_context::{TxCert, TxInfoV1, TxInfoV2};
use crate::tx_info_populate::{
    datums_to_plutus, inputs_to_txininfos, mint_to_plutus, output_to_plutus,
    required_signers_to_plutus_padded, tx_hash_to_array, valid_range_to_posix,
    withdrawals_to_plutus,
};
use dugite_primitives::transaction::{
    Transaction as PrimTransaction, TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut,
};
use num_bigint::BigInt;

/// Build a [`TxInfoV1`] (Alonzo shape) from a decoded transaction.
///
/// V1 omits reference_inputs, inline datums, and Conway-era
/// governance/treasury fields. Certificates are populated via the
/// shared `populate_gov::certificates_to_plutus`. Redeemers do not
/// appear on V1 TxInfo at all — V1 redeemers are surfaced through
/// the per-redeemer `ScriptContext.purpose`.
pub fn populate_tx_info_v1(
    tx: &PrimTransaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    slot_config: &SlotConfig,
) -> Result<TxInfoV1, PhaseTwoError> {
    let inputs = inputs_to_txininfos(&tx.body.inputs, resolved)?;
    let outputs: Vec<_> = tx
        .body
        .outputs
        .iter()
        .map(output_to_plutus)
        .collect::<Result<_, _>>()?;
    let mint = mint_to_plutus(&tx.body.mint);
    let valid_range = valid_range_to_posix(
        tx.body.validity_interval_start.map(|s| s.0),
        tx.body.ttl.map(|s| s.0),
        slot_config,
    )?;
    let signatories = required_signers_to_plutus_padded(&tx.body.required_signers);
    let dcert: Vec<TxCert> = certificates_to_plutus(&tx.body.certificates)?;
    let wdrl = withdrawals_to_plutus(&tx.body.withdrawals)?;
    let data = datums_to_plutus(&tx.witness_set.plutus_data)?;
    Ok(TxInfoV1 {
        inputs,
        outputs,
        fee: BigInt::from(tx.body.fee.0),
        mint,
        dcert,
        wdrl,
        valid_range,
        signatories,
        data,
        txid: tx_hash_to_array(&tx.hash),
    })
}

/// Build a [`TxInfoV2`] (Babbage shape) from a decoded transaction.
///
/// V2 extends V1 with `reference_inputs`, inline-datum visibility via
/// `OutputDatum::Inline(...)` (already produced by `output_to_plutus`),
/// and a `redeemers` map. The redeemers map is left **empty for now**
/// — the per-redeemer purpose + Data mapping lands in UPLC-9 part 3f.
pub fn populate_tx_info_v2(
    tx: &PrimTransaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    slot_config: &SlotConfig,
) -> Result<TxInfoV2, PhaseTwoError> {
    let inputs = inputs_to_txininfos(&tx.body.inputs, resolved)?;
    let reference_inputs = inputs_to_txininfos(&tx.body.reference_inputs, resolved)?;
    let outputs: Vec<_> = tx
        .body
        .outputs
        .iter()
        .map(output_to_plutus)
        .collect::<Result<_, _>>()?;
    let mint = mint_to_plutus(&tx.body.mint);
    let valid_range = valid_range_to_posix(
        tx.body.validity_interval_start.map(|s| s.0),
        tx.body.ttl.map(|s| s.0),
        slot_config,
    )?;
    let signatories = required_signers_to_plutus_padded(&tx.body.required_signers);
    let dcert: Vec<TxCert> = certificates_to_plutus(&tx.body.certificates)?;
    let wdrl = withdrawals_to_plutus(&tx.body.withdrawals)?;
    let data = datums_to_plutus(&tx.witness_set.plutus_data)?;
    Ok(TxInfoV2 {
        inputs,
        reference_inputs,
        outputs,
        fee: BigInt::from(tx.body.fee.0),
        mint,
        dcert,
        wdrl,
        valid_range,
        signatories,
        // Filled by UPLC-9 part 3g (redeemers map).
        redeemers: Vec::new(),
        data,
        txid: tx_hash_to_array(&tx.hash),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address as PrimAddress, EnterpriseAddress};
    use dugite_primitives::credentials::Credential as PrimCred;
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::time::SlotNo;
    use dugite_primitives::transaction::{
        OutputDatum as PrimOutputDatum, Transaction, TransactionBody, TransactionInput,
        TransactionOutput, TransactionWitnessSet,
    };
    use dugite_primitives::value::{Lovelace, Value};
    use std::collections::BTreeMap;

    fn h28(b: u8) -> dugite_primitives::hash::Hash28 {
        Hash::<28>([b; 28])
    }

    fn h32(b: u8) -> dugite_primitives::hash::Hash<32> {
        Hash::<32>([b; 32])
    }

    fn enterprise_output(lovelace: u64) -> TransactionOutput {
        TransactionOutput {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::VerificationKey(h28(0x88)),
            }),
            value: Value::lovelace(lovelace),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    fn empty_witness_set() -> TransactionWitnessSet {
        TransactionWitnessSet {
            vkey_witnesses: vec![],
            native_scripts: vec![],
            bootstrap_witnesses: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            plutus_data: vec![],
            redeemers: vec![],
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        }
    }

    fn minimal_body(fee: u64) -> TransactionBody {
        TransactionBody {
            inputs: vec![],
            outputs: vec![],
            fee: Lovelace(fee),
            ttl: None,
            certificates: vec![],
            withdrawals: BTreeMap::new(),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: BTreeMap::new(),
            script_data_hash: None,
            collateral: vec![],
            required_signers: vec![],
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: vec![],
            update: None,
            voting_procedures: BTreeMap::new(),
            proposal_procedures: vec![],
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
        }
    }

    fn build_tx(body: TransactionBody) -> Transaction {
        Transaction {
            hash: h32(0xab),
            era: Era::Alonzo,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    fn slot_cfg() -> SlotConfig {
        SlotConfig {
            network_start_unix_seconds: 1_666_656_000,
            slot_zero_offset: 0,
            slot_length_ms: 1_000,
        }
    }

    #[test]
    fn v1_minimal_tx_yields_empty_collections() {
        let tx = build_tx(minimal_body(100));
        let info = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.fee, BigInt::from(100));
        assert_eq!(info.txid, [0xab; 32]);
        assert!(info.inputs.is_empty());
        assert!(info.outputs.is_empty());
        assert!(info.dcert.is_empty());
        assert!(info.wdrl.is_empty());
        assert!(info.data.is_empty());
    }

    #[test]
    fn v1_carries_outputs_and_valid_range() {
        let mut body = minimal_body(50);
        body.outputs = vec![enterprise_output(900_000)];
        body.validity_interval_start = Some(SlotNo(5));
        body.ttl = Some(SlotNo(15));
        let tx = build_tx(body);
        let info = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.outputs.len(), 1);
        assert_eq!(info.valid_range.lower, Some(1_666_656_005_000));
        assert_eq!(info.valid_range.upper, Some(1_666_656_015_000));
    }

    #[test]
    fn v1_carries_withdrawals() {
        // Reward address: header 0xe0 (mainnet, key-stake), 28-byte hash.
        let mut wdrl_addr = vec![0xe0u8];
        wdrl_addr.extend([0x77u8; 28]);
        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(wdrl_addr, Lovelace(50));
        let mut body = minimal_body(1);
        body.withdrawals = withdrawals;
        let tx = build_tx(body);
        let info = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.wdrl.len(), 1);
        assert_eq!(info.wdrl[0].1, BigInt::from(50));
    }

    #[test]
    fn v1_propagates_byron_output_failure() {
        let mut body = minimal_body(1);
        body.outputs = vec![TransactionOutput {
            address: PrimAddress::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![],
            }),
            value: Value::lovelace(1),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }];
        let tx = build_tx(body);
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn v2_minimal_tx_yields_empty_collections() {
        let tx = build_tx(minimal_body(123));
        let info = populate_tx_info_v2(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.fee, BigInt::from(123));
        assert!(info.reference_inputs.is_empty());
        assert!(info.redeemers.is_empty()); // deferred to part 3g
    }

    #[test]
    fn v2_resolves_reference_inputs() {
        let ref_in = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let mut body = minimal_body(1);
        body.reference_inputs = vec![ref_in.clone()];
        let tx = build_tx(body);
        let resolved = vec![(ref_in, enterprise_output(1), vec![])];
        let info = populate_tx_info_v2(&tx, &resolved, &slot_cfg()).unwrap();
        assert_eq!(info.reference_inputs.len(), 1);
    }

    #[test]
    fn v2_surfaces_missing_input() {
        let input = TransactionInput {
            transaction_id: h32(0x99),
            index: 0,
        };
        let mut body = minimal_body(1);
        body.inputs = vec![input];
        let tx = build_tx(body);
        let err = populate_tx_info_v2(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::UtxoDecode(_)));
    }

    #[test]
    fn v1_and_v2_share_signatories_and_datums() {
        // Pin: identical input → identical signatories + datums across V1/V2.
        let signer = {
            let mut bytes = [0u8; 32];
            bytes[..28].copy_from_slice(&[7u8; 28]);
            Hash::<32>(bytes)
        };
        let mut body = minimal_body(1);
        body.required_signers = vec![signer];
        let mut tx = build_tx(body);
        tx.witness_set.plutus_data = vec![dugite_primitives::transaction::PlutusData::Integer(
            BigInt::from(11),
        )];
        let v1 = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap();
        let v2 = populate_tx_info_v2(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(v1.signatories, v2.signatories);
        assert_eq!(v1.data, v2.data);
        assert_eq!(v1.fee, v2.fee);
        assert_eq!(v1.txid, v2.txid);
    }
}
