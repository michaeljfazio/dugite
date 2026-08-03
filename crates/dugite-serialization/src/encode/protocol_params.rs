use crate::cbor::*;
use dugite_primitives::transaction::*;

use super::certificate::encode_rational;

/// Encode a ProtocolParamUpdate as a CBOR map with integer keys per Conway CDDL.
///
/// Only fields that are `Some` are included in the map (sparse encoding).
/// Key mapping follows the Conway era protocol_param_update CDDL:
///   0: min_fee_a, 1: min_fee_b, 2: max_block_body_size, 3: max_tx_size,
///   4: max_block_header_size, 5: key_deposit, 6: pool_deposit, 7: e_max,
///   8: n_opt, 9: a0, 10: rho, 11: tau,
///   (keys 12-15 are Babbage-only: decentralization, extra_entropy,
///    protocol_version, min_utxo_value — not present in Conway PPU)
///   16: min_pool_cost, 17: ada_per_utxo_byte, 18: cost_models,
///   19: execution_costs, 20: max_tx_ex_units, 21: max_block_ex_units,
///   22: max_val_size, 23: collateral_percentage, 24: max_collateral_inputs,
///   25: pool_voting_thresholds(5), 26: drep_voting_thresholds(10),
///   27: min_committee_size, 28: committee_term_limit, 29: gov_action_lifetime,
///   30: gov_action_deposit, 31: drep_deposit, 32: drep_activity,
///   33: min_fee_ref_script_cost_per_byte
pub fn encode_protocol_param_update(ppu: &ProtocolParamUpdate) -> Vec<u8> {
    encode_ppu_map(&ppu_entries(ppu, PpuKeySet::Conway))
}

/// Encode a pre-Conway (Shelley..Babbage) `protocol_param_update`.
///
/// The Conway type genuinely has no keys 12-15, but the Shelley..Babbage one
/// does, and dugite's decoder reads all four:
///
/// - 12 `d` — decentralisation, live on mainnet throughout Shelley
/// - 13 `extra_entropy` — folded into the epoch nonce at TICKN; mainnet
///   injected a non-neutral value effective epoch 259
/// - 14 `protocol_version` — `[major, minor]`
/// - 15 `min_utxo_value` — the flat Shelley-Mary minUTxO (#919)
///
/// Emitting them is what makes `decode -> encode` a fixpoint for a pre-Conway
/// update proposal, and therefore what keeps its transaction id stable.
///
/// Mirrors `read_pre_conway_protocol_param_update` in `decode/era_shelley.rs`,
/// which carries the per-key Haskell citations
/// (`eras/{shelley,alonzo,babbage}/impl/src/Cardano/Ledger/*/PParams.hs`).
pub fn encode_pre_conway_protocol_param_update(ppu: &ProtocolParamUpdate) -> Vec<u8> {
    encode_ppu_map(&ppu_entries(ppu, PpuKeySet::PreConway))
}

/// Which `protocol_param_update` key set to emit.
///
/// One entry builder serves both, deliberately: three drifted copies of
/// `read_metadatum` were the mechanism behind #937, and triplicated block-body
/// encoders were the mechanism behind BOTH #932 and #938. A second hand-written
/// copy of a 30-key table is the same trap.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PpuKeySet {
    /// Conway and later: keys 0-11 and 16-33.
    Conway,
    /// Shelley..Babbage: keys 0-24, including 12-15.
    PreConway,
}

fn encode_ppu_map(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut buf = encode_map_header(entries.len());
    for (key, value) in entries {
        buf.extend(encode_uint(*key));
        buf.extend(value.clone());
    }
    buf
}

fn ppu_entries(ppu: &ProtocolParamUpdate, key_set: PpuKeySet) -> Vec<(u64, Vec<u8>)> {
    // Count non-None fields to determine map size
    let mut entries: Vec<(u64, Vec<u8>)> = Vec::new();

    if let Some(v) = ppu.min_fee_a {
        entries.push((0, encode_uint(v)));
    }
    if let Some(v) = ppu.min_fee_b {
        entries.push((1, encode_uint(v)));
    }
    if let Some(v) = ppu.max_block_body_size {
        entries.push((2, encode_uint(v)));
    }
    if let Some(v) = ppu.max_tx_size {
        entries.push((3, encode_uint(v)));
    }
    if let Some(v) = ppu.max_block_header_size {
        entries.push((4, encode_uint(v)));
    }
    if let Some(ref v) = ppu.key_deposit {
        entries.push((5, encode_uint(v.0)));
    }
    if let Some(ref v) = ppu.pool_deposit {
        entries.push((6, encode_uint(v.0)));
    }
    if let Some(v) = ppu.e_max {
        entries.push((7, encode_uint(v)));
    }
    if let Some(v) = ppu.n_opt {
        entries.push((8, encode_uint(v)));
    }
    if let Some(ref v) = ppu.a0 {
        entries.push((9, encode_rational(v)));
    }
    if let Some(ref v) = ppu.rho {
        entries.push((10, encode_rational(v)));
    }
    if let Some(ref v) = ppu.tau {
        entries.push((11, encode_rational(v)));
    }
    // Keys 12-15 exist only in the Shelley..Babbage PParamUpdate type. The
    // Conway type drops all four (decentralisation and extra entropy are gone;
    // protocol_version moved to the HardForkInitiation governance action;
    // min_utxo_value was replaced by coinsPerUTxOByte at key 17).
    if key_set == PpuKeySet::PreConway {
        if let Some(ref v) = ppu.d {
            entries.push((12, encode_rational(v)));
        }
        if let Some(ref v) = ppu.extra_entropy {
            // Shelley `Nonce`: [0] = NeutralNonce, [1, bstr32] = Nonce(h).
            // `Some(ZERO)` is the explicit neutral nonce, not "absent".
            let mut buf = Vec::new();
            if *v == dugite_primitives::hash::Hash32::ZERO {
                buf.extend(encode_array_header(1));
                buf.extend(encode_uint(0));
            } else {
                buf.extend(encode_array_header(2));
                buf.extend(encode_uint(1));
                buf.extend(encode_bytes(v.as_bytes()));
            }
            entries.push((13, buf));
        }
        // protocol_version is `[major, minor]` — a single key carrying both
        // halves, so it is emitted only when both are present.
        if let (Some(major), Some(minor)) = (ppu.protocol_version_major, ppu.protocol_version_minor)
        {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(major));
            buf.extend(encode_uint(minor));
            entries.push((14, buf));
        }
        if let Some(ref v) = ppu.min_utxo_value {
            entries.push((15, encode_uint(v.0)));
        }
    }
    if let Some(ref v) = ppu.min_pool_cost {
        entries.push((16, encode_uint(v.0)));
    }
    if let Some(ref v) = ppu.ada_per_utxo_byte {
        entries.push((17, encode_uint(v.0)));
    }
    if let Some(ref v) = ppu.cost_models {
        entries.push((18, encode_cost_models(v)));
    }
    if let Some(ref v) = ppu.execution_costs {
        let mut buf = encode_array_header(2);
        buf.extend(encode_rational(&v.mem_price));
        buf.extend(encode_rational(&v.step_price));
        entries.push((19, buf));
    }
    if let Some(ref v) = ppu.max_tx_ex_units {
        let mut buf = encode_array_header(2);
        buf.extend(encode_uint(v.mem));
        buf.extend(encode_uint(v.steps));
        entries.push((20, buf));
    }
    if let Some(ref v) = ppu.max_block_ex_units {
        let mut buf = encode_array_header(2);
        buf.extend(encode_uint(v.mem));
        buf.extend(encode_uint(v.steps));
        entries.push((21, buf));
    }
    if let Some(v) = ppu.max_val_size {
        entries.push((22, encode_uint(v)));
    }
    if let Some(v) = ppu.collateral_percentage {
        entries.push((23, encode_uint(v)));
    }
    if let Some(v) = ppu.max_collateral_inputs {
        entries.push((24, encode_uint(v)));
    }
    // Key 25: pool_voting_thresholds — 5-element array
    if ppu.pvt_motion_no_confidence.is_some()
        || ppu.pvt_committee_normal.is_some()
        || ppu.pvt_committee_no_confidence.is_some()
        || ppu.pvt_hard_fork.is_some()
        || ppu.pvt_pp_security_group.is_some()
    {
        let mut buf = encode_array_header(5);
        let zero = Rational {
            numerator: 0,
            denominator: 1,
        };
        buf.extend(encode_rational(
            ppu.pvt_motion_no_confidence.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.pvt_committee_normal.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.pvt_committee_no_confidence.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(ppu.pvt_hard_fork.as_ref().unwrap_or(&zero)));
        buf.extend(encode_rational(
            ppu.pvt_pp_security_group.as_ref().unwrap_or(&zero),
        ));
        entries.push((25, buf));
    }
    // Key 26: drep_voting_thresholds — 10-element array
    if ppu.dvt_pp_network_group.is_some()
        || ppu.dvt_pp_economic_group.is_some()
        || ppu.dvt_pp_technical_group.is_some()
        || ppu.dvt_pp_gov_group.is_some()
        || ppu.dvt_hard_fork.is_some()
        || ppu.dvt_no_confidence.is_some()
        || ppu.dvt_committee_normal.is_some()
        || ppu.dvt_committee_no_confidence.is_some()
        || ppu.dvt_constitution.is_some()
        || ppu.dvt_treasury_withdrawal.is_some()
    {
        let mut buf = encode_array_header(10);
        let zero = Rational {
            numerator: 0,
            denominator: 1,
        };
        buf.extend(encode_rational(
            ppu.dvt_no_confidence.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_committee_normal.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_committee_no_confidence.as_ref().unwrap_or(&zero),
        ));
        // Order is load-bearing and matches Haskell `EncCBOR DRepVotingThresholds`
        // (Conway/PParams.hs) element for element:
        //   0 motionNoConfidence   5 ppNetworkGroup
        //   1 committeeNormal      6 ppEconomicGroup
        //   2 committeeNoConfidence 7 ppTechnicalGroup
        //   3 updateToConstitution  8 ppGovGroup
        //   4 hardForkInitiation    9 treasuryWithdrawal
        //
        // This previously dropped `constitution` from index 3, shifted the next
        // six up by one, and appended it at index 9 — so a dugite-built
        // ParameterChange proposing new DRep thresholds installed the WRONG
        // ones (hard-fork threshold into the constitution slot, and so on).
        // The decoder was always right; only this side was wrong (#951).
        buf.extend(encode_rational(
            ppu.dvt_constitution.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(ppu.dvt_hard_fork.as_ref().unwrap_or(&zero)));
        buf.extend(encode_rational(
            ppu.dvt_pp_network_group.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_pp_economic_group.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_pp_technical_group.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_pp_gov_group.as_ref().unwrap_or(&zero),
        ));
        buf.extend(encode_rational(
            ppu.dvt_treasury_withdrawal.as_ref().unwrap_or(&zero),
        ));
        entries.push((26, buf));
    }
    if let Some(v) = ppu.min_committee_size {
        entries.push((27, encode_uint(v)));
    }
    if let Some(v) = ppu.committee_term_limit {
        entries.push((28, encode_uint(v)));
    }
    if let Some(v) = ppu.gov_action_lifetime {
        entries.push((29, encode_uint(v)));
    }
    if let Some(ref v) = ppu.gov_action_deposit {
        entries.push((30, encode_uint(v.0)));
    }
    if let Some(ref v) = ppu.drep_deposit {
        entries.push((31, encode_uint(v.0)));
    }
    if let Some(v) = ppu.drep_activity {
        entries.push((32, encode_uint(v)));
    }
    if let Some(ref v) = ppu.min_fee_ref_script_cost_per_byte {
        // NonNegativeInterval: encode the full rational as tag-30 [num, den].
        entries.push((33, encode_rational(v)));
    }

    // Keys 34-37 — the Dijkstra ref-script re-parameterisation
    // (`PParams.hs` `ppuTag = 34..37` in `DijkstraEra`).
    //
    // `read_protocol_param_update` has read all four since #475 Phase 5, but
    // nothing emitted them, so a dugite-built Dijkstra `ParameterChange` would
    // have silently dropped every ref-script parameter it proposed. Same shape
    // as the tx-body key 6 gap, and latent for the same reason — no on-chain
    // corpus exercises it. Found by the encode-first structured fuzz target
    // (#974).
    //
    // Emitting them under the Conway key set too is safe and deliberate: a
    // Conway PPU never has them populated, so no Conway bytes change, and
    // gating them on the era would need a key-set argument threaded through
    // `encode_gov_action`.
    if key_set == PpuKeySet::Conway {
        if let Some(v) = ppu.max_ref_script_size_per_block {
            entries.push((34, encode_uint(v as u64)));
        }
        if let Some(v) = ppu.max_ref_script_size_per_tx {
            entries.push((35, encode_uint(v as u64)));
        }
        if let Some(v) = ppu.ref_script_cost_stride {
            entries.push((36, encode_uint(v as u64)));
        }
        if let Some(ref v) = ppu.ref_script_cost_multiplier {
            // PositiveInterval — tag-30 [num, den], same as key 33.
            entries.push((37, encode_rational(v)));
        }
    }

    entries
}

/// Encode a pre-Conway update proposal — transaction body key 6.
///
/// ```text
/// update = [ proposed_protocol_parameter_updates, epoch ]
/// proposed_protocol_parameter_updates = { * genesishash => protocol_param_update }
/// ```
///
/// `genesishash` is the 28-byte blake2b_224 of a genesis delegate vkey. The
/// decoder pads it to `Hash32` for storage, so the low 28 bytes go back on the
/// wire.
pub fn encode_update_proposal(update: &UpdateProposal) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_map_header(update.proposed_updates.len()));
    for (genesis_hash, ppu) in &update.proposed_updates {
        buf.extend(encode_bytes(&genesis_hash.as_bytes()[..28]));
        buf.extend(encode_pre_conway_protocol_param_update(ppu));
    }
    buf.extend(encode_uint(update.epoch));
    buf
}

/// Encode CostModels as CBOR map: `{0: [v1...], 1: [v2...], 2: [v3...], 3: [v4...]}`.
///
/// The V4 slot (key 3) was added in Dijkstra per
/// `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/PParams.hs`. Absent
/// languages are simply omitted from the wire map (issue #475 Phase 5).
pub fn encode_cost_models(cm: &CostModels) -> Vec<u8> {
    let count = [&cm.plutus_v1, &cm.plutus_v2, &cm.plutus_v3, &cm.plutus_v4]
        .iter()
        .filter(|m| m.is_some())
        .count()
        + cm.unknown_cost_models.len();
    let mut buf = encode_map_header(count);
    if let Some(ref v1) = cm.plutus_v1 {
        buf.extend(encode_uint(0));
        buf.extend(encode_array_header(v1.len()));
        for cost in v1 {
            buf.extend(encode_int(*cost as i128));
        }
    }
    if let Some(ref v2) = cm.plutus_v2 {
        buf.extend(encode_uint(1));
        buf.extend(encode_array_header(v2.len()));
        for cost in v2 {
            buf.extend(encode_int(*cost as i128));
        }
    }
    if let Some(ref v3) = cm.plutus_v3 {
        buf.extend(encode_uint(2));
        buf.extend(encode_array_header(v3.len()));
        for cost in v3 {
            buf.extend(encode_int(*cost as i128));
        }
    }
    if let Some(ref v4) = cm.plutus_v4 {
        buf.extend(encode_uint(3));
        buf.extend(encode_array_header(v4.len()));
        for cost in v4 {
            buf.extend(encode_int(*cost as i128));
        }
    }
    // #770: unknown-language entries (keys ≥ 4) in ascending key order, mirroring
    // Haskell `flattenCostModels` (typed models folded on top of the unknown map).
    for (key, costs) in &cm.unknown_cost_models {
        buf.extend(encode_uint(u64::from(*key)));
        buf.extend(encode_array_header(costs.len()));
        for cost in costs {
            buf.extend(encode_int(*cost as i128));
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::transaction::{
        CostModels, ExUnitPrices, ExUnits, ProtocolParamUpdate, Rational,
    };
    use dugite_primitives::value::Lovelace;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build a Rational from numerator/denominator.
    fn rat(n: u64, d: u64) -> Rational {
        Rational {
            numerator: n,
            denominator: d,
        }
    }

    // ── empty map ────────────────────────────────────────────────────────────

    /// An all-None ProtocolParamUpdate must encode as CBOR map(0) = 0xa0.
    #[test]
    fn test_empty_ppu_encodes_as_map0() {
        let ppu = ProtocolParamUpdate::default();
        let encoded = encode_protocol_param_update(&ppu);
        assert_eq!(encoded, vec![0xa0], "empty ppu should be map(0) = 0xa0");
    }

    // ── sparse map with integer fields ────────────────────────────────────────

    /// A PPU with only min_fee_a (key 0) and min_fee_b (key 1) set must produce
    /// a CBOR map(2) with keys 0 and 1 in ascending order.
    #[test]
    fn test_sparse_min_fee_fields() {
        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(44),
            min_fee_b: Some(155_381),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // First byte: map(2) = 0xa2
        assert_eq!(encoded[0], 0xa2, "should be map(2)");

        // Keys must appear in ascending order: 0 then 1
        assert_eq!(encoded[1], 0x00, "first key must be 0 (min_fee_a)");
        // value 44 = 0x18 0x2c  (one-byte uint)
        assert_eq!(encoded[2], 0x18);
        assert_eq!(encoded[3], 0x2c, "min_fee_a value should be 44 (0x2c)");

        assert_eq!(encoded[4], 0x01, "second key must be 1 (min_fee_b)");
    }

    // ── rational fields use CBOR tag 30 ──────────────────────────────────────

    /// a0 (key 9), rho (key 10), and tau (key 11) must be encoded with CBOR tag 30.
    #[test]
    fn test_rational_fields_use_tag_30() {
        let ppu = ProtocolParamUpdate {
            a0: Some(rat(3, 10)),   // key 9
            rho: Some(rat(1, 100)), // key 10
            tau: Some(rat(1, 50)),  // key 11
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "should be map(3)");

        // key 9 = 0x09
        let mut pos = 1;
        assert_eq!(encoded[pos], 0x09, "key 9 (a0)");
        pos += 1;

        // value: tag(30) = 0xd8 0x1e, then array(2), num, den
        assert_eq!(encoded[pos], 0xd8, "tag high byte for a0");
        assert_eq!(encoded[pos + 1], 0x1e, "tag low byte 30 for a0");
        pos += 2;
        assert_eq!(encoded[pos], 0x82, "array(2) for rational a0");
        pos += 1;
        // numerator 3
        assert_eq!(encoded[pos], 0x03);
        pos += 1;
        // denominator 10
        assert_eq!(encoded[pos], 0x0a);
        pos += 1;

        // key 10 = 0x0a (rho)
        assert_eq!(encoded[pos], 0x0a, "key 10 (rho)");
        pos += 1;
        assert_eq!(encoded[pos], 0xd8, "tag(30) high byte for rho");
        assert_eq!(encoded[pos + 1], 0x1e, "tag(30) low byte for rho");
        pos += 2 + 1; // skip tag bytes + array(2)

        // numerator 1, denominator 100
        assert_eq!(encoded[pos], 0x01);
        pos += 1;
        assert_eq!(encoded[pos], 0x18); // 1-byte uint prefix
        assert_eq!(encoded[pos + 1], 0x64); // 100 = 0x64
        pos += 2;

        // key 11 = 0x0b (tau)
        assert_eq!(encoded[pos], 0x0b, "key 11 (tau)");
        pos += 1;
        assert_eq!(encoded[pos], 0xd8, "tag(30) high byte for tau");
        assert_eq!(encoded[pos + 1], 0x1e, "tag(30) low byte for tau");
    }

    // ── execution costs (key 19) ─────────────────────────────────────────────

    /// execution_costs (key 19) must encode as array(2) of two rationals,
    /// each wrapped in tag(30).
    #[test]
    fn test_execution_costs_key_19() {
        let ppu = ProtocolParamUpdate {
            execution_costs: Some(ExUnitPrices {
                mem_price: rat(577, 10_000),
                step_price: rat(721, 10_000_000),
            }),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1, "should be map(1)");
        // key 19 = 0x13
        assert_eq!(encoded[1], 0x13, "key 19 (execution_costs)");
        // value starts with array(2) = 0x82
        assert_eq!(encoded[2], 0x82, "array(2) for execution costs");
        // first element: rational mem_price — must start with tag(30)
        assert_eq!(encoded[3], 0xd8, "tag(30) high byte for mem_price");
        assert_eq!(encoded[4], 0x1e, "tag(30) low byte for mem_price");
    }

    // ── ExUnits (key 20) ─────────────────────────────────────────────────────

    /// max_tx_ex_units (key 20) must encode as array(2) of two plain uints
    /// (no tag 30 — ExUnits are not rationals).
    #[test]
    fn test_ex_units_key_20_plain_uints() {
        let ppu = ProtocolParamUpdate {
            max_tx_ex_units: Some(ExUnits {
                mem: 14_000_000,
                steps: 10_000_000_000,
            }),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1);
        // key 20 = 0x14
        assert_eq!(encoded[1], 0x14, "key 20 (max_tx_ex_units)");
        // value: array(2) = 0x82
        assert_eq!(encoded[2], 0x82, "array(2) for ExUnits");
        // Next byte is a uint (NOT 0xd8 tag-30); mem = 14_000_000
        // 14_000_000 in uint encoding starts with 0x1a (4-byte uint)
        assert_eq!(encoded[3], 0x1a, "mem should be 4-byte uint (no tag 30)");
    }

    // ── pool voting thresholds (key 25) ──────────────────────────────────────

    /// pool_voting_thresholds (key 25) must encode as a 5-element array of rationals.
    #[test]
    fn test_pool_voting_thresholds_key_25() {
        let r = rat(1, 2);
        let ppu = ProtocolParamUpdate {
            pvt_motion_no_confidence: Some(r.clone()),
            pvt_committee_normal: Some(r.clone()),
            pvt_committee_no_confidence: Some(r.clone()),
            pvt_hard_fork: Some(r.clone()),
            pvt_pp_security_group: Some(r.clone()),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(1)
        assert_eq!(encoded[0], 0xa1);
        // key 25 = 0x18 0x19 (two-byte encoding since 25 > 23)
        assert_eq!(encoded[1], 0x18, "key 25 high byte");
        assert_eq!(encoded[2], 0x19, "key 25 value (25 = 0x19)");
        // value: array(5) = 0x85
        assert_eq!(encoded[3], 0x85, "array(5) for pool voting thresholds");
        // Each element starts with tag(30) = 0xd8 0x1e
        assert_eq!(encoded[4], 0xd8, "first pvt element tag(30) high");
        assert_eq!(encoded[5], 0x1e, "first pvt element tag(30) low");
    }

    // ── DRep voting thresholds (key 26) ──────────────────────────────────────

    /// drep_voting_thresholds (key 26) must encode as a 10-element array of rationals.
    #[test]
    fn test_drep_voting_thresholds_key_26() {
        let r = rat(2, 3);
        let ppu = ProtocolParamUpdate {
            dvt_no_confidence: Some(r.clone()),
            dvt_committee_normal: Some(r.clone()),
            dvt_committee_no_confidence: Some(r.clone()),
            dvt_hard_fork: Some(r.clone()),
            dvt_pp_network_group: Some(r.clone()),
            dvt_pp_economic_group: Some(r.clone()),
            dvt_pp_technical_group: Some(r.clone()),
            dvt_pp_gov_group: Some(r.clone()),
            dvt_treasury_withdrawal: Some(r.clone()),
            dvt_constitution: Some(r.clone()),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(1)
        assert_eq!(encoded[0], 0xa1);
        // key 26 = 0x18 0x1a (two-byte encoding since 26 > 23)
        assert_eq!(encoded[1], 0x18, "key 26 high byte");
        assert_eq!(encoded[2], 0x1a, "key 26 value (26 = 0x1a)");
        // value: array(10) = 0x8a
        assert_eq!(encoded[3], 0x8a, "array(10) for drep voting thresholds");
        // First element: tag(30)
        assert_eq!(encoded[4], 0xd8, "first dvt element tag(30) high");
        assert_eq!(encoded[5], 0x1e, "first dvt element tag(30) low");
    }

    // ── Conway governance fields (keys 27-33) ────────────────────────────────

    /// Conway governance fields keys 27-33 must encode as simple uint values
    /// (or a rational for key 33, min_fee_ref_script_cost_per_byte).
    /// All of these keys are ≥ 24, so they use two-byte CBOR uint encoding.
    #[test]
    fn test_conway_governance_fields() {
        let ppu = ProtocolParamUpdate {
            min_committee_size: Some(5),                         // key 27
            committee_term_limit: Some(146),                     // key 28
            gov_action_lifetime: Some(6),                        // key 29
            gov_action_deposit: Some(Lovelace(100_000_000_000)), // key 30
            drep_deposit: Some(Lovelace(500_000_000)),           // key 31
            drep_activity: Some(20),                             // key 32
            min_fee_ref_script_cost_per_byte: Some(Rational {
                numerator: 15,
                denominator: 1,
            }), // key 33 — encoded as rational 15/1
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(7) = 0xa7
        assert_eq!(encoded[0], 0xa7, "should be map(7) for 7 Conway fields");

        // Walk through to confirm key ordering: 27, 28, 29, 30, 31, 32, 33
        let mut pos = 1;

        // key 27 = 0x18 0x1b  (min_committee_size)
        assert_eq!(encoded[pos], 0x18, "key 27 high byte");
        assert_eq!(encoded[pos + 1], 0x1b, "key 27 value (27 = 0x1b)");
        pos += 2;
        // value 5
        assert_eq!(encoded[pos], 0x05);
        pos += 1;

        // key 28 = 0x18 0x1c  (committee_term_limit)
        assert_eq!(encoded[pos], 0x18, "key 28 high byte");
        assert_eq!(encoded[pos + 1], 0x1c, "key 28 value (28 = 0x1c)");
        pos += 2;
        // value 146 = 0x18 0x92
        assert_eq!(encoded[pos], 0x18);
        assert_eq!(encoded[pos + 1], 0x92); // 146 = 0x92
        pos += 2;

        // key 29 = 0x18 0x1d  (gov_action_lifetime)
        assert_eq!(encoded[pos], 0x18, "key 29 high byte");
        assert_eq!(encoded[pos + 1], 0x1d, "key 29 value (29 = 0x1d)");
        pos += 2;
        // value 6
        assert_eq!(encoded[pos], 0x06);
        pos += 1;

        // key 30 = 0x18 0x1e  (gov_action_deposit)
        assert_eq!(encoded[pos], 0x18, "key 30 high byte");
        assert_eq!(encoded[pos + 1], 0x1e, "key 30 value (30 = 0x1e)");
        pos += 2;
        // 100_000_000_000 is a 5-byte uint (> 4294967296), encoded as 0x1b + 8 bytes
        assert_eq!(
            encoded[pos], 0x1b,
            "gov_action_deposit should be 8-byte uint"
        );
        pos += 9; // 1 prefix + 8 bytes

        // key 31 = 0x18 0x1f  (drep_deposit)
        assert_eq!(encoded[pos], 0x18, "key 31 high byte");
        assert_eq!(encoded[pos + 1], 0x1f, "key 31 value (31 = 0x1f)");
        pos += 2;
        // 500_000_000 = 0x1d_cd_65_00  → 4-byte uint: 0x1a
        assert_eq!(encoded[pos], 0x1a, "drep_deposit should be 4-byte uint");
        pos += 5; // 1 prefix + 4 bytes

        // key 32 = 0x18 0x20  (drep_activity)
        assert_eq!(encoded[pos], 0x18, "key 32 high byte");
        assert_eq!(encoded[pos + 1], 0x20, "key 32 value (32 = 0x20)");
        pos += 2;
        // value 20 = 0x14
        assert_eq!(encoded[pos], 0x14);
        pos += 1;

        // key 33 = 0x18 0x21  (min_fee_ref_script_cost_per_byte)
        assert_eq!(encoded[pos], 0x18, "key 33 high byte");
        assert_eq!(encoded[pos + 1], 0x21, "key 33 value (33 = 0x21)");
        pos += 2;
        // value: rational 15/1 → tag(30) = 0xd8 0x1e
        assert_eq!(encoded[pos], 0xd8, "key 33 value must be rational (tag 30)");
        assert_eq!(encoded[pos + 1], 0x1e, "tag 30 low byte");
    }

    // ── integer keys are in ascending order ──────────────────────────────────

    /// Setting keys 0, 9, 20, 27 in a single PPU and verifying monotone key order.
    #[test]
    fn test_keys_are_in_ascending_order() {
        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(44),                                 // key 0
            a0: Some(rat(3, 10)),                                // key 9
            max_tx_ex_units: Some(ExUnits { mem: 1, steps: 2 }), // key 20
            min_committee_size: Some(3),                         // key 27
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // Collect the key bytes by walking the map manually.
        // map(4) = 0xa4
        assert_eq!(encoded[0], 0xa4, "map(4)");

        // key 0 at position 1
        assert_eq!(encoded[1], 0x00, "first key should be 0");

        // After value (uint 44 = 0x18 0x2c) key 9 appears
        // pos 1 = key 0x00, pos 2-3 = value (0x18 0x2c), pos 4 = key 9
        assert_eq!(encoded[4], 0x09, "second key should be 9");

        // After rational (tag 30 = 0xd8 0x1e + array(2) + 0x03 + 0x0a = 5 bytes) key 20 appears
        // pos 5..9 = rational bytes (5 bytes), pos 10 = key 20 (0x14, single byte since 20 ≤ 23)
        assert_eq!(encoded[10], 0x14, "third key should be 20 (0x14)");
    }

    // ── cost models ──────────────────────────────────────────────────────────

    /// Cost models with only V1 must produce map(1) {0: [costs...]}.
    #[test]
    fn test_cost_models_v1_only() {
        let cm = CostModels {
            plutus_v1: Some(vec![100, 200, 300]),
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);

        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1, "cost models map should have 1 entry");
        // key 0
        assert_eq!(encoded[1], 0x00, "PlutusV1 key = 0");
        // array(3) = 0x83
        assert_eq!(encoded[2], 0x83, "V1 array should have 3 elements");
        // value 100 = 0x18 0x64 (1-byte uint)
        assert_eq!(encoded[3], 0x18, "100 uses 1-byte uint prefix");
        assert_eq!(encoded[4], 0x64, "100 = 0x64");

        // value 200 = 0x18 0xc8
        assert_eq!(encoded[5], 0x18, "200 uses 1-byte uint prefix");
        assert_eq!(encoded[6], 0xc8, "200 = 0xc8");

        // value 300 > 255, so uses 2-byte uint: 0x19 0x01 0x2c
        assert_eq!(encoded[7], 0x19, "300 uses 2-byte uint prefix");
        assert_eq!(encoded[8], 0x01, "300 high byte");
        assert_eq!(encoded[9], 0x2c, "300 low byte (0x012c = 300)");
    }

    /// Cost models with V1, V2, and V3 must produce map(3) with keys 0, 1, 2.
    #[test]
    fn test_cost_models_all_versions() {
        let cm = CostModels {
            plutus_v1: Some(vec![1]),
            plutus_v2: Some(vec![2]),
            plutus_v3: Some(vec![3]),
            plutus_v4: None,
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);

        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "should be map(3) for all three versions");

        // key 0
        assert_eq!(encoded[1], 0x00, "V1 key = 0");
        // array(1) = 0x81, value 1
        assert_eq!(encoded[2], 0x81);
        assert_eq!(encoded[3], 0x01);

        // key 1
        assert_eq!(encoded[4], 0x01, "V2 key = 1");
        // array(1) = 0x81, value 2
        assert_eq!(encoded[5], 0x81);
        assert_eq!(encoded[6], 0x02);

        // key 2
        assert_eq!(encoded[7], 0x02, "V3 key = 2");
        // array(1) = 0x81, value 3
        assert_eq!(encoded[8], 0x81);
        assert_eq!(encoded[9], 0x03);
    }

    /// Cost models with all four versions (V1+V2+V3+V4) must produce map(4)
    /// with keys 0,1,2,3 in slot order — Dijkstra (issue #475 Phase 5).
    #[test]
    fn test_cost_models_all_four_versions_with_v4() {
        let cm = CostModels {
            plutus_v1: Some(vec![1]),
            plutus_v2: Some(vec![2]),
            plutus_v3: Some(vec![3]),
            plutus_v4: Some(vec![4]),
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);
        // map(4) = 0xa4
        assert_eq!(encoded[0], 0xa4, "all 4 versions present → map(4)");
        // The 4th slot (V4) is at key 3 (CBOR uint 0x03), array(1) [uint 4]
        // Walk to the trailing entry — V4 at offset 10 (after V1/V2/V3 each
        // contributing 3 bytes: key + array(1) + value).
        assert_eq!(encoded[10], 0x03, "PlutusV4 occupies map key 3");
        assert_eq!(encoded[11], 0x81, "V4 array(1)");
        assert_eq!(encoded[12], 0x04, "V4 cost value 4");
    }

    /// Cost models with V4 only (issue #475 Phase 5).
    #[test]
    fn test_cost_models_v4_only() {
        let cm = CostModels {
            plutus_v4: Some(vec![42, -1, 7]),
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);
        // map(1), key 3, array(3) = 0xa1 0x03 0x83
        assert_eq!(encoded[0], 0xa1);
        assert_eq!(encoded[1], 0x03, "PlutusV4 cost-model key = 3 (Dijkstra)");
        assert_eq!(encoded[2], 0x83);
    }

    /// #770: unknown-language entries (keys ≥ 4) are encoded after the typed
    /// keys in ascending key order, mirroring Haskell `flattenCostModels`.
    #[test]
    fn test_cost_models_preserves_unknown_entry() {
        let cm = CostModels {
            plutus_v2: Some(vec![10]),
            unknown_cost_models: [(5u8, vec![99i64, -1])].into_iter().collect(),
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);
        // map(2): key 1 (V2) then key 5 (unknown), ascending.
        assert_eq!(encoded[0], 0xa2, "map(2): one typed + one unknown");
        assert_eq!(encoded[1], 0x01, "first key must be 1 (V2)");
        // V2 value: array(1) [10] = 0x81 0x0a
        assert_eq!(&encoded[2..5], &[0x81, 0x0a, 0x05]);
        // ...then key 5 (unknown), array(2) [99, -1] = 0x82 0x18 0x63 0x20
        assert_eq!(&encoded[5..], &[0x82, 0x18, 0x63, 0x20]);

        // Round-trips through the canonical decoder, preserving the unknown.
        let mut r = crate::decode::reader::Reader::new(&encoded);
        let back = crate::decode::era_conway::read_cost_models(&mut r).unwrap();
        assert_eq!(back.plutus_v2, Some(vec![10]));
        assert_eq!(back.unknown_cost_models.get(&5), Some(&vec![99, -1]));
    }

    /// #770: the two hand-rolled flat-map emitters — `encode_cost_models`
    /// (PPU/active-params) and `CostModels::to_cbor` (phase-2 evaluator feed) —
    /// must stay byte-identical, including unknown entries, so they cannot drift.
    #[test]
    fn test_encode_cost_models_matches_to_cbor_with_unknown() {
        let cm = CostModels {
            plutus_v1: Some(vec![1, 2]),
            plutus_v3: Some(vec![-300, 7]),
            unknown_cost_models: [(4u8, vec![5i64]), (9u8, vec![6, 7])].into_iter().collect(),
            ..Default::default()
        };
        assert_eq!(
            encode_cost_models(&cm),
            cm.to_cbor().expect("non-empty cost models encode"),
            "encode_cost_models and CostModels::to_cbor must be byte-identical"
        );
    }

    /// Cost models with negative values must use CBOR negative integer encoding.
    /// -1 encodes as 0x20.
    #[test]
    fn test_cost_models_negative_values() {
        let cm = CostModels {
            plutus_v1: Some(vec![-1, -100, 0, 1]),
            ..Default::default()
        };
        let encoded = encode_cost_models(&cm);

        // map(1), key 0, array(4)
        assert_eq!(encoded[0], 0xa1);
        assert_eq!(encoded[1], 0x00);
        assert_eq!(encoded[2], 0x84, "array(4)");

        // -1 = 0x20  (CBOR: major type 1, additional info 0)
        assert_eq!(encoded[3], 0x20, "-1 should encode as 0x20");
        // -100: encode_int(-100) = 0x38 0x63
        assert_eq!(
            encoded[4], 0x38,
            "-100 should use 1-byte negative encoding prefix"
        );
        assert_eq!(encoded[5], 0x63, "-100 value byte (99 = 0x63)");
        // 0 = 0x00
        assert_eq!(encoded[6], 0x00, "0 should encode as 0x00");
        // 1 = 0x01
        assert_eq!(encoded[7], 0x01, "1 should encode as 0x01");
    }

    // ── cost models via protocol param update (key 18) ───────────────────────

    /// cost_models set on a PPU must produce key 18 in the map.
    #[test]
    fn test_ppu_cost_models_key_18() {
        let ppu = ProtocolParamUpdate {
            cost_models: Some(CostModels {
                plutus_v1: Some(vec![42]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let encoded = encode_protocol_param_update(&ppu);

        // map(1)
        assert_eq!(encoded[0], 0xa1);
        // key 18 = 0x12
        assert_eq!(encoded[1], 0x12, "cost_models key should be 18 (0x12)");
        // value: cost models starts with map(1) = 0xa1
        assert_eq!(encoded[2], 0xa1, "cost models inner map(1)");
    }

    // ── encode→decode round-trip (key correctness regression) ────────────────

    /// Every Conway PPU key round-trips, compared as a WHOLE struct.
    ///
    /// The previous version of this test claimed in its own docstring to cover
    /// "keys ... 25-26" and populated NEITHER — every threshold was left at
    /// `..Default::default()`, and the assertions were a hand-written list of
    /// nine fields. That is why #951 survived: the PPU key 26 encoder wrote the
    /// ten `drep_voting_thresholds` in the wrong order, and nothing here ever
    /// put a value in key 26 to notice.
    ///
    /// Two changes make it hold: every field is populated with a DISTINCT
    /// value, so a permutation shows up as two fields swapping; and the
    /// comparison is `assert_eq!` on the whole `ProtocolParamUpdate`, so a key
    /// added later cannot escape by not being listed.
    #[test]
    fn test_ppu_round_trip_all_conway_keys() {
        use crate::decode::ppu_from_cbor;

        let original = conway_ppu_all_keys();

        // Guard the guard: if a field is left None the round-trip is trivially
        // satisfied for it, which is exactly how this test used to pass.
        assert_eq!(
            conway_populated_field_count(&original),
            CONWAY_PPU_FIELD_COUNT,
            "the fixture must populate every Conway PPU field; an unpopulated \
             field is a field this test does not check"
        );

        let encoded = encode_protocol_param_update(&original);
        let decoded = ppu_from_cbor(&encoded).expect("round-trip decode must succeed");

        assert_eq!(
            decoded, original,
            "Conway PPU did not survive encode -> decode. A field holding \
             ANOTHER field's value is the #951 shape: keys 25 and 26 are \
             POSITIONAL arrays of 5 and 10 thresholds, so a wrong write order \
             is invisible to any per-key check."
        );

        // Idempotence, so a non-canonical encoding cannot hide behind a lenient
        // decode.
        assert_eq!(
            encode_protocol_param_update(&decoded),
            encoded,
            "PPU encoder must be idempotent"
        );
    }

    /// The pre-Conway key set, including keys 12-15, which the Conway type does
    /// not have and which had NO encoder at all until the fuzz work for #974
    /// surfaced it. `d` was live on mainnet throughout Shelley and
    /// `extra_entropy` carried the one-time non-neutral value at epoch 259.
    #[test]
    fn test_ppu_round_trip_all_pre_conway_keys() {
        use crate::decode::pre_conway_ppu_from_cbor;

        let original = ProtocolParamUpdate {
            min_fee_a: Some(44),
            min_fee_b: Some(155_381),
            max_block_body_size: Some(90_112),
            max_tx_size: Some(16_384),
            max_block_header_size: Some(1_100),
            key_deposit: Some(Lovelace(2_000_000)),
            pool_deposit: Some(Lovelace(500_000_000)),
            e_max: Some(18),
            n_opt: Some(500),
            a0: Some(rat(3, 10)),
            rho: Some(rat(3, 1000)),
            tau: Some(rat(1, 5)),
            // Keys 12-15 — pre-Conway only.
            d: Some(rat(1, 2)),
            extra_entropy: Some(dugite_primitives::hash::Hash::from_bytes([0x5a; 32])),
            protocol_version_major: Some(8),
            protocol_version_minor: Some(1),
            min_utxo_value: Some(Lovelace(1_000_000)),
            min_pool_cost: Some(Lovelace(170_000_000)),
            ada_per_utxo_byte: Some(Lovelace(4_310)),
            cost_models: Some(CostModels {
                plutus_v1: Some(vec![100, 200]),
                plutus_v2: Some(vec![300]),
                plutus_v3: None,
                plutus_v4: None,
                ..Default::default()
            }),
            execution_costs: Some(ExUnitPrices {
                mem_price: rat(577, 10_000),
                step_price: rat(721, 10_000_000),
            }),
            max_tx_ex_units: Some(ExUnits {
                mem: 14_000_000,
                steps: 10_000_000_000,
            }),
            max_block_ex_units: Some(ExUnits {
                mem: 62_000_000,
                steps: 20_000_000_000,
            }),
            max_val_size: Some(5_000),
            collateral_percentage: Some(150),
            max_collateral_inputs: Some(3),
            ..Default::default()
        };

        let encoded = encode_pre_conway_protocol_param_update(&original);
        let decoded = pre_conway_ppu_from_cbor(&encoded).expect("round-trip decode must succeed");

        assert_eq!(
            decoded, original,
            "pre-Conway PPU did not survive encode -> decode"
        );
        assert_eq!(
            encode_pre_conway_protocol_param_update(&decoded),
            encoded,
            "pre-Conway PPU encoder must be idempotent"
        );
    }

    /// Fields the Conway PPU wire type carries (keys 0-11, 16-33, 34-37).
    ///
    /// Keys 12-15 are pre-Conway only and are excluded deliberately: including
    /// them would make the Conway round-trip fail on fields the Conway encoder
    /// is correct not to emit.
    const CONWAY_PPU_FIELD_COUNT: usize = 47;

    fn conway_populated_field_count(ppu: &ProtocolParamUpdate) -> usize {
        let mut n = 0;
        let mut count = |present: bool| {
            if present {
                n += 1;
            }
        };
        count(ppu.min_fee_a.is_some());
        count(ppu.min_fee_b.is_some());
        count(ppu.max_block_body_size.is_some());
        count(ppu.max_tx_size.is_some());
        count(ppu.max_block_header_size.is_some());
        count(ppu.key_deposit.is_some());
        count(ppu.pool_deposit.is_some());
        count(ppu.e_max.is_some());
        count(ppu.n_opt.is_some());
        count(ppu.a0.is_some());
        count(ppu.rho.is_some());
        count(ppu.tau.is_some());
        count(ppu.min_pool_cost.is_some());
        count(ppu.ada_per_utxo_byte.is_some());
        count(ppu.cost_models.is_some());
        count(ppu.execution_costs.is_some());
        count(ppu.max_tx_ex_units.is_some());
        count(ppu.max_block_ex_units.is_some());
        count(ppu.max_val_size.is_some());
        count(ppu.collateral_percentage.is_some());
        count(ppu.max_collateral_inputs.is_some());
        count(ppu.min_fee_ref_script_cost_per_byte.is_some());
        count(ppu.drep_deposit.is_some());
        count(ppu.gov_action_deposit.is_some());
        count(ppu.gov_action_lifetime.is_some());
        count(ppu.dvt_pp_network_group.is_some());
        count(ppu.dvt_pp_economic_group.is_some());
        count(ppu.dvt_pp_technical_group.is_some());
        count(ppu.dvt_pp_gov_group.is_some());
        count(ppu.dvt_hard_fork.is_some());
        count(ppu.dvt_no_confidence.is_some());
        count(ppu.dvt_committee_normal.is_some());
        count(ppu.dvt_committee_no_confidence.is_some());
        count(ppu.dvt_constitution.is_some());
        count(ppu.dvt_treasury_withdrawal.is_some());
        count(ppu.pvt_motion_no_confidence.is_some());
        count(ppu.pvt_committee_normal.is_some());
        count(ppu.pvt_committee_no_confidence.is_some());
        count(ppu.pvt_hard_fork.is_some());
        count(ppu.pvt_pp_security_group.is_some());
        count(ppu.min_committee_size.is_some());
        count(ppu.committee_term_limit.is_some());
        count(ppu.drep_activity.is_some());
        count(ppu.max_ref_script_size_per_block.is_some());
        count(ppu.max_ref_script_size_per_tx.is_some());
        count(ppu.ref_script_cost_stride.is_some());
        count(ppu.ref_script_cost_multiplier.is_some());
        n
    }

    /// Every Conway PPU field, each with a DISTINCT value.
    ///
    /// Distinctness is the point: identical thresholds would round-trip through
    /// a permuted encoder unchanged, which is precisely how a wrong order hides.
    fn conway_ppu_all_keys() -> ProtocolParamUpdate {
        ProtocolParamUpdate {
            min_fee_a: Some(44),
            min_fee_b: Some(155_381),
            max_block_body_size: Some(90_112),
            max_tx_size: Some(16_384),
            max_block_header_size: Some(1_100),
            key_deposit: Some(Lovelace(2_000_000)),
            pool_deposit: Some(Lovelace(500_000_000)),
            e_max: Some(18),
            n_opt: Some(500),
            a0: Some(rat(3, 10)),
            rho: Some(rat(3, 1_000)),
            tau: Some(rat(1, 5)),
            min_pool_cost: Some(Lovelace(170_000_000)),
            ada_per_utxo_byte: Some(Lovelace(4_310)),
            cost_models: Some(CostModels {
                plutus_v1: Some(vec![100, 200]),
                plutus_v2: Some(vec![300, 400]),
                plutus_v3: Some(vec![500]),
                plutus_v4: Some(vec![600]),
                ..Default::default()
            }),
            execution_costs: Some(ExUnitPrices {
                mem_price: rat(577, 10_000),
                step_price: rat(721, 10_000_000),
            }),
            max_tx_ex_units: Some(ExUnits {
                mem: 14_000_000,
                steps: 10_000_000_000,
            }),
            max_block_ex_units: Some(ExUnits {
                mem: 62_000_000,
                steps: 20_000_000_000,
            }),
            max_val_size: Some(5_000),
            collateral_percentage: Some(150),
            max_collateral_inputs: Some(3),
            min_fee_ref_script_cost_per_byte: Some(rat(15, 1)),
            drep_deposit: Some(Lovelace(500_000_001)),
            gov_action_deposit: Some(Lovelace(100_000_000_000)),
            gov_action_lifetime: Some(6),
            // Key 26 — ten DISTINCT thresholds, in Haskell's
            // `EncCBOR DRepVotingThresholds` order. Every value is already in
            // LOWEST TERMS: `Reader::read_rational` divides by the gcd at
            // decode time to match Haskell's `%` smart constructor, so a
            // non-reduced fixture fails the round-trip on the fixture's fault.
            dvt_pp_network_group: Some(rat(501, 1_000)),
            dvt_pp_economic_group: Some(rat(503, 1_000)),
            dvt_pp_technical_group: Some(rat(507, 1_000)),
            dvt_pp_gov_group: Some(rat(509, 1_000)),
            dvt_hard_fork: Some(rat(511, 1_000)),
            dvt_no_confidence: Some(rat(513, 1_000)),
            dvt_committee_normal: Some(rat(517, 1_000)),
            dvt_committee_no_confidence: Some(rat(519, 1_000)),
            dvt_constitution: Some(rat(521, 1_000)),
            dvt_treasury_withdrawal: Some(rat(523, 1_000)),
            // Key 25 — five DISTINCT pool thresholds.
            pvt_motion_no_confidence: Some(rat(301, 1_000)),
            pvt_committee_normal: Some(rat(303, 1_000)),
            pvt_committee_no_confidence: Some(rat(307, 1_000)),
            pvt_hard_fork: Some(rat(309, 1_000)),
            pvt_pp_security_group: Some(rat(311, 1_000)),
            min_committee_size: Some(5),
            committee_term_limit: Some(146),
            drep_activity: Some(20),
            // Dijkstra keys 34-37.
            max_ref_script_size_per_block: Some(1_000_000),
            max_ref_script_size_per_tx: Some(200_000),
            ref_script_cost_stride: Some(25_600),
            ref_script_cost_multiplier: Some(rat(6, 5)),
            ..Default::default()
        }
    }

    // ── round-trip of a realistic PPU ────────────────────────────────────────

    /// A realistic Conway-era PPU with several fields set must encode without
    /// panicking, and the total byte count must be deterministic.
    #[test]
    fn test_realistic_ppu_is_deterministic() {
        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(44),
            min_fee_b: Some(155_381),
            max_block_body_size: Some(90_112),
            max_tx_size: Some(16_384),
            key_deposit: Some(Lovelace(2_000_000)),
            pool_deposit: Some(Lovelace(500_000_000)),
            a0: Some(rat(3, 10)),
            rho: Some(rat(3, 1000)),
            tau: Some(rat(1, 5)),
            n_opt: Some(500),
            min_pool_cost: Some(Lovelace(170_000_000)),
            ada_per_utxo_byte: Some(Lovelace(4_310)),
            gov_action_deposit: Some(Lovelace(100_000_000_000)),
            drep_deposit: Some(Lovelace(500_000_000)),
            drep_activity: Some(20),
            min_committee_size: Some(5),
            committee_term_limit: Some(146),
            ..Default::default()
        };

        let first = encode_protocol_param_update(&ppu);
        let second = encode_protocol_param_update(&ppu);

        // Deterministic: two calls produce identical bytes
        assert_eq!(first, second, "encoding must be deterministic");

        // Non-empty
        assert!(!first.is_empty(), "encoded PPU must not be empty");

        // Starts with a map header (0xa0..0xb7 for small maps, 0xb8 for larger)
        assert!(
            first[0] >= 0xa0,
            "first byte must be a CBOR map header, got {:#04x}",
            first[0]
        );
    }

    /// Round-trip every Conway-valid `ProtocolParamUpdate` field with a
    /// distinct value.
    ///
    /// This is the test that caught #951: the encoder wrote
    /// `drep_voting_thresholds` (key 26) in the wrong order — dropping
    /// `constitution` from index 3, shifting six fields up, and appending it at
    /// index 9 — so a dugite-built ParameterChange installed the WRONG
    /// thresholds. It also guards the #919 class (key 15 decoded then dropped).
    ///
    /// Two deliberate accommodations, both verified rather than assumed:
    ///
    /// 1. `min_utxo_value`, `d`, `extra_entropy` and `protocol_version_*` are
    ///    Babbage-only (PPU keys 12-15) and are NOT part of the Conway PPU —
    ///    Conway moves protocol-version changes to the `HardForkInitiation`
    ///    governance action. They are asserted absent, not round-tripped.
    /// 2. `read_rational` reduces fractions, so 10/100 comes back as 1/10.
    ///    Rationals are therefore compared by VALUE, not struct equality.
    ///
    /// NOTE this is necessary but NOT sufficient: a shared wrong order on both
    /// halves would still pass. #951 was caught only because the two disagreed.
    /// The durable guard is a Haskell-derived fixture.
    #[test]
    fn protocol_param_update_round_trips_every_conway_field() {
        use dugite_primitives::transaction::{ProtocolParamUpdate, Rational};
        use dugite_primitives::value::Lovelace;

        fn rat(n: u64) -> Option<Rational> {
            Some(Rational {
                numerator: n,
                denominator: 100,
            })
        }
        /// Compare by value: a/b == c/d  <=>  a*d == c*b.
        fn rat_eq(a: &Option<Rational>, b: &Option<Rational>, what: &str) {
            match (a, b) {
                (Some(x), Some(y)) => assert_eq!(
                    x.numerator as i128 * y.denominator as i128,
                    y.numerator as i128 * x.denominator as i128,
                    "{what}: {x:?} != {y:?} by value"
                ),
                (None, None) => {}
                _ => panic!("{what}: presence mismatch {a:?} vs {b:?}"),
            }
        }

        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(101),
            min_fee_b: Some(102),
            max_block_body_size: Some(103),
            max_tx_size: Some(104),
            max_block_header_size: Some(105),
            key_deposit: Some(Lovelace(1006)),
            pool_deposit: Some(Lovelace(1007)),
            e_max: Some(108),
            n_opt: Some(109),
            a0: rat(10),
            rho: rat(11),
            tau: rat(12),
            min_pool_cost: Some(Lovelace(1013)),
            ada_per_utxo_byte: Some(Lovelace(1014)),
            max_val_size: Some(116),
            collateral_percentage: Some(117),
            max_collateral_inputs: Some(118),
            min_fee_ref_script_cost_per_byte: rat(19),
            drep_deposit: Some(Lovelace(1023)),
            gov_action_deposit: Some(Lovelace(1024)),
            gov_action_lifetime: Some(125),
            // 10 DRep thresholds, each distinct so any permutation shows up.
            dvt_no_confidence: rat(31),
            dvt_committee_normal: rat(32),
            dvt_committee_no_confidence: rat(33),
            dvt_constitution: rat(34),
            dvt_hard_fork: rat(30),
            dvt_pp_network_group: rat(26),
            dvt_pp_economic_group: rat(27),
            dvt_pp_technical_group: rat(28),
            dvt_pp_gov_group: rat(29),
            dvt_treasury_withdrawal: rat(35),
            // 5 pool thresholds.
            pvt_motion_no_confidence: rat(36),
            pvt_committee_normal: rat(37),
            pvt_committee_no_confidence: rat(38),
            pvt_hard_fork: rat(39),
            pvt_pp_security_group: rat(40),
            ..Default::default()
        };

        let enc = encode_protocol_param_update(&ppu);
        let d = crate::decode::era_conway::read_protocol_param_update_for_test(
            &mut crate::decode::reader::Reader::new(&enc),
        )
        .expect("PPU must decode its own encoding");

        // Scalars must survive exactly.
        assert_eq!(d.min_fee_a, ppu.min_fee_a);
        assert_eq!(d.min_fee_b, ppu.min_fee_b);
        assert_eq!(d.max_block_body_size, ppu.max_block_body_size);
        assert_eq!(d.max_tx_size, ppu.max_tx_size);
        assert_eq!(d.max_block_header_size, ppu.max_block_header_size);
        assert_eq!(d.key_deposit, ppu.key_deposit);
        assert_eq!(d.pool_deposit, ppu.pool_deposit);
        assert_eq!(d.e_max, ppu.e_max);
        assert_eq!(d.n_opt, ppu.n_opt);
        assert_eq!(d.min_pool_cost, ppu.min_pool_cost);
        assert_eq!(d.ada_per_utxo_byte, ppu.ada_per_utxo_byte);
        assert_eq!(d.max_val_size, ppu.max_val_size);
        assert_eq!(d.collateral_percentage, ppu.collateral_percentage);
        assert_eq!(d.max_collateral_inputs, ppu.max_collateral_inputs);
        assert_eq!(d.drep_deposit, ppu.drep_deposit);
        assert_eq!(d.gov_action_deposit, ppu.gov_action_deposit);
        assert_eq!(d.gov_action_lifetime, ppu.gov_action_lifetime);

        // Rationals by value.
        for (got, want, what) in [
            (&d.a0, &ppu.a0, "a0"),
            (&d.rho, &ppu.rho, "rho"),
            (&d.tau, &ppu.tau, "tau"),
            (
                &d.min_fee_ref_script_cost_per_byte,
                &ppu.min_fee_ref_script_cost_per_byte,
                "min_fee_ref_script_cost_per_byte",
            ),
            (
                &d.dvt_no_confidence,
                &ppu.dvt_no_confidence,
                "dvt_no_confidence",
            ),
            (
                &d.dvt_committee_normal,
                &ppu.dvt_committee_normal,
                "dvt_committee_normal",
            ),
            (
                &d.dvt_committee_no_confidence,
                &ppu.dvt_committee_no_confidence,
                "dvt_committee_no_confidence",
            ),
            (
                &d.dvt_constitution,
                &ppu.dvt_constitution,
                "dvt_constitution",
            ),
            (&d.dvt_hard_fork, &ppu.dvt_hard_fork, "dvt_hard_fork"),
            (
                &d.dvt_pp_network_group,
                &ppu.dvt_pp_network_group,
                "dvt_pp_network_group",
            ),
            (
                &d.dvt_pp_economic_group,
                &ppu.dvt_pp_economic_group,
                "dvt_pp_economic_group",
            ),
            (
                &d.dvt_pp_technical_group,
                &ppu.dvt_pp_technical_group,
                "dvt_pp_technical_group",
            ),
            (
                &d.dvt_pp_gov_group,
                &ppu.dvt_pp_gov_group,
                "dvt_pp_gov_group",
            ),
            (
                &d.dvt_treasury_withdrawal,
                &ppu.dvt_treasury_withdrawal,
                "dvt_treasury_withdrawal",
            ),
            (
                &d.pvt_motion_no_confidence,
                &ppu.pvt_motion_no_confidence,
                "pvt_motion_no_confidence",
            ),
            (
                &d.pvt_committee_normal,
                &ppu.pvt_committee_normal,
                "pvt_committee_normal",
            ),
            (
                &d.pvt_committee_no_confidence,
                &ppu.pvt_committee_no_confidence,
                "pvt_committee_no_confidence",
            ),
            (&d.pvt_hard_fork, &ppu.pvt_hard_fork, "pvt_hard_fork"),
            (
                &d.pvt_pp_security_group,
                &ppu.pvt_pp_security_group,
                "pvt_pp_security_group",
            ),
        ] {
            rat_eq(got, want, what);
        }

        // Babbage-only PPU keys 12-15 must NOT appear in a Conway encoding.
        assert_eq!(d.min_utxo_value, None, "min_utxo_value is Babbage-only");
        assert_eq!(d.d, None, "decentralization is Babbage-only");
        assert_eq!(d.extra_entropy, None, "extra_entropy is Babbage-only");
        assert_eq!(
            d.protocol_version_major, None,
            "Conway moves protocol version to HardForkInitiation"
        );
        assert_eq!(d.protocol_version_minor, None);
    }
}
