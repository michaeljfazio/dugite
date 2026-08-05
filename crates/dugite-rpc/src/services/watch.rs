//! `WatchService` — pattern-filtered transaction watches.
//!
//! Issue #1007: `WatchTx` streams transactions from CONFIRMED blocks via
//! `TipFeed` (the same feed `SyncService::FollowTip` uses), matching the
//! proto's own comment on `WatchTx` — *"Stream transactions from the
//! **chain**"* — rather than the mempool-admission events it used to
//! source from (that behaviour now correctly belongs to
//! `SubmitService::WatchMempool`, whose own proto comment says *"from
//! the **mempool**"*, and which already exists as the low-latency
//! pre-confirmation alternative).
//!
//! Filtered through the real `TxPredicate` matcher
//! (`crate::map::patterns::matches_tx_predicate` — address / asset /
//! mint via `produces` / `has_address` / `moves_asset` / `mints_asset`).
//! `consumes` (needs resolved-input lookups this path doesn't have) and
//! `has_certificate` (needs a certificate-type matcher not yet built)
//! are NOT matched — and a request naming either (anywhere, including
//! nested under `not` / `all_of` / `any_of`) is REJECTED with
//! `Status::unimplemented` before it ever subscribes
//! (`tx_predicate_has_unsupported_leaf`), rather than silently accepted
//! and under-filtered. A non-matching tx is filtered out of the stream
//! entirely — never delivered "just in case", per this project's
//! reject-over-silent-skip rule.
//!
//! `AnyChainTx.block` is now populated (the confirmed block a matching
//! tx belongs to) — the previous mempool-sourced design could never
//! populate it, since a mempool tx has no confirming block yet.
//!
//! `undo` / `idle` are now emitted too: each subscriber keeps a bounded
//! (`HISTORY_CAP` blocks — comfortably above mainnet's security
//! parameter k=2160) history of which blocks it has told THIS client
//! about, keyed by slot. The history retains only `(slot, hash)` per
//! entry, NOT the matching txs or block content — those are re-resolved
//! from `ChainDB` (the same `resolve_matches` apply uses) when a
//! rollback actually needs them. This is deliberate: rollbacks are rare
//! and bounded (a handful a day, capped by k), so paying one extra
//! `ChainDB` lookup on that rare path beats retaining a full block's
//! content — CBOR bytes AND the expanded proto, per matching tx — for
//! every applied block, forever, per subscriber, with no cap on
//! concurrent subscribers. The earlier design cached the full envelope;
//! at `HISTORY_CAP` blocks that was order-of-GB per stream, an
//! unauthenticated memory-exhaustion vector this project's own
//! "adversarial-deployment" posture exists to rule out. A block with
//! zero matching txs emits `idle(BlockRef)` so a client can tell "no
//! match" from "stalled".
//!
//! A rollback whose target predates everything this subscriber has ever
//! been told about is not an error: nothing was ever asserted for that
//! range, so there is nothing to undo. A rollback that needs to undo an
//! entry this subscriber's bounded history has since evicted (or whose
//! re-resolution fails — the block's `ChainDB` retention window elapsed
//! before the rollback arrived) is different: the client WAS told about
//! that tx as `apply` and we can no longer prove it's been undone. That
//! case terminates the stream with `Status::data_loss` rather than
//! silently emitting a partial (and therefore wrong) undo sequence — the
//! #1004 failure mode of a stream that looks correct and isn't.
//!
//! Every emitted `WatchTxResponse` is run through `masking::apply`
//! (issue #1004) using the request's `FieldMask`, captured once at
//! subscribe time and applied per event.

use std::collections::VecDeque;
use std::sync::Arc;

use dugite_primitives::hash::Hash32;
use dugite_serialization::decode::decode_block;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::warn;

use super::{mask_paths, send_masked, ServiceState};
use crate::context::{LedgerContext, TipInfo};
use crate::map::block::any_chain_block_watch;
use crate::map::message_names;
use crate::map::patterns::{matches_tx_predicate, tx_predicate_has_unsupported_leaf};
use crate::map::tx::tx_to_proto;
use crate::masking;
use crate::proto::{v1alpha, v1beta};
use crate::tip_feed::TipRollback;

const SERVICE_LABEL: &str = "watch";

/// Bound on how many blocks' worth of history each `WatchTx` subscriber
/// retains for `undo` — comfortably above mainnet's security parameter
/// (k=2160), so any rollback the chain-selection layer would actually
/// allow is covered. Each entry is `(slot, hash)` only (~40 bytes), so
/// the whole history is a ~200KB rounding error per subscriber — see the
/// module doc for why content is NOT cached here.
const HISTORY_CAP: usize = 4_320;

/// One applied block's identity — enough to re-resolve it from `ChainDB`
/// on rollback, and nothing more. See the module doc for why this does
/// NOT cache the block or its matching txs.
struct HistoryEntry {
    slot: u64,
    hash: [u8; 32],
}

/// Whether a rollback to `target_slot` needs to undo history entries
/// this subscriber's bounded cache has already evicted.
///
/// `history_truncated` is true once any entry has ever been popped off
/// the front for exceeding `HISTORY_CAP`; `oldest_retained_slot` is the
/// current `history.front()` slot, or `None` when the cache is empty
/// right now. Once truncation has ever happened, an empty-or-shallow
/// remaining history can no longer prove there's nothing left to undo —
/// so "empty after truncation" counts as a gap too, rather than assuming
/// the best.
fn rollback_has_coverage_gap(
    history_truncated: bool,
    oldest_retained_slot: Option<u64>,
    target_slot: u64,
) -> bool {
    history_truncated && oldest_retained_slot.unwrap_or(u64::MAX) > target_slot
}

/// Resolve one applied block (by `slot`/`hash`) into its matching
/// transactions (v1beta shape) plus the block envelope they share.
/// Returns `None` when the block can't be resolved or decoded — logged,
/// and the caller should treat the event as unresolvable rather than
/// guess. Used both for a live apply and for re-resolving a history
/// entry when a rollback needs to undo it.
async fn resolve_matches(
    ctx: &Arc<dyn LedgerContext>,
    slot: u64,
    hash: [u8; 32],
    predicate: Option<&v1beta::watch::TxPredicate>,
) -> Option<(v1beta::watch::AnyChainBlock, Vec<v1beta::cardano::Tx>)> {
    let hash32 = Hash32::from_bytes(hash);
    let raw = match ctx.block_by_hash(&hash32).await {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            warn!(
                slot,
                hash = %hex::encode(hash),
                "WatchTx: block not found in ChainDB; skipping event"
            );
            return None;
        }
        Err(e) => {
            warn!(
                slot,
                hash = %hex::encode(hash),
                error = ?e,
                "WatchTx: block_by_hash failed; skipping event"
            );
            return None;
        }
    };
    let block = match decode_block(&raw.cbor) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                slot,
                hash = %hex::encode(hash),
                error = %e,
                "WatchTx: block decode failed; skipping event"
            );
            return None;
        }
    };
    let any_block = any_chain_block_watch(&raw, Some(&block));
    let matches = block
        .transactions
        .iter()
        .filter(|tx| matches_tx_predicate(predicate, tx))
        .map(tx_to_proto)
        .collect();
    Some((any_block, matches))
}

/// Spawn the shared `WatchTx` stream task. Emits `v1beta` responses;
/// `v1alpha` callers re-encode at the boundary (mirrors
/// `SyncService::spawn_follow_tip_stream_beta`'s split — one mechanism,
/// not two independently-maintained copies of the apply/undo/idle
/// bookkeeping).
async fn spawn_watch_tx_stream_beta(
    ctx: Arc<dyn LedgerContext>,
    buffer: usize,
    predicate: Option<v1beta::watch::TxPredicate>,
    mask: Vec<String>,
    mut apply_rx: broadcast::Receiver<TipInfo>,
    mut rollback_rx: broadcast::Receiver<TipRollback>,
) -> mpsc::Receiver<Result<v1beta::watch::WatchTxResponse, Status>> {
    let (tx, rx) = mpsc::channel(buffer);
    tokio::spawn(async move {
        let mut history: VecDeque<HistoryEntry> = VecDeque::new();
        let mut history_truncated = false;
        loop {
            tokio::select! {
                applied = apply_rx.recv() => {
                    match applied {
                        Ok(tip) => {
                            let Some((block, matches)) =
                                resolve_matches(&ctx, tip.slot, tip.hash, predicate.as_ref()).await
                            else {
                                continue;
                            };
                            if matches.is_empty() {
                                // `watch.proto`'s BlockRef has no `timestamp`
                                // field (unlike sync.proto's), so this can't
                                // reuse `block_ref_from_tip`.
                                let resp = v1beta::watch::WatchTxResponse {
                                    action: Some(v1beta::watch::watch_tx_response::Action::Idle(
                                        v1beta::watch::BlockRef {
                                            slot: tip.slot,
                                            hash: tip.hash.to_vec(),
                                            height: tip.block_number,
                                        },
                                    )),
                                };
                                let masked =
                                    masking::apply(&mask, resp, message_names::WATCH_TX_RESPONSE_BETA);
                                if send_masked(&tx, masked).await {
                                    return;
                                }
                            } else {
                                for m in matches {
                                    let item = v1beta::watch::AnyChainTx {
                                        chain: Some(v1beta::watch::any_chain_tx::Chain::Cardano(m)),
                                        block: Some(block.clone()),
                                    };
                                    let resp = v1beta::watch::WatchTxResponse {
                                        action: Some(v1beta::watch::watch_tx_response::Action::Apply(item)),
                                    };
                                    let masked = masking::apply(
                                        &mask,
                                        resp,
                                        message_names::WATCH_TX_RESPONSE_BETA,
                                    );
                                    if send_masked(&tx, masked).await {
                                        return;
                                    }
                                }
                            }
                            history.push_back(HistoryEntry { slot: tip.slot, hash: tip.hash });
                            if history.len() > HISTORY_CAP {
                                history.pop_front();
                                history_truncated = true;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                lagged = n,
                                "WatchTx: apply broadcast lagged; client may have missed blocks"
                            );
                            let _ = tx
                                .send(Err(Status::resource_exhausted(format!(
                                    "subscriber lagged by {n} apply events; reconnect and resync"
                                ))))
                                .await;
                            return;
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
                rb = rollback_rx.recv() => {
                    match rb {
                        Ok(ev) => {
                            let oldest_retained_slot = history.front().map(|e| e.slot);
                            if rollback_has_coverage_gap(history_truncated, oldest_retained_slot, ev.slot) {
                                let _ = tx
                                    .send(Err(Status::data_loss(format!(
                                        "WatchTx: rollback to slot {} needs to undo applied txs \
                                         this subscriber's retained history (cap={HISTORY_CAP} \
                                         blocks) has already evicted; reconnect and resync from \
                                         the new tip",
                                        ev.slot
                                    ))))
                                    .await;
                                return;
                            }
                            while history.back().is_some_and(|e| e.slot > ev.slot) {
                                let entry = history.pop_back().expect("checked Some above");
                                let Some((block, matches)) =
                                    resolve_matches(&ctx, entry.slot, entry.hash, predicate.as_ref())
                                        .await
                                else {
                                    // We told the client about this block's
                                    // matching txs as `apply`; we can no
                                    // longer resolve it to prove they've
                                    // been undone (e.g. ChainDB's retention
                                    // window elapsed before this rollback
                                    // arrived). Fail loud rather than
                                    // silently drop the rest of the undo
                                    // sequence — the #1004 failure mode of
                                    // a stream that looks correct and isn't.
                                    let _ = tx
                                        .send(Err(Status::data_loss(format!(
                                            "WatchTx: rolled-back block at slot {} could not be \
                                             re-resolved for undo; reconnect and resync",
                                            entry.slot
                                        ))))
                                        .await;
                                    return;
                                };
                                for m in matches.into_iter().rev() {
                                    let item = v1beta::watch::AnyChainTx {
                                        chain: Some(v1beta::watch::any_chain_tx::Chain::Cardano(m)),
                                        block: Some(block.clone()),
                                    };
                                    let resp = v1beta::watch::WatchTxResponse {
                                        action: Some(v1beta::watch::watch_tx_response::Action::Undo(item)),
                                    };
                                    let masked = masking::apply(
                                        &mask,
                                        resp,
                                        message_names::WATCH_TX_RESPONSE_BETA,
                                    );
                                    if send_masked(&tx, masked).await {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Best-effort, matching FollowTip: a missed
                            // rollback notification means our history may
                            // now disagree with the client's, but the next
                            // apply/rollback still moves both forward
                            // correctly — no different from a ChainSync
                            // client tolerating the same gap.
                        }
                    }
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::rollback_has_coverage_gap;

    #[test]
    fn gap_when_truncated_and_target_predates_retained_window() {
        assert!(rollback_has_coverage_gap(true, Some(500), 100));
    }

    #[test]
    fn no_gap_when_truncated_but_target_within_retained_window() {
        assert!(!rollback_has_coverage_gap(true, Some(50), 100));
    }

    #[test]
    fn no_gap_when_truncated_and_target_exactly_at_retained_window_edge() {
        // front().slot == target_slot: the oldest retained entry is not
        // strictly greater than target_slot, so it (and everything after)
        // is still coverable by the normal drain loop.
        assert!(!rollback_has_coverage_gap(true, Some(100), 100));
    }

    #[test]
    fn no_gap_when_never_truncated_even_if_target_predates_everything() {
        // Rollback target older than anything ever recorded, but nothing
        // was ever evicted: the client was simply never told about
        // anything in that range, so there's nothing to undo.
        assert!(!rollback_has_coverage_gap(false, Some(500), 100));
        assert!(!rollback_has_coverage_gap(false, None, 100));
    }

    #[test]
    fn truncated_and_currently_empty_history_is_treated_as_a_gap() {
        // Can't prove there's nothing left to undo once eviction has
        // happened and the cache no longer holds anything to check.
        assert!(rollback_has_coverage_gap(true, None, 100));
    }
}

fn recode_watch_tx_response_to_alpha(
    beta: v1beta::watch::WatchTxResponse,
) -> v1alpha::watch::WatchTxResponse {
    use prost::Message;

    fn recode_any_chain_tx(beta: v1beta::watch::AnyChainTx) -> v1alpha::watch::AnyChainTx {
        v1alpha::watch::AnyChainTx {
            chain: beta.chain.map(|c| match c {
                v1beta::watch::any_chain_tx::Chain::Cardano(tx) => {
                    let bytes = tx.encode_to_vec();
                    v1alpha::watch::any_chain_tx::Chain::Cardano(
                        v1alpha::cardano::Tx::decode(bytes.as_slice())
                            .expect("v1alpha Tx subset-compatible with v1beta"),
                    )
                }
            }),
            block: beta.block.map(|b| {
                let bytes = b.encode_to_vec();
                v1alpha::watch::AnyChainBlock::decode(bytes.as_slice())
                    .expect("v1alpha AnyChainBlock subset-compatible with v1beta")
            }),
        }
    }

    v1alpha::watch::WatchTxResponse {
        action: beta.action.map(|a| match a {
            v1beta::watch::watch_tx_response::Action::Apply(item) => {
                v1alpha::watch::watch_tx_response::Action::Apply(recode_any_chain_tx(item))
            }
            v1beta::watch::watch_tx_response::Action::Undo(item) => {
                v1alpha::watch::watch_tx_response::Action::Undo(recode_any_chain_tx(item))
            }
            v1beta::watch::watch_tx_response::Action::Idle(block_ref) => {
                v1alpha::watch::watch_tx_response::Action::Idle(v1alpha::watch::BlockRef {
                    slot: block_ref.slot,
                    hash: block_ref.hash,
                    height: block_ref.height,
                })
            }
        }),
    }
}

/// Recode a `v1alpha` `TxPredicate` to `v1beta` — they share the same
/// shape at v0.19.2, so prost re-encoding round-trips exactly (mirrors
/// the same trick used throughout `SubmitService`/`SyncService`).
fn recode_predicate_to_beta(
    predicate: Option<v1alpha::watch::TxPredicate>,
) -> Option<v1beta::watch::TxPredicate> {
    use prost::Message;
    predicate
        .map(|p| p.encode_to_vec())
        .and_then(|b| v1beta::watch::TxPredicate::decode(b.as_slice()).ok())
}

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
        let req = request.into_inner();
        let mask = mask_paths(req.field_mask);
        let predicate_beta = recode_predicate_to_beta(req.predicate);
        if predicate_beta
            .as_ref()
            .is_some_and(tx_predicate_has_unsupported_leaf)
        {
            return Err(Status::unimplemented(
                "WatchTx: TxPattern.consumes / has_certificate matching is not \
                 implemented; resubmit without those fields set",
            ));
        }
        let apply_rx = self.state.tip_feed.subscribe_apply();
        let rollback_rx = self.state.tip_feed.subscribe_rollback();
        let mut beta_rx = spawn_watch_tx_stream_beta(
            self.state.context.clone(),
            self.state.config.stream_buffer,
            predicate_beta,
            mask,
            apply_rx,
            rollback_rx,
        )
        .await;

        let (tx, rx) = mpsc::channel(self.state.config.stream_buffer);
        tokio::spawn(async move {
            while let Some(item) = beta_rx.recv().await {
                let alpha = item.map(recode_watch_tx_response_to_alpha);
                if tx.send(alpha).await.is_err() {
                    break;
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
        let req = request.into_inner();
        let mask = mask_paths(req.field_mask);
        let predicate_beta = req.predicate;
        if predicate_beta
            .as_ref()
            .is_some_and(tx_predicate_has_unsupported_leaf)
        {
            return Err(Status::unimplemented(
                "WatchTx: TxPattern.consumes / has_certificate matching is not \
                 implemented; resubmit without those fields set",
            ));
        }
        let apply_rx = self.state.tip_feed.subscribe_apply();
        let rollback_rx = self.state.tip_feed.subscribe_rollback();
        let rx = spawn_watch_tx_stream_beta(
            self.state.context.clone(),
            self.state.config.stream_buffer,
            predicate_beta,
            mask,
            apply_rx,
            rollback_rx,
        )
        .await;
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
