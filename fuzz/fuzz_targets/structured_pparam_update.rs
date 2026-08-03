//! Encode-first round-trip over `ProtocolParamUpdate` keys 0-37 (issue #974).
//!
//! ## Why this target exists
//!
//! #951: the PPU key 26 encoder wrote the ten `drep_voting_thresholds`
//! elements in the WRONG ORDER. It dropped `constitution` from index 3,
//! shifted six up, and appended it at index 9 — where Haskell puts
//! `treasuryWithdrawal`. The DECODER had always been correct (it matches
//! `EncCBOR DRepVotingThresholds` exactly), so a dugite-built
//! `ParameterChange` installed the wrong governance thresholds: the very
//! values that decide whether a governance action passes.
//!
//! Nothing in the fuzz harness could reach it. `fuzz_protocol_params` exists,
//! but it is decode-first: a byte mutator must first synthesise a CBOR map
//! carrying key 26 with a well-formed ten-element array of tag-30 rationals
//! before the encoder runs at all. Starting from a corpus of real blocks —
//! none of which contain a ParameterChange proposal — that never happened.
//!
//! Generating the structure inverts it. A permutation between encoder and
//! decoder is detected the moment two of the ten thresholds differ, which the
//! generator arranges by drawing all ten independently.
//!
//! ## Properties
//!
//! - **self-decodability** `ppu_from_cbor(encode(ppu))` must succeed
//! - **structural fixpoint** the decoded update equals the generated one,
//!   field for field — this is what separates a wrong ORDER from a wrong value
//! - **idempotence** re-encoding the decoded update reproduces the bytes
//!
//! ## Caveat
//!
//! A same-process round-trip cannot catch a wrong order shared by BOTH halves.
//! #951 was caught only because the two disagreed. Haskell-derived fixtures
//! remain the oracle; this raises reachability.
//!
//! Run with: cargo +nightly fuzz run fuzz_structured_pparam_update -- -max_total_time=300

#![no_main]

use dugite_fuzz::{Gen, PpuShape};
use dugite_primitives::transaction::ProtocolParamUpdate;
use dugite_serialization::decode::{ppu_from_cbor, pre_conway_ppu_from_cbor};
use dugite_serialization::SerializationError;
use dugite_serialization::{encode_pre_conway_protocol_param_update, encode_protocol_param_update};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut gen = Gen::new(data);

    // Conway+ key set: 0-11, 16-33, plus the Dijkstra additions 34-37.
    let conway = gen.ppu_for(PpuShape::Conway);
    round_trip(
        "Conway",
        &conway,
        encode_protocol_param_update,
        ppu_from_cbor,
    );

    // Shelley..Babbage key set: 0-24, including 12 (d), 13 (extra_entropy),
    // 14 (protocol_version) and 15 (min_utxo_value). These four have no
    // counterpart in the Conway type, and until this target existed nothing
    // encoded them at all — tx-body key 6 carried a decoded update proposal
    // straight into a re-encode that dropped it, changing the transaction id.
    let pre_conway = gen.ppu_for(PpuShape::PreConway);
    round_trip(
        "pre-Conway",
        &pre_conway,
        encode_pre_conway_protocol_param_update,
        pre_conway_ppu_from_cbor,
    );
});

fn round_trip(
    label: &str,
    ppu: &ProtocolParamUpdate,
    encode: fn(&ProtocolParamUpdate) -> Vec<u8>,
    decode: fn(&[u8]) -> Result<ProtocolParamUpdate, SerializationError>,
) {
    let encoded = encode(ppu);

    let decoded = match decode(&encoded) {
        Ok(p) => p,
        Err(e) => panic!(
            "{label}: the PPU encoder emitted bytes its own decoder rejects: {e}\n\
             This is the #948 shape — encoder and decoder disagreeing on a \
             field's wire form — applied to the sparse PPU map.\n\
             ppu     = {ppu:#?}\n\
             encoded = {}",
            hex(&encoded),
        ),
    };

    assert!(
        *ppu == decoded,
        "{label}: ProtocolParamUpdate did not survive encode -> decode intact.\n\
         A field that comes back holding ANOTHER field's value is the #951 \
         shape: keys 25 and 26 are POSITIONAL arrays of 5 and 10 thresholds, \
         so a wrong write order is invisible to any per-key check.\n\
         A field that comes back None is the tx-body key 6 shape: decoded, \
         then dropped by an encoder with no arm for it.\n\
         before = {ppu:#?}\n\
         after  = {decoded:#?}",
    );

    let re_encoded = encode(&decoded);
    assert!(
        encoded == re_encoded,
        "{label}: encoder is not idempotent for ProtocolParamUpdate\n\
         first  = {}\n\
         second = {}",
        hex(&encoded),
        hex(&re_encoded),
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
