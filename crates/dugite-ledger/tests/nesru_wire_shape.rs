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

/// The `Pulser`'s four fields, and which of them is the live fold state.
///
/// ```text
/// pulser = array(4)[ pulseSize, FreeVars, balance, RewardAns ]
///                      0x01       0x84     0xb3     0x82
///                                 4688 B   1369 B   145 B
///
///   FreeVars  = array(4)[ fvAddrsRew   tag-258 set, 140 items
///                       , fvTotalStake uint 54003425994184880
///                       , fvProtVer    array(2)
///                       , fvPoolRewardInfo map(1) ]
///   balance   = map(19)  Credential -> StakeWithDelegation   <- THE WORK QUEUE
///                        (array(2)[CompactForm Coin, pool KeyHash] — NOT a
///                        bare CompactCoin; oracle-verified against
///                        `Cardano.Ledger.State.Stake`, corrected from an
///                        earlier draft of this comment)
///   RewardAns = array(2)[ map(1), map(1) ]           <- the answer so far
/// ```
///
/// **An earlier version of this test called the tag-258 set at `FreeVars[0]`
/// "the work queue". It is not** — it is `fvAddrsRew`, the registered-accounts
/// prefilter, and it holds **140** entries where the real queue holds **19**.
/// The two are both credential sets and both sit inside the pulser, which is
/// what makes the misreading easy; the counts are what separate them. 140 is
/// the 120 credentials seeded into the LIVE registration set plus the genesis
/// accounts, while 19 is what the GO snapshot actually carries in epoch 2 —
/// delegations registered in epoch 0 reach `go` only after two boundaries, so
/// the fold at epoch 2 still covers only the genesis credentials.
///
/// That divergence is the check: a set whose size tracks live registrations
/// cannot be the queue of a fold over a frozen snapshot.
///
/// The conclusion the misreading was cited for survives it, and is in fact
/// stronger. BOTH `balance` (work remaining) and `RewardAns` (answer so far)
/// are live fold state — which is why the plan that put the wire arms in
/// Phase 2 and pulsing in Phase 3 was wrong, for two reasons rather than one.
///
/// **UPDATE (#1071): incremental pulsing IS production-wired now** —
/// `reward_pulser::RewardFold`/`InFlightFold`, pulsed per block in
/// `apply.rs`, differential-tested against a batch fold. The precondition
/// this paragraph originally described no longer blocks the `Pulsing` arm.
/// What still does is narrower: the Pulser's `FreeVars.fvPoolRewardInfo` is a
/// `Map (KeyHash StakePool) PoolRewardInfo`, and `PoolRewardInfo.poolPs` is
/// `StakePoolSnapShot` — a DIFFERENT, 10-field derived record from
/// `PoolParams`, oracle-verified — that dugite's own `PoolRewardInfo` struct
/// does not carry at all; `balance` needs `StakeWithDelegation`-keyed
/// entries, not a bare stake amount; and `RewardAns` needs `Reward`-typed
/// entries. None of that is missing because the FOLD doesn't run yet — it is
/// missing because nothing computes or stores those specific shapes anywhere,
/// live or persisted. `encode_possible_reward_update`
/// (`dugite-node/src/node/n2c_query/encoding.rs`) implements `SNothing` and
/// `Complete` fully; `Pulsing` still falls back to `SNothing` rather than
/// fabricate that structure, and says so at the call site.
#[test]
fn the_pulser_carries_live_fold_state_not_a_summary() {
    let b = fixture("pulsing.hex");
    // 81 83 00, then an 1178-byte RewardSnapShot, then the pulser.
    let pulser = &b[1181..];
    assert_eq!(pulser[0], 0x84, "Pulser is array(4)");
    assert_eq!(
        pulser[1], 0x01,
        "pulseSize = max 1 (ceil(creds/4k)) = 1 below 4k=160"
    );
    assert_eq!(pulser[2], 0x84, "FreeVars is array(4)");
    assert_eq!(
        &pulser[3..7],
        &[0xd9, 0x01, 0x02, 0x9f],
        "FreeVars[0] is fvAddrsRew, a tag-258 indefinite set — the pv<=6 \
         registration prefilter, NOT the fold's work queue"
    );
    // The queue is the THIRD pulser field, a definite map of Credential ->
    // CompactCoin. Locate it by walking past FreeVars (4688 bytes from its
    // header at pulser[2]).
    let balance = &pulser[2 + 4688..];
    assert_eq!(
        balance[0], 0xb3,
        "the balance is map(19) — the credentials still to fold. A tag-258 \
         set header here would mean fvAddrsRew was misidentified as the queue"
    );
    let ans = &balance[1369..];
    assert_eq!(
        ans[0], 0x82,
        "RewardAns is array(2), the answer accumulated so far"
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
