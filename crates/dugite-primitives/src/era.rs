use serde::{Deserialize, Serialize};

/// Cardano ledger eras
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Era {
    /// Byron era (original OBFT consensus)
    Byron,
    /// Shelley era (decentralization, staking)
    Shelley,
    /// Allegra era (token locking, time-lock scripts)
    Allegra,
    /// Mary era (multi-asset support)
    Mary,
    /// Alonzo era (Plutus smart contracts)
    Alonzo,
    /// Babbage era (Plutus V2, reference inputs/scripts)
    Babbage,
    /// Conway era (on-chain governance, CIP-1694)
    Conway,
    /// Dijkstra era (protocol version 12, storage era tag 8, HFC index 7)
    Dijkstra,
}

impl Era {
    pub fn is_shelley_based(&self) -> bool {
        !matches!(self, Era::Byron)
    }

    /// Whether blocks of this era are validated under **TPraos** — the
    /// transitional Praos that carries a BFT overlay schedule — rather than
    /// **Praos**.
    ///
    /// Haskell selects the consensus protocol by ERA, not by the value of `d`
    /// in the ledger state. `ouroboros-consensus-cardano`'s `HFEras` wires
    /// `ShelleyBlock (TPraos c)` for Shelley/Allegra/Mary/Alonzo and
    /// `ShelleyBlock (Praos c)` for Babbage onward, and the Praos
    /// `LedgerView` carries neither `d` nor `GenDelegs` — so the OVERLAY rule
    /// is not merely skipped for Babbage+ headers, it is *unreachable*.
    ///
    /// dugite has a single header validator for both, so the overlay check is
    /// gated at its call sites. That gate MUST key off the block's era, never
    /// off ledger pparams: a corrupt or stale ledger state carrying
    /// pre-Babbage pparams would otherwise run the overlay classifier on a
    /// Praos header and falsely reject a canonical block (#985).
    ///
    /// Byron answers `false` — it has no Praos header crypto at all and is
    /// validated on the ledger path, never reaching the overlay gate.
    pub fn uses_tpraos(&self) -> bool {
        matches!(self, Era::Shelley | Era::Allegra | Era::Mary | Era::Alonzo)
    }

    pub fn supports_native_assets(&self) -> bool {
        matches!(
            self,
            Era::Mary | Era::Alonzo | Era::Babbage | Era::Conway | Era::Dijkstra
        )
    }

    pub fn supports_plutus(&self) -> bool {
        matches!(
            self,
            Era::Alonzo | Era::Babbage | Era::Conway | Era::Dijkstra
        )
    }

    pub fn supports_plutus_v2(&self) -> bool {
        matches!(self, Era::Babbage | Era::Conway | Era::Dijkstra)
    }

    pub fn supports_plutus_v3(&self) -> bool {
        matches!(self, Era::Conway | Era::Dijkstra)
    }

    /// PlutusV4 introduced in Dijkstra (issue #475 Phase 5).
    pub fn supports_plutus_v4(&self) -> bool {
        matches!(self, Era::Dijkstra)
    }

    pub fn supports_governance(&self) -> bool {
        matches!(self, Era::Conway | Era::Dijkstra)
    }

    pub fn supports_reference_inputs(&self) -> bool {
        matches!(self, Era::Babbage | Era::Conway | Era::Dijkstra)
    }

    pub fn supports_inline_datums(&self) -> bool {
        matches!(self, Era::Babbage | Era::Conway | Era::Dijkstra)
    }

    pub fn supports_reference_scripts(&self) -> bool {
        matches!(self, Era::Babbage | Era::Conway | Era::Dijkstra)
    }

    /// Derive the era from the on-chain protocol version major number.
    ///
    /// This mapping follows the Cardano ledger convention:
    /// - Byron: major 0-1
    /// - Shelley: major 2
    /// - Allegra: major 3
    /// - Mary: major 4
    /// - Alonzo: major 5-6
    /// - Babbage: major 7-8
    /// - Conway: major 9-11
    /// - Dijkstra: major 12+
    pub fn from_protocol_major(major: u64) -> Era {
        match major {
            0..=1 => Era::Byron,
            2 => Era::Shelley,
            3 => Era::Allegra,
            4 => Era::Mary,
            5..=6 => Era::Alonzo,
            7..=8 => Era::Babbage,
            9..=11 => Era::Conway,
            _ => Era::Dijkstra, // 12+ = Dijkstra
        }
    }

    /// Era index for the N2C protocol (hard-fork combinator era index)
    pub fn to_era_index(self) -> u32 {
        match self {
            Era::Byron => 0,
            Era::Shelley => 1,
            Era::Allegra => 2,
            Era::Mary => 3,
            Era::Alonzo => 4,
            Era::Babbage => 5,
            Era::Conway => 6,
            Era::Dijkstra => 7,
        }
    }
}

impl std::fmt::Display for Era {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Era::Byron => write!(f, "Byron"),
            Era::Shelley => write!(f, "Shelley"),
            Era::Allegra => write!(f, "Allegra"),
            Era::Mary => write!(f, "Mary"),
            Era::Alonzo => write!(f, "Alonzo"),
            Era::Babbage => write!(f, "Babbage"),
            Era::Conway => write!(f, "Conway"),
            Era::Dijkstra => write!(f, "Dijkstra"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_shelley_based() {
        assert!(!Era::Byron.is_shelley_based());
        assert!(Era::Shelley.is_shelley_based());
        assert!(Era::Conway.is_shelley_based());
    }

    #[test]
    fn test_supports_native_assets() {
        assert!(!Era::Byron.supports_native_assets());
        assert!(!Era::Shelley.supports_native_assets());
        assert!(!Era::Allegra.supports_native_assets());
        assert!(Era::Mary.supports_native_assets());
        assert!(Era::Conway.supports_native_assets());
    }

    #[test]
    fn test_supports_plutus() {
        assert!(!Era::Mary.supports_plutus());
        assert!(Era::Alonzo.supports_plutus());
        assert!(Era::Babbage.supports_plutus());
        assert!(Era::Conway.supports_plutus());
    }

    #[test]
    fn test_supports_plutus_v2_v3() {
        assert!(!Era::Alonzo.supports_plutus_v2());
        assert!(Era::Babbage.supports_plutus_v2());
        assert!(!Era::Babbage.supports_plutus_v3());
        assert!(Era::Conway.supports_plutus_v3());
    }

    #[test]
    fn test_supports_plutus_v4() {
        // V4 lights up exactly at the Dijkstra hard fork (issue #475 Phase 5).
        for prior in [
            Era::Byron,
            Era::Shelley,
            Era::Allegra,
            Era::Mary,
            Era::Alonzo,
            Era::Babbage,
            Era::Conway,
        ] {
            assert!(
                !prior.supports_plutus_v4(),
                "{prior:?} must NOT support V4 — V4 lights up at Dijkstra"
            );
        }
        assert!(Era::Dijkstra.supports_plutus_v4());
    }

    #[test]
    fn test_supports_governance() {
        assert!(!Era::Babbage.supports_governance());
        assert!(Era::Conway.supports_governance());
    }

    #[test]
    fn test_supports_reference_features() {
        assert!(!Era::Alonzo.supports_reference_inputs());
        assert!(Era::Babbage.supports_reference_inputs());
        assert!(Era::Babbage.supports_inline_datums());
        assert!(Era::Babbage.supports_reference_scripts());
        assert!(Era::Conway.supports_reference_inputs());
    }

    #[test]
    fn test_era_index() {
        assert_eq!(Era::Byron.to_era_index(), 0);
        assert_eq!(Era::Shelley.to_era_index(), 1);
        assert_eq!(Era::Allegra.to_era_index(), 2);
        assert_eq!(Era::Mary.to_era_index(), 3);
        assert_eq!(Era::Alonzo.to_era_index(), 4);
        assert_eq!(Era::Babbage.to_era_index(), 5);
        assert_eq!(Era::Conway.to_era_index(), 6);
        assert_eq!(Era::Dijkstra.to_era_index(), 7);
    }

    #[test]
    fn test_from_protocol_major() {
        assert_eq!(Era::from_protocol_major(0), Era::Byron);
        assert_eq!(Era::from_protocol_major(1), Era::Byron);
        assert_eq!(Era::from_protocol_major(2), Era::Shelley);
        assert_eq!(Era::from_protocol_major(3), Era::Allegra);
        assert_eq!(Era::from_protocol_major(4), Era::Mary);
        assert_eq!(Era::from_protocol_major(5), Era::Alonzo);
        assert_eq!(Era::from_protocol_major(6), Era::Alonzo);
        assert_eq!(Era::from_protocol_major(7), Era::Babbage);
        assert_eq!(Era::from_protocol_major(8), Era::Babbage);
        assert_eq!(Era::from_protocol_major(9), Era::Conway);
        assert_eq!(Era::from_protocol_major(10), Era::Conway);
        assert_eq!(Era::from_protocol_major(11), Era::Conway);
        assert_eq!(Era::from_protocol_major(12), Era::Dijkstra);
        assert_eq!(Era::from_protocol_major(100), Era::Dijkstra);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Era::Byron), "Byron");
        assert_eq!(format!("{}", Era::Conway), "Conway");
        assert_eq!(format!("{}", Era::Dijkstra), "Dijkstra");
    }

    #[test]
    fn test_ordering() {
        assert!(Era::Byron < Era::Shelley);
        assert!(Era::Shelley < Era::Conway);
        assert!(Era::Alonzo < Era::Babbage);
        assert!(Era::Conway < Era::Dijkstra);
    }

    /// #985. The set is spelled out rather than derived from an ordering so
    /// that adding a future era cannot silently extend it — a new era defaults
    /// to Praos, which is the safe answer, and this test states the whole
    /// mapping so the intent survives.
    ///
    /// Mirrors `ouroboros-consensus-cardano`'s `HFEras`:
    /// `ShelleyBlock (TPraos c)` for Shelley/Allegra/Mary/Alonzo,
    /// `ShelleyBlock (Praos c)` for Babbage/Conway/Dijkstra.
    #[test]
    fn tpraos_eras_are_exactly_shelley_through_alonzo() {
        for era in [Era::Shelley, Era::Allegra, Era::Mary, Era::Alonzo] {
            assert!(era.uses_tpraos(), "{era:?} is a TPraos era in cardano-node");
        }
        for era in [Era::Babbage, Era::Conway, Era::Dijkstra] {
            assert!(
                !era.uses_tpraos(),
                "{era:?} is a Praos era — its headers have no overlay schedule, \
                 and treating one as TPraos falsely rejects canonical blocks (#985)"
            );
        }
        // Byron predates both and never reaches the Praos header validator.
        assert!(!Era::Byron.uses_tpraos());
    }
}
