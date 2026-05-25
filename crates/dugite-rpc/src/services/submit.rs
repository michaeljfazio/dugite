//! `SubmitService` — transaction submission + mempool inspection.
//!
//! M3 (this commit): SubmitTx, ReadMempool, WaitForTx, WatchMempool all
//! implemented against `LedgerContext::submit_tx` /
//! `mempool_snapshot` + `MempoolFeed`. EvalTx remains UNIMPLEMENTED —
//! it needs a non-committing UPLC evaluation helper extracted from
//! `dugite_ledger::plutus::eval_phase_two_raw` (deferred follow-up).
//!
//! Tx era inference: SubmitTx infers the era from the SubmitTxRequest's
//! oneof variant — only `raw` is defined at v1beta v0.19.2, so we
//! default to Conway era id (6). M2.B+ may add explicit era hints.

use dugite_mempool::{MempoolEvent, MempoolRemoveReason};
use std::collections::HashSet;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::proto::{v1alpha, v1beta};
use crate::SubmitOutcome;

const SERVICE_LABEL: &str = "submit";
const EVAL_UNIMPL: &str =
    "EvalTx requires a non-committing UPLC evaluation helper; deferred follow-up";

/// Conway-era CBOR tag the spec uses for txs at PV9+ today.
const DEFAULT_ERA_ID: u16 = 6;

fn extract_raw_tx_beta(req_tx: &Option<v1beta::submit::AnyChainTx>) -> Option<&[u8]> {
    req_tx
        .as_ref()
        .and_then(|t| t.r#type.as_ref())
        .map(|t| match t {
            v1beta::submit::any_chain_tx::Type::Raw(b) => b.as_slice(),
        })
}

fn extract_raw_tx_alpha(req_tx: &Option<v1alpha::submit::AnyChainTx>) -> Option<&[u8]> {
    req_tx
        .as_ref()
        .and_then(|t| t.r#type.as_ref())
        .map(|t| match t {
            v1alpha::submit::any_chain_tx::Type::Raw(b) => b.as_slice(),
        })
}

// ─── v1alpha ─────────────────────────────────────────────────────────────

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
        Err(Status::unimplemented(EVAL_UNIMPL))
    }

    async fn submit_tx(
        &self,
        request: Request<v1alpha::submit::SubmitTxRequest>,
    ) -> Result<Response<v1alpha::submit::SubmitTxResponse>, Status> {
        let req = request.into_inner();
        let raw = extract_raw_tx_alpha(&req.tx)
            .ok_or_else(|| Status::invalid_argument("SubmitTxRequest.tx.raw is required"))?;
        match self.state.context.submit_tx(DEFAULT_ERA_ID, raw).await {
            SubmitOutcome::Accepted { hash } => {
                Ok(Response::new(v1alpha::submit::SubmitTxResponse {
                    r#ref: hash.as_ref().to_vec(),
                }))
            }
            SubmitOutcome::Rejected { reason } => Err(Status::failed_precondition(reason)),
        }
    }

    type WaitForTxStream = ReceiverStream<Result<v1alpha::submit::WaitForTxResponse, Status>>;

    async fn wait_for_tx(
        &self,
        request: Request<v1alpha::submit::WaitForTxRequest>,
    ) -> Result<Response<Self::WaitForTxStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "wait_for_tx");
        let refs: HashSet<Vec<u8>> = request.into_inner().r#ref.into_iter().collect();
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);

        // For each requested ref, check the current mempool snapshot to
        // emit STAGE_MEMPOOL immediately if already present.
        for r in &refs {
            if r.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(r);
                let h = dugite_primitives::hash::Hash32::from_bytes(arr);
                if self.state.context.mempool_contains(&h).await {
                    let _ = tx
                        .send(Ok(v1alpha::submit::WaitForTxResponse {
                            r#ref: r.clone(),
                            stage: v1alpha::submit::Stage::Mempool as i32,
                        }))
                        .await;
                }
            }
        }

        let send = tx;
        tokio::spawn(async move {
            let watched = refs;
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added { tx_hash, .. }) => {
                        let hb = tx_hash.as_ref().to_vec();
                        if watched.contains(&hb)
                            && send
                                .send(Ok(v1alpha::submit::WaitForTxResponse {
                                    r#ref: hb,
                                    stage: v1alpha::submit::Stage::Mempool as i32,
                                }))
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Removed { tx_hash, reason }) => {
                        let hb = tx_hash.as_ref().to_vec();
                        if watched.contains(&hb) && reason == MempoolRemoveReason::Mined {
                            let _ = send
                                .send(Ok(v1alpha::submit::WaitForTxResponse {
                                    r#ref: hb,
                                    stage: v1alpha::submit::Stage::Confirmed as i32,
                                }))
                                .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn read_mempool(
        &self,
        _request: Request<v1alpha::submit::ReadMempoolRequest>,
    ) -> Result<Response<v1alpha::submit::ReadMempoolResponse>, Status> {
        let snapshot = self
            .state
            .context
            .mempool_snapshot()
            .await
            .map_err(Status::from)?;
        let items = snapshot
            .into_iter()
            .map(|raw_tx| v1alpha::submit::TxInMempool {
                r#ref: raw_tx.hash.as_ref().to_vec(),
                native_bytes: raw_tx.cbor,
                stage: v1alpha::submit::Stage::Mempool as i32,
                parsed_state: None, // M4 may populate; clients can decode native_bytes
            })
            .collect();
        Ok(Response::new(v1alpha::submit::ReadMempoolResponse {
            items,
        }))
    }

    type WatchMempoolStream = ReceiverStream<Result<v1alpha::submit::WatchMempoolResponse, Status>>;

    async fn watch_mempool(
        &self,
        _request: Request<v1alpha::submit::WatchMempoolRequest>,
    ) -> Result<Response<Self::WatchMempoolStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "watch_mempool");
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added {
                        tx_hash, raw_cbor, ..
                    }) => {
                        let item = v1alpha::submit::TxInMempool {
                            r#ref: tx_hash.as_ref().to_vec(),
                            native_bytes: raw_cbor.unwrap_or_default(),
                            stage: v1alpha::submit::Stage::Mempool as i32,
                            parsed_state: None,
                        };
                        if tx
                            .send(Ok(v1alpha::submit::WatchMempoolResponse { tx: Some(item) }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Removed { .. }) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

// ─── v1beta ──────────────────────────────────────────────────────────────

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
        Err(Status::unimplemented(EVAL_UNIMPL))
    }

    async fn submit_tx(
        &self,
        request: Request<v1beta::submit::SubmitTxRequest>,
    ) -> Result<Response<v1beta::submit::SubmitTxResponse>, Status> {
        let req = request.into_inner();
        let raw = extract_raw_tx_beta(&req.tx)
            .ok_or_else(|| Status::invalid_argument("SubmitTxRequest.tx.raw is required"))?;
        match self.state.context.submit_tx(DEFAULT_ERA_ID, raw).await {
            SubmitOutcome::Accepted { hash } => {
                Ok(Response::new(v1beta::submit::SubmitTxResponse {
                    r#ref: hash.as_ref().to_vec(),
                }))
            }
            SubmitOutcome::Rejected { reason } => Err(Status::failed_precondition(reason)),
        }
    }

    type WaitForTxStream = ReceiverStream<Result<v1beta::submit::WaitForTxResponse, Status>>;

    async fn wait_for_tx(
        &self,
        request: Request<v1beta::submit::WaitForTxRequest>,
    ) -> Result<Response<Self::WaitForTxStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "wait_for_tx");
        let refs: HashSet<Vec<u8>> = request.into_inner().r#ref.into_iter().collect();
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);

        for r in &refs {
            if r.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(r);
                let h = dugite_primitives::hash::Hash32::from_bytes(arr);
                if self.state.context.mempool_contains(&h).await {
                    let _ = tx
                        .send(Ok(v1beta::submit::WaitForTxResponse {
                            r#ref: r.clone(),
                            stage: v1beta::submit::Stage::Mempool as i32,
                        }))
                        .await;
                }
            }
        }

        let send = tx;
        tokio::spawn(async move {
            let watched = refs;
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added { tx_hash, .. }) => {
                        let hb = tx_hash.as_ref().to_vec();
                        if watched.contains(&hb)
                            && send
                                .send(Ok(v1beta::submit::WaitForTxResponse {
                                    r#ref: hb,
                                    stage: v1beta::submit::Stage::Mempool as i32,
                                }))
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Removed { tx_hash, reason }) => {
                        let hb = tx_hash.as_ref().to_vec();
                        if watched.contains(&hb) && reason == MempoolRemoveReason::Mined {
                            let _ = send
                                .send(Ok(v1beta::submit::WaitForTxResponse {
                                    r#ref: hb,
                                    stage: v1beta::submit::Stage::Confirmed as i32,
                                }))
                                .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn read_mempool(
        &self,
        _request: Request<v1beta::submit::ReadMempoolRequest>,
    ) -> Result<Response<v1beta::submit::ReadMempoolResponse>, Status> {
        let snapshot = self
            .state
            .context
            .mempool_snapshot()
            .await
            .map_err(Status::from)?;
        let items = snapshot
            .into_iter()
            .map(|raw_tx| v1beta::submit::TxInMempool {
                r#ref: raw_tx.hash.as_ref().to_vec(),
                native_bytes: raw_tx.cbor,
                stage: v1beta::submit::Stage::Mempool as i32,
                parsed_state: None,
            })
            .collect();
        Ok(Response::new(v1beta::submit::ReadMempoolResponse { items }))
    }

    type WatchMempoolStream = ReceiverStream<Result<v1beta::submit::WatchMempoolResponse, Status>>;

    async fn watch_mempool(
        &self,
        _request: Request<v1beta::submit::WatchMempoolRequest>,
    ) -> Result<Response<Self::WatchMempoolStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "watch_mempool");
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added {
                        tx_hash, raw_cbor, ..
                    }) => {
                        let item = v1beta::submit::TxInMempool {
                            r#ref: tx_hash.as_ref().to_vec(),
                            native_bytes: raw_cbor.unwrap_or_default(),
                            stage: v1beta::submit::Stage::Mempool as i32,
                            parsed_state: None,
                        };
                        if tx
                            .send(Ok(v1beta::submit::WatchMempoolResponse { tx: Some(item) }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Removed { .. }) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
