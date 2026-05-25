//! `dugite_primitives::Transaction` → `utxorpc.v1beta.cardano.Tx` mapping.
//!
//! M1.B scope: every field of `Tx` that maps from a single concrete
//! dugite type — inputs, outputs, fee, mint, withdrawals, validity,
//! successful, auxiliary-data hash (in `auxiliary.metadata` as a stub
//! marker), and hash. M2 fills in `certificates`, `witnesses`,
//! `collateral` (Plutus-redeemer chain), `auxiliary.scripts`,
//! `proposals`, `votes` — they all need the cross-cutting cert /
//! governance / script / plutus_data / metadatum mapping modules.

use crate::map::common::{coin_bigint, hash_bytes, signed_bigint};
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionInput, TransactionOutput,
};
use dugite_primitives::value::Value;

/// Map one [`Transaction`] to its [`pb::Tx`] protobuf shape.
///
/// `cert / witnesses / collateral / auxiliary.scripts / proposals /
/// votes` are M2's mapping modules; until then they emit empty/default
/// values. `successful` reflects `Transaction::is_valid` so clients can
/// distinguish phase-2-failing txs without parsing the body themselves.
pub fn tx_to_proto(tx: &Transaction) -> pb::Tx {
    pb::Tx {
        inputs: tx.body.inputs.iter().map(tx_input_to_proto).collect(),
        outputs: tx.body.outputs.iter().map(tx_output_to_proto).collect(),
        certificates: Vec::new(), // M2
        withdrawals: tx
            .body
            .withdrawals
            .iter()
            .map(|(addr, qty)| pb::Withdrawal {
                reward_account: addr.clone(),
                coin: Some(coin_bigint(qty.0)),
                // Plutus-redeemer wiring lands in M2 once the redeemer
                // mapper covers WithdrawalPurpose.
                redeemer: None,
            })
            .collect(),
        mint: mint_to_proto(&tx.body.mint),
        reference_inputs: tx
            .body
            .reference_inputs
            .iter()
            .map(tx_input_to_proto)
            .collect(),
        witnesses: None, // M2
        collateral: collateral_to_proto(tx),
        fee: Some(coin_bigint(tx.body.fee.0)),
        validity: Some(pb::TxValidity {
            start: tx.body.validity_interval_start.map(|s| s.0).unwrap_or(0),
            ttl: tx.body.ttl.map(|s| s.0).unwrap_or(0),
        }),
        successful: tx.is_valid,
        auxiliary: aux_to_proto(tx),
        hash: hash_bytes(&tx.hash),
        proposals: Vec::new(), // M2
        votes: Vec::new(),     // M2
    }
}

fn tx_input_to_proto(input: &TransactionInput) -> pb::TxInput {
    pb::TxInput {
        tx_hash: hash_bytes(&input.transaction_id),
        output_index: input.index,
        // `as_output` resolves the spent UTxO content. Populating it
        // requires a ledger lookup at map time, which couples the
        // mapper to LedgerContext. Deferred to M2 where the QueryService
        // path naturally has the UTxO available.
        as_output: None,
        // Redeemer mapping requires the M2 PlutusData mapper.
        redeemer: None,
    }
}

fn tx_output_to_proto(out: &TransactionOutput) -> pb::TxOutput {
    pb::TxOutput {
        address: out.address.to_bytes(),
        coin: Some(coin_bigint(out.value.coin.0)),
        assets: assets_from_value(&out.value),
        datum: datum_to_proto(&out.datum),
        // Reference scripts attached to outputs need the M2 Script
        // mapper (native vs plutus_v1/v2/v3).
        script: None,
        // Pass-through of the verbatim CBOR if the decoder retained it.
        // Clients verifying datum-hash / address bytes can trust this.
        original_cbor: out.raw_cbor.clone(),
    }
}

fn assets_from_value(value: &Value) -> Vec<pb::Multiasset> {
    value
        .multi_asset
        .iter()
        .map(|(policy, assets)| pb::Multiasset {
            policy_id: hash_bytes(policy),
            assets: assets
                .iter()
                .map(|(name, qty)| pb::Asset {
                    name: name.0.clone(),
                    quantity: Some(coin_bigint(*qty)),
                    // `output_coin` is the asset-balance projection of the
                    // wider TxOutput value; the generated proto includes
                    // an `output_coin` field only on later spec revisions.
                    // Skip for now — populated at the v1beta v0.19.2 level
                    // produces no extra field.
                })
                .collect(),
        })
        .collect()
}

fn mint_to_proto(
    mint: &std::collections::BTreeMap<
        dugite_primitives::hash::PolicyId,
        std::collections::BTreeMap<dugite_primitives::value::AssetName, i64>,
    >,
) -> Vec<pb::Multiasset> {
    mint.iter()
        .map(|(policy, assets)| pb::Multiasset {
            policy_id: hash_bytes(policy),
            assets: assets
                .iter()
                .map(|(name, qty)| pb::Asset {
                    name: name.0.clone(),
                    quantity: Some(signed_bigint(*qty)),
                })
                .collect(),
        })
        .collect()
}

fn datum_to_proto(datum: &OutputDatum) -> Option<pb::Datum> {
    match datum {
        OutputDatum::None => None,
        OutputDatum::DatumHash(h) => Some(pb::Datum {
            hash: hash_bytes(h),
            // Datum payload + original_cbor land in M2's PlutusData
            // mapper (inline datums carry a parsed payload; hash-only
            // datums have neither).
            payload: None,
            original_cbor: None,
        }),
        OutputDatum::InlineDatum { raw_cbor, .. } => Some(pb::Datum {
            // The on-chain hash of an inline datum is `blake2b_256` of
            // its CBOR; computing it on the hot path would re-hash on
            // every map. Clients that need the hash can re-derive from
            // `original_cbor`.
            hash: Vec::new(),
            payload: None, // M2 PlutusData mapper
            original_cbor: raw_cbor.clone(),
        }),
    }
}

fn aux_to_proto(tx: &Transaction) -> Option<pb::AuxData> {
    // M1.B: empty AuxData when there is no auxiliary-data hash. With one,
    // emit an empty AuxData so downstream clients can detect presence —
    // M2 fills `metadata` + `scripts` once the metadatum mapper lands.
    if tx.body.auxiliary_data_hash.is_none() && tx.auxiliary_data.is_none() {
        None
    } else {
        Some(pb::AuxData {
            metadata: Vec::new(),
            scripts: Vec::new(),
        })
    }
}

fn collateral_to_proto(tx: &Transaction) -> Option<pb::Collateral> {
    let has_any = !tx.body.collateral.is_empty()
        || tx.body.collateral_return.is_some()
        || tx.body.total_collateral.is_some();
    if !has_any {
        return None;
    }
    Some(pb::Collateral {
        collateral: tx.body.collateral.iter().map(tx_input_to_proto).collect(),
        collateral_return: tx.body.collateral_return.as_ref().map(tx_output_to_proto),
        total_collateral: tx
            .body
            .total_collateral
            .map(|coin| coin_bigint(coin.0))
            .map(Some)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address, ByronAddress};
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::transaction::*;
    use dugite_primitives::value::Lovelace;
    use std::collections::BTreeMap;

    fn empty_tx_body() -> TransactionBody {
        TransactionBody {
            inputs: vec![],
            outputs: vec![],
            fee: Lovelace(0),
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
            direct_deposits: BTreeMap::new(),
            guards: Vec::new(),
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

    fn minimal_tx(hash: [u8; 32], fee: u64) -> Transaction {
        let mut body = empty_tx_body();
        body.fee = Lovelace(fee);
        Transaction {
            era: dugite_primitives::era::Era::Conway,
            hash: Hash32::from_bytes(hash),
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    #[test]
    fn tx_hash_and_fee_round_trip() {
        let tx = minimal_tx([7u8; 32], 1_500_000);
        let pb = tx_to_proto(&tx);
        assert_eq!(pb.hash, vec![7u8; 32]);
        assert!(pb.successful);
        let fee = pb.fee.expect("fee set");
        match fee.big_int.unwrap() {
            pb::big_int::BigInt::Int(v) => assert_eq!(v, 1_500_000),
            other => panic!("unexpected BigInt: {other:?}"),
        }
        // No optional sections populated.
        assert!(pb.inputs.is_empty());
        assert!(pb.outputs.is_empty());
        assert!(pb.mint.is_empty());
        assert!(pb.withdrawals.is_empty());
        assert!(pb.certificates.is_empty());
        assert!(pb.collateral.is_none());
        assert!(pb.auxiliary.is_none());
        assert_eq!(pb.validity.unwrap().ttl, 0);
    }

    #[test]
    fn tx_with_invalid_flag_maps_successful_false() {
        let mut tx = minimal_tx([1u8; 32], 0);
        tx.is_valid = false;
        let pb = tx_to_proto(&tx);
        assert!(!pb.successful);
    }

    #[test]
    fn tx_inputs_and_outputs_round_trip() {
        let mut tx = minimal_tx([0u8; 32], 200_000);
        tx.body.inputs.push(TransactionInput {
            transaction_id: Hash32::from_bytes([9u8; 32]),
            index: 3,
        });
        tx.body.outputs.push(TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![1, 2, 3, 4],
            }),
            value: Value::lovelace(7_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        });

        let pb = tx_to_proto(&tx);
        assert_eq!(pb.inputs.len(), 1);
        assert_eq!(pb.inputs[0].tx_hash, vec![9u8; 32]);
        assert_eq!(pb.inputs[0].output_index, 3);

        assert_eq!(pb.outputs.len(), 1);
        let out = &pb.outputs[0];
        assert_eq!(
            out.address,
            Address::Byron(ByronAddress {
                payload: vec![1, 2, 3, 4],
            })
            .to_bytes()
        );
        match out.coin.as_ref().unwrap().big_int.as_ref().unwrap() {
            pb::big_int::BigInt::Int(v) => assert_eq!(*v, 7_000_000),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(out.assets.is_empty());
        assert!(out.datum.is_none());
    }

    #[test]
    fn tx_with_mint_burn_is_signed_bigint() {
        let mut tx = minimal_tx([0u8; 32], 0);
        let policy = dugite_primitives::hash::PolicyId::from_bytes([0xAB; 28]);
        let name =
            dugite_primitives::value::AssetName::new(vec![0xC0, 0xFF, 0xEE]).expect("asset name");
        let mut assets = BTreeMap::new();
        assets.insert(name, -1_000_000_i64);
        tx.body.mint.insert(policy, assets);

        let pb = tx_to_proto(&tx);
        assert_eq!(pb.mint.len(), 1);
        let ma = &pb.mint[0];
        assert_eq!(ma.policy_id, vec![0xAB; 28]);
        match ma.assets[0]
            .quantity
            .as_ref()
            .unwrap()
            .big_int
            .as_ref()
            .unwrap()
        {
            pb::big_int::BigInt::Int(v) => assert_eq!(*v, -1_000_000),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tx_with_withdrawals_emits_correctly() {
        let mut tx = minimal_tx([0u8; 32], 0);
        tx.body
            .withdrawals
            .insert(vec![0xE0; 29], Lovelace(2_500_000));
        let pb = tx_to_proto(&tx);
        assert_eq!(pb.withdrawals.len(), 1);
        assert_eq!(pb.withdrawals[0].reward_account, vec![0xE0; 29]);
        match pb.withdrawals[0]
            .coin
            .as_ref()
            .unwrap()
            .big_int
            .as_ref()
            .unwrap()
        {
            pb::big_int::BigInt::Int(v) => assert_eq!(*v, 2_500_000),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
