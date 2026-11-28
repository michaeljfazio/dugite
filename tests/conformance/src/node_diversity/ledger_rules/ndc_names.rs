//! Haskell constructor name -> node-diversity-conformance (NDC) `ledger-rules` scenario name.
//!
//! The scenario's `result` vocabulary (`schema.json`, `properties.scenarios.items.properties.result.oneOf`,
//! 81 entries: `pass` plus 80 failure names) mostly equals the cardano-ledger constructor name; this file
//! holds only the exceptions.  A constructor in [`super::tag_table::RULES`] with no row here either has the
//! identical NDC name, or has no NDC name at all and so cannot be a scenario result.
//!
//! Every row is read from independent implementations that map the Haskell failures onto this vocabulary,
//! never from dugite's encoder.  Evidence tags used in the third column:
//!   [yano]     bloxbean/yano `ledger-rules/.../fixtures/amaru/AmaruCorpusNames.java` (also quotes Amaru's
//!              Haskell checker `Run.hs`, `normalizeLedgerFailure` and below, at 40ba9950)
//!   [dingo]    blinklabs-io/dingo `internal/ledgerrulesconformance/report.go` (maps Dingo's OWN errors, so it
//!              corroborates the NDC side of a row but does not name Haskell constructors)
//!   [scarab]   KD5UJC-XYZ/scarab `tools/.../ledger/LedgerRulesNames.scala` (`of`/`deleg`/`govCert`/... over the
//!              Conway constructors)
//!   [gerolamo] HarmonicLabs/gerolamo `src/conformance/ndc/ledgerRules.ts` (`CORPUS_NAMES`)
//!   [fixtures] `target/node-diversity/corpora/ndc/scenarios/ledger-rules/fixtures/scenarios/*.json`
//!              (`expected.predicate`; the Haskell node scores 368/368 on them)
//!
//! A Haskell constructor may appear in several rows (one constructor, several NDC names, chosen by a
//! payload or certificate kind the tag path does not carry): those rows say so.  A row whose independent
//! sources DISAGREE says so in its evidence; nothing was picked silently.

// Rows the harness deliberately does NOT carry, because each would turn a wire-level divergence
// into a match (the scenario scores names; the point of the harness is the bytes):
//
//  * `ConwayMempoolFailure` -> `BadInputsUTxO` / `InputSetEmptyUTxO` ([scarab] and the checker).
//    Haskell DOES send `ConwayMempoolFailure "All inputs are spent. Transaction has probably
//    already been included"` (`Conway/Rules/Mempool.hs`, raised when none of a transaction's inputs
//    is in the UTxO), and the reference checker (`Run.hs`, `normalizeLedgerFailure`) maps that text
//    to `InputSetEmptyUTxO` for an empty input set and `BadInputsUTxO` otherwise.  dugite's
//    `ConwayMempoolFailure` is something else: its generic fallback, with the text
//    "transaction validation failed", for a failure it has no typed encoding for.  A row keyed on
//    the constructor alone would score every fallback as the failure it stood in for, so the
//    constructor has no name here.  The cost is stated, not hidden: dugite sends the typed
//    `BadInputsUTxO` where a real node sends the mempool text (fixtures 00050, 00074, decided by
//    #1138 today; their shadow runs match by name).
//  * `OutputTooSmallUTxO` (UTXO tag 9) -> `BabbageOutputTooSmallUTxO` ([scarab] only): Conway's
//    check raises `BabbageOutputTooSmallUTxO` (tag 21); a node that sends tag 9 sends different
//    bytes from cardano-node.
//  * `OutputBootAddrAttrsTooBig` (UTXO tag 10) -> `OutputTooBigUTxO`: at the pinned ledger revision
//    cardano-node itself sends `OutputTooBigUTxO` (tag 11) for the oversized Byron attributes
//    (Allegra `Utxo.hs:399`), so a node that sends tag 10 sends different bytes.
// Both of the last two are named, with their evidence, in the divergence issues the expected
// results link to if dugite emits them.

/// `(Haskell constructor name, NDC scenario name, evidence)` for every constructor in
/// `tag_table::RULES` whose NDC name is NOT identical to the Haskell name.
pub static HASKELL_TO_NDC: &[(&str, &str, &str)] = &[
    // ---- DELEG ----
    (
        "StakeKeyRegisteredDELEG",
        "StakeKeyRegistered",
        "[yano][scarab][gerolamo][dingo] agree; fixtures 00106-00109,00272",
    ),
    (
        "StakeKeyNotRegisteredDELEG",
        "StakeKeyNotRegistered",
        "ONE-TO-MANY by certificate kind (the failure carries only the credential): this row is the \
         (Conway)unregistration; [yano][scarab][dingo] agree, fixtures 00290,00291. [gerolamo] has no \
         per-certificate split and names every StakeKeyNotRegisteredDELEG StakeCredentialInvalidPoolDelegation",
    ),
    (
        "StakeKeyNotRegisteredDELEG",
        "StakeCredentialInvalidPoolDelegation",
        "ONE-TO-MANY by certificate kind: delegation to a pool from an unregistered credential; \
         [yano][scarab][dingo] agree, [gerolamo] and Amaru's checker default (CHECKER_DEFAULTS) use this for \
         every StakeKeyNotRegisteredDELEG; fixtures 00100-00103,00286",
    ),
    (
        "StakeKeyNotRegisteredDELEG",
        "StakeCredentialInvalidVoteDelegation",
        "ONE-TO-MANY by certificate kind: vote or stake+vote delegation from an unregistered credential \
         (a stake+vote delegation counts as the vote's, scarab CertificateKind, dingo); [yano][scarab][dingo] \
         agree; fixtures 00287,00288",
    ),
    (
        "StakeKeyHasNonZeroAccountBalanceDELEG",
        "StakeKeyHasNonZeroAccountBalance",
        "[yano][scarab][dingo][gerolamo] agree (gerolamo also lists the older spelling \
         StakeKeyHasNonZeroRewardAccountBalanceDELEG); fixtures 00104,00105",
    ),
    (
        "DelegateeDRepNotRegisteredDELEG",
        "DelegateeDRepNotRegistered",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00061-00064",
    ),
    (
        "DelegateeStakePoolNotRegisteredDELEG",
        "DelegateeStakePoolNotRegistered",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00065-00068",
    ),
    // ---- GOVCERT ----
    (
        "ConwayDRepAlreadyRegistered",
        "DRepAlreadyRegistered",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00059,00060",
    ),
    (
        "ConwayDRepNotRegistered",
        "DRepNotRegistered",
        "[yano][scarab][dingo] agree ([gerolamo] CORPUS_NAMES has no entry for it); fixtures 00289,00292",
    ),
    (
        "ConwayDRepIncorrectDeposit",
        "IncorrectDepositDRep",
        "[yano][scarab] agree; fixture 00347. [gerolamo] has no entry; [dingo] only matches the \
         DRep-registration deposit error (certificate type 16)",
    ),
    (
        "ConwayDRepIncorrectRefund",
        "IncorrectDepositDRep",
        "[yano][scarab] agree: Amaru's checker normalizeGovCertFailure names BOTH ConwayDRepIncorrectDeposit \
         and ConwayDRepIncorrectRefund IncorrectDepositDRep; fixture 00305 (deregistration with incorrect \
         refund). [gerolamo] has no entry; [dingo] only names the registration deposit",
    ),
    (
        "ConwayCommitteeHasPreviouslyResigned",
        "CommitteeHasPreviouslyResigned",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00206-00210,00217",
    ),
    (
        "ConwayCommitteeIsUnknown",
        "CommitteeIsUnknown",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00201,00204,00205,00220,00343",
    ),
    // ---- UTXOW ----
    (
        "MissingVKeyWitnessesUTXOW",
        "MissingVerificationKeyWitnessesUTXOW",
        "[yano][scarab][dingo][gerolamo] agree; fixtures 00084-00086,00293-00295,00318,00356",
    ),
    // ---- UTXO ----
    (
        "WrongNetwork",
        "WrongNetworkInTxOutput",
        "[yano][scarab][dingo][gerolamo] agree; fixture 00128 (WrongNetworkInTxBody and \
         WrongNetworkWithdrawal keep their names)",
    ),
    // ---- GOV ----
    (
        "ZeroTreasuryWithdrawals",
        "TreasuryWithdrawalsAllZeros",
        "[yano][scarab][dingo][gerolamo] agree; fixture 00130",
    ),
    (
        "UnelectedCommitteeVoters",
        "VotersDoNotExist",
        "[yano][scarab][dingo][gerolamo] agree: protocol-version-11-only GOV failure folded into \
         VotersDoNotExist (Amaru checker normalizeGovFailure, Run.hs:603-604); fixture 00171 (v11)",
    ),
    // ---- UTXOS CollectErrors: ONE constructor, NDC name chosen by the CollectError payload ----
    (
        "CollectErrors",
        "ByronTxOutInContext",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (BabbageContextError ByronTxOutInContext)]; \
         [yano][scarab] agree; fixtures 00365-00367",
    ),
    (
        "CollectErrors",
        "CurrentTreasuryFieldNotSupported",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (ConwayContextError CurrentTreasuryFieldNotSupported)]; \
         [yano][scarab] agree; fixtures 00312,00313",
    ),
    (
        "CollectErrors",
        "InlineDatumsNotSupported",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (BabbageContextError InlineDatumsNotSupported)]; \
         [yano][scarab] agree; fixtures 00314,00353,00354",
    ),
    (
        "CollectErrors",
        "MissingCostModel",
        "PAYLOAD-DEPENDENT: CollectErrors [NoCostModel]; [yano][scarab] agree; fixture 00330",
    ),
    (
        "CollectErrors",
        "OutsideForecast",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (.. TimeTranslationPastHorizon ..)]. DISAGREEMENT \
         on the constructor: [yano] says Haskell's own UTXO OutsideForecast (tag 17) is unreachable in \
         Conway and the NDC name is ONLY this CollectErrors payload; [scarab] maps BOTH UTXO tag 17 and this \
         payload to OutsideForecast (tag 17 also has the identical name, so it is reachable either way)",
    ),
    (
        "CollectErrors",
        "ProposalProceduresFieldNotSupported",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (ConwayContextError ProposalProceduresFieldNotSupported)]; \
         [yano][scarab] agree; fixtures 00306,00307",
    ),
    (
        "CollectErrors",
        "TreasuryDonationFieldNotSupported",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (ConwayContextError TreasuryDonationFieldNotSupported)]; \
         [yano][scarab] agree; fixtures 00308,00309",
    ),
    (
        "CollectErrors",
        "VotingProceduresFieldNotSupported",
        "PAYLOAD-DEPENDENT: CollectErrors [BadTranslation (ConwayContextError VotingProceduresFieldNotSupported)]; \
         [yano][scarab] agree; fixtures 00310,00311",
    ),
    // ---- LEDGER ConwayMempoolFailure: text-dependent, reached through the mempool path ----
];

/// NDC names (every `result` value of the scenario schema) that match NO constructor in
/// `tag_table::RULES` by identical spelling, with the reason.  The eight `CollectErrors` payload names
/// are also reachable through [`HASKELL_TO_NDC`]; they are repeated here because no constructor carries
/// their name.
pub static NDC_NAMES_WITHOUT_CONSTRUCTOR: &[(&str, &str)] = &[
    (
        "pass",
        "not a failure: the transaction was accepted (fixtures spell it `result: Pass`)",
    ),
    (
        "DecodingFailure",
        "not a ledger predicate failure: the transaction bytes do not decode (fixtures 00077,00078,00080,00362,\
         00363, `expected.decoding_failure`); a node rejects it at deserialisation before any rule runs, so \
         there is no ApplyTxErr tag path. [dingo][scarab][gerolamo] all emit it for undecodable input",
    ),
    (
        "ByronTxOutInContext",
        "payload of ConwayUtxosPredFailure.CollectErrors (BabbageContextError), see HASKELL_TO_NDC",
    ),
    (
        "CurrentTreasuryFieldNotSupported",
        "payload of ConwayUtxosPredFailure.CollectErrors (ConwayContextError), see HASKELL_TO_NDC",
    ),
    (
        "InlineDatumsNotSupported",
        "payload of ConwayUtxosPredFailure.CollectErrors (BabbageContextError), see HASKELL_TO_NDC",
    ),
    (
        "MissingCostModel",
        "payload of ConwayUtxosPredFailure.CollectErrors (CollectError NoCostModel), see HASKELL_TO_NDC",
    ),
    (
        "ProposalProceduresFieldNotSupported",
        "payload of ConwayUtxosPredFailure.CollectErrors (ConwayContextError), see HASKELL_TO_NDC",
    ),
    (
        "TreasuryDonationFieldNotSupported",
        "payload of ConwayUtxosPredFailure.CollectErrors (ConwayContextError), see HASKELL_TO_NDC",
    ),
    (
        "VotingProceduresFieldNotSupported",
        "payload of ConwayUtxosPredFailure.CollectErrors (ConwayContextError), see HASKELL_TO_NDC",
    ),
];
