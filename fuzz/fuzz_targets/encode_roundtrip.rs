//! Fuzz target for CBOR encoder/decoder round-trip soundness.
//!
//! ## Why this target was rewritten (issue #973)
//!
//! v2.4.1 through v2.4.5 were almost entirely CBOR **encoder** defects —
//! #930, #932, #938, #939, #940, #947, #948, #951, #952. This target ran
//! nightly throughout and caught none of them. Three independent structural
//! defects explained it, each sufficient on its own:
//!
//! 1. `if let Ok(re_decoded) = decode_transaction(...)` — encoder output that
//!    FAILED TO DECODE silently skipped every assertion. That is exactly
//!    #948 (`encode_drep` wrote a 32-byte DRep KeyHash where `read_drep`
//!    demands 28, so dugite's own output was self-undecodable) and #932's
//!    `encode_voter` StakePool arm.
//! 2. `if encoded.as_slice() == data` gated the hash assertion on the
//!    re-encode being byte-identical to the input — i.e. it only fired when
//!    the encoder was already correct, and disarmed itself in precisely the
//!    case it existed to catch.
//! 3. It compared 6 of 24 `TransactionBody` fields — and `outputs.len()`, the
//!    count, not the contents. `update`, `required_signers`,
//!    `proposal_procedures`, `voting_procedures`, `withdrawals`, the entire
//!    `witness_set` and `auxiliary_data` were never looked at. Every one of
//!    those is where one of the bugs above lived.
//!
//! ## The properties now asserted
//!
//! For a transaction the decoder accepted, with `E` = `encode_transaction`
//! and `D` = `decode_transaction`:
//!
//! - **P1 (self-decodability)** `D(E(tx))` must succeed. An encoder that
//!   emits bytes its own decoder rejects is always a bug, with no exception —
//!   so this is a hard failure, not a skipped branch. This is the #948 shape.
//! - **P2 (structural fixpoint)** `D(E(tx)) == tx`, comparing the WHOLE
//!   `Transaction` rather than a hand-picked subset. This is the #951 shape:
//!   the PPU key 26 encoder wrote `drep_voting_thresholds` in the wrong order,
//!   so a field written at index 3 came back at index 9 — invisible to a
//!   6-field check, immediate under whole-struct equality.
//! - **P3 (idempotence)** `E(D(E(tx))) == E(tx)`. Replaces the old
//!   byte-identity gate. It expresses the legitimate intent — a non-canonical
//!   INPUT may canonicalise on re-encode — without the flaw, because it makes
//!   no reference to the input bytes and therefore cannot be disarmed by the
//!   encoder being wrong.
//! - **P4 (hash stability)** when the encoder did reproduce the input
//!   byte-for-byte, the transaction id must be unchanged. Kept from the
//!   original target; it is sound, it was just the only check that could fire.
//!
//! ## What this cannot catch
//!
//! A same-process round-trip is necessary but NOT sufficient: a wrong shape
//! shared by BOTH halves round-trips perfectly. #951 was caught only because
//! the encoder and decoder disagreed — the decoder had always been right. The
//! durable oracle is a Haskell-derived fixture, not this target. See the
//! caveat pinned in the v2.5.0 tests.
//!
//! Run with: cargo +nightly fuzz run fuzz_encode_roundtrip -- -max_total_time=300

#![no_main]

use dugite_fuzz::normalise_for_comparison;
use dugite_primitives::era::Era;
use dugite_primitives::transaction::Transaction;
use dugite_serialization::{decode_block, decode_transaction, encode_transaction};
use libfuzzer_sys::fuzz_target;

/// Byron standalone transactions are `tag(30, bstr(...))` on the wire, a shape
/// `encode_transaction` does not produce — it emits the Alonzo-family
/// `[body, witness_set, is_valid, aux_data]` array for every pre-Dijkstra era.
/// There is no Byron encoder, so P1-P3 do not apply to era 0. Decoding is
/// still exercised (that is `fuzz_byron_block_decode`'s remit and
/// `fuzz_decode_transaction`'s).
const FIRST_ENCODABLE_ERA: u16 = 1;

/// Highest era id `decode_transaction` accepts (7 = Dijkstra, CIP-0167).
const LAST_ERA: u16 = 7;

/// Assert P1-P3 for one decoded transaction.
fn assert_encoder_is_sound(era_id: u16, tx: &Transaction, ctx: &str) {
    let encoded = encode_transaction(tx);

    // P1 — self-decodability. Hard failure.
    let mut re_decoded = match decode_transaction(era_id, &encoded) {
        Ok(t) => t,
        Err(e) => panic!(
            "{ctx}: encode_transaction emitted bytes its OWN decoder rejects: {e}\n\
             This is the #948 shape (encoder and decoder disagree on a field's \
             wire width or framing) and is always a bug.\n\
             encoded ({} bytes) = {}",
            encoded.len(),
            hex(&encoded),
        ),
    };

    // P3 — idempotence. Note this deliberately does NOT compare against the
    // input bytes: a non-canonical input canonicalising is legal, an encoder
    // that cannot reproduce its own output is not.
    //
    // `assert!` rather than `assert_eq!` so the hex formatting is paid only on
    // failure — this runs on every fuzz iteration.
    let re_encoded = encode_transaction(&re_decoded);
    assert!(
        encoded == re_encoded,
        "{ctx}: encoder is not idempotent — E(D(E(tx))) != E(tx)\n\
         first  = {}\n\
         second = {}",
        hex(&encoded),
        hex(&re_encoded),
    );

    // P2 — structural fixpoint over the whole transaction.
    let mut original = tx.clone();
    normalise_for_comparison(&mut original);
    normalise_for_comparison(&mut re_decoded);
    assert!(
        original == re_decoded,
        "{ctx}: transaction did not survive encode -> decode intact\n\
         before = {original:#?}\n\
         after  = {re_decoded:#?}",
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn era_id_of(era: Era) -> u16 {
    match era {
        Era::Byron => 0,
        Era::Shelley => 1,
        Era::Allegra => 2,
        Era::Mary => 3,
        Era::Alonzo => 4,
        Era::Babbage => 5,
        Era::Conway => 6,
        Era::Dijkstra => 7,
    }
}

fuzz_target!(|data: &[u8]| {
    // Test 1 — standalone transaction round-trip, every encodable era.
    for era_id in FIRST_ENCODABLE_ERA..=LAST_ERA {
        if let Ok(tx) = decode_transaction(era_id, data) {
            assert_encoder_is_sound(era_id, &tx, &format!("standalone tx (era {era_id})"));

            // P4 — hash stability when the encoder reproduced the input
            // exactly. Sound as written; it was simply the only assertion the
            // old target could reach.
            let encoded = encode_transaction(&tx);
            if encoded.as_slice() == data {
                let re_decoded =
                    decode_transaction(era_id, &encoded).expect("P1 already proved this decodes");
                assert_eq!(
                    tx.hash, re_decoded.hash,
                    "era {era_id}: transaction id changed across a byte-identical \
                     round-trip",
                );
            }
        }
    }

    // Test 2 — every transaction inside a decoded block.
    //
    // Block-embedded transactions reach shapes a standalone decode does not:
    // the block decoder splits body / witness / auxiliary segments and
    // reassembles them, so a tx that only ever appears inside a block still
    // gets its encoder audited here.
    if let Ok(block) = decode_block(data) {
        let era_id = era_id_of(block.era);
        if era_id >= FIRST_ENCODABLE_ERA {
            for (i, tx) in block.transactions.iter().enumerate() {
                assert_encoder_is_sound(era_id, tx, &format!("block tx {i} (era {era_id})"));

                let encoded = encode_transaction(tx);
                let original_bytes = tx.raw_cbor.as_deref().unwrap_or(&[]);
                if encoded.as_slice() == original_bytes {
                    let re_decoded = decode_transaction(era_id, &encoded)
                        .expect("P1 already proved this decodes");
                    assert_eq!(
                        tx.hash, re_decoded.hash,
                        "block tx {i}: transaction id changed across a byte-identical \
                         round-trip",
                    );
                }
            }
        }
    }
});
