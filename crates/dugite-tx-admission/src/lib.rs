//! Conversion of the ledger's `Vec<ValidationError>` into the wire-level
//! [`TxValidationError`] that `encode_apply_tx_err` turns into an `ApplyTxErr`.
//!
//! This code used to live, `pub(crate)`, in `dugite-node`'s `serve.rs`.  It moved here unchanged
//! so that the node-diversity conformance harness (#1125) runs the SAME conversion, at the same
//! protocol version, that a real LocalTxSubmission rejection goes through.  The harness then
//! decodes the bytes `encode_apply_tx_err` emits and never consults a hand-written name table.

use dugite_network::TxValidationError;
use dugite_primitives::transaction::Transaction;

/// Turn every failure the ledger raised for `tx` into the single `TxValidationError` the N2C
/// server encodes: one failure stays as it is, several become [`TxValidationError::Multiple`].
///
/// This is the tail of `LedgerTxValidator::validate_tx` (`dugite-node`), extracted so the node
/// and the conformance harness cannot drift apart.  `pv_major` is the CURRENT protocol version
/// (two DELEG failures and the script-integrity mismatch change shape at PV 11).
///
/// `errors` must be non-empty: a rejection with no failure is not a rejection.
pub fn rejection_from_validation_errors(
    errors: Vec<dugite_ledger::validation::ValidationError>,
    tx: &Transaction,
    utxo_view: &impl dugite_ledger::utxo::UtxoLookup,
    pv_major: u64,
) -> TxValidationError {
    let mut mapped: Vec<TxValidationError> =
        enrich_validation_errors(errors, tx, utxo_view, pv_major);
    if mapped.len() == 1 {
        mapped.pop().expect("vec has exactly one element")
    } else {
        TxValidationError::Multiple(mapped)
    }
}

/// Convert a `dugite_primitives::transaction::Voter` into the `(disc, credential_hex)`
/// wire representation used by the GOV predicate failure CBOR encoder.
///
/// Wire discriminators match the Conway CDDL `voter` encoding:
///   0 = CC key, 1 = CC script, 2 = DRep key, 3 = DRep script, 4 = SPO (key only)
fn voter_to_disc_hex(voter: &dugite_primitives::transaction::Voter) -> (u8, String) {
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::transaction::Voter;
    match voter {
        Voter::ConstitutionalCommittee(Credential::VerificationKey(h)) => {
            (0u8, hex::encode(&h.as_bytes()[..28]))
        }
        Voter::ConstitutionalCommittee(Credential::Script(h)) => {
            (1u8, hex::encode(&h.as_bytes()[..28]))
        }
        Voter::DRep(Credential::VerificationKey(h)) => (2u8, hex::encode(&h.as_bytes()[..28])),
        Voter::DRep(Credential::Script(h)) => (3u8, hex::encode(&h.as_bytes()[..28])),
        Voter::StakePool(h32) => {
            // Pool voter: 28 bytes stored in the first 28 bytes of Hash32
            (4u8, hex::encode(&h32.as_bytes()[..28]))
        }
    }
}

/// Convert a `GovActionId` to `"<txhash>#<action_index>"` string.
fn gov_action_id_to_string(id: &dugite_primitives::transaction::GovActionId) -> String {
    format!("{}#{}", id.transaction_id.to_hex(), id.action_index)
}

/// Map a `NetworkId` to the wire byte Haskell's `Network` uses: 0 testnet,
/// 1 mainnet.
fn network_id_to_wire(n: dugite_primitives::network::NetworkId) -> u8 {
    match n {
        dugite_primitives::network::NetworkId::Mainnet => 1,
        _ => 0,
    }
}

/// Map dugite's redeemer-purpose name to the `PlutusPurpose AsIx` wire tag.
///
/// Conway numbering (`ConwayPlutusPurpose`): 0 spending, 1 minting,
/// 2 certifying, 3 rewarding, 4 voting, 5 proposing. An unknown name returns
/// `None` so the caller falls back rather than inventing a tag.
fn redeemer_tag_to_wire(tag: &str) -> Option<u8> {
    match tag.to_ascii_lowercase().as_str() {
        "spend" | "spending" => Some(0),
        "mint" | "minting" => Some(1),
        "cert" | "certifying" => Some(2),
        "reward" | "rewarding" | "withdrawal" => Some(3),
        "vote" | "voting" => Some(4),
        "propose" | "proposing" => Some(5),
        _ => None,
    }
}

/// #1025: build the full `Vec<TxValidationError>` for a rejected tx,
/// COMBINING same-kind `ValidationError` occurrences into a single
/// byte-correct typed wire arm where the real Haskell predicate failure
/// needs a whole set/collection that dugite raises one entry at a time —
/// and, for two kinds whose payload needs data the raw `ValidationError`
/// never carries, re-deriving that data fresh from the already-decoded
/// `tx` (and, for collateral, the same UTxO view Phase-1 validation used).
///
/// This exists in `dugite-node` specifically because `dugite-ledger`'s
/// validation code (owned by a concurrent session this session) is off
/// limits: none of `ValidationError`'s payloads were widened to make this
/// possible. Everything below is a PURE re-derivation of data the
/// validator already established — it makes no new accept/reject decision,
/// only reshapes already-validated facts into the wire shape Haskell uses.
/// Where that reshape needs data genuinely absent here too (a `ScriptHash`
/// for `MissingRedeemer`, the whole `GovAction` for `MalformedProposal`),
/// the corresponding error is left generic by the per-error fallback below
/// — see the #1025 PR description for the full per-variant table.
pub fn enrich_validation_errors(
    errors: Vec<dugite_ledger::validation::ValidationError>,
    tx: &dugite_primitives::transaction::Transaction,
    utxo_view: &impl dugite_ledger::utxo::UtxoLookup,
    pv_major: u64,
) -> Vec<TxValidationError> {
    use dugite_ledger::validation::ValidationError as VE;
    use dugite_primitives::transaction::GovAction;

    let mut consumed = vec![false; errors.len()];
    let mut mapped: Vec<TxValidationError> = Vec::new();

    // ── MissingRequiredDatumsUTXOW (UTXOW tag 11) ──
    //
    // Haskell's second field is `Map.keysSet (tx witness datums)` — every
    // datum hash the tx's OWN witness set supplies, a pure witness-set
    // derivation independent of which hashes are missing. If it can't be
    // derived faithfully (no preserved raw spans — see the datum-hash
    // re-encoding trap documented on `supplied_datum_hashes`), these
    // occurrences are left generic rather than shipped with a wrong set.
    let missing_idx: Vec<usize> = errors
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, VE::MissingDatumWitness(_)).then_some(i))
        .collect();
    if !missing_idx.is_empty() {
        if let Some(provided) = supplied_datum_hashes(tx) {
            let missing = missing_idx
                .iter()
                .map(|&i| match &errors[i] {
                    VE::MissingDatumWitness(h) => h.clone(),
                    _ => unreachable!("filtered above"),
                })
                .collect();
            mapped.push(TxValidationError::MissingRequiredDatumsUTXOW { missing, provided });
            for &i in &missing_idx {
                consumed[i] = true;
            }
        }
    }

    // ── NotAllowedSupplementalDatumsUTXOW (UTXOW tag 12) ──
    //
    // Haskell's second field is `getSupplementalDataHashes` — every datum
    // hash referenced by the tx's OWN outputs. Unlike the missing-datum
    // case this needs no raw-span reconstruction: `OutputDatum::DatumHash`
    // already carries the hash directly, so this is always derivable.
    let extra_idx: Vec<usize> = errors
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, VE::ExtraDatumWitness(_)).then_some(i))
        .collect();
    if !extra_idx.is_empty() {
        let allowed = allowed_output_datum_hashes(tx);
        let extra = extra_idx
            .iter()
            .map(|&i| match &errors[i] {
                VE::ExtraDatumWitness(h) => h.clone(),
                _ => unreachable!("filtered above"),
            })
            .collect();
        mapped.push(TxValidationError::NotAllowedSupplementalDatumsUTXOW { extra, allowed });
        for &i in &extra_idx {
            consumed[i] = true;
        }
    }

    // ── OutputBootAddrAttrsTooBigUTXO (UTXO tag 10) ──
    //
    // `oversized_outputs` already carries the exact tx-body output indices
    // (outputs are an ORDERED sequence, not a set, so this is a direct
    // positional lookup — no UTxO join needed).
    for (i, e) in errors.iter().enumerate() {
        if let VE::OutputBootAddrAttrsTooBig { oversized_outputs } = e {
            let outs: Option<Vec<String>> = oversized_outputs
                .iter()
                .map(|&idx| tx.body.outputs.get(idx).map(raw_output_hex))
                .collect();
            if let Some(outputs_raw_cbor) = outs {
                mapped.push(TxValidationError::OutputBootAddrAttrsTooBigUTXO { outputs_raw_cbor });
                consumed[i] = true;
            }
        }
    }

    // ── ScriptsNotPaidUTxOUTXO (UTXO tag 13) ──
    //
    // `ScriptLockedCollateral`'s `inputs` are TxIn refs only; the matching
    // `TxOut` is looked up against the SAME `utxo_view` Phase-1 validation
    // already used (no new trust decision — the input was already resolved
    // once during validation).
    for (i, e) in errors.iter().enumerate() {
        if let VE::ScriptLockedCollateral { inputs } = e {
            let resolved: Option<Vec<(String, String)>> = inputs
                .iter()
                .map(|s| {
                    let txin = parse_tx_input_ref(s)?;
                    let out = utxo_view.lookup(&txin)?;
                    Some((s.clone(), raw_output_hex(&out)))
                })
                .collect();
            if let Some(inputs_outputs) = resolved {
                mapped.push(TxValidationError::ScriptsNotPaidUTxOUTXO { inputs_outputs });
                consumed[i] = true;
            }
        }
    }

    // ── BabbageOutputTooSmallUTxO (Conway UTXO tag 21) ──
    //
    // Haskell aggregates EVERY offending output in the tx into ONE
    // `NonEmpty (TxOut era, Coin)` failure; dugite's Phase-1 validator
    // raises one `ValidationError::OutputTooSmall` PER offending output
    // (carrying its `output_index` into `tx.body.outputs`), so this groups
    // every occurrence from a single `validate_transaction` call back into
    // the one Haskell-shaped failure — same aggregation pattern as
    // `OutputBootAddrAttrsTooBigUTXO` above, except each `ValidationError`
    // here already carries its own index rather than one error carrying
    // many.
    let output_too_small_idx: Vec<usize> = errors
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, VE::OutputTooSmall { .. }).then_some(i))
        .collect();
    if !output_too_small_idx.is_empty() {
        let outs: Option<Vec<(String, u64)>> = output_too_small_idx
            .iter()
            .map(|&i| match &errors[i] {
                VE::OutputTooSmall {
                    minimum,
                    output_index,
                    ..
                } => {
                    // The sentinel index points at `collateral_return`, which
                    // Haskell folds into the SAME `BabbageOutputTooSmallUTxO`
                    // via `allSizedOutputsTxBodyF`.
                    let out = if *output_index
                        == dugite_ledger::validation::COLLATERAL_RETURN_OUTPUT_INDEX
                    {
                        tx.body.collateral_return.as_ref()
                    } else {
                        tx.body.outputs.get(*output_index)
                    };
                    out.map(|o| (raw_output_hex(o), *minimum))
                }
                _ => unreachable!("filtered above"),
            })
            .collect();
        if let Some(outputs) = outs {
            mapped.push(TxValidationError::BabbageOutputTooSmallUTxO { outputs });
            for &i in &output_too_small_idx {
                consumed[i] = true;
            }
        }
    }

    // ── MissingRedeemersUTXOW (UTXOW tag 10) ──
    //
    // Haskell raises ONE `MissingRedeemers (NonEmpty (PlutusPurpose AsItem era,
    // ScriptHash))` carrying every missing purpose; dugite raises one
    // `MissingRedeemer` per purpose, so they aggregate into a single frame.
    //
    // The ScriptHash now travels on the ValidationError itself (#1025) — every
    // raise site already had it. The ITEM is re-derived here by (tag, index)
    // using the SAME ordering the raise site indexed by, which is documented at
    // each site in `dugite_ledger::validation::collateral`:
    //
    //   Mint    `body.mint.keys().enumerate()`                 (BTreeMap order)
    //   Cert    raw positional index into `body.certificates`
    //   Reward  `body.withdrawals.keys().enumerate()`          (BTreeMap order)
    //   Vote    `body.voting_procedures.keys().enumerate()`    (BTreeMap order)
    //   Propose raw positional index into `body.proposal_procedures`
    //
    // Every one of those is a deterministic order over a container this
    // function has in hand, so the lookup cannot name a different item than
    // the validator meant. If ANY entry fails to resolve, the whole aggregate
    // is abandoned and every occurrence falls through to the generic arm —
    // a frame naming the wrong input would be worse than a free-text reason.
    {
        let missing_redeemer_idx: Vec<usize> = errors
            .iter()
            .enumerate()
            .filter_map(|(i, e)| matches!(e, VE::MissingRedeemer { .. }).then_some(i))
            .collect();
        if !missing_redeemer_idx.is_empty() {
            let mut entries: Vec<(dugite_network::PlutusPurposeItem, String)> = Vec::new();
            let mut all_resolved = true;
            for &i in &missing_redeemer_idx {
                let VE::MissingRedeemer {
                    tag,
                    index,
                    script_hash,
                } = &errors[i]
                else {
                    unreachable!("filtered above")
                };
                let ix = *index as usize;
                let item =
                    match tag.as_str() {
                        "Mint" => tx.body.mint.keys().nth(ix).map(|p| {
                            dugite_network::PlutusPurposeItem::Minting {
                                policy_id: p.to_hex(),
                            }
                        }),
                        "Cert" => tx.body.certificates.get(ix).map(|c| {
                            dugite_network::PlutusPurposeItem::Certifying(Box::new(c.clone()))
                        }),
                        "Reward" => tx.body.withdrawals.keys().nth(ix).map(|a| {
                            dugite_network::PlutusPurposeItem::Withdrawing {
                                account: hex::encode(a),
                            }
                        }),
                        "Vote" => tx.body.voting_procedures.keys().nth(ix).map(|v| {
                            dugite_network::PlutusPurposeItem::Voting(Box::new(v.clone()))
                        }),
                        "Propose" => tx.body.proposal_procedures.get(ix).map(|p| {
                            dugite_network::PlutusPurposeItem::Proposing(Box::new(p.clone()))
                        }),
                        // No `Spend` arm: the missing-redeemer check never raises
                        // one. An unrecognised tag must NOT be guessed at.
                        _ => None,
                    };
                match item {
                    Some(item) => entries.push((item, script_hash.clone())),
                    None => {
                        all_resolved = false;
                        break;
                    }
                }
            }
            if all_resolved && !entries.is_empty() {
                mapped.push(TxValidationError::MissingRedeemersUTXOW { entries });
                for &i in &missing_redeemer_idx {
                    consumed[i] = true;
                }
            }
        }
    }

    // ── MalformedProposalGOV (GOV tag 1) ──
    //
    // Haskell's payload is the WHOLE `GovAction`, one failure per offending
    // proposal. `ValidationError::MalformedProposal` now carries the proposal's
    // INDEX (#1025), so this is a direct positional lookup into
    // `tx.body.proposal_procedures` — the same shape as the
    // `OutputBootAddrAttrsTooBigUTXO` enrichment below.
    //
    // The index is what makes this well-defined for a multi-proposal tx. The
    // alternative considered and rejected was re-deriving `ppuWellFormed` here,
    // which would duplicate a ~30-field structural check in a second place and
    // could disagree with the validator that actually made the decision.
    for (i, e) in errors.iter().enumerate() {
        if let VE::MalformedProposal { proposal_index, .. } = e {
            if let Some(p) = tx.body.proposal_procedures.get(*proposal_index) {
                mapped.push(TxValidationError::MalformedProposalGOV {
                    action: Box::new(p.gov_action.clone()),
                });
                consumed[i] = true;
            }
            // Out-of-range index: fall through to the generic arm rather than
            // ship a frame describing the wrong proposal.
        }
    }

    // ── ZeroTreasuryWithdrawalsGOV (GOV tag 15) ──
    //
    // Haskell raises ONE `ZeroTreasuryWithdrawals (GovAction era)` PER
    // offending proposal — dugite's `offending_proposals` aggregates into a
    // single marker with no correlation back to a specific proposal, so
    // this re-derives the zero-sum condition directly from
    // `tx.body.proposal_procedures` (mirroring
    // `is_treasury_withdrawals_zero_sum`'s PV==9 bootstrap skip) rather
    // than trying to match strings back to proposals.
    if errors
        .iter()
        .any(|e| matches!(e, VE::ZeroTreasuryWithdrawals { .. }))
    {
        let mut any_built = false;
        for p in &tx.body.proposal_procedures {
            if let GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash,
            } = &p.gov_action
            {
                let sum: u128 = withdrawals.values().map(|c| c.0 as u128).sum();
                if sum == 0 && pv_major != 9 {
                    mapped.push(TxValidationError::ZeroTreasuryWithdrawalsGOV {
                        withdrawals: withdrawals
                            .iter()
                            .map(|(a, c)| (hex::encode(a), c.0))
                            .collect(),
                        policy_hash: policy_hash.as_ref().map(|h| hex::encode(h.as_ref())),
                    });
                    any_built = true;
                }
            }
        }
        if any_built {
            for (i, e) in errors.iter().enumerate() {
                if matches!(e, VE::ZeroTreasuryWithdrawals { .. }) {
                    consumed[i] = true;
                }
            }
        }
    }

    // Everything not combined above goes through the existing per-error
    // mapping (typed where #979/#1025 already cover it, generic otherwise).
    for (i, e) in errors.into_iter().enumerate() {
        if !consumed[i] {
            mapped.push(convert_validation_error_at_pv(e, pv_major));
        }
    }
    mapped
}

/// Every datum hash the tx's OWN witness set supplies — `Map.keysSet`
/// applied to Haskell's `TxDats`, needed by `MissingRequiredDatumsUTXOW`'s
/// second field.
///
/// Prefers the raw per-element CBOR spans (`plutus_data_element_spans`) the
/// SAME way `dugite-ledger`'s own datum validation does: on-chain datums
/// are frequently non-canonically encoded, and Haskell memoises + hashes
/// the ORIGINAL bytes, never a re-encoding (see the crate-wide
/// `crypto-output-cbor-reencode` lesson). Returns `None` — rather than a
/// hash set built from a lossy re-encode — when those spans aren't
/// available, so the caller can fall back to leaving the occurrence
/// generic instead of shipping a wrong set.
fn supplied_datum_hashes(tx: &dugite_primitives::transaction::Transaction) -> Option<Vec<String>> {
    let spans = tx
        .witness_set
        .raw_plutus_data_cbor
        .as_deref()
        .and_then(dugite_serialization::plutus_data_element_spans)?;
    if spans.len() != tx.witness_set.plutus_data.len() {
        return None;
    }
    Some(
        spans
            .iter()
            .map(|raw| dugite_primitives::hash::blake2b_256(raw).to_hex())
            .collect(),
    )
}

/// Every datum hash referenced by the tx's OWN outputs —
/// `getSupplementalDataHashes`, needed by
/// `NotAllowedSupplementalDatumsUTXOW`'s second field. Unlike
/// [`supplied_datum_hashes`] this never needs re-hashing: an output's
/// `OutputDatum::DatumHash` already carries the hash verbatim.
fn allowed_output_datum_hashes(tx: &dugite_primitives::transaction::Transaction) -> Vec<String> {
    use dugite_primitives::transaction::OutputDatum;
    tx.body
        .outputs
        .iter()
        .filter_map(|o| match &o.datum {
            OutputDatum::DatumHash(h) => Some(h.to_hex()),
            _ => None,
        })
        .collect()
}

/// Hex-encoded raw CBOR of a `TransactionOutput`, preferring the ORIGINAL
/// wire bytes (`raw_cbor`, populated by every era's decoder) over a fresh
/// re-encode — the same "never re-encode when the original bytes survived"
/// rule the datum-hash path above follows, and for the same reason: a
/// legacy-vs-post-Alonzo or indefinite-length-datum mismatch in a re-encode
/// has bitten this codebase before (`crypto-output-cbor-reencode`).
fn raw_output_hex(output: &dugite_primitives::transaction::TransactionOutput) -> String {
    match &output.raw_cbor {
        Some(raw) => hex::encode(raw),
        None => hex::encode(dugite_serialization::encode::encode_transaction_output(
            output,
        )),
    }
}

/// Parse dugite's `"<txhash>#<index>"` `TransactionInput::Display` format
/// back into a real `TransactionInput` for a UTxO lookup.
fn parse_tx_input_ref(s: &str) -> Option<dugite_primitives::transaction::TransactionInput> {
    let (hash_hex, idx_str) = s.rsplit_once('#')?;
    let transaction_id = dugite_primitives::hash::Hash32::from_hex(hash_hex).ok()?;
    let index: u32 = idx_str.parse().ok()?;
    Some(dugite_primitives::transaction::TransactionInput {
        transaction_id,
        index,
    })
}

/// Convert a ledger `ValidationError` into the network-facing
/// `TxValidationError`, at the CURRENT protocol version.
///
/// The protocol version is load-bearing, not decoration. Two DELEG failures
/// changed shape at PV 11 (`hardforkConwayDELEGIncorrectDepositsAndRefunds`):
/// below it, an incorrect stake-key deposit OR refund is
/// `IncorrectDepositDELEG Coin` (tag 1, carrying only the supplied value);
/// from PV 11 they split into `DepositIncorrectDELEG` (tag 7) and
/// `RefundIncorrectDELEG` (tag 8), each carrying a full `Mismatch`.
///
/// Every real network runs PV 10 today, so emitting only the PV>=11 form would
/// mean the reachable case degrades while the implemented arms are dead code —
/// exactly the inversion #978 found in the withdrawal path.
pub fn convert_validation_error_at_pv(
    e: dugite_ledger::validation::ValidationError,
    pv_major: u64,
) -> TxValidationError {
    use dugite_ledger::validation::ValidationError as VE;
    // Pre-PV11 collapses both mismatches onto ONE constructor carrying only
    // the supplied amount. Handle them here so the main table stays a plain
    // 1:1 map.
    if pv_major <= 10 {
        match e {
            VE::StakeRegistrationDepositMismatch { declared, .. } => {
                return TxValidationError::IncorrectDepositDELEG { supplied: declared };
            }
            VE::StakeDeregistrationRefundMismatch { declared, .. } => {
                // Upstream reports the REFUND through the same
                // `IncorrectDepositDELEG` constructor pre-PV11 — there is no
                // separate refund tag before the split.
                return TxValidationError::IncorrectDepositDELEG { supplied: declared };
            }
            other => return convert_validation_error(other),
        }
    }
    // #1058: the script-integrity-hash mismatch also splits at PV 11.
    // `checkScriptIntegrityHash` chooses
    //   pv < 11  -> PPViewHashesDontMatch mismatch                    (UTXOW 13)
    //   pv >= 11 -> ScriptIntegrityHashMismatch mismatch preimage      (UTXOW 18)
    // and the two differ in tag AND payload shape (13 flattens the Mismatch via
    // ToGroup; 18 nests it and adds a third field). dugite emitted 13 at every
    // PV, which is wrong on preview/PV11 — the #978 inversion.
    if let VE::ScriptDataHashMismatch { expected, actual } = &e {
        return TxValidationError::ScriptIntegrityHashMismatchUTXOW {
            supplied: Some(actual.clone()),
            expected: Some(expected.clone()),
            // Haskell's `originalBytes <$> scriptIntegrity` — the preimage, not a
            // hash. dugite's Phase-1 error carries only hashes, so SNothing.
            expected_bytes: None,
        };
    }
    convert_validation_error(e)
}

/// Convert a ledger `ValidationError` into the network-facing `TxValidationError`.
///
/// PV-independent mappings only — see [`convert_validation_error_at_pv`] for
/// the two that are gated.
pub fn convert_validation_error(
    e: dugite_ledger::validation::ValidationError,
) -> TxValidationError {
    use dugite_ledger::validation::ValidationError as VE;
    match e {
        VE::NoInputs => TxValidationError::NoInputs,
        VE::InputNotFound(input) => TxValidationError::InputNotFound { input },
        VE::ValueNotConserved {
            inputs,
            outputs,
            fee,
        } => TxValidationError::ValueNotConserved {
            inputs,
            outputs,
            fee,
        },
        VE::FeeTooSmall { minimum, actual } => TxValidationError::FeeTooSmall { minimum, actual },
        VE::OutputTooSmall {
            minimum, actual, ..
        } => TxValidationError::OutputTooSmall { minimum, actual },
        VE::TxTooLarge { maximum, actual } => TxValidationError::TxTooLarge { maximum, actual },
        VE::MissingRequiredSigner(signer) => TxValidationError::MissingRequiredSigner { signer },
        VE::MissingWitness(input) => TxValidationError::MissingWitness { input },
        VE::TtlExpired { current_slot, ttl } => TxValidationError::TtlExpired { current_slot, ttl },
        VE::NotYetValid {
            current_slot,
            valid_from,
        } => TxValidationError::NotYetValid {
            current_slot,
            valid_from,
        },
        // `ValidationError::ScriptFailed` has exactly ONE producer —
        // `phase2_admission_error`, for `is_valid = true` + a genuine evaluation
        // failure — so it maps to the dedicated phase-2 carrier and gets the
        // typed `ValidationTagMismatch … FailedUnexpectedly` wire class (#1053).
        //
        // It deliberately does NOT ride `TxValidationError::ScriptFailed`: that
        // network variant is the free-text catch-all for ~20 unrelated
        // rejections below, and a typed phase-2 class stamped on all of them
        // would be worse than the generic fallback (#979).
        VE::ScriptFailed(reason) => TxValidationError::Phase2ScriptsFailedUnexpectedly {
            messages: vec![reason],
        },
        VE::InsufficientCollateral { balance, required } => {
            TxValidationError::InsufficientCollateral { balance, required }
        }
        VE::TooManyCollateralInputs { max, actual } => {
            TxValidationError::TooManyCollateralInputs { max, actual }
        }
        // No dedicated wire variant yet (dugite issue #1024, tracked
        // alongside #979's "generic rejections" backlog) — falls back to the
        // same generic-reason pattern already used for `MalformedProposal`
        // below. The transaction is still correctly rejected; only the
        // MsgRejectTx REASON is generic rather than a byte-exact
        // `ConwayLedgerPredFailure` tag-6 frame.
        VE::RefScriptsSizeTooBig { maximum, actual } => TxValidationError::ScriptFailed {
            reason: format!(
                "ConwayTxRefScriptsSizeTooBig: total ref-script size {actual} exceeds \
                 per-transaction maximum {maximum}"
            ),
        },
        VE::CollateralNotFound(input) => TxValidationError::CollateralNotFound { input },
        VE::CollateralHasTokens(value) => TxValidationError::CollateralHasTokens { value },
        VE::CollateralMismatch { declared, computed } => {
            TxValidationError::CollateralMismatch { declared, computed }
        }
        VE::ReferenceInputNotFound(input) => TxValidationError::ReferenceInputNotFound { input },
        VE::ReferenceInputOverlapsInput(input) => {
            TxValidationError::ReferenceInputOverlapsInput { input }
        }
        VE::ReferenceInputsNotDisjointFromInputs(inputs) => {
            TxValidationError::ReferenceInputsNotDisjointFromInputs { inputs }
        }
        VE::MultiAssetNotConserved {
            policy,
            input_side,
            output_side,
        } => TxValidationError::MultiAssetNotConserved {
            policy,
            input_side,
            output_side,
        },
        VE::InvalidMint => TxValidationError::InvalidMint,
        VE::ExUnitsExceeded => TxValidationError::ExUnitsExceeded,
        VE::ScriptDataHashMismatch { expected, actual } => {
            TxValidationError::ScriptDataHashMismatch { expected, actual }
        }
        VE::UnexpectedScriptDataHash => TxValidationError::UnexpectedScriptDataHash,
        VE::MissingScriptDataHash => TxValidationError::MissingScriptDataHash,
        VE::DuplicateInput(input) => TxValidationError::DuplicateInput { input },
        VE::NativeScriptFailed => TxValidationError::NativeScriptFailed,
        VE::InvalidWitnessSignature(vkey) => TxValidationError::InvalidWitnessSignature { vkey },
        VE::NetworkMismatch { expected, actual } => TxValidationError::NetworkMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        },
        VE::AuxiliaryDataHashWithoutData => TxValidationError::AuxiliaryDataHashWithoutData,
        VE::AuxiliaryDataWithoutHash => TxValidationError::AuxiliaryDataWithoutHash,
        VE::BlockExUnitsExceeded {
            resource,
            limit,
            total,
        } => TxValidationError::BlockExUnitsExceeded {
            resource,
            limit,
            total,
        },
        VE::OutputValueTooLarge { maximum, actual } => {
            TxValidationError::OutputValueTooLarge { maximum, actual }
        }
        VE::MissingRawCbor => TxValidationError::MissingRawCbor,
        VE::MissingSlotConfig => TxValidationError::MissingSlotConfig,
        VE::IsValidTagMismatch { declared, evaluated } => {
            TxValidationError::IsValidTagMismatch { declared, evaluated }
        }
        // Phase-2 collection/context errors (Haskell `UtxosFailure
        // (CollectErrors …)` — e.g. TimeTranslationPastHorizon, strict UTxO
        // decode) ride the ScriptFailed wire variant so the N2C reject
        // reason carries the full message without a wire-format change.
        VE::Phase2CollectError(reason) => TxValidationError::ScriptFailed { reason },
        // CEK panic: rejected at admission (reject-by-default), surfaced to
        // the client as a script failure (#733 correction 3).
        VE::Phase2EvalPanic(reason) => TxValidationError::ScriptFailed { reason },
        VE::MissingSpendRedeemer { index } => TxValidationError::MissingSpendRedeemer { index },
        VE::RedeemerIndexOutOfRange { tag, index, max } => {
            TxValidationError::RedeemerIndexOutOfRange { tag, index, max: max as u32 }
        }
        VE::MissingInputWitness(credential) => {
            TxValidationError::MissingInputWitness { credential }
        }
        VE::MissingScriptWitness(credential) => {
            TxValidationError::MissingScriptWitness { credential }
        }
        VE::MissingWithdrawalWitness(credential) => {
            TxValidationError::MissingWithdrawalWitness { credential }
        }
        VE::MissingWithdrawalScriptWitness(credential) => {
            TxValidationError::MissingWithdrawalScriptWitness { credential }
        }
        VE::MissingCertificateWitness(credential) => {
            TxValidationError::MissingCertificateWitness { credential }
        }
        VE::MissingCertificateScriptWitness(credential) => {
            TxValidationError::MissingCertificateScriptWitness { credential }
        }
        VE::ValueOverflow => TxValidationError::ValueOverflow,
        VE::EraGatingViolation {
            certificate_type,
            required_era,
            current_era,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "Era gating violation: {certificate_type} requires {required_era}, current era is {current_era}"
            ),
        },
        VE::GovernancePreConway { current_version } => TxValidationError::ScriptFailed {
            reason: format!(
                "Governance features not available pre-Conway (current protocol version: {current_version})"
            ),
        },
        VE::TreasuryValueMismatch { declared, actual } => {
            TxValidationError::TreasuryValueMismatch {
                supplied: declared,
                expected: actual,
            }
        }
        VE::UnelectedCommitteeMember {
            cold_credential_hash,
        } => TxValidationError::ConwayCommitteeIsUnknown {
            credential: cold_credential_hash,
        },
        VE::MissingRedeemer {
            tag,
            index,
            script_hash,
        } => TxValidationError::ScriptFailed {
            reason: format!("Missing redeemer for {tag} at index {index} (script {script_hash})"),
        },
        VE::MissingDatumWitness(datum_hash) => TxValidationError::ScriptFailed {
            reason: format!("Missing datum witness for script-locked input: datum hash {datum_hash}"),
        },
        VE::ExtraDatumWitness(datum_hash) => TxValidationError::ScriptFailed {
            reason: format!("Extra (unreferenced) datum witness in transaction: datum hash {datum_hash}"),
        },
        VE::TxRefScriptSizeTooLarge { actual, limit } => TxValidationError::TxTooLarge {
            // Map to TxTooLarge — closest semantic match for a transaction that
            // exceeds a size-based limit (ppMaxRefScriptSizePerTxG, Conway+).
            maximum: limit,
            actual,
        },
        VE::ZeroWithdrawal { account } => TxValidationError::ScriptFailed {
            reason: format!("Zero withdrawal amount for reward account: {account}"),
        },
        VE::WithdrawalsNotInRewardsCERTS { bad } => {
            TxValidationError::WithdrawalsNotInRewardsCERTS { bad: bad.clone() }
        }
        VE::ConwayWithdrawalsMissingAccounts { missing } => {
            TxValidationError::WithdrawalsMissingAccounts {
                missing: missing.clone(),
            }
        }
        VE::ConwayIncompleteWithdrawals { incomplete } => {
            TxValidationError::IncompleteWithdrawals {
                mismatches: incomplete.clone(),
            }
        }
        VE::PoolRetirementTooLate {
            retirement_epoch,
            current_epoch,
            max_epoch,
            ..
        } => TxValidationError::StakePoolRetirementWrongEpochPOOL {
            gt_expected: current_epoch,
            lt_supplied: retirement_epoch,
            lt_expected: max_epoch,
        },
        VE::PoolRetirementTooEarly {
            retirement_epoch,
            current_epoch,
        } => TxValidationError::StakePoolRetirementWrongEpochPOOL {
            // Upstream folds "too early" and "too late" into ONE constructor
            // carrying both bounds; the early case violates the `RelGT` bound,
            // so `lt_expected` is reported as the retirement epoch itself.
            gt_expected: current_epoch,
            lt_supplied: retirement_epoch,
            lt_expected: retirement_epoch,
        },
        VE::WrongNetworkPool {
            expected,
            actual,
            pool_id,
        } => TxValidationError::WrongNetworkPOOL {
            expected: network_id_to_wire(expected),
            supplied: network_id_to_wire(actual),
            pool_id,
        },
        VE::StakeRegistrationDepositMismatch { declared, expected } => {
            TxValidationError::DepositIncorrectDELEG {
                supplied: declared,
                expected,
            }
        }
        VE::StakeKeyHasNonZeroBalance { balance, .. } => {
            // Upstream's payload is the BALANCE alone — the credential is not
            // part of `StakeKeyHasNonZeroAccountBalanceDELEG`.
            TxValidationError::StakeKeyHasNonZeroAccountBalanceDELEG { balance }
        }
        VE::StakeDeregistrationRefundMismatch { declared, expected } => {
            TxValidationError::RefundIncorrectDELEG {
                supplied: declared,
                expected,
            }
        }
        VE::StakeKeyAlreadyRegistered { credential_hash } => {
            TxValidationError::StakeKeyRegisteredDELEG {
                credential: credential_hash.clone(),
            }
        }
        VE::StakeKeyNotRegisteredForDeregistration { credential_hash } => {
            TxValidationError::StakeKeyNotRegisteredDELEG {
                credential: credential_hash.clone(),
            }
        }
        VE::DelegateePoolNotRegistered { pool_id } => {
            TxValidationError::DelegateeStakePoolNotRegisteredDELEG {
                pool_id: pool_id.clone(),
            }
        }
        VE::StakePoolNotRegisteredForRetirement { pool_id } => {
            TxValidationError::StakePoolNotRegisteredOnKeyPOOL {
                pool_id: pool_id.clone(),
            }
        }
        VE::DRepAlreadyRegistered { credential_hash } => {
            TxValidationError::ConwayDRepAlreadyRegistered {
                credential: credential_hash,
            }
        }
        VE::DRepIncorrectDeposit { declared, expected } => {
            TxValidationError::ConwayDRepIncorrectDeposit {
                supplied: declared,
                expected,
            }
        }
        VE::DRepIncorrectRefund {
            declared, expected, ..
        } => TxValidationError::ConwayDRepIncorrectRefund {
            supplied: declared,
            expected,
        },
        VE::MalformedScriptWitnesses { hashes } => {
            TxValidationError::MalformedScriptWitnessesUTXOW {
                script_hashes: hashes,
            }
        }
        VE::MalformedReferenceScripts { hashes } => {
            TxValidationError::MalformedReferenceScriptsUTXOW {
                script_hashes: hashes,
            }
        }
        VE::DisallowedVotesDuringBootstrap { violations } => {
            TxValidationError::DisallowedVotesDuringBootstrap {
                violations: violations
                    .iter()
                    .map(|(voter, gid)| {
                        let (disc, hex) = voter_to_disc_hex(voter);
                        (disc, hex, gov_action_id_to_string(gid))
                    })
                    .collect(),
            }
        }
        VE::TreasuryWithdrawalReturnAccountsDoNotExist { bad_addrs } => {
            TxValidationError::TreasuryWithdrawalReturnAccountsDoNotExist {
                accounts: bad_addrs,
            }
        }
        VE::InvalidMetadata { .. } => TxValidationError::InvalidMetadataUTXOW,
        VE::ProposalDepositIncorrect { declared, expected } => {
            TxValidationError::ProposalDepositIncorrect { declared, expected }
        }
        VE::CommitteeHasPreviouslyResigned {
            cold_credential_hash,
        } => TxValidationError::ConwayCommitteeHasPreviouslyResigned {
            credential: cold_credential_hash,
        },
        VE::VrfKeyHashAlreadyRegistered {
            vrf_keyhash,
            existing_pool_id,
        } => TxValidationError::VrfKeyHashAlreadyRegisteredPOOL {
            pool_id: existing_pool_id,
            vrf_key_hash: vrf_keyhash,
        },
        VE::StakePoolCostTooLow { actual, minimum } => {
            TxValidationError::StakePoolCostTooLowPOOL {
                supplied: actual,
                expected: minimum,
            }
        }
        VE::PoolRewardAccountWrongNetwork {
            expected,
            actual,
            pool_id,
        } => TxValidationError::WrongNetworkPOOL {
            expected: network_id_to_wire(expected),
            supplied: network_id_to_wire(actual),
            pool_id,
        },
        VE::AuxiliaryDataHashMismatch { declared, computed } => {
            TxValidationError::ConflictingMetadataHashUTXOW {
                supplied: declared,
                expected: computed,
            }
        }
        VE::WrongNetworkInOutput {
            expected, addresses, ..
        } => TxValidationError::WrongNetworkInOutput {
            // The wire shape carries the EXPECTED network plus the offending
            // address set — there is no "actual network" field.
            expected: network_id_to_wire(expected),
            addresses,
        },
        VE::WrongNetworkWithdrawal {
            expected, accounts, ..
        } => TxValidationError::WrongNetworkWithdrawal {
            expected: network_id_to_wire(expected),
            accounts,
        },
        // #1028: both sides are now typed `Option<Hash28>` end to end, so the
        // `StrictMaybe` distinction survives from the predicate to the wire.
        // Previously the ledger carried hex STRINGS and used the empty string as
        // an absence sentinel, which this arm had to re-interpret — and which
        // could not express "the constitution has no guardrail" at all.
        VE::InvalidGuardrailsScriptHash { got, expected } => {
            TxValidationError::InvalidGuardrailsScriptHash {
                got: got.map(|h| h.to_hex()),
                expected: expected.map(|h| h.to_hex()),
            }
        }
        VE::UnspendableUTxONoDatumHash { input, .. } => {
            TxValidationError::UnspendableUTxONoDatumHashUTXOW {
                inputs: vec![input],
            }
        }
        VE::WdrlNotDelegatedToDRep { credential_hash } => {
            TxValidationError::WdrlNotDelegatedToDRep {
                key_hashes: vec![credential_hash],
            }
        }
        VE::MalformedProposal {
            reason,
            proposal_index,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "Governance proposal rejected: malformed PParamsUpdate at proposal \
                 {proposal_index} ({reason})"
            ),
        },
        VE::DisallowedVoters { violations } => TxValidationError::DisallowedVoters {
            violations: violations
                .iter()
                .map(|(voter, action_id)| {
                    let (disc, cred_hex) = voter_to_disc_hex(voter);
                    (disc, cred_hex, gov_action_id_to_string(action_id))
                })
                .collect(),
        },
        VE::VotersDoNotExist { voters } => TxValidationError::VotersDoNotExist {
            voters: voters.iter().map(voter_to_disc_hex).collect(),
        },
        VE::GovActionsDoNotExist { action_ids } => TxValidationError::GovActionsDoNotExist {
            action_ids: action_ids.iter().map(gov_action_id_to_string).collect(),
        },
        VE::InvalidPrevGovActionId {
            action_index,
            action_type,
            prev_action_id,
            proposal,
        } => TxValidationError::InvalidPrevGovActionId {
            action_index,
            action_type: action_type.to_string(),
            prev_action_id: prev_action_id.as_ref().map(gov_action_id_to_string),
            proposal,
        },
        // #1026: PV9 bootstrap proposal-submission restriction, GOV tag 12.
        // Typed from the start — see the encoder arm's note on why a new
        // predicate must not join the #979/#1050 generic-fallback backlog.
        VE::DisallowedProposalDuringBootstrap {
            action_index,
            action_type,
            proposal,
        } => TxValidationError::DisallowedProposalDuringBootstrap {
            action_index,
            action_type: action_type.to_string(),
            proposal,
        },
        // No dedicated wire variant yet (dugite issue #1021, tracked
        // alongside #979's "generic rejections" backlog) — same
        // generic-reason fallback pattern as `MalformedProposal` above. The
        // transaction is still correctly rejected; only the MsgRejectTx
        // REASON is generic rather than a byte-exact `ConwayGovPredFailure`
        // tag-10 frame.
        VE::ProposalCantFollow {
            action_index,
            target_major,
            target_minor,
            base_major,
            base_minor,
            ..
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "ProposalCantFollow: proposal index {action_index} target protocol version \
                 {target_major}.{target_minor} cannot follow base {base_major}.{base_minor}"
            ),
        },
        VE::UnelectedCommitteeVoters { hot_keys } => TxValidationError::UnelectedCommitteeVoters {
            // hot_keys are Hash32 (typed: byte 28 = 0x00 key, 0x01 script).
            // Map each to (disc, credential_hex) where disc=0 for key, 1 for script,
            // and the credential is the first 28 bytes (the actual hash).
            hot_credentials: hot_keys
                .iter()
                .map(|h| {
                    let bytes = h.as_bytes();
                    let disc = if bytes[28] == 0x01 { 1u8 } else { 0u8 };
                    (disc, hex::encode(&bytes[..28]))
                })
                .collect(),
        },
        VE::VotingOnExpiredGovAction { expired_votes } => {
            TxValidationError::VotingOnExpiredGovAction {
                expired_votes: expired_votes
                    .iter()
                    .map(|(voter, action_id, _expires, _current)| {
                        let (disc, cred_hex) = voter_to_disc_hex(voter);
                        (disc, cred_hex, gov_action_id_to_string(action_id))
                    })
                    .collect(),
            }
        }
        VE::ProposalReturnAccountDoesNotExist { bad_addrs } => {
            TxValidationError::ProposalReturnAccountDoesNotExist {
                bad_addrs: bad_addrs.clone(),
            }
        }
        VE::ProposalProcedureNetworkIdMismatch {
            expected,
            mismatched,
        } => match mismatched.first() {
            // Upstream carries ONE offending account per failure
            // (`ProposalProcedureNetworkIdMismatch AccountAddress Network`);
            // dugite aggregates, so the first is reported and the rest are
            // visible in the server-side log.
            Some((account, _)) => TxValidationError::ProposalProcedureNetworkIdMismatch {
                account: account.clone(),
                network: expected,
            },
            None => TxValidationError::ScriptFailed {
                reason: "ProposalProcedureNetworkIdMismatch".to_string(),
            },
        },
        VE::TreasuryWithdrawalsNetworkIdMismatch {
            expected,
            mismatched,
        } => TxValidationError::TreasuryWithdrawalsNetworkIdMismatch {
            accounts: mismatched.into_iter().map(|(a, _)| a).collect(),
            network: expected,
        },
        VE::ZeroTreasuryWithdrawals {
            offending_proposals,
        } => TxValidationError::ScriptFailed {
            reason: format!("ZeroTreasuryWithdrawals: {offending_proposals:?}"),
        },
        VE::ConflictingCommitteeUpdate { conflicts } => {
            TxValidationError::ConflictingCommitteeUpdate {
                credentials: conflicts,
            }
        }
        VE::ExpirationEpochTooSmall { invalid_members } => {
            TxValidationError::ExpirationEpochTooSmall {
                members: invalid_members,
            }
        }
        VE::ExtraRedeemer { tag, index } => match redeemer_tag_to_wire(&tag) {
            Some(t) => TxValidationError::ExtraRedeemersUTXOW {
                purposes: vec![(t, index)],
            },
            None => TxValidationError::ScriptFailed {
                reason: format!("ExtraRedeemer: unknown purpose {tag}"),
            },
        },
        VE::ScriptLockedCollateral { inputs } => TxValidationError::ScriptFailed {
            reason: format!("Collateral input(s) at script-locked addresses: {inputs:?}"),
        },
        VE::ExtraneousScriptWitness { hashes } => {
            TxValidationError::ExtraneousScriptWitnessesUTXOW {
                script_hashes: hashes,
            }
        }
        VE::PoolMedataHashTooBig { pool, hash_size } => {
            TxValidationError::PoolMedataHashTooBigPOOL {
                pool_id: pool,
                size: hash_size as u64,
            }
        }
        VE::OutputBootAddrAttrsTooBig { oversized_outputs } => TxValidationError::ScriptFailed {
            reason: format!("OutputBootAddrAttrsTooBig: {oversized_outputs:?}"),
        },
        VE::MIRCertificateTooLateInEpoch {
            current_slot,
            deadline,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "MIRCertificateTooLateInEpoch: current_slot={current_slot}, deadline={deadline}"
            ),
        },
        VE::InsufficientForInstantaneousRewards {
            pot,
            required,
            available,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "InsufficientForInstantaneousRewards: pot={pot:?}, required={required}, available={available}"
            ),
        },
        VE::MIRTransferNotCurrentlyAllowed => TxValidationError::ScriptFailed {
            reason: "MIRTransferNotCurrentlyAllowed (pre-Alonzo MIR pot-to-pot transfer)"
                .to_string(),
        },
        VE::MIRNegativesNotCurrentlyAllowed => TxValidationError::ScriptFailed {
            reason: "MIRNegativesNotCurrentlyAllowed (pre-Alonzo negative MIR delta)".to_string(),
        },
        VE::MIRProducesNegativeUpdate { credentials } => TxValidationError::ScriptFailed {
            reason: format!("MIRProducesNegativeUpdate: credentials={credentials:?}"),
        },
        VE::InsufficientForTransferDELEG {
            pot,
            requested,
            available,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "InsufficientForTransferDELEG: pot={pot:?}, requested={requested}, available={available}"
            ),
        },
        VE::MIRNegativeTransfer { pot, amount } => TxValidationError::ScriptFailed {
            reason: format!("MIRNegativeTransfer: pot={pot:?}, amount={amount}"),
        },
        // #804: Shelley UTXOW `MIRInsufficientGenesisSigsUTXOW` — a
        // whole-transaction check (not a per-cert DELEG predicate like the
        // MIR variants above), but riding the same `ScriptFailed` wire
        // variant for the same reason as `Phase2CollectError`/
        // `Phase2EvalPanic` below: no dedicated N2C wire tag exists for it
        // yet, and the reason string carries full diagnostic detail.
        VE::MIRInsufficientGenesisSigs {
            present,
            required,
            signers,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "MIRInsufficientGenesisSigsUTXOW: present={present}, required={required}, signers={signers:?}"
            ),
        },
        VE::NonGenesisUpdatePPUP { proposed, genesis } => TxValidationError::ScriptFailed {
            reason: format!(
                "NonGenesisUpdatePPUP: proposed_keys not subset of genesis_delegates \
                 ({proposed:?} ∉ {genesis:?})"
            ),
        },
        VE::PPUpdateWrongEpoch {
            current,
            target,
            period,
        } => TxValidationError::ScriptFailed {
            reason: format!(
                "PPUpdateWrongEpoch: current={current}, target={target}, period={period:?}"
            ),
        },
        VE::PVCannotFollowPPUP { bad_pv } => TxValidationError::ScriptFailed {
            reason: format!("PVCannotFollowPPUP: bad_pv={bad_pv:?}"),
        },
        VE::DRepNotRegistered { credential_hash } => {
            TxValidationError::DRepNotRegistered { credential_hash }
        }
        VE::PoolMarginInvalid {
            numerator,
            denominator,
        } => TxValidationError::PoolMarginInvalid {
            numerator,
            denominator,
        },
        VE::InvalidRewardAccount(msg) => TxValidationError::ScriptFailed {
            reason: format!("InvalidRewardAccount: {msg}"),
        },
        VE::DelegateeDRepNotRegistered { drep_id } => {
            TxValidationError::DelegateeDRepNotRegisteredDELEG {
                credential: drep_id.clone(),
            }
        }
        VE::StakeKeyNotRegisteredForDelegation { credential_hash } => {
            TxValidationError::StakeKeyNotRegisteredDELEG {
                credential: credential_hash.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
    use dugite_primitives::value::Lovelace;

    // ─── convert_validation_error ────────────────────────────────────────────
    //
    // The N2C tx-submission path calls `convert_validation_error` on every
    // ledger rejection.  cardano-cli surfaces the resulting `TxValidationError`
    // discriminant to operators and to scripts that match on it, so the
    // ledger→network mapping is part of our wire contract.  A regression here
    // would silently rebrand "fee too small" rejections as generic script
    // failures (etc.) — exactly the class of bug that bites operators in
    // production.

    /// #1053 WIRING test. The typed `ValidationTagMismatch` encoder arm is
    /// worthless if this mapping still points `VE::ScriptFailed` at the free-text
    /// catch-all — #977's exact failure mode, where the fix landed in a path
    /// nothing reached.
    ///
    /// The assertion is deliberately two-sided: the phase-2 failure must become
    /// the dedicated carrier, AND a rejection that merely BORROWS the catch-all
    /// (`RefScriptsSizeTooBig`) must NOT, because a typed phase-2 class stamped
    /// on it would be a confident lie (#979).
    #[test]
    fn convert_validation_error_routes_phase2_failure_to_its_own_carrier() {
        use dugite_ledger::validation::ValidationError as VE;

        let e = convert_validation_error(VE::ScriptFailed(
            "Plutus evaluation failed: script returned Error term".to_string(),
        ));
        match e {
            TxValidationError::Phase2ScriptsFailedUnexpectedly { messages } => {
                assert_eq!(messages.len(), 1, "one message per failed script");
                assert!(
                    messages[0].contains("script returned Error term"),
                    "the evaluator's diagnostic must survive: {:?}",
                    messages[0]
                );
            }
            other => panic!(
                "VE::ScriptFailed must map to the dedicated phase-2 carrier so it \
                 reaches the client as a typed ValidationTagMismatch; got {other:?}"
            ),
        }

        // The catch-all must keep its other tenants.
        let e = convert_validation_error(VE::RefScriptsSizeTooBig {
            maximum: 204_800,
            actual: 300_000,
        });
        assert!(
            matches!(e, TxValidationError::ScriptFailed { .. }),
            "RefScriptsSizeTooBig is NOT a phase-2 script failure and must not \
             acquire the phase-2 wire class; got {e:?}"
        );
    }

    #[test]
    fn convert_validation_error_preserves_known_variants() {
        use dugite_ledger::validation::ValidationError as VE;

        // Structural pass-throughs: discriminant + payload must be preserved.
        let e = convert_validation_error(VE::FeeTooSmall {
            minimum: 100,
            actual: 50,
        });
        assert!(matches!(
            e,
            TxValidationError::FeeTooSmall {
                minimum: 100,
                actual: 50
            }
        ));

        let e = convert_validation_error(VE::TxTooLarge {
            maximum: 16384,
            actual: 20000,
        });
        assert!(matches!(
            e,
            TxValidationError::TxTooLarge {
                maximum: 16384,
                actual: 20000
            }
        ));

        let e = convert_validation_error(VE::ValueNotConserved {
            inputs: 10,
            outputs: 9,
            fee: 0,
        });
        assert!(matches!(
            e,
            TxValidationError::ValueNotConserved {
                inputs: 10,
                outputs: 9,
                fee: 0
            }
        ));

        let e = convert_validation_error(VE::InsufficientCollateral {
            balance: -500,
            required: 1_500_000,
        });
        assert!(matches!(
            e,
            TxValidationError::InsufficientCollateral {
                balance: -500,
                required: 1_500_000,
            }
        ));

        // `VE::CollateralHasTokens` must carry the offending `Value` straight
        // through to the wire-facing variant — the whole point of #1050 is
        // that a plain reason string can no longer represent this payload.
        let mut multi_asset = std::collections::BTreeMap::new();
        multi_asset.insert(
            dugite_primitives::hash::Hash28::from_bytes([0x11; 28]),
            std::collections::BTreeMap::from([(
                dugite_primitives::value::AssetName(b"x".to_vec()),
                7u64,
            )]),
        );
        let value = dugite_primitives::value::Value {
            coin: dugite_primitives::value::Lovelace(123),
            multi_asset,
        };
        let e = convert_validation_error(VE::CollateralHasTokens(value.clone()));
        match e {
            TxValidationError::CollateralHasTokens { value: got } => {
                assert_eq!(
                    got, value,
                    "the exact offending Value must survive conversion"
                );
            }
            other => panic!("expected CollateralHasTokens, got {other:?}"),
        }

        let e = convert_validation_error(VE::NoInputs);
        assert!(matches!(e, TxValidationError::NoInputs));

        let e = convert_validation_error(VE::ValueOverflow);
        assert!(matches!(e, TxValidationError::ValueOverflow));

        // IsValidTagMismatch (#522) must map 1:1 to the network variant so
        // the rejection reason is visible to the submitting client.
        let e = convert_validation_error(VE::IsValidTagMismatch {
            declared: false,
            evaluated: true,
        });
        assert!(
            matches!(
                e,
                TxValidationError::IsValidTagMismatch {
                    declared: false,
                    evaluated: true
                }
            ),
            "IsValidTagMismatch must map 1:1 to network variant, got {e:?}"
        );
    }

    /// #979: the predicates that remain generic do so DELIBERATELY, because
    /// Conway has no counterpart for them — not because nobody got round to
    /// them. Each case below names why.
    #[test]
    fn conway_predicates_without_a_haskell_counterpart_stay_generic() {
        use dugite_ledger::validation::ValidationError as VE;

        let cases: Vec<(VE, &str)> = vec![
            (
                // `ZeroWithdrawal` has no constructor anywhere in the Conway
                // failure tree. dugite raises it as a defensive check.
                VE::ZeroWithdrawal {
                    account: "stake_test1abc".to_string(),
                },
                "no Conway constructor",
            ),
            (
                // MIR certificates were REMOVED in Conway, so every MIR
                // failure is structurally unreachable for a Conway tx. The
                // Shelley constructors still exist upstream but cannot be
                // produced by the Conway LEDGER rule.
                VE::MIRNegativesNotCurrentlyAllowed,
                "MIR certs do not exist in Conway",
            ),
            (
                VE::MIRTransferNotCurrentlyAllowed,
                "MIR certs do not exist in Conway",
            ),
            (
                // The PPUP rule was removed in Conway (replaced by governance
                // actions), so its failures are likewise unreachable.
                VE::PVCannotFollowPPUP { bad_pv: (12, 0) },
                "PPUP rule removed in Conway",
            ),
            (
                // dugite-specific era gating with no upstream analogue: Haskell
                // enforces era shape at the DECODER, before any rule runs.
                VE::GovernancePreConway { current_version: 8 },
                "dugite-specific era gate",
            ),
        ];
        for (v, why) in cases {
            let mapped = convert_validation_error(v);
            assert!(
                matches!(mapped, TxValidationError::ScriptFailed { .. }),
                "expected a deliberate generic failure ({why}), got {mapped:?}"
            );
        }
    }

    /// #979 acceptance criterion 3: the remaining generic failures are a
    /// **closed, justified set**.
    ///
    /// This scans the mapping table itself and requires every arm that still
    /// produces `ScriptFailed` to appear below with a reason. Adding a new
    /// generic arm fails this test until it is justified, and typing one
    /// requires removing it from the list — so the set cannot drift in either
    /// direction.
    ///
    /// The reasons fall into four kinds, and only the third and fourth are
    /// outstanding work:
    ///
    /// * **Removed in Conway** — the rule that raised the failure no longer
    ///   exists, so the failure is structurally unreachable for a Conway
    ///   transaction. MIR certificates and the PPUP rule are both gone.
    /// * **dugite-specific** — a defensive check with no upstream analogue;
    ///   Haskell rejects the same transaction earlier, usually at the decoder.
    /// * **Payload insufficient** — a counterpart EXISTS, but dugite's error
    ///   does not carry what the wire needs (a whole `GovAction`, a `TxOut`,
    ///   a `UTxO` map, a script hash). Emitting a typed frame with an empty or
    ///   invented payload would reach cardano-cli as `DeserialiseFailure`,
    ///   which is strictly worse than the generic error. These need the ledger
    ///   error enriched first, exactly as #979 did for the four that were.
    /// * **No wire arm yet** — unlike "payload insufficient," the ledger
    ///   error already carries everything a typed frame would need; only the
    ///   CBOR encoder arm itself hasn't been written. Newly-added predicates
    ///   (found by this Conway-Phase-1 validation audit) land here until a
    ///   follow-up wires the encoder — tracked alongside #979.
    ///
    ///   `#1025` enriched five MORE of these — `MissingDatumWitness`,
    ///   `ExtraDatumWitness`, `OutputBootAddrAttrsTooBig`,
    ///   `ScriptLockedCollateral`, `ZeroTreasuryWithdrawals` — but WITHOUT
    ///   touching `dugite-ledger` (a concurrent session owns it): the
    ///   missing data is re-derived one layer up, in
    ///   `enrich_validation_errors`, from the already-decoded `tx` (and,
    ///   for collateral, the UTxO view Phase-1 validation already used).
    ///   Those five variants STILL appear in the list below, mapping to
    ///   `ScriptFailed`, exactly as before — `enrich_validation_errors`
    ///   intercepts them before they would ever reach
    ///   `convert_validation_error`, so their entries here are now the
    ///   FALLBACK for the rare case the enrichment itself can't build a
    ///   faithful payload (see each one's updated reason below), not the
    ///   normal path. This test only scans `convert_validation_error`'s own
    ///   source and cannot see that upstream interception — a live
    ///   round-trip is the only way to observe it, which is why #1025's
    ///   verification is a wire-byte assertion test on
    ///   `enrich_validation_errors` directly, not just this guard.
    #[test]
    fn remaining_generic_failures_are_a_closed_justified_set() {
        // (variant, why it is still generic)
        const JUSTIFIED: &[(&str, &str)] = &[
            // ── Removed in Conway: structurally unreachable ──
            (
                "MIRCertificateTooLateInEpoch",
                "MIR certs removed in Conway",
            ),
            ("MIRInsufficientGenesisSigs", "MIR certs removed in Conway"),
            ("MIRNegativeTransfer", "MIR certs removed in Conway"),
            (
                "MIRNegativesNotCurrentlyAllowed",
                "MIR certs removed in Conway",
            ),
            ("MIRProducesNegativeUpdate", "MIR certs removed in Conway"),
            (
                "MIRTransferNotCurrentlyAllowed",
                "MIR certs removed in Conway",
            ),
            (
                "InsufficientForInstantaneousRewards",
                "MIR certs removed in Conway",
            ),
            (
                "InsufficientForTransferDELEG",
                "MIR certs removed in Conway",
            ),
            ("NonGenesisUpdatePPUP", "PPUP rule removed in Conway"),
            ("PPUpdateWrongEpoch", "PPUP rule removed in Conway"),
            ("PVCannotFollowPPUP", "PPUP rule removed in Conway"),
            // ── dugite-specific: no upstream analogue ──
            (
                "EraGatingViolation",
                "dugite era gate; Haskell rejects at the decoder",
            ),
            (
                "GovernancePreConway",
                "dugite era gate; Haskell rejects at the decoder",
            ),
            (
                "InvalidRewardAccount",
                "dugite parse guard; no Conway constructor",
            ),
            ("ZeroWithdrawal", "no Conway constructor"),
            // `ScriptFailed` was here as "the generic failure itself" until
            // #1053. It is now the ONLY ledger error meaning "phase-2 evaluation
            // ran and disagreed with a Phase2Valid tag", and it maps to
            // `Phase2ScriptsFailedUnexpectedly` → the typed
            // `ValidationTagMismatch … FailedUnexpectedly`. Re-adding it here
            // would mean the typed arm had been unwired.
            // ── Payload insufficient: counterpart exists, data does not ──
            (
                "MalformedProposal",
                "GOV 1 FIXED by `enrich_validation_errors` (#1025): the ValidationError now \
                 carries the offending proposal's INDEX, so the whole GovAction is a direct \
                 positional lookup into `tx.body.proposal_procedures` — no need to re-derive \
                 `ppuWellFormed` in a second place. This arm is the fallback for an \
                 out-of-range index.",
            ),
            (
                "ZeroTreasuryWithdrawals",
                "GOV 15's real shape is FIXED by `enrich_validation_errors` (#1025), which \
                 re-derives the zero-sum condition directly from \
                 `tx.body.proposal_procedures` and emits one `ZeroTreasuryWithdrawalsGOV` \
                 per offending proposal. This arm in `convert_validation_error` itself is \
                 unreachable in practice (the enrichment always finds at least one match \
                 whenever this `ValidationError` fires) but stays generic as the \
                 structural fallback.",
            ),
            (
                "MissingDatumWitness",
                "UTXOW 11 FIXED by `enrich_validation_errors` (#1025): aggregates every \
                 per-hash occurrence plus a fresh witness-set derivation into one \
                 `MissingRequiredDatumsUTXOW`. This arm is the fallback for the rare case \
                 the raw per-element CBOR spans aren't preserved (see \
                 `supplied_datum_hashes`).",
            ),
            (
                "ExtraDatumWitness",
                "UTXOW 12 FIXED by `enrich_validation_errors` (#1025), same shape as \
                 MissingDatumWitness above but always derivable (no raw-span dependency) \
                 — this arm should be effectively unreachable, kept as the structural \
                 fallback.",
            ),
            (
                "MissingRedeemer",
                "UTXOW 10 FIXED by `enrich_validation_errors` (#1025): the ValidationError \
                 now carries the ScriptHash (every raise site already had it), and the \
                 `AsItem` value is re-derived from `tx.body` using the same deterministic \
                 ordering the raise site indexed by. This arm is the fallback for an \
                 unresolvable (tag, index) pair — a frame naming the wrong item would be \
                 worse than a free-text reason.",
            ),
            (
                "OutputBootAddrAttrsTooBig",
                "UTXO 10 FIXED by `enrich_validation_errors` (#1025): tx-body outputs are \
                 an ordered sequence, so the indices dugite already carries are a direct \
                 positional lookup into `tx.body.outputs`. This arm is the fallback for \
                 an out-of-range index.",
            ),
            (
                "ScriptLockedCollateral",
                "UTXO 13 FIXED by `enrich_validation_errors` (#1025): each offending TxIn \
                 is resolved to its TxOut via the SAME UTxO view Phase-1 validation \
                 already used. This arm is the fallback for an unresolvable input.",
            ),
            (
                "Phase2CollectError",
                "UTXOS 1 needs the structured CollectError list",
            ),
            (
                "Phase2EvalPanic",
                "no upstream constructor for an evaluator panic",
            ),
            // ── Typed on the happy path; generic only when unencodable ──
            (
                "ExtraRedeemer",
                "typed unless the purpose name is unrecognised",
            ),
            (
                "ProposalProcedureNetworkIdMismatch",
                "typed unless the offender list is empty",
            ),
            // ── No wire arm yet: payload is complete, encoder isn't written ──
            (
                "ProposalCantFollow",
                "GOV 10 newly implemented (#1021); payload complete, no CBOR arm yet",
            ),
            (
                "RefScriptsSizeTooBig",
                "LEDGER 6 newly implemented (#1024); payload complete, no CBOR arm yet",
            ),
        ];

        let src = include_str!("lib.rs");
        let start = src
            .find("pub fn convert_validation_error(")
            .expect("mapping function present");
        // Bound the scan to the mapping function.
        let body = &src[start..];
        // The mapping function is the last item before the test module in this file (it was
        // followed by a `///` item in serve.rs), so bound the scan at either marker.
        let end = [body.find("\n/// "), body.find("\n#[cfg(test)]")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(body.len());
        let body = &body[..end];

        // Comments are STRIPPED before scanning. A comment that merely NAMES
        // `TxValidationError::ScriptFailed` — for instance to explain why a
        // neighbouring arm deliberately does NOT use it (#1053) — would
        // otherwise be attributed to the arm it sits after and reported as an
        // unjustified degradation. The guard must measure code, not prose.
        let body: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .map(|l| format!("{l}\n"))
            .collect();
        let body = body.as_str();

        let mut found: Vec<&str> = Vec::new();
        let arms: Vec<(usize, &str)> = body
            .match_indices("\n        VE::")
            .map(|(i, _)| {
                let rest = &body[i + "\n        VE::".len()..];
                let name_end = rest
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                (i, &rest[..name_end])
            })
            .collect();
        for (k, (pos, name)) in arms.iter().enumerate() {
            let stop = arms.get(k + 1).map(|(p, _)| *p).unwrap_or(body.len());
            if body[*pos..stop].contains("TxValidationError::ScriptFailed") {
                found.push(name);
            }
        }
        found.sort_unstable();
        found.dedup();

        let justified: std::collections::BTreeSet<&str> =
            JUSTIFIED.iter().map(|(n, _)| *n).collect();
        let found_set: std::collections::BTreeSet<&str> = found.iter().copied().collect();

        let unjustified: Vec<&&str> = found_set.difference(&justified).collect();
        assert!(
            unjustified.is_empty(),
            "these still degrade to ScriptFailed with no recorded reason: {unjustified:?}\n\
             Either give them a typed arm, or add them to JUSTIFIED with why."
        );

        let stale: Vec<&&str> = justified.difference(&found_set).collect();
        assert!(
            stale.is_empty(),
            "these are listed as deliberately generic but are no longer generic: {stale:?}\n\
             Remove them from JUSTIFIED — a stale entry hides the next regression."
        );
    }

    // ══ #1025: `enrich_validation_errors` ══════════════════════════════

    fn minimal_tx(body: dugite_primitives::transaction::TransactionBody) -> Transaction {
        Transaction {
            hash: Hash32::from_bytes([0u8; 32]),
            era: dugite_primitives::era::Era::Conway,
            body,
            witness_set: dugite_primitives::transaction::TransactionWitnessSet::default(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    struct EmptyUtxo;
    impl dugite_ledger::utxo::UtxoLookup for EmptyUtxo {
        fn lookup(
            &self,
            _input: &dugite_primitives::transaction::TransactionInput,
        ) -> Option<dugite_primitives::transaction::TransactionOutput> {
            None
        }
    }

    use dugite_ledger::validation::ValidationError as VE;
    use dugite_primitives::transaction::Transaction;

    /// `ZeroTreasuryWithdrawalsGOV` must be re-derived from
    /// `tx.body.proposal_procedures` (Haskell needs the whole `GovAction`,
    /// which dugite's `ValidationError` never carries — #1025).
    #[test]
    fn enrich_zero_treasury_withdrawals_rederives_from_tx_body() {
        let mut withdrawals = std::collections::BTreeMap::new();
        withdrawals.insert(vec![0x55u8; 29], Lovelace(0));
        let mut body = dugite_primitives::transaction::TransactionBody::default();
        body.proposal_procedures.push(ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0xAAu8; 29],
            gov_action: GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash: None,
            },
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        });
        let tx = minimal_tx(body);

        let errors = vec![VE::ZeroTreasuryWithdrawals {
            offending_proposals: vec!["placeholder".to_string()],
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        assert!(
            matches!(
                &mapped[0],
                TxValidationError::ZeroTreasuryWithdrawalsGOV { withdrawals, policy_hash }
                    if withdrawals.len() == 1 && withdrawals[0].1 == 0 && policy_hash.is_none()
            ),
            "expected a real ZeroTreasuryWithdrawalsGOV rebuilt from tx.body, got {:?}",
            mapped[0]
        );
    }

    /// The PV==9 bootstrap skip (`hardforkConwayBootstrapPhase`) must carry
    /// over into the re-derivation, or a bootstrap-phase tx would get a
    /// typed rejection Haskell would never produce.
    #[test]
    fn enrich_zero_treasury_withdrawals_respects_pv9_bootstrap_skip() {
        let mut withdrawals = std::collections::BTreeMap::new();
        withdrawals.insert(vec![0x55u8; 29], Lovelace(0));
        let mut body = dugite_primitives::transaction::TransactionBody::default();
        body.proposal_procedures.push(ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0xAAu8; 29],
            gov_action: GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash: None,
            },
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        });
        let tx = minimal_tx(body);

        let errors = vec![VE::ZeroTreasuryWithdrawals {
            offending_proposals: vec!["placeholder".to_string()],
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 9);
        assert_eq!(mapped.len(), 1);
        assert!(
            matches!(&mapped[0], TxValidationError::ScriptFailed { .. }),
            "PV==9 bootstrap must fall through to the generic arm (no offender found \
             at PV 9, matching `is_treasury_withdrawals_zero_sum`'s bootstrap skip), \
             got {:?}",
            mapped[0]
        );
    }

    /// `OutputBootAddrAttrsTooBigUTXO` must be built from a direct
    /// positional lookup into `tx.body.outputs` — no UTxO join needed since
    /// outputs are the tx's own ordered sequence.
    #[test]
    fn enrich_output_boot_addr_attrs_too_big_indexes_tx_body_outputs() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{OutputDatum, TransactionOutput};
        use dugite_primitives::value::Value;

        let output = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x11; 28])),
            }),
            value: Value {
                coin: Lovelace(1_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: Some(vec![0x82, 0x01, 0x02]),
        };
        let mut body = dugite_primitives::transaction::TransactionBody::default();
        body.outputs.push(output);
        let tx = minimal_tx(body);

        let errors = vec![VE::OutputBootAddrAttrsTooBig {
            oversized_outputs: vec![0],
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            TxValidationError::OutputBootAddrAttrsTooBigUTXO { outputs_raw_cbor } => {
                assert_eq!(outputs_raw_cbor, &vec![hex::encode([0x82, 0x01, 0x02])]);
            }
            other => panic!("expected OutputBootAddrAttrsTooBigUTXO, got {other:?}"),
        }
    }

    /// An out-of-range index must NOT panic or silently drop the failure —
    /// it falls through to the generic arm rather than shipping a
    /// truncated `outputs_raw_cbor` list.
    #[test]
    fn enrich_output_boot_addr_attrs_too_big_falls_back_on_bad_index() {
        let tx = minimal_tx(dugite_primitives::transaction::TransactionBody::default());
        let errors = vec![VE::OutputBootAddrAttrsTooBig {
            oversized_outputs: vec![0], // tx has ZERO outputs
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        assert!(matches!(&mapped[0], TxValidationError::ScriptFailed { .. }));
    }

    /// `BabbageOutputTooSmallUTxO` must aggregate every offending
    /// `ValidationError::OutputTooSmall` occurrence from ONE
    /// `validate_transaction` call into a single typed arm, re-encoding
    /// each offending output via its `output_index` — mirroring Haskell's
    /// per-tx (not per-output) `NonEmpty` aggregation.
    #[test]
    fn enrich_output_too_small_aggregates_multiple_outputs_by_index() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{OutputDatum, TransactionOutput};
        use dugite_primitives::value::Value;

        let make_output = |raw: Vec<u8>, coin: u64| TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x11; 28])),
            }),
            value: Value {
                coin: Lovelace(coin),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: Some(raw),
        };
        let mut body = dugite_primitives::transaction::TransactionBody::default();
        body.outputs.push(make_output(vec![0x82, 0x01, 0x02], 1)); // index 0
        body.outputs
            .push(make_output(vec![0x82, 0x03, 0x04], 999_999)); // index 1, NOT offending
        body.outputs.push(make_output(vec![0x82, 0x05, 0x06], 2)); // index 2
        let tx = minimal_tx(body);

        let errors = vec![
            VE::OutputTooSmall {
                minimum: 1_000_000,
                actual: 1,
                output_index: 0,
            },
            VE::OutputTooSmall {
                minimum: 2_000_000,
                actual: 2,
                output_index: 2,
            },
        ];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            TxValidationError::BabbageOutputTooSmallUTxO { outputs } => {
                assert_eq!(
                    outputs,
                    &vec![
                        (hex::encode([0x82, 0x01, 0x02]), 1_000_000),
                        (hex::encode([0x82, 0x05, 0x06]), 2_000_000),
                    ]
                );
            }
            other => panic!("expected BabbageOutputTooSmallUTxO, got {other:?}"),
        }
    }

    /// The sentinel index resolves to `body.collateral_return` — Haskell
    /// folds the collateral-return output into the SAME
    /// `BabbageOutputTooSmallUTxO` via `allSizedOutputsTxBodyF`, and 18d in
    /// the tx-zoo pins this on the wire.
    #[test]
    fn enrich_output_too_small_resolves_collateral_return_sentinel() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{OutputDatum, TransactionOutput};
        use dugite_primitives::value::Value;

        let body = dugite_primitives::transaction::TransactionBody {
            collateral_return: Some(TransactionOutput {
                address: Address::Enterprise(EnterpriseAddress {
                    network: NetworkId::Mainnet,
                    payment: Credential::VerificationKey(Hash28::from_bytes([0x22; 28])),
                }),
                value: Value {
                    coin: Lovelace(1),
                    multi_asset: Default::default(),
                },
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: Some(vec![0x82, 0x0a, 0x0b]),
            }),
            ..Default::default()
        };
        let tx = minimal_tx(body);

        let errors = vec![VE::OutputTooSmall {
            minimum: 1_500_000,
            actual: 1,
            output_index: dugite_ledger::validation::COLLATERAL_RETURN_OUTPUT_INDEX,
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            TxValidationError::BabbageOutputTooSmallUTxO { outputs } => {
                assert_eq!(outputs, &vec![(hex::encode([0x82, 0x0a, 0x0b]), 1_500_000)]);
            }
            other => panic!("expected BabbageOutputTooSmallUTxO, got {other:?}"),
        }
    }

    /// An out-of-range `output_index` must NOT panic or silently drop the
    /// failure — it falls through to `convert_validation_error`'s existing
    /// (unenriched) `TxValidationError::OutputTooSmall` mapping rather than
    /// shipping a truncated `outputs` list. Unlike `OutputBootAddrAttrsTooBig`
    /// above, `OutputTooSmall` already has a direct (if wire-unencoded)
    /// `TxValidationError` counterpart, so the fallback here is that
    /// variant, not `ScriptFailed`.
    #[test]
    fn enrich_output_too_small_falls_back_on_bad_index() {
        let tx = minimal_tx(dugite_primitives::transaction::TransactionBody::default());
        let errors = vec![VE::OutputTooSmall {
            minimum: 1_000_000,
            actual: 1,
            output_index: 0, // tx has ZERO outputs
        }];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        assert!(matches!(
            &mapped[0],
            TxValidationError::OutputTooSmall {
                minimum: 1_000_000,
                actual: 1,
            }
        ));
    }

    /// `MissingRequiredDatumsUTXOW`'s "provided" set needs the preserved raw
    /// CBOR spans; without them (`raw_plutus_data_cbor: None`, e.g. from a
    /// re-serialized tx), enrichment must decline rather than guess, and the
    /// occurrence falls through to the generic arm.
    #[test]
    fn enrich_missing_datum_witness_falls_back_without_raw_spans() {
        let tx = minimal_tx(dugite_primitives::transaction::TransactionBody::default());
        let errors = vec![VE::MissingDatumWitness("11".repeat(32))];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        assert!(matches!(&mapped[0], TxValidationError::ScriptFailed { .. }));
    }

    /// `NotAllowedSupplementalDatumsUTXOW`'s second field never needs a raw
    /// span — `OutputDatum::DatumHash` already carries the hash directly —
    /// so this must ALWAYS build the typed arm when the error fires.
    #[test]
    fn enrich_extra_datum_witness_derives_allowed_set_from_output_datums() {
        use dugite_primitives::address::{Address, EnterpriseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::DatumHash;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::transaction::{OutputDatum, TransactionOutput};
        use dugite_primitives::value::Value;

        let dh = DatumHash::from_bytes([0x77u8; 32]);
        let output = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: dugite_primitives::network::NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x11; 28])),
            }),
            value: Value {
                coin: Lovelace(1_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::DatumHash(dh),
            script_ref: None,
            is_legacy: false,
            raw_cbor: Some(vec![0x82, 0x01, 0x02]),
        };
        let mut body = dugite_primitives::transaction::TransactionBody::default();
        body.outputs.push(output);
        let tx = minimal_tx(body);

        let errors = vec![VE::ExtraDatumWitness(dh.to_hex())];
        let mapped = enrich_validation_errors(errors, &tx, &EmptyUtxo, 10);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            TxValidationError::NotAllowedSupplementalDatumsUTXOW { extra, allowed } => {
                assert_eq!(extra, &vec![dh.to_hex()]);
                assert_eq!(allowed, &vec![dh.to_hex()]);
            }
            other => panic!("expected NotAllowedSupplementalDatumsUTXOW, got {other:?}"),
        }
    }

    /// `ScriptsNotPaidUTxOUTXO` must resolve each offending TxIn against
    /// the SAME UTxO view Phase-1 validation already used, and decline
    /// (fall through to generic) when the input can't be resolved rather
    /// than shipping a partial map.
    #[test]
    fn enrich_script_locked_collateral_resolves_against_utxo_view() {
        struct OneUtxo;
        impl dugite_ledger::utxo::UtxoLookup for OneUtxo {
            fn lookup(
                &self,
                input: &dugite_primitives::transaction::TransactionInput,
            ) -> Option<dugite_primitives::transaction::TransactionOutput> {
                use dugite_primitives::address::{Address, EnterpriseAddress};
                use dugite_primitives::credentials::Credential;
                use dugite_primitives::hash::Hash28;
                use dugite_primitives::transaction::{OutputDatum, TransactionOutput};
                use dugite_primitives::value::Value;
                if input.index == 0 {
                    Some(TransactionOutput {
                        address: Address::Enterprise(EnterpriseAddress {
                            network: dugite_primitives::network::NetworkId::Mainnet,
                            payment: Credential::Script(Hash28::from_bytes([0x22; 28])),
                        }),
                        value: Value {
                            coin: Lovelace(5_000_000),
                            multi_asset: Default::default(),
                        },
                        datum: OutputDatum::None,
                        script_ref: None,
                        is_legacy: false,
                        raw_cbor: Some(vec![0x82, 0x03, 0x04]),
                    })
                } else {
                    None
                }
            }
        }

        let tx = minimal_tx(dugite_primitives::transaction::TransactionBody::default());
        let resolvable_input = format!("{}#0", hex::encode([0x99u8; 32]));
        let unresolvable_input = format!("{}#1", hex::encode([0x99u8; 32]));

        // Resolvable case.
        let errors = vec![VE::ScriptLockedCollateral {
            inputs: vec![resolvable_input.clone()],
        }];
        let mapped = enrich_validation_errors(errors, &tx, &OneUtxo, 10);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            TxValidationError::ScriptsNotPaidUTxOUTXO { inputs_outputs } => {
                assert_eq!(inputs_outputs.len(), 1);
                assert_eq!(inputs_outputs[0].0, resolvable_input);
                assert_eq!(inputs_outputs[0].1, hex::encode([0x82, 0x03, 0x04]));
            }
            other => panic!("expected ScriptsNotPaidUTxOUTXO, got {other:?}"),
        }

        // Unresolvable case — must decline, not emit a partial map.
        let errors = vec![VE::ScriptLockedCollateral {
            inputs: vec![unresolvable_input],
        }];
        let mapped = enrich_validation_errors(errors, &tx, &OneUtxo, 10);
        assert_eq!(mapped.len(), 1);
        assert!(matches!(&mapped[0], TxValidationError::ScriptFailed { .. }));
    }

    /// #979 acceptance criterion 4 — the PV inversion.
    ///
    /// `hardforkConwayDELEGIncorrectDepositsAndRefunds` is `pvMajor > 10`, so
    /// the same ledger error must produce DIFFERENT wire failures either side
    /// of PV 11. Getting this backwards is #978 exactly: the reachable case
    /// degrades while the implemented arm is dead code.
    #[test]
    fn deleg_deposit_and_refund_are_pv_gated() {
        use dugite_ledger::validation::ValidationError as VE;

        for pv in [9u64, 10] {
            assert!(
                matches!(
                    convert_validation_error_at_pv(
                        VE::StakeRegistrationDepositMismatch {
                            declared: 1,
                            expected: 2
                        },
                        pv
                    ),
                    TxValidationError::IncorrectDepositDELEG { supplied: 1 }
                ),
                "PV{pv}: deposit mismatch must use the pre-PV11 constructor"
            );
            assert!(
                matches!(
                    convert_validation_error_at_pv(
                        VE::StakeDeregistrationRefundMismatch {
                            declared: 3,
                            expected: 4
                        },
                        pv
                    ),
                    // Pre-PV11 the REFUND is reported through the same
                    // `IncorrectDepositDELEG` constructor — there is no
                    // separate refund tag before the split.
                    TxValidationError::IncorrectDepositDELEG { supplied: 3 }
                ),
                "PV{pv}: refund mismatch must also use IncorrectDepositDELEG"
            );
        }

        for pv in [11u64, 12] {
            assert!(matches!(
                convert_validation_error_at_pv(
                    VE::StakeRegistrationDepositMismatch {
                        declared: 1,
                        expected: 2
                    },
                    pv
                ),
                TxValidationError::DepositIncorrectDELEG {
                    supplied: 1,
                    expected: 2
                }
            ));
            assert!(matches!(
                convert_validation_error_at_pv(
                    VE::StakeDeregistrationRefundMismatch {
                        declared: 3,
                        expected: 4
                    },
                    pv
                ),
                TxValidationError::RefundIncorrectDELEG {
                    supplied: 3,
                    expected: 4
                }
            ));
        }
    }

    /// #979: the counterpart-bearing predicates must NOT be generic any more.
    ///
    /// This is the direction that regressed for years — an arm quietly falling
    /// through to `ScriptFailed` reaches cardano-cli as
    /// `ConwayMempoolFailure "transaction validation failed"`, which the
    /// bidirectional parity oracle scores CLASSDIFF the moment a test
    /// exercises it.
    #[test]
    fn predicates_with_a_haskell_counterpart_are_typed() {
        use dugite_ledger::validation::ValidationError as VE;

        let cases: Vec<VE> = vec![
            VE::StakeKeyAlreadyRegistered {
                credential_hash: "00".repeat(32),
            },
            VE::StakeKeyNotRegisteredForDelegation {
                credential_hash: "00".repeat(32),
            },
            VE::DelegateeDRepNotRegistered {
                drep_id: "00".repeat(32),
            },
            VE::DelegateePoolNotRegistered {
                pool_id: "11".repeat(28),
            },
            VE::StakePoolNotRegisteredForRetirement {
                pool_id: "11".repeat(28),
            },
            VE::StakeRegistrationDepositMismatch {
                declared: 1,
                expected: 2,
            },
            VE::StakeDeregistrationRefundMismatch {
                declared: 1,
                expected: 2,
            },
            VE::StakeKeyHasNonZeroBalance {
                credential_hash: "00".repeat(32),
                balance: 7,
            },
            VE::DRepAlreadyRegistered {
                credential_hash: "00".repeat(32),
            },
            VE::DRepIncorrectDeposit {
                declared: 1,
                expected: 500_000_000,
            },
            VE::DRepIncorrectRefund {
                credential_hash: "00".repeat(32),
                declared: 1,
                expected: 2,
            },
            VE::CommitteeHasPreviouslyResigned {
                cold_credential_hash: "00".repeat(32),
            },
            VE::UnelectedCommitteeMember {
                cold_credential_hash: "00".repeat(32),
            },
            VE::StakePoolCostTooLow {
                actual: 1,
                minimum: 2,
            },
            VE::PoolMedataHashTooBig {
                pool: "11".repeat(28),
                hash_size: 64,
            },
            VE::VrfKeyHashAlreadyRegistered {
                vrf_keyhash: "22".repeat(32),
                existing_pool_id: "11".repeat(28),
            },
            VE::PoolRetirementTooLate {
                retirement_epoch: 99,
                current_epoch: 5,
                e_max: 5,
                max_epoch: 10,
            },
            VE::PoolRetirementTooEarly {
                retirement_epoch: 1,
                current_epoch: 5,
            },
            VE::InvalidMetadata { labels: vec![7] },
            VE::ExtraneousScriptWitness {
                hashes: vec!["33".repeat(28)],
            },
            VE::MalformedScriptWitnesses {
                hashes: vec!["33".repeat(28)],
            },
            VE::MalformedReferenceScripts {
                hashes: vec!["33".repeat(28)],
            },
            VE::ExtraRedeemer {
                tag: "Spend".to_string(),
                index: 0,
            },
            VE::UnspendableUTxONoDatumHash {
                input: format!("{}#0", "44".repeat(32)),
                language: "PlutusV2".to_string(),
            },
            VE::ConflictingCommitteeUpdate {
                conflicts: vec!["00".repeat(32)],
            },
            VE::ExpirationEpochTooSmall {
                invalid_members: vec![("00".repeat(32), 5)],
            },
            VE::TreasuryWithdrawalReturnAccountsDoNotExist {
                bad_addrs: vec![format!("e0{}", "55".repeat(28))],
            },
            VE::TreasuryWithdrawalsNetworkIdMismatch {
                expected: 0,
                mismatched: vec![(format!("e0{}", "55".repeat(28)), 1)],
            },
            VE::ProposalProcedureNetworkIdMismatch {
                expected: 0,
                mismatched: vec![(format!("e0{}", "55".repeat(28)), 1)],
            },
            VE::InvalidGuardrailsScriptHash {
                got: None,
                expected: Some(dugite_primitives::hash::Hash28::from_bytes([0x66; 28])),
            },
            VE::WdrlNotDelegatedToDRep {
                credential_hash: "77".repeat(28),
            },
            VE::TreasuryValueMismatch {
                declared: 1,
                actual: 2,
            },
        ];
        for v in cases {
            let label = format!("{v:?}");
            let mapped = convert_validation_error(v);
            assert!(
                !matches!(mapped, TxValidationError::ScriptFailed { .. }),
                "still degrading to ScriptFailed: {label}"
            );
        }
    }

    /// GOV predicate failures now produce dedicated `TxValidationError` variants
    /// (not `ScriptFailed`) so that the CBOR encoder can emit structured
    /// `ConwayGovPredFailure` wire format instead of a generic `ConwayMempoolFailure`.
    #[test]
    fn convert_validation_error_maps_gov_predicates_to_dedicated_variants() {
        use dugite_ledger::validation::ValidationError as VE;

        // ProposalDepositIncorrect: typed, carrying both amounts, so the wire
        // form is ConwayGovPredFailure tag 4 rather than a generic mempool
        // failure. Verified end-to-end: cardano-cli 11.0.0.0 decodes it and
        // prints "ProposalDepositIncorrect".
        let mapped = convert_validation_error(VE::ProposalDepositIncorrect {
            declared: 1,
            expected: 100_000_000_000,
        });
        assert!(
            matches!(
                mapped,
                TxValidationError::ProposalDepositIncorrect {
                    declared: 1,
                    expected: 100_000_000_000
                }
            ),
            "ProposalDepositIncorrect must map to its own variant, got {mapped:?}"
        );

        let mapped = convert_validation_error(VE::DisallowedVoters { violations: vec![] });
        assert!(
            matches!(mapped, TxValidationError::DisallowedVoters { .. }),
            "DisallowedVoters must map to DisallowedVoters variant, got {mapped:?}"
        );

        let mapped = convert_validation_error(VE::VotersDoNotExist { voters: vec![] });
        assert!(
            matches!(mapped, TxValidationError::VotersDoNotExist { .. }),
            "VotersDoNotExist must map to VotersDoNotExist variant, got {mapped:?}"
        );

        let mapped = convert_validation_error(VE::VotingOnExpiredGovAction {
            expired_votes: vec![],
        });
        assert!(
            matches!(mapped, TxValidationError::VotingOnExpiredGovAction { .. }),
            "VotingOnExpiredGovAction must map to dedicated variant, got {mapped:?}"
        );

        let mapped = convert_validation_error(VE::GovActionsDoNotExist { action_ids: vec![] });
        assert!(
            matches!(mapped, TxValidationError::GovActionsDoNotExist { .. }),
            "GovActionsDoNotExist must map to dedicated variant, got {mapped:?}"
        );

        let mapped =
            convert_validation_error(VE::ProposalReturnAccountDoesNotExist { bad_addrs: vec![] });
        assert!(
            matches!(
                mapped,
                TxValidationError::ProposalReturnAccountDoesNotExist { .. }
            ),
            "ProposalReturnAccountDoesNotExist must map to dedicated variant, got {mapped:?}"
        );

        let mapped = convert_validation_error(VE::UnelectedCommitteeVoters { hot_keys: vec![] });
        assert!(
            matches!(mapped, TxValidationError::UnelectedCommitteeVoters { .. }),
            "UnelectedCommitteeVoters must map to dedicated variant, got {mapped:?}"
        );
    }

    #[test]
    fn convert_validation_error_maps_size_predicates_to_size_variants() {
        use dugite_ledger::validation::ValidationError as VE;

        // `TxRefScriptSizeTooLarge` is intentionally remapped to `TxTooLarge`
        // because that is the closest semantic match — exercised here so
        // future refactors keep the mapping intentional rather than accidental.
        let e = convert_validation_error(VE::TxRefScriptSizeTooLarge {
            actual: 2048,
            limit: 1024,
        });
        assert!(matches!(
            e,
            TxValidationError::TxTooLarge {
                maximum: 1024,
                actual: 2048
            }
        ));

        let e = convert_validation_error(VE::OutputValueTooLarge {
            maximum: 5000,
            actual: 6000,
        });
        assert!(matches!(
            e,
            TxValidationError::OutputValueTooLarge {
                maximum: 5000,
                actual: 6000
            }
        ));
    }
}
