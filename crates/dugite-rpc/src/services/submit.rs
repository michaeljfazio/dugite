//! `SubmitService` — transaction submission + mempool inspection.
//!
//! M1.A stubs: every method returns `UNIMPLEMENTED`. M3 fills SubmitTx /
//! ReadMempool / WaitForTx / WatchMempool. `EvalTx` (Plutus dry-run) stays
//! UNIMPLEMENTED past M3 — it needs a non-committing
//! `dugite_uplc::evaluate_in_context` helper extracted from
//! `dugite_ledger::plutus::eval_phase_two_raw` (deferred follow-up).

use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::proto::{v1alpha, v1beta};

const UNIMPL: &str = "M1.A stub — SubmitService method not implemented yet";
const EVAL_UNIMPL: &str = "EvalTx requires a non-committing UPLC evaluation helper; deferred to a \
     follow-up milestone after the SubmitService base lands";

#[derive(Clone)]
pub struct SubmitSvcAlpha {
    state: ServiceState,
}

impl SubmitSvcAlpha {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1alpha::submit::submit_service_server::SubmitService for SubmitSvcAlpha {
    async fn eval_tx(
        &self,
        _request: Request<v1alpha::submit::EvalTxRequest>,
    ) -> Result<Response<v1alpha::submit::EvalTxResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(EVAL_UNIMPL))
    }

    async fn submit_tx(
        &self,
        _request: Request<v1alpha::submit::SubmitTxRequest>,
    ) -> Result<Response<v1alpha::submit::SubmitTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type WaitForTxStream = ReceiverStream<Result<v1alpha::submit::WaitForTxResponse, Status>>;

    async fn wait_for_tx(
        &self,
        _request: Request<v1alpha::submit::WaitForTxRequest>,
    ) -> Result<Response<Self::WaitForTxStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_mempool(
        &self,
        _request: Request<v1alpha::submit::ReadMempoolRequest>,
    ) -> Result<Response<v1alpha::submit::ReadMempoolResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type WatchMempoolStream = ReceiverStream<Result<v1alpha::submit::WatchMempoolResponse, Status>>;

    async fn watch_mempool(
        &self,
        _request: Request<v1alpha::submit::WatchMempoolRequest>,
    ) -> Result<Response<Self::WatchMempoolStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}

#[derive(Clone)]
pub struct SubmitSvcBeta {
    state: ServiceState,
}

impl SubmitSvcBeta {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1beta::submit::submit_service_server::SubmitService for SubmitSvcBeta {
    async fn eval_tx(
        &self,
        _request: Request<v1beta::submit::EvalTxRequest>,
    ) -> Result<Response<v1beta::submit::EvalTxResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(EVAL_UNIMPL))
    }

    async fn submit_tx(
        &self,
        _request: Request<v1beta::submit::SubmitTxRequest>,
    ) -> Result<Response<v1beta::submit::SubmitTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type WaitForTxStream = ReceiverStream<Result<v1beta::submit::WaitForTxResponse, Status>>;

    async fn wait_for_tx(
        &self,
        _request: Request<v1beta::submit::WaitForTxRequest>,
    ) -> Result<Response<Self::WaitForTxStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_mempool(
        &self,
        _request: Request<v1beta::submit::ReadMempoolRequest>,
    ) -> Result<Response<v1beta::submit::ReadMempoolResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    type WatchMempoolStream = ReceiverStream<Result<v1beta::submit::WatchMempoolResponse, Status>>;

    async fn watch_mempool(
        &self,
        _request: Request<v1beta::submit::WatchMempoolRequest>,
    ) -> Result<Response<Self::WatchMempoolStream>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}
