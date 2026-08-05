//! `WatchService` — pattern-filtered transaction watches.
//!
//! `WatchTx` streams transactions observed in the mempool via
//! `MempoolFeed`, filtered through the real `TxPredicate` matcher
//! (`crate::map::patterns::matches_tx_predicate` — address / asset /
//! mint via `produces` / `has_address` / `moves_asset` / `mints_asset`;
//! `consumes` / `has_certificate` are NOT matched, since those need
//! resolved-input lookups the mempool path doesn't have — documented in
//! `docs/src/running/utxo-rpc.md`'s Limitations section, not silently
//! dropped). A non-matching tx is filtered out of the stream entirely —
//! never delivered "just in case", per this project's reject/filter-over
//! -silent-pass-through rule.
//!
//! `AnyChainTx.block` is unconditionally unset: the proto's own comment
//! on `WatchTx` says "stream transactions from the **chain**", but this
//! implementation streams mempool ADMISSION events, which by definition
//! precede confirmation — there is no block to report yet. `undo` and
//! `idle` (both block-scoped: undoing a previously-emitted match on
//! rollback, or signalling a block with zero matches) are consequently
//! never emitted either. Re-sourcing from `TipFeed` instead of
//! `MempoolFeed` (mirroring `SyncService::spawn_follow_tip_stream_beta`)
//! would fix this but needs `TipRollback` to carry the rolled-back
//! block's transactions, which it does not today — tracked as
//! <https://github.com/michaeljfazio/dugite/issues/1007>, out of scope
//! for a `dugite-rpc`-only change.
//!
//! Every emitted `WatchTxResponse` is run through `masking::apply`
//! (issue #1004) using the request's `FieldMask`, captured once at
//! subscribe time and applied per event.

use dugite_mempool::MempoolEvent;
use dugite_serialization::decode_transaction;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::warn;

use super::{mask_paths, send_masked, ServiceState};
use crate::map::message_names;
use crate::map::patterns::matches_tx_predicate;
use crate::map::tx::tx_to_proto;
use crate::masking;
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
        let req = request.into_inner();
        let mask = mask_paths(req.field_mask);
        // Recode the v1alpha predicate to v1beta — they share the same
        // shape at v0.19.2, so prost re-encoding round-trips exactly.
        let predicate_beta: Option<v1beta::watch::TxPredicate> = {
            use prost::Message;
            req.predicate
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
                        let masked =
                            masking::apply(&mask, resp, message_names::WATCH_TX_RESPONSE_ALPHA);
                        if send_masked(&tx, masked).await {
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
        let req = request.into_inner();
        let mask = mask_paths(req.field_mask);
        let predicate_beta = req.predicate;
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
                        let masked =
                            masking::apply(&mask, resp, message_names::WATCH_TX_RESPONSE_BETA);
                        if send_masked(&tx, masked).await {
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
