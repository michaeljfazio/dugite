//! Tag-path -> constructor table for Conway `ApplyTxErr` predicate failures.
//!
//! This file is TRANSCRIBED from IntersectMBO/cardano-ledger source, never derived from
//! dugite's own encoder: every [`Variant`] quotes the file and line it was read from, at the
//! revision named by [`LEDGER_REVISION`].  The harness decodes the bytes `encode_apply_tx_err`
//! emits through it, so a name that is reported is a name the wire bytes carry.
//!
//! A failure value is a CBOR array `[tag, field, ...]`.  [`Rule`] is one predicate-failure type
//! (one Haskell sum type), [`Variant`] one of its constructors.  A variant that wraps the failure
//! of another rule names it in `nested`, and the wrapped failure sits at array element
//! `nested_field`.
//!
//! Below `RULES`, `PAYLOAD_RULES` / `NDC_PAYLOAD_LEAVES` / `PAYLOAD_NOTES` transcribe the payload
//! encodings needed to name a failure whose constructor alone is not enough (`CollectErrors`,
//! `ValidationTagMismatch`, credentials), and `ARITY` / `PAYLOAD_ARITY` give each constructor's
//! array length (tag element included) so a decoder can reject a wrong-length array.
//!
//! No protocol-version gating lives here: the PV decides which constructor the ledger RAISES
//! (e.g. `IncorrectDepositDELEG` below PV11, `DepositIncorrectDELEG`/`RefundIncorrectDELEG` from
//! PV11), and every constructor keeps its own tag in every version.

/// One constructor of a predicate-failure type.
#[derive(Debug, Clone, Copy)]
pub struct Variant {
    /// The array tag (element 0 of the failure array).
    pub tag: u64,
    /// The Haskell constructor name, exactly as spelled in cardano-ledger.
    pub name: &'static str,
    /// The rule whose failure this constructor wraps, if it wraps one.
    pub nested: Option<&'static str>,
    /// Array element holding the wrapped failure (1 unless the constructor has other leading fields).
    pub nested_field: usize,
    /// `path/to/File.hs:LINE` of the `Sum ... tag` line this row was read from.
    pub source: &'static str,
}

/// One predicate-failure sum type.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// The Haskell type name, e.g. `ConwayUtxoPredFailure`.
    pub name: &'static str,
    pub variants: &'static [Variant],
}

/// The cardano-ledger revision every `source` below was read at.
/// Tag `cardano-ledger-conway-1.22.1.0`: the newest conway release in the CHaP index state
/// (2026-05-02) that cardano-node 11.0.1's `cabal.project` pins, and the one its
/// `cardano-ledger-conway >= 1.22.1.0` bound resolves to.  The shelley POOL rule is
/// byte-identical at the `cardano-ledger-shelley-1.18.1.0` tag (`b7c17cf3`).
pub const LEDGER_REVISION: &str = "226b002d5b5e83e24355f8a28ab214f3259eabda";

/// The root type of an `ApplyTxErr` failure list element.
pub const ROOT_RULE: &str = "ConwayLedgerPredFailure";

/// Look a rule up by Haskell type name.
pub fn rule(name: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.name == name)
}

/// Every rule, transcribed from the files named in the comments.
pub static RULES: &[Rule] = &[
    // LEDGER (no tag 0: tags start at 1).
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs
    Rule {
        name: "ConwayLedgerPredFailure",
        variants: &[
            Variant {
                tag: 1,
                name: "ConwayUtxowFailure",
                nested: Some("ConwayUtxowPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:260",
            },
            Variant {
                tag: 2,
                name: "ConwayCertsFailure",
                nested: Some("ConwayCertsPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:261",
            },
            Variant {
                tag: 3,
                name: "ConwayGovFailure",
                nested: Some("ConwayGovPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:262",
            },
            Variant {
                tag: 4,
                name: "ConwayWdrlNotDelegatedToDRep",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:263",
            },
            Variant {
                tag: 5,
                name: "ConwayTreasuryValueMismatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:265",
            },
            Variant {
                tag: 6,
                name: "ConwayTxRefScriptsSizeTooBig",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:266",
            },
            Variant {
                tag: 7,
                name: "ConwayMempoolFailure",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:267",
            },
            Variant {
                tag: 8,
                name: "ConwayWithdrawalsMissingAccounts",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:268",
            },
            Variant {
                tag: 9,
                name: "ConwayIncompleteWithdrawals",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:269",
            },
        ],
    },
    // UTXOW.  Tag 13 PPViewHashesDontMatch (flattened Mismatch) and tag 18
    // ScriptIntegrityHashMismatch (Mismatch + preimage) are both present at this revision.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs
    Rule {
        name: "ConwayUtxowPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "UtxoFailure",
                nested: Some("ConwayUtxoPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:250",
            },
            Variant {
                tag: 1,
                name: "InvalidWitnessesUTXOW",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:251",
            },
            Variant {
                tag: 2,
                name: "MissingVKeyWitnessesUTXOW",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:252",
            },
            Variant {
                tag: 3,
                name: "MissingScriptWitnessesUTXOW",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:253",
            },
            Variant {
                tag: 4,
                name: "ScriptWitnessNotValidatingUTXOW",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:254",
            },
            Variant {
                tag: 5,
                name: "MissingTxBodyMetadataHash",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:255",
            },
            Variant {
                tag: 6,
                name: "MissingTxMetadata",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:256",
            },
            Variant {
                tag: 7,
                name: "ConflictingMetadataHash",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:257",
            },
            Variant {
                tag: 8,
                name: "InvalidMetadata",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:258",
            },
            Variant {
                tag: 9,
                name: "ExtraneousScriptWitnessesUTXOW",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:259",
            },
            Variant {
                tag: 10,
                name: "MissingRedeemers",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:260",
            },
            Variant {
                tag: 11,
                name: "MissingRequiredDatums",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:261",
            },
            Variant {
                tag: 12,
                name: "NotAllowedSupplementalDatums",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:262",
            },
            Variant {
                tag: 13,
                name: "PPViewHashesDontMatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:263",
            },
            Variant {
                tag: 14,
                name: "UnspendableUTxONoDatumHash",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:264",
            },
            Variant {
                tag: 15,
                name: "ExtraRedeemers",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:265",
            },
            Variant {
                tag: 16,
                name: "MalformedScriptWitnesses",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:266",
            },
            Variant {
                tag: 17,
                name: "MalformedReferenceScripts",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:267",
            },
            Variant {
                tag: 18,
                name: "ScriptIntegrityHashMismatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxow.hs:268",
            },
        ],
    },
    // UTXO.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs
    Rule {
        name: "ConwayUtxoPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "UtxosFailure",
                nested: Some("ConwayUtxosPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:318",
            },
            Variant {
                tag: 1,
                name: "BadInputsUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:319",
            },
            Variant {
                tag: 2,
                name: "OutsideValidityIntervalUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:320",
            },
            Variant {
                tag: 3,
                name: "MaxTxSizeUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:321",
            },
            Variant {
                tag: 4,
                name: "InputSetEmptyUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:322",
            },
            Variant {
                tag: 5,
                name: "FeeTooSmallUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:323",
            },
            Variant {
                tag: 6,
                name: "ValueNotConservedUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:324",
            },
            Variant {
                tag: 7,
                name: "WrongNetwork",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:325",
            },
            Variant {
                tag: 8,
                name: "WrongNetworkWithdrawal",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:326",
            },
            Variant {
                tag: 9,
                name: "OutputTooSmallUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:327",
            },
            Variant {
                tag: 10,
                name: "OutputBootAddrAttrsTooBig",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:328",
            },
            Variant {
                tag: 11,
                name: "OutputTooBigUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:329",
            },
            Variant {
                tag: 12,
                name: "InsufficientCollateral",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:330",
            },
            Variant {
                tag: 13,
                name: "ScriptsNotPaidUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:331",
            },
            Variant {
                tag: 14,
                name: "ExUnitsTooBigUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:332",
            },
            Variant {
                tag: 15,
                name: "CollateralContainsNonADA",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:333",
            },
            Variant {
                tag: 16,
                name: "WrongNetworkInTxBody",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:334",
            },
            Variant {
                tag: 17,
                name: "OutsideForecast",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:335",
            },
            Variant {
                tag: 18,
                name: "TooManyCollateralInputs",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:336",
            },
            Variant {
                tag: 19,
                name: "NoCollateralInputs",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:337",
            },
            Variant {
                tag: 20,
                name: "IncorrectTotalCollateralField",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:338",
            },
            Variant {
                tag: 21,
                name: "BabbageOutputTooSmallUTxO",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:339",
            },
            Variant {
                tag: 22,
                name: "BabbageNonDisjointRefInputs",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs:340",
            },
        ],
    },
    // UTXOS.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxos.hs
    Rule {
        name: "ConwayUtxosPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "ValidationTagMismatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxos.hs:138",
            },
            Variant {
                tag: 1,
                name: "CollectErrors",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxos.hs:139",
            },
        ],
    },
    // CERTS.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs
    Rule {
        name: "ConwayCertsPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "WithdrawalsNotInRewardsCERTS",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs:171",
            },
            Variant {
                tag: 1,
                name: "CertFailure",
                nested: Some("ConwayCertPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs:172",
            },
        ],
    },
    // CERT (no tag 0: tags start at 1).
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Cert.hs
    Rule {
        name: "ConwayCertPredFailure",
        variants: &[
            Variant {
                tag: 1,
                name: "DelegFailure",
                nested: Some("ConwayDelegPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Cert.hs:276",
            },
            Variant {
                tag: 2,
                name: "PoolFailure",
                nested: Some("ShelleyPoolPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Cert.hs:277",
            },
            Variant {
                tag: 3,
                name: "GovCertFailure",
                nested: Some("ConwayGovCertPredFailure"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Cert.hs:278",
            },
        ],
    },
    // DELEG (no tag 0).  Tags 7 and 8 encode their Mismatch with `To` (a nested
    // 2-array), not `ToGroup`; payload only, no effect on the tag path.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs
    Rule {
        name: "ConwayDelegPredFailure",
        variants: &[
            Variant {
                tag: 1,
                name: "IncorrectDepositDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:131",
            },
            Variant {
                tag: 2,
                name: "StakeKeyRegisteredDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:133",
            },
            Variant {
                tag: 3,
                name: "StakeKeyNotRegisteredDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:135",
            },
            Variant {
                tag: 4,
                name: "StakeKeyHasNonZeroAccountBalanceDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:137",
            },
            Variant {
                tag: 5,
                name: "DelegateeDRepNotRegisteredDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:139",
            },
            Variant {
                tag: 6,
                name: "DelegateeStakePoolNotRegisteredDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:141",
            },
            Variant {
                tag: 7,
                name: "DepositIncorrectDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:143",
            },
            Variant {
                tag: 8,
                name: "RefundIncorrectDELEG",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs:145",
            },
        ],
    },
    // POOL (cardano-ledger-shelley, used unchanged by Conway).  Hand-written
    // `encodeListLen` encoding, not `Sum`; `source` is the line carrying the tag literal.  Tag 2
    // (`WrongCertificateTypePOOL`) was removed from the type and is never encoded.
    // Source: eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs
    Rule {
        name: "ShelleyPoolPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "StakePoolNotRegisteredOnKeyPOOL",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:160",
            },
            Variant {
                tag: 1,
                name: "StakePoolRetirementWrongEpochPOOL",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:163",
            },
            Variant {
                tag: 3,
                name: "StakePoolCostTooLowPOOL",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:168",
            },
            Variant {
                tag: 4,
                name: "WrongNetworkPOOL",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:170",
            },
            Variant {
                tag: 5,
                name: "PoolMedataHashTooBig",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:172",
            },
            Variant {
                tag: 6,
                name: "VRFKeyHashAlreadyRegistered",
                nested: None,
                nested_field: 0,
                source: "eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs:174",
            },
        ],
    },
    // GOVCERT.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs
    Rule {
        name: "ConwayGovCertPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "ConwayDRepAlreadyRegistered",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:134",
            },
            Variant {
                tag: 1,
                name: "ConwayDRepNotRegistered",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:135",
            },
            Variant {
                tag: 2,
                name: "ConwayDRepIncorrectDeposit",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:136",
            },
            Variant {
                tag: 3,
                name: "ConwayCommitteeHasPreviouslyResigned",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:137",
            },
            Variant {
                tag: 4,
                name: "ConwayDRepIncorrectRefund",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:138",
            },
            Variant {
                tag: 5,
                name: "ConwayCommitteeIsUnknown",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs:139",
            },
        ],
    },
    // GOV.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs
    Rule {
        name: "ConwayGovPredFailure",
        variants: &[
            Variant {
                tag: 0,
                name: "GovActionsDoNotExist",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:267",
            },
            Variant {
                tag: 1,
                name: "MalformedProposal",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:269",
            },
            Variant {
                tag: 2,
                name: "ProposalProcedureNetworkIdMismatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:271",
            },
            Variant {
                tag: 3,
                name: "TreasuryWithdrawalsNetworkIdMismatch",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:273",
            },
            Variant {
                tag: 4,
                name: "ProposalDepositIncorrect",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:275",
            },
            Variant {
                tag: 5,
                name: "DisallowedVoters",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:277",
            },
            Variant {
                tag: 6,
                name: "ConflictingCommitteeUpdate",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:279",
            },
            Variant {
                tag: 7,
                name: "ExpirationEpochTooSmall",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:281",
            },
            Variant {
                tag: 8,
                name: "InvalidPrevGovActionId",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:283",
            },
            Variant {
                tag: 9,
                name: "VotingOnExpiredGovAction",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:285",
            },
            Variant {
                tag: 10,
                name: "ProposalCantFollow",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:287",
            },
            Variant {
                tag: 11,
                name: "InvalidGuardrailsScriptHash",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:289",
            },
            Variant {
                tag: 12,
                name: "DisallowedProposalDuringBootstrap",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:291",
            },
            Variant {
                tag: 13,
                name: "DisallowedVotesDuringBootstrap",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:293",
            },
            Variant {
                tag: 14,
                name: "VotersDoNotExist",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:295",
            },
            Variant {
                tag: 15,
                name: "ZeroTreasuryWithdrawals",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:297",
            },
            Variant {
                tag: 16,
                name: "ProposalReturnAccountDoesNotExist",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:299",
            },
            Variant {
                tag: 17,
                name: "TreasuryWithdrawalReturnAccountsDoNotExist",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:301",
            },
            Variant {
                tag: 18,
                name: "UnelectedCommitteeVoters",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:303",
            },
        ],
    },
];

// ---------------------------------------------------------------------------
// Payload encodings (not part of RULES: nothing in RULES names these as a `nested` rule, they hang
// off a payload field).  Same revision as LEDGER_REVISION.
// ---------------------------------------------------------------------------

/// Payload rules, same shape as [`RULES`].  `nested` here names another entry of this table.
pub static PAYLOAD_RULES: &[Rule] = &[
    // ConwayUtxosPredFailure.CollectErrors carries a NonEmpty (CollectError era): a CBOR LIST of these
    // arrays (definite length up to 23 elements, indefinite-length above).
    // Source: eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs
    Rule {
        name: "CollectError",
        variants: &[
            Variant {
                tag: 0,
                name: "NoRedeemer",
                nested: None,
                nested_field: 0,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs:103",
            },
            Variant {
                tag: 1,
                name: "NoWitness",
                nested: None,
                nested_field: 0,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs:104",
            },
            Variant {
                tag: 2,
                name: "NoCostModel",
                nested: None,
                nested_field: 0,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs:105",
            },
            Variant {
                tag: 3,
                name: "BadTranslation",
                nested: Some("ConwayContextError"),
                nested_field: 1,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs:106",
            },
        ],
    },
    // ContextError ConwayEra = ConwayContextError ConwayEra (Conway TxInfo.hs:142).  Tags start at 8 so
    // they cannot clash with the Babbage/Alonzo tags nested under tag 8.
    // Source: eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs
    Rule {
        name: "ConwayContextError",
        variants: &[
            Variant {
                tag: 8,
                name: "BabbageContextError",
                nested: Some("BabbageContextError"),
                nested_field: 1,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:234",
            },
            Variant {
                tag: 9,
                name: "CertificateNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:236",
            },
            Variant {
                tag: 10,
                name: "PlutusPurposeNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:238",
            },
            Variant {
                tag: 11,
                name: "CurrentTreasuryFieldNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:240",
            },
            Variant {
                tag: 12,
                name: "VotingProceduresFieldNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:242",
            },
            Variant {
                tag: 13,
                name: "ProposalProceduresFieldNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:244",
            },
            Variant {
                tag: 14,
                name: "TreasuryDonationFieldNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:246",
            },
            Variant {
                tag: 15,
                name: "ReferenceInputsNotDisjointFromInputs",
                nested: None,
                nested_field: 0,
                source: "eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs:248",
            },
        ],
    },
    // Tags 1 and 7 are the constructors of AlonzoContextError (TranslationLogicMissingInput,
    // TimeTranslationPastHorizon), which Babbage encodes FLAT: the array is [1, txin] / [7, text], with no
    // extra array level for the AlonzoContextError wrapper.  Tag 3 is unused.  Alonzo's own
    // AlonzoContextError encoding (Alonzo/Plutus/TxInfo.hs:192,194) uses the same tags 1 and 7.
    // Source: eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs
    Rule {
        name: "BabbageContextError",
        variants: &[
            Variant {
                tag: 0,
                name: "ByronTxOutInContext",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:274",
            },
            Variant {
                tag: 1,
                name: "TranslationLogicMissingInput",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:276",
            },
            Variant {
                tag: 2,
                name: "RedeemerPointerPointsToNothing",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:278",
            },
            Variant {
                tag: 4,
                name: "InlineDatumsNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:280",
            },
            Variant {
                tag: 5,
                name: "ReferenceScriptsNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:282",
            },
            Variant {
                tag: 6,
                name: "ReferenceInputsNotSupported",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:284",
            },
            Variant {
                tag: 7,
                name: "TimeTranslationPastHorizon",
                nested: None,
                nested_field: 0,
                source: "eras/babbage/impl/src/Cardano/Ledger/Babbage/TxInfo.hs:286",
            },
        ],
    },
    // Payload of ByronTxOutInContext / InlineDatumsNotSupported / ReferenceScriptsNotSupported (field 1).
    // Source: libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs
    Rule {
        name: "TxOutSource",
        variants: &[
            Variant {
                tag: 0,
                name: "TxOutFromInput",
                nested: None,
                nested_field: 0,
                source: "libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs:100",
            },
            Variant {
                tag: 1,
                name: "TxOutFromOutput",
                nested: None,
                nested_field: 0,
                source: "libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs:101",
            },
        ],
    },
    // Field 2 of ConwayUtxosPredFailure.ValidationTagMismatch (field 1 is IsValid, a bare CBOR bool).
    // FailedUnexpectedly carries a NonEmpty FailureDescription, i.e. a LIST of FailureDescription arrays.
    // Source: eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs
    Rule {
        name: "TagMismatchDescription",
        variants: &[
            Variant {
                tag: 0,
                name: "PassedUnexpectedly",
                nested: None,
                nested_field: 0,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs:335",
            },
            Variant {
                tag: 1,
                name: "FailedUnexpectedly",
                nested: None,
                nested_field: 0,
                source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs:336",
            },
        ],
    },
    // Elements of the FailedUnexpectedly list.  Tag 0 is intentionally absent (a removed constructor).
    // Source: eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs
    Rule {
        name: "FailureDescription",
        variants: &[Variant {
            tag: 1,
            name: "PlutusFailure",
            nested: None,
            nested_field: 0,
            source: "eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs:305",
        }],
    },
    // Hand-written encodeListLen 2 (not Sum); `source` is the line carrying the tag literal.  Payload of
    // StakeKeyNotRegisteredDELEG (field 1), ConwayDRepNotRegistered, ConwayDRepAlreadyRegistered (field 1),
    // and every other Credential field.  Field 1 is the 28-byte hash as a CBOR byte string.
    // Source: libs/cardano-ledger-core/src/Cardano/Ledger/Credential.hs
    Rule {
        name: "Credential",
        variants: &[
            Variant {
                tag: 0,
                name: "KeyHashObj",
                nested: None,
                nested_field: 0,
                source: "libs/cardano-ledger-core/src/Cardano/Ledger/Credential.hs:320",
            },
            Variant {
                tag: 1,
                name: "ScriptHashObj",
                nested: None,
                nested_field: 0,
                source: "libs/cardano-ledger-core/src/Cardano/Ledger/Credential.hs:321",
            },
        ],
    },
];

/// The eight NDC `ledger-rules` names that are leaf constructors under
/// `ConwayUtxosPredFailure.CollectErrors` (UTXOS tag 1): `(NDC name, tag path)`.  A path element is
/// `Type#tag`; `[..]` is the CollectError list element, then `>` descends into field 1.  Example: a
/// `CollectErrors` failure `[1, [ [3, [8, [0, src]]] ]]` is the `ByronTxOutInContext` leaf.
pub static NDC_PAYLOAD_LEAVES: &[(&str, &str)] = &[
    ("ByronTxOutInContext", "CollectError#3 > ConwayContextError#8 > BabbageContextError#0"),
    ("CurrentTreasuryFieldNotSupported", "CollectError#3 > ConwayContextError#11"),
    ("InlineDatumsNotSupported", "CollectError#3 > ConwayContextError#8 > BabbageContextError#4"),
    ("MissingCostModel", "CollectError#2 (NoCostModel; payload is a Language)"),
    ("OutsideForecast", "CollectError#3 > ConwayContextError#8 > BabbageContextError#7 (TimeTranslationPastHorizon, field 1 is a Text)"),
    ("ProposalProceduresFieldNotSupported", "CollectError#3 > ConwayContextError#13"),
    ("TreasuryDonationFieldNotSupported", "CollectError#3 > ConwayContextError#14"),
    ("VotingProceduresFieldNotSupported", "CollectError#3 > ConwayContextError#12"),
];

/// Payload encodings that are not a `[tag, ...]` array (or need a caveat): `(type, how it is encoded)`.
pub static PAYLOAD_NOTES: &[(&str, &str)] = &[
    ("NonEmpty a / [a]", "a CBOR LIST of the element encodings, no tag: definite length when it has at most 23 elements, indefinite-length (0x9f .. 0xff) above (Binary/Encoding/EncCBOR.hs:299,306; Encoder.hs:492 encodeList).  So ConwayUtxosPredFailure.CollectErrors is [1, [CollectError, ...]] and FailedUnexpectedly is [1, [FailureDescription, ...]]"),
    ("IsValid", "a newtype over Bool, encoded as a bare CBOR bool (0xf4 false / 0xf5 true), Alonzo/Tx.hs:143-145 and Binary EncCBOR Bool.  ValidationTagMismatch is [0, isValid, TagMismatchDescription]: isValid is the tag the transaction CARRIED (the evaluation gave the opposite)"),
    ("TagMismatchDescription", "array [0] (PassedUnexpectedly, length 1) or [1, [FailureDescription ...]] (FailedUnexpectedly)"),
    ("FailureDescription", "[1, text, bytes]; tag 0 never used"),
    ("Credential", "[0, 28-byte keyhash] (KeyHashObj) or [1, 28-byte scripthash] (ScriptHashObj); NOTE the Haskell data declaration lists ScriptHashObj first but the wire tag of ScriptHashObj is 1 (KeyHashObj is 0)"),
    ("StakeKeyNotRegisteredDELEG", "array [3, Credential]: the credential is element 1.  ConwayDRepNotRegistered / ConwayDRepAlreadyRegistered are [1, Credential] / [0, Credential]: element 1 (a Credential DRepRole, same encoding)"),
    ("TimeTranslationPastHorizon", "field 1 is a Text (AlonzoContextError.TimeTranslationPastHorizon !Text, Alonzo/Plutus/TxInfo.hs:182), not an EpochInfo error type; the ledger renders the failure to text before wrapping it"),
    ("BabbageContextError wrapper", "Conway wraps Babbage in ConwayContextError tag 8: [3, [8, [<babbage tag>, ...]]] for a CollectError BadTranslation; Alonzo constructors are flattened into the Babbage tag space (tags 1 and 7), there is no [.., [..]] level for AlonzoContextError"),
    ("NoCostModel payload", "a Plutus Language; its EncCBOR instance was NOT transcribed (not needed to name the failure)"),
    ("NoRedeemer / CertificateNotSupported / PlutusPurposeNotSupported / VotingProcedures / ProposalProcedures / Coin payloads", "not transcribed: not needed to name the failure"),
];

/// Expected CBOR array length INCLUDING the tag element, for every constructor in [`RULES`] (constructor
/// names are unique across RULES).  `ToGroup` mismatches flatten into the failure array, so a length is
/// 1 + (number of encoded fields) with each `ToGroup` Mismatch counting 2 (supplied/expected, no inner array).
/// Everything else carries one element per `To` field; a `To mm` Mismatch (DELEG tags 7/8) is ONE nested array.
pub static ARITY: &[(&str, usize)] = &[
    ("ConwayUtxowFailure", 2),
    ("ConwayCertsFailure", 2),
    ("ConwayGovFailure", 2),
    ("ConwayWdrlNotDelegatedToDRep", 2),
    ("ConwayTreasuryValueMismatch", 3),
    ("ConwayTxRefScriptsSizeTooBig", 3),
    ("ConwayMempoolFailure", 2),
    ("ConwayWithdrawalsMissingAccounts", 2),
    ("ConwayIncompleteWithdrawals", 2),
    ("UtxoFailure", 2),
    ("InvalidWitnessesUTXOW", 2),
    ("MissingVKeyWitnessesUTXOW", 2),
    ("MissingScriptWitnessesUTXOW", 2),
    ("ScriptWitnessNotValidatingUTXOW", 2),
    ("MissingTxBodyMetadataHash", 2),
    ("MissingTxMetadata", 2),
    ("ConflictingMetadataHash", 3),
    ("InvalidMetadata", 1),
    ("ExtraneousScriptWitnessesUTXOW", 2),
    ("MissingRedeemers", 2),
    ("MissingRequiredDatums", 3),
    ("NotAllowedSupplementalDatums", 3),
    ("PPViewHashesDontMatch", 3),
    ("UnspendableUTxONoDatumHash", 2),
    ("ExtraRedeemers", 2),
    ("MalformedScriptWitnesses", 2),
    ("MalformedReferenceScripts", 2),
    ("ScriptIntegrityHashMismatch", 3),
    ("UtxosFailure", 2),
    ("BadInputsUTxO", 2),
    ("OutsideValidityIntervalUTxO", 3),
    ("MaxTxSizeUTxO", 3),
    ("InputSetEmptyUTxO", 1),
    ("FeeTooSmallUTxO", 3),
    ("ValueNotConservedUTxO", 3),
    ("WrongNetwork", 3),
    ("WrongNetworkWithdrawal", 3),
    ("OutputTooSmallUTxO", 2),
    ("OutputBootAddrAttrsTooBig", 2),
    ("OutputTooBigUTxO", 2),
    ("InsufficientCollateral", 3),
    ("ScriptsNotPaidUTxO", 2),
    ("ExUnitsTooBigUTxO", 3),
    ("CollateralContainsNonADA", 2),
    ("WrongNetworkInTxBody", 3),
    ("OutsideForecast", 2),
    ("TooManyCollateralInputs", 3),
    ("NoCollateralInputs", 1),
    ("IncorrectTotalCollateralField", 3),
    ("BabbageOutputTooSmallUTxO", 2),
    ("BabbageNonDisjointRefInputs", 2),
    ("ValidationTagMismatch", 3),
    ("CollectErrors", 2),
    ("WithdrawalsNotInRewardsCERTS", 2),
    ("CertFailure", 2),
    ("DelegFailure", 2),
    ("PoolFailure", 2),
    ("GovCertFailure", 2),
    ("IncorrectDepositDELEG", 2),
    ("StakeKeyRegisteredDELEG", 2),
    ("StakeKeyNotRegisteredDELEG", 2),
    ("StakeKeyHasNonZeroAccountBalanceDELEG", 2),
    ("DelegateeDRepNotRegisteredDELEG", 2),
    ("DelegateeStakePoolNotRegisteredDELEG", 2),
    ("DepositIncorrectDELEG", 2),
    ("RefundIncorrectDELEG", 2),
    ("StakePoolNotRegisteredOnKeyPOOL", 2),
    ("StakePoolRetirementWrongEpochPOOL", 4),
    ("StakePoolCostTooLowPOOL", 3),
    ("WrongNetworkPOOL", 4),
    ("PoolMedataHashTooBig", 3),
    ("VRFKeyHashAlreadyRegistered", 3),
    ("ConwayDRepAlreadyRegistered", 2),
    ("ConwayDRepNotRegistered", 2),
    ("ConwayDRepIncorrectDeposit", 3),
    ("ConwayCommitteeHasPreviouslyResigned", 2),
    ("ConwayDRepIncorrectRefund", 3),
    ("ConwayCommitteeIsUnknown", 2),
    ("GovActionsDoNotExist", 2),
    ("MalformedProposal", 2),
    ("ProposalProcedureNetworkIdMismatch", 3),
    ("TreasuryWithdrawalsNetworkIdMismatch", 3),
    ("ProposalDepositIncorrect", 3),
    ("DisallowedVoters", 2),
    ("ConflictingCommitteeUpdate", 2),
    ("ExpirationEpochTooSmall", 2),
    ("InvalidPrevGovActionId", 2),
    ("VotingOnExpiredGovAction", 2),
    ("ProposalCantFollow", 4),
    ("InvalidGuardrailsScriptHash", 3),
    ("DisallowedProposalDuringBootstrap", 2),
    ("DisallowedVotesDuringBootstrap", 2),
    ("VotersDoNotExist", 2),
    ("ZeroTreasuryWithdrawals", 2),
    ("ProposalReturnAccountDoesNotExist", 2),
    ("TreasuryWithdrawalReturnAccountsDoNotExist", 2),
    ("UnelectedCommitteeVoters", 2),
];

/// Same as [`ARITY`] for the constructors of [`PAYLOAD_RULES`] (constructor names are unique within the table).
pub static PAYLOAD_ARITY: &[(&str, usize)] = &[
    ("NoRedeemer", 2),
    ("NoWitness", 2),
    ("NoCostModel", 2),
    ("BadTranslation", 2),
    ("BabbageContextError", 2),
    ("CertificateNotSupported", 2),
    ("PlutusPurposeNotSupported", 2),
    ("CurrentTreasuryFieldNotSupported", 2),
    ("VotingProceduresFieldNotSupported", 2),
    ("ProposalProceduresFieldNotSupported", 2),
    ("TreasuryDonationFieldNotSupported", 2),
    ("ReferenceInputsNotDisjointFromInputs", 2),
    ("ByronTxOutInContext", 2),
    ("TranslationLogicMissingInput", 2),
    ("RedeemerPointerPointsToNothing", 2),
    ("InlineDatumsNotSupported", 2),
    ("ReferenceScriptsNotSupported", 2),
    ("ReferenceInputsNotSupported", 2),
    ("TimeTranslationPastHorizon", 2),
    ("TxOutFromInput", 2),
    ("TxOutFromOutput", 2),
    ("PassedUnexpectedly", 1),
    ("FailedUnexpectedly", 2),
    ("PlutusFailure", 3),
    ("KeyHashObj", 2),
    ("ScriptHashObj", 2),
];
