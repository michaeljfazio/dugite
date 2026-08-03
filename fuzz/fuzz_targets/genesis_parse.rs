//! Fuzz the `dugite-node` genesis parsers (issue #975).
//!
//! ## Why this is not merely untrusted-input hardening
//!
//! A genesis parse bug is a **consensus divergence**. v2.2.0 fixed exactly
//! that: float→rational conversion used a fixed 1e6 denominator and rounded
//! into it, which is exact only for values expressible in millionths. Mainnet's
//! real `priceSteps` is `0.0000721`; `round(0.0000721 * 1e6) = 72` gives
//! `9/125000 = 0.000072` — a 0.14% error silently applied to every script fee
//! dugite computed. The same conversion feeds `a0`, `rho`, `tau` and every
//! governance voting threshold.
//!
//! `crates/dugite-node/src/genesis.rs` has seven `load*` entry points and
//! nothing fuzzed any of them, because the module was only reachable from
//! `main.rs`. #975 exposed it through the lib target.
//!
//! ## Properties
//!
//! - no parser panics on arbitrary input (they may only return `Err`)
//! - `ShelleyGenesis::validate` never panics on a value that deserialised —
//!   it exists to reject degenerate genesis files at startup rather than
//!   dividing by zero later in consensus (#545 E8/E9, #546)
//! - **every float→rational conversion is exact** whenever the value's decimal
//!   form fits in `u64/u64`. This is the v2.2.0 property stated directly: a
//!   conversion that silently rounds is the defect, and a conversion that is
//!   merely *a* rational is not enough.
//!
//! Seeded from every network's real genesis JSON, so the mutator works from a
//! document that parses rather than from random bytes.
//!
//! Run with: cargo +nightly fuzz run fuzz_genesis_parse -- -max_total_time=300

#![no_main]

// The node's parsers are compiled in directly rather than reached through the
// `dugite-node` crate.
//
// Depending on the crate pulls in `mithril-client`, whose native deps and
// `inventory`/`typetag` static initializers do not survive sancov
// instrumentation (`ld: initializer pointer has no target`) — it broke the
// build of every target in this workspace, not just this one. `genesis.rs` has
// no `crate::` references at all, so compiling the file directly costs nothing
// and keeps the fuzz workspace buildable everywhere. `dugite-node`'s own
// lib.rs uses the same `#[path]` technique for the N2C encoder.
// These files are `pub` in dugite-node, but inside a fuzz binary they are a
// private module, so every item this target does not call trips dead_code.
// That is an artefact of the inclusion, not a finding.
#[allow(dead_code)]
#[path = "../../crates/dugite-node/src/genesis.rs"]
mod genesis;

use genesis::{AlonzoGenesis, AlonzoRational, ByronGenesis, ConwayGenesis, ShelleyGenesis};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // Byron / Conway: parse only — no rational conversion on this path.
    let _ = serde_json::from_str::<ByronGenesis>(text);
    let _ = serde_json::from_str::<ConwayGenesis>(text);

    if let Ok(shelley) = serde_json::from_str::<ShelleyGenesis>(text) {
        // Must return Err, never panic, on a degenerate genesis.
        let _ = shelley.validate();
    }

    if let Ok(alonzo) = serde_json::from_str::<AlonzoGenesis>(text) {
        for (name, price) in [
            ("priceMem", &alonzo.execution_prices.pr_mem),
            ("priceSteps", &alonzo.execution_prices.pr_steps),
        ] {
            let rational = price.to_rational();

            assert!(
                rational.denominator != 0,
                "{name}: zero denominator would divide by zero in fee calculation",
            );

            // Only assert exactness where exactness is achievable: the
            // conversion documents a bounded fallback for decimals that cannot
            // be represented in u64/u64 at all.
            let Some(value) = price_as_f64(price) else {
                continue;
            };
            if !decimal_fits_u64(value) {
                continue;
            }

            let reconstructed = rational.numerator as f64 / rational.denominator as f64;
            assert!(
                reconstructed == value,
                "{name}: float->rational conversion LOST PRECISION.\n\
                 This is the v2.2.0 defect: a 1e6-denominator conversion turns \
                 mainnet's priceSteps 0.0000721 into 9/125000 = 0.000072, a \
                 0.14% error in every script fee.\n\
                 input        = {value}\n\
                 rational     = {}/{}\n\
                 reconstructed= {reconstructed}",
                rational.numerator,
                rational.denominator,
            );
        }
    }
});

/// The f64 behind an Alonzo price, for the struct and float spellings alike.
fn price_as_f64(price: &AlonzoRational) -> Option<f64> {
    match price {
        AlonzoRational::Float(f) => Some(*f),
        // The struct spelling is already exact; there is nothing to lose.
        AlonzoRational::Struct { .. } => None,
    }
}

/// Whether the shortest decimal form of `f` is representable as `u64/u64`.
///
/// Deliberately conservative and independent of the parser under test: it
/// decides only WHETHER exactness is required, never what the answer is.
fn decimal_fits_u64(f: f64) -> bool {
    if !f.is_finite() || f <= 0.0 {
        return false;
    }
    let s = format!("{f}");
    // Exponential forms and anything with a sign are left to the fallback.
    if s.contains(['e', 'E', '-']) {
        return false;
    }
    let Some((int_part, frac_part)) = s.split_once('.') else {
        return true;
    };
    // numerator = all digits; denominator = 10^frac_len. Both must fit u64.
    let digits = int_part.len() + frac_part.len();
    digits <= 19 && 10u64.checked_pow(frac_part.len() as u32).is_some()
}
