//! Ouroboros Praos consensus: chain selection, epoch transitions, VRF leader checks,
//! and HFC era history tracking.

pub mod chain_fragment;
pub mod chain_selection;
pub mod epoch;
pub mod era_history;
pub mod forecast;
pub mod loe;
pub mod overlay;
pub mod peras_wire;
pub mod praos;
pub mod slot_leader;

pub use chain_selection::{
    ChainPreference, ChainSelection, CsjDissent, DensityWindow, GenesisDensityComparator,
};
pub use era_history::{Bound, EraHistory, EraParams, EraSummaryEntry, PastHorizonError};
pub use forecast::{forecast_for, OutsideForecastRange};
pub use overlay::{OBftSlot, OverlayContext};
pub use peras_wire::{
    decode_bound, decode_era_params, encode_bound, encode_era_params, BoundWire, EraParamsWire,
    PerasWireError,
};
pub use praos::{ConsensusError, CryptoVerificationParams, OuroborosPraos, ValidationMode};
pub use slot_leader::{compute_leader_schedule, LeaderSlot};

/// Compute the stability window in slots: `ceil(3 * k / f)`.
///
/// This is the maximum number of slots that the chain tip may lag behind the
/// wall-clock before a ledger-view forecast becomes impossible (Haskell:
/// `TraceNoLedgerView`).  Used by the forge loop to gate leader-election
/// attempts when the node is too far behind to produce a valid block.
///
/// # Reference
/// Haskell `forecastFor` in `Ouroboros.Consensus.NodeKernel.forkBlockForging`:
/// the forecast fails when `currentSlot >= tipSlot + 1 + stabilityWindow`.
/// `stabilityWindow = ceil(3 * k / f)`.
///
/// For preview / mainnet (k = 2160, f = 0.05) this equals **129 600 slots
/// (36 hours)**.
///
/// # Panics
/// Panics in debug mode if `f <= 0.0`.  Returns `u64::MAX` in release mode
/// when `f <= 0.0` (effectively disabling the gate, which is safe).
pub fn stability_window_slots(k: u64, f: f64) -> u64 {
    if f <= 0.0 {
        // Degenerate: disable the gate (caller treats u64::MAX as "no limit").
        return u64::MAX;
    }
    // Haskell `computeStabilityWindow` is `ceiling (3*k /. f)` over EXACT
    // rationals — `f` originates as a decimal literal in the genesis JSON
    // (e.g. 0.05).  Our callers carry it as the f64 serde parsed from that
    // literal.  Recover the exact decimal via shortest-roundtrip formatting
    // (for any human-written genesis decimal this IS the original literal),
    // then compute `ceil(3k·den/num)` in pure u128 integer arithmetic —
    // no float division, no epsilon (#733 correction 7).
    if let Some((num, den)) = f64_to_decimal_rational(f) {
        let prod = 3u128 * (k as u128) * den;
        let ceiled = prod.div_ceil(num);
        if let Ok(v) = u64::try_from(ceiled) {
            return v;
        }
    }
    // Fallback (non-decimal-representable f, e.g. synthetic test values):
    // previous float path with nearest-integer snap.
    let exact = (3.0 * k as f64) / f;
    let nearest = exact.round();
    if (exact - nearest).abs() < 1e-6 {
        nearest as u64
    } else {
        exact.ceil() as u64
    }
}

/// Recover the exact decimal rational `num/den` whose shortest-roundtrip
/// decimal representation produced this f64 (e.g. `0.05` → `(5, 100)`).
///
/// Returns `None` for non-finite values, exponent-formatted extremes, or
/// decimals longer than u128 arithmetic can hold.
fn f64_to_decimal_rational(f: f64) -> Option<(u128, u128)> {
    if !f.is_finite() || f <= 0.0 {
        return None;
    }
    let s = format!("{f}");
    if s.contains(['e', 'E']) {
        return None;
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, fr)) => (i, fr),
        None => (s.as_str(), ""),
    };
    if frac_part.len() > 30 {
        return None;
    }
    let den = 10u128.checked_pow(frac_part.len() as u32)?;
    let int_val: u128 = int_part.parse().ok()?;
    let frac_val: u128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().ok()?
    };
    let num = int_val.checked_mul(den)?.checked_add(frac_val)?;
    if num == 0 {
        return None;
    }
    Some((num, den))
}

#[cfg(test)]
mod tests {
    use super::stability_window_slots;

    #[test]
    fn stability_window_slots_preview() {
        // Preview / mainnet: k=2160, f=0.05 → 3*2160/0.05 = 129 600 exactly.
        assert_eq!(
            stability_window_slots(2160, 0.05),
            129_600,
            "stability window for k=2160, f=0.05 must be 129600 slots"
        );
    }

    #[test]
    fn stability_window_slots_mainnet() {
        // Mainnet uses the same params as preview testnet.
        assert_eq!(stability_window_slots(2160, 0.05), 129_600);
    }

    #[test]
    fn stability_window_slots_snaps_float_noise_to_exact_integer() {
        // 3k/f exactly an integer in ℚ must NOT round up to +1 due to f64
        // representation noise. Sweep the real-world coefficient values.
        for (k, f, expect) in [
            (2160u64, 0.05f64, 129_600u64), // mainnet/preprod/preview ratio
            (432, 0.05, 25_920),            // preview k
            (10, 0.1, 300),                 // devnet-style
            (10, 0.2, 150),                 // devnet-style
            (2160, 0.2, 32_400),
        ] {
            assert_eq!(
                stability_window_slots(k, f),
                expect,
                "k={k} f={f} must be exactly {expect}"
            );
        }
    }

    #[test]
    fn stability_window_slots_rounds_up() {
        // When 3k/f is not an integer, result is rounded up.
        // k=1, f=0.3 → 3/0.3 = 10.0 exactly → 10.
        assert_eq!(stability_window_slots(1, 0.3), 10);
        // k=1, f=0.7 → 3/0.7 ≈ 4.2857 → ceil → 5.
        assert_eq!(stability_window_slots(1, 0.7), 5);
    }

    #[test]
    fn stability_window_slots_zero_f_returns_max() {
        // Degenerate: f=0 → u64::MAX (gate is disabled).
        assert_eq!(stability_window_slots(2160, 0.0), u64::MAX);
    }

    #[test]
    fn stability_window_slots_exact_integer_math() {
        // #733 correction 7: exact rational ceiling, mirroring Haskell
        // computeStabilityWindow = ceiling (3k /. f) over ℚ.
        // f=0.049 → 3·2160·1000/49 = 6_480_000/49 = 132244.897… → 132245.
        assert_eq!(stability_window_slots(2160, 0.049), 132_245);
        // f=0.07 → 3·1000·100/7 = 300_000/7 = 42857.14… → 42858.
        assert_eq!(stability_window_slots(1000, 0.07), 42_858);
        // Large k where f64 noise could flip a naive ceil: 3k/f exact.
        assert_eq!(stability_window_slots(1_000_000_007, 0.05), 60_000_000_420);
    }

    #[test]
    fn f64_to_decimal_rational_recovers_genesis_literals() {
        assert_eq!(super::f64_to_decimal_rational(0.05), Some((5, 100)));
        assert_eq!(super::f64_to_decimal_rational(0.1), Some((1, 10)));
        assert_eq!(super::f64_to_decimal_rational(1.0), Some((1, 1)));
        assert_eq!(super::f64_to_decimal_rational(0.2), Some((2, 10)));
        assert_eq!(super::f64_to_decimal_rational(0.0), None);
        assert_eq!(super::f64_to_decimal_rational(-0.05), None);
    }
}
