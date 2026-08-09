//! Ouroboros network protocol implementation for the Dugite Cardano node.
//!
//! Four-layer architecture:
//! - Layer 1: Bearer (TCP, Unix socket transport)
//! - Layer 2: Multiplexer (SDU framing, fairness, demux)
//! - Layer 3: Mini-protocols (ChainSync, BlockFetch, TxSubmission2, etc.)
//! - Layer 4: Connection Manager (lifecycle, peer management)

pub mod cbor_limits;
pub mod codec;
pub mod error;

pub mod bearer;

pub mod mux;

pub mod handshake;

pub mod protocol;

pub mod peer;

pub mod connection;

pub mod metrics;
pub mod n2c_client;

pub use error::*;

// Re-export MempoolProvider from primitives (used by TxSubmission2, LocalTxSubmission, LocalTxMonitor).
pub use dugite_primitives::mempool::MempoolProvider;

// ─── Public Traits ───
// These are the integration boundary with dugite-node.
// The node crate implements these traits and passes them to the network layer.

/// Provides block data from ChainDB for N2N server protocols.
///
/// The node crate implements this trait over its ChainDB instance so that
/// the network layer can serve blocks to peers without depending on storage internals.
pub trait BlockProvider: Send + Sync + 'static {
    /// Get raw block CBOR by its 32-byte header hash.
    fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;

    /// Check if a block with the given header hash exists in the chain database.
    fn has_block(&self, hash: &[u8; 32]) -> bool;

    /// Get current chain tip information (slot, hash, block number).
    fn get_tip(&self) -> TipInfo;

    /// Get the next block after a given slot. Returns `(slot, hash, cbor)` if found.
    ///
    /// Uses strict `>` comparison: only returns blocks with `slot > after_slot`.
    fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)>;

    /// Get the chain-order successor of the block identified by `(slot, hash)`.
    ///
    /// A Byron Epoch Boundary Block (EBB) shares its absolute slot with the
    /// first main block of the epoch, so a slot-only cursor cannot step from
    /// the EBB to the same-slot main block — it would serve the peer a chain
    /// with a hole in it.  Cursor-driven servers (ChainSync, LocalChainSync)
    /// and range iteration MUST advance by point, mirroring cardano-node,
    /// whose followers and iterators are keyed by `Point` with same-slot
    /// EBB/main pairs disambiguated by header hash.
    ///
    /// Default implementation: fall back to the slot-based lookup, which is
    /// correct for chains without same-slot blocks (Shelley+).  Providers
    /// backed by real chain storage SHOULD override this.
    fn get_next_block_after_point(
        &self,
        slot: u64,
        hash: &[u8; 32],
    ) -> Option<(u64, [u8; 32], Vec<u8>)> {
        let _ = hash;
        self.get_next_block_after_slot(slot)
    }

    /// Return `true` iff `hash` is on the current canonical chain (the
    /// volatile `selected_chain` window OR the immutable layer).  A fork
    /// block stored alongside the chain returns `false`.
    ///
    /// Used by the ChainSync server to detect when its follower cursor's
    /// block has been rolled back off the active chain and to trigger a
    /// downstream `MsgRollBackward` before serving any further blocks
    /// (Haskell's "follower cursor revalidation").
    ///
    /// Default implementation: assume any known block is on chain, which
    /// is correct for simple block stores that do not maintain forks.
    fn is_on_chain(&self, hash: &[u8; 32]) -> bool {
        self.has_block(hash)
    }

    /// Actual slot of `hash` if it is on the current canonical chain, else
    /// `None` (#908).
    ///
    /// Used by the ChainSync server to validate a `MsgFindIntersect` point:
    /// the hash must be canonical AND the client's claimed slot must match.
    ///
    /// Default implementation: derive it from [`Self::find_chain_ancestor`],
    /// accepting only a self-ancestor. Providers backed by real chain storage
    /// MUST override this — `find_chain_ancestor` is a *rewind* helper and
    /// typically resolves only the volatile window plus the immutable tip, so
    /// it answers `None` for the deep history that clients legitimately offer
    /// as intersection anchors.
    fn canonical_point_slot(&self, hash: &[u8; 32]) -> Option<u64> {
        match self.find_chain_ancestor(hash) {
            Some((slot, found, _)) if &found == hash => Some(slot),
            _ => None,
        }
    }

    /// Find the most recent ancestor of `start_hash` that is on the current
    /// canonical chain.  Returns `Some((slot, hash, block_number))` for the
    /// first ancestor (walking via `prev_hash` links) found on chain, or
    /// `None` when no on-chain ancestor exists within reach.
    ///
    /// Callers invoke this after observing `!is_on_chain(cursor_hash)` —
    /// the follower cursor's block has been displaced by a chain switch
    /// and must be rewound before any forward serving resumes.
    ///
    /// Default implementation: report `start_hash` itself if it is known;
    /// concrete providers MUST override this to walk the prev_hash chain
    /// through their volatile store.
    fn find_chain_ancestor(&self, start_hash: &[u8; 32]) -> Option<(u64, [u8; 32], u64)> {
        let _ = start_hash;
        None
    }

    /// Get the first block at or after a given slot. Returns `(slot, hash, cbor)`.
    ///
    /// Uses `>=` comparison, so `get_block_at_or_after_slot(0)` includes blocks
    /// at slot 0 (e.g. Byron genesis EBB).  Used by ChainSync when the cursor is
    /// at Origin and we must serve the very first block on the chain.
    fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
        // Default: fall back to strict-after lookup.  This is correct when
        // slot > 0, but misses slot-0 blocks.  Implementations should override.
        if slot == 0 {
            self.get_next_block_after_slot(0)
        } else {
            self.get_next_block_after_slot(slot.saturating_sub(1))
        }
    }

    /// Collect multiple blocks in a contiguous slot range [`from_slot`, `to_slot`].
    ///
    /// Returns up to `limit` blocks as `(slot, hash, cbor)` tuples in ascending
    /// slot order.  Implementations SHOULD acquire the storage lock **once** for
    /// the entire batch rather than once per block — this is the primary purpose
    /// of this method.
    ///
    /// The default implementation delegates to [`get_block_at_or_after_slot`] and
    /// [`get_next_block_after_point`] in a loop.  Concrete implementations backed
    /// by a real storage layer MUST override this with a single lock acquisition.
    ///
    /// Iteration is point-cursor driven so that a Byron EBB and the same-slot
    /// first main block of the epoch are BOTH returned, in chain order.
    fn get_blocks_in_range(
        &self,
        from_slot: u64,
        to_slot: u64,
        limit: usize,
    ) -> Vec<(u64, [u8; 32], Vec<u8>)> {
        let mut blocks: Vec<(u64, [u8; 32], Vec<u8>)> = Vec::new();
        let mut cursor: Option<(u64, [u8; 32])> = None;
        while blocks.len() < limit {
            let next = match cursor {
                None => self.get_block_at_or_after_slot(from_slot),
                Some((slot, hash)) => self.get_next_block_after_point(slot, &hash),
            };
            match next {
                Some((slot, hash, cbor)) if slot <= to_slot => {
                    cursor = Some((slot, hash));
                    blocks.push((slot, hash, cbor));
                }
                _ => break,
            }
        }
        blocks
    }
}

/// Chain tip information returned by [`BlockProvider::get_tip`].
#[derive(Debug, Clone)]
pub struct TipInfo {
    /// Slot number of the tip block.
    pub slot: u64,
    /// 32-byte header hash of the tip block.
    pub hash: [u8; 32],
    /// Block number (height) of the tip block.
    pub block_number: u64,
}

/// Validates transactions before mempool admission.
///
/// The node crate implements this over its ledger state to perform Phase-1 and Phase-2
/// validation. Called by N2C LocalTxSubmission and N2N TxSubmission2 protocols.
pub trait TxValidator: Send + Sync + 'static {
    /// Validate a transaction given its era identifier and raw CBOR bytes.
    fn validate_tx(&self, era_id: u16, tx_bytes: &[u8]) -> Result<(), TxValidationError>;
}

/// Transaction validation errors returned to N2C/N2N clients.
///
/// Each variant maps to a specific failure reason from the ledger validation
/// pipeline, encoded in protocol responses to inform the submitting peer.
/// This enum mirrors the full set of `dugite_ledger::validation::ValidationError`
/// variants to enable lossless error propagation from ledger → network → client.
#[derive(Debug, Clone)]
pub enum TxValidationError {
    DecodeFailed {
        reason: String,
    },
    LedgerStateUnavailable,
    NoInputs,
    InputNotFound {
        input: String,
    },
    ValueNotConserved {
        inputs: u64,
        outputs: u64,
        fee: u64,
    },
    FeeTooSmall {
        minimum: u64,
        actual: u64,
    },
    OutputTooSmall {
        minimum: u64,
        actual: u64,
    },
    TxTooLarge {
        maximum: u64,
        actual: u64,
    },
    MissingRequiredSigner {
        signer: String,
    },
    MissingWitness {
        input: String,
    },
    TtlExpired {
        current_slot: u64,
        ttl: u64,
    },
    NotYetValid {
        current_slot: u64,
        valid_from: u64,
    },
    /// **CATCH-ALL — do NOT give this variant a typed encoder arm.**
    ///
    /// `serve.rs` routes roughly twenty unrelated `ValidationError`s onto this
    /// one carrier (`RefScriptsSizeTooBig`, `Phase2EvalPanic`,
    /// `GovernancePreConway`, `MissingDatumWitness`, `ZeroWithdrawal`,
    /// `ScriptLockedCollateral`, …). Its only invariant is "some rejection whose
    /// reason survives as free text", so any typed wire class attached here
    /// would MISLABEL most of what flows through it — an arm that is worse than
    /// the generic fallback (#979).
    ///
    /// The genuine phase-2 script failure now has its own carrier,
    /// [`TxValidationError::Phase2ScriptsFailedUnexpectedly`].
    ScriptFailed {
        reason: String,
    },
    /// Conway `UtxosFailure (ValidationTagMismatch Phase2Valid
    /// (FailedUnexpectedly …))` — the transaction declared `is_valid = true`
    /// and its Plutus scripts then failed evaluation (#1053).
    ///
    /// This is the ONLY ledger error that means "phase-2 evaluation ran and
    /// disagreed with a `Phase2Valid` tag", so it gets a dedicated carrier
    /// rather than riding the [`TxValidationError::ScriptFailed`] catch-all —
    /// otherwise the typed wire class would also be stamped on every unrelated
    /// rejection that happens to share that variant.
    ///
    /// Haskell (`Cardano.Ledger.Alonzo.Rules.Utxos`
    /// `scriptsValidateTransition`):
    ///
    /// ```haskell
    /// Fails _ps fs ->
    ///   failBecause $
    ///     ValidationTagMismatch
    ///       (tx ^. isValidTxL)
    ///       (FailedUnexpectedly (scriptFailureToFailureDescription <$> fs))
    /// ```
    Phase2ScriptsFailedUnexpectedly {
        /// One entry per failed script — the `Text` half of each Haskell
        /// `PlutusFailure Text ByteString`. dugite raises a single aggregated
        /// message today; the field is a `Vec` because Haskell's payload is a
        /// `NonEmpty FailureDescription` and the shape must stay correct if a
        /// future evaluator reports per-script failures.
        messages: Vec<String>,
    },
    /// `InsufficientCollateral DeltaCoin Coin` (Conway `ConwayUtxoPredFailure`
    /// tag 12) — `balance` is the collateral balance actually present (may be
    /// NEGATIVE if `collateral_return` over-declares), `required` is
    /// `ceil(fee * collateralPercentage / 100)`.
    ///
    /// `DeltaCoin` is `newtype DeltaCoin = DeltaCoin Integer` with a
    /// newtype-derived `EncCBOR` — a bare SIGNED CBOR integer, no array or
    /// group wrapper (oracle-verified, #1050). Field order in the Sum
    /// encoding is `DeltaCoin` (balance) then `Coin` (required), matching
    /// dugite's own N2C decoder (`n2c_client.rs` tag 12) which already reads
    /// `[balance_delta, required]` in that order.
    InsufficientCollateral {
        balance: i128,
        required: u64,
    },
    TooManyCollateralInputs {
        max: u64,
        actual: u64,
    },
    CollateralNotFound {
        input: String,
    },
    /// `CollateralContainsNonADA (Value era)` (Conway `ConwayUtxoPredFailure`
    /// tag 15) — the FULL multi-asset `Value` Haskell reports (oracle-
    /// verified against `Cardano.Ledger.Babbage.Rules.Utxo`, #1050): either
    /// the raw sum of collateral-input `Value`s, or — only when the
    /// collateral inputs are ada-only but `collateral_return` itself carries
    /// tokens — the return output's own `Value`. NEVER the netted (inputs
    /// minus return) balance in the general case, so a bare `input: String`
    /// could never carry this payload.
    CollateralHasTokens {
        value: dugite_primitives::value::Value,
    },
    CollateralMismatch {
        declared: u64,
        computed: u64,
    },
    ReferenceInputNotFound {
        input: String,
    },
    ReferenceInputOverlapsInput {
        input: String,
    },
    /// Phase-2 PlutusV3 `TxInfo` translation: `inputs ∩ reference_inputs ≠ ∅`.
    /// Wire shape: `ConwayContextError::ReferenceInputsNotDisjointFromInputs`
    /// (CBOR tag 15) surfaced as a `BadTranslation` collect-error.  Introduced
    /// by Haskell `cardano-ledger` PR #5011 at PV >= 11 for V3-only txs.
    ReferenceInputsNotDisjointFromInputs {
        inputs: Vec<String>,
    },
    MultiAssetNotConserved {
        policy: String,
        input_side: i128,
        output_side: i128,
    },
    InvalidMint,
    ExUnitsExceeded,
    ScriptDataHashMismatch {
        expected: String,
        actual: String,
    },
    UnexpectedScriptDataHash,
    MissingScriptDataHash,
    DuplicateInput {
        input: String,
    },
    NativeScriptFailed,
    InvalidWitnessSignature {
        vkey: String,
    },
    NetworkMismatch {
        expected: String,
        actual: String,
    },
    AuxiliaryDataHashWithoutData,
    AuxiliaryDataWithoutHash,
    BlockExUnitsExceeded {
        resource: String,
        limit: u64,
        total: u64,
    },
    OutputValueTooLarge {
        maximum: u64,
        actual: u64,
    },
    MissingRawCbor,
    MissingSlotConfig,
    MissingSpendRedeemer {
        index: u32,
    },
    RedeemerIndexOutOfRange {
        tag: String,
        index: u32,
        max: u32,
    },
    MissingInputWitness {
        credential: String,
    },
    MissingScriptWitness {
        credential: String,
    },
    MissingWithdrawalWitness {
        credential: String,
    },
    /// A certificate whose subject credential is a SCRIPT was submitted
    /// without the corresponding script witness. Encodes as Utxow tag 3
    /// `MissingScriptWitnessesUTXOW`, the same as the input and withdrawal
    /// forms — Haskell does not distinguish the three on the wire.
    /// Conway `ProposalDepositIncorrect` (ConwayGovPredFailure tag 4): a
    /// governance proposal declares a deposit that does not equal the current
    /// `govActionDeposit` parameter.
    ///
    /// The check is EXACT EQUALITY (`Mismatch 'RelEQ Coin`), not a floor —
    /// over-depositing is rejected just as firmly as under-depositing.
    ProposalDepositIncorrect {
        /// Deposit declared in the ProposalProcedure.
        declared: u64,
        /// Expected deposit from the protocol parameter.
        expected: u64,
    },
    MissingCertificateScriptWitness {
        /// Hex-encoded script hash that had no witness.
        credential: String,
    },
    MissingWithdrawalScriptWitness {
        credential: String,
    },
    MissingCertificateWitness {
        credential: String,
    },
    ValueOverflow,
    /// Conway PV <= 10 `WithdrawalsNotInRewardsCERTS` (CERTS tag 0, wrapped in
    /// Ledger tag 2 `ConwayCertsFailure`).
    ///
    /// The PV <= 10 form BUNDLES both failure modes that PV >= 11 splits into
    /// tags 8 and 9: a reward account that is missing/wrong-network, AND a
    /// withdrawal whose amount does not equal the balance. Haskell derives it
    /// from `withdrawalsThatDoNotDrainAccounts` with `amountAcceptable = (==)`,
    /// and only the SUPPLIED value survives into the payload at this PV — the
    /// expected balance is not reported.
    ///
    /// This is the variant that actually fires on mainnet, preprod, preview and
    /// the devnet, all of which run PV10. dugite implemented the two PV >= 11
    /// encodings but not this one, so the only REACHABLE withdrawal failure
    /// degraded to a stringly-typed `ScriptFailed` and reached clients as a
    /// generic `ConwayMempoolFailure "transaction validation failed"`.
    WithdrawalsNotInRewardsCERTS {
        /// `(reward_account_hex, supplied_coin)` for every withdrawal whose
        /// account is missing OR whose amount mismatches the balance.
        bad: Vec<(String, u64)>,
    },
    /// Conway PV >= 11 `ConwayWithdrawalsMissingAccounts` (Ledger tag 8).
    ///
    /// One or more withdrawals reference a reward account that is not
    /// registered (or is on the wrong network). Payload mirrors the Haskell
    /// `Withdrawals` newtype, a map from reward-account-bytes (hex) to the
    /// supplied coin amount.
    WithdrawalsMissingAccounts {
        /// `(reward_account_hex, supplied_coin)` per missing account.
        missing: Vec<(String, u64)>,
    },
    /// Conway PV >= 11 `ConwayIncompleteWithdrawals` (Ledger tag 9).
    ///
    /// Withdrawals exist but their supplied amount does not equal the
    /// registered reward-account balance. Payload mirrors the Haskell
    /// `NonEmptyMap RewardAccount (Mismatch 'RelEQ Coin)` where each
    /// mismatch encodes on the wire as `[supplied, expected]`.
    IncompleteWithdrawals {
        /// `(reward_account_hex, supplied_coin, expected_balance)` per
        /// mismatched withdrawal.
        mismatches: Vec<(String, u64, u64)>,
    },
    /// The tx body `is_valid` flag does not match the Phase-2 Plutus evaluation
    /// result.  Mirrors Haskell `ValidationTagMismatch` from
    /// `Cardano.Ledger.Conway.Rules.Utxos`.  Rejected at mempool admission to
    /// prevent BPs from ever forging blocks with is_valid-tagged txs that
    /// disagree with actual script execution (DoS class: #522).
    IsValidTagMismatch {
        declared: bool,
        evaluated: bool,
    },
    /// Conway `ConwayDRepNotRegistered` predicate failure (#546 F2): an
    /// `UnregDRep` certificate names a DRep credential that is not in the
    /// registry.  Without this rejection, a tx could credit a deposit refund
    /// for a credential that never registered.
    DRepNotRegistered {
        credential_hash: String,
    },
    /// Shelley+ `PoolMarginsInvalidPOOL` predicate failure (#546 F3): a
    /// `PoolRegistration` cert declares `margin = numerator/denominator`
    /// outside `[0, 1]` (denominator == 0 or numerator > denominator).
    PoolMarginInvalid {
        numerator: u64,
        denominator: u64,
    },

    // ── Conway GOV predicate failures (Ledger tag 3) ──────────────────────
    // These map to `ConwayLedgerPredFailure::ConwayGovFailure (ConwayGovPredFailure)`
    // in Haskell wire format.  The inner GOV pred uses its own integer tags
    // (0..18) distinct from the outer Ledger-level tags (1..9).
    /// `GovActionsDoNotExist` (GOV tag 0): one or more votes reference a
    /// `GovActionId` that is not in the active-proposal set.
    ///
    /// Each element is a hex-encoded tx-hash combined with the action index:
    /// `"<txhash>#<index>"`.
    GovActionsDoNotExist {
        action_ids: Vec<String>,
    },
    /// `InvalidPrevGovActionId` (GOV tag 8): a proposal's `prev_action_id`
    /// does not chain onto its governance purpose — it is neither that
    /// purpose's enacted root, nor an active in-flight proposal, nor (for
    /// `prev_action_id = None`) is the purpose still unrooted.
    ///
    /// Haskell fails the whole transaction here (`failBecause`), so dugite
    /// must reject at admission rather than silently drop the proposal —
    /// dropping let dugite's forge mint blocks cardano-node rejected.
    ///
    /// `action_type` is the proposal's action name, for operator diagnosis.
    ///
    /// `proposal` is the full offending `ProposalProcedure` — Haskell's
    /// `InvalidPrevGovActionId (ProposalProcedure era)` predicate payload is
    /// the ENTIRE proposal, so the LocalTxSubmission encoder needs the whole
    /// value (not just the lineage fields above) to emit a byte-exact
    /// `ConwayGovPredFailure` tag-8 frame. Boxed for the same hot-path
    /// enum-size reason as `dugite_ledger::validation::ValidationError`'s
    /// mirror variant (dugite issue #915).
    InvalidPrevGovActionId {
        action_index: u32,
        action_type: String,
        prev_action_id: Option<String>,
        proposal: Box<dugite_primitives::transaction::ProposalProcedure>,
    },
    /// One element of `MissingRedeemers`' payload: the purpose whose redeemer
    /// is absent, paired with the script hash it would have run.
    ///
    /// Haskell: `NonEmpty (PlutusPurpose AsItem era, ScriptHash)`.
    /// `AsItem` is `newtype AsItem ix it = AsItem { unAsItem :: it }` with a
    /// NEWTYPE-derived `EncCBOR`, so it encodes the ITEM ONLY — the index is a
    /// phantom type parameter and never reaches the wire. That is the whole
    /// difference from `ExtraRedeemers`, which is `AsIx` and encodes the index.
    /// Getting this backwards would produce a frame cardano-cli cannot decode.
    MissingRedeemersUTXOW {
        entries: Vec<(PlutusPurposeItem, String)>,
    },
    /// `MalformedProposal` (GOV tag 1): a `ParameterChange` proposal's
    /// `PParamsUpdate` fails `ppuWellFormed`.
    ///
    /// Haskell's payload is `MalformedProposal (GovAction era)` — the WHOLE
    /// governance action, so the encoder needs the value itself rather than a
    /// reason string. `dugite_ledger::validation::ValidationError` carries the
    /// offending proposal's INDEX (#1025), which `dugite-node` uses to look the
    /// action back up in `tx.body.proposal_procedures`; that is what makes this
    /// well-defined for a tx carrying several proposals, where a reason string
    /// alone could not say which one failed.
    ///
    /// Boxed for the same hot-path enum-size reason as
    /// `InvalidPrevGovActionId`'s payload above.
    MalformedProposalGOV {
        action: Box<dugite_primitives::transaction::GovAction>,
    },
    /// `DisallowedVoters` (GOV tag 5): a voter type is not authorised for
    /// the action type of the referenced governance action.
    ///
    /// Each element is `(<voter_hex>, "<txhash>#<index>")` where `voter_hex`
    /// is the encoded wire discriminator + credential bytes (hex), matching
    /// `Voter` CBOR wire format (disc 0-4, see `read_voter`).
    DisallowedVoters {
        /// Each `(voter_disc, credential_hex, action_id)` triple.
        violations: Vec<(u8, String, String)>,
    },
    // ── #979: typed CERT / DELEG failures ───────────────────────────────
    //
    // Nesting: Ledger 2 (ConwayCertsFailure) -> CERTS 1 (CertFailure)
    //          -> CERT 1 (DelegFailure) -> DELEG tag.
    //
    // **DELEG tags are 1-based.** `IncorrectDepositDELEG` = 1 through
    // `RefundIncorrectDELEG` = 8; there is no tag 0. GOVCERT below IS 0-based,
    // so the two cannot share a numbering assumption.
    //
    // Credentials are dugite "typed-hash32": 28-byte hash + byte 28 as the
    // key/script discriminator. Both halves are needed — Haskell's
    // `Credential` is `array(2)[disc, bstr(28)]`.
    /// `StakeKeyRegisteredDELEG` (DELEG tag 2).
    StakeKeyRegisteredDELEG {
        /// Typed-hash32 hex of the stake credential.
        credential: String,
    },
    /// `StakeKeyNotRegisteredDELEG` (DELEG tag 3).
    ///
    /// dugite distinguishes the delegation and deregistration cases
    /// internally; upstream has ONE constructor, so both map here and the
    /// extra precision is deliberately dropped rather than given a tag that
    /// does not exist.
    StakeKeyNotRegisteredDELEG {
        /// Typed-hash32 hex of the stake credential.
        credential: String,
    },
    /// `StakeKeyHasNonZeroAccountBalanceDELEG` (DELEG tag 4).
    ///
    /// The payload is the BALANCE (a `Coin`), not the credential.
    StakeKeyHasNonZeroAccountBalanceDELEG {
        /// Remaining reward balance in lovelace.
        balance: u64,
    },
    /// `DelegateeDRepNotRegisteredDELEG` (DELEG tag 5).
    DelegateeDRepNotRegisteredDELEG {
        /// Typed-hash32 hex of the DRep credential.
        credential: String,
    },
    /// `DelegateeStakePoolNotRegisteredDELEG` (DELEG tag 6).
    ///
    /// A `KeyHash StakePool` — a bare `bstr(28)`, NOT a `Credential`.
    DelegateeStakePoolNotRegisteredDELEG {
        /// Hex-encoded 28-byte pool key hash.
        pool_id: String,
    },
    /// `IncorrectDepositDELEG` (DELEG tag 1) — the **PV<=10** form.
    ///
    /// `hardforkConwayDELEGIncorrectDepositsAndRefunds` is `pvMajor > 10`, so
    /// below PV 11 an incorrect stake-key deposit *or refund* is reported
    /// through this one constructor, carrying only the SUPPLIED amount. Every
    /// real network runs PV 10 today, which makes this the only DELEG
    /// deposit/refund failure currently reachable — tags 7 and 8 below are the
    /// PV>=11 replacements.
    IncorrectDepositDELEG {
        /// The amount the certificate declared.
        supplied: u64,
    },
    /// `DepositIncorrectDELEG` (DELEG tag 7) — the **PV>=11** form.
    ///
    /// `Mismatch RelEQ Coin` written with `To` — i.e. **nested** as
    /// `array(2)[supplied, expected]`. GOVCERT writes the same type with
    /// `ToGroup`, which flattens it. Getting this backwards is what produced
    /// `DeserialiseFailure … "expected word"` on the first
    /// `ProposalDepositIncorrect` attempt.
    DepositIncorrectDELEG {
        /// Deposit the certificate declared.
        supplied: u64,
        /// Deposit the protocol parameters require.
        expected: u64,
    },
    /// `RefundIncorrectDELEG` (DELEG tag 8). Nested `Mismatch`, as tag 7.
    RefundIncorrectDELEG {
        /// Refund the certificate declared.
        supplied: u64,
        /// Refund the ledger holds for the credential.
        expected: u64,
    },

    // ── #979: typed GOVCERT failures ────────────────────────────────────
    //
    // Ledger 2 -> CERTS 1 -> CERT 3 (GovCertFailure) -> GOVCERT tag.
    // These tags ARE 0-based.
    /// `ConwayDRepAlreadyRegistered` (GOVCERT tag 0).
    ConwayDRepAlreadyRegistered {
        /// Typed-hash32 hex of the DRep credential.
        credential: String,
    },
    /// `ConwayDRepIncorrectDeposit` (GOVCERT tag 2).
    ///
    /// `ToGroup mm` — the `Mismatch` is **FLATTENED** into the constructor's
    /// own fields, unlike DELEG tags 7/8 which nest it.
    ConwayDRepIncorrectDeposit {
        /// Deposit the certificate declared.
        supplied: u64,
        /// Deposit the protocol parameters require.
        expected: u64,
    },
    /// `ConwayCommitteeHasPreviouslyResigned` (GOVCERT tag 3).
    ConwayCommitteeHasPreviouslyResigned {
        /// Typed-hash32 hex of the cold committee credential.
        credential: String,
    },
    /// `ConwayDRepIncorrectRefund` (GOVCERT tag 4). Flattened `Mismatch`.
    ConwayDRepIncorrectRefund {
        /// Refund the certificate declared.
        supplied: u64,
        /// Refund the ledger holds for the DRep.
        expected: u64,
    },
    /// `ConwayCommitteeIsUnknown` (GOVCERT tag 5).
    ConwayCommitteeIsUnknown {
        /// Typed-hash32 hex of the cold committee credential.
        credential: String,
    },

    // ── #979: typed POOL failures ───────────────────────────────────────
    //
    // Ledger 2 -> CERTS 1 -> CERT 2 (PoolFailure) -> POOL.
    //
    // `ShelleyPoolPredFailure`'s `EncCBOR` is HAND-ROLLED rather than built
    // from the `Sum` combinators, so each arm states its own `encodeListLen`
    // and splices `Mismatch` fields in individually. There is no tag 2.
    /// `StakePoolNotRegisteredOnKeyPOOL` — `array(2)[0, pool_id]`.
    ///
    /// Raised by a `PoolRetirement` certificate naming a pool ID that is
    /// not currently registered. `ShelleyPoolPredFailure` is reused
    /// UNMODIFIED in the Conway POOL rule, so this is the ONE field, a bare
    /// `KeyHash StakePool` — no `Credential` wrapper, same shape as
    /// [`Self::DelegateeStakePoolNotRegisteredDELEG`] but nested under
    /// `PoolFailure` (CERT tag 2) rather than `DelegFailure` (CERT tag 1).
    StakePoolNotRegisteredOnKeyPOOL {
        /// Hex-encoded 28-byte pool key hash.
        pool_id: String,
    },
    /// `StakePoolCostTooLowPOOL` — `array(3)[3, supplied, expected]`.
    StakePoolCostTooLowPOOL {
        /// Cost the pool registration declared.
        supplied: u64,
        /// `minPoolCost` protocol parameter.
        expected: u64,
    },
    /// `WrongNetworkPOOL` — `array(4)[4, expected, supplied, pool_id]`.
    ///
    /// Note the field order: **expected precedes supplied**, the reverse of
    /// every other `Mismatch` on this wire.
    WrongNetworkPOOL {
        /// Network the node is configured for.
        expected: u8,
        /// Network found in the pool's reward account.
        supplied: u8,
        /// Hex-encoded 28-byte pool key hash.
        pool_id: String,
    },
    /// `PoolMedataHashTooBig` — `array(3)[5, pool_id, size]`.
    PoolMedataHashTooBigPOOL {
        /// Hex-encoded 28-byte pool key hash.
        pool_id: String,
        /// Size of the offending metadata hash, in bytes.
        size: u64,
    },
    /// `VRFKeyHashAlreadyRegistered` — `array(3)[6, pool_id, vrf_key_hash]`.
    ///
    /// Pool id FIRST, then the VRF hash.
    VrfKeyHashAlreadyRegisteredPOOL {
        /// Hex-encoded 28-byte pool key hash.
        pool_id: String,
        /// Hex-encoded 32-byte VRF verification key hash.
        vrf_key_hash: String,
    },
    /// `StakePoolRetirementWrongEpochPOOL` —
    /// `array(4)[1, gt_expected, lt_supplied, lt_expected]`.
    ///
    /// Two `Mismatch`es, and the first one's `supplied` is **discarded** by
    /// the encoder (`Mismatch _ gtExpected`). Three fields, not four.
    StakePoolRetirementWrongEpochPOOL {
        /// Current epoch — the `RelGT` bound the retirement must exceed.
        gt_expected: u64,
        /// Retirement epoch the certificate declared.
        lt_supplied: u64,
        /// `current epoch + eMax` — the `RelLTEQ` bound.
        lt_expected: u64,
    },

    // ── #979: typed UTXOW failures (Ledger 1 -> UTXOW tag) ──────────────
    /// `InvalidMetadata` (UTXOW tag 8) — carries **no payload** upstream.
    InvalidMetadataUTXOW,
    /// `ExtraneousScriptWitnessesUTXOW` (UTXOW tag 9) — `Set ScriptHash`.
    ExtraneousScriptWitnessesUTXOW {
        /// Hex-encoded 28-byte script hashes.
        script_hashes: Vec<String>,
    },
    /// `UnspendableUTxONoDatumHash` (UTXOW tag 14) — `Set TxIn`.
    UnspendableUTxONoDatumHashUTXOW {
        /// `"<txhash>#<index>"` inputs.
        inputs: Vec<String>,
    },
    /// `ExtraRedeemers` (UTXOW tag 15) — `[PlutusPurpose AsIx]`, each
    /// `array(2)[purpose_tag, index]`.
    ExtraRedeemersUTXOW {
        /// `(purpose_tag, index)` pairs. Purpose tags follow the redeemer
        /// tag numbering: 0 spend, 1 mint, 2 cert, 3 reward, 4 voting,
        /// 5 proposing.
        purposes: Vec<(u8, u32)>,
    },
    /// `MalformedScriptWitnesses` (UTXOW tag 16) — `Set ScriptHash`.
    MalformedScriptWitnessesUTXOW {
        /// Hex-encoded 28-byte script hashes.
        script_hashes: Vec<String>,
    },
    /// `MalformedReferenceScripts` (UTXOW tag 17) — `Set ScriptHash`.
    MalformedReferenceScriptsUTXOW {
        /// Hex-encoded 28-byte script hashes.
        script_hashes: Vec<String>,
    },

    // ── #1025: further typed UTXOW/UTXO failures ────────────────────────
    /// `MissingRequiredDatums` (UTXOW tag 11) —
    /// `NonEmptySet DataHash` (missing) then `Set DataHash` (every datum
    /// hash present in the tx's own witness set — `Alonzo/Rules/Utxow.hs`'s
    /// `missingRequiredDatums`, a pure witness-set derivation with no
    /// ledger-state join).
    MissingRequiredDatumsUTXOW {
        /// Hex-encoded 32-byte datum hashes the tx failed to supply.
        missing: Vec<String>,
        /// Hex-encoded 32-byte datum hashes the tx's witness set DOES supply.
        provided: Vec<String>,
    },
    /// `NotAllowedSupplementalDatums` (UTXOW tag 12) —
    /// `NonEmptySet DataHash` (unneeded) then `Set DataHash` (every datum
    /// hash referenced by the tx's own outputs — `getSupplementalDataHashes`,
    /// also a pure tx-body derivation with no ledger-state join).
    NotAllowedSupplementalDatumsUTXOW {
        /// Hex-encoded 32-byte datum hashes supplied but not needed.
        extra: Vec<String>,
        /// Hex-encoded 32-byte datum hashes referenced by the tx's outputs.
        allowed: Vec<String>,
    },
    /// `OutputBootAddrAttrsTooBig` (UTXO tag 10) — `NonEmpty (TxOut era)`, a
    /// plain LIST (not a set) of the offending outputs' raw CBOR.
    OutputBootAddrAttrsTooBigUTXO {
        /// Raw hex-encoded CBOR of each offending `TxOut`, in tx-body order.
        outputs_raw_cbor: Vec<String>,
    },
    /// `ScriptsNotPaidUTxO` (UTXO tag 13) —
    /// `NonEmptyMap TxIn (TxOut era)`: collateral inputs at a script-locked
    /// address, paired with the TxOut they resolve to (looked up against the
    /// same UTxO view Phase-1 validation already used).
    ScriptsNotPaidUTxOUTXO {
        /// `("<txhash>#<index>", raw_hex_cbor_of_txout)` pairs.
        inputs_outputs: Vec<(String, String)>,
    },
    /// `BabbageOutputTooSmallUTxO` (Conway `ConwayUtxoPredFailure` tag 21) —
    /// `NonEmpty (TxOut era, Coin)`: every output below the era's minimum
    /// UTxO value, paired with the minimum it was required to meet. The old
    /// pre-Babbage `OutputTooSmallUTxO` (tag 9, bare `NonEmpty (TxOut era)`)
    /// is structurally unreachable on a Conway tx — this is the ONLY
    /// reachable form, so no tag-9 arm is implemented.
    ///
    /// Haskell's `EncCBOR (BabbageTxOut era)` is NOT `MemoBytes` — it
    /// re-encodes the typed `TxOut` on every failure — so dugite's own raw
    /// (or freshly re-encoded, if raw bytes were never captured) `TxOut`
    /// CBOR is byte-correct here.
    BabbageOutputTooSmallUTxO {
        /// `(raw_hex_cbor_of_txout, required_minimum_coin)` pairs, in
        /// tx-body output order.
        outputs: Vec<(String, u64)>,
    },
    /// `ZeroTreasuryWithdrawals` (GOV tag 15) — `GovAction era` (the WHOLE
    /// offending `TreasuryWithdrawals` action, not an identifier). Haskell's
    /// GOV rule raises one of these PER offending proposal in a multi-
    /// proposal tx, so this variant represents a SINGLE offender; a tx with
    /// several zero-sum withdrawal proposals produces several of these
    /// wrapped in [`TxValidationError::Multiple`].
    ZeroTreasuryWithdrawalsGOV {
        /// `(account_bytes_hex, coin)` withdrawal map entries — always all
        /// zero (that's what makes the proposal a zero-sum offender), kept
        /// here so the wire payload is byte-faithful to the real `GovAction`
        /// rather than reporting a truncated/summarized map.
        withdrawals: Vec<(String, u64)>,
        /// Hex-encoded 28-byte guardrails policy script hash, if any.
        policy_hash: Option<String>,
    },

    // ── #979: further typed GOV failures (Ledger 3 -> GOV tag) ──────────
    //
    // `AccountAddress` encodes as a byte string of the serialized account
    // address (network header byte + 28-byte credential = 29 bytes), via
    // `encCBOR . runPut . putAccountAddress`.
    /// `ProposalProcedureNetworkIdMismatch` (GOV tag 2).
    ProposalProcedureNetworkIdMismatch {
        /// Hex-encoded account address bytes of the offending return account.
        account: String,
        /// Network the node is configured for.
        network: u8,
    },
    /// `TreasuryWithdrawalsNetworkIdMismatch` (GOV tag 3).
    TreasuryWithdrawalsNetworkIdMismatch {
        /// Hex-encoded account address bytes.
        accounts: Vec<String>,
        /// Network the node is configured for.
        network: u8,
    },
    /// `ConflictingCommitteeUpdate` (GOV tag 6) —
    /// `NonEmptySet (Credential ColdCommitteeRole)`.
    ConflictingCommitteeUpdate {
        /// Typed-hash32 hex of each conflicting cold credential.
        credentials: Vec<String>,
    },
    /// `ExpirationEpochTooSmall` (GOV tag 7) —
    /// `NonEmptyMap (Credential ColdCommitteeRole) EpochNo`.
    ExpirationEpochTooSmall {
        /// `(typed-hash32 hex, expiry epoch)` per offending member.
        members: Vec<(String, u64)>,
    },
    /// `ScriptIntegrityHashMismatch` (UTXOW tag 18) — the **PV>=11** form of a
    /// script-integrity-hash mismatch (#1058).
    ///
    /// `checkScriptIntegrityHash` (cardano-ledger
    /// `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs`) picks the
    /// constructor by protocol version:
    ///
    /// ```haskell
    /// $ if pvMajor (pp ^. ppProtocolVersionL) < natVersion @11
    ///   then PPViewHashesDontMatch mismatch
    ///   else ScriptIntegrityHashMismatch mismatch expectedScriptIntegrity
    /// ```
    ///
    /// The two differ in BOTH tag and payload shape
    /// (`Conway/Rules/Utxow.hs`):
    ///
    /// ```haskell
    /// PPViewHashesDontMatch       mm  -> Sum … 13 !> ToGroup mm
    /// ScriptIntegrityHashMismatch x y -> Sum … 18 !> To x !> To y
    /// ```
    ///
    /// so tag 13 FLATTENS the `Mismatch` into the constructor array while tag 18
    /// carries it as a self-contained `array(2)` plus a SECOND field. dugite
    /// emitted tag 13 at every PV, which is wrong on preview/PV11 — the #978
    /// inversion (there, only the unreachable PV>=11 arms existed).
    ///
    /// `expected_bytes` is Haskell's `originalBytes <$> scriptIntegrity`: the
    /// script-integrity **preimage**, not a hash. dugite's Phase-1 error carries
    /// only hashes, so this is `None` (`SNothing`) — structurally valid and
    /// decodable, omitting a diagnostic. Plumbing the preimage span out of
    /// Phase-1 is a larger change and deliberately does not gate the tag fix.
    ScriptIntegrityHashMismatchUTXOW {
        /// Hex-encoded hash the transaction body declared, if any.
        supplied: Option<String>,
        /// Hex-encoded hash recomputed from the script context, if any.
        expected: Option<String>,
        /// Hex-encoded script-integrity preimage bytes, if known.
        expected_bytes: Option<String>,
    },
    /// `HardForkApplyTxErrWrongEra` — the submitted transaction's era does not
    /// match the ledger's current era (#1047).
    ///
    /// This is NOT a ledger predicate failure and does NOT share their wire
    /// shape. `ApplyTxErr` for the HFC is
    /// `Either (MismatchEraInfo xs) (OneEraApplyTxErr xs)`, and
    /// `encodeEitherMismatch` (ouroboros-consensus
    /// `HardFork/Combinator/Serialisation/Common.hs`) branches on the `Either`:
    ///
    /// ```haskell
    /// (HardForkNodeToClientEnabled{}, Right a) ->
    ///   mconcat [ Enc.encodeListLen 1, enc a ]
    /// (HardForkNodeToClientEnabled{}, Left (MismatchEraInfo err)) ->
    ///   mconcat
    ///     [ Enc.encodeListLen 2
    ///     , encodeNS (hpure (fn encodeName)) era1
    ///     , encodeNS (hpure (fn (encodeName . getLedgerEraInfo))) era2
    ///     ]
    ///   where (era1, era2) = Match.mismatchToNS err
    /// ```
    ///
    /// So the normal case is `array(1)[…]` — which is what every other variant
    /// in this enum produces — and the wrong-era case is a top-level
    /// **`array(2)`** of two `encodeNS` values. `encodeNS` is
    /// `array(2)[word8 index, value]`, and `encodeName` is
    /// `Serialise.encode . singleEraName`, i.e. a CBOR **text** string.
    ///
    /// Field order is pinned by `mkEraMismatch`
    /// (`HardFork/Combinator/AcrossEras.hs`), whose
    /// `Mismatch SingleEraInfo LedgerEraInfo` gives `SingleEraInfo` = the
    /// TRANSACTION's era and `LedgerEraInfo` = the LEDGER's era, and by
    /// `encodeEitherMismatch` emitting `era1` (SingleEraInfo) first. Getting
    /// this order backwards would mislabel the reply exactly as #1051's spurious
    /// Set tag made one undecodable.
    ///
    /// `singleEraName = T.pack (L.eraName @era)` (`ShelleyHFC.hs`), i.e.
    /// cardano-ledger's era name: "Byron", "Shelley", …, "Conway", "Dijkstra".
    HardForkApplyTxErrWrongEra {
        /// HFC index of the era the client declared for its transaction.
        tx_era_index: u8,
        /// Era name for the transaction's era (Haskell `SingleEraInfo`).
        tx_era_name: String,
        /// HFC index of the ledger's current era.
        ledger_era_index: u8,
        /// Era name for the ledger's era (Haskell `LedgerEraInfo`).
        ledger_era_name: String,
    },
    /// `DisallowedProposalDuringBootstrap` (GOV tag 12) —
    /// `DisallowedProposalDuringBootstrap (ProposalProcedure era)`.
    ///
    /// At PV9 only `ParameterChange` / `HardForkInitiation` / `InfoAction` may
    /// be PROPOSED (Haskell `checkBootstrapProposal`, step 1 of
    /// `processProposal`). dugite had only the symmetric VOTE-side restriction
    /// below, so a bootstrap-disallowed proposal was accepted where
    /// cardano-node rejects it (#1026).
    ///
    /// One-field payload carrying the ENTIRE proposal, exactly like
    /// [`TxValidationError::InvalidPrevGovActionId`]'s tag 8 — so the encoder
    /// re-encodes it with `dugite_serialization::encode_proposal_procedure`, the
    /// same function that builds proposals into tx bodies for signing, keeping
    /// both paths byte-identical by construction. Boxed for the same hot-path
    /// enum-size reason.
    DisallowedProposalDuringBootstrap {
        action_index: u32,
        action_type: String,
        proposal: Box<dugite_primitives::transaction::ProposalProcedure>,
    },
    /// `DisallowedVotesDuringBootstrap` (GOV tag 13) —
    /// `NonEmpty (Voter, GovActionId)`.
    DisallowedVotesDuringBootstrap {
        /// `(voter_disc, credential_hex, "<txhash>#<index>")` triples, the
        /// same shape as [`TxValidationError::DisallowedVoters`].
        violations: Vec<(u8, String, String)>,
    },
    /// `TreasuryWithdrawalReturnAccountsDoNotExist` (GOV tag 17) —
    /// `NonEmpty AccountAddress`.
    TreasuryWithdrawalReturnAccountsDoNotExist {
        /// Hex-encoded account address bytes.
        accounts: Vec<String>,
    },
    /// `InvalidGuardrailsScriptHash` (GOV tag 11) — two
    /// `StrictMaybe ScriptHash` values: the hash in the proposal, then the
    /// current constitution's.
    InvalidGuardrailsScriptHash {
        /// Hex-encoded 28-byte script hash from the proposal, if any.
        got: Option<String>,
        /// Hex-encoded 28-byte script hash of the constitution, if any.
        expected: Option<String>,
    },

    /// `ConflictingMetadataHash` (UTXOW tag 7).
    ///
    /// `ToGroup mm` — FLATTENED. `Mismatch { mismatchSupplied = mdh,
    /// mismatchExpected = hashTxAuxData md' }`, i.e. the hash the body
    /// DECLARED comes first and the recomputed one second.
    ConflictingMetadataHashUTXOW {
        /// Hex-encoded auxiliary-data hash declared in the transaction body.
        supplied: String,
        /// Hex-encoded hash recomputed over the auxiliary data.
        expected: String,
    },
    /// `WrongNetwork` (UTXO tag 7) — `Network` then `Set Addr`.
    ///
    /// The network field is the EXPECTED one; the set holds the offending
    /// addresses. There is no "actual network" field on this wire.
    WrongNetworkInOutput {
        /// Network the node is configured for.
        expected: u8,
        /// Hex-encoded raw address bytes of every offending output.
        addresses: Vec<String>,
    },
    /// `WrongNetworkWithdrawal` (UTXO tag 8) — `Network` then
    /// `Set RewardAccount`.
    WrongNetworkWithdrawal {
        /// Network the node is configured for.
        expected: u8,
        /// Hex-encoded reward-account bytes of every offending withdrawal.
        accounts: Vec<String>,
    },

    // ── #979: Ledger-level ──────────────────────────────────────────────
    /// `ConwayWdrlNotDelegatedToDRep` (Ledger tag 4) —
    /// `NonEmpty (KeyHash Staking)`.
    ///
    /// A bare `bstr(28)` per element, **not** a `Credential`: there is no
    /// discriminator on this wire.
    WdrlNotDelegatedToDRep {
        /// Hex-encoded 28-byte staking key hashes.
        key_hashes: Vec<String>,
    },
    /// `ConwayTreasuryValueMismatch` (Ledger tag 5).
    ///
    /// `Sum (… . unswapMismatch) 5 !> ToGroup (swapMismatch mm)` — flattened
    /// AND swapped, so the wire order is `expected` then `supplied`. Upstream
    /// flags this in a comment of its own: "The serialisation order is in
    /// reverse".
    TreasuryValueMismatch {
        /// Treasury value the transaction declared.
        supplied: u64,
        /// Treasury value the ledger holds.
        expected: u64,
    },

    /// `VotersDoNotExist` (GOV tag 14): a voter is not in the corresponding
    /// credential registry (DRep map, pool map, CC hot-key map).
    VotersDoNotExist {
        /// Each `(voter_disc, credential_hex)` pair.
        voters: Vec<(u8, String)>,
    },
    /// `VotingOnExpiredGovAction` (GOV tag 9): a vote targets an action
    /// whose `expiresAfterEpoch` has already passed.
    ///
    /// Elements mirror `DisallowedVoters`: `(voter_disc, credential_hex, action_id)`.
    VotingOnExpiredGovAction {
        expired_votes: Vec<(u8, String, String)>,
    },
    /// `ProposalReturnAccountDoesNotExist` (GOV tag 16): a proposal's
    /// `return_addr` names a stake credential that is not registered.
    ///
    /// Each element is the hex-encoded raw reward-address bytes.
    ProposalReturnAccountDoesNotExist {
        bad_addrs: Vec<String>,
    },
    /// `UnelectedCommitteeVoters` (GOV tag 18): at PV >= 11, a
    /// Constitutional Committee vote's hot credential is not backed by
    /// an elected (non-resigned, non-expired) cold credential.
    ///
    /// Each element is `(disc, credential_hex)` matching Credential CBOR
    /// wire format (0=key, 1=script).
    UnelectedCommitteeVoters {
        hot_credentials: Vec<(u8, String)>,
    },

    /// Multiple validation errors collected.
    Multiple(Vec<TxValidationError>),
    /// Catch-all for other validation failures.
    Other(String),
}

/// The ITEM half of a `PlutusPurpose AsItem` — the value the missing redeemer
/// would have been executed for.
///
/// `ConwayPlutusPurpose` (cardano-ledger `Conway/Scripts.hs`) has an
/// `EncCBORGroup` instance with `listLen _ = 2`, so each purpose encodes as
/// `array(2)[tag, item]` with these constructor tags:
///
/// | Constructor        | Tag | Item                |
/// |--------------------|-----|---------------------|
/// | `ConwaySpending`   | 0   | `TxIn`              |
/// | `ConwayMinting`    | 1   | `PolicyID`          |
/// | `ConwayCertifying` | 2   | `TxCert era`        |
/// | `ConwayWithdrawing`| 3   | `AccountAddress`    |
/// | `ConwayVoting`     | 4   | `Voter`             |
/// | `ConwayProposing`  | 5   | `ProposalProcedure` |
///
/// Spending is absent here on purpose: dugite's missing-redeemer check raises
/// only the five non-spend purposes (see
/// `dugite_ledger::validation::collateral`), so a `Spending` arm would be
/// dead code that could not be exercised — and therefore could not be
/// verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlutusPurposeItem {
    /// Tag 1 — the `PolicyID`, which for a minting purpose IS the script hash.
    Minting { policy_id: String },
    /// Tag 2 — the whole certificate.
    Certifying(Box<dugite_primitives::transaction::Certificate>),
    /// Tag 3 — the 29-byte reward account, hex.
    Withdrawing { account: String },
    /// Tag 4 — the voter.
    Voting(Box<dugite_primitives::transaction::Voter>),
    /// Tag 5 — the whole proposal procedure.
    Proposing(Box<dugite_primitives::transaction::ProposalProcedure>),
}

impl std::fmt::Display for TxValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TxValidationError {}

/// Provides UTxO lookups for LocalStateQuery protocol responses.
///
/// The node crate implements this over its UTxO store so that the network layer
/// can answer UTxO queries from N2C clients without depending on ledger internals.
/// The chain point an LSQ acquisition pinned, threaded into every UTxO query
/// so its answer comes from the same ledger state as every other query in that
/// acquisition (#1068).
///
/// An acquisition is a point-in-time view by construction upstream:
/// `ouroboros-consensus` resolves `MsgAcquire` to one `ExtLedgerState` and runs
/// `answerQuery` against it, with no live side-channel for any query. dugite
/// pins a `NodeStateSnapshot` for everything else but read the LIVE ledger for
/// UTxO, so one `MsgAcquire..MsgRelease` session could answer from two ledger
/// points — the UTxO set from the current tip and everything else from a
/// snapshot up to one block older.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtxoViewPoint {
    /// The acquisition pinned the origin of the chain.
    Origin,
    /// The acquisition pinned a specific block.
    Specific {
        /// Slot number of the pinned block.
        slot: u64,
        /// Header hash of the pinned block.
        hash: [u8; 32],
    },
}

/// Serves UTxO lookups for an LSQ acquisition.
///
/// Every method takes the acquisition's pinned point and returns `None` when it
/// cannot honour it — the caller turns that into a query error rather than
/// silently answering from a different ledger point, which is the defect
/// (#1068) this signature exists to make inexpressible. The point is a required
/// parameter, not a defaulted one, so an implementation cannot quietly ignore
/// it the way `_state: &NodeStateSnapshot` did.
pub trait UtxoQueryProvider: Send + Sync {
    /// Look up all UTxOs at a given address (raw address bytes), as of `at`.
    fn utxos_at_address_bytes(
        &self,
        addr_bytes: &[u8],
        at: &UtxoViewPoint,
    ) -> Option<Vec<UtxoSnapshot>>;

    /// Look up UTxOs by specific transaction inputs (tx_hash, output_index),
    /// as of `at`. Default returns empty — override if the store supports it.
    fn utxos_by_tx_inputs(
        &self,
        _inputs: &[(Vec<u8>, u32)],
        _at: &UtxoViewPoint,
    ) -> Option<Vec<UtxoSnapshot>> {
        Some(vec![])
    }

    /// Return the entire UTxO set (GetUTxOWhole) as of `at`.
    /// Default returns empty — override if the store supports it.
    fn utxos_all(&self, _at: &UtxoViewPoint) -> Option<Vec<UtxoSnapshot>> {
        Some(vec![])
    }
}

/// A single asset within a multi-asset value: `(asset_name, quantity)`.
pub type AssetEntry = (Vec<u8>, u64);

/// A policy group within a multi-asset value: `(policy_id, assets)`.
pub type PolicyAssets = (Vec<u8>, Vec<AssetEntry>);

/// Multi-asset snapshot: `[(policy_id, [(asset_name, quantity)])]`.
pub type MultiAssetSnapshot = Vec<PolicyAssets>;

/// UTxO snapshot for query responses, containing all fields needed for CBOR encoding.
///
/// Field names match the old API to minimize node integration churn.
#[derive(Debug, Clone)]
pub struct UtxoSnapshot {
    /// Transaction hash (raw bytes, typically 32 bytes).
    pub tx_hash: Vec<u8>,
    /// Output index within the transaction.
    pub output_index: u32,
    /// Address bytes (raw Cardano address encoding).
    pub address_bytes: Vec<u8>,
    /// Lovelace value at this output.
    pub lovelace: u64,
    /// Multi-asset values: `[(policy_id, [(asset_name, quantity)])]`.
    pub multi_asset: MultiAssetSnapshot,
    /// Optional datum hash (32 bytes).
    ///
    /// Set when the output uses `OutputDatum::DatumHash`. Mutually exclusive
    /// with `inline_datum`.
    pub datum_hash: Option<Vec<u8>>,
    /// Optional **inline** datum, as the verbatim CBOR bytes of the
    /// `PlutusData` value (CIP-32 / Babbage+).
    ///
    /// Emitted as CBOR map key 2 with the inline-datum variant:
    ///   `2: [1, tag(24) bstr(inline_datum_cbor)]`
    ///
    /// Per the Conway CDDL `datum_option = [0, $hash32] // [1, data]` where
    /// `data = #6.24(bytes .cbor data)` — the integer discriminator selects
    /// between hashed and inline.
    ///
    /// `TransactionOutput.raw_cbor` is `#[serde(skip)]` so re-encoding from
    /// the in-memory `PlutusData` after an LSM round-trip can mutate the
    /// bytes (canonical-vs-non-canonical, map ordering). We carry the
    /// preserved CBOR here directly so the N2C query path emits the exact
    /// bytes cardano-cli's auto-balance evaluator needs to reconstruct the
    /// `ScriptContext.txInfoOutputs` datum field bit-for-bit. Without this
    /// field cardano-cli silently underestimates `ex_units` for any tx
    /// that spends an inline-datum UTxO, and dugite-forged blocks are
    /// rejected by cardano-node with `ValidationTagMismatch (IsValid True)
    /// (FailedUnexpectedly (PlutusFailure …))`.
    pub inline_datum: Option<Vec<u8>>,
    /// Reference script attached to this output (CIP-33 / Babbage+).
    ///
    /// Emitted as CBOR map key 3 in PostAlonzo output encoding:
    ///   `3: tag(24) bstr(encode_script_ref(script_ref))`
    ///
    /// `TransactionOutput.raw_cbor` is `#[serde(skip)]` so it does NOT survive
    /// LSM round-trips; we carry the structured `ScriptRef` here instead so that
    /// the N2C query encoder can reconstruct the correct CBOR even after a store
    /// round-trip.
    pub script_ref: Option<dugite_primitives::transaction::ScriptRef>,
    /// Optional raw CBOR of the entire output (for Plutus script evaluation).
    ///
    /// When `Some`, the bytes are written verbatim to the N2C response in place
    /// of a re-encoded output, preserving the original wire format.  After an
    /// LSM round-trip this field is `None` (it is not persisted); the encoder
    /// falls back to re-encoding from the structured fields above, including
    /// `script_ref`.
    pub raw_cbor: Option<Vec<u8>>,
}

/// Metrics bridge for connection events.
///
/// Implemented by the node layer to bridge protocol-level events to the
/// Prometheus metrics system (e.g. `peers_connected` gauge).
pub trait ConnectionMetrics: Send + Sync + 'static {
    /// Called when a new peer connection is established.
    fn on_connect(&self);
    /// Called when a peer connection is closed.
    fn on_disconnect(&self);
    /// Called when a connection-level error occurs.
    fn on_error(&self, label: &str);
}

// ─── Convenience re-exports ───
// Key types re-exported at crate root for ergonomic imports.

pub use protocol::blockfetch::client::BlockFetchClient;
pub use protocol::blockfetch::decision::BlockFetchDecision;
pub use protocol::chainsync::client::{ChainSyncEvent, PipelinedChainSyncClient};
pub use protocol::chainsync::jumping::{
    bisect_midpoint, EraParams as CsjEraParams, JumpInstruction, JumpState, JumperState,
    PeerJumpState, TransitionError as CsjTransitionError,
};
pub use protocol::chainsync::server::{BlockAnnouncement, RollbackAnnouncement};
pub use protocol::keepalive::client::{KeepAliveClient, DEFAULT_KEEPALIVE_INTERVAL};
pub use protocol::keepalive::server::KeepAliveServer;
pub use protocol::local_state_query::server::QueryHandler;
pub use protocol::peersharing::client::PeerSharingClient;
pub use protocol::txsubmission::client::{TxSource, TxSubmissionClient};
pub use protocol::txsubmission::server::TxSubmissionServer;
pub use protocol::txsubmission::{TxAdmission, TxIdAndSize};

pub use peer::manager::{PeerInfo, PeerManager, PeerSource, PeerState, PEER_LATENCY_DEFAULT_MS};
pub use peer::{Governor, GovernorConfig, PeerTargets};

pub use connection::manager::ConnectionManagerConfig;
pub use connection::{ConnectionHandler, ConnectionManager, ConnectionState};

pub use mux::channel::MuxChannel;
pub use mux::{Direction, Mux};

pub use bearer::tcp::TcpBearer;
pub use bearer::unix::UnixBearer;

pub use handshake::n2c::N2CVersionData;
pub use handshake::n2n::N2NVersionData;
pub use n2c_client::{N2CClient, TipResult};
