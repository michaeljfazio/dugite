//! Slot-horizon forecast check.
//!
//! Reference: `Ouroboros.Consensus.Forecast.OutsideForecastRange` and the
//! Praos forecast computation in `Ouroboros.Consensus.Shelley.Ledger.SupportsProtocol`.
//!
//! When the ledger needs to validate a future slot's view (e.g. VRF/KES checks
//! against an upcoming block), the forecast can only project so far into the
//! future before the ledger view becomes uncertain. The valid forecast window
//! is `[at, max_for)` where `max_for = succ_with_origin(at) + stability_window`
//! and `succ_with_origin(at)` returns `0` for origin and `at + 1` otherwise.
//!
//! `stability_window = ceil(3 * k / f)` — the same formula used elsewhere in
//! the consensus layer.

use dugite_primitives::time::SlotNo;
use thiserror::Error;

/// Error returned when a forecast is requested for a slot beyond the valid
/// horizon `[at, max_for)`.
///
/// Reference: Haskell `OutsideForecastRange` in `Ouroboros.Consensus.Forecast`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("OutsideForecastRange: at={at:?}, max_for={max_for}, requested={requested}")]
pub struct OutsideForecastRange {
    /// Ledger tip slot at the time of the forecast (None at origin).
    pub at: Option<SlotNo>,
    /// Exclusive upper bound on forecastable slots.
    pub max_for: SlotNo,
    /// Slot for which the forecast was requested.
    pub requested: SlotNo,
}

/// Check whether `requested` lies within the forecast window `[at, max_for)`
/// where `max_for = succ_with_origin(at) + stability_window`.
///
/// `succ_with_origin(at)` is `0` at origin and `at + 1` otherwise.
///
/// Reference: Haskell `forecastFor` in `Ouroboros.Consensus.NodeKernel`.
pub fn forecast_for(
    at: Option<SlotNo>,
    stability_window: u64,
    requested: SlotNo,
) -> Result<(), OutsideForecastRange> {
    let succ_at = match at {
        Some(s) => s.0.saturating_add(1),
        None => 0,
    };
    let max_for = SlotNo(succ_at.saturating_add(stability_window));
    if requested.0 < max_for.0 {
        Ok(())
    } else {
        Err(OutsideForecastRange {
            at,
            max_for,
            requested,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outside_forecast_range_beyond_horizon() {
        // tip 1000, sw 100 → max_for = 1001 + 100 = 1101.  requested 1101 → err.
        let err = forecast_for(Some(SlotNo(1000)), 100, SlotNo(1101)).unwrap_err();
        assert_eq!(err.at, Some(SlotNo(1000)));
        assert_eq!(err.max_for, SlotNo(1101));
        assert_eq!(err.requested, SlotNo(1101));
    }

    #[test]
    fn test_forecast_within_horizon_ok() {
        // tip 1000, sw 100 → max_for = 1101.
        // requested 1100 → ok (within window).
        forecast_for(Some(SlotNo(1000)), 100, SlotNo(1100)).unwrap();
        // requested 1101 → err (boundary, exclusive).
        assert!(forecast_for(Some(SlotNo(1000)), 100, SlotNo(1101)).is_err());
    }

    #[test]
    fn test_forecast_at_origin_ok() {
        // origin, sw 100 → succ = 0 → max_for = 100.
        // requested 99 → ok.
        forecast_for(None, 100, SlotNo(99)).unwrap();
        // requested 101 → err (past horizon).
        assert!(forecast_for(None, 100, SlotNo(101)).is_err());
    }

    #[test]
    fn test_forecast_at_origin_zero() {
        // origin, sw 100 → max_for = 100.  requested 0 → ok.
        forecast_for(None, 100, SlotNo(0)).unwrap();
    }

    #[test]
    fn test_forecast_max_for_exclusive() {
        // tip 500, sw 50 → succ = 501 → max_for = 551.
        // requested 550 → ok.
        forecast_for(Some(SlotNo(500)), 50, SlotNo(550)).unwrap();
        // requested 551 → err (max_for is exclusive).
        let err = forecast_for(Some(SlotNo(500)), 50, SlotNo(551)).unwrap_err();
        assert_eq!(err.max_for, SlotNo(551));
        assert_eq!(err.requested, SlotNo(551));
    }

    #[test]
    fn test_forecast_at_origin_boundary() {
        // origin, sw 100 → max_for = 100.  requested 100 → err (boundary, exclusive).
        let err = forecast_for(None, 100, SlotNo(100)).unwrap_err();
        assert_eq!(err.at, None);
        assert_eq!(err.max_for, SlotNo(100));
    }

    #[test]
    fn test_forecast_zero_stability_window() {
        // sw = 0 → max_for = succ(at).  Always errors except when requested < succ(at).
        // tip 1000 → max_for = 1001 → requested 1000 → ok, 1001 → err.
        forecast_for(Some(SlotNo(1000)), 0, SlotNo(1000)).unwrap();
        assert!(forecast_for(Some(SlotNo(1000)), 0, SlotNo(1001)).is_err());
    }
}
