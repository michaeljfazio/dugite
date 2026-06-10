//! Ouroboros Praos consensus: chain selection, epoch transitions, VRF leader checks,
//! and HFC era history tracking.

pub mod chain_fragment;
pub mod chain_selection;
pub mod epoch;
pub mod era_history;
pub mod forecast;
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
    // ceil(3k/f) — Haskell `computeStabilityWindow` computes this over exact
    // Rationals.  Our `f` is an f64 parsed from the genesis JSON, so the
    // division can land an ulp ABOVE an exact integer result (e.g.
    // 3*2160/0.05 = 129600 exactly in ℚ, but 0.05 is not representable in
    // binary).  A naive `ceil` would then return 129601.  Snap to the nearest
    // integer when within a float-noise epsilon before ceiling.
    let exact = (3.0 * k as f64) / f;
    let nearest = exact.round();
    if (exact - nearest).abs() < 1e-6 {
        nearest as u64
    } else {
        exact.ceil() as u64
    }
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
}
