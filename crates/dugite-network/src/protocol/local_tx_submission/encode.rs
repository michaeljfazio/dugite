//! CBOR encoding for `ApplyTxErr` — structured rejection reasons matching Haskell cardano-node.
//!
//! When a transaction is rejected via LocalTxSubmission, the Haskell node sends a structured
//! CBOR encoding of `ApplyTxErr` containing era-specific predicate failures. This module
//! encodes `TxValidationError` into the same wire format so that `cardano-cli` and other
//! standard Cardano tools can parse rejection reasons.
//!
//! ## Wire format
//!
//! The `ApplyTxErr` payload (inside `MsgRejectTx = [2, payload]`) is:
//! ```text
//! [[era_id, [failure_0, failure_1, ...]]]
//! ```
//!
//! Each failure is nested three levels deep for Conway UTxO errors:
//! ```text
//! ConwayLedgerPredFailure(tag=1) → ConwayUtxowPredFailure(tag=0) → ConwayUtxoPredFailure(tag=N)
//! ```
//!
//! ## References
//!
//! - `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs`
//! - `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs`
//! - `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs`

use minicbor::Encoder;

use crate::{PlutusPurposeItem, TxValidationError};

/// CBOR tag 258 — marks a CBOR array as a mathematical set (sorted, no duplicates).
/// Required by Conway-era encoding for sets of TxIn, KeyHash, ScriptHash, etc.
const CBOR_TAG_SET: u64 = 258;

/// Encode a `TxValidationError` into the `ApplyTxErr` CBOR payload.
///
/// The returned bytes represent the full `ApplyTxErr` structure:
/// `[[era_id, [failure_0, failure_1, ...]]]`
///
/// This is appended directly after the `[2, ...]` MsgRejectTx tag in the server.
pub fn encode_apply_tx_err(error: &TxValidationError, era_id: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);

    // Collect all failures (flatten Multiple variant)
    let errors = flatten_errors(error);

    // Outer HFC wrapper: array(1) containing the era-tagged payload
    enc.array(1).expect("infallible");

    // Era-tagged payload: [era_id, [failure_0, failure_1, ...]]
    enc.array(2).expect("infallible");
    enc.u16(era_id).expect("infallible");

    // Array of ConwayLedgerPredFailure items
    enc.array(errors.len() as u64).expect("infallible");
    for err in &errors {
        encode_conway_ledger_pred_failure(&mut enc, err);
    }

    buf
}

/// Flatten a `TxValidationError` into a list of individual errors.
/// The `Multiple` variant is recursively expanded.
fn flatten_errors(error: &TxValidationError) -> Vec<&TxValidationError> {
    match error {
        TxValidationError::Multiple(errors) => errors.iter().flat_map(flatten_errors).collect(),
        other => vec![other],
    }
}

/// Encode a single `TxValidationError` as a `ConwayLedgerPredFailure`.
///
/// Most validation errors map to:
///   `ConwayLedgerPredFailure::ConwayUtxowFailure(tag=1)`
///     → `ConwayUtxowPredFailure::UtxoFailure(tag=0)`
///       → `ConwayUtxoPredFailure(tag=N, fields...)`
///
/// Witness-level errors skip the Utxo layer:
///   `ConwayLedgerPredFailure::ConwayUtxowFailure(tag=1)`
///     → `ConwayUtxowPredFailure(tag=N, fields...)`
///
/// Unmapped errors fall back to:
///   `ConwayLedgerPredFailure::ConwayMempoolFailure(tag=7, text)`
fn encode_conway_ledger_pred_failure(enc: &mut Encoder<&mut Vec<u8>>, err: &TxValidationError) {
    match err {
        // ── UTxO-level failures: Ledger(1) → Utxow(0) → Utxo(tag) ──

        // Tag 1: BadInputsUTxO — set of missing/bad inputs
        TxValidationError::InputNotFound { input }
        | TxValidationError::DuplicateInput { input } => {
            if let Some((tx_hash, tx_ix)) = parse_tx_input(input) {
                encode_utxo_failure(enc, 1, |enc| {
                    // tag(258) array(1)[ [tx_hash_bytes, tx_ix] ]
                    enc.tag(minicbor::data::Tag::new(CBOR_TAG_SET))
                        .expect("infallible");
                    enc.array(1).expect("infallible");
                    enc.array(2).expect("infallible");
                    enc.bytes(&tx_hash).expect("infallible");
                    enc.u32(tx_ix).expect("infallible");
                });
            } else {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            }
        }

        // Tag 2: OutsideValidityIntervalUTxO — [validity_interval, current_slot]
        TxValidationError::TtlExpired { current_slot, ttl } => {
            encode_utxo_failure(enc, 2, |enc| {
                // ValidityInterval: array(2)[ SNothing (lower), SJust ttl (upper) ]
                enc.array(2).expect("infallible");
                // SNothing = array(0)
                enc.array(0).expect("infallible");
                // SJust ttl = array(1)[ ttl ]
                enc.array(1).expect("infallible");
                enc.u64(*ttl).expect("infallible");
                // current_slot
                enc.u64(*current_slot).expect("infallible");
            });
        }
        TxValidationError::NotYetValid {
            current_slot,
            valid_from,
        } => {
            encode_utxo_failure(enc, 2, |enc| {
                // ValidityInterval: array(2)[ SJust valid_from (lower), SNothing (upper) ]
                enc.array(2).expect("infallible");
                // SJust valid_from = array(1)[ valid_from ]
                enc.array(1).expect("infallible");
                enc.u64(*valid_from).expect("infallible");
                // SNothing = array(0)
                enc.array(0).expect("infallible");
                // current_slot
                enc.u64(*current_slot).expect("infallible");
            });
        }

        // Tag 3: MaxTxSizeUTxO — [supplied (actual), expected (max)] (no swap)
        TxValidationError::TxTooLarge { maximum, actual } => {
            encode_utxo_failure(enc, 3, |enc| {
                enc.u64(*actual).expect("infallible");
                enc.u64(*maximum).expect("infallible");
            });
        }

        // Tag 4: InputSetEmptyUTxO — no fields
        TxValidationError::NoInputs => {
            encode_utxo_failure(enc, 4, |_enc| {});
        }

        // Tag 5: FeeTooSmallUTxO — [expected (min), supplied (actual)] (swapped)
        TxValidationError::FeeTooSmall { minimum, actual } => {
            encode_utxo_failure(enc, 5, |enc| {
                enc.u64(*minimum).expect("infallible");
                enc.u64(*actual).expect("infallible");
            });
        }

        // Tag 6: ValueNotConservedUTxO — [consumed, produced] (no swap)
        // consumed = sum of input values, produced = outputs + fee
        TxValidationError::ValueNotConserved {
            inputs,
            outputs,
            fee,
        } => {
            let consumed = *inputs;
            let produced = outputs.saturating_add(*fee);
            encode_utxo_failure(enc, 6, |enc| {
                // Coin values encoded as uint (ADA-only)
                enc.u64(consumed).expect("infallible");
                enc.u64(produced).expect("infallible");
            });
        }

        // Tag 12: InsufficientCollateral — [balance (DeltaCoin), required (Coin)]
        //
        // `Sum InsufficientCollateral 12 !> To a !> To b` with `a :: DeltaCoin`
        // (the balance) first, `b :: Coin` (the requirement) second —
        // oracle-verified against `Cardano.Ledger.Babbage.Rules.Utxo` (#1050).
        // `DeltaCoin` is `newtype DeltaCoin = DeltaCoin Integer` with a
        // newtype-derived `EncCBOR`, i.e. a BARE signed CBOR integer — no
        // array/group wrapper, unlike `IncorrectTotalCollateralField` below
        // whose two fields happen to share the same (int, uint) shape but are
        // a DIFFERENT predicate (tag 20).
        TxValidationError::InsufficientCollateral { balance, required } => {
            match i64::try_from(*balance) {
                Ok(balance_i64) => {
                    encode_utxo_failure(enc, 12, |enc| {
                        enc.i64(balance_i64).expect("infallible");
                        enc.u64(*required).expect("infallible");
                    });
                }
                // Practically unreachable (collateral balances never approach
                // i64's range), but an out-of-range value must fall back
                // rather than silently truncate into a WRONG balance (#979
                // rule: an unverified/lossy typed arm can be worse than the
                // generic one).
                Err(_) => partial_fallback(enc, err),
            }
        }

        // Tag 15: CollateralContainsNonADA — the FULL `Value` (Coin +
        // multi-asset) Haskell's `validateCollateralContainsNonADA` reports.
        // `Sum CollateralContainsNonADA 15 !> To a` with `a :: Value era` —
        // oracle-verified (#1050). Re-encoded with
        // `dugite_serialization::encode_value`, the SAME encoder transaction
        // outputs use, so the two paths stay byte-identical by construction.
        TxValidationError::CollateralHasTokens { value } => {
            encode_utxo_failure(enc, 15, |enc| {
                let raw = dugite_serialization::encode_value(value);
                enc.writer_mut().extend_from_slice(&raw);
            });
        }

        // Tag 20: IncorrectTotalCollateralField — [delta_coin_int, declared_coin_uint]
        // DeltaCoin is a signed integer; Coin is unsigned.
        TxValidationError::CollateralMismatch { declared, computed } => {
            encode_utxo_failure(enc, 20, |enc| {
                // IncorrectTotalCollateralField: [delta_coin_int, declared_coin_uint]
                // delta = computed - declared (signed)
                let delta = (*computed as i64) - (*declared as i64);
                enc.i64(delta).expect("infallible");
                enc.u64(*declared).expect("infallible");
            });
        }

        // Tag 18: TooManyCollateralInputs — [max_allowed, actual_count] (swapped)
        TxValidationError::TooManyCollateralInputs { max, actual } => {
            encode_utxo_failure(enc, 18, |enc| {
                enc.u64(*max).expect("infallible");
                enc.u64(*actual).expect("infallible");
            });
        }

        // Tag 19: NoCollateralInputs — no fields
        // Note: We can't distinguish "no collateral" from "collateral not found" in
        // the current TxValidationError enum. CollateralNotFound falls through to mempool.

        // Conway phase-2 V3 TxInfo translation failure:
        //   Ledger(1) → Utxow(0) → Utxo(0) → Utxos(1) → CollectErrors
        //     [ BadTranslation(3) → ConwayContextError tag 15 (NonEmpty TxIn) ]
        //
        // Introduced by Haskell cardano-ledger PR #5011 at PV >= 11.
        // See `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`:
        //   `ReferenceInputsNotDisjointFromInputs common ->`
        //   `  encode $ Sum ReferenceInputsNotDisjointFromInputs 15 !> To common`
        TxValidationError::ReferenceInputsNotDisjointFromInputs { inputs } => {
            let parsed: Vec<([u8; 32], u32)> =
                inputs.iter().filter_map(|s| parse_tx_input(s)).collect();
            if parsed.is_empty() {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            } else {
                encode_utxo_failure(enc, 0, |enc| {
                    // ConwayUtxosPredFailure: [1, CollectErrors-payload]
                    enc.array(2).expect("infallible");
                    enc.u8(1).expect("infallible");
                    // NonEmpty (CollectError era) → CBOR list of CollectError
                    enc.array(1).expect("infallible");
                    // CollectError::BadTranslation = [3, ContextError]
                    enc.array(2).expect("infallible");
                    enc.u8(3).expect("infallible");
                    // ConwayContextError::ReferenceInputsNotDisjointFromInputs
                    //   = [15, NonEmpty TxIn]
                    enc.array(2).expect("infallible");
                    enc.u8(15).expect("infallible");
                    // NonEmpty TxIn encoded as definite-length CBOR list of [hash, ix].
                    enc.array(parsed.len() as u64).expect("infallible");
                    for (hash, ix) in &parsed {
                        enc.array(2).expect("infallible");
                        enc.bytes(hash).expect("infallible");
                        enc.u32(*ix).expect("infallible");
                    }
                });
            }
        }

        // Tag 22: BabbageNonDisjointRefInputs — `NonEmpty TxIn`.
        //
        // #1051: this MUST be a bare list, NOT a `Set` — `Sum
        // BabbageNonDisjointRefInputs 22 !> To x` with `x :: NonEmpty TxIn`,
        // and `EncCBOR (NonEmpty a) = encCBOR . toList` never emits CBOR tag
        // 258 (that tag belongs exclusively to `Set`'s `encodeWithSetTag`).
        // The previous `enc.tag(CBOR_TAG_SET)` here produced a frame
        // cardano-cli's decoder rejects outright (`DeserialiseFailure
        // "expected list len or indef"`), oracle-verified against
        // `Cardano.Ledger.Conway.Rules.Utxo`.
        TxValidationError::ReferenceInputOverlapsInput { input } => {
            if let Some((tx_hash, tx_ix)) = parse_tx_input(input) {
                encode_utxo_failure(enc, 22, |enc| {
                    // NonEmpty TxIn: a bare `variableListLenEncoding` list —
                    // dugite raises one input per occurrence today, so this
                    // is always a 1-element list, but `list_open`/`list_close`
                    // are used (rather than a literal `array(1)`) so the
                    // shape stays correct if a future caller aggregates more
                    // than one overlapping input into a single failure.
                    list_open(enc, 1);
                    enc.array(2).expect("infallible");
                    enc.bytes(&tx_hash).expect("infallible");
                    enc.u32(tx_ix).expect("infallible");
                    list_close(enc, 1);
                });
            } else {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            }
        }

        // ── Witness-level failures: Ledger(1) → Utxow(tag) ──

        // Utxow tag 1: InvalidWitnessesUTXOW — [vkey_bytes...]
        TxValidationError::InvalidWitnessSignature { vkey } => {
            if let Some(vkey_bytes) = parse_hex_bytes(vkey) {
                encode_utxow_failure(enc, 1, |enc| {
                    enc.array(1).expect("infallible");
                    enc.bytes(&vkey_bytes).expect("infallible");
                });
            } else {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            }
        }

        // Utxow tag 2: MissingVKeyWitnessesUTXOW — tag(258) set of keyhash bytes(28)
        TxValidationError::MissingInputWitness { credential }
        | TxValidationError::MissingCertificateWitness { credential }
        | TxValidationError::MissingWithdrawalWitness { credential } => {
            if let Some(keyhash) = parse_hex_bytes(credential) {
                encode_utxow_failure(enc, 2, |enc| {
                    enc.tag(minicbor::data::Tag::new(CBOR_TAG_SET))
                        .expect("infallible");
                    enc.array(1).expect("infallible");
                    enc.bytes(&keyhash).expect("infallible");
                });
            } else {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            }
        }

        // Utxow tag 3: MissingScriptWitnessesUTXOW — tag(258) set of script hashes
        TxValidationError::MissingScriptWitness { credential }
        | TxValidationError::MissingCertificateScriptWitness { credential }
        | TxValidationError::MissingWithdrawalScriptWitness { credential } => {
            if let Some(script_hash) = parse_hex_bytes(credential) {
                encode_utxow_failure(enc, 3, |enc| {
                    enc.tag(minicbor::data::Tag::new(CBOR_TAG_SET))
                        .expect("infallible");
                    enc.array(1).expect("infallible");
                    enc.bytes(&script_hash).expect("infallible");
                });
            } else {
                {
                    tracing::debug!(err = ?err, "LocalTxSubmission: partial encode fallback");
                    encode_mempool_fallback(enc, "transaction validation failed");
                }
            }
        }

        // Utxow tag 5: MissingTxBodyMetadataHash
        TxValidationError::AuxiliaryDataWithoutHash => {
            // We don't have the expected hash, but the tag structure is [5, hash_bytes].
            // Fall back to mempool since we lack the actual metadata hash.
            encode_mempool_fallback(
                enc,
                "AuxiliaryDataWithoutHash: auxiliary data present but no hash in tx body",
            );
        }

        // Utxow tag 6: MissingTxMetadata
        TxValidationError::AuxiliaryDataHashWithoutData => {
            // We don't have the declared hash. Fall back to mempool.
            encode_mempool_fallback(
                enc,
                "AuxiliaryDataHashWithoutData: metadata hash declared but no auxiliary data",
            );
        }

        // Utxow tag 13: ScriptIntegrityHashMismatch (formerly PPViewHashesDontMatch pre-PV11) — [supplied_hash_or_null, expected_hash_or_null]
        TxValidationError::ScriptDataHashMismatch { expected, actual } => {
            encode_utxow_failure(enc, 13, |enc| {
                // supplied (actual from tx) — StrictMaybe encoding
                if let Some(hash_bytes) = parse_hex_bytes(actual) {
                    enc.array(1).expect("infallible");
                    enc.bytes(&hash_bytes).expect("infallible");
                } else {
                    enc.array(0).expect("infallible");
                }
                // expected (computed from script context) — StrictMaybe encoding
                if let Some(hash_bytes) = parse_hex_bytes(expected) {
                    enc.array(1).expect("infallible");
                    enc.bytes(&hash_bytes).expect("infallible");
                } else {
                    enc.array(0).expect("infallible");
                }
            });
        }

        // Utxow tag 13: ScriptIntegrityHashMismatch — unexpected hash present
        TxValidationError::UnexpectedScriptDataHash => {
            encode_utxow_failure(enc, 13, |enc| {
                // supplied = SJust (some hash, but we don't have it — encode as present-but-unknown)
                // expected = SNothing
                // Since we lack the actual hash bytes, fall back:
                enc.array(0).expect("infallible"); // supplied unknown
                enc.array(0).expect("infallible"); // expected nothing
            });
        }

        // Utxow tag 13: ScriptIntegrityHashMismatch — required hash missing
        TxValidationError::MissingScriptDataHash => {
            encode_utxow_failure(enc, 13, |enc| {
                // supplied = SNothing (tx didn't include hash)
                enc.array(0).expect("infallible");
                // expected = SJust (some hash, but we don't have bytes)
                enc.array(0).expect("infallible");
            });
        }

        // ── Ledger-level failures ──

        // Ledger tag 5: ConwayTreasuryValueMismatch (swapped: [expected, supplied])
        // Note: This variant currently maps to ScriptFailed in serve.rs, so it won't
        // reach here. But if TxValidationError is extended, this handles it.

        // Ledger tag 2: ConwayCertsFailure(ConwayCertsPredFailure)
        //   CERTS tag 0: WithdrawalsNotInRewardsCERTS(Withdrawals)
        //
        // Wire shape: array(2)[2, array(2)[0, { reward_account_bytes => coin }]]
        //
        // This is the PV <= 10 form, and therefore the ONLY withdrawal failure
        // reachable on mainnet/preprod/preview/devnet today. It bundles both
        // modes that PV >= 11 splits into tags 8 and 9 — missing account and
        // wrong amount — because Haskell builds it from
        // `unWithdrawals invalid <> fmap mismatchSupplied incomplete`, keeping
        // only the SUPPLIED coin. There is deliberately no expected value here.
        //
        // Before this arm existed the error fell through to a stringly-typed
        // `ScriptFailed`, so cardano-cli saw
        // `ConwayMempoolFailure "transaction validation failed"` where
        // cardano-node returns the typed failure. Same class as #925 and the
        // ProposalDepositIncorrect arm below: dugite reached the right VERDICT
        // with the wrong REASON, which the parity oracle scores as CLASSDIFF.
        TxValidationError::WithdrawalsNotInRewardsCERTS { bad } => {
            enc.array(2).expect("infallible");
            enc.u8(2).expect("infallible"); // Ledger tag 2: ConwayCertsFailure
            enc.array(2).expect("infallible");
            enc.u8(0).expect("infallible"); // CERTS tag 0
            enc.map(bad.len() as u64).expect("infallible");
            for (addr_hex, coin) in bad {
                match parse_hex_bytes(addr_hex) {
                    Some(bytes) => enc.bytes(&bytes).expect("infallible"),
                    None => enc.bytes(addr_hex.as_bytes()).expect("infallible"),
                };
                enc.u64(*coin).expect("infallible");
            }
        }

        // Ledger tag 8: ConwayWithdrawalsMissingAccounts(Withdrawals)
        // Wire shape: array(2)[8, { reward_account_bytes => coin, ... }]
        // `Withdrawals` is a newtype around `Map RewardAccount Coin`, which
        // EncCBOR encodes as a definite-length CBOR map.
        TxValidationError::WithdrawalsMissingAccounts { missing } => {
            enc.array(2).expect("infallible");
            enc.u8(8).expect("infallible");
            enc.map(missing.len() as u64).expect("infallible");
            for (addr_hex, coin) in missing {
                match parse_hex_bytes(addr_hex) {
                    Some(bytes) => enc.bytes(&bytes).expect("infallible"),
                    None => enc.bytes(addr_hex.as_bytes()).expect("infallible"),
                };
                enc.u64(*coin).expect("infallible");
            }
        }

        // Ledger tag 9: ConwayIncompleteWithdrawals(NonEmptyMap RewardAccount (Mismatch 'RelEQ Coin))
        // Wire shape: array(2)[9, { reward_account_bytes => [supplied_coin, expected_coin], ... }]
        // `Mismatch` EncCBOR encodes as `array(2)[supplied, expected]` (NOT
        // swapped — the field-swap on tag 5 ConwayTreasuryValueMismatch is a
        // separate Haskell-level bug in how the constructor is wired).
        TxValidationError::IncompleteWithdrawals { mismatches } => {
            enc.array(2).expect("infallible");
            enc.u8(9).expect("infallible");
            enc.map(mismatches.len() as u64).expect("infallible");
            for (addr_hex, supplied, expected) in mismatches {
                match parse_hex_bytes(addr_hex) {
                    Some(bytes) => enc.bytes(&bytes).expect("infallible"),
                    None => enc.bytes(addr_hex.as_bytes()).expect("infallible"),
                };
                enc.array(2).expect("infallible");
                enc.u64(*supplied).expect("infallible");
                enc.u64(*expected).expect("infallible");
            }
        }

        // ── Conway GOV predicate failures: Ledger(3) → ConwayGovPredFailure ──

        // Ledger tag 3 (ConwayGovFailure): array(2)[3, ConwayGovPredFailure]
        //
        // ConwayGovPredFailure inner tags (distinct from Ledger tags 1-9):
        //   0  = GovActionsDoNotExist          [govActionId, ...]
        //   5  = DisallowedVoters               [(voter, govActionId), ...]
        //   8  = InvalidPrevGovActionId         proposalProcedure (single value,
        //        NOT wrapped in an array — see the encoder arm below)
        //   9  = VotingOnExpiredGovAction        [(voter, govActionId), ...]
        //   14 = VotersDoNotExist               [voter, ...]
        //   16 = ProposalReturnAccountDoesNotExist  return_addr_bytes
        //   18 = UnelectedCommitteeVoters        [credential, ...]
        //
        // GovActionId CBOR:   array(2)[txhash_bytes_32, action_index_uint]
        // Voter CBOR:         array(2)[disc, hash28_bytes]
        //   disc 0=CC key, 1=CC script, 2=DRep key, 3=DRep script, 4=SPO
        // Credential CBOR:    array(2)[disc, hash28_bytes]
        //   disc 0=key, 1=script

        // Tag 0: GovActionsDoNotExist
        TxValidationError::GovActionsDoNotExist { action_ids } => {
            encode_gov_failure(enc, 0, |enc| {
                let parsed: Vec<([u8; 32], u32)> = action_ids
                    .iter()
                    .filter_map(|s| parse_tx_input(s))
                    .collect();
                enc.array(parsed.len() as u64).expect("infallible");
                for (hash, idx) in &parsed {
                    enc.array(2).expect("infallible");
                    enc.bytes(hash).expect("infallible");
                    enc.u32(*idx).expect("infallible");
                }
            });
        }

        // Tag 5: DisallowedVoters
        TxValidationError::DisallowedVoters { violations } => {
            encode_gov_failure(enc, 5, |enc| {
                enc.array(violations.len() as u64).expect("infallible");
                for (voter_disc, cred_hex, action_id) in violations {
                    if let (Some(cred_bytes), Some((hash, idx))) =
                        (parse_hex_28(cred_hex), parse_tx_input(action_id))
                    {
                        // Each element: [(voter), (govActionId)]  → array(2)
                        enc.array(2).expect("infallible");
                        // voter: array(2)[disc, hash28_bytes]
                        enc.array(2).expect("infallible");
                        enc.u8(*voter_disc).expect("infallible");
                        enc.bytes(&cred_bytes).expect("infallible");
                        // govActionId: array(2)[txhash_32, action_idx]
                        enc.array(2).expect("infallible");
                        enc.bytes(&hash).expect("infallible");
                        enc.u32(idx).expect("infallible");
                    }
                }
            });
        }

        // Tag 9: VotingOnExpiredGovAction
        TxValidationError::VotingOnExpiredGovAction { expired_votes } => {
            encode_gov_failure(enc, 9, |enc| {
                enc.array(expired_votes.len() as u64).expect("infallible");
                for (voter_disc, cred_hex, action_id) in expired_votes {
                    if let (Some(cred_bytes), Some((hash, idx))) =
                        (parse_hex_28(cred_hex), parse_tx_input(action_id))
                    {
                        enc.array(2).expect("infallible");
                        enc.array(2).expect("infallible");
                        enc.u8(*voter_disc).expect("infallible");
                        enc.bytes(&cred_bytes).expect("infallible");
                        enc.array(2).expect("infallible");
                        enc.bytes(&hash).expect("infallible");
                        enc.u32(idx).expect("infallible");
                    }
                }
            });
        }

        // Tag 14: VotersDoNotExist
        TxValidationError::VotersDoNotExist { voters } => {
            encode_gov_failure(enc, 14, |enc| {
                enc.array(voters.len() as u64).expect("infallible");
                for (voter_disc, cred_hex) in voters {
                    if let Some(cred_bytes) = parse_hex_28(cred_hex) {
                        enc.array(2).expect("infallible");
                        enc.u8(*voter_disc).expect("infallible");
                        enc.bytes(&cred_bytes).expect("infallible");
                    }
                }
            });
        }

        // Tag 16: ProposalReturnAccountDoesNotExist
        // Wire shape: array(2)[16, return_addr_bytes]
        // bad_addrs typically has exactly one entry (one bad return address per proposal).
        TxValidationError::ProposalReturnAccountDoesNotExist { bad_addrs } => {
            if let Some(first_hex) = bad_addrs.first() {
                let addr_bytes =
                    parse_hex_bytes(first_hex).unwrap_or_else(|| first_hex.as_bytes().to_vec());
                encode_gov_failure(enc, 16, |enc| {
                    enc.bytes(&addr_bytes).expect("infallible");
                });
            } else {
                encode_mempool_fallback(
                    enc,
                    "ProposalReturnAccountDoesNotExist: no return address",
                );
            }
        }

        // Tag 18: UnelectedCommitteeVoters
        TxValidationError::UnelectedCommitteeVoters { hot_credentials } => {
            encode_gov_failure(enc, 18, |enc| {
                enc.array(hot_credentials.len() as u64).expect("infallible");
                for (disc, cred_hex) in hot_credentials {
                    if let Some(cred_bytes) = parse_hex_28(cred_hex) {
                        enc.array(2).expect("infallible");
                        enc.u8(*disc).expect("infallible");
                        enc.bytes(&cred_bytes).expect("infallible");
                    }
                }
            });
        }

        // Tag 8: InvalidPrevGovActionId — [8, <the whole ProposalProcedure>]
        //
        // Haskell (`Cardano.Ledger.Conway.Rules.Gov`):
        //   InvalidPrevGovActionId (ProposalProcedure era)
        // encoded via the `Sum` pattern as a ONE-field constructor: the
        // entire `ProposalProcedure` value is the single payload item, not
        // just its lineage fields. `proposal` was plumbed through from
        // `dugite-ledger::validation::ValidationError` (dugite issue #915)
        // specifically so this frame can be byte-exact instead of the
        // generic `ConwayMempoolFailure` fallback below. Re-encoded with
        // `dugite_serialization::encode_proposal_procedure` — the SAME
        // function used to build `ProposalProcedure`s into transaction
        // bodies for signing, so both paths stay byte-identical.
        //
        // Reference: `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/Procedures.hs`
        // (`EncCBOR ProposalProcedure`) and
        // `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
        // (`ConwayGovPredFailure` tag 8).
        // GOV tag 4: ProposalDepositIncorrect (Mismatch 'RelEQ Coin)
        //
        // Wire shape: array(2)[3, array(3)[4, declared, expected]] — the
        // Mismatch is FLATTENED into the constructor, not nested.
        //
        // This is the opposite of how Mismatch appears inside Ledger tag 9
        // IncompleteWithdrawals, where it is a map VALUE and therefore must be
        // a self-contained array(2). In a constructor-field position Haskell
        // encodes it as a GROUP (encodeListLen (1 + groupsize) <> tag <>
        // encCBORGroup), so the two Coins sit directly in the constructor's
        // array.
        //
        // Verified empirically against cardano-cli 11.0.0.0: the nested form
        // produced `DecoderFailure ... DeserialiseFailure 10 "expected word"`
        // — the client could not decode the rejection at all, which is worse
        // than the generic reason it replaced.
        TxValidationError::ProposalDepositIncorrect { declared, expected } => {
            enc.array(2).expect("infallible");
            enc.u8(3).expect("infallible"); // Ledger tag 3: ConwayGovFailure
            enc.array(3).expect("infallible");
            enc.u8(4).expect("infallible"); // GOV tag 4
            enc.u64(*declared).expect("infallible");
            enc.u64(*expected).expect("infallible");
        }

        // GOV tag 1: MalformedProposal (GovAction era) — the whole action is
        // the single payload item, exactly like tag 8's ProposalProcedure.
        // Re-encoded with `dugite_serialization::encode_gov_action`, the SAME
        // encoder used to build governance actions into transaction bodies for
        // signing, so the two paths stay byte-identical by construction.
        //
        // Reference: `Conway/Rules/Gov.hs` (`ConwayGovPredFailure` tag 1).
        TxValidationError::MalformedProposalGOV { action } => {
            let raw = dugite_serialization::encode_gov_action(action);
            encode_gov_failure(enc, 1, |enc| {
                let writer = enc.writer_mut();
                writer.extend_from_slice(&raw);
            });
        }

        TxValidationError::InvalidPrevGovActionId { proposal, .. } => {
            let raw = dugite_serialization::encode_proposal_procedure(proposal);
            encode_gov_failure(enc, 8, |enc| {
                let writer = enc.writer_mut();
                writer.extend_from_slice(&raw);
            });
        }

        // Decode-level rejection: the tx failed `decode_transaction` before
        // Phase-1 ever ran (e.g. a Conway duplicate input hard-fails the
        // strict-set decoder, mirroring Haskell `decodeSetEnforceNoDuplicates`).
        // Haskell fails these at the codec layer with a DeserialiseFailure and
        // drops the connection; dugite deliberately answers a structured
        // MsgRejectTx instead. Carry the decoder's reason (#925): the rejected
        // bytes are the client's own submission, so unlike the C8 fallback
        // below there are no ledger internals to leak.
        TxValidationError::DecodeFailed { reason } => {
            encode_mempool_fallback(enc, &format!("transaction decode failed: {reason}"));
        }

        // ══ #979: typed CERT / DELEG failures ═══════════════════════════
        //
        // Ledger 2 -> CERTS 1 -> CERT 1 -> DELEG tag. DELEG tags are 1-BASED.
        TxValidationError::StakeKeyRegisteredDELEG { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => encode_deleg_failure(enc, 2, |e| encode_credential(e, disc, &h)),
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::StakeKeyNotRegisteredDELEG { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => encode_deleg_failure(enc, 3, |e| encode_credential(e, disc, &h)),
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::StakeKeyHasNonZeroAccountBalanceDELEG { balance } => {
            // The payload is the Coin, not the credential.
            encode_deleg_failure(enc, 4, |e| {
                e.u64(*balance).expect("infallible");
            });
        }
        TxValidationError::DelegateeDRepNotRegisteredDELEG { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => encode_deleg_failure(enc, 5, |e| encode_credential(e, disc, &h)),
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::DelegateeStakePoolNotRegisteredDELEG { pool_id } => {
            // `KeyHash StakePool` — a bare bstr(28), no Credential wrapper.
            match parse_hex_28(pool_id) {
                Some(h) => encode_deleg_failure(enc, 6, |e| {
                    e.bytes(&h).expect("infallible");
                }),
                None => partial_fallback(enc, err),
            }
        }
        // PV<=10: one constructor, one field, no Mismatch at all.
        TxValidationError::IncorrectDepositDELEG { supplied } => {
            encode_deleg_failure(enc, 1, |e| {
                e.u64(*supplied).expect("infallible");
            });
        }
        // `To mm` — the Mismatch is NESTED as array(2)[supplied, expected].
        TxValidationError::DepositIncorrectDELEG { supplied, expected } => {
            encode_deleg_failure(enc, 7, |e| {
                e.array(2).expect("infallible");
                e.u64(*supplied).expect("infallible");
                e.u64(*expected).expect("infallible");
            });
        }
        TxValidationError::RefundIncorrectDELEG { supplied, expected } => {
            encode_deleg_failure(enc, 8, |e| {
                e.array(2).expect("infallible");
                e.u64(*supplied).expect("infallible");
                e.u64(*expected).expect("infallible");
            });
        }

        // ══ #979: typed GOVCERT failures (0-based, unlike DELEG) ════════
        TxValidationError::ConwayDRepAlreadyRegistered { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => {
                    encode_govcert_failure(enc, 0, |e| encode_credential(e, disc, &h))
                }
                None => partial_fallback(enc, err),
            }
        }
        // `ToGroup mm` — the Mismatch is FLATTENED into the constructor's own
        // fields. Contrast DELEG 7/8 above, which nest the same type.
        TxValidationError::ConwayDRepIncorrectDeposit { supplied, expected } => {
            encode_govcert_failure(enc, 2, |e| {
                e.u64(*supplied).expect("infallible");
                e.u64(*expected).expect("infallible");
            });
        }
        TxValidationError::ConwayCommitteeHasPreviouslyResigned { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => {
                    encode_govcert_failure(enc, 3, |e| encode_credential(e, disc, &h))
                }
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::ConwayDRepIncorrectRefund { supplied, expected } => {
            encode_govcert_failure(enc, 4, |e| {
                e.u64(*supplied).expect("infallible");
                e.u64(*expected).expect("infallible");
            });
        }
        TxValidationError::ConwayCommitteeIsUnknown { credential } => {
            match parse_typed_credential(credential) {
                Some((disc, h)) => {
                    encode_govcert_failure(enc, 5, |e| encode_credential(e, disc, &h))
                }
                None => partial_fallback(enc, err),
            }
        }

        // ══ #979: typed POOL failures (hand-rolled arities) ═════════════
        TxValidationError::StakePoolNotRegisteredOnKeyPOOL { pool_id } => {
            match parse_hex_28(pool_id) {
                Some(h) => encode_pool_failure(enc, |e| {
                    e.array(2).expect("infallible");
                    e.u8(0).expect("infallible");
                    e.bytes(&h).expect("infallible");
                }),
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::StakePoolCostTooLowPOOL { supplied, expected } => {
            encode_pool_failure(enc, |e| {
                e.array(3).expect("infallible");
                e.u8(3).expect("infallible");
                e.u64(*supplied).expect("infallible");
                e.u64(*expected).expect("infallible");
            });
        }
        TxValidationError::WrongNetworkPOOL {
            expected,
            supplied,
            pool_id,
        } => match parse_hex_28(pool_id) {
            // NB: expected BEFORE supplied — the reverse of every other
            // Mismatch on this wire.
            Some(h) => encode_pool_failure(enc, |e| {
                e.array(4).expect("infallible");
                e.u8(4).expect("infallible");
                e.u8(*expected).expect("infallible");
                e.u8(*supplied).expect("infallible");
                e.bytes(&h).expect("infallible");
            }),
            None => partial_fallback(enc, err),
        },
        TxValidationError::PoolMedataHashTooBigPOOL { pool_id, size } => {
            match parse_hex_28(pool_id) {
                Some(h) => encode_pool_failure(enc, |e| {
                    e.array(3).expect("infallible");
                    e.u8(5).expect("infallible");
                    e.bytes(&h).expect("infallible");
                    e.u64(*size).expect("infallible");
                }),
                None => partial_fallback(enc, err),
            }
        }
        TxValidationError::VrfKeyHashAlreadyRegisteredPOOL {
            pool_id,
            vrf_key_hash,
        } => match (parse_hex_28(pool_id), hex::decode(vrf_key_hash)) {
            // Pool id FIRST, then the VRF hash.
            (Some(p), Ok(v)) if v.len() == 32 => encode_pool_failure(enc, |e| {
                e.array(3).expect("infallible");
                e.u8(6).expect("infallible");
                e.bytes(&p).expect("infallible");
                e.bytes(&v).expect("infallible");
            }),
            _ => partial_fallback(enc, err),
        },
        TxValidationError::StakePoolRetirementWrongEpochPOOL {
            gt_expected,
            lt_supplied,
            lt_expected,
        } => {
            // THREE fields, not four: the first Mismatch's `supplied` is
            // discarded by the encoder (`Mismatch _ gtExpected`).
            encode_pool_failure(enc, |e| {
                e.array(4).expect("infallible");
                e.u8(1).expect("infallible");
                e.u64(*gt_expected).expect("infallible");
                e.u64(*lt_supplied).expect("infallible");
                e.u64(*lt_expected).expect("infallible");
            });
        }

        // ══ #979: typed UTXOW failures ══════════════════════════════════
        TxValidationError::InvalidMetadataUTXOW => {
            // No payload upstream: `Sum InvalidMetadata 8` with nothing after.
            encode_utxow_failure(enc, 8, |_| {});
        }
        TxValidationError::ExtraneousScriptWitnessesUTXOW { script_hashes } => {
            encode_script_hash_set_utxow(enc, err, 9, script_hashes);
        }
        TxValidationError::UnspendableUTxONoDatumHashUTXOW { inputs } => {
            let parsed: Vec<([u8; 32], u32)> =
                inputs.iter().filter_map(|s| parse_tx_input(s)).collect();
            if parsed.len() != inputs.len() {
                partial_fallback(enc, err);
            } else {
                encode_utxow_failure(enc, 14, |e| {
                    set_open(e, parsed.len());
                    for (hash, ix) in &parsed {
                        e.array(2).expect("infallible");
                        e.bytes(hash).expect("infallible");
                        e.u32(*ix).expect("infallible");
                    }
                    set_close(e, parsed.len());
                });
            }
        }
        TxValidationError::ExtraRedeemersUTXOW { purposes } => {
            // `[PlutusPurpose AsIx]` — a plain list, not a set.
            encode_utxow_failure(enc, 15, |e| {
                list_open(e, purposes.len());
                for (tag, ix) in purposes {
                    e.array(2).expect("infallible");
                    e.u8(*tag).expect("infallible");
                    e.u32(*ix).expect("infallible");
                }
                list_close(e, purposes.len());
            });
        }
        TxValidationError::MalformedScriptWitnessesUTXOW { script_hashes } => {
            encode_script_hash_set_utxow(enc, err, 16, script_hashes);
        }
        TxValidationError::MalformedReferenceScriptsUTXOW { script_hashes } => {
            encode_script_hash_set_utxow(enc, err, 17, script_hashes);
        }

        // ══ #1025: further typed UTXOW/UTXO failures ═══════════════════
        // UTXOW tag 10: MissingRedeemers (NonEmpty (PlutusPurpose AsItem era, ScriptHash))
        //
        // Each element is a 2-tuple, so `array(2)[purpose, script_hash]`, and
        // the purpose is itself `array(2)[tag, item]` (EncCBORGroup with
        // `listLen _ = 2`). `AsItem` encodes the ITEM ONLY — it is a newtype
        // over the item with a newtype-derived EncCBOR, and the index is a
        // phantom parameter. Contrast tag 15 ExtraRedeemers above, which is
        // `AsIx` and therefore writes the index.
        //
        // Every item encoder is the SAME one that builds these values into
        // transaction bodies for signing, so the two paths cannot drift.
        TxValidationError::MissingRedeemersUTXOW { entries } => {
            let parsed: Vec<([u8; 28], &PlutusPurposeItem)> = entries
                .iter()
                .filter_map(|(purpose, sh)| parse_hex_28(sh).map(|h| (h, purpose)))
                .collect();
            // A `Withdrawing` account is a 29-byte reward account, not a hash;
            // validate it separately so a malformed one falls back rather than
            // being silently dropped from the list.
            let accounts_ok = entries.iter().all(|(p, _)| match p {
                PlutusPurposeItem::Withdrawing { account } => {
                    hex::decode(account).map(|b| b.len() == 29).unwrap_or(false)
                }
                PlutusPurposeItem::Minting { policy_id } => parse_hex_28(policy_id).is_some(),
                _ => true,
            });
            if parsed.len() != entries.len() || parsed.is_empty() || !accounts_ok {
                partial_fallback(enc, err);
            } else {
                encode_utxow_failure(enc, 10, |e| {
                    list_open(e, parsed.len());
                    for (script_hash, purpose) in &parsed {
                        e.array(2).expect("infallible");
                        // PlutusPurpose AsItem = array(2)[tag, item]
                        e.array(2).expect("infallible");
                        match purpose {
                            PlutusPurposeItem::Minting { policy_id } => {
                                e.u8(1).expect("infallible");
                                let h = parse_hex_28(policy_id).expect("validated above");
                                e.bytes(&h).expect("infallible");
                            }
                            PlutusPurposeItem::Certifying(cert) => {
                                e.u8(2).expect("infallible");
                                let raw = dugite_serialization::encode_certificate(cert);
                                e.writer_mut().extend_from_slice(&raw);
                            }
                            PlutusPurposeItem::Withdrawing { account } => {
                                e.u8(3).expect("infallible");
                                let raw = hex::decode(account).expect("validated above");
                                e.bytes(&raw).expect("infallible");
                            }
                            PlutusPurposeItem::Voting(voter) => {
                                e.u8(4).expect("infallible");
                                let raw = dugite_serialization::encode_voter(voter);
                                e.writer_mut().extend_from_slice(&raw);
                            }
                            PlutusPurposeItem::Proposing(proposal) => {
                                e.u8(5).expect("infallible");
                                let raw = dugite_serialization::encode_proposal_procedure(proposal);
                                e.writer_mut().extend_from_slice(&raw);
                            }
                        }
                        e.bytes(script_hash).expect("infallible");
                    }
                    list_close(e, parsed.len());
                });
            }
        }
        TxValidationError::MissingRequiredDatumsUTXOW { missing, provided } => {
            let missing_parsed: Vec<[u8; 32]> =
                missing.iter().filter_map(|h| parse_hex_32(h)).collect();
            let provided_parsed: Vec<[u8; 32]> =
                provided.iter().filter_map(|h| parse_hex_32(h)).collect();
            if missing_parsed.len() != missing.len()
                || provided_parsed.len() != provided.len()
                || missing_parsed.is_empty()
            {
                partial_fallback(enc, err);
            } else {
                encode_utxow_failure(enc, 11, |e| {
                    set_open(e, missing_parsed.len());
                    for h in &missing_parsed {
                        e.bytes(h).expect("infallible");
                    }
                    set_close(e, missing_parsed.len());
                    set_open(e, provided_parsed.len());
                    for h in &provided_parsed {
                        e.bytes(h).expect("infallible");
                    }
                    set_close(e, provided_parsed.len());
                });
            }
        }
        TxValidationError::NotAllowedSupplementalDatumsUTXOW { extra, allowed } => {
            let extra_parsed: Vec<[u8; 32]> =
                extra.iter().filter_map(|h| parse_hex_32(h)).collect();
            let allowed_parsed: Vec<[u8; 32]> =
                allowed.iter().filter_map(|h| parse_hex_32(h)).collect();
            if extra_parsed.len() != extra.len()
                || allowed_parsed.len() != allowed.len()
                || extra_parsed.is_empty()
            {
                partial_fallback(enc, err);
            } else {
                encode_utxow_failure(enc, 12, |e| {
                    set_open(e, extra_parsed.len());
                    for h in &extra_parsed {
                        e.bytes(h).expect("infallible");
                    }
                    set_close(e, extra_parsed.len());
                    set_open(e, allowed_parsed.len());
                    for h in &allowed_parsed {
                        e.bytes(h).expect("infallible");
                    }
                    set_close(e, allowed_parsed.len());
                });
            }
        }
        TxValidationError::OutputBootAddrAttrsTooBigUTXO { outputs_raw_cbor } => {
            let parsed: Vec<Vec<u8>> = outputs_raw_cbor
                .iter()
                .filter_map(|h| hex::decode(h).ok())
                .collect();
            if parsed.len() != outputs_raw_cbor.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                // `NonEmpty (TxOut era)` — a plain LIST, not a set.
                encode_utxo_failure(enc, 10, |e| {
                    list_open(e, parsed.len());
                    for raw in &parsed {
                        e.writer_mut().extend_from_slice(raw);
                    }
                    list_close(e, parsed.len());
                });
            }
        }
        TxValidationError::ScriptsNotPaidUTxOUTXO { inputs_outputs } => {
            let parsed: Vec<([u8; 32], u32, Vec<u8>)> = inputs_outputs
                .iter()
                .filter_map(|(input, out_hex)| {
                    let (hash, ix) = parse_tx_input(input)?;
                    let out = hex::decode(out_hex).ok()?;
                    Some((hash, ix, out))
                })
                .collect();
            if parsed.len() != inputs_outputs.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                // `NonEmptyMap TxIn (TxOut era)` — a MAP, not array pairs.
                encode_utxo_failure(enc, 13, |e| {
                    map_open(e, parsed.len());
                    for (hash, ix, out) in &parsed {
                        e.array(2).expect("infallible");
                        e.bytes(hash).expect("infallible");
                        e.u32(*ix).expect("infallible");
                        e.writer_mut().extend_from_slice(out);
                    }
                    map_close(e, parsed.len());
                });
            }
        }
        TxValidationError::BabbageOutputTooSmallUTxO { outputs } => {
            let parsed: Vec<(Vec<u8>, u64)> = outputs
                .iter()
                .filter_map(|(out_hex, min)| hex::decode(out_hex).ok().map(|raw| (raw, *min)))
                .collect();
            if parsed.len() != outputs.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                // `NonEmpty (TxOut era, Coin)` — a LIST of array(2) pairs,
                // NOT a set and NOT a map (the same TxOut could repeat).
                encode_utxo_failure(enc, 21, |e| {
                    list_open(e, parsed.len());
                    for (raw, min) in &parsed {
                        e.array(2).expect("infallible");
                        e.writer_mut().extend_from_slice(raw);
                        e.u64(*min).expect("infallible");
                    }
                    list_close(e, parsed.len());
                });
            }
        }

        // ══ #979: further typed GOV failures ════════════════════════════
        TxValidationError::ProposalProcedureNetworkIdMismatch { account, network } => {
            match hex::decode(account) {
                Ok(bytes) if !bytes.is_empty() => encode_gov_failure(enc, 2, |e| {
                    e.bytes(&bytes).expect("infallible");
                    e.u8(*network).expect("infallible");
                }),
                _ => partial_fallback(enc, err),
            }
        }
        TxValidationError::TreasuryWithdrawalsNetworkIdMismatch { accounts, network } => {
            let parsed: Vec<Vec<u8>> = accounts
                .iter()
                .filter_map(|a| hex::decode(a).ok())
                .collect();
            if parsed.len() != accounts.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                encode_gov_failure(enc, 3, |e| {
                    set_open(e, parsed.len());
                    for a in &parsed {
                        e.bytes(a).expect("infallible");
                    }
                    set_close(e, parsed.len());
                    e.u8(*network).expect("infallible");
                });
            }
        }
        TxValidationError::ConflictingCommitteeUpdate { credentials } => {
            let parsed: Vec<(u8, [u8; 28])> = credentials
                .iter()
                .filter_map(|c| parse_typed_credential(c))
                .collect();
            if parsed.len() != credentials.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                encode_gov_failure(enc, 6, |e| {
                    set_open(e, parsed.len());
                    for (disc, h) in &parsed {
                        encode_credential(e, *disc, h);
                    }
                    set_close(e, parsed.len());
                });
            }
        }
        TxValidationError::ExpirationEpochTooSmall { members } => {
            let parsed: Vec<((u8, [u8; 28]), u64)> = members
                .iter()
                .filter_map(|(c, ep)| parse_typed_credential(c).map(|p| (p, *ep)))
                .collect();
            if parsed.len() != members.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                // `NonEmptyMap` derives its EncCBOR newtype-wise from `Map`:
                // encodeMap, same 23-element threshold, no set tag.
                encode_gov_failure(enc, 7, |e| {
                    map_open(e, parsed.len());
                    for ((disc, h), ep) in &parsed {
                        encode_credential(e, *disc, h);
                        e.u64(*ep).expect("infallible");
                    }
                    map_close(e, parsed.len());
                });
            }
        }
        TxValidationError::DisallowedVotesDuringBootstrap { violations } => {
            let parsed: Vec<(u8, [u8; 28], [u8; 32], u32)> = violations
                .iter()
                .filter_map(|(d, c, a)| {
                    let cred = parse_hex_28(c)?;
                    let (h, i) = parse_tx_input(a)?;
                    Some((*d, cred, h, i))
                })
                .collect();
            if parsed.len() != violations.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                encode_gov_failure(enc, 13, |e| {
                    list_open(e, parsed.len());
                    for (disc, cred, hash, ix) in &parsed {
                        e.array(2).expect("infallible");
                        e.array(2).expect("infallible");
                        e.u8(*disc).expect("infallible");
                        e.bytes(cred).expect("infallible");
                        e.array(2).expect("infallible");
                        e.bytes(hash).expect("infallible");
                        e.u32(*ix).expect("infallible");
                    }
                    list_close(e, parsed.len());
                });
            }
        }
        TxValidationError::TreasuryWithdrawalReturnAccountsDoNotExist { accounts } => {
            let parsed: Vec<Vec<u8>> = accounts
                .iter()
                .filter_map(|a| hex::decode(a).ok())
                .collect();
            if parsed.len() != accounts.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                // `NonEmpty AccountAddress` — a plain LIST, not a set.
                encode_gov_failure(enc, 17, |e| {
                    list_open(e, parsed.len());
                    for a in &parsed {
                        e.bytes(a).expect("infallible");
                    }
                    list_close(e, parsed.len());
                });
            }
        }
        TxValidationError::InvalidGuardrailsScriptHash { got, expected } => {
            // Two `StrictMaybe ScriptHash`. `encodeStrictMaybe` writes
            // `array(0)` for SNothing and `array(1)[x]` for SJust.
            let g = got.as_ref().map(|h| parse_hex_28(h));
            let x = expected.as_ref().map(|h| parse_hex_28(h));
            if matches!(g, Some(None)) || matches!(x, Some(None)) {
                partial_fallback(enc, err);
            } else {
                encode_gov_failure(enc, 11, |e| {
                    for v in [g.flatten(), x.flatten()] {
                        match v {
                            Some(h) => {
                                e.array(1).expect("infallible");
                                e.bytes(&h).expect("infallible");
                            }
                            None => {
                                e.array(0).expect("infallible");
                            }
                        }
                    }
                });
            }
        }
        TxValidationError::ZeroTreasuryWithdrawalsGOV {
            withdrawals,
            policy_hash,
        } => {
            let parsed: Vec<(Vec<u8>, u64)> = withdrawals
                .iter()
                .filter_map(|(a, c)| hex::decode(a).ok().map(|b| (b, *c)))
                .collect();
            let policy = policy_hash.as_ref().map(|h| parse_hex_28(h));
            if parsed.len() != withdrawals.len() || matches!(policy, Some(None)) {
                partial_fallback(enc, err);
            } else {
                // GovAction::TreasuryWithdrawals = array(3)[2, {account: coin}, opt_policy_hash]
                encode_gov_failure(enc, 15, |e| {
                    e.array(3).expect("infallible");
                    e.u32(2).expect("infallible");
                    map_open(e, parsed.len());
                    for (account, coin) in &parsed {
                        e.bytes(account).expect("infallible");
                        e.u64(*coin).expect("infallible");
                    }
                    map_close(e, parsed.len());
                    match policy.flatten() {
                        Some(h) => {
                            e.bytes(&h).expect("infallible");
                        }
                        None => {
                            e.null().expect("infallible");
                        }
                    }
                });
            }
        }

        TxValidationError::ConflictingMetadataHashUTXOW { supplied, expected } => {
            match (hex::decode(supplied), hex::decode(expected)) {
                (Ok(a), Ok(b)) if a.len() == 32 && b.len() == 32 => {
                    // ToGroup: the Mismatch's two fields are spliced directly
                    // into the constructor, so arity is 3 not 2.
                    encode_utxow_failure(enc, 7, |e| {
                        e.bytes(&a).expect("infallible");
                        e.bytes(&b).expect("infallible");
                    });
                }
                _ => partial_fallback(enc, err),
            }
        }
        TxValidationError::WrongNetworkInOutput {
            expected,
            addresses,
        } => {
            let parsed: Vec<Vec<u8>> = addresses
                .iter()
                .filter_map(|a| hex::decode(a).ok())
                .collect();
            if parsed.len() != addresses.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                encode_utxo_failure(enc, 7, |e| {
                    e.u8(*expected).expect("infallible");
                    set_open(e, parsed.len());
                    for a in &parsed {
                        e.bytes(a).expect("infallible");
                    }
                    set_close(e, parsed.len());
                });
            }
        }
        TxValidationError::WrongNetworkWithdrawal { expected, accounts } => {
            let parsed: Vec<Vec<u8>> = accounts
                .iter()
                .filter_map(|a| hex::decode(a).ok())
                .collect();
            if parsed.len() != accounts.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                encode_utxo_failure(enc, 8, |e| {
                    e.u8(*expected).expect("infallible");
                    set_open(e, parsed.len());
                    for a in &parsed {
                        e.bytes(a).expect("infallible");
                    }
                    set_close(e, parsed.len());
                });
            }
        }

        // ══ #979: Ledger-level ══════════════════════════════════════════
        TxValidationError::WdrlNotDelegatedToDRep { key_hashes } => {
            // `NonEmpty (KeyHash Staking)` — bare bstr(28), NOT a Credential.
            let parsed: Vec<[u8; 28]> = key_hashes.iter().filter_map(|h| parse_hex_28(h)).collect();
            if parsed.len() != key_hashes.len() || parsed.is_empty() {
                partial_fallback(enc, err);
            } else {
                enc.array(2).expect("infallible");
                enc.u8(4).expect("infallible");
                list_open(enc, parsed.len());
                for h in &parsed {
                    enc.bytes(h).expect("infallible");
                }
                list_close(enc, parsed.len());
            }
        }
        TxValidationError::TreasuryValueMismatch { supplied, expected } => {
            // FLATTENED and SWAPPED: `ToGroup (swapMismatch mm)`, which upstream
            // annotates "The serialisation order is in reverse".
            enc.array(3).expect("infallible");
            enc.u8(5).expect("infallible");
            enc.u64(*expected).expect("infallible");
            enc.u64(*supplied).expect("infallible");
        }

        // ── Fallback for all unmapped variants ──
        // ConwayMempoolFailure (Ledger tag 7): [7, "descriptive text"]
        //
        // C8 fix: do NOT send internal Rust debug formatting (struct names,
        // field values, hashes) to the client — that leaks pool IDs, stake
        // credential hashes, and other ledger internals. Log the full detail
        // server-side at DEBUG level and send only a generic reason.
        _ => {
            tracing::debug!(
                err = ?err,
                "LocalTxSubmission: unmapped validation error (sending generic rejection)"
            );
            encode_mempool_fallback(enc, "transaction validation failed");
        }
    }
}

/// Log the full error server-side and send the generic rejection.
///
/// Used when a typed arm exists but the payload cannot be encoded faithfully —
/// a malformed hex field, a hash of the wrong width, an empty `NonEmpty`. An
/// arm that emitted a typed failure with a WRONG or EMPTY payload would be
/// worse than the generic one: cardano-cli would fail to decode it and report
/// `DeserialiseFailure` instead of a rule name. Better a truthful generic
/// error than a confident lie.
fn partial_fallback(enc: &mut Encoder<&mut Vec<u8>>, err: &TxValidationError) {
    tracing::debug!(err = ?err, "LocalTxSubmission: typed arm could not encode payload faithfully");
    encode_mempool_fallback(enc, "transaction validation failed");
}

/// The three UTXOW arms whose payload is exactly `Set ScriptHash`.
fn encode_script_hash_set_utxow(
    enc: &mut Encoder<&mut Vec<u8>>,
    err: &TxValidationError,
    utxow_tag: u8,
    hashes: &[String],
) {
    let parsed: Vec<[u8; 28]> = hashes.iter().filter_map(|h| parse_hex_28(h)).collect();
    if parsed.len() != hashes.len() || parsed.is_empty() {
        partial_fallback(enc, err);
        return;
    }
    encode_utxow_failure(enc, utxow_tag, |e| {
        set_open(e, parsed.len());
        for h in &parsed {
            e.bytes(h).expect("infallible");
        }
        set_close(e, parsed.len());
    });
}

// ── Container encodings (#979) ──
//
// Haskell `cardano-ledger-binary` `Encoder.hs`:
//
//   lengthThreshold = 23
//   variableListLenEncoding len contents =
//     if len <= lengthThreshold then exactListLenEncoding len contents
//                               else encodeListLenIndef <> contents <> encodeBreak
//
// `EncCBOR [a]` and `EncCBOR (NonEmpty a)` both go through it (`NonEmpty` is
// `encCBOR . toList`), and `encodeSet` prefixes tag 258 at PV>=9 and then
// applies the same rule. `encodeMap` is the same threshold without the tag.
//
// Predicate-failure payloads are usually small, so the definite branch is what
// runs in practice — but "usually small" is not a bound. A tx can carry more
// than 23 extraneous script witnesses, and at 24 the header must change shape
// or cardano-cli's decoder sees a truncated list. Same rule #938 established
// for the tx-body encoders.

/// Haskell `lengthThreshold`.
const LIST_DEFINITE_MAX: usize = 23;

/// Open a `variableListLenEncoding` list. Pair with [`list_close`].
fn list_open(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    if len <= LIST_DEFINITE_MAX {
        enc.array(len as u64).expect("infallible");
    } else {
        enc.begin_array().expect("infallible");
    }
}

/// Close a list opened by [`list_open`].
fn list_close(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    if len > LIST_DEFINITE_MAX {
        enc.end().expect("infallible");
    }
}

/// Open `encodeSet` at PV>=9: `tag(258)` then a variable-length list.
///
/// `NonEmptySet` derives its `EncCBOR` newtype-wise from `Set`, so it is
/// byte-identical — the non-emptiness is a type-level claim only.
fn set_open(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    enc.tag(minicbor::data::Tag::new(CBOR_TAG_SET))
        .expect("infallible");
    list_open(enc, len);
}

fn set_close(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    list_close(enc, len);
}

/// Open `encodeMap` — the same threshold as lists, without a tag.
/// `NonEmptyMap` derives newtype-wise from `Map`.
fn map_open(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    if len <= LIST_DEFINITE_MAX {
        enc.map(len as u64).expect("infallible");
    } else {
        enc.begin_map().expect("infallible");
    }
}

fn map_close(enc: &mut Encoder<&mut Vec<u8>>, len: usize) {
    if len > LIST_DEFINITE_MAX {
        enc.end().expect("infallible");
    }
}

/// Encode a Haskell `Credential` — `array(2)[disc, bstr(28)]`, `disc` 0 for
/// `KeyHashObj` and 1 for `ScriptHashObj`.
fn encode_credential(enc: &mut Encoder<&mut Vec<u8>>, disc: u8, hash28: &[u8; 28]) {
    enc.array(2).expect("infallible");
    enc.u8(disc).expect("infallible");
    enc.bytes(hash28).expect("infallible");
}

/// Split dugite's "typed-hash32" credential encoding into `(discriminator,
/// hash28)`.
///
/// dugite carries stake/DRep/committee credentials as a 32-byte value whose
/// first 28 bytes are the hash and whose **byte 28 is the credential
/// discriminator** — `0x00` key, `0x01` script (`Credential::to_typed_hash32`).
/// That is what makes a faithful `Credential` encoding possible at all: the
/// wire form needs the discriminator, and a bare 28-byte hash cannot supply
/// it.
fn parse_typed_credential(s: &str) -> Option<(u8, [u8; 28])> {
    let raw = hex::decode(s).ok()?;
    if raw.len() != 32 {
        // Tolerate a bare 28-byte hash by assuming a key credential — some
        // call sites predate the typed form. Never guess for anything else.
        if raw.len() == 28 {
            let mut h = [0u8; 28];
            h.copy_from_slice(&raw);
            return Some((0, h));
        }
        return None;
    }
    let disc = match raw[28] {
        0 => 0u8,
        1 => 1u8,
        _ => return None,
    };
    let mut h = [0u8; 28];
    h.copy_from_slice(&raw[..28]);
    Some((disc, h))
}

/// Encode a `ConwayCertsPredFailure` in the Ledger nesting: `[2, [tag, …]]`.
fn encode_certs_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    certs_tag: u8,
    encode_fields: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    let mut field_buf = Vec::new();
    let mut field_enc = Encoder::new(&mut field_buf);
    encode_fields(&mut field_enc);
    let n = count_cbor_items(&field_buf);

    enc.array(2).expect("infallible");
    enc.u8(2).expect("infallible"); // Ledger tag 2: ConwayCertsFailure
    enc.array((n + 1) as u64).expect("infallible");
    enc.u8(certs_tag).expect("infallible");
    enc.writer_mut().extend_from_slice(&field_buf);
}

/// Encode a `ConwayCertPredFailure` under `CertFailure`:
/// `[2, [1, [cert_tag, …]]]`.
///
/// `ConwayCertPredFailure` has **no tag 0** — it starts at 1 (`DelegFailure`),
/// 2 (`PoolFailure`), 3 (`GovCertFailure`).
fn encode_cert_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    cert_tag: u8,
    encode_body: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    let mut body = Vec::new();
    let mut body_enc = Encoder::new(&mut body);
    encode_body(&mut body_enc);

    encode_certs_failure(enc, 1, |e| {
        // CERTS tag 1 = CertFailure, whose single field is the CERT failure.
        e.array(2).expect("infallible");
        e.u8(cert_tag).expect("infallible");
        e.writer_mut().extend_from_slice(&body);
    });
}

/// `ConwayDelegPredFailure` — `[2, [1, [1, [deleg_tag, …]]]]`.
///
/// **DELEG tags are 1-based**: `IncorrectDepositDELEG` = 1 …
/// `RefundIncorrectDELEG` = 8. There is no tag 0. Numbering from declaration
/// order (which is correct for GOVCERT) is off by one on every arm.
fn encode_deleg_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    deleg_tag: u8,
    encode_fields: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    let mut field_buf = Vec::new();
    let mut field_enc = Encoder::new(&mut field_buf);
    encode_fields(&mut field_enc);
    let n = count_cbor_items(&field_buf);

    encode_cert_failure(enc, 1, |e| {
        // The CERT arm already wrote `[cert_tag, <body>]`'s opening; the body
        // is the DELEG failure array itself.
        e.array((n + 1) as u64).expect("infallible");
        e.u8(deleg_tag).expect("infallible");
        e.writer_mut().extend_from_slice(&field_buf);
    });
}

/// `ConwayGovCertPredFailure` — `[2, [1, [3, [govcert_tag, …]]]]`.
///
/// GOVCERT tags ARE 0-based, unlike DELEG.
fn encode_govcert_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    govcert_tag: u8,
    encode_fields: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    let mut field_buf = Vec::new();
    let mut field_enc = Encoder::new(&mut field_buf);
    encode_fields(&mut field_enc);
    let n = count_cbor_items(&field_buf);

    encode_cert_failure(enc, 3, |e| {
        e.array((n + 1) as u64).expect("infallible");
        e.u8(govcert_tag).expect("infallible");
        e.writer_mut().extend_from_slice(&field_buf);
    });
}

/// `ShelleyPoolPredFailure` — `[2, [1, [2, <raw>]]]`.
///
/// POOL is the one rule in this tree whose `EncCBOR` is **hand-rolled** rather
/// than built from the `Sum`/`encode` combinators:
///
/// ```haskell
/// StakePoolCostTooLowPOOL (Mismatch supplied expected) ->
///   encodeListLen 3 <> encCBOR (3 :: Word8) <> encCBOR supplied <> encCBOR expected
/// ```
///
/// So the caller writes the whole `array(n)[tag, …]` itself: the arity is not
/// derivable from a field count, the `Mismatch` fields are spliced in
/// individually, and `StakePoolRetirementWrongEpochPOOL` even DROPS one of
/// them. There is also no tag 2.
fn encode_pool_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    encode_raw: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    let mut body = Vec::new();
    let mut body_enc = Encoder::new(&mut body);
    encode_raw(&mut body_enc);

    encode_cert_failure(enc, 2, |e| {
        e.writer_mut().extend_from_slice(&body);
    });
}

// ── Encoding helpers ──

/// Encode a `ConwayUtxoPredFailure` wrapped in the full three-level nesting:
/// `[1, [0, [tag, fields...]]]`
///
/// The closure `encode_fields` writes the fields for the specific `ConwayUtxoPredFailure`
/// variant. The tag and surrounding arrays are handled by this function.
fn encode_utxo_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    utxo_tag: u8,
    encode_fields: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    // Count the fields that will be written by the closure.
    // We use a temporary buffer to determine the count.
    let mut field_buf = Vec::new();
    let mut field_enc = Encoder::new(&mut field_buf);
    encode_fields(&mut field_enc);

    // Count top-level CBOR items in field_buf
    let field_count = count_cbor_items(&field_buf);

    // ConwayLedgerPredFailure: array(2)[1, utxow_payload]
    enc.array(2).expect("infallible");
    enc.u8(1).expect("infallible"); // tag 1: ConwayUtxowFailure

    // ConwayUtxowPredFailure: array(2)[0, utxo_payload]
    enc.array(2).expect("infallible");
    enc.u8(0).expect("infallible"); // tag 0: UtxoFailure

    // ConwayUtxoPredFailure: array(N+1)[utxo_tag, fields...]
    enc.array((field_count + 1) as u64).expect("infallible");
    enc.u8(utxo_tag).expect("infallible");

    // Write the pre-encoded field bytes directly
    let writer = enc.writer_mut();
    writer.extend_from_slice(&field_buf);
}

/// Encode a `ConwayUtxowPredFailure` wrapped in the Ledger nesting:
/// `[1, [tag, fields...]]`
///
/// Used for witness-level errors that don't go through the Utxo layer.
fn encode_utxow_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    utxow_tag: u8,
    encode_fields: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    // Count fields via temporary buffer
    let mut field_buf = Vec::new();
    let mut field_enc = Encoder::new(&mut field_buf);
    encode_fields(&mut field_enc);
    let field_count = count_cbor_items(&field_buf);

    // ConwayLedgerPredFailure: array(2)[1, utxow_payload]
    enc.array(2).expect("infallible");
    enc.u8(1).expect("infallible"); // tag 1: ConwayUtxowFailure

    // ConwayUtxowPredFailure: array(N+1)[utxow_tag, fields...]
    enc.array((field_count + 1) as u64).expect("infallible");
    enc.u8(utxow_tag).expect("infallible");

    // Write the pre-encoded field bytes directly
    let writer = enc.writer_mut();
    writer.extend_from_slice(&field_buf);
}

/// Encode a `ConwayMempoolFailure` (Ledger tag 7) with a text description.
/// Used as fallback for error variants that can't be mapped to structured CBOR.
/// Encode a `ConwayMempoolFailure` (tag 7) with a sanitized message.
///
/// C8 fix: the `text` parameter MUST NOT contain internal Rust debug output
/// (struct field names, enum variant names, internal state values). Callers
/// should log the detailed error server-side at `tracing::debug!` level and
/// pass only a sanitized public reason string here.
fn encode_mempool_fallback(enc: &mut Encoder<&mut Vec<u8>>, text: &str) {
    // ConwayLedgerPredFailure: array(2)[7, text]
    enc.array(2).expect("infallible");
    enc.u8(7).expect("infallible"); // tag 7: ConwayMempoolFailure
    enc.str(text).expect("infallible");
}

/// Encode a `ConwayGovPredFailure` wrapped in the Ledger `ConwayGovFailure` nesting:
/// `array(2)[3, array(2)[gov_tag, payload]]`
///
/// Used for governance-level errors that go through Ledger tag 3 rather than
/// the UTxO/Utxow path (Ledger tag 1).
///
/// The closure `encode_payload` writes the payload items for the specific
/// `ConwayGovPredFailure` variant.  The array wrapping and tag are written here.
fn encode_gov_failure(
    enc: &mut Encoder<&mut Vec<u8>>,
    gov_tag: u8,
    encode_payload: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) {
    // Count the payload items that will be written by the closure.
    let mut payload_buf = Vec::new();
    let mut payload_enc = Encoder::new(&mut payload_buf);
    encode_payload(&mut payload_enc);
    let payload_count = count_cbor_items(&payload_buf);

    // ConwayLedgerPredFailure: array(2)[3, conway_gov_payload]
    enc.array(2).expect("infallible");
    enc.u8(3).expect("infallible"); // Ledger tag 3: ConwayGovFailure

    // ConwayGovPredFailure: array(N+1)[gov_tag, payload_items...]
    enc.array((payload_count + 1) as u64).expect("infallible");
    enc.u8(gov_tag).expect("infallible");

    // Write the pre-encoded payload bytes directly
    let writer = enc.writer_mut();
    writer.extend_from_slice(&payload_buf);
}

// ── Parsing helpers ──

/// Parse a transaction input string in the format `"hex_txhash#index"` into
/// a 32-byte hash and output index.
///
/// Returns `None` if the format is invalid.
fn parse_tx_input(s: &str) -> Option<([u8; 32], u32)> {
    let (hash_hex, idx_str) = s.rsplit_once('#')?;
    let idx: u32 = idx_str.parse().ok()?;
    let hash_bytes = parse_hex_bytes(hash_hex)?;
    if hash_bytes.len() != 32 {
        return None;
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hash_bytes);
    Some((hash, idx))
}

/// Parse a hex string into raw bytes. Returns `None` if the string has odd length
/// or contains non-hex characters.
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Parse a hex string into exactly 28 raw bytes.
/// Returns `None` if the string does not decode to exactly 28 bytes.
fn parse_hex_28(s: &str) -> Option<[u8; 28]> {
    let bytes = parse_hex_bytes(s)?;
    if bytes.len() != 28 {
        return None;
    }
    let mut arr = [0u8; 28];
    arr.copy_from_slice(&bytes);
    Some(arr)
}

/// Parse a hex string into exactly 32 raw bytes (datum hashes, #1025).
/// Returns `None` if the string does not decode to exactly 32 bytes.
fn parse_hex_32(s: &str) -> Option<[u8; 32]> {
    let bytes = parse_hex_bytes(s)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(arr)
}

/// Count the number of top-level CBOR data items in a byte buffer.
/// Used to determine array lengths for the `Sum` encoding pattern.
fn count_cbor_items(buf: &[u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let mut dec = minicbor::Decoder::new(buf);
    let mut count = 0;
    while dec.position() < buf.len() {
        if dec.skip().is_err() {
            break;
        }
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicbor::Decoder;

    // ── Helper: decode the outer structure and return the inner failure CBOR ──

    /// Decode `[[era_id, [failure_0, ...]]]` and return era_id + number of failures.
    fn decode_outer(bytes: &[u8]) -> (u16, u64) {
        let mut dec = Decoder::new(bytes);
        let outer_len = dec.array().unwrap().unwrap();
        assert_eq!(outer_len, 1, "outer HFC wrapper must be array(1)");

        let inner_len = dec.array().unwrap().unwrap();
        assert_eq!(inner_len, 2, "inner must be array(2) [era_id, failures]");

        let era_id = dec.u16().unwrap();
        let n_failures = dec.array().unwrap().unwrap();
        (era_id, n_failures)
    }

    /// Decode a ConwayLedgerPredFailure and return (ledger_tag, remaining_decoder_position).
    fn decode_ledger_tag(dec: &mut Decoder<'_>) -> u8 {
        let _arr = dec.array().unwrap();
        dec.u8().unwrap()
    }

    /// Decode ConwayUtxowPredFailure tag from within a Ledger(1) wrapper.
    fn decode_utxow_tag(dec: &mut Decoder<'_>) -> u8 {
        let _arr = dec.array().unwrap();
        dec.u8().unwrap()
    }

    /// Decode ConwayUtxoPredFailure tag from within Utxow(0) wrapper.
    fn decode_utxo_tag(dec: &mut Decoder<'_>) -> u8 {
        let _arr = dec.array().unwrap();
        dec.u8().unwrap()
    }

    // ── PV<=10 withdrawal failure (the only reachable one today) ──

    // ══ #979 golden vectors ═════════════════════════════════════════════
    //
    // Each of these asserts the EXACT payload bytes of the
    // `ConwayLedgerPredFailure`, not merely the tag. An arm that reaches the
    // right tag with the wrong payload shape is worse than the generic
    // failure it replaced: cardano-cli reports `DeserialiseFailure` instead of
    // a rule name, which is what the first `ProposalDepositIncorrect` attempt
    // did.
    //
    // Tags are from cardano-ledger @4f7cb2d6874df70561e32147084ed82cee773e8a.

    /// Strip the HFC + era + failure-list wrappers and return the raw
    /// `ConwayLedgerPredFailure` bytes.
    fn ledger_failure_bytes(err: &TxValidationError) -> Vec<u8> {
        let bytes = encode_apply_tx_err(err, 6);
        let mut dec = Decoder::new(&bytes);
        assert_eq!(dec.array().unwrap(), Some(1), "HFC wrapper");
        assert_eq!(dec.array().unwrap(), Some(2), "[era_id, failures]");
        assert_eq!(dec.u16().unwrap(), 6, "Conway era id");
        assert_eq!(dec.array().unwrap(), Some(1), "exactly one failure");
        bytes[dec.position()..].to_vec()
    }

    fn assert_ledger_bytes(err: &TxValidationError, want: &[u8], what: &str) {
        let got = ledger_failure_bytes(err);
        assert_eq!(
            hex::encode(&got),
            hex::encode(want),
            "{what}: wrong ConwayLedgerPredFailure bytes"
        );
    }

    /// A typed-hash32 credential: 28-byte hash + discriminator byte.
    fn typed_cred(byte: u8, script: bool) -> String {
        let mut v = vec![byte; 28];
        v.push(if script { 1 } else { 0 });
        v.extend_from_slice(&[0u8; 3]);
        hex::encode(v)
    }

    /// `[2, [1, [1, [tag, …]]]]` — Ledger 2 / CERTS 1 / CERT 1 / DELEG.
    fn deleg_prefix() -> Vec<u8> {
        vec![0x82, 0x02, 0x82, 0x01, 0x82, 0x01]
    }

    /// `[2, [1, [3, [tag, …]]]]` — Ledger 2 / CERTS 1 / CERT 3 / GOVCERT.
    fn govcert_prefix() -> Vec<u8> {
        vec![0x82, 0x02, 0x82, 0x01, 0x82, 0x03]
    }

    /// `[2, [1, [2, …]]]` — Ledger 2 / CERTS 1 / CERT 2 / POOL.
    fn pool_prefix() -> Vec<u8> {
        vec![0x82, 0x02, 0x82, 0x01, 0x82, 0x02]
    }

    /// DELEG tags are **1-based**: `StakeKeyRegisteredDELEG` is 2, not 1.
    /// A 0-based reading — correct for GOVCERT — is off by one on every arm.
    #[test]
    fn golden_stake_key_registered_deleg_is_tag_2() {
        let mut want = deleg_prefix();
        // array(2)[2, Credential] where Credential = array(2)[0, bstr28]
        want.extend_from_slice(&[0x82, 0x02, 0x82, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0xAA; 28]);
        assert_ledger_bytes(
            &TxValidationError::StakeKeyRegisteredDELEG {
                credential: typed_cred(0xAA, false),
            },
            &want,
            "StakeKeyRegisteredDELEG",
        );
    }

    /// The script discriminator must survive: dugite carries it in byte 28 of
    /// the typed-hash32, and Haskell's `Credential` needs it on the wire.
    #[test]
    fn golden_script_credential_keeps_its_discriminator() {
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x03, 0x82, 0x01, 0x58, 0x1c]);
        want.extend_from_slice(&[0xBB; 28]);
        assert_ledger_bytes(
            &TxValidationError::StakeKeyNotRegisteredDELEG {
                credential: typed_cred(0xBB, true),
            },
            &want,
            "StakeKeyNotRegisteredDELEG (script)",
        );
    }

    /// The payload is the BALANCE, not the credential.
    #[test]
    fn golden_stake_key_has_non_zero_balance_carries_a_coin() {
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x04, 0x19, 0x03, 0xE8]); // 1000
        assert_ledger_bytes(
            &TxValidationError::StakeKeyHasNonZeroAccountBalanceDELEG { balance: 1000 },
            &want,
            "StakeKeyHasNonZeroAccountBalanceDELEG",
        );
    }

    /// `KeyHash StakePool` — a bare bstr(28), NOT a `Credential` array.
    #[test]
    fn golden_delegatee_stake_pool_not_registered_is_a_bare_key_hash() {
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x06, 0x58, 0x1c]);
        want.extend_from_slice(&[0xCC; 28]);
        assert_ledger_bytes(
            &TxValidationError::DelegateeStakePoolNotRegisteredDELEG {
                pool_id: hex::encode([0xCC; 28]),
            },
            &want,
            "DelegateeStakePoolNotRegisteredDELEG",
        );
    }

    /// **The Mismatch trap.** DELEG writes `To mm` — the Mismatch NESTS as
    /// `array(2)[supplied, expected]`. GOVCERT writes `ToGroup mm` for the
    /// same type, which FLATTENS it. Both are asserted here so neither can
    /// drift onto the other's shape.
    #[test]
    fn golden_deleg_nests_mismatch_but_govcert_flattens_it() {
        // DELEG tag 7: [7, [supplied, expected]] — three items total.
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x07, 0x82, 0x0A, 0x14]); // [7, [10, 20]]
        assert_ledger_bytes(
            &TxValidationError::DepositIncorrectDELEG {
                supplied: 10,
                expected: 20,
            },
            &want,
            "DepositIncorrectDELEG (nested Mismatch)",
        );

        // GOVCERT tag 2: [2, supplied, expected] — FLATTENED, arity 3.
        let mut want = govcert_prefix();
        want.extend_from_slice(&[0x83, 0x02, 0x0A, 0x14]);
        assert_ledger_bytes(
            &TxValidationError::ConwayDRepIncorrectDeposit {
                supplied: 10,
                expected: 20,
            },
            &want,
            "ConwayDRepIncorrectDeposit (flattened Mismatch)",
        );
    }

    #[test]
    fn golden_refund_incorrect_deleg_is_tag_8() {
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x08, 0x82, 0x01, 0x02]);
        assert_ledger_bytes(
            &TxValidationError::RefundIncorrectDELEG {
                supplied: 1,
                expected: 2,
            },
            &want,
            "RefundIncorrectDELEG",
        );
    }

    /// GOVCERT tags ARE 0-based, unlike DELEG.
    #[test]
    fn golden_conway_drep_already_registered_is_govcert_tag_0() {
        let mut want = govcert_prefix();
        want.extend_from_slice(&[0x82, 0x00, 0x82, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0xDD; 28]);
        assert_ledger_bytes(
            &TxValidationError::ConwayDRepAlreadyRegistered {
                credential: typed_cred(0xDD, false),
            },
            &want,
            "ConwayDRepAlreadyRegistered",
        );
    }

    #[test]
    fn golden_committee_govcert_tags() {
        let mut want = govcert_prefix();
        want.extend_from_slice(&[0x82, 0x03, 0x82, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0x11; 28]);
        assert_ledger_bytes(
            &TxValidationError::ConwayCommitteeHasPreviouslyResigned {
                credential: typed_cred(0x11, false),
            },
            &want,
            "ConwayCommitteeHasPreviouslyResigned",
        );

        let mut want = govcert_prefix();
        want.extend_from_slice(&[0x82, 0x05, 0x82, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0x22; 28]);
        assert_ledger_bytes(
            &TxValidationError::ConwayCommitteeIsUnknown {
                credential: typed_cred(0x22, false),
            },
            &want,
            "ConwayCommitteeIsUnknown",
        );
    }

    /// `StakePoolNotRegisteredOnKeyPOOL` — POOL tag 0, ONE field, a bare
    /// `KeyHash StakePool` bstr(28). Raised by `PoolRetirement` of an
    /// unregistered pool (dugite #1023-class issue: distinct from
    /// `DelegateeStakePoolNotRegisteredDELEG`, which is DELEG tag 6 above —
    /// same "unregistered pool" condition, different rule, different wire
    /// nesting).
    #[test]
    fn golden_stake_pool_not_registered_on_key_pool_is_pool_tag_0() {
        let mut want = pool_prefix();
        want.extend_from_slice(&[0x82, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0x77; 28]);
        assert_ledger_bytes(
            &TxValidationError::StakePoolNotRegisteredOnKeyPOOL {
                pool_id: hex::encode([0x77; 28]),
            },
            &want,
            "StakePoolNotRegisteredOnKeyPOOL",
        );
    }

    /// Malformed pool-id hex must fall back to the generic mempool failure
    /// rather than emit a wrong-shaped typed frame.
    #[test]
    fn golden_stake_pool_not_registered_on_key_pool_falls_back_on_malformed_hex() {
        let got = ledger_failure_bytes(&TxValidationError::StakePoolNotRegisteredOnKeyPOOL {
            pool_id: "not-hex".to_string(),
        });
        // ConwayMempoolFailure: [7, text]
        assert_eq!(got[0], 0x82);
        assert_eq!(got[1], 0x07);
    }

    /// POOL's `EncCBOR` is hand-rolled: `encodeListLen 3 <> 3 <> supplied <>
    /// expected`. There is no tag 2 in this rule.
    #[test]
    fn golden_stake_pool_cost_too_low_is_pool_tag_3() {
        let mut want = pool_prefix();
        want.extend_from_slice(&[0x83, 0x03, 0x18, 0x64, 0x18, 0xC8]); // [3, 100, 200]
        assert_ledger_bytes(
            &TxValidationError::StakePoolCostTooLowPOOL {
                supplied: 100,
                expected: 200,
            },
            &want,
            "StakePoolCostTooLowPOOL",
        );
    }

    /// `WrongNetworkPOOL` writes **expected before supplied** — the reverse of
    /// every other Mismatch on this wire.
    #[test]
    fn golden_wrong_network_pool_writes_expected_first() {
        let mut want = pool_prefix();
        want.extend_from_slice(&[0x84, 0x04, 0x01, 0x00, 0x58, 0x1c]);
        want.extend_from_slice(&[0x33; 28]);
        assert_ledger_bytes(
            &TxValidationError::WrongNetworkPOOL {
                expected: 1,
                supplied: 0,
                pool_id: hex::encode([0x33; 28]),
            },
            &want,
            "WrongNetworkPOOL",
        );
    }

    /// Pool id FIRST, then the VRF hash — `VRFKeyHashAlreadyRegistered
    /// (KeyHash StakePool) (VRFVerKeyHash StakePoolVRF)`.
    #[test]
    fn golden_vrf_key_hash_already_registered_orders_pool_then_vrf() {
        let mut want = pool_prefix();
        want.extend_from_slice(&[0x83, 0x06, 0x58, 0x1c]);
        want.extend_from_slice(&[0x44; 28]);
        want.extend_from_slice(&[0x58, 0x20]);
        want.extend_from_slice(&[0x55; 32]);
        assert_ledger_bytes(
            &TxValidationError::VrfKeyHashAlreadyRegisteredPOOL {
                pool_id: hex::encode([0x44; 28]),
                vrf_key_hash: hex::encode([0x55; 32]),
            },
            &want,
            "VRFKeyHashAlreadyRegistered",
        );
    }

    /// THREE fields, not four: the first Mismatch's `supplied` is discarded
    /// by the encoder (`Mismatch _ gtExpected`).
    #[test]
    fn golden_pool_retirement_wrong_epoch_drops_one_mismatch_field() {
        let mut want = pool_prefix();
        // [1, 5, 99, 10] — 99 needs the 0x18 uint8 prefix (CBOR inlines 0..23 only).
        want.extend_from_slice(&[0x84, 0x01, 0x05, 0x18, 0x63, 0x0A]);
        assert_ledger_bytes(
            &TxValidationError::StakePoolRetirementWrongEpochPOOL {
                gt_expected: 5,
                lt_supplied: 99,
                lt_expected: 10,
            },
            &want,
            "StakePoolRetirementWrongEpochPOOL",
        );
    }

    /// `InvalidMetadata` carries NO payload upstream — `Sum InvalidMetadata 8`
    /// with nothing after it. dugite's own error has a `labels` field, which
    /// must NOT be emitted.
    #[test]
    fn golden_invalid_metadata_has_no_payload() {
        assert_ledger_bytes(
            &TxValidationError::InvalidMetadataUTXOW,
            &[0x82, 0x01, 0x81, 0x08],
            "InvalidMetadata",
        );
    }

    #[test]
    fn golden_script_hash_set_utxow_arms() {
        for (err, tag, what) in [
            (
                TxValidationError::ExtraneousScriptWitnessesUTXOW {
                    script_hashes: vec![hex::encode([0x66; 28])],
                },
                9u8,
                "ExtraneousScriptWitnessesUTXOW",
            ),
            (
                TxValidationError::MalformedScriptWitnessesUTXOW {
                    script_hashes: vec![hex::encode([0x66; 28])],
                },
                16,
                "MalformedScriptWitnesses",
            ),
            (
                TxValidationError::MalformedReferenceScriptsUTXOW {
                    script_hashes: vec![hex::encode([0x66; 28])],
                },
                17,
                "MalformedReferenceScripts",
            ),
        ] {
            // [1, [tag, 258([bstr28])]]
            let mut want = vec![0x82, 0x01, 0x82, tag, 0xD9, 0x01, 0x02, 0x81, 0x58, 0x1c];
            want.extend_from_slice(&[0x66; 28]);
            assert_ledger_bytes(&err, &want, what);
        }
    }

    // ══ #1025 golden vectors ════════════════════════════════════════════
    //
    // Tags oracle-verified against cardano-ledger
    // `a88b60bdcf3248dfe5a2f9372c188c399233f479` (pinned in
    // `tests/conformance/upstream/sources.toml`): `ConwayUtxowPredFailure`
    // tags 11/12 (`Conway/Rules/Utxow.hs:84`, wraps Alonzo's tags 2/3
    // unmodified), `ConwayUtxoPredFailure` tags 10/13
    // (`Conway/Rules/Utxo.hs`), `ConwayGovPredFailure` tag 15
    // (`Conway/Rules/Gov.hs:179`).

    /// `MissingRequiredDatums (NonEmptySet DataHash) (Set DataHash)` —
    /// UTXOW tag 11. Neither set is sorted (matching every other Set arm in
    /// this file — see `golden_script_hash_set_utxow_arms` above: none of
    /// them sort either. This is a one-off reject-reason payload, not a
    /// canonical/hashed wire form, so element order is not load-bearing).
    #[test]
    fn golden_missing_required_datums_utxow() {
        let missing = hex::encode([0x11u8; 32]);
        let provided = vec![hex::encode([0x11u8; 32]), hex::encode([0x22u8; 32])];
        let mut want = vec![0x82, 0x01, 0x83, 0x0B];
        // missing: tag(258) array(1)[bstr32]
        want.extend_from_slice(&[0xD9, 0x01, 0x02, 0x81, 0x58, 0x20]);
        want.extend_from_slice(&[0x11; 32]);
        // provided: tag(258) array(2)[bstr32, bstr32]
        want.extend_from_slice(&[0xD9, 0x01, 0x02, 0x82, 0x58, 0x20]);
        want.extend_from_slice(&[0x11; 32]);
        want.extend_from_slice(&[0x58, 0x20]);
        want.extend_from_slice(&[0x22; 32]);
        assert_ledger_bytes(
            &TxValidationError::MissingRequiredDatumsUTXOW {
                missing: vec![missing],
                provided,
            },
            &want,
            "MissingRequiredDatumsUTXOW",
        );
    }

    /// `NotAllowedSupplementalDatums (NonEmptySet DataHash) (Set DataHash)`
    /// — UTXOW tag 12.
    #[test]
    fn golden_not_allowed_supplemental_datums_utxow() {
        let hash = hex::encode([0x33u8; 32]);
        let mut want = vec![0x82, 0x01, 0x83, 0x0C];
        want.extend_from_slice(&[0xD9, 0x01, 0x02, 0x81, 0x58, 0x20]);
        want.extend_from_slice(&[0x33; 32]);
        want.extend_from_slice(&[0xD9, 0x01, 0x02, 0x81, 0x58, 0x20]);
        want.extend_from_slice(&[0x33; 32]);
        assert_ledger_bytes(
            &TxValidationError::NotAllowedSupplementalDatumsUTXOW {
                extra: vec![hash.clone()],
                allowed: vec![hash],
            },
            &want,
            "NotAllowedSupplementalDatumsUTXOW",
        );
    }

    /// `OutputBootAddrAttrsTooBig (NonEmpty (TxOut era))` — UTXO tag 10. A
    /// plain LIST (no set tag), full three-level `[1,[0,[10, list]]]`
    /// nesting since it's a `ConwayUtxoPredFailure`, not a
    /// `ConwayUtxowPredFailure`. The raw bytes stand in for "some encoded
    /// TxOut" — this test only pins the WRAPPER shape, not a real output's
    /// contents.
    #[test]
    fn golden_output_boot_addr_attrs_too_big_utxo() {
        let raw = vec![0x82u8, 0x01, 0x02];
        let want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x82, 0x0A, // Utxo: [10, list]
            0x81, // list(1)
            0x82, 0x01, 0x02, // the raw "TxOut" bytes, embedded verbatim
        ];
        assert_ledger_bytes(
            &TxValidationError::OutputBootAddrAttrsTooBigUTXO {
                outputs_raw_cbor: vec![hex::encode(&raw)],
            },
            &want,
            "OutputBootAddrAttrsTooBigUTXO",
        );
    }

    /// `ScriptsNotPaidUTxO (NonEmptyMap TxIn (TxOut era))` — UTXO tag 13. A
    /// MAP (`TxIn -> TxOut`), not array pairs.
    #[test]
    fn golden_scripts_not_paid_utxo() {
        let raw = vec![0x82u8, 0x01, 0x02];
        let mut want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x82, 0x0D, // Utxo: [13, map]
            0xA1, // map(1)
        ];
        // key: TxIn = array(2)[bstr32, u32]
        want.extend_from_slice(&[0x82, 0x58, 0x20]);
        want.extend_from_slice(&[0x44; 32]);
        want.push(0x00);
        // value: raw TxOut bytes, embedded verbatim
        want.extend_from_slice(&raw);
        assert_ledger_bytes(
            &TxValidationError::ScriptsNotPaidUTxOUTXO {
                inputs_outputs: vec![(
                    format!("{}#0", hex::encode([0x44u8; 32])),
                    hex::encode(&raw),
                )],
            },
            &want,
            "ScriptsNotPaidUTxOUTXO",
        );
    }

    /// `BabbageOutputTooSmallUTxO (NonEmpty (TxOut era, Coin))` — Conway
    /// UTXO tag 21. A LIST of `array(2)[txout, min_coin]` pairs — neither a
    /// set nor a map, unlike its UTXO-tag-13/10 neighbors above.
    #[test]
    fn golden_babbage_output_too_small_utxo() {
        let raw = vec![0x82u8, 0x01, 0x02];
        let mut want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x82, 0x15, // Utxo: [21, list]  (0x15 = 21)
            0x81, // list(1)
            0x82, // pair: array(2)
        ];
        want.extend_from_slice(&raw); // the raw "TxOut" bytes, embedded verbatim
        want.extend_from_slice(&[0x1A, 0x00, 0x0F, 0x42, 0x40]); // min_coin = 1_000_000
        assert_ledger_bytes(
            &TxValidationError::BabbageOutputTooSmallUTxO {
                outputs: vec![(hex::encode(&raw), 1_000_000)],
            },
            &want,
            "BabbageOutputTooSmallUTxO",
        );
    }

    /// Multiple offending outputs aggregate into ONE `NonEmpty` list —
    /// mirroring Haskell's per-tx (not per-output) predicate failure.
    #[test]
    fn golden_babbage_output_too_small_utxo_aggregates_multiple_outputs() {
        let raw_a = vec![0x82u8, 0x0A, 0x0B];
        let raw_b = vec![0x82u8, 0x0C, 0x0D];
        let mut want = vec![
            0x82, 0x01, 0x82, 0x00, 0x82, 0x15, // Ledger/Utxow/Utxo[21]
            0x82, // list(2)
            0x82, // pair 1: array(2)
        ];
        want.extend_from_slice(&raw_a);
        want.push(0x01); // min_coin = 1
        want.push(0x82); // pair 2: array(2)
        want.extend_from_slice(&raw_b);
        want.push(0x02); // min_coin = 2
        assert_ledger_bytes(
            &TxValidationError::BabbageOutputTooSmallUTxO {
                outputs: vec![(hex::encode(&raw_a), 1), (hex::encode(&raw_b), 2)],
            },
            &want,
            "BabbageOutputTooSmallUTxO (multiple outputs)",
        );
    }

    /// Malformed output hex must fall back to the generic mempool failure
    /// rather than emit a wrong-shaped typed frame.
    #[test]
    fn golden_babbage_output_too_small_utxo_falls_back_on_malformed_hex() {
        let got = ledger_failure_bytes(&TxValidationError::BabbageOutputTooSmallUTxO {
            outputs: vec![("not-hex".to_string(), 1_000_000)],
        });
        // ConwayMempoolFailure: [7, text]
        assert_eq!(got[0], 0x82);
        assert_eq!(got[1], 0x07);
    }

    /// `ZeroTreasuryWithdrawals (GovAction era)` — GOV tag 15. The field is
    /// the WHOLE `GovAction`, which for `TreasuryWithdrawals` is itself
    /// `array(3)[2, {account: coin}, opt_policy_hash]` — this is a `GovAction`
    /// nested one level inside the `ConwayGovPredFailure` payload, not a
    /// bespoke shape.
    #[test]
    fn golden_zero_treasury_withdrawals_gov() {
        let account = hex::encode([0x55u8; 29]);
        let want = vec![
            0x82, 0x03, // Ledger: [3, ...] (ConwayGovFailure)
            0x82, 0x0F, // GovPredFailure: [15, GovAction]
            0x83, 0x02, // GovAction: [2, ...] (TreasuryWithdrawals)
            0xA1, // withdrawals map(1)
            0x58, 0x1D, // bstr(29)
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            0x55, // 29 bytes
            0x1A, 0x00, 0x0F, 0x42, 0x40, // 1_000_000 as u32-width uint
            0xF6, // policy_hash = null (SNothing)
        ];
        assert_ledger_bytes(
            &TxValidationError::ZeroTreasuryWithdrawalsGOV {
                withdrawals: vec![(account, 1_000_000)],
                policy_hash: None,
            },
            &want,
            "ZeroTreasuryWithdrawalsGOV",
        );
    }

    /// A malformed payload (odd-length hex) must fall back to the generic
    /// mempool failure rather than emit a wrong-shaped typed frame — same
    /// safety net every other arm in this file relies on
    /// (`partial_fallback`).
    #[test]
    fn golden_missing_required_datums_utxow_falls_back_on_malformed_hex() {
        let got = ledger_failure_bytes(&TxValidationError::MissingRequiredDatumsUTXOW {
            missing: vec!["not-hex".to_string()],
            provided: vec![],
        });
        // ConwayMempoolFailure: [7, text]
        assert_eq!(got[0], 0x82);
        assert_eq!(got[1], 0x07);
    }

    /// `encodeSet` uses `variableListLenEncoding`: definite up to 23 elements,
    /// INDEFINITE above. Predicate-failure payloads are usually small, but
    /// "usually" is not a bound — a tx can carry more than 23 extraneous
    /// witnesses, and at 24 the header must change shape.
    #[test]
    fn golden_set_crosses_the_23_element_threshold() {
        let mk = |n: usize| TxValidationError::MalformedScriptWitnessesUTXOW {
            script_hashes: (0..n).map(|i| hex::encode([i as u8; 28])).collect(),
        };

        let at23 = ledger_failure_bytes(&mk(23));
        // 258( array(23) ) -> d9 0102 97
        assert_eq!(
            &at23[4..8],
            &[0xD9, 0x01, 0x02, 0x97],
            "23 elements must use a DEFINITE header"
        );

        let at24 = ledger_failure_bytes(&mk(24));
        // 258( indefinite array ) -> d9 0102 9f ... ff
        assert_eq!(
            &at24[4..8],
            &[0xD9, 0x01, 0x02, 0x9F],
            "24 elements must switch to the INDEFINITE header"
        );
        assert_eq!(
            *at24.last().unwrap(),
            0xFF,
            "an indefinite array must be terminated by a break"
        );
    }

    /// `ExtraRedeemers` is `[PlutusPurpose AsIx]` — a plain LIST, with no set
    /// tag. Emitting 258(...) here would be a decode failure upstream.
    #[test]
    fn golden_extra_redeemers_is_a_list_not_a_set() {
        let want = vec![0x82, 0x01, 0x82, 0x0F, 0x81, 0x82, 0x01, 0x03];
        assert_ledger_bytes(
            &TxValidationError::ExtraRedeemersUTXOW {
                purposes: vec![(1, 3)],
            },
            &want,
            "ExtraRedeemers",
        );
    }

    #[test]
    fn golden_unspendable_utxo_no_datum_hash_is_a_txin_set() {
        let mut want = vec![
            0x82, 0x01, 0x82, 0x0E, 0xD9, 0x01, 0x02, 0x81, 0x82, 0x58, 0x20,
        ];
        want.extend_from_slice(&[0x77; 32]);
        want.push(0x05);
        assert_ledger_bytes(
            &TxValidationError::UnspendableUTxONoDatumHashUTXOW {
                inputs: vec![format!("{}#5", hex::encode([0x77; 32]))],
            },
            &want,
            "UnspendableUTxONoDatumHash",
        );
    }

    /// `ConwayWdrlNotDelegatedToDRep (NonEmpty (KeyHash Staking))` — bare
    /// bstr(28) elements in a plain list. NOT `Credential`, so no
    /// discriminator array, and NOT a set, so no tag 258.
    #[test]
    fn golden_wdrl_not_delegated_to_drep_is_a_bare_key_hash_list() {
        let mut want = vec![0x82, 0x04, 0x81, 0x58, 0x1c];
        want.extend_from_slice(&[0x88; 28]);
        assert_ledger_bytes(
            &TxValidationError::WdrlNotDelegatedToDRep {
                key_hashes: vec![hex::encode([0x88; 28])],
            },
            &want,
            "ConwayWdrlNotDelegatedToDRep",
        );
    }

    /// `ConwayTreasuryValueMismatch` is BOTH flattened and SWAPPED —
    /// `Sum (… . unswapMismatch) 5 !> ToGroup (swapMismatch mm)`, which
    /// upstream annotates "The serialisation order is in reverse".
    #[test]
    fn golden_treasury_value_mismatch_is_flattened_and_swapped() {
        assert_ledger_bytes(
            &TxValidationError::TreasuryValueMismatch {
                supplied: 1,
                expected: 2,
            },
            // [5, expected, supplied] — NOT [5, supplied, expected]
            &[0x83, 0x05, 0x02, 0x01],
            "ConwayTreasuryValueMismatch",
        );
    }

    /// `NonEmptyMap` derives its `EncCBOR` newtype-wise from `Map`, so
    /// `encodeMap`: no set tag, same 23-element threshold.
    #[test]
    fn golden_expiration_epoch_too_small_is_a_map() {
        let mut want = vec![0x82, 0x03, 0x82, 0x07, 0xA1, 0x82, 0x00, 0x58, 0x1c];
        want.extend_from_slice(&[0x99; 28]);
        want.extend_from_slice(&[0x18, 0x64]); // epoch 100
        assert_ledger_bytes(
            &TxValidationError::ExpirationEpochTooSmall {
                members: vec![(typed_cred(0x99, false), 100)],
            },
            &want,
            "ExpirationEpochTooSmall",
        );
    }

    /// `NonEmptySet` derives newtype-wise from `Set`, so it DOES carry tag 258
    /// — unlike `NonEmpty`, which does not.
    #[test]
    fn golden_conflicting_committee_update_is_a_tagged_set() {
        let mut want = vec![
            0x82, 0x03, 0x82, 0x06, 0xD9, 0x01, 0x02, 0x81, 0x82, 0x00, 0x58, 0x1c,
        ];
        want.extend_from_slice(&[0xAB; 28]);
        assert_ledger_bytes(
            &TxValidationError::ConflictingCommitteeUpdate {
                credentials: vec![typed_cred(0xAB, false)],
            },
            &want,
            "ConflictingCommitteeUpdate",
        );
    }

    /// `TreasuryWithdrawalReturnAccountsDoNotExist (NonEmpty AccountAddress)`
    /// — a plain list of account-address BYTE STRINGS, no set tag.
    #[test]
    fn golden_treasury_withdrawal_return_accounts_is_an_untagged_list() {
        let acct = format!("e0{}", hex::encode([0x01; 28]));
        let mut want = vec![0x82, 0x03, 0x82, 0x11, 0x81, 0x58, 0x1d, 0xe0];
        want.extend_from_slice(&[0x01; 28]);
        assert_ledger_bytes(
            &TxValidationError::TreasuryWithdrawalReturnAccountsDoNotExist {
                accounts: vec![acct],
            },
            &want,
            "TreasuryWithdrawalReturnAccountsDoNotExist",
        );
    }

    /// Two `StrictMaybe ScriptHash`: `array(0)` for SNothing, `array(1)[x]`
    /// for SJust.
    #[test]
    fn golden_invalid_guardrails_script_hash_uses_strict_maybe() {
        let mut want = vec![0x82, 0x03, 0x83, 0x0B, 0x81, 0x58, 0x1c];
        want.extend_from_slice(&[0xCD; 28]);
        want.push(0x80); // SNothing
        assert_ledger_bytes(
            &TxValidationError::InvalidGuardrailsScriptHash {
                got: Some(hex::encode([0xCD; 28])),
                expected: None,
            },
            &want,
            "InvalidGuardrailsScriptHash",
        );
    }

    /// `ConflictingMetadataHash` is `ToGroup mm` — flattened, arity 3 — and
    /// the DECLARED hash comes first: `Mismatch { mismatchSupplied = mdh,
    /// mismatchExpected = hashTxAuxData md' }`.
    #[test]
    fn golden_conflicting_metadata_hash_is_flattened_declared_first() {
        let mut want = vec![0x82, 0x01, 0x83, 0x07, 0x58, 0x20];
        want.extend_from_slice(&[0xA1; 32]);
        want.extend_from_slice(&[0x58, 0x20]);
        want.extend_from_slice(&[0xB2; 32]);
        assert_ledger_bytes(
            &TxValidationError::ConflictingMetadataHashUTXOW {
                supplied: hex::encode([0xA1; 32]),
                expected: hex::encode([0xB2; 32]),
            },
            &want,
            "ConflictingMetadataHash",
        );
    }

    /// `WrongNetwork Network (Set Addr)` — the EXPECTED network then the set
    /// of offending addresses. There is no "actual network" field on this
    /// wire, and the set carries EVERY offender, not just the first.
    #[test]
    fn golden_wrong_network_in_output_carries_the_whole_address_set() {
        let a1 = vec![0x01u8; 29];
        let a2 = vec![0x02u8; 29];
        // [1, [0, [7, network, 258([addr, addr])]]]
        let mut want = vec![
            0x82, 0x01, 0x82, 0x00, 0x83, 0x07, 0x01, 0xD9, 0x01, 0x02, 0x82,
        ];
        want.extend_from_slice(&[0x58, 0x1d]);
        want.extend_from_slice(&a1);
        want.extend_from_slice(&[0x58, 0x1d]);
        want.extend_from_slice(&a2);
        assert_ledger_bytes(
            &TxValidationError::WrongNetworkInOutput {
                expected: 1,
                addresses: vec![hex::encode(&a1), hex::encode(&a2)],
            },
            &want,
            "WrongNetwork",
        );
    }

    #[test]
    fn golden_wrong_network_withdrawal_is_utxo_tag_8() {
        let acct = vec![0xE0u8; 29];
        let mut want = vec![
            0x82, 0x01, 0x82, 0x00, 0x83, 0x08, 0x00, 0xD9, 0x01, 0x02, 0x81,
        ];
        want.extend_from_slice(&[0x58, 0x1d]);
        want.extend_from_slice(&acct);
        assert_ledger_bytes(
            &TxValidationError::WrongNetworkWithdrawal {
                expected: 0,
                accounts: vec![hex::encode(&acct)],
            },
            &want,
            "WrongNetworkWithdrawal",
        );
    }

    /// **The PV inversion (#978's shape, acceptance criterion 4).**
    ///
    /// `hardforkConwayDELEGIncorrectDepositsAndRefunds pv = pvMajor pv > 10`.
    /// Below PV 11 an incorrect stake-key deposit OR refund is
    /// `IncorrectDepositDELEG Coin` — DELEG tag **1**, one field, no
    /// `Mismatch`. From PV 11 they split into tags 7 and 8, each carrying a
    /// full `Mismatch`.
    ///
    /// Every real network runs PV 10 today, so an implementation carrying only
    /// tags 7/8 would have the reachable case degrade to a generic failure
    /// while the implemented arms sat dead — which is precisely what #978
    /// found in the withdrawal path.
    #[test]
    fn golden_deleg_deposit_is_tag_1_pre_pv11_and_tag_7_after() {
        // PV<=10: [1, supplied] — no Mismatch.
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x01, 0x18, 0x64]);
        assert_ledger_bytes(
            &TxValidationError::IncorrectDepositDELEG { supplied: 100 },
            &want,
            "IncorrectDepositDELEG (PV<=10)",
        );

        // PV>=11: [7, [supplied, expected]] — nested Mismatch.
        let mut want = deleg_prefix();
        want.extend_from_slice(&[0x82, 0x07, 0x82, 0x18, 0x64, 0x18, 0xC8]);
        assert_ledger_bytes(
            &TxValidationError::DepositIncorrectDELEG {
                supplied: 100,
                expected: 200,
            },
            &want,
            "DepositIncorrectDELEG (PV>=11)",
        );
    }

    /// A payload that cannot be encoded faithfully must fall back to the
    /// generic failure rather than emit a typed frame with a wrong or empty
    /// body — the latter reaches cardano-cli as `DeserialiseFailure`, which is
    /// strictly less useful than the generic error it replaced.
    #[test]
    fn malformed_payloads_fall_back_instead_of_emitting_a_broken_frame() {
        for err in [
            TxValidationError::StakeKeyRegisteredDELEG {
                credential: "not-hex".into(),
            },
            TxValidationError::DelegateeStakePoolNotRegisteredDELEG {
                pool_id: hex::encode([0u8; 31]),
            },
            TxValidationError::MalformedScriptWitnessesUTXOW {
                script_hashes: vec![],
            },
            TxValidationError::WdrlNotDelegatedToDRep { key_hashes: vec![] },
            TxValidationError::VrfKeyHashAlreadyRegisteredPOOL {
                pool_id: hex::encode([0x44; 28]),
                vrf_key_hash: hex::encode([0x55; 28]), // must be 32
            },
        ] {
            let got = ledger_failure_bytes(&err);
            assert_eq!(
                got[0..2],
                [0x82, 0x07],
                "{err:?} must fall back to ConwayMempoolFailure (Ledger tag 7)"
            );
        }
    }

    /// GOLDEN: `WithdrawalsNotInRewardsCERTS` must encode as
    /// `[2, [0, {account => coin}]]` — Ledger tag 2 (ConwayCertsFailure)
    /// wrapping CERTS tag 0.
    ///
    /// Before this arm existed the error fell through to a stringly-typed
    /// `ScriptFailed` and reached cardano-cli as
    /// `ConwayMempoolFailure "transaction validation failed"`. The rewards
    /// round (#958) caught it on the first run that ever executed the
    /// withdrawal path: dugite REJECTED the wrong-amount withdrawal — the
    /// correct verdict — but cardano-node reported a withdrawal-class failure
    /// and dugite reported a generic one. The parity oracle scores that
    /// CLASSDIFF, and it is the same defect class as #925.
    ///
    /// Note there is deliberately NO expected-balance field: at PV<=10 Haskell
    /// keeps only `mismatchSupplied`, so a decoder must not look for one.
    #[test]
    fn withdrawals_not_in_rewards_certs_encodes_as_certs_tag_0() {
        let acct = "e0".to_string() + &"11".repeat(28);
        let err = TxValidationError::WithdrawalsNotInRewardsCERTS {
            bad: vec![(acct.clone(), 1_107_046_523)],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let (era_id, n) = decode_outer(&bytes);
        assert_eq!(era_id, 6, "Conway era id");
        assert_eq!(n, 1, "exactly one failure");

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        let _era = dec.u16().unwrap();
        dec.array().unwrap();

        assert_eq!(
            decode_ledger_tag(&mut dec),
            2,
            "Ledger tag 2 = ConwayCertsFailure"
        );

        let _certs_arr = dec.array().unwrap();
        assert_eq!(
            dec.u8().unwrap(),
            0,
            "CERTS tag 0 = WithdrawalsNotInRewardsCERTS"
        );

        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1, "one bad withdrawal");
        let key = dec.bytes().unwrap();
        assert_eq!(
            key.len(),
            29,
            "reward account is 29 bytes (header + 28-byte cred)"
        );
        assert_eq!(dec.u64().unwrap(), 1_107_046_523, "the SUPPLIED amount");
    }

    // ── Parsing tests ──

    #[test]
    fn test_parse_tx_input_valid() {
        let hash_hex = "a".repeat(64); // 32 bytes of 0xaa
        let input = format!("{hash_hex}#42");
        let (hash, idx) = parse_tx_input(&input).unwrap();
        assert_eq!(idx, 42);
        assert_eq!(hash, [0xaa; 32]);
    }

    #[test]
    fn test_parse_tx_input_invalid_no_hash() {
        assert!(parse_tx_input("#3").is_none());
    }

    #[test]
    fn test_parse_tx_input_invalid_no_index() {
        let hash_hex = "a".repeat(64);
        assert!(parse_tx_input(&hash_hex).is_none());
    }

    #[test]
    fn test_parse_tx_input_invalid_index() {
        let hash_hex = "a".repeat(64);
        let input = format!("{hash_hex}#abc");
        assert!(parse_tx_input(&input).is_none());
    }

    #[test]
    fn test_parse_tx_input_wrong_hash_length() {
        let input = "abcd#0"; // 2 bytes, not 32
        assert!(parse_tx_input(input).is_none());
    }

    #[test]
    fn test_parse_hex_bytes_valid() {
        let bytes = parse_hex_bytes("deadbeef").unwrap();
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_parse_hex_bytes_odd_length() {
        assert!(parse_hex_bytes("abc").is_none());
    }

    #[test]
    fn test_parse_hex_bytes_invalid_chars() {
        assert!(parse_hex_bytes("zzzz").is_none());
    }

    // ── Encoding tests ──

    #[test]
    fn test_encode_no_inputs() {
        let err = TxValidationError::NoInputs;
        let bytes = encode_apply_tx_err(&err, 6);
        let (era_id, n_failures) = decode_outer(&bytes);
        assert_eq!(era_id, 6);
        assert_eq!(n_failures, 1);

        // Navigate into the failure
        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap(); // outer [[...]]
        dec.array().unwrap(); // [era_id, [...]]
        dec.u16().unwrap(); // era_id
        dec.array().unwrap(); // failures array

        let ledger_tag = decode_ledger_tag(&mut dec);
        assert_eq!(ledger_tag, 1, "ConwayUtxowFailure");

        let utxow_tag = decode_utxow_tag(&mut dec);
        assert_eq!(utxow_tag, 0, "UtxoFailure");

        let utxo_tag = decode_utxo_tag(&mut dec);
        assert_eq!(utxo_tag, 4, "InputSetEmptyUTxO");
    }

    #[test]
    fn test_encode_fee_too_small() {
        let err = TxValidationError::FeeTooSmall {
            minimum: 200_000,
            actual: 170_000,
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let (era_id, n_failures) = decode_outer(&bytes);
        assert_eq!(era_id, 6);
        assert_eq!(n_failures, 1);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();

        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        // ConwayUtxoPredFailure: array(3)[5, min_fee, actual_fee]
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 5, "FeeTooSmallUTxO");
        let min_fee = dec.u64().unwrap();
        let actual_fee = dec.u64().unwrap();
        assert_eq!(min_fee, 200_000, "minimum fee first (swapped)");
        assert_eq!(actual_fee, 170_000, "actual fee second");
    }

    #[test]
    fn test_encode_value_not_conserved() {
        let err = TxValidationError::ValueNotConserved {
            inputs: 5_000_000,
            outputs: 4_500_000,
            fee: 200_000,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();

        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 6, "ValueNotConservedUTxO");
        let consumed = dec.u64().unwrap();
        let produced = dec.u64().unwrap();
        assert_eq!(consumed, 5_000_000, "consumed = inputs");
        assert_eq!(produced, 4_700_000, "produced = outputs + fee");
    }

    #[test]
    fn test_encode_tx_too_large() {
        let err = TxValidationError::TxTooLarge {
            maximum: 16_384,
            actual: 20_000,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 3, "MaxTxSizeUTxO");
        let supplied = dec.u64().unwrap();
        let expected = dec.u64().unwrap();
        assert_eq!(supplied, 20_000, "actual (supplied) first");
        assert_eq!(expected, 16_384, "maximum (expected) second");
    }

    #[test]
    fn test_encode_ttl_expired() {
        let err = TxValidationError::TtlExpired {
            current_slot: 1000,
            ttl: 500,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 2, "OutsideValidityIntervalUTxO");

        // ValidityInterval: array(2)[ SNothing, SJust(ttl) ]
        let vi_len = dec.array().unwrap().unwrap();
        assert_eq!(vi_len, 2);
        // SNothing (lower bound) = array(0)
        let lower_len = dec.array().unwrap().unwrap();
        assert_eq!(lower_len, 0, "no lower bound for TtlExpired");
        // SJust(ttl) (upper bound) = array(1)[ttl]
        let upper_len = dec.array().unwrap().unwrap();
        assert_eq!(upper_len, 1);
        let ttl = dec.u64().unwrap();
        assert_eq!(ttl, 500);

        // current_slot
        let current = dec.u64().unwrap();
        assert_eq!(current, 1000);
    }

    #[test]
    fn test_encode_not_yet_valid() {
        let err = TxValidationError::NotYetValid {
            current_slot: 100,
            valid_from: 500,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 2, "OutsideValidityIntervalUTxO");

        // ValidityInterval: array(2)[ SJust(valid_from), SNothing ]
        let vi_len = dec.array().unwrap().unwrap();
        assert_eq!(vi_len, 2);
        // SJust(valid_from) = array(1)[valid_from]
        let lower_len = dec.array().unwrap().unwrap();
        assert_eq!(lower_len, 1);
        let valid_from = dec.u64().unwrap();
        assert_eq!(valid_from, 500);
        // SNothing (upper) = array(0)
        let upper_len = dec.array().unwrap().unwrap();
        assert_eq!(upper_len, 0, "no upper bound for NotYetValid");

        let current = dec.u64().unwrap();
        assert_eq!(current, 100);
    }

    #[test]
    fn test_encode_bad_inputs() {
        let hash_hex = "ab".repeat(32); // 32 bytes
        let input = format!("{hash_hex}#7");
        let err = TxValidationError::InputNotFound { input };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 1, "BadInputsUTxO");

        // tag(258) followed by array(1)[ [hash, idx] ]
        let cbor_tag = dec.tag().unwrap();
        assert_eq!(cbor_tag.as_u64(), 258);
        let set_len = dec.array().unwrap().unwrap();
        assert_eq!(set_len, 1);
        let txin_len = dec.array().unwrap().unwrap();
        assert_eq!(txin_len, 2);
        let tx_hash = dec.bytes().unwrap();
        assert_eq!(tx_hash, vec![0xab; 32]);
        let tx_ix = dec.u32().unwrap();
        assert_eq!(tx_ix, 7);
    }

    #[test]
    fn test_encode_missing_vkey_witness() {
        let credential_hex = "cd".repeat(28); // 28-byte keyhash
        let err = TxValidationError::MissingInputWitness {
            credential: credential_hex,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();

        // Ledger tag 1: ConwayUtxowFailure
        assert_eq!(decode_ledger_tag(&mut dec), 1);

        // Utxow tag 2: MissingVKeyWitnessesUTXOW (NOT Utxo tag 0)
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2);
        let utxow_tag = dec.u8().unwrap();
        assert_eq!(utxow_tag, 2, "MissingVKeyWitnessesUTXOW");

        // tag(258) set of keyhash bytes
        let cbor_tag = dec.tag().unwrap();
        assert_eq!(cbor_tag.as_u64(), 258);
        let set_len = dec.array().unwrap().unwrap();
        assert_eq!(set_len, 1);
        let keyhash = dec.bytes().unwrap();
        assert_eq!(keyhash, vec![0xcd; 28]);
    }

    #[test]
    fn test_encode_too_many_collateral_inputs() {
        let err = TxValidationError::TooManyCollateralInputs { max: 3, actual: 5 };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 18, "TooManyCollateralInputs");
        let max_allowed = dec.u64().unwrap();
        let actual_count = dec.u64().unwrap();
        assert_eq!(max_allowed, 3, "max first (swapped)");
        assert_eq!(actual_count, 5, "actual second");
    }

    #[test]
    fn test_encode_multiple_errors() {
        let err = TxValidationError::Multiple(vec![
            TxValidationError::NoInputs,
            TxValidationError::FeeTooSmall {
                minimum: 200_000,
                actual: 100_000,
            },
        ]);
        let bytes = encode_apply_tx_err(&err, 6);
        let (era_id, n_failures) = decode_outer(&bytes);
        assert_eq!(era_id, 6);
        assert_eq!(n_failures, 2, "two flattened failures");
    }

    #[test]
    fn test_encode_fallback() {
        // C8 fix: the fallback arm must NOT expose internal Rust debug details.
        // The error variant "Other" with a detailed message must produce a sanitized
        // wire message ("transaction validation failed") rather than the raw debug string.
        let err = TxValidationError::Other("something unexpected".to_string());
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();

        // Ledger tag 7: ConwayMempoolFailure
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 7, "ConwayMempoolFailure");
        let text = dec.str().unwrap();
        // C8: must NOT contain the internal error string or Rust debug formatting.
        assert!(
            !text.contains("something unexpected"),
            "internal error text must not reach the wire: got {text:?}"
        );
        assert!(
            !text.contains('{') && !text.contains('}'),
            "Rust struct formatting must not reach the wire: got {text:?}"
        );
        // Must be the sanitized generic message.
        assert_eq!(text, "transaction validation failed");
    }

    #[test]
    fn test_encode_decode_failed_carries_reason() {
        // #925: a Conway duplicate-input tx fails `decode_transaction` at the
        // strict-set layer ("set: duplicate element") — Phase-1's
        // DuplicateInput arm never runs, so the rejection reaches the encoder
        // as DecodeFailed. It must carry the decoder's reason instead of the
        // generic "transaction validation failed": the submitted bytes are the
        // client's own, so echoing the decode reason leaks no ledger
        // internals (C8).
        let err = TxValidationError::DecodeFailed {
            reason: "set: duplicate element".to_string(),
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();

        // Ledger tag 7: ConwayMempoolFailure
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 7, "ConwayMempoolFailure");
        let text = dec.str().unwrap();
        assert_eq!(
            text, "transaction decode failed: set: duplicate element",
            "DecodeFailed must surface the decoder's reason (#925)"
        );
    }

    #[test]
    fn test_encode_script_data_hash_mismatch() {
        let expected_hex = "aa".repeat(32);
        let actual_hex = "bb".repeat(32);
        let err = TxValidationError::ScriptDataHashMismatch {
            expected: expected_hex,
            actual: actual_hex,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);

        // Utxow tag 13: ScriptIntegrityHashMismatch (formerly PPViewHashesDontMatch pre-PV11)
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let utxow_tag = dec.u8().unwrap();
        assert_eq!(utxow_tag, 13, "ScriptIntegrityHashMismatch");

        // supplied (actual): SJust(hash)
        let s_len = dec.array().unwrap().unwrap();
        assert_eq!(s_len, 1);
        let actual_bytes = dec.bytes().unwrap();
        assert_eq!(actual_bytes, vec![0xbb; 32]);

        // expected: SJust(hash)
        let e_len = dec.array().unwrap().unwrap();
        assert_eq!(e_len, 1);
        let expected_bytes = dec.bytes().unwrap();
        assert_eq!(expected_bytes, vec![0xaa; 32]);
    }

    #[test]
    fn test_encode_collateral_mismatch() {
        let err = TxValidationError::CollateralMismatch {
            declared: 5_000_000,
            computed: 4_800_000,
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        dec.array().unwrap();
        dec.array().unwrap();
        dec.u16().unwrap();
        dec.array().unwrap();
        assert_eq!(decode_ledger_tag(&mut dec), 1);
        assert_eq!(decode_utxow_tag(&mut dec), 0);

        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 3);
        let tag = dec.u8().unwrap();
        assert_eq!(tag, 20, "IncorrectTotalCollateralField");

        // delta = computed - declared = 4_800_000 - 5_000_000 = -200_000
        let delta = dec.i64().unwrap();
        assert_eq!(delta, -200_000);
        let declared = dec.u64().unwrap();
        assert_eq!(declared, 5_000_000);
    }

    // ── #1050: `InsufficientCollateral` (Ledger tag 1, UTXOW tag 0, UTXO tag 12) ──

    /// CBOR golden: `InsufficientCollateral DeltaCoin Coin` = `array(3)[12,
    /// balance_int, required_uint]`. `DeltaCoin` is a BARE signed CBOR
    /// integer (newtype-derived `EncCBOR`, no wrapper) — this pins that the
    /// balance is emitted as a plain negative int, not an array or a
    /// `Mismatch`-shaped pair.
    #[test]
    fn test_encode_insufficient_collateral_golden() {
        let err = TxValidationError::InsufficientCollateral {
            balance: -500_000,
            required: 1_500_000,
        };
        let want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x83, 0x0c, // Utxo: [12, balance, required]  (0x0c = 12)
            0x3a, 0x00, 0x07, 0xa1, 0x1f, // balance = -500_000 (CBOR negative int)
            0x1a, 0x00, 0x16, 0xe3, 0x60, // required = 1_500_000 (CBOR uint)
        ];
        assert_ledger_bytes(&err, &want, "InsufficientCollateral");
    }

    /// A positive balance (sufficient-looking collateral that still fails
    /// some OTHER check upstream of this rule, or a boundary value) must
    /// still encode as a plain non-negative CBOR int through the same path —
    /// `DeltaCoin`'s sign is not special-cased.
    #[test]
    fn test_encode_insufficient_collateral_positive_balance() {
        let err = TxValidationError::InsufficientCollateral {
            balance: 151,
            required: 152,
        };
        let want = vec![
            0x82, 0x01, 0x82, 0x00, 0x83, 0x0c, // Ledger/Utxow/Utxo[12]
            0x18, 0x97, // balance = 151
            0x18, 0x98, // required = 152
        ];
        assert_ledger_bytes(&err, &want, "InsufficientCollateral (positive balance)");
    }

    /// A `balance` outside `i64`'s range must fall back to the generic
    /// mempool failure rather than silently truncate into a WRONG value —
    /// the standing #979 rule that an unverified/lossy typed arm can be
    /// worse than the generic one. Collateral balances never approach this
    /// range in practice; this only pins the defensive path.
    #[test]
    fn insufficient_collateral_falls_back_on_out_of_range_balance() {
        let err = TxValidationError::InsufficientCollateral {
            balance: i128::MAX,
            required: 1,
        };
        let got = ledger_failure_bytes(&err);
        // ConwayMempoolFailure: [7, text]
        assert_eq!(got[0], 0x82);
        assert_eq!(got[1], 0x07);
    }

    // ── #1050: `CollateralHasTokens` → `CollateralContainsNonADA` (UTXO tag 15) ──

    /// CBOR golden: `CollateralContainsNonADA (Value era)` = `array(2)[15,
    /// value]` where `value` is the FULL multi-asset `Value` (coin +
    /// multi-asset map), re-encoded with the SAME `dugite_serialization::
    /// encode_value` transaction outputs use — never the netted balance.
    #[test]
    fn test_encode_collateral_has_tokens_golden() {
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::value::{AssetName, Lovelace, Value};
        use std::collections::BTreeMap;

        let mut assets = BTreeMap::new();
        assets.insert(AssetName(b"tok".to_vec()), 100u64);
        let mut multi_asset = BTreeMap::new();
        multi_asset.insert(Hash28::from_bytes([0xAB; 28]), assets);
        let value = Value {
            coin: Lovelace(5_000_000),
            multi_asset,
        };

        let err = TxValidationError::CollateralHasTokens {
            value: value.clone(),
        };

        let mut want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x82, 0x0f, // Utxo: [15, value]  (0x0f = 15)
        ];
        want.extend_from_slice(&dugite_serialization::encode_value(&value));
        assert_ledger_bytes(&err, &want, "CollateralHasTokens");

        // The embedded payload must be byte-identical to what the ordinary
        // tx-output Value encoder produces for the same value — the two
        // paths must stay in lockstep by construction, not by review (the
        // #932/#938 lesson).
        let raw = dugite_serialization::encode_value(&value);
        let got = ledger_failure_bytes(&err);
        assert_eq!(
            &got[6..],
            &raw[..],
            "payload must reuse encode_value verbatim"
        );
    }

    /// An ada-only `Value` (the degenerate/unreachable-in-practice case,
    /// since this predicate only fires when there ARE tokens) must still
    /// encode via the same bare-uint `encode_value` path, not an array —
    /// pins that no special-casing was added for the collateral call site.
    #[test]
    fn test_encode_collateral_has_tokens_ada_only_value_shape() {
        use dugite_primitives::value::{Lovelace, Value};

        let value = Value {
            coin: Lovelace(42),
            multi_asset: Default::default(),
        };
        let err = TxValidationError::CollateralHasTokens { value };
        let got = ledger_failure_bytes(&err);
        // Ledger[1,...] / Utxow[0,...] / Utxo[15, 42] — the value is a bare
        // uint, not array(2).
        assert_eq!(&got, &[0x82, 0x01, 0x82, 0x00, 0x82, 0x0f, 0x18, 0x2a]);
    }

    // ── #1051: `ReferenceInputOverlapsInput` → `BabbageNonDisjointRefInputs` (UTXO tag 22) ──

    /// CBOR golden pinning the FULL byte shape — including that the payload
    /// is a BARE list, NOT wrapped in CBOR tag 258. The previous
    /// implementation wrapped a `NonEmpty TxIn` in a spurious `Set` tag,
    /// producing bytes cardano-cli's decoder rejects outright
    /// (`DeserialiseFailure "expected list len or indef"`) — this test would
    /// have caught that: the prior version of this test only asserted the
    /// tag number (22) and never inspected the payload bytes, so it PINNED
    /// the bug rather than catching it (the #948-class "test pinned the
    /// bug" trap).
    #[test]
    fn test_encode_reference_input_overlaps_golden() {
        let hash_hex = "ff".repeat(32);
        let input = format!("{hash_hex}#0");
        let err = TxValidationError::ReferenceInputOverlapsInput { input };

        let mut want = vec![
            0x82, 0x01, // Ledger: [1, ...]
            0x82, 0x00, // Utxow: [0, ...]
            0x82, 0x16, // Utxo: [22, list]  (0x16 = 22)
            0x81, // list(1) — NOT tag(258) + list(1)
            0x82, // pair: array(2)
            0x58, 0x20, // bstr(32) header
        ];
        want.extend_from_slice(&[0xff; 32]);
        want.push(0x00); // tx_ix = 0
        assert_ledger_bytes(&err, &want, "ReferenceInputOverlapsInput");
    }

    /// Explicitly assert the ABSENCE of CBOR tag 258 (`0xd9, 0x01, 0x02`)
    /// anywhere in the encoded failure — the direct regression pin for
    /// #1051 (`Set` tag on a `NonEmpty` payload).
    #[test]
    fn reference_input_overlaps_never_emits_set_tag() {
        let hash_hex = "ab".repeat(32);
        let input = format!("{hash_hex}#3");
        let err = TxValidationError::ReferenceInputOverlapsInput { input };
        let got = ledger_failure_bytes(&err);
        let set_tag_bytes = [0xd9u8, 0x01, 0x02]; // tag(258), 2-byte form
        assert!(
            !got.windows(3).any(|w| w == set_tag_bytes),
            "BabbageNonDisjointRefInputs must never emit CBOR tag 258 (Set): {got:02x?}"
        );
    }

    // ── #1050/#1051: round-trip through dugite's OWN N2C decoder ──
    //
    // A same-process round-trip is necessary but NOT sufficient (a shared
    // wrong shape on both halves still passes — the standing caveat on
    // every golden test in this file); the oracle-verified byte-exact
    // goldens above are the real check. This is the encoder/decoder
    // AGREEMENT check the task calls for: dugite's own `n2c_client`
    // `decode_reject_reason` must not choke on what this encoder now
    // produces (previously it wouldn't have reached these arms at all —
    // they fell back to the generic `ConwayMempoolFailure` text).
    #[test]
    fn insufficient_collateral_round_trips_through_n2c_client_decoder() {
        let err = TxValidationError::InsufficientCollateral {
            balance: -500_000,
            required: 1_500_000,
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec)
            .expect("dugite's own decoder must parse its own encoder's output");
        assert!(reason.contains("InsufficientCollateral"), "got: {reason}");
        assert!(reason.contains("-500000"), "got: {reason}");
        assert!(reason.contains("1500000"), "got: {reason}");
    }

    #[test]
    fn collateral_has_tokens_round_trips_through_n2c_client_decoder() {
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::value::{AssetName, Lovelace, Value};
        use std::collections::BTreeMap;

        let mut assets = BTreeMap::new();
        assets.insert(AssetName(b"tok".to_vec()), 100u64);
        let mut multi_asset = BTreeMap::new();
        multi_asset.insert(Hash28::from_bytes([0xAB; 28]), assets);
        let value = Value {
            coin: Lovelace(5_000_000),
            multi_asset,
        };
        let err = TxValidationError::CollateralHasTokens { value };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec)
            .expect("dugite's own decoder must parse its own encoder's output");
        assert!(reason.contains("CollateralContainsNonADA"), "got: {reason}");
    }

    #[test]
    fn reference_input_overlaps_round_trips_through_n2c_client_decoder() {
        let hash_hex = "ff".repeat(32);
        let input = format!("{hash_hex}#0");
        let err = TxValidationError::ReferenceInputOverlapsInput { input };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec)
            .expect("dugite's own decoder must parse its own encoder's output");
        assert!(
            reason.contains("BabbageNonDisjointRefInputs"),
            "got: {reason}"
        );
    }

    /// dugite #470 wire test: round-trip
    /// `ReferenceInputsNotDisjointFromInputs` through the N2C encoder and
    /// verify the produced CBOR has the full nested tag path expected by
    /// Haskell cardano-ledger:
    ///   Ledger(1) → Utxow(0) → Utxo(0=UtxosFailure) →
    ///   Utxos(1=CollectErrors) → NonEmpty[ CollectError(3=BadTranslation) →
    ///   ConwayContextError(15) → NonEmpty TxIn ].
    #[test]
    fn test_encode_conway_context_error_tag_15() {
        let hash_hex = "ab".repeat(32);
        let input = format!("{hash_hex}#3");
        let err = TxValidationError::ReferenceInputsNotDisjointFromInputs {
            inputs: vec![input],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        // Walk the CBOR explicitly to assert every nested tag matches the
        // Haskell wire layout.
        let mut dec = Decoder::new(&bytes);
        // Outer HFC wrapper array(1)
        assert_eq!(dec.array().unwrap(), Some(1));
        // [era_id, failures]
        assert_eq!(dec.array().unwrap(), Some(2));
        let _era = dec.u16().unwrap();
        // failure list of length 1
        assert_eq!(dec.array().unwrap(), Some(1));
        // ConwayLedgerPredFailure: [1, ...]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 1, "Ledger tag = ConwayUtxowFailure");
        // ConwayUtxowPredFailure: [0, ...]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 0, "Utxow tag = UtxoFailure");
        // ConwayUtxoPredFailure: array(2) [0=UtxosFailure, payload]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 0, "Utxo tag = UtxosFailure");
        // ConwayUtxosPredFailure: [1=CollectErrors, NonEmpty(CollectError)]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 1, "Utxos tag = CollectErrors");
        // NonEmpty list of CollectError, length 1
        assert_eq!(dec.array().unwrap(), Some(1));
        // CollectError = [3=BadTranslation, ContextError]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "CollectError tag = BadTranslation");
        // ConwayContextError = [15, NonEmpty TxIn]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(
            dec.u8().unwrap(),
            15,
            "ConwayContextError tag = ReferenceInputsNotDisjointFromInputs"
        );
        // NonEmpty TxIn: array(1) [ [hash, ix] ]
        assert_eq!(dec.array().unwrap(), Some(1));
        assert_eq!(dec.array().unwrap(), Some(2));
        let hash_bytes = dec.bytes().unwrap();
        assert_eq!(hash_bytes, &[0xABu8; 32][..]);
        assert_eq!(dec.u32().unwrap(), 3);
    }

    /// dugite #470: encoder↔decoder round-trip — the produced bytes must
    /// be parsed by the n2c_client decoder and surface
    /// `ReferenceInputsNotDisjointFromInputs` (with the offending TxIn).
    #[test]
    fn test_roundtrip_conway_context_error_tag_15() {
        use crate::n2c_client::decode_reject_reason;

        let hash_hex = "cd".repeat(32);
        let input = format!("{hash_hex}#7");
        let err = TxValidationError::ReferenceInputsNotDisjointFromInputs {
            inputs: vec![input.clone()],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        // `decode_reject_reason` expects to read the ApplyTxErr payload
        // directly (outer array(1) wrapper).
        let mut dec = Decoder::new(&bytes);
        let reason = decode_reject_reason(&mut dec).expect("decoder must return a reason string");
        assert!(
            reason.contains("ReferenceInputsNotDisjointFromInputs"),
            "decoded reason should name the variant, got: {reason}"
        );
        assert!(
            reason.contains(&hash_hex) || reason.contains("cdcdcd"),
            "decoded reason should include the offending TxIn, got: {reason}"
        );
    }

    /// Test that the encoder output can be decoded by the existing client decoder.
    /// This validates encoder↔decoder compatibility.
    #[test]
    fn test_roundtrip_through_decoder() {
        let err = TxValidationError::FeeTooSmall {
            minimum: 200_000,
            actual: 170_000,
        };
        let apply_tx_err = encode_apply_tx_err(&err, 6);

        // Wrap in MsgRejectTx-like structure that decode_reject_reason expects.
        // The decoder expects to start AFTER the [2, ...] tag, at the ApplyTxErr payload.
        // Looking at n2c_client.rs:1155, it calls array() twice then reads era_idx.
        // The first array() enters the outer [[...]], the second enters [era_id, [...]].
        let mut dec = Decoder::new(&apply_tx_err);

        // Replicate decode_reject_reason logic
        let _ = dec.array().unwrap(); // outer array(1)
        let _ = dec.array().unwrap(); // [era_id, failures]
        let era_idx = dec.u8().unwrap();
        assert_eq!(era_idx, 6);

        let n_errors = dec.array().unwrap().unwrap();
        assert_eq!(n_errors, 1);

        // Decode the ConwayLedgerPredFailure
        let _ = dec.array().unwrap(); // failure array
        let ledger_tag = dec.u8().unwrap();
        assert_eq!(ledger_tag, 1); // ConwayUtxowFailure

        let _ = dec.array().unwrap(); // Utxow array
        let utxow_tag = dec.u8().unwrap();
        assert_eq!(utxow_tag, 0); // UtxoFailure

        let _ = dec.array().unwrap(); // Utxo array
        let utxo_tag = dec.u8().unwrap();
        assert_eq!(utxo_tag, 5); // FeeTooSmallUTxO

        let min_fee = dec.u64().unwrap();
        let actual_fee = dec.u64().unwrap();
        assert_eq!(min_fee, 200_000);
        assert_eq!(actual_fee, 170_000);
    }

    // ── Issue #457: Conway Ledger pred failure tags 8 / 9 ──

    /// CBOR golden: `ConwayWithdrawalsMissingAccounts` (Ledger tag 8).
    ///
    /// Wire shape: `array(2)[8, { reward_account_bytes => coin, ... }]`
    /// wrapped in the standard `array(1)[ array(2)[era_id, [failure]] ]`
    /// HFC envelope.
    #[test]
    fn test_encode_withdrawals_missing_accounts_golden() {
        // 29-byte reward account: header 0xe0 (stake key, mainnet) + 28-byte hash 0x11..
        let addr_bytes: Vec<u8> = std::iter::once(0xe0)
            .chain(std::iter::repeat_n(0x11, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::WithdrawalsMissingAccounts {
            missing: vec![(addr_hex.clone(), 1_000_000)],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let (era, n) = decode_outer(&bytes);
        assert_eq!(era, 6);
        assert_eq!(n, 1);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        let ledger_tag = decode_ledger_tag(&mut dec);
        assert_eq!(ledger_tag, 8, "expected ConwayWithdrawalsMissingAccounts");

        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1);
        let addr = dec.bytes().unwrap();
        assert_eq!(addr, addr_bytes.as_slice());
        let coin = dec.u64().unwrap();
        assert_eq!(coin, 1_000_000);
    }

    /// CBOR golden: `ConwayIncompleteWithdrawals` (Ledger tag 9).
    ///
    /// Wire shape: `array(2)[9, { reward_account_bytes => array(2)[supplied, expected], ... }]`.
    /// Field order on the wire for `Mismatch 'RelEQ Coin` is `[supplied, expected]`
    /// per `EncCBOR` instance in `cardano-ledger-core:Cardano.Ledger.BaseTypes`.
    #[test]
    fn test_encode_incomplete_withdrawals_golden() {
        let addr_bytes: Vec<u8> = std::iter::once(0xe0)
            .chain(std::iter::repeat_n(0x22, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::IncompleteWithdrawals {
            mismatches: vec![(addr_hex.clone(), 500_000, 750_000)],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        let ledger_tag = decode_ledger_tag(&mut dec);
        assert_eq!(ledger_tag, 9, "expected ConwayIncompleteWithdrawals");

        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1);
        let addr = dec.bytes().unwrap();
        assert_eq!(addr, addr_bytes.as_slice());
        let _ = dec.array().unwrap();
        let supplied = dec.u64().unwrap();
        let expected = dec.u64().unwrap();
        assert_eq!(supplied, 500_000, "Mismatch wire order: supplied first");
        assert_eq!(expected, 750_000, "Mismatch wire order: expected second");
    }

    /// Round-trip: encode tag 8 → decode via the n2c_client decoder used
    /// by `cardano-cli`. The decoded reason must name the variant and
    /// include the missing account bytes.
    #[test]
    fn test_roundtrip_tag8_through_n2c_decoder() {
        let addr_bytes: Vec<u8> = std::iter::once(0xe0)
            .chain(std::iter::repeat_n(0x33, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::WithdrawalsMissingAccounts {
            missing: vec![(addr_hex.clone(), 42)],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("ConwayWithdrawalsMissingAccounts"),
            "decoded reason should name variant, got: {reason}"
        );
        assert!(reason.contains(&addr_hex), "should include addr: {reason}");
        assert!(reason.contains("42"), "should include coin: {reason}");
    }

    /// Round-trip: encode tag 9 → decode via n2c_client. Decoded reason
    /// must include `supplied=` and `expected=` in the correct order.
    #[test]
    fn test_roundtrip_tag9_through_n2c_decoder() {
        let addr_bytes: Vec<u8> = std::iter::once(0xe0)
            .chain(std::iter::repeat_n(0x44, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::IncompleteWithdrawals {
            mismatches: vec![(addr_hex.clone(), 111, 222)],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("ConwayIncompleteWithdrawals"),
            "decoded reason should name variant, got: {reason}"
        );
        assert!(
            reason.contains("supplied=111"),
            "supplied not 111: {reason}"
        );
        assert!(
            reason.contains("expected=222"),
            "expected not 222: {reason}"
        );
    }

    // ── Issue #???  Conway GOV predicate failure CBOR encoding (Ledger tag 3) ──

    /// CBOR golden: `GovActionsDoNotExist` (Ledger tag 3, GOV tag 0).
    ///
    /// Wire: `array(2)[3, array(2)[0, [array(2)[txhash_32, action_idx], ...]]]`
    #[test]
    fn test_encode_gov_actions_do_not_exist_golden() {
        let hash_hex = "ab".repeat(32);
        let action_id = format!("{hash_hex}#3");
        let err = TxValidationError::GovActionsDoNotExist {
            action_ids: vec![action_id.clone()],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let (era, n) = decode_outer(&bytes);
        assert_eq!(era, 6);
        assert_eq!(n, 1);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        // ConwayLedgerPredFailure: array(2)[3, ...]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3 = ConwayGovFailure");

        // ConwayGovPredFailure: array(2)[0, [GovActionId,...]]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 0, "GOV tag 0 = GovActionsDoNotExist");

        // array(1)[GovActionId]
        assert_eq!(dec.array().unwrap(), Some(1));
        // GovActionId: array(2)[txhash_32, action_idx]
        assert_eq!(dec.array().unwrap(), Some(2));
        let hash_bytes = dec.bytes().unwrap();
        assert_eq!(hash_bytes, &[0xabu8; 32][..]);
        assert_eq!(dec.u32().unwrap(), 3);
    }

    /// Round-trip: `GovActionsDoNotExist` → encode → decode via n2c_client.
    #[test]
    fn test_roundtrip_gov_actions_do_not_exist() {
        let hash_hex = "cd".repeat(32);
        let action_id = format!("{hash_hex}#0");
        let err = TxValidationError::GovActionsDoNotExist {
            action_ids: vec![action_id.clone()],
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("GovActionsDoNotExist"),
            "must name variant, got: {reason}"
        );
        assert!(
            reason.contains("cdcdcd"),
            "must include hash fragment, got: {reason}"
        );
    }

    /// CBOR golden: `DisallowedVoters` (Ledger tag 3, GOV tag 5).
    ///
    /// Wire: `array(2)[3, array(2)[5, [array(2)[ voter, govActionId ], ...]]]`
    /// where voter = `array(2)[disc, hash28]`.
    #[test]
    fn test_encode_disallowed_voters_golden() {
        let cred_hex = "aa".repeat(28);
        let hash_hex = "bb".repeat(32);
        let action_id = format!("{hash_hex}#1");
        let err = TxValidationError::DisallowedVoters {
            // disc 2 = DRep key
            violations: vec![(2u8, cred_hex.clone(), action_id.clone())],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 5, "GOV tag 5 = DisallowedVoters");

        // array(1)[(voter, govActionId)]
        assert_eq!(dec.array().unwrap(), Some(1));
        // pair
        assert_eq!(dec.array().unwrap(), Some(2));
        // voter: array(2)[2, cred_hash28]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 2, "DRep-key disc");
        let cred_bytes = dec.bytes().unwrap();
        assert_eq!(cred_bytes, &[0xaau8; 28][..]);
        // govActionId: array(2)[hash32, idx]
        assert_eq!(dec.array().unwrap(), Some(2));
        let hash_bytes = dec.bytes().unwrap();
        assert_eq!(hash_bytes, &[0xbbu8; 32][..]);
        assert_eq!(dec.u32().unwrap(), 1);
    }

    /// Round-trip: `DisallowedVoters` → encode → decode via n2c_client.
    #[test]
    fn test_roundtrip_disallowed_voters() {
        let cred_hex = "11".repeat(28);
        let hash_hex = "22".repeat(32);
        let action_id = format!("{hash_hex}#0");
        let err = TxValidationError::DisallowedVoters {
            violations: vec![(4u8, cred_hex.clone(), action_id)], // disc 4 = SPO
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("DisallowedVoters"),
            "must name variant, got: {reason}"
        );
    }

    /// CBOR golden: `VotersDoNotExist` (Ledger tag 3, GOV tag 14).
    ///
    /// Wire: `array(2)[3, array(2)[14, [voter, ...]]]`
    #[test]
    fn test_encode_voters_do_not_exist_golden() {
        let cred_hex = "cc".repeat(28);
        let err = TxValidationError::VotersDoNotExist {
            // disc 0 = CC key
            voters: vec![(0u8, cred_hex.clone())],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 14, "GOV tag 14 = VotersDoNotExist");

        assert_eq!(dec.array().unwrap(), Some(1));
        // voter: array(2)[0, hash28]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 0, "CC-key disc");
        let cred_bytes = dec.bytes().unwrap();
        assert_eq!(cred_bytes, &[0xccu8; 28][..]);
    }

    /// Round-trip: `VotersDoNotExist` → encode → decode via n2c_client.
    #[test]
    fn test_roundtrip_voters_do_not_exist() {
        let cred_hex = "dd".repeat(28);
        let err = TxValidationError::VotersDoNotExist {
            voters: vec![(2u8, cred_hex.clone())], // disc 2 = DRep key
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("VotersDoNotExist"),
            "must name variant, got: {reason}"
        );
        assert!(
            reason.contains("DRep-key"),
            "must decode DRep-key disc, got: {reason}"
        );
    }

    /// CBOR golden: `VotingOnExpiredGovAction` (Ledger tag 3, GOV tag 9).
    #[test]
    fn test_encode_voting_on_expired_gov_action_golden() {
        let cred_hex = "ee".repeat(28);
        let hash_hex = "ff".repeat(32);
        let action_id = format!("{hash_hex}#2");
        let err = TxValidationError::VotingOnExpiredGovAction {
            expired_votes: vec![(4u8, cred_hex.clone(), action_id)], // disc 4 = SPO
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 9, "GOV tag 9 = VotingOnExpiredGovAction");

        assert_eq!(dec.array().unwrap(), Some(1));
        assert_eq!(dec.array().unwrap(), Some(2));
        // voter: array(2)[4, hash28]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 4, "SPO disc");
        let cred_bytes = dec.bytes().unwrap();
        assert_eq!(cred_bytes, &[0xeeu8; 28][..]);
        // govActionId
        assert_eq!(dec.array().unwrap(), Some(2));
        let hash_bytes = dec.bytes().unwrap();
        assert_eq!(hash_bytes, &[0xffu8; 32][..]);
        assert_eq!(dec.u32().unwrap(), 2);
    }

    /// CBOR golden: `ProposalReturnAccountDoesNotExist` (Ledger tag 3, GOV tag 16).
    ///
    /// Wire: `array(2)[3, array(2)[16, return_addr_bytes]]`
    #[test]
    fn test_encode_proposal_return_account_does_not_exist_golden() {
        let addr_bytes: Vec<u8> = std::iter::once(0xe0)
            .chain(std::iter::repeat_n(0x55, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::ProposalReturnAccountDoesNotExist {
            bad_addrs: vec![addr_hex.clone()],
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(
            dec.u8().unwrap(),
            16,
            "GOV tag 16 = ProposalReturnAccountDoesNotExist"
        );

        let decoded_addr = dec.bytes().unwrap();
        assert_eq!(decoded_addr, addr_bytes.as_slice());
    }

    /// Round-trip: `ProposalReturnAccountDoesNotExist` → encode → decode.
    #[test]
    fn test_roundtrip_proposal_return_account_does_not_exist() {
        let addr_bytes: Vec<u8> = std::iter::once(0xe1)
            .chain(std::iter::repeat_n(0x66, 28))
            .collect();
        let addr_hex = hex::encode(&addr_bytes);
        let err = TxValidationError::ProposalReturnAccountDoesNotExist {
            bad_addrs: vec![addr_hex.clone()],
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("ProposalReturnAccountDoesNotExist"),
            "must name variant, got: {reason}"
        );
        assert!(
            reason.contains(&addr_hex),
            "must include addr hex, got: {reason}"
        );
    }

    /// CBOR golden: `UnelectedCommitteeVoters` (Ledger tag 3, GOV tag 18).
    ///
    /// Wire: `array(2)[3, array(2)[18, [Credential, ...]]]`
    /// where Credential = `array(2)[disc, hash28]`.
    #[test]
    fn test_encode_unelected_committee_voters_golden() {
        let cred_hex = "77".repeat(28);
        let err = TxValidationError::UnelectedCommitteeVoters {
            hot_credentials: vec![(0u8, cred_hex.clone())], // disc 0 = key
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(
            dec.u8().unwrap(),
            18,
            "GOV tag 18 = UnelectedCommitteeVoters"
        );

        assert_eq!(dec.array().unwrap(), Some(1));
        // Credential: array(2)[0, hash28]
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 0, "key disc");
        let cred_bytes = dec.bytes().unwrap();
        assert_eq!(cred_bytes, &[0x77u8; 28][..]);
    }

    /// Round-trip: `UnelectedCommitteeVoters` → encode → decode via n2c_client.
    #[test]
    fn test_roundtrip_unelected_committee_voters() {
        let cred_hex = "88".repeat(28);
        let err = TxValidationError::UnelectedCommitteeVoters {
            hot_credentials: vec![(1u8, cred_hex.clone())], // disc 1 = script
        };
        let bytes = encode_apply_tx_err(&err, 6);
        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("UnelectedCommitteeVoters"),
            "must name variant, got: {reason}"
        );
        assert!(
            reason.contains("1:"),
            "must decode script disc=1, got: {reason}"
        );
    }

    /// CBOR golden: `ProposalDepositIncorrect` (Ledger tag 3, GOV tag 4).
    ///
    /// Payload is `Mismatch 'RelEQ Coin`, FLATTENED into the constructor
    /// array (encCBORGroup), unlike the Mismatch values inside Ledger tag 9
    /// `IncompleteWithdrawals`, which are map values and therefore nested.
    ///
    /// Expected wire (ConwayLedgerPredFailure only):
    ///   `8203 8304 <declared> <expected>`
    ///   = array(2)[3, array(3)[4, declared, expected]]
    #[test]
    fn test_encode_proposal_deposit_incorrect_golden() {
        let err = TxValidationError::ProposalDepositIncorrect {
            declared: 99_999_999_999,
            expected: 100_000_000_000,
        };
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        encode_conway_ledger_pred_failure(&mut enc, &err);
        let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        eprintln!("ProposalDepositIncorrect bytes = {hex}");

        // Mismatch is FLATTENED into the constructor array — array(3), not
        // array(2) wrapping a nested array(2). cardano-cli rejects the nested
        // form with DeserialiseFailure "expected word".
        let mut dec = Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(2), "outer array(2)");
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3 = ConwayGovFailure");
        assert_eq!(
            dec.array().unwrap(),
            Some(3),
            "gov array(3) — Mismatch flattened"
        );
        assert_eq!(dec.u8().unwrap(), 4, "GOV tag 4 = ProposalDepositIncorrect");
        assert_eq!(dec.u64().unwrap(), 99_999_999_999, "declared");
        assert_eq!(dec.u64().unwrap(), 100_000_000_000, "expected");
        assert_eq!(dec.position(), buf.len(), "no trailing bytes");
    }

    // ── #1025: `MissingRedeemers` (Ledger tag 1, UTXOW tag 10) ──

    /// CBOR golden: a single `Minting` purpose.
    ///
    /// `MissingRedeemers (NonEmpty (PlutusPurpose AsItem era, ScriptHash))`
    /// = list of `array(2)[ array(2)[tag, item], bstr28 ]`.
    ///
    /// For a minting purpose the item IS the policy id, so the same 28 bytes
    /// appear twice — once as the purpose's item and once as the script hash.
    /// That is upstream's shape, not a duplication bug.
    #[test]
    fn test_encode_missing_redeemers_minting_golden() {
        let policy = "ab".repeat(28);
        let err = TxValidationError::MissingRedeemersUTXOW {
            entries: vec![(
                PlutusPurposeItem::Minting {
                    policy_id: policy.clone(),
                },
                policy.clone(),
            )],
        };

        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        encode_conway_ledger_pred_failure(&mut enc, &err);

        let mut expected = vec![
            0x82, 0x01, // ConwayLedgerPredFailure: array(2)[1, ...] (UtxowFailure)
            0x82, 0x0a, // ConwayUtxowPredFailure: array(2)[10, ...]
            0x81, // NonEmpty list of 1
            0x82, // (purpose, scripthash) pair
            0x82, 0x01, // PlutusPurpose AsItem: array(2)[1 = ConwayMinting, item]
            0x58, 0x1c, // bstr(28) policy id
        ];
        expected.extend(std::iter::repeat_n(0xABu8, 28));
        expected.extend([0x58, 0x1c]); // bstr(28) script hash
        expected.extend(std::iter::repeat_n(0xABu8, 28));

        assert_eq!(buf, expected, "MissingRedeemers must be byte-exact");
    }

    /// `AsItem` must encode the ITEM ONLY — never the index.
    ///
    /// `newtype AsItem ix it = AsItem { unAsItem :: it }` derives EncCBOR via
    /// the newtype, so `ix` is phantom. `ExtraRedeemers` (tag 15) is the `AsIx`
    /// counterpart and DOES write an index; encoding one like the other is the
    /// easiest way to produce a frame cardano-cli cannot decode. This asserts
    /// the two are shaped differently on purpose.
    #[test]
    fn as_item_encodes_no_index_unlike_as_ix() {
        let account = format!("e1{}", "22".repeat(28)); // 29-byte reward account
        let sh = "cd".repeat(28);
        let mut item_buf = Vec::new();
        encode_conway_ledger_pred_failure(
            &mut Encoder::new(&mut item_buf),
            &TxValidationError::MissingRedeemersUTXOW {
                entries: vec![(
                    PlutusPurposeItem::Withdrawing {
                        account: account.clone(),
                    },
                    sh.clone(),
                )],
            },
        );
        // array(2)[3 = ConwayWithdrawing, bstr(29)] — no index between them.
        assert_eq!(
            &item_buf[6..9],
            &[0x82, 0x03, 0x58],
            "AsItem: tag then straight to the item"
        );

        let mut ix_buf = Vec::new();
        encode_conway_ledger_pred_failure(
            &mut Encoder::new(&mut ix_buf),
            &TxValidationError::ExtraRedeemersUTXOW {
                purposes: vec![(3, 7)],
            },
        );
        // array(2)[3, 7] — AsIx writes the index as the second element.
        assert_eq!(
            &ix_buf[5..8],
            &[0x82, 0x03, 0x07],
            "AsIx: tag then the index"
        );
    }

    /// Malformed hex must fall back to the generic reason rather than emit a
    /// short or wrong list — the repo's standing rule that an unverified arm
    /// can be worse than the generic one.
    #[test]
    fn missing_redeemers_falls_back_on_unparseable_input() {
        let err = TxValidationError::MissingRedeemersUTXOW {
            entries: vec![(
                PlutusPurposeItem::Minting {
                    policy_id: "not-hex".to_string(),
                },
                "ab".repeat(28),
            )],
        };
        let mut buf = Vec::new();
        encode_conway_ledger_pred_failure(&mut Encoder::new(&mut buf), &err);
        assert_ne!(
            &buf[..4],
            &[0x82, 0x01, 0x82, 0x0a],
            "an unparseable policy id must NOT produce a tag-10 frame"
        );
    }

    // ── #1025: `MalformedProposal` (Ledger tag 3, GOV tag 1) ──

    /// CBOR golden: `MalformedProposal` carries the WHOLE `GovAction`, so a
    /// `ParameterChange` with a single `maxTxSize = 0` field must reproduce
    /// the same bytes the transaction-body encoder writes for that action.
    ///
    /// `GovAction::ParameterChange = array(4)[0, prev_action_id, ppu, policy]`,
    /// and the `ProtocolParamUpdate` is a SPARSE integer-keyed map — key 3 is
    /// `maxTransactionSize` (key 2 is `maxBlockBodySize`). Do not confuse this
    /// map with the POSITIONAL `array(31)` `GetCurrentPParams` replies with,
    /// where the two orderings differ.
    #[test]
    fn test_encode_malformed_proposal_golden() {
        use dugite_primitives::transaction::{GovAction, ProtocolParamUpdate};

        let ppu = ProtocolParamUpdate {
            max_tx_size: Some(0),
            ..Default::default()
        };
        let err = TxValidationError::MalformedProposalGOV {
            action: Box::new(GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ppu),
                policy_hash: None,
            }),
        };

        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        encode_conway_ledger_pred_failure(&mut enc, &err);

        let expected = vec![
            0x82, 0x03, // ConwayLedgerPredFailure: array(2)[3, ...]
            0x82, 0x01, // ConwayGovPredFailure: array(2)[1, ...]
            0x84, // GovAction: array(4)
            0x00, // ParameterChange discriminator
            0xf6, // prev_action_id: null
            0xa1, 0x03, 0x00, // ppu: {3: 0}  (key 3 = maxTransactionSize)
            0xf6, // policy_hash: null
        ];
        assert_eq!(buf, expected, "MalformedProposal must be byte-exact");
    }

    /// The payload must be the action from the transaction-body encoder, not a
    /// re-implementation: assert the frame embeds exactly what
    /// `encode_gov_action` produces for the same value. A hand-rolled copy in
    /// the rejection path is how #932/#938 both went wrong.
    #[test]
    fn test_malformed_proposal_reuses_the_body_gov_action_encoder() {
        use dugite_primitives::transaction::{GovAction, ProtocolParamUpdate};

        let action = GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ProtocolParamUpdate {
                max_block_body_size: Some(0),
                gov_action_lifetime: Some(0),
                ..Default::default()
            }),
            policy_hash: None,
        };
        let raw = dugite_serialization::encode_gov_action(&action);

        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        encode_conway_ledger_pred_failure(
            &mut enc,
            &TxValidationError::MalformedProposalGOV {
                action: Box::new(action),
            },
        );

        assert_eq!(
            &buf[4..],
            &raw[..],
            "the GOV tag-1 payload must be byte-identical to the body encoder's output"
        );
        assert_eq!(&buf[..4], &[0x82, 0x03, 0x82, 0x01]);
    }

    // ── Issue #915: `InvalidPrevGovActionId` (Ledger tag 3, GOV tag 8) ──

    /// CBOR golden: `InvalidPrevGovActionId` with an `InfoAction` proposal.
    ///
    /// Exercises the worked example from dugite issue #915: deposit
    /// 1,000,000 lovelace; mainnet keyhash return address with credential
    /// `0x11` x 28; `InfoAction`; anchor url `"https://x"`, hash `0xAA` x 32.
    /// Wire (bypassing the outer HFC/era wrapper, i.e. just the
    /// `ConwayLedgerPredFailure`):
    ///
    /// ```text
    /// 8203820884 1a000f4240 581de1<11x28> 8106 82 6968747470733a2f2f78 5820<aax32>
    /// ```
    ///
    /// = `array(2)[3, array(2)[8, array(4)[1000000, bstr(29), [6], [url, hash]]]]`
    #[test]
    fn test_encode_invalid_prev_gov_action_id_golden() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
        use dugite_primitives::value::Lovelace;

        let return_addr: Vec<u8> = std::iter::once(0xE1)
            .chain(std::iter::repeat_n(0x11, 28))
            .collect();
        let proposal = ProposalProcedure {
            deposit: Lovelace(1_000_000),
            return_addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: "https://x".to_string(),
                data_hash: Hash32::from_bytes([0xAA; 32]),
            },
        };
        let err = TxValidationError::InvalidPrevGovActionId {
            action_index: 0,
            action_type: "InfoAction".to_string(),
            prev_action_id: None,
            proposal: Box::new(proposal),
        };

        // Encode just the `ConwayLedgerPredFailure` sub-tree (bypassing the
        // outer HFC/era wrapper) to assert the exact worked-example bytes.
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        encode_conway_ledger_pred_failure(&mut enc, &err);

        let mut expected = vec![
            0x82, 0x03, // ConwayLedgerPredFailure: array(2)[3, ...]
            0x82, 0x08, // ConwayGovPredFailure: array(2)[8, ...]
            0x84, // ProposalProcedure: array(4)
            0x1a, 0x00, 0x0f, 0x42, 0x40, // deposit = 1_000_000
            0x58, 0x1d, // return_addr: bstr(29)
        ];
        expected.push(0xe1); // mainnet keyhash reward-account header
        expected.extend(std::iter::repeat_n(0x11u8, 28));
        expected.extend([0x81, 0x06]); // gov_action: InfoAction = array(1)[6]
        expected.push(0x82); // anchor: array(2)
        expected.push(0x69); // text(9)
        expected.extend(b"https://x");
        expected.push(0x58);
        expected.push(0x20); // bstr(32)
        expected.extend([0xAAu8; 32]);

        assert_eq!(buf, expected, "InvalidPrevGovActionId must be byte-exact");
    }

    /// Full-envelope check: `InvalidPrevGovActionId` through
    /// `encode_apply_tx_err` decodes with the correct Ledger/GOV tags and
    /// round-trips the deposit + gov_action fields.
    #[test]
    fn test_encode_invalid_prev_gov_action_id_full_envelope() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
        use dugite_primitives::value::Lovelace;

        let return_addr: Vec<u8> = std::iter::once(0xE0)
            .chain(std::iter::repeat_n(0x22, 28))
            .collect();
        let proposal = ProposalProcedure {
            deposit: Lovelace(500_000_000),
            return_addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::from_bytes([0xBB; 32]),
            },
        };
        let err = TxValidationError::InvalidPrevGovActionId {
            action_index: 1,
            action_type: "InfoAction".to_string(),
            prev_action_id: None,
            proposal: Box::new(proposal),
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let (era, n) = decode_outer(&bytes);
        assert_eq!(era, 6);
        assert_eq!(n, 1);

        let mut dec = Decoder::new(&bytes);
        let _ = dec.array().unwrap();
        let _ = dec.array().unwrap();
        let _ = dec.u16().unwrap();
        let _ = dec.array().unwrap();

        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 3, "Ledger tag 3 = ConwayGovFailure");
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u8().unwrap(), 8, "GOV tag 8 = InvalidPrevGovActionId");

        // The whole ProposalProcedure is the single payload item.
        assert_eq!(dec.array().unwrap(), Some(4));
        let deposit = dec.u64().unwrap();
        assert_eq!(deposit, 500_000_000);
        let addr = dec.bytes().unwrap();
        assert_eq!(addr.len(), 29);
        assert_eq!(addr[0], 0xE0);
        // gov_action: InfoAction = array(1)[6]
        assert_eq!(dec.array().unwrap(), Some(1));
        assert_eq!(dec.u8().unwrap(), 6);
        // anchor: array(2)[url, hash]
        assert_eq!(dec.array().unwrap(), Some(2));
        let url = dec.str().unwrap();
        assert_eq!(url, "https://example.com");
        let hash = dec.bytes().unwrap();
        assert_eq!(hash, &[0xBBu8; 32][..]);
    }

    /// Round-trip: encode → decode via the n2c_client decoder. The generic
    /// `ConwayGovPredFailure` fallback (no dedicated tag-8 decode arm yet)
    /// must still name the tag rather than erroring out or panicking.
    #[test]
    fn test_roundtrip_invalid_prev_gov_action_id_through_n2c_decoder() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
        use dugite_primitives::value::Lovelace;

        let return_addr: Vec<u8> = std::iter::once(0xE1)
            .chain(std::iter::repeat_n(0x33, 28))
            .collect();
        let proposal = ProposalProcedure {
            deposit: Lovelace(42),
            return_addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: "https://y".to_string(),
                data_hash: Hash32::from_bytes([0xCC; 32]),
            },
        };
        let err = TxValidationError::InvalidPrevGovActionId {
            action_index: 0,
            action_type: "InfoAction".to_string(),
            prev_action_id: None,
            proposal: Box::new(proposal),
        };
        let bytes = encode_apply_tx_err(&err, 6);

        let mut dec = Decoder::new(&bytes);
        let reason = crate::n2c_client::decode_reject_reason(&mut dec).unwrap();
        assert!(
            reason.contains("tag=8"),
            "must name GOV tag 8, got: {reason}"
        );
    }
}
