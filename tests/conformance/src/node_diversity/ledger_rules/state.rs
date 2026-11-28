//! Building a real [`LedgerState`] from a fixture's `initial_state`.
//!
//! The validation context is NEVER built by hand.  A hand-built [`ValidationContext`] has optional
//! fields that are silently skipped when `None`, and 126 of the 368 fixtures expect `pass`, so a
//! context that forgets a field passes them by construction.  Instead this module populates the
//! ledger's own state (UTxO set, reward accounts, pool registry, DRep registry, committee,
//! proposals, treasury, ...) and the harness asks the ledger for the context with
//! `LedgerState::mempool_validation_context()`, exactly as the node's LocalTxSubmission path does.
//!
//! Every section of `initial_state` has a writer here and an unknown section is an error, so a
//! section the fixtures gain later cannot be silently ignored.  [`Section`] lists them; the
//! negative control in the gate test drops each in turn.

use std::sync::Arc;

use dugite_consensus::EraHistory;
use dugite_ledger::plutus::SlotConfig;
use dugite_ledger::state::{DRepRegistration, LedgerState, PoolRegistration, ProposalState};
use dugite_primitives::block::{Point, Tip};
use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::network::NetworkId;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use dugite_primitives::transaction::{
    Anchor, Constitution, DRep, GovAction, GovActionId, ProposalProcedure, Rational,
};
use dugite_primitives::value::Lovelace;
use serde_json::Value;

use super::fixture::Fixture;

/// Preprod's system start, POSIX milliseconds (`2022-06-01T00:00:00Z`).  Every fixture is
/// `network: preprod`; the era history the fixtures carry counts time from this instant.
pub const PREPROD_SYSTEM_START_MS: u64 = 1_654_041_600_000;

/// Preprod's Praos security parameter `k`.
pub const PREPROD_SECURITY_PARAM: u64 = 2160;

/// Preprod's network magic.
pub const PREPROD_MAGIC: u64 = 1;

/// A section of `initial_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Utxo,
    Pools,
    Accounts,
    Dreps,
    Committee,
    Proposals,
    ProposalsRoots,
    GovernanceActivity,
    Pots,
    GuardrailScript,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::Utxo,
        Section::Pools,
        Section::Accounts,
        Section::Dreps,
        Section::Committee,
        Section::Proposals,
        Section::ProposalsRoots,
        Section::GovernanceActivity,
        Section::Pots,
        Section::GuardrailScript,
    ];

    /// The key under `initial_state`.
    pub fn key(self) -> &'static str {
        match self {
            Section::Utxo => "utxo",
            Section::Pools => "pools",
            Section::Accounts => "accounts",
            Section::Dreps => "dreps",
            Section::Committee => "committee",
            Section::Proposals => "proposals",
            Section::ProposalsRoots => "proposals_roots",
            Section::GovernanceActivity => "governance_activity",
            Section::Pots => "pots",
            Section::GuardrailScript => "guardrail_script",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// Everything a validation run needs, derived from one fixture.
pub struct Prepared {
    pub ledger: LedgerState,
    pub era_history: EraHistory,
    pub slot_config: SlotConfig,
    /// The slot the transaction is validated at (`point.slot`).
    pub current_slot: u64,
    /// The protocol version the fixture declares (`protocol_parameters.version.major`).
    pub pv_major: u64,
}

fn obj<'a>(v: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    v.as_object().ok_or(format!("{what}: not an object"))
}

fn field<'a>(v: &'a Value, key: &str, what: &str) -> Result<&'a Value, String> {
    v.get(key).ok_or(format!("{what}: missing `{key}`"))
}

fn uint(v: &Value, what: &str) -> Result<u64, String> {
    v.as_u64().ok_or(format!("{what}: not an unsigned integer"))
}

fn text<'a>(v: &'a Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or(format!("{what}: not a string"))
}

fn ratio(v: &Value, what: &str) -> Result<Rational, String> {
    Ok(Rational {
        numerator: uint(field(v, "numerator", what)?, what)?,
        denominator: uint(field(v, "denominator", what)?, what)?,
    })
}

/// Take `key` out of `map`, so that whatever is left over at the end is an unmapped field.
fn take(map: &mut serde_json::Map<String, Value>, key: &str) -> Result<Value, String> {
    map.remove(key)
        .ok_or(format!("protocol_parameters: missing `{key}`"))
}

/// Map the scenario's protocol-parameter document onto dugite's [`ProtocolParameters`].
///
/// Strict in both directions: every key must be consumed (an unmapped key is an error, not a
/// silently ignored parameter), and a key whose value dugite cannot represent (the reference-script
/// fee tiers, which are constants in the Conway ledger) is an error unless it equals the constant.
pub fn protocol_parameters(doc: &Value) -> Result<ProtocolParameters, String> {
    let mut m = obj(doc, "protocol_parameters")?.clone();
    let mut p = ProtocolParameters::mainnet_defaults();

    p.min_fee_a = uint(&take(&mut m, "min_fee_coefficient")?, "min_fee_coefficient")?;
    p.min_fee_b = uint(&take(&mut m, "min_fee_constant")?, "min_fee_constant")?;
    // The reference-script fee schedule: dugite's field is the base price per byte; the 25 KiB
    // tier size and the 1.2x multiplier are constants of the Conway ledger.
    let fees = take(&mut m, "min_fee_reference_scripts")?;
    p.min_fee_ref_script_cost_per_byte =
        ratio(field(&fees, "base", "min_fee_reference_scripts")?, "base")?;
    let range = uint(field(&fees, "range", "min_fee_reference_scripts")?, "range")?;
    let mult = ratio(
        field(&fees, "multiplier", "min_fee_reference_scripts")?,
        "multiplier",
    )?;
    if range != 25_600 || mult.numerator * 5 != mult.denominator * 6 {
        return Err(format!(
            "min_fee_reference_scripts range {range} / multiplier {}/{} is not the Conway constant (25600, 6/5); \
             dugite cannot represent it",
            mult.numerator, mult.denominator
        ));
    }
    p.ada_per_utxo_byte = Lovelace(uint(
        &take(&mut m, "min_utxo_deposit_coefficient")?,
        "min_utxo_deposit_coefficient",
    )?);
    if let Some(constant) = m.remove("min_utxo_deposit_constant") {
        if uint(&constant, "min_utxo_deposit_constant")? != 0 {
            return Err("min_utxo_deposit_constant is not 0; dugite has no such parameter".into());
        }
    }
    p.max_block_body_size = uint(&take(&mut m, "max_block_body_size")?, "max_block_body_size")?;
    p.max_block_header_size = uint(
        &take(&mut m, "max_block_header_size")?,
        "max_block_header_size",
    )?;
    p.max_tx_size = uint(
        &take(&mut m, "max_transaction_size")?,
        "max_transaction_size",
    )?;
    p.max_val_size = uint(&take(&mut m, "max_value_size")?, "max_value_size")?;
    let max_ref = uint(
        &take(&mut m, "max_reference_scripts_size")?,
        "max_reference_scripts_size",
    )?;
    if max_ref != 204_800 {
        return Err(format!(
            "max_reference_scripts_size {max_ref} is not the Conway constant 204800; dugite cannot represent it"
        ));
    }
    p.key_deposit = Lovelace(uint(
        &take(&mut m, "stake_credential_deposit")?,
        "stake_credential_deposit",
    )?);
    p.pool_deposit = Lovelace(uint(
        &take(&mut m, "stake_pool_deposit")?,
        "stake_pool_deposit",
    )?);
    p.e_max = uint(
        &take(&mut m, "stake_pool_retirement_epoch_bound")?,
        "stake_pool_retirement_epoch_bound",
    )?;
    p.a0 = ratio(
        &take(&mut m, "stake_pool_pledge_influence")?,
        "stake_pool_pledge_influence",
    )?;
    p.min_pool_cost = Lovelace(uint(
        &take(&mut m, "min_stake_pool_cost")?,
        "min_stake_pool_cost",
    )?);
    p.n_opt = uint(
        &take(&mut m, "desired_number_of_stake_pools")?,
        "desired_number_of_stake_pools",
    )?;
    p.rho = ratio(&take(&mut m, "monetary_expansion")?, "monetary_expansion")?;
    p.tau = ratio(&take(&mut m, "treasury_expansion")?, "treasury_expansion")?;
    p.collateral_percentage = uint(
        &take(&mut m, "collateral_percentage")?,
        "collateral_percentage",
    )?;
    p.max_collateral_inputs = uint(
        &take(&mut m, "max_collateral_inputs")?,
        "max_collateral_inputs",
    )?;

    let models = take(&mut m, "plutus_cost_models")?;
    let mut models = obj(&models, "plutus_cost_models")?.clone();
    let mut cost_model = |key: &str| -> Result<Option<Vec<i64>>, String> {
        models
            .remove(key)
            .map(|v| {
                v.as_array()
                    .ok_or(format!("{key}: not an array"))?
                    .iter()
                    .map(|n| n.as_i64().ok_or(format!("{key}: non-integer cost")))
                    .collect()
            })
            .transpose()
    };
    p.cost_models.plutus_v1 = cost_model("plutus_v1")?;
    p.cost_models.plutus_v2 = cost_model("plutus_v2")?;
    p.cost_models.plutus_v3 = cost_model("plutus_v3")?;
    if let Some(extra) = models.keys().next() {
        return Err(format!("plutus_cost_models: unmapped key `{extra}`"));
    }

    let prices = take(&mut m, "script_execution_prices")?;
    p.execution_costs.mem_price = ratio(
        field(&prices, "mem_price", "script_execution_prices")?,
        "mem_price",
    )?;
    p.execution_costs.step_price = ratio(
        field(&prices, "step_price", "script_execution_prices")?,
        "step_price",
    )?;
    let ex = |v: Value, what: &str| -> Result<dugite_primitives::transaction::ExUnits, String> {
        Ok(dugite_primitives::transaction::ExUnits {
            mem: uint(field(&v, "mem", what)?, what)?,
            steps: uint(field(&v, "steps", what)?, what)?,
        })
    };
    p.max_tx_ex_units = ex(
        take(&mut m, "max_execution_units_per_transaction")?,
        "max_execution_units_per_transaction",
    )?;
    p.max_block_ex_units = ex(
        take(&mut m, "max_execution_units_per_block")?,
        "max_execution_units_per_block",
    )?;

    let mut spo = obj(
        &take(&mut m, "stake_pool_voting_thresholds")?,
        "stake_pool_voting_thresholds",
    )?
    .clone();
    let r = |map: &mut serde_json::Map<String, Value>, k: &str| -> Result<Rational, String> {
        ratio(
            &map.remove(k)
                .ok_or(format!("voting thresholds: missing `{k}`"))?,
            k,
        )
    };
    p.pvt_motion_no_confidence = r(&mut spo, "motion_no_confidence")?;
    p.pvt_committee_normal = r(&mut spo, "committee_normal")?;
    p.pvt_committee_no_confidence = r(&mut spo, "committee_no_confidence")?;
    p.pvt_hard_fork = r(&mut spo, "hard_fork_initiation")?;
    p.pvt_pp_security_group = r(&mut spo, "security_voting_threshold")?;
    if let Some(extra) = spo.keys().next() {
        return Err(format!(
            "stake_pool_voting_thresholds: unmapped key `{extra}`"
        ));
    }
    let mut drep = obj(
        &take(&mut m, "delegate_representative_voting_thresholds")?,
        "delegate_representative_voting_thresholds",
    )?
    .clone();
    p.dvt_no_confidence = r(&mut drep, "motion_no_confidence")?;
    p.dvt_committee_normal = r(&mut drep, "committee_normal")?;
    p.dvt_committee_no_confidence = r(&mut drep, "committee_no_confidence")?;
    p.dvt_constitution = r(&mut drep, "update_constitution")?;
    p.dvt_hard_fork = r(&mut drep, "hard_fork_initiation")?;
    p.dvt_pp_network_group = r(&mut drep, "pp_network_group")?;
    p.dvt_pp_economic_group = r(&mut drep, "pp_economic_group")?;
    p.dvt_pp_technical_group = r(&mut drep, "pp_technical_group")?;
    p.dvt_pp_gov_group = r(&mut drep, "pp_governance_group")?;
    p.dvt_treasury_withdrawal = r(&mut drep, "treasury_withdrawal")?;
    if let Some(extra) = drep.keys().next() {
        return Err(format!(
            "delegate_representative_voting_thresholds: unmapped key `{extra}`"
        ));
    }

    p.committee_min_size = uint(
        &take(&mut m, "constitutional_committee_min_size")?,
        "constitutional_committee_min_size",
    )?;
    p.committee_max_term_length = uint(
        &take(&mut m, "constitutional_committee_max_term_length")?,
        "constitutional_committee_max_term_length",
    )?;
    p.gov_action_lifetime = uint(
        &take(&mut m, "governance_action_lifetime")?,
        "governance_action_lifetime",
    )?;
    p.gov_action_deposit = Lovelace(uint(
        &take(&mut m, "governance_action_deposit")?,
        "governance_action_deposit",
    )?);
    p.drep_deposit = Lovelace(uint(
        &take(&mut m, "delegate_representative_deposit")?,
        "delegate_representative_deposit",
    )?);
    p.drep_activity = uint(
        &take(&mut m, "delegate_representative_max_idle_time")?,
        "delegate_representative_max_idle_time",
    )?;
    let version = take(&mut m, "version")?;
    p.protocol_version_major = uint(field(&version, "major", "version")?, "version.major")?;
    p.protocol_version_minor = uint(field(&version, "minor", "version")?, "version.minor")?;

    if let Some(extra) = m.keys().next() {
        return Err(format!("protocol_parameters: unmapped key `{extra}`"));
    }
    Ok(p)
}

/// Build the era history from the fixture's `era_history` document.  Times are seconds in the
/// document and picoseconds in dugite's `Bound`.
pub fn era_history(doc: &Value) -> Result<EraHistory, String> {
    let window = uint(
        field(doc, "stability_window", "era_history")?,
        "stability_window",
    )?;
    let eras = field(doc, "eras", "era_history")?
        .as_array()
        .ok_or("era_history.eras: not an array")?;
    let bound = |b: &Value| -> Result<Value, String> {
        Ok(serde_json::json!({
            "time_pico": u128::from(uint(field(b, "time", "bound")?, "time")?) * 1_000_000_000_000u128,
            "slot": uint(field(b, "slot", "bound")?, "slot")?,
            "epoch": uint(field(b, "epoch", "bound")?, "epoch")?,
        }))
    };
    let mut entries = Vec::new();
    for era in eras {
        let params = field(era, "params", "era")?;
        let end = field(era, "end", "era")?;
        entries.push(serde_json::json!({
            "era": text(field(params, "era_name", "era.params")?, "era_name")?,
            "start": bound(field(era, "start", "era")?)?,
            "end": if end.is_null() { Value::Null } else { bound(end)? },
            "params": {
                "epoch_size": uint(field(params, "epoch_size_slots", "era.params")?, "epoch_size_slots")?,
                "slot_length_ms": uint(field(params, "slot_length", "era.params")?, "slot_length")?,
                "safe_zone": window,
                "genesis_window": window,
            },
        }));
    }
    serde_json::from_value(serde_json::json!({ "entries": entries }))
        .map_err(|e| format!("era_history: {e}"))
}

// ── credentials and hashes, as CBOR hex ─────────────────────────────────────────────────────

fn hash28(bytes: &[u8], what: &str) -> Result<Hash28, String> {
    let arr: [u8; 28] = bytes
        .try_into()
        .map_err(|_| format!("{what}: expected 28 bytes, got {}", bytes.len()))?;
    Ok(Hash28::from_bytes(arr))
}

/// `[0, bytes(28)]` is a key credential, `[1, bytes(28)]` a script credential.
fn credential_cbor(hex_cbor: &str, what: &str) -> Result<Credential, String> {
    let bytes = hex::decode(hex_cbor).map_err(|e| format!("{what}: {e}"))?;
    let mut d = minicbor::Decoder::new(&bytes);
    let len = d.array().map_err(|e| format!("{what}: {e}"))?;
    if len != Some(2) {
        return Err(format!("{what}: not a 2-element array"));
    }
    let kind = d.u8().map_err(|e| format!("{what}: {e}"))?;
    let hash = hash28(d.bytes().map_err(|e| format!("{what}: {e}"))?, what)?;
    match kind {
        0 => Ok(Credential::VerificationKey(hash)),
        1 => Ok(Credential::Script(hash)),
        n => Err(format!("{what}: credential kind {n}")),
    }
}

fn drep_cbor(hex_cbor: &str, what: &str) -> Result<DRep, String> {
    let bytes = hex::decode(hex_cbor).map_err(|e| format!("{what}: {e}"))?;
    let mut d = minicbor::Decoder::new(&bytes);
    let len = d.array().map_err(|e| format!("{what}: {e}"))?;
    let kind = d.u8().map_err(|e| format!("{what}: {e}"))?;
    match (kind, len) {
        (0, Some(2)) => Ok(DRep::KeyHash(
            hash28(d.bytes().map_err(|e| format!("{what}: {e}"))?, what)?.to_hash32_padded(),
        )),
        (1, Some(2)) => Ok(DRep::ScriptHash(hash28(
            d.bytes().map_err(|e| format!("{what}: {e}"))?,
            what,
        )?)),
        (2, Some(1)) => Ok(DRep::Abstain),
        (3, Some(1)) => Ok(DRep::NoConfidence),
        _ => Err(format!("{what}: not a DRep")),
    }
}

fn gov_action_id(v: &Value, what: &str) -> Result<GovActionId, String> {
    let id = hex::decode(text(field(v, "transaction_id", what)?, what)?)
        .map_err(|e| format!("{what}: {e}"))?;
    let arr: [u8; 32] = id
        .as_slice()
        .try_into()
        .map_err(|_| format!("{what}: transaction_id is not 32 bytes"))?;
    Ok(GovActionId {
        transaction_id: Hash32::from_bytes(arr),
        action_index: u32::try_from(uint(field(v, "proposal_index", what)?, what)?)
            .map_err(|_| format!("{what}: proposal_index too large"))?,
    })
}

fn dummy_anchor() -> Anchor {
    Anchor {
        url: "https://example.invalid/fixture".into(),
        data_hash: Hash32::from_bytes([0u8; 32]),
    }
}

/// A governance action of the `ProposalKind` the fixture names.  The kind says which lineage a
/// proposal chains onto (and so which transactions may follow it); no content beyond that is
/// specified, so the action carries none.
fn gov_action(kind: &str) -> Result<GovAction, String> {
    Ok(match kind {
        "ProtocolParameters" => GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::default(),
            policy_hash: None,
        },
        "ProtocolParameters(security-group)" => GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(dugite_primitives::transaction::ProtocolParamUpdate {
                max_block_body_size: Some(90_112),
                ..Default::default()
            }),
            policy_hash: None,
        },
        "ConstitutionalCommittee" => GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: Vec::new(),
            members_to_add: Default::default(),
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        },
        "Constitution" => GovAction::NewConstitution {
            prev_action_id: None,
            constitution: Constitution {
                anchor: dummy_anchor(),
                script_hash: None,
            },
        },
        "TreasuryWithdrawals" => GovAction::TreasuryWithdrawals {
            withdrawals: Default::default(),
            policy_hash: None,
        },
        // `Orphan` names a proposal no other action chains onto; its content is irrelevant to
        // the fixtures that use it (a vote only needs the action to exist), and an information
        // action is the one with no lineage.
        "Orphan" | "Information" => GovAction::InfoAction,
        other => {
            let version = other
                .strip_prefix("HardFork(")
                .and_then(|r| r.strip_suffix(')'))
                .and_then(|v| v.split_once('.'))
                .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)))
                .ok_or(format!("unknown proposal kind `{other}`"))?;
            GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: version,
            }
        }
    })
}

/// Rewrite an indefinite-length OUTER map (`0xbf ... 0xff`) as a definite-length one, leaving
/// every nested item byte for byte as it was (a nested Plutus-data list must keep its encoding:
/// datum hashes are taken over it).  Anything else is returned unchanged.
pub fn definite_outer_map(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.first() != Some(&0xbf) {
        return Ok(bytes.to_vec());
    }
    let mut d = minicbor::Decoder::new(bytes);
    d.map().map_err(|e| e.to_string())?;
    let body_start = d.position();
    let mut pairs = 0u64;
    while d.datatype().map_err(|e| e.to_string())? != minicbor::data::Type::Break {
        d.skip().map_err(|e| e.to_string())?;
        d.skip().map_err(|e| e.to_string())?;
        pairs += 1;
    }
    let body_end = d.position();
    let mut out = Vec::with_capacity(bytes.len());
    let mut enc = minicbor::Encoder::new(&mut out);
    enc.map(pairs).map_err(|e| e.to_string())?;
    out.extend_from_slice(&bytes[body_start..body_end]);
    Ok(out)
}

/// Build the ledger, era history and slot configuration for `fixture`.  `skip` leaves one
/// `initial_state` section out, for the negative control.
pub fn build(fixture: &Fixture, skip: Option<Section>) -> Result<Prepared, String> {
    let params = protocol_parameters(&fixture.protocol_parameters)?;
    let pv_major = params.protocol_version_major;
    let history = era_history(&fixture.era_history)?;
    if fixture.network != "preprod" {
        return Err(format!(
            "network `{}`: only preprod is wired",
            fixture.network
        ));
    }
    let epoch_size = history
        .entries()
        .last()
        .map(|e| e.params.epoch_size)
        .ok_or("empty era history")?;
    let window = history.entries()[0].params.safe_zone;

    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;
    ledger.era = dugite_primitives::era::Era::Conway;
    ledger.epoch = EpochNo(fixture.slot / epoch_size);
    ledger.epoch_length = epoch_size;
    ledger.shelley_transition_epoch = 0;
    ledger.node_network = Some(NetworkId::Testnet);
    ledger.network_magic = PREPROD_MAGIC;
    ledger.security_param = PREPROD_SECURITY_PARAM;
    ledger.stability_window_3kf = window;
    ledger.tip = Tip {
        point: Point::Specific(SlotNo(fixture.slot), Hash32::from_bytes([0u8; 32])),
        block_number: BlockNo(0),
    };
    let slot_config = SlotConfig {
        zero_time: PREPROD_SYSTEM_START_MS,
        zero_slot: 0,
        slot_length: 1000,
        safe_zone_horizon_slot: None,
    };
    ledger.slot_config = slot_config;

    // `guardrail_script`: absent or null means the enacted constitution has NO guardrail, which is
    // still a constitution (a proposal that carries a policy must then be rejected), so the
    // constitution is always present unless the section is the one being dropped.
    let mut guardrail: Option<dugite_primitives::hash::Hash28> = None;

    for (key, value) in &fixture.initial_state {
        let section =
            Section::from_key(key).ok_or(format!("unknown initial_state section `{key}`"))?;
        if skip == Some(section) {
            continue;
        }
        apply_section(&mut ledger, section, value, &mut guardrail, &fixture.id)?;
    }
    if skip != Some(Section::GuardrailScript) {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.constitution = Some(Constitution {
            anchor: dummy_anchor(),
            script_hash: guardrail,
        });
    }

    Ok(Prepared {
        ledger,
        era_history: history,
        slot_config,
        current_slot: fixture.slot,
        pv_major,
    })
}

fn apply_section(
    ledger: &mut LedgerState,
    section: Section,
    value: &Value,
    guardrail: &mut Option<Hash28>,
    id: &str,
) -> Result<(), String> {
    let what = format!("{id}: initial_state.{}", section.key());
    let list = |v: &Value| -> Result<Vec<Value>, String> {
        v.as_array().cloned().ok_or(format!("{what}: not an array"))
    };
    match section {
        Section::Utxo => {
            for (i, entry) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let input = hex::decode(text(field(entry, "input", &w)?, &w)?)
                    .map_err(|e| format!("{w}: {e}"))?;
                let output = hex::decode(text(field(entry, "output", &w)?, &w)?)
                    .map_err(|e| format!("{w}: {e}"))?;
                let input = dugite_serialization::decode_transaction_input(&input)
                    .map_err(|e| format!("{w}.input: {e}"))?;
                // Era 6 is Conway: an output is decoded with the ledger's own decoder, after
                // its OUTER indefinite-length map header (125 of the fixtures' state outputs
                // use one, as cardano-node accepts) is made definite.  This normalises state
                // only: the transaction under test is never touched, so the decoder's refusal
                // of the same shape inside a transaction stays visible as a result.
                let output = definite_outer_map(&output).map_err(|e| format!("{w}.output: {e}"))?;
                let output = dugite_serialization::decode_transaction_output(6, &output)
                    .map_err(|e| format!("{w}.output: {e}"))?;
                ledger.utxo.utxo_set.insert(input, output);
            }
        }
        Section::Pools => {
            let mut registry = (*ledger.certs.pool_params).clone();
            for (i, pool) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let raw = hex::decode(text(pool, &w)?).map_err(|e| format!("{w}: {e}"))?;
                let pool_id = hash28(&raw, &w)?;
                // The fixture names a registered pool and nothing else about it.  A VRF key hash
                // distinct per pool keeps the key registry honest.
                let mut vrf = [0u8; 32];
                vrf[..28].copy_from_slice(pool_id.as_bytes());
                registry.insert(
                    pool_id,
                    PoolRegistration {
                        pool_id,
                        vrf_keyhash: Hash32::from_bytes(vrf),
                        pledge: Lovelace(0),
                        cost: Lovelace(0),
                        margin_numerator: 0,
                        margin_denominator: 1,
                        reward_account: Vec::new(),
                        owners: Vec::new(),
                        relays: Vec::new(),
                        metadata_url: None,
                        metadata_hash: None,
                    },
                );
                ledger
                    .certs
                    .vrf_key_hashes
                    .insert(Hash32::from_bytes(vrf), 1);
                ledger
                    .certs
                    .pool_deposits
                    .insert(pool_id, ledger.epochs.protocol_params.pool_deposit.0);
            }
            ledger.certs.pool_params = Arc::new(registry);
        }
        Section::Accounts => {
            for (i, acct) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let cred = credential_cbor(text(field(acct, "credential", &w)?, &w)?, &w)?;
                let key = cred.to_typed_hash32();
                let deposit = uint(field(acct, "deposit", &w)?, &w)?;
                let rewards = acct
                    .get("rewards")
                    .map(|r| uint(r, &w))
                    .transpose()?
                    .unwrap_or(0);
                ledger.certs.reward_accounts.insert(key, Lovelace(rewards));
                ledger.certs.stake_key_deposits.insert(key, deposit);
                ledger.certs.total_stake_key_deposits += deposit;
                if let Credential::Script(_) = cred {
                    ledger.certs.script_stake_credentials.insert(key);
                }
                if let Some(pool) = acct.get("pool") {
                    let raw = hex::decode(text(field(pool, "id", &w)?, &w)?)
                        .map_err(|e| format!("{w}.pool: {e}"))?;
                    ledger.certs.delegations.insert(key, hash28(&raw, &w)?);
                }
                if let Some(drep) = acct.get("drep") {
                    let drep = drep_cbor(text(field(drep, "id", &w)?, &w)?, &w)?;
                    let gov = Arc::make_mut(&mut ledger.gov.governance);
                    if let Some(drep_key) = drep.credential_hash32() {
                        if let Some(reg) = gov.dreps.get_mut(&drep_key) {
                            reg.delegs.insert(key);
                        }
                    }
                    gov.vote_delegations.insert(key, drep);
                }
            }
        }
        Section::Dreps => {
            let epoch = ledger.epoch;
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            for (i, d) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let cred = credential_cbor(text(field(d, "credential", &w)?, &w)?, &w)?;
                let key = cred.to_typed_hash32();
                gov.dreps.insert(
                    key,
                    DRepRegistration {
                        credential: cred,
                        deposit: Lovelace(uint(field(d, "deposit", &w)?, &w)?),
                        anchor: None,
                        registered_epoch: epoch,
                        drep_expiry: EpochNo(uint(field(d, "valid_until", &w)?, &w)?),
                        active: true,
                        delegs: Default::default(),
                    },
                );
                gov.drep_registration_count += 1;
            }
        }
        Section::Committee => {
            let epoch = ledger.epoch;
            let max_term = ledger.epochs.protocol_params.committee_max_term_length;
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            for (i, m) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let cold = credential_cbor(text(field(m, "cold_credential", &w)?, &w)?, &w)?;
                let cold_key = cold.to_typed_hash32();
                if let Credential::Script(_) = cold {
                    gov.script_committee_credentials.insert(cold_key);
                }
                // `valid_until` is the member's term in the ENACTED committee
                // (`committeeMembers`: cold credential -> epoch); `status` is its entry in the
                // committee AUTHORIZATION state (`csCommitteeCreds`: hot credential or resigned).
                // A member with a `status` and no term is therefore authorised but unelected
                // (fixtures 00171, 00217).  An entry with NEITHER is a bare committee member the
                // fixture says may authorise a hot key (00211, expected `Pass`), which GOVCERT
                // only allows for a current member, so it is seated with the longest term the
                // parameters allow.
                match (m.get("valid_until"), m.get("status")) {
                    (Some(until), _) => {
                        gov.committee_expiration
                            .insert(cold_key, EpochNo(uint(until, &w)?));
                    }
                    (None, None) => {
                        gov.committee_expiration
                            .insert(cold_key, EpochNo(epoch.0 + max_term));
                    }
                    (None, Some(_)) => {}
                }
                match m.get("status") {
                    None => {}
                    Some(Value::String(s)) if s == "resigned" => {
                        gov.committee_resigned.insert(cold_key, None);
                    }
                    Some(Value::String(hot)) => {
                        let hot = credential_cbor(hot, &w)?;
                        let hot_key = hot.to_typed_hash32();
                        if let Credential::Script(_) = hot {
                            gov.script_committee_hot_credentials.insert(hot_key);
                        }
                        gov.committee_hot_keys.insert(cold_key, hot_key);
                    }
                    Some(_) => {
                        return Err(format!(
                            "{w}.status: neither a hot credential nor `resigned`"
                        ))
                    }
                }
            }
        }
        Section::Proposals => {
            let lifetime = ledger.epochs.protocol_params.gov_action_lifetime;
            let deposit = ledger.epochs.protocol_params.gov_action_deposit;
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            for (i, p) in list(value)?.iter().enumerate() {
                let w = format!("{what}[{i}]");
                let items = p.as_array().ok_or(format!("{w}: not an array"))?;
                if items.len() != 3 {
                    return Err(format!("{w}: not a 3-element array"));
                }
                let gid = gov_action_id(&items[0], &w)?;
                let action = gov_action(text(&items[1], &w)?)?;
                let expires = uint(&items[2], &w)?;
                let state = ProposalState {
                    procedure: ProposalProcedure {
                        deposit,
                        // A reward account on the fixture's network; the fixtures say nothing
                        // about it.
                        return_addr: [&[0xe0u8][..], &[0x11u8; 28][..]].concat(),
                        gov_action: action,
                        anchor: dummy_anchor(),
                    },
                    proposed_epoch: EpochNo(expires.saturating_sub(lifetime)),
                    expires_epoch: EpochNo(expires),
                    yes_votes: 0,
                    no_votes: 0,
                    abstain_votes: 0,
                    submission_index: gov.proposal_count,
                };
                gov.proposal_count += 1;
                gov.proposals.insert(gid, state);
            }
        }
        Section::ProposalsRoots => {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            for (key, v) in obj(value, &what)? {
                let gid = gov_action_id(v, &what)?;
                match key.as_str() {
                    "protocol_parameters" => {
                        gov.proposal_roots.pparam.root = Some(gid.clone());
                        gov.enacted_pparam_update = Some(gid);
                    }
                    "hard_fork" => {
                        gov.proposal_roots.hard_fork.root = Some(gid.clone());
                        gov.enacted_hard_fork = Some(gid);
                    }
                    "constitutional_committee" => {
                        gov.proposal_roots.committee.root = Some(gid.clone());
                        gov.enacted_committee = Some(gid);
                    }
                    "constitution" => {
                        gov.proposal_roots.constitution.root = Some(gid.clone());
                        gov.enacted_constitution = Some(gid);
                    }
                    other => return Err(format!("{what}: unknown root `{other}`")),
                }
            }
        }
        Section::GovernanceActivity => {
            let gov = Arc::make_mut(&mut ledger.gov.governance);
            gov.num_dormant_epochs =
                uint(field(value, "consecutive_dormant_epochs", &what)?, &what)?;
        }
        Section::Pots => {
            for (key, v) in obj(value, &what)? {
                match key.as_str() {
                    "treasury" => ledger.epochs.treasury = Lovelace(uint(v, &what)?),
                    "reserves" => ledger.epochs.reserves = Lovelace(uint(v, &what)?),
                    other => return Err(format!("{what}: unknown pot `{other}`")),
                }
            }
        }
        Section::GuardrailScript => {
            if !value.is_null() {
                let raw = hex::decode(text(value, &what)?).map_err(|e| format!("{what}: {e}"))?;
                *guardrail = Some(hash28(&raw, &what)?);
            }
        }
    }
    Ok(())
}
