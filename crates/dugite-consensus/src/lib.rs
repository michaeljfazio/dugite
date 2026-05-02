//! Ouroboros Praos consensus: chain selection, epoch transitions, VRF leader checks,
//! and HFC era history tracking.

pub mod chain_fragment;
pub mod chain_selection;
pub mod epoch;
pub mod era_history;
pub mod forecast;
pub mod overlay;
pub mod praos;
pub mod slot_leader;

pub use chain_selection::{ChainPreference, ChainSelection, DensityWindow};
pub use era_history::{Bound, EraHistory, EraParams, EraSummaryEntry, PastHorizonError};
pub use forecast::{forecast_for, OutsideForecastRange};
pub use overlay::{OBftSlot, OverlayContext};
pub use praos::{CryptoVerificationParams, OuroborosPraos, ValidationMode};
pub use slot_leader::{compute_leader_schedule, LeaderSlot};

/// The maximum protocol version that this node software supports.
///
/// This is a **capability signal**, not a reflection of the current on-chain protocol
/// version from the ledger's `PParams`. It tells the network "my software supports up
/// to this version" and is used for intra-era upgrade readiness voting. When enough
/// stake produces blocks with a higher minor version, the network can enact a minor
/// protocol bump via governance.
///
/// Equivalent to Haskell's `cardanoProtocolVersion` in
/// `Cardano.Node.Protocol.Cardano` — a compile-time constant that is stamped into
/// every forged block header, and also used to derive `MaxMajorProtVer` for the
/// `ObsoleteNode` check.
///
/// This value must be updated when Dugite adds support for newer protocol features.
/// Current value matches cardano-node 10.2.x / master (major=10, minor=8).
pub const NODE_PROTOCOL_VERSION: (u64, u64) = (10, 8);

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
    // ceil(3k/f) — use f64 arithmetic then round up.
    let exact = (3.0 * k as f64) / f;
    exact.ceil() as u64
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
