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
//! - certificates → `Vec<TxCert>` (all Conway cert variants)
//! - votes / proposal_procedures (governance fields)
//! - redeemers map (all six purpose types: Spend/Mint/Cert/Reward/Vote/Propose)
//!
//! ## Redeemers Map Ordering
//!
//! Haskell reference: `Cardano.Ledger.Babbage.TxInfo.transTxRedeemers`:
//! ```haskell
//! transTxRedeemers proxy pv tx =
//!   PV2.unsafeFromList <$> mapM (transRedeemerPtr …)
//!     (Map.toList $ tx ^. witsTxL . rdmrsTxWitsL . unRedeemersL)
//! ```
//!
//! The `Redeemers` witness map is `Map (ConwayPlutusPurpose AsIx era) (Data, ExUnits)`.
//! `ConwayPlutusPurpose AsIx` derives `Ord` from constructor order:
//!   0=ConwaySpending, 1=ConwayMinting, 2=ConwayCertifying,
//!   3=ConwayRewarding, 4=ConwayVoting, 5=ConwayProposing.
//! Within each constructor, the key is a `Word32` index (numeric ascending).
//! `Map.toList` iterates in this order. `PV2.unsafeFromList` preserves it.
//!
//! In dugite we replicate this by sorting `ResolvedRedeemer`s by
//! `(purpose_rank, redeemer.index)` before building the Vec, where
//! `purpose_rank` mirrors the Haskell constructor order above.

use crate::data::Data;
use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::populate_gov::{
    certificate_to_plutus, certificates_to_plutus, proposals_to_plutus, voting_procedures_to_plutus,
};
use crate::redeemer_resolve::resolve_redeemers;
use crate::script_context::{Credential, ScriptPurpose, TxInfoV3};
use crate::tx_info_populate::{
    credential_to_plutus, datums_to_plutus, inputs_to_txininfos, mint_to_plutus, output_to_plutus,
    plutus_data_to_data, required_signers_to_plutus_padded, sort_inputs, tx_hash_to_array,
    valid_range_to_posix,
};
use dugite_primitives::transaction::{
    RedeemerTag, Transaction as PrimTransaction, TransactionInput as PrimTxIn,
    TransactionOutput as PrimTxOut,
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
    // reward-account blobs in raw-blob lex order (header high-nibble 0xE=key
    // before 0xF=script ⇒ Key < Script). The ledger `Map RewardAccount Coin`
    // orders by `(Network, Credential Script<Key, hash)` ⇒ Script < Key, so we
    // re-order via `ledger_ordered_withdrawals` to match `Map.toList` byte-exact.
    let wdrl = {
        let ordered = crate::tx_info_populate::ledger_ordered_withdrawals(&tx.body.withdrawals)?;
        let mut out: Vec<(Credential, BigInt)> = Vec::with_capacity(ordered.len());
        for (stake, amount) in ordered {
            out.push((credential_to_plutus(&stake), BigInt::from(amount.0)));
        }
        out
    };

    // Populate the V3 txInfoRedeemers map.
    //
    // Haskell: `transTxRedeemers` builds `Map.toList` of the witness-set
    // `Redeemers` map (keyed by `ConwayPlutusPurpose AsIx`). The `Ord` for
    // `ConwayPlutusPurpose AsIx` is derived from constructor order:
    //   0=Spending, 1=Minting, 2=Certifying, 3=Rewarding, 4=Voting, 5=Proposing
    // then by `Word32` index within each constructor (ascending numeric).
    //
    // In dugite we resolve all redeemers, then sort by `(purpose_rank, index)`
    // to match `Map.toList` order before constructing the map Vec.
    let redeemers: Vec<(ScriptPurpose, Data)> = if tx.witness_set.redeemers.is_empty() {
        Vec::new()
    } else {
        let mut resolved = resolve_redeemers(tx, resolved)?;
        // Sort by (purpose_rank, redeemer_index) to replicate Haskell's
        // `Map (ConwayPlutusPurpose AsIx) _` toList order.
        resolved.sort_by_key(|rr| (purpose_rank(&rr.tag), rr.index));
        resolved
            .into_iter()
            .map(|rr| {
                // Use V3 encoding for the map key: Spending uses bare txid.
                // Non-Spending purposes are identical between V1/V2/V3 EXCEPT
                // `Certifying` (#833): the cert payload must follow the
                // CONTEXT language's schema (Conway `TxCert`), not the
                // witnessing script's — `rr.purpose` was baked once at
                // redeemer-resolve time using whichever script executes
                // this redeemer, which diverges on a mixed-language tx
                // (e.g. a V1/V2-witnessed cert redeemer reused inside a V3
                // context's redeemers map). Re-encode from the tx body
                // certificate here.
                let purpose = match rr.purpose {
                    ScriptPurpose::Certifying(i, _) => {
                        let cert =
                            tx.body.certificates.get(rr.index as usize).ok_or_else(|| {
                                PhaseTwoError::Internal(format!(
                                    "populate_tx_info_v3: certifying redeemer references \
                                 certificates[{idx}] but tx has {n}",
                                    idx = rr.index,
                                    n = tx.body.certificates.len()
                                ))
                            })?;
                        ScriptPurpose::Certifying(i, certificate_to_plutus(cert)?)
                    }
                    other => other,
                };
                Ok((purpose, plutus_data_to_data(&rr.redeemer_data)))
            })
            .collect::<Result<Vec<_>, PhaseTwoError>>()?
    };
    tracing::trace!(
        count = redeemers.len(),
        "populate_tx_info_v3: redeemers resolved"
    );

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
        redeemers,
        datums,
        txid: tx_hash_to_array(&tx.hash),
        votes,
        proposal_procedures,
        current_treasury: tx.body.treasury_value.map(|v| BigInt::from(v.0)),
        treasury_donation: tx.body.donation.map(|v| BigInt::from(v.0)),
    })
}

/// Map a [`RedeemerTag`] to the sort rank matching `ConwayPlutusPurpose`'s
/// derived `Ord` (constructor order in the data type definition):
///
/// ```text
/// ConwaySpending   = 0  (Spending redeemer)
/// ConwayMinting    = 1  (Mint redeemer)
/// ConwayCertifying = 2  (Cert redeemer)
/// ConwayRewarding  = 3  (Reward redeemer)
/// ConwayVoting     = 4  (Vote redeemer)
/// ConwayProposing  = 5  (Propose redeemer)
/// ```
///
/// Haskell source: `Cardano.Ledger.Conway.Scripts`
/// (`data ConwayPlutusPurpose f era = ConwaySpending | ConwayMinting | …
///   deriving (Eq, Ord, …)`).
/// The `Redeemers` witness map is `Map (ConwayPlutusPurpose AsIx) _`;
/// `Map.toList` iterates in this derived `Ord` order.
pub(crate) fn purpose_rank(tag: &RedeemerTag) -> u8 {
    match tag {
        RedeemerTag::Spend => 0,
        RedeemerTag::Mint => 1,
        RedeemerTag::Cert => 2,
        RedeemerTag::Reward => 3,
        RedeemerTag::Vote => 4,
        RedeemerTag::Propose => 5,
        // Dijkstra Guarding — not a Conway-era purpose; assign a high rank so
        // it sorts after the standard 6 and doesn't corrupt the canonical map
        // ordering. In practice, V3 scripts on Conway (PV9-PV11) never see
        // Guarding redeemers; this is a no-op placeholder for PV12+ Dijkstra.
        RedeemerTag::Guarding => 6,
    }
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
        // Tx has no redeemers in its witness set → map is empty.
        assert!(info.redeemers.is_empty());
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

    // ────────────────────────────────────────────────────────────
    // Redeemers map population tests (task-13f)
    //
    // Cross-validated against Haskell:
    //   transTxRedeemers (Babbage.TxInfo) + ConwayPlutusPurpose Ord
    //   PlutusLedgerApi.V3.Contexts: ScriptPurpose makeIsDataSchemaIndexed
    //     [('Minting,0), ('Spending,1), ('Rewarding,2), ('Certifying,3),
    //      ('Voting,4), ('Proposing,5)]
    //   Map.toList order: ConwayPlutusPurpose AsIx Ord =
    //     Spending(0) < Minting(1) < Certifying(2) < Rewarding(3) < Voting(4) < Proposing(5)
    // ────────────────────────────────────────────────────────────

    /// Helper: build a V3 script hash deterministically.
    fn v3_script_hash(seed: u8) -> ([u8; 28], Vec<u8>) {
        let bytes = vec![seed; 4];
        let mut buf = vec![3u8];
        buf.extend_from_slice(&bytes);
        let hash = dugite_primitives::hash::blake2b_224(&buf).0;
        (hash, bytes)
    }

    /// Mint + Spend redeemers: Haskell Ord puts Spending before Minting in the
    /// `ConwayPlutusPurpose` constructor order (Spending=0, Minting=1), so in
    /// `txInfoRedeemers` Spending comes first even if the on-wire redeemer array
    /// listed Mint before Spend.
    ///
    /// The Plutus `ScriptPurpose` Data encoding for the map KEYS uses different
    /// tag assignments: Minting=0, Spending=1. The MAP ORDERING however is driven
    /// by the internal `ConwayPlutusPurpose AsIx` Ord, not the Data key tag.
    #[test]
    fn populate_tx_info_v3_redeemers_spend_then_mint_ordering() {
        use dugite_primitives::credentials::Credential as PrimCred;
        use dugite_primitives::hash::Hash;
        use dugite_primitives::transaction::{ExUnits, PlutusData, Redeemer, RedeemerTag};
        use dugite_primitives::value::AssetName;
        use std::collections::BTreeMap;

        let (spend_script_hash, spend_bytes) = v3_script_hash(0xaa);
        let (mint_script_hash, mint_bytes) = v3_script_hash(0xbb);

        // Build an input locked by spend_script_hash.
        let input = TransactionInput {
            transaction_id: h32(0xcc),
            index: 0,
        };
        let spent_out = TransactionOutput {
            address: PrimAddress::Enterprise(dugite_primitives::address::EnterpriseAddress {
                network: dugite_primitives::network::NetworkId::Testnet,
                payment: PrimCred::Script(Hash::<28>(spend_script_hash)),
            }),
            value: Value::lovelace(5_000_000),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let mut body = minimal_tx_body(vec![input.clone()], vec![], 200_000);
        // Add a mint for the mint policy.
        let mut policy_assets = BTreeMap::new();
        policy_assets.insert(AssetName::new(b"T".to_vec()).unwrap(), 1i64);
        body.mint
            .insert(Hash::<28>(mint_script_hash), policy_assets);

        let mut ws = empty_witness_set();
        ws.plutus_v3_scripts = vec![spend_bytes, mint_bytes];
        // On-wire: Mint redeemer first (index 0 for policy), Spend second.
        // The canonical sorted order should be Spend first, then Mint.
        ws.redeemers = vec![
            Redeemer {
                tag: RedeemerTag::Mint,
                index: 0,
                data: PlutusData::Integer(BigInt::from(42i64)),
                ex_units: ExUnits {
                    mem: 100,
                    steps: 100,
                },
            },
            Redeemer {
                tag: RedeemerTag::Spend,
                index: 0,
                data: PlutusData::Integer(BigInt::from(7i64)),
                ex_units: ExUnits {
                    mem: 200,
                    steps: 200,
                },
            },
        ];
        let tx = build_tx(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let info = populate_tx_info_v3(&tx, &resolved, &slot_cfg()).unwrap();

        // txInfoRedeemers must have 2 entries.
        assert_eq!(info.redeemers.len(), 2, "must have 2 redeemers");

        // Entry 0 must be Spending (ConwayPlutusPurpose rank 0 < Minting rank 1).
        // ScriptPurpose::Spending = Constr 1 [TxOutRef]
        // (makeIsDataSchemaIndexed ''ScriptPurpose [('Spending, 1)])
        assert!(
            matches!(
                &info.redeemers[0].0,
                crate::script_context::ScriptPurpose::Spending(_)
            ),
            "first redeemer must be Spending (ConwayPlutusPurpose Ord rank 0); got {:?}",
            info.redeemers[0].0
        );
        // Entry 1 must be Minting.
        assert!(
            matches!(
                &info.redeemers[1].0,
                crate::script_context::ScriptPurpose::Minting(_)
            ),
            "second redeemer must be Minting (ConwayPlutusPurpose Ord rank 1); got {:?}",
            info.redeemers[1].0
        );

        // The redeemer Data values must correspond to the respective redeemer data.
        // Spending redeemer data = PlutusData::Integer(7)
        assert_eq!(
            info.redeemers[0].1,
            crate::data::Data::I(BigInt::from(7i64)),
            "Spending redeemer data must be I(7)"
        );
        // Minting redeemer data = PlutusData::Integer(42)
        assert_eq!(
            info.redeemers[1].1,
            crate::data::Data::I(BigInt::from(42i64)),
            "Minting redeemer data must be I(42)"
        );

        // The Spending ScriptPurpose key must use the V3 BARE-txid TxOutRef form
        // when serialized via to_data_v3().
        // ScriptPurpose::Spending.to_data_v3() = Constr 1 [Constr 0 [B32, I idx]]
        //   where B32 is bare bytes (NOT the V1/V2 double-wrapped Constr 0 [B32]).
        // Haskell: PlutusLedgerApi.V3.Contexts — TxId newtype deriving ToData from
        //   BuiltinByteString → bare B(32) in the TxOutRef payload.
        let spend_data = info.redeemers[0].0.to_data_v3();
        let Data::Constr(1, ref spend_fields) = spend_data else {
            panic!("Spending ScriptPurpose must be Constr 1; got {spend_data:?}");
        };
        // TxOutRef = Constr 0 [B32 (bare), I idx]  — V3 bare-txid form
        let Data::Constr(0, ref txoutref_fields) = spend_fields[0] else {
            panic!("TxOutRef must be Constr 0; got {:?}", spend_fields[0]);
        };
        assert_eq!(txoutref_fields.len(), 2);
        // V3: txid must be BARE B(32), NOT Constr 0 [B(32)]
        assert!(
            matches!(&txoutref_fields[0], Data::B(b) if b.len() == 32),
            "V3 Spending TxOutRef txid must be bare B(32); got {:?}",
            txoutref_fields[0]
        );
        // Must NOT be the double-wrapped V1/V2 Constr 0 [B32] form
        assert!(
            !matches!(&txoutref_fields[0], Data::Constr(0, _)),
            "V3 TxOutRef txid must NOT be Constr-wrapped (that is V1/V2 form)"
        );
    }

    /// Voting redeemer: Constr 4 [Voter] in the ScriptPurpose Data encoding.
    /// Voter encoding: CommitteeVoter=0, DRepVoter=1, StakePoolVoter=2
    /// (from makeIsDataSchemaIndexed ''Voter in PlutusLedgerApi.V3.Contexts).
    #[test]
    fn populate_tx_info_v3_voting_redeemer_encodes_correctly() {
        use dugite_primitives::credentials::Credential as PrimCred;
        use dugite_primitives::hash::Hash;
        use dugite_primitives::transaction::{
            ExUnits, GovActionId, PlutusData, Redeemer, RedeemerTag, Vote, Voter, VotingProcedure,
        };

        let (script_hash, script_bytes) = v3_script_hash(0xdd);

        let mut body = minimal_tx_body(vec![], vec![], 1);
        let mut inner = std::collections::BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0x10),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
        // DRep voter with script credential — this is the only kind that can
        // dispatch a Plutus Voting redeemer.
        body.voting_procedures.insert(
            Voter::DRep(PrimCred::Script(Hash::<28>(script_hash))),
            inner,
        );

        let mut ws = empty_witness_set();
        ws.plutus_v3_scripts = vec![script_bytes];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Vote,
            index: 0,
            data: PlutusData::Integer(BigInt::from(99i64)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let info = populate_tx_info_v3(&tx, &[], &slot_cfg()).unwrap();

        assert_eq!(info.redeemers.len(), 1, "must have 1 voting redeemer");
        // ScriptPurpose::Voting = Constr 4 [Voter]
        // makeIsDataSchemaIndexed ''ScriptPurpose [('Voting, 4)]
        // Use to_data_v3() to reflect actual serialization path (same for Voting).
        let purpose_data = info.redeemers[0].0.to_data_v3();
        let Data::Constr(4, ref voter_fields) = purpose_data else {
            panic!("Voting ScriptPurpose must be Constr 4; got {purpose_data:?}");
        };
        assert_eq!(voter_fields.len(), 1);
        // Voter::DRepVoter(DRepCredential(Script(...))) = Constr 1 [Constr 1 [B28]]
        // DRepCredential is newtype deriving ToData from Credential → bare Credential
        // ScriptCredential = Constr 1 [B28]
        let Data::Constr(1, ref drep_fields) = voter_fields[0] else {
            panic!("DRepVoter must be Constr 1; got {:?}", voter_fields[0]);
        };
        assert_eq!(drep_fields.len(), 1);
        // DRepCredential passes through to Credential (Constr 1 for Script)
        assert!(
            matches!(&drep_fields[0], Data::Constr(1, inner) if inner.len() == 1),
            "DRepCredential(ScriptCredential) must be Constr 1 [B28]; got {:?}",
            drep_fields[0]
        );
        // The redeemer Data value
        assert_eq!(
            info.redeemers[0].1,
            Data::I(BigInt::from(99i64)),
            "Voting redeemer data must be I(99)"
        );
    }
}
