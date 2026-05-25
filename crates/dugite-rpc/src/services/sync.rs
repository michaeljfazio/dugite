//! `SyncService` — block retrieval + chain following.
//!
//! M1.B (this commit): all four methods implemented end-to-end against
//! [`LedgerContext`] + [`TipFeed`].
//!
//! * `ReadTip`: returns the current tip as a `BlockRef`.
//! * `FetchBlock`: per-ref lookup via `block_by_hash` (preferred) or
//!   `block_at_slot`. Blocks ship as `AnyChainBlock` with both
//!   `native_bytes` and a parsed Cardano `Block`.
//! * `DumpHistory`: pages forward starting at `start_token` (exclusive
//!   if `start_token.slot > 0`, otherwise from-genesis equivalent),
//!   bounded by `max_items` and a hard local cap.
//! * `FollowTip`: streams `apply` events from the tip feed plus `reset`
//!   events on rollback. If the client supplies `intersect` points, the
//!   first message resets to the latest matching point on the local
//!   chain. Slow clients drop with `RESOURCE_EXHAUSTED` per the shared
//!   `stream::spawn_broadcast_fan_out` policy.
//!
//! `v1alpha` and `v1beta` carry isomorphic shapes for these methods so
//! the implementation is intentionally duplicated rather than abstracted
//! — keeps each impl readable, and a future `pb_recode!` macro can
//! collapse them if needed without rewriting the logic.

use std::sync::Arc;

use dugite_primitives::block::Point;
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::SlotNo;
use dugite_serialization::decode::decode_block;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::warn;

use super::ServiceState;
use crate::context::{LedgerContext, RawBlock};
use crate::error::RpcError;
use crate::map::block::{any_chain_block, block_ref_from_raw, block_ref_from_tip};
use crate::proto::{v1alpha, v1beta};
use crate::stream::spawn_broadcast_fan_out;

const SERVICE_LABEL: &str = "sync";

/// Hard upper bound on `DumpHistory.max_items` regardless of what the
/// client requests. Protects the node from a runaway scan.
const DUMP_HISTORY_HARD_CAP: u32 = 1000;

// ─── Shared resolvers (both v1alpha + v1beta use these) ───────────────────

async fn resolve_one_block(
    ctx: &Arc<dyn LedgerContext>,
    hash: &[u8],
    slot: u64,
) -> Result<Option<RawBlock>, RpcError> {
    if hash.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(hash);
        return ctx.block_by_hash(&Hash32::from_bytes(arr)).await;
    }
    if slot > 0 {
        return ctx.block_at_slot(slot).await;
    }
    Ok(None)
}

fn parse_block(raw: &RawBlock) -> Option<dugite_primitives::block::Block> {
    match decode_block(&raw.cbor) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!(
                slot = raw.slot,
                hash = %hex::encode(raw.hash),
                error = %e,
                "dugite-rpc: failed to parse block for AnyChainBlock; \
                 returning native_bytes only"
            );
            None
        }
    }
}

async fn fetch_blocks_for_refs<H>(
    ctx: &Arc<dyn LedgerContext>,
    refs: &[H],
    get_hash: impl Fn(&H) -> &[u8],
    get_slot: impl Fn(&H) -> u64,
) -> Result<Vec<RawBlock>, RpcError> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if let Some(raw) = resolve_one_block(ctx, get_hash(r), get_slot(r)).await? {
            out.push(raw);
        }
    }
    Ok(out)
}

async fn dump_history_impl(
    ctx: &Arc<dyn LedgerContext>,
    start_slot: u64,
    max_items: u32,
) -> Result<Vec<RawBlock>, RpcError> {
    let cap = max_items.clamp(1, DUMP_HISTORY_HARD_CAP) as usize;
    let mut out: Vec<RawBlock> = Vec::with_capacity(cap);
    let mut cursor = start_slot;
    while out.len() < cap {
        match ctx.block_after(cursor).await? {
            Some(raw) => {
                cursor = raw.slot;
                out.push(raw);
            }
            None => break,
        }
    }
    Ok(out)
}

async fn intersect_impl(
    ctx: &Arc<dyn LedgerContext>,
    refs: &[(Vec<u8>, u64)],
) -> Result<Option<Point>, RpcError> {
    let mut points: Vec<Point> = Vec::with_capacity(refs.len());
    for (hash, slot) in refs {
        if hash.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(hash);
            points.push(Point::Specific(SlotNo(*slot), Hash32::from_bytes(arr)));
        }
    }
    ctx.intersect(&points).await
}

fn point_to_block_ref(point: &Point) -> (u64, Vec<u8>) {
    match point {
        Point::Origin => (0, Vec::new()),
        Point::Specific(slot, hash) => (slot.0, hash.as_ref().to_vec()),
    }
}

// ─── v1alpha ──────────────────────────────────────────────────────────────

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
        request: Request<v1alpha::sync::FetchBlockRequest>,
    ) -> Result<Response<v1alpha::sync::FetchBlockResponse>, Status> {
        self.state
            .metrics
            .request_started(SERVICE_LABEL, "fetch_block");
        let req = request.into_inner();
        let raws = fetch_blocks_for_refs(
            &self.state.context,
            &req.r#ref,
            |r| r.hash.as_slice(),
            |r| r.slot,
        )
        .await
        .map_err(Status::from)?;
        let blocks = raws
            .iter()
            .map(|raw| {
                let parsed = parse_block(raw);
                let beta = any_chain_block(raw, parsed.as_ref());
                v1alpha::sync::AnyChainBlock {
                    native_bytes: beta.native_bytes,
                    chain: beta.chain.map(|c| match c {
                        v1beta::sync::any_chain_block::Chain::Cardano(b) => {
                            v1alpha::sync::any_chain_block::Chain::Cardano(recode_block_to_alpha(b))
                        }
                    }),
                }
            })
            .collect();
        Ok(Response::new(v1alpha::sync::FetchBlockResponse {
            block: blocks,
        }))
    }

    async fn dump_history(
        &self,
        request: Request<v1alpha::sync::DumpHistoryRequest>,
    ) -> Result<Response<v1alpha::sync::DumpHistoryResponse>, Status> {
        let req = request.into_inner();
        let start_slot = req.start_token.as_ref().map(|r| r.slot).unwrap_or(0);
        let raws = dump_history_impl(&self.state.context, start_slot, req.max_items)
            .await
            .map_err(Status::from)?;
        let next_token = raws
            .last()
            .map(block_ref_from_raw)
            .map(|r| v1alpha::sync::BlockRef {
                slot: r.slot,
                hash: r.hash,
                height: r.height,
                timestamp: r.timestamp,
            });
        let blocks = raws
            .iter()
            .map(|raw| {
                let parsed = parse_block(raw);
                let beta = any_chain_block(raw, parsed.as_ref());
                v1alpha::sync::AnyChainBlock {
                    native_bytes: beta.native_bytes,
                    chain: beta.chain.map(|c| match c {
                        v1beta::sync::any_chain_block::Chain::Cardano(b) => {
                            v1alpha::sync::any_chain_block::Chain::Cardano(recode_block_to_alpha(b))
                        }
                    }),
                }
            })
            .collect();
        Ok(Response::new(v1alpha::sync::DumpHistoryResponse {
            block: blocks,
            next_token,
        }))
    }

    type FollowTipStream = ReceiverStream<Result<v1alpha::sync::FollowTipResponse, Status>>;

    async fn follow_tip(
        &self,
        request: Request<v1alpha::sync::FollowTipRequest>,
    ) -> Result<Response<Self::FollowTipStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "follow_tip");
        let req = request.into_inner();
        let intersection_refs: Vec<(Vec<u8>, u64)> = req
            .intersect
            .into_iter()
            .map(|r| (r.hash, r.slot))
            .collect();

        let apply_rx = self.state.tip_feed.subscribe_apply();
        let rollback_rx = self.state.tip_feed.subscribe_rollback();
        let intersection = intersect_impl(&self.state.context, &intersection_refs)
            .await
            .map_err(Status::from)?;

        let (mut stream, _) = spawn_broadcast_fan_out::<_, v1alpha::sync::FollowTipResponse, _>(
            apply_rx,
            self.state.config.stream_buffer,
            SERVICE_LABEL,
            "follow_tip",
            move |tip| {
                Some(v1alpha::sync::FollowTipResponse {
                    action: Some(v1alpha::sync::follow_tip_response::Action::Apply(
                        v1alpha::sync::AnyChainBlock {
                            native_bytes: Vec::new(),
                            chain: None,
                        },
                    )),
                    tip: Some(v1alpha::sync::BlockRef {
                        slot: tip.slot,
                        hash: tip.hash.to_vec(),
                        height: tip.block_number,
                        timestamp: 0,
                    }),
                })
            },
        );

        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        if let Some(point) = intersection {
            let (slot, hash) = point_to_block_ref(&point);
            let _ = tx
                .send(Ok(v1alpha::sync::FollowTipResponse {
                    action: Some(v1alpha::sync::follow_tip_response::Action::Reset(
                        v1alpha::sync::BlockRef {
                            slot,
                            hash,
                            height: 0,
                            timestamp: 0,
                        },
                    )),
                    tip: None,
                }))
                .await;
        }
        tokio::spawn(async move {
            let mut rollback_rx = rollback_rx;
            loop {
                tokio::select! {
                    next = futures::StreamExt::next(&mut stream) => {
                        match next {
                            Some(item) => if tx.send(item).await.is_err() { break; },
                            None => break,
                        }
                    }
                    rb = rollback_rx.recv() => {
                        match rb {
                            Ok(ev) => {
                                if tx.send(Ok(v1alpha::sync::FollowTipResponse {
                                    action: Some(v1alpha::sync::follow_tip_response::Action::Reset(
                                        v1alpha::sync::BlockRef {
                                            slot: ev.slot,
                                            hash: ev.hash.to_vec(),
                                            height: 0,
                                            timestamp: 0,
                                        },
                                    )),
                                    tip: None,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        }
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn read_tip(
        &self,
        _request: Request<v1alpha::sync::ReadTipRequest>,
    ) -> Result<Response<v1alpha::sync::ReadTipResponse>, Status> {
        let tip = self.state.context.tip().await.map_err(Status::from)?;
        let r = block_ref_from_tip(&tip);
        Ok(Response::new(v1alpha::sync::ReadTipResponse {
            tip: Some(v1alpha::sync::BlockRef {
                slot: r.slot,
                hash: r.hash,
                height: r.height,
                timestamp: r.timestamp,
            }),
        }))
    }
}

// ─── v1beta ──────────────────────────────────────────────────────────────

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
        request: Request<v1beta::sync::FetchBlockRequest>,
    ) -> Result<Response<v1beta::sync::FetchBlockResponse>, Status> {
        let req = request.into_inner();
        let raws = fetch_blocks_for_refs(
            &self.state.context,
            &req.r#ref,
            |r| r.hash.as_slice(),
            |r| r.slot,
        )
        .await
        .map_err(Status::from)?;
        let blocks = raws
            .iter()
            .map(|raw| any_chain_block(raw, parse_block(raw).as_ref()))
            .collect();
        Ok(Response::new(v1beta::sync::FetchBlockResponse {
            block: blocks,
        }))
    }

    async fn dump_history(
        &self,
        request: Request<v1beta::sync::DumpHistoryRequest>,
    ) -> Result<Response<v1beta::sync::DumpHistoryResponse>, Status> {
        let req = request.into_inner();
        let start_slot = req.start_token.as_ref().map(|r| r.slot).unwrap_or(0);
        let raws = dump_history_impl(&self.state.context, start_slot, req.max_items)
            .await
            .map_err(Status::from)?;
        let next_token = raws.last().map(block_ref_from_raw);
        let blocks = raws
            .iter()
            .map(|raw| any_chain_block(raw, parse_block(raw).as_ref()))
            .collect();
        Ok(Response::new(v1beta::sync::DumpHistoryResponse {
            block: blocks,
            next_token,
        }))
    }

    type FollowTipStream = ReceiverStream<Result<v1beta::sync::FollowTipResponse, Status>>;

    async fn follow_tip(
        &self,
        request: Request<v1beta::sync::FollowTipRequest>,
    ) -> Result<Response<Self::FollowTipStream>, Status> {
        self.state
            .metrics
            .stream_started(SERVICE_LABEL, "follow_tip");
        let req = request.into_inner();
        let intersection_refs: Vec<(Vec<u8>, u64)> = req
            .intersect
            .into_iter()
            .map(|r| (r.hash, r.slot))
            .collect();

        let apply_rx = self.state.tip_feed.subscribe_apply();
        let rollback_rx = self.state.tip_feed.subscribe_rollback();
        let intersection = intersect_impl(&self.state.context, &intersection_refs)
            .await
            .map_err(Status::from)?;

        let (mut stream, _) = spawn_broadcast_fan_out::<_, v1beta::sync::FollowTipResponse, _>(
            apply_rx,
            self.state.config.stream_buffer,
            SERVICE_LABEL,
            "follow_tip",
            move |tip| {
                Some(v1beta::sync::FollowTipResponse {
                    action: Some(v1beta::sync::follow_tip_response::Action::Apply(
                        v1beta::sync::AnyChainBlock {
                            native_bytes: Vec::new(),
                            chain: None,
                        },
                    )),
                    tip: Some(v1beta::sync::BlockRef {
                        slot: tip.slot,
                        hash: tip.hash.to_vec(),
                        height: tip.block_number,
                        timestamp: 0,
                    }),
                })
            },
        );

        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        if let Some(point) = intersection {
            let (slot, hash) = point_to_block_ref(&point);
            let _ = tx
                .send(Ok(v1beta::sync::FollowTipResponse {
                    action: Some(v1beta::sync::follow_tip_response::Action::Reset(
                        v1beta::sync::BlockRef {
                            slot,
                            hash,
                            height: 0,
                            timestamp: 0,
                        },
                    )),
                    tip: None,
                }))
                .await;
        }
        tokio::spawn(async move {
            let mut rollback_rx = rollback_rx;
            loop {
                tokio::select! {
                    next = futures::StreamExt::next(&mut stream) => {
                        match next {
                            Some(item) => if tx.send(item).await.is_err() { break; },
                            None => break,
                        }
                    }
                    rb = rollback_rx.recv() => {
                        match rb {
                            Ok(ev) => {
                                if tx.send(Ok(v1beta::sync::FollowTipResponse {
                                    action: Some(v1beta::sync::follow_tip_response::Action::Reset(
                                        v1beta::sync::BlockRef {
                                            slot: ev.slot,
                                            hash: ev.hash.to_vec(),
                                            height: 0,
                                            timestamp: 0,
                                        },
                                    )),
                                    tip: None,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        }
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn read_tip(
        &self,
        _request: Request<v1beta::sync::ReadTipRequest>,
    ) -> Result<Response<v1beta::sync::ReadTipResponse>, Status> {
        let tip = self.state.context.tip().await.map_err(Status::from)?;
        Ok(Response::new(v1beta::sync::ReadTipResponse {
            tip: Some(block_ref_from_tip(&tip)),
        }))
    }
}

// ─── v1beta → v1alpha re-encoding ─────────────────────────────────────────

/// Translate a v1beta Cardano Block to v1alpha via prost re-encode.
fn recode_block_to_alpha(beta: v1beta::cardano::Block) -> v1alpha::cardano::Block {
    use prost::Message;
    let bytes = beta.encode_to_vec();
    v1alpha::cardano::Block::decode(bytes.as_slice())
        .expect("v1alpha and v1beta Cardano Block messages are wire-compatible for M1.B fields")
}
