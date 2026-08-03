//! Encode-first whole-transaction round-trip (issue #974).
//!
//! ## Why this target exists
//!
//! `fuzz_encode_roundtrip` is decode-first: it can only audit the encoder on
//! transactions the fuzzer first synthesised as valid CBOR. That bounds it to
//! the shapes a byte mutator can invent from the corpus, and the corpus is
//! real on-chain blocks — which contain no DRep delegation certificate, no
//! governance proposal, no bootstrap witness, and no `ProtocolParamUpdate`.
//!
//! Every one of those is where a v2.4.1-v2.4.5 encoder defect lived.
//!
//! This target generates the transaction instead, so the deep optional fields
//! are directly addressable rather than something the fuzzer has to discover.
//! It asserts the same properties as `fuzz_encode_roundtrip` — the two are
//! complementary, not redundant: this one reaches shapes no corpus contains,
//! that one reaches real-world encodings no generator would think to produce
//! (indefinite-length maps, legacy output arrays, era quirks).
//!
//! ## Properties
//!
//! - **P1 (self-decodability)** `decode(encode(tx))` must succeed. An encoder
//!   emitting bytes its own decoder rejects is always a bug — the #948 shape.
//! - **P2 (structural fixpoint)** the decoded transaction equals the generated
//!   one. A field that comes back holding a DIFFERENT field's value is the
//!   #951 shape; a field that comes back `None` is the tx-body key 6 shape
//!   (decoded, then dropped by an encoder with no arm for it).
//! - **P3 (idempotence)** re-encoding the decoded transaction reproduces the
//!   bytes exactly.
//!
//! ## Caveat
//!
//! A same-process round-trip cannot catch a wrong shape shared by BOTH halves.
//! Haskell-derived fixtures remain the oracle; this raises reachability.
//!
//! Run with: cargo +nightly fuzz run fuzz_structured_tx_encode -- -max_total_time=300

#![no_main]

use dugite_fuzz::{normalise_for_comparison, Gen};
use dugite_primitives::era::Era;
use dugite_serialization::{decode_transaction, encode_transaction};
use libfuzzer_sys::fuzz_target;

/// HFC era id for Conway, matching `decode_transaction`'s dispatch table.
const CONWAY_ERA_ID: u16 = 6;

fuzz_target!(|data: &[u8]| {
    let mut gen = Gen::new(data);
    let tx = gen.transaction(Era::Conway);

    let encoded = encode_transaction(&tx);

    // P1 — self-decodability. Hard failure, never a skip.
    let decoded = match decode_transaction(CONWAY_ERA_ID, &encoded) {
        Ok(t) => t,
        Err(e) => panic!(
            "encode_transaction emitted bytes its OWN decoder rejects: {e}\n\
             This is the #948 shape (encoder and decoder disagreeing on a \
             field's wire width or framing) and is always a bug.\n\
             tx      = {tx:#?}\n\
             encoded = {}",
            hex(&encoded),
        ),
    };

    // P3 — idempotence, checked before P2 so a byte-level difference reports
    // as bytes rather than as a large struct diff.
    let re_encoded = encode_transaction(&decoded);
    assert!(
        encoded == re_encoded,
        "encoder is not idempotent — E(D(E(tx))) != E(tx)\n\
         first  = {}\n\
         second = {}",
        hex(&encoded),
        hex(&re_encoded),
    );

    // P2 — structural fixpoint over the whole transaction.
    let mut original = tx.clone();
    let mut round_tripped = decoded;
    normalise_for_comparison(&mut original);
    normalise_for_comparison(&mut round_tripped);
    assert!(
        original == round_tripped,
        "generated transaction did not survive encode -> decode intact\n\
         before = {original:#?}\n\
         after  = {round_tripped:#?}",
    );
});

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
