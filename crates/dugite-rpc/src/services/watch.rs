//! `WatchService` — pattern-filtered transaction watches.
//!
//! M4 (this commit): `WatchTx` streams transactions from the mempool
//! via `MempoolFeed`. Each tx is emitted as `WatchTxResponse.action.apply`
//! once observed in the mempool. Predicate filtering is a no-op
//! match-all in M4; pattern matching against `TxPattern` (address /
//! payment-cred / asset) requires the M5 `map/patterns.rs` matcher.
//!
//! Block-level context (`AnyChainTx.block`) is left empty in M4 — `tx →
//! block` resolution requires an async ChainDB lookup per event,
//! deferred to M5.

use dugite_mempool::MempoolEvent;
use dugite_serialization::decode_transaction;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::warn;

use super::ServiceState;
use crate::map::patterns::matches_tx_predicate;
use crate::map::tx::tx_to_proto;
use crate::proto::{v1alpha, v1beta};

const SERVICE_LABEL: &str = "watch";

// Use Conway era for decode — same default as SubmitService.
const DEFAULT_ERA_ID: u16 = 6;

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
        request: Request<v1alpha::watch::WatchTxRequest>,
    ) -> Result<Response<Self::WatchTxStream>, Status> {
        self.state.metrics.stream_started(SERVICE_LABEL, "watch_tx");
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        // Recode the v1alpha predicate to v1beta — they share the same
        // shape at v0.19.2, so prost re-encoding round-trips exactly.
        let predicate_beta: Option<v1beta::watch::TxPredicate> = {
            use prost::Message;
            request
                .into_inner()
                .predicate
                .map(|p| p.encode_to_vec())
                .and_then(|b| v1beta::watch::TxPredicate::decode(b.as_slice()).ok())
        };
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added {
                        tx_hash,
                        raw_cbor: Some(cbor),
                    }) => {
                        let decoded = match decode_transaction(DEFAULT_ERA_ID, &cbor) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(
                                    hash = %hex::encode(tx_hash.as_ref()),
                                    error = %e,
                                    "WatchTx: tx decode failed; skipping event"
                                );
                                continue;
                            }
                        };
                        if !matches_tx_predicate(predicate_beta.as_ref(), &decoded) {
                            continue;
                        }
                        let beta_tx = tx_to_proto(&decoded);
                        use prost::Message;
                        let alpha_tx =
                            v1alpha::cardano::Tx::decode(beta_tx.encode_to_vec().as_slice())
                                .expect("v1alpha Tx subset-compatible with v1beta");
                        let item = v1alpha::watch::AnyChainTx {
                            chain: Some(v1alpha::watch::any_chain_tx::Chain::Cardano(alpha_tx)),
                            block: None,
                        };
                        let resp = v1alpha::watch::WatchTxResponse {
                            action: Some(v1alpha::watch::watch_tx_response::Action::Apply(item)),
                        };
                        if tx.send(Ok(resp)).await.is_err() {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Added { raw_cbor: None, .. }) => continue,
                    Ok(MempoolEvent::Removed { .. }) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
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
        request: Request<v1beta::watch::WatchTxRequest>,
    ) -> Result<Response<Self::WatchTxStream>, Status> {
        self.state.metrics.stream_started(SERVICE_LABEL, "watch_tx");
        let mut events = self.state.mempool_feed.subscribe();
        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        let predicate_beta = request.into_inner().predicate;
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(MempoolEvent::Added {
                        tx_hash,
                        raw_cbor: Some(cbor),
                    }) => {
                        let decoded = match decode_transaction(DEFAULT_ERA_ID, &cbor) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(
                                    hash = %hex::encode(tx_hash.as_ref()),
                                    error = %e,
                                    "WatchTx: tx decode failed; skipping event"
                                );
                                continue;
                            }
                        };
                        if !matches_tx_predicate(predicate_beta.as_ref(), &decoded) {
                            continue;
                        }
                        let beta_tx = tx_to_proto(&decoded);
                        let item = v1beta::watch::AnyChainTx {
                            chain: Some(v1beta::watch::any_chain_tx::Chain::Cardano(beta_tx)),
                            block: None,
                        };
                        let resp = v1beta::watch::WatchTxResponse {
                            action: Some(v1beta::watch::watch_tx_response::Action::Apply(item)),
                        };
                        if tx.send(Ok(resp)).await.is_err() {
                            break;
                        }
                    }
                    Ok(MempoolEvent::Added { raw_cbor: None, .. }) => continue,
                    Ok(MempoolEvent::Removed { .. }) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
