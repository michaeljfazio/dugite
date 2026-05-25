//! `WatchService` — pattern-filtered transaction watches.
//!
//! M1.A stubs: returns `UNIMPLEMENTED`. M4 fills WatchTx via a
//! [`TipFeed`](crate::tip_feed::TipFeed) +
//! [`MempoolFeed`](crate::mempool_feed::MempoolFeed) merge, with
//! `Pattern` filtering through `map/patterns.rs`.

use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::proto::{v1alpha, v1beta};

const UNIMPL: &str = "M1.A stub — WatchService method not implemented yet";

#[derive(Clone)]
pub struct WatchSvcAlpha {
    state: ServiceState,
}

impl WatchSvcAlpha {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1alpha::watch::watch_service_server::WatchService for WatchSvcAlpha {
    type WatchTxStream = ReceiverStream<Result<v1alpha::watch::WatchTxResponse, Status>>;

    async fn watch_tx(
        &self,
        _request: Request<v1alpha::watch::WatchTxRequest>,
    ) -> Result<Response<Self::WatchTxStream>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }
}

#[derive(Clone)]
pub struct WatchSvcBeta {
    state: ServiceState,
}

impl WatchSvcBeta {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1beta::watch::watch_service_server::WatchService for WatchSvcBeta {
    type WatchTxStream = ReceiverStream<Result<v1beta::watch::WatchTxResponse, Status>>;

    async fn watch_tx(
        &self,
        _request: Request<v1beta::watch::WatchTxRequest>,
    ) -> Result<Response<Self::WatchTxStream>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }
}
