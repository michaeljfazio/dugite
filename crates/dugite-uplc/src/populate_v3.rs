//! V3 `TxInfo` builder — assembles a [`TxInfoV3`] from a decoded
//! [`Transaction`] plus the resolved-UTxO map.
//!
//! ## Scope
//!
//! This module lands the structural V3 builder using the translation
//! helpers from [`crate::tx_info_populate`]. Fields populated here:
//!
//! - inputs / reference_inputs (resolved against the UTxO map)
//! - outputs
//! - fee
//! - mint
//! - valid_range
//! - signatories (28-byte unpadded)
//! - datums (from witness set, hashed)
//! - txid (= tx.hash)
//! - current_treasury / treasury_donation
//!
//! Fields **deferred to later UPLC-9 parts**:
//!
//! - certificates → `Vec<TxCert>` ─ UPLC-9 part 3e
//! - votes / proposal_procedures ─ UPLC-9 part 3e
//! - redeemers map ─ UPLC-9 part 3f
//!
//! Those remain `Vec::new()` / `None` here so the V3 builder is
//! reviewable in isolation. Plutus scripts that don't consult those
//! fields (the common case for fresh deployments) already see a
//! complete-enough context; scripts that do will progressively
//! pick up the missing pieces as the follow-on PRs land.

use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::populate_gov::{
    certificates_to_plutus, proposals_to_plutus, voting_procedures_to_plutus,
};
use crate::script_context::{Credential, TxInfoV3};
use crate::tx_info_populate::{
    credential_to_plutus, datums_to_plutus, inputs_to_txininfos, mint_to_plutus, output_to_plutus,
    required_signers_to_plutus_padded, sort_inputs, tx_hash_to_array, valid_range_to_posix,
};
use dugite_primitives::address::Address as PrimAddress;
use dugite_primitives::transaction::{
    Transaction as PrimTransaction, TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut,
};
use num_bigint::BigInt;

/// Build a [`TxInfoV3`] from a decoded transaction + resolved-UTxO
/// triples + the network's slot config.
///
/// `resolved` must contain every input referenced by `tx.body.inputs`
/// and `tx.body.reference_inputs` (the same set
/// [`crate::phase_two::decode_phase_two_inputs`] hands back). Missing
/// entries surface as [`PhaseTwoError::UtxoDecode`] from the input
/// resolver.
pub fn populate_tx_info_v3(
    tx: &PrimTransaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    slot_config: &SlotConfig,
) -> Result<TxInfoV3, PhaseTwoError> {
    // Both `inputsTxBodyL` and `refInputsTxBodyL` are `Set TxIn` in
    // cardano-ledger — presented in ascending `Ord TxIn` order.
    let sorted = sort_inputs(&tx.body.inputs);
    let inputs = inputs_to_txininfos(&sorted, resolved)?;
    let sorted_refs = sort_inputs(&tx.body.reference_inputs);
    let reference_inputs = inputs_to_txininfos(&sorted_refs, resolved)?;
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
    let datums = datums_to_plutus(
        &tx.witness_set.plutus_data,
        tx.witness_set.raw_plutus_data_cbor.as_deref(),
    )?;
    let votes = voting_procedures_to_plutus(&tx.body.voting_procedures);
    let proposal_procedures = proposals_to_plutus(&tx.body.proposal_procedures)?;
    // Translate certificates into the V3 TxCert list (field 5 of txInfoV3).
    let certs = certificates_to_plutus(&tx.body.certificates)?;
    tracing::trace!(count = certs.len(), "populate_tx_info_v3: certs translated");
    // Translate withdrawals into the V3 wdrl map (field 6 of txInfoV3).
    // V3 key type is `Credential` DIRECTLY — NOT wrapped in StakingHash as V1/V2 did.
    // `tx.body.withdrawals` is a BTreeMap<Vec<u8>, Lovelace> keyed by 29-byte
    // reward-account blobs in lex order (canonical CBOR map order). Iteration
    // over BTreeMap is in key order, so the resulting Vec preserves that order.
    let wdrl = {
        let mut out: Vec<(Credential, BigInt)> = Vec::with_capacity(tx.body.withdrawals.len());
        for (reward_account, amount) in &tx.body.withdrawals {
            let addr = PrimAddress::from_bytes(reward_account).map_err(|e| {
                PhaseTwoError::Internal(format!("populate_tx_info_v3: wdrl reward_account: {e}"))
            })?;
            let stake = match addr {
                PrimAddress::Reward(r) => r.stake,
                other => {
                    return Err(PhaseTwoError::Internal(format!(
                        "populate_tx_info_v3: wdrl expected Reward address, got {other:?}"
                    )));
                }
            };
            out.push((credential_to_plutus(&stake), BigInt::from(amount.0)));
        }
        out
    };

    Ok(TxInfoV3 {
        inputs,
        reference_inputs,
        outputs,
        fee: BigInt::from(tx.body.fee.0),
        mint,
        certs,
        wdrl,
        valid_range,
        signatories,
        // TODO(task-13f): redeemers map not yet populated — wired as empty Vec.
        redeemers: Vec::new(),
        datums,
        txid: tx_hash_to_array(&tx.hash),
        votes,
        proposal_procedures,
        current_treasury: tx.body.treasury_value.map(|v| BigInt::from(v.0)),
        treasury_donation: tx.body.donation.map(|v| BigInt::from(v.0)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_context::{Credential as PlCredential, OutputDatum as PlOutputDatum};
    use dugite_primitives::address::{Address as PrimAddress, EnterpriseAddress};
    use dugite_primitives::credentials::Credential as PrimCred;
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::time::SlotNo;
    use dugite_primitives::transaction::{
        OutputDatum as PrimOutputDatum, PlutusData as PrimPlutusData, Transaction, TransactionBody,
        TransactionInput, TransactionOutput, TransactionWitnessSet,
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

    fn minimal_tx_body(
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        fee: u64,
    ) -> TransactionBody {
        TransactionBody {
            inputs,
            outputs,
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
            direct_deposits: ::std::collections::BTreeMap::new(),
            guards: vec![],
        }
    }

    fn build_tx(body: TransactionBody, witness_set: TransactionWitnessSet) -> Transaction {
        Transaction {
            hash: h32(0xab),
            era: Era::Conway,
            body,
            witness_set,
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
            safe_zone_horizon_slot: None,
        }
    }

    // ────────────────────────────────────────────────────────────
    // Happy path
    // ────────────────────────────────────────────────────────────

    #[test]
    fn populate_tx_info_v3_minimal_tx_yields_empty_collections() {
        let tx = build_tx(
            minimal_tx_body(vec![], vec![], 170_000),
            empty_witness_set(),
        );
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.fee, BigInt::from(170_000));
        assert_eq!(info.txid, [0xab; 32]);
        assert!(info.inputs.is_empty());
        assert!(info.reference_inputs.is_empty());
        assert!(info.outputs.is_empty());
        assert!(info.certs.is_empty());
        assert!(info.wdrl.is_empty());
        assert!(info.datums.is_empty());
        assert!(info.redeemers.is_empty()); // TODO(task-13f): deferred
        assert!(info.votes.is_empty());
        assert!(info.proposal_procedures.is_empty());
        assert_eq!(info.current_treasury, None);
        assert_eq!(info.treasury_donation, None);
        assert_eq!(info.valid_range.lower, None);
        assert_eq!(info.valid_range.upper, None);
    }

    #[test]
    fn populate_tx_info_v3_resolves_inputs() {
        let input = TransactionInput {
            transaction_id: h32(0xcc),
            index: 7,
        };
        let body = minimal_tx_body(vec![input.clone()], vec![], 1);
        let tx = build_tx(body, empty_witness_set());
        let resolved = vec![(input.clone(), enterprise_output(500_000), vec![])];

        let info = populate_tx_info_v3(&tx, &resolved, &slot_cfg()).unwrap();
        assert_eq!(info.inputs.len(), 1);
        assert_eq!(info.inputs[0].out_ref.tx_id, [0xcc; 32]);
        assert_eq!(info.inputs[0].out_ref.idx, 7);
        assert_eq!(
            info.inputs[0].resolved.value.policies[0].1[0].1,
            BigInt::from(500_000)
        );
    }

    #[test]
    fn populate_tx_info_v3_resolves_reference_inputs_too() {
        let ref_in = TransactionInput {
            transaction_id: h32(0xdd),
            index: 1,
        };
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.reference_inputs = vec![ref_in.clone()];
        let tx = build_tx(body, empty_witness_set());
        let resolved = vec![(ref_in, enterprise_output(99), vec![])];

        let info = populate_tx_info_v3(&tx, &resolved, &slot_cfg()).unwrap();
        assert_eq!(info.reference_inputs.len(), 1);
        assert_eq!(info.reference_inputs[0].out_ref.tx_id, [0xdd; 32]);
    }

    #[test]
    fn populate_tx_info_v3_translates_outputs() {
        let body = minimal_tx_body(
            vec![],
            vec![enterprise_output(2_500_000), enterprise_output(1_000_000)],
            1,
        );
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.outputs.len(), 2);
        assert!(matches!(
            info.outputs[0].address.payment,
            PlCredential::PubKey(_)
        ));
        assert_eq!(info.outputs[0].datum, PlOutputDatum::None);
        assert_eq!(
            info.outputs[0].value.policies[0].1[0].1,
            BigInt::from(2_500_000)
        );
    }

    #[test]
    fn populate_tx_info_v3_translates_mint() {
        let mut policy_assets = BTreeMap::new();
        policy_assets.insert(
            dugite_primitives::value::AssetName::new(b"X".to_vec()).unwrap(),
            42i64,
        );
        let mut mint = BTreeMap::new();
        mint.insert(h28(0x11), policy_assets);
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.mint = mint;
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.mint.policies.len(), 1);
        assert_eq!(info.mint.policies[0].0, [0x11; 28]);
        assert_eq!(info.mint.policies[0].1[0].1, BigInt::from(42));
    }

    #[test]
    fn populate_tx_info_v3_translates_valid_range() {
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.validity_interval_start = Some(SlotNo(10));
        body.ttl = Some(SlotNo(20));
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.valid_range.lower, Some(1_666_656_010_000));
        assert_eq!(info.valid_range.upper, Some(1_666_656_020_000));
    }

    #[test]
    fn populate_tx_info_v3_unpads_required_signers_to_28_bytes() {
        // Required signer stored as Hash<32> with last 4 bytes zero.
        let padded = {
            let mut bytes = [0u8; 32];
            for (i, slot) in bytes.iter_mut().take(28).enumerate() {
                *slot = (i + 1) as u8;
            }
            Hash::<32>(bytes)
        };
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.required_signers = vec![padded];
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.signatories.len(), 1);
        let expected: [u8; 28] = std::array::from_fn(|i| (i + 1) as u8);
        assert_eq!(info.signatories[0], expected);
    }

    #[test]
    fn populate_tx_info_v3_hashes_witness_set_datums() {
        // Each datum surfaces with its blake2b_256 hash and the
        // translated `Data`.
        let mut witness_set = empty_witness_set();
        witness_set.plutus_data = vec![PrimPlutusData::Integer(BigInt::from(7))];
        let tx = build_tx(minimal_tx_body(vec![], vec![], 1), witness_set);
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.datums.len(), 1);
        let (h, d) = &info.datums[0];
        // Reconstruct hash independently for the pin.
        let manual_cbor = d.to_cbor().unwrap();
        let manual_hash = dugite_primitives::hash::blake2b_256(&manual_cbor).0;
        assert_eq!(h, &manual_hash);
    }

    #[test]
    fn populate_tx_info_v3_populates_treasury_fields_when_present() {
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.treasury_value = Some(Lovelace(1_000_000_000));
        body.donation = Some(Lovelace(50_000));
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.current_treasury, Some(BigInt::from(1_000_000_000)));
        assert_eq!(info.treasury_donation, Some(BigInt::from(50_000)));
    }

    // ────────────────────────────────────────────────────────────
    // Error propagation
    // ────────────────────────────────────────────────────────────

    // ────────────────────────────────────────────────────────────
    // certs + wdrl (task #13)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn populate_tx_info_v3_translates_stake_deregistration_cert() {
        use dugite_primitives::credentials::Credential as PrimCred;
        use dugite_primitives::transaction::Certificate;
        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.certificates = vec![Certificate::StakeDeregistration(PrimCred::VerificationKey(
            h28(0xcc),
        ))];
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();
        // Should have exactly 1 cert
        assert_eq!(info.certs.len(), 1);
        // TxCertUnRegStaking = Constr 1 [cred, Maybe Lovelace]
        let crate::script_context::TxCert(ref d) = info.certs[0];
        let crate::data::Data::Constr(tag, ref fields) = d else {
            panic!("cert must be Constr; got {d:?}");
        };
        assert_eq!(
            *tag, 1u64,
            "StakeDeregistration -> TxCertUnRegStaking (Constr 1)"
        );
        assert_eq!(fields.len(), 2, "must have credential + Maybe");
        // credential = PubKeyCredential([0xcc;28]) = Constr 0 [B28]
        assert!(
            matches!(&fields[0], crate::data::Data::Constr(0, inner) if inner.len() == 1),
            "credential must be Constr 0 [B28]; got {:?}",
            fields[0]
        );
        // deposit field = None (pre-Conway StakeDeregistration has no deposit) = Constr 1 []
        assert_eq!(
            fields[1],
            crate::data::Data::Constr(1, vec![]),
            "pre-Conway StakeDeregistration deposit must be None (Constr 1 [])"
        );
    }

    #[test]
    fn populate_tx_info_v3_translates_withdrawal_with_pubkey_credential() {
        use dugite_primitives::value::Lovelace;
        // Reward address blob: header 0xe0 (mainnet key-stake) || [0x77; 28]
        let mut reward_addr = vec![0xe0u8];
        reward_addr.extend_from_slice(&[0x77u8; 28]);

        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.withdrawals
            .insert(reward_addr.clone(), Lovelace(333_000));
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();

        assert_eq!(info.wdrl.len(), 1, "must have 1 withdrawal entry");
        let (cred, amt) = &info.wdrl[0];
        // V3 key: Credential directly, NOT StakingHash-wrapped
        assert!(
            matches!(cred, crate::script_context::Credential::PubKey(h) if *h == [0x77u8; 28]),
            "wdrl key must be PubKeyCredential([0x77;28]); got {cred:?}"
        );
        assert_eq!(*amt, BigInt::from(333_000u64));
    }

    #[test]
    fn populate_tx_info_v3_translates_withdrawal_with_script_credential() {
        use dugite_primitives::value::Lovelace;
        // Reward address blob: header 0xf0 (mainnet script-stake) || [0x88; 28]
        let mut reward_addr = vec![0xf0u8];
        reward_addr.extend_from_slice(&[0x88u8; 28]);

        let mut body = minimal_tx_body(vec![], vec![], 1);
        body.withdrawals
            .insert(reward_addr.clone(), Lovelace(111_000));
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();

        assert_eq!(info.wdrl.len(), 1);
        let (cred, amt) = &info.wdrl[0];
        assert!(
            matches!(cred, crate::script_context::Credential::Script(h) if *h == [0x88u8; 28]),
            "wdrl key must be ScriptCredential([0x88;28]); got {cred:?}"
        );
        assert_eq!(*amt, BigInt::from(111_000u64));
    }

    #[test]
    fn populate_tx_info_v3_wdrl_preserves_btreemap_key_order() {
        use dugite_primitives::value::Lovelace;
        // Two reward accounts with different key bytes — BTreeMap orders by
        // raw byte lex order of the 29-byte reward-account blob.
        let mut addr_lo = vec![0xe0u8]; // key-stake mainnet
        addr_lo.extend_from_slice(&[0x10u8; 28]); // smaller bytes
        let mut addr_hi = vec![0xe0u8];
        addr_hi.extend_from_slice(&[0x20u8; 28]); // larger bytes

        let mut body = minimal_tx_body(vec![], vec![], 1);
        // Insert in reverse order — BTreeMap will sort them
        body.withdrawals.insert(addr_hi.clone(), Lovelace(200_000));
        body.withdrawals.insert(addr_lo.clone(), Lovelace(100_000));
        let tx = build_tx(body, empty_witness_set());
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();

        assert_eq!(info.wdrl.len(), 2);
        // BTreeMap iterates in key order: addr_lo (0x10) < addr_hi (0x20)
        assert!(
            matches!(&info.wdrl[0].0, crate::script_context::Credential::PubKey(h) if *h == [0x10u8; 28]),
            "first wdrl entry must be 0x10 (smaller key); got {:?}",
            info.wdrl[0].0
        );
        assert_eq!(info.wdrl[0].1, BigInt::from(100_000u64));
        assert!(
            matches!(&info.wdrl[1].0, crate::script_context::Credential::PubKey(h) if *h == [0x20u8; 28]),
            "second wdrl entry must be 0x20 (larger key); got {:?}",
            info.wdrl[1].0
        );
        assert_eq!(info.wdrl[1].1, BigInt::from(200_000u64));
    }

    #[test]
    fn populate_tx_info_v3_surfaces_missing_input() {
        let input = TransactionInput {
            transaction_id: h32(0x99),
            index: 0,
        };
        let body = minimal_tx_body(vec![input], vec![], 1);
        let tx = build_tx(body, empty_witness_set());
        let err = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::UtxoDecode(_)));
    }

    #[test]
    fn populate_tx_info_v3_surfaces_byron_output_failure() {
        let mut body = minimal_tx_body(vec![], vec![], 1);
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
        let tx = build_tx(body, empty_witness_set());
        let err = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }
}
