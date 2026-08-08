//! The `nesRu` wire shape, pinned to cardano-node 11.0.1 captures.
//!
//! `NewEpochState[4]` is `StrictMaybe PulsingRewUpdate`, and its three states
//! have DIFFERENT arities — which is the detail a types-only reading gets
//! wrong, and the reason these are captures rather than derivations:
//!
//! ```text
//! SNothing              80                    array(0)
//! SJust (Pulsing s p)   81 83 00 88 ...       array(1)[ array(3)[0, snapshot(8), pulser] ]
//! SJust (Complete r)    81 82 01 85 ...       array(1)[ array(2)[1, update(5)] ]
//! ```
//!
//! #1057 (`SnapShot`) and #1067 (`NonMyopic`) each produced a plausible WRONG
//! shape from `deriving EncCBOR`. Flattening `Pulsing`'s `array(3)` to an
//! `array(2)` to match `Complete` is exactly that class of mistake, and this
//! test exists to make it fail loudly.
//!
//! Fixtures live in `tests/fixtures/nesru/` with the capture procedure and the
//! ~100-credential threshold that makes `Pulsing` observable.

use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    // CARGO_MANIFEST_DIR is crates/dugite-ledger; fixtures are repo-root-relative.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .join("tests/fixtures/nesru")
        .join(name);
    let hex =
        std::fs::read_to_string(&root).unwrap_or_else(|e| panic!("read {}: {e}", root.display()));
    hex::decode(hex.trim()).expect("fixture hex")
}

#[test]
fn snothing_is_array0() {
    assert_eq!(fixture("snothing.hex"), vec![0x80]);
}

/// `Pulsing` is `array(3)` — sum tag, `RewardSnapShot`, `Pulser`.
///
/// The `array(8)` for `RewardSnapShot` pins its field count:
/// `rewFees`, `rewProtocolVersion`, `rewNonMyopic`, `rewDeltaR1`, `rewR`,
/// `rewDeltaT1`, `rewLikelihoods`, `rewLeaders`.
#[test]
fn pulsing_is_sjust_array3_with_an_8_field_snapshot() {
    let b = fixture("pulsing.hex");
    assert_eq!(b[0], 0x81, "SJust wraps in array(1)");
    assert_eq!(
        b[1], 0x83,
        "Pulsing is array(3), NOT array(2) like Complete"
    );
    assert_eq!(b[2], 0x00, "sum tag 0 = Pulsing");
    assert_eq!(b[3], 0x88, "RewardSnapShot is array(8)");
    assert!(
        b.len() > 1000,
        "a real Pulsing snapshot carries likelihoods and leader rewards; \
         got {} bytes, which suggests the capture missed the window",
        b.len()
    );
}

/// `Complete` is `array(2)` — sum tag and `RewardUpdate`, whose 5 fields are
/// `deltaT`, `deltaR`, `rs`, `deltaF`, `nonMyopic`.
#[test]
fn complete_is_sjust_array2_with_a_5_field_update() {
    for name in ["complete.hex", "complete-nonzero.hex"] {
        let b = fixture(name);
        assert_eq!(b[0], 0x81, "{name}: SJust wraps in array(1)");
        assert_eq!(b[1], 0x82, "{name}: Complete is array(2)");
        assert_eq!(b[2], 0x01, "{name}: sum tag 1 = Complete");
        assert_eq!(b[3], 0x85, "{name}: RewardUpdate is array(5)");
    }
}

/// The `Pulser` is the REMAINING FOLD STATE, and it dominates the record.
///
/// This is the measurement that says the `Pulsing` wire arm cannot be built
/// before incremental pulsing exists. `array(3)[2]` is not a summary or a
/// handful of parameters — it is 84% of the bytes, and its second field is a
/// tagged indefinite set of the credentials still to be folded:
///
/// ```text
/// pulser = array(4)[ pulseSize, <4688 bytes>, <1369>, <145> ]
///                      0x01        0x84         0xb3    0x82
///                                   └─ array(4) whose head is the work queue:
///                                      d9 0102 9f 8200581c…  (tag-258 indefinite set)
/// ```
///
/// The queue sits one level deeper than a first read of the hex suggests —
/// `84 01 84 d90102 9f`, not `84 01 d90102 9f`. That correction came from the
/// fixture rejecting the assertion, which is the whole reason to write the
/// check against captured bytes rather than against the type signature.
///
/// `pulseSize = 1` here because `max 1 (ceil(creds / 4k))` stays 1 below
/// 4k = 160, which is why ~100 credentials were needed to make `Pulsing`
/// observable at all on the devnet.
///
/// Emitting this byte-exactly requires knowing WHICH credentials remain, in
/// `Set` order, at the queried slot — i.e. the pulser must actually be pulsing.
/// A node that computes the whole update at the boundary has no such state to
/// report, so there is nothing to encode from. Phase 2 cannot ship this arm
/// independently of Phase 3; the plan that separated them was wrong.
#[test]
fn the_pulser_carries_remaining_fold_state_not_a_summary() {
    let b = fixture("pulsing.hex");
    // 81 83 00, then an 1178-byte RewardSnapShot, then the pulser.
    let pulser = &b[1181..];
    assert_eq!(pulser[0], 0x84, "Pulser is array(4)");
    assert_eq!(
        pulser[1], 0x01,
        "pulseSize = max 1 (ceil(creds/4k)) = 1 below 4k=160"
    );
    assert_eq!(pulser[2], 0x84, "the fold state is itself an array(4)");
    assert_eq!(
        &pulser[3..7],
        &[0xd9, 0x01, 0x02, 0x9f],
        "the remaining credentials are a tag-258 INDEFINITE set — the fold's \
         work queue, not a summary"
    );
    assert!(
        pulser.len() * 10 > b.len() * 8,
        "the pulser should dominate the record (>80%); got {} of {} bytes. \
         If this shrinks, the capture was taken when the fold was nearly done \
         and understates what the arm must reproduce",
        pulser.len(),
        b.len()
    );
}

/// The two arms must not be conflated: different arity, different tag.
///
/// Emitting `Pulsing` with `Complete`'s framing would be self-undecodable by
/// cardano-cli — the #948 shape.
#[test]
fn the_two_arms_are_structurally_distinct() {
    let p = fixture("pulsing.hex");
    let c = fixture("complete-nonzero.hex");
    assert_ne!(p[1], c[1], "arity must differ (0x83 vs 0x82)");
    assert_ne!(p[2], c[2], "sum tag must differ (0 vs 1)");
}

/// `RewardSnapShot`'s field ORDER, pinned by an arithmetic identity rather
/// than by position alone.
///
/// Arity checks cannot distinguish three adjacent `u64`s. But `rPot = deltaR1
/// + fees` and `R = rPot - deltaT1`, so with `rewFees = 0` the three satisfy
/// `deltaR1 == R + deltaT1` in exactly one assignment. Reading them in any
/// other order breaks the identity, which is what makes this a real check on
/// the layout that `MonetaryStep` mirrors:
///
/// ```text
/// [0] rewFees = 0
/// [3] rewDeltaR1 = 17989722017445
/// [4] rewR       = 14391777613956
/// [5] rewDeltaT1 =  3597944403489     4 + 5 == 3  ✓
/// ```
#[test]
fn reward_snapshot_field_order_is_fixed_by_the_pot_identity() {
    let b = fixture("pulsing.hex");
    // 81 83 00 88 <f0=00> <f1: 82 ..> <f2: 82 ..> <f3..f5: 1b + 8 bytes each>
    assert_eq!(b[4], 0x00, "[0] rewFees is 0 in this capture");
    let u64_at = |off: usize| {
        assert_eq!(b[off], 0x1b, "expected a 64-bit uint at {off}");
        u64::from_be_bytes(b[off + 1..off + 9].try_into().unwrap())
    };
    // f1 = ProtVer array(2) (3 bytes), f2 = NonMyopic array(2) (11 bytes).
    let f3 = 5 + 3 + 11;
    let (delta_r1, r, delta_t1) = (u64_at(f3), u64_at(f3 + 9), u64_at(f3 + 18));
    assert_eq!(
        delta_r1,
        r + delta_t1,
        "rPot identity broken: deltaR1={delta_r1} R={r} deltaT1={delta_t1}. \
         Either the field order is not (deltaR1, R, deltaT1) or the capture \
         came from an epoch with non-zero fees"
    );
    assert!(
        delta_r1 > 0,
        "a zero expansion would satisfy the identity vacuously"
    );
}

/// The all-zero fixture pins shape but no field widths; the non-zero one must
/// actually exercise them, or a 64-bit/uint width bug hides behind zeros.
#[test]
fn the_nonzero_complete_fixture_exercises_real_widths() {
    let b = fixture("complete-nonzero.hex");
    assert_eq!(
        b[4], 0x1b,
        "deltaT should be a 64-bit uint (0x1b) in the non-zero fixture — if \
         this is 0x00 the capture came from an epoch with no rewards and \
         cannot catch a width bug"
    );
    let zero = fixture("complete.hex");
    assert_eq!(
        zero[4], 0x00,
        "the epoch-0 fixture is deliberately all-zero"
    );
    assert!(
        b.len() > zero.len() * 10,
        "the non-zero capture must carry real rs entries"
    );
}
