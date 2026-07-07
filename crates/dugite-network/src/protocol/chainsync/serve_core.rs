//! Shared ChainSync / LocalChainSync serve-loop core (issue #881).
//!
//! N2N `ChainSyncServer` (`chainsync::server`) and N2C `LocalChainSyncServer`
//! (`local_chainsync::server`) implement the *same* Ouroboros ChainSync
//! server state machine and differ in exactly one respect: how the
//! `MsgRollForward` payload is encoded.
//!
//! - N2N sends only the block *header*, HFC-wrapped:
//!   `extract_header_for_chainsync` (fallible — the header must be parsed
//!   out of the block body).
//! - N2C sends the *full block*, `Serialised`-wrapped: `wrap_serialised`
//!   (infallible — no parsing required).
//!
//! Before this extraction the two servers had drifted: N2C was missing the
//! duplicate-serve dedup, the `biased` rollback-first `select!` ordering,
//! the `StMustReply` retry loop (#869), `cursor_at_origin` genesis-EBB
//! handling, and lagged-rollback safety (Bug J) that N2N already had.  This
//! module is the single implementation both servers now delegate to, so a
//! fix applied once (e.g. #869, #876) automatically covers both wire
//! protocols.
//!
//! The payload encoder is threaded through as a plain `fn` pointer — never
//! boxed — so there is no vtable/allocation overhead on the hot per-block
//! serve path.

use std::time::Duration;

use tokio::sync::broadcast;

use crate::codec::Point;
use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;
use crate::BlockProvider;

use super::server::{
    BlockAnnouncement, RollbackAnnouncement, CHAINSYNC_MUST_REPLY_TIMEOUT_MAX,
    CHAINSYNC_MUST_REPLY_TIMEOUT_MIN,
};
use super::{decode_message, encode_message, ChainSyncMessage};

/// Encodes a block's raw storage CBOR into the wire-ready `MsgRollForward`
/// payload.  A plain `fn` pointer (not a boxed closure) — the encoder is
/// always a stateless free function, so this keeps the hot per-block serve
/// path allocation- and vtable-free.
pub(crate) type PayloadEncoder = fn(&[u8]) -> Result<Vec<u8>, ProtocolError>;

/// Shared ChainSync/LocalChainSync serve-loop core.
///
/// Owns the per-connection follower cursor and drives `MsgFindIntersect` /
/// `MsgRequestNext` handling identically for both wire protocols; the only
/// per-protocol behaviour is `payload_encoder` (header-only vs full-block)
/// and the `protocol` label used in tracing/errors.
pub(crate) struct ServeCore {
    /// Current cursor: the last point served to this peer.
    pub(crate) cursor_slot: u64,
    pub(crate) cursor_hash: [u8; 32],
    /// Whether the cursor has been initialized (via intersection or genesis).
    cursor_initialized: bool,
    /// True when the cursor is at Origin — meaning no block has been served
    /// yet and we must include blocks at slot 0 (e.g. Byron genesis EBB).
    cursor_at_origin: bool,
    /// Per-connection `StMustReply` timeout, drawn once at construction
    /// uniformly from `[CHAINSYNC_MUST_REPLY_TIMEOUT_MIN, CHAINSYNC_MUST_REPLY_TIMEOUT_MAX]`.
    /// Mirrors Haskell's per-connection timeout draw (#701).
    must_reply_timeout: Duration,
    /// Encodes the `MsgRollForward` payload from raw block CBOR — the only
    /// point of divergence between N2N ChainSync and N2C LocalChainSync.
    payload_encoder: PayloadEncoder,
    /// Mini-protocol name used in tracing spans and `ProtocolError` variants
    /// (`"ChainSync"` or `"LocalChainSync"`).
    protocol: &'static str,
}

impl ServeCore {
    /// Create a new core with a freshly-drawn random `StMustReply` timeout.
    pub(crate) fn new(payload_encoder: PayloadEncoder, protocol: &'static str) -> Self {
        Self::new_with_timeout(Self::draw_must_reply_timeout(), payload_encoder, protocol)
    }

    /// Test/explicit-timeout constructor.
    pub(crate) fn new_with_timeout(
        must_reply_timeout: Duration,
        payload_encoder: PayloadEncoder,
        protocol: &'static str,
    ) -> Self {
        Self {
            cursor_slot: 0,
            cursor_hash: [0; 32],
            cursor_initialized: false,
            cursor_at_origin: false,
            must_reply_timeout,
            payload_encoder,
            protocol,
        }
    }

    /// Draw a fresh `StMustReply` timeout uniformly in
    /// `[CHAINSYNC_MUST_REPLY_TIMEOUT_MIN, CHAINSYNC_MUST_REPLY_TIMEOUT_MAX]`.
    fn draw_must_reply_timeout() -> Duration {
        use rand::Rng;
        let span = CHAINSYNC_MUST_REPLY_TIMEOUT_MAX - CHAINSYNC_MUST_REPLY_TIMEOUT_MIN;
        let span_ms = span.as_millis() as u64;
        let mut rng = rand::rng();
        let offset_ms = rng.random_range(0..=span_ms);
        CHAINSYNC_MUST_REPLY_TIMEOUT_MIN + Duration::from_millis(offset_ms)
    }

    /// Configured `StMustReply` timeout (exposed for diagnostics / tests).
    pub(crate) fn must_reply_timeout(&self) -> Duration {
        self.must_reply_timeout
    }

    /// Run the ChainSync/LocalChainSync server loop.
    ///
    /// Handles `MsgFindIntersect`, `MsgRequestNext`, and `MsgDone` from the
    /// client.  Uses `block_provider` to look up blocks, `announcement_rx`
    /// to wait for new blocks at the tip, and `rollback_rx` to propagate
    /// chain rollbacks to downstream peers via `MsgRollBackward`.
    pub(crate) async fn run<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        mut announcement_rx: broadcast::Receiver<BlockAnnouncement>,
        mut rollback_rx: broadcast::Receiver<RollbackAnnouncement>,
    ) -> Result<(), ProtocolError> {
        let task_id = self as *const _ as usize;
        tracing::info!(
            task_id,
            protocol = self.protocol,
            "chainsync server: task started"
        );
        loop {
            tracing::debug!(
                task_id,
                protocol = self.protocol,
                cursor_slot = self.cursor_slot,
                cursor_at_origin = self.cursor_at_origin,
                "chainsync server: awaiting next client message"
            );
            let msg_bytes = channel.recv().await.map_err(|e| {
                tracing::info!(
                    task_id,
                    protocol = self.protocol,
                    error = %e,
                    cursor_slot = self.cursor_slot,
                    "chainsync server: channel.recv() failed — task exiting"
                );
                ProtocolError::from(e)
            })?;
            let msg = decode_message(&msg_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: self.protocol,
                reason: e,
            })?;

            match msg {
                ChainSyncMessage::MsgFindIntersect(points) => {
                    self.handle_find_intersect(channel, block_provider, &points)
                        .await?;
                }
                ChainSyncMessage::MsgRequestNext => {
                    self.handle_request_next(
                        channel,
                        block_provider,
                        &mut announcement_rx,
                        &mut rollback_rx,
                    )
                    .await?;
                }
                ChainSyncMessage::MsgDone => {
                    tracing::debug!(
                        protocol = self.protocol,
                        "chainsync server: client sent MsgDone"
                    );
                    return Ok(());
                }
                other => {
                    return Err(ProtocolError::AgencyViolation {
                        protocol: self.protocol,
                        state: "StIdle".to_string(),
                        received_tag: format!("{other:?}")
                            .as_bytes()
                            .first()
                            .copied()
                            .unwrap_or(0),
                    });
                }
            }
        }
    }

    /// Handle MsgFindIntersect: walk the client's points to find the best match.
    ///
    /// # Cursor poisoning fix (#876)
    ///
    /// A point is only accepted as an intersection when BOTH:
    ///  1. `hash` is on the *canonical* chain (`is_on_chain`, not merely
    ///     `has_block` — a fork block stored alongside the chain must be
    ///     rejected), and
    ///  2. the client-claimed `slot` matches the block's actual on-chain
    ///     slot (`find_chain_ancestor` on an on-chain hash returns that
    ///     hash's own slot).
    ///
    /// Without this check a malicious or buggy client could claim an
    /// arbitrary slot for a real hash (e.g. `(u64::MAX, real_hash)`),
    /// poisoning the follower cursor and wedging the connection — every
    /// subsequent `try_serve_next_block` lookup is keyed off the poisoned
    /// `cursor_slot`.  On mismatch we fall through exactly as we do for a
    /// point we don't recognise at all, eventually replying
    /// `MsgIntersectNotFound` if no point in the list validates.
    async fn handle_find_intersect<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        points: &[Point],
    ) -> Result<(), ProtocolError> {
        let tip = block_provider.get_tip();

        // Walk points in order (most recent first), find the first one we have.
        for point in points {
            match point {
                Point::Origin => {
                    // We always have genesis.
                    self.cursor_slot = 0;
                    self.cursor_hash = [0; 32];
                    self.cursor_initialized = true;
                    self.cursor_at_origin = true;

                    let response = encode_message(&ChainSyncMessage::MsgIntersectFound {
                        point: Point::Origin,
                        tip_slot: tip.slot,
                        tip_hash: tip.hash,
                        tip_block_number: tip.block_number,
                    });
                    channel.send(response).await.map_err(ProtocolError::from)?;
                    return Ok(());
                }
                Point::Specific(slot, hash) => {
                    if block_provider.is_on_chain(hash) {
                        if let Some((actual_slot, ancestor_hash, _block_no)) =
                            block_provider.find_chain_ancestor(hash)
                        {
                            if actual_slot == *slot && ancestor_hash == *hash {
                                self.cursor_slot = *slot;
                                self.cursor_hash = *hash;
                                self.cursor_initialized = true;
                                self.cursor_at_origin = false;

                                let response =
                                    encode_message(&ChainSyncMessage::MsgIntersectFound {
                                        point: point.clone(),
                                        tip_slot: tip.slot,
                                        tip_hash: tip.hash,
                                        tip_block_number: tip.block_number,
                                    });
                                channel.send(response).await.map_err(ProtocolError::from)?;
                                return Ok(());
                            }
                            tracing::warn!(
                                protocol = self.protocol,
                                claimed_slot = slot,
                                actual_slot,
                                "chainsync server: MsgFindIntersect point claimed a slot that \
                                 does not match the on-chain block's actual slot; rejecting"
                            );
                        }
                    }
                }
            }
        }

        // No intersection found.
        let response = encode_message(&ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: tip.slot,
            tip_hash: tip.hash,
            tip_block_number: tip.block_number,
        });
        channel.send(response).await.map_err(ProtocolError::from)?;
        Ok(())
    }

    /// Handle MsgRequestNext: serve the next header/block, propagate
    /// rollback, or wait for an announcement.
    ///
    /// # StMustReply loop fix (#869)
    ///
    /// Once `MsgAwaitReply` has been sent, the client is in `StMustReply`
    /// and — per the Ouroboros ChainSync state machine — will not send
    /// another `MsgRequestNext` until it receives a reply.  Every wake-up
    /// arm in the `select!` below can legitimately produce nothing to serve
    /// (a spurious announcement wake, a `Lagged` with nothing new past the
    /// cursor, or the periodic timeout re-poll at a quiescent tip).  Before
    /// this fix, any such "nothing to serve" outcome caused this function to
    /// return `Ok(())` without ever sending a message, silently handing
    /// agency back to the outer `run()` loop — which then blocks on
    /// `channel.recv()` waiting for a client message that (per protocol)
    /// will never arrive, wedging the connection until the client's own
    /// timeout fires.
    ///
    /// The fix: after `MsgAwaitReply` is sent, loop the `select!` until a
    /// message is actually sent (`served == true`).  `MsgAwaitReply` itself
    /// is sent exactly once — the client is already in `StMustReply` and
    /// resending it would be a protocol violation.
    async fn handle_request_next<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        announcement_rx: &mut broadcast::Receiver<BlockAnnouncement>,
        rollback_rx: &mut broadcast::Receiver<RollbackAnnouncement>,
    ) -> Result<(), ProtocolError> {
        // ── Check for pending rollbacks before serving ──────────────────────
        // Drain any rollback announcements that arrived between the last
        // MsgRequestNext and now.  If the cursor is beyond the rollback point,
        // we must send MsgRollBackward instead of the next block.
        if let Some(rb) = Self::drain_rollback(rollback_rx) {
            if self.cursor_slot > rb.slot
                || (self.cursor_slot == rb.slot && self.cursor_hash != rb.hash)
            {
                return self.send_rollback(channel, block_provider, &rb).await;
            }
        }

        // ── Drain buffered block announcements ──────────────────────────────
        // Block announcements are broadcast as dugite forges/relays blocks,
        // and are buffered in the receiver.  The direct `next_block` lookup
        // below is authoritative — dugite commits each block to ChainDB
        // before broadcasting the announcement, so any block we need to
        // serve is already reachable via the block provider.  Drain all
        // buffered announcements here so that the MsgAwaitReply loop only
        // wakes on genuinely fresh post-drain events; otherwise a stale
        // announcement for an already-served block would be re-served and
        // rejected by the downstream peer with `UnexpectedBlockNo`.
        loop {
            match announcement_rx.try_recv() {
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // Try to find and serve the next block after our cursor.
        let task_id = self as *const _ as usize;
        let pre_lookup_cursor = self.cursor_slot;
        if self.try_serve_next_block(channel, block_provider).await? {
            tracing::debug!(
                task_id,
                protocol = self.protocol,
                pre_lookup_cursor,
                post_serve_cursor = self.cursor_slot,
                "chainsync server: direct serve path produced a message"
            );
            return Ok(());
        }

        // We're at the tip — send MsgAwaitReply and wait for announcement.
        tracing::info!(
            task_id,
            protocol = self.protocol,
            cursor_slot = self.cursor_slot,
            "chainsync server: no next block — sending MsgAwaitReply, entering StMustReply"
        );
        let await_msg = encode_message(&ChainSyncMessage::MsgAwaitReply);
        channel.send(await_msg).await.map_err(|e| {
            tracing::warn!(
                task_id,
                protocol = self.protocol,
                error = %e,
                "chainsync server: MsgAwaitReply send failed"
            );
            ProtocolError::from(e)
        })?;

        // Wait for a block announcement OR rollback.  Timeout is the
        // per-connection random draw in [601 s, 911 s] — matches Haskell's
        // `minChainSyncTimeout`/`maxChainSyncTimeout` for non-trustable peers
        // and prevents re-poll synchronization across a population of peers.
        // Issue #701.
        let timeout = self.must_reply_timeout;

        // Bug J fix (2026-05-16): biased select so rollback_rx is always
        // checked first when both arms are ready.  Combined with the cursor
        // revalidation in `try_serve_next_block`, this eliminates the race
        // where an announcement arriving simultaneously with a rollback
        // would advance the cursor past the new chain's earlier blocks.
        //
        // #869 fix: loop until a message is actually sent (`served == true`).
        // `MsgAwaitReply` is NOT re-sent on subsequent iterations — the
        // client is already in `StMustReply`.
        loop {
            let served: bool = tokio::select! {
                biased;

                // ── Rollback received while waiting at tip ──────────────────
                // This is the critical path for issue #299: a follower in
                // MsgAwaitReply (StMustReply) must receive MsgRollBackward
                // when the chain switches to a better fork.
                rollback = rollback_rx.recv() => {
                    match rollback {
                        Ok(rb) => {
                            // Only send rollback if cursor is at or beyond the
                            // rollback point.  If cursor is behind, the peer
                            // hasn't seen the rolled-back blocks yet and will
                            // naturally follow the new fork.
                            if self.cursor_slot > rb.slot
                                || (self.cursor_slot == rb.slot && self.cursor_hash != rb.hash)
                            {
                                self.send_rollback(channel, block_provider, &rb).await?;
                                true
                            } else {
                                // Cursor is behind the rollback point — route
                                // through `try_serve_next_block` so cursor
                                // revalidation (Bug J) still applies on the
                                // post-rollback chain.
                                self.try_serve_next_block(channel, block_provider).await?
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Missed rollback events — send rollback to current tip.
                            let tip = block_provider.get_tip();
                            let rb = RollbackAnnouncement {
                                slot: tip.slot,
                                hash: tip.hash,
                            };
                            self.send_rollback(channel, block_provider, &rb).await?;
                            true
                        }
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                announcement = announcement_rx.recv() => {
                    tracing::info!(
                        task_id,
                        protocol = self.protocol,
                        cursor_slot = self.cursor_slot,
                        "chainsync server: woke from StMustReply on announcement"
                    );
                    match announcement {
                        Ok(_ann) => {
                            // The announcement is used only as a wake signal —
                            // the block_provider is authoritative, and we MUST
                            // re-use the same cursor-aware lookup as the
                            // direct-serve path so that (a) slot-0 blocks at
                            // Origin are handled via the inclusive `>=` lookup
                            // and (b) `cursor_at_origin` is reliably cleared
                            // after the first serve.  Trusting `ann.hash`/
                            // `ann.slot` directly skips the `cursor_at_origin =
                            // false` transition and causes the next
                            // MsgRequestNext to re-serve the first block, which
                            // Haskell rejects with `UnexpectedBlockNo`.
                            //
                            // Bug J fix (2026-05-16): defence-in-depth — drain
                            // any queued rollback first so the
                            // announcement-first race is closed even if
                            // `try_serve_next_block`'s cursor revalidation
                            // somehow missed (e.g., the cursor was still on
                            // the old chain but the rollback queue had a
                            // pending event).
                            if let Some(rb) = Self::drain_rollback(rollback_rx) {
                                if self.cursor_slot > rb.slot
                                    || (self.cursor_slot == rb.slot && self.cursor_hash != rb.hash)
                                {
                                    self.send_rollback(channel, block_provider, &rb).await?;
                                    true
                                } else {
                                    self.try_serve_next_block(channel, block_provider).await?
                                }
                            } else {
                                self.try_serve_next_block(channel, block_provider).await?
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                n,
                                protocol = self.protocol,
                                "chainsync server: announcement channel lagged; catching up from tip"
                            );
                            // Route through `try_serve_next_block` so cursor
                            // revalidation still applies after the lag.
                            self.try_serve_next_block(channel, block_provider).await?
                        }
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                _ = tokio::time::sleep(timeout) => {
                    tracing::warn!(
                        task_id,
                        protocol = self.protocol,
                        cursor_slot = self.cursor_slot,
                        timeout_seconds = timeout.as_secs(),
                        "chainsync server: StMustReply timed out — re-polling block provider for next block"
                    );
                    self.try_serve_next_block(channel, block_provider).await?
                }
            };

            if served {
                return Ok(());
            }
            // Nothing was sent (spurious wake) — the client is still in
            // StMustReply and will not send another MsgRequestNext, so we
            // must keep waiting on the SAME select! rather than returning.
        }
    }

    /// Look up the next block after the cursor and send it as `MsgRollForward`.
    ///
    /// This is the single authoritative serve path used by every wake-up site
    /// in `handle_request_next` (direct path, announcement wake-up, timeout
    /// poll). It respects `cursor_at_origin` by using the inclusive `>=`
    /// lookup so a block at slot 0 (e.g. Byron genesis EBB) is not skipped,
    /// and it unconditionally clears `cursor_at_origin` after a successful
    /// serve.
    ///
    /// # Cursor revalidation (Bug J fix, 2026-05-16)
    ///
    /// Before serving, validate that the cursor's block is still on the
    /// canonical chain.  If a fork switch has displaced it, send
    /// `MsgRollBackward` to the most recent ancestor still on chain
    /// instead of `MsgRollForward`.  This matches the Haskell ChainSync
    /// server's behaviour: the follower's cursor anchor must always be
    /// on the chain that the server is currently serving.
    ///
    /// Without this check, `get_next_block_after_slot(cursor_slot)`
    /// silently skips any new-chain blocks whose slots fall ≤ `cursor_slot`
    /// after a chain switch — the empirical Bug J failure mode.
    ///
    /// Returns `Ok(true)` if a message was sent (either `MsgRollForward` or
    /// `MsgRollBackward`), `Ok(false)` if there is no next block to serve
    /// (caller should continue waiting at the tip).
    async fn try_serve_next_block<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
    ) -> Result<bool, ProtocolError> {
        let task_id = self as *const _ as usize;
        // ── Cursor revalidation (Bug J) ─────────────────────────────────────
        // Skip when the cursor is at Origin — the all-zero hash is by design
        // not a stored block, so `is_on_chain` would always be false and we'd
        // emit a redundant Origin → Origin rollback.
        if !self.cursor_at_origin && !block_provider.is_on_chain(&self.cursor_hash) {
            let rb = match block_provider.find_chain_ancestor(&self.cursor_hash) {
                Some((slot, hash, _bn)) => RollbackAnnouncement { slot, hash },
                // No ancestor reachable on chain — rewind all the way to
                // Origin and let the peer resync from genesis.
                None => RollbackAnnouncement {
                    slot: 0,
                    hash: [0u8; 32],
                },
            };
            tracing::info!(
                task_id,
                protocol = self.protocol,
                cursor_slot = self.cursor_slot,
                rewind_slot = rb.slot,
                "chainsync server: cursor rolled off chain; \
                 sending MsgRollBackward to ancestor"
            );
            self.send_rollback(channel, block_provider, &rb).await?;
            return Ok(true);
        }

        // When cursor_at_origin is true, use the inclusive `>=` lookup so that
        // a block at slot 0 is not skipped.  The strict `>` lookup would miss
        // it since cursor_slot is 0.
        //
        // Otherwise advance by POINT, not by slot: a Byron EBB shares its
        // absolute slot with the first main block of the epoch, and a
        // slot-only advance from the EBB would skip that main block,
        // serving the peer a chain with a hole in it.
        let lookup_start = std::time::Instant::now();
        let next_block = if self.cursor_at_origin {
            block_provider.get_block_at_or_after_slot(0)
        } else {
            block_provider.get_next_block_after_point(self.cursor_slot, &self.cursor_hash)
        };
        let lookup_elapsed = lookup_start.elapsed();
        if lookup_elapsed.as_millis() > 50 {
            tracing::warn!(
                task_id,
                protocol = self.protocol,
                cursor_slot = self.cursor_slot,
                elapsed_ms = lookup_elapsed.as_millis() as u64,
                "chainsync server: block_provider lookup slow"
            );
        }

        let Some((slot, hash, block_cbor)) = next_block else {
            return Ok(false);
        };

        // Encode the payload — HFC-wrapped header for N2N, Serialised full
        // block for N2C.  See `PayloadEncoder`.
        let payload = (self.payload_encoder)(&block_cbor).map_err(|e| {
            tracing::warn!(
                task_id,
                protocol = self.protocol,
                serve_slot = slot,
                error = %e,
                "chainsync server: payload encoding failed"
            );
            e
        })?;

        let tip = block_provider.get_tip();
        let response = encode_message(&ChainSyncMessage::MsgRollForward {
            header: payload,
            tip_slot: tip.slot,
            tip_hash: tip.hash,
            tip_block_number: tip.block_number,
        });
        let send_start = std::time::Instant::now();
        let send_result = channel.send(response).await;
        let send_elapsed = send_start.elapsed();
        if send_elapsed.as_millis() > 100 {
            tracing::warn!(
                task_id,
                protocol = self.protocol,
                cursor_slot = self.cursor_slot,
                serve_slot = slot,
                send_elapsed_ms = send_elapsed.as_millis() as u64,
                ok = send_result.is_ok(),
                "chainsync server: MsgRollForward send slow"
            );
        }
        send_result.map_err(ProtocolError::from)?;
        tracing::info!(
            task_id,
            protocol = self.protocol,
            cursor_slot = self.cursor_slot,
            serve_slot = slot,
            "chainsync server: served MsgRollForward"
        );

        // Advance cursor and — critically — clear cursor_at_origin so the
        // next MsgRequestNext uses the strict `>` lookup and does not
        // re-serve the same block.
        self.cursor_slot = slot;
        self.cursor_hash = hash;
        self.cursor_at_origin = false;
        Ok(true)
    }

    /// Drain any pending rollback announcements, returning the most recent one.
    ///
    /// Multiple rollbacks may have queued up between calls — only the latest
    /// matters because each successive rollback supersedes the previous one.
    pub(crate) fn drain_rollback(
        rollback_rx: &mut broadcast::Receiver<RollbackAnnouncement>,
    ) -> Option<RollbackAnnouncement> {
        let mut latest: Option<RollbackAnnouncement> = None;
        loop {
            match rollback_rx.try_recv() {
                Ok(rb) => latest = Some(rb),
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    // Missed some — continue draining to get latest.
                    continue;
                }
                Err(_) => break,
            }
        }
        latest
    }

    /// Send `MsgRollBackward` to the downstream peer and rewind the cursor.
    async fn send_rollback<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        rb: &RollbackAnnouncement,
    ) -> Result<(), ProtocolError> {
        let tip = block_provider.get_tip();
        let point = if rb.slot == 0 && rb.hash == [0u8; 32] {
            Point::Origin
        } else {
            Point::Specific(rb.slot, rb.hash)
        };

        tracing::info!(
            protocol = self.protocol,
            rollback_slot = rb.slot,
            cursor_slot = self.cursor_slot,
            "chainsync server: sending MsgRollBackward to downstream peer"
        );

        let response = encode_message(&ChainSyncMessage::MsgRollBackward {
            point: point.clone(),
            tip_slot: tip.slot,
            tip_hash: tip.hash,
            tip_block_number: tip.block_number,
        });
        channel.send(response).await.map_err(ProtocolError::from)?;

        // Rewind cursor to the rollback point.
        self.cursor_slot = rb.slot;
        self.cursor_hash = rb.hash;
        self.cursor_at_origin = matches!(point, Point::Origin);

        Ok(())
    }
}

/// Test-only mock `BlockProvider` implementations shared by the N2N
/// (`chainsync::server`) and N2C (`local_chainsync::server`) test suites, so
/// both wirings exercise the exact same `BlockProvider` semantics against
/// the shared [`ServeCore`].
#[cfg(test)]
pub(crate) mod test_support {
    use crate::{BlockProvider, TipInfo};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Flat block store with no fork tracking — every stored block is
    /// considered on-chain.  Matches the assumption of most ChainSync tests,
    /// which don't model competing forks.
    pub(crate) struct MockBlockProvider {
        pub(crate) blocks: Vec<(u64, [u8; 32], Vec<u8>)>,
    }

    impl BlockProvider for MockBlockProvider {
        fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.blocks
                .iter()
                .find(|(_, h, _)| h == hash)
                .map(|(_, _, cbor)| cbor.clone())
        }

        fn has_block(&self, hash: &[u8; 32]) -> bool {
            self.blocks.iter().any(|(_, h, _)| h == hash)
        }

        fn get_tip(&self) -> TipInfo {
            if let Some((slot, hash, _)) = self.blocks.last() {
                TipInfo {
                    slot: *slot,
                    hash: *hash,
                    block_number: self.blocks.len() as u64,
                }
            } else {
                TipInfo {
                    slot: 0,
                    hash: [0; 32],
                    block_number: 0,
                }
            }
        }

        fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            self.blocks
                .iter()
                .find(|(s, _, _)| *s > after_slot)
                .cloned()
        }

        fn get_next_block_after_point(
            &self,
            slot: u64,
            hash: &[u8; 32],
        ) -> Option<(u64, [u8; 32], Vec<u8>)> {
            if let Some(pos) = self
                .blocks
                .iter()
                .position(|(s, h, _)| *s == slot && h == hash)
            {
                return self.blocks.get(pos + 1).cloned();
            }
            self.get_next_block_after_slot(slot)
        }

        fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            // Inclusive `>=` lookup — the default trait impl is exclusive at
            // slot 0 and would silently skip a genesis-EBB-style block sitting
            // exactly at slot 0 (issue #868).
            self.blocks.iter().find(|(s, _, _)| *s >= slot).cloned()
        }

        fn is_on_chain(&self, hash: &[u8; 32]) -> bool {
            self.blocks.iter().any(|(_, h, _)| h == hash)
        }

        fn find_chain_ancestor(&self, start_hash: &[u8; 32]) -> Option<(u64, [u8; 32], u64)> {
            self.blocks
                .iter()
                .enumerate()
                .find(|(_, (_, h, _))| h == start_hash)
                .map(|(i, (s, h, _))| (*s, *h, (i + 1) as u64))
        }
    }

    /// Block slot, hash, and CBOR data — the elements stored in mutable mock providers.
    pub(crate) type BlockEntry = (u64, [u8; 32], Vec<u8>);

    /// Mock block provider that supports mutation for simulating rollback scenarios.
    ///
    /// Wraps blocks in an `Arc<Mutex<_>>` so both the server task and the test
    /// can modify the block set concurrently (simulating ChainDB changes during
    /// a fork switch).  Like [`MockBlockProvider`], every stored block is
    /// considered on-chain — "removal" of a block from the vector IS the
    /// rollback simulation.
    pub(crate) struct MutableMockBlockProvider {
        pub(crate) blocks: Arc<Mutex<Vec<BlockEntry>>>,
    }

    impl MutableMockBlockProvider {
        pub(crate) fn new(blocks: Vec<BlockEntry>) -> Self {
            Self {
                blocks: Arc::new(Mutex::new(blocks)),
            }
        }
    }

    impl BlockProvider for MutableMockBlockProvider {
        fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .find(|(_, h, _)| h == hash)
                .map(|(_, _, cbor)| cbor.clone())
        }

        fn has_block(&self, hash: &[u8; 32]) -> bool {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .any(|(_, h, _)| h == hash)
        }

        fn get_tip(&self) -> TipInfo {
            let blocks = self.blocks.lock().unwrap();
            if let Some((slot, hash, _)) = blocks.last() {
                TipInfo {
                    slot: *slot,
                    hash: *hash,
                    block_number: blocks.len() as u64,
                }
            } else {
                TipInfo {
                    slot: 0,
                    hash: [0; 32],
                    block_number: 0,
                }
            }
        }

        fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _, _)| *s > after_slot)
                .cloned()
        }

        fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _, _)| *s >= slot)
                .cloned()
        }

        fn is_on_chain(&self, hash: &[u8; 32]) -> bool {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .any(|(_, h, _)| h == hash)
        }

        fn find_chain_ancestor(&self, start_hash: &[u8; 32]) -> Option<(u64, [u8; 32], u64)> {
            self.blocks
                .lock()
                .unwrap()
                .iter()
                .enumerate()
                .find(|(_, (_, h, _))| h == start_hash)
                .map(|(i, (s, h, _))| (*s, *h, (i + 1) as u64))
        }
    }

    /// Fork-aware mock that distinguishes blocks on the *canonical chain*
    /// from blocks merely stored as forks — required to exercise cursor
    /// revalidation (Bug J) and `MsgFindIntersect` fork/slot validation (#876).
    pub(crate) type StoredBlock = (u64, u64, [u8; 32], Vec<u8>); // (slot, block_no, prev_hash, cbor)

    pub(crate) struct ForkAwareMockProvider {
        /// Hashes on the current canonical chain, oldest → newest.
        pub(crate) chain: Arc<Mutex<Vec<[u8; 32]>>>,
        /// Every stored block keyed by hash.
        pub(crate) store: Arc<Mutex<HashMap<[u8; 32], StoredBlock>>>,
    }

    impl ForkAwareMockProvider {
        pub(crate) fn new() -> Self {
            Self {
                chain: Arc::new(Mutex::new(Vec::new())),
                store: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        /// Append a block to the canonical chain.
        pub(crate) fn push_on_chain(
            &self,
            slot: u64,
            hash: [u8; 32],
            prev_hash: [u8; 32],
            block_no: u64,
            cbor: Vec<u8>,
        ) {
            self.store
                .lock()
                .unwrap()
                .insert(hash, (slot, block_no, prev_hash, cbor));
            self.chain.lock().unwrap().push(hash);
        }

        /// Store a block as a fork (not on canonical chain).
        pub(crate) fn put_fork(
            &self,
            slot: u64,
            hash: [u8; 32],
            prev_hash: [u8; 32],
            block_no: u64,
            cbor: Vec<u8>,
        ) {
            self.store
                .lock()
                .unwrap()
                .insert(hash, (slot, block_no, prev_hash, cbor));
        }

        /// Replace the canonical chain wholesale (simulates a fork switch).
        /// All previously-on-chain blocks not in `new_chain` become forks.
        pub(crate) fn replace_chain(&self, new_chain: Vec<[u8; 32]>) {
            *self.chain.lock().unwrap() = new_chain;
        }
    }

    impl BlockProvider for ForkAwareMockProvider {
        fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.store
                .lock()
                .unwrap()
                .get(hash)
                .map(|(_, _, _, cbor)| cbor.clone())
        }

        fn has_block(&self, hash: &[u8; 32]) -> bool {
            self.store.lock().unwrap().contains_key(hash)
        }

        fn get_tip(&self) -> TipInfo {
            let chain = self.chain.lock().unwrap();
            let store = self.store.lock().unwrap();
            if let Some(tip_hash) = chain.last() {
                if let Some((slot, block_no, _, _)) = store.get(tip_hash) {
                    return TipInfo {
                        slot: *slot,
                        hash: *tip_hash,
                        block_number: *block_no,
                    };
                }
            }
            TipInfo {
                slot: 0,
                hash: [0; 32],
                block_number: 0,
            }
        }

        fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            // Walk the canonical chain in order looking for the first block
            // with slot > after_slot.  Mirrors VolatileDB's behaviour.
            let chain = self.chain.lock().unwrap();
            let store = self.store.lock().unwrap();
            for hash in chain.iter() {
                if let Some((slot, _, _, cbor)) = store.get(hash) {
                    if *slot > after_slot {
                        return Some((*slot, *hash, cbor.clone()));
                    }
                }
            }
            None
        }

        fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, [u8; 32], Vec<u8>)> {
            let chain = self.chain.lock().unwrap();
            let store = self.store.lock().unwrap();
            for hash in chain.iter() {
                if let Some((s, _, _, cbor)) = store.get(hash) {
                    if *s >= slot {
                        return Some((*s, *hash, cbor.clone()));
                    }
                }
            }
            None
        }

        fn is_on_chain(&self, hash: &[u8; 32]) -> bool {
            self.chain.lock().unwrap().iter().any(|h| h == hash)
        }

        fn find_chain_ancestor(&self, start_hash: &[u8; 32]) -> Option<(u64, [u8; 32], u64)> {
            let chain: std::collections::HashSet<[u8; 32]> =
                self.chain.lock().unwrap().iter().copied().collect();
            let store = self.store.lock().unwrap();
            let mut current = *start_hash;
            let mut visited = std::collections::HashSet::new();
            loop {
                if !visited.insert(current) {
                    return None;
                }
                if chain.contains(&current) {
                    let (slot, block_no, _, _) = store.get(&current)?;
                    return Some((*slot, current, *block_no));
                }
                let (_, _, prev_hash, _) = store.get(&current)?;
                current = *prev_hash;
            }
        }
    }
}
