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
