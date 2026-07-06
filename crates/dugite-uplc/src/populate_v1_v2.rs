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

use crate::data::Data;
use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::populate_gov::{certificate_to_plutus_v1v2, certificates_to_plutus_v1v2};
use crate::populate_v3::purpose_rank;
use crate::redeemer_resolve::resolve_redeemers;
use crate::script_context::{ScriptPurpose, TxCert, TxInfoV1, TxInfoV2};
use crate::tx_info_populate::{
    check_v1_output_restrictions, datums_to_plutus, guard_conway_features_for_v1v2,
    inputs_to_txininfos, inputs_to_txininfos_v1, mint_to_plutus, output_to_plutus,
    outputs_to_plutus_v1, plutus_data_to_data, required_signers_to_plutus_padded, sort_inputs,
    tx_hash_to_array, valid_range_to_posix, withdrawals_to_plutus,
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
    // cardano-ledger's `inputsTxBodyL :: Set TxIn` is presented to Plutus
    // validators in ascending `Ord TxIn` order (TxId raw bytes, then TxIx).
    // The on-wire CBOR `array` has no ordering guarantee; sort here so
    // `txInfoInputs` is byte-exact with Haskell.
    //
    // #837 item 3: Alonzo's `txInfoIn`/`txInfoOut` silently DROP
    // Byron-addressed in/outputs (`mapMaybe`/`catMaybes` — a documented
    // Haskell "mistake" preserved for consensus compatibility); every
    // later era (which still runs V1 under a DIFFERENT, stricter
    // `EraPlutusTxInfo` instance) hard-errors with `ByronTxOutInContext`.
    // `inputs_to_txininfos_v1`/`outputs_to_plutus_v1` encode that era
    // dispatch; see their doc comments for the exact Haskell source.
    //
    // #818: the Conway field-presence gate + the V1-only inline-datum /
    // reference-script / reference-input restrictions run BEFORE anything
    // else is built, matching Haskell's `CollectErrors`-class hard
    // rejection of the whole tx.
    guard_conway_features_for_v1v2(tx)?;
    check_v1_output_restrictions(tx, resolved)?;
    let sorted = sort_inputs(&tx.body.inputs);
    let inputs = inputs_to_txininfos_v1(&sorted, resolved, tx.era)?;
    let outputs = outputs_to_plutus_v1(&tx.body.outputs, tx.era)?;
    let mint = mint_to_plutus(&tx.body.mint);
    let valid_range = valid_range_to_posix(
        tx.body.validity_interval_start.map(|s| s.0),
        tx.body.ttl.map(|s| s.0),
        slot_config,
    )?;
    let signatories = required_signers_to_plutus_padded(&tx.body.required_signers);
    let dcert: Vec<TxCert> = certificates_to_plutus_v1v2(&tx.body.certificates)?;
    let wdrl = withdrawals_to_plutus(&tx.body.withdrawals)?;
    let data = datums_to_plutus(
        &tx.witness_set.plutus_data,
        tx.witness_set.raw_plutus_data_cbor.as_deref(),
    )?;
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
    // #818: same Conway field-presence gate as V1 — Haskell runs this
    // unconditionally at the top of BOTH the V1 and V2 `toPlutusTxInfo`
    // instances. V2 is fully exempt from the V1-only inline-datum /
    // reference-script / reference-input restrictions in every era.
    guard_conway_features_for_v1v2(tx)?;
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
    let dcert: Vec<TxCert> = certificates_to_plutus_v1v2(&tx.body.certificates)?;
    let wdrl = withdrawals_to_plutus(&tx.body.withdrawals)?;
    let data = datums_to_plutus(
        &tx.witness_set.plutus_data,
        tx.witness_set.raw_plutus_data_cbor.as_deref(),
    )?;
    // V2 `txInfoRedeemers :: Map ScriptPurpose Redeemer` (added in PlutusV2).
    // Leaving it empty makes scripts that look up a redeemer by purpose (the
    // "forwarding"/multi-validator pattern) trace "Could not find redeemer" and
    // return Error. Resolve every witness redeemer to (ScriptPurpose, data) and
    // sort by `(purpose_rank, index)` to match Haskell `transTxRedeemers`'
    // `Map.toList` order (Babbage RdmrPtr Ord = Spend<Mint<Cert<Reward, identical
    // to the Conway AsIx ranks). The V2 `TxInfoV2::to_data` encodes the keys with
    // V2 conventions (wrapped TxId for Spending). (#22 Error-term class.)
    let redeemers: Vec<(ScriptPurpose, Data)> = if tx.witness_set.redeemers.is_empty() {
        Vec::new()
    } else {
        let mut rs = resolve_redeemers(tx, resolved)?;
        rs.sort_by_key(|rr| (purpose_rank(&rr.tag), rr.index));
        rs.into_iter()
            .map(|rr| {
                // #833: the `Certifying` cert payload embedded in this
                // context's `txInfoRedeemers` map must follow the CONTEXT
                // language's cert schema (V1/V2 `DCert`), not the
                // witnessing script's — `rr.purpose` was baked once at
                // redeemer-resolve time using whichever script executes
                // this redeemer, which diverges on a mixed-language tx
                // (e.g. a V3-witnessed cert redeemer reused inside a V2
                // spending script's context). Re-encode from the tx body
                // certificate here.
                let purpose = match rr.purpose {
                    ScriptPurpose::Certifying(i, _) => {
                        let cert =
                            tx.body.certificates.get(rr.index as usize).ok_or_else(|| {
                                PhaseTwoError::Internal(format!(
                                    "populate_tx_info_v2: certifying redeemer references \
                                 certificates[{idx}] but tx has {n}",
                                    idx = rr.index,
                                    n = tx.body.certificates.len()
                                ))
                            })?;
                        ScriptPurpose::Certifying(i, certificate_to_plutus_v1v2(cert)?)
                    }
                    other => other,
                };
                Ok((purpose, plutus_data_to_data(&rr.redeemer_data)))
            })
            .collect::<Result<Vec<_>, PhaseTwoError>>()?
    };
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
        redeemers,
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
            direct_deposits: ::std::collections::BTreeMap::new(),
            guards: Vec::new(),
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

    fn build_tx_with_witness(
        body: TransactionBody,
        witness_set: TransactionWitnessSet,
    ) -> Transaction {
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

    fn byron_output() -> TransactionOutput {
        TransactionOutput {
            address: PrimAddress::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![],
            }),
            value: Value::lovelace(1),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// #837 item 3: Alonzo's `EraPlutusTxInfo 'PlutusV1 AlonzoEra` instance
    /// silently DROPS Byron-addressed outputs from V1 `TxInfo`
    /// (`mapMaybe transTxOut` — a documented Haskell "mistake" preserved
    /// for consensus compatibility) rather than failing the translation.
    /// `build_tx` defaults to `Era::Alonzo`.
    #[test]
    fn v1_alonzo_drops_byron_outputs_instead_of_erroring() {
        let mut body = minimal_body(1);
        body.outputs = vec![byron_output(), enterprise_output(500_000)];
        let tx = build_tx(body);
        let info = populate_tx_info_v1(&tx, &[], &slot_cfg())
            .expect("Alonzo must silently drop the Byron output, not error");
        assert_eq!(
            info.outputs.len(),
            1,
            "Byron output must be dropped; only the enterprise output survives"
        );
    }

    /// Babbage+ (and Conway, which reuses Babbage's V1 instance unchanged)
    /// hard-errors on a Byron output with `ByronTxOutInContext` — the
    /// lenient drop-and-continue behavior is Alonzo-only.
    #[test]
    fn v1_babbage_still_hard_errors_on_byron_output() {
        let mut body = minimal_body(1);
        body.outputs = vec![byron_output()];
        let mut tx = build_tx(body);
        tx.era = Era::Babbage;
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// A resolved INPUT (not just a created output) with a Byron address
    /// must also be dropped in Alonzo, not just outputs.
    #[test]
    fn v1_alonzo_drops_byron_addressed_resolved_input() {
        let byron_input = TransactionInput {
            transaction_id: h32(0x11),
            index: 0,
        };
        let normal_input = TransactionInput {
            transaction_id: h32(0x22),
            index: 0,
        };
        let mut body = minimal_body(1);
        body.inputs = vec![byron_input.clone(), normal_input.clone()];
        let tx = build_tx(body);
        let resolved = vec![
            (byron_input, byron_output(), vec![]),
            (normal_input, enterprise_output(1), vec![]),
        ];
        let info = populate_tx_info_v1(&tx, &resolved, &slot_cfg())
            .expect("Alonzo must silently drop the Byron-addressed resolved input");
        assert_eq!(
            info.inputs.len(),
            1,
            "Byron-addressed input must be dropped"
        );
    }

    /// The era gate must not swallow a genuinely unresolved input — that
    /// remains a hard `UtxoDecode` error at every era, including Alonzo.
    #[test]
    fn v1_alonzo_still_surfaces_missing_input() {
        let input = TransactionInput {
            transaction_id: h32(0x99),
            index: 0,
        };
        let mut body = minimal_body(1);
        body.inputs = vec![input];
        let tx = build_tx(body);
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::UtxoDecode(_)));
    }

    // #818: Conway field-presence gate (`guardConwayFeaturesForPlutusV1V2`)
    // + V1-only per-output/per-input restrictions ───────────────────────

    fn conway_tx(body: TransactionBody) -> Transaction {
        let mut tx = build_tx(body);
        tx.era = Era::Conway;
        tx
    }

    fn babbage_tx(body: TransactionBody) -> Transaction {
        let mut tx = build_tx(body);
        tx.era = Era::Babbage;
        tx
    }

    fn inline_datum_output(lovelace: u64) -> TransactionOutput {
        let mut out = enterprise_output(lovelace);
        out.datum = PrimOutputDatum::InlineDatum {
            data: dugite_primitives::transaction::PlutusData::Integer(num_bigint::BigInt::from(0)),
            raw_cbor: None,
        };
        out
    }

    fn ref_script_output(lovelace: u64) -> TransactionOutput {
        let mut out = enterprise_output(lovelace);
        out.script_ref = Some(dugite_primitives::transaction::ScriptRef::PlutusV1(vec![
            0xff,
        ]));
        out
    }

    /// Conway's `guardConwayFeaturesForPlutusV1V2` fires for ANY V1/V2
    /// script when the tx body carries a non-empty `voting_procedures` —
    /// even a completely unrelated plain-spend output — because it is a
    /// whole-transaction gate, not scoped to the redeemer's own purpose.
    #[test]
    fn v1_conway_rejects_non_empty_voting_procedures_for_unrelated_spend() {
        use dugite_primitives::transaction::{GovActionId, Voter, VotingProcedure};
        let mut body = minimal_body(1);
        body.outputs = vec![enterprise_output(1_000_000)];
        let mut inner = BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0x10),
                action_index: 0,
            },
            VotingProcedure {
                vote: dugite_primitives::transaction::Vote::Yes,
                anchor: None,
            },
        );
        body.voting_procedures
            .insert(Voter::DRep(PrimCred::VerificationKey(h28(1))), inner);
        let tx = conway_tx(body);
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
        // V2 hits the identical gate.
        let err2 = populate_tx_info_v2(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err2, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn v1_conway_rejects_non_empty_proposal_procedures() {
        use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
        let mut body = minimal_body(1);
        body.proposal_procedures = vec![ProposalProcedure {
            deposit: Lovelace(1),
            return_addr: {
                let mut v = vec![0xe0u8];
                v.extend([0u8; 28]);
                v
            },
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: String::new(),
                data_hash: h32(0),
            },
        }];
        let tx = conway_tx(body);
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// `currentTreasuryValue` is a STRUCTURAL presence check — `SJust`
    /// fails for ANY wrapped amount, including an explicit zero.
    #[test]
    fn v1_conway_rejects_current_treasury_value_even_when_zero() {
        let mut body = minimal_body(1);
        body.treasury_value = Some(Lovelace(0));
        let tx = conway_tx(body);
        let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// `treasuryDonation` is a VALUE check (`== Coin 0` passes) — an
    /// explicit-but-zero donation must NOT trip the gate, only a non-zero
    /// one.
    #[test]
    fn v1_conway_allows_zero_donation_but_rejects_nonzero_donation() {
        let mut body_zero = minimal_body(1);
        body_zero.donation = Some(Lovelace(0));
        let tx_zero = conway_tx(body_zero);
        populate_tx_info_v1(&tx_zero, &[], &slot_cfg())
            .expect("an explicit zero donation must not trip TreasuryDonationFieldNotSupported");

        let mut body_nonzero = minimal_body(1);
        body_nonzero.donation = Some(Lovelace(1));
        let tx_nonzero = conway_tx(body_nonzero);
        let err = populate_tx_info_v1(&tx_nonzero, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// InlineDatumsNotSupported survives in BOTH Babbage and Conway for V1.
    #[test]
    fn v1_rejects_inline_datum_on_output_in_babbage_and_conway() {
        for tx in [
            babbage_tx({
                let mut b = minimal_body(1);
                b.outputs = vec![inline_datum_output(1)];
                b
            }),
            conway_tx({
                let mut b = minimal_body(1);
                b.outputs = vec![inline_datum_output(1)];
                b
            }),
        ] {
            let err = populate_tx_info_v1(&tx, &[], &slot_cfg()).unwrap_err();
            assert!(
                matches!(err, PhaseTwoError::Internal(_)),
                "V1 must reject an inline datum on an output in era {:?}",
                tx.era
            );
        }
    }

    /// ReferenceScriptsNotSupported is Babbage-ONLY — Conway's own
    /// module-local `transTxOutV1` drops this check.
    #[test]
    fn v1_babbage_rejects_reference_script_but_conway_allows_it() {
        let mut babbage_body = minimal_body(1);
        babbage_body.outputs = vec![ref_script_output(1)];
        let babbage = babbage_tx(babbage_body);
        let err = populate_tx_info_v1(&babbage, &[], &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));

        let mut conway_body = minimal_body(1);
        conway_body.outputs = vec![ref_script_output(1)];
        let conway = conway_tx(conway_body);
        populate_tx_info_v1(&conway, &[], &slot_cfg())
            .expect("Conway's V1 TxInfo no longer checks for a reference script");
    }

    /// The Babbage blanket "any reference inputs at all" rule fails V1
    /// regardless of content; Conway drops it (a V1 script may freely have
    /// reference inputs — they are simply invisible to `PV1.TxInfo`).
    #[test]
    fn v1_babbage_rejects_any_reference_inputs_but_conway_allows_them() {
        let ref_in = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let resolved = vec![(ref_in.clone(), enterprise_output(1), vec![])];

        let mut babbage_body = minimal_body(1);
        babbage_body.reference_inputs = vec![ref_in.clone()];
        let babbage = babbage_tx(babbage_body);
        let err = populate_tx_info_v1(&babbage, &resolved, &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));

        let mut conway_body = minimal_body(1);
        conway_body.reference_inputs = vec![ref_in];
        let conway = conway_tx(conway_body);
        populate_tx_info_v1(&conway, &resolved, &slot_cfg())
            .expect("Conway's V1 TxInfo no longer restricts reference inputs");
    }

    /// Conway still translates (then discards) reference inputs — an
    /// inline datum on a REFERENCE input's resolved output must still
    /// fail, even though reference inputs never appear in `TxInfoV1`.
    #[test]
    fn v1_conway_rejects_inline_datum_on_reference_input() {
        let ref_in = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let resolved = vec![(ref_in.clone(), inline_datum_output(1), vec![])];
        let mut body = minimal_body(1);
        body.reference_inputs = vec![ref_in];
        let tx = conway_tx(body);
        let err = populate_tx_info_v1(&tx, &resolved, &slot_cfg()).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// V2 is fully exempt from ALL of the V1-only per-output/per-input
    /// restrictions, in every era: inline datum, reference script, and
    /// reference inputs are all freely observable.
    #[test]
    fn v2_allows_inline_datum_reference_script_and_reference_inputs() {
        let ref_in = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let resolved = vec![(ref_in.clone(), enterprise_output(1), vec![])];
        for era in [Era::Babbage, Era::Conway] {
            let mut body = minimal_body(1);
            body.outputs = vec![inline_datum_output(1), ref_script_output(1)];
            body.reference_inputs = vec![ref_in.clone()];
            let mut tx = build_tx(body);
            tx.era = era;
            populate_tx_info_v2(&tx, &resolved, &slot_cfg()).unwrap_or_else(|e| {
                panic!("V2 must allow inline datum / reference script / reference inputs in {era:?}: {e}")
            });
        }
    }

    #[test]
    fn v2_minimal_tx_yields_empty_collections() {
        let tx = build_tx(minimal_body(123));
        let info = populate_tx_info_v2(&tx, &[], &slot_cfg()).unwrap();
        assert_eq!(info.fee, BigInt::from(123));
        assert!(info.reference_inputs.is_empty());
        assert!(info.redeemers.is_empty()); // minimal tx has no redeemers
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

    /// #833: mixed-language tx — a V2 spending script + a stake-deregistration
    /// cert witnessed by a DIFFERENT-language (V3) script. The V2 context's
    /// `txInfoRedeemers` `Certifying` key must carry the V1/V2 `DCert` shape
    /// (StakingHash-wrapped, exactly 1 field), NOT the Conway V3 `TxCert`
    /// shape the cert's own V3 script would see in ITS context. Before the
    /// fix, `rr.purpose` was baked once at redeemer-resolve time using the
    /// witnessing script's language (V3 here) and reused verbatim in every
    /// context's redeemers map.
    #[test]
    fn v2_certifying_redeemer_uses_v1v2_dcert_schema_even_when_cert_witnessed_by_v3_script() {
        use dugite_primitives::credentials::Credential as PrimCred;
        use dugite_primitives::transaction::{
            Certificate, ExUnits, PlutusData as PrimPlutusData, Redeemer, RedeemerTag,
        };

        // V2 spending script.
        let spend_bytes = vec![0x01u8, 0x02];
        let spend_hash = {
            let mut buf = vec![2u8];
            buf.extend_from_slice(&spend_bytes);
            dugite_primitives::hash::blake2b_224(&buf).0
        };
        // V3 cert-witnessing script — different language than the spending script.
        let cert_bytes = vec![0x03u8, 0x04];
        let cert_hash = {
            let mut buf = vec![3u8];
            buf.extend_from_slice(&cert_bytes);
            dugite_primitives::hash::blake2b_224(&buf).0
        };

        let input = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let spent_out = TransactionOutput {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::Script(Hash::<28>(spend_hash)),
            }),
            value: Value::lovelace(5_000_000),
            // V1/V2 spending requires a resolvable datum; use an inline
            // datum (Babbage+) so this test doesn't also need a witness
            // datum wired up.
            datum: PrimOutputDatum::InlineDatum {
                data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
                raw_cbor: None,
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let mut body = minimal_body(200_000);
        body.inputs = vec![input.clone()];
        body.certificates = vec![Certificate::StakeDeregistration(PrimCred::Script(
            Hash::<28>(cert_hash),
        ))];

        let mut ws = empty_witness_set();
        ws.plutus_v2_scripts = vec![spend_bytes];
        ws.plutus_v3_scripts = vec![cert_bytes];
        ws.redeemers = vec![
            Redeemer {
                tag: RedeemerTag::Spend,
                index: 0,
                data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
                ex_units: ExUnits { mem: 1, steps: 1 },
            },
            Redeemer {
                tag: RedeemerTag::Cert,
                index: 0,
                data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
                ex_units: ExUnits { mem: 1, steps: 1 },
            },
        ];

        let tx = build_tx_with_witness(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let info = populate_tx_info_v2(&tx, &resolved, &slot_cfg()).unwrap();

        // 2 redeemers: Spend (ConwayPlutusPurpose rank 0) then Cert (rank 2).
        assert_eq!(info.redeemers.len(), 2);
        let (purpose, _) = &info.redeemers[1];
        let ScriptPurpose::Certifying(idx, tx_cert) = purpose else {
            panic!("second redeemer must be Certifying; got {purpose:?}");
        };
        assert_eq!(*idx, 0);
        // V1/V2 DCertDelegDeRegKey (StakingHash (ScriptCredential h)) =
        //   Constr 1 [Constr 0 [Constr 1 [B h]]]  — exactly 1 field.
        assert_eq!(
            tx_cert.0,
            Data::Constr(
                1,
                vec![Data::Constr(
                    0,
                    vec![Data::Constr(1, vec![Data::B(cert_hash.to_vec())])]
                )]
            ),
            "Certifying cert Data must use the V1/V2 DCert schema (1 field), \
             not the V3 TxCert shape the witnessing V3 script would see"
        );
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
