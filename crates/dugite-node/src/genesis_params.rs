//! Network-derived Ouroboros Genesis parameters.
//!
//! Centralises every Genesis-mode constant that depends on the loaded
//! genesis configuration, replacing the previous mainnet-hardcoded
//! `GsmConfig` defaults (k=2160, sgen=129 600 regardless of network).
//!
//! # Haskell reference
//!
//! - `sgen` (genesis window): `computeStabilityWindow k f = ceiling (3k/f)`
//!   (cardano-ledger `Cardano.Ledger.Shelley.StabilityWindow`), surfaced
//!   per-era as `EraParams.eraGenesisWin` (`shelleyEraParams`).
//! - Historicity cutoff: `mkGenesisConfig` sets
//!   `gcHistoricityCutoff = 3*2160*20 + 3600` seconds for mainnet — one
//!   maximum-duration stability window (in WALL-CLOCK seconds, i.e.
//!   `sgen_slots × slot_length`) plus a one-hour safety margin.
//! - `MinBigLedgerPeersForTrustedState`: default
//!   `NumberOfBigLedgerPeers 5` (`defaultNumberOfBigLedgerPeers`,
//!   cardano-diffusion `Cardano.Network.Diffusion.Configuration`).

use crate::config::LowLevelGenesisOptions;

/// Genesis-mode parameters derived from the network's Shelley genesis plus
/// the operator's `LowLevelGenesisOptions`.
#[derive(Debug, Clone)]
pub struct GenesisParams {
    /// Security parameter `k` from the Shelley genesis.
    pub security_param_k: u64,
    /// Active slot coefficient `f` from the Shelley genesis.
    pub active_slot_coeff: f64,
    /// Shelley-era slot length in seconds (mainnet: 1.0).
    pub slot_length_secs: f64,
    /// Genesis window `sgen = ceil(3k/f)` in slots.
    pub sgen_slots: u64,
    /// Historicity cutoff in seconds: `sgen × slot_length + 3600`.
    pub historicity_cutoff_secs: u64,
    /// Minimum ACTIVE (hot) big-ledger peers for the Honest Availability
    /// Assumption (`MinBigLedgerPeersForTrustedState`, default 5).
    pub min_big_ledger_peers: usize,
    /// Low-level toggles and tunables (cardano-node `LowLevelGenesisOptions`).
    pub options: LowLevelGenesisOptions,
}

impl GenesisParams {
    /// Build from network genesis values.
    ///
    /// `slot_length_secs` is the SHELLEY-era slot length (the historicity
    /// cutoff and LoP rates are defined against Shelley-based eras; Byron
    /// headers never reach those subsystems in practice because the cutoff
    /// only matters near the wall-clock present).
    pub fn from_network(
        security_param_k: u64,
        active_slot_coeff: f64,
        slot_length_secs: f64,
        min_big_ledger_peers: usize,
        options: LowLevelGenesisOptions,
    ) -> Self {
        let sgen_slots =
            dugite_consensus::stability_window_slots(security_param_k, active_slot_coeff);
        // 3k/f slots of wall-clock time + 1 hour, matching
        // `gcHistoricityCutoff = 3 * 2160 * 20 + 3600` (mainnet f=0.05 ⇒
        // 3k/f slots × 1 s/slot = 3*2160*20 s).
        let historicity_cutoff_secs = (sgen_slots as f64 * slot_length_secs).round() as u64 + 3600;
        GenesisParams {
            security_param_k,
            active_slot_coeff,
            slot_length_secs,
            sgen_slots,
            historicity_cutoff_secs,
            min_big_ledger_peers,
            options,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_params() {
        // Mainnet/preprod: k=2160, f=0.05, 1 s slots.
        let p = GenesisParams::from_network(2160, 0.05, 1.0, 5, Default::default());
        assert_eq!(p.sgen_slots, 129_600);
        // gcHistoricityCutoff = 3*2160*20 + 3600 = 133_200 s.
        assert_eq!(p.historicity_cutoff_secs, 133_200);
        assert_eq!(p.min_big_ledger_peers, 5);
        // Upstream defaults flow through.
        assert!(p.options.enable_csj);
        assert_eq!(p.options.effective_csj_jump_size(), 4320);
    }

    #[test]
    fn preview_params() {
        // Preview: k=432, f=0.05, 1 s slots — sgen MUST be network-derived
        // (the old hardcoded 129 600 was a mainnet value).
        let p = GenesisParams::from_network(432, 0.05, 1.0, 5, Default::default());
        assert_eq!(p.sgen_slots, 25_920);
        assert_eq!(p.historicity_cutoff_secs, 25_920 + 3600);
    }

    #[test]
    fn devnet_params() {
        // Local devnet shape: k=10, f=0.1, 1 s slots.
        let p = GenesisParams::from_network(10, 0.1, 1.0, 5, Default::default());
        assert_eq!(p.sgen_slots, 300);
        assert_eq!(p.historicity_cutoff_secs, 300 + 3600);
    }
}
