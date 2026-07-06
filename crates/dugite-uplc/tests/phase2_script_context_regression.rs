//! Regression tests for ScriptContext / flat-decoder bugs that caused
//! phase-2 evaluation failures on real Alonzo mainnet transactions:
//!
//! **Bug A — ADA value policy key (`PlutusValue::to_data`)**
//!   The Ada policy key was emitted as 28 zero bytes instead of the empty
//!   bytestring.  Scripts that inspect the value map (e.g., to check an ADA
//!   output) silently found no entry under b"".  Fixed by using
//!   `Data::B(Vec::new())` when `policy == [0u8; 28]`.
//!
//! **Bug B — `PosixTimeRange::to_data` wrong structure**
//!   The Plutus `Interval POSIXTime` type has a three-layer nesting:
//!   `Interval (LowerBound (Extended POSIXTime) Bool) (UpperBound … Bool)`.
//!   The old code emitted a completely flat pair `Constr 0 [bound, bound]`
//!   without the `LowerBound`/`UpperBound` wrappers or the `Extended`
//!   constructor (`Finite`/`NegInf`/`PosInf`).  Scripts that navigate the
//!   validity range with `unConstrData` immediately got the wrong shape.
//!
//! **Bug C — flat decoder: compound type-tag lists**
//!   The flat constant type-tag list decoder only handled atoms 0-4
//!   (Integer/ByteString/String/Unit/Bool).  Atom 8 (Data), atom 5 (List),
//!   atom 6 (Pair), and the Apply connector atom 7 were all rejected with
//!   `FlatDecode("compound universe tag … not yet wired")`.  Scripts
//!   containing `Data` or `List` constants silently fell through to the
//!   fallback `from_flat(outer_cbor_bytes)` path, which misread the CBOR
//!   header as flat version bytes and later failed with `filler missing
//!   terminating 1 bit within 8 bits`.
//!
//! **Bug D — V1/V2 txInfoWdrl encoding mismatch**
//!
//!   V1: `txInfoWdrl :: [(StakingCredential, Integer)]` — an AssocList,
//!   encoded as `Data::List[Constr 0 [cred, amt]]`.
//!   V2: `txInfoWdrl :: Map StakingCredential Integer` — an AssocMap,
//!   encoded as `Data::Map[(cred, amt)]`.
//!   Both were previously emitted as `Data::List[Constr 0 [cred, amt]]`,
//!   then briefly (incorrectly) fixed as `Data::Map` for both. The correct
//!   fix encodes each version differently per the Haskell schema.
//!
//! **Bug E — TxId not wrapped in its Constr 0 newtype**
//!   `TxOutRef.txId` is `TxId = Constr 0 [B bytes32]` (a newtype with index 0),
//!   NOT bare bytes. Scripts that navigate `txInfoInputs` call `unConstrData`
//!   on the TxId field expecting a constructor; bare bytes caused
//!   `unConstrData on non-Constr Data`. Same issue on `txInfoId`.
//!
//! **Bug F — fee encoded as Integer instead of Value**
//!   `txInfoFee :: Value` — the fee is an ADA-only Plutus Value map
//!   `Map[(b"", Map[(b"", I lovelace)])]`, not a bare `I lovelace`.
//!
//! **Bug G — V1 txInfoData encoded as Map instead of AssocList**
//!   V1 `txInfoData :: [(DatumHash, Datum)]` — encoded as
//!   `Data::List[Constr 0 [B32, datum]]`, NOT `Data::Map`.
//!   V2 correctly uses `Map`.
//!
//! **Bug H — flat decoder depth limit (256) too low for large DeFi scripts**
//!   Real-world DeFi validators (10 KB+) exceed 256 nesting levels. The
//!   depth limit was raised to 4096 and `stacker::maybe_grow` added to
//!   the recursive decoder to prevent stack overflow on deep but valid scripts.

use dugite_uplc::data::Data;
use dugite_uplc::script_context::Address;
use dugite_uplc::script_context::{
    Credential, GovActionId, OutputDatum, PlutusValue, PosixTimeRange, ScriptContextV1,
    ScriptPurpose, StakingCredential, TxInInfo, TxInfoV1, TxInfoV2, TxInfoV3, TxOut, TxOutRef,
};
use num_bigint::BigInt;
use std::rc::Rc;

// ─────────────────────────────────────────────────────────────────────────────
// Bug A: ADA policy key
// ─────────────────────────────────────────────────────────────────────────────

/// `PlutusValue::to_data` for an ADA-only value must use the **empty**
/// bytestring as the policy key, not 28 zero bytes.
///
/// Reference: `PlutusLedgerApi.V1.Value.adaSymbol = CurrencySymbol ""`
#[test]
fn ada_value_policy_key_is_empty_bytestring() {
    // Build an ADA-only PlutusValue (the sentinel 28-zero-byte policy that
    // `value_to_plutus` inserts for the ADA entry).
    let v = PlutusValue {
        policies: vec![([0u8; 28], vec![(Vec::new(), BigInt::from(5_000_000u64))])],
    };
    let d = v.to_data();
    // Must be a Map.
    let Data::Map(entries) = &d else {
        panic!("PlutusValue.to_data() must be Data::Map, got {d:?}");
    };
    assert_eq!(entries.len(), 1, "expected exactly one policy entry");
    // The policy key must be an EMPTY bytestring.
    let (key, _) = &entries[0];
    assert_eq!(
        key,
        &Data::B(Vec::new()),
        "Ada policy key must be b\"\" (empty bytes), not 28 zero bytes; \
         got {key:?}"
    );
}

/// Regression: before the fix, the Ada policy key was 28 zero bytes.
#[test]
fn ada_value_policy_key_is_not_28_zero_bytes() {
    let v = PlutusValue {
        policies: vec![([0u8; 28], vec![(Vec::new(), BigInt::from(1u64))])],
    };
    let d = v.to_data();
    let Data::Map(entries) = &d else {
        panic!("expected Map");
    };
    let (key, _) = &entries[0];
    assert_ne!(
        key,
        &Data::B(vec![0u8; 28]),
        "Ada policy key must NOT be 28 zero bytes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug B: PosixTimeRange encoding
// ─────────────────────────────────────────────────────────────────────────────

/// The always-valid interval (no validity_start, no TTL) must encode as:
/// `Constr 0 [LowerBound, UpperBound]` where:
///   - `LowerBound = Constr 0 [Constr 0 [] {NegInf}, Constr 1 [] {True}]`
///   - `UpperBound = Constr 0 [Constr 2 [] {PosInf}, Constr 1 [] {True}]`
#[test]
fn posix_time_range_always_valid_has_correct_structure() {
    let r = PosixTimeRange {
        lower: None,
        upper: None,
    };
    let d = r.to_data(false);

    // Outer: Interval = Constr 0 [lower_bound, upper_bound]
    let Data::Constr(0, ref outer_fields) = d else {
        panic!("Interval must be Constr 0; got {d:?}");
    };
    assert_eq!(outer_fields.len(), 2, "Interval has 2 fields (lb, ub)");

    // Lower bound: LowerBound NegInf True = Constr 0 [Constr 0 [], Constr 1 []]
    let Data::Constr(0, ref lb_fields) = outer_fields[0] else {
        panic!("LowerBound must be Constr 0; got {:?}", outer_fields[0]);
    };
    assert_eq!(
        lb_fields.len(),
        2,
        "LowerBound has 2 fields (extended, closed)"
    );
    assert_eq!(
        lb_fields[0],
        Data::Constr(0, vec![]),
        "LowerBound(None) extended must be NegInf = Constr 0 []"
    );
    assert_eq!(
        lb_fields[1],
        Data::Constr(1, vec![]),
        "LowerBound closed must be True = Constr 1 []"
    );

    // Upper bound: UpperBound PosInf True = Constr 0 [Constr 2 [], Constr 1 []]
    let Data::Constr(0, ref ub_fields) = outer_fields[1] else {
        panic!("UpperBound must be Constr 0; got {:?}", outer_fields[1]);
    };
    assert_eq!(
        ub_fields.len(),
        2,
        "UpperBound has 2 fields (extended, closed)"
    );
    assert_eq!(
        ub_fields[0],
        Data::Constr(2, vec![]),
        "UpperBound(None) extended must be PosInf = Constr 2 []"
    );
    assert_eq!(
        ub_fields[1],
        Data::Constr(1, vec![]),
        "UpperBound PosInf closed must be True = Constr 1 []"
    );
}

/// A finite validity interval [vs_ms, ttl_ms) must encode as:
///   lower = Constr 0 [Constr 1 [I(vs_ms)], Constr 1 []]   -- Finite, closed
///   upper = Constr 0 [Constr 1 [I(ttl_ms)], Constr 0 []]  -- Finite, open
#[test]
fn posix_time_range_finite_bounds_have_correct_structure() {
    let vs_ms = 1_643_594_463_000i64;
    let ttl_ms = 1_643_598_062_000i64;
    let r = PosixTimeRange {
        lower: Some(vs_ms),
        upper: Some(ttl_ms),
    };
    let d = r.to_data(false);

    let Data::Constr(0, ref outer) = d else {
        panic!("Interval must be Constr 0");
    };

    // Lower: LowerBound (Finite vs_ms) True
    let Data::Constr(0, ref lb) = outer[0] else {
        panic!("LowerBound must be Constr 0");
    };
    assert_eq!(
        lb[0],
        Data::Constr(1, vec![Data::I(BigInt::from(vs_ms))]),
        "lower extended must be Finite vs_ms = Constr 1 [I(vs_ms)]"
    );
    assert_eq!(
        lb[1],
        Data::Constr(1, vec![]),
        "lower closed must be True = Constr 1 []"
    );

    // Upper: UpperBound (Finite ttl_ms) False
    let Data::Constr(0, ref ub) = outer[1] else {
        panic!("UpperBound must be Constr 0");
    };
    assert_eq!(
        ub[0],
        Data::Constr(1, vec![Data::I(BigInt::from(ttl_ms))]),
        "upper extended must be Finite ttl_ms = Constr 1 [I(ttl_ms)]"
    );
    assert_eq!(
        ub[1],
        Data::Constr(0, vec![]),
        "upper closed must be False = Constr 0 [] (half-open upper)"
    );
}

/// #772 regression: a ttl-ONLY validity interval (`invalid_hereafter` set, no
/// `invalid_before`) has an ERA-GATED upper-bound closure. Pre-Conway uses
/// `PV1.to` (INCLUSIVE, closure True); Conway+ uses `strictUpperBound`
/// (EXCLUSIVE, closure False). dugite previously gated this on the script
/// LANGUAGE (always inclusive for V1/V2), over-charging a validity-range-reading
/// Reward redeemer by +1453 cpu vs cardano-node in the Conway era.
#[test]
fn posix_time_range_ttl_only_upper_closure_is_era_gated() {
    let r = PosixTimeRange {
        lower: None,
        upper: Some(1_781_858_645_000i64),
    };
    let upper_closure = |conway: bool| -> Data {
        let Ok((0, outer)) = r.to_data(conway).into_constr() else {
            panic!("interval must be Constr 0");
        };
        let Ok((0, ub)) = outer[1].clone().into_constr() else {
            panic!("upper bound must be Constr 0");
        };
        ub[1].clone()
    };
    // Pre-Conway (Alonzo/Babbage): `PV1.to` → UpperBound (Finite t) True.
    assert_eq!(
        upper_closure(false),
        Data::Constr(1, vec![]),
        "pre-Conway ttl-only upper closure must be True (inclusive, PV1.to)"
    );
    // Conway+: `strictUpperBound` → UpperBound (Finite t) False.
    assert_eq!(
        upper_closure(true),
        Data::Constr(0, vec![]),
        "Conway+ ttl-only upper closure must be False (exclusive, strictUpperBound)"
    );
}

/// Regression: before the fix, bounds were flat Constr 1 [val, unit]
/// without the outer LowerBound/UpperBound Constr 0 wrapper.
#[test]
fn posix_time_range_does_not_use_old_flat_encoding() {
    let r = PosixTimeRange {
        lower: Some(1_000_000i64),
        upper: Some(2_000_000i64),
    };
    let d = r.to_data(false);
    let Data::Constr(0, ref outer) = d else {
        panic!("must be Constr 0");
    };
    // Old encoding: bound(Some(t)) = Constr 1 [I(t), Constr 1 []]
    // New encoding: LowerBound = Constr 0 [Constr 1 [I(t)], Bool]
    // Check that neither field is Constr 1 at the top level (that was the old bug).
    assert!(
        !matches!(outer[0], Data::Constr(1, _)),
        "lower bound must NOT be Constr 1 (old buggy encoding)"
    );
    assert!(
        !matches!(outer[1], Data::Constr(1, _)),
        "upper bound must NOT be Constr 1 (old buggy encoding)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug C: flat decoder — Data constant type tag
// ─────────────────────────────────────────────────────────────────────────────

/// A UPLC program containing a `Data` constant (type tag 8 in the flat
/// universe-tag encoding) must decode and round-trip without error.
///
/// The test constructs a small program `(lam x. (const (Data (I 42))))`,
/// encodes it, decodes it, and verifies the constant survives the round-trip.
#[test]
fn flat_round_trips_data_constant() {
    use dugite_uplc::term::{Constant, Term};
    use dugite_uplc::Program;

    let data_val = Data::I(BigInt::from(42i64));
    let program = Program {
        version: (1, 0, 0),
        term: Term::Lam(Rc::new(Term::Const(Constant::Data(data_val.clone())))),
    };

    let flat = program.to_flat().expect("encode Data constant program");
    let decoded = Program::from_flat(&flat).expect("decode Data constant program");

    assert_eq!(decoded.version, (1, 0, 0));
    let Term::Lam(body) = decoded.term else {
        panic!("expected Lam");
    };
    // body is Rc<Term>; clone to get an owned Term for pattern matching.
    let Term::Const(Constant::Data(d)) = (*body).clone() else {
        panic!("expected Const(Data(_))");
    };
    assert_eq!(d, data_val, "Data constant must survive flat round-trip");
}

/// A program containing a `List(Integer)` constant must round-trip.
#[test]
fn flat_round_trips_list_integer_constant() {
    use dugite_uplc::term::{Constant, Term, TypeTag};
    use dugite_uplc::Program;

    let elements = vec![
        Constant::Integer(BigInt::from(1i64)),
        Constant::Integer(BigInt::from(2i64)),
        Constant::Integer(BigInt::from(3i64)),
    ];
    let program = Program {
        version: (1, 0, 0),
        term: Term::Const(Constant::ProtoList {
            elem_type: TypeTag::Integer,
            elements: elements.clone(),
        }),
    };

    let flat = program.to_flat().expect("encode List(Integer) program");
    let decoded = Program::from_flat(&flat).expect("decode List(Integer) program");

    let Term::Const(Constant::ProtoList {
        elements: decoded_elems,
        ..
    }) = decoded.term
    else {
        panic!("expected Const(ProtoList)");
    };
    assert_eq!(
        decoded_elems, elements,
        "List elements must survive flat round-trip"
    );
}

/// A program containing a `Pair(Integer, ByteString)` constant must round-trip.
#[test]
fn flat_round_trips_pair_constant() {
    use dugite_uplc::term::{Constant, Term, TypeTag};
    use dugite_uplc::Program;

    let program = Program {
        version: (1, 0, 0),
        term: Term::Const(Constant::ProtoPair {
            a_type: TypeTag::Integer,
            b_type: TypeTag::ByteString,
            a: Box::new(Constant::Integer(BigInt::from(99i64))),
            b: Box::new(Constant::ByteString(vec![0xde, 0xad, 0xbe, 0xef])),
        }),
    };

    let flat = program.to_flat().expect("encode Pair program");
    let decoded = Program::from_flat(&flat).expect("decode Pair program");

    let Term::Const(Constant::ProtoPair { a, b, .. }) = decoded.term else {
        panic!("expected Const(ProtoPair)");
    };
    assert_eq!(*a, Constant::Integer(BigInt::from(99i64)));
    assert_eq!(*b, Constant::ByteString(vec![0xde, 0xad, 0xbe, 0xef]));
}

/// A program containing a `List(Data)` constant must round-trip.
/// `List(Data)` is the most common compound type in real Plutus V1/V2 scripts.
#[test]
fn flat_round_trips_list_data_constant() {
    use dugite_uplc::term::{Constant, Term, TypeTag};
    use dugite_uplc::Program;

    let items = vec![
        Constant::Data(Data::I(BigInt::from(1i64))),
        Constant::Data(Data::B(vec![0xaa, 0xbb])),
        Constant::Data(Data::Constr(0, vec![Data::I(BigInt::from(3i64))])),
    ];
    let program = Program {
        version: (1, 0, 0),
        term: Term::Const(Constant::ProtoList {
            elem_type: TypeTag::Data,
            elements: items.clone(),
        }),
    };

    let flat = program.to_flat().expect("encode List(Data) program");
    let decoded = Program::from_flat(&flat).expect("decode List(Data) program");

    let Term::Const(Constant::ProtoList {
        elements: decoded_items,
        ..
    }) = decoded.term
    else {
        panic!("expected Const(ProtoList)");
    };
    assert_eq!(decoded_items, items);
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug D: txInfoWdrl must be Data::Map
// ─────────────────────────────────────────────────────────────────────────────

fn minimal_txinfo_v1() -> TxInfoV1 {
    TxInfoV1 {
        inputs: vec![],
        outputs: vec![],
        fee: BigInt::from(0u64),
        mint: PlutusValue::default(),
        dcert: vec![],
        wdrl: vec![],
        valid_range: PosixTimeRange {
            lower: None,
            upper: None,
        },
        signatories: vec![],
        data: vec![],
        txid: [0u8; 32],
    }
}

fn minimal_txinfo_v2() -> TxInfoV2 {
    TxInfoV2 {
        inputs: vec![],
        reference_inputs: vec![],
        outputs: vec![],
        fee: BigInt::from(0u64),
        mint: PlutusValue::default(),
        dcert: vec![],
        wdrl: vec![],
        valid_range: PosixTimeRange {
            lower: None,
            upper: None,
        },
        signatories: vec![],
        redeemers: vec![],
        data: vec![],
        txid: [0u8; 32],
    }
}

fn minimal_txinfo_v3() -> TxInfoV3 {
    TxInfoV3 {
        inputs: vec![],
        reference_inputs: vec![],
        outputs: vec![],
        fee: BigInt::from(0u64),
        mint: PlutusValue::default(),
        certs: vec![],
        wdrl: vec![],
        valid_range: PosixTimeRange {
            lower: None,
            upper: None,
        },
        signatories: vec![],
        redeemers: vec![],
        datums: vec![],
        txid: [0u8; 32],
        votes: vec![],
        proposal_procedures: vec![],
        current_treasury: None,
        treasury_donation: None,
    }
}

/// V3 `txInfoFee :: Lovelace` is a newtype over Integer with a newtype-derived
/// `ToData`, so it serialises as a BARE `I lovelace` — NOT a Value map (unlike
/// V1/V2 `txInfoFee :: Value`). Guards against re-applying the V1/V2 ada-Value
/// encoding to V3 (a regression the adversarial review caught).
#[test]
fn txinfo_v3_fee_is_bare_integer_not_value_map() {
    let lovelace = 177_721u64;
    let mut info = minimal_txinfo_v3();
    info.fee = BigInt::from(lovelace);
    let d = info.to_data();
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV3 must be Constr 0");
    };
    // V3 TxInfo to_data field order: [inputs, refInputs, outputs, fee, ...] —
    // fee is field index 3.
    match &fields[3] {
        Data::I(n) => assert_eq!(
            *n,
            BigInt::from(lovelace),
            "V3 fee must be the bare lovelace integer"
        ),
        other => panic!("V3 txInfoFee must be Data::I (bare Lovelace), got {other:?}"),
    }
}

fn dummy_staking_cred() -> (StakingCredential, BigInt) {
    (
        StakingCredential::Hash(Credential::PubKey([0x77u8; 28])),
        BigInt::from(50u64),
    )
}

/// `TxInfoV1.txInfoWdrl` must be encoded as a `Data::List` of `Constr 0` pairs,
/// NOT a `Data::Map`.
///
/// V1 uses `[(StakingCredential, Integer)]` — an association list, not an
/// association map — matching the Haskell `AssocMap.toList` semantics for V1.
/// V2+ changed this to a proper `Map`.
///
/// Reference: `PlutusLedgerApi.V1.Contexts.TxInfo.txInfoWdrl`
///   `txInfoWdrl :: [(StakingCredential, Integer)]`
#[test]
fn txinfo_v1_wdrl_is_data_list_of_constr_pairs() {
    let (sc, amt) = dummy_staking_cred();
    let mut info = minimal_txinfo_v1();
    info.wdrl = vec![(sc, amt)];
    let d = info.to_data(false);

    // V1 TxInfo: Constr 0 [inputs, outputs, fee, mint, dcert, wdrl, ...]
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV1 must be Constr 0");
    };
    // wdrl is field index 5
    let wdrl_field = &fields[5];
    // V1 wdrl must be a List (not a Map).
    assert!(
        matches!(wdrl_field, Data::List(_)),
        "txInfoWdrl (V1 field 5) must be Data::List, got {wdrl_field:?}"
    );
    assert!(
        !matches!(wdrl_field, Data::Map(_)),
        "txInfoWdrl (V1) must NOT be Data::Map"
    );
    // Each element must be Constr 0 [cred, Integer].
    let Data::List(ref items) = wdrl_field else {
        unreachable!()
    };
    assert_eq!(items.len(), 1, "expected 1 withdrawal entry");
    assert!(
        matches!(&items[0], Data::Constr(0, inner) if inner.len() == 2),
        "V1 wdrl entry must be Constr 0 [cred, amt]; got {:?}",
        items[0]
    );
}

/// `TxInfoV2.txInfoWdrl` must be `Data::Map` (V2 uses AssocMap, not AssocList).
/// Reference: `PlutusLedgerApi.V2.Contexts.TxInfo.txInfoWdrl :: Map StakingCredential Integer`
#[test]
fn txinfo_v2_wdrl_is_data_map() {
    let (sc, amt) = dummy_staking_cred();
    let mut info = minimal_txinfo_v2();
    info.wdrl = vec![(sc, amt)];
    let d = info.to_data(false);

    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV2 must be Constr 0");
    };
    // V2 field order: inputs=0, reference_inputs=1, outputs=2, fee=3, mint=4,
    //                 dcert=5, wdrl=6, valid_range=7, signatories=8, redeemers=9,
    //                 data=10, txid=11
    let wdrl_field = &fields[6];
    assert!(
        matches!(wdrl_field, Data::Map(_)),
        "txInfoWdrl (V2 field 6) must be Data::Map, got {wdrl_field:?}"
    );
    assert!(
        !matches!(wdrl_field, Data::List(_)),
        "txInfoWdrl (V2) must NOT be Data::List"
    );
}

/// With no withdrawals, V1 wdrl must be an empty List (not empty Map).
#[test]
fn txinfo_v1_empty_wdrl_is_empty_list() {
    let info = minimal_txinfo_v1();
    let d = info.to_data(false);
    let Data::Constr(0, ref fields) = d else {
        panic!("expected Constr 0");
    };
    assert_eq!(
        fields[5],
        Data::List(vec![]),
        "V1 empty wdrl must be empty List (not Map); got {:?}",
        fields[5]
    );
}

/// V2 empty wdrl must be an empty Map.
#[test]
fn txinfo_v2_empty_wdrl_is_empty_map() {
    let info = minimal_txinfo_v2();
    let d = info.to_data(false);
    let Data::Constr(0, ref fields) = d else {
        panic!("expected Constr 0");
    };
    assert_eq!(
        fields[6],
        Data::Map(vec![]),
        "V2 empty wdrl must be empty Map; got {:?}",
        fields[6]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Integration: flat decoder + arbitrary-precision integer
// ─────────────────────────────────────────────────────────────────────────────

/// Large Integer constants (> i64::MAX) must decode correctly.
/// This exercises the arbitrary-precision `read_integer_bigint` path.
#[test]
fn flat_round_trips_large_integer_constant() {
    use dugite_uplc::term::{Constant, Term};
    use dugite_uplc::Program;

    // A large positive integer that doesn't fit in i64.
    let large: BigInt = BigInt::from(i64::MAX) + 1i64;
    let program = Program {
        version: (1, 0, 0),
        term: Term::Const(Constant::Integer(large.clone())),
    };
    let flat = program.to_flat().expect("encode large integer");
    let decoded = Program::from_flat(&flat).expect("decode large integer");
    let Term::Const(Constant::Integer(n)) = decoded.term else {
        panic!("expected Const(Integer)");
    };
    assert_eq!(n, large, "large Integer must survive flat round-trip");
}

/// Negative integers must round-trip correctly via zig-zag.
#[test]
fn flat_round_trips_negative_integer_constant() {
    use dugite_uplc::term::{Constant, Term};
    use dugite_uplc::Program;

    for &n in &[-1i64, i64::MIN, -123456789i64] {
        let program = Program {
            version: (1, 0, 0),
            term: Term::Const(Constant::Integer(BigInt::from(n))),
        };
        let flat = program.to_flat().expect("encode negative int");
        let decoded = Program::from_flat(&flat).expect("decode negative int");
        let Term::Const(Constant::Integer(v)) = decoded.term else {
            panic!("expected Const(Integer)");
        };
        assert_eq!(v, BigInt::from(n), "negative int {n} must round-trip");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug E: TxId must be wrapped in Constr 0
// ─────────────────────────────────────────────────────────────────────────────

/// `TxOutRef::to_data` must produce `Constr 0 [Constr 0 [B bytes32], I idx]`.
///
/// The TxId newtype is `makeIsDataSchemaIndexed ''TxId [('TxId, 0)]` —
/// serialises as `Constr 0 [B bytes32]`, NOT bare `B bytes32`.
/// Scripts that navigate `txInfoInputs` call `unConstrData` on the TxId field;
/// bare bytes caused `unConstrData on non-Constr Data` and broke EVERY spend.
#[test]
fn txoutref_txid_is_constr_wrapped() {
    let r = TxOutRef {
        tx_id: [0xab; 32],
        idx: 7,
    };
    let d = r.to_data();
    // Outer: Constr 0 [TxId, Integer]
    let Data::Constr(0, ref outer) = d else {
        panic!("TxOutRef must be Constr 0; got {d:?}");
    };
    assert_eq!(outer.len(), 2, "TxOutRef has 2 fields");
    // Inner TxId: must be Constr 0 [B bytes32], NOT bare B bytes32.
    let Data::Constr(0, ref id_fields) = outer[0] else {
        panic!(
            "TxId must be Constr 0 [B bytes32]; got {:?} — this is Bug E (TxId not wrapped)",
            outer[0]
        );
    };
    assert_eq!(id_fields.len(), 1);
    assert!(
        matches!(&id_fields[0], Data::B(b) if b.len() == 32),
        "TxId inner field must be B bytes32; got {:?}",
        id_fields[0]
    );
    // Index
    assert!(
        matches!(&outer[1], Data::I(i) if i == &BigInt::from(7u64)),
        "TxOutRef idx must be I(7); got {:?}",
        outer[1]
    );
}

/// Regression: the old (buggy) encoding emitted `Constr 0 [B bytes32, I idx]`.
/// Assert that the new encoding does NOT have bare bytes in the TxId field.
#[test]
fn txoutref_txid_is_not_bare_bytes() {
    let r = TxOutRef {
        tx_id: [0x11; 32],
        idx: 0,
    };
    let Data::Constr(0, ref outer) = r.to_data() else {
        panic!("expected Constr 0");
    };
    assert!(
        !matches!(&outer[0], Data::B(_)),
        "TxId field must NOT be bare bytes (Bug E regression); got {:?}",
        outer[0]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 bare-txid: V3 `TxId = newtype … deriving newtype ToData` → BARE B(32)
// everywhere (txInfoId, TxOutRef.txId, GovActionId.txId), unlike V1/V2's
// `Constr 0 [B32]` wrapper. Routing the V3 path through the wrapped form broke
// every V3 spending/governance validator at the first unConstrData.
// ─────────────────────────────────────────────────────────────────────────────

/// `TxOutRef::to_data_v3` must embed the txid as BARE `B(32)`:
/// `Constr 0 [B(32), I idx]` — NOT the V1/V2 `Constr 0 [Constr 0 [B32], I idx]`.
#[test]
fn txoutref_v3_txid_is_bare_bytes() {
    let r = TxOutRef {
        tx_id: [0xcd; 32],
        idx: 3,
    };
    let Data::Constr(0, ref outer) = r.to_data_v3() else {
        panic!("V3 TxOutRef must be Constr 0");
    };
    assert_eq!(outer.len(), 2);
    assert!(
        matches!(&outer[0], Data::B(b) if b.len() == 32),
        "V3 TxOutRef txid must be BARE B(32); got {:?}",
        outer[0]
    );
    assert!(matches!(&outer[1], Data::I(i) if i == &BigInt::from(3u64)));
}

/// The V3 `TxInfo.inputs` path (`TxInInfo::to_data_v3`) must embed the bare-txid
/// V3 TxOutRef, NOT the wrapped V1/V2 one.
#[test]
fn v3_txininfo_embeds_bare_txid_outref() {
    let info = {
        let mut i = minimal_txinfo_v3();
        i.inputs = vec![TxInInfo {
            out_ref: TxOutRef {
                tx_id: [0x01; 32],
                idx: 0,
            },
            resolved: TxOut {
                address: Address {
                    payment: Credential::PubKey([0x02; 28]),
                    staking: None,
                },
                value: PlutusValue::default(),
                datum: OutputDatum::None,
                reference_script: None,
            },
        }];
        i
    };
    let Data::Constr(0, ref fields) = info.to_data() else {
        panic!("TxInfoV3 must be Constr 0");
    };
    // field[0] = inputs : List[TxInInfo]
    let Data::List(ref inputs) = fields[0] else {
        panic!("inputs must be a List");
    };
    let Data::Constr(0, ref txininfo) = inputs[0] else {
        panic!("TxInInfo must be Constr 0");
    };
    // txininfo[0] = TxOutRef = Constr 0 [B32 BARE, I idx]
    let Data::Constr(0, ref outref) = txininfo[0] else {
        panic!("TxOutRef must be Constr 0");
    };
    assert!(
        matches!(&outref[0], Data::B(b) if b.len() == 32),
        "V3 TxInInfo's TxOutRef txid must be BARE B(32), not Constr-wrapped; got {:?}",
        outref[0]
    );
}

/// `GovActionId::to_data` (V3 votes map) must embed the bare-txid form.
#[test]
fn gov_action_id_v3_txid_is_bare_bytes() {
    let g = GovActionId {
        tx_id: [0x09; 32],
        idx: 1,
    };
    let Data::Constr(0, ref outer) = g.to_data() else {
        panic!("GovActionId must be Constr 0");
    };
    assert_eq!(outer.len(), 2);
    assert!(
        matches!(&outer[0], Data::B(b) if b.len() == 32),
        "V3 GovActionId txid must be BARE B(32); got {:?}",
        outer[0]
    );
    assert!(matches!(&outer[1], Data::I(i) if i == &BigInt::from(1u64)));
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug F: fee must be an ADA-only Value (Map), not a bare Integer
// ─────────────────────────────────────────────────────────────────────────────

/// `txInfoFee :: Value` — must be `Map[(b"", Map[(b"", I lovelace)])]`.
/// Scripts that inspect the fee (e.g., to check minimum fee) call
/// `unMap`/`valueOf` on the fee field; bare Integer caused
/// `unMapData on non-Map Data`.
#[test]
fn txinfo_v1_fee_is_ada_value_map() {
    let lovelace = 177_721u64;
    let mut info = minimal_txinfo_v1();
    info.fee = BigInt::from(lovelace);
    let d = info.to_data(false);

    // V1 TxInfo: Constr 0 [inputs, outputs, fee, ...]  — fee is field 2
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV1 must be Constr 0");
    };
    let fee_field = &fields[2];

    // fee must be a Map (not I)
    assert!(
        matches!(fee_field, Data::Map(_)),
        "txInfoFee (V1 field 2) must be Data::Map (ADA-only Value), got {fee_field:?}"
    );
    assert!(
        !matches!(fee_field, Data::I(_)),
        "txInfoFee must NOT be bare Integer (Bug F regression)"
    );

    // Verify the map structure: Map[(b"", Map[(b"", I lovelace)])]
    let Data::Map(ref outer_entries) = fee_field else {
        unreachable!()
    };
    assert_eq!(outer_entries.len(), 1, "ADA-only Value has 1 policy");
    let (policy_key, inner_map) = &outer_entries[0];
    assert_eq!(
        policy_key,
        &Data::B(Vec::new()),
        "ADA policy key must be b\"\" (empty bytes)"
    );
    let Data::Map(ref token_entries) = inner_map else {
        panic!("inner value must be Map; got {inner_map:?}");
    };
    assert_eq!(token_entries.len(), 1, "ADA-only token map has 1 entry");
    let (token_key, amt) = &token_entries[0];
    assert_eq!(
        token_key,
        &Data::B(Vec::new()),
        "ADA token name must be b\"\" (empty bytes)"
    );
    assert_eq!(
        amt,
        &Data::I(BigInt::from(lovelace)),
        "fee amount must be I(lovelace)"
    );
}

/// Same fee-as-Value check for V2.
#[test]
fn txinfo_v2_fee_is_ada_value_map() {
    let mut info = minimal_txinfo_v2();
    info.fee = BigInt::from(5_000_000u64);
    let d = info.to_data(false);
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV2 must be Constr 0");
    };
    // V2: inputs=0, ref_inputs=1, outputs=2, fee=3
    let fee_field = &fields[3];
    assert!(
        matches!(fee_field, Data::Map(_)),
        "txInfoFee (V2 field 3) must be Data::Map (ADA-only Value), got {fee_field:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Bug G: V1 txInfoData must be AssocList (List[Constr 0]), not Map
// ─────────────────────────────────────────────────────────────────────────────

/// V1 `txInfoData :: [(DatumHash, Datum)]` is an AssocList.
/// Encoded as `Data::List[Constr 0 [B32(hash), datum]]`.
/// The old and incorrect V2 Map encoding breaks Haskell lookup via `findDatum`.
#[test]
fn txinfo_v1_data_is_list_of_constr_pairs() {
    let datum_hash = [0xdd; 32];
    let datum_value = Data::I(BigInt::from(99u64));
    let mut info = minimal_txinfo_v1();
    info.data = vec![(datum_hash, datum_value.clone())];
    let d = info.to_data(false);

    // V1 TxInfo: Constr 0 [inputs, outputs, fee, mint, dcert, wdrl, validRange, sigs, data, id]
    // data is field index 8
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV1 must be Constr 0");
    };
    let data_field = &fields[8];
    assert!(
        matches!(data_field, Data::List(_)),
        "txInfoData (V1 field 8) must be Data::List; got {data_field:?}"
    );
    assert!(
        !matches!(data_field, Data::Map(_)),
        "txInfoData (V1) must NOT be Data::Map (Bug G regression)"
    );
    let Data::List(ref items) = data_field else {
        unreachable!()
    };
    assert_eq!(items.len(), 1, "expected 1 datum entry");
    let Data::Constr(0, ref pair) = items[0] else {
        panic!(
            "V1 data entry must be Constr 0 [B32, datum]; got {:?}",
            items[0]
        );
    };
    assert_eq!(pair.len(), 2);
    assert!(
        matches!(&pair[0], Data::B(b) if b.len() == 32),
        "datum hash must be B32; got {:?}",
        pair[0]
    );
    assert_eq!(&pair[1], &datum_value, "datum value must match");
}

/// V2 `txInfoData :: Map DatumHash Datum` — must be Data::Map, not List.
#[test]
fn txinfo_v2_data_is_map() {
    let datum_hash = [0xee; 32];
    let datum_value = Data::B(vec![0x01, 0x02]);
    let mut info = minimal_txinfo_v2();
    info.data = vec![(datum_hash, datum_value)];
    let d = info.to_data(false);

    // V2: field 10 is data
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV2 must be Constr 0");
    };
    let data_field = &fields[10];
    assert!(
        matches!(data_field, Data::Map(_)),
        "txInfoData (V2 field 10) must be Data::Map; got {data_field:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V1 txInfoId must be Constr-wrapped TxId
// ─────────────────────────────────────────────────────────────────────────────

/// `txInfoId :: TxId = Constr 0 [B bytes32]` — same wrapping rule as TxOutRef.
#[test]
fn txinfo_v1_txid_is_constr_wrapped() {
    let mut info = minimal_txinfo_v1();
    info.txid = [0xfe; 32];
    let d = info.to_data(false);

    // V1 TxInfo field 9 is txid
    let Data::Constr(0, ref fields) = d else {
        panic!("TxInfoV1 must be Constr 0");
    };
    let id_field = &fields[9];
    let Data::Constr(0, ref inner) = id_field else {
        panic!("txInfoId (V1 field 9) must be TxId = Constr 0 [B32]; got {id_field:?}");
    };
    assert_eq!(inner.len(), 1);
    assert!(
        matches!(&inner[0], Data::B(b) if b.len() == 32),
        "txInfoId inner bytes must be B32; got {:?}",
        inner[0]
    );
}

/// `txInfoId` must NOT be bare bytes.
#[test]
fn txinfo_v1_txid_is_not_bare_bytes() {
    let info = minimal_txinfo_v1();
    let d = info.to_data(false);
    let Data::Constr(0, ref fields) = d else {
        panic!("expected Constr 0")
    };
    assert!(
        !matches!(&fields[9], Data::B(_)),
        "txInfoId (V1 field 9) must NOT be bare bytes; got {:?}",
        fields[9]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V1 TxOut must use 3-field schema (no ref_script field)
// ─────────────────────────────────────────────────────────────────────────────

fn minimal_txout_v1(datum: OutputDatum) -> TxOut {
    TxOut {
        address: Address {
            payment: Credential::PubKey([0x11; 28]),
            staking: None,
        },
        value: PlutusValue::default(),
        datum,
        reference_script: None,
    }
}

/// V1 TxOut must be `Constr 0 [Address, Value, Maybe DatumHash]` — 3 fields.
/// V2 TxOut is `Constr 0 [Address, Value, OutputDatum, Maybe ScriptHash]` — 4 fields.
#[test]
fn txout_v1_has_three_fields() {
    let out = minimal_txout_v1(OutputDatum::None);
    let d = out.to_data_v1();
    let Data::Constr(0, ref fields) = d else {
        panic!("TxOut (V1) must be Constr 0; got {d:?}");
    };
    assert_eq!(
        fields.len(),
        3,
        "V1 TxOut must have 3 fields (Address, Value, Maybe DatumHash); got {}",
        fields.len()
    );
}

/// V2 TxOut must be `Constr 0 [Address, Value, OutputDatum, Maybe ScriptHash]` — 4 fields.
#[test]
fn txout_v2_has_four_fields() {
    let out = minimal_txout_v1(OutputDatum::None);
    let d = out.to_data();
    let Data::Constr(0, ref fields) = d else {
        panic!("TxOut (V2) must be Constr 0; got {d:?}");
    };
    assert_eq!(
        fields.len(),
        4,
        "V2 TxOut must have 4 fields (Address, Value, OutputDatum, Maybe ScriptHash); got {}",
        fields.len()
    );
}

/// V1 TxOut with a DatumHash must emit `Just h = Constr 0 [B32]` as field 2.
#[test]
fn txout_v1_datum_hash_is_just_b32() {
    let hash = [0x77; 32];
    let out = minimal_txout_v1(OutputDatum::Hash(hash));
    let d = out.to_data_v1();
    let Data::Constr(0, ref fields) = d else {
        panic!("expected Constr 0")
    };
    let Data::Constr(0, ref maybe_inner) = fields[2] else {
        panic!(
            "V1 DatumHash must be Just = Constr 0 [B32]; got {:?}",
            fields[2]
        );
    };
    assert!(
        matches!(&maybe_inner[0], Data::B(b) if b.len() == 32),
        "DatumHash inner must be B32"
    );
}

/// V1 TxOut with no datum must emit `Nothing = Constr 1 []` as field 2.
#[test]
fn txout_v1_no_datum_is_nothing() {
    let out = minimal_txout_v1(OutputDatum::None);
    let d = out.to_data_v1();
    let Data::Constr(0, ref fields) = d else {
        panic!("expected Constr 0")
    };
    assert_eq!(
        fields[2],
        Data::Constr(1, vec![]),
        "V1 no-datum must be Nothing = Constr 1 []; got {:?}",
        fields[2]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Integration: ScriptContext V1 spending purpose has Constr-wrapped TxOutRef
// ─────────────────────────────────────────────────────────────────────────────

/// The full `ScriptContext` for a Spend redeemer must carry a properly
/// Constr-wrapped `TxOutRef` inside `ScriptPurpose::Spending`.
/// Scripts call `unConstrData` on the purpose's first field to get the TxOutRef,
/// then `unConstrData` again on fields[0] to unwrap the TxId newtype.
#[test]
fn script_context_v1_spend_purpose_has_wrapped_txid() {
    let purpose = ScriptPurpose::Spending(TxOutRef {
        tx_id: [0x42; 32],
        idx: 2,
    });
    let ctx = ScriptContextV1 {
        tx_info: minimal_txinfo_v1(),
        purpose,
    };
    let d = ctx.to_data(false);
    // ctx = Constr 0 [TxInfo, ScriptPurpose]
    let Data::Constr(0, ref ctx_fields) = d else {
        panic!("ScriptContext must be Constr 0");
    };
    // ScriptPurpose::Spending = Constr 1 [TxOutRef]
    let Data::Constr(1, ref purpose_fields) = ctx_fields[1] else {
        panic!("Spending purpose must be Constr 1; got {:?}", ctx_fields[1]);
    };
    assert_eq!(purpose_fields.len(), 1);
    // TxOutRef = Constr 0 [TxId, Integer]
    let Data::Constr(0, ref outref_fields) = purpose_fields[0] else {
        panic!("TxOutRef must be Constr 0; got {:?}", purpose_fields[0]);
    };
    // TxId = Constr 0 [B bytes32]
    let Data::Constr(0, ref txid_fields) = outref_fields[0] else {
        panic!(
            "TxId in Spending purpose must be Constr 0 [B32]; got {:?}",
            outref_fields[0]
        );
    };
    assert!(
        matches!(&txid_fields[0], Data::B(b) if b.len() == 32),
        "TxId inner field must be B32"
    );
}

// Bug (#22) — V1/V2 `Rewarding StakingCredential` was missing the `StakingHash`
// wrapper. `Rewarding (Credential)` was emitted as `Constr 2 [Credential]`
// instead of `Constr 2 [StakingHash Credential]` = `Constr 2 [Constr 0 [Cred]]`.
// A staking-script deserializer then read the inner `ScriptCredential`'s
// `Constr 1` tag as `StakingPtr` (StakingCredential: StakingHash=0, StakingPtr=1)
// and `unIData`'d the 28-byte hash → "unIData on non-I". V3 (`Rewarding
// Credential`, no StakingCredential) keeps the bare form.
#[test]
fn v1v2_rewarding_purpose_wraps_credential_in_staking_hash() {
    let p = ScriptPurpose::Rewarding(Credential::Script([0xab; 28]));
    let d = p.to_data();
    // Constr 2 [ Constr 0 [ Constr 1 [B28] ] ]
    let Data::Constr(2, outer) = &d else {
        panic!("Rewarding must be Constr 2, got {d:?}");
    };
    assert_eq!(
        outer.len(),
        1,
        "Rewarding has one field (StakingCredential)"
    );
    let Data::Constr(0, sh) = &outer[0] else {
        panic!(
            "V1/V2 Rewarding field must be StakingHash = Constr 0, got {:?}",
            outer[0]
        );
    };
    assert_eq!(sh.len(), 1, "StakingHash wraps one Credential");
    let Data::Constr(1, cred) = &sh[0] else {
        panic!("inner must be ScriptCredential = Constr 1, got {:?}", sh[0]);
    };
    assert!(matches!(&cred[0], Data::B(b) if b.len() == 28));
}

#[test]
fn v3_rewarding_purpose_uses_bare_credential() {
    let p = ScriptPurpose::Rewarding(Credential::Script([0xab; 28]));
    let d = p.to_data_v3();
    // V3: Constr 2 [ Constr 1 [B28] ]  (Credential directly, NO StakingHash)
    let Data::Constr(2, outer) = &d else {
        panic!("Rewarding must be Constr 2, got {d:?}");
    };
    let Data::Constr(1, cred) = &outer[0] else {
        panic!(
            "V3 Rewarding field must be the bare Credential = Constr 1, got {:?}",
            outer[0]
        );
    };
    assert!(matches!(&cred[0], Data::B(b) if b.len() == 28));
}
