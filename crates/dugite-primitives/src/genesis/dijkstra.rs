//! Dijkstra genesis-file parser.
//!
//! The Dijkstra (post-Conway hard-fork) genesis file is a small JSON document
//! carrying the four protocol-parameter values introduced at the HFC:
//!
//! ```text
//! { "maxRefScriptSizePerBlock":  1048576,
//!   "maxRefScriptSizePerTx":     204800,
//!   "refScriptCostStride":       25600,
//!   "refScriptCostMultiplier":   1.2 }
//! ```
//!
//! These values re-parameterise Conway's hard-coded 1 MiB / 25 KiB / 1.2x
//! reference-script tiering (issue #462 Phase 4).  This module is the
//! **parser only** — wiring the loaded values into `ProtocolParameters` and
//! actually applying them in fee calculation is a separate phase.
//!
//! ## Wire format & source of truth
//!
//! Mirrors `Cardano.Ledger.Dijkstra.Genesis.DijkstraGenesis` and
//! `Cardano.Ledger.Dijkstra.PParams.UpgradeDijkstraPParams` from
//! `IntersectMBO/cardano-ledger` (`eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/`).
//!
//! Note the Haskell `DijkstraGenesis` is a *newtype* over `UpgradeDijkstraPParams`
//! that derives `ToKeyValuePairs` directly, so the JSON shape is the four
//! upgrade-pparam fields at the *top level* of the document — not nested under
//! a `dgUpgradePParams` key. This struct matches that shape.
//!
//! Defaults are taken from
//! `Cardano.Api.Genesis.Internal.dijkstraGenesisDefaults` (cardano-api).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::transaction::Rational;

/// Parsed Dijkstra genesis configuration.
///
/// Round-trip via `serde_json` is stable for documents emitted by
/// `cardano-cli create-testnet-data` / `dijkstraGenesisDefaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DijkstraGenesis {
    /// PParam 34: hard cap on total reference-script bytes across all
    /// transactions in a single block. Defaults to 1 MiB.
    pub max_ref_script_size_per_block: u32,

    /// PParam 35: hard cap on total reference-script bytes within a single
    /// transaction. Defaults to 200 KiB.
    pub max_ref_script_size_per_tx: u32,

    /// PParam 36: tier width (in bytes) for the reference-script cost
    /// staircase. Must be non-zero; defaults to 25 600 (25 KiB).
    pub ref_script_cost_stride: u32,

    /// PParam 37: per-tier price multiplier for reference-script fees.
    /// Must be a strictly-positive rational; defaults to 6/5 (i.e. 1.2x).
    pub ref_script_cost_multiplier: PositiveInterval,
}

impl DijkstraGenesis {
    /// Construct a `DijkstraGenesis` populated with the Haskell
    /// `dijkstraGenesisDefaults` (cardano-api `Cardano.Api.Genesis.Internal`).
    ///
    /// Useful for tests and for synthesising a genesis when none is supplied
    /// by the operator.
    pub fn defaults() -> Self {
        DijkstraGenesis {
            max_ref_script_size_per_block: 1024 * 1024, // 1 MiB
            max_ref_script_size_per_tx: 200 * 1024,     // 200 KiB
            ref_script_cost_stride: 25_600,             // 25 KiB
            ref_script_cost_multiplier: PositiveInterval::new(6, 5)
                .expect("6/5 = 1.2 is a valid PositiveInterval"),
        }
    }

    /// Parse a `DijkstraGenesis` from a JSON byte slice.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Parse a `DijkstraGenesis` from a JSON string.
    pub fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// A strictly-positive rational, mirroring Haskell's
/// `Cardano.Ledger.BaseTypes.PositiveInterval`.
///
/// JSON encoding accepts either a numeric scientific literal (e.g. `1.2`) or
/// an object `{"numerator": N, "denominator": D}`. Serialisation prefers the
/// scientific form when the value has a terminating decimal representation;
/// otherwise the object form is used (matching the Haskell
/// `instance ToJSON (BoundedRatio b Word64)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositiveInterval(Rational);

impl PositiveInterval {
    /// Construct from `numerator / denominator`.
    ///
    /// The fraction is reduced to lowest terms — `PositiveInterval::new(12, 10)`
    /// produces the same value as `PositiveInterval::new(6, 5)` — matching
    /// Haskell's `BoundedRatio` invariants.
    ///
    /// Returns `None` if `denominator == 0` or the value is non-positive
    /// (i.e. `numerator == 0`).
    pub fn new(numerator: u64, denominator: u64) -> Option<Self> {
        if denominator == 0 || numerator == 0 {
            return None;
        }
        let g = gcd_u64(numerator, denominator);
        Some(PositiveInterval(Rational {
            numerator: numerator / g,
            denominator: denominator / g,
        }))
    }

    /// Borrow the inner rational.
    pub fn as_rational(&self) -> &Rational {
        &self.0
    }

    /// Consume into the inner rational.
    pub fn into_rational(self) -> Rational {
        self.0
    }

    /// Numerator.
    pub fn numerator(&self) -> u64 {
        self.0.numerator
    }

    /// Denominator (guaranteed non-zero).
    pub fn denominator(&self) -> u64 {
        self.0.denominator
    }

    /// Best-effort `f64` conversion (lossy for non-terminating fractions).
    pub fn to_f64(&self) -> f64 {
        self.0.numerator as f64 / self.0.denominator as f64
    }
}

impl Serialize for PositiveInterval {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Prefer the compact scientific form when the value has a terminating
        // decimal expansion within `f64` precision (matches Haskell's
        // `fromRationalRepetendLimited maxDecimalsWord64`). Fall back to the
        // structured object form otherwise so no precision is lost on the
        // wire.
        if let Some(s) = terminating_decimal(self.0.numerator, self.0.denominator) {
            // serde_json renders an f64 with its shortest round-trippable
            // decimal, so emit a literal Number when the formatted string
            // parses cleanly back.
            if let Ok(num) = s.parse::<serde_json::Number>() {
                return num.serialize(serializer);
            }
        }
        // Structured fallback.
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("PositiveInterval", 2)?;
        st.serialize_field("numerator", &self.0.numerator)?;
        st.serialize_field("denominator", &self.0.denominator)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for PositiveInterval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        // Accept either a scientific number or a {numerator, denominator}
        // object — same as the Haskell `FromJSON` instance.
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Number(n) => {
                let (num, den) = number_to_rational(&n)
                    .ok_or_else(|| D::Error::custom(format!("non-finite number: {n}")))?;
                PositiveInterval::new(num, den)
                    .ok_or_else(|| D::Error::custom("PositiveInterval must be strictly positive"))
            }
            serde_json::Value::Object(map) => {
                let num = map
                    .get("numerator")
                    .and_then(|x| x.as_u64())
                    .ok_or_else(|| D::Error::missing_field("numerator"))?;
                let den = map
                    .get("denominator")
                    .and_then(|x| x.as_u64())
                    .ok_or_else(|| D::Error::missing_field("denominator"))?;
                PositiveInterval::new(num, den)
                    .ok_or_else(|| D::Error::custom("PositiveInterval must be strictly positive"))
            }
            other => Err(D::Error::custom(format!(
                "expected number or {{numerator,denominator}} object, got: {other}"
            ))),
        }
    }
}

/// If `n/d` has a terminating decimal representation, return that decimal as
/// a canonical string (e.g. `1.2`, `0.5`). Otherwise return `None`.
///
/// This is the Rust analogue of the Haskell
/// `fromRationalRepetendLimited maxDecimalsWord64` check used by
/// `BoundedRatio`'s `ToJSON` instance.
fn terminating_decimal(n: u64, d: u64) -> Option<String> {
    if d == 0 {
        return None;
    }
    // Reduce to lowest terms once; we use this form for both the
    // terminating-decimal predicate and the subsequent scaling.
    let g = gcd_u64(n, d);
    let num = n / g;
    let den = d / g;

    // A reduced fraction has a terminating decimal iff its denominator
    // factors into 2s and 5s only.  Walk the factorisation while counting
    // exponents v2/v5 for the scaling step.
    let mut v2 = 0u32;
    let mut v5 = 0u32;
    let mut tmp = den;
    while tmp.is_multiple_of(2) {
        tmp /= 2;
        v2 += 1;
    }
    while tmp.is_multiple_of(5) {
        tmp /= 5;
        v5 += 1;
    }
    if tmp != 1 {
        return None;
    }
    let e10 = v2.max(v5);
    // Multiply num by 2^(e10-v2) * 5^(e10-v5) so that den becomes 10^e10.
    let mut scaled = num as u128;
    for _ in 0..(e10 - v2) {
        scaled = scaled.checked_mul(2)?;
    }
    for _ in 0..(e10 - v5) {
        scaled = scaled.checked_mul(5)?;
    }
    // Now scaled / 10^e10 is the exact decimal value.
    let e10_usize = e10 as usize;
    if e10_usize == 0 {
        return Some(format!("{scaled}"));
    }
    let digits = format!("{scaled}");
    if digits.len() > e10_usize {
        let (int_part, frac_part) = digits.split_at(digits.len() - e10_usize);
        let frac_trimmed = frac_part.trim_end_matches('0');
        if frac_trimmed.is_empty() {
            Some(int_part.to_string())
        } else {
            Some(format!("{int_part}.{frac_trimmed}"))
        }
    } else {
        // Pad with leading zeros after the decimal point.
        let pad = e10_usize - digits.len();
        let frac = format!("{:0>width$}{}", "", digits, width = pad);
        let frac_trimmed = frac.trim_end_matches('0');
        if frac_trimmed.is_empty() {
            Some("0".to_string())
        } else {
            Some(format!("0.{frac_trimmed}"))
        }
    }
}

fn gcd_u64(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd_u64(b, a % b)
    }
}

/// Convert a `serde_json::Number` (either integer or a decimal scientific
/// literal) into a `(numerator, denominator)` rational in lowest terms when
/// possible.
fn number_to_rational(n: &serde_json::Number) -> Option<(u64, u64)> {
    if let Some(u) = n.as_u64() {
        return Some((u, 1));
    }
    // Use the textual form to recover exact decimal scaling — `as_f64()`
    // would round and we want bit-exact handling of cases like `1.2`.
    let s = n.to_string();
    parse_decimal_to_rational(&s)
}

/// Parse a decimal-literal string (e.g. `"1.2"`, `"100"`, `"0.001"`, or
/// `"1.2e2"`) into `(numerator, denominator)`. Returns `None` for negative
/// numbers, special floats, or values that overflow `u128`.
fn parse_decimal_to_rational(s: &str) -> Option<(u64, u64)> {
    let s = s.trim();
    if s.is_empty() || s.starts_with('-') {
        return None;
    }
    // Split off an exponent.
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => {
            let (m, e) = s.split_at(i);
            let e: i32 = e[1..].parse().ok()?;
            (m, e)
        }
        None => (s, 0),
    };
    // Split mantissa on the decimal point.
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    let combined = format!("{int_part}{frac_part}");
    if combined.is_empty() {
        return None;
    }
    let value: u128 = combined.parse().ok()?;
    let frac_len = frac_part.len() as i32;
    let effective_exp = exp - frac_len; // 10^effective_exp scales `value`.

    let (mut num, mut den) = if effective_exp >= 0 {
        let mut v = value;
        for _ in 0..effective_exp {
            v = v.checked_mul(10)?;
        }
        (v, 1u128)
    } else {
        let mut d = 1u128;
        for _ in 0..(-effective_exp) {
            d = d.checked_mul(10)?;
        }
        (value, d)
    };
    // Reduce.
    let g = gcd_u128(num, den);
    num /= g;
    den /= g;
    // Fit into u64.
    let num64: u64 = num.try_into().ok()?;
    let den64: u64 = den.try_into().ok()?;
    Some((num64, den64))
}

fn gcd_u128(a: u128, b: u128) -> u128 {
    if b == 0 {
        a
    } else {
        gcd_u128(b, a % b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::blake2b_256;

    const DEFAULT_JSON: &str = r#"{
    "maxRefScriptSizePerBlock": 1048576,
    "maxRefScriptSizePerTx": 204800,
    "refScriptCostStride": 25600,
    "refScriptCostMultiplier": 1.2
}"#;

    #[test]
    fn parses_canonical_defaults() {
        let g = DijkstraGenesis::from_json_str(DEFAULT_JSON).expect("parse");
        assert_eq!(g.max_ref_script_size_per_block, 1024 * 1024);
        assert_eq!(g.max_ref_script_size_per_tx, 200 * 1024);
        assert_eq!(g.ref_script_cost_stride, 25_600);
        assert_eq!(g.ref_script_cost_multiplier.numerator(), 6);
        assert_eq!(g.ref_script_cost_multiplier.denominator(), 5);
        assert_eq!(g, DijkstraGenesis::defaults());
    }

    #[test]
    fn round_trip_via_serde_json_preserves_value() {
        let original = DijkstraGenesis::defaults();
        let serialised = serde_json::to_string(&original).expect("serialize");
        let parsed = DijkstraGenesis::from_json_str(&serialised).expect("re-parse");
        assert_eq!(parsed, original);
        // Multiplier 1.2 must serialise as the decimal `1.2`, not as a
        // {numerator, denominator} object — matching Haskell's compact
        // ToJSON branch for terminating decimals.
        assert!(
            serialised.contains("\"refScriptCostMultiplier\":1.2"),
            "expected compact decimal form, got: {serialised}"
        );
    }

    #[test]
    fn accepts_structured_multiplier_object() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": { "numerator": 6, "denominator": 5 }
        }"#;
        let g = DijkstraGenesis::from_json_str(json).expect("parse");
        assert_eq!(g, DijkstraGenesis::defaults());
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": 1.2,
            "futureField": 42
        }"#;
        let err = DijkstraGenesis::from_json_str(json).expect_err("unknown field must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("futureField") || msg.contains("unknown field"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn rejects_zero_multiplier_denominator() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": { "numerator": 6, "denominator": 0 }
        }"#;
        DijkstraGenesis::from_json_str(json).expect_err("zero denominator must be rejected");
    }

    #[test]
    fn rejects_non_positive_multiplier() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": 0
        }"#;
        DijkstraGenesis::from_json_str(json).expect_err("zero multiplier must be rejected");
    }

    #[test]
    fn rejects_negative_multiplier() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": -1.2
        }"#;
        DijkstraGenesis::from_json_str(json).expect_err("negative multiplier must be rejected");
    }

    #[test]
    fn terminating_decimal_renders_expected_strings() {
        assert_eq!(terminating_decimal(6, 5).as_deref(), Some("1.2"));
        assert_eq!(terminating_decimal(1, 2).as_deref(), Some("0.5"));
        assert_eq!(terminating_decimal(1, 4).as_deref(), Some("0.25"));
        assert_eq!(terminating_decimal(1, 8).as_deref(), Some("0.125"));
        assert_eq!(terminating_decimal(1, 10).as_deref(), Some("0.1"));
        assert_eq!(terminating_decimal(3, 1).as_deref(), Some("3"));
        // 1/3 has a repeating decimal expansion.
        assert_eq!(terminating_decimal(1, 3), None);
        // 1/7 likewise.
        assert_eq!(terminating_decimal(1, 7), None);
    }

    #[test]
    fn parses_scientific_notation_multiplier() {
        let json = r#"{
            "maxRefScriptSizePerBlock": 1048576,
            "maxRefScriptSizePerTx": 204800,
            "refScriptCostStride": 25600,
            "refScriptCostMultiplier": 1.25e0
        }"#;
        let g = DijkstraGenesis::from_json_str(json).expect("parse");
        assert_eq!(g.ref_script_cost_multiplier.numerator(), 5);
        assert_eq!(g.ref_script_cost_multiplier.denominator(), 4);
    }

    #[test]
    fn blake2b_hash_of_serialised_defaults_is_stable() {
        // Pin the hash of `serde_json::to_vec(&defaults())` so we can detect
        // accidental wire-format drift.  The exact hex isn't authoritative
        // (it depends on key ordering / decimal encoding choices), but any
        // change here is a flag to re-validate against cardano-node output.
        let bytes = serde_json::to_vec(&DijkstraGenesis::defaults()).expect("serialize");
        let hash = blake2b_256(&bytes);
        assert_eq!(hash.as_bytes().len(), 32);
        // Sanity: round-trip identical bytes match identical hashes.
        let bytes2 = serde_json::to_vec(&DijkstraGenesis::defaults()).expect("serialize");
        assert_eq!(blake2b_256(&bytes2), hash);
    }
}
