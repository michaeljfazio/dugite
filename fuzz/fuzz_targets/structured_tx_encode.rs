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

/// Every era `encode_transaction` can emit, with its HFC id from
/// `decode_transaction`'s dispatch table.
///
/// Byron is excluded: its standalone form is `tag(30, bstr(...))` and there is
/// no Byron encoder, so P1-P3 do not apply.
///
/// Sweeping all of them is not thoroughness for its own sake — the era-specific
/// body fields are exactly the ones that sat unfuzzed. Key 6 (`update`) is
/// pre-Conway only and had no encoder arm at all; `sub_transactions`,
/// `account_balance_intervals`, `direct_deposits` and `guards` are Dijkstra
/// only and were never generated.
const ENCODABLE_ERAS: [(Era, u16); 7] = [
    (Era::Shelley, 1),
    (Era::Allegra, 2),
    (Era::Mary, 3),
    (Era::Alonzo, 4),
    (Era::Babbage, 5),
    (Era::Conway, 6),
    (Era::Dijkstra, 7),
];

fuzz_target!(|data: &[u8]| {
    let mut gen = Gen::new(data);

    // One era per input, selected from the entropy stream: generating all seven
    // per run would spend most of the budget re-encoding near-identical bodies.
    let (era, era_id) = ENCODABLE_ERAS[(gen.byte() as usize) % ENCODABLE_ERAS.len()];
    let tx = gen.transaction(era);

    let encoded = encode_transaction(&tx);

    // P1 — self-decodability. Hard failure, never a skip.
    let decoded = match decode_transaction(era_id, &encoded) {
        Ok(t) => t,
        Err(e) => panic!(
            "era {era:?}: encode_transaction emitted bytes its OWN decoder rejects: {e}\n\
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
        "era {era:?}: encoder is not idempotent — E(D(E(tx))) != E(tx)\n\
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
        "era {era:?}: generated transaction did not survive encode -> decode intact\n\
         before = {original:#?}\n\
         after  = {round_tripped:#?}",
    );
});

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
