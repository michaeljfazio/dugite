/// Public API surface of dugite-node, exposed for integration testing.
///
/// The binary crate (main.rs) owns all module declarations and wires them
/// together into the full node. This lib target re-exports the items that
/// integration tests need to exercise the block forging pipeline without
/// starting a live network.
pub mod config;
pub mod config_reload;
pub mod csj_orchestrator;
pub mod forge;
pub mod genesis_governor;
pub mod genesis_params;
pub mod genesis_peer_state;
pub mod gsm;
pub mod snapshot_convert;
pub mod verify_snapshot;

/// `peer_connection` module exposed for the `test-utils` integration-test
/// feature so that `tests/lifecycle_invariants.rs` can construct fake
/// `PeerConnection` instances to exercise Fix-A (Hot→Warm→Hot channel
/// recovery) without a live N2N handshake.
///
/// Gated on `feature = "test-utils"` — not included in default or production
/// builds.  The binary crate references the same source files via `mod node;`
/// in `main.rs` as a separate compilation unit.
#[cfg(feature = "test-utils")]
pub mod node {
    /// `PeerConnection` and its lifecycle types, exposed for integration tests.
    pub mod peer_connection;
}
