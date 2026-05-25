//! `QueryService` — read-only ledger queries.
//!
//! M1.A stubs: every method returns `UNIMPLEMENTED`. M2 fills the
//! ReadParams / ReadUtxos / SearchUtxos / ReadGenesis / ReadEra paths;
//! `read_data` / `read_tx` map to mempool + chain tx lookups.
//!
//! `v1beta` adds `read_state` beyond `v1alpha`.

use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::proto::{v1alpha, v1beta};

const UNIMPL: &str = "M1.A stub — QueryService method not implemented yet";

#[derive(Clone)]
pub struct QuerySvcAlpha {
    state: ServiceState,
}

impl QuerySvcAlpha {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1alpha::query::query_service_server::QueryService for QuerySvcAlpha {
    async fn read_params(
        &self,
        _request: Request<v1alpha::query::ReadParamsRequest>,
    ) -> Result<Response<v1alpha::query::ReadParamsResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_utxos(
        &self,
        _request: Request<v1alpha::query::ReadUtxosRequest>,
    ) -> Result<Response<v1alpha::query::ReadUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn search_utxos(
        &self,
        _request: Request<v1alpha::query::SearchUtxosRequest>,
    ) -> Result<Response<v1alpha::query::SearchUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_data(
        &self,
        _request: Request<v1alpha::query::ReadDataRequest>,
    ) -> Result<Response<v1alpha::query::ReadDataResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tx(
        &self,
        _request: Request<v1alpha::query::ReadTxRequest>,
    ) -> Result<Response<v1alpha::query::ReadTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_genesis(
        &self,
        _request: Request<v1alpha::query::ReadGenesisRequest>,
    ) -> Result<Response<v1alpha::query::ReadGenesisResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_era_summary(
        &self,
        _request: Request<v1alpha::query::ReadEraSummaryRequest>,
    ) -> Result<Response<v1alpha::query::ReadEraSummaryResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}

#[derive(Clone)]
pub struct QuerySvcBeta {
    state: ServiceState,
}

impl QuerySvcBeta {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1beta::query::query_service_server::QueryService for QuerySvcBeta {
    async fn read_params(
        &self,
        _request: Request<v1beta::query::ReadParamsRequest>,
    ) -> Result<Response<v1beta::query::ReadParamsResponse>, Status> {
        let _ = &self.state;
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_utxos(
        &self,
        _request: Request<v1beta::query::ReadUtxosRequest>,
    ) -> Result<Response<v1beta::query::ReadUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn search_utxos(
        &self,
        _request: Request<v1beta::query::SearchUtxosRequest>,
    ) -> Result<Response<v1beta::query::SearchUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_data(
        &self,
        _request: Request<v1beta::query::ReadDataRequest>,
    ) -> Result<Response<v1beta::query::ReadDataResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tx(
        &self,
        _request: Request<v1beta::query::ReadTxRequest>,
    ) -> Result<Response<v1beta::query::ReadTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_genesis(
        &self,
        _request: Request<v1beta::query::ReadGenesisRequest>,
    ) -> Result<Response<v1beta::query::ReadGenesisResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_era_summary(
        &self,
        _request: Request<v1beta::query::ReadEraSummaryRequest>,
    ) -> Result<Response<v1beta::query::ReadEraSummaryResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    /// `v1beta`-only — ad-hoc CBOR-shaped state queries.
    async fn read_state(
        &self,
        _request: Request<v1beta::query::ReadStateRequest>,
    ) -> Result<Response<v1beta::query::ReadStateResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}
