//! Property-based tests for the Rat (rational number) arithmetic.
//!
//! Rat handles all reward calculations and had overflow bugs fixed on 2026-03-09.
//! These tests verify algebraic properties hold across small and large value ranges.

use proptest::prelude::*;
use torsten_ledger::Rat;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Small values for basic algebraic properties.
fn small_nonzero() -> impl Strategy<Value = i128> {
    prop::num::i128::ANY.prop_filter_map("nonzero", |v| {
        let v = v % 10_000;
        if v == 0 {
            Some(1)
        } else {
            Some(v)
        }
    })
}

fn small_pos() -> impl Strategy<Value = i128> {
    1i128..10_000i128
}

/// Large values that exercise cross-reduction but whose products stay within i128.
/// Range ~10^18 (u64::MAX territory) — large enough to overflow without cross-reduction,
/// small enough that products of two values fit in i128 (~10^36 < 1.7*10^38).
fn large_nonzero() -> impl Strategy<Value = i128> {
    (i64::MAX as i128 / 2..i64::MAX as i128).prop_map(|v| if v == 0 { 1 } else { v })
}

fn large_pos() -> impl Strategy<Value = i128> {
    i64::MAX as i128 / 2..i64::MAX as i128
}

/// Very large values near i128::MAX — for no-panic testing only (precision may be lost).
fn extreme_pos() -> impl Strategy<Value = i128> {
    i128::MAX / 8..i128::MAX / 4
}

fn extreme_nonzero() -> impl Strategy<Value = i128> {
    (i128::MAX / 8..i128::MAX / 4).prop_map(|v| if v == 0 { 1 } else { v })
}

/// Mixed strategy combining small and large values.
fn any_nonzero() -> impl Strategy<Value = i128> {
    prop_oneof![small_nonzero(), large_nonzero(), (-10_000i128..-1i128),]
}

fn any_pos() -> impl Strategy<Value = i128> {
    prop_oneof![small_pos(), large_pos()]
}

/// Generate a Rat from two nonzero values.
fn arb_rat() -> impl Strategy<Value = Rat> {
    (any_nonzero(), any_pos()).prop_map(|(n, d)| Rat::new(n, d))
}

/// Generate a Rat from small values (for associativity/distributivity which chain operations).
fn small_rat() -> impl Strategy<Value = Rat> {
    (small_nonzero(), small_pos()).prop_map(|(n, d)| Rat::new(n, d))
}

/// Generate a nonzero Rat (for division tests).
fn nonzero_rat() -> impl Strategy<Value = Rat> {
    (any_nonzero(), any_pos())
        .prop_filter("nonzero numerator", |(n, _)| *n != 0)
        .prop_map(|(n, d)| Rat::new(n, d))
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn rats_eq(a: &Rat, b: &Rat) -> bool {
    // Compare as normalized fractions
    a.n == b.n && a.d == b.d
}

// ---------------------------------------------------------------------------
// Property 1: Normalization — always reduced, d > 0
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn normalization(n in any_nonzero(), d in any_pos()) {
        let r = Rat::new(n, d);
        prop_assert!(r.d > 0, "denominator must be positive, got {}", r.d);

        // Check GCD(|n|, d) == 1 (fully reduced)
        let mut a = r.n.unsigned_abs();
        let mut b = r.d.unsigned_abs();
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        prop_assert_eq!(a, 1, "Rat({}/{}) not fully reduced (gcd={})", r.n, r.d, a);
    }
}

// ---------------------------------------------------------------------------
// Property 2: Addition commutativity — a + b == b + a
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn add_commutative(a in arb_rat(), b in arb_rat()) {
        let ab = a.add(&b);
        let ba = b.add(&a);
        prop_assert!(rats_eq(&ab, &ba), "{:?} + {:?}: {:?} != {:?}", a, b, ab, ba);
    }
}

// ---------------------------------------------------------------------------
// Property 3: Addition associativity — (a+b)+c == a+(b+c)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn add_associative(a in small_rat(), b in small_rat(), c in small_rat()) {
        let ab_c = a.add(&b).add(&c);
        let a_bc = a.add(&b.add(&c));
        prop_assert!(rats_eq(&ab_c, &a_bc),
            "({:?}+{:?})+{:?} = {:?} != {:?}+({:?}+{:?}) = {:?}",
            a, b, c, ab_c, a, b, c, a_bc);
    }
}

// ---------------------------------------------------------------------------
// Property 4: Multiplication commutativity — a * b == b * a
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn mul_commutative(a in arb_rat(), b in arb_rat()) {
        let ab = a.mul(&b);
        let ba = b.mul(&a);
        prop_assert!(rats_eq(&ab, &ba), "{:?} * {:?}: {:?} != {:?}", a, b, ab, ba);
    }
}

// ---------------------------------------------------------------------------
// Property 5: Multiplication associativity — (a*b)*c == a*(b*c)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn mul_associative(a in small_rat(), b in small_rat(), c in small_rat()) {
        let ab_c = a.mul(&b).mul(&c);
        let a_bc = a.mul(&b.mul(&c));
        prop_assert!(rats_eq(&ab_c, &a_bc),
            "({:?}*{:?})*{:?} = {:?} != {:?}*({:?}*{:?}) = {:?}",
            a, b, c, ab_c, a, b, c, a_bc);
    }
}

// ---------------------------------------------------------------------------
// Property 6: Distributivity — a*(b+c) == a*b + a*c
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn distributive(a in small_rat(), b in small_rat(), c in small_rat()) {
        let lhs = a.mul(&b.add(&c));
        let rhs = a.mul(&b).add(&a.mul(&c));
        prop_assert!(rats_eq(&lhs, &rhs),
            "{:?}*({:?}+{:?}) = {:?} != {:?}*{:?}+{:?}*{:?} = {:?}",
            a, b, c, lhs, a, b, a, c, rhs);
    }
}

// ---------------------------------------------------------------------------
// Property 7: Division inverse — a * b / b == a (b != 0)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn div_inverse(a in arb_rat(), b in nonzero_rat()) {
        let result = a.mul(&b).div(&b);
        prop_assert!(rats_eq(&result, &a),
            "{:?} * {:?} / {:?} = {:?}, expected {:?}", a, b, b, result, a);
    }
}

// ---------------------------------------------------------------------------
// Property 8: Additive identity — a + 0 == a
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn additive_identity(a in arb_rat()) {
        let zero = Rat::new(0, 1);
        let result = a.add(&zero);
        prop_assert!(rats_eq(&result, &a),
            "{:?} + 0 = {:?}, expected {:?}", a, result, a);
    }
}

// ---------------------------------------------------------------------------
// Property 9: Multiplicative identity — a * 1 == a
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn multiplicative_identity(a in arb_rat()) {
        let one = Rat::new(1, 1);
        let result = a.mul(&one);
        prop_assert!(rats_eq(&result, &a),
            "{:?} * 1 = {:?}, expected {:?}", a, result, a);
    }
}

// ---------------------------------------------------------------------------
// Property 10: floor_u64 bounds — result <= n/d for positive rationals
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn floor_u64_bounds(n in 0i128..i128::from(u64::MAX), d in 1i128..1_000_000i128) {
        let r = Rat::new(n, d);
        let floored = r.floor_u64();
        // floor_u64 should be <= true value
        prop_assert!(i128::from(floored) <= r.n / r.d + 1,
            "floor_u64({:?}) = {} exceeds bound", r, floored);
        // And should equal integer division for positive values
        if r.n >= 0 && r.d > 0 {
            prop_assert_eq!(floored, (r.n / r.d) as u64);
        }
    }
}

// ---------------------------------------------------------------------------
// Property 11: min_rat — always returns the smaller value
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn min_rat_correct(a in small_rat(), b in small_rat()) {
        let m = a.min_rat(&b);
        // m should be <= both a and b (using cross multiplication)
        // m.n * a.d <= a.n * m.d (m <= a)
        prop_assert!(m.n * a.d <= a.n * m.d,
            "min({:?}, {:?}) = {:?} > a", a, b, m);
        prop_assert!(m.n * b.d <= b.n * m.d,
            "min({:?}, {:?}) = {:?} > b", a, b, m);
        // m should equal either a or b
        prop_assert!(rats_eq(&m, &a) || rats_eq(&m, &b),
            "min({:?}, {:?}) = {:?} is neither a nor b", a, b, m);
    }
}

// ---------------------------------------------------------------------------
// Property 12: No overflow on large values (the 2026-03-09 fix range)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn no_overflow_large_values(
        n1 in extreme_nonzero(), d1 in extreme_pos(),
        n2 in extreme_nonzero(), d2 in extreme_pos(),
    ) {
        let a = Rat::new(n1, d1);
        let b = Rat::new(n2, d2);

        // These should not panic (precision loss is acceptable for extreme values)
        let _ = a.add(&b);
        let _ = a.sub(&b);
        let _ = a.mul(&b);
        if b.n != 0 {
            let _ = a.div(&b);
        }
        let _ = a.min_rat(&b);
        let _ = a.floor_u64();
    }
}

// ---------------------------------------------------------------------------
// Property 13: Subtraction — a - a == 0
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn sub_self_is_zero(a in arb_rat()) {
        let result = a.sub(&a);
        prop_assert_eq!(result.n, 0, "{:?} - {:?} = {:?}, expected 0", a, a, result);
    }
}

// ---------------------------------------------------------------------------
// Property 14: Division self — a / a == 1 (a != 0)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn div_self_is_one(a in nonzero_rat()) {
        let result = a.div(&a);
        prop_assert!(rats_eq(&result, &Rat::new(1, 1)),
            "{:?} / {:?} = {:?}, expected 1/1", a, a, result);
    }
}

// ---------------------------------------------------------------------------
// Property 15: Zero denominator safety
// ---------------------------------------------------------------------------

#[test]
fn zero_denominator_returns_zero() {
    let r = Rat::new(42, 0);
    assert_eq!(r.n, 0);
    assert_eq!(r.d, 1);
}

// ---------------------------------------------------------------------------
// Property 16: Negative denominator normalization
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn negative_denominator_normalized(n in any_nonzero(), d in -10_000i128..-1i128) {
        let r = Rat::new(n, d);
        prop_assert!(r.d > 0, "denominator should be positive after normalization, got {}", r.d);
    }
}
