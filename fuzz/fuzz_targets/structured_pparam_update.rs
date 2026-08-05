//! Encode-first round-trip over every era's `ProtocolParamUpdate` key set
//! (issue #974), plus unknown-key rejection (issue #1013).
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
//! ## Properties (per era)
//!
//! - **self-decodability** `ppu_from_cbor(encode(ppu), era)` must succeed
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
//! ## Era-gated key sets + unknown-key rejection (#1013)
//!
//! The decoder used to silently SKIP an unrecognized key (Haskell hard-
//! rejects the whole decode — accept-where-Haskell-rejects, the dangerous
//! direction: a Haskell peer rejects the same bytes at decode and never
//! recovers). It also treated the pre-Conway and Conway+ key sets each as one
//! flat "union" range, when the real per-era `eraPParams` lists (oracle-
//! verified) have gaps that differ Shelley/Allegra/Mary vs Alonzo vs Babbage
//! vs Conway vs Dijkstra. `PpuShape` now has one variant per era family
//! (`Gen::ppu_for`), and this target exercises TWO properties per shape:
//! every valid key still round-trips (above), and a key from OUTSIDE that
//! shape's exact key set is REJECTED, not silently accepted or skipped.
//!
//! Run with: cargo +nightly fuzz run fuzz_structured_pparam_update -- -max_total_time=300

#![no_main]

use dugite_fuzz::{Gen, PpuShape};
use dugite_primitives::transaction::ProtocolParamUpdate;
use dugite_primitives::Era;
use dugite_serialization::decode::{ppu_from_cbor, pre_conway_ppu_from_cbor};
use dugite_serialization::SerializationError;
use dugite_serialization::{encode_pre_conway_protocol_param_update, encode_protocol_param_update};
use libfuzzer_sys::fuzz_target;

const SHAPES: [PpuShape; 5] = [
    PpuShape::ShelleyFamily,
    PpuShape::Alonzo,
    PpuShape::Babbage,
    PpuShape::Conway,
    PpuShape::Dijkstra,
];

type PpuEncodeFn = fn(&ProtocolParamUpdate) -> Vec<u8>;
type PpuDecodeFn = fn(&[u8], Era) -> Result<ProtocolParamUpdate, SerializationError>;

fuzz_target!(|data: &[u8]| {
    let mut gen = Gen::new(data);

    for shape in SHAPES {
        let era = shape.era();
        let ppu = gen.ppu_for(shape);
        let (encode, decode): (PpuEncodeFn, PpuDecodeFn) =
            if matches!(shape, PpuShape::Conway | PpuShape::Dijkstra) {
                (encode_protocol_param_update, ppu_from_cbor)
            } else {
                (
                    encode_pre_conway_protocol_param_update,
                    pre_conway_ppu_from_cbor,
                )
            };
        round_trip(shape, era, &ppu, encode, decode);
        reject_unknown_key(&mut gen, shape, era, decode);
    }
});

fn round_trip(
    shape: PpuShape,
    era: Era,
    ppu: &ProtocolParamUpdate,
    encode: fn(&ProtocolParamUpdate) -> Vec<u8>,
    decode: fn(&[u8], Era) -> Result<ProtocolParamUpdate, SerializationError>,
) {
    let label = format!("{shape:?}");
    let encoded = encode(ppu);

    let decoded = match decode(&encoded, era) {
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
    let indefinite_decoded = match decode(&indefinite, era) {
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

/// Issue #1013: a key OUTSIDE `shape`'s exact valid set must be REJECTED —
/// never silently skipped (the pre-fix behavior) and never accepted as if it
/// belonged to a different era's key set.
///
/// The candidate lists below are hand-picked from the SAME oracle-verified
/// per-era table the decoder itself cites (`read_protocol_param_update` /
/// `read_pre_conway_protocol_param_update` doc comments) rather than derived
/// from a second, independent "is this key valid" predicate — a drifted
/// duplicate of the validity rule is exactly the trap #1012's `for_each_*`
/// consolidation removed elsewhere in this codebase, and reintroducing it
/// here (in the harness that is supposed to CATCH drift) would defeat the
/// point.
fn invalid_keys_for(shape: PpuShape) -> &'static [u64] {
    match shape {
        // Shelley/Allegra/Mary: 0-16, no gaps. 17-24 (Plutus-era) and 25+
        // (Conway governance / Dijkstra ref-script) don't exist yet.
        PpuShape::ShelleyFamily => &[17, 18, 20, 22, 24, 25, 30, 33, 34, 37, 999],
        // Alonzo: 0-14, 16-24. Gap: 15 (min_utxo_value).
        PpuShape::Alonzo => &[15, 25, 30, 34, 999],
        // Babbage: 0-11, 14, 16-24. Gaps: 12, 13, 15.
        PpuShape::Babbage => &[12, 13, 15, 25, 34, 999],
        // Conway: 0-11, 16-33. Gaps: 12, 13, 14, 15. 34+ is Dijkstra-only.
        PpuShape::Conway => &[12, 13, 14, 15, 34, 35, 36, 37, 999],
        // Dijkstra (dugite-supported subset): 0-11, 16-37. Same four gaps as
        // Conway; 38/39 exist upstream but dugite has no fields yet.
        PpuShape::Dijkstra => &[12, 13, 14, 15, 38, 39, 999],
    }
}

fn reject_unknown_key(
    gen: &mut Gen<'_>,
    shape: PpuShape,
    era: Era,
    decode: fn(&[u8], Era) -> Result<ProtocolParamUpdate, SerializationError>,
) {
    let candidates = invalid_keys_for(shape);
    let key = candidates[gen.choice(candidates.len() as u8) as usize];

    // {key: 0} — a single-entry map. Rejection must fire on the KEY, before
    // any attempt to decode a value, so the value's own shape is irrelevant;
    // uint(0) is valid CBOR and keeps the fixture minimal.
    let definite = single_entry_ppu_map(key);
    let definite_result = decode(&definite, era);
    assert!(
        definite_result.is_err(),
        "{shape:?}: key {key} must be REJECTED (definite map), got {definite_result:?}\n\
         bytes = {}",
        hex(&definite),
    );

    // Same probe through the indefinite-length path — #1012's lesson is that
    // a fix covering only one framing leaves the other framing's behavior
    // unverified.
    let indefinite = to_indefinite_map(&definite);
    let indefinite_result = decode(&indefinite, era);
    assert!(
        indefinite_result.is_err(),
        "{shape:?}: key {key} must be REJECTED on the indefinite-map path too, \
         got {indefinite_result:?}\nbytes = {}",
        hex(&indefinite),
    );
}

/// Build `{key: 0}` as a definite-length CBOR map(1).
fn single_entry_ppu_map(key: u64) -> Vec<u8> {
    let mut v = vec![0xa1]; // map(1)
    v.extend(encode_cbor_uint(key));
    v.push(0x00); // uint(0) — never actually decoded on the reject path
    v
}

fn encode_cbor_uint(n: u64) -> Vec<u8> {
    if n <= 23 {
        vec![n as u8]
    } else if n <= 0xff {
        vec![0x18, n as u8]
    } else if n <= 0xffff {
        let b = (n as u16).to_be_bytes();
        vec![0x19, b[0], b[1]]
    } else {
        let b = (n as u32).to_be_bytes();
        vec![0x1a, b[0], b[1], b[2], b[3]]
    }
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
