//! `dugite_primitives::Transaction` → `utxorpc.v1beta.cardano.Tx` mapping.
//!
//! Every `Tx` field is populated: inputs, outputs, fee, mint,
//! withdrawals, validity, successful, hash, certificates, witnesses,
//! collateral (Plutus-redeemer chain), proposals, and votes — the last
//! four via the cross-cutting `cert` / `governance` / `script` /
//! `plutus_data` mapping modules.

use crate::map::common::{coin_bigint, hash_bytes, signed_bigint};
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionInput, TransactionOutput,
};
use dugite_primitives::value::Value;

/// Map one [`Transaction`] to its [`pb::Tx`] protobuf shape.
///
/// Every `Tx` field with a single-source-of-truth dugite type is
/// populated: inputs / outputs / certificates / withdrawals / mint /
/// reference_inputs / witnesses (vkey + scripts + plutus_data) /
/// collateral / fee / validity / successful / auxiliary (metadata +
/// scripts) / hash / proposals / votes. `TxInput.as_output` (needs a
/// ledger lookup — see `tx_input_to_proto`) and `TxOutput.script`
/// (reference scripts on outputs) remain unmapped; both are documented
/// at their call sites, not silently dropped.
pub fn tx_to_proto(tx: &Transaction) -> pb::Tx {
    pb::Tx {
        inputs: tx.body.inputs.iter().map(tx_input_to_proto).collect(),
        outputs: tx.body.outputs.iter().map(tx_output_to_proto).collect(),
        certificates: tx
            .body
            .certificates
            .iter()
            .map(crate::map::cert::certificate_to_proto)
            .collect(),
        withdrawals: tx
            .body
            .withdrawals
            .iter()
            .map(|(addr, qty)| pb::Withdrawal {
                reward_account: addr.clone(),
                coin: Some(coin_bigint(qty.0)),
                // Redeemer linkage requires per-redeemer index ↔ withdrawal
                // mapping (rdmr-purpose = Reward at index N); deferred.
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
        witnesses: Some(witness_set_to_proto(tx)),
        collateral: collateral_to_proto(tx),
        fee: Some(coin_bigint(tx.body.fee.0)),
        validity: Some(pb::TxValidity {
            start: tx.body.validity_interval_start.map(|s| s.0).unwrap_or(0),
            ttl: tx.body.ttl.map(|s| s.0).unwrap_or(0),
        }),
        successful: tx.is_valid,
        auxiliary: aux_to_proto(tx),
        hash: hash_bytes(&tx.hash),
        proposals: tx
            .body
            .proposal_procedures
            .iter()
            .map(crate::map::governance::proposal_to_proto)
            .collect(),
        votes: crate::map::governance::votes_to_proto(&tx.body.voting_procedures),
    }
}

fn witness_set_to_proto(tx: &Transaction) -> pb::WitnessSet {
    let mut scripts: Vec<pb::Script> = Vec::new();
    for ns in &tx.witness_set.native_scripts {
        scripts.push(pb::Script {
            script: Some(pb::script::Script::Native(
                crate::map::script::native_script_to_proto(ns),
            )),
        });
    }
    for b in &tx.witness_set.plutus_v1_scripts {
        scripts.push(pb::Script {
            script: Some(pb::script::Script::PlutusV1(b.clone())),
        });
    }
    for b in &tx.witness_set.plutus_v2_scripts {
        scripts.push(pb::Script {
            script: Some(pb::script::Script::PlutusV2(b.clone())),
        });
    }
    for b in &tx.witness_set.plutus_v3_scripts {
        scripts.push(pb::Script {
            script: Some(pb::script::Script::PlutusV3(b.clone())),
        });
    }

    pb::WitnessSet {
        vkeywitness: tx
            .witness_set
            .vkey_witnesses
            .iter()
            .map(|w| pb::VKeyWitness {
                vkey: w.vkey.clone(),
                signature: w.signature.clone(),
            })
            .collect(),
        script: scripts,
        plutus_datums: tx
            .witness_set
            .plutus_data
            .iter()
            .map(crate::map::plutus_data::plutus_data_to_proto)
            .collect(),
        redeemers: tx
            .witness_set
            .redeemers
            .iter()
            .map(redeemer_to_proto)
            .collect(),
        bootstrap_witnesses: tx
            .witness_set
            .bootstrap_witnesses
            .iter()
            .map(|w| pb::BootstrapWitness {
                vkey: w.vkey.clone(),
                signature: w.signature.clone(),
                chain_code: w.chain_code.clone(),
                attributes: w.attributes.clone(),
            })
            .collect(),
    }
}

fn redeemer_to_proto(r: &dugite_primitives::transaction::Redeemer) -> pb::Redeemer {
    use dugite_primitives::transaction::RedeemerTag;
    let purpose = match r.tag {
        RedeemerTag::Spend => pb::RedeemerPurpose::Spend as i32,
        RedeemerTag::Mint => pb::RedeemerPurpose::Mint as i32,
        RedeemerTag::Cert => pb::RedeemerPurpose::Cert as i32,
        RedeemerTag::Reward => pb::RedeemerPurpose::Reward as i32,
        RedeemerTag::Vote => pb::RedeemerPurpose::Vote as i32,
        RedeemerTag::Propose => pb::RedeemerPurpose::Propose as i32,
        // Dijkstra-only Guarding tag — proto schema doesn't yet
        // expose a dedicated purpose; map to UNSPECIFIED so clients
        // can detect the unmapped case via a non-Plutus purpose.
        RedeemerTag::Guarding => pb::RedeemerPurpose::Unspecified as i32,
    };
    pb::Redeemer {
        purpose,
        payload: Some(crate::map::plutus_data::plutus_data_to_proto(&r.data)),
        index: r.index,
        ex_units: Some(pb::ExUnits {
            steps: r.ex_units.steps,
            memory: r.ex_units.mem,
        }),
        original_cbor: Vec::new(),
    }
}

fn tx_input_to_proto(input: &TransactionInput) -> pb::TxInput {
    pb::TxInput {
        tx_hash: hash_bytes(&input.transaction_id),
        output_index: input.index,
        // `as_output` resolves the spent UTxO content. Populating it
        // requires a ledger lookup at map time, which would couple this
        // pure mapping module to `LedgerContext` — not done; a future
        // caller with UTxO access on hand (e.g. `QueryService`) could
        // fill it in after the fact instead.
        as_output: None,
        // Per-input redeemer linkage needs index-matching against
        // `WitnessSet.redeemers` (`RedeemerTag::Spend` + the input's
        // position) — not done; `redeemer_to_proto` below already maps
        // the underlying `PlutusData`, this is purely the linking step.
        redeemer: None,
    }
}

pub fn tx_output_to_proto(out: &TransactionOutput) -> pb::TxOutput {
    pb::TxOutput {
        address: out.address.to_bytes(),
        coin: Some(coin_bigint(out.value.coin.0)),
        assets: assets_from_value(&out.value),
        datum: datum_to_proto(&out.datum),
        // `out.script_ref` (native / plutus_v1-v4) is not mapped to
        // `pb::Script` here yet, despite `crate::map::script` already
        // covering the same variants for witness-set scripts — not
        // done, tracked informally rather than under a stale milestone
        // name.
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
        // Hash-only datum: no parsed payload was ever received on the
        // wire, so there is nothing for `plutus_data::plutus_data_to_proto`
        // to map — `payload`/`original_cbor` stay unset (not a gap;
        // hash-only datums structurally have neither).
        OutputDatum::DatumHash(h) => Some(pb::Datum {
            hash: hash_bytes(h),
            payload: None,
            original_cbor: None,
        }),
        OutputDatum::InlineDatum { data, raw_cbor } => Some(pb::Datum {
            // The on-chain hash of an inline datum is `blake2b_256` of
            // its CBOR; computing it on the hot path would re-hash on
            // every map. Clients that need the hash can re-derive from
            // `original_cbor`.
            hash: Vec::new(),
            payload: Some(crate::map::plutus_data::plutus_data_to_proto(data)),
            original_cbor: raw_cbor.clone(),
        }),
    }
}

fn aux_to_proto(tx: &Transaction) -> Option<pb::AuxData> {
    // `auxiliary_data` carries the full parsed metadata + scripts (via
    // `crate::map::metadatum::aux_data_to_proto`); `auxiliary_data_hash`
    // alone (no parsed witness-set aux data available to this mapping
    // call) is downgraded to an empty `AuxData` so a client can at
    // least detect presence.
    match (&tx.auxiliary_data, &tx.body.auxiliary_data_hash) {
        (Some(aux), _) => Some(crate::map::metadatum::aux_data_to_proto(aux)),
        (None, Some(_)) => Some(pb::AuxData {
            metadata: Vec::new(),
            scripts: Vec::new(),
        }),
        (None, None) => None,
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
    fn tx_with_auxiliary_data_maps_metadata_via_metadatum_module() {
        // `aux_to_proto` previously always emitted an empty `AuxData`
        // (metadata: [], scripts: []) even when `tx.auxiliary_data` held
        // real metadata — `crate::map::metadatum::aux_data_to_proto`
        // existed, fully implemented, but was never called from here.
        let mut tx = minimal_tx([9u8; 32], 1);
        let mut metadata = BTreeMap::new();
        metadata.insert(42u64, TransactionMetadatum::Int(7));
        tx.auxiliary_data = Some(AuxiliaryData {
            metadata,
            native_scripts: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            raw_cbor: None,
        });
        let pb = tx_to_proto(&tx);
        let aux = pb
            .auxiliary
            .expect("auxiliary set when auxiliary_data present");
        assert_eq!(
            aux.metadata.len(),
            1,
            "metadata must be mapped, not dropped"
        );
        assert_eq!(aux.metadata[0].label, 42);
    }

    #[test]
    fn tx_with_only_auxiliary_data_hash_emits_empty_aux_data() {
        // No parsed `auxiliary_data` (e.g. mempool tx before the witness
        // set round-trips), but the body declares a hash — still signal
        // presence with an empty AuxData rather than None.
        let mut tx = minimal_tx([10u8; 32], 1);
        tx.body.auxiliary_data_hash = Some(dugite_primitives::hash::Hash32::from_bytes([1u8; 32]));
        let pb = tx_to_proto(&tx);
        let aux = pb
            .auxiliary
            .expect("auxiliary set when only the hash is known");
        assert!(aux.metadata.is_empty());
        assert!(aux.scripts.is_empty());
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
