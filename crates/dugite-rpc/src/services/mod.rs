//! RPC service implementations — one module per utxorpc service.
//!
//! Each module exposes two thin wrappers (one for `v1alpha`, one for
//! `v1beta`) over the same backing state. M1.A ships pure stubs that
//! return `UNIMPLEMENTED` for every method; M1.B fills `SyncService`
//! end-to-end; M2 fills `QueryService`; M3 `SubmitService`; M4
//! `WatchService`.

pub mod query;
pub mod submit;
pub mod sync;
pub mod watch;

/// Shared state held by every service implementation.
///
/// Cheap to clone (`Arc` over everything mutable). Held by both
/// `v1alpha` and `v1beta` wrappers of the same service so they back the
/// same source of truth.
#[derive(Clone)]
pub struct ServiceState {
    pub context: std::sync::Arc<dyn crate::context::LedgerContext>,
    pub tip_feed: crate::tip_feed::TipFeed,
    pub mempool_feed: crate::mempool_feed::MempoolFeed,
    pub metrics: crate::metrics::SharedMetrics,
    pub config: std::sync::Arc<crate::config::RpcConfig>,
}

impl std::fmt::Debug for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceState")
            .field("port", &self.config.port)
            .field("bind", &self.config.bind)
            .finish_non_exhaustive()
    }
}
