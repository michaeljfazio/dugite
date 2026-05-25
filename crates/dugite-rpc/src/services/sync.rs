//! `SyncService` — block retrieval + chain following.
//!
//! M1.A stubs: every method returns `UNIMPLEMENTED`. M1.B fills
//! `FetchBlock`, `DumpHistory`, `ReadTip`, `FollowTip` with real
//! mappings against `LedgerContext::block_by_hash` / `blocks_range` /
//! `tip` / `TipFeed`.

use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::proto::{v1alpha, v1beta};

const UNIMPL: &str = "M1.A stub — SyncService method not implemented yet";

/// `v1alpha` SyncService wrapper.
#[derive(Clone)]
pub struct SyncSvcAlpha {
    state: ServiceState,
}

impl SyncSvcAlpha {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1alpha::sync::sync_service_server::SyncService for SyncSvcAlpha {
    async fn fetch_block(
        &self,
        _request: Request<v1alpha::sync::FetchBlockRequest>,
    ) -> Result<Response<v1alpha::sync::FetchBlockResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }

    async fn dump_history(
        &self,
        _request: Request<v1alpha::sync::DumpHistoryRequest>,
    ) -> Result<Response<v1alpha::sync::DumpHistoryResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type FollowTipStream = ReceiverStream<Result<v1alpha::sync::FollowTipResponse, Status>>;

    async fn follow_tip(
        &self,
        _request: Request<v1alpha::sync::FollowTipRequest>,
    ) -> Result<Response<Self::FollowTipStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tip(
        &self,
        _request: Request<v1alpha::sync::ReadTipRequest>,
    ) -> Result<Response<v1alpha::sync::ReadTipResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}

/// `v1beta` SyncService wrapper.
#[derive(Clone)]
pub struct SyncSvcBeta {
    state: ServiceState,
}

impl SyncSvcBeta {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1beta::sync::sync_service_server::SyncService for SyncSvcBeta {
    async fn fetch_block(
        &self,
        _request: Request<v1beta::sync::FetchBlockRequest>,
    ) -> Result<Response<v1beta::sync::FetchBlockResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }

    async fn dump_history(
        &self,
        _request: Request<v1beta::sync::DumpHistoryRequest>,
    ) -> Result<Response<v1beta::sync::DumpHistoryResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type FollowTipStream = ReceiverStream<Result<v1beta::sync::FollowTipResponse, Status>>;

    async fn follow_tip(
        &self,
        _request: Request<v1beta::sync::FollowTipRequest>,
    ) -> Result<Response<Self::FollowTipStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tip(
        &self,
        _request: Request<v1beta::sync::ReadTipRequest>,
    ) -> Result<Response<v1beta::sync::ReadTipResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}
