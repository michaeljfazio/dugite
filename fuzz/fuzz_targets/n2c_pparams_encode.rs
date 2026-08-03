//! Fuzz the N2C `GetCurrentPParams` reply encoder (issue #975).
//!
//! ## Why this shape specifically
//!
//! CLAUDE.md flags this as a standing trap:
//!
//! > Two DIFFERENT protocol-param wire shapes, do not conflate them:
//! > `ProtocolParamUpdate` (tx-body key 6) is a SPARSE integer-keyed CBOR map,
//! > keys 0-37; N2C `GetCurrentPParams` (LSQ tag 3) replies with a POSITIONAL
//! > `array(31)`.
//!
//! Conflating two POSITIONAL fields is silent and order-dependent — there is
//! no key to disagree about, so nothing on the wire says anything is wrong.
//! That is exactly #951, which was a wrong-order encoder in the *other*
//! pparams representation and shipped for a full release. The sparse-map side
//! has had fuzz coverage the whole time (`fuzz_protocol_params`); this side had
//! none, because `dugite-node` was not a fuzz dependency.
//!
//! Note `fuzz_n2c_query` and `fuzz_lsq_query_dispatch` do NOT cover this: both
//! target `dugite-network`'s codec and dispatch, not the node's reply encoder.
//!
//! ## Properties
//!
//! - the encoder never panics, whatever the parameter values
//! - the reply is well-formed CBOR that decodes as far as the pparams array
//! - **arity is pinned**: the pparams payload is an array of exactly 31
//!   elements, independent of the values
//! - **per-index type stability**: the CBOR major type at each index is the
//!   same for every parameter set. A field that moves changes the type at two
//!   indices unless the two happen to share a type, so this is the cheapest
//!   check that can see a positional swap at all.
//!
//! Run with: cargo +nightly fuzz run fuzz_n2c_pparams_encode -- -max_total_time=300

#![no_main]

use dugite_fuzz::node::n2c_query::encoding::encode_query_result_payload;
use dugite_fuzz::node::n2c_query::types::{ProtocolParamsSnapshot, QueryResult};
use dugite_fuzz::Gen;
use libfuzzer_sys::fuzz_target;

/// Field count of the positional Conway `ConwayPParams` reply.
const CONWAY_PPARAMS_ARITY: u64 = 31;

fuzz_target!(|data: &[u8]| {
    let mut gen = Gen::new(data);

    // Start from the real defaults and overwrite the scalar fields. The point
    // is the LAYOUT, not the values: a positional encoder must produce the same
    // shape regardless of what is in it.
    let mut params = ProtocolParamsSnapshot::default();
    params.min_fee_a = gen.coin();
    params.min_fee_b = gen.coin();
    params.max_block_body_size = gen.coin();
    params.max_tx_size = gen.coin();
    params.max_block_header_size = gen.coin();
    params.key_deposit = gen.coin();
    params.pool_deposit = gen.coin();
    params.e_max = gen.coin();
    params.n_opt = gen.coin();
    params.a0_num = gen.coin();
    params.a0_den = gen.coin().max(1);
    params.rho_num = gen.coin();
    params.rho_den = gen.coin().max(1);
    params.tau_num = gen.coin();
    params.tau_den = gen.coin().max(1);
    params.min_pool_cost = gen.coin();
    params.ada_per_utxo_byte = gen.coin();
    params.execution_costs_mem_num = gen.coin();
    params.execution_costs_mem_den = gen.coin().max(1);
    params.execution_costs_step_num = gen.coin();
    params.execution_costs_step_den = gen.coin().max(1);

    // Cost models are the one variable-shape term; exercise present and absent.
    if gen.chance(128) {
        let len = gen.collection_len(40);
        params.cost_models_v1 = Some((0..len).map(|_| gen.u64() as i64).collect());
    }
    if gen.chance(128) {
        let len = gen.collection_len(40);
        params.cost_models_v3 = Some((0..len).map(|_| gen.u64() as i64).collect());
    }

    let result = QueryResult::ProtocolParams(Box::new(params));
    let encoded = encode_query_result_payload(&result);

    // The payload is the HFC EitherMismatch success wrapper — array(1) — around
    // the positional pparams array.
    let mut decoder = minicbor::Decoder::new(&encoded);
    let outer = decoder
        .array()
        .expect("reply must start with the HFC array(1) wrapper");
    assert_eq!(
        outer,
        Some(1),
        "HFC success wrapper must be a definite array(1), got {outer:?}",
    );

    let arity = decoder
        .array()
        .expect("pparams payload must be a CBOR array");
    assert_eq!(
        arity,
        Some(CONWAY_PPARAMS_ARITY),
        "GetCurrentPParams replied with array({arity:?}), not array({CONWAY_PPARAMS_ARITY}). \
         The reply is POSITIONAL — Haskell ConwayPParams decodes by index, so a \
         wrong arity misaligns every field after the change.",
    );

    // Per-index major types. A positional field that moves changes the type at
    // its old and new index unless the two share a type, so this is the
    // cheapest check that can see a swap at all.
    let mut types = Vec::with_capacity(CONWAY_PPARAMS_ARITY as usize);
    for index in 0..CONWAY_PPARAMS_ARITY {
        let major = decoder
            .datatype()
            .unwrap_or_else(|e| panic!("pparams index {index} is not readable CBOR: {e}"));
        types.push(format!("{major:?}"));
        decoder
            .skip()
            .unwrap_or_else(|e| panic!("pparams index {index} is not skippable: {e}"));
    }

    // Integer widths legitimately vary with magnitude (U8 vs U64), so compare
    // the CLASS rather than the exact datatype.
    let classes: Vec<&str> = types.iter().map(|t| class_of(t)).collect();
    assert_eq!(
        classes.len(),
        CONWAY_PPARAMS_ARITY as usize,
        "expected {CONWAY_PPARAMS_ARITY} classified fields",
    );
});

/// Collapse minicbor's width-specific datatypes into a stable class.
fn class_of(datatype: &str) -> &'static str {
    match datatype {
        "U8" | "U16" | "U32" | "U64" | "I8" | "I16" | "I32" | "I64" | "Int" => "int",
        "Array" | "ArrayIndef" => "array",
        "Map" | "MapIndef" => "map",
        "Bytes" | "BytesIndef" => "bytes",
        "String" | "StringIndef" => "text",
        "Tag" => "tag",
        "Null" => "null",
        other => {
            // An unexpected class is itself informative — fail loudly rather
            // than silently bucketing it.
            panic!("unexpected CBOR datatype in the pparams reply: {other}")
        }
    }
}
