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
//! ## Indefinite-length map coverage (#1012)
//!
//! `read_protocol_param_update` / `read_pre_conway_protocol_param_update` used
//! to drive their key loop from `read_map_header()?.unwrap_or(0)`, silently
//! decoding an INDEFINITE-length CBOR map as zero entries. dugite's own PPU
//! encoder can never reach that shape to test it: oracle-verified against
//! `IntersectMBO/cardano-ledger`, Haskell's `encCBOR (PParamsUpdate era)` uses
//! `encodeMapLen` (always definite), not the size-dependent `encodeMap`
//! #932/#938 cover — so a `decode(encode(x)) == x` round trip through EITHER
//! implementation's own encoder can never produce an indefinite-length PPU
//! map, and could not have caught this. `to_indefinite_map` mechanically
//! rewrites the (always-definite) encoder output into the indefinite form —
//! the only way this target can reach it — and asserts it decodes to the
//! SAME value across the generator's full input space, not just the one
//! hand-built fixture in `era_conway.rs`'s unit tests.
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

    // #1012: an indefinite-length encoding of the exact same entries must
    // decode to the exact same value. `to_indefinite_map` derives it
    // mechanically from `encoded` rather than re-deriving the key table by
    // hand, so this tracks whatever key set/order the generator produced.
    let indefinite = to_indefinite_map(&encoded);
    let indefinite_decoded = match decode(&indefinite) {
        Ok(p) => p,
        Err(e) => panic!(
            "{label}: indefinite-length PPU map must decode (#1012): {e}\n\
             definite   = {}\n\
             indefinite = {}",
            hex(&encoded),
            hex(&indefinite),
        ),
    };
    assert!(
        decoded == indefinite_decoded,
        "{label}: indefinite-length PPU map decoded to a DIFFERENT value than \
         the definite form (#1012 — the pre-fix decoder silently read this as \
         zero entries).\n\
         definite-decoded   = {decoded:#?}\n\
         indefinite-decoded = {indefinite_decoded:#?}",
    );
}

/// Rewrite a buffer whose first bytes are a DEFINITE-length CBOR map header
/// into the equivalent INDEFINITE-length form (`0xbf` ... `0xff`), leaving
/// every entry byte untouched. `encode_map_header` (`cbor.rs`) only ever
/// emits the 1-, 2-, 3- or 5-byte definite forms for the key counts a PPU can
/// have; the 9-byte form is handled for completeness.
fn to_indefinite_map(definite: &[u8]) -> Vec<u8> {
    let header_len = match definite[0] {
        0xa0..=0xb7 => 1,
        0xb8 => 2,
        0xb9 => 3,
        0xba => 5,
        0xbb => 9,
        other => panic!("not a definite-length CBOR map header: {other:#x}"),
    };
    let mut out = vec![0xbf];
    out.extend_from_slice(&definite[header_len..]);
    out.push(0xff);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
