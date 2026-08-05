//! RPC service implementations — one module per utxorpc service.
//!
//! Each module exposes two thin wrappers (one for `v1alpha`, one for
//! `v1beta`) over the same backing state. `SyncService`, `QueryService`,
//! `SubmitService`, and `WatchService` are all implemented end-to-end —
//! see each module's own doc comment for per-method coverage and any
//! documented (not silently missing) limitations.

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

/// Extract a request's `FieldMask.paths`, defaulting to empty (no
/// `FieldMask` on the wire, or one with zero paths) — `masking::apply`
/// treats an empty slice as "return everything unpruned", matching the
/// canonical `google.protobuf.FieldMask` doc: *"If a FieldMask object is
/// not present in a get operation, the operation applies to all
/// fields."* Shared by every service module so there is exactly one
/// place that decides what "no mask" means.
pub(crate) fn mask_paths(field_mask: Option<prost_types::FieldMask>) -> Vec<String> {
    field_mask.map(|m| m.paths).unwrap_or_default()
}

/// Send a `masking::apply` result on a streaming response channel,
/// surfacing a masking failure to the client as `Status::internal`
/// instead of silently dropping it or (worse) sending the response
/// unmasked. One generic helper shared by every streaming service
/// (`FollowTip`, `WatchTx`, `WatchMempool`) rather than a per-type copy
/// — the same "one mechanism, not N drifting copies" reasoning behind
/// `masking::apply` itself.
///
/// Returns `true` if the caller's loop should stop: either the client
/// is gone (send failed) or masking failed. A masking failure is a
/// persistent programmer error (wrong `message_name` for `T`, or
/// descriptor drift) — it recurs identically on every future event, so
/// continuing to retry achieves nothing; ending the stream with one
/// clear error is strictly better than looping on the same failure (or
/// silently degrading) forever.
pub(crate) async fn send_masked<T: Send + 'static>(
    tx: &tokio::sync::mpsc::Sender<Result<T, tonic::Status>>,
    result: Result<T, crate::error::RpcError>,
) -> bool {
    match result {
        Ok(resp) => tx.send(Ok(resp)).await.is_err(),
        Err(e) => {
            let _ = tx.send(Err(tonic::Status::from(e))).await;
            true
        }
    }
}
