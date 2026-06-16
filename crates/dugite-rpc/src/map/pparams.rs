//! `dugite_primitives::ProtocolParameters` → `utxorpc.v1beta.cardano.PParams`.
//!
//! Conway-shape projection. Field mapping is mechanical — every utxorpc
//! PParams field maps from a single dugite field with at most a unit
//! conversion. Fields the spec doesn't define at v1beta v0.19.2 (e.g.
//! Dijkstra-only PV12 additions) are intentionally skipped — they'll
//! land when the spec bumps to expose them.

use crate::map::common::coin_bigint;
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::Rational as DRational;

/// Project a dugite `ProtocolParameters` into the utxorpc `PParams` shape.
pub fn pparams_to_proto(p: &ProtocolParameters) -> pb::PParams {
    pb::PParams {
        coins_per_utxo_byte: Some(coin_bigint(p.ada_per_utxo_byte.0)),
        max_tx_size: p.max_tx_size,
        min_fee_coefficient: Some(coin_bigint(p.min_fee_a)),
        min_fee_constant: Some(coin_bigint(p.min_fee_b)),
        max_block_body_size: p.max_block_body_size,
        max_block_header_size: p.max_block_header_size,
        stake_key_deposit: Some(coin_bigint(p.key_deposit.0)),
        pool_deposit: Some(coin_bigint(p.pool_deposit.0)),
        pool_retirement_epoch_bound: p.e_max,
        desired_number_of_pools: p.n_opt,
        pool_influence: Some(rational_to_proto(&p.a0)),
        monetary_expansion: Some(rational_to_proto(&p.rho)),
        treasury_expansion: Some(rational_to_proto(&p.tau)),
        min_pool_cost: Some(coin_bigint(p.min_pool_cost.0)),
        protocol_version: Some(pb::ProtocolVersion {
            major: p.protocol_version_major as u32,
            minor: p.protocol_version_minor as u32,
        }),
        max_value_size: p.max_val_size,
        collateral_percentage: p.collateral_percentage,
        max_collateral_inputs: p.max_collateral_inputs,
        cost_models: Some(cost_models_to_proto(&p.cost_models)),
        prices: Some(pb::ExPrices {
            steps: Some(rational_to_proto(&p.execution_costs.step_price)),
            memory: Some(rational_to_proto(&p.execution_costs.mem_price)),
        }),
        max_execution_units_per_transaction: Some(pb::ExUnits {
            steps: p.max_tx_ex_units.steps,
            memory: p.max_tx_ex_units.mem,
        }),
        max_execution_units_per_block: Some(pb::ExUnits {
            steps: p.max_block_ex_units.steps,
            memory: p.max_block_ex_units.mem,
        }),
        // minFeeRefScriptCostPerByte is a NonNegativeInterval; map the full
        // rational into the proto RationalNumber.
        min_fee_script_ref_cost_per_byte: Some(pb::RationalNumber {
            numerator: p.min_fee_ref_script_cost_per_byte.numerator as i32,
            denominator: p.min_fee_ref_script_cost_per_byte.denominator as u32,
        }),
        pool_voting_thresholds: Some(pb::VotingThresholds {
            thresholds: vec![
                rational_to_proto(&p.pvt_motion_no_confidence),
                rational_to_proto(&p.pvt_committee_normal),
                rational_to_proto(&p.pvt_committee_no_confidence),
                rational_to_proto(&p.pvt_hard_fork),
                rational_to_proto(&p.pvt_pp_security_group),
            ],
        }),
        drep_voting_thresholds: Some(pb::VotingThresholds {
            thresholds: vec![
                rational_to_proto(&p.dvt_pp_network_group),
                rational_to_proto(&p.dvt_pp_economic_group),
                rational_to_proto(&p.dvt_pp_technical_group),
                rational_to_proto(&p.dvt_pp_gov_group),
                rational_to_proto(&p.dvt_hard_fork),
                rational_to_proto(&p.dvt_no_confidence),
                rational_to_proto(&p.dvt_committee_normal),
                rational_to_proto(&p.dvt_committee_no_confidence),
                rational_to_proto(&p.dvt_constitution),
                rational_to_proto(&p.dvt_treasury_withdrawal),
            ],
        }),
        min_committee_size: p.committee_min_size as u32,
        committee_term_limit: p.committee_max_term_length,
        governance_action_validity_period: p.gov_action_lifetime,
        governance_action_deposit: Some(coin_bigint(p.gov_action_deposit.0)),
        drep_deposit: Some(coin_bigint(p.drep_deposit.0)),
        drep_inactivity_period: p.drep_activity,
    }
}

fn rational_to_proto(r: &DRational) -> pb::RationalNumber {
    pb::RationalNumber {
        numerator: r.numerator as i32,
        denominator: r.denominator as u32,
    }
}

fn cost_models_to_proto(c: &dugite_primitives::transaction::CostModels) -> pb::CostModels {
    pb::CostModels {
        plutus_v1: c
            .plutus_v1
            .as_ref()
            .map(|v| pb::CostModel { values: v.clone() }),
        plutus_v2: c
            .plutus_v2
            .as_ref()
            .map(|v| pb::CostModel { values: v.clone() }),
        plutus_v3: c
            .plutus_v3
            .as_ref()
            .map(|v| pb::CostModel { values: v.clone() }),
        plutus_v4: c
            .plutus_v4
            .as_ref()
            .map(|v| pb::CostModel { values: v.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::value::Lovelace;

    fn default_params() -> ProtocolParameters {
        // A pragmatic mainnet-ish set of params for round-trip testing.
        let mut p = ProtocolParameters::mainnet_defaults();
        p.min_fee_a = 44;
        p.min_fee_b = 155_381;
        p.max_block_body_size = 90_112;
        p.max_tx_size = 16_384;
        p.max_block_header_size = 1_100;
        p.key_deposit = Lovelace(2_000_000);
        p.pool_deposit = Lovelace(500_000_000);
        p.e_max = 18;
        p.n_opt = 500;
        p.min_pool_cost = Lovelace(170_000_000);
        p.ada_per_utxo_byte = Lovelace(4_310);
        p.max_val_size = 5_000;
        p.collateral_percentage = 150;
        p.max_collateral_inputs = 3;
        p.protocol_version_major = 10;
        p.protocol_version_minor = 0;
        p.drep_deposit = Lovelace(500_000_000);
        p.drep_activity = 20;
        p.gov_action_deposit = Lovelace(100_000_000_000);
        p.gov_action_lifetime = 6;
        p.committee_min_size = 7;
        p.committee_max_term_length = 146;
        p
    }

    #[test]
    fn pparams_basic_fields_round_trip() {
        let p = default_params();
        let pb = pparams_to_proto(&p);
        assert_eq!(pb.max_tx_size, 16_384);
        assert_eq!(pb.max_block_body_size, 90_112);
        assert_eq!(pb.max_block_header_size, 1_100);
        assert_eq!(pb.pool_retirement_epoch_bound, 18);
        assert_eq!(pb.desired_number_of_pools, 500);
        assert_eq!(pb.max_value_size, 5_000);
        assert_eq!(pb.collateral_percentage, 150);
        assert_eq!(pb.max_collateral_inputs, 3);
        assert_eq!(pb.min_committee_size, 7);
        assert_eq!(pb.committee_term_limit, 146);
        assert_eq!(pb.drep_inactivity_period, 20);
    }

    #[test]
    fn pparams_protocol_version_round_trip() {
        let p = default_params();
        let pb = pparams_to_proto(&p);
        let pv = pb.protocol_version.expect("pv set");
        assert_eq!(pv.major, 10);
        assert_eq!(pv.minor, 0);
    }

    #[test]
    fn pparams_voting_thresholds_have_expected_counts() {
        let p = default_params();
        let pb = pparams_to_proto(&p);
        let pool_vt = pb.pool_voting_thresholds.expect("pool_vt set");
        assert_eq!(pool_vt.thresholds.len(), 5);
        let drep_vt = pb.drep_voting_thresholds.expect("drep_vt set");
        assert_eq!(drep_vt.thresholds.len(), 10);
    }

    #[test]
    fn pparams_coin_fields_are_bigint() {
        let p = default_params();
        let pb = pparams_to_proto(&p);
        let pd = pb.pool_deposit.expect("pool_deposit set");
        match pd.big_int.unwrap() {
            pb::big_int::BigInt::Int(v) => assert_eq!(v, 500_000_000),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
