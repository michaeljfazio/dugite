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
pub mod gsm;
pub mod verify_snapshot;
