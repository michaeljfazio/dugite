//! Integration coverage for `dugite-primitives::genesis::dijkstra`.
//!
//! Round-trips an upstream-style `dijkstra-genesis.json` fixture through the
//! public API (file load → parse → serialise → re-parse → equality).
//!
//! The fixture is synthesised from
//! `cardano-api/src/Cardano/Api/Genesis/Internal.hs::dijkstraGenesisDefaults`
//! (the same defaults `cardano-cli create-testnet-data` emits as
//! `dijkstra-genesis.json`).  When IntersectMBO publishes a canonical sample
//! file in the cardano-node `configuration/` tree we should swap this fixture
//! for that file.

use dugite_primitives::genesis::{DijkstraGenesis, PositiveInterval};

const UPSTREAM_DEFAULTS_JSON: &str = r#"{
    "maxRefScriptSizePerBlock": 1048576,
    "maxRefScriptSizePerTx": 204800,
    "refScriptCostStride": 25600,
    "refScriptCostMultiplier": 1.2
}"#;

#[test]
fn upstream_defaults_round_trip_via_public_api() {
    let parsed = DijkstraGenesis::from_json_str(UPSTREAM_DEFAULTS_JSON)
        .expect("upstream defaults JSON must parse");

    // Field-by-field equality (cheap and obvious failure messages).
    assert_eq!(parsed.max_ref_script_size_per_block, 1_048_576);
    assert_eq!(parsed.max_ref_script_size_per_tx, 204_800);
    assert_eq!(parsed.ref_script_cost_stride, 25_600);
    assert_eq!(parsed.ref_script_cost_multiplier.numerator(), 6);
    assert_eq!(parsed.ref_script_cost_multiplier.denominator(), 5);

    // The hand-built defaults constructor must agree with the JSON parser
    // byte-exact — otherwise our default values have drifted from upstream.
    assert_eq!(parsed, DijkstraGenesis::defaults());

    // Round-trip: serialise → re-parse → equality preserved.
    let bytes = serde_json::to_vec(&parsed).expect("serialise");
    let reparsed: DijkstraGenesis = DijkstraGenesis::from_json_slice(&bytes).expect("re-parse");
    assert_eq!(reparsed, parsed);
}

#[test]
fn positive_interval_constructor_round_trips() {
    // Defaults: 6/5 (= 1.2). Both string-derived and constructor-derived
    // values must round-trip through serde to the same JSON token.
    let pi = PositiveInterval::new(6, 5).expect("strictly positive");
    let json = serde_json::to_string(&pi).expect("serialise");
    assert_eq!(json, "1.2");
    let back: PositiveInterval = serde_json::from_str(&json).expect("parse");
    assert_eq!(back, pi);
}

#[test]
fn structured_multiplier_object_is_accepted() {
    // Some external tooling may emit the rational in the structured
    // `{numerator,denominator}` form.  This must parse to the same value as
    // the compact `1.2`.
    let json = r#"{
        "maxRefScriptSizePerBlock": 1048576,
        "maxRefScriptSizePerTx": 204800,
        "refScriptCostStride": 25600,
        "refScriptCostMultiplier": { "numerator": 12, "denominator": 10 }
    }"#;
    let parsed = DijkstraGenesis::from_json_str(json).expect("structured form parses");
    assert_eq!(parsed, DijkstraGenesis::defaults());
}
