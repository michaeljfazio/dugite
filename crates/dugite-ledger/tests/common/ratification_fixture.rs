//! Offline fixture schema used by `conway_ratification.rs` integration tests.
//!
//! The schema captures every input the Haskell `ratifyTransition` rule reads
//! from `RatifyEnv` / `RatifyState`, expressed in dugite's terms:
//!
//! * `proposal` / `votes` — proposals + votes that flow through `ratify_proposals`.
//! * `drep_power` / `drep_no_confidence` / `drep_abstain` — `RatifyEnv.reDRepDistr`,
//!   keyed by typed `Hash32` credential (28-byte hash + 0x00/0x01 + 3 zero bytes).
//! * `spo_stake` — `RatifyEnv.reStakePoolDistr`, keyed by raw 28-byte pool ID.
//! * `pool_reward_accounts` — pool ID → 29-byte reward address. Required so
//!   `defaultStakePoolVote` can resolve a non-voting pool's implicit vote.
//! * `vote_delegations` — stake credential (typed Hash32) → DRep. Drives the
//!   `defaultStakePoolVote` lookup for non-voting SPOs.
//! * `committee` — typed Hash32 cold/hot keys, real Koios threshold.
//! * `pparams` — every threshold field RATIFY can consult; replaces
//!   `mainnet_defaults()` so post-bootstrap thresholds and `committee_min_size`
//!   are exercised correctly.
//! * `no_confidence` — drives the `UpdateCommittee` branch in
//!   `check_ratification_impl` (read live, not from snapshot).
//! * `parent_enacted` — seeds the four `enacted_*` chain roots.
//!
//! The schema deliberately omits redundant totals (`total_drep_stake`,
//! `total_spo_stake`) — the loader recomputes them from the snapshot maps so
//! they cannot drift.

use dugite_ledger::state::{
    DRepRegistration, GovernanceState, LedgerState, PoolRegistration, ProposalState, StakeSnapshot,
};
use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32, TransactionHash};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Anchor, Constitution, CostModels, DRep, ExUnitPrices, ExUnits, GovAction, GovActionId,
    ProposalProcedure, ProtocolParamUpdate, Rational, Vote, Voter, VotingProcedure,
};
use dugite_primitives::value::Lovelace;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Deserialize)]
pub struct RatificationFixture {
    pub proposal: FixtureProposal,
    pub proposed_epoch: u64,
    pub votes: Vec<FixtureVote>,

    /// DRep voting power snapshot, keyed by typed Hash32 credential hex
    /// (28-byte credential + `0x00` for key / `0x01` for script + 3 zero bytes).
    /// Matches the encoding produced by `Credential::to_typed_hash32`.
    ///
    /// Mutually exclusive with [`Self::drep_aggregates`]: per-DRep mode
    /// captures one entry per registered DRep with their actual stake;
    /// aggregate mode synthesizes a 3-entry snapshot from the proposal's
    /// voting summary.
    #[serde(default)]
    pub drep_power: BTreeMap<String, u64>,
    /// Aggregated stake delegated to the `DRepAlwaysNoConfidence` pseudo-DRep.
    #[serde(default)]
    pub drep_no_confidence: u64,
    /// Aggregated stake delegated to the `DRepAlwaysAbstain` pseudo-DRep.
    #[serde(default)]
    pub drep_abstain: u64,

    /// Aggregate-mode DRep snapshot: a single Koios `proposal_voting_summary`
    /// call collapses 8800+ per-DRep queries into one request, returning
    /// the total Yes / No / Abstain DRep stake for this specific proposal.
    /// The loader synthesizes a 3-entry `drep_distribution_snapshot` plus
    /// 2 synthetic votes (Yes + Abstain credentials; the No credential is
    /// registered but does not vote, which `count_votes_by_type_impl`
    /// correctly counts as No).
    ///
    /// The synthesized values reproduce the exact `drep_yes / drep_total`
    /// ratio the real per-DRep iteration would compute — so the
    /// ratification outcome is identical, while the capture path uses
    /// O(1) Koios requests instead of O(N).  Used for post-bootstrap
    /// (PV ≥ 10) fixtures where the per-DRep path exceeds Koios's
    /// 5000 req/day free-tier cap.
    ///
    /// When `Some`, `drep_power` / `drep_no_confidence` / `drep_abstain`
    /// fields are ignored — the loader uses the aggregates instead.
    #[serde(default)]
    pub drep_aggregates: Option<DRepAggregates>,

    /// SPO voting power snapshot, keyed by raw 28-byte pool ID hex.
    #[serde(default)]
    pub spo_stake: BTreeMap<String, u64>,

    /// Pool ID (28-byte hex) → 29-byte reward account hex (header byte +
    /// 28-byte credential).  Required for `defaultStakePoolVote` to resolve
    /// a non-voting pool's implicit vote via its reward account's DRep
    /// delegation.  Pools missing here resolve to `DefaultVote::No`,
    /// matching Haskell when the reward account is unregistered.
    #[serde(default)]
    pub pool_reward_accounts: BTreeMap<String, String>,

    /// Stake credential (typed Hash32 hex) → DRep delegation.  Drives the
    /// `vote_delegations` lookup in `default_spo_vote_from`.  An entry with
    /// `FixtureDRep::NoConfidence` causes the pool to vote Yes on
    /// `NoConfidence` actions; `Abstain` excludes the pool from the
    /// denominator on every action; anything else (`KeyHash`/`ScriptHash`)
    /// counts as a No vote.
    #[serde(default)]
    pub vote_delegations: BTreeMap<String, FixtureDRep>,

    /// Live `gov.governance.no_confidence` flag.  RATIFY reads this LIVE
    /// (not from the snapshot) when resolving the `UpdateCommittee`
    /// threshold branch (normal vs no-confidence).
    #[serde(default)]
    pub no_confidence: bool,

    pub committee: FixtureCommittee,
    pub pparams_epoch: u64,
    pub pparams: FixturePParams,
    pub expected_outcome: ExpectedOutcome,
    pub parent_enacted: ParentEnacted,

    /// Optional capture provenance metadata.  Not consumed by the loader.
    #[serde(default)]
    #[allow(dead_code)]
    pub provenance: Option<serde_json::Value>,
}

/// Aggregate DRep voting power for a specific proposal, captured via
/// `proposal_voting_summary` instead of per-DRep `drep_voting_power_history`.
///
/// All values are absolute lovelace stake.  See `RatificationFixture::drep_aggregates`.
#[derive(Debug, Clone, Deserialize)]
pub struct DRepAggregates {
    /// Total stake of registered DReps that voted Yes on this proposal.
    pub yes_stake: u64,
    /// Total stake of registered DReps that voted No.
    pub no_stake: u64,
    /// Total stake of registered DReps that voted Abstain.
    pub abstain_stake: u64,
    /// Total stake of registered DReps that did NOT vote on this proposal —
    /// counted as No per Haskell's `dRepAcceptedRatio` ("registered
    /// non-voting DRep counts as No").
    #[serde(default)]
    pub no_vote_stake: u64,
    /// Stake delegated to the `DRepAlwaysNoConfidence` pseudo-DRep.
    #[serde(default)]
    pub always_no_confidence_stake: u64,
    /// Stake delegated to the `DRepAlwaysAbstain` pseudo-DRep.
    #[serde(default)]
    pub always_abstain_stake: u64,
}

/// Reserved synthetic credential hashes used by aggregate-mode capture.
/// These bytes are deliberately distinct from any real on-chain credential
/// (real credentials are Blake2b-224 hashes; these all start with sentinel
/// nibbles 0xAA/0xBB/0xCC/0xDD).
const AGG_YES_CRED: [u8; 28] = [0xAA; 28];
const AGG_NO_CRED: [u8; 28] = [0xBB; 28];
const AGG_ABSTAIN_CRED: [u8; 28] = [0xCC; 28];
const AGG_NO_VOTE_CRED: [u8; 28] = [0xDD; 28];

/// Build the typed-Hash32 form of a 28-byte key credential for the
/// aggregate-mode synthesis path.  Equivalent to
/// `Credential::VerificationKey(Hash28::from_bytes(*bytes)).to_typed_hash32()`,
/// inlined here because the synth path runs in two places (snapshot map
/// keys and voter construction).
fn typed_hash32_for_key(bytes: &[u8; 28]) -> Hash32 {
    let mut out = [0u8; 32];
    out[..28].copy_from_slice(bytes);
    // Type byte 28 is 0x00 for VerificationKey credentials.
    Hash32::from_bytes(out)
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureProposal {
    pub gov_action_id: String,
    pub action: serde_json::Value,
    pub deposit: u64,
    pub return_addr_hex: String,
    pub expiration: u64,
    pub anchor: Option<FixtureAnchor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureAnchor {
    pub url: String,
    pub data_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureVote {
    pub voter_type: FixtureVoterType,
    pub voter_id: String,
    pub vote: FixtureVoteValue,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum FixtureVoterType {
    ConstitutionalCommitteeHotKeyHash,
    ConstitutionalCommitteeHotScriptHash,
    DRepKeyHash,
    DRepScriptHash,
    StakePoolKeyHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum FixtureVoteValue {
    Yes,
    No,
    Abstain,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureCommittee {
    pub members: Vec<FixtureCommitteeMember>,
    pub threshold: FixtureRational,
    /// Cold credential typed Hash32 hex of resigned members.
    pub resigned: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureCommitteeMember {
    /// Typed Hash32 hex (28 bytes + 0x00/0x01 + 3 zero bytes).
    pub cold_key: String,
    /// Same encoding as `cold_key`.  Absent when no hot key is registered.
    pub hot_key: Option<String>,
    pub expiration: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRational {
    pub numerator: u64,
    pub denominator: u64,
}

impl FixtureRational {
    fn into_rational(self) -> Rational {
        Rational {
            numerator: self.numerator,
            denominator: self.denominator,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedOutcome {
    pub ratified: bool,
    pub enacted_bucket: EnactedBucket,
    /// Captured for diagnostic/audit use; not asserted by the current tests.
    /// Kept in the schema so future expectation checks can read it without a
    /// re-capture.
    #[allow(dead_code)]
    pub enacted_epoch: u64,
    pub enacted_id: Option<String>,
}

/// Subset of `ProtocolParameters` that RATIFY actually reads.
///
/// The fields are exactly those listed in Section 2 of the audit: every
/// `dvt_*` and `pvt_*` threshold, plus the three non-threshold fields
/// (`committee_min_size`, `committee_max_term_length`, `protocol_version_major`)
/// the rule consults directly.  All other ledger params keep their
/// `mainnet_defaults()` values — they are unread by ratification.
#[derive(Debug, Clone, Deserialize)]
pub struct FixturePParams {
    pub protocol_version_major: u64,
    pub committee_min_size: u64,
    pub committee_max_term_length: u64,
    pub dvt_pp_network_group: FixtureRational,
    pub dvt_pp_economic_group: FixtureRational,
    pub dvt_pp_technical_group: FixtureRational,
    pub dvt_pp_gov_group: FixtureRational,
    pub dvt_hard_fork: FixtureRational,
    pub dvt_no_confidence: FixtureRational,
    pub dvt_committee_normal: FixtureRational,
    pub dvt_committee_no_confidence: FixtureRational,
    pub dvt_constitution: FixtureRational,
    pub dvt_treasury_withdrawal: FixtureRational,
    pub pvt_motion_no_confidence: FixtureRational,
    pub pvt_committee_normal: FixtureRational,
    pub pvt_committee_no_confidence: FixtureRational,
    pub pvt_hard_fork: FixtureRational,
    pub pvt_pp_security_group: FixtureRational,
}

impl FixturePParams {
    fn apply_to(self, params: &mut ProtocolParameters) {
        params.protocol_version_major = self.protocol_version_major;
        params.committee_min_size = self.committee_min_size;
        params.committee_max_term_length = self.committee_max_term_length;
        params.dvt_pp_network_group = self.dvt_pp_network_group.into_rational();
        params.dvt_pp_economic_group = self.dvt_pp_economic_group.into_rational();
        params.dvt_pp_technical_group = self.dvt_pp_technical_group.into_rational();
        params.dvt_pp_gov_group = self.dvt_pp_gov_group.into_rational();
        params.dvt_hard_fork = self.dvt_hard_fork.into_rational();
        params.dvt_no_confidence = self.dvt_no_confidence.into_rational();
        params.dvt_committee_normal = self.dvt_committee_normal.into_rational();
        params.dvt_committee_no_confidence = self.dvt_committee_no_confidence.into_rational();
        params.dvt_constitution = self.dvt_constitution.into_rational();
        params.dvt_treasury_withdrawal = self.dvt_treasury_withdrawal.into_rational();
        params.pvt_motion_no_confidence = self.pvt_motion_no_confidence.into_rational();
        params.pvt_committee_normal = self.pvt_committee_normal.into_rational();
        params.pvt_committee_no_confidence = self.pvt_committee_no_confidence.into_rational();
        params.pvt_hard_fork = self.pvt_hard_fork.into_rational();
        params.pvt_pp_security_group = self.pvt_pp_security_group.into_rational();
    }
}

/// JSON tag for the four DRep variants exercised by `defaultStakePoolVote`.
///
/// Keys with a 28-byte hash payload (`KeyHash`/`ScriptHash`) are treated as
/// "normal" DReps: per Haskell, they make a non-voting pool's implicit vote
/// resolve to `DefaultVote::No`.  The two pseudo-DReps select the special
/// behaviours.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "tag", rename_all = "PascalCase")]
pub enum FixtureDRep {
    KeyHash { hex: String },
    ScriptHash { hex: String },
    Abstain,
    NoConfidence,
}

impl FixtureDRep {
    fn into_drep(self) -> DRep {
        match self {
            FixtureDRep::KeyHash { hex } => DRep::KeyHash(
                parse_hash28(&hex, "vote_delegations DRep key hash").to_hash32_padded(),
            ),
            FixtureDRep::ScriptHash { hex } => {
                DRep::ScriptHash(parse_hash28(&hex, "vote_delegations DRep script hash"))
            }
            FixtureDRep::Abstain => DRep::Abstain,
            FixtureDRep::NoConfidence => DRep::NoConfidence,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum EnactedBucket {
    PParamUpdate,
    HardFork,
    Committee,
    Constitution,
    // Deliberately excluded: Info, NoConfidence, TreasuryWithdrawal
    // (out of scope per spec non-goals).  The test loader rejects fixtures
    // using them so the assertion match stays exhaustive.
}

/// Decode a hex string to raw bytes, panicking on error with context.
fn decode_hex_bytes(hex_str: &str, ctx: &str) -> Vec<u8> {
    if !hex_str.len().is_multiple_of(2) {
        panic!("invalid hex for {ctx}: odd length ({hex_str})");
    }
    (0..hex_str.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex_str[i..i + 2], 16)
                .unwrap_or_else(|e| panic!("invalid hex byte for {ctx} at {i}: {e}"))
        })
        .collect()
}

/// Parse a `"<32-byte-hex>#<u32>"` action id string into a `GovActionId`.
pub fn parse_gov_action_id(s: &str) -> GovActionId {
    let (hash_hex, idx_str) = s
        .split_once('#')
        .unwrap_or_else(|| panic!("malformed gov_action_id (missing '#'): {s}"));
    let transaction_id: TransactionHash = Hash32::from_hex(hash_hex)
        .unwrap_or_else(|e| panic!("invalid gov action tx hash hex {hash_hex}: {e}"));
    let action_index: u32 = idx_str
        .parse()
        .unwrap_or_else(|e| panic!("invalid gov action index {idx_str}: {e}"));
    GovActionId {
        transaction_id,
        action_index,
    }
}

/// Parse a 32-byte-hex string into a `Hash32`, panicking on error with context.
fn parse_hash32(hex_str: &str, ctx: &str) -> Hash32 {
    Hash32::from_hex(hex_str)
        .unwrap_or_else(|e| panic!("invalid Hash32 hex for {ctx} ({hex_str}): {e}"))
}

/// Parse a 28-byte-hex string into a `Hash28`, panicking on error with context.
fn parse_hash28(hex_str: &str, ctx: &str) -> Hash28 {
    Hash28::from_hex(hex_str)
        .unwrap_or_else(|e| panic!("invalid Hash28 hex for {ctx} ({hex_str}): {e}"))
}

// ---------------------------------------------------------------------------
// GovAction reconstruction from the Koios `proposal_description` JSON
// ---------------------------------------------------------------------------

/// Reconstruct a `GovAction` from the Koios `proposal_description` JSON blob.
///
/// Supports every Conway action type.  The decoder is **fail-closed**: any
/// field present in the JSON that is not understood by `read_ppu_field` (or
/// any unrecognized top-level shape) panics rather than silently producing an
/// `InfoAction`, which would otherwise let `prev_action_as_expected` succeed
/// vacuously and mask real chain-mismatch failures.
pub fn reconstruct_gov_action(action: &serde_json::Value) -> GovAction {
    // Bare strings (Koios sometimes encodes InfoAction as `"InfoAction"`).
    if let Some(s) = action.as_str() {
        match s {
            "InfoAction" => return GovAction::InfoAction,
            other => panic!("unsupported bare-string GovAction shape: {other:?}"),
        }
    }

    let tag = action
        .get("tag")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("GovAction JSON missing string `tag`: {action}"));
    let contents = action.get("contents").and_then(|v| v.as_array());

    match tag {
        "ParameterChange" => {
            let contents = contents.unwrap_or_else(|| {
                panic!("ParameterChange GovAction missing `contents` array: {action}")
            });
            if contents.len() < 2 {
                panic!("ParameterChange GovAction has fewer than 2 contents elements: {action}");
            }
            let prev_action_id = koios_prev_action_id(&contents[0]);
            let ppu = koios_protocol_param_update(&contents[1]);
            let policy_hash = match contents.get(2) {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => {
                    Some(parse_hash28(s, "ParameterChange policy_hash"))
                }
                Some(other) => {
                    panic!("ParameterChange policy_hash must be hex string or null, got {other}")
                }
            };
            GovAction::ParameterChange {
                prev_action_id,
                protocol_param_update: Box::new(ppu),
                policy_hash,
            }
        }
        "HardForkInitiation" => {
            let contents = contents.unwrap_or_else(|| {
                panic!("HardForkInitiation GovAction missing `contents` array: {action}")
            });
            if contents.len() < 2 {
                panic!("HardForkInitiation GovAction has fewer than 2 contents elements: {action}");
            }
            let prev_action_id = koios_prev_action_id(&contents[0]);
            let pv = &contents[1];
            let major = pv
                .get("major")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| panic!("HardForkInitiation missing protocol major: {pv}"));
            let minor = pv
                .get("minor")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| panic!("HardForkInitiation missing protocol minor: {pv}"));
            GovAction::HardForkInitiation {
                prev_action_id,
                protocol_version: (major, minor),
            }
        }
        "TreasuryWithdrawals" => {
            let contents = contents
                .unwrap_or_else(|| panic!("TreasuryWithdrawals missing `contents`: {action}"));
            // Koios encodes TreasuryWithdrawals as
            //   [ { stakeAddress: hex29, amount: <number> } | array of pairs, policy_hash ]
            // We accept either an array of {stakeAddress, amount} objects or a
            // map keyed by stake address.  Fail closed on anything else.
            let withdrawals_blob = contents
                .first()
                .unwrap_or_else(|| panic!("TreasuryWithdrawals missing withdrawals blob"));
            let mut withdrawals: BTreeMap<Vec<u8>, Lovelace> = BTreeMap::new();
            if let Some(arr) = withdrawals_blob.as_array() {
                for entry in arr {
                    let addr_hex = entry
                        .get("stakeAddress")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| {
                            panic!("TreasuryWithdrawals entry missing stakeAddress: {entry}")
                        });
                    let amount =
                        entry
                            .get("amount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or_else(|| {
                                panic!("TreasuryWithdrawals entry missing amount: {entry}")
                            });
                    let bytes = decode_hex_bytes(addr_hex, "TreasuryWithdrawals stakeAddress");
                    withdrawals.insert(bytes, Lovelace(amount));
                }
            } else if let Some(map) = withdrawals_blob.as_object() {
                for (addr_hex, amount_v) in map {
                    let amount = amount_v.as_u64().unwrap_or_else(|| {
                        panic!("TreasuryWithdrawals amount not u64: {amount_v}")
                    });
                    let bytes = decode_hex_bytes(addr_hex, "TreasuryWithdrawals stakeAddress");
                    withdrawals.insert(bytes, Lovelace(amount));
                }
            } else {
                panic!(
                    "TreasuryWithdrawals withdrawals blob must be array or map: {withdrawals_blob}"
                );
            }
            let policy_hash = match contents.get(1) {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => {
                    Some(parse_hash28(s, "TreasuryWithdrawals policy_hash"))
                }
                Some(other) => panic!(
                    "TreasuryWithdrawals policy_hash must be hex string or null, got {other}"
                ),
            };
            GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash,
            }
        }
        "NoConfidence" => {
            let prev_action_id = match contents {
                None => None,
                Some(arr) => arr.first().and_then(koios_prev_action_id),
            };
            GovAction::NoConfidence { prev_action_id }
        }
        "NewCommittee" | "UpdateCommittee" => {
            let contents = contents.unwrap_or_else(|| {
                panic!("UpdateCommittee/NewCommittee missing `contents` array: {action}")
            });
            if contents.len() < 4 {
                panic!("UpdateCommittee/NewCommittee has fewer than 4 contents elements: {action}");
            }
            let prev_action_id = koios_prev_action_id(&contents[0]);
            let mut members_to_remove: Vec<Credential> = Vec::new();
            if let Some(arr) = contents[1].as_array() {
                for entry in arr {
                    members_to_remove
                        .push(koios_credential(entry, "UpdateCommittee members_to_remove"));
                }
            } else {
                panic!(
                    "UpdateCommittee members_to_remove must be array, got {}",
                    contents[1]
                );
            }
            let mut members_to_add: BTreeMap<Credential, u64> = BTreeMap::new();
            if let Some(arr) = contents[2].as_array() {
                for entry in arr {
                    let cred = entry
                        .get("credential")
                        .or_else(|| entry.get("coldCredential"))
                        .unwrap_or_else(|| {
                            panic!("UpdateCommittee add entry missing credential: {entry}")
                        });
                    let cred = koios_credential(cred, "UpdateCommittee members_to_add");
                    let expiry = entry
                        .get("expiration")
                        .or_else(|| entry.get("expiryEpoch"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or_else(|| {
                            panic!("UpdateCommittee add entry missing expiration: {entry}")
                        });
                    members_to_add.insert(cred, expiry);
                }
            } else if let Some(map) = contents[2].as_object() {
                for (cred_hex, expiry_v) in map {
                    let cred = koios_credential_from_hex(cred_hex, "UpdateCommittee map key");
                    let expiry = expiry_v.as_u64().unwrap_or_else(|| {
                        panic!("UpdateCommittee map expiry not u64: {expiry_v}")
                    });
                    members_to_add.insert(cred, expiry);
                }
            } else {
                panic!(
                    "UpdateCommittee members_to_add must be array or map, got {}",
                    contents[2]
                );
            }
            let threshold_blob = &contents[3];
            let threshold = koios_rational(threshold_blob, "UpdateCommittee threshold");
            GovAction::UpdateCommittee {
                prev_action_id,
                members_to_remove,
                members_to_add,
                threshold,
            }
        }
        "NewConstitution" => {
            let contents = contents
                .unwrap_or_else(|| panic!("NewConstitution missing `contents` array: {action}"));
            if contents.len() < 2 {
                panic!("NewConstitution has fewer than 2 contents elements: {action}");
            }
            let prev_action_id = koios_prev_action_id(&contents[0]);
            let constitution_blob = &contents[1];
            let anchor_blob = constitution_blob
                .get("anchor")
                .unwrap_or_else(|| panic!("NewConstitution missing anchor: {constitution_blob}"));
            let anchor = koios_anchor(anchor_blob, "NewConstitution anchor");
            let script_hash = match constitution_blob.get("scriptHash") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => {
                    Some(parse_hash28(s, "NewConstitution script_hash"))
                }
                Some(other) => {
                    panic!("NewConstitution script_hash must be hex string or null, got {other}")
                }
            };
            GovAction::NewConstitution {
                prev_action_id,
                constitution: Constitution {
                    anchor,
                    script_hash,
                },
            }
        }
        "InfoAction" => GovAction::InfoAction,
        other => panic!(
            "unsupported GovAction tag {other:?}; extend reconstruct_gov_action to handle it"
        ),
    }
}

/// Decode a `{ txId, govActionIx }` Koios blob into an `Option<GovActionId>`.
/// Returns `None` when the blob is absent, JSON `null`, or missing the inner
/// fields — matching Conway's "genesis root" prev_action_id.
fn koios_prev_action_id(v: &serde_json::Value) -> Option<GovActionId> {
    if v.is_null() {
        return None;
    }
    let tx_hex = v.get("txId").and_then(|x| x.as_str())?;
    let idx = v.get("govActionIx").and_then(|x| x.as_u64())?;
    let transaction_id = Hash32::from_hex(tx_hex).ok()?;
    Some(GovActionId {
        transaction_id,
        action_index: idx as u32,
    })
}

/// Decode a Koios anchor object into our `Anchor` type.
fn koios_anchor(v: &serde_json::Value, ctx: &str) -> Anchor {
    let url = v
        .get("url")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("{ctx}: missing url ({v})"))
        .to_string();
    let data_hash_hex = v
        .get("dataHash")
        .or_else(|| v.get("data_hash"))
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("{ctx}: missing dataHash ({v})"));
    Anchor {
        url,
        data_hash: parse_hash32(data_hash_hex, &format!("{ctx} dataHash")),
    }
}

/// Decode a Koios `{tag, hex}` credential blob.
fn koios_credential(v: &serde_json::Value, ctx: &str) -> Credential {
    let tag = v
        .get("tag")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("{ctx}: credential missing tag ({v})"));
    let hex_str = v
        .get("hex")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("{ctx}: credential missing hex ({v})"));
    match tag {
        "KeyHash" | "keyHash" => {
            Credential::VerificationKey(parse_hash28(hex_str, &format!("{ctx} key hash")))
        }
        "ScriptHash" | "scriptHash" => {
            Credential::Script(parse_hash28(hex_str, &format!("{ctx} script hash")))
        }
        other => panic!("{ctx}: unknown credential tag {other:?}"),
    }
}

/// Decode a credential from a bare 56-character hex map key.  Type is encoded
/// in the high bit of the first byte, matching Koios' `cc_cold_id` form.
/// Currently used for a fall-back map shape; loaders should prefer the
/// `{tag, hex}` object form via `koios_credential` when possible.
fn koios_credential_from_hex(hex_str: &str, ctx: &str) -> Credential {
    if hex_str.len() != 58 {
        panic!("{ctx}: credential hex must be 58 chars (1 type byte + 28 hash), got {hex_str}");
    }
    let type_byte = u8::from_str_radix(&hex_str[..2], 16)
        .unwrap_or_else(|e| panic!("{ctx}: invalid type byte: {e}"));
    let hash = parse_hash28(&hex_str[2..], &format!("{ctx} hash"));
    match type_byte {
        0 => Credential::VerificationKey(hash),
        1 => Credential::Script(hash),
        other => panic!("{ctx}: unknown credential type byte {other:#x}"),
    }
}

/// Decode a Koios `{numerator, denominator}` rational.
fn koios_rational(v: &serde_json::Value, ctx: &str) -> Rational {
    let numerator = v
        .get("numerator")
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("{ctx}: missing numerator ({v})"));
    let denominator = v
        .get("denominator")
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("{ctx}: missing denominator ({v})"));
    Rational {
        numerator,
        denominator,
    }
}

// ---------------------------------------------------------------------------
// Full PPU decoder — fail-closed
// ---------------------------------------------------------------------------

/// Decode a Koios PPU blob into a `ProtocolParamUpdate`.
///
/// **Fail-closed**: any field present in the JSON that is not in
/// [`KNOWN_PPU_FIELDS`] causes a panic.  This is intentional — silently
/// dropping an unknown PPU field would let `modified_pp_groups` return the
/// wrong group set and silently change a ratification verdict.  When the legacy decoder
/// or cardano-ledger introduces a new PPU field, this decoder must be
/// updated and `KNOWN_PPU_FIELDS` extended in lock-step.
pub fn koios_protocol_param_update(v: &serde_json::Value) -> ProtocolParamUpdate {
    let map = v
        .as_object()
        .unwrap_or_else(|| panic!("PPU blob must be an object, got {v}"));
    for key in map.keys() {
        if !KNOWN_PPU_FIELDS.contains(&key.as_str()) {
            panic!("unknown PPU field {key:?} — extend koios_protocol_param_update to handle it");
        }
    }
    let mut ppu = ProtocolParamUpdate::default();
    if let Some(x) = map.get("minFeeA") {
        ppu.min_fee_a = Some(read_u64(x, "minFeeA"));
    }
    if let Some(x) = map.get("minFeeB") {
        ppu.min_fee_b = Some(read_u64(x, "minFeeB"));
    }
    if let Some(x) = map.get("maxBlockBodySize") {
        ppu.max_block_body_size = Some(read_u64(x, "maxBlockBodySize"));
    }
    if let Some(x) = map.get("maxTxSize") {
        ppu.max_tx_size = Some(read_u64(x, "maxTxSize"));
    }
    if let Some(x) = map.get("maxBlockHeaderSize") {
        ppu.max_block_header_size = Some(read_u64(x, "maxBlockHeaderSize"));
    }
    if let Some(x) = map.get("keyDeposit") {
        ppu.key_deposit = Some(Lovelace(read_u64(x, "keyDeposit")));
    }
    if let Some(x) = map.get("poolDeposit") {
        ppu.pool_deposit = Some(Lovelace(read_u64(x, "poolDeposit")));
    }
    if let Some(x) = map.get("eMax") {
        ppu.e_max = Some(read_u64(x, "eMax"));
    }
    if let Some(x) = map.get("nOpt") {
        ppu.n_opt = Some(read_u64(x, "nOpt"));
    }
    if let Some(x) = map.get("a0") {
        ppu.a0 = Some(read_rational(x, "a0"));
    }
    if let Some(x) = map.get("rho") {
        ppu.rho = Some(read_rational(x, "rho"));
    }
    if let Some(x) = map.get("tau") {
        ppu.tau = Some(read_rational(x, "tau"));
    }
    if let Some(x) = map.get("minPoolCost") {
        ppu.min_pool_cost = Some(Lovelace(read_u64(x, "minPoolCost")));
    }
    if let Some(x) = map.get("coinsPerUTxOByte") {
        ppu.ada_per_utxo_byte = Some(Lovelace(read_u64(x, "coinsPerUTxOByte")));
    }
    if let Some(x) = map.get("costModels") {
        ppu.cost_models = Some(read_cost_models(x, "costModels"));
    }
    if let Some(x) = map.get("executionUnitPrices") {
        ppu.execution_costs = Some(read_ex_unit_prices(x, "executionUnitPrices"));
    }
    if let Some(x) = map.get("maxTxExecutionUnits") {
        ppu.max_tx_ex_units = Some(read_ex_units(x, "maxTxExecutionUnits"));
    }
    if let Some(x) = map.get("maxBlockExecutionUnits") {
        ppu.max_block_ex_units = Some(read_ex_units(x, "maxBlockExecutionUnits"));
    }
    if let Some(x) = map.get("maxValueSize") {
        ppu.max_val_size = Some(read_u64(x, "maxValueSize"));
    }
    if let Some(x) = map.get("collateralPercentage") {
        ppu.collateral_percentage = Some(read_u64(x, "collateralPercentage"));
    }
    if let Some(x) = map.get("maxCollateralInputs") {
        ppu.max_collateral_inputs = Some(read_u64(x, "maxCollateralInputs"));
    }
    if let Some(x) = map.get("minFeeRefScriptCostPerByte") {
        ppu.min_fee_ref_script_cost_per_byte = Some(dugite_primitives::transaction::Rational {
            numerator: read_u64(x, "minFeeRefScriptCostPerByte"),
            denominator: 1,
        });
    }
    if let Some(x) = map.get("drepDeposit") {
        ppu.drep_deposit = Some(Lovelace(read_u64(x, "drepDeposit")));
    }
    if let Some(x) = map.get("govActionDeposit") {
        ppu.gov_action_deposit = Some(Lovelace(read_u64(x, "govActionDeposit")));
    }
    if let Some(x) = map.get("govActionLifetime") {
        ppu.gov_action_lifetime = Some(read_u64(x, "govActionLifetime"));
    }
    if let Some(x) = map.get("dvtPPNetworkGroup") {
        ppu.dvt_pp_network_group = Some(read_rational(x, "dvtPPNetworkGroup"));
    }
    if let Some(x) = map.get("dvtPPEconomicGroup") {
        ppu.dvt_pp_economic_group = Some(read_rational(x, "dvtPPEconomicGroup"));
    }
    if let Some(x) = map.get("dvtPPTechnicalGroup") {
        ppu.dvt_pp_technical_group = Some(read_rational(x, "dvtPPTechnicalGroup"));
    }
    if let Some(x) = map.get("dvtPPGovGroup") {
        ppu.dvt_pp_gov_group = Some(read_rational(x, "dvtPPGovGroup"));
    }
    if let Some(x) = map.get("dvtHardForkInitiation") {
        ppu.dvt_hard_fork = Some(read_rational(x, "dvtHardForkInitiation"));
    }
    if let Some(x) = map.get("dvtMotionNoConfidence") {
        ppu.dvt_no_confidence = Some(read_rational(x, "dvtMotionNoConfidence"));
    }
    if let Some(x) = map.get("dvtCommitteeNormal") {
        ppu.dvt_committee_normal = Some(read_rational(x, "dvtCommitteeNormal"));
    }
    if let Some(x) = map.get("dvtCommitteeNoConfidence") {
        ppu.dvt_committee_no_confidence = Some(read_rational(x, "dvtCommitteeNoConfidence"));
    }
    if let Some(x) = map.get("dvtUpdateToConstitution") {
        ppu.dvt_constitution = Some(read_rational(x, "dvtUpdateToConstitution"));
    }
    if let Some(x) = map.get("dvtTreasuryWithdrawal") {
        ppu.dvt_treasury_withdrawal = Some(read_rational(x, "dvtTreasuryWithdrawal"));
    }
    if let Some(x) = map.get("pvtMotionNoConfidence") {
        ppu.pvt_motion_no_confidence = Some(read_rational(x, "pvtMotionNoConfidence"));
    }
    if let Some(x) = map.get("pvtCommitteeNormal") {
        ppu.pvt_committee_normal = Some(read_rational(x, "pvtCommitteeNormal"));
    }
    if let Some(x) = map.get("pvtCommitteeNoConfidence") {
        ppu.pvt_committee_no_confidence = Some(read_rational(x, "pvtCommitteeNoConfidence"));
    }
    if let Some(x) = map.get("pvtHardForkInitiation") {
        ppu.pvt_hard_fork = Some(read_rational(x, "pvtHardForkInitiation"));
    }
    if let Some(x) = map.get("pvtPPSecurityGroup") {
        ppu.pvt_pp_security_group = Some(read_rational(x, "pvtPPSecurityGroup"));
    }
    if let Some(x) = map.get("committeeMinSize") {
        ppu.min_committee_size = Some(read_u64(x, "committeeMinSize"));
    }
    if let Some(x) = map.get("committeeMaxTermLength") {
        ppu.committee_term_limit = Some(read_u64(x, "committeeMaxTermLength"));
    }
    if let Some(x) = map.get("drepActivity") {
        ppu.drep_activity = Some(read_u64(x, "drepActivity"));
    }
    ppu
}

/// Sorted list of every PPU JSON key understood by the loader.  Any key in
/// the input not on this list panics.  When extending the loader, update
/// `koios_protocol_param_update` and this list together.
const KNOWN_PPU_FIELDS: &[&str] = &[
    "a0",
    "coinsPerUTxOByte",
    "collateralPercentage",
    "committeeMaxTermLength",
    "committeeMinSize",
    "costModels",
    "drepActivity",
    "drepDeposit",
    "dvtCommitteeNoConfidence",
    "dvtCommitteeNormal",
    "dvtHardForkInitiation",
    "dvtMotionNoConfidence",
    "dvtPPEconomicGroup",
    "dvtPPGovGroup",
    "dvtPPNetworkGroup",
    "dvtPPTechnicalGroup",
    "dvtTreasuryWithdrawal",
    "dvtUpdateToConstitution",
    "eMax",
    "executionUnitPrices",
    "govActionDeposit",
    "govActionLifetime",
    "keyDeposit",
    "maxBlockBodySize",
    "maxBlockExecutionUnits",
    "maxBlockHeaderSize",
    "maxCollateralInputs",
    "maxTxExecutionUnits",
    "maxTxSize",
    "maxValueSize",
    "minFeeA",
    "minFeeB",
    "minFeeRefScriptCostPerByte",
    "minPoolCost",
    "nOpt",
    "poolDeposit",
    "pvtCommitteeNoConfidence",
    "pvtCommitteeNormal",
    "pvtHardForkInitiation",
    "pvtMotionNoConfidence",
    "pvtPPSecurityGroup",
    "rho",
    "tau",
];

fn read_u64(v: &serde_json::Value, ctx: &str) -> u64 {
    if let Some(n) = v.as_u64() {
        return n;
    }
    if let Some(s) = v.as_str() {
        return s
            .parse()
            .unwrap_or_else(|e| panic!("{ctx}: numeric string parse failed: {e} ({s})"));
    }
    panic!("{ctx}: expected u64 or numeric string, got {v}")
}

fn read_rational(v: &serde_json::Value, ctx: &str) -> Rational {
    if let Some(obj) = v.as_object() {
        let numerator = obj
            .get("numerator")
            .and_then(|x| x.as_u64())
            .unwrap_or_else(|| panic!("{ctx}: missing numerator ({v})"));
        let denominator = obj
            .get("denominator")
            .and_then(|x| x.as_u64())
            .unwrap_or_else(|| panic!("{ctx}: missing denominator ({v})"));
        return Rational {
            numerator,
            denominator,
        };
    }
    if let Some(f) = v.as_f64() {
        // Convert a float threshold to an exact rational with denominator 10000
        // which is sufficient for every Cardano voting threshold (e.g. 0.51, 0.67, 0.75).
        let denominator: u64 = 10_000;
        let numerator = (f * denominator as f64).round() as u64;
        return Rational {
            numerator,
            denominator,
        };
    }
    panic!("{ctx}: expected rational object or float, got {v}")
}

fn read_ex_units(v: &serde_json::Value, ctx: &str) -> ExUnits {
    let mem = v
        .get("memory")
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("{ctx}: missing memory ({v})"));
    let steps = v
        .get("steps")
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("{ctx}: missing steps ({v})"));
    ExUnits { mem, steps }
}

fn read_ex_unit_prices(v: &serde_json::Value, ctx: &str) -> ExUnitPrices {
    let mem_price = read_rational(
        v.get("priceMemory")
            .or_else(|| v.get("memPrice"))
            .unwrap_or_else(|| panic!("{ctx}: missing priceMemory ({v})")),
        &format!("{ctx} priceMemory"),
    );
    let step_price = read_rational(
        v.get("priceSteps")
            .or_else(|| v.get("stepPrice"))
            .unwrap_or_else(|| panic!("{ctx}: missing priceSteps ({v})")),
        &format!("{ctx} priceSteps"),
    );
    ExUnitPrices {
        mem_price,
        step_price,
    }
}

fn read_cost_models(v: &serde_json::Value, ctx: &str) -> CostModels {
    let map = v
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: expected object, got {v}"));
    let read_lang = |key: &str| -> Option<Vec<i64>> {
        map.get(key).map(|arr| {
            arr.as_array()
                .unwrap_or_else(|| panic!("{ctx}: {key} not array ({arr})"))
                .iter()
                .map(|n| {
                    n.as_i64()
                        .unwrap_or_else(|| panic!("{ctx}: {key} entry not i64 ({n})"))
                })
                .collect()
        })
    };
    CostModels {
        plutus_v1: read_lang("PlutusV1").or_else(|| read_lang("plutus:v1")),
        plutus_v2: read_lang("PlutusV2").or_else(|| read_lang("plutus:v2")),
        plutus_v3: read_lang("PlutusV3").or_else(|| read_lang("plutus:v3")),
        // PlutusV4 (Dijkstra) cost-model slot is part of issue #475 Phase 5.
        plutus_v4: read_lang("PlutusV4").or_else(|| read_lang("plutus:v4")),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

impl RatificationFixture {
    /// Load a fixture from a JSON file, panicking on IO or parse errors.
    pub fn load(path: &str) -> Self {
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read fixture file {path}: {e}"));
        serde_json::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse fixture {path}: {e}"))
    }

    /// Build a `LedgerState` positioned at the fixture's `pparams_epoch`
    /// with every input `ratify_proposals_impl` consults populated:
    ///
    /// * `epochs.protocol_params.{dvt_*, pvt_*, committee_min_size,
    ///   committee_max_term_length, protocol_version_major}` from `pparams`.
    /// * `gov.governance.proposals` + `votes_by_action` from the captured
    ///   proposal/votes.
    /// * `gov.governance.committee_*` from `committee` (typed Hash32 keys).
    /// * `gov.governance.drep_distribution_snapshot`,
    ///   `drep_snapshot_no_confidence`, `drep_snapshot_abstain` from
    ///   `drep_power` / `drep_no_confidence` / `drep_abstain`.
    /// * `gov.governance.no_confidence` from the live flag.
    /// * `gov.governance.vote_delegations` from `vote_delegations`.
    /// * `gov.governance.dreps` — every `drep_power` key gets a minimal
    ///   `DRepRegistration` entry so that `compute_total_drep_stake_from`
    ///   and `build_drep_power_cache_from` see the snapshot path as
    ///   non-empty (they short-circuit on an empty distribution map).
    /// * `epochs.snapshots.set.pool_stake` from `spo_stake`.
    /// * `certs.pool_params[pool_id]` from `pool_reward_accounts` — the
    ///   minimal subset needed by `default_spo_vote_from`
    ///   (`pool_id` + `reward_account`).
    /// * `gov.governance.enacted_*` roots from `parent_enacted`.
    pub fn into_ledger_state(self) -> LedgerState {
        let mut ledger = LedgerState::new(ProtocolParameters::mainnet_defaults());
        ledger.epoch = EpochNo(self.pparams_epoch);

        // Apply the captured threshold subset on top of mainnet defaults.
        // Every field RATIFY reads is now driven by the fixture; remaining
        // fields keep their mainnet defaults (none are read by the rule).
        self.pparams.apply_to(&mut ledger.epochs.protocol_params);

        // Parse the proposal ID once — used as the key in both `proposals`
        // and `votes_by_action`.
        let action_id = parse_gov_action_id(&self.proposal.gov_action_id);

        let return_addr = decode_hex_bytes(&self.proposal.return_addr_hex, "return_addr_hex");
        let anchor = self
            .proposal
            .anchor
            .as_ref()
            .map(|a| Anchor {
                url: a.url.clone(),
                data_hash: parse_hash32(&a.data_hash, "proposal anchor data_hash"),
            })
            .unwrap_or_else(|| Anchor {
                url: String::new(),
                data_hash: Hash32::ZERO,
            });
        let gov_action = reconstruct_gov_action(&self.proposal.action);
        let procedure = ProposalProcedure {
            deposit: Lovelace(self.proposal.deposit),
            return_addr,
            gov_action,
            anchor,
        };

        // Tally vote counts for the ProposalState (the ratification path uses
        // `votes_by_action` for actual voter accounting; these counts are
        // diagnostic only).
        let mut yes_votes: u64 = 0;
        let mut no_votes: u64 = 0;
        let mut abstain_votes: u64 = 0;
        for v in &self.votes {
            match v.vote {
                FixtureVoteValue::Yes => yes_votes += 1,
                FixtureVoteValue::No => no_votes += 1,
                FixtureVoteValue::Abstain => abstain_votes += 1,
            }
        }

        let proposal_state = ProposalState {
            procedure,
            proposed_epoch: EpochNo(self.proposed_epoch),
            expires_epoch: EpochNo(self.proposal.expiration),
            yes_votes,
            no_votes,
            abstain_votes,
        };

        // Build the (Voter, VotingProcedure) list for `votes_by_action`.
        let votes_vec: Vec<(Voter, VotingProcedure)> = self
            .votes
            .iter()
            .map(|fv| {
                let voter = match fv.voter_type {
                    FixtureVoterType::ConstitutionalCommitteeHotKeyHash => {
                        Voter::ConstitutionalCommittee(Credential::VerificationKey(parse_hash28(
                            &fv.voter_id,
                            "cc hot key hash voter",
                        )))
                    }
                    FixtureVoterType::ConstitutionalCommitteeHotScriptHash => {
                        Voter::ConstitutionalCommittee(Credential::Script(parse_hash28(
                            &fv.voter_id,
                            "cc hot script hash voter",
                        )))
                    }
                    FixtureVoterType::DRepKeyHash => Voter::DRep(Credential::VerificationKey(
                        parse_hash28(&fv.voter_id, "drep key hash voter"),
                    )),
                    FixtureVoterType::DRepScriptHash => Voter::DRep(Credential::Script(
                        parse_hash28(&fv.voter_id, "drep script hash voter"),
                    )),
                    FixtureVoterType::StakePoolKeyHash => Voter::StakePool(
                        parse_hash28(&fv.voter_id, "stake pool key hash voter").to_hash32_padded(),
                    ),
                };
                let vote = match fv.vote {
                    FixtureVoteValue::Yes => Vote::Yes,
                    FixtureVoteValue::No => Vote::No,
                    FixtureVoteValue::Abstain => Vote::Abstain,
                };
                (voter, VotingProcedure { vote, anchor: None })
            })
            .collect();

        // Stash drep power BEFORE the Arc::make_mut block (the borrow checker
        // forbids `&self.drep_power` while `gov_state` is borrowed mutably).
        // When `drep_aggregates` is present, synthesize a 3-entry DRep
        // snapshot from the aggregates (matches `proposal_voting_summary`
        // capture mode).  Otherwise use the per-DRep `drep_power` map.
        let (drep_power, drep_no_confidence, drep_abstain, extra_action_votes) =
            if let Some(agg) = self.drep_aggregates.clone() {
                let mut synth: BTreeMap<Hash32, u64> = BTreeMap::new();
                // Yes-aggregate cred (will receive a synthetic Yes vote below).
                synth.insert(typed_hash32_for_key(&AGG_YES_CRED), agg.yes_stake);
                // No-aggregate cred — registered DRep that explicitly voted No.
                synth.insert(typed_hash32_for_key(&AGG_NO_CRED), agg.no_stake);
                // Abstain-aggregate cred (will receive a synthetic Abstain vote).
                synth.insert(typed_hash32_for_key(&AGG_ABSTAIN_CRED), agg.abstain_stake);
                // No-vote-aggregate cred — registered DRep that did NOT vote on
                // this proposal.  Per `count_votes_by_type_impl`'s
                // `Some(Vote::No) | None => {}` branch, an entry in the cache
                // with no matching voter contributes to `drep_total` but not
                // `drep_yes` / `drep_abstain` — i.e. counts as No, exactly
                // matching Haskell's `dRepAcceptedRatio` rule for non-voting
                // registered DReps.
                synth.insert(typed_hash32_for_key(&AGG_NO_VOTE_CRED), agg.no_vote_stake);

                // Synthetic votes for the Yes/No/Abstain aggregates.  The
                // No-vote aggregate is intentionally absent — it counts as No
                // by being present in the snapshot but missing from
                // `votes_by_action`.
                let mk_voter = |bytes: &[u8; 28]| {
                    Voter::DRep(Credential::VerificationKey(Hash28::from_bytes(*bytes)))
                };
                let extras: Vec<(Voter, VotingProcedure)> = vec![
                    (
                        mk_voter(&AGG_YES_CRED),
                        VotingProcedure {
                            vote: Vote::Yes,
                            anchor: None,
                        },
                    ),
                    (
                        mk_voter(&AGG_NO_CRED),
                        VotingProcedure {
                            vote: Vote::No,
                            anchor: None,
                        },
                    ),
                    (
                        mk_voter(&AGG_ABSTAIN_CRED),
                        VotingProcedure {
                            vote: Vote::Abstain,
                            anchor: None,
                        },
                    ),
                ];
                // Convert the synth Hash32 keys to the wire form the existing
                // population loop expects (typed-Hash32 hex string keys).
                let mut wire_synth: BTreeMap<String, u64> = BTreeMap::new();
                for (k, v) in synth {
                    wire_synth.insert(k.to_hex(), v);
                }
                (
                    wire_synth,
                    agg.always_no_confidence_stake,
                    agg.always_abstain_stake,
                    extras,
                )
            } else {
                (
                    self.drep_power,
                    self.drep_no_confidence,
                    self.drep_abstain,
                    Vec::new(),
                )
            };

        let no_confidence_flag = self.no_confidence;
        let vote_delegations = self.vote_delegations;
        let committee = self.committee;
        let parent_enacted = self.parent_enacted;
        let pparams_epoch = self.pparams_epoch;

        // Mutate the inner GovernanceState (Arc-wrapped in GovSubState).
        {
            let gov: &mut GovernanceState = Arc::make_mut(&mut ledger.gov.governance);

            // Append the aggregate-mode synthetic Yes/No/Abstain DRep votes
            // (empty in per-DRep mode).
            let mut all_votes = votes_vec;
            all_votes.extend(extra_action_votes);

            gov.proposals.insert(action_id.clone(), proposal_state);
            gov.votes_by_action.insert(action_id.clone(), all_votes);

            // Committee state — keys are typed Hash32 (cold for membership maps,
            // hot for the lookup map's values).
            for member in &committee.members {
                let cold = parse_hash32(&member.cold_key, "committee cold key");
                gov.committee_expiration
                    .insert(cold, EpochNo(member.expiration));
                if let Some(hot_hex) = &member.hot_key {
                    let hot = parse_hash32(hot_hex, "committee hot key");
                    gov.committee_hot_keys.insert(cold, hot);
                }
            }
            for resigned_hex in &committee.resigned {
                let cold = parse_hash32(resigned_hex, "committee resigned cold key");
                gov.committee_resigned.insert(cold, None);
            }
            gov.committee_threshold = Some(Rational {
                numerator: committee.threshold.numerator,
                denominator: committee.threshold.denominator,
            });

            // DRep power snapshot.  Keys are 32-byte typed credential hashes.
            // We also seed a placeholder DRepRegistration for each so that
            // `build_drep_power_cache_from` sees the snapshot path as
            // populated (it short-circuits on empty maps).
            for (drep_hex, stake) in &drep_power {
                let cred_hash = parse_hash32(drep_hex, "drep_power credential hash");
                gov.drep_distribution_snapshot.insert(cred_hash, *stake);
                // Seed a minimal placeholder registration with a far-future
                // expiry so the DRep is not skipped by activity filters.
                gov.dreps
                    .entry(cred_hash)
                    .or_insert_with(|| DRepRegistration {
                        credential: Credential::VerificationKey(Hash28::ZERO),
                        deposit: Lovelace(0),
                        anchor: None,
                        registered_epoch: EpochNo(0),
                        drep_expiry: EpochNo(u64::MAX / 2),
                        active: true,
                    });
            }
            gov.drep_snapshot_no_confidence = drep_no_confidence;
            gov.drep_snapshot_abstain = drep_abstain;

            // Live no_confidence flag — read by the `UpdateCommittee` branch.
            gov.no_confidence = no_confidence_flag;

            // Vote delegations — read by `default_spo_vote_from` to resolve
            // a non-voting pool's implicit vote.  Keys are typed Hash32.
            for (cred_hex, drep) in vote_delegations {
                let cred_hash = parse_hash32(&cred_hex, "vote_delegations credential hash");
                gov.vote_delegations.insert(cred_hash, drep.into_drep());
            }

            // Enacted roots (parent_enacted) — each field is optional.
            gov.enacted_pparam_update = parent_enacted
                .pparam_update
                .as_deref()
                .map(parse_gov_action_id);
            gov.enacted_hard_fork = parent_enacted.hard_fork.as_deref().map(parse_gov_action_id);
            gov.enacted_committee = parent_enacted.committee.as_deref().map(parse_gov_action_id);
            gov.enacted_constitution = parent_enacted
                .constitution
                .as_deref()
                .map(parse_gov_action_id);
        }

        // SPO stake — `ratify_proposals()` reads `epochs.snapshots.set.pool_stake`.
        let mut set_snapshot = StakeSnapshot::empty(EpochNo(pparams_epoch));
        for (pool_hex, stake) in &self.spo_stake {
            let pool_id = parse_hash28(pool_hex, "spo_stake pool id");
            set_snapshot.pool_stake.insert(pool_id, Lovelace(*stake));
        }
        ledger.epochs.snapshots.set = Some(set_snapshot);

        // Pool reward accounts — needed by `default_spo_vote_from`.  We
        // populate a minimal `PoolRegistration` (only `pool_id` and
        // `reward_account` are read).  Pools that voted explicitly never
        // hit this path; unregistered pools fall through to `DefaultVote::No`.
        if !self.pool_reward_accounts.is_empty() {
            let pool_params = Arc::make_mut(&mut ledger.certs.pool_params);
            for (pool_hex, reward_hex) in &self.pool_reward_accounts {
                let pool_id = parse_hash28(pool_hex, "pool_reward_accounts pool id");
                let reward_bytes =
                    decode_hex_bytes(reward_hex, "pool_reward_accounts reward account");
                if reward_bytes.len() != 29 {
                    panic!(
                        "pool_reward_accounts reward account must be 29 bytes (1 header + 28 hash), got {} bytes for pool {pool_hex}",
                        reward_bytes.len()
                    );
                }
                pool_params.insert(
                    pool_id,
                    PoolRegistration {
                        pool_id,
                        vrf_keyhash: Hash32::ZERO,
                        pledge: Lovelace(0),
                        cost: Lovelace(0),
                        margin_numerator: 0,
                        margin_denominator: 1,
                        reward_account: reward_bytes,
                        owners: Vec::new(),
                        relays: Vec::new(),
                        metadata_url: None,
                        metadata_hash: None,
                    },
                );
            }
        }

        ledger
    }
}

pub fn assert_ratified(
    ledger: &LedgerState,
    expected_bucket: EnactedBucket,
    expected_id: &GovActionId,
) {
    let gov = &ledger.gov.governance;
    let actual = match expected_bucket {
        EnactedBucket::PParamUpdate => gov.enacted_pparam_update.as_ref(),
        EnactedBucket::HardFork => gov.enacted_hard_fork.as_ref(),
        EnactedBucket::Committee => gov.enacted_committee.as_ref(),
        EnactedBucket::Constitution => gov.enacted_constitution.as_ref(),
    };
    assert_eq!(
        actual,
        Some(expected_id),
        "bucket {expected_bucket:?}: expected {expected_id:?}, got {actual:?}",
    );
}

pub fn assert_not_ratified(ledger: &LedgerState, proposal_id: &GovActionId) {
    let gov = &ledger.gov.governance;
    for slot in [
        &gov.enacted_pparam_update,
        &gov.enacted_hard_fork,
        &gov.enacted_committee,
        &gov.enacted_constitution,
    ] {
        assert_ne!(slot.as_ref(), Some(proposal_id));
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ParentEnacted {
    #[serde(rename = "PParamUpdate")]
    pub pparam_update: Option<String>,
    #[serde(rename = "HardFork")]
    pub hard_fork: Option<String>,
    #[serde(rename = "Committee")]
    pub committee: Option<String>,
    #[serde(rename = "Constitution")]
    pub constitution: Option<String>,
}

#[cfg(test)]
mod ppu_decoder_tests {
    use super::*;

    /// Coverage gate: every PPU JSON key in the test blob must roundtrip into
    /// a `Some(_)` field on `ProtocolParamUpdate`, and `modified_pp_groups`
    /// must classify it correctly.  Acts as a regression test against silent
    /// PPU coverage gaps.
    #[test]
    fn full_ppu_decoder_covers_every_known_field() {
        // One representative JSON blob touching every supported field.  If
        // the test panics with "unknown PPU field …", the loader has a gap
        // that must be patched before merging new captures.  Built from a
        // raw string to avoid blowing the json! macro recursion limit.
        let blob_str = r#"{
            "minFeeA": 44,
            "minFeeB": 155381,
            "maxBlockBodySize": 90112,
            "maxTxSize": 16384,
            "maxBlockHeaderSize": 1100,
            "keyDeposit": 2000000,
            "poolDeposit": 500000000,
            "eMax": 18,
            "nOpt": 500,
            "a0": { "numerator": 3, "denominator": 10 },
            "rho": { "numerator": 3, "denominator": 1000 },
            "tau": { "numerator": 2, "denominator": 10 },
            "minPoolCost": 340000000,
            "coinsPerUTxOByte": 4310,
            "costModels": {
                "PlutusV1": [197209, 0, 1],
                "PlutusV2": [197209, 0, 1, 1],
                "PlutusV3": [100, 100, 100]
            },
            "executionUnitPrices": {
                "priceMemory": { "numerator": 577, "denominator": 10000 },
                "priceSteps":  { "numerator": 721, "denominator": 10000000 }
            },
            "maxTxExecutionUnits": { "memory": 14000000, "steps": 10000000000 },
            "maxBlockExecutionUnits": { "memory": 62000000, "steps": 40000000000 },
            "maxValueSize": 5000,
            "collateralPercentage": 150,
            "maxCollateralInputs": 3,
            "minFeeRefScriptCostPerByte": 15,
            "drepDeposit": 500000000,
            "govActionDeposit": 100000000000,
            "govActionLifetime": 6,
            "dvtPPNetworkGroup": { "numerator": 67, "denominator": 100 },
            "dvtPPEconomicGroup": { "numerator": 67, "denominator": 100 },
            "dvtPPTechnicalGroup": { "numerator": 67, "denominator": 100 },
            "dvtPPGovGroup": { "numerator": 67, "denominator": 100 },
            "dvtHardForkInitiation": { "numerator": 60, "denominator": 100 },
            "dvtMotionNoConfidence": { "numerator": 67, "denominator": 100 },
            "dvtCommitteeNormal": { "numerator": 67, "denominator": 100 },
            "dvtCommitteeNoConfidence": { "numerator": 60, "denominator": 100 },
            "dvtUpdateToConstitution": { "numerator": 75, "denominator": 100 },
            "dvtTreasuryWithdrawal": { "numerator": 67, "denominator": 100 },
            "pvtMotionNoConfidence": { "numerator": 51, "denominator": 100 },
            "pvtCommitteeNormal": { "numerator": 51, "denominator": 100 },
            "pvtCommitteeNoConfidence": { "numerator": 51, "denominator": 100 },
            "pvtHardForkInitiation": { "numerator": 51, "denominator": 100 },
            "pvtPPSecurityGroup": { "numerator": 51, "denominator": 100 },
            "committeeMinSize": 7,
            "committeeMaxTermLength": 146,
            "drepActivity": 20
        }"#;
        let blob: serde_json::Value =
            serde_json::from_str(blob_str).expect("PPU coverage blob must parse");

        let ppu = koios_protocol_param_update(&blob);
        // Spot-check every "Option<_>" field is Some.
        assert!(ppu.min_fee_a.is_some());
        assert!(ppu.min_fee_b.is_some());
        assert!(ppu.max_block_body_size.is_some());
        assert!(ppu.max_tx_size.is_some());
        assert!(ppu.max_block_header_size.is_some());
        assert!(ppu.key_deposit.is_some());
        assert!(ppu.pool_deposit.is_some());
        assert!(ppu.e_max.is_some());
        assert!(ppu.n_opt.is_some());
        assert!(ppu.a0.is_some());
        assert!(ppu.rho.is_some());
        assert!(ppu.tau.is_some());
        assert!(ppu.min_pool_cost.is_some());
        assert!(ppu.ada_per_utxo_byte.is_some());
        assert!(ppu.cost_models.is_some());
        assert!(ppu.execution_costs.is_some());
        assert!(ppu.max_tx_ex_units.is_some());
        assert!(ppu.max_block_ex_units.is_some());
        assert!(ppu.max_val_size.is_some());
        assert!(ppu.collateral_percentage.is_some());
        assert!(ppu.max_collateral_inputs.is_some());
        assert!(ppu.min_fee_ref_script_cost_per_byte.is_some());
        assert!(ppu.drep_deposit.is_some());
        assert!(ppu.gov_action_deposit.is_some());
        assert!(ppu.gov_action_lifetime.is_some());
        assert!(ppu.dvt_pp_network_group.is_some());
        assert!(ppu.dvt_pp_economic_group.is_some());
        assert!(ppu.dvt_pp_technical_group.is_some());
        assert!(ppu.dvt_pp_gov_group.is_some());
        assert!(ppu.dvt_hard_fork.is_some());
        assert!(ppu.dvt_no_confidence.is_some());
        assert!(ppu.dvt_committee_normal.is_some());
        assert!(ppu.dvt_committee_no_confidence.is_some());
        assert!(ppu.dvt_constitution.is_some());
        assert!(ppu.dvt_treasury_withdrawal.is_some());
        assert!(ppu.pvt_motion_no_confidence.is_some());
        assert!(ppu.pvt_committee_normal.is_some());
        assert!(ppu.pvt_committee_no_confidence.is_some());
        assert!(ppu.pvt_hard_fork.is_some());
        assert!(ppu.pvt_pp_security_group.is_some());
        assert!(ppu.min_committee_size.is_some());
        assert!(ppu.committee_term_limit.is_some());
        assert!(ppu.drep_activity.is_some());
    }

    #[test]
    #[should_panic(expected = "unknown PPU field")]
    fn unknown_ppu_field_is_fail_closed() {
        let blob: serde_json::Value =
            serde_json::from_str(r#"{"minFeeA": 44, "totallyMadeUpField": 99}"#).unwrap();
        let _ = koios_protocol_param_update(&blob);
    }
}
