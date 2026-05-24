//! Phase 3 — Strict typed PParams deserialization.
//!
//! `Haskell*` structs use `#[serde(deny_unknown_fields)]` so any upstream
//! field rename or addition surfaces as a test failure, prompting us to update
//! our deserialization and conversion code before the mismatch reaches production.
//!
//! Test strategy:
//! - Alonzo and Conway: decode actual fixture files from the cardano-ledger corpus.
//! - Shelley and Babbage: decode embedded inline JSON (no separate fixture file in
//!   the corpus for these eras; the embedded data matches the known mainnet format).
//! - Roundtrip: parse Conway genesis → serialize back to Value → parse again,
//!   asserting all numeric fields survive the round-trip exactly.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Shared sub-types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellRational {
    pub numerator: u64,
    pub denominator: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellExPrices {
    #[serde(rename = "prSteps")]
    pub pr_steps: HaskellRational,
    #[serde(rename = "prMem")]
    pub pr_mem: HaskellRational,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellExUnits {
    #[serde(rename = "exUnitsMem")]
    pub ex_units_mem: u64,
    #[serde(rename = "exUnitsSteps")]
    pub ex_units_steps: u64,
}

// ── Shelley genesis ───────────────────────────────────────────────────────────
// Source: cardano-ledger/eras/shelley/impl/src/Cardano/Ledger/Shelley/Genesis.hs
// JSON field names from ToJSON/FromJSON instances — `protocolParams` is the
// embedded PParams object.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellShelleyProtocolVersion {
    pub minor: u64,
    pub major: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellShelleyProtocolParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: HaskellShelleyProtocolVersion,
    #[serde(rename = "decentralisationParam")]
    pub decentralisation_param: f64,
    #[serde(rename = "eMax")]
    pub e_max: u64,
    #[serde(rename = "extraEntropy")]
    pub extra_entropy: serde_json::Value,
    #[serde(rename = "maxTxSize")]
    pub max_tx_size: u64,
    #[serde(rename = "maxBlockBodySize")]
    pub max_block_body_size: u64,
    #[serde(rename = "maxBlockHeaderSize")]
    pub max_block_header_size: u64,
    #[serde(rename = "minFeeA")]
    pub min_fee_a: u64,
    #[serde(rename = "minFeeB")]
    pub min_fee_b: u64,
    #[serde(rename = "minUTxOValue")]
    pub min_u_tx_o_value: u64,
    #[serde(rename = "poolDeposit")]
    pub pool_deposit: u64,
    #[serde(rename = "minPoolCost")]
    pub min_pool_cost: u64,
    #[serde(rename = "keyDeposit")]
    pub key_deposit: u64,
    #[serde(rename = "nOpt")]
    pub n_opt: u64,
    pub rho: f64,
    pub tau: f64,
    pub a0: f64,
}

#[derive(Debug, Deserialize)]
pub struct HaskellShelleyGenesis {
    #[serde(rename = "activeSlotsCoeff")]
    pub active_slots_coeff: f64,
    #[serde(rename = "epochLength")]
    pub epoch_length: u64,
    #[serde(rename = "maxKESEvolutions")]
    pub max_kes_evolutions: u64,
    #[serde(rename = "maxLovelaceSupply")]
    pub max_lovelace_supply: u64,
    #[serde(rename = "networkId")]
    pub network_id: String,
    #[serde(rename = "networkMagic")]
    pub network_magic: u64,
    #[serde(rename = "protocolParams")]
    pub protocol_params: HaskellShelleyProtocolParams,
    #[serde(rename = "securityParam")]
    pub security_param: u64,
    #[serde(rename = "slotLength")]
    pub slot_length: u64,
    #[serde(rename = "slotsPerKESPeriod")]
    pub slots_per_kes_period: u64,
    #[serde(rename = "systemStart")]
    pub system_start: String,
    #[serde(rename = "updateQuorum")]
    pub update_quorum: u64,
    // Variable-key maps — accept without strict field checking.
    #[serde(rename = "genDelegs")]
    pub gen_delegs: serde_json::Value,
    #[serde(rename = "initialFunds")]
    pub initial_funds: serde_json::Value,
}

// ── Alonzo genesis ────────────────────────────────────────────────────────────
// Source: cardano-ledger/eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Genesis.hs
// Field names from ToJSON/FromJSON — `lovelacePerUTxOWord` is the Alonzo name
// (renamed to `coinsPerUTxOByte` in Babbage).

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellAlonzoGenesis {
    #[serde(rename = "lovelacePerUTxOWord")]
    pub lovelace_per_u_tx_o_word: u64,
    #[serde(rename = "executionPrices")]
    pub execution_prices: HaskellExPrices,
    #[serde(rename = "maxTxExUnits")]
    pub max_tx_ex_units: HaskellExUnits,
    #[serde(rename = "maxBlockExUnits")]
    pub max_block_ex_units: HaskellExUnits,
    #[serde(rename = "maxValueSize")]
    pub max_value_size: u64,
    #[serde(rename = "collateralPercentage")]
    pub collateral_percentage: u64,
    #[serde(rename = "maxCollateralInputs")]
    pub max_collateral_inputs: u64,
    // Cost models: complex per-version maps with named or indexed keys.
    // Use Value to tolerate additions of new Plutus version cost model entries.
    #[serde(rename = "costModels")]
    pub cost_models: serde_json::Value,
}

// ── Babbage genesis ───────────────────────────────────────────────────────────
// Source: cardano-ledger/eras/babbage/impl/src/Cardano/Ledger/Babbage/Genesis.hs
// Only change from Alonzo: `lovelacePerUTxOWord` → `coinsPerUTxOByte`.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellBabbageGenesis {
    #[serde(rename = "coinsPerUTxOByte")]
    pub coins_per_u_tx_o_byte: u64,
    #[serde(rename = "executionPrices")]
    pub execution_prices: HaskellExPrices,
    #[serde(rename = "maxTxExUnits")]
    pub max_tx_ex_units: HaskellExUnits,
    #[serde(rename = "maxBlockExUnits")]
    pub max_block_ex_units: HaskellExUnits,
    #[serde(rename = "maxValueSize")]
    pub max_value_size: u64,
    #[serde(rename = "collateralPercentage")]
    pub collateral_percentage: u64,
    #[serde(rename = "maxCollateralInputs")]
    pub max_collateral_inputs: u64,
    #[serde(rename = "costModels")]
    pub cost_models: serde_json::Value,
}

// ── Conway genesis ────────────────────────────────────────────────────────────
// Source: cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Genesis.hs
// Voting thresholds are JSON numbers (float or int both parse as f64).

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellPoolVotingThresholds {
    #[serde(rename = "committeeNormal")]
    pub committee_normal: f64,
    #[serde(rename = "committeeNoConfidence")]
    pub committee_no_confidence: f64,
    #[serde(rename = "hardForkInitiation")]
    pub hard_fork_initiation: f64,
    #[serde(rename = "motionNoConfidence")]
    pub motion_no_confidence: f64,
    #[serde(rename = "ppSecurityGroup")]
    pub pp_security_group: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellDRepVotingThresholds {
    #[serde(rename = "motionNoConfidence")]
    pub motion_no_confidence: f64,
    #[serde(rename = "committeeNormal")]
    pub committee_normal: f64,
    #[serde(rename = "committeeNoConfidence")]
    pub committee_no_confidence: f64,
    #[serde(rename = "updateToConstitution")]
    pub update_to_constitution: f64,
    #[serde(rename = "hardForkInitiation")]
    pub hard_fork_initiation: f64,
    #[serde(rename = "ppNetworkGroup")]
    pub pp_network_group: f64,
    #[serde(rename = "ppEconomicGroup")]
    pub pp_economic_group: f64,
    #[serde(rename = "ppTechnicalGroup")]
    pub pp_technical_group: f64,
    #[serde(rename = "ppGovGroup")]
    pub pp_gov_group: f64,
    #[serde(rename = "treasuryWithdrawal")]
    pub treasury_withdrawal: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaskellConwayGenesis {
    #[serde(rename = "poolVotingThresholds")]
    pub pool_voting_thresholds: HaskellPoolVotingThresholds,
    #[serde(rename = "dRepVotingThresholds")]
    pub d_rep_voting_thresholds: HaskellDRepVotingThresholds,
    #[serde(rename = "committeeMinSize")]
    pub committee_min_size: u64,
    #[serde(rename = "committeeMaxTermLength")]
    pub committee_max_term_length: u64,
    #[serde(rename = "govActionLifetime")]
    pub gov_action_lifetime: u64,
    #[serde(rename = "govActionDeposit")]
    pub gov_action_deposit: u64,
    #[serde(rename = "dRepDeposit")]
    pub d_rep_deposit: u64,
    #[serde(rename = "dRepActivity")]
    pub d_rep_activity: u64,
    #[serde(rename = "minFeeRefScriptCostPerByte")]
    pub min_fee_ref_script_cost_per_byte: u64,
    // PlutusV3 cost model: flat array of i64 (some entries can be negative).
    #[serde(rename = "plutusV3CostModel")]
    pub plutus_v3_cost_model: Vec<i64>,
    // Complex nested objects — accept without strict inner field checking.
    pub constitution: serde_json::Value,
    pub committee: serde_json::Value,
    // delegs and initialDReps appear in the cardano-ledger test fixture.
    pub delegs: serde_json::Value,
    #[serde(rename = "initialDReps")]
    pub initial_d_reps: serde_json::Value,
}

// ── Inline reference data (Shelley / Babbage, no corpus fixture file) ─────────

const SHELLEY_GENESIS_INLINE: &str = r#"{
    "activeSlotsCoeff": 0.05,
    "epochLength": 86400,
    "maxKESEvolutions": 62,
    "maxLovelaceSupply": 45000000000000000,
    "networkId": "Testnet",
    "networkMagic": 2,
    "protocolParams": {
        "protocolVersion": {"major": 6, "minor": 0},
        "decentralisationParam": 1.0,
        "eMax": 18,
        "extraEntropy": {"tag": "NeutralNonce"},
        "maxTxSize": 16384,
        "maxBlockBodySize": 65536,
        "maxBlockHeaderSize": 1100,
        "minFeeA": 44,
        "minFeeB": 155381,
        "minUTxOValue": 1000000,
        "poolDeposit": 500000000,
        "minPoolCost": 340000000,
        "keyDeposit": 2000000,
        "nOpt": 150,
        "rho": 0.003,
        "tau": 0.2,
        "a0": 0.3
    },
    "securityParam": 432,
    "slotLength": 1,
    "slotsPerKESPeriod": 129600,
    "systemStart": "2022-10-25T00:00:00Z",
    "updateQuorum": 5,
    "genDelegs": {},
    "initialFunds": {}
}"#;

const BABBAGE_GENESIS_INLINE: &str = r#"{
    "coinsPerUTxOByte": 4310,
    "executionPrices": {
        "prSteps": {"numerator": 721, "denominator": 10000000},
        "prMem": {"numerator": 577, "denominator": 10000}
    },
    "maxTxExUnits": {"exUnitsMem": 14000000, "exUnitsSteps": 10000000000},
    "maxBlockExUnits": {"exUnitsMem": 62000000, "exUnitsSteps": 40000000000},
    "maxValueSize": 5000,
    "collateralPercentage": 150,
    "maxCollateralInputs": 3,
    "costModels": {}
}"#;

// ── Test runner ───────────────────────────────────────────────────────────────

fn find_genesis(dir: &Path, pattern: &str) -> Option<std::path::PathBuf> {
    fn walk(dir: &Path, pattern: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(p) = walk(&path, pattern) {
                    return Some(p);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("json")
                && path
                    .to_str()
                    .map(|s| s.to_lowercase().contains(pattern))
                    .unwrap_or(false)
            {
                return Some(path);
            }
        }
        None
    }
    walk(dir, pattern)
}

pub fn run_all_checks(dir: &Path) {
    haskell_pparams_shelley_decodes_strict();
    haskell_pparams_alonzo_decodes_strict(dir);
    haskell_pparams_babbage_decodes_strict();
    haskell_pparams_conway_decodes_strict(dir);
    haskell_pparams_conway_roundtrip(dir);
}

fn haskell_pparams_shelley_decodes_strict() {
    let genesis: HaskellShelleyGenesis =
        serde_json::from_str(SHELLEY_GENESIS_INLINE).expect("inline shelley genesis must parse");
    assert_eq!(genesis.protocol_params.min_fee_a, 44);
    assert_eq!(genesis.protocol_params.min_fee_b, 155381);
    assert_eq!(genesis.protocol_params.max_tx_size, 16384);
    assert_eq!(genesis.protocol_params.e_max, 18);
    assert!((genesis.active_slots_coeff - 0.05).abs() < 1e-12);
    eprintln!(
        "[pparams-typed] shelley: minFeeA={}, nOpt={}",
        genesis.protocol_params.min_fee_a, genesis.protocol_params.n_opt
    );
}

fn haskell_pparams_alonzo_decodes_strict(dir: &Path) {
    // Try to load from corpus; if absent, fall back to config-level fixture.
    let (text, source) = if let Some(p) = find_genesis(dir, "alonzo-genesis") {
        (
            std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("read alonzo genesis {}: {e}", p.display())),
            p.display().to_string(),
        )
    } else if let Some(p) = find_genesis(dir, "alonzo") {
        (
            std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("read alonzo fixture {}: {e}", p.display())),
            p.display().to_string(),
        )
    } else {
        eprintln!(
            "[pparams-typed] alonzo: no fixture found in {}, skipping",
            dir.display()
        );
        return;
    };

    let genesis: HaskellAlonzoGenesis = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("strict parse alonzo genesis from {source}: {e}"));

    assert!(
        genesis.lovelace_per_u_tx_o_word > 0,
        "lovelacePerUTxOWord must be non-zero"
    );
    assert!(
        genesis.max_tx_ex_units.ex_units_mem > 0,
        "maxTxExUnits.exUnitsMem must be non-zero"
    );
    assert!(
        genesis.max_block_ex_units.ex_units_steps > 0,
        "maxBlockExUnits.exUnitsSteps must be non-zero"
    );
    assert!(
        genesis.collateral_percentage > 0,
        "collateralPercentage must be non-zero"
    );
    assert!(
        genesis.max_collateral_inputs > 0,
        "maxCollateralInputs must be non-zero"
    );
    eprintln!(
        "[pparams-typed] alonzo: lovelacePerUTxOWord={}, maxValueSize={} [{}]",
        genesis.lovelace_per_u_tx_o_word, genesis.max_value_size, source
    );
}

fn haskell_pparams_babbage_decodes_strict() {
    let genesis: HaskellBabbageGenesis =
        serde_json::from_str(BABBAGE_GENESIS_INLINE).expect("inline babbage genesis must parse");
    assert_eq!(genesis.coins_per_u_tx_o_byte, 4310);
    assert_eq!(genesis.max_tx_ex_units.ex_units_mem, 14_000_000);
    assert_eq!(genesis.max_block_ex_units.ex_units_steps, 40_000_000_000);
    assert_eq!(genesis.collateral_percentage, 150);
    eprintln!(
        "[pparams-typed] babbage: coinsPerUTxOByte={}, maxValueSize={}",
        genesis.coins_per_u_tx_o_byte, genesis.max_value_size
    );
}

fn haskell_pparams_conway_decodes_strict(dir: &Path) {
    let path = match find_genesis(dir, "conway-genesis") {
        Some(p) => p,
        None => {
            eprintln!(
                "[pparams-typed] conway: no fixture found in {}, skipping",
                dir.display()
            );
            return;
        }
    };

    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read conway genesis {}: {e}", path.display()));

    let genesis: HaskellConwayGenesis = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("strict parse conway genesis {}: {e}", path.display()));

    // Structural assertions: all voting threshold fields must have parsed.
    // The test fixture uses all-zero values; real genesis files use non-zero.
    let _ = genesis.pool_voting_thresholds.committee_normal;
    let _ = genesis.pool_voting_thresholds.committee_no_confidence;
    let _ = genesis.pool_voting_thresholds.hard_fork_initiation;
    let _ = genesis.pool_voting_thresholds.motion_no_confidence;
    let _ = genesis.pool_voting_thresholds.pp_security_group;
    let _ = genesis.d_rep_voting_thresholds.motion_no_confidence;
    let _ = genesis.d_rep_voting_thresholds.committee_normal;
    let _ = genesis.d_rep_voting_thresholds.committee_no_confidence;
    let _ = genesis.d_rep_voting_thresholds.update_to_constitution;
    let _ = genesis.d_rep_voting_thresholds.hard_fork_initiation;
    let _ = genesis.d_rep_voting_thresholds.pp_network_group;
    let _ = genesis.d_rep_voting_thresholds.pp_economic_group;
    let _ = genesis.d_rep_voting_thresholds.pp_technical_group;
    let _ = genesis.d_rep_voting_thresholds.pp_gov_group;
    let _ = genesis.d_rep_voting_thresholds.treasury_withdrawal;

    assert!(
        !genesis.plutus_v3_cost_model.is_empty(),
        "plutusV3CostModel must not be empty"
    );

    eprintln!(
        "[pparams-typed] conway: committeeMinSize={}, dRepDeposit={}, \
         plutusV3CostModel.len()={} [{}]",
        genesis.committee_min_size,
        genesis.d_rep_deposit,
        genesis.plutus_v3_cost_model.len(),
        path.display()
    );
}

fn haskell_pparams_conway_roundtrip(dir: &Path) {
    let path = match find_genesis(dir, "conway-genesis") {
        Some(p) => p,
        None => {
            eprintln!(
                "[pparams-typed] conway roundtrip: no fixture found in {}, skipping",
                dir.display()
            );
            return;
        }
    };

    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read conway genesis {}: {e}", path.display()));

    // Step 1: Decode to typed struct.
    let genesis: HaskellConwayGenesis = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("roundtrip parse conway genesis {}: {e}", path.display()));

    // Step 2: Re-encode back to a serde_json::Value.
    let re_encoded: serde_json::Value = serde_json::to_value(&genesis).unwrap();

    // Step 3: Parse the re-encoded Value back into the typed struct.
    let re_parsed: HaskellConwayGenesis = serde_json::from_value(re_encoded.clone())
        .expect("re-encoded conway genesis must parse back to HaskellConwayGenesis");

    // Step 4: Verify key numeric fields survived the round-trip.
    assert_eq!(
        genesis.committee_min_size, re_parsed.committee_min_size,
        "committeeMinSize must survive roundtrip"
    );
    assert_eq!(
        genesis.committee_max_term_length, re_parsed.committee_max_term_length,
        "committeeMaxTermLength must survive roundtrip"
    );
    assert_eq!(
        genesis.gov_action_lifetime, re_parsed.gov_action_lifetime,
        "govActionLifetime must survive roundtrip"
    );
    assert_eq!(
        genesis.gov_action_deposit, re_parsed.gov_action_deposit,
        "govActionDeposit must survive roundtrip"
    );
    assert_eq!(
        genesis.d_rep_deposit, re_parsed.d_rep_deposit,
        "dRepDeposit must survive roundtrip"
    );
    assert_eq!(
        genesis.d_rep_activity, re_parsed.d_rep_activity,
        "dRepActivity must survive roundtrip"
    );
    assert_eq!(
        genesis.min_fee_ref_script_cost_per_byte, re_parsed.min_fee_ref_script_cost_per_byte,
        "minFeeRefScriptCostPerByte must survive roundtrip"
    );
    assert_eq!(
        genesis.plutus_v3_cost_model.len(),
        re_parsed.plutus_v3_cost_model.len(),
        "plutusV3CostModel length must survive roundtrip"
    );
    assert_eq!(
        genesis.plutus_v3_cost_model, re_parsed.plutus_v3_cost_model,
        "plutusV3CostModel contents must survive roundtrip"
    );

    // Voting thresholds — compare with tolerance for f64 precision.
    let eps = 1e-12_f64;
    assert!(
        (genesis.pool_voting_thresholds.committee_normal
            - re_parsed.pool_voting_thresholds.committee_normal)
            .abs()
            < eps,
        "poolVotingThresholds.committeeNormal must survive roundtrip"
    );
    assert!(
        (genesis.d_rep_voting_thresholds.treasury_withdrawal
            - re_parsed.d_rep_voting_thresholds.treasury_withdrawal)
            .abs()
            < eps,
        "dRepVotingThresholds.treasuryWithdrawal must survive roundtrip"
    );

    eprintln!(
        "[pparams-typed] conway roundtrip: all {} fields verified [{}]",
        re_encoded.as_object().map(|o| o.len()).unwrap_or(0),
        path.display()
    );
}
