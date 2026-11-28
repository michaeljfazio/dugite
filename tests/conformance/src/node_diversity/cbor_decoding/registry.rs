//! The rule -> decoder registry.
//!
//! One entry per rule of the dataset that dugite has a production decoder for, built on
//! `dugite_serialization::decode::rules` (decode) and `dugite_serialization::encode::rules`
//! (re-encode).  A rule that is NOT in the registry has no decoder; the runner reports every one
//! of its samples as a failure (never a skip), and the guards publish how many rules that is.
//!
//! What goes into an entry, and what does not:
//!
//! * The decoder is the one the production paths use for that item.  Where dugite has no decoder
//!   at the granularity of a rule (it decodes a field only inside its parent structure), the rule
//!   is left out rather than given a decoder written for the harness.
//! * The re-encoder is dugite's own encoder.  A few rules whose only dugite encoder is part of a
//!   parent's encoding use a thin split-out of that encoder (see `encode::rules`).
//! * Every decoded value that can carry preserved input bytes goes through
//!   [`checked_scrub`] before it is encoded.

use std::collections::BTreeMap;

use dugite_primitives::era::Era;
use dugite_serialization::cbor::encode_uint;
use dugite_serialization::decode::rules as dec;
use dugite_serialization::encode::rules as enc;
use dugite_serialization::SerializationError;

use super::scrub::{checked_scrub, NoPreservedBytes, Scrub};
use super::HarnessDefect;
use crate::node_diversity::report::CborEra;

/// Why a sample failed, with the stage that failed (the report's `decode:` / `encode:` prefix).
#[derive(Debug)]
pub enum Failure {
    Decode(String),
    Encode(String),
}

type Decode = Box<dyn Fn(&[u8]) -> Result<(), String>>;
type Reencode = Box<dyn Fn(&[u8]) -> Result<Vec<u8>, Failure>>;

/// A rule's decoder and re-encoder.
pub struct Codec {
    /// Whether the bytes decode (used for samples that must be refused).
    pub decode: Decode,
    /// Decode, clear preserved bytes, re-encode.
    pub reencode: Reencode,
}

/// Run an encoder, turning its panic into an encode failure.  A harness defect keeps unwinding.
fn encoded(f: impl FnOnce() -> Vec<u8>) -> Result<Vec<u8>, Failure> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(bytes) => Ok(bytes),
        Err(payload) => {
            if payload.downcast_ref::<HarnessDefect>().is_some() {
                std::panic::resume_unwind(payload);
            }
            let text = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into());
            Err(Failure::Encode(format!("encoder panicked: {text}")))
        }
    }
}

/// A codec for a value that carries no preserved input bytes.
fn plain<T: NoPreservedBytes + 'static>(
    decode: impl Fn(&[u8]) -> Result<T, SerializationError> + Clone + 'static,
    encode: impl Fn(&T) -> Vec<u8> + 'static,
) -> Codec {
    let d = decode.clone();
    Codec {
        decode: Box::new(move |b| d(b).map(|_| ()).map_err(|e| e.to_string())),
        reencode: Box::new(move |b| {
            let value = decode(b).map_err(|e| Failure::Decode(e.to_string()))?;
            encoded(|| encode(&value))
        }),
    }
}

/// A codec for a value that can carry preserved input bytes: they are cleared (and verified
/// cleared) between decode and encode.
fn scrubbed<T: Scrub + std::fmt::Debug + 'static>(
    decode: impl Fn(&[u8]) -> Result<T, SerializationError> + Clone + 'static,
    encode: impl Fn(&T) -> Vec<u8> + 'static,
) -> Codec {
    let d = decode.clone();
    Codec {
        decode: Box::new(move |b| d(b).map(|_| ()).map_err(|e| e.to_string())),
        reencode: Box::new(move |b| {
            let mut value = decode(b).map_err(|e| Failure::Decode(e.to_string()))?;
            checked_scrub(&mut value);
            encoded(|| encode(&value))
        }),
    }
}

/// Whether decoding `input` as `rule` yields a value that re-emits opaque input bytes: a Byron
/// address keeps its payload as the bytes it was read from and writes them back, so an echo of
/// the input and a rebuild are indistinguishable for it.  Such samples are still scored; they are
/// excluded from the discriminating and echo-proof counts and counted on their own.
pub fn reemits_opaque_input(rule: &str, input: &[u8]) -> bool {
    use super::normalize::{parse, Item};
    rule == "address"
        && matches!(parse(input), Ok(Item::Bytes(b)) if b.first().is_some_and(|h| h >> 4 == 0b1000))
}

/// A codec for a value without the `NoPreservedBytes` statement: only for `address`, whose Byron
/// variant carries opaque input bytes (see [`reemits_opaque_input`]).
fn opaque<T: 'static>(
    decode: impl Fn(&[u8]) -> Result<T, SerializationError> + Clone + 'static,
    encode: impl Fn(&T) -> Vec<u8> + 'static,
) -> Codec {
    let d = decode.clone();
    Codec {
        decode: Box::new(move |b| d(b).map(|_| ()).map_err(|e| e.to_string())),
        reencode: Box::new(move |b| {
            let value = decode(b).map_err(|e| Failure::Decode(e.to_string()))?;
            encoded(|| encode(&value))
        }),
    }
}

/// The registry of one era.
pub fn registry(era: CborEra) -> BTreeMap<&'static str, Codec> {
    let e = match era {
        CborEra::Conway => Era::Conway,
        CborEra::Dijkstra => Era::Dijkstra,
    };
    let mut m: BTreeMap<&'static str, Codec> = BTreeMap::new();

    // ── primitives, by the reader the production decoders use for the field ─────────────────
    for rule in [
        "hash28",
        "addr_keyhash",
        "pool_keyhash",
        "script_hash",
        "policy_id",
        "guardrails_script_hash",
    ] {
        m.insert(rule, plain(dec::hash28, enc::hash28));
    }
    for rule in [
        "hash32",
        "transaction_id",
        "auxiliary_data_hash",
        "script_data_hash",
        "vrf_keyhash",
    ] {
        m.insert(rule, plain(dec::hash32, enc::hash32));
    }
    // Keys and signatures are read by the production header, operational-certificate and witness
    // readers with `read_bytes_owned`, which checks no length (`read_hash::<32>` / `<64>` are
    // length-checked but are not what production uses for these fields).
    for rule in ["vkey", "vrf_vkey", "kes_vkey", "signature"] {
        m.insert(rule, plain(dec::bytes_owned, |b| enc::bytes(b)));
    }
    m.insert("coin", plain(dec::lovelace, enc::lovelace));
    for rule in [
        "epoch",
        "slot",
        "block_number",
        "kes_period",
        "major_protocol_version",
    ] {
        m.insert(rule, plain(dec::uint, |v| encode_uint(*v)));
    }
    m.insert("network_id", plain(dec::network_id, enc::network_id));
    for rule in ["unit_interval", "nonnegative_interval"] {
        m.insert(rule, plain(dec::rational, enc::rational));
    }
    m.insert("vote", plain(dec::vote, enc::vote));
    m.insert("ex_units", plain(dec::ex_units, enc::ex_units));
    m.insert(
        "ex_unit_prices",
        plain(dec::ex_unit_prices, enc::ex_unit_prices),
    );
    m.insert(
        "protocol_version",
        plain(dec::protocol_version, enc::protocol_version),
    );
    m.insert(
        "operational_cert",
        plain(dec::operational_cert, enc::operational_cert),
    );
    // `big_int`, `bounded_bytes` and `%constr<plutus_data>` are NOT registered: dugite has one
    // plutus-data decoder, none per kind, and a kind filter written for the harness would be the
    // decoder under test.  They fail as "no decoder registered".

    // ── blocks, transactions ────────────────────────────────────────────────────────────────
    m.insert("block", scrubbed(move |b| dec::block(b, e), enc::block));
    m.insert("header", scrubbed(dec::block_header, enc::block_header));
    m.insert(
        "transaction",
        scrubbed(move |b| dec::transaction(b, e), enc::transaction),
    );
    m.insert(
        "transaction_body",
        scrubbed(
            move |b| dec::transaction_body(b, e),
            move |v| enc::transaction_body(v, e),
        ),
    );
    m.insert(
        "transaction_witness_set",
        scrubbed(
            move |b| dec::transaction_witness_set(b, e),
            move |v| enc::transaction_witness_set(v, e),
        ),
    );
    m.insert(
        "transaction_input",
        plain(dec::transaction_input, enc::transaction_input),
    );
    m.insert(
        "transaction_output",
        scrubbed(dec::transaction_output, enc::transaction_output),
    );
    m.insert(
        "babbage_transaction_output",
        scrubbed(dec::transaction_output, enc::transaction_output),
    );
    m.insert(
        "alonzo_transaction_output",
        scrubbed(dec::legacy_transaction_output, enc::transaction_output),
    );
    m.insert("value", plain(dec::value, enc::value));
    m.insert("mint", plain(dec::mint, enc::mint));
    m.insert(
        "%multiasset<positive_coin>",
        plain(dec::multiasset, enc::multiasset),
    );
    m.insert(
        "datum_option",
        scrubbed(dec::datum_option, enc::datum_option),
    );
    m.insert("script", plain(dec::script, enc::script));
    m.insert("address", opaque(dec::address, enc::address));

    // ── certificates and governance ─────────────────────────────────────────────────────────
    m.insert("certificate", plain(dec::certificate, enc::certificate));
    for rule in [
        "credential",
        "stake_credential",
        "drep_credential",
        "committee_cold_credential",
        "committee_hot_credential",
    ] {
        m.insert(rule, plain(dec::credential, enc::credential));
    }
    m.insert("drep", plain(dec::drep, enc::drep));
    m.insert("anchor", plain(dec::anchor, enc::anchor));
    m.insert(
        "gov_action_id",
        plain(dec::gov_action_id, enc::gov_action_id),
    );
    m.insert("voter", plain(dec::voter, enc::voter));
    m.insert(
        "voting_procedure",
        plain(dec::voting_procedure, enc::voting_procedure),
    );
    m.insert(
        "voting_procedures",
        plain(dec::voting_procedures, enc::voting_procedures),
    );
    m.insert(
        "proposal_procedure",
        plain(
            move |b| dec::proposal_procedure(b, e),
            enc::proposal_procedure,
        ),
    );
    m.insert(
        "gov_action",
        plain(move |b| dec::gov_action(b, e), enc::gov_action),
    );
    m.insert("constitution", plain(dec::constitution, enc::constitution));
    m.insert(
        "protocol_param_update",
        plain(
            move |b| dec::protocol_param_update(b, e),
            enc::protocol_param_update,
        ),
    );
    m.insert("cost_models", plain(dec::cost_models, enc::cost_models));

    // ── scripts, data, redeemers, auxiliary data ────────────────────────────────────────────
    m.insert(
        "native_script",
        plain(dec::native_script, enc::native_script),
    );
    m.insert("plutus_data", plain(dec::plutus_data, enc::plutus_data));
    m.insert(
        "redeemers",
        plain(move |b| dec::redeemers(b, e), |v| enc::redeemers(v)),
    );
    // Dugite decodes auxiliary data through one decoder for all three shapes (the Shelley map, the
    // Allegra array and the Alonzo tag-259 map).  It has none for a single shape, so
    // `auxiliary_data_array` and `auxiliary_data_map` are left unregistered rather than given the
    // union decoder, which would accept every sample those rules name as invalid.
    m.insert(
        "auxiliary_data",
        scrubbed(move |b| dec::auxiliary_data(b, e), enc::auxiliary_data),
    );
    m.insert("metadatum", plain(dec::metadatum, enc::metadatum));
    m.insert("metadata", plain(dec::metadata, enc::metadata));

    // ── pool parameters ─────────────────────────────────────────────────────────────────────
    m.insert("relay", plain(dec::relay, enc::relay));
    m.insert(
        "pool_metadata",
        plain(dec::pool_metadata, enc::pool_metadata),
    );
    m.insert("withdrawals", plain(dec::withdrawals, enc::withdrawals));
    m.insert("data", plain(dec::data, enc::data));

    // ── per era ─────────────────────────────────────────────────────────────────────────────
    match era {
        CborEra::Conway => {
            m.insert("redeemer", plain(dec::redeemer, enc::redeemer));
            m.insert(
                "certificates",
                plain(dec::certificates, |v| enc::certificates(v)),
            );
            m.insert(
                "proposal_procedures",
                plain(
                    move |b| dec::proposal_procedures(b, e),
                    |v| enc::proposal_procedures(v),
                ),
            );
        }
        CborEra::Dijkstra => {
            m.insert(
                "sub_transaction_body",
                scrubbed(dec::sub_transaction_body, enc::sub_transaction_body),
            );
            m.insert(
                "account_balance_interval",
                plain(dec::account_balance_interval, enc::account_balance_interval),
            );
            m.insert(
                "account_balance_intervals",
                plain(dec::account_balance_intervals, |v| {
                    enc::account_balance_intervals(v)
                }),
            );
        }
    }
    m
}
